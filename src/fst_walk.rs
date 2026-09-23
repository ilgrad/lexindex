//! `fst`'s node format read one step at a time, for the walks that follow a single path.
//!
//! `fst::raw::Fst::node` decodes a whole node before a caller can ask it anything — its pack
//! sizes, transition count, end address and final output — and `find_input` and `transition` then
//! decode the step itself. A walk down one path pays for all of that at every node it passes,
//! though a step needs only two answers: whether the node is final, and where one input byte leads.
//! This module reads those, from the same bytes, and nothing else.
//!
//! The offsets are those of `fst` 0.4.7's `raw::node`, which [`fst_bounds`](crate::fst_bounds)
//! mirrors too: that module measures a node before `fst` may decode it, this one reads it. Every
//! read is bounds-checked, so a malformed node can panic here or answer wrong, as it can in `fst`'s
//! own decoder, but no read leaves the blob; and on a blob `from_untrusted_bytes` accepted,
//! `node_fits` has vouched for every byte a step reads.

use crate::fst_bounds;

/// `fst`'s `EMPTY_ADDRESS`: the empty final node, a state rather than bytes in the blob.
const EMPTY: usize = 0;

/// `fst::raw::common_inputs::COMMON_INPUTS_INV`, as far as a state byte can index it: a
/// one-transition node whose input is one of these stores it in the six low bits of its state.
const COMMON_INPUTS: [u8; 63] = *b"te/oasripcnw.hlm-du012g=:bf3y5&_4v9678k%?xCDASFIBEjPTzRNM+LOqHG";

/// Where a transducer's walks start, and which of its nodes carry a transition index: what a
/// [`StringIndex`](crate::StringIndex) keeps beside its map, read from the blob once at load.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    root: usize,
    /// A node with more transitions than this carries a 256-byte index of them: 32 from format
    /// version 2 on, and none in version 1.
    indexed_above: usize,
}

impl Layout {
    /// `None` unless the footer names a root inside the blob — the check `Fst::new` skips.
    pub(crate) fn of(bytes: &[u8]) -> Option<Self> {
        Some(Self {
            root: fst_bounds::root_addr(bytes)?,
            indexed_above: if fst_bounds::version(bytes) >= 2 {
                32
            } else {
                usize::MAX
            },
        })
    }
}

/// A little-endian integer of `n` bytes at `at`, as `fst` packs one; one unaligned load where the
/// blob has eight bytes to give.
#[inline(always)]
fn uint(bytes: &[u8], at: usize, n: usize) -> u64 {
    if n == 0 {
        return 0;
    }
    match bytes.get(at..at.wrapping_add(8)) {
        Some(&[a, b, c, d, e, f, g, h]) => {
            u64::from_le_bytes([a, b, c, d, e, f, g, h]) & (u64::MAX >> (64 - 8 * n.min(8)))
        }
        _ => bytes[at..at + n]
            .iter()
            .rev()
            .fold(0, |v, &b| v << 8 | u64::from(b)),
    }
}

/// The address a packed delta of `size` bytes at `at` leads to from a node ending at `end`: zero
/// is the empty final node, anything else is subtracted from the end.
#[inline(always)]
fn target(bytes: &[u8], at: usize, size: usize, end: usize) -> usize {
    match uint(bytes, at, size) as usize {
        EMPTY => EMPTY,
        delta => end - delta,
    }
}

/// The input of a one-transition node, and how many bytes it takes below the state byte.
#[inline(always)]
fn one_input(bytes: &[u8], addr: usize, state: u8) -> (u8, usize) {
    match state & 0b0011_1111 {
        0 => (bytes[addr - 1], 1),
        common => (COMMON_INPUTS[usize::from(common) - 1], 0),
    }
}

