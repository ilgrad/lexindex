//! What each index would cost on a set of keys, and which of them answer the caller's questions.
//!
//! Five indexes with five size curves is a choice nobody should have to make from a README table:
//! the sizes are corpus-specific, and the spread between them on one corpus is larger than the
//! spread of any one of them across corpora. [`plan`] sorts the keys once, measures the statistics
//! the formats are actually paid in, and prices every index the caller's [`Needs`] allow.
//!
//! **What is measured and what is modelled.** The sort makes `n`, the mean key length and the LCP
//! structure exact; everything a format does with them is arithmetic. The one input no statistic
//! gives is what the symbol table and the phrase dictionary squeeze a suffix into, and that can only
//! be read off a build — so two 100 000-key draws supply it, both coded under the vocabulary the
//! *corpus* would buy, along with the bytes an fst spends a key and the bits the perfect hash
//! spends per key. Scored against the built blob on 23 corpora of half a million to ten
//! million keys, at each of the three priced blocks, the [`DictIndex`] estimate lands within
//! **1.0 % median, 4.2 % at the 90th percentile and 7.2 % at worst**.
//! [`StringIndex`] is looser — **1.1 % median, 6.7 % at the 90th percentile and 11.7 % at worst**
//! over 19 corpora of a quarter million to nineteen million keys — because an fst merges equal
//! right-languages, and how much it merges is a property of the *density* of the whole key set
//! rather than of any statistic of it. Only the run draw keeps that density, which is why the fst
//! rate is read off that one and carried flat; the same rate read off the hash draw and scaled by
//! trie nodes, which is what this crate shipped through 4.0, lands at 5.2 / 36.0 / 53.1 instead.
//! The family that breaks it outright is a *dense* id space — ten million decimal ids merge to
//! 356 bytes whole, which no draw of a hundred thousand of them can see, and the estimate reads
//! about a hundredfold high. The ranking survives, since a hundred times almost nothing is still
//! two orders under every other index; the byte count does not, and [`Plan`] flags it rather than
//! letting a caller quote the number it trusts least.
//!
//! Below the sample size there is nothing to model: the plan builds all the candidates and reports
//! what they weigh.

use crate::charcode::{self, CharCode, Tally};
use crate::dict_index::{AsKey, views};
use crate::{DictIndex, IndexError, StringIndex, dict_index};
use std::fmt;

#[cfg(feature = "mph")]
use crate::{ClosedHashIndex, CompactHashIndex, PerfectHashIndex};

/// Keys a sample holds. Below this the plan builds the real indexes instead of modelling them.
const SAMPLE: usize = 100_000;
/// Keys a run of the sample holds: consecutive keys of the sorted corpus, so that what the sample
/// shares between neighbours — the `(lcp, len)` pairs a header code is learned on, the 32-back
/// distance a restart is coded against — is the corpus's own. A uniform draw put every sampled key
/// about `n / SAMPLE` positions from the next and read the header rate 0.51 where a million decimal
/// ids spend 0.34, a third of that blob. Measured over run lengths 256 to 16 384 on eight corpora
/// at three blocks: 256 leaks 1 % through the 32-back pairs that cross a run boundary, 1 024 and
/// 4 096 price alike, and 4 096 has the better median and worst case of the two.
const RUN: usize = 4096;

/// The [`DictIndex`] blocks the plan prices. The block is a knob, not a constant — on an English
/// word list the three named points of its curve span 2.51 to 2.85 bytes a key — so a ranking that
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
/// build both. The model's error is inside 15 % at the 90th percentile of where it was validated
/// and 23 % at worst; 1.3× covers both.
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
    /// The cheapest to run a [`Workload`] on: every candidate priced at the mean of the operations
    /// the workload asks, weighted as it weighs them, from the model behind
    /// [`Latency`](Self::Latency). An operation is a question the index has to answer as well, so a
    /// workload adds to the [`Needs`] — see [`Workload`].
    Workload(Workload),
}

impl Objective {
    /// `needs`, with whatever this objective's own operations require added to it.
    fn asks(self, needs: Needs) -> Needs {
        match self {
            Self::Workload(workload) => workload.asks(needs),
            _ => needs,
        }
    }
}

/// How often each operation is asked of the index, for [`Objective::Workload`].
///
/// The weights are relative: `hits(9).misses(1)` is nine hits to a miss, and so is
/// `hits(900).misses(100)`, so counts read off a log serve as they are. An operation left at zero
/// is one the workload never asks, and a workload that asks none of them is an `id(key)` over half
/// members and half strangers — [`Objective::Latency`], exactly.
///
/// Every operation is a question the index must be able to answer, so the plan adds it to the
/// [`Needs`]: a share of [`reverse`](Self::reverse) rules out the two indexes that store no keys,
/// as [`Needs::reverse`] does, and a share of any of the three prefix queries asks for an ordered
/// index, as [`Needs::prefix`] does.
///
/// ```
/// # use lexindex::{plan_for, Kind, Needs, Objective, Workload};
/// let keys = ["apple", "apricot", "banana", "blueberry"];
/// let asked = Workload::default().hits(1).common_prefix(9);
/// let p = plan_for(&keys, Needs::default(), Objective::Workload(asked))?;
/// // A prefix query asks for an ordered index, whatever the needs said.
/// assert!(matches!(p.best().kind, Kind::String | Kind::Dict));
/// # Ok::<(), lexindex::IndexError>(())
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Workload {
    hits: u64,
    misses: u64,
    reverse: u64,
    prefix: u64,
    common_prefix: u64,
    longest_prefix: u64,
    batch: u32,
}

impl Workload {
    /// `id(key)` on a key the index holds.
    pub fn hits(mut self, weight: u64) -> Self {
        self.hits = weight;
        self
    }

    /// `id(key)` on a key it does not hold.
    pub fn misses(mut self, weight: u64) -> Self {
        self.misses = weight;
        self
    }

    /// `key(id)`.
    pub fn reverse(mut self, weight: u64) -> Self {
        self.reverse = weight;
        self
    }

    /// A prefix or range query, priced as a `prefix_count`: what enumerating one costs past the
    /// count is how many keys it returns, which is a fact about the corpus and not the index.
    pub fn prefix(mut self, weight: u64) -> Self {
        self.prefix = weight;
        self
    }

    /// `common_prefix(query)`: every key that is a prefix of the query.
    pub fn common_prefix(mut self, weight: u64) -> Self {
        self.common_prefix = weight;
        self
    }

    /// `longest_prefix(query)`: the longest key that is a prefix of the query.
    pub fn longest_prefix(mut self, weight: u64) -> Self {
        self.longest_prefix = weight;
        self
    }

    /// Keys an `ids_of` call holds, for the [`hits`](Self::hits) and [`misses`](Self::misses);
    /// `0` and `1` are one key a call. Batches of 16 and 1 024 were measured: in between, what a
    /// key costs is linear in `log2(size)`, and past 1 024 it costs what it does at 1 024.
    pub fn batch(mut self, size: u32) -> Self {
        self.batch = size;
        self
    }

    /// `needs`, with what these operations require added to it.
    fn asks(self, needs: Needs) -> Needs {
        Needs {
            reverse: needs.reverse || self.reverse > 0,
            prefix: needs.prefix
                || self.prefix > 0
                || self.common_prefix > 0
                || self.longest_prefix > 0,
            ..needs
        }
    }

    /// The mean nanoseconds of this workload's operations on a structure with these costs, at
    /// `(s, len)` from [`Ops::at`]; infinite if the structure cannot answer one of them.
    fn nanos(self, ops: &Ops, s: f64, len: f64) -> f64 {
        let at = |cost: Cost| cost.nanos(s, len);
        // A batch was measured over mixed probes, so what it saves is read there, as a share of
        // one call, and taken to save hits and misses alike.
        let one = at(ops.mixed);
        let log = f64::from(self.batch.max(1)).log2().min(10.0);
        let batched = if log <= 4.0 {
            one + (at(ops.batch16) - one) * log / 4.0
        } else {
            at(ops.batch16) + (at(ops.batch1024) - at(ops.batch16)) * (log - 4.0) / 6.0
        };
        let share = batched / one;
        let asked = [
            (self.hits, Some(ops.hit), share),
            (self.misses, Some(ops.miss), share),
            (self.reverse, ops.key, 1.0),
            (self.prefix, ops.prefix, 1.0),
            (self.common_prefix, ops.common_prefix, 1.0),
            (self.longest_prefix, ops.longest_prefix, 1.0),
        ];
        let total: f64 = asked.iter().map(|&(weight, ..)| weight as f64).sum();
        if total == 0.0 {
            return batched;
        }
        asked
            .iter()
            .filter(|&&(weight, ..)| weight > 0)
            .map(|&(weight, cost, share)| weight as f64 * cost.map_or(f64::INFINITY, at) * share)
            .sum::<f64>()
            / total
    }

