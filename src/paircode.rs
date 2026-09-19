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
    /// Every run's own bases and widths, as offsets from the group's.
    Frame(Frames),
    /// The group carries one table; `pairs[i]` is `(lcp << 8) | len`.
    Table { w: u32, pairs: Vec<u16> },
}

/// How every run of a frame group writes its prologue.
///
/// A run's frame is its own — two bases and two field widths fitted to its fifteen or thirty-one
/// pairs — and spelling all four out cost two varints and a byte, three bytes a run against the
/// six its codes take. Measured over nine corpora at block 256, the prologue was **exactly**
/// three bytes on every run of all but one (`paths`, 3.116): `lcp` and `len` bases under 128
/// everywhere, so neither varint ever reached a second byte.
///
/// Across a group those four numbers barely move — on real words the `lcp` base spans 0 to 14 and
/// the `len` base 1 to 3 — so the group states the four minima once and every run carries four
/// offsets, `p` bits wide in all: 0.70 to 2.39 bytes a run, 23 to 77 % less.
///
/// The offsets are what is stored, so a field too narrow for a run is a *bigger* run and never a
/// wrong one: a clamped base leaves more pairs outside the frame and each of those escapes, which
/// is the path a pair that does not fit already takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frames {
    /// The smallest `lcp` base, `len` base, `lcp` width and `len` width in the group.
    bl: usize,
    bn: usize,
    wl: u32,
    wn: u32,
    /// The mask each of the four offsets is read under, in the order a run writes them. Stored
    /// rather than the widths they were built from: a run decodes its prologue twice a lookup,
    /// and `(1 << w) - 1` four times over cost more than the shifts and the ands together.
    m: [u32; 4],
    /// The bit each offset starts at, and in `sh[4]` the bit the codes do.
    sh: [u8; 5],
}

/// Bits a run's prologue may take, and the widest either base offset may be. One eight-byte load
/// off the run's first byte answers the whole of it, and a base offset that wants more than
/// thirty-two bits — a four-gigabyte spread of shared prefixes across one shard — leaves the group
/// to the table instead.
const PROLOGUE_MAX: u32 = 56;
const OFFSET_MAX: u32 = 32;

/// One run's own frame as the group is fitted over it, with what the pairs it cannot name cost.
#[derive(Clone, Copy)]
struct Fit {
    bl: usize,
    bn: usize,
    wl: u32,
    wn: u32,
    esc: usize,
    n: usize,
}

fn fit_run(pairs: &[(usize, usize)], w: &mut Widths) -> Fit {
    let (wl, wn, esc) = frame_widths(pairs, w);
    let (bl, bn) = bases(pairs);
    Fit {
        bl,
        bn,
        wl,
        wn,
        esc,
        n: pairs.len(),
    }
}

impl Frames {
    /// The layout a group with no runs carries, which no reader ever decodes a prologue under.
    pub(crate) const NONE: Self = Self {
        bl: 0,
        bn: 0,
        wl: 0,
        wn: 0,
        m: [0; 4],
        sh: [0; 5],
    };

    /// The layout of four offsets `p` bits wide over the four bases `lo`, or `None` when they want
    /// more bits than one load can answer.
    fn new(lo: [usize; 4], p: [u32; 4]) -> Option<Self> {
        if p.iter().sum::<u32>() > PROLOGUE_MAX || p[0] > OFFSET_MAX || p[1] > OFFSET_MAX {
            return None;
        }
        let mut sh = [0u8; 5];
        for k in 0..4 {
            sh[k + 1] = sh[k] + p[k] as u8;
        }
        Some(Self {
            bl: lo[0],
            bn: lo[1],
            wl: lo[2] as u32,
            wn: lo[3] as u32,
            m: p.map(|w| ((1u64 << w) - 1) as u32),
            sh,
        })
    }

