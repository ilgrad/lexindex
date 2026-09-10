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
        Self::build_sorted_to_file_checked(items, path, || Ok(()))
    }

    /// [`build_sorted_to_file`](Self::build_sorted_to_file) with a last word from the caller,
    /// asked **inside** the atomic write and before the rename that publishes the file.
    ///
    /// It exists for a source that cannot report failure through its `Iterator`: the Python binding
    /// adapts an arbitrary iterable, and an iterable that raises halfway simply stops. Without this
    /// hook the builder would see a stream that ended, finish a truncated index and rename it over
    /// whatever was at `path`, and the error would arrive after the damage. Returning `Err` here
    /// aborts the write with the temporary file removed and the target untouched.
    pub(crate) fn build_sorted_to_file_checked<I, S, C>(
        items: I,
        path: impl AsRef<std::path::Path>,
        check: C,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnOnce() -> Result<(), IndexError>,
    {
        let mut n = 0;
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            use std::io::Write;
            w.write_all(MAGIC)?;
            let mut builder = MapBuilder::new(&mut *w)?;
            n = Self::insert_sorted(&mut builder, items)?;
            check()?;
            builder.finish()?;
            Ok(())
        })?;
        Ok(n)
    }

    /// [`build`](Self::build) for a corpus that does not fit in memory, written straight to
    /// `path`. Returns the number of distinct keys written.
    ///
    /// The keys are taken in one pass, in any order, and never held together: every
    /// [`RUN_BYTES`] of them is sorted and deduplicated in memory and spilled as one run to a
    /// temporary directory beside `path`, and the runs are then merged — a `k`-way merge over one
    /// buffered reader each, a duplicate across runs dropped like one within — into
    /// [`build_sorted_to_file`](Self::build_sorted_to_file). The blob is exactly what `build`
    /// followed by [`save`](Self::save) would have written, since `build` sorts, deduplicates and
    /// inserts and this does the same in pieces; a corpus that fits in one run is sorted and fed
    /// straight in without touching the disk. Peak memory is one run — the key bytes plus eight
    /// per key, up to `RUN_BYTES` each — and a 1 MiB read buffer per run in the merge, whatever
    /// the key count. Transient disk is the distinct keys once, next to the output, removed on
    /// every exit path.
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let path = std::env::temp_dir().join("lexindex-doc-build_to_file.bix");
    /// let n = StringIndex::build_to_file(["banana", "apple", "banana", "cherry"], &path)?;
    /// let idx = StringIndex::load(&path)?;
    /// assert_eq!((n, idx.id("banana")), (3, Some(1)));
    /// # std::fs::remove_file(&path).ok();
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn build_to_file<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_checked(items, path, || Ok(()))
    }

    /// [`build_to_file`](Self::build_to_file) with a last word from the caller, asked once the
    /// input has ended — before the merge, so a source that failed does not pay for one — and
    /// again **inside** the atomic write, before the rename that publishes the file. It exists
    /// for the reason [`build_sorted_to_file_checked`](Self::build_sorted_to_file_checked) does.
    pub(crate) fn build_to_file_checked<I, S, C>(
        items: I,
        path: impl AsRef<std::path::Path>,
        check: C,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        Self::build_to_file_runs(items, path.as_ref(), check, RUN_BYTES)
    }

    /// [`build_to_file_checked`](Self::build_to_file_checked) with the run size exposed.
    ///
    /// Only the tests pass anything but [`RUN_BYTES`]: a corpus that needs more than one 256 MiB
    /// run is far too large for a unit test, and a merge bug that only appears with several runs
    /// is exactly the kind this has to catch.
    fn build_to_file_runs<I, S, C>(
        items: I,
        path: &std::path::Path,
        mut check: C,
        run_bytes: usize,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        let mut run = Run::with_budget(run_bytes);
        let mut runs = Runs::beside(path);
        for key in items {
            let key = key.as_ref();
            if !run.fits(key) {
                if run.is_empty() {
                    return Err(IndexError::Format(
                        "string-index: a key longer than the run budget",
                    ));
                }
                runs.spill(run.sorted())?;
                run.clear();
            }
            run.push(key);
        }
        check()?;
        if runs.count == 0 {
            return Self::build_sorted_to_file_checked(run.sorted(), path, check);
        }
        if !run.is_empty() {
            runs.spill(run.sorted())?;
        }
        // A read error in the merge cannot travel through `Iterator::next`; it is parked here, the
        // stream ends, and the check inside the atomic write reports it before anything is
        // published.
        let failed = std::cell::RefCell::new(None);
        let merged = runs.merge(&failed)?;
        Self::build_sorted_to_file_checked(merged, path, || {
            if let Some(e) = failed.borrow_mut().take() {
                return Err(IndexError::Io(e));
            }
            check()
        })
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

    /// [`id`](Self::id) over `n` keys given as bytes by position, for a caller whose keys are
    /// not `str`s — a lookup reading an Arrow buffer.
    #[cfg(feature = "python")]
    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        (0..n).map(|i| self.map.get(key(i))).collect()
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

    /// The id the first key **not less than** `query` has — equivalently, how many keys sort below
    /// it. `id` answers only for keys that are present; this answers for any string, which is what
    /// a caller paginating, bucketing or binary-searching an id space actually needs.
    ///
    /// Returns [`len`](Self::len) when every key sorts below `query`, so the result is always a
    /// valid insertion point in `0..=len`. Comparison is on UTF-8 bytes, which is the order the ids
    /// were assigned in.
    ///
    /// Costs 479 ns against 238 ns for [`id`](Self::id) on the same key, measured on the
    /// 479 823-word `/usr/share/dict/words`. The factor of two is a seek that has to be able to
    /// stop between keys where `id` can stop at one.
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let idx = StringIndex::build(["apple", "banana", "cherry"])?;
    /// assert_eq!(idx.lower_bound("banana"), 1); // present: its own id
    /// assert_eq!(idx.lower_bound("bb"), 2);     // absent: where it would go
    /// assert_eq!(idx.lower_bound("zzz"), 3);    // past the end: len
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn lower_bound(&self, query: &str) -> u64 {
        self.rank_of_first_ge(query.as_bytes())
    }

    /// How many keys satisfy `lo ≤ key < hi` — the count [`range`](Self::range) would return,
    /// without decoding a single key. Two order lookups rather than a walk over the matches, so it
    /// costs the same on a range of three keys as on one of three million.
    ///
    /// Zero when `hi ≤ lo`.
    pub fn range_count(&self, lo: &str, hi: &str) -> u64 {
        self.lower_bound(hi).saturating_sub(self.lower_bound(lo))
    }

    /// The **contiguous** id range of the keys starting with `prefix`.
    ///
    /// Ids are assigned in lexicographic order and keys sharing a prefix are adjacent in that
    /// order, so every match is an id in one half-open interval — which makes a prefix a slice of
    /// the id space, usable as an array range or a bitset window rather than a set of ids to test
    /// one at a time. Empty (`start == end`) when nothing matches; `0..len` for an empty prefix.
    ///
    /// Two order lookups, 420 ns on a three-byte prefix of a real word against 238 ns for
    /// [`id`](Self::id) on the whole word -- so a prefix costs less than twice a point lookup no
    /// matter how many keys carry it.
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let idx = StringIndex::build(["apple", "apricot", "banana"])?;
    /// assert_eq!(idx.prefix_id_range("ap"), 0..2);
    /// assert_eq!(idx.prefix_id_range("z"), 3..3);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn prefix_id_range(&self, prefix: &str) -> std::ops::Range<u64> {
        let start = self.rank_of_first_ge(prefix.as_bytes());
        // The exclusive end of a prefix range is the first key that does not carry the prefix:
        // the same bytes with the last one incremented. A trailing `0xff` cannot appear in UTF-8,
        // so the carry loop is unreachable for a `&str` -- it is here because the invariant that
        // makes it unreachable belongs to the caller's type, not to this function.
        let mut upper = prefix.as_bytes().to_vec();
        let end = loop {
            match upper.pop() {
                Some(0xff) => continue,
                Some(b) => {
                    upper.push(b + 1);
                    break self.rank_of_first_ge(&upper);
                }
                None => break self.len() as u64,
            }
        };
        start..end.max(start)
    }

    /// How many keys start with `prefix` — [`prefix_id_range`](Self::prefix_id_range)'s width.
    pub fn prefix_count(&self, prefix: &str) -> u64 {
        let r = self.prefix_id_range(prefix);
        r.end - r.start
    }

    /// The value stored for the first key `≥ query`, which is that key's rank, or `len` if there is
    /// none. Every order statistic above is this function.
    fn rank_of_first_ge(&self, query: &[u8]) -> u64 {
        self.map
            .range()
            .ge(query)
            .into_stream()
            .next()
            .map_or(self.len() as u64, |(_, v)| v)
    }

    /// All `(key, id)` pairs in lexicographic (= id) order, **lazily**: the transducer is streamed
    /// once, so nothing is materialised up front and no key is decoded twice. Prefer this to
    /// `prefix("")` when the index is large.
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

    /// [`iter`](Self::iter) resumed **after** `after`, which is excluded even when it is in the
    /// index — the cursor form: hand back the last key you processed and get the rest.
    /// [`range_iter`](Self::range_iter) covers a bounded range; this is the one that runs to the
    /// end. Seeking is `O(after.len())` and the stream then runs at full speed, so a scan split
    /// into chunks pays one seek per chunk instead of a walk per key, which is what the Python
    /// `__iter__` does (a `#[pyclass]` cannot hold a stream borrowing the index across `__next__`).
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let idx = StringIndex::build(["apple", "apricot", "banana"]).unwrap();
    /// let rest: Vec<_> = idx.iter_after("apricot").collect();
    /// assert_eq!(rest, [("banana".to_string(), 2)]);
    /// ```
    pub fn iter_after<'a>(&'a self, after: &'a str) -> impl Iterator<Item = (String, u64)> + 'a {
        let mut stream = self.map.range().gt(after).into_stream();
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
    /// never an out-of-bounds read — which is why this is a safe `fn`, as every loader in the crate
    /// has been since 1.0. What it is not is *total*: the perfect-hash loaders answer a crafted blob
    /// with an `Err`, and this one may panic instead. Blobs from an untrusted source go through
    /// [`from_untrusted_bytes`](Self::from_untrusted_bytes), which makes that check.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true)
    }

    /// Reconstruct from bytes **someone else wrote**, validating the transducer before any query
    /// can reach it. Slower than [`from_bytes`](Self::from_bytes) and total where that one is not.
    ///
    /// [`from_bytes`](Self::from_bytes) documents what it does not defend against: `fst`'s node
    /// decoder is safe Rust but not total, the checksum in front of it is public and recomputable,
    /// and a blob crafted to carry a matching one can panic instead of returning. This method
    /// closes that gap in the only two ways available from outside `fst`.
    ///
    /// It **checks the transducer as a graph** rather than spot-checking it, in time proportional
    /// to its nodes and transitions and never to the keys they spell — `fst` documents a billion
    /// strings in 896 bytes, and a loader that streamed them would be the denial of service it
    /// exists to prevent. Every node reachable from the root is decoded once and every transition
    /// read, so a node that would panic a query panics here instead, where it is caught. Every
    /// transition must point strictly *below* the node holding it, which is how `fst` lays nodes
    /// out and what makes the walk terminate on bytes that were not laid out that way at all.
    /// Every accepted path must spell valid UTF-8, so [`key`](Self::key) is total on what loads:
    /// `fst` stores byte strings, and a blob with a non-UTF-8 key passes every check `fst` makes.
    /// And the outputs must be ranks by construction — a final node carries none of its own and
    /// each transition carries exactly the number of keys its node spells before it, which is what
    /// the builder writes and what makes value `i` mean "the `i`-th key in byte order" — so a blob
    /// whose values are a *permutation* of the ranks, or whose footer claims a length its graph
    /// does not spell, is refused here and accepted by [`from_bytes`](Self::from_bytes), which
    /// only samples both ends.
    ///
    /// And it **catches the panic**, at the load boundary, turning it into
    /// [`IndexError::Format`]. Two consequences worth knowing before relying on it: under
    /// `panic = "abort"` there is no unwinding to catch, so a crafted blob aborts the process
    /// instead — still not undefined behaviour, but not an `Err` either; and the panic runs the
    /// process-wide hook on its way out, so the rejection normally prints a panic message to
    /// stderr. Nothing is suppressed, because the hook is global and another thread's panic is not
    /// this loader's to silence.
    ///
    /// What it costs is two decodes of every node and the checksum: 22.9 ms against
    /// 0.7 ms for the owned load, on the 479 823-word `/usr/share/dict/words`, or
    /// 48 ns per key and 32× the load it replaces. That ratio is the whole
    /// design — it is a price worth paying
    /// once for a blob from a stranger and not worth paying at all for one of your own, so use
    /// [`from_bytes`](Self::from_bytes) for the latter.
    ///
    /// ```
    /// use lexindex::StringIndex;
    /// let blob = StringIndex::build(["apple", "banana"])?.to_bytes();
    /// assert_eq!(StringIndex::from_untrusted_bytes(&blob)?.id("banana"), Some(1));
    /// assert!(StringIndex::from_untrusted_bytes(b"BIX4 and nonsense").is_err());
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn from_untrusted_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared_untrusted(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// [`from_untrusted_bytes`](Self::from_untrusted_bytes) over any byte source: the owned copy
    /// that method makes, the file [`load_untrusted`](Self::load_untrusted) reads, or the mapping
    /// [`load_mmap_untrusted`](Self::load_mmap_untrusted) borrows.
    fn from_shared_untrusted(blob: SharedBytes) -> Result<Self, IndexError> {
        // `AssertUnwindSafe` because nothing crosses the boundary on the panic path: the half-built
        // index is dropped inside, and the caller gets an `Err` that borrows nothing from it.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            // Structure before content, so a malformed transducer is *diagnosed* rather than
            // survived: `from_shared`'s own rank spot-check walks the very nodes that can panic, so
            // running it first would leave `catch_unwind` as the only thing standing between a
            // crafted blob and an abort. The checksum runs after the walk for the same reason --
            // it reads the whole blob, and is worth doing only once the layout is known good.
            let idx = Self::from_shared(blob, false)?;
            idx.validate()?;
            idx.map.as_fst().verify()?;
            Ok(idx)
        }))
        .map_err(|_| IndexError::Format("fst node decoder panicked on this blob"))?
    }

    /// Check the transducer as a graph — layout, UTF-8, ranks, length — in time proportional to
    /// its nodes and transitions, never to the keys they spell.
    ///
    /// Two sweeps over the addresses reachable from the root. `fst` writes a node after its
    /// targets, so a transition must point strictly below the node holding it: parents have the
    /// larger addresses and children the smaller, which is what lets each sweep visit every node
    /// after the ones it depends on, and what rules out a cycle.
    ///
    /// **Downwards**, parents first: reachability, the layout rule, and UTF-8. Two paths can reach
    /// one node at a character boundary and inside a character, so what is propagated is the *set*
    /// of decoder states a node is reachable in, a bit per state; every transition byte must be
    /// legal from every state in the set, and a final node — a key ends here — must be reachable
    /// at the boundary only.
    ///
    /// **Upwards**, children first: the outputs. The builder puts the whole of a key's rank on the
    /// transitions leading to it: a final node carries no output of its own, and the `i`-th
    /// transition out of a node carries exactly the number of keys the node spells before it — one
    /// if the node is final, plus the size of each earlier subtree. Sizes are summed here from the
    /// leaves up, and the root's must be the length in the footer.
    ///
    /// Together these say that streaming the transducer yields keys in byte order with values
    /// `0, 1, 2, …`, every key decodes as a `str`, and `len` is honest — without streaming it.
    fn validate(&self) -> Result<(), IndexError> {
        let fst = self.map.as_fst();
        let root = fst.root().addr();
        if root >= fst.as_bytes().len() {
            return Err(IndexError::Format("fst node address is outside the blob"));
        }
        // One byte per blob byte up to the root: the UTF-8 states a node is reachable in, zero
        // where no node is reachable. State 0 is a character boundary, so bit 0 alone is "at a
        // boundary, only".
        let mut states = vec![0u8; root + 1];
        states[root] = 1;
        for addr in (0..=root).rev() {
            let reached = states[addr];
            if reached == 0 {
                continue;
            }
            let node = fst.node(addr);
            if node.is_final() && reached != 1 {
                return Err(IndexError::Format("fst key ends inside a UTF-8 character"));
            }
            for t in node.transitions() {
                if t.addr >= addr {
                    return Err(IndexError::Format(
                        "fst transition does not point below its own node",
                    ));
                }
                let mut from = reached;
                while from != 0 {
                    let state = from.trailing_zeros() as u8;
                    from &= from - 1;
                    let next = utf8_step(state, t.inp)
                        .ok_or(IndexError::Format("fst key is not UTF-8"))?;
                    states[t.addr] |= 1 << next;
                }
            }
        }
        // A dense index over the reachable addresses — a bitset with prefix popcounts — keeps the
        // subtree sizes at one word per node rather than one per blob byte.
        let words = root / 64 + 1;
        let mut bits = vec![0u64; words];
        let mut below = vec![0u64; words + 1];
        for (w, chunk) in states.chunks(64).enumerate() {
            let mut word = 0u64;
            for (i, &s) in chunk.iter().enumerate() {
                if s != 0 {
                    word |= 1 << i;
                }
            }
            bits[w] = word;
            below[w + 1] = below[w] + u64::from(word.count_ones());
        }
        drop(states);
        let index = |addr: usize| {
            let seen_in_word = bits[addr / 64] & ((1u64 << (addr % 64)) - 1);
            (below[addr / 64] + u64::from(seen_in_word.count_ones())) as usize
        };
        let mut size = vec![0u64; below[words] as usize];
        for (w, &reachable) in bits.iter().enumerate() {
            let mut word = reachable;
            while word != 0 {
                let addr = w * 64 + word.trailing_zeros() as usize;
                word &= word - 1;
                let node = fst.node(addr);
                if node.is_final() && node.final_output().value() != 0 {
                    return Err(IndexError::Format("fst value is not the key's rank"));
                }
                let mut keys = u64::from(node.is_final());
                let mut prev: Option<u8> = None;
                for t in node.transitions() {
                    if prev.is_some_and(|p| t.inp <= p) {
                        return Err(IndexError::Format(
                            "fst transitions are not in increasing byte order",
                        ));
                    }
                    prev = Some(t.inp);
                    if t.out.value() != keys {
                        return Err(IndexError::Format("fst value is not the key's rank"));
                    }
                    keys = keys
                        .checked_add(size[index(t.addr)])
                        .ok_or(IndexError::Format("fst key count overflows"))?;
                }
                size[index(addr)] = keys;
            }
        }
        if size[index(root)] != self.map.len() as u64 {
            return Err(IndexError::Format(
                "fst holds a different number of keys than its header says",
            ));
        }
        Ok(())
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
        crate::blob::write_atomically_with(path.as_ref(), |w| Ok(self.write_to(w)?))
    }

    /// The [`to_bytes`](Self::to_bytes) blob streamed into `w`, from the FST's own bytes.
    pub(crate) fn write_to(&self, w: &mut dyn std::io::Write) -> std::io::Result<()> {
        w.write_all(MAGIC)?;
        w.write_all(self.map.as_fst().as_bytes())
    }

    /// Load an index previously written with [`StringIndex::save`] (reads the whole file into memory).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// [`load`](Self::load) for a file **someone else wrote**: the bytes go through
    /// [`from_untrusted_bytes`](Self::from_untrusted_bytes), whose validation and cost this
    /// inherits. For a file too large to read into memory there is
    /// [`load_mmap_untrusted`](Self::load_mmap_untrusted), under `load_mmap`'s obligation.
    pub fn load_untrusted(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared_untrusted(SharedBytes::from_owned(std::fs::read(path)?))
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
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded to this function's own contract — the caller guarantees the file is
        // not mutated while the mapping lives.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }

    /// [`load_mmap`](Self::load_mmap) plus the checksum [`load`](Self::load) makes: one pass over
    /// the mapping at load, so the load costs a read of the file — but the pages stay shared and
    /// nothing is copied, which is what separates it from `load` on a multi-gigabyte blob. For a
    /// file you wrote but did not carry yourself.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap): the file must not change while the
    /// index is alive. The checksum is computed once, at load, and says nothing about later.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_verified(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded to this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), true)
    }

    /// [`load_mmap`](Self::load_mmap) for a file **someone else wrote** and you cannot afford to
    /// copy: the validation of [`from_untrusted_bytes`](Self::from_untrusted_bytes) over the
    /// mapping, pages shared, nothing copied.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap), and it weighs more here: the
    /// validation reads the mapping once and trusts what it saw, so a file that changes afterwards
    /// — the stranger's, if they can still write it — is exactly what the obligation forbids. Map
    /// a copy you own.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_untrusted(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded to this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared_untrusted(SharedBytes::from_mmap(std::sync::Arc::new(mmap)))
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

