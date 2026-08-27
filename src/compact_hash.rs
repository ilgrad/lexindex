//! Fingerprint minimal-perfect-hash dictionary: the smallest `string → dense id` map.
//!
//! Like [`PerfectHashIndex`](crate::PerfectHashIndex) but it stores only a small **fingerprint** per
//! key instead of the key itself, so it costs a byte-ish per key rather than tens. Two trade-offs:
//! membership is **probabilistic** — a non-member query that hashes to a member's slot *and* whose
//! fingerprint collides is a false positive, with probability `2^-fingerprint_bits` (6.25% at 4
//! bits, ≈ 0.4% at 8, ≈ 0.0015% at 16) — and there is **no reverse `id → key`** (the keys are not
//! stored). Use it
//! for a fixed vocabulary where the tiniest footprint matters and rare false positives are acceptable;
//! reach for [`PerfectHashIndex`](crate::PerfectHashIndex) (exact membership + reverse) or
//! [`StringIndex`](crate::StringIndex) (ordered) otherwise.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::hash::{fingerprint_bits, hash_key};
use epserde::prelude::*;
use ptr_hash::DefaultPtrHash;

const MAGIC_V1: &[u8; 4] = b"BCH1"; // 0.5.x: width counts bytes, fingerprints byte-aligned
const MAGIC_V2: &[u8; 4] = b"BCH2"; // 0.6.0: width counts bits, fingerprints bit-packed
const MAGIC_V3: &[u8; 4] = b"BCH3"; // [magic 4][n u64][fp_bits u32][cap u64][mph_len u64][check u32]
const HEADER_LEN: usize = 36;
const CHECKED_LEN: usize = 32; // header bytes the trailing check covers

/// The smallest string→dense-id dictionary: a minimal perfect hash plus one small fingerprint per key.
pub struct CompactHashIndex {
    mph: Option<DefaultPtrHash>, // None iff empty
    fps: SharedBytes,            // n fingerprints of fp_bits each, bit-packed in slot order
    fp_bits: u32,                // 1..=64
    n: usize,
    // Length of the MPH's internal remap (see `crate::hash::overflow_cap`); `u64::MAX` for blobs
    // from versions that did not record it, meaning "unbounded" — the pre-0.7 behaviour.
    overflow_cap: u64,
}

