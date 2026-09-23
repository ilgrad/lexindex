//! An order-keeping code for the characters of a [`DictIndex`](crate::DictIndex)'s keys: one or
//! two bytes a character where UTF-8 spends two to four.
//!
//! The dictionary's codec sees bytes. A symbol table learns the byte pairs a script repeats, but a
//! Chinese key of three characters still reaches it as nine bytes, and a Russian title spends a
//! byte on every letter that says only which half of the Cyrillic block the letter is in. This code
//! respells every character in one or two bytes, fitted to the characters a blob's keys hold: the
//! most frequent may take a byte of their own, and the rest share pages of two-byte codewords, a
//! lead byte naming the page and the second the character's place in it.
//!
//! Two properties make it invisible to everything downstream. The codewords ascend with the
//! characters, and none is a prefix of another — a lead byte says on its own whether a second
//! follows — so coded keys sort exactly as the keys do: ids stay ranks, a prefix of a key is a
//! prefix of its code, and a probe spelled the same way is compared byte for byte by a codec that
//! did not change. A query holding a character the code does not spell cannot be a key, and still
//! has a place in the order, which the first codeword above the missing character marks.
//!
//! A code is kept only where it pays for its table ([`Tally::choose`]). On the three non-Latin
//! corpora measured on 2026-09-23 it takes jieba's lexicon 16.8 % smaller at the default block,
//! Chinese Wikipedia titles 6.7 % and Russian ones 10.5 %; every Latin corpus measured keeps its
//! `BDX3` blob byte for byte.

use crate::dict_index::{put_varint, varint_at};

/// Leads a code may use. A lead is one byte, and each mode leaves it half of the values.
const LEADS: usize = 128;
/// Code points one page of the [`Index`] covers.
const PAGE: usize = 256;
/// Pages the index can name: every scalar value is below `0x110000 = 0x1100 * 256`.
const PAGES: usize = 0x1100;

/// Which characters a code spells, and in which bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Every character the keys hold, ASCII included, in bytes below `0x80`: a lead under `0x80`
    /// and a page's second byte under it too. What that buys is the packed codec — a shard whose
    /// suffixes use no byte above `0x7f` stores them at seven bits a byte, which on jieba's lexicon
    /// is 6 of the 17 points the code takes off, against a code of whole bytes.
    Seven,
    /// ASCII as itself and every other character under a lead from `0x80`, a page's second byte
    /// any byte: for an alphabet a seven-bit code cannot hold, up to 32 768 characters.
    Eight,
}

impl Mode {
    /// The byte lead 0 is written as.
    fn base(self) -> u8 {
        match self {
            Mode::Seven => 0,
            Mode::Eight => 0x80,
        }
    }

    /// Characters one page of two-byte codewords holds.
    fn page(self) -> usize {
        match self {
            Mode::Seven => 128,
            Mode::Eight => 256,
        }
    }

    /// The byte a blob names the mode by.
    fn tag(self) -> u8 {
        match self {
            Mode::Seven => 7,
            Mode::Eight => 8,
        }
    }
}

/// A codeword as the index holds it: `len << 16 | first << 8 | second`, and zero for a character
/// the code does not spell.
type Word = u32;

fn word(first: u8, second: Option<u8>) -> Word {
    match second {
        None => 1 << 16 | u32::from(first) << 8,
        Some(s) => 2 << 16 | u32::from(first) << 8 | u32::from(s),
    }
}

/// Write `w` at `at`, returning where it ends. `out` has room: a codeword is at most two bytes and
/// never longer than the character it spells is in UTF-8, bar ASCII in [`Mode::Seven`], which is
/// what the callers' buffers of twice the query are sized for.
#[inline(always)]
fn put(out: &mut [u8], at: usize, w: Word) -> usize {
    out[at] = (w >> 8) as u8;
    if w >> 16 == 2 {
        out[at + 1] = w as u8;
        at + 2
    } else {
        at + 1
    }
}

/// Codewords by code point, two levels deep: a page of 256 code points, then the point in it. Only
/// the pages the alphabet touches hold a table — a Chinese alphabet of 12 000 characters touches
/// about a hundred of the 4 352, a hundred kilobytes beside the first level's eight and a half.
struct Index {
    /// `pages[cp >> 8]`: one past the page's place in `words`, or zero for a page the alphabet has
    /// no character on.
    pages: Box<[u16]>,
    words: Box<[Word]>,
}

