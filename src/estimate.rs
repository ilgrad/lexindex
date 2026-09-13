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
//! of half a million to ten million keys, the [`DictIndex`] estimate lands within **1.4 % median,
//! 4.5 % at the 90th percentile and 5.1 % at worst**. [`StringIndex`] is looser — 3.5 % median,
//! 9.6 % at the 90th percentile, and 30 % on a corpus of file paths — because an fst merges equal
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
    /// Whether an index of this kind answers everything `needs` asks for.
    fn answers(self, needs: &Needs) -> bool {
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
}

impl Plan {
    /// The cheapest candidate that answers the caller's questions. There is always one:
    /// [`StringIndex`] answers every question this crate can be asked.
    pub fn best(&self) -> Estimate {
        self.estimates[0]
    }

    /// Every candidate the [`Needs`] allow, cheapest first.
    pub fn estimates(&self) -> &[Estimate] {
        &self.estimates
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

    /// Whether the two cheapest candidates are close enough that the ranking should not be
    /// trusted over a real build.
    pub fn close(&self) -> bool {
        self.estimates.len() > 1
            && self.estimates[1].bytes as f64 <= self.estimates[0].bytes as f64 * CLOSE
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
            "{} keys, mean length {:.1}, mean shared prefix {:.1}",
            self.keys, self.mean_len, self.mean_lcp
        )?;
        for e in &self.estimates {
            let mark = if e.kind == best.kind { '*' } else { ' ' };
            let block = match e.block {
                Some(b) => format!(" at block {b}"),
                None => String::new(),
            };
            writeln!(
                f,
                "{mark} {:<17} {:>12} bytes  {:5.2} B/key  {}{}",
                e.kind.name(),
                e.bytes,
                e.bytes_per_key(self.keys),
                if e.measured { "built" } else { "estimated" },
                block
            )?;
        }
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

/// What one build of the sample measures that no statistic gives.
struct Sample {
    /// Compressed suffix bytes over raw suffix bytes, from a `DictIndex` of the sample.
    ratio: f64,
    /// The packed per-block arrays, per block.
    per_block: f64,
    /// One symbol table's serialised bytes.
    table: f64,
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
        let dict = DictIndex::build_with_block(keys, dict_index::DEFAULT_BLOCK)?;
        let [tables, _heads, arrays, data] = dict.section_lens();
        let blocks = keys.len().div_ceil(dict.block()) as f64;
        let entries = (keys.len() - blocks as usize) as f64;
        let restarts = blocks * (dict.block().div_ceil(dict.micro()) - 1) as f64;
        let raw = suffix_bytes(&shape, entries, restarts);
        let fst_node =
            StringIndex::build(keys)?.serialized_len() as f64 / shape.trie_nodes().max(1.0);
        Ok(Self {
            ratio: if raw > 0.0 {
                (data as f64 - entries) / raw
            } else {
                1.0
            },
            per_block: arrays as f64 / blocks,
            table: tables as f64 / shards_for(keys.len(), dict.block()) as f64,
            fst_node,
            #[cfg(feature = "mph")]
            hash_fit: hash_fits(keys)?,
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
    let mut sorted: Vec<&str> = keys.iter().map(AsRef::as_ref).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let shape = Shape::of(&sorted);
    let kinds: Vec<Kind> = [
        Kind::Compact,
        Kind::Closed,
        Kind::Perfect,
        Kind::String,
        Kind::Dict,
    ]
    .into_iter()
    .filter(|k| k.answers(&needs) && k.available())
    .collect();

    let mut estimates = if sorted.len() <= sample_size() {
        weigh(&sorted, &kinds)?
    } else {
        let step = sorted.len() / sample_size();
        let sample: Vec<&str> = sorted.iter().step_by(step).copied().collect();
        model(&shape, &Sample::of(&sample)?, &kinds)
    };
    estimates.sort_by_key(|e| e.bytes);
    Ok(Plan {
        keys: sorted.len(),
        mean_len: shape.mean_len(),
        mean_lcp: shape.mean_lcp1(),
        estimates,
        needs,
    })
}

/// Build every candidate and report what it weighs. What a corpus no larger than the sample gets,
/// since the sample would be the corpus.
fn weigh(keys: &[&str], kinds: &[Kind]) -> Result<Vec<Estimate>, IndexError> {
    let mut out = Vec::with_capacity(kinds.len());
    for &kind in kinds {
        let (bytes, block) = match kind {
            Kind::String => (StringIndex::build(keys)?.serialized_len() as u64, None),
            Kind::Dict => (
                DictIndex::build_with_block(keys, dict_index::DEFAULT_BLOCK)?.serialized_len()
                    as u64,
                Some(dict_index::DEFAULT_BLOCK),
            ),
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
fn model(shape: &Shape, sample: &Sample, kinds: &[Kind]) -> Vec<Estimate> {
    kinds
        .iter()
        .map(|&kind| {
            let (bytes, block) = match kind {
                Kind::String => (sample.fst_node * shape.trie_nodes(), None),
                Kind::Dict => (
                    dict_bytes(shape, sample, dict_index::DEFAULT_BLOCK),
                    Some(dict_index::DEFAULT_BLOCK),
                ),
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
fn dict_bytes(shape: &Shape, sample: &Sample, block: usize) -> f64 {
    let n = shape.n as f64;
    let blocks = shape.n.div_ceil(block) as f64;
    let entries = n - blocks;
    let restarts = blocks * (block.div_ceil(dict_index::micro_for(block)) - 1) as f64;
    let data = entries + sample.ratio * suffix_bytes(shape, entries, restarts);
    let tables = shards_for(shape.n, block) as f64 * sample.table;
    dict_index::HEADER as f64
        + tables
        + blocks * shape.mean_len()
        + data
        + sample.per_block * blocks
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
                    e.kind.answers(&needs),
                    "{:?} does not answer {needs:?}",
                    e.kind
                );
            }
            let best = plan.best();
            assert!(plan.estimates().iter().all(|e| e.bytes >= best.bytes));
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
