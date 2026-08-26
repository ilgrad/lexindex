//! Deterministic, **version-stable** key hashes shared by the minimal-perfect-hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence.

/// FNV-1a over the bytes, then a splitmix64 finalizer for avalanche (so structured keys like
/// `"key_0001"` still spread evenly across the MPH's buckets). This drives the perfect-hash **slot**.
pub(crate) fn hash_key(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9); // splitmix64 finalizer
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// A **fingerprint** of the low `fp_bytes` bytes, from an *independent* hash of the key (a different
/// basis/multiplier than [`hash_key`]). Independence from the slot hash is what makes the collision
/// probability of a non-member landing on a member's slot ≈ `256^-fp_bytes`. `fp_bytes ∈ {1, 2, 4}`.
pub(crate) fn fingerprint(s: &str, fp_bytes: usize) -> u64 {
    let mut h: u64 = 0x0000_0100_0000_01b3; // distinct basis from hash_key
    for &b in s.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15); // golden-ratio odd multiplier
    }
    h ^= h >> 29;
    let bits = fp_bytes * 8;
    if bits >= 64 {
        h
    } else {
        h & ((1u64 << bits) - 1)
    }
}

use ptr_hash::{DefaultPtrHash, PtrHash, PtrHashParams};

/// Build the MPH with `default_compact` parameters (λ=3.9): measured 2.17 bits/key on real words
/// vs 2.41 for the λ=3.5 default, with identical query time at 480k and 5M keys. Compact
/// construction can occasionally fail (pilot eviction chains grow too long), so fall back to the
/// default parameters; both produce the same `DefaultPtrHash` type, so blobs stay compatible
/// either way.
pub(crate) fn build_mph(hashes: &[u64]) -> DefaultPtrHash {
    PtrHash::try_new(hashes, PtrHashParams::default_compact())
        .unwrap_or_else(|| PtrHash::new(hashes, PtrHashParams::default()))
}