/// One byte of the UTF-8 decoder, over the states `core::str::from_utf8` recognises: `0` is a
/// character boundary; `1` wants one continuation byte; `2` wants one from `A0..=BF` (after `E0`,
/// no overlongs); `3` wants two; `4` wants one from `80..=9F` (after `ED`, no surrogates); `5`
/// wants two, the first from `90..=BF` (after `F0`, no overlongs); `6` wants three; `7` wants two,
/// the first from `80..=8F` (after `F4`, nothing past U+10FFFF). `None` is a byte the state does
/// not accept.
fn utf8_step(state: u8, byte: u8) -> Option<u8> {
    Some(match (state, byte) {
        (0, 0x00..=0x7f) | (1, 0x80..=0xbf) => 0,
        (0, 0xc2..=0xdf) | (2, 0xa0..=0xbf) | (3, 0x80..=0xbf) | (4, 0x80..=0x9f) => 1,
        (0, 0xe0) => 2,
        (0, 0xe1..=0xec | 0xee..=0xef) | (5, 0x90..=0xbf) | (6, 0x80..=0xbf) | (7, 0x80..=0x8f) => {
            3
        }
        (0, 0xed) => 4,
        (0, 0xf0) => 5,
        (0, 0xf1..=0xf3) => 6,
        (0, 0xf4) => 7,
        _ => return None,
    })
}

