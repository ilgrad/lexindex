//! A fixed-width code over one shard's suffix alphabet, for the corpora a symbol table cannot
//! reach.
//!
//! [`fsst`](crate::fsst) codes a byte in eight bits and buys its compression by naming runs of
//! them. On a shard whose suffixes are drawn from four characters, or sixteen, or sixty-four, that
//! is the wrong trade: a code of two bits already meets the order-0 bound and no run of bases is
//! frequent enough to pay for a symbol. Measured over a shard at a time, a DNA blob goes 7.36 →
//! 4.34 bytes a key and a base-64 one 13.24 → 10.37, while `uuid` — seventeen characters, which do
//! not fit a nibble — is left to the symbol table on every one of its shards.
//!
//! A code is `width` bits. The `2^width - 1` commonest bytes get one each; the next `2^width - 1`
//! get the all-ones code and then their own; anything else gets two all-ones codes and its eight
//! bits. When the alphabet fits the width outright there is no escape and every byte is one code.
//! The codes of one run are continuous — an entry's suffix starts where the one before it ended,
//! mid-byte — which is what the last 0.3 bytes a key are: padding each suffix to a byte instead
//! costs 4.66 on DNA against 4.34.

use std::cmp::Ordering;

/// The widths a code may take. One bit is a two-letter alphabet and eight is a byte, past which
/// the symbol table is the better structure anyway.
const WIDTHS: std::ops::RangeInclusive<u32> = 1..=8;

/// A shard's packed alphabet: the bytes behind a one-code symbol and the bytes behind an escaped
/// one, in code order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Alphabet {
    width: u32,
    one: Vec<u8>,
    two: Vec<u8>,
    /// Inverse of `one` and `two`: for each byte, the code it takes and whether it is escaped.
    /// `u16::MAX` for a byte neither names, which is written raw.
    code: Box<[u16; 256]>,
}

/// The all-ones code at `width` bits, which escapes when `two` is not empty.
#[inline(always)]
const fn top(width: u32) -> usize {
    (1usize << width) - 1
}

/// Codes one raw byte takes under a width whose alphabet does not fit: two escapes and its eight
/// bits, `width` at a time.
#[inline(always)]
const fn raw_codes(width: u32) -> usize {
    2 + 8usize.div_ceil(width as usize)
}

impl Alphabet {
    /// The cheapest alphabet over `freq`, the byte frequencies of a shard's suffixes, and the bits
    /// its codes would take. `None` when the shard stores no suffix bytes at all.
    pub(crate) fn of(freq: &[u64; 256]) -> Option<(Self, u64)> {
        let ranked = {
            let mut seen: Vec<(u64, u8)> = (0..=255u8)
                .filter(|&b| freq[b as usize] > 0)
                .map(|b| (freq[b as usize], b))
                .collect();
            // By frequency, and by byte where two are equally common, so the alphabet a build
            // produces does not depend on the order a `Vec` happened to be in.
            seen.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            seen
        };
        if ranked.is_empty() {
            return None;
        }
        let (width, bits) = WIDTHS
            .map(|w| (w, Self::bits(&ranked, w)))
            .min_by_key(|&(w, bits)| (bits, w))?;
        let (one, two) = Self::split(&ranked, width);
        let mut code = Box::new([u16::MAX; 256]);
        for (i, &b) in one.iter().enumerate() {
            code[b as usize] = i as u16;
        }
        for (i, &b) in two.iter().enumerate() {
            code[b as usize] = (1 << 15) | i as u16;
        }
        Some((
            Self {
                width,
                one,
                two,
                code,
            },
            bits,
        ))
    }

    /// What `ranked` costs in bits at `width`.
    fn bits(ranked: &[(u64, u8)], width: u32) -> u64 {
        let sum = |r: &[(u64, u8)]| r.iter().map(|&(f, _)| f).sum::<u64>();
        if ranked.len() <= 1 << width {
            return sum(ranked) * u64::from(width);
        }
        let k = top(width);
        let (one, two, rest) = (
            sum(&ranked[..k]),
            sum(&ranked[k..(2 * k).min(ranked.len())]),
            sum(ranked.get(2 * k..).unwrap_or_default()),
        );
        (one + 2 * two + rest * raw_codes(width) as u64) * u64::from(width)
    }

