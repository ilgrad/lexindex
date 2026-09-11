//! An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways, in
//! 41 % less than [`StringIndex`](crate::StringIndex) takes.
//!
//! The sorted keys are cut into blocks of `block` keys (32 by default). A block stores its first
//! key whole and every other as the length of the prefix it shares with its predecessor and the
//! suffix after it, the suffix under a static symbol table ([`fsst`]) trained on the index's own
//! suffixes. Beside the blocks sit three flat arrays with one entry per block: where its head key
//! ends, an eight-byte sample of that head, and where its entries start.
//!
//! `id` is a binary search over the samples, then over the heads of the few blocks whose sample
//! equals the probe's, then one block scanned without decoding anything: an entry's stored suffix
//! is compared against the probe symbol by symbol, and the shared-prefix length says on its own
//! when the probe has been passed. `key(id)` is the block's head plus the entries between it and
//! the id whose shared-prefix length strictly increases — a monotonic stack over the headers finds
//! them, and only those are decoded, each one eight-byte store per code. There are no automata, so
//! a fuzzy query is a `StringIndex` question; prefix and range are two order lookups and a walk,
//! which this index answers itself at 3–4 bytes per key.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::extsort::{RUN_BYTES, Run, Runs};
use crate::fsst::{self, ESCAPE, Table};
use std::cmp::Ordering;

/// `[magic 4][n u64][block u32][heads u64][data u64][table u32][payload u64][check u32]`, then
/// the symbol table, the head keys end to end, the head ends (`u32` each), the head samples
/// (`u64`), the block starts (`u64`), and the block data.
const MAGIC: &[u8; 4] = b"BDX1";
const HEADER: usize = 48;
const CHECKED: usize = 44; // header bytes the trailing check covers
const PER_BLOCK: usize = 4 + 8 + 8; // bytes the three arrays hold per block
const DEFAULT_BLOCK: usize = 32;
const MAX_BLOCK: usize = 1024;
/// How deep a key's staircase of shared prefixes may be before [`DictIndex::key_bytes_into`] gives
/// up tracking it and decodes every entry instead. The dictionary reaches 12 at the largest block
/// and a path list 18, so the bound is slack; a block holding a chain like `a`, `aa`, `aaa` is what
/// reaches it, which is legal input and merely slow, not wrong.
const STAIRS: usize = 32;
/// About how many suffixes the symbol table is trained on: every block's worth, from blocks spread
/// evenly over the index.
const TRAIN_PIECES: usize = 20_000;
/// Samples serialised at a time, so writing does not copy the array whole.
const CHUNK: usize = 4096;
/// Blocks a thread must be given before the encoding is worth splitting across threads at all.
const PARALLEL_BLOCKS: usize = 64;

/// An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways.
///
/// Ids are ranks. `id(key)` is the number of keys below it, `key(id)` the key at that rank, and
/// [`lower_bound`](Self::lower_bound) the rank a key would have, so every range of keys is a
/// range of ids. 3.52 bytes per key on real words, against 5.95 for the transducer of
/// [`StringIndex`](crate::StringIndex) and 10.9 for [`PerfectHashIndex`](crate::PerfectHashIndex);
/// `id` costs a few hundred nanoseconds and `key` about two hundred, both dominated by the block
/// scan, which the `block` given at build time sets — smaller blocks are faster and larger.
///
/// Immutable once built, and built in memory: the keys are sorted and deduplicated, then encoded
/// block by block. Persisted with [`to_bytes`](Self::to_bytes) / [`save`](Self::save) and read
/// back by [`from_bytes`](Self::from_bytes) / [`load`](Self::load), which check every length and
/// both checksums, or by `load_mmap`, which borrows the keys and the block data from the mapped
/// file and reads only the per-block samples.
pub struct DictIndex {
    block: usize,
    n: usize,
    /// The sections as they are serialised, owned or mapped. Every block's first key, whole, end
    /// to end; `head_ends[b]` closes block `b`'s.
    heads: SharedBytes,
    /// A `u32` per block, little-endian, decoded where it is read — the two arrays below too.
    head_ends: SharedBytes,
    /// The first eight bytes of each head as a big-endian word, so a search compares heads only
    /// inside the run of blocks that share the probe's.
    ///
    /// The one section a mapping does not borrow. Two binary searches over it open every lookup,
    /// and `<[u64]>::partition_point` is the only form of that search that keeps its steps out of
    /// the branch predictor: it selects with `hint::select_unpredictable`, which needs a `u64`
    /// slice and so an alignment a section of a blob does not have. Reading the words out of the
    /// bytes instead measured 110 ns against 26 for the two searches — a hand-written branchless
    /// step is folded back into a branch by the compiler, and blocking that with `black_box` pays
    /// the same back in instructions. Eight bytes a block, so a mapped index holds one byte per
    /// four keys at the default block, and borrows everything else.
    samples: Vec<u64>,
    /// Where block `b`'s `block − 1` front-coded entries start in `data`, a `u64` per block.
    blocks: SharedBytes,
    data: SharedBytes,
    table: Table,
}

/// The sample the search runs on: a key's first eight bytes, zero-padded, in byte order.
#[inline(always)]
fn sample_of(key: &[u8]) -> u64 {
    fsst::word_at(key, 0).swap_bytes()
}

/// Entry `i` of a little-endian `u32` array.
#[inline(always)]
fn u32_at(array: &[u8], i: usize) -> usize {
    u32::from_le_bytes(array[4 * i..4 * i + 4].try_into().expect("4 bytes")) as usize
}

/// Entry `i` of a little-endian `u64` array.
#[inline(always)]
fn u64_at(array: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(array[8 * i..8 * i + 8].try_into().expect("8 bytes"))
}

/// Block `b`'s head out of its sections. The arrays are trusted only as far as their sections
/// reach: a mapping is loaded without the walk over them, so an end past the heads, or before
/// the previous one, gives a short head rather than a panic.
#[inline(always)]
fn head_of<'a>(heads: &'a [u8], ends: &[u8], b: usize) -> &'a [u8] {
    let end = u32_at(ends, b);
    let start = if b == 0 { 0 } else { u32_at(ends, b - 1) };
    heads.get(start..end).unwrap_or_default()
}

