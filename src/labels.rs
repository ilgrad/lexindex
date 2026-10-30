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
//! Chosen per blob: the build that tries labels keeps them only where the blob comes out smaller.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::dict_index::{Bytes, DictIndex, MAGIC, micro_for};
use crate::phrase::{self, LABEL_MAX, Map};
use std::cell::Cell;

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

    /// Label `rank` handed to `f` as it reads forward; `None` past the labels or on one this crate
    /// did not write.
    #[inline]
    pub(crate) fn with<R>(&self, rank: usize, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        SCRATCH.with(|cell| {
            let mut buf = cell.take();
            let got = self.index.key_bytes_into(rank as u64, &mut buf)
                && (2..=LABEL_MAX + 1).contains(&buf.len());
            let out = got.then(|| {
                buf[1..].reverse();
                f(&buf[1..])
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
fn pays(len: usize, count: u32) -> bool {
    let (l, c) = (len as f64, f64::from(count));
    c * (0.58 * l - 2.5) > 0.6 * l + 3.0
}

/// The labels worth a dictionary over `keys`, sorted and distinct, in id order, with the nested
/// index that stores them; `None` where none pays. `phrases` are the blob's own, which a label
/// does not repeat, and `room` the ids left past them.
pub(crate) fn select(
    keys: &[impl crate::dict_index::AsKey],
    phrases: &[Vec<u8>],
    room: usize,
) -> Option<(Vec<Vec<u8>>, Labels)> {
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
        .filter(|&(l, c)| pays(l.len(), c) && !known.contains(l))
        .collect();
    if kept.is_empty() {
        return None;
    }
    // The most used first, ties in byte order, so that the cut is a function of the keys.
    kept.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    kept.truncate(room.min(phrase::IDS_MAX));
    let mut stored: Vec<Vec<u8>> = kept
        .iter()
        .enumerate()
        .map(|(rank, &(l, _))| {
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
    Some((labels, Labels::of(index, firsts)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_edges_of_a_path_list_become_labels_in_id_order() {
        let mut keys: Vec<String> = Vec::new();
        for dir in ["alpha", "beta", "gamma", "delta"] {
            for f in 0..40 {
                keys.push(format!("/usr/share/{dir}/component-{f:02}/index.html"));
            }
        }
        keys.sort();
        let (labels, store) = select(&keys, &[], 1 << 20).expect("repeated labels pay");
        assert_eq!(labels.len(), store.len());
        for (rank, label) in labels.iter().enumerate() {
            assert_eq!(
                store.with(rank, <[u8]>::to_vec).as_deref(),
                Some(&label[..])
            );
        }
        assert!(
            labels.iter().any(|l| l.ends_with(b"/index.html")),
            "{labels:?}"
        );
        assert!(store.with(labels.len(), |_| ()).is_none());
    }

    #[test]
    fn a_label_that_is_already_a_phrase_is_not_stored_twice() {
        let mut keys: Vec<String> = (0..200)
            .map(|i| format!("k{i:03}/tail-shared-by-all"))
            .collect();
        keys.sort();
        let (with, _) = select(&keys, &[], 1 << 20).expect("the tail pays");
        let tail = with
            .iter()
            .find(|l| l.ends_with(b"tail-shared-by-all"))
            .cloned();
        let tail = tail.expect("the shared tail is a label");
        let without =
            select(&keys, std::slice::from_ref(&tail), 1 << 20).map_or_else(Vec::new, |(l, _)| l);
        assert!(!without.contains(&tail));
    }
}
