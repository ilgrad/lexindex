//! An in-crate minimal perfect hash, so that a loader can bound every read it makes.
//!
//! **This is a spike, not shipped API** (`own-mphf`, off by default). The reason it exists is not
//! speed or size for their own sake: `ptr_hash` keeps its pilot table private, so a blob holding one
//! cannot be validated from outside the crate that owns it, and that is what forces `from_bytes`,
//! `load` and `load_mmap` to be `unsafe fn` on the two hash indexes. An MPH whose every array has a
//! length in *our* header can be checked, and those loaders become safe.
//!
//! # Construction
//!
//! PTHash's shape, which PHast and PtrHash both descend from. Keys are grouped into buckets by a
//! first hash; each bucket gets a one-byte *pilot* chosen so that the bucket's keys land on slots
//! nothing has taken yet. Buckets are placed largest first, while the table is still empty enough
//! that a small pilot can be found. The table is slightly larger than `n`, so the last few keys have
//! somewhere to go; the slots at or above `n` are then remapped down into the holes below it, which
//! is what makes the result *minimal*.
//!
//! Bucket assignment is deliberately skewed — 60 % of the keys into 30 % of the buckets — because
//! the cost is dominated by the hardest buckets, and skew makes them arrive first, when almost every
//! slot is free.
//!
//! # Space
//!
//! One byte per bucket plus the remap: `8/λ + 16.125·(1−α)/α` bits per key. Both terms matter and
//! they pull against each other — a larger `λ` is fewer pilots but harder buckets, and a smaller `α`
//! is an easier search but more slots to remap. The measured trade is tabulated on [`LAMBDA`].

use crate::IndexError;

/// The one error every overflow check below reports; naming it keeps the arithmetic readable.
const SIZE: IndexError = IndexError::Format("mphf: blob sections do not fit in memory");

/// Keys per bucket, and the table's fill. Together they set both the size — `8/λ` bits per key plus
/// `16.125·(1−α)/α` for the remap — and the whole cost of construction, and the two do not trade the
/// way an independent pilot per bucket would suggest. A window pilot is not 256 independent tries but
/// four runs of 64 correlated shifts, which makes a large bucket much dearer than a Poisson model
/// predicts. Measured at 10 M real-word bigram hashes, single-threaded, against `ptr_hash` at ~1.9 s:
///
/// | λ, α | bits/key | build | vs `ptr_hash` |
/// |---|---|---|---|
/// | 3.6, 0.98 | 2.555 | 2.0 s | 1.05× |
/// | **3.9, 0.98** | **2.384** | **3.1 s** | **1.63×** |
/// | 4.0, 0.98 | 2.333 | 5.5 s | 2.9× |
/// | 3.9, 0.99 | 2.218 | 11.8 s | 6.2× |
/// | 4.0, 0.985 | 2.250 | 25.5 s | 13.4× |
/// | 4.2, 0.97 | 2.324 | 41.8 s | 22× |
///
/// The λ axis is steep and not monotone with size: 4.0 costs 1.8× the build of 3.9 for 0.05 bits,
/// and 4.2 costs 13× more than that. This row is the one that meets both gates; the ones below it
/// buy size the build cannot afford, and 3.6 buys build the size gate will not allow.
const LAMBDA: f64 = 3.9;

/// Table fill. Below 1 there are spare slots for the last buckets to land on; the leftovers above
/// `n` are what the remap pays for.
const ALPHA: f64 = 0.98;

/// How many global seeds to try before giving up. A restart costs a full construction, so this is a
/// safety net against pathological input rather than an expected path.
const SEED_TRIES: u32 = 32;

/// How many slot seeds a single part may try before the whole table is rebuilt. A part is placed
/// independently of every other, so a part that will not pack costs one part's work to retry, not
/// the table's — without this the failure rate is amplified by the number of parts.
const PART_TRIES: u32 = 32;

/// Keys per part. The displacement search reads the ownership table at random, and that is the
/// whole cost of construction, so a part is sized to keep its slice of it in L2: 2^18 keys is about
/// 1 MiB of `u32`. Parts are independent — a key's bucket and its slot both stay inside one.
const KEYS_PER_PART: u64 = 1 << 18;

/// A minimal perfect hash over a set of 64-bit key hashes.
///
/// Maps each hash that was built in to a distinct value in `[0, n)`. A hash that was *not* built in
/// gets some value in that range too — membership is the caller's problem, exactly as with
/// `ptr_hash`, and both indexes in this crate answer it with a stored key or a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mphf {
    /// How many keys were built in; the image is exactly `[0, n)`.
    n: u64,
    /// Table size, at least `n`. Slots in `[n, slots)` are reached through `remap`.
    slots: u64,
    /// How many independent parts the table is cut into; see [`KEYS_PER_PART`].
    parts: u64,
    /// Buckets in each part. `pilots.len()` is this times `parts`.
    buckets_per_part: u64,
    /// Slots a key's window base can take in each part. A part's stride is this plus [`GUARD`],
    /// so a window starting at the last base still lies inside the part.
    slots_per_part: u64,
    /// `slots_per_part + GUARD`, kept rather than derived: it is one add on every lookup.
    stride: u64,
    /// How many buckets of a part are on the dense side of the skew, or 0 for no skew. Derived
    /// from `buckets_per_part`, but kept because deriving it costs a float multiply, and `index`
    /// is on the hot path of every lookup the crate makes.
    dense_buckets: u64,
    /// The slot seed each part was placed under. Parts retry independently, so these differ.
    part_seed: Vec<u64>,
    /// One pilot per bucket. A byte: the top two bits pick one of [`MUL`], the low six a shift.
    pilots: Vec<u8>,
    /// The remap, one entry per slot in `[n, slots)`, as a block base plus an offset into it: the
    /// free slot below `n` that slot stands for is `remap_base[i / REMAP_BLOCK] + remap_off[i]`.
    /// See [`REMAP_BLOCK`] for why the offsets fit in 16 bits.
    remap_base: Vec<u32>,
    /// Offset from its block's base, one per entry. Two bytes, not four — the remap is the whole
    /// difference between a table that meets the size gate at α = 0.98 and one that does not.
    remap_off: Vec<u16>,
    /// Mixed into every hash, so a failed construction can be retried on a different table.
    seed: u64,
}

/// Multiply-shift range reduction: `x` scaled into `[0, k)` without a division.
#[inline(always)]
fn scale(x: u64, k: u64) -> u64 {
    ((x as u128 * k as u128) >> 64) as u64
}

/// Spread a key hash for the part and bucket choice. One multiply, not a full mix: `h` arrives
/// from `hash_key`, which has already avalanched it, so all this has to do is decorrelate the field
/// this reads from the one `base` reads. `index` runs this on every lookup, and a `mix` here
/// measured 1.9× off `ptr_hash` where this measures [pending].
#[inline(always)]
fn spread(h: u64, seed: u64) -> u64 {
    (h ^ seed).wrapping_mul(MUL[0])
}

/// A cheap bijective mix, used to derive independent streams from one hash.
#[inline(always)]
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The four multipliers behind a key's four window bases: `2^64 · frac(√p)` for the first four
/// primes, rounded to odd (PARI/GP, 50 digits). Odd, so each is a bijection on `u64`, and Weyl
/// constants, so the high bits of the product spread well for any input that is already a hash.
const MUL: [u64; 4] = [
    0x6A09_E667_F3BC_C909,
    0xBB67_AE85_84CA_A73B,
    0x3C6E_F372_FE94_F82B,
    0xA54F_F53A_5F1D_36F1,
];

