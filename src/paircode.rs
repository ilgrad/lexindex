//! The `(lcp, len)` pairs of a front-coded run, coded at a fixed width.
//!
//! `BDX2` spends one byte an entry: `lcp << 4 | len` when both fit a nibble, and otherwise an
//! escape byte with the pair as two varints at the head of that entry's suffix. On a path list the
//! shared prefix is a hundred bytes, so nearly every entry escapes and the pair costs three bytes
//! where its entropy is under one and a half. The escape, not the field width, is the cost.
//!
//! Two codes replace it, and a group — one shard's runs of one kind — takes whichever is smaller:
//!
//! * a **frame of reference**, per run: the smallest `lcp` and the smallest `len` in the run as
//!   varints, two four-bit widths in a byte after them, and every pair as the two offsets from
//!   those bases side by side;
//! * a **learned table**, per group: the pairs worth naming held in a table of `2^w - 1` entries,
//!   two bytes each, and every pair as its index.
//!
//! Both reserve the all-ones code for the escape, which continues as the two varints at the head
//! of the entry's own suffix exactly as `BDX2`'s does, and both put entry `i`'s code at a fixed
//! bit position — no header's position waits on another's, so a scan reads the `i`th pair without
//! walking the `i - 1` before it.

use crate::dict_index::{put_varint, varint_at};

/// Widths a frame's two fields may take: four bits each, side by side in the byte that ends the
/// run's prologue.
const FRAME_MAX: u32 = 15;
/// Widths the learned table may take. Ten bits is 1 023 pairs and two kilobytes of table a group,
/// which is where a shard of 65 536 keys stops paying for more.
const TABLE_MAX: u32 = 10;
/// The bytes a table's group carries before its pairs: the tag, the width and the count.
const TABLE_HEADER: usize = 4;
/// A pair the table can name: both fields a byte, so a table entry is two bytes.
const FIELD_MAX: usize = u8::MAX as usize;

/// How one group's pairs are coded. A group is one shard's runs of one kind — its microblocks or
/// its restarts — because the two kinds differ in length by the microblock size and a run of seven
/// entries does not repay a prologue that a run of thirty-one does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Code {
    /// Every run carries its own bases and widths.
    Frame,
    /// The group carries one table; `pairs[i]` is `(lcp << 8) | len`.
    Table { w: u32, pairs: Vec<u16> },
}

/// Bytes the two varints of an escaped pair take.
fn escape_len(lcp: usize, len: usize) -> usize {
    varint_len(lcp) + varint_len(len)
}

fn varint_len(mut v: usize) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// The widths a run's pairs are cheapest under, and what that costs in bytes — the header bits,
/// the escapes it leaves and the prologue.
fn frame_widths(pairs: &[(usize, usize)]) -> (u32, u32, usize) {
    let (bl, bn) = bases(pairs);
    let base = 1 + varint_len(bl) + varint_len(bn);
    let mut best = (0, 0, usize::MAX);
    for wl in 0..=FRAME_MAX {
        for wn in 0..=FRAME_MAX {
            let bits = pairs.len() * (wl + wn) as usize;
            let mut cost = bits.div_ceil(8) + base;
            for &(lcp, len) in pairs {
                if !frame_fits(lcp - bl, len - bn, wl, wn) {
                    cost += escape_len(lcp, len);
                }
            }
            if cost < best.2 {
                best = (wl, wn, cost);
            }
        }
    }
    best
}

/// The smallest `lcp` and the smallest `len` in a run, which are what its codes are offsets from.
fn bases(pairs: &[(usize, usize)]) -> (usize, usize) {
    pairs.iter().fold((usize::MAX, usize::MAX), |(l, n), p| {
        (l.min(p.0), n.min(p.1))
    })
}

/// Whether the offsets `(dl, dn)` fit the widths without landing on the escape.
fn frame_fits(dl: usize, dn: usize, wl: u32, wn: u32) -> bool {
    let (top_l, top_n) = (top(wl), top(wn));
    dl <= top_l && dn <= top_n && (dl, dn) != (top_l, top_n)
}

/// The all-ones value of a `w`-bit field.
fn top(w: u32) -> usize {
    (1usize << w) - 1
}

impl Code {
    /// The cheaper of the two codes over `runs`, the runs of one group.
    pub(crate) fn choose(runs: &[&[(usize, usize)]]) -> Self {
        Self::choose_cost(runs).0
    }

