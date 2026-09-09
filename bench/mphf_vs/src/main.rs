//! One process, A-B-A-B: lexindex's `Mphf` (the `bench-mphf` export) against `ph` 0.11.0's PHast
//! (`Function`, `SeedOnly`) and PHast+ (`Function2`, `ShiftOnlyWrapped`), both at 8-bit seeds and
//! bucket size 4.5, and `ptr_hash` 2.1.1's three parameter sets, over the same distinct
//! splitmix64 keys and the same shuffled probe order. Builds are the minimum over the rounds,
//! lookups the minimum of three passes. lexindex takes the keys as hashes; `ph` hashes each key
//! with its default seeded hasher (wyhash) at build and on every lookup level, `ptr_hash` with
//! `FastIntHash` (one multiply). Threads: lexindex and `ph` take the count directly, `ptr_hash`
//! runs inside a rayon pool of that size.
//!
//! `cd bench/mphf_vs && cargo run --release -- [n] [rounds] [threads]`
use lexindex::Mphf;
use ph::phast::{
    Function, Function2, Params, SeedOnly, ShiftOnlyWrapped, bits_per_seed_to_100_bucket_size,
};
use ph::seeds::Bits8;
use ph::{BuildDefaultSeededHasher, GetSize};
use ptr_hash::hash::FastIntHash;
use ptr_hash::{CompactPtrHash, DefaultPtrHash, PtrHashParams};
use std::time::Instant;

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn shuffled(n: usize) -> Vec<u32> {
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut r = 0x2545_F491_4F6C_DD1Du64;
    for i in (1..n).rev() {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        order.swap(i, (r % (i as u64 + 1)) as usize);
    }
    order
}

struct Row {
    name: &'static str,
    build: f64,
    bits: f64,
    lookup: f64,
}

impl Row {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            build: f64::INFINITY,
            bits: 0.0,
            lookup: f64::INFINITY,
        }
    }

    /// One round: time `build`, then take the size and the lookup time from what it produced.
    fn round<T>(
        &mut self,
        n: usize,
        order: &[u32],
        build: impl FnOnce() -> T,
        bits: impl Fn(&T) -> f64,
        get: impl Fn(&T, u32) -> u64,
    ) {
        let t = Instant::now();
        let f = build();
        self.build = self.build.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
        self.bits = bits(&f);
        for _ in 0..3 {
            let t = Instant::now();
            let mut acc = 0u64;
            for &i in order {
                acc = acc.wrapping_add(get(&f, i));
            }
            std::hint::black_box(acc);
            self.lookup = self.lookup.min(t.elapsed().as_secs_f64() * 1e9 / n as f64);
        }
    }
}

fn main() {
    let arg = |i: usize, default: usize| {
        std::env::args()
            .nth(i)
            .and_then(|a| a.parse().ok())
            .unwrap_or(default)
    };
    let (n, rounds, threads) = (arg(1, 10_000_000), arg(2, 3), arg(3, 1));
    let mut keys: Vec<u64> = (0..n as u64).map(splitmix).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), n, "splitmix64 collided");
    let order = shuffled(n);
    let params = Params::new(Bits8, bits_per_seed_to_100_bucket_size(8));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    let mut rows = [
        Row::new("lexindex MPH2"),
        Row::new("ph 0.11 PHast+ (ShiftOnlyWrapped)"),
        Row::new("ph 0.11 PHast (SeedOnly)"),
        Row::new("ptr_hash 2.1.1 compact"),
        Row::new("ptr_hash 2.1.1 balanced"),
        Row::new("ptr_hash 2.1.1 fast"),
    ];
    for _ in 0..rounds {
        rows[0].round(
            n,
            &order,
            || Mphf::build_with_threads(&keys, threads).expect("build"),
            |m| m.byte_len() as f64 * 8.0 / n as f64,
            |m, i| m.index(keys[i as usize]),
        );
        rows[1].round(
            n,
            &order,
            || -> Function2<Bits8, ShiftOnlyWrapped> {
                Function2::with_slice_p_threads_hash_sc(
                    &keys,
                    &params,
                    threads,
                    BuildDefaultSeededHasher::default(),
                    ShiftOnlyWrapped,
                )
            },
            |f| f.size_bytes() as f64 * 8.0 / n as f64,
            |f, i| f.get(&keys[i as usize]) as u64,
        );
        rows[2].round(
            n,
            &order,
            || -> Function<Bits8, SeedOnly> {
                Function::with_slice_p_threads_hash_sc(
                    &keys,
                    &params,
                    threads,
                    BuildDefaultSeededHasher::default(),
                    SeedOnly,
                )
            },
            |f| f.size_bytes() as f64 * 8.0 / n as f64,
            |f, i| f.get(&keys[i as usize]) as u64,
        );
        rows[3].round(
            n,
            &order,
            || {
                pool.install(|| {
                    CompactPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_compact())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, i| h.index(&keys[i as usize]) as u64,
        );
        rows[4].round(
            n,
            &order,
            || {
                pool.install(|| {
                    CompactPtrHash::<FastIntHash, u64>::new(
                        &keys,
                        PtrHashParams::default_balanced(),
                    )
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, i| h.index(&keys[i as usize]) as u64,
        );
        rows[5].round(
            n,
            &order,
            || {
                pool.install(|| {
                    DefaultPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_fast())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, i| h.index(&keys[i as usize]) as u64,
        );
    }
    println!(
        "n {n}, {threads} thread(s), builds min of {rounds}, lookups over a shuffled probe order, min of 3"
    );
    println!(
        "{:<36} {:>9} {:>14} {:>12}",
        "", "bits/key", "build ns/key", "lookup ns"
    );
    for r in &rows {
        println!(
            "{:<36} {:>9.3} {:>14.1} {:>12.1}",
            r.name, r.bits, r.build, r.lookup
        );
    }
}
