//! A block's bytes: the codecs, the entry stream, and the compares that walk it.

use super::*;

/// How one shard's suffixes are coded: a symbol table, with or without a share of its byte codes
/// given to the blob's [phrases](crate::phrase), or a fixed-width code over its alphabet.
///
/// The three are priced against each other on the shard's own bytes — its suffixes and the headers
/// they imply, since a packed `len` counts codes and a symbol-coded one counts bytes. A shard of
/// DNA picks the packed code, a shard of UUIDs the table alone and a shard of URLs the table with
/// phrases, in the same blob if the corpus holds all three.
pub(crate) enum Codec {
    Symbols { table: Table, split: Option<Split> },
    Packed(Alphabet),
}

impl Codec {
    /// Bits one unit of a coded suffix takes, which is what a header's `len` counts in.
    #[inline(always)]
    pub(super) fn unit(&self) -> u32 {
        match self {
            Codec::Symbols { .. } => 8,
            Codec::Packed(a) => a.width(),
        }
    }

    /// Appends the bytes `piece` stands for to `out`, stopping once `cap` of them are there;
    /// `false` on a stream this crate did not write.
    ///
    /// A climb wants a whole suffix only from the last stair it kept: every earlier one is
    /// truncated again at the next stair's shared prefix, so the bytes past it are decoded and
    /// thrown away. Handing the cap down is what keeps them undecoded — and keeps the room the
    /// fixed-width stores need from being sized by the coded suffix rather than by the answer.
    #[inline]
    pub(super) fn decode_into(
        &self,
        dict: &Dict,
        piece: Piece<'_>,
        out: &mut Vec<u8>,
        cap: usize,
    ) -> bool {
        match self {
            Codec::Symbols {
                table,
                split: Some(split),
            } if dict.has_labels() => decode_labelled(table, *split, dict, piece.bytes(), out, cap),
            Codec::Symbols {
                table,
                split: Some(split),
            } => decode_tiered(table, *split, dict, piece.bytes(), out, cap),
            Codec::Symbols { table, .. } => table.decode_into(piece.bytes(), out, cap),
            Codec::Packed(a) => a.decode_into(&piece.codes(a.width()), out, cap),
        }
    }

    /// `[kind u8][payload]`, behind the `u32` length the tables section gives every shard. A split
    /// is two more bytes at the head of the payload, and a kind of its own so that a shard which
    /// bought no phrases pays nothing for the ones that did.
    pub(super) fn serialized_len(&self) -> usize {
        1 + match self {
            Codec::Symbols { table, split } => {
                table.serialized_len() + 2 * usize::from(split.is_some())
            }
            Codec::Packed(a) => a.serialized_len(),
        }
    }

    pub(super) fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            Codec::Symbols { table, split } => {
                match split {
                    Some(split) => {
                        out.push(2);
                        split.write_to(out);
                    }
                    None => out.push(0),
                }
                table.write_to(out);
            }
            Codec::Packed(a) => {
                out.push(1);
                a.write_to(out);
            }
        }
    }

    /// Whether the shard gave any of its byte codes to the blob's phrases.
    pub(super) fn has_phrases(&self) -> bool {
        matches!(self, Codec::Symbols { split: Some(_), .. })
    }

    pub(super) fn read(bytes: &[u8]) -> Option<Self> {
        match bytes.split_first()? {
            (0, rest) => Table::from_bytes(rest).map(|table| Codec::Symbols { table, split: None }),
            (1, rest) => Alphabet::read(rest).map(Codec::Packed),
            (2, rest) => {
                let (split, rest) = Split::read(rest)?;
                let table = Table::from_bytes(rest)?;
                // A symbol code past the split would be read as a phrase prefix.
                (table.len() <= usize::from(split.symbols())).then_some(Codec::Symbols {
                    table,
                    split: Some(split),
                })
            }
            _ => None,
        }
    }
}

/// One entry's coded suffix inside its run's stream: where it starts, in bits, and how many units
/// it holds. A symbol-coded run starts every entry on a byte; a packed one does not, which is the
/// last third of a byte a key on a corpus of four characters.
#[derive(Clone, Copy)]
pub(super) struct Piece<'a> {
    pub(super) stream: &'a [u8],
    pub(super) start: usize,
    pub(super) units: usize,
}

