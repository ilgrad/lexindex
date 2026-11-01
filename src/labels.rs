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
use crate::fsst::Table;
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
    let trie = Trie::of(
        phrases
            .iter()
            .map(Vec::as_slice)
            .chain(kept.iter().map(|&(l, _)| l)),
    );
    let uses: Vec<Usage> = kept.iter().map(|_| Usage::default()).collect();
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
            for i in start + 1..end {
                let off = i - start;
                let (shared, scope) = if off % micro == 0 {
                    let shared = lcps[i + 1 - micro..=i].iter().copied().min();
                    (shared.unwrap_or(0), end)
                } else {
                    (lcps[i], (start + (off / micro + 1) * micro).min(end))
                };
                let suffix = &keys[i].key_bytes()[shared..];
                phrase::picks(suffix, encoder, &trie, &mut w, |at, id| {
                    let Some(used) = id.checked_sub(phrases.len()).map(|rank| &uses[rank]) else {
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
    uses
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
    // The depths at which a key branches from the keys after it are the suffix minima of the lcp
    // array to its right; with what it shares with the key before, they cut its residual into the
    // edges of the patricia trie. One stack, walked from the right.
    let mut counts: Map<&[u8], u32> = Map::default();
    let mut stack: Vec<usize> = Vec::new();
    let mut cuts: Vec<usize> = Vec::new();
    for i in (0..n).rev() {
        let next = lcps[i + 1];
        while stack.last().is_some_and(|&v| v >= next) {
            stack.pop();
        }
        stack.push(next);
        let (d0, k) = (lcps[i], key(i));
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
        for w in cuts.windows(2) {
            let label = &k[w[0]..w[1]];
            if (2..=LABEL_MAX).contains(&label.len()) {
                *counts.entry(label).or_insert(0) += 1;
            }
        }
    }
    let known: std::collections::HashSet<&[u8]> = phrases.iter().map(Vec::as_slice).collect();
    let mut kept: Vec<(&[u8], u32)> = counts
        .into_iter()
        .filter(|&(l, c)| pays(l.len(), u64::from(c)) && !known.contains(l))
        .collect();
    if kept.is_empty() {
        return None;
    }
    // The most used first, ties in byte order, so that the cut is a function of the keys.
    kept.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    kept.truncate(room.min(phrase::IDS_MAX));
    let uses = usage(keys, &lcps, block, micro, tables, phrases, &kept);
    let mut used: Vec<(&[u8], u64, u64)> = kept
        .iter()
        .zip(&uses)
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