/// Key bytes one run of [`StringIndex::build_to_file`] holds before it is sorted and spilled.
pub const RUN_BYTES: usize = 256 << 20;

/// Process-wide counter in the runs directory's name, so two builds in one process aimed at the
/// same path do not share one.
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One run of [`StringIndex::build_to_file`]: an arena of key bytes and a span per key, so a run
/// costs its bytes plus eight per key rather than a `String` and an allocation each. Both are
/// reserved once, at the first key, to the budget — the pages are only touched as they fill.
struct Run {
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
    budget: usize,
}

impl Run {
    fn with_budget(budget: usize) -> Self {
        Self {
            bytes: Vec::new(),
            spans: Vec::new(),
            budget,
        }
    }

    fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Whether `key` still fits: its bytes in the arena and its span in the table.
    fn fits(&self, key: &str) -> bool {
        self.bytes.len() + key.len() <= self.budget && self.spans.len() < self.budget / 8
    }

    fn push(&mut self, key: &str) {
        if self.bytes.capacity() == 0 {
            self.bytes.reserve_exact(self.budget);
            self.spans.reserve_exact(self.budget / 8);
        }
        // `fits` bounded the arena by `budget`, and the budget is a `usize` the caller chose;
        // `u32` spans hold a run up to 4 GiB, which `RUN_BYTES` is far below.
        self.spans.push((self.bytes.len() as u32, key.len() as u32));
        self.bytes.extend_from_slice(key.as_bytes());
    }

