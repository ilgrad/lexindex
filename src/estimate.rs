//! What each index would cost on a set of keys, and which of them answer the caller's questions.
//!
//! Five indexes with five size curves is a choice nobody should have to make from a README table:
//! the sizes are corpus-specific, and the spread between them on one corpus is larger than the
//! spread of any one of them across corpora. [`plan`] sorts the keys once, measures the statistics
//! the formats are actually paid in, and prices every index the caller's [`Needs`] allow.
//!
//! **What is measured and what is modelled.** The sort makes `n`, the mean key length and the LCP
//! structure exact; everything a format does with them is arithmetic. The one input no statistic
//! gives is what the symbol table squeezes a suffix into, and that can only be read off a build —
//! so one build of a 100 000-key sample supplies it, along with the bytes an fst spends per trie
//! node and the bits the perfect hash spends per key. Scored against the built blob on 23 corpora
//! of half a million to ten million keys, at each of the three priced blocks, the [`DictIndex`]
//! estimate lands within **1.4 % median, 4.5 % at the 90th percentile and 5.4 % at worst**.
//! [`StringIndex`] is looser — 3.4 % median, 9.8 % at the 90th percentile, and 30 % on a corpus of
//! file paths — because an fst merges equal
//! suffixes, and how much it merges is a property of the whole key set rather than of a sample of
//! it. The one family past both is corpora whose mean suffix is about a byte, where the ratio read
//! from a sample does not carry to full density, and [`Plan`] says so rather than quoting a number
//! it does not have.
//!
//! Below the sample size there is nothing to model: the plan builds all the candidates and reports
//! what they weigh.

use crate::{DictIndex, IndexError, StringIndex, dict_index};
use std::fmt;

#[cfg(feature = "mph")]
use crate::{ClosedHashIndex, CompactHashIndex, PerfectHashIndex};

/// Keys a sample holds. Below this the plan builds the real indexes instead of modelling them.
const SAMPLE: usize = 100_000;

/// The [`DictIndex`] blocks the plan prices. The block is a knob, not a constant — on an English
/// word list the three named points of its curve span 2.79 to 3.23 bytes a key — so a ranking that
/// offered one of them would be answering a question the caller did not ask. Ascending, so that
/// candidates tying on bytes keep block order in the ladder.
const DICT_BLOCKS: [usize; 3] = [
    crate::DictProfile::Fast.block(),
    crate::DictProfile::Balanced.block(),
    crate::DictProfile::Compact.block(),
];

#[cfg(test)]
thread_local! {
    /// Sample size a test asks for, so the modelled path can be exercised without building a
    /// corpus larger than the real sample -- which is what it takes to reach it otherwise.
    static SAMPLE_OVERRIDE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Keys the sample holds for this call.
fn sample_size() -> usize {
    #[cfg(test)]
    {
        let over = SAMPLE_OVERRIDE.with(std::cell::Cell::get);
        if over != 0 {
            return over;
        }
    }
    SAMPLE
}

/// How close two candidates have to be before the plan stops trusting its own ranking and says to
/// build both. The model's error is inside 5 % everywhere it was validated but one corpus family;
/// 1.3× is that with room.
const CLOSE: f64 = 1.3;

/// A mean suffix under this many bytes is where a symbol-table ratio read from a sample stops
/// carrying to full density — measured at −19 % on ten million numbers, whose mean suffix is one
/// byte.
const THIN_SUFFIX: f64 = 2.0;

/// What the caller has to be able to do with the index. Nothing is required by default, so a
/// `Needs::default()` asks only for `id(key)`.
///
/// ```
/// # use lexindex::Needs;
/// let needs = Needs::default().reverse().prefix();
/// assert!(needs.reverse && needs.prefix && !needs.fuzzy);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Needs {
    /// `key(id)` as well as `id(key)`.
    pub reverse: bool,
    /// Ids in lexicographic order, and in-order iteration.
    pub ordered: bool,
    /// Prefix and range queries.
    pub prefix: bool,
    /// Fuzzy (Levenshtein) or subsequence queries.
    pub fuzzy: bool,
    /// A non-member must be answered as one. Without it a probabilistic index is allowed, which is
    /// how the two smallest ones get their size.
    pub exact: bool,
}

impl Needs {
    /// Require [`reverse`](Self::reverse).
    pub fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    /// Require [`ordered`](Self::ordered).
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }

    /// Require [`prefix`](Self::prefix), which implies an ordered index.
    pub fn prefix(mut self) -> Self {
        self.prefix = true;
        self
    }

    /// Require [`fuzzy`](Self::fuzzy).
    pub fn fuzzy(mut self) -> Self {
        self.fuzzy = true;
        self
    }

    /// Require [`exact`](Self::exact) membership.
    pub fn exact(mut self) -> Self {
        self.exact = true;
        self
    }
}

/// One of the crate's indexes, as an answer rather than a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// [`CompactHashIndex`](crate::CompactHashIndex): smallest, `string → id` only, probabilistic.
    Compact,
    /// [`ClosedHashIndex`](crate::ClosedHashIndex): the perfect hash and nothing else.
    Closed,
    /// [`PerfectHashIndex`](crate::PerfectHashIndex): verified membership and reverse, unordered.
    Perfect,
    /// [`StringIndex`]: an fst — ordered, prefix, range, fuzzy, subsequence.
    String,
    /// [`DictIndex`]: an ordered dictionary, front-coded; no automata.
    Dict,
}

impl Kind {
    /// Whether an index of this kind answers everything `needs` asks for — what [`plan`] filters
    /// the five candidates by, public so a caller that presents the ranking can say *why* a kind
    /// is missing from it without restating the table.
    ///
    /// ```
    /// use lexindex::{Kind, Needs};
    /// // The two hash indexes answer `id(key)` and nothing else.
    /// assert!(Kind::Closed.answers(Needs::default()));
    /// assert!(!Kind::Closed.answers(Needs::default().exact()));
    /// assert!(Kind::Dict.answers(Needs::default().prefix().exact()));
    /// assert!(!Kind::Dict.answers(Needs::default().fuzzy()));
    /// ```
    pub fn answers(self, needs: Needs) -> bool {
        let (reverse, ordered, fuzzy, exact) = match self {
            Self::Compact => (false, false, false, false),
            Self::Closed => (false, false, false, false),
            Self::Perfect => (true, false, false, true),
            Self::String => (true, true, true, true),
            Self::Dict => (true, true, false, true),
        };
        (!needs.reverse || reverse)
            && (!(needs.ordered || needs.prefix) || ordered)
            && (!needs.fuzzy || fuzzy)
            && (!needs.exact || exact)
    }

