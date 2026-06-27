//! Ordered string↔id index backed by a finite-state transducer ([`fst::Map`]).
//!
//! Keys are stored in lexicographic order and assigned dense ids `0..n` by that order; the FST gives
//! compressed `key → id` with prefix and range iteration, and a [`StringArena`] gives `O(1)`
//! `id → key`. The whole index serialises to a flat, relocatable blob.

use crate::arena::StringArena;
use crate::IndexError;
use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};

const MAGIC: &[u8; 4] = b"BIX1";

/// An immutable, ordered string↔id index.
pub struct StringIndex {
    map: Map<Vec<u8>>,
    arena: StringArena,
}

impl StringIndex {
    /// Build an index from a collection of strings. Duplicates are removed and the keys are sorted;
    /// the id of a key is its rank in sorted order, so ids are stable for the same key set.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut keys: Vec<String> = items.into_iter().map(|s| s.as_ref().to_owned()).collect();
        keys.sort_unstable();
        keys.dedup();
        let mut builder = MapBuilder::memory();
        for (i, k) in keys.iter().enumerate() {
            builder.insert(k.as_bytes(), i as u64)?;
        }
        let map = Map::new(builder.into_inner()?)?;
        let arena = StringArena::build(&keys);
        Ok(Self { map, arena })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the index has no keys.
    pub fn is_empty(&self) -> bool {
        self.map.len() == 0
    }

    /// Id of `key`, or `None` if absent.
    pub fn id(&self, key: &str) -> Option<u64> {
        self.map.get(key)
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.map.get(key).is_some()
    }

    /// Key for `id`, or `None` if out of range.
    pub fn key(&self, id: u64) -> Option<&str> {
        self.arena.get(id as usize)
    }

    /// All `(key, id)` pairs whose key starts with `prefix`, in lexicographic order.
    pub fn prefix(&self, prefix: &str) -> Vec<(String, u64)> {
        // The `fst` streamer borrows per-call, so the collection loop is inlined (it cannot be
        // abstracted behind a helper returning owned data without a lifetime conflict).
        let mut out = Vec::new();
        let mut stream = self
            .map
            .search(Str::new(prefix).starts_with())
            .into_stream();
        while let Some((k, v)) = stream.next() {
            out.push((String::from_utf8_lossy(k).into_owned(), v));
        }
        out
    }

    /// All `(key, id)` pairs with `lo ≤ key < hi`, in lexicographic order.
    pub fn range(&self, lo: &str, hi: &str) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stream = self.map.range().ge(lo).lt(hi).into_stream();
        while let Some((k, v)) = stream.next() {
            out.push((String::from_utf8_lossy(k).into_owned(), v));
        }
        out
    }

    /// Serialise to a self-describing blob: `[magic 4][map_len u64][fst bytes][arena bytes]`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let map_bytes = self.map.as_fst().as_bytes();
        let mut out = Vec::with_capacity(12 + map_bytes.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(map_bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(map_bytes);
        out.extend_from_slice(&self.arena.to_bytes());
        out
    }

    /// Reconstruct an index from [`StringIndex::to_bytes`] output.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() < 12 || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let map_len = u64::from_le_bytes(bytes[4..12].try_into().unwrap()) as usize;
        let map_end = 12usize
            .checked_add(map_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format("fst length out of range"))?;
        let map = Map::new(bytes[12..map_end].to_vec())?;
        let arena = StringArena::from_bytes(&bytes[map_end..])?;
        if map.len() != arena.len() {
            return Err(IndexError::Format("fst / arena length mismatch"));
        }
        Ok(Self { map, arena })
    }

    /// Write the index to `path` (see [`StringIndex::to_bytes`]).
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes())?;
        Ok(())
    }

    /// Load an index previously written with [`StringIndex::save`].
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StringIndex {
        StringIndex::build(["banana", "apple", "apricot", "cherry", "apple"]).unwrap()
    }

    #[test]
    fn ids_are_sorted_rank_and_reversible() {
        let idx = sample();
        assert_eq!(idx.len(), 4); // duplicate "apple" deduped
        assert!(!idx.is_empty());
        // sorted: apple(0) apricot(1) banana(2) cherry(3)
        assert_eq!(idx.id("apple"), Some(0));
        assert_eq!(idx.id("banana"), Some(2));
        assert_eq!(idx.id("missing"), None);
        assert!(idx.contains("cherry") && !idx.contains("durian"));
        assert_eq!(idx.key(1), Some("apricot"));
        assert_eq!(idx.key(99), None);
    }

    #[test]
    fn prefix_and_range_queries() {
        let idx = sample();
        let ap: Vec<String> = idx.prefix("ap").into_iter().map(|(k, _)| k).collect();
        assert_eq!(ap, vec!["apple", "apricot"]);
        assert!(idx.prefix("z").is_empty());
        let r: Vec<String> = idx
            .range("apricot", "cherry")
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(r, vec!["apricot", "banana"]); // [lo, hi)
    }

    #[test]
    fn roundtrips_through_bytes() {
        let idx = sample();
        let restored = StringIndex::from_bytes(&idx.to_bytes()).unwrap();
        assert_eq!(restored.len(), idx.len());
        for k in ["apple", "apricot", "banana", "cherry"] {
            assert_eq!(restored.id(k), idx.id(k));
        }
        assert_eq!(restored.key(3), Some("cherry"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = sample();
        let path = std::env::temp_dir().join(format!("betula_index_{}.bix", std::process::id()));
        idx.save(&path).unwrap();
        let loaded = StringIndex::load(&path).unwrap();
        assert_eq!(loaded.id("banana"), Some(2));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rejects_corrupt_buffers() {
        assert!(StringIndex::from_bytes(b"nope").is_err());
        let mut good = sample().to_bytes();
        good[0] = b'X'; // break the magic
        assert!(StringIndex::from_bytes(&good).is_err());
    }

    #[test]
    fn empty_index() {
        let idx = StringIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(idx.id("x"), None);
        assert_eq!(idx.key(0), None);
        assert!(StringIndex::from_bytes(&idx.to_bytes()).unwrap().is_empty());
    }
}
