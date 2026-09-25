//! A character-wise double-array trie: the dictionary-matching queries — every key that is a
//! prefix of a text, the longest one, every key occurring anywhere in it — at one load a character.
//!
//! A double array numbers a trie's edges so that the child of the node at base `b` along label
//! `x` sits in slot `b + x`, and a walk is one indexed load a step, checked by the label the slot
//! holds. Here a label is a **character**, not a byte: the lexicon's characters are numbered by
//! frequency, 1 for the most frequent, through a table indexed by code point, so a Chinese word of
//! three characters is three steps rather than nine, and the frequent characters crowd the low
//! labels, where the rows pack densely.
//!
//! Each slot is eight bytes and holds everything a step reads — the label, whether a key ends
//! there, whether anything continues, the child's base and the key's id — so a match costs no load
//! past the slot that reaches it. Bases are unique, which makes the label check exact. The ids are
//! the keys' ranks in byte order, the ids [`StringIndex`](crate::StringIndex) and
//! [`DictIndex`](crate::DictIndex) assign to the same keys, so either of them spells an id back.
//!
//! Placement puts the root at base 0 and every other row at the lowest base where all its slots
//! are free, the rows with the most children first: on jieba's lexicon three slots in four are
//! used.
//!
//! The blob is the header, the slots, the code table and the characters past the Basic
//! Multilingual Plane. Every load walks the slots once, so that no slot names a base whose row
//! runs past the array or an id past the end: the walks then read without a bounds check, and a
//! crafted blob answers wrong ids, never out-of-range ones.

use std::collections::HashMap;
use std::mem::MaybeUninit;

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::pages::Pages;

/// `[magic 4][n u64][empty u64][slots u64][table u32][supp u32][max_label u32][payload u64]
/// [reserved 12, zero][check u32]`, then the slots (8 B each), the code table (2 B each) and the
/// supplementary characters (code point u32, label u32). Sixty-four bytes rather than the 52 the
/// fields need, so that a slot is as aligned as the blob, and a blob here starts at 16 bytes or
/// better — on a page when mapped, on a huge page past 2 MiB: no slot straddles a cache line.
const MAGIC: &[u8; 4] = b"BDA1";
const HEADER: usize = 64;
const RESERVED: usize = 48;
const CHECKED: usize = 60; // header bytes the trailing check covers
const SUPP_ENTRY: usize = 8;

/// A slot: label (16 bits) | a key ends here | nothing continues | base (23) | id (23).
const LABEL_MASK: u64 = 0xFFFF;
const WORD: u64 = 1 << 16;
const LEAF: u64 = 1 << 17;
const BASE_SHIFT: u32 = 18;
const BASE_BITS: u32 = 23;
const BASE_MASK: u64 = (1 << BASE_BITS) - 1;
const ID_SHIFT: u32 = BASE_SHIFT + BASE_BITS;

/// Ids and bases are 23 bits.
const MAX_KEYS: usize = 1 << (64 - ID_SHIFT);
const MAX_SLOTS: usize = 1 << BASE_BITS;
/// Label 0 is "not in the lexicon".
const MAX_ALPHABET: usize = LABEL_MASK as usize;
/// The code table covers the Basic Multilingual Plane; the characters past it are looked up.
const BMP: u32 = 0x1_0000;

/// No key ends at this node.
const NO_WORD: u32 = u32::MAX;

/// Texts up to this many bytes are decoded into buffers on the stack.
const STACK_TEXT: usize = 256;

/// An immutable character-wise double-array trie over a set of strings.
pub struct DoubleArrayIndex {
    /// The whole blob, for [`to_bytes`](Self::to_bytes) and [`save`](Self::save).
    blob: SharedBytes,
    /// Eight bytes a slot.
    slots: SharedBytes,
    /// A label for every code point below the table's length, two bytes each.
    table: SharedBytes,
    /// `(code point, label)` past the Basic Multilingual Plane, ascending.
    supp: Box<[(u32, u16)]>,
    n: usize,
    /// The empty key's id: no slot can hold it, since it ends at the root.
    empty: Option<u64>,
}

/// The code point starting at `b[i]` and its width.
///
/// # Safety
/// `b[i..]` must start with a whole UTF-8 sequence.
#[inline(always)]
unsafe fn decode(b: &[u8], i: usize) -> (u32, usize) {
    // SAFETY: a UTF-8 sequence carries as many continuation bytes as its lead says.
    unsafe {
        let b0 = u32::from(*b.get_unchecked(i));
        if b0 < 0x80 {
            (b0, 1)
        } else if b0 < 0xE0 {
            (
                (b0 & 0x1F) << 6 | u32::from(*b.get_unchecked(i + 1)) & 0x3F,
                2,
            )
        } else if b0 < 0xF0 {
            (
                (b0 & 0x0F) << 12
                    | (u32::from(*b.get_unchecked(i + 1)) & 0x3F) << 6
                    | u32::from(*b.get_unchecked(i + 2)) & 0x3F,
                3,
            )
        } else {
            (
                (b0 & 0x07) << 18
                    | (u32::from(*b.get_unchecked(i + 1)) & 0x3F) << 12
                    | (u32::from(*b.get_unchecked(i + 2)) & 0x3F) << 6
                    | u32::from(*b.get_unchecked(i + 3)) & 0x3F,
                4,
            )
        }
    }
}

/// Slot `s` of the array at `slots`, which holds `len` of them.
///
/// # Safety
/// `s` must be below `len`.
#[inline(always)]
unsafe fn slot(slots: *const u8, len: usize, s: usize) -> u64 {
    // What makes this hold is the walk over every slot at load. The fuzz targets and Miri run with
    // debug assertions, so a blob that walk lets through and a query then reads past shows here.
    debug_assert!(s < len, "slot {s} of {len}");
    // SAFETY: slot `s` is eight bytes inside the array, and a byte array needs no alignment.
    u64::from_le_bytes(unsafe { *slots.add(8 * s).cast::<[u8; 8]>() })
}

/// The slots in use, as bits, sized for the largest array a slot can address, so that no search
/// runs off the end; and a summary bit a word, set while the word has a free slot, so that a search
/// crosses the densely packed bottom of the array 64 words at a time.
struct Occupancy {
    used: Vec<u64>,
    open: Vec<u64>,
    /// Bases taken, one bit each, `pad` words up: bases are unique, and a search reads the bits up
    /// to `max_label` below a first child's slot, which near slot 0 lie below base 0, and are clear.
    taken: Vec<u64>,
    pad: usize,
}

impl Occupancy {
    fn new(max_label: usize) -> Self {
        // A search stops at the last base that fits, tests `CHUNK` words from there, and reads
        // the word after each; a row's last child is `max_label` past its base.
        let words = (MAX_SLOTS + max_label) / 64 + CHUNK + 4;
        let pad = max_label / 64 + 1;
        Self {
            used: vec![0; words],
            open: vec![u64::MAX; words / 64 + 2],
            taken: vec![0; words + pad],
            pad,
        }
    }