    /// Whether this build has the index at all: three of the five are behind the `mph` feature.
    const fn available(self) -> bool {
        matches!(self, Self::String | Self::Dict) || cfg!(feature = "mph")
    }

    /// The type's name, for an explanation a reader can act on.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Compact => "CompactHashIndex",
            Self::Closed => "ClosedHashIndex",
            Self::Perfect => "PerfectHashIndex",
            Self::String => "StringIndex",
            Self::Dict => "DictIndex",
        }
    }
}

/// What [`plan_for`] ranks its candidates by.
///
/// [`Memory`](Self::Memory) is what [`plan`] has always done and is the default, so a caller who
/// has not thought about it is not moved by this existing.
///
/// ```
/// # use lexindex::{plan_for, Needs, Objective, Kind};
/// let keys = ["apple", "apricot", "banana", "blueberry"];
/// let small = plan_for(&keys, Needs::default().exact(), Objective::Memory)?;
/// let quick = plan_for(&keys, Needs::default().exact(), Objective::Latency)?;
/// assert!(small.best().bytes <= quick.best().bytes);
/// assert!(quick.nanos(&quick.best()) <= small.nanos(&small.best()));
/// # Ok::<(), lexindex::IndexError>(())
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Objective {
    /// The smallest blob, which is what [`plan`] ranks by.
    #[default]
    Memory,
    /// The fastest `id(key)`, from [the model](Plan::nanos) — an ordering, never a prediction of
    /// your machine's nanoseconds.
    Latency,
    /// Whichever candidate is nearest to both: the one whose worse ratio — its bytes against the
    /// smallest, its modelled latency against the fastest — is smallest. On a corpus where one
    /// index is both, that index; where they differ, the one that gives up least on either.
    Balanced,
}

/// The three constants of one structure's `id(key)` latency: `a + b·log2(n / 100 000) + c·len`,
/// nanoseconds, where `len` is the mean key length in bytes.
#[derive(Clone, Copy)]
struct Cost {
    a: f64,
    b: f64,
    c: f64,
}

impl Cost {
    /// Below [`SAMPLE`] the fit has no evidence, so this reports the 100 000-key figure rather
    /// than extrapolating a curve past where it was measured — a smaller corpus is not slower,
    /// and what the number is for is the ordering.
    fn nanos(self, keys: usize, mean_len: f64) -> f64 {
        let n = (keys.max(SAMPLE) as f64 / SAMPLE as f64).log2();
        self.a + self.b * n + self.c * mean_len
    }

    /// Between two measured blocks, linear in `log2(block)`, which is what the three constants
    /// move in over 32..=1024.
    fn between(self, other: Self, t: f64) -> Self {
        let mix = |x: f64, y: f64| x + (y - x) * t;
        Self {
            a: mix(self.a, other.a),
            b: mix(self.b, other.b),
            c: mix(self.c, other.c),
        }
    }
}

/// Least squares over 240 measured cells: eight structures over thirteen corpora at 100 000,
/// 1 000 000 and 10 000 000 keys, in one process, the lanes alternating so that no structure is
/// timed with the caches still warm from its own build. `bench/latency_model.py measure` produced
/// them and `fit` re-derives this table from
/// `bench/results/latency-model-2026-09-13-arz-6786f74-dirty.json` — the tree was dirty with the
/// planner change these constants are for, which cannot move an `id`. Mean absolute error is
/// 9–21 % of the measurement.
///
/// **Far too coarse to quote and quite enough to rank.** Scored against the *published* sweep,
/// which these were not fitted to: the fastest of its six lexindex lanes is named in 30 of 30
/// corpus-size cells — though `ClosedHashIndex` wins most of those outright, so the number that
/// means something is the comparison a caller actually faces. `DictIndex` against `StringIndex`,
/// which decides an ordered index, is right in **27 of 30** and thirteen of thirteen at 100 000
/// keys; which `DictIndex` block is fastest, 26 of 30.
///
/// **These are one machine's cache latencies**, an AMD Ryzen 7 5800HS with 16 MB of L3, timed
/// through the Python binding so that every one of them carries that call. Nothing here rescales
/// them for another machine, and no number they produce is a measurement of the caller's corpus.
const COST: [(Kind, Cost); 4] = [
    (
        Kind::Closed,
        Cost {
            a: 47.0,
            b: 14.10,
            c: 0.37,
        },
    ),
    (
        Kind::Compact,
        Cost {
            a: 61.7,
            b: 23.99,
            c: 0.30,
        },
    ),
    (
        Kind::Perfect,
        Cost {
            a: 101.5,
            b: 41.29,
            c: 1.11,
        },
    ),
    (
        Kind::String,
        Cost {
            a: 140.4,
            b: 60.47,
            c: 9.10,
        },
    ),
];

/// [`Kind::Dict`] is a curve, not a point: the block trades a scan against a search and both ends
/// of 32..=1024 were measured. Ascending by block, interpolated in between and clamped outside.
const DICT_COST: [(usize, Cost); 4] = [
    (
        32,
        Cost {
            a: 180.7,
            b: 68.17,
            c: 3.98,
        },
    ),
    (
        128,
        Cost {
            a: 214.3,
            b: 64.29,
            c: 4.01,
        },
    ),
    (
        256,
        Cost {
            a: 230.3,
            b: 56.78,
            c: 3.95,
        },
    ),
    (
        1024,
        Cost {
            a: 278.2,
            b: 47.15,
            c: 4.10,
        },
    ),
];

/// The cost constants for one candidate.
fn cost_of(kind: Kind, block: Option<usize>) -> Cost {
    if kind != Kind::Dict {
        let (_, cost) = COST
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("every kind but Dict is in the table");
        return *cost;
    }
    let block = block.unwrap_or(dict_index::DEFAULT_BLOCK);
    let at = DICT_COST.partition_point(|(b, _)| *b < block);
    if at == 0 {
        return DICT_COST[0].1;
    }
    if at == DICT_COST.len() {
        return DICT_COST[DICT_COST.len() - 1].1;
    }
    let (lo, low) = DICT_COST[at - 1];
    let (hi, high) = DICT_COST[at];
    let t =
        ((block as f64).log2() - (lo as f64).log2()) / ((hi as f64).log2() - (lo as f64).log2());
    low.between(high, t)
}