/// How many leading bytes `a` and `b` share.
#[inline]
fn lcp(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = fsst::word_at(a, i) ^ fsst::word_at(b, i);
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn put_varint(out: &mut Vec<u8>, mut v: usize) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// The varint at the start of `data` and the bytes after it; `None` if it is cut short or would
/// not fit a `usize`.
fn get_varint(mut data: &[u8]) -> Option<(usize, &[u8])> {
    let mut v = 0usize;
    let mut shift = 0u32;
    loop {
        let (&b, rest) = data.split_first()?;
        data = rest;
        let part = (b & 0x7F) as usize;
        if shift >= usize::BITS || part.checked_shl(shift)? >> shift != part {
            return None;
        }
        v |= part << shift;
        if b < 0x80 {
            return Some((v, data));
        }
        shift += 7;
    }
}

/// An entry's header: one byte `lcp << 4 | len` when both are below 15, else `0xFF` and the two
/// as varints.
fn put_header(out: &mut Vec<u8>, lcp: usize, len: usize) {
    if lcp < 15 && len < 15 {
        out.push(((lcp << 4) | len) as u8);
    } else {
        out.push(0xFF);
        put_varint(out, lcp);
        put_varint(out, len);
    }
}

/// The header at the start of `data` as (lcp, len, the bytes after it); `None` if it is cut short.
#[inline(always)]
fn get_header(data: &[u8]) -> Option<(usize, usize, &[u8])> {
    let (&b, rest) = data.split_first()?;
    if b != 0xFF {
        return Some(((b >> 4) as usize, (b & 0xF) as usize, rest));
    }
    let (lcp, rest) = get_varint(rest)?;
    let (len, rest) = get_varint(rest)?;
    Some((lcp, len, rest))
}

/// What one thread produces for its contiguous range of blocks: the sections it would have
/// appended, with the two that are offsets kept relative to the range so the concatenation can
/// rebase them.
struct Part {
    heads: Vec<u8>,
    head_ends: Vec<u64>,
    samples: Vec<u64>,
    blocks: Vec<u64>,
    data: Vec<u8>,
}

/// Encode a range of whole blocks. `keys` must start on a block boundary, which is what makes the
/// parts concatenate into the blob a single pass would have written.
fn encode_range<S: AsRef<str>>(keys: &[S], block: usize, encoder: &fsst::Encoder) -> Part {
    let nb = keys.len().div_ceil(block);
    let mut part = Part {
        heads: Vec::new(),
        head_ends: Vec::with_capacity(nb),
        samples: Vec::with_capacity(nb),
        blocks: Vec::with_capacity(nb),
        data: Vec::new(),
    };
    let mut packed = Vec::with_capacity(64);
    for chunk in keys.chunks(block) {
        let head = chunk[0].as_ref().as_bytes();
        part.heads.extend_from_slice(head);
        part.head_ends.push(part.heads.len() as u64);
        part.samples.push(sample_of(head));
        part.blocks.push(part.data.len() as u64);
        for w in chunk.windows(2) {
            let (a, b) = (w[0].as_ref().as_bytes(), w[1].as_ref().as_bytes());
            let l = lcp(a, b);
            packed.clear();
            encoder.encode_into(&b[l..], &mut packed);
            put_header(&mut part.data, l, packed.len());
            part.data.extend_from_slice(&packed);
        }
    }
    part
}

impl DictIndex {
    /// Build from a collection of strings, in any order; duplicates are removed and the ids are
    /// the ranks of the distinct keys in byte order. Blocks of 32 keys.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_with_block(items, DEFAULT_BLOCK)
    }

    /// [`build`](Self::build) with `block` keys per block, `1..=1024`. A lookup scans up to
    /// `block − 1` entries, and a reverse lookup reads that many headers but decodes only the
    /// staircase among them — so smaller blocks are faster and larger ones share more and store
    /// less. On real words 16 / 32 / 64 / 128 / 256 give 4.35 / 3.52 / 3.10 / 2.89 / 2.78 bytes
    /// per key, `id` at 302–311 / 314–337 / 354–358 / 437–441 / 599–602 ns and `key_into` at
    /// 124–125 / 173–176 / 269–272 / 462–463 / 848–853. At 128 the index is under `marisa-trie`'s
    /// 2.98 on that corpus.
    pub fn build_with_block<I, S>(items: I, block: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        // Sorted and deduplicated in place, comparing through `AsRef` rather than collecting owned
        // `String`s: every key is copied into a block below in either case, so the intermediate
        // copy only doubled the peak for a caller that already owned the corpus.
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        Self::from_sorted(&keys, block)
    }

    /// Build from keys that are **already in ascending byte order** — a sorted file, a database
    /// cursor, the output of an external sort. Adjacent duplicates are dropped exactly as
    /// [`build`](Self::build) drops them after sorting, so for the same key set the two produce
    /// **byte-identical** blobs. Blocks of 32 keys.
    ///
    /// It is not the faster of the two and does not claim to be: `build`'s sort is
    /// pattern-defeating, so it recognises an ascending run and returns almost at once — over
    /// 479 823 words the two measured 31.9 against 32.1 ms. Nor does it hold less, since the symbol
    /// table is trained on a sample of the blocks before any block is encoded, so the keys are read
    /// twice either way. What it buys is the **check**.
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
        let mut keys: Vec<S> = items.into_iter().collect();
        if keys.windows(2).any(|w| w[1].as_ref() < w[0].as_ref()) {
            return Err(IndexError::Format(
                "dict: build_sorted got a key below its predecessor",
            ));
        }
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        Self::from_sorted(&keys, block)
    }

    /// The constructor for a corpus that does not fit in memory, written straight to `path`: the
    /// keys, in any order, are sorted in runs that spill beside the output and merged back, and the
    /// block data is encoded into the file as it is produced. Returns the number of distinct keys
    /// written, since a caller streaming keys it does not retain has no other way to learn how many
    /// were distinct. Blocks of 32 keys.
    ///
    /// The bytes are exactly what [`build`](Self::build) would produce for the same key set, through
    /// the same atomic replace [`save`](Self::save) uses — a crash leaves either the previous file
    /// or nothing, never a half-written index.
    ///
    /// **What is still held.** Not the corpus, and not the block data, which is the bulk of the
    /// index. The block heads and the three per-block arrays are, at roughly
    /// `(mean head length + 20) / block` bytes per key — under 1 at 32 keys per block on English
    /// words, a quarter of that at 128. An index whose heads alone pass 4 GiB is refused with an
    /// error naming the larger block that would fit it.
    ///
    /// **Transient disk**: one run file per `RUN_BYTES` of keys, in a directory beside the output,
    /// removed however the build ends. **Three passes** over the sorted keys: the symbol table is
    /// trained on suffixes from blocks spread over the index, that spread is a function of the key
    /// count, and the key count is what the first pass establishes. Where the corpus needed more
    /// than one run, each pass is a fresh merge of them.
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
        Self::build_to_file_checked(items, path, block, || Ok(()))
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
        block: usize,
        check: C,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        Self::build_to_file_runs(items, path.as_ref(), block, check, RUN_BYTES)
    }

    /// [`build_to_file_checked`](Self::build_to_file_checked) with the run budget exposed, so a
    /// test can force the spill-and-merge path without a corpus of a quarter of a gigabyte.
    fn build_to_file_runs<I, S, C>(
        items: I,
        path: &std::path::Path,
        block: usize,
        mut check: C,
        run_bytes: usize,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        let mut run = Run::with_budget(run_bytes);
        let mut runs = Runs::beside(path);
        for key in items {
            let key = key.as_ref();
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
            return Self::write_sorted(&mut run, path, block, check);
        }
        if !run.is_empty() {
            runs.spill(run.sorted())?;
        }
        // The run's arena is the build's largest allocation and nothing reads it again.
        drop(run);
        Self::write_sorted(&runs, path, block, check)
    }

    /// The three passes and the write, over a key stream that can be walked again.
    fn write_sorted<R, C>(
        src: R,
        path: &std::path::Path,
        block: usize,
        mut check: C,
    ) -> Result<usize, IndexError>
    where
        R: Replay,
        C: FnMut() -> Result<(), IndexError>,
    {
        use std::io::{Read, Seek, SeekFrom, Write};

        let mut src = src;
        // Pass one: the block heads, their ends and samples, and the key count. None of the three
        // depends on the symbol table, and the count is what fixes the training stride below.
        let mut n = 0usize;
        let mut heads: Vec<u8> = Vec::new();
        let mut head_ends: Vec<u8> = Vec::new();
        let mut samples: Vec<u8> = Vec::new();
        src.each(&mut |key| {
            if n % block == 0 {
                heads.extend_from_slice(key.as_bytes());
                let end = u32::try_from(heads.len()).map_err(|_| {
                    IndexError::Format("dict: the block heads exceed 4 GiB; use a larger block")
                })?;
                head_ends.extend_from_slice(&end.to_le_bytes());
                samples.extend_from_slice(&sample_of(key.as_bytes()).to_le_bytes());
            }
            n += 1;
            Ok(())
        })?;
        let nb = n.div_ceil(block);
        let step = (nb * (block - 1) / TRAIN_PIECES).max(1);

        // Pass two: the training sample, in the order `from_sorted` collects it -- blocks in order,
        // entries within a block in order -- so the table it trains is the same table.
        let mut arena: Vec<u8> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut prev: Vec<u8> = Vec::new();
        let mut i = 0usize;
        src.each(&mut |key| {
            let bytes = key.as_bytes();
            if i % block != 0 && (i / block) % step == 0 {
                let at = arena.len();
                arena.extend_from_slice(&bytes[lcp(&prev, bytes)..]);
                spans.push((at, arena.len()));
            }
            prev.clear();
            prev.extend_from_slice(bytes);
            i += 1;
            Ok(())
        })?;
        let table = {
            let pieces: Vec<&[u8]> = spans.iter().map(|&(a, b)| &arena[a..b]).collect();
            Table::train(&pieces)
        };
        drop(arena);
        drop(spans);

        let mut table_bytes = Vec::with_capacity(table.serialized_len());
        table.write_to(&mut table_bytes);
        let encoder = table.encoder();
        let blocks_at = HEADER + table_bytes.len() + heads.len() + head_ends.len() + samples.len();
        let mut blocks = Vec::with_capacity(nb * 8);
        crate::blob::write_atomically_with(path, |w| {
            // The header is written last: it carries the block data's length and a hash over every
            // section, and neither is known until the encoding is done.
            w.write_all(&[0u8; HEADER])?;
            w.write_all(&table_bytes)?;
            w.write_all(&heads)?;
            w.write_all(&head_ends)?;
            w.write_all(&samples)?;
            let zeros = [0u8; 4096];
            let mut left = nb * 8;
            while left > 0 {
                let take = left.min(zeros.len());
                w.write_all(&zeros[..take])?;
                left -= take;
            }

            // Pass three: encode. The block starts fall out of it, which is why their section was
            // reserved rather than written.
            let mut data_len = 0u64;
            let mut packed = Vec::with_capacity(64);
            let mut entry = Vec::with_capacity(80);
            let mut prev: Vec<u8> = Vec::new();
            let mut i = 0usize;
            src.each(&mut |key| {
                let bytes = key.as_bytes();
                if i % block == 0 {
                    blocks.extend_from_slice(&data_len.to_le_bytes());
                } else {
                    let l = lcp(&prev, bytes);
                    packed.clear();
                    encoder.encode_into(&bytes[l..], &mut packed);
                    entry.clear();
                    put_header(&mut entry, l, packed.len());
                    entry.extend_from_slice(&packed);
                    w.write_all(&entry)?;
                    data_len += entry.len() as u64;
                }
                prev.clear();
                prev.extend_from_slice(bytes);
                i += 1;
                Ok(())
            })?;
            if i != n {
                return Err(IndexError::Format(
                    "dict: the key stream changed between passes",
                ));
            }

            w.flush()?;
            let file = w.get_mut();
            file.seek(SeekFrom::Start(blocks_at as u64))?;
            file.write_all(&blocks)?;
            // The payload hash runs over the sections in blob order, and the block starts are only
            // known once the data is encoded -- so it is taken from the file rather than from the
            // stream, one sequential read of what was just written.
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
            h[0..4].copy_from_slice(MAGIC);
            h[4..12].copy_from_slice(&(n as u64).to_le_bytes());
            h[12..16].copy_from_slice(&(block as u32).to_le_bytes());
            h[16..24].copy_from_slice(&(heads.len() as u64).to_le_bytes());
            h[24..32].copy_from_slice(&data_len.to_le_bytes());
            h[32..36].copy_from_slice(&(table_bytes.len() as u32).to_le_bytes());
            h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
            let check_word = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
            h[CHECKED..HEADER].copy_from_slice(&check_word.to_le_bytes());
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&h)?;
            check()
        })?;
        Ok(n)
    }

    fn from_sorted<S: AsRef<str>>(keys: &[S], block: usize) -> Result<Self, IndexError> {
        let threads = (keys.len().div_ceil(block) / PARALLEL_BLOCKS)
            .min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
            .max(1);
        Self::from_sorted_on(keys, block, threads)
    }

    /// [`from_sorted`](Self::from_sorted) with the thread count fixed, so a test can hold the
    /// output to the one a single thread produces.
    fn from_sorted_on<S: AsRef<str>>(
        keys: &[S],
        block: usize,
        threads: usize,
    ) -> Result<Self, IndexError> {
        let n = keys.len();
        let nb = n.div_ceil(block);
        // Train on the suffixes a spread of blocks would store.
        let step = (nb * (block - 1) / TRAIN_PIECES).max(1);
        let mut pieces: Vec<&[u8]> = Vec::new();
        for (b, chunk) in keys.chunks(block).enumerate() {
            if b % step != 0 {
                continue;
            }
            for w in chunk.windows(2) {
                let (a, b) = (w[0].as_ref().as_bytes(), w[1].as_ref().as_bytes());
                pieces.push(&b[lcp(a, b)..]);
            }
        }
        let table = Table::train(&pieces);
        drop(pieces);
        let encoder = table.encoder();

        // Once the table is fixed a block depends on nothing outside itself, so contiguous ranges
        // of blocks encode on their own threads and the parts are concatenated in order — the bytes
        // do not depend on how many threads ran. The threads take a `&[&str]` view rather than the
        // caller's `&[S]`, which would need `S: Sync` on a signature that has not asked for it; the
        // view costs sixteen bytes a key for the length of the encoding and is only built when
        // there is enough work to split.
        let span = (block * nb.div_ceil(threads)).max(1);
        let parts: Vec<Part> = if threads == 1 {
            keys.chunks(span)
                .map(|range| encode_range(range, block, &encoder))
                .collect()
        } else {
            let view: Vec<&str> = keys.iter().map(AsRef::as_ref).collect();
            std::thread::scope(|scope| {
                let running: Vec<_> = view
                    .chunks(span)
                    .map(|range| scope.spawn(|| encode_range(range, block, &encoder)))
                    .collect();
                running
                    .into_iter()
                    .map(|h| h.join().expect("encoding a range of blocks cannot panic"))
                    .collect()
            })
        };

        let mut heads = Vec::with_capacity(parts.iter().map(|p| p.heads.len()).sum());
        let mut head_ends = Vec::with_capacity(nb * 4);
        let mut samples = Vec::with_capacity(nb);
        let mut blocks = Vec::with_capacity(nb * 8);
        let mut data = Vec::with_capacity(parts.iter().map(|p| p.data.len()).sum());
        // Each part is dropped as it is appended, so the two copies never coexist whole.
        for part in parts {
            let (at_head, at_data) = (heads.len() as u64, data.len() as u64);
            for end in &part.head_ends {
                let end = u32::try_from(at_head + end).map_err(|_| {
                    IndexError::Format("dict: the block heads exceed 4 GiB; use a larger block")
                })?;
                head_ends.extend_from_slice(&end.to_le_bytes());
            }
            for start in &part.blocks {
                blocks.extend_from_slice(&(at_data + start).to_le_bytes());
            }
            heads.extend_from_slice(&part.heads);
            samples.extend_from_slice(&part.samples);
            data.extend_from_slice(&part.data);
        }
        Ok(Self {
            block,
            n,
            heads: SharedBytes::from_owned(heads),
            head_ends: SharedBytes::from_owned(head_ends),
            samples,
            blocks: SharedBytes::from_owned(blocks),
            data: SharedBytes::from_owned(data),
            table,
        })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Keys per block, as given at build time.
    pub fn block(&self) -> usize {
        self.block
    }

    /// Number of blocks.
    #[inline(always)]
    fn blocks_len(&self) -> usize {
        self.samples.len()
    }

    #[inline(always)]
    fn head_end(&self, b: usize) -> usize {
        u32_at(&self.head_ends, b)
    }

    /// Where block `b`'s entries start, as stored — past `data` on a blob nothing walked.
    #[inline(always)]
    fn block_start(&self, b: usize) -> u64 {
        u64_at(&self.blocks, b)
    }

    #[inline(always)]
    fn head(&self, b: usize) -> &[u8] {
        head_of(&self.heads, &self.head_ends, b)
    }

    /// Block `b`'s entries, bounded the way [`head`](Self::head) is.
    #[inline(always)]
    fn block_data(&self, b: usize) -> &[u8] {
        let (data, blocks): (&[u8], &[u8]) = (&self.data, &self.blocks);
        let at = |b: usize| usize::try_from(u64_at(blocks, b)).unwrap_or(usize::MAX);
        let end = if b + 1 < self.blocks_len() {
            at(b + 1)
        } else {
            data.len()
        };
        data.get(at(b)..end).unwrap_or_default()
    }

    /// How many leading bytes an entry's stored suffix shares with `rest`, and how the suffix
    /// orders against it — read off the codes, nothing decoded. A stream this crate did not write
    /// ends the suffix where it stops making sense.
    #[inline]
    fn compare_piece(&self, packed: &[u8], rest: &[u8]) -> (usize, Ordering) {
        let mut c = 0;
        let mut i = 0;
        while i < packed.len() {
            let (word, len) = if packed[i] == ESCAPE {
                let Some(&b) = packed.get(i + 1) else {
                    break;
                };
                i += 2;
                (u64::from(b), 1)
            } else {
                let Some(sym) = self.table.symbol(packed[i]) else {
                    break;
                };
                i += 1;
                sym
            };
            let m = len.min(rest.len() - c);
            let theirs = fsst::word_at(rest, c);
            let x = (word ^ theirs) & fsst::low_mask(m);
            if x != 0 {
                let d = (x.trailing_zeros() / 8) as usize;
                let (a, b) = ((word >> (8 * d)) as u8, (theirs >> (8 * d)) as u8);
                return (c + d, a.cmp(&b));
            }
            c += m;
            if m < len {
                return (c, Ordering::Greater);
            }
        }
        let ord = if c == rest.len() {
            Ordering::Equal
        } else {
            Ordering::Less
        };
        (c, ord)
    }

    /// The rank of the first key not below `probe`, and whether that key is `probe`.
    fn locate(&self, probe: &[u8]) -> (u64, bool) {
        if self.n == 0 {
            return (0, false);
        }
        // The blocks whose heads share the probe's first eight bytes, and the one before them:
        // the probe can only be in one of these.
        let s = sample_of(probe);
        let (heads, ends): (&[u8], &[u8]) = (&self.heads, &self.head_ends);
        let lo = self.samples.partition_point(|&x| x < s);
        let hi = self.samples.partition_point(|&x| x <= s);
        // The last block in [from, hi) whose head is not past the probe.
        let (mut l, mut r) = (lo.saturating_sub(1), hi);
        while l < r {
            let m = l + (r - l) / 2;
            if head_of(heads, ends, m) <= probe {
                l = m + 1;
            } else {
                r = m;
            }
        }
        if l == 0 {
            return (0, false);
        }
        let b = l - 1;
        let base = (b * self.block) as u64;
        let head = head_of(heads, ends, b);
        if head == probe {
            return (base, true);
        }
        let count = (self.n - b * self.block).min(self.block);
        // The probe is above the previous entry and shares `matched` bytes with it.
        let mut matched = lcp(head, probe);
        let mut at = self.block_data(b);
        for j in 1..count as u64 {
            let Some((l, len, rest)) = get_header(at) else {
                return (base + j, false);
            };
            let (piece, tail) = rest.split_at(len.min(rest.len()));
            at = tail;
            // This entry differs from the previous one at `l`. Below `matched` the probe agreed
            // with the previous entry, so a shorter shared prefix puts this entry past the probe;
            // a longer one keeps it below, with nothing new matched.
            if l < matched {
                return (base + j, false);
            }
            if l > matched {
                continue;
            }
            let (c, ord) = self.compare_piece(piece, &probe[matched..]);
            match ord {
                Ordering::Equal => return (base + j, true),
                Ordering::Greater => return (base + j, false),
                Ordering::Less => matched += c,
            }
        }
        (base + count as u64, false)
    }

    /// Rank of `key` if it is a member.
    pub fn id(&self, key: &str) -> Option<u64> {
        self.id_bytes(key.as_bytes())
    }

    pub(crate) fn id_bytes(&self, key: &[u8]) -> Option<u64> {
        match self.locate(key) {
            (rank, true) => Some(rank),
            _ => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.locate(key.as_bytes()).1
    }

    /// The rank of the first key not below `key`: `key`'s own id if it is a member, otherwise
    /// the id it would have, `len()` past every key. Two of these bound a range of keys as a
    /// range of ids.
    pub fn lower_bound(&self, key: &str) -> u64 {
        self.locate(key.as_bytes()).0
    }

    /// The smallest `(key, id)` with `key >= query` (the *successor*), or `None` if every key is
    /// smaller.
    pub fn successor(&self, query: &str) -> Option<(String, u64)> {
        let rank = self.locate(query.as_bytes()).0;
        self.key(rank).map(|k| (k, rank))
    }

    /// The largest `(key, id)` with `key <= query` (the *predecessor*), or `None` if every key is
    /// larger. A present `query` is its own predecessor; otherwise the answer sits one rank below
    /// the first key above it, ids being the sorted rank.
    pub fn predecessor(&self, query: &str) -> Option<(String, u64)> {
        let (rank, found) = self.locate(query.as_bytes());
        let at = if found { rank } else { rank.checked_sub(1)? };
        self.key(at).map(|k| (k, at))
    }

    /// How many keys satisfy `lo <= key < hi`, without decoding any of them.
    pub fn range_count(&self, lo: &str, hi: &str) -> u64 {
        self.lower_bound(hi).saturating_sub(self.lower_bound(lo))
    }

    /// The **contiguous** id range of the keys starting with `prefix`, half-open.
    ///
    /// Ids are the lexicographic rank and keys sharing a prefix are adjacent in that order, so
    /// every match is an id in one interval — a prefix is a slice of the id space, not a set of
    /// ids to test one at a time. Two order lookups, whatever the number of matches. Empty
    /// (`start == end`) when nothing matches; `0..len` for an empty prefix.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// assert_eq!(idx.prefix_id_range("ap"), 0..2);
    /// assert_eq!(idx.prefix_id_range("z"), 3..3);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn prefix_id_range(&self, prefix: &str) -> std::ops::Range<u64> {
        let start = self.locate(prefix.as_bytes()).0;
        // The exclusive end is the first key that does not carry the prefix: the same bytes with
        // the last one incremented. A trailing `0xff` cannot appear in UTF-8, so the carry loop is
        // unreachable for a `&str` — it is here because the invariant that makes it unreachable
        // belongs to the caller's type, not to this function.
        let mut upper = prefix.as_bytes().to_vec();
        let end = loop {
            match upper.pop() {
                Some(0xff) => continue,
                Some(b) => {
                    upper.push(b + 1);
                    break self.locate(&upper).0;
                }
                None => break self.n as u64,
            }
        };
        start..end.max(start)
    }

    /// How many keys start with `prefix` — [`prefix_id_range`](Self::prefix_id_range)'s width.
    pub fn prefix_count(&self, prefix: &str) -> u64 {
        let r = self.prefix_id_range(prefix);
        r.end - r.start
    }

    /// Batched [`id`](Self::id): one answer per key, aligned with `keys`.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u64>> {
        self.ids_of_with(keys.len(), |i| keys[i].as_ref().as_bytes())
    }

    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        (0..n).map(|i| self.id_bytes(key(i))).collect()
    }

    /// Decode the next entry of a block onto `cur`, which holds the previous one, moving `at`
    /// past it; `false` on data this crate did not write.
    #[inline]
    fn advance(&self, at: &mut &[u8], cur: &mut Vec<u8>) -> bool {
        let Some((l, len, rest)) = get_header(at) else {
            return false;
        };
        if l > cur.len() || len > rest.len() {
            return false;
        }
        cur.truncate(l);
        let (piece, tail) = rest.split_at(len);
        *at = tail;
        self.table.decode_into(piece, cur)
    }

    /// The block walk that decodes every entry. Correct at any staircase depth, and what
    /// [`key_bytes_into`](Self::key_bytes_into) falls back to when one does not fit.
    fn walk_to(&self, b: usize, steps: usize, out: &mut Vec<u8>) -> bool {
        out.clear();
        out.extend_from_slice(self.head(b));
        let mut at = self.block_data(b);
        (0..steps).all(|_| self.advance(&mut at, out))
    }

    /// The key at rank `id` into `out`, cleared first; `false`, with `out` empty, past the last
    /// key.
    ///
    /// An entry stores what it shares with its predecessor, so an entry whose `lcp` is at least a
    /// later entry's contributes nothing that survives to the key being asked for. The entries that
    /// do contribute form a strictly increasing staircase of `lcp`, and a monotonic stack over the
    /// headers finds it in the one pass the walk already makes. Every header is still read — a
    /// header is what says where the next one begins — but a handful of suffixes are decoded rather
    /// than one per entry, and the decode is the expensive half: 207 → 146 ns at the default block
    /// on the dictionary, 751 → 454 at 128 per block, 464 → 265 on a path list.
    fn key_bytes_into(&self, id: u64, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        if id >= self.n {
            return false;
        }
        let b = id / self.block;
        let steps = id % self.block;
        let data = self.block_data(b);
        let mut at = data;
        let mut stair = [(0usize, &data[..0]); STAIRS];
        let mut depth = 0usize;
        for _ in 0..steps {
            let Some((l, len, rest)) = get_header(at) else {
                return false;
            };
            if len > rest.len() {
                return false;
            }
            let (piece, tail) = rest.split_at(len);
            at = tail;
            while depth > 0 && stair[depth - 1].0 >= l {
                depth -= 1;
            }
            if depth == STAIRS {
                return self.walk_to(b, steps, out);
            }
            stair[depth] = (l, piece);
            depth += 1;
        }
        out.extend_from_slice(self.head(b));
        for &(l, piece) in &stair[..depth] {
            if l > out.len() {
                return false;
            }
            out.truncate(l);
            if !self.table.decode_into(piece, out) {
                return false;
            }
        }
        true
    }

    /// The key at rank `id`; `None` at or past `len()`.
    pub fn key(&self, id: u64) -> Option<String> {
        let mut out = String::new();
        self.key_into(id, &mut out).then_some(out)
    }

    /// [`key`](Self::key) into a string the caller keeps, so a loop over ids allocates nothing:
    /// `out` is cleared and, when the answer is `true`, holds the key. `false` at or past
    /// `len()`, and for the key a corrupted blob decodes to something that is not UTF-8.
    pub fn key_into(&self, id: u64, out: &mut String) -> bool {
        let mut buf = std::mem::take(out).into_bytes();
        let found = self.key_bytes_into(id, &mut buf);
        match String::from_utf8(buf) {
            Ok(s) => {
                *out = s;
                found
            }
            Err(_) => false,
        }
    }

    /// Every key with its id, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (String, u64)> + '_ {
        self.iter_from(0)
    }

    /// All `(key, id)` pairs whose key starts with `prefix`, in lexicographic order.
    ///
    /// A sorted dictionary needs no automaton for this: the matches are the contiguous run that
    /// [`prefix_id_range`](Self::prefix_id_range) names, so the cost is one order lookup plus one
    /// decode per key returned.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// assert_eq!(idx.prefix("ap"), [("apple".to_string(), 0), ("apricot".to_string(), 1)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn prefix(&self, prefix: &str) -> Vec<(String, u64)> {
        self.prefix_iter(prefix).collect()
    }

    /// Like [`prefix`](Self::prefix) but **lazy**: one key is decoded per step, so an autocomplete
    /// wanting the first handful never pays for the rest.
    pub fn prefix_iter<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (String, u64)> + 'a {
        self.iter_from(self.lower_bound(prefix))
            .take_while(move |(k, _)| k.starts_with(prefix))
    }

    /// Every key that is a **prefix of `query`**, shortest first, with its id — the reverse of
    /// [`prefix`](Self::prefix), which returns the keys `query` is a prefix of.
    ///
    /// This is the dictionary-matching query: given a vocabulary and a position in a sentence, it
    /// returns the entries that start there, and [`longest_prefix`](Self::longest_prefix) picks the
    /// one a longest-match tokeniser takes. The empty key, if the index holds it, is a prefix of
    /// everything and comes first.
    ///
    /// One order lookup per character boundary of `query`. A trie answers this in a single walk
    /// down the query; a sorted array has no such walk, and this is what that costs.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["a", "ap", "apple", "b"])?;
    /// assert_eq!(idx.common_prefix("apples"), [("a".to_string(), 0), ("ap".to_string(), 1), ("apple".to_string(), 2)]);
    /// assert_eq!(idx.longest_prefix("apples"), Some(("apple".to_string(), 2)));
    /// assert_eq!(idx.longest_prefix("zebra"), None);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn common_prefix(&self, query: &str) -> Vec<(String, u64)> {
        let mut found = Vec::new();
        for end in 0..=query.len() {
            if !query.is_char_boundary(end) {
                continue;
            }
            if let Some(id) = self.id(&query[..end]) {
                found.push((query[..end].to_owned(), id));
            }
        }
        found
    }

    /// The longest key that is a prefix of `query`, or `None` if no key is — the match a
    /// longest-match tokeniser takes. See [`common_prefix`](Self::common_prefix).
    ///
    /// Walks down from `query` itself and stops at the first hit, so a long match is cheap and
    /// only a query that matches nothing pays for every boundary.
    pub fn longest_prefix(&self, query: &str) -> Option<(String, u64)> {
        for end in (0..=query.len()).rev() {
            if !query.is_char_boundary(end) {
                continue;
            }
            if let Some(id) = self.id(&query[..end]) {
                return Some((query[..end].to_owned(), id));
            }
        }
        None
    }

    /// All `(key, id)` pairs with `lo <= key < hi`, in lexicographic order.
    pub fn range(&self, lo: &str, hi: &str) -> Vec<(String, u64)> {
        self.range_iter(lo, hi).collect()
    }

    /// Like [`range`](Self::range) but **lazy** — see [`prefix_iter`](Self::prefix_iter).
    pub fn range_iter<'a>(
        &'a self,
        lo: &'a str,
        hi: &'a str,
    ) -> impl Iterator<Item = (String, u64)> + 'a {
        self.iter_from(self.lower_bound(lo))
            .take_while(move |(k, _)| k.as_str() < hi)
    }

    /// [`iter`](Self::iter) resumed after `after`, which is excluded whether or not it is a key —
    /// what a cursor wants.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// let rest: Vec<_> = idx.iter_after("apricot").collect();
    /// assert_eq!(rest, [("banana".to_string(), 2)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn iter_after(&self, after: &str) -> impl Iterator<Item = (String, u64)> + '_ {
        let (rank, found) = self.locate(after.as_bytes());
        self.iter_from(rank + u64::from(found))
    }

    /// [`iter`](Self::iter) from rank `start` on.
    pub(crate) fn iter_from(&self, start: u64) -> impl Iterator<Item = (String, u64)> + '_ {
        let mut id = usize::try_from(start).unwrap_or(usize::MAX);
        let mut cur: Vec<u8> = Vec::new();
        let mut at: &[u8] = &[];
        let mut primed = false;
        std::iter::from_fn(move || {
            if id >= self.n {
                return None;
            }
            let (b, j) = (id / self.block, id % self.block);
            let ok = if !primed || j == 0 {
                cur.clear();
                cur.extend_from_slice(self.head(b));
                at = self.block_data(b);
                let skip = if primed { 0 } else { j };
                primed = true;
                (0..skip).all(|_| self.advance(&mut at, &mut cur))
            } else {
                self.advance(&mut at, &mut cur)
            };
            if !ok {
                id = self.n; // a stream this crate did not write ends the walk
                return None;
            }
            let this = id as u64;
            id += 1;
            Some((String::from_utf8_lossy(&cur).into_owned(), this))
        })
    }

    /// The payload sections in order, each handed to `f` once, from where they are; the symbol
    /// table and the samples, the two the index does not hold as their serialised bytes, go out
    /// in pieces so that nothing the size of the index is copied.
    fn sections(
        &self,
        mut f: impl FnMut(&[u8]) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        let mut table = Vec::with_capacity(self.table.serialized_len());
        self.table.write_to(&mut table);
        f(&table)?;
        for section in [&self.heads, &self.head_ends] {
            f(section)?;
        }
        let mut buf = Vec::with_capacity(8 * CHUNK);
        for chunk in self.samples.chunks(CHUNK) {
            buf.clear();
            chunk
                .iter()
                .for_each(|w| buf.extend_from_slice(&w.to_le_bytes()));
            f(&buf)?;
        }
        for section in [&self.blocks, &self.data] {
            f(section)?;
        }
        Ok(())
    }

    fn header(&self) -> [u8; HEADER] {
        let mut hasher = crate::blob::BlockHasher::new();
        self.sections(|s| {
            hasher.update(s);
            Ok(())
        })
        .expect("hashing the sections cannot fail");
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(MAGIC);
        h[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        h[12..16].copy_from_slice(&(self.block as u32).to_le_bytes());
        h[16..24].copy_from_slice(&(self.heads.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&(self.data.len() as u64).to_le_bytes());
        h[32..36].copy_from_slice(&(self.table.serialized_len() as u32).to_le_bytes());
        h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
        let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        h
    }

    /// Serialise to `[magic "BDX1"][n][block][head bytes][data bytes][table bytes][payload]
    /// [check]`, then the symbol table, the head keys, the three per-block arrays and the block
    /// data. `check` is a hash of the preceding header bytes and `payload` a hash of everything
    /// after it, both verified on load.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_len());
        out.extend_from_slice(&self.header());
        self.sections(|s| {
            out.extend_from_slice(s);
            Ok(())
        })
        .expect("appending the sections cannot fail");
        out
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it.
    pub fn serialized_len(&self) -> usize {
        HEADER
            + self.table.serialized_len()
            + self.heads.len()
            + self.blocks_len() * PER_BLOCK
            + self.data.len()
    }

    /// Reconstruct from [`DictIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes: the magic, both checksums, every section length, the symbol
    /// table and the three per-block arrays are checked before anything is trusted, so a
    /// crafted blob is at worst *wrong* — a key that is not the one built, a shorter walk —
    /// never out of bounds. The block data itself is read with every access bounded.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true)
    }

    /// The loader behind every way in. The framing — magic, header checksum, block size, the
    /// section lengths against the blob — is checked always and the symbol table parsed; with
    /// `verify`, the payload checksum and [`check_layout`](Self::check_layout) as well, which
    /// read every section. Without it the sections are borrowed as they are and the accessors
    /// bound what the arrays say, so a mapping loads without touching its pages.
    fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let bytes: &[u8] = &blob;
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED..HEADER].try_into().unwrap());
        if check != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        if verify {
            let stored = u64::from_le_bytes(bytes[36..44].try_into().unwrap());
            if stored != crate::blob::hash_block(&bytes[HEADER..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
        }
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let n = usize::try_from(u64_at(4))
            .map_err(|_| IndexError::Format("dict: key count out of range"))?;
        let block = u32_at(12) as usize;
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block size out of range"));
        }
        let heads_len = usize::try_from(u64_at(16))
            .map_err(|_| IndexError::Format("dict: head bytes out of range"))?;
        let data_len = usize::try_from(u64_at(24))
            .map_err(|_| IndexError::Format("dict: block data out of range"))?;
        let table_len = u32_at(32) as usize;
        let nb = n.div_ceil(block);
        let arrays = nb
            .checked_mul(PER_BLOCK)
            .ok_or(IndexError::Format("dict: block count out of range"))?;
        let total = HEADER
            .checked_add(table_len)
            .and_then(|t| t.checked_add(heads_len))
            .and_then(|t| t.checked_add(arrays))
            .and_then(|t| t.checked_add(data_len));
        if total != Some(bytes.len()) {
            return Err(IndexError::Format(
                "dict: the section lengths do not add up to the blob",
            ));
        }
        let mut at = HEADER;
        let mut take = |len: usize| {
            at += len;
            blob.subslice(at - len, at)
                .expect("the sections add up to the blob")
        };
        let table = Table::from_bytes(&take(table_len))
            .ok_or(IndexError::Format("dict: bad symbol table"))?;
        let heads = take(heads_len);
        let head_ends = take(nb * 4);
        let samples = take(nb * 8)
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect();
        let blocks = take(nb * 8);
        let data = take(data_len);
        let idx = Self {
            block,
            n,
            heads,
            head_ends,
            samples,
            blocks,
            data,
            table,
        };
        if verify {
            idx.check_layout()?;
        }
        Ok(idx)
    }

    /// The array invariants every query relies on: heads and blocks in order and inside their
    /// sections, every sample the one its head gives.
    fn check_layout(&self) -> Result<(), IndexError> {
        let nb = self.blocks_len();
        if nb == 0 {
            return if self.heads.is_empty() && self.data.is_empty() {
                Ok(())
            } else {
                Err(IndexError::Format("dict: an empty index with key bytes"))
            };
        }
        let mut prev = 0;
        for end in (0..nb).map(|b| self.head_end(b)) {
            if end < prev || end > self.heads.len() {
                return Err(IndexError::Format("dict: head table out of order"));
            }
            prev = end;
        }
        if prev != self.heads.len() {
            return Err(IndexError::Format(
                "dict: head table does not cover the head bytes",
            ));
        }
        let mut prev = 0;
        for start in (0..nb).map(|b| self.block_start(b)) {
            if start < prev || start > self.data.len() as u64 {
                return Err(IndexError::Format("dict: block table out of order"));
            }
            prev = start;
        }
        if self.block_start(0) != 0 {
            return Err(IndexError::Format("dict: block table out of order"));
        }
        if (0..nb).any(|b| self.samples[b] != sample_of(self.head(b))) {
            return Err(IndexError::Format(
                "dict: head samples do not match the heads",
            ));
        }
        Ok(())
    }

    /// The queries the fuzz shim puts to every blob. Short, so the pairwise range probes stay
    /// cheap, and spread over the byte order so a prefix range is sometimes empty, sometimes the
    /// whole index, and sometimes neither.
    #[cfg(feature = "fuzzing")]
    const FUZZ_PROBES: [&str; 6] = ["", "\u{0}", "a", "ab", "zzzzzzzzzzzzzzzzz", "\u{10FFFF}"];

    /// Whether `bytes` loads, and whether what loaded answers without panicking — by the checked
    /// path and by the mapping's, which takes the arrays as they are. Exists for the libFuzzer
    /// target in `fuzz/`; see the `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_load_and_query(bytes: &[u8]) -> bool {
        let checked = Self::from_bytes(bytes).ok();
        let framed = Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), false).ok();
        assert!(
            checked.is_none() || framed.is_some(),
            "the framing is the checked path's"
        );
        for idx in checked.iter().chain(&framed) {
            let n = idx.len() as u64;
            for probe in Self::FUZZ_PROBES {
                assert!(idx.id(probe).is_none_or(|id| id < n), "{probe:?}");
                assert!(idx.lower_bound(probe) <= n, "{probe:?}");
                let r = idx.prefix_id_range(probe);
                assert!(
                    r.start <= r.end && r.end <= n,
                    "prefix_id_range({probe:?}) = {r:?} over {n} keys"
                );
                assert_eq!(idx.prefix_count(probe), r.end - r.start, "{probe:?}");
                assert!(
                    idx.successor(probe).is_none_or(|(_, id)| id < n),
                    "successor({probe:?}) past {n}"
                );
                assert!(
                    idx.predecessor(probe).is_none_or(|(_, id)| id < n),
                    "predecessor({probe:?}) past {n}"
                );
                for hi in Self::FUZZ_PROBES {
                    assert!(
                        idx.range_count(probe, hi) <= n,
                        "range_count({probe:?}, {hi:?})"
                    );
                }
                assert!(
                    idx.prefix_iter(probe).take(64).all(|(_, id)| id < n),
                    "prefix({probe:?}) past {n}"
                );
                assert!(
                    idx.range_iter(probe, "zzzz").take(64).all(|(_, id)| id < n),
                    "range({probe:?}, ..) past {n}"
                );
                assert!(
                    idx.iter_after(probe).take(64).all(|(_, id)| id < n),
                    "iter_after({probe:?}) past {n}"
                );
            }
            for id in [0, 1, n / 2, n.saturating_sub(1), n, u64::MAX] {
                assert!(id < n || idx.key(id).is_none(), "key({id}) past {n}");
            }
            assert!(idx.iter().take(64).count() as u64 <= n);
        }
        // On a blob that passed every check, the ordered surface must agree with a walk -- the
        // property the unit tests assert on indexes this crate built, here over one a fuzzer did.
        // Two guards keep it from reporting a difference that is not a bug: an index longer than
        // the walk is left alone, and so is one whose block data decodes to something that is not
        // UTF-8, since `iter` is lossy there and `key` refuses, which is a documented difference
        // rather than a disagreement.
        if let Some(idx) = &checked {
            let all: Vec<(String, u64)> = idx.iter().take(257).collect();
            for (i, (_, id)) in all.iter().enumerate() {
                assert_eq!(*id, i as u64, "iter is not in rank order");
            }
            let whole = all.len() == idx.len() && all.len() <= 256;
            let utf8 = all
                .iter()
                .all(|(k, id)| idx.key(*id).as_deref() == Some(k.as_str()));
            if whole && utf8 {
                for lo in Self::FUZZ_PROBES {
                    let want: Vec<(String, u64)> = all
                        .iter()
                        .filter(|(k, _)| k.starts_with(lo))
                        .cloned()
                        .collect();
                    assert_eq!(idx.prefix(lo), want, "prefix({lo:?})");
                    assert_eq!(
                        idx.prefix_count(lo) as usize,
                        want.len(),
                        "prefix_count({lo:?})"
                    );
                    assert_eq!(
                        idx.successor(lo),
                        all.iter().find(|(k, _)| k.as_str() >= lo).cloned(),
                        "successor({lo:?})"
                    );
                    assert_eq!(
                        idx.predecessor(lo),
                        all.iter().rev().find(|(k, _)| k.as_str() <= lo).cloned(),
                        "predecessor({lo:?})"
                    );
                    assert_eq!(
                        idx.iter_after(lo).collect::<Vec<_>>(),
                        all.iter()
                            .filter(|(k, _)| k.as_str() > lo)
                            .cloned()
                            .collect::<Vec<_>>(),
                        "iter_after({lo:?})"
                    );
                    for hi in Self::FUZZ_PROBES {
                        let want: Vec<(String, u64)> = all
                            .iter()
                            .filter(|(k, _)| k.as_str() >= lo && k.as_str() < hi)
                            .cloned()
                            .collect();
                        assert_eq!(idx.range(lo, hi), want, "range({lo:?}, {hi:?})");
                        assert_eq!(
                            idx.range_count(lo, hi) as usize,
                            want.len(),
                            "range_count({lo:?}, {hi:?})"
                        );
                    }
                }
            }
        }
        checked.is_some()
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        w.write_all(&self.header())?;
        self.sections(|s| Ok(w.write_all(s)?))
    }

    /// Load an index previously written with [`DictIndex::save`]. Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and borrow it: the heads, the block data and the two offset arrays
    /// are read where they lie, and the load touches the header, the symbol table and the
    /// per-block samples — eight bytes a block, a byte per four keys at the default block, read
    /// into memory because the search over them opens every lookup (see the field). Skips the
    /// payload checksum and the walk over the arrays [`load`](Self::load) makes — the mapped file
    /// is trusted intact, and every access bounds what the arrays say.
    ///
    /// # Safety
    /// One obligation, and it is not about the bytes: the file must not be modified or truncated
    /// by any process while the returned index is alive, because the index borrows the mapping.
    /// A crafted file is *not* undefined behaviour here — the framing is checked, and the rest
    /// is read with every access bounded — it is merely wrong. See
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }

    /// [`load_mmap`](Self::load_mmap) plus the checks [`load`](Self::load) makes — the payload
    /// checksum and the walk over the per-block arrays — one pass over the mapping at load,
    /// pages still shared and nothing copied. For a file you wrote but did not carry yourself.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap): the file must not change while the
    /// index is alive. The checks run once, at load, and say nothing about later.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_verified(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), true)
    }
}

