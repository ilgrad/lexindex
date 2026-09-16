//! Deterministic, **version-stable** key hashes shared by the hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence. Every word is read with `from_le_bytes`, so a blob written on one endianness reads
//! the same on the other.
//!
//! Both hashes read a key the same way — its [`words`] — and differ only in the constants they mix
//! them with, so [`hash_pair_bytes`] reads the key once. The shape is wyhash's and rapidhash's: a
//! key of 4..=16 bytes is two words from four overlapping 4-byte loads, 17..=32 bytes are its first
//! and last sixteen, longer keys run two multiply lanes over 32-byte blocks and end on the words of
//! their last 32 bytes, and 1..=3 bytes fan out into one word. The words go through two 64×64→128
//! multiplies folded to 64 bits, side by side, and one more that merges them with the length. No
//! loop and no branch on the length inside a class, so a stream of mixed-length keys costs no branch
//! mispredictions, where the hash 2.0–3.x shipped — one multiply per 8-byte word, in a loop, then a
//! tail switch — lost 6 of its 9.5 ns to them. Measured A-B-A-B in one process on 2026-09-16
//! (`local/hashbench/src/bin/newhash.rs`), the hash alone over keys read in order: dictionary words
//! 3.9 against 7.3 ns, English titles 7.4 against 11.2, URLs 6.5 against 13.2, 8-byte keys 3.8
//! against 2.9 — the one loss; shuffled over 100 K boxed keys, 9.3 / 11.8 / 14.9 against 11.5 / 16 /
//! 21. The distribution battery below reads the same for both (avalanche |z| 4.62, low-bit χ²
//! |z| ≤ 2.5, no 64-bit collision on any corpus).
//!
//! The fold is kept from 2.0, and it is the whole point of the multiply. A 64-bit product never
//! carries downward, so a difference confined to the top *k* bits of a word stays confined to the
//! top *k* bits of the product, and any fixed bijection after it only moves those bits to where
//! the next word's own difference XORs them away — the two-word collision family 1.1 had on
//! ordinary text (`d`↔`t` with `e`↔`o` at bytes 8i+7 and 8i+11, in *both* hashes). The high half
//! of the product depends on every input bit through the carries, so folding it in leaves no
//! difference with a fixed shape to cancel.
//!
//! The constants are consecutive 64-bit words of π's fraction — nothing up the sleeve (PARI/GP:
//! `frac(Pi)` scaled by 2^64, the first word `0x243F6A8885A308D3`) — keeping the odd ones with 27
//! to 35 bits set. Different constants in every position is what decorrelates the two hashes: a
//! difference that cancels along one product does not cancel along the other's.

/// The slot hash's constants: seeds of the two lanes, the merge's, and the two lane states of the
/// block loop.
const SLOT: [u64; 8] = [
    0x243f_6a88_85a3_08d3,
    0x082e_fa98_ec4e_6c89,
    0x4528_21e6_38d0_1377,
    0xc0ac_29b7_c97c_50dd,
    0x3f84_d5b5_b547_0917,
    0x9216_d5d9_8979_fb1b,
    0xba7c_9045_f12c_7f99,
    0x24a1_9947_b391_6cf7,
];
/// The fingerprint's, in the same roles: the next eight such words.
const FP: [u64; 8] = [
    0x6369_20d8_7157_4e69,
    0x7b54_a41d_c25a_59b5,
    0x9c30_d539_2af2_6013,
    0xca41_7918_b8db_38ef,
    0xd715_77c1_bd31_4b27,
    0xa154_86af_7c72_e993,
    0x7a32_5381_2895_8677,
    0x3b8f_4898_6b4b_b9af,
];

/// The full 128-bit product of two words, its halves folded together. One `mul` on a 64-bit
/// machine and a short software product on a 32-bit one, the same value everywhere.
#[inline(always)]
fn mum(a: u64, b: u64) -> u64 {
    let p = a as u128 * b as u128;
    (p as u64) ^ ((p >> 64) as u64)
}

#[inline(always)]
fn r4(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64
}

