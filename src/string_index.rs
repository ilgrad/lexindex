//! Ordered string↔id index backed by a finite-state transducer ([`fst::Map`]).
//!
//! Keys are stored in lexicographic order and assigned dense ids `0..n` by that order. The FST gives
//! compressed `key → id` with prefix / range / fuzzy iteration; `id → key` is reconstructed by walking
//! the same transducer by rank (the ids *are* the FST outputs), so **no separate reverse map is
//! stored** — the whole index is a single FST, which roughly halves the serialised size. It
//! serialises to a flat, relocatable blob.

use crate::IndexError;
use crate::blob::SharedBytes;
use fst::automaton::{Automaton, Levenshtein, Str, Subsequence};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};

const MAGIC: &[u8; 4] = b"BIX4";

/// An immutable, ordered string↔id index — a single finite-state transducer.
pub struct StringIndex {
    map: Map<SharedBytes>,
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
        let map = Map::new(SharedBytes::from_owned(builder.into_inner()?))?;
        Ok(Self { map })
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
    ///
    /// The id is the key's sorted rank, and the FST stores each key's rank as its transducer output,
    /// so the key is reconstructed by walking the FST from the root: at each node take the last
    /// transition whose accumulated output is `<= id` (that subtree's minimum rank), stopping when a
    /// final state's total output equals `id`. This is `O(key length)` and needs no separate reverse
    /// map — the returned `String` is decoded on the fly (forward lookups borrow; this one rebuilds).
    pub fn key(&self, id: u64) -> Option<String> {
        let fst = self.map.as_fst();
        let mut node = fst.root();
        let mut acc: u64 = 0;
        let mut key: Vec<u8> = Vec::new();
        loop {
            if node.is_final() && acc + node.final_output().value() == id {
                return String::from_utf8(key).ok();
            }
            // Transitions are in increasing byte order — increasing subtree-minimum rank. The subtree
            // holding `id` is the last one whose minimum (`acc + out`) does not exceed it.
            let mut chosen = None;
            for i in 0..node.len() {
                let t = node.transition(i);
                if acc + t.out.value() <= id {
                    chosen = Some(t);
                } else {
                    break;
                }
            }
            let t = chosen?; // no transition qualifies ⇒ `id` is out of range
            acc += t.out.value();
            key.push(t.inp);
            node = fst.node(t.addr);
        }
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

    /// All `(key, id)` pairs within Levenshtein edit distance `max_distance` of `query`, in
    /// lexicographic order — typo-tolerant lookup / fuzzy autocomplete. The whole FST is walked by the
    /// edit-distance automaton (no full scan of the key set). Returns [`IndexError::Automaton`] if the
    /// automaton for this `query` and `max_distance` would be too large (lower `max_distance`).
    pub fn fuzzy(&self, query: &str, max_distance: u32) -> Result<Vec<(String, u64)>, IndexError> {
        let lev = Levenshtein::new(query, max_distance)
            .map_err(|e| IndexError::Automaton(e.to_string()))?;
        let mut out = Vec::new();
        let mut stream = self.map.search(&lev).into_stream();
        while let Some((k, v)) = stream.next() {
            out.push((String::from_utf8_lossy(k).into_owned(), v));
        }
        Ok(out)
    }

    /// All `(key, id)` pairs whose key contains `query` as a subsequence — its characters appear in
    /// order but not necessarily contiguously (e.g. `"ace"` matches `"abcde"`) — in lexicographic
    /// order. Useful for fuzzy/abbreviation matching.
    pub fn subsequence(&self, query: &str) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stream = self.map.search(Subsequence::new(query)).into_stream();
        while let Some((k, v)) = stream.next() {
            out.push((String::from_utf8_lossy(k).into_owned(), v));
        }
        out
    }

    /// The smallest `(key, id)` with `key >= query` (the *successor*), or `None` if every key is
    /// smaller. `O(query length)` — it seeks the FST, never scans the key set.
    pub fn successor(&self, query: &str) -> Option<(String, u64)> {
        let mut stream = self.map.range().ge(query).into_stream();
        stream
            .next()
            .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
    }

