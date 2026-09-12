//! An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways, in
//! 45 % less than [`StringIndex`](crate::StringIndex) takes.
//!
//! The sorted keys are cut into blocks of `block` keys (256 by default). A block stores its first
//! key whole and every other as the length of the prefix it shares with its predecessor and the
//! suffix after it, the suffix under a static symbol table ([`fsst`]) trained on the suffixes of
//! the 65 536 keys around it. Those two live apart inside the block: one header byte an entry
//! first, then every suffix end to end. A scan rules most entries out by the header alone, and
//! reading 127 of them is two cache lines where the interleaved form spread the same bytes over
//! seven. Beside the
//! blocks sit an eight-byte sample of each block's head and two arrays that are narrower than a
//! word an entry ([`offsets`](crate::offsets)): where a block's head ends, and where its entries
//! start.
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
use crate::offsets::{self, Offsets};
use std::cmp::Ordering;

/// `[magic 4][n u64][block u32][heads u64][data u64][tables u32][payload u64][head width u8]
/// [block width u8][superblock shift u8][micro width u8][micro u16][shard u16][check u32]`, then
/// the symbol tables — one per `shard` blocks, each behind its own `u32` length, so a reader that
/// knows how many there are walks them without a directory — the head keys end to end, the head
/// ends packed ([`offsets`]), the head samples
/// (`u64`), the block data, the block starts packed and the microblock starts packed. The two
/// start arrays come last because their width is only known once the data is encoded, which is
/// what lets a streamed build write every section once, in order.
const MAGIC: &[u8; 4] = b"BDX2";
/// The formats this one replaces, with what a loader says about each: neither split a block into
/// microblocks, so every lookup scanned the block whole, and nothing in this version can read one.
/// A refusal names what wrote the blob, because a bare "bad magic" sends someone hunting for disk
/// corruption when the file is intact and merely old.
const LEGACY_MAGIC: [(&[u8; 4], &str); 1] = [(
    b"BDX1",
    "dict: blob written by lexindex 2.0 or 2.1, whose blocks interleaved an entry's header \
         with its suffix; rebuild the index from its keys",
)];
/// The header byte that says an entry's shared-prefix and suffix lengths did not fit a nibble
/// each, and are varints at the start of its suffix instead.
const WIDE: u8 = 0xFF;
const HEADER: usize = 56;
const CHECKED: usize = 52; // header bytes the trailing check covers
const DEFAULT_BLOCK: usize = 256;
const MAX_BLOCK: usize = 1024;
/// How deep a key's staircase of shared prefixes may be before [`DictIndex::key_bytes_into`] gives
/// up tracking it and decodes every entry instead. The dictionary reaches 12 at the largest block
/// and a path list 18, so the bound is slack; a block holding a chain like `a`, `aa`, `aaa` is what
/// reaches it, which is legal input and merely slow, not wrong.
const STAIRS: usize = 32;
/// About how many suffixes a symbol table is trained on, from runs of keys spread evenly over the
/// shard it covers — so an index of `n` keys trains a sample this size `n / SHARD_KEYS` times, and
/// that product is what a shard costs to build. The pair sits at the flat bottom of that trade:
/// holding the product fixed, shards of 262 144 / 131 072 / 65 536 / 32 768 / 16 384 keys come out
/// 1.5 / 0.8 / 0.4 / 0.4 / 0.9 % above the smallest size each of eight corpora reaches, and the two
/// ends are worse for opposite reasons — one table over neighbourhoods that share nothing, and
/// 900 bytes of table for every shard.
const TRAIN_PIECES: usize = 10_000;
/// Keys one symbol table is trained on and covers. A table serialises to about 900 bytes, so a
/// shard this size costs 0.015 bytes a key, and what it buys is locality: against one table over
/// the whole index, 0.01 bytes a key on the dictionary, 0.12 on a million URLs, 0.23 on ten
/// million article titles, 0.75 on Russian ones and **1.25 on a path list**, where a million paths
/// run through a few thousand directories — more than one table can hold at once. The one thing it
/// costs is the tables: `domains`, a million short names that end alike wherever they are cut,
/// comes out 0.06 larger.
/// Shards are contiguous and aligned to blocks, so the table for block `b` is `b / shard`.
const SHARD_KEYS: usize = 65_536;
/// How many consecutive keys one of those runs holds. A constant rather than `block`, so the sample
/// keeps its shape whatever block the caller picked: spending the budget on whole blocks meant 157
/// neighbourhoods at 128 keys a block and 19 at 1024, and a table trained on 19 of them is a
/// lottery — `paths` at 1024 came out 0.52 B/key larger than it needed to be, larger than the same
/// corpus at 256. Measured over twelve corpora at three block sizes: 28 of the 36 sizes fall, the
/// worst rises 0.05 B/key, and the dictionary at the default block is sampled identically either
/// way.
const TRAIN_RUN: usize = 256;
/// Samples serialised at a time, so writing does not copy the array whole.
const CHUNK: usize = 4096;
/// Blocks a thread must be given before the encoding is worth splitting across threads at all.
const PARALLEL_BLOCKS: usize = 64;
/// Lookups a batched [`DictIndex::ids_of`] keeps in flight at once.
const LANES: usize = 32;

/// An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways.
///
/// Ids are ranks. `id(key)` is the number of keys below it, `key(id)` the key at that rank, and
/// [`lower_bound`](Self::lower_bound) the rank a key would have, so every range of keys is a
/// range of ids. 2.84 bytes per key on real words, against 5.95 for the transducer of
/// [`StringIndex`](crate::StringIndex) and 10.9 for [`PerfectHashIndex`](crate::PerfectHashIndex);
/// `id` costs a few hundred nanoseconds and `key` about two hundred, both dominated by the scan of
/// one microblock, whose size follows the `block` given at build time.
///
/// Immutable once built, and built in memory: the keys are sorted and deduplicated, then encoded
/// block by block. Persisted with [`to_bytes`](Self::to_bytes) / [`save`](Self::save) and read
/// back by [`from_bytes`](Self::from_bytes) / [`load`](Self::load), which check every length and
/// both checksums, or by `load_mmap`, which borrows the keys and the block data from the mapped
/// file and reads only the per-block samples.
pub struct DictIndex {
    block: usize,
    /// Keys per microblock, a divisor of `block` at or below 32. A block stores the first key
    /// of every microblock past its own as a *restart*, front-coded against the restart before it,
    /// and a lookup walks those restarts to the one microblock it must scan.
    micro: usize,
    /// Microblocks a full block holds, `block / micro` rounded up. Kept rather than derived: every
    /// step of a lookup would otherwise divide by it, and a division by a runtime value is twenty
    /// cycles where the rest of the step is a handful.
    per: usize,
    n: usize,
    /// The sections as they are serialised, owned or mapped. Every block's first key, whole, end
    /// to end; `head_ends[b]` closes block `b`'s.
    heads: SharedBytes,
    /// Where each block's head ends, packed against one base per superblock.
    head_ends: Offsets,
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
    /// Where block `b`'s restart stream starts in `data`, packed the same way; its microblocks
    /// follow it.
    blocks: Offsets,
    /// Where each microblock's own front-coded entries start in `data`, packed the same way. One
    /// entry per microblock, which at the default block is one per sixteen keys.
    micros: Offsets,
    data: SharedBytes,
    /// One symbol table per shard of `shard` blocks; the table for block `b` is `tables[b /
    /// shard]`, and `tables` is never empty.
    tables: Vec<Table>,
    /// Blocks one symbol table covers. At least one, and at most what the header's `u16` holds.
    shard: usize,
}

/// The sample the search runs on: a key's first eight bytes, zero-padded, in byte order.
#[inline(always)]
fn sample_of(key: &[u8]) -> u64 {
    fsst::word_at(key, 0).swap_bytes()
}

/// Keys per microblock for a block of `block`: its largest divisor at or below 32, or the block
/// itself when it has none above 1.
///
/// A lookup scans one restart an earlier microblock plus one entry of its own, `block / micro +
/// micro − 2` in all, and the square root of the block minimises that count — but a restart entry
/// costs about four ordinary ones, its suffix being coded against a key a microblock away rather
/// than its neighbour, so the count is the wrong thing to minimise. Measured over block ∈ {128,
/// 256, 512} × micro ∈ {8, 16, 32, 64} on real words (`local/dictbench`, 2026-09-12): 32 stores
/// 0.08 B/key less than 16 at every block for 0–7 ns on `id`, and 64 saves 0.05 more for 35–50 ns.
/// Confirmed over four corpora from 9 to 52 bytes a key: 8 is smaller *and* slower than 16
/// nowhere — it is dominated everywhere — 16 → 32 buys bytes at 1.0–1.9 mB/ns against `id` and
/// `key_into` together, the rate at which the default block itself was chosen, and 32 → 64 at
/// 0.2–0.5, which is why the cap is 32 and not a function of the keys.
/// A divisor keeps every microblock of a block full but the last, which is what makes a restart's
/// rank `j * micro` rather than a running sum. A block of 32 or fewer, or a prime one, is a single
/// microblock, the layout of one level.
fn micro_for(block: usize) -> usize {
    (2..=32).rev().find(|d| block % d == 0).unwrap_or(block)
}

