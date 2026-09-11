//! Deterministic, **version-stable** key hashes shared by the minimal-perfect-hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence. Every word is read with `from_le_bytes`, so a blob written on one endianness reads
//! the same on the other.
//!
//! Both hashes are the same shape — seed, one [`round`] per 8-byte word, one round for the tail,
//! one finalizer — differing only in their constants. Eight bytes at a time rather than the byte
//! chain that shipped before 1.0: measured 1.5× on a 9.3-byte dictionary word, 1.7× on a 10.9-byte
//! bigram and 6.2–6.6× on an 80-byte URI-like key, A-B-A-B in one process
//! (`local/hashbench`). The round is a 64×64→128 multiply folded to 64 bits; the `u128` is one
//! `mul` on a 64-bit machine and a short software product on a 32-bit one, and the values are the
//! same everywhere.

/// The slot hash's seed and multiplier. The seed is π's first 64 bits; the multiplier is the
/// golden-ratio odd constant.
const SLOT_SEED: u64 = 0x243f_6a88_85a3_08d3;
const SLOT_MUL: u64 = 0x9e37_79b9_7f4a_7c15;
/// The fingerprint's, from π's next 64 bits and murmur3's first finalizer constant. A *different
/// multiplier* is what decorrelates the two: a difference that cancels along one accumulator's
/// orbit does not cancel along the other's.
const FP_SEED: u64 = 0x1319_8a2e_0370_7344;
const FP_MUL: u64 = 0xff51_afd7_ed55_8ccd;

/// One 8-byte word into an accumulator: the full 128-bit product, its halves folded together.
///
/// The fold is the whole point. A 64-bit product never carries downward, so a difference confined
/// to the top *k* bits of a word stays confined to the top *k* bits of the product, and any fixed
/// bijection after it — the rotate that shipped through 1.1 — only moves those bits to where the
/// next word's own difference XORs them away. That was a two-word collision family on ordinary
/// text: keys differing at bytes 8i+7 and 8i+11 alone (`d`↔`t` with `e`↔`o`, or a case flip with
/// `e`↔`i`) collided in *both* hashes with probability 13–100 %, whatever the multiplier, and
/// `CompactHashIndex` merged them into one id. The high half of the product depends on every
/// input bit through the carries, so folding it in leaves no difference with a fixed shape to
/// cancel; a single-bit scan over every position pair of a 24-byte key finds no weak pair
/// (`local/collide.rs`), where the rotate round had 35.
#[inline(always)]
fn round(h: u64, w: u64, m: u64) -> u64 {
    let p = (h ^ w) as u128 * m as u128;
    (p as u64) ^ ((p >> 64) as u64)
}

/// The trailing 0–7 bytes as one word, without a byte loop and without reading out of bounds: two
/// overlapping 4-byte loads above 3 bytes, a three-way fan-out below.
///
/// The packing is injective at every length — 4..=7 covers every byte through the overlap, and
/// 1..=3 places `b[0]`, `b[n / 2]` and `b[n - 1]` in separate octets — so the tail word, with the
/// length folded into the finalizer, determines a short key completely.
#[inline(always)]
fn tail(b: &[u8]) -> u64 {
    match b.len() {
        0 => 0,
        1..=3 => {
            let n = b.len();
            (b[0] as u64) | ((b[n / 2] as u64) << 16) | ((b[n - 1] as u64) << 32)
        }
        _ => {
            let n = b.len();
            let lo = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            let hi = u32::from_le_bytes(b[n - 4..].try_into().unwrap()) as u64;
            lo | (hi << 32)
        }
    }
}

