//! Blobs written by *published* lexindex versions must still load and answer correctly.
//!
//! Every other test in this repo builds an index and reads it back with the same code, so all of
//! them would keep passing if a dependency bump silently changed the `epserde` image embedded in an
//! MPH blob, or if a framing field moved. The files in `tests/data/` were written by 0.5.1, 0.7.0,
//! 0.8.0, 0.8.1 and 0.9.1 through their PyPI wheels (`local/gen_golden.py` regenerates them) and
//! cover every on-disk format the current loader claims to accept: `BMP2`/`BMP3`/`BMP4` and
//! `BCH1`/`BCH3`/`BCH4`/`BCH5`.
//!
//! Ids from a minimal perfect hash are not reproducible across builds, so the assertions are the
//! invariants a correct load must satisfy — a bijection onto `[0, n)`, an exact reverse where the
//! index has one — rather than pinned id values. A blob that loaded but deserialised to a different
//! structure would fail them.

use std::path::PathBuf;

/// Versions whose blobs are kept. `0.5.1` is the oldest still-accepted `PerfectHashIndex` format.
const VERSIONS: [&str; 5] = ["0.5.1", "0.7.0", "0.8.0", "0.8.1", "0.9.1"];

fn data(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data")).join(name)
}

/// The exact key set every blob was built from, in the order the generator wrote it.
fn keys() -> Vec<String> {
    let text = std::fs::read_to_string(data("golden-keys.txt")).expect("golden key list");
    let keys: Vec<String> = text.lines().map(str::to_owned).collect();
    assert_eq!(keys.len(), 1000);
    keys
}

/// Keys the blobs were *not* built from — same shape as the real ones, so a membership check has to
/// do more than notice they look odd.
fn non_members() -> Vec<String> {
    (0..1000).map(|i| format!("absent-{i:04}")).collect()
}

#[test]
fn string_index_blobs_from_every_published_version_load() {
    let keys = keys();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();

    for version in VERSIONS {
        let path = data(&format!("golden-{version}-string.bix"));
        let idx = lexindex::StringIndex::load(&path).unwrap_or_else(|e| panic!("{version}: {e}"));
        assert_eq!(idx.len(), sorted.len(), "{version}");
        // `StringIndex` ids *are* reproducible: the id of a key is its rank in sorted order.
        for (rank, key) in sorted.iter().enumerate() {
            let id = rank as u64;
            assert_eq!(idx.id(key), Some(id), "{version}: id({key:?})");
            assert_eq!(
                idx.key(id).as_deref(),
                Some(key.as_str()),
                "{version}: key({id})"
            );
        }
        for absent in non_members() {
            assert_eq!(
                idx.id(&absent),
                None,
                "{version}: {absent:?} is not a member"
            );
        }
        // Traversal on a loaded blob, not just point lookups: every hit a prefix scan reports has
        // to carry the id the point lookup gives.
        let hits = idx.prefix("ka");
        assert!(!hits.is_empty(), "{version}: the corpus has \"ka\" keys");
        for (key, id) in hits {
            assert_eq!(idx.id(&key), Some(id), "{version}: prefix hit {key:?}");
        }
    }
}

#[cfg(feature = "mph")]
mod mph {
    use super::{VERSIONS, data, keys, non_members};

    #[test]
    fn perfect_hash_blobs_from_every_published_version_load() {
        let keys = keys();
        for version in VERSIONS {
            let path = data(&format!("golden-{version}-perfect.bmp"));
            // SAFETY: the blob is a committed file this repo generated with its own published
            // wheels — the trusted-source obligation the loader documents.
            let idx = unsafe { lexindex::PerfectHashIndex::load(&path) }
                .unwrap_or_else(|e| panic!("{version}: {e}"));
            assert_eq!(idx.len(), keys.len(), "{version}");

            let mut seen = vec![false; keys.len()];
            for key in &keys {
                let id = idx
                    .id(key)
                    .unwrap_or_else(|| panic!("{version}: member {key:?} not found"));
                assert!((id as usize) < keys.len(), "{version}: id out of range");
                assert!(!seen[id as usize], "{version}: id {id} handed out twice");
                seen[id as usize] = true;
                // The reverse map is exact, so a mis-deserialised arena shows up here.
                assert_eq!(idx.key(id), Some(key.as_str()), "{version}: key({id})");
            }
            for absent in non_members() {
                assert_eq!(
                    idx.id(&absent),
                    None,
                    "{version}: {absent:?} is not a member"
                );
            }
        }
    }

