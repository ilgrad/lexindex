//! Minimal-perfect-hash dictionary backed by [`ptr_hash`].
//!
//! For a fixed set of `n` distinct strings, a minimal perfect hash maps each to a distinct slot in
//! `[0, n)` with no gaps and near-`O(1)` lookup in tiny space. `ptr_hash` builds the MPH; we key it on
//! a deterministic 64-bit hash of each string (so queries take `&str` without allocating) and keep a
//! [`StringArena`] from slot → key. The arena doubles as a **membership check**: an MPH returns a slot
//! for *any* input, so a query is only a hit if the stored key at that slot equals the query.
//!
//! Build fails (rather than silently corrupting) on the astronomically rare event that two distinct
//! keys collide in the 64-bit hash — reach for [`crate::StringIndex`] or rebuild in that case.

use crate::arena::StringArena;
use crate::IndexError;
use ptr_hash::{DefaultPtrHash, PtrHash, PtrHashParams};

/// Deterministic 64-bit hash of a string (fixed-seed `SipHash`, stable within a process).
fn hash_key(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// An immutable minimal-perfect-hash dictionary: fastest exact `string → dense id` with reverse lookup.
pub struct PerfectHashIndex {
    mph: Option<DefaultPtrHash>, // None iff empty (ptr_hash needs a non-empty key set)
    arena: StringArena,          // slot → key (also verifies membership)
    n: usize,
}

impl PerfectHashIndex {
    /// Build from a collection of strings. Duplicates are removed; ids are arbitrary slots in `[0, n)`
    /// (no defined order — use [`crate::StringIndex`] when order matters).
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut keys: Vec<String> = items.into_iter().map(|s| s.as_ref().to_owned()).collect();
        keys.sort_unstable();
        keys.dedup();
        let n = keys.len();
        if n == 0 {
            return Ok(Self {
                mph: None,
                arena: StringArena::default(),
                n: 0,
            });
        }
        let hashes: Vec<u64> = keys.iter().map(|k| hash_key(k)).collect();
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(IndexError::Format(
                "perfect-hash: 64-bit key-hash collision; rebuild or use StringIndex",
            ));
        }
        let mph: DefaultPtrHash = PtrHash::new(&hashes, PtrHashParams::default());
        let mut by_slot: Vec<Option<String>> = (0..n).map(|_| None).collect();
        for (k, h) in keys.iter().zip(&hashes) {
            let slot = mph.index(h);
            if slot >= n || by_slot[slot].is_some() {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = Some(k.clone());
        }
        let arena = StringArena::build(by_slot.into_iter().map(|o| o.unwrap()));
        Ok(Self {
            mph: Some(mph),
            arena,
            n,
        })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the dictionary has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Dense id of `key`, or `None` if absent (membership is verified against the stored key).
    pub fn id(&self, key: &str) -> Option<u32> {
        let mph = self.mph.as_ref()?;
        let slot = mph.index(&hash_key(key));
        if slot < self.n && self.arena.get(slot) == Some(key) {
            Some(slot as u32)
        } else {
            None
        }
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Key for `id`, or `None` if out of range.
    pub fn key(&self, id: u32) -> Option<&str> {
        self.arena.get(id as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_reverse_and_membership() {
        let words = ["alpha", "beta", "gamma", "delta", "alpha"];
        let idx = PerfectHashIndex::build(words).unwrap();
        assert_eq!(idx.len(), 4); // deduped
        assert!(!idx.is_empty());
        for w in ["alpha", "beta", "gamma", "delta"] {
            let id = idx.id(w).expect("present");
            assert!((id as usize) < idx.len());
            assert_eq!(idx.key(id), Some(w)); // round-trips through the slot
            assert!(idx.contains(w));
        }
        assert_eq!(idx.id("epsilon"), None); // absent → verified miss
        assert!(!idx.contains("epsilon"));
        assert_eq!(idx.key(99), None);
    }

    #[test]
    fn ids_are_a_dense_permutation() {
        let words: Vec<String> = (0..500).map(|i| format!("key_{i:04}")).collect();
        let idx = PerfectHashIndex::build(&words).unwrap();
        let mut ids: Vec<u32> = words.iter().map(|w| idx.id(w).unwrap()).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..500).collect::<Vec<u32>>()); // exactly 0..n, no gaps or repeats
    }

    #[test]
    fn empty_dictionary() {
        let idx = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(idx.id("x"), None);
        assert_eq!(idx.key(0), None);
    }
}