/// splitmix64's finalizer, with the key's length folded in first.
///
/// The length is not optional: the tail is zero-padded into a word, so without it `"a"` and
/// `"a\0"` — both legal `&str` — would hash identically.
#[inline(always)]
fn fmix(mut h: u64, len: u64) -> u64 {
    h ^= len;
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// The **slot** hash: what the perfect hash indexes by. Structured keys like `"key_0001"` differ
/// only in their tail bytes, which the finalizer spreads across all 64 output bits before the MPH
/// ever sees them.
#[inline]
pub(crate) fn hash_key(s: &str) -> u64 {
    hash_key_bytes(s.as_bytes())
}

/// [`hash_key`] over the key's bytes: what a lookup reading an Arrow buffer holds.
pub(crate) fn hash_key_bytes(b: &[u8]) -> u64 {
    let mut h = SLOT_SEED;
    let mut c = b.chunks_exact(8);
    for w in &mut c {
        h = round(h, u64::from_le_bytes(w.try_into().unwrap()), SLOT_MUL);
    }
    h = round(h, tail(c.remainder()), SLOT_MUL);
    fmix(h, b.len() as u64)
}

/// The **fingerprint** hash: a *separate* hash of the key, uncorrelated with [`hash_key`] for
/// well-distributed keys. That decorrelation is what makes the chance a non-member both lands on a
/// used slot and matches the low `b` bits stored for that slot about `2^-b` — the tunable
/// false-positive rate. It is an upper bound, not an equality: a non-member whose raw slot falls
/// past the remap is rejected before the fingerprint is ever compared. Not a security primitive —
/// both hashes are deterministic and unseeded, so an adversary who picks the queries can search for
/// collisions.
///
/// Callers keep all 64 bits and truncate to the table width themselves; the collision side table
/// stores the full value, so two distinct keys merge only when they collide in *both* 64-bit hashes
/// at once (~`2^-128` per pair) — not at the `2^-(64+b)` a truncated side match would allow.
#[inline]
pub(crate) fn fingerprint_full(s: &str) -> u64 {
    fingerprint_full_bytes(s.as_bytes())
}

/// [`fingerprint_full`] over the key's bytes: what a lookup reading an Arrow buffer holds.
pub(crate) fn fingerprint_full_bytes(b: &[u8]) -> u64 {
    let mut h = FP_SEED;
    let mut c = b.chunks_exact(8);
    for w in &mut c {
        h = round(h, u64::from_le_bytes(w.try_into().unwrap()), FP_MUL);
    }
    h = round(h, tail(c.remainder()), FP_MUL);
    fmix(h, (b.len() as u64).rotate_left(32))
}

/// `(hash_key, fingerprint_full)` in one pass over the key's bytes — bit-for-bit the two functions
/// above, with both states advanced inside a single loop, so each word is loaded once. Every
/// `CompactHashIndex` path needs both hashes; `PerfectHashIndex::build` keeps using [`hash_key`]
/// alone, and `build_to_file` takes both, the second for its replay digest.
#[inline]
pub(crate) fn hash_pair(s: &str) -> (u64, u64) {
    hash_pair_bytes(s.as_bytes())
}

/// [`hash_pair`] over the key's bytes: what a lookup reading an Arrow buffer holds.
pub(crate) fn hash_pair_bytes(b: &[u8]) -> (u64, u64) {
    let (mut slot, mut fp) = (SLOT_SEED, FP_SEED);
    let mut c = b.chunks_exact(8);
    for w in &mut c {
        let w = u64::from_le_bytes(w.try_into().unwrap());
        slot = round(slot, w, SLOT_MUL);
        fp = round(fp, w, FP_MUL);
    }
    let t = tail(c.remainder());
    slot = round(slot, t, SLOT_MUL);
    fp = round(fp, t, FP_MUL);
    let n = b.len() as u64;
    (fmix(slot, n), fmix(fp, n.rotate_left(32)))
}

/// Partition `hashes` (parallel to some key order) into the MPH's key set and the collided
/// leftovers. One representative per distinct hash value — the smallest original index — goes to
/// the MPH; every other member of a colliding group is returned as `(hash, original_index)` in
/// original-index order, so the caller can hand them deterministic tail ids. Almost always the
/// second vector is empty: a 64-bit collision needs ~10^8 keys before its probability is even
/// 10^-4. When it is not, the index still builds — colliding keys are served from a side table
/// instead of failing the build outright (the hash is deterministic, so a retry could never help).
pub(crate) fn split_collisions(hashes: &[u64]) -> (Vec<u64>, Vec<(u64, u32)>) {
    let mut pairs: Vec<(u64, u32)> = hashes
        .iter()
        .enumerate()
        .map(|(i, &h)| (h, i as u32))
        .collect();
    pairs.sort_unstable();
    let mut mph_hashes = Vec::with_capacity(pairs.len());
    let mut extras = Vec::new();
    for p in pairs.chunk_by(|a, b| a.0 == b.0) {
        mph_hashes.push(p[0].0);
        extras.extend_from_slice(&p[1..]);
    }
    extras.sort_unstable_by_key(|&(_, i)| i);
    (mph_hashes, extras)
}

/// Two distinct strings with equal [`hash_key`], found offline by a Pollard-rho birthday search
/// (`local/hashcollide`, ~2^32 map steps). They drive the side-table tests in both MPH indexes;
/// the golden test below pins the collision itself, so a changed hash breaks loudly here before
/// anything subtle happens in tests built on the pair.
#[cfg(test)]
pub(crate) const COLLIDING_PAIR: (&str, &str) = ("lgywf6nnfq3in", "sax4tnfbfpa7n");

#[cfg(test)]
mod golden {
    use super::{fingerprint_full, hash_key};

    #[test]
    fn the_pinned_collision_pair_still_collides() {
        let (a, b) = super::COLLIDING_PAIR;
        assert_ne!(a, b);
        assert_eq!(hash_key(a), hash_key(b));
        assert_eq!(hash_key(a), 0x6e30_65fe_6c85_4ff2);
        // The pair collides in the slot hash only — the independent fingerprint tells them apart.
        assert_ne!(fingerprint_full(a), fingerprint_full(b));
    }

    /// Every serialised MPH blob is keyed on these hashes, so a hash that silently changed — a
    /// tweaked constant, a reordered finalizer, a byte-order slip in a refactor — would make every
    /// previously-saved index load wrong without any test failing. These pinned values turn that
    /// into a loud CI failure instead. **Do not "fix" them to match new output: changing the hash
    /// is a breaking blob-format change and must bump the format magic, not this table.** 2.0 did
    /// exactly that — `BMP7` and `BCH7` — when the round was replaced.
    #[test]
    fn hash_key_is_stable() {
        assert_eq!(hash_key(""), 0x6d83_15b9_dee0_feb1);
        assert_eq!(hash_key("a"), 0xbf7c_cb3a_479f_1a5d);
        assert_eq!(hash_key("apple"), 0xc147_ef0f_5b30_8081);
        assert_eq!(hash_key("GET"), 0x51b7_ea28_3181_36d8);
        assert_eq!(hash_key("é中🎉"), 0xdc3c_6a40_ff1a_bc88);
        assert_eq!(hash_key("member-00042"), 0xb5f8_5009_b647_c12a);
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint_full(""), 0x9e7e_cf5b_d57d_4fa1);
        assert_eq!(fingerprint_full("apple"), 0x4b6a_e8cc_993f_a404);
        assert_eq!(fingerprint_full("é中🎉"), 0x7c27_66ce_cb21_5389);
        // The table stores the low `b` bits of that value — the widths the indexes actually write.
        assert_eq!(fingerprint_full("GET") & 0xf, 0xf);
        assert_eq!(fingerprint_full("member-00042") & 0xffff, 0x0b01);
    }

    /// The fused pass is an optimisation, not a second hash function: it must agree with the two
    /// it replaces on every input, or every blob written by one version reads wrong under another.
    #[test]
    fn the_fused_pass_equals_the_two_separate_hashes() {
        let cases: Vec<String> = ["", "a", "apple", "GET", "é中🎉", "member-00042"]
            .iter()
            .map(|s| s.to_string())
            .chain((0..2_000).map(|i| format!("key-{i:07}-{}", "x".repeat(i % 97))))
            .collect();
        for s in &cases {
            assert_eq!(
                super::hash_pair(s),
                (hash_key(s), fingerprint_full(s)),
                "{s:?}"
            );
        }
    }
}