    /// The layout that covers every run of a group.
    fn over(fits: &[Fit]) -> Option<Self> {
        let first = fits.first()?;
        let mut lo = [first.bl, first.bn, first.wl as usize, first.wn as usize];
        let mut hi = lo;
        for f in fits {
            for (k, v) in [f.bl, f.bn, f.wl as usize, f.wn as usize]
                .into_iter()
                .enumerate()
            {
                lo[k] = lo[k].min(v);
                hi[k] = hi[k].max(v);
            }
        }
        Self::new(lo, [0, 1, 2, 3].map(|k| bits_of(hi[k] - lo[k]) as u32))
    }

    /// Bits every run of the group spends on its prologue.
    #[inline(always)]
    fn bits(&self) -> u32 {
        u32::from(self.sh[4])
    }

    /// The width of each offset, as the group states it in the blob.
    fn widths(&self) -> [u32; 4] {
        self.m.map(u32::count_ones)
    }

    /// What the group costs under this layout: its own bytes, and every run's prologue, codes and
    /// escapes. The prologue and the codes share one rounding because they share the run's bytes.
    fn cost(&self, fits: &[Fit]) -> usize {
        let pro = self.bits() as usize;
        self.serialized_len()
            + fits
                .iter()
                .map(|f| (pro + f.n * (f.wl + f.wn) as usize).div_ceil(8) + f.esc)
                .sum::<usize>()
    }

    fn serialized_len(&self) -> usize {
        varint_len(self.bl) + varint_len(self.bn) + 4
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        let p = self.widths();
        put_varint(out, self.bl);
        put_varint(out, self.bn);
        out.push(((self.wl as u8) << 4) | self.wn as u8);
        out.push(p[0] as u8);
        out.push(p[1] as u8);
        out.push(((p[2] as u8) << 4) | p[3] as u8);
    }

