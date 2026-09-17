//! End to end, `&str -> id`: what a user measures. `ClosedHashIndex` (the key hash plus the
//! perfect hash, nothing else) against PtrHash twins over the same `hash_key_bytes` values — so
//! that difference is the MPHF and the index's framing — and against PtrHash hashing the bytes
//! itself with xxh3, which is what a PtrHash user gets; `CompactHashIndex` and the bare `Mphf`
//! for scale. One corpus file of one key a line; the probe is every key in a shuffled order,
//! read from the corpus's own storage, so each lookup pays the same read of its key.
//! A round builds every row first and then times the lookups with the rows in turn within each
//! pass, so that a machine warming through the round slows every row alike; each turn starts
//! with an untimed sweep over the first 2^20 probe keys, which brings back into the cache what
//! the previous row's turn took out of it.
//!
//! `strings <corpus> [rounds] [threads]`; `MPHF_VS_ROWS`, `MPHF_VS_LOOKUP_THREADS` as in
//! `mphf_vs`. `MPHF_VS_COPIES=k` looks each function up through k copies of its table, made one
//! after another after its build, and reports each lookup column as the median over the copies of
//! a copy's fastest pass, the spread then the slowest copy over the fastest: where a table lands
//! moves a lookup that misses the cache, and one table a row is one draw of it. Every copy is
//! checked to answer the first 2^20 probe keys as the table built does, but for PtrHash compact's:
//! `CompactPtrHash` is not `Clone`, so those are built again, and a build does not promise the
//! same table twice.
use std::sync::Arc;
use std::time::Instant;

use lexindex::{ClosedHashIndex, CompactHashIndex, Mphf, hash_key_bytes};
use ptr_hash::hash::{FastIntHash, Xxh3};
use ptr_hash::{CompactPtrHash, DefaultPtrHash, PtrHashParams};

const CHUNK: usize = 4096;

/// Probe keys swept, untimed, before each row's turn.
const WARM: usize = 1 << 20;

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

/// A lookup column over the copies of a table: with one copy its fastest pass and the spread of
/// its passes; with more, the median of the copies' fastest passes and the slowest of those over
/// the fastest.
fn column(copies: &[Stat]) -> String {
    let mut fastest: Vec<f64> = copies
        .iter()
        .map(|s| s.min)
        .filter(|m| m.is_finite())
        .collect();
    if copies.len() < 2 || fastest.is_empty() {
        return copies[0].cell();
    }
    fastest.sort_by(f64::total_cmp);
    format!(
        "{:>8.1} {:>+6.1}%",
        fastest[(fastest.len() - 1) / 2],
        (fastest[fastest.len() - 1] / fastest[0] - 1.0) * 100.0
    )
}

/// A pass over a slice of the probe order: the wrapping sum of the answers.
type Pass<'a> = Box<dyn Fn(&[&str]) -> u64 + Sync + 'a>;

/// One copy of a built function: a pass of single lookups, and one of batch lookups where the
/// function has them — each pass the function's own loop.
struct Table<'a> {
    row: usize,
    copy: usize,
    /// Whether the copy must answer every key as the row's first table does.
    exact: bool,
    single: Pass<'a>,
    batch: Option<Pass<'a>>,
}

/// How a row's tables after the first are made.
enum Copies<'a, T> {
    /// From the table built: the same function.
    Of(&'a dyn Fn(&T) -> T),
    /// Built again over the same keys. `same` says whether that build gives the same table, so
    /// the copy must answer every key as the first does: lexindex's builds are deterministic,
    /// `ptr_hash`'s at scale is not.
    Rebuilt {
        again: &'a dyn Fn() -> T,
        same: bool,
    },
}

/// One pass of single lookups over `probe`, out of line: each function's lookup is inlined into
/// a loop of its own rather than into the round beside everything else in it.
#[inline(never)]
fn sweep<T>(f: &T, probe: &[&str], get: &impl Fn(&T, &str) -> u64) -> u64 {
    let mut acc = 0u64;
    for &k in probe {
        acc = acc.wrapping_add(get(f, k));
    }
    acc
}

#[inline(never)]
fn sweep_batch<T>(f: &T, probe: &[&str], batch: fn(&T, &[&str]) -> u64) -> u64 {
    let mut acc = 0u64;
    for chunk in probe.chunks(CHUNK) {
        acc = acc.wrapping_add(batch(f, chunk));
    }
    acc
}

struct Row {
    name: &'static str,
    run: bool,
    build: Stat,
    bits: f64,
    /// A column a copy of the table.
    lookup: Vec<Stat>,
    batch: Vec<Stat>,
    /// Wall time a key with `lookup_threads` threads each taking a share of the probe order.
    lookup_mt: Vec<Stat>,
    batch_mt: Vec<Stat>,
}

impl Row {
    fn new(name: &'static str, run: bool, copies: usize) -> Self {
        Self {
            name,
            run,
            build: Stat::NONE,
            bits: 0.0,
            lookup: vec![Stat::NONE; copies],
            batch: vec![Stat::NONE; copies],
            lookup_mt: vec![Stat::NONE; copies],
            batch_mt: vec![Stat::NONE; copies],
        }
    }