impl std::fmt::Debug for DictIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictIndex")
            .field("len", &self.n)
            .field("block", &self.block)
            .field("bytes", &self.serialized_len())
            .finish_non_exhaustive()
    }
}

/// A sorted, distinct key stream [`DictIndex::build_to_file`] can walk more than once — either the
/// single in-memory run the corpus fitted in, or a merge of the runs it spilled.
trait Replay {
    fn each(&mut self, f: &mut dyn FnMut(&str) -> Result<(), IndexError>)
    -> Result<(), IndexError>;
}

impl Replay for &mut Run {
    fn each(
        &mut self,
        f: &mut dyn FnMut(&str) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        for key in self.sorted() {
            f(key)?;
        }
        Ok(())
    }
}

impl Replay for &Runs {
    fn each(
        &mut self,
        f: &mut dyn FnMut(&str) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        // The merge interleaves the runs but deduplicates only within one, so equal keys arrive
        // adjacent; dropping them here is what makes this stream the one `build` sorts to.
        let failed = std::cell::RefCell::new(None);
        let mut prev = String::new();
        let mut seen = false;
        for key in self.merge(&failed)? {
            if seen && prev == key {
                continue;
            }
            f(&key)?;
            prev.clear();
            prev.push_str(&key);
            seen = true;
        }
        if let Some(e) = failed.borrow_mut().take() {
            return Err(IndexError::Io(e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden keys plus the shapes the encoding has cases for: the empty key, NUL bytes,
    /// multibyte characters, a prefix of another key, a shared prefix and a suffix past the
    /// one-byte header's fifteen, and a key past eight bytes that shares its sample with others.
    fn corpus() -> Vec<String> {
        let mut keys: Vec<String> = include_str!("../tests/data/golden-keys.txt")
            .lines()
            .map(str::to_owned)
            .collect();
        let long = "x".repeat(20);
        keys.extend(
            [
                "",
                "a",
                "a\0",
                "a\0b",
                "é",
                "école",
                "écoles",
                "ka",
                "ka-",
                "ka-00",
                "ka-0000-and-then-some-more-bytes-than-fifteen",
                "ka-0000-and-then-some-more-bytes-than-sixteen",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        keys.push(long.clone());
        keys.push(format!("{long}a"));
        keys.push(format!("{long}b{}", "y".repeat(40)));
        keys.push(format!("{long}b{}z", "y".repeat(40)));
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn probes(keys: &[String]) -> Vec<String> {
        let mut out = vec![
            String::new(),
            "\u{10FFFF}".into(),
            "kb".into(),
            "ka-00000".into(),
        ];
        for k in keys.iter().step_by(7) {
            out.push(format!("{k}x"));
            out.push(k[..k.len() - k.chars().last().map_or(0, char::len_utf8)].to_owned());
            out.push(format!("{k}\0"));
        }
        out
    }

    fn check(idx: &DictIndex, keys: &[String]) {
        assert_eq!(idx.len(), keys.len());
        let mut buf = String::from("scratch");
        for (rank, k) in keys.iter().enumerate() {
            let rank = rank as u64;
            assert_eq!(idx.id(k), Some(rank), "{k:?}");
            assert!(idx.contains(k), "{k:?}");
            assert_eq!(idx.lower_bound(k), rank, "{k:?}");
            assert_eq!(idx.key(rank).as_deref(), Some(k.as_str()), "{rank}");
            assert!(idx.key_into(rank, &mut buf), "{rank}");
            assert_eq!(&buf, k, "{rank}");
        }
        for p in probes(keys) {
            let expect = keys.partition_point(|k| k.as_bytes() < p.as_bytes()) as u64;
            let member = keys.get(expect as usize).is_some_and(|k| *k == p);
            assert_eq!(idx.id(&p), member.then_some(expect), "{p:?}");
            assert_eq!(idx.contains(&p), member, "{p:?}");
            assert_eq!(idx.lower_bound(&p), expect, "{p:?}");
        }
        let n = keys.len() as u64;
        assert_eq!(idx.key(n), None);
        assert_eq!(idx.key(u64::MAX), None);
        assert!(!idx.key_into(n, &mut buf) && buf.is_empty());
        let walked: Vec<(String, u64)> = idx.iter().collect();
        let expect: Vec<(String, u64)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u64))
            .collect();
        assert_eq!(walked, expect);
        assert_eq!(idx.ids_of(&keys[..5]), (0..5).map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn answers_every_key_and_stranger_at_every_block_size() {
        let keys = corpus();
        for block in [1, 2, 3, 7, 32, 500, 1024] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            assert_eq!(idx.block(), block);
            check(&idx, &keys);
            let blob = idx.to_bytes();
            assert_eq!(blob.len(), idx.serialized_len(), "block {block}");
            assert_eq!(&blob[..4], b"BDX1");
            let back = DictIndex::from_bytes(&blob).unwrap();
            assert_eq!(back.to_bytes(), blob, "block {block}");
            check(&back, &keys);
        }
    }

    #[test]
    fn the_ordered_queries_agree_with_a_linear_scan() {
        let keys = corpus();
        let pairs: Vec<(String, u64)> = keys.iter().cloned().zip(0..).collect();
        let ps = probes(&keys);
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            for p in &ps {
                let want: Vec<(String, u64)> = pairs
                    .iter()
                    .filter(|(k, _)| k.starts_with(p))
                    .cloned()
                    .collect();
                assert_eq!(&idx.prefix(p), &want, "block {block} prefix {p:?}");
                assert_eq!(
                    idx.prefix_iter(p).take(2).collect::<Vec<_>>(),
                    want.iter().take(2).cloned().collect::<Vec<_>>(),
                    "block {block} prefix_iter {p:?}"
                );
                let r = idx.prefix_id_range(p);
                assert_eq!(
                    r.end - r.start,
                    want.len() as u64,
                    "block {block} range {p:?}"
                );
                assert_eq!(
                    idx.prefix_count(p),
                    want.len() as u64,
                    "block {block} count {p:?}"
                );
                if let Some((_, first)) = want.first() {
                    assert_eq!(r.start, *first, "block {block} range start {p:?}");
                }

                let want = pairs
                    .iter()
                    .find(|(k, _)| k.as_str() >= p.as_str())
                    .cloned();
                assert_eq!(idx.successor(p), want, "block {block} successor {p:?}");
                let want = pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| k.as_str() <= p.as_str())
                    .cloned();
                assert_eq!(idx.predecessor(p), want, "block {block} predecessor {p:?}");

                let want: Vec<(String, u64)> = pairs
                    .iter()
                    .filter(|(k, _)| k.as_str() > p.as_str())
                    .cloned()
                    .collect();
                assert_eq!(
                    idx.iter_after(p).collect::<Vec<_>>(),
                    want,
                    "block {block} after {p:?}"
                );
            }
            for w in ps.chunks(2).filter(|w| w.len() == 2) {
                let (lo, hi) = (&w[0], &w[1]);
                let want: Vec<(String, u64)> = pairs
                    .iter()
                    .filter(|(k, _)| k.as_str() >= lo.as_str() && k.as_str() < hi.as_str())
                    .cloned()
                    .collect();
                assert_eq!(
                    &idx.range(lo, hi),
                    &want,
                    "block {block} range {lo:?}..{hi:?}"
                );
                assert_eq!(
                    idx.range_count(lo, hi),
                    want.len() as u64,
                    "block {block} count"
                );
            }
        }
    }

    #[test]
    fn a_staircase_deeper_than_the_stack_still_answers() {
        // Every key extends the one before it by a byte, so every entry adds a level and the
        // staircase is as deep as the rank inside the block. Past `STAIRS` the walk stops
        // tracking it and decodes each entry instead, which is the path this pins.
        let keys: Vec<String> = (1..=2 * STAIRS).map(|n| "a".repeat(n)).collect();
        for block in [STAIRS - 1, STAIRS + 1, MAX_BLOCK] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            check(&idx, &keys);
            check(&DictIndex::from_bytes(&idx.to_bytes()).unwrap(), &keys);
        }
    }

    #[test]
    fn iter_from_resumes_inside_a_block() {
        let keys = corpus();
        let idx = DictIndex::build_with_block(&keys, 5).unwrap();
        for start in [
            0u64,
            1,
            4,
            5,
            6,
            17,
            keys.len() as u64 - 1,
            keys.len() as u64,
            u64::MAX,
        ] {
            let got: Vec<u64> = idx.iter_from(start).map(|(_, id)| id).collect();
            let expect: Vec<u64> = (start.min(keys.len() as u64)..keys.len() as u64).collect();
            assert_eq!(got, expect, "from {start}");
            assert!(
                idx.iter_from(start).all(|(k, id)| Some(k) == idx.key(id)),
                "from {start}"
            );
        }
    }

    #[test]
    fn duplicates_and_input_order_do_not_matter() {
        let a = DictIndex::build(["pear", "apple", "fig", "apple"]).unwrap();
        let b = DictIndex::build(["fig", "pear", "apple"]).unwrap();
        assert_eq!(a.to_bytes(), b.to_bytes());
        assert_eq!(a.len(), 3);
        assert_eq!(a.id("apple"), Some(0));
        assert_eq!(a.id("pear"), Some(2));
        assert_eq!(a.lower_bound("banana"), 1);
        assert_eq!(a.lower_bound("zebra"), 3);
    }

    #[test]
    fn an_empty_index_answers_and_round_trips() {
        let idx = DictIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(
            (idx.id(""), idx.key(0), idx.lower_bound("x")),
            (None, None, 0)
        );
        assert!(!idx.contains(""));
        assert_eq!(idx.iter().count(), 0);
        let blob = idx.to_bytes();
        assert_eq!(blob.len(), idx.serialized_len());
        let back = DictIndex::from_bytes(&blob).unwrap();
        assert!(back.is_empty() && back.to_bytes() == blob);
    }

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
        let keys = corpus();
        // Descending, then ascending: every key twice, its copies far enough apart that a 64-byte
        // run budget puts them in different runs, which is where a merge that failed to
        // deduplicate across runs would show.
        let twice: Vec<&str> = keys
            .iter()
            .rev()
            .chain(keys.iter())
            .map(String::as_str)
            .collect();
        let dir = scratch("dictruns");
        let path = dir.join("idx.bdx");
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let want = DictIndex::build_with_block(&keys, block)
                .unwrap()
                .to_bytes();
            let n = DictIndex::build_to_file_runs(&twice, &path, block, || Ok(()), 64).unwrap();
            assert_eq!(n, keys.len());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                want,
                "block {block}, spilled"
            );
            assert_eq!(entries(&dir), ["idx.bdx"], "the runs directory is gone");
            check(&DictIndex::load(&path).unwrap(), &keys);
            // The same corpus in one run: sorted in memory, no runs directory at all.
            let n = DictIndex::build_to_file_with_block(&twice, &path, block).unwrap();
            assert_eq!(n, keys.len());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                want,
                "block {block}, in memory"
            );
            assert_eq!(entries(&dir), ["idx.bdx"]);
        }
        assert_eq!(DictIndex::build_to_file(&twice, &path).unwrap(), keys.len());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            DictIndex::build(&keys).unwrap().to_bytes()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_to_file_refuses_what_it_cannot_hold_and_publishes_nothing() {
        let dir = scratch("dictruns_err");
        let path = dir.join("idx.bdx");
        assert_eq!(
            DictIndex::build_to_file(Vec::<String>::new(), &path).unwrap(),
            0
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            DictIndex::build(Vec::<&str>::new()).unwrap().to_bytes()
        );
        assert!(DictIndex::load(&path).unwrap().is_empty());

        // A key no run can hold is an error, not an unbounded allocation -- and the empty index
        // written above is still there.
        let long = "x".repeat(65);
        let err =
            DictIndex::build_to_file_runs([long.as_str()], &path, 32, || Ok(()), 64).unwrap_err();
        assert!(err.to_string().contains("run budget"), "{err}");
        assert!(DictIndex::load(&path).unwrap().is_empty());
        for block in [0, MAX_BLOCK + 1] {
            let err = DictIndex::build_to_file_with_block(["a"], &path, block).unwrap_err();
            assert!(err.to_string().contains("block must be"), "{err}");
        }

        // The caller's last word, asked once the input has ended and again inside the write.
        std::fs::write(&path, b"previous").unwrap();
        let keys: Vec<String> = (0..200)
            .map(|i| format!("k{:03}", (i * 7919) % 200))
            .collect();
        let calls = std::cell::Cell::new(0);
        let late = || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(IndexError::Format("late"))
            } else {
                Ok(())
            }
        };
        let err = DictIndex::build_to_file_runs(&keys, &path, 32, late, 64).unwrap_err();
        assert!(matches!(err, IndexError::Format("late")), "{err}");
        assert_eq!(
            calls.get(),
            2,
            "once after the input, once inside the write"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        assert_eq!(entries(&dir), ["idx.bdx"], "the runs directory is gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_block_size_outside_the_range_is_refused() {
        for block in [0, MAX_BLOCK + 1] {
            let err = DictIndex::build_with_block(["a"], block).unwrap_err();
            assert!(err.to_string().contains("block must be"), "{err}");
            let err = DictIndex::build_sorted_with_block(["a"], block).unwrap_err();
            assert!(err.to_string().contains("block must be"), "{err}");
        }
    }

    #[test]
    fn the_prefixes_of_a_query_agree_with_a_linear_scan() {
        let keys = corpus();
        let ps = probes(&keys);
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            for q in &ps {
                // Prefixes of `q` sort by length, so the linear scan in rank order is already
                // shortest first and its last element is the longest match.
                let want: Vec<(String, u64)> = keys
                    .iter()
                    .zip(0u64..)
                    .filter(|(k, _)| q.starts_with(k.as_str()))
                    .map(|(k, id)| (k.clone(), id))
                    .collect();
                assert_eq!(&idx.common_prefix(q), &want, "block {block} query {q:?}");
                assert_eq!(
                    idx.longest_prefix(q),
                    want.last().cloned(),
                    "block {block} query {q:?}"
                );
            }
        }
    }

    #[test]
    fn the_encoding_does_not_depend_on_how_many_threads_ran() {
        let keys = corpus();
        for block in [1usize, 3, 32] {
            let want = DictIndex::from_sorted_on(&keys, block, 1)
                .unwrap()
                .to_bytes();
            for threads in [2usize, 3, 5, 8, 64] {
                let got = DictIndex::from_sorted_on(&keys, block, threads).unwrap();
                assert_eq!(got.to_bytes(), want, "block {block}, {threads} threads");
                check(&got, &keys);
            }
        }
    }

    #[test]
    fn a_sorted_build_is_byte_identical_to_the_sorting_one() {
        let keys = corpus();
        // The same key set scrambled with repeats and in order with adjacent repeats. The blob is
        // the contract, so compare the bytes rather than the answers -- and the scrambled input is
        // descending from its first pair, so the sorted builder must refuse it.
        let mut scrambled: Vec<&str> = keys.iter().map(String::as_str).collect();
        scrambled.reverse();
        scrambled.extend(keys.iter().step_by(3).map(String::as_str));
        let mut repeated: Vec<&str> = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            repeated.push(k);
            if i % 3 == 0 {
                repeated.push(k);
            }
        }
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let want = DictIndex::build_with_block(scrambled.iter().copied(), block).unwrap();
            let got = DictIndex::build_sorted_with_block(repeated.iter().copied(), block).unwrap();
            assert_eq!(got.to_bytes(), want.to_bytes(), "block {block}");
            check(&got, &keys);
            let err =
                DictIndex::build_sorted_with_block(scrambled.iter().copied(), block).unwrap_err();
            assert!(err.to_string().contains("below its predecessor"), "{err}");
        }
        assert_eq!(
            DictIndex::build_sorted(&keys).unwrap().to_bytes(),
            DictIndex::build(&keys).unwrap().to_bytes()
        );
    }

    #[test]
    fn a_single_descent_anywhere_in_a_sorted_build_is_refused() {
        let keys = corpus();
        for at in [0usize, 1, keys.len() / 2, keys.len() - 2] {
            let mut swapped: Vec<&str> = keys.iter().map(String::as_str).collect();
            swapped.swap(at, at + 1);
            let err = DictIndex::build_sorted(swapped).unwrap_err();
            assert!(err.to_string().contains("below its predecessor"), "{err}");
        }
        assert!(
            DictIndex::build_sorted(Vec::<&str>::new())
                .unwrap()
                .is_empty()
        );
        assert_eq!(DictIndex::build_sorted(["a", "a", "a"]).unwrap().len(), 1);
    }

    #[test]
    fn varints_round_trip_and_a_cut_or_oversized_one_is_refused() {
        for v in [
            0usize,
            1,
            127,
            128,
            300,
            1 << 20,
            usize::MAX >> 1,
            usize::MAX,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            out.push(0xAB);
            assert_eq!(get_varint(&out), Some((v, &[0xABu8][..])));
        }
        assert_eq!(get_varint(&[]), None);
        assert_eq!(get_varint(&[0x80]), None);
        assert_eq!(get_varint(&[0x80; 12]), None);
        let mut out = Vec::new();
        put_header(&mut out, 14, 14);
        assert_eq!(out, [0xEE]);
        assert_eq!(get_header(&out), Some((14, 14, &[][..])));
        out.clear();
        put_header(&mut out, 15, 3);
        assert_eq!(out, [0xFF, 15, 3]);
        assert_eq!(get_header(&out), Some((15, 3, &[][..])));
        assert_eq!(get_header(&[]), None);
        assert_eq!(get_header(&[0xFF, 15]), None);
    }

    #[test]
    fn save_and_load() {
        let keys = corpus();
        let idx = DictIndex::build(&keys).unwrap();
        let dir = std::env::temp_dir().join(format!("lexindex-dict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.bdx");
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes());
        let back = DictIndex::load(&path).unwrap();
        check(&back, &keys);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(DictIndex::load(&path).is_err());
    }

    /// Recompute both checksums after a deliberate edit, so the structural checks are reached.
    fn reframe(blob: &mut [u8]) {
        let payload = crate::blob::hash_block(&blob[HEADER..]);
        blob[36..44].copy_from_slice(&payload.to_le_bytes());
        let check = crate::blob::hash_bytes(&blob[..CHECKED]) as u32;
        blob[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
    }

    /// `from_bytes` refuses `blob` naming `what`; the mapping's loader — framing only — refuses
    /// it too unless the refusal is the payload checksum's or the layout walk's.
    fn refused(blob: &[u8], what: &str) {
        let err = DictIndex::from_bytes(blob).unwrap_err().to_string();
        assert!(err.contains(what), "expected {what:?}, got {err:?}");
        let walked = [
            "payload checksum",
            "out of order",
            "does not cover",
            "head samples",
            "empty index",
        ];
        let framing = !walked.iter().any(|w| what.contains(w));
        let framed =
            DictIndex::from_shared(crate::blob::SharedBytes::from_owned(blob.to_vec()), false);
        assert_eq!(framed.is_err(), framing, "{what:?} on the mapping's path");
    }

    #[test]
    fn every_length_and_table_in_a_blob_is_checked() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 2).unwrap().to_bytes();
        let u64_at = |i: usize| u64::from_le_bytes(blob[i..i + 8].try_into().unwrap()) as usize;
        let table_len = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
        let (heads_len, nb) = (u64_at(16), keys.len().div_ceil(2));
        let heads_at = HEADER + table_len;
        let ends_at = heads_at + heads_len;
        let samples_at = ends_at + nb * 4;
        let blocks_at = samples_at + nb * 8;
        let data_at = blocks_at + nb * 8;
        assert_eq!(data_at + u64_at(24), blob.len());

        refused(&blob[..HEADER - 1], "truncated");
        let mut b = blob.clone();
        b[..4].copy_from_slice(b"BCL1");
        refused(&b, "bad magic");
        let mut b = blob.clone();
        b[4] ^= 1;
        refused(&b, "header checksum");
        let mut b = blob.clone();
        b[data_at] ^= 1;
        refused(&b, "payload checksum");

        let edited = |edit: &dyn Fn(&mut Vec<u8>), what: &str| {
            let mut b = blob.clone();
            edit(&mut b);
            reframe(&mut b);
            refused(&b, what);
        };
        edited(
            &|b| b[12..16].copy_from_slice(&0u32.to_le_bytes()),
            "block size",
        );
        edited(
            &|b| b[12..16].copy_from_slice(&1025u32.to_le_bytes()),
            "block size",
        );
        edited(
            &|b| b[4..12].copy_from_slice(&(keys.len() as u64 + 2).to_le_bytes()),
            "add up",
        );
        edited(
            &|b| b[16..24].copy_from_slice(&(heads_len as u64 + 1).to_le_bytes()),
            "add up",
        );
        edited(
            &|b| b[4..12].copy_from_slice(&u64::MAX.to_le_bytes()),
            "out of range",
        );
        edited(&|b| b[HEADER] = 255, "bad symbol table");
        edited(&|b| b[HEADER + 1] = 0, "bad symbol table");
        edited(
            &|b| b[ends_at..ends_at + 4].copy_from_slice(&u32::MAX.to_le_bytes()),
            "head table out of order",
        );
        edited(
            &|b| {
                let last = ends_at + (nb - 1) * 4;
                b[last..last + 4].copy_from_slice(&(heads_len as u32 - 1).to_le_bytes());
            },
            "does not cover",
        );
        edited(&|b| b[samples_at] ^= 1, "head samples");
        edited(&|b| b[blocks_at] = 1, "block table out of order");
        edited(
            &|b| b[blocks_at + 8..blocks_at + 16].copy_from_slice(&u64::MAX.to_le_bytes()),
            "block table out of order",
        );
        edited(&|b| b.push(0), "add up");

        let empty = DictIndex::build(Vec::<String>::new()).unwrap().to_bytes();
        let mut b = empty.clone();
        b[16..24].copy_from_slice(&1u64.to_le_bytes());
        b.push(b'k');
        reframe(&mut b);
        refused(&b, "empty index with key bytes");
    }

    /// Block data the crate did not write — every byte an escape, a code past the table, a header
    /// claiming more than is there — answers wrong or short, never out of bounds.
    #[test]
    fn corrupted_block_data_never_panics() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 8).unwrap().to_bytes();
        let data_len = u64::from_le_bytes(blob[24..32].try_into().unwrap()) as usize;
        let data_at = blob.len() - data_len;
        for fill in [0x00u8, 0x0F, 0xF0, 0xFE, 0xFF] {
            let mut b = blob.clone();
            b[data_at..].fill(fill);
            reframe(&mut b);
            let idx = DictIndex::from_bytes(&b).unwrap();
            for p in keys.iter().chain(probes(&keys).iter()) {
                let _ = (idx.id(p), idx.lower_bound(p));
            }
            let mut buf = String::new();
            for id in 0..keys.len() as u64 {
                let _ = (idx.key(id), idx.key_into(id, &mut buf));
            }
            assert!(idx.iter().count() <= keys.len());
        }
        // A header whose lcp exceeds the key so far, with the rest of the block intact.
        let mut b = blob.clone();
        b[data_at] = 0xF0 | (b[data_at] & 0x0F);
        reframe(&mut b);
        let idx = DictIndex::from_bytes(&b).unwrap();
        assert_eq!(idx.key(0).as_deref(), Some(keys[0].as_str()));
        let _ = idx.key(1);
        assert_eq!(idx.iter().count(), 1);
    }

    /// The mapping's loader takes the per-block arrays as they are, so every access bounds them:
    /// an end out of order or past the heads, a start past the data, a sample that is not its
    /// head's — wrong answers and short walks, never a panic, and the sections written back as
    /// they were read.
    #[test]
    fn a_load_without_the_walk_bounds_what_the_arrays_say() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 4).unwrap().to_bytes();
        let table_len = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
        let heads_len = u64::from_le_bytes(blob[16..24].try_into().unwrap()) as usize;
        let nb = keys.len().div_ceil(4);
        let ends_at = HEADER + table_len + heads_len;
        let samples_at = ends_at + nb * 4;
        let blocks_at = samples_at + nb * 8;
        type Edit<'a> = &'a dyn Fn(&mut Vec<u8>);
        let edits: [Edit; 8] = [
            &|b| b[ends_at..ends_at + 4].copy_from_slice(&u32::MAX.to_le_bytes()),
            &|b| b[ends_at + 4 * (nb - 1)..][..4].copy_from_slice(&0u32.to_le_bytes()),
            &|b| {
                let past = heads_len as u32 + 7;
                b[ends_at + 4 * (nb / 2)..][..4].copy_from_slice(&past.to_le_bytes());
            },
            &|b| b[samples_at..samples_at + 8].fill(0xFF),
            &|b| b[blocks_at..blocks_at + 8].copy_from_slice(&u64::MAX.to_le_bytes()),
            &|b| b[blocks_at + 8 * (nb / 2)..][..8].copy_from_slice(&u64::MAX.to_le_bytes()),
            &|b| b[blocks_at + 8 * (nb - 1)..][..8].copy_from_slice(&1u64.to_le_bytes()),
            &|b| {
                let past = blob.len() as u64 - (blocks_at + 8 * nb) as u64 + 1;
                b[blocks_at + 8 * (nb - 1)..][..8].copy_from_slice(&past.to_le_bytes());
            },
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut b = blob.clone();
            edit(&mut b);
            reframe(&mut b);
            assert!(
                DictIndex::from_bytes(&b).is_err(),
                "edit {i}: the walk refuses this"
            );
            let idx =
                DictIndex::from_shared(crate::blob::SharedBytes::from_owned(b.clone()), false)
                    .unwrap();
            assert_eq!(idx.len(), keys.len());
            for p in keys.iter().chain(probes(&keys).iter()) {
                let _ = (idx.id(p), idx.contains(p), idx.lower_bound(p));
            }
            let mut buf = String::new();
            for id in 0..=keys.len() as u64 {
                let _ = (idx.key(id), idx.key_into(id, &mut buf));
            }
            assert!(idx.iter().count() <= keys.len(), "edit {i}");
            assert_eq!(idx.to_bytes(), b, "edit {i}");
        }
    }

    /// `load_mmap` borrows every section from the file and answers like the owned index;
    /// `load_mmap_verified` adds the checks `load` makes, so a flipped payload byte the plain
    /// mapping takes is refused by it.
    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_borrows_the_file_and_answers_like_the_owned_index() {
        let keys = corpus();
        let idx = DictIndex::build_with_block(&keys, 3).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_dict_mmap_{}.bdx", std::process::id()));
        idx.save(&path).unwrap();
        // SAFETY: the file is written above and not touched while a mapping of it is alive.
        let mapped = unsafe { DictIndex::load_mmap(&path) }.unwrap();
        check(&mapped, &keys);
        assert_eq!(mapped.to_bytes(), idx.to_bytes());
        // SAFETY: as above.
        let verified = unsafe { DictIndex::load_mmap_verified(&path) }.unwrap();
        check(&verified, &keys);
        drop((mapped, verified));
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 0x55; // block data: a suffix code
        std::fs::write(&path, &bytes).unwrap();
        // SAFETY: as above — the rewrite happened with no mapping alive.
        let mapped = unsafe { DictIndex::load_mmap(&path) }.unwrap();
        assert_eq!(mapped.len(), keys.len());
        // SAFETY: as above.
        let err = unsafe { DictIndex::load_mmap_verified(&path) }.unwrap_err();
        assert!(err.to_string().contains("payload checksum"), "{err}");
        assert!(DictIndex::load(&path).is_err());
        drop(mapped);
        std::fs::remove_file(&path).unwrap();
        // SAFETY: nothing to map; the open fails.
        assert!(unsafe { DictIndex::load_mmap(&path) }.is_err());
    }
}