    fn read(bytes: &[u8]) -> Option<(Self, &[u8])> {
        let mut at = 0;
        let bl = varint_at(bytes, &mut at)?;
        let bn = varint_at(bytes, &mut at)?;
        let &[widths, p0, p1, pw] = bytes.get(at..at + 4)? else {
            return None;
        };
        let f = Self::new(
            [bl, bn, usize::from(widths >> 4), usize::from(widths & 0xF)],
            [
                u32::from(p0),
                u32::from(p1),
                u32::from(pw >> 4),
                u32::from(pw & 0xF),
            ],
        )?;
        Some((f, bytes.get(at + 4..)?))
    }
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

/// The widths a run's pairs are cheapest under, and what the pairs they leave outside the frame
/// cost in escape bytes. The prologue is the group's and the header bits fall out of the widths,
/// so neither is the run's to report; both are constant in the search and neither moves its answer.
///
/// Every width pair is priced, but the escapes are summed from a histogram rather than re-walked
/// for each: a pair escapes under `(wl, wn)` when either offset needs more bits than its width
/// gives, or when both offsets are the all-ones values that spell the escape. The first is a
/// suffix sum over the bits each offset needs; the second is a corner one pair can hit at one
/// width pair only, where its offsets are each a power of two less one. So the run is walked once
/// and the 256 width pairs are priced from a 17 × 17 table — where the search walked the run 256
/// times, which was half of a build. The answer is the search's, tie for tie.
fn frame_widths(pairs: &[(usize, usize)], w: &mut Widths) -> (u32, u32, usize) {
    const W: usize = FRAME_MAX as usize + 1;
    let (bl, bn) = bases(pairs);
    // The bits the run's own offsets need. A width past them fits every pair by range and lands
    // none on the escape, so it costs the same escapes as the width one below and more bits —
    // pricing `hl_max + 1` prices every wider frame with it. Fifteen entries of words reach four
    // bits, so this is six rows of a possible seventeen, and clearing all seventeen of the three
    // tables a run was six kilobytes a run and a twentieth of a build.
    let (mut hl_max, mut hn_max) = (0usize, 0usize);
    for &(lcp, len) in pairs {
        hl_max = hl_max.max(bits_of(lcp - bl).min(W));
        hn_max = hn_max.max(bits_of(len - bn).min(W));
    }
    let lh = (hl_max + 1).min(FRAME_MAX as usize);
    let nh = (hn_max + 1).min(FRAME_MAX as usize);
    let (rows, cols) = (lh.max(hl_max), nh.max(hn_max));
    for (esc, corner) in w.esc[..=rows].iter_mut().zip(w.corner[..=rows].iter_mut()) {
        esc[..=cols].fill(0);
        corner[..=cols].fill(0);
    }
    // `esc[hl][hn]`: the escape bytes of the pairs whose offsets need exactly `hl` and `hn`
    // bits; an offset past the widest frame counts under `W`, where no width reaches it.
    let mut total = 0usize;
    for &(lcp, len) in pairs {
        let (dl, dn) = (lcp - bl, len - bn);
        let e = escape_len(lcp, len);
        let (hl, hn) = (bits_of(dl).min(W), bits_of(dn).min(W));
        w.esc[hl][hn] += e;
        total += e;
        if hl < W && hn < W && (dl + 1).is_power_of_two() && (dn + 1).is_power_of_two() {
            w.corner[hl][hn] += e;
        }
    }
    // `fit[wl][wn]`: the escape bytes of every pair both of whose offsets fit those widths by
    // range — the prefix sum of `esc` over `0..=wl` × `0..=wn`.
    for wl in 0..=lh {
        let mut row = 0;
        for wn in 0..=nh {
            row += w.esc[wl][wn];
            w.fit[wl][wn] = row + if wl > 0 { w.fit[wl - 1][wn] } else { 0 };
        }
    }
    let (mut best, mut cheapest) = ((0, 0, 0), usize::MAX);
    for wl in 0..=lh {
        for wn in 0..=nh {
            let bits = pairs.len() * (wl + wn);
            let esc = (total - w.fit[wl][wn]) + w.corner[wl][wn];
            let cost = bits.div_ceil(8) + esc;
            if cost < cheapest {
                (best, cheapest) = ((wl as u32, wn as u32, esc), cost);
            }
        }
    }
    best
}

/// The three tables [`frame_widths`] prices a run's widths over, indexed by the bits an offset
/// needs. The caller owns them because only the rectangle a run reaches is cleared, and a run of
/// fifteen entries reaches a small corner of them.
pub(crate) struct Widths {
    esc: [[usize; GRID]; GRID],
    corner: [[usize; GRID]; GRID],
    fit: [[usize; GRID]; GRID],
}

/// Rows and columns the three tables need: a width from zero to [`FRAME_MAX`], and one past it for
/// the offsets no frame reaches.
const GRID: usize = FRAME_MAX as usize + 2;

impl Default for Widths {
    fn default() -> Self {
        Self {
            esc: [[0; GRID]; GRID],
            corner: [[0; GRID]; GRID],
            fit: [[0; GRID]; GRID],
        }
    }
}

/// Bits an offset needs: none for zero, `floor(log2 d) + 1` otherwise.
#[inline(always)]
fn bits_of(d: usize) -> usize {
    (usize::BITS - d.leading_zeros()) as usize
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
    /// The cheaper of the two codes over `runs`, the runs of one group, with what the winner costs
    /// in bytes: its own, its headers' and the escapes it leaves. A caller pricing one shape of the
    /// data against another needs the number, and it falls out of the same search.
    pub(crate) fn choose_cost(runs: &[&[(usize, usize)]]) -> (Self, usize) {
        let mut w = Widths::default();
        let fits: Vec<Fit> = runs
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| fit_run(r, &mut w))
            .collect();
        let frame = Frames::over(&fits).map(|f| (f, f.cost(&fits)));
        match (Self::best_table(runs), frame) {
            (Some((code, bytes)), Some((_, frame))) if bytes < frame => (code, bytes),
            (Some((code, bytes)), None) => (code, bytes),
            (_, Some((f, cost))) => (Code::Frame(f), cost),
            (None, None) => (Code::Frame(Frames::NONE), 0),
        }
    }

