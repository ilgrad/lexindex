//! Blobs written by *published* lexindex versions, held to whatever this version promises about
//! them — that they still load and answer correctly, or that they are refused for a stated reason.
//!
//! Every other test in this repo builds an index and reads it back with the same code, so all of
//! them would keep passing if a dependency bump silently changed the `epserde` image embedded in an
//! MPH blob, or if a framing field moved. The files in `tests/data/` were written by 0.5.1, 0.7.0,
//! 0.8.0, 0.8.1 and 0.9.1 through their PyPI wheels (`local/gen_golden.py` regenerates them).
//!
//! 1.0 replaced the minimal perfect hash with one this crate owns, so every `BMP*` blob written
//! before it is unreadable — the crate that could decode the embedded image is no longer linked.
//! That is the promise now under test for `PerfectHashIndex`: refused, by a message that says the
//! blob is *old* rather than corrupt.
//!
//! Ids from the pre-1.0 minimal perfect hash were not reproducible across builds, so the
//! assertions are the invariants a correct load must satisfy — a bijection onto `[0, n)`, an exact
//! reverse where the index has one — rather than pinned id values. A blob that loaded but
//! deserialised to a different structure would fail them.

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

    /// The refusal has to be *legible*: someone with a five-year-old blob and a fresh lexindex
    /// gets one message, and it must tell them the file is old and rebuildable rather than send
    /// them looking for disk corruption.
    #[test]
    fn perfect_hash_blobs_from_every_published_version_are_refused_by_name() {
        for version in VERSIONS {
            let path = data(&format!("golden-{version}-perfect.bmp"));
            let err = match lexindex::PerfectHashIndex::load(&path) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{version}: a pre-1.0 blob was accepted"),
            };
            assert!(err.contains("lexindex < 1.0"), "{version}: {err}");
            assert!(err.contains("rebuild"), "{version}: {err}");
        }
    }

    /// A blob this version writes reloads exactly — and, because construction is deterministic
    /// since 1.0, reloads to the *same ids* a fresh build assigns.
    #[test]
    fn a_perfect_hash_blob_round_trips_exactly() {
        let keys = keys();
        let idx = lexindex::PerfectHashIndex::build(&keys).unwrap();
        let blob = idx.to_bytes().unwrap();
        assert_eq!(&blob[0..4], b"BMP6");
        let back = lexindex::PerfectHashIndex::from_bytes(&blob).expect("its own blob loads");
        for key in &keys {
            let id = idx.id(key).expect("member");
            assert_eq!(back.id(key), Some(id));
            assert_eq!(back.key(id), Some(key.as_str()));
        }
        for absent in non_members() {
            assert_eq!(back.id(&absent), None, "{absent:?} is not a member");
        }
    }

    /// Same as the perfect-hash refusal, and a shade stronger: this index stores no keys, so its
    /// pre-1.0 blobs cannot even be converted, and the message has to say so.
    #[test]
    fn compact_hash_blobs_from_every_published_version_are_refused_by_name() {
        for version in VERSIONS {
            let path = data(&format!("golden-{version}-compact.bch"));
            let err = match lexindex::CompactHashIndex::load(&path) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{version}: a pre-1.0 blob was accepted"),
            };
            assert!(err.contains("lexindex < 1.0"), "{version}: {err}");
            assert!(err.contains("rebuild"), "{version}: {err}");
        }
    }

    /// `CompactHashIndex` membership is probabilistic, so the assertions split: every member must
    /// be found (a false negative is impossible by construction), while non-members are held to the
    /// 8-bit table's false-positive rate with room to spare — 1 000 probes at 2^-8 expect ~4.
    #[test]
    fn a_1_0_compact_hash_blob_round_trips_exactly() {
        let keys = keys();
        let idx = lexindex::CompactHashIndex::build(&keys, 1).unwrap();
        let blob = idx.to_bytes().unwrap();
        assert_eq!(&blob[0..4], b"BCH6");
        let back = lexindex::CompactHashIndex::from_bytes(&blob).expect("its own blob loads");
        assert_eq!(back.len(), keys.len());
        for key in &keys {
            assert!(back.contains(key), "false negative on {key:?}");
            assert_eq!(back.id(key), idx.id(key));
        }
        let false_positives = non_members().iter().filter(|k| back.contains(k)).count();
        assert!(
            false_positives <= 20,
            "{false_positives} of 1 000 non-members accepted at 8 fingerprint bits",
        );
    }

    /// The 1.0 formats pinned by their **bytes**, which nothing could do before this release: ids
    /// from the pre-1.0 minimal perfect hash were not reproducible across builds, so every earlier
    /// golden test could only assert invariants. Construction is deterministic now (1.0-03) and the
    /// key hash is fixed (1.0-04), so the whole blob is a constant and this test is the strongest
    /// statement available: a changed header field, a changed section order, a changed checksum
    /// function or a changed hash fails *here*, at the line that names the format, rather than in
    /// whatever loads a stale blob a year from now.
    ///
    /// These two files are also what seeds `parse_compact`/`parse_perfect`. A seed that still
    /// parses but no longer resembles what the writer emits is the failure mode 1.0 already hit
    /// once, and byte-identity is what rules it out.
    ///
    /// Each is named by the release whose writer first produced it, and only the current pair is
    /// pinned this way: 1.1 re-encoded the key arena in blocks, so `golden-1.0.0-perfect.bmp` is
    /// now held to what a *reader* must promise it — see
    /// [`the_1_0_perfect_hash_blob_still_loads`] — and `golden-1.1.0-perfect.bmp` took over here.
    ///
    /// Regenerating them, if a format or the hash is deliberately changed:
    /// `cargo run --release --manifest-path local/goldengen/Cargo.toml`.
    #[test]
    fn the_current_hash_blobs_are_byte_identical_to_a_fresh_build() {
        let keys = keys();
        let compact = lexindex::CompactHashIndex::build(&keys, 1)
            .unwrap()
            .to_bytes()
            .unwrap();
        let perfect = lexindex::PerfectHashIndex::build(&keys)
            .unwrap()
            .to_bytes()
            .unwrap();

        for (name, magic, fresh) in [
            ("golden-1.0.0-compact.bch", &b"BCH6"[..], compact),
            ("golden-1.1.0-perfect.bmp", &b"BMP6"[..], perfect),
        ] {
            let path = data(name);
            let stored = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(&fresh[..4], magic, "{name}");
            assert_eq!(
                stored.len(),
                fresh.len(),
                "{name} changed size; regenerate {}",
                path.display()
            );
            assert!(
                stored == fresh,
                "{name} changed; regenerate {}",
                path.display()
            );
        }
    }

    /// What a `BMP5` blob is promised now that it is no longer the format this version writes:
    /// it loads, it answers every key with the id a fresh build assigns, it refuses every
    /// non-member, and it maps zero-copy. The bytes are free to differ — 1.1's arena is 2.7 bytes
    /// per key smaller — but nothing a caller can observe through the API may.
    ///
    /// This is the whole reason `BMP6` exists as a separate magic rather than a silent change to
    /// the arena: the format has two readable versions now, and this test is the statement that
    /// the older one is genuinely readable and not merely parsed.
    #[test]
    fn the_1_0_perfect_hash_blob_still_loads() {
        let keys = keys();
        let path = data("golden-1.0.0-perfect.bmp");
        let stored = std::fs::read(&path).unwrap();
        assert_eq!(&stored[..4], b"BMP5", "the 1.0 fixture must stay 1.0");

        let old = lexindex::PerfectHashIndex::load(&path).expect("a 1.0 blob still loads");
        let fresh = lexindex::PerfectHashIndex::build(&keys).unwrap();
        assert_eq!(old.len(), keys.len());
        for key in &keys {
            let id = fresh.id(key).expect("member");
            assert_eq!(old.id(key), Some(id), "id({key:?})");
            assert_eq!(old.key(id), Some(key.as_str()), "key({id})");
        }
        for absent in non_members() {
            assert_eq!(old.id(&absent), None, "{absent:?} is not a member");
        }
    }

    /// The zero-copy path against a real file on disk, not a buffer this process just wrote.
    #[cfg(feature = "mmap")]
    #[test]
    fn the_newest_blobs_also_load_zero_copy() {
        let keys = keys();
        // SAFETY: a committed blob, and nothing in this process writes to it while mapped.
        let string = unsafe { lexindex::StringIndex::load_mmap(data("golden-0.9.1-string.bix")) }
            .expect("mmap load");
        assert_eq!(string.len(), keys.len());
        for key in keys.iter().take(50) {
            assert!(string.id(key).is_some());
        }

        for name in ["golden-1.0.0-perfect.bmp", "golden-1.1.0-perfect.bmp"] {
            // SAFETY: as above.
            let perfect = unsafe { lexindex::PerfectHashIndex::load_mmap(data(name)) }
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(perfect.len(), keys.len(), "{name}");
            for key in keys.iter().take(50) {
                let id = perfect.id(key).unwrap_or_else(|| panic!("{name}: {key:?}"));
                assert_eq!(perfect.key(id), Some(key.as_str()), "{name}");
            }
        }
    }
}

