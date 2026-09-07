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
//! One byte per bucket plus the remap. At `λ` keys per bucket that is `8/λ` bits per key, so the
//! bucket size is the whole size story and `α` (the table's fill) trades against how hard the search
//! is. The remap is a `u32` per slot above `n`, which is `32·(1−α)/α` bits per key — the part with
//! the most room left in it, and the first thing to compress if the rest holds up.

use crate::IndexError;

/// Keys per bucket, and the table's fill. Together they set both the size — `8/λ` bits per key plus
/// the remap — and the whole cost of construction, and the two do not trade the way an independent
/// pilot per bucket would suggest. A window pilot is not 256 independent tries but four runs of 64
/// correlated shifts, which makes a large bucket much dearer than the Poisson model predicts.
/// Measured at 10 M real-word bigram hashes, single-threaded, `u32` remap:
///
/// | λ, α | bits/key | build | with an Elias-Fano remap |
/// |---|---|---|---|
/// | 3.9, 0.99 | 2.383 | 11.9 s | 2.309 |
/// | 3.9, 0.98 | 2.712 | 3.6 s | 2.207 |
/// | 3.6, 0.98 | 2.883 | 2.0 s | **2.378** |
/// | 4.2, 0.97 | 2.903 | 41.8 s | 2.122 |
///
/// So (3.6, 0.98) builds 6× faster than (3.9, 0.99) at the same size — but only once the remap is
/// Elias-Fano rather than a `u32` per overflowing slot. Until then the only setting that meets the
/// 2.4 bits/key gate is the first row, and that is what ships here.
const LAMBDA: f64 = 3.9;