/// Slots past a part's last window base, so `base + shift` never reaches the next part.
const GUARD: u64 = 64;

/// Magic of a standalone minimal-perfect-hash blob.
const MAGIC: &[u8; 4] = b"MPH1";

/// Blob format version. [`REMAP_BLOCK`] and the six-bit shift field are part of it: a blob written
/// under different values does not fail some subtle way, it fails the length check below.
const FORMAT: u16 = 1;

/// Magic 4, version 2, reserved 2, eight `u64` scalars, then a `u32` check over all of that.
const HEADER: usize = 4 + 2 + 2 + 8 * 8 + 4;

/// Header bytes the trailing check covers.
const CHECKED: usize = HEADER - 4;

/// Remap entries per block base.
///
/// The remap is a *non-decreasing* sequence: the holes below `n` are handed out in increasing order,
/// and a slot at or above `n` that no key reached repeats its predecessor rather than breaking the
/// run. So a block needs only its first value in full, and the rest as offsets from it — which fit
/// in 16 bits with room to spare, because a block spans `REMAP_BLOCK · n / m` slots on average and
/// `u16::MAX` is 26 standard deviations above that at α = 0.99, more at every lower fill. A block
/// that overflowed anyway fails the seed rather than truncating, which is the safe direction.
///
/// 16.125 bits an entry against 32 for a `u32` each. Elias-Fano with a sampled select would reach
/// ~8.5, and is what to reach for if λ ever has to drop to 3.6 — but it puts a select on the lookup
/// path, and `id` is the gate with the least room in it.
const REMAP_BLOCK: usize = 256;

/// 64 consecutive occupancy bits starting at `bit`. The map carries one spare word at its end, so
/// the read past the last real word is in bounds.
#[inline(always)]
fn window(taken: &[u64], bit: u64) -> u64 {
    let w = (bit / 64) as usize;
    let o = bit % 64;
    if o == 0 {
        taken[w]
    } else {
        (taken[w] >> o) | (taken[w + 1] << (64 - o))
    }
}

/// The shape of a table, shared by every part of it.
struct Layout {
    seed: u64,
    buckets_per_part: u64,
    dense_buckets: u64,
    slots_per_part: u64,
    stride: u64,
}

/// One part's share of the four tables construction writes. Disjoint by construction: `chunks_mut`
/// hands each part its own, which is what lets parts be placed independently of each other.
struct PartTables<'a> {
    owner: &'a mut [u32],
    taken: &'a mut [u64],
    pilots: &'a mut [u8],
    placed: &'a mut [bool],
}

impl Mphf {
    /// Which bucket of its own part a hash belongs to, from the mixed hash the part came from.
    ///
    /// Skewed on purpose: the first 60 % of the space goes to the first 30 % of the buckets, so
    /// those buckets are about twice the average size. They are also placed first, and a large
    /// bucket is only cheap to place while the part is empty. Every field of `hb` read here is
    /// disjoint from the top bits the part came from, or the skew would correlate with the part.
    #[inline(always)]
    fn bucket_in_part(hb: u64, buckets: u64, dense: u64) -> u64 {
        let hl = hb.rotate_left(17);
        if dense == 0 {
            return scale(hl, buckets);
        }
        // Branchless on purpose. The split is 60/40, which is as close to unpredictable as a branch
        // gets, and `index` is on the hot path of every lookup — select the two operands, then do
        // the one multiply, rather than let the compiler choose between two multiplies.
        let (lo, span) = if hl < (u64::MAX / 5) * 3 {
            (0, dense)
        } else {
            (dense, buckets - dense)
        };
        lo + scale(hl.rotate_left(23), span)
    }

    /// Where a key's window under base function `j` starts, within its part.
    ///
    /// A pilot is a base function and a shift, and the slot is `base + shift`. The point of that
    /// shape is the search: the shifts under which one key lands on a free slot are exactly the
    /// zero bits of the occupancy window starting at its base, so a bucket's usable shifts are one
    /// AND of its keys' windows — 64 pilots for one load per key, against 64 hashes and 64 loads
    /// when the pilot was mixed into the slot.
    #[inline(always)]
    fn base(h: u64, part_seed: u64, j: usize, slots: u64) -> u64 {
        scale((h ^ part_seed).wrapping_mul(MUL[j]), slots)
    }

    /// Which part `h` falls in and which of that part's buckets, as a flat index into `pilots`.
    ///
    /// `bucket_in_part` stays below `buckets_per_part`, so the result stays below `pilots.len()`.
    #[inline(always)]
    fn locate(&self, h: u64) -> (u64, u64) {
        let hb = spread(h, self.seed);
        let part = scale(hb, self.parts);
        let b = part * self.buckets_per_part
            + Self::bucket_in_part(hb, self.buckets_per_part, self.dense_buckets);
        (part, b)
    }

    /// The id of `h` in `[0, n)`.
    ///
    /// # Panics
    ///
    /// If the table is empty. `[0, 0)` has no inhabitant, so there is no answer to return and no
    /// degenerate table that could produce one; callers check `n() != 0` first.
    #[inline(always)]
    pub fn index(&self, h: u64) -> u64 {
        let (part, b) = self.locate(h);
        let pilot = self.pilots[b as usize];
        let base = Self::base(
            h,
            self.part_seed[part as usize],
            usize::from(pilot >> 6),
            self.slots_per_part,
        );
        let s = part * self.stride + base + u64::from(pilot & 63);
        if s < self.n {
            s
        } else {
            let i = (s - self.n) as usize;
            u64::from(self.remap_base[i / REMAP_BLOCK]) + u64::from(self.remap_off[i])
        }
    }

    /// [`index`](Self::index) over a batch, with the pilot byte a later key will need pulled into
    /// cache while the current key resolves.
    ///
    /// The pilot table is the only access in `index` that is random over more than a page, and at
    /// ~2 bits a key it outgrows L2 somewhere around a million keys — from there every lookup pays
    /// a miss whose latency nothing else in the query can hide. A batch can see the next key's
    /// bucket and a single lookup cannot, which is the whole of the difference; recomputing that
    /// bucket to issue the prefetch costs two multiplies against the miss it hides.
    pub fn index_all(&self, hashes: &[u64]) -> Vec<u64> {
        const AHEAD: usize = 16;
        let mut out = Vec::with_capacity(hashes.len());
        for (i, &h) in hashes.iter().enumerate() {
            if let Some(&next) = hashes.get(i + AHEAD) {
                crate::blob::prefetch_byte(&self.pilots, self.locate(next).1 as usize);
            }
            out.push(self.index(h));
        }
        out
    }

    /// How many keys are in the image.
    pub fn n(&self) -> u64 {
        self.n
    }

    /// Bits per key: what the table costs, and the number its design is judged on. Only the
    /// measurements read it — a caller sizing a blob wants `PerfectHashIndex::serialized_len`,
    /// which counts the arena too.
    #[cfg(test)]
    pub fn bits_per_key(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        ((self.pilots.len() + self.remap_base.len() * 4 + self.remap_off.len() * 2) * 8) as f64
            / self.n as f64
    }

    /// Bytes [`to_bytes`](Self::to_bytes) will write.
    pub fn byte_len(&self) -> usize {
        HEADER
            + self.part_seed.len() * 8
            + self.pilots.len()
            + self.remap_base.len() * 4
            + self.remap_off.len() * 2
    }