impl Index {
    #[inline(always)]
    fn get(&self, c: char) -> Word {
        match self.pages[c as usize >> 8] {
            0 => 0,
            p => self.words[(usize::from(p) - 1) * PAGE + (c as usize & 0xFF)],
        }
    }
}

/// What a query became under a code, beside its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// Every character has a codeword, so the bytes order against every key as the query does.
    Exact,
    /// A character has none, so no key is the query; a key is below the query exactly when it is
    /// below the bytes, which end on the first codeword above the missing character.
    Absent,
    /// A character above every one the code spells, after a prefix whose code no key passes: every
    /// key is below the query.
    Last,
}

/// An order-keeping code over one blob's characters.
pub(crate) struct CharCode {
    mode: Mode,
    /// The characters spelled, ascending: every one the keys hold in [`Mode::Seven`], every one
    /// past ASCII in [`Mode::Eight`].
    chars: Box<[char]>,
    /// The rank of each lead's first character, and past the last lead the alphabet's size. A lead
    /// of one character is that character's whole codeword; a wider one is a page of two-byte
    /// codewords, the second byte a character's place in it.
    firsts: Box<[u16]>,
    index: Index,
    /// What [`write_to`](Self::write_to) writes, counted once.
    len: usize,
}

impl CharCode {
    /// The code over `chars`, ascending, under leads of the widths `firsts` bounds.
    fn new(mode: Mode, chars: Box<[char]>, firsts: Box<[u16]>) -> Self {
        let mut pages = vec![0u16; PAGES].into_boxed_slice();
        let mut words: Vec<Word> = Vec::new();
        for (l, pair) in firsts.windows(2).enumerate() {
            let (first, end) = (usize::from(pair[0]), usize::from(pair[1]));
            let lead = mode.base() + l as u8;
            for (at, &c) in chars[first..end].iter().enumerate() {
                let p = c as usize >> 8;
                if pages[p] == 0 {
                    words.resize(words.len() + PAGE, 0);
                    pages[p] = (words.len() / PAGE) as u16;
                }
                words[(usize::from(pages[p]) - 1) * PAGE + (c as usize & 0xFF)] =
                    if end - first == 1 {
                        word(lead, None)
                    } else {
                        word(lead, Some(at as u8))
                    };
            }
        }
        let mut prev = 0u32;
        let deltas: usize = chars
            .iter()
            .map(|&c| {
                let d = c as u32 - prev;
                prev = c as u32;
                varint_len(d)
            })
            .sum();
        let len = 1 + varint_len(chars.len() as u32) + firsts.len() + deltas;
        Self {
            mode,
            chars,
            firsts,
            index: Index {
                pages,
                words: words.into_boxed_slice(),
            },
            len,
        }
    }

    /// Append `key`'s code to `out`; `false`, with `out` as it was plus some of the key, for a key
    /// holding a character the code does not spell — which a key the code was chosen over never
    /// does, and a stream that changed between two passes can.
    pub(crate) fn encode_key(&self, key: &str, out: &mut Vec<u8>) -> bool {
        for c in key.chars() {
            if self.mode == Mode::Eight && c.is_ascii() {
                out.push(c as u8);
                continue;
            }
            match self.index.get(c) {
                0 => return false,
                w => {
                    out.push((w >> 8) as u8);
                    if w >> 16 == 2 {
                        out.push(w as u8);
                    }
                }
            }
        }
        true
    }

    /// `query` as the keys are spelled, into `out`, which must hold twice the query's bytes: how
    /// many of them it took, and what they stand for.
    pub(crate) fn probe_into(&self, query: &str, out: &mut [u8]) -> (Probe, usize) {
        let mut o = 0;
        for c in query.chars() {
            if self.mode == Mode::Eight && c.is_ascii() {
                out[o] = c as u8;
                o += 1;
                continue;
            }
            let w = self.index.get(c);
            if w != 0 {
                o = put(out, o, w);
                continue;
            }
            // No key holds `c` here, so the keys below the query are those below the first
            // character that is spelled and above it — or, above every one, all that carry the
            // prefix, which end where the prefix's code, incremented, begins.
            let next = self.chars.partition_point(|&x| x < c);
            return match self.chars.get(next) {
                Some(&next) => (Probe::Absent, put(out, o, self.index.get(next))),
                None => match bump(&mut out[..o]) {
                    Some(len) => (Probe::Absent, len),
                    None => (Probe::Last, 0),
                },
            };
        }
        (Probe::Exact, o)
    }