    /// [`choose`](Self::choose) with what the winner costs in bytes: its own, its headers' and the
    /// escapes it leaves. A caller pricing one shape of the data against another needs the number,
    /// and it falls out of the same search.
    pub(crate) fn choose_cost(runs: &[&[(usize, usize)]]) -> (Self, usize) {
        let frame: usize = runs
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| frame_widths(r).2)
            .sum();
        match Self::best_table(runs) {
            Some((code, bytes)) if bytes < frame => (code, bytes),
            _ => (Code::Frame, frame),
        }
    }

    /// The learned table at the width that codes `runs` smallest, with what it costs including its
    /// own bytes; `None` when no width beats naming nothing.
    pub(crate) fn best_table(runs: &[&[(usize, usize)]]) -> Option<(Self, usize)> {
        let mut counts: std::collections::HashMap<(usize, usize), usize> =
            std::collections::HashMap::new();
        let mut entries = 0usize;
        for pair in runs.iter().copied().flatten() {
            entries += 1;
            if pair.0 <= FIELD_MAX && pair.1 <= FIELD_MAX {
                *counts.entry(*pair).or_default() += 1;
            }
        }
        if entries == 0 {
            return None;
        }
        // A pair is worth a table slot for what its escapes would have cost, so the slots go to the
        // pairs whose frequency times escape length is largest, not to the commonest pairs.
        let mut ranked: Vec<((usize, usize), usize)> = counts
            .into_iter()
            .map(|(pair, n)| (pair, n * escape_len(pair.0, pair.1)))
            .collect();
        ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let escaped_all: usize = ranked.iter().map(|&(_, saved)| saved).sum();
        let loose: usize = runs
            .iter()
            .copied()
            .flatten()
            .filter(|p| p.0 > FIELD_MAX || p.1 > FIELD_MAX)
            .map(|p| escape_len(p.0, p.1))
            .sum();
        let mut best: Option<(Self, usize)> = None;
        for w in 1..=TABLE_MAX {
            let slots = top(w).min(ranked.len());
            let named: usize = ranked[..slots].iter().map(|&(_, saved)| saved).sum();
            let table = TABLE_HEADER + 2 * slots;
            let cost = (entries * w as usize).div_ceil(8) + (escaped_all - named) + loose + table;
            if best.as_ref().is_none_or(|(_, b)| cost < *b) {
                let pairs = ranked[..slots]
                    .iter()
                    .map(|&((lcp, len), _)| ((lcp as u16) << 8) | len as u16)
                    .collect();
                best = Some((Code::Table { w, pairs }, cost));
            }
        }
        best
    }

    /// The code as the group's bytes in the blob.
    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            Code::Frame => out.push(0),
            Code::Table { w, pairs } => {
                out.push(1);
                out.push(*w as u8);
                out.extend_from_slice(&(pairs.len() as u16).to_le_bytes());
                for p in pairs {
                    out.extend_from_slice(&p.to_le_bytes());
                }
            }
        }
    }

    /// The group at the start of `bytes` and what is left after it; `None` if it is cut short, its
    /// width is one this version does not write, or its table is larger than that width can name.
    pub(crate) fn read(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let (&tag, rest) = bytes.split_first()?;
        match tag {
            0 => Some((Code::Frame, rest)),
            1 => {
                let (&w, rest) = rest.split_first()?;
                let (count, rest) = rest.split_first_chunk::<2>()?;
                let (w, count) = (u32::from(w), usize::from(u16::from_le_bytes(*count)));
                if w == 0 || w > TABLE_MAX || count > top(w) {
                    return None;
                }
                let (table, rest) = rest.split_at_checked(2 * count)?;
                let pairs = table
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Some((Code::Table { w, pairs }, rest))
            }
            _ => None,
        }
    }

    /// Bytes [`write_to`](Self::write_to) appends.
    pub(crate) fn serialized_len(&self) -> usize {
        match self {
            Code::Frame => 1,
            Code::Table { pairs, .. } => TABLE_HEADER + 2 * pairs.len(),
        }
    }
}

/// One run's header stream as it is written: the prologue the frame needs, then `count` codes at a
/// fixed width.
pub(crate) struct Writer {
    bits: u64,
    used: u32,
    out: Vec<u8>,
    width: u32,
    /// `Some((base_lcp, wl, base_len, wn))` under a frame, `None` under a table.
    frame: Option<(usize, u32, usize, u32)>,
    table: std::collections::HashMap<u16, usize>,
}

