//! A hash sidecar over a [`DictIndex`]: `id(key)` from a minimal perfect hash and a table of ranks,
//! the ordered queries from the dictionary it rides on.
//!
//! A [`DictIndex`] answers `id` by searching: its block samples, a walk of one block's restarts,
//! then a scan of one microblock, a few thousand instructions in all. [`HashedDictIndex`] keeps
//! the dictionary whole and stores beside it, at each key's slot in a minimal perfect hash, the
//! key's rank and `fingerprint_bits` of a second hash. Its `id` is one hash and one read of each
//! table, about a hundred instructions, and never touches the dictionary; `key`, `prefix`,
//! `lower_bound` and iteration go to [`dict`](HashedDictIndex::dict), whose ids are the same ranks.
//!
//! The sidecar costs `⌈log2 n⌉ + fingerprint_bits` bits a key and about two for the perfect hash:
//! closed, 2.7 bytes a key at a million keys and 3.2 at ten million, and `fingerprint_bits / 8`
//! more for membership. What that width decides is documented on [`HashedDictIndex`].

use std::sync::Arc;

use crate::blob::SharedBytes;
use crate::compact_hash::{NO_SLOT, fp_mask, read_fp, write_fp};
use crate::hash::{fingerprint_full_bytes, hash_key_bytes, hash_pair_bytes};
use crate::mphf::Mphf;
use crate::pages::Pages;
use crate::{DictIndex, IndexError};

/// `[magic 4][n u64][fp_bits u32][dict_len u64][mph_len u64][side_len u32][payload u64][check u32]`,
/// then the embedded `BDX3` blob, the MPH blob, the bit-packed rank table and the side table.
const MAGIC: &[u8; 4] = b"BHD1";
const HEADER: usize = 48;
const CHECKED: usize = 44; // header bytes the trailing check covers
const SIDE_ENTRY: usize = 24; // hash u64 + full second hash u64 + rank u64
/// The widest fingerprint. With at most `u32::MAX` keys a rank takes at most 32 bits, so a rank
/// and its fingerprint always fit the 64 bits one read returns.
const MAX_FP_BITS: u32 = 32;

/// A [`DictIndex`] with a hash sidecar answering `id(key)`: the dictionary's ranks and ordered
/// queries, at a hash index's lookup cost.
///
/// `key`, `prefix`, `lower_bound` and iteration go to [`dict`](Self::dict), whose ids are the same
/// ranks. The membership contract is chosen once, at build:
///
/// - **`fingerprint_bits >= 1`**: [`id`](Self::id) answers a non-member with `Some` at a rate of
///   `2^-fingerprint_bits`, as [`CompactHashIndex::id`](crate::CompactHashIndex::id) does. A member
///   always gets its own rank.
/// - **`fingerprint_bits == 0`**: `id` is the dictionary's own search: exact, at the dictionary's
///   cost. The smallest sidecar, for a caller whose hot path is
///   [`id_unchecked`](Self::id_unchecked).
///
/// [`id_unchecked`](Self::id_unchecked) is the closed path at any width: a member's rank, and some
/// rank below `len()` for anything else.
pub struct HashedDictIndex {
    // Shared rather than owned so the Python binding can hand the dictionary out as a
    // `DictIndex` object of its own without a copy.
    dict: Arc<DictIndex>,
    mph: Option<Mphf>, // over the distinct key hashes; None iff empty
    // One value of `width` bits a slot, bit-packed: the rank of the slot's key above `fp_bits` of
    // its second hash.
    ranks: SharedBytes,
    fp_bits: u32, // 0..=MAX_FP_BITS
    width: u32,   // rank_bits(n) + fp_bits, at most 64
    n: usize,
    // (hash, full second hash, rank) of every key whose 64-bit hash a key of lower rank shares,
    // sorted; almost always empty. The lowest-ranked key of such a group owns the slot, and the
    // others are matched here by the full second hash, whatever `fp_bits` is.
    side: Vec<(u64, u64, u64)>,
}

/// The bits a rank below `n` takes: `⌈log2 n⌉`, and at least one, so that no table is zero bits
/// wide.
fn rank_bits(n: usize) -> u32 {
    u64::BITS - (n.max(2) as u64 - 1).leading_zeros()
}

fn check_fingerprint_bits(bits: u32) -> Result<(), IndexError> {
    if bits <= MAX_FP_BITS {
        Ok(())
    } else {
        Err(IndexError::Format(
            "hashed-dict: fingerprint_bits must be in 0..=32",
        ))
    }
}

/// Bytes a table of `count` values of `width` bits occupies. `count ≤ u32::MAX` and `width ≤ 64`,
/// so the product fits a `u64`; whether the table fits *this platform's* address space is
/// answered here rather than by a truncating cast on a 32-bit target.
fn table_len(count: usize, width: u32) -> Result<usize, IndexError> {
    usize::try_from((count as u64 * u64::from(width)).div_ceil(8))
        .map_err(|_| IndexError::Format("hashed-dict: rank table too large"))
}

