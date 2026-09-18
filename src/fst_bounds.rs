//! Whether `fst`'s node decoder can read a node without leaving the blob.
//!
//! `fst` is safe Rust, so the worst a crafted transducer can do is panic — but a panic is a denial
//! of service, and under `panic = "abort"` it is not even catchable. The loader that reads blobs
//! from an untrusted source therefore answers the question here, on its own, before handing an
//! address to `fst`: given these bytes and this address, does every read the decoder will make land
//! inside the node, and does every field it will divide or subtract by hold a value it can?
//!
//! What this is not is a second decoder. It reads exactly the fields whose values decide the size
//! of a node — the state byte, the pack sizes, the transition count and the packed deltas — and
//! computes the same offsets `fst` will; it reads no input byte, no output value and no transition
//! index, because none of those can move a read. An address this module accepts is one `fst` can
//! decode; an address it rejects is a blob this crate refuses, which is the only outcome a hostile
//! blob is entitled to.
//!
//! The offsets mirror `fst` 0.4.7's `raw::node`. They are a property of the serialised format,
//! which is versioned in the blob's own header (`fst::raw::VERSION`, 3 since 2019) and cannot
//! change under a caller without the version changing with it.

/// `fst`'s `EMPTY_ADDRESS`: the address of the empty final node, which is not a node in the blob.
const EMPTY: usize = 0;
/// Transitions above which a node carries a 256-byte index of them, from version 2 on.
const INDEXED: usize = 32;

/// The address `fst` will decode the root node from, read from the footer the way `Fst::new` reads
/// it, and `None` unless it names a byte of this blob.
///
/// `Fst::new` reads the same bytes and then checks them only when the address is zero: its
/// `(root == EMPTY && len != 36) && root + 21 != len` short-circuits on the first conjunct, so a
/// blob naming a root past its own end is accepted there and `Fst::root` indexes the blob with it.
///
/// Version 3 keeps a four-byte checksum after the footer and versions 1 and 2 do not, which is the
/// only thing that moves between them; `Fst::new` refuses every other version, so only those three
/// reach here.
pub(crate) fn root_addr(bytes: &[u8]) -> Option<usize> {
    // `Fst::new`'s own floor: below it the version, type, root and length overlap.
    let (head, _) = bytes
        .split_first_chunk::<8>()
        .filter(|_| bytes.len() >= 36)?;
    let end = if u64::from_le_bytes(*head) <= 2 {
        bytes.len()
    } else {
        bytes.len().checked_sub(4)?
    };
    let at = end.checked_sub(8)?;
    let root = usize::try_from(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?)).ok()?;
    (root < bytes.len()).then_some(root)
}

/// The blob's `fst` format version, which decides whether a node carries a transition index.
pub(crate) fn version(bytes: &[u8]) -> u64 {
    bytes
        .split_first_chunk::<8>()
        .map_or(0, |(head, _)| u64::from_le_bytes(*head))
}

/// A `u64` packed little-endian in `n` bytes ending at `at` (exclusive), as `fst` writes them.
fn unpack(bytes: &[u8], at: usize, n: usize) -> Option<u64> {
    let mut v = 0u64;
    for (i, &b) in bytes.get(at..at.checked_add(n)?)?.iter().enumerate() {
        v |= u64::from(b) << (8 * i);
    }
    Some(v)
}

/// Whether the packed transition deltas of a node all point at a byte of the blob.
///
/// A delta is subtracted from the node's end address, so one larger than that address underflows.
/// In a release build the difference wraps and the walk's own rule — a transition points strictly
/// below the node holding it — rejects the node a step later; in a debug build the subtraction
/// panics first. Checking it here is what makes the walk total in both.
fn deltas_fit(bytes: &[u8], first: usize, count: usize, size: usize, end: usize) -> bool {
    (0..count).all(|i| {
        // The deltas are written in reverse, so the `i`-th sits `i * size` bytes below the first.
        let at = match first.checked_sub(i * size) {
            Some(at) => at,
            None => return false,
        };
        match unpack(bytes, at, size) {
            Some(delta) => delta == EMPTY as u64 || usize::try_from(delta).is_ok_and(|d| d <= end),
            None => false,
        }
    })
}