/// Table fill. Below 1 there are spare slots for the last buckets to land on; the leftovers above
/// `n` are what the remap pays for.
const ALPHA: f64 = 0.99;

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
    /// For each slot in `[n, slots)`, the free slot below `n` it stands for.
    remap: Vec<u32>,
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
        if hl < (u64::MAX / 5) * 3 {
            scale(hl.rotate_left(23), dense)
        } else {
            dense + scale(hl.rotate_left(23), buckets - dense)
        }
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

    /// The id of `h` in `[0, n)`.
    #[inline(always)]
    pub fn index(&self, h: u64) -> u64 {
        let hb = spread(h, self.seed);
        let part = scale(hb, self.parts);
        // `bucket_in_part` stays below `buckets_per_part`, so this stays below `pilots.len()`.
        let b = part * self.buckets_per_part
            + Self::bucket_in_part(hb, self.buckets_per_part, self.dense_buckets);
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
            u64::from(self.remap[(s - self.n) as usize])
        }
    }

    /// How many keys are in the image.
    pub fn n(&self) -> u64 {
        self.n
    }

    /// Bits per key, the number the whole spike is judged on.
    pub fn bits_per_key(&self) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        ((self.pilots.len() + self.remap.len() * 4) * 8) as f64 / self.n as f64
    }

    /// Build over `hashes`, which must already be distinct.
    ///
    /// Fails only if no global seed works, which needs input the bucket assignment cannot spread —
    /// duplicate hashes will do it, and the caller has already ruled those out.
    pub fn build(hashes: &[u64]) -> Result<Self, IndexError> {
        for attempt in 0..SEED_TRIES {
            let seed = mix(0xA5A5_5A5A_DEAD_BEEF ^ u64::from(attempt));
            if let Some(built) = Self::try_build(hashes, seed) {
                return Ok(built);
            }
        }
        Err(IndexError::Build(
            "minimal perfect hash: no seed placed every bucket",
        ))
    }

    fn try_build(hashes: &[u64], seed: u64) -> Option<Self> {
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
                remap: Vec::new(),
                seed,
            });
        }
        let parts = n.div_ceil(KEYS_PER_PART).max(1);
        let per_part = n.div_ceil(parts);
        let buckets_per_part = ((per_part as f64 / LAMBDA).ceil() as u64).max(1);
        let slots_per_part = ((per_part as f64 / ALPHA).ceil() as u64).max(1);
        let stride = slots_per_part + GUARD;
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

        // Group the keys by bucket, CSR-style: one counting pass, then one placing pass. A
        // `Vec<Vec<u64>>` would allocate once per bucket, which at λ ≈ 4 is a quarter of `n`.
        // Global bucket numbers are `part * buckets_per_part + local`, so a part's buckets — and
        // with them its keys — come out contiguous, which is what the per-part loop below needs.
        let bucket_of = |h: u64| {
            let hb = spread(h, seed);
            scale(hb, parts) * buckets_per_part
                + Self::bucket_in_part(hb, buckets_per_part, dense_buckets)
        };
        let mut start = vec![0u32; buckets as usize + 1];
        for &h in hashes {
            start[bucket_of(h) as usize + 1] += 1;
        }
        for i in 0..buckets as usize {
            start[i + 1] += start[i];
        }
        let mut cursor = start.clone();
        let mut by_bucket = vec![0u64; hashes.len()];
        for &h in hashes {
            let b = bucket_of(h) as usize;
            by_bucket[cursor[b] as usize] = h;
            cursor[b] += 1;
        }

        // Who owns each slot, so a colliding bucket can be evicted rather than the pilot grown.
        // `FREE` is the empty marker; there are fewer than `u32::MAX` buckets by construction.
        const FREE: u32 = u32::MAX;
        let mut owner = vec![FREE; slots as usize];
        // The same occupancy as one bit per slot, plus a spare word for `window`. Whether a pilot
        // is usable at all is a question about free slots and nothing else, and a part's bits are
        // 33 KiB against `owner`'s 1 MiB — the search runs out of L1 instead of L2.
        let mut taken = vec![0u64; (slots as usize).div_ceil(64) + 1];
        let mut pilots = vec![0u8; buckets as usize];
        let mut placed = vec![false; buckets as usize];
        let mut bases: [Vec<u64>; 4] = std::array::from_fn(|_| Vec::with_capacity(64));
        let mut victims: Vec<u32> = Vec::with_capacity(16);
        let mut best_victims: Vec<u32> = Vec::with_capacity(16);
        let mut part_seed: Vec<u64> = Vec::with_capacity(parts as usize);
        let mut order: Vec<u32> = Vec::with_capacity(buckets_per_part as usize);
        let mut queue: std::collections::VecDeque<u32> =
            std::collections::VecDeque::with_capacity(buckets_per_part as usize);
        let size_of = |b: u32| start[b as usize + 1] - start[b as usize];

        // A part at a time. Every slot a part can reach lies in its own window, so the displacement
        // search — which reads occupancy at random and is the whole cost of construction — works
        // inside one cache-resident slice instead of striding a table the size of the output.
        for part in 0..parts {
            let first = part * buckets_per_part;
            let slot_base = part * stride;

            // Largest bucket first: the hard ones are cheap only while the part is still empty.
            // The order is a property of the bucket sizes, which no retry changes.
            order.clear();
            order.extend(first as u32..(first + buckets_per_part) as u32);
            order.sort_unstable_by_key(|&b| std::cmp::Reverse(size_of(b)));
            let mut placed_part = false;
            for retry in 0..PART_TRIES {
                let pseed = mix(seed ^ part.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(retry));
                for s in slot_base..slot_base + stride {
                    owner[s as usize] = FREE;
                    taken[(s / 64) as usize] &= !(1 << (s % 64));
                }
                for b in first..first + buckets_per_part {
                    placed[b as usize] = false;
                }
                queue.clear();
                queue.extend(order.iter().copied());

                // Displacement is bounded so a part that livelocks is retried instead of looping.
                // The bound is tight on purpose, because the two cases separate cleanly: measured at
                // 10 M keys, every part that finished did so in 1.2–1.9 pops per bucket, while the
                // four that circulated burned 64 each before the old loose bound gave up — and every
                // one of them placed on the next seed. Four times the bucket count is twice the worst
                // healthy part and a thirtieth of a stuck one.
                let mut budget = 4 * buckets_per_part + 4096;
                // A cuckoo table cycles when two buckets keep taking each other's slots. The standard
                // guard is to refuse to evict anything displaced in the last few steps; 16 is what
                // PtrHash uses, but the ring may not cover a sizeable share of a small part, or it
                // would forbid every eviction there is and fail on a bucket it could have placed.
                let recent_len = (buckets_per_part as usize / 4).clamp(1, 16);
                let mut recent = [u32::MAX; 16];
                let mut recent_at = 0usize;
                let mut failed = false;

                while let Some(b) = queue.pop_front() {
                    let keys =
                        &by_bucket[start[b as usize] as usize..start[b as usize + 1] as usize];
                    // Empty buckets need no pilot; already-placed ones are stale queue entries left by
                    // an eviction that was itself undone, and re-placing them only churns the part.
                    if keys.is_empty() || placed[b as usize] {
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
                            let base = Self::base(h, pseed, j, slots_per_part);
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
                            free &= !window(&taken, slot_base + base);
                            if free == 0 {
                                break;
                            }
                        }
                        if free != 0 {
                            chosen =
                                Some((((j as u8) << 6) | free.trailing_zeros() as u8, 0, true));
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
                                    let o = owner[(slot_base + base + d) as usize];
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
                                    Some((_, best, was_downhill)) => match (downhill, was_downhill)
                                    {
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
                        let vk =
                            &by_bucket[start[v as usize] as usize..start[v as usize + 1] as usize];
                        let pv = pilots[v as usize];
                        for &h in vk {
                            let s = slot_base
                                + Self::base(h, pseed, usize::from(pv >> 6), slots_per_part)
                                + u64::from(pv & 63);
                            if owner[s as usize] == v {
                                owner[s as usize] = FREE;
                                taken[(s / 64) as usize] &= !(1 << (s % 64));
                            }
                        }
                        placed[v as usize] = false;
                        recent[recent_at] = v;
                        recent_at = (recent_at + 1) % recent_len;
                        queue.push_front(v);
                    }
                    best_victims.clear();

                    let (j, d) = (usize::from(pilot >> 6), u64::from(pilot & 63));
                    for &base in &bases[j] {
                        let s = slot_base + base + d;
                        owner[s as usize] = b;
                        taken[(s / 64) as usize] |= 1 << (s % 64);
                    }
                    pilots[b as usize] = pilot;
                    placed[b as usize] = true;
                }

                if !failed {
                    part_seed.push(pseed);
                    placed_part = true;
                    break;
                }
            }
            if !placed_part {
                return None;
            }
        }

        // Minimal at last: every occupied slot at or above `n` is redirected to a hole below it.
        // There are exactly as many of each, because the table holds `n` keys in `slots` slots.
        let mut holes = (0..n).filter(|&s| taken[(s / 64) as usize] >> (s % 64) & 1 == 0);
        let mut remap = vec![0u32; (slots - n) as usize];
        for (i, entry) in remap.iter_mut().enumerate() {
            let s = n + i as u64;
            if taken[(s / 64) as usize] >> (s % 64) & 1 == 1 {
                // A hole must exist: occupancy below `n` plus occupancy above it is exactly `n`.
                *entry = holes.next().expect("a hole for every overflowing slot") as u32;
            }
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
            remap,
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
                m.remap.len(),
            );
        }
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
