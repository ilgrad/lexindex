//! Property-based tests: the rank-walk `id <-> key` round-trip invariants, and deserialiser
//! robustness — `from_bytes` on arbitrary bytes (`StringIndex`, whose loader is safe) or on a
//! corrupted self-produced blob (all three) must fail cleanly, never panic.

use lexindex::{DictIndex, Overlay, StringIndex};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn distinct_sorted(mut keys: Vec<String>) -> Vec<String> {
    keys.sort();
    keys.dedup();
    keys
}

/// A multibyte alphabet: `é` (2 bytes), `中` (3 bytes), `🎉` (4 bytes) stress the UTF-8 paths that a
/// pure-ASCII regex would never reach.
fn multibyte_keys() -> impl Strategy<Value = Vec<String>> {
    let key = prop::collection::vec(
        prop::sample::select(vec!['a', 'b', 'z', 'à', 'é', 'Ω', '中', '🎉']),
        0..6,
    )
    .prop_map(|cs| cs.into_iter().collect::<String>());
    prop::collection::vec(key, 0..40)
}

/// The query's characters appear in `haystack` in order, not necessarily contiguously.
fn is_char_subsequence(query: &str, haystack: &str) -> bool {
    let mut q = query.chars().peekable();
    for c in haystack.chars() {
        if q.peek() == Some(&c) {
            q.next();
        }
    }
    q.peek().is_none()
}

fn check_string_index_roundtrip(keys: &[String]) {
    let idx = StringIndex::build(keys).unwrap();
    let expected = distinct_sorted(keys.to_vec());
    assert_eq!(idx.len(), expected.len());
    for (rank, key) in expected.iter().enumerate() {
        let id = rank as u64;
        assert_eq!(idx.id(key), Some(id), "id({key:?})");
        // id -> key is the rank-walk over the FST, with no stored reverse map
        assert_eq!(idx.key(id).as_deref(), Some(key.as_str()), "key({id})");
    }
    assert_eq!(idx.key(expected.len() as u64), None); // one past the end
    // a serialise round-trip preserves every lookup
    let restored = StringIndex::from_bytes(&idx.to_bytes()).unwrap();
    for (rank, key) in expected.iter().enumerate() {
        assert_eq!(restored.id(key), Some(rank as u64));
        assert_eq!(restored.key(rank as u64).as_deref(), Some(key.as_str()));
    }
}

/// Every key's id is its rank and comes back as the key, every stranger built from a key -- a
/// character more, a character less, a NUL appended -- is absent with the lower bound the sorted
/// list gives, and a serialise round-trip changes nothing; at three block sizes, so that the
/// front-coded entries, the block heads and the single-key blocks are all exercised.
fn check_dict_index_roundtrip(keys: &[String]) {
    let expected = distinct_sorted(keys.to_vec());
    let mut probes: Vec<String> = Vec::new();
    for key in &expected {
        probes.push(format!("{key}a"));
        probes.push(format!("{key}\0"));
        let mut shorter = key.clone();
        shorter.pop();
        probes.push(shorter);
    }
    for block in [1, 3, 32] {
        let idx = DictIndex::build_with_block(keys, block).unwrap();
        let restored = DictIndex::from_bytes(&idx.to_bytes()).unwrap();
        for idx in [&idx, &restored] {
            assert_eq!(idx.len(), expected.len());
            for (rank, key) in expected.iter().enumerate() {
                let id = rank as u64;
                assert_eq!(idx.id(key), Some(id), "id({key:?}) at block {block}");
                assert_eq!(idx.lower_bound(key), id, "lower_bound({key:?})");
                assert_eq!(idx.key(id).as_deref(), Some(key.as_str()), "key({id})");
            }
            assert_eq!(idx.key(expected.len() as u64), None);
            for probe in &probes {
                let at = expected.partition_point(|k| k.as_bytes() < probe.as_bytes());
                let member = expected.get(at).is_some_and(|k| k == probe);
                assert_eq!(idx.id(probe), member.then_some(at as u64), "id({probe:?})");
                assert_eq!(idx.lower_bound(probe), at as u64, "lower_bound({probe:?})");
            }
            let walked: Vec<String> = idx.iter().map(|(k, _)| k).collect();
            assert_eq!(walked, expected);
        }
    }
}

