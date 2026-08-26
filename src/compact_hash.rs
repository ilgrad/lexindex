//! Fingerprint minimal-perfect-hash dictionary: the smallest `string → dense id` map.
//!
//! Like [`PerfectHashIndex`](crate::PerfectHashIndex) but it stores only a small **fingerprint** per
//! key instead of the key itself, so it costs a few bytes per key rather than tens. Two trade-offs:
//! membership is **probabilistic** — a non-member query that hashes to a member's slot *and* whose
//! fingerprint collides is a false positive, with probability `256^-fingerprint_bytes` (≈ 0.4% at 1
//! byte, ≈ 0.0015% at 2) — and there is **no reverse `id → key`** (the keys are not stored). Use it
//! for a fixed vocabulary where the tiniest footprint matters and rare false positives are acceptable;
//! reach for [`PerfectHashIndex`](crate::PerfectHashIndex) (exact membership + reverse) or
//! [`StringIndex`](crate::StringIndex) (ordered) otherwise.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::hash::{fingerprint, hash_key};
use epserde::prelude::*;
use ptr_hash::{DefaultPtrHash, PtrHash, PtrHashParams};

const MAGIC: &[u8; 4] = b"BCH1";
const HEADER_LEN: usize = 24; // [magic 4][n u64][fp_bytes u32][mph_len u64]

/// The smallest string→dense-id dictionary: a minimal perfect hash plus one small fingerprint per key.
pub struct CompactHashIndex {
    mph: Option<DefaultPtrHash>, // None iff empty
    fps: SharedBytes,            // n * fp_bytes fingerprints, in slot order
    fp_bytes: usize,             // 1, 2, or 4
    n: usize,
}

impl CompactHashIndex {
    /// Build from a collection of strings, storing `fingerprint_bytes` (1, 2, or 4) per key. Fewer
    /// bytes ⇒ smaller index but a higher false-positive rate on membership (`256^-fingerprint_bytes`).
    /// Duplicates are removed; ids are arbitrary dense slots in `[0, n)` (no defined order).
    pub fn build<I, S>(items: I, fingerprint_bytes: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !matches!(fingerprint_bytes, 1 | 2 | 4) {
            return Err(IndexError::Format(
                "compact-hash: fingerprint_bytes must be 1, 2, or 4",
            ));
        }
        // Sorted and deduplicated in place, comparing through `AsRef` rather than collecting owned
        // `String`s: the keys are copied once more into the structure below, so an intermediate
        // copy of the whole corpus bought nothing.
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        let n = keys.len();
        if n == 0 {
            return Ok(Self {
                mph: None,
                fps: SharedBytes::from_owned(Vec::new()),
                fp_bytes: fingerprint_bytes,
                n: 0,
            });
        }
        let hashes: Vec<u64> = keys.iter().map(|k| hash_key(k.as_ref())).collect();
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(IndexError::Format(
                "compact-hash: 64-bit key-hash collision; rebuild or use StringIndex",
            ));
        }
        let mph: DefaultPtrHash = PtrHash::new(&hashes, PtrHashParams::default());
        let mut fps = vec![0u8; n * fingerprint_bytes];
        let mut seen = vec![false; n];
        for (k, h) in keys.iter().zip(&hashes) {
            let slot = mph.index(h);
            if slot >= n || seen[slot] {
                return Err(IndexError::Format(
                    "compact-hash: construction was not minimal/perfect",
                ));
            }
            seen[slot] = true;
            let fp = fingerprint(k.as_ref(), fingerprint_bytes);
            fps[slot * fingerprint_bytes..(slot + 1) * fingerprint_bytes]
                .copy_from_slice(&fp.to_le_bytes()[..fingerprint_bytes]);
        }
        Ok(Self {
            mph: Some(mph),
            fps: SharedBytes::from_owned(fps),
            fp_bytes: fingerprint_bytes,
            n,
        })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the dictionary has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Dense id of `key`, or `None`. Membership is checked against the stored fingerprint, so a `Some`
    /// result is correct except for a `256^-fingerprint_bytes` false-positive chance on a non-member.
    pub fn id(&self, key: &str) -> Option<u32> {
        let mph = self.mph.as_ref()?;
        let slot = mph.index(&hash_key(key));
        if slot >= self.n {
            return None;
        }
        if read_fp(self.fps.as_ref(), slot, self.fp_bytes)? == fingerprint(key, self.fp_bytes) {
            Some(slot as u32)
        } else {
            None
        }
    }

