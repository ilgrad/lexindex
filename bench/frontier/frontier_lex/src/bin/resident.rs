//! What a lexindex blob weighs and what a load of it keeps, for `bench/frontier/floor.py`: build
//! each kind from a one-key-per-line file as `frontier_lex` does, write its blob, then count the
//! heap two loads keep -- `from_bytes`, which copies the blob in, and `load_mmap`, which leaves the
//! blob to the page cache and allocates only what it derives beside it -- each after a pass of
//! lookups, so nothing derived lazily is missed. Prints `kind,keys,blob,from_bytes_heap,mmap_heap`
//! a kind. Nothing is timed.
//!
//! The count is the bytes each live allocation asked for, as string-index-benchmarks'
//! `harness/marisa_resident.cpp` counts marisa's. Its own binary, so that `frontier_lex`'s timings
//! never pay for a counting allocator.
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::BufRead;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use lexindex::{DictIndex, DictLayout, StringIndex};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded to `System` unchanged; the counter reads only the layouts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Relaxed);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            LIVE.fetch_sub(layout.size(), Relaxed);
            LIVE.fetch_add(new_size, Relaxed);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The heap `load` leaves behind once `probe` has run over what it returned.
fn kept<T>(load: impl FnOnce() -> T, probe: impl Fn(&T)) -> usize {
    let before = LIVE.load(Relaxed);
    let index = load();
    probe(&index);
    let after = LIVE.load(Relaxed);
    drop(index);
    after - before
}

trait Resident: Sized {
    fn blob(&self) -> Vec<u8>;
    fn from_bytes(bytes: &[u8]) -> Self;
    /// # Safety
    /// Nothing may modify the file while the index maps it.
    unsafe fn load_mmap(path: &Path) -> Self;
    fn id(&self, key: &str) -> Option<u64>;
}

impl Resident for DictIndex {
    fn blob(&self) -> Vec<u8> {
        self.to_bytes()
    }
    fn from_bytes(bytes: &[u8]) -> Self {
        DictIndex::from_bytes(bytes).expect("dict from_bytes")
    }
    unsafe fn load_mmap(path: &Path) -> Self {
        unsafe { DictIndex::load_mmap(path) }.expect("dict load_mmap")
    }
    fn id(&self, key: &str) -> Option<u64> {
        DictIndex::id(self, key)
    }
}

impl Resident for StringIndex {
    fn blob(&self) -> Vec<u8> {
        self.to_bytes()
    }
    fn from_bytes(bytes: &[u8]) -> Self {
        StringIndex::from_bytes(bytes).expect("string from_bytes")
    }
    unsafe fn load_mmap(path: &Path) -> Self {
        unsafe { StringIndex::load_mmap(path) }.expect("string load_mmap")
    }
    fn id(&self, key: &str) -> Option<u64> {
        StringIndex::id(self, key)
    }
}

fn measure<T: Resident>(built: T, keys: &[String], kind: &str, file: &Path) {
    let blob = built.blob();
    drop(built);
    std::fs::write(file, &blob).expect("write the blob");
    let probe = |index: &T| {
        for (id, key) in keys.iter().enumerate().step_by(97) {
            assert_eq!(
                index.id(key),
                Some(id as u64),
                "{kind} answered a wrong id for {key:?}"
            );
        }
    };
    let owned = kept(|| T::from_bytes(&blob), probe);
    // SAFETY: the file is this process's own, written above, and nothing writes it while mapped.
    let mapped = kept(|| unsafe { T::load_mmap(file) }, probe);
    println!("{kind},{},{},{owned},{mapped}", keys.len(), blob.len());
}

fn dict(keys: &[String], layout: DictLayout) -> DictIndex {
    DictIndex::build_with(keys, layout).expect("dict build")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: resident <keys.txt> <dict32|dict256|dict1024|labels256|labels1024|string>..."
        );
        std::process::exit(2);
    }
    // Read, sorted and deduplicated as `frontier_lex` reads a corpus, so a blob here is the blob
    // the campaign sized, byte for byte.
    let file = std::fs::File::open(&args[1]).expect("open");
    let mut keys: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map(|line| line.expect("read"))
        .filter(|line| !line.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let blob = std::env::temp_dir().join(format!("lexindex-resident-{}.bin", std::process::id()));
    println!("kind,keys,blob,from_bytes_heap,mmap_heap");
    for kind in &args[2..] {
        match kind.as_str() {
            "dict32" => measure(dict(&keys, DictLayout::new(32)), &keys, kind, &blob),
            "dict256" => measure(dict(&keys, DictLayout::new(256)), &keys, kind, &blob),
            "dict1024" => measure(dict(&keys, DictLayout::new(1024)), &keys, kind, &blob),
            "labels256" => {
                let layout = DictLayout::new(256).with_labels(true);
                measure(dict(&keys, layout), &keys, kind, &blob);
            }
            "labels1024" => {
                let layout = DictLayout::new(1024).with_labels(true);
                measure(dict(&keys, layout), &keys, kind, &blob);
            }
            "string" => {
                let built = StringIndex::build(&keys).expect("string build");
                measure(built, &keys, kind, &blob);
            }
            other => {
                eprintln!("unknown kind {other}");
                std::process::exit(2);
            }
        }
    }
    std::fs::remove_file(&blob).ok();
}