#[derive(Debug, Clone)]
enum Op {
    Add(String),
    Remove(String),
}

impl Op {
    fn key(&self) -> &str {
        match self {
            Op::Add(k) | Op::Remove(k) => k,
        }
    }
}

/// A base and a sequence of edits over the *same* tiny alphabet, so additions collide with base
/// keys, removals hit live keys, and re-additions of removed keys all happen often rather than by
/// luck.
fn overlay_ops() -> impl Strategy<Value = (Vec<String>, Vec<Op>)> {
    let key = || "[ab]{0,4}".prop_map(String::from);
    (
        prop::collection::vec(key(), 0..12),
        prop::collection::vec(
            prop_oneof![key().prop_map(Op::Add), key().prop_map(Op::Remove)],
            0..40,
        ),
    )
}

/// The overlay against a `BTreeSet` applying the same edits.
///
/// The plan's phrasing was "overlay ≡ the index rebuilt from the same operation sequence", and that
/// is not checkable as written: a rebuilt `StringIndex` numbers by sorted rank, so its ids are a
/// different numbering by construction. What is checkable, and is what a caller actually relies on,
/// is that the *key set* matches, that `key(id(k)) == k` for every live key, and that an id once
/// issued never comes to mean a different key.
fn check_overlay_matches_a_set(initial: &[String], ops: &[Op]) {
    let mut ov = Overlay::new(StringIndex::build(initial).unwrap());
    let mut model: BTreeSet<String> = initial.iter().cloned().collect();
    let mut issued: BTreeMap<u64, String> = (0..ov.id_space())
        .map(|id| {
            (
                id,
                ov.key(id).expect("every base id is live before any edit"),
            )
        })
        .collect();
    let universe: BTreeSet<String> = initial
        .iter()
        .cloned()
        .chain(ops.iter().map(|o| o.key().to_owned()))
        .collect();

    for op in ops {
        match op {
            Op::Add(k) => {
                let id = ov.add(k);
                // `or_insert` and not `insert`: an id handed back for a revived or already-present
                // key must keep the key it was issued for, which is the invariant being tested.
                issued.entry(id).or_insert_with(|| k.clone());
                model.insert(k.clone());
            }
            Op::Remove(k) => {
                assert_eq!(ov.remove(k), model.remove(k), "remove({k:?})");
            }
        }
        assert_eq!(ov.len(), model.len(), "len after {op:?}");
        for k in &universe {
            assert_eq!(
                ov.contains(k),
                model.contains(k),
                "contains({k:?}) after {op:?}"
            );
            if let Some(id) = ov.id(k) {
                assert_eq!(ov.key(id).as_deref(), Some(k.as_str()), "key(id({k:?}))");
            }
        }
        for (&id, k) in &issued {
            match ov.key(id) {
                Some(back) => assert_eq!(&back, k, "id {id} came to mean a different key"),
                None => assert!(!model.contains(k), "live key {k:?} lost its id {id}"),
            }
        }
    }

    assert_eq!(ov.keys().into_iter().collect::<BTreeSet<_>>(), model);

    // A blob has to survive the edits, not just the key set: the retired ids are what decides
    // where the next addition lands, and they exist nowhere else in the file.
    let blob = ov.to_bytes().unwrap();
    let back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes).unwrap();
    assert_eq!(back.len(), ov.len());
    assert_eq!(back.id_space(), ov.id_space());
    for k in &universe {
        assert_eq!(back.id(k), ov.id(k), "id({k:?}) after a round trip");
    }
    for id in 0..ov.id_space() {
        assert_eq!(back.key(id), ov.key(id), "key({id}) after a round trip");
    }

    let compacted = ov.compact().unwrap();
    assert_eq!(compacted.len(), model.len());
    assert_eq!(compacted.keys().into_iter().collect::<BTreeSet<_>>(), model);
    for k in &model {
        let id = compacted.id(k).expect("every live key survives a compact");
        assert_eq!(compacted.key(id).as_deref(), Some(k.as_str()));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn overlay_matches_a_set_through_every_edit((initial, ops) in overlay_ops()) {
        check_overlay_matches_a_set(&initial, &ops);
    }

    // Prefix-nested keys over a 2-symbol alphabet: many keys are prefixes of others (the hardest
    // case for the rank-walk, where a node is both final and has out-transitions).
    #[test]
    fn string_index_roundtrip_prefix_nested(keys in prop::collection::vec("[ab]{0,6}", 0..40)) {
        check_string_index_roundtrip(&keys);
    }

    #[test]
    fn string_index_roundtrip_multibyte(keys in multibyte_keys()) {
        check_string_index_roundtrip(&keys);
    }

    #[test]
    fn dict_index_roundtrip_prefix_nested(keys in prop::collection::vec("[ab]{0,6}", 0..40)) {
        check_dict_index_roundtrip(&keys);
    }

    #[test]
    fn dict_index_roundtrip_multibyte(keys in multibyte_keys()) {
        check_dict_index_roundtrip(&keys);
    }

    // The subsequence automaton must agree, key for key, with the character-level reference over
    // an alphabet where characters are 1-4 bytes long and share leading bytes (`à`/`é` both start
    // `C3`, `Ω`'s second byte is `é`'s second byte).
    #[test]
    fn subsequence_matches_the_character_reference(
        keys in multibyte_keys().prop_filter("non-empty", |k| !k.is_empty()),
        query in prop::collection::vec(
            prop::sample::select(vec!['a', 'z', 'à', 'é', 'Ω', '中', '🎉']),
            0..3,
        ).prop_map(|cs| cs.into_iter().collect::<String>()),
    ) {
        let idx = StringIndex::build(&keys).unwrap();
        let got: Vec<String> = idx.subsequence(&query).into_iter().map(|(k, _)| k).collect();
        let want: Vec<String> = distinct_sorted(keys.clone())
            .into_iter()
            .filter(|k| is_char_subsequence(&query, k))
            .collect();
        prop_assert_eq!(got, want);
    }

    // Arbitrary bytes must never panic or read out of bounds — only `Ok`/`Err`.
    #[test]
    fn string_index_from_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = StringIndex::from_bytes(&data);
    }

    // The strict loader has to hold to the same on arbitrary bytes, and to more: it is the one
    // that promises an `Err` where `from_bytes` is allowed to panic.
    #[test]
    fn string_index_from_untrusted_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = StringIndex::from_untrusted_bytes(&data);
    }

    // Random bytes almost never reach `fst`'s node decoder — they fail the magic, and past that the
    // checksum. Mutating a *real* blob is what gets there: flip one byte inside the FST body and
    // re-seal nothing, and the CRC usually catches it, but the surviving cases are exactly the
    // class that produced the 44-byte panic `fuzz/Cargo.toml` records. Whatever the mutation, the
    // strict loader must return, and whatever it returns must be self-consistent: an index that
    // loads has to answer its own keys.
    #[test]
    fn string_index_untrusted_survives_seeded_mutations(
        keys in prop::collection::vec("[ab]{0,5}", 1..12),
        at in any::<prop::sample::Index>(),
        xor in 1u8..=255,
    ) {
        let keys = distinct_sorted(keys);
        let mut blob = StringIndex::build(keys.iter()).unwrap().to_bytes();
        let i = at.index(blob.len());
        blob[i] ^= xor;
        if let Ok(idx) = StringIndex::from_untrusted_bytes(&blob) {
            let mut seen = 0u64;
            for (rank, key) in idx.iter().enumerate() {
                prop_assert_eq!(key.1, rank as u64);
                seen += 1;
            }
            prop_assert_eq!(seen, idx.len() as u64);
        }
    }

    // No false rejections at scale: whatever `to_bytes` writes, the strict loader takes, and the
    // index it hands back answers exactly what the original did.
    #[test]
    fn string_index_untrusted_round_trip(keys in multibyte_keys()) {
        let keys = distinct_sorted(keys);
        let idx = StringIndex::build(keys.iter()).unwrap();
        let back = StringIndex::from_untrusted_bytes(&idx.to_bytes())
            .map_err(|e| TestCaseError::fail(format!("refused its own blob: {e}")))?;
        prop_assert_eq!(back.len(), keys.len());
        for (rank, key) in keys.iter().enumerate() {
            prop_assert_eq!(back.id(key), Some(rank as u64));
        }
    }

    // `fst` takes any byte strings; the strict loader takes exactly the transducers whose every key
    // is UTF-8, and hands back their ranks. The alphabet is the interesting bytes: ASCII, the
    // continuation range's edges, lead bytes with a restricted second byte, and two beyond range.
    #[test]
    fn string_index_untrusted_accepts_exactly_the_utf8_key_sets(
        keys in prop::collection::vec(
            prop::collection::vec(
                prop::sample::select(vec![
                    0x61u8, 0x7f, 0x80, 0x8f, 0x9f, 0xa0, 0xbf, 0xc2, 0xc3, 0xe0, 0xed, 0xef, 0xf0,
                    0xf4, 0xff,
                ]),
                0..5,
            ),
            0..8,
        ),
    ) {
        let mut keys = keys;
        keys.sort();
        keys.dedup();
        let mut b = fst::MapBuilder::memory();
        for (rank, key) in keys.iter().enumerate() {
            b.insert(key, rank as u64).unwrap();
        }
        let mut blob = b"BIX4".to_vec();
        blob.extend_from_slice(&b.into_inner().unwrap());
        let all_utf8 = keys.iter().all(|k| std::str::from_utf8(k).is_ok());
        match StringIndex::from_untrusted_bytes(&blob) {
            Ok(idx) => {
                prop_assert!(all_utf8, "loaded a transducer with a non-UTF-8 key: {:02x?}", keys);
                prop_assert_eq!(idx.len(), keys.len());
                for (rank, key) in keys.iter().enumerate() {
                    let key = std::str::from_utf8(key).unwrap();
                    prop_assert_eq!(idx.id(key), Some(rank as u64));
                    let back = idx.key(rank as u64);
                    prop_assert_eq!(back.as_deref(), Some(key));
                }
            }
            Err(e) => prop_assert!(!all_utf8, "refused a UTF-8 transducer {:02x?}: {}", keys, e),
        }
    }

    // Random bytes never spell `OVL2`, so the overlay's framing is fuzzed outward from a real blob:
    // one byte flipped, or the blob cut short, or one of its four claimed lengths replaced by a
    // hostile value. Only `Ok`/`Err` — never a panic, and in particular never an allocation sized
    // by a number the blob merely claims (`1 << 61` tombstone words used to be an overflow panic in
    // debug and a wrapped length check in release). The hostile lengths are written *without*
    // re-sealing the header, so most of them are caught by the checksum — the point here is that
    // the parser survives them, and the messages they would otherwise reach are pinned by name in
    // the unit tests.
    //
    // The base blob is deliberately not re-parsed: the loader closure returns the same fixed base
    // the blob was written over. That is `StringIndex::from_bytes`'s own property, pinned two tests
    // above, and parsing mutated FST bytes here would only rediscover the `fst` node-decoder panic
    // that `fuzz/Cargo.toml` records as out of this crate's scope.
    #[test]
    fn overlay_framing_survives_seeded_mutations(
        additions in prop::collection::vec("[cd]{0,4}", 0..6),
        removals in prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "d"]), 0..4),
        at in any::<prop::sample::Index>(),
        xor in 1u8..=255,
        kind in 0u8..6,
        hostile in prop::sample::select(vec![0u64, 1, 7, u32::MAX as u64, u64::MAX, 1 << 61, 1 << 62]),
    ) {
        let base = || StringIndex::build(["a", "b"]);
        let mut ov = Overlay::new(base().unwrap());
        for a in &additions {
            ov.add(a);
        }
        for r in &removals {
            ov.remove(r);
        }
        let mut blob = ov.to_bytes().unwrap();
        prop_assert!(Overlay::from_bytes_with(&blob, |_| base()).is_ok(), "the unmutated blob must load");

        match kind {
            0 => {
                let pos = at.index(blob.len());
                blob[pos] ^= xor;
            }
            1 => {
                let pos = at.index(blob.len());
                blob.truncate(pos);
            }
            // The base blob's length, the addition count, the addition region's length and the
            // tombstone word count: the four numbers the parser must never trust into an index or
            // an allocation.
            2 => blob[5..13].copy_from_slice(&hostile.to_le_bytes()),
            3 => blob[13..21].copy_from_slice(&hostile.to_le_bytes()),
            4 => blob[21..29].copy_from_slice(&hostile.to_le_bytes()),
            _ => blob[29..37].copy_from_slice(&hostile.to_le_bytes()),
        }
        // The verdict is not the point; surviving the parse is.
        let _ = Overlay::from_bytes_with(&blob, |_| base());
    }

    // A single flipped byte in a real blob must be *rejected* by an owned load, not merely survive
    // it. Owned `from_bytes` runs the FST's CRC-32 checksum, which detects every single-byte error
    // (a ≤8-bit burst) with certainty; a flip in the magic or the framing fails even earlier. So the
    // guarantee is stronger than "no panic" — it is a clean `Err`, with no corrupt index ever handed
    // back to be queried. (`load_mmap` deliberately skips this scan; it is not exercised here.)
    #[test]
    fn string_index_corrupt_blob_is_rejected(
        keys in prop::collection::vec("[ab]{0,6}", 1..30),
        at in any::<prop::sample::Index>(),
        xor in 1u8..=255,
    ) {
        let mut blob = StringIndex::build(&keys).unwrap().to_bytes();
        let pos = at.index(blob.len());
        blob[pos] ^= xor;
        prop_assert!(
            StringIndex::from_bytes(&blob).is_err(),
            "single-byte flip at {pos} (xor {xor}) was accepted by an owned load",
        );
    }
}

