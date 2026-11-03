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
//! `id` is a binary search over the samples, then, where a run of blocks shares the probe's sample,
//! a binary search of their heads or, over a long run, a descent of the run's trie of words past
//! what its heads share and one head compare ([`ties`](crate::ties)), then one block scanned
//! without decoding anything: an entry's stored suffix is compared against the probe symbol by
//! symbol, and the shared-prefix length says on its own when the probe has been passed. `key(id)`
//! is the block's head plus the entries between it and the id whose shared-prefix length strictly
//! increases — a monotonic stack over the headers finds them, and only those are decoded, each one
//! eight-byte store per code. There are no automata, so a fuzzy query is a `StringIndex` question;
//! prefix and range are two order lookups and a walk, which this index answers itself at 3–4 bytes
//! per key.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::charcode::{self, CharCode, Probe, Tally};
use crate::extsort::{RUN_BYTES, Replay, Run, Runs};
use crate::fsst::{self, ESCAPE, Table};
use crate::offsets::{self, Offsets};
use crate::packed::{self, Alphabet};
use crate::paircode::{self, Code};
use crate::phrase::{self, Dict, Split, Trie};
use crate::room::{Room, commit};
use crate::ties::{MIN_RUN, Ties};
use std::cmp::Ordering;
use std::sync::OnceLock;

mod build;
mod codec;
mod encode;
mod format;
mod query;
mod route;
#[cfg(test)]
mod tests;

pub(crate) use build::encode_all;
pub use format::{DictLayout, DictProfile, DictSections};
pub(crate) use route::sample_at;
// What a child defines and a sibling uses is imported here, so that every child's
// `use super::*` sees it.
use build::READ_AHEAD;
use codec::{
    Codec, CodedRun, Entries, LabelledCmp, PackedCmp, Piece, SymbolsCmp, TieredCmp, scan_as,
    shard_bits_of, split_run,
};
use encode::{
    Collected, Part, Phrases, Scratch, Shard, arrays_len, build_pieces, codecs_len, codes_len,
    collect_block, encode_range, mine_phrases, packed_micros, packed_offsets, rebase_micros,
    train_shards, train_threads, write_codecs, write_codes,
};
use route::{RawRoute, common_head, common_head_raw, head_of, past_equal, samples_of};

/// `[magic 4][n u64][block u32][heads u64][data u64][codecs u32][payload u64][head width u8]
/// [block width u8][superblock shift u8][micro width u8][micro u16][shard u16][codes u32]
/// [g u16][phrases u32][chars u32][reserved u16][check u32]`, then the head keys end to end, the
/// head ends packed ([`offsets`]), the block data, the suffix codecs — one per
/// `shard` blocks, each behind its own `u32` length, so a reader that knows how many there are
/// walks them without a directory — the header codes, two per shard and each self-delimiting
/// ([`paircode`]), the phrase dictionary the whole blob shares ([`phrase`](crate::phrase)), the
/// block and microblock starts packed, and last the character code the keys are spelled in
/// ([`charcode`]), which only a `BDX4` blob holds.
///
/// Everything but the keys themselves comes after the data, because none of it is known until the
/// data is encoded: the starts' widths, the code a shard's entries were written under, and the
/// codec its suffixes were. That is what lets a streamed build write every section once, in order.
pub(crate) const MAGIC: &[u8; 4] = b"BDX3";

/// A `BDX3` blob whose keys are spelled in a [character code](charcode), which it stores last and
/// whose length is the header's `chars`. A magic of its own rather than a flag in `BDX3`'s
/// reserved bytes, which 4.2 did not read: it would have compared a query it never coded against
/// coded keys and answered wrongly, where a new magic is refused by name. A blob without a code is
/// still `BDX3`, byte for byte what 4.2 wrote, and 4.2 still reads it.
const CODED_MAGIC: &[u8; 4] = b"BDX4";