/// What one index is expected to weigh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// The index this is about.
    pub kind: Kind,
    /// Bytes its serialised blob is expected to take.
    pub bytes: u64,
    /// Keys a block holds, for [`Kind::Dict`], and `None` for the rest.
    pub block: Option<usize>,
    /// Whether `bytes` was weighed rather than modelled — true for every candidate when the corpus
    /// is no larger than the sample.
    pub measured: bool,
}

impl Estimate {
    /// Bytes a key, the number this crate's tables are quoted in.
    pub fn bytes_per_key(&self, keys: usize) -> f64 {
        if keys == 0 {
            0.0
        } else {
            self.bytes as f64 / keys as f64
        }
    }
}

/// What [`plan`] found: every candidate priced, cheapest first, and what it is unsure about.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    keys: usize,
    mean_len: f64,
    mean_lcp: f64,
    estimates: Vec<Estimate>,
    needs: Needs,
    objective: Objective,
}

impl Plan {
    /// The candidate the [`Objective`] puts first among those that answer the caller's questions.
    /// There is always one: [`StringIndex`] answers every question this crate can be asked.
    pub fn best(&self) -> Estimate {
        self.estimates[0]
    }

    /// Every candidate the [`Needs`] allow, in the [`Objective`]'s order, best first.
    pub fn estimates(&self) -> &[Estimate] {
        &self.estimates
    }

    /// What this plan ranked by.
    pub fn objective(&self) -> Objective {
        self.objective
    }

    /// The modelled `id(key)` latency of a candidate, in nanoseconds.
    ///
    /// **An estimate, and of one machine.** `a + b·log2(n / 100 000) + c·mean_len` per structure,
    /// least-squares fitted to 240 cells measured on this crate's own hardware — an AMD Ryzen 7
    /// 5800HS with 16 MB of L3 — with mean absolute error 9–21 % of the measurement. It is
    /// accurate enough to order the candidates and nowhere near accurate enough to quote: on the
    /// published sweep, which it was not fitted to, it picks `DictIndex` over `StringIndex` the
    /// way the measurement does in 27 of 30 corpus-size cells and 13 of 13 at 100 000 keys — and
    /// it will not tell you what your own lookup costs.
    ///
    /// ```
    /// # use lexindex::{plan, Needs};
    /// let p = plan(&["apple", "apricot", "banana"], Needs::default())?;
    /// // Cheaper to store is not cheaper to ask.
    /// assert!(p.estimates().iter().all(|e| p.nanos(e) > 0.0));
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn nanos(&self, estimate: &Estimate) -> f64 {
        cost_of(estimate.kind, estimate.block).nanos(self.keys, self.mean_len)
    }

    /// Distinct keys the plan was made for.
    pub fn keys(&self) -> usize {
        self.keys
    }

    /// The mean key length and the mean LCP between adjacent keys in order — the two statistics
    /// every front-coded size follows from.
    pub fn shape(&self) -> (f64, f64) {
        (self.mean_len, self.mean_lcp)
    }

    /// Whether the two cheapest candidates *of different kinds* are close enough that the ranking
    /// should not be trusted over a real build.
    ///
    /// Different kinds, because two `DictIndex` blocks are always within a few per cent of each
    /// other and warning about that would be warning about every plan: they come off the same
    /// model, the ladder names both, and picking between them is a speed decision rather than a
    /// size one. By bytes whatever the [`Objective`] is, since what this doubts is the size
    /// estimate.
    pub fn close(&self) -> bool {
        let Some(first) = self.estimates.iter().min_by_key(|e| e.bytes) else {
            return false;
        };
        self.estimates
            .iter()
            .filter(|e| e.kind != first.kind)
            .map(|e| e.bytes)
            .min()
            .is_some_and(|bytes| bytes as f64 <= first.bytes as f64 * CLOSE)
    }

    /// Whether the corpus is one whose suffixes are too short for a sampled symbol-table ratio to
    /// carry — where the sizes are quoted with much less confidence than usual.
    pub fn thin(&self) -> bool {
        !self.estimates[0].measured && self.mean_len - self.mean_lcp < THIN_SUFFIX
    }
}

/// The explanation: what each candidate would weigh, which was chosen, and what the plan is not
/// sure about. `Display` rather than an `explain()` so it composes with `{}` and `to_string()`.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let best = self.best();
        writeln!(
            f,
            "{} keys, mean length {:.1}, mean shared prefix {:.1}, ranked by {}",
            self.keys,
            self.mean_len,
            self.mean_lcp,
            match self.objective {
                Objective::Memory => "size",
                Objective::Latency => "latency",
                Objective::Balanced => "balance",
            }
        )?;
        for e in &self.estimates {
            // By block as well as kind: three of these rows are dictionaries.
            let mark = if (e.kind, e.block) == (best.kind, best.block) {
                '*'
            } else {
                ' '
            };
            let block = match e.block {
                Some(b) => format!(" at block {b}"),
                None => String::new(),
            };
            writeln!(
                f,
                "{mark} {:<17} {:>12} bytes  {:5.2} B/key  {:>5.0} ns  {}{}",
                e.kind.name(),
                e.bytes,
                e.bytes_per_key(self.keys),
                self.nanos(e),
                if e.measured { "built" } else { "estimated" },
                block
            )?;
        }
        writeln!(
            f,
            "the nanoseconds are a model of this crate's own machine, not a measurement of yours"
        )?;
        if self.close() {
            writeln!(
                f,
                "the two cheapest are within {CLOSE:.1}x, which is inside what an estimate can \
                 separate: build both"
            )?;
        }
        if self.thin() {
            writeln!(
                f,
                "the mean suffix is {:.1} bytes, too short for a sampled compression ratio to \
                 carry: build rather than quote",
                self.mean_len - self.mean_lcp
            )?;
        }
        Ok(())
    }
}

/// What one walk over the sorted keys tells the models. All of it exact.
struct Shape {
    n: usize,
    /// Bytes in all the keys.
    len: u64,
    /// Bytes shared with the previous key, summed — what front coding does not store.
    lcp1: u64,
    /// The same for the keys 32 apart, which is what a restart entry is coded against.
    lcp32: u64,
    /// Pairs the `lcp32` sum is over.
    pairs32: u64,
}