#[cfg(feature = "mph")]
mod mph {
    use super::{distinct_sorted, multibyte_keys};
    use lexindex::{CompactHashIndex, PerfectHashIndex};
    use proptest::prelude::*;

    fn check_perfect_hash_roundtrip(keys: &[String]) {
        let idx = PerfectHashIndex::build(keys).unwrap();
        let expected = distinct_sorted(keys.to_vec());
        assert_eq!(idx.len(), expected.len());
        let mut seen = vec![false; expected.len()];
        for key in &expected {
            let id = idx.id(key).expect("member is present") as usize;
            assert!(id < expected.len());
            assert!(!seen[id], "ids must be a bijection onto [0, n)");
            seen[id] = true;
            assert_eq!(idx.key(id as u32), Some(key.as_str())); // exact reverse
        }
        // SAFETY: the blob comes straight from this index's own `to_bytes`.
        let restored = PerfectHashIndex::from_bytes(&idx.to_bytes().unwrap()).unwrap();
        for key in &expected {
            assert_eq!(restored.id(key), idx.id(key));
        }
    }

    fn check_compact_no_false_negative(keys: &[String], fp: usize) {
        let idx = CompactHashIndex::build(keys, fp).unwrap();
        let expected = distinct_sorted(keys.to_vec());
        assert_eq!(idx.len(), expected.len());
        for key in &expected {
            // membership is probabilistic only for *non*-members; a member is never a false negative
            assert!(idx.contains(key), "false negative on member {key:?}");
            assert!((idx.id_unchecked(key) as usize) < expected.len());
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn perfect_hash_roundtrip(keys in multibyte_keys()) {
            check_perfect_hash_roundtrip(&keys);
        }

        #[test]
        fn compact_hash_no_false_negative(
            keys in multibyte_keys(),
            fp in prop::sample::select(vec![1usize, 2, 4]),
        ) {
            check_compact_no_false_negative(&keys, fp);
        }

        // The never-panics property on arbitrary bytes lives with the *safe* framing parser in each
        // index's own tests (`parse_frame_never_panics`), where the framing parser is reachable
        // without building an index first.

        // Any single flipped byte — header field, MPH region or fingerprint table — must be
        // *rejected* by an owned load: the header carries a checksum and, since 0.8, so does the
        // whole payload, so even a flipped fingerprint bit (which would only have perturbed the
        // probabilistic membership answer) fails cleanly instead of loading corrupt.
        #[test]
        fn compact_hash_corrupt_blob_is_rejected(
            keys in multibyte_keys().prop_filter("non-empty", |k| !k.is_empty()),
            fp in prop::sample::select(vec![1usize, 2, 4]),
            at in any::<prop::sample::Index>(),
            xor in 1u8..=255,
        ) {
            let idx = CompactHashIndex::build(&keys, fp).unwrap();
            let mut blob = idx.to_bytes().unwrap();
            assert_eq!(&blob[0..4], b"BCH7");
            let pos = 4 + at.index(blob.len() - 4);
            blob[pos] ^= xor;
            prop_assert!(
                CompactHashIndex::from_bytes(&blob).is_err(),
                "single-byte flip at {pos} (xor {xor}) was accepted by an owned load",
            );
        }

        /// A blob is a reproducible artefact since 1.0, which is a property of *every* key set and not
        /// of the two that happen to be in a unit test. Building twice must give the same bytes for all
        /// three indexes — the two hash ones because their MPH is deterministic, `StringIndex` because
        /// an FST over sorted keys is canonical.
        ///
        /// The thread count is the other half of the claim and is tested where threads actually enter,
        /// in `mphf`: here every build takes the default, so what this covers is the key set.
        #[test]
        fn building_twice_gives_the_same_bytes(keys in multibyte_keys(), fp in 1u32..=16) {
            let perfect = PerfectHashIndex::build(&keys).unwrap().to_bytes().unwrap();
            prop_assert_eq!(
                &perfect,
                &PerfectHashIndex::build(&keys).unwrap().to_bytes().unwrap()
            );

            let compact = CompactHashIndex::build_bits(&keys, fp).unwrap().to_bytes().unwrap();
            prop_assert_eq!(
                &compact,
                &CompactHashIndex::build_bits(&keys, fp).unwrap().to_bytes().unwrap()
            );

            let string = lexindex::StringIndex::build(&keys).unwrap().to_bytes();
            prop_assert_eq!(&string, &lexindex::StringIndex::build(&keys).unwrap().to_bytes());
        }
    }
}

proptest! {
    /// The four order statistics against the obvious model: a sorted `Vec` searched by hand.
    ///
    /// The oracle is deliberately naive -- `partition_point`, a linear prefix filter -- because the
    /// implementation's whole claim is that it reaches the same answers without touching the keys.
    /// Multibyte keys are in the alphabet on purpose: the ids are assigned in *byte* order, and a
    /// prefix range's exclusive end is built by incrementing a byte, so a `char`-shaped assumption
    /// anywhere in either would show up here as a disagreement on `é`, `中` or `🎉`.
    #[test]
    fn order_statistics_agree_with_a_sorted_vec(
        keys in multibyte_keys(),
        probes in prop::collection::vec(
            prop::collection::vec(
                prop::sample::select(vec!['a', 'b', 'z', 'à', 'é', 'Ω', '中', '🎉']), 0..4)
                .prop_map(|cs| cs.into_iter().collect::<String>()),
            1..12),
    ) {
        let idx = StringIndex::build(&keys).unwrap();
        let sorted = distinct_sorted(keys.clone());

        for q in &probes {
            let expected = sorted.partition_point(|k| k.as_str() < q.as_str()) as u64;
            prop_assert_eq!(idx.lower_bound(q), expected, "lower_bound({:?})", q);

            let matching: Vec<&String> = sorted.iter().filter(|k| k.starts_with(q.as_str())).collect();
            let range = idx.prefix_id_range(q);
            prop_assert_eq!(idx.prefix_count(q), matching.len() as u64, "prefix_count({:?})", q);
            prop_assert_eq!(range.end - range.start, matching.len() as u64);
            // The range is the ids of the matches, not merely as many of them.
            for (offset, key) in matching.iter().enumerate() {
                prop_assert_eq!(idx.id(key), Some(range.start + offset as u64));
            }
        }

        for pair in probes.windows(2) {
            let (lo, hi) = (&pair[0], &pair[1]);
            let expected = sorted.iter().filter(|k| k.as_str() >= lo.as_str() && k.as_str() < hi.as_str()).count();
            prop_assert_eq!(idx.range_count(lo, hi), expected as u64, "range_count({:?}, {:?})", lo, hi);
            prop_assert_eq!(idx.range_count(lo, hi), idx.range(lo, hi).len() as u64);
        }
    }
}