    /// Times `build` over `keys` keys, takes the size from what it produced, and adds its
    /// copies to `tables`: the table built and one fewer than the row has columns made as
    /// `copies` says, one after another. `get` answers one key, `batch` the wrapping sum of the
    /// answers to a chunk.
    #[allow(clippy::too_many_arguments)]
    fn build<'a, T: Send + Sync + 'a>(
        &mut self,
        row: usize,
        keys: usize,
        tables: &mut Vec<Table<'a>>,
        build: impl FnOnce() -> T,
        bits: impl Fn(&T) -> f64,
        copies: Copies<'_, T>,
        get: impl Fn(&T, &str) -> u64 + Copy + Send + Sync + 'a,
        batch: Option<fn(&T, &[&str]) -> u64>,
    ) {
        if !self.run {
            return;
        }
        let t = Instant::now();
        let f = build();
        self.build
            .add(t.elapsed().as_secs_f64() * 1e9 / keys as f64);
        self.bits = bits(&f);
        let exact = match &copies {
            Copies::Of(_) => true,
            Copies::Rebuilt { same, .. } => *same,
        };
        let more: Vec<T> = (1..self.lookup.len())
            .map(|_| match &copies {
                Copies::Of(copy) => copy(&f),
                Copies::Rebuilt { again, .. } => again(),
            })
            .collect();
        for (c, f) in std::iter::once(f).chain(more).enumerate() {
            let f = Arc::new(f);
            let g = Arc::clone(&f);
            tables.push(Table {
                row,
                copy: c,
                exact,
                single: Box::new(move |probe| sweep(&*f, probe, &get)),
                batch: batch.map(|b| {
                    Box::new(move |probe: &[&str]| sweep_batch(&*g, probe, b)) as Pass<'a>
                }),
            });
        }
    }
}