    /// `CompactHashIndex` membership is probabilistic, so the assertions split: every member must
    /// be found (a false negative is impossible by construction), while non-members are held to the
    /// 8-bit table's false-positive rate with room to spare — 1 000 probes at 2^-8 expect ~4.
    #[test]
    fn compact_hash_blobs_from_every_accepted_version_load() {
        let keys = keys();
        for version in VERSIONS {
            let path = data(&format!("golden-{version}-compact.bch"));
            // SAFETY: as above — a committed blob this repo wrote.
            let loaded = unsafe { lexindex::CompactHashIndex::load(&path) };
            if version == "0.5.1" {
                // `BCH1` is refused on purpose (0.7 rewrote the layout); the message must say so
                // rather than the load half-succeeding.
                assert!(loaded.is_err(), "a 0.5.1 BCH1 blob must be refused");
                continue;
            }
            let idx = loaded.unwrap_or_else(|e| panic!("{version}: {e}"));
            assert_eq!(idx.len(), keys.len(), "{version}");

            let mut seen = vec![false; keys.len()];
            for key in &keys {
                assert!(idx.contains(key), "{version}: false negative on {key:?}");
                let id = idx.id(key).expect("a member has an id") as usize;
                assert!(
                    id < keys.len() && !seen[id],
                    "{version}: id {id} is not a bijection"
                );
                seen[id] = true;
            }
            let false_positives = non_members().iter().filter(|k| idx.contains(k)).count();
            assert!(
                false_positives <= 20,
                "{version}: {false_positives} of 1 000 non-members accepted at 8 fingerprint bits",
            );
        }
    }

    /// The zero-copy path against a real file on disk, not a buffer this process just wrote.
    #[cfg(feature = "mmap")]
    #[test]
    fn the_newest_blobs_also_load_zero_copy() {
        let keys = keys();
        // SAFETY: committed blobs, and nothing in this process writes to them while mapped.
        let perfect =
            unsafe { lexindex::PerfectHashIndex::load_mmap(data("golden-0.9.1-perfect.bmp")) }
                .expect("mmap load");
        let compact =
            unsafe { lexindex::CompactHashIndex::load_mmap(data("golden-0.9.1-compact.bch")) }
                .expect("mmap load");
        let string = unsafe { lexindex::StringIndex::load_mmap(data("golden-0.9.1-string.bix")) }
            .expect("mmap load");
        assert_eq!(perfect.len(), keys.len());
        assert_eq!(compact.len(), keys.len());
        assert_eq!(string.len(), keys.len());
        for key in keys.iter().take(50) {
            let id = perfect.id(key).expect("member");
            assert_eq!(perfect.key(id), Some(key.as_str()));
            assert!(compact.contains(key));
            assert!(string.id(key).is_some());
        }
    }
}

/// The fuzz targets in `fuzz/` are seeded from these same files, and a target that rejected every
/// seed on its first branch would explore nothing while still reporting "no crashes". This asserts
/// the shims they call actually accept a real blob, so the seed corpus is worth something.
#[cfg(all(feature = "fuzzing", feature = "mph"))]
#[test]
fn the_fuzz_shims_accept_a_real_blob() {
    let compact = std::fs::read(data("golden-0.9.1-compact.bch")).unwrap();
    let perfect = std::fs::read(data("golden-0.9.1-perfect.bmp")).unwrap();
    for verify in [false, true] {
        assert!(
            lexindex::fuzzing::parse_compact_frame(&compact, verify),
            "compact frame rejected (verify={verify})"
        );
        assert!(
            lexindex::fuzzing::parse_perfect_frame(&perfect, verify),
            "perfect frame rejected (verify={verify})"
        );
    }
    assert!(!lexindex::fuzzing::parse_compact_frame(&perfect, true));
    assert!(!lexindex::fuzzing::parse_perfect_frame(&compact, true));
}
