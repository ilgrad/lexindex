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
use crate::hash::{fingerprint_full, hash_key, hash_pair};
use epserde::prelude::*;
use ptr_hash::DefaultPtrHash;

const MAGIC_V1: &[u8; 4] = b"BCH1"; // 0.5.x: width counts bytes, fingerprints byte-aligned
const MAGIC_V2: &[u8; 4] = b"BCH2"; // 0.6.0: width counts bits, fingerprints bit-packed
const MAGIC_V3: &[u8; 4] = b"BCH3"; // 0.7: [magic 4][n u64][fp_bits u32][cap u64][mph_len u64][check u32]
const MAGIC_V4: &[u8; 4] = b"BCH4"; // 0.8.0: v3 + [side_len u32][payload u64] before the check
const MAGIC_V5: &[u8; 4] = b"BCH5"; // same layout as v4; side fingerprints are the full 64-bit hash
const HEADER_V3: usize = 36;
const CHECKED_V3: usize = 32; // header bytes the trailing check covers
const HEADER_V4: usize = 48;
const CHECKED_V4: usize = 44;
const SIDE_ENTRY: usize = 20; // hash u64 + fingerprint u64 + id u32

/// Header + owned sections (MPH buffer, side buffer) of a serialised blob.
type SerialisedParts = ([u8; HEADER_V4], Vec<u8>, Vec<u8>);

/// The validated framing of a blob — every field a query will trust — with the MPH region located
/// but not deserialised. Produced by the safe `parse_frame`, which any bytes may reach; consumed by
/// the unsafe `from_shared`, the only place the `epserde` region is touched.
struct Frame {
    n: usize,
    fp_bits: u32,
    overflow_cap: u64,
    mph: std::ops::Range<usize>, // the epserde region; ignored when `n == 0`
    fps: SharedBytes,
    side: Vec<(u64, u64, u32)>,
}

