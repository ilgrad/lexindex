//! Minimal-perfect-hash dictionary.
//!
//! For a fixed set of `n` distinct strings, a minimal perfect hash maps each to a distinct slot in
//! `[0, n)` with no gaps and near-`O(1)` lookup in tiny space. [`crate::mphf::Mphf`] builds the MPH;
//! we key it on a deterministic 64-bit hash of each string (so queries take `&str` without
//! allocating) and keep a [`StringArena`] from slot → key. The arena doubles as a **membership
//! check**: an MPH maps *any* input to some slot, so a query is only a hit if the stored key at that
//! slot equals the query.
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
use crate::mphf::Mphf;

/// Every format before this one embedded `ptr_hash`'s `epserde` image, whose private fields no
/// loader could validate. 1.0 replaced the backend precisely so that a blob could be checked, and
/// the old images cannot be read without the crate that is now gone — so they are refused by name
/// rather than half-supported.
const LEGACY_MAGICS: [&[u8; 4]; 3] = [b"BMP2", b"BMP3", b"BMP4"];
const MAGIC_V5: &[u8; 4] = b"BMP5"; // [magic 4][n u64][mph_len u64][side_len u32][payload u64][check u32]
const HEADER_V5: usize = 36;
const CHECKED_V5: usize = 32;
const SIDE_ENTRY: usize = 12; // hash u64 + id u32
/// "No key assigned to this slot yet" while a build fills its slot → key-index table. Never a real
/// index: `build` rejects `n > u32::MAX`, so the largest index a key can have is `u32::MAX - 1`.
const NO_KEY: u32 = u32::MAX;

/// The v5 header, from the four values that vary. Shared by [`PerfectHashIndex::to_bytes`] and the
/// streaming [`build_to_file`](PerfectHashIndex::build_to_file), which assembles the same blob
/// without ever holding the index — one writer of this layout, so the two cannot drift.
fn header_bytes(n: usize, mph_len: usize, side_len: usize, payload: u64) -> [u8; HEADER_V5] {
    let mut header = [0u8; HEADER_V5];
    header[0..4].copy_from_slice(MAGIC_V5);
    header[4..12].copy_from_slice(&(n as u64).to_le_bytes());
    header[12..20].copy_from_slice(&(mph_len as u64).to_le_bytes());
    header[20..24].copy_from_slice(&(side_len as u32).to_le_bytes());
    header[24..32].copy_from_slice(&payload.to_le_bytes());
    let check = crate::hash::hash_bytes(&header[..CHECKED_V5]) as u32;
    header[CHECKED_V5..].copy_from_slice(&check.to_le_bytes());
    header
}

/// Header + owned sections (MPH buffer, side buffer) of a serialised blob.
type SerialisedParts = ([u8; HEADER_V5], Vec<u8>, Vec<u8>);

/// The largest slice of the arena that may be dirty at once during a streamed build.
///
/// The streamed build writes each key at its perfect-hash slot, which is random with respect to
/// file offset, so a direct fill dirties 4 KB pages across the whole mapping for the whole pass. A
/// page holds ~178 keys at typical lengths, so once writeback starts cleaning pages the pass keeps
/// re-dirtying them and the build writes its own file dozens of times over — measured 55-91x the
/// output size and 2.7x the wall time from 5 M keys up, and at 100 M it did not finish in fifty
/// minutes. Confining the dirty set to one window fixes it. 32 MB is the largest window that held
/// 1.0x reproducibly on a real filesystem; 64 MB measured 1.4-2.3x and 128 MB 2.9-23x.
#[cfg(feature = "mmap")]
const SPILL_WINDOW: usize = 32 << 20;

/// How much a window accumulates before its buffer is written out. Buffers grow on demand, so this
/// is an upper bound per window rather than an up-front cost — 4.7 MB for a 2.3 GB arena. It is
/// also why the spill is one file written at computed offsets rather than one file per window: a
/// file per window would be a file descriptor per window.
#[cfg(feature = "mmap")]
const SPILL_BUF: usize = 64 << 10;

/// Bytes of slot tag in front of each spilled key. The key's *length* is not stored: the arena
/// prefix already knows it, so the spill costs four bytes per key and nothing else.
#[cfg(feature = "mmap")]
const SLOT_TAG: usize = 4;

/// Process-wide counter for spill names, so two threads building to the same directory cannot
/// collide on one.
#[cfg(feature = "mmap")]
static SPILL_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Where the arena sits in the output being written and how its offsets are encoded.
///
/// Grouped rather than passed as four positional arguments, and it owns `span` so the arena fill
/// and the duplicate check cannot disagree about where a slot lives.
#[cfg(feature = "mmap")]
struct ArenaLayout<'a> {
    prefix: &'a [u8],
    width: usize,
    data_start: usize,
    data_len: usize,
    /// [`SPILL_WINDOW`] in every build; a handful of bytes in the tests, so the windowed fill is
    /// exercised by the same code path rather than by a `cfg(test)` copy of it.
    window: usize,
}

