//! A dictionary of phrases the whole blob shares, and the byte codes a shard spends to name them.
//!
//! [`fsst`](crate::fsst) trains a table of at most eight bytes a symbol on each shard of 65 536
//! keys. That is the right structure for the bytes inside a token and the wrong one for the tokens
//! themselves: `https://`, `/wiki/`, `.html`, `index`, `2024` recur across a whole corpus, are
//! longer than eight bytes as often as not, and a shard's table can hold 255 of anything. A phrase
//! section holds them once for the blob, and a shard gives up some of its byte codes to point at
//! them: `symbols` codes stay the table's, one stays the raw escape, and the rest become phrase
//! prefixes — `wide` of them naming 65 536 phrases in two further bytes and the others 256 in one.
//!
//! What a shard spends is its own decision, priced on its own bytes: a shard of English titles buys
//! phrases and a shard of UUIDs does not, in the same blob. Measured over the suffix streams of a
//! million keys: article titles 7.81 → 5.88 bytes a key, URLs 7.93 → 5.86, paths 10.82 → 7.73,
//! identifiers −23.6 %, and `words`, `uuid`, `numeric` and `opaque` left alone because the phrases
//! never earn their dictionary there.

use crate::fsst::{self, ESCAPE, Table};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// The longest phrase, which is also how far a parse looks ahead at one position.
pub(crate) const MAX: usize = 32;
/// Adjacent tokens a mined window may span.
///
/// Three, because the parse a round mines under is already near-optimal: a candidate that beats
/// what covers it rarely needs more, and the long phrases are reached by the rounds merging what
/// the last one kept. Measured on a million URLs, 16 spends 5.2 s and 863 MB to reach 7.4875 bytes
/// a key, 3 spends 2.1 s and 342 MB to reach 7.4820, and 2 breaks the miner outright (9.25) — a
/// span of two tokens is worth about what its parts are.
const WINDOW: usize = 3;
/// Rounds of parse-and-count. Each one parses under the candidates the last kept, so a phrase is
/// scored against the phrases it competes with rather than against the symbols alone, and each one
/// can merge what the last kept into something longer. Four is the knee: the fifth was worth
/// 0.001 bytes a key on a million URLs and a fifth of the build.
const ROUNDS: usize = 4;
/// Shards the miner asks before spending its remaining rounds, and suffixes it asks them about.
const PROBE_SHARDS: usize = 3;
const PROBE_PIECES: usize = 2_048;
/// Per cent a shard's split has to win by before it is worth taking -- asked of a probe here,
/// before the miner spends its remaining rounds, and of the shard itself in `Collected::settle`.
///
/// Not a byte, because a shard that names phrases pays for them at every lookup: every code of
/// its suffixes is asked whether it is a phrase before it is read as a symbol. Measured at block
/// 256 -- on a word list the phrases took 0.9 % of the blob for 2 % of an `id`, 5 % of a
/// `key_into` and twice the build; on a million URLs they took 21 % for 4 % and 12 %. Every
/// corpus measured falls on one side or the other: 0 or 0.9 % against 5.7 to 27.6 %.
pub(crate) const MARGIN: u64 = 3;
/// Passes of the pruning fixed point. Two: the first drops the candidates nothing picks, the
/// second what is left once they are gone, and the loop stops early when nothing moved.
const PRUNE_ROUNDS: usize = 2;
/// Candidates a round carries into the next. An uncapped pool ranks length above reuse — whole
/// rare spans score as one phrase — and cost a prototype 1.2 bytes a key on article titles.
const CAP: usize = 32_768;
/// Sampled suffixes a candidate must have to compete for a slot. A vocabulary as large as the
/// sample is the same failure as no cap at all: every suffix parses as itself, and a phrase used
/// once is a phrase the dictionary pays for and nothing names twice.
const PIECES_PER_PHRASE: usize = 4;
/// Candidate spans one pool holds at once. A corpus of long keys offers millions of them and
/// almost all are seen once: past this the map drops everything at or below a rising floor, which
/// is what keeps a build of a million paths inside a gigabyte.
const GAIN_CAP: usize = 1 << 18;
/// Pools the shards are mined in, which is what [`GAIN_CAP`] applies to and therefore what decides
/// the vocabulary. A constant, and deliberately not the thread count: a pool keeps the candidates
/// above its own median, so a pool that held twice as many shards kept a different half of them,
/// and the blob came out different on a machine with fewer cores -- measured before this was a
/// constant, one to sixteen cores gave three blobs of a million urls and six of a million paths.
/// Sixteen is the number that leaves every blob this crate has published unchanged.
const POOLS: usize = 16;
/// Suffixes the miner reads, spread over the shard samples. The vocabulary is the corpus's, not one
/// shard's, but it converges long before every sample is in: this bounds the miner's map rather
/// than its answer.
const PIECES: usize = 400_000;
/// Phrases a dictionary group holds, which is what a two-byte end inside the group is enough for.
const GROUP: usize = 256;
/// The symbol counts a shard may keep for itself; the rest of the byte space goes to phrases. The
/// last keeps everything but one code, which then names 256 phrases in one more byte or 65 536 in
/// two — the cheapest way a shard that mostly wants its symbols can still buy a few phrases.
const SYMBOLS: [u8; 5] = [127, 191, 223, 239, 254];
/// How far a round's gain overstates what keeping the phrase is worth, as the factor its storage
/// is charged at.
///
/// A round scores a span against the tokens it would replace *today*, and almost every span has a
/// near-substitute: drop it and its bytes are covered by a shorter phrase or a symbol at little
/// more cost, so the marginal saving is a fraction of the counted gain. Measured over the whole
/// blob at 1 M keys, the fraction is about a sixth — at 4 the vocabulary costs more than it saves
/// (urls 7.52 against 7.51) and at 8 the shards start giving phrases up altogether (7.71).
const STRICT: u64 = 6;

/// How a shard spends its byte codes.
///
/// `symbols` of them name a symbol, 255 is the raw escape, and the `255 - symbols` between are
/// phrase prefixes: the last `wide` name 65 536 phrases in two further bytes, the others 256 in
/// one. A phrase is therefore two bytes while the cheap prefixes last and three after that, which
/// is what makes the dictionary worth ranking — the ids that are used most are the ones that fit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Split {
    symbols: u8,
    wide: u8,
}

impl Split {
    /// The prefix codes, of which `wide` are the wide ones.
    #[inline(always)]
    fn prefixes(self) -> usize {
        255 - self.symbols as usize
    }

    /// Ids that cost two bytes.
    #[inline(always)]
    fn two(self) -> usize {
        (self.prefixes() - self.wide as usize) * GROUP
    }

