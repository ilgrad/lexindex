//! The trie labels a `BDX5` dictionary names past its phrases.
//!
//! Front coding takes the head a key shares with its predecessor; what is left, on a path list, is
//! a run of patricia-trie edge labels — directory and file names under different parents — and
//! most of those labels occur many times. Phrases name spans of up to [`phrase::MAX`] bytes that
//! the sampled miner sees at least ten times; a label used two to nine times, or longer than a
//! phrase, is coded by symbols at every occurrence. On a million paths the second and later
//! occurrences of repeated labels are 49 % of every byte front coding stores.
//!
//! A labelled blob stores each such label once and names it with the reference a phrase takes. The
//! labels are a nested [`DictIndex`] at [`BLOCK`] keys a block, each label reversed behind a tier
//! byte: reversal makes labels that end alike (`/index.html`, `.py`) share a prefix the nested
//! front coding takes out, and the tier puts the most used labels first, so that they take the
//! two-byte references. A label's id is its rank there, past the blob's phrases.
//!
//! Decoding a label costs what a nested `key` does, so the labels a lookup reads most are not
//! stored there at all: they become phrases, flat bytes and a direct offset. Those are few and
//! their reads are many, since an entry near the top of a block is read by every key below it
//! that shares its prefix; which they are takes a parse of the blob to know, as the count of an
//! edge says little about the reads of the label the parse really takes.
//!
//! Chosen per blob: the build that tries labels keeps them only where the blob comes out smaller.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::dict_index::{
    AsKey, Bytes, DictIndex, MAGIC, build_threads, micro_for, shard_blocks_for,
};
use crate::extsort::Replay;
use crate::fsst::{Encoder, Table};
use crate::phrase::{self, LABEL_MAX, Map, Trie};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// Keys a block of the nested dictionary holds: a label is at most this many entries' decode
/// away from a stored head.
pub(crate) const BLOCK: usize = 16;
/// Most used labels that sort ahead of the rest, so that the cheap references go to them.
pub(crate) const HOT: usize = 16_384;
/// The tier byte of a label among the [`HOT`] ones, and of every other.
const TIER_HOT: u8 = 1;
const TIER_COLD: u8 = 2;

/// The labels of a blob: the nested index that holds them, and each label's first byte.
///
/// The first bytes are a byte a label kept beside the index, so that a compare which differs from
/// the probe there — half of those a lookup makes on a million paths, since an entry's suffix
/// starts where its key branches — is settled without decoding the label. Serialised as
/// `[index bytes u32][index][first bytes]`.
pub(crate) struct Labels {
    index: DictIndex,
    firsts: SharedBytes,
}

/// A label as the nested dictionary spells it: reversed, with eight bytes of padding behind it.
///
/// Read a word at a time from its far end, so that no byte is moved before it is needed: a compare
/// that stops inside the first word has reversed nothing.
#[derive(Clone, Copy)]
pub(crate) struct Label<'a> {
    stored: &'a [u8],
    len: usize,
}

impl Label<'_> {
    pub(crate) fn len(self) -> usize {
        self.len
    }

    /// The label's bytes from `at`, which is below its length, up to eight of them, as a
    /// little-endian word, and how many there are: what [`crate::dict_index`]'s compare and store
    /// take.
    #[inline(always)]
    pub(crate) fn word(self, at: usize) -> (u64, usize) {
        let left = self.len - at;
        let (start, kept) = if left >= 8 { (left - 8, 8) } else { (0, left) };
        let chunk = self.stored[start..]
            .first_chunk::<8>()
            .expect("the padding is eight bytes");
        (u64::from_be_bytes(*chunk) >> (8 * (8 - kept)), kept)
    }
}

