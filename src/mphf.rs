//! The minimal perfect hash behind both hash indexes, in-crate so that a loader can bound every
//! read it makes.
//!
//! It exists for that reason and not for speed or size: the `ptr_hash` crate it replaced keeps its
//! pilot table private, so a blob holding one could not be validated from outside the crate that
//! owned it, and that forced `from_bytes`, `load` and `load_mmap` to be `unsafe fn` on both
//! indexes. An MPH whose every array has a length in *our* header can be checked, and those
//! loaders are safe.
//!
//! # Construction
//!
//! [PHast]'s map-or-bump. Keys are grouped into buckets by their hash, and each bucket gets a
//! one-byte *seed*. Ten bits of a key's hash are its offset inside a slice of 1024 values whose
//! start is another function of the hash; the seed moves every key of the bucket two values per
//! step, wrapping inside its slice, and is chosen so that every key lands on a value nothing has
//! taken. Slice start and bucket both grow with the hash, so seeding the buckets in nearly
//! ascending order keeps the live edge of the occupancy map in L1 and never returns to anything
//! behind it. The seeds that place a bucket are read off one bitwise OR of its keys' occupancy
//! windows, 64 seeds at a time, and of those the one whose values are lowest is taken: low values
//! are what the buckets still to come cannot use. A bucket no seed places is *bumped* (seed 0)
//! and its keys go to a second, smaller table under a fresh hash, and so on down to a tail of a
//! few hundred keys placed by exhaustive search. Bumped keys land in the holes the first table
//! left, through a remap of every lower table's value to a hole; that is what makes the result
//! minimal.
//!
//! Nothing is ever displaced. The [PtrHash]-shaped builder this replaced evicted buckets that were
//! in the way and re-placed them, and three quarters of its time was that pass reading a random
//! slot-owner table. Bumping costs the bumped keys' own seeds and a remap entry each instead, and
//! the construction is a streaming pass over sorted keys. [PTHash] is the common ancestor of all
//! three.
//!
//! # Space
//!
//! `8/λ` bits per key for the first level's seeds, plus what the bumped keys cost: their own
//! levels' seeds and an Elias–Fano hole per lower-level value. The trade against `λ` is
//! tabulated on [`LAMBDA`].
//!
//! # Format
//!
//! `MPH3` is what this version writes. `MPH2`, the same tables 1.1 to 3.0 wrote with one offset
//! field and 255 shifts a seed, is still read under its own seed geometry; so is `MPH1`, the
//! eviction-based table 1.0 wrote, whose lookup is a different function over a different header.
//!
//! [PTHash]: https://arxiv.org/abs/2104.10402
//! [PHast]: https://arxiv.org/abs/2504.17918
//! [PtrHash]: https://arxiv.org/abs/2502.15539

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::IndexError;
use crate::pages::{Pages, Zeroed};

/// The one error every overflow check below reports; naming it keeps the arithmetic readable.
const SIZE: IndexError = IndexError::Format("mphf: blob sections do not fit in memory");

/// Keys per bucket on every bumping level. Seeds are `8/λ` bits per key, and every bucket that no
/// seed places is bumped, so a larger `λ` is fewer seeds but more bumped keys, each of which
/// costs its own level's seed share and ~9 bits of remap. Measured on 10 M word-bigram hashes,
/// one thread, one run (`MPH3`; `MPH2` was 2.154 / 2.089 / 2.070 bits at 4.15 / 4.5 / 4.7):
///
/// | λ    | bits/key | bumped | build ns/key |
/// |------|----------|--------|--------------|
/// | 4.2  | 1.989    | 0.7 %  | 69           |
/// | 4.35 | 1.962    | 1.1 %  | 67           |
/// | 4.5  | 1.954    | 1.6 %  | 65           |
/// | 4.65 | 1.951    | 2.2 %  | 65           |
/// | 4.8  | 1.959    | 2.9 %  | 65           |
///
/// The bits are flat from 4.5 to 4.65 and a bumped key is the lookup's cost — a chain of
/// dependent lines where a placed key is one — so the lower `λ` of the two is the one shipped.
///
/// A ratio, `9 / 2`, so the bucket count is exact integer arithmetic: see [`ceil_div_ratio`].
const LAMBDA: (u64, u64) = (9, 2);

/// Slice length on a level big enough to afford it; smaller levels use a shorter one, see
/// [`slice_for`]. A key's values under every seed stay inside its slice.
const SLICE: u64 = 1024;

/// Values between two consecutive shifts of one key on an `MPH2` level, a power of two. Two
/// rather than one spreads a bucket's candidate placements over twice the slice for the same 255
/// seeds, which is worth a tenth of the bumped keys. An `MPH3` level's stride is its slice over
/// its shifts, so that a key's shifts run once round its slice.
const STRIDE: u64 = 2;

/// Mode bits of an `MPH3` seed: its top bits pick which field of the hash is a key's offset in
/// its slice, the rest are its shift — four fields and 64 shifts each. A bucket's keys are one
/// rigid constellation a mode, which the shifts rotate; two keys of a bucket on one value in one
/// mode, stuck there under every shift, part in the others, and four constellations chosen among
/// by the lowest sum pack tighter than one: measured on 10 M word-bigram hashes, 1.58 % of the
/// keys bumped and 1.954 bits per key against `MPH2`'s 3.03 % and 2.088, for 1.4 times the
/// build. `MPH2` is zero mode bits: one field, 255 shifts.
const MODE_BITS: u32 = 2;

/// [`MODE_BITS`] as the header writes it.
const MODE_BITS_BYTE: u8 = MODE_BITS as u8;

/// Buckets the placement order may look ahead. A bucket of one key is held back until every
/// larger bucket within roughly a slice ahead of it is placed, because a single key fits any hole
/// while a large bucket needs a run of them; this bounds the wait.
const WINDOW: u32 = 512;

/// Bucket-size term of the placement priority for sizes 1..=7, in units of 1024 (one bucket of
/// index) for a slice of 1024 positions: the PHast delta=2 table. Measured 0.03 bits/key below
/// the lag heuristic it replaced, at no build-time cost. See [`ell`].
const WEIGHTS: [i64; 7] = [-50137, 65904, 111782, 139890, 159029, 175922, 186995];

/// Per-bucket working memory of a placement pass, kept across buckets so that a bucket's search
/// allocates and zeroes nothing.
struct Scratch {
    /// The smallest value each key can take on this map.
    starts: [u64; MAX_BUCKET],
    /// Each key's offset in its slice.
    offs: [u64; MAX_BUCKET],
    /// The keys' wrap shifts, sorted; `cuts[..nc]`.
    cuts: [u64; MAX_BUCKET],
    nc: usize,
    /// Each key's wrap shift, in key order.
    cut: [u64; MAX_BUCKET],
    /// Where each key's shifts are on the map: the plane's word offset and the bit of shift 0.
    plane: [usize; MAX_BUCKET],
    bit: [u64; MAX_BUCKET],
    /// The keys' values under a candidate shift.
    vals: [u64; MAX_BUCKET],
    /// How far past the best seed's sum an interval's floor may be and still be scanned; 0 is
    /// the exact minimum, a sweep may trade some of it for time.
    margin: u64,
}

impl Scratch {
    fn new() -> Self {
        Self {
            starts: [0; MAX_BUCKET],
            offs: [0; MAX_BUCKET],
            cuts: [0; MAX_BUCKET],
            nc: 0,
            cut: [0; MAX_BUCKET],
            plane: [0; MAX_BUCKET],
            bit: [0; MAX_BUCKET],
            vals: [0; MAX_BUCKET],
            margin: tun(16, 0.0) as u64,
        }
    }
}

/// Size classes of the placement order: a bucket of more keys than this is ordered as if it had
/// this many. Its priority term is what [`ell`] gives that size.
const CLASSES: usize = 16;

/// Largest bucket a seed is searched for. Above this a bucket is bumped outright: it could only
/// arise from structured input, and its keys spread out under the next level's hash.
const MAX_BUCKET: usize = 64;

/// A level with this many keys or fewer is placed by the tail instead of bumping further. A level
/// is cheaper per key than the tail as long as it is long enough to bump few of them.
const TAIL_KEYS: u64 = 256;

/// Keys per bucket in the tail, whose seeds are two bytes and searched exhaustively: `6.5`, as a
/// ratio.
const TAIL_LAMBDA: (u64, u64) = (13, 2);

/// Fill of the tail's table; the slack above its keys is what lets the last buckets place: `0.96`,
/// as a ratio.
const TAIL_ALPHA: (u64, u64) = (24, 25);

/// Hash seeds the tail tries before construction fails. Only duplicate hashes get there.
const TAIL_TRIES: u32 = 64;

/// Bumping levels at most. Each bumps a few percent of its keys, so the tail is reached after
/// three or four; the bound is for input a level cannot spread.
const MAX_LEVELS: usize = 16;

/// Most buckets per chunk. Chunks are placed independently, in parallel, and every boundary
/// between two costs about two hundred keys bumped, so they are few and large.
const CHUNK: u64 = 1 << 16;

/// Fewest buckets per chunk on a level after the first: about the window, so that a chunk's
/// placement order is the level's. A first level is cut no finer than a quarter chunk: a boundary
/// costs about two hundred keys whatever the level, and a level that small builds in milliseconds.
const MIN_CHUNK: u64 = 1 << 12;

/// Chunks a level too small for whole chunks is cut into; and the pieces the last two chunks'
/// worth of a first level is cut into, so that however many threads there are finish together.
const PIECES: u64 = 8;

/// Where each chunk of a level of `buckets` starts; the last runs to the end. A boundary between
/// two chunks costs about two hundred keys bumped whatever the level's size, so the first level,
/// which holds nearly every key, is cut into chunks of [`CHUNK`] only — and a level below two of
/// them is one chunk — while a `deep` level, a few per cent of the keys, is cut into [`PIECES`]
/// so that it too spreads over the threads. Derived from the level, never from the thread
/// count, so that the count cannot reach the result.
fn chunk_starts(buckets: u64, deep: bool) -> Vec<u64> {
    let pieces = tun(17, PIECES as f64) as u64;
    let tail = tun(19, PIECES as f64) as u64;
    let mut starts = vec![0];
    let mut at = 0;
    if deep || buckets < 4 * CHUNK || tail == 0 {
        let floor = if deep { MIN_CHUNK } else { CHUNK / 4 };
        let size = (buckets / pieces).clamp(floor, CHUNK);
        while buckets - at >= 2 * size {
            at += size;
            starts.push(at);
        }
    } else {
        while buckets - at >= 3 * CHUNK {
            at += CHUNK;
            starts.push(at);
        }
        let small = 2 * CHUNK / tail;
        while buckets - at >= 2 * small {
            at += small;
            starts.push(at);
        }
    }
    starts
}

/// Share of the values at the start of a run held back for the gap before it, at the run's
/// first value; the share falls linearly to nothing a slice on. `0.966`, in fixed point over 2^52.
const HELD: u64 = 4_350_477_240_039_899;

/// The gap before bucket `b` is placed after the run from `b`, and reaches a slice past `b`'s
/// first value. Mark taken in `map`, the run's, a share of those values held back for it: in
/// the middle of a level the buckets before `b` take a share falling linearly from [`HELD`] at
/// `b`'s first value to nothing a slice on, since a key lands evenly across its slice. Which
/// values is a draw fixed by the level and the value. Returns what was marked, to clear once
/// the run is placed.
fn reserve(level: &Level, b: u64, origin: u64, map: &mut Map) -> Vec<u64> {
    let held = held_share();
    let slice = level.slice;
    let lo = ((b as u128 * level.n as u128) / level.buckets as u128) as u64;
    let mut marked = Vec::new();
    for y in 0..slice.min(level.n - lo) {
        let draw = mix(0x9E37_79B9_7F4A_7C15 ^ (b << 20) ^ y) >> 12;
        if draw < held * (slice - y) / slice {
            let v = lo + y - origin;
            map.set(v);
            marked.push(v);
        }
    }
    marked
}

/// Remap entries per block base in the 1.0 format.
const REMAP_BLOCK: usize = 256;

/// First-level seeds, a byte each, from which [`V2::index_all`] prefetches them: a smaller level
/// sits in L2 on current cores, where a prefetch is only more work.
const PREFETCH_SEEDS: usize = 1 << 18;

/// Keys between a first-level seed's prefetch and its read in [`V2::index_all`].
const SEED_AHEAD: usize = 64;

/// Keys [`V2::index_all`] takes through the first level before it answers those it bumped.
const BATCH_BLOCK: usize = 1024;

/// Bumped keys between one stage of [`V2::resolve_bumped`] and the next.
const STAGE_GAP: usize = 8;

/// Multiply-shift range reduction: `x` scaled into `[0, k)` without a division.
#[inline(always)]
fn scale(x: u64, k: u64) -> u64 {
    ((x as u128 * k as u128) >> 64) as u64
}

