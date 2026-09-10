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
    let b = s.as_bytes();
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
pub(crate) fn fingerprint_full(s: &str) -> u64 {
    let b = s.as_bytes();
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
    let b = s.as_bytes();
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
    /// is a breaking blob-format change and must bump the format magic, not this table.** 1.2 did
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