    /// The learned table at the width that codes `runs` smallest, with what it costs including its
    /// own bytes; `None` when no width beats naming nothing.
    pub(crate) fn best_table(runs: &[&[(usize, usize)]]) -> Option<(Self, usize)> {
        // A pair the table can name is two bytes, so its count lives in an array over the key the
        // table stores: the pairs are counted once for every codec priced, and hashing them into a
        // map was most of the count.
        let mut counts = vec![0u32; 1 << 16];
        let mut entries = 0usize;
        for pair in runs.iter().copied().flatten() {
            entries += 1;
            if pair.0 <= FIELD_MAX && pair.1 <= FIELD_MAX {
                counts[(pair.0 << 8) | pair.1] += 1;
            }
        }
        if entries == 0 {
            return None;
        }
        // A pair is worth a table slot for what its escapes would have cost, so the slots go to the
        // pairs whose frequency times escape length is largest, not to the commonest pairs.
        let mut ranked: Vec<((usize, usize), usize)> = counts
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .map(|(key, &n)| {
                let pair = (key >> 8, key & 0xFF);
                (pair, n as usize * escape_len(pair.0, pair.1))
            })
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
            Code::Frame(f) => {
                out.push(0);
                f.write_to(out);
            }
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
            0 => Frames::read(rest).map(|(f, rest)| (Code::Frame(f), rest)),
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
            Code::Frame(f) => 1 + f.serialized_len(),
            Code::Table { pairs, .. } => TABLE_HEADER + 2 * pairs.len(),
        }
    }
}

/// A code as its writers need it: under a table, the slot for every pair the table could name —
/// `(lcp << 8) | len` to slot, `u16::MAX` where it names none. Built once for a group, because a
/// writer is made for every run, and the map of the table each one built cost more than writing
/// the run.
pub(crate) struct Inverse<'a> {
    code: &'a Code,
    slots: Vec<u16>,
}

impl<'a> Inverse<'a> {
    pub(crate) fn of(code: &'a Code) -> Self {
        let mut slots = Vec::new();
        if let Code::Table { pairs, .. } = code {
            slots = vec![u16::MAX; 1 << 16];
            for (i, &p) in pairs.iter().enumerate() {
                slots[usize::from(p)] = i as u16;
            }
        }
        Self { code, slots }
    }
}

/// One run's header stream as it is written: the prologue the frame needs, then `count` codes at a
/// fixed width.
pub(crate) struct Writer<'a> {
    bits: u64,
    used: u32,
    out: Vec<u8>,
    width: u32,
    /// `Some((base_lcp, wl, base_len, wn))` under a frame, `None` under a table.
    frame: Option<(usize, u32, usize, u32)>,
    /// The table's slots by pair under a table, empty under a frame.
    slots: &'a [u16],
}

