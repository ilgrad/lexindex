//! Encoding a sorted range into blocks: codecs trained, phrases mined, runs written.

use super::*;

/// Bytes the whole codec section takes: every shard's codec behind its own length, so a reader
/// that knows how many there are can walk them without a directory.
pub(super) fn codecs_len(codecs: &[Codec]) -> usize {
    codecs.iter().map(|c| 4 + c.serialized_len()).sum()
}

pub(super) fn write_codecs(codecs: &[Codec], out: &mut Vec<u8>) {
    for c in codecs {
        out.extend_from_slice(&(c.serialized_len() as u32).to_le_bytes());
        c.write_to(out);
    }
}

/// Bytes the header codes take: two a shard, each self-delimiting, so they need no lengths of
/// their own.
pub(super) fn codes_len(codes: &[(Code, Code)]) -> usize {
    codes
        .iter()
        .map(|(m, r)| m.serialized_len() + r.serialized_len())
        .sum()
}

pub(super) fn write_codes(codes: &[(Code, Code)], out: &mut Vec<u8>) {
    for (m, r) in codes {
        m.write_to(out);
        r.write_to(out);
    }
}

/// What the trainers may hold between them. One runs per training thread and each holds its own
/// sample, so an unbounded build's peak follows the core count rather than the work: 3.0 MB a
/// trainer is 45 MB over sixteen threads and 380 over a machine with 128. The bound is in bytes
/// rather than in threads because a thread count is fitted to the machine it was written on, and it
/// costs nothing measurable — training is not the critical path until the cap is far below this
/// one: capping a ten-million-key build at 8 trainers moved it −0.4 %, at 4 by +2.9 %.
pub(super) const TRAIN_BUDGET: usize = 64 << 20;

/// What one trainer holds while it runs, from narrowing the affinity mask `available_parallelism`
/// reads from sixteen threads down to one and reading `VmHWM`: 2.6 MB over a million keys, 3.0 over
/// ten million, and flat in `n` either way.
pub(super) const TRAIN_BYTES: usize = 3 << 20;

/// Trainers the budget allows, and never none.
pub(super) fn train_threads(threads: usize) -> usize {
    threads.clamp(1, TRAIN_BUDGET / TRAIN_BYTES)
}

/// The suffixes a build trains and mines on, one list a shard: what a spread of runs would store,
/// in key order — a restart is coded against the restart before it, every other key against its
/// predecessor. Each shard is sampled inside itself, by the rule that samples the whole index when
/// there is one shard.
///
/// `dense` takes every suffix rather than [`TRAIN_PIECES`] a shard. That is what the estimator's
/// sample does: a hundred thousand keys standing in for a million hold a tenth of the evidence, and
/// the miner prices a span on the pieces it is shown — at a sixth of them it reads a span seen twice
/// as a hundred uses of the blob rather than ten.
///
/// Only the runs drawn are visited: a key's piece is coded against the key a microblock before it
/// when it opens a microblock and against the one before it otherwise, and both are a subtraction
/// away. Walking every key to carry the two along cost three divisions a key and read every one of
/// them: 16 ms of a million article titles, against 4.5 for the sixth of them that are drawn.
pub(super) fn build_pieces<K: AsKey>(
    keys: &[K],
    block: usize,
    micro: usize,
    dense: usize,
) -> Vec<Vec<&[u8]>> {
    let n = keys.len();
    let nb = n.div_ceil(block);
    let shard = shard_blocks_for(block);
    let span = shard * block;
    let mut pieces: Vec<Vec<&[u8]>> = vec![Vec::new(); nb.div_ceil(shard).max(1)];
    for (s, out) in pieces.iter_mut().enumerate() {
        let (start, end) = (s * span, ((s + 1) * span).min(n));
        let step = if dense > 0 {
            dense
        } else {
            ((end - start) / TRAIN_PIECES).max(1)
        };
        for from in (start..end).step_by(TRAIN_RUN * step) {
            let to = (from + TRAIN_RUN).min(end);
            // A run is a stride past the last one, so neither its first keys nor the ones they are
            // coded against are in any cache: those are asked for together, and every later key a
            // [`READ_AHEAD`] early, as the other passes over a caller's keys ask for them.
            for key in &keys[from.saturating_sub(micro)..to.min(from + READ_AHEAD)] {
                crate::blob::prefetch_key(key.key_bytes());
            }
            for i in from..to {
                if let Some(next) = keys[..to].get(i + READ_AHEAD) {
                    crate::blob::prefetch_key(next.key_bytes());
                }
                let off = i % block;
                // A block's head is stored whole, so it is no piece.
                if off == 0 {
                    continue;
                }
                let against = keys[if off % micro == 0 { i - micro } else { i - 1 }].key_bytes();
                let key = keys[i].key_bytes();
                out.push(&key[lcp(against, key)..]);
            }
        }
    }
    pieces
}

