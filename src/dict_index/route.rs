//! Where a probe goes: samples over the heads, ties, the restart words and the microblock route.

use super::*;

/// Whether the key a word was taken from may end before the word's byte `i`, most significant
/// first: the zero bytes past its last nonzero one may be padding as well as the key's own, so a
/// probe that shares them does not say how far it agrees with the key.
#[inline(always)]
fn may_end_before(w: u64, i: usize) -> bool {
    8 - w.trailing_zeros() / 8 < i as u32
}

/// A route over the heads as the caller spells them: what [`DictIndex::g`], the first head's
/// prefix and [`DictIndex::samples`] are to the coded heads.
///
/// The code keeps the order, so a decoded head orders against a query exactly as its code orders
/// against the query's, and the blocks the samples place a query among are the ones they would
/// place its code among. What routing this way buys is time: the search over the samples is a
/// dozen dependent loads, and the query's spelling — a few hundred instructions that the search
/// never reads — runs while those loads are in flight instead of before the first of them issues.
pub(super) struct RawRoute {
    g: usize,
    prefix: Box<[u8]>,
    samples: Vec<u64>,
}

/// The sample the search runs on: the eight bytes of a key at `g`, zero-padded, in byte order.
///
/// Every head shares its first `g` bytes, so ordering by this word is ordering by the head.
#[inline(always)]
pub(crate) fn sample_at(key: &[u8], g: usize) -> u64 {
    fsst::word_at(key, g).swap_bytes()
}

/// The end of the run of samples equal to `s` that starts at `lo`, the first sample not below it.
/// A second binary search over all the samples would pay their dependent loads again for a run
/// that is empty on most probes; this pays one compare for that case and gallops through a run,
/// so it is logarithmic in the run's length rather than in the samples'.
#[inline]
pub(super) fn past_equal(samples: &[u64], lo: usize, s: u64) -> usize {
    if samples.get(lo) != Some(&s) {
        return lo;
    }
    let mut hi = lo + 1;
    let mut step = 1;
    while samples.get(hi + step - 1) == Some(&s) {
        hi += step;
        step *= 2;
    }
    let end = (hi + step - 1).min(samples.len());
    hi + samples[hi..end].partition_point(|&x| x <= s)
}

/// The blocks a probe routed to sample `s` can be in: `Err(l)` for the block boundary itself, and
/// `Ok((lo, hi))` for the heads that still have to be compared with it.
///
/// A head whose sample is below the probe's is below the probe, and one whose sample is above it
/// is above — the samples are the heads' next eight bytes after the prefix every head and the
/// probe share, zero-padded. So only a run of samples equal to the probe's leaves a head to
/// compare, and where no sample equals it the boundary is the first sample above it outright.
/// Comparing the head before that run as well, which every lookup did, put one more dependent
/// head load and a `memcmp` in front of the block the samples had already chosen.
#[inline(always)]
pub(super) fn sample_range(
    samples: &[u64],
    routed: Result<u64, usize>,
) -> Result<(usize, usize), usize> {
    let s = routed?;
    let lo = samples.partition_point(|&x| x < s);
    let hi = past_equal(samples, lo, s);
    if hi == lo { Err(lo) } else { Ok((lo, hi)) }
}

/// Whether `probe` starts with `prefix`, the bytes every head shares; if not, the block boundary
/// it lands on, since it is then below every head or above every one. A probe shorter than the
/// prefix is not a key: a proper prefix of it sorts below every head, and anything else is placed
/// by the same comparison.
#[inline(always)]
fn place_prefix(prefix: &[u8], blocks: usize, probe: &[u8]) -> Result<(), usize> {
    let below = match probe.get(..prefix.len()) {
        Some(front) => match front.cmp(prefix) {
            Ordering::Equal => return Ok(()),
            order => order == Ordering::Less,
        },
        None => probe <= prefix,
    };
    Err(if below { 0 } else { blocks })
}

/// Bytes every head in `heads` shares. They are sorted, so it is what the first and the last share
/// and nothing else has to be read.
pub(super) fn common_head(heads: &[u8], ends: &Offsets, nb: usize) -> usize {
    if nb == 0 {
        return 0;
    }
    let (first, last) = (head_of(heads, ends, 0), head_of(heads, ends, nb - 1));
    lcp(first, last).min(u16::MAX as usize)
}