    fn take(&mut self, s: usize) {
        let w = s / 64;
        self.used[w] |= 1 << (s % 64);
        if self.used[w] == u64::MAX {
            self.open[w / 64] &= !(1 << (w % 64));
        }
    }

    fn take_base(&mut self, b: usize) {
        let x = b + 64 * self.pad;
        self.taken[x / 64] |= 1 << (x % 64);
    }

    /// The first word at or after `w` with a free slot.
    fn open_from(&self, w: usize) -> usize {
        let mut j = w / 64;
        let mut m = self.open[j] & u64::MAX << (w % 64);
        while m == 0 {
            j += 1;
            m = self.open[j];
        }
        j * 64 + m.trailing_zeros() as usize
    }

    /// Bit `j` set while word `w + j` has a free slot, for the `CHUNK` words from `w`.
    fn open_words(&self, w: usize) -> u64 {
        window(&self.open, w / 64, (w % 64) as u32) & (u64::MAX >> (64 - CHUNK))
    }

    /// The bit offset in `taken` of the base whose child `c0` is word `w`'s first slot.
    fn base_at(&self, w: usize, c0: usize) -> (usize, u32) {
        let x = 64 * (w + self.pad) - c0;
        (x / 64, (x % 64) as u32)
    }

    /// Word `w`'s slots a child with label `c0` could take: free, and its base not taken.
    fn candidates(&self, w: usize, c0: usize) -> u64 {
        let (q, s) = self.base_at(w, c0);
        !self.used[w] & !window(&self.taken, q, s)
    }

    /// The same for the `CHUNK` words from `w0`.
    fn candidates_chunk(&self, w0: usize, c0: usize) -> [u64; CHUNK] {
        let (q, s) = self.base_at(w0, c0);
        let used: &[u64; CHUNK] = self.used[w0..w0 + CHUNK].try_into().expect("CHUNK words");
        let taken: &[u64; CHUNK + 1] = self.taken[q..q + CHUNK + 1]
            .try_into()
            .expect("CHUNK + 1 words");
        let mut fit = [0u64; CHUNK];
        if s == 0 {
            for j in 0..CHUNK {
                fit[j] = !used[j] & !taken[j];
            }
        } else {
            for j in 0..CHUNK {
                fit[j] = !used[j] & !(taken[j] >> s | taken[j + 1] << (64 - s));
            }
        }
        fit
    }
}

/// Words of candidate bases a search tests together: a wide row needs dozens of its children's
/// slots to rule out 64 bases, and the same test over several words at once runs as vector code.
const CHUNK: usize = 32;

/// A chunk with this few words holding a free slot is tested a word at a time: past the densely
/// packed bottom, a two-child row's search crosses chunks with one such word each.
const SPARSE: u32 = 4;

/// Bits `64 q + s .. 64 q + s + 64` of a bitmap, `s < 64`.
#[inline(always)]
fn window(words: &[u64], q: usize, s: u32) -> u64 {
    if s == 0 {
        return words[q];
    }
    words[q] >> s | words[q + 1] << (64 - s)
}

/// The explicit trie placement works from, breadth first from the root at 0: node `v`'s children
/// are the nodes `first[v] .. first[v + 1]`, in label order.
struct Trie {
    label: Vec<u16>,
    word: Vec<u32>,
    first: Vec<u32>,
}

impl Trie {
    /// `keys[id]` as labels are `labels[starts[id] .. starts[id + 1]]`, the keys distinct and in
    /// byte order from `first_key` on, and none of them empty.
    ///
    /// Byte order puts the keys under a node in one run of ids, as label order would, but runs
    /// its children in code-point order: each node's are sorted by label before they are numbered.
    fn build(labels: &[u16], starts: &[usize], first_key: usize) -> Self {
        let n = starts.len() - 1;
        let mut t = Trie {
            label: vec![0],
            word: vec![NO_WORD],
            first: Vec::new(),
        };
        // (lo, hi, depth): the keys under each node, a range of ids.
        let mut span: Vec<(u32, u32, u32)> = vec![(first_key as u32, n as u32, 0)];
        let mut runs: Vec<(u16, u32, u32)> = Vec::new();
        let mut v = 0;
        while v < t.label.len() {
            let (mut lo, hi, d) = span[v];
            let d = d as usize;
            // The key the node spells sorts first among the keys under it.
            if lo < hi && starts[lo as usize + 1] - starts[lo as usize] == d {
                t.word[v] = lo;
                lo += 1;
            }
            t.first.push(t.label.len() as u32);
            // Every key left is longer than `d`: it extends the node's, and is not it.
            runs.clear();
            let mut i = lo;
            while i < hi {
                let c = labels[starts[i as usize] + d];
                let mut j = i + 1;
                while j < hi && labels[starts[j as usize] + d] == c {
                    j += 1;
                }
                runs.push((c, i, j));
                i = j;
            }
            runs.sort_unstable_by_key(|r| r.0);
            for &(c, i, j) in &runs {
                t.label.push(c);
                t.word.push(NO_WORD);
                span.push((i, j, d as u32 + 1));
            }
            v += 1;
        }
        t.first.push(t.label.len() as u32);
        t
    }

    fn kids(&self, v: usize) -> std::ops::Range<usize> {
        self.first[v] as usize..self.first[v + 1] as usize
    }
}

/// The slot the lowest search a set of labels could succeed at starts from, for a row's first
/// label, its first two, and all of them.
///
/// A row fits at base `b` when no row has `b` and every slot `b + c` of its labels is free. Slots and
/// bases are only ever taken, so for any set of labels the lowest slot its first child could take
/// never moves down, and a search that started at or below that slot and found it leaves it for
/// the next row with the same labels: the next search skips, exactly, what this one proved has no
/// room. On a small alphabet the free slots left low in the array suit no label, and without the
/// floors every row rescanned them: a build over 480 k English words took 16.6 s, and 0.5 with
/// them, placing every row where it did before.
struct Floors {
    first: Vec<usize>,
    pair: HashMap<u32, usize>,
    all: HashMap<Box<[u16]>, usize>,
}

impl Floors {
    fn new(max_label: usize) -> Self {
        Self {
            // A first child's slot is at least its label: bases start at 0.
            first: (0..=max_label).collect(),
            pair: HashMap::new(),
            all: HashMap::new(),
        }
    }

    /// The floors for the first label, the first two and all of them, 0 where none is known.
    fn get(&self, labels: &[u16]) -> (usize, usize, usize) {
        let pair = match labels {
            [a, b, ..] => self.pair.get(&pair_key(*a, *b)).copied().unwrap_or(0),
            _ => 0,
        };
        let all = if labels.len() > 2 {
            self.all.get(labels).copied().unwrap_or(0)
        } else {
            0
        };
        (self.first[usize::from(labels[0])], pair, all)
    }

