//! Deterministic, **version-stable** key hashes shared by the minimal-perfect-hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence.

/// FNV-1a over the bytes, then a splitmix64 finalizer for avalanche (so structured keys like
/// `"key_0001"` still spread evenly across the MPH's buckets). This drives the perfect-hash **slot**.
#[inline]
pub(crate) fn hash_key(s: &str) -> u64 {
    hash_bytes(s.as_bytes())
}

/// [`hash_key`] over raw bytes. Also used as a 32-bit integrity check (its low half) of the
/// lexindex-owned blob header, so an accidentally corrupted `n` / width / `overflow_cap` fails
/// cleanly at load instead of steering queries with a bogus bound.
#[inline]
pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9); // splitmix64 finalizer
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// A **fingerprint** of the low `bits` bits, from an *independent* hash of the key (a different
/// basis/multiplier than [`hash_key`]). Independence from the slot hash is what makes the
/// probability that a non-member both lands on a used slot and matches its fingerprint `2^-bits` —
/// the tunable false-positive rate. `bits ∈ 1..=64`.
pub(crate) fn fingerprint_bits(s: &str, bits: u32) -> u64 {
    let mut h: u64 = 0x0000_0100_0000_01b3; // distinct basis from hash_key
    for &b in s.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15); // golden-ratio odd multiplier
    }
    h ^= h >> 29;
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
pub(crate) fn build_mph(hashes: &[u64]) -> Result<DefaultPtrHash, crate::IndexError> {
    PtrHash::try_new(hashes, PtrHashParams::default_compact())
        .or_else(|| PtrHash::try_new(hashes, PtrHashParams::default()))
        .ok_or(crate::IndexError::Build(
            "minimal-perfect-hash construction failed after exhausting its retry seeds",
        ))
}

/// The exact length of the MPH's internal remap vector: the largest member `raw_slot - n`, plus
/// one. ptr_hash's `index()` reads that remap *unchecked* (cacheline-ef `index_unchecked`), and the
/// remap only covers raw slots up to the last member-occupied one — so a non-member whose raw slot
/// lands in the trailing free zone indexes out of bounds: a debug assertion at best, undefined
/// behaviour in release. Queries bound the remap access with this cap and answer `None` outright
/// past it — a provably free slot cannot hold a member.
pub(crate) fn overflow_cap(mph: &DefaultPtrHash, hashes: &[u64], n: usize) -> u64 {
    let mut cap: u64 = 0;
    mph.index_stream::<32, false, _>(hashes.iter())
        .for_each(|raw| {
            if raw >= n {
                cap = cap.max((raw - n + 1) as u64);
            }
        });
    cap
}

/// Slot for a key hash, or `None` when the raw slot is past the MPH's remap — a trailing free slot
/// no member occupies, which ptr_hash's own `index()` would read out of bounds (unchecked).
/// This is the ONLY place `mph.index()` may be called on a possibly-non-member hash.
#[inline]
pub(crate) fn slot_for(mph: &DefaultPtrHash, n: usize, overflow_cap: u64, h: u64) -> Option<usize> {
    let raw = mph.index_no_remap(&h);
    if raw < n {
        Some(raw)
    } else if (raw - n) as u64 >= overflow_cap {
        None
    } else {
        Some(mph.index(&h)) // remapped; in bounds because the cap is the remap's exact length
    }
}

/// Batch [`slot_for`]: streams raw (non-remapped) slots with software prefetch, then triages the
/// rare `raw ≥ n` cases — `usize::MAX` marks a definite non-member past the remap.
pub(crate) fn triage_slots(
    mph: &DefaultPtrHash,
    n: usize,
    overflow_cap: u64,
    hashes: &[u64],
) -> Vec<usize> {
    let mut slots = Vec::with_capacity(hashes.len());
    mph.index_stream::<32, false, _>(hashes.iter())
        .for_each(|s| slots.push(s));
    for (i, slot) in slots.iter_mut().enumerate() {
        if *slot >= n {
            *slot = if (*slot - n) as u64 >= overflow_cap {
                usize::MAX
            } else {
                mph.index(&hashes[i])
            };
        }
    }
    slots
}
