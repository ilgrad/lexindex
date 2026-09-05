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
    /// Build from strings in index order (`items[i]` becomes key `i`), deriving the count and the
    /// total byte length from a first pass over the iterator — hence the `Clone` bound.
    ///
    /// Knowing both totals up front is what lets the arena be assembled in one exactly-sized buffer
    /// with the offsets written in place. Collecting the data and the offsets separately and
    /// concatenating them afterwards, as this did before 0.10, held two copies of the whole corpus
    /// alive across the concatenation — on a 2 M-key
    /// [`PerfectHashIndex`](crate::PerfectHashIndex) build that was 36 MB of the peak.
    ///
    /// A caller that already knows the totals should use [`build_exact`](Self::build_exact): the
    /// first pass looks cheap (it reads string *lengths*, never their bytes) but it follows the
    /// caller's permutation, so it is one cache miss per key.
    pub(crate) fn build<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        I::IntoIter: Clone,
        S: AsRef<str>,
    {
        let items = items.into_iter();
        let (n, data_len) = items.clone().fold((0usize, 0usize), |(n, len), s| {
            (n + 1, len + s.as_ref().len())
        });
        Self::assemble(items, n, data_len).expect("totals came from the iterator itself")
    }

    /// [`build`](Self::build) for a caller that already knows how many strings there are and how
    /// many bytes they hold. `PerfectHashIndex` does: both fall out of the hashing pass it runs
    /// anyway, so handing them over removes a whole permuted walk over the keys.
    ///
    /// The totals are a shortcut, not a promise the arena relies on. If they turn out not to match
    /// what the iterator yields, the assembly is discarded and [`build`](Self::build) redoes it
    /// from the iterator alone — a wrong hint costs time, never a malformed arena.
    pub(crate) fn build_exact<I, S>(items: I, n: usize, data_len: usize) -> Self
    where
        I: IntoIterator<Item = S>,
        I::IntoIter: Clone,
        S: AsRef<str>,
    {
        let items = items.into_iter();
        match Self::assemble(items.clone(), n, data_len) {
            Some(arena) => arena,
            None => Self::build(items),
        }
    }

    /// Write the whole arena into one buffer sized for `n` strings holding `data_len` bytes, with
    /// each offset written as soon as its string lands. `None` if the iterator disagrees with
    /// either total — the caller decides what to do about it.
    fn assemble<I, S>(items: I, n: usize, data_len: usize) -> Option<Self>
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        let width = if data_len <= u32::MAX as usize {
            NARROW
        } else {
            WIDE
        };
        let n_off = n + 1;
        let data_start = HEADER + n_off * width;
        let mut blob = Vec::with_capacity(data_start + data_len);
        blob.extend_from_slice(&(n_off as u64).to_le_bytes());
        blob.push(width as u8);
        blob.resize(data_start, 0); // offset table, filled in as the data lands after it
        write_offset(&mut blob, HEADER, width, 0);
        let mut count = 0usize;
        for s in items {
            blob.extend_from_slice(s.as_ref().as_bytes());
            count += 1;
            if count > n {
                return None; // more strings than the table was sized for
            }
            let end = (blob.len() - data_start) as u64;
            write_offset(&mut blob, HEADER + count * width, width, end);
        }
        // A narrow table that turned out to need wide offsets has silently truncated them, so the
        // width is re-derived from what was actually written rather than from the promise.
        if count != n || (width == NARROW && blob.len() - data_start > u32::MAX as usize) {
            return None;
        }
        Some(
            Self::from_shared(SharedBytes::from_owned(blob)).expect("freshly built arena is valid"),
        )
    }

    /// The arena's fixed prefix — `[n_off u64][width u8][offsets]` — for strings whose lengths are
    /// known in index order but whose bytes are not yet available. Returns the prefix, the data
    /// length the offsets describe, and the offset width.
    ///
    /// This is what lets a builder place the keys without ever holding them: the offset table is
    /// derivable from the lengths alone, so the bytes can be written afterwards, out of order,
    /// straight into a mapped file. Read one back with [`offset_at`](Self::offset_at).
    pub(crate) fn prefix_for_lengths(lens: &[u32]) -> (Vec<u8>, usize, usize) {
        let data_len: usize = lens.iter().map(|&l| l as usize).sum();
        let width = if data_len <= u32::MAX as usize {
            NARROW
        } else {
            WIDE
        };
        let n_off = lens.len() + 1;
        let mut blob = vec![0u8; HEADER + n_off * width];
        blob[..8].copy_from_slice(&(n_off as u64).to_le_bytes());
        blob[8] = width as u8;
        let mut at = 0u64;
        write_offset(&mut blob, HEADER, width, 0);
        for (i, &l) in lens.iter().enumerate() {
            at += l as u64;
            write_offset(&mut blob, HEADER + (i + 1) * width, width, at);
        }
        (blob, data_len, width)
    }

    /// Offset `i` out of a prefix built by [`prefix_for_lengths`](Self::prefix_for_lengths).
    pub(crate) fn offset_at(prefix: &[u8], width: usize, i: usize) -> u64 {
        read_offset(prefix, HEADER + i * width, width).unwrap_or(0)
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
        let lo = usize::try_from(read_offset(bytes, at, self.width).ok()?).ok()?;
        let hi = usize::try_from(read_offset(bytes, at + self.width, self.width).ok()?).ok()?;
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
        let lo = usize::try_from(read_offset(bytes, at, self.width).ok()?).ok()?;
        let hi = usize::try_from(read_offset(bytes, at + self.width, self.width).ok()?).ok()?;
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
        // Header-supplied, so converted checked: a count that does not fit this platform's `usize`
        // must fail here rather than truncate on a 32-bit target.
        let n_off = usize::try_from(read_u64(bytes, 0)?)
            .map_err(|_| IndexError::Format("arena: offset count out of range"))?;
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
            || read_offset(bytes, last, width)? != data_len as u64
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

/// Write one offset of the arena's own width. Only [`StringArena::build`] calls this, into the
/// table it has already reserved, so an out-of-range `at` is a bug in this file.
fn write_offset(bytes: &mut [u8], at: usize, width: usize, off: u64) {
    match width {
        NARROW => bytes[at..at + NARROW].copy_from_slice(&(off as u32).to_le_bytes()),
        _ => bytes[at..at + WIDE].copy_from_slice(&off.to_le_bytes()),
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

    /// The serialised layout is a format, not an implementation detail: every blob any released
    /// version wrote is parsed by [`from_shared`](StringArena::from_shared) above, so a build that
    /// quietly changed a byte would break `load` on existing files. Pinned in full rather than by
    /// length — the 0.10 rewrite that assembles the arena in one buffer, offsets written in place,
    /// had to prove it produced exactly what the two-buffer version did.
    #[test]
    fn small_arenas_use_narrow_offsets() {
        let arena = StringArena::build(["apple", "banana"]);
        assert_eq!(arena.width, NARROW);
        let mut want = Vec::new();
        want.extend_from_slice(&3u64.to_le_bytes()); // n_off = 2 keys + 1
        want.push(NARROW as u8);
        for o in [0u32, 5, 11] {
            want.extend_from_slice(&o.to_le_bytes());
        }
        want.extend_from_slice(b"applebanana");
        assert_eq!(arena.to_bytes(), want);
        assert_eq!(want.len(), HEADER + 3 * NARROW + 11);
    }

    /// The totals a caller passes to `build_exact` are a shortcut, not a promise: whatever they
    /// say, the arena must come out exactly as `build` would have derived it. A count that is too
    /// small is caught mid-fill, one that is too large at the end, and a byte total that is merely
    /// wrong only mis-sizes the initial allocation.
    #[test]
    fn a_wrong_hint_never_changes_the_arena() {
        let items = ["apple", "banana", "", "cherry"]; // 17 bytes over 4 strings
        let want = StringArena::build(items).to_bytes();
        for (n, data_len) in [(4, 17), (3, 17), (5, 17), (0, 0), (4, 0), (4, 999)] {
            let got = StringArena::build_exact(items, n, data_len);
            assert_eq!(got.to_bytes(), want, "hint n={n}, data_len={data_len}");
            assert_eq!(got.get(3), Some("cherry"));
        }
    }

    /// An empty entry between two others, and one at each end: the offset written for key `i` is
    /// the *end* of its bytes, so a zero-length key is the case where two consecutive offsets are
    /// equal and an off-by-one in the fill loop would not otherwise show.
    #[test]
    fn empty_keys_keep_their_slots() {
        let arena = StringArena::build(["", "a", "", "bc", ""]);
        assert_eq!(arena.len(), 5);
        let got: Vec<Option<&str>> = (0..6).map(|i| arena.get(i)).collect();
        assert_eq!(
            got,
            [Some(""), Some("a"), Some(""), Some("bc"), Some(""), None]
        );
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
