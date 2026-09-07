//! Deterministic, **version-stable** key hashes shared by the minimal-perfect-hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence.

/// FNV-1a over the bytes, then a splitmix64 finalizer for avalanche (so structured keys like
/// `"key_0001"` still spread evenly across the MPH's buckets). This drives the perfect-hash
/// **slot**; it is [`crate::blob::hash_bytes`], which the blob headers also use as their check.
#[inline]
pub(crate) fn hash_key(s: &str) -> u64 {
    crate::blob::hash_bytes(s.as_bytes())
}

/// The **fingerprint** hash: a *separate* hash of the key (a different basis/multiplier than
/// [`hash_key`], so it is uncorrelated with the slot hash for well-distributed keys). That
/// decorrelation is what makes the chance a non-member both lands on a used slot and matches the
/// low `b` bits stored for that slot about `2^-b` — the tunable false-positive rate. It is an upper
/// bound, not an equality: a non-member whose raw slot falls past the remap is rejected before the
/// fingerprint is ever compared. Not a security primitive — both hashes are deterministic and
/// unseeded, so an adversary who picks the queries can search for collisions.
///
/// Callers keep all 64 bits and truncate to the table width themselves; the collision side table
/// stores the full value, so two distinct keys merge only when they collide in *both* 64-bit hashes
/// at once (~`2^-128` per pair) — not at the `2^-(64+b)` a truncated side match would allow.
pub(crate) fn fingerprint_full(s: &str) -> u64 {
    let mut h: u64 = 0x0000_0100_0000_01b3; // distinct basis from hash_key
    for &b in s.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15); // golden-ratio odd multiplier
    }
    h ^ (h >> 29)
}

/// `(hash_key, fingerprint_full)` in one pass over the key's bytes — bit-for-bit the two functions
/// above, with both states advanced inside a single loop. Every `CompactHashIndex` path needs both
/// hashes, and reading the key once instead of twice measured **21.3 → 18.2 ns/key** on real-word
/// bigrams and **126 → 77 ns/key** on 80-byte URI-like keys (three rounds each, values asserted
/// identical). `PerfectHashIndex` keeps using [`hash_key`] alone — it never needs the second hash.
#[inline]
pub(crate) fn hash_pair(s: &str) -> (u64, u64) {
    let mut slot: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    let mut fp: u64 = 0x0000_0100_0000_01b3;
    for &b in s.as_bytes() {
        slot ^= b as u64;
        slot = slot.wrapping_mul(0x0000_0100_0000_01b3);
        fp = (fp ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    slot = (slot ^ (slot >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    slot = (slot ^ (slot >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (slot ^ (slot >> 31), fp ^ (fp >> 29))
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
pub(crate) const COLLIDING_PAIR: (&str, &str) = ("x5iojurfgtipm", "7gvob4sxctomf");

#[cfg(test)]
mod golden {
    use super::{fingerprint_full, hash_key};

    #[test]
    fn the_pinned_collision_pair_still_collides() {
        let (a, b) = super::COLLIDING_PAIR;
        assert_ne!(a, b);
        assert_eq!(hash_key(a), hash_key(b));
        assert_eq!(hash_key(a), 0x156a_c9d1_f216_0cbf);
        // The pair collides in the slot hash only — the independent fingerprint tells them apart.
        assert_ne!(fingerprint_full(a), fingerprint_full(b));
    }

    /// Every serialised MPH blob is keyed on these hashes, so a hash that silently changed — a
    /// tweaked constant, a reordered finalizer, a byte-order slip in a refactor — would make every
    /// previously-saved index load wrong without any test failing. These pinned values turn that
    /// into a loud CI failure instead. **Do not "fix" them to match new output: changing the hash
    /// is a breaking blob-format change and must bump the format magic, not this table.**
    #[test]
    fn hash_key_is_stable() {
        assert_eq!(hash_key(""), 0xf52a_15e9_a9b5_e89b);
        assert_eq!(hash_key("a"), 0x02c0_bdbf_4814_20f8);
        assert_eq!(hash_key("apple"), 0xba8e_799d_ceb3_bcb1);
        assert_eq!(hash_key("GET"), 0xbc92_c6e8_93bb_a505);
        assert_eq!(hash_key("é中🎉"), 0x27bc_4d93_237a_01bc);
        assert_eq!(hash_key("member-00042"), 0xc0fb_e8c4_a80e_0db3);
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(fingerprint_full(""), 0x0000_0100_0000_09b3);
        assert_eq!(fingerprint_full("apple"), 0x627e_4f52_427c_b65d);
        assert_eq!(fingerprint_full("é中🎉"), 0x2a7b_2637_5d6e_2054);
        // The table stores the low `b` bits of that value — the widths the indexes actually write.
        assert_eq!(fingerprint_full("GET") & 0xf, 0x2);
        assert_eq!(fingerprint_full("member-00042") & 0xffff, 0x8fa1);
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