/// The hash's own quality battery: what a change to [`round`], [`tail`] or [`fmix`] has to
/// survive before it ships, and the source of the committed `bench/results/hash-quality-*.txt`.
///
/// Run it with
/// `cargo test --release --features bench-mphf -- --ignored --nocapture hash_quality`.
/// It is `#[ignore]`d and behind a feature because it runs for tens of seconds and because an
/// ignored test's body still counts against the crate's coverage floor.
///
/// Every statistic is reported as a standard-normal `z`, so one bound reads across the whole
/// battery. Chi-square goes through the Wilson–Hilferty transform rather than the textbook
/// `(x − k) / sqrt(2k)`, which is wrong exactly where the thresholds live: at 255 degrees of
/// freedom and a true `z` of 6.00 the two read 5.99 and 7.07 (checked against `incgam` in
/// PARI/GP). The battery runs some forty tables, 88 064 avalanche cells and 1 806 336
/// bit-independence cells, so `|z| < 6.5` is the bound throughout — a family-wise false-alarm
/// rate of 1.5 × 10⁻⁴ over all 1 894 400 of them.
#[cfg(all(test, feature = "bench-mphf"))]
mod quality {
    use super::{fingerprint_full_bytes, hash_key_bytes, hash_pair_bytes};

    /// `|z|` a single cell of the battery may reach before it is called a failure. Derived in
    /// PARI/GP from the cell count, which is what a bound like this is a function of: 88 064
    /// avalanche cells (688 input bits over five lengths × 64 output bits × two hashes) and
    /// 1 806 336 bit-independence cells (448 input bits × two hashes × 2 016 output pairs) come
    /// to 1 894 400, and a family-wise rate of 10⁻³ over that many wants a two-sided `z` of
    /// 6.211; 6.5 leaves it at 1.5 × 10⁻⁴. The tables want far less. An earlier bound of 6.0 was
    /// derived from 44 032 cells, which counted one hash and no bit-independence cell at all.
    const Z_BOUND: f64 = 6.5;