    /// The keys ascending and distinct, in place.
    fn sorted(&mut self) -> impl Iterator<Item = &str> {
        let bytes = &self.bytes;
        let key = |&(at, len): &(u32, u32)| &bytes[at as usize..(at + len) as usize];
        self.spans.sort_unstable_by(|a, b| key(a).cmp(key(b)));
        self.spans.dedup_by(|a, b| key(a) == key(b));
        self.spans
            .iter()
            .map(move |s| std::str::from_utf8(key(s)).expect("appended from a str"))
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.spans.clear();
    }
}

/// The spilled runs: a directory beside the output, one file per run — `[keys u64]`, then
/// `[len u32][bytes]` per key, ascending and distinct — removed when this is dropped, whichever
/// way the build ends. Nothing is created until the first spill, so a corpus that fits in one
/// run leaves no trace.
struct Runs {
    beside: std::path::PathBuf,
    dir: Option<std::path::PathBuf>,
    count: usize,
}

impl Runs {
    fn beside(target: &std::path::Path) -> Self {
        Self {
            beside: target.to_path_buf(),
            dir: None,
            count: 0,
        }
    }

    fn dir(&mut self) -> Result<&std::path::Path, IndexError> {
        if self.dir.is_none() {
            let mut dir = self.beside.clone();
            let seq = RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dir.as_mut_os_string()
                .push(format!(".{}.{seq}.runs", std::process::id()));
            // `create_dir`, not `create_dir_all`: a directory already at this pid-and-counter
            // name is someone else's, and an error rather than a place to write.
            std::fs::create_dir(&dir)?;
            self.dir = Some(dir);
        }
        Ok(self.dir.as_deref().expect("just created"))
    }

