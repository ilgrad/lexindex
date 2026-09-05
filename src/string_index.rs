//! Ordered string↔id index backed by a finite-state transducer ([`fst::Map`]).
//!
//! Keys are stored in lexicographic order and assigned dense ids `0..n` by that order. The FST gives
//! compressed `key → id` with prefix / range / fuzzy iteration; `id → key` is reconstructed by walking
//! the same transducer by rank (the ids *are* the FST outputs), so **no separate reverse map is
//! stored** — the whole index is a single FST, which roughly halves the serialised size. It
//! serialises to a flat, relocatable blob.

use crate::IndexError;
use crate::blob::SharedBytes;
use fst::automaton::{Automaton, Levenshtein, Str};
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
        // Sorted and deduplicated in place, comparing through `AsRef` rather than collecting owned
        // `String`s: the keys are copied once more into the structure below, so an intermediate
        // copy of the whole corpus bought nothing.
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        let mut builder = MapBuilder::memory();
        for (i, k) in keys.iter().enumerate() {
            builder.insert(k.as_ref().as_bytes(), i as u64)?;
        }
        let map = Map::new(SharedBytes::from_owned(builder.into_inner()?))?;
        Ok(Self { map })
    }

    /// Build from keys that are **already in ascending order**, without materialising them.
    ///
    /// [`build`](Self::build) has to collect its input into a `Vec` before it can sort it, so a
    /// caller who already has the keys ordered — a sorted file, a database cursor, the output of an
    /// external sort — pays for a second copy of the corpus it does not need. This one streams
    /// straight into the transducer. Adjacent duplicates are dropped exactly as `build` drops them
    /// after sorting, so for the same key set the two produce **byte-identical** blobs.
    ///
    /// The ordering is the caller's precondition and is checked anyway: the transducer builder
    /// refuses a key that does not exceed its predecessor, so an unsorted input returns an error
    /// naming the pair rather than building an index that answers wrongly. Ordering is by *bytes*,
    /// which for UTF-8 is the same as `str`'s `Ord` — a list sorted by a locale collation is not
    /// sorted for this purpose.
    ///
    /// One trap is worth naming because it is the natural use of this method: **keys composed from
    /// sorted parts are not themselves sorted** unless the separator is below every byte that can
    /// follow a part. Joining a sorted word list to itself with `.` produces `'tween-decks.&c`
    /// before `'tween.ARU`, because `-` (0x2D) is below `.` (0x2E) — walking the pairs in
    /// component order then hands this method a descending pair. A separator below every byte in
    /// the components (`\0` always qualifies) makes the composition order-preserving.
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let idx = StringIndex::build_sorted(["apple", "apricot", "apricot", "banana"]).unwrap();
    /// assert_eq!(idx.len(), 3);
    /// assert_eq!(idx.id("banana"), Some(2));
    /// ```
    pub fn build_sorted<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut builder = MapBuilder::memory();
        Self::insert_sorted(&mut builder, items)?;
        let map = Map::new(SharedBytes::from_owned(builder.into_inner()?))?;
        Ok(Self { map })
    }

    /// [`build_sorted`](Self::build_sorted) writing the blob straight to `path`, so the finished
    /// index never has to fit in memory either. Returns the number of distinct keys written.
    ///
    /// The bytes are exactly what [`to_bytes`](Self::to_bytes) would produce, written through the
    /// same atomic replace [`save`](Self::save) uses — a crash leaves either the previous file or
    /// nothing, never a half-written index. What is *not* held is the corpus and the transducer:
    /// `fst`'s builder keeps a bounded node registry and the rest goes to the writer, so peak memory
    /// is independent of the key count.
    ///
    /// The count is returned rather than left to a subsequent [`load`](Self::load) because a caller
    /// streaming keys it does not retain has no other way to learn how many were distinct.
    pub fn build_sorted_to_file<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut n = 0;
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            use std::io::Write;
            w.write_all(MAGIC)?;
            let mut builder = MapBuilder::new(&mut *w)?;
            n = Self::insert_sorted(&mut builder, items)?;
            builder.finish()?;
            Ok(())
        })?;
        Ok(n)
    }

    /// Feed an ascending key stream into `builder`, dropping adjacent duplicates and numbering what
    /// survives from 0. Returns how many keys were inserted.
    ///
    /// `prev` is a reused buffer rather than a clone per key: the duplicate test needs the previous
    /// key to outlive the item that produced it, and this is a streaming builder whose whole point
    /// is not allocating per key.
    fn insert_sorted<W, I, S>(builder: &mut MapBuilder<W>, items: I) -> Result<usize, IndexError>
    where
        W: std::io::Write,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut n = 0usize;
        let mut prev = String::new();
        for key in items {
            let key = key.as_ref();
            if n > 0 && key == prev {
                continue;
            }
            builder.insert(key.as_bytes(), n as u64)?;
            prev.clear();
            prev.push_str(key);
            n += 1;
        }
        Ok(n)
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
    /// The outputs are read from the blob, so the rank sums are checked: a value that would
    /// overflow answers `None` rather than wrapping into a wrong key.
    pub fn key(&self, id: u64) -> Option<String> {
        rank_walk(self.map.as_fst(), id)
    }

    /// All `(key, id)` pairs whose key starts with `prefix`, in lexicographic order.
    pub fn prefix(&self, prefix: &str) -> Vec<(String, u64)> {
        self.prefix_iter(prefix).collect()
    }

    /// Like [`prefix`](Self::prefix) but **lazy**: one key is decoded per step, so an autocomplete
    /// wanting the first handful never pays for the rest — `idx.prefix_iter("a").take(10)` walks ten
    /// keys, where `prefix("a")` materialises every match.
    pub fn prefix_iter<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (String, u64)> + 'a {
        let mut stream = self
            .map
            .search(Str::new(prefix).starts_with())
            .into_stream();
        // `fst` streams are `Streamer`, not `Iterator`; adapt by moving the stream into the closure
        // and decoding to owned data, so no borrow of the stream escapes.
        std::iter::from_fn(move || {
            stream
                .next()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
        })
    }

    /// All `(key, id)` pairs with `lo ≤ key < hi`, in lexicographic order.
    pub fn range(&self, lo: &str, hi: &str) -> Vec<(String, u64)> {
        self.range_iter(lo, hi).collect()
    }

    /// Like [`range`](Self::range) but **lazy** — see [`prefix_iter`](Self::prefix_iter).
    pub fn range_iter<'a>(
        &'a self,
        lo: &'a str,
        hi: &'a str,
    ) -> impl Iterator<Item = (String, u64)> + 'a {
        let mut stream = self.map.range().ge(lo).lt(hi).into_stream();
        std::iter::from_fn(move || {
            stream
                .next()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
        })
    }

    /// All `(key, id)` pairs within Levenshtein edit distance `max_distance` of `query`, in
    /// lexicographic order — typo-tolerant lookup / fuzzy autocomplete. The whole FST is walked by the
    /// edit-distance automaton (no full scan of the key set). Returns [`IndexError::Automaton`] if the
    /// automaton for this `query` and `max_distance` would be too large (lower `max_distance`).
    pub fn fuzzy(&self, query: &str, max_distance: u32) -> Result<Vec<(String, u64)>, IndexError> {
        Ok(self.fuzzy_iter(query, max_distance)?.collect())
    }

    /// Like [`fuzzy`](Self::fuzzy) but **lazy** — see [`prefix_iter`](Self::prefix_iter). The
    /// automaton is built eagerly (so a too-large one still errors up front); only the walk is lazy.
    pub fn fuzzy_iter(
        &self,
        query: &str,
        max_distance: u32,
    ) -> Result<impl Iterator<Item = (String, u64)> + '_, IndexError> {
        let lev = Levenshtein::new(query, max_distance)
            .map_err(|e| IndexError::Automaton(e.to_string()))?;
        // Pass the automaton by value so the stream owns it — a `&lev` would borrow a local.
        let mut stream = self.map.search(lev).into_stream();
        Ok(std::iter::from_fn(move || {
            stream
                .next()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
        }))
    }

    /// All `(key, id)` pairs whose key contains `query` as a subsequence — its characters appear in
    /// order but not necessarily contiguously (e.g. `"ace"` matches `"abcde"`) — in lexicographic
    /// order. Useful for fuzzy/abbreviation matching. Matching is by **character**, so a multi-byte
    /// character matches only as a whole (`"é"` does not match `"àΩ"`, whose bytes contain both of
    /// its own).
    ///
    /// Cost is not the `O(query length)` of the seeking queries: the automaton has no prefix to seek
    /// on, so the traversal visits the FST's nodes and is linear in the index, not in the answer.
    pub fn subsequence(&self, query: &str) -> Vec<(String, u64)> {
        self.subsequence_iter(query).collect()
    }

    /// Like [`subsequence`](Self::subsequence) but **lazy** — see
    /// [`prefix_iter`](Self::prefix_iter).
    pub fn subsequence_iter<'a>(
        &'a self,
        query: &'a str,
    ) -> impl Iterator<Item = (String, u64)> + 'a {
        let mut stream = self
            .map
            .search(crate::subsequence::UnicodeSubsequence::new(query))
            .into_stream();
        std::iter::from_fn(move || {
            stream
                .next()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v))
        })
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

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it;
    /// [`save`](Self::save) writes exactly this many.
    pub fn serialized_len(&self) -> usize {
        MAGIC.len() + self.map.as_fst().as_bytes().len()
    }

    /// Reconstruct an index from [`StringIndex::to_bytes`] output (copies the blob into owned memory).
    ///
    /// The whole FST is checksum-verified, which costs one `O(blob)` pass — the same order as the
    /// copy this method already makes; [`load_mmap`](Self::load_mmap) skips it to stay instant (see
    /// its caveat). That pass rejects **accidental** corruption: `fst`'s own reader validates only
    /// the framing and warns that a structurally-plausible but corrupt body "will probably panic"
    /// on traversal, and a flipped or truncated byte fails the checksum long before any traversal.
    ///
    /// It is not a defence against a **crafted** blob, and this method does not promise one. The
    /// checksum is public and deterministic, so bytes chosen to carry a matching one reach `fst`'s
    /// node decoder with an invalid body: a 44-byte input found by the fuzz target that used to
    /// live in `fuzz/` panics inside this crate's own rank spot-check, before the caller ever runs
    /// a query. `fst` is safe Rust throughout, so the worst case stays a panic or a wrong answer,
    /// never an out-of-bounds read — which is why this stays a safe `fn` while the perfect-hash
    /// loaders are `unsafe`. Blobs from an untrusted source need a check this crate cannot make
    /// for you.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true)
    }

    /// Reconstruct from a shared byte source, borrowing the FST from it without copying. Backs both the
    /// owned [`from_bytes`](StringIndex::from_bytes) and the zero-copy
    /// [`load_mmap`](StringIndex::load_mmap). `verify` runs the FST's `O(blob)` checksum pass — on
    /// for owned loads, off for mmap so it stays instant.
    fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < 4 || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let map = Map::new(
            blob.subslice(4, blob.len())
                .ok_or(IndexError::Format("fst range out of range"))?,
        )?;
        if verify {
            map.as_fst().verify()?;
            Self::verify_ranks(&map)?;
        }
        Ok(Self { map })
    }

    /// The lexindex invariant on top of a valid FST: the value stored for the `i`-th key in sorted
    /// order is exactly `i`. `fst` guarantees memory safety for any structurally valid map, but a
    /// map with other values would answer wrong ids and defeat the rank-walk. A full walk costs
    /// ~80 ns/key (measured: 0.7 → 40 ms on the 479 823-word dictionary, 58× the owned load), so
    /// this is a spot check at both ends instead: the first key's value must be 0 and the rank-walk
    /// to `len - 1` must succeed, which pins the range of the outputs. A map whose values are a
    /// *permutation* of the ranks still passes — and still cannot violate memory safety; it answers
    /// wrong ids, and [`key`](Self::key) checks its arithmetic rather than trusting it.
    fn verify_ranks(map: &Map<SharedBytes>) -> Result<(), IndexError> {
        let len = map.len() as u64;
        if len == 0 {
            return Ok(());
        }
        let first = map.stream().next().map(|(_, v)| v);
        if first != Some(0) || rank_walk(map.as_fst(), len - 1).is_none() {
            return Err(IndexError::Format(
                "string-index: FST values are not the sorted ranks",
            ));
        }
        Ok(())
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// straight from the FST's own buffer, so saving never assembles a serialised copy.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            use std::io::Write;
            w.write_all(MAGIC)?;
            w.write_all(self.map.as_fst().as_bytes())?;
            Ok(())
        })
    }

    /// Load an index previously written with [`StringIndex::save`] (reads the whole file into memory).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// **Zero-copy load**: memory-map the file and borrow the index directly from the mapped pages —
    /// no read into RAM, so a multi-gigabyte index is ready instantly and its pages are shared across
    /// processes by the OS page cache. `key(id)` still returns an owned `String`; all other queries
    /// borrow the map.
    ///
    /// # Safety
    /// The mapped bytes are borrowed, not copied, so the file **must not be modified or truncated
    /// by anyone — this process or another — for as long as the returned index (or anything derived
    /// from it) is alive**. A concurrent write through any handle changes memory Rust believes is
    /// immutable, and a truncation makes the mapping fault; neither is something this function can
    /// check, which is why it is `unsafe` (`memmap2::Mmap::map` is `unsafe` for exactly this
    /// reason). lexindex blobs are written once and never updated in place, so a normal
    /// `save` → `load_mmap` workflow satisfies this; publishing new versions under new paths, or
    /// [`load`](Self::load), keeps it safe without the obligation.
    #[cfg(feature = "mmap")]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded to this function's own contract — the caller guarantees the file is
        // not mutated while the mapping lives.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }
}