impl CompactHashIndex {
    /// Build from a collection of strings, storing `fingerprint_bytes` (1, 2, or 4) per key —
    /// byte-granular sugar for [`build_bits`](Self::build_bits) with `8 × fingerprint_bytes`.
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
        Self::build_bits(items, fingerprint_bytes as u32 * 8)
    }

    /// Build storing exactly `fingerprint_bits` (1..=64) per key. Fewer bits ⇒ smaller index but a
    /// higher false-positive rate on membership: exactly `2^-fingerprint_bits` (6.25% at 4 bits,
    /// ≈ 0.4% at 8, ≈ 0.0015% at 16). Duplicates are removed; ids are arbitrary dense slots in
    /// `[0, n)` (no defined order).
    pub fn build_bits<I, S>(items: I, fingerprint_bits: u32) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=64).contains(&fingerprint_bits) {
            return Err(IndexError::Format(
                "compact-hash: fingerprint_bits must be in 1..=64",
            ));
        }
        // Sorted and deduplicated in place, comparing through `AsRef` rather than collecting owned
        // `String`s: the keys are copied once more into the structure below, so an intermediate
        // copy of the whole corpus bought nothing.
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        let n = keys.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "compact-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        if n == 0 {
            return Ok(Self {
                mph: None,
                fps: SharedBytes::from_owned(Vec::new()),
                fp_bits: fingerprint_bits,
                n: 0,
                overflow_cap: 0,
            });
        }
        let hashes: Vec<u64> = keys.iter().map(|k| hash_key(k.as_ref())).collect();
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(IndexError::Format(
                "compact-hash: two keys share a 64-bit hash; the deterministic hash means this key \
                 set can never build - use StringIndex or change the keys",
            ));
        }
        let mph = crate::hash::build_mph(&hashes)?;
        let table_len = (n as u64 * fingerprint_bits as u64).div_ceil(8) as usize;
        let mut fps = vec![0u8; table_len];
        let mut seen = vec![false; n];
        for (k, h) in keys.iter().zip(&hashes) {
            let slot = mph.index(h);
            if slot >= n || seen[slot] {
                return Err(IndexError::Format(
                    "compact-hash: construction was not minimal/perfect",
                ));
            }
            seen[slot] = true;
            write_fp(
                &mut fps,
                slot,
                fingerprint_bits,
                crate::hash::fingerprint_bits(k.as_ref(), fingerprint_bits),
            );
        }
        let overflow_cap = crate::hash::overflow_cap(&mph, &hashes, n);
        Ok(Self {
            mph: Some(mph),
            fps: SharedBytes::from_owned(fps),
            fp_bits: fingerprint_bits,
            n,
            overflow_cap,
        })
    }

    /// Slot for a key hash, or `None` when the raw slot is past the MPH's remap — a trailing free
    /// slot no member occupies, which ptr_hash's own `index()` would read out of bounds.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        crate::hash::slot_for(self.mph.as_ref()?, self.n, self.overflow_cap, h)
    }

    /// Width of the stored fingerprints in bits; the membership false-positive rate is
    /// `2^-fingerprint_bits`.
    pub fn fingerprint_bits(&self) -> u32 {
        self.fp_bits
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
    /// result is correct except for a `2^-fingerprint_bits` false-positive chance on a non-member.
    pub fn id(&self, key: &str) -> Option<u32> {
        let slot = self.slot_for(hash_key(key))?;
        if slot >= self.n {
            return None;
        }
        if read_fp(self.fps.as_ref(), slot, self.fp_bits)? == fingerprint_bits(key, self.fp_bits) {
            Some(slot as u32)
        } else {
            None
        }
    }

    /// Dense id **without** checking the fingerprint — `key` must be a member, or the result is an
    /// arbitrary valid slot. The fastest lookup for a closed vocabulary. Returns `0` when empty, and
    /// for a non-member whose slot falls past the MPH's remap (which is bounded rather than read
    /// unchecked — being unsafe on a wrong key is not one of the trade-offs this method makes).
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        self.slot_for(hash_key(key)).unwrap_or(0) as u32
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input (`None` where
    /// the fingerprint rejects). Slot resolution streams through the MPH with 32 queries' worth
    /// of software prefetch in flight, and the fingerprint lines are prefetched ahead of the
    /// compare. Measured on real words: 1.1× the per-key loop at 480 k keys, 1.2× at 5 M.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u32>> {
        let Some(mph) = &self.mph else {
            return vec![None; keys.len()];
        };
        // Both hashes are computed in one pass over the keys, so the verify pass below never
        // touches the strings again — it is a pure fingerprint-table compare with the lines
        // prefetched ahead.
        let mut hashes = Vec::with_capacity(keys.len());
        let mut wanted = Vec::with_capacity(keys.len());
        for k in keys {
            hashes.push(hash_key(k.as_ref()));
            wanted.push(fingerprint_bits(k.as_ref(), self.fp_bits));
        }
        // ptr_hash's stream iterator is internal-iteration only (`next()` is unimplemented by
        // design), so drain it with `for_each`.
        // MINIMAL=false: raw slots, so the stream never touches the remap (see `slot_for`). Raw
        // slots ≥ n are triaged here: past the cap they are provably non-members, otherwise the
        // (rare, ~1%) per-key `index()` resolves the remapped slot.
        let slots = crate::hash::triage_slots(mph, self.n, self.overflow_cap, &hashes);
        let fps = self.fps.as_ref();
        const AHEAD: usize = 32;
        (0..keys.len())
            .map(|i| {
                if let Some(&s) = slots.get(i + AHEAD) {
                    if s < self.n {
                        crate::blob::prefetch_byte(
                            fps,
                            (s as u64 * self.fp_bits as u64 / 8) as usize,
                        );
                    }
                }
                let slot = slots[i];
                if slot >= self.n {
                    return None;
                }
                (read_fp(fps, slot, self.fp_bits)? == wanted[i]).then_some(slot as u32)
            })
            .collect()
    }

    /// Whether `key` is present (subject to the fingerprint false-positive rate).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Serialise to `[magic "BCH3"][n u64][fp_bits u32][overflow_cap u64][mph_len u64][check u32]
    /// [mph epserde bytes][bit-packed fingerprints]`. `check` is a hash of the preceding header
    /// bytes: `overflow_cap` bounds an otherwise unchecked read inside the MPH, so it must not be
    /// taken on trust from a blob that lost bytes in transit.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let fp = self.fps.as_ref();
        let mut out = Vec::with_capacity(HEADER_LEN + mph_buf.len() + fp.len());
        out.extend_from_slice(MAGIC_V3);
        out.extend_from_slice(&(self.n as u64).to_le_bytes());
        out.extend_from_slice(&self.fp_bits.to_le_bytes());
        out.extend_from_slice(&self.overflow_cap.to_le_bytes());
        out.extend_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        let check = crate::hash::hash_bytes(&out[..CHECKED_LEN]) as u32;
        out.extend_from_slice(&check.to_le_bytes());
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
        // 0.5/0.6 blobs predate the recorded remap bound and, unlike the arena-backed index, store
        // no keys to recompute it from — so there is nothing to heal and loading one unbounded is
        // the very defect 0.7 fixes. Refuse with an actionable message instead.
        if bytes.len() >= 4 && (&bytes[0..4] == MAGIC_V1 || &bytes[0..4] == MAGIC_V2) {
            return Err(IndexError::Format(
                "compact-hash: this blob was written by lexindex < 0.7, whose lookups could read \
                 past the perfect hash's remap; the keys are not stored, so it cannot be repaired \
                 on load - rebuild the index with 0.7 or later",
            ));
        }
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC_V3 {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED_LEN..HEADER_LEN].try_into().unwrap());
        if check != crate::hash::hash_bytes(&bytes[..CHECKED_LEN]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let n64 = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        if n64 > u32::MAX as u64 {
            return Err(IndexError::Format(
                "compact-hash: header claims more than u32::MAX keys",
            ));
        }
        let n = n64 as usize;
        let fp_bits = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if !(1..=64).contains(&fp_bits) {
            return Err(IndexError::Format("compact-hash: bad fingerprint width"));
        }
        let overflow_cap = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let header = HEADER_LEN;
        let mph_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap()) as usize;
        let mph_end = header
            .checked_add(mph_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format("mph length out of range"))?;
        let mph = if n == 0 {
            None
        } else {
            let mut reader = &bytes[header..mph_end];
            Some(
                DefaultPtrHash::deserialize_full(&mut reader)
                    .map_err(|e| IndexError::Serde(e.to_string()))?,
            )
        };
        let fps = blob
            .subslice(mph_end, blob.len())
            .ok_or(IndexError::Format("fingerprint range out of range"))?;
        // `n` is untrusted (read from the header), so guard the multiply — a fabricated huge `n` would
        // otherwise overflow and panic in a debug build instead of failing cleanly.
        let expected = (n as u64)
            .checked_mul(fp_bits as u64)
            .map(|bits| bits.div_ceil(8))
            .ok_or(IndexError::Format(
                "compact-hash: fingerprint length mismatch",
            ))?;
        if expected != fps.len() as u64 {
            return Err(IndexError::Format(
                "compact-hash: fingerprint length mismatch",
            ));
        }
        if let Some(mph) = &mph {
            if mph.n() != n {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
        }
        Ok(Self {
            mph,
            fps,
            fp_bits,
            n,
            overflow_cap,
        })
    }

    /// Write the dictionary to `path`.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically(path.as_ref(), &self.to_bytes()?)
    }

    /// Load a dictionary previously written with [`CompactHashIndex::save`].
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?))
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