thread_local! {
    /// What a label decodes into before it is read: a lookup that meets one allocates nothing.
    static SCRATCH: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

impl Labels {
    /// Bytes the nested dictionary holds on the heap; its keys and first bytes stay in the blob.
    pub(crate) fn held(&self) -> usize {
        self.index.held().total()
    }

    /// Labels as a blob holds them; `None` on an index this crate would not have written there —
    /// one that names labels of its own, or one in a character code.
    pub(crate) fn of(index: DictIndex, firsts: SharedBytes) -> Option<Self> {
        (!index.has_labels()
            && !index.is_coded()
            && !index.is_empty()
            && firsts.len() == index.len())
        .then_some(Self { index, firsts })
    }

    /// The labels a blob's dictionary section holds after its phrases, borrowed from it.
    pub(crate) fn read(section: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let bad = || IndexError::Format("dict: bad label dictionary");
        let len = section
            .get(..4)
            .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
            .ok_or_else(bad)?;
        let end = len.checked_add(4).ok_or_else(bad)?;
        let nested = section.subslice(4, end).ok_or_else(bad)?;
        // Only a plain dictionary is written here. Refused by its magic before it is parsed, a
        // nested labelled one cannot recurse the reader as deep as a crafted blob's bytes allow.
        if nested.get(..4) != Some(&MAGIC[..]) {
            return Err(bad());
        }
        let index = DictIndex::from_shared(nested, verify)?;
        let firsts = section.subslice(end, section.len()).ok_or_else(bad)?;
        Self::of(index, firsts).ok_or_else(bad)
    }

    /// The first byte of label `rank`; `None` past the labels.
    #[inline(always)]
    pub(crate) fn first(&self, rank: usize) -> Option<u8> {
        self.firsts.get(rank).copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.index.len()
    }

    /// Label `rank` handed to `f`; `None` past the labels or on one this crate did not write.
    #[inline]
    pub(crate) fn with<R>(&self, rank: usize, f: impl FnOnce(Label<'_>) -> R) -> Option<R> {
        SCRATCH.with(|cell| {
            let mut buf = cell.take();
            let got = self.index.key_bytes_into(rank as u64, &mut buf)
                && (2..=LABEL_MAX + 1).contains(&buf.len());
            let out = got.then(|| {
                let len = buf.len() - 1;
                buf.extend_from_slice(&[0; 8]);
                f(Label {
                    stored: &buf[1..],
                    len,
                })
            });
            cell.set(buf);
            out
        })
    }

    pub(crate) fn serialized_len(&self) -> usize {
        4 + self.index.serialized_len() + self.firsts.len()
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.index.serialized_len() as u32).to_le_bytes());
        out.extend_from_slice(&self.index.to_bytes());
        out.extend_from_slice(&self.firsts);
    }
}

/// Whether a label of `len` bytes used `count` times pays for its place in the dictionary: inline
/// the symbols spend about 0.58 bytes a byte, a reference 2.5 bytes, and the nested dictionary
/// about 0.6 bytes a byte plus 3 for the entry (`plans/research/2026-10-30-paths-size.md` §2).
fn pays(len: usize, count: u64) -> bool {
    let (l, c) = (len as f64, count as f64);
    c * (0.58 * l - 2.5) > 0.6 * l + 3.0
}

/// The reads a label has to see for each byte a flat entry costs over a nested one, in one lookup
/// of every key: the same on every corpus and at every size, since bytes and reads both go by the
/// key. At 16 the blob is at most 0.04 bytes a key larger than one with nothing flat, and at block
/// 1024 smaller on six corpora of eight. At 8 domain names grow past the blob with nothing flat by
/// more than the labels' reads repay.
const READS_A_BYTE: f64 = 16.0;

/// Whether a label of `len` bytes read `reads` times earns a phrase's flat entry, `len + 2` bytes
/// against a nested label's `0.6 len + 3`.
fn earns_flat(len: usize, reads: u64) -> bool {
    len <= phrase::MAX && reads as f64 >= READS_A_BYTE * (0.4 * len as f64 - 1.0).max(1.0)
}

/// What a blob's parse does with a label: how many times it names the label, and how many entries'
/// worth of lookups then read it, in one lookup of every key.
#[derive(Default)]
struct Usage {
    picks: AtomicU64,
    reads: AtomicU64,
}

/// The [`Usage`] of each of `kept`, from the cheapest coding of every entry of the blob, in the
/// entries' own runs. A token at depth `a` of entry `i` is read by the keys of the entry's scope
/// from `i` on that share more than `a` bytes with it: a microblock's entry by that microblock, a
/// restart by its whole block. A key that shares exactly `a` is told apart by the label's first
/// byte, which costs no decode.
struct Parse {
    trie: Trie,
    uses: Vec<Usage>,
    phrases: usize,
    micro: usize,
}

impl Parse {
    fn over(phrases: &[Vec<u8>], kept: &[(&[u8], u32)], micro: usize) -> Self {
        Self {
            trie: Trie::of(
                phrases
                    .iter()
                    .map(Vec::as_slice)
                    .chain(kept.iter().map(|&(l, _)| l)),
            ),
            uses: kept.iter().map(|_| Usage::default()).collect(),
            phrases: phrases.len(),
            micro,
        }
    }