/// Times every table's lookups over `probe`, the tables in turn within each of three passes:
/// single and batch lookups on one thread, then as many passes with `threads` threads, each
/// taking a share of the probe order. Every turn starts with an untimed sweep over the first
/// [`WARM`] probe keys, whose answers an exact copy must agree with the row's first table on.
fn lookups(rows: &mut [Row], tables: &[Table<'_>], probe: &[&str], threads: usize) {
    let n = probe.len() as f64;
    let warm = &probe[..probe.len().min(WARM)];
    let ns = |t: Instant| t.elapsed().as_secs_f64() * 1e9 / n;
    let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
    let mut answers = vec![None; rows.len()];
    let mut warm_up = |t: &Table<'_>| {
        let sum = (t.single)(warm);
        let first = *answers[t.row].get_or_insert(sum);
        assert!(
            !t.exact || sum == first,
            "copy {} of {} answers differently from copy 0",
            t.copy,
            names[t.row]
        );
    };
    for _ in 0..3 {
        for t in tables {
            warm_up(t);
            let s = Instant::now();
            std::hint::black_box((t.single)(probe));
            rows[t.row].lookup[t.copy].add(ns(s));
            if let Some(batch) = &t.batch {
                let s = Instant::now();
                std::hint::black_box(batch(probe));
                rows[t.row].batch[t.copy].add(ns(s));
            }
        }
    }
    if threads < 2 {
        return;
    }
    let share = probe.len().div_ceil(threads);
    for _ in 0..3 {
        for t in tables {
            warm_up(t);
            let s = Instant::now();
            std::thread::scope(|scope| {
                for part in probe.chunks(share) {
                    scope.spawn(move || std::hint::black_box((t.single)(part)));
                }
            });
            rows[t.row].lookup_mt[t.copy].add(ns(s));
            if let Some(batch) = &t.batch {
                let s = Instant::now();
                std::thread::scope(|scope| {
                    for part in probe.chunks(share) {
                        scope.spawn(move || std::hint::black_box(batch(part)));
                    }
                });
                rows[t.row].batch_mt[t.copy].add(ns(s));
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
    let copies: usize = std::env::var("MPHF_VS_COPIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
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
    let names = [
        "lexindex ClosedHashIndex",
        "lexindex Mphf + hash_key_bytes",
        "ptr_hash 2.1.1 fast, lexindex's hash",
        "ptr_hash 2.1.1 compact, lexindex's hash",
        "ptr_hash 2.1.1 fast, xxh3 over the bytes",
        "ptr_hash 2.1.1 compact, xxh3 over the bytes",
        "lexindex CompactHashIndex fp=1",
    ];
    let mut rows = names.map(|name| Row::new(name, wanted(name), copies));
    let closed = || ClosedHashIndex::build(keys.iter().copied()).expect("build");
    let compact_index = || CompactHashIndex::build(keys.iter().copied(), 1).expect("build");
    // `CompactPtrHash` is not `Clone` (its `EliasFano` remap is not), so its copies are built
    // again, and `ptr_hash` does not promise the same table twice.
    let compact_int = || {
        pool.install(|| {
            CompactPtrHash::<FastIntHash, u64>::new(
                &hashes(&keys),
                PtrHashParams::default_compact(),
            )
        })
    };
    let compact_xxh3 = || {
        pool.install(|| {
            CompactPtrHash::<Xxh3, &[u8]>::new(&bytes, PtrHashParams::default_compact())
        })
    };
    for _ in 0..rounds {
        let mut tables = Vec::new();
        rows[0].build(
            0,
            n,
            &mut tables,
            closed,
            |z| z.serialized_len() as f64 * 8.0 / n as f64,
            Copies::Rebuilt {
                again: &closed,
                same: true,
            },
            |z, k| u64::from(z.id(k)),
            Some(|z: &ClosedHashIndex, chunk: &[&str]| {
                z.ids_of(chunk)
                    .into_iter()
                    .map(u64::from)
                    .fold(0, u64::wrapping_add)
            }),
        );
        rows[1].build(
            1,
            n,
            &mut tables,
            || Mphf::build_with_threads(&hashes(&keys), threads).expect("build"),
            |m| m.byte_len() as f64 * 8.0 / n as f64,
            Copies::Of(&Mphf::clone),
            |m, k| m.index(hash_key_bytes(k.as_bytes())),
            Some(|m: &Mphf, chunk: &[&str]| {
                m.index_all(&hashes(chunk))
                    .into_iter()
                    .fold(0, u64::wrapping_add)
            }),
        );
        rows[2].build(
            2,
            n,
            &mut tables,
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
            Copies::Of(&DefaultPtrHash::clone),
            |h, k| h.index(&hash_key_bytes(k.as_bytes())) as u64,
            Some(|h: &DefaultPtrHash<FastIntHash, u64>, chunk: &[&str]| {
                let hs = hashes(chunk);
                h.index_stream::<32, _>(hs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[3].build(
            3,
            n,
            &mut tables,
            compact_int,
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Rebuilt {
                again: &compact_int,
                same: false,
            },
            |h, k| h.index(&hash_key_bytes(k.as_bytes())) as u64,
            Some(|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[&str]| {
                let hs = hashes(chunk);
                h.index_stream::<32, _>(hs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[4].build(
            4,
            n,
            &mut tables,
            || {
                pool.install(|| {
                    DefaultPtrHash::<Xxh3, &[u8]>::new(&bytes, PtrHashParams::default_fast())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Of(&DefaultPtrHash::clone),
            |h, k| h.index(&k.as_bytes()) as u64,
            Some(|h: &DefaultPtrHash<Xxh3, &[u8]>, chunk: &[&str]| {
                let bs: Vec<&[u8]> = chunk.iter().map(|k| k.as_bytes()).collect();
                h.index_stream::<32, _>(bs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[5].build(
            5,
            n,
            &mut tables,
            compact_xxh3,
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Rebuilt {
                again: &compact_xxh3,
                same: false,
            },
            |h, k| h.index(&k.as_bytes()) as u64,
            Some(|h: &CompactPtrHash<Xxh3, &[u8]>, chunk: &[&str]| {
                let bs: Vec<&[u8]> = chunk.iter().map(|k| k.as_bytes()).collect();
                h.index_stream::<32, _>(bs.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[6].build(
            6,
            n,
            &mut tables,
            compact_index,
            |c| c.serialized_len().expect("len") as f64 * 8.0 / n as f64,
            Copies::Rebuilt {
                again: &compact_index,
                same: true,
            },
            |c, k| u64::from(c.id_unchecked(k)),
            Some(|c: &CompactHashIndex, chunk: &[&str]| {
                c.ids_of(chunk)
                    .into_iter()
                    .map(|id| u64::from(id.unwrap_or(0)))
                    .fold(0, u64::wrapping_add)
            }),
        );
        lookups(&mut rows, &tables, &probe, lookup_threads);
    }
    println!(
        "{path}: {n} keys, mean {mean:.1} bytes; {threads} thread(s) for the PtrHash and Mphf builds (the indexes use their own); builds min of {rounds} rounds; lookups over every key in one shuffled order, the key read from the corpus, min of {} passes, the rows in turn; spread = slowest over fastest; batch = ids_of / index_all / index_stream in chunks of {CHUNK}{}{}",
        3 * rounds,
        if copies > 1 {
            format!(
                "; each lookup column the median over {copies} copies of a table of the copy's fastest pass, spread = slowest copy over fastest; the ptr_hash compact rows' copies built again"
            )
        } else {
            String::new()
        },
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
            column(&r.lookup),
            column(&r.batch),
            if lookup_threads > 1 {
                format!(" {} {}", column(&r.lookup_mt), column(&r.batch_mt))
            } else {
                String::new()
            }
        );
    }
    if copies > 1 {
        println!("per copy, fastest pass (ns): lookup / batch / lookup x / batch x");
        for r in rows.iter().filter(|r| r.run) {
            let list = |s: &[Stat]| {
                s.iter()
                    .map(|s| {
                        if s.min.is_finite() {
                            format!("{:.2}", s.min)
                        } else {
                            "-".to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            println!(
                "{:<44} {} / {} / {} / {}",
                r.name,
                list(&r.lookup),
                list(&r.batch),
                list(&r.lookup_mt),
                list(&r.batch_mt)
            );
        }
    }
}