    /// The largest `(key, id)` with `key <= query` (the *predecessor*), or `None` if every key is
    /// larger. `O(query length)`: if `query` is present it is its own predecessor; otherwise the answer
    /// sits one rank below the smallest key greater than `query` (ids are the sorted rank).
    pub fn predecessor(&self, query: &str) -> Option<(String, u64)> {
        if let Some(id) = self.id(query) {
            return Some((query.to_owned(), id));
        }
        // Rank of the smallest key strictly greater than `query` == number of keys below `query`.
        let mut stream = self.map.range().gt(query).into_stream();
        let rank_above = stream.next().map_or(self.len() as u64, |(_, v)| v);
        rank_above
            .checked_sub(1)
            .and_then(|r| self.key(r).map(|k| (k, r)))
    }

    /// All `(key, id)` pairs in lexicographic (= id) order, **lazily**: keys are decoded one at a time
    /// by the rank-walk, so nothing is materialised up front. Prefer this to `prefix("")` when the index
    /// is large. Each step is `O(key length)`.
    pub fn iter(&self) -> impl Iterator<Item = (String, u64)> + '_ {
        // `fst` streams are `Streamer`, not `Iterator`; adapt one via `from_fn`, decoding to owned data
        // so no borrow of the stream escapes.
        let mut stream = self.map.stream();
        std::iter::from_fn(move || {
            stream
                .next()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
        })
    }

    /// Serialise to a self-describing blob: `[magic 4][fst bytes]` — the FST *is* the whole index.
    pub fn to_bytes(&self) -> Vec<u8> {
        let map_bytes = self.map.as_fst().as_bytes();
        let mut out = Vec::with_capacity(4 + map_bytes.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(map_bytes);
        out
    }

    /// Reconstruct an index from [`StringIndex::to_bytes`] output (copies the blob into owned memory).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// Reconstruct from a shared byte source, borrowing the FST from it without copying. Backs both the
    /// owned [`from_bytes`](StringIndex::from_bytes) and the zero-copy
    /// [`load_mmap`](StringIndex::load_mmap).
    fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < 4 || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let map = Map::new(
            blob.subslice(4, blob.len())
                .ok_or(IndexError::Format("fst range out of range"))?,
        )?;
        Ok(Self { map })
    }

    /// Write the index to `path` (see [`StringIndex::to_bytes`]).
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes())?;
        Ok(())
    }

    /// Load an index previously written with [`StringIndex::save`] (reads the whole file into memory).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// **Zero-copy load**: memory-map the file and borrow the index directly from the mapped pages —
    /// no read into RAM, so a multi-gigabyte index is ready instantly and its pages are shared across
    /// processes by the OS page cache. `key(id)` still returns an owned `String`; all other queries
    /// borrow the map.
    ///
    /// # Safety / caveat
    /// Memory-mapping trusts the file to stay unchanged: another process truncating or overwriting it
    /// while the index is alive would make the borrowed bytes unsound. lexindex blobs are written once
    /// and are immutable — do not modify the file for the lifetime of the returned index.
    #[cfg(feature = "mmap")]
    pub fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: see the caveat above — the mapped file must not be mutated while it is mapped.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)))
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
        assert_eq!(idx.key(1).as_deref(), Some("apricot"));
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
    fn fuzzy_search_tolerates_typos() {
        let idx = sample(); // apple, apricot, banana, cherry
        // one insertion away from "apple"
        let near: Vec<String> = idx
            .fuzzy("aple", 1)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(near, vec!["apple"]);
        // one deletion away from "apricot"
        assert!(
            idx.fuzzy("aprcot", 2)
                .unwrap()
                .iter()
                .any(|(k, _)| k == "apricot")
        );
        // distance 0 is exact: a non-key returns nothing, a key returns itself with its id
        assert!(idx.fuzzy("zzz", 0).unwrap().is_empty());
        assert_eq!(
            idx.fuzzy("banana", 0).unwrap(),
            vec![("banana".to_string(), 2)]
        );
    }

    #[test]
    fn subsequence_matches_non_contiguous() {
        let idx = sample();
        // "ap" is an (in-order) subsequence of apple and apricot only
        let ap: Vec<String> = idx.subsequence("ap").into_iter().map(|(k, _)| k).collect();
        assert_eq!(ap, vec!["apple", "apricot"]);
        // "ae" matches apple (a…e) but not apricot (no trailing e)
        let ae: Vec<String> = idx.subsequence("ae").into_iter().map(|(k, _)| k).collect();
        assert_eq!(ae, vec!["apple"]);
    }

    #[test]
    fn predecessor_successor_and_iter() {
        let idx = sample(); // apple(0) apricot(1) banana(2) cherry(3)
        // successor: smallest key >= query
        assert_eq!(idx.successor("apple"), Some(("apple".into(), 0))); // present -> itself
        assert_eq!(idx.successor("ba"), Some(("banana".into(), 2))); // between apricot and banana
        assert_eq!(idx.successor("a"), Some(("apple".into(), 0))); // before all -> first
        assert_eq!(idx.successor("zzz"), None); // after all
        // predecessor: largest key <= query
        assert_eq!(idx.predecessor("cherry"), Some(("cherry".into(), 3))); // present -> itself
        assert_eq!(idx.predecessor("ba"), Some(("apricot".into(), 1))); // between apricot and banana
        assert_eq!(idx.predecessor("zzz"), Some(("cherry".into(), 3))); // after all -> last
        assert_eq!(idx.predecessor("a"), None); // before all
        // iter yields every (key, id) in sorted order, lazily
        let all: Vec<(String, u64)> = idx.iter().collect();
        assert_eq!(
            all,
            vec![
                ("apple".into(), 0),
                ("apricot".into(), 1),
                ("banana".into(), 2),
                ("cherry".into(), 3),
            ]
        );
        // empty index has neither neighbour and an empty iterator
        let empty = StringIndex::build(Vec::<String>::new()).unwrap();
        assert_eq!(empty.successor("x"), None);
        assert_eq!(empty.predecessor("x"), None);
        assert_eq!(empty.iter().count(), 0);
    }

    #[test]
    fn roundtrips_through_bytes() {
        let idx = sample();
        let restored = StringIndex::from_bytes(&idx.to_bytes()).unwrap();
        assert_eq!(restored.len(), idx.len());
        for k in ["apple", "apricot", "banana", "cherry"] {
            assert_eq!(restored.id(k), idx.id(k));
        }
        assert_eq!(restored.key(3).as_deref(), Some("cherry"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = sample();
        let path = std::env::temp_dir().join(format!("lexindex_{}.bix", std::process::id()));
        idx.save(&path).unwrap();
        let loaded = StringIndex::load(&path).unwrap();
        assert_eq!(loaded.id("banana"), Some(2));
        std::fs::remove_file(&path).ok();
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_matches_owned_load() {
        // A non-trivial catalog so the id->key rank-walk descends several FST levels.
        let keys: Vec<String> = (0..50).map(|i| format!("entity-{i:04}")).collect();
        let idx = StringIndex::build(&keys).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_mmap_{}.bix", std::process::id()));
        idx.save(&path).unwrap();
        let mapped = StringIndex::load_mmap(&path).unwrap();
        assert_eq!(mapped.len(), idx.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(mapped.id(k), Some(i as u64)); // forward borrows the mapped FST
            assert_eq!(mapped.key(i as u64).as_deref(), Some(k.as_str())); // reverse decodes from the map
        }
        assert_eq!(mapped.prefix("entity-001").len(), 10); // 0010..0019
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

    #[test]
    fn key_rank_walk_handles_prefixes_and_multibyte() {
        // The id->key rank-walk over the FST must reconstruct every key, including keys that are
        // prefixes of each other (the final-state case) and multibyte UTF-8 boundaries.
        let raw = [
            "a",
            "ab",
            "abc",
            "abcd",
            "b",
            "ba",
            "cat",
            "catalog",
            "cats",
            "entity-0000",
            "entity-0001",
            "entity-0010",
            "naïve",
            "naïveté",
            "zzz",
        ];
        let idx = StringIndex::build(raw).unwrap();
        let mut sorted: Vec<&str> = raw.to_vec();
        sorted.sort_unstable();
        for (i, k) in sorted.iter().enumerate() {
            assert_eq!(idx.id(k), Some(i as u64));
            assert_eq!(idx.key(i as u64).as_deref(), Some(*k)); // rank-walk round-trips
        }
        assert_eq!(idx.key(sorted.len() as u64), None); // out of range
    }
}