    /// Ids the split can name at all.
    #[inline(always)]
    fn limit(self) -> usize {
        self.two() + self.wide as usize * (1 << 16)
    }

    #[inline(always)]
    pub(crate) fn symbols(self) -> u8 {
        self.symbols
    }

    /// `[symbols u8][wide u8]`, and what a blob holds there; `None` on a pair that names more
    /// prefixes than the byte space has.
    pub(crate) fn read(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let ([symbols, wide], rest) = bytes.split_first_chunk::<2>()?;
        let split = Self {
            symbols: *symbols,
            wide: *wide,
        };
        (*symbols > 0 && *symbols < ESCAPE && usize::from(*wide) <= split.prefixes())
            .then_some((split, rest))
    }

    pub(crate) fn write_to(self, out: &mut Vec<u8>) {
        out.push(self.symbols);
        out.push(self.wide);
    }

    /// What a parse charges under this split.
    #[inline(always)]
    fn prices(self) -> Prices {
        Prices {
            two: self.two(),
            limit: self.limit(),
        }
    }

    /// The phrase the stream at `at` names, and the bytes it took; `None` when the code is not a
    /// phrase prefix or the stream ends inside the reference.
    #[inline(always)]
    pub(crate) fn read_at(self, stream: &[u8], at: usize) -> Option<(usize, usize)> {
        let code = *stream.get(at)? as usize;
        if code < self.symbols as usize || code == ESCAPE as usize {
            return None;
        }
        let class = code - self.symbols as usize;
        let narrow = self.prefixes() - self.wide as usize;
        if class < narrow {
            Some((class * GROUP + usize::from(*stream.get(at + 1)?), 2))
        } else {
            let hi = usize::from(*stream.get(at + 1)?);
            let lo = usize::from(*stream.get(at + 2)?);
            Some((
                self.two() + (class - narrow) * (1 << 16) + (hi << 8) + lo,
                3,
            ))
        }
    }

    /// The reference to phrase `id`, appended to `out`.
    #[inline]
    fn write(self, id: usize, out: &mut Vec<u8>) {
        let narrow = self.prefixes() - self.wide as usize;
        if id < self.two() {
            out.push(self.symbols + (id / GROUP) as u8);
            out.push((id % GROUP) as u8);
        } else {
            let above = id - self.two();
            out.push(self.symbols + (narrow + (above >> 16)) as u8);
            out.push((above >> 8) as u8);
            out.push(above as u8);
        }
    }

    /// The splits a shard chooses between for a dictionary of `phrases`: every symbol count, with
    /// the fewest wide prefixes that reach the whole dictionary, since a wide prefix is one fewer
    /// cheap code.
    fn family(phrases: usize) -> Vec<Split> {
        let mut out = Vec::with_capacity(2 * SYMBOLS.len());
        for &symbols in &SYMBOLS {
            let q = 255 - symbols as usize;
            for wide in [
                0usize,
                (0..=q)
                    .find(|&j| (q - j) * GROUP + j * (1 << 16) >= phrases)
                    .unwrap_or(q),
            ] {
                let split = Split {
                    symbols,
                    wide: wide as u8,
                };
                if !out.contains(&split) {
                    out.push(split);
                }
            }
        }
        out
    }
}

/// What a parse charges for a phrase: two bytes below `two`, three up to `limit`, and nothing
/// above it because no code names it.
///
/// A [`Split`] is one of these and a way of writing the bytes; the miner is the other, where every
/// candidate is scored as if the cheap prefixes had no end. Scoring under a real split instead
/// bounds the vocabulary at that split's limit, and the rounds then rank the same few thousand
/// spans against each other however large the pool is.
#[derive(Clone, Copy)]
struct Prices {
    two: usize,
    limit: usize,
}

impl Prices {
    const MINING: Self = Self {
        two: usize::MAX,
        limit: usize::MAX,
    };

    #[inline(always)]
    fn bytes_for(self, id: usize) -> usize {
        if id < self.two { 2 } else { 3 }
    }
}

/// The phrases a blob's suffix streams name, by id.
///
/// The bytes lie end to end in id order, which is rank order: a group of 256 shares a `u32` base
/// and each phrase in it ends at a `u16` inside that group, so a phrase is two loads and no walk.
/// Stored whole rather than front-coded against its neighbour — rank order is not lexicographic, so
/// there is nothing to share.
pub(crate) struct Dict {
    bytes: Vec<u8>,
    /// Where each group of [`GROUP`] phrases starts.
    bases: Vec<u32>,
    /// Where each phrase ends inside its group, with the opening zero of every group, so group `g`
    /// owns `ends[257 * g ..]`.
    ends: Vec<u16>,
    count: usize,
}

/// Entries a group's slice of [`Dict::ends`] holds: one a phrase and the opening zero.
const ENDS: usize = GROUP + 1;

impl Dict {
    pub(crate) fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            bases: Vec::new(),
            ends: Vec::new(),
            count: 0,
        }
    }

    pub(crate) fn of(phrases: &[Vec<u8>]) -> Self {
        let mut dict = Self::empty();
        for group in phrases.chunks(GROUP) {
            dict.bases.push(dict.bytes.len() as u32);
            dict.ends.push(0);
            for p in group {
                debug_assert!((1..=MAX).contains(&p.len()));
                dict.bytes.extend_from_slice(p);
                dict.ends.push(
                    (dict.bytes.len() - *dict.bases.last().expect("a group is open") as usize)
                        as u16,
                );
            }
            dict.ends
                .resize(dict.ends.len() + ENDS - (group.len() + 1), 0);
        }
        dict.count = phrases.len();
        dict
    }

    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// The bytes of phrase `id`; `None` past the dictionary or on one this crate did not write.
    #[inline(always)]
    pub(crate) fn at(&self, id: usize) -> Option<&[u8]> {
        if id >= self.count {
            return None;
        }
        let (g, k) = (id / GROUP, id % GROUP);
        let base = *self.bases.get(g)? as usize;
        let start = base + usize::from(*self.ends.get(ENDS * g + k)?);
        let end = base + usize::from(*self.ends.get(ENDS * g + k + 1)?);
        self.bytes.get(start..end)
    }

    /// `[count u32][bases][ends][bytes]`, the two arrays derived from the count.
    pub(crate) fn serialized_len(&self) -> usize {
        4 + 4 * self.bases.len() + 2 * self.ends.len() + self.bytes.len()
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.count as u32).to_le_bytes());
        for &b in &self.bases {
            out.extend_from_slice(&b.to_le_bytes());
        }
        for &e in &self.ends {
            out.extend_from_slice(&e.to_le_bytes());
        }
        out.extend_from_slice(&self.bytes);
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Self> {
        let (count, rest) = bytes.split_first_chunk::<4>()?;
        let count = u32::from_le_bytes(*count) as usize;
        let groups = count.div_ceil(GROUP);
        let (bases, rest) = rest.split_at_checked(4 * groups)?;
        let (ends, bytes) = rest.split_at_checked(2 * ENDS * groups)?;
        let dict = Self {
            bytes: bytes.to_vec(),
            bases: bases
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().expect("4 bytes")))
                .collect(),
            ends: ends
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes(c.try_into().expect("2 bytes")))
                .collect(),
            count,
        };
        // Every phrase has to name bytes that are there and to be a phrase a parse could have
        // written, or a lookup would read somebody else's.
        (0..count)
            .all(|id| dict.at(id).is_some_and(|p| (1..=MAX).contains(&p.len())))
            .then_some(dict)
    }
}