/// Whether `fst` can decode the node at `addr` without reading outside `bytes`.
///
/// `addr` is the node's *last* byte, which is how `fst` addresses nodes: a node is written
/// backwards, so its fields are found by subtracting from `addr`, and every one of those
/// subtractions is what this checks.
pub(crate) fn node_fits(bytes: &[u8], version: u64, addr: usize) -> bool {
    if addr == EMPTY {
        // The empty final node is a state, not bytes: `fst` decodes it without reading anything.
        return true;
    }
    let Some(&state) = bytes.get(addr) else {
        return false;
    };
    match state >> 6 {
        // One transition, to the node written just before this one: an optional input byte, and an
        // address that is this node's end less one.
        0b11 => {
            let input = usize::from(state & 0b0011_1111 == 0);
            // The transition's address is this node's end less one, so the end cannot be the
            // first byte of the blob.
            addr > input
        }
        // One transition, with its own packed address and output.
        0b10 => {
            let input = usize::from(state & 0b0011_1111 == 0);
            let Some(sizes) = addr.checked_sub(input + 1).and_then(|at| bytes.get(at)) else {
                return false;
            };
            let (tsize, osize) = (usize::from(sizes >> 4), usize::from(sizes & 0b1111));
            // `unpack_uint` takes one to eight bytes and asserts it; a nibble can say more, and a
            // transition is always unpacked, so zero is out of range for it too.
            if !(1..=8).contains(&tsize) || osize > 8 {
                return false;
            }
            let Some(end) = addr.checked_sub(input + 1 + tsize + osize) else {
                return false;
            };
            deltas_fit(bytes, addr - input - 1 - tsize, 1, tsize, end)
        }
        // Any number of transitions, each with a packed address and output, optionally behind a
        // 256-byte index, and a final output of the node's own.
        _ => {
            let count_len = usize::from(state & 0b0011_1111 == 0);
            let Some(sizes) = addr.checked_sub(count_len + 1).and_then(|at| bytes.get(at)) else {
                return false;
            };
            let (tsize, osize) = (usize::from(sizes >> 4), usize::from(sizes & 0b1111));
            let ntrans = if count_len == 0 {
                usize::from(state & 0b0011_1111)
            } else {
                // One transition is always in the state byte, so `fst` reuses the value for 256.
                match addr.checked_sub(1).and_then(|at| bytes.get(at)) {
                    Some(&1) => 256,
                    Some(&n) => usize::from(n),
                    None => return false,
                }
            };
            if (ntrans > 0 && !(1..=8).contains(&tsize)) || osize > 8 {
                return false;
            }
            let index = usize::from(version >= 2 && ntrans > INDEXED) * 256;
            let is_final = state & 0b0100_0000 != 0;
            let Some(end) = addr.checked_sub(
                count_len
                    + 1
                    + ntrans
                    + ntrans * tsize
                    + index
                    + ntrans * osize
                    + if is_final { osize } else { 0 },
            ) else {
                return false;
            };
            ntrans == 0
                || deltas_fit(
                    bytes,
                    addr - count_len - 1 - index - ntrans - tsize,
                    ntrans,
                    tsize,
                    end,
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The footer is the only place the root address is written, and a blob that names one past
    /// its own end is the panic `fst` carries: it reads the same bytes and checks them only when
    /// the address is zero.
    #[test]
    fn a_root_past_the_blob_is_not_a_root() {
        let mut bytes = vec![0u8; 64];
        bytes[..8].copy_from_slice(&3u64.to_le_bytes());
        let n = bytes.len();
        for root in [u64::MAX, n as u64] {
            bytes[n - 12..n - 4].copy_from_slice(&root.to_le_bytes());
            assert_eq!(root_addr(&bytes), None, "root {root}");
        }
        bytes[n - 12..n - 4].copy_from_slice(&(n as u64 - 21).to_le_bytes());
        assert_eq!(root_addr(&bytes), Some(n - 21));
        // Versions 1 and 2 keep no checksum, so the footer sits four bytes later.
        bytes[..8].copy_from_slice(&2u64.to_le_bytes());
        bytes[n - 8..].copy_from_slice(&7u64.to_le_bytes());
        assert_eq!(root_addr(&bytes), Some(7));
        assert_eq!(root_addr(&bytes[..35]), None, "no room for a footer");
        assert_eq!(root_addr(&[]), None);
    }

    #[test]
    fn a_node_claiming_more_bytes_than_sit_below_it_does_not_fit() {
        // One transition, its address and output packed in eight bytes each: nineteen bytes below
        // a node that has three.
        assert!(!node_fits(&[0, 0, 0x88, 0b1000_0001, 0], 3, 3));
        // The same node with sizes it can afford: one byte of address, no output.
        assert!(node_fits(&[0, 0, 0x01, 0x10, 0b1000_0001, 0], 3, 4));
        // A transition size of zero is not one `fst` can unpack, and nine is past what it packs.
        for sizes in [0x00u8, 0x90] {
            let bytes = [0, 0, 0x01, sizes, 0b1000_0001, 0];
            assert!(!node_fits(&bytes, 3, 4), "sizes {sizes:#x}");
        }
    }

    #[test]
    fn a_node_whose_transition_points_above_its_end_does_not_fit() {
        // The delta is subtracted from the node's end, so one larger than it wraps.
        assert!(!node_fits(&[0, 0, 0xFF, 0x10, 0b1000_0001, 0], 3, 4));
        assert!(node_fits(&[0, 0, 0x01, 0x10, 0b1000_0001, 0], 3, 4));
        // Zero is `fst`'s empty final node rather than a delta, and always legal.
        assert!(node_fits(&[0, 0, 0x00, 0x10, 0b1000_0001, 0], 3, 4));
    }

    /// The many-transition shape, where the count can come from the state byte or the byte below
    /// it, and a node past the threshold carries 256 bytes of index a short blob cannot hold.
    #[test]
    fn a_node_of_many_transitions_is_measured_by_its_count() {
        // Two transitions in the state byte, one byte of address each, no outputs: five bytes
        // below the node -- the pack sizes, two inputs, two addresses.
        let mut bytes = vec![0u8; 8];
        bytes[7] = 0b0000_0010;
        bytes[6] = 0x10;
        assert!(node_fits(&bytes, 3, 7));
        // The same node one byte short of its own transitions.
        assert!(!node_fits(&bytes[3..], 3, 4));
        // Thirty-three transitions ask for an index of 256 bytes from version 2 on and for none at
        // version 1; eight bytes are short of either.
        bytes[7] = 33;
        assert!(!node_fits(&bytes, 3, 7));
        assert!(!node_fits(&bytes, 1, 7));
        let mut wide = vec![0u8; 400];
        wide[300] = 33;
        wide[299] = 0x10;
        assert!(
            !node_fits(&wide, 3, 300),
            "256 of index over 66 of transitions"
        );
        assert!(node_fits(&wide, 1, 300), "no index at version 1");
    }

    #[test]
    fn the_empty_node_reads_nothing_and_fits_anything() {
        assert!(node_fits(&[], 3, 0));
        assert!(!node_fits(&[], 3, 1));
        assert!(!node_fits(&[0b1000_0001], 3, 1), "one byte is not a node");
    }
}