#[cfg(test)]
thread_local! {
    /// Blocks a shard covers while a test is running; zero is the real rule. A second table exists
    /// only past 65 536 keys, which is more than a unit test should have to build.
    static SHARD_OVERRIDE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Blocks one symbol table covers, for a given block size: [`SHARD_KEYS`] keys' worth, at least one
/// block and never more than the header's `u16` can name. Read once, on the thread that builds.
fn shard_blocks_for(block: usize) -> usize {
    #[cfg(test)]
    {
        let over = SHARD_OVERRIDE.with(std::cell::Cell::get);
        if over != 0 {
            return over;
        }
    }
    (SHARD_KEYS / block).clamp(1, u16::MAX as usize)
}

/// Bytes the whole table section takes: every table behind its own length, so a reader that knows
/// how many there are can walk them without a directory.
fn tables_len(tables: &[Table]) -> usize {
    tables.iter().map(|t| 4 + t.serialized_len()).sum()
}

fn write_tables(tables: &[Table], out: &mut Vec<u8>) {
    for t in tables {
        out.extend_from_slice(&(t.serialized_len() as u32).to_le_bytes());
        t.write_to(out);
    }
}

/// One symbol table a shard, trained in parallel: the shards are independent, and training is
/// linear in the sample, so a table a shard costs the whole budget again on every one of them.
fn train_shards(samples: &[Vec<&[u8]>], threads: usize) -> Vec<Table> {
    if threads == 1 || samples.len() == 1 {
        return samples.iter().map(|s| Table::train(s)).collect();
    }
    let span = samples.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let running: Vec<_> = samples
            .chunks(span)
            .map(|c| scope.spawn(move || c.iter().map(|s| Table::train(s)).collect::<Vec<_>>()))
            .collect();
        running
            .into_iter()
            .flat_map(|h| h.join().expect("training a symbol table cannot panic"))
            .collect()
    })
}

/// Bytes the per-block arrays take: one sample a block, the two packed block arrays and the packed
/// microblock starts. `None` where a header names more blocks than a blob on this platform could
/// hold.
fn arrays_len(
    nb: usize,
    nm: usize,
    head_width: u32,
    block_width: u32,
    micro_width: u32,
    shift: u32,
) -> Option<usize> {
    let packed = offsets::section_len(nb, head_width, shift)?
        .checked_add(offsets::section_len(nb, block_width, shift)?)?
        .checked_add(offsets::section_len(nm, micro_width, shift)?)?;
    nb.checked_mul(8)?.checked_add(packed)
}

/// A monotone array of block offsets as the two sections a blob stores, under the width the
/// values themselves ask for.
fn packed_offsets(values: &[u64]) -> Offsets {
    let width = offsets::width_of(values, offsets::SHIFT);
    let (bases, deltas) = offsets::pack(values, offsets::SHIFT, width);
    Offsets::new(
        SharedBytes::from_owned(bases),
        SharedBytes::from_owned(deltas),
        width,
        offsets::SHIFT,
    )
}

/// Block `b`'s head out of its sections. The arrays are trusted only as far as their sections
/// reach: a mapping is loaded without the walk over them, so an end past the heads, or before
/// the previous one, gives a short head rather than a panic.
#[inline(always)]
fn head_of<'a>(heads: &'a [u8], ends: &Offsets, b: usize) -> &'a [u8] {
    let (start, end) = if b == 0 {
        (0, ends.at(0))
    } else {
        ends.pair(b - 1)
    };
    let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
    heads.get(at(start)..at(end)).unwrap_or_default()
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

/// The varint at `*at` in `data`, advancing `*at` past it.
#[inline(always)]
fn varint_at(data: &[u8], at: &mut usize) -> Option<usize> {
    let (v, rest) = get_varint(data.get(*at..)?)?;
    *at = data.len() - rest.len();
    Some(v)
}

/// An entry's header: one byte `lcp << 4 | len` in the header stream when both are below 15, else
/// [`WIDE`] there and the two as varints at the start of the entry's own suffix. The same bytes
/// either way — the wide pair only moves — and a header stream of exactly one byte an entry is
/// what lets a block carry two streams without storing where they meet.
fn put_header(hdrs: &mut Vec<u8>, sfx: &mut Vec<u8>, lcp: usize, len: usize) {
    if lcp < 15 && len < 15 {
        hdrs.push(((lcp << 4) | len) as u8);
    } else {
        hdrs.push(WIDE);
        put_varint(sfx, lcp);
        put_varint(sfx, len);
    }
}

/// One block's two streams, read in order. The headers are one byte an entry, so the entry count
/// says where they end and the suffixes begin: the split is derived, not stored.
///
/// Splitting them is what makes a scan cheap. A scan rules most entries out by their shared-prefix
/// length alone, which lives in the header — interleaved, those 127 bytes were spread over the
/// seven cache lines of a 128-key block, and here they are two.
struct Entries<'a> {
    hdrs: &'a [u8],
    sfx: &'a [u8],
    at: usize,
    off: usize,
}

impl<'a> Entries<'a> {
    #[inline]
    fn of(data: &'a [u8], count: usize) -> Self {
        let (hdrs, sfx) = data.split_at(count.saturating_sub(1).min(data.len()));
        Self {
            hdrs,
            sfx,
            at: 0,
            off: 0,
        }
    }

    /// The next entry's shared-prefix length and suffix length, leaving the suffix itself for
    /// [`piece`](Self::piece) or [`skip`](Self::skip); `None` past the last header.
    #[inline(always)]
    fn head(&mut self) -> Option<(usize, usize)> {
        let b = *self.hdrs.get(self.at)?;
        self.at += 1;
        if b != WIDE {
            return Some(((b >> 4) as usize, (b & 0xF) as usize));
        }
        let lcp = varint_at(self.sfx, &mut self.off)?;
        let len = varint_at(self.sfx, &mut self.off)?;
        Some((lcp, len))
    }

    /// The suffix of the entry [`head`](Self::head) just read. A stream this crate did not write
    /// gives a short piece rather than a panic.
    #[inline(always)]
    fn piece(&mut self, len: usize) -> &'a [u8] {
        let end = self.off.saturating_add(len).min(self.sfx.len());
        let piece = &self.sfx[self.off..end];
        self.off = end;
        piece
    }

    /// Past that suffix without reading it — what a scan does for every entry its header rules
    /// out, and the reason the two streams are apart.
    #[inline(always)]
    fn skip(&mut self, len: usize) {
        self.off = self.off.saturating_add(len).min(self.sfx.len());
    }
}

/// What one thread produces for its contiguous range of blocks: the sections it would have
/// appended, with the two that are offsets kept relative to the range so the concatenation can
/// rebase them.
struct Part {
    heads: Vec<u8>,
    head_ends: Vec<u64>,
    samples: Vec<u64>,
    blocks: Vec<u64>,
    micros: Vec<u64>,
    data: Vec<u8>,
}

/// The two streams a run of front-coded keys becomes: one header byte an entry, then the suffixes.
/// A block builds three of these — its restarts and, inside `body`, each of its microblocks.
#[derive(Default)]
struct Streams {
    hdrs: Vec<u8>,
    sfx: Vec<u8>,
}

impl Streams {
    fn clear(&mut self) {
        self.hdrs.clear();
        self.sfx.clear();
    }

    fn len(&self) -> usize {
        self.hdrs.len() + self.sfx.len()
    }

    /// Append `key` coded against `prev`, through `encoder` and `packed` as scratch.
    fn push(&mut self, prev: &[u8], key: &[u8], encoder: &fsst::Encoder, packed: &mut Vec<u8>) {
        let l = lcp(prev, key);
        packed.clear();
        encoder.encode_into(&key[l..], packed);
        put_header(&mut self.hdrs, &mut self.sfx, l, packed.len());
        self.sfx.extend_from_slice(packed);
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.hdrs);
        out.extend_from_slice(&self.sfx);
    }
}

/// Write the block held in `restarts` and `body` — whose last microblock is still in `entries` —
/// and rebase the microblock starts it fixes onto `micros`.
fn flush_block(
    w: &mut impl std::io::Write,
    restarts: &Streams,
    entries: &mut Streams,
    body: &mut Vec<u8>,
    starts: &[u64],
    data_len: &mut u64,
    micros: &mut Vec<u64>,
) -> std::io::Result<()> {
    entries.write_to(body);
    entries.clear();
    w.write_all(&restarts.hdrs)?;
    w.write_all(&restarts.sfx)?;
    w.write_all(body)?;
    let at = *data_len + restarts.len() as u64;
    micros.extend(starts.iter().map(|s| at + s));
    *data_len += (restarts.len() + body.len()) as u64;
    body.clear();
    Ok(())
}

