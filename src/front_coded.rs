//! Front-coded string dictionary: compact `id → String` for **sorted** keys.
//!
//! The reverse map of a sorted index is dominated by the raw key bytes plus one 8-byte offset per
//! key. Sorted catalogs share long prefixes (`"entity-000123"`, `"entity-000124"`), so this stores
//! keys in fixed-size buckets: the first key of each bucket is verbatim and the rest are
//! `(shared-prefix length, suffix)` deltas against their predecessor. That collapses both the offset
//! table (one pointer per *bucket*, not per key) and the shared prefixes, at the cost of decoding up
//! to `BUCKET_SIZE - 1` deltas to reconstruct a key. Keys MUST be supplied in sorted order — this
//! backs [`crate::StringIndex`], whose ids are the sorted rank.

use crate::IndexError;

/// Keys per bucket. Larger ⇒ smaller pointer table and more prefix reuse, but up to `BUCKET_SIZE - 1`
/// delta decodes per random access. 8 is the usual front-coding sweet spot.
const BUCKET_SIZE: usize = 8;

/// A front-coded dictionary addressable by index (`id → key`), for sorted keys.
#[derive(Clone, Debug, Default)]
pub(crate) struct FrontCodedDict {
    data: Vec<u8>,
    bucket_ptrs: Vec<u64>, // byte offset in `data` where each bucket begins (len == number of buckets)
    n: usize,
}

impl FrontCodedDict {
    /// Build from strings in **sorted** index order (`items[i]` becomes key `i`).
    pub(crate) fn build<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut data = Vec::new();
        let mut bucket_ptrs = Vec::new();
        let mut prev: Vec<u8> = Vec::new();
        let mut n = 0usize;
        for s in items {
            let key = s.as_ref().as_bytes();
            if n.is_multiple_of(BUCKET_SIZE) {
                // Bucket head: stored verbatim so decoding a bucket needs no earlier bucket.
                bucket_ptrs.push(data.len() as u64);
                write_varint(&mut data, key.len() as u64);
                data.extend_from_slice(key);
            } else {
                // Delta against the previous key: shared-prefix length + the differing suffix.
                let lcp = common_prefix_len(&prev, key);
                write_varint(&mut data, lcp as u64);
                write_varint(&mut data, (key.len() - lcp) as u64);
                data.extend_from_slice(&key[lcp..]);
            }
            prev.clear();
            prev.extend_from_slice(key);
            n += 1;
        }
        Self {
            data,
            bucket_ptrs,
            n,
        }
    }

    /// Number of stored strings.
    pub(crate) fn len(&self) -> usize {
        self.n
    }

    /// The string at index `i`, or `None` if out of range (or, only for a corrupt blob, if the
    /// reconstructed bytes are not valid UTF-8).
    pub(crate) fn get(&self, i: usize) -> Option<String> {
        if i >= self.n {
            return None;
        }
        let within = i % BUCKET_SIZE;
        let mut pos = *self.bucket_ptrs.get(i / BUCKET_SIZE)? as usize;
        // Bucket head: [len][bytes].
        let head_len = read_varint(&self.data, &mut pos)? as usize;
        let mut key = self.data.get(pos..pos.checked_add(head_len)?)?.to_vec();
        pos += head_len;
        // Replay the deltas up to the target key within the bucket.
        for _ in 0..within {
            let lcp = read_varint(&self.data, &mut pos)? as usize;
            let slen = read_varint(&self.data, &mut pos)? as usize;
            let suffix = self.data.get(pos..pos.checked_add(slen)?)?;
            pos += slen;
            if lcp > key.len() {
                return None; // corrupt: prefix longer than the reconstructed predecessor
            }
            key.truncate(lcp);
            key.extend_from_slice(suffix);
        }
        String::from_utf8(key).ok()
    }

    /// Serialise to `[n: u64][num_buckets: u64][bucket_ptrs: num_buckets × u64][data]` (little-endian).
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let nb = self.bucket_ptrs.len();
        let mut out = Vec::with_capacity(16 + nb * 8 + self.data.len());
        out.extend_from_slice(&(self.n as u64).to_le_bytes());
        out.extend_from_slice(&(nb as u64).to_le_bytes());
        for &p in &self.bucket_ptrs {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }

    /// Parse the [`FrontCodedDict::to_bytes`] layout, validating every length and pointer (untrusted
    /// input): it can fail, but never reads out of bounds and never panics.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let n = read_u64(bytes, 0)? as usize;
        let nb = read_u64(bytes, 8)? as usize;
        if nb != n.div_ceil(BUCKET_SIZE) {
            return Err(IndexError::Format("front-coded: bucket count mismatch"));
        }
        let data_start = 16 + nb * 8;
        if bytes.len() < data_start {
            return Err(IndexError::Format("front-coded: truncated pointer table"));
        }
        let mut bucket_ptrs = Vec::with_capacity(nb);
        for i in 0..nb {
            bucket_ptrs.push(read_u64(bytes, 16 + i * 8)?);
        }
        let data = bytes[data_start..].to_vec();
        if bucket_ptrs.first().is_some_and(|&p| p != 0)
            || bucket_ptrs.windows(2).any(|w| w[1] < w[0])
            || bucket_ptrs.last().is_some_and(|&p| p as usize > data.len())
        {
            return Err(IndexError::Format("front-coded: bad bucket pointers"));
        }
        Ok(Self {
            data,
            bucket_ptrs,
            n,
        })
    }
}