impl Writer {
    /// A writer for one run of `pairs` under `code`, its prologue already in the stream.
    pub(crate) fn new(code: &Code, pairs: &[(usize, usize)]) -> Self {
        match code {
            Code::Frame => {
                let (wl, wn, _) = frame_widths(pairs);
                let (bl, bn) = bases(pairs);
                let mut out = Vec::with_capacity(8);
                put_varint(&mut out, bl);
                put_varint(&mut out, bn);
                out.push(((wl as u8) << 4) | wn as u8);
                Self {
                    bits: 0,
                    used: 0,
                    out,
                    width: wl + wn,
                    frame: Some((bl, wl, bn, wn)),
                    table: std::collections::HashMap::new(),
                }
            }
            Code::Table { w, pairs } => Self {
                bits: 0,
                used: 0,
                out: Vec::new(),
                width: *w,
                frame: None,
                table: pairs.iter().enumerate().map(|(i, &p)| (p, i)).collect(),
            },
        }
    }

    /// Codes one pair, appending its escape varints to `sfx` when it does not fit.
    pub(crate) fn push(&mut self, lcp: usize, len: usize, sfx: &mut Vec<u8>) {
        let code = match self.frame {
            Some((bl, wl, bn, wn)) => {
                let (dl, dn) = (lcp - bl, len - bn);
                if frame_fits(dl, dn, wl, wn) {
                    (dl << wn) | dn
                } else {
                    put_varint(sfx, lcp);
                    put_varint(sfx, len);
                    (top(wl) << wn) | top(wn)
                }
            }
            None => {
                let named = (lcp <= FIELD_MAX && len <= FIELD_MAX)
                    .then(|| self.table.get(&(((lcp as u16) << 8) | len as u16)).copied())
                    .flatten();
                match named {
                    Some(i) => i,
                    None => {
                        put_varint(sfx, lcp);
                        put_varint(sfx, len);
                        top(self.width)
                    }
                }
            }
        };
        self.bits |= (code as u64) << self.used;
        self.used += self.width;
        while self.used >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.used -= 8;
        }
    }

    /// Appends the run's header bytes, the last one padded with zeroes.
    pub(crate) fn finish(mut self, out: &mut Vec<u8>) {
        if self.used > 0 {
            self.out.push(self.bits as u8);
        }
        out.extend_from_slice(&self.out);
    }
}

/// One run's header stream as it is read: the pair at any index, at the cost of the two loads its
/// bits span.
pub(crate) struct Reader<'a> {
    hdrs: &'a [u8],
    /// Where the codes start, past the frame's prologue.
    at: usize,
    width: u32,
    frame: Option<(usize, u32, usize, u32)>,
    table: &'a [u16],
}

impl<'a> Reader<'a> {
    /// A reader over nothing, for a walk that has not opened a run yet.
    pub(crate) fn none() -> Self {
        Self {
            hdrs: &[],
            at: 0,
            width: 0,
            frame: None,
            table: &[],
        }
    }

    /// A reader over a run of `count` pairs coded under `code`, and where its suffixes begin. A
    /// stream this crate did not write gives `None` rather than a panic.
    pub(crate) fn of(code: &'a Code, data: &'a [u8], count: usize) -> Option<(Self, &'a [u8])> {
        let (at, width, frame) = match code {
            Code::Frame => {
                let mut at = 0;
                let bl = varint_at(data, &mut at)?;
                let bn = varint_at(data, &mut at)?;
                let &widths = data.get(at)?;
                at += 1;
                let (wl, wn) = (u32::from(widths >> 4), u32::from(widths & 0xF));
                (at, wl + wn, Some((bl, wl, bn, wn)))
            }
            Code::Table { w, .. } => (0, *w, None),
        };
        let end = at + (count * width as usize).div_ceil(8);
        let (hdrs, sfx) = data.split_at_checked(end)?;
        let table = match code {
            Code::Table { pairs, .. } => pairs.as_slice(),
            Code::Frame => &[],
        };
        Some((
            Self {
                hdrs,
                at,
                width,
                frame,
                table,
            },
            sfx,
        ))
    }

    /// The `i`th pair, and whether it escaped — an escaped pair's two varints sit at the head of
    /// its own suffix, so only a walk in order can read them.
    #[inline(always)]
    pub(crate) fn code(&self, i: usize) -> Option<usize> {
        let bit = self.at * 8 + i * self.width as usize;
        let (byte, shift) = (bit / 8, bit % 8);
        let mut word = 0u64;
        let take = self.hdrs.get(byte..)?;
        let n = take.len().min(8);
        word |= u64::from_le_bytes({
            let mut buf = [0u8; 8];
            buf[..n].copy_from_slice(&take[..n]);
            buf
        });
        Some(((word >> shift) & ((1u64 << self.width) - 1)) as usize)
    }