#[cfg(feature = "mmap")]
impl ArenaLayout<'_> {
    /// The byte range slot `slot` occupies in the file.
    fn span(&self, slot: usize) -> std::ops::Range<usize> {
        let lo = self.data_start + StringArena::offset_at(self.prefix, self.width, slot) as usize;
        let hi =
            self.data_start + StringArena::offset_at(self.prefix, self.width, slot + 1) as usize;
        lo..hi
    }

    /// Which window slot `slot` is filled in. Clamped because a zero-length key at the very end of
    /// the arena starts exactly at `data_len`, one past the last window.
    fn window_of(&self, slot: usize, windows: usize) -> usize {
        let off = StringArena::offset_at(self.prefix, self.width, slot) as usize;
        (off / self.window).min(windows - 1)
    }
}

/// What both replay passes say when pass two disagrees with pass one.
#[cfg(feature = "mmap")]
const REPLAY_MISMATCH: &str =
    "perfect-hash: the source did not replay the same keys in the same order";

/// Where pass two may write the key at `slot`, once it has shown itself to be the key pass one put
/// there.
///
/// The hash answers "same key" only probabilistically — two strings can share a 64-bit hash — so
/// the slot's own length is checked as well. It is free: pass one sized the slot from that key's
/// length. Without it an equal-hash key of a different length reaches `copy_from_slice` with
/// mismatched lengths, which panics, or writes a short record into the spill, whose framing is read
/// back by the arena's lengths and would silently desynchronise from there on.
#[cfg(feature = "mmap")]
fn replay_span(
    layout: &ArenaLayout<'_>,
    expected_hash: u64,
    slot: u32,
    key: &str,
) -> Result<std::ops::Range<usize>, IndexError> {
    if hash_key(key) != expected_hash {
        return Err(IndexError::Build(REPLAY_MISMATCH));
    }
    let span = layout.span(slot as usize);
    if key.len() != span.len() {
        return Err(IndexError::Build(REPLAY_MISMATCH));
    }
    Ok(span)
}

/// A sibling temporary holding pass two's records until each window can be scattered.
///
/// Opened `O_CREAT|O_EXCL` under a pid-and-counter name for the same reason the output temporary
/// is — the name is predictable, and a planted symlink at it must not be followed — and removed on
/// every exit path, error paths included, by `Drop`. A hard kill leaks it, exactly as it leaks the
/// output temporary.
#[cfg(feature = "mmap")]
struct Spill {
    path: std::path::PathBuf,
    file: std::fs::File,
}

#[cfg(feature = "mmap")]
impl Spill {
    fn create(target: &std::path::Path) -> Result<Self, IndexError> {
        let dir = target.parent().unwrap_or_else(|| std::path::Path::new("."));
        let stem = target
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("index"));
        for _ in 0..128 {
            let seq = SPILL_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut path = dir.join(stem);
            path.as_mut_os_string()
                .push(format!(".{}.{seq}.spill", std::process::id()));
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok(Self { path, file }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(IndexError::Io(std::io::Error::from(
            std::io::ErrorKind::AlreadyExists,
        )))
    }

    fn write_at(&mut self, at: usize, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        self.file.seek(std::io::SeekFrom::Start(at as u64))?;
        self.file.write_all(bytes)
    }

    fn reader_at(&mut self, at: usize) -> std::io::Result<std::io::BufReader<&std::fs::File>> {
        use std::io::Seek;
        self.file.seek(std::io::SeekFrom::Start(at as u64))?;
        Ok(std::io::BufReader::with_capacity(SPILL_BUF, &self.file))
    }
}

#[cfg(feature = "mmap")]
impl Drop for Spill {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

/// Fill the arena from a second pass over `source`, never leaving more than one [`SPILL_WINDOW`]
/// dirty.
///
/// Pass two appends each key to the spill region of the window its slot falls in — all sequential
/// writes — and each window is then read back sequentially and scattered inside itself, small
/// enough to stay dirty until it is flushed. The extra disk traffic is one arena-sized write plus
/// one read; the alternative of re-reading the source once per window was measured and rejected,
/// because one pass over a 100 M source costs 33 s and the windows number in the dozens.
///
/// Returns how many keys the source produced, so the caller reports a short replay the same way it
/// does for the direct fill.
#[cfg(feature = "mmap")]
fn fill_arena_windowed<F, I, S>(
    map: &mut memmap2::MmapMut,
    layout: &ArenaLayout<'_>,
    hashes: &[u64],
    slot_of: &[u32],
    source: &mut F,
    target: &std::path::Path,
) -> Result<usize, IndexError>
where
    F: FnMut() -> I,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    use std::io::Read;