/// Length of the shared leading bytes of `a` and `b`.
fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// LEB128 unsigned varint append.
fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// LEB128 unsigned varint read, advancing `pos`. `None` on end-of-buffer or an over-long encoding.
fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, IndexError> {
    let slice = bytes
        .get(at..at + 8)
        .ok_or(IndexError::Format("front-coded: unexpected end of buffer"))?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_all(keys: &[&str]) {
        let dict = FrontCodedDict::build(keys.iter().copied());
        assert_eq!(dict.len(), keys.len());
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(dict.get(i).as_deref(), Some(*k), "key {i}");
        }
        assert_eq!(dict.get(keys.len()), None); // out of range
    }

    #[test]
    fn reconstructs_across_bucket_boundaries() {
        // 20 keys > 2 buckets of 8; heavy shared prefixes exercise the delta path.
        let keys: Vec<String> = (0..20).map(|i| format!("entity-{i:05}")).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        roundtrip_all(&refs);
    }

    #[test]
    fn handles_edges_singletons_and_empty() {
        roundtrip_all(&[]);
        roundtrip_all(&["only"]);
        roundtrip_all(&["a", "ab", "abc"]); // increasing, lcp grows
        roundtrip_all(&["", "a"]); // empty first key
                                   // exactly one and one-more-than a full bucket
        let exactly: Vec<String> = (0..8).map(|i| format!("k{i}")).collect();
        let plus: Vec<String> = (0..9).map(|i| format!("k{i}")).collect();
        roundtrip_all(&exactly.iter().map(String::as_str).collect::<Vec<_>>());
        roundtrip_all(&plus.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn lcp_on_byte_boundary_keeps_utf8_valid() {
        // Shared prefix cuts through a multi-byte character; reconstruction must be byte-exact.
        roundtrip_all(&["café", "cafés", "cafétéria", "naïve", "naïveté"]);
    }

    #[test]
    fn serialises_and_reloads() {
        let keys: Vec<String> = (0..37).map(|i| format!("entity-{i:04}")).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let dict = FrontCodedDict::build(refs.iter().copied());
        let restored = FrontCodedDict::from_bytes(&dict.to_bytes()).unwrap();
        assert_eq!(restored.len(), dict.len());
        for (i, k) in refs.iter().enumerate() {
            assert_eq!(restored.get(i).as_deref(), Some(*k));
        }
    }

    #[test]
    fn empty_serialises_and_reloads() {
        let dict = FrontCodedDict::build(Vec::<&str>::new());
        let restored = FrontCodedDict::from_bytes(&dict.to_bytes()).unwrap();
        assert_eq!(restored.len(), 0);
        assert_eq!(restored.get(0), None);
    }

    #[test]
    fn rejects_corrupt_buffers() {
        assert!(FrontCodedDict::from_bytes(b"short").is_err()); // < 16-byte header
        let good = FrontCodedDict::build(["a", "b", "c"]).to_bytes();
        // Corrupt the declared key count so the bucket-count invariant breaks.
        let mut bad = good.clone();
        bad[0] = 0xff;
        assert!(FrontCodedDict::from_bytes(&bad).is_err());
    }

    #[test]
    fn is_smaller_than_a_flat_arena_on_sorted_keys() {
        // Sanity: on a structured sorted catalog the blob beats the raw bytes + 8-byte offsets.
        let n = 5_000;
        let keys: Vec<String> = (0..n).map(|i| format!("entity-{i:012}")).collect();
        let dict = FrontCodedDict::build(keys.iter().map(String::as_str));
        let flat = keys.iter().map(String::len).sum::<usize>() + (n + 1) * 8;
        assert!(
            dict.to_bytes().len() * 2 < flat,
            "front-coded {} vs flat {}",
            dict.to_bytes().len(),
            flat
        );
    }
}