/// The fuzz targets in `fuzz/` are seeded from these same files, and a target that rejected every
/// seed on its first branch would explore nothing while still reporting "no crashes". This asserts
/// the shims they call actually accept a real blob, so the seed corpus is worth something.
///
/// **The `0.9.1` blobs stopped being seeds when 1.0 changed the backend.** They are refused on the
/// magic now, one branch in, which is exactly the failure this test exists to catch — and it did not
/// catch it, because nothing in CI built the `fuzzing` feature. Both are fixed here: the seeds are
/// current, and `ci.yml` runs this test.
#[cfg(all(feature = "fuzzing", feature = "mph"))]
#[test]
fn the_fuzz_shims_accept_a_real_blob() {
    let compact = std::fs::read(data("golden-1.0.0-compact.bch")).unwrap();
    let perfect = std::fs::read(data("golden-1.1.0-perfect.bmp")).unwrap();
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

    // Both arena encodings are seeds: the flat table is still parsed, and a target that only ever
    // saw blocked offsets would leave that half of the reader unexplored.
    let flat = std::fs::read(data("golden-1.0.0-perfect.bmp")).unwrap();
    assert!(lexindex::fuzzing::parse_perfect_frame(&flat, true));

    // A pre-1.0 blob is refused at the magic, so it is worth nothing as a seed. Pinned so that the
    // seeds cannot silently go stale again the next time a format changes.
    let old = std::fs::read(data("golden-0.9.1-compact.bch")).unwrap();
    assert!(!lexindex::fuzzing::parse_compact_frame(&old, false));

    // The standalone MPH seed. `parse_mphf` starts inside the format the other two only reach
    // behind their own header, so without this file its target would have nothing to mutate: an
    // `MPH1` header is eight scalars, a checksum and a length identity, and no blob written for
    // another format gets past the magic.
    let mphf = std::fs::read(data("golden-1.0.0-mphf.bin")).unwrap();
    assert_eq!(&mphf[..4], b"MPH1");
    assert!(lexindex::fuzzing::parse_mphf(&mphf), "MPH seed rejected");
    assert!(!lexindex::fuzzing::parse_mphf(&compact));
    assert!(!lexindex::fuzzing::parse_mphf(&perfect));
}