impl Layout {
    /// The node at `addr`: its final output if it is final, and — for `input`, if it has one — the
    /// transition's target and the output on the edge.
    #[inline(always)]
    fn step(
        self,
        bytes: &[u8],
        addr: usize,
        input: Option<u8>,
    ) -> (Option<u64>, Option<(usize, u64)>) {
        if addr == EMPTY {
            return (Some(0), None);
        }
        let state = bytes[addr];
        match state >> 6 {
            // One transition, never final, to the node written just before this one: no output.
            0b11 => {
                let (inp, len) = one_input(bytes, addr, state);
                (None, (input == Some(inp)).then(|| (addr - len - 1, 0)))
            }
            // One transition, never final, with a packed target and output of its own.
            0b10 => {
                let (inp, len) = one_input(bytes, addr, state);
                if input != Some(inp) {
                    return (None, None);
                }
                let sizes = addr - len - 1;
                let (tsize, osize) = (
                    usize::from(bytes[sizes] >> 4),
                    usize::from(bytes[sizes] & 15),
                );
                let end = sizes - tsize - osize;
                (
                    None,
                    Some((
                        target(bytes, sizes - tsize, tsize, end),
                        uint(bytes, end, osize),
                    )),
                )
            }
            // Any number of transitions: inputs, then targets, then outputs, then the final
            // output, each written downwards from the pack sizes.
            _ => {
                let is_final = state & 0b0100_0000 != 0;
                let (ntrans, count_len) = match state & 0b0011_1111 {
                    // One transition always fits the state byte, so `fst` reuses 1 for 256.
                    0 => match bytes[addr - 1] {
                        1 => (256, 1),
                        n => (usize::from(n), 1),
                    },
                    n => (usize::from(n), 0),
                };
                let sizes = addr - count_len - 1;
                let (tsize, osize) = (
                    usize::from(bytes[sizes] >> 4),
                    usize::from(bytes[sizes] & 15),
                );
                let index = if ntrans > self.indexed_above { 256 } else { 0 };
                let inputs = sizes - index - ntrans;
                let outputs = inputs - ntrans * tsize;
                let final_output =
                    is_final.then(|| uint(bytes, outputs - (ntrans + 1) * osize, osize));
                let Some(b) = input else {
                    return (final_output, None);
                };
                let i = if index != 0 {
                    match usize::from(bytes[sizes - 256 + usize::from(b)]) {
                        i if i < ntrans => i,
                        _ => return (final_output, None),
                    }
                } else {
                    // Inputs are written in reverse, the first transition's nearest the sizes.
                    match bytes[inputs..sizes].iter().position(|&x| x == b) {
                        Some(p) => ntrans - p - 1,
                        None => return (final_output, None),
                    }
                };
                let end = outputs - ntrans * osize - if is_final { osize } else { 0 };
                let to = target(bytes, inputs - (i + 1) * tsize, tsize, end);
                (
                    final_output,
                    Some((to, uint(bytes, outputs - (i + 1) * osize, osize))),
                )
            }
        }
    }

    /// `f(end, value)` at every final state `query` passes on a character boundary, the empty
    /// prefix first: the keys that are prefixes of `query`, shortest first, each `&query[..end]`.
    ///
    /// Sums are checked, as the rank walk checks them: a transition whose output would overflow
    /// ends the walk, and a final output that would is not reported.
    #[inline(always)]
    pub(crate) fn common_prefixes(self, bytes: &[u8], query: &str, mut f: impl FnMut(usize, u64)) {
        let q = query.as_bytes();
        let (mut addr, mut acc) = (self.root, 0u64);
        for i in 0..=q.len() {
            let (fin, next) = self.step(bytes, addr, q.get(i).copied());
            // A final state inside a character cannot be a key this crate built, since keys come
            // from `&str` — but `from_bytes` loads any transducer, and slicing there would panic.
            if let Some(v) = fin.and_then(|out| acc.checked_add(out)) {
                if query.is_char_boundary(i) {
                    f(i, v);
                }
            }
            let Some((to, out)) = next else { return };
            let Some(sum) = acc.checked_add(out) else {
                return;
            };
            (addr, acc) = (to, sum);
        }
    }

    /// The value stored for `key`, or `None`: `fst::Map::get`, with its sums checked.
    #[inline(always)]
    pub(crate) fn get(self, bytes: &[u8], key: &[u8]) -> Option<u64> {
        let (mut addr, mut acc) = (self.root, 0u64);
        for &b in key {
            let (to, out) = self.step(bytes, addr, Some(b)).1?;
            (addr, acc) = (to, acc.checked_add(out)?);
        }
        acc.checked_add(self.step(bytes, addr, None).0?)
    }
}