/// One symbol table a shard, trained in parallel: the shards are independent, and training is
/// linear in the sample, so a table a shard costs the whole budget again on every one of them.
pub(super) fn train_shards(samples: &[Vec<&[u8]>], threads: usize) -> Vec<Table> {
    if threads == 1 || samples.len() == 1 {
        return samples.iter().map(|s| Table::train(s)).collect();
    }
    let span = samples.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let running: Vec<_> = samples
            .chunks(span)
            .map(|c| scope.spawn(move || c.iter().map(|s| Table::train(s)).collect::<Vec<_>>()))
            .collect();
        running
            .into_iter()
            .flat_map(|h| h.join().expect("training a symbol table cannot panic"))
            .collect()
    })
}

/// Bytes the per-block arrays take: one sample a block, the two packed block arrays and the packed
/// microblock starts. `None` where a header names more blocks than a blob on this platform could
/// hold.
pub(super) fn arrays_len(
    nb: usize,
    nm: usize,
    head_width: u32,
    block_width: u32,
    micro_width: u32,
    shift: u32,
) -> Option<usize> {
    offsets::section_len(nb, head_width, shift)?
        .checked_add(offsets::section_len(nb, block_width, shift)?)?
        .checked_add(if nm == 0 {
            0
        } else {
            (nm as u64)
                .checked_mul(u64::from(micro_width))?
                .div_ceil(8)
                .checked_add(8)
                .and_then(|v| usize::try_from(v).ok())?
        })
}

/// A monotone array of block offsets as the two sections a blob stores, under the width the
/// values themselves ask for.
pub(super) fn packed_offsets(values: &[u64]) -> Offsets {
    let width = offsets::width_of(values, offsets::SHIFT);
    let (bases, deltas) = offsets::pack(values, offsets::SHIFT, width);
    Offsets::new(
        SharedBytes::from_owned(bases),
        SharedBytes::from_owned(deltas),
        width,
        offsets::SHIFT,
    )
}

/// The microblock starts as the one section a blob stores them in: offsets inside their own
/// block, so no base of their own — see [`rebase_micros`].
pub(super) fn packed_micros(values: &[u64]) -> offsets::Packed {
    let width = offsets::packed_width_of(values);
    offsets::Packed::new(
        SharedBytes::from_owned(offsets::packed_pack(values, width)),
        width,
    )
}

/// What one thread produces for its contiguous range of blocks: the sections it would have
/// appended, with the two that are offsets kept relative to the range so the concatenation can
/// rebase them.
pub(super) struct Part {
    pub(super) heads: Vec<u8>,
    pub(super) head_ends: Vec<u64>,
    pub(super) blocks: Vec<u64>,
    pub(super) micros: Vec<u64>,
    pub(super) data: Vec<u8>,
    /// The two header codes of each shard the range covers, microblocks' first.
    pub(super) codes: Vec<(Code, Code)>,
    /// The suffix codec each of those shards settled on.
    pub(super) codecs: Vec<Codec>,
}