/// The overlay format `0.12.0` published, and the one `1.0` writes. Their base is a `StringIndex`,
/// whose ids are a key's rank in sorted order, so unlike the MPH blobs these can be asserted exactly
/// rather than by invariant.
///
/// The overlay is also the one pre-1.0 blob this version still reads: `OVL1`'s parser never went
/// away, and a `StringIndex` base is a format 1.0 can still open. What `OVL1` cannot have is either
/// checksum, so the two files below are checked for different things — this one for loading at all,
/// the `1.0` one for its exact bytes.
///
/// Both are seeds the `parse_overlay` fuzz target needs: reaching a magic, a base blob whose length
/// agrees with the file, and (for `OVL2`) two checksums by chance is hopeless, so without them the
/// fuzzer would only ever exercise the rejection paths.
#[test]
fn the_overlay_blob_from_0_12_0_still_loads() {
    let keys = keys();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();

    let path = data("golden-0.12.0-overlay.ovl");
    let blob = std::fs::read(&path).expect("golden overlay blob");
    assert_eq!(&blob[..4], b"OVL1", "the legacy fixture must stay legacy");
    let ov = lexindex::Overlay::from_bytes_with(&blob, lexindex::StringIndex::from_bytes)
        .expect("0.12.0 overlay blob loads");

    // Three added, three removed: the count is unchanged, the membership is not.
    assert_eq!(ov.len(), sorted.len());
    assert_eq!(ov.id_space(), sorted.len() as u64 + 3);

    for removed in &sorted[..3] {
        assert_eq!(
            ov.id(removed),
            None,
            "removed key {removed:?} is still live"
        );
    }
    for (rank, key) in sorted.iter().enumerate().skip(3) {
        let id = rank as u64;
        assert_eq!(ov.id(key), Some(id), "id({key:?})");
        assert_eq!(ov.key(id).as_deref(), Some(key.as_str()), "key({id})");
    }
    for (i, added) in ["absent-0000", "absent-0001", "absent-0002"]
        .iter()
        .enumerate()
    {
        let id = sorted.len() as u64 + i as u64;
        assert_eq!(ov.id(added), Some(id), "id({added:?})");
        assert_eq!(ov.key(id).as_deref(), Some(*added), "key({id})");
    }
}