impl<'a> Writer<'a> {
    /// A writer for one run of `pairs` under `code`, its prologue already in the stream.
    pub(crate) fn new(code: &'a Inverse<'_>, pairs: &[(usize, usize)], w: &mut Widths) -> Self {
        match code.code {
            Code::Frame(f) => {
                let run = fit_run(pairs, w);
                // What the group's fields hold. They were sized over this very run, so the clamp
                // is unreachable on a blob this crate wrote and correct on one it did not: a base
                // cut short leaves pairs outside the frame, and those escape.
                let mut d = [
                    run.bl.saturating_sub(f.bl) as u64,
                    run.bn.saturating_sub(f.bn) as u64,
                    u64::from(run.wl.saturating_sub(f.wl)),
                    u64::from(run.wn.saturating_sub(f.wn)),
                ];
                for (v, m) in d.iter_mut().zip(f.m) {
                    *v = (*v).min(u64::from(m));
                }
                let (bl, bn) = (f.bl + d[0] as usize, f.bn + d[1] as usize);
                let (wl, wn) = (f.wl + d[2] as u32, f.wn + d[3] as u32);
                // The codes carry on in the bit the prologue stopped at, so a group whose runs
                // agree on all four spends nothing at all here.
                let mut acc = 0u64;
                for (k, v) in d.into_iter().enumerate() {
                    acc |= v << f.sh[k];
                }
                let mut used = f.bits();
                let mut out = Vec::with_capacity(8);
                while used >= 8 {
                    out.push(acc as u8);
                    acc >>= 8;
                    used -= 8;
                }
                Self {
                    bits: acc,
                    used,
                    out,
                    width: wl + wn,
                    frame: Some((bl, wl, bn, wn)),
                    slots: &[],
                }
            }
            Code::Table { w, .. } => Self {
                bits: 0,
                used: 0,
                out: Vec::new(),
                width: *w,
                frame: None,
                slots: &code.slots,
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
                let slot = if lcp <= FIELD_MAX && len <= FIELD_MAX {
                    self.slots[(lcp << 8) | len]
                } else {
                    u16::MAX
                };
                if slot == u16::MAX {
                    put_varint(sfx, lcp);
                    put_varint(sfx, len);
                    top(self.width)
                } else {
                    usize::from(slot)
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

/// One run's header stream as it is read: a cursor over its codes, refilled a machine word at a
/// time.
///
/// A word holds eight or so codes at the widths a frame takes, so the load, its bounds check and
/// the shift that aligns it are paid once for all of them rather than once a code. That is most of
/// what a walk over a microblock's headers costs beyond splitting the pairs themselves.
pub(crate) struct Reader<'a> {
    /// The run's whole data — the headers, and past `hdr_end` the suffixes. A header's eight-byte
    /// load may run on into the suffixes, and the mask drops what it took: that keeps it one
    /// unaligned load everywhere but the run's last seven bytes, where a copy of what is left is
    /// the price of not reading past the run.
    data: &'a [u8],
    hdr_end: usize,
    /// The bit the next code starts at.
    bit: usize,
    width: u32,
    /// The all-ones code at `width`: the mask a code is read under, and the escape.
    mask: u64,
    /// Flat rather than an `Option`, so that a walk told which kind it is reading at compile time
    /// carries neither the discriminant nor a branch on it; zero under a table.
    frame: Frame,
    is_frame: bool,
    table: &'a [u16],
}

/// A frame's two bases and what splits a code into their offsets, arranged the way a header reads
/// them: the field width is the shift, and the escape is the all-ones code the mask already holds.
#[derive(Clone, Copy)]
struct Frame {
    bl: usize,
    bn: usize,
    wn: u32,
    mask_n: usize,
}

impl Frame {
    const NONE: Self = Self {
        bl: 0,
        bn: 0,
        wn: 0,
        mask_n: 0,
    };
}

impl<'a> Reader<'a> {
    /// A reader over nothing, for a walk that has not opened a run yet.
    pub(crate) fn none() -> Self {
        Self {
            data: &[],
            hdr_end: 0,
            bit: 0,
            width: 0,
            mask: 0,
            frame: Frame::NONE,
            is_frame: false,
            table: &[],
        }
    }

    /// A reader over a run of `count` pairs coded under `code`, and where its suffixes begin. A
    /// stream this crate did not write gives `None` rather than a panic.
    ///
    /// Inlined on purpose: a climb opens two runs and a scan one, so the prologue is paid once a
    /// lookup, and out of line it was a call through the got with the code's kind unknown to the
    /// walk that followed.
    #[inline]
    pub(crate) fn of(code: &'a Code, data: &'a [u8], count: usize) -> Option<(Self, &'a [u8])> {
        let (at, width, frame) = match code {
            Code::Frame(f) => {
                let bits = f.bits();
                if data.len() * 8 < bits as usize {
                    return None;
                }
                let word = match data.first_chunk::<8>() {
                    Some(w) => u64::from_le_bytes(*w),
                    None => tail_word(data, 0),
                };
                let take = |k: usize| (word >> f.sh[k]) & u64::from(f.m[k]);
                let (dbl, dbn) = (take(0) as usize, take(1) as usize);
                let (wl, wn) = (f.wl + take(2) as u32, f.wn + take(3) as u32);
                if wl > FRAME_MAX || wn > FRAME_MAX {
                    return None;
                }
                let frame = Frame {
                    bl: f.bl + dbl,
                    bn: f.bn + dbn,
                    wn,
                    mask_n: top(wn),
                };
                (bits as usize, wl + wn, Some(frame))
            }
            Code::Table { w, .. } => (0, *w, None),
        };
        let is_frame = frame.is_some();
        let frame = frame.unwrap_or(Frame::NONE);
        let end = (at + count * width as usize).div_ceil(8);
        let sfx = data.get(end..)?;
        let table = match code {
            Code::Table { pairs, .. } => pairs.as_slice(),
            Code::Frame(_) => &[],
        };
        Some((
            Self {
                data,
                hdr_end: end,
                bit: at,
                width,
                mask: (1u64 << width) - 1,
                frame,
                is_frame,
                table,
            },
            sfx,
        ))
    }

    /// The next code. A walk keeps the cursor under the headers' end by counting entries, and a
    /// code past it reads whatever the suffixes hold rather than failing — which is the same
    /// answer a wrong count gave when every code was addressed on its own.
    #[inline(always)]
    pub(crate) fn next_code(&mut self) -> usize {
        let bit = self.bit;
        self.bit = bit + self.width as usize;
        let (byte, shift) = (bit / 8, (bit % 8) as u32);
        let word = match self.data.get(byte..byte + 8) {
            Some(w) => u64::from_le_bytes(w.try_into().expect("eight bytes")),
            None => tail_word(self.data, byte),
        };
        ((word >> shift) & self.mask) as usize
    }

    /// The pair `code` stands for, or `None` when it is the escape. Only a caller that does not
    /// already know the kind — a test, or a walk of one header — asks this way.
    #[cfg(test)]
    ///
    /// A frame's escape is its two all-ones offsets side by side, which is the all-ones code at
    /// the width — the mask the code was read under — so the test is one comparison against a
    /// value the read already had, whatever the two field widths are.
    #[inline(always)]
    fn pair(&self, code: usize) -> Option<(usize, usize)> {
        if self.is_frame {
            self.pair_as::<true>(code)
        } else {
            self.pair_as::<false>(code)
        }
    }

    /// Whether the codes are a frame's offsets rather than a table's indices, which is one answer
    /// for the whole run and therefore one a walk asks before it starts.
    #[inline(always)]
    pub(crate) fn is_frame(&self) -> bool {
        self.is_frame
    }

    /// [`pair`](Self::pair) with the kind known at compile time. A table's walk then holds none of
    /// the frame's four words and a frame's walk none of the table's two, which is what the header
    /// loop had no registers for.
    #[inline(always)]
    pub(crate) fn pair_as<const FRAME: bool>(&self, code: usize) -> Option<(usize, usize)> {
        if FRAME {
            if code == self.mask as usize {
                return None;
            }
            let f = self.frame;
            Some((f.bl + (code >> f.wn), f.bn + (code & f.mask_n)))
        } else {
            let p = *self.table.get(code)?;
            Some((usize::from(p >> 8), usize::from(p & 0xFF)))
        }
    }

    /// Where this run's suffixes start inside its data.
    pub(crate) fn header_bytes(&self) -> usize {
        self.hdr_end
    }
}

/// The last bytes of a run as a word, zero-padded: the load a header takes when eight bytes do
/// not remain. `byte` is inside `data`.
#[cold]
#[inline(never)]
fn tail_word(data: &[u8], byte: usize) -> u64 {
    let mut buf = [0u8; 8];
    let take = &data[byte.min(data.len())..];
    let n = take.len().min(8);
    buf[..n].copy_from_slice(&take[..n]);
    u64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes a group's runs and reads every pair back.
    fn round_trip(runs: &[Vec<(usize, usize)>]) -> Code {
        let borrowed: Vec<&[(usize, usize)]> = runs.iter().map(Vec::as_slice).collect();
        let code = Code::choose_cost(&borrowed).0;
        let mut bytes = Vec::new();
        code.write_to(&mut bytes);
        let (read, rest) = Code::read(&bytes).expect("a group this crate wrote");
        assert!(rest.is_empty());
        assert_eq!(read, code);
        let inverse = Inverse::of(&code);
        for pairs in runs.iter().filter(|r| !r.is_empty()) {
            let mut writer = Writer::new(&inverse, pairs, &mut Widths::default());
            let mut sfx = Vec::new();
            for &(lcp, len) in pairs {
                writer.push(lcp, len, &mut sfx);
            }
            let mut data = Vec::new();
            writer.finish(&mut data);
            let headers = data.len();
            data.extend_from_slice(&sfx);
            let (mut reader, tail) = Reader::of(&code, &data, pairs.len()).expect("our own run");
            assert_eq!(reader.header_bytes(), headers);
            let mut at = 0;
            for (i, &(lcp, len)) in pairs.iter().enumerate() {
                let c = reader.next_code();
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
        assert!(matches!(code, Code::Frame(_)), "{code:?}");
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
        assert_eq!(
            Code::choose_cost(&[&[][..], &[][..]]).0,
            Code::Frame(Frames::NONE)
        );
    }

    #[test]
    fn a_group_whose_runs_agree_spends_no_prologue() {
        let fits: Vec<Fit> = (0..8)
            .map(|_| fit_run(&[(9, 4), (9, 5), (10, 4)], &mut Widths::default()))
            .collect();
        let f = Frames::over(&fits).expect("eight runs of the same shape");
        assert_eq!(f.bits(), 0, "{f:?}");
        assert_eq!(f.widths(), [0; 4]);
        // Every run's prologue is then the run's first code, and the codes start at bit zero.
        assert_eq!(f.cost(&fits), f.serialized_len() + 8 * fits[0].esc + 8);
    }

    #[test]
    fn a_layout_wider_than_one_load_is_refused() {
        // A base offset past `OFFSET_MAX`, and four offsets past `PROLOGUE_MAX` together.
        assert!(Frames::new([0; 4], [OFFSET_MAX + 1, 0, 0, 0]).is_none());
        assert!(Frames::new([0; 4], [30, 30, 4, 4]).is_none());
        let f = Frames::new([7, 2, 1, 1], [5, 3, 2, 2]).expect("a layout one load answers");
        assert_eq!(f.bits(), 12);
        assert_eq!(f.widths(), [5, 3, 2, 2]);
        let mut bytes = Vec::new();
        Code::Frame(f).write_to(&mut bytes);
        assert_eq!(Code::read(&bytes).expect("our own group").0, Code::Frame(f));
        // The same bytes with a base offset of forty bits: refused where they are read, so a blob
        // claiming it never reaches a run.
        let at = bytes.len() - 3;
        bytes[at] = 40;
        assert!(Code::read(&bytes).is_none());
    }

    #[test]
    fn a_run_claiming_a_width_no_frame_has_is_refused() {
        // `wl` is at the cap and the run's offset is all ones, so the two add past it.
        let f = Frames::new([0, 0, FRAME_MAX as usize, 0], [0, 0, 4, 0]).expect("a layout");
        assert!(Reader::of(&Code::Frame(f), &[0xFF; 8], 4).is_none());
    }

    #[test]
    fn a_run_outside_its_group_escapes_rather_than_lying() {
        // The group is fitted over short prefixes; the run written under it starts far past them,
        // so its base clamps to what the field holds and every pair falls outside the frame.
        let code = Code::Frame(Frames::new([1, 2, 1, 1], [2, 2, 2, 2]).expect("a layout"));
        let stranger = [(900, 40), (901, 41), (902, 42)];
        let inverse = Inverse::of(&code);
        let mut writer = Writer::new(&inverse, &stranger, &mut Widths::default());
        let mut sfx = Vec::new();
        for &(lcp, len) in &stranger {
            writer.push(lcp, len, &mut sfx);
        }
        let mut data = Vec::new();
        writer.finish(&mut data);
        data.extend_from_slice(&sfx);
        let (mut reader, tail) = Reader::of(&code, &data, stranger.len()).expect("our own run");
        let mut at = 0;
        for &(lcp, len) in &stranger {
            let c = reader.next_code();
            assert!(reader.pair(c).is_none(), "a stranger's pair was named");
            assert_eq!(
                (
                    varint_at(tail, &mut at).expect("lcp"),
                    varint_at(tail, &mut at).expect("len")
                ),
                (lcp, len)
            );
        }
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
