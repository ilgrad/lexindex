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

/// The state a walk is in once it has read a query's first character, for every character of the
/// Basic Multilingual Plane that begins a key.
///
/// A walk down a lexicon of Chinese words spends most of its instructions on its first character:
/// the root and the two nodes below it take its three bytes, each node decoded in full, before the
/// walk reaches the words that begin with it. This answers those steps with one lookup — a page of
/// 64 code points, a bit for each, and a rank into the states. A character outside the plane is left
/// to the walk; one inside it that no key begins with ends the walk.
pub(crate) struct FirstChars {
    /// The empty key's value, if the index holds it: the one match before any character.
    root: Option<u64>,
    /// Bit `c % 64` of `bits[c / 64]` is set when code point `c` begins a key.
    bits: Box<[u64]>,
    /// Where page `p`'s first state is in `states`.
    base: Box<[u32]>,
    /// `(addr, sum)` for each character that begins a key, in code point order: the node the walk
    /// reaches past the character's bytes, and the outputs summed on the way there.
    states: Box<[(u32, u32)]>,
}

impl FirstChars {
    /// `None` when a state does not fit the table's `u32`s — past 4 GiB of blob or 4 Gi keys — and
    /// the walk then reads the first character as it reads every other.
    pub(crate) fn derive<D: AsRef<[u8]>>(fst: &fst::raw::Fst<D>) -> Option<Self> {
        let root = fst.root();
        let mut found = Vec::new();
        let mut spelled = [0u8; 3];
        for t in root.transitions() {
            let len = match t.inp {
                0x00..=0x7F => 1,
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                // Four bytes, outside the plane, or no character's first byte at all.
                _ => continue,
            };
            spelled[0] = t.inp;
            descend(fst, t.addr, t.out.value(), &mut spelled, 1, len, &mut found)?;
        }
        let pages = found.last().map_or(0, |&(cp, ..)| cp as usize / 64 + 1);
        let (mut bits, mut base) = (vec![0u64; pages], vec![0u32; pages]);
        for (i, &(cp, ..)) in found.iter().enumerate() {
            let page = cp as usize / 64;
            if bits[page] == 0 {
                base[page] = u32::try_from(i).ok()?;
            }
            bits[page] |= 1 << (cp % 64);
        }
        Some(Self {
            root: root.is_final().then(|| root.final_output().value()),
            bits: bits.into(),
            base: base.into(),
            states: found
                .into_iter()
                .map(|(_, addr, sum)| (addr, sum))
                .collect(),
        })
    }

    /// The state past `c`: `Some(None)` when no key begins with it, `None` when `c` is outside the
    /// plane the table holds.
    #[inline(always)]
    fn after(&self, c: char) -> Option<Option<(usize, u64)>> {
        let cp = u32::from(c);
        if cp > 0xFFFF {
            return None;
        }
        let page = (cp / 64) as usize;
        let bit = 1u64 << (cp % 64);
        let bits = self.bits.get(page).copied().unwrap_or(0);
        if bits & bit == 0 {
            return Some(None);
        }
        let (addr, sum) =
            self.states[self.base[page] as usize + (bits & (bit - 1)).count_ones() as usize];
        Some(Some((addr as usize, u64::from(sum))))
    }

    /// The bytes the table holds on the heap.
    #[cfg(test)]
    pub(crate) fn heap_len(&self) -> usize {
        self.bits.len() * 8 + self.base.len() * 4 + self.states.len() * 8
    }
}

/// Every character of one UTF-8 length that `spelled[..len]` begins, from the node at `addr`,
/// pushed as `(code point, addr, sum)` in code point order. `None` when a state does not fit `u32`.
fn descend<D: AsRef<[u8]>>(
    fst: &fst::raw::Fst<D>,
    addr: usize,
    sum: u64,
    spelled: &mut [u8; 3],
    len: usize,
    want: usize,
    found: &mut Vec<(u32, u32, u32)>,
) -> Option<()> {
    if len == want {
        // A byte path no `&str` spells is one no query reaches: an overlong or surrogate form must
        // not take the place of the character it would decode to.
        if let Some(c) = std::str::from_utf8(&spelled[..len])
            .ok()
            .and_then(|s| s.chars().next())
        {
            found.push((
                u32::from(c),
                u32::try_from(addr).ok()?,
                u32::try_from(sum).ok()?,
            ));
        }
        return Some(());
    }
    for t in fst.node(addr).transitions() {
        // A walk ends where a sum would overflow, so a character past one begins no match.
        let (0x80..=0xBF, Some(next)) = (t.inp, sum.checked_add(t.out.value())) else {
            continue;
        };
        spelled[len] = t.inp;
        descend(fst, t.addr, next, spelled, len + 1, want, found)?;
    }
    Some(())
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
    pub(crate) fn common_prefixes(
        self,
        bytes: &[u8],
        first: Option<&FirstChars>,
        query: &str,
        mut f: impl FnMut(usize, u64),
    ) {
        let q = query.as_bytes();
        let (mut addr, mut acc, mut from) = (self.root, 0u64, 0);
        if let Some(table) = first {
            let c = query.chars().next();
            if let Some(past) = c.and_then(|c| table.after(c)) {
                if let Some(v) = table.root {
                    f(0, v);
                }
                let Some((to, sum)) = past else { return };
                (addr, acc, from) = (to, sum, c.map_or(0, char::len_utf8));
            }
        }
        for i in from..=q.len() {
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
