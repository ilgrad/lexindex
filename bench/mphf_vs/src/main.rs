//! One process, A-B-A-B: lexindex's `Mphf` (the `bench-mphf` export) against `ph` 0.11.0's PHast
//! (`Function`, `SeedOnly`) and PHast+ (`Function2`, `ShiftOnlyWrapped`), both at 8-bit seeds and
//! bucket size 4.5, and `ptr_hash` 2.1.1's three parameter sets, over the same distinct
//! splitmix64 keys and the same shuffled probe order. Builds: the minimum over the rounds, and
//! the spread (the slowest round over the fastest, minus one). Lookups: every key once per pass
//! in the one shuffled order, three passes a round, the minimum and the spread over all passes;
//! and where a function has a batch or streaming form — lexindex `index_all`, `ptr_hash`
//! `index_stream` — that too, over the same order in chunks of 4096 keys; `ph` has none.
//! A round builds every row first and then times the lookups with the rows in turn within each
//! pass, so that a machine warming through the round slows every row alike; each turn starts
//! with an untimed sweep over the first 2^20 probe keys, which brings back into the cache what
//! the previous row's turn took out of it.
//! lexindex takes the keys as hashes; `ph` hashes each key with its default seeded hasher
//! (wyhash) at build and on every lookup level, `ptr_hash` with `FastIntHash` (one multiply).
//! Threads: lexindex and `ph` take the count directly, `ptr_hash` runs inside a rayon pool of
//! that size. Every build takes the keys in generation order, as they were drawn.
//!
//! `cd bench/mphf_vs && cargo run --release -- [n] [rounds] [threads]`
//!
//! `MPHF_VS_ROWS=lexindex,fast` runs only the rows whose name contains one of the substrings.
//! `MPHF_VS_NO_THP=1` turns transparent huge pages off for the process (`prctl`): the control
//! for lexindex's tables, which ask for them from 2 MiB up; no competitor's table asks.
//! `MPHF_VS_PROBES=m` probes m keys drawn at random (with replacement) instead of every key once:
//! at 1 B keys the probe order alone is another 8 GB beside a competitor's build peak.
//! `MPHF_VS_MISSES=1` adds a last-level-cache-misses-per-lookup column, counted over the same
//! single-lookup passes through `perf_event_open(2)` on the timing thread. It is off by default
//! because it puts an `ioctl` either side of a timed pass, and it is what separates a row that is
//! slow because it touches another cache line from one that is slow because it does more work:
//! at ten million keys `ptr_hash` compact takes 0.712 misses a lookup against lexindex's 0.862 and
//! is still twice the nanoseconds. A kernel that refuses the event leaves the column out.
//! `MPHF_VS_COPIES=k` looks each function up through k copies of its table, made one after
//! another after its build, and reports each lookup column as the median over the copies of a
//! copy's fastest pass, the spread then the slowest copy over the fastest: where a table lands
//! moves a lookup that misses the cache, and one table a row is one draw of it. Every copy is
//! checked to answer the first 2^20 probe keys as the table built does, but for PtrHash
//! compact's and balanced's: `CompactPtrHash` is not `Clone`, so those are built again, and at
//! 1 B keys a build does not give the same table twice.
//! `MPHF_VS_PLACEMENT=1` reads, before the lookups and after them, where each copy's pages
//! landed: its resident megabytes and two rates of eight threads over them, a sequential read and
//! independent loads of random lines (the `placement` module) — on a machine whose memory is not
//! uniform, the variable a lookup column carries.
use lexindex::Mphf;
use ph::phast::{
    Function, Function2, Params, SeedOnly, ShiftOnlyWrapped, bits_per_seed_to_100_bucket_size,
};
use ph::seeds::Bits8;
use ph::{BuildDefaultSeededHasher, GetSize};
use ptr_hash::hash::FastIntHash;
use ptr_hash::{CompactPtrHash, DefaultPtrHash, PtrHashParams};
use std::sync::Arc;
use std::time::Instant;

mod placement;

#[global_allocator]
static ALLOCATOR: placement::Tracked = placement::Tracked;

const CHUNK: usize = 4096;