impl<'a> Piece<'a> {
    /// The bytes of a symbol-coded piece, bounded by what the stream holds.
    ///
    /// `start` is a bit inside the stream and `units` no more than the stream is long, so the sum
    /// is under twice an address and the saturating form only cost the compare that proves it.
    #[inline(always)]
    pub(super) fn bytes(self) -> &'a [u8] {
        let at = self.start / 8;
        let end = (at + self.units).min(self.stream.len());
        self.stream.get(at..end).unwrap_or_default()
    }

    #[inline(always)]
    fn codes(self, width: u32) -> packed::Codes<'a> {
        packed::Codes::new(self.stream, self.start, width, self.units)
    }

    /// Units the stream actually holds of this piece, which is fewer than asked for only on a
    /// stream this crate did not write.
    #[inline(always)]
    pub(super) fn units(self, unit: u32) -> usize {
        let left = (self.stream.len() * 8).saturating_sub(self.start);
        self.units.min(left / unit as usize)
    }
}

/// [`DictIndex::shard_bits`] for a shard of `shard` blocks.
pub(super) fn shard_bits_of(shard: usize) -> u32 {
    if shard.is_power_of_two() {
        shard.trailing_zeros()
    } else {
        u32::MAX
    }
}

/// One block's two streams, read in order. Every entry's header is the same number of bits, and the
/// group's code says how many, so the entry count says where the headers end and the suffixes
/// begin: the split is derived, not stored.
///
/// Splitting them is what makes a scan cheap. A scan rules most entries out by their shared-prefix
/// length alone, which lives in the header — interleaved, those 127 bytes were spread over the
/// seven cache lines of a 128-key block, and here they are two.
pub(super) struct Entries<'a> {
    codes: paircode::Reader<'a>,
    pub(super) sfx: &'a [u8],
    at: usize,
    pub(super) count: usize,
    /// Where the suffixes read so far end, in bits and unbounded by the stream: ruling an entry
    /// out by its header moves it, and the clamp is paid by the few entries that are read.
    reach: usize,
    /// The suffixes' length in bits, which the cursor is clamped to, and in units, which no
    /// entry's length may pass: checked once where a header is decoded, so the cursor's own
    /// arithmetic cannot leave the run and needs no saturating step of its own.
    pub(super) end_bits: usize,
    max_units: usize,
    unit: u32,
}

/// One front-coded run as a climb addresses it: its group's code, its shard's codec, its bytes
/// and how many keys it holds.
#[derive(Clone, Copy)]
pub(super) struct CodedRun<'a> {
    pub(super) codec: &'a Codec,
    pub(super) code: &'a Code,
    pub(super) data: &'a [u8],
    pub(super) count: usize,
}

impl<'a> CodedRun<'a> {
    #[inline]
    pub(super) fn entries(&self) -> Entries<'a> {
        Entries::of(self.code, self.codec, self.data, self.count)
    }
}

impl<'a> Entries<'a> {
    /// The entries of a run of `count` keys — `count - 1` headers — under its group's `code` and
    /// its shard's `codec`. A run this crate did not write reads as an empty one rather than
    /// panicking.
    #[inline(always)]
    pub(super) fn of(code: &'a Code, codec: &Codec, data: &'a [u8], count: usize) -> Self {
        let entries = count.saturating_sub(1);
        match paircode::Reader::of(code, data, entries) {
            Some((codes, sfx)) => Self {
                codes,
                sfx,
                at: 0,
                count: entries,
                reach: 0,
                end_bits: sfx.len().saturating_mul(8),
                max_units: match codec.unit() {
                    8 => sfx.len(),
                    unit => sfx.len().saturating_mul(8) / unit as usize,
                },
                unit: codec.unit(),
            },
            None => Self::empty(),
        }
    }

    /// A run with nothing in it, which every walk ends on at once.
    #[inline]
    pub(super) fn empty() -> Self {
        Self {
            codes: paircode::Reader::none(),
            sfx: &[],
            at: 0,
            count: 0,
            reach: 0,
            end_bits: 0,
            max_units: 0,
            unit: 8,
        }
    }

