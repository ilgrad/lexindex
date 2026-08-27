//! Property-based tests: the rank-walk `id <-> key` round-trip invariants, and deserialiser
//! robustness — `from_bytes` on arbitrary or corrupted bytes must fail cleanly, never panic.

use lexindex::StringIndex;
use proptest::prelude::*;

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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

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

        #[test]
        fn perfect_hash_from_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..256)) {
            let _ = PerfectHashIndex::from_bytes(&data);
        }

        #[test]
        fn compact_hash_from_bytes_never_panics(data in prop::collection::vec(any::<u8>(), 0..256)) {
            let _ = CompactHashIndex::from_bytes(&data);
        }

        // Any single flipped byte — header field, MPH region or fingerprint table — must be
        // *rejected* by an owned load: the header carries a checksum because `overflow_cap` bounds
        // an otherwise unchecked read inside the MPH, and since BCH4 the whole payload carries one
        // too, so even a flipped fingerprint bit (which would only have perturbed the probabilistic
        // membership answer) fails cleanly instead of loading corrupt.
        #[test]
        fn compact_hash_corrupt_blob_is_rejected(
            keys in multibyte_keys().prop_filter("non-empty", |k| !k.is_empty()),
            fp in prop::sample::select(vec![1usize, 2, 4]),
            at in any::<prop::sample::Index>(),
            xor in 1u8..=255,
        ) {
            let idx = CompactHashIndex::build(&keys, fp).unwrap();
            let mut blob = idx.to_bytes().unwrap();
            assert_eq!(&blob[0..4], b"BCH4");
            let pos = 4 + at.index(blob.len() - 4);
            blob[pos] ^= xor;
            prop_assert!(
                CompactHashIndex::from_bytes(&blob).is_err(),
                "single-byte flip at {pos} (xor {xor}) was accepted by an owned load",
            );
        }
    }
}
