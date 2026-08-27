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
//! Build fails (rather than silently corrupting) if two distinct keys collide in the 64-bit hash.
//! The hash is deterministic and unseeded (that is what makes the serialised MPH reloadable), so a
//! colliding key set can **never** build — the fix is [`crate::StringIndex`] or changing the keys,
//! not retrying. The probability is `n(n-1)/2^65`: negligible below ~10 M keys (2.7e-6 at 10 M),
//! 2.7e-4 at 100 M, and ~2.7% at 1 G.

use crate::IndexError;
use crate::arena::StringArena;
use crate::blob::SharedBytes;
use crate::hash::hash_key;
use epserde::prelude::*;
use ptr_hash::DefaultPtrHash;

const MAGIC_V2: &[u8; 4] = b"BMP2"; // header [magic 4][n u64][mph_len u64]
const MAGIC_V3: &[u8; 4] = b"BMP3"; // header [magic 4][n u64][overflow_cap u64][mph_len u64]

/// An immutable minimal-perfect-hash dictionary: fastest exact `string → dense id` with reverse lookup.
pub struct PerfectHashIndex {
    mph: Option<DefaultPtrHash>, // None iff empty (ptr_hash needs a non-empty key set)
    arena: StringArena,          // slot → key (also verifies membership)
    n: usize,
    // Length of the MPH's internal remap (see `crate::hash::overflow_cap`); `u64::MAX` for blobs
    // from versions that did not record it, meaning "unbounded" — the pre-0.7 behaviour.
    overflow_cap: u64,
}

