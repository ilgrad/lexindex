//! [`Estimate::resident_bytes`] against the heap a mapped load really keeps, counted by the
//! allocator. One test in this binary: the count is of the whole process.
#![cfg(all(feature = "mph", feature = "mmap"))]

use lexindex::{
    ClosedHashIndex, CompactHashIndex, DictIndex, DictLayout, DoubleArrayIndex, Estimate,
    HashedDictIndex, Kind, Needs, PerfectHashIndex, StringIndex, plan,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to `System` with the arguments it was given; the counter is all that
// is added.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Relaxed);
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size, Relaxed);
        LIVE.fetch_sub(layout.size(), Relaxed);
        // SAFETY: the caller's contract, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// What `load` leaves on the heap while its result lives.
fn heap_of<T>(load: impl FnOnce() -> T) -> usize {
    let before = LIVE.load(Relaxed);
    let index = load();
    let after = LIVE.load(Relaxed);
    drop(index);
    after.saturating_sub(before)
}

/// The heap a load of this row's index keeps, and the bytes of its file.
fn counted(e: &Estimate, keys: &[&str], path: &Path) -> (usize, usize) {
    // A row prices one layout, so what it quotes is that layout's build.
    let layout = || {
        let block = e.block.expect("a dictionary names its block");
        let micro = e.micro.expect("and its microblock");
        DictLayout::new(block).with_micro(micro)
    };
    // SAFETY: of every `load_mmap`: nothing writes the file while it is mapped, and each mapping
    // ends before the next `save`.
    let heap = unsafe {
        match e.kind {
            Kind::String => {
                StringIndex::build(keys).unwrap().save(path).unwrap();
                heap_of(|| StringIndex::load_mmap(path).unwrap())
            }
            Kind::Dict => {
                let dict = DictIndex::build_with(keys, layout()).unwrap();
                dict.save(path).unwrap();
                heap_of(|| DictIndex::load_mmap(path).unwrap())
            }
            Kind::Hashed => {
                let dict = DictIndex::build_with(keys, layout()).unwrap();
                HashedDictIndex::from_dict(dict, 0)
                    .unwrap()
                    .save(path)
                    .unwrap();
                heap_of(|| HashedDictIndex::load_mmap(path).unwrap())
            }
            Kind::DoubleArray => {
                DoubleArrayIndex::build(keys).unwrap().save(path).unwrap();
                heap_of(|| DoubleArrayIndex::load_mmap(path).unwrap())
            }
            Kind::Perfect => {
                PerfectHashIndex::build(keys).unwrap().save(path).unwrap();
                heap_of(|| PerfectHashIndex::load_mmap(path).unwrap())
            }
            Kind::Compact => {
                CompactHashIndex::build_bits(keys, 8)
                    .unwrap()
                    .save(path)
                    .unwrap();
                heap_of(|| CompactHashIndex::load_mmap(path).unwrap())
            }
            // No mapping: the owned load is the blob, and the estimate says so.
            Kind::Closed => {
                ClosedHashIndex::build(keys).unwrap().save(path).unwrap();
                let owned = heap_of(|| ClosedHashIndex::load(path).unwrap());
                owned.saturating_sub(std::fs::metadata(path).unwrap().len() as usize)
            }
            other => panic!("no loader here for {other:?}"),
        }
    };
    (heap, std::fs::metadata(path).unwrap().len() as usize)
}

#[test]
fn the_resident_column_is_what_a_mapped_load_keeps() {
    let latin: Vec<String> = (0..4_000)
        .map(|i| format!("/usr/share/doc/package-{}/file-{i:05}.html", i % 37))
        .collect();
    // A coded blob, with characters past the Basic Multilingual Plane for the double array.
    let han: Vec<String> = (0..4_000u32)
        .map(|i| {
            let a = char::from_u32(0x4E00 + i % 3_000).unwrap();
            let b = char::from_u32(0x20000 + i % 700).unwrap();
            format!("北京{a}{b}{i}")
        })
        .collect();
    let dir = std::env::temp_dir().join(format!("lexindex-resident-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("blob");
    for corpus in [&latin, &han] {
        let mut keys: Vec<&str> = corpus.iter().map(String::as_str).collect();
        keys.sort_unstable();
        let p = plan(&keys, Needs::default().closed_vocabulary()).unwrap();
        for kind in [
            Kind::String,
            Kind::Dict,
            Kind::Hashed,
            Kind::DoubleArray,
            Kind::Perfect,
            Kind::Compact,
            Kind::Closed,
        ] {
            assert!(
                p.estimates().iter().any(|e| e.kind == kind),
                "{kind:?} is not in\n{p}"
            );
        }
        for e in p.estimates() {
            let (heap, blob) = counted(e, &keys, &path);
            assert_eq!(e.bytes as usize, blob, "{e:?}");
            let priced = (e.resident_bytes - e.bytes) as usize;
            // The structs themselves are not priced, a constant and not a rate; a coded blob's
            // route over its decoded heads is taken blob by blob, and is priced when it is built.
            let slack = 1_024 + heap / 100;
            assert!(
                priced.abs_diff(heap) <= slack,
                "{e:?}: priced {priced} bytes of heap, the load keeps {heap}"
            );
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