/// A cheap bijective mix, used to derive independent streams from one hash.
#[inline(always)]
fn mix(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The hash a bumping level after the first sees. The first level reads the key hash as it is,
/// because it arrives avalanched; every later one mixes it under the level's own salt, so a bucket
/// that stuck together on one level comes apart on the next.
#[inline(always)]
fn level_hash(h: u64, level: usize) -> u64 {
    mix(h ^ (level as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

const ONES: u64 = 0x0101_0101_0101_0101;

/// `SELECT_IN_BYTE[r << 8 | b]` is the position of the `r`-th set bit of the byte `b`, from 0.
const SELECT_IN_BYTE: [u8; 2048] = {
    let mut table = [0u8; 2048];
    let mut b = 0usize;
    while b < 256 {
        let (mut r, mut bit) = (0usize, 0usize);
        while bit < 8 {
            if (b >> bit) & 1 == 1 {
                table[(r << 8) | b] = bit as u8;
                r += 1;
            }
            bit += 1;
        }
        b += 1;
    }
    table
};

/// The position of the `r`-th set bit of `x`, from 0, for `r` below `x`'s set bits; past them,
/// some position in the word. The byte holding it comes from the running byte counts compared all
/// at once, the bit from a table, and nothing branches: which bit it is is as unpredictable as the
/// key that asked.
#[inline(always)]
fn select_in_word(x: u64, r: u64) -> u64 {
    let s = x - ((x >> 1) & 0x5555_5555_5555_5555);
    let s = (s & 0x3333_3333_3333_3333) + ((s >> 2) & 0x3333_3333_3333_3333);
    // Byte `i` counts the set bits of bytes `0..=i`, at most 64.
    let sums = ((s + (s >> 4)) & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(ONES);
    // The high bit of byte `i` is set where that count is at most `r`: both are below 128, so no
    // byte borrows from the next.
    let at_most = (((r & 0x7F).wrapping_mul(ONES) | (ONES << 7)) - sums) & (ONES << 7);
    let byte = ((((at_most >> 7).wrapping_mul(ONES)) >> 56) * 8).min(56);
    let before = ((sums << 8) >> byte) & 0xFF;
    let rank = r.wrapping_sub(before) & 7;
    byte + u64::from(SELECT_IN_BYTE[((rank << 8) | ((x >> byte) & 0xFF)) as usize])
}

/// Occupancy of a level's values, laid out so that the values one key can take under consecutive
/// shifts are consecutive bits: value `v` is bit `v >> shift` of plane `v & (stride - 1)`. The
/// seed search then reads 64 shifts of a key in one window whatever the stride.
struct Map {
    shift: u32,
    /// Words per plane, with spare words at each plane's end so that a window read from the last
    /// value is in bounds.
    plane: usize,
    words: Vec<u64>,
}

impl Map {
    fn new(values: u64, shift: u32) -> Self {
        let plane = ((values >> shift) / 64) as usize + 3;
        Self {
            shift,
            plane,
            words: vec![0; plane << shift],
        }
    }

    /// The plane of `v`, as a word offset, and its bit index within the plane.
    #[inline(always)]
    fn at(&self, v: u64) -> (usize, u64) {
        let plane = (v & ((1 << self.shift) - 1)) as usize;
        (plane * self.plane, v >> self.shift)
    }

    #[inline(always)]
    fn get(&self, v: u64) -> bool {
        let (p, i) = self.at(v);
        self.words[p + (i / 64) as usize] >> (i % 64) & 1 == 1
    }

    #[inline(always)]
    fn set(&mut self, v: u64) {
        let (p, i) = self.at(v);
        self.words[p + (i / 64) as usize] |= 1 << (i % 64);
    }

    fn clear(&mut self, v: u64) {
        let (p, i) = self.at(v);
        self.words[p + (i / 64) as usize] &= !(1 << (i % 64));
    }

    /// 64 bits of the plane at word offset `p`, from bit `i`.
    #[inline(always)]
    fn window(&self, p: usize, i: u64) -> u64 {
        let pair = &self.words[p + (i / 64) as usize..][..2];
        let o = i % 64;
        (pair[0] >> o) | (pair[1] << 1 << (63 - o))
    }

    /// The values below `n` no key landed on, in order: a word of every plane at a time, their
    /// zero bits merged in bit order. Yielded as found, so that the list never exists.
    fn holes(&self, n: u64) -> impl Iterator<Item = u64> + '_ {
        let planes = 1usize << self.shift;
        let words = if n == 0 {
            0
        } else {
            (((n - 1) >> self.shift) / 64 + 1) as usize
        };
        (0..words).flat_map(move |w| {
            let mut free = 0u64;
            for p in 0..planes {
                free |= !self.words[p * self.plane + w];
            }
            std::iter::from_fn(move || {
                (free != 0).then(|| {
                    let j = free.trailing_zeros();
                    free &= free - 1;
                    j
                })
            })
            .flat_map(move |j| {
                let i = (w as u64 * 64 + u64::from(j)) << self.shift;
                (0..planes).filter_map(move |p| {
                    let v = i | p as u64;
                    (v < n && self.words[p * self.plane + w] >> j & 1 == 0).then_some(v)
                })
            })
        })
    }

    /// OR `other` into this map, plane by plane, `other`'s first word at word `at` of each plane
    /// of this one — before its first when negative. Only the words both have are touched.
    fn merge(&mut self, other: &Map, at: i64) {
        self.combine(other, at, |d, s| *d |= s);
    }

    fn combine(&mut self, other: &Map, at: i64, f: impl Fn(&mut u64, u64)) {
        let from = at.max(0) as usize;
        let to = (at + other.plane as i64).clamp(from as i64, self.plane as i64) as usize;
        for p in 0..1usize << self.shift {
            let dst = &mut self.words[p * self.plane + from..p * self.plane + to];
            let src = &other.words[p * other.plane + (from as i64 - at) as usize..][..to - from];
            for (d, &s) in dst.iter_mut().zip(src) {
                f(d, s);
            }
        }
    }
}

/// Slice length for a level of `n` keys: the full [`SLICE`] from 2^15 keys, 512 from 2^12, 256
/// below. A shorter slice is fewer values a key can take, and a longer one is a level of fewer
/// slices, whose end — the buckets whose slices wrap into its start — is a larger share of it;
/// the thresholds are where the first level's bumped share crossed, measured over 4 k–100 k
/// keys. Never longer than the level: 255 shifts at stride 1 need 256 values.
fn slice_for(n: u64) -> u64 {
    let natural = if n >= 1 << 15 {
        SLICE
    } else if n >= 1 << 12 {
        512
    } else {
        256
    };
    natural.min(tun(1, SLICE as f64) as u64)
}

/// Magic of a standalone minimal-perfect-hash blob, and its format version.
const MAGIC: &[u8; 4] = b"MPH3";
const FORMAT: u16 = 3;

/// The magic and version 1.1 to 3.0 wrote: the same tables over [`Geometry::Mph2`]'s seeds, with
/// the remap in an older layout, read and converted.
const MAGIC_V2: &[u8; 4] = b"MPH2";
const FORMAT_V2: u16 = 2;

/// The seed geometry of a table's levels, which the header's mode-bits byte names; an `MPH2`
/// blob's is the first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Geometry {
    /// One offset field and 255 shifts a seed, at [`STRIDE`]: zero mode bits.
    Mph2,
    /// [`MODE_BITS`] mode bits, the shifts at a stride of the slice over them.
    Mph3,
}

impl Geometry {
    fn mode_bits(self) -> u32 {
        match self {
            Self::Mph2 => 0,
            Self::Mph3 => tun(2, f64::from(MODE_BITS)) as u32,
        }
    }
}

/// Magic 4, version 2, the levels' mode bits 1, reserved 1, then seven `u64` scalars: `n`, level
/// count, tail keys, tail buckets, tail range, tail seed, the remap's low bits (`MPH2`: its hole
/// count). A level row of three `u64` per level follows, then a `u32` check over all of it.
const FIXED: usize = 4 + 2 + 2 + 7 * 8;

/// Keys, buckets and slice length of one level.
const LEVEL_ROW: usize = 3 * 8;

/// Header bytes for a table of `levels` bumping levels, check included.
fn header_len(levels: usize) -> usize {
    FIXED + levels * LEVEL_ROW + 4
}

/// Construction parameters, overridable from the measurements so a sweep needs no rebuild.
#[cfg(all(test, feature = "bench-mphf"))]
mod tune {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub const N: usize = 20;
    pub static SET: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    pub static VAL: [AtomicU64; N] = [const { AtomicU64::new(0) }; N];
    pub fn get(i: usize, default: f64) -> f64 {
        if SET[i].load(Ordering::Relaxed) == 1 {
            f64::from_bits(VAL[i].load(Ordering::Relaxed))
        } else {
            default
        }
    }
    pub fn set(i: usize, v: f64) {
        VAL[i].store(v.to_bits(), Ordering::Relaxed);
        SET[i].store(1, Ordering::Relaxed);
    }
}

#[cfg(all(test, feature = "bench-mphf"))]
fn tun(i: usize, default: f64) -> f64 {
    tune::get(i, default)
}

/// Wall time between consecutive marks of one build, printed when `LEXINDEX_MPHF_PHASES` is set;
/// how a measurement tells a serial phase from a parallel one.
#[cfg(all(test, feature = "bench-mphf"))]
fn phase(label: &str) {
    use std::cell::Cell;
    use std::time::Instant;
    thread_local!(static LAST: Cell<Option<Instant>> = const { Cell::new(None) });
    if std::env::var_os("LEXINDEX_MPHF_PHASES").is_none() {
        return;
    }
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| -> u64 {
        status
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            .unwrap_or(0)
    };
    LAST.with(|c| {
        let now = Instant::now();
        if let Some(prev) = c.get() {
            eprintln!(
                "phase {label:<20} {:>8.2} ms  rss {:>6} MB  hwm {:>6} MB",
                (now - prev).as_secs_f64() * 1e3,
                kb("VmRSS:") / 1024,
                kb("VmHWM:") / 1024,
            );
        }
        c.set(Some(now));
    });
}

#[cfg(not(all(test, feature = "bench-mphf")))]
#[inline(always)]
fn phase(_: &str) {}

/// Work counters of the seed search, for the sweep: buckets searched, intervals scanned, window
/// reads, candidate shifts checked.
#[cfg(all(test, feature = "bench-mphf"))]
mod work {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub static BUCKETS: AtomicU64 = AtomicU64::new(0);
    pub static INTERVALS: AtomicU64 = AtomicU64::new(0);
    pub static WINDOWS: AtomicU64 = AtomicU64::new(0);
    pub static CANDIDATES: AtomicU64 = AtomicU64::new(0);
    pub static ON: AtomicU64 = AtomicU64::new(0);
    #[inline(always)]
    pub fn add(c: &AtomicU64, n: u64) {
        if ON.load(Relaxed) == 1 {
            c.fetch_add(n, Relaxed);
        }
    }
    pub fn reset(on: bool) {
        for c in [&BUCKETS, &INTERVALS, &WINDOWS, &CANDIDATES] {
            c.store(0, Relaxed);
        }
        ON.store(u64::from(on), Relaxed);
    }
    pub fn report() -> String {
        let b = BUCKETS.load(Relaxed).max(1) as f64;
        format!(
            "per bucket: intervals {:.2} windows {:.2} candidates {:.2}",
            INTERVALS.load(Relaxed) as f64 / b,
            WINDOWS.load(Relaxed) as f64 / b,
            CANDIDATES.load(Relaxed) as f64 / b
        )
    }
}

#[cfg(all(test, feature = "bench-mphf"))]
macro_rules! count {
    ($c:ident, $n:expr) => {
        work::add(&work::$c, $n)
    };
}

#[cfg(not(all(test, feature = "bench-mphf")))]
macro_rules! count {
    ($c:ident, $n:expr) => {};
}

#[cfg(not(all(test, feature = "bench-mphf")))]
#[inline(always)]
fn tun(_: usize, default: f64) -> f64 {
    default
}

/// The priority table for bucket sizes 1..=7: [`WEIGHTS`], unless a sweep sets one.
#[cfg(all(test, feature = "bench-mphf"))]
fn weights() -> [i64; 7] {
    if tune::SET[8].load(std::sync::atomic::Ordering::Relaxed) == 1 {
        let mut w = [0i64; 7];
        for (i, v) in w.iter_mut().enumerate() {
            *v = tun(8 + i, 0.0) as i64;
        }
        w
    } else {
        WEIGHTS
    }
}

#[cfg(not(all(test, feature = "bench-mphf")))]
#[inline(always)]
fn weights() -> [i64; 7] {
    WEIGHTS
}

/// The stride of a level with `slice` under `mode_bits`: with modes, the slice over the shifts,
/// so that a key's shifts run once round its slice; without, [`STRIDE`], or less on a slice too
/// short for 255 shifts at that stride to be distinct positions.
fn stride_for(slice: u64, mode_bits: u32) -> u64 {
    let natural = if mode_bits == 0 {
        (slice >> 8).clamp(1, STRIDE)
    } else {
        slice >> (8 - mode_bits)
    };
    let stride = tun(7, natural as f64) as u64;
    debug_assert!(stride.is_power_of_two());
    stride
}

/// Bucket-size term of the placement priority: [`WEIGHTS`] up to seven keys, linear past that,
/// scaled with the slice. A term is a delay in buckets, and what transfers between slice sizes
/// is the delay as a share of the buckets whose slices overlap one slice: measured at 10 k keys
/// (slice 512), the table halved gives 2.20 bits/key against 2.27 unscaled.
fn ell(k: usize, slice: u64) -> i64 {
    let w = weights();
    let term = match k {
        0 => w[0],
        1..=7 => w[k - 1],
        _ => w[6] + (w[6] - w[5]) * (k as i64 - 7),
    };
    term * slice as i64 / 1024
}

/// A minimal perfect hash over a set of 64-bit key hashes.
///
/// Maps each hash that was built in to a distinct value in `[0, n)`. A hash that was *not* built in
/// gets some value in that range too — membership is the caller's problem, as with any minimal
/// perfect hash, and both indexes in this crate answer it with a stored key or a fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mphf {
    table: Table,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Table {
    /// The 1.0 format, readable but no longer written.
    V1(V1),
    V2(V2),
}

/// One bumping level: a seed per bucket over the keys it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Level {
    /// Keys this level was built over, and its range of values.
    n: u64,
    buckets: u64,
    /// A power of two; `h & (slice - 1)` is a key's offset in its slice, which starts anywhere
    /// in `[0, n)` and wraps past `n` to the range's beginning, so that every value is reachable
    /// from the same number of starts and the range has no underfilled ends.
    slice: u64,
    /// Log2 of the stride — the values between two consecutive shifts of a key, [`stride_for`]
    /// the slice — kept because it is on every lookup.
    shift: u32,
    /// Mode bits of the level's seeds: [`Geometry::mode_bits`].
    mode_bits: u32,
    /// One per bucket; 0 is bumped.
    seeds: Pages<u8>,
}

/// `ceil(n / (num / den))`, exactly. Every size the format derives from a load factor — a level's
/// buckets, the tail's buckets and range — goes through here, so the blob a key set builds to is
/// integer arithmetic over the keys and never a question of how a target rounds a division.
fn ceil_div_ratio(n: u64, (num, den): (u64, u64)) -> u64 {
    (u128::from(n) * u128::from(den)).div_ceil(u128::from(num)) as u64
}

/// A ratio tunable: the shipped constant, or the sweep's `f64` read to a thousandth, so `4.5` is
/// still `9 / 2` when a sweep sets it.
fn ratio(i: usize, default: (u64, u64)) -> (u64, u64) {
    let swept = tun(i, f64::NAN);
    if swept.is_nan() {
        default
    } else {
        ((swept * 1000.0).round() as u64, 1000)
    }
}

/// [`HELD`], or the share a sweep sets, over 2^52.
fn held_share() -> u64 {
    let swept = tun(18, f64::NAN);
    if swept.is_nan() {
        HELD
    } else {
        (swept * (1u64 << 52) as f64) as u64
    }
}

impl Level {
    /// The shape of a level over `n` keys, before its seeds are found. `n` must be at least the
    /// slice, which every level above [`TAIL_KEYS`] is.
    fn shape(n: u64, geometry: Geometry) -> Self {
        let slice = slice_for(n);
        let mode_bits = geometry.mode_bits();
        Self {
            n,
            buckets: ceil_div_ratio(n, ratio(0, LAMBDA)).max(1),
            slice,
            shift: stride_for(slice, mode_bits).trailing_zeros(),
            mode_bits,
            seeds: Pages::default(),
        }
    }

    /// A key's offset in its slice under `mode`: ten bits of its hash, a different field per
    /// mode, so that two keys on one value in one mode are on different ones in another.
    #[inline(always)]
    fn offset(&self, h: u64, mode: u32) -> u64 {
        (h >> (8 * mode)) & (self.slice - 1)
    }

    /// The value of `h` under `seed`, which is nonzero: the seed's mode picks the key's offset in
    /// its slice, its shift moves the key that many strides on, wrapped inside the slice, from
    /// the slice's start, wrapped inside the range. Below `n` for every `h` and every seed.
    #[inline(always)]
    fn value(&self, h: u64, seed: u8) -> u64 {
        self.value_in(self.form(), h, seed)
    }

    /// [`value`](Self::value) under `form`, the level's own or [`Self::SHIPPED`] where a caller
    /// has checked they agree: a loop over many keys then reads the geometry as immediates.
    #[inline(always)]
    fn value_in(&self, form: Form, h: u64, seed: u8) -> u64 {
        let seed = u64::from(seed);
        let shift_bits = 8 - form.mode_bits;
        let mode = (seed >> shift_bits) as u32;
        let t = seed & ((1u64 << shift_bits) - 1);
        let offset = (h >> (8 * mode)) & form.mask;
        let v = scale(h, self.n) + ((offset + (t << form.shift)) & form.mask);
        if v >= self.n { v - self.n } else { v }
    }

    /// The geometry [`value`](Self::value) reads.
    #[inline(always)]
    fn form(&self) -> Form {
        Form {
            mode_bits: self.mode_bits,
            shift: self.shift,
            mask: self.slice - 1,
        }
    }

    /// The geometry of every level of [`SLICE`] values a slice that this version builds.
    const SHIPPED: Form = Form {
        mode_bits: MODE_BITS,
        shift: (SLICE >> (8 - MODE_BITS)).trailing_zeros(),
        mask: SLICE - 1,
    };
}

/// What [`Level::value`] needs of a level's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Form {
    mode_bits: u32,
    shift: u32,
    mask: u64,
}

/// The last level: no bumping, two-byte seeds, a table with slack, and a hash seed of its own
/// because it is the one level that can fail and be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tail {
    keys: u64,
    buckets: u64,
    /// Its range of values, `keys / TAIL_ALPHA`; the remap covers all of it.
    range: u64,
    seed: u64,
    seeds: Vec<u16>,
}

impl Tail {
    const EMPTY: Self = Self {
        keys: 0,
        buckets: 0,
        range: 0,
        seed: 0,
        seeds: Vec::new(),
    };

    /// Where `h` lands under `seed`: an independent position per seed, so the search is a plain
    /// sequence of tries rather than a slide.
    #[inline(always)]
    fn position(h: u64, seed: u16, range: u64) -> u64 {
        let salted = h.wrapping_add(u64::from(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        scale(mix(salted), range)
    }
}

/// Values a line of the remap holds, so that a value's line is its index shifted.
const LINE: usize = 128;

/// Bits of a line past its base: the high parts of its [`LINE`] values in unary.
const LINE_BITS: usize = 512 - 32;

/// The unary bits of a sparse line, which keeps one more low bit a value in its last two words.
const SPARSE_LINE_BITS: usize = LINE_BITS - LINE;

/// A 64-byte line of the remap: a `u32` base in the low half of the first word, then the high
/// parts of [`LINE`] values above the base in unary — a zero a step, a one a value. Where a
/// line's values are spread too far for that, the base's top bit marks it *sparse*: its high
/// parts are one bit shorter, so the steps are half as many, and the bit that leaves each
/// value is the value's bit in the line's last two words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, align(64))]
struct Line([u64; 8]);

// SAFETY: eight words, each of which may be zero.
unsafe impl Zeroed for Line {}

/// The base's top bit: the line is sparse.
const SPARSE: u64 = 1 << 31;

/// The remap: which hole of the first level each value of the levels below it, and of the tail,
/// takes. The `j`-th value a key landed on takes the `j`-th hole, and a value no key landed on —
/// a hash that was never built in — takes its predecessor's, so over every value the holes are
/// a non-decreasing sequence, and it is Elias–Fano: the low bits packed, the high parts as unary
/// gaps. The high parts are cut into lines of [`LINE`] values, each carrying its first value's
/// high part as a base, so that a value's high part is one select inside one line rather than a
/// walk from a sample; the low bits sit in a parallel array. A lookup is those two reads, which
/// depend on nothing but the value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Remap {
    /// Values of the levels below the first and of the tail, together: the sequence's length.
    len: u64,
    /// Low bits a value: from what the density alone asks, up to what leaves every line's unary
    /// part inside the line.
    low_bits: u32,
    low: Pages<u64>,
    lines: Pages<Line>,
}

impl Remap {
    fn low_words(len: u64, low_bits: u32) -> usize {
        (len as usize * low_bits as usize).div_ceil(64)
    }

    fn line_count(len: u64) -> usize {
        (len as usize).div_ceil(LINE)
    }

    /// Low bits for `len` values below `u` from the density alone: what leaves the high part
    /// about as dense as it is sparse.
    fn natural_low_bits(len: u64, u: u64) -> u32 {
        u.checked_div(len).map_or(0, |q| q.max(1).ilog2())
    }

    /// Whether a line of `values` fits at `low_bits`: as a dense line, or as a sparse one; `None`
    /// when as neither. A line holds a one a value and a zero a step of the high parts, and the
    /// steps halve with each low bit.
    fn line_fits(line: &[u64], low_bits: u32) -> Option<bool> {
        let span = |bits: u32| {
            let (first, last) = (line[0] >> bits, line[line.len() - 1] >> bits);
            (last < SPARSE).then_some((last - first) as usize + line.len())
        };
        if span(low_bits).is_some_and(|s| s <= LINE_BITS) {
            Some(false)
        } else if span(low_bits + 1).is_some_and(|s| s <= SPARSE_LINE_BITS) {
            Some(true)
        } else {
            None
        }
    }

    /// The sequence `values`, non-decreasing and below `u`, at the fewest low bits from the
    /// natural ones up at which every line fits, dense or sparse.
    fn encode(values: &[u64], u: u64) -> Self {
        let len = values.len() as u64;
        let mut low_bits = Self::natural_low_bits(len, u);
        while !values
            .chunks(LINE)
            .all(|line| Self::line_fits(line, low_bits).is_some())
        {
            low_bits += 1;
        }
        let mut remap = Self {
            len,
            low_bits,
            low: Pages::zeroed(Self::low_words(len, low_bits)),
            lines: Pages::zeroed(Self::line_count(len)),
        };
        if low_bits > 0 {
            for (j, &v) in values.iter().enumerate() {
                debug_assert!(v < u && (j == 0 || values[j - 1] <= v));
                let low = v & ((1 << low_bits) - 1);
                let at = j * low_bits as usize;
                let (w, o) = (at / 64, at % 64);
                remap.low[w] |= low << o;
                if o + low_bits as usize > 64 {
                    remap.low[w + 1] |= low >> (64 - o);
                }
            }
        }
        for (k, line) in values.chunks(LINE).enumerate() {
            let sparse = Self::line_fits(line, low_bits).expect("low bits at which it fits");
            let bits = low_bits + u32::from(sparse);
            let base = line[0] >> bits;
            let mut words = [0u64; 8];
            words[0] = base | if sparse { SPARSE } else { 0 };
            for (r, &v) in line.iter().enumerate() {
                let p = 32 + r + ((v >> bits) - base) as usize;
                words[p / 64] |= 1 << (p % 64);
                if sparse {
                    words[6 + r / 64] |= (v >> low_bits & 1) << (r % 64);
                }
            }
            remap.lines[k] = Line(words);
        }
        remap
    }

    /// The `j`-th value, for `j < len`. On a validated table this is exact; on anything else it
    /// is some number, which is all the caller needs.
    ///
    /// The high part is the `r`-th one of the line's unary part above the base, `r` the value's
    /// index in its line: which word holds it is read off the words' counts, all eight compared
    /// against `r` at once, and the bit is found inside its word without a loop. A sparse line's
    /// last two words are its values' extra low bits rather than unary, and its high parts are
    /// a bit shorter.
    #[inline(always)]
    fn get(&self, j: u64) -> u64 {
        let Some(line) = self.lines.get(j as usize / LINE) else {
            return 0;
        };
        let r = j % LINE as u64;
        let sparse = line.0[0] >> 31 & 1;
        let base = line.0[0] & (SPARSE - 1);
        let unary = sparse.wrapping_sub(1);
        let words = [
            line.0[0] & !0xFFFF_FFFF,
            line.0[1],
            line.0[2],
            line.0[3],
            line.0[4],
            line.0[5],
            line.0[6] & unary,
            line.0[7] & unary,
        ];
        let mut before = [0u64; 8];
        for w in 1..8 {
            before[w] = before[w - 1] + u64::from(words[w - 1].count_ones());
        }
        let w = (1..8).filter(|&w| before[w] <= r).count();
        let p = w as u64 * 64 + select_in_word(words[w], r - before[w]);
        let extra = line.0[6 + (r / 64) as usize] >> (r % 64) & sparse;
        let high = ((base + (p - 32) - r) << sparse) | extra;
        let low = if self.low_bits == 0 {
            0
        } else {
            let at = j * u64::from(self.low_bits);
            let (w, o) = ((at / 64) as usize, at % 64);
            let mut x = self.low[w] >> o;
            if o + u64::from(self.low_bits) > 64 {
                x |= self.low[w + 1] << (64 - o);
            }
            x & ((1 << self.low_bits) - 1)
        };
        (high << self.low_bits) | low
    }

    /// Pulls in what [`get`](Self::get) reads for `j`: its line and its low word.
    #[inline(always)]
    fn prefetch(&self, j: u64) {
        crate::blob::prefetch(&self.lines, j as usize / LINE);
        crate::blob::prefetch(&self.low, (j * u64::from(self.low_bits) / 64) as usize);
    }