impl Shape {
    fn of(keys: &[&str]) -> Self {
        let mut s = Self {
            n: keys.len(),
            len: 0,
            lcp1: 0,
            lcp32: 0,
            pairs32: 0,
        };
        for (i, k) in keys.iter().enumerate() {
            s.len += k.len() as u64;
            if i > 0 {
                s.lcp1 += dict_index::lcp(keys[i - 1].as_bytes(), k.as_bytes()) as u64;
            }
            if i >= 32 {
                s.lcp32 += dict_index::lcp(keys[i - 32].as_bytes(), k.as_bytes()) as u64;
                s.pairs32 += 1;
            }
        }
        s
    }

    fn mean_len(&self) -> f64 {
        self.len as f64 / self.n.max(1) as f64
    }

    fn mean_lcp1(&self) -> f64 {
        self.lcp1 as f64 / (self.n.max(2) - 1) as f64
    }

    fn mean_lcp32(&self) -> f64 {
        if self.pairs32 == 0 {
            self.mean_lcp1()
        } else {
            self.lcp32 as f64 / self.pairs32 as f64
        }
    }

    /// Characters the keys add over their predecessors: the nodes of the trie an fst minimises.
    fn trie_nodes(&self) -> f64 {
        (self.len - self.lcp1) as f64
    }
}

/// What one build of the sample at one [`DICT_BLOCKS`] entry measures. All three constants move
/// with the block -- a block of 32 restarts eight times as often as one of 256 and carries eight
/// times the per-block arrays -- so each priced block gets its own build rather than the default
/// block's numbers stretched over it.
struct DictFit {
    block: usize,
    /// Compressed suffix bytes over raw suffix bytes, from a `DictIndex` of the sample.
    ratio: f64,
    /// The packed per-block arrays, per block.
    per_block: f64,
    /// One symbol table's serialised bytes.
    table: f64,
}

/// What one build of the sample measures that no statistic gives.
struct Sample {
    /// One fit per priced block.
    dict: [DictFit; DICT_BLOCKS.len()],
    /// What an fst spends on a trie node at the sample's density.
    ///
    /// This is the coarsest number in the plan and it does not get better by fitting. How far an
    /// fst minimises below its trie depends on how much the keys share at *full* density, which a
    /// sample cannot see: carried straight across, it reads 30 % high on `paths` and 10 % low on
    /// `opaque` — and fitting the trend between two sample sizes and extrapolating made both worse
    /// (`paths` +55 %), because the local slope between 50 k and 100 k keys does not point where
    /// the curve goes over the next four e-folds.
    fst_node: f64,
    /// `(fixed, per key)` for each hash index, fitted over two sample sizes: their blobs are a
    /// header, a perfect hash and a per-key table, and at a hundred thousand keys the header is
    /// still 0.06 bytes a key -- scaling one measurement by `n` carries that constant with it and
    /// reads 34 % high.
    #[cfg(feature = "mph")]
    hash_fit: [(f64, f64); 3],
}

impl Sample {
    fn of(keys: &[&str]) -> Result<Self, IndexError> {
        let shape = Shape::of(keys);
        let fst_node =
            StringIndex::build(keys)?.serialized_len() as f64 / shape.trie_nodes().max(1.0);
        let mut dict = Vec::with_capacity(DICT_BLOCKS.len());
        for block in DICT_BLOCKS {
            dict.push(DictFit::of(keys, &shape, block)?);
        }
        Ok(Self {
            dict: dict
                .try_into()
                .unwrap_or_else(|_| unreachable!("one fit a priced block")),
            fst_node,
            #[cfg(feature = "mph")]
            hash_fit: hash_fits(keys)?,
        })
    }

    /// The fit for a block, or the nearest priced one. Nothing in the crate asks for a block
    /// outside [`DICT_BLOCKS`]; the constants move smoothly with it, so the nearest is the best
    /// answer available rather than an error.
    fn dict_fit(&self, block: usize) -> &DictFit {
        self.dict
            .iter()
            .min_by_key(|f| f.block.abs_diff(block))
            .expect("DICT_BLOCKS is never empty")
    }
}

impl DictFit {
    fn of(keys: &[&str], shape: &Shape, block: usize) -> Result<Self, IndexError> {
        let dict = DictIndex::build_with_block(keys, block)?;
        let [tables, _heads, arrays, data] = dict.section_lens();
        let blocks = keys.len().div_ceil(dict.block()) as f64;
        let entries = (keys.len() - blocks as usize) as f64;
        let restarts = blocks * (dict.block().div_ceil(dict.micro()) - 1) as f64;
        let raw = suffix_bytes(shape, entries, restarts);
        Ok(Self {
            block,
            ratio: if raw > 0.0 {
                (data as f64 - entries) / raw
            } else {
                1.0
            },
            per_block: arrays as f64 / blocks,
            table: tables as f64 / shards_for(keys.len(), dict.block()) as f64,
        })
    }
}

/// A fitted `(fixed, per key)` line at `n`.
#[cfg(feature = "mph")]
fn line(fit: (f64, f64), n: usize) -> f64 {
    fit.0 + fit.1 * n as f64
}

/// Symbol tables an index of `n` keys holds: one a shard, and never none.
fn shards_for(n: usize, block: usize) -> usize {
    n.div_ceil(block)
        .div_ceil(dict_index::shard_blocks_for(block))
        .max(1)
}

/// Raw suffix bytes a `DictIndex` of this shape front-codes away: every entry but a block's head
/// stores what it does not share with the key before it, and a restart shares with the key 32
/// back rather than the one before.
fn suffix_bytes(shape: &Shape, entries: f64, restarts: f64) -> f64 {
    let l = shape.mean_len();
    (entries - restarts) * (l - shape.mean_lcp1()) + restarts * (l - shape.mean_lcp32())
}

