//! A non-decreasing `u64` array as one base every `1 << SHIFT` entries and a narrow delta from it
//! for each entry, so that reading entry `i` stays two loads and no branch.
//!
//! [`DictIndex`](crate::DictIndex) keeps two of these — where each block's head key ends, and
//! where its entries start. Both grow by one block's worth at a time, so a superblock's entries
//! sit within a few kilobytes of its base and the delta needs a dozen bits rather than sixty-four.
//! On the dictionary at the default block that is 12 bytes a block down to 3, which is 0.28 bytes
//! a key, and it is also why neither array has a four-gigabyte ceiling: the base is a full word.

use crate::blob::SharedBytes;

/// Entries under one base, as a shift. A base costs `128 >> SHIFT` bits a block across the two
/// arrays and a wider superblock spans more bytes, so the delta widens by about a bit each time
/// the shift goes up: on the dictionary at the default block, shifts 3/4/5/6/7/8 measured
/// 33/27/25/25/26/27 bits a block for both arrays together. Five and six tie at the floor, and
/// six is the one whose bases array is half the size.
pub(crate) const SHIFT: u32 = 6;
/// The widest delta a blob may declare. A read is one eight-byte load at a byte offset, so the
/// bits of one entry must fit a word after a shift of at most seven.
pub(crate) const MAX_WIDTH: u32 = 56;

/// Bytes the bases of `n` entries take.
pub(crate) fn bases_len(n: usize, shift: u32) -> usize {
    n.div_ceil(1 << shift) * 8
}

/// Bytes the deltas of `n` entries take, with the eight-byte tail that lets the last one be read
/// as a whole word.
pub(crate) fn deltas_len(n: usize, width: u32) -> usize {
    if n == 0 {
        0
    } else {
        (n * width as usize).div_ceil(8) + 8
    }
}

/// What both sections of `n` entries take together, or `None` where that is not a length this
/// platform can address. The arithmetic runs in `u64` because the count comes out of a header and
/// a 32-bit reader must refuse an impossible one rather than wrap on the way to refusing it.
pub(crate) fn section_len(n: usize, width: u32, shift: u32) -> Option<usize> {
    let n = n as u64;
    let bases = n.div_ceil(1u64 << shift).checked_mul(8)?;
    let deltas = if n == 0 {
        0
    } else {
        n.checked_mul(u64::from(width))?
            .div_ceil(8)
            .checked_add(8)?
    };
    usize::try_from(bases.checked_add(deltas)?).ok()
}

/// The narrowest delta width that covers `values`, which must not decrease.
pub(crate) fn width_of(values: &[u64], shift: u32) -> u32 {
    let span = values
        .chunks(1 << shift)
        .map(|c| c[c.len() - 1] - c[0])
        .max()
        .unwrap_or(0);
    64 - span.leading_zeros()
}

/// `values` as (bases, deltas), under the width [`width_of`] gives for the same `shift`.
pub(crate) fn pack(values: &[u64], shift: u32, width: u32) -> (Vec<u8>, Vec<u8>) {
    let mut bases = Vec::with_capacity(bases_len(values.len(), shift));
    for chunk in values.chunks(1 << shift) {
        bases.extend_from_slice(&chunk[0].to_le_bytes());
    }
    let mut deltas = vec![0u8; deltas_len(values.len(), width)];
    for (i, &v) in values.iter().enumerate() {
        let delta = v - values[(i >> shift) << shift];
        let bit = i * width as usize;
        let at = bit / 8;
        let word = u64::from_le_bytes(deltas[at..at + 8].try_into().expect("8 bytes"))
            | delta << (bit % 8);
        deltas[at..at + 8].copy_from_slice(&word.to_le_bytes());
    }
    (bases, deltas)
}

/// One packed array, as the two sections it was read from.
pub(crate) struct Offsets {
    bases: SharedBytes,
    deltas: SharedBytes,
    width: u32,
    shift: u32,
}

impl Offsets {
    /// The sections as they lie in a blob. Their lengths must be the ones [`bases_len`] and
    /// [`deltas_len`] give for the entry count, which is what the loader checks.
    pub(crate) fn new(bases: SharedBytes, deltas: SharedBytes, width: u32, shift: u32) -> Offsets {
        Offsets {
            bases,
            deltas,
            width,
            shift,
        }
    }

    /// Entry `i`, which must be one of the entries packed.
    #[inline(always)]
    pub(crate) fn at(&self, i: usize) -> u64 {
        let at = (i >> self.shift) * 8;
        let base = u64::from_le_bytes(self.bases[at..at + 8].try_into().expect("8 bytes"));
        let bit = i * self.width as usize;
        let word = u64::from_le_bytes(
            self.deltas[bit / 8..bit / 8 + 8]
                .try_into()
                .expect("8 bytes"),
        );
        // Wrapping: a blob nothing walked can name any base at all, and a read past the end of
        // a section is a wrong answer the caller bounds, not an overflow to panic on.
        base.wrapping_add((word >> (bit % 8)) & ((1u64 << self.width) - 1))
    }

