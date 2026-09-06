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
    /// Key → id for everything added after the base, kept even once tombstoned so that re-adding
    /// revives the original id instead of issuing a second one for the same string.
    added: std::collections::HashMap<String, u64>,
    /// Additions in id order, so `key(id)` is an index rather than a scan.
    added_keys: Vec<String>,
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
            added: std::collections::HashMap::new(),
            added_keys: Vec::new(),
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
        self.base.base_len() as u64 + self.added_keys.len() as u64
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
    /// A key the base holds costs the base's own lookup plus one bitset probe. A key added later
    /// costs a hash-map lookup on top, which is the price of an overlay and the reason
    /// [`compact`](Self::compact) exists.
    pub fn id(&self, key: &str) -> Option<u64> {
        if let Some(id) = self.base.base_id(key) {
            // Only reached once the base has already answered, so the base path pays for this bit
            // and nothing more. A base key cannot also be an addition: `add` revives instead.
            return (!self.is_dead(id)).then_some(id);
        }
        match self.added.get(key) {
            Some(&id) if !self.is_dead(id) => Some(id),
            _ => None,
        }
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
        if let Some(&id) = self.added.get(key) {
            if self.is_dead(id) {
                self.set_dead(id, false);
                self.live += 1;
            }
            return id;
        }
        let id = self.id_space();
        self.added_keys.push(key.to_owned());
        self.added.insert(key.to_owned(), id);
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
            Some(off) => self.added_keys.get(off as usize).cloned(),
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

#[cfg(all(feature = "mph", target_pointer_width = "64"))]
impl OverlayBase for crate::PerfectHashIndex {
    const BASE_TAG: u8 = 2;

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

#[cfg(all(feature = "mph", target_pointer_width = "64"))]
impl OverlayKeys for crate::PerfectHashIndex {
    fn base_key(&self, id: u64) -> Option<String> {
        self.key(u32::try_from(id).ok()?).map(str::to_owned)
    }

    fn rebuild(keys: Vec<String>) -> Result<Self, IndexError> {
        Self::build(keys)
    }
}

#[cfg(all(feature = "mph", target_pointer_width = "64"))]
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
const OVERLAY_MAGIC: &[u8; 4] = b"OVL1";

/// `[magic 4][base tag 1][base blob len 8][additions 8]`.
const OVERLAY_HEADER: usize = 4 + 1 + 8 + 8;

impl<I: OverlayBase> Overlay<I> {
    /// Serialise to `[magic 4][base tag u8][base blob len u64][additions u64][base blob][additions][tombstones]`.
    ///
    /// Additions are length-prefixed (`u32` length, then the bytes) in id order; tombstones are a
    /// `u64` word count followed by the words, little-endian throughout. Neither `len` nor the
    /// live/dead split is stored: both are derived on load, so a blob cannot disagree with itself
    /// about how many keys it holds.
    ///
    /// The base is serialised with its own `to_bytes`, so the blob inherits exactly the
    /// trust model of the base's format — see [`from_bytes_with`](Self::from_bytes_with).
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let base = self.base.base_to_bytes()?;
        let added: usize = self.added_keys.iter().map(|k| 4 + k.len()).sum();
        let mut out =
            Vec::with_capacity(OVERLAY_HEADER + base.len() + added + 8 + self.dead.len() * 8);
        out.extend_from_slice(OVERLAY_MAGIC);
        out.push(I::BASE_TAG);
        out.extend_from_slice(&(base.len() as u64).to_le_bytes());
        out.extend_from_slice(&(self.added_keys.len() as u64).to_le_bytes());
        out.extend_from_slice(&base);
        for key in &self.added_keys {
            let len = u32::try_from(key.len())
                .map_err(|_| IndexError::Format("added key longer than 4 GiB"))?;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(key.as_bytes());
        }
        out.extend_from_slice(&(self.dead.len() as u64).to_le_bytes());
        for word in &self.dead {
            out.extend_from_slice(&word.to_le_bytes());
        }
        Ok(out)
    }

    /// Write [`to_bytes`](Self::to_bytes) to `path`.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        std::fs::write(path, self.to_bytes()?).map_err(IndexError::Io)
    }

    /// Reconstruct from [`to_bytes`](Self::to_bytes) output, with `load_base` reconstructing the
    /// base from its slice of the blob.
    ///
    /// The base loader is the caller's because the bases do not agree on safety:
    /// [`StringIndex::from_bytes`](crate::StringIndex::from_bytes) is safe, while the perfect-hash
    /// loaders are `unsafe fn` — a deserialised `ptr_hash` MPH cannot be validated from outside.
    /// Taking a closure keeps that distinction where the language already puts it instead of
    /// making every overlay load `unsafe` to accommodate two of the three bases:
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
    /// A `PerfectHashIndex` base is the same call with the `unsafe` the base's own loader demands:
    /// `unsafe { Overlay::from_bytes_with(&blob, |b| PerfectHashIndex::from_bytes(b)) }` — the
    /// closure body is what needs the block, so the surrounding call stays honest about which part
    /// carries the obligation.
    ///
    /// Additions are checked for duplicates, tombstones for bits outside the id space, and every
    /// added key for UTF-8. They are *not* checked against the base: a `CompactHashIndex` base
    /// answers membership probabilistically, so a false positive would reject a sound blob.
    pub fn from_bytes_with(
        bytes: &[u8],
        load_base: impl FnOnce(&[u8]) -> Result<I, IndexError>,
    ) -> Result<Self, IndexError> {
        let header = OVERLAY_HEADER;
        if bytes.len() < header || &bytes[..4] != OVERLAY_MAGIC {
            return Err(IndexError::Format("bad overlay magic or truncated header"));
        }
        if bytes[4] != I::BASE_TAG {
            return Err(IndexError::Format(
                "overlay blob was written over a different base index",
            ));
        }
        let base_len = read_u64(bytes, 5) as usize;
        let added_count = read_u64(bytes, 13) as usize;
        let base_end = header
            .checked_add(base_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(IndexError::Format("overlay base blob out of range"))?;
        let base = load_base(&bytes[header..base_end])?;

        let mut at = base_end;
        let mut added_keys = Vec::with_capacity(added_count.min(1 << 16));
        let mut added = std::collections::HashMap::with_capacity(added_count.min(1 << 16));
        for i in 0..added_count {
            if at + 4 > bytes.len() {
                return Err(IndexError::Format("overlay additions truncated"));
            }
            let len = read_u32(bytes, at) as usize;
            at += 4;
            let end = at
                .checked_add(len)
                .filter(|end| *end <= bytes.len())
                .ok_or(IndexError::Format("overlay addition out of range"))?;
            let key = std::str::from_utf8(&bytes[at..end])
                .map_err(|_| IndexError::Format("overlay addition is not UTF-8"))?;
            at = end;
            if added
                .insert(key.to_string(), base.base_len() as u64 + i as u64)
                .is_some()
            {
                return Err(IndexError::Format("duplicate key among overlay additions"));
            }
            added_keys.push(key.to_string());
        }

        if at + 8 > bytes.len() {
            return Err(IndexError::Format("overlay tombstones truncated"));
        }
        let words = read_u64(bytes, at) as usize;
        at += 8;
        if bytes.len() - at != words * 8 {
            return Err(IndexError::Format("overlay tombstone length mismatch"));
        }
        let dead: Vec<u64> = (0..words)
            .map(|w| read_u64(bytes, at + w * 8))
            .collect::<Vec<_>>();

        let id_space = base.base_len() as u64 + added_keys.len() as u64;
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

        Ok(Self {
            base,
            added,
            added_keys,
            dead,
            live,
        })
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

    /// Assemble a blob by hand, so a test can put in what `to_bytes` never would.
    fn craft(base: &StringIndex, additions: &[&[u8]], dead: &[u64]) -> Vec<u8> {
        let base = base.to_bytes();
        let mut out = b"OVL1".to_vec();
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

    #[test]
    fn a_malformed_blob_is_rejected_rather_than_half_loaded() {
        let base = StringIndex::build(["a", "b"]).unwrap();
        let good = craft(&base, &[b"c"], &[0b100]);
        assert!(Overlay::from_bytes_with(&good, StringIndex::from_bytes).is_ok());

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("bad overlay magic or truncated header", {
                let mut b = good.clone();
                b[0] = b'X';
                b
            }),
            ("bad overlay magic or truncated header", good[..12].to_vec()),
            ("overlay blob was written over a different base index", {
                let mut b = good.clone();
                b[4] = 99;
                b
            }),
            ("overlay base blob out of range", {
                let mut b = good.clone();
                b[5..13].copy_from_slice(&u64::MAX.to_le_bytes());
                b
            }),
            ("overlay addition out of range", {
                let mut b = craft(&base, &[b"c"], &[]);
                let at = b.len() - 8 - 4 - 1;
                b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
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
            ("overlay tombstones truncated", {
                let mut b = craft(&base, &[b"c"], &[]);
                b.truncate(b.len() - 4);
                b
            }),
            ("overlay additions truncated", {
                let mut b = craft(&base, &[b"c"], &[]);
                b.truncate(b.len() - 8 - 4 - 1 + 2);
                b
            }),
        ];
        for (expected, bytes) in cases {
            match Overlay::from_bytes_with(&bytes, StringIndex::from_bytes) {
                Err(IndexError::Format(msg)) => assert_eq!(msg, expected),
                other => panic!("expected {expected:?}, got {:?}", other.map(|o| o.len())),
            }
        }
    }

    #[cfg(all(feature = "mph", target_pointer_width = "64"))]
    #[test]
    fn a_blob_refuses_the_wrong_base_before_the_loader_runs() {
        use crate::PerfectHashIndex;
        let ov = Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        let blob = ov.to_bytes().unwrap();
        let mut called = false;
        let got = Overlay::<PerfectHashIndex>::from_bytes_with(&blob, |b| {
            called = true;
            // SAFETY: never reached -- the tag check rejects the blob first, which is the point.
            unsafe { PerfectHashIndex::from_bytes(b) }
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
    #[cfg(all(feature = "mph", target_pointer_width = "64"))]
    #[test]
    fn a_perfect_hash_overlay_round_trips_through_its_own_loader() {
        use crate::PerfectHashIndex;
        let mut ov = Overlay::new(PerfectHashIndex::build(["one", "two"]).unwrap());
        ov.add("three");
        assert!(ov.remove("two"));
        let blob = ov.to_bytes().unwrap();
        // SAFETY: the blob was produced by `to_bytes` in this process and has not left it.
        let back = Overlay::from_bytes_with(&blob, |b| unsafe { PerfectHashIndex::from_bytes(b) })
            .unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.id("two"), None);
        for k in ["one", "three"] {
            assert_eq!(back.key(back.id(k).unwrap()).as_deref(), Some(k));
        }
    }

    #[cfg(all(feature = "mph", target_pointer_width = "64"))]
    #[test]
    fn a_compact_hash_overlay_round_trips_its_membership() {
        use crate::CompactHashIndex;
        let mut ov = Overlay::new(CompactHashIndex::build(["alpha", "beta"], 4).unwrap());
        ov.add("gamma");
        assert!(ov.remove("alpha"));
        let blob = ov.to_bytes().unwrap();
        // SAFETY: the blob was produced by `to_bytes` in this process and has not left it.
        let back = Overlay::from_bytes_with(&blob, |b| unsafe { CompactHashIndex::from_bytes(b) })
            .unwrap();
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

    #[cfg(all(feature = "mph", target_pointer_width = "64"))]
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

    /// `CompactHashIndex` stores no keys, so an overlay over it has membership and nothing else —
    /// `key` and `compact` are not merely unimplemented but absent, since `OverlayKeys` is what
    /// provides them and it cannot be implemented without the keys.
    #[cfg(all(feature = "mph", target_pointer_width = "64"))]
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
}