    /// Whether the run's pairs are a frame's offsets rather than a table's indices — what a walk
    /// asks once so that [`head_as`](Self::head_as) does not ask it a header at a time.
    #[inline(always)]
    pub(super) fn is_frame(&self) -> bool {
        self.codes.is_frame()
    }

    /// The run past its first `n` entries, for a walk that knows they are below what it looks for.
    /// A run this crate did not write may read as an empty one.
    #[inline]
    pub(super) fn pass(&mut self, n: usize) {
        for _ in 0..n {
            let Some((_, len)) = self.head() else {
                *self = Self::empty();
                return;
            };
            self.skip(len);
        }
        self.count -= n;
        self.at = 0;
    }

    /// The next entry's shared-prefix length and suffix length, leaving the suffix itself for
    /// [`piece`](Self::piece) or [`skip`](Self::skip); `None` past the last header.
    #[inline(always)]
    pub(super) fn head(&mut self) -> Option<(usize, usize)> {
        if self.is_frame() {
            self.head_as::<true>()
        } else {
            self.head_as::<false>()
        }
    }

    /// [`head`](Self::head) for a run whose code kind the caller already knows.
    #[inline(always)]
    fn head_as<const FRAME: bool>(&mut self) -> Option<(usize, usize)> {
        if self.at >= self.count {
            return None;
        }
        self.at += 1;
        self.head_within::<FRAME>()
    }

    /// The next header, counted by the caller. Reading past a run's headers reads its suffixes,
    /// which is a wrong pair rather than a wrong address: the codes are bounded by the run's data
    /// and the walk that took them is refused by the one cursor check that follows it.
    #[inline(always)]
    pub(super) fn head_within<const FRAME: bool>(&mut self) -> Option<(usize, usize)> {
        let code = self.codes.next_code();
        let (lcp, len) = match self.codes.pair_as::<FRAME>(code) {
            Some(pair) => pair,
            None => {
                // A pair no code could name continues as two varints at the head of its own
                // suffix, on a byte, so that they read the same whatever width the codes are.
                let mut at = self.off().div_ceil(8);
                let lcp = varint_at(self.sfx, &mut at)?;
                let len = varint_at(self.sfx, &mut at)?;
                self.reach = at * 8;
                (lcp, len)
            }
        };
        (len <= self.max_units).then_some((lcp, len))
    }

    /// Where the next entry's coded suffix starts, in bits — a multiple of eight under a symbol
    /// table, and any bit under a packed alphabet. A stream this crate did not write ends it at
    /// the end of the suffixes.
    #[inline(always)]
    fn off(&self) -> usize {
        // `head` refuses a length the run cannot hold, so one entry's bits are within the
        // suffixes and a run's worth of them within a thousand times that; the clamp is what a
        // run this crate did not write runs into, and the wrapping is so that a blob claiming a
        // run longer than the address space gives a wrong piece on a 32-bit target rather than a
        // panic -- which is what every other bound here does.
        self.reach().min(self.end_bits)
    }

    /// Where the suffixes read so far end, unbounded by the stream. It only grows, so a walk that
    /// would have stopped at any entry past the stream stops on this one bound at the end of it.
    #[inline(always)]
    pub(super) fn reach(&self) -> usize {
        self.reach
    }

    /// The suffix of the entry [`head`](Self::head) just read. A stream this crate did not write
    /// gives a short piece rather than a panic.
    #[inline(always)]
    pub(super) fn piece(&mut self, len: usize) -> Piece<'a> {
        let piece = self.at_off(len);
        self.skip(len);
        piece
    }

    /// The piece `len` units long that starts where the cursor is, without moving it.
    #[inline(always)]
    fn at_off(&self, len: usize) -> Piece<'a> {
        Piece {
            stream: self.sfx,
            start: self.off(),
            units: len,
        }
    }

    /// Past that suffix without reading it — what a scan does for every entry its header rules
    /// out, and the reason the two streams are apart.
    #[inline(always)]
    pub(super) fn skip(&mut self, len: usize) {
        self.reach = self
            .reach
            .wrapping_add(len.wrapping_mul(self.unit as usize));
    }
}