/// [`common_head`] over the sections as a build holds them, before they are packed.
pub(super) fn common_head_raw(heads: &[u8], ends: &[u64]) -> usize {
    let Some(&last_end) = ends.last() else {
        return 0;
    };
    let first = &heads[..ends[0] as usize];
    let start = if ends.len() == 1 {
        0
    } else {
        ends[ends.len() - 2] as usize
    };
    lcp(first, &heads[start..last_end as usize]).min(u16::MAX as usize)
}

/// The sample of every head, in block order.
pub(super) fn samples_of(heads: &[u8], ends: &[u64], g: usize) -> Vec<u64> {
    let mut at = 0usize;
    ends.iter()
        .map(|&end| {
            let head = &heads[at..end as usize];
            at = end as usize;
            sample_at(head, g)
        })
        .collect()
}

/// Block `b`'s head out of its sections. The arrays are trusted only as far as their sections
/// reach: a mapping is loaded without the walk over them, so an end past the heads, or before
/// the previous one, gives a short head rather than a panic.
#[inline(always)]
pub(super) fn head_of<'a>(heads: &'a [u8], ends: &Offsets, b: usize) -> &'a [u8] {
    let (start, end) = if b == 0 {
        (0, ends.at(0))
    } else {
        ends.pair(b - 1)
    };
    let at = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
    heads.get(at(start)..at(end)).unwrap_or_default()
}

impl DictIndex {
    /// `query` as the keys are spelled, handed to `f` with what the bytes stand for. A blob without
    /// a code hands the query over as it is. One with a code spells it into a buffer on the stack
    /// for a query of up to 128 bytes, so that a lookup allocates nothing, and onto the heap past
    /// that.
    #[inline(always)]
    pub(super) fn probe<R>(&self, query: &str, f: impl FnOnce(&[u8], Probe) -> R) -> R {
        const STACK: usize = 256;
        let Some(code) = &self.code else {
            return f(query.as_bytes(), Probe::Exact);
        };
        if 2 * query.len() <= STACK {
            let mut buf = [0u8; STACK];
            let (probe, len) = code.probe_into(query, &mut buf);
            f(&buf[..len], probe)
        } else {
            let mut buf = vec![0u8; 2 * query.len()];
            let (probe, len) = code.probe_into(query, &mut buf);
            f(&buf[..len], probe)
        }
    }

    /// [`locate`](Self::locate) for a query as the caller spells it: a query holding a character
    /// the code does not spell is no key, and its rank is where the code places it.
    ///
    /// On a blob that routes on the query's own bytes, the route comes first and the spelling
    /// after, so that the spelling runs while the search over the samples waits on its loads. A
    /// query whose code is a stand-in (`Probe::Absent`) is placed the same way: every key below
    /// the query is below the stand-in and every other key is not below it, so the boundary the
    /// raw query finds is the stand-in's, or the one before it where a head *is* the stand-in —
    /// and the rank that block's scan gives is the same.
    #[inline]
    pub(super) fn locate_query(&self, query: &str) -> (u64, bool) {
        let Some(raw) = self.raw.as_deref() else {
            return self.probe(query, |bytes, probe| match probe {
                Probe::Exact => self.locate(bytes),
                Probe::Absent => (self.locate(bytes).0, false),
                Probe::Last => (self.n as u64, false),
            });
        };
        let range = sample_range(&raw.samples, self.route_raw(raw, query.as_bytes()));
        self.probe(query, |bytes, probe| {
            if probe == Probe::Last {
                return (self.n as u64, false);
            }
            let l = match range {
                Ok((lo, hi)) => self.head_boundary(bytes, lo, hi),
                Err(l) => l,
            };
            let (rank, hit) = self.locate_in(l, bytes);
            (rank, hit && probe == Probe::Exact)
        })
    }

    /// A stored key's bytes as the caller spells them; `None` for bytes no build wrote.
    pub(super) fn spelled(&self, stored: &[u8]) -> Option<String> {
        match &self.code {
            None => std::str::from_utf8(stored).ok().map(str::to_owned),
            Some(code) => {
                let mut out = Vec::with_capacity(4 * stored.len());
                code.decode_into(stored, &mut out)
                    .then(|| String::from_utf8(out).ok())
                    .flatten()
            }
        }
    }