/// The smallest string→dense-id dictionary: a minimal perfect hash plus one small fingerprint per key.
pub struct CompactHashIndex {
    mph: Option<DefaultPtrHash>, // over one hash per distinct hash value; None iff empty
    fps: SharedBytes,            // m fingerprints of fp_bits each, bit-packed in slot order
    fp_bits: u32,                // 1..=64
    n: usize,
    // Length of the MPH's internal remap (see `crate::hash::overflow_cap`). Blobs written before
    // 0.7 recorded it are refused: with no stored keys there is nothing to recompute it from.
    overflow_cap: u64,
    // (hash, full 64-bit second hash, id) for every key whose 64-bit hash collides with another
    // key's, sorted; almost always empty. Keys here have tail ids [m, n) and no slot in the table
    // above. The second hash is stored untruncated regardless of fp_bits, so keys sharing the
    // collided hash are told apart with 64 fresh bits, not fp_bits of them.
    side: Vec<(u64, u64, u32)>,
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
    /// higher false-positive rate on membership: `2^-fingerprint_bits` by construction (6.25% at 4
    /// bits, ≈ 0.4% at 8, ≈ 0.0015% at 16; measured 6.2530% at 4 bits over 2 M non-member probes).
    /// That rate describes *random* non-members — both hashes are deterministic and unseeded, so it
    /// is not a defence against an adversary who chooses the queries. Duplicates are removed; ids
    /// are arbitrary dense slots in `[0, n)` (no defined order) and, like
    /// [`PerfectHashIndex`](crate::PerfectHashIndex)'s, are not reproducible across builds — persist
    /// the blob, not the key list.
    ///
    /// The build **streams**: only a `(hash, second hash)` pair — 16 bytes — is kept per key, never
    /// the strings, so building from a lazy iterator costs `16 × n` bytes of peak memory no matter
    /// how large the keys are. Both hashes are kept at their full 64 bits here regardless of
    /// `fingerprint_bits` (the width only governs what the fingerprint *table* stores), so the one
    /// thing the build cannot tell from a duplicate is two *distinct* keys colliding in **both**
    /// 64-bit hashes at once — `≈ 2^-128` per pair, negligible at any reachable scale. Keys
    /// colliding in the slot hash alone are served exactly, from a side table keyed by the full
    /// second hash.
    pub fn build_bits<I, S>(items: I, fingerprint_bits: u32) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Rejected before a single item is pulled: the iterator may be huge, or endless, and
        // consuming it only to report a bad width would be a hang the caller cannot see into.
        check_fingerprint_bits(fingerprint_bits)?;
        let pairs = items.into_iter().map(|k| hash_pair(k.as_ref())).collect();
        Self::build_from_pairs(pairs, fingerprint_bits)
    }

    /// [`build_bits`](Self::build_bits) after the hashing pass: `pairs` holds
    /// `(hash_key, fingerprint_full)` per key, in any order. Split out so the Python binding can
    /// hash items one at a time while it still holds the GIL and hand over only the 16-byte pairs.
    pub(crate) fn build_from_pairs(
        mut pairs: Vec<(u64, u64)>,
        fingerprint_bits: u32,
    ) -> Result<Self, IndexError> {
        check_fingerprint_bits(fingerprint_bits)?;
        pairs.sort_unstable();
        // Duplicate keys produce identical pairs. Distinct keys deduplicate here only by colliding
        // in both full 64-bit hashes at once — never because the fingerprint table is narrow.
        pairs.dedup();
        let n = pairs.len();
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
                side: Vec::new(),
            });
        }
        // One representative per distinct hash value builds the MPH and owns the slot; the (almost
        // always zero) same-hash leftovers get tail ids [m, n) in the side table.
        let mut mph_hashes = Vec::with_capacity(n);
        let mut side: Vec<(u64, u64, u32)> = Vec::new();
        let mut rep_fps = Vec::with_capacity(n);
        for run in pairs.chunk_by(|a, b| a.0 == b.0) {
            mph_hashes.push(run[0].0);
            rep_fps.push(run[0].1);
            for &(h, fp) in &run[1..] {
                side.push((h, fp, 0)); // ids assigned once m is known
            }
        }
        let m = mph_hashes.len();
        for (j, e) in side.iter_mut().enumerate() {
            e.2 = (m + j) as u32;
        }
        let mph = crate::hash::build_mph(&mph_hashes)?;
        // `m ≤ u32::MAX` and `fingerprint_bits ≤ 64`, so the product fits a u64; whether the table
        // fits this platform's address space is answered here, not by a truncating cast on a
        // 32-bit target.
        let table_len = usize::try_from((m as u64 * fingerprint_bits as u64).div_ceil(8))
            .map_err(|_| IndexError::Format("compact-hash: fingerprint table too large"))?;
        let mut fps = vec![0u8; table_len];
        let mut seen = vec![false; m];
        for (h, fp) in mph_hashes.iter().zip(&rep_fps) {
            let slot = mph.index(h);
            if slot >= m || seen[slot] {
                return Err(IndexError::Format(
                    "compact-hash: construction was not minimal/perfect",
                ));
            }
            seen[slot] = true;
            write_fp(
                &mut fps,
                slot,
                fingerprint_bits,
                *fp & fp_mask(fingerprint_bits),
            );
        }
        let overflow_cap = crate::hash::overflow_cap(&mph, &mph_hashes, m);
        Ok(Self {
            mph: Some(mph),
            fps: SharedBytes::from_owned(fps),
            fp_bits: fingerprint_bits,
            n,
            overflow_cap,
            side,
        })
    }

    /// Number of MPH-resolved keys: `n` minus the side-table entries. Slots, the fingerprint table
    /// and the remap are bounded by this, not by `n`.
    #[inline]
    fn m(&self) -> usize {
        self.n - self.side.len()
    }

    /// Ids of keys whose 64-bit hash collides with another key's, matched by the **full** 64-bit
    /// second hash — off the hot path: it runs only when the table is non-empty. Entries under one
    /// hash carry pairwise-distinct second hashes (the build deduplicates on the pair), so the
    /// match is unambiguous, and it must run *before* the fingerprint table is consulted: a side
    /// key's truncated fingerprint may tie its representative's, and the table would then claim
    /// the query for the representative's id. Entries are sorted.
    #[cold]
    fn side_lookup(&self, h: u64, fp: u64) -> Option<u32> {
        let start = self.side.partition_point(|e| e.0 < h);
        self.side[start..]
            .iter()
            .take_while(|e| e.0 == h)
            .find_map(|e| (e.1 == fp).then_some(e.2))
    }

    /// Slot for a key hash, or `None` when the raw slot is past the MPH's remap — a trailing free
    /// slot no member occupies, which ptr_hash's own `index()` would read out of bounds.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        crate::hash::slot_for(self.mph.as_ref()?, self.m(), self.overflow_cap, h)
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
        if self.side.is_empty() {
            // The overwhelming case (no hash collision anywhere in the index): one predicted
            // branch, then exactly the side-free lookup. Both hashes come from a single pass over
            // the key — a hit needs both, and only a non-member landing past the remap (rarer than
            // 1 in 100 queries) pays for a fingerprint it never compares.
            let (h, full) = hash_pair(key);
            let slot = self.slot_for(h)?;
            return (read_fp(self.fps.as_ref(), slot, self.fp_bits)?
                == full & fp_mask(self.fp_bits))
            .then_some(slot as u32);
        }
        self.id_with_side(key)
    }

    /// [`id`](Self::id) for an index that contains at least one hash collision: the side probe
    /// runs first (it is exact on the full second hash, and the truncated table could otherwise
    /// answer for a side key whose fingerprint bits tie its representative's), then the ordinary
    /// slot-and-fingerprint path.
    #[cold]
    fn id_with_side(&self, key: &str) -> Option<u32> {
        let (h, full) = hash_pair(key);
        if let Some(id) = self.side_lookup(h, full) {
            return Some(id);
        }
        let slot = self.slot_for(h)?;
        (read_fp(self.fps.as_ref(), slot, self.fp_bits)? == full & fp_mask(self.fp_bits))
            .then_some(slot as u32)
    }

    /// Dense id **without** checking the fingerprint — `key` must be a member, or the result is an
    /// arbitrary valid slot. The fastest lookup for a closed vocabulary. Returns `0` when empty, and
    /// for a non-member whose slot falls past the MPH's remap (which is bounded rather than read
    /// unchecked — being unsafe on a wrong key is not one of the trade-offs this method makes). In
    /// the rare index that contains a 64-bit hash collision, keys sharing the collided hash resolve
    /// through the side table (matched by the full second hash — exact for members even there);
    /// every other index skips that with one predictable branch.
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        let h = hash_key(key);
        if !self.side.is_empty() {
            if let Some(id) = self.side_lookup(h, fingerprint_full(key)) {
                return id;
            }
        }
        self.slot_for(h).unwrap_or(0) as u32
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input (`None` where
    /// the fingerprint rejects). Slot resolution streams through the MPH with 32 queries' worth
    /// of software prefetch in flight, and the fingerprint lines are prefetched ahead of the
    /// compare. Measured on real words: 1.1× the per-key loop at 480 k keys, 1.2× at 5 M. The
    /// rare index holding a hash collision takes the per-key path instead — the side probe must
    /// precede the fingerprint compare, which defeats the batched layout.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u32>> {
        let Some(mph) = &self.mph else {
            return vec![None; keys.len()];
        };
        if !self.side.is_empty() {
            return keys.iter().map(|k| self.id_with_side(k.as_ref())).collect();
        }
        // Both hashes are computed in one pass over the keys, so the verify pass below never
        // touches the strings again — it is a pure fingerprint-table compare with the lines
        // prefetched ahead.
        let mut hashes = Vec::with_capacity(keys.len());
        let mut wanted = Vec::with_capacity(keys.len());
        let mask = fp_mask(self.fp_bits);
        for k in keys {
            let (h, full) = hash_pair(k.as_ref());
            hashes.push(h);
            wanted.push(full & mask);
        }
        // ptr_hash's stream iterator is internal-iteration only (`next()` is unimplemented by
        // design), so drain it with `for_each`.
        // MINIMAL=false: raw slots, so the stream never touches the remap (see `slot_for`). Raw
        // slots ≥ n are triaged here: past the cap they are provably non-members, otherwise the
        // (rare, ~1%) per-key `index()` resolves the remapped slot.
        let m = self.m();
        let slots = crate::hash::triage_slots(mph, m, self.overflow_cap, &hashes);
        let fps = self.fps.as_ref();
        const AHEAD: usize = 32;
        (0..keys.len())
            .map(|i| {
                if let Some(&s) = slots.get(i + AHEAD) {
                    if s < m {
                        crate::blob::prefetch_byte(
                            fps,
                            (s as u64 * self.fp_bits as u64 / 8) as usize,
                        );
                    }
                }
                let slot = slots[i];
                if slot < m {
                    read_fp(fps, slot, self.fp_bits)
                        .and_then(|f| (f == wanted[i]).then_some(slot as u32))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Whether `key` is present (subject to the fingerprint false-positive rate).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Serialised header + owned sections (the fingerprint table is borrowed separately): shared by
    /// [`to_bytes`](Self::to_bytes) and the streaming [`save`](Self::save) so the two emit
    /// byte-identical blobs.
    fn serialised_parts(&self) -> Result<SerialisedParts, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let mut side_buf = Vec::with_capacity(self.side.len() * SIDE_ENTRY);
        for &(h, fp, id) in &self.side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&fp.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut payload = crate::hash::BlockHasher::new();
        payload.update(&mph_buf);
        payload.update(self.fps.as_ref());
        payload.update(&side_buf);
        let mut header = [0u8; HEADER_V4];
        header[0..4].copy_from_slice(MAGIC_V5);
        header[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        header[12..16].copy_from_slice(&self.fp_bits.to_le_bytes());
        header[16..24].copy_from_slice(&self.overflow_cap.to_le_bytes());
        header[24..32].copy_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        header[32..36].copy_from_slice(&(self.side.len() as u32).to_le_bytes());
        header[36..44].copy_from_slice(&payload.finish().to_le_bytes());
        let check = crate::hash::hash_bytes(&header[..CHECKED_V4]) as u32;
        header[CHECKED_V4..].copy_from_slice(&check.to_le_bytes());
        Ok((header, mph_buf, side_buf))
    }

    /// Serialise to `[magic "BCH5"][n u64][fp_bits u32][overflow_cap u64][mph_len u64][side_len u32]
    /// [payload u64][check u32][mph epserde bytes][bit-packed fingerprints][side entries]`. `check`
    /// is a hash of the preceding header bytes — `overflow_cap` bounds an otherwise unchecked read
    /// inside the MPH, so it must not be taken on trust from a blob that lost bytes in transit —
    /// and `payload` a streaming hash of everything after the header, verified on owned loads.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        let fp = self.fps.as_ref();
        let mut out = Vec::with_capacity(HEADER_V4 + mph_buf.len() + fp.len() + side_buf.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(fp);
        out.extend_from_slice(&side_buf);
        Ok(out)
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it — for sizing
    /// a buffer or reporting bytes/key; [`save`](Self::save) writes exactly this many.
    pub fn serialized_len(&self) -> Result<usize, IndexError> {
        let mph = match &self.mph {
            Some(mph) => crate::hash::mph_serialized_len(mph)?,
            None => 0,
        };
        Ok(HEADER_V4 + mph + self.fps.len() + self.side.len() * SIDE_ENTRY)
    }

    /// Reconstruct from [`CompactHashIndex::to_bytes`] output (copies the blob into owned memory).
    ///
    /// The lexindex header, fingerprint table and side table are fully bounds-validated, and owned
    /// loads verify a streaming checksum of the whole payload, so *accidental* corruption anywhere
    /// in the blob fails cleanly.
    ///
    /// # Safety
    /// The embedded minimal perfect hash is deserialised by [`epserde`] and cannot be validated:
    /// `ptr_hash` reads its pilot table unchecked, and the fields that would bound that read
    /// (`parts`, `buckets`, the fast-modulo constants) are private, so no amount of checking on
    /// this side can make a hostile blob safe. A crafted blob can therefore read out of bounds.
    /// The caller must pass only bytes produced by [`to_bytes`](Self::to_bytes) /
    /// [`save`](Self::save) — the same "trust your own blob" contract as
    /// [`load_mmap`](Self::load_mmap), which additionally skips the checksum scan.
    pub unsafe fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        // SAFETY: forwarded from this function's contract.
        unsafe { Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true) }
    }

    /// The lexindex framing of `blob`, parsed and bounds-validated — magic, header checksum, the
    /// payload checksum when `verify` (owned loads; off for mmap so mapping stays proportional to
    /// the MPH alone), lengths, the side table and the fingerprint range — with the MPH region
    /// located but **not** deserialised. Safe on arbitrary bytes: this is the half a property test
    /// fuzzes, and everything `from_shared` trusts comes out of here.
    fn parse_frame(blob: &SharedBytes, verify: bool) -> Result<Frame, IndexError> {
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
        if bytes.len() < 4 {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let (header, checked) = match &bytes[0..4] {
            m if m == MAGIC_V3 => (HEADER_V3, CHECKED_V3),
            m if m == MAGIC_V4 || m == MAGIC_V5 => (HEADER_V4, CHECKED_V4),
            _ => return Err(IndexError::Format("bad magic or truncated header")),
        };
        if bytes.len() < header {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[checked..checked + 4].try_into().unwrap());
        if check != crate::hash::hash_bytes(&bytes[..checked]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let v45 = header == HEADER_V4;
        // Owned v4/v5 loads verify the whole payload — one streaming pass over everything after
        // the header — so a flipped byte in the MPH region, the fingerprint table or the side
        // table is rejected here rather than perturbing answers (or aborting in epserde) later.
        if verify && v45 {
            let stored = u64::from_le_bytes(bytes[36..44].try_into().unwrap());
            if stored != crate::hash::hash_block(&bytes[HEADER_V4..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
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
        // `mph_len` and the side-byte count are header-supplied; convert and multiply checked so a
        // fabricated length fails cleanly on every target width instead of truncating or wrapping
        // on a 32-bit one.
        let mph_len = usize::try_from(u64::from_le_bytes(bytes[24..32].try_into().unwrap()))
            .map_err(|_| IndexError::Format("mph length out of range"))?;
        let side_len = if v45 {
            u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize
        } else {
            0
        };
        if side_len > n || (side_len == n && n > 0) {
            return Err(IndexError::Format("side table length out of range"));
        }
        // A 0.8.0 side table stored fingerprints truncated to `fp_bits`, which cannot be widened
        // after the fact (the keys are gone). Collision-free 0.8.0 blobs are bit-identical to v5
        // and load fine; the astronomically rare collided one must be rebuilt.
        if &bytes[0..4] == MAGIC_V4 && side_len > 0 {
            return Err(IndexError::Format(
                "compact-hash: this blob was written by lexindex 0.8.0 and contains a collision \
                 side table with truncated fingerprints; rebuild the index with 0.8.1 or later",
            ));
        }
        let m = n - side_len;
        let side_bytes = side_len
            .checked_mul(SIDE_ENTRY)
            .ok_or(IndexError::Format("side table length out of range"))?;
        let side_start = bytes
            .len()
            .checked_sub(side_bytes)
            .ok_or(IndexError::Format("side table length out of range"))?;
        let mph_end = header
            .checked_add(mph_len)
            .filter(|&e| e <= side_start)
            .ok_or(IndexError::Format("mph length out of range"))?;
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
                "compact-hash: side-table ids are not the tail id range",
            ));
        }
        let fps = blob
            .subslice(mph_end, side_start)
            .ok_or(IndexError::Format("fingerprint range out of range"))?;
        // `n` is untrusted (read from the header), so guard the multiply — a fabricated huge `n` would
        // otherwise overflow and panic in a debug build instead of failing cleanly.
        let expected = (m as u64)
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
        Ok(Frame {
            n,
            fp_bits,
            overflow_cap,
            mph: header..mph_end,
            fps,
            side,
        })
    }

    /// Reconstruct from a shared source: the validated framing from
    /// [`parse_frame`](Self::parse_frame), then the MPH deserialised by `epserde` into memory; the
    /// fingerprint table (the bulk) is borrowed zero-copy, so `load_mmap` never copies it.
    ///
    /// # Safety
    /// The MPH region must be an `epserde` image this crate wrote — see
    /// [`from_bytes`](Self::from_bytes): `ptr_hash` reads its pilot table unchecked, so a crafted
    /// region is undefined behaviour and nothing here can reject it. The framing checks that run
    /// first turn every *accidental* corruption into an error.
    unsafe fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let frame = Self::parse_frame(&blob, verify)?;
        let m = frame.n - frame.side.len();
        let mph = if frame.n == 0 {
            None
        } else {
            let mut reader = &blob.as_ref()[frame.mph];
            // A safe fn in epserde 0.8 that is unsound for a crafted region: the caller's contract
            // is what makes this call sound.
            let mph = DefaultPtrHash::deserialize_full(&mut reader)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
            if mph.n() != m {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        Ok(Self {
            mph,
            fps: frame.fps,
            fp_bits: frame.fp_bits,
            n: frame.n,
            overflow_cap: frame.overflow_cap,
            side: frame.side,
        })
    }

    /// Write the dictionary to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section, so saving peaks at the index's own memory plus the small MPH buffer
    /// rather than a full serialised copy.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            use std::io::Write;
            w.write_all(&header)?;
            w.write_all(&mph_buf)?;
            w.write_all(self.fps.as_ref())?;
            w.write_all(&side_buf)
        })
    }

    /// Load a dictionary previously written with [`CompactHashIndex::save`] (reads the whole file
    /// and verifies the payload checksum).
    ///
    /// # Safety
    /// The file must have been written by [`save`](Self::save) — see
    /// [`from_bytes`](Self::from_bytes) for why a crafted blob cannot be rejected.
    pub unsafe fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        // SAFETY: forwarded from this function's contract.
        unsafe { Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true) }
    }

    /// Memory-map the file and borrow the fingerprint table zero-copy (only the small MPH is read
    /// into memory). Skips the payload-checksum scan `load` performs — the mapped file is trusted
    /// intact.
    ///
    /// # Safety
    /// Two obligations. The file must have been written by [`save`](Self::save): the embedded
    /// perfect hash cannot be validated, so a crafted file is undefined behaviour — see
    /// [`from_bytes`](Self::from_bytes). And the caller must guarantee the file is not modified or
    /// truncated by any process while the returned index is alive — see
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: both forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        unsafe { Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false) }
    }
}