/// A `BDX3` blob whose dictionary names trie labels past its phrases (`crate::labels`): the
/// dictionary section is the phrases behind their own length, then the labels as a nested `BDX3`
/// blob. A magic of its own because a reader that did not know the labels would refuse the ids
/// that name them only one lookup at a time; it refuses the blob by name instead. Never coded.
const LABELLED_MAGIC: &[u8; 4] = b"BDX5";

/// The formats this one replaces, with what a loader says about each. A refusal names what wrote
/// the blob, because a bare "bad magic" sends someone hunting for disk corruption when the file is
/// intact and merely old.
const LEGACY_MAGIC: [(&[u8; 4], &str); 2] = [
    (
        b"BDX1",
        "dict: blob written by lexindex 2.0 or 2.1, whose blocks interleaved an entry's header \
         with its suffix; rebuild the index from its keys",
    ),
    (
        b"BDX2",
        "dict: blob written by lexindex 2.2 or 3.x, which spent a byte on every entry's header; \
         rebuild the index from its keys",
    ),
];

pub(crate) const HEADER: usize = 72;

const CHECKED: usize = 68; // header bytes the trailing check covers

pub(crate) const DEFAULT_BLOCK: usize = 256;

const MAX_BLOCK: usize = 1024;

/// How deep a key's staircase of shared prefixes may be before [`DictIndex::key_bytes_into`] gives
/// up tracking it and decodes every entry instead. The dictionary reaches 12 at the largest block
/// and a path list 18, so the bound is slack; a block holding a chain like `a`, `aa`, `aaa` is what
/// reaches it, which is legal input and merely slow, not wrong.
const STAIRS: usize = 32;

/// Where each stair's suffix sits, not the suffix itself: most entries are popped again, and a
/// span is two words to record where a slice is two words to build and bound.
///
/// Left uninitialised because a walk keeps a handful of stairs and clearing the array cost sixty
/// instructions of every lookup — a fifth of what a lookup that reads no header at all costs.
type Stairs = [std::mem::MaybeUninit<(usize, usize, usize)>; STAIRS];

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

/// Suffixes a shard prices its candidate splits on. A split is settled on a sample rather than on
/// the shard, because settling on one means coding every suffix of the shard a second time.
const SETTLE_PIECES: usize = 8_192;

/// Keys below which no phrase is mined. A phrase is stored once and named by every key that holds
/// it, so a small index can never earn back a dictionary — and this is what keeps a build of a few
/// thousand keys paying nothing for the miner at all.
const MINE_MIN: usize = 1 << 14;

/// Keys per candidate the miner carries into its last round, which is what the vocabulary is
/// chosen out of rather than the vocabulary itself. The dictionary is paid once and named by every
/// key, so ten times the keys afford roughly ten times the candidates.
const KEYS_PER_PHRASE: usize = 8;

/// Blocks a thread must be given before the encoding is worth splitting across threads at all.
const PARALLEL_BLOCKS: usize = 64;

/// Lookups a batched [`DictIndex::ids_of`] keeps in flight at once.
const LANES: usize = 32;