/// The `1.0` overlay format, pinned by its bytes rather than by what a load produces.
///
/// `StringIndex` construction is deterministic and so is `Overlay::to_bytes`, so a fresh build over
/// the same keys and edits must reproduce this file exactly. That makes it a check on the format
/// itself: a changed section order, a changed header field, or a changed checksum function fails
/// here rather than in whatever loads a stale blob a year from now. Nothing in the file depends on
/// the MPH key hash — the base is an FST and the two checksums live in `blob` — so the hash upgrade
/// still to come in this release does not touch it.
///
/// Regenerating it, if the format is deliberately changed: write `fresh` to the path below.
#[test]
fn the_1_0_overlay_blob_is_byte_identical_to_a_fresh_build() {
    let mut sorted = keys();
    sorted.sort();
    sorted.dedup();

    let mut ov = lexindex::Overlay::new(lexindex::StringIndex::build(&sorted).unwrap());
    for added in ["absent-0000", "absent-0001", "absent-0002"] {
        ov.add(added);
    }
    for removed in &sorted[..3] {
        assert!(ov.remove(removed));
    }
    let fresh = ov.to_bytes().unwrap();

    let path = data("golden-1.0.0-overlay.ovl");
    let stored = std::fs::read(&path).expect("golden overlay blob");
    assert_eq!(&stored[..4], b"OVL2");
    assert_eq!(
        stored.len(),
        fresh.len(),
        "the 1.0 overlay format changed size; regenerate {}",
        path.display()
    );
    assert!(
        stored == fresh,
        "the 1.0 overlay format changed; regenerate {}",
        path.display()
    );

    let back = lexindex::Overlay::from_bytes_with(&stored, lexindex::StringIndex::from_bytes)
        .expect("the committed blob loads");
    assert_eq!(back.len(), sorted.len());
    assert_eq!(back.id_space(), sorted.len() as u64 + 3);
    for removed in &sorted[..3] {
        assert_eq!(back.id(removed), None, "{removed:?} is still live");
    }
    for (rank, key) in sorted.iter().enumerate().skip(3) {
        assert_eq!(back.id(key), Some(rank as u64), "id({key:?})");
    }
}

/// A blob that `from_bytes` panics on and `from_untrusted_bytes` refuses.
///
/// `SECURITY.md` states the one exception to "arbitrary bytes get an `Err`": the ordered loader
/// can panic instead, because `fst`'s node decoder is safe Rust but not total and the checksum in
/// front of it is public, so bytes crafted to carry a matching one reach an invalid body. That
/// sentence was written from a fuzz run; this is the specimen, kept so the claim stays a measured
/// fact rather than a recollection. libFuzzer found it against `StringIndex::from_bytes` and could
/// not minimise it below 111 bytes.
///
/// The pair of assertions is the point. The first pins the gap the security policy documents, and
/// fails the day `fst` gains a total decoder — which is a fix, and should be noticed as one. The
/// second is the contract [`StringIndex::from_untrusted_bytes`] exists to provide, checked on a
/// real crafted blob rather than on bytes invented to be rejected.
#[test]
fn the_untrusted_loader_refuses_the_blob_the_owned_one_panics_on() {
    let bytes = std::fs::read(data("panicking-1.0.0-string.bix")).expect("the panic specimen");
    assert_eq!(bytes.len(), 111);

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let owned = std::panic::catch_unwind(|| lexindex::StringIndex::from_bytes(&bytes));
    std::panic::set_hook(hook);
    assert!(
        owned.is_err(),
        "from_bytes no longer panics on this blob -- if fst's decoder became total, \
         SECURITY.md and the from_bytes docstring both need their exception removed"
    );

    assert!(matches!(
        lexindex::StringIndex::from_untrusted_bytes(&bytes),
        Err(lexindex::IndexError::Format(_))
    ));
}
