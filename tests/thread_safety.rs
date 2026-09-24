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
    assert_send_sync::<lexindex::DictIndex>();
    assert_send_sync::<lexindex::DoubleArrayIndex>();
    #[cfg(feature = "mph")]
    {
        assert_send_sync::<lexindex::CompactHashIndex>();
        assert_send_sync::<lexindex::PerfectHashIndex>();
        assert_send_sync::<lexindex::HashedDictIndex>();
    }
}

/// The guarantee in use: one shared index, many threads, every lookup still correct. A compile-time
/// bound says the types *may* be shared; this says sharing them actually works.
#[test]
fn one_index_serves_many_readers() {
    let keys: Vec<String> = (0..2_000).map(|i| format!("key-{i:05}")).collect();
    let idx = Arc::new(StringIndex::build(&keys).unwrap());
    let double_array = Arc::new(lexindex::DoubleArrayIndex::build(&keys).unwrap());
    // Longer than the 256 bytes the occurrence walk decodes on the stack, and a `k` starts every
    // key, so the joined text holds these hundred and nothing across a join.
    let text = keys[100..200].concat();

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let idx = Arc::clone(&idx);
            let keys = keys.clone();
            let (double_array, text) = (Arc::clone(&double_array), text.clone());
            thread::spawn(move || {
                for (rank, key) in keys.iter().enumerate() {
                    let id = rank as u64;
                    assert_eq!(idx.id(key), Some(id));
                    assert_eq!(idx.key(id).as_deref(), Some(key.as_str()));
                    assert_eq!(double_array.id(key), Some(id)); // the same ranks
                }
                let mut occurrences = 0;
                double_array.for_each_occurrence(&text, |_, _, _| occurrences += 1);
                (idx.prefix("key-001").len(), occurrences)
            })
        })
        .collect();

    // Every reader must agree — a data race would show up as a differing count.
    for r in readers {
        assert_eq!(r.join().unwrap(), (100, 100)); // key-00100..key-00199, both ways
    }
}

#[cfg(feature = "mph")]
#[test]
fn mph_dictionaries_serve_many_readers() {
    use lexindex::{CompactHashIndex, PerfectHashIndex};

    let keys: Vec<String> = (0..2_000).map(|i| format!("tok-{i:05}")).collect();
    let exact = Arc::new(PerfectHashIndex::build(&keys).unwrap());
    let compact = Arc::new(CompactHashIndex::build(&keys, 2).unwrap());
    let dict = lexindex::DictIndex::build(&keys).unwrap();
    let hashed = Arc::new(lexindex::HashedDictIndex::from_dict(dict, 8).unwrap());

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let (exact, compact, keys) = (Arc::clone(&exact), Arc::clone(&compact), keys.clone());
            let hashed = Arc::clone(&hashed);
            thread::spawn(move || {
                for (rank, key) in keys.iter().enumerate() {
                    let id = exact.id(key).expect("member");
                    assert_eq!(exact.key(id), Some(key.as_str())); // reverse round-trips
                    assert!(compact.contains(key)); // never a false negative on a member
                    assert_eq!(hashed.id(key), Some(rank as u64)); // the keys are in rank order
                }
            })
        })
        .collect();

    for r in readers {
        r.join().unwrap();
    }
}