/// The runs of one group — one kind of run over a whole shard — as [`Code::choose_cost`] reads
/// them.
type Group<'a> = Vec<&'a [(usize, usize)]>;

/// One shard's runs as they are collected: every entry's `(lcp, len)` and coded suffix, end to end
/// in the order the blocks will be written, with a block's restart run before its microblocks.
///
/// A group's header code is chosen on all of its runs at once, and a run's frame on all of its
/// pairs, so neither can be written while the keys are still arriving — the shard is coded once
/// here and written once after. It holds about sixteen bytes a key plus the suffixes, one shard at
/// a time, which is a megabyte or two on the thread that is already holding the keys.
pub(super) struct Collected {
    /// Every entry's shared-prefix length, in the order the blocks are written.
    lcps: Vec<usize>,
    /// Every entry's suffix as the key holds it, end to end, and where each of them ends.
    raw: Vec<u8>,
    raw_ends: Vec<usize>,
    /// The same suffixes under the shard's symbol table, and where each of those ends.
    coded: Vec<u8>,
    coded_ends: Vec<usize>,
    /// The same again with a share of the byte codes given to the blob's phrases — filled only
    /// when a sample of the shard says a split is worth pricing, and left empty otherwise.
    tier: Vec<u8>,
    tier_ends: Vec<usize>,
    /// What the suffix bytes are, which is what a packed alphabet is chosen on.
    freq: [u64; 256],
    /// Entries in each run, a block's restart run first.
    runs: Vec<usize>,
    /// Runs in each block, which is its microblocks plus the restart run.
    per_block: Vec<usize>,
}

impl Default for Collected {
    fn default() -> Self {
        Self {
            lcps: Vec::new(),
            raw: Vec::new(),
            raw_ends: Vec::new(),
            coded: Vec::new(),
            coded_ends: Vec::new(),
            tier: Vec::new(),
            tier_ends: Vec::new(),
            freq: [0; 256],
            runs: Vec::new(),
            per_block: Vec::new(),
        }
    }
}

impl Collected {
    pub(super) fn clear(&mut self) {
        self.lcps.clear();
        self.raw.clear();
        self.raw_ends.clear();
        self.coded.clear();
        self.coded_ends.clear();
        self.tier.clear();
        self.tier_ends.clear();
        self.freq = [0; 256];
        self.runs.clear();
        self.per_block.clear();
    }

    /// Starts a run; every [`push`](Self::push) after this one belongs to it.
    fn open(&mut self) {
        self.runs.push(0);
    }

    /// Append `key` coded against `prev`, through `encoder` and `scratch` as scratch.
    fn push(&mut self, prev: &[u8], key: &[u8], encoder: &fsst::Encoder, scratch: &mut Vec<u8>) {
        let l = lcp(prev, key);
        let raw = &key[l..];
        self.lcps.push(l);
        self.raw.extend_from_slice(raw);
        self.raw_ends.push(self.raw.len());
        for &b in raw {
            self.freq[b as usize] += 1;
        }
        scratch.clear();
        encoder.encode_into(raw, scratch);
        self.coded.extend_from_slice(scratch);
        self.coded_ends.push(self.coded.len());
        *self.runs.last_mut().expect("a run is open") += 1;
    }

    fn entries(&self) -> usize {
        self.lcps.len()
    }