    /// One block's picks: `keys` are its keys, and `lcps[i]` what key `i` shares with key `i - 1`,
    /// so `lcps[0]` is not read. Nothing outside the block decides any of it.
    fn block<K: AsKey>(
        &self,
        keys: &[K],
        lcps: &[usize],
        encoder: &Encoder,
        w: &mut phrase::Scratch,
    ) {
        let (end, micro) = (keys.len(), self.micro);
        for i in 1..end {
            let (shared, scope) = if i % micro == 0 {
                let shared = lcps[i + 1 - micro..=i].iter().copied().min();
                (shared.unwrap_or(0), end)
            } else {
                (lcps[i], ((i / micro + 1) * micro).min(end))
            };
            let suffix = &keys[i].key_bytes()[shared..];
            phrase::picks(suffix, encoder, &self.trie, w, |at, id| {
                let Some(used) = id.checked_sub(self.phrases).map(|rank| &self.uses[rank]) else {
                    return;
                };
                let depth = shared + at;
                let reach = lcps[i + 1..scope].iter().take_while(|&&l| l > depth);
                used.picks.fetch_add(1, Ordering::Relaxed);
                used.reads
                    .fetch_add(1 + reach.count() as u64, Ordering::Relaxed);
            });
        }
    }
}

/// [`Parse`] over `keys`, a range of blocks a thread.
fn usage<K: AsKey + Sync>(
    keys: &[K],
    lcps: &[usize],
    block: usize,
    micro: usize,
    tables: &[Table],
    phrases: &[Vec<u8>],
    kept: &[(&[u8], u32)],
) -> Vec<Usage> {
    let n = keys.len();
    let parsing = Parse::over(phrases, kept, micro);
    let shard = shard_blocks_for(block);
    let blocks = n.div_ceil(block);
    let parse = |range: std::ops::Range<usize>| {
        let mut w = phrase::Scratch::default();
        let mut encoder = None;
        for b in range {
            let table = (b / shard).min(tables.len() - 1);
            if encoder.as_ref().is_none_or(|&(at, _)| at != table) {
                encoder = Some((table, tables[table].encoder()));
            }
            let Some((_, encoder)) = &encoder else {
                unreachable!("set above");
            };
            let start = b * block;
            let end = (start + block).min(n);
            parsing.block(&keys[start..end], &lcps[start..end], encoder, &mut w);
        }
    };
    let threads = build_threads(n, block);
    if threads == 1 {
        parse(0..blocks);
    } else {
        std::thread::scope(|scope| {
            for t in 0..threads {
                let parse = &parse;
                scope.spawn(move || parse(blocks * t / threads..blocks * (t + 1) / threads));
            }
        });
    }
    parsing.uses
}

/// [`Parse`] over a key stream, a block at a time on this thread: a block needs nothing past itself,
/// so the walk holds one block's keys.
fn usage_of_stream<R: Replay>(
    src: &mut R,
    block: usize,
    micro: usize,
    tables: &[Table],
    phrases: &[Vec<u8>],
    kept: &[(&[u8], u32)],
) -> Result<Vec<Usage>, IndexError> {
    let parsing = Parse::over(phrases, kept, micro);
    let shard = shard_blocks_for(block);
    let mut w = phrase::Scratch::default();
    let mut lcps = Vec::new();
    let mut parse_block = |b: usize, bytes: &[u8], ends: &[usize]| {
        let view: Vec<Bytes<'_>> = ends.windows(2).map(|e| Bytes(&bytes[e[0]..e[1]])).collect();
        lcps.clear();
        lcps.push(0);
        lcps.extend(
            view.windows(2)
                .map(|p| crate::dict_index::lcp(p[0].key_bytes(), p[1].key_bytes())),
        );
        let encoder = tables[(b / shard).min(tables.len() - 1)].encoder();
        parsing.block(&view, &lcps, &encoder, &mut w);
    };
    let (mut bytes, mut ends, mut i) = (Vec::new(), vec![0usize], 0usize);
    src.each(&mut |key| {
        if i % block == 0 && i > 0 {
            parse_block(i / block - 1, &bytes, &ends);
            bytes.clear();
            ends.truncate(1);
        }
        bytes.extend_from_slice(key.as_bytes());
        ends.push(bytes.len());
        i += 1;
        Ok(())
    })?;
    if i > 0 {
        parse_block((i - 1) / block, &bytes, &ends);
    }
    Ok(parsing.uses)
}