    /// What the ladder's first line says was ranked by. A workload that asks nothing is latency,
    /// and says so.
    fn describe(self) -> String {
        let mut asked: Vec<String> = [
            ("hits", self.hits),
            ("misses", self.misses),
            ("reverse", self.reverse),
            ("prefix", self.prefix),
            ("common_prefix", self.common_prefix),
            ("longest_prefix", self.longest_prefix),
        ]
        .iter()
        .filter(|&&(_, weight)| weight > 0)
        .map(|(name, weight)| format!("{name} {weight}"))
        .collect();
        let name = if asked.is_empty() {
            "latency"
        } else {
            "workload"
        };
        if self.batch > 1 {
            asked.push(format!("batch {}", self.batch));
        }
        if asked.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({})", asked.join(", "))
        }
    }
}

/// One operation's latency constants: `a + b·s + c·len + d·s·len` nanoseconds, where `len` is the
/// mean key length in bytes and `s` is where the candidate's blob sits against the cache, as
/// [`Ops::at`] reads it.
#[derive(Clone, Copy)]
struct Cost {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
}

impl Cost {
    fn nanos(self, s: f64, len: f64) -> f64 {
        self.a + self.b * s + self.c * len + self.d * s * len
    }

    /// Between two measured blocks, linear in `log2(block)`, which is what the constants move in
    /// over 32..=1024.
    fn between(self, other: Self, t: f64) -> Self {
        let mix = |x: f64, y: f64| x + (y - x) * t;
        Self {
            a: mix(self.a, other.a),
            b: mix(self.b, other.b),
            c: mix(self.c, other.c),
            d: mix(self.d, other.d),
        }
    }
}

/// One entry of the tables below, on one line.
const fn cost(a: f64, b: f64, c: f64, d: f64) -> Cost {
    Cost { a, b, c, d }
}

/// Where `s` counts from: a blob under 64 KiB is priced as a blob of 64 KiB.
const HINGE: f64 = 65_536.0;

/// The shortest mean key length of any corpus the model was fitted on. Shorter keys are priced as
/// these, rather than on a slope read off longer ones.
const LEN_FLOOR: f64 = 4.89;

/// One structure's constants for each operation the model prices, and `None` for one it does not
/// answer.
#[derive(Clone, Copy)]
struct Ops {
    /// The smallest `s` the structure was fitted at, never below zero. A smaller blob is priced
    /// here, since below it the fit has no evidence.
    floor: f64,
    /// `id(key)` over half members and half strangers, shuffled: what [`Objective::Latency`] and
    /// [`Objective::Balanced`] rank by.
    mixed: Cost,
    /// `id(key)` on members.
    hit: Cost,
    /// `id(key)` on strangers.
    miss: Cost,
    /// A key of an `ids_of` batch of 16, half members and half strangers.
    batch16: Cost,
    /// The same, in batches of 1 024.
    batch1024: Cost,
    /// `key(id)`.
    key: Option<Cost>,
    /// `prefix_count` of a member's first three characters.
    prefix: Option<Cost>,
    /// `common_prefix` of a member with one to three characters added, or of the characters alone.
    common_prefix: Option<Cost>,
    /// `longest_prefix` of the same.
    longest_prefix: Option<Cost>,
}

impl Ops {
    /// `(s, len)` for a candidate whose blob is `bytes` over `keys` keys of mean length `mean_len`.
    ///
    /// `s` is `log2` of the blob over [`HINGE`] rather than of the key count, because what a lookup
    /// waits on is how far its bytes are from the CPU, and the bytes a key takes differ tenfold
    /// between corpora for a `DictIndex` alone. A corpus under [`SAMPLE`] keys is read at its
    /// [`SAMPLE`]-key equivalent — a smaller corpus is not slower, and was not measured — and `s`
    /// and `len` are held at the edges of the evidence, [`floor`](Self::floor) and [`LEN_FLOOR`].
    fn at(&self, bytes: u64, keys: usize, mean_len: f64) -> (f64, f64) {
        let scale = (SAMPLE as f64 / keys.max(1) as f64).max(1.0);
        let s = (bytes as f64 * scale / HINGE).log2().max(self.floor);
        (s, mean_len.max(LEN_FLOOR))
    }

    /// Between two measured blocks, each constant the way [`Cost::between`] moves one.
    fn between(self, other: Self, t: f64) -> Self {
        let mix = |x: Option<Cost>, y: Option<Cost>| Some(x?.between(y?, t));
        Self {
            floor: self.floor + (other.floor - self.floor) * t,
            mixed: self.mixed.between(other.mixed, t),
            hit: self.hit.between(other.hit, t),
            miss: self.miss.between(other.miss, t),
            batch16: self.batch16.between(other.batch16, t),
            batch1024: self.batch1024.between(other.batch1024, t),
            key: mix(self.key, other.key),
            prefix: mix(self.prefix, other.prefix),
            common_prefix: mix(self.common_prefix, other.common_prefix),
            longest_prefix: mix(self.longest_prefix, other.longest_prefix),
        }
    }
}

/// Least squares on relative error over 240 measured cells: eight structures over thirteen corpora
/// at 100 000 keys, eleven at 1 000 000 and six at 10 000 000, in one process, every operation over
/// fresh probes of its own and every structure timed at every place in its operation's round.
/// `bench/latency_model.py measure` produced them and `fit` re-derives these tables from
/// `bench/results/latency-model-2026-09-23-arz-7731027.json`, with no slope below zero; the two
/// `HashedDictIndex` lanes that artifact also holds are measured and not priced. Mean absolute
/// error is 6–12 % of the measurement on `id(key)` and 26–42 % on prefix counts, whose cost is
/// mostly how many keys the prefix matches.
///
/// **Far too coarse to quote and quite enough to rank.** Fitted with a corpus left out, the model
/// names the fastest index on that corpus in 30 of 30 corpus-size cells for `id(key)`, for nine
/// hits to a miss, for batches of 1 024, for `key(id)` and for a `common_prefix`-heavy workload;
/// over ten workloads the mean pick costs 1.000–1.050× the fastest. Against the *published*
/// sweep, which it was not fitted to, it names the fastest of every lane in 28 of 30 cells — never
/// more than 1.12× slower — the faster of `DictIndex` and `StringIndex` in 29, never more than
/// 1.02× slower, and the fastest `DictIndex` block in 21, never more than 1.05× slower.
///
/// **What it gets wrong is a choice between ordered indexes, because there the candidates are
/// close.** The transducer is the faster at 100 000 keys on six corpora of thirteen and at every
/// size on `numeric`, and the blocks of one dictionary are often within a few per cent of each
/// other. Held out, the fastest ordered `id(key)` is named in 19 of 30 cells and one within 5 %
/// of it — what placement in physical memory moves a single-threaded probe on this machine — in
/// 23, never more than 1.11× slower. Nine prefix counts to a hit is the weakest workload: 15 and
/// 23 of 30, and 1.64× on ten million `numeric` ids, whose transducer of a few hundred bytes is
/// smaller than any the fit sees once `numeric` is left out.
///
/// **These are one machine's cache latencies**, an AMD Ryzen 7 5800HS with 16 MB of L3, timed
/// through the Python binding so that every one of them carries that call. Nothing here rescales
/// them for another machine, and no number they produce is a measurement of the caller's corpus.
const COST: [(Kind, Ops); 4] = [
    (
        Kind::Closed,
        Ops {
            floor: 0.00,
            mixed: cost(47.4, 8.87, 0.060, 0.000),
            hit: cost(47.5, 8.47, 0.062, 0.000),
            miss: cost(45.3, 8.78, 0.068, 0.000),
            batch16: cost(34.1, 1.98, 0.073, 0.000),
            batch1024: cost(31.9, 0.78, 0.063, 0.000),
            key: None,
            prefix: None,
            common_prefix: None,
            longest_prefix: None,
        },
    ),
    (
        Kind::Compact,
        Ops {
            floor: 0.92,
            mixed: cost(43.1, 16.10, 0.095, 0.000),
            hit: cost(37.5, 15.90, 0.110, 0.000),
            miss: cost(26.6, 15.13, 0.103, 0.010),
            batch16: cost(39.0, 3.06, 0.090, 0.000),
            batch1024: cost(33.7, 1.45, 0.070, 0.000),
            key: None,
            prefix: None,
            common_prefix: None,
            longest_prefix: None,
        },
    ),
    (
        Kind::Perfect,
        Ops {
            floor: 3.30,
            mixed: cost(-7.7, 22.55, 0.000, 0.000),
            hit: cost(-23.3, 27.28, 0.000, 0.000),
            miss: cost(-21.6, 16.27, 0.000, 0.000),
            batch16: cost(36.9, 5.38, 0.028, 0.000),
            batch1024: cost(35.6, 2.62, 0.066, 0.000),
            key: Some(cost(-4.7, 21.27, 0.042, 0.000)),
            prefix: None,
            common_prefix: None,
            longest_prefix: None,
        },
    ),
    (
        Kind::String,
        Ops {
            floor: 0.00,
            mixed: cost(152.2, 15.30, 0.000, 1.209),
            hit: cost(150.1, 18.16, 0.000, 1.185),
            miss: cost(142.0, 15.55, 0.000, 1.206),
            batch16: cost(143.2, 18.03, 0.000, 1.191),
            batch1024: cost(138.9, 18.45, 0.000, 1.186),
            key: Some(cost(251.3, 22.94, 10.942, 1.541)),
            prefix: Some(cost(365.8, 88.14, 1.537, 0.000)),
            common_prefix: Some(cost(347.8, 0.00, 0.000, 1.149)),
            longest_prefix: Some(cost(212.7, 17.36, 0.000, 0.968)),
        },
    ),
];