    /// Entry `i`'s suffix as the key holds it.
    fn raw_at(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.raw_ends[i - 1] };
        &self.raw[start..self.raw_ends[i]]
    }

    /// Entry `i`'s suffix under the symbol table.
    fn coded_at(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.coded_ends[i - 1] };
        &self.coded[start..self.coded_ends[i]]
    }

    /// Entry `i`'s suffix under the symbol table and the phrases together.
    fn tier_at(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.tier_ends[i - 1] };
        &self.tier[start..self.tier_ends[i]]
    }

    /// Code every entry again under `split`, filling [`tier`](Self::tier).
    fn recode(&mut self, enc: &fsst::Encoder, trie: &Trie, split: Split, w: &mut phrase::Scratch) {
        let (mut tier, mut ends) = (
            std::mem::take(&mut self.tier),
            std::mem::take(&mut self.tier_ends),
        );
        tier.clear();
        ends.clear();
        for i in 0..self.entries() {
            phrase::encode_into(self.raw_at(i), enc, trie, Some(split), w, &mut tier);
            ends.push(tier.len());
        }
        self.tier = tier;
        self.tier_ends = ends;
    }

    /// The `(lcp, len)` pairs the shard would store under `codec`, in entry order.
    fn pairs(&self, codec: &Codec, out: &mut Vec<(usize, usize)>) {
        out.clear();
        out.reserve(self.entries());
        match codec {
            Codec::Symbols { split: Some(_), .. } => {
                out.extend((0..self.entries()).map(|i| (self.lcps[i], self.tier_at(i).len())))
            }
            Codec::Symbols { .. } => {
                out.extend((0..self.entries()).map(|i| (self.lcps[i], self.coded_at(i).len())))
            }
            Codec::Packed(a) => {
                out.extend((0..self.entries()).map(|i| (self.lcps[i], a.codes_for(self.raw_at(i)))))
            }
        }
    }

    /// The pairs of every run, told apart by kind: the microblocks' and the restarts'.
    fn groups<'a>(&self, pairs: &'a [(usize, usize)]) -> (Group<'a>, Group<'a>) {
        let (mut micro, mut restart) = (Vec::new(), Vec::new());
        let mut at = 0;
        let mut r = 0;
        for &runs in &self.per_block {
            for k in 0..runs {
                let run = &pairs[at..at + self.runs[r]];
                if k == 0 { &mut restart } else { &mut micro }.push(run);
                at += self.runs[r];
                r += 1;
            }
        }
        (micro, restart)
    }

    /// What the shard would take under `codec`: the two header codes' bytes, the escapes they
    /// leave, and the coded suffixes — with the two codes, the microblocks' first, so that the
    /// codec that wins is written under the codes it was priced with rather than chosen again.
    fn price(&self, codec: &Codec, pairs: &mut Vec<(usize, usize)>) -> (usize, (Code, Code)) {
        self.pairs(codec, pairs);
        let (micro, restart) = self.groups(pairs);
        let (micro, restart) = (Code::choose_cost(&micro), Code::choose_cost(&restart));
        let headers = micro.1 + restart.1;
        let suffixes = match codec {
            Codec::Symbols { split: Some(_), .. } => self.tier.len(),
            Codec::Symbols { .. } => self.coded.len(),
            // The codes of a run are continuous and every run ends on a byte, which is where the
            // rounding goes.
            Codec::Packed(a) => {
                let mut at = 0;
                let mut bytes = 0;
                for &count in &self.runs {
                    let codes: usize = pairs[at..at + count].iter().map(|p| p.1).sum();
                    bytes += (codes * a.width() as usize).div_ceil(8);
                    at += count;
                }
                bytes
            }
        };
        (headers + suffixes, (micro.0, restart.0))
    }

    /// The codec the shard is cheapest under, with its pairs and its two header codes. The symbol
    /// table is the one already trained, and it wins unless another beats it outright: a tie goes
    /// to the table, whose scan reads a byte at a time rather than a code.
    pub(super) fn settle(
        &mut self,
        table: Table,
        phrases: &Phrases,
        pairs: &mut Vec<(usize, usize)>,
        w: &mut phrase::Scratch,
    ) -> (Codec, (Code, Code)) {
        let mut best = Codec::Symbols {
            table: table.clone(),
            split: None,
        };
        let (mut bytes, mut codes) = self.price(&best, pairs);
        if let Some((alphabet, _)) = Alphabet::of(&self.freq) {
            let alternative = Codec::Packed(alphabet);
            let (packed, packed_codes) = self.price(&alternative, pairs);
            if packed < bytes {
                best = alternative;
                bytes = packed;
                codes = packed_codes;
            }
        }
        if phrases.dict.len() > 0 && self.entries() > 0 {
            // The split is chosen on a sample: settling on one means coding every suffix of the
            // shard a second time, and there are whole corpora where none of them ever wins.
            let step = (self.entries() / SETTLE_PIECES).max(1);
            let sample: Vec<&[u8]> = (0..self.entries())
                .step_by(step)
                .map(|i| self.raw_at(i))
                .collect();
            let (split, cut) =
                phrase::settle(&sample, &table, &phrases.trie, phrases.dict.len(), w);
            if let Some(split) = split {
                self.recode(&cut.encoder(), &phrases.trie, split, w);
                let tier = Codec::Symbols {
                    table: cut,
                    split: Some(split),
                };
                let (tiered, tiered_codes) = self.price(&tier, pairs);
                if (tiered as u64) * 100 < (bytes as u64) * (100 - phrase::MARGIN) {
                    best = tier;
                    codes = tiered_codes;
                }
            }
        }
        // The pairs are left as the winner's: the codec priced last is not always the one kept.
        self.pairs(&best, pairs);
        (best, codes)
    }
}