    /// Serialise to a self-describing blob.
    ///
    /// Every section length is *derived* from the eight scalars in the header rather than written
    /// beside them, which is stronger than recording it: a loader that recomputes the lengths
    /// cannot be told a length that disagrees with the table it describes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len());
        let mut header = [0u8; HEADER];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&FORMAT.to_le_bytes());
        // Reserved; written zero and required to be zero, so a later flag cannot be read as absent.
        header[6..8].copy_from_slice(&0u16.to_le_bytes());
        for (i, v) in [
            self.n,
            self.slots,
            self.parts,
            self.buckets_per_part,
            self.slots_per_part,
            self.stride,
            self.dense_buckets,
            self.seed,
        ]
        .into_iter()
        .enumerate()
        {
            header[8 + i * 8..16 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        let check = crate::hash::hash_bytes(&header[..CHECKED]) as u32;
        header[CHECKED..].copy_from_slice(&check.to_le_bytes());
        out.extend_from_slice(&header);

        for &s in &self.part_seed {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out.extend_from_slice(&self.pilots);
        for &b in &self.remap_base {
            out.extend_from_slice(&b.to_le_bytes());
        }
        for &o in &self.remap_off {
            out.extend_from_slice(&o.to_le_bytes());
        }
        debug_assert_eq!(out.len(), self.byte_len());
        out
    }

    /// Reconstruct from [`to_bytes`](Self::to_bytes) output.
    ///
    /// **Safe on arbitrary bytes**, which is the whole reason this hash exists. Every read
    /// [`index`](Self::index) makes is bounded by a scalar in the header, so the checks below are
    /// exactly that list: each one rules out an index that could otherwise leave its table. What is
    /// *not* checked is that the table is a bijection over any particular key set — that needs the
    /// keys, and a blob that is merely wrong rather than malformed answers wrong ids, not unsound
    /// ones.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() < HEADER || &bytes[0..4] != MAGIC {
            return Err(IndexError::Format("mphf: bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED..HEADER].try_into().expect("4 bytes"));
        if check != crate::hash::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("mphf: header checksum mismatch"));
        }
        if u16::from_le_bytes(bytes[4..6].try_into().expect("2 bytes")) != FORMAT {
            return Err(IndexError::Format("mphf: unsupported format version"));
        }
        if u16::from_le_bytes(bytes[6..8].try_into().expect("2 bytes")) != 0 {
            return Err(IndexError::Format(
                "mphf: reserved header field is not zero",
            ));
        }
        let at = |i: usize| u64::from_le_bytes(bytes[8 + i * 8..16 + i * 8].try_into().expect("8"));
        let (n, slots, parts) = (at(0), at(1), at(2));
        let (buckets_per_part, slots_per_part, stride) = (at(3), at(4), at(5));
        let (dense_buckets, seed) = (at(6), at(7));

        if n == 0 {
            if bytes.len() != HEADER
                || (slots | parts | buckets_per_part | slots_per_part | stride | dense_buckets) != 0
            {
                return Err(IndexError::Format(
                    "mphf: empty table with a non-empty shape",
                ));
            }
            return Ok(Self {
                n: 0,
                slots: 0,
                parts: 0,
                buckets_per_part: 0,
                slots_per_part: 0,
                stride: 0,
                dense_buckets: 0,
                part_seed: Vec::new(),
                pilots: Vec::new(),
                remap_base: Vec::new(),
                remap_off: Vec::new(),
                seed,
            });
        }

        // `index` reaches `pilots[part * buckets_per_part + bucket_in_part(..)]`, and
        // `bucket_in_part` stays below `buckets_per_part` only while the skew boundary is inside it.
        if parts == 0 || buckets_per_part == 0 || slots_per_part == 0 {
            return Err(IndexError::Format("mphf: a table dimension is zero"));
        }
        if dense_buckets >= buckets_per_part {
            return Err(IndexError::Format(
                "mphf: skew boundary outside the bucket range",
            ));
        }
        // A slot is `part * stride + base + shift` with `base < slots_per_part` and `shift <= 63`,
        // so a part's slots stay inside its own stride only with this much room; and the occupancy
        // map is split on word boundaries, which is where the multiple of 64 comes from.
        if stride % 64 != 0 || stride < slots_per_part + 63 {
            return Err(IndexError::Format(
                "mphf: stride too small for a key's window",
            ));
        }
        if stride.checked_mul(parts) != Some(slots) || n > slots {
            return Err(IndexError::Format(
                "mphf: slot count disagrees with the parts",
            ));
        }

        let entries = (slots - n) as usize;
        let bucket_count = usize::try_from(
            parts
                .checked_mul(buckets_per_part)
                .ok_or(IndexError::Format("mphf: bucket count out of range"))?,
        )
        .map_err(|_| IndexError::Format("mphf: bucket count out of range"))?;
        let seeds = usize::try_from(parts)
            .map_err(|_| IndexError::Format("mphf: part count out of range"))?;
        let blocks = entries.div_ceil(REMAP_BLOCK);
        let want = HEADER
            .checked_add(seeds.checked_mul(8).ok_or(SIZE)?)
            .and_then(|v| v.checked_add(bucket_count))
            .and_then(|v| v.checked_add(blocks.checked_mul(4)?))
            .and_then(|v| v.checked_add(entries.checked_mul(2)?))
            .ok_or(SIZE)?;
        if bytes.len() != want {
            return Err(IndexError::Format(
                "mphf: blob length disagrees with the header",
            ));
        }

        let mut at = HEADER;
        let part_seed: Vec<u64> = bytes[at..at + seeds * 8]
            .chunks_exact(8)
            .map(|w| u64::from_le_bytes(w.try_into().expect("8 bytes")))
            .collect();
        at += seeds * 8;
        let pilots = bytes[at..at + bucket_count].to_vec();
        at += bucket_count;
        let remap_base: Vec<u32> = bytes[at..at + blocks * 4]
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes(w.try_into().expect("4 bytes")))
            .collect();
        at += blocks * 4;
        let remap_off: Vec<u16> = bytes[at..]
            .chunks_exact(2)
            .map(|w| u16::from_le_bytes(w.try_into().expect("2 bytes")))
            .collect();

        // The one check that costs more than a comparison, and the one that makes the image a
        // promise rather than a hope: a remapped slot must land below `n`. Callers index their own
        // arrays by what `index` returns, so an id outside `[0, n)` is their unsoundness, not ours.
        for (i, &off) in remap_off.iter().enumerate() {
            if u64::from(remap_base[i / REMAP_BLOCK]) + u64::from(off) >= n {
                return Err(IndexError::Format(
                    "mphf: a remap entry points outside the image",
                ));
            }
        }

        Ok(Self {
            n,
            slots,
            parts,
            buckets_per_part,
            slots_per_part,
            stride,
            dense_buckets,
            part_seed,
            pilots,
            remap_base,
            remap_off,
            seed,
        })
    }

    /// Build over `hashes`, which must already be distinct.
    ///
    /// Fails only if no global seed works, which needs input the bucket assignment cannot spread —
    /// duplicate hashes will do it, and the caller has already ruled those out.
    pub fn build(hashes: &[u64]) -> Result<Self, IndexError> {
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        Self::build_with_threads(hashes, threads)
    }

    /// [`build`](Self::build) on a fixed number of threads.
    ///
    /// The result does not depend on `threads` — a part is placed from its own index, its own keys
    /// and its own slice of the tables, so the only thing the thread count changes is how long it
    /// takes. Exposed so that a test can prove it rather than assert it, and so a caller inside its
    /// own pool can decline to open another one.
    pub fn build_with_threads(hashes: &[u64], threads: usize) -> Result<Self, IndexError> {
        for attempt in 0..SEED_TRIES {
            let seed = mix(0xA5A5_5A5A_DEAD_BEEF ^ u64::from(attempt));
            if let Some(built) = Self::try_build(hashes, seed, threads.max(1)) {
                return Ok(built);
            }
        }
        Err(IndexError::Build(
            "minimal perfect hash: no seed placed every bucket",
        ))
    }

    /// Sort one part's keys into its buckets, CSR-style: one counting pass, then one placing pass.
    /// `starts` is the part's slice of the global offset array and `off` where its keys begin, so
    /// the result is indexed exactly as one counting sort over the whole table would have left it.
    fn group_part(
        lay: &Layout,
        keys: &mut [u64],
        starts: &mut [u32],
        off: u32,
        scratch: &mut Vec<u64>,
        cursor: &mut Vec<u32>,
    ) {
        let local = |h: u64| {
            Self::bucket_in_part(spread(h, lay.seed), lay.buckets_per_part, lay.dense_buckets)
                as usize
        };
        cursor.clear();
        cursor.resize(starts.len(), 0);
        scratch.clear();
        scratch.extend_from_slice(keys);
        for &h in scratch.iter() {
            cursor[local(h)] += 1;
        }
        let mut acc = off;
        for (b, s) in starts.iter_mut().enumerate() {
            *s = acc;
            acc += cursor[b];
            cursor[b] = *s;
        }
        for &h in scratch.iter() {
            let b = local(h);
            keys[(cursor[b] - off) as usize] = h;
            cursor[b] += 1;
        }
    }

    /// Place one part: a pilot for each of its buckets such that no two of its keys take one slot.
    /// Returns the slot seed that worked, or `None` if none of [`PART_TRIES`] did.
    ///
    /// Everything it touches is the part's own. `start` and `by_bucket` are read-only and shared,
    /// the four tables are disjoint slices, and every slot a key can reach lies inside the part —
    /// so parts can be placed in any order, on any thread, and the table comes out the same. That
    /// is what makes the build deterministic under a thread count rather than merely usually equal.
    fn place_part(
        lay: &Layout,
        part: u64,
        start: &[u32],
        by_bucket: &[u64],
        t: PartTables<'_>,
    ) -> Option<u64> {
        // Buckets are numbered within the part; `start` and `by_bucket` are global, so these two
        // closures are the only place the two numberings meet.
        let first = (part * lay.buckets_per_part) as usize;
        let size_of = |b: u32| start[first + b as usize + 1] - start[first + b as usize];
        let keys_of = |b: u32| {
            let g = first + b as usize;
            &by_bucket[start[g] as usize..start[g + 1] as usize]
        };
        const FREE: u32 = u32::MAX;
        let mut bases: [Vec<u64>; 4] = std::array::from_fn(|_| Vec::with_capacity(64));
        let mut victims: Vec<u32> = Vec::with_capacity(16);
        let mut best_victims: Vec<u32> = Vec::with_capacity(16);
        let mut order: Vec<u32> = Vec::with_capacity(lay.buckets_per_part as usize);
        let mut queue: std::collections::VecDeque<u32> =
            std::collections::VecDeque::with_capacity(lay.buckets_per_part as usize);

        // Largest bucket first: the hard ones are cheap only while the part is still empty.
        // The order is a property of the bucket sizes, which no retry changes.
        order.clear();
        order.extend(0..lay.buckets_per_part as u32);
        order.sort_unstable_by_key(|&b| std::cmp::Reverse(size_of(b)));
        for retry in 0..PART_TRIES {
            let pseed = mix(lay.seed ^ part.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(retry));
            for s in 0..lay.stride {
                t.owner[s as usize] = FREE;
                t.taken[(s / 64) as usize] &= !(1 << (s % 64));
            }
            for b in 0..lay.buckets_per_part {
                t.placed[b as usize] = false;
            }
            queue.clear();
            queue.extend(order.iter().copied());

            // Displacement is bounded so a part that livelocks is retried instead of looping, and
            // the bound is tight on purpose because the two populations barely overlap. Over 425
            // parts at 10 M and 100 M keys every one that finished did so in 1.028 to 1.269 pops
            // per bucket, mean 1.109 — while a part that circulates burns 64 before any loose bound
            // gives up, and places on the next seed. Four times the bucket count sits 3.15× above
            // the worst healthy part and a sixteenth of a stuck one.
            let mut budget = 4 * lay.buckets_per_part + 4096;
            // A cuckoo table cycles when two buckets keep taking each other's slots. The standard
            // guard is to refuse to evict anything displaced in the last few steps; 16 is what
            // PtrHash uses, but the ring may not cover a sizeable share of a small part, or it
            // would forbid every eviction there is and fail on a bucket it could have placed.
            let recent_len = (lay.buckets_per_part as usize / 4).clamp(1, 16);
            let mut recent = [u32::MAX; 16];
            let mut recent_at = 0usize;
            let mut failed = false;

            while let Some(b) = queue.pop_front() {
                let keys = keys_of(b);
                // Empty buckets need no pilot; already-placed ones are stale queue entries left by
                // an eviction that was itself undone, and re-placing them only churns the part.
                if keys.is_empty() || t.placed[b as usize] {
                    continue;
                }
                if budget == 0 {
                    failed = true;
                    break;
                }
                budget -= 1;

                // The window base of every key under every base function, once per bucket. A
                // base function under which two keys of this bucket share a base is out: their
                // slots would coincide under every shift, and no eviction changes that.
                let mut usable = [true; 4];
                for (j, bj) in bases.iter_mut().enumerate() {
                    bj.clear();
                    for &h in keys {
                        let base = Self::base(h, pseed, j, lay.slots_per_part);
                        if bj.contains(&base) {
                            usable[j] = false;
                            break;
                        }
                        bj.push(base);
                    }
                }

                // Prefer a pilot that collides with nothing: under each base function, AND the
                // keys' windows and take the first zero. Failing that, the second pass below
                // prices what each pilot would displace and takes the cheapest.
                let mut chosen: Option<(u8, u32, bool)> = None;
                for j in 0..4 {
                    if !usable[j] {
                        continue;
                    }
                    let mut free = !0u64;
                    for &base in &bases[j] {
                        free &= !window(t.taken, base);
                        if free == 0 {
                            break;
                        }
                    }
                    if free != 0 {
                        chosen = Some((((j as u8) << 6) | free.trailing_zeros() as u8, 0, true));
                        break;
                    }
                }
                if chosen.is_none() {
                    for j in 0..4 {
                        if !usable[j] {
                            continue;
                        }
                        for d in 0..GUARD {
                            victims.clear();
                            let mut cost = 0u32;
                            for &base in &bases[j] {
                                let o = t.owner[(base + d) as usize];
                                if o != FREE && o != b && !victims.contains(&o) {
                                    victims.push(o);
                                    // Squared, so displacing one big bucket loses to displacing
                                    // two small ones: the big one is the expensive one to re-place.
                                    cost += size_of(o) * size_of(o);
                                }
                            }
                            // The first pass would have taken a free pilot.
                            debug_assert!(cost != 0);
                            if victims.iter().any(|&v| recent[..recent_len].contains(&v)) {
                                continue;
                            }
                            // Displacing a bucket bigger than this one is what stalls a part:
                            // the big ones are placed first, into an empty part, and once evicted
                            // they need a run of free slots that no longer exists. Prefer the
                            // pilots that push work downhill, and reach for the rest only when
                            // there is no such pilot at all.
                            let downhill = victims.iter().all(|&v| size_of(v) <= size_of(b));
                            let better = match chosen {
                                None => true,
                                Some((_, best, was_downhill)) => match (downhill, was_downhill) {
                                    (true, false) => true,
                                    (false, true) => false,
                                    _ => cost < best,
                                },
                            };
                            if better {
                                chosen = Some((((j as u8) << 6) | d as u8, cost, downhill));
                                best_victims.clear();
                                best_victims.extend_from_slice(&victims);
                            }
                        }
                    }
                }

                // No pilot at all placed this bucket's keys on distinct slots, or every one that
                // did would evict a bucket displaced moments ago: a different seed is the way out.
                let Some((pilot, _, _)) = chosen else {
                    failed = true;
                    break;
                };

                // Clear each victim's slots and hand it back before claiming its own. A displaced
                // bucket goes to the *front*, so its chain is followed to the end before the next
                // fresh bucket is touched. That is not a nicety: appending them instead diffuses
                // displaced buckets among the pending ones, and the part settles into an
                // equilibrium where eviction hands back as many keys as placement takes — measured
                // over one un-partitioned table of 10 000 keys, ~85 buckets stayed unplaced across
                // 1.3 M displacements with no trend. Depth-first, the same table builds at once.
                for &v in &best_victims {
                    let vk = keys_of(v);
                    let pv = t.pilots[v as usize];
                    for &h in vk {
                        let s = Self::base(h, pseed, usize::from(pv >> 6), lay.slots_per_part)
                            + u64::from(pv & 63);
                        if t.owner[s as usize] == v {
                            t.owner[s as usize] = FREE;
                            t.taken[(s / 64) as usize] &= !(1 << (s % 64));
                        }
                    }
                    t.placed[v as usize] = false;
                    recent[recent_at] = v;
                    recent_at = (recent_at + 1) % recent_len;
                    queue.push_front(v);
                }
                best_victims.clear();

                let (j, d) = (usize::from(pilot >> 6), u64::from(pilot & 63));
                for &base in &bases[j] {
                    let s = base + d;
                    t.owner[s as usize] = b;
                    t.taken[(s / 64) as usize] |= 1 << (s % 64);
                }
                t.pilots[b as usize] = pilot;
                t.placed[b as usize] = true;
            }

            if !failed {
                return Some(pseed);
            }
        }
        None
    }

    fn try_build(hashes: &[u64], seed: u64, threads: usize) -> Option<Self> {
        let n = hashes.len() as u64;
        if n == 0 {
            return Some(Self {
                n: 0,
                slots: 0,
                parts: 0,
                buckets_per_part: 0,
                slots_per_part: 0,
                stride: 0,
                dense_buckets: 0,
                part_seed: Vec::new(),
                pilots: Vec::new(),
                remap_base: Vec::new(),
                remap_off: Vec::new(),
                seed,
            });
        }
        let parts = n.div_ceil(KEYS_PER_PART).max(1);
        let per_part = n.div_ceil(parts);
        let buckets_per_part = ((per_part as f64 / LAMBDA).ceil() as u64).max(1);
        let slots_per_part = ((per_part as f64 / ALPHA).ceil() as u64).max(1);
        // Rounded up to whole bitmap words: a part owns its slots outright, and the occupancy
        // map is split between parts by `chunks_mut`, which cannot split a word between two.
        let stride = (slots_per_part + GUARD).next_multiple_of(64);
        // A degenerate part (one or two buckets) would leave one side of the split empty; folding
        // that case into a zero here keeps the test out of `index`.
        let dense_buckets = {
            let d = (buckets_per_part as f64 * 0.3) as u64;
            if d == 0 || d >= buckets_per_part {
                0
            } else {
                d
            }
        };
        let buckets = buckets_per_part * parts;
        let slots = stride * parts;
        // `per_part * parts >= n` and `slots_per_part >= per_part`, so the table holds every key.
        debug_assert!(slots >= n);

        // By part first. A histogram over every bucket is 10 MB at 10 M keys and every increment is
        // a cache miss; a histogram over the parts is a few dozen counters that never leave L1. Once
        // the keys are grouped by part, each part sorts its own into its own buckets — in parallel,
        // and inside a slice that fits in cache. Measured at 10 M: 631 ms of a 1 094 ms build was
        // this grouping, all of it serial.
        let part_of = |h: u64| scale(spread(h, seed), parts) as usize;
        let mut part_start = vec![0u32; parts as usize + 1];
        for &h in hashes {
            part_start[part_of(h) + 1] += 1;
        }
        for p in 0..parts as usize {
            part_start[p + 1] += part_start[p];
        }
        let mut by_bucket = vec![0u64; hashes.len()];
        {
            let mut cursor = part_start.clone();
            for &h in hashes {
                let p = part_of(h);
                by_bucket[cursor[p] as usize] = h;
                cursor[p] += 1;
            }
        }

        let lay = Layout {
            seed,
            buckets_per_part,
            dense_buckets,
            slots_per_part,
            stride,
        };

        // Each part sorts its own keys into its own buckets. `start` is written in absolute
        // offsets into `by_bucket`, so what the placement pass reads is exactly what it read when
        // this was one serial counting sort.
        let mut start = vec![0u32; buckets as usize + 1];
        let bpp = buckets_per_part as usize;
        let group = (parts as usize).div_ceil(threads.clamp(1, parts as usize));
        {
            let mut slices: Vec<&mut [u64]> = Vec::with_capacity(parts as usize);
            let mut rest: &mut [u64] = &mut by_bucket;
            for p in 0..parts as usize {
                let (head, tail) = rest.split_at_mut((part_start[p + 1] - part_start[p]) as usize);
                slices.push(head);
                rest = tail;
            }
            let mut work: Vec<(&mut [u64], &mut [u32], u32)> = slices
                .into_iter()
                .zip(start.chunks_mut(bpp))
                .zip(part_start.iter().copied())
                .map(|((k, st), off)| (k, st, off))
                .collect();
            std::thread::scope(|scope| {
                for chunk in work.chunks_mut(group) {
                    let lay = &lay;
                    scope.spawn(move || {
                        let mut scratch: Vec<u64> = Vec::new();
                        let mut cursor: Vec<u32> = Vec::new();
                        for (keys, starts, off) in chunk.iter_mut() {
                            Self::group_part(lay, keys, starts, *off, &mut scratch, &mut cursor);
                        }
                    });
                }
            });
        }
        start[buckets as usize] = n as u32;

        // Who owns each slot, so a colliding bucket can be evicted rather than the pilot grown.
        // `u32::MAX` is the empty marker; a part has fewer buckets than that by construction.
        let mut owner = vec![u32::MAX; slots as usize];
        // The same occupancy as one bit per slot. Whether a pilot is usable at all is a question
        // about free slots and nothing else, and a part's bits are 33 KiB against `owner`'s 1 MiB —
        // the search runs out of L1 rather than L2. A key's window never crosses out of its part, so
        // `window`'s read of the word after the one it starts in stays inside the part's own words.
        let words_per_part = (stride / 64) as usize;
        let mut taken = vec![0u64; words_per_part * parts as usize];
        let mut pilots = vec![0u8; buckets as usize];
        let mut placed = vec![false; buckets as usize];
        let mut part_seed = vec![0u64; parts as usize];

        // One thread per group of parts, and the groups are contiguous so every table splits with
        // `chunks_mut`. A group that cannot place one of its parts sets the flag and stops; there is
        // nothing to unwind, because the next seed rebuilds everything anyway.
        let stalled = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            for (g, ((((o, tk), pi), pl), ps)) in owner
                .chunks_mut(stride as usize * group)
                .zip(taken.chunks_mut(words_per_part * group))
                .zip(pilots.chunks_mut(bpp * group))
                .zip(placed.chunks_mut(bpp * group))
                .zip(part_seed.chunks_mut(group))
                .enumerate()
            {
                let (lay, start, by_bucket, stalled) = (&lay, &start, &by_bucket, &stalled);
                scope.spawn(move || {
                    for (k, ((((o, tk), pi), pl), ps)) in o
                        .chunks_mut(stride as usize)
                        .zip(tk.chunks_mut(words_per_part))
                        .zip(pi.chunks_mut(bpp))
                        .zip(pl.chunks_mut(bpp))
                        .zip(ps.iter_mut())
                        .enumerate()
                    {
                        let tables = PartTables {
                            owner: o,
                            taken: tk,
                            pilots: pi,
                            placed: pl,
                        };
                        match Self::place_part(
                            lay,
                            (g * group + k) as u64,
                            start,
                            by_bucket,
                            tables,
                        ) {
                            Some(pseed) => *ps = pseed,
                            None => {
                                stalled.store(true, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                });
            }
        });
        if stalled.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }

        // Minimal at last: every occupied slot at or above `n` is redirected to a hole below it.
        // There are exactly as many of each, because the table holds `n` keys in `slots` slots.
        let mut holes = (0..n).filter(|&s| taken[(s / 64) as usize] >> (s % 64) & 1 == 0);
        let entries = (slots - n) as usize;
        let mut remap_base = vec![0u32; entries.div_ceil(REMAP_BLOCK)];
        let mut remap_off = vec![0u16; entries];
        let mut hole = 0u32;
        for i in 0..entries {
            let s = n + i as u64;
            if taken[(s / 64) as usize] >> (s % 64) & 1 == 1 {
                // A hole must exist: occupancy below `n` plus occupancy above it is exactly `n`.
                hole = holes.next().expect("a hole for every overflowing slot") as u32;
            }
            // A slot no key reached keeps the previous hole. Nothing ever reads that entry; what it
            // buys is that the sequence never decreases, which is what makes the offsets small.
            if i % REMAP_BLOCK == 0 {
                remap_base[i / REMAP_BLOCK] = hole;
            }
            remap_off[i] = u16::try_from(hole - remap_base[i / REMAP_BLOCK]).ok()?;
        }

        Some(Self {
            n,
            slots,
            parts,
            buckets_per_part,
            slots_per_part,
            stride,
            dense_buckets,
            part_seed,
            pilots,
            remap_base,
            remap_off,
            seed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct pseudo-random hashes, standing in for `hash_key` over a real corpus. The
    /// construction only ever sees hashes, so the corpus matters to the *measurement*, not to
    /// correctness — what matters here is that they are distinct and unstructured.
    fn hashes(n: usize) -> Vec<u64> {
        let mut out: Vec<u64> = (0..n as u64)
            .map(|i| mix(i.wrapping_mul(0x2545_F491)))
            .collect();
        out.sort_unstable();
        out.dedup();
        assert_eq!(out.len(), n, "the generator produced a collision");
        out
    }

    /// The whole contract: every key gets its own id, and the ids are exactly `[0, n)`.
    fn assert_bijection(hs: &[u64]) {
        let mphf = Mphf::build(hs).expect("build");
        assert_eq!(mphf.n(), hs.len() as u64);
        let mut seen = vec![false; hs.len()];
        for &h in hs {
            let id = mphf.index(h);
            assert!(
                id < hs.len() as u64,
                "id {id} out of range for n = {}",
                hs.len()
            );
            assert!(!seen[id as usize], "id {id} handed out twice");
            seen[id as usize] = true;
        }
        assert!(
            seen.iter().all(|&s| s),
            "some id in [0, n) was never produced"
        );
    }

    #[test]
    fn it_is_a_bijection_onto_the_dense_range() {
        for n in [1usize, 2, 3, 7, 64, 1_000, 10_000] {
            assert_bijection(&hashes(n));
        }
    }

    /// The gate's determinism criterion, as a test rather than a claim: a part is placed from its
    /// own index, its own keys and its own slice, so the thread count cannot reach the result.
    #[test]
    fn the_table_is_the_same_on_one_thread_and_on_eight() {
        for n in [1000usize, 300_000] {
            let hs = hashes(n);
            let one = Mphf::build_with_threads(&hs, 1).expect("one thread");
            let eight = Mphf::build_with_threads(&hs, 8).expect("eight threads");
            assert_eq!(one, eight, "n = {n}: the thread count changed the table");
        }
    }

    /// Structured hashes, which is what an adversary who controls the keys can reach for. The
    /// contract is not that every set builds — it is that construction either produces a bijection
    /// or says `Build`, and never loops, panics or hands out an id twice.
    #[test]
    fn structured_hash_sets_either_build_or_refuse() {
        let n = 5000u64;
        type Family = (&'static str, fn(u64) -> u64);
        let families: [Family; 7] = [
            ("sequential", |i| i),
            ("strided high", |i| i << 40),
            ("strided low", |i| i << 3),
            ("shared top half", |i| 0xFFFF_FFFF_0000_0000 | i),
            ("shared low half", |i| (i << 32) | 0x0000_0000_DEAD_BEEF),
            ("mirrored", |i| i ^ (i << 32)),
            ("sparse bits", |i| i.wrapping_mul(0x0101_0101_0101_0101)),
        ];
        for (name, f) in families {
            let mut hs: Vec<u64> = (0..n).map(f).collect();
            hs.sort_unstable();
            hs.dedup();
            match Mphf::build(&hs) {
                Ok(m) => {
                    let mut seen = vec![false; hs.len()];
                    for &h in &hs {
                        let id = m.index(h) as usize;
                        assert!(id < hs.len(), "{name}: id {id} out of range");
                        assert!(!seen[id], "{name}: id {id} handed out twice");
                        seen[id] = true;
                    }
                }
                Err(IndexError::Build(_)) => {}
                Err(e) => panic!("{name}: unexpected error {e}"),
            }
        }
    }

    /// Sizes around the part boundary, where a table goes from one part to two and the last part is
    /// a different size from the rest.
    #[test]
    fn it_holds_where_the_parts_divide() {
        let k = super::KEYS_PER_PART;
        for n in [k - 1, k, k + 1, 2 * k - 1, 2 * k, 2 * k + 1] {
            assert_bijection(&hashes(n as usize));
        }
    }

    #[test]
    fn an_empty_set_builds_and_answers_nothing() {
        let mphf = Mphf::build(&[]).unwrap();
        assert_eq!(mphf.n(), 0);
        assert_eq!(mphf.bits_per_key(), 0.0);
    }

    /// Sizes either side of a power of two, and of the 64-bit words the occupancy bitset uses.
    #[test]
    fn it_holds_at_the_boundaries_of_its_own_word_size() {
        for n in [63usize, 64, 65, 127, 128, 129, 255, 256, 257] {
            assert_bijection(&hashes(n));
        }
    }

    /// One table big enough to have several parts and a non-empty remap, built once: every blob
    /// test below mutates *this* blob rather than random bytes, because random bytes never spell
    /// `MPH1` and would only ever exercise the first line of the loader.
    fn reference() -> &'static (Vec<u64>, Vec<u8>) {
        static REF: std::sync::OnceLock<(Vec<u64>, Vec<u8>)> = std::sync::OnceLock::new();
        REF.get_or_init(|| {
            let hs = hashes((KEYS_PER_PART + 1000) as usize);
            let blob = Mphf::build(&hs).expect("build").to_bytes();
            (hs, blob)
        })
    }

    /// Rewrite one of the eight header scalars and re-checksum, which is what an adversary does:
    /// the checksum catches corruption, not intent, so every field-level check has to stand on its
    /// own.
    fn with_scalar(blob: &[u8], field: usize, value: u64) -> Vec<u8> {
        let mut out = blob.to_vec();
        out[8 + field * 8..16 + field * 8].copy_from_slice(&value.to_le_bytes());
        let check = crate::hash::hash_bytes(&out[..CHECKED]) as u32;
        out[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        out
    }

    #[test]
    fn a_blob_round_trips_to_the_same_table() {
        for n in [0usize, 1, 2, 63, 64, 65, 1000, 100_000] {
            let hs = hashes(n);
            let mphf = Mphf::build(&hs).expect("build");
            let blob = mphf.to_bytes();
            assert_eq!(blob.len(), mphf.byte_len());
            let back = Mphf::from_bytes(&blob).expect("its own blob loads");
            assert_eq!(back.to_bytes(), blob, "n = {n}");
            assert_eq!(back.n(), mphf.n());
            for &h in &hs {
                assert_eq!(back.index(h), mphf.index(h), "n = {n}");
            }
        }
    }

    /// A cut blob has a header that still checksums; only the derived total length catches it.
    #[test]
    fn a_truncated_blob_is_refused() {
        let (_, blob) = reference();
        for cut in [
            0,
            1,
            HEADER - 1,
            HEADER,
            HEADER + 1,
            blob.len() / 2,
            blob.len() - 1,
        ] {
            assert!(
                Mphf::from_bytes(&blob[..cut]).is_err(),
                "a blob cut to {cut} bytes was accepted"
            );
        }
        let mut long = blob.clone();
        long.push(0);
        assert!(Mphf::from_bytes(&long).is_err(), "a trailing byte passed");
    }

    /// Every scalar the loader relies on, driven to the value that would break the read it bounds.
    #[test]
    fn each_header_invariant_is_enforced() {
        let (_, blob) = reference();
        let at = |i: usize| u64::from_le_bytes(blob[8 + i * 8..16 + i * 8].try_into().unwrap());
        let (n, parts, stride) = (at(0), at(2), at(5));
        let (buckets_per_part, slots_per_part) = (at(3), at(4));
        // field, value, what it would have broken
        let cases: [(usize, u64, &str); 10] = [
            (0, n + 1, "n above the slot count"),
            (1, 0, "slots disagreeing with parts * stride"),
            (2, 0, "zero parts"),
            (3, 0, "zero buckets"),
            (4, 0, "zero slots per part"),
            (5, stride + 1, "a stride that is not a multiple of 64"),
            (5, stride * 2, "a stride disagreeing with the slot count"),
            (4, stride - 62, "a part whose keys reach past its stride"),
            (6, buckets_per_part, "a skew boundary at the bucket count"),
            (6, u64::MAX, "a skew boundary past the bucket count"),
        ];
        for (field, value, what) in cases {
            assert!(
                Mphf::from_bytes(&with_scalar(blob, field, value)).is_err(),
                "accepted {what}"
            );
        }
        // `slots_per_part` names no section length, so any value inside the window bound loads —
        // the table then hands out ids the builder never would, which is a wrong blob, not an
        // unsound one. That split is the whole contract: validated for soundness, trusted for
        // correctness.
        assert!(slots_per_part <= stride - 63);
        assert!(Mphf::from_bytes(&with_scalar(blob, 4, stride - 63)).is_ok());
        // A header that checksums but was written by a different version of this file.
        let mut wrong_version = blob.clone();
        wrong_version[4..6].copy_from_slice(&(FORMAT + 1).to_le_bytes());
        let check = crate::hash::hash_bytes(&wrong_version[..CHECKED]) as u32;
        wrong_version[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        assert!(Mphf::from_bytes(&wrong_version).is_err(), "accepted v2");
        assert!(parts > 1, "the reference table should have several parts");
    }

    #[test]
    fn a_flipped_header_bit_is_caught_by_the_checksum() {
        let (_, blob) = reference();
        for byte in 0..CHECKED {
            for bit in 0..8 {
                let mut bad = blob.clone();
                bad[byte] ^= 1 << bit;
                assert!(
                    Mphf::from_bytes(&bad).is_err(),
                    "bit {bit} of header byte {byte} passed"
                );
            }
        }
    }

    /// The soundness property, and the reason `from_bytes` can be safe: **whatever** the body says,
    /// an accepted table answers inside `[0, n)`. The body is deliberately not checksummed — the
    /// pilots are unconstrained by construction, and the remap is validated by value on load — so
    /// this holds for arbitrary bytes rather than only for corruption-free ones.
    #[test]
    fn an_arbitrary_body_still_answers_inside_the_image() {
        let (hs, blob) = reference();
        let mut rng = 0x243F_6A88_85A3_08D3u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut accepted = 0;
        for _ in 0..64 {
            let mut bad = blob.clone();
            for _ in 0..32 {
                let i = HEADER + (next() as usize) % (bad.len() - HEADER);
                bad[i] = next() as u8;
            }
            let Ok(mphf) = Mphf::from_bytes(&bad) else {
                continue;
            };
            accepted += 1;
            for &h in hs.iter().take(4096) {
                assert!(mphf.index(h) < mphf.n(), "an id escaped the image");
            }
            for _ in 0..4096 {
                assert!(mphf.index(next()) < mphf.n(), "an id escaped the image");
            }
        }
        assert!(
            accepted > 0,
            "no mutated body was accepted, so nothing was tested"
        );
    }

    /// The remap is the one table whose contents can point outside the image, so it is the one
    /// table checked by value. Drive an entry past `n` and the blob must be refused.
    #[test]
    fn a_remap_entry_outside_the_image_is_refused() {
        let (_, blob) = reference();
        let mut bad = blob.clone();
        let len = bad.len();
        // The last two bytes are the final `remap_off`; `u16::MAX` on top of its block base is far
        // past `n` for any table this size.
        bad[len - 2..].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(
            Mphf::from_bytes(&bad).is_err(),
            "a remap entry past the image was accepted"
        );
    }

    #[test]
    fn an_empty_blob_is_only_accepted_with_an_empty_shape() {
        let empty = Mphf::build(&[]).expect("build").to_bytes();
        assert_eq!(empty.len(), HEADER);
        assert_eq!(Mphf::from_bytes(&empty).expect("loads").n(), 0);
        // n = 0 with a shape claimed anyway: the loader must not read the tables that shape names.
        assert!(Mphf::from_bytes(&with_scalar(&empty, 2, 1)).is_err());
        assert!(Mphf::from_bytes(&with_scalar(&empty, 5, 64)).is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

        /// Arbitrary bytes are a weak fuzzer here — they never spell the magic — but the first
        /// lines of the loader are exactly where a length check is easiest to get wrong.
        #[test]
        fn arbitrary_bytes_never_panic(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        ) {
            let _ = Mphf::from_bytes(&data);
        }

        /// Any single scalar replaced by any value at all, re-checksummed. Accepting is allowed;
        /// panicking, allocating by a claimed length, or answering outside `[0, n)` is not.
        #[test]
        fn any_crafted_header_scalar_is_safe(
            field in 0usize..8,
            value in proptest::prelude::any::<u64>(),
            probe in proptest::prelude::any::<u64>(),
        ) {
            let (_, blob) = reference();
            if let Ok(mphf) = Mphf::from_bytes(&with_scalar(blob, field, value)) {
                proptest::prop_assert!(mphf.n() > 0);
                proptest::prop_assert!(mphf.index(probe) < mphf.n());
            }
        }
    }
}

/// Measurements for the 1.0-01 spike. Not part of the test suite — they take minutes and answer
/// "how big and how fast", which is a question about this machine, not about correctness.
///
/// `cargo test --features own-mphf,mph --release --lib mphf::spike -- --ignored --nocapture`
#[cfg(all(test, feature = "mph"))]
mod spike {
    use super::*;

    /// The same corpus the shipped benchmark uses: real dictionary-word bigrams. Synthetic keys
    /// would answer a different question — see `CLAUDE.md`.
    fn bigram_hashes(n: usize) -> Vec<u64> {
        let path = std::env::var("LEXINDEX_BENCH_WORDS")
            .unwrap_or_else(|_| "/usr/share/dict/words".to_string());
        let text = std::fs::read_to_string(&path).expect("a system word list");
        let mut vocab: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        vocab.sort_unstable();
        vocab.dedup();
        let m = (n as f64).sqrt().ceil() as usize;
        let v = &vocab[..m.min(vocab.len())];
        let w = v.len();
        assert!(n <= w * w, "vocabulary too small for {n} distinct bigrams");
        let mut hs: Vec<u64> = (0..n)
            .map(|k| crate::hash::hash_key(&format!("{}.{}", v[k % w], v[(k / w) % w])))
            .collect();
        hs.sort_unstable();
        hs.dedup();
        assert_eq!(hs.len(), n, "the corpus collided under hash_key");
        hs
    }

    /// Minimum over `rounds` passes; the minimum is the least-throttled one, which is the estimator
    /// this repo settled on after measuring the machine's idle drift.
    fn min_ns<T>(rounds: usize, hs: &[u64], f: impl Fn(&T) -> u64, s: &T) -> f64 {
        let mut best = f64::INFINITY;
        for _ in 0..rounds {
            let t = std::time::Instant::now();
            let sum = f(s);
            let ns = t.elapsed().as_secs_f64() * 1e9 / hs.len() as f64;
            std::hint::black_box(sum);
            best = best.min(ns);
        }
        best
    }

    /// Size and correctness at scale. Split from the comparison below because bits/key is a
    /// property of the construction and not of the machine: it is the one number worth taking
    /// while something else is running, and the gate it answers to is the hard one.
    #[test]
    #[ignore = "measurement, not a test"]
    fn size_at_scale() {
        for n in [1_000_000usize, 10_000_000] {
            let hs = bigram_hashes(n);
            let t = std::time::Instant::now();
            let m = Mphf::build(&hs).expect("own build");
            let ms = t.elapsed().as_secs_f64() * 1e3;

            let mut seen = vec![false; n];
            for &h in &hs {
                let id = m.index(h) as usize;
                assert!(id < n, "id {id} out of range at n={n}");
                assert!(!seen[id], "id {id} handed out twice at n={n}");
                seen[id] = true;
            }

            println!(
                "n {n:>9}  bits/key {:>6.3}  (target <= 2.400)  pilots {:>9}  remap {:>8}  build {ms:>7.0} ms (loaded machine, not a timing claim)",
                m.bits_per_key(),
                m.pilots.len(),
                m.remap_off.len(),
            );
        }
    }

    /// Peak resident memory of a build, which is the question 10 M cannot answer: `owner` alone is
    /// four bytes for every slot. Reported next to the size so a redesign has a number to beat.
    #[test]
    #[ignore = "measurement, not a test"]
    fn peak_memory_at_100m() {
        let n = 100_000_000usize;
        let hs = bigram_hashes(n);
        let after_keys = peak_rss_mb();
        let t = std::time::Instant::now();
        let m = Mphf::build(&hs).expect("own build");
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "n {n}  bits/key {:>6.3}  build {ms:>8.0} ms  peak RSS {:>6} MB ({:>5.1} B/key, {:>5.1} of it the keys)",
            m.bits_per_key(),
            peak_rss_mb(),
            peak_rss_mb() as f64 * 1e6 / n as f64,
            after_keys as f64 * 1e6 / n as f64,
        );
        let mut seen = vec![false; n];
        for &h in &hs {
            let id = m.index(h) as usize;
            assert!(id < n && !seen[id], "not a bijection at n = {n}");
            seen[id] = true;
        }
    }

    /// `VmHWM` — the high-water mark, not the current size, so a freed peak still shows up.
    fn peak_rss_mb() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))?
                    .split_whitespace()
                    .nth(1)?
                    .parse::<u64>()
                    .ok()
            })
            .map_or(0, |kb| kb / 1024)
    }

    /// The spike's whole case: size, build time and lookup against the shipped `ptr_hash` alias,
    /// alternated in one process so a drifting machine moves both.
    #[test]
    #[ignore = "measurement, not a test"]
    fn against_the_ptr_hash_alias() {
        for n in [1_000_000usize, 10_000_000] {
            let hs = bigram_hashes(n);
            let mut own_build = f64::INFINITY;
            let mut ref_build = f64::INFINITY;
            let mut own_ns = f64::INFINITY;
            let mut ref_ns = f64::INFINITY;
            let mut own_bits = 0.0;

            // A-B-A-B in one process: two rounds of (ours, theirs), minimum of each.
            for _ in 0..2 {
                let t = std::time::Instant::now();
                let own = Mphf::build(&hs).expect("own build");
                own_build = own_build.min(t.elapsed().as_secs_f64() * 1e3);
                own_bits = own.bits_per_key();
                own_ns = own_ns.min(min_ns(
                    3,
                    &hs,
                    |m: &Mphf| hs.iter().map(|&h| m.index(h)).sum(),
                    &own,
                ));

                // The shipped alias, exactly as `PerfectHashIndex` builds it — `default_compact`
                // first, which is where the 2.169 bits/key baseline comes from.
                let t = std::time::Instant::now();
                let theirs = crate::hash::build_mph(&hs).expect("alias build");
                ref_build = ref_build.min(t.elapsed().as_secs_f64() * 1e3);
                ref_ns = ref_ns.min(min_ns(
                    3,
                    &hs,
                    |m: &ptr_hash::DefaultPtrHash| hs.iter().map(|&h| m.index(&h) as u64).sum(),
                    &theirs,
                ));
            }

            println!(
                "n {n:>9}\n                   bits/key  own {own_bits:>6.3}   (target <= 2.400)\n                   build ms  own {own_build:>8.0}  ptr_hash {ref_build:>8.0}   ratio {:>5.2}x (target <= 2.00x)\n                   id ns/key own {own_ns:>8.2}  ptr_hash {ref_ns:>8.2}   ratio {:>5.2}x (target <= 1.10x)",
                own_build / ref_build,
                own_ns / ref_ns,
            );
        }
    }
}
