//! Where a table's pages landed, read as rates — for a machine whose memory is not uniform.
//!
//! Which physical memory a table gets is the history of the process's allocations, and where the
//! memory is not uniform that moves every lookup that misses the cache. The machine these results
//! come from pairs an 8 GB module in one rank with a 32 GB one in two: the top 16 GiB of physical
//! memory interleaves both channels and the rest is the larger module alone. Without root a
//! page's physical address is hidden, so what is read is two rates over every page a copy of a
//! table occupies, each by [`READERS`] threads. Every line, 4 KiB at a time in a shuffled order so
//! that a table on huge pages and one on small pages are read alike: the more of the table the
//! channels interleave, the faster. And independent loads of random lines, the access a lookup
//! makes, which a lookup follows where the first rate does not: that sees which channel a page is
//! on, not how many ranks and banks a table's lines are spread over.
//!
//! The pages are found through the allocator, so that no competitor's private fields have to be
//! reached into: [`Tracked`] notes every live allocation of [`LARGE`] bytes or more made while a
//! table is [`attribute`]d, and `/proc/self/pagemap` keeps those of its pages that are resident
//! and this process's alone — a page only ever read maps the shared zero page, which reads as
//! fast as the cache.

use std::alloc::{GlobalAlloc, Layout, System};
use std::mem::MaybeUninit;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Allocations below this are not noted: a table's pages are in its large arrays.
const LARGE: usize = 1 << 20;

/// Live noted allocations at most: the allocator cannot allocate for its own bookkeeping, so they
/// sit in a fixed array.
const SLOTS: usize = 4096;

/// Threads reading a table's pages: the machine's rates above were read with eight.
const READERS: usize = 8;

/// Passes over a table's pages; the fastest is the one reported.
const PASSES: usize = 3;

/// Independent loads each reader makes of random lines of a table, a pass.
const LOADS: usize = 2_500_000;

const PAGE: usize = 4096;
const LINE: usize = 64;
const NOBODY: usize = usize::MAX;

/// The table whose large allocations are being noted, or [`NOBODY`].
static TABLE: AtomicUsize = AtomicUsize::new(NOBODY);

/// Set when a noted allocation found no free slot, so that a short page list is not taken for a
/// table's.
static OVERFLOW: AtomicBool = AtomicBool::new(false);

struct Live {
    n: usize,
    /// Address, bytes, table.
    at: [(usize, usize, usize); SLOTS],
}

static LIVE: Mutex<Live> = Mutex::new(Live {
    n: 0,
    at: [(0, 0, 0); SLOTS],
});

impl Live {
    fn note(&mut self, at: usize, bytes: usize, table: usize) {
        if self.n == SLOTS {
            OVERFLOW.store(true, Ordering::Relaxed);
            return;
        }
        self.at[self.n] = (at, bytes, table);
        self.n += 1;
    }

    /// Forgets the allocation at `at`, returning the table it was noted for.
    fn forget(&mut self, at: usize) -> Option<usize> {
        let i = self.at[..self.n].iter().position(|e| e.0 == at)?;
        let table = self.at[i].2;
        self.n -= 1;
        self.at[i] = self.at[self.n];
        Some(table)
    }
}

/// The notes, whatever a panic elsewhere left them in: the allocator must not panic.
fn live() -> MutexGuard<'static, Live> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn current() -> Option<usize> {
    let table = TABLE.load(Ordering::Relaxed);
    (table != NOBODY).then_some(table)
}

fn noted(p: *mut u8, bytes: usize) {
    if bytes >= LARGE
        && !p.is_null()
        && let Some(table) = current()
    {
        live().note(p as usize, bytes, table);
    }
}

/// The system allocator, noting the large allocations made for a table.
pub struct Tracked;

// SAFETY: every call is passed to `System` under the caller's own contract; the notes only record
// the addresses it returns, in memory of their own.
unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        noted(p, layout.size());
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        noted(p, layout.size());
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if layout.size() >= LARGE {
            live().forget(p as usize);
        }
        unsafe { System.dealloc(p, layout) }
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let was = if layout.size() >= LARGE {
            live().forget(p as usize)
        } else {
            None
        };
        let q = unsafe { System.realloc(p, layout, new_size) };
        if q.is_null() {
            // The old block is still live.
            if let Some(table) = was {
                live().note(p as usize, layout.size(), table);
            }
        } else if new_size >= LARGE
            && let Some(table) = was.or_else(current)
        {
            live().note(q as usize, new_size, table);
        }
        q
    }
}

/// Notes the large allocations made from now on as `table`'s, or no one's.
pub fn attribute(table: Option<usize>) {
    TABLE.store(table.unwrap_or(NOBODY), Ordering::SeqCst);
}