/// The blob's phrases as a build holds them: the dictionary every shard names and the walk a parse
/// takes through it. One of each for the whole build, shared by every encoding thread.
pub(super) struct Phrases {
    pub(super) dict: Dict,
    trie: Trie,
}

impl Phrases {
    pub(super) fn none() -> Self {
        Self {
            dict: Dict::empty(),
            trie: Trie::empty(),
        }
    }

    fn of(phrases: Vec<Vec<u8>>) -> Self {
        Self {
            trie: Trie::of(phrases.iter().map(Vec::as_slice)),
            dict: Dict::of(&phrases),
        }
    }

    /// The phrases of `self` with the trie labels of `keys` named past them, where any pay: those a
    /// lookup reads most as phrases of their own, the others nested.
    pub(super) fn labelled<K: AsKey + Sync>(
        &self,
        keys: &[K],
        block: usize,
        micro: usize,
        tables: &[Table],
    ) -> Option<Self> {
        let phrases = self.listed()?;
        let room = phrase::IDS_MAX - phrases.len();
        let got = crate::labels::select(keys, &phrases, room, block, micro, tables)?;
        Some(Self::past(phrases, got))
    }

    /// [`labelled`](Self::labelled) off a key stream whose edges `count` holds: the same labels, at
    /// one more walk of `src`.
    pub(super) fn labelled_from<R: Replay>(
        &self,
        count: crate::labels::EdgeCount,
        src: &mut R,
        block: usize,
        micro: usize,
        tables: &[Table],
    ) -> Result<Option<Self>, IndexError> {
        let Some(phrases) = self.listed() else {
            return Ok(None);
        };
        let room = phrase::IDS_MAX - phrases.len();
        let got = count.select(src, &phrases, room, block, micro, tables)?;
        Ok(got.map(|got| Self::past(phrases, got)))
    }

    fn listed(&self) -> Option<Vec<Vec<u8>>> {
        (0..self.dict.phrases())
            .map(|id| self.dict.at(id).map(<[u8]>::to_vec))
            .collect()
    }

    fn past(mut phrases: Vec<Vec<u8>>, got: crate::labels::Selected) -> Self {
        phrases.extend(got.flat);
        let dict = Dict::of(&phrases);
        match got.nested {
            Some((labels, store)) => Self {
                trie: Trie::of(phrases.iter().chain(&labels).map(Vec::as_slice)),
                dict: dict.with_labels(store),
            },
            None => Self {
                trie: Trie::of(phrases.iter().map(Vec::as_slice)),
                dict,
            },
        }
    }
}