    /// Dense id **without** checking the fingerprint — `key` must be a member, or the result is an
    /// arbitrary valid slot. The fastest lookup for a closed vocabulary. Returns `0` when empty.
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        match &self.mph {
            Some(mph) => mph.index(&hash_key(key)) as u32,
            None => 0,
        }
    }

    /// Whether `key` is present (subject to the fingerprint false-positive rate).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Serialise to `[magic 4][n u64][fp_bytes u32][mph_len u64][mph epserde bytes][fingerprints]`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let fp = self.fps.as_ref();
        let mut out = Vec::with_capacity(HEADER_LEN + mph_buf.len() + fp.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.n as u64).to_le_bytes());
        out.extend_from_slice(&(self.fp_bytes as u32).to_le_bytes());
        out.extend_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(fp);
        Ok(out)
    }

    /// Reconstruct from [`CompactHashIndex::to_bytes`] output (copies the blob into owned memory).
    ///
    /// The lexindex header and fingerprint table are fully bounds-validated, but the embedded minimal
    /// perfect hash is deserialised by [`epserde`]: feed only blobs produced by
    /// [`to_bytes`](Self::to_bytes) / [`save`](Self::save). A corrupted MPH region may abort on a failed
    /// allocation rather than returning a clean error — the same "trust your own blob" contract as
    /// [`load_mmap`](Self::load_mmap).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// Reconstruct from a shared source. The MPH is deserialised into memory; the fingerprint table
    /// (the bulk) is borrowed zero-copy — so `load_mmap` never copies it.
    fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let n = u64::from_le_bytes(bytes[4..12].try_into().unwrap()) as usize;
        let fp_bytes = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let mph_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        if !matches!(fp_bytes, 1 | 2 | 4) {
            return Err(IndexError::Format("compact-hash: bad fingerprint width"));
        }
        let mph_end = HEADER_LEN
            .checked_add(mph_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format("mph length out of range"))?;
        let mph = if n == 0 {
            None
        } else {
            let mut reader = &bytes[HEADER_LEN..mph_end];
            Some(
                DefaultPtrHash::deserialize_full(&mut reader)
                    .map_err(|e| IndexError::Serde(e.to_string()))?,
            )
        };
        let fps = blob
            .subslice(mph_end, blob.len())
            .ok_or(IndexError::Format("fingerprint range out of range"))?;
        // `n` is untrusted (read from the header), so guard the multiply — a fabricated huge `n` would
        // otherwise overflow `usize` and panic in a debug build instead of failing cleanly.
        if n.checked_mul(fp_bytes) != Some(fps.len()) {
            return Err(IndexError::Format(
                "compact-hash: fingerprint length mismatch",
            ));
        }
        Ok(Self {
            mph,
            fps,
            fp_bytes,
            n,
        })
    }

    /// Write the dictionary to `path`.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes()?)?;
        Ok(())
    }

    /// Load a dictionary previously written with [`CompactHashIndex::save`].
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Memory-map the file and borrow the fingerprint table zero-copy (only the small MPH is read into
    /// memory). See [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the immutability caveat.
    #[cfg(feature = "mmap")]
    pub fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the mapped file must not be mutated while it is mapped (see StringIndex::load_mmap).
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)))
    }
}