/// The edges of the patricia trie over a sorted, distinct key stream, off nothing but the key before
/// and the depths on its path: the edges below where the next key leaves that path end there, and
/// every edge ends once. `open` holds the path's depths, ascending from the root's 0.
fn close_edges<'a>(
    open: &mut Vec<usize>,
    last: &'a [u8],
    shared: usize,
    edge: &mut impl FnMut(&'a [u8]),
) {
    while let Some(&deep) = open.last() {
        if deep <= shared {
            break;
        }
        open.pop();
        let from = open.last().map_or(shared, |&up| up.max(shared));
        edge(&last[from..deep]);
    }
    if open.last() != Some(&shared) {
        open.push(shared);
    }
}

/// A key's end opened on the path `open`; a key that is empty ends at the root.
fn open_key(open: &mut Vec<usize>, len: usize) {
    if open.last().is_some_and(|&at| at < len) {
        open.push(len);
    }
}

/// Whether a label of `len` bytes could pay at any count: under five bytes a reference costs what
/// the symbols do, and past [`LABEL_MAX`] no parse looks.
fn may_pay(len: usize) -> bool {
    (5..=LABEL_MAX).contains(&len)
}

/// The labels that pay at `counts`, past those `phrases` holds already, the most used first, ties in
/// byte order, so that the cut at `room` is a function of the keys.
fn admitted<'a>(
    counts: impl Iterator<Item = (&'a [u8], u32)>,
    phrases: &[Vec<u8>],
    room: usize,
) -> Vec<(&'a [u8], u32)> {
    let known: std::collections::HashSet<&[u8]> = phrases.iter().map(Vec::as_slice).collect();
    let mut kept: Vec<(&[u8], u32)> = counts
        .filter(|&(l, c)| pays(l.len(), u64::from(c)) && !known.contains(l))
        .collect();
    kept.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    kept.truncate(room.min(phrase::IDS_MAX));
    kept
}

/// Bits of each of [`Seen`]'s two tables a key buys, at most twice this after rounding up. Past 8 a
/// bit costs about what it spares the count on a million paths, at about 70 bytes an extra label.
const SEEN_BITS_A_KEY: usize = 8;

/// Which labels a pass saw at least twice, with false positives only: what keeps most of the labels
/// that occur once, the bulk of them on any corpus, out of the exact count behind it.
struct Seen {
    once: Vec<u64>,
    twice: Vec<u64>,
}

impl Seen {
    fn for_keys(keys: usize) -> Self {
        let bits = keys
            .saturating_mul(SEEN_BITS_A_KEY)
            .next_power_of_two()
            .max(1 << 16);
        Self {
            once: vec![0; bits / 64],
            twice: vec![0; bits / 64],
        }
    }

    /// The two bits of `label`, each as a word's index and a mask in it.
    #[inline]
    fn places(&self, label: &[u8]) -> [(usize, u64); 2] {
        let hash = crate::blob::hash_bytes(label);
        let words = self.once.len();
        let place = |h: u64| {
            let word = (h >> 6) as usize & (words - 1);
            (word, 1u64 << (h & 63))
        };
        [place(hash), place(hash >> 32)]
    }

    fn add(&mut self, label: &[u8]) {
        let [(a, x), (b, y)] = self.places(label);
        if self.once[a] & x != 0 && self.once[b] & y != 0 {
            self.twice[a] |= x;
            self.twice[b] |= y;
        }
        self.once[a] |= x;
        self.once[b] |= y;
    }

    fn twice(&self, label: &[u8]) -> bool {
        let [(a, x), (b, y)] = self.places(label);
        self.twice[a] & x != 0 && self.twice[b] & y != 0
    }
}

/// The edges of a key stream, walked once to [`sift`](Self::sift) the labels that occur once and
/// once more to [`count`](Self::count) the others: the streamed build's half of [`select`], which
/// holds a label only from the second pass and only if the first saw it twice.
pub(crate) struct EdgeCount {
    last: Vec<u8>,
    open: Vec<usize>,
    seen: Seen,
    counts: Map<Box<[u8]>, u32>,
}

impl EdgeCount {
    pub(crate) fn for_keys(keys: usize) -> Self {
        Self {
            last: Vec::new(),
            open: vec![0],
            seen: Seen::for_keys(keys),
            counts: Map::default(),
        }
    }

    fn walk(last: &mut Vec<u8>, open: &mut Vec<usize>, key: &[u8], edge: &mut impl FnMut(&[u8])) {
        let shared = crate::dict_index::lcp(last, key);
        close_edges(open, last, shared, edge);
        open_key(open, key.len());
        last.clear();
        last.extend_from_slice(key);
    }