    fn set_pair(&mut self, labels: &[u16], slot: usize) {
        self.pair.insert(pair_key(labels[0], labels[1]), slot);
    }

    fn set_all(&mut self, labels: &[u16], slot: usize) {
        match self.all.get_mut(labels) {
            Some(f) => *f = slot,
            None => {
                self.all.insert(labels.into(), slot);
            }
        }
    }
}

fn pair_key(a: u16, b: u16) -> u32 {
    u32::from(a) << 16 | u32::from(b)
}

/// The slot of the lowest set bit of `fit`, whose word 0 is word `w0` of the array.
fn lowest(fit: &[u64; CHUNK], w0: usize) -> Option<usize> {
    let j = fit.iter().position(|&f| f != 0)?;
    Some(64 * (w0 + j) + fit[j].trailing_zeros() as usize)
}

/// Every row's base: the root's is 0, and every other row takes the lowest base no row has
/// whose slots are all free, the rows with the most children first. Wide rows are the hard ones to
/// fit, so they go while the array is sparse; the single-child rows, most of a lexicon's, then fill
/// the holes from the bottom.
///
/// A search starts at the highest of its [`Floors`], walks the free slots its first child could
/// take, `CHUNK` words of 64 at a time, and tests the bases they imply against the other children's
/// slots, 64 bases a word at once.
fn place(t: &Trie, max_label: usize) -> Result<Vec<u32>, IndexError> {
    let too_many = || {
        IndexError::Format(
            "double-array: the trie needs more than 8 388 608 slots; bases are 23 bits",
        )
    };
    let nodes = t.label.len();
    let mut base = vec![0u32; nodes];
    let mut o = Occupancy::new(max_label);
    o.take(0);
    o.take_base(0);
    for k in t.kids(0) {
        o.take(usize::from(t.label[k]));
    }
    let mut rows: Vec<u32> = (1..nodes as u32)
        .filter(|&v| !t.kids(v as usize).is_empty())
        .collect();
    rows.sort_by_key(|&v| std::cmp::Reverse(t.kids(v as usize).len()));
    // Every slot below `lo` is in use.
    let mut lo = 0usize;
    let mut floors = Floors::new(max_label);
    // Past this word a base would not fit a slot.
    let last_word = (MAX_SLOTS - max_label) / 64;
    // The other children's slots relative to the first child's: a word offset and a shift.
    let mut rel: Vec<(usize, u32)> = Vec::new();
    for v in rows {
        let labels = &t.label[t.kids(v as usize)];
        let c0 = usize::from(labels[0]);
        rel.clear();
        rel.extend(labels[1..].iter().map(|&c| {
            let r = usize::from(c) - c0;
            (r / 64, (r % 64) as u32)
        }));
        let w = o.open_from(lo / 64);
        lo = 64 * w + (!o.used[w]).trailing_zeros() as usize;
        // The first child's slot: every base is at least 0, every slot below `lo` is used, and
        // every floor holds. A floor is learned only by a search that started at or below it, and
        // a longer set's floor can lie above a shorter one's.
        let (f1, f2, fall) = floors.get(labels);
        let e1 = lo.max(f1);
        let e2 = e1.max(f2);
        let mut e = e2.max(fall);
        let (mut learn1, mut learn2) = (e == e1, e == e2 && labels.len() > 1);
        let s = 'search: loop {
            let w0 = o.open_from(e / 64);
            if w0 > last_word {
                return Err(too_many());
            }
            let mask0 = if w0 == e / 64 {
                u64::MAX << (e % 64)
            } else {
                u64::MAX
            };
            let open = o.open_words(w0);
            if rel.is_empty() || open.count_ones() <= SPARSE {
                let mut m = open;
                while m != 0 {
                    let j = m.trailing_zeros() as usize;
                    m &= m - 1;
                    let w = w0 + j;
                    let mut f = o.candidates(w, c0);
                    if j == 0 {
                        f &= mask0;
                    }
                    if f == 0 {
                        continue;
                    }
                    if learn1 {
                        floors.first[c0] = 64 * w + f.trailing_zeros() as usize;
                        learn1 = false;
                    }
                    for (r, &(dq, sh)) in rel.iter().enumerate() {
                        f &= !window(&o.used, w + dq, sh);
                        if f == 0 {
                            break;
                        }
                        if r == 0 && learn2 {
                            floors.set_pair(labels, 64 * w + f.trailing_zeros() as usize);
                            learn2 = false;
                        }
                    }
                    if f != 0 {
                        break 'search 64 * w + f.trailing_zeros() as usize;
                    }
                }
            } else {
                // Bit `i` of `fit[j]` stands for the base whose first child is slot
                // `64 (w0 + j) + i`.
                let mut fit = o.candidates_chunk(w0, c0);
                fit[0] &= mask0;
                if learn1 {
                    if let Some(s) = lowest(&fit, w0) {
                        floors.first[c0] = s;
                        learn1 = false;
                    }
                }
                for (r, &(dq, sh)) in rel.iter().enumerate() {
                    let from: &[u64; CHUNK + 1] = o.used[w0 + dq..w0 + dq + CHUNK + 1]
                        .try_into()
                        .expect("CHUNK + 1 words");
                    let mut any = 0;
                    if sh == 0 {
                        for j in 0..CHUNK {
                            fit[j] &= !from[j];
                            any |= fit[j];
                        }
                    } else {
                        for j in 0..CHUNK {
                            fit[j] &= !(from[j] >> sh | from[j + 1] << (64 - sh));
                            any |= fit[j];
                        }
                    }
                    if any == 0 {
                        break;
                    }
                    if r == 0 && learn2 {
                        floors.set_pair(labels, lowest(&fit, w0).expect("a bit is set"));
                        learn2 = false;
                    }
                }
                if let Some(s) = lowest(&fit, w0) {
                    break 'search s;
                }
            }
            e = 64 * (w0 + CHUNK);
        };
        if labels.len() > 2 {
            floors.set_all(labels, s);
        }
        // No fit lies below `e`, which is at least `c0`.
        let b = s - c0;
        if b + max_label >= MAX_SLOTS {
            return Err(too_many());
        }
        o.take_base(b);
        for &c in labels {
            o.take(b + usize::from(c));
        }
        base[v as usize] = b as u32;
    }
    Ok(base)
}

