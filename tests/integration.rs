//! End-to-end tests of the public `lexindex` API.

use lexindex::StringIndex;

fn catalog() -> Vec<String> {
    (0..2_000).map(|i| format!("entity-{i:05}")).collect()
}

#[test]
fn string_index_round_trips_a_catalog() {
    let names = catalog();
    let idx = StringIndex::build(&names).unwrap();
    assert_eq!(idx.len(), names.len());

    // forward + reverse are consistent across the whole catalog
    for name in &names {
        let id = idx.id(name).expect("present");
        assert_eq!(idx.key(id).as_deref(), Some(name.as_str()));
    }
    assert_eq!(idx.id("entity-99999"), None);

    // prefix narrows the catalog; results stay sorted
    let p = idx.prefix("entity-001");
    assert_eq!(p.len(), 100); // entity-00100 .. entity-00199
    assert!(p.windows(2).all(|w| w[0].0 <= w[1].0));

    // serialise → reload is faithful
    let reloaded = StringIndex::from_bytes(&idx.to_bytes()).unwrap();
    assert_eq!(reloaded.len(), idx.len());
    assert_eq!(reloaded.id("entity-01234"), idx.id("entity-01234"));
}

#[cfg(feature = "mph")]
#[test]
fn perfect_hash_index_is_a_fast_dictionary() {
    use lexindex::PerfectHashIndex;
    let names = catalog();
    let idx = PerfectHashIndex::build(&names).unwrap();
    assert_eq!(idx.len(), names.len());

    let mut ids: Vec<u32> = names.iter().map(|n| idx.id(n).expect("present")).collect();
    for (name, &id) in names.iter().zip(&ids) {
        assert_eq!(idx.key(id), Some(name.as_str())); // slot → key round-trips
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), names.len()); // ids are a dense bijection onto [0, n)

    assert!(!idx.contains("entity-99999"));
}

#[cfg(feature = "mmap")]
#[test]
fn zero_copy_mmap_load_round_trips() {
    let names = catalog();
    let idx = StringIndex::build(&names).unwrap();
    let path = std::env::temp_dir().join(format!("lexindex_it_mmap_{}.bix", std::process::id()));
    idx.save(&path).unwrap();

    // load_mmap borrows the index from the mapped file — forward, reverse and prefix all work.
    // SAFETY: this test owns the file and nothing writes to it while the map is alive.
    let mapped = unsafe { StringIndex::load_mmap(&path) }.unwrap();
    assert_eq!(mapped.len(), names.len());
    for name in &names {
        let id = mapped.id(name).expect("present");
        assert_eq!(mapped.key(id).as_deref(), Some(name.as_str()));
    }
    assert_eq!(mapped.prefix("entity-001").len(), 100);
    std::fs::remove_file(&path).ok();
}