/// An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways.
///
/// Ids are ranks. `id(key)` is the number of keys below it, `key(id)` the key at that rank, and
/// [`lower_bound`](Self::lower_bound) the rank a key would have, so every range of keys is a
/// range of ids. 2.64 bytes per key on real words, against 5.95 for the transducer of
/// [`StringIndex`](crate::StringIndex) and 10.9 for [`PerfectHashIndex`](crate::PerfectHashIndex);
/// `id` costs a few hundred nanoseconds and `key` about two hundred, both dominated by the scan of
/// one microblock, whose size follows the `block` given at build time.
///
/// Immutable once built, and built in memory: the keys are sorted and deduplicated, then encoded
/// block by block. Persisted with [`to_bytes`](Self::to_bytes) / [`save`](Self::save) and read
/// back by [`from_bytes`](Self::from_bytes) / [`load`](Self::load), which check every length and
/// both checksums, or by `load_mmap`, which borrows the keys and the block data from the mapped
/// file and builds only the per-block samples.
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
    /// The one section a mapping does not borrow. A binary search over it opens every lookup,
    /// and `<[u64]>::partition_point` is the only form of that search that keeps its steps out of
    /// the branch predictor: it selects with `hint::select_unpredictable`, which needs a `u64`
    /// slice and so an alignment a section of a blob does not have. Reading the words out of the
    /// bytes instead measured 110 ns against 26 for the two searches — a hand-written branchless
    /// step is folded back into a branch by the compiler, and blocking that with `black_box` pays
    /// the same back in instructions.
    ///
    /// Derived at load rather than stored: a sample is eight bytes of a head, and the heads are in
    /// the blob already — what the section bought was contiguity, and contiguity is what a load
    /// can build. Eight bytes a block off every blob: 2.666 bytes a key to 2.635 on words at the
    /// default block, 1.099 to 1.037 on a million numeric keys at 128. A mapped index holds the
    /// array and borrows everything else, which it did when the array was read rather than built,
    /// so neither the load nor a lookup moved — five corpora, load within 0.9 % and `id` and `key`
    /// within 3 %, both ways.
    samples: Vec<u64>,
    /// What places a probe among the heads of a long run of equal samples, derived from the heads
    /// the way the samples are and held in no blob. See [`Ties`]. Boxed, so that an index with no
    /// such run carries eight bytes for it rather than a hundred and twenty.
    ties: Option<Box<Ties>>,
    /// Where block `b`'s restart stream starts in `data`, packed the same way; its microblocks
    /// follow it.
    blocks: Offsets,
    /// Where each microblock's own front-coded entries start in `data`, packed the same way. One
    /// entry per microblock, which at the default block is one per thirty-two keys.
    micros: offsets::Packed,
    data: SharedBytes,
    /// One suffix codec per shard of `shard` blocks; the codec for block `b` is `codecs[b /
    /// shard]`, and `codecs` is never empty.
    codecs: Vec<Codec>,
    /// The two header codes of each shard, the microblocks' first: one shard's entries are coded
    /// once for all of its blocks, which is what pays for a code wider than a byte. As long as
    /// `tables`, and indexed the same way.
    codes: Vec<(Code, Code)>,
    /// Blocks one symbol table covers. At least one, and at most what the header's `u16` holds.
    shard: usize,
    /// `shard`'s log when it is a power of two, as every build writes it, and `u32::MAX` when a
    /// blob says otherwise: the shard of a block is then a shift rather than a 64-bit `div`, which
    /// sat on a lookup's dependent path twice — the block comes out of the search, and its codec
    /// is needed before the scan can start.
    shard_bits: u32,
    /// The phrases the blob's shards name, shared by all of them: empty on a corpus whose shards
    /// all settled on their tables alone, which is what [`phrase::mine`] returns when nothing
    /// repeats often enough to earn its dictionary bytes.
    phrases: Dict,
    /// Bytes every block head shares, which the sample is taken past.
    ///
    /// A million URLs all begin `https://`, so a sample of their first eight bytes is the same word
    /// for every block and the search that opens a lookup answers nothing: 3 906 of 3 907 samples
    /// were duplicates and an `id` compared 11.95 heads. Taken at `g` the duplicates fall to 165
    /// and the comparisons to 1.17. Two bytes a blob and none a block, and the search over the
    /// samples is the same search — what changes is which eight bytes it reads.
    g: usize,
    /// The code every key is spelled in, where one paid: every section above holds coded bytes,
    /// a query is coded before it is compared, and a key decoded after it is read. Boxed, so that
    /// a blob without one — every Latin corpus — carries eight bytes for it rather than eighty.
    code: Option<Box<CharCode>>,
    /// The samples again, taken from the heads as they decode: a coded blob's lookups route on the
    /// query's own bytes, before it is spelled, where these separate the blocks as well as the
    /// coded samples do. Derived at load, like the samples; `None` on every other blob.
    raw: Option<Box<RawRoute>>,
    /// `per` words a block: the bytes every restart key of the block shares with its head, then
    /// the eight bytes past them of each restart key, the way a sample is taken past `g` —
    /// [`NO_ROUTE`] in the first word of a block whose restarts did not decode, and `u64::MAX`
    /// past the last block's restarts. A lookup picks its microblock off these and walks the
    /// restart run only where the probe's word ties one. Unset until
    /// [`route_microblocks`](Self::route_microblocks) derives them, and empty where every block is
    /// one microblock.
    ///
    /// Behind a `OnceLock` rather than a `&mut self` setter because a `DictIndex` is shared
    /// wherever it is served — the Python object and a `HashedDictIndex` both hold it in an `Arc` —
    /// and a lookup racing the derivation walks the restart run until the words are in.
    mroute: OnceLock<Box<[u64]>>,
}