/// A hash of the bytes a span holds, which is what the miner counts on. FNV in one lane and a
/// multiply-rotate in the other, so a map of millions of spans has no collisions worth the walk.
///
/// Folded a byte at a time from the left, so the windows out of one position extend one state
/// rather than rehashing a span each time — which is the miner's inner loop.
#[derive(Clone, Copy)]
struct Span(u64, u64);

impl Span {
    const NEW: Self = Self(0xcbf2_9ce4_8422_2325, 0x9E37_79B9_7F4A_7C15);

    #[inline(always)]
    fn push(&mut self, bytes: &[u8]) {
        for &x in bytes {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3);
            self.1 = (self.1.rotate_left(5) ^ u64::from(x)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    #[inline(always)]
    fn key(self, len: usize) -> u128 {
        (u128::from(self.0) << 64 | u128::from(self.1)) ^ len as u128
    }
}

/// A hasher for keys that are already hashes, or are a word: one multiply, since the map is keyed
/// on [`span_hash`], on trie edges, and on the symbol trainer's symbols packed in a word.
#[derive(Default)]
pub(crate) struct Mix(u64);

impl Hasher for Mix {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0 ^ v)
            .wrapping_mul(0x517c_c1b7_2722_0a95)
            .rotate_left(26);
    }

    fn write_u128(&mut self, v: u128) {
        self.write_u64(v as u64);
        self.write_u64((v >> 64) as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type Map<K, V> = HashMap<K, V, BuildHasherDefault<Mix>>;

/// One thread's gains in one merge partition: what a span saves, and one place it occurs.
type Gains<'a> = Map<u128, (u64, &'a [u8])>;

/// The phrases, as a parse walks them: one edge a byte, so a position that starts no phrase costs
/// one lookup and stops.
///
/// A double array: a node is a slot, and its child on byte `b` is slot `base + b`, which holds one
/// when its `check` names the node. A step is two dependent loads whatever the fan-out, where the
/// hash probe it replaced was most of the parse and a scan of a node's children was no better —
/// the nodes a walk meets second and third have twenty and five children apiece on URLs.
pub(crate) struct Trie {
    slots: Vec<Slot>,
}

/// One slot of the double array, the three fields together so that a step reads one record for
/// the node and one for the child.
#[derive(Clone, Copy)]
struct Slot {
    base: u32,
    /// The parent of the node in this slot.
    check: u32,
    /// The phrase the node in this slot ends, plus one; zero for a node that ends none.
    phrase: u32,
}

/// A slot no node occupies. Its `check` is the root's too: no node has that id, so no step lands
/// on either.
const VACANT: Slot = Slot {
    base: 0,
    check: u32::MAX,
    phrase: 0,
};

impl Trie {
    pub(crate) fn of<'a>(phrases: impl IntoIterator<Item = &'a [u8]>) -> Self {
        // Inserted through a map in phrase order, then laid out breadth first, so that the wide
        // nodes near the root take their slots before the narrow ones that fill the gaps between.
        let mut edges: Map<u64, u32> = Map::default();
        let mut ends = vec![0u32];
        let mut links: Vec<(u32, u8, u32)> = Vec::new();
        for (id, p) in phrases.into_iter().enumerate() {
            let mut node = 0u32;
            for &b in p {
                let key = u64::from(node) << 8 | u64::from(b);
                node = match edges.get(&key) {
                    Some(&c) => c,
                    None => {
                        let c = ends.len() as u32;
                        ends.push(0);
                        edges.insert(key, c);
                        links.push((node, b, c));
                        c
                    }
                };
            }
            ends[node as usize] = id as u32 + 1;
        }
        links.sort_unstable();
        let mut off = vec![0u32; ends.len() + 1];
        for &(parent, _, _) in &links {
            off[parent as usize + 1] += 1;
        }
        for i in 0..ends.len() {
            off[i + 1] += off[i];
        }
        let mut trie = Self {
            slots: vec![Slot {
                phrase: ends[0],
                ..VACANT
            }],
        };
        // One bit a slot, so that a run of taken slots is skipped a word at a time; a slot past
        // the end is free.
        let mut taken = vec![1u64];
        let free_from = |taken: &[u64], mut slot: usize| {
            while let Some(&word) = taken.get(slot / 64) {
                let rest = !word >> (slot % 64);
                if rest != 0 {
                    return slot + rest.trailing_zeros() as usize;
                }
                slot = (slot / 64 + 1) * 64;
            }
            slot
        };
        let is_free = |taken: &[u64], slot: usize| {
            taken
                .get(slot / 64)
                .is_none_or(|w| (w >> (slot % 64)) & 1 == 0)
        };
        // The nodes in the order they are laid out, each with its slot: the root, then children
        // as their parents place them.
        let mut order = vec![(0u32, 0u32)];
        // The lowest free slot, where every search starts. It skips the first 256: a slot below
        // `b` cannot hold a child on byte `b`, so a hole left there would stall it under the whole
        // taken run, and every node's search would rescan that run.
        let mut lowest = 256;
        let mut i = 0;
        while i < order.len() {
            let (old, parent) = order[i];
            let old = old as usize;
            i += 1;
            let kids = &links[off[old] as usize..off[old + 1] as usize];
            let Some(&(_, first, _)) = kids.first() else {
                continue;
            };
            // First fit: the lowest free slot the first child can take whose siblings' slots are
            // free too.
            let mut f = free_from(&taken, lowest);
            let base = loop {
                let base = f - usize::from(first);
                if kids
                    .iter()
                    .all(|&(_, b, _)| is_free(&taken, base + usize::from(b)))
                {
                    break base;
                }
                f = free_from(&taken, f + 1);
            };
            trie.slots[parent as usize].base = base as u32;
            for &(_, b, c) in kids {
                let s = base + usize::from(b);
                if s >= trie.slots.len() {
                    trie.slots.resize(s + 1, VACANT);
                    taken.resize(s / 64 + 1, 0);
                }
                taken[s / 64] |= 1 << (s % 64);
                trie.slots[s].check = parent;
                trie.slots[s].phrase = ends[c as usize];
                order.push((c, s as u32));
            }
            lowest = free_from(&taken, lowest);
        }
        trie
    }

    pub(crate) fn empty() -> Self {
        Self::of(std::iter::empty())
    }

    /// The child of `node` on `b`, with the phrase it ends plus one.
    #[inline(always)]
    fn step(&self, node: u32, b: u8) -> Option<(u32, u32)> {
        let slot = self.slots[node as usize].base as usize + usize::from(b);
        let child = self.slots.get(slot)?;
        (child.check == node).then_some((slot as u32, child.phrase))
    }
}

/// What a token of a parse is.
const SYMBOL: u8 = 0;
const RAW: u8 = 1;
const PHRASE: u8 = 2;

/// The two arrays a parse fills, kept across calls so a shard allocates once.
#[derive(Default)]
pub(crate) struct Scratch {
    cost: Vec<u32>,
    /// Per byte, the token the cheapest coding from there starts with: kind, symbol code or phrase
    /// id, and the input bytes it covers.
    pick: Vec<(u8, u32, u32)>,
    tokens: Vec<(usize, usize, u8, u32)>,
}

/// The cheapest coding of `s`, **in bits**, leaving its tokens in `w`: a symbol costs eight, an
/// escaped byte sixteen, a phrase what its rank makes it.
///
/// Cheapest rather than longest-first, because the two costs are not the same shape: a phrase of
/// six bytes at sixteen bits loses to two symbols of three at eight each, and a greedy walk cannot
/// see that. One pass backwards over the string, so it is linear in the bytes and in the phrases
/// that actually start inside it.
fn parse(
    s: &[u8],
    enc: &fsst::Encoder,
    trie: &Trie,
    prices: Option<Prices>,
    w: &mut Scratch,
) -> u32 {
    let n = s.len();
    w.cost.clear();
    w.cost.resize(n + 1, 0);
    w.pick.clear();
    w.pick.resize(n, (RAW, 0, 1));
    for i in (0..n).rev() {
        // The symbol table's own answer at this position, which is the longest it can match.
        let (code, len) = enc.step(fsst::word_at(s, i), n - i);
        let (mut best, mut choice) = if code == ESCAPE {
            (16 + w.cost[i + 1], (RAW, 0, 1))
        } else {
            (8 + w.cost[i + len], (SYMBOL, u32::from(code), len as u32))
        };
        if let Some(prices) = prices {
            let mut node = 0u32;
            for (k, &b) in s[i..n.min(i + MAX)].iter().enumerate() {
                let Some((c, phrase)) = trie.step(node, b) else {
                    break;
                };
                node = c;
                let id = phrase as usize;
                if id == 0 || id > prices.limit {
                    continue;
                }
                let cost = 8 * prices.bytes_for(id - 1) as u32 + w.cost[i + k + 1];
                if cost < best {
                    best = cost;
                    choice = (PHRASE, (id - 1) as u32, k as u32 + 1);
                }
            }
        }
        w.cost[i] = best;
        w.pick[i] = choice;
    }
    w.cost[0]
}

/// The tokens of the last [`parse`] of a string of `n` bytes, as `(start, length, kind, id)`.
fn tokens(w: &mut Scratch, n: usize) {
    w.tokens.clear();
    let mut i = 0;
    while i < n {
        let (kind, id, len) = w.pick[i];
        w.tokens.push((i, len as usize, kind, id));
        i += len as usize;
    }
}

/// Codes `s` under a shard's table and split, appending to `out`; returns the bytes written.
pub(crate) fn encode_into(
    s: &[u8],
    enc: &fsst::Encoder,
    trie: &Trie,
    split: Option<Split>,
    w: &mut Scratch,
    out: &mut Vec<u8>,
) -> usize {
    let before = out.len();
    parse(s, enc, trie, split.map(Split::prices), w);
    tokens(w, s.len());
    for &(start, len, kind, id) in &w.tokens {
        match kind {
            SYMBOL => out.push(id as u8),
            PHRASE => split
                .expect("a phrase token under no split")
                .write(id as usize, out),
            _ => {
                out.push(ESCAPE);
                out.push(s[start]);
            }
        }
        debug_assert!(len > 0);
    }
    out.len() - before
}

/// What a shard would spend on `pieces` under `split`, in bytes.
fn price(
    pieces: &[&[u8]],
    enc: &fsst::Encoder,
    trie: &Trie,
    split: Option<Split>,
    w: &mut Scratch,
) -> u64 {
    let prices = split.map(Split::prices);
    pieces
        .iter()
        .map(|p| u64::from(parse(p, enc, trie, prices, w)) / 8)
        .sum()
}

/// The spans of `pieces` a parse under `split` left to the symbols, appended to `out`.
fn residue<'a>(
    pieces: &[&'a [u8]],
    enc: &fsst::Encoder,
    trie: &Trie,
    split: Split,
    w: &mut Scratch,
    out: &mut Vec<&'a [u8]>,
) {
    for &p in pieces {
        parse(p, enc, trie, Some(split.prices()), w);
        tokens(w, p.len());
        let mut from = 0;
        for &(start, len, kind, _) in &w.tokens {
            if kind != PHRASE {
                continue;
            }
            if from < start {
                out.push(&p[from..start]);
            }
            from = start + len;
        }
        if from < p.len() {
            out.push(&p[from..]);
        }
    }
}

/// The split a shard is cheapest under, with the table to read it back — `None` when giving byte
/// codes up to phrases costs more than it saves, which is the answer on a corpus the miner found
/// nothing in.
///
/// `table` is the shard's own, trained on every byte. The candidates are priced under its first
/// `k` symbols, and the winner is then given a table trained for the bytes it actually leaves to
/// the symbols: which symbols earn most is a different question once the repeated spans are named
/// elsewhere, and on article titles the refit is worth 0.2 bytes a key on its own.
pub(crate) fn settle(
    pieces: &[&[u8]],
    table: &Table,
    trie: &Trie,
    phrases: usize,
    w: &mut Scratch,
) -> (Option<Split>, Table) {
    if phrases == 0 {
        return (None, table.clone());
    }
    let family = Split::family(phrases);
    let mut heads: Vec<u8> = Vec::with_capacity(family.len());
    for split in &family {
        if !heads.contains(&split.symbols) {
            heads.push(split.symbols);
        }
    }
    // Row zero is the shard's whole table with no phrases to spend on; the rest are the prefixes
    // the family asks for, one row each rather than one a split.
    let mut encs = Vec::with_capacity(heads.len() + 1);
    encs.push(table.encoder());
    encs.extend(heads.iter().map(|&k| table.head(usize::from(k)).encoder()));
    let rows: Vec<usize> = family
        .iter()
        .map(|s| 1 + heads.iter().position(|&k| k == s.symbols).unwrap_or(0))
        .collect();
    let mut bytes = vec![0u64; family.len() + 1];
    let mut memo = Memo::default();
    for &piece in pieces {
        memo.fill(piece, &encs, trie);
        bytes[0] += u64::from(memo.price(0, None, &mut w.cost)) / 8;
        for (j, split) in family.iter().enumerate() {
            bytes[j + 1] += u64::from(memo.price(rows[j], Some(split.prices()), &mut w.cost)) / 8;
        }
    }
    let mut low = bytes[0];
    let mut won = None;
    for (j, split) in family.iter().enumerate() {
        if bytes[j + 1] < low {
            low = bytes[j + 1];
            won = Some(*split);
        }
    }
    let Some(split) = won else {
        return (None, table.clone());
    };
    let mut cut = table.head(usize::from(split.symbols));
    let mut left = Vec::new();
    residue(pieces, &cut.encoder(), trie, split, w, &mut left);
    let refit = Table::train_to(&left, usize::from(split.symbols));
    if price(pieces, &refit.encoder(), trie, Some(split), w) < low {
        cut = refit;
    }
    (Some(split), cut)
}

/// One piece's parse inputs, kept while every candidate of a [`family`](Split::family) is priced
/// on it: what each candidate's symbols answer at every position, and every phrase the trie
/// matches there.
///
/// The ten splits of a family name five symbol prefixes between them and one trie, and a price
/// reads only the cost array a parse leaves — so the walk that fills these runs once a piece
/// rather than once a piece a split. The trie walk is the half that pays: it probes up to
/// [`MAX`] nodes at every position where the encoder answers once.
#[derive(Default)]
struct Memo {
    /// `(code, len)` at every position, one row an encoder, the rows end to end.
    steps: Vec<(u8, u8)>,
    /// `(length, one-based id)` of every phrase the trie matches, by where it starts.
    hits: Vec<(u8, u32)>,
    /// `hits[at[i]..at[i + 1]]` are the phrases that start at `i`.
    at: Vec<u32>,
    len: usize,
}

impl Memo {
    fn fill(&mut self, s: &[u8], encs: &[fsst::Encoder], trie: &Trie) {
        let n = s.len();
        self.len = n;
        self.steps.clear();
        self.steps.reserve(encs.len() * n);
        for enc in encs {
            self.steps.extend((0..n).map(|i| {
                let (code, len) = enc.step(fsst::word_at(s, i), n - i);
                (code, len as u8)
            }));
        }
        self.hits.clear();
        self.at.clear();
        self.at.push(0);
        for i in 0..n {
            let mut node = 0u32;
            for (k, &b) in s[i..n.min(i + MAX)].iter().enumerate() {
                let Some((c, phrase)) = trie.step(node, b) else {
                    break;
                };
                node = c;
                if phrase != 0 {
                    self.hits.push((k as u8 + 1, phrase));
                }
            }
            self.at.push(self.hits.len() as u32);
        }
    }

