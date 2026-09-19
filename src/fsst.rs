//! A static symbol table for the suffixes a [`DictIndex`](crate::DictIndex) stores, after FSST
//! (Boncz, Neumann and Leis, VLDB 2020) and in a table format of its own: up to 255 symbols of
//! one to eight bytes, a one-byte
//! code for each, and the code `255` followed by the byte itself for what no symbol covers.
//!
//! The table is trained once per shard of blocks, over a sample of the pieces that shard will
//! store, by the encoder's own parse: five rounds of parse-and-count, each keeping the 255
//! candidates — the current symbols and every adjacent pair up to eight bytes — that cover the
//! most bytes. A symbol of three bytes or
//! more is accepted only if the encoder's slot for its first three bytes is still free, so every
//! symbol in the table is one the encoder can reach and a round's counts mean what they say. The
//! encoder has the reference's shape: per two-byte prefix the best symbol of one or two bytes, and
//! a hash of the first three bytes to at most one longer symbol, so a position costs one eight-byte
//! load and at most two lookups. Decoding is one eight-byte store per code.

use crate::phrase::Map;
use crate::room::{Room, commit};

pub(crate) const ESCAPE: u8 = 255;
const MAX_SYMBOLS: usize = 255;
const MAX_LEN: usize = 8;
/// Three rounds gave 5.204 bytes per key on the dictionary, five 5.185, eight 5.184.
const ROUNDS: usize = 5;
const SLOTS: usize = 1024;

/// The symbols in code order, each as the little-endian word of its bytes and its length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Table {
    words: Vec<u64>,
    lens: Vec<u8>,
}

/// At most eight bytes as a little-endian word, zero-padded.
#[inline(always)]
fn word_of(bytes: &[u8]) -> u64 {
    let mut w = [0u8; 8];
    w[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(w)
}

/// The eight bytes at `s[i..]` as a little-endian word, zero-padded past the end.
///
/// The last eight bytes of a long enough slice hold every byte from `i` on, so the tail is that
/// load shifted down rather than a gather: this runs once per entry compared, and on a short key
/// most of the compares are in the tail. Only a slice under eight bytes long is gathered, a byte
/// at a time — a copy of a run-time length is a `memcpy` call.
#[inline(always)]
pub(crate) fn word_at(s: &[u8], i: usize) -> u64 {
    match s.get(i..i + 8) {
        Some(w) => u64::from_le_bytes(w.try_into().unwrap()),
        None => match (s.last_chunk::<8>(), i < s.len()) {
            (Some(w), true) => u64::from_le_bytes(*w) >> (8 * (i + 8 - s.len())),
            _ => s
                .get(i..)
                .unwrap_or_default()
                .iter()
                .enumerate()
                .fold(0, |w, (k, &b)| w | u64::from(b) << (8 * k)),
        },
    }
}

/// The low `len` bytes of a word.
#[inline(always)]
pub(crate) fn low_mask(len: usize) -> u64 {
    if len >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * len)) - 1
    }
}

/// The encoder's slot for a symbol of three bytes or more: a hash of its first three bytes.
#[inline(always)]
fn slot_of(word: u64) -> usize {
    ((word & 0xFF_FFFF).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 54) as usize
}

/// One slot of the encoder: a symbol of three to eight bytes as a word, its byte mask, its code
/// and its length; `len == 0` is an empty slot.
#[derive(Clone, Copy, Default)]
struct Slot {
    word: u64,
    mask: u64,
    code: u8,
    len: u8,
}

/// The greedy encoder over one [`Table`].
pub(crate) struct Encoder {
    /// `code | len << 8` per two-byte prefix; `ESCAPE | 1 << 8` where no symbol starts so.
    short: Vec<u16>,
    /// The same for the last byte of a string, where only a one-byte symbol applies.
    single: [u16; 256],
    slots: Vec<Slot>,
}

