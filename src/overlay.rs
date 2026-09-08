//! Additions and removals on top of an index that is immutable by design.
//!
//! The three indexes are build-once summaries: there is no way to add a key to a finite-state
//! transducer or a minimal perfect hash without rebuilding it. [`Overlay`] is the usual answer —
//! keep the base as it is, hold what came later beside it, and mark what went away — so a catalog
//! that mostly grows at the edges is not rebuilt on every change.
//!
//! **Removal is by id, not by key, and that is what makes it well defined over a probabilistic
//! base.** A tombstone is a bit against the id the base already assigned, tested only after the
//! base has said yes, so a live base key costs one bitset probe and nothing else. Over
//! [`CompactHashIndex`](crate::CompactHashIndex) a false positive that lands on a tombstoned id
//! reads as absent — which is the contract that index already has, rather than a new one.
//!
//! The same probabilistic base makes [`remove`](Overlay::remove) the one operation to be careful
//! with: a key the base has never seen can match a live id, and removing it retires that id along
//! with the real key behind it. See the method for the whole of it.

use crate::IndexError;

/// What an [`Overlay`] needs from the index underneath it.
///
/// Deliberately not the indexes' own `len` and `id`: those return different id widths (`u64` for
/// [`StringIndex`](crate::StringIndex), `u32` for the hash indexes) and an overlay has to number
/// base keys and its own additions in one space, so it works in `u64` throughout. The names differ
/// from the inherent ones for the same reason — an inherent method wins name resolution, and a
/// collision here would silently be the other id type.
pub trait OverlayBase {
    /// How many keys the base holds. Overlay ids for additions start here.
    fn base_len(&self) -> usize;

    /// The base's id for `key`, widened. `None` if the base does not hold it — subject to the
    /// base's own membership contract, which for `CompactHashIndex` is probabilistic.
    fn base_id(&self, key: &str) -> Option<u64>;

    /// The base's own serialised form, embedded verbatim in [`Overlay::to_bytes`].
    fn base_to_bytes(&self) -> Result<Vec<u8>, IndexError>;

    /// Which base produced a blob, recorded in its header.
    ///
    /// [`Overlay::from_bytes_with`] checks this against the base being loaded *before* calling the
    /// loader, so handing a `StringIndex` blob to a perfect-hash loader is a named error rather
    /// than an unchecked deserialisation of the wrong bytes. Tags `0..=15` are reserved for this
    /// crate; an outside implementation should pick above that.
    const BASE_TAG: u8;

    /// Whether [`base_id`](Self::base_id) never answers `Some` for a key the base does not hold.
    ///
    /// True for the two exact indexes, false for `CompactHashIndex`, whose fingerprints admit false
    /// positives. [`Overlay::from_bytes_with`] uses it to reject a crafted blob whose additions
    /// duplicate a base key — a blob that no [`to_bytes`](Overlay::to_bytes) writes, and that would
    /// otherwise load with a [`len`](Overlay::len) counting a key twice while
    /// [`id`](Overlay::id) can only ever answer the base's. Over a probabilistic base the same
    /// check would reject sound blobs, so it is not applied there.
    ///
    /// Defaults to `false`, the conservative answer: an outside implementation opts in.
    const EXACT_MEMBERSHIP: bool = false;
}

/// A base that stores its keys, and so can answer `id → key` and be rebuilt from its own contents.
///
/// [`CompactHashIndex`](crate::CompactHashIndex) stores no keys and does not implement this, which
/// is why an overlay over it has neither [`key`](Overlay::key) nor [`compact`](Overlay::compact).
/// The absence is in the type rather than in a runtime error.
pub trait OverlayKeys: OverlayBase + Sized {
    /// The base's key at `id`, or `None` if `id` is out of the base's range.
    fn base_key(&self, id: u64) -> Option<String>;

    /// Build a fresh base from `keys`, for [`Overlay::compact`].
    fn rebuild(keys: Vec<String>) -> Result<Self, IndexError>;
}

/// The `BuildHasher` of [`Overlay`]'s addition lookup, whose keys are already hashes: it hands back
/// the `u64` it was given.
///
/// That map is keyed on `blob::hash_block` output, which a splitmix64 finalizer has already
/// avalanched. Running the default SipHash over those eight bytes again would add its cost to every
/// `id` on an addition and buy nothing — the bucket index would be no better distributed. Only
/// `write_u64` is implemented; the map's key type is `u64`, so nothing else can be reached.
///
/// This is not a HashDoS position, and does not change one: `SECURITY.md` already says the hashes
/// are unseeded and deterministic, so someone who chooses the keys can search for collisions
/// offline whatever this map hashes with.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PreHashed(u64);

impl std::hash::Hasher for PreHashed {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("the addition lookup is keyed by u64, which hashes through write_u64")
    }

    fn write_u64(&mut self, h: u64) {
        self.0 = h;
    }
}

impl std::hash::BuildHasher for PreHashed {
    type Hasher = Self;

    fn build_hasher(&self) -> Self {
        Self(0)
    }
}

/// An immutable index plus the keys added after it and the ids retired from it.
///
/// Ids are stable: an id issued once is never issued again, and removing a key does not renumber
/// anything. Ids of additions continue from `base.len()`, so they never collide with base ids.
/// Re-adding a removed key restores its original id rather than issuing a new one — the key set is
/// what a caller asked about, and reviving keeps the numbering as small as it can be.
///
/// ```
/// use lexindex::{Overlay, StringIndex};
/// let mut ov = Overlay::new(StringIndex::build(["apple", "banana"])?);
/// let cherry = ov.add("cherry");
/// assert_eq!(ov.key(cherry).as_deref(), Some("cherry"));
/// assert!(ov.remove("apple"));
/// assert_eq!(ov.id("apple"), None);
/// assert_eq!(ov.len(), 2);
/// # Ok::<(), lexindex::IndexError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Overlay<I> {
    base: I,
    /// Every added key, once, in id order, as the records the blob format uses: `[len: u32 LE]`
    /// then the bytes. The in-memory arena *is* the blob's addition region.
    ///
    /// The additions are the only strings an overlay owns, and until 1.0.1 it owned each of them
    /// twice: once in a `Vec<String>`, so `key(id)` could index, and again as the owned key of a
    /// `HashMap<String, u64>`, so `id(key)` could hash. One arena serves both. The length travels
    /// *inside* the record rather than in a side table so that a lookup that has the record's
    /// start has everything: the map, then the bytes, and nothing in between to wait for.
    added_data: Vec<u8>,
    /// Where the `i`-th addition's record starts in [`added_data`](Self::added_data); its length
    /// is how many additions there are. Read by `key(id)`, never by `id(key)`.
    added_starts: Vec<u32>,
    /// Hash of an added key → `(index, record start)` of the first addition carrying that hash.
    /// The stored bytes are compared before the index is believed, so the map never holds a
    /// string of its own; and the start rides in the value so the compare needs no second table.
    ///
    /// Which hash is an in-memory choice and not a format one: nothing here is serialised, and a
    /// blob's additions are a plain length-prefixed list. It is `blob::hash_block` because that one
    /// exists in every feature configuration — `hash::hash_key` lives behind `mph` and an overlay
    /// does not — and because it consumes eight bytes per multiply, so a long added key does not
    /// pay per byte the way the byte-serial `hash_bytes` would.
    added_lookup: std::collections::HashMap<u64, (u32, u32), PreHashed>,
    /// `(hash, index, start)` for an addition whose hash a previous one already had. Two *distinct* keys
    /// need a full 64-bit collision to land here — `n(n-1)/2^65`, which is 2.7e-8 at a million
    /// additions — so this is empty in every run that will ever happen. It exists because
    /// "essentially never" is not a contract, and the lookup above would otherwise answer `None`
    /// for a key it holds.
    added_collisions: Vec<(u64, u32, u32)>,
    /// One bit per id ever issued: base ids below `base.base_len()`, additions above it.
    dead: Vec<u64>,
    live: usize,
}