    /// The code of every prefix of `query` that ends on a character boundary, up to the first
    /// character the code does not spell — past which no key can be a prefix of the query. `out`
    /// is the code of the longest; `ends` the `(query bytes, code bytes)` of each, shortest first,
    /// the empty prefix included.
    pub(crate) fn prefixes(&self, query: &str, out: &mut Vec<u8>, ends: &mut Vec<(usize, usize)>) {
        out.clear();
        ends.clear();
        ends.push((0, 0));
        for (at, c) in query.char_indices() {
            if self.mode == Mode::Eight && c.is_ascii() {
                out.push(c as u8);
            } else {
                match self.index.get(c) {
                    0 => return,
                    w => {
                        out.push((w >> 8) as u8);
                        if w >> 16 == 2 {
                            out.push(w as u8);
                        }
                    }
                }
            }
            ends.push((at + c.len_utf8(), out.len()));
        }
    }

    /// The character whose codeword starts at `at`, and the codeword's length; `None` where no
    /// codeword starts, which only a blob this crate did not write holds.
    #[inline(always)]
    fn char_at(&self, coded: &[u8], at: usize) -> Option<(char, usize)> {
        let b = *coded.get(at)?;
        if self.mode == Mode::Eight && b < 0x80 {
            return Some((char::from(b), 1));
        }
        let l = usize::from(b.wrapping_sub(self.mode.base()));
        let first = usize::from(*self.firsts.get(l)?);
        let width = usize::from(*self.firsts.get(l + 1)?) - first;
        if width == 1 {
            return Some((self.chars[first], 1));
        }
        let s = usize::from(*coded.get(at + 1)?);
        (s < width).then(|| (self.chars[first + s], 2))
    }

    /// Append what `coded` spells to `out`; `false` on bytes no code wrote.
    pub(crate) fn decode_into(&self, coded: &[u8], out: &mut Vec<u8>) -> bool {
        let mut at = 0;
        while at < coded.len() {
            let Some((c, len)) = self.char_at(coded, at) else {
                return false;
            };
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            at += len;
        }
        true
    }

    /// Replace `buf`'s code by what it spells, in place, so that a caller who keeps one buffer
    /// allocates nothing; `false`, with `buf` empty, on bytes no code wrote.
    ///
    /// The code is moved to the top of four times its length and decoded upwards from the bottom.
    /// No codeword spells more than four bytes, so after `k` bytes of code the output is at most
    /// `4k` long while the next unread byte sits at `3c + k`, `c` the code's length — never behind
    /// the output, since `k <= c`.
    pub(crate) fn decode_in_place(&self, buf: &mut Vec<u8>) -> bool {
        let c = buf.len();
        buf.resize(4 * c, 0);
        buf.copy_within(..c, 3 * c);
        let (mut r, mut w) = (3 * c, 0);
        while r < 4 * c {
            let Some((ch, len)) = self.char_at(buf, r) else {
                buf.clear();
                return false;
            };
            r += len;
            let s = ch.len_utf8();
            ch.encode_utf8(&mut buf[w..w + s]);
            w += s;
        }
        buf.truncate(w);
        true
    }

