//! One process, A-B-A-B: lexindex's `Mphf` (the `bench-mphf` export) against `ph` 0.11.0's PHast
//! (`Function`, `SeedOnly`) and PHast+ (`Function2`, `ShiftOnlyWrapped`), both at 8-bit seeds and
//! bucket size 4.5, and `ptr_hash` 2.1.1's three parameter sets, over the same distinct
//! splitmix64 keys and the same shuffled probe order. Builds: the minimum over the rounds, and
//! the spread (the slowest round over the fastest, minus one). Lookups: every key once per pass
//! in the one shuffled order, three passes a round, the minimum and the spread over all passes;
//! and where a function has a batch or streaming form — lexindex `index_all`, `ptr_hash`
//! `index_stream` — that too, over the same order in chunks of 4096 keys; `ph` has none.
//! lexindex takes the keys as hashes; `ph` hashes each key with its default seeded hasher
//! (wyhash) at build and on every lookup level, `ptr_hash` with `FastIntHash` (one multiply).
//! Threads: lexindex and `ph` take the count directly, `ptr_hash` runs inside a rayon pool of
//! that size.
//!
//! `cd bench/mphf_vs && cargo run --release -- [n] [rounds] [threads]`
//!
//! `MPHF_VS_ROWS=lexindex,fast` runs only the rows whose name contains one of the substrings.
//! `MPHF_VS_NO_THP=1` turns transparent huge pages off for the process (`prctl`): the control
//! for lexindex's tables, which ask for them from 2 MiB up; no competitor's table asks.
//! `MPHF_VS_PROBES=m` probes m keys drawn at random (with replacement) instead of every key once:
//! at 1 B keys the probe order alone is another 8 GB beside a competitor's build peak.
use lexindex::Mphf;
use ph::phast::{
    Function, Function2, Params, SeedOnly, ShiftOnlyWrapped, bits_per_seed_to_100_bucket_size,
};
use ph::seeds::Bits8;
use ph::{BuildDefaultSeededHasher, GetSize};
use ptr_hash::hash::FastIntHash;
use ptr_hash::{CompactPtrHash, DefaultPtrHash, PtrHashParams};
use std::time::Instant;

const CHUNK: usize = 4096;

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

/// The probe order, and the key count the build time is per.
struct Probe<'a> {
    keys: usize,
    order: &'a [u64],
}

/// The fastest and the slowest of a series of timings, in ns per key.
#[derive(Clone, Copy)]
struct Stat {
    min: f64,
    max: f64,
}

impl Stat {
    const NONE: Self = Self {
        min: f64::INFINITY,
        max: 0.0,
    };

    fn add(&mut self, ns: f64) {
        self.min = self.min.min(ns);
        self.max = self.max.max(ns);
    }

    fn cell(self) -> String {
        if self.min.is_infinite() {
            return format!("{:>8} {:>7}", "-", "");
        }
        format!(
            "{:>8.1} {:>+6.1}%",
            self.min,
            (self.max / self.min - 1.0) * 100.0
        )
    }
}

struct Row {
    name: &'static str,
    run: bool,
    build: Stat,
    /// Peak resident growth over the build, in MB: the table and whatever the build held at
    /// its peak, above the keys and the probe order the process already holds.
    peak_mb: f64,
    bits: f64,
    lookup: Stat,
    batch: Stat,
    /// Wall time a key with `lookup_threads` threads each taking a share of the probe order.
    lookup_mt: Stat,
    batch_mt: Stat,
}

/// A row's batch lookup: the wrapping sum of the answers to a chunk, if it has one.
type Batch<'a, T> = Option<&'a (dyn Fn(&T, &[u64]) -> u64 + Sync)>;

/// A `/proc/self/status` field in kB: `VmRSS:` for the resident set now, `VmHWM:` for its
/// peak since the last reset.
fn status_kb(field: &str) -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    status
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse().ok())
        .expect("a VmRSS/VmHWM line")
}

/// Resets the process's peak resident set to its current one (`clear_refs` 5), so that the
/// next `VmHWM` reading is the peak from here on.
fn reset_high_water() {
    std::fs::write("/proc/self/clear_refs", "5\n").expect("/proc/self/clear_refs");
}

