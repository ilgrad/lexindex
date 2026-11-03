//! From keys to an index: views, sorting, the character code, and the builders.

use super::*;

/// `f` over `keys` cut into `threads` contiguous ranges, each on a thread of its own where there is
/// more than one; the results in key order.
pub(super) fn in_ranges<'k, T: Send>(
    keys: &[&'k str],
    threads: usize,
    f: impl Fn(&[&'k str]) -> T + Sync,
) -> Vec<T> {
    if threads <= 1 || keys.len() <= 1 {
        return vec![f(keys)];
    }
    let f = &f;
    std::thread::scope(|scope| {
        let running: Vec<_> = keys
            .chunks(keys.len().div_ceil(threads))
            .map(|range| scope.spawn(move || f(range)))
            .collect();
        running
            .into_iter()
            .map(|h| {
                h.join()
                    .expect("counting or spelling a range of keys cannot panic")
            })
            .collect()
    })
}

/// Keys a pass over them prefetches ahead of the one it is on: far enough for a load from memory to
/// land in time.
pub(super) const READ_AHEAD: usize = 32;

/// `keys` in order, the key [`READ_AHEAD`] places on prefetched as each is handed out. A caller's
/// keys lie wherever it allocated them, so a pass in key order otherwise waits on memory at every
/// key: counting the bytes outside ASCII over a million DNA reads took 24 ns a key rather than 7.
fn read_ahead<'a, 'k>(keys: &'a [&'k str]) -> impl Iterator<Item = &'k str> + 'a {
    keys.iter().enumerate().map(move |(i, &key)| {
        if let Some(next) = keys.get(i + READ_AHEAD) {
            crate::blob::prefetch_key(next.as_bytes());
        }
        key
    })
}

/// The keys as `str`s every thread of the build can share, which the caller's `S` need not be:
/// sixteen bytes a key, taken once for the whole build. Taken before the keys are sorted, so that
/// the order is checked on the view, a range of keys a thread, where on `S` it could only be one.
///
/// Its pages are touched a range a thread before the one pass that fills it, which cannot be split,
/// since `S` need not be `Sync`: the first touch of a page is a fault the kernel serves on the
/// thread that takes it, and the pass took all of them in turn — 9 to 10 ms of a million keys,
/// against 5.4 with the faults taken first.
pub(super) fn view_of<S: AsRef<str>>(keys: &[S], threads: usize) -> Vec<&str> {
    let mut view = Vec::with_capacity(keys.len());
    if threads > 1 {
        let spare = view.spare_capacity_mut();
        let size = spare.len().div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            for range in spare.chunks_mut(size) {
                // One write a page, to a slot the vector still counts as uninitialised.
                scope.spawn(move || {
                    for slot in range.iter_mut().step_by(4096 / size_of::<&str>()) {
                        *slot = std::mem::MaybeUninit::zeroed();
                    }
                });
            }
        });
    }
    view.extend(keys.iter().map(AsRef::as_ref));
    view
}

/// Whether every key is above the one before it — what keys that come sorted and distinct are —
/// a range of keys a thread, and each range's last key against the next one's first. Over keys
/// that came in order the one pass of [`dedup_ascending`] was a step of the build no thread shared:
/// 13.6 ms of a million article titles and 9.8 of the 68.7 a word list built in, against 4.9 and
/// 2.4 a range a thread.
pub(super) fn strictly_ascending(keys: &[&str], threads: usize) -> bool {
    let size = keys.len().div_ceil(threads.max(1)).max(1);
    in_ranges(keys, threads, |range| {
        let mut prev: Option<&str> = None;
        read_ahead(range).all(|key| {
            let above = prev.is_none_or(|p| p < key);
            prev = Some(key);
            above
        })
    })
    .into_iter()
    .all(|above| above)
        && (size..keys.len())
            .step_by(size)
            .all(|i| keys[i - 1] < keys[i])
}

/// Drop adjacent duplicates from `keys` in place, in the one pass that checks they ascend: `false`
/// at the first key below its predecessor, with `keys` then a permutation of what it was. It reads
/// ahead, and stands for the two passes a sort's check for a run and `dedup_by` made over keys
/// scattered through the heap: 34 ns a key over a million DNA reads against 7 for this one, and 41
/// against 27 over a million paths.
fn dedup_ascending(keys: &mut Vec<&str>) -> bool {
    let mut kept = 0;
    for i in 0..keys.len() {
        if let Some(next) = keys.get(i + READ_AHEAD) {
            crate::blob::prefetch_key(next.as_bytes());
        }
        if kept > 0 {
            match keys[i].cmp(keys[kept - 1]) {
                Ordering::Less => return false,
                Ordering::Equal => continue,
                Ordering::Greater => {}
            }
        }
        if kept != i {
            keys.swap(kept, i);
        }
        kept += 1;
    }
    keys.truncate(kept);
    true
}

/// Whether `keys` ascend, with their duplicates dropped if they do: [`strictly_ascending`] first,
/// which answers keys that are distinct already on every thread, and the serial pass only for keys
/// that are not.
pub(super) fn distinct_ascending(keys: &mut Vec<&str>, threads: usize) -> bool {
    strictly_ascending(keys, threads) || dedup_ascending(keys)
}

/// `keys` sorted, unless they ascend already, and their duplicates dropped.
pub(super) fn sort_distinct(keys: &mut Vec<&str>, threads: usize) {
    if !distinct_ascending(keys, threads) {
        sort_on(keys, threads);
        assert!(dedup_ascending(keys), "sorted keys ascend");
    }
}