fn read_fp(bytes: &[u8], slot: usize, fp_bytes: usize) -> Option<u64> {
    let start = slot.checked_mul(fp_bytes)?;
    let slice = bytes.get(start..start.checked_add(fp_bytes)?)?;
    let mut buf = [0u8; 8];
    buf[..fp_bytes].copy_from_slice(slice);
    Some(u64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_lookup_and_membership() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma", "delta", "alpha"], 2).unwrap();
        assert_eq!(idx.len(), 4);
        assert!(!idx.is_empty());
        let mut ids = Vec::new();
        for w in ["alpha", "beta", "gamma", "delta"] {
            let id = idx.id(w).expect("present");
            assert!((id as usize) < idx.len());
            assert_eq!(idx.id_unchecked(w), id);
            assert!(idx.contains(w));
            ids.push(id);
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4); // dense bijection onto [0, n)
        assert_eq!(idx.id("epsilon"), None);
    }

    #[test]
    fn false_positive_rate_is_bounded() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = CompactHashIndex::build(&members, 2).unwrap();
        for m in &members {
            assert!(idx.contains(m)); // no false negatives, ever
        }
        let fp = (0..20_000)
            .filter(|i| idx.id(&format!("stranger-{i:06}")).is_some())
            .count();
        // 2-byte fingerprint ⇒ ~1/65536 per non-member; comfortably a handful over 20k probes.
        assert!(
            fp < 50,
            "false positives {fp}/20000 too high for a 2-byte fingerprint"
        );
    }

    #[test]
    fn much_smaller_than_perfect_hash_index() {
        let words: Vec<String> = (0..5_000).map(|i| format!("token-{i:05}")).collect();
        let compact = CompactHashIndex::build(&words, 1)
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        let exact = crate::PerfectHashIndex::build(&words)
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        assert!(compact * 3 < exact, "compact {compact} vs exact {exact}");
    }

    #[test]
    fn round_trips_and_rejects_corrupt() {
        let idx = CompactHashIndex::build(["GET", "POST", "PUT", "DELETE"], 2).unwrap();
        let restored = CompactHashIndex::from_bytes(&idx.to_bytes().unwrap()).unwrap();
        for w in ["GET", "POST", "PUT", "DELETE"] {
            assert_eq!(restored.id(w), idx.id(w));
        }
        assert_eq!(restored.id("PATCH"), None);
        assert!(CompactHashIndex::from_bytes(b"nope").is_err());
        assert!(CompactHashIndex::build(["a"], 3).is_err()); // bad fingerprint width
    }

    #[test]
    fn from_bytes_rejects_bad_width_and_length() {
        let good = CompactHashIndex::build(["a", "bb", "ccc"], 2)
            .unwrap()
            .to_bytes()
            .unwrap();

        // The fingerprint-width field (u32 at bytes 12..16) set to an unsupported value is rejected.
        let mut bad_width = good.clone();
        bad_width[12] = 3;
        assert!(matches!(
            CompactHashIndex::from_bytes(&bad_width),
            Err(IndexError::Format(_))
        ));

        // Dropping a fingerprint byte makes the table length != n * fp_bytes.
        assert!(matches!(
            CompactHashIndex::from_bytes(&good[..good.len() - 1]),
            Err(IndexError::Format(_))
        ));
    }

    #[test]
    fn from_bytes_rejects_overflowing_n_without_panicking() {
        // A fabricated huge `n` in the header (with the MPH region left intact) must fail cleanly, not
        // overflow `n * fp_bytes` — which would panic in a debug build.
        let mut blob = CompactHashIndex::build(["a", "bb", "ccc"], 4)
            .unwrap()
            .to_bytes()
            .unwrap();
        blob[11] ^= 0x40; // n: 3 -> 2^62, so `n * 4` would wrap u64
        assert!(matches!(
            CompactHashIndex::from_bytes(&blob),
            Err(IndexError::Format(_))
        ));
    }

    #[test]
    fn empty_round_trips() {
        let empty = CompactHashIndex::build(Vec::<String>::new(), 1).unwrap();
        assert!(empty.is_empty() && empty.id("x").is_none() && empty.id_unchecked("x") == 0);
        let restored = CompactHashIndex::from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = CompactHashIndex::build(["a", "b", "c"], 1).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_ch_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(CompactHashIndex::load(&path).unwrap().id("b"), idx.id("b"));
        std::fs::remove_file(&path).ok();
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_matches_owned() {
        let words: Vec<String> = (0..128).map(|i| format!("k{i:03}")).collect();
        let idx = CompactHashIndex::build(&words, 2).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_ch_mmap_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        let mapped = CompactHashIndex::load_mmap(&path).unwrap();
        assert_eq!(mapped.len(), idx.len());
        for w in &words {
            assert_eq!(mapped.id(w), idx.id(w));
        }
        assert!(!mapped.contains("k999"));
        std::fs::remove_file(&path).ok();
    }
}