    /// The two code tables at `width`: every byte in one of them when the alphabet fits, and
    /// otherwise the commonest behind a code each and the next behind an escape.
    fn split(ranked: &[(u64, u8)], width: u32) -> (Vec<u8>, Vec<u8>) {
        let bytes: Vec<u8> = ranked.iter().map(|&(_, b)| b).collect();
        if bytes.len() <= 1 << width {
            return (bytes, Vec::new());
        }
        let k = top(width);
        (
            bytes[..k].to_vec(),
            bytes[k..(2 * k).min(bytes.len())].to_vec(),
        )
    }

    /// Bits one code takes, which is what a header's `len` counts in.
    #[inline(always)]
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    /// Codes `s` takes, without writing it.
    pub(crate) fn codes_for(&self, s: &[u8]) -> usize {
        s.iter()
            .map(|&b| match self.code[b as usize] {
                u16::MAX => raw_codes(self.width),
                c if c >> 15 == 1 => 2,
                _ => 1,
            })
            .sum()
    }

    /// Appends `s` to `out`, which holds the run's codes so far; returns the codes written.
    pub(crate) fn encode_into(&self, s: &[u8], out: &mut Bits) -> usize {
        let before = out.len;
        for &b in s {
            match self.code[b as usize] {
                u16::MAX => {
                    out.push(top(self.width), self.width);
                    out.push(top(self.width), self.width);
                    let mut left = u32::from(b);
                    for _ in 0..8usize.div_ceil(self.width as usize) {
                        out.push(left as usize & top(self.width), self.width);
                        left >>= self.width;
                    }
                }
                c if c >> 15 == 1 => {
                    out.push(top(self.width), self.width);
                    out.push(usize::from(c & 0x7FFF), self.width);
                }
                c => out.push(usize::from(c), self.width),
            }
        }
        out.len - before
    }

    /// The byte `codes` opens with and how many codes it took, or `None` past the end or on codes
    /// this crate did not write.
    #[inline(always)]
    fn byte_at(&self, codes: &Codes<'_>, at: usize) -> Option<(u8, usize)> {
        let first = codes.at(at)?;
        if self.two.is_empty() || first != top(self.width) {
            return self.one.get(first).map(|&b| (b, 1));
        }
        let second = codes.at(at + 1)?;
        if second != top(self.width) {
            return self.two.get(second).map(|&b| (b, 2));
        }
        let mut byte = 0u32;
        let per = 8usize.div_ceil(self.width as usize);
        for i in 0..per {
            byte |= (codes.at(at + 2 + i)? as u32) << (i as u32 * self.width);
        }
        Some((byte as u8, 2 + per))
    }

    /// Appends the bytes `codes` stands for to `out`; `false` on codes this crate did not write.
    pub(crate) fn decode_into(&self, codes: &Codes<'_>, out: &mut Vec<u8>) -> bool {
        let mut at = 0;
        while at < codes.len {
            let Some((b, took)) = self.byte_at(codes, at) else {
                return false;
            };
            out.push(b);
            at += took;
        }
        true
    }

    /// How many leading bytes `codes` shares with `rest`, and how it orders against it — decoded a
    /// byte at a time, which is what a scan needs and no more.
    #[inline]
    pub(crate) fn compare(&self, codes: &Codes<'_>, rest: &[u8]) -> (usize, Ordering) {
        let mut at = 0;
        let mut c = 0;
        while at < codes.len {
            let Some((b, took)) = self.byte_at(codes, at) else {
                break;
            };
            at += took;
            let Some(&theirs) = rest.get(c) else {
                return (c, Ordering::Greater);
            };
            if b != theirs {
                return (c, b.cmp(&theirs));
            }
            c += 1;
        }
        let ord = if c == rest.len() {
            Ordering::Equal
        } else {
            Ordering::Less
        };
        (c, ord)
    }

    /// `[width u8][one len - 1 u8][two len u8][one][two]`. The first table is never empty and can
    /// hold all 256 bytes, which is one more than its length field holds.
    pub(crate) fn serialized_len(&self) -> usize {
        3 + self.one.len() + self.two.len()
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.width as u8);
        out.push((self.one.len() - 1) as u8);
        out.push(self.two.len() as u8);
        out.extend_from_slice(&self.one);
        out.extend_from_slice(&self.two);
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Self> {
        let (&width, rest) = bytes.split_first()?;
        let (&ones, rest) = rest.split_first()?;
        let (&twos, rest) = rest.split_first()?;
        let width = u32::from(width);
        let ones = usize::from(ones) + 1;
        if !WIDTHS.contains(&width) || rest.len() != ones + usize::from(twos) {
            return None;
        }
        // Every code has to name at most one byte, or a read would not be the write's inverse.
        let (one, two) = rest.split_at(ones);
        if one.len() > 1 << width || two.len() > top(width) {
            return None;
        }
        if !two.is_empty() && one.len() != top(width) {
            return None;
        }
        let mut code = Box::new([u16::MAX; 256]);
        for (i, &b) in one.iter().enumerate() {
            if code[b as usize] != u16::MAX {
                return None;
            }
            code[b as usize] = i as u16;
        }
        for (i, &b) in two.iter().enumerate() {
            if code[b as usize] != u16::MAX {
                return None;
            }
            code[b as usize] = (1 << 15) | i as u16;
        }
        Some(Self {
            width,
            one: one.to_vec(),
            two: two.to_vec(),
            code,
        })
    }
}