impl<I: OverlayBase> Overlay<I> {
    /// Wrap `base`. Nothing is copied and no id changes; an overlay with no edits answers exactly
    /// as the base does.
    pub fn new(base: I) -> Self {
        let live = base.base_len();
        Self {
            base,
            added_data: Vec::new(),
            added_starts: Vec::new(),
            added_lookup: std::collections::HashMap::default(),
            added_collisions: Vec::new(),
            dead: Vec::new(),
            live,
        }
    }

    /// The index underneath, unchanged.
    pub fn base(&self) -> &I {
        &self.base
    }

    /// How many keys are live: base keys plus additions, less what has been removed.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Whether every key has been removed (or there were none).
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many ids have ever been issued. `key(id)` is `None` for every `id` at or above this,
    /// and ids below it may be live or retired.
    pub fn id_space(&self) -> u64 {
        self.base.base_len() as u64 + self.added_starts.len() as u64
    }

    /// The bytes of the addition whose record starts at `start`.
    fn record_at(&self, start: u32) -> &[u8] {
        let at = start as usize;
        let len = read_u32(&self.added_data, at) as usize;
        &self.added_data[at + 4..at + 4 + len]
    }

    /// The `i`-th addition, borrowed from the arena.
    fn added_key(&self, i: usize) -> &str {
        std::str::from_utf8(self.record_at(self.added_starts[i]))
            .expect("every overlay addition was UTF-8 when it was pushed")
    }

    /// The index of the addition equal to `key`, or `None` if there is none.
    fn added_index(&self, key: &str) -> Option<usize> {
        let h = crate::blob::hash_block(key.as_bytes());
        let &(first, start) = self.added_lookup.get(&h)?;
        if self.record_at(start) == key.as_bytes() {
            return Some(first as usize);
        }
        // Reached only when two distinct added keys share a 64-bit hash; see `added_collisions`.
        self.added_collisions.iter().find_map(|&(hh, i, at)| {
            (hh == h && self.record_at(at) == key.as_bytes()).then_some(i as usize)
        })
    }

    /// Append `key` to the arena and index it. The caller has already established that no addition
    /// equals it, which is what lets an occupied slot go straight to the collision list.
    ///
    /// The three narrowings panic rather than wrap, the way `Vec` panics on capacity overflow: an
    /// overlay is a staging structure, and one holding four billion additions or four gibibytes of
    /// them has missed every reasonable point to [`compact`](Self::compact).
    fn push_added(&mut self, key: &str) -> usize {
        let i = self.added_starts.len();
        let index = u32::try_from(i).expect("an Overlay holds at most u32::MAX additions");
        let start = u32::try_from(self.added_data.len())
            .expect("an Overlay holds at most 4 GiB of added keys");
        let len = u32::try_from(key.len()).expect("an added key is at most 4 GiB long");
        self.added_starts.push(start);
        self.added_data.extend_from_slice(&len.to_le_bytes());
        self.added_data.extend_from_slice(key.as_bytes());
        let h = crate::blob::hash_block(key.as_bytes());
        match self.added_lookup.entry(h) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((index, start));
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                self.added_collisions.push((h, index, start));
            }
        }
        i
    }

    fn is_dead(&self, id: u64) -> bool {
        let (w, b) = ((id / 64) as usize, id % 64);
        self.dead.get(w).is_some_and(|word| word >> b & 1 == 1)
    }

    fn set_dead(&mut self, id: u64, dead: bool) {
        let (w, b) = ((id / 64) as usize, id % 64);
        if w >= self.dead.len() {
            if !dead {
                return;
            }
            self.dead.resize(w + 1, 0);
        }
        if dead {
            self.dead[w] |= 1 << b;
        } else {
            self.dead[w] &= !(1 << b);
        }
    }

    /// The id of `key`, or `None` if it is absent or has been removed.
    ///
    /// A key the base holds costs the base's own lookup plus one bitset probe, and is unaffected
    /// by anything below. A key added later costs a hash lookup and an arena read on top, which is
    /// the price of an overlay and the reason [`compact`](Self::compact) exists.
    ///
    /// That second path is two dependent memory accesses: the map, whose value carries the
    /// record's position, and then the record itself, whose length is its first four bytes. The
    /// arena layout was chosen for exactly that count. Its first version kept the lengths in a
    /// side table, which put a third access between the two and cost 40 % on an added key once
    /// probes were shuffled -- in insertion order the loss hid behind a stride prefetcher.
    /// Measured on a million ten-byte additions over a 100 000-word base, shuffled probes, `id`
    /// on an addition is 278 ns against 295 ns for the `Vec<String>` + `HashMap<String, u64>`
    /// layout it replaced, at 54 bytes held per addition against 147.
    pub fn id(&self, key: &str) -> Option<u64> {
        if let Some(id) = self.base.base_id(key) {
            // Only reached once the base has already answered, so the base path pays for this bit
            // and nothing more. A base key cannot also be an addition: `add` revives instead.
            return (!self.is_dead(id)).then_some(id);
        }
        let id = self.base.base_len() as u64 + self.added_index(key)? as u64;
        (!self.is_dead(id)).then_some(id)
    }

    /// Whether `key` is live, with the base's membership contract — exact for
    /// [`StringIndex`](crate::StringIndex) and [`PerfectHashIndex`](crate::PerfectHashIndex),
    /// probabilistic for [`CompactHashIndex`](crate::CompactHashIndex).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Add `key` and return its id, or return the id it already has.
    ///
    /// Re-adding a removed key revives its id. Over a probabilistic base a key that false-positives
    /// against the base is reported as already present and nothing is stored — the same answer the
    /// base alone would have given, which is why the overlay does not make that contract worse.
    pub fn add(&mut self, key: &str) -> u64 {
        if let Some(id) = self.base.base_id(key) {
            if self.is_dead(id) {
                self.set_dead(id, false);
                self.live += 1;
            }
            return id;
        }
        if let Some(i) = self.added_index(key) {
            let id = self.base.base_len() as u64 + i as u64;
            if self.is_dead(id) {
                self.set_dead(id, false);
                self.live += 1;
            }
            return id;
        }
        let id = self.id_space();
        self.push_added(key);
        self.live += 1;
        id
    }

    /// Remove `key`. Returns whether it was live.
    ///
    /// The id is retired, not recycled: `key(id)` stops answering and no later addition receives
    /// it, so ids handed out earlier stay valid for whoever holds them.
    ///
    /// **Over a probabilistic base this is the one operation whose false positive costs
    /// something.** [`CompactHashIndex`](crate::CompactHashIndex) stores no keys, so a string it
    /// has never seen can still match a live id — and removing it retires *that* id, taking a real
    /// key with it. Reads already had this contract; a write does not get to be safer than the
    /// index it writes to. Remove only keys the caller knows are present, or put the overlay over
    /// [`PerfectHashIndex`](crate::PerfectHashIndex), whose membership is verified against the
    /// stored key.
    pub fn remove(&mut self, key: &str) -> bool {
        let Some(id) = self.id(key) else {
            return false;
        };
        self.set_dead(id, true);
        self.live -= 1;
        true
    }
}

impl<I: OverlayKeys> Overlay<I> {
    /// The key at `id`, or `None` if the id was never issued or has been retired.
    pub fn key(&self, id: u64) -> Option<String> {
        if self.is_dead(id) {
            return None;
        }
        let base_n = self.base.base_len() as u64;
        match id.checked_sub(base_n) {
            None => self.base.base_key(id),
            // `try_from` rather than `as`: on a 32-bit target an id above `usize::MAX` would
            // otherwise truncate onto a real addition and answer someone else's key.
            Some(off) => usize::try_from(off)
                .ok()
                .filter(|&i| i < self.added_starts.len())
                .map(|i| self.added_key(i).to_owned()),
        }
    }

