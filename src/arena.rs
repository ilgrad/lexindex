//! Compact `id → &str` storage: one contiguous byte buffer plus `n + 1` offsets, so a key is a
//! slice `data[offsets[i]..offsets[i + 1]]`. This avoids the per-`String` allocation/overhead of a
//! `Vec<String>` reverse map while keeping `O(1)` random access, and serialises to a flat blob.

use crate::IndexError;

/// A contiguous arena of UTF-8 strings addressable by index.
#[derive(Clone, Debug, Default)]
pub(crate) struct StringArena {
    data: Vec<u8>,
    offsets: Vec<u64>, // length n + 1; offsets[0] == 0, offsets[n] == data.len()
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
        Self { data, offsets }
    }

    /// Number of stored strings.
    pub(crate) fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    /// The string at index `i`, or `None` if out of range.
    pub(crate) fn get(&self, i: usize) -> Option<&str> {
        let lo = *self.offsets.get(i)? as usize;
        let hi = *self.offsets.get(i + 1)? as usize;
        // Built only from `&str`, so the bytes are valid UTF-8; tolerate corruption defensively.
        std::str::from_utf8(self.data.get(lo..hi)?).ok()
    }

    /// Serialise to `[n+1: u64][offsets: (n+1) × u64][data]` (little-endian).
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.offsets.len() * 8 + self.data.len());
        out.extend_from_slice(&(self.offsets.len() as u64).to_le_bytes());
        for &o in &self.offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// Parse the [`StringArena::to_bytes`] layout, validating every length and offset (untrusted input).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let n_off = read_u64(bytes, 0)? as usize;
        if n_off == 0 {
            return Err(IndexError::Format("arena: zero offsets (need at least 1)"));
        }
        let data_start = 8 + n_off * 8;
        if bytes.len() < data_start {
            return Err(IndexError::Format("arena: truncated offset table"));
        }
        let mut offsets = Vec::with_capacity(n_off);
        for i in 0..n_off {
            offsets.push(read_u64(bytes, 8 + i * 8)?);
        }
        let data = bytes[data_start..].to_vec();
        if offsets[0] != 0 || *offsets.last().unwrap() as usize != data.len() {
            return Err(IndexError::Format("arena: offsets do not span the data"));
        }
        if offsets.windows(2).any(|w| w[1] < w[0]) {
            return Err(IndexError::Format("arena: non-monotone offsets"));
        }
        Ok(Self { data, offsets })
    }
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, IndexError> {
    let end = at + 8;
    let slice = bytes
        .get(at..end)
        .ok_or(IndexError::Format("arena: unexpected end of buffer"))?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}