/// One front-coded run's bytes, told apart: [`DictSections`] sums these over every run in the blob.
#[derive(Default)]
pub(super) struct RunSplit {
    pub(super) headers: u64,
    pub(super) wide_bytes: u64,
    pub(super) codes: u64,
    pub(super) entries: u64,
    pub(super) wide: u64,
}

/// Split one run of `keys` front-coded keys into its header bytes, the `(lcp, len)` varints of the
/// entries its code could not name, and the symbol-coded suffixes.
///
/// The three always sum to `data`, because the codes are what is left rather than what was walked:
/// a stream this crate did not write ends the walk early and leaves the split approximate, not the
/// total wrong, which is the way round an accounting tool wants it.
pub(super) fn split_run(code: &Code, codec: &Codec, data: &[u8], keys: usize) -> RunSplit {
    let mut entries = Entries::of(code, codec, data, keys);
    let mut split = RunSplit {
        headers: entries.codes.header_bytes() as u64,
        ..RunSplit::default()
    };
    let suffixes = entries.sfx.len() as u64;
    loop {
        let before = entries.off();
        let Some((_, len)) = entries.head() else {
            break;
        };
        split.entries += 1;
        if entries.off() != before {
            // The varints start on the byte the cursor was in and end on one, whatever bit the
            // codes before them stopped at.
            split.wide_bytes += (entries.off() / 8 - before.div_ceil(8)) as u64;
            split.wide += 1;
        }
        entries.skip(len);
    }
    split.codes = suffixes.saturating_sub(split.wide_bytes);
    split
}

/// What a compare has left of the probe: the whole key, and the byte the compare starts at.
///
/// A suffix slice would name the same bytes, and read them differently: the last three bytes of a
/// three-byte slice are a fold a byte at a time, where the key's last eight bytes are one load,
/// taken once a walk as `tail` and shifted down. A compare starts where the probe already agrees
/// with the key before it -- 6.3 bytes into a 9.3-byte word on the dictionary -- so the slice is
/// under eight bytes for most compares and the key it came from never is.
#[derive(Clone, Copy)]
pub(super) struct Rest<'a> {
    key: &'a [u8],
    at: usize,
    /// [`tail_word`] of `key`, read once a walk rather than once a compare.
    tail: u64,
}

/// A key's last eight bytes as a little-endian word, or the whole key zero-padded when it is
/// shorter: every byte a compare can reach past the last whole word of the key.
#[inline]
fn tail_word(key: &[u8]) -> u64 {
    match key.last_chunk::<8>() {
        Some(w) => u64::from_le_bytes(*w),
        None => key
            .iter()
            .enumerate()
            .fold(0, |w, (k, &b)| w | u64::from(b) << (8 * k)),
    }
}

impl<'a> Rest<'a> {
    /// Bytes of the probe still to compare. `at` never passes the key: it starts at what the run's
    /// head shares with the probe and grows only by what a compare matched.
    #[inline(always)]
    fn len(self) -> usize {
        self.key.len() - self.at
    }

    /// The eight bytes at `at + c`, zero-padded past the key: one load where eight remain, and the
    /// key's tail shifted down where they do not.
    #[inline(always)]
    fn word(self, c: usize) -> u64 {
        let i = self.at + c;
        if let Some(w) = self.key.get(i..).and_then(|s| s.first_chunk::<8>()) {
            return u64::from_le_bytes(*w);
        }
        // Past the last whole word, so `i` is above `key.len() - 8`, or the key is under eight
        // bytes and the tail is all of it.
        let shift = 8 * (i - self.key.len().saturating_sub(8));
        self.tail.checked_shr(shift as u32).unwrap_or(0)
    }

    /// The suffix itself, for a codec that compares a byte at a time and gains nothing by the
    /// whole key.
    #[inline(always)]
    pub(super) fn bytes(self) -> &'a [u8] {
        &self.key[self.at..]
    }
}