/// The first word of a block's [`DictIndex::mroute`] group that leaves the block to its restart
/// run.
const NO_ROUTE: u64 = u64::MAX;

/// Keys per microblock for a block of `block`: its smallest divisor at or above the square root of
/// the block, between 16 and 32, or the block itself when it has none.
///
/// A lookup scans one restart an earlier microblock plus one entry of its own, `block / micro +
/// micro − 2` in all, and the square root of the block minimises that count. Under `BDX2` that was
/// the wrong thing to minimise: a restart entry costs about four ordinary ones, its suffix being
/// coded against a key a microblock away rather than its neighbour, and a nibble header was cheap
/// enough that the bytes won — measured over block ∈ {128, 256, 512} × micro ∈ {8, 16, 32, 64} on
/// real words (`local/dictbench`, 2026-09-12), 32 stored 0.08 B/key less than 16 at every block
/// for 0–7 ns on `id`, buying bytes at 1.0–1.9 mB/ns.
///
/// A coded header is dearer to read than a nibble and the balance moves with it. Measured over
/// twelve corpora at block 256 (`local/dictprof`, 2026-09-19): 16 is faster than 32 on **both**
/// lanes of **all twelve**, by 1–10 % on `id` and 2–19 % on `key`, for 1–4 % more bytes — and the
/// bytes are still under `BDX2`'s everywhere, 0.50 to 0.95 of them. That is 0.1–0.4 mB/ns, an
/// order of magnitude below the rate the cap of 32 was set at, so the rule now follows the count.
///
/// A divisor keeps every microblock of a block full but the last, which is what makes a restart's
/// rank `j * micro` rather than a running sum. The floor of 16 is what keeps a small block from
/// paying a run's prologue every few keys: a block of 32 holds 96 bytes of words and each run
/// opens with about four, so the square root's micro of 8 would have added 17 % where the same
/// step at 256 adds 4 — enough to put a block of 32 *above* `BDX2`, which is the one thing the
/// format may not do. A block of 16 or fewer, or a prime one, is a single microblock, the layout
/// of one level.
pub(crate) fn micro_for(block: usize) -> usize {
    let from = block.isqrt().clamp(16, 32);
    (from..=32)
        .find(|d| block % d == 0)
        .or_else(|| (2..=32).rev().find(|d| block % d == 0))
        .unwrap_or(block)
}

#[cfg(test)]
thread_local! {
    /// Blocks a shard covers while a test is running; zero is the real rule. A second table exists
    /// only past 65 536 keys, which is more than a unit test should have to build.
    static SHARD_OVERRIDE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Blocks one symbol table covers, for a given block size: [`SHARD_KEYS`] keys' worth, at least one
/// block and never more than the header's `u16` can name. Read once, on the thread that builds.
pub(crate) fn shard_blocks_for(block: usize) -> usize {
    #[cfg(test)]
    {
        let over = SHARD_OVERRIDE.with(std::cell::Cell::get);
        if over != 0 {
            return over;
        }
    }
    (SHARD_KEYS / block).clamp(1, u16::MAX as usize)
}

/// Threads a build of `n` keys encodes on: one per [`PARALLEL_BLOCKS`] blocks, at most the
/// machine's.
pub(crate) fn build_threads(n: usize, block: usize) -> usize {
    (n.div_ceil(block) / PARALLEL_BLOCKS)
        .min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
        .max(1)
}

/// A key as the encoder reads it: its bytes. Every builder takes `str`s, which is what the blanket
/// implementation covers; a key under a character code is bytes that are not UTF-8.
pub(crate) trait AsKey {
    fn key_bytes(&self) -> &[u8];
}

impl<S: AsRef<str> + ?Sized> AsKey for S {
    #[inline(always)]
    fn key_bytes(&self) -> &[u8] {
        self.as_ref().as_bytes()
    }
}

/// A key given as its bytes.
#[derive(Clone, Copy)]
pub(crate) struct Bytes<'a>(pub(crate) &'a [u8]);