#[inline(always)]
fn r8(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// The four words of a key of 4..=32 bytes. To 16 bytes, two words from four 4-byte loads at 0,
/// `d`, `n - 4` and `n - 4 - d`, where `d = ⌊n / 8⌋ · 4`: below 8 bytes `d` is 0 and the first
/// and last four bytes cover the key, to 15 it is 4 and the first and last eight do, at 16 it is
/// 8 and the loads are the four quarters — so at every length every byte is read, and with the
/// length the words determine the key. Above 16 bytes, the first and the last sixteen, which
/// overlap while the key is shorter than 32.
#[inline(always)]
fn words(b: &[u8]) -> [u64; 4] {
    let n = b.len();
    if n <= 16 {
        let d = (n >> 3) << 2;
        [
            r4(b, 0) | (r4(b, d) << 32),
            r4(b, n - 4) | (r4(b, n - 4 - d) << 32),
            0,
            0,
        ]
    } else {
        [r8(b, 0), r8(b, 8), r8(b, n - 16), r8(b, n - 8)]
    }
}

/// The words of a key below 4 bytes: `b[0]`, `b[n / 2]` and `b[n - 1]` in separate octets of one
/// word — injective at each length — and all zeros for the empty key, which the length folded
/// into the merge keeps apart from `"\0"`.
#[inline(always)]
fn short(b: &[u8]) -> [u64; 4] {
    let n = b.len();
    if n == 0 {
        return [0; 4];
    }
    [
        (b[0] as u64) | ((b[n / 2] as u64) << 16) | ((b[n - 1] as u64) << 32),
        0,
        0,
        0,
    ]
}

/// Two multiplies side by side over the words, and one that merges them with the length.
///
/// The length is not optional: a short key's words are zero-padded, so without it `"a"` and
/// `"a\0"` — both legal `&str` — would hash identically.
#[inline(always)]
fn merge(w: [u64; 4], len: u64, k: &[u64; 8]) -> u64 {
    let a = mum(w[0] ^ k[0], w[1] ^ k[1]);
    let c = mum(w[2] ^ k[2], w[3] ^ k[3]);
    mum(a ^ len ^ k[4], c ^ k[5])
}

/// One 32-byte block into a pair of lane states.
#[inline(always)]
fn block(s: (u64, u64), w: [u64; 4], k: &[u64; 8]) -> (u64, u64) {
    (mum(w[0] ^ s.0, w[1] ^ k[0]), mum(w[2] ^ s.1, w[3] ^ k[1]))
}

/// The words of any key under one constant set: [`words`] to 32 bytes, [`short`] below 4, and
/// above 32 the two lanes over the leading whole blocks — the last block of the key is read as
/// part of its last 32 bytes, which the lanes are folded into. The bytes both read are hashed
/// twice, which costs nothing and keeps every length on the one path.
#[inline(always)]
fn key_words(b: &[u8], k: &[u64; 8]) -> [u64; 4] {
    let n = b.len();
    if n <= 32 {
        return if n >= 4 { words(b) } else { short(b) };
    }
    let mut s = (k[6], k[7]);
    let mut p = 0;
    while n - p > 32 {
        s = block(s, [r8(b, p), r8(b, p + 8), r8(b, p + 16), r8(b, p + 24)], k);
        p += 32;
    }
    let mut w = words(&b[n - 32..]);
    w[0] ^= s.0;
    w[2] ^= s.1;
    w
}

/// The **slot** hash: what the perfect hash indexes by. Structured keys like `"key_0001"` differ
/// only in a few bytes, which the merge spreads across all 64 output bits before the MPH ever
/// sees them.
#[inline]
pub(crate) fn hash_key(s: &str) -> u64 {
    hash_key_bytes(s.as_bytes())
}

/// [`hash_key`] over the key's bytes: what a lookup reading an Arrow buffer holds.
#[inline]
pub fn hash_key_bytes(b: &[u8]) -> u64 {
    merge(key_words(b, &SLOT), b.len() as u64, &SLOT)
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
#[inline]
pub(crate) fn fingerprint_full_bytes(b: &[u8]) -> u64 {
    merge(key_words(b, &FP), (b.len() as u64).rotate_left(32), &FP)
}

/// `(hash_key, fingerprint_full)` in one pass over the key's bytes — bit-for-bit the two functions
/// above, the words read once and, above 32 bytes, both hashes' lanes run over each block as it is
/// loaded. Every `CompactHashIndex` path needs both hashes; `PerfectHashIndex::build` keeps using
/// [`hash_key`] alone, and `build_to_file` takes both, the second for its replay digest.
#[inline]
pub(crate) fn hash_pair(s: &str) -> (u64, u64) {
    hash_pair_bytes(s.as_bytes())
}

/// [`hash_pair`] over the key's bytes: what a lookup reading an Arrow buffer holds.
pub(crate) fn hash_pair_bytes(b: &[u8]) -> (u64, u64) {
    let n = b.len();
    let len = n as u64;
    if n <= 32 {
        let w = if n >= 4 { words(b) } else { short(b) };
        return (merge(w, len, &SLOT), merge(w, len.rotate_left(32), &FP));
    }
    let (mut s, mut f) = ((SLOT[6], SLOT[7]), (FP[6], FP[7]));
    let mut p = 0;
    while n - p > 32 {
        let w = [r8(b, p), r8(b, p + 8), r8(b, p + 16), r8(b, p + 24)];
        s = block(s, w, &SLOT);
        f = block(f, w, &FP);
        p += 32;
    }
    let w = words(&b[n - 32..]);
    (
        merge([w[0] ^ s.0, w[1], w[2] ^ s.1, w[3]], len, &SLOT),
        merge(
            [w[0] ^ f.0, w[1], w[2] ^ f.1, w[3]],
            len.rotate_left(32),
            &FP,
        ),
    )
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
pub(crate) const COLLIDING_PAIR: (&str, &str) = ("2z4vqnm4rshfe", "6c6rjaoegwraa");

#[cfg(test)]
mod golden {
    use super::{fingerprint_full, hash_key};

    #[test]
    fn the_pinned_collision_pair_still_collides() {
        let (a, b) = super::COLLIDING_PAIR;
        assert_ne!(a, b);
        assert_eq!(hash_key(a), hash_key(b));
        assert_eq!(hash_key(a), 0xf2ed_3d38_004f_7110);
        // The pair collides in the slot hash only — the independent fingerprint tells them apart.
        assert_ne!(fingerprint_full(a), fingerprint_full(b));
    }

    /// `f` over every input against its pinned value, all of them reported at once when any
    /// moved: what a deliberate change needs to repin the table in one run.
    fn pinned(f: fn(&str) -> u64, table: &[(&str, u64)]) {
        let actual: Vec<String> = table
            .iter()
            .map(|&(s, want)| format!("({s:?}, {:#018x}) wanted {want:#018x}", f(s)))
            .collect();
        assert!(
            table.iter().all(|&(s, want)| f(s) == want),
            "pinned values moved:\n{}",
            actual.join("\n")
        );
    }

    /// Every serialised MPH blob is keyed on these hashes, so a hash that silently changed — a
    /// tweaked constant, a reordered merge, a byte-order slip in a refactor — would make every
    /// previously-saved index load wrong without any test failing. These pinned values turn that
    /// into a loud CI failure instead. **Do not "fix" them to match new output: changing the hash
    /// is a breaking blob-format change and must bump the format magic, not this table.** 2.0 did
    /// exactly that — `BMP7` and `BCH7` — when the round was replaced, and 4.0 again — `BMP8`,
    /// `BCH8`, `BCL2` — when the shape did.
    #[test]
    fn hash_key_is_stable() {
        pinned(
            hash_key,
            &[
                ("", 0x6e2f_2e91_3577_6ae6),
                ("a", 0x9fec_cfe5_8406_fd60),
                ("GET", 0xb346_7f8a_084d_f3f9),
                ("four", 0xbbf2_5c43_0083_4136),
                ("apple", 0xee2b_68d3_51dd_e03d),
                ("é中🎉", 0x44d6_b770_0550_61c8),
                ("member-00042", 0xd36c_56af_fc50_dd99),
                ("sixteen bytes!!!", 0xb0f4_e284_3b60_0bf7),
                ("a key of 17 bytes", 0x8191_c4d0_71c3_a0b5),
                ("thirty-two bytes, the class edge", 0x8de3_b04c_e9cc_c16c),
                ("thirty-three bytes: one block in!", 0x9a78_b624_f316_ac7d),
                (
                    "a key long enough for the block loop to run twice over it, and then some",
                    0xcfa9_cd68_39b9_f888,
                ),
            ],
        );
    }

    #[test]
    fn fingerprint_is_stable() {
        pinned(
            fingerprint_full,
            &[
                ("", 0x50a7_011a_5132_afaa),
                ("GET", 0x2b62_d211_e028_8212),
                ("apple", 0x8567_3f6c_e423_1e97),
                ("é中🎉", 0xcf95_7356_4eaf_470d),
                ("member-00042", 0x35e5_e583_590c_cfec),
                ("a key of 17 bytes", 0x4447_7b95_328e_b27d),
                ("thirty-three bytes: one block in!", 0xeaee_1e3a_1bcd_06a8),
                (
                    "a key long enough for the block loop to run twice over it, and then some",
                    0x6f6a_9237_53f3_ee3d,
                ),
            ],
        );
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

    /// Every byte of every length to 100 is read by both hashes — flipping it moves them — and a
    /// trailing zero byte moves them too: the coverage the word layout promises.
    #[test]
    fn every_byte_moves_both_hashes() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for len in 0..=100usize {
            let key: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let (s, f) = super::hash_pair_bytes(&key);
            for i in 0..len {
                let mut k = key.clone();
                k[i] ^= 1 << (next() % 8);
                let (s2, f2) = super::hash_pair_bytes(&k);
                assert!(s2 != s && f2 != f, "byte {i} of {len} does not move a hash");
            }
            let mut k = key.clone();
            k.push(0);
            let (s2, f2) = super::hash_pair_bytes(&k);
            assert!(
                s2 != s && f2 != f,
                "a trailing zero does not move a hash at {len}"
            );
        }
    }
}

/// The hash's own quality battery: what a change to [`words`], [`merge`] or a constant has to
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
        let mut rng = Rng(0x5eed_0007);
        let mut out: Vec<(&'static str, Vec<Vec<u8>>)> = Vec::new();
        // Every family spelled from words is built here or not at all: without a dictionary there
        // is nothing to spell with, and a stand-in word makes the family one key repeated `n`
        // times, which the collision count then reports as a broken hash.
        if let Some(words) = dictionary() {
            let word = |i: usize| words[i % words.len()].as_str();
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
            out.push((
                "paths",
                (0..n)
                    .map(|i| {
                        format!("/usr/lib/{}/{}/{}.so", word(i), word(i * 3), word(i * 5))
                            .into_bytes()
                    })
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
        for (name, keys) in &out {
            let mut distinct: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(
                distinct.len(),
                keys.len(),
                "{name} repeats a key: its collisions would measure this generator, not the hash"
            );
        }
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
