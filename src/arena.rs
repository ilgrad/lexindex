//! Compact `slot → &str` storage: a contiguous byte buffer plus `n + 1` offsets, so a key is the
//! slice `data[offsets[i]..offsets[i + 1]]`. Backs [`PerfectHashIndex`](crate::PerfectHashIndex),
//! whose `id()` hot path needs the stored key (zero-copy `&str`) to verify membership exactly.
//! (`StringIndex` stores no keys — it reconstructs them from the FST by a rank-walk — and
//! `CompactHashIndex` stores only fingerprints.) It views a [`SharedBytes`], so a memory-mapped load
//! borrows it without copying.
//!
//! # Offset width
//!
//! Offsets are **4 bytes** unless the arena needs more than 4 GiB, in which case they are 8. An
//! 8-byte offset was the whole table's width before 0.5.0 and dominated the index: on the 479 823
//! word dictionary it cost 8.0 of `PerfectHashIndex`'s 17.6 bytes per key, to address a 4.9 MB
//! arena. Choosing the width per arena rather than capping it keeps corpora above 4 GiB working;
//! the cost is one branch in [`get`](StringArena::get), on a field that never changes for the life
//! of the index and so predicts perfectly.

use crate::IndexError;
use crate::blob::SharedBytes;

/// Byte width of an offset in an arena small enough for 32-bit offsets, and in one that is not.
const NARROW: usize = 4;
const WIDE: usize = 8;
/// `[n_off: u64][width: u8]`.
const HEADER: usize = 9;

/// A contiguous arena of UTF-8 strings addressable by index, viewing a shared byte source.
#[derive(Clone, Debug)]
pub(crate) struct StringArena {
    blob: SharedBytes, // [n_off: u64][width: u8][offsets: n_off × width][data]
    n: usize,          // number of strings == n_off - 1
    data_start: usize, // HEADER + n_off * width
    width: usize,      // NARROW or WIDE
}

impl StringArena {
    /// Build from strings in index order (`items[i]` becomes key `i`).
    pub(crate) fn build<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut data = Vec::new();
        let mut offsets = vec![0u64];
        for s in items {
            data.extend_from_slice(s.as_ref().as_bytes());
            offsets.push(data.len() as u64);
        }
        let width = if data.len() <= u32::MAX as usize {
            NARROW
        } else {
            WIDE
        };
        let n_off = offsets.len();
        let mut blob = Vec::with_capacity(HEADER + n_off * width + data.len());
        blob.extend_from_slice(&(n_off as u64).to_le_bytes());
        blob.push(width as u8);
        for o in &offsets {
            match width {
                NARROW => blob.extend_from_slice(&(*o as u32).to_le_bytes()),
                _ => blob.extend_from_slice(&o.to_le_bytes()),
            }
        }
        blob.extend_from_slice(&data);
        Self::from_shared(SharedBytes::from_owned(blob)).expect("freshly built arena is valid")
    }

    /// Number of stored strings.
    pub(crate) fn len(&self) -> usize {
        self.n
    }

    /// The string at index `i`, or `None` if out of range. Borrows the shared source — zero-copy.
    pub(crate) fn get(&self, i: usize) -> Option<&str> {
        if i >= self.n {
            return None;
        }
        let bytes = self.blob.as_ref();
        let at = HEADER + i * self.width;
        let lo = read_offset(bytes, at, self.width).ok()? as usize;
        let hi = read_offset(bytes, at + self.width, self.width).ok()? as usize;
        let start = self.data_start.checked_add(lo)?;
        let end = self.data_start.checked_add(hi)?;
        std::str::from_utf8(bytes.get(start..end)?).ok()
    }

    /// Prefetch the cache line holding slot `i`'s offset pair (pipelined batch lookups).
    #[inline(always)]
    pub(crate) fn prefetch_offsets(&self, i: usize) {
        if i < self.n {
            crate::blob::prefetch_byte(self.blob.as_ref(), HEADER + i * self.width);
        }
    }

    /// Absolute `(start, end)` byte span of slot `i` in the blob, or `None` if out of range /
    /// corrupt. Splitting `get` into span + [`str_at`](Self::str_at) lets a batch caller prefetch
    /// the data bytes between the two.
    #[inline(always)]
    pub(crate) fn span(&self, i: usize) -> Option<(usize, usize)> {
        if i >= self.n {
            return None;
        }
        let bytes = self.blob.as_ref();
        let at = HEADER + i * self.width;
        let lo = read_offset(bytes, at, self.width).ok()? as usize;
        let hi = read_offset(bytes, at + self.width, self.width).ok()? as usize;
        Some((
            self.data_start.checked_add(lo)?,
            self.data_start.checked_add(hi)?,
        ))
    }

    #[inline(always)]
    pub(crate) fn prefetch_span(&self, span: (usize, usize)) {
        crate::blob::prefetch_byte(self.blob.as_ref(), span.0);
    }

    #[inline(always)]
    pub(crate) fn str_at(&self, span: (usize, usize)) -> Option<&str> {
        std::str::from_utf8(self.blob.as_ref().get(span.0..span.1)?).ok()
    }

    /// The serialised layout, borrowed: the arena already *is* its own blob, so writing it out
    /// never needs a copy.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.blob.as_ref()
    }

    /// Serialise to the layout it already holds (a test convenience; production paths borrow
    /// [`as_bytes`](Self::as_bytes)).
    #[cfg(test)]
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        self.blob.as_ref().to_vec()
    }

    /// Parse an owned blob (copies once) — a test convenience; production loads go through
    /// [`StringArena::from_shared`] (owned or memory-mapped).
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// View a shared blob without copying, validating the header (untrusted input): the width must
    /// be one this format defines, the offset table must fit, and its ends must span the data.
    /// Individual offsets are bounds-checked lazily in [`get`](StringArena::get), so a
    /// memory-mapped load stays instant (no `O(n)` scan).
    pub(crate) fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        let n_off = read_u64(bytes, 0)? as usize;
        if n_off == 0 {
            return Err(IndexError::Format("arena: zero offsets (need at least 1)"));
        }
        let width = match bytes.get(8) {
            Some(&w) if w as usize == NARROW => NARROW,
            Some(&w) if w as usize == WIDE => WIDE,
            _ => return Err(IndexError::Format("arena: unknown offset width")),
        };
        let data_start = n_off
            .checked_mul(width)
            .and_then(|t| t.checked_add(HEADER))
            .ok_or(IndexError::Format("arena: offset table too large"))?;
        if bytes.len() < data_start {
            return Err(IndexError::Format("arena: truncated offset table"));
        }
        let data_len = bytes.len() - data_start;
        let last = HEADER + (n_off - 1) * width;
        if read_offset(bytes, HEADER, width)? != 0
            || read_offset(bytes, last, width)? as usize != data_len
        {
            return Err(IndexError::Format("arena: offsets do not span the data"));
        }
        Ok(Self {
            blob,
            n: n_off - 1,
            data_start,
            width,
        })
    }
}