/// `(fixed, per key)` for the three hash indexes, from a build at the sample size and another at
/// half of it. Their size is a straight line in `n` -- a perfect hash and a per-key table over a
/// header -- so two points fit it exactly, and neither depends on what the keys say beyond their
/// length. The key arena is taken out of `PerfectHashIndex` here and put back at `n`, where the
/// corpus's own byte count is known exactly.
#[cfg(feature = "mph")]
fn hash_fits(keys: &[&str]) -> Result<[(f64, f64); 3], IndexError> {
    let bytes = |ks: &[&str]| -> Result<[f64; 3], IndexError> {
        let arena: f64 = ks.iter().map(|k| k.len() as f64).sum();
        Ok([
            CompactHashIndex::build(ks, 1)?.serialized_len()? as f64,
            ClosedHashIndex::build(ks)?.serialized_len() as f64,
            PerfectHashIndex::build(ks)?.serialized_len()? as f64 - arena,
        ])
    };
    let half = keys.len() / 2;
    let (small, big) = (bytes(&keys[..half])?, bytes(keys)?);
    let mut fit = [(0.0, 0.0); 3];
    for (i, f) in fit.iter_mut().enumerate() {
        let slope = (big[i] - small[i]) / (keys.len() - half).max(1) as f64;
        *f = (big[i] - slope * keys.len() as f64, slope);
    }
    Ok(fit)
}

/// Price every index the needs allow, from one sorted copy of the keys.
///
/// The keys are sorted and deduplicated internally, so they may arrive in any order and the
/// estimate is for the distinct ones. An empty set is a plan with every candidate at its empty
/// size, not an error.
///
/// ```
/// # use lexindex::{plan, Needs};
/// let keys = ["apple", "apricot", "banana"];
/// let p = plan(&keys, Needs::default().reverse().prefix()).unwrap();
/// assert_eq!(p.keys(), 3);
/// println!("{p}");   // the explanation
/// ```
pub fn plan<S: AsRef<str>>(keys: &[S], needs: Needs) -> Result<Plan, IndexError> {
    plan_for(keys, needs, Objective::Memory)
}

/// [`plan`], ranked by something other than the blob size.
///
/// The candidates and their sizes are the same; only the order, and so [`Plan::best`], differ.
/// [`Objective::Memory`] is exactly [`plan`].
///
/// ```
/// # use lexindex::{plan_for, Needs, Objective};
/// let keys = ["apple", "apricot", "banana"];
/// let p = plan_for(&keys, Needs::default().reverse(), Objective::Latency)?;
/// println!("{p}");   // the ladder, fastest first, with the modelled nanoseconds
/// # Ok::<(), lexindex::IndexError>(())
/// ```
pub fn plan_for<S: AsRef<str>>(
    keys: &[S],
    needs: Needs,
    objective: Objective,
) -> Result<Plan, IndexError> {
    let mut sorted: Vec<&str> = keys.iter().map(AsRef::as_ref).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let shape = Shape::of(&sorted);
    let candidates = candidates(needs);
    let mut estimates = if sorted.len() <= sample_size() {
        weigh(&sorted, &candidates)?
    } else {
        let drawn = sample(&sorted, sample_size());
        model(&shape, &Sample::of(&drawn)?, &candidates)
    };
    rank(&mut estimates, objective, sorted.len(), shape.mean_len());
    Ok(Plan {
        keys: sorted.len(),
        mean_len: shape.mean_len(),
        mean_lcp: shape.mean_lcp1(),
        estimates,
        needs,
        objective,
    })
}

/// Put the candidates in the objective's order, best first. Ties, and every order, break on bytes,
/// so two runs over the same keys rank them the same way.
fn rank(estimates: &mut [Estimate], objective: Objective, keys: usize, mean_len: f64) {
    let nanos = |e: &Estimate| cost_of(e.kind, e.block).nanos(keys, mean_len);
    match objective {
        Objective::Memory => estimates.sort_by_key(|e| e.bytes),
        Objective::Latency => {
            estimates.sort_by(|x, y| nanos(x).total_cmp(&nanos(y)).then(x.bytes.cmp(&y.bytes)));
        }
        Objective::Balanced => {
            let smallest = estimates.iter().map(|e| e.bytes).min().unwrap_or(0).max(1) as f64;
            let fastest = estimates
                .iter()
                .map(nanos)
                .fold(f64::INFINITY, f64::min)
                .max(1.0);
            // What it gives up on the objective it does worse on. The winner is the candidate
            // whose worse ratio is smallest, which is the one nearest to both at once.
            let worse = |e: &Estimate| (e.bytes as f64 / smallest).max(nanos(e) / fastest);
            estimates.sort_by(|x, y| worse(x).total_cmp(&worse(y)).then(x.bytes.cmp(&y.bytes)));
        }
    }
}

/// Every index the needs allow, as the ladder's rows: one per kind, except [`Kind::Dict`], which
/// is one per [`DICT_BLOCKS`] entry because its block is a choice and not a default.
fn candidates(needs: Needs) -> Vec<(Kind, Option<usize>)> {
    let mut out = Vec::with_capacity(4 + DICT_BLOCKS.len());
    for kind in [
        Kind::Compact,
        Kind::Closed,
        Kind::Perfect,
        Kind::String,
        Kind::Dict,
    ] {
        if !kind.answers(needs) || !kind.available() {
            continue;
        }
        if kind == Kind::Dict {
            out.extend(DICT_BLOCKS.map(|block| (kind, Some(block))));
        } else {
            out.push((kind, None));
        }
    }
    out
}

/// `want` keys spread over the sorted corpus, in order, and exactly `want` of them.
///
/// A plain `step_by(n / want)` is a stride rather than a sample, and integer division makes it a
/// poor one: a corpus of 150 001 keys gets a step of one, so "the sample" is the whole corpus and
/// the constants are read off a build the size of the real index. The fractional stride
/// `⌊(i·n + phase) / want⌋` takes `want` keys at any size, and the phase — derived from the corpus
/// itself, so that a plan over the same keys is still reproducible — stops the draw from landing
/// on the same offset of every prefix group.
fn sample<'a>(sorted: &[&'a str], want: usize) -> Vec<&'a str> {
    let n = sorted.len();
    if n <= want || want == 0 {
        return sorted.to_vec();
    }
    let ends = crate::blob::hash_bytes(sorted[0].as_bytes())
        ^ crate::blob::hash_bytes(sorted[n - 1].as_bytes());
    let phase = (ends ^ n as u64) % n as u64;
    (0..want as u64)
        .map(|i| sorted[((i * n as u64 + phase) / want as u64) as usize])
        .collect()
}