/// Write one run: its headers under `code`, then its suffixes, each preceded by the two varints of
/// the pair the code could not name.
fn write_run(
    code: &paircode::Inverse,
    codec: &Codec,
    collected: &Collected,
    pairs: &[(usize, usize)],
    first: usize,
    scratch: &mut Scratch,
) {
    let Scratch {
        block: out,
        run: body,
        widths,
        ..
    } = scratch;
    // A block whose only microblock is itself has no restarts, and an empty run is no bytes: a
    // frame would still write its prologue, and the microblock that follows starts where the
    // block does.
    if pairs.is_empty() {
        return;
    }
    let mut writer = paircode::Writer::new(code, pairs, widths);
    body.clear();
    let mut escape = Vec::new();
    for (i, &(l, len)) in pairs.iter().enumerate() {
        escape.clear();
        writer.push(l, len, &mut escape);
        if !escape.is_empty() {
            // The varints of a pair no code could name sit on a byte, so that a packed run reads
            // them the way a symbol-coded one does.
            body.pad();
            body.extend_from_slice(&escape);
        }
        match codec {
            Codec::Symbols { split: Some(_), .. } => {
                body.extend_from_slice(collected.tier_at(first + i))
            }
            Codec::Symbols { .. } => body.extend_from_slice(collected.coded_at(first + i)),
            Codec::Packed(a) => {
                let codes = a.encode_into(collected.raw_at(first + i), body);
                debug_assert_eq!(codes, len, "a packed suffix priced at a different width");
            }
        }
    }
    writer.finish(out);
    body.drain_into(out);
}

/// One block's keys, collected into `runs` in the order the block is written: its restart run
/// first — the first key of every microblock past the head, each coded against the restart before
/// it — then each microblock's own entries.
pub(super) fn collect_block(
    collected: &mut Collected,
    keys: &[&[u8]],
    micro: usize,
    encoder: &fsst::Encoder,
    scratch: &mut Vec<u8>,
) {
    let mut prev_restart = keys[0];
    // The restart run opens first because it is written first, and is left empty when the block is
    // a single microblock.
    collected.open();
    for keys in keys.chunks(micro).skip(1) {
        collected.push(prev_restart, keys[0], encoder, scratch);
        prev_restart = keys[0];
    }
    for keys in keys.chunks(micro) {
        collected.open();
        for w in keys.windows(2) {
            collected.push(w[0], w[1], encoder, scratch);
        }
    }
    collected.per_block.push(1 + keys.len().div_ceil(micro));
}

/// What writing a shard's blocks needs and neither build should allocate per block.
#[derive(Default)]
pub(super) struct Scratch {
    block: Vec<u8>,
    run: packed::Bits,
    starts: Vec<u64>,
    widths: paircode::Widths,
}

/// One shard as it is about to be written: its entries, the pairs they code to, and the two
/// dictionaries the shard settled on.
pub(super) struct Shard<'a> {
    pub(super) collected: &'a Collected,
    pub(super) pairs: &'a [(usize, usize)],
    pub(super) codec: &'a Codec,
    pub(super) codes: &'a (Code, Code),
}

impl Shard<'_> {
    /// Write the shard, block by block, through `sink`, growing the two start arrays. `at` is
    /// where it begins in the block data and is left past its end.
    ///
    /// Both builds go through this: the in-memory one sinks into its part, the streamed one into
    /// the file, and the bytes are the same either way.
    pub(super) fn write(
        &self,
        at: &mut u64,
        blocks: &mut Vec<u64>,
        micros: &mut Vec<u64>,
        scratch: &mut Scratch,
        sink: &mut impl FnMut(&[u8]) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        let (mut first, mut r) = (0usize, 0usize);
        let codes = (
            paircode::Inverse::of(&self.codes.0),
            paircode::Inverse::of(&self.codes.1),
        );
        for &runs in &self.collected.per_block {
            blocks.push(*at);
            scratch.starts.clear();
            scratch.block.clear();
            for k in 0..runs {
                let count = self.collected.runs[r];
                // A microblock's start is only known once the runs before it are written, so it is
                // taken from the block being built and rebased onto the blob here.
                if k > 0 {
                    scratch.starts.push(*at + scratch.block.len() as u64);
                }
                let code = if k == 0 { &codes.1 } else { &codes.0 };
                write_run(
                    code,
                    self.codec,
                    self.collected,
                    &self.pairs[first..first + count],
                    first,
                    scratch,
                );
                first += count;
                r += 1;
            }
            micros.extend_from_slice(&scratch.starts);
            sink(&scratch.block)?;
            *at += scratch.block.len() as u64;
        }
        Ok(())
    }
}