/// Probe keys swept, untimed, before each row's turn.
const WARM: usize = 1 << 20;

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

    /// The same cell for a count a lookup rather than a nanosecond, which is under ten.
    fn cell3(self) -> String {
        if self.min.is_infinite() {
            return format!("{:>8} {:>7}", "-", "");
        }
        format!(
            "{:>8.3} {:>+6.1}%",
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

struct Row {
    name: &'static str,
    run: bool,
    build: Stat,
    /// Peak resident growth over the build, in MB: the table and whatever the build held at
    /// its peak, above the keys, the probe order and the tables already built.
    peak_mb: f64,
    /// Anonymous huge pages in the process right after the build, in MB: whether the table got
    /// the 2 MiB pages it asked for (only lexindex's asks).
    huge_mb: f64,
    bits: f64,
    /// A column a copy of the table.
    lookup: Vec<Stat>,
    batch: Vec<Stat>,
    /// Last-level cache misses a single lookup, over the same passes, when `MPHF_VS_MISSES` is
    /// set. Empty otherwise.
    misses: Stat,
    /// Wall time a key with `lookup_threads` threads each taking a share of the probe order.
    lookup_mt: Vec<Stat>,
    batch_mt: Vec<Stat>,
    /// A list a copy of the table, one entry a round when `MPHF_VS_PLACEMENT` is set: where it
    /// landed, read before the lookups and after them.
    place: Vec<Vec<(placement::Read, placement::Read)>>,
}

/// A pass over a slice of the probe order: the wrapping sum of the answers.
type Pass = Box<dyn Fn(&[u64]) -> u64 + Sync>;

/// One copy of a built function: a pass of single lookups, and one of batch lookups where the
/// function has them — each pass the function's own loop.
struct Table {
    row: usize,
    copy: usize,
    /// Whether the copy must answer every key as the row's first table does.
    exact: bool,
    single: Pass,
    batch: Option<Pass>,
}

/// How a row's tables after the first are made.
enum Copies<'a, T> {
    /// From the table built: the same function.
    Of(&'a dyn Fn(&T) -> T),
    /// Built again over the same keys: a table of the same size and parameters, whose answers
    /// may differ.
    Rebuilt(&'a dyn Fn() -> T),
}

/// One pass of single lookups over `probe`, out of line: each function's lookup is inlined into
/// a loop of its own rather than into the round beside everything else in it.
#[inline(never)]
fn sweep<T>(f: &T, probe: &[u64], get: &impl Fn(&T, u64) -> u64) -> u64 {
    let mut acc = 0u64;
    for &k in probe {
        acc = acc.wrapping_add(get(f, k));
    }
    acc
}

#[inline(never)]
fn sweep_batch<T>(f: &T, probe: &[u64], batch: fn(&T, &[u64]) -> u64) -> u64 {
    let mut acc = 0u64;
    for chunk in probe.chunks(CHUNK) {
        acc = acc.wrapping_add(batch(f, chunk));
    }
    acc
}

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

/// A `/proc/self/smaps_rollup` field in kB, `AnonHugePages:` for the huge pages backing the
/// process's anonymous memory.
fn smaps_kb(field: &str) -> f64 {
    let rollup =
        std::fs::read_to_string("/proc/self/smaps_rollup").expect("/proc/self/smaps_rollup");
    rollup
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.trim().trim_end_matches(" kB").parse().ok())
        .expect("an AnonHugePages line")
}

/// Resets the process's peak resident set to its current one (`clear_refs` 5), so that the
/// next `VmHWM` reading is the peak from here on.
fn reset_high_water() {
    std::fs::write("/proc/self/clear_refs", "5\n").expect("/proc/self/clear_refs");
}

impl Row {
    fn new(name: &'static str, run: bool, copies: usize) -> Self {
        Self {
            name,
            run,
            build: Stat::NONE,
            peak_mb: 0.0,
            huge_mb: 0.0,
            bits: 0.0,
            lookup: vec![Stat::NONE; copies],
            batch: vec![Stat::NONE; copies],
            misses: Stat::NONE,
            lookup_mt: vec![Stat::NONE; copies],
            batch_mt: vec![Stat::NONE; copies],
            place: vec![Vec::new(); copies],
        }
    }

    /// Times `build` over `keys` keys, takes the size from what it produced, and adds its
    /// copies to `tables`: the table built and one fewer than the row has columns made as
    /// `copies` says, one after another. `get` answers one key, `batch` the wrapping sum of the
    /// answers to a chunk.
    #[allow(clippy::too_many_arguments)]
    fn build<T: Send + Sync + 'static>(
        &mut self,
        row: usize,
        keys: usize,
        tables: &mut Vec<Table>,
        build: impl FnOnce() -> T,
        bits: impl Fn(&T) -> f64,
        copies: Copies<'_, T>,
        get: impl Fn(&T, u64) -> u64 + Copy + Send + Sync + 'static,
        batch: Option<fn(&T, &[u64]) -> u64>,
    ) {
        if !self.run {
            return;
        }
        reset_high_water();
        let before = status_kb("VmRSS:");
        // A copy's large allocations are noted as the table it will be in `tables`.
        let first = tables.len();
        placement::attribute(Some(first));
        let t = Instant::now();
        let f = build();
        self.build
            .add(t.elapsed().as_secs_f64() * 1e9 / keys as f64);
        placement::attribute(None);
        self.peak_mb = self.peak_mb.max((status_kb("VmHWM:") - before) / 1024.0);
        self.huge_mb = self.huge_mb.max(smaps_kb("AnonHugePages:") / 1024.0);
        self.bits = bits(&f);
        let exact = matches!(copies, Copies::Of(_));
        let more: Vec<T> = (1..self.lookup.len())
            .map(|c| {
                placement::attribute(Some(first + c));
                let f = match copies {
                    Copies::Of(copy) => copy(&f),
                    Copies::Rebuilt(build) => build(),
                };
                placement::attribute(None);
                f
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
                batch: batch
                    .map(|b| Box::new(move |probe: &[u64]| sweep_batch(&*g, probe, b)) as Pass),
            });
        }
    }
}