    /// Every live key, in id order.
    pub fn keys(&self) -> Vec<String> {
        (0..self.id_space()).filter_map(|id| self.key(id)).collect()
    }

    /// Rebuild the base from the live keys and return a fresh overlay over it, collapsing the
    /// additions and tombstones back into one index.
    ///
    /// **Ids do not survive this.** A rebuilt [`StringIndex`](crate::StringIndex) numbers by sorted
    /// rank and a rebuilt perfect hash numbers by construction, so an id held across a `compact` is
    /// meaningless. That is the trade the overlay exists to postpone, not to remove.
    pub fn compact(self) -> Result<Self, IndexError> {
        Ok(Self::new(I::rebuild(self.keys())?))
    }
}

/// Share one base between several overlays, and let a caller hold on to it as well.
///
/// The Python bindings need exactly this: `Overlay(index)` must not take the index away from the
/// object the caller still holds, and cloning a `PerfectHashIndex` is not on offer.
impl<T: OverlayBase> OverlayBase for std::sync::Arc<T> {
    const BASE_TAG: u8 = T::BASE_TAG;
    const EXACT_MEMBERSHIP: bool = T::EXACT_MEMBERSHIP;

    fn base_len(&self) -> usize {
        (**self).base_len()
    }

    fn base_id(&self, key: &str) -> Option<u64> {
        (**self).base_id(key)
    }

    fn base_to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        (**self).base_to_bytes()
    }
}

impl<T: OverlayKeys> OverlayKeys for std::sync::Arc<T> {
    fn base_key(&self, id: u64) -> Option<String> {
        (**self).base_key(id)
    }

    fn rebuild(keys: Vec<String>) -> Result<Self, IndexError> {
        Ok(std::sync::Arc::new(T::rebuild(keys)?))
    }
}

impl OverlayBase for crate::StringIndex {
    const BASE_TAG: u8 = 1;
    const EXACT_MEMBERSHIP: bool = true;

    fn base_len(&self) -> usize {
        self.len()
    }

    fn base_id(&self, key: &str) -> Option<u64> {
        self.id(key)
    }

    fn base_to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        Ok(self.to_bytes())
    }
}

impl OverlayKeys for crate::StringIndex {
    fn base_key(&self, id: u64) -> Option<String> {
        self.key(id)
    }

    fn rebuild(keys: Vec<String>) -> Result<Self, IndexError> {
        Self::build(keys)
    }
}

#[cfg(feature = "mph")]
impl OverlayBase for crate::PerfectHashIndex {
    const BASE_TAG: u8 = 2;
    const EXACT_MEMBERSHIP: bool = true;

    fn base_len(&self) -> usize {
        self.len()
    }

    fn base_id(&self, key: &str) -> Option<u64> {
        self.id(key).map(u64::from)
    }

    fn base_to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        self.to_bytes()
    }
}

#[cfg(feature = "mph")]
impl OverlayKeys for crate::PerfectHashIndex {
    fn base_key(&self, id: u64) -> Option<String> {
        self.key(u32::try_from(id).ok()?).map(str::to_owned)
    }

    fn rebuild(keys: Vec<String>) -> Result<Self, IndexError> {
        Self::build(keys)
    }
}

#[cfg(feature = "mph")]
impl OverlayBase for crate::CompactHashIndex {
    const BASE_TAG: u8 = 3;

    fn base_len(&self) -> usize {
        self.len()
    }

    fn base_id(&self, key: &str) -> Option<u64> {
        self.id(key).map(u64::from)
    }

    fn base_to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        self.to_bytes()
    }
}

/// Magic for the [`Overlay::to_bytes`] blob. The trailing digit is the format version, as
/// everywhere else in this crate; a change to the layout below bumps it.
const OVERLAY_MAGIC: &[u8; 4] = b"OVL2";

/// The format `0.12` wrote: the same three sections, but with the tombstone word count buried
/// between the additions and the words, and no checksum anywhere. Still read — the parser is the
/// same one, and an overlay over a `StringIndex` is the one pre-1.0 blob in this crate that 1.0
/// can still open — but never written: saving one again produces `OVL2`.
const OVERLAY_MAGIC_V1: &[u8; 4] = b"OVL1";

/// `[magic 4][base tag 1][base blob len 8][addition count 8][addition bytes 8][tombstone words 8]
/// [payload 8][check 4]`.
const OVERLAY_HEADER: usize = 4 + 1 + 8 + 8 + 8 + 8 + 8 + 4;

/// Header bytes the trailing check covers.
const OVERLAY_CHECKED: usize = OVERLAY_HEADER - 4;

/// `[magic 4][base tag 1][base blob len 8][addition count 8]`.
const OVERLAY_HEADER_V1: usize = 4 + 1 + 8 + 8;

/// Fill in the two checksums of an otherwise complete `OVL2` blob: the payload hash over every
/// section, then the header hash over the header including it. Both are recomputed from the bytes
/// that are there, so sealing a blob a test has tampered with is the same operation as writing one
/// — which is what lets those tests show that the semantic checks stand on their own.
fn seal(out: &mut [u8]) {
    let payload = crate::blob::hash_block(&out[OVERLAY_HEADER..]);
    out[37..45].copy_from_slice(&payload.to_le_bytes());
    let check = crate::blob::hash_bytes(&out[..OVERLAY_CHECKED]) as u32;
    out[OVERLAY_CHECKED..OVERLAY_HEADER].copy_from_slice(&check.to_le_bytes());
}

/// The three sections of an overlay blob, already bounded by whichever header framed them.
struct Sections<'a> {
    base: &'a [u8],
    added_count: usize,
    additions: &'a [u8],
    tombstones: &'a [u8],
}

