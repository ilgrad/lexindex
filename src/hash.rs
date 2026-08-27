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

/// A **fingerprint** of the low `bits` bits, from a *separate* hash of the key (a different
/// basis/multiplier than [`hash_key`], so it is uncorrelated with the slot hash for well-distributed
/// keys). That decorrelation is what makes the chance a non-member both lands on a used slot and
/// matches its fingerprint about `2^-bits` — the tunable false-positive rate. It is an upper bound,
/// not an equality: a non-member whose raw slot falls past the remap is rejected before the
/// fingerprint is ever compared. `bits ∈ 1..=64`. Not a security primitive — both hashes are
/// deterministic and unseeded, so an adversary who picks the queries can search for collisions.
pub(crate) fn fingerprint_bits(s: &str, bits: u32) -> u64 {
    let h = fingerprint_full(s);
    if bits >= 64 {
        h
    } else {
        h & ((1u64 << bits) - 1)
    }
}

/// The untruncated 64-bit second hash behind [`fingerprint_bits`]. The compact index's collision
/// side table stores this full value rather than the table's truncated width, so two distinct keys
/// merge only when they collide in *both* 64-bit hashes at once (~`2^-128` per pair) — not at the
/// `2^-(64+bits)` a truncated side match would allow.
pub(crate) fn fingerprint_full(s: &str) -> u64 {
    let mut h: u64 = 0x0000_0100_0000_01b3; // distinct basis from hash_key
    for &b in s.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15); // golden-ratio odd multiplier
    }
    h ^ (h >> 29)
}

/// Streaming hash over a byte stream fed in arbitrary chunks — the whole-payload integrity check
/// of the v4 blob formats. Consumes 8-byte words (one multiply per word, ~8× the byte-serial
/// [`hash_bytes`] on large payloads), buffers across chunk boundaries so section splits never
/// change the result, and folds the total length into the finalizer so a trailing zero-pad cannot
/// alias a shorter stream. Version-stable like [`hash_key`]: written blobs pin it forever. Like the
/// header check, it guards **accidental** corruption, not a crafted blob — it is public and
/// deterministic, so an attacker can recompute it.
pub(crate) struct BlockHasher {
    h: u64,
    buf: [u8; 8],
    buf_len: usize,
    total: u64,
}

impl BlockHasher {
    pub(crate) fn new() -> Self {
        Self {
            h: 0x9e37_79b9_7f4a_7c15, // golden-ratio basis, distinct from hash_bytes'
            buf: [0; 8],
            buf_len: 0,
            total: 0,
        }
    }

    #[inline]
    fn word(&mut self, w: u64) {
        self.h = (self.h ^ w).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        self.h ^= self.h >> 29;
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        if self.buf_len > 0 {
            let take = bytes.len().min(8 - self.buf_len);
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&bytes[..take]);
            self.buf_len += take;
            if self.buf_len < 8 {
                return; // `bytes` exhausted without completing the pending word
            }
            bytes = &bytes[take..];
            self.word(u64::from_le_bytes(self.buf));
        }
        let mut words = bytes.chunks_exact(8);
        for w in &mut words {
            self.word(u64::from_le_bytes(w.try_into().unwrap()));
        }
        let tail = words.remainder();
        self.buf[..tail.len()].copy_from_slice(tail);
        self.buf_len = tail.len();
    }

    pub(crate) fn finish(mut self) -> u64 {
        if self.buf_len > 0 {
            self.buf[self.buf_len..].fill(0);
            let w = u64::from_le_bytes(self.buf);
            self.word(w);
        }
        // splitmix64 finalizer over (state ^ length): a zero-padded tail differs from genuine
        // zeros because the lengths differ.
        let mut h = self.h ^ self.total;
        h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        h ^ (h >> 31)
    }
}

/// [`BlockHasher`] over one contiguous slice.
pub(crate) fn hash_block(bytes: &[u8]) -> u64 {
    let mut h = BlockHasher::new();
    h.update(bytes);
    h.finish()
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

/// Two distinct strings with equal [`hash_key`], found offline by a Pollard-rho birthday search
/// (`local/hashcollide`, ~2^32 map steps). They drive the side-table tests in both MPH indexes;
/// the golden test below pins the collision itself, so a changed hash breaks loudly here before
/// anything subtle happens in tests built on the pair.
#[cfg(test)]
pub(crate) const COLLIDING_PAIR: (&str, &str) = ("x5iojurfgtipm", "7gvob4sxctomf");

#[cfg(test)]
mod golden {
    use super::{BlockHasher, fingerprint_bits, hash_block, hash_key};

    #[test]
    fn the_pinned_collision_pair_still_collides() {
        let (a, b) = super::COLLIDING_PAIR;
        assert_ne!(a, b);
        assert_eq!(hash_key(a), hash_key(b));
        assert_eq!(hash_key(a), 0x156a_c9d1_f216_0cbf);
        // The pair collides in the slot hash only — the independent fingerprint tells them apart.
        assert_ne!(fingerprint_bits(a, 64), fingerprint_bits(b, 64));
    }

    /// The v4 payload check is part of the blob format: pinned like the key hashes below.
    #[test]
    fn block_hash_is_stable() {
        assert_eq!(hash_block(b""), 0xe220_a839_7b1d_cdaf);
        assert_eq!(hash_block(b"a"), 0x8b92_4b9e_e3ce_42da);
        assert_eq!(hash_block(b"12345678"), 0xd56d_dc08_eb9e_e133);
        assert_eq!(hash_block(b"123456789"), 0xaa5d_e941_889d_7528);
        let long: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
        assert_eq!(hash_block(&long), 0x3ade_b0bd_009e_9a90);
    }

    /// Chunk boundaries must never change the digest (sections are streamed in arbitrary splits),
    /// and a zero-padded tail must not alias genuine zeros.
    #[test]
    fn block_hash_is_split_invariant() {
        let data: Vec<u8> = (0..255u8).cycle().take(4097).collect();
        let whole = hash_block(&data);
        for split in [0usize, 1, 7, 8, 9, 63, 4096, 4097] {
            let mut h = BlockHasher::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finish(), whole, "split at {split}");
        }
        let mut three = BlockHasher::new();
        for chunk in data.chunks(11) {
            three.update(chunk);
        }
        assert_eq!(three.finish(), whole);
        assert_ne!(hash_block(b"ab"), hash_block(b"ab\0"));
        assert_ne!(hash_block(b""), hash_block(b"\0"));
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
        assert_eq!(fingerprint_bits("", 64), 0x0000_0100_0000_09b3);
        assert_eq!(fingerprint_bits("apple", 64), 0x627e_4f52_427c_b65d);
        assert_eq!(fingerprint_bits("é中🎉", 64), 0x2a7b_2637_5d6e_2054);
        assert_eq!(fingerprint_bits("GET", 4), 0x2);
        assert_eq!(fingerprint_bits("member-00042", 16), 0x8fa1);
        // A narrower width is exactly the low bits of the full 64-bit fingerprint.
        for s in ["", "a", "apple", "GET", "é中🎉"] {
            let full = fingerprint_bits(s, 64);
            for b in [1u32, 4, 8, 16, 32] {
                assert_eq!(
                    fingerprint_bits(s, b),
                    full & ((1u64 << b) - 1),
                    "{s:?} b={b}"
                );
            }
        }
    }
}