/// `keys` sorted on `threads` threads: a range a thread sorted in place, then cut at splitters drawn
/// from every range, so that each thread merges the keys between two splitters from every range
/// into its own part of one buffer. Keys of equal bytes are interchangeable, so the order is the
/// one-thread sort's, whatever the thread count. The sort was the one serial phase of a build from
/// unsorted keys: 2.5 % of its instructions on a million domains, at one thread's pace.
pub(super) fn sort_on(keys: &mut Vec<&str>, threads: usize) {
    let n = keys.len();
    // Every range must give `threads` keys to the sample.
    let threads = threads.min(n.isqrt()).max(1);
    if threads == 1 {
        keys.sort_unstable();
        return;
    }
    let size = n.div_ceil(threads);
    std::thread::scope(|scope| {
        for range in keys.chunks_mut(size) {
            scope.spawn(move || range.sort_unstable());
        }
    });
    let mut out: Vec<&str> = vec![""; n];
    {
        let runs: Vec<&[&str]> = keys.chunks(size).collect();
        let mut sample: Vec<&str> = runs
            .iter()
            .flat_map(|r| (0..threads).map(move |i| r[i * r.len() / threads]))
            .collect();
        sample.sort_unstable();
        let splitters: Vec<&str> = (1..threads)
            .map(|j| sample[j * sample.len() / threads])
            .collect();
        // Where each splitter falls in each run: part `j` of a run is its keys from splitter
        // `j - 1` up to, not including, splitter `j`.
        let cuts: Vec<Vec<usize>> = runs
            .iter()
            .map(|r| {
                let mut c = Vec::with_capacity(threads + 1);
                c.push(0);
                c.extend(splitters.iter().map(|s| r.partition_point(|k| k < s)));
                c.push(r.len());
                c
            })
            .collect();
        std::thread::scope(|scope| {
            let mut rest = out.as_mut_slice();
            for j in 0..threads {
                let parts: Vec<&[&str]> = runs
                    .iter()
                    .zip(&cuts)
                    .map(|(r, c)| &r[c[j]..c[j + 1]])
                    .collect();
                let len = parts.iter().map(|p| p.len()).sum();
                let (part, tail) = std::mem::take(&mut rest).split_at_mut(len);
                rest = tail;
                scope.spawn(move || merge_into(&parts, part));
            }
        });
    }
    *keys = out;
}

/// The sorted `parts` merged into `out`, which has room for exactly all of them.
fn merge_into<'k>(parts: &[&[&'k str]], out: &mut [&'k str]) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    use std::collections::binary_heap::PeekMut;
    let mut next = vec![1usize; parts.len()];
    let mut heads: BinaryHeap<Reverse<(&str, usize)>> = parts
        .iter()
        .enumerate()
        .filter_map(|(r, p)| p.first().map(|&k| Reverse((k, r))))
        .collect();
    for slot in out {
        let mut top = heads.peek_mut().expect("as many keys as slots");
        let Reverse((key, r)) = *top;
        *slot = key;
        match parts[r].get(next[r]) {
            Some(&k) => {
                next[r] += 1;
                *top = Reverse((k, r));
            }
            None => {
                PeekMut::pop(top);
            }
        }
    }
}

/// The [character code](charcode) a set of keys is smallest under, or `None` where none pays. A
/// scan for bytes outside ASCII settles every Latin corpus without decoding a character. Both
/// passes run a range of keys a thread: a caller's keys lie wherever it allocated them, so a pass
/// in key order waits on memory a key, and one thread keeps only so many of those loads in flight.
pub(crate) fn choose_code(keys: &[&str], threads: usize) -> Option<CharCode> {
    let (bytes, high) = in_ranges(keys, threads, |range| {
        read_ahead(range).fold((0u64, 0u64), |(b, h), k| {
            (b + k.len() as u64, h + charcode::high_bytes(k.as_bytes()))
        })
    })
    .into_iter()
    .fold((0, 0), |(b, h), (rb, rh)| (b + rb, h + rh));
    if !charcode::worth_counting(bytes, high) {
        return None;
    }
    let mut tally = Tally::new();
    for part in in_ranges(keys, threads, |range| {
        let mut part = Tally::new();
        read_ahead(range).for_each(|k| part.add(k));
        part
    }) {
        tally.merge(part);
    }
    tally.choose()
}

/// `key` as a blob under `code` spells it: its own bytes without one, and otherwise its spelling in
/// `buf`. A key the code cannot spell was not in the stream the code was chosen over.
fn spell<'a>(
    code: Option<&CharCode>,
    key: &'a str,
    buf: &'a mut Vec<u8>,
) -> Result<&'a [u8], IndexError> {
    let Some(code) = code else {
        return Ok(key.as_bytes());
    };
    buf.clear();
    if !code.encode_key(key, buf) {
        return Err(IndexError::Format(
            "dict: the key stream changed between passes",
        ));
    }
    Ok(buf)
}

/// Every key spelled under `code`, end to end, with where each ends; `None` if a key holds a
/// character the code does not spell.
pub(crate) fn encode_all(code: &CharCode, keys: &[&str]) -> Option<(Vec<u8>, Vec<usize>)> {
    let mut arena = Vec::new();
    let mut ends = Vec::new();
    for key in read_ahead(keys) {
        if !code.encode_key(key, &mut arena) {
            return None;
        }
        ends.push(arena.len());
    }
    Some((arena, ends))
}

