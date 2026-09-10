//! Minimal perfect hash and nothing else: `string → dense id` for a vocabulary known to be closed.
//!
//! [`CompactHashIndex`](crate::CompactHashIndex) stores a fingerprint per key so that it can say
//! *no*; this index stores nothing per key and never says it. `id` returns a `u32` rather than an
//! `Option`: a member's id, and for any other string some id in `[0, n)` — which is all a perfect
//! hash can answer, and the whole contract. The size is the perfect hash alone, 2.09 bits per key
//! on real words, a fifth of the smallest fingerprinted index and a tenth of any trie.
//!
//! It is a separate type rather than `CompactHashIndex` at zero fingerprint bits because a
//! membership check that is always `Some` would be a signature that lies. Reach for it as a
//! token → id map when every query is a member by construction — a tokenizer over its own
//! vocabulary, a column of a table joined on a key it was built from, a hash-partitioned pipeline
//! — and for [`CompactHashIndex`](crate::CompactHashIndex) the moment a stranger can ask.

use crate::IndexError;
use crate::hash::{hash_key, hash_pair};
use crate::mphf::Mphf;

/// `[magic 4][n u64][mph_len u64][side_len u32][payload u64][check u32]`
const MAGIC: &[u8; 4] = b"BCL1";
const HEADER: usize = 36;
const CHECKED: usize = 32; // header bytes the trailing check covers
const SIDE_ENTRY: usize = 20; // hash u64 + second hash u64 + id u32

/// The validated framing of a blob — every field a query will trust — with the MPH region located
/// but not parsed.
struct Frame {
    n: usize,
    mph: std::ops::Range<usize>, // the MPH region; ignored when `n == 0`
    side: Vec<(u64, u64, u32)>,
}

/// `string → dense id` over a closed vocabulary: the minimal perfect hash and nothing else.
pub struct ClosedHashIndex {
    mph: Option<Mphf>, // over one hash per distinct hash value; None iff empty
    n: usize,
    // (hash, full 64-bit second hash, id) for every key whose 64-bit hash collides with another
    // key's, sorted; almost always empty. Keys here have tail ids [m, n) and no slot in the hash.
    side: Vec<(u64, u64, u32)>,
}

