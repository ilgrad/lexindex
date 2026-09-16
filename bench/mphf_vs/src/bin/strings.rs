//! End to end, `&str -> id`: what a user measures. `ClosedHashIndex` (the key hash plus the
//! perfect hash, nothing else) against PtrHash twins over the same `hash_key_bytes` values — so
//! that difference is the MPHF and the index's framing — and against PtrHash hashing the bytes
//! itself with xxh3, which is what a PtrHash user gets; `CompactHashIndex` and the bare `Mphf`
//! for scale. One corpus file of one key a line; the probe is every key in a shuffled order,
//! read from the corpus's own storage, so each lookup pays the same read of its key.
//!
//! `strings <corpus> [rounds] [threads]`; `MPHF_VS_ROWS`, `MPHF_VS_LOOKUP_THREADS` as in
//! `mphf_vs`.
use std::time::Instant;

use lexindex::{ClosedHashIndex, CompactHashIndex, Mphf, hash_key_bytes};
use ptr_hash::hash::{FastIntHash, Xxh3};
use ptr_hash::{CompactPtrHash, DefaultPtrHash, PtrHashParams};

const CHUNK: usize = 4096;

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

type Batch<'a, T> = Option<&'a (dyn Fn(&T, &[&str]) -> u64 + Sync)>;

struct Row {
    name: &'static str,
    run: bool,
    build: Stat,
    bits: f64,
    lookup: Stat,
    batch: Stat,
    lookup_mt: Stat,
    batch_mt: Stat,
}

impl Row {
    fn new(name: &'static str, run: bool) -> Self {
        Self {
            name,
            run,
            build: Stat::NONE,
            bits: 0.0,
            lookup: Stat::NONE,
            batch: Stat::NONE,
            lookup_mt: Stat::NONE,
            batch_mt: Stat::NONE,
        }
    }