fn check_fingerprint_bits(bits: u32) -> Result<(), IndexError> {
    if (1..=64).contains(&bits) {
        Ok(())
    } else {
        Err(IndexError::Format(
            "compact-hash: fingerprint_bits must be in 1..=64",
        ))
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

    /// The loader is `unsafe` because a crafted blob cannot be rejected (see
    /// [`CompactHashIndex::from_bytes`]). Every blob below is either produced by this crate or a
    /// deliberate corruption of the *validated* framing, which the loader rejects before the MPH is
    /// touched.
    fn from_bytes(bytes: &[u8]) -> Result<CompactHashIndex, IndexError> {
        unsafe { CompactHashIndex::from_bytes(bytes) }
    }

    /// The safe half of the loader must never panic on arbitrary bytes — only `Ok`/`Err` — which
    /// is where the "garbage fails cleanly" property lives now that the loaders are `unsafe`.
    #[test]
    fn parse_frame_never_panics() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(&prop::collection::vec(any::<u8>(), 0..256), |data| {
                let _ = CompactHashIndex::parse_frame(&SharedBytes::from_owned(data), true);
                Ok(())
            })
            .unwrap();
    }

    /// Every truncation of a real blob is rejected by the framing alone, with or without the
    /// payload checksum — so the epserde region is never reached on a short read.
    #[test]
    fn parse_frame_rejects_every_truncation() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let blob = idx.to_bytes().unwrap();
        for verify in [true, false] {
            for k in 0..blob.len() {
                let cut = SharedBytes::from_owned(blob[..k].to_vec());
                assert!(
                    CompactHashIndex::parse_frame(&cut, verify).is_err(),
                    "truncated to {k} bytes (verify={verify}) parsed"
                );
            }
            assert!(
                CompactHashIndex::parse_frame(&SharedBytes::from_owned(blob.clone()), verify)
                    .is_ok()
            );
        }
    }

    #[test]
    fn serialized_len_matches_to_bytes() {
        for keys in [vec![], vec!["alpha"], vec!["alpha", "beta", "gamma"]] {
            let idx = CompactHashIndex::build(&keys, 1).unwrap();
            assert_eq!(idx.serialized_len().unwrap(), idx.to_bytes().unwrap().len());
        }
    }

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
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        for w in ["GET", "POST", "PUT", "DELETE"] {
            assert_eq!(restored.id(w), idx.id(w));
        }
        assert_eq!(restored.id("PATCH"), None);
        assert!(from_bytes(b"nope").is_err());
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
            assert!(matches!(from_bytes(&bad_width), Err(IndexError::Format(_))));
        }
        // ...and a BCH1 blob may only claim 1, 2 or 4 *bytes*.
        let mut bad_v1 = good.clone();
        bad_v1[0..4].copy_from_slice(b"BCH1");
        bad_v1[12] = 3;
        assert!(matches!(from_bytes(&bad_v1), Err(IndexError::Format(_))));

        // Dropping a byte makes the table length disagree with ceil(n * fp_bits / 8).
        assert!(matches!(
            from_bytes(&good[..good.len() - 1]),
            Err(IndexError::Format(_))
        ));
    }

    /// A 0.7 "BCH3" blob (no side table, no payload checksum) still loads and answers like the
    /// index that wrote it.
    #[test]
    fn a_0_7_bch3_blob_still_loads() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 2).unwrap();
        let v4 = idx.to_bytes().unwrap();
        assert_eq!(&v4[0..4], b"BCH5");
        assert!(idx.side.is_empty());
        let mut v3 = Vec::with_capacity(v4.len() - HEADER_V4 + HEADER_V3);
        v3.extend_from_slice(b"BCH3");
        v3.extend_from_slice(&v4[4..32]); // n, fp_bits, cap, mph_len
        let check = crate::hash::hash_bytes(&v3[..CHECKED_V3]) as u32;
        v3.extend_from_slice(&check.to_le_bytes());
        v3.extend_from_slice(&v4[HEADER_V4..]); // mph + fingerprints (side is empty)
        let restored = from_bytes(&v3).unwrap();
        for w in ["alpha", "beta", "gamma"] {
            assert_eq!(restored.id(w), idx.id(w));
        }
        assert_eq!(restored.id("delta"), None);
    }

    /// 0.5/0.6 blobs predate the recorded remap bound and store no keys to recompute it from, so
    /// they are refused with a message that names the fix rather than loaded unbounded.
    #[test]
    fn a_pre_0_7_blob_is_refused_with_a_rebuild_message() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let v4 = idx.to_bytes().unwrap();
        assert_eq!(&v4[0..4], b"BCH5");
        for (magic, width) in [(b"BCH1", 1u32), (b"BCH2", 8)] {
            // The 0.5/0.6 layout: [magic][n][width][mph_len][mph][fingerprints].
            let mut old = Vec::new();
            old.extend_from_slice(magic);
            old.extend_from_slice(&v4[4..12]); // n
            old.extend_from_slice(&width.to_le_bytes());
            old.extend_from_slice(&v4[24..32]); // mph_len
            old.extend_from_slice(&v4[HEADER_V4..]);
            let err = match from_bytes(&old) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("a pre-0.7 blob was accepted"),
            };
            assert!(err.contains("rebuild the index with 0.7"), "{err}");
        }
    }

    /// A header that lost bytes in transit must be refused rather than used to steer queries
    /// (`overflow_cap` bounds an otherwise unchecked read), and — new in v4 — so must a flipped
    /// byte anywhere in the payload, caught by the whole-payload checksum on owned loads.
    #[test]
    fn corrupt_headers_and_payloads_are_refused() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let good = idx.to_bytes().unwrap();
        assert!(from_bytes(&good).is_ok());
        for pos in [4, 12, 16, 23, 24, 31, 32, 35, 36, 43, 44, 47] {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "header byte {pos} was accepted");
        }
        for pos in (HEADER_V4..good.len()).step_by(5) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "payload byte {pos} was accepted");
        }
    }

    /// A real 64-bit hash collision (the pinned pair from `crate::hash`) must build and resolve
    /// both keys — the fingerprint stands in for the stored key in the side probe. Whatever the
    /// width, no member may ever be a false negative.
    #[test]
    fn colliding_keys_build_and_resolve_by_fingerprint() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys: Vec<String> = (0..500).map(|i| format!("filler-{i:03}")).collect();
        keys.push(a.to_string());
        keys.push(b.to_string());
        let idx = CompactHashIndex::build_bits(&keys, 64).unwrap();
        assert_eq!(idx.len(), 502);
        assert_eq!(idx.side.len(), 1);
        let (ia, ib) = (idx.id(a).unwrap(), idx.id(b).unwrap());
        assert_ne!(ia, ib);
        assert_eq!(idx.id_unchecked(a), ia);
        assert_eq!(idx.id_unchecked(b), ib);
        assert_eq!(idx.ids_of(&[a, b]), vec![Some(ia), Some(ib)]);
        let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k).unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            keys.len(),
            "ids must stay a bijection onto [0, n)"
        );
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.side, idx.side);
        assert_eq!(restored.id(a), Some(ia));
        assert_eq!(restored.id(b), Some(ib));
        // The side table stores the full second hash, so the table width must never decide whether
        // the pair stays two keys: the pinned pair's low fingerprint bits tie at 1 bit, which the
        // 0.8.0 truncated side table silently merged into one id.
        for bits in [1u32, 2, 4, 8, 16] {
            let idx = CompactHashIndex::build_bits(&keys, bits).unwrap();
            assert_eq!(idx.len(), 502, "bits={bits}");
            assert_eq!(idx.side.len(), 1, "bits={bits}");
            let (ia, ib) = (idx.id(a).unwrap(), idx.id(b).unwrap());
            assert_ne!(ia, ib, "bits={bits}");
            assert_eq!(idx.id_unchecked(a), ia, "bits={bits}");
            assert_eq!(idx.id_unchecked(b), ib, "bits={bits}");
            assert_eq!(idx.ids_of(&[a, b]), vec![Some(ia), Some(ib)], "bits={bits}");
            for k in &keys {
                assert!(idx.contains(k), "false negative on {k:?} at {bits} bits");
            }
            let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
            assert_eq!(restored.id(a), Some(ia), "bits={bits}");
            assert_eq!(restored.id(b), Some(ib), "bits={bits}");
        }
    }

    /// A bad fingerprint width is rejected before the iterator is touched — it may be endless, and
    /// hashing it to completion just to report the width would hang instead of returning `Err`.
    #[test]
    fn a_bad_fingerprint_width_is_rejected_before_the_iterator_runs() {
        let mut pulled = 0usize;
        let items = std::iter::repeat_with(|| {
            pulled += 1;
            "x"
        })
        .take(1_000);
        assert!(CompactHashIndex::build_bits(items, 0).is_err());
        assert_eq!(pulled, 0, "the iterator must not be consumed");
    }

    /// A 0.8.0 "BCH4" blob is bit-identical to v5 when its side table is empty — it must load.
    /// One that *has* a side table stored truncated fingerprints there, which cannot be widened
    /// without the keys — it must be refused with a message naming the rebuild.
    #[test]
    fn a_0_8_0_bch4_blob_loads_only_without_a_side_table() {
        let rehash = |blob: &mut [u8]| {
            let payload = crate::hash::hash_block(&blob[HEADER_V4..]);
            blob[36..44].copy_from_slice(&payload.to_le_bytes());
            let check = crate::hash::hash_bytes(&blob[..CHECKED_V4]) as u32;
            blob[CHECKED_V4..HEADER_V4].copy_from_slice(&check.to_le_bytes());
        };

        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 2).unwrap();
        assert!(idx.side.is_empty());
        let mut v4 = idx.to_bytes().unwrap();
        v4[0..4].copy_from_slice(b"BCH4");
        rehash(&mut v4);
        let restored = from_bytes(&v4).unwrap();
        for w in ["alpha", "beta", "gamma"] {
            assert_eq!(restored.id(w), idx.id(w));
        }

        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = CompactHashIndex::build([a, b], 1).unwrap();
        assert_eq!(idx.side.len(), 1);
        let mut v4 = idx.to_bytes().unwrap();
        v4[0..4].copy_from_slice(b"BCH4");
        rehash(&mut v4);
        let err = match from_bytes(&v4) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a 0.8.0 blob with a truncated side table was accepted"),
        };
        assert!(err.contains("rebuild the index with 0.8.1"), "{err}");
    }

    /// Side ids are handed out verbatim by `id()`, so the loader must pin them to the tail range
    /// [m, n) structurally — the checksums vouch for transport, not for what was written. A blob
    /// with a re-checksummed out-of-range or duplicate id is refused, never served.
    #[test]
    fn tampered_side_ids_are_refused_even_with_valid_checksums() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = CompactHashIndex::build([a, b, "filler"], 1).unwrap();
        assert_eq!(idx.side.len(), 1);
        let good = idx.to_bytes().unwrap();
        // The lone side entry's id lives in the blob's last 4 bytes.
        for bad_id in [0u32, 1, 3, u32::MAX] {
            let mut bad = good.clone();
            let at = bad.len() - 4;
            bad[at..].copy_from_slice(&bad_id.to_le_bytes());
            let payload = crate::hash::hash_block(&bad[HEADER_V4..]);
            bad[36..44].copy_from_slice(&payload.to_le_bytes());
            let check = crate::hash::hash_bytes(&bad[..CHECKED_V4]) as u32;
            bad[CHECKED_V4..HEADER_V4].copy_from_slice(&check.to_le_bytes());
            let err = match from_bytes(&bad) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("side id {bad_id} was accepted"),
            };
            assert!(err.contains("side-table ids"), "{err}");
        }
    }

    /// `save` streams sections instead of assembling one buffer; the file must still be
    /// byte-identical to `to_bytes`.
    #[test]
    fn save_streams_the_same_bytes_as_to_bytes() {
        let words: Vec<String> = (0..500).map(|i| format!("w{i}")).collect();
        let idx = CompactHashIndex::build(&words, 2).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_chstream_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes().unwrap());
        std::fs::remove_file(&path).ok();
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
                if raw >= idx.m() && (raw - idx.m()) as u64 >= idx.overflow_cap {
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
            let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
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
        assert!(matches!(from_bytes(&blob), Err(IndexError::Format(_))));
    }

    #[test]
    fn empty_round_trips() {
        let empty = CompactHashIndex::build_bits(Vec::<String>::new(), 4).unwrap();
        assert_eq!(empty.fingerprint_bits(), 4);
        assert!(empty.is_empty() && empty.id("x").is_none());
        let empty = CompactHashIndex::build(Vec::<String>::new(), 1).unwrap();
        assert!(empty.is_empty() && empty.id("x").is_none() && empty.id_unchecked("x") == 0);
        let restored = from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = CompactHashIndex::build(["a", "b", "c"], 1).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_ch_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(
            unsafe { CompactHashIndex::load(&path) }.unwrap().id("b"),
            idx.id("b")
        );
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
        let mapped = unsafe { CompactHashIndex::load_mmap(&path) }.unwrap();
        assert_eq!(mapped.len(), idx.len());
        for w in &words {
            assert_eq!(mapped.id(w), idx.id(w));
        }
        assert!(!mapped.contains("k999"));
        std::fs::remove_file(&path).ok();
    }
}