    pub(crate) fn sift(&mut self, key: &[u8]) {
        let seen = &mut self.seen;
        Self::walk(&mut self.last, &mut self.open, key, &mut |label| {
            if may_pay(label.len()) {
                seen.add(label);
            }
        });
    }

    /// The first pass is over: the path of its last key has edges still open.
    pub(crate) fn sifted(&mut self) {
        let seen = &mut self.seen;
        close_edges(&mut self.open, &self.last, 0, &mut |label: &[u8]| {
            if may_pay(label.len()) {
                seen.add(label);
            }
        });
        self.last.clear();
    }

    fn tally(seen: &Seen, counts: &mut Map<Box<[u8]>, u32>, label: &[u8]) {
        if !may_pay(label.len()) || !seen.twice(label) {
            return;
        }
        match counts.get_mut(label) {
            Some(count) => *count += 1,
            None => {
                counts.insert(label.into(), 1);
            }
        }
    }

    pub(crate) fn count(&mut self, key: &[u8]) {
        let (seen, counts) = (&self.seen, &mut self.counts);
        Self::walk(&mut self.last, &mut self.open, key, &mut |label| {
            Self::tally(seen, counts, label);
        });
    }

    /// The labels worth a dictionary, as [`select`] chooses them over the same keys: one more walk
    /// of `src`, for the parse's picks. `None` where none pays.
    pub(crate) fn select<R: Replay>(
        mut self,
        src: &mut R,
        phrases: &[Vec<u8>],
        room: usize,
        block: usize,
        micro: usize,
        tables: &[Table],
    ) -> Result<Option<Selected>, IndexError> {
        let (seen, counts) = (&self.seen, &mut self.counts);
        close_edges(&mut self.open, &self.last, 0, &mut |label: &[u8]| {
            Self::tally(seen, counts, label);
        });
        let Self { seen, counts, .. } = self;
        drop(seen);
        let kept = admitted(counts.iter().map(|(l, &c)| (l.as_ref(), c)), phrases, room);
        if room == 0 || kept.is_empty() {
            return Ok(None);
        }
        let uses = usage_of_stream(src, block, micro, tables, phrases, &kept)?;
        Ok(selected_from(&kept, &uses))
    }
}

/// What a dictionary names past its phrases.
pub(crate) struct Selected {
    /// The labels read often enough to be phrases themselves, most used first.
    pub(crate) flat: Vec<Vec<u8>>,
    /// The others in id order, past the flat ones, with the nested index that stores them; `None`
    /// when every label went flat.
    pub(crate) nested: Option<(Vec<Vec<u8>>, Labels)>,
}

/// The labels worth a dictionary over `keys`, sorted and distinct; `None` where none pays.
/// `phrases` are the blob's own, which a label does not repeat, `room` the ids left past them, and
/// `block`, `micro` and `tables` the blob's layout and its shards' symbols.
///
/// An edge's count only admits a label. What keeps it is the parse's own picks: on a million paths
/// a quarter of the admitted labels do not pay for themselves once the parse has chosen, and 18 %
/// are never taken at all, since a phrase or a run of symbols is as cheap.
pub(crate) fn select<K: AsKey + Sync>(
    keys: &[K],
    phrases: &[Vec<u8>],
    room: usize,
    block: usize,
    micro: usize,
    tables: &[Table],
) -> Option<Selected> {
    let n = keys.len();
    if n < 2 || room == 0 {
        return None;
    }
    let key = |i: usize| keys[i].key_bytes();
    // `lcps[i]` is what key `i` shares with key `i - 1`; zero past both ends.
    let mut lcps = vec![0usize; n + 1];
    for (i, slot) in lcps.iter_mut().enumerate().take(n).skip(1) {
        *slot = crate::dict_index::lcp(key(i - 1), key(i));
    }
    let mut counts: Map<&[u8], u32> = Map::default();
    let mut edge = |label| {
        if may_pay(<[u8]>::len(label)) {
            *counts.entry(label).or_insert(0) += 1;
        }
    };
    let mut open = vec![0];
    open_key(&mut open, key(0).len());
    for (i, &shared) in lcps.iter().enumerate().take(n).skip(1) {
        close_edges(&mut open, key(i - 1), shared, &mut edge);
        open_key(&mut open, key(i).len());
    }
    close_edges(&mut open, key(n - 1), 0, &mut edge);
    let kept = admitted(counts.into_iter(), phrases, room);
    if kept.is_empty() {
        return None;
    }
    let uses = usage(keys, &lcps, block, micro, tables, phrases, &kept);
    selected_from(&kept, &uses)
}