    fn spill<'a>(&mut self, keys: impl Iterator<Item = &'a str>) -> Result<(), IndexError> {
        use std::io::Write;
        let name = self.count.to_string();
        let path = self.dir()?.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, file);
        // The count goes first so the reader knows a run's end from a truncated key.
        w.write_all(&[0u8; 8])?;
        let mut n = 0u64;
        for key in keys {
            let len = u32::try_from(key.len())
                .map_err(|_| IndexError::Format("string-index: a key longer than 4 GiB"))?;
            w.write_all(&len.to_le_bytes())?;
            w.write_all(key.as_bytes())?;
            n += 1;
        }
        w.flush()?;
        let mut file = w.into_inner().map_err(|e| e.into_error())?;
        {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&n.to_le_bytes())?;
        }
        self.count += 1;
        Ok(())
    }

    /// Every run as one ascending stream; equal keys from different runs come out adjacent, for
    /// the sorted builder to drop. A read error is parked in `failed` and ends the stream.
    fn merge<'a>(
        &self,
        failed: &'a std::cell::RefCell<Option<std::io::Error>>,
    ) -> Result<impl Iterator<Item = String> + 'a, IndexError> {
        use std::cmp::Reverse;
        let dir = self.dir.as_deref().expect("a run was spilled");
        let mut readers = Vec::with_capacity(self.count);
        let mut heap = std::collections::BinaryHeap::with_capacity(self.count);
        for i in 0..self.count {
            let mut reader = RunReader::open(&dir.join(i.to_string()))?;
            if let Some(key) = reader.next()? {
                heap.push(Reverse((key, i)));
            }
            readers.push(reader);
        }
        Ok(std::iter::from_fn(move || {
            let Reverse((key, i)) = heap.pop()?;
            match readers[i].next() {
                Ok(Some(next)) => heap.push(Reverse((next, i))),
                Ok(None) => {}
                Err(e) => {
                    *failed.borrow_mut() = Some(e);
                    heap.clear();
                }
            }
            Some(key)
        }))
    }
}

impl Drop for Runs {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

struct RunReader {
    reader: std::io::BufReader<std::fs::File>,
    left: u64,
}

impl RunReader {
    fn open(path: &std::path::Path) -> Result<Self, IndexError> {
        use std::io::Read;
        let mut reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut count = [0u8; 8];
        reader.read_exact(&mut count)?;
        Ok(Self {
            reader,
            left: u64::from_le_bytes(count),
        })
    }