    fn round<T: Sync>(
        &mut self,
        probe: &[&str],
        lookup_threads: usize,
        build: impl FnOnce() -> T,
        bits: impl Fn(&T) -> f64,
        get: impl Fn(&T, &str) -> u64 + Sync,
        batch: Batch<'_, T>,
    ) {
        if !self.run {
            return;
        }
        let n = probe.len() as f64;
        let t = Instant::now();
        let f = build();
        self.build.add(t.elapsed().as_secs_f64() * 1e9 / n);
        self.bits = bits(&f);
        for _ in 0..3 {
            let t = Instant::now();
            let mut acc = 0u64;
            for &k in probe {
                acc = acc.wrapping_add(get(&f, k));
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
                        for &k in part {
                            acc = acc.wrapping_add(get(f, k));
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

fn hashes(keys: &[&str]) -> Vec<u64> {
    keys.iter().map(|k| hash_key_bytes(k.as_bytes())).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .expect("usage: strings <corpus> [rounds] [threads]");
    let arg =
        |i: usize, default: usize| args.get(i).and_then(|a| a.parse().ok()).unwrap_or(default);
    let (rounds, threads) = (arg(2, 3), arg(3, 1));
    let only: Vec<String> = std::env::var("MPHF_VS_ROWS")
        .map(|s| s.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let wanted = |name: &str| only.is_empty() || only.iter().any(|f| name.contains(f.as_str()));
    let lookup_threads: usize = std::env::var("MPHF_VS_LOOKUP_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let text = std::fs::read_to_string(path).expect("a corpus of one key a line");
    let mut keys: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    keys.sort_unstable();
    keys.dedup();
    let n = keys.len();
    let bytes: Vec<&[u8]> = keys.iter().map(|k| k.as_bytes()).collect();
    let mean = bytes.iter().map(|b| b.len()).sum::<usize>() as f64 / n as f64;
    let probe: Vec<&str> = shuffled(n).into_iter().map(|i| keys[i as usize]).collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    let mut rows = [
        Row::new(
            "lexindex ClosedHashIndex",
            wanted("lexindex ClosedHashIndex"),
        ),
        Row::new(
            "lexindex Mphf + hash_key_bytes",
            wanted("lexindex Mphf + hash_key_bytes"),
        ),
        Row::new(
            "ptr_hash 2.1.1 fast, lexindex's hash",
            wanted("ptr_hash 2.1.1 fast, lexindex's hash"),
        ),
        Row::new(
            "ptr_hash 2.1.1 compact, lexindex's hash",
            wanted("ptr_hash 2.1.1 compact, lexindex's hash"),
        ),
        Row::new(
            "ptr_hash 2.1.1 fast, xxh3 over the bytes",
            wanted("ptr_hash 2.1.1 fast, xxh3 over the bytes"),
        ),
        Row::new(
            "ptr_hash 2.1.1 compact, xxh3 over the bytes",
            wanted("ptr_hash 2.1.1 compact, xxh3 over the bytes"),
        ),
        Row::new(
            "lexindex CompactHashIndex fp=1",
            wanted("lexindex CompactHashIndex fp=1"),
        ),
    ];
    for _ in 0..rounds {
        rows[0].round(
            &probe,
            lookup_threads,
            || ClosedHashIndex::build(keys.iter().copied()).expect("build"),
            |z| z.serialized_len() as f64 * 8.0 / n as f64,
            |z, k| u64::from(z.id(k)),
            Some(&|z: &ClosedHashIndex, chunk: &[&str]| {
                z.ids_of(chunk)
                    .into_iter()
                    .map(u64::from)
                    .fold(0, u64::wrapping_add)
            }),
        );
        rows[1].round(
            &probe,
            lookup_threads,
            || Mphf::build_with_threads(&hashes(&keys), threads).expect("build"),
            |m| m.byte_len() as f64 * 8.0 / n as f64,
            |m, k| m.index(hash_key_bytes(k.as_bytes())),
            Some(&|m: &Mphf, chunk: &[&str]| {
                m.index_all(&hashes(chunk))
                    .into_iter()
                    .fold(0, u64::wrapping_add)
            }),
        );
        rows[2].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    DefaultPtrHash::<FastIntHash, u64>::new(
                        &hashes(&keys),
                        PtrHashParams::default_fast(),
                    )
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&hash_key_bytes(k.as_bytes())) as u64,
            Some(&|h: &DefaultPtrHash<FastIntHash, u64>, chunk: &[&str]| {
                let hs = hashes(chunk);
                h.index_stream::<32, _>(hs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[3].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    CompactPtrHash::<FastIntHash, u64>::new(
                        &hashes(&keys),
                        PtrHashParams::default_compact(),
                    )
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&hash_key_bytes(k.as_bytes())) as u64,
            Some(&|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[&str]| {
                let hs = hashes(chunk);
                h.index_stream::<32, _>(hs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[4].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    DefaultPtrHash::<Xxh3, &[u8]>::new(&bytes, PtrHashParams::default_fast())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&k.as_bytes()) as u64,
            Some(&|h: &DefaultPtrHash<Xxh3, &[u8]>, chunk: &[&str]| {
                let bs: Vec<&[u8]> = chunk.iter().map(|k| k.as_bytes()).collect();
                h.index_stream::<32, _>(bs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[5].round(
            &probe,
            lookup_threads,
            || {
                pool.install(|| {
                    CompactPtrHash::<Xxh3, &[u8]>::new(&bytes, PtrHashParams::default_compact())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            |h, k| h.index(&k.as_bytes()) as u64,
            Some(&|h: &CompactPtrHash<Xxh3, &[u8]>, chunk: &[&str]| {
                let bs: Vec<&[u8]> = chunk.iter().map(|k| k.as_bytes()).collect();
                h.index_stream::<32, _>(bs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[6].round(
            &probe,
            lookup_threads,
            || CompactHashIndex::build(keys.iter().copied(), 1).expect("build"),
            |c| c.serialized_len().expect("len") as f64 * 8.0 / n as f64,
            |c, k| u64::from(c.id_unchecked(k)),
            Some(&|c: &CompactHashIndex, chunk: &[&str]| {
                c.ids_of(chunk)
                    .into_iter()
                    .map(|id| u64::from(id.unwrap_or(0)))
                    .fold(0, u64::wrapping_add)
            }),
        );
    }
    println!(
        "{path}: {n} keys, mean {mean:.1} bytes; {threads} thread(s) for the PtrHash and Mphf builds (the indexes use their own); builds min of {rounds} rounds; lookups over every key in one shuffled order, the key read from the corpus, min of {} passes; spread = slowest over fastest; batch = ids_of / index_all / index_stream in chunks of {CHUNK}{}",
        3 * rounds,
        if lookup_threads > 1 {
            format!(
                "; x{lookup_threads} = wall ns a key with {lookup_threads} lookup threads over shares of the probe order"
            )
        } else {
            String::new()
        }
    );
    println!(
        "{:<44} {:>9} {:>16} {:>16} {:>16}{}",
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
        }
    );
    for r in rows.iter().filter(|r| r.run) {
        println!(
            "{:<44} {:>9.3} {} {} {}{}",
            r.name,
            r.bits,
            r.build.cell(),
            r.lookup.cell(),
            r.batch.cell(),
            if lookup_threads > 1 {
                format!(" {} {}", r.lookup_mt.cell(), r.batch_mt.cell())
            } else {
                String::new()
            }
        );
    }
}
