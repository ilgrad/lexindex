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
//!
//! 1.1 changed the perfect hash once more (`MPH2`, a different function over the same keys) and
//! re-encoded the key arena, and 2.0 replaced the key hash itself — the round it shipped with had
//! a two-word collision family on ordinary text — so the hash blobs 1.0 and 1.1 wrote are refused
//! by name like the pre-1.0 ones, and the blobs 2.0 writes are the ones pinned byte for byte.

use std::path::PathBuf;

/// Versions whose blobs are kept. `0.5.1` is the oldest still-accepted `StringIndex` format.
const VERSIONS: [&str; 5] = ["0.5.1", "0.7.0", "0.8.0", "0.8.1", "0.9.1"];

/// The versions whose hash blobs 2.0 refuses on top of those: keyed on the hash 2.0 replaced.
#[cfg(feature = "mph")]
const HASH_VERSIONS_REFUSED: [&str; 2] = ["1.0.0", "1.1.0"];

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
    use super::{HASH_VERSIONS_REFUSED, VERSIONS, data, keys, non_members};

    /// The refusal has to be *legible*: someone with a five-year-old blob and a fresh lexindex
    /// gets one message, and it must tell them the file is old and rebuildable rather than send
    /// them looking for disk corruption. The 1.0 and 1.1 blobs are held to it too: loaded under
    /// 2.0's hash they would answer wrong ids, so the loader must not get that far.
    #[test]
    fn perfect_hash_blobs_from_every_published_version_are_refused_by_name() {
        for version in VERSIONS.iter().chain(HASH_VERSIONS_REFUSED.iter()) {
            let path = data(&format!("golden-{version}-perfect.bmp"));
            let err = match lexindex::PerfectHashIndex::load(&path) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{version}: a blob from before 2.0 was accepted"),
            };
            assert!(err.contains("lexindex < 2.0"), "{version}: {err}");
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
        assert_eq!(&blob[0..4], b"BMP7");
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
    /// old blobs cannot even be converted, and the message has to say so.
    #[test]
    fn compact_hash_blobs_from_every_published_version_are_refused_by_name() {
        for version in VERSIONS.iter().chain(HASH_VERSIONS_REFUSED.iter()) {
            let path = data(&format!("golden-{version}-compact.bch"));
            let err = match lexindex::CompactHashIndex::load(&path) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{version}: a blob from before 2.0 was accepted"),
            };
            assert!(err.contains("lexindex < 2.0"), "{version}: {err}");
            assert!(err.contains("rebuild"), "{version}: {err}");
        }
    }

    /// `CompactHashIndex` membership is probabilistic, so the assertions split: every member must
    /// be found (a false negative is impossible by construction), while non-members are held to the
    /// 8-bit table's false-positive rate with room to spare — 1 000 probes at 2^-8 expect ~4.
    #[test]
    fn a_compact_hash_blob_round_trips_exactly() {
        let keys = keys();
        let idx = lexindex::CompactHashIndex::build(&keys, 1).unwrap();
        let blob = idx.to_bytes().unwrap();
        assert_eq!(&blob[0..4], b"BCH7");
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
    /// Each is named by the release whose writer first produced it, and only the current set is
    /// pinned this way: 1.1 re-encoded the key arena and replaced the perfect hash, 2.0 replaced
    /// the key hash, so the 1.0 and 1.1 files are now the refused fixtures and every hash blob
    /// here is 2.0's — the plain pair, the fingerprinted arena, the overflow arena and the
    /// `ClosedHashIndex` blob.
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
        let perfect_fp = lexindex::PerfectHashIndex::build_with_fingerprints(&keys)
            .unwrap()
            .to_bytes()
            .unwrap();
        let mut overflow_keys = keys.clone();
        overflow_keys.push("x".repeat(300));
        let perfect_overflow = lexindex::PerfectHashIndex::build(&overflow_keys)
            .unwrap()
            .to_bytes()
            .unwrap();
        let closed = lexindex::ClosedHashIndex::build(&keys).unwrap().to_bytes();

        for (name, magic, fresh) in [
            ("golden-2.0.0-compact.bch", &b"BCH7"[..], compact),
            ("golden-2.0.0-perfect.bmp", &b"BMP7"[..], perfect),
            ("golden-2.0.0-perfect-fp.bmp", &b"BMP7"[..], perfect_fp),
            (
                "golden-2.0.0-perfect-overflow.bmp",
                &b"BMP7"[..],
                perfect_overflow,
            ),
            ("golden-2.0.0-closed.bcl", &b"BCL1"[..], closed),
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

    /// The refused fixtures must stay what they were: a `BMP5`, a `BMP6` and two `BCH6` — one
    /// with 1.0's `MPH1` inside, one with `MPH2` — so that the refusal tests above keep refusing
    /// the real thing and not a file someone regenerated by mistake.
    #[test]
    fn the_refused_fixtures_are_still_the_old_formats() {
        for (name, magic, mphf) in [
            ("golden-1.0.0-perfect.bmp", &b"BMP5"[..], &b"MPH1"[..]),
            ("golden-1.1.0-perfect.bmp", &b"BMP6"[..], &b"MPH2"[..]),
            ("golden-1.0.0-compact.bch", &b"BCH6"[..], &b"MPH1"[..]),
            ("golden-1.1.0-compact.bch", &b"BCH6"[..], &b"MPH2"[..]),
        ] {
            let stored = std::fs::read(data(name)).unwrap();
            assert_eq!(&stored[..4], magic, "{name}");
            assert!(
                stored.windows(4).any(|w| w == mphf),
                "{name}: no {mphf:?} inside"
            );
        }
    }

    /// The closed index's blob: it loads, every golden key gets a distinct id below `n`, and so
    /// does every stranger -- the only promise this index makes, and the one byte-identity
    /// cannot state.
    #[test]
    fn the_closed_blob_loads_and_answers_every_key() {
        let keys = keys();
        let idx = lexindex::ClosedHashIndex::load(data("golden-2.0.0-closed.bcl")).unwrap();
        assert_eq!(idx.len(), keys.len());
        let mut seen = vec![false; keys.len()];
        for key in &keys {
            let id = idx.id(key) as usize;
            assert!(
                id < keys.len() && !std::mem::replace(&mut seen[id], true),
                "{key:?}: id {id} is not distinct"
            );
        }
        let singular: Vec<u32> = keys.iter().map(|k| idx.id(k)).collect();
        assert_eq!(idx.ids_of(&keys), singular);
        for stranger in non_members() {
            assert!((idx.id(&stranger) as usize) < keys.len(), "{stranger:?}");
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

        for name in ["golden-2.0.0-perfect.bmp", "golden-2.0.0-perfect-fp.bmp"] {
            // SAFETY: as above.
            let perfect = unsafe { lexindex::PerfectHashIndex::load_mmap(data(name)) }
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(perfect.len(), keys.len(), "{name}");
            assert_eq!(
                perfect.has_fingerprints(),
                name.ends_with("-fp.bmp"),
                "{name}"
            );
            for key in keys.iter().take(50) {
                let id = perfect.id(key).unwrap_or_else(|| panic!("{name}: {key:?}"));
                assert_eq!(perfect.key(id), Some(key.as_str()), "{name}");
                assert_eq!(perfect.id(&format!("{key}~")), None, "{name}");
            }
        }
    }

    /// The 2.0 arena with an overflow entry: the same 1000 keys plus one of 300 bytes, which puts
    /// its block's offsets behind the data instead of widening every block. It maps zero-copy,
    /// every key answers, and the long one reads back whole.
    #[cfg(feature = "mmap")]
    #[test]
    fn the_overflow_blob_loads_zero_copy_and_reads_its_long_key() {
        let keys = keys();
        let long = "x".repeat(300);
        let path = data("golden-2.0.0-perfect-overflow.bmp");
        // SAFETY: a committed blob, and nothing in this process writes to it while mapped.
        let perfect = unsafe { lexindex::PerfectHashIndex::load_mmap(path) }.unwrap();
        assert_eq!(perfect.len(), keys.len() + 1);
        assert!(!perfect.has_fingerprints());
        for key in keys.iter().chain(std::iter::once(&long)) {
            let id = perfect.id(key).unwrap_or_else(|| panic!("{key:?}"));
            assert_eq!(perfect.key(id), Some(key.as_str()));
            assert_eq!(perfect.id(&format!("{key}~")), None);
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
    let compact = std::fs::read(data("golden-2.0.0-compact.bch")).unwrap();
    let perfect = std::fs::read(data("golden-2.0.0-perfect.bmp")).unwrap();
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
    assert!(lexindex::fuzzing::inspect(&compact) && lexindex::fuzzing::inspect(&perfect));

    // The 1.0 and 1.1 pairs stopped being seeds when 2.0 replaced the key hash: refused at the
    // magic, one branch in, like the 0.9.1 blob below.
    for name in [
        "golden-1.0.0-perfect.bmp",
        "golden-1.1.0-perfect.bmp",
        "golden-1.0.0-compact.bch",
        "golden-1.1.0-compact.bch",
    ] {
        let blob = std::fs::read(data(name)).unwrap();
        assert!(
            !lexindex::fuzzing::parse_perfect_frame(&blob, false),
            "{name}"
        );
        assert!(
            !lexindex::fuzzing::parse_compact_frame(&blob, false),
            "{name}"
        );
    }
    // And the two encodings 2.0 added: the fingerprinted arena, and the one with an overflow
    // table behind its data.
    for name in [
        "golden-2.0.0-perfect-fp.bmp",
        "golden-2.0.0-perfect-overflow.bmp",
    ] {
        let blob = std::fs::read(data(name)).unwrap();
        assert!(
            lexindex::fuzzing::parse_perfect_frame(&blob, true),
            "{name}"
        );
    }

    // The closed index's blob, through its own target; the other hash blobs are refused at the
    // magic, and its blob at theirs.
    let closed = std::fs::read(data("golden-2.0.0-closed.bcl")).unwrap();
    assert!(lexindex::fuzzing::parse_closed_frame(&closed));
    assert!(!lexindex::fuzzing::parse_closed_frame(&compact));
    assert!(!lexindex::fuzzing::parse_closed_frame(&perfect));
    assert!(!lexindex::fuzzing::parse_compact_frame(&closed, true));
    assert!(!lexindex::fuzzing::parse_perfect_frame(&closed, true));

    // The dictionary blob, through its own target -- loaded and queried -- and refused by the
    // framing parsers, as their blobs are by it. The `BDX1` file stays a seed as well: it is
    // refused at the magic now, which is a path a mutation can still reach.
    let dict = std::fs::read(data("golden-2.2.0-dict.bdx")).unwrap();
    assert!(lexindex::fuzzing::load_dict(&dict));
    assert!(!lexindex::fuzzing::load_dict(&closed));
    assert!(!lexindex::fuzzing::parse_closed_frame(&dict));
    assert!(!lexindex::fuzzing::load_dict(
        &std::fs::read(data("golden-2.0.0-dict.bdx")).unwrap()
    ));

    // The overlay seeds, through both of their targets: the frame-only one and the one that also
    // parses the embedded base. `OVL1` matters as much as `OVL2` here -- it is the format without
    // checksums, so it is the one a mutation can still reach the framing through.
    for name in ["golden-1.0.0-overlay.ovl", "golden-0.12.0-overlay.ovl"] {
        let blob = std::fs::read(data(name)).unwrap();
        assert!(
            lexindex::fuzzing::parse_overlay_frame(&blob),
            "{name}: overlay frame rejected"
        );
        assert!(
            lexindex::fuzzing::parse_overlay_untrusted(&blob),
            "{name}: overlay with an untrusted base rejected"
        );
    }

    // A pre-1.0 blob is refused at the magic, so it is worth nothing as a seed. Pinned so that the
    // seeds cannot silently go stale again the next time a format changes.
    let old = std::fs::read(data("golden-0.9.1-compact.bch")).unwrap();
    assert!(!lexindex::fuzzing::parse_compact_frame(&old, false));

    // The standalone MPH seeds, one per readable format. `parse_mphf` starts inside the format
    // the other two only reach behind their own header, so without these files its target would
    // have nothing to mutate: each header is a handful of scalars, a checksum and a length
    // identity, and no blob written for another format gets past the magic.
    for (name, magic) in [
        ("golden-1.0.0-mphf.bin", &b"MPH1"[..]),
        ("golden-1.1.0-mphf.bin", &b"MPH2"[..]),
        ("golden-2.0.0-mphf.bin", &b"MPH2"[..]),
    ] {
        let mphf = std::fs::read(data(name)).unwrap();
        assert_eq!(&mphf[..4], magic, "{name}");
        assert!(
            lexindex::fuzzing::parse_mphf(&mphf),
            "{name}: MPH seed rejected"
        );
    }
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
    // The path forms are the same check over the file and over a mapping of it.
    assert!(matches!(
        lexindex::StringIndex::load_untrusted(data("panicking-1.0.0-string.bix")),
        Err(lexindex::IndexError::Format(_))
    ));
    #[cfg(feature = "mmap")]
    {
        // SAFETY: committed test data; nothing writes it while the map is alive.
        let mapped = unsafe {
            lexindex::StringIndex::load_mmap_untrusted(data("panicking-1.0.0-string.bix"))
        };
        assert!(matches!(mapped, Err(lexindex::IndexError::Format(_))));
    }
}

/// The dictionary blob of 2.0, pinned byte for byte like the hash blobs: the symbol table is
/// trained deterministically and the layout has no seed, so a fresh build over the golden keys
/// is the file. It loads, answers every key with its rank and every stranger with `None`, and
/// gives every key back in order.
/// `BDX1` and `BDX2` held the same keys in a shape this version cannot read: `BDX1` put an entry's
/// header immediately before its suffix, and neither split a block into microblocks. The refusal
/// has to say so and that the fix is to rebuild -- the same promise the hash blobs are held to
/// above.
#[test]
fn an_older_dictionary_blob_is_refused_by_name() {
    let path = data("golden-2.0.0-dict.bdx");
    let stored = std::fs::read(&path).unwrap();
    assert_eq!(&stored[..4], b"BDX1", "the refused fixture was regenerated");
    let err = match lexindex::DictIndex::load(&path) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a BDX1 blob was accepted"),
    };
    assert!(err.contains("older lexindex"), "{err}");
    assert!(err.contains("rebuild"), "{err}");
    assert!(lexindex::DictIndex::from_bytes(&stored).is_err());
    // `BDX2` never left a release, so there is no fixture for it; the magic is refused all the
    // same, which is what a blob from a development checkout meets.
    let mut two = std::fs::read(data("golden-2.2.0-dict.bdx")).unwrap();
    two[..4].copy_from_slice(b"BDX2");
    let err = lexindex::DictIndex::from_bytes(&two)
        .unwrap_err()
        .to_string();
    assert!(err.contains("older lexindex"), "{err}");
}

#[test]
fn the_dict_blob_is_byte_identical_to_a_fresh_build_and_answers_every_key() {
    let keys = keys();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    let fresh = lexindex::DictIndex::build(&keys).unwrap().to_bytes();
    let path = data("golden-2.2.0-dict.bdx");
    let stored = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&fresh[..4], b"BDX3");
    assert!(
        stored == fresh,
        "golden-2.2.0-dict.bdx changed; regenerate {}",
        path.display()
    );
    let idx = lexindex::DictIndex::load(&path).unwrap();
    assert_eq!(idx.len(), sorted.len());
    for (rank, key) in sorted.iter().enumerate() {
        assert_eq!(idx.id(key), Some(rank as u64), "{key:?}");
        assert_eq!(
            idx.key(rank as u64).as_deref(),
            Some(key.as_str()),
            "{rank}"
        );
    }
    for stranger in non_members() {
        assert_eq!(idx.id(&stranger), None, "{stranger:?}");
    }
    let walked: Vec<String> = idx.iter().map(|(k, _)| k).collect();
    assert_eq!(walked, sorted);
}

/// `inspect` names every blob in `tests/data/` from its header alone, and refuses the pre-1.0
/// hash blobs the way the loaders do: by the type to rebuild, not as corrupt.
#[test]
fn every_golden_blob_inspects_from_its_header() {
    use lexindex::{BlobKind, inspect, inspect_file};
    for v in VERSIONS {
        let i = inspect_file(data(&format!("golden-{v}-string.bix"))).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys, i.bytes),
            (BlobKind::StringIndex, "BIX4", Some(1000), 1180),
            "{v}"
        );
        let err = inspect_file(data(&format!("golden-{v}-perfect.bmp"))).unwrap_err();
        let err = err.to_string();
        assert!(
            err.contains("< 2.0") && err.contains("PerfectHashIndex::build"),
            "{v}: {err}"
        );
        let err = inspect_file(data(&format!("golden-{v}-compact.bch"))).unwrap_err();
        let err = err.to_string();
        assert!(
            err.contains("< 2.0") && err.contains("CompactHashIndex::build"),
            "{v}: {err}"
        );
    }
    // The fingerprinted arena of 2.0: the same perfect hash, one byte per arena slot more.
    let fingerprinted = inspect_file(data("golden-2.0.0-perfect-fp.bmp")).unwrap();
    let plain = inspect_file(data("golden-2.0.0-perfect.bmp")).unwrap();
    assert_eq!(
        (
            fingerprinted.kind,
            fingerprinted.format.as_str(),
            fingerprinted.keys,
            fingerprinted.mph_bytes,
            fingerprinted.side_entries,
            fingerprinted.arena_bytes,
        ),
        (
            BlobKind::PerfectHashIndex,
            "BMP7",
            Some(1000),
            plain.mph_bytes,
            Some(0),
            plain.arena_bytes.map(|b| b + 1008), // 63 blocks of 16 slots, one byte each
        )
    );
    // The overflow arena of 2.0: one 300-byte key more, its block's offsets behind the data — the
    // same 63 blocks, plus the key, a 68-byte entry and an 8-byte trailer.
    let overflow = inspect_file(data("golden-2.0.0-perfect-overflow.bmp")).unwrap();
    assert_eq!(
        (
            overflow.format.as_str(),
            overflow.keys,
            overflow.side_entries,
            overflow.arena_bytes,
        ),
        (
            "BMP7",
            Some(1001),
            Some(0),
            plain.arena_bytes.map(|b| b + 300 + 68 + 8),
        )
    );
    // The closed index of 2.0: the same perfect hash over the same keys as the 1.1 hash blobs,
    // and nothing else -- 36 bytes of header, then the `MPH2` region.
    let closed = inspect_file(data("golden-2.0.0-closed.bcl")).unwrap();
    assert_eq!(
        (
            closed.kind,
            closed.format.as_str(),
            closed.keys,
            closed.mph_bytes,
            closed.side_entries,
            closed.arena_bytes,
            closed.fingerprint_bits,
            closed.bytes,
        ),
        (
            BlobKind::ClosedHashIndex,
            "BCL1",
            Some(1000),
            plain.mph_bytes,
            Some(0),
            None,
            None,
            36 + plain.mph_bytes.unwrap(),
        )
    );
    // The dictionary of 2.0: 48 bytes of header, the symbol table, the keys and the arrays.
    let dict = inspect_file(data("golden-2.0.0-dict.bdx")).unwrap();
    assert_eq!(
        (
            dict.kind,
            dict.format.as_str(),
            dict.keys,
            dict.mph_bytes,
            dict.side_entries,
            dict.fingerprint_bits,
        ),
        (BlobKind::DictIndex, "BDX1", Some(1000), None, None, None)
    );
    assert!(dict.arena_bytes.unwrap() < dict.bytes);
    // The hash blobs of 1.0 and 1.1 are refused the way the loaders refuse them — old, not
    // corrupt — while their standalone perfect-hash tables still inspect: a table is keyed on
    // nothing but the hashes it was handed.
    for (v, mphf, mph) in [("1.0.0", "MPH1", 521), ("1.1.0", "MPH2", 429)] {
        for (kind, ext) in [
            ("PerfectHashIndex", "perfect.bmp"),
            ("CompactHashIndex", "compact.bch"),
        ] {
            let err = inspect_file(data(&format!("golden-{v}-{ext}"))).unwrap_err();
            let err = err.to_string();
            assert!(err.contains("< 2.0") && err.contains(kind), "{v}: {err}");
        }
        let i = inspect_file(data(&format!("golden-{v}-mphf.bin"))).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys, i.mph_bytes, i.bytes),
            (BlobKind::Mphf, mphf, Some(1000), Some(mph), mph),
            "{v}"
        );
    }
    // The 2.0 pair and the table inside them: one perfect hash, no side entries over these keys,
    // and the arena of a `CompactHashIndex` at its default width is one byte per key.
    let mph = plain.mph_bytes.unwrap();
    assert_eq!(
        (
            plain.kind,
            plain.keys,
            plain.side_entries,
            plain.arena_bytes
        ),
        (
            BlobKind::PerfectHashIndex,
            Some(1000),
            Some(0),
            Some(plain.bytes - 36 - mph)
        )
    );
    let i = inspect_file(data("golden-2.0.0-compact.bch")).unwrap();
    assert_eq!(
        (
            i.kind,
            i.format.as_str(),
            i.keys,
            i.fingerprint_bits,
            i.mph_bytes,
            i.arena_bytes,
            i.side_entries
        ),
        (
            BlobKind::CompactHashIndex,
            "BCH7",
            Some(1000),
            Some(8),
            Some(mph),
            Some(1000),
            Some(0)
        )
    );
    let i = inspect_file(data("golden-2.0.0-mphf.bin")).unwrap();
    assert_eq!(
        (i.kind, i.format.as_str(), i.keys, i.mph_bytes, i.bytes),
        (BlobKind::Mphf, "MPH2", Some(1000), Some(mph), mph)
    );
    // Both overlay formats: three keys added and three retired over the 1000-key ordered base.
    for (name, format) in [
        ("golden-0.12.0-overlay.ovl", "OVL1"),
        ("golden-1.0.0-overlay.ovl", "OVL2"),
    ] {
        let i = inspect_file(data(name)).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys),
            (BlobKind::Overlay, format, Some(1000)),
            "{name}"
        );
        let o = i.overlay.unwrap();
        assert_eq!((o.base_tag, o.additions, o.retired), (1, 3, 3), "{name}");
        let base = o.base.unwrap();
        assert_eq!(
            (base.kind, base.format.as_str(), base.keys, base.bytes),
            (BlobKind::StringIndex, "BIX4", Some(1000), 1180),
            "{name}"
        );
    }
    // The header is all it reads: the two specimens the owned loader panics on inspect without
    // one, and the bytes say what the file says.
    for name in ["panicking-1.0.0-string.bix", "panicking-1.0.0-overlay.ovl"] {
        let bytes = std::fs::read(data(name)).unwrap();
        assert_eq!(
            inspect(&bytes).map_err(|e| e.to_string()),
            inspect_file(data(name)).map_err(|e| e.to_string()),
            "{name}"
        );
    }
}