    /// One pass over the lines: as many lines as the length needs, each holding exactly as many
    /// ones as values, and every value below `u`. What keeps [`get`](Self::get) inside the image
    /// for every `j` below the length.
    fn validate(&self, u: u64) -> bool {
        if self.lines.len() != Self::line_count(self.len)
            || self.low.len() != Self::low_words(self.len, self.low_bits)
            || self.low_bits >= 64
        {
            return false;
        }
        for (k, line) in self.lines.iter().enumerate() {
            let unary = if line.0[0] & SPARSE == 0 { 8 } else { 6 };
            let ones = (line.0[0] >> 32).count_ones() as usize
                + line.0[1..unary]
                    .iter()
                    .map(|w| w.count_ones() as usize)
                    .sum::<usize>();
            if ones != (self.len as usize - k * LINE).min(LINE) {
                return false;
            }
        }
        (0..self.len).all(|j| self.get(j) < u)
    }

    /// The remap of an `MPH2` blob, read into this layout: its occupancy bits over the values and
    /// its Elias–Fano hole list, whose `j`-th hole the `j`-th set value takes. Refused unless the
    /// set bits are as many as the holes and every hole is below `u`; the rank and select
    /// samples it also carried are derivable and go unread.
    fn from_v2(
        set: &[u64],
        low: &[u64],
        high: &[u64],
        holes: u64,
        low_bits: u32,
        entries: usize,
        u: u64,
    ) -> Result<Self, IndexError> {
        const BAD: IndexError = IndexError::Format("mphf: a remap entry points outside the image");
        let mut hole_at = Vec::with_capacity(holes as usize);
        let mut j = 0u64;
        for (w, &word) in high.iter().enumerate() {
            let mut x = word;
            while x != 0 {
                let p = w as u64 * 64 + u64::from(x.trailing_zeros());
                x &= x - 1;
                if j >= holes || p < j {
                    return Err(BAD);
                }
                let lo = if low_bits == 0 {
                    0
                } else {
                    let at = j * u64::from(low_bits);
                    let (lw, o) = ((at / 64) as usize, at % 64);
                    let mut x = low[lw] >> o;
                    if o + u64::from(low_bits) > 64 {
                        x |= low[lw + 1] << (64 - o);
                    }
                    x & ((1 << low_bits) - 1)
                };
                let hole = ((p - j) << low_bits) | lo;
                if hole >= u {
                    return Err(BAD);
                }
                hole_at.push(hole);
                j += 1;
            }
        }
        if j != holes {
            return Err(BAD);
        }
        let mut holes = hole_at.into_iter();
        let mut hole = 0;
        let mut values = Vec::with_capacity(entries);
        for i in 0..entries {
            if set[i / 64] >> (i % 64) & 1 == 1 {
                hole = holes.next().ok_or(IndexError::Format(
                    "mphf: occupied values disagree with the hole count",
                ))?;
            }
            values.push(hole);
        }
        if holes.next().is_some() {
            return Err(IndexError::Format(
                "mphf: occupied values disagree with the hole count",
            ));
        }
        Ok(Self::encode(&values, u))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct V2 {
    /// How many keys were built in; the image is exactly `[0, n)`.
    n: u64,
    /// The seed geometry of every level, which the magic names.
    geometry: Geometry,
    /// The bumping level over every key, inline because it is on every lookup; `None` when `n` is
    /// at most [`TAIL_KEYS`].
    first: Option<Level>,
    /// The levels the bumped keys go to, in order.
    rest: Vec<Level>,
    tail: Tail,
    remap: Remap,
}

/// What a placement pass reads: one level's shape and its keys, grouped by bucket.
struct Run<'a> {
    level: &'a Level,
    keys: &'a [u64],
    /// CSR offsets into `keys`: `start[b]` is where the keys of the run's bucket `b` begin, and
    /// the entry after the last bucket's is where they end.
    start: &'a [u32],
}

impl Run<'_> {
    #[inline(always)]
    fn size(&self, b: u32) -> u32 {
        self.start[b as usize + 1] - self.start[b as usize]
    }

    #[inline(always)]
    fn keys_of(&self, b: u32) -> &[u64] {
        &self.keys[self.start[b as usize] as usize..self.start[b as usize + 1] as usize]
    }
}

/// The keys of a level after the first, grouped by bucket in bucket order — a counting sort of
/// their level hashes. The order inside a bucket does not reach its seed. Each thread scatters
/// a range of buckets, so that its writes stay together; the hash is computed again on each
/// pass rather than listed, since the list would be as large as the result.
fn group_by_bucket(hs: &[u64], lv: usize, buckets: u64, threads: usize) -> Vec<u64> {
    let mut at = vec![0u32; buckets as usize + 1];
    for &h in hs {
        at[scale(level_hash(h, lv), buckets) as usize + 1] += 1;
    }
    for b in 0..buckets as usize {
        at[b + 1] += at[b];
    }
    phase("count");
    let mut keys = vec![0u64; hs.len()];
    let per = (buckets as usize).div_ceil(threads.max(1)).max(1 << 12);
    std::thread::scope(|scope| {
        let mut keys = &mut keys[..];
        for b0 in (0..buckets as usize).step_by(per) {
            let b1 = (b0 + per).min(buckets as usize);
            let (k0, k1) = (at[b0] as usize, at[b1] as usize);
            let (out, rest) = std::mem::take(&mut keys).split_at_mut(k1 - k0);
            keys = rest;
            let slots = &at[b0..=b1];
            scope.spawn(move || {
                let mut next: Vec<u32> = slots.iter().map(|&s| s - k0 as u32).collect();
                for &h in hs {
                    let hi = level_hash(h, lv);
                    let b = scale(hi, buckets) as usize;
                    if (b0..b1).contains(&b) {
                        let slot = &mut next[b - b0];
                        out[*slot as usize] = hi;
                        *slot += 1;
                    }
                }
            });
        }
    });
    keys
}

/// Whether `hashes` are sorted: in parts on `threads` when there are enough of them.
fn is_sorted(hashes: &[u64], threads: usize) -> bool {
    let part = hashes.len().div_ceil(threads.max(1));
    if threads <= 1 || part < 1 << 20 {
        return hashes.is_sorted();
    }
    std::thread::scope(|scope| {
        let parts: Vec<_> = hashes
            .chunks(part)
            .enumerate()
            .map(|(i, c)| {
                scope.spawn(move || (i == 0 || hashes[i * part - 1] <= c[0]) && c.is_sorted())
            })
            .collect();
        parts
            .into_iter()
            .all(|p| p.join().expect("a sort check panicked"))
    })
}

/// Where a level's keys come from.
enum Source<'a> {
    /// Grouped by bucket in bucket order, all in memory.
    Slice(&'a [u64]),
    /// Sorted, pulled as the level's chunks are claimed, so that a first level over more keys
    /// than fit in memory is built from a file. Only a first level is fed this way; the bumped
    /// keys are a few per cent and stay in memory.
    Stream(&'a mut (dyn Iterator<Item = u64> + Send)),
}

/// A level's chunks, claimed in order together with their keys: a subslice of the grouped keys,
/// or what a sequential reader cut at the chunk's last bucket. Claiming and reading are one
/// step under one lock, so the stream is only ever read in chunk order, whichever thread asks.
enum Feed<'a> {
    Slice {
        keys: &'a [u64],
        /// Where each chunk's keys begin, and where the last one's end.
        bounds: Vec<usize>,
        next: AtomicUsize,
    },
    Stream(std::sync::Mutex<Cutter<'a>>),
}

struct Cutter<'a> {
    hashes: &'a mut (dyn Iterator<Item = u64> + Send),
    /// The key that ended the previous chunk: the first of a later one.
    pending: Option<u64>,
    next: usize,
    fed: u64,
}

impl<'a> Feed<'a> {
    fn new(source: Source<'a>, starts: &[u64], buckets: u64) -> Self {
        match source {
            Source::Slice(keys) => Feed::Slice {
                keys,
                bounds: starts
                    .iter()
                    .map(|&first| keys.partition_point(|&h| scale(h, buckets) < first))
                    .chain(std::iter::once(keys.len()))
                    .collect(),
                next: AtomicUsize::new(0),
            },
            Source::Stream(hashes) => Feed::Stream(std::sync::Mutex::new(Cutter {
                hashes,
                pending: None,
                next: 0,
                fed: 0,
            })),
        }
    }

    /// The next chunk and its keys, grouped by bucket; `None` once every chunk is claimed. A
    /// stream's chunk is read into `buf`, the caller's, so that a thread reading chunk after
    /// chunk fills one buffer rather than growing a fresh one each time.
    fn next<'b>(
        &'b self,
        starts: &[u64],
        buckets: u64,
        buf: &'b mut Vec<u64>,
    ) -> Option<(usize, &'b [u64])> {
        match self {
            Feed::Slice { keys, bounds, next } => {
                let k = next.fetch_add(1, Ordering::Relaxed);
                (k + 1 < bounds.len()).then(|| (k, &keys[bounds[k]..bounds[k + 1]]))
            }
            Feed::Stream(cutter) => {
                let mut c = cutter.lock().expect("a chunk reader panicked");
                let k = c.next;
                if k >= starts.len() {
                    return None;
                }
                // The chunk ends where the next one's first bucket begins; the last runs to the
                // end of the stream. The key that ends it is kept for the chunk it belongs to,
                // which is not necessarily the next: a chunk can be empty.
                let limit = starts.get(k + 1).copied();
                buf.clear();
                let mut carried = c.pending.take();
                while let Some(h) = carried.take().or_else(|| c.hashes.next()) {
                    if limit.is_some_and(|l| scale(h, buckets) >= l) {
                        c.pending = Some(h);
                        break;
                    }
                    buf.push(h);
                }
                c.next += 1;
                c.fed += buf.len() as u64;
                Some((k, &buf[..]))
            }
        }
    }

    /// Keys handed out in all.
    fn fed(&self) -> u64 {
        match self {
            Feed::Slice { keys, .. } => keys.len() as u64,
            Feed::Stream(cutter) => cutter.lock().expect("a chunk reader panicked").fed,
        }
    }
}

/// What a placed chunk keeps for the gaps beside it, until both are placed.
struct Piece {
    /// Occupancy of the values it placed, from value `origin`.
    map: Map,
    origin: u64,
    /// CSR offsets of the gap's buckets after it, [`Run::start`]-style, into `tail_keys`.
    tail: Vec<u32>,
    /// The keys of the gap's buckets, copied out so that the chunk's own keys can go once it is
    /// placed — a first level fed from a file holds only the chunks in flight.
    tail_keys: Vec<u64>,
}

/// The keys of the buckets `from..to` of `run` that `seeds` bumped, appended to `out`.
fn bumped_keys(run: &Run<'_>, seeds: &[u8], from: u32, to: u32, out: &mut Vec<u64>) {
    for b in from..to {
        if seeds[b as usize] == 0 {
            out.extend_from_slice(run.keys_of(b));
        }
    }
}

/// Bucket boundaries of the keys `keys[from..]`, whose buckets are `first` on, into `ends`, one
/// per bucket, CSR-style: `ends[i]` is the key index past the last key of bucket `first + i`.
/// The keys are grouped by bucket in bucket order, so each key stores the index past itself, the
/// last one wins, and a bucket with no key takes its predecessor's end.
fn bucket_ends(keys: &[u64], from: usize, buckets: u64, first: u64, ends: &mut [u32]) {
    // Eight buckets computed before any is stored: a store whose address is still unknown
    // holds back the loads after it, and that is 5x here.
    let mut i = from;
    let mut eights = keys.chunks_exact(8);
    for eight in &mut eights {
        let mut bs = [0usize; 8];
        for (b, &h) in bs.iter_mut().zip(eight) {
            *b = (scale(h, buckets) - first) as usize;
        }
        for (j, &b) in bs.iter().enumerate() {
            ends[b] = (i + j + 1) as u32;
        }
        i += 8;
    }
    for (j, &h) in eights.remainder().iter().enumerate() {
        ends[(scale(h, buckets) - first) as usize] = (i + j + 1) as u32;
    }
    let mut last = from as u32;
    for e in ends.iter_mut() {
        last = last.max(*e);
        *e = last;
    }
}

impl V2 {
    fn levels(&self) -> impl Iterator<Item = &Level> {
        self.first.iter().chain(&self.rest)
    }

    fn level_count(&self) -> usize {
        usize::from(self.first.is_some()) + self.rest.len()
    }

    #[inline(always)]
    fn index(&self, h: u64) -> u64 {
        if let Some(l) = &self.first {
            let seed = l.seeds[scale(h, l.buckets) as usize];
            if seed != 0 {
                return l.value(h, seed);
            }
        }
        self.index_bumped(h)
    }

    /// A key the first level bumped: try each further level, then the tail, and map what places it
    /// to a hole. A hash that was never built in can reach a bucket nothing was ever bumped from,
    /// in which case the tail is empty and 0 is as good an answer as any.
    #[cold]
    #[inline(never)]
    fn index_bumped(&self, h: u64) -> u64 {
        self.bumped_value(h).map_or(0, |v| self.remap.get(v))
    }

    /// The remap entry a bumped key lands on: its value on the first further level that places it,
    /// after every earlier level's values, or its place in the tail; `None` when the tail is empty.
    #[inline(always)]
    fn bumped_value(&self, h: u64) -> Option<u64> {
        let mut shift = 0u64;
        for (i, l) in self.rest.iter().enumerate() {
            let hi = level_hash(h, i + 1);
            let seed = l.seeds[scale(hi, l.buckets) as usize];
            if seed != 0 {
                return Some(shift + l.value(hi, seed));
            }
            shift += l.n;
        }
        let t = &self.tail;
        if t.buckets == 0 {
            return None;
        }
        let ht = mix(h ^ t.seed);
        let seed = t.seeds[scale(ht, t.buckets) as usize];
        Some(shift + Tail::position(ht, seed, t.range))
    }

    /// [`Mphf::index_all`] on this table.
    fn index_all(&self, hashes: &[u64]) -> Vec<u64> {
        self.index_all_from(hashes, PREFETCH_SEEDS)
    }

    /// [`index_all`](Self::index_all) with the first-level size it prefetches from as a parameter.
    /// Below it the batch is the single lookup in a loop. From it, a key's seed is pulled in
    /// [`SEED_AHEAD`] keys before its turn, and the keys the first level bumps are noted and
    /// answered once their block has been through it.
    fn index_all_from(&self, hashes: &[u64], prefetch_seeds: usize) -> Vec<u64> {
        let Some(l) = self
            .first
            .as_ref()
            .filter(|l| l.seeds.len() >= prefetch_seeds)
        else {
            return hashes.iter().map(|&h| self.index(h)).collect();
        };
        if l.form() == Level::SHIPPED {
            self.batch::<true>(l, hashes)
        } else {
            self.batch::<false>(l, hashes)
        }
    }

    /// [`index_all_from`](Self::index_all_from) past its check, over the first level `l`:
    /// under [`Level::SHIPPED`] when `SHIPPED`, which the loop then reads as immediates.
    #[inline(always)]
    fn batch<const SHIPPED: bool>(&self, l: &Level, hashes: &[u64]) -> Vec<u64> {
        let mut out = vec![0u64; hashes.len()];
        // A bumped key's offset in its block, and how far its answer has got.
        let mut bumped: Vec<(usize, u64)> = Vec::new();
        // The buckets of the next `SEED_AHEAD` keys, found when their seeds were pulled in, so
        // that the read does not find them again: a multiply a key fewer on the loop.
        let mut ring = [0u64; SEED_AHEAD];
        for (i, &h) in hashes.iter().take(SEED_AHEAD).enumerate() {
            ring[i] = scale(h, l.buckets);
            crate::blob::prefetch_byte(&l.seeds, ring[i] as usize);
        }
        let blocks = hashes.chunks(BATCH_BLOCK).zip(out.chunks_mut(BATCH_BLOCK));
        for (b, (block, answers)) in blocks.enumerate() {
            let base = b * BATCH_BLOCK;
            bumped.clear();
            // The keys `SEED_AHEAD` on from the block's, whose seeds it pulls in; the last keys
            // of the batch have none.
            let ahead = hashes
                .len()
                .saturating_sub(base + SEED_AHEAD)
                .min(block.len());
            let next = &hashes[(base + SEED_AHEAD).min(hashes.len())..][..ahead];
            Self::answer_block::<SHIPPED>(l, block, next, base, &mut ring, answers, &mut bumped);
            if !bumped.is_empty() {
                self.resolve_bumped(block, &mut bumped, answers);
            }
        }
        out
    }

    /// One block of [`batch`](Self::batch): the first level's answer for each key of `block`
    /// into `answers`, the seeds of `next` — the keys [`SEED_AHEAD`] on — pulled in on the way,
    /// and the keys the level bumped listed in `bumped` by their offset. Its own function so
    /// that the loop's registers are its own.
    #[inline(never)]
    fn answer_block<const SHIPPED: bool>(
        l: &Level,
        block: &[u64],
        next: &[u64],
        base: usize,
        ring: &mut [u64; SEED_AHEAD],
        answers: &mut [u64],
        bumped: &mut Vec<(usize, u64)>,
    ) {
        let form = if SHIPPED { Level::SHIPPED } else { l.form() };
        let seeds: &[u8] = &l.seeds;
        let ahead = next.len();
        let (pulled, rest) = block.split_at(ahead);
        let (pulled_out, rest_out) = answers.split_at_mut(ahead);
        let keys = pulled.iter().zip(next).zip(pulled_out.iter_mut());
        for (k, ((&h, &coming), answer)) in keys.enumerate() {
            let slot = (base + k) % SEED_AHEAD;
            let at = ring[slot] as usize;
            let bucket = scale(coming, l.buckets);
            ring[slot] = bucket;
            crate::blob::prefetch_byte(seeds, bucket as usize);
            let seed = seeds[at];
            if seed == 0 {
                bumped.push((k, 0));
            }
            *answer = l.value_in(form, h, seed);
        }
        for (k, (&h, answer)) in rest.iter().zip(rest_out.iter_mut()).enumerate() {
            let k = ahead + k;
            let seed = seeds[ring[(base + k) % SEED_AHEAD] as usize];
            if seed == 0 {
                bumped.push((k, 0));
            }
            *answer = l.value_in(form, h, seed);
        }
    }

    /// The keys of `block` the first level bumped, each held in `bumped` by its offset and how far
    /// its answer has got — `u64::MAX` once that answer is 0 — answered into `out`, the block's
    /// answers.
    ///
    /// Such a key makes two more loads, each known only once the one before it is read: its next
    /// level's seed, then the remap's line and low word for the value it lands on. Its answer is
    /// three stages, each pulling in what the next one reads, and the stages run side by side,
    /// each [`STAGE_GAP`] keys behind the one before it: a load is issued that many steps before
    /// it is read however many keys the block bumped, where a pass over all of them per stage
    /// leaves the later stages' loads almost no lead.
    #[inline(always)]
    fn resolve_bumped(&self, block: &[u64], bumped: &mut [(usize, u64)], out: &mut [u64]) {
        let behind = |i: usize, stages: usize| i.checked_sub(stages * STAGE_GAP);
        for i in 0..bumped.len() + 2 * STAGE_GAP {
            if let Some(&(k, _)) = bumped.get(i) {
                if let Some(next) = self.rest.first() {
                    let at = scale(level_hash(block[k], 1), next.buckets) as usize;
                    crate::blob::prefetch_byte(&next.seeds, at);
                }
            }
            if let Some((k, at)) = behind(i, 1).and_then(|j| bumped.get_mut(j)) {
                *at = self.bumped_value(block[*k]).map_or(u64::MAX, |v| {
                    self.remap.prefetch(v);
                    v
                });
            }
            if let Some(&(k, at)) = behind(i, 2).and_then(|j| bumped.get(j)) {
                out[k] = if at == u64::MAX {
                    0
                } else {
                    self.remap.get(at)
                };
            }
        }
    }