    /// The rank of the first key not below `probe`, and whether that key is `probe`.
    pub(super) fn locate(&self, probe: &[u8]) -> (u64, bool) {
        if self.n == 0 {
            return (0, false);
        }
        let (l, known) = match self.route(probe) {
            Ok(s) => self.sample_boundary(probe, s),
            Err(l) => (l, None),
        };
        match known {
            Some((matched, len)) if matched == len && matched == probe.len() => {
                (((l - 1) * self.block) as u64, true)
            }
            Some((matched, _)) => self.locate_past(l - 1, probe, matched),
            None => self.locate_in(l, probe),
        }
    }

    /// [`sample_range`] and the head compare after it, for one probe whose sample is
    /// `s`. A run of equal samples that has a trie is not measured: its root knows where it ends,
    /// and on paths the gallop to that end was two dozen steps of a lookup. Only a run of
    /// [`MIN_RUN`] or more can have one, and the sample `MIN_RUN - 1` past the first says whether
    /// it is that long; a shorter run is measured and its heads compared.
    #[inline(always)]
    fn sample_boundary(&self, probe: &[u8], s: u64) -> (usize, Option<(usize, usize)>) {
        let samples = &self.samples;
        let lo = samples.partition_point(|&x| x < s);
        if samples.get(lo) != Some(&s) {
            return (lo, None);
        }
        if samples.get(lo + MIN_RUN - 1) == Some(&s) {
            if let Some(placed) = self.tie_boundary(probe, lo) {
                return placed;
            }
        }
        (
            self.head_boundary(probe, lo, past_equal(samples, lo, s)),
            None,
        )
    }

    /// The probe's sample, or the block boundary it lands on outright.
    ///
    /// The samples are taken past the `g` bytes every head shares, so they place a probe only if
    /// it shares them too. One that does not is below every head or above every one, and that is
    /// the answer the samples would have had to produce.
    #[inline]
    pub(super) fn route(&self, probe: &[u8]) -> Result<u64, usize> {
        if self.g > 0 {
            let head = self.head(0);
            place_prefix(&head[..self.g.min(head.len())], self.blocks_len(), probe)?;
        }
        Ok(sample_at(probe, self.g))
    }

    /// The route over the decoded heads, where the blob has a code and the decoded samples tie no
    /// more blocks than the coded ones: eight bytes of UTF-8 hold fewer characters than eight bytes
    /// of code, and a sample shared by a run of blocks leaves their heads to be compared — against
    /// the spelled query, which is then waited for. Chinese titles tie fewer blocks decoded (51 of
    /// 3 907 against 70) and Russian ones far more (659 against 161), so the choice is per blob.
    /// `None` too for heads that do not decode, which only a blob this crate did not write holds.
    pub(super) fn raw_route(&self) -> Option<Box<RawRoute>> {
        let code = self.code.as_deref()?;
        let nb = self.blocks_len();
        if nb == 0 {
            return None;
        }
        let decoded = |b: usize, need: usize| {
            let mut out = Vec::new();
            code.decode_front(self.head(b), need, &mut out)
                .then_some(out)
        };
        let (first, last) = (decoded(0, usize::MAX)?, decoded(nb - 1, usize::MAX)?);
        let g = lcp(&first, &last).min(u16::MAX as usize);
        let mut samples = Vec::with_capacity(nb);
        for b in 0..nb {
            samples.push(sample_at(&decoded(b, g + 8)?, g));
        }
        let ties = |s: &[u64]| s.windows(2).filter(|w| w[0] == w[1]).count();
        (ties(&samples) <= ties(&self.samples)).then(|| {
            Box::new(RawRoute {
                g,
                prefix: first[..g].into(),
                samples,
            })
        })
    }

    /// [`route`](Self::route) for a query as the caller spells it, over the decoded heads.
    #[inline]
    fn route_raw(&self, raw: &RawRoute, query: &[u8]) -> Result<u64, usize> {
        if raw.g > 0 {
            place_prefix(&raw.prefix, self.blocks_len(), query)?;
        }
        Ok(sample_at(query, raw.g))
    }