    /// The cheapest coding of the memoised piece, in bits, under the symbols of `row` and the
    /// phrase prices of `prices` — [`parse`](parse)'s recurrence over what is already walked.
    fn price(&self, row: usize, prices: Option<Prices>, cost: &mut Vec<u32>) -> u32 {
        let n = self.len;
        cost.clear();
        cost.resize(n + 1, 0);
        let steps = &self.steps[row * n..row * n + n];
        for i in (0..n).rev() {
            let (code, len) = steps[i];
            let mut best = if code == ESCAPE {
                16 + cost[i + 1]
            } else {
                8 + cost[i + usize::from(len)]
            };
            if let Some(prices) = prices {
                for &(k, id) in &self.hits[self.at[i] as usize..self.at[i + 1] as usize] {
                    let id = id as usize;
                    if id > prices.limit {
                        continue;
                    }
                    let at = 8 * prices.bytes_for(id - 1) as u32 + cost[i + usize::from(k)];
                    if at < best {
                        best = at;
                    }
                }
            }
            cost[i] = best;
        }
        cost[0]
    }
}

/// One round's gains over one thread's share of the samples, in bytes saved, into `gain` — one
/// map a merge partition, by [`part_of`], so that the partitions can be merged across threads
/// without a thread's map being walked or copied.
fn round<'a>(
    share: &[(&[&'a [u8]], &Table)],
    trie: &Trie,
    phrases: &[Vec<u8>],
    gain: &mut [Gains<'a>],
) {
    let (mut w, mut alone) = (Scratch::default(), Scratch::default());
    for &(sample, table) in share {
        let enc = table.encoder();
        // What a phrase already in the vocabulary would cost this shard's symbols instead, filled
        // as the parse meets it: the same span recurs in every window it opens, and re-parsing it
        // each time was a third of the miner.
        let mut solo = vec![0u32; phrases.len()];
        for &p in sample {
            parse(p, &enc, trie, Some(Prices::MINING), &mut w);
            tokens(&mut w, p.len());
            for a in 0..w.tokens.len() {
                let start = w.tokens[a].0;
                let (mut bytes, mut cost) = (0usize, 0u64);
                let mut hash = Span::NEW;
                for b in a..w.tokens.len().min(a + WINDOW) {
                    let (_, len, kind, _) = w.tokens[b];
                    if bytes + len > MAX {
                        break;
                    }
                    hash.push(&p[start + bytes..start + bytes + len]);
                    bytes += len;
                    cost += if kind == SYMBOL { 1 } else { 2 };
                    let span = &p[start..start + bytes];
                    let saving = if b == a {
                        // A phrase already in the vocabulary earns what it saves against the
                        // symbols that would code it instead.
                        if kind != PHRASE {
                            continue;
                        }
                        let id = w.tokens[b].3 as usize;
                        if solo[id] == 0 {
                            solo[id] = parse(span, &enc, trie, None, &mut alone) / 8;
                        }
                        u64::from(solo[id]).saturating_sub(2)
                    } else if bytes >= 3 {
                        cost - 2
                    } else {
                        0
                    };
                    if saving > 0 {
                        let key = hash.key(bytes);
                        gain[part_of(key, gain.len())]
                            .entry(key)
                            .or_insert((0, span))
                            .0 += saving;
                    }
                }
            }
            if gain.iter().map(Map::len).sum::<usize>() > GAIN_CAP {
                // Half by gain, not everything under a rising floor: on a corpus where most spans
                // are seen twice a floor of one takes almost all of them, and once it has risen a
                // span first met late can never clear it.
                let mut gains: Vec<u64> = gain
                    .iter()
                    .flat_map(|part| part.values().map(|(g, _)| *g))
                    .collect();
                let half = gains.len() / 2;
                let (_, &mut median, _) = gains.select_nth_unstable(half);
                for part in gain.iter_mut() {
                    part.retain(|_, (g, _)| *g > median);
                    part.shrink_to_fit();
                }
            }
        }
    }
}

/// The merge partition a candidate's key falls in, out of `parts`: the top bits of its second
/// lane, which is a multiply-rotate hash, scaled.
fn part_of(key: u128, parts: usize) -> usize {
    ((((key as u64) >> 32) * parts as u64) >> 32) as usize
}

/// How the candidates rank: by what one saves, then by its bytes, so that a tie falls the same
/// way on every run.
fn rank(x: &(u64, &[u8]), y: &(u64, &[u8])) -> Ordering {
    y.0.cmp(&x.0).then_with(|| x.1.cmp(y.1))
}

/// Keeps the `take` candidates that rank first, in no particular order: the ranking is total, so
/// they are the same `take` a sort would have put first, found in one pass instead of a sort.
fn cut(ranked: &mut Vec<(u64, &[u8])>, take: usize) {
    if ranked.len() > take {
        ranked.select_nth_unstable_by(take, rank);
        ranked.truncate(take);
    }
}

/// One partition's candidates summed over every thread's map of it, cut to the `take` that rank
/// first — which is all a partition can contribute to the round's first `take`.
fn merge(maps: Vec<Gains<'_>>, take: usize) -> Vec<(u64, &[u8])> {
    let mut maps = maps.into_iter();
    let mut gain = maps.next().unwrap_or_default();
    for map in maps {
        for (k, (g, s)) in map {
            gain.entry(k).or_insert((0, s)).0 += g;
        }
    }
    let mut ranked: Vec<(u64, &[u8])> = gain.into_values().collect();
    cut(&mut ranked, take);
    ranked
}

/// Whether any of a spread of shards would buy `phrases` — asked after the first round, because
/// the rounds are the build's cost and a corpus can repeat spans without any of them paying.
///
/// The miner ranks a span by what coding it once would save; a shard answers the question that
/// actually decides the format, which is whether giving byte codes up to phrases beats keeping
/// them for symbols over all of its own bytes. On a million opaque keys the two disagree: 40 000
/// spans clear the miner's bar and no shard takes one.
fn worth(samples: &[(&[&[u8]], &Table)], phrases: &[Vec<u8>]) -> bool {
    let trie = Trie::of(phrases.iter().map(Vec::as_slice));
    let mut w = Scratch::default();
    let step = samples.len().div_ceil(PROBE_SHARDS).max(1);
    samples.iter().step_by(step).any(|&(sample, table)| {
        let take = sample.len().div_ceil(PROBE_PIECES).max(1);
        let probe: Vec<&[u8]> = sample.iter().copied().step_by(take).collect();
        // Chosen on one half of the probe and priced on the other, alternately so that the two
        // halves cover the same span of the keys: `settle` picks the split that fits the pieces it
        // is given, and asking it about those same pieces is what let a word list clear a bar its
        // own shards then refused -- the miner ran and nothing took a phrase.
        let fit: Vec<&[u8]> = probe.iter().copied().step_by(2).collect();
        let held: Vec<&[u8]> = probe.iter().copied().skip(1).step_by(2).collect();
        let (Some(split), cut) = settle(&fit, table, &trie, phrases.len(), &mut w) else {
            return false;
        };
        // By a margin, not by a byte: a split that only just wins on a sample is one the shard
        // itself refuses once its whole stream is on the bill.
        let with = price(&held, &cut.encoder(), &trie, Some(split), &mut w);
        let alone = price(&held, &table.encoder(), &trie, None, &mut w);
        with * 100 < alone * (100 - MARGIN)
    })
}

/// The phrases a parse of `samples` picks, ranked by what they save, with the rest dropped.
///
/// A round ranks a candidate by what it *would* save; this asks the parse. A span that always
/// loses to a longer phrase covering it is never picked, and paying for it in the dictionary costs
/// every key in the blob — so the vocabulary the shards see is the one the parse uses, and the ids
/// that are cheapest to name go to the phrases that earn most.
fn prune(
    samples: &[(&[&[u8]], &Table)],
    mut phrases: Vec<Vec<u8>>,
    threads: usize,
) -> Vec<Vec<u8>> {
    for _ in 0..PRUNE_ROUNDS {
        let before = phrases.len();
        phrases = prune_once(samples, phrases, threads);
        if phrases.len() == before || phrases.is_empty() {
            break;
        }
    }
    phrases
}

fn prune_once(
    samples: &[(&[&[u8]], &Table)],
    phrases: Vec<Vec<u8>>,
    threads: usize,
) -> Vec<Vec<u8>> {
    let trie = Trie::of(phrases.iter().map(Vec::as_slice));
    // The most generous split any shard could buy: what it cannot reach, none of them can.
    let prices = Split::family(phrases.len())
        .into_iter()
        .map(Split::prices)
        .max_by_key(|p| p.limit)
        .unwrap_or(Prices::MINING);
    let share = samples.len().div_ceil(threads.max(1)).max(1);
    let mut saved = std::thread::scope(|scope| {
        let running: Vec<_> = samples
            .chunks(share)
            .map(|chunk| {
                let (trie, phrases) = (&trie, &phrases);
                scope.spawn(move || {
                    let mut saved = vec![0u64; phrases.len()];
                    let (mut w, mut alone) = (Scratch::default(), Scratch::default());
                    for &(sample, table) in chunk {
                        let enc = table.encoder();
                        // What a phrase saves depends on the shard's own symbols, so its cost
                        // under them alone is taken once per shard and only for the ones used.
                        let mut cost = vec![0u32; phrases.len()];
                        for &p in sample {
                            parse(p, &enc, trie, Some(prices), &mut w);
                            tokens(&mut w, p.len());
                            for &(_, _, kind, id) in &w.tokens {
                                if kind != PHRASE {
                                    continue;
                                }
                                let id = id as usize;
                                if cost[id] == 0 {
                                    cost[id] =
                                        parse(&phrases[id], &enc, trie, None, &mut alone) / 8;
                                }
                                let paid = prices.bytes_for(id) as u32;
                                saved[id] += u64::from(cost[id].saturating_sub(paid));
                            }
                        }
                    }
                    saved
                })
            })
            .collect();
        running
            .into_iter()
            .map(|h| h.join().expect("pruning a share cannot panic"))
            .reduce(|mut a, b| {
                a.iter_mut().zip(b).for_each(|(x, y)| *x += y);
                a
            })
            .unwrap_or_default()
    });
    saved.resize(phrases.len(), 0);
    let mut ranked: Vec<(u64, Vec<u8>)> = phrases
        .into_iter()
        .zip(&saved)
        // A phrase the parse never picks is one the dictionary pays for and nothing names. What
        // it would be worth if it were picked is the miner's question, and was asked there: this
        // one is only whether the vocabulary as a whole leaves it any bytes to cover.
        .filter(|(_, g)| **g > 0)
        .map(|(p, g)| (*g, p))
        .collect();
    ranked.sort_unstable_by(|x, y| y.0.cmp(&x.0).then_with(|| x.1.cmp(&y.1)));
    ranked.into_iter().map(|(_, p)| p).collect()
}

/// The phrases a corpus is worth carrying, best first.
///
/// Mined from the shard samples — the suffixes each table was trained on — in rounds of
/// parse-and-count: every window of up to [`WINDOW`] adjacent tokens covering 3 to [`MAX`] bytes
/// earns what coding it as one phrase would save against its tokens, and each round keeps the
/// candidates that earned most so the next one scores a phrase against its competition rather than
/// against the symbols alone.
///
/// `pool` is what the last round keeps, which is the vocabulary before pruning. Summing a thread's
/// gains into another's is the same total as one walk of the samples, and the ranking is over
/// distinct spans, so the threads do not move it.
pub(crate) fn mine(
    samples: &[Vec<&[u8]>],
    tables: &[Table],
    keys: usize,
    pool: usize,
    threads: usize,
) -> Vec<Vec<u8>> {
    // The miner reads a bounded slice of the samples: the vocabulary is the corpus's and converges
    // long before every shard is in, while the map of candidate spans is not bounded by anything
    // else.
    let total: usize = samples.iter().map(Vec::len).sum();
    let step = total.div_ceil(PIECES.max(1)).max(1);
    let taken: Vec<Vec<&[u8]>> = samples
        .iter()
        .map(|s| s.iter().copied().step_by(step).collect())
        .collect();
    let pairs: Vec<(&[&[u8]], &Table)> = taken
        .iter()
        .map(Vec::as_slice)
        .zip(tables.iter())
        .filter(|(s, _)| !s.is_empty())
        .collect();
    if pairs.is_empty() {
        return Vec::new();
    }
    let taken_pieces: usize = taken.iter().map(Vec::len).sum();
    let room = (taken_pieces / PIECES_PER_PHRASE).max(1);
    let (pool, cap) = (pool.min(room), CAP.min(room));
    // A gain counted on the sample is one key in `scale` of the blob's.
    let scale = (keys / taken_pieces.max(1)).max(1) as u64;
    let share = pairs.len().div_ceil(POOLS).max(1);
    let pools = pairs.len().div_ceil(share);
    // As many merge partitions as mining threads: a pool's gains are kept in one map a partition,
    // so the merge is one thread a partition over maps nobody else touches. The partitioning does
    // not reach the answer -- [`rank`] is a total order over distinct spans, so a partition's own
    // first `take` are the union's first `take` that fell in it -- which is why this one may
    // follow the machine where the pools may not.
    let parts = pools.min(threads.max(1)).max(1);
    let per_thread = pools.div_ceil(threads.max(1)).max(1);
    let mut phrases: Vec<Vec<u8>> = Vec::new();
    for r in 0..ROUNDS {
        let trie = Trie::of(phrases.iter().map(Vec::as_slice));
        let maps: Vec<Vec<Gains<'_>>> = std::thread::scope(|scope| {
            let running: Vec<_> = pairs
                .chunks(share * per_thread)
                .map(|group| {
                    let (trie, phrases) = (&trie, &phrases);
                    scope.spawn(move || {
                        group
                            .chunks(share)
                            .map(|chunk| {
                                let mut gain: Vec<Gains<'_>> =
                                    (0..parts).map(|_| Map::default()).collect();
                                round(chunk, trie, phrases, &mut gain);
                                gain
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            running
                .into_iter()
                .flat_map(|h| h.join().expect("mining a pool cannot panic"))
                .collect()
        });
        // Every round but the last carries the cap, because a round that parses under a vocabulary
        // of millions scores whole rare spans as one phrase and ranks length above reuse.
        let take = if r + 1 == ROUNDS { pool } else { cap };
        // The union is never built and never sorted whole: each partition is merged on its own
        // thread and cut to `take`, the cuts are cut again, and only those are ranked. The ranking
        // is total, so the survivors are the union's first `take`. Merging every map into one and
        // sorting the union was most of the miner's wall clock — the sort's tie-break reads the
        // span's bytes, a cache miss a compare, and ties on the gain are the common case.
        let mut by_part: Vec<Vec<Gains<'_>>> =
            (0..parts).map(|_| Vec::with_capacity(maps.len())).collect();
        for thread in maps {
            for (p, part) in thread.into_iter().enumerate() {
                by_part[p].push(part);
            }
        }
        let tops: Vec<Vec<(u64, &[u8])>> = std::thread::scope(|scope| {
            let running: Vec<_> = by_part
                .into_iter()
                .map(|part| scope.spawn(move || merge(part, take)))
                .collect();
            running
                .into_iter()
                .map(|h| h.join().expect("merging a partition cannot panic"))
                .collect()
        });
        let mut ranked = tops.concat();
        cut(&mut ranked, take);
        ranked.sort_unstable_by(rank);
        phrases = ranked
            .iter()
            .take(take)
            // A candidate has to clear its own storage — its bytes, a `u16` end and its share of a
            // group's `u32` base — against what it saves over the whole blob rather than over the
            // sample the gain was counted on. Applied every round, not only the last, so that a
            // corpus with nothing to repeat is answered after one of them instead of five.
            .filter(|(gain, s)| gain * scale > (s.len() as u64 + 3) * STRICT)
            .map(|(_, s)| s.to_vec())
            .collect();
        if phrases.is_empty() || (r == 0 && !worth(&pairs, &phrases)) {
            return Vec::new();
        }
    }
    prune(&pairs, phrases, threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_of(pieces: &[&[u8]]) -> Table {
        Table::train(pieces)
    }

    #[test]
    fn a_split_reads_back_every_id_it_can_name() {
        for split in Split::family(200_000) {
            let limit = split.limit();
            for id in [
                0,
                1,
                GROUP - 1,
                GROUP,
                split.two().saturating_sub(1),
                split.two(),
                limit - 1,
            ] {
                if id >= limit {
                    continue;
                }
                let mut out = Vec::new();
                split.write(id, &mut out);
                assert_eq!(out.len(), split.prices().bytes_for(id), "{split:?} {id}");
                assert!(
                    out[0] >= split.symbols && out[0] != ESCAPE,
                    "{split:?} {id}"
                );
                assert_eq!(
                    split.read_at(&out, 0),
                    Some((id, out.len())),
                    "{split:?} {id}"
                );
            }
            // A symbol code and the raw escape are not phrases whatever follows them.
            assert_eq!(split.read_at(&[0, 0, 0], 0), None, "{split:?}");
            assert_eq!(split.read_at(&[ESCAPE, 0, 0], 0), None, "{split:?}");
        }
    }

    #[test]
    fn a_dictionary_reads_back_every_phrase() {
        let phrases: Vec<Vec<u8>> = (0..1000u32)
            .map(|i| format!("phrase-{i:04}-{}", "x".repeat((i % 17) as usize)).into_bytes())
            .collect();
        let dict = Dict::of(&phrases);
        assert_eq!(dict.len(), phrases.len());
        for (id, p) in phrases.iter().enumerate() {
            assert_eq!(dict.at(id), Some(p.as_slice()), "{id}");
        }
        assert_eq!(dict.at(phrases.len()), None);
        let mut bytes = Vec::new();
        dict.write_to(&mut bytes);
        assert_eq!(bytes.len(), dict.serialized_len());
        let back = Dict::read(&bytes).expect("our own dictionary");
        assert_eq!(back.len(), dict.len());
        for (id, p) in phrases.iter().enumerate() {
            assert_eq!(back.at(id), Some(p.as_slice()), "{id}");
        }
        assert!(Dict::read(&bytes[..bytes.len() - 1]).is_none());
        assert!(Dict::read(&[]).is_none());
        assert_eq!(Dict::read(&0u32.to_le_bytes()).map(|d| d.len()), Some(0));
    }

    #[test]
    fn a_dictionary_this_crate_did_not_write_is_refused() {
        let dict = Dict::of(&[b"alpha".to_vec(), b"beta".to_vec()]);
        let mut bytes = Vec::new();
        dict.write_to(&mut bytes);
        // An end past the bytes, and a phrase of no length at all.
        let mut b = bytes.clone();
        let at = 4 + 4 + 2;
        b[at..at + 2].copy_from_slice(&9999u16.to_le_bytes());
        assert!(Dict::read(&b).is_none());
        let mut b = bytes.clone();
        b[at..at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert!(Dict::read(&b).is_none());
    }

    /// The parse is what the encoder charges for, so the two have to agree to the byte, and what
    /// comes out has to be what went in.
    #[test]
    fn a_parse_costs_what_the_coding_takes_and_reads_back() {
        let pieces: Vec<&[u8]> = vec![
            b"https://example.com/wiki/Alpha",
            b"https://example.com/wiki/Beta",
            b"https://example.org/wiki/Gamma",
            b"https://example.com/index.html",
        ];
        let table = table_of(&pieces);
        let phrases: Vec<Vec<u8>> = vec![
            b"https://example.".to_vec(),
            b"com/wiki/".to_vec(),
            b"index.html".to_vec(),
        ];
        let trie = Trie::of(phrases.iter().map(Vec::as_slice));
        let dict = Dict::of(&phrases);
        let split = Split {
            symbols: 191,
            wide: 0,
        };
        let cut = table.head(split.symbols as usize);
        let enc = cut.encoder();
        let mut w = Scratch::default();
        for p in &pieces {
            let mut out = Vec::new();
            let bits = parse(p, &enc, &trie, Some(split.prices()), &mut w);
            let bytes = encode_into(p, &enc, &trie, Some(split), &mut w, &mut out);
            assert_eq!(bits as usize, 8 * bytes, "{p:?}");
            assert_eq!(out.len(), bytes);
            // Walk it back the way a reader does.
            let mut back = Vec::new();
            let mut i = 0;
            while i < out.len() {
                if let Some((id, took)) = split.read_at(&out, i) {
                    back.extend_from_slice(dict.at(id).expect("a phrase we wrote"));
                    i += took;
                } else if out[i] == ESCAPE {
                    back.push(out[i + 1]);
                    i += 2;
                } else {
                    let (word, len) = cut.symbol(out[i]).expect("a symbol we wrote");
                    back.extend_from_slice(&word.to_le_bytes()[..len]);
                    i += 1;
                }
            }
            assert_eq!(&back, p, "{p:?}");
        }
    }

    #[test]
    fn a_corpus_with_no_phrases_settles_on_its_table_alone() {
        let pieces: Vec<&[u8]> = vec![b"abc", b"abd", b"abe"];
        let table = table_of(&pieces);
        let (split, kept) = settle(&pieces, &table, &Trie::empty(), 0, &mut Scratch::default());
        assert_eq!(split, None);
        assert_eq!(kept.len(), table.len());
    }

    #[test]
    fn the_miner_finds_the_phrase_a_corpus_repeats() {
        let keys: Vec<String> = (0..4000u32)
            .map(|i| format!("https://example.com/wiki/article/{i:06}"))
            .collect();
        let pieces: Vec<&[u8]> = keys.iter().map(|k| k.as_bytes()).collect();
        let table = table_of(&pieces);
        let phrases = mine(
            std::slice::from_ref(&pieces),
            std::slice::from_ref(&table),
            pieces.len(),
            4096,
            2,
        );
        assert!(!phrases.is_empty(), "the miner found nothing to repeat");
        assert!(
            phrases.iter().any(|p| p.windows(5).any(|w| w == b"wiki/")),
            "{:?}",
            &phrases[..phrases.len().min(8)]
        );
        let trie = Trie::of(phrases.iter().map(Vec::as_slice));
        let mut w = Scratch::default();
        let (split, kept) = settle(&pieces, &table, &trie, phrases.len(), &mut w);
        let split = split.expect("a corpus of one URL shape buys phrases");
        let alone = price(&pieces, &table.encoder(), &trie, None, &mut w);
        let with = price(&pieces, &kept.encoder(), &trie, Some(split), &mut w);
        assert!(with < alone, "{with} against {alone}");
    }
}