    /// Entries `i` and `i + 1`. Every read on the search path is a span between two entries —
    /// a head, a block's data — and neighbours share a base and, unless the width is wide enough
    /// that two deltas cannot meet in one word, the word their deltas are cut from. So the pair
    /// costs what one entry costs.
    #[inline(always)]
    pub(crate) fn pair(&self, i: usize) -> (u64, u64) {
        if 2 * self.width as usize + 7 > 64 {
            return (self.at(i), self.at(i + 1));
        }
        let base = |j: usize| {
            let at = j * 8;
            u64::from_le_bytes(self.bases[at..at + 8].try_into().expect("8 bytes"))
        };
        let (j, k) = (i >> self.shift, (i + 1) >> self.shift);
        let first = base(j);
        let second = if j == k { first } else { base(k) };
        let bit = i * self.width as usize;
        let word = u64::from_le_bytes(
            self.deltas[bit / 8..bit / 8 + 8]
                .try_into()
                .expect("8 bytes"),
        ) >> (bit % 8);
        let mask = (1u64 << self.width) - 1;
        (
            first.wrapping_add(word & mask),
            second.wrapping_add((word >> self.width) & mask),
        )
    }

    /// Pull in what entry `i` will be read from: a read touches both arrays, so both are named.
    #[inline(always)]
    pub(crate) fn prefetch(&self, i: usize) {
        crate::blob::prefetch_byte(&self.bases, (i >> self.shift) * 8);
        crate::blob::prefetch_byte(&self.deltas, i * self.width as usize / 8);
    }

    pub(crate) fn sections(&self) -> [&SharedBytes; 2] {
        [&self.bases, &self.deltas]
    }

    /// Bytes both sections take together.
    pub(crate) fn len(&self) -> usize {
        self.bases.len() + self.deltas.len()
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn shift(&self) -> u32 {
        self.shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(values: &[u64], shift: u32) -> (u32, usize) {
        let width = width_of(values, shift);
        let (bases, deltas) = pack(values, shift, width);
        assert_eq!(bases.len(), bases_len(values.len(), shift));
        assert_eq!(deltas.len(), deltas_len(values.len(), width));
        let o = Offsets::new(
            SharedBytes::from_owned(bases),
            SharedBytes::from_owned(deltas),
            width,
            shift,
        );
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(o.at(i), v, "entry {i} of {values:?}");
            if i + 1 < values.len() {
                assert_eq!(o.pair(i), (v, values[i + 1]), "pair {i} of {values:?}");
            }
        }
        (width, o.bases.len() + o.deltas.len())
    }

    #[test]
    fn a_packed_array_reads_back_every_entry() {
        assert_eq!(roundtrip(&[], 6).0, 0);
        // A lone entry is its own base, so there is nothing left for a delta to say.
        assert_eq!(roundtrip(&[7], 6), (0, 16));
        // A run that never moves needs no delta at all.
        assert_eq!(roundtrip(&[9; 500], 6).0, 0);
        let steps: Vec<u64> = (0..1000).map(|i| i * i % 97 + i * 13).collect();
        let mut running = 0;
        let cumulative: Vec<u64> = steps
            .iter()
            .map(|s| {
                running += s;
                running
            })
            .collect();
        for shift in [1, 4, 6, 10] {
            roundtrip(&cumulative, shift);
        }
    }

    /// A width past twenty-eight cannot fit two deltas in a word, so the pair falls back —
    /// and still answers what two reads answer.
    #[test]
    fn a_wide_array_reads_its_pairs_one_entry_at_a_time() {
        let values: Vec<u64> = (0..200).map(|i| i * (1 << 30)).collect();
        let (width, _) = roundtrip(&values, 6);
        assert!(width > 28, "{width}");
    }

    #[test]
    fn a_wide_jump_only_widens_its_own_superblock_scheme() {
        // The width is global, so one long block widens every delta -- but the bases mean the
        // jump itself costs nothing, which is what keeps the array off sixty-four bits.
        let mut values: Vec<u64> = (0..128).map(|i| i * 10).collect();
        values[64..].iter_mut().for_each(|v| *v += 1 << 40);
        let (width, bytes) = roundtrip(&values, 6);
        assert!(width <= 11, "{width}");
        assert!(bytes < 128 * 8, "{bytes}");
    }

    #[test]
    fn the_last_entry_of_a_full_word_is_still_a_whole_word_read() {
        // Sixty-four entries of eight bits end exactly on a byte, so the tail is what keeps the
        // read in bounds.
        let values: Vec<u64> = (0..64).map(|i| i * 4).collect();
        assert_eq!(roundtrip(&values, 6).0, 8);
    }
}