    /// [`Ties::boundary`] for the run of equal samples that starts at block `lo`, if it has a trie.
    #[inline(always)]
    pub(super) fn tie_boundary(
        &self,
        probe: &[u8],
        lo: usize,
    ) -> Option<(usize, Option<(usize, usize)>)> {
        let ties = self.ties.as_deref()?;
        let root = ties.root(lo)?;
        let flat = |a: usize, c: usize| self.head_boundary(probe, a, c);
        Some(ties.boundary(root, probe, |b| self.head(b), flat))
    }

    /// The first block index in `[l, r)` whose head is past `probe`, or `r` — the samples have
    /// already narrowed that to the run of blocks whose heads share the probe's eight bytes past
    /// the prefix, and the block before the run is below the probe.
    #[inline]
    pub(super) fn head_boundary(&self, probe: &[u8], mut l: usize, mut r: usize) -> usize {
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
    pub(super) fn scan_run(
        &self,
        codec: &Codec,
        entries: &Entries<'_>,
        count: usize,
        probe: &[u8],
        matched: usize,
    ) -> (usize, usize, bool) {
        // What the loop's own bound then stands in for, so that no header pays for it again.
        if entries.count + 1 != count {
            return (0, matched, false);
        }
        let frame = entries.is_frame();
        match codec {
            Codec::Symbols { table, split: None } => {
                scan_as(frame, entries, count, probe, matched, &SymbolsCmp(table))
            }
            Codec::Symbols {
                table,
                split: Some(split),
            } if self.phrases.has_labels() => {
                let cmp = LabelledCmp {
                    table,
                    split: *split,
                    dict: &self.phrases,
                };
                scan_as(frame, entries, count, probe, matched, &cmp)
            }
            Codec::Symbols {
                table,
                split: Some(split),
            } => {
                let cmp = TieredCmp {
                    table,
                    split: *split,
                    dict: &self.phrases,
                };
                scan_as(frame, entries, count, probe, matched, &cmp)
            }
            Codec::Packed(a) => scan_as(frame, entries, count, probe, matched, &PackedCmp(a)),
        }
    }

    /// The words of [`mroute`](Self::mroute), read off every block's restart run.
    pub(super) fn micro_route(&self) -> Vec<u64> {
        let per = self.per;
        if per == 1 {
            return Vec::new();
        }
        let mut out = vec![u64::MAX; self.blocks_len() * per];
        let mut cur = Vec::new();
        for (b, group) in out.chunks_exact_mut(per).enumerate() {
            group[0] = NO_ROUTE;
            let r = self.micros_in(b);
            if r < 2 {
                continue;
            }
            let run = CodedRun {
                codec: self.codec_of(b),
                code: &self.codes_of(b).1,
                data: self.restart_data(b),
                count: r,
            };
            if let Some(o) = self.restart_words(run, self.head(b), &mut cur, &mut group[1..r]) {
                group[0] = o as u64;
            }
        }
        out
    }

    /// One block's restart words into `words`, and the bytes they are taken past; `None` for a run
    /// this crate did not write.
    ///
    /// What every restart shares with the head is the least prefix any two neighbours share, so
    /// one pass over the headers finds it, and a second decodes each restart only as far as the
    /// eight bytes past it: a restart that shares all of them with the one before has its word.
    fn restart_words(
        &self,
        run: CodedRun<'_>,
        head: &[u8],
        cur: &mut Vec<u8>,
        words: &mut [u64],
    ) -> Option<usize> {
        let mut entries = run.entries();
        let mut o = head.len();
        for _ in 0..words.len() {
            let (l, len) = entries.head()?;
            entries.skip(len);
            o = o.min(l);
        }
        if entries.reach() > entries.end_bits {
            return None;
        }
        let need = o + 8;
        let mut entries = run.entries();
        cur.clear();
        cur.extend_from_slice(head);
        for word in words.iter_mut() {
            let (l, len) = entries.head()?;
            let piece = entries.piece(len);
            if l < need {
                if l > cur.len() || piece.units(run.codec.unit()) != len {
                    return None;
                }
                cur.truncate(l);
                if !run.codec.decode_into(&self.phrases, piece, cur, need - l) {
                    return None;
                }
            }
            *word = sample_at(cur, o);
        }
        // Restarts this crate wrote ascend, and so do their words. The route counts on it: the
        // word it takes as the one below a probe must be below it, or the two can agree past the
        // probe's last byte, on its padding, and hand the scan more matched bytes than the probe
        // has. A blob's restarts are not checked at load, so words out of order leave the block to
        // its restart run.
        words.is_sorted().then_some(o)
    }

    /// The microblock of block `b` — `r` of them — that `probe` falls in, off the block's words in
    /// [`mroute`](Self::mroute), and how many bytes the probe shares with that microblock's first
    /// key; `None` where only the restart run can say, which is where a restart's word is the
    /// probe's own. `matched` is what the probe shares with the head, which it is above.
    #[inline(always)]
    pub(super) fn micro_routed(
        &self,
        b: usize,
        r: usize,
        probe: &[u8],
        matched: usize,
    ) -> Option<(usize, usize)> {
        let per = self.per;
        let group = self.mroute.get()?.get(b * per..(b + 1) * per)?;
        let o = group[0];
        if o == NO_ROUTE {
            return None;
        }
        let o = o as usize;
        // The probe leaves the prefix every restart shares, above the head: above all of them.
        if matched < o {
            return Some((r - 1, matched));
        }
        let s = sample_at(probe, o);
        let words = &group[1..];
        // The words ascend — a block whose restarts do not is never routed — so the count of words
        // below the probe is a bisection's answer: four branchless steps over fifteen words where a
        // compare of every word took fifteen, and five over thirty-one.
        let j = words.partition_point(|&w| w < s);
        if words.get(j) == Some(&s) {
            return None;
        }
        let Some(&below) = j.checked_sub(1).map(|i| &words[i]) else {
            return Some((0, matched));
        };
        let i = ((below ^ s).leading_zeros() / 8) as usize;
        if may_end_before(below, i) {
            return None;
        }
        debug_assert!(
            o + i < probe.len(),
            "a route word below the probe agrees with its padding"
        );
        Some((j, o + i))
    }

    /// The rest of [`locate`](Self::locate) once the block boundary is known: the block below it
    /// is the only one that can hold `probe`. Its restart words, or its restart run where they
    /// tie, say which of its microblocks can, and that one microblock is scanned — the block
    /// itself never is.
    pub(super) fn locate_in(&self, l: usize, probe: &[u8]) -> (u64, bool) {
        if l == 0 {
            return (0, false);
        }
        let b = l - 1;
        let head = self.head(b);
        let matched = lcp(head, probe);
        if matched == head.len() && matched == probe.len() {
            return ((b * self.block) as u64, true);
        }
        self.locate_past(b, probe, matched)
    }

    /// [`locate_in`](Self::locate_in) past block `b`'s head, which is below `probe` and shares
    /// `matched` bytes with it: what a lookup that has already compared the two starts from.
    #[inline(always)]
    fn locate_past(&self, b: usize, probe: &[u8], matched: usize) -> (u64, bool) {
        let base = b * self.block;
        let codec = self.codec_of(b);
        let r = self.micros_in(b);
        let block_base = self.blocks.at(b);
        let (j, matched) = if r > 1 {
            match self.micro_routed(b, r, probe, matched) {
                Some(routed) => routed,
                None => {
                    let restarts = Entries::of(
                        &self.codes_of(b).1,
                        codec,
                        self.restart_data_at(b, block_base),
                        r,
                    );
                    let (j, matched, hit) = self.scan_run(codec, &restarts, r, probe, matched);
                    if hit {
                        return ((base + j * self.micro) as u64, true);
                    }
                    (j, matched)
                }
            }
        } else {
            (0, matched)
        };
        let count = self.micro_count(b, j);
        let entries = Entries::of(
            &self.codes_of(b).0,
            codec,
            self.micro_data_at(b, j, block_base),
            count,
        );
        let (k, _, hit) = self.scan_run(codec, &entries, count, probe, matched);
        let rank = base + j * self.micro + k + usize::from(!hit);
        (rank as u64, hit)
    }
}