impl HashedDictIndex {
    /// Build the sidecar over `dict`, storing `fingerprint_bits` (`0..=32`) of a second hash
    /// beside each key's rank. See [`HashedDictIndex`] for what the width decides.
    ///
    /// The dictionary is walked once in rank order and each key hashed; 24 bytes a key are held
    /// while the perfect hash is built over the distinct hashes and the ranks are written at their
    /// slots. The same dictionary and width give the same blob, byte for byte, on any machine.
    /// Two keys that collide in the 64-bit key hash are served exactly, from a side table keyed by
    /// the full second hash; keys colliding in both 64-bit hashes at once, `≈ 2^-128` a pair, are
    /// told apart only at `fingerprint_bits == 0`.
    pub fn from_dict(dict: DictIndex, fingerprint_bits: u32) -> Result<Self, IndexError> {
        Self::from_shared_dict(Arc::new(dict), fingerprint_bits)
    }

    /// [`from_dict`](Self::from_dict) over a dictionary another handle shares.
    pub(crate) fn from_shared_dict(
        dict: Arc<DictIndex>,
        fingerprint_bits: u32,
    ) -> Result<Self, IndexError> {
        check_fingerprint_bits(fingerprint_bits)?;
        let n = dict.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Build("hashed-dict: more than u32::MAX keys"));
        }
        let mut pairs: Vec<(u64, u64)> = Vec::with_capacity(n);
        pairs.extend(dict.iter().map(|(key, _)| hash_pair_bytes(key.as_bytes())));
        if pairs.len() != n {
            return Err(IndexError::Format(
                "hashed-dict: the dictionary's walk ended before its last key",
            ));
        }
        Self::from_pairs(dict, pairs, fingerprint_bits)
    }

    /// The build after the hashing pass: `pairs[rank]` is the `(hash, full second hash)` of the
    /// key at `rank`. Split out so a test can hand it a collision no search would find.
    fn from_pairs(
        dict: Arc<DictIndex>,
        mut pairs: Vec<(u64, u64)>,
        fp_bits: u32,
    ) -> Result<Self, IndexError> {
        let n = pairs.len();
        let width = rank_bits(n) + fp_bits;
        if n == 0 {
            return Ok(Self {
                dict,
                mph: None,
                ranks: SharedBytes::from_owned(Vec::new()),
                fp_bits,
                width,
                n,
                side: Vec::new(),
            });
        }
        let mut hashes: Vec<u64> = pairs.iter().map(|p| p.0).collect();
        hashes.sort_unstable();
        // Each hash two or more keys share, once.
        let mut collided: Vec<u64> = hashes
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        collided.dedup();
        hashes.dedup();
        let m = hashes.len();
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let mph = Mphf::build_with_threads(&hashes, threads)?;
        drop(hashes);

        // Every key of a shared hash but the lowest-ranked goes to the side table.
        let mut side: Vec<(u64, u64, u64)> = Vec::new();
        if !collided.is_empty() {
            let mut owned = vec![false; collided.len()];
            for (rank, &(h, full)) in pairs.iter().enumerate() {
                if let Ok(g) = collided.binary_search(&h) {
                    if owned[g] {
                        side.push((h, full, rank as u64));
                    }
                    owned[g] = true;
                }
            }
        }
        // Each hash becomes its slot, on every thread when there are enough keys to share out.
        let slots = |part: &mut [(u64, u64)]| part.iter_mut().for_each(|p| p.0 = mph.index(p.0));
        if threads <= 1 || n < threads * 4096 {
            slots(&mut pairs);
        } else {
            std::thread::scope(|scope| {
                for part in pairs.chunks_mut(n.div_ceil(threads)) {
                    scope.spawn(move || slots(part));
                }
            });
        }
        for &(_, _, rank) in &side {
            pairs[rank as usize].0 = NO_SLOT;
        }
        side.sort_unstable();

        let mut ranks = Pages::zeroed(table_len(m, width)?);
        // One bit a slot: this only has to catch a construction that was not minimal/perfect.
        let mut seen = vec![0u64; m.div_ceil(64)];
        let mask = if fp_bits == 0 { 0 } else { fp_mask(fp_bits) };
        // The slots come in random order, so the line a later key writes is pulled in while this
        // one's store resolves.
        const AHEAD: usize = 16;
        for (rank, &(slot, full)) in pairs.iter().enumerate() {
            if let Some(&(next, _)) = pairs.get(rank + AHEAD) {
                let at = next.saturating_mul(u64::from(width)) / 8;
                crate::blob::prefetch_byte(&ranks, at as usize);
            }
            if slot == NO_SLOT {
                continue;
            }
            let slot = slot as usize;
            if slot >= m || (seen[slot / 64] >> (slot % 64)) & 1 == 1 {
                return Err(IndexError::Format(
                    "hashed-dict: construction was not minimal/perfect",
                ));
            }
            seen[slot / 64] |= 1 << (slot % 64);
            write_fp(
                &mut ranks,
                slot,
                width,
                ((rank as u64) << fp_bits) | (full & mask),
            );
        }
        Ok(Self {
            dict,
            mph: Some(mph),
            ranks: SharedBytes::from_pages(ranks),
            fp_bits,
            width,
            n,
            side,
        })
    }

    /// The dictionary the sidecar rides on, for `key`, `prefix`, `lower_bound`, `range`,
    /// iteration and the exact `id`. Its ids are this index's.
    pub fn dict(&self) -> &DictIndex {
        &self.dict
    }

    /// [`dict`](Self::dict) as the shared handle the Python binding hands out.
    #[cfg(feature = "python")]
    pub(crate) fn shared_dict(&self) -> &Arc<DictIndex> {
        &self.dict
    }

    /// Width of the stored fingerprints in bits. From one up, [`id`](Self::id) answers a
    /// non-member at `2^-fingerprint_bits`; at zero it is the dictionary's own exact search.
    pub fn fingerprint_bits(&self) -> u32 {
        self.fp_bits
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the index has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The value at `slot`: a rank above `fp_bits` of a second hash. `slot` is below the slot
    /// count — the MPH's image is `[0, m)` for every hash, validated when a blob is parsed — and
    /// the table holds `m` values, sized at build and checked at parse.
    ///
    /// One unaligned 8-byte load, which holds the whole value whenever `width` is at most 57 — a
    /// rank of up to 25 bits beside 32 fingerprint bits, or any rank beside 24. The generic read
    /// is out of line, for the table's last seven bytes and the widths past 57.
    #[inline(always)]
    fn value(&self, slot: usize) -> u64 {
        let bytes = self.ranks.as_ref();
        let bit = slot as u64 * u64::from(self.width);
        let at = (bit / 8) as usize;
        match bytes.get(at..at + 8) {
            Some(word) if self.width <= 57 => {
                let word = u64::from_le_bytes(word.try_into().expect("an 8-byte slice"));
                (word >> (bit % 8)) & (u64::MAX >> (64 - self.width))
            }
            _ => self.value_at_the_end(slot),
        }
    }

    /// [`value`](Self::value) for the table's last seven bytes and for widths over 57.
    #[cold]
    #[inline(never)]
    fn value_at_the_end(&self, slot: usize) -> u64 {
        read_fp(self.ranks.as_ref(), slot, self.width).unwrap_or(0)
    }

    /// Ranks of keys whose 64-bit hash a key of lower rank shares, matched by the **full** second
    /// hash — off the hot path: it runs only when the table is non-empty, and before the rank
    /// table is read, since a side key's fingerprint may tie the one its group's owner stored.
    #[cold]
    fn side_lookup(&self, h: u64, full: u64) -> Option<u64> {
        let start = self.side.partition_point(|e| e.0 < h);
        self.side[start..]
            .iter()
            .take_while(|e| e.0 == h)
            .find_map(|e| (e.1 == full).then_some(e.2))
    }

    /// Rank of `key`, or `None`.
    ///
    /// From one fingerprint bit up this is the sidecar: one hash, a read of each table, and a
    /// non-member answered `Some` at `2^-fingerprint_bits`. At zero bits it is
    /// [`dict().id`](DictIndex::id), exact and at the dictionary's cost.
    // Forced: the two hashes and the perfect hash make the body too large for `#[inline]`, and as a
    // call a loop of independent lookups ran 12 % slower at ten million keys (one process, the two
    // alternated); one lookup's latency is the same either way.
    #[inline(always)]
    pub fn id(&self, key: &str) -> Option<u64> {
        if self.fp_bits == 0 {
            return self.exact_id(key);
        }
        if !self.side.is_empty() {
            return self.id_with_side(key.as_bytes());
        }
        let (h, full) = hash_pair_bytes(key.as_bytes());
        self.checked(self.mph.as_ref()?.index(h), full)
    }

    /// [`id`](Self::id) at zero bits: the dictionary's search, kept out of `id`'s body so the
    /// sidecar path stays small enough to inline.
    #[inline(never)]
    fn exact_id(&self, key: &str) -> Option<u64> {
        self.dict.id(key)
    }

    /// [`id`](Self::id) for an index holding a 64-bit hash collision: the side table first, then
    /// the sidecar.
    #[cold]
    fn id_with_side(&self, key: &[u8]) -> Option<u64> {
        let (h, full) = hash_pair_bytes(key);
        if let Some(rank) = self.side_lookup(h, full) {
            return Some(rank);
        }
        self.checked(self.mph.as_ref()?.index(h), full)
    }

    /// The rank at `slot` if its fingerprint is `full`'s low bits, for a width of at least one.
    /// A blob this crate wrote holds ranks below `n` only; one it did not is refused here rather
    /// than answered past the end.
    #[inline(always)]
    fn checked(&self, slot: u64, full: u64) -> Option<u64> {
        let v = self.value(slot as usize);
        let rank = v >> self.fp_bits;
        ((v ^ full) & fp_mask(self.fp_bits) == 0 && rank < self.n as u64).then_some(rank)
    }

    /// Rank of `key` **without** any membership check: `key` must be a member, or the result is
    /// some rank below `len()`. The fastest lookup for a closed vocabulary, at any width; `0` when
    /// the index is empty. Keys sharing a 64-bit hash resolve through the side table by their full
    /// second hash, exactly for members even there.
    // Forced, as `id` is: 3-8 % on a loop of independent lookups.
    #[inline(always)]
    pub fn id_unchecked(&self, key: &str) -> u64 {
        if !self.side.is_empty() {
            return self.unchecked_with_side(key);
        }
        self.closed_rank(hash_key_bytes(key.as_bytes()))
    }

    /// [`id_unchecked`](Self::id_unchecked) for an index holding a 64-bit hash collision.
    #[cold]
    fn unchecked_with_side(&self, key: &str) -> u64 {
        let h = hash_key_bytes(key.as_bytes());
        match self.side_lookup(h, fingerprint_full_bytes(key.as_bytes())) {
            Some(rank) => rank,
            None => self.closed_rank(h),
        }
    }

    /// The rank the table holds at `h`'s slot, with no fingerprint compared.
    #[inline(always)]
    fn closed_rank(&self, h: u64) -> u64 {
        match &self.mph {
            // A blob this crate wrote holds ranks below `n` only; one it did not is bounded here
            // rather than answered past the end.
            Some(mph) => (self.value(mph.index(h) as usize) >> self.fp_bits).min(self.n as u64 - 1),
            None => 0,
        }
    }

    /// Batched [`id`](Self::id): one answer per key, aligned with `keys`.
    ///
    /// With a fingerprint, the keys are hashed in one pass with later keys' bytes prefetched, the
    /// perfect hash answers them all with its own lines in flight, and the rank table's lines are
    /// prefetched ahead of the compare — what a loop of `id` serialises, this overlaps. The rare
    /// index holding a hash collision takes the per-key path. Without a fingerprint it is
    /// [`dict().ids_of`](DictIndex::ids_of).
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u64>> {
        self.ids_of_with(keys.len(), |i| keys[i].as_ref().as_bytes())
    }

    /// [`ids_of`](Self::ids_of) over `n` keys given as bytes by position, for a caller whose keys
    /// are not `str`s — a lookup reading an Arrow buffer.
    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        if self.fp_bits == 0 {
            return self.dict.ids_of_with(n, key);
        }
        let Some(mph) = &self.mph else {
            return vec![None; n];
        };
        if !self.side.is_empty() {
            return (0..n).map(|i| self.id_with_side(key(i))).collect();
        }
        const AHEAD: usize = 32;
        let mut hashes = Vec::with_capacity(n);
        let mut wanted = Vec::with_capacity(n);
        for i in 0..n {
            if i + AHEAD < n {
                crate::blob::prefetch_key(key(i + AHEAD));
            }
            let (h, full) = hash_pair_bytes(key(i));
            hashes.push(h);
            wanted.push(full);
        }
        let slots = mph.index_all(&hashes);
        let table = self.ranks.as_ref();
        (0..n)
            .map(|i| {
                if let Some(&s) = slots.get(i + AHEAD) {
                    crate::blob::prefetch_byte(table, (s * u64::from(self.width) / 8) as usize);
                }
                self.checked(slots[i], wanted[i])
            })
            .collect()
    }

    /// Whether `key` is present, under the same contract as [`id`](Self::id).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// The header and the owned sections, shared by [`to_bytes`](Self::to_bytes) and
    /// [`save`](Self::save) through [`write_to`](Self::write_to).
    fn serialised_parts(&self) -> ([u8; HEADER], Vec<u8>, Vec<u8>) {
        let mph_buf = self.mph.as_ref().map_or_else(Vec::new, Mphf::to_bytes);
        let mut side_buf = Vec::with_capacity(self.side.len() * SIDE_ENTRY);
        for &(h, full, rank) in &self.side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&full.to_le_bytes());
            side_buf.extend_from_slice(&rank.to_le_bytes());
        }
        let mut payload = crate::blob::BlockHasher::new();
        payload.update(&mph_buf);
        payload.update(self.ranks.as_ref());
        payload.update(&side_buf);
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(MAGIC);
        h[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        h[12..16].copy_from_slice(&self.fp_bits.to_le_bytes());
        h[16..24].copy_from_slice(&(self.dict.serialized_len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&(mph_buf.len() as u64).to_le_bytes());
        h[32..36].copy_from_slice(&(self.side.len() as u32).to_le_bytes());
        h[36..44].copy_from_slice(&payload.finish().to_le_bytes());
        let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..].copy_from_slice(&check.to_le_bytes());
        (h, mph_buf, side_buf)
    }

    /// Serialise to `[magic "BHD1"][n u64][fp_bits u32][dict_len u64][mph_len u64][side_len u32]
    /// [payload u64][check u32][dictionary][MPH blob][bit-packed ranks][side entries]`, where the
    /// dictionary is its own `DictIndex` blob byte for byte, with its own checksums. `check` is a
    /// hash of the preceding header bytes and `payload` one of the three sections after the
    /// dictionary; owned loads verify both, and the dictionary's.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_len());
        self.write_to(&mut out)
            .expect("writing to a Vec cannot fail");
        out
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it;
    /// [`save`](Self::save) writes exactly this many.
    pub fn serialized_len(&self) -> usize {
        HEADER
            + self.dict.serialized_len()
            + self.mph.as_ref().map_or(0, Mphf::byte_len)
            + self.ranks.len()
            + self.side.len() * SIDE_ENTRY
    }

    /// The [`to_bytes`](Self::to_bytes) blob streamed into `w`, the dictionary and the rank table
    /// from where they are.
    fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts();
        w.write_all(&header)?;
        self.dict.write_to(w)?;
        w.write_all(&mph_buf)?;
        w.write_all(self.ranks.as_ref())?;
        w.write_all(&side_buf)?;
        Ok(())
    }

    /// Reconstruct from [`to_bytes`](Self::to_bytes) output, copying the blob into owned memory.
    ///
    /// Safe on arbitrary bytes. The header, the dictionary, the perfect hash, the rank table and
    /// the side table are each bounded by lengths this crate wrote and checks here, and the
    /// checksums are verified, so a crafted blob is at worst *wrong* — ranks for a table it did not
    /// build — and every rank it answers is below `len()`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::copy_of(bytes), true)
    }

    /// Load `bytes` both ways and query what loaded; `true` if the checked way did. Exists for the
    /// libFuzzer target in `fuzz/`; see the `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_load_and_query(bytes: &[u8]) -> bool {
        let checked = Self::from_bytes(bytes).ok();
        let framed = Self::from_shared(SharedBytes::copy_of(bytes), false).ok();
        assert!(
            checked.is_none() || framed.is_some(),
            "the framing is the checked path's"
        );
        for idx in checked.iter().chain(&framed) {
            let n = idx.len() as u64;
            let probes = ["", "a", "zzzz", "\u{0}", "key-000001-1", "\u{10ffff}"];
            for probe in probes {
                assert!(idx.id(probe).is_none_or(|r| r < n), "id({probe:?})");
                assert!(
                    idx.id_unchecked(probe) < n.max(1),
                    "id_unchecked({probe:?})"
                );
            }
            assert!(idx.ids_of(&probes).iter().flatten().all(|&r| r < n));
        }
        checked.is_some()
    }

    /// The framing of `blob`, checked — magic, header checksum, the payload checksum when
    /// `verify`, every length against the blob, the side table's ranks — with the dictionary and
    /// the MPH region located, not parsed.
    fn parse_frame(blob: &SharedBytes, verify: bool) -> Result<Frame, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() < HEADER || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        if u32_at(CHECKED) != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let n64 = u64_at(4);
        if n64 > u64::from(u32::MAX) {
            return Err(IndexError::Format(
                "hashed-dict: header claims more than u32::MAX keys",
            ));
        }
        let n = n64 as usize;
        let fp_bits = u32_at(12);
        check_fingerprint_bits(fp_bits)?;
        let width = rank_bits(n) + fp_bits;
        // Header-supplied lengths are converted and added checked, so a fabricated one fails
        // cleanly on every target width instead of truncating or wrapping on a 32-bit one.
        let dict_len = usize::try_from(u64_at(16))
            .map_err(|_| IndexError::Format("hashed-dict: dictionary length out of range"))?;
        let mph_len = usize::try_from(u64_at(24))
            .map_err(|_| IndexError::Format("hashed-dict: mph length out of range"))?;
        let side_len = u32_at(32) as usize;
        if side_len >= n.max(1) {
            return Err(IndexError::Format("side table length out of range"));
        }
        if (n == 0) != (mph_len == 0) {
            return Err(IndexError::Format("hashed-dict: mph length out of range"));
        }
        let dict_end = HEADER
            .checked_add(dict_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format(
                "hashed-dict: dictionary length out of range",
            ))?;
        let mph_end = dict_end
            .checked_add(mph_len)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format("hashed-dict: mph length out of range"))?;
        let ranks_end = mph_end
            .checked_add(table_len(n - side_len, width)?)
            .filter(|&e| e <= bytes.len())
            .ok_or(IndexError::Format(
                "hashed-dict: rank table length mismatch",
            ))?;
        if Some(bytes.len() - ranks_end) != side_len.checked_mul(SIDE_ENTRY) {
            return Err(IndexError::Format("side table length out of range"));
        }
        // Owned loads verify the sidecar whole — one streaming pass after the dictionary, which
        // its own loader verifies — so a flipped byte is an error here, not a wrong answer later.
        if verify && u64_at(36) != crate::blob::hash_block(&bytes[dict_end..]) {
            return Err(IndexError::Format("payload checksum mismatch"));
        }
        let mut side: Vec<(u64, u64, u64)> = bytes[ranks_end..]
            .chunks_exact(SIDE_ENTRY)
            .map(|e| {
                (
                    u64::from_le_bytes(e[0..8].try_into().unwrap()),
                    u64::from_le_bytes(e[8..16].try_into().unwrap()),
                    u64::from_le_bytes(e[16..24].try_into().unwrap()),
                )
            })
            .collect();
        // `id` hands a side rank out verbatim, so one past the last key is refused here —
        // structurally, since the checksums vouch for transport, not construction.
        if side.iter().any(|e| e.2 >= n64) {
            return Err(IndexError::Format(
                "hashed-dict: a side-table rank is past the last key",
            ));
        }
        side.sort_unstable(); // restore the binary-search invariant regardless of the blob
        let ranks = blob.subslice(mph_end, ranks_end).ok_or(IndexError::Format(
            "hashed-dict: rank table length mismatch",
        ))?;
        Ok(Frame {
            n,
            fp_bits,
            width,
            dict: HEADER..dict_end,
            mph: dict_end..mph_end,
            ranks,
            side,
        })
    }

    /// Reconstruct from a shared source: the checked framing; the dictionary through its own
    /// loader, over its own region of the same source, so a mapping stays a mapping; the perfect
    /// hash copied into memory; the rank table borrowed where it lies.
    fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let frame = Self::parse_frame(&blob, verify)?;
        let region = blob
            .subslice(frame.dict.start, frame.dict.end)
            .ok_or(IndexError::Format(
                "hashed-dict: dictionary length out of range",
            ))?;
        let dict = DictIndex::from_shared(region, verify)?;
        if dict.len() != frame.n {
            return Err(IndexError::Format(
                "hashed-dict: the dictionary and the header disagree on the key count",
            ));
        }
        let mph = if frame.n == 0 {
            None
        } else {
            let mph = Mphf::from_bytes(&blob.as_ref()[frame.mph])?;
            if mph.n() != (frame.n - frame.side.len()) as u64 {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        Ok(Self {
            dict: Arc::new(dict),
            mph,
            ranks: frame.ranks,
            fp_bits: frame.fp_bits,
            width: frame.width,
            n: frame.n,
            side: frame.side,
        })
    }

    /// Write the index to `path`: the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section, so saving peaks at the index's own memory plus the small MPH buffer.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    /// Load an index written with [`save`](Self::save), reading the whole file and verifying every
    /// checksum. Safe on any file — see [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and borrow it: the dictionary loads as
    /// [`DictIndex::load_mmap`] does and the rank table is read where it lies; only the perfect
    /// hash and the dictionary's per-block samples are read into memory. Skips the checksums
    /// [`load`](Self::load) verifies.
    ///
    /// # Safety
    /// One obligation, and it is not about the bytes: the file must not be modified or truncated by
    /// any process while the returned index is alive, because the index borrows the mapping. A
    /// crafted file is *not* undefined behaviour here — the framing is checked and every read is
    /// bounded — it is merely wrong. See
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(Arc::new(mmap)), false)
    }

    /// [`load_mmap`](Self::load_mmap) plus the checks [`load`](Self::load) makes: one pass over the
    /// mapping at load, the pages still shared and nothing copied but what `load_mmap` copies. For
    /// a file you wrote but did not carry yourself.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap): the file must not change while the
    /// index is alive. The checks run once, at load, and say nothing about later.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_verified(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(Arc::new(mmap)), true)
    }
}