    fn next(&mut self) -> std::io::Result<Option<String>> {
        use std::io::Read;
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let mut len = [0u8; 4];
        self.reader.read_exact(&mut len)?;
        let mut key = vec![0u8; u32::from_le_bytes(len) as usize];
        self.reader.read_exact(&mut key)?;
        // Written from a `str` by this process moments ago: anything else is the disk lying.
        String::from_utf8(key)
            .map(Some)
            .map_err(|_| std::io::Error::other("string-index: a run file is corrupt"))
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
    fn iter_after_resumes_a_scan_without_repeating_the_cursor() {
        let keys: Vec<String> = (0..300).map(|i| format!("item-{i:04}")).collect();
        let idx = StringIndex::build(&keys).unwrap();
        let all = idx.iter().collect::<Vec<_>>();

        // Chunking a scan through the cursor reproduces the whole sequence exactly once, which is
        // the property the Python `__iter__` depends on: no key repeated at a chunk seam, none lost.
        for chunk in [1usize, 7, 299, 300, 1000] {
            let mut seen = Vec::new();
            let mut resume: Option<String> = None;
            loop {
                let batch: Vec<_> = match &resume {
                    Some(after) => idx.iter_after(after).take(chunk).collect(),
                    None => idx.iter().take(chunk).collect(),
                };
                if batch.is_empty() {
                    break;
                }
                resume = Some(batch[batch.len() - 1].0.clone());
                seen.extend(batch);
            }
            assert_eq!(seen, all, "chunk size {chunk}");
        }

        // The cursor is exclusive whether or not it is a key, and a cursor past the end is empty.
        assert_eq!(
            idx.iter_after("item-0000").next(),
            Some(("item-0001".into(), 1))
        );
        assert_eq!(
            idx.iter_after("item-0000!").next(),
            Some(("item-0001".into(), 1))
        );
        assert_eq!(idx.iter_after("").count(), 300);
        assert_eq!(idx.iter_after("zzz").count(), 0);
        assert_eq!(
            StringIndex::build([] as [&str; 0])
                .unwrap()
                .iter_after("")
                .count(),
            0
        );
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

    /// The whole point of the untrusted loader: it refuses a blob the owned one accepts.
    ///
    /// A *permutation* of the ranks passes both ends of the spot check — the first key's value is
    /// 0 and some path accumulates `n - 1` — so `from_bytes` loads it and then answers wrong ids.
    /// Only the full stream catches it, and the full stream is what this loader pays for.
    #[test]
    fn the_untrusted_loader_refuses_a_permutation_the_owned_one_takes() {
        fn blob_with_values(values: &[u64]) -> Vec<u8> {
            let mut b = fst::MapBuilder::memory();
            for (i, &v) in values.iter().enumerate() {
                b.insert(format!("key-{i:03}"), v).unwrap();
            }
            let mut out = b"BIX4".to_vec();
            out.extend_from_slice(&b.into_inner().unwrap());
            out
        }
        let permuted = blob_with_values(&[0, 2, 1, 3]);
        let loose = StringIndex::from_bytes(&permuted).expect("the spot check passes");
        assert_eq!(loose.id("key-001"), Some(2), "and it answers a wrong id");
        assert!(StringIndex::from_untrusted_bytes(&permuted).is_err());

        // The same shape with the real ranks passes both, so what is refused is the values.
        let good = blob_with_values(&[0, 1, 2, 3]);
        assert!(StringIndex::from_bytes(&good).is_ok());
        assert_eq!(
            StringIndex::from_untrusted_bytes(&good)
                .unwrap()
                .id("key-001"),
            Some(1)
        );
    }

    /// No false rejections: whatever `to_bytes` writes, the strict loader takes — including the
    /// empty index and a single key, which are the two shapes with special node addresses.
    #[test]
    fn the_untrusted_loader_accepts_every_blob_this_crate_writes() {
        for keys in [
            vec![],
            vec!["only"],
            vec!["a", "b"],
            vec!["", "a", "ab", "abc"],
            vec!["é中🎉", "über", "zebra"],
        ] {
            let idx = StringIndex::build(keys.iter()).unwrap();
            let blob = idx.to_bytes();
            let back = StringIndex::from_untrusted_bytes(&blob)
                .unwrap_or_else(|e| panic!("refused its own blob for {keys:?}: {e}"));
            assert_eq!(back.len(), keys.len());
            // By byte order, which is not the order they are written above: `zebra` starts 0x7a and
            // every non-ASCII key starts 0xc3.
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            for (i, k) in sorted.iter().enumerate() {
                assert_eq!(back.id(k), Some(i as u64), "{k:?} in {sorted:?}");
            }
        }
    }

    /// Bytes that are not an FST at all stop at the magic or the checksum, before the walk.
    #[test]
    fn the_untrusted_loader_refuses_bytes_that_are_not_an_fst() {
        for bytes in [
            &b""[..],
            &b"BIX"[..],
            &b"NOPE0123456789"[..],
            &b"BIX4 and nonsense past the magic"[..],
            &[0xffu8; 64][..],
        ] {
            assert!(
                StringIndex::from_untrusted_bytes(bytes).is_err(),
                "accepted {bytes:?}"
            );
        }
    }

    /// The decoder table against the standard library's, over every one- and two-byte string,
    /// every three-byte string that starts with a lead byte, and the four-byte strings whose
    /// continuation bytes sit on the range edges.
    #[test]
    fn utf8_steps_agree_with_from_utf8() {
        fn accepts(bytes: &[u8]) -> bool {
            let mut state = 0u8;
            for &b in bytes {
                match utf8_step(state, b) {
                    Some(next) => state = next,
                    None => return false,
                }
            }
            state == 0
        }
        let check = |bytes: &[u8]| {
            assert_eq!(
                accepts(bytes),
                std::str::from_utf8(bytes).is_ok(),
                "{bytes:02x?}"
            );
        };
        check(b"");
        for a in 0..=255u8 {
            check(&[a]);
            for b in 0..=255u8 {
                check(&[a, b]);
                if a >= 0xe0 {
                    for c in 0..=255u8 {
                        check(&[a, b, c]);
                    }
                }
                if a >= 0xf0 {
                    let edges = [0x00, 0x7f, 0x80, 0x8f, 0x90, 0xbf, 0xc0, 0xff];
                    for c in edges {
                        for d in edges {
                            check(&[a, b, c, d]);
                        }
                    }
                }
            }
        }
    }

    /// A blob over byte strings that are not UTF-8 is a valid `fst` — its builder takes any bytes
    /// — and the owned loader takes it, then answers `None` for a live id. (Its spot check
    /// decodes the *last* key, so that one has to be UTF-8 for the blob to get in.) The strict
    /// loader refuses it, including the case a minimised transducer makes subtle: a node shared
    /// between a path at a character boundary and a path inside a character.
    #[test]
    fn the_untrusted_loader_refuses_a_key_that_is_not_utf8() {
        fn raw_blob(keys: &[&[u8]]) -> Vec<u8> {
            let mut b = fst::MapBuilder::memory();
            for (i, k) in keys.iter().enumerate() {
                b.insert(k, i as u64).unwrap();
            }
            let mut out = b"BIX4".to_vec();
            out.extend_from_slice(&b.into_inner().unwrap());
            out
        }
        let loose = StringIndex::from_bytes(&raw_blob(&[b"a\xff", b"b"])).unwrap();
        assert_eq!(loose.len(), 2);
        assert_eq!(loose.key(0), None, "a live id with no key is the bug");
        for keys in [
            &[&b"a\xff"[..], &b"b"[..]][..],
            &[&b"ab"[..], &b"a\xff"[..]][..],
            &[&b"\xc3"[..]][..], // ends inside a character
            // One shared `\xa9` node, legal after `\xc3` and not after `a`.
            &[&b"a\xa9"[..], &b"\xc3\xa9"[..]][..],
            &[&b"\xed\xa0\x80"[..]][..],     // a surrogate
            &[&b"\xf4\x90\x80\x80"[..]][..], // past U+10FFFF
            &[&b"\xc0\xaf"[..]][..],         // an overlong slash
        ] {
            assert!(
                StringIndex::from_untrusted_bytes(&raw_blob(keys)).is_err(),
                "accepted {keys:02x?}"
            );
        }
        let ok =
            StringIndex::from_untrusted_bytes(&raw_blob(&[b"a", b"\xc3\xa9", "中".as_bytes()]))
                .unwrap();
        assert_eq!(ok.key(1).as_deref(), Some("é"));
    }

    /// The footer's length is what `len` answers, and it is not derived from the graph. A blob
    /// whose graph spells fewer keys than the footer claims — re-sealed, so the checksum passes —
    /// is refused, however large the claim: the check counts nodes, not keys.
    #[test]
    fn the_untrusted_loader_refuses_a_length_the_graph_does_not_spell() {
        fn crc32c(data: &[u8]) -> u32 {
            let mut crc = !0u32;
            for &b in data {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 == 1 {
                        (crc >> 1) ^ 0x82f6_3b78
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        // `fst` v3: `[..nodes][len u64][root u64][masked crc32c u32]`, the mask being Snappy's.
        fn with_len(blob: &[u8], len: u64) -> Vec<u8> {
            let mut out = blob.to_vec();
            let end = out.len();
            out[end - 20..end - 12].copy_from_slice(&len.to_le_bytes());
            let sum = crc32c(&out[4..end - 4]);
            let masked = sum.rotate_right(15).wrapping_add(0xa282_ead8);
            out[end - 4..].copy_from_slice(&masked.to_le_bytes());
            out
        }
        let blob = StringIndex::build(["a", "b", "c"]).unwrap().to_bytes();
        assert_eq!(
            StringIndex::from_untrusted_bytes(&with_len(&blob, 3))
                .unwrap()
                .len(),
            3,
            "re-sealing alone changes nothing"
        );
        for claimed in [0, 2, 4, 1 << 40, u64::MAX] {
            assert!(
                StringIndex::from_untrusted_bytes(&with_len(&blob, claimed)).is_err(),
                "accepted a footer claiming {claimed} keys over three"
            );
        }
    }

    /// The cost is the graph's, not the language's: every 16-letter word over `{a, b}` is 65 536
    /// keys in a transducer of a few dozen nodes, and the check is exact on it.
    #[test]
    fn the_untrusted_loader_counts_nodes_not_keys() {
        let keys: Vec<String> = (0..1u32 << 16)
            .map(|i| {
                (0..16)
                    .rev()
                    .map(|b| if i >> b & 1 == 1 { 'b' } else { 'a' })
                    .collect()
            })
            .collect();
        let blob = StringIndex::build(keys.iter()).unwrap().to_bytes();
        assert!(blob.len() < 1024, "{} bytes for 65 536 keys", blob.len());
        let idx = StringIndex::from_untrusted_bytes(&blob).unwrap();
        assert_eq!(idx.len(), 1 << 16);
        assert_eq!(idx.id(&keys[0]), Some(0));
        assert_eq!(idx.id(&keys[65_535]), Some(65_535));
        assert_eq!(idx.key(40_000).as_deref(), Some(keys[40_000].as_str()));
    }

    /// The path loaders agree with the byte loaders and refuse in the same places:
    /// `load_untrusted` is `from_untrusted_bytes` over the file, `load_mmap_verified` is
    /// `load_mmap` plus the checksum `load` makes, `load_mmap_untrusted` is the full validation
    /// over the mapping.
    #[cfg(feature = "mmap")]
    #[test]
    fn the_path_loaders_agree_with_the_byte_loaders_and_refuse_alike() {
        let idx = StringIndex::build(["apple", "banana", "cherry"]).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_loaders_{}.bix", std::process::id()));
        idx.save(&path).unwrap();
        // SAFETY: this test owns the file and nothing writes to it while a map is alive.
        for back in [
            StringIndex::load_untrusted(&path).unwrap(),
            unsafe { StringIndex::load_mmap_verified(&path) }.unwrap(),
            unsafe { StringIndex::load_mmap_untrusted(&path) }.unwrap(),
        ] {
            assert_eq!(back.id("banana"), Some(1));
            assert_eq!(back.key(2).as_deref(), Some("cherry"));
        }
        // A flipped checksum byte: the plain mapping never looks at it, every other loader does.
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        std::fs::write(&path, &bytes).unwrap();
        assert!(StringIndex::load(&path).is_err());
        assert!(unsafe { StringIndex::load_mmap(&path) }.is_ok());
        assert!(unsafe { StringIndex::load_mmap_verified(&path) }.is_err());
        assert!(StringIndex::load_untrusted(&path).is_err());
        assert!(unsafe { StringIndex::load_mmap_untrusted(&path) }.is_err());
        // A permutation of the ranks, CRC-correct: the spot check passes, the full one does not.
        let mut b = fst::MapBuilder::memory();
        for (k, v) in [("a", 0u64), ("b", 2), ("c", 1), ("d", 3)] {
            b.insert(k, v).unwrap();
        }
        let mut permuted = b"BIX4".to_vec();
        permuted.extend_from_slice(&b.into_inner().unwrap());
        std::fs::write(&path, &permuted).unwrap();
        assert!(unsafe { StringIndex::load_mmap_verified(&path) }.is_ok());
        assert!(unsafe { StringIndex::load_mmap_untrusted(&path) }.is_err());
        assert!(StringIndex::load_untrusted(&path).is_err());
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

    /// A directory of its own per test, so a leftover runs directory has nowhere to hide.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lexindex_{name}_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn build_to_file_writes_the_blob_build_would_have_run_by_run() {
        // 200 keys in a scrambled order, every one twice, the second copies after the first: a
        // 64-byte run holds a handful, so the duplicates land in different runs.
        let keys: Vec<String> = (0..200)
            .map(|i| format!("k{:03}", (i * 7919) % 200))
            .collect();
        let twice: Vec<&str> = keys.iter().chain(keys.iter()).map(String::as_str).collect();
        let want = StringIndex::build(&keys).unwrap().to_bytes();
        let dir = scratch("runs");
        let path = dir.join("idx.bix");
        let n = StringIndex::build_to_file_runs(&twice, &path, || Ok(()), 64).unwrap();
        assert_eq!(n, 200);
        assert_eq!(std::fs::read(&path).unwrap(), want);
        assert_eq!(entries(&dir), ["idx.bix"], "the runs directory is gone");
        let loaded = StringIndex::load(&path).unwrap();
        assert_eq!(loaded.id("k199"), Some(199));

        // One run: sorted in memory, no directory at all.
        assert_eq!(StringIndex::build_to_file(&twice, &path).unwrap(), 200);
        assert_eq!(std::fs::read(&path).unwrap(), want);
        assert_eq!(entries(&dir), ["idx.bix"]);

        // Runs bounded by the span table rather than the arena: empty keys cost no bytes.
        let empties = ["", "a", "", "b", "", "", "c"];
        assert_eq!(
            StringIndex::build_to_file_runs(empties, &path, || Ok(()), 16).unwrap(),
            4
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            StringIndex::build(empties).unwrap().to_bytes()
        );

        assert_eq!(
            StringIndex::build_to_file(Vec::<String>::new(), &path).unwrap(),
            0
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            StringIndex::build(Vec::<String>::new()).unwrap().to_bytes()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_to_file_check_aborts_before_the_file_is_published() {
        let dir = scratch("runs_check");
        let path = dir.join("idx.bix");
        std::fs::write(&path, b"previous").unwrap();
        let keys: Vec<String> = (0..100)
            .map(|i| format!("k{:03}", (i * 37) % 100))
            .collect();

        // Refused once the input has ended: no merge, no file.
        let err = StringIndex::build_to_file_runs(
            &keys,
            &path,
            || Err(IndexError::Format("stopped")),
            64,
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::Format("stopped")), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");

        // Refused inside the atomic write, after the merge: the file is still the previous one.
        let calls = std::cell::Cell::new(0);
        let late = || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(IndexError::Format("late"))
            } else {
                Ok(())
            }
        };
        let err = StringIndex::build_to_file_runs(&keys, &path, late, 64).unwrap_err();
        assert!(matches!(err, IndexError::Format("late")), "{err}");
        assert_eq!(
            calls.get(),
            2,
            "once after the input, once inside the write"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        assert_eq!(entries(&dir), ["idx.bix"], "the runs directory is gone");

        // A key no run can hold is an error, not an unbounded allocation.
        let long = "x".repeat(65);
        let err =
            StringIndex::build_to_file_runs([long.as_str()], &path, || Ok(()), 64).unwrap_err();
        assert!(err.to_string().contains("run budget"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        std::fs::remove_dir_all(&dir).ok();
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