impl<I: OverlayBase> Overlay<I> {
    /// Serialise to `[header][base blob][additions][tombstones]`, where the header is
    /// `[magic "OVL2"][base tag u8][base blob len u64][addition count u64][addition bytes u64]
    /// [tombstone words u64][payload u64][check u32]`.
    ///
    /// Additions are length-prefixed (`u32` length, then the bytes) in id order; tombstones are
    /// bare `u64` words; little-endian throughout. `check` is a hash of the header bytes before it,
    /// and `payload` a hash of everything after it — so a flipped bit in an addition that stays
    /// valid UTF-8, or in a tombstone word, is caught on load instead of loading as a different key
    /// or a revived id.
    ///
    /// **Every section length is in the header.** That is what lets the loader bound each region
    /// before reading a byte of it, and what makes the payload hash checkable *before* any of the
    /// contents are trusted. Neither `len` nor the live/dead split is stored: both are derived on
    /// load, so a blob cannot disagree with itself about how many keys it holds.
    ///
    /// The base is serialised with its own `to_bytes`, so the blob inherits exactly the
    /// trust model of the base's format — see [`from_bytes_with`](Self::from_bytes_with).
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let base = self.base.base_to_bytes()?;
        let added = self.added_data.len();
        let mut out = Vec::with_capacity(OVERLAY_HEADER + base.len() + added + self.dead.len() * 8);
        out.resize(OVERLAY_HEADER, 0);
        out.extend_from_slice(&base);
        // The arena already holds the additions in the blob's own record form.
        out.extend_from_slice(&self.added_data);
        for word in &self.dead {
            out.extend_from_slice(&word.to_le_bytes());
        }
        // Written last, over the sections already in place: the payload hash covers exactly the
        // bytes the loader will hash back, and nothing the header says about them.
        out[0..4].copy_from_slice(OVERLAY_MAGIC);
        out[4] = I::BASE_TAG;
        out[5..13].copy_from_slice(&(base.len() as u64).to_le_bytes());
        out[13..21].copy_from_slice(&(self.added_starts.len() as u64).to_le_bytes());
        out[21..29].copy_from_slice(&(added as u64).to_le_bytes());
        out[29..37].copy_from_slice(&(self.dead.len() as u64).to_le_bytes());
        seal(&mut out);
        Ok(out)
    }

    /// Write [`to_bytes`](Self::to_bytes) to `path`, atomically: a crash, a full disk or a kill
    /// mid-write leaves the previous file intact rather than a truncated one under the real name.
    /// The overlay is the crate's *mutable* layer, so it is the structure most likely to be
    /// rewritten in place, and it gets the same guarantee the three indexes already have.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        let bytes = self.to_bytes()?;
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            use std::io::Write;
            w.write_all(&bytes)?;
            Ok(())
        })
    }

    /// Reconstruct from [`to_bytes`](Self::to_bytes) output, with `load_base` reconstructing the
    /// base from its slice of the blob.
    ///
    /// The base loader is the caller's because `Overlay<I>` is generic over the base and each base
    /// parses its own blob; naming the function is how the overlay learns which one to call:
    ///
    /// ```
    /// # use lexindex::{Overlay, StringIndex};
    /// let mut ov = Overlay::new(StringIndex::build(["apple", "banana"])?);
    /// ov.add("cherry");
    /// ov.remove("apple");
    /// let blob = ov.to_bytes()?;
    /// let back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes)?;
    /// assert_eq!(back.len(), 2);
    /// assert_eq!(back.id("apple"), None);
    /// assert_eq!(back.id("cherry"), ov.id("cherry"));
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    ///
    /// **Safe on arbitrary bytes — as far as `load_base` is.** The order is: the header checksum,
    /// then the section lengths against the bytes actually present, then the payload checksum, and
    /// only then the contents. So no header field can steer an index or an allocation, and nothing
    /// is parsed out of a region that has not already been shown intact.
    ///
    /// The base region is then handed to `load_base`, and **that closure is where the caller
    /// decides how far to trust it**. Both hash indexes' loaders are total, so for those the
    /// question does not arise. `StringIndex::from_bytes` is the exception `SECURITY.md`
    /// documents — a crafted transducer can make it *panic* — so a blob a stranger wrote must be
    /// loaded with [`StringIndex::from_untrusted_bytes`](crate::StringIndex::from_untrusted_bytes)
    /// instead. Everything the overlay checks is total either way; it cannot vouch for a region it
    /// hands to someone else.
    ///
    /// Past the checksums the checks are semantic, because a hash vouches for transport and not for
    /// what was written: additions are checked for duplicates and, over a base whose membership is
    /// exact ([`EXACT_MEMBERSHIP`](OverlayBase::EXACT_MEMBERSHIP)), against the base itself;
    /// tombstones are checked for bits outside the id space; every added key for UTF-8. Over a
    /// `CompactHashIndex` base the check against the base is skipped, because a false positive
    /// would reject a sound blob.
    ///
    /// A `0.12` blob (magic `OVL1`) still loads, and gets every check above except the two
    /// checksums, which that format does not carry. Re-saving it writes `OVL2` and it gains them.
    pub fn from_bytes_with(
        bytes: &[u8],
        load_base: impl FnOnce(&[u8]) -> Result<I, IndexError>,
    ) -> Result<Self, IndexError> {
        let legacy = bytes.len() >= 4 && &bytes[..4] == OVERLAY_MAGIC_V1;
        let header = if legacy {
            OVERLAY_HEADER_V1
        } else {
            OVERLAY_HEADER
        };
        if bytes.len() < header || (!legacy && &bytes[..4] != OVERLAY_MAGIC) {
            return Err(IndexError::Format("bad overlay magic or truncated header"));
        }
        if !legacy {
            let check = u32::from_le_bytes(
                bytes[OVERLAY_CHECKED..OVERLAY_HEADER]
                    .try_into()
                    .expect("4 bytes"),
            );
            if check != crate::blob::hash_bytes(&bytes[..OVERLAY_CHECKED]) as u32 {
                return Err(IndexError::Format("overlay header checksum mismatch"));
            }
        }
        if bytes[4] != I::BASE_TAG {
            return Err(IndexError::Format(
                "overlay blob was written over a different base index",
            ));
        }
        // Every count below comes from the blob, so it is narrowed rather than cast: on a 32-bit
        // target `as usize` would truncate a fabricated length into a plausible one.
        let base_len = usize::try_from(read_u64(bytes, 5))
            .map_err(|_| IndexError::Format("overlay base blob length out of range"))?;
        let added_count = usize::try_from(read_u64(bytes, 13))
            .map_err(|_| IndexError::Format("overlay addition count out of range"))?;
        let base_end = header
            .checked_add(base_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(IndexError::Format("overlay base blob out of range"))?;

        let sections = if legacy {
            v1_sections(bytes, base_end, added_count)?
        } else {
            v2_sections(bytes, base_end, added_count)?
        };
        let base = load_base(sections.base)?;
        Self::assemble(base, sections)
    }

    /// Read a file written by [`save`](Self::save); see
    /// [`from_bytes_with`](Self::from_bytes_with) for why the base loader is the caller's.
    pub fn load_with(
        path: impl AsRef<std::path::Path>,
        load_base: impl FnOnce(&[u8]) -> Result<I, IndexError>,
    ) -> Result<Self, IndexError> {
        let bytes = std::fs::read(path).map_err(IndexError::Io)?;
        Self::from_bytes_with(&bytes, load_base)
    }

    /// The contents of an already-bounded blob, checked against each other and against the base.
    fn assemble(base: I, sections: Sections<'_>) -> Result<Self, IndexError> {
        let Sections {
            added_count,
            additions,
            tombstones,
            ..
        } = sections;
        let mut at = 0;
        // The region's length is an upper bound on the arena, and those bytes have already been
        // shown to be present — so this reserve is sized by the blob rather than by a claim in it.
        let mut this = Self {
            base,
            added_data: Vec::with_capacity(additions.len()),
            added_starts: Vec::with_capacity(added_count.min(1 << 16)),
            added_lookup: std::collections::HashMap::with_capacity_and_hasher(
                added_count.min(1 << 16),
                PreHashed::default(),
            ),
            added_collisions: Vec::new(),
            dead: Vec::new(),
            live: 0,
        };
        for _ in 0..added_count {
            let (key, next) = read_addition(additions, at)?;
            at = next;
            // `add` consults the base first and revives its id rather than issuing a second one, so
            // no blob this crate writes holds an addition the base already has. Over a
            // probabilistic base the same check would reject sound blobs, hence the constant.
            if I::EXACT_MEMBERSHIP && this.base.base_id(key).is_some() {
                return Err(IndexError::Format("overlay addition duplicates a base key"));
            }
            if this.added_index(key).is_some() {
                return Err(IndexError::Format("duplicate key among overlay additions"));
            }
            this.push_added(key);
        }
        // Only reachable under `OVL2`, whose header states the region's length independently of the
        // additions in it: `OVL1` has no such field, so its region is whatever the walk consumed.
        if at != additions.len() {
            return Err(IndexError::Format(
                "overlay addition region is longer than its additions",
            ));
        }

        // Read from the slice rather than from any count: a fabricated count can no longer size an
        // allocation, because the bytes it claims have already been shown to be there.
        let dead: Vec<u64> = tombstones
            .chunks_exact(8)
            .map(|w| u64::from_le_bytes(w.try_into().expect("8 bytes")))
            .collect();

        let id_space = this.id_space();
        let last = (id_space / 64) as usize;
        let stray = dead.iter().enumerate().any(|(w, word)| match w.cmp(&last) {
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => word & !mask_below(id_space % 64) != 0,
            std::cmp::Ordering::Greater => *word != 0,
        });
        if stray {
            return Err(IndexError::Format("overlay tombstone outside the id space"));
        }
        // Every set bit is below `id_space`, so the count cannot exceed it.
        let retired: u64 = dead.iter().map(|w| u64::from(w.count_ones())).sum();
        let live = (id_space - retired) as usize;

        this.dead = dead;
        this.live = live;
        Ok(this)
    }
}