impl PerfectHashIndex {
    /// Build from a collection of strings. Duplicates are removed; ids are arbitrary slots in `[0, n)`
    /// (no defined order — use [`crate::StringIndex`] when order matters).
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
        if n == 0 {
            return Ok(Self {
                mph: None,
                arena: StringArena::build(Vec::<&str>::new()), // offsets = [0]: a valid empty arena
                n: 0,
                overflow_cap: 0,
            });
        }
        let hashes: Vec<u64> = keys.iter().map(|k| hash_key(k.as_ref())).collect();
        let mut sorted = hashes.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(IndexError::Format(
                "perfect-hash: two keys share a 64-bit hash; the deterministic hash means this key \
                 set can never build - use StringIndex or change the keys",
            ));
        }
        let mph = crate::hash::build_mph(&hashes);
        // Slots hold borrowed keys: the arena copies them anyway, so cloning here would put a
        // second copy of the corpus alongside `keys` for the length of the loop.
        let mut by_slot: Vec<Option<&str>> = vec![None; n];
        for (k, h) in keys.iter().zip(&hashes) {
            let slot = mph.index(h);
            if slot >= n || by_slot[slot].is_some() {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = Some(k.as_ref());
        }
        let arena = StringArena::build(by_slot.into_iter().map(|o| o.unwrap()));
        let overflow_cap = crate::hash::overflow_cap(&mph, &hashes, n);
        Ok(Self {
            mph: Some(mph),
            arena,
            n,
            overflow_cap,
        })
    }

    /// Slot for a key hash, or `None` when the raw slot is past the MPH's remap — a trailing free
    /// slot no member occupies, which ptr_hash's own `index()` would read out of bounds.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        let mph = self.mph.as_ref()?;
        let raw = mph.index_no_remap(&h);
        if raw < self.n {
            Some(raw)
        } else if (raw - self.n) as u64 >= self.overflow_cap {
            None
        } else {
            Some(mph.index(&h)) // remapped; in bounds because the cap is the remap's exact length
        }
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
        let slot = self.slot_for(hash_key(key))?;
        if slot < self.n && self.arena.get(slot) == Some(key) {
            Some(slot as u32)
        } else {
            None
        }
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input (`None` where
    /// absent). Not just a loop — slot resolution streams through the MPH with 32 queries'
    /// worth of software prefetch in flight, and the arena's offset and data lines are prefetched
    /// ahead of the key comparison, so the cache misses a one-at-a-time loop pays per key overlap
    /// instead of serialising. Measured on real words: 1.5× the per-key loop at 480 k keys,
    /// 1.8× at 5 M (the gap grows once the MPH outruns the caches).
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u32>> {
        let Some(mph) = &self.mph else {
            return vec![None; keys.len()];
        };
        let hashes: Vec<u64> = keys.iter().map(|k| hash_key(k.as_ref())).collect();
        // ptr_hash's stream iterator is internal-iteration only (`next()` is unimplemented by
        // design), so drain it with `for_each`; then resolve arena spans with the offset lines
        // prefetched ahead, and compare with the data lines prefetched ahead.
        // MINIMAL=false: raw slots, so the stream never touches the remap (see `slot_for` — the
        // remap is unchecked in ptr_hash and only safe up to `overflow_cap`). Raw slots ≥ n are
        // triaged here: past the cap they are provably non-members, otherwise the (rare, ~1%)
        // per-key `index()` resolves the remapped slot.
        let mut slots = Vec::with_capacity(keys.len());
        mph.index_stream::<32, false, _>(hashes.iter())
            .for_each(|s| slots.push(s));
        for (i, slot) in slots.iter_mut().enumerate() {
            if *slot >= self.n {
                *slot = if (*slot - self.n) as u64 >= self.overflow_cap {
                    usize::MAX // sentinel: definitely absent
                } else {
                    mph.index(&hashes[i])
                };
            }
        }
        const AHEAD: usize = 16;
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(slots.len());
        for (i, &slot) in slots.iter().enumerate() {
            if let Some(&s) = slots.get(i + AHEAD) {
                if s < self.n {
                    self.arena.prefetch_offsets(s);
                }
            }
            spans.push(if slot < self.n {
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
                let sp = spans[i]?;
                (self.arena.str_at(sp) == Some(keys[i].as_ref())).then_some(slots[i] as u32)
            })
            .collect()
    }

    /// Dense id of `key` **without** verifying membership: `key` MUST be one of the built keys, or the
    /// result is an arbitrary (but valid) slot in `[0, n)`. Skips the stored-key comparison that [`id`]
    /// does, so it is the fastest possible lookup — use it for a **fixed/closed vocabulary** (the
    /// canonical hot-path use of a perfect hash), where membership is already guaranteed. Returns `0`
    /// for an empty dictionary.
    ///
    /// [`id`]: PerfectHashIndex::id
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        match &self.mph {
            Some(mph) => mph.index(&hash_key(key)) as u32,
            None => 0,
        }
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Key for `id`, or `None` if out of range.
    pub fn key(&self, id: u32) -> Option<&str> {
        self.arena.get(id as usize)
    }

    /// Serialise to a self-describing blob: `[magic "BMP3"][n u64][overflow_cap u64][mph_len
    /// u64][mph epserde bytes][arena bytes]`. The MPH is serialised with [`epserde`]; reloading
    /// queries correctly because the key hash is version-stable.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut mph_buf = Vec::new();
        if let Some(mph) = &self.mph {
            mph.serialize(&mut mph_buf)
                .map_err(|e| IndexError::Serde(e.to_string()))?;
        }
        let arena_buf = self.arena.to_bytes();
        let mut out = Vec::with_capacity(28 + mph_buf.len() + arena_buf.len());
        out.extend_from_slice(MAGIC_V3);
        out.extend_from_slice(&(self.n as u64).to_le_bytes());
        out.extend_from_slice(&self.overflow_cap.to_le_bytes());
        out.extend_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(&arena_buf);
        Ok(out)
    }

    /// Reconstruct from [`PerfectHashIndex::to_bytes`] output. The lexindex framing (magic, lengths,
    /// arena offsets) is fully bounds-validated and never reads out of bounds, but the embedded minimal
    /// perfect hash is deserialised by [`epserde`]: feed only blobs produced by
    /// [`to_bytes`](Self::to_bytes) / [`save`](Self::save), since a corrupted MPH region may abort on a
    /// failed allocation — the same "trust your own blob" contract as [`load_mmap`](Self::load_mmap).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// Reconstruct from a shared byte source. The MPH structure (a few bytes/key) is deserialised into
    /// owned memory; the key arena — the bulk of the blob — is borrowed zero-copy from `blob`, so a
    /// memory-mapped load never copies it. Backs both `from_bytes` and `load_mmap`.
    fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < 20 || (&bytes[0..4] != MAGIC_V3 && &bytes[0..4] != MAGIC_V2) {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let n = u64::from_le_bytes(bytes[4..12].try_into().unwrap()) as usize;
        // A 0.5/0.6 "BMP2" blob has no recorded cap: fall back to "unbounded", the behaviour it was
        // built with. Rebuilding (not just re-saving) is what removes the unchecked-remap window.
        let (overflow_cap, header) = if &bytes[0..4] == MAGIC_V3 {
            if bytes.len() < 28 {
                return Err(IndexError::Format("bad magic or truncated header"));
            }
            (u64::from_le_bytes(bytes[12..20].try_into().unwrap()), 28)
        } else {
            (u64::MAX, 20)
        };
        let mph_len = u64::from_le_bytes(bytes[header - 8..header].try_into().unwrap()) as usize;
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
        let arena = StringArena::from_shared(
            blob.subslice(mph_end, blob.len())
                .ok_or(IndexError::Format("arena range out of range"))?,
        )?;
        if arena.len() != n {
            return Err(IndexError::Format("mph / arena length mismatch"));
        }
        Ok(Self {
            mph,
            arena,
            n,
            overflow_cap,
        })
    }

    /// Write the dictionary to `path` (see [`PerfectHashIndex::to_bytes`]).
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes()?)?;
        Ok(())
    }

    /// Load a dictionary previously written with [`PerfectHashIndex::save`] (reads the whole file).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Memory-map the file and borrow the key arena (the bulk of the blob) zero-copy; only the small
    /// MPH structure is read into memory. See [`StringIndex::load_mmap`](crate::StringIndex::load_mmap)
    /// for the immutability caveat.
    #[cfg(feature = "mmap")]
    pub fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the mapped file must not be mutated while it is mapped (see StringIndex::load_mmap).
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let restored = PerfectHashIndex::from_bytes(&idx.to_bytes().unwrap()).unwrap();
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
        let loaded = PerfectHashIndex::load(&path).unwrap();
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
        let mapped = PerfectHashIndex::load_mmap(&path).unwrap();
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
        let restored = PerfectHashIndex::from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.id("x"), None);

        assert!(PerfectHashIndex::from_bytes(b"nope").is_err());
        let mut good = PerfectHashIndex::build(["a", "b"])
            .unwrap()
            .to_bytes()
            .unwrap();
        good[0] = b'X'; // break the magic
        assert!(PerfectHashIndex::from_bytes(&good).is_err());
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

    /// 0.5.0 narrowed the arena's offsets, so a blob written by 0.1-0.4 no longer describes this
    /// layout. It must be refused by the magic, not silently misread as the new one.
    #[test]
    fn a_0_6_bmp2_blob_still_loads() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma"]).unwrap();
        let v3 = idx.to_bytes().unwrap();
        assert_eq!(&v3[0..4], b"BMP3");
        // A 0.5/0.6 blob is the v3 bytes minus the cap field.
        let mut v2 = v3.clone();
        v2.drain(12..20);
        v2[0..4].copy_from_slice(b"BMP2");
        let restored = PerfectHashIndex::from_bytes(&v2).unwrap();
        for w in ["alpha", "beta", "gamma"] {
            assert_eq!(restored.id(w), idx.id(w));
            assert_eq!(restored.key(restored.id(w).unwrap()), Some(w));
        }
        assert_eq!(restored.id("delta"), None);
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

    #[test]
    fn rejects_a_pre_0_5_blob() {
        let mut old = PerfectHashIndex::build(["a", "b"])
            .unwrap()
            .to_bytes()
            .unwrap();
        old[0..4].copy_from_slice(b"BMP1");
        assert!(PerfectHashIndex::from_bytes(&old).is_err());
    }
}