    fn build(hashes: &[u64], threads: usize) -> Result<Self, IndexError> {
        phase("start");
        // Both callers hand over sorted hashes, and a bucket is monotone in its hash, so sorted
        // input is already grouped by bucket; anything else is sorted first.
        let sorted: std::borrow::Cow<[u64]> = if is_sorted(hashes, threads) {
            std::borrow::Cow::Borrowed(hashes)
        } else {
            let mut v = hashes.to_vec();
            v.sort_unstable();
            std::borrow::Cow::Owned(v)
        };
        phase("sort");
        Self::build_from(sorted.len() as u64, Source::Slice(&sorted), threads)
    }

    /// [`build`](Self::build) over `n` sorted, distinct hashes pulled from `hashes` one chunk of
    /// the first level at a time, so that a set too large to hold is built from a file. The
    /// table is the one `build` gives the same keys, by construction: both feed the same
    /// placement. A stream that yields other than `n` keys is refused.
    fn build_streaming(
        n: u64,
        hashes: &mut (dyn Iterator<Item = u64> + Send),
        threads: usize,
    ) -> Result<Self, IndexError> {
        phase("start");
        Self::build_from(n, Source::Stream(hashes), threads)
    }

    fn build_from(n: u64, source: Source<'_>, threads: usize) -> Result<Self, IndexError> {
        const SHORT: IndexError = IndexError::Build(
            "minimal perfect hash: the source yielded other than the promised number of keys",
        );
        let mut levels: Vec<Level> = Vec::new();
        let mut maps: Vec<Map> = Vec::new();
        let mut remaining: Vec<u64>;

        if n > TAIL_KEYS {
            let (level, taken, bumped, fed) = Self::build_level(n, source, threads, false);
            if fed != n {
                return Err(SHORT);
            }
            remaining = bumped;
            levels.push(level);
            maps.push(taken);
        } else {
            remaining = match source {
                Source::Slice(keys) => keys.to_vec(),
                Source::Stream(hashes) => hashes.collect(),
            };
            if remaining.len() as u64 != n {
                return Err(SHORT);
            }
        }

        while remaining.len() as u64 > TAIL_KEYS && levels.len() < MAX_LEVELS {
            let lv = levels.len();
            let buckets = Level::shape(remaining.len() as u64, Geometry::Mph3).buckets;
            let keys = group_by_bucket(&remaining, lv, buckets, threads);
            phase("level pairs");
            let (level, taken, _, _) =
                Self::build_level(keys.len() as u64, Source::Slice(&keys), threads, true);
            drop(keys);
            // The keys it bumped, in the order they came: a bucket's order does not reach its
            // seed. A level that bumps everything has spread nothing; the tail takes the keys
            // as they are.
            let fed = remaining.len();
            remaining
                .retain(|&h| level.seeds[scale(level_hash(h, lv), level.buckets) as usize] == 0);
            if remaining.len() == fed {
                break;
            }
            phase("level remaining");
            levels.push(level);
            maps.push(taken);
        }

        let (tail, tail_map) = Self::build_tail(&remaining).ok_or(IndexError::Build(
            "minimal perfect hash: no seed placed every bucket",
        ))?;
        phase("tail");

        // Minimal at last: the values the levels below the first hand out, in order, take the
        // holes the first level left, in order — there are exactly as many of each — and a value
        // no key landed on takes its predecessor's, or the first.
        let entries =
            levels.iter().skip(1).map(|l| l.n as usize).sum::<usize>() + tail.range as usize;
        let mut holes: Box<dyn Iterator<Item = u64>> = match maps.first() {
            Some(first) => Box::new(first.holes(n)),
            None => Box::new(0..n),
        };
        let mut holes = holes.by_ref().peekable();
        let mut hole = holes.peek().copied().unwrap_or(0);
        let ranges = maps
            .iter()
            .zip(&levels)
            .skip(1)
            .map(|(m, l)| (m, l.n))
            .chain(std::iter::once((&tail_map, tail.range)));
        let mut values = Vec::with_capacity(entries);
        for (map, range) in ranges {
            for v in 0..range {
                if map.get(v) {
                    hole = holes.next().expect("a hole per value a key landed on");
                }
                values.push(hole);
            }
        }
        debug_assert!(holes.next().is_none());
        phase("remap values");
        let remap = Remap::encode(&values, n);
        phase("remap lines");

        let mut levels = levels.into_iter();
        Ok(Self {
            n,
            geometry: Geometry::Mph3,
            first: levels.next(),
            rest: levels.collect(),
            tail,
            remap,
        })
    }

    /// One bumping level over `n` keys from `source`, grouped by bucket in bucket order. Returns
    /// the level, its occupancy map, the keys it bumped, and how many keys the source fed it.
    fn build_level(
        n: u64,
        source: Source<'_>,
        threads: usize,
        deep: bool,
    ) -> (Level, Map, Vec<u64>, u64) {
        let mut level = Level::shape(n, Geometry::Mph3);
        let buckets = level.buckets;
        let slice = level.slice;
        let starts = chunk_starts(buckets, deep);
        let chunks = starts.len();
        // Buckets whose slices reach past a bucket, from before it: consecutive buckets' slices
        // start within `n / buckets` of each other. So many before each chunk boundary, and
        // before the level's end, are its gap: placed once the runs on both sides are done,
        // against both. The run after a gap starts with the gap's share of its first slice held
        // back — more than the buckets before it would take in the middle of a level, since the
        // gap has nowhere further to spill and the run does.
        let reach = (((slice + 2) * buckets).div_ceil(n) + 1).min(buckets / 2);
        let first_of = |k: usize| starts[k];
        let end_of = |k: usize| starts.get(k + 1).copied().unwrap_or(buckets);
        let run_end = |k: usize| end_of(k) - reach;
        let longest = (0..chunks)
            .map(|k| end_of(k) - first_of(k))
            .max()
            .unwrap_or(0);
        // The smallest value a key of bucket `b` can take.
        let lo = |b: u64| ((b as u128 * n as u128) / buckets as u128) as u64;
        let feed = Feed::new(source, &starts, buckets);
        // What a stream's chunk buffer holds: the longest chunk's share of the keys, and the
        // spread of a Poisson count on top.
        let expected = match feed {
            Feed::Stream(_) => {
                let share = (longest as u128 * n as u128 / buckets.max(1) as u128) as usize;
                share + 4 * (share as f64).sqrt() as usize
            }
            Feed::Slice { .. } => 0,
        };
        let shift = level.shift;
        let align = |v: u64| v & !((64 << shift) - 1);
        let word = |o: u64| (o >> shift) as i64 / 64;

        // Chunks are claimed in order from the feed, with their keys. A chunk finds its buckets'
        // boundaries and seeds its run into a private occupancy map covering just the values its
        // buckets can reach — a few dozen KiB, which is what keeps the search in L1. A gap is
        // placed by whichever thread finished the later of its two chunks, against a map
        // prefilled from theirs, which is all that reaches it, over the gap keys the left chunk
        // copied out. Seeds go into the level's table as each piece is placed, and a chunk's
        // map into the level's once the gaps on both sides of it are — so that a level holds
        // the pieces in flight, not all of them — and the gap at the level's end, whose values
        // wrap, is placed last, against everything. The keys the chunks bumped are listed in
        // chunk order as the chunks finish — a chunk's once every chunk before it is in, by
        // whichever thread finds it so — and the gaps' after them: the order the level hands
        // on. The list is reserved here, at about the share a level bumps, so that a worker's
        // append does not move it into that worker's heap, where it would outlive its use.
        let placed = Mutex::new((Pages::zeroed(buckets as usize), Map::new(n, shift)));
        let pieces: Vec<Mutex<Option<Piece>>> = (0..chunks).map(|_| Mutex::new(None)).collect();
        let bumps: Vec<Mutex<Option<Vec<u64>>>> = (0..chunks).map(|_| Mutex::new(None)).collect();
        let bumped = Mutex::new((0usize, Vec::with_capacity(n as usize / 32)));
        let gap_bumps: Vec<OnceLock<Vec<u64>>> = (1..chunks).map(|_| OnceLock::new()).collect();
        let end_tail: OnceLock<(Vec<u32>, Vec<u64>)> = OnceLock::new();
        let done: Vec<AtomicBool> = (0..chunks).map(|_| AtomicBool::new(false)).collect();
        let claimed: Vec<AtomicBool> = (1..chunks).map(|_| AtomicBool::new(false)).collect();
        // Gaps yet to read each chunk's map: one on each side, none past the level's ends.
        let readers: Vec<AtomicUsize> = (0..chunks)
            .map(|k| AtomicUsize::new(usize::from(k > 0) + usize::from(k + 1 < chunks)))
            .collect();
        let level_ref = &level;
        let settle = |first: u64, seeds: &[u8]| {
            let mut placed = placed.lock().expect("a chunk placer panicked");
            placed.0[first as usize..][..seeds.len()].copy_from_slice(seeds);
        };
        let drain = || {
            let mut out = bumped.lock().expect("a chunk placer panicked");
            while out.0 < chunks {
                let Some(keys) = bumps[out.0].lock().expect("a chunk placer panicked").take()
                else {
                    break;
                };
                out.1.extend(keys);
                out.0 += 1;
            }
        };
        let release = |k: usize| {
            let piece = pieces[k]
                .lock()
                .expect("a chunk placer panicked")
                .take()
                .expect("a chunk is released once, after it is placed");
            let mut placed = placed.lock().expect("a chunk placer panicked");
            placed.1.merge(&piece.map, word(piece.origin));
            drop(placed);
            if k + 1 == chunks {
                assert!(
                    end_tail.set((piece.tail, piece.tail_keys)).is_ok(),
                    "one chunk ends the level"
                );
            }
        };
        std::thread::scope(|scope| {
            for _ in 0..threads.clamp(1, chunks) {
                scope.spawn(|| {
                    let mut start = vec![0u32; longest as usize + 1];
                    let mut buf = Vec::with_capacity(expected);
                    while let Some((k, chunk)) = feed.next(&starts, buckets, &mut buf) {
                        let (first, end, last) = (first_of(k), run_end(k), end_of(k));
                        let len = (last - first) as usize + 1;
                        start[..len].fill(0);
                        bucket_ends(chunk, 0, buckets, first, &mut start[1..len]);
                        let run = Run {
                            level: level_ref,
                            keys: chunk,
                            start: &start[..len],
                        };
                        let origin = align(lo(first));
                        let mut map = Map::new((lo(end) + slice).min(n) - origin, shift);
                        let marked = reserve(level_ref, first, origin, &mut map);
                        let mut seeds = vec![0u8; (end - first) as usize];
                        seed_run(&run, &mut seeds, &mut map, origin);
                        for v in marked {
                            map.clear(v);
                        }
                        let mut bumped = Vec::new();
                        bumped_keys(&run, &seeds, 0, (end - first) as u32, &mut bumped);
                        // The gap's buckets and their keys, offsets rebased onto the copy.
                        let tail_from = start[(end - first) as usize];
                        let tail_keys = chunk[tail_from as usize..start[len - 1] as usize].to_vec();
                        let tail = start[(end - first) as usize..len]
                            .iter()
                            .map(|&s| s - tail_from)
                            .collect();
                        settle(first, &seeds);
                        *bumps[k].lock().expect("a chunk placer panicked") = Some(bumped);
                        drain();
                        *pieces[k].lock().expect("a chunk placer panicked") = Some(Piece {
                            map,
                            origin,
                            tail,
                            tail_keys,
                        });
                        done[k].store(true, Ordering::SeqCst);
                        if readers[k].load(Ordering::SeqCst) == 0 {
                            release(k);
                        }
                        // The two flags are set before either is read, so of the two threads
                        // finishing a gap's chunks at least one sees both set.
                        for g in [k.wrapping_sub(1), k] {
                            if g >= claimed.len()
                                || !done[g].load(Ordering::SeqCst)
                                || !done[g + 1].load(Ordering::SeqCst)
                                || claimed[g].swap(true, Ordering::SeqCst)
                            {
                                continue;
                            }
                            let (first, end) = (run_end(g), end_of(g));
                            let origin = align(lo(first));
                            let mut map = Map::new(lo(end) + slice - origin, shift);
                            let mut seeds = vec![0u8; (end - first) as usize];
                            let mut bumped = Vec::new();
                            {
                                // Locked in index order, as every gap does, so two gaps sharing
                                // a chunk take turns rather than wait on each other.
                                let left = pieces[g].lock().expect("a chunk placer panicked");
                                let right = pieces[g + 1].lock().expect("a chunk placer panicked");
                                let left = left.as_ref().expect("done follows set");
                                let right = right.as_ref().expect("done follows set");
                                map.merge(&left.map, word(left.origin) - word(origin));
                                map.merge(&right.map, word(right.origin) - word(origin));
                                let run = Run {
                                    level: level_ref,
                                    keys: &left.tail_keys,
                                    start: &left.tail,
                                };
                                seed_run(&run, &mut seeds, &mut map, origin);
                                bumped_keys(&run, &seeds, 0, (end - first) as u32, &mut bumped);
                            }
                            settle(first, &seeds);
                            assert!(gap_bumps[g].set(bumped).is_ok(), "a gap is placed once");
                            placed
                                .lock()
                                .expect("a chunk placer panicked")
                                .1
                                .merge(&map, word(origin));
                            for c in [g, g + 1] {
                                if readers[c].fetch_sub(1, Ordering::SeqCst) == 1 {
                                    release(c);
                                }
                            }
                        }
                    }
                });
            }
        });
        phase("chunks");
        let fed = feed.fed();

        let (mut seeds, mut taken) = placed.into_inner().expect("a chunk placer panicked");
        let (tail, tail_keys) = end_tail
            .into_inner()
            .expect("the last chunk ends the level");
        let (drained, mut bumped) = bumped.into_inner().expect("a chunk placer panicked");
        debug_assert_eq!(drained, chunks, "every chunk's bumped keys were listed");
        for slot in gap_bumps {
            bumped.extend(slot.into_inner().expect("every gap was placed"));
        }
        phase("merge");
        let run = Run {
            level: &level,
            keys: &tail_keys,
            start: &tail,
        };
        let sd = &mut seeds[(buckets - reach) as usize..];
        seed_run(&run, sd, &mut taken, 0);
        bumped_keys(&run, sd, 0, reach as u32, &mut bumped);
        phase("gaps");
        level.seeds = seeds;
        (level, taken, bumped, fed)
    }

