//! Compact `slot → &str` storage: a contiguous byte buffer plus `n + 1` offsets, so a key is the
//! slice `data[offsets[i]..offsets[i + 1]]`. Backs [`PerfectHashIndex`](crate::PerfectHashIndex),
//! whose `id()` hot path needs the stored key (zero-copy `&str`) to verify membership exactly.
//! (`StringIndex` stores no keys — it reconstructs them from the FST by a rank-walk — and
//! `CompactHashIndex` stores only fingerprints.) It views a [`SharedBytes`], so a memory-mapped load
//! borrows it without copying.

use crate::blob::SharedBytes;
use crate::IndexError;

/// A contiguous arena of UTF-8 strings addressable by index, viewing a shared byte source.
#[derive(Clone, Debug)]
pub(crate) struct StringArena {
    blob: SharedBytes, // [n_off: u64][offsets: n_off × u64][data]
    n: usize,          // number of strings == n_off - 1
    data_start: usize, // 8 + n_off * 8
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
        let n_off = offsets.len();
        let mut blob = Vec::with_capacity(8 + n_off * 8 + data.len());
        blob.extend_from_slice(&(n_off as u64).to_le_bytes());
        for o in &offsets {
            blob.extend_from_slice(&o.to_le_bytes());
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
        let lo = read_u64(bytes, 8 + i * 8).ok()? as usize;
        let hi = read_u64(bytes, 8 + (i + 1) * 8).ok()? as usize;
        let start = self.data_start.checked_add(lo)?;
        let end = self.data_start.checked_add(hi)?;
        std::str::from_utf8(bytes.get(start..end)?).ok()
    }

    /// Serialise to `[n+1: u64][offsets: (n+1) × u64][data]` — the layout it already holds.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        self.blob.as_ref().to_vec()
    }

    /// Parse an owned blob (copies once) — a test convenience; production loads go through
    /// [`StringArena::from_shared`] (owned or memory-mapped).
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// View a shared blob without copying, validating the header (untrusted input): the offset table
    /// must fit and its ends must span the data. Individual offsets are bounds-checked lazily in
    /// [`get`](StringArena::get), so a memory-mapped load stays instant (no `O(n)` scan).
    pub(crate) fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        let n_off = read_u64(bytes, 0)? as usize;
        if n_off == 0 {
            return Err(IndexError::Format("arena: zero offsets (need at least 1)"));
        }
        let data_start = 8usize
            .checked_add(
                n_off
                    .checked_mul(8)
                    .ok_or(IndexError::Format("arena: offset table too large"))?,
            )
            .ok_or(IndexError::Format("arena: offset table too large"))?;
        if bytes.len() < data_start {
            return Err(IndexError::Format("arena: truncated offset table"));
        }
        let data_len = bytes.len() - data_start;
        if read_u64(bytes, 8)? != 0 || read_u64(bytes, 8 + (n_off - 1) * 8)? as usize != data_len {
            return Err(IndexError::Format("arena: offsets do not span the data"));
        }
        Ok(Self {
            blob,
            n: n_off - 1,
            data_start,
        })
    }
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, IndexError> {
    let end = at
        .checked_add(8)
        .ok_or(IndexError::Format("arena: offset overflow"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(IndexError::Format("arena: unexpected end of buffer"))?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
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
    fn rejects_corrupt_headers() {
        assert!(StringArena::from_bytes(b"short").is_err()); // < 8-byte header
        let mut good = StringArena::build(["a", "b"]).to_bytes();
        good[0] = 0xff; // absurd offset count → truncated table
        assert!(StringArena::from_bytes(&good).is_err());
    }
}