/// Every whole page of `table`'s live noted allocations that is resident and this process's
/// alone: bit 63 of its `/proc/self/pagemap` entry (present) and bit 56 (mapped here only). A
/// page only ever read maps the shared zero page, which is present and not exclusive.
fn pages(table: usize) -> Vec<usize> {
    assert!(
        !OVERFLOW.load(Ordering::Relaxed),
        "more than {SLOTS} large allocations live at once; the page lists are short"
    );
    // Allocated before the lock and filled within its capacity: an allocation of note under the
    // lock would take it again.
    let mut ranges = Vec::with_capacity(SLOTS);
    {
        let live = live();
        ranges.extend(
            live.at[..live.n]
                .iter()
                .filter(|e| e.2 == table)
                .map(|e| (e.0, e.1)),
        );
    }
    let pagemap = std::fs::File::open("/proc/self/pagemap").expect("/proc/self/pagemap");
    let mut pages = Vec::new();
    for (at, bytes) in ranges {
        let (first, end) = (at.div_ceil(PAGE), (at + bytes) / PAGE);
        if end <= first {
            continue;
        }
        let mut entries = vec![0u8; (end - first) * 8];
        pagemap
            .read_exact_at(&mut entries, (first * 8) as u64)
            .expect("read /proc/self/pagemap");
        for (i, e) in entries.as_chunks::<8>().0.iter().enumerate() {
            let e = u64::from_le_bytes(*e);
            if e >> 63 == 1 && (e >> 56) & 1 == 1 {
                pages.push((first + i) * PAGE);
            }
        }
    }
    pages
}

/// Where a copy of a table landed, as [`read`] finds it.
#[derive(Clone, Copy)]
pub struct Read {
    /// Resident MB.
    pub mb: f64,
    /// GB/s at which every line is read, 4 KiB at a time in a shuffled order.
    pub seq: f64,
    /// Wall ns a load, of independent loads of random lines.
    pub rnd: f64,
}

/// `table`'s resident pages, and the fastest of [`PASSES`] passes of [`READERS`] threads over
/// them: reading every line of each page, the pages in a shuffled order and each thread a share
/// of them; and making [`LOADS`] independent loads each of lines drawn at random.
pub fn read(table: usize) -> Read {
    let pages = pages(table);
    let mb = (pages.len() * PAGE) as f64 / 1e6;
    if pages.is_empty() {
        return Read {
            mb,
            seq: f64::NAN,
            rnd: f64::NAN,
        };
    }
    Read {
        mb,
        seq: sequential(&pages),
        rnd: random(&pages),
    }
}

/// Reads one line of this process's memory.
///
/// # Safety
///
/// `line` is inside a live allocation of this process. It is read as `MaybeUninit` because part
/// of a table's allocation may never have been written; nothing writes the tables while they are
/// read.
unsafe fn touch(line: usize) {
    std::hint::black_box(unsafe { std::ptr::read_volatile(line as *const MaybeUninit<u64>) });
}

fn sequential(pages: &[usize]) -> f64 {
    let order: Vec<usize> = crate::shuffled(pages.len())
        .into_iter()
        .map(|i| pages[i as usize])
        .collect();
    let share = order.len().div_ceil(READERS);
    let bytes = (order.len() * PAGE) as f64;
    let mut best = 0.0f64;
    for _ in 0..PASSES {
        let t = Instant::now();
        std::thread::scope(|scope| {
            for part in order.chunks(share) {
                scope.spawn(move || {
                    for &page in part {
                        for line in (page..page + PAGE).step_by(LINE) {
                            // SAFETY: a line of a whole page of a live table.
                            unsafe { touch(line) };
                        }
                    }
                });
            }
        });
        best = best.max(bytes / t.elapsed().as_nanos() as f64);
    }
    best
}

fn random(pages: &[usize]) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..PASSES {
        let t = Instant::now();
        std::thread::scope(|scope| {
            for reader in 0..READERS as u64 {
                scope.spawn(move || {
                    let mut r =
                        0x9E37_79B9_7F4A_7C15 ^ (reader + 1).wrapping_mul(0x2545_F491_4F6C_DD1D);
                    for _ in 0..LOADS {
                        r ^= r << 13;
                        r ^= r >> 7;
                        r ^= r << 17;
                        let page = pages[((r as u128 * pages.len() as u128) >> 64) as usize];
                        // SAFETY: a line of a whole page of a live table.
                        unsafe { touch(page + (r as usize % (PAGE / LINE)) * LINE) };
                    }
                });
            }
        });
        best = best.min(t.elapsed().as_nanos() as f64 / (READERS * LOADS) as f64);
    }
    best
}