    /// `[mode u8][characters varint][leads u8][lead widths, less one, a byte each][the characters'
    /// code points as varints, the first whole and each after it past the one before]`.
    pub(crate) fn serialized_len(&self) -> usize {
        self.len
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.mode.tag());
        put_varint(out, self.chars.len());
        out.push((self.firsts.len() - 1) as u8);
        for pair in self.firsts.windows(2) {
            out.push((pair[1] - pair[0] - 1) as u8);
        }
        let mut prev = 0u32;
        for &c in self.chars.iter() {
            put_varint(out, (c as u32 - prev) as usize);
            prev = c as u32;
        }
    }

    /// The code a blob stores, every field checked: a mode this crate writes, lead widths that
    /// cover the characters exactly, and characters that ascend, are scalar values, and — in
    /// [`Mode::Eight`] — are not ASCII.
    pub(crate) fn read(bytes: &[u8]) -> Option<Self> {
        let mode = match *bytes.first()? {
            7 => Mode::Seven,
            8 => Mode::Eight,
            _ => return None,
        };
        let mut at = 1;
        let count = varint_at(bytes, &mut at)?;
        if count == 0 || count > LEADS * mode.page() {
            return None;
        }
        let leads = usize::from(*bytes.get(at)?);
        at += 1;
        if leads == 0 || leads > LEADS {
            return None;
        }
        let mut firsts = Vec::with_capacity(leads + 1);
        firsts.push(0u16);
        let mut sum = 0usize;
        for &w in bytes.get(at..at + leads)? {
            let w = usize::from(w) + 1;
            if w > mode.page() {
                return None;
            }
            sum += w;
            firsts.push(u16::try_from(sum).ok()?);
        }
        at += leads;
        if sum != count {
            return None;
        }
        let mut chars = Vec::with_capacity(count);
        let mut prev = 0usize;
        for i in 0..count {
            let d = varint_at(bytes, &mut at)?;
            if i > 0 && d == 0 {
                return None;
            }
            let cp = prev.checked_add(d)?;
            let c = char::from_u32(u32::try_from(cp).ok()?)?;
            if mode == Mode::Eight && c.is_ascii() {
                return None;
            }
            chars.push(c);
            prev = cp;
        }
        (at == bytes.len()).then(|| Self::new(mode, chars.into(), firsts.into()))
    }
}

/// Bytes [`put_varint`] spends on `v`.
fn varint_len(v: u32) -> usize {
    (32 - (v | 1).leading_zeros()).div_ceil(7) as usize
}

/// The least byte string above every one `bytes` is a prefix of: the last byte not `0xff`
/// incremented, and what follows it dropped. `None` where there is none — `bytes` empty or all
/// `0xff` — and every string is below.
fn bump(bytes: &mut [u8]) -> Option<usize> {
    let last = bytes.iter().rposition(|&b| b != 0xFF)?;
    bytes[last] += 1;
    Some(last + 1)
}

/// Whether keys this many bytes long, this many of them not ASCII, could be worth a code at all:
/// one saves at most three of a character's four bytes, and must save a tenth of the keys to be
/// kept, so under four thirtieths of the bytes outside ASCII it cannot — which is every Latin
/// corpus, found by a scan that never decodes a character.
pub(crate) fn worth_counting(bytes: u64, high: u64) -> bool {
    high * 30 >= bytes * 4
}

/// Bytes of `key` outside ASCII.
#[inline]
pub(crate) fn high_bytes(key: &[u8]) -> u64 {
    key.iter().filter(|&&b| b >= 0x80).count() as u64
}

/// How often each character occurs over a set of keys, and what the keys weigh in UTF-8: what
/// [`choose`](Self::choose) decides on.
pub(crate) struct Tally {
    bytes: u64,
    /// Occurrences by code point, a page of 256 at a time, each page allocated when it is met.
    pages: Vec<Option<Box<[u64; PAGE]>>>,
}

impl Tally {
    pub(crate) fn new() -> Self {
        Self {
            bytes: 0,
            pages: vec![None; PAGES],
        }
    }

    pub(crate) fn add(&mut self, key: &str) {
        self.bytes += key.len() as u64;
        for c in key.chars() {
            let page = self.pages[c as usize >> 8].get_or_insert_with(|| Box::new([0; PAGE]));
            page[c as usize & 0xFF] += 1;
        }
    }

    /// The code these keys are smallest under, or `None` where no code saves a tenth of their
    /// bytes, table included, or where the table would outweigh what the blob gains.
    ///
    /// Every character the keys hold is spelled in seven-bit bytes where 128 pages of 128 can
    /// hold them all, and the characters past ASCII in whole bytes where 128 pages of 256 can;
    /// past that, no code. As many of the most frequent characters as still fit then take a byte
    /// of their own — but only where together they are a tenth of the characters written. Below
    /// that a single saves little and costs the headers their regularity: a Chinese key under
    /// pages alone is two bytes a character, so every shared prefix and suffix length is even,
    /// and the twenty singles jieba's lexicon could afford took it from 16.8 % smaller to 15.3.
    /// Russian titles, whose letters are 96 % of what they write, are 10.5 % smaller with them
    /// and 2.2 % *larger* without.
    pub(crate) fn choose(&self) -> Option<CharCode> {
        self.choose_for(self.bytes)
    }