impl AsKey for Bytes<'_> {
    #[inline(always)]
    fn key_bytes(&self) -> &[u8] {
        self.0
    }
}

/// How many leading bytes `a` and `b` share.
///
/// Whole words first. The tail under eight bytes was a byte loop whose exit no predictor learns;
/// it is now a few overlapping loads placed at their own offsets in one word, so that the first
/// differing byte is one `trailing_zeros`: the last eight bytes of each slice where both run to
/// eight — the bytes read twice are equal, so their `xor` is zero —, four bytes at `0` and at
/// `n - 4` for a shorter slice of four or more, and its first, middle and last byte under four.
#[inline]
pub(crate) fn lcp(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = fsst::word_at(a, i) ^ fsst::word_at(b, i);
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    if i == n {
        return n;
    }
    let (at, x) = if n >= 8 {
        (n - 8, fsst::word_at(a, n - 8) ^ fsst::word_at(b, n - 8))
    } else if n >= 4 {
        let head = u64::from(quad_at(a, 0) ^ quad_at(b, 0));
        let tail = u64::from(quad_at(a, n - 4) ^ quad_at(b, n - 4));
        (0, head | tail << (8 * (n - 4)))
    } else {
        let byte = |k: usize| u64::from(a[k] ^ b[k]) << (8 * k);
        (0, byte(0) | byte(n / 2) | byte(n - 1))
    };
    if x == 0 {
        n
    } else {
        at + (x.trailing_zeros() / 8) as usize
    }
}

/// The four bytes at `s[i..]` as a little-endian word; the caller has them in bounds.
#[inline(always)]
fn quad_at(s: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(s[i..i + 4].try_into().unwrap())
}