/// Build every candidate and report what it weighs. What a corpus no larger than the sample gets,
/// since the sample would be the corpus.
fn weigh(keys: &[&str], candidates: &[(Kind, Option<usize>)]) -> Result<Vec<Estimate>, IndexError> {
    let mut out = Vec::with_capacity(candidates.len());
    for &(kind, at) in candidates {
        let (bytes, block) = match kind {
            Kind::String => (StringIndex::build(keys)?.serialized_len() as u64, None),
            Kind::Dict => {
                let block = at.unwrap_or(dict_index::DEFAULT_BLOCK);
                (
                    DictIndex::build_with_block(keys, block)?.serialized_len() as u64,
                    Some(block),
                )
            }
            #[cfg(feature = "mph")]
            Kind::Compact => (
                CompactHashIndex::build(keys, 1)?.serialized_len()? as u64,
                None,
            ),
            #[cfg(feature = "mph")]
            Kind::Closed => (ClosedHashIndex::build(keys)?.serialized_len() as u64, None),
            #[cfg(feature = "mph")]
            Kind::Perfect => (
                PerfectHashIndex::build(keys)?.serialized_len()? as u64,
                None,
            ),
            #[cfg(not(feature = "mph"))]
            _ => unreachable!("the hash indexes are not candidates without their feature"),
        };
        out.push(Estimate {
            kind,
            bytes,
            block,
            measured: true,
        });
    }
    Ok(out)
}

/// Price every candidate at `n` from the exact shape and the sample's constants.
fn model(shape: &Shape, sample: &Sample, candidates: &[(Kind, Option<usize>)]) -> Vec<Estimate> {
    candidates
        .iter()
        .map(|&(kind, at)| {
            let (bytes, block) = match kind {
                Kind::String => (sample.fst_node * shape.trie_nodes(), None),
                Kind::Dict => {
                    let block = at.unwrap_or(dict_index::DEFAULT_BLOCK);
                    (
                        dict_bytes(shape, sample.dict_fit(block), block),
                        Some(block),
                    )
                }
                #[cfg(feature = "mph")]
                Kind::Compact => (line(sample.hash_fit[0], shape.n), None),
                #[cfg(feature = "mph")]
                Kind::Closed => (line(sample.hash_fit[1], shape.n), None),
                #[cfg(feature = "mph")]
                Kind::Perfect => (line(sample.hash_fit[2], shape.n) + shape.len as f64, None),
                #[cfg(not(feature = "mph"))]
                _ => unreachable!("the hash indexes are not candidates without their feature"),
            };
            Estimate {
                kind,
                bytes: bytes.max(0.0) as u64,
                block,
                measured: false,
            }
        })
        .collect()
}