/// [`Kind::Dict`] is a curve, not a point: the block trades a scan against a search and both ends
/// of 32..=1024 were measured. Ascending by block, interpolated in between and clamped outside.
const DICT_COST: [(usize, Ops); 4] = [
    (
        32,
        Ops {
            floor: 0.86,
            mixed: cost(147.3, 34.02, 0.000, 0.358),
            hit: cost(130.4, 40.33, 0.000, 0.325),
            miss: cost(130.0, 39.71, 0.000, 0.339),
            batch16: cost(223.9, 23.65, 0.000, 0.310),
            batch1024: cost(211.4, 23.94, 0.000, 0.308),
            key: Some(cost(151.8, 20.66, 0.000, 0.319)),
            prefix: Some(cost(270.8, 0.00, 0.000, 0.000)),
            common_prefix: Some(cost(1158.8, 56.94, 44.316, 4.957)),
            longest_prefix: Some(cost(513.8, 40.82, 0.000, 0.478)),
        },
    ),
    (
        128,
        Ops {
            floor: 0.65,
            mixed: cost(191.1, 27.69, 0.000, 0.395),
            hit: cost(177.7, 32.61, 0.000, 0.373),
            miss: cost(174.3, 32.61, 0.000, 0.380),
            batch16: cost(238.5, 24.68, 0.000, 0.345),
            batch1024: cost(223.8, 25.77, 0.000, 0.340),
            key: Some(cost(189.4, 18.28, 0.024, 0.430)),
            prefix: Some(cost(274.1, 0.00, 0.000, 0.000)),
            common_prefix: Some(cost(1222.8, 48.86, 54.566, 4.425)),
            longest_prefix: Some(cost(592.7, 32.15, 0.000, 0.490)),
        },
    ),
    (
        256,
        Ops {
            floor: 0.63,
            mixed: cost(222.0, 22.85, 0.000, 0.419),
            hit: cost(210.7, 27.38, 0.000, 0.392),
            miss: cost(206.3, 27.40, 0.000, 0.400),
            batch16: cost(257.7, 24.13, 0.000, 0.355),
            batch1024: cost(243.9, 24.82, 0.000, 0.351),
            key: Some(cost(209.9, 16.45, 0.591, 0.382)),
            prefix: Some(cost(282.5, 0.00, 0.000, 0.000)),
            common_prefix: Some(cost(1191.7, 56.70, 65.209, 3.793)),
            longest_prefix: Some(cost(634.6, 28.12, 0.000, 0.525)),
        },
    ),
    (
        1024,
        Ops {
            floor: 0.49,
            mixed: cost(289.2, 18.60, 0.000, 0.428),
            hit: cost(280.2, 21.96, 0.000, 0.416),
            miss: cost(274.0, 21.96, 0.000, 0.417),
            batch16: cost(317.5, 22.11, 0.000, 0.383),
            batch1024: cost(303.6, 22.25, 0.000, 0.380),
            key: Some(cost(267.1, 18.74, 1.136, 0.406)),
            prefix: Some(cost(352.2, 0.00, 0.000, 0.000)),
            common_prefix: Some(cost(1232.7, 81.44, 92.909, 2.122)),
            longest_prefix: Some(cost(744.2, 26.80, 0.849, 0.432)),
        },
    ),
];