/// The stored suffix's next `len` bytes, as a little-endian word, against `rest` from `c`: `None`
/// while they agree, and the answer as soon as they do not or the suffix outruns `rest`.
#[inline(always)]
fn compare_word(word: u64, len: usize, rest: Rest<'_>, c: &mut usize) -> Option<(usize, Ordering)> {
    let m = len.min(rest.len() - *c);
    let theirs = rest.word(*c);
    let x = (word ^ theirs) & fsst::low_mask(m);
    if x != 0 {
        let d = (x.trailing_zeros() / 8) as usize;
        let (a, b) = ((word >> (8 * d)) as u8, (theirs >> (8 * d)) as u8);
        return Some((*c + d, a.cmp(&b)));
    }
    *c += m;
    (m < len).then_some((*c, Ordering::Greater))
}

/// How a scan compares one entry's coded suffix with the probe: the shard's codec, told apart once
/// a run rather than once an entry, so that each codec's walk is a loop of its own.
pub(super) trait SuffixCmp {
    /// Bits one unit of a coded suffix takes, which is what a header's `len` counts in.
    fn unit(&self) -> usize;
    fn compare(&self, piece: Piece<'_>, rest: Rest<'_>) -> (usize, Ordering);
}

pub(super) struct SymbolsCmp<'a>(pub(super) &'a Table);

pub(super) struct TieredCmp<'a> {
    pub(super) table: &'a Table,
    pub(super) split: Split,
    pub(super) dict: &'a Dict,
}

/// [`TieredCmp`] over a dictionary that names trie labels past its phrases.
pub(super) struct LabelledCmp<'a> {
    pub(super) table: &'a Table,
    pub(super) split: Split,
    pub(super) dict: &'a Dict,
}

pub(super) struct PackedCmp<'a>(pub(super) &'a Alphabet);

impl SuffixCmp for SymbolsCmp<'_> {
    #[inline(always)]
    fn unit(&self) -> usize {
        8
    }

    #[inline(always)]
    fn compare(&self, piece: Piece<'_>, rest: Rest<'_>) -> (usize, Ordering) {
        compare_symbols(self.0, piece.bytes(), rest)
    }
}

impl SuffixCmp for TieredCmp<'_> {
    #[inline(always)]
    fn unit(&self) -> usize {
        8
    }

    #[inline(always)]
    fn compare(&self, piece: Piece<'_>, rest: Rest<'_>) -> (usize, Ordering) {
        compare_tiered(self.table, self.split, self.dict, piece.bytes(), rest)
    }
}

impl SuffixCmp for LabelledCmp<'_> {
    #[inline(always)]
    fn unit(&self) -> usize {
        8
    }

    #[inline(always)]
    fn compare(&self, piece: Piece<'_>, rest: Rest<'_>) -> (usize, Ordering) {
        compare_labelled(self.table, self.split, self.dict, piece.bytes(), rest)
    }
}

impl SuffixCmp for PackedCmp<'_> {
    #[inline(always)]
    fn unit(&self) -> usize {
        self.0.width() as usize
    }

    #[inline(always)]
    fn compare(&self, piece: Piece<'_>, rest: Rest<'_>) -> (usize, Ordering) {
        self.0.compare(&piece.codes(self.0.width()), rest.bytes())
    }
}

/// [`DictIndex::scan_run`] with the header code's kind told apart as well.
#[inline(always)]
pub(super) fn scan_as<C: SuffixCmp>(
    frame: bool,
    entries: &Entries<'_>,
    count: usize,
    probe: &[u8],
    matched: usize,
    cmp: &C,
) -> (usize, usize, bool) {
    if frame {
        scan_with::<true, C>(entries, count, probe, matched, cmp)
    } else {
        scan_with::<false, C>(entries, count, probe, matched, cmp)
    }
}