impl DictIndex {
    /// Build from a collection of strings, in any order; duplicates are removed and the ids are
    /// the ranks of the distinct keys in byte order. Blocks of 256 keys.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_with_block(items, DEFAULT_BLOCK)
    }

    /// [`build`](Self::build) with `block` keys per block, `1..=1024`. The block is what a stored
    /// head and its arrays are shared over, and it is split into microblocks of 16 to 32 — the
    /// smallest divisor of the block at or above its square root — of which a lookup scans one,
    /// after one restart a microblock: `block / micro + micro − 2` entries, not `block − 1`. On
    /// real words 32 / 64 / 128 / 256 / 512 / 1024 give 2.85 / 2.72 / 2.66 / 2.64 / 2.52 / 2.51
    /// bytes per key, `id` at 253–254 / 250–254 / 263–265 / 281–283 / 312–322 / 341–342 ns and
    /// `key_into` at 141–142 / 151–152 / 169–171 / 198–199 / 259–261 / 307. Every block is
    /// under `marisa-trie`'s 2.955 floor on that corpus. [`DictProfile`] names three points of
    /// that curve for a caller who does not want to pick one.
    pub fn build_with_block<I, S>(items: I, block: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        // Sorted and deduplicated as a view of the keys rather than as owned `String`s: every key
        // is copied into a block below in either case, so the intermediate copy only doubled the
        // peak for a caller that already owned the corpus.
        let keys: Vec<S> = items.into_iter().collect();
        let threads = build_threads(keys.len(), block);
        let mut view = view_of(&keys, threads);
        sort_distinct(&mut view, threads);
        Self::from_view(view, block, micro_for(block))
    }

    /// Build from keys that are **already in ascending byte order** — a sorted file, a database
    /// cursor, the output of an external sort. Adjacent duplicates are dropped exactly as
    /// [`build`](Self::build) drops them after sorting, so for the same key set the two produce
    /// **byte-identical** blobs. Blocks of 256 keys.
    ///
    /// It is not the faster of the two and does not claim to be: `build` looks for keys already in
    /// order before it sorts, in the pass that drops duplicates, so over sorted keys the two run the
    /// same code. Nor does it hold less, since the symbol table is trained on a sample of the blocks
    /// before any block is encoded, so the keys are read twice either way. What it buys is the
    /// **check**.
    ///
    /// The order is the caller's precondition and is checked anyway: a key below its predecessor
    /// returns an error rather than an index that answers wrongly, which is what an unsorted input
    /// would produce here, since every lookup is a binary search over the block heads. Ordering is
    /// by *bytes*, which for UTF-8 is the same as `str`'s `Ord` — a list sorted by a locale
    /// collation is not sorted for this purpose, and neither is one whose keys were composed from
    /// sorted parts (see [`StringIndex::build_sorted`](crate::StringIndex::build_sorted) for why
    /// the separator decides that).
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build_sorted(["apple", "apricot", "apricot", "banana"]).unwrap();
    /// assert_eq!(idx.len(), 3);
    /// assert_eq!(idx.id("banana"), Some(2));
    /// assert!(DictIndex::build_sorted(["banana", "apple"]).is_err());
    /// ```
    pub fn build_sorted<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_sorted_with_block(items, DEFAULT_BLOCK)
    }

    /// [`build_sorted`](Self::build_sorted) with `block` keys per block, `1..=1024`; the block is
    /// the same trade-off [`build_with_block`](Self::build_with_block) documents.
    pub fn build_sorted_with_block<I, S>(items: I, block: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        let keys: Vec<S> = items.into_iter().collect();
        let threads = build_threads(keys.len(), block);
        let mut view = view_of(&keys, threads);
        if !distinct_ascending(&mut view, threads) {
            return Err(IndexError::Format(
                "dict: build_sorted got a key below its predecessor",
            ));
        }
        Self::from_view(view, block, micro_for(block))
    }

    /// [`build`](Self::build) with the block, the microblock and the labels of `layout`. With
    /// `labels`, a dictionary of the corpus's trie labels is tried as well and kept only where the
    /// blob comes out smaller (`BDX5`, or `BDX3` with more phrases where a lookup reads every
    /// label often); otherwise the blob is the plain layout's, byte for byte, and a corpus spelled
    /// in a character code takes no labels. Path lists repeat whole directory and file names under
    /// different parents, which is what a label names once: at block 1024 a million paths go
    /// 9.323 → 8.530 bytes a key, under `marisa-trie`'s 8.81 at its smallest, and URLs, English
    /// titles, identifiers, domains and PyPI names come out 2.7–3.8 % smaller. A member `id` reads
    /// 1.5 labels on average through the nested dictionary: +24 % instructions on those paths and
    /// +6–10 % elsewhere, counted and not yet timed — which is why no default builds labels.
    ///
    /// ```
    /// use lexindex::{DictIndex, DictLayout};
    /// let keys = ["/usr/share/doc", "/usr/share/man", "/usr/lib", "/home/me/doc"];
    /// let index = DictIndex::build_with(keys, DictLayout::new(256).with_labels(true))?;
    /// assert_eq!(index.id("/usr/lib"), Some(1));
    /// assert_eq!(
    ///     DictIndex::build_with(keys, DictLayout::new(256))?.to_bytes(),
    ///     DictIndex::build_with_block(keys, 256)?.to_bytes()
    /// );
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn build_with<I, S>(items: I, layout: DictLayout) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let (block, micro) = layout.sizes()?;
        let labels = layout.labels;
        let keys: Vec<S> = items.into_iter().collect();
        let threads = build_threads(keys.len(), block);
        let mut view = view_of(&keys, threads);
        sort_distinct(&mut view, threads);
        if !labels || choose_code(&view, threads).is_some() {
            return Self::from_view(view, block, micro);
        }
        let plain = Self::from_sorted(&view, block, micro)?;
        Ok(match Self::from_sorted_labelled(&view, block, micro)? {
            Some(labelled) if labelled.serialized_len() < plain.serialized_len() => labelled,
            _ => plain,
        })
    }

    /// The labelled half of [`build_with`](Self::build_with) alone, whether or not it wins:
    /// `None` where no label pays. For the harnesses that size both halves.
    #[cfg(feature = "fuzzing")]
    #[doc(hidden)]
    pub fn build_labelled<I, S>(items: I, block: usize) -> Result<Option<Self>, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        let keys: Vec<S> = items.into_iter().collect();
        let threads = build_threads(keys.len(), block);
        let mut view = view_of(&keys, threads);
        sort_distinct(&mut view, threads);
        Self::from_sorted_labelled(&view, block, micro_for(block))
    }

    /// [`build_with_block`](Self::build_with_block) with `micro` keys a microblock rather than the
    /// 16 to 32 it chooses: `1..=block`, dividing `block`. A lookup scans one restart a microblock
    /// and then one microblock, `block / micro + micro - 2` entries, so a larger `micro` stores
    /// fewer restarts for a longer scan. At the default block of 256, microblocks of 128 rather
    /// than 16 made fourteen corpora 0.12–0.34 bytes a key smaller — paths 6.85 → 6.51, the
    /// 479 823-word dictionary 2.64 → 2.45, a million URLs 7.51 → 7.35, a million UUIDs
    /// 18.04 → 17.89 — and 64 took 80–90 % of that. What the longer scan costs an `id` is not
    /// measured yet, which is why no builder chooses it by itself. The blob is the same format:
    /// lexindex 4.5.3 loads one built here and answers every key of those fourteen.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let keys = ["apple", "apricot", "banana", "cherry"];
    /// let index = DictIndex::build_with_layout(keys, 4, 2)?;
    /// assert_eq!(index.id("banana"), Some(2));
    /// assert!(DictIndex::build_with_layout(keys, 4, 3).is_err()); // 3 does not divide 4
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn build_with_layout<I, S>(items: I, block: usize, micro: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        if micro == 0 || block % micro != 0 {
            return Err(IndexError::Format("dict: micro must divide block"));
        }
        let keys: Vec<S> = items.into_iter().collect();
        let threads = build_threads(keys.len(), block);
        let mut view = view_of(&keys, threads);
        sort_distinct(&mut view, threads);
        Self::from_view(view, block, micro)
    }

    /// The constructor for a corpus that does not fit in memory, written straight to `path`: the
    /// keys, in any order, are sorted in runs that spill beside the output and merged back, and the
    /// block data is encoded into the file as it is produced. Returns the number of distinct keys
    /// written, since a caller streaming keys it does not retain has no other way to learn how many
    /// were distinct. Blocks of 256 keys.
    ///
    /// The bytes are exactly what [`build`](Self::build) would produce for the same key set, through
    /// the same atomic replace [`save`](Self::save) uses — a crash leaves either the previous file
    /// or nothing, never a half-written index.
    ///
    /// **What is still held.** Not the corpus, and not the block data, which is the bulk of the
    /// index. The block heads, the per-block arrays and one start a microblock are, at roughly
    /// `(mean head length + 20) / block + 8 / micro` bytes per key — 0.36 at the default on English
    /// words, 0.31 at 512 keys a block.
    ///
    /// **Transient disk**: one run file per `RUN_BYTES` of keys, in a directory beside the output,
    /// removed however the build ends. **Three passes** over the sorted keys: the symbol table is
    /// trained on suffixes from blocks spread over the index, that spread is a function of the key
    /// count, and the key count is what the first pass establishes. A corpus with two bytes in
    /// fifteen or more outside ASCII takes a fourth after the first, which counts its characters
    /// for the code they may be spelled in. Where the corpus needed more than one run, each pass is
    /// a fresh merge of them.
    pub fn build_to_file<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_with_block(items, path, DEFAULT_BLOCK)
    }

    /// [`build_to_file`](Self::build_to_file) with `block` keys per block, `1..=1024`; the block is
    /// the same trade-off [`build_with_block`](Self::build_with_block) documents, and it decides
    /// what this holds as well as what it stores.
    pub fn build_to_file_with_block<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
        block: usize,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_with(items, path, DictLayout::new(block))
    }

    /// [`build_to_file`](Self::build_to_file) with the block, the microblock and the labels of
    /// `layout`, writing the bytes [`build_with`](Self::build_with) makes.
    ///
    /// **`labels` costs what this constructor exists to avoid, in part.** The labels are chosen over
    /// the whole corpus, so it holds two more things while it chooses: 2 to 4 bytes a key for a
    /// filter of the labels seen twice, and each of those labels until it knows which pay. The peak
    /// went 290 → 582 MB on 7.3 million paths and 738 → 1 086 MB on 100 million bigrams, against
    /// 2 105 MB for the sorting build of those paths. It walks the keys twice more, for the parse
    /// that decides them and for the second encoding, and for that encoding holds a second blob on
    /// disk beside the output.
    ///
    /// ```
    /// use lexindex::{DictIndex, DictLayout};
    /// let keys = ["usr/lib/a", "usr/lib/b", "var/lib/a", "var/lib/b"];
    /// let layout = DictLayout::new(32).with_labels(true);
    /// let path = std::env::temp_dir().join(format!("lexindex-doc-{}.bdx", std::process::id()));
    /// assert_eq!(DictIndex::build_to_file_with(keys, &path, layout)?, 4);
    /// assert_eq!(
    ///     std::fs::read(&path)?,
    ///     DictIndex::build_with(keys, layout)?.to_bytes()
    /// );
    /// # std::fs::remove_file(&path)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn build_to_file_with<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
        layout: DictLayout,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_checked(items, path, layout, || Ok(()))
    }

    /// [`build_to_file`](Self::build_to_file) with a last word from the caller, asked once the
    /// input has ended — before the merge, so a source that failed does not pay for one — and again
    /// **inside** the atomic write, before the rename that publishes the file.
    ///
    /// It exists for a source that cannot report failure through its `Iterator`: the Python binding
    /// adapts an arbitrary iterable, and an iterable that raises halfway simply stops. Without this
    /// hook the builder would finish a truncated index and rename it over whatever was at `path`.
    pub(crate) fn build_to_file_checked<I, S, C>(
        items: I,
        path: impl AsRef<std::path::Path>,
        layout: DictLayout,
        check: C,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        Self::build_to_file_runs(items, path.as_ref(), layout, check, RUN_BYTES)
    }

    /// [`build_to_file_checked`](Self::build_to_file_checked) with the run budget exposed, so a
    /// test can force the spill-and-merge path without a corpus of a quarter of a gigabyte.
    pub(super) fn build_to_file_runs<I, S, C>(
        items: I,
        path: &std::path::Path,
        layout: DictLayout,
        mut check: C,
        run_bytes: usize,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        layout.sizes()?;
        let mut run = Run::with_budget(run_bytes);
        let mut runs = Runs::beside(path);
        let mut read = 0usize;
        for key in items {
            let key = key.as_ref();
            read += 1;
            if !run.fits(key) {
                if run.is_empty() {
                    return Err(IndexError::Format("dict: a key longer than the run budget"));
                }
                runs.spill(run.sorted())?;
                run.clear();
            }
            run.push(key);
        }
        check()?;
        if runs.is_empty() {
            return Self::write_sorted(&mut run, path, layout, read, check);
        }
        if !run.is_empty() {
            runs.spill(run.sorted())?;
        }
        // The run's arena is the build's largest allocation and nothing reads it again.
        drop(run);
        Self::write_sorted(&mut runs, path, layout, read, check)
    }

    /// The passes and the write, over a key stream that can be walked again; `read` is how many keys
    /// went into it, repeats among them.
    fn write_sorted<R, C>(
        src: R,
        path: &std::path::Path,
        layout: DictLayout,
        read: usize,
        mut check: C,
    ) -> Result<usize, IndexError>
    where
        R: Replay,
        C: FnMut() -> Result<(), IndexError>,
    {
        let (block, micro) = layout.sizes()?;
        let mut src = src;
        // Labels want a count over every key, and sifting them in the first pass and counting them
        // in the second costs no walk of its own.
        let mut edges = layout
            .labels
            .then(|| crate::labels::EdgeCount::for_keys(read));
        // Pass one: the block heads, their ends and samples, and the key count. None of the three
        // depends on the symbol table, and the count is what fixes the training stride below.
        let mut n = 0usize;
        let mut heads: Vec<u8> = Vec::new();
        let mut head_ends: Vec<u64> = Vec::new();
        let (mut bytes, mut high) = (0u64, 0u64);
        src.each(&mut |key| {
            if n % block == 0 {
                heads.extend_from_slice(key.as_bytes());
                head_ends.push(heads.len() as u64);
            }
            bytes += key.len() as u64;
            high += charcode::high_bytes(key.as_bytes());
            n += 1;
            if let Some(edges) = &mut edges {
                edges.sift(key.as_bytes());
            }
            Ok(())
        })?;
        if let Some(edges) = &mut edges {
            edges.sifted();
        }
        // The code is chosen on exactly the counts `choose_code` takes over the same keys, so the
        // file is the blob `build` makes; a corpus the bound rules out pays for no tally pass.
        let code = if charcode::worth_counting(bytes, high) {
            let mut tally = Tally::new();
            src.each(&mut |key| {
                tally.add(key);
                Ok(())
            })?;
            tally.choose()
        } else {
            None
        };
        // A corpus in a character code takes no labels, as `build_with` has it.
        if code.is_some() {
            edges = None;
        }
        let mut spelt: Vec<u8> = Vec::new();
        if let Some(code) = &code {
            let mut at = 0;
            let mut coded = Vec::with_capacity(heads.len());
            for end in &mut head_ends {
                let head =
                    std::str::from_utf8(&heads[at..*end as usize]).expect("a head is a whole key");
                spell(Some(code), head, &mut spelt)?;
                coded.extend_from_slice(&spelt);
                at = *end as usize;
                *end = coded.len() as u64;
            }
            heads = coded;
        }
        let code = code.as_ref();
        // Every head is in, so the prefix they share is known and the samples are taken past it.
        let g = common_head_raw(&heads, &head_ends);
        let nb = n.div_ceil(block);
        let shard = shard_blocks_for(block);
        let span = shard * block;
        let shards = nb.div_ceil(shard).max(1);
        let mut step = (span.min(n) / TRAIN_PIECES).max(1);

        // Pass two: the training sample, in the order `from_sorted` collects it -- runs in order,
        // keys within a run in order, each coded against the key the encoding will code it
        // against -- so the table it trains is the same table. One sample a shard.
        let mut arena: Vec<u8> = Vec::new();
        let mut spans: Vec<Vec<(usize, usize)>> = vec![Vec::new(); shards];
        let mut prev: Vec<u8> = Vec::new();
        let mut restart: Vec<u8> = Vec::new();
        let mut i = 0usize;
        src.each(&mut |key| {
            let bytes = spell(code, key, &mut spelt)?;
            let off = i % block;
            if off == 0 && i % span == 0 {
                step = ((n - i).min(span) / TRAIN_PIECES).max(1);
            }
            let take = (i % span / TRAIN_RUN) % step == 0;
            if off == 0 || off % micro == 0 {
                if off != 0 && take {
                    let at = arena.len();
                    arena.extend_from_slice(&bytes[lcp(&restart, bytes)..]);
                    spans[i / span].push((at, arena.len()));
                }
                restart.clear();
                restart.extend_from_slice(bytes);
            } else if take {
                let at = arena.len();
                arena.extend_from_slice(&bytes[lcp(&prev, bytes)..]);
                spans[i / span].push((at, arena.len()));
            }
            prev.clear();
            prev.extend_from_slice(bytes);
            i += 1;
            if let Some(edges) = &mut edges {
                edges.count(key.as_bytes());
            }
            Ok(())
        })?;
        let (tables, phrases) = {
            let samples: Vec<Vec<&[u8]>> = spans
                .iter()
                .map(|s| s.iter().map(|&(a, b)| &arena[a..b]).collect())
                .collect();
            let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            let threads = train_threads(threads.min(shards));
            let tables = train_shards(&samples, threads);
            let phrases = mine_phrases(&samples, &tables, n, threads);
            (tables, phrases)
        };
        drop(arena);
        drop(spans);

        // A labelled blob is built as well only to be compared, and the parse that names its labels
        // needs the phrases the plain one takes.
        let labelled = match edges {
            Some(edges) => phrases.labelled_from(edges, &mut src, block, micro, &tables)?,
            None => None,
        };
        let head_width = offsets::width_of(&head_ends, offsets::SHIFT);
        let (head_bases, head_deltas) = offsets::pack(&head_ends, offsets::SHIFT, head_width);
        drop(head_ends);
        let frame = Frame {
            n,
            block,
            micro,
            shard,
            g,
            heads: &heads,
            head_width,
            head_bases,
            head_deltas,
            tables: &tables,
            code,
        };
        crate::blob::write_atomically_with(path, |w| {
            let Some(labelled) = &labelled else {
                frame.encode(&mut src, &phrases, w)?;
                return check();
            };
            // The one a labelled build should keep goes straight to its place, and the plain one
            // beside it, since a corpus that asked for labels mostly keeps them.
            let first = frame.encode(&mut src, labelled, w)?;
            let mut spare = crate::blob::Spare::beside(path)?;
            let plain = {
                let mut spare = std::io::BufWriter::new(spare.file());
                let plain = frame.encode(&mut src, &phrases, &mut spare)?;
                std::io::Write::flush(&mut spare)?;
                plain
            };
            let named = first.has_labels || first.phrases > phrases.dict.phrases();
            if !(named && first.bytes < plain.bytes) {
                std::io::Write::flush(w)?;
                let file = w.get_mut();
                file.set_len(0)?;
                std::io::Seek::rewind(file)?;
                let spare = spare.file();
                std::io::Seek::rewind(spare)?;
                std::io::copy(spare, file)?;
            }
            check()
        })?;
        Ok(n)
    }

    pub(super) fn from_sorted<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
    ) -> Result<Self, IndexError> {
        Self::from_sorted_on(keys, block, micro, build_threads(keys.len(), block))
    }

    /// The in-memory build over sorted, distinct keys — a [view](view_of) of the caller's — spelled
    /// in the [character code](charcode) they pay for, if any.
    fn from_view(view: Vec<&str>, block: usize, micro: usize) -> Result<Self, IndexError> {
        match choose_code(&view, build_threads(view.len(), block)) {
            None => Self::from_sorted(&view, block, micro),
            Some(code) => Self::from_coded(view, block, micro, code),
        }
    }

    /// [`from_view`](Self::from_view) under `code`: every key spelled, a range of them a thread,
    /// each range into an arena of its own, and the build run over the spellings.
    pub(super) fn from_coded(
        keys: Vec<&str>,
        block: usize,
        micro: usize,
        code: CharCode,
    ) -> Result<Self, IndexError> {
        let parts = in_ranges(&keys, build_threads(keys.len(), block), |range| {
            encode_all(&code, range).expect("a code chosen over the keys spells every one of them")
        });
        drop(keys);
        let coded: Vec<Bytes<'_>> = parts
            .iter()
            .flat_map(|(arena, ends)| views(arena, ends))
            .collect();
        let mut idx = Self::from_sorted(&coded, block, micro)?;
        idx.code = Some(Box::new(code));
        idx.raw = idx.raw_route();
        Ok(idx)
    }

    /// The builds a plan prices a `DictIndex` from: a uniform draw of the corpus at the default
    /// block, and a draw of consecutive runs at each of `blocks` — every one of them coded under
    /// the one vocabulary the corpus buys.
    ///
    /// A sample of a hundred thousand keys standing in for a million has to buy the vocabulary the
    /// million would: the miner admits a span on what it saves over the whole blob against what
    /// storing it costs once, so at the sample's own count it buys a tenth of the dictionary and
    /// reads the suffix ratio 0.71 where the million-key blob spends 0.53 on article titles. So the
    /// sample is mined at `corpus` keys, over every suffix it holds rather than [`TRAIN_PIECES`] a
    /// shard — at a third of them the plan's worst blob goes from 5.9 % out to 21.9 % — and in as
    /// many pools as the corpus's own build would fill.
    ///
    /// **That vocabulary is bought once for the plan**, not once a draw and not once a block. It is
    /// a property of the corpus rather than of the block: over eleven corpora the three priced
    /// blocks read the suffix ratio within 0.4 % of each other and the dictionary within 3 %, and
    /// pricing each block off its own mined sample was both two thirds of what a plan spends and
    /// *less* accurate — the smallest block's own draw reads the ratio 21 % high on decimal ids.
    /// Every priced block shards the keys the same way — a shard is [`SHARD_KEYS`] keys whatever
    /// the block — so one set of tables covers the same ranges in all three.
    pub(crate) fn plan_builds<K: AsKey + Sync>(
        spread: &[K],
        runs: &[K],
        blocks: &[usize],
        corpus: usize,
    ) -> Result<(Self, Vec<Self>), IndexError> {
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let micro = micro_for(DEFAULT_BLOCK);
        let pieces = build_pieces(spread, DEFAULT_BLOCK, micro, 1);
        let tables = train_shards(&pieces, train_threads(threads));
        let phrases = mine_phrases(&pieces, &tables, corpus, train_threads(threads));
        drop(pieces);
        let spread = Self::from_vocabulary(
            spread,
            DEFAULT_BLOCK,
            micro,
            build_threads(spread.len(), DEFAULT_BLOCK),
            &tables,
            &phrases,
        )?;
        // The run draw keeps its own tables — a symbol table is a neighbourhood's, and runs of
        // consecutive keys are what carries one — but not its own dictionary: it would mine the
        // vocabulary of a hundred thousand keys, and what a block costs is read off a blob the
        // corpus's vocabulary coded.
        let runs = blocks
            .iter()
            .map(|&block| {
                let micro = micro_for(block);
                let threads = build_threads(runs.len(), block);
                let pieces = build_pieces(runs, block, micro, 0);
                let tables = train_shards(&pieces, train_threads(threads));
                drop(pieces);
                Self::from_vocabulary(runs, block, micro, threads, &tables, &phrases)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((spread, runs))
    }

    /// [`from_sorted`](Self::from_sorted) with the thread count fixed, so a test can hold the
    /// output to the one a single thread produces.
    pub(super) fn from_sorted_on<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
        threads: usize,
    ) -> Result<Self, IndexError> {
        Self::from_sorted_as(keys, block, micro, threads, keys.len())
    }

    /// The build over sorted, distinct keys, with no trie labels whatever the corpus: what the
    /// nested dictionary of a labelled one is built with.
    pub(crate) fn from_sorted_plain<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
    ) -> Result<Self, IndexError> {
        Self::from_sorted(keys, block, micro)
    }

    /// [`from_sorted`](Self::from_sorted) with the blob's trie labels named in its dictionary
    /// (`crate::labels`); `None` where none pays or no shard would name them.
    pub(super) fn from_sorted_labelled<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
    ) -> Result<Option<Self>, IndexError> {
        let threads = build_threads(keys.len(), block);
        let pieces = build_pieces(keys, block, micro, 0);
        let tables = train_shards(&pieces, train_threads(threads));
        let phrases = mine_phrases(&pieces, &tables, keys.len(), train_threads(threads));
        drop(pieces);
        let mined = phrases.dict.phrases();
        let Some(phrases) = phrases.labelled(keys, block, micro, &tables) else {
            return Ok(None);
        };
        let idx = Self::from_vocabulary(keys, block, micro, threads, &tables, &phrases)?;
        // A shard that settles for no phrases names no label either, flat or nested.
        let named = idx.phrases.has_labels() || idx.phrases.phrases() > mined;
        Ok(named.then_some(idx))
    }

    /// Whether the dictionary names trie labels: a `BDX5` blob.
    pub(crate) fn has_labels(&self) -> bool {
        self.phrases.has_labels()
    }

    /// Whether the keys are spelled in a character code: a `BDX4` blob.
    pub(crate) fn is_coded(&self) -> bool {
        self.code.is_some()
    }

    /// The build, with the phrase dictionary bought for a corpus of `corpus` keys. That is the
    /// index's own count for every build but the estimator's. A sample of a hundred thousand keys
    /// standing in for a million has to buy the vocabulary the million would: the miner admits a
    /// span on what it saves over the whole blob against what storing it costs once, so at the
    /// sample's own count it buys a tenth of the dictionary and reads the suffix ratio 0.71 where
    /// the million-key blob spends 0.53 on article titles. Such a build also trains and mines on
    /// every suffix it has rather than [`TRAIN_PIECES`] a shard, since its evidence is already an
    /// eighth of the real build's.
    fn from_sorted_as<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
        threads: usize,
        corpus: usize,
    ) -> Result<Self, IndexError> {
        let pieces = build_pieces(keys, block, micro, usize::from(corpus != keys.len()));
        let tables = train_shards(&pieces, train_threads(threads));
        let phrases = mine_phrases(&pieces, &tables, corpus, train_threads(threads));
        drop(pieces);
        Self::from_vocabulary(keys, block, micro, threads, &tables, &phrases)
    }

    /// The encoding half of a build: the keys coded under a vocabulary already chosen. Split out
    /// because the estimator prices three blocks off one sample and the vocabulary is the corpus's
    /// rather than the block's — see [`plan_builds`](Self::plan_builds).
    pub(super) fn from_vocabulary<K: AsKey + Sync>(
        keys: &[K],
        block: usize,
        micro: usize,
        threads: usize,
        tables: &[Table],
        phrases: &Phrases,
    ) -> Result<Self, IndexError> {
        let n = keys.len();
        let nb = n.div_ceil(block);
        let shard = shard_blocks_for(block);
        let span = shard * block;

        // Once the table is fixed a block depends on nothing outside itself, so contiguous ranges
        // of blocks encode on their own threads and the parts are concatenated in order — the bytes
        // do not depend on how many threads ran.
        // A range must cover whole shards, because a shard's header code is chosen from all of
        // its runs at once: split inside one and two threads would each choose their own.
        let run = (span * nb.div_ceil(threads).div_ceil(shard)).max(1);
        let parts: Vec<Part> = if threads == 1 {
            keys.chunks(run)
                .enumerate()
                .map(|(c, range)| {
                    encode_range(range, block, micro, tables, phrases, shard, c * run / block)
                })
                .collect()
        } else {
            std::thread::scope(|scope| {
                let running: Vec<_> = keys
                    .chunks(run)
                    .enumerate()
                    .map(|(c, range)| {
                        scope.spawn(move || {
                            encode_range(
                                range,
                                block,
                                micro,
                                tables,
                                phrases,
                                shard,
                                c * run / block,
                            )
                        })
                    })
                    .collect();
                running
                    .into_iter()
                    .map(|h| h.join().expect("encoding a range of blocks cannot panic"))
                    .collect()
            })
        };

        let mut heads = Vec::with_capacity(parts.iter().map(|p| p.heads.len()).sum());
        let mut head_ends = Vec::with_capacity(nb);
        let mut blocks = Vec::with_capacity(nb);
        let mut micros = Vec::with_capacity(parts.iter().map(|p| p.micros.len()).sum());
        let mut data = Vec::with_capacity(parts.iter().map(|p| p.data.len()).sum());
        let mut codes = Vec::with_capacity(nb.div_ceil(shard).max(1));
        let mut codecs = Vec::with_capacity(nb.div_ceil(shard).max(1));
        // Each part is dropped as it is appended, so the two copies never coexist whole.
        for part in parts {
            let (at_head, at_data) = (heads.len() as u64, data.len() as u64);
            head_ends.extend(part.head_ends.iter().map(|end| at_head + end));
            blocks.extend(part.blocks.iter().map(|start| at_data + start));
            micros.extend(part.micros.iter().map(|start| at_data + start));
            heads.extend_from_slice(&part.heads);
            data.extend_from_slice(&part.data);
            codes.extend(part.codes);
            codecs.extend(part.codecs);
        }
        rebase_micros(&mut micros, &blocks, block.div_ceil(micro));
        // An index of no keys still holds one codec and one pair of codes, which is what a reader
        // indexes into.
        if codes.is_empty() {
            codes.push((
                Code::Frame(paircode::Frames::NONE),
                Code::Frame(paircode::Frames::NONE),
            ));
            codecs.push(Codec::Symbols {
                table: tables[0].clone(),
                split: None,
            });
        }
        // A dictionary no shard bought is bytes the blob carries and nothing names — which is the
        // answer on a corpus where the miner found spans that repeat but none that pay.
        let phrases = if codecs.iter().any(Codec::has_phrases) {
            phrases.dict.clone()
        } else {
            Dict::empty()
        };
        // Every head is in, so the prefix they share is known and the samples are taken past it.
        let g = common_head_raw(&heads, &head_ends);
        let samples = samples_of(&heads, &head_ends, g);
        let mut idx = Self {
            block,
            micro,
            per: block.div_ceil(micro),
            n,
            heads: SharedBytes::from_owned(heads),
            head_ends: packed_offsets(&head_ends),
            samples,
            ties: None,
            blocks: packed_offsets(&blocks),
            micros: packed_micros(if block.div_ceil(micro) == 1 {
                &[]
            } else {
                &micros
            }),
            data: SharedBytes::from_owned(data),
            codecs,
            codes,
            shard,
            shard_bits: shard_bits_of(shard),
            phrases,
            g,
            code: None,
            raw: None,
            mroute: OnceLock::new(),
        };
        idx.ties = Ties::derive(&idx.samples, |b| idx.head(b));
        Ok(idx)
    }
}