/// A run's codes as they are written: a bit stream, so one entry's suffix begins where the one
/// before it ended.
#[derive(Default)]
pub(crate) struct Bits {
    out: Vec<u8>,
    /// Codes written, which is what a header's `len` counts.
    len: usize,
    bits: u64,
    used: u32,
}

impl Bits {
    pub(crate) fn clear(&mut self) {
        self.out.clear();
        self.len = 0;
        self.bits = 0;
        self.used = 0;
    }

    #[inline(always)]
    fn push(&mut self, code: usize, width: u32) {
        self.bits |= (code as u64) << self.used;
        self.used += width;
        self.len += 1;
        while self.used >= 8 {
            self.out.push(self.bits as u8);
            self.bits >>= 8;
            self.used -= 8;
        }
    }

    /// Bits written so far, which is where the next code would start.
    #[cfg(test)]
    fn bits(&self) -> usize {
        self.out.len() * 8 + self.used as usize
    }

    /// Appends the stream to `out`, the last byte padded with zeroes, and leaves it empty.
    pub(crate) fn drain_into(&mut self, out: &mut Vec<u8>) {
        if self.used > 0 {
            self.out.push(self.bits as u8);
        }
        out.extend_from_slice(&self.out);
        self.clear();
    }

    /// Pads to the next byte, so that the varints of a pair no code could name are byte-aligned.
    pub(crate) fn pad(&mut self) {
        if self.used > 0 {
            self.out.push(self.bits as u8);
            self.bits = 0;
            self.used = 0;
        }
    }

    pub(crate) fn extend_from_slice(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.used, 0, "a raw write into a stream mid-byte");
        self.out.extend_from_slice(bytes);
    }
}

/// One entry's codes inside a run's stream: where they start, and how many there are.
#[derive(Clone, Copy)]
pub(crate) struct Codes<'a> {
    bytes: &'a [u8],
    /// The bit the first code starts at.
    start: usize,
    width: u32,
    pub(crate) len: usize,
}

impl<'a> Codes<'a> {
    pub(crate) fn new(bytes: &'a [u8], start: usize, width: u32, len: usize) -> Self {
        Self {
            bytes,
            start,
            width,
            len,
        }
    }