    /// The pair `code` stands for, or `None` when it is the escape.
    #[inline(always)]
    pub(crate) fn pair(&self, code: usize) -> Option<(usize, usize)> {
        match self.frame {
            Some((bl, wl, bn, wn)) => {
                let (dl, dn) = (code >> wn, code & top(wn));
                if (dl, dn) == (top(wl), top(wn)) {
                    return None;
                }
                Some((bl + dl, bn + dn))
            }
            None => {
                let p = *self.table.get(code)?;
                Some((usize::from(p >> 8), usize::from(p & 0xFF)))
            }
        }
    }

    /// Where this run's suffixes start inside its data.
    pub(crate) fn header_bytes(&self) -> usize {
        self.hdrs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes a group's runs and reads every pair back.
    fn round_trip(runs: &[Vec<(usize, usize)>]) -> Code {
        let borrowed: Vec<&[(usize, usize)]> = runs.iter().map(Vec::as_slice).collect();
        let code = Code::choose(&borrowed);
        let mut bytes = Vec::new();
        code.write_to(&mut bytes);
        let (read, rest) = Code::read(&bytes).expect("a group this crate wrote");
        assert!(rest.is_empty());
        assert_eq!(read, code);
        for pairs in runs.iter().filter(|r| !r.is_empty()) {
            let mut writer = Writer::new(&code, pairs);
            let mut sfx = Vec::new();
            for &(lcp, len) in pairs {
                writer.push(lcp, len, &mut sfx);
            }
            let mut data = Vec::new();
            writer.finish(&mut data);
            let headers = data.len();
            data.extend_from_slice(&sfx);
            let (reader, tail) = Reader::of(&code, &data, pairs.len()).expect("our own run");
            assert_eq!(reader.header_bytes(), headers);
            let mut at = 0;
            for (i, &(lcp, len)) in pairs.iter().enumerate() {
                let c = reader.code(i).expect("a code in our own run");
                let got = match reader.pair(c) {
                    Some(pair) => pair,
                    None => {
                        let l = varint_at(tail, &mut at).expect("an escaped lcp");
                        let n = varint_at(tail, &mut at).expect("an escaped len");
                        (l, n)
                    }
                };
                assert_eq!(got, (lcp, len), "pair {i}");
            }
        }
        code
    }

    #[test]
    fn a_run_of_deep_shared_prefixes_takes_the_frame_and_escapes_nothing() {
        let runs = vec![(0..31).map(|i| (100 + i % 4, 7 + i % 3)).collect()];
        let code = round_trip(&runs);
        assert_eq!(code, Code::Frame);
    }

    #[test]
    fn a_run_of_few_distinct_pairs_takes_the_table() {
        let runs: Vec<Vec<(usize, usize)>> = (0..64)
            .map(|_| (0..31).map(|i| (i % 2, 4 + i % 2)).collect())
            .collect();
        let code = round_trip(&runs);
        assert!(matches!(code, Code::Table { .. }), "{code:?}");
    }

    #[test]
    fn pairs_wider_than_a_byte_escape_under_either_code() {
        let runs = vec![vec![(0, 3), (70_000, 9), (1, 300), (2, 2)]];
        round_trip(&runs);
    }

    #[test]
    fn one_pair_a_run_costs_a_prologue_and_no_bits() {
        let runs = vec![vec![(5, 5)]; 8];
        round_trip(&runs);
    }

    #[test]
    fn a_group_of_empty_runs_has_no_table() {
        assert_eq!(Code::choose(&[&[][..], &[][..]]), Code::Frame);
    }

    #[test]
    fn a_cut_short_group_is_refused() {
        let code = Code::Table {
            w: 4,
            pairs: vec![0x0102, 0x0203],
        };
        let mut bytes = Vec::new();
        code.write_to(&mut bytes);
        assert_eq!(bytes.len(), code.serialized_len());
        for cut in 0..bytes.len() {
            assert!(Code::read(&bytes[..cut]).is_none(), "cut at {cut}");
        }
        assert!(
            Code::read(&[1, 11, 0, 0]).is_none(),
            "a width we never write"
        );
        assert!(Code::read(&[2]).is_none(), "a tag we never write");
    }

    #[test]
    fn a_table_whose_count_exceeds_its_width_is_refused() {
        // Width 1 names one pair; a header claiming two is not one of ours.
        assert!(Code::read(&[1, 1, 2, 0, 0, 1, 0, 2]).is_none());
    }
}
