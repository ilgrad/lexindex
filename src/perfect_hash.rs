//! Minimal-perfect-hash dictionary backed by [`ptr_hash`].
//!
//! For a fixed set of `n` distinct strings, a minimal perfect hash maps each to a distinct slot in
//! `[0, n)` with no gaps and near-`O(1)` lookup in tiny space. `ptr_hash` builds the MPH; we key it on
//! a deterministic 64-bit hash of each string (so queries take `&str` without allocating) and keep a
//! [`StringArena`] from slot → key. The arena doubles as a **membership check**: an MPH maps *any*
//! input to some slot, so a query is only a hit if the stored key at that slot equals the query.
//! (ptr_hash's own minimal `index()` is unchecked past its remap for non-members — queries bound it
//! with the recorded remap length; see `slot_for`.)
//!
//! Two distinct keys colliding in the 64-bit hash cannot fail the build (the hash is deterministic
//! and unseeded — that is what makes the serialised MPH reloadable — so a retry could never help).
//! Instead the MPH is built over one representative per distinct hash value and the colliding
//! leftovers get tail ids served from a tiny **side table**, consulted only after the arena
//! comparison has already missed. The expected number of colliding pairs is `n(n-1)/2^65`:
//! negligible below ~10 M keys (2.7e-6 at 10 M), 2.7e-4 at 100 M, ~2.7% at 1 G — so the side table
//! is almost always empty and costs nothing on the hot path.

use crate::IndexError;
use crate::arena::StringArena;
use crate::blob::SharedBytes;
use crate::hash::hash_key;
use epserde::prelude::*;
use ptr_hash::DefaultPtrHash;

const MAGIC_V2: &[u8; 4] = b"BMP2"; // 0.5/0.6: [magic 4][n u64][mph_len u64]
const MAGIC_V3: &[u8; 4] = b"BMP3"; // 0.7: [magic 4][n u64][overflow_cap u64][mph_len u64][check u32]
const MAGIC_V4: &[u8; 4] = b"BMP4"; // [magic 4][n u64][mph_len u64][side_len u32][payload u64][check u32]
const HEADER_V2: usize = 20;
const HEADER_V3: usize = 32;
const CHECKED_V3: usize = 28; // header bytes the trailing check covers
const HEADER_V4: usize = 36;
const CHECKED_V4: usize = 32;
const SIDE_ENTRY: usize = 12; // hash u64 + id u32
/// "No key assigned to this slot yet" while a build fills its slot → key-index table. Never a real
/// index: `build` rejects `n > u32::MAX`, so the largest index a key can have is `u32::MAX - 1`.
const NO_KEY: u32 = u32::MAX;

/// The v4 header, from the four values that vary. Shared by [`PerfectHashIndex::to_bytes`] and the
/// streaming [`build_to_file`](PerfectHashIndex::build_to_file), which assembles the same blob
/// without ever holding the index — one writer of this layout, so the two cannot drift.
fn header_bytes(n: usize, mph_len: usize, side_len: usize, payload: u64) -> [u8; HEADER_V4] {
    let mut header = [0u8; HEADER_V4];
    header[0..4].copy_from_slice(MAGIC_V4);
    header[4..12].copy_from_slice(&(n as u64).to_le_bytes());
    header[12..20].copy_from_slice(&(mph_len as u64).to_le_bytes());
    header[20..24].copy_from_slice(&(side_len as u32).to_le_bytes());
    header[24..32].copy_from_slice(&payload.to_le_bytes());
    let check = crate::hash::hash_bytes(&header[..CHECKED_V4]) as u32;
    header[CHECKED_V4..].copy_from_slice(&check.to_le_bytes());
    header
}

/// Header + owned sections (MPH buffer, side buffer) of a serialised blob.
type SerialisedParts = ([u8; HEADER_V4], Vec<u8>, Vec<u8>);

/// The validated framing of a blob — every field a query will trust — with the MPH region located
/// but not deserialised. Produced by the safe `parse_frame`, which any bytes may reach; consumed by
/// the unsafe `from_shared`, the only place the `epserde` region is touched.
struct Frame {
    n: usize,
    mph: std::ops::Range<usize>, // the epserde region; ignored when `n == 0`
    arena: StringArena,
    side: Vec<(u64, u32)>,
}

/// An immutable minimal-perfect-hash dictionary: fastest exact `string → dense id` with reverse lookup.
pub struct PerfectHashIndex {
    mph: Option<DefaultPtrHash>, // over one hash per distinct hash value; None iff empty
    arena: StringArena, // id → key (also verifies membership); ids [m, n) are the side keys
    n: usize,
    // Length of the MPH's internal remap (see `crate::hash::overflow_cap`); always recomputed from
    // the arena on load, so the header's copy is never trusted for bounds.
    overflow_cap: u64,
    // (hash, id) for every key whose hash collides with another key's, sorted; almost always empty.
    side: Vec<(u64, u32)>,
}