fn read_offset(bytes: &[u8], at: usize, width: usize) -> Result<u64, IndexError> {
    let end = at
        .checked_add(width)
        .ok_or(IndexError::Format("arena: offset overflow"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(IndexError::Format("arena: unexpected end of buffer"))?;
    Ok(match width {
        NARROW => u32::from_le_bytes(slice.try_into().unwrap()) as u64,
        _ => u64::from_le_bytes(slice.try_into().unwrap()),
    })
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, IndexError> {
    read_offset(bytes, at, WIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_get_and_roundtrip() {
        let arena = StringArena::build(["apple", "banana", "", "cherry"]);
        assert_eq!(arena.len(), 4);
        assert_eq!(arena.get(0), Some("apple"));
        assert_eq!(arena.get(2), Some("")); // empty string is a valid entry
        assert_eq!(arena.get(3), Some("cherry"));
        assert_eq!(arena.get(4), None); // out of range
        let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
        assert_eq!(restored.len(), 4);
        assert_eq!(restored.get(1), Some("banana"));
    }

    #[test]
    fn empty_arena_roundtrips() {
        let arena = StringArena::build(Vec::<&str>::new());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.get(0), None);
        assert_eq!(StringArena::from_bytes(&arena.to_bytes()).unwrap().len(), 0);
    }

    #[test]
    fn small_arenas_use_narrow_offsets() {
        let arena = StringArena::build(["apple", "banana"]);
        assert_eq!(arena.width, NARROW);
        // header + 3 narrow offsets + "applebanana"
        assert_eq!(arena.to_bytes().len(), HEADER + 3 * NARROW + 11);
    }

    /// The wide path cannot be reached by building a >4 GiB arena in a test, so drive it through
    /// the parser: a hand-written wide blob must load and read back identically.
    #[test]
    fn wide_offsets_round_trip() {
        let data = b"applebanana";
        let mut blob = Vec::new();
        blob.extend_from_slice(&3u64.to_le_bytes());
        blob.push(WIDE as u8);
        for o in [0u64, 5, 11] {
            blob.extend_from_slice(&o.to_le_bytes());
        }
        blob.extend_from_slice(data);
        let arena = StringArena::from_bytes(&blob).unwrap();
        assert_eq!(arena.width, WIDE);
        assert_eq!(arena.get(0), Some("apple"));
        assert_eq!(arena.get(1), Some("banana"));
        assert_eq!(arena.get(2), None);
    }

    #[test]
    fn rejects_corrupt_headers() {
        assert!(StringArena::from_bytes(b"short").is_err()); // < 8-byte header
        let mut good = StringArena::build(["a", "b"]).to_bytes();
        good[0] = 0xff; // absurd offset count → truncated table
        assert!(StringArena::from_bytes(&good).is_err());

        let mut bad_width = StringArena::build(["a", "b"]).to_bytes();
        bad_width[8] = 3; // neither 4 nor 8
        assert!(StringArena::from_bytes(&bad_width).is_err());
    }
}