impl Row {
    fn new(name: &'static str, run: bool) -> Self {
        Self {
            name,
            run,
            build: Stat::NONE,
            peak_mb: 0.0,
            bits: 0.0,
            lookup: Stat::NONE,
            batch: Stat::NONE,
            lookup_mt: Stat::NONE,
            batch_mt: Stat::NONE,
        }
    }

    /// One round: time `build`, then take the size and the lookup times from what it produced.
    /// `get` answers one key, `batch` the wrapping sum of the answers to a chunk.
    fn round<T: Sync>(
        &mut self,
        probe: &Probe<'_>,
        lookup_threads: usize,
        build: impl FnOnce() -> T,
        bits: impl Fn(&T) -> f64,
        get: impl Fn(&T, u64) -> u64 + Sync,
        batch: Batch<'_, T>,
    ) {
        if !self.run {
            return;
        }
        let (keys, probe) = (probe.keys, probe.order);
        let n = probe.len() as f64;
        reset_high_water();
        let before = status_kb("VmRSS:");
        let t = Instant::now();
        let f = build();
        self.build
            .add(t.elapsed().as_secs_f64() * 1e9 / keys as f64);
        self.peak_mb = self.peak_mb.max((status_kb("VmHWM:") - before) / 1024.0);
        self.bits = bits(&f);
        for _ in 0..3 {
            let t = Instant::now();
            let mut acc = 0u64;
            for &h in probe {
                acc = acc.wrapping_add(get(&f, h));
            }
            std::hint::black_box(acc);
            self.lookup.add(t.elapsed().as_secs_f64() * 1e9 / n);
            if let Some(batch) = batch {
                let t = Instant::now();
                let mut acc = 0u64;
                for chunk in probe.chunks(CHUNK) {
                    acc = acc.wrapping_add(batch(&f, chunk));
                }
                std::hint::black_box(acc);
                self.batch.add(t.elapsed().as_secs_f64() * 1e9 / n);
            }
        }
        if lookup_threads < 2 {
            return;
        }
        let share = probe.len().div_ceil(lookup_threads);
        let (f, get) = (&f, &get);
        for _ in 0..3 {
            let t = Instant::now();
            std::thread::scope(|scope| {
                for part in probe.chunks(share) {
                    scope.spawn(move || {
                        let mut acc = 0u64;
                        for &h in part {
                            acc = acc.wrapping_add(get(f, h));
                        }
                        std::hint::black_box(acc);
                    });
                }
            });
            self.lookup_mt.add(t.elapsed().as_secs_f64() * 1e9 / n);
            if let Some(batch) = batch {
                let t = Instant::now();
                std::thread::scope(|scope| {
                    for part in probe.chunks(share) {
                        scope.spawn(move || {
                            let mut acc = 0u64;
                            for chunk in part.chunks(CHUNK) {
                                acc = acc.wrapping_add(batch(f, chunk));
                            }
                            std::hint::black_box(acc);
                        });
                    }
                });
                self.batch_mt.add(t.elapsed().as_secs_f64() * 1e9 / n);
            }
        }
    }
}

/// Transparent huge pages off for this process, whatever a table asks for.
fn disable_thp() {
    unsafe extern "C" {
        fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    }
    const PR_SET_THP_DISABLE: i32 = 41;
    // SAFETY: a process-wide flag; no memory is touched.
    let rc = unsafe { prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) };
    assert_eq!(rc, 0, "prctl(PR_SET_THP_DISABLE) failed");
}