    /// [`choose`](Self::choose) for a corpus of `bytes` in all, of which these keys are a draw: its
    /// characters taken in the proportions the draw holds them, and its table the draw's.
    pub(crate) fn choose_for(&self, bytes: u64) -> Option<CharCode> {
        let alphabet: Vec<(char, u64)> = self
            .pages
            .iter()
            .enumerate()
            .filter_map(|(p, page)| Some((p, page.as_ref()?)))
            .flat_map(|(p, page)| {
                page.iter()
                    .enumerate()
                    .filter(|(_, n)| **n > 0)
                    .map(move |(i, &n)| {
                        let c = char::from_u32((p * PAGE + i) as u32).expect("counted from a char");
                        (c, n)
                    })
            })
            .collect();
        if alphabet.is_empty() {
            return None;
        }
        let total: u64 = alphabet.iter().map(|&(_, n)| n).sum();
        let (mode, spelled) = if alphabet.len() <= LEADS * Mode::Seven.page() {
            (Mode::Seven, &alphabet[..])
        } else {
            let high = &alphabet[alphabet.partition_point(|&(c, _)| c.is_ascii())..];
            if high.len() > LEADS * Mode::Eight.page() {
                return None;
            }
            (Mode::Eight, high)
        };
        let mut ranked: Vec<usize> = (0..spelled.len()).collect();
        ranked.sort_by(|&a, &b| spelled[b].1.cmp(&spelled[a].1).then(a.cmp(&b)));
        let mut single = vec![false; spelled.len()];
        let mark = |k: usize, single: &mut [bool]| {
            single.fill(false);
            for &i in &ranked[..k] {
                single[i] = true;
            }
        };
        // A single is a lead of its own and splits the run it sat in, so the leads a set of singles
        // needs only grow as the set does, and the largest set that fits is a binary search.
        let (mut lo, mut hi) = (0, spelled.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            mark(mid, &mut single);
            if widths(&single, mode.page()).len() <= LEADS {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let covered: u64 = ranked[..lo].iter().map(|&i| spelled[i].1).sum();
        mark(if covered * 10 >= total { lo } else { 0 }, &mut single);
        let widths = widths(&single, mode.page());
        let mut firsts = Vec::with_capacity(widths.len() + 1);
        firsts.push(0u16);
        for w in &widths {
            firsts.push(firsts[firsts.len() - 1] + *w as u16);
        }
        let chars: Box<[char]> = spelled.iter().map(|&(c, _)| c).collect();
        let code = CharCode::new(mode, chars, firsts.into());
        let spelt: u64 = alphabet
            .iter()
            .map(|&(c, n)| {
                n * match code.index.get(c) {
                    0 => 1, // ASCII under `Mode::Eight`, itself
                    w => u64::from(w >> 16),
                }
            })
            .sum();
        let spelt = (u128::from(spelt) * u128::from(bytes) / u128::from(self.bytes.max(1))) as u64;
        let table = code.serialized_len() as u64;
        // Two bars, both on counts rather than on a trial build. The keys have to come out a tenth
        // smaller, which no corpus mostly in ASCII comes near. And a twentieth of what the code
        // saves has to cover the table, because the codec behind it takes out most of the same
        // bytes on its own: of what the code saved on jieba's lexicon, Chinese titles and Russian
        // ones, 22, 11 and 5 % was still saved in the blob. Priced against the whole saving, a
        // table of two thousand ideographs over three thousand keys made the blob 0.7 % larger.
        let pays = (spelt + table) * 10 <= bytes * 9 && table * 20 <= bytes.saturating_sub(spelt);
        pays.then_some(code)
    }
}

/// The widths of the leads a code needs: a lead a single, and the runs of characters between
/// singles cut into pages of `page`. A piece of one character is a single all the same.
fn widths(single: &[bool], page: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut run = 0usize;
    let flush = |run: &mut usize, out: &mut Vec<usize>| {
        while *run > 0 {
            let w = (*run).min(page);
            out.push(w);
            *run -= w;
        }
    };
    for &s in single {
        if s {
            flush(&mut run, &mut out);
            out.push(1);
        } else {
            run += 1;
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
impl CharCode {
    /// A code over every character of `keys` under `mode`, whatever it saves, with the `singles`
    /// most frequent taking a byte where they fit: what a test needs to reach a coded blob on a
    /// handful of keys, where [`Tally::choose`] rightly finds no table worth its bytes.
    pub(crate) fn forced<S: AsRef<str>>(keys: &[S], seven: bool, singles: usize) -> Self {
        let mut tally = Tally::new();
        for k in keys {
            tally.add(k.as_ref());
        }
        let mut alphabet: Vec<(char, u64)> = Vec::new();
        for (p, page) in tally.pages.iter().enumerate() {
            if let Some(page) = page {
                for (i, &n) in page.iter().enumerate() {
                    if n > 0 {
                        alphabet.push((char::from_u32((p * PAGE + i) as u32).unwrap(), n));
                    }
                }
            }
        }
        let mode = if seven { Mode::Seven } else { Mode::Eight };
        if mode == Mode::Eight {
            alphabet.retain(|(c, _)| !c.is_ascii());
        }
        let mut ranked: Vec<usize> = (0..alphabet.len()).collect();
        ranked.sort_by(|&a, &b| alphabet[b].1.cmp(&alphabet[a].1).then(a.cmp(&b)));
        let mut single = vec![false; alphabet.len()];
        for &i in ranked.iter().take(singles) {
            single[i] = true;
        }
        let widths = widths(&single, mode.page());
        assert!(widths.len() <= LEADS, "{} leads", widths.len());
        let mut firsts = vec![0u16];
        for w in &widths {
            firsts.push(firsts[firsts.len() - 1] + *w as u16);
        }
        CharCode::new(
            mode,
            alphabet.iter().map(|&(c, _)| c).collect(),
            firsts.into(),
        )
    }

    fn encoded(&self, key: &str) -> Vec<u8> {
        let mut out = Vec::new();
        assert!(self.encode_key(key, &mut out), "{key:?}");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn roundtrip(code: &CharCode) -> CharCode {
        let mut blob = Vec::new();
        code.write_to(&mut blob);
        assert_eq!(blob.len(), code.serialized_len());
        let back = CharCode::read(&blob).expect("reads what it wrote");
        let mut again = Vec::new();
        back.write_to(&mut again);
        assert_eq!(again, blob);
        back
    }

    /// Characters from four scripts and both ends of the code space, weighted so that some are
    /// frequent enough to be singles and most are not.
    fn text() -> impl Strategy<Value = String> {
        let pool: Vec<char> = "\u{0}\u{1}az~\u{7f}\u{80}éжЖё中国人之的\u{ffff}\u{10000}\u{10ffff}"
            .chars()
            .chain((0x4E00..0x4E00 + 300).filter_map(char::from_u32))
            .collect();
        proptest::collection::vec(proptest::sample::select(pool), 0..6)
            .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The code keeps the order, is a prefix of a key's code exactly where it is a prefix of
        /// the key, and decodes back — in both modes and whatever the singles.
        #[test]
        fn coded_keys_sort_as_the_keys_do(
            keys in proptest::collection::vec(text(), 1..40),
            seven in any::<bool>(),
            singles in 0usize..40,
        ) {
            // A build writes no code over no characters, and a loader refuses one.
            let code = CharCode::forced(&keys, seven, singles);
            let code = if code.chars.is_empty() { code } else { roundtrip(&code) };
            for a in &keys {
                let ca = code.encoded(a);
                let mut back = Vec::new();
                prop_assert!(code.decode_into(&ca, &mut back));
                prop_assert_eq!(&back, a.as_bytes());
                let mut inplace = ca.clone();
                prop_assert!(code.decode_in_place(&mut inplace));
                prop_assert_eq!(&inplace, a.as_bytes());
                for b in &keys {
                    let cb = code.encoded(b);
                    prop_assert_eq!(ca.cmp(&cb), a.cmp(b), "{:?} {:?}", a, b);
                    prop_assert_eq!(cb.starts_with(&ca), b.starts_with(a.as_str()));
                }
            }
        }

        /// A probe orders against every key as the query does, whether or not the query's
        /// characters are all spelled — and says exactly which of the two it is.
        #[test]
        fn a_probe_orders_as_its_query(
            keys in proptest::collection::vec(text(), 1..30),
            queries in proptest::collection::vec(text(), 1..30),
            seven in any::<bool>(),
            singles in 0usize..40,
        ) {
            let code = CharCode::forced(&keys, seven, singles);
            for q in &queries {
                let mut buf = vec![0u8; 2 * q.len()];
                let (probe, len) = code.probe_into(q, &mut buf);
                let bytes = &buf[..len];
                let spelled = q.chars().all(|c| {
                    (!seven && c.is_ascii()) || code.index.get(c) != 0
                });
                prop_assert_eq!(probe == Probe::Exact, spelled);
                for k in &keys {
                    let ck = code.encoded(k);
                    match probe {
                        Probe::Exact => prop_assert_eq!(ck.as_slice().cmp(bytes), k.as_str().cmp(q)),
                        Probe::Absent => {
                            prop_assert!(k != q);
                            prop_assert_eq!(ck.as_slice() < bytes, k.as_str() < q.as_str(), "{:?} {:?}", k, q);
                        }
                        Probe::Last => prop_assert!(k.as_str() < q.as_str()),
                    }
                }
            }
        }

        /// The prefixes a common-prefix walk is handed are the query's own, coded, up to the first
        /// character without a codeword.
        #[test]
        fn prefixes_are_the_querys_own(
            keys in proptest::collection::vec(text(), 1..20),
            query in text(),
            seven in any::<bool>(),
        ) {
            let code = CharCode::forced(&keys, seven, 3);
            let (mut out, mut ends) = (Vec::new(), Vec::new());
            code.prefixes(&query, &mut out, &mut ends);
            prop_assert_eq!(ends[0], (0, 0));
            for &(e, ce) in &ends {
                let mut buf = vec![0u8; 2 * e];
                let (probe, len) = code.probe_into(&query[..e], &mut buf);
                prop_assert_eq!(probe, Probe::Exact);
                prop_assert_eq!(&buf[..len], &out[..ce]);
            }
            let &(last, _) = ends.last().unwrap();
            if last < query.len() {
                let c = query[last..].chars().next().unwrap();
                prop_assert!(code.index.get(c) == 0 && (seven || !c.is_ascii()));
            }
        }

        /// A table no build wrote is refused or read, never a panic.
        #[test]
        fn arbitrary_tables_never_panic(data in proptest::collection::vec(any::<u8>(), 0..64)) {
            if let Some(code) = CharCode::read(&data) {
                let mut out = Vec::new();
                let _ = code.decode_into(&data, &mut out);
                let mut buf = data.clone();
                let _ = code.decode_in_place(&mut buf);
            }
        }
    }

    #[test]
    fn a_table_is_refused_field_by_field() {
        let keys = ["中国", "人民", "a中"];
        let code = CharCode::forced(&keys, true, 1);
        let mut good = Vec::new();
        code.write_to(&mut good);
        assert!(CharCode::read(&good).is_some());
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(CharCode::read(&trailing).is_none(), "trailing bytes");
        assert!(
            CharCode::read(&good[..good.len() - 1]).is_none(),
            "cut short"
        );
        let mut mode = good.clone();
        mode[0] = 9;
        assert!(CharCode::read(&mode).is_none(), "a mode no build writes");
        // `[7][count][leads][widths…][chars…]`: a count the widths do not cover.
        let mut count = good.clone();
        count[1] += 1;
        assert!(CharCode::read(&count).is_none());
        // Characters that do not ascend: a zero step after the first.
        let repeat = [7, 2, 1, 1, 0x61, 0];
        assert!(CharCode::read(&repeat).is_none());
        assert!(CharCode::read(&[7, 2, 1, 1, 0x61, 1]).is_some());
        // A surrogate, and a step past the last scalar value.
        assert!(CharCode::read(&[7, 1, 1, 0, 0x80, 0xB0, 0x03]).is_none());
        assert!(CharCode::read(&[7, 1, 1, 0, 0x80, 0x80, 0x44]).is_none());
        // ASCII under the eight-bit mode, which spells it as itself.
        assert!(CharCode::read(&[8, 1, 1, 0, 0x61]).is_none());
        assert!(CharCode::read(&[8, 1, 1, 0, 0xE9, 0x01]).is_some());
        // A page wider than the mode's, and no leads at all.
        assert!(CharCode::read(&[7, 129, 1, 1, 128]).is_none());
        assert!(CharCode::read(&[7, 1, 0]).is_none());
    }

    #[test]
    fn a_character_above_every_codeword_places_the_query_past_its_prefix() {
        let keys = ["a", "ab", "ac", "b"];
        let code = CharCode::forced(&keys, true, 0);
        let mut buf = [0u8; 16];
        // `a` then a character above all four: past `ac`, before `b`.
        let (p, len) = code.probe_into("a\u{10ffff}", &mut buf);
        assert_eq!(p, Probe::Absent);
        let probe = buf[..len].to_vec();
        // The bytes may be a key's own code; that key is not below them, as it is not below the
        // query.
        assert!(code.encoded("ac") < probe && probe <= code.encoded("b"));
        // Nothing before it, so every key is below.
        assert_eq!(code.probe_into("\u{10ffff}", &mut buf).0, Probe::Last);
    }

    #[test]
    fn the_choice_keeps_a_code_only_where_it_pays() {
        let mut latin = Tally::new();
        for w in ["apple", "banana", "cherry", "naïve"] {
            latin.add(w);
        }
        assert!(latin.choose().is_none(), "nothing to save");
        // Keys of three characters each from 2 000 Chinese ones: nine bytes a key in UTF-8, six
        // coded, for a table of about two kilobytes — and no character frequent enough for the
        // singles to cover a tenth of what is written. Five thousand keys save 15 kB, too few to
        // pay for the table out of what the blob keeps of them; sixty thousand save 180.
        let chinese = |keys: usize| {
            let mut tally = Tally::new();
            let mut x = 7u32;
            for _ in 0..keys {
                let key: String = (0..3)
                    .map(|_| {
                        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        char::from_u32(0x4E00 + (x >> 16) % 2000).unwrap()
                    })
                    .collect();
                tally.add(&key);
            }
            tally
        };
        assert!(
            chinese(5000).choose().is_none(),
            "a table the blob does not earn back"
        );
        let code = chinese(60_000)
            .choose()
            .expect("a third of the bytes is worth a table");
        assert_eq!(code.mode, Mode::Seven);
        assert!(
            code.firsts.windows(2).all(|p| p[1] - p[0] > 1),
            "no singles"
        );
        // Russian: a few letters are most of what is written, and take a byte each.
        let mut russian = Tally::new();
        for _ in 0..500 {
            russian.add("программирование на языке");
        }
        let code = russian
            .choose()
            .expect("a byte a letter where UTF-8 spends two");
        assert!(
            code.firsts.windows(2).all(|p| p[1] - p[0] == 1),
            "all singles"
        );
        assert!(code.encoded("программа").len() == 9);
    }

    #[test]
    fn the_eight_bit_mode_takes_an_alphabet_seven_bits_cannot() {
        let mut tally = Tally::new();
        // 20 000 distinct characters: more than 128 pages of 128, fewer than 128 of 256. Their
        // table is 20 kB, and written 25 times over they save the twenty times that it needs.
        let big: String = (0x4E00..0x4E00 + 20_000)
            .filter_map(char::from_u32)
            .collect();
        for _ in 0..25 {
            tally.add(&big);
        }
        let code = tally.choose().expect("fits whole bytes");
        assert_eq!(code.mode, Mode::Eight);
        let code = roundtrip(&code);
        let mut back = Vec::new();
        assert!(code.decode_into(&code.encoded(&big), &mut back));
        assert_eq!(back, big.as_bytes());
        // Past 32 768 characters no code fits.
        let mut huge = Tally::new();
        huge.add(
            &(0x4E00..0x4E00 + 33_000)
                .filter_map(char::from_u32)
                .collect::<String>(),
        );
        assert!(huge.choose().is_none());
    }

    #[test]
    fn worth_counting_is_the_bound_a_code_can_reach() {
        assert!(!worth_counting(100, 13));
        assert!(worth_counting(30, 4));
        assert_eq!(high_bytes("aé中".as_bytes()), 5);
    }
}