impl ClosedHashIndex {
    /// Build from a collection of strings. Duplicates are removed; ids are arbitrary dense slots.
    /// The build streams: it keeps 16 hashed bytes per key, never the strings.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let pairs = items
            .into_iter()
            .map(|s| hash_pair(s.as_ref()))
            .collect::<Vec<_>>();
        Self::build_from_pairs(pairs)
    }

    /// [`build`](Self::build) over hashed keys — the streaming entry the Python constructor uses.
    pub(crate) fn build_from_pairs(mut pairs: Vec<(u64, u64)>) -> Result<Self, IndexError> {
        pairs.sort_unstable();
        // Duplicate keys produce identical pairs; distinct keys deduplicate here only by colliding
        // in both 64-bit hashes at once.
        pairs.dedup();
        let n = pairs.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "closed-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        if n == 0 {
            return Ok(Self {
                mph: None,
                n: 0,
                side: Vec::new(),
            });
        }
        // One representative per distinct hash value builds the MPH and owns the slot; the (almost
        // always zero) same-hash leftovers get tail ids [m, n) in the side table.
        let mut hashes = Vec::with_capacity(n);
        let mut side: Vec<(u64, u64, u32)> = Vec::new();
        for run in pairs.chunk_by(|a, b| a.0 == b.0) {
            hashes.push(run[0].0);
            for &(h, second) in &run[1..] {
                side.push((h, second, 0)); // ids assigned once m is known
            }
        }
        let m = hashes.len();
        for (j, e) in side.iter_mut().enumerate() {
            e.2 = (m + j) as u32;
        }
        drop(pairs);
        let mph = Mphf::build(&hashes)?;
        // One bit per slot: this only has to catch a construction that was not minimal/perfect.
        let mut seen = vec![0u64; m.div_ceil(64)];
        for &h in &hashes {
            let slot = mph.index(h) as usize;
            if slot >= m || seen[slot / 64] >> (slot % 64) & 1 == 1 {
                return Err(IndexError::Format(
                    "closed-hash: construction was not minimal/perfect",
                ));
            }
            seen[slot / 64] |= 1 << (slot % 64);
        }
        Ok(Self {
            mph: Some(mph),
            n,
            side,
        })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the index holds no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Dense id of `key` if it is a member; **some** id in `[0, n)` otherwise, and `0` for an
    /// empty index. Nothing stored can tell the two apart, so nothing tries: this is the
    /// [`CompactHashIndex::id_unchecked`](crate::CompactHashIndex::id_unchecked) contract as the
    /// only method, for a caller who knows every query is a member. In the rare index that holds a
    /// 64-bit hash collision, keys sharing the collided hash resolve through the side table,
    /// matched on the full second hash — exact for members even there; every other index skips
    /// that with one predictable branch.
    #[inline]
    pub fn id(&self, key: &str) -> u32 {
        if self.side.is_empty() {
            return self.slot_for(hash_key(key));
        }
        self.id_with_side(key)
    }

    /// Slot for a key hash; the MPH's remap covers every slot it can produce, so the answer is
    /// always below `m`. `0` for an empty index, which has no table to ask.
    #[inline]
    fn slot_for(&self, h: u64) -> u32 {
        self.mph.as_ref().map_or(0, |mph| mph.index(h) as u32)
    }

    /// [`id`](Self::id) for an index that contains at least one hash collision: the side probe,
    /// exact on the full second hash, then the ordinary slot. Entries under one hash carry
    /// pairwise-distinct second hashes (the build deduplicates on the pair), so the match is
    /// unambiguous.
    #[cold]
    fn id_with_side(&self, key: &str) -> u32 {
        let (h, second) = hash_pair(key);
        let start = self.side.partition_point(|e| e.0 < h);
        if let Some(id) = self.side[start..]
            .iter()
            .take_while(|e| e.0 == h)
            .find_map(|e| (e.1 == second).then_some(e.2))
        {
            return id;
        }
        self.slot_for(h)
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input. The key bytes
    /// are prefetched ahead of the hashing and the perfect hash's own stream keeps 32 queries in
    /// flight, so what a one-at-a-time loop serialises, this overlaps. The rare index holding a
    /// hash collision takes the per-key path instead.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<u32> {
        let Some(mph) = &self.mph else {
            return vec![0; keys.len()];
        };
        if !self.side.is_empty() {
            return keys.iter().map(|k| self.id_with_side(k.as_ref())).collect();
        }
        // Far enough ahead that a DRAM miss has time to land, near enough that the line is still
        // there: the slice holds the `String` headers contiguously, but their bytes are wherever
        // the allocator put them.
        const AHEAD: usize = 32;
        let mut hashes = Vec::with_capacity(keys.len());
        for (i, k) in keys.iter().enumerate() {
            if let Some(next) = keys.get(i + AHEAD) {
                crate::blob::prefetch_byte(next.as_ref().as_bytes(), 0);
            }
            hashes.push(hash_key(k.as_ref()));
        }
        mph.index_all(&hashes)
            .into_iter()
            .map(|slot| slot as u32)
            .collect()
    }

    /// Serialised header + owned sections, shared by [`to_bytes`](Self::to_bytes) and the
    /// streaming [`save`](Self::save) so the two emit byte-identical blobs.
    fn serialised_parts(&self) -> ([u8; HEADER], Vec<u8>, Vec<u8>) {
        let mph_buf = match &self.mph {
            Some(mph) => mph.to_bytes(),
            None => Vec::new(),
        };
        let mut side_buf = Vec::with_capacity(self.side.len() * SIDE_ENTRY);
        for &(h, second, id) in &self.side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&second.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut payload = crate::blob::BlockHasher::new();
        payload.update(&mph_buf);
        payload.update(&side_buf);
        let mut header = [0u8; HEADER];
        header[0..4].copy_from_slice(MAGIC);
        header[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        header[12..20].copy_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        header[20..24].copy_from_slice(&(self.side.len() as u32).to_le_bytes());
        header[24..32].copy_from_slice(&payload.finish().to_le_bytes());
        let check = crate::blob::hash_bytes(&header[..CHECKED]) as u32;
        header[CHECKED..].copy_from_slice(&check.to_le_bytes());
        (header, mph_buf, side_buf)
    }

    /// Serialise to `[magic "BCL1"][n u64][mph_len u64][side_len u32][payload u64][check u32]
    /// [MPH blob][side entries]`. `check` is a hash of the preceding header bytes and `payload` a
    /// streaming hash of everything after it, verified on load; the MPH region carries its own
    /// header and validates its own lengths, which is what makes [`from_bytes`](Self::from_bytes)
    /// a safe fn even though this index stores nothing to check an answer against.
    pub fn to_bytes(&self) -> Vec<u8> {
        let (header, mph_buf, side_buf) = self.serialised_parts();
        let mut out = Vec::with_capacity(HEADER + mph_buf.len() + side_buf.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(&side_buf);
        out
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it — for sizing
    /// a buffer or reporting bytes/key; [`save`](Self::save) writes exactly this many.
    pub fn serialized_len(&self) -> usize {
        let mph = match &self.mph {
            Some(mph) => mph.byte_len(),
            None => 0,
        };
        HEADER + mph + self.side.len() * SIDE_ENTRY
    }

    /// Reconstruct from [`ClosedHashIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes. Every array the index will read is bounded by a length this crate
    /// wrote and checks here — the header, the side ids and the MPH's own header alike — so a
    /// crafted blob is at worst *wrong*, never unsound. This index stores no keys, so "wrong" means
    /// it answers with ids for a table it did not build; the payload checksum is what turns
    /// accidental corruption into a clean error rather than a wrong answer.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let frame = Self::parse_frame(bytes)?;
        let m = frame.n - frame.side.len();
        let mph = if frame.n == 0 {
            None
        } else {
            let mph = Mphf::from_bytes(&bytes[frame.mph])?;
            if mph.n() != m as u64 {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        Ok(Self {
            mph,
            n: frame.n,
            side: frame.side,
        })
    }

    /// Whether the framing of `bytes` parses. Exists for the libFuzzer target in `fuzz/`, which
    /// lives in its own crate and so cannot reach `parse_frame`. See the `lexindex::fuzzing`
    /// module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_parse_frame(bytes: &[u8]) -> bool {
        Self::parse_frame(bytes).is_ok()
    }

    /// The lexindex framing of `bytes`, parsed and bounds-validated — magic, both checksums,
    /// lengths and the side table — with the MPH region located but **not** deserialised. Safe on
    /// arbitrary bytes: this is the half a property test fuzzes, and everything
    /// [`from_bytes`](Self::from_bytes) trusts comes out of here.
    fn parse_frame(bytes: &[u8]) -> Result<Frame, IndexError> {
        if bytes.len() < HEADER || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED..HEADER].try_into().unwrap());
        if check != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let stored = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        if stored != crate::blob::hash_block(&bytes[HEADER..]) {
            return Err(IndexError::Format("payload checksum mismatch"));
        }
        let n64 = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        if n64 > u32::MAX as u64 {
            return Err(IndexError::Format(
                "closed-hash: header claims more than u32::MAX keys",
            ));
        }
        let n = n64 as usize;
        // `mph_len` and the side-byte count are header-supplied; convert and multiply checked so a
        // fabricated length fails cleanly on every target width instead of truncating or wrapping
        // on a 32-bit one.
        let mph_len = usize::try_from(u64::from_le_bytes(bytes[12..20].try_into().unwrap()))
            .map_err(|_| IndexError::Format("mph length out of range"))?;
        let side_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
        if side_len > n || (side_len == n && n > 0) {
            return Err(IndexError::Format("side table length out of range"));
        }
        let m = n - side_len;
        let side_bytes = side_len
            .checked_mul(SIDE_ENTRY)
            .ok_or(IndexError::Format("side table length out of range"))?;
        let side_start = bytes
            .len()
            .checked_sub(side_bytes)
            .ok_or(IndexError::Format("side table length out of range"))?;
        // Nothing lies between the MPH region and the side table, so the two lengths must meet.
        if HEADER.checked_add(mph_len) != Some(side_start) {
            return Err(IndexError::Format("mph length out of range"));
        }
        let mut side: Vec<(u64, u64, u32)> = bytes[side_start..]
            .chunks_exact(SIDE_ENTRY)
            .map(|e| {
                (
                    u64::from_le_bytes(e[0..8].try_into().unwrap()),
                    u64::from_le_bytes(e[8..16].try_into().unwrap()),
                    u32::from_le_bytes(e[16..20].try_into().unwrap()),
                )
            })
            .collect();
        side.sort_unstable(); // restore the binary-search invariant regardless of the blob
        // Side ids must be exactly the tail range [m, n): `id()` hands them out verbatim, so an
        // unvalidated blob could otherwise answer with an id at or past `len()`. Checked
        // structurally, not via the checksums — those only vouch for transport, not construction.
        let mut ids: Vec<u32> = side.iter().map(|e| e.2).collect();
        ids.sort_unstable();
        if !ids.iter().copied().eq(m as u32..n as u32) {
            return Err(IndexError::Format(
                "closed-hash: side-table ids are not the tail id range",
            ));
        }
        Ok(Frame {
            n,
            mph: HEADER..side_start,
            side,
        })
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts();
        w.write_all(&header)?;
        w.write_all(&mph_buf)?;
        w.write_all(&side_buf)?;
        Ok(())
    }

    /// Load an index previously written with [`ClosedHashIndex::save`]. Safe on any file — see
    /// [`from_bytes`](Self::from_bytes). There is no `load_mmap`: the whole blob is the perfect
    /// hash, which is read into memory whichever way it is loaded.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The safe loader must never panic on arbitrary bytes — only `Ok`/`Err`.
    #[test]
    fn parse_frame_never_panics() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(&prop::collection::vec(any::<u8>(), 0..256), |data| {
                let _ = ClosedHashIndex::parse_frame(&data);
                Ok(())
            })
            .unwrap();
    }

    /// Every truncation of a real blob is rejected by the framing alone, so the MPH region is
    /// never reached on a short read.
    #[test]
    fn parse_frame_rejects_every_truncation() {
        let blob = ClosedHashIndex::build(["alpha", "beta", "gamma"])
            .unwrap()
            .to_bytes();
        for k in 0..blob.len() {
            assert!(
                ClosedHashIndex::parse_frame(&blob[..k]).is_err(),
                "truncated to {k} bytes parsed"
            );
        }
        assert!(ClosedHashIndex::parse_frame(&blob).is_ok());
    }

    #[test]
    fn serialized_len_matches_to_bytes() {
        for keys in [vec![], vec!["alpha"], vec!["alpha", "beta", "gamma"]] {
            let idx = ClosedHashIndex::build(&keys).unwrap();
            assert_eq!(idx.serialized_len(), idx.to_bytes().len());
        }
    }

    /// Members get a bijection onto `[0, n)`; duplicates are one key.
    #[test]
    fn members_get_distinct_dense_ids() {
        let idx = ClosedHashIndex::build(["alpha", "beta", "gamma", "delta", "alpha"]).unwrap();
        assert_eq!(idx.len(), 4);
        assert!(!idx.is_empty());
        let mut ids: Vec<u32> = ["alpha", "beta", "gamma", "delta"]
            .iter()
            .map(|w| idx.id(w))
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, [0, 1, 2, 3]);
    }

    /// The contract on a stranger is only a bound: whatever it answers is a real id.
    #[test]
    fn strangers_get_some_id_below_n() {
        let words: Vec<String> = (0..10_000).map(|i| format!("word-{i:05}")).collect();
        let idx = ClosedHashIndex::build(&words).unwrap();
        for i in 0..10_000 {
            let stranger = format!("stranger-{i}");
            assert!((idx.id(&stranger) as usize) < idx.len(), "{stranger}");
        }
        assert!(
            idx.ids_of(&["nope", "never"])
                .iter()
                .all(|&id| (id as usize) < idx.len())
        );
    }

    #[test]
    fn batch_matches_singular() {
        let words: Vec<String> = (0..5_000).map(|i| format!("w{i}")).collect();
        let idx = ClosedHashIndex::build(&words).unwrap();
        let probes: Vec<&str> = words.iter().map(String::as_str).rev().collect();
        let batch = idx.ids_of(&probes);
        assert_eq!(batch.len(), probes.len());
        for (p, &id) in probes.iter().zip(&batch) {
            assert_eq!(id, idx.id(p), "{p}");
        }
        assert!(idx.ids_of(&Vec::<&str>::new()).is_empty());
    }

    /// A real 64-bit hash collision (the pinned pair from `crate::hash`) must build and resolve
    /// both keys through the side table, in every lookup form and across a round trip.
    #[test]
    fn colliding_keys_build_and_resolve_by_the_second_hash() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys: Vec<String> = (0..500).map(|i| format!("filler-{i:03}")).collect();
        keys.push(a.to_string());
        keys.push(b.to_string());
        let idx = ClosedHashIndex::build(&keys).unwrap();
        assert_eq!(idx.len(), 502);
        assert_eq!(idx.side.len(), 1);
        let (ia, ib) = (idx.id(a), idx.id(b));
        assert_ne!(ia, ib);
        assert_eq!(idx.ids_of(&[a, b]), vec![ia, ib]);
        let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k)).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            keys.len(),
            "ids must stay a bijection onto [0, n)"
        );
        let restored = ClosedHashIndex::from_bytes(&idx.to_bytes()).unwrap();
        assert_eq!(restored.side, idx.side);
        assert_eq!((restored.id(a), restored.id(b)), (ia, ib));
        assert_eq!(restored.ids_of(&keys), idx.ids_of(&keys));
    }

    #[test]
    fn round_trips_and_rejects_corrupt() {
        let idx = ClosedHashIndex::build(["alpha", "beta", "gamma"]).unwrap();
        let good = idx.to_bytes();
        assert_eq!(&good[0..4], b"BCL1");
        let back = ClosedHashIndex::from_bytes(&good).unwrap();
        assert_eq!(back.len(), 3);
        for w in ["alpha", "beta", "gamma"] {
            assert_eq!(back.id(w), idx.id(w));
        }
        assert!(ClosedHashIndex::from_bytes(b"nope").is_err());
        for pos in [4, 11, 12, 19, 20, 23, 24, 31, 32, 35] {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(
                ClosedHashIndex::from_bytes(&bad).is_err(),
                "header byte {pos} was accepted"
            );
        }
        for pos in (HEADER..good.len()).step_by(5) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(
                ClosedHashIndex::from_bytes(&bad).is_err(),
                "payload byte {pos} was accepted"
            );
        }
    }

    /// A fabricated huge `n` with the MPH region left intact must fail cleanly, not overflow.
    #[test]
    fn from_bytes_rejects_overflowing_n_without_panicking() {
        let mut blob = ClosedHashIndex::build(["a", "bb", "ccc"])
            .unwrap()
            .to_bytes();
        blob[11] ^= 0x40; // n: 3 -> 2^62
        assert!(matches!(
            ClosedHashIndex::from_bytes(&blob),
            Err(IndexError::Format(_))
        ));
    }

    /// The side ids are checked structurally, not through the checksums: a blob whose checksums
    /// were recomputed over a bad id is still refused, because `id()` would hand it out verbatim.
    #[test]
    fn tampered_side_ids_are_refused_even_with_valid_checksums() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = ClosedHashIndex::build([a, b, "filler"]).unwrap();
        assert_eq!(idx.side.len(), 1);
        let good = idx.to_bytes();
        for bad_id in [0u32, 1, 3, u32::MAX] {
            let mut bad = good.clone();
            let at = bad.len() - 4;
            bad[at..].copy_from_slice(&bad_id.to_le_bytes());
            let payload = crate::blob::hash_block(&bad[HEADER..]);
            bad[24..32].copy_from_slice(&payload.to_le_bytes());
            let check = crate::blob::hash_bytes(&bad[..CHECKED]) as u32;
            bad[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
            let err = match ClosedHashIndex::from_bytes(&bad) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("side id {bad_id} was accepted"),
            };
            assert!(err.contains("side-table ids"), "{err}");
        }
    }

    /// `[0, 0)` has no inhabitant, so the MPH has no table and `Mphf::index` would panic on one.
    /// Every query path has to notice that before it asks.
    #[test]
    fn empty_round_trips() {
        let empty = ClosedHashIndex::build(Vec::<String>::new()).unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.id("x"), 0);
        assert_eq!(empty.ids_of(&["x", "y"]), vec![0, 0]);
        let restored = ClosedHashIndex::from_bytes(&empty.to_bytes()).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.ids_of(&["x"]), vec![0]);
    }

    #[test]
    fn the_same_keys_always_produce_the_same_blob() {
        let words: Vec<String> = (0..50_000).map(|i| format!("word-{i:05}")).collect();
        let first = ClosedHashIndex::build(&words).unwrap().to_bytes();
        let again = ClosedHashIndex::build(&words).unwrap().to_bytes();
        assert_eq!(first, again);
    }

    /// The whole size is the perfect hash: under 0.30 bytes per key past the header, a fifth
    /// of `CompactHashIndex` at its default width.
    #[test]
    fn the_blob_is_the_perfect_hash_alone() {
        let words: Vec<String> = (0..100_000).map(|i| format!("word-{i:06}")).collect();
        let closed = ClosedHashIndex::build(&words).unwrap();
        let compact = crate::CompactHashIndex::build(&words, 1).unwrap();
        let per_key = closed.serialized_len() as f64 / words.len() as f64;
        assert!(per_key < 0.30, "{per_key} B/key");
        assert!(closed.serialized_len() * 4 < compact.serialized_len().unwrap());
        for w in &words {
            assert_eq!(closed.id(w), compact.id_unchecked(w), "{w}");
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = ClosedHashIndex::build(["a", "b", "c"]).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_cl_{}.bcl", std::process::id()));
        idx.save(&path).unwrap();
        let back = ClosedHashIndex::load(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes());
        assert_eq!(back.id("b"), idx.id("b"));
        std::fs::remove_file(&path).ok();
    }
}