/// Encode a range of whole blocks. `keys` must start on a block boundary, which is what makes the
/// parts concatenate into the blob a single pass would have written.
fn encode_range<S: AsRef<str>>(
    keys: &[S],
    block: usize,
    micro: usize,
    tables: &[Table],
    shard: usize,
    first_block: usize,
) -> Part {
    let nb = keys.len().div_ceil(block);
    let mut part = Part {
        heads: Vec::new(),
        head_ends: Vec::with_capacity(nb),
        samples: Vec::with_capacity(nb),
        blocks: Vec::with_capacity(nb),
        micros: Vec::with_capacity(nb * block.div_ceil(micro)),
        data: Vec::new(),
    };
    let mut packed = Vec::with_capacity(64);
    let (mut restarts, mut entries) = (Streams::default(), Streams::default());
    let mut body: Vec<u8> = Vec::with_capacity(block * 8);
    let mut starts: Vec<u64> = Vec::with_capacity(block.div_ceil(micro));
    // A range walks its shards in order, so one encoder is live at a time rather than one per
    // shard: an encoder is 150 KB of lookup tables and a table is 2 KB.
    let mut at_shard = (first_block / shard).min(tables.len() - 1);
    let mut encoder = tables[at_shard].encoder();
    for (b, chunk) in keys.chunks(block).enumerate() {
        let s = ((first_block + b) / shard).min(tables.len() - 1);
        if s != at_shard {
            encoder = tables[s].encoder();
            at_shard = s;
        }
        let encoder = &encoder;
        let head = chunk[0].as_ref().as_bytes();
        part.heads.extend_from_slice(head);
        part.head_ends.push(part.heads.len() as u64);
        part.samples.push(sample_of(head));
        part.blocks.push(part.data.len() as u64);
        restarts.clear();
        body.clear();
        starts.clear();
        let mut prev_restart = head;
        for (j, keys) in chunk.chunks(micro).enumerate() {
            let first = keys[0].as_ref().as_bytes();
            if j > 0 {
                restarts.push(prev_restart, first, encoder, &mut packed);
                prev_restart = first;
            }
            starts.push(body.len() as u64);
            entries.clear();
            for w in keys.windows(2) {
                let (a, b) = (w[0].as_ref().as_bytes(), w[1].as_ref().as_bytes());
                entries.push(a, b, encoder, &mut packed);
            }
            entries.write_to(&mut body);
        }
        // The restarts close only once the block does, so a microblock's start is known relative
        // to the body and rebased here.
        let at = part.data.len() as u64 + restarts.len() as u64;
        part.micros.extend(starts.iter().map(|s| at + s));
        restarts.write_to(&mut part.data);
        part.data.extend_from_slice(&body);
    }
    part
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
    /// head and its arrays are shared over, and it is split into microblocks of 32 — one microblock
    /// below that — of which a lookup scans one, after one restart a microblock: `block / 32 + 30`
    /// entries from 64 up, not `block − 1`. On
    /// real words 32 / 64 / 128 / 256 / 512 / 1024 give 3.23 / 3.03 / 2.90 / 2.84 / 2.81 / 2.79
    /// bytes per key, `id` at 252–254 / 272–284 / 285–288 / 298–302 / 316–322 / 344–347 ns and
    /// `key_into` at 154–155 / 169–172 / 184–188 / 207 / 227–232 / 263–272. At 256 the index is
    /// under `marisa-trie`'s 2.955 floor on that corpus.
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
        Self::from_sorted(&keys, block, micro_for(block))
    }

    /// Build from keys that are **already in ascending byte order** — a sorted file, a database
    /// cursor, the output of an external sort. Adjacent duplicates are dropped exactly as
    /// [`build`](Self::build) drops them after sorting, so for the same key set the two produce
    /// **byte-identical** blobs. Blocks of 256 keys.
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
        Self::from_sorted(&keys, block, micro_for(block))
    }

    /// [`build_with_block`](Self::build_with_block) with the microblock size chosen by the caller
    /// instead of by `micro_for`: `1..=block`, dividing it. For the harnesses that measure the
    /// (block, micro) grid; the shipped builders take the largest divisor at or below 32.
    #[cfg(feature = "fuzzing")]
    #[doc(hidden)]
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
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        Self::from_sorted(&keys, block, micro)
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
    /// words, 0.31 at 512 keys a block. An index whose heads alone pass 4 GiB is refused with
    /// an error naming the larger block that would fit it.
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
        let mut head_ends: Vec<u64> = Vec::new();
        let mut samples: Vec<u8> = Vec::new();
        src.each(&mut |key| {
            if n % block == 0 {
                heads.extend_from_slice(key.as_bytes());
                head_ends.push(heads.len() as u64);
                samples.extend_from_slice(&sample_of(key.as_bytes()).to_le_bytes());
            }
            n += 1;
            Ok(())
        })?;
        let nb = n.div_ceil(block);
        let micro = micro_for(block);
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
            let bytes = key.as_bytes();
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
            Ok(())
        })?;
        let tables = {
            let samples: Vec<Vec<&[u8]>> = spans
                .iter()
                .map(|s| s.iter().map(|&(a, b)| &arena[a..b]).collect())
                .collect();
            let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            train_shards(&samples, threads.min(shards))
        };
        drop(arena);
        drop(spans);

        let mut table_bytes = Vec::with_capacity(tables_len(&tables));
        write_tables(&tables, &mut table_bytes);
        let mut at_shard = 0usize;
        let mut encoder = tables[0].encoder();
        let head_width = offsets::width_of(&head_ends, offsets::SHIFT);
        let (head_bases, head_deltas) = offsets::pack(&head_ends, offsets::SHIFT, head_width);
        drop(head_ends);
        let mut blocks = Vec::with_capacity(nb);
        let mut micros = Vec::with_capacity(nb * block.div_ceil(micro));
        crate::blob::write_atomically_with(path, |w| {
            // The header is written last: it carries the block data's length and a hash over every
            // section, and neither is known until the encoding is done.
            w.write_all(&[0u8; HEADER])?;
            w.write_all(&table_bytes)?;
            w.write_all(&heads)?;
            w.write_all(&head_bases)?;
            w.write_all(&head_deltas)?;
            w.write_all(&samples)?;

            // Pass three: encode. The block and microblock starts fall out of it, which is why
            // their sections were left to the end.
            let mut data_len = 0u64;
            let mut packed = Vec::with_capacity(64);
            // A block's headers only reach the file once its suffixes are known, and its restarts
            // only once the block does, so one block at a time is buffered — at most 1024 entries,
            // and the price of the streams being apart in the blob `from_sorted` writes in one
            // pass.
            let (mut restarts, mut entries) = (Streams::default(), Streams::default());
            let mut body: Vec<u8> = Vec::with_capacity(block * 8);
            let mut starts: Vec<u64> = Vec::with_capacity(block.div_ceil(micro));
            let mut prev: Vec<u8> = Vec::new();
            let mut restart: Vec<u8> = Vec::new();
            let mut i = 0usize;
            src.each(&mut |key| {
                let bytes = key.as_bytes();
                let off = i % block;
                if off == 0 {
                    let s = i / span;
                    if s != at_shard {
                        encoder = tables[s].encoder();
                        at_shard = s;
                    }
                    flush_block(
                        w,
                        &restarts,
                        &mut entries,
                        &mut body,
                        &starts,
                        &mut data_len,
                        &mut micros,
                    )?;
                    restarts.clear();
                    starts.clear();
                    starts.push(0);
                    blocks.push(data_len);
                } else if off % micro == 0 {
                    entries.write_to(&mut body);
                    entries.clear();
                    starts.push(body.len() as u64);
                    restarts.push(&restart, bytes, &encoder, &mut packed);
                } else {
                    entries.push(&prev, bytes, &encoder, &mut packed);
                }
                if off % micro == 0 {
                    restart.clear();
                    restart.extend_from_slice(bytes);
                }
                prev.clear();
                prev.extend_from_slice(bytes);
                i += 1;
                Ok(())
            })?;
            flush_block(
                w,
                &restarts,
                &mut entries,
                &mut body,
                &starts,
                &mut data_len,
                &mut micros,
            )?;
            if i != n {
                return Err(IndexError::Format(
                    "dict: the key stream changed between passes",
                ));
            }

            // The two start arrays are the last sections precisely because this is where they are
            // known: their widths fall out of the encoding that has just finished.
            let block_width = offsets::width_of(&blocks, offsets::SHIFT);
            let (block_bases, block_deltas) = offsets::pack(&blocks, offsets::SHIFT, block_width);
            // A block that is one microblock starts its only microblock where the block starts.
            let micros: &[u64] = if block.div_ceil(micro) == 1 {
                &[]
            } else {
                &micros
            };
            let micro_width = offsets::width_of(micros, offsets::SHIFT);
            let (micro_bases, micro_deltas) = offsets::pack(micros, offsets::SHIFT, micro_width);
            w.write_all(&block_bases)?;
            w.write_all(&block_deltas)?;
            w.write_all(&micro_bases)?;
            w.write_all(&micro_deltas)?;
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
            h[0..4].copy_from_slice(MAGIC);
            h[4..12].copy_from_slice(&(n as u64).to_le_bytes());
            h[12..16].copy_from_slice(&(block as u32).to_le_bytes());
            h[16..24].copy_from_slice(&(heads.len() as u64).to_le_bytes());
            h[24..32].copy_from_slice(&data_len.to_le_bytes());
            h[32..36].copy_from_slice(&(table_bytes.len() as u32).to_le_bytes());
            h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
            h[44] = head_width as u8;
            h[45] = block_width as u8;
            h[46] = offsets::SHIFT as u8;
            h[47] = micro_width as u8;
            h[48..50].copy_from_slice(&(micro as u16).to_le_bytes());
            h[50..52].copy_from_slice(&(shard as u16).to_le_bytes());
            let check_word = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
            h[CHECKED..HEADER].copy_from_slice(&check_word.to_le_bytes());
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&h)?;
            check()
        })?;
        Ok(n)
    }

    fn from_sorted<S: AsRef<str>>(
        keys: &[S],
        block: usize,
        micro: usize,
    ) -> Result<Self, IndexError> {
        let threads = (keys.len().div_ceil(block) / PARALLEL_BLOCKS)
            .min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
            .max(1);
        Self::from_sorted_on(keys, block, micro, threads)
    }

    /// [`from_sorted`](Self::from_sorted) with the thread count fixed, so a test can hold the
    /// output to the one a single thread produces.
    fn from_sorted_on<S: AsRef<str>>(
        keys: &[S],
        block: usize,
        micro: usize,
        threads: usize,
    ) -> Result<Self, IndexError> {
        let n = keys.len();
        let nb = n.div_ceil(block);
        let shard = shard_blocks_for(block);
        let span = shard * block;
        // Train on the suffixes a spread of runs would store, in key order: a restart is coded
        // against the restart before it, every other key against its predecessor. Each shard is
        // sampled inside itself, by the rule that samples the whole index when there is one shard.
        let mut pieces: Vec<Vec<&[u8]>> = vec![Vec::new(); nb.div_ceil(shard).max(1)];
        let mut restart: &[u8] = keys.first().map_or(&[][..], |k| k.as_ref().as_bytes());
        let mut prev: &[u8] = restart;
        let mut step = (span.min(n) / TRAIN_PIECES).max(1);
        for (i, key) in keys.iter().enumerate().skip(1) {
            let key = key.as_ref().as_bytes();
            let off = i % block;
            if off == 0 {
                if i % span == 0 {
                    step = ((n - i).min(span) / TRAIN_PIECES).max(1);
                }
                restart = key;
                prev = key;
                continue;
            }
            let starts = off % micro == 0;
            if (i % span / TRAIN_RUN) % step == 0 {
                let against = if starts { restart } else { prev };
                pieces[i / span].push(&key[lcp(against, key)..]);
            }
            if starts {
                restart = key;
            }
            prev = key;
        }
        let tables = train_shards(&pieces, threads);
        drop(pieces);

        // Once the table is fixed a block depends on nothing outside itself, so contiguous ranges
        // of blocks encode on their own threads and the parts are concatenated in order — the bytes
        // do not depend on how many threads ran. The threads take a `&[&str]` view rather than the
        // caller's `&[S]`, which would need `S: Sync` on a signature that has not asked for it; the
        // view costs sixteen bytes a key for the length of the encoding and is only built when
        // there is enough work to split.
        let run = (block * nb.div_ceil(threads)).max(1);
        let parts: Vec<Part> = if threads == 1 {
            keys.chunks(run)
                .enumerate()
                .map(|(c, range)| {
                    encode_range(range, block, micro, &tables, shard, c * run / block)
                })
                .collect()
        } else {
            let view: Vec<&str> = keys.iter().map(AsRef::as_ref).collect();
            std::thread::scope(|scope| {
                let running: Vec<_> = view
                    .chunks(run)
                    .enumerate()
                    .map(|(c, range)| {
                        let tables = &tables;
                        scope.spawn(move || {
                            encode_range(range, block, micro, tables, shard, c * run / block)
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
        let mut samples = Vec::with_capacity(nb);
        let mut blocks = Vec::with_capacity(nb);
        let mut micros = Vec::with_capacity(parts.iter().map(|p| p.micros.len()).sum());
        let mut data = Vec::with_capacity(parts.iter().map(|p| p.data.len()).sum());
        // Each part is dropped as it is appended, so the two copies never coexist whole.
        for part in parts {
            let (at_head, at_data) = (heads.len() as u64, data.len() as u64);
            head_ends.extend(part.head_ends.iter().map(|end| at_head + end));
            blocks.extend(part.blocks.iter().map(|start| at_data + start));
            micros.extend(part.micros.iter().map(|start| at_data + start));
            heads.extend_from_slice(&part.heads);
            samples.extend_from_slice(&part.samples);
            data.extend_from_slice(&part.data);
        }
        Ok(Self {
            block,
            micro,
            per: block.div_ceil(micro),
            n,
            heads: SharedBytes::from_owned(heads),
            head_ends: packed_offsets(&head_ends),
            samples,
            blocks: packed_offsets(&blocks),
            micros: packed_offsets(if block.div_ceil(micro) == 1 {
                &[]
            } else {
                &micros
            }),
            data: SharedBytes::from_owned(data),
            tables,
            shard,
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

    /// The symbol table block `b` was encoded under.
    #[inline(always)]
    fn table_of(&self, b: usize) -> &Table {
        let s = b / self.shard;
        &self.tables[if s < self.tables.len() { s } else { 0 }]
    }

    #[inline(always)]
    fn head_end(&self, b: usize) -> usize {
        usize::try_from(self.head_ends.at(b)).unwrap_or(usize::MAX)
    }

    /// Where block `b`'s entries start, as stored — past `data` on a blob nothing walked.
    #[inline(always)]
    fn block_start(&self, b: usize) -> u64 {
        self.blocks.at(b)
    }

    #[inline(always)]
    fn head(&self, b: usize) -> &[u8] {
        head_of(&self.heads, &self.head_ends, b)
    }

    /// Keys in block `b` — the last one holds the remainder. It is also where its header stream
    /// ends, one byte an entry, so every reader needs it before it can find the suffixes.
    #[inline(always)]
    fn count_in(&self, b: usize) -> usize {
        (self.n - b * self.block).min(self.block)
    }

    /// Microblocks in block `b`, which is also the length of its restart run counting the head.
    /// Only the last block can hold fewer than a full block's.
    #[inline(always)]
    fn micros_in(&self, b: usize) -> usize {
        if b + 1 < self.blocks_len() {
            self.per
        } else {
            self.count_in(b).div_ceil(self.micro)
        }
    }

    /// Keys in microblock `j` of block `b`, the same way [`count_in`](Self::count_in) counts a
    /// block's.
    #[inline(always)]
    fn micro_count(&self, b: usize, j: usize) -> usize {
        self.count_in(b)
            .saturating_sub(j * self.micro)
            .min(self.micro)
    }

    /// The microblock the key at rank `id` sits in.
    #[inline(always)]
    fn micro_of(&self, id: usize) -> usize {
        (id / self.block) * self.per + (id % self.block) / self.micro
    }

    /// Where microblock `j` of block `b` starts in `data`. A block that is one microblock has no
    /// restart run, so its only microblock starts where the block does and the blob stores no
    /// array for it.
    #[inline(always)]
    fn micro_start(&self, b: usize, j: usize) -> u64 {
        if self.per == 1 {
            self.blocks.at(b)
        } else {
            self.micros.at(b * self.per + j)
        }
    }

    /// Where block `b`'s data ends in `data`.
    #[inline(always)]
    fn block_end(&self, b: usize) -> usize {
        let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        if b + 1 < self.blocks_len() {
            at(self.blocks.at(b + 1))
        } else {
            self.data.len()
        }
    }

    /// Block `b`'s restart run: the first key of each of its microblocks past the head, each coded
    /// against the one before it. Bounded the way [`head`](Self::head) is.
    #[inline(always)]
    fn restart_data(&self, b: usize) -> &[u8] {
        let data: &[u8] = &self.data;
        let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        let (start, end) = (at(self.blocks.at(b)), at(self.micro_start(b, 0)));
        data.get(start..end).unwrap_or_default()
    }

    /// Microblock `j` of block `b`, bounded the same way. The last microblock of a block ends where
    /// the block does, not where the next block's restarts start.
    #[inline(always)]
    fn micro_data(&self, b: usize, j: usize) -> &[u8] {
        let data: &[u8] = &self.data;
        let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        let (start, end) = if j + 1 < self.micros_in(b) {
            let (start, end) = self.micros.pair(b * self.per + j);
            (at(start), at(end))
        } else {
            (at(self.micro_start(b, j)), self.block_end(b))
        };
        data.get(start..end).unwrap_or_default()
    }

    /// How many leading bytes an entry's stored suffix shares with `rest`, and how the suffix
    /// orders against it — read off the codes, nothing decoded. A stream this crate did not write
    /// ends the suffix where it stops making sense.
    #[inline]
    fn compare_piece(&self, table: &Table, packed: &[u8], rest: &[u8]) -> (usize, Ordering) {
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
                let Some(sym) = table.symbol(packed[i]) else {
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
        let lo = self.samples.partition_point(|&x| x < s);
        let hi = self.samples.partition_point(|&x| x <= s);
        let l = self.head_boundary(probe, lo.saturating_sub(1), hi);
        self.locate_in(l, probe)
    }

    /// The first block index in `[l, r)` whose head is past `probe` — the samples have already
    /// narrowed that to the blocks whose heads can share the probe's first eight bytes.
    #[inline]
    fn head_boundary(&self, probe: &[u8], mut l: usize, mut r: usize) -> usize {
        let (heads, ends): (&[u8], &Offsets) = (&self.heads, &self.head_ends);
        while l < r {
            let m = l + (r - l) / 2;
            if head_of(heads, ends, m) <= probe {
                l = m + 1;
            } else {
                r = m;
            }
        }
        l
    }

    /// Walk a front-coded run of `count` keys against `probe`, whose first key is below it and
    /// shares `matched` bytes with it. Answers with the last key of the run not above the probe,
    /// how many bytes that one shares with it, and whether it *is* the probe — so the first key not
    /// below the probe is the one after it, in this run or wherever the run ends.
    ///
    /// Every entry but one is ruled out by its header alone. An entry stores what it shares with
    /// the key before it, and below `matched` the probe agreed with that key, so a shorter shared
    /// prefix puts the entry past the probe and a longer one keeps it below with nothing new
    /// matched. The suffix of a ruled-out entry is never read.
    #[inline]
    fn scan_run(
        &self,
        table: &Table,
        entries: &mut Entries<'_>,
        count: usize,
        probe: &[u8],
        mut matched: usize,
    ) -> (usize, usize, bool) {
        for j in 1..count {
            let Some((l, len)) = entries.head() else {
                return (j - 1, matched, false);
            };
            if l < matched {
                return (j - 1, matched, false);
            }
            if l > matched {
                entries.skip(len);
                continue;
            }
            let (c, ord) = self.compare_piece(table, entries.piece(len), &probe[matched..]);
            match ord {
                Ordering::Equal => return (j, matched + c, true),
                Ordering::Greater => return (j - 1, matched, false),
                Ordering::Less => matched += c,
            }
        }
        (count.saturating_sub(1), matched, false)
    }

    /// The rest of [`locate`](Self::locate) once the block boundary is known: the block below it
    /// is the only one that can hold `probe`. Its restart run says which of its microblocks can,
    /// and that one microblock is scanned — the block itself never is.
    fn locate_in(&self, l: usize, probe: &[u8]) -> (u64, bool) {
        if l == 0 {
            return (0, false);
        }
        let b = l - 1;
        let base = b * self.block;
        let table = self.table_of(b);
        let head = self.head(b);
        if head == probe {
            return (base as u64, true);
        }
        // The probe is above the run's first key and shares `matched` bytes with it.
        let matched = lcp(head, probe);
        let r = self.micros_in(b);
        let (j, matched) = if r > 1 {
            let mut restarts = Entries::of(self.restart_data(b), r);
            let (j, matched, hit) = self.scan_run(table, &mut restarts, r, probe, matched);
            if hit {
                return ((base + j * self.micro) as u64, true);
            }
            (j, matched)
        } else {
            (0, matched)
        };
        let count = self.micro_count(b, j);
        let mut entries = Entries::of(self.micro_data(b, j), count);
        let (k, _, hit) = self.scan_run(table, &mut entries, count, probe, matched);
        let rank = base + j * self.micro + k + usize::from(!hit);
        (rank as u64, hit)
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

    /// [`ids_of`](Self::ids_of) over `n` keys given as bytes by position, for a caller whose keys
    /// are not `str`s — a lookup reading an Arrow buffer.
    ///
    /// A batch is not a loop here. A single lookup is a chain of dependent loads — the binary
    /// search over the block samples, then the block's head, then its data — and each one waits on
    /// the last, so a lookup into an index past the last-level cache spends most of its time
    /// stalled. Doing `LANES` of them in lockstep puts that many loads in flight at once: the
    /// binary searches advance one step for every lane before any lane takes its second, and the
    /// blocks the lanes landed on are prefetched whole before any lane is scanned.
    ///
    /// What that is worth is what is left stalled, so it grows with the index and shrinks with the
    /// block — a larger block leaves fewer samples to search and more bytes to scan, and only the
    /// search is fully hidden. Real word bigrams, half of the probes members, shuffled, a loop of
    /// [`id`](Self::id) as the control in the same process:
    ///
    /// | keys | block | index | `id` in a loop | `ids_of` |
    /// |---:|---:|---:|---:|---:|
    /// | 10 M | 32 | 39 MB | 861 ns | **535** |
    /// | 10 M | 128 | 32 MB | 771 | 645 |
    /// | 1 M | 32 | 3.2 MB | 377 | 306 |
    /// | 200 k | 32 | 0.6 MB | 277 | 281 |
    ///
    /// The last row is the point: an index that fits in cache has nothing to hide, and the batch
    /// is then the same work in a less obvious order.
    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        let mut out = Vec::with_capacity(n);
        if self.n == 0 {
            out.resize(n, None);
            return out;
        }
        let nb = self.blocks_len();
        // Enough rounds for the widest search: each one at least halves the remaining span.
        let rounds = usize::BITS as usize - nb.leading_zeros() as usize + 1;
        let mut s = [0u64; LANES];
        let (mut lo_at, mut lo_len) = ([0usize; LANES], [0usize; LANES]);
        let (mut hi_at, mut hi_len) = ([0usize; LANES], [0usize; LANES]);
        let mut bound = [0usize; LANES];
        for base in (0..n).step_by(LANES) {
            let m = LANES.min(n - base);
            for j in 0..m {
                s[j] = sample_of(key(base + j));
                (lo_at[j], lo_len[j], hi_at[j], hi_len[j]) = (0, nb, 0, nb);
            }
            // `lo` is the first sample not below the probe's, `hi` the first above it — the two
            // `partition_point`s `locate` makes, run for every lane at once.
            for _ in 0..rounds {
                for j in 0..m {
                    if lo_len[j] > 0 {
                        let half = lo_len[j] >> 1;
                        if self.samples[lo_at[j] + half] < s[j] {
                            lo_at[j] += half + 1;
                            lo_len[j] -= half + 1;
                        } else {
                            lo_len[j] = half;
                        }
                    }
                    if hi_len[j] > 0 {
                        let half = hi_len[j] >> 1;
                        if self.samples[hi_at[j] + half] <= s[j] {
                            hi_at[j] += half + 1;
                            hi_len[j] -= half + 1;
                        } else {
                            hi_len[j] = half;
                        }
                    }
                }
            }
            for j in 0..m {
                bound[j] = self.head_boundary(key(base + j), lo_at[j].saturating_sub(1), hi_at[j]);
            }
            // The block start is one load and the data it names another, so they are pulled in as
            // two passes rather than one: the second cannot be issued until the first has landed.
            for &b in bound.iter().take(m) {
                if b > 0 {
                    self.blocks.prefetch(b - 1);
                }
            }
            // A block's entries are contiguous, so the whole run is pulled in, not just its first
            // line: at 128 keys a block is several lines and the scan walks all of them.
            for &b in bound.iter().take(m) {
                if b > 0 {
                    let at = self.block_start(b - 1) as usize;
                    let end = if b < nb {
                        self.block_start(b) as usize
                    } else {
                        self.data.len()
                    };
                    for line in (at..end.min(at + 8 * 64)).step_by(64) {
                        crate::blob::prefetch_byte(&self.data, line);
                    }
                }
            }
            for (j, &b) in bound.iter().enumerate().take(m) {
                out.push(match self.locate_in(b, key(base + j)) {
                    (rank, true) => Some(rank),
                    _ => None,
                });
            }
        }
        out
    }

    /// Decode the next entry of a block onto `cur`, which holds the previous one, moving `at`
    /// past it; `false` on data this crate did not write.
    #[inline]
    fn advance(&self, table: &Table, entries: &mut Entries<'_>, cur: &mut Vec<u8>) -> bool {
        let Some((l, len)) = entries.head() else {
            return false;
        };
        let piece = entries.piece(len);
        if l > cur.len() || piece.len() != len {
            return false;
        }
        cur.truncate(l);
        table.decode_into(piece, cur)
    }

    /// Take `out`, which holds a front-coded run's first key, `steps` entries along that run.
    ///
    /// An entry stores what it shares with its predecessor, so an entry whose `lcp` is at least a
    /// later entry's contributes nothing that survives to the key being asked for. The entries that
    /// do contribute form a strictly increasing staircase of `lcp`, and a monotonic stack over the
    /// headers finds it in the one pass the walk already makes. Every header is still read — the
    /// lengths before an entry are what place its suffix — but a handful of suffixes are decoded
    /// rather than one per entry, and the decode is the expensive half: 207 → 146 ns at 32 keys
    /// a block on the dictionary, 751 → 454 at 128, 464 → 265 on a path list.
    ///
    /// A staircase deeper than the stack falls back to decoding every entry, which is correct at
    /// any depth.
    fn climb(
        &self,
        table: &Table,
        data: &[u8],
        count: usize,
        steps: usize,
        out: &mut Vec<u8>,
    ) -> bool {
        let mut entries = Entries::of(data, count);
        // Where each stair's suffix sits, not the suffix itself: most entries are popped again,
        // and a span is two words to record where a slice is two words to build and bound.
        let mut stair = [(0usize, 0usize, 0usize); STAIRS];
        let mut depth = 0usize;
        for _ in 0..steps {
            let Some((l, len)) = entries.head() else {
                return false;
            };
            let at = entries.off;
            entries.skip(len);
            if entries.off - at != len {
                return false;
            }
            while depth > 0 && stair[depth - 1].0 >= l {
                depth -= 1;
            }
            if depth == STAIRS {
                // Nothing has been written yet, so `out` still holds the run's first key.
                let mut entries = Entries::of(data, count);
                return (0..steps).all(|_| self.advance(table, &mut entries, out));
            }
            stair[depth] = (l, at, len);
            depth += 1;
        }
        for &(l, at, len) in &stair[..depth] {
            let piece = &entries.sfx[at..at + len];
            if l > out.len() {
                return false;
            }
            out.truncate(l);
            if !table.decode_into(piece, out) {
                return false;
            }
        }
        true
    }

    /// The key at rank `id` into `out`, cleared first; `false`, with `out` empty, past the last
    /// key.
    ///
    /// Two climbs, not one: the block's restart run up to the microblock the rank falls in, then
    /// that microblock up to the rank. Together they read `block / micro + micro − 2` headers
    /// where one level read `block − 1`.
    fn key_bytes_into(&self, id: u64, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        if id >= self.n {
            return false;
        }
        let b = id / self.block;
        let off = id % self.block;
        let (j, steps) = (off / self.micro, off % self.micro);
        out.extend_from_slice(self.head(b));
        let table = self.table_of(b);
        if j > 0 && !self.climb(table, self.restart_data(b), self.micros_in(b), j, out) {
            return false;
        }
        self.climb(
            table,
            self.micro_data(b, j),
            self.micro_count(b, j),
            steps,
            out,
        )
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

    /// Batched [`key`](Self::key): one answer per id, aligned with `ids`, `None` where an id is
    /// past the last key or a corrupted blob decodes to something that is not UTF-8.
    ///
    /// **Ids that ascend within one block are answered by a single walk of it.** A block is a
    /// chain — every entry is coded against the one before it — so re-entering it per id re-reads
    /// the same headers, while walking it once decodes each entry once. That is exactly the shape
    /// [`prefix_id_range`](Self::prefix_id_range) hands over, and it is why going through that
    /// range used to be slower than [`prefix`](Self::prefix).
    ///
    /// Ids in no particular order cost what they always did: the per-id staircase, which reads the
    /// headers below the id but decodes only the few entries that contribute a byte. A walk is
    /// started only when the *next* id is in the same block and above this one, so a scattered
    /// batch never pays for entries it does not want.
    pub fn keys_of(&self, ids: &[u64]) -> Vec<Option<String>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut buf: Vec<u8> = Vec::new();
        let mut entries = Entries::of(&[], 0);
        // The microblock a walk is open on and how many of its entries it has consumed;
        // `usize::MAX` for none.
        let (mut open, mut consumed) = (usize::MAX, 0usize);
        for (i, &id) in ids.iter().enumerate() {
            if id >= self.n as u64 {
                out.push(None);
                continue;
            }
            let at = id as usize;
            let m = self.micro_of(at);
            let j = at % self.block % self.micro;
            if open != m || j < consumed {
                let more = ids.get(i + 1).is_some_and(|&next| {
                    next > id && next < self.n as u64 && self.micro_of(next as usize) == m
                });
                // Opening a walk costs the climb to the microblock's first key, so it only pays
                // when another id of the same microblock follows.
                if !more || !self.key_bytes_into((at - j) as u64, &mut buf) {
                    out.push(self.key(id));
                    open = usize::MAX;
                    continue;
                }
                let (b, j) = (at / self.block, at % self.block / self.micro);
                entries = Entries::of(self.micro_data(b, j), self.micro_count(b, j));
                (open, consumed) = (m, 0);
            }
            let mut ok = true;
            while consumed < j && ok {
                ok = self.advance(self.table_of(at / self.block), &mut entries, &mut buf);
                consumed += 1;
            }
            if !ok {
                out.push(None);
                open = usize::MAX;
                continue;
            }
            out.push(std::str::from_utf8(&buf).ok().map(str::to_owned));
        }
        out
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
        let mut restart: Vec<u8> = Vec::new();
        let mut entries = Entries::of(&[], 0);
        let mut restarts = Entries::of(&[], 0);
        let mut primed = false;
        std::iter::from_fn(move || {
            if id >= self.n {
                return None;
            }
            let (b, off) = (id / self.block, id % self.block);
            let (j, k) = (off / self.micro, off % self.micro);
            let ok = if !primed || off == 0 {
                // A block opens on its head; a resume point inside one is reached by walking the
                // restart run to its microblock and that microblock to the key.
                restart.clear();
                restart.extend_from_slice(self.head(b));
                restarts = Entries::of(self.restart_data(b), self.micros_in(b));
                primed = true;
                let ok =
                    (0..j).all(|_| self.advance(self.table_of(b), &mut restarts, &mut restart));
                cur.clear();
                cur.extend_from_slice(&restart);
                entries = Entries::of(self.micro_data(b, j), self.micro_count(b, j));
                ok && (0..k).all(|_| self.advance(self.table_of(b), &mut entries, &mut cur))
            } else if k == 0 {
                // The next microblock opens on the next restart, not on the key just returned.
                let ok = self.advance(self.table_of(b), &mut restarts, &mut restart);
                cur.clear();
                cur.extend_from_slice(&restart);
                entries = Entries::of(self.micro_data(b, j), self.micro_count(b, j));
                ok
            } else {
                self.advance(self.table_of(b), &mut entries, &mut cur)
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
        let mut tables = Vec::with_capacity(tables_len(&self.tables));
        write_tables(&self.tables, &mut tables);
        f(&tables)?;
        f(&self.heads)?;
        for section in self.head_ends.sections() {
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
        f(&self.data)?;
        for section in self.blocks.sections() {
            f(section)?;
        }
        for section in self.micros.sections() {
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
        h[32..36].copy_from_slice(&(tables_len(&self.tables) as u32).to_le_bytes());
        h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
        h[44] = self.head_ends.width() as u8;
        h[45] = self.blocks.width() as u8;
        h[46] = self.head_ends.shift() as u8;
        h[47] = self.micros.width() as u8;
        h[48..50].copy_from_slice(&(self.micro as u16).to_le_bytes());
        h[50..52].copy_from_slice(&(self.shard as u16).to_le_bytes());
        let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        h
    }

    /// Serialise to `[magic "BDX2"][n][block][head bytes][data bytes][table bytes][payload]
    /// [offset widths][micro][shard][check]`, then the symbol tables, the head keys, the per-block
    /// arrays,
    /// the block data and the two start arrays. `check` is a hash of the preceding header bytes and
    /// `payload` a hash of everything after it, both verified on load.
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
            + tables_len(&self.tables)
            + self.heads.len()
            + self.blocks_len() * 8
            + self.head_ends.len()
            + self.blocks.len()
            + self.micros.len()
            + self.data.len()
    }

    /// Reconstruct from [`DictIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes: the magic, both checksums, every section length, the symbol
    /// table and the four per-block arrays are checked before anything is trusted, so a
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
        if let Some((_, why)) = LEGACY_MAGIC.iter().find(|(m, _)| bytes.starts_with(*m)) {
            return Err(IndexError::Format(why));
        }
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
        let (head_width, block_width, shift, micro_width) = (
            u32::from(bytes[44]),
            u32::from(bytes[45]),
            u32::from(bytes[46]),
            u32::from(bytes[47]),
        );
        if head_width > offsets::MAX_WIDTH
            || block_width > offsets::MAX_WIDTH
            || micro_width > offsets::MAX_WIDTH
            || shift >= 32
        {
            return Err(IndexError::Format("dict: offset widths out of range"));
        }
        let micro = u16::from_le_bytes(bytes[48..50].try_into().unwrap()) as usize;
        if !(1..=block).contains(&micro) {
            return Err(IndexError::Format("dict: microblock size out of range"));
        }
        let shard = u16::from_le_bytes(bytes[50..52].try_into().unwrap()) as usize;
        if shard == 0 {
            return Err(IndexError::Format("dict: symbol-table shard out of range"));
        }
        let nb = n.div_ceil(block);
        // Every microblock holds at least one key, so the count is bounded by the key count and
        // the product below cannot overflow.
        let per = block.div_ceil(micro);
        let nm = match nb {
            0 => 0,
            _ if per == 1 => 0,
            nb => (nb - 1) * per + (n - (nb - 1) * block).min(block).div_ceil(micro),
        };
        let arrays = arrays_len(nb, nm, head_width, block_width, micro_width, shift)
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
        let tables = {
            let section = take(table_len);
            let bad = || IndexError::Format("dict: bad symbol table");
            let mut tables = Vec::new();
            let mut at = 0usize;
            for _ in 0..nb.div_ceil(shard).max(1) {
                let len = section
                    .get(at..at + 4)
                    .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
                    .ok_or_else(bad)?;
                let raw = section.get(at + 4..at + 4 + len).ok_or_else(bad)?;
                tables.push(Table::from_bytes(raw).ok_or_else(bad)?);
                at += 4 + len;
            }
            if at != section.len() {
                return Err(bad());
            }
            tables
        };
        let heads = take(heads_len);
        let head_ends = Offsets::new(
            take(offsets::bases_len(nb, shift)),
            take(offsets::deltas_len(nb, head_width)),
            head_width,
            shift,
        );
        let samples = take(nb * 8)
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes")))
            .collect();
        let data = take(data_len);
        let blocks = Offsets::new(
            take(offsets::bases_len(nb, shift)),
            take(offsets::deltas_len(nb, block_width)),
            block_width,
            shift,
        );
        let micros = Offsets::new(
            take(offsets::bases_len(nm, shift)),
            take(offsets::deltas_len(nm, micro_width)),
            micro_width,
            shift,
        );
        let idx = Self {
            block,
            micro,
            per,
            n,
            heads,
            head_ends,
            samples,
            blocks,
            micros,
            data,
            tables,
            shard,
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
        // The two start arrays interleave: a block opens on its restart run, then its microblocks
        // in order, and the last of them closes where the next block opens. One sweep over both
        // says every span is forward and the last one ends the data.
        let out_of_order = IndexError::Format("dict: block table out of order");
        let mut prev = 0;
        for b in 0..nb {
            let start = self.block_start(b);
            if start < prev {
                return Err(out_of_order);
            }
            prev = start;
            for j in 0..self.micros_in(b) {
                let at = self.micro_start(b, j);
                if at < prev {
                    return Err(out_of_order);
                }
                prev = at;
            }
            let end = self.block_end(b) as u64;
            if end < prev {
                return Err(out_of_order);
            }
            prev = end;
        }
        if self.block_start(0) != 0 || prev != self.data.len() as u64 {
            return Err(out_of_order);
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
            assert_eq!(&blob[..4], b"BDX2");
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
    fn keys_of_answers_what_key_answers_in_any_order() {
        let keys = corpus();
        let n = keys.len() as u64;
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            // Ascending and contiguous (the walk), every third (a sparse ascending run),
            // descending (no walk), scattered, and out of range.
            let orders: [Vec<u64>; 5] = [
                (0..n).collect(),
                (0..n).step_by(3).collect(),
                (0..n).rev().collect(),
                (0..n).map(|i| (i * 7919) % n).collect(),
                vec![n, n + 1, u64::MAX, 0, n - 1],
            ];
            for ids in &orders {
                let want: Vec<Option<String>> = ids.iter().map(|&i| idx.key(i)).collect();
                assert_eq!(&idx.keys_of(ids), &want, "block {block}");
            }
            assert!(idx.keys_of(&[]).is_empty());
        }
    }

    #[test]
    fn a_batch_answers_exactly_what_the_keys_answer_one_by_one() {
        let keys = corpus();
        let mut queries = keys.clone();
        queries.extend(probes(&keys));
        assert!(
            queries.len() > LANES * 2,
            "the lane loop must run more than once"
        );
        for block in [1usize, 3, 32, MAX_BLOCK] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            let want: Vec<Option<u64>> = queries.iter().map(|q| idx.id(q)).collect();
            assert_eq!(idx.ids_of(&queries), want, "block {block}");
            // The ragged last lane, at every width that can end one.
            for take in [0usize, 1, LANES - 1, LANES, LANES + 1] {
                assert_eq!(
                    idx.ids_of(&queries[..take]),
                    want[..take].to_vec(),
                    "block {block} take {take}"
                );
            }
        }
        let empty = DictIndex::build(Vec::<&str>::new()).unwrap();
        assert_eq!(
            empty.ids_of(&queries[..LANES + 1]),
            vec![None; LANES + 1],
            "an empty index still answers once per key"
        );
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

    /// Holds the shard size for as long as it is alive, and puts the real rule back after.
    struct Shards;

    impl Shards {
        fn of(blocks: usize) -> Self {
            SHARD_OVERRIDE.with(|c| c.set(blocks));
            Shards
        }
    }

    impl Drop for Shards {
        fn drop(&mut self) {
            SHARD_OVERRIDE.with(|c| c.set(0));
        }
    }

    #[test]
    fn a_table_a_shard_answers_every_key_and_survives_the_blob() {
        let keys = corpus();
        for block in [1usize, 3, 32, 256] {
            for shard in [1usize, 2, 7] {
                let _held = Shards::of(shard);
                let idx = DictIndex::build_with_block(&keys, block).unwrap();
                let want = keys.len().div_ceil(block).div_ceil(shard).max(1);
                assert_eq!(idx.tables.len(), want, "block {block}, shard {shard}");
                assert_eq!(idx.shard, shard);
                check(&idx, &keys);
                let back = DictIndex::from_bytes(&idx.to_bytes()).unwrap();
                assert_eq!(back.tables.len(), want);
                check(&back, &keys);
            }
        }
    }

    #[test]
    fn a_streamed_build_shards_its_tables_the_way_the_sorting_one_does() {
        let keys = corpus();
        let dir = scratch("dictshards");
        let path = dir.join("idx.bdx");
        let feed: Vec<&str> = keys.iter().map(String::as_str).collect();
        for (block, shard) in [(1usize, 3usize), (3, 2), (32, 2), (256, 1)] {
            let _held = Shards::of(shard);
            let want = DictIndex::build_with_block(&keys, block)
                .unwrap()
                .to_bytes();
            DictIndex::build_to_file_runs(&feed, &path, block, || Ok(()), 64).unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                want,
                "block {block}, shard {shard}"
            );
            check(&DictIndex::load(&path).unwrap(), &keys);
        }
    }

    #[test]
    fn the_encoding_does_not_depend_on_how_many_threads_ran() {
        let keys = corpus();
        for block in [1usize, 3, 32] {
            // A thread's range starts inside a shard as often as on one, and the encoder it picks
            // is the one the block's number names, so the split must not move a single byte.
            for shard in [0usize, 2] {
                let _held = Shards::of(shard);
                let want = DictIndex::from_sorted_on(&keys, block, micro_for(block), 1)
                    .unwrap()
                    .to_bytes();
                for threads in [2usize, 5, 64] {
                    let got =
                        DictIndex::from_sorted_on(&keys, block, micro_for(block), threads).unwrap();
                    assert_eq!(
                        got.to_bytes(),
                        want,
                        "block {block}, shard {shard}, {threads} threads"
                    );
                    check(&got, &keys);
                }
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
        // Two entries, the second wide: the header stream holds a byte each, and the wide pair's
        // varints sit at the head of its own suffix rather than between the headers.
        let (mut hdrs, mut sfx) = (Vec::new(), Vec::new());
        put_header(&mut hdrs, &mut sfx, 14, 14);
        sfx.extend_from_slice(&[b'a'; 14]);
        put_header(&mut hdrs, &mut sfx, 15, 3);
        sfx.extend_from_slice(b"bcd");
        assert_eq!(hdrs, [0xEE, 0xFF]);
        assert_eq!(&sfx[14..], &[15, 3, b'b', b'c', b'd']);
        let mut data = hdrs.clone();
        data.extend_from_slice(&sfx);
        let mut entries = Entries::of(&data, 3);
        assert_eq!(entries.head(), Some((14, 14)));
        assert_eq!(entries.piece(14), &[b'a'; 14]);
        assert_eq!(entries.head(), Some((15, 3)));
        assert_eq!(entries.piece(3), b"bcd");
        assert_eq!(entries.head(), None);
        // A stream that stops mid-entry gives short answers, never a panic.
        assert_eq!(Entries::of(&[], 1).head(), None);
        assert_eq!(Entries::of(&[0xFF, 15], 2).head(), None);
        let mut cut = Entries::of(&[0x0A, b'x'], 2);
        assert_eq!(cut.head(), Some((0, 10)));
        assert_eq!(cut.piece(10), b"x");
        let mut past = Entries::of(&[0x0A, b'x'], 2);
        assert_eq!(past.head(), Some((0, 10)));
        past.skip(10);
        assert_eq!(past.piece(1), b"");
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

    /// Where every section of a blob starts, read out of its own header.
    struct Layout {
        head_bases: usize,
        head_deltas: usize,
        samples: usize,
        data: usize,
        block_bases: usize,
        block_deltas: usize,
        micro_bases: usize,
        micro_deltas: usize,
        head_width: u32,
        block_width: u32,
        micro_width: u32,
        nb: usize,
    }

    fn layout(blob: &[u8], block: usize) -> Layout {
        let u64_at = |i: usize| u64::from_le_bytes(blob[i..i + 8].try_into().unwrap()) as usize;
        let (n, nb) = (u64_at(4), u64_at(4).div_ceil(block));
        let table_len = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
        let (head_width, block_width, shift, micro_width) = (
            u32::from(blob[44]),
            u32::from(blob[45]),
            u32::from(blob[46]),
            u32::from(blob[47]),
        );
        let micro = u16::from_le_bytes(blob[48..50].try_into().unwrap()) as usize;
        assert_eq!(shift, offsets::SHIFT);
        assert_eq!(micro, micro_for(block));
        assert!(head_width > 0 && block_width > 0, "a corpus with no spread");
        let nm = match nb {
            0 => 0,
            _ if micro >= block => 0,
            nb => (nb - 1) * block.div_ceil(micro) + (n - (nb - 1) * block).div_ceil(micro),
        };
        let head_bases = HEADER + table_len + u64_at(16);
        let head_deltas = head_bases + offsets::bases_len(nb, shift);
        let samples = head_deltas + offsets::deltas_len(nb, head_width);
        let data = samples + nb * 8;
        let block_bases = data + u64_at(24);
        let block_deltas = block_bases + offsets::bases_len(nb, shift);
        let micro_bases = block_deltas + offsets::deltas_len(nb, block_width);
        let micro_deltas = micro_bases + offsets::bases_len(nm, shift);
        assert_eq!(
            micro_deltas + offsets::deltas_len(nm, micro_width),
            blob.len()
        );
        Layout {
            head_bases,
            head_deltas,
            samples,
            data,
            block_bases,
            block_deltas,
            micro_bases,
            micro_deltas,
            head_width,
            block_width,
            micro_width,
            nb,
        }
    }

    /// The base entry `i` is measured from, as it stands and as it is set.
    fn base_of(blob: &[u8], at: usize, i: usize) -> u64 {
        let at = at + (i >> offsets::SHIFT) * 8;
        u64::from_le_bytes(blob[at..at + 8].try_into().unwrap())
    }

    fn set_base(blob: &mut [u8], at: usize, i: usize, v: u64) {
        let at = at + (i >> offsets::SHIFT) * 8;
        blob[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// Entry `i`'s delta, in place, so an edit names one entry rather than a run of bytes.
    fn set_delta(blob: &mut [u8], at: usize, width: u32, i: usize, v: u64) {
        let bit = i * width as usize;
        let (at, off) = (at + bit / 8, bit % 8);
        let mask = ((1u64 << width) - 1) << off;
        let word = u64::from_le_bytes(blob[at..at + 8].try_into().unwrap());
        blob[at..at + 8].copy_from_slice(&(((word & !mask) | ((v << off) & mask)).to_le_bytes()));
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
        let heads_len = u64::from_le_bytes(blob[16..24].try_into().unwrap()) as usize;
        let l = layout(&blob, 2);
        let nb = l.nb;

        refused(&blob[..HEADER - 1], "truncated");
        let mut b = blob.clone();
        b[..4].copy_from_slice(b"BCL1");
        refused(&b, "bad magic");
        let mut b = blob.clone();
        b[4] ^= 1;
        refused(&b, "header checksum");
        let mut b = blob.clone();
        b[l.data] ^= 1;
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
        edited(&|b| b[44] = 57, "offset widths out of range");
        edited(&|b| b[46] = 32, "offset widths out of range");
        edited(&|b| b[47] = 57, "offset widths out of range");
        edited(
            &|b| b[48..50].copy_from_slice(&0u16.to_le_bytes()),
            "microblock size out of range",
        );
        edited(
            &|b| b[48..50].copy_from_slice(&3u16.to_le_bytes()),
            "microblock size out of range",
        );
        edited(
            &|b| b[50..52].copy_from_slice(&0u16.to_le_bytes()),
            "symbol-table shard out of range",
        );
        edited(&|b| b[HEADER] = 255, "bad symbol table");
        edited(&|b| b[HEADER + 1] = 0, "bad symbol table");
        edited(
            &|b| set_base(b, l.head_bases, 0, u64::MAX),
            "head table out of order",
        );
        edited(
            &|b| {
                // The last superblock a byte lower: still in order, one byte short of the heads.
                let base = base_of(b, l.head_bases, nb - 1);
                set_base(b, l.head_bases, nb - 1, base - 1);
            },
            "does not cover",
        );
        edited(&|b| b[l.samples] ^= 1, "head samples");
        edited(
            &|b| set_base(b, l.block_bases, 0, 1),
            "block table out of order",
        );
        edited(
            &|b| set_delta(b, l.block_deltas, l.block_width, 1, u64::MAX),
            "block table out of order",
        );
        edited(&|b| b.push(0), "add up");
        // A block of two is one microblock, so this blob stores no microblock starts; the two
        // edits to that array need a block that has one.
        assert_eq!(
            l.micro_bases,
            blob.len(),
            "no microblock starts at one microblock a block"
        );
        let blob = DictIndex::build_with_block(&keys, 64).unwrap().to_bytes();
        let l = layout(&blob, 64);
        assert!(
            l.micro_bases < blob.len(),
            "two microblocks a block store their starts"
        );
        let edited = |edit: &dyn Fn(&mut Vec<u8>), what: &str| {
            let mut b = blob.clone();
            edit(&mut b);
            reframe(&mut b);
            refused(&b, what);
        };
        edited(
            &|b| set_base(b, l.micro_bases, 0, u64::MAX),
            "block table out of order",
        );
        edited(
            &|b| set_delta(b, l.micro_deltas, l.micro_width, 1, u64::MAX),
            "block table out of order",
        );

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
        let (block, micro) = (64, micro_for(64));
        assert!(
            micro < block && keys.len() > micro,
            "two microblocks in the first block"
        );
        let blob = DictIndex::build_with_block(&keys, block)
            .unwrap()
            .to_bytes();
        let data_len = u64::from_le_bytes(blob[24..32].try_into().unwrap()) as usize;
        let data_at = layout(&blob, block).data;
        for fill in [0x00u8, 0x0F, 0xF0, 0xFE, 0xFF] {
            let mut b = blob.clone();
            b[data_at..data_at + data_len].fill(fill);
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
        // A header whose lcp exceeds the key so far, with the rest of the block intact. A block
        // opens on its restarts, so this one breaks the walk at the second microblock and the
        // first still answers.
        let mut b = blob.clone();
        b[data_at] = 0xF0 | (b[data_at] & 0x0F);
        reframe(&mut b);
        let idx = DictIndex::from_bytes(&b).unwrap();
        assert_eq!(idx.key(0).as_deref(), Some(keys[0].as_str()));
        let _ = idx.key(1);
        assert_eq!(idx.iter().count(), micro);
        assert_eq!(idx.key(micro as u64), None);
    }

    /// The mapping's loader takes the per-block arrays as they are, so every access bounds them:
    /// an end out of order or past the heads, a start past the data, a sample that is not its
    /// head's — wrong answers and short walks, never a panic, and the sections written back as
    /// they were read.
    #[test]
    fn a_load_without_the_walk_bounds_what_the_arrays_say() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 4).unwrap().to_bytes();
        let l = layout(&blob, 4);
        let nb = l.nb;
        type Edit<'a> = &'a dyn Fn(&mut Vec<u8>);
        let edits: [Edit; 8] = [
            &|b| set_base(b, l.head_bases, 0, u64::MAX),
            &|b| {
                let base = base_of(b, l.head_bases, nb - 1);
                set_base(b, l.head_bases, nb - 1, base - 1);
            },
            &|b| set_delta(b, l.head_deltas, l.head_width, nb / 2, u64::MAX),
            &|b| b[l.samples..l.samples + 8].fill(0xFF),
            &|b| set_base(b, l.block_bases, 0, u64::MAX),
            &|b| set_delta(b, l.block_deltas, l.block_width, nb / 2, u64::MAX),
            &|b| set_base(b, l.block_bases, nb - 1, 0),
            &|b| set_delta(b, l.block_deltas, l.block_width, nb - 1, u64::MAX),
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