    let n = hashes.len();
    let windows = layout.data_len.div_ceil(layout.window);
    // Every region's size is known before a single key is read back: the arena prefix already says
    // where each slot lands and how long it is, so the spill can be one file at computed offsets
    // instead of a file per window.
    let mut region_len = vec![0usize; windows];
    for slot in 0..n {
        region_len[layout.window_of(slot, windows)] += SLOT_TAG + layout.span(slot).len();
    }
    let mut region_at = Vec::with_capacity(windows);
    let mut acc = 0usize;
    for &len in &region_len {
        region_at.push(acc);
        acc += len;
    }

    let mut spill = Spill::create(target)?;
    let mut buf: Vec<Vec<u8>> = vec![Vec::new(); windows];
    let mut cursor = vec![0usize; windows];
    let mut i = 0usize;
    for item in source() {
        let key = item.as_ref();
        if i >= n {
            return Err(IndexError::Build(REPLAY_MISMATCH));
        }
        let slot = slot_of[i];
        replay_span(layout, hashes[i], slot, key)?;
        let w = layout.window_of(slot as usize, windows);
        let out = &mut buf[w];
        out.extend_from_slice(&slot.to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        if out.len() >= SPILL_BUF {
            spill.write_at(region_at[w] + cursor[w], out)?;
            cursor[w] += out.len();
            out.clear();
        }
        i += 1;
    }
    if i != n {
        return Ok(i);
    }
    for (w, out) in buf.iter().enumerate() {
        if !out.is_empty() {
            spill.write_at(region_at[w] + cursor[w], out)?;
        }
    }
    drop(buf);

    for w in 0..windows {
        let mut left = region_len[w];
        let mut reader = spill.reader_at(region_at[w])?;
        let mut tag = [0u8; SLOT_TAG];
        while left > 0 {
            reader.read_exact(&mut tag)?;
            let span = layout.span(u32::from_le_bytes(tag) as usize);
            left -= SLOT_TAG + span.len();
            reader.read_exact(&mut map[span])?;
        }
        let from = layout.data_start + w * layout.window;
        let len = layout.data_len.min((w + 1) * layout.window) - w * layout.window;
        map.flush_range(from, len)?;
    }
    Ok(n)
}

/// The validated framing of a blob — every field a query will trust — with the MPH region located
/// but not parsed. Produced by `parse_frame` and consumed by `from_shared`; both are safe, because
/// the MPH region validates itself (see [`Mphf::from_bytes`]).
struct Frame {
    n: usize,
    mph: std::ops::Range<usize>, // the `MPH1` region; ignored when `n == 0`
    arena: StringArena,
    side: Vec<(u64, u32)>,
}

/// An immutable minimal-perfect-hash dictionary: fastest exact `string → dense id` with reverse lookup.
pub struct PerfectHashIndex {
    mph: Option<Mphf>,  // over one hash per distinct hash value; None iff empty
    arena: StringArena, // id → key (also verifies membership); ids [m, n) are the side keys
    n: usize,
    // (hash, id) for every key whose hash collides with another key's, sorted; almost always empty.
    side: Vec<(u64, u32)>,
}

impl PerfectHashIndex {
    /// Build from a collection of strings. Duplicates are removed; ids are arbitrary slots in `[0, n)`
    /// (no defined order — use [`crate::StringIndex`] when order matters).
    ///
    /// Ids are **reproducible**: the same key set always produces the same blob, byte for byte,
    /// on any thread count. They are still arbitrary — nothing about a key predicts its id — and
    /// they change whenever the key set does, so persist the blob rather than re-deriving it if an
    /// id is stored outside the index.
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
        let mph = Mphf::build(&sorted)?;
        let mut by_slot: Vec<u32> = vec![NO_KEY; n];
        for (i, h) in hashes.iter().enumerate() {
            let slot = mph.index(*h) as usize;
            if slot >= n || by_slot[slot] != NO_KEY {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = i as u32;
        }
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
        let mph = Mphf::build(&mph_hashes)?;
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
            let slot = mph.index(*h) as usize;
            if slot >= m || by_slot[slot] != NO_KEY {
                return Err(IndexError::Format(
                    "perfect-hash: construction was not minimal/perfect",
                ));
            }
            by_slot[slot] = i as u32;
        }
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
            side,
        })
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