/// Times every table's lookups over `probe`, the tables in turn within each of three passes:
/// single and batch lookups on one thread, then as many passes with `threads` threads, each
/// taking a share of the probe order. Every turn starts with an untimed sweep over the first
/// [`WARM`] probe keys, whose answers an exact copy must agree with the row's first table on.
fn lookups(
    rows: &mut [Row],
    tables: &[Table],
    probe: &[u64],
    threads: usize,
    misses: Option<&Counter>,
) {
    let n = probe.len() as f64;
    let warm = &probe[..probe.len().min(WARM)];
    let ns = |t: Instant| t.elapsed().as_secs_f64() * 1e9 / n;
    let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
    let mut answers = vec![None; rows.len()];
    let mut warm_up = |t: &Table| {
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
            if let Some(c) = misses {
                c.start();
            }
            let s = Instant::now();
            std::hint::black_box((t.single)(probe));
            rows[t.row].lookup[t.copy].add(ns(s));
            if let Some(c) = misses {
                rows[t.row].misses.add(c.stop() / n);
            }
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

/// A hardware counter over the calling thread, opened through `perf_event_open(2)`.
///
/// There is no `/proc` file for cache misses, and the column that says *why* one row's lookup is
/// slower than another's cannot come from the clock: two functions that touch the same number of
/// cache lines and differ by a multiply are a different finding from two that differ by a miss.
/// Reset and read around one pass, so the count is that pass's and not the round's. A kernel that
/// refuses the event -- `perf_event_paranoid` above 2, a container without the capability --
/// leaves the column empty rather than failing the run, and the counter is off unless
/// `MPHF_VS_MISSES` asks for it, so a published timing is never taken with a syscall either side
/// of it.
struct Counter(i32);

/// `PERF_TYPE_HARDWARE` / `PERF_COUNT_HW_CACHE_MISSES`: the last level, which is the one a table
/// larger than the cache pays for.
const HW_CACHE_MISSES: u64 = 3;

impl Counter {
    fn open(config: u64) -> Option<Counter> {
        unsafe extern "C" {
            fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
        }
        // `struct perf_event_attr` as bytes, so that none of its bitfields has to be spelled as a
        // Rust type: type at 0, size at 4, config at 8, and the flag word at 40, where bit 0 is
        // `disabled`, bit 5 `exclude_kernel` and bit 6 `exclude_hv`.
        const ATTR_LEN: usize = 136;
        let mut attr = [0u8; ATTR_LEN];
        attr[0..4].copy_from_slice(&0u32.to_le_bytes());
        attr[4..8].copy_from_slice(&(ATTR_LEN as u32).to_le_bytes());
        attr[8..16].copy_from_slice(&config.to_le_bytes());
        attr[40..48].copy_from_slice(&((1u64 << 0) | (1 << 5) | (1 << 6)).to_le_bytes());
        // SAFETY: `perf_event_open(attr, pid = 0, cpu = -1, group = -1, flags = 0)` reads
        // `attr.size` bytes from a buffer of exactly that length and returns a file descriptor.
        let fd = unsafe { syscall(298, attr.as_mut_ptr(), 0i32, -1i32, -1i32, 0u64) };
        (fd >= 0).then_some(Counter(fd as i32))
    }

    fn ioctl(&self, request: u64) {
        unsafe extern "C" {
            fn ioctl(fd: i32, request: u64, ...) -> i32;
        }
        // SAFETY: the three `PERF_EVENT_IOC_*` below take no argument the kernel dereferences.
        unsafe { ioctl(self.0, request, 0u64) };
    }

    /// Zero the count and start it.
    fn start(&self) {
        self.ioctl(0x2403); // PERF_EVENT_IOC_RESET
        self.ioctl(0x2400); // PERF_EVENT_IOC_ENABLE
    }

    /// Stop the count and read it.
    fn stop(&self) -> f64 {
        self.ioctl(0x2401); // PERF_EVENT_IOC_DISABLE
        unsafe extern "C" {
            fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
        }
        let mut got = [0u8; 8];
        // SAFETY: the descriptor reads one `u64` into a buffer of exactly eight bytes.
        let n = unsafe { read(self.0, got.as_mut_ptr(), 8) };
        if n == 8 {
            u64::from_le_bytes(got) as f64
        } else {
            0.0
        }
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn close(fd: i32) -> i32;
        }
        // SAFETY: a descriptor this type opened and owns.
        unsafe { close(self.0) };
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

/// A `ph` function written out and read back: a copy in memory of its own.
fn reread<T>(write: impl Fn(&mut Vec<u8>) -> std::io::Result<()>, read: impl Fn(&[u8]) -> T) -> T {
    let mut bytes = Vec::new();
    write(&mut bytes).expect("write a ph function to memory");
    read(&bytes)
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
    let copies: usize = std::env::var("MPHF_VS_COPIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let place = std::env::var_os("MPHF_VS_PLACEMENT").is_some();
    // In generation order: every function here buckets its keys in the build, and a sorted input
    // would spare one of them that. splitmix64 is a bijection, so the keys are distinct.
    let keys: Vec<u64> = (0..n as u64).map(splitmix).collect();
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
    // Off unless asked: every ioctl is a syscall either side of a timed pass, and a published
    // timing is taken without one.
    let counter = std::env::var_os("MPHF_VS_MISSES").and_then(|_| Counter::open(HW_CACHE_MISSES));
    if std::env::var_os("MPHF_VS_MISSES").is_some() && counter.is_none() {
        eprintln!("perf_event_open refused; no miss column");
    }
    let params = Params::new(Bits8, bits_per_seed_to_100_bucket_size(8));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("rayon pool");
    let names = [
        "lexindex MPH3",
        "ph 0.11 PHast+ (ShiftOnlyWrapped)",
        "ph 0.11 PHast (SeedOnly)",
        "ptr_hash 2.1.1 compact",
        "ptr_hash 2.1.1 balanced",
        "ptr_hash 2.1.1 fast",
    ];
    let mut rows = names.map(|name| Row::new(name, wanted(name), copies));
    // `CompactPtrHash` is not `Clone` (its `EliasFano` remap is not), so its copies are built
    // again. `ptr_hash` draws its global seed from a fixed-seed generator, and at 1 M keys a
    // second build answers every probe key as the first, but at 1 B it does not: the sums of the
    // answers to 2^20 probe keys differed in each of four processes, by 6e9 to 3.6e10 of 5.2e14.
    let compact = || {
        pool.install(|| {
            CompactPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_compact())
        })
    };
    let balanced = || {
        pool.install(|| {
            CompactPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_balanced())
        })
    };
    for _ in 0..rounds {
        let mut tables = Vec::new();
        rows[0].build(
            0,
            n,
            &mut tables,
            || Mphf::build_with_threads(&keys, threads).expect("build"),
            |m| m.byte_len() as f64 * 8.0 / n as f64,
            Copies::Of(&Mphf::clone),
            |m, h| m.index(h),
            Some(|m: &Mphf, chunk: &[u64]| {
                m.index_all(chunk).into_iter().fold(0, u64::wrapping_add)
            }),
        );
        rows[1].build(
            1,
            n,
            &mut tables,
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
            Copies::Of(&|f: &Function2<Bits8, ShiftOnlyWrapped>| {
                reread(
                    |out| f.write(out),
                    |bytes| Function2::read(&mut &bytes[..]).expect("read PHast+ back"),
                )
            }),
            |f, h| f.get(&h) as u64,
            None,
        );
        rows[2].build(
            2,
            n,
            &mut tables,
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
            Copies::Of(&|f: &Function<Bits8, SeedOnly>| {
                reread(
                    |out| f.write(out),
                    |bytes| Function::read(&mut &bytes[..]).expect("read PHast back"),
                )
            }),
            |f, h| f.get(&h) as u64,
            None,
        );
        rows[3].build(
            3,
            n,
            &mut tables,
            compact,
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Rebuilt(&compact),
            |h, k| h.index(&k) as u64,
            Some(|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[4].build(
            4,
            n,
            &mut tables,
            balanced,
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Rebuilt(&balanced),
            |h, k| h.index(&k) as u64,
            Some(|h: &CompactPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        rows[5].build(
            5,
            n,
            &mut tables,
            || {
                pool.install(|| {
                    DefaultPtrHash::<FastIntHash, u64>::new(&keys, PtrHashParams::default_fast())
                })
            },
            |h| {
                let (p, r) = h.bits_per_element();
                p + r
            },
            Copies::Of(&DefaultPtrHash::clone),
            |h, k| h.index(&k) as u64,
            Some(|h: &DefaultPtrHash<FastIntHash, u64>, chunk: &[u64]| {
                h.index_stream::<32, _>(chunk.iter())
                    .fold(0usize, usize::wrapping_add) as u64
            }),
        );
        let before: Vec<placement::Read> = if place {
            (0..tables.len()).map(placement::read).collect()
        } else {
            Vec::new()
        };
        lookups(&mut rows, &tables, &order, lookup_threads, counter.as_ref());
        for (i, was) in before.into_iter().enumerate() {
            let t = &tables[i];
            rows[t.row].place[t.copy].push((was, placement::read(i)));
        }
    }
    let sample = if probes < n {
        format!(" of {probes} keys drawn at random")
    } else {
        String::new()
    };
    println!(
        "n {n}, {threads} thread(s), builds min of {rounds} rounds, lookups over one shuffled probe order{sample}, min of {} passes, the rows in turn; spread = slowest over fastest; batch = index_all / index_stream in chunks of {CHUNK}{}{}",
        3 * rounds,
        if copies > 1 {
            format!(
                "; each lookup column the median over {copies} copies of a table of the copy's fastest pass, spread = slowest copy over fastest; ptr_hash compact's and balanced's copies built again"
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
            format!(
                "{}{:>14}{:>9}",
                if counter.is_some() {
                    format!("{:>17}", "LLC miss/lookup")
                } else {
                    String::new()
                },
                "build peak MB",
                "huge MB"
            )
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
            column(&r.lookup),
            column(&r.batch),
            if lookup_threads > 1 {
                format!(" {} {}", column(&r.lookup_mt), column(&r.batch_mt))
            } else {
                String::new()
            },
            if single {
                format!(
                    "{}{:>14.1}{:>9.1}",
                    if counter.is_some() {
                        format!(" {}", r.misses.cell3())
                    } else {
                        String::new()
                    },
                    r.peak_mb,
                    r.huge_mb
                )
            } else {
                String::new()
            }
        );
    }
    if place {
        println!(
            "placement, per copy and round: resident MB; GB/s at which 8 threads read every line of its pages, 4 KiB at a time in a shuffled order; wall ns a load of 8 threads' independent loads of random lines of it -- each before the lookups -> after"
        );
        for r in rows.iter().filter(|r| r.run) {
            let copies: Vec<String> = r
                .place
                .iter()
                .map(|rounds| {
                    rounds
                        .iter()
                        .map(|(was, now)| {
                            format!(
                                "{:.0} MB {:.2} -> {:.2} GB/s {:.2} -> {:.2} ns",
                                was.mb, was.seq, now.seq, was.rnd, now.rnd
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect();
            println!("{:<36} {}", r.name, copies.join(" | "));
        }
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
                "{:<36} {} / {} / {} / {}",
                r.name,
                list(&r.lookup),
                list(&r.batch),
                list(&r.lookup_mt),
                list(&r.batch_mt)
            );
        }
    }
}