    /// splitmix64, so the corpora and the probe sets are the same on every machine and every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    /// A chi-square statistic over `counts` as a standard-normal `z`, by Wilson–Hilferty.
    ///
    /// `counts` must have at least two cells and enough mass that the expected count per cell is
    /// comfortably above five — every caller here keeps it above twenty.
    fn chi2_z(counts: &[u64]) -> f64 {
        let k = counts.len();
        let total: u64 = counts.iter().sum();
        let e = total as f64 / k as f64;
        assert!(
            k > 1 && e >= 5.0,
            "chi-square wants {k} cells with mass, got e = {e}"
        );
        let chi2: f64 = counts
            .iter()
            .map(|&c| {
                let d = c as f64 - e;
                d * d / e
            })
            .sum();
        let k = (k - 1) as f64;
        let t = 2.0 / (9.0 * k);
        ((chi2 / k).cbrt() - (1.0 - t)) / t.sqrt()
    }

    /// A count of successes out of `n` trials with success probability `p`, as a standard-normal
    /// `z`. Used where a chi-square would have two cells.
    fn binom_z(hits: u64, n: u64, p: f64) -> f64 {
        let mean = n as f64 * p;
        (hits as f64 - mean) / (mean * (1.0 - p)).sqrt()
    }

    /// The dictionary if this machine has one, else `None` — the battery reports what it ran on.
    fn dictionary() -> Option<Vec<String>> {
        let text = std::fs::read_to_string("/usr/share/dict/words").ok()?;
        let words: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect();
        (words.len() > 10_000).then_some(words)
    }