#[inline(always)]
fn fp_mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Fingerprint of `slot` from the bit-packed table, or `None` if the table is too short.
#[inline(always)]
fn read_fp(bytes: &[u8], slot: usize, bits: u32) -> Option<u64> {
    let bitpos = (slot as u64).checked_mul(bits as u64)?;
    let byte = (bitpos / 8) as usize;
    let off = (bitpos % 8) as u32;
    if let Some(chunk) = bytes.get(byte..byte + 8) {
        let w = u64::from_le_bytes(chunk.try_into().unwrap());
        let v = if off + bits <= 64 {
            w >> off
        } else {
            // off ≥ 1 here (bits ≤ 64), so the shift below is < 64
            (w >> off) | ((*bytes.get(byte + 8)? as u64) << (64 - off))
        };
        Some(v & fp_mask(bits))
    } else {
        // Within 8 bytes of the table's end (≤ 7 bytes available from `byte`): accumulate the
        // covering bytes without reading past the end. Only the last few slots ever land here.
        let last = ((bitpos + bits as u64 - 1) / 8) as usize;
        let mut v: u64 = 0;
        for (j, i) in (byte..=last).enumerate() {
            v |= (*bytes.get(i)? as u64) << (8 * j as u32);
        }
        Some((v >> off) & fp_mask(bits))
    }
}