/// The walk itself, for one kind of header code and one codec: the run's cursor lives in locals,
/// so that the loop holds the few words a header needs and nothing a compare does.
#[inline(never)]
fn scan_with<const FRAME: bool, C: SuffixCmp>(
    entries: &Entries<'_>,
    count: usize,
    probe: &[u8],
    mut matched: usize,
    cmp: &C,
) -> (usize, usize, bool) {
    let mut codes = entries.codes;
    let sfx = entries.sfx;
    let mut reach = entries.reach;
    let tail = tail_word(probe);
    let (end_bits, max_units, unit) = (entries.end_bits, entries.max_units, cmp.unit());
    for j in 1..count {
        let code = codes.next_code();
        let (l, len) = match codes.pair_as::<FRAME>(code) {
            Some(pair) => pair,
            None => match escaped(sfx, reach.min(end_bits)) {
                Some((l, len, at)) => {
                    reach = at;
                    (l, len)
                }
                None => return (j - 1, matched, false),
            },
        };
        if len > max_units || l < matched {
            return (j - 1, matched, false);
        }
        let start = reach.min(end_bits);
        reach = reach.wrapping_add(len.wrapping_mul(unit));
        if l > matched {
            continue;
        }
        let piece = Piece {
            stream: sfx,
            start,
            units: len,
        };
        let (c, ord) = cmp.compare(
            piece,
            Rest {
                key: probe,
                at: matched,
                tail,
            },
        );
        match ord {
            Ordering::Equal => return (j, matched + c, true),
            Ordering::Greater => return (j - 1, matched, false),
            Ordering::Less => matched += c,
        }
    }
    (count.saturating_sub(1), matched, false)
}

/// The pair an escape stands for — two varints at the head of the entry's own suffix, on a byte —
/// and the bit its suffix proper starts at.
#[cold]
#[inline(never)]
fn escaped(sfx: &[u8], off: usize) -> Option<(usize, usize, usize)> {
    let mut at = off.div_ceil(8);
    let lcp = varint_at(sfx, &mut at)?;
    let len = varint_at(sfx, &mut at)?;
    Some((lcp, len, at * 8))
}

/// How a suffix that ran out against `rest` orders: equal only if `rest` ran out with it.
#[inline(always)]
fn ended(c: usize, rest: Rest<'_>) -> (usize, Ordering) {
    let ord = if c == rest.len() {
        Ordering::Equal
    } else {
        Ordering::Less
    };
    (c, ord)
}

/// [`SuffixCmp::compare`] for a symbol table: a symbol is up to eight bytes, so the compare is a word
/// at a time rather than a byte.
#[inline]
fn compare_symbols(table: &Table, packed: &[u8], rest: Rest<'_>) -> (usize, Ordering) {
    let (words, lens) = table.parts();
    let mut c = 0;
    let mut i = 0;
    while let Some(&code) = packed.get(i) {
        let k = usize::from(code);
        let (word, len) = if k < words.len() {
            i += 1;
            (words[k], usize::from(lens[k]))
        } else if code == ESCAPE {
            let Some(&b) = packed.get(i + 1) else {
                break;
            };
            i += 2;
            (u64::from(b), 1)
        } else {
            break;
        };
        if let Some(answer) = compare_word(word, len, rest, &mut c) {
            return answer;
        }
    }
    ended(c, rest)
}

/// [`compare_symbols`] where some of the byte codes name phrases instead: the same walk, with the
/// phrase's bytes compared eight at a time. Kept apart from the plain loop rather than branching
/// inside it — a shard that bought no phrases is the common one, and this is the inner loop of
/// every lookup.
#[inline(always)]
fn compare_tiered(
    table: &Table,
    split: Split,
    dict: &Dict,
    packed: &[u8],
    rest: Rest<'_>,
) -> (usize, Ordering) {
    // The codes below `symbols` are the table's, so the common code is told apart by one compare
    // against a bound the loop holds, and a phrase prefix by what is left.
    let (words, lens) = table.parts();
    let n = words.len().min(usize::from(split.symbols()));
    let (words, lens) = (&words[..n], &lens[..n]);
    let mut c = 0;
    let mut i = 0;
    while let Some(&code) = packed.get(i) {
        let k = usize::from(code);
        if k < words.len() {
            i += 1;
            if let Some(answer) = compare_word(words[k], usize::from(lens[k]), rest, &mut c) {
                return answer;
            }
            continue;
        }
        if code == ESCAPE {
            let Some(&b) = packed.get(i + 1) else {
                break;
            };
            i += 2;
            if let Some(answer) = compare_word(u64::from(b), 1, rest, &mut c) {
                return answer;
            }
            continue;
        }
        let Some((id, took)) = split.read_at(packed, i) else {
            break;
        };
        // The padded chunk, not the phrase's own bytes: its last word is a whole load off the
        // padding, where a slice of its length would have been assembled a byte at a time.
        let Some((phrase, plen)) = dict.chunk(id) else {
            break;
        };
        i += took;
        let mut k = 0;
        for word in phrase.chunks_exact(8) {
            if k >= plen {
                break;
            }
            let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            if let Some(answer) = compare_word(word, (plen - k).min(8), rest, &mut c) {
                return answer;
            }
            k += 8;
        }
    }
    ended(c, rest)
}