/// What every encoding of a streamed build shares: all of it known before the first entry is coded.
struct Frame<'a> {
    n: usize,
    block: usize,
    micro: usize,
    shard: usize,
    g: usize,
    heads: &'a [u8],
    head_width: u32,
    head_bases: Vec<u8>,
    head_deltas: Vec<u8>,
    tables: &'a [Table],
    code: Option<&'a CharCode>,
}

/// What a streamed encoding comes to: its size, and what its dictionary kept.
struct Encoded {
    bytes: u64,
    has_labels: bool,
    phrases: usize,
}

impl Frame<'_> {
    /// The blob under `phrases`, whole, over one more walk of `src`, into `w`.
    fn encode<R: Replay>(
        &self,
        src: &mut R,
        phrases: &Phrases,
        w: &mut std::io::BufWriter<&mut std::fs::File>,
    ) -> Result<Encoded, IndexError> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let (block, micro) = (self.block, self.micro);
        let (shard, span) = (self.shard, self.shard * self.block);
        let nb = self.n.div_ceil(block);
        let mut at_shard = 0usize;
        let mut encoder = self.tables[0].encoder();
        let mut blocks = Vec::with_capacity(nb);
        let mut micros = Vec::with_capacity(nb * block.div_ceil(micro));
        let mut spelt: Vec<u8> = Vec::new();
        // The header is written last: it carries the block data's length and a hash over every
        // section, and neither is known until the encoding is done.
        w.write_all(&[0u8; HEADER])?;
        w.write_all(self.heads)?;
        w.write_all(&self.head_bases)?;
        w.write_all(&self.head_deltas)?;

        // Pass three: encode. The block and microblock starts fall out of it, which is why
        // their sections were left to the end.
        let mut data_len = 0u64;
        let mut coded = Vec::with_capacity(64);
        // A shard's entries are coded once, from all of its runs at once, so a shard is
        // collected before any of it is written: a couple of megabytes of pairs and coded
        // suffixes, against the key stream this build exists for. Its keys are buffered a
        // block at a time, because a block is written restarts first and they arrive last.
        let shards = nb.div_ceil(shard).max(1);
        let mut codes: Vec<(Code, Code)> = Vec::with_capacity(shards);
        let mut codecs: Vec<Codec> = Vec::with_capacity(shards);
        let mut pairs = Vec::new();
        let mut collected = Collected::default();
        let mut parsed = phrase::Scratch::default();
        let mut scratch = Scratch::default();
        let mut keys: Vec<u8> = Vec::with_capacity(block * 32);
        // `ends[k]` closes the k-th buffered key, so the first entry is where the first
        // one starts and what `truncate` leaves behind.
        let mut ends: Vec<usize> = Vec::with_capacity(block + 1);
        ends.push(0);
        let mut i = 0usize;
        src.each(&mut |key| {
            if i % block == 0 && i > 0 {
                let view: Vec<&[u8]> = ends.windows(2).map(|w| &keys[w[0]..w[1]]).collect();
                collect_block(&mut collected, &view, micro, &encoder, &mut coded);
                keys.clear();
                ends.truncate(1);
            }
            if i % span == 0 && i > 0 {
                let (codec, pair) = collected.settle(
                    self.tables[at_shard].clone(),
                    phrases,
                    &mut pairs,
                    &mut parsed,
                );
                Shard {
                    collected: &collected,
                    pairs: &pairs,
                    codec: &codec,
                    codes: &pair,
                }
                .write(
                    &mut data_len,
                    &mut blocks,
                    &mut micros,
                    &mut scratch,
                    &mut |bytes| Ok(w.write_all(bytes)?),
                )?;
                codes.push(pair);
                codecs.push(codec);
                collected.clear();
                at_shard = i / span;
                encoder = self.tables[at_shard].encoder();
            }
            keys.extend_from_slice(spell(self.code, key, &mut spelt)?);
            ends.push(keys.len());
            i += 1;
            Ok(())
        })?;
        let view: Vec<&[u8]> = ends.windows(2).map(|w| &keys[w[0]..w[1]]).collect();
        if !view.is_empty() {
            collect_block(&mut collected, &view, micro, &encoder, &mut coded);
        }
        let (codec, pair) = collected.settle(
            self.tables[at_shard].clone(),
            phrases,
            &mut pairs,
            &mut parsed,
        );
        Shard {
            collected: &collected,
            pairs: &pairs,
            codec: &codec,
            codes: &pair,
        }
        .write(
            &mut data_len,
            &mut blocks,
            &mut micros,
            &mut scratch,
            &mut |bytes| Ok(w.write_all(bytes)?),
        )?;
        codes.push(pair);
        codecs.push(codec);
        // Both dictionaries follow the data, because neither a shard's suffix codec nor its
        // header codes is known until the shard is coded.
        let mut table_bytes = Vec::with_capacity(codecs_len(&codecs));
        write_codecs(&codecs, &mut table_bytes);
        w.write_all(&table_bytes)?;
        let mut code_bytes = Vec::with_capacity(codes_len(&codes));
        write_codes(&codes, &mut code_bytes);
        w.write_all(&code_bytes)?;
        // A dictionary no shard bought is bytes the blob carries and nothing names.
        let kept = if codecs.iter().any(Codec::has_phrases) {
            &phrases.dict
        } else {
            &Dict::empty()
        };
        let mut dictionary = Vec::with_capacity(kept.serialized_len());
        kept.write_to(&mut dictionary);
        w.write_all(&dictionary)?;
        if i != self.n {
            return Err(IndexError::Format(
                "dict: the key stream changed between passes",
            ));
        }

        // The two start arrays are the last sections precisely because this is where they are
        // known: their widths fall out of the encoding that has just finished.
        let block_width = offsets::width_of(&blocks, offsets::SHIFT);
        let (block_bases, block_deltas) = offsets::pack(&blocks, offsets::SHIFT, block_width);
        // A block that is one microblock starts its only microblock where the block starts.
        rebase_micros(&mut micros, &blocks, block.div_ceil(micro));
        let micros: &[u64] = if block.div_ceil(micro) == 1 {
            &[]
        } else {
            &micros
        };
        let micro_width = offsets::packed_width_of(micros);
        let micro_deltas = offsets::packed_pack(micros, micro_width);
        w.write_all(&block_bases)?;
        w.write_all(&block_deltas)?;
        w.write_all(&micro_deltas)?;
        let mut chars = Vec::with_capacity(self.code.map_or(0, CharCode::serialized_len));
        if let Some(code) = self.code {
            code.write_to(&mut chars);
        }
        w.write_all(&chars)?;
        w.flush()?;
        // The payload hash runs over the sections in blob order, so it is taken from the file
        // rather than from the stream: one sequential read of what was just written.
        let file = w.get_mut();
        file.seek(SeekFrom::Start(HEADER as u64))?;
        let mut hasher = crate::blob::BlockHasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let got = file.read(&mut buf)?;
            if got == 0 {
                break;
            }
            hasher.update(&buf[..got]);
        }
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(if self.code.is_some() {
            CODED_MAGIC
        } else if kept.has_labels() {
            LABELLED_MAGIC
        } else {
            MAGIC
        });
        h[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        h[12..16].copy_from_slice(&(self.block as u32).to_le_bytes());
        h[16..24].copy_from_slice(&(self.heads.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&data_len.to_le_bytes());
        h[32..36].copy_from_slice(&(table_bytes.len() as u32).to_le_bytes());
        h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
        h[44] = self.head_width as u8;
        h[45] = block_width as u8;
        h[46] = offsets::SHIFT as u8;
        h[47] = micro_width as u8;
        h[48..50].copy_from_slice(&(self.micro as u16).to_le_bytes());
        h[50..52].copy_from_slice(&(self.shard as u16).to_le_bytes());
        h[52..56].copy_from_slice(&(code_bytes.len() as u32).to_le_bytes());
        h[56..58].copy_from_slice(&(self.g as u16).to_le_bytes());
        h[58..62].copy_from_slice(&(dictionary.len() as u32).to_le_bytes());
        h[62..66].copy_from_slice(&(chars.len() as u32).to_le_bytes());
        let check_word = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..HEADER].copy_from_slice(&check_word.to_le_bytes());
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&h)?;
        let file = w.get_mut();
        file.seek(SeekFrom::End(0))?;
        Ok(Encoded {
            bytes: file.stream_position()?,
            has_labels: kept.has_labels(),
            phrases: kept.phrases(),
        })
    }
}