    /// The `i`th code, or `None` past the stream — a run this crate did not write ends the walk
    /// rather than reading somebody else's bytes.
    #[inline(always)]
    fn at(&self, i: usize) -> Option<usize> {
        if i >= self.len {
            return None;
        }
        let bit = self.start + i * self.width as usize;
        let (byte, shift) = (bit / 8, bit % 8);
        if bit + self.width as usize > self.bytes.len() * 8 {
            return None;
        }
        let take = self.bytes.get(byte..)?;
        let n = take.len().min(8);
        let mut buf = [0u8; 8];
        buf[..n].copy_from_slice(&take[..n]);
        Some(((u64::from_le_bytes(buf) >> shift) as usize) & top(self.width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frequencies(text: &[u8]) -> [u64; 256] {
        let mut freq = [0u64; 256];
        for &b in text {
            freq[b as usize] += 1;
        }
        freq
    }

    /// Writes `pieces` as one run and reads every one of them back.
    fn round_trip(pieces: &[&[u8]]) -> Alphabet {
        let text: Vec<u8> = pieces.concat();
        let (alphabet, bits) = Alphabet::of(&frequencies(&text)).expect("a non-empty shard");
        let mut stream = Bits::default();
        let spans: Vec<(usize, usize)> = pieces
            .iter()
            .map(|p| {
                let start = stream.bits();
                let len = alphabet.encode_into(p, &mut stream);
                (start, len)
            })
            .collect();
        assert_eq!(
            bits,
            stream.bits() as u64,
            "the price and the stream disagree"
        );
        let mut data = Vec::new();
        stream.drain_into(&mut data);
        for (piece, &(start, len)) in pieces.iter().zip(&spans) {
            assert_eq!(alphabet.codes_for(piece), len, "{piece:?}");
            let codes = Codes::new(&data, start, alphabet.width(), len);
            let mut out = Vec::new();
            assert!(alphabet.decode_into(&codes, &mut out), "{piece:?}");
            assert_eq!(&out, piece);
            assert_eq!(
                alphabet.compare(&codes, piece),
                (piece.len(), Ordering::Equal)
            );
        }
        let stored = {
            let mut bytes = Vec::new();
            alphabet.write_to(&mut bytes);
            assert_eq!(bytes.len(), alphabet.serialized_len());
            Alphabet::read(&bytes).expect("our own alphabet")
        };
        assert_eq!(stored, alphabet);
        alphabet
    }

    #[test]
    fn an_alphabet_that_fits_the_width_spends_no_escape() {
        let a = round_trip(&[b"ACGT", b"GATTACA", b"TTTT"]);
        assert_eq!((a.width(), a.one.len(), a.two.len()), (2, 4, 0));
        assert_eq!(a.codes_for(b"ACGT"), 4);
    }

    #[test]
    fn the_bytes_past_the_width_escape_once_and_then_raw() {
        // One byte in a thousand and twenty-five others once each: a one-bit code with an escape
        // and a raw tail beats the five-bit code the alphabet would otherwise fit in.
        let mut text = vec![b'a'; 1000];
        text.extend(b'b'..=b'z');
        let a = round_trip(&[&text[..500], &text[500..1000], &text[1000..]]);
        assert_eq!((a.width(), a.one.len(), a.two.len()), (1, 1, 1));
        assert_eq!(a.codes_for(b"a"), 1);
        assert_eq!(a.codes_for(b"b"), 2);
        assert_eq!(a.codes_for(b"z"), raw_codes(1));
    }

    #[test]
    fn a_byte_no_code_names_is_written_raw() {
        // 300 distinct bytes do not exist, but 200 do: at any width below eight the tail is raw.
        let text: Vec<u8> = (0..=255u8).flat_map(|b| [b, b, b]).collect();
        let (a, _) = Alphabet::of(&frequencies(&text)).unwrap();
        assert_eq!(a.width(), 8, "a flat byte alphabet is a byte code");
        let a = round_trip(&[&text[..64], &text[64..200], &text[200..]]);
        assert_eq!(a.codes_for(&text), text.len());
    }

    #[test]
    fn an_empty_shard_has_no_alphabet() {
        assert!(Alphabet::of(&[0u64; 256]).is_none());
    }

    #[test]
    fn a_run_reads_back_from_any_bit_it_starts_on() {
        let pieces: Vec<&[u8]> = vec![b"AC", b"G", b"TTGA", b"C", b"GGGGGGG"];
        round_trip(&pieces);
    }

    #[test]
    fn an_alphabet_this_crate_did_not_write_is_refused() {
        let (a, _) = Alphabet::of(&frequencies(b"ACGTACGT")).unwrap();
        let mut bytes = Vec::new();
        a.write_to(&mut bytes);
        // A width outside the range, a length that does not cover the tables, and a byte named
        // twice.
        for edit in [0usize, 1, 2] {
            let mut b = bytes.clone();
            b[edit] = 99;
            assert!(Alphabet::read(&b).is_none(), "{edit}");
        }
        let mut b = bytes.clone();
        let last = b.len() - 1;
        b[last] = b[last - 1];
        assert!(Alphabet::read(&b).is_none(), "a byte behind two codes");
        assert!(Alphabet::read(&bytes[..2]).is_none());
    }

    #[test]
    fn a_truncated_run_ends_the_walk_rather_than_reading_past_it() {
        let (a, _) = Alphabet::of(&frequencies(b"ACGTACGTGGCC")).unwrap();
        let mut stream = Bits::default();
        let len = a.encode_into(b"ACGTACGT", &mut stream);
        let mut data = Vec::new();
        stream.drain_into(&mut data);
        let cut = Codes::new(&data[..1], 0, a.width(), len);
        let mut out = Vec::new();
        assert!(!a.decode_into(&cut, &mut out));
        assert!(out.len() < 8);
        assert!(a.compare(&cut, b"ACGTACGT").0 < 8);
    }
}