/// The constants for one candidate.
fn ops_of(kind: Kind, block: Option<usize>) -> Ops {
    if kind != Kind::Dict {
        let (_, ops) = COST
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("every kind but Dict is in the table");
        return *ops;
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

/// What one operation of `objective` is modelled to cost on a candidate: `id(key)`, unless the
/// objective is a workload, whose mean operation it is.
fn nanos_of(objective: Objective, estimate: &Estimate, keys: usize, mean_len: f64) -> f64 {
    let ops = ops_of(estimate.kind, estimate.block);
    let (s, len) = ops.at(estimate.bytes, keys, mean_len);
    match objective {
        Objective::Workload(workload) => workload.nanos(&ops, s, len),
        _ => ops.mixed.nanos(s, len),
    }
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

    /// What every candidate had to answer: the needs the plan was asked for, and whatever the
    /// [`Objective`] adds to them. [`Kind::answers`] against these is why a kind is missing from
    /// the ranking.
    ///
    /// ```
    /// # use lexindex::{plan_for, Kind, Needs, Objective, Workload};
    /// let asked = Objective::Workload(Workload::default().hits(9).longest_prefix(1));
    /// let p = plan_for(&["apple", "apricot"], Needs::default(), asked)?;
    /// // A prefix query is a question only an ordered index answers.
    /// assert!(p.needs().prefix && !Kind::Perfect.answers(p.needs()));
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn needs(&self) -> Needs {
        self.needs
    }

    /// The modelled cost of one of the objective's operations on a candidate, in nanoseconds:
    /// `id(key)`, or under [`Objective::Workload`] the workload's mean operation — infinite on a
    /// candidate that cannot answer one of them.
    ///
    /// **An estimate, and of one machine.** `a + b·s + c·mean_len + d·s·mean_len` per structure
    /// and operation, where `s` is `log2` of the candidate's blob over 64 KiB, fitted to 240 cells
    /// measured on this crate's own hardware — an AMD Ryzen 7 5800HS with 16 MB of L3 — with mean
    /// absolute error 6–12 % on `id(key)`. It is accurate enough to order the candidates and
    /// nowhere near accurate enough to quote: fitted with a corpus left out, its fastest ordered
    /// index on that corpus is the measured fastest in 19 of 30 corpus-size cells, within 5 % of
    /// it in 23 and never more than 1.11× slower — and it will not tell you what your own lookup
    /// costs.
    ///
    /// ```
    /// # use lexindex::{plan, Needs};
    /// let p = plan(&["apple", "apricot", "banana"], Needs::default())?;
    /// // Cheaper to store is not cheaper to ask.
    /// assert!(p.estimates().iter().all(|e| p.nanos(e) > 0.0));
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn nanos(&self, estimate: &Estimate) -> f64 {
        nanos_of(self.objective, estimate, self.keys, self.mean_len)
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
                Objective::Memory => "size".to_string(),
                Objective::Latency => "latency".to_string(),
                Objective::Balanced => "balance".to_string(),
                Objective::Workload(workload) => workload.describe(),
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
#[derive(Default, Clone)]
struct Shape {
    n: usize,
    /// Bytes in all the keys.
    len: u64,
    /// Those of them outside ASCII: what says whether a character code is worth counting.
    high: u64,
    /// Bytes shared with the previous key, summed — what front coding does not store.
    lcp1: u64,
    /// The same for the keys 32 apart, which is what a restart entry is coded against.
    lcp32: u64,
    /// Pairs the `lcp32` sum is over.
    pairs32: u64,
}

impl Shape {
    fn of<K: AsKey>(keys: &[K]) -> Self {
        let mut walk = ShapeWalk::default();
        for k in keys {
            walk.push(k.key_bytes());
        }
        walk.finish()
    }

    /// This shape as a character code spells the keys, read off a draw of them in both spellings:
    /// the bytes scaled as the uniform draw's, the shared prefixes as those of the run draw's
    /// neighbours. `high` stays the keys' own; the choice it decides has been made.
    fn spelt(&self, spread: [&Shape; 2], runs: [&Shape; 2]) -> Shape {
        let scale = |x: u64, to: u64, from: u64| match from {
            0 => x,
            _ => (u128::from(x) * u128::from(to) / u128::from(from)) as u64,
        };
        Shape {
            len: scale(self.len, spread[1].len, spread[0].len),
            lcp1: scale(self.lcp1, runs[1].lcp1, runs[0].lcp1),
            lcp32: scale(self.lcp32, runs[1].lcp32, runs[0].lcp32),
            ..self.clone()
        }
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
}

/// Keys back a restart entry is coded against, and so the second distance [`Shape`] measures.
const LCP_BACK: usize = 32;

/// [`Shape`] gathered one key at a time, for a stream that is never all in memory at once.
///
/// The ring holds the last [`LCP_BACK`] keys, which is all a walk needs to see to measure both
/// distances: the key before this one, and the key a restart entry would be coded against.
#[derive(Default)]
struct ShapeWalk {
    shape: Shape,
    ring: Vec<Vec<u8>>,
}

impl ShapeWalk {
    /// The next key of an ascending, distinct stream.
    fn push(&mut self, key: &[u8]) {
        let n = self.shape.n;
        self.shape.len += key.len() as u64;
        self.shape.high += charcode::high_bytes(key);
        if n > 0 {
            let prev = &self.ring[(n - 1) % LCP_BACK];
            self.shape.lcp1 += dict_index::lcp(prev, key) as u64;
        }
        if n >= LCP_BACK {
            // The slot about to be overwritten holds the key `LCP_BACK` back, and nothing else
            // reads it.
            let back = &self.ring[n % LCP_BACK];
            self.shape.lcp32 += dict_index::lcp(back, key) as u64;
            self.shape.pairs32 += 1;
        }
        if self.ring.len() < LCP_BACK {
            self.ring.push(key.to_owned());
        } else {
            let slot = &mut self.ring[n % LCP_BACK];
            slot.clear();
            slot.extend_from_slice(key);
        }
        self.shape.n += 1;
    }

    fn finish(self) -> Shape {
        self.shape
    }
}

/// At most `want` keys of a stream, drawn by hash rather than by position.
///
/// A key is kept while its hash is under a cutoff; when twice the wanted count has gathered, the
/// cutoff drops to the median and the half above it goes. What survives is a uniform sample of the
/// distinct keys — the hash decides, not where a key fell — and it is the same sample on every run
/// over the same keys, which is what keeps a plan reproducible. The resident cost is bounded by
/// twice the sample, whatever the corpus, so the same draw serves [`plan`], which holds the sorted
/// keys, and [`plan_file`], which never holds them at all.
///
/// It is the draw for everything that is a property of the whole key set: what an fst spends on
/// a node — a stride over sorted keys takes one key from every prefix group whatever the group's
/// size, and a `StringIndex` estimate read off such a sample runs 5 % low on a corpus of shared
/// prefixes where this one lands within 0.4 % — and what vocabulary a corpus is worth, which a
/// draw of consecutive keys overstates: built for the corpus's economy, [`RunSample`] read the
/// suffix ratio 0.34 on a million paths where this draw and the blob read 0.39.
struct HashSample<K> {
    want: usize,
    cutoff: u64,
    kept: Vec<(u64, K)>,
}

impl<K: AsRef<str> + Ord> HashSample<K> {
    fn new(want: usize) -> Self {
        Self {
            want,
            cutoff: u64::MAX,
            kept: Vec::new(),
        }
    }

    fn push(&mut self, key: K) {
        let hash = crate::blob::hash_bytes(key.as_ref().as_bytes());
        self.keep(hash, key);
    }

    /// The hash of a key the draw would keep, and `None` for one it would not — so a caller that
    /// has to copy a key to hand it over copies only the ones that survive.
    fn wanted(&self, key: &str) -> Option<u64> {
        let hash = crate::blob::hash_bytes(key.as_bytes());
        (hash <= self.cutoff).then_some(hash)
    }

    fn keep(&mut self, hash: u64, key: K) {
        if hash > self.cutoff {
            return;
        }
        self.kept.push((hash, key));
        if self.want > 0 && self.kept.len() >= 2 * self.want {
            self.trim();
        }
    }

    fn trim(&mut self) {
        if self.kept.len() <= self.want {
            return;
        }
        self.kept
            .select_nth_unstable_by_key(self.want - 1, |(h, _)| *h);
        self.kept.truncate(self.want);
        self.cutoff = self.kept.iter().map(|(h, _)| *h).max().unwrap_or(u64::MAX);
    }

    /// The sample, ascending — the order every model constant is read in.
    fn finish(mut self) -> Vec<K> {
        self.trim();
        let mut keys: Vec<K> = self.kept.into_iter().map(|(_, k)| k).collect();
        keys.sort_unstable();
        keys
    }
}

/// At most `want` keys of a stream, in `want / RUN` runs of consecutive keys.
///
/// A run may start at every [`RUN`]th key of the stream, and does while the key's hash is under a
/// cutoff; when twice the wanted number of runs has opened, the cutoff drops to the median start
/// hash and the half above it goes. The hash decides which of the aligned positions start a run,
/// not where in the corpus they fall, so the draw is the same over the sorted keys [`plan`] holds
/// and the merged stream [`plan_file`] walks, the same on every run over the same keys, and its
/// resident cost is bounded by twice the sample whatever the corpus. Aligned starts never overlap,
/// so at most one run is filling at a time.
///
/// It is the draw for what a blob pays per stored entry and per block, which is a property of the
/// neighbours a key is coded against: see [`RUN`].
struct RunSample<K> {
    runs: usize,
    len: usize,
    cutoff: u64,
    /// Keys offered so far, which is the position of the next one.
    at: usize,
    /// The start hash of the run the next kept key opens, decided by [`wanted`](Self::wanted).
    opens: Option<u64>,
    /// Every run opened and not yet dropped: its start's hash, and the keys it holds.
    kept: Vec<(u64, Vec<K>)>,
    /// The run still filling, as an index into `kept`.
    open: Option<usize>,
}

impl<K: AsRef<str> + Ord> RunSample<K> {
    fn new(want: usize) -> Self {
        let len = RUN.min(want).max(1);
        Self {
            runs: (want / len).max(1),
            len,
            cutoff: u64::MAX,
            at: 0,
            opens: None,
            kept: Vec::new(),
            open: None,
        }
    }

    /// Whether the draw keeps the next key of the stream: as the start of a run, when its position
    /// is a multiple of the run length and its hash is under the cutoff, or as the next key of the
    /// run still filling. Every key is offered, in order, and one this says `true` to is then
    /// handed to [`keep`](Self::keep) — so a caller that has to copy a key to hand it over copies
    /// only the ones that survive.
    fn wanted(&mut self, key: &str) -> bool {
        let at = self.at;
        self.at += 1;
        if at % self.len == 0 {
            self.open = None;
            let hash = crate::blob::hash_bytes(key.as_bytes());
            self.opens = (hash <= self.cutoff).then_some(hash);
            return self.opens.is_some();
        }
        self.open.is_some()
    }

    fn keep(&mut self, key: K) {
        if let Some(hash) = self.opens.take() {
            self.kept.push((hash, vec![key]));
            self.open = Some(self.kept.len() - 1);
            if self.kept.len() >= 2 * self.runs {
                self.trim();
            }
        } else if let Some(i) = self.open {
            self.kept[i].1.push(key);
        }
    }

    fn push(&mut self, key: K) {
        if self.wanted(key.as_ref()) {
            self.keep(key);
        }
    }

    fn trim(&mut self) {
        if self.kept.len() <= self.runs {
            return;
        }
        let filling = self.open.map(|i| self.kept[i].0);
        self.kept
            .select_nth_unstable_by_key(self.runs - 1, |(h, _)| *h);
        self.kept.truncate(self.runs);
        self.cutoff = self.kept.iter().map(|(h, _)| *h).max().unwrap_or(u64::MAX);
        // The selection moved the runs about, and may have dropped the one still filling.
        self.open = filling.and_then(|h| self.kept.iter().position(|(k, _)| *k == h));
    }

    /// The sample, ascending — the order every model constant is read in.
    fn finish(mut self) -> Vec<K> {
        self.trim();
        let mut keys: Vec<K> = self.kept.into_iter().flat_map(|(_, run)| run).collect();
        keys.sort_unstable();
        keys
    }
}

/// What one build of the sample at one [`DICT_BLOCKS`] entry measures. Every rate moves with the
/// block -- a block of 32 restarts eight times as often as one of 256 and carries eight times the
/// per-block arrays -- so each priced block gets its own build rather than the default block's
/// numbers stretched over it.
struct DictFit {
    block: usize,
    rates: Rates,
}

/// What one build of the sample measures that no statistic gives.
struct Sample {
    /// One fit per priced block.
    dict: [DictFit; DICT_BLOCKS.len()],
    /// The corpus's [`Shape`] as a `DictIndex` spells its keys: the shape itself, or where a build
    /// would take a character code, the shape under it.
    dict_shape: Shape,
    /// What that code's table weighs, zero without one.
    chars: f64,
    /// What an fst spends a key at the run draw's density.
    ///
    /// Read off [`RunSample`] and carried across flat, because how far an fst minimises below its
    /// trie is a property of *density* and the run draw is the only one that keeps it: the keys of
    /// a run are the corpus's own neighbours, so the right-languages that merge in the corpus merge
    /// in the draw. Scored against the built blob on 19 corpora of a quarter million to nineteen
    /// million keys this lands within **1.1 % median, 6.7 % at the 90th percentile and 11.7 % at
    /// worst**, against 5.2 / 36.0 / 53.1 for the same model read off [`HashSample`] and scaled by
    /// trie nodes, which is what this replaced.
    ///
    /// Fitting the slope between two run-draw sizes and extrapolating is worse than carrying it
    /// flat — 7.0 % median, 26.2 % at the 90th percentile — for the reason a slope always fails
    /// here: between a quarter and a whole sample the curve has not yet turned.
    ///
    /// A corpus of a *dense* id space is where it still breaks. Ten million decimal ids merge to
    /// 356 bytes whole and the draw cannot see it, so the estimate reads about a hundredfold high.
    /// The ranking survives — the number is still two orders under every other index's, so a plan
    /// picks the fst — but the byte count does not, and [`Plan::thin`] is what says so.
    fst_per_key: f64,
    /// `(fixed, per key)` for each hash index, fitted over two sample sizes: their blobs are a
    /// header, a perfect hash and a per-key table, and at a hundred thousand keys the header is
    /// still 0.06 bytes a key -- scaling one measurement by `n` carries that constant with it and
    /// reads 34 % high.
    #[cfg(feature = "mph")]
    hash_fit: [(f64, f64); 3],
}

impl Sample {
    /// The constants the two draws measure for the corpus `shape` describes.
    fn of(spread: &[&str], runs: &[&str], shape: &Shape) -> Result<Self, IndexError> {
        let fst_per_key =
            StringIndex::build(runs)?.serialized_len() as f64 / runs.len().max(1) as f64;
        let (dict, dict_shape, chars) = match code_for(spread, runs, shape) {
            None => (Self::dict_fits(spread, runs, shape.n)?, shape.clone(), 0.0),
            Some(code) => {
                let spell = |keys: &[&str]| {
                    dict_index::encode_all(&code, keys)
                        .expect("a code chosen over both draws spells both")
                };
                let (spread_arena, spread_ends) = spell(spread);
                let (runs_arena, runs_ends) = spell(runs);
                let (coded_spread, coded_runs) = (
                    views(&spread_arena, &spread_ends),
                    views(&runs_arena, &runs_ends),
                );
                let dict_shape = shape.spelt(
                    [&Shape::of(spread), &Shape::of(&coded_spread)],
                    [&Shape::of(runs), &Shape::of(&coded_runs)],
                );
                (
                    Self::dict_fits(&coded_spread, &coded_runs, shape.n)?,
                    dict_shape,
                    code.serialized_len() as f64,
                )
            }
        };
        Ok(Self {
            dict,
            dict_shape,
            chars,
            fst_per_key,
            #[cfg(feature = "mph")]
            hash_fit: hash_fits(spread)?,
        })
    }

    /// One build of the draws a priced block, all three under the one vocabulary the corpus buys:
    /// what a block costs is the block's, what a suffix compresses to is the corpus's.
    fn dict_fits<K: AsKey + Sync>(
        spread: &[K],
        runs: &[K],
        corpus: usize,
    ) -> Result<[DictFit; DICT_BLOCKS.len()], IndexError> {
        let draws = Draws::of(spread, runs, corpus);
        let (spread_blob, runs_blobs) = DictIndex::plan_builds(spread, runs, &DICT_BLOCKS, corpus)?;
        let mut dict = Vec::with_capacity(DICT_BLOCKS.len());
        for (block, runs_blob) in DICT_BLOCKS.into_iter().zip(&runs_blobs) {
            dict.push(DictFit::of(&draws, block, &spread_blob, runs_blob));
        }
        Ok(dict
            .try_into()
            .unwrap_or_else(|_| unreachable!("one fit a priced block")))
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

/// One build's rates: the suffix ratio, what a stored entry's header costs, the packed arrays a
/// block, a shard's vocabulary, and the dictionary a key of the corpus.
///
/// The blob is walked rather than measured by section, because the two rates that matter most share
/// one: `BDX3` writes a run's coded `(lcp, len)` pairs and its coded suffixes into the same block
/// data, and they neither cost the same nor move together with the keys.
///
/// Two draws feed one set of rates, each read off the build that can see it. The vocabulary is a
/// property of the whole key set, so `ratio` and `phrases` come from the uniform [`HashSample`],
/// built as a sample of `corpus` keys rather than as an index of its own: its phrases mined at the
/// corpus's economy, so the dictionary it holds is the one the plan prices, per key of the corpus,
/// and its suffixes coded under that vocabulary. What a stored entry and a block cost is a property
/// of the neighbours a key is coded against, so `header`, `per_block` and `table` come from the
/// [`RunSample`], which keeps its own symbol tables — a table is a neighbourhood's — and is coded
/// under the same dictionary.
struct Rates {
    /// Compressed suffix bytes over raw suffix bytes. Read off a build of the sample as an index of
    /// its own it was 0.71 where the million-key blob spends 0.53 on article titles: the miner buys
    /// a phrase against what it saves over the whole blob, and a hundred thousand keys afford a
    /// tenth of the dictionary a million do. No slope fitted below the sample reaches the corpus
    /// either -- the vocabulary is flat below a hundred thousand keys and falls faster the further
    /// it goes -- so the sample is built with the corpus's economics instead, and the rate is
    /// carried flat.
    ratio: f64,
    /// Bytes a stored entry's `(lcp, len)` pair costs, escapes included. A pair is coded against
    /// its shard's own distribution, which a run of consecutive keys carries unchanged; a uniform
    /// draw of the keys put its neighbours further apart and read 0.51 where a million decimal ids
    /// spend 0.34.
    header: f64,
    /// The packed per-block arrays, per block.
    per_block: f64,
    /// One shard's symbol table and header code, serialised.
    table: f64,
    /// The phrase dictionary's bytes per key of the corpus: the dictionary the sample bought for
    /// the corpus, over the keys that will name it.
    phrases: f64,
}

impl Rates {
    /// The rates of both draws, combined — read off the two blobs
    /// [`plan_builds`](DictIndex::plan_builds) wrote, and building nothing itself.
    fn of(draws: &Draws, spread: &DictIndex, runs: &DictIndex) -> Self {
        let spread = Self::read(spread, &draws.spread_shape, draws.corpus);
        let runs = Self::read(runs, &draws.runs_shape, draws.runs_shape.n);
        Self {
            ratio: spread.ratio,
            phrases: spread.phrases,
            ..runs
        }
    }

    /// Every rate of one build, its dictionary priced over `corpus` keys.
    fn read(dict: &DictIndex, shape: &Shape, corpus: usize) -> Self {
        let keys = shape.n;
        let s = dict.sections();
        let blocks = keys.div_ceil(dict.block()) as f64;
        let (entries, restarts) = ((s.entries + s.restarts) as f64, s.restarts as f64);
        let raw = suffix_bytes(shape, entries, restarts);
        Self {
            ratio: if raw > 0.0 {
                (s.entry_codes + s.restart_codes) as f64 / raw
            } else {
                1.0
            },
            header: if entries > 0.0 {
                (s.entry_headers + s.entry_wide + s.restart_headers + s.restart_wide) as f64
                    / entries
            } else {
                0.0
            },
            per_block: (s.head_ends + s.block_offsets + s.micro_offsets) as f64 / blocks,
            table: (s.tables + s.header_codes) as f64 / shards_for(keys, dict.block()) as f64,
            phrases: s.phrases as f64 / corpus.max(1) as f64,
        }
    }
}

impl DictFit {
    fn of(draws: &Draws, block: usize, spread: &DictIndex, runs: &DictIndex) -> Self {
        Self {
            block,
            rates: Rates::of(draws, spread, runs),
        }
    }
}

/// The two draws of a corpus of `corpus` keys, each ascending and distinct, with their shapes.
struct Draws {
    /// The shape of the uniform draw, by [`HashSample`].
    spread_shape: Shape,
    /// The shape of the draw of consecutive runs, by [`RunSample`].
    runs_shape: Shape,
    corpus: usize,
}

impl Draws {
    fn of<K: AsKey>(spread: &[K], runs: &[K], corpus: usize) -> Self {
        Self {
            spread_shape: Shape::of(spread),
            runs_shape: Shape::of(runs),
            corpus,
        }
    }
}

/// The character code a build of the whole corpus would spell its keys in, chosen off both draws:
/// the corpus's own byte counts say whether one is worth counting and weigh what it saves against
/// its table, and the draws say which characters it holds and how often. Both draws are counted,
/// so that the code spells every key either holds.
fn code_for(spread: &[&str], runs: &[&str], shape: &Shape) -> Option<CharCode> {
    if !charcode::worth_counting(shape.len, shape.high) {
        return None;
    }
    let mut tally = Tally::new();
    for key in spread.iter().chain(runs) {
        tally.add(key);
    }
    tally.choose_for(shape.len)
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
/// The candidates and their sizes are the same; only the order, and so [`Plan::best`], differ —
/// except that a [`Workload`]'s operations add to the needs, since each is a question the index has
/// to answer. [`Objective::Memory`] is exactly [`plan`].
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
    let needs = objective.asks(needs);
    let mut sorted: Vec<&str> = keys.iter().map(AsRef::as_ref).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let shape = Shape::of(&sorted);
    let candidates = candidates(needs);
    let mut estimates = if sorted.len() <= sample_size() {
        weigh(&sorted, &candidates)?
    } else {
        let (spread, runs) = draws(&sorted, sample_size());
        model(&shape, &Sample::of(&spread, &runs, &shape)?, &candidates)
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

/// [`plan_for`] over a keys file, without ever holding the corpus.
///
/// One key a line, UTF-8, in any order; **an empty line is not a key**, since a file that ends in a
/// newline is the common case and an empty key is not. Duplicates are counted once, as they are in
/// [`plan`].
///
/// The file is sorted externally — runs in memory, spilled beside it, merged back — and the merge
/// is where `n`, the mean key length and both shared-prefix distances are counted, exactly and not
/// from a sample. What a sample is still needed for is the three numbers no statistic gives, and
/// those come from two draws of 100 000 keys, both decided **by hash** in the one pass over the
/// merge: a uniform draw of the distinct keys, and runs of 4 096 consecutive keys each started by
/// the hash of its first — the draws [`plan`] makes over the sorted keys it holds. So the two agree
/// on the shape to the byte and read their constants off the same samples.
///
/// The resident cost is the run budget plus the sample, not the corpus: on a 925 MB, 7 343 721-line
/// path list this peaks at a third of a gigabyte where [`plan`] on the loaded keys peaks at 1.4.
/// The runs are written to a directory beside `path` and removed however the call ends, so the
/// file's own directory must be writable.
///
/// ```no_run
/// # use lexindex::{plan_file, Needs, Objective};
/// let p = plan_file("keys.txt", Needs::default().prefix(), Objective::Memory)?;
/// println!("{p}");
/// # Ok::<(), lexindex::IndexError>(())
/// ```
pub fn plan_file(
    path: impl AsRef<std::path::Path>,
    needs: Needs,
    objective: Objective,
) -> Result<Plan, IndexError> {
    plan_file_with(path.as_ref(), needs, objective, crate::extsort::RUN_BYTES)
}

/// [`plan_file`] at a chosen run budget, so a test can reach the merge without a corpus that
/// spills a quarter of a gigabyte.
fn plan_file_with(
    path: &std::path::Path,
    needs: Needs,
    objective: Objective,
    run_bytes: usize,
) -> Result<Plan, IndexError> {
    use crate::extsort::{Replay, Run, Runs};
    let needs = objective.asks(needs);
    let mut run = Run::with_budget(run_bytes);
    let mut runs = Runs::beside(path);
    read_lines(path, &mut |key| {
        if !run.fits(key) {
            if run.is_empty() {
                return Err(IndexError::Format("plan: a key longer than the run budget"));
            }
            runs.spill(run.sorted())?;
            run.clear();
        }
        run.push(key);
        Ok(())
    })?;

    let want = sample_size();
    let mut walk = ShapeWalk::default();
    let mut spread: HashSample<String> = HashSample::new(want);
    let mut adjacent: RunSample<String> = RunSample::new(want);
    // A corpus no larger than the sample is weighed rather than modelled, exactly as `plan` weighs
    // one, so the two answer a small file identically. Kept only while it stays small enough to be
    // the sample itself.
    let mut all: Option<Vec<String>> = Some(Vec::new());
    let mut each = |key: &str| {
        walk.push(key.as_bytes());
        if let Some(hash) = spread.wanted(key) {
            spread.keep(hash, key.to_owned());
        }
        if adjacent.wanted(key) {
            adjacent.keep(key.to_owned());
        }
        if let Some(keys) = &mut all {
            keys.push(key.to_owned());
            if keys.len() > want {
                all = None;
            }
        }
        Ok(())
    };
    if runs.is_empty() {
        (&mut run).each(&mut each)?;
    } else {
        if !run.is_empty() {
            runs.spill(run.sorted())?;
        }
        // The run's arena is the plan's largest allocation and nothing reads it again.
        drop(run);
        (&mut runs).each(&mut each)?;
    }

    let shape = walk.finish();
    let candidates = candidates(needs);
    let mut estimates = match all {
        Some(keys) => {
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            weigh(&keys, &candidates)?
        }
        None => {
            let (spread, adjacent) = (spread.finish(), adjacent.finish());
            let spread: Vec<&str> = spread.iter().map(String::as_str).collect();
            let adjacent: Vec<&str> = adjacent.iter().map(String::as_str).collect();
            model(
                &shape,
                &Sample::of(&spread, &adjacent, &shape)?,
                &candidates,
            )
        }
    };
    rank(&mut estimates, objective, shape.n, shape.mean_len());
    Ok(Plan {
        keys: shape.n,
        mean_len: shape.mean_len(),
        mean_lcp: shape.mean_lcp1(),
        estimates,
        needs,
        objective,
    })
}

/// Every non-empty line of a keys file, in order, with the line number in any UTF-8 error — the
/// number is the whole difference between an error a caller can act on and one they cannot.
fn read_lines(
    path: &std::path::Path,
    each: &mut dyn FnMut(&str) -> Result<(), IndexError>,
) -> Result<(), IndexError> {
    use std::io::BufRead;
    let open = std::fs::File::open(path)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let mut reader = std::io::BufReader::new(open);
    let mut line = String::new();
    let mut at = 0usize;
    loop {
        line.clear();
        at += 1;
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Err(IndexError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}:{at}: not UTF-8", path.display()),
                )));
            }
            Err(e) => return Err(IndexError::Io(e)),
        }
        let key = line.strip_suffix('\n').unwrap_or(&line);
        let key = key.strip_suffix('\r').unwrap_or(key);
        if !key.is_empty() {
            each(key)?;
        }
    }
}

/// Put the candidates in the objective's order, best first. Ties, and every order, break on bytes,
/// so two runs over the same keys rank them the same way.
fn rank(estimates: &mut [Estimate], objective: Objective, keys: usize, mean_len: f64) {
    let nanos = |e: &Estimate| nanos_of(objective, e, keys, mean_len);
    match objective {
        Objective::Memory => estimates.sort_by_key(|e| e.bytes),
        Objective::Latency | Objective::Workload(_) => {
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

/// The two draws of the sorted corpus, at most `want` keys each and in order: the uniform one by
/// [`HashSample`], the runs by [`RunSample`].
///
/// Not `step_by(n / want)`, which was neither a sample nor `want` keys: integer division gives a
/// step of one at 150 001 keys, so "the sample" was the whole corpus and every constant was read
/// off a build the size of the real index.
fn draws<'a>(sorted: &[&'a str], want: usize) -> (Vec<&'a str>, Vec<&'a str>) {
    if sorted.len() <= want || want == 0 {
        return (sorted.to_vec(), sorted.to_vec());
    }
    let mut spread = HashSample::new(want);
    let mut runs = RunSample::new(want);
    for key in sorted {
        spread.push(*key);
        runs.push(*key);
    }
    (spread.finish(), runs.finish())
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
                Kind::String => (sample.fst_per_key * shape.n as f64, None),
                Kind::Dict => {
                    let block = at.unwrap_or(dict_index::DEFAULT_BLOCK);
                    let fit = sample.dict_fit(block);
                    (
                        dict_bytes(&sample.dict_shape, fit, block) + sample.chars,
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

/// The format as the model: a header, the symbol tables, the phrase dictionary, one head a block
/// stored whole, one byte an entry, the suffixes the table squeezed, and the packed arrays — over
/// the keys as the blob spells them, so without the character code's table, which is the caller's.
fn dict_bytes(shape: &Shape, fit: &DictFit, block: usize) -> f64 {
    let n = shape.n as f64;
    let blocks = shape.n.div_ceil(block) as f64;
    let entries = n - blocks;
    let restarts = blocks * (block.div_ceil(dict_index::micro_for(block)) - 1) as f64;
    let r = &fit.rates;
    let tables = shards_for(shape.n, block) as f64 * r.table;
    dict_index::HEADER as f64
        + tables
        + r.phrases * n
        + blocks * shape.mean_len()
        + r.header * entries
        + r.ratio * suffix_bytes(shape, entries, restarts)
        + r.per_block * blocks
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

    /// [`corpus`] in Chinese: the prefixes in ideographs, and tails of four out of four hundred of
    /// them — a corpus a `DictIndex` spells in a character code.
    fn ideographs(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                h ^= h >> 29;
                h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                let tail: String = (0..4)
                    .map(|k| char::from_u32(0x4E00 + ((h >> (16 + 9 * k)) % 400) as u32).unwrap())
                    .collect();
                format!(
                    "{}{tail}",
                    ["北", "北京", "上", "上海", "中", "中国", "广"][i % 7]
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
        let at = |block: usize| {
            let ops = ops_of(Kind::Dict, Some(block));
            let (s, len) = ops.at(300_000, SAMPLE, 10.0);
            ops.mixed.nanos(s, len)
        };
        // Below and above the measured ends it is clamped, not extrapolated.
        assert_eq!(at(1), at(32));
        assert_eq!(at(1024), at(4096));
        // Between them it moves, and at 100 000 keys a bigger block costs more.
        assert!(at(32) < at(64) && at(64) < at(128));
        assert!(at(256) < at(512) && at(512) < at(1024));
    }

    /// Every constant the fit produced, at every operation: never below zero, never cheaper for a
    /// larger blob or longer keys, and never read past the edge of its evidence — a blob under its
    /// smallest fitted size, keys shorter than any corpus's and a corpus under the sample are all
    /// priced at those edges.
    #[test]
    fn the_model_grows_with_the_blob_and_the_keys_and_stops_at_its_evidence() {
        let kinds = [
            (Kind::Closed, None),
            (Kind::Compact, None),
            (Kind::Perfect, None),
            (Kind::String, None),
            (Kind::Dict, Some(32)),
            (Kind::Dict, Some(100)),
            (Kind::Dict, Some(1024)),
        ];
        for (kind, block) in kinds {
            let ops = ops_of(kind, block);
            assert!(ops.floor >= 0.0, "{kind:?} {block:?}");
            let costs = [ops.mixed, ops.hit, ops.miss, ops.batch16, ops.batch1024]
                .into_iter()
                .chain(
                    [ops.key, ops.prefix, ops.common_prefix, ops.longest_prefix]
                        .into_iter()
                        .flatten(),
                );
            for cost in costs {
                let nanos = |bytes: u64, keys: usize, len: f64| {
                    let (s, len) = ops.at(bytes, keys, len);
                    cost.nanos(s, len)
                };
                let mut last = 0.0;
                for shift in 10..40 {
                    for len in [1.0, 10.0, 40.0, 200.0] {
                        assert!(
                            nanos(1 << shift, SAMPLE * 100, len) > 0.0,
                            "{kind:?} {block:?}"
                        );
                    }
                    let now = nanos(1 << shift, SAMPLE * 100, 20.0);
                    assert!(now >= last, "{kind:?} {block:?}");
                    assert!(
                        nanos(1 << shift, SAMPLE * 100, 40.0) >= now,
                        "{kind:?} {block:?}"
                    );
                    last = now;
                }
                assert_eq!(nanos(1, SAMPLE, 1.0), nanos(0, SAMPLE, LEN_FLOOR));
                assert_eq!(
                    nanos(1 << 20, SAMPLE / 10, 20.0),
                    nanos(10 << 20, SAMPLE, 20.0)
                );
            }
            // `id(key)` itself gets dearer as the blob grows, which is what `Latency` ranks by.
            let (near, len) = ops.at(1 << 20, SAMPLE, 20.0);
            let (far, _) = ops.at(1 << 30, SAMPLE, 20.0);
            assert!(
                ops.mixed.nanos(near, len) < ops.mixed.nanos(far, len),
                "{kind:?} {block:?}"
            );
        }
    }

    /// The model prices an operation for every kind that answers it and for no kind that does not,
    /// so what decides a workload's candidates is its needs and never a missing constant.
    #[test]
    fn every_kind_prices_exactly_the_operations_it_answers() {
        for kind in [
            Kind::Compact,
            Kind::Closed,
            Kind::Perfect,
            Kind::String,
            Kind::Dict,
        ] {
            let ops = ops_of(kind, None);
            let reverse = kind.answers(Needs::default().reverse());
            let prefix = kind.answers(Needs::default().prefix());
            assert_eq!(ops.key.is_some(), reverse, "{kind:?}");
            assert_eq!(ops.prefix.is_some(), prefix, "{kind:?}");
            assert_eq!(ops.common_prefix.is_some(), prefix, "{kind:?}");
            assert_eq!(ops.longest_prefix.is_some(), prefix, "{kind:?}");
        }
    }

    /// A batch is priced between the sizes that were measured and at the last of them past it, and
    /// saves hits and misses the share it saved the mixed lookups it was measured on.
    #[test]
    fn a_batch_is_priced_between_the_sizes_measured() {
        let ops = ops_of(Kind::Perfect, None);
        let (s, len) = ops.at(1 << 30, SAMPLE * 100, 12.0);
        let at = |size: u32| Workload::default().hits(1).batch(size).nanos(&ops, s, len);
        let hit = ops.hit.nanos(s, len);
        let share = |cost: Cost| cost.nanos(s, len) / ops.mixed.nanos(s, len);
        let close = |x: f64, y: f64| (x - y).abs() <= 1e-9 * y.abs();
        assert!(close(at(0), hit) && close(at(1), hit));
        assert!(close(at(16), hit * share(ops.batch16)));
        assert!(close(at(1_024), hit * share(ops.batch1024)));
        assert!(close(at(u32::MAX), at(1_024)));
        let (lo, hi) = (at(16).min(at(1_024)), at(16).max(at(1_024)));
        assert!(
            (lo..=hi).contains(&at(128)),
            "{} not in {lo}..={hi}",
            at(128)
        );
        assert!(at(16) < at(1), "a key of a batch costs less than a call");
    }

    /// A workload is ranked by what it asks: among the ordered indexes, the fst for a workload of
    /// `common_prefix` and the dictionary for one of prefix counts, and for batches of lookups a
    /// hash index ahead of both.
    #[test]
    fn a_workload_ranks_by_what_it_asks() {
        let keys = corpus(2_000);
        let best = |asked: Workload| {
            plan_for(&keys, Needs::default(), Objective::Workload(asked))
                .unwrap()
                .best()
                .kind
        };
        assert_eq!(
            best(Workload::default().hits(1).common_prefix(9)),
            Kind::String
        );
        assert_eq!(best(Workload::default().hits(1).prefix(9)), Kind::Dict);
        if cfg!(feature = "mph") {
            let kind = best(Workload::default().hits(1).batch(1_024));
            assert!(matches!(kind, Kind::Closed | Kind::Compact), "{kind:?}");
        }
    }

    /// An operation is a question the index has to answer, so a workload that asks one narrows the
    /// candidates exactly as the need would.
    #[test]
    fn a_workload_asks_for_what_its_operations_need() {
        let keys = corpus(500);
        let kinds = |p: Plan| {
            let mut kinds: Vec<_> = p
                .estimates()
                .iter()
                .map(|e| (e.kind.name(), e.block))
                .collect();
            kinds.sort_unstable();
            kinds
        };
        for (asked, needs) in [
            (Workload::default().reverse(1), Needs::default().reverse()),
            (Workload::default().prefix(1), Needs::default().prefix()),
            (
                Workload::default().common_prefix(1),
                Needs::default().prefix(),
            ),
            (
                Workload::default().longest_prefix(1),
                Needs::default().prefix(),
            ),
        ] {
            let got = plan_for(&keys, Needs::default(), Objective::Workload(asked)).unwrap();
            assert_eq!(kinds(got), kinds(plan(&keys, needs).unwrap()), "{asked:?}");
        }
    }

    /// With nothing asked, a workload is an `id(key)` over half members and half strangers: the
    /// ranking, the nanoseconds and the ladder of `Latency`.
    #[test]
    fn a_workload_that_asks_nothing_is_latency() {
        let keys = corpus(2_000);
        let needs = Needs::default().exact();
        let latency = plan_for(&keys, needs, Objective::Latency).unwrap();
        let empty = plan_for(&keys, needs, Objective::Workload(Workload::default())).unwrap();
        assert_eq!(latency.estimates(), empty.estimates());
        for e in latency.estimates() {
            assert_eq!(latency.nanos(e), empty.nanos(e));
        }
        assert_eq!(latency.to_string(), empty.to_string());
    }

    #[test]
    fn the_ladder_names_the_workload_it_ranked_by() {
        let ladder = |asked: Workload| {
            plan_for(&corpus(500), Needs::default(), Objective::Workload(asked))
                .unwrap()
                .to_string()
        };
        let text = ladder(Workload::default().hits(9).misses(1).batch(64));
        let first = text.lines().next().unwrap();
        assert!(
            first.ends_with("ranked by workload (hits 9, misses 1, batch 64)"),
            "{text}"
        );
        assert!(text.contains("not a measurement of yours"), "{text}");
        let text = ladder(Workload::default().batch(64));
        let first = text.lines().next().unwrap();
        assert!(first.ends_with("ranked by latency (batch 64)"), "{text}");
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

    /// A dense id space is the corpus an fst folds away almost entirely, and the one the model
    /// used to miss by four orders of magnitude: read off a hash draw and scaled by trie nodes, the
    /// estimate put a `StringIndex` over a million decimal ids at 2.28 bytes a key where the blob
    /// weighs 0.0003, and the ladder handed the caller a `DictIndex` three thousand times larger
    /// than what it was asking for. The draw is what fixes it -- a hash draw of a dense space is
    /// not dense, so the merges never happen in the sample -- and the ranking is what this guards.
    #[test]
    fn a_dense_id_space_ranks_the_fst_first() {
        let keys: Vec<String> = (0..20_000u32).map(|i| i.to_string()).collect();
        let plan = with_sample(2_000, || plan(&keys, Needs::default()).unwrap());
        assert!(plan.estimates().iter().all(|e| !e.measured));
        assert_eq!(
            plan.best().kind,
            Kind::String,
            "a dense id space is an fst's corpus, ranked {:?}",
            plan.estimates()
                .iter()
                .map(|e| (e.kind, e.bytes))
                .collect::<Vec<_>>()
        );
        let truth = StringIndex::build(&keys).unwrap().serialized_len() as u64;
        let fst = plan.best().bytes;
        assert!(
            fst < DictIndex::build(&keys).unwrap().serialized_len() as u64,
            "the estimate has to stay under the index it displaced: {fst} against {truth} true"
        );
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
    /// point is near the build it predicts. Near, not exact — the plan is a fit carried over an
    /// e-fold it did not see, and 5 % is well inside what it claims on real corpora. What the
    /// bound is here for is a term dropped or counted twice, which moves a point much further.
    #[test]
    fn the_model_prices_each_block_within_five_per_cent() {
        let keys = corpus(60_000);
        let plan = with_sample(6_000, || {
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
                err.abs() < 0.05,
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

    /// A corpus a build spells in a character code is priced in it: the draws spelt as the corpus
    /// would be, and the shape scaled by what the spelling did to them. This one reads 1.7–2.3 %
    /// low that way, and 7.5–11.0 % high priced in UTF-8.
    #[test]
    fn the_model_prices_a_coded_corpus_near_its_build() {
        let keys = ideographs(60_000);
        let blob = DictIndex::build(&keys).unwrap().to_bytes();
        assert_eq!(&blob[..4], b"BDX4", "a corpus a code pays on");
        let plan = with_sample(6_000, || {
            plan_for(&keys, Needs::default().ordered(), Objective::Memory).unwrap()
        });
        for e in plan.estimates().iter().filter(|e| e.kind == Kind::Dict) {
            assert!(!e.measured);
            let truth = DictIndex::build_with_block(&keys, e.block.unwrap())
                .unwrap()
                .serialized_len() as f64;
            let err = (e.bytes as f64 - truth) / truth;
            assert!(
                err.abs() < 0.05,
                "block {:?}: {} against {truth} ({:+.1} %)",
                e.block,
                e.bytes,
                100.0 * err
            );
        }
    }

    /// The draw takes the size it asked for at any corpus size. A `step_by(n / want)` gives a step
    /// of one just past `want` -- 150 001 keys would "sample" all 150 001 of them -- and the
    /// fractional stride is what fixes it.
    #[test]
    fn the_sample_is_runs_of_consecutive_keys() {
        let keys = corpus(150_001);
        let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 150_001, "the corpus is distinct");

        let (spread, drawn) = draws(&sorted, SAMPLE);
        assert_eq!(
            spread.len(),
            SAMPLE,
            "the uniform draw is the size it asked for"
        );
        assert!(spread.windows(2).all(|w| w[0] < w[1]), "in order, distinct");
        assert!(drawn.windows(2).all(|w| w[0] < w[1]), "in order, distinct");
        // `SAMPLE / RUN` runs, each starting at a multiple of `RUN` and holding the `RUN` keys from
        // there -- fewer only for a run that starts within `RUN` of the corpus's end.
        let starts: Vec<usize> = drawn
            .iter()
            .map(|k| sorted.binary_search(k).unwrap())
            .filter(|at| at % RUN == 0)
            .collect();
        assert_eq!(starts.len(), SAMPLE / RUN);
        let held: usize = starts.iter().map(|&s| RUN.min(sorted.len() - s)).sum();
        assert_eq!(drawn.len(), held);
        for (i, k) in drawn.iter().enumerate() {
            let at = sorted.binary_search(k).unwrap();
            if (at + 1) % RUN != 0 && at + 1 < sorted.len() {
                assert_eq!(drawn[i + 1], sorted[at + 1], "a run is consecutive keys");
            }
        }
        // Same keys, same draws: a plan over one corpus is reproducible.
        assert_eq!((spread.clone(), drawn.clone()), draws(&sorted, SAMPLE));
        // And the phase is the corpus's, so two corpora of the same size do not draw alike.
        let other = corpus(150_001)
            .iter()
            .map(|k| format!("z{k}"))
            .collect::<Vec<_>>();
        let mut theirs: Vec<&str> = other.iter().map(String::as_str).collect();
        theirs.sort_unstable();
        let starts_of = |ks: &[&str], drawn: &[&str]| -> Vec<usize> {
            drawn
                .iter()
                .map(|k| ks.binary_search(k).unwrap())
                .filter(|at| at % RUN == 0)
                .collect()
        };
        assert_ne!(
            starts_of(&sorted, &drawn),
            starts_of(&theirs, &draws(&theirs, SAMPLE).1)
        );

        // A corpus no larger than the draw is the draw.
        assert_eq!(draws(&sorted[..SAMPLE], SAMPLE).1.len(), SAMPLE);
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

    fn keys_file(name: &str, text: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lexindex-plan-file-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn beside(path: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// A file no larger than the sample is weighed, and weighed exactly as `plan` weighs the same
    /// keys in memory: same candidates, same bytes, same order.
    #[test]
    fn a_small_file_plans_exactly_as_the_loaded_corpus_does() {
        let keys = corpus(1_500);
        let text = keys.join("\n") + "\n";
        let path = keys_file("small.txt", &text);
        for objective in [
            Objective::Memory,
            Objective::Latency,
            Objective::Balanced,
            Objective::Workload(Workload::default().hits(3).longest_prefix(1)),
        ] {
            let needs = Needs::default().reverse().ordered();
            let want = plan_for(&keys, needs, objective).unwrap();
            let got = plan_file(&path, needs, objective).unwrap();
            assert_eq!(got, want, "{objective:?}");
            assert!(got.estimates().iter().all(|e| e.measured));
        }
        assert_eq!(beside(&path), vec!["small.txt".to_string()], "no runs left");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Past the sample the two plans are the *same* plan: the shape is counted during the merge
    /// rather than sampled, and the sample is drawn by hash, which does not depend on whether the
    /// keys arrived as a slice or as a merge of spilled runs.
    #[test]
    fn a_merged_file_plans_exactly_as_the_loaded_corpus_does() {
        // The second corpus is one a build spells in a character code, which the plan decides on
        // counts the file's merge makes and the loaded keys' walk makes alike.
        for keys in [corpus(20_000), ideographs(20_000)] {
            let path = keys_file("merged.txt", &(keys.join("\n") + "\n"));
            let needs = Needs::default().ordered();
            let (want, got) = with_sample(2_000, || {
                (
                    plan(&keys, needs).unwrap(),
                    // 8 KiB a run over 20 000 keys is a real merge: hundreds of runs, collapsed.
                    plan_file_with(&path, needs, Objective::Memory, 8 << 10).unwrap(),
                )
            });
            assert!(got.estimates().iter().all(|e| !e.measured));
            assert_eq!(
                got.shape(),
                want.shape(),
                "the shape is counted, not sampled"
            );
            assert_eq!(got, want);
            assert_eq!(
                beside(&path),
                vec!["merged.txt".to_string()],
                "no runs left"
            );
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    /// A file is lines, not keys: a blank line is the newline at the end of the last one, and a
    /// key written twice is one key.
    #[test]
    fn blank_lines_are_skipped_and_duplicates_counted_once() {
        let path = keys_file("odd.txt", "b\n\na\nb\r\n\nc\n");
        let p = plan_file(&path, Needs::default(), Objective::Memory).unwrap();
        assert_eq!(p.keys(), 3);
        assert_eq!(p, plan(&["a", "b", "c"], Needs::default()).unwrap());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// The library takes `&str`, so a line that is not UTF-8 is an error naming the line rather
    /// than a key spelled with replacement characters.
    #[test]
    fn a_line_that_is_not_utf8_names_itself() {
        let path = keys_file("bad.txt", "");
        std::fs::write(&path, b"ok\nfine\n\xff\xfe\n").unwrap();
        let err = plan_file(&path, Needs::default(), Objective::Memory).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("bad.txt:3:"), "{text}");
        assert!(text.contains("not UTF-8"), "{text}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// The draw is the hash's, not the position's, so it is the same draw every time and it is
    /// spread over the corpus rather than over its start.
    #[test]
    fn the_stream_draw_is_the_in_memory_draw() {
        let keys = corpus(50_000);
        let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        let want = 3 * RUN;
        let mut stream: RunSample<String> = RunSample::new(want);
        for k in &sorted {
            if stream.wanted(k) {
                stream.keep((*k).to_owned());
            }
        }
        let streamed = stream.finish();
        let drawn = draws(&sorted, want).1;
        assert!(
            drawn.len() <= want && drawn.len() > want - RUN,
            "{}",
            drawn.len()
        );
        assert_eq!(streamed, drawn, "plan_file draws what plan draws");
        let starts: Vec<usize> = drawn
            .iter()
            .map(|k| sorted.binary_search(k).unwrap())
            .filter(|at| at % RUN == 0)
            .collect();
        assert_eq!(starts.len(), 3);
    }

    #[test]
    fn the_hash_sample_is_uniform_and_reproducible() {
        let keys = corpus(50_000);
        let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        let draw = || {
            let mut s = HashSample::new(1_000);
            for k in &sorted {
                s.push(k);
            }
            s.finish()
        };
        let first = draw();
        assert_eq!(first.len(), 1_000);
        assert!(first.windows(2).all(|w| w[0] < w[1]), "ascending, distinct");
        assert_eq!(first, draw(), "the same file draws the same sample");
        // Uniform: each tenth of the corpus holds about a tenth of the draw.
        let mut buckets = [0usize; 10];
        for k in &first {
            let at = sorted.binary_search(k).unwrap();
            buckets[at * 10 / sorted.len()] += 1;
        }
        assert!(
            buckets.iter().all(|&b| (40..=160).contains(&b)),
            "{buckets:?}"
        );
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