fn main() {
    let arg = |i: usize, default: usize| {
        std::env::args()
            .nth(i)
            .and_then(|a| a.parse().ok())
            .unwrap_or(default)
    };
    let (n, rounds, threads) = (arg(1, 10_000_000), arg(2, 3), arg(3, 1));
    let only: Vec<String> = std::env::var("MPHF_VS_ROWS")
        .map(|s| s.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let wanted = |name: &str| only.is_empty() || only.iter().any(|f| name.contains(f.as_str()));
    if std::env::var_os("MPHF_VS_NO_THP").is_some() {
        disable_thp();
    }
    let lookup_threads: usize = std::env::var("MPHF_VS_LOOKUP_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let mut keys: Vec<u64> = (0..n as u64).map(splitmix).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), n, "splitmix64 collided");
    let probes: usize = std::env::var("MPHF_VS_PROBES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(n);
    let order: Vec<u64> = if probes < n {
        let mut r = 0x2545_F491_4F6C_DD1Du64;
        (0..probes)
            .map(|_| {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                keys[(r % n as u64) as usize]
            })
            .collect()
    } else {
        shuffled(n).into_iter().map(|i| keys[i as usize]).collect()
    };
    let probe = Probe {
        keys: n,
        order: &order,
    };
    let params = Params::new(Bits8, bits_per_seed_to_100_bucket_size(8));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    let mut rows = [
        Row::new("lexindex MPH3", wanted("lexindex MPH3")),
        Row::new(
            "ph 0.11 PHast+ (ShiftOnlyWrapped)",
            wanted("ph 0.11 PHast+ (ShiftOnlyWrapped)"),
        ),
        Row::new(
            "ph 0.11 PHast (SeedOnly)",
            wanted("ph 0.11 PHast (SeedOnly)"),
        ),
        Row::new("ptr_hash 2.1.1 compact", wanted("ptr_hash 2.1.1 compact")),
        Row::new("ptr_hash 2.1.1 balanced", wanted("ptr_hash 2.1.1 balanced")),
        Row::new("ptr_hash 2.1.1 fast", wanted("ptr_hash 2.1.1 fast")),
    ];
    for _ in 0..rounds {
        rows[0].round(
            &probe,
            lookup_threads,
            || Mphf::build_with_threads(&keys, threads).expect("build"),
            |m| m.byte_len() as f64 * 8.0 / n as f64,
            |m, h| m.index(h),
            Some(&|m: &Mphf, chunk: &[u64]| {
                m.index_all(chunk).into_iter().fold(0, u64::wrapping_add)
            }),
        );
        rows[1].round(
            &probe,
            lookup_threads,
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
            |f, h| f.get(&h) as u64,
            None,
        );
        rows[2].round(
            &probe,
            lookup_threads,
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
            |f, h| f.get(&h) as u64,
            None,
        );
        rows[3].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    CompactPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_compact())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&k) as u64,
            Some(&|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[4].round(
            &probe,
            lookup_threads,
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
            |h, k| h.index(&k) as u64,
            Some(&|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[5].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    DefaultPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_fast())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&k) as u64,
            Some(&|h: &DefaultPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
    }
    let sample = if probes < n {
        format!(" of {probes} keys drawn at random")
    } else {
        String::new()
    };
    println!(
        "n {n}, {threads} thread(s), builds min of {rounds} rounds, lookups over one shuffled probe order{sample}, min of {} passes; spread = slowest over fastest; batch = index_all / index_stream in chunks of {CHUNK}{}",
        3 * rounds,
        if lookup_threads > 1 {
            format!(
                "; x{lookup_threads} = wall ns a key with {lookup_threads} lookup threads over shares of the probe order"
            )
        } else {
            String::new()
        }
    );
    let single = true;
    println!(
        "{:<36} {:>9} {:>16} {:>16} {:>16}{}{}",
        "",
        "bits/key",
        "build ns/key",
        "lookup ns",
        "batch ns",
        if lookup_threads > 1 {
            format!(
                "{:>17} {:>16}",
                format!("lookup x{lookup_threads}"),
                format!("batch x{lookup_threads}")
            )
        } else {
            String::new()
        },
        if single {
            format!("{:>14}", "build peak MB")
        } else {
            String::new()
        }
    );
    for r in rows.iter().filter(|r| r.run) {
        println!(
            "{:<36} {:>9.3} {} {} {}{}{}",
            r.name,
            r.bits,
            r.build.cell(),
            r.lookup.cell(),
            r.batch.cell(),
            if lookup_threads > 1 {
                format!(" {} {}", r.lookup_mt.cell(), r.batch_mt.cell())
            } else {
                String::new()
            },
            if single {
                format!("{:>14.1}", r.peak_mb)
            } else {
                String::new()
            }
        );
    }
}