/// The rank-walk behind [`StringIndex::key`], over the raw FST so the load-time rank check can run
/// it before an index exists.
fn rank_walk(fst: &fst::raw::Fst<SharedBytes>, id: u64) -> Option<String> {
    let mut node = fst.root();
    let mut acc: u64 = 0;
    let mut key: Vec<u8> = Vec::new();
    loop {
        if node.is_final() && acc.checked_add(node.final_output().value()) == Some(id) {
            return String::from_utf8(key).ok();
        }
        // Transitions are in increasing byte order — increasing subtree-minimum rank. The subtree
        // holding `id` is the last one whose minimum (`acc + out`) does not exceed it; outputs
        // are non-decreasing in transition order, so binary search finds it. Dictionary FSTs
        // fan out ~50 ways near the root, where this beats the linear scan most (measured
        // 1.77× on whole-dictionary `keys_of`).
        let n = node.len();
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if acc
                .checked_add(node.transition(mid).out.value())
                .is_some_and(|min| min <= id)
            {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return None; // no transition qualifies ⇒ `id` is out of range
        }
        let t = node.transition(lo - 1);
        acc = acc.checked_add(t.out.value())?;
        key.push(t.inp);
        node = fst.node(t.addr);
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

    /// A multi-byte query character must match a whole haystack character, not its bytes scattered
    /// across two: `é` is `[C3 A9]` and `àΩ` is `[C3 A0 CE A9]`, which a byte-level subsequence
    /// automaton (fst's own) accepts.
    #[test]
    fn subsequence_is_character_aligned() {
        let idx = StringIndex::build(["àΩ", "café", "èé", "ĉ"]).unwrap();
        let hits: Vec<String> = idx.subsequence("é").into_iter().map(|(k, _)| k).collect();
        assert_eq!(hits, vec!["café".to_string(), "èé".to_string()]);
        assert!(idx.subsequence("Ω").iter().any(|(k, _)| k == "àΩ")); // whole characters still match
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
    fn lazy_iterators_match_their_eager_forms() {
        let keys: Vec<String> = (0..300).map(|i| format!("item-{i:04}")).collect();
        let idx = StringIndex::build(&keys).unwrap();

        // Each *_iter is the same sequence as the Vec-returning form...
        assert_eq!(
            idx.prefix_iter("item-01").collect::<Vec<_>>(),
            idx.prefix("item-01")
        );
        assert_eq!(
            idx.range_iter("item-0010", "item-0020").collect::<Vec<_>>(),
            idx.range("item-0010", "item-0020")
        );
        assert_eq!(
            idx.subsequence_iter("i0").collect::<Vec<_>>(),
            idx.subsequence("i0")
        );
        assert_eq!(
            idx.fuzzy_iter("item-0100", 1).unwrap().collect::<Vec<_>>(),
            idx.fuzzy("item-0100", 1).unwrap()
        );

        // ...and `.take(n)` is exactly its first n, which is what a bounded autocomplete needs.
        let full = idx.prefix("item-01");
        assert_eq!(full.len(), 100); // item-0100..item-0199
        for n in [0usize, 1, 7, 100, 500] {
            assert_eq!(
                idx.prefix_iter("item-01").take(n).collect::<Vec<_>>(),
                full[..n.min(full.len())]
            );
        }
    }

    #[test]
    fn fuzzy_iter_reports_a_too_large_automaton_up_front() {
        let idx = sample();
        // The automaton is built eagerly, so an impossible distance errors before any walking.
        assert!(idx.fuzzy_iter("abcdefghijklmnopqrstuvwxyz", 50).is_err());
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
        // SAFETY: this test owns the file and does not touch it while the map is alive.
        let mapped = unsafe { StringIndex::load_mmap(&path) }.unwrap();
        assert_eq!(mapped.len(), idx.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(mapped.id(k), Some(i as u64)); // forward borrows the mapped FST
            assert_eq!(mapped.key(i as u64).as_deref(), Some(k.as_str())); // reverse decodes from the map
        }
        assert_eq!(mapped.prefix("entity-001").len(), 10); // 0010..0019
        std::fs::remove_file(&path).ok();
    }

    /// A blob whose FST is structurally valid — and CRC-correct, because it was built by `fst`
    /// itself — but whose values are not the sorted ranks must be refused by an owned load. Both
    /// cases the spot check covers: values shifted off zero, and values that never reach `n - 1`.
    #[test]
    fn owned_load_rejects_values_that_are_not_ranks() {
        fn blob_with_values(values: &[u64]) -> Vec<u8> {
            let mut b = fst::MapBuilder::memory();
            for (i, &v) in values.iter().enumerate() {
                b.insert(format!("key-{i:03}"), v).unwrap();
            }
            let mut out = b"BIX4".to_vec();
            out.extend_from_slice(&b.into_inner().unwrap());
            out
        }
        // Shifted by one: the first key's value is 1, not 0.
        let shifted = blob_with_values(&[1, 2, 3, 4]);
        assert!(StringIndex::from_bytes(&shifted).is_err());
        // All zeros: the first value is right, but no path accumulates `n - 1`.
        let flat = blob_with_values(&[0, 0, 0, 0]);
        assert!(StringIndex::from_bytes(&flat).is_err());
        // The same construction with the real ranks loads and answers correctly, so the check
        // rejects the values, not the hand-built framing.
        let good = blob_with_values(&[0, 1, 2, 3]);
        let idx = StringIndex::from_bytes(&good).unwrap();
        assert_eq!(idx.id("key-002"), Some(2));
        // `load_mmap` skips the scan by design, so the same bytes map without complaint.
        #[cfg(feature = "mmap")]
        {
            let path =
                std::env::temp_dir().join(format!("lexindex_ranks_{}.bix", std::process::id()));
            std::fs::write(&path, &shifted).unwrap();
            // SAFETY: this test owns the file and nothing writes to it while the map is alive.
            assert!(unsafe { StringIndex::load_mmap(&path) }.is_ok());
            std::fs::remove_file(&path).ok();
        }
    }

    /// The claim `build_sorted` makes is not "similar" but *byte-identical*, which is the only form
    /// of it that lets a caller switch between the two without republishing blobs: ids are ranks,
    /// so any disagreement about ordering or deduplication would renumber every key after the first
    /// difference.
    #[test]
    fn build_sorted_is_byte_identical_to_build() {
        let unsorted = [
            "banana",
            "apple",
            "apricot",
            "cherry",
            "apple",
            "",
            "é中🎉",
            "ap",
        ];
        let mut sorted: Vec<&str> = unsorted.to_vec();
        sorted.sort_unstable();
        let streamed = StringIndex::build_sorted(&sorted).unwrap();
        assert_eq!(
            streamed.to_bytes(),
            StringIndex::build(unsorted).unwrap().to_bytes()
        );
        // Duplicates were adjacent in the sorted input and had to be dropped, not inserted twice.
        assert_eq!(streamed.len(), 7);
        assert_eq!(streamed.id(""), Some(0));
        assert_eq!(streamed.key(6).as_deref(), Some("é中🎉"));
    }

    /// The precondition the caller cannot be trusted with: an unsorted stream must fail, not build
    /// an index whose ids are not ranks.
    #[test]
    fn build_sorted_refuses_input_that_is_not_ascending() {
        let Err(err) = StringIndex::build_sorted(["banana", "apple"]) else {
            panic!("a descending pair must not build");
        };
        assert!(matches!(err, IndexError::Fst(_)), "{err}");
        // A repeated key that is *not* adjacent is the same violation, not a duplicate to drop.
        assert!(StringIndex::build_sorted(["a", "b", "a"]).is_err());
    }

    #[test]
    fn build_sorted_to_file_writes_the_blob_build_would_have() {
        let keys = ["ant", "ant", "bee", "cicada"];
        let path = std::env::temp_dir().join(format!("lexindex_stream_{}.bix", std::process::id()));
        let n = StringIndex::build_sorted_to_file(keys, &path).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            StringIndex::build(keys).unwrap().to_bytes()
        );
        let loaded = StringIndex::load(&path).unwrap();
        assert_eq!(loaded.id("cicada"), Some(2));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn build_sorted_accepts_an_empty_stream() {
        let idx = StringIndex::build_sorted(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        let path = std::env::temp_dir().join(format!("lexindex_empty_{}.bix", std::process::id()));
        assert_eq!(
            StringIndex::build_sorted_to_file(Vec::<String>::new(), &path).unwrap(),
            0
        );
        assert!(StringIndex::load(&path).unwrap().is_empty());
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