impl PerfectHashIndex {
    /// Build from a collection of strings. Duplicates are removed; ids are arbitrary slots in `[0, n)`
    /// (no defined order — use [`crate::StringIndex`] when order matters).
    ///
    /// Ids are **not reproducible**: the perfect hash's construction is randomised, so building the
    /// same key set twice assigns different slots (measured on 50 k keys, ~53 % keep their id). Ids
    /// survive [`save`](Self::save)/[`load`](Self::load) of one built index exactly, so persist the
    /// blob — not the key list — whenever an id is stored outside the index.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Sorted and deduplicated in place, comparing through `AsRef` rather than collecting owned
        // `String`s: the keys are copied once more into the structure below, so an intermediate
        // copy of the whole corpus bought nothing.
        let mut keys: Vec<S> = items.into_iter().collect();
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.dedup_by(|a, b| a.as_ref() == b.as_ref());
        let n = keys.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "perfect-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        if n == 0 {
            return Ok(Self {
                mph: None,
                arena: StringArena::build(Vec::<&str>::new()), // offsets = [0]: a valid empty arena
                n: 0,
                overflow_cap: 0,
                side: Vec::new(),
            });
        }
        // The arena holds every key exactly once, so the sum of the key lengths *is* its data
        // length — free here, because this pass already has each key in hand, and it saves the
        // arena a walk in slot order that would otherwise cost a cache miss per key.
        let mut data_len = 0usize;
        let hashes: Vec<u64> = keys
            .iter()
            .map(|k| {
                let key = k.as_ref();
                data_len += key.len();
                hash_key(key)
            })
            .collect();
        // A sorted copy answers whether *any* two keys share a hash, and — when none does — is
        // also what the MPH is built from. Feeding it rather than `hashes` is free (it exists
        // either way) and keeps the MPH's input in the same order the pre-0.10 code gave it, so
        // nothing about construction changes. The alternative it replaces — always partitioning
        // `(hash, index)` pairs — allocated 16 bytes per key more, and held them across the MPH
        // build, to describe a situation that essentially never arises: a 64-bit collision among n
        // keys needs n ≈ 10^8 to reach probability 1e-4.
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Self::build_with_collisions(keys, hashes, data_len);
        }
        // No collision: every key is its own representative, and `hashes` — still in key order —
        // maps each slot back to its key with no further indirection.
        let mph = crate::hash::build_mph(&sorted)?;
        let mut by_slot: Vec<u32> = vec![NO_KEY; n];
        for (i, h) in hashes.iter().enumerate() {
            let slot = mph.index(h);
            if slot >= n || by_slot[slot] != NO_KEY {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = i as u32;
        }
        let overflow_cap = crate::hash::overflow_cap(&mph, &sorted, n);
        // Before the arena allocates: neither hash vector is needed alongside it.
        drop(sorted);
        drop(hashes);
        let arena = StringArena::build_exact(
            by_slot.iter().map(|&i| keys[i as usize].as_ref()),
            n,
            data_len,
        );
        Ok(Self {
            mph: Some(mph),
            arena,
            n,
            overflow_cap,
            side: Vec::new(),
        })
    }

    /// The build path for a key set in which at least two distinct keys share a [`hash_key`]
    /// value. One representative per distinct hash value builds the MPH; the colliding leftovers
    /// get tail ids `[m, n)` and are found through the side table instead. Split out of
    /// [`build`](Self::build) because it costs memory — the `(hash, index)` partition — that the
    /// overwhelmingly common case must not pay.
    #[cold]
    fn build_with_collisions<S: AsRef<str>>(
        keys: Vec<S>,
        hashes: Vec<u64>,
        data_len: usize,
    ) -> Result<Self, IndexError> {
        let n = keys.len();
        let (mph_hashes, extras) = crate::hash::split_collisions(&hashes);
        let m = mph_hashes.len();
        let mph = crate::hash::build_mph(&mph_hashes)?;
        let mut is_extra = vec![false; n];
        for &(_, i) in &extras {
            is_extra[i as usize] = true;
        }
        // Slots hold key indices, not the keys: the arena copies the bytes anyway, and 4 bytes a
        // slot rather than a 16-byte `Option<&str>` is 12 bytes per key off the build's peak.
        let mut by_slot: Vec<u32> = vec![NO_KEY; m];
        for (i, h) in hashes.iter().enumerate() {
            if is_extra[i] {
                continue;
            }
            let slot = mph.index(h);
            if slot >= m || by_slot[slot] != NO_KEY {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = i as u32;
        }
        let overflow_cap = crate::hash::overflow_cap(&mph, &mph_hashes, m);
        drop(hashes);
        drop(mph_hashes);
        let arena = StringArena::build_exact(
            by_slot
                .iter()
                .map(|&i| keys[i as usize].as_ref())
                .chain(extras.iter().map(|&(_, i)| keys[i as usize].as_ref())),
            n,
            data_len,
        );
        let mut side: Vec<(u64, u32)> = extras
            .iter()
            .enumerate()
            .map(|(j, &(h, _))| (h, (m + j) as u32))
            .collect();
        side.sort_unstable(); // by hash, for the binary search in `side_lookup`
        Ok(Self {
            mph: Some(mph),
            arena,
            n,
            overflow_cap,
            side,
        })
    }

    /// Number of MPH-resolved keys: `n` minus the side-table entries. Slots and the remap are
    /// bounded by this, not by `n`.
    #[inline]
    fn m(&self) -> usize {
        self.n - self.side.len()
    }

    /// Ids of keys whose 64-bit hash collides with another key's live here, off the hot path: the
    /// probe runs only after the arena comparison has already missed (or, for `id_unchecked`, only
    /// when the table is non-empty — i.e. for indexes that actually contain a collision).
    #[cold]
    fn side_lookup(&self, h: u64, key: &str) -> Option<u32> {
        let start = self.side.partition_point(|e| e.0 < h);
        self.side[start..]
            .iter()
            .take_while(|e| e.0 == h)
            .find_map(|e| (self.arena.get(e.1 as usize) == Some(key)).then_some(e.1))
    }

    /// Slot for a key hash, or `None` when the raw slot is past the MPH's remap — a trailing free
    /// slot no member occupies, which ptr_hash's own `index()` would read out of bounds.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        crate::hash::slot_for(self.mph.as_ref()?, self.m(), self.overflow_cap, h)
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the dictionary has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Dense id of `key`, or `None` if absent (membership is verified against the stored key).
    pub fn id(&self, key: &str) -> Option<u32> {
        if self.side.is_empty() {
            // The overwhelming case (no hash collision anywhere in the index): one predicted
            // branch, then exactly the side-free lookup — the hash dies at slot resolution,
            // nothing stays live for a probe that cannot happen.
            let slot = self.slot_for(hash_key(key))?;
            return (self.arena.get(slot) == Some(key)).then_some(slot as u32);
        }
        self.id_with_side(key)
    }

    /// [`id`](Self::id) for an index that contains at least one hash collision: the side probe
    /// runs only after the arena comparison has missed.
    #[cold]
    fn id_with_side(&self, key: &str) -> Option<u32> {
        let h = hash_key(key);
        if let Some(slot) = self.slot_for(h) {
            if self.arena.get(slot) == Some(key) {
                return Some(slot as u32);
            }
        }
        self.side_lookup(h, key)
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input (`None` where
    /// absent). Not just a loop — the key bytes are prefetched ahead of the hashing pass, slot
    /// resolution streams through the MPH with 16 queries' worth of prefetch in flight, and the
    /// arena's offset and data lines are prefetched ahead of the key comparison, so the cache
    /// misses a one-at-a-time loop pays per key overlap instead of serialising.
    ///
    /// Real-word bigrams at 10 M, against the per-key loop: **3.6×** when the batch's strings are
    /// scattered in memory (3.3× at 1 M, 3.2× at 5 M). Keys allocated in probe order are already
    /// visible to the hardware prefetcher, and there the win is the single FFI crossing rather
    /// than the prefetching. See [`CompactHashIndex::ids_of`] for the same measurement in more
    /// detail — this index pays for the stored keys it compares against, which is why it is the
    /// slower of the two either way.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u32>> {
        let Some(mph) = &self.mph else {
            return vec![None; keys.len()];
        };
        // The slice holds the `String`/`&str` headers contiguously, but their bytes are wherever
        // they were allocated, so hashing a batch is one dependent cache miss per key. Pulling a
        // later key's first line in now is the prefetch the per-key `id` cannot make — it has no
        // next key to look at — and the compare pass below does the same for its own second touch.
        const AHEAD: usize = 16;
        let mut hashes: Vec<u64> = Vec::with_capacity(keys.len());
        for (i, k) in keys.iter().enumerate() {
            if let Some(next) = keys.get(i + AHEAD) {
                crate::blob::prefetch_byte(next.as_ref().as_bytes(), 0);
            }
            hashes.push(hash_key(k.as_ref()));
        }
        // ptr_hash's stream iterator is internal-iteration only (`next()` is unimplemented by
        // design), so drain it with `for_each`; then resolve arena spans with the offset lines
        // prefetched ahead, and compare with the data lines prefetched ahead.
        // MINIMAL=false: raw slots, so the stream never touches the remap (see `slot_for` — the
        // remap is unchecked in ptr_hash and only safe up to `overflow_cap`). Raw slots ≥ n are
        // triaged here: past the cap they are provably non-members, otherwise the (rare, ~1%)
        // per-key `index()` resolves the remapped slot.
        let m = self.m();
        let slots = crate::hash::triage_slots(mph, m, self.overflow_cap, &hashes);
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(slots.len());
        for (i, &slot) in slots.iter().enumerate() {
            if let Some(&s) = slots.get(i + AHEAD) {
                if s < m {
                    self.arena.prefetch_offsets(s);
                }
            }
            spans.push(if slot < m {
                self.arena.span(slot)
            } else {
                None
            });
        }
        (0..keys.len())
            .map(|i| {
                if let Some(Some(sp)) = spans.get(i + AHEAD / 2) {
                    self.arena.prefetch_span(*sp);
                }
                if let Some(next) = keys.get(i + AHEAD / 2) {
                    crate::blob::prefetch_byte(next.as_ref().as_bytes(), 0);
                }
                let hit = spans[i].and_then(|sp| {
                    (self.arena.str_at(sp) == Some(keys[i].as_ref())).then_some(slots[i] as u32)
                });
                if hit.is_none() && !self.side.is_empty() {
                    return self.side_lookup(hashes[i], keys[i].as_ref());
                }
                hit
            })
            .collect()
    }

    /// Dense id of `key` **without** verifying membership: `key` MUST be one of the built keys, or the
    /// result is an arbitrary (but valid) slot in `[0, n)`. Skips the stored-key comparison that [`id`]
    /// does, so it is the fastest possible lookup — use it for a **fixed/closed vocabulary** (the
    /// canonical hot-path use of a perfect hash), where membership is already guaranteed. Returns `0`
    /// for an empty dictionary, and for a non-member whose slot falls past the MPH's remap (which is
    /// bounded rather than read unchecked — being unsafe on a wrong key is not one of the trade-offs
    /// this method makes). In the rare index that contains a 64-bit hash collision, keys sharing the
    /// collided hash resolve through the side table (which does compare stored keys — correctness for
    /// members is kept even there); every other index skips that with one predictable branch.
    ///
    /// [`id`]: PerfectHashIndex::id
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        let h = hash_key(key);
        if !self.side.is_empty() {
            if let Some(id) = self.side_lookup(h, key) {
                return id;
            }
        }
        self.slot_for(h).unwrap_or(0) as u32
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Key for `id`, or `None` if out of range.
    pub fn key(&self, id: u32) -> Option<&str> {
        self.arena.get(id as usize)
    }

    /// Serialised header + owned sections (the arena is borrowed separately): shared by
    /// [`to_bytes`](Self::to_bytes) and the streaming [`save`](Self::save) so the two emit
    /// byte-identical blobs.
    fn serialised_parts(&self) -> Result<SerialisedParts, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let mut side_buf = Vec::with_capacity(self.side.len() * SIDE_ENTRY);
        for &(h, id) in &self.side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut payload = crate::hash::BlockHasher::new();
        payload.update(&mph_buf);
        payload.update(self.arena.as_bytes());
        payload.update(&side_buf);
        let header = header_bytes(self.n, mph_buf.len(), self.side.len(), payload.finish());
        Ok((header, mph_buf, side_buf))
    }

    /// Serialise to a self-describing blob: `[magic "BMP4"][n u64][mph_len u64][side_len u32]
    /// [payload u64][check u32][mph epserde bytes][arena bytes][side entries]`. The MPH is
    /// serialised with [`epserde`]; reloading queries correctly because the key hash is
    /// version-stable. `check` is a hash of the preceding header bytes and `payload` a streaming
    /// hash of everything after the header, so a blob that lost bytes in transit fails cleanly at
    /// load. (`overflow_cap` is not stored: it is recomputed from the arena on every load.)
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        let arena = self.arena.as_bytes();
        let mut out = Vec::with_capacity(HEADER_V4 + mph_buf.len() + arena.len() + side_buf.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(arena);
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
        Ok(HEADER_V4 + mph + self.arena.as_bytes().len() + self.side.len() * SIDE_ENTRY)
    }

    /// Reconstruct from [`PerfectHashIndex::to_bytes`] output. The lexindex framing (magic, lengths,
    /// arena offsets, side table) is fully bounds-validated, and owned loads verify a streaming
    /// checksum of the whole payload, so *accidental* corruption anywhere in the blob fails cleanly.
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
    /// the MPH alone), lengths, the side table and the arena — with the MPH region located but
    /// **not** deserialised. Safe on arbitrary bytes: this is the half a property test fuzzes, and
    /// everything `from_shared` trusts comes out of here.
    /// Whether the framing of `bytes` parses. Exists for the libFuzzer target in `fuzz/`, which
    /// lives in its own crate and so cannot reach `parse_frame` (private, and returning a private
    /// type). See the `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_parse_frame(bytes: &[u8], verify: bool) -> bool {
        Self::parse_frame(&SharedBytes::from_owned(bytes.to_vec()), verify).is_ok()
    }

    fn parse_frame(blob: &SharedBytes, verify: bool) -> Result<Frame, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < 4 {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let (header, n_at, len_at) = match &bytes[0..4] {
            m if m == MAGIC_V2 => (HEADER_V2, 4, 12),
            m if m == MAGIC_V3 => (HEADER_V3, 4, 20),
            m if m == MAGIC_V4 => (HEADER_V4, 4, 12),
            _ => return Err(IndexError::Format("bad magic or truncated header")),
        };
        if bytes.len() < header {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        // v3/v4 headers carry a checksum over their framing fields; verify it so accidental
        // corruption of those fails cleanly. The v3 `overflow_cap` field is deliberately *not*
        // read — the cap is recomputed from the arena below, so a wrong (even maliciously
        // re-checksummed) cap cannot steer a query past the remap.
        let (checked, side_len) = match &bytes[0..4] {
            m if m == MAGIC_V3 => (Some(CHECKED_V3), 0usize),
            m if m == MAGIC_V4 => (
                Some(CHECKED_V4),
                u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize,
            ),
            _ => (None, 0),
        };
        if let Some(c) = checked {
            let check = u32::from_le_bytes(bytes[c..c + 4].try_into().unwrap());
            if check != crate::hash::hash_bytes(&bytes[..c]) as u32 {
                return Err(IndexError::Format("header checksum mismatch"));
            }
        }
        // Owned v4 loads verify the whole payload — one streaming pass over everything after the
        // header — so a flipped byte in the MPH region, the arena or the side table is rejected
        // here rather than surfacing as a wrong answer (or an epserde abort) later.
        if verify && &bytes[0..4] == MAGIC_V4 {
            let stored = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
            if stored != crate::hash::hash_block(&bytes[HEADER_V4..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
        }
        let n64 = u64::from_le_bytes(bytes[n_at..n_at + 8].try_into().unwrap());
        if n64 > u32::MAX as u64 {
            return Err(IndexError::Format(
                "perfect-hash: header claims more than u32::MAX keys",
            ));
        }
        let n = n64 as usize;
        if side_len > n || (side_len == n && n > 0) {
            return Err(IndexError::Format("side table length out of range"));
        }
        let m = n - side_len;
        // `mph_len` and the side-byte count are header-supplied; convert and multiply checked so a
        // fabricated length fails cleanly on every target width instead of truncating or wrapping
        // on a 32-bit one.
        let mph_len = usize::try_from(u64::from_le_bytes(
            bytes[len_at..len_at + 8].try_into().unwrap(),
        ))
        .map_err(|_| IndexError::Format("mph length out of range"))?;
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
        let mut side: Vec<(u64, u32)> = bytes[side_start..]
            .chunks_exact(SIDE_ENTRY)
            .map(|e| {
                (
                    u64::from_le_bytes(e[0..8].try_into().unwrap()),
                    u32::from_le_bytes(e[8..12].try_into().unwrap()),
                )
            })
            .collect();
        side.sort_unstable(); // restore the binary-search invariant regardless of the blob
        // Side ids must be exactly the tail range [m, n) — the arena rows the MPH does not cover.
        // Checked structurally, not via the checksums: those only vouch for transport, and a wrong
        // id here would alias two keys onto one row or dangle past the arena.
        let mut ids: Vec<u32> = side.iter().map(|e| e.1).collect();
        ids.sort_unstable();
        if !ids.iter().copied().eq(m as u32..n as u32) {
            return Err(IndexError::Format(
                "perfect-hash: side-table ids are not the tail id range",
            ));
        }
        let arena = StringArena::from_shared(
            blob.subslice(mph_end, side_start)
                .ok_or(IndexError::Format("arena range out of range"))?,
        )?;
        if arena.len() != n {
            return Err(IndexError::Format("mph / arena length mismatch"));
        }
        Ok(Frame {
            n,
            mph: header..mph_end,
            arena,
            side,
        })
    }

    /// Reconstruct from a shared byte source: the validated framing from
    /// [`parse_frame`](Self::parse_frame), then the MPH structure (a few bytes/key) deserialised
    /// by `epserde` into owned memory; the key arena — the bulk of the blob — is borrowed zero-copy,
    /// so a memory-mapped load never copies it. Backs `from_bytes`, `load` and `load_mmap`.
    ///
    /// # Safety
    /// The MPH region must be an `epserde` image this crate wrote — see
    /// [`from_bytes`](Self::from_bytes): `ptr_hash` reads its pilot table unchecked, so a crafted
    /// region is undefined behaviour and nothing here can reject it. The framing checks that run
    /// first turn every *accidental* corruption into an error.
    unsafe fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let Frame {
            n,
            mph,
            arena,
            side,
        } = Self::parse_frame(&blob, verify)?;
        let m = n - side.len();
        let mph = if n == 0 {
            None
        } else {
            let mut reader = &blob.as_ref()[mph];
            // A safe fn in epserde 0.8 that is unsound for a crafted region: the caller's contract
            // is what makes this call sound.
            let mph = DefaultPtrHash::deserialize_full(&mut reader)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
            if mph.n() != m {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        // The remap bound is always recomputed from the stored keys, never taken from a header:
        // the arena is bounds-validated, so hashing the MPH's own members — ids [0, m), the
        // representatives — yields the exact bound regardless of what any header claims. (Side
        // keys are deliberately excluded: they are not MPH members, and folding their raw slots
        // in could only inflate the bound back over the remap's true end.) Chunked so the scratch
        // buffer stays flat on a huge corpus. `CompactHashIndex` cannot do this — it stores no
        // keys — which is why its cap is trusted from its checked header and its `from_bytes` is
        // a stricter trust-your-own-blob contract.
        let overflow_cap = match &mph {
            Some(mph) => {
                const CHUNK: usize = 1 << 16;
                let mut hashes = Vec::with_capacity(CHUNK.min(m));
                let mut cap = 0;
                for start in (0..m).step_by(CHUNK) {
                    hashes.clear();
                    for i in start..(start + CHUNK).min(m) {
                        let key = arena
                            .get(i)
                            .ok_or(IndexError::Format("arena slot out of range"))?;
                        hashes.push(hash_key(key));
                    }
                    cap = cap.max(crate::hash::overflow_cap(mph, &hashes, m));
                }
                cap
            }
            None => 0,
        };
        Ok(Self {
            mph,
            arena,
            n,
            overflow_cap,
            side,
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
            w.write_all(self.arena.as_bytes())?;
            w.write_all(&side_buf)?;
            Ok(())
        })
    }

    /// Build straight to `path` **without ever holding the keys**, for a corpus that does not fit
    /// in memory. Returns the number of keys written; the file is byte-identical to what
    /// [`build`](Self::build) + [`save`](Self::save) would have produced for the same key set.
    ///
    /// `source` is a *factory*, not an iterator, and it is called **twice**. That is the shape the
    /// problem has, not an inconvenience: the arena stores keys in slot order, slot order is only
    /// known once the perfect hash is built, and the perfect hash needs every key's hash first.
    /// Pass one hashes the corpus and records lengths (12 bytes per key, nothing else); the offset
    /// table follows from the lengths alone; pass two replays the corpus and writes each key
    /// straight into its place in the mapped file. A one-shot iterator cannot be passed by
    /// construction, which is the point — the signature states the requirement instead of
    /// documenting it.
    ///
    /// **Keys must be distinct.** [`build`](Self::build) sorts and deduplicates, which this cannot
    /// do: a repeated hash in pass one is either a duplicate key or a genuine 64-bit collision, and
    /// which one it is changes `n`, the tail ids and therefore every offset already computed — a
    /// third pass. With distinct keys required, a repeat is by definition a collision, and a
    /// duplicate is caught (by comparing the two written entries) before the file is published.
    ///
    /// Measured on the same key set both ways (`examples/peak.rs`, real-word pairs): the whole
    /// process peaks at **471 MB at 10 M keys against 1 272 MB** for `build` handed a list of the
    /// same keys, and 87 against 146 at 1 M. Of the streamed build's 44.1 bytes per key, 23.5 are
    /// the output file itself — the arena is written through a mapping, so its pages are resident
    /// until the kernel writes them back (reclaimable page cache, not anonymous memory). The
    /// anonymous part is 20.6 bytes per key, flat in `n`, which is what the design predicts: eight
    /// for the hash, four for the length, eight for the sorted copy that looks for collisions.
    #[cfg(feature = "mmap")]
    pub fn build_to_file<F, I, S>(
        path: impl AsRef<std::path::Path>,
        mut source: F,
    ) -> Result<usize, IndexError>
    where
        F: FnMut() -> I,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut hashes: Vec<u64> = Vec::new();
        let mut lens: Vec<u32> = Vec::new();
        for item in source() {
            let key = item.as_ref();
            lens.push(u32::try_from(key.len()).map_err(|_| {
                IndexError::Format("perfect-hash: a key is longer than u32::MAX bytes")
            })?);
            hashes.push(hash_key(key));
        }
        let n = hashes.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "perfect-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        if n == 0 {
            return Self::build(Vec::<&str>::new())?.save(path).map(|()| 0);
        }
        // Same shape as `build`: a sorted copy answers whether any two keys share a hash and, when
        // none does, is what the MPH is built from. The `(hash, index)` partition is 16 bytes per
        // key and is only paid for when a collision actually exists.
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        let collided = sorted.windows(2).any(|w| w[0] == w[1]);
        let (mph_hashes, extras) = if collided {
            drop(sorted);
            crate::hash::split_collisions(&hashes)
        } else {
            (sorted, Vec::new())
        };
        let m = mph_hashes.len();
        let mph = crate::hash::build_mph(&mph_hashes)?;

        // Where each *input* key goes: its MPH slot, or a tail id for the rare extra. Both
        // side tables below are allocated only when a collision actually exists, which needs
        // n ≈ 10^8 to reach a probability of 1e-4.
        let mut is_extra = Vec::new();
        let mut collided_hashes = std::collections::HashSet::new();
        if collided {
            is_extra = vec![false; n];
            for &(h, i) in &extras {
                is_extra[i as usize] = true;
                collided_hashes.insert(h);
            }
        }
        let mut slot_of: Vec<u32> = vec![NO_KEY; n];
        let mut taken = vec![false; m];
        let mut rep_of: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
        for (i, h) in hashes.iter().enumerate() {
            if collided && is_extra[i] {
                continue;
            }
            let slot = mph.index(h);
            if slot >= m || taken[slot] {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            taken[slot] = true;
            slot_of[i] = slot as u32;
            if collided_hashes.contains(h) {
                rep_of.insert(*h, i as u32);
            }
        }
        for (j, &(_, i)) in extras.iter().enumerate() {
            slot_of[i as usize] = (m + j) as u32;
        }
        drop(taken);
        drop(is_extra);
        drop(mph_hashes);

        let mut len_by_slot = vec![0u32; n];
        for (i, &slot) in slot_of.iter().enumerate() {
            len_by_slot[slot as usize] = lens[i];
        }
        drop(lens);
        let (arena_prefix, data_len, width) = StringArena::prefix_for_lengths(&len_by_slot);
        drop(len_by_slot);

        let mut side: Vec<(u64, u32)> = extras
            .iter()
            .enumerate()
            .map(|(j, &(h, _))| (h, (m + j) as u32))
            .collect();
        side.sort_unstable();
        let mut side_buf = Vec::with_capacity(side.len() * SIDE_ENTRY);
        for &(h, id) in &side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut mph_buf = Vec::new();
        mph.serialize(&mut mph_buf)
            .map_err(|e| IndexError::Serde(e.to_string()))?;
        drop(mph);

        let arena_start = HEADER_V4 + mph_buf.len();
        let data_start = arena_start + arena_prefix.len();
        let total = data_start + data_len + side_buf.len();
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            let file: &mut std::fs::File = w.get_mut();
            file.set_len(total as u64)?;
            // SAFETY: the file was created exclusively by `write_atomically_with` under a name no
            // other process knows yet, and nothing else touches it until the rename below.
            let mut map = unsafe { memmap2::MmapMut::map_mut(&*file)? };
            map[HEADER_V4..arena_start].copy_from_slice(&mph_buf);
            map[arena_start..data_start].copy_from_slice(&arena_prefix);
            map[data_start + data_len..].copy_from_slice(&side_buf);

            let mut i = 0usize;
            for item in source() {
                let key = item.as_ref();
                if i >= n || hash_key(key) != hashes[i] {
                    return Err(IndexError::Build(
                        "perfect-hash: the source did not replay the same keys in the same order",
                    ));
                }
                let at = data_start
                    + StringArena::offset_at(&arena_prefix, width, slot_of[i] as usize) as usize;
                map[at..at + key.len()].copy_from_slice(key.as_bytes());
                i += 1;
            }
            if i != n {
                return Err(IndexError::Build(
                    "perfect-hash: the source did not replay the same keys in the same order",
                ));
            }
            // Every extra shares a hash with its representative. Distinct keys make that a genuine
            // 64-bit collision, which the side table handles; equal keys mean the caller broke the
            // one precondition this build has, and the file must not be published.
            let span = |slot: usize| {
                let lo = data_start + StringArena::offset_at(&arena_prefix, width, slot) as usize;
                let hi =
                    data_start + StringArena::offset_at(&arena_prefix, width, slot + 1) as usize;
                lo..hi
            };
            for &(h, i) in &extras {
                let rep = rep_of[&h] as usize;
                if map[span(slot_of[i as usize] as usize)] == map[span(slot_of[rep] as usize)] {
                    return Err(IndexError::Build(
                        "perfect-hash: build_to_file needs distinct keys and the source repeated one",
                    ));
                }
            }

            let mut payload = crate::hash::BlockHasher::new();
            payload.update(&map[HEADER_V4..]);
            let header = header_bytes(n, mph_buf.len(), side.len(), payload.finish());
            map[..HEADER_V4].copy_from_slice(&header);
            map.flush()?;
            Ok(())
        })?;
        Ok(n)
    }

    /// Load a dictionary previously written with [`PerfectHashIndex::save`] (reads the whole file
    /// and verifies the payload checksum).
    ///
    /// # Safety
    /// The file must have been written by [`save`](Self::save) — see
    /// [`from_bytes`](Self::from_bytes) for why a crafted blob cannot be rejected.
    pub unsafe fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        // SAFETY: forwarded from this function's contract.
        unsafe { Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true) }
    }

    /// Memory-map the file and borrow the key arena (the bulk of the blob) zero-copy; only the small
    /// MPH structure is read into memory. Skips the payload-checksum scan `load` performs — the
    /// mapped file is trusted intact.
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