/// Bound the two sections after the base from the `OVL2` header, and verify the payload checksum
/// over all three — before a single addition or tombstone is read.
fn v2_sections(
    bytes: &[u8],
    base_end: usize,
    added_count: usize,
) -> Result<Sections<'_>, IndexError> {
    let added_bytes = usize::try_from(read_u64(bytes, 21))
        .map_err(|_| IndexError::Format("overlay addition region out of range"))?;
    let words = usize::try_from(read_u64(bytes, 29))
        .map_err(|_| IndexError::Format("overlay tombstone count out of range"))?;
    let added_end = base_end
        .checked_add(added_bytes)
        .filter(|end| *end <= bytes.len())
        .ok_or(IndexError::Format("overlay addition region out of range"))?;
    let tombstone_bytes = words
        .checked_mul(8)
        .ok_or(IndexError::Format("overlay tombstone count out of range"))?;
    if bytes.len() - added_end != tombstone_bytes {
        return Err(IndexError::Format("overlay tombstone length mismatch"));
    }
    let stored = read_u64(bytes, 37);
    if stored != crate::blob::hash_block(&bytes[OVERLAY_HEADER..]) {
        return Err(IndexError::Format("overlay payload checksum mismatch"));
    }
    Ok(Sections {
        base: &bytes[OVERLAY_HEADER..base_end],
        added_count,
        additions: &bytes[base_end..added_end],
        tombstones: &bytes[added_end..],
    })
}

/// The same three sections out of an `OVL1` blob, whose addition region has no stated length: it
/// is whatever a length-only walk of `added_count` additions consumes, and the tombstone word count
/// sits after it rather than in the header.
fn v1_sections(
    bytes: &[u8],
    base_end: usize,
    added_count: usize,
) -> Result<Sections<'_>, IndexError> {
    let mut at = base_end;
    for _ in 0..added_count {
        at = read_addition(bytes, at)?.1;
    }
    let added_end = at;
    let count_end = at
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or(IndexError::Format("overlay tombstones truncated"))?;
    let words = usize::try_from(read_u64(bytes, at))
        .map_err(|_| IndexError::Format("overlay tombstone count out of range"))?;
    let tombstone_bytes = words
        .checked_mul(8)
        .ok_or(IndexError::Format("overlay tombstone count out of range"))?;
    if bytes.len() - count_end != tombstone_bytes {
        return Err(IndexError::Format("overlay tombstone length mismatch"));
    }
    Ok(Sections {
        base: &bytes[OVERLAY_HEADER_V1..base_end],
        added_count,
        additions: &bytes[base_end..added_end],
        tombstones: &bytes[count_end..],
    })
}