/// The format as the model: a header, the symbol tables, one head a block stored whole, one byte
/// an entry, the suffixes the table squeezed, and the packed arrays.
fn dict_bytes(shape: &Shape, fit: &DictFit, block: usize) -> f64 {
    let n = shape.n as f64;
    let blocks = shape.n.div_ceil(block) as f64;
    let entries = n - blocks;
    let restarts = blocks * (block.div_ceil(dict_index::micro_for(block)) - 1) as f64;
    let data = entries + fit.ratio * suffix_bytes(shape, entries, restarts);
    let tables = shards_for(shape.n, block) as f64 * fit.table;
    dict_index::HEADER as f64 + tables + blocks * shape.mean_len() + data + fit.per_block * blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys with a shape worth modelling: a handful of shared prefixes and tails that share almost
    /// nothing, which is what a corpus of names or ids looks like. Tails that repeat would instead
    /// measure how far an fst can minimise, which is not what the model claims to predict.
    fn corpus(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                h ^= h >> 29;
                h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                format!(
                    "{}-{:012x}",
                    ["ab", "abc", "b", "cde", "d", "ef", "g"][i % 7],
                    h >> 16
                )
            })
            .collect()
    }

    fn with_sample<T>(size: usize, f: impl FnOnce() -> T) -> T {
        SAMPLE_OVERRIDE.with(|c| c.set(size));
        let out = f();
        SAMPLE_OVERRIDE.with(|c| c.set(0));
        out
    }

    #[test]
    fn every_candidate_answers_what_was_asked() {
        let keys = corpus(2_000);
        for bits in 0..32u8 {
            let needs = Needs {
                reverse: bits & 1 != 0,
                ordered: bits & 2 != 0,
                prefix: bits & 4 != 0,
                fuzzy: bits & 8 != 0,
                exact: bits & 16 != 0,
            };
            let plan = plan(&keys, needs).unwrap();
            assert!(!plan.estimates().is_empty(), "{needs:?}");
            for e in plan.estimates() {
                assert!(
                    e.kind.answers(needs),
                    "{:?} does not answer {needs:?}",
                    e.kind
                );
            }
            let best = plan.best();
            assert!(plan.estimates().iter().all(|e| e.bytes >= best.bytes));
        }
    }

    /// The default objective is what `plan` always did, to the byte and to the order.
    #[test]
    fn memory_ranks_exactly_as_it_always_did() {
        let keys = corpus(2_000);
        for bits in 0..32u8 {
            let needs = Needs {
                reverse: bits & 1 != 0,
                ordered: bits & 2 != 0,
                prefix: bits & 4 != 0,
                fuzzy: bits & 8 != 0,
                exact: bits & 16 != 0,
            };
            let p = plan(&keys, needs).unwrap();
            assert_eq!(p.objective(), Objective::Memory);
            assert_eq!(p, plan_for(&keys, needs, Objective::Memory).unwrap());
            assert!(p.estimates().windows(2).all(|w| w[0].bytes <= w[1].bytes));
        }
    }

    /// The objectives disagree, which is the whole point of having them: asked for exact
    /// membership and a reverse lookup, the smallest answer is the dictionary and the fastest is
    /// the perfect hash.
    #[test]
    fn latency_and_memory_pick_different_indexes() {
        let keys = corpus(2_000);
        let needs = Needs::default().exact().reverse();
        let small = plan_for(&keys, needs, Objective::Memory).unwrap();
        let quick = plan_for(&keys, needs, Objective::Latency).unwrap();
        assert_eq!(small.best().kind, Kind::Dict);
        assert!(
            quick
                .estimates()
                .windows(2)
                .all(|w| { quick.nanos(&w[0]) <= quick.nanos(&w[1]) })
        );
        if cfg!(feature = "mph") {
            assert_eq!(quick.best().kind, Kind::Perfect);
            assert!(quick.best().bytes > small.best().bytes);
            assert!(quick.nanos(&quick.best()) < small.nanos(&small.best()));
        }
        // Same candidates, same sizes; only the order moved.
        let mut a: Vec<_> = small.estimates().to_vec();
        let mut b: Vec<_> = quick.estimates().to_vec();
        a.sort_by_key(|e| e.bytes);
        b.sort_by_key(|e| e.bytes);
        assert_eq!(a, b);
    }

    /// Balance is what it says: the winner gives up less, on whichever axis it does worse, than
    /// either extreme's winner does on the axis it ignores.
    #[test]
    fn balanced_gives_up_less_than_either_extreme() {
        let keys = corpus(2_000);
        let needs = Needs::default().exact().reverse();
        let plans: Vec<Plan> = [Objective::Memory, Objective::Latency, Objective::Balanced]
            .into_iter()
            .map(|o| plan_for(&keys, needs, o).unwrap())
            .collect();
        let small = plans[0].best().bytes as f64;
        let fast = plans[1].nanos(&plans[1].best());
        let worse = |p: &Plan| {
            let e = p.best();
            (e.bytes as f64 / small).max(p.nanos(&e) / fast)
        };
        let balanced = worse(&plans[2]);
        assert!(balanced <= worse(&plans[0]) + f64::EPSILON, "{balanced}");
        assert!(balanced <= worse(&plans[1]) + f64::EPSILON, "{balanced}");
    }

    /// The model is a curve in the block, fitted at four points and read anywhere in 1..=1024.
    #[test]
    fn the_dict_cost_follows_the_block_it_was_measured_at() {
        let at = |block: usize| cost_of(Kind::Dict, Some(block)).nanos(SAMPLE, 10.0);
        // Below and above the measured ends it is clamped, not extrapolated.
        assert_eq!(at(1), at(32));
        assert_eq!(at(1024), at(4096));
        // Between them it moves, and at 100 000 keys a bigger block costs more.
        assert!(at(32) < at(64) && at(64) < at(128));
        assert!(at(256) < at(512) && at(512) < at(1024));
    }

    /// Both arguments of the model do what the measurements said they do.
    #[test]
    fn the_model_grows_with_the_corpus_and_with_the_keys() {
        for kind in [Kind::Closed, Kind::Perfect, Kind::String, Kind::Dict] {
            if !kind.available() {
                continue;
            }
            let c = cost_of(kind, None);
            assert!(
                c.nanos(SAMPLE, 10.0) < c.nanos(SAMPLE * 100, 10.0),
                "{kind:?}"
            );
            assert!(c.nanos(SAMPLE, 10.0) < c.nanos(SAMPLE, 40.0), "{kind:?}");
            // Nothing below the sample extrapolates past where the fit has evidence.
            assert_eq!(c.nanos(1, 10.0), c.nanos(SAMPLE, 10.0), "{kind:?}");
        }
    }

    /// The ladder says what it ranked by and that the nanoseconds are not a measurement.
    #[test]
    fn the_ladder_names_its_objective_and_disclaims_the_model() {
        let keys = corpus(500);
        for (objective, word) in [
            (Objective::Memory, "ranked by size"),
            (Objective::Latency, "ranked by latency"),
            (Objective::Balanced, "ranked by balance"),
        ] {
            let text = plan_for(&keys, Needs::default(), objective)
                .unwrap()
                .to_string();
            assert!(text.contains(word), "{text}");
            assert!(text.contains(" ns  "), "{text}");
            assert!(text.contains("not a measurement of yours"), "{text}");
        }
    }

    #[test]
    fn a_fuzzy_question_has_exactly_one_answer() {
        let plan = plan(&corpus(500), Needs::default().fuzzy()).unwrap();
        assert_eq!(plan.estimates().len(), 1);
        assert_eq!(plan.best().kind, Kind::String);
    }

    #[test]
    fn a_corpus_no_larger_than_the_sample_is_weighed_rather_than_modelled() {
        let keys = corpus(1_500);
        let plan = plan(&keys, Needs::default().reverse().ordered()).unwrap();
        assert!(plan.estimates().iter().all(|e| e.measured));
        assert!(
            !plan.thin(),
            "a weighed plan is never the model's blind spot"
        );
        let dict = plan
            .estimates()
            .iter()
            .find(|e| e.kind == Kind::Dict)
            .unwrap();
        let built = DictIndex::build_with_block(&keys, dict.block.unwrap())
            .unwrap()
            .serialized_len();
        assert_eq!(
            dict.bytes, built as u64,
            "a weighed estimate is the build itself"
        );
        let fst = plan
            .estimates()
            .iter()
            .find(|e| e.kind == Kind::String)
            .unwrap();
        assert_eq!(
            fst.bytes,
            StringIndex::build(&keys).unwrap().serialized_len() as u64
        );
    }

    #[test]
    fn the_model_lands_near_the_truth() {
        let keys = corpus(20_000);
        let plan = with_sample(2_000, || plan(&keys, Needs::default()).unwrap());
        assert!(plan.estimates().iter().all(|e| !e.measured));
        for e in plan.estimates() {
            let truth = match e.kind {
                Kind::String => StringIndex::build(&keys).unwrap().serialized_len() as u64,
                Kind::Dict => DictIndex::build_with_block(&keys, e.block.unwrap())
                    .unwrap()
                    .serialized_len() as u64,
                #[cfg(feature = "mph")]
                Kind::Compact => CompactHashIndex::build(&keys, 1)
                    .unwrap()
                    .serialized_len()
                    .unwrap() as u64,
                #[cfg(feature = "mph")]
                Kind::Closed => ClosedHashIndex::build(&keys).unwrap().serialized_len() as u64,
                #[cfg(feature = "mph")]
                Kind::Perfect => PerfectHashIndex::build(&keys)
                    .unwrap()
                    .serialized_len()
                    .unwrap() as u64,
                #[cfg(not(feature = "mph"))]
                _ => unreachable!(),
            };
            let err = (e.bytes as f64 - truth as f64) / truth as f64;
            assert!(
                err.abs() < 0.15,
                "{:?}: {} against {truth} ({:+.1} %)",
                e.kind,
                e.bytes,
                100.0 * err
            );
        }
    }

    /// The block is a candidate, not a default: all three are priced, and the ladder marks the one
    /// that won rather than every row of the kind that won.
    #[test]
    fn every_dict_block_is_its_own_candidate() {
        let keys = corpus(1_500);
        let plan = plan(&keys, Needs::default().ordered()).unwrap();
        let dicts: Vec<&Estimate> = plan
            .estimates()
            .iter()
            .filter(|e| e.kind == Kind::Dict)
            .collect();
        assert_eq!(dicts.len(), DICT_BLOCKS.len());
        for e in &dicts {
            let block = e.block.unwrap();
            assert!(DICT_BLOCKS.contains(&block), "{block}");
            let built = DictIndex::build_with_block(&keys, block)
                .unwrap()
                .serialized_len() as u64;
            assert_eq!(e.bytes, built, "at block {block}");
        }
        let mut blocks: Vec<usize> = dicts.iter().map(|e| e.block.unwrap()).collect();
        blocks.sort_unstable();
        blocks.dedup();
        assert_eq!(blocks.len(), DICT_BLOCKS.len(), "one row a block");

        let text = plan.to_string();
        assert_eq!(text.matches('*').count(), 1, "one winner: {text}");
        for block in DICT_BLOCKS {
            assert!(text.contains(&format!("at block {block}")), "{text}");
        }
    }

    /// The modelled block curve is the one a real build walks: bigger blocks are smaller, and each
    /// point is within a couple of per cent of the build it predicts.
    #[test]
    fn the_model_prices_each_block_within_two_per_cent() {
        let keys = corpus(20_000);
        let plan = with_sample(2_000, || {
            plan_for(&keys, Needs::default().ordered(), Objective::Memory).unwrap()
        });
        let mut dicts: Vec<&Estimate> = plan
            .estimates()
            .iter()
            .filter(|e| e.kind == Kind::Dict)
            .collect();
        dicts.sort_by_key(|e| e.block);
        assert_eq!(dicts.len(), DICT_BLOCKS.len());
        for e in &dicts {
            assert!(!e.measured);
            let truth = DictIndex::build_with_block(&keys, e.block.unwrap())
                .unwrap()
                .serialized_len() as f64;
            let err = (e.bytes as f64 - truth) / truth;
            assert!(
                err.abs() < 0.02,
                "block {:?}: {} against {truth} ({:+.1} %)",
                e.block,
                e.bytes,
                100.0 * err
            );
        }
        assert!(
            dicts.windows(2).all(|w| w[0].bytes > w[1].bytes),
            "a bigger block stores less: {dicts:?}"
        );
    }

    /// The draw takes the size it asked for at any corpus size. A `step_by(n / want)` gives a step
    /// of one just past `want` -- 150 001 keys would "sample" all 150 001 of them -- and the
    /// fractional stride is what fixes it.
    #[test]
    fn the_sample_is_the_size_it_asked_for() {
        let keys = corpus(150_001);
        let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 150_001, "the corpus is distinct");

        let drawn = sample(&sorted, SAMPLE);
        assert_eq!(drawn.len(), SAMPLE);
        assert!(drawn.windows(2).all(|w| w[0] < w[1]), "in order, distinct");
        // Same keys, same draw: a plan over one corpus is reproducible.
        assert_eq!(drawn, sample(&sorted, SAMPLE));
        // And the phase is the corpus's, so two corpora of the same size do not draw alike.
        let other = corpus(150_001)
            .iter()
            .map(|k| format!("z{k}"))
            .collect::<Vec<_>>();
        let mut theirs: Vec<&str> = other.iter().map(String::as_str).collect();
        theirs.sort_unstable();
        let offsets = |ks: &[&str], drawn: &[&str]| -> Vec<usize> {
            drawn
                .iter()
                .map(|k| ks.binary_search(k).unwrap())
                .take(16)
                .collect()
        };
        assert_ne!(
            offsets(&sorted, &drawn),
            offsets(&theirs, &sample(&theirs, SAMPLE))
        );

        // A corpus no larger than the draw is the draw.
        assert_eq!(sample(&sorted[..SAMPLE], SAMPLE).len(), SAMPLE);
    }

    /// Two dictionary blocks are always within a per cent or two of each other, and warning about
    /// that would be warning about every plan. The doubt is about kinds.
    #[test]
    fn two_dict_blocks_are_not_the_close_call() {
        let estimate = |kind, block, bytes| Estimate {
            kind,
            bytes,
            block,
            measured: false,
        };
        let of = |estimates| Plan {
            keys: 1_000,
            mean_len: 10.0,
            mean_lcp: 4.0,
            estimates,
            needs: Needs::default(),
            objective: Objective::Memory,
        };
        let dicts = |blocks: [u64; 3]| {
            vec![
                estimate(Kind::Dict, Some(32), blocks[0]),
                estimate(Kind::Dict, Some(256), blocks[1]),
                estimate(Kind::Dict, Some(1024), blocks[2]),
            ]
        };

        let mut rows = dicts([1_020, 1_010, 1_000]);
        rows.push(estimate(Kind::String, None, 4_000));
        assert!(!of(rows).close(), "three blocks of one kind are one answer");

        let mut rows = dicts([1_020, 1_010, 1_000]);
        rows.push(estimate(Kind::String, None, 1_100));
        assert!(of(rows).close(), "a second kind inside 1.3x is the warning");
    }

    #[test]
    fn an_empty_corpus_is_a_plan_and_not_an_error() {
        let plan = plan::<String>(&[], Needs::default()).unwrap();
        assert_eq!(plan.keys(), 0);
        assert_eq!(plan.best().bytes_per_key(0), 0.0);
    }

    #[test]
    fn duplicates_are_counted_once() {
        let keys = ["b", "a", "b", "a", "c"];
        assert_eq!(plan(&keys, Needs::default()).unwrap().keys(), 3);
    }

    #[test]
    fn the_explanation_names_the_choice_and_the_shape() {
        let text = plan(&corpus(400), Needs::default().prefix())
            .unwrap()
            .to_string();
        assert!(
            text.contains("DictIndex") || text.contains("StringIndex"),
            "{text}"
        );
        assert!(text.contains("keys, mean length"), "{text}");
        assert!(text.contains('*'), "the chosen line is marked: {text}");
    }
}