    /// The key families a real index meets, each one a shape that has broken a hash somewhere:
    /// a shared prefix, a shared suffix, a dense numeric tail, a small alphabet, multi-byte
    /// characters, and the pre-sorted integer sequence a synthetic benchmark generates.
    fn families(n: usize) -> Vec<(&'static str, Vec<Vec<u8>>)> {
        let words = dictionary().unwrap_or_default();
        let word = |i: usize| -> &str {
            if words.is_empty() {
                "lexindex"
            } else {
                words[i % words.len()].as_str()
            }
        };
        let mut rng = Rng(0x5eed_0007);
        let mut out: Vec<(&'static str, Vec<Vec<u8>>)> = Vec::new();
        if !words.is_empty() {
            out.push((
                "words",
                words
                    .iter()
                    .take(n)
                    .map(|w| w.as_bytes().to_vec())
                    .collect(),
            ));
            out.push((
                "bigrams",
                (0..n)
                    .map(|i| format!("{}.{}", word(i * 7), word(i * 13 + 1)).into_bytes())
                    .collect(),
            ));
            out.push((
                "prefixed",
                (0..n)
                    .map(|i| format!("https://example.com/a/b/{}", word(i)).into_bytes())
                    .collect(),
            ));
            out.push((
                "suffixed",
                (0..n)
                    .map(|i| format!("{}@mail.example.com", word(i)).into_bytes())
                    .collect(),
            ));
        }
        out.push((
            "numeric",
            (0..n).map(|i| format!("key_{i:09}").into_bytes()).collect(),
        ));
        out.push((
            "decimal",
            (0..n).map(|i| i.to_string().into_bytes()).collect(),
        ));
        out.push((
            "uuid",
            (0..n)
                .map(|_| {
                    let (a, b) = (rng.next(), rng.next());
                    format!(
                        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                        a >> 32,
                        (a >> 16) & 0xffff,
                        a & 0xffff,
                        b >> 48,
                        b & 0xffff_ffff_ffff
                    )
                    .into_bytes()
                })
                .collect(),
        ));
        out.push((
            "cyrillic",
            (0..n)
                .map(|i| {
                    let mut s = String::new();
                    let mut x = i;
                    for _ in 0..6 {
                        s.push(char::from_u32(0x430 + (x % 32) as u32).expect("in range"));
                        x /= 32;
                    }
                    s.into_bytes()
                })
                .collect(),
        ));
        out.push((
            "dna",
            (0..n)
                .map(|_| (0..40).map(|_| b"ACGT"[rng.below(4)]).collect())
                .collect(),
        ));
        out.push((
            "paths",
            (0..n)
                .map(|i| {
                    format!("/usr/lib/{}/{}/{}.so", word(i), word(i * 3), word(i * 5)).into_bytes()
                })
                .collect(),
        ));
        out
    }

    /// Every output bit must flip with probability ½ when any one input bit flips: the strict
    /// avalanche criterion, cell by cell, for both hashes and five key lengths.
    ///
    /// Returns `(worst |z|, where)`.
    fn avalanche(trials: usize) -> (f64, String) {
        let mut rng = Rng(0xa1a1_0001);
        let mut worst = (0.0f64, String::from("none"));
        for len in [4usize, 9, 16, 24, 33] {
            let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
            for bit in 0..len * 8 {
                let mut flips = [[0u64; 64]; 2];
                for key in &keys {
                    let base = hash_pair_bytes(key);
                    let mut other = key.clone();
                    other[bit / 8] ^= 1 << (bit % 8);
                    let moved = hash_pair_bytes(&other);
                    for (h, (a, b)) in [(0, (base.0, moved.0)), (1, (base.1, moved.1))] {
                        let x = a ^ b;
                        for (out, cell) in flips[h].iter_mut().enumerate() {
                            *cell += (x >> out) & 1;
                        }
                    }
                }
                for (h, name) in [(0, "slot"), (1, "fingerprint")] {
                    for (out, &f) in flips[h].iter().enumerate() {
                        let z = binom_z(f, trials as u64, 0.5).abs();
                        if z > worst.0 {
                            worst = (z, format!("{name} len {len} in-bit {bit} out-bit {out}"));
                        }
                    }
                }
            }
        }
        worst
    }

    /// How many input bits one (length, hash) pair contributes. A 640-bit key would otherwise
    /// cost five times what a 128-bit one does for no extra statistical reach: the criterion is
    /// about the *output* pairs, and every input bit tests all 2 016 of them. Sampling the input
    /// bits with the battery's own fixed RNG keeps the cost flat in key length and the cell
    /// count — which `Z_BOUND` is a function of — a constant.
    const BIC_IN_BITS: usize = 128;

    /// Two output bits must flip independently of each other: the bit-independence criterion, as
    /// a 2×2 table per (input bit, output pair) and a chi-square on it — for **both** hashes over
    /// four key lengths, the same surface avalanche covers.
    ///
    /// Returns `(worst |z|, where)`.
    fn bit_independence(trials: usize) -> (f64, String) {
        let mut rng = Rng(0xb1c0_0002);
        let mut worst = (0.0f64, String::from("none"));
        for len in [8usize, 16, 24, 80] {
            let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
            let mut bits: Vec<usize> = (0..len * 8).collect();
            // A deterministic partial shuffle, so which bits are tested is fixed across machines.
            for i in 0..bits.len().min(BIC_IN_BITS) {
                let j = i + rng.below(bits.len() - i);
                bits.swap(i, j);
            }
            bits.truncate(BIC_IN_BITS);
            for &bit in &bits {
                let mut joint = [vec![[0u64; 4]; 64 * 64], vec![[0u64; 4]; 64 * 64]];
                for key in &keys {
                    let mut other = key.clone();
                    other[bit / 8] ^= 1 << (bit % 8);
                    let (a0, a1) = hash_pair_bytes(key);
                    let (b0, b1) = hash_pair_bytes(&other);
                    for (h, x) in [(0usize, a0 ^ b0), (1, a1 ^ b1)] {
                        for i in 0..64 {
                            let bi = ((x >> i) & 1) as usize;
                            for j in i + 1..64 {
                                let bj = ((x >> j) & 1) as usize;
                                joint[h][i * 64 + j][bi * 2 + bj] += 1;
                            }
                        }
                    }
                }
                for (h, name) in [(0usize, "slot"), (1, "fingerprint")] {
                    for i in 0..64 {
                        for j in i + 1..64 {
                            let z = chi2_z(&joint[h][i * 64 + j]).abs();
                            if z > worst.0 {
                                worst = (
                                    z,
                                    format!("{name} len {len} in-bit {bit} out-bits ({i},{j})"),
                                );
                            }
                        }
                    }
                }
            }
        }
        worst
    }

    /// The scan that caught the pre-2.0 collision family: keys differing in exactly two bytes.
    ///
    /// Through 1.1 the round was a multiply and a rotate, and a difference in the top bits of one
    /// word could be cancelled by a difference in another — keys differing at bytes `8i+7` and
    /// `8i+11` collided in *both* hashes with probability 13–100 %, and `CompactHashIndex` merged
    /// them into one id. Every ordered pair of byte positions and every pair of single-bit deltas
    /// is tried here against random base keys; a double collision is a catastrophe, a collision in
    /// one hash alone is expected about `trials · 2⁻⁶⁴` times, i.e. never.
    ///
    /// Returns `(worst double-collision count, combos tried, where)`.
    fn two_byte_differential(len: usize, trials: usize) -> (u64, usize, String) {
        let mut rng = Rng(0xd1ff_0003);
        let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
        let mut worst = (0u64, String::from("none"));
        let mut combos = 0usize;
        for i in 0..len {
            for j in i + 1..len {
                for bi in 0..8u8 {
                    for bj in 0..8u8 {
                        combos += 1;
                        let mut both = 0u64;
                        for key in &keys {
                            let mut other = key.clone();
                            other[i] ^= 1 << bi;
                            other[j] ^= 1 << bj;
                            if hash_pair_bytes(key) == hash_pair_bytes(&other) {
                                both += 1;
                            }
                        }
                        if both > worst.0 {
                            worst = (both, format!("bytes ({i},{j}) bits ({bi},{bj})"));
                        }
                    }
                }
            }
        }
        (worst.0, combos, worst.1)
    }

    /// The report. Prints every statistic and fails on the first one past the bound, so the
    /// committed artifact and the pass/fail are the same run.
    #[test]
    #[ignore = "runs for tens of seconds; the committed output is bench/results/hash-quality-*.txt"]
    fn hash_quality() {
        println!("lexindex hash quality — bound |z| < {Z_BOUND}\n");
        println!(
            "corpus source: {}",
            if dictionary().is_some() {
                "/usr/share/dict/words plus generated families"
            } else {
                "generated families only (no /usr/share/dict/words on this machine)"
            }
        );

        let (z, where_) = avalanche(8192);
        println!("\navalanche (strict, 8192 keys per cell, 5 lengths, both hashes)");
        println!("  worst |z| {z:6.2}   {where_}");
        assert!(z < Z_BOUND, "avalanche: |z| {z:.2} at {where_}");

        let (z, where_) = bit_independence(2048);
        println!(
            "\nbit independence (2048 keys, lengths 8/16/24/80, both hashes, \u{2264}{BIC_IN_BITS} input bits each)"
        );
        println!("  worst |z| {z:6.2}   {where_}");
        assert!(z < Z_BOUND, "bit independence: |z| {z:.2} at {where_}");

        let (both, combos, where_) = two_byte_differential(24, 2048);
        println!("\ntwo-byte differential (24-byte keys, 2048 bases per combo)");
        println!("  {combos} combos, worst double collision {both}/2048   {where_}");
        assert_eq!(both, 0, "two-byte differential: {both} at {where_}");

        println!("\nper corpus (n = 100 000 where the family allows it)");
        println!(
            "  {:<10} {:>7} {:>6} {:>9} {:>9} {:>9} {:>9}",
            "family", "n", "coll", "z top12", "z low12", "z fp8", "z joint"
        );
        for (name, keys) in families(100_000) {
            let n = keys.len();
            let mut slots: Vec<u64> = Vec::with_capacity(n);
            let mut top = vec![0u64; 4096];
            let mut low = vec![0u64; 4096];
            let mut fp8 = vec![0u64; 256];
            let mut joint = vec![0u64; 4096];
            for key in &keys {
                let (slot, fp) = hash_pair_bytes(key);
                assert_eq!(
                    (slot, fp),
                    (hash_key_bytes(key), fingerprint_full_bytes(key))
                );
                slots.push(slot);
                top[(slot >> 52) as usize] += 1;
                low[(slot & 0xfff) as usize] += 1;
                fp8[(fp & 0xff) as usize] += 1;
                joint[(((slot & 0x3f) << 6) | (fp & 0x3f)) as usize] += 1;
            }
            slots.sort_unstable();
            let coll = n - 1 - slots.windows(2).filter(|w| w[0] != w[1]).count();
            let zs = [chi2_z(&top), chi2_z(&low), chi2_z(&fp8), chi2_z(&joint)];
            println!(
                "  {name:<10} {n:>7} {coll:>6} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                zs[0], zs[1], zs[2], zs[3]
            );
            assert_eq!(coll, 0, "{name}: {coll} slot-hash collisions in {n} keys");
            for (z, what) in zs.iter().zip(["top12", "low12", "fp8", "joint"]) {
                assert!(z.abs() < Z_BOUND, "{name} {what}: |z| {:.2}", z.abs());
            }
        }
        println!("\nall cells within the bound");
    }
}