/// What the parse's `uses` of `kept` leave: the labels that pay for themselves once it has chosen,
/// the flat ones apart and the others in their nested dictionary.
fn selected_from(kept: &[(&[u8], u32)], uses: &[Usage]) -> Option<Selected> {
    let mut used: Vec<(&[u8], u64, u64)> = kept
        .iter()
        .zip(uses)
        .map(|(&(l, _), u)| {
            let picks = u.picks.load(Ordering::Relaxed);
            (l, picks, u.reads.load(Ordering::Relaxed))
        })
        .filter(|&(l, picks, _)| pays(l.len(), picks))
        .collect();
    if used.is_empty() {
        return None;
    }
    used.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let (flat, rest): (Vec<_>, Vec<_>) = used
        .into_iter()
        .partition(|&(l, _, reads)| earns_flat(l.len(), reads));
    let flat = flat.into_iter().map(|(l, ..)| l.to_vec()).collect();
    if rest.is_empty() {
        return Some(Selected { flat, nested: None });
    }
    let mut stored: Vec<Vec<u8>> = rest
        .iter()
        .enumerate()
        .map(|(rank, &(l, ..))| {
            let mut s = Vec::with_capacity(l.len() + 1);
            s.push(if rank < HOT { TIER_HOT } else { TIER_COLD });
            s.extend(l.iter().rev());
            s
        })
        .collect();
    stored.sort_unstable();
    let index = {
        let views: Vec<Bytes<'_>> = stored.iter().map(|s| Bytes(s)).collect();
        DictIndex::from_sorted_plain(&views, BLOCK, micro_for(BLOCK)).ok()?
    };
    let labels: Vec<Vec<u8>> = stored
        .iter()
        .map(|s| s[1..].iter().rev().copied().collect())
        .collect();
    let firsts = SharedBytes::from_owned(labels.iter().map(|l| l[0]).collect());
    let store = Labels::of(index, firsts)?;
    Some(Selected {
        flat,
        nested: Some((labels, store)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spelled(label: Label<'_>) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < label.len() {
            let (word, kept) = label.word(out.len());
            out.extend_from_slice(&word.to_le_bytes()[..kept]);
        }
        out
    }

    /// With no symbols to spell a byte in, so that the parse takes every label it can.
    fn chosen(keys: &[String], phrases: &[Vec<u8>]) -> Option<Selected> {
        select(
            keys,
            phrases,
            1 << 20,
            256,
            micro_for(256),
            &[Table::train(&[])],
        )
    }

    fn every_label(got: &Selected) -> Vec<Vec<u8>> {
        let mut all = got.flat.clone();
        if let Some((labels, _)) = &got.nested {
            all.extend_from_slice(labels);
        }
        all
    }

    /// The patricia edges as a walk from the right finds them, a key's residual cut at the depths
    /// where a later key branches from it: the definition [`close_edges`] has to agree with.
    fn edges_by_definition(keys: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let n = keys.len();
        let mut lcps = vec![0usize; n + 1];
        for i in 1..n {
            lcps[i] = crate::dict_index::lcp(&keys[i - 1], &keys[i]);
        }
        let (mut stack, mut cuts, mut out) = (Vec::new(), Vec::new(), Vec::new());
        for i in (0..n).rev() {
            let next = lcps[i + 1];
            while stack.last().is_some_and(|&v| v >= next) {
                stack.pop();
            }
            stack.push(next);
            let (d0, k) = (lcps[i], &keys[i]);
            cuts.clear();
            cuts.push(d0);
            cuts.extend(
                stack
                    .iter()
                    .rev()
                    .take_while(|&&v| v > d0)
                    .filter(|&&v| v < k.len()),
            );
            cuts.sort_unstable();
            if cuts.last().is_some_and(|&c| c < k.len()) {
                cuts.push(k.len());
            }
            out.extend(cuts.windows(2).map(|w| k[w[0]..w[1]].to_vec()));
        }
        out.sort_unstable();
        out
    }

    fn edges_by_walk(keys: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let (mut last, mut open, mut out) = (Vec::new(), vec![0], Vec::new());
        for key in keys {
            EdgeCount::walk(&mut last, &mut open, key, &mut |label| {
                out.push(label.to_vec());
            });
        }
        close_edges(&mut open, &last, 0, &mut |label: &[u8]| {
            out.push(label.to_vec());
        });
        out.sort_unstable();
        out
    }

    #[test]
    fn the_walk_forward_finds_the_edges_of_the_definition() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        for round in 0..400 {
            let (count, alphabet, longest) = (1 + next(60), 1 + next(4), 1 + next(12));
            let mut keys: Vec<Vec<u8>> = (0..count)
                .map(|_| {
                    let len = next(longest + 1);
                    (0..len).map(|_| b'a' + next(alphabet) as u8).collect()
                })
                .collect();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(
                edges_by_walk(&keys),
                edges_by_definition(&keys),
                "round {round}: {keys:?}"
            );
        }
    }

    #[test]
    fn a_label_seen_twice_is_never_missed() {
        let mut seen = Seen::for_keys(100);
        let labels: Vec<Vec<u8>> = (0..50_000u32).map(|i| i.to_le_bytes().to_vec()).collect();
        for label in &labels {
            seen.add(label);
        }
        let once = labels.iter().filter(|l| seen.twice(l)).count();
        for label in &labels {
            seen.add(label);
        }
        assert!(labels.iter().all(|l| seen.twice(l)));
        // 65 536 bits a table is about 0.76 bits set a label, so the table is nearly full here and
        // this bound only says that a label met once is not always taken for two.
        assert!(once < labels.len(), "{once}");
    }

    #[test]
    fn what_may_pay_is_all_that_can() {
        assert!(!pays(4, u64::MAX) && !may_pay(4));
        assert!(pays(5, 1 << 20) && may_pay(5));
        assert!(may_pay(LABEL_MAX) && !may_pay(LABEL_MAX + 1));
    }

    /// Six directories holding the same files and the same subdirectory. A file's name is read by
    /// the one key that ends in it; the subdirectory's by every key under it.
    fn path_list() -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for dir in 0..6 {
            for file in 0..60u64 {
                let name = file.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                keys.push(format!("/{dir}/{name:024x}"));
            }
            for file in 0..40 {
                keys.push(format!("/{dir}/shared-dir/{file:02}"));
            }
        }
        keys.sort();
        keys
    }

    #[test]
    fn a_label_is_spelled_whatever_its_length() {
        for len in 1..=40usize {
            let label: Vec<u8> = (0..len as u8).map(|b| b + 1).collect();
            let mut stored: Vec<u8> = label.iter().rev().copied().collect();
            stored.extend_from_slice(&[0; 8]);
            let got = spelled(Label {
                stored: &stored,
                len,
            });
            assert_eq!(got, label);
        }
    }

    #[test]
    fn the_edges_of_a_path_list_become_labels_in_id_order() {
        let keys = path_list();
        let got = chosen(&keys, &[]).expect("repeated labels pay");
        let (labels, store) = got.nested.as_ref().expect("a file's name is read once");
        assert_eq!(labels.len(), store.len());
        for (rank, label) in labels.iter().enumerate() {
            assert_eq!(store.with(rank, spelled).as_deref(), Some(&label[..]));
        }
        assert!(store.with(labels.len(), |_| ()).is_none());
    }

    #[test]
    fn a_label_that_is_already_a_phrase_is_not_stored_twice() {
        let mut keys: Vec<String> = (0..200)
            .map(|i| format!("k{i:03}/tail-shared-by-all"))
            .collect();
        keys.sort();
        let with = every_label(&chosen(&keys, &[]).expect("the tail pays"));
        let tail = with
            .iter()
            .find(|l| l.ends_with(b"tail-shared-by-all"))
            .cloned();
        let tail = tail.expect("the shared tail is a label");
        let without =
            chosen(&keys, std::slice::from_ref(&tail)).map_or_else(Vec::new, |s| every_label(&s));
        assert!(!without.contains(&tail));
    }

    /// An edge whose bytes the symbol table spells in one code is counted and admitted, but a
    /// reference to it, at two bytes, is never the cheapest coding.
    #[test]
    fn a_label_the_parse_never_takes_is_not_stored() {
        let mut keys: Vec<String> = Vec::new();
        for c in (b'A'..=b'Z').map(char::from) {
            keys.push(format!("{c}z"));
            keys.extend((0..10).map(|d| format!("{c}abcdefgh{d}")));
        }
        keys.sort();
        let edge = b"abcdefgh".to_vec();
        assert!(pays(edge.len(), 26));
        let raw = chosen(&keys, &[]).expect("with no symbols the edge is worth naming");
        assert!(every_label(&raw).contains(&edge));
        let pieces: Vec<&[u8]> = keys.iter().map(String::as_bytes).collect();
        let tables = [Table::train(&pieces)];
        let got = select(&keys, &[], 1 << 20, 256, micro_for(256), &tables);
        assert!(got.is_none_or(|s| !every_label(&s).contains(&edge)));
    }

    /// Microblocks of two: `b-label-name-1` opens one, a restart its block reads, and the key below
    /// it shares the label; `b0` shares only the byte before it, which the first byte settles. The
    /// other two occurrences are read by their own keys alone.
    #[test]
    fn a_label_is_read_by_the_keys_of_its_scope_that_share_it() {
        let keys = [
            "a",
            "b-label-name-0",
            "b-label-name-1",
            "b-label-name-2",
            "b0",
            "c-label-name-0",
            "d",
        ]
        .map(String::from);
        let mut lcps = vec![0; keys.len() + 1];
        for i in 1..keys.len() {
            lcps[i] = crate::dict_index::lcp(keys[i - 1].as_bytes(), keys[i].as_bytes());
        }
        let kept: [(&[u8], u32); 2] = [(b"-label-name-", 0), (b"never-in-the-keys", 0)];
        let uses = usage(&keys, &lcps, 8, 2, &[Table::train(&[])], &[], &kept);
        let got = |u: &Usage| {
            (
                u.picks.load(Ordering::Relaxed),
                u.reads.load(Ordering::Relaxed),
            )
        };
        assert_eq!(got(&uses[0]), (3, 4));
        assert_eq!(got(&uses[1]), (0, 0));
    }

    /// Two tables, one that spells the label in one code, cheaper than a reference, and one that
    /// spells nothing: which of them a block is parsed under decides whether the label is picked, so
    /// a walk that names the wrong shard shows.
    #[test]
    fn a_walk_of_the_keys_counts_what_the_sorted_build_counts() {
        const LABEL: &[u8] = b"-labels-";
        let keys: Vec<String> = (0..70_000u32)
            .map(|i| format!("{i:06}-labels-{:02}", i % 89))
            .collect();
        let mut lcps = vec![0; keys.len() + 1];
        for i in 1..keys.len() {
            lcps[i] = crate::dict_index::lcp(keys[i - 1].as_bytes(), keys[i].as_bytes());
        }
        let tables = [Table::train(&[]), Table::train(&[LABEL; 64])];
        let kept: [(&[u8], u32); 1] = [(LABEL, 0)];
        let got = |uses: &[Usage]| -> Vec<(u64, u64)> {
            uses.iter()
                .map(|u| {
                    (
                        u.picks.load(Ordering::Relaxed),
                        u.reads.load(Ordering::Relaxed),
                    )
                })
                .collect()
        };
        let block = 1024;
        let micro = micro_for(block);
        assert!(shard_blocks_for(block) * block < keys.len(), "two shards");
        let held = got(&usage(&keys, &lcps, block, micro, &tables, &[], &kept));
        let mut run = crate::extsort::Run::with_budget(1 << 24);
        for key in &keys {
            run.push(key);
        }
        let walked = usage_of_stream(&mut &mut run, block, micro, &tables, &[], &kept).unwrap();
        assert_eq!(got(&walked), held);
        let flipped = [tables[1].clone(), tables[0].clone()];
        assert_ne!(
            got(&usage(&keys, &lcps, block, micro, &flipped, &[], &kept)),
            held,
            "the tables have to move the picks"
        );
        assert!(held[0].0 > 0);
    }

    #[test]
    fn a_label_goes_flat_on_its_reads_and_only_up_to_a_phrase_long() {
        assert!(earns_flat(2, 16));
        assert!(!earns_flat(2, 15));
        assert!(earns_flat(8, 36));
        assert!(!earns_flat(8, 35));
        assert!(earns_flat(phrase::MAX, 1 << 20));
        assert!(!earns_flat(phrase::MAX + 1, 1 << 40));
    }

    #[test]
    fn the_labels_a_lookup_reads_most_go_flat_and_no_others() {
        let keys = path_list();
        let got = chosen(&keys, &[]).expect("repeated labels pay");
        assert!(
            got.flat.iter().any(|l| l.ends_with(b"shared-dir/")),
            "{:?}",
            got.flat
        );
        let (labels, _) = got.nested.as_ref().expect("a file's name is read once");
        assert!(labels.iter().all(|l| !l.ends_with(b"shared-dir/")));
        assert!(got.flat.iter().all(|l| !labels.contains(l)));
    }
}