/// Write fingerprint `fp` (already masked to `bits`) for `slot` into the zeroed bit-packed table.
#[inline]
fn write_fp(fps: &mut [u8], slot: usize, bits: u32, fp: u64) {
    if bits % 8 == 0 {
        // Byte-aligned widths (including the 8-bit default) take a straight copy: the generic
        // OR-in loop below costs a measurable ~2.5% of build time at 1 M keys.
        let k = (bits / 8) as usize;
        let start = slot * k;
        fps[start..start + k].copy_from_slice(&fp.to_le_bytes()[..k]);
        return;
    }
    let bitpos = slot as u64 * bits as u64;
    let byte = (bitpos / 8) as usize;
    let off = (bitpos % 8) as u32;
    // Bytes of `fp << off` past the fingerprint's own span are zero, so skipping the ones that
    // fall past the table's end drops nothing.
    for (j, &b) in (fp << off).to_le_bytes().iter().enumerate() {
        if let Some(dst) = fps.get_mut(byte + j) {
            *dst |= b;
        }
    }
    if off > 0 && off + bits > 64 {
        fps[byte + 8] |= (fp >> (64 - off)) as u8;
    }
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

    /// Batch equals singular on every probe, members and misses alike; a fingerprint false
    /// positive would show up as a batch/singular disagreement, not just a wrong answer.
    #[test]
    fn batch_matches_singular_including_misses() {
        let keys: Vec<String> = (0..3_000).map(|i| format!("k{i}")).collect();
        let idx = CompactHashIndex::build(&keys, 2).unwrap();
        let probes: Vec<String> = keys
            .iter()
            .cloned()
            .chain((0..500).map(|i| format!("miss{i}")))
            .collect();
        assert_eq!(
            idx.ids_of(&probes),
            probes.iter().map(|p| idx.id(p)).collect::<Vec<_>>()
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

        // The width field (u32 at bytes 12..16) outside 1..=64 bits is rejected...
        for w in [0u8, 65] {
            let mut bad_width = good.clone();
            bad_width[12] = w;
            assert!(matches!(
                CompactHashIndex::from_bytes(&bad_width),
                Err(IndexError::Format(_))
            ));
        }
        // ...and a BCH1 blob may only claim 1, 2 or 4 *bytes*.
        let mut bad_v1 = good.clone();
        bad_v1[0..4].copy_from_slice(b"BCH1");
        bad_v1[12] = 3;
        assert!(matches!(
            CompactHashIndex::from_bytes(&bad_v1),
            Err(IndexError::Format(_))
        ));

        // Dropping a byte makes the table length disagree with ceil(n * fp_bits / 8).
        assert!(matches!(
            CompactHashIndex::from_bytes(&good[..good.len() - 1]),
            Err(IndexError::Format(_))
        ));
    }

    /// 0.5/0.6 blobs predate the recorded remap bound and store no keys to recompute it from, so
    /// they are refused with a message that names the fix rather than loaded unbounded.
    #[test]
    fn a_pre_0_7_blob_is_refused_with_a_rebuild_message() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let v3 = idx.to_bytes().unwrap();
        assert_eq!(&v3[0..4], b"BCH3");
        for (magic, width) in [(b"BCH1", 1u32), (b"BCH2", 8)] {
            let mut old = v3.clone();
            old.drain(CHECKED_LEN..HEADER_LEN); // no header check
            old.drain(16..24); // no overflow_cap
            old[0..4].copy_from_slice(magic);
            old[12..16].copy_from_slice(&width.to_le_bytes());
            let err = match CompactHashIndex::from_bytes(&old) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("a pre-0.7 blob was accepted"),
            };
            assert!(err.contains("rebuild the index with 0.7"), "{err}");
        }
    }

    /// `overflow_cap` bounds a read that ptr_hash performs unchecked, so a header that lost bytes
    /// in transit must be refused rather than used to steer queries.
    #[test]
    fn a_corrupt_header_is_refused() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let good = idx.to_bytes().unwrap();
        assert!(CompactHashIndex::from_bytes(&good).is_ok());
        for pos in [4, 12, 16, 23, 24, 31, 32, 35] {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(
                CompactHashIndex::from_bytes(&bad).is_err(),
                "header byte {pos} was accepted"
            );
        }
    }

    /// `id_unchecked` skips the fingerprint comparison, not the remap bound.
    #[test]
    fn id_unchecked_is_bounded_for_strangers() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        for round in 0..60 {
            let idx = CompactHashIndex::build(&members, 1).unwrap();
            for probe in 0..2_000 {
                let s = format!("stranger-{round}-{probe}");
                assert!((idx.id_unchecked(&s) as usize) < idx.len());
            }
        }
    }

    /// The regression test for the unchecked-remap window: ptr_hash's `index()` reads its remap
    /// out of bounds for a non-member whose raw slot lands past the last member-occupied one
    /// (debug assertion / release UB). Rebuild many times (each build rolls new eviction
    /// entropy), find a stranger in that zone via the raw slot, and require `id()` to answer
    /// `None` instead of touching the remap. Requires the zone to occur at least once across the
    /// rebuilds — if this ever fails with "no trailing free zone", raise `BUILDS` rather than
    /// letting the test pass vacuously.
    #[test]
    fn strangers_past_the_remap_are_rejected_not_ub() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let mut engaged = 0u32;
        for round in 0..300 {
            let idx = CompactHashIndex::build_bits(&members, 8).unwrap();
            let mph = idx.mph.as_ref().unwrap();
            for probe in 0..5_000 {
                let s = format!("stranger-{round}-{probe}");
                let raw = mph.index_no_remap(&crate::hash::hash_key(&s));
                if raw >= idx.n && (raw - idx.n) as u64 >= idx.overflow_cap {
                    assert_eq!(idx.id(&s), None);
                    assert_eq!(idx.ids_of(&[&s]), vec![None]);
                    engaged += 1;
                }
            }
            if engaged >= 20 {
                break;
            }
        }
        assert!(
            engaged > 0,
            "no trailing free zone in 300 builds - raise BUILDS"
        );
    }

    /// Every width round-trips through build, serde and the batch path; the fingerprints for the
    /// last slots sit within 8 bytes of the table's end, covering the tail read path.
    #[test]
    fn sub_byte_and_odd_widths_round_trip() {
        let keys: Vec<String> = (0..300).map(|i| format!("key-{i:03}")).collect();
        for bits in [1u32, 3, 4, 6, 8, 12, 33, 64] {
            let idx = CompactHashIndex::build_bits(&keys, bits).unwrap();
            assert_eq!(idx.fingerprint_bits(), bits);
            let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k).expect("member")).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), keys.len(), "bits={bits}: ids not dense");
            let restored = CompactHashIndex::from_bytes(&idx.to_bytes().unwrap()).unwrap();
            assert_eq!(restored.fingerprint_bits(), bits);
            let probes: Vec<String> = keys
                .iter()
                .cloned()
                .chain((0..100).map(|i| format!("miss-{i}")))
                .collect();
            let singular: Vec<Option<u32>> = probes.iter().map(|p| idx.id(p)).collect();
            assert_eq!(restored.ids_of(&probes), singular, "bits={bits}");
            assert_eq!(idx.ids_of(&probes), singular, "bits={bits}");
        }
        assert!(CompactHashIndex::build_bits(&keys, 0).is_err());
        assert!(CompactHashIndex::build_bits(&keys, 65).is_err());
    }

    /// The packed table read back equals the reference: every fingerprint extracted at every
    /// width, against a naive bit-by-bit reader.
    #[test]
    fn packed_table_matches_naive_reference() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(
                &(1u32..=64, prop::collection::vec(any::<u64>(), 1..50)),
                |(bits, raw)| {
                    let masked: Vec<u64> = raw.iter().map(|f| f & fp_mask(bits)).collect();
                    let n = masked.len();
                    let mut table = vec![0u8; (n as u64 * bits as u64).div_ceil(8) as usize];
                    for (i, &f) in masked.iter().enumerate() {
                        write_fp(&mut table, i, bits, f);
                    }
                    for (i, &f) in masked.iter().enumerate() {
                        prop_assert_eq!(
                            read_fp(&table, i, bits),
                            Some(f),
                            "slot {} bits {}",
                            i,
                            bits
                        );
                        let mut naive: u64 = 0;
                        for b in 0..bits as u64 {
                            let pos = i as u64 * bits as u64 + b;
                            let bit = (table[(pos / 8) as usize] >> (pos % 8)) & 1;
                            naive |= (bit as u64) << b;
                        }
                        prop_assert_eq!(naive, f);
                    }
                    Ok(())
                },
            )
            .unwrap();
    }

    /// At 4 bits the advertised false-positive rate is 2^-4 = 6.25%; check it statistically
    /// (20 000 probes ⇒ expect 1 250, σ ≈ 34; the bound below is ≈ +7σ, far outside noise).
    #[test]
    fn four_bit_false_positive_rate_is_bounded() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = CompactHashIndex::build_bits(&members, 4).unwrap();
        for m in &members {
            assert!(idx.contains(m));
        }
        let fp = (0..20_000)
            .filter(|i| idx.id(&format!("stranger-{i:06}")).is_some())
            .count();
        assert!(
            fp < 1_500,
            "false positives {fp}/20000 too high for a 4-bit fingerprint (expect ~1250)"
        );
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
        let empty = CompactHashIndex::build_bits(Vec::<String>::new(), 4).unwrap();
        assert_eq!(empty.fingerprint_bits(), 4);
        assert!(empty.is_empty() && empty.id("x").is_none());
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