/// [`Codec::decode_into`] where some of the byte codes name phrases.
#[inline]
fn decode_tiered(
    table: &Table,
    split: Split,
    dict: &Dict,
    packed: &[u8],
    out: &mut Vec<u8>,
    cap: usize,
) -> bool {
    // Room for the widest reading of every code, so that each one is a store of a width the
    // compiler knows rather than a `memcpy` call with a length it learns at run time -- which is
    // what a symbol-only decode has always done, and what this one did not: on a million urls the
    // key lane spent 69 ns a lookup inside `memmove` and `BDX2` spent none.
    //
    // A phrase is at most `phrase::MAX` bytes for the two codes that name it, a symbol eight for
    // one and an escape one for two, so `MAX / 2` a byte is exactly enough: before a phrase's
    // store at least two bytes of input are left and `2 * MAX / 2` is the `MAX` it writes.
    let Some(room) = packed.len().checked_mul(phrase::MAX / 2) else {
        return false;
    };
    let mut room = Room::of(out, room.min(cap.saturating_add(phrase::MAX)));
    let mut o = 0;
    let mut i = 0;
    while i < packed.len() && o < cap {
        if let Some((id, took)) = split.read_at(packed, i) {
            let Some((phrase, len)) = dict.chunk(id) else {
                return false;
            };
            room.put(o, phrase);
            o += len;
            i += took;
        } else if packed[i] == ESCAPE {
            let Some(&b) = packed.get(i + 1) else {
                return false;
            };
            room.byte(o, b);
            o += 1;
            i += 2;
        } else {
            let Some((word, len)) = table.symbol(packed[i]) else {
                return false;
            };
            room.put(o, &word.to_le_bytes());
            o += len;
            i += 1;
        }
    }
    // SAFETY: the loop wrote every byte below `o`, and a failed one commits nothing.
    unsafe { commit(out, o) };
    true
}

/// [`compare_tiered`] where ids past the phrases name trie labels: a label is decoded out of the
/// nested dictionary and compared eight bytes at a time like a phrase. A loop of its own, so that a
/// blob without labels runs the loop it always ran.
#[inline(always)]
fn compare_labelled(
    table: &Table,
    split: Split,
    dict: &Dict,
    packed: &[u8],
    rest: Rest<'_>,
) -> (usize, Ordering) {
    let (words, lens) = table.parts();
    let n = words.len().min(usize::from(split.symbols()));
    let (words, lens) = (&words[..n], &lens[..n]);
    let mut c = 0;
    let mut i = 0;
    while let Some(&code) = packed.get(i) {
        let k = usize::from(code);
        if k < words.len() {
            i += 1;
            if let Some(answer) = compare_word(words[k], usize::from(lens[k]), rest, &mut c) {
                return answer;
            }
            continue;
        }
        if code == ESCAPE {
            let Some(&b) = packed.get(i + 1) else {
                break;
            };
            i += 2;
            if let Some(answer) = compare_word(u64::from(b), 1, rest, &mut c) {
                return answer;
            }
            continue;
        }
        let Some((id, took)) = split.read_at(packed, i) else {
            break;
        };
        i += took;
        if let Some((phrase, plen)) = dict.chunk(id) {
            // As in `compare_tiered`: whole loads off the padding.
            let mut k = 0;
            for word in phrase.chunks_exact(8) {
                if k >= plen {
                    break;
                }
                let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
                if let Some(answer) = compare_word(word, (plen - k).min(8), rest, &mut c) {
                    return answer;
                }
                k += 8;
            }
            continue;
        }
        let Some(first) = dict.label_first(id) else {
            break;
        };
        // An entry's suffix starts where its key branches, so a label is told from the probe
        // at its first byte about half the time, and that compare needs no decode.
        if c == rest.len() {
            return (c, Ordering::Greater);
        }
        let theirs = rest.word(c) as u8;
        if first != theirs {
            return (c, first.cmp(&theirs));
        }
        match compare_label(dict, id, rest, &mut c) {
            Some(None) => {}
            Some(Some(answer)) => return answer,
            None => break,
        }
    }
    ended(c, rest)
}