/// Whether some slot names a row running past an array of `n_slots` or an id past `n`, which a
/// walk would follow out of range, or is one no build writes: an empty slot with bits set, an id
/// where no key ends, a leaf with a base or without a key.
///
/// No branch a slot — which slots are empty follows no pattern a predictor learns — and no compare:
/// flags become masks, and a bound is a wrapping subtraction whose top bit is set exactly when the
/// value is past it, so the loop is ands, ors and subtractions, which vectorise.
fn slots_malformed(slots: &[u8], n: u64, max_label: usize, n_slots: usize) -> bool {
    const TOP: u64 = 1 << 63;
    // The highest base whose row fits; unused when there are no slots.
    let last_base = n_slots.saturating_sub(max_label + 1) as u64;
    let mut stray = 0u64;
    for e in slots.chunks_exact(8) {
        let v = u64::from_le_bytes(e.try_into().expect("eight bytes"));
        let word = ((v >> 16) & 1).wrapping_neg();
        let leaf = ((v >> 17) & 1).wrapping_neg();
        let empty = ((v & LABEL_MASK).wrapping_sub(1) >> 63).wrapping_neg();
        let base = (v >> BASE_SHIFT) & BASE_MASK;
        let id = v >> ID_SHIFT;
        stray |= v & empty
            | id & !word
            | (base | !word & 1) & leaf
            | last_base.wrapping_sub(base) & !leaf & TOP
            | n.wrapping_sub(id + 1) & word & TOP;
    }
    stray != 0
}

/// The blob's header.
#[allow(clippy::too_many_arguments)]
fn header_bytes(
    n: usize,
    empty: Option<u64>,
    n_slots: usize,
    table: usize,
    supp: usize,
    max_label: usize,
    payload: u64,
) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[0..4].copy_from_slice(MAGIC);
    h[4..12].copy_from_slice(&(n as u64).to_le_bytes());
    h[12..20].copy_from_slice(&empty.unwrap_or(u64::MAX).to_le_bytes());
    h[20..28].copy_from_slice(&(n_slots as u64).to_le_bytes());
    h[28..32].copy_from_slice(&(table as u32).to_le_bytes());
    h[32..36].copy_from_slice(&(supp as u32).to_le_bytes());
    h[36..40].copy_from_slice(&(max_label as u32).to_le_bytes());
    h[40..48].copy_from_slice(&payload.to_le_bytes());
    let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
    h[CHECKED..].copy_from_slice(&check.to_le_bytes());
    h
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("eight bytes"))
}