/// Micro starts as offsets inside their own block, which is how the blob stores them.
///
/// Packed absolutely, one superblock of sixty-four microblocks spans four blocks of data, so its
/// deltas need twelve bits and a base of its own — 13.0 bits an entry on the dictionary, and
/// `micro_offsets` is 3.8 % of that blob and 8.3 % of a blob of decimal ids. Against `blocks[b]`
/// the delta is one block's span and the base is nothing, and the base costs a reader no load it
/// was not already making: `restart_data` reads `blocks[b]` before the scan starts.
///
/// Every microblock starts at or after its block, so no entry goes negative.
pub(super) fn rebase_micros(micros: &mut [u64], blocks: &[u64], per: usize) {
    for (i, m) in micros.iter_mut().enumerate() {
        *m -= blocks[i / per];
    }
}

/// The blob's phrases, mined from the samples the shard tables were trained on.
pub(super) fn mine_phrases(
    samples: &[Vec<&[u8]>],
    tables: &[Table],
    n: usize,
    threads: usize,
) -> Phrases {
    if n < MINE_MIN || phrase::packed_only(samples, tables) {
        return Phrases::none();
    }
    let pool = (n / KEYS_PER_PHRASE).clamp(1 << 12, 1 << 19);
    Phrases::of(phrase::mine(samples, tables, n, pool, threads))
}

/// Encode a range of whole shards. `keys` must start on a shard boundary, which is what makes the
/// parts concatenate into the blob a single pass would have written — and what lets a shard's
/// header codes be chosen from all of its runs.
pub(super) fn encode_range<K: AsKey>(
    keys: &[K],
    block: usize,
    micro: usize,
    tables: &[Table],
    phrases: &Phrases,
    shard: usize,
    first_block: usize,
) -> Part {
    let nb = keys.len().div_ceil(block);
    let mut part = Part {
        heads: Vec::new(),
        head_ends: Vec::with_capacity(nb),
        blocks: Vec::with_capacity(nb),
        micros: Vec::with_capacity(nb * block.div_ceil(micro)),
        data: Vec::new(),
        codes: Vec::new(),
        codecs: Vec::new(),
    };
    let mut coded = Vec::with_capacity(64);
    let mut pairs = Vec::new();
    let mut scratch = Scratch::default();
    let mut collected = Collected::default();
    let mut parsed = phrase::Scratch::default();
    let mut view: Vec<&[u8]> = Vec::with_capacity(block);
    let mut at = 0u64;
    for (s, span) in keys.chunks(shard * block).enumerate() {
        let table = &tables[((first_block / shard) + s).min(tables.len() - 1)];
        let encoder = table.encoder();
        collected.clear();
        for chunk in span.chunks(block) {
            let head = chunk[0].key_bytes();
            part.heads.extend_from_slice(head);
            part.head_ends.push(part.heads.len() as u64);
            view.clear();
            view.extend(chunk.iter().map(AsKey::key_bytes));
            collect_block(&mut collected, &view, micro, &encoder, &mut coded);
        }
        let (codec, codes) = collected.settle(table.clone(), phrases, &mut pairs, &mut parsed);
        Shard {
            collected: &collected,
            pairs: &pairs,
            codec: &codec,
            codes: &codes,
        }
        .write(
            &mut at,
            &mut part.blocks,
            &mut part.micros,
            &mut scratch,
            &mut |bytes| {
                part.data.extend_from_slice(bytes);
                Ok(())
            },
        )
        .expect("appending to a vector cannot fail");
        part.codes.push(codes);
        part.codecs.push(codec);
    }
    part
}