impl Table {
    /// The symbols in the order given, which becomes their code order.
    fn from_symbols<'a>(syms: impl IntoIterator<Item = &'a [u8]>) -> Table {
        let mut t = Table::default();
        for s in syms {
            debug_assert!((1..=MAX_LEN).contains(&s.len()));
            t.words.push(word_of(s));
            t.lens.push(s.len() as u8);
        }
        debug_assert!(t.words.len() <= MAX_SYMBOLS);
        t
    }

    pub(crate) fn len(&self) -> usize {
        self.words.len()
    }

    /// The symbol behind `code` as (word, length); `None` for a code past the table, which is
    /// where the escape always is.
    #[inline(always)]
    pub(crate) fn symbol(&self, code: u8) -> Option<(u64, usize)> {
        let i = code as usize;
        Some((*self.words.get(i)?, self.lens[i] as usize))
    }

    /// The first `k` symbols, which the trainer ranked highest. What a shard prices its candidate
    /// splits under before it knows which one it will keep: dropping a symbol only frees encoder
    /// slots, never takes one from a symbol that outranks it, so the prefix is a table.
    pub(crate) fn head(&self, k: usize) -> Table {
        let k = k.min(self.words.len());
        Table {
            words: self.words[..k].to_vec(),
            lens: self.lens[..k].to_vec(),
        }
    }

    /// Train over `sample`, the pieces the index will store.
    pub(crate) fn train(sample: &[&[u8]]) -> Table {
        Table::train_to(sample, MAX_SYMBOLS)
    }

    /// Train a table of at most `max` symbols. A shard that gives byte codes to the blob's
    /// [phrases](crate::phrase) has fewer than 255 to spend and trains for the ones it kept: which
    /// symbols earn most is not the same question once the repeated spans are named elsewhere, and
    /// truncating a table trained for 255 answers the wrong one.
    pub(crate) fn train_to(sample: &[&[u8]], max: usize) -> Table {
        let mut table = Table::default();
        for _ in 0..ROUNDS {
            let enc = table.encoder();
            let codes = table.len();
            // Pieces are symbols or single bytes, so a piece is a code: the symbols first, then
            // the 256 bytes. Counted in flat arrays instead of hashing strings.
            let space = codes + 256;
            // What every code stands for, once a round: the inner pair loop asked for this
            // 261 121 times a round, and all but 511 of the answers were the row's own.
            let piece: Vec<(u64, usize)> = (0..space)
                .map(|code| {
                    if code < codes {
                        (table.words[code], table.lens[code] as usize)
                    } else {
                        (u64::from((code - codes) as u8), 1)
                    }
                })
                .collect();
            let mut count1 = vec![0u32; space];
            let mut count2 = vec![0u32; space * space];
            for &s in sample {
                let mut i = 0;
                let mut prev = usize::MAX;
                while i < s.len() {
                    let (code, len) = enc.step(word_at(s, i), s.len() - i);
                    let code = if code == ESCAPE {
                        codes + s[i] as usize
                    } else {
                        code as usize
                    };
                    count1[code] += 1;
                    // Single bytes are counted at every position, so they stay in the running
                    // against the symbols that cover them.
                    if len > 1 {
                        for &b in &s[i..i + len] {
                            count1[codes + b as usize] += 1;
                        }
                    }
                    if prev != usize::MAX {
                        count2[prev * space + code] += 1;
                    }
                    prev = code;
                    i += len;
                }
            }
            // A candidate is at most eight bytes, so its gain is keyed on its word and its
            // length: keyed on a `Vec` of its bytes, the allocations were a third of the trainer.
            let mut gain: Map<(u64, u8), u64> = Map::default();
            for (&c, &(word, len)) in count1.iter().zip(&piece) {
                if c > 0 {
                    *gain.entry((word, len as u8)).or_default() += u64::from(c) * len as u64;
                }
            }
            for (&(wa, la), row) in piece.iter().zip(count2.chunks_exact(space)) {
                // A row already at the length cap concatenates with nothing.
                if la == MAX_LEN {
                    continue;
                }
                for (&c, &(wb, lb)) in row.iter().zip(&piece) {
                    if c == 0 || la + lb > MAX_LEN {
                        continue;
                    }
                    *gain
                        .entry((wa | wb << (8 * la), (la + lb) as u8))
                        .or_default() += u64::from(c) * (la + lb) as u64;
                }
            }
            // The rank is (gain down, bytes up); comparing the bytes as one big-endian word and
            // then the length orders them the same way without a slice compare a step.
            let mut ranked: Vec<(u64, u64, usize, [u8; 8])> = Vec::with_capacity(gain.len());
            ranked.extend(gain.into_iter().map(|((word, len), g)| {
                let bytes = word.to_le_bytes();
                (!g, u64::from_be_bytes(bytes), usize::from(len), bytes)
            }));
            ranked.sort_unstable_by_key(|&(g, be, len, _)| (g, be, len));
            table = Table::reachable(ranked.iter().map(|(_, _, len, s)| &s[..*len]), max);
        }
        table
    }

    /// The first `max` of `ranked` the encoder can reach: a symbol of three bytes or more only if
    /// its slot is still free.
    fn reachable<'a>(ranked: impl Iterator<Item = &'a [u8]>, max: usize) -> Table {
        let max = max.min(MAX_SYMBOLS);
        let mut taken = vec![false; SLOTS];
        let mut syms: Vec<&[u8]> = Vec::with_capacity(max);
        for s in ranked {
            if syms.len() == max {
                break;
            }
            if s.len() >= 3 && std::mem::replace(&mut taken[slot_of(word_of(s))], true) {
                continue;
            }
            syms.push(s);
        }
        Table::from_symbols(syms)
    }

    pub(crate) fn encoder(&self) -> Encoder {
        let esc = u16::from(ESCAPE) | 1 << 8;
        let mut short = vec![esc; 1 << 16];
        let mut single = [esc; 256];
        let mut slots = vec![Slot::default(); SLOTS];
        for (code, (&word, &len)) in self.words.iter().zip(&self.lens).enumerate() {
            let entry = code as u16 | u16::from(len) << 8;
            match len {
                1 => {
                    let b = (word & 0xFF) as usize;
                    single[b] = entry;
                    // Behind every two-byte prefix nothing longer claims.
                    for second in 0..256 {
                        let e = &mut short[b | second << 8];
                        if *e == esc {
                            *e = entry;
                        }
                    }
                }
                2 => short[(word & 0xFFFF) as usize] = entry,
                _ => {
                    let slot = &mut slots[slot_of(word)];
                    if slot.len < len {
                        *slot = Slot {
                            word,
                            mask: low_mask(len as usize),
                            code: code as u8,
                            len,
                        };
                    }
                }
            }
        }
        Encoder {
            short,
            single,
            slots,
        }
    }

    /// `packed` decoded onto the end of `out`, stopping once `cap` bytes are there; `false`, with
    /// `out` cut back to where it was, for a stream this table did not write — a code past the
    /// table, or an escape with nothing after it.
    pub(crate) fn decode_into(&self, packed: &[u8], out: &mut Vec<u8>, cap: usize) -> bool {
        // Every code becomes one eight-byte store and advances by its length, so the slack past
        // the decoded end is at most seven bytes per code — and past a cap, seven bytes in all.
        let mut room = Room::of(
            out,
            packed.len().saturating_mul(8).min(cap.saturating_add(7)),
        );
        let mut o = 0;
        let mut i = 0;
        while i < packed.len() && o < cap {
            let code = packed[i];
            if code == ESCAPE {
                let Some(&b) = packed.get(i + 1) else {
                    return false;
                };
                room.byte(o, b);
                o += 1;
                i += 2;
            } else {
                let Some((word, len)) = self.symbol(code) else {
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

    /// `[count u8][lengths count][the symbols' bytes, end to end]`.
    pub(crate) fn serialized_len(&self) -> usize {
        1 + self.lens.len() + self.lens.iter().map(|&l| l as usize).sum::<usize>()
    }

    pub(crate) fn write_to(&self, out: &mut Vec<u8>) {
        out.push(self.lens.len() as u8);
        out.extend_from_slice(&self.lens);
        for (&w, &l) in self.words.iter().zip(&self.lens) {
            out.extend_from_slice(&w.to_le_bytes()[..l as usize]);
        }
    }

    /// The table [`write_to`](Self::write_to) wrote, if `bytes` is exactly that.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Table> {
        let (&count, rest) = bytes.split_first()?;
        let (lens, mut rest) = rest.split_at_checked(count as usize)?;
        let mut t = Table {
            words: Vec::with_capacity(lens.len()),
            lens: lens.to_vec(),
        };
        for &l in lens {
            if !(1..=MAX_LEN as u8).contains(&l) {
                return None;
            }
            let (s, r) = rest.split_at_checked(l as usize)?;
            t.words.push(word_of(s));
            rest = r;
        }
        rest.is_empty().then_some(t)
    }
}

impl Encoder {
    /// What the encoder emits at a position: (code, bytes covered), for the word there and the
    /// `rem` bytes left in the string. The code is `ESCAPE` for a byte nothing covers.
    #[inline(always)]
    pub(crate) fn step(&self, word: u64, rem: usize) -> (u8, usize) {
        let e = &self.slots[slot_of(word)];
        if e.len != 0 && e.len as usize <= rem && word & e.mask == e.word {
            return (e.code, e.len as usize);
        }
        let sc = if rem >= 2 {
            self.short[(word & 0xFFFF) as usize]
        } else {
            self.single[(word & 0xFF) as usize]
        };
        (sc as u8, (sc >> 8) as usize)
    }

    pub(crate) fn encode_into(&self, s: &[u8], out: &mut Vec<u8>) {
        let mut i = 0;
        while i < s.len() {
            let (code, len) = self.step(word_at(s, i), s.len() - i);
            if code == ESCAPE {
                out.push(ESCAPE);
                out.push(s[i]);
            } else {
                out.push(code);
            }
            i += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_keys() -> Vec<&'static str> {
        include_str!("../tests/data/golden-keys.txt")
            .lines()
            .collect()
    }

    fn roundtrip(table: &Table, keys: &[&[u8]]) -> usize {
        let enc = table.encoder();
        let mut packed = Vec::new();
        let mut back = Vec::new();
        let mut total = 0;
        for &k in keys {
            packed.clear();
            enc.encode_into(k, &mut packed);
            total += packed.len();
            back.clear();
            back.extend_from_slice(b"prefix-");
            assert!(table.decode_into(&packed, &mut back, usize::MAX), "{k:?}");
            assert_eq!(&back[7..], k, "{k:?}");
        }
        total
    }

    #[test]
    fn a_trained_table_round_trips_and_compresses_the_golden_keys() {
        let keys = golden_keys();
        let sample: Vec<&[u8]> = keys.iter().map(|k| k.as_bytes()).collect();
        let table = Table::train(&sample);
        assert!(table.len() > 100 && table.len() <= MAX_SYMBOLS);
        let raw: usize = sample.iter().map(|k| k.len()).sum();
        let packed = roundtrip(&table, &sample);
        assert!(packed * 3 < raw * 2, "{packed} of {raw} raw bytes");
        // Deterministic: the same sample gives the same table.
        assert_eq!(Table::train(&sample), table);
        let mut bytes = Vec::new();
        table.write_to(&mut bytes);
        assert_eq!(bytes.len(), table.serialized_len());
        assert_eq!(Table::from_bytes(&bytes), Some(table));
    }

    #[test]
    fn an_empty_table_escapes_every_byte() {
        let table = Table::train(&[]);
        assert_eq!(table.len(), 0);
        let mut packed = Vec::new();
        table.encoder().encode_into(b"ab", &mut packed);
        assert_eq!(packed, [ESCAPE, b'a', ESCAPE, b'b']);
        let mut back = Vec::new();
        assert!(table.decode_into(&packed, &mut back, usize::MAX));
        assert_eq!(back, b"ab");
        let mut bytes = Vec::new();
        table.write_to(&mut bytes);
        assert_eq!(bytes, [0]);
    }

    #[test]
    fn nul_bytes_and_short_tails_are_bytes_like_any_other() {
        let keys: Vec<&[u8]> = vec![b"a\0b", b"a\0", b"\0\0\0", b"", b"a", b"a\0b\0c\0d\0e\0"];
        let table = Table::train(&keys);
        roundtrip(&table, &keys);
    }

    #[test]
    fn a_stream_the_table_did_not_write_is_refused_and_leaves_out_as_it_was() {
        let table = Table::train(&[b"abc".as_slice()]);
        let mut out = b"keep".to_vec();
        assert!(!table.decode_into(&[ESCAPE], &mut out, usize::MAX));
        assert_eq!(out, b"keep");
        assert!(!table.decode_into(&[254], &mut out, usize::MAX));
        assert_eq!(out, b"keep");
    }

    #[test]
    fn table_bytes_are_checked_to_the_last_byte() {
        assert_eq!(Table::from_bytes(&[]), None);
        assert_eq!(Table::from_bytes(&[1]), None); // no length
        assert_eq!(Table::from_bytes(&[1, 0, b'a']), None); // a length of zero
        assert_eq!(Table::from_bytes(&[1, 9, b'a']), None); // past eight
        assert_eq!(Table::from_bytes(&[1, 2, b'a']), None); // cut short
        assert_eq!(Table::from_bytes(&[1, 1, b'a', b'b']), None); // trailing byte
        let t = Table::from_bytes(&[2, 1, 3, b'a', b'x', b'y', b'z']).unwrap();
        assert_eq!(t.symbol(0), Some((u64::from(b'a'), 1)));
        assert_eq!(t.symbol(1), Some((word_of(b"xyz"), 3)));
        assert_eq!(t.symbol(2), None);
    }
}