    /// The tail: every bucket placed, largest first, under the first of 65 536 seeds that lands its
    /// keys on distinct free positions. Fails only when some bucket has no such seed, which with
    /// distinct hashes is a probability the retries make negligible; with duplicates it is certain.
    fn build_tail(hs: &[u64]) -> Option<(Tail, Map)> {
        let keys = hs.len() as u64;
        if keys == 0 {
            return Some((Tail::EMPTY, Map::new(0, 0)));
        }
        let buckets = ceil_div_ratio(keys, ratio(4, TAIL_LAMBDA)).max(1);
        let range = ceil_div_ratio(keys, ratio(5, TAIL_ALPHA)).max(keys);
        let mut pairs: Vec<(u64, u64)> = Vec::with_capacity(hs.len());
        let mut order: Vec<(u32, u32)> = Vec::new();
        let mut pos: Vec<u64> = Vec::new();
        'attempt: for attempt in 0..TAIL_TRIES {
            let seed = mix(0x7A11_5EED_0000_0000 ^ u64::from(attempt));
            pairs.clear();
            pairs.extend(hs.iter().map(|&h| {
                let ht = mix(h ^ seed);
                (scale(ht, buckets), ht)
            }));
            pairs.sort_unstable();
            order.clear();
            let mut i = 0;
            while i < pairs.len() {
                let mut j = i + 1;
                while j < pairs.len() && pairs[j].0 == pairs[i].0 {
                    j += 1;
                }
                order.push((i as u32, j as u32));
                i = j;
            }
            order.sort_by_key(|&(s, e)| (Reverse(e - s), s));

            let mut taken = Map::new(range, 0);
            let mut seeds = vec![0u16; buckets as usize];
            for &(s, e) in &order {
                let ks = &pairs[s as usize..e as usize];
                let mut found = None;
                'seed: for seed in 0..=u16::MAX {
                    pos.clear();
                    for &(_, ht) in ks {
                        let p = Tail::position(ht, seed, range);
                        if taken.get(p) || pos.contains(&p) {
                            continue 'seed;
                        }
                        pos.push(p);
                    }
                    found = Some(seed);
                    break;
                }
                let Some(seed) = found else {
                    continue 'attempt;
                };
                for &p in &pos {
                    taken.set(p);
                }
                seeds[ks[0].0 as usize] = seed;
            }
            return Some((
                Tail {
                    keys,
                    buckets,
                    range,
                    seed,
                    seeds,
                },
                taken,
            ));
        }
        None
    }

    /// Section lengths, all derived from the header.
    /// Bytes of the seeds (the levels' and the tail's), of the remap's low bits, and of its lines.
    fn sections(&self) -> (usize, usize, usize) {
        let (m, l) = (self.remap.len, self.remap.low_bits);
        (
            self.levels().map(|l| l.seeds.len()).sum::<usize>() + self.tail.seeds.len() * 2,
            Remap::low_words(m, l) * 8,
            Remap::line_count(m) * 64,
        )
    }

    fn byte_len(&self) -> usize {
        let (a, b, c) = self.sections();
        header_len(self.level_count()) + a + b + c
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len());
        self.write_into(&mut out).expect("a Vec takes every write");
        debug_assert_eq!(out.len(), self.byte_len());
        out
    }

    /// The blob, section by section into `w`: what [`to_bytes`](Self::to_bytes) assembles,
    /// without the copy, for a table on its way to a file.
    fn write_into(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        let hl = header_len(self.level_count());
        let mut header = vec![0u8; hl];
        header[0..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&FORMAT.to_le_bytes());
        header[6] = self.geometry.mode_bits() as u8;
        // Reserved; written zero and required to be zero, so a later flag cannot be read as absent.
        header[7] = 0;
        let scalars = [
            self.n,
            self.level_count() as u64,
            self.tail.keys,
            self.tail.buckets,
            self.tail.range,
            self.tail.seed,
            u64::from(self.remap.low_bits),
        ];
        let rows = self.levels().flat_map(|l| [l.n, l.buckets, l.slice]);
        for (i, v) in scalars.into_iter().chain(rows).enumerate() {
            header[8 + i * 8..16 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        let check = crate::blob::hash_bytes(&header[..hl - 4]) as u32;
        header[hl - 4..].copy_from_slice(&check.to_le_bytes());
        w.write_all(&header)?;

        for l in self.levels() {
            w.write_all(&l.seeds)?;
        }
        for &s in &self.tail.seeds {
            w.write_all(&s.to_le_bytes())?;
        }
        for &word in &self.remap.low {
            w.write_all(&word.to_le_bytes())?;
        }
        for line in &self.remap.lines {
            for &word in &line.0 {
                w.write_all(&word.to_le_bytes())?;
            }
        }
        Ok(())
    }

    /// Every read `index` makes is bounded by a scalar in the header, and the checks are exactly
    /// that list: a bucket index below its level's seed count, a value below its level's range,
    /// a remap value below the entry count, a line holding a one for each of its values, and a
    /// hole below `n`. An `MPH2` blob is read into this layout, its remap converted.
    fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let v2 = match bytes.get(0..4) {
            Some(m) if m == MAGIC => false,
            Some(m) if m == MAGIC_V2 => true,
            _ => return Err(IndexError::Format("mphf: bad magic or truncated header")),
        };
        if bytes.len() < FIXED + 4 {
            return Err(IndexError::Format("mphf: truncated header"));
        }
        let at = |i: usize| u64::from_le_bytes(bytes[8 + i * 8..16 + i * 8].try_into().expect("8"));
        let (n, level_count) = (at(0), at(1));
        let level_count = usize::try_from(level_count)
            .ok()
            .filter(|&c| c <= MAX_LEVELS)
            .ok_or(IndexError::Format("mphf: too many levels"))?;
        let hl = header_len(level_count);
        if bytes.len() < hl {
            return Err(IndexError::Format("mphf: truncated header"));
        }
        let check = u32::from_le_bytes(bytes[hl - 4..hl].try_into().expect("4 bytes"));
        if check != crate::blob::hash_bytes(&bytes[..hl - 4]) as u32 {
            return Err(IndexError::Format("mphf: header checksum mismatch"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().expect("2 bytes"));
        if version != if v2 { FORMAT_V2 } else { FORMAT } {
            return Err(IndexError::Format("mphf: unsupported format version"));
        }
        let geometry = match (v2, bytes[6]) {
            (true, 0) | (false, 0) => Geometry::Mph2,
            (false, MODE_BITS_BYTE) => Geometry::Mph3,
            _ => {
                return Err(IndexError::Format("mphf: unsupported seed geometry"));
            }
        };
        if bytes[7] != 0 {
            return Err(IndexError::Format(
                "mphf: reserved header field is not zero",
            ));
        }
        let tail = Tail {
            keys: at(2),
            buckets: at(3),
            range: at(4),
            seed: at(5),
            seeds: Vec::new(),
        };
        // The remap's low bits, or an `MPH2` blob's hole count.
        let scalar6 = at(6);

        if n == 0 {
            if bytes.len() != hl
                || level_count != 0
                || (tail.keys | tail.buckets | tail.range | tail.seed | scalar6) != 0
            {
                return Err(IndexError::Format(
                    "mphf: empty table with a non-empty shape",
                ));
            }
            return Ok(Self {
                n: 0,
                geometry,
                first: None,
                rest: Vec::new(),
                tail,
                remap: Remap::default(),
            });
        }

        let mut levels = Vec::with_capacity(level_count);
        let mut entries = 0usize;
        for i in 0..level_count {
            let (ln, buckets, slice) = (at(7 + i * 3), at(8 + i * 3), at(9 + i * 3));
            // `value` wraps once, so it is below `ln` only while a slice fits inside the range.
            if ln == 0 || buckets == 0 || !slice.is_power_of_two() || slice > ln {
                return Err(IndexError::Format("mphf: a level's shape is inconsistent"));
            }
            if i == 0 && ln != n {
                return Err(IndexError::Format(
                    "mphf: the first level does not cover the table",
                ));
            }
            if i > 0 {
                entries = entries
                    .checked_add(usize::try_from(ln).map_err(|_| SIZE)?)
                    .ok_or(SIZE)?;
            }
            levels.push(Level {
                n: ln,
                buckets,
                slice,
                shift: stride_for(slice, geometry.mode_bits()).trailing_zeros(),
                mode_bits: geometry.mode_bits(),
                seeds: Pages::default(),
            });
        }
        // An empty tail answers 0, which is inside the image; a non-empty one needs both tables.
        if (tail.buckets == 0) != (tail.range == 0) || (tail.buckets == 0) != (tail.keys == 0) {
            return Err(IndexError::Format("mphf: the tail's shape is inconsistent"));
        }
        entries = entries
            .checked_add(usize::try_from(tail.range).map_err(|_| SIZE)?)
            .ok_or(SIZE)?;
        // Narrowed rather than cast: on a 32-bit target `as usize` would truncate a fabricated
        // count into a plausible section length.
        let mut want = hl;
        for l in &levels {
            want = want
                .checked_add(usize::try_from(l.buckets).map_err(|_| SIZE)?)
                .ok_or(SIZE)?;
        }
        let tail_buckets = usize::try_from(tail.buckets).map_err(|_| SIZE)?;
        want = want
            .checked_add(tail_buckets.checked_mul(2).ok_or(SIZE)?)
            .ok_or(SIZE)?;
        // The remap's sections: this layout's low words and lines, or an `MPH2` blob's occupancy
        // words, rank samples, and its hole list's low words, high words and select samples.
        let mut v2_sizes = (0usize, 0usize, 0usize, 0usize, 0usize);
        let (low_bits, holes) = if v2 {
            let holes = scalar6;
            if (levels.is_empty() && holes != 0) || holes > n {
                return Err(IndexError::Format("mphf: hole count out of range"));
            }
            let low_bits = Remap::natural_low_bits(holes, n);
            let holes_len = usize::try_from(holes).map_err(|_| SIZE)?;
            let set_words = entries.div_ceil(64);
            let rank_samples = entries.div_ceil(512);
            let low_words = holes_len
                .checked_mul(low_bits as usize)
                .ok_or(SIZE)?
                .div_ceil(64);
            let high_words = if holes == 0 {
                0
            } else {
                holes_len
                    .checked_add(usize::try_from(n >> low_bits).map_err(|_| SIZE)?)
                    .and_then(|v| v.checked_add(1))
                    .ok_or(SIZE)?
                    .div_ceil(64)
            };
            let samples = holes_len.div_ceil(128);
            v2_sizes = (set_words, rank_samples, low_words, high_words, samples);
            want = want
                .checked_add(set_words.checked_mul(8).ok_or(SIZE)?)
                .and_then(|v| v.checked_add(rank_samples.checked_mul(4)?))
                .and_then(|v| v.checked_add(low_words.checked_mul(8)?))
                .and_then(|v| v.checked_add(high_words.checked_mul(8)?))
                .and_then(|v| v.checked_add(samples.checked_mul(4)?))
                .ok_or(SIZE)?;
            (low_bits, holes)
        } else {
            let low_bits = u32::try_from(scalar6)
                .ok()
                .filter(|&b| b < 64)
                .ok_or(IndexError::Format("mphf: remap low bits out of range"))?;
            let low_words = entries
                .checked_mul(low_bits as usize)
                .ok_or(SIZE)?
                .div_ceil(64);
            want = want
                .checked_add(low_words.checked_mul(8).ok_or(SIZE)?)
                .and_then(|v| v.checked_add(entries.div_ceil(LINE).checked_mul(64)?))
                .ok_or(SIZE)?;
            (low_bits, 0)
        };
        if bytes.len() != want {
            return Err(IndexError::Format(
                "mphf: blob length disagrees with the header",
            ));
        }

        let mut p = hl;
        for l in &mut levels {
            let len = l.buckets as usize;
            l.seeds = Pages::from_slice(&bytes[p..p + len]);
            p += len;
        }
        fn take<const W: usize, T>(
            bytes: &[u8],
            p: &mut usize,
            count: usize,
            f: fn([u8; W]) -> T,
        ) -> Vec<T> {
            let v = bytes[*p..*p + count * W]
                .chunks_exact(W)
                .map(|w| f(w.try_into().expect("a whole word")))
                .collect();
            *p += count * W;
            v
        }
        let tail = Tail {
            seeds: take(bytes, &mut p, tail_buckets, u16::from_le_bytes),
            ..tail
        };
        let remap = if v2 {
            let (set_words, rank_samples, low_words, high_words, samples) = v2_sizes;
            let set = take(bytes, &mut p, set_words, u64::from_le_bytes);
            p += rank_samples * 4;
            let low = take(bytes, &mut p, low_words, u64::from_le_bytes);
            let high = take(bytes, &mut p, high_words, u64::from_le_bytes);
            p += samples * 4;
            Remap::from_v2(&set, &low, &high, holes, low_bits, entries, n)?
        } else {
            let low = take(
                bytes,
                &mut p,
                Remap::low_words(entries as u64, low_bits),
                u64::from_le_bytes,
            );
            let lines = take(bytes, &mut p, entries.div_ceil(LINE), |c: [u8; 64]| {
                let mut words = [0u64; 8];
                for (w, x) in words.iter_mut().zip(c.chunks_exact(8)) {
                    *w = u64::from_le_bytes(x.try_into().expect("a whole word"));
                }
                Line(words)
            });
            Remap {
                len: entries as u64,
                low_bits,
                low: Pages::from_slice(&low),
                lines: Pages::from_slice(&lines),
            }
        };
        debug_assert_eq!(p, bytes.len());

        // The check that costs more than a comparison, and the one that makes the image a promise
        // rather than a hope: every line must hold a one for each of its values, so that a select
        // stays inside it, and every hole must lie below `n`. Callers index their own arrays by
        // what `index` returns, so an id outside `[0, n)` is their unsoundness, not ours.
        if !remap.validate(n) {
            return Err(IndexError::Format(
                "mphf: a remap entry points outside the image",
            ));
        }

        let mut levels = levels.into_iter();
        Ok(Self {
            n,
            geometry,
            first: levels.next(),
            rest: levels.collect(),
            tail,
            remap,
        })
    }
}

/// Seed the buckets of a run against `taken`, whose bit 0 is value `origin`. `seeds` is those
/// buckets' slice of the level's table, one per bucket of the run.
///
/// Buckets are taken from a window of [`WINDOW`] starting at the lowest one still unplaced, by
/// priority: larger buckets slightly ahead of their index, single keys held back by about the
/// slice — a single key fits any hole and is the one to leave for last. Within the window the
/// occupancy map's live edge is a few hundred bytes, so the search runs out of L1.
fn seed_run(run: &Run<'_>, seeds: &mut [u8], taken: &mut Map, origin: u64) {
    let end = seeds.len() as u32;
    debug_assert!(run.start.len() > end as usize);
    let level = run.level;
    let window = tun(6, f64::from(WINDOW)) as u32;
    // Buckets wait in a queue per size class, each in index order, which is priority order
    // within the class: the priority is the class's term less 1024 per bucket of index. The next
    // bucket is the best of the class heads, kept in a heap of at most one entry per class.
    let mut term = [0i64; CLASSES];
    for (c, t) in term.iter_mut().enumerate() {
        *t = ell(c + 1, level.slice);
    }
    let class = |b: u32| (run.size(b) as usize).min(CLASSES) - 1;
    let priority = |b: u32| -> i64 { term[class(b)] - 1024 * i64::from(b) };
    let mut queue: [std::collections::VecDeque<u32>; CLASSES] = Default::default();
    let mut heads: BinaryHeap<(i64, Reverse<u32>)> = BinaryHeap::with_capacity(CLASSES);
    let mut done = vec![0u64; (end as usize).div_ceil(64)];
    let mut scratch = Scratch::new();
    let (mut front, mut pushed) = (0u32, 0u32);
    loop {
        while front < end
            && (run.size(front) == 0 || done[(front / 64) as usize] >> (front % 64) & 1 == 1)
        {
            front += 1;
        }
        if front == end {
            break;
        }
        let limit = end.min(front.saturating_add(window));
        while pushed < limit {
            if run.size(pushed) != 0 {
                let q = &mut queue[class(pushed)];
                if q.is_empty() {
                    heads.push((priority(pushed), Reverse(pushed)));
                }
                q.push_back(pushed);
            }
            pushed += 1;
        }
        let (_, Reverse(b)) = heads.pop().expect("the front bucket is in a queue");
        let q = &mut queue[class(b)];
        let head = q.pop_front();
        debug_assert_eq!(head, Some(b));
        if let Some(&next) = q.front() {
            heads.push((priority(next), Reverse(next)));
        }
        let ks = run.keys_of(b);
        seeds[b as usize] = seed_bucket(level, ks, taken, origin, &mut scratch);
        done[(b / 64) as usize] |= 1 << (b % 64);
    }
}

/// The seed that lands every key of the bucket on a distinct free value, marking those values
/// taken; 0 if there is none.
///
/// A seed is a mode and a shift. The mode picks each key's offset in its slice; shift `t` puts a
/// key `t` strides past that offset, wrapping inside the slice. On the map a key's values under
/// consecutive shifts are consecutive bits of one plane, so with at most 64 shifts a mode its
/// occupancy under every shift is one word — its run up to its wrap, and from `period` bits back
/// after it — and the shifts that place the bucket are the zero bits of the OR of its keys'
/// words. Two keys with the same base in a mode collide under every shift of that mode and are
/// tried in the others; the rest are caught when a candidate's values are listed.
///
/// The seed to take is the one whose values are lowest, because low values are what the buckets
/// still to come cannot use anyway. Within a mode values grow with the shift until a key wraps
/// and drops by a slice, so the sum is lowest at the first feasible shift at or after some wrap,
/// and those are the candidates: one `trailing_zeros` each, the sum from the wraps at or before
/// it. Levels with more shifts a mode go through [`seed_bucket_wide`].
fn seed_bucket(
    level: &Level,
    ks: &[u64],
    taken: &mut Map,
    origin: u64,
    scratch: &mut Scratch,
) -> u8 {
    let shift_bits = 8 - level.mode_bits;
    if shift_bits > 6 {
        return seed_bucket_wide(level, ks, taken, origin, scratch);
    }
    let k = ks.len();
    if k > MAX_BUCKET {
        return 0;
    }
    let Scratch {
        starts,
        offs,
        cut,
        plane,
        bit,
        vals,
        ..
    } = scratch;
    count!(BUCKETS, 1);
    let (slice, mask) = (level.slice, level.slice - 1);
    let delta = 1u64 << level.shift;
    let (shift, dm) = (delta.trailing_zeros(), delta - 1);
    let period = slice >> shift;
    let ks_delta = k as u64 * delta;
    // Values past the range's end wrap to its beginning; `limit` is where that is on this map,
    // which a chunk's private map never reaches. A key whose slice crosses it is read bit by bit.
    let limit = level.n - origin;
    let fold = |v: u64| if v >= limit { v - limit } else { v };
    let mut slow = 0u64;
    for i in 0..k {
        starts[i] = scale(ks[i], level.n) - origin;
        slow |= u64::from(starts[i] + slice > limit) << i;
    }
    let end = 1u64 << shift_bits;
    let mut best: Option<(u64, u32, u64)> = None;
    for mode in 0..1u32 << level.mode_bits {
        // The keys' offsets in this mode, the shifts at which they wrap, and where their runs
        // start on the map.
        let mut base = 0u64;
        let mut wraps = 0u64;
        for i in 0..k {
            let o = level.offset(ks[i], mode);
            offs[i] = o;
            base += o;
            let c = (slice - o + dm) >> shift;
            cut[i] = c;
            if c < end {
                wraps |= 1 << c;
            }
            vals[i] = fold(starts[i] + o);
            let (p, b) = taken.at(vals[i]);
            plane[i] = p;
            bit[i] = b;
        }
        // Two keys on one value under shift 0 stay together under every shift of this mode.
        let collides = if k <= 16 {
            (0..k).any(|i| vals[i + 1..k].contains(&vals[i]))
        } else {
            let mut sorted = vals[..k].to_vec();
            sorted.sort_unstable();
            sorted.windows(2).any(|w| w[0] == w[1])
        };
        if collides {
            continue;
        }
        // The value of key `i` under shift `t`.
        let value = |i: usize, t: u64| fold(starts[i] + ((offs[i] + (t << shift)) & mask));
        // Shifts no key can take: shift 0 of mode 0, everything past `end`, and every key's word.
        let mut u = u64::from(mode == 0);
        if end < 64 {
            u |= u64::MAX << end;
        }
        for i in 0..k {
            count!(WINDOWS, 1);
            u |= if slow >> i & 1 == 1 {
                (0..end).fold(0, |w, j| w | (u64::from(taken.get(value(i, j))) << j))
            } else if period == 64 {
                // The run after the wrap is the `64 - cut` positions right before the run to
                // it, so the word is one window ending at the wrap, rotated.
                taken
                    .window(plane[i], bit[i] + cut[i] - 64)
                    .rotate_left(cut[i] as u32)
            } else if cut[i] >= end {
                taken.window(plane[i], bit[i])
            } else {
                let c = cut[i];
                let before = taken.window(plane[i], bit[i]) & ((1u64 << c) - 1);
                before | (taken.window(plane[i], bit[i] + c - period) << c)
            };
            if u == u64::MAX {
                break;
            }
        }
        // Candidates: the first feasible shift at or after each wrap, and after shift 0. The
        // sum of the values there is the offsets, plus a stride a key a shift, less a slice
        // for every key that has wrapped. A candidate whose values fold onto one another is
        // struck out and the wrap retried.
        let mut from = wraps | 1;
        while from != 0 {
            let c = u64::from(from.trailing_zeros());
            from &= from - 1;
            let free = !u & (u64::MAX << c);
            if free == 0 {
                break;
            }
            let t = u64::from(free.trailing_zeros());
            count!(CANDIDATES, 1);
            let wrapped = (0..k).filter(|&i| cut[i] <= t).count() as u64;
            let sum = base + ks_delta * t - slice * wrapped;
            if best.is_some_and(|(s, _, _)| s <= sum) {
                continue;
            }
            for (i, v) in vals[..k].iter_mut().enumerate() {
                *v = value(i, t);
            }
            if (0..k).any(|i| vals[i + 1..k].contains(&vals[i])) {
                u |= 1 << t;
                from |= 1 << c;
                continue;
            }
            best = Some((sum, mode, t));
        }
    }
    let Some((_, mode, t)) = best else {
        return 0;
    };
    for i in 0..k {
        let o = level.offset(ks[i], mode);
        taken.set(fold(starts[i] + ((o + (t << shift)) & mask)));
    }
    (mode << shift_bits) as u8 | t as u8
}

/// The seed for a level of more than 64 shifts a mode, the `MPH2` function: the same search
/// over up to four windows a key, interval by interval.
///
/// A seed is a mode and a shift. The mode picks each key's offset in its slice; shift `t` puts a
/// key `t` strides past that offset, wrapping inside the slice, so the shifts that put one key on
/// a free value are the zero bits of its occupancy window read at stride, and the shifts that
/// place the bucket are the zero bits of the OR of its keys' windows — up to 64 shifts for one
/// load per key. Two keys with the same base in a mode collide under every shift of that mode
/// and are tried in the others; the rest are caught when a candidate's values are listed.
///
/// The seed to take is the one whose values are lowest, because low values are what the buckets
/// still to come cannot use anyway. Within a mode values grow with the shift until a key wraps
/// and drops by a slice, so the sum is lowest in some later interval between wraps, and the first
/// feasible seed of each interval is a candidate. Intervals are visited from the last; one whose
/// values at entry already exceed the best candidate is skipped, and most are.
fn seed_bucket_wide(
    level: &Level,
    ks: &[u64],
    taken: &mut Map,
    origin: u64,
    scratch: &mut Scratch,
) -> u8 {
    let k = ks.len();
    if k > MAX_BUCKET {
        return 0;
    }
    let Scratch {
        starts,
        offs,
        cuts,
        nc,
        cut,
        plane,
        bit,
        vals,
        margin,
    } = scratch;
    let margin = *margin;
    count!(BUCKETS, 1);
    let (slice, mask) = (level.slice, level.slice - 1);
    let delta = 1u64 << level.shift;
    let (shift, dm) = (delta.trailing_zeros(), delta - 1);
    let period = slice >> shift;
    let ks_delta = k as u64 * delta;
    // Values past the range's end wrap to its beginning; `limit` is where that is on this map,
    // which a chunk's private map never reaches. A key whose slice crosses it is read bit by bit.
    let limit = level.n - origin;
    let fold = |v: u64| if v >= limit { v - limit } else { v };
    let mut slow = 0u64;
    for i in 0..k {
        starts[i] = scale(ks[i], level.n) - origin;
        slow |= u64::from(starts[i] + slice > limit) << i;
    }
    // The shifts of a mode are `0..end`, less shift 0 of mode 0, which is the bumped seed.
    let shift_bits = 8 - level.mode_bits;
    let modes = 1u32 << level.mode_bits;
    let end = 1u64 << shift_bits;
    let mut best: Option<(u64, u32, u64)> = None;
    for mode in 0..modes {
        // The keys' offsets in this mode, and the shifts at which they wrap, sorted: inside an
        // interval between two the sum grows by `k` strides per shift, and at each cut a key
        // drops by a slice.
        let mut n = 0;
        for i in 0..k {
            let o = level.offset(ks[i], mode);
            offs[i] = o;
            let c = (slice - o + dm) >> shift;
            cut[i] = c;
            if c < end {
                let mut at = n;
                while at > 0 && cuts[at - 1] > c {
                    cuts[at] = cuts[at - 1];
                    at -= 1;
                }
                cuts[at] = c;
                n += 1;
            }
        }
        *nc = n;
        for i in 0..k {
            vals[i] = fold(starts[i] + offs[i]);
            let (p, b) = taken.at(vals[i]);
            plane[i] = p;
            bit[i] = b;
        }
        // Two keys on one value under shift 0 stay together under every shift of this mode.
        let collides = if k <= 16 {
            (0..k).any(|i| vals[i + 1..k].contains(&vals[i]))
        } else {
            let mut sorted = vals[..k].to_vec();
            sorted.sort_unstable();
            sorted.windows(2).any(|w| w[0] == w[1])
        };
        if collides {
            continue;
        }
        // The value of key `i` under shift `t`.
        let value = |i: usize, t: u64| fold(starts[i] + ((offs[i] + (t << shift)) & mask));
        // With at most 64 shifts, a mode's whole occupancy is one word a key: its run up to
        // its wrap, and from `period` bits back after it.
        let mut whole = (end <= 64).then(|| {
            let mut u = u64::from(mode == 0);
            if end < 64 {
                u |= u64::MAX << end;
            }
            for i in 0..k {
                count!(WINDOWS, 1);
                u |= if slow >> i & 1 == 1 {
                    (0..end).fold(0, |w, j| w | (u64::from(taken.get(value(i, j))) << j))
                } else if cut[i] >= end {
                    taken.window(plane[i], bit[i])
                } else {
                    let c = cut[i];
                    let before = taken.window(plane[i], bit[i]) & ((1u64 << c) - 1);
                    before | (taken.window(plane[i], bit[i] + c - period) << c)
                };
            }
            u
        });
        if whole == Some(u64::MAX) {
            continue;
        }
        // First feasible shift in `[from, to)`, an interval no key wraps inside, so each key's
        // occupancy under 64 consecutive shifts is one window of its plane: from the bit of
        // shift 0, or `period` bits before it once the key has wrapped. A shift where two keys
        // fold onto one value is skipped.
        let first_feasible = |taken: &Map,
                              from: u64,
                              to: u64,
                              vals: &mut [u64],
                              whole: &mut Option<u64>|
         -> Option<u64> {
            if let Some(u) = whole {
                let above = if to >= 64 { u64::MAX } else { (1u64 << to) - 1 };
                loop {
                    let free = !*u & (u64::MAX << from) & above;
                    if free == 0 {
                        return None;
                    }
                    let t = u64::from(free.trailing_zeros());
                    count!(CANDIDATES, 1);
                    for (i, v) in vals[..k].iter_mut().enumerate() {
                        *v = value(i, t);
                    }
                    if !(0..k).any(|i| vals[i + 1..k].contains(&vals[i])) {
                        return Some(t);
                    }
                    *u |= 1 << t;
                }
            }
            let mut t0 = from;
            while t0 < to {
                let mut u = u64::from(t0 == 0 && mode == 0);
                if to - t0 < 64 {
                    u |= u64::MAX << (to - t0);
                }
                for i in 0..k {
                    count!(WINDOWS, 1);
                    u |= if slow >> i & 1 == 1 {
                        (0..64).fold(0, |w, j| w | (u64::from(taken.get(value(i, t0 + j))) << j))
                    } else {
                        let back = if cut[i] <= t0 { period } else { 0 };
                        taken.window(plane[i], bit[i] + t0 - back)
                    };
                    if u == u64::MAX {
                        break;
                    }
                }
                while u != u64::MAX {
                    count!(CANDIDATES, 1);
                    let j = u64::from((!u).trailing_zeros());
                    for (i, v) in vals[..k].iter_mut().enumerate() {
                        *v = value(i, t0 + j);
                    }
                    if !(0..k).any(|i| vals[i + 1..k].contains(&vals[i])) {
                        return Some(t0 + j);
                    }
                    u |= 1 << j;
                }
                t0 += 64;
            }
            None
        };
        // Intervals from the last, whose values are lowest: one is worth scanning only while it
        // can still beat the best, and at each cut going back one key un-wraps.
        let mut from = if n == 0 { 0 } else { cuts[n - 1] };
        let mut floor: u64 = (0..k).map(|i| (offs[i] + (from << shift)) & mask).sum();
        let mut to = end;
        for j in (0..=n).rev() {
            if from < to {
                let stop = match best {
                    Some((sum, _, _)) if floor + margin >= sum => None,
                    Some((sum, _, _)) => Some(to.min(from + (sum - floor).div_ceil(ks_delta))),
                    None => Some(to),
                };
                if let Some(stop) = stop {
                    count!(INTERVALS, 1);
                    if let Some(t) = first_feasible(taken, from, stop, vals, &mut whole) {
                        let sum = floor + ks_delta * (t - from);
                        if best
                            .is_none_or(|(s, m, sd)| sum < s || (sum == s && (mode, t) < (m, sd)))
                        {
                            best = Some((sum, mode, t));
                        }
                    }
                }
            }
            if j > 0 {
                let prev = if j == 1 { 0 } else { cuts[j - 2] };
                floor = floor + slice - ks_delta * (from - prev);
                to = from;
                from = prev;
            }
        }
    }
    let Some((_, mode, t)) = best else {
        return 0;
    };
    for i in 0..k {
        let o = level.offset(ks[i], mode);
        taken.set(fold(starts[i] + ((o + (t << shift)) & mask)));
    }
    (mode << shift_bits) as u8 | t as u8
}

impl Mphf {
    /// The id of `h` in `[0, n)`.
    ///
    /// Must not be called on an empty table: `[0, 0)` has no inhabitant, so there is no answer to
    /// return; callers check `n() != 0` first.
    #[inline(always)]
    pub fn index(&self, h: u64) -> u64 {
        match &self.table {
            Table::V2(t) => t.index(h),
            Table::V1(t) => t.index(h),
        }
    }

    /// [`index`](Self::index) over a batch, which a single lookup cannot see past: the cache lines
    /// a later key will read are pulled in while the current one resolves.
    ///
    /// The first level's seed is the only load most keys make, and at ~2 bits a key the seeds
    /// outgrow L2 around a million keys: from there every lookup pays a miss nothing else in the
    /// query can hide, unless the batch issued it a few dozen keys before; below it the batch is the
    /// single lookup in a loop. The few per cent of keys the first level bumps make two more loads,
    /// each known only once the one before it is read — their next level's seed, then the remap's
    /// line and low word — so they are answered after their block, in stages that each run a few
    /// keys behind the one before.
    pub fn index_all(&self, hashes: &[u64]) -> Vec<u64> {
        match &self.table {
            Table::V2(t) => t.index_all(hashes),
            Table::V1(t) => {
                const AHEAD: usize = 16;
                let mut out = Vec::with_capacity(hashes.len());
                for (i, &h) in hashes.iter().enumerate() {
                    if let Some(&next) = hashes.get(i + AHEAD) {
                        crate::blob::prefetch_byte(&t.pilots, t.locate(next).1 as usize);
                    }
                    out.push(t.index(h));
                }
                out
            }
        }
    }

    /// How many keys are in the image.
    pub fn n(&self) -> u64 {
        match &self.table {
            Table::V2(t) => t.n,
            Table::V1(t) => t.n,
        }
    }

    /// Bits per key: what the table costs, and the number its design is judged on. Only the
    /// measurements read it — a caller sizing a blob wants `PerfectHashIndex::serialized_len`,
    /// which counts the arena too.
    #[cfg(all(test, feature = "bench-mphf"))]
    pub fn bits_per_key(&self) -> f64 {
        let n = self.n();
        if n == 0 {
            return 0.0;
        }
        let header = match &self.table {
            Table::V2(t) => header_len(t.level_count()),
            Table::V1(_) => HEADER_V1,
        };
        ((self.byte_len() - header) * 8) as f64 / n as f64
    }

    /// Bytes [`to_bytes`](Self::to_bytes) will write.
    pub fn byte_len(&self) -> usize {
        match &self.table {
            Table::V2(t) => t.byte_len(),
            Table::V1(t) => t.byte_len(),
        }
    }

    /// Serialise to a self-describing blob, in the format the table was built or loaded in.
    ///
    /// Every section length is *derived* from the scalars in the header rather than written beside
    /// them, which is stronger than recording it: a loader that recomputes the lengths cannot be
    /// told a length that disagrees with the table it describes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match &self.table {
            Table::V2(t) => t.to_bytes(),
            Table::V1(t) => t.to_bytes(),
        }
    }

    /// [`to_bytes`](Self::to_bytes) written to `w` section by section rather than assembled
    /// first: the same bytes, without holding a second copy of the table.
    pub(crate) fn write_into(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        match &self.table {
            Table::V2(t) => t.write_into(w),
            Table::V1(t) => w.write_all(&t.to_bytes()),
        }
    }

    /// Reconstruct from [`to_bytes`](Self::to_bytes) output, of this version or of 1.0.
    ///
    /// **Safe on arbitrary bytes**, which is the whole reason this hash exists. Every read
    /// [`index`](Self::index) makes is bounded by a scalar in the header, so the checks are exactly
    /// that list: each one rules out an index that could otherwise leave its table. What is *not*
    /// checked is that the table is a bijection over any particular key set — that needs the keys,
    /// and a blob that is merely wrong rather than malformed answers wrong ids, not unsound ones.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let table = match bytes.get(0..4) {
            Some(m) if m == MAGIC || m == MAGIC_V2 => Table::V2(V2::from_bytes(bytes)?),
            Some(m) if m == MAGIC_V1 => Table::V1(V1::from_bytes(bytes)?),
            _ => return Err(IndexError::Format("mphf: bad magic or truncated header")),
        };
        Ok(Self { table })
    }

    /// Build over `hashes`, which must already be distinct.
    ///
    /// Fails only if the tail finds no seed, which needs input no level can spread — duplicate
    /// hashes will do it, and the caller has already ruled those out.
    pub fn build(hashes: &[u64]) -> Result<Self, IndexError> {
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        Self::build_with_threads(hashes, threads)
    }

    /// [`build`](Self::build) over `n` distinct hashes in ascending order, pulled from `hashes`
    /// one first-level chunk at a time rather than held in a slice: the table a set too large
    /// to hold in memory is built from, and byte for byte the one `build` gives the same
    /// hashes. A source that yields other than `n` hashes is refused, as is one out of order —
    /// by a wrong table, not an error, so the caller sorts. Borrowed, so a source that parks a
    /// read error can be asked about it afterwards.
    pub(crate) fn build_from_sorted(
        n: u64,
        hashes: &mut (dyn Iterator<Item = u64> + Send),
        threads: usize,
    ) -> Result<Self, IndexError> {
        if n > u64::from(u32::MAX) {
            return Err(IndexError::Build(
                "minimal perfect hash: more than u32::MAX keys",
            ));
        }
        Ok(Self {
            table: Table::V2(V2::build_streaming(n, hashes, threads)?),
        })
    }

    /// [`build`](Self::build) on a fixed number of threads.
    ///
    /// The result does not depend on `threads` — a chunk is placed from its own buckets into its
    /// own map, and the chunking is fixed, so the only thing the thread count changes is how long
    /// it takes. Exposed so that a test can prove it rather than assert it, and so a caller inside
    /// its own pool can decline to open another one.
    pub fn build_with_threads(hashes: &[u64], threads: usize) -> Result<Self, IndexError> {
        // The CSR offsets are `u32`, so the table cannot describe more keys than that. Both
        // callers refuse such an index first, for their own reason — their ids are `u32` — but
        // the limit belongs where it is assumed, not two modules away.
        if hashes.len() > u32::MAX as usize {
            return Err(IndexError::Build(
                "minimal perfect hash: more than u32::MAX keys",
            ));
        }
        Ok(Self {
            table: Table::V2(V2::build(hashes, threads.max(1))?),
        })
    }
}

