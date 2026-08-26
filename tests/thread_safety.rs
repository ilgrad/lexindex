//! The index types must stay `Send + Sync`, and must actually serve concurrent readers.
//!
//! The PyO3 bindings release the GIL around building, bulk queries and persistence
//! (`Python::detach`), which is sound only while these hold. That is already enforced indirectly —
//! each closure captures `&self`, so losing `Sync` would stop `&Self` being `Send` and break the
//! build of `src/python.rs` — but only in a build that enables the `python` feature. Pinning it
//! here means a plain `cargo test` catches a dependency bump that takes it away, and names the
//! reason rather than surfacing it as a confusing closure-bound error somewhere else.

use lexindex::StringIndex;
use std::sync::Arc;
use std::thread;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn index_types_are_send_and_sync() {
    assert_send_sync::<StringIndex>();
    #[cfg(feature = "mph")]
    {
        assert_send_sync::<lexindex::CompactHashIndex>();
        assert_send_sync::<lexindex::PerfectHashIndex>();
    }
}

/// The guarantee in use: one shared index, many threads, every lookup still correct. A compile-time
/// bound says the types *may* be shared; this says sharing them actually works.
#[test]
fn one_index_serves_many_readers() {
    let keys: Vec<String> = (0..2_000).map(|i| format!("key-{i:05}")).collect();
    let idx = Arc::new(StringIndex::build(&keys).unwrap());

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let idx = Arc::clone(&idx);
            let keys = keys.clone();
            thread::spawn(move || {
                for (rank, key) in keys.iter().enumerate() {
                    let id = rank as u64;
                    assert_eq!(idx.id(key), Some(id));
                    assert_eq!(idx.key(id).as_deref(), Some(key.as_str()));
                }
                idx.prefix("key-001").len()
            })
        })
        .collect();

    // Every reader must agree — a data race would show up as a differing count.
    for r in readers {
        assert_eq!(r.join().unwrap(), 100); // key-00100..key-00199
    }
}

#[cfg(feature = "mph")]
#[test]
fn mph_dictionaries_serve_many_readers() {
    use lexindex::{CompactHashIndex, PerfectHashIndex};

    let keys: Vec<String> = (0..2_000).map(|i| format!("tok-{i:05}")).collect();
    let exact = Arc::new(PerfectHashIndex::build(&keys).unwrap());
    let compact = Arc::new(CompactHashIndex::build(&keys, 2).unwrap());

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let (exact, compact, keys) = (Arc::clone(&exact), Arc::clone(&compact), keys.clone());
            thread::spawn(move || {
                for key in &keys {
                    let id = exact.id(key).expect("member");
                    assert_eq!(exact.key(id), Some(key.as_str())); // reverse round-trips
                    assert!(compact.contains(key)); // never a false negative on a member
                }
            })
        })
        .collect();

    for r in readers {
        r.join().unwrap();
    }
}