impl DoubleArrayIndex {
    /// Build an index from a collection of strings. Duplicates are removed and the keys are
    /// sorted; the id of a key is its rank in byte order, as [`StringIndex`](crate::StringIndex)
    /// numbers the same keys.
    ///
    /// Refused, with a message naming the limit: more than 8 388 608 keys, more than 65 535
    /// distinct characters, or a trie that needs more than 8 388 608 slots — about four million
    /// Chinese words, fewer long Latin ones.
    ///
    /// ```
    /// use lexindex::DoubleArrayIndex;
    /// let idx = DoubleArrayIndex::build(["北京", "北京大学", "大学", "大学生"])?;
    /// assert_eq!(idx.id("大学"), Some(2));
    /// assert_eq!(idx.id("北"), None);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut keys: Vec<S> = items.into_iter().collect();
        // Keys that arrive strictly ascending are what the sort and the dedup would leave, and one
        // comparison a key finds out.
        if !keys.is_sorted_by(|a, b| a.as_ref() < b.as_ref()) {
            keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
            keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        }
        Self::build_ranked(&keys)
    }

    /// `keys` sorted and distinct; a key's id is its position.
    fn build_ranked<S: AsRef<str>>(keys: &[S]) -> Result<Self, IndexError> {
        let n = keys.len();
        if n > MAX_KEYS {
            return Err(IndexError::Format(
                "double-array: more than 8 388 608 keys; ids are 23 bits",
            ));
        }
        let empty = keys
            .first()
            .is_some_and(|k| k.as_ref().is_empty())
            .then_some(0);

        // Every key's characters, decoded once.
        let mut cps: Vec<u32> = Vec::new();
        let mut starts: Vec<usize> = Vec::with_capacity(n + 1);
        for k in keys {
            starts.push(cps.len());
            cps.extend(k.as_ref().chars().map(u32::from));
        }
        starts.push(cps.len());

        // Labels: 1 for the most frequent character, ties by code point, so the same keys always
        // number the same way.
        let mut freq = vec![0u64; cps.iter().max().map_or(0, |&cp| cp as usize + 1)];
        for &cp in &cps {
            freq[cp as usize] += 1;
        }
        let mut chars: Vec<(u64, u32)> = freq
            .iter()
            .enumerate()
            .filter(|&(_, &f)| f > 0)
            .map(|(cp, &f)| (f, cp as u32))
            .collect();
        drop(freq);
        if chars.len() > MAX_ALPHABET {
            return Err(IndexError::Format(
                "double-array: more than 65 535 distinct characters; labels are 16 bits",
            ));
        }
        chars.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let max_label = chars.len();
        let table_len = chars
            .iter()
            .map(|&(_, cp)| cp)
            .filter(|&cp| cp < BMP)
            .max()
            .map_or(0, |cp| cp as usize + 1);
        let mut table = vec![0u16; table_len];
        let mut supp: Vec<(u32, u16)> = Vec::new();
        for (rank, &(_, cp)) in chars.iter().enumerate() {
            let label = (rank + 1) as u16;
            if cp < BMP {
                table[cp as usize] = label;
            } else {
                supp.push((cp, label));
            }
        }
        supp.sort_unstable();
        let label_of = |cp: u32| -> u16 {
            if cp < BMP {
                table[cp as usize]
            } else {
                supp[supp
                    .binary_search_by_key(&cp, |e| e.0)
                    .expect("a lexicon character")]
                .1
            }
        };

        let labels: Vec<u16> = cps.iter().map(|&cp| label_of(cp)).collect();
        drop(cps);
        let trie = Trie::build(&labels, &starts, usize::from(empty.is_some()));
        drop((labels, starts));

        let base = place(&trie, max_label)?;
        let n_slots = if max_label == 0 {
            0
        } else {
            let top = base.iter().copied().max().unwrap_or(0) as usize;
            top + max_label + 1
        };
        if n_slots > MAX_SLOTS {
            return Err(IndexError::Format(
                "double-array: the trie needs more than 8 388 608 slots; bases are 23 bits",
            ));
        }

        let len = HEADER + 8 * n_slots + 2 * table_len + SUPP_ENTRY * supp.len();
        let mut blob = Pages::<u8>::zeroed(len);
        {
            let slots = &mut blob[HEADER..HEADER + 8 * n_slots];
            for v in 0..trie.label.len() {
                let b = base[v] as usize;
                for k in trie.kids(v) {
                    let c = trie.label[k];
                    let mut s = u64::from(c);
                    if trie.word[k] != NO_WORD {
                        s |= WORD | u64::from(trie.word[k]) << ID_SHIFT;
                    }
                    if trie.kids(k).is_empty() {
                        s |= LEAF;
                    } else {
                        s |= u64::from(base[k]) << BASE_SHIFT;
                    }
                    let at = 8 * (b + usize::from(c));
                    slots[at..at + 8].copy_from_slice(&s.to_le_bytes());
                }
            }
        }
        let mut at = HEADER + 8 * n_slots;
        for &l in &table {
            blob[at..at + 2].copy_from_slice(&l.to_le_bytes());
            at += 2;
        }
        for &(cp, l) in &supp {
            blob[at..at + 4].copy_from_slice(&cp.to_le_bytes());
            blob[at + 4..at + 8].copy_from_slice(&u32::from(l).to_le_bytes());
            at += SUPP_ENTRY;
        }
        let payload = crate::blob::hash_block(&blob[HEADER..]);
        blob[..HEADER].copy_from_slice(&header_bytes(
            n,
            empty,
            n_slots,
            table_len,
            supp.len(),
            max_label,
            payload,
        ));
        // The payload hash was just computed from these bytes; the walk over the slots still runs,
        // and holds the builder to what every load demands.
        Self::from_shared(SharedBytes::from_pages(blob), false)
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the index has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The label of `cp`, or 0 if no key holds it.
    #[inline(always)]
    fn label(&self, table: *const u8, table_len: usize, cp: u32) -> u64 {
        if (cp as usize) < table_len {
            // SAFETY: entry `cp` is inside the table.
            u64::from(u16::from_le_bytes(unsafe {
                *table.add(2 * cp as usize).cast::<[u8; 2]>()
            }))
        } else {
            self.label_past_table(cp)
        }
    }

    #[cold]
    #[inline(never)]
    fn label_past_table(&self, cp: u32) -> u64 {
        self.supp
            .binary_search_by_key(&cp, |e| e.0)
            .map_or(0, |i| u64::from(self.supp[i].1))
    }

    /// Id of `key`, or `None` if absent.
    #[inline(always)]
    pub fn id(&self, key: &str) -> Option<u64> {
        let b = key.as_bytes();
        if b.is_empty() {
            return self.empty;
        }
        let (slots, n_slots) = (self.slots.as_ptr(), self.slots.len() / 8);
        let (table, table_len) = (self.table.as_ptr(), self.table.len() / 2);
        let mut base = 0usize;
        let mut i = 0;
        loop {
            // SAFETY: `key` is UTF-8 and `i` a character boundary inside it.
            let (cp, w) = unsafe { decode(b, i) };
            let x = self.label(table, table_len, cp);
            if x == 0 {
                return None;
            }
            // SAFETY: `base` is a row's, and every row's `max_label` slots past its base are
            // inside the array — `build` sizes it so, and every load checks it; `x <= max_label`.
            let v = unsafe { slot(slots, n_slots, base + x as usize) };
            if v & LABEL_MASK != x {
                return None;
            }
            i += w;
            if i == b.len() {
                return (v & WORD != 0).then_some(v >> ID_SHIFT);
            }
            if v & LEAF != 0 {
                return None;
            }
            base = ((v >> BASE_SHIFT) & BASE_MASK) as usize;
        }
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Every key that is a **prefix of `query`**, shortest first, with its id. The empty key, if
    /// the index holds it, is a prefix of everything and comes first.
    ///
    /// ```
    /// use lexindex::DoubleArrayIndex;
    /// let idx = DoubleArrayIndex::build(["北京", "北京大学", "大学"])?;
    /// assert_eq!(
    ///     idx.common_prefix("北京大学生"),
    ///     [("北京".to_string(), 0), ("北京大学".to_string(), 1)]
    /// );
    /// assert_eq!(idx.longest_prefix("北京大学生"), Some(("北京大学".to_string(), 1)));
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn common_prefix(&self, query: &str) -> Vec<(String, u64)> {
        let mut found = Vec::new();
        self.for_each_common_prefix(query, |end, id| found.push((query[..end].to_owned(), id)));
        found
    }

    /// The longest key that is a prefix of `query`, or `None` if no key is — the match a
    /// longest-match tokeniser takes. See [`common_prefix`](Self::common_prefix).
    pub fn longest_prefix(&self, query: &str) -> Option<(String, u64)> {
        let mut last = None;
        self.for_each_common_prefix(query, |end, id| last = Some((end, id)));
        last.map(|(end, id)| (query[..end].to_owned(), id))
    }

    /// [`common_prefix`](Self::common_prefix) with nothing allocated: `f(end, id)` for every key
    /// that is a prefix of `query`, shortest first, where the key is `&query[..end]`. The walk
    /// ends where no key continues, so the query can be the whole rest of a text.
    ///
    /// ```
    /// use lexindex::DoubleArrayIndex;
    /// let idx = DoubleArrayIndex::build(["a", "ap", "apple", "b"])?;
    /// let mut found = Vec::new();
    /// idx.for_each_common_prefix("apples", |end, id| found.push((end, id)));
    /// assert_eq!(found, [(1, 0), (2, 1), (5, 2)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    #[inline(always)]
    pub fn for_each_common_prefix(&self, query: &str, mut f: impl FnMut(usize, u64)) {
        if let Some(id) = self.empty {
            f(0, id);
        }
        let b = query.as_bytes();
        let (slots, n_slots) = (self.slots.as_ptr(), self.slots.len() / 8);
        let (table, table_len) = (self.table.as_ptr(), self.table.len() / 2);
        let mut base = 0usize;
        let mut i = 0;
        while i < b.len() {
            // SAFETY: `query` is UTF-8 and `i` a character boundary inside it.
            let (cp, w) = unsafe { decode(b, i) };
            let x = self.label(table, table_len, cp);
            if x == 0 {
                return;
            }
            // SAFETY: as in `id`.
            let v = unsafe { slot(slots, n_slots, base + x as usize) };
            if v & LABEL_MASK != x {
                return;
            }
            i += w;
            if v & WORD != 0 {
                f(i, v >> ID_SHIFT);
            }
            if v & LEAF != 0 {
                return;
            }
            base = ((v >> BASE_SHIFT) & BASE_MASK) as usize;
        }
    }

    /// Every key occurring in `text`, as `f(start, end, id)` with the key `&text[start..end]`:
    /// starts ascending, and within a start shortest first. The empty key, if the index holds it,
    /// is not reported.
    ///
    /// The whole-text form of [`for_each_common_prefix`](Self::for_each_common_prefix) that a
    /// dictionary segmenter runs at every character. Each character is decoded and looked up once,
    /// rather than once for every walk that reaches it.
    ///
    /// ```
    /// use lexindex::DoubleArrayIndex;
    /// let idx = DoubleArrayIndex::build(["北京", "北京大学", "大学", "大学生"])?;
    /// let mut found = Vec::new();
    /// idx.for_each_occurrence("北京大学生", |start, end, id| found.push((start, end, id)));
    /// assert_eq!(found, [(0, 6, 0), (0, 12, 1), (6, 12, 2), (6, 15, 3)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    #[inline]
    pub fn for_each_occurrence(&self, text: &str, mut f: impl FnMut(usize, usize, u64)) {
        self.occurrences(text, |_, _, start, end, id| f(start, end, id));
    }

    /// [`for_each_occurrence`](Self::for_each_occurrence) as `f(start_char, end_char, id)`, the
    /// offsets a Python `str` is indexed by.
    #[cfg(feature = "python")]
    pub(crate) fn for_each_occurrence_in_chars(
        &self,
        text: &str,
        mut f: impl FnMut(usize, usize, u64),
    ) {
        self.occurrences(text, |p, j, _, _, id| f(p, j, id));
    }

    /// The occurrence walk, reporting every match in characters and in bytes: `f(first
    /// character, one past the last, start byte, end byte, id)`.
    #[inline(always)]
    fn occurrences(&self, text: &str, f: impl FnMut(usize, usize, usize, usize, u64)) {
        if text.len() <= STACK_TEXT {
            let mut labels = [MaybeUninit::<u16>::uninit(); STACK_TEXT + 1];
            let mut ends = [MaybeUninit::<usize>::uninit(); STACK_TEXT + 1];
            self.occurrences_with(text, &mut labels, &mut ends, f);
        } else {
            let mut labels = Vec::<u16>::with_capacity(text.len() + 1);
            let mut ends = Vec::<usize>::with_capacity(text.len() + 1);
            self.occurrences_with(
                text,
                labels.spare_capacity_mut(),
                ends.spare_capacity_mut(),
                f,
            );
        }
    }

    /// The walk over buffers of at least `text.len() + 1` entries each.
    #[inline(always)]
    fn occurrences_with(
        &self,
        text: &str,
        labels: &mut [MaybeUninit<u16>],
        ends: &mut [MaybeUninit<usize>],
        mut f: impl FnMut(usize, usize, usize, usize, u64),
    ) {
        let b = text.as_bytes();
        assert!(labels.len() > b.len() && ends.len() > b.len());
        let (table, table_len) = (self.table.as_ptr(), self.table.len() / 2);
        ends[0].write(0);
        let mut n = 0;
        let mut i = 0;
        while i < b.len() {
            // SAFETY: `text` is UTF-8 and `i` a character boundary inside it; a character is a
            // byte at least, so `n < i + 1 <= b.len()`, inside both buffers by the assert.
            unsafe {
                let (cp, w) = decode(b, i);
                // Written as `u16`: every label is, and the table stores them so.
                labels
                    .get_unchecked_mut(n)
                    .write(self.label(table, table_len, cp) as u16);
                i += w;
                n += 1;
                ends.get_unchecked_mut(n).write(i);
            }
        }
        // Every walk ends on this label at the latest.
        labels[n].write(0);
        // SAFETY: the first `n + 1` entries of both are written above.
        let (labels, ends) = unsafe {
            (
                std::slice::from_raw_parts(labels.as_ptr().cast::<u16>(), n + 1),
                std::slice::from_raw_parts(ends.as_ptr().cast::<usize>(), n + 1),
            )
        };
        let (slots, n_slots) = (self.slots.as_ptr(), self.slots.len() / 8);
        for p in 0..n {
            let mut base = 0usize;
            let mut j = p;
            loop {
                // SAFETY: a walk from `p` stops at the final 0 label at the latest, so `j <= n`.
                let x = u64::from(unsafe { *labels.get_unchecked(j) });
                if x == 0 {
                    break;
                }
                // SAFETY: as in `id`.
                let v = unsafe { slot(slots, n_slots, base + x as usize) };
                if v & LABEL_MASK != x {
                    break;
                }
                j += 1;
                if v & WORD != 0 {
                    // SAFETY: `p < j <= n`, and `ends` has `n + 1` entries.
                    let (start, end) = unsafe { (*ends.get_unchecked(p), *ends.get_unchecked(j)) };
                    f(p, j, start, end, v >> ID_SHIFT);
                }
                if v & LEAF != 0 {
                    break;
                }
                base = ((v >> BASE_SHIFT) & BASE_MASK) as usize;
            }
        }
    }

    /// Serialise to `[magic "BDA1"][n u64][empty u64][slots u64][table u32][supp u32]
    /// [max_label u32][payload u64][reserved 12, zero][check u32][slots][code table]
    /// [supplementary characters]`,
    /// all little-endian. `check` is a hash of the header bytes before it and `payload` a hash of
    /// everything after the header.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.blob.to_vec()
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it.
    pub fn serialized_len(&self) -> usize {
        self.blob.len()
    }

    /// Reconstruct from [`DoubleArrayIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes: besides the framing and both checksums, the load walks every slot
    /// and refuses a blob in which one names a base whose row runs past the array or an id past
    /// the end, so a crafted blob answers wrong ids, never out-of-range ones, and the walks read
    /// without a bounds check. The walk is one pass over the slots: `load_mmap`, which is the
    /// mapping and this walk, takes 1.07 ms on jieba's 349 045-word lexicon.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::copy_of(bytes), true)
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes).
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            Ok(std::io::Write::write_all(w, self.blob.as_ref())?)
        })
    }

    /// Load an index previously written with [`DoubleArrayIndex::save`]. Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and read the slots where they lie. Skips the payload checksum
    /// [`load`](Self::load) verifies, but not the walk over the slots, which is what lets the
    /// queries read without a bounds check.
    ///
    /// # Safety
    /// The file must not be modified or truncated by any process while the returned index is
    /// alive, because the index borrows the mapping. A crafted file is *not* undefined behaviour
    /// here — it is merely wrong. See [`StringIndex::load_mmap`](crate::StringIndex::load_mmap)
    /// for the full contract.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }

    /// Load `bytes` both ways and query what loaded; `true` if the checked way did. For the
    /// libFuzzer target in `fuzz/`, which cannot reach the loader's internals: see the
    /// `lexindex::fuzzing` module.
    ///
    /// The probes are spelled from the blob's own characters, so that the walks go past the root:
    /// every id inside `[0, n)`, every match ending on a character boundary after the one before
    /// it, the occurrence walk agreeing with the prefix walk from every character, and the blob
    /// written back byte for byte.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_load_and_query(bytes: &[u8]) -> bool {
        let checked = Self::from_bytes(bytes).ok();
        let framed = Self::from_shared(SharedBytes::copy_of(bytes), false).ok();
        assert!(
            checked.is_none() || framed.is_some(),
            "the framing is the checked path's"
        );
        for idx in checked.iter().chain(&framed) {
            assert_eq!(
                idx.to_bytes(),
                bytes,
                "the blob is not written back as read"
            );
            let n = idx.len() as u64;
            let table = idx.table.as_ref();
            let mut chars: Vec<char> = (0..table.len() / 2)
                .filter(|&cp| table[2 * cp] | table[2 * cp + 1] != 0)
                .filter_map(|cp| char::from_u32(cp as u32))
                .chain(idx.supp.iter().filter_map(|&(cp, _)| char::from_u32(cp)))
                .take(48)
                .collect();
            chars.push('\u{10FFFF}');
            let mut probes: Vec<String> = vec![String::new(), chars.iter().collect()];
            for &a in &chars {
                probes.push(a.to_string());
                for &b in chars.iter().take(6) {
                    probes.push([a, b, a].iter().collect());
                }
            }
            for q in &probes {
                assert!(idx.id(q).is_none_or(|id| id < n), "id({q:?})");
                let mut prefixes = Vec::new();
                idx.for_each_common_prefix(q, |end, id| prefixes.push((end, id)));
                let mut last = None;
                for &(end, id) in &prefixes {
                    assert!(
                        id < n && q.is_char_boundary(end),
                        "prefix ({end}, {id}) of {q:?}"
                    );
                    assert!(
                        last.is_none_or(|l| l < end || (l == 0 && end == 0)),
                        "order in {q:?}"
                    );
                    last = Some(end);
                }
                let mut want = Vec::new();
                for (start, _) in q.char_indices() {
                    idx.for_each_common_prefix(&q[start..], |end, id| {
                        if end > 0 {
                            want.push((start, start + end, id));
                        }
                    });
                }
                let mut got = Vec::new();
                idx.for_each_occurrence(q, |start, end, id| got.push((start, end, id)));
                assert_eq!(got, want, "occurrences in {q:?}");
            }
        }
        checked.is_some()
    }

    /// The framing of `bytes`, validated, and every slot checked — see
    /// [`from_bytes`](Self::from_bytes).
    fn from_shared(blob: SharedBytes, verify_payload: bool) -> Result<Self, IndexError> {
        let bytes: &[u8] = &blob;
        if bytes.len() < HEADER || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        if u32_at(bytes, CHECKED) != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        if bytes[RESERVED..CHECKED].iter().any(|&b| b != 0) {
            return Err(IndexError::Format(
                "double-array: reserved header bytes set",
            ));
        }
        let n = u64_at(bytes, 4);
        let empty = u64_at(bytes, 12);
        let n_slots = u64_at(bytes, 20);
        let table_len = u32_at(bytes, 28) as usize;
        let supp_len = u32_at(bytes, 32) as usize;
        let max_label = u32_at(bytes, 36) as usize;
        if n > MAX_KEYS as u64 {
            return Err(IndexError::Format(
                "double-array: more keys than ids can name",
            ));
        }
        let n = n as usize;
        let empty = match empty {
            u64::MAX => None,
            id if id < n as u64 => Some(id),
            _ => {
                return Err(IndexError::Format(
                    "double-array: the empty key's id is past the end",
                ));
            }
        };
        if n_slots > MAX_SLOTS as u64 {
            return Err(IndexError::Format(
                "double-array: more slots than bases can name",
            ));
        }
        let n_slots = n_slots as usize;
        if table_len > BMP as usize || supp_len > 0x11_0000 - BMP as usize {
            return Err(IndexError::Format(
                "double-array: code table length out of range",
            ));
        }
        if max_label > MAX_ALPHABET
            || (max_label == 0) != (n_slots == 0)
            || max_label >= n_slots.max(1)
        {
            return Err(IndexError::Format("double-array: label range out of range"));
        }
        // Every length is bounded above, so none of these sums can overflow.
        let table_at = HEADER + 8 * n_slots;
        let supp_at = table_at + 2 * table_len;
        if bytes.len() != supp_at + SUPP_ENTRY * supp_len {
            return Err(IndexError::Format(
                "double-array: section lengths do not meet the blob's",
            ));
        }
        if verify_payload && u64_at(bytes, 40) != crate::blob::hash_block(&bytes[HEADER..]) {
            return Err(IndexError::Format("payload checksum mismatch"));
        }
        for e in bytes[table_at..supp_at].chunks_exact(2) {
            if usize::from(u16::from_le_bytes([e[0], e[1]])) > max_label {
                return Err(IndexError::Format(
                    "double-array: a code-table label past the largest",
                ));
            }
        }
        let mut supp = Vec::with_capacity(supp_len);
        for e in bytes[supp_at..].chunks_exact(SUPP_ENTRY) {
            let (cp, label) = (u32_at(e, 0), u32_at(e, 4));
            let ascending = supp.last().is_none_or(|&(prev, _)| prev < cp);
            if !(BMP..0x11_0000).contains(&cp) || !ascending {
                return Err(IndexError::Format(
                    "double-array: supplementary characters out of range or order",
                ));
            }
            if label == 0 || label as usize > max_label {
                return Err(IndexError::Format(
                    "double-array: a supplementary label out of range",
                ));
            }
            supp.push((cp, label as u16));
        }
        if slots_malformed(&bytes[HEADER..table_at], n as u64, max_label, n_slots) {
            return Err(IndexError::Format(
                "double-array: a slot names a row past the array or an id past the end, or holds \
                 bits no build sets",
            ));
        }
        let slots = blob.subslice(HEADER, table_at).expect("inside the blob");
        let table = blob.subslice(table_at, supp_at).expect("inside the blob");
        Ok(Self {
            blob,
            slots,
            table,
            supp: supp.into_boxed_slice(),
            n,
            empty,
        })
    }
}