    /// Slot for a key hash; `None` only for an empty index. The MPH's remap covers every slot it
    /// can produce, so the answer is always a valid arena row and membership is decided by the
    /// stored key alone.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        Some(self.mph.as_ref()?.index(h) as usize)
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
    /// than the prefetching. See [`crate::CompactHashIndex::ids_of`] for the same measurement in more
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
        // Every slot is a real arena row — the MPH's remap covers its whole slot range — so the
        // three passes are just a pipeline: pilots prefetched inside `index_all`, then the arena's
        // offset lines prefetched ahead of the span pass, then its data lines ahead of the compare.
        let slots = mph.index_all(&hashes);
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(slots.len());
        for (i, &slot) in slots.iter().enumerate() {
            if let Some(&s) = slots.get(i + AHEAD) {
                self.arena.prefetch_offsets(s as usize);
            }
            spans.push(self.arena.span(slot as usize));
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
        let mph_buf = match &self.mph {
            Some(mph) => mph.to_bytes(),
            None => Vec::new(),
        };
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

    /// Serialise to a self-describing blob: `[magic "BMP5"][n u64][mph_len u64][side_len u32]
    /// [payload u64][check u32][MPH1 blob][arena bytes][side entries]`. Reloading queries correctly
    /// because the key hash is version-stable. `check` is a hash of the preceding header bytes and
    /// `payload` a streaming hash of everything after the header, so a blob that lost bytes in
    /// transit fails cleanly at load; the MPH region carries its own header and validates its own
    /// lengths, which is what makes [`from_bytes`](Self::from_bytes) a safe fn.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        let arena = self.arena.as_bytes();
        let mut out = Vec::with_capacity(HEADER_V5 + mph_buf.len() + arena.len() + side_buf.len());
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
            Some(mph) => mph.byte_len(),
            None => 0,
        };
        Ok(HEADER_V5 + mph + self.arena.as_bytes().len() + self.side.len() * SIDE_ENTRY)
    }

    /// Reconstruct from [`PerfectHashIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes. Every array the index will read is bounded by a length this crate
    /// wrote and checks here — the framing (magic, lengths, arena offsets, side ids) and the MPH's
    /// own header alike — so a crafted blob is at worst *wrong*, never unsound. Owned loads also
    /// verify a streaming checksum of the whole payload, which is what turns accidental corruption
    /// into a clean error rather than a wrong answer.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true)
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
        if LEGACY_MAGICS.contains(&<&[u8; 4]>::try_from(&bytes[0..4]).expect("4 bytes")) {
            return Err(IndexError::Format(
                "perfect-hash: blob written by lexindex < 1.0, whose minimal perfect hash came \
                 from a crate this version no longer links; rebuild the index from its keys",
            ));
        }
        if &bytes[0..4] != MAGIC_V5 || bytes.len() < HEADER_V5 {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED_V5..HEADER_V5].try_into().unwrap());
        if check != crate::hash::hash_bytes(&bytes[..CHECKED_V5]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let side_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
        // Owned loads verify the whole payload — one streaming pass over everything after the
        // header — so a flipped byte in the MPH region, the arena or the side table is rejected
        // here rather than surfacing as a wrong answer later.
        if verify {
            let stored = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
            if stored != crate::hash::hash_block(&bytes[HEADER_V5..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
        }
        let (header, len_at) = (HEADER_V5, 12);
        let n64 = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
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
    /// [`parse_frame`](Self::parse_frame), then the MPH structure (a few bytes/key) copied into
    /// owned memory; the key arena — the bulk of the blob — is borrowed zero-copy, so a
    /// memory-mapped load never copies it. Backs `from_bytes`, `load` and `load_mmap`.
    fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
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
            let mph = Mphf::from_bytes(&blob.as_ref()[mph])?;
            if mph.n() != m as u64 {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        Ok(Self {
            mph,
            arena,
            n,
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
    /// in memory. Returns the number of keys written.
    ///
    /// The file is a blob that answers exactly as [`build`](Self::build) + [`save`](Self::save)
    /// would have for the same key set — every key a member, every `key(id)` round trip intact —
    /// but **not necessarily the same bytes**: the arena's offset width is chosen from the key
    /// lengths this pass sees, and a streamed build knows them before it has the keys. The MPH
    /// itself is deterministic, so the ids agree.
    ///
    /// `source` is a *factory*, not an iterator, and it is called **twice**. That is the shape the
    /// problem has, not an inconvenience: the arena stores keys in slot order, slot order is only
    /// known once the perfect hash is built, and the perfect hash needs every key's hash first.
    /// Pass one hashes the corpus and records lengths (12 bytes per key, nothing else); the offset
    /// table follows from the lengths alone; pass two replays the corpus and places each key. A
    /// one-shot iterator cannot be passed by construction, which is the point — the signature
    /// states the requirement instead of documenting it.
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
    ///
    /// **Transient disk space**: an output whose key arena exceeds 32 MB is filled through a spill
    /// file alongside it, so the build needs roughly 2.2x the output size free in the target
    /// directory until it finishes. Filling the arena directly instead is what made the build write
    /// its file dozens of times over — the arena is filled in perfect-hash slot order, which is
    /// random with respect to file offset, so the whole mapping stays dirty for the whole pass and
    /// the kernel writes it back over and over. The spill is written sequentially,
    /// removed on every exit path, and on a machine with memory to spare it never reaches the disk
    /// at all.
    #[cfg(feature = "mmap")]
    pub fn build_to_file<F, I, S>(
        path: impl AsRef<std::path::Path>,
        source: F,
    ) -> Result<usize, IndexError>
    where
        F: FnMut() -> I,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_checked(path, source, || Ok(()))
    }

    /// [`build_to_file`](Self::build_to_file) with a last word from the caller, asked after each
    /// pass over the source — the second time **inside** the atomic write, before the rename that
    /// publishes the file.
    ///
    /// It exists for a source that cannot report failure through its iterator: the Python binding
    /// adapts an arbitrary iterable, and one that raises halfway simply stops. Without this hook a
    /// deterministic failure would stop both passes at the same key, the two passes would agree,
    /// and a truncated index would be renamed over whatever was at `path`. Returning `Err` after
    /// pass one abandons the build before the perfect hash is even built; after pass two it aborts
    /// the write with the temporary removed and the target untouched.
    #[cfg(feature = "mmap")]
    pub(crate) fn build_to_file_checked<F, I, S, C>(
        path: impl AsRef<std::path::Path>,
        source: F,
        check: C,
    ) -> Result<usize, IndexError>
    where
        F: FnMut() -> I,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        Self::build_to_file_windowed(path, source, check, SPILL_WINDOW)
    }

    /// [`build_to_file_checked`](Self::build_to_file_checked) with the arena window size exposed.
    ///
    /// Only the tests pass anything but [`SPILL_WINDOW`]: an arena large enough to need more than
    /// one 32 MB window is far too large to build in a unit test, and a windowing bug that only
    /// appears past 32 MB of keys is exactly the kind this has to catch.
    #[cfg(feature = "mmap")]
    fn build_to_file_windowed<F, I, S, C>(
        path: impl AsRef<std::path::Path>,
        mut source: F,
        mut check: C,
        window: usize,
    ) -> Result<usize, IndexError>
    where
        F: FnMut() -> I,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
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
        check()?;
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
        let mph = Mphf::build(&mph_hashes)?;

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
            let slot = mph.index(*h) as usize;
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
        let mph_buf = mph.to_bytes();
        drop(mph);

        let arena_start = HEADER_V5 + mph_buf.len();
        let data_start = arena_start + arena_prefix.len();
        let total = data_start + data_len + side_buf.len();
        crate::blob::write_atomically_with(path.as_ref(), |w| {
            let file: &mut std::fs::File = w.get_mut();
            file.set_len(total as u64)?;
            // SAFETY: the file was created exclusively by `write_atomically_with` under a name no
            // other process knows yet, and nothing else touches it until the rename below.
            let mut map = unsafe { memmap2::MmapMut::map_mut(&*file)? };
            map[HEADER_V5..arena_start].copy_from_slice(&mph_buf);
            map[arena_start..data_start].copy_from_slice(&arena_prefix);
            map[data_start + data_len..].copy_from_slice(&side_buf);

            let layout = ArenaLayout {
                prefix: &arena_prefix,
                width,
                data_start,
                data_len,
                window,
            };
            // An arena that fits in one window is already confined, so it is filled straight
            // through the mapping and no temporary is created; past that the fill goes through a
            // spill so the dirty set stays one window wide. See [`SPILL_WINDOW`] for the
            // measurements behind the threshold.
            let i = if data_len <= window {
                let mut i = 0usize;
                for item in source() {
                    let key = item.as_ref();
                    if i >= n {
                        return Err(IndexError::Build(REPLAY_MISMATCH));
                    }
                    let span = replay_span(&layout, hashes[i], slot_of[i], key)?;
                    map[span].copy_from_slice(key.as_bytes());
                    i += 1;
                }
                i
            } else {
                fill_arena_windowed(
                    &mut map,
                    &layout,
                    &hashes,
                    &slot_of,
                    &mut source,
                    path.as_ref(),
                )?
            };
            if i != n {
                return Err(IndexError::Build(
                    "perfect-hash: the source did not replay the same keys in the same order",
                ));
            }
            // Every extra shares a hash with its representative. Distinct keys make that a genuine
            // 64-bit collision, which the side table handles; equal keys mean the caller broke the
            // one precondition this build has, and the file must not be published.
            for &(h, i) in &extras {
                let rep = rep_of[&h] as usize;
                if map[layout.span(slot_of[i as usize] as usize)]
                    == map[layout.span(slot_of[rep] as usize)]
                {
                    return Err(IndexError::Build(
                        "perfect-hash: build_to_file needs distinct keys and the source repeated one",
                    ));
                }
            }

            check()?;

            let mut payload = crate::hash::BlockHasher::new();
            payload.update(&map[HEADER_V5..]);
            let header = header_bytes(n, mph_buf.len(), side.len(), payload.finish());
            map[..HEADER_V5].copy_from_slice(&header);
            map.flush()?;
            Ok(())
        })?;
        Ok(n)
    }

    /// Load a dictionary previously written with [`PerfectHashIndex::save`] (reads the whole file
    /// and verifies the payload checksum). Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and borrow the key arena (the bulk of the blob) zero-copy; only the small
    /// MPH structure is read into memory. Skips the payload-checksum scan `load` performs — the
    /// mapped file is trusted intact.
    ///
    /// # Safety
    /// One obligation, and it is not about the bytes: the file must not be modified or truncated by
    /// any process while the returned index is alive, because the index borrows the mapping. A
    /// crafted file is *not* undefined behaviour here — the same validation
    /// [`from_bytes`](Self::from_bytes) performs runs on the mapping — it is merely wrong. See
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
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

        // Not compared byte for byte against `build` + `save`: the streamed build picks the
        // arena's offset width from the lengths it saw in pass one, which need not match. What
        // must hold is that the file is a valid blob answering exactly like the in-memory index.
        let idx = PerfectHashIndex::load(&path).unwrap();
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

    /// `window_of` is asked for a slot whose offset is exactly `data_len`, which is what a
    /// zero-length key in the last arena position produces; without the clamp that indexes one
    /// past the last window. The public build cannot be steered into it — which slot the empty key
    /// lands in is the perfect hash's choice — so the invariant is pinned directly.
    #[test]
    fn window_of_clamps_a_zero_length_key_at_the_end_of_the_arena() {
        let (prefix, data_len, width) = StringArena::prefix_for_lengths(&[4, 4, 0]);
        assert_eq!(data_len, 8);
        let layout = ArenaLayout {
            prefix: &prefix,
            width,
            data_start: 0,
            data_len,
            window: 4,
        };
        let windows = data_len.div_ceil(layout.window);
        assert_eq!(windows, 2);
        assert_eq!(layout.window_of(2, windows), windows - 1);
        assert!(layout.span(2).is_empty());
    }

    /// A 64-bit hash collision cannot be found in a test, so the collision is supplied instead:
    /// `replay_span` is handed the hash of the key it is about to see, which is exactly what a
    /// colliding second pass would produce, over a slot pass one sized for a longer key. Both fill
    /// paths go through this function, so this pins the invariant for both. Without the length
    /// check the direct path would panic inside `copy_from_slice` and the windowed path would write
    /// a short record and desynchronise the spill.
    #[test]
    fn a_replayed_key_of_the_wrong_length_is_refused_even_when_the_hash_matches() {
        let (prefix, data_len, width) = StringArena::prefix_for_lengths(&[3]);
        let layout = ArenaLayout {
            prefix: &prefix,
            width,
            data_start: 0,
            data_len,
            window: 64,
        };
        assert_eq!(layout.span(0).len(), 3);

        // The hash matches by construction; only the length differs.
        let short = "b";
        match replay_span(&layout, hash_key(short), 0, short) {
            Err(IndexError::Build(msg)) => assert_eq!(msg, REPLAY_MISMATCH),
            other => panic!("expected a replay mismatch, got {other:?}"),
        }
        // The same length, the same hash: this is the ordinary path and it yields the slot.
        assert_eq!(
            replay_span(&layout, hash_key("abc"), 0, "abc").unwrap(),
            0..3
        );
        // A key whose hash does not match is refused before the length is ever consulted.
        assert!(replay_span(&layout, hash_key("zzz"), 0, "abc").is_err());
    }

    /// Nothing in a unit test can build a 32 MB arena, so the window is shrunk instead and the
    /// production path runs unchanged over hundreds of windows.
    #[test]
    fn build_to_file_windowed_answers_exactly_like_the_single_window_build() {
        let keys: Vec<String> = (0..2_000)
            .map(|i| format!("key-{i}-{}", "x".repeat(i % 17)))
            .collect();
        let wide = tmp("win_wide.bmp");
        let narrow = tmp("win_narrow.bmp");
        PerfectHashIndex::build_to_file(&wide, || keys.iter()).unwrap();
        let n = PerfectHashIndex::build_to_file_windowed(&narrow, || keys.iter(), || Ok(()), 64)
            .unwrap();
        assert_eq!(n, keys.len());

        let (a, b) = (
            PerfectHashIndex::load(&wide).unwrap(),
            PerfectHashIndex::load(&narrow).unwrap(),
        );
        assert_eq!(b.len(), keys.len());
        let mut ids: Vec<u32> = Vec::with_capacity(keys.len());
        for k in &keys {
            // Two builds over the same keys agree exactly, but this pair has different arena
            // widths, so assert only what the widths cannot change.
            assert!(a.contains(k) && b.contains(k), "{k}");
            let id = b.id(k).expect("every key is a member");
            assert_eq!(b.key(id), Some(k.as_str()));
            ids.push(id);
        }
        ids.sort_unstable();
        assert!(
            ids.iter().copied().eq(0..keys.len() as u32),
            "ids are exactly the dense range [0, n)"
        );
        assert_eq!(b.id("not-a-key"), None);
        std::fs::remove_dir_all(wide.parent().unwrap()).ok();
        std::fs::remove_dir_all(narrow.parent().unwrap()).ok();
    }

    /// An empty key at the end of the arena starts exactly at `data_len`, one past the last
    /// window, which is why `window_of` clamps. Keys of length zero and one also make the windows
    /// land mid-key, so a record can straddle a boundary.
    #[test]
    fn build_to_file_windowed_handles_empty_and_boundary_keys() {
        let mut keys: Vec<String> = (0..200).map(|i| format!("{i:03}")).collect();
        keys.push(String::new());
        keys.push("z".to_string());
        let path = tmp("win_edge.bmp");
        for window in [1usize, 2, 3, 7, 64] {
            PerfectHashIndex::build_to_file_windowed(&path, || keys.iter(), || Ok(()), window)
                .unwrap();
            // SAFETY: written by this crate a line above.
            let idx = PerfectHashIndex::load(&path).unwrap();
            for k in &keys {
                let id = idx
                    .id(k)
                    .unwrap_or_else(|| panic!("{k:?} at window {window}"));
                assert_eq!(idx.key(id), Some(k.as_str()), "window {window}");
            }
            assert_eq!(idx.id("nope"), None);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// The window buffers flush mid-pass once a window has accumulated `SPILL_BUF`, and that is
    /// the branch every real build spends its time in while the small-window tests above never
    /// reach it. Enough keys to overflow a window several times, and a window large enough that
    /// they land in only a few.
    #[test]
    fn build_to_file_windowed_flushes_buffers_mid_pass() {
        let keys: Vec<String> = (0..10_000)
            .map(|i| format!("key-{i:06}-{}", "y".repeat(i % 23)))
            .collect();
        let direct = tmp("win_flush_direct.bmp");
        let windowed = tmp("win_flush_windowed.bmp");
        PerfectHashIndex::build_to_file(&direct, || keys.iter()).unwrap();
        PerfectHashIndex::build_to_file_windowed(&windowed, || keys.iter(), || Ok(()), 128 << 10)
            .unwrap();
        let (a, b) = (
            PerfectHashIndex::load(&direct).unwrap(),
            PerfectHashIndex::load(&windowed).unwrap(),
        );
        for k in &keys {
            assert!(a.contains(k), "{k}");
            let id = b
                .id(k)
                .unwrap_or_else(|| panic!("{k} lost by the windowed fill"));
            assert_eq!(b.key(id), Some(k.as_str()));
        }
        std::fs::remove_dir_all(direct.parent().unwrap()).ok();
        std::fs::remove_dir_all(windowed.parent().unwrap()).ok();
    }

    /// A second pass that simply *stops early* is the failure the replay check cannot see key by
    /// key — every key it did produce matched — so the windowed fill reports the count back and
    /// the caller refuses on the total.
    #[test]
    fn build_to_file_windowed_refuses_a_source_that_replays_short() {
        let keys: Vec<String> = (0..300).map(|i| format!("k{i:04}")).collect();
        let path = tmp("win_short.bmp");
        let dir = path.parent().unwrap().to_path_buf();
        let mut pass = 0;
        let err = PerfectHashIndex::build_to_file_windowed(
            &path,
            || {
                pass += 1;
                let take = if pass > 1 { keys.len() - 5 } else { keys.len() };
                keys[..take].to_vec()
            },
            || Ok(()),
            16,
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::Build(_)), "{err}");
        assert!(!path.exists(), "a short replay must not publish a file");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".spill") || n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The spill is a second temporary next to the target, so it gets the same guarantee the
    /// output temporary already has: gone on success, and gone when the build refuses.
    #[test]
    fn build_to_file_windowed_leaves_no_spill_behind() {
        let keys: Vec<String> = (0..300).map(|i| format!("k{i:04}")).collect();
        let path = tmp("win_spill.bmp");
        let dir = path.parent().unwrap().to_path_buf();
        PerfectHashIndex::build_to_file_windowed(&path, || keys.iter(), || Ok(()), 16).unwrap();

        // A source that replays a different key set: pass two fails after the spill exists.
        let mut pass = 0;
        let err = PerfectHashIndex::build_to_file_windowed(
            &path,
            || {
                pass += 1;
                let mut k = keys.clone();
                if pass > 1 {
                    k[7] = "different".to_string();
                }
                k
            },
            || Ok(()),
            16,
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::Build(_)), "{err}");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".spill") || name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_to_file_matches_the_in_memory_index_key_for_key() {
        let keys: Vec<String> = (0..500).map(|i| format!("k{i:04}")).collect();
        let path = tmp("cross.bmp");
        PerfectHashIndex::build_to_file(&path, || keys.iter()).unwrap();
        // SAFETY: written by this crate a line above.
        let file = PerfectHashIndex::load(&path).unwrap();
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
        assert_eq!(PerfectHashIndex::load(&path).unwrap().len(), 0);
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

    /// Shorthand: the loader is safe now, and every blob below is either produced by this crate
    /// or a deliberate corruption of one.
    fn from_bytes(bytes: &[u8]) -> Result<PerfectHashIndex, IndexError> {
        PerfectHashIndex::from_bytes(bytes)
    }

    /// Arbitrary bytes must never panic — only `Ok`/`Err`. The whole loader has this property
    /// since 1.0; `parse_frame` is fuzzed separately because it is the half a libFuzzer target can
    /// reach without building an index first.
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
    /// payload checksum — so the MPH region is never reached on a short read.
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

    /// `[0, 0)` has no inhabitant, so the MPH has no table and `Mphf::index` would panic on one.
    /// Every query path has to notice that before it asks — including the batch, which is the one
    /// that allocates an answer per key.
    #[test]
    fn empty_dictionary() {
        let idx = PerfectHashIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(idx.id("x"), None);
        assert_eq!(idx.key(0), None);
        assert_eq!(idx.id_unchecked("x"), 0);
        assert!(!idx.contains("x"));
        assert_eq!(idx.ids_of(&["x", "y"]), vec![None, None]);
        assert_eq!(
            from_bytes(&idx.to_bytes().unwrap()).unwrap().ids_of(&["x"]),
            vec![None]
        );
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
    ///
    /// Every pre-1.0 blob embedded a `ptr_hash` image this crate can no longer read. The refusal
    /// has to *name* that — a bare "bad magic" would send someone hunting for a corrupt file when
    /// the file is intact and merely old.
    #[test]
    fn a_pre_1_0_blob_is_refused_by_name() {
        let idx = PerfectHashIndex::build(["alpha", "beta", "gamma"]).unwrap();
        let good = idx.to_bytes().unwrap();
        for magic in LEGACY_MAGICS {
            let mut old = good.clone();
            old[0..4].copy_from_slice(magic);
            let err = match from_bytes(&old) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{} was accepted", std::str::from_utf8(magic).unwrap()),
            };
            assert!(err.contains("lexindex < 1.0"), "{err}");
            assert!(err.contains("rebuild"), "{err}");
        }
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
        for pos in (HEADER_V5..good.len()).step_by(7) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "payload byte {pos} was accepted");
        }
    }

    /// A non-member is a valid input to a minimal perfect hash — it simply lands on some other
    /// key's slot. Every path must therefore stay inside the arena for a stranger, and the two
    /// membership-verifying paths must agree that it is absent.
    #[test]
    fn strangers_land_on_a_real_row_and_are_rejected() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = PerfectHashIndex::build(&members).unwrap();
        let strangers: Vec<String> = (0..20_000).map(|i| format!("stranger-{i:05}")).collect();
        for s in &strangers {
            assert!((idx.id_unchecked(s) as usize) < idx.len());
            assert_eq!(idx.id(s), None);
        }
        assert_eq!(idx.ids_of(&strangers), vec![None; strangers.len()]);
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
        // The blob carries the side section and reloads identically — owned and mapped.
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
            let payload = crate::hash::hash_block(&bad[HEADER_V5..]);
            bad[24..32].copy_from_slice(&payload.to_le_bytes());
            let check = crate::hash::hash_bytes(&bad[..CHECKED_V5]) as u32;
            bad[CHECKED_V5..HEADER_V5].copy_from_slice(&check.to_le_bytes());
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

    /// Construction is deterministic, and that is a promise `build`'s docstring now makes: the
    /// same keys must produce the same blob, byte for byte, however many threads the machine has.
    #[test]
    fn the_same_keys_always_produce_the_same_blob() {
        let words: Vec<String> = (0..50_000).map(|i| format!("word-{i:05}")).collect();
        let first = PerfectHashIndex::build(&words).unwrap().to_bytes().unwrap();
        let again = PerfectHashIndex::build(&words).unwrap().to_bytes().unwrap();
        assert_eq!(first, again);
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