impl std::fmt::Debug for HashedDictIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HashedDictIndex")
            .field("len", &self.n)
            .field("fingerprint_bits", &self.fp_bits)
            .field("block", &self.dict.block())
            .field("bytes", &self.serialized_len())
            .finish_non_exhaustive()
    }
}

/// What [`HashedDictIndex::parse_frame`] checked and located.
struct Frame {
    n: usize,
    fp_bits: u32,
    width: u32,
    dict: std::ops::Range<usize>,
    mph: std::ops::Range<usize>,
    ranks: SharedBytes,
    side: Vec<(u64, u64, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("key-{i:06}-{}", i % 7)).collect()
    }

    fn build(keys: &[String], bits: u32) -> HashedDictIndex {
        HashedDictIndex::from_dict(DictIndex::build(keys).unwrap(), bits).unwrap()
    }

    fn ranked(keys: &[String]) -> Vec<String> {
        let mut sorted = keys.to_vec();
        sorted.sort();
        sorted.dedup();
        sorted
    }

    /// Rewrite the sidecar's payload checksum and the header's check after a test has edited
    /// the blob, so the edit reaches the structural checks.
    fn reseal(blob: &mut [u8], dict_len: usize) {
        let payload = crate::blob::hash_block(&blob[HEADER + dict_len..]);
        blob[36..44].copy_from_slice(&payload.to_le_bytes());
        let check = crate::blob::hash_bytes(&blob[..CHECKED]) as u32;
        blob[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
    }

    #[test]
    fn the_rank_read_returns_what_was_written_at_every_width() {
        // `value` is one 8-byte load up to 57 bits and the generic read past that and in the
        // table's last seven bytes. Widths past 57 need 2^25 keys to arise from a build, so the
        // table is written here directly.
        let mut index = build(&words(3), 0);
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for width in 1..=64u32 {
            let slots = 37;
            let mut table = vec![0u8; (slots * width as usize).div_ceil(8)];
            let written: Vec<u64> = (0..slots)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    state >> (64 - width)
                })
                .collect();
            for (slot, &v) in written.iter().enumerate() {
                write_fp(&mut table, slot, width, v);
            }
            index.ranks = SharedBytes::from_owned(table);
            index.width = width;
            for (slot, &v) in written.iter().enumerate() {
                assert_eq!(index.value(slot), v, "width {width}, slot {slot}");
            }
        }
    }

    #[test]
    fn every_member_answers_its_rank_at_every_width() {
        let keys = words(3_000);
        let sorted = ranked(&keys);
        for bits in [0u32, 1, 4, 7, 8, 13, 16, 24, 32] {
            let idx = build(&keys, bits);
            assert_eq!((idx.len(), idx.fingerprint_bits()), (sorted.len(), bits));
            assert!(!idx.is_empty());
            for (rank, key) in sorted.iter().enumerate() {
                assert_eq!(idx.id(key), Some(rank as u64), "bits={bits} {key:?}");
                assert_eq!(idx.id_unchecked(key), rank as u64, "bits={bits} {key:?}");
                assert!(idx.contains(key));
            }
            let all: Vec<Option<u64>> = (0..sorted.len() as u64).map(Some).collect();
            assert_eq!(idx.ids_of(&sorted), all, "bits={bits}");
        }
    }

    /// The table is as wide as a rank and its fingerprint need, and no wider: the width steps
    /// where `n - 1` gains a bit.
    #[test]
    fn a_rank_takes_ceil_log2_n_bits() {
        let cases = [(0, 1), (1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (1024, 10)];
        for (n, bits) in cases {
            assert_eq!(rank_bits(n), bits, "n={n}");
        }
        assert_eq!((rank_bits(1025), rank_bits(u32::MAX as usize)), (11, 32));
        let idx = build(&words(1025), 8);
        assert_eq!(idx.width, 11 + 8);
        assert_eq!(idx.ranks.len(), (1025 * 19usize).div_ceil(8));
    }

    #[test]
    fn zero_bits_is_exact_and_a_fingerprint_is_bounded() {
        let keys = words(20_000);
        let strangers: Vec<String> = (0..100_000).map(|i| format!("absent-{i}")).collect();
        let exact = build(&keys, 0);
        assert!(strangers[..10_000].iter().all(|k| exact.id(k).is_none()));
        assert!(
            exact
                .ids_of(&strangers[..10_000])
                .iter()
                .all(Option::is_none)
        );
        for bits in [4u32, 8] {
            let idx = build(&keys, bits);
            let batch = idx.ids_of(&strangers);
            let single: Vec<Option<u64>> = strangers.iter().map(|k| idx.id(k)).collect();
            assert_eq!(batch, single, "bits={bits}");
            let hits = single.iter().flatten().count() as f64;
            // A binomial count, within six standard deviations of `n p`.
            let p = 1.0 / f64::from(1u32 << bits);
            let expected = strangers.len() as f64 * p;
            let sd = (expected * (1.0 - p)).sqrt();
            assert!(
                (hits - expected).abs() < 6.0 * sd,
                "bits={bits}: {hits} false positives against {expected:.0}"
            );
            assert!(single.iter().flatten().all(|&r| r < idx.len() as u64));
            let n = idx.len() as u64;
            assert!(strangers[..1_000].iter().all(|k| idx.id_unchecked(k) < n));
        }
    }

    #[test]
    fn the_empty_index_and_the_one_key_index() {
        for bits in [0u32, 8] {
            let empty =
                HashedDictIndex::from_dict(DictIndex::build([""; 0]).unwrap(), bits).unwrap();
            assert!(empty.is_empty());
            assert_eq!((empty.id("x"), empty.id_unchecked("x")), (None, 0));
            assert_eq!(empty.ids_of(&["x"]), vec![None]);
            let restored = HashedDictIndex::from_bytes(&empty.to_bytes()).unwrap();
            assert!(restored.is_empty() && restored.id("x").is_none());

            let one =
                HashedDictIndex::from_dict(DictIndex::build(["only"]).unwrap(), bits).unwrap();
            assert_eq!((one.id("only"), one.id_unchecked("only")), (Some(0), 0));
            assert_eq!(one.id_unchecked("other"), 0);
            let restored = HashedDictIndex::from_bytes(&one.to_bytes()).unwrap();
            assert_eq!(restored.id("only"), Some(0));
        }
    }

    #[test]
    fn a_fingerprint_past_32_bits_is_refused() {
        let dict = DictIndex::build(["a", "b"]).unwrap();
        let err = HashedDictIndex::from_dict(dict, 33).unwrap_err();
        assert!(err.to_string().contains("0..=32"), "{err}");
    }

    /// The pinned 64-bit key-hash collision: both keys keep their own ranks at every width,
    /// through the side table, and the table survives a round trip.
    #[test]
    fn colliding_keys_resolve_through_the_side_table() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys = words(500);
        keys.extend([a.to_string(), b.to_string()]);
        let sorted = ranked(&keys);
        let rank = |k: &str| sorted.iter().position(|s| s == k).unwrap() as u64;
        let (ra, rb) = (rank(a), rank(b));
        for bits in [0u32, 1, 8, 16] {
            let idx = build(&keys, bits);
            assert_eq!(
                idx.side,
                vec![(
                    hash_key_bytes(b.as_bytes()),
                    fingerprint_full_bytes(b.as_bytes()),
                    rb
                )]
            );
            assert_eq!((idx.id(a), idx.id(b)), (Some(ra), Some(rb)), "bits={bits}");
            assert_eq!((idx.id_unchecked(a), idx.id_unchecked(b)), (ra, rb));
            assert_eq!(idx.ids_of(&[a, b]), vec![Some(ra), Some(rb)], "bits={bits}");
            for (r, key) in sorted.iter().enumerate() {
                assert_eq!(idx.id_unchecked(key), r as u64, "bits={bits} {key:?}");
                assert_eq!(idx.id(key), Some(r as u64), "bits={bits} {key:?}");
            }
            let restored = HashedDictIndex::from_bytes(&idx.to_bytes()).unwrap();
            assert_eq!(restored.side, idx.side);
            assert_eq!((restored.id(a), restored.id(b)), (Some(ra), Some(rb)));
        }
    }

    /// Three keys on one hash, fabricated at the pair level: the owner is the lowest rank, both
    /// followers are matched by their full second hash, and a stranger on the same hash is not
    /// taken for any of them.
    #[test]
    fn a_three_way_collision_keeps_every_rank() {
        let dict = Arc::new(DictIndex::build(words(6)).unwrap());
        let pairs = vec![(7, 70), (1, 10), (7, 71), (2, 20), (7, 72), (3, 30)];
        let idx = HashedDictIndex::from_pairs(dict, pairs.clone(), 8).unwrap();
        assert_eq!(idx.side, vec![(7, 71, 2), (7, 72, 4)]);
        for (rank, &(h, full)) in pairs.iter().enumerate() {
            let slot = idx.mph.as_ref().unwrap().index(h);
            let found = idx.side_lookup(h, full).or_else(|| idx.checked(slot, full));
            assert_eq!(found, Some(rank as u64));
        }
        assert_eq!(idx.side_lookup(7, 73), None);
    }

    #[test]
    fn round_trips_byte_for_byte_through_every_loader() {
        let keys = words(5_000);
        let idx = build(&keys, 8);
        let blob = idx.to_bytes();
        assert_eq!(blob.len(), idx.serialized_len());
        assert_eq!(&blob[..4], MAGIC);
        assert!(format!("{idx:?}").starts_with("HashedDictIndex { len: 5000"));
        let path = std::env::temp_dir().join(format!(
            "lexindex_hashed_dict_{}_{:?}.bhd",
            std::process::id(),
            std::thread::current().id()
        ));
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), blob);
        #[allow(unused_mut)]
        let mut loaded = vec![
            HashedDictIndex::from_bytes(&blob).unwrap(),
            HashedDictIndex::load(&path).unwrap(),
        ];
        #[cfg(feature = "mmap")]
        {
            // SAFETY: the file is this test's own and is not modified while the indexes live.
            loaded.push(unsafe { HashedDictIndex::load_mmap(&path) }.unwrap());
            // SAFETY: as above.
            loaded.push(unsafe { HashedDictIndex::load_mmap_verified(&path) }.unwrap());
        }
        let sorted = ranked(&keys);
        for other in &loaded {
            assert_eq!(other.to_bytes(), blob);
            for (rank, key) in sorted.iter().enumerate().step_by(7) {
                assert_eq!(other.id(key), Some(rank as u64));
                assert_eq!(other.dict().key(rank as u64).as_deref(), Some(key.as_str()));
            }
        }
        drop(loaded);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_same_keys_give_the_same_blob() {
        let keys = words(4_000);
        let mut reversed = keys.clone();
        reversed.reverse();
        assert_eq!(build(&keys, 8).to_bytes(), build(&reversed, 8).to_bytes());
    }

    #[test]
    fn the_embedded_dictionary_is_the_dictionarys_own_blob() {
        let dict = DictIndex::build_with_block(words(2_000), 1024).unwrap();
        let dict_blob = dict.to_bytes();
        let blob = HashedDictIndex::from_dict(dict, 8).unwrap().to_bytes();
        assert_eq!(&blob[HEADER..HEADER + dict_blob.len()], &dict_blob[..]);
        assert_eq!(
            HashedDictIndex::from_bytes(&blob).unwrap().dict().block(),
            1024
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        use proptest::prelude::*;
        proptest::test_runner::TestRunner::default()
            .run(&prop::collection::vec(any::<u8>(), 0..512), |data| {
                let _ = HashedDictIndex::from_bytes(&data);
                let _ = HashedDictIndex::from_shared(SharedBytes::copy_of(&data), false);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn every_truncation_and_every_flipped_byte_is_refused() {
        let blob = build(&words(300), 8).to_bytes();
        for len in 0..blob.len() {
            assert!(
                HashedDictIndex::from_bytes(&blob[..len]).is_err(),
                "a blob cut at {len} of {} loaded",
                blob.len()
            );
        }
        for at in 0..blob.len() {
            let mut bad = blob.clone();
            bad[at] ^= 0x40;
            assert!(
                HashedDictIndex::from_bytes(&bad).is_err(),
                "a flip at {at} loaded"
            );
        }
    }

    #[test]
    fn a_side_rank_past_the_end_is_refused_under_valid_checksums() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = build(&[a.to_string(), b.to_string(), "filler".to_string()], 8);
        assert_eq!(idx.side.len(), 1);
        let mut bad = idx.to_bytes();
        let end = bad.len();
        bad[end - 8..].copy_from_slice(&3u64.to_le_bytes());
        reseal(&mut bad, idx.dict().serialized_len());
        let err = HashedDictIndex::from_bytes(&bad).unwrap_err().to_string();
        assert!(err.contains("side-table rank"), "{err}");
    }

    /// A rank table forged to hold ranks past the end, under valid checksums, still answers
    /// inside `[0, n)`: `id` refuses those ranks and `id_unchecked` bounds them.
    #[test]
    fn a_forged_rank_table_never_answers_past_the_end() {
        let keys = words(1_000);
        let idx = build(&keys, 8);
        assert!(idx.side.is_empty());
        let mut bad = idx.to_bytes();
        let ranks_start =
            HEADER + idx.dict().serialized_len() + idx.mph.as_ref().unwrap().byte_len();
        bad[ranks_start..].fill(0xff);
        reseal(&mut bad, idx.dict().serialized_len());
        let forged = HashedDictIndex::from_bytes(&bad).unwrap();
        let n = forged.len() as u64;
        assert!(keys.iter().all(|k| forged.id(k).is_none()));
        assert!(keys.iter().all(|k| forged.id_unchecked(k) == n - 1));
        assert!(forged.ids_of(&keys).iter().all(Option::is_none));
    }

    /// The framing refuses what the header cannot mean: a key count past `u32::MAX`, a width past
    /// 32, a side table as long as the index, and a perfect hash on an empty index.
    #[test]
    fn a_header_that_cannot_describe_an_index_is_refused() {
        let idx = build(&words(50), 8);
        let blob = idx.to_bytes();
        let dict_len = idx.dict().serialized_len();
        let refuse = |at: usize, value: &[u8], why: &str| {
            let mut bad = blob.clone();
            bad[at..at + value.len()].copy_from_slice(value);
            reseal(&mut bad, dict_len);
            let err = HashedDictIndex::from_bytes(&bad).unwrap_err().to_string();
            assert!(err.contains(why), "{err}");
        };
        refuse(4, &(1u64 << 32).to_le_bytes(), "u32::MAX");
        refuse(12, &33u32.to_le_bytes(), "0..=32");
        refuse(32, &50u32.to_le_bytes(), "side table");
        refuse(4, &0u64.to_le_bytes(), "mph length");
        refuse(16, &u64::MAX.to_le_bytes(), "dictionary length");
    }
}