/// One length-prefixed addition at `at`, and where the next one starts. Every bound is checked
/// against `bytes`, so a fabricated length is an error rather than a slice past the end.
fn read_addition(bytes: &[u8], at: usize) -> Result<(&str, usize), IndexError> {
    let len_end = at
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or(IndexError::Format("overlay additions truncated"))?;
    let len = read_u32(bytes, at) as usize;
    let end = len_end
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or(IndexError::Format("overlay addition out of range"))?;
    let key = std::str::from_utf8(&bytes[len_end..end])
        .map_err(|_| IndexError::Format("overlay addition is not UTF-8"))?;
    Ok((key, end))
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

/// Bits `0..n` set. `n` is always `id_space % 64`, so it never reaches 64 and the shift is defined.
fn mask_below(n: u64) -> u64 {
    (1u64 << n) - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringIndex;

    /// Assemble a blob by hand, so a test can put in what `to_bytes` never would. Sealed like a
    /// real one: what these tests exercise is the checks *past* the checksums.
    fn craft(base: &StringIndex, additions: &[&[u8]], dead: &[u64]) -> Vec<u8> {
        craft_raw(&base.to_bytes(), additions, dead)
    }

    /// [`craft`] over a base region that is not an index this crate built.
    fn craft_raw(base: &[u8], additions: &[&[u8]], dead: &[u64]) -> Vec<u8> {
        let added: usize = additions.iter().map(|k| 4 + k.len()).sum();
        let mut out = vec![0u8; OVERLAY_HEADER];
        out[0..4].copy_from_slice(OVERLAY_MAGIC);
        out[4] = <StringIndex as OverlayBase>::BASE_TAG;
        out[5..13].copy_from_slice(&(base.len() as u64).to_le_bytes());
        out[13..21].copy_from_slice(&(additions.len() as u64).to_le_bytes());
        out[21..29].copy_from_slice(&(added as u64).to_le_bytes());
        out[29..37].copy_from_slice(&(dead.len() as u64).to_le_bytes());
        out.extend_from_slice(base);
        for key in additions {
            out.extend_from_slice(&(key.len() as u32).to_le_bytes());
            out.extend_from_slice(key);
        }
        for word in dead {
            out.extend_from_slice(&word.to_le_bytes());
        }
        seal(&mut out);
        out
    }

    /// An overlay is exactly as trustworthy as the loader it is handed.
    ///
    /// The specimen is the 111-byte `StringIndex` blob `tests/golden.rs` keeps: bytes crafted so
    /// that `fst`'s node decoder panics inside `from_bytes`. Sealed into an `OVL2` frame it is
    /// still those bytes, and every check the overlay makes for itself — magic, header checksum,
    /// the four lengths, the base tag, the payload checksum — passes before the base region is
    /// handed over. So this pins both halves of what the loader's docstring now says: the ordinary
    /// loader panics *through* an overlay, and `from_untrusted_bytes` turns that into the `Err` a
    /// caller can act on. Nothing about the frame can make that choice for them.
    #[test]
    fn an_overlay_is_only_as_trustworthy_as_the_base_loader_it_is_given() {
        const SPECIMEN: &[u8] = include_bytes!("../tests/data/panicking-1.0.0-string.bix");
        let blob = craft_raw(SPECIMEN, &[b"added"], &[]);

        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let trusting = std::panic::catch_unwind(|| {
            Overlay::<StringIndex>::from_bytes_with(&blob, StringIndex::from_bytes)
        });
        std::panic::set_hook(hook);
        assert!(
            trusting.is_err(),
            "from_bytes no longer panics on the specimen -- if fst's decoder became total, this \
             test and the loader docstring both need their exception removed"
        );

        assert!(matches!(
            Overlay::<StringIndex>::from_bytes_with(&blob, StringIndex::from_untrusted_bytes),
            Err(IndexError::Format(_))
        ));
    }

    /// The `OVL1` layout `0.12` wrote: no checksums, and the tombstone word count in the body.
    fn craft_v1(base: &StringIndex, additions: &[&[u8]], dead: &[u64]) -> Vec<u8> {
        let base = base.to_bytes();
        let mut out = OVERLAY_MAGIC_V1.to_vec();
        out.push(<StringIndex as OverlayBase>::BASE_TAG);
        out.extend_from_slice(&(base.len() as u64).to_le_bytes());
        out.extend_from_slice(&(additions.len() as u64).to_le_bytes());
        out.extend_from_slice(&base);
        for key in additions {
            out.extend_from_slice(&(key.len() as u32).to_le_bytes());
            out.extend_from_slice(key);
        }
        out.extend_from_slice(&(dead.len() as u64).to_le_bytes());
        for word in dead {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_blob_round_trips_every_edit_and_the_numbering() {
        let mut ov = Overlay::new(StringIndex::build(["a", "b", "c"]).unwrap());
        ov.add("d");
        ov.add("e");
        assert!(ov.remove("b"));
        assert!(ov.remove("d"));
        let blob = ov.to_bytes().unwrap();
        let mut back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes).unwrap();
        assert_eq!(back.len(), ov.len());
        assert_eq!(back.id_space(), ov.id_space());
        assert_eq!(back.keys(), ov.keys());
        for k in ["a", "b", "c", "d", "e"] {
            assert_eq!(back.id(k), ov.id(k), "{k}");
        }
        // A load carries the retired ids too, so the next addition continues past them rather than
        // reusing one, and a revival still finds its old id.
        assert_eq!(back.add("f"), 5);
        assert_eq!(back.add("d"), 3);
    }

    #[test]
    fn a_blob_with_no_edits_round_trips() {
        let ov = Overlay::new(StringIndex::build(["x"]).unwrap());
        let blob = ov.to_bytes().unwrap();
        let back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.id("x"), Some(0));
    }

    #[test]
    fn save_and_load_with_agree_with_the_in_memory_overlay() {
        let dir = std::env::temp_dir().join(format!("lexindex-ovl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.bin");
        let mut ov = Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        ov.add("c");
        ov.save(&path).unwrap();
        let back = Overlay::load_with(&path, StringIndex::from_bytes).unwrap();
        assert_eq!(back.keys(), ov.keys());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `save` goes through the crate's atomic writer, like the three indexes: rewriting an overlay
    /// in place replaces it whole or not at all, and leaves no temporary behind. The overlay is the
    /// mutable layer, so it is the file most often written over a live one.
    #[test]
    fn save_replaces_an_existing_file_whole_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("lexindex-ovl-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlay.bin");

        let mut first = Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        first.add("c");
        first.save(&path).unwrap();
        let first_len = std::fs::metadata(&path).unwrap().len();

        // A shorter overlay over the same path: a non-atomic rewrite could leave the tail of the
        // longer one behind, which would still parse as a valid blob.
        let second = Overlay::new(StringIndex::build(["a"]).unwrap());
        second.save(&path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() < first_len);
        let back = Overlay::load_with(&path, StringIndex::from_bytes).unwrap();
        assert_eq!(back.keys(), second.keys());

        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "atomic write left a temporary behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_malformed_blob_is_rejected_rather_than_half_loaded() {
        let base = StringIndex::build(["a", "b"]).unwrap();
        let good = craft(&base, &[b"c"], &[0b100]);
        assert!(Overlay::from_bytes_with(&good, StringIndex::from_bytes).is_ok());

        // Where the payload starts in a blob crafted over this base: `craft` writes the header,
        // then the base blob, so the first addition's length prefix begins here.
        let additions_at = OVERLAY_HEADER + base.to_bytes().len();

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("bad overlay magic or truncated header", {
                let mut b = good.clone();
                b[0] = b'X';
                b
            }),
            ("bad overlay magic or truncated header", good[..12].to_vec()),
            // Every case below is re-sealed after the tamper, so what it reaches is the check it
            // names and not the checksum: the semantic checks have to stand on their own.
            ("overlay blob was written over a different base index", {
                let mut b = good.clone();
                b[4] = 99;
                seal(&mut b);
                b
            }),
            // `u32::MAX`, not `u64::MAX`: a length that fits `usize` at *either* width takes the
            // range check at both, where `u64::MAX` is refused one step earlier on a 32-bit target
            // and this assertion would name the wrong message there. The narrowing itself is the
            // case below.
            ("overlay base blob out of range", {
                let mut b = good.clone();
                b[5..13].copy_from_slice(&u64::from(u32::MAX).to_le_bytes());
                seal(&mut b);
                b
            }),
            // Refused for its width on a 32-bit target and for its range on a 64-bit one; both are
            // a clean `Format`, and which one runs is the platform's business, not this test's.
            ("", {
                let mut b = good.clone();
                b[5..13].copy_from_slice(&u64::MAX.to_le_bytes());
                seal(&mut b);
                b
            }),
            ("overlay addition out of range", {
                let mut b = craft(&base, &[b"c"], &[]);
                b[additions_at..additions_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                seal(&mut b);
                b
            }),
            // The addition region's stated length is longer than the additions in it — a gap the
            // walk would otherwise skip past silently, since it stops at `added_count`.
            ("overlay addition region is longer than its additions", {
                let mut b = craft(&base, &[b"c"], &[]);
                b[21..29].copy_from_slice(&9u64.to_le_bytes());
                b.extend_from_slice(&[0; 4]);
                seal(&mut b);
                b
            }),
            ("overlay addition region out of range", {
                let mut b = craft(&base, &[b"c"], &[]);
                b[21..29].copy_from_slice(&u64::MAX.to_le_bytes());
                seal(&mut b);
                b
            }),
            (
                "duplicate key among overlay additions",
                craft(&base, &[b"c", b"c"], &[]),
            ),
            (
                "overlay addition is not UTF-8",
                craft(&base, &[&[0xff]], &[]),
            ),
            (
                "overlay tombstone outside the id space",
                craft(&base, &[b"c"], &[0b1000]),
            ),
            (
                "overlay tombstone outside the id space",
                craft(&base, &[b"c"], &[0, 1]),
            ),
            ("overlay tombstone length mismatch", {
                let mut b = craft(&base, &[b"c"], &[0]);
                b.truncate(b.len() - 1);
                b
            }),
            // A tombstone count large enough that `count * 8` wraps: in debug this used to be an
            // arithmetic-overflow panic and in release a wrapped product of zero, which let the
            // length identity pass and drove a `2^61`-element allocation. Both are `Format` now.
            ("overlay tombstone count out of range", {
                let mut b = craft(&base, &[b"c"], &[]);
                b[29..37].copy_from_slice(&(1u64 << 61).to_le_bytes());
                seal(&mut b);
                b
            }),
            ("overlay tombstone count out of range", {
                let mut b = craft(&base, &[b"c"], &[]);
                b[29..37].copy_from_slice(&u64::MAX.to_le_bytes());
                seal(&mut b);
                b
            }),
            // `add` revives a base key's own id rather than issuing a second one, so `to_bytes`
            // never writes this; loading it would count "a" twice in `len` while `id` could only
            // ever answer the base's.
            (
                "overlay addition duplicates a base key",
                craft(&base, &[b"a"], &[]),
            ),
        ];
        for (expected, bytes) in cases {
            match Overlay::from_bytes_with(&bytes, StringIndex::from_bytes) {
                // An empty `expected` asks only for a clean refusal, for the one case whose
                // message is a property of the target's pointer width.
                Err(IndexError::Format(msg)) if expected.is_empty() => {
                    assert!(msg.starts_with("overlay base blob"), "{msg}")
                }
                Err(IndexError::Format(msg)) => assert_eq!(msg, expected),
                other => panic!("expected {expected:?}, got {:?}", other.map(|o| o.len())),
            }
        }
    }

    /// The checksums are what `OVL2` exists for: a flipped bit anywhere past the magic is an error
    /// rather than a different key, a revived id, or a section boundary read from a wrong number.
    /// Every byte is tried, so this covers the header, the embedded base blob, the additions and
    /// the tombstone words alike — the base's own checksum catches its region, ours catches the
    /// rest, and neither region has a gap between them.
    #[test]
    fn a_flipped_bit_anywhere_is_caught() {
        let mut ov = Overlay::new(StringIndex::build(["a", "b", "c"]).unwrap());
        ov.add("dd");
        assert!(ov.remove("b"));
        let good = ov.to_bytes().unwrap();
        assert_eq!(&good[..4], OVERLAY_MAGIC);

        for pos in 0..good.len() {
            for bit in [0x01u8, 0x80] {
                let mut bad = good.clone();
                bad[pos] ^= bit;
                assert!(
                    Overlay::from_bytes_with(&bad, StringIndex::from_bytes).is_err(),
                    "byte {pos} bit {bit:#04x} was accepted",
                );
            }
        }
    }

    /// The two checksums cover disjoint regions, so each has to be shown to work on its own: a
    /// header-only tamper that is *not* re-sealed must fail on the header check specifically, and a
    /// payload-only tamper under a valid header must fail on the payload check.
    #[test]
    fn each_checksum_catches_its_own_region() {
        let base = StringIndex::build(["a", "b"]).unwrap();
        let good = craft(&base, &[b"c"], &[0b10]);

        let mut header_only = good.clone();
        header_only[13..21].copy_from_slice(&7u64.to_le_bytes());
        assert!(matches!(
            Overlay::from_bytes_with(&header_only, StringIndex::from_bytes),
            Err(IndexError::Format("overlay header checksum mismatch"))
        ));

        let mut payload_only = good.clone();
        let last = payload_only.len() - 1;
        payload_only[last] ^= 0b1;
        assert!(matches!(
            Overlay::from_bytes_with(&payload_only, StringIndex::from_bytes),
            Err(IndexError::Format("overlay payload checksum mismatch"))
        ));
    }

    /// `0.12`'s `OVL1` blobs still load — over a `StringIndex`, the one base whose own format 1.0
    /// can still read — and saving one again upgrades it to `OVL2`, checksums and all. That is the
    /// only migration this format needs: the parser never went away.
    #[test]
    fn a_legacy_blob_loads_and_is_rewritten_as_the_new_format() {
        let base = StringIndex::build(["a", "b", "c"]).unwrap();
        let old = craft_v1(&base, &[b"d"], &[0b010]);
        assert_eq!(&old[..4], OVERLAY_MAGIC_V1);

        let ov = Overlay::from_bytes_with(&old, StringIndex::from_bytes).expect("0.12 blob loads");
        assert_eq!(ov.len(), 3);
        assert_eq!(ov.id("b"), None);
        assert_eq!(ov.id("d"), Some(3));

        let new = ov.to_bytes().unwrap();
        assert_eq!(&new[..4], OVERLAY_MAGIC);
        let back = Overlay::from_bytes_with(&new, StringIndex::from_bytes).unwrap();
        assert_eq!(back.len(), ov.len());
        for key in ["a", "b", "c", "d"] {
            assert_eq!(back.id(key), ov.id(key), "id({key:?})");
        }
    }

    /// The legacy path frames its own sections, so it has framing errors the new one cannot reach:
    /// its addition region ends wherever the walk ends, and the tombstone count sits after it.
    #[test]
    fn the_legacy_framing_is_validated_too() {
        let base = StringIndex::build(["a", "b"]).unwrap();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("overlay tombstones truncated", {
                let mut b = craft_v1(&base, &[b"c"], &[]);
                b.truncate(b.len() - 4);
                b
            }),
            ("overlay additions truncated", {
                let mut b = craft_v1(&base, &[b"c"], &[]);
                b.truncate(b.len() - 8 - 4 - 1 + 2);
                b
            }),
            ("overlay tombstone length mismatch", {
                let mut b = craft_v1(&base, &[b"c"], &[0]);
                b.truncate(b.len() - 1);
                b
            }),
            ("overlay tombstone count out of range", {
                let mut b = craft_v1(&base, &[b"c"], &[]);
                let at = b.len() - 8;
                b[at..].copy_from_slice(&(1u64 << 61).to_le_bytes());
                b
            }),
            (
                "overlay tombstone outside the id space",
                craft_v1(&base, &[b"c"], &[0b1000]),
            ),
        ];
        for (expected, bytes) in cases {
            match Overlay::from_bytes_with(&bytes, StringIndex::from_bytes) {
                Err(IndexError::Format(msg)) => assert_eq!(msg, expected),
                other => panic!("expected {expected:?}, got {:?}", other.map(|o| o.len())),
            }
        }
    }

    #[cfg(feature = "mph")]
    #[test]
    fn a_blob_refuses_the_wrong_base_before_the_loader_runs() {
        use crate::PerfectHashIndex;
        let ov = Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        let blob = ov.to_bytes().unwrap();
        let mut called = false;
        let got = Overlay::<PerfectHashIndex>::from_bytes_with(&blob, |b| {
            called = true;
            PerfectHashIndex::from_bytes(b)
        });
        assert!(
            matches!(
                got,
                Err(IndexError::Format(
                    "overlay blob was written over a different base index"
                ))
            ),
            "a StringIndex blob was accepted for a perfect-hash base"
        );
        assert!(
            !called,
            "the base loader ran on bytes that were never written for it"
        );
    }

    /// An overlay over a shared base answers as one over an owned base, which is what lets the
    /// Python bindings wrap an index the caller still holds.
    #[test]
    fn an_overlay_over_a_shared_base_behaves_the_same() {
        let base = std::sync::Arc::new(StringIndex::build(["a", "b"]).unwrap());
        let mut ov = Overlay::new(std::sync::Arc::clone(&base));
        ov.add("c");
        assert!(ov.remove("a"));
        assert_eq!(ov.keys(), vec!["b".to_string(), "c".into()]);
        assert_eq!(
            base.len(),
            2,
            "the base is untouched and still the caller's"
        );
        let blob = ov.to_bytes().unwrap();
        let back = Overlay::from_bytes_with(&blob, |b| {
            StringIndex::from_bytes(b).map(std::sync::Arc::new)
        })
        .unwrap();
        assert_eq!(back.keys(), ov.keys());
        assert_eq!(back.compact().unwrap().len(), 2);
    }
    #[cfg(feature = "mph")]
    #[test]
    fn a_perfect_hash_overlay_round_trips_through_its_own_loader() {
        use crate::PerfectHashIndex;
        let mut ov = Overlay::new(PerfectHashIndex::build(["one", "two"]).unwrap());
        ov.add("three");
        assert!(ov.remove("two"));
        let blob = ov.to_bytes().unwrap();
        // SAFETY: the blob was produced by `to_bytes` in this process and has not left it.
        let back = Overlay::from_bytes_with(&blob, PerfectHashIndex::from_bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.id("two"), None);
        for k in ["one", "three"] {
            assert_eq!(back.key(back.id(k).unwrap()).as_deref(), Some(k));
        }
    }

    #[cfg(feature = "mph")]
    #[test]
    fn a_compact_hash_overlay_round_trips_its_membership() {
        use crate::CompactHashIndex;
        let mut ov = Overlay::new(CompactHashIndex::build(["alpha", "beta"], 4).unwrap());
        ov.add("gamma");
        assert!(ov.remove("alpha"));
        let blob = ov.to_bytes().unwrap();
        // SAFETY: the blob was produced by `to_bytes` in this process and has not left it.
        let back = Overlay::from_bytes_with(&blob, CompactHashIndex::from_bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.id("alpha"), None);
        assert!(back.contains("beta") && back.contains("gamma"));
    }

    #[test]
    fn an_overlay_with_no_edits_answers_as_the_base() {
        let base = StringIndex::build(["apple", "banana", "cherry"]).unwrap();
        let ov = Overlay::new(StringIndex::build(["apple", "banana", "cherry"]).unwrap());
        assert_eq!(ov.len(), base.len());
        for k in ["apple", "banana", "cherry"] {
            assert_eq!(ov.id(k), base.id(k));
        }
        assert_eq!(ov.id("durian"), None);
    }

    #[test]
    fn ids_are_never_recycled_and_never_change_meaning() {
        let mut ov = Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        let c = ov.add("c");
        let d = ov.add("d");
        assert_eq!((c, d), (2, 3));
        assert!(ov.remove("c"));
        // The retired id is not handed to the next addition, so an id held elsewhere never comes
        // to mean a different key.
        assert_eq!(ov.add("e"), 4);
        assert_eq!(ov.key(c), None);
        assert_eq!(ov.key(4).as_deref(), Some("e"));
        // Re-adding revives the original id rather than issuing a second one for one string.
        assert_eq!(ov.add("c"), c);
        assert_eq!(ov.key(c).as_deref(), Some("c"));
        assert_eq!(ov.id_space(), 5);
    }

    #[test]
    fn removing_a_base_key_hides_it_from_both_directions() {
        let mut ov = Overlay::new(StringIndex::build(["a", "b", "c"]).unwrap());
        assert!(ov.remove("b"));
        assert_eq!(ov.id("b"), None);
        assert_eq!(ov.key(1), None);
        assert_eq!(ov.len(), 2);
        assert!(!ov.remove("b"), "removing twice is not a second removal");
        assert_eq!(ov.len(), 2);
        assert_eq!(ov.keys(), vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn compact_keeps_the_keys_and_drops_the_numbering() {
        let mut ov = Overlay::new(StringIndex::build(["b", "d"]).unwrap());
        ov.add("a");
        ov.add("c");
        assert!(ov.remove("d"));
        let before: Vec<u64> = ["a", "b", "c"].iter().map(|k| ov.id(k).unwrap()).collect();
        assert_eq!(before, vec![2, 0, 3]);
        let after = ov.compact().unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after.keys(), vec!["a".to_string(), "b".into(), "c".into()]);
        // A rebuilt `StringIndex` numbers by sorted rank, which is the renumbering `compact`
        // documents: an id held across it is meaningless.
        assert_eq!(["a", "b", "c"].map(|k| after.id(k).unwrap()), [0u64, 1, 2]);
        assert_eq!(after.id("d"), None);
    }

    #[test]
    fn an_empty_base_still_takes_additions() {
        let mut ov = Overlay::new(StringIndex::build(Vec::<String>::new()).unwrap());
        assert!(ov.is_empty());
        assert_eq!(ov.add("only"), 0);
        assert_eq!(ov.key(0).as_deref(), Some("only"));
        assert_eq!(ov.len(), 1);
        assert!(!ov.is_empty());
    }

    #[cfg(feature = "mph")]
    #[test]
    fn overlay_over_a_perfect_hash_round_trips() {
        use crate::PerfectHashIndex;
        let mut ov = Overlay::new(PerfectHashIndex::build(["one", "two"]).unwrap());
        let three = ov.add("three");
        assert_eq!(three, 2);
        assert_eq!(ov.key(three).as_deref(), Some("three"));
        // The base's own numbering is whatever the perfect hash chose; what must hold is that the
        // overlay reads every live key back through it.
        for k in ["one", "two", "three"] {
            assert_eq!(ov.key(ov.id(k).unwrap()).as_deref(), Some(k));
        }
        assert!(ov.remove("one"));
        assert_eq!(ov.id("one"), None);
        assert_eq!(ov.len(), 2);
        let after = ov.compact().unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(
            after
                .keys()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            ["three".to_string(), "two".into()].into_iter().collect()
        );
    }

    /// The duplicate-addition check is keyed on `EXACT_MEMBERSHIP`, so it must not fire over a
    /// probabilistic base: `CompactHashIndex::id` answers `Some` for keys it never held, and
    /// rejecting on that would refuse blobs that are perfectly sound.
    #[cfg(feature = "mph")]
    #[test]
    fn a_duplicate_addition_is_refused_only_over_an_exact_base() {
        use crate::{CompactHashIndex, PerfectHashIndex};
        // Compile-time: which bases are exact is a property of the types, not of this run.
        const {
            assert!(<StringIndex as OverlayBase>::EXACT_MEMBERSHIP);
            assert!(<PerfectHashIndex as OverlayBase>::EXACT_MEMBERSHIP);
            assert!(!<CompactHashIndex as OverlayBase>::EXACT_MEMBERSHIP);
        }

        /// The same hand-assembly as `craft`, over whichever base is given.
        fn craft_over<B: OverlayBase>(base_blob: &[u8], addition: &[u8]) -> Vec<u8> {
            let mut out = vec![0u8; OVERLAY_HEADER];
            out[0..4].copy_from_slice(OVERLAY_MAGIC);
            out[4] = B::BASE_TAG;
            out[5..13].copy_from_slice(&(base_blob.len() as u64).to_le_bytes());
            out[13..21].copy_from_slice(&1u64.to_le_bytes());
            out[21..29].copy_from_slice(&(4 + addition.len() as u64).to_le_bytes());
            out.extend_from_slice(base_blob);
            out.extend_from_slice(&(addition.len() as u32).to_le_bytes());
            out.extend_from_slice(addition);
            seal(&mut out);
            out
        }

        let perfect = PerfectHashIndex::build(["one", "two"]).unwrap();
        let blob = craft_over::<PerfectHashIndex>(&perfect.to_bytes().unwrap(), b"one");
        // SAFETY: the base blob was produced by `to_bytes` in this process and has not left it.
        match Overlay::from_bytes_with(&blob, PerfectHashIndex::from_bytes) {
            Err(IndexError::Format(msg)) => {
                assert_eq!(msg, "overlay addition duplicates a base key")
            }
            other => panic!(
                "expected a duplicate error, got {:?}",
                other.map(|o| o.len())
            ),
        }

        let compact = CompactHashIndex::build(["one", "two"], 4).unwrap();
        let blob = craft_over::<CompactHashIndex>(&compact.to_bytes().unwrap(), b"one");
        // SAFETY: as above.
        let back = Overlay::from_bytes_with(&blob, CompactHashIndex::from_bytes)
            .expect("a probabilistic base must not have its additions checked against it");
        assert_eq!(back.len(), 3);
    }

    /// `CompactHashIndex` stores no keys, so an overlay over it has membership and nothing else —
    /// `key` and `compact` are not merely unimplemented but absent, since `OverlayKeys` is what
    /// provides them and it cannot be implemented without the keys.
    #[cfg(feature = "mph")]
    #[test]
    fn overlay_over_a_compact_hash_tracks_membership() {
        use crate::CompactHashIndex;
        let keys = ["alpha", "beta", "gamma"];
        let mut ov = Overlay::new(CompactHashIndex::build(keys, 4).unwrap());
        assert_eq!(ov.len(), 3);
        let delta = ov.add("delta");
        assert!(ov.contains("delta") && ov.id("delta") == Some(delta));
        assert!(ov.remove("beta"));
        assert_eq!(ov.id("beta"), None);
        assert_eq!(ov.len(), 3);
        // No false negatives: whatever the fingerprint does on strings never inserted, a key that
        // is live is found.
        for k in ["alpha", "gamma", "delta"] {
            assert!(ov.contains(k), "{k}");
        }
    }

    /// Two distinct additions whose 64-bit hashes are equal.
    ///
    /// The pair was found by Pollard rho over `blob::hash_block` (`local/blobcollide`), because no
    /// key set anyone builds will produce one: `n(n-1)/2^65` is 2.7e-8 at a million additions. The
    /// lookup's collision list exists for exactly this case, and without a pinned pair nothing
    /// would ever execute it — an untested branch guarding an event that cannot be reproduced is
    /// how a wrong answer waits years to be found.
    #[test]
    fn additions_sharing_a_hash_keep_their_own_ids() {
        const A: &str = "mqko3xbnxy2gd";
        const B: &str = "eye7iouhhweao";
        assert_ne!(A, B);
        assert_eq!(
            crate::blob::hash_block(A.as_bytes()),
            crate::blob::hash_block(B.as_bytes()),
            "the pinned pair no longer collides: `hash_block` changed, so find another with \
             `local/blobcollide` rather than deleting this test"
        );

        let base = StringIndex::build(["apple", "banana"]).unwrap();
        let mut ov = Overlay::new(base);
        let a = ov.add(A);
        let b = ov.add(B);
        assert_ne!(a, b, "a shared hash must not merge two keys onto one id");
        assert_eq!((ov.id(A), ov.id(B)), (Some(a), Some(b)));
        assert_eq!(
            (ov.key(a).as_deref(), ov.key(b).as_deref()),
            (Some(A), Some(B))
        );
        assert_eq!(
            ov.add(A),
            a,
            "re-adding revives rather than issuing a second id"
        );
        assert_eq!(ov.len(), 4);

        // Through a blob as well: `assemble` rebuilds the lookup from the bytes, so it has to
        // reach the same branch — including the duplicate check that runs before each push.
        let blob = ov.to_bytes().unwrap();
        let back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes).unwrap();
        assert_eq!((back.id(A), back.id(B)), (Some(a), Some(b)));

        assert!(ov.remove(A));
        assert_eq!((ov.id(A), ov.id(B)), (None, Some(b)));
    }
}