/// The keys an [`encode_all`] arena holds, one view each.
pub(crate) fn views<'a>(arena: &'a [u8], ends: &[usize]) -> Vec<Bytes<'a>> {
    let mut at = 0;
    ends.iter()
        .map(|&end| {
            let key = Bytes(&arena[at..end]);
            at = end;
            key
        })
        .collect()
}

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: usize) {
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

/// The varint at `*at` in `data`, advancing `*at` past it. The one-byte case is the run of a
/// front-coded block -- a base under 128, a length under 128 -- and it is the one a scan pays for.
#[inline(always)]
pub(crate) fn varint_at(data: &[u8], at: &mut usize) -> Option<usize> {
    if let Some(&b) = data.get(*at) {
        if b < 0x80 {
            *at += 1;
            return Some(usize::from(b));
        }
    }
    let (v, rest) = get_varint(data.get(*at..)?)?;
    *at = data.len() - rest.len();
    Some(v)
}

impl DictIndex {
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

    /// Derive the words that send a lookup straight to its microblock, once, and return the bytes
    /// they take.
    ///
    /// A lookup finds its block off the samples, then walks the block's restart run — the first
    /// key of each microblock, front-coded — to the one microblock it scans. Routed, it compares
    /// eight bytes of every restart key at once instead, and walks the run only for a probe whose
    /// eight bytes tie a restart's. `id`, `contains`, `lower_bound`, `ids_of` and the order queries
    /// all go that way. Against the same index unrouted, in one process at the default block, `id`
    /// took 19–31 % less time on twelve corpora of thirteen at a million keys and 13–22 % on the
    /// six measured at ten million, and 23–36 % and 14–28 % at 1024 keys a block. At 32, two
    /// microblocks a block, it saved at most 15 %, and cost `numeric` at ten million 3–10 %.
    /// `paths`, whose restart keys tie in their eight bytes, did not move.
    ///
    /// No load does this, because what it buys is paid in memory and at load. The words are eight
    /// bytes a restart, `8 / micro` a key — 0.5 at the default block, 0.25 at 1024 — held beside
    /// the index and in no blob, so a routed index is that much larger than
    /// [`serialized_len`](Self::serialized_len). Deriving them decodes the start of every
    /// restart, about fifteen instructions a key, and so reads nearly every page of a file
    /// `load_mmap` borrows. A second call derives nothing; a lookup that runs on another thread
    /// before the words are in walks the restart run.
    pub fn route_microblocks(&self) -> usize {
        let words = self
            .mroute
            .get_or_init(|| self.micro_route().into_boxed_slice());
        size_of_val::<[u64]>(words)
    }

    /// Number of blocks.
    #[inline(always)]
    fn blocks_len(&self) -> usize {
        self.samples.len()
    }

    /// The shard block `b` is in.
    #[inline(always)]
    fn shard_of(&self, b: usize) -> usize {
        if self.shard_bits < usize::BITS {
            b >> self.shard_bits
        } else {
            b / self.shard
        }
    }

    /// The suffix codec block `b` was encoded under.
    #[inline(always)]
    fn codec_of(&self, b: usize) -> &Codec {
        let s = self.shard_of(b);
        &self.codecs[if s < self.codecs.len() { s } else { 0 }]
    }

    /// The header codes block `b`'s entries were written under, the microblocks' first.
    #[inline(always)]
    fn codes_of(&self, b: usize) -> &(Code, Code) {
        let s = self.shard_of(b);
        &self.codes[if s < self.codes.len() { s } else { 0 }]
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
    ///
    /// The array holds offsets inside the block — see [`rebase_micros`] — so the block's own start
    /// is the base every read adds.
    #[inline(always)]
    fn micro_start(&self, b: usize, j: usize) -> u64 {
        if self.per == 1 {
            self.blocks.at(b)
        } else {
            self.blocks
                .at(b)
                .wrapping_add(self.micros.at(b * self.per + j))
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
        self.restart_data_at(b, self.blocks.at(b))
    }

    /// [`restart_data`](Self::restart_data) with the block's start already in hand, for the lookup
    /// path, which reads it once and hands it to both this and [`micro_data_at`](Self::micro_data_at).
    #[inline(always)]
    fn restart_data_at(&self, b: usize, base: u64) -> &[u8] {
        let data: &[u8] = &self.data;
        let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        let end = if self.per == 1 {
            base
        } else {
            base.wrapping_add(self.micros.at(b * self.per))
        };
        let (start, end) = (at(base), at(end));
        data.get(start..end).unwrap_or_default()
    }

    /// Microblock `j` of block `b`, bounded the same way. The last microblock of a block ends where
    /// the block does, not where the next block's restarts start.
    #[inline(always)]
    fn micro_data(&self, b: usize, j: usize) -> &[u8] {
        self.micro_data_at(b, j, self.blocks.at(b))
    }

    /// [`micro_data`](Self::micro_data) with the block's start already in hand.
    #[inline(always)]
    fn micro_data_at(&self, b: usize, j: usize, base: u64) -> &[u8] {
        let data: &[u8] = &self.data;
        let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        let (start, end) = if j + 1 < self.micros_in(b) {
            let (start, end) = self.micros.pair(b * self.per + j);
            (at(base.wrapping_add(start)), at(base.wrapping_add(end)))
        } else {
            let start = if self.per == 1 {
                base
            } else {
                base.wrapping_add(self.micros.at(b * self.per + j))
            };
            (at(start), self.block_end(b))
        };
        data.get(start..end).unwrap_or_default()
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