impl std::fmt::Debug for DoubleArrayIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubleArrayIndex")
            .field("len", &self.n)
            .field("bytes", &self.serialized_len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringIndex;

    /// Every shape the build and the walks have a case for: the empty key, NUL, one to four bytes
    /// a character, the planes past the first, keys that are prefixes of keys, a lone leaf under
    /// the root.
    fn keys() -> Vec<String> {
        [
            "",
            "\0",
            "a",
            "ab",
            "abc",
            "abd",
            "b",
            "é",
            "éa",
            "北",
            "北京",
            "北京大学",
            "大学",
            "大学生",
            "学生",
            "生",
            "😀",
            "😀😀",
            "𠀀",
            "𠀀北",
            "a😀",
            "北a",
            "zzz",
            "\u{FFFF}",
            "\u{10FFFF}",
            "日本語",
            "日本",
            "本",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn texts() -> Vec<String> {
        [
            "",
            "北京大学生",
            "abcde",
            "x北京y大学z",
            "😀😀😀𠀀北京",
            "\0a\0ab",
            "日本語の本",
            "\u{FFFF}\u{10FFFF}",
            "ééa",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn prefixes_by_string_index(s: &StringIndex, q: &str) -> Vec<(usize, u64)> {
        let mut out = Vec::new();
        s.for_each_common_prefix(q, |end, id| out.push((end, id)));
        out
    }

    fn check(keys: &[String], queries: &[String]) {
        let da = DoubleArrayIndex::build(keys).unwrap();
        let si = StringIndex::build(keys).unwrap();
        assert_eq!(da.len(), si.len());
        let mut all: Vec<String> = queries.to_vec();
        for k in keys {
            all.push(k.clone());
            all.push(format!("{k}x"));
            all.push(format!("{k}北"));
            all.push(format!("{k}😀"));
            let mut cut = k.clone();
            cut.pop();
            all.push(cut);
        }
        for q in &all {
            assert_eq!(da.id(q), si.id(q), "id of {q:?}");
            assert_eq!(da.contains(q), si.contains(q));
            let mut got = Vec::new();
            da.for_each_common_prefix(q, |end, id| got.push((end, id)));
            assert_eq!(got, prefixes_by_string_index(&si, q), "prefixes of {q:?}");
            assert_eq!(da.common_prefix(q), si.common_prefix(q));
            assert_eq!(da.longest_prefix(q), si.longest_prefix(q));
            // Occurrences: the prefix walk from every character, the empty key left out.
            let mut want = Vec::new();
            for (start, _) in q.char_indices() {
                for (end, id) in prefixes_by_string_index(&si, &q[start..]) {
                    if end > 0 {
                        want.push((start, start + end, id));
                    }
                }
            }
            let mut got = Vec::new();
            da.for_each_occurrence(q, |start, end, id| got.push((start, end, id)));
            assert_eq!(got, want, "occurrences in {q:?}");
        }
    }

    #[test]
    fn agrees_with_string_index() {
        check(&keys(), &texts());
        // Without the empty key, and with it alone.
        let rest: Vec<String> = keys().into_iter().filter(|k| !k.is_empty()).collect();
        check(&rest, &texts());
        check(&[String::new()], &texts());
        check(&[], &texts());
        // A text past the stack buffers.
        let long: String = std::iter::repeat_n("北京大学生😀abc", 40).collect();
        assert!(long.len() > STACK_TEXT);
        check(&keys(), &[long]);
    }

    #[test]
    fn ids_are_byte_order_ranks_and_duplicates_collapse() {
        let idx = DoubleArrayIndex::build(["b", "a", "北", "a", ""]).unwrap();
        assert_eq!(idx.len(), 4);
        assert_eq!(
            ["", "a", "b", "北"].map(|k| idx.id(k)),
            [Some(0), Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn round_trips_through_bytes_and_files() {
        let idx = DoubleArrayIndex::build(keys()).unwrap();
        let bytes = idx.to_bytes();
        assert_eq!(bytes.len(), idx.serialized_len());
        let back = DoubleArrayIndex::from_bytes(&bytes).unwrap();
        assert_eq!(back.to_bytes(), bytes);
        let path = std::env::temp_dir().join(format!("lexindex_da_{}.bda", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let loaded = DoubleArrayIndex::load(&path).unwrap();
        #[cfg(feature = "mmap")]
        // SAFETY: the file is this test's own and nothing writes it while the index lives.
        let mapped = unsafe { DoubleArrayIndex::load_mmap(&path) }.unwrap();
        for k in keys() {
            assert_eq!(loaded.id(&k), idx.id(&k));
            #[cfg(feature = "mmap")]
            assert_eq!(mapped.id(&k), idx.id(&k));
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_same_keys_always_produce_the_same_blob() {
        let a = DoubleArrayIndex::build(keys()).unwrap().to_bytes();
        let mut reversed = keys();
        reversed.reverse();
        assert_eq!(DoubleArrayIndex::build(reversed).unwrap().to_bytes(), a);
    }

    /// Every truncation and every flipped byte of a small blob, and a stride through a large one:
    /// `keys()` reach U+FFFF, so its code table alone is 128 KiB.
    #[test]
    fn from_bytes_never_panics_on_a_truncated_or_flipped_blob() {
        let small = DoubleArrayIndex::build(["", "a", "ab", "é", "éa", "b"])
            .unwrap()
            .to_bytes();
        let large = DoubleArrayIndex::build(keys()).unwrap().to_bytes();
        for (bytes, step) in [(&small, 1), (&large, 97)] {
            for len in (0..bytes.len()).step_by(step) {
                assert!(DoubleArrayIndex::from_bytes(&bytes[..len]).is_err());
            }
            for at in (0..bytes.len()).step_by(step) {
                let mut b = bytes.clone();
                b[at] ^= 0x40;
                assert!(
                    DoubleArrayIndex::from_bytes(&b).is_err(),
                    "flip at {at} loaded"
                );
            }
        }
    }

    /// Re-hash a tampered blob, so that only the structural checks stand between it and a query.
    fn rehash(b: &mut [u8]) {
        let payload = crate::blob::hash_block(&b[HEADER..]);
        b[40..48].copy_from_slice(&payload.to_le_bytes());
        let check = crate::blob::hash_bytes(&b[..CHECKED]) as u32;
        b[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
    }

    /// The checks the walks rely on are structural, not the checksums: an attacker recomputes
    /// those. A slot naming a row past the array, an id past the end, a label past the largest —
    /// each refused with valid checksums.
    #[test]
    fn a_rehashed_blob_is_still_refused_where_a_walk_would_read_out_of_bounds() {
        let idx = DoubleArrayIndex::build(keys()).unwrap();
        let bytes = idx.to_bytes();
        let n_slots = u64_at(&bytes, 20) as usize;
        let slot_at = |b: &[u8], s: usize| u64_at(b, HEADER + 8 * s);
        let inner = (0..n_slots)
            .find(|&s| {
                let v = slot_at(&bytes, s);
                v & LABEL_MASK != 0 && v & LEAF == 0
            })
            .unwrap();
        let word = (0..n_slots)
            .find(|&s| slot_at(&bytes, s) & WORD != 0)
            .unwrap();
        let tamper = |s: usize, v: u64| {
            let mut b = bytes.clone();
            b[HEADER + 8 * s..HEADER + 8 * s + 8].copy_from_slice(&v.to_le_bytes());
            rehash(&mut b);
            b
        };
        let v = slot_at(&bytes, inner);
        let far = (v & !(BASE_MASK << BASE_SHIFT)) | ((n_slots as u64 - 1) << BASE_SHIFT);
        assert!(DoubleArrayIndex::from_bytes(&tamper(inner, far)).is_err());
        let v = slot_at(&bytes, word);
        let past = (v & ((1 << ID_SHIFT) - 1)) | ((idx.len() as u64) << ID_SHIFT);
        assert!(DoubleArrayIndex::from_bytes(&tamper(word, past)).is_err());
        // A code-table label past the largest.
        let mut b = bytes.clone();
        let table_at = HEADER + 8 * n_slots;
        b[table_at..table_at + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        rehash(&mut b);
        assert!(DoubleArrayIndex::from_bytes(&b).is_err());
        // And the untouched blob, re-hashed, still loads.
        let mut b = bytes.clone();
        rehash(&mut b);
        assert!(DoubleArrayIndex::from_bytes(&b).is_ok());
    }

    #[test]
    fn more_characters_than_labels_is_refused() {
        let keys: Vec<String> = (0x4E00u32..)
            .filter_map(char::from_u32)
            .chain((0x20000u32..).filter_map(char::from_u32))
            .take(MAX_ALPHABET + 1)
            .map(String::from)
            .collect();
        let err = DoubleArrayIndex::build(&keys[..MAX_ALPHABET + 1]).unwrap_err();
        assert!(err.to_string().contains("65 535"));
    }
}