#[cfg(all(test, feature = "mmap"))]
mod stream_build_tests {
    use super::*;

    /// A directory of this test's own. Shared temp names would be enough for the files themselves,
    /// but one test below asserts that *no* `.tmp` is left behind, and the tests run in parallel:
    /// scanning a shared directory would see a sibling's temporary and fail at random.
    fn tmp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lexindex_bmpstream_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("index.bmp")
    }

    #[test]
    fn build_to_file_builds_an_index_indistinguishable_from_the_in_memory_one() {
        // Keys deliberately of different lengths and not in sorted order: the streaming build has
        // to place them by slot, which is neither input nor sorted order.
        let keys: Vec<String> = (0..2_000)
            .map(|i| format!("key-{i}-{}", "x".repeat(i % 17)))
            .collect();
        let path = tmp("same.bmp");
        let n = PerfectHashIndex::build_to_file(&path, || keys.iter()).unwrap();
        assert_eq!(n, keys.len());

        // Not compared byte for byte against `build` + `save`, because that is not a property
        // either of them has: `ptr_hash` construction is not deterministic, and two in-memory
        // builds of the same key set already differ in their serialised pilots. What must hold is
        // that the file is a valid blob answering exactly like the index built in memory.
        // SAFETY: written by this crate a line above.
        let idx = unsafe { PerfectHashIndex::load(&path) }.unwrap();
        assert_eq!(idx.len(), keys.len());
        let mut ids: Vec<u32> = Vec::with_capacity(keys.len());
        for k in &keys {
            let id = idx.id(k).expect("every key is a member");
            assert_eq!(idx.key(id), Some(k.as_str()));
            ids.push(id);
        }
        ids.sort_unstable();
        assert!(
            ids.iter().copied().eq(0..keys.len() as u32),
            "ids are exactly the dense range [0, n)"
        );
        assert_eq!(idx.id("not-a-key"), None);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn build_to_file_matches_the_in_memory_index_key_for_key() {
        let keys: Vec<String> = (0..500).map(|i| format!("k{i:04}")).collect();
        let path = tmp("cross.bmp");
        PerfectHashIndex::build_to_file(&path, || keys.iter()).unwrap();
        // SAFETY: written by this crate a line above.
        let file = unsafe { PerfectHashIndex::load(&path) }.unwrap();
        let mem = PerfectHashIndex::build(keys.iter()).unwrap();
        // The *ids* may differ (see above), but the round trip and the membership answers may not.
        for k in &keys {
            assert!(file.contains(k) && mem.contains(k));
            assert_eq!(file.key(file.id(k).unwrap()), Some(k.as_str()));
        }
        for miss in ["", "k9999", "K0000", "k0000 "] {
            assert_eq!(file.id(miss), None, "{miss}");
            assert_eq!(mem.id(miss), None, "{miss}");
        }
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn build_to_file_handles_an_empty_source() {
        let path = tmp("empty.bmp");
        assert_eq!(
            PerfectHashIndex::build_to_file(&path, Vec::<&str>::new).unwrap(),
            0
        );
        // SAFETY: written by this crate a line above.
        assert_eq!(unsafe { PerfectHashIndex::load(&path) }.unwrap().len(), 0);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn build_to_file_refuses_a_duplicate_key_and_leaves_the_target_alone() {
        let path = tmp("dupe.bmp");
        PerfectHashIndex::build_to_file(&path, || ["a", "b"]).unwrap();
        let before = std::fs::read(&path).unwrap();

        let err = PerfectHashIndex::build_to_file(&path, || ["x", "y", "x"]).unwrap_err();
        assert!(
            format!("{err}").contains("distinct keys"),
            "unexpected error: {err}"
        );
        // The precondition is caught before the rename, so the previous index survives intact and
        // no temporary is left behind.
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|f| f.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temporaries left behind: {leftovers:?}"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn build_to_file_refuses_a_source_that_does_not_replay() {
        let path = tmp("drift.bmp");
        let mut round = 0;
        let err = PerfectHashIndex::build_to_file(&path, || {
            round += 1;
            if round == 1 {
                vec!["a", "b", "c"]
            } else {
                vec!["a", "different", "c"]
            }
        })
        .unwrap_err();
        assert!(
            format!("{err}").contains("replay"),
            "unexpected error: {err}"
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn build_to_file_refuses_a_source_that_replays_short() {
        let path = tmp("short.bmp");
        let mut round = 0;
        let err = PerfectHashIndex::build_to_file(&path, || {
            round += 1;
            if round == 1 {
                vec!["a", "b", "c"]
            } else {
                vec!["a", "b"]
            }
        })
        .unwrap_err();
        assert!(
            format!("{err}").contains("replay"),
            "unexpected error: {err}"
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loader is `unsafe` because a crafted blob cannot be rejected (see
    /// [`PerfectHashIndex::from_bytes`]). Every blob below is either produced by this crate or a
    /// deliberate corruption of the *validated* framing, which the loader rejects before the MPH is
    /// touched.
    fn from_bytes(bytes: &[u8]) -> Result<PerfectHashIndex, IndexError> {
        unsafe { PerfectHashIndex::from_bytes(bytes) }
    }

    /// The safe half of the loader must never panic on arbitrary bytes — only `Ok`/`Err` — which
    /// is where the "garbage fails cleanly" property lives now that the loaders are `unsafe`.
    #[test]
    fn parse_frame_never_panics() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(&prop::collection::vec(any::<u8>(), 0..256), |data| {
                let _ = PerfectHashIndex::parse_frame(&SharedBytes::from_owned(data), true);
                Ok(())
            })
            .unwrap();
    }

    /// Every truncation of a real blob is rejected by the framing alone, with or without the
    /// payload checksum — so the epserde region is never reached on a short read.
    #[test]
    fn parse_frame_rejects_every_truncation() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma"]).unwrap();
        let blob = idx.to_bytes().unwrap();
        for verify in [true, false] {
            for k in 0..blob.len() {
                let cut = SharedBytes::from_owned(blob[..k].to_vec());
                assert!(
                    PerfectHashIndex::parse_frame(&cut, verify).is_err(),
                    "truncated to {k} bytes (verify={verify}) parsed"
                );
            }
            assert!(
                PerfectHashIndex::parse_frame(&SharedBytes::from_owned(blob.clone()), verify)
                    .is_ok()
            );
        }
    }

    #[test]
    fn serialized_len_matches_to_bytes() {
        for keys in [vec![], vec!["alpha"], vec!["alpha", "beta", "gamma"]] {
            let idx = PerfectHashIndex::build(&keys).unwrap();
            assert_eq!(idx.serialized_len().unwrap(), idx.to_bytes().unwrap().len());
        }
    }

    #[test]
    fn forward_reverse_and_membership() {
        let words = ["alpha", "beta", "gamma", "delta", "alpha"];
        let idx = PerfectHashIndex::build(words).unwrap();
        assert_eq!(idx.len(), 4); // deduped
        assert!(!idx.is_empty());
        for w in ["alpha", "beta", "gamma", "delta"] {
            let id = idx.id(w).expect("present");
            assert!((id as usize) < idx.len());
            assert_eq!(idx.key(id), Some(w)); // round-trips through the slot
            assert!(idx.contains(w));
        }
        assert_eq!(idx.id("epsilon"), None); // absent → verified miss
        assert!(!idx.contains("epsilon"));
        assert_eq!(idx.key(99), None);
    }

    #[test]
    fn id_unchecked_matches_id_for_members() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma", "delta"]).unwrap();
        for w in ["alpha", "beta", "gamma", "delta"] {
            assert_eq!(idx.id_unchecked(w), idx.id(w).unwrap()); // same slot, no verification
        }
        let empty = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        assert_eq!(empty.id_unchecked("x"), 0); // empty dictionary → 0
    }

    #[test]
    fn ids_are_a_dense_permutation() {
        let words: Vec<String> = (0..500).map(|i| format!("key_{i:04}")).collect();
        let idx = PerfectHashIndex::build(&words).unwrap();
        let mut ids: Vec<u32> = words.iter().map(|w| idx.id(w).unwrap()).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..500).collect::<Vec<u32>>()); // exactly 0..n, no gaps or repeats
    }

    #[test]
    fn empty_dictionary() {
        let idx = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(idx.id("x"), None);
        assert_eq!(idx.key(0), None);
    }

    #[test]
    fn round_trips_through_bytes() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma", "delta"]).unwrap();
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.len(), idx.len());
        for w in ["alpha", "beta", "gamma", "delta"] {
            // the serialised MPH yields the same slot, and reverse lookup matches
            assert_eq!(restored.id(w), idx.id(w));
            assert_eq!(restored.key(idx.id(w).unwrap()), Some(w));
        }
        assert_eq!(restored.id("zeta"), None); // verified membership survives the round-trip
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = PerfectHashIndex::build(["GET", "POST", "PUT", "DELETE"]).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_mph_{}.bmp", std::process::id()));
        idx.save(&path).unwrap();
        let loaded = unsafe { PerfectHashIndex::load(&path) }.unwrap();
        for w in ["GET", "POST", "PUT", "DELETE"] {
            assert_eq!(loaded.id(w), idx.id(w));
        }
        std::fs::remove_file(&path).ok();
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_matches_owned_load() {
        let words: Vec<String> = (0..64).map(|i| format!("token_{i:03}")).collect();
        let idx = PerfectHashIndex::build(&words).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_mph_mmap_{}.bmp", std::process::id()));
        idx.save(&path).unwrap();
        let mapped = unsafe { PerfectHashIndex::load_mmap(&path) }.unwrap();
        assert_eq!(mapped.len(), idx.len());
        for w in &words {
            let id = mapped.id(w).expect("present"); // membership checks against the mapped arena
            assert_eq!(mapped.key(id), Some(w.as_str()));
        }
        assert!(!mapped.contains("token_999")); // verified miss survives the mmap load
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_round_trips_and_rejects_corrupt() {
        let empty = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        let restored = from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.id("x"), None);

        assert!(from_bytes(b"nope").is_err());
        let mut good = PerfectHashIndex::build(["a", "b"])
            .unwrap()
            .to_bytes()
            .unwrap();
        good[0] = b'X'; // break the magic
        assert!(from_bytes(&good).is_err());
    }

    /// The streamed batch is the same function as the singular accessor, element for element —
    /// including misses, which exercise both the slot bound and the stored-key comparison.
    #[test]
    fn batch_matches_singular_including_misses() {
        let keys: Vec<String> = (0..3_000).map(|i| format!("k{i}")).collect();
        let idx = PerfectHashIndex::build(&keys).unwrap();
        let probes: Vec<String> = keys
            .iter()
            .cloned()
            .chain((0..500).map(|i| format!("miss{i}")))
            .collect();
        let batch = idx.ids_of(&probes);
        for (p, b) in probes.iter().zip(&batch) {
            assert_eq!(idx.id(p), *b);
        }
        assert!(batch[keys.len()..].iter().all(Option::is_none));
    }

    /// A 0.7 "BMP3" blob (header keeps a now-ignored `overflow_cap` field) still loads: the bound
    /// is recomputed from the arena, and every lookup matches the source index.
    #[test]
    fn a_0_7_bmp3_blob_still_loads() {
        let words: Vec<String> = (0..2_000).map(|i| format!("word-{i:05}")).collect();
        let idx = PerfectHashIndex::build(&words).unwrap();
        let v3 = synthesize_v3(&idx, idx.overflow_cap);
        let restored = from_bytes(&v3).unwrap();
        assert_eq!(restored.overflow_cap, idx.overflow_cap);
        for w in &words {
            assert_eq!(restored.id(w), idx.id(w));
        }
        assert_eq!(restored.id("delta"), None);
    }

    /// A 0.5/0.6 "BMP2" blob predates the recorded remap bound, but stores every key — so the load
    /// path recomputes the bound instead of falling back to the unbounded (unsound) behaviour. The
    /// healed value must equal what a fresh build records.
    #[test]
    fn a_0_6_bmp2_blob_is_healed_on_load() {
        // Both sides of the heal's chunk boundary (64 Ki keys): one pass and several.
        for n in [2_000usize, 70_000] {
            let words: Vec<String> = (0..n).map(|i| format!("word-{i:05}")).collect();
            let idx = PerfectHashIndex::build(&words).unwrap();
            let v4 = idx.to_bytes().unwrap();
            assert_eq!(&v4[0..4], b"BMP4");
            assert!(idx.side.is_empty(), "sequential keys must not collide");
            // A 0.5/0.6 blob is `[magic][n][mph_len]` + the same mph and arena bytes.
            let mut v2 = Vec::with_capacity(v4.len() - HEADER_V4 + HEADER_V2);
            v2.extend_from_slice(b"BMP2");
            v2.extend_from_slice(&v4[4..12]); // n
            v2.extend_from_slice(&v4[12..20]); // mph_len
            v2.extend_from_slice(&v4[HEADER_V4..]); // mph + arena (side is empty)
            let restored = from_bytes(&v2).unwrap();
            assert_eq!(
                restored.overflow_cap, idx.overflow_cap,
                "healed cap at n = {n}"
            );
            for w in &words {
                assert_eq!(restored.id(w), idx.id(w));
                assert_eq!(restored.key(restored.id(w).unwrap()), Some(w.as_str()));
            }
            assert_eq!(restored.id("delta"), None);
        }
    }

    /// Rebuild a 0.7 "BMP3" blob from a (side-free) v4 index, with the cap field set to `cap`
    /// and a valid header checksum.
    fn synthesize_v3(idx: &PerfectHashIndex, cap: u64) -> Vec<u8> {
        let v4 = idx.to_bytes().unwrap();
        assert_eq!(&v4[0..4], b"BMP4");
        assert!(idx.side.is_empty());
        let mut v3 = Vec::with_capacity(v4.len() - HEADER_V4 + HEADER_V3);
        v3.extend_from_slice(b"BMP3");
        v3.extend_from_slice(&v4[4..12]); // n
        v3.extend_from_slice(&cap.to_le_bytes());
        v3.extend_from_slice(&v4[12..20]); // mph_len
        let check = crate::hash::hash_bytes(&v3[..CHECKED_V3]) as u32;
        v3.extend_from_slice(&check.to_le_bytes());
        v3.extend_from_slice(&v4[HEADER_V4..]); // mph + arena (side is empty)
        v3
    }

    /// A header that lost bytes in transit must be refused rather than used to frame sections,
    /// and — new in v4 — so must a flipped byte anywhere in the payload (MPH region, arena or
    /// side table), caught by the whole-payload checksum on owned loads.
    #[test]
    fn corrupt_headers_and_payloads_are_refused() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma"]).unwrap();
        let good = idx.to_bytes().unwrap();
        assert!(from_bytes(&good).is_ok());
        for pos in [4, 12, 19, 20, 27, 31, 32, 35] {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "header byte {pos} was accepted");
        }
        for pos in (HEADER_V4..good.len()).step_by(7) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "payload byte {pos} was accepted");
        }
    }

    /// `id_unchecked` skips the stored-key comparison, not the remap bound: a stranger past the
    /// remap must still return a valid slot rather than reading out of bounds.
    #[test]
    fn id_unchecked_is_bounded_for_strangers() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        for round in 0..60 {
            let idx = PerfectHashIndex::build(&members).unwrap();
            for probe in 0..2_000 {
                let s = format!("stranger-{round}-{probe}");
                assert!((idx.id_unchecked(&s) as usize) < idx.len());
            }
        }
    }

    /// Same regression as `compact_hash::strangers_past_the_remap_are_rejected_not_ub`, for the
    /// verified-membership index.
    #[test]
    fn strangers_past_the_remap_are_rejected_not_ub() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let mut engaged = 0u32;
        for round in 0..300 {
            let idx = PerfectHashIndex::build(&members).unwrap();
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

    /// A real 64-bit hash collision (the pinned pair from `crate::hash`) must build, keep ids a
    /// bijection, answer exactly for both keys on every query path, and survive serde — the whole
    /// point of the side table.
    #[test]
    fn colliding_keys_build_and_resolve_exactly() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys: Vec<String> = (0..500).map(|i| format!("filler-{i:03}")).collect();
        keys.push(a.to_string());
        keys.push(b.to_string());
        let idx = PerfectHashIndex::build(&keys).unwrap();
        assert_eq!(idx.len(), 502);
        assert_eq!(idx.side.len(), 1);
        let (ia, ib) = (idx.id(a).unwrap(), idx.id(b).unwrap());
        assert_ne!(ia, ib);
        assert_eq!(idx.key(ia), Some(a)); // exact reverse for both, tail id included
        assert_eq!(idx.key(ib), Some(b));
        assert_eq!(idx.id_unchecked(a), ia); // members stay correct even for the collided hash
        assert_eq!(idx.id_unchecked(b), ib);
        assert_eq!(
            idx.ids_of(&[a, b, "not-there"]),
            vec![Some(ia), Some(ib), None]
        );
        let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k).unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            keys.len(),
            "ids must stay a bijection onto [0, n)"
        );
        // The v4 blob carries the side section and reloads identically — owned and mapped.
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.side, idx.side);
        assert_eq!(restored.id(a), Some(ia));
        assert_eq!(restored.id(b), Some(ib));
        assert_eq!(restored.key(ib), Some(b));
        #[cfg(feature = "mmap")]
        {
            let path =
                std::env::temp_dir().join(format!("lexindex_side_{}.bmp", std::process::id()));
            idx.save(&path).unwrap();
            let mapped = unsafe { PerfectHashIndex::load_mmap(&path) }.unwrap();
            assert_eq!(mapped.id(a), Some(ia));
            assert_eq!(mapped.id(b), Some(ib));
            std::fs::remove_file(&path).ok();
        }
    }

    /// Side ids index the arena's tail rows, so the loader pins them to exactly [m, n)
    /// structurally — the checksums vouch for transport, not for what was written. A blob with a
    /// re-checksummed out-of-range or duplicate id is refused, never served.
    #[test]
    fn tampered_side_ids_are_refused_even_with_valid_checksums() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = PerfectHashIndex::build([a, b, "filler"]).unwrap();
        assert_eq!(idx.side.len(), 1);
        let good = idx.to_bytes().unwrap();
        // The lone side entry's id lives in the blob's last 4 bytes.
        for bad_id in [0u32, 1, 3, u32::MAX] {
            let mut bad = good.clone();
            let at = bad.len() - 4;
            bad[at..].copy_from_slice(&bad_id.to_le_bytes());
            let payload = crate::hash::hash_block(&bad[HEADER_V4..]);
            bad[24..32].copy_from_slice(&payload.to_le_bytes());
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
        let idx = PerfectHashIndex::build(&words).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_stream_{}.bmp", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes().unwrap());
        std::fs::remove_file(&path).ok();
    }

    /// A forged v3 `overflow_cap` — set to `u64::MAX` with the header checksum re-computed so the
    /// loader accepts it — must not re-open the out-of-bounds remap read: the cap is recomputed
    /// from the arena on load (v4 does not even store one), so the forged value is discarded and
    /// strangers past the *true* remap are still rejected.
    #[test]
    fn a_tampered_cap_cannot_steer_a_query_past_the_remap() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let mut engaged = 0u32;
        for round in 0..300 {
            let idx = PerfectHashIndex::build(&members).unwrap();
            if idx.overflow_cap == 0 {
                continue; // no trailing free zone this build; nothing to forge past
            }
            let restored = from_bytes(&synthesize_v3(&idx, u64::MAX)).unwrap();
            assert_eq!(restored.overflow_cap, idx.overflow_cap); // recomputed, not the forged MAX
            let mph = restored.mph.as_ref().unwrap();
            for probe in 0..5_000 {
                let s = format!("stranger-{round}-{probe}");
                let raw = mph.index_no_remap(&crate::hash::hash_key(&s));
                if raw >= restored.m() && (raw - restored.m()) as u64 >= restored.overflow_cap {
                    assert_eq!(restored.id(&s), None);
                    assert_eq!(restored.ids_of(&[&s]), vec![None]);
                    engaged += 1;
                }
            }
            if engaged >= 20 {
                break;
            }
        }
        assert!(
            engaged > 0,
            "no trailing free zone in 300 builds - raise rounds"
        );
    }

    #[test]
    fn rejects_a_pre_0_5_blob() {
        let mut old = PerfectHashIndex::build(["a", "b"])
            .unwrap()
            .to_bytes()
            .unwrap();
        old[0..4].copy_from_slice(b"BMP1");
        assert!(from_bytes(&old).is_err());
    }
}
