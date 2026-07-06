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
use crate::blob::SharedBytes;
use crate::hash::hash_key;
use crate::IndexError;
use epserde::prelude::*;
use ptr_hash::{DefaultPtrHash, PtrHash, PtrHashParams};

const MPH_MAGIC: &[u8; 4] = b"BMP1";

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
                arena: StringArena::build(Vec::<&str>::new()), // offsets = [0]: a valid empty arena
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

    /// Dense id of `key` **without** verifying membership: `key` MUST be one of the built keys, or the
    /// result is an arbitrary (but valid) slot in `[0, n)`. Skips the stored-key comparison that [`id`]
    /// does, so it is the fastest possible lookup — use it for a **fixed/closed vocabulary** (the
    /// canonical hot-path use of a perfect hash), where membership is already guaranteed. Returns `0`
    /// for an empty dictionary.
    ///
    /// [`id`]: PerfectHashIndex::id
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        match &self.mph {
            Some(mph) => mph.index(&hash_key(key)) as u32,
            None => 0,
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

    /// Serialise to a self-describing blob: `[magic 4][n u64][mph_len u64][mph epserde bytes][arena
    /// bytes]`. The MPH is serialised with [`epserde`]; reloading queries correctly because the key
    /// hash is version-stable.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let arena_buf = self.arena.to_bytes();
        let mut out = Vec::with_capacity(20 + mph_buf.len() + arena_buf.len());
        out.extend_from_slice(MPH_MAGIC);
        out.extend_from_slice(&(self.n as u64).to_le_bytes());
        out.extend_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(&arena_buf);
        Ok(out)
    }

    /// Reconstruct from [`PerfectHashIndex::to_bytes`] output. The lexindex framing (magic, lengths,
    /// arena offsets) is fully bounds-validated and never reads out of bounds, but the embedded minimal
    /// perfect hash is deserialised by [`epserde`]: feed only blobs produced by
    /// [`to_bytes`](Self::to_bytes) / [`save`](Self::save), since a corrupted MPH region may abort on a
    /// failed allocation — the same "trust your own blob" contract as [`load_mmap`](Self::load_mmap).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// Reconstruct from a shared byte source. The MPH structure (a few bytes/key) is deserialised into
    /// owned memory; the key arena — the bulk of the blob — is borrowed zero-copy from `blob`, so a
    /// memory-mapped load never copies it. Backs both `from_bytes` and `load_mmap`.
    fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < 20 || &bytes[0..4] != MPH_MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let n = u64::from_le_bytes(bytes[4..12].try_into().unwrap()) as usize;
        let mph_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap()) as usize;
        let mph_end = 20usize
            .checked_add(mph_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format("mph length out of range"))?;
        let mph = if n == 0 {
            None
        } else {
            let mut reader = &bytes[20..mph_end];
            Some(
                DefaultPtrHash::deserialize_full(&mut reader)
                    .map_err(|e| IndexError::Serde(e.to_string()))?,
            )
        };
        let arena = StringArena::from_shared(
            blob.subslice(mph_end, blob.len())
                .ok_or(IndexError::Format("arena range out of range"))?,
        )?;
        if arena.len() != n {
            return Err(IndexError::Format("mph / arena length mismatch"));
        }
        Ok(Self { mph, arena, n })
    }

    /// Write the dictionary to `path` (see [`PerfectHashIndex::to_bytes`]).
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes()?)?;
        Ok(())
    }

    /// Load a dictionary previously written with [`PerfectHashIndex::save`] (reads the whole file).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Memory-map the file and borrow the key arena (the bulk of the blob) zero-copy; only the small
    /// MPH structure is read into memory. See [`StringIndex::load_mmap`](crate::StringIndex::load_mmap)
    /// for the immutability caveat.
    #[cfg(feature = "mmap")]
    pub fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the mapped file must not be mutated while it is mapped (see StringIndex::load_mmap).
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)))
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
    fn id_unchecked_matches_id_for_members() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma", "delta"]).unwrap();
        for w in ["alpha", "beta", "gamma", "delta"] {
            assert_eq!(idx.id_unchecked(w), idx.id(w).unwrap()); // same slot, no verification
        }
        let empty = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        assert_eq!(empty.id_unchecked("x"), 0); // empty dictionary → 0
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

    #[test]
    fn round_trips_through_bytes() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma", "delta"]).unwrap();
        let restored = PerfectHashIndex::from_bytes(&idx.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.len(), idx.len());
        for w in ["alpha", "beta", "gamma", "delta"] {
            // the serialised MPH yields the same slot, and reverse lookup matches
            assert_eq!(restored.id(w), idx.id(w));
            assert_eq!(restored.key(idx.id(w).unwrap()), Some(w));
        }
        assert_eq!(restored.id("zeta"), None); // verified membership survives the round-trip
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = PerfectHashIndex::build(["GET", "POST", "PUT", "DELETE"]).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_mph_{}.bmp", std::process::id()));
        idx.save(&path).unwrap();
        let loaded = PerfectHashIndex::load(&path).unwrap();
        for w in ["GET", "POST", "PUT", "DELETE"] {
            assert_eq!(loaded.id(w), idx.id(w));
        }
        std::fs::remove_file(&path).ok();
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_matches_owned_load() {
        let words: Vec<String> = (0..64).map(|i| format!("token_{i:03}")).collect();
        let idx = PerfectHashIndex::build(&words).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_mph_mmap_{}.bmp", std::process::id()));
        idx.save(&path).unwrap();
        let mapped = PerfectHashIndex::load_mmap(&path).unwrap();
        assert_eq!(mapped.len(), idx.len());
        for w in &words {
            let id = mapped.id(w).expect("present"); // membership checks against the mapped arena
            assert_eq!(mapped.key(id), Some(w.as_str()));
        }
        assert!(!mapped.contains("token_999")); // verified miss survives the mmap load
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_round_trips_and_rejects_corrupt() {
        let empty = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        let restored = PerfectHashIndex::from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.id("x"), None);

        assert!(PerfectHashIndex::from_bytes(b"nope").is_err());
        let mut good = PerfectHashIndex::build(["a", "b"])
            .unwrap()
            .to_bytes()
            .unwrap();
        good[0] = b'X'; // break the magic
        assert!(PerfectHashIndex::from_bytes(&good).is_err());
    }
}