// ---- The 1.0 format: read, queried, written back, never built. -------------------------------

/// Magic of a 1.0 blob.
const MAGIC_V1: &[u8; 4] = b"MPH1";

const FORMAT_V1: u16 = 1;

/// Magic 4, version 2, reserved 2, eight `u64` scalars, then a `u32` check over all of that.
const HEADER_V1: usize = 4 + 2 + 2 + 8 * 8 + 4;

/// Header bytes the trailing check covers.
const CHECKED_V1: usize = HEADER_V1 - 4;

/// Spread a key hash for the part and bucket choice: one multiply, since `h` arrives avalanched.
#[inline(always)]
fn spread(h: u64, seed: u64) -> u64 {
    (h ^ seed).wrapping_mul(MUL[0])
}

/// The four multipliers behind a key's four window bases: `2^64 · frac(√p)` for the first four
/// primes, rounded to odd.
const MUL: [u64; 4] = [
    0x6A09_E667_F3BC_C909,
    0xBB67_AE85_84CA_A73B,
    0x3C6E_F372_FE94_F82B,
    0xA54F_F53A_5F1D_36F1,
];

/// The 1.0 table: a PtrHash-shaped pilot per bucket, parts placed independently, slots at or above
/// `n` remapped down. Blob written by 1.0 still load and answer the ids they always did.
#[derive(Debug, Clone, PartialEq, Eq)]
struct V1 {
    n: u64,
    slots: u64,
    parts: u64,
    buckets_per_part: u64,
    slots_per_part: u64,
    stride: u64,
    dense_buckets: u64,
    part_seed: Vec<u64>,
    pilots: Vec<u8>,
    remap_base: Vec<u32>,
    remap_off: Vec<u16>,
    seed: u64,
}

impl V1 {
    /// Which bucket of its own part a hash belongs to: skewed, 60 % of the space into the first
    /// 30 % of the buckets, branchless because the split is as unpredictable as a branch gets.
    #[inline(always)]
    fn bucket_in_part(hb: u64, buckets: u64, dense: u64) -> u64 {
        let hl = hb.rotate_left(17);
        if dense == 0 {
            return scale(hl, buckets);
        }
        let (lo, span) = if hl < (u64::MAX / 5) * 3 {
            (0, dense)
        } else {
            (dense, buckets - dense)
        };
        lo + scale(hl.rotate_left(23), span)
    }

    #[inline(always)]
    fn base(h: u64, part_seed: u64, j: usize, slots: u64) -> u64 {
        scale((h ^ part_seed).wrapping_mul(MUL[j]), slots)
    }

    /// Which part `h` falls in and which of that part's buckets, as a flat index into `pilots`.
    #[inline(always)]
    fn locate(&self, h: u64) -> (u64, u64) {
        let hb = spread(h, self.seed);
        let part = scale(hb, self.parts);
        let b = part * self.buckets_per_part
            + Self::bucket_in_part(hb, self.buckets_per_part, self.dense_buckets);
        (part, b)
    }