/// Label `id` against `rest` from `c`, out of line: the decode is what a labelled compare spends
/// most on, and the walk around it is the one every entry takes. `None` on a label this crate did
/// not write.
#[inline(never)]
fn compare_label(
    dict: &Dict,
    id: usize,
    rest: Rest<'_>,
    c: &mut usize,
) -> Option<Option<(usize, Ordering)>> {
    dict.label(id, |label| {
        let mut at = 0;
        while at < label.len() {
            let (word, kept) = label.word(at);
            if let Some(answer) = compare_word(word, kept, rest, c) {
                return Some(answer);
            }
            at += kept;
        }
        None
    })
}

/// [`decode_tiered`] where ids past the phrases name trie labels. The same fixed-width stores between
/// labels, with the room asked for again after each one: a label is up to [`phrase::LABEL_MAX`]
/// bytes for the two codes that name it, too wide to size the room by the stream.
#[inline(never)]
fn decode_labelled(
    table: &Table,
    split: Split,
    dict: &Dict,
    packed: &[u8],
    out: &mut Vec<u8>,
    cap: usize,
) -> bool {
    let start = out.len();
    let mut i = 0;
    loop {
        let wanted = cap.saturating_sub(out.len() - start);
        let Some(room) = (packed.len() - i).checked_mul(phrase::MAX / 2) else {
            out.truncate(start);
            return false;
        };
        let mut room = Room::of(out, room.min(wanted.saturating_add(phrase::MAX)));
        let mut o = 0;
        let mut label = None;
        while i < packed.len() && o < wanted {
            if let Some((id, took)) = split.read_at(packed, i) {
                let Some((phrase, len)) = dict.chunk(id) else {
                    label = Some(id);
                    i += took;
                    break;
                };
                room.put(o, phrase);
                o += len;
                i += took;
            } else if packed[i] == ESCAPE {
                let Some(&b) = packed.get(i + 1) else {
                    out.truncate(start);
                    return false;
                };
                room.byte(o, b);
                o += 1;
                i += 2;
            } else {
                let Some((word, len)) = table.symbol(packed[i]) else {
                    out.truncate(start);
                    return false;
                };
                room.put(o, &word.to_le_bytes());
                o += len;
                i += 1;
            }
        }
        // SAFETY: the loop wrote every byte below `o`.
        unsafe { commit(out, o) };
        let Some(id) = label else {
            return true;
        };
        let read = dict.label(id, |label| {
            let mut room = Room::of(out, label.len() + 8);
            let mut at = 0;
            while at < label.len() {
                let (word, kept) = label.word(at);
                room.put(at, &word.to_le_bytes());
                at += kept;
            }
            // SAFETY: the loop wrote every byte below `at`, which is the label's length.
            unsafe { commit(out, at) };
        });
        if read.is_none() {
            out.truncate(start);
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Rest::word` at every start and offset of every key length to twenty is `word_at` of the
    /// key at `at + c`: the one load and shift for a key of eight bytes or more, the whole key's
    /// tail shifted down under eight, zero at and past the end.
    #[test]
    fn rest_word_is_the_key_word_past_at() {
        let bytes: Vec<u8> = (1..=20u8).map(|k| k.wrapping_mul(53) | 1).collect();
        for len in 0..=20 {
            let key = &bytes[..len];
            let tail = tail_word(key);
            for at in 0..=len {
                let rest = Rest { key, at, tail };
                for c in 0..=len - at + 2 {
                    assert_eq!(
                        rest.word(c),
                        fsst::word_at(key, at + c),
                        "len {len} at {at} c {c}"
                    );
                }
            }
        }
    }
}