    #[inline(always)]
    fn index(&self, h: u64) -> u64 {
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

    fn byte_len(&self) -> usize {
        HEADER_V1
            + self.part_seed.len() * 8
            + self.pilots.len()
            + self.remap_base.len() * 4
            + self.remap_off.len() * 2
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.byte_len());
        let mut header = [0u8; HEADER_V1];
        header[0..4].copy_from_slice(MAGIC_V1);
        header[4..6].copy_from_slice(&FORMAT_V1.to_le_bytes());
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
        let check = crate::blob::hash_bytes(&header[..CHECKED_V1]) as u32;
        header[CHECKED_V1..].copy_from_slice(&check.to_le_bytes());
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

    fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() < HEADER_V1 || &bytes[0..4] != MAGIC_V1 {
            return Err(IndexError::Format("mphf: bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED_V1..HEADER_V1].try_into().expect("4 bytes"));
        if check != crate::blob::hash_bytes(&bytes[..CHECKED_V1]) as u32 {
            return Err(IndexError::Format("mphf: header checksum mismatch"));
        }
        if u16::from_le_bytes(bytes[4..6].try_into().expect("2 bytes")) != FORMAT_V1 {
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
            if bytes.len() != HEADER_V1
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
        // so a part's slots stay inside its own stride only with this much room.
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

        let entries = usize::try_from(slots - n).map_err(|_| SIZE)?;
        let bucket_count = usize::try_from(
            parts
                .checked_mul(buckets_per_part)
                .ok_or(IndexError::Format("mphf: bucket count out of range"))?,
        )
        .map_err(|_| IndexError::Format("mphf: bucket count out of range"))?;
        let seeds = usize::try_from(parts)
            .map_err(|_| IndexError::Format("mphf: part count out of range"))?;
        let blocks = entries.div_ceil(REMAP_BLOCK);
        let want = HEADER_V1
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

        let mut at = HEADER_V1;
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
}

/// The hashes of the golden key list every byte-pinned fixture is built from.
#[cfg(test)]
fn golden_hashes() -> Vec<u64> {
    let keys = include_str!("../tests/data/golden-keys.txt");
    let mut hs: Vec<u64> = keys.lines().map(crate::hash::hash_key).collect();
    hs.sort_unstable();
    hs.dedup();
    hs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct pseudo-random hashes, ascending: what the streaming build is promised.
    fn sorted_hashes(n: usize) -> Vec<u64> {
        let mut hs: Vec<u64> = (0..n as u64)
            .map(|i| mix(i ^ 0x5DEE_CE66_D1CE_4E5B))
            .collect();
        hs.sort_unstable();
        hs.dedup();
        assert_eq!(hs.len(), n);
        hs
    }

    /// The table built from a stream is the table built from the slice, byte for byte: below
    /// the tail's size, in one chunk, and over enough keys for several chunks and the pieces at
    /// the level's end — on one thread and on several, since the chunks are claimed in whatever
    /// order the threads reach them.
    #[test]
    fn a_streamed_build_is_byte_identical_to_the_sliced_one() {
        for n in [0usize, 1, 100, 300, 5_000, 1_000_000] {
            let hs = sorted_hashes(n);
            for threads in [1usize, 4] {
                let sliced = Mphf::build_with_threads(&hs, threads).unwrap().to_bytes();
                let streamed = Mphf::build_from_sorted(n as u64, &mut hs.iter().copied(), threads)
                    .unwrap()
                    .to_bytes();
                assert!(sliced == streamed, "n = {n}, threads = {threads}");
            }
        }
    }

    /// A stream that yields fewer or more hashes than promised cannot have built the level it
    /// was shaped for, and says so instead of handing back a table over the wrong set.
    #[test]
    fn a_streamed_build_refuses_a_source_of_the_wrong_length() {
        let hs = sorted_hashes(5_000);
        for (n, take) in [
            (5_000u64, 4_999usize),
            (5_000, 5_000 - 1),
            (4_999, 5_000),
            (100, 99),
            (100, 101),
        ] {
            let result = Mphf::build_from_sorted(n, &mut hs.iter().copied().take(take), 2);
            if take as u64 == n {
                assert!(result.is_ok(), "n = {n}, yielded {take}");
            } else {
                let err = result.err().map(|e| e.to_string()).unwrap_or_default();
                assert!(
                    err.contains("promised number of keys"),
                    "n = {n}, yielded {take}: {err}"
                );
            }
        }
    }

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
    fn assert_bijection(hs: &[u64]) -> Mphf {
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
        mphf
    }

    fn v2(m: &Mphf) -> &V2 {
        match &m.table {
            Table::V2(t) => t,
            Table::V1(_) => panic!("a built table is V2"),
        }
    }

    #[test]
    fn it_is_a_bijection_onto_the_dense_range() {
        for n in [1usize, 2, 3, 7, 64, 1_000, 10_000, 20_000] {
            assert_bijection(&hashes(n));
        }
    }

    /// The batch answers what the single lookup answers, key for key, for members and for hashes
    /// never built in: in a loop and staged, over tables with no first level, with only a tail
    /// behind it and with later levels, and on batches that end inside a block.
    #[test]
    fn index_all_answers_what_index_answers() {
        let strangers: Vec<u64> = (0..20_000u64).map(|i| mix(!i)).collect();
        for n in [1usize, 100, 300, 5_000, 200_000] {
            let hs = hashes(n);
            let m = Mphf::build(&hs).expect("build");
            let t = v2(&m);
            if n == 200_000 {
                assert!(!t.rest.is_empty(), "no later level to stage through");
            }
            for probe in [&hs[..], &strangers[..]] {
                let want: Vec<u64> = probe.iter().map(|&h| m.index(h)).collect();
                assert_eq!(m.index_all(probe), want, "n = {n}");
                for len in [0, 1, SEED_AHEAD + 1, BATCH_BLOCK + 3, probe.len()] {
                    let part = &probe[..len.min(probe.len())];
                    assert_eq!(
                        t.index_all_from(part, 0),
                        want[..part.len()],
                        "n = {n}, {len} keys"
                    );
                }
            }
        }
    }

    /// The select inside a word finds the bit that clearing the lower set bits one at a time
    /// reaches, for every rank, in words dense, sparse and with whole bytes empty.
    #[test]
    fn select_in_word_finds_the_rth_set_bit() {
        let edges = [
            1u64,
            1 << 63,
            u64::MAX,
            0x8000_0000_0000_0001,
            0x00FF_0000_0000_FF00,
        ];
        let random = (0..20_000u64).map(|i| {
            let x = mix(i);
            match i % 4 {
                0 => x,
                1 => x & mix(!x),
                2 => x & mix(!x) & mix(x ^ i),
                _ => x | mix(!x),
            }
        });
        for word in edges.into_iter().chain(random) {
            let mut rest = word;
            for r in 0..u64::from(word.count_ones()) {
                assert_eq!(
                    select_in_word(word, r),
                    u64::from(rest.trailing_zeros()),
                    "{word:#x}, r = {r}"
                );
                rest &= rest - 1;
            }
        }
    }

    /// `Remap::get` returns every value it encoded: uniform sequences over dense and sparse
    /// universes, a sequence with runs of one value, two clusters at the ends of the universe, and
    /// one whose last line spans far more than the natural low bits let a line hold, so that the
    /// encoder has to go up from them.
    #[test]
    fn the_remap_returns_every_value_it_encoded() {
        let uniform = |len: u64, u: u64| {
            let mut v: Vec<u64> = (0..len).map(|i| scale(mix(i ^ u), u)).collect();
            v.sort_unstable();
            v
        };
        let top = 1u64 << 32;
        let packed: Vec<u64> = (0..99 * LINE as u64 - 1).chain([1 << 30]).collect();
        let cases = [
            (vec![7], 10),
            ((0..700).collect(), 700),
            ((0..3_000).map(|i| i / 7).collect(), 500),
            (uniform(1_000, 2_000), 2_000),
            (uniform(3_000, 100_000), 100_000),
            (uniform(200_000, 1 << 36), 1 << 36),
            ((0..1_000).chain(top - 1_000..top).collect(), top),
            (packed, 1 << 30 | 1),
        ];
        for (values, u) in cases {
            let remap = Remap::encode(&values, u);
            assert!(remap.validate(u));
            assert!(remap.low_bits >= Remap::natural_low_bits(values.len() as u64, u));
            for (j, &v) in values.iter().enumerate() {
                assert_eq!(
                    remap.get(j as u64),
                    v,
                    "value {j} of {} below {u}",
                    values.len()
                );
            }
        }
        // A line spread too far for its low bits is sparse before the whole table's low bits
        // go up: the sparse form holds twice the span, and past that the low bits rise.
        let natural = |values: &[u64], u| Remap::natural_low_bits(values.len() as u64, u);
        // 12 799 values below 2^19 keep 5 low bits; the last line's 127 values step by 100, a
        // span of 393 high parts at 5 bits (too wide for 480 unary bits) and 196 at 6.
        let sparse: Vec<u64> = (0..99 * LINE as u64)
            .chain((0..LINE as u64 - 1).map(|i| 99 * LINE as u64 + i * 100))
            .collect();
        let u = 1 << 19;
        let remap = Remap::encode(&sparse, u);
        assert_eq!((remap.low_bits, natural(&sparse, u)), (5, 5));
        assert!(remap.lines[..99].iter().all(|l| l.0[0] & SPARSE == 0));
        assert!(remap.lines[99].0[0] & SPARSE != 0);
        assert!((0..sparse.len()).all(|j| remap.get(j as u64) == sparse[j]));
        let far: Vec<u64> = (0..99 * LINE as u64 - 1).chain([1 << 30]).collect();
        let u = (1 << 30) + 1;
        let remap = Remap::encode(&far, u);
        assert!(remap.low_bits > natural(&far, u));
        assert!((0..far.len()).all(|j| remap.get(j as u64) == far[j]));
    }

    /// A real table's remap is the gapless sequence: one value per value of the levels below the
    /// first and of the tail, non-decreasing, one step per key the first level bumped, and every
    /// step onto a hole the first level left.
    #[test]
    fn the_remap_is_the_gapless_hole_sequence() {
        let hs = hashes(200_000);
        let m = Mphf::build(&hs).expect("build");
        let t = v2(&m);
        let first = t.first.as_ref().expect("a first level");
        let entries = t.rest.iter().map(|l| l.n).sum::<u64>() + t.tail.range;
        assert_eq!(t.remap.len, entries);
        assert!(t.remap.lines.len() > 4, "a few lines prove little");
        let bumped = hs
            .iter()
            .filter(|&&h| first.seeds[scale(h, first.buckets) as usize] == 0)
            .count();
        let mut taken = vec![false; hs.len()];
        for &h in &hs {
            let seed = first.seeds[scale(h, first.buckets) as usize];
            if seed != 0 {
                taken[first.value(h, seed) as usize] = true;
            }
        }
        let mut steps = 0;
        let mut last = t.remap.get(0);
        assert!(!taken[last as usize]);
        for j in 1..entries {
            let v = t.remap.get(j);
            assert!(v >= last, "value {j}");
            if v > last {
                steps += 1;
                assert!(!taken[v as usize], "value {j} is not a hole");
            }
            last = v;
        }
        assert_eq!(steps + 1, bumped);
    }

    /// Sorted input is the fast path; unsorted input must build the same table.
    #[test]
    fn unsorted_input_builds_the_same_table() {
        let hs = hashes(10_000);
        let mut shuffled = hs.clone();
        shuffled.reverse();
        shuffled.swap(1, 4000);
        let a = Mphf::build(&hs).unwrap();
        let b = Mphf::build(&shuffled).unwrap();
        assert_eq!(a, b);
    }

    /// The gate's determinism criterion, as a test rather than a claim: chunks are fixed and each is
    /// placed from its own buckets into its own map, so the thread count cannot reach the result.
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

    /// Duplicate hashes cannot be told apart by any level; the contract is a `Build` error, not
    /// a loop or a panic.
    #[test]
    fn duplicate_hashes_are_refused() {
        let mut hs = hashes(6000);
        hs[10] = hs[11];
        assert!(matches!(Mphf::build(&hs), Err(IndexError::Build(_))));
        let small = [7u64, 7, 9];
        assert!(matches!(Mphf::build(&small), Err(IndexError::Build(_))));
    }

    /// Sizes around the tail threshold and the chunk boundary, where a table goes from tail-only
    /// to one bumping level, and from one chunk to two chunks and a gap.
    #[test]
    fn it_holds_where_the_levels_and_chunks_divide() {
        let per_chunk = (CHUNK * LAMBDA.0 / LAMBDA.1) as usize;
        for n in [
            TAIL_KEYS as usize,
            TAIL_KEYS as usize + 1,
            per_chunk - 1,
            per_chunk + 1000,
            2 * per_chunk + 1,
        ] {
            let m = assert_bijection(&hashes(n));
            let t = v2(&m);
            assert_eq!(t.first.is_none(), n <= TAIL_KEYS as usize, "n = {n}");
        }
    }

    #[test]
    fn an_empty_set_builds_and_answers_nothing() {
        let mphf = Mphf::build(&[]).unwrap();
        assert_eq!(mphf.n(), 0);
        assert_eq!(mphf.byte_len(), header_len(0));
    }

    /// Sizes either side of a power of two, and of the 64-bit words the occupancy bitset uses.
    #[test]
    fn it_holds_at_the_boundaries_of_its_own_word_size() {
        for n in [63usize, 64, 65, 127, 128, 129, 255, 256, 257] {
            assert_bijection(&hashes(n));
        }
    }

    /// One table big enough to have two chunks, a second bumping level and a tail, built once:
    /// every blob test below mutates *this* blob rather than random bytes, because random bytes
    /// never spell `MPH3` and would only ever exercise the first line of the loader.
    fn reference() -> &'static (Vec<u64>, Vec<u8>) {
        static REF: std::sync::OnceLock<(Vec<u64>, Vec<u8>)> = std::sync::OnceLock::new();
        REF.get_or_init(|| {
            let hs = hashes(300_000);
            let m = assert_bijection(&hs);
            let t = v2(&m);
            assert!(
                t.level_count() >= 2,
                "the reference should bump into a second level"
            );
            assert!(t.tail.buckets > 0, "the reference should have a tail");
            (hs, m.to_bytes())
        })
    }

    /// The header's level count, read back from a blob.
    fn levels_of(blob: &[u8]) -> usize {
        u64::from_le_bytes(blob[16..24].try_into().unwrap()) as usize
    }

    /// Rewrite one header scalar and re-checksum, which is what an adversary does: the checksum
    /// catches corruption, not intent, so every field-level check has to stand on its own.
    fn with_scalar(blob: &[u8], field: usize, value: u64) -> Vec<u8> {
        let mut out = blob.to_vec();
        out[8 + field * 8..16 + field * 8].copy_from_slice(&value.to_le_bytes());
        // The check stays where the blob's real header ends, even when the level count is what
        // was rewritten: a count that moves the check is a length disagreement for the loader.
        let hl = header_len(levels_of(blob));
        let check = crate::blob::hash_bytes(&out[..hl - 4]) as u32;
        out[hl - 4..hl].copy_from_slice(&check.to_le_bytes());
        out
    }

    /// Field `j` of level row `i`: keys, buckets, slice.
    fn with_row(blob: &[u8], i: usize, j: usize, value: u64) -> Vec<u8> {
        with_scalar(blob, 7 + i * 3 + j, value)
    }

    #[test]
    fn a_blob_round_trips_to_the_same_table() {
        for n in [0usize, 1, 2, 63, 64, 65, 1000, 100_000] {
            let hs = hashes(n);
            let mphf = Mphf::build(&hs).expect("build");
            let blob = mphf.to_bytes();
            assert_eq!(blob.len(), mphf.byte_len());
            let back = Mphf::from_bytes(&blob).expect("its own blob loads");
            assert_eq!(back, mphf, "n = {n}");
            assert_eq!(back.to_bytes(), blob, "n = {n}");
            for &h in &hs {
                assert_eq!(back.index(h), mphf.index(h), "n = {n}");
            }
        }
    }

    /// A cut blob has a header that still checksums; only the derived total length catches it.
    #[test]
    fn a_truncated_blob_is_refused() {
        let (_, blob) = reference();
        let hl = header_len(levels_of(blob));
        for cut in [
            0,
            1,
            4,
            FIXED,
            hl - 1,
            hl,
            hl + 1,
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
        let n = at(0);
        let slice0 = at(9);
        let cases: [(Vec<u8>, &str); 14] = [
            (with_scalar(blob, 0, n + 1), "a first level not covering n"),
            (
                with_scalar(blob, 1, 0),
                "no levels, with their bytes still present",
            ),
            (
                with_scalar(blob, 1, MAX_LEVELS as u64 + 1),
                "too many levels",
            ),
            (
                with_scalar(blob, 2, 0),
                "tail keys zero with a tail present",
            ),
            (with_scalar(blob, 3, 0), "tail buckets zero with a range"),
            (with_scalar(blob, 4, 0), "tail range zero with buckets"),
            (
                with_scalar(blob, 4, at(4) + 64),
                "a tail range the remap does not cover",
            ),
            (with_row(blob, 0, 1, 0), "a level with no buckets"),
            (
                with_row(blob, 0, 2, slice0 + 1),
                "a slice that is not a power of two",
            ),
            (
                with_row(blob, 0, 2, 1 << 40),
                "a slice reaching past its level",
            ),
            (with_row(blob, 1, 0, 0), "a level with no keys"),
            (
                with_row(blob, 1, 0, u64::MAX),
                "a level range the remap cannot hold",
            ),
            (with_scalar(blob, 6, 64), "remap low bits past a word"),
            (
                with_scalar(blob, 6, at(6) ^ 1),
                "remap low bits disagreeing with the sections",
            ),
        ];
        for (bad, what) in cases {
            assert!(Mphf::from_bytes(&bad).is_err(), "accepted {what}");
        }
        // The tail seed names no section, so any value loads — the table then hands out ids the
        // builder never would, which is a wrong blob, not an unsound one. That split is the whole
        // contract: validated for soundness, trusted for correctness.
        assert!(Mphf::from_bytes(&with_scalar(blob, 5, at(5) ^ 1)).is_ok());
        // A header that checksums but was written by a different version of this file, one
        // naming a seed geometry this version does not know, and one whose reserved byte carries
        // a flag it does not know about.
        let hl = header_len(levels_of(blob));
        for (at, word) in [(4usize, FORMAT + 1), (6, 1), (6, 1 << 8)] {
            let mut bad = blob.clone();
            bad[at..at + 2].copy_from_slice(&word.to_le_bytes());
            let check = crate::blob::hash_bytes(&bad[..hl - 4]) as u32;
            bad[hl - 4..hl].copy_from_slice(&check.to_le_bytes());
            assert!(
                Mphf::from_bytes(&bad).is_err(),
                "accepted a header word at {at}"
            );
        }
    }

    #[test]
    fn a_flipped_header_bit_is_caught_by_the_checksum() {
        let (_, blob) = reference();
        let hl = header_len(levels_of(blob));
        for byte in 0..hl - 4 {
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
    /// seeds are unconstrained by construction, and the remap is validated by value on load — so
    /// this holds for arbitrary bytes rather than only for corruption-free ones.
    #[test]
    fn an_arbitrary_body_still_answers_inside_the_image() {
        let (hs, blob) = reference();
        let hl = header_len(levels_of(blob));
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
                let i = hl + (next() as usize) % (bad.len() - hl);
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
    /// table checked by value. Raise a line's base so that its values pass `n`, and the blob must
    /// be refused; so must a line with a one too few or too many, since a select past the last
    /// one would read some other bit.
    #[test]
    fn a_remap_entry_outside_the_image_is_refused() {
        let (_, blob) = reference();
        let m = Mphf::from_bytes(blob).unwrap();
        let t = v2(&m);
        let (seeds, low, _) = t.sections();
        let lines_at = header_len(t.level_count()) + seeds + low;
        let last = lines_at + (t.remap.lines.len() - 1) * 64;
        let mut bad = blob.clone();
        bad[last..last + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            Mphf::from_bytes(&bad).is_err(),
            "a hole past the image was accepted"
        );
        let word = t.remap.lines[0].0[1];
        for flipped in [word & (word - 1), word | (!word & (!word).wrapping_neg())] {
            let mut bad = blob.clone();
            bad[lines_at + 8..lines_at + 16].copy_from_slice(&flipped.to_le_bytes());
            assert!(
                Mphf::from_bytes(&bad).is_err(),
                "a line with the wrong number of ones was accepted"
            );
        }
    }

    #[test]
    fn an_empty_blob_is_only_accepted_with_an_empty_shape() {
        let empty = Mphf::build(&[]).expect("build").to_bytes();
        assert_eq!(empty.len(), header_len(0));
        assert_eq!(Mphf::from_bytes(&empty).expect("loads").n(), 0);
        // n = 0 with a shape claimed anyway: the loader must not read the tables that shape names.
        for field in 1..7 {
            assert!(Mphf::from_bytes(&with_scalar(&empty, field, 1)).is_err());
        }
    }

    /// The committed `MPH3` fixture is byte for byte what a fresh build over the golden keys'
    /// hashes writes: construction is deterministic, so a changed header field, section order,
    /// checksum or placement rule fails here, at the line that names the format. Regenerate it
    /// only after a deliberate change, with the `write_golden_mphf` spike.
    #[test]
    fn the_current_golden_blob_is_byte_identical_to_a_fresh_build() {
        const GOLDEN: &[u8] = include_bytes!("../tests/data/golden-3.1.0-mphf.bin");
        let mphf = Mphf::build(&golden_hashes()).expect("build");
        assert!(matches!(mphf.table, Table::V2(_)));
        assert_eq!(&GOLDEN[..4], MAGIC);
        assert_eq!(
            mphf.to_bytes(),
            GOLDEN,
            "regenerate tests/data/golden-3.1.0-mphf.bin"
        );
        assert_eq!(Mphf::from_bytes(GOLDEN).expect("parses"), mphf);
    }

    /// The committed `MPH2` fixture — the same keys under 3.0's geometry — still parses and still
    /// answers the same bijection, through the loaded geometry rather than this version's;
    /// written again it is an `MPH3` blob naming that geometry, with the remap in this layout,
    /// which loads to the same table.
    #[test]
    fn the_mph2_golden_blob_still_reads_under_its_own_geometry() {
        const GOLDEN: &[u8] = include_bytes!("../tests/data/golden-2.0.0-mphf.bin");
        let mphf = Mphf::from_bytes(GOLDEN).expect("the committed MPH2 fixture parses");
        assert_eq!(&GOLDEN[..4], MAGIC_V2);
        let Table::V2(t) = &mphf.table else {
            panic!("an MPH2 blob is a levels table");
        };
        assert_eq!(t.geometry, Geometry::Mph2);
        assert!(
            t.levels()
                .all(|l| l.mode_bits == 0 && l.shift == stride_for(l.slice, 0).trailing_zeros())
        );
        let hs = golden_hashes();
        let mut seen = vec![false; hs.len()];
        for &h in &hs {
            let id = mphf.index(h) as usize;
            assert!(id < hs.len() && !seen[id]);
            seen[id] = true;
        }
        let again = mphf.to_bytes();
        assert_eq!((&again[..4], again[6]), (&MAGIC[..], 0));
        assert_eq!(again.len(), mphf.byte_len());
        assert_eq!(Mphf::from_bytes(&again).expect("loads"), mphf);
        let fresh = Mphf::build(&hs).expect("build").to_bytes();
        assert_eq!(fresh[6], MODE_BITS_BYTE);
        assert_ne!(fresh, again);
    }

    /// The committed `MPH1` fixture, parsed and written straight back — byte for byte.
    ///
    /// Every scalar in that header is little-endian on purpose and a `to_ne_bytes` slip is
    /// invisible on x86, so this is the assertion the weekly big-endian Miri job runs. It reads a
    /// blob rather than building one, which is what makes it affordable there.
    #[test]
    fn the_golden_blob_parses_and_writes_back_identically() {
        const GOLDEN: &[u8] = include_bytes!("../tests/data/golden-1.0.0-mphf.bin");
        let mphf = Mphf::from_bytes(GOLDEN).expect("the committed MPH1 fixture parses");
        assert_eq!(mphf.to_bytes(), GOLDEN);
        assert_eq!(mphf.byte_len(), GOLDEN.len());
        assert!(matches!(mphf.table, Table::V1(_)));
        assert!(mphf.n() > 0);
        for h in [0u64, 1, u64::MAX, 0x9e37_79b9_7f4a_7c15] {
            assert!(mphf.index(h) < mphf.n());
        }
        assert_eq!(
            mphf.index_all(&[1, 2, 3]),
            vec![mphf.index(1), mphf.index(2), mphf.index(3)]
        );
    }

    /// Rewrite one of the eight `MPH1` header scalars and re-checksum.
    fn with_v1_scalar(blob: &[u8], field: usize, value: u64) -> Vec<u8> {
        let mut out = blob.to_vec();
        out[8 + field * 8..16 + field * 8].copy_from_slice(&value.to_le_bytes());
        let check = crate::blob::hash_bytes(&out[..CHECKED_V1]) as u32;
        out[CHECKED_V1..HEADER_V1].copy_from_slice(&check.to_le_bytes());
        out
    }

    /// The 1.0 loader's invariants, on the 1.0 fixture: nothing builds that format any more, so
    /// the fixture is the only blob these checks can be driven through.
    #[test]
    fn each_v1_header_invariant_is_enforced() {
        const GOLDEN: &[u8] = include_bytes!("../tests/data/golden-1.0.0-mphf.bin");
        let at = |i: usize| u64::from_le_bytes(GOLDEN[8 + i * 8..16 + i * 8].try_into().unwrap());
        let (n, parts, stride) = (at(0), at(2), at(5));
        let (buckets_per_part, slots_per_part) = (at(3), at(4));
        assert!(parts >= 1);
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
                Mphf::from_bytes(&with_v1_scalar(GOLDEN, field, value)).is_err(),
                "accepted {what}"
            );
        }
        assert!(slots_per_part <= stride - 63);
        assert!(Mphf::from_bytes(&with_v1_scalar(GOLDEN, 4, stride - 63)).is_ok());
        for (at, word) in [(4usize, FORMAT_V1 + 1), (6, 1)] {
            let mut bad = GOLDEN.to_vec();
            bad[at..at + 2].copy_from_slice(&word.to_le_bytes());
            let check = crate::blob::hash_bytes(&bad[..CHECKED_V1]) as u32;
            bad[CHECKED_V1..HEADER_V1].copy_from_slice(&check.to_le_bytes());
            assert!(
                Mphf::from_bytes(&bad).is_err(),
                "accepted a header word at {at}"
            );
        }
        for cut in [HEADER_V1 - 1, HEADER_V1, GOLDEN.len() - 1] {
            assert!(Mphf::from_bytes(&GOLDEN[..cut]).is_err());
        }
        let len = GOLDEN.len();
        let mut bad = GOLDEN.to_vec();
        bad[len - 2..].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(
            Mphf::from_bytes(&bad).is_err(),
            "a remap entry past the image"
        );
        let empty = with_v1_scalar(&GOLDEN[..HEADER_V1], 0, 0);
        assert!(Mphf::from_bytes(&empty).is_err(), "a zero n with a shape");
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
            field in 0usize..13,
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

/// Measurements, behind their own feature. They take minutes and answer "how big and how fast",
/// which is a question about this machine rather than about correctness — and an `#[ignore]`d test
/// still counts as uncovered code, so leaving them in the default build would spend a percent of
/// the coverage floor on scaffolding that never runs.
///
/// `cargo test --features bench-mphf --release --lib mphf::spike -- --ignored --nocapture`
#[cfg(all(test, feature = "bench-mphf"))]
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

    fn env(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }

    /// The levels a table has: keys on each, and in the tail.
    fn shape(m: &Mphf) -> String {
        match &m.table {
            Table::V2(t) => {
                let levels: Vec<String> = t.levels().map(|l| l.n.to_string()).collect();
                format!("levels [{}] tail {}", levels.join(", "), t.tail.keys)
            }
            Table::V1(_) => "v1".to_string(),
        }
    }

    /// One thread, 10 M keys — the number the construction work is measured against.
    /// `perf record` on this test is how a phase is opened up further.
    #[test]
    #[ignore = "measurement, not a test"]
    fn build_single_threaded() {
        let n = env("LEXINDEX_MPHF_N", 10_000_000);
        let rounds = env("LEXINDEX_MPHF_ROUNDS", 3);
        let threads = env("LEXINDEX_MPHF_THREADS", 1);
        let hs = bigram_hashes(n);
        let mut best = f64::INFINITY;
        let mut bits = 0.0;
        let mut shape_s = String::new();
        for _ in 0..rounds {
            let t = std::time::Instant::now();
            let m = Mphf::build_with_threads(&hs, threads).expect("build");
            best = best.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
            bits = m.bits_per_key();
            shape_s = shape(&m);
            std::hint::black_box(m);
        }
        println!(
            "n {n:>9}   bits/key {bits:>6.3}   build {best:>7.1} ns/key ({threads} thread(s), min of {rounds})   {shape_s}"
        );
    }

    /// Parameter sweep: `LEXINDEX_MPHF_SWEEP="lambda=4.15,slice=1024,w1=-50137,w2=65904;..."`,
    /// one build per config, single-threaded, on `LEXINDEX_MPHF_N` keys. Prints the size and its
    /// parts, and every level's bumped fraction.
    #[test]
    #[ignore = "measurement, not a test"]
    fn sweep() {
        let n = env("LEXINDEX_MPHF_N", 10_000_000);
        let hs = bigram_hashes(n);
        let spec = std::env::var("LEXINDEX_MPHF_SWEEP").unwrap_or_default();
        let names = [
            "lambda", "slice", "modes", "unused3", "tlambda", "talpha", "window", "delta", "w1",
            "w2", "w3", "w4", "w5", "w6", "w7", "unused15", "margin", "chunk", "phantom", "tail",
        ];
        let threads = env("LEXINDEX_MPHF_THREADS", 1);
        for cfg in spec.split(';').filter(|c| !c.trim().is_empty()) {
            for i in 0..names.len() {
                tune::SET[i].store(0, std::sync::atomic::Ordering::Relaxed);
            }
            for kv in cfg.split(',') {
                let (k, v) = kv.trim().split_once('=').expect("key=value");
                let i = names
                    .iter()
                    .position(|&nm| nm == k)
                    .expect("a known parameter");
                tune::set(i, v.parse().expect("a number"));
            }
            work::reset(std::env::var_os("LEXINDEX_MPHF_WORK").is_some());
            let t = std::time::Instant::now();
            let m = Mphf::build_with_threads(&hs, threads).expect("build");
            let ns = t.elapsed().as_secs_f64() * 1e9 / n as f64;
            if std::env::var_os("LEXINDEX_MPHF_WORK").is_some() {
                println!("    {}", work::report());
            }
            let mut seen = vec![false; n];
            for &h in &hs {
                let id = m.index(h) as usize;
                assert!(id < n && !seen[id], "{cfg}: not a bijection");
                seen[id] = true;
            }
            let Table::V2(v) = &m.table else {
                unreachable!()
            };
            let (seeds, low, lines) = v.sections();
            // Lookup cost over a shuffled probe order, min of 3.
            let mut order: Vec<u32> = (0..n as u32).collect();
            let mut r = 0x2545_F491_4F6C_DD1Du64;
            for i in (1..n).rev() {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                order.swap(i, (r % (i as u64 + 1)) as usize);
            }
            let mut id_ns = f64::INFINITY;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let mut acc = 0u64;
                for &i in &order {
                    acc = acc.wrapping_add(m.index(hs[i as usize]));
                }
                std::hint::black_box(acc);
                id_ns = id_ns.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
            }
            let eps: Vec<String> = v
                .levels()
                .map(|l| l.n)
                .chain(std::iter::once(v.tail.keys))
                .collect::<Vec<_>>()
                .windows(2)
                .map(|w| format!("{:.2}%", 100.0 * w[1] as f64 / w[0] as f64))
                .collect();
            // Where the first level's bumped keys come from: buckets by size, and the share of
            // each size's keys that were bumped.
            if let Some(l) = &v.first {
                let mut size = vec![0u32; l.buckets as usize];
                for &h in &hs {
                    size[scale(h, l.buckets) as usize] += 1;
                }
                let mut keys = [0u64; 24];
                let mut lost = [0u64; 24];
                for (b, &k) in size.iter().enumerate() {
                    let c = (k as usize).min(23);
                    keys[c] += u64::from(k);
                    if l.seeds[b] == 0 {
                        lost[c] += u64::from(k);
                    }
                }
                let total: u64 = lost.iter().sum();
                let row: Vec<String> = (1..24)
                    .filter(|&c| keys[c] > 0)
                    .map(|c| {
                        format!(
                            "{}{}:{:.1}%/{:.0}%",
                            c,
                            if c == 23 { "+" } else { "" },
                            100.0 * lost[c] as f64 / keys[c].max(1) as f64,
                            100.0 * lost[c] as f64 / total.max(1) as f64
                        )
                    })
                    .collect();
                println!(
                    "    bumped by bucket size (share of its keys / share of all bumped): {}",
                    row.join(" ")
                );
                // Buckets no shift can place: two keys on one value under shift 1 stay
                // together under nearly every shift, whatever the load, so their keys are the
                // floor under the bumped share. `hs` is sorted and `scale` is monotone, so a
                // bucket's keys are one run of `hs`.
                let (mut stuck_keys, mut stuck_lost) = (0u64, 0u64);
                let mut at = 0usize;
                while at < hs.len() {
                    let b = scale(hs[at], l.buckets);
                    let mut end = at;
                    while end < hs.len() && scale(hs[end], l.buckets) == b {
                        end += 1;
                    }
                    let run = &hs[at..end];
                    let stuck = (0..run.len()).any(|i| {
                        run[i + 1..]
                            .iter()
                            .any(|&h| l.value(h, 1) == l.value(run[i], 1))
                    });
                    if stuck {
                        stuck_keys += run.len() as u64;
                        stuck_lost += u64::from(l.seeds[b as usize] == 0) * run.len() as u64;
                    }
                    at = end;
                }
                println!(
                    "    stuck buckets (a same-value pair under shift 1): {:.2}% of keys, {:.1}% of all bumped, {:.1}% of their own keys bumped",
                    100.0 * stuck_keys as f64 / n as f64,
                    100.0 * stuck_lost as f64 / total.max(1) as f64,
                    100.0 * stuck_lost as f64 / stuck_keys.max(1) as f64
                );
                // Where in its slice a placed key lands: the cumulative share of keys at or
                // below each sixteenth of the slice.
                if std::env::var_os("LEXINDEX_MPHF_OFFSETS").is_some() {
                    let mut bins = [0u64; 16];
                    for &h in &hs {
                        let b = scale(h, l.buckets);
                        let seed = l.seeds[b as usize];
                        if seed == 0 {
                            continue;
                        }
                        let lo = ((b as u128 * l.n as u128) / l.buckets as u128) as u64;
                        let off = (l.value(h, seed) + l.n - lo) % l.n;
                        bins[((off * 16 / l.slice) as usize).min(15)] += 1;
                    }
                    let total: u64 = bins.iter().sum();
                    let mut acc = 0u64;
                    let row: Vec<String> = bins
                        .iter()
                        .map(|&c| {
                            acc += c;
                            format!("{:.3}", acc as f64 / total.max(1) as f64)
                        })
                        .collect();
                    println!(
                        "    offset cdf by sixteenth of the slice: {}",
                        row.join(" ")
                    );
                    // Where in a chunk the bumped buckets are: sixteenths of the run, then the
                    // last `gap` buckets of it.
                    let starts = chunk_starts(l.buckets, false);
                    let chunk = starts.get(1).copied().unwrap_or(l.buckets);
                    let gap = ((l.slice + 2) * l.buckets).div_ceil(l.n) + 1;
                    let mut all = [0u64; 17];
                    let mut lost = [0u64; 17];
                    for (b, &k) in size.iter().enumerate() {
                        let k0 = starts.partition_point(|&s| s <= b as u64) - 1;
                        let end = starts.get(k0 + 1).copied().unwrap_or(l.buckets);
                        let (p, len) = (b as u64 - starts[k0], end - starts[k0]);
                        let bin = if p >= len - gap {
                            16
                        } else {
                            (p * 16 / (len - gap)) as usize
                        };
                        all[bin] += u64::from(k);
                        if l.seeds[b] == 0 {
                            lost[bin] += u64::from(k);
                        }
                    }
                    let row: Vec<String> = (0..17)
                        .map(|i| format!("{:.1}", 100.0 * lost[i] as f64 / all[i].max(1) as f64))
                        .collect();
                    println!(
                        "    bumped % by sixteenth of the run, then the gap ({} chunks, first {chunk}, gap {gap}): {}",
                        starts.len(),
                        row.join(" ")
                    );
                }
            }
            println!(
                "{cfg:<28} bits {:>6.3} = seeds {:.3} + remap {:.3} (low bits {})  bumped [{}]  build {ns:>5.1} ns/key  id {id_ns:.2} ns",
                m.bits_per_key(),
                seeds as f64 * 8.0 / n as f64,
                (low + lines) as f64 * 8.0 / n as f64,
                v.remap.low_bits,
                eps.join(" "),
            );
        }
    }

    /// Rewrite the committed `MPH3` fixture after a deliberate format change. Nothing else may.
    #[test]
    #[ignore = "writes a fixture"]
    fn write_golden_mphf() {
        let blob = Mphf::build(&golden_hashes()).expect("build").to_bytes();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/golden-3.1.0-mphf.bin"
        );
        std::fs::write(path, &blob).expect("write the fixture");
        println!("{path}: {} bytes", blob.len());
    }

    /// Size and correctness at scale, small tables included, because the tail is a different
    /// construction and its cost shows only there.
    #[test]
    #[ignore = "measurement, not a test"]
    fn size_at_scale() {
        for n in [
            100usize, 1_000, 4_000, 5_000, 10_000, 100_000, 1_000_000, 10_000_000,
        ] {
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
                "n {n:>9}  bits/key {:>6.3}  (target <= 2.400)  {}  build {ms:>7.1} ms (loaded machine, not a timing claim)",
                m.bits_per_key(),
                shape(&m),
            );
        }
    }

    /// Peak resident memory of a build, which is the question 10 M cannot answer. Reported next
    /// to the size so a redesign has a number to beat.
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

    /// Size, build time and lookup on its own terms: the absolute cost, which is what a regression
    /// would move.
    #[test]
    #[ignore = "measurement, not a test"]
    fn cost_at_scale() {
        for n in [1_000_000usize, 10_000_000] {
            let hs = bigram_hashes(n);
            let mut build_ms = f64::INFINITY;
            let mut id_ns = f64::INFINITY;
            let mut bits = 0.0;
            for _ in 0..2 {
                let t = std::time::Instant::now();
                let own = Mphf::build(&hs).expect("build");
                build_ms = build_ms.min(t.elapsed().as_secs_f64() * 1e3);
                bits = own.bits_per_key();
                id_ns = id_ns.min(min_ns(
                    3,
                    &hs,
                    |m: &Mphf| hs.iter().map(|&h| m.index(h)).sum(),
                    &own,
                ));
            }
            println!(
                "n {n:>9}   bits/key {bits:>6.3}   build {build_ms:>8.0} ms   id {id_ns:>6.2} ns/key"
            );
        }
    }

    /// R-02: what partitioning the table would cost and buy. The keys are cut by their top
    /// `LEXINDEX_MPHF_PART_BITS` bits into parts, each built as its own `Mphf` after one multiply
    /// remixes the hash (a part's keys share their top bits, and the bucket is the top bits), and
    /// a lookup is `offsets[part] + part.index(h')`. Against the monolithic table, A-B-A-B over
    /// `LEXINDEX_MPHF_ROUNDS`: size, build on `LEXINDEX_MPHF_THREADS` (parts claimed from a
    /// counter, each built single-threaded), lookup over one shuffled probe order. The remix and
    /// the per-part sort are done before the clock starts, as the monolith's sort is.
    #[test]
    #[ignore = "measurement, not a test"]
    fn partitioned() {
        const REMIX: u64 = 0x9E37_79B9_7F4A_7C15;
        let n = env("LEXINDEX_MPHF_N", 10_000_000);
        let rounds = env("LEXINDEX_MPHF_ROUNDS", 3);
        let threads = env("LEXINDEX_MPHF_THREADS", 1);
        let part_bits = env("LEXINDEX_MPHF_PART_BITS", 6) as u32;
        let hs = bigram_hashes(n);
        let parts = 1usize << part_bits;
        let part_of = |h: u64| (h >> (64 - part_bits)) as usize;
        let bounds: Vec<usize> = (0..=parts)
            .map(|p| hs.partition_point(|&h| part_of(h) < p))
            .collect();
        let keys: Vec<Vec<u64>> = (0..parts)
            .map(|p| {
                let mut k: Vec<u64> = hs[bounds[p]..bounds[p + 1]]
                    .iter()
                    .map(|h| h.wrapping_mul(REMIX))
                    .collect();
                k.sort_unstable();
                k
            })
            .collect();
        let mut offsets = vec![0u64; parts + 1];
        for p in 0..parts {
            offsets[p + 1] = offsets[p] + keys[p].len() as u64;
        }
        let mut order: Vec<u32> = (0..n as u32).collect();
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        for i in (1..n).rev() {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            order.swap(i, (r % (i as u64 + 1)) as usize);
        }
        let build_parts = || -> Vec<Mphf> {
            let next = AtomicUsize::new(0);
            let built: Vec<Mutex<Option<Mphf>>> = (0..parts).map(|_| Mutex::new(None)).collect();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        loop {
                            let p = next.fetch_add(1, Ordering::Relaxed);
                            if p >= parts {
                                break;
                            }
                            let m = Mphf::build_with_threads(&keys[p], 1).expect("part");
                            *built[p].lock().unwrap() = Some(m);
                        }
                    });
                }
            });
            built
                .into_iter()
                .map(|m| m.into_inner().unwrap().unwrap())
                .collect()
        };
        let (mut mono_build, mut parts_build) = (f64::INFINITY, f64::INFINITY);
        let (mut mono_look, mut parts_look) = (f64::INFINITY, f64::INFINITY);
        let (mut mono_bits, mut parts_bits) = (0.0, 0.0);
        let mut shape_s = String::new();
        for _ in 0..rounds {
            let t = std::time::Instant::now();
            let mono = Mphf::build_with_threads(&hs, threads).expect("mono");
            mono_build = mono_build.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
            let t = std::time::Instant::now();
            let ps = build_parts();
            parts_build = parts_build.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
            mono_bits = mono.bits_per_key();
            let bytes: usize = ps.iter().map(Mphf::byte_len).sum::<usize>() + 8 * (parts + 1);
            parts_bits = bytes as f64 * 8.0 / n as f64;
            shape_s = shape(&mono);
            let mut seen = vec![false; n];
            for &h in &hs {
                let p = part_of(h);
                let id = (offsets[p] + ps[p].index(h.wrapping_mul(REMIX))) as usize;
                assert!(id < n && !seen[id], "parts: not a bijection");
                seen[id] = true;
            }
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let mut acc = 0u64;
                for &i in &order {
                    acc = acc.wrapping_add(mono.index(hs[i as usize]));
                }
                std::hint::black_box(acc);
                mono_look = mono_look.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
                let t = std::time::Instant::now();
                let mut acc = 0u64;
                for &i in &order {
                    let h = hs[i as usize];
                    let p = part_of(h);
                    acc = acc.wrapping_add(offsets[p] + ps[p].index(h.wrapping_mul(REMIX)));
                }
                std::hint::black_box(acc);
                parts_look = parts_look.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
            }
        }
        println!(
            "n {n} parts {parts} (~{} keys each) threads {threads} rounds {rounds}\n  mono   bits/key {mono_bits:>6.3}   build {mono_build:>6.1} ns/key   lookup {mono_look:>5.1} ns   {shape_s}\n  parts  bits/key {parts_bits:>6.3}   build {parts_build:>6.1} ns/key   lookup {parts_look:>5.1} ns",
            n / parts
        );
    }
}
