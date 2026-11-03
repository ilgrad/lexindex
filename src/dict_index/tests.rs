use super::build::{
    choose_code, distinct_ascending, in_ranges, sort_distinct, sort_on, strictly_ascending, view_of,
};
use super::encode::{TRAIN_BUDGET, TRAIN_BYTES};
use super::route::sample_range;
use super::*;

/// The golden keys plus the shapes the encoding has cases for: the empty key, NUL bytes,
/// multibyte characters, a prefix of another key, a shared prefix and a suffix past the
/// one-byte header's fifteen, and a key past eight bytes that shares its sample with others.
fn corpus() -> Vec<String> {
    let mut keys: Vec<String> = include_str!("../../tests/data/golden-keys.txt")
        .lines()
        .map(str::to_owned)
        .collect();
    let long = "x".repeat(20);
    keys.extend(
        [
            "",
            "a",
            "a\0",
            "a\0b",
            "é",
            "école",
            "écoles",
            "ka",
            "ka-",
            "ka-00",
            "ka-0000-and-then-some-more-bytes-than-fifteen",
            "ka-0000-and-then-some-more-bytes-than-sixteen",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    keys.push(long.clone());
    keys.push(format!("{long}a"));
    keys.push(format!("{long}b{}", "y".repeat(40)));
    keys.push(format!("{long}b{}z", "y".repeat(40)));
    keys.sort_unstable();
    keys.dedup();
    keys
}

fn probes(keys: &[String]) -> Vec<String> {
    let mut out = vec![
        String::new(),
        "\u{10FFFF}".into(),
        "kb".into(),
        "ka-00000".into(),
    ];
    for k in keys.iter().step_by(7) {
        out.push(format!("{k}x"));
        out.push(k[..k.len() - k.chars().last().map_or(0, char::len_utf8)].to_owned());
        out.push(format!("{k}\0"));
    }
    out
}

fn check(idx: &DictIndex, keys: &[String]) {
    assert_eq!(idx.len(), keys.len());
    let mut buf = String::from("scratch");
    for (rank, k) in keys.iter().enumerate() {
        let rank = rank as u64;
        assert_eq!(idx.id(k), Some(rank), "{k:?}");
        assert!(idx.contains(k), "{k:?}");
        assert_eq!(idx.lower_bound(k), rank, "{k:?}");
        assert_eq!(idx.key(rank).as_deref(), Some(k.as_str()), "{rank}");
        assert!(idx.key_into(rank, &mut buf), "{rank}");
        assert_eq!(&buf, k, "{rank}");
    }
    for p in probes(keys) {
        let expect = keys.partition_point(|k| k.as_bytes() < p.as_bytes()) as u64;
        let member = keys.get(expect as usize).is_some_and(|k| *k == p);
        assert_eq!(idx.id(&p), member.then_some(expect), "{p:?}");
        assert_eq!(idx.contains(&p), member, "{p:?}");
        assert_eq!(idx.lower_bound(&p), expect, "{p:?}");
    }
    let n = keys.len() as u64;
    assert_eq!(idx.key(n), None);
    assert_eq!(idx.key(u64::MAX), None);
    assert!(!idx.key_into(n, &mut buf) && buf.is_empty());
    let walked: Vec<(String, u64)> = idx.iter().collect();
    let expect: Vec<(String, u64)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), i as u64))
        .collect();
    assert_eq!(walked, expect);
    assert_eq!(idx.ids_of(&keys[..5]), (0..5).map(Some).collect::<Vec<_>>());
}

#[test]
fn answers_every_key_and_stranger_at_every_block_size() {
    let keys = corpus();
    for block in [1, 2, 3, 7, 32, 500, 1024] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        assert_eq!(idx.block(), block);
        check(&idx, &keys);
        let blob = idx.to_bytes();
        assert_eq!(blob.len(), idx.serialized_len(), "block {block}");
        assert_eq!(&blob[..4], b"BDX3");
        let back = DictIndex::from_bytes(&blob).unwrap();
        assert_eq!(back.to_bytes(), blob, "block {block}");
        check(&back, &keys);
    }
}

/// [`corpus`] with keys in two more scripts and an astral plane: what a character code is for,
/// beside every shape the golden keys pin.
fn scripts() -> Vec<String> {
    let mut keys = corpus();
    let stems = [
        "北京",
        "北京大学",
        "中华人民共和国",
        "中国",
        "上海",
        "москва",
        "московский",
        "мир",
        "мирный",
        "ёж",
        "яблоко",
        "𠀋",
        "𠀋𠀍",
    ];
    for (i, stem) in stems.iter().enumerate() {
        keys.push((*stem).to_owned());
        keys.push(format!("{stem}{i}"));
        keys.push(format!("{stem}-{stem}"));
    }
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// [`probes`] and the queries a code can miss: a character below, between and above those it
/// spells, alone, after a key and inside one.
fn script_probes(keys: &[String]) -> Vec<String> {
    let mut out = probes(keys);
    for miss in [
        "\u{1}",
        "~",
        "\u{80}",
        "\u{3000}",
        "丁",
        "中丁",
        "北京\u{4E00}",
        "мф",
        "м\u{10FFFF}",
        "ё",
        "я\0",
        "\u{FFFF}",
        "𠀋\u{20000}",
    ] {
        out.push(miss.to_owned());
    }
    for k in keys.iter().step_by(11) {
        out.push(format!("{k}丁"));
        out.push(format!("{k}~"));
        let mut chars: Vec<char> = k.chars().collect();
        if let Some(c) = chars.first_mut() {
            *c = char::from_u32(*c as u32 + 1).unwrap_or('\u{10FFFF}');
        }
        out.push(chars.into_iter().collect());
    }
    out
}

/// `keys` spelled in the code [`CharCode::forced`] makes of them, whatever it saves.
fn coded(keys: &[String], block: usize, seven: bool, singles: usize) -> DictIndex {
    let code = CharCode::forced(keys, seven, singles);
    let view = keys.iter().map(String::as_str).collect();
    DictIndex::from_coded(view, block, micro_for(block), code).unwrap()
}

#[test]
fn a_coded_blob_answers_every_query_a_linear_scan_does() {
    let keys = scripts();
    let ps = script_probes(&keys);
    let mut queries = keys.clone();
    queries.extend(ps.iter().cloned());
    for (seven, singles) in [(true, 0), (true, 12), (false, 0), (false, 5)] {
        for block in [1usize, 32, MAX_BLOCK] {
            let what = format!("seven {seven}, {singles} singles, block {block}");
            let idx = coded(&keys, block, seven, singles);
            let blob = idx.to_bytes();
            assert_eq!(&blob[..4], CODED_MAGIC, "{what}");
            assert_eq!(blob.len(), idx.serialized_len(), "{what}");
            let sections = idx.sections();
            assert_eq!(sections.total() as usize, blob.len(), "{what}");
            assert!(sections.chars > 0, "{what}");
            check(&idx, &keys);
            let back = DictIndex::from_bytes(&blob).unwrap();
            assert_eq!(back.to_bytes(), blob, "{what}");
            check(&back, &keys);
            scan_ordered(&back, &keys, &ps, &what);
            scan_prefixes(&back, &keys, &ps, &what);
            batch_matches(&back, &queries, &what);
            keys_of_matches(&back, &what);
            // Bytes that are no string are no key, in a coded blob as in a plain one.
            assert_eq!(
                back.ids_of_with(2, |i| [b"\xff".as_slice(), keys[3].as_bytes()][i]),
                [None, Some(3)],
                "{what}"
            );
            let mapped = DictIndex::from_shared(SharedBytes::from_owned(blob), false).unwrap();
            check(&mapped, &keys);
        }
    }
}

/// Every query placed the way [`DictIndex::locate`] places it and, where the restart words
/// answer, their microblock and shared prefix checked against the restart run's. How many
/// they answered.
fn routed_as_scanned(idx: &DictIndex, queries: &[String], what: &str) -> usize {
    let mut routed = 0;
    for query in queries {
        idx.probe(query, |probe, _| {
            let l = match sample_range(&idx.samples, idx.route(probe)) {
                Ok((lo, hi)) => idx.head_boundary(probe, lo, hi),
                Err(l) => l,
            };
            let Some(b) = l.checked_sub(1) else {
                return;
            };
            let (head, r) = (idx.head(b), idx.micros_in(b));
            if head == probe || r < 2 {
                return;
            }
            let matched = lcp(head, probe);
            let (from, shared, tied) = idx.micro_routed(b, r, probe, matched);
            let codec = idx.codec_of(b);
            let restarts = || Entries::of(&idx.codes_of(b).1, codec, idx.restart_data(b), r);
            let (j, m, hit) = idx.scan_run(codec, &restarts(), r, probe, matched);
            if tied {
                let mut rest = restarts();
                rest.pass(from);
                let (k, shared, part) = idx.scan_run(codec, &rest, r - from, probe, shared);
                assert_eq!((j, m, hit), (from + k, shared, part), "{what}: {query:?}");
            } else {
                assert_eq!((j, m, hit), (from, shared, false), "{what}: {query:?}");
                routed += 1;
            }
        });
    }
    routed
}

#[test]
fn restart_words_place_a_probe_where_the_restart_run_does() {
    // Eight bytes past a long shared prefix that hold NUL and 0x01: a word that pads a short
    // key and one that holds a zero read the same.
    let mut x = 11u32;
    let mut next = || {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (x >> 16) as usize
    };
    let mut zeros: Vec<String> = (0..6000)
        .map(|_| {
            let len = next() % 12;
            let mut k = String::from("a/long/shared/prefix/");
            k.extend((0..len).map(|_| ['\0', '\u{1}', 'a', 'b'][next() % 4]));
            k
        })
        .collect();
    zeros.sort_unstable();
    zeros.dedup();
    let (golden, cjk) = (corpus(), scripts());
    for block in [32usize, 64, DEFAULT_BLOCK, MAX_BLOCK] {
        for (name, keys) in [("golden", &golden), ("zeros", &zeros)] {
            let what = format!("{name}, block {block}");
            let idx = DictIndex::build_with_block(keys, block).unwrap();
            let blob = idx.to_bytes();
            let mut queries = keys.clone();
            queries.extend(probes(keys));
            assert_eq!(
                routed_as_scanned(&idx, &queries, &what),
                0,
                "{what}: unrouted"
            );
            let bytes = idx.route_microblocks();
            assert_eq!(bytes, idx.blocks_len() * idx.per * 8, "{what}");
            assert_eq!(idx.route_microblocks(), bytes, "{what}: routed twice");
            let routed = routed_as_scanned(&idx, &queries, &what);
            assert!(routed > 0, "{what}: the words place no query");
            check(&idx, keys);
            assert_eq!(idx.to_bytes(), blob, "{what}: the words are in the blob");
        }
        let what = format!("coded, block {block}");
        let idx = coded(&cjk, block, false, 5);
        idx.route_microblocks();
        let mut queries = cjk.clone();
        queries.extend(script_probes(&cjk));
        assert!(routed_as_scanned(&idx, &queries, &what) > 0, "{what}");
        check(&idx, &cjk);
    }
    // One microblock a block: nothing to route, and nothing held.
    let idx = DictIndex::build_with_block(&golden, 16).unwrap();
    assert_eq!(idx.route_microblocks(), 0);
    check(&idx, &golden);
}

/// Up to `n` keys of two to five characters drawn from `alphabet` CJK ideographs.
fn ideographs(n: usize, alphabet: u32) -> Vec<String> {
    let mut x = 7u32;
    let mut keys: Vec<String> = (0..n)
        .map(|i| {
            (0..2 + i % 4)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    char::from_u32(0x4E00 + (x >> 16) % alphabet).unwrap()
                })
                .collect()
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// `n` keys of two to six characters over `alphabet` ideographs, the low ones far more frequent:
/// an alphabet only `Mode::Eight` holds, and still one a code pays for.
fn skewed(n: usize, alphabet: u32) -> Vec<String> {
    let mut x = 11u64;
    let mut keys: Vec<String> = (0..n)
        .map(|i| {
            (0..2 + i % 5)
                .map(|_| {
                    x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                    let u = (x >> 11) as f64 / (1u64 << 53) as f64;
                    char::from_u32(0x4E00 + (f64::from(alphabet) * u * u * u) as u32).unwrap()
                })
                .collect()
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// A coded blob that routes on the query's own bytes answers every query as the same blob
/// routed on the query's code does, and as the sorted keys say: members, their prefixes, and
/// queries holding characters the code does not spell — below every character, between two,
/// above every one, outside the BMP, and NUL.
#[test]
fn routing_on_the_query_as_spelled_places_it_where_its_code_does() {
    let cjk = ideographs(20_000, 300);
    let mixed: Vec<String> = cjk
        .iter()
        .enumerate()
        .map(|(i, k)| match i % 5 {
            0 => format!("{k}{}", i % 97),
            1 => format!("a{k}"),
            2 => format!("{k} {k}"),
            _ => k.clone(),
        })
        .collect();
    let prefixed: Vec<String> = cjk.iter().map(|k| format!("中文{k}")).collect();
    let letter = |v: u64| char::from_u32(0x430 + (v % 32) as u32).unwrap();
    let mut h = 3u64;
    let cyrillic: Vec<String> = (0..30_000u64)
        .map(|i| {
            let word: String = (0..3 + i % 9)
                .map(|_| {
                    h = h
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1442695040888963407);
                    letter(h >> 59)
                })
                .collect();
            if i % 3 == 0 { word + " 1" } else { word }
        })
        .collect();
    // Thirty four-letter stems and a tail each: eight bytes of UTF-8 are the stem alone, so the
    // decoded samples tie in runs where the coded ones, a byte a frequent letter, do not.
    let stems: Vec<String> = (0..30_000u64)
        .map(|i| {
            let stem: String = (0..4).map(|j| letter(i % 30 * 7 + j * 5)).collect();
            let tail: String = (0..3).map(|j| letter((i / 30) >> (5 * j))).collect();
            stem + &tail
        })
        .collect();
    let absent = [
        '\u{9FA5}',
        'z',
        '\u{10FFFF}',
        '\u{1F600}',
        '\0',
        '\u{7FF}',
        '\u{FFFF}',
    ];
    let mut x = 99u32;
    let mut pick = move |m: usize| {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (x >> 8) as usize % m
    };
    for (name, mut keys, routes_raw) in [
        ("cjk", cjk.clone(), Some(true)),
        ("mixed", mixed, None),
        ("prefixed", prefixed, None),
        ("eight", skewed(300_000, 20_000), None),
        ("cyrillic", cyrillic, None),
        ("stems", stems, Some(false)),
    ] {
        keys.sort_unstable();
        keys.dedup();
        for block in [1usize, 16, DEFAULT_BLOCK] {
            let built = DictIndex::build_with_block(&keys, block).unwrap();
            let blob = built.to_bytes();
            assert_eq!(&blob[..4], CODED_MAGIC, "{name}");
            let idx = DictIndex::from_bytes(&blob).unwrap();
            assert_eq!(
                idx.raw.is_some(),
                built.raw.is_some(),
                "{name} block {block}"
            );
            if let (DEFAULT_BLOCK, Some(raw)) = (block, routes_raw) {
                assert_eq!(idx.raw.is_some(), raw, "{name}");
            }
            let mut coded = DictIndex::from_bytes(&blob).unwrap();
            coded.raw = None;
            let mut forced = DictIndex::from_bytes(&blob).unwrap();
            forced.raw = forced.raw.take().or_else(|| {
                // Coded samples that all tie make any decoded ones the better route.
                let mut plain = DictIndex::from_bytes(&blob).unwrap();
                plain.samples.iter_mut().for_each(|s| *s = 0);
                plain.raw_route()
            });
            assert!(
                forced.raw.is_some(),
                "{name} block {block}: a route can be forced"
            );
            let mut queries: Vec<String> = vec![String::new(), "\0".into(), "\u{10FFFF}".into()];
            for k in keys.iter().step_by(11) {
                queries.push(k.clone());
                let chars: Vec<char> = k.chars().collect();
                queries.extend((0..chars.len()).map(|end| chars[..end].iter().collect()));
                for &c in &absent {
                    let mut q: String = chars[..chars.len() - 1].iter().collect();
                    q.push(c);
                    queries.push(q.clone());
                    q.push_str(k);
                    queries.push(q);
                }
                let other = &keys[pick(keys.len())];
                queries.push(format!("{k}{other}"));
                queries.push(format!("{}{other}", chars[0]));
            }
            for q in &queries {
                let want = coded.locate_query(q);
                assert_eq!(idx.locate_query(q), want, "{name} block {block} {q:?}");
                assert_eq!(
                    forced.locate_query(q),
                    want,
                    "{name} block {block} forced {q:?}"
                );
                let rank = keys.partition_point(|k| k.as_str() < q.as_str()) as u64;
                let member = keys.get(rank as usize).is_some_and(|k| k == q);
                assert_eq!(want, (rank, member), "{name} block {block} truth {q:?}");
            }
            check(&idx, &keys);
        }
    }
}

/// A batch on a coded blob answers each key as `id` does, whatever the keys before it spelled
/// to: members, strangers longer and shorter than their neighbours, keys holding a character
/// the code does not spell, the empty key, and bytes that are not UTF-8.
#[test]
fn a_coded_batch_answers_each_key_as_id_does() {
    let keys = ideographs(20_000, 300);
    let idx = DictIndex::build(&keys).unwrap();
    assert!(idx.code.is_some());
    let mut queries: Vec<String> = Vec::new();
    for (i, k) in keys.iter().enumerate().step_by(3) {
        queries.push(k.clone());
        match i % 5 {
            0 => queries.push(format!("{k}\u{9FA5}")),
            1 => queries.push(format!("{k}{k}")),
            2 => queries.push(format!("\u{10FFFF}{k}")),
            3 => queries.push(String::new()),
            _ => queries.push(format!("{k}\u{1F600}")),
        }
    }
    let want: Vec<Option<u64>> = queries.iter().map(|q| idx.id(q)).collect();
    assert!(want.iter().filter(|id| id.is_some()).count() > keys.len() / 3);
    assert_eq!(idx.ids_of(&queries), want);
    let mut bytes: Vec<Vec<u8>> = queries.iter().map(|q| q.as_bytes().to_vec()).collect();
    let mut want = want;
    for (b, id) in bytes.iter_mut().zip(want.iter_mut()).step_by(7) {
        b.push(0xFF);
        *id = None;
    }
    assert_eq!(idx.ids_of_with(bytes.len(), |i| &bytes[i]), want);
}

#[test]
fn the_build_spells_its_keys_in_a_code_only_where_it_pays() {
    let plain = |keys: &[String]| {
        DictIndex::from_sorted(keys, DEFAULT_BLOCK, micro_for(DEFAULT_BLOCK))
            .unwrap()
            .serialized_len()
    };
    let cjk = ideographs(20_000, 300);
    let blob = DictIndex::build(&cjk).unwrap().to_bytes();
    assert_eq!(&blob[..4], CODED_MAGIC);
    assert!(
        blob.len() < plain(&cjk),
        "{} coded against {} plain",
        blob.len(),
        plain(&cjk)
    );
    check(&DictIndex::from_bytes(&blob).unwrap(), &cjk);
    // Two thousand ideographs over three thousand keys: a table of two kilobytes against 10.5
    // saved before the codec, under the twentieth the choice asks. The bar is conservative
    // here — this blob would have come out 4.1 % smaller coded — and what it guards against
    // is a corpus whose codec keeps less of the saving, as Russian titles keep 5 %.
    let sparse = ideographs(3000, 2000);
    let blob = DictIndex::build(&sparse).unwrap().to_bytes();
    assert_eq!(&blob[..4], MAGIC);
    assert_eq!(blob.len(), plain(&sparse));
    // The golden keys are ASCII but for four, and a code would spend bytes on them.
    let keys = corpus();
    let blob = DictIndex::build(&keys).unwrap().to_bytes();
    assert_eq!(&blob[..4], MAGIC);
    assert_eq!(
        &blob[62..68],
        [0; 6],
        "a plain blob's reserved bytes stay as 4.2 left them"
    );
}

#[test]
fn a_streamed_build_spells_its_keys_as_the_sorting_one_does() {
    let keys = ideographs(20_000, 300);
    let twice: Vec<&str> = keys
        .iter()
        .rev()
        .chain(keys.iter())
        .map(String::as_str)
        .collect();
    let dir = scratch("dictcoded");
    let path = dir.join("idx.bdx");
    for block in [1usize, 32, DEFAULT_BLOCK, MAX_BLOCK] {
        let want = DictIndex::build_with_block(&keys, block)
            .unwrap()
            .to_bytes();
        assert_eq!(&want[..4], CODED_MAGIC, "block {block}");
        let n =
            DictIndex::build_to_file_runs(&twice, &path, DictLayout::new(block), || Ok(()), 4096)
                .unwrap();
        assert_eq!(n, keys.len());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            want,
            "block {block}, spilled"
        );
        let n = DictIndex::build_to_file_with_block(&twice, &path, block).unwrap();
        assert_eq!(n, keys.len());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            want,
            "block {block}, in memory"
        );
        check(&DictIndex::load(&path).unwrap(), &keys);
    }
    std::fs::remove_dir_all(&dir).ok();
}

proptest::proptest! {
    /// The sort on threads orders keys as one thread's sort does, duplicates and all, for any
    /// thread count — including more threads than the sample can take a key a range from.
    #[test]
    fn sort_on_threads_is_the_one_thread_sort(
        keys in proptest::collection::vec("[ab\u{e9}]{0,6}", 0..3000),
        threads in 1usize..17,
    ) {
        let mut want: Vec<&str> = keys.iter().map(String::as_str).collect();
        want.sort_unstable();
        let mut got: Vec<&str> = keys.iter().map(String::as_str).collect();
        sort_on(&mut got, threads);
        proptest::prop_assert_eq!(got, want);
    }
}

proptest::proptest! {
    /// The pass that checks the order and drops duplicates leaves what a sort and `dedup`
    /// leave, and refuses exactly the keys that do not ascend, leaving them all — on any
    /// number of threads, over keys that come sorted as well as over keys that do not.
    #[test]
    fn one_pass_dedups_as_sort_and_dedup_do(
        mut keys in proptest::collection::vec("[ab\u{e9}\u{4e2d}]{0,3}", 0..40),
        sorted in proptest::bool::ANY,
        threads in 1usize..8,
    ) {
        if sorted {
            keys.sort_unstable();
        }
        let mut want = keys.clone();
        want.sort_unstable();
        want.dedup();
        let mut got = view_of(&keys, threads);
        sort_distinct(&mut got, threads);
        proptest::prop_assert_eq!(&got, &want);
        proptest::prop_assert_eq!(
            strictly_ascending(&view_of(&keys, threads), threads),
            keys.windows(2).all(|w| w[0] < w[1])
        );
        let ascends = keys.windows(2).all(|w| w[0] <= w[1]);
        let mut once = view_of(&keys, threads);
        proptest::prop_assert_eq!(distinct_ascending(&mut once, threads), ascends);
        if ascends {
            proptest::prop_assert_eq!(&once, &want);
        } else {
            let mut all = keys.clone();
            all.sort_unstable();
            once.sort_unstable();
            proptest::prop_assert_eq!(&once, &all);
        }
    }
}

/// Counted and spelled a range of keys a thread, the code and every spelling are the ones one
/// thread makes.
#[test]
fn the_code_does_not_depend_on_how_many_threads_counted() {
    let keys = ideographs(20_000, 300);
    let view: Vec<&str> = keys.iter().map(String::as_str).collect();
    let one = choose_code(&view, 1).expect("ideographs pay for a code");
    let (arena, ends) = encode_all(&one, &view).unwrap();
    let serial: Vec<&[u8]> = views(&arena, &ends).into_iter().map(|b| b.0).collect();
    let (mut want, mut got) = (Vec::new(), Vec::new());
    one.write_to(&mut want);
    for threads in [2, 3, 7] {
        let code = choose_code(&view, threads).unwrap();
        got.clear();
        code.write_to(&mut got);
        assert_eq!(got, want, "{threads} threads");
        let parts = in_ranges(&view, threads, |range| encode_all(&code, range).unwrap());
        let spelt: Vec<&[u8]> = parts
            .iter()
            .flat_map(|(arena, ends)| views(arena, ends))
            .map(|b| b.0)
            .collect();
        assert_eq!(spelt, serial, "{threads} threads");
    }
}

#[test]
fn a_corrupted_character_code_is_refused_or_answers_without_panicking() {
    let keys = scripts();
    let ps = script_probes(&keys);
    let (mut loaded, mut spurned) = (0, 0);
    for seven in [true, false] {
        let blob = coded(&keys, 32, seven, 3).to_bytes();
        let len = u32::from_le_bytes(blob[62..66].try_into().unwrap()) as usize;
        let at = blob.len() - len;
        assert_eq!(layout(&blob, 32).chars, at);
        for i in 0..len {
            for xor in [0x01u8, 0x80, 0xFF] {
                let mut b = blob.clone();
                b[at + i] ^= xor;
                reframe(&mut b);
                if answers_or_is_refused(&b, &keys, &ps, true) {
                    loaded += 1;
                } else {
                    spurned += 1;
                }
            }
        }
        // A length that does not reach the end of the blob, or runs past it.
        for len in [len - 1, len + 1] {
            let mut b = blob.clone();
            b[62..66].copy_from_slice(&(len as u32).to_le_bytes());
            reframe(&mut b);
            refused(&b, "do not add up");
        }
        // The magic alone decides whether there is a code: a coded blob read as a plain one
        // has bytes left over, and a plain one read as coded names a section it lacks.
        let mut b = blob.clone();
        b[..4].copy_from_slice(MAGIC);
        reframe(&mut b);
        refused(&b, "do not add up");
    }
    let mut plain = DictIndex::build(&keys).unwrap().to_bytes();
    plain[..4].copy_from_slice(CODED_MAGIC);
    reframe(&mut plain);
    refused(&plain, "bad character code");
    assert!(
        loaded > 0 && spurned > 0,
        "loaded {loaded}, refused {spurned}"
    );
}

/// A corpus spilled into more runs than a merge may open builds the same blob as one held in
/// memory. The collapse runs between the spill and the three passes, and if it lost, reordered
/// or duplicated a key the bytes would differ.
#[test]
fn a_streamed_build_over_a_collapsed_merge_writes_the_same_blob() {
    let keys = corpus();
    crate::extsort::set_fan_in(4);
    let dir = scratch("dictcollapse");
    let path = dir.join("idx.bdx");
    for block in [1usize, 32, 256] {
        let want = DictIndex::build_with_block(&keys, block)
            .unwrap()
            .to_bytes();
        // A 32-byte run budget over this corpus is dozens of runs against a fan-in of four.
        let n = DictIndex::build_to_file_runs(&keys, &path, DictLayout::new(block), || Ok(()), 32)
            .unwrap();
        assert_eq!(n, keys.len());
        assert_eq!(std::fs::read(&path).unwrap(), want, "block {block}");
        assert_eq!(entries(&dir), ["idx.bdx"], "the runs directory is gone");
        check(&DictIndex::load(&path).unwrap(), &keys);
    }
    crate::extsort::set_fan_in(0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_sections_account_for_every_byte_and_every_key() {
    let keys = corpus();
    for block in [1, 2, 3, 7, 32, 500, 1024] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        let s = idx.sections();
        let what = format!("block {block}");
        assert_eq!(s.total() as usize, idx.serialized_len(), "{what}");
        assert_eq!(s.total() as usize, idx.to_bytes().len(), "{what}");
        // Every key is stored exactly once, as a head, a restart or an entry.
        let blocks = keys.len().div_ceil(block) as u64;
        assert_eq!(blocks + s.restarts + s.entries, keys.len() as u64, "{what}");
        // The headers are a stream now, not a byte an entry, so what the accounting has to
        // hold is that every run's is counted — and that at the block sizes this index is
        // built at it is narrower than the byte an entry the format it replaced spent. A run
        // of one entry is not: its frame's prologue is three bytes whatever follows it.
        assert_eq!(s.restart_headers > 0, s.restarts > 0, "{what}");
        assert_eq!(s.entry_headers > 0, s.entries > 0, "{what}");
        if block >= 32 {
            assert!(s.entry_headers < s.entries, "{what}");
        }
        if block == 1 {
            // Every key is its own block head, so nothing is front-coded at all.
            assert_eq!(
                (s.entries, s.entry_headers, s.entry_codes, s.wide),
                (0, 0, 0, 0),
                "{what}"
            );
        } else {
            // This corpus carries keys past the one-byte header's fifteen on purpose.
            assert!(s.wide > 0 && s.entry_wide > 0, "{what}");
        }
        // A mapping walks the same blob to the same split.
        assert_eq!(
            DictIndex::from_bytes(&idx.to_bytes()).unwrap().sections(),
            s,
            "{what}"
        );
    }
}

#[test]
fn a_block_that_is_one_microblock_stores_no_restarts() {
    let keys = corpus();
    // `micro_for(16)` is 16, so a block of 16 is one microblock and has no restart run.
    let whole = DictIndex::build_with_block(&keys, 16).unwrap();
    let s = whole.sections();
    assert_eq!(
        (
            s.restarts,
            s.restart_headers,
            s.restart_wide,
            s.restart_codes
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        s.micro_offsets, 0,
        "no array is stored for a single microblock"
    );
    assert_eq!(s.total() as usize, whole.serialized_len());
    // A block of 256 is sixteen microblocks, and the restarts that reach them are paid for.
    let cut = DictIndex::build_with_block(&keys, 256).unwrap();
    let t = cut.sections();
    assert!(t.restarts > 0 && t.micro_offsets > 0);
    assert_eq!(t.total() as usize, cut.serialized_len());
}

#[test]
fn an_empty_index_has_only_a_header_and_a_table() {
    let idx = DictIndex::build(Vec::<String>::new()).unwrap();
    let s = idx.sections();
    assert_eq!((s.heads, s.entries, s.restarts), (0, 0, 0));
    assert_eq!(s.total() as usize, idx.serialized_len());
}

#[test]
fn the_trainers_are_bounded_by_a_budget_and_not_by_the_core_count() {
    assert_eq!(train_threads(1), 1);
    assert_eq!(train_threads(8), 8);
    let cap = train_threads(usize::MAX);
    assert_eq!(train_threads(4096), cap);
    assert!(
        cap * TRAIN_BYTES <= TRAIN_BUDGET,
        "{cap} trainers overrun the budget"
    );
    assert!(
        cap >= 16,
        "a bound below this machine's own thread count would cost build time"
    );
}

#[test]
fn the_profiles_name_three_points_of_the_block_curve() {
    let keys = corpus();
    let mut sizes = Vec::new();
    for profile in [
        DictProfile::Fast,
        DictProfile::Balanced,
        DictProfile::Compact,
    ] {
        let idx = DictIndex::build_with_block(&keys, profile.block()).unwrap();
        assert_eq!(idx.block(), profile.block(), "{profile:?}");
        check(&idx, &keys);
        sizes.push(idx.serialized_len());
    }
    assert!(sizes[0] > sizes[1] && sizes[1] > sizes[2], "{sizes:?}");
    assert_eq!(DictProfile::Balanced.block(), DEFAULT_BLOCK);
    assert_eq!(DictIndex::build(&keys).unwrap().block(), DEFAULT_BLOCK);
}

#[test]
fn the_ordered_queries_agree_with_a_linear_scan() {
    let keys = corpus();
    let ps = probes(&keys);
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        scan_ordered(&idx, &keys, &ps, &format!("block {block}"));
    }
}

/// Every ordered query `idx` answers for `ps`, against a scan of `keys` in rank order.
fn scan_ordered(idx: &DictIndex, keys: &[String], ps: &[String], what: &str) {
    let pairs: Vec<(String, u64)> = keys.iter().cloned().zip(0..).collect();
    for p in ps {
        let want: Vec<(String, u64)> = pairs
            .iter()
            .filter(|(k, _)| k.starts_with(p))
            .cloned()
            .collect();
        assert_eq!(&idx.prefix(p), &want, "{what} prefix {p:?}");
        assert_eq!(
            idx.prefix_iter(p).take(2).collect::<Vec<_>>(),
            want.iter().take(2).cloned().collect::<Vec<_>>(),
            "{what} prefix_iter {p:?}"
        );
        let r = idx.prefix_id_range(p);
        assert_eq!(r.end - r.start, want.len() as u64, "{what} range {p:?}");
        assert_eq!(idx.prefix_count(p), want.len() as u64, "{what} count {p:?}");
        if let Some((_, first)) = want.first() {
            assert_eq!(r.start, *first, "{what} range start {p:?}");
        }

        let want = pairs
            .iter()
            .find(|(k, _)| k.as_str() >= p.as_str())
            .cloned();
        assert_eq!(idx.successor(p), want, "{what} successor {p:?}");
        let want = pairs
            .iter()
            .rev()
            .find(|(k, _)| k.as_str() <= p.as_str())
            .cloned();
        assert_eq!(idx.predecessor(p), want, "{what} predecessor {p:?}");

        let want: Vec<(String, u64)> = pairs
            .iter()
            .filter(|(k, _)| k.as_str() > p.as_str())
            .cloned()
            .collect();
        assert_eq!(
            idx.iter_after(p).collect::<Vec<_>>(),
            want,
            "{what} after {p:?}"
        );
    }
    for w in ps.chunks(2).filter(|w| w.len() == 2) {
        let (lo, hi) = (&w[0], &w[1]);
        let want: Vec<(String, u64)> = pairs
            .iter()
            .filter(|(k, _)| k.as_str() >= lo.as_str() && k.as_str() < hi.as_str())
            .cloned()
            .collect();
        assert_eq!(&idx.range(lo, hi), &want, "{what} range {lo:?}..{hi:?}");
        assert_eq!(idx.range_count(lo, hi), want.len() as u64, "{what} count");
    }
}

#[test]
fn a_staircase_deeper_than_the_stack_still_answers() {
    // Every key extends the one before it by a byte, so every entry adds a level and the
    // staircase is as deep as the rank inside the block. Past `STAIRS` the walk stops
    // tracking it and decodes each entry instead, which is the path this pins.
    let keys: Vec<String> = (1..=2 * STAIRS).map(|n| "a".repeat(n)).collect();
    for block in [STAIRS - 1, STAIRS + 1, MAX_BLOCK] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        check(&idx, &keys);
        check(&DictIndex::from_bytes(&idx.to_bytes()).unwrap(), &keys);
    }
}

#[test]
fn iter_from_resumes_inside_a_block() {
    let keys = corpus();
    let idx = DictIndex::build_with_block(&keys, 5).unwrap();
    for start in [
        0u64,
        1,
        4,
        5,
        6,
        17,
        keys.len() as u64 - 1,
        keys.len() as u64,
        u64::MAX,
    ] {
        let got: Vec<u64> = idx.iter_from(start).map(|(_, id)| id).collect();
        let expect: Vec<u64> = (start.min(keys.len() as u64)..keys.len() as u64).collect();
        assert_eq!(got, expect, "from {start}");
        assert!(
            idx.iter_from(start).all(|(k, id)| Some(k) == idx.key(id)),
            "from {start}"
        );
    }
}

#[test]
fn duplicates_and_input_order_do_not_matter() {
    let a = DictIndex::build(["pear", "apple", "fig", "apple"]).unwrap();
    let b = DictIndex::build(["fig", "pear", "apple"]).unwrap();
    assert_eq!(a.to_bytes(), b.to_bytes());
    assert_eq!(a.len(), 3);
    assert_eq!(a.id("apple"), Some(0));
    assert_eq!(a.id("pear"), Some(2));
    assert_eq!(a.lower_bound("banana"), 1);
    assert_eq!(a.lower_bound("zebra"), 3);
}

#[test]
fn an_empty_index_answers_and_round_trips() {
    let idx = DictIndex::build(Vec::<String>::new()).unwrap();
    assert!(idx.is_empty());
    assert_eq!(
        (idx.id(""), idx.key(0), idx.lower_bound("x")),
        (None, None, 0)
    );
    assert!(!idx.contains(""));
    assert_eq!(idx.iter().count(), 0);
    let blob = idx.to_bytes();
    assert_eq!(blob.len(), idx.serialized_len());
    let back = DictIndex::from_bytes(&blob).unwrap();
    assert!(back.is_empty() && back.to_bytes() == blob);
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lexindex_{name}_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn build_to_file_writes_the_blob_build_would_have_run_by_run() {
    let keys = corpus();
    // Descending, then ascending: every key twice, its copies far enough apart that a 64-byte
    // run budget puts them in different runs, which is where a merge that failed to
    // deduplicate across runs would show.
    let twice: Vec<&str> = keys
        .iter()
        .rev()
        .chain(keys.iter())
        .map(String::as_str)
        .collect();
    let dir = scratch("dictruns");
    let path = dir.join("idx.bdx");
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let want = DictIndex::build_with_block(&keys, block)
            .unwrap()
            .to_bytes();
        let n = DictIndex::build_to_file_runs(&twice, &path, DictLayout::new(block), || Ok(()), 64)
            .unwrap();
        assert_eq!(n, keys.len());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            want,
            "block {block}, spilled"
        );
        assert_eq!(entries(&dir), ["idx.bdx"], "the runs directory is gone");
        check(&DictIndex::load(&path).unwrap(), &keys);
        // The same corpus in one run: sorted in memory, no runs directory at all.
        let n = DictIndex::build_to_file_with_block(&twice, &path, block).unwrap();
        assert_eq!(n, keys.len());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            want,
            "block {block}, in memory"
        );
        assert_eq!(entries(&dir), ["idx.bdx"]);
    }
    assert_eq!(DictIndex::build_to_file(&twice, &path).unwrap(), keys.len());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        DictIndex::build(&keys).unwrap().to_bytes()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_to_file_refuses_what_it_cannot_hold_and_publishes_nothing() {
    let dir = scratch("dictruns_err");
    let path = dir.join("idx.bdx");
    assert_eq!(
        DictIndex::build_to_file(Vec::<String>::new(), &path).unwrap(),
        0
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        DictIndex::build(Vec::<&str>::new()).unwrap().to_bytes()
    );
    assert!(DictIndex::load(&path).unwrap().is_empty());

    // A key no run can hold is an error, not an unbounded allocation -- and the empty index
    // written above is still there.
    let long = "x".repeat(65);
    let err =
        DictIndex::build_to_file_runs([long.as_str()], &path, DictLayout::new(32), || Ok(()), 64)
            .unwrap_err();
    assert!(err.to_string().contains("run budget"), "{err}");
    assert!(DictIndex::load(&path).unwrap().is_empty());
    for block in [0, MAX_BLOCK + 1] {
        let err = DictIndex::build_to_file_with_block(["a"], &path, block).unwrap_err();
        assert!(err.to_string().contains("block must be"), "{err}");
    }

    // The caller's last word, asked once the input has ended and again inside the write.
    std::fs::write(&path, b"previous").unwrap();
    let keys: Vec<String> = (0..200)
        .map(|i| format!("k{:03}", (i * 7919) % 200))
        .collect();
    let calls = std::cell::Cell::new(0);
    let late = || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Err(IndexError::Format("late"))
        } else {
            Ok(())
        }
    };
    let err =
        DictIndex::build_to_file_runs(&keys, &path, DictLayout::new(32), late, 64).unwrap_err();
    assert!(matches!(err, IndexError::Format("late")), "{err}");
    assert_eq!(
        calls.get(),
        2,
        "once after the input, once inside the write"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"previous");
    assert_eq!(entries(&dir), ["idx.bdx"], "the runs directory is gone");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_block_size_outside_the_range_is_refused() {
    for block in [0, MAX_BLOCK + 1] {
        let err = DictIndex::build_with_block(["a"], block).unwrap_err();
        assert!(err.to_string().contains("block must be"), "{err}");
        let err = DictIndex::build_sorted_with_block(["a"], block).unwrap_err();
        assert!(err.to_string().contains("block must be"), "{err}");
    }
}

#[test]
fn keys_of_answers_what_key_answers_in_any_order() {
    let keys = corpus();
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        keys_of_matches(&idx, &format!("block {block}"));
    }
}

/// [`DictIndex::keys_of`] answers every order of ids as [`DictIndex::key`] does one at a time.
fn keys_of_matches(idx: &DictIndex, what: &str) {
    let n = idx.len() as u64;
    // Ascending and contiguous (the walk), every third (a sparse ascending run),
    // descending (no walk), scattered, and out of range.
    let orders: [Vec<u64>; 5] = [
        (0..n).collect(),
        (0..n).step_by(3).collect(),
        (0..n).rev().collect(),
        (0..n).map(|i| (i * 7919) % n).collect(),
        vec![n, n + 1, u64::MAX, 0, n - 1],
    ];
    for ids in &orders {
        let want: Vec<Option<String>> = ids.iter().map(|&i| idx.key(i)).collect();
        assert_eq!(&idx.keys_of(ids), &want, "{what}");
    }
    assert!(idx.keys_of(&[]).is_empty());
}

#[test]
fn a_batch_answers_exactly_what_the_keys_answer_one_by_one() {
    let keys = corpus();
    let mut queries = keys.clone();
    queries.extend(probes(&keys));
    assert!(
        queries.len() > LANES * 2,
        "the lane loop must run more than once"
    );
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        batch_matches(&idx, &queries, &format!("block {block}"));
    }
    let empty = DictIndex::build(Vec::<&str>::new()).unwrap();
    assert_eq!(
        empty.ids_of(&queries[..LANES + 1]),
        vec![None; LANES + 1],
        "an empty index still answers once per key"
    );
}

/// Runs of blocks whose samples are all equal, of every length that changes the lanes' path —
/// one block, one short of a run with a trie, exactly one, one past, and a long run that ends
/// at the last block — answered by the batch as `id` answers them, routed or not, built or
/// loaded (a load derives the tries).
#[test]
fn a_batch_places_probes_in_runs_of_equal_samples_as_id_does() {
    let block = 4;
    let mut keys = Vec::new();
    for (group, run) in [1, MIN_RUN - 1, MIN_RUN, MIN_RUN + 1, 3 * MIN_RUN]
        .iter()
        .enumerate()
    {
        // Eight shared bytes after the group's own two: every head of the group samples alike.
        keys.extend((0..run * block).map(|i| format!("{group:02}same-run{i:05}")));
    }
    let mut queries = keys.clone();
    for k in &keys {
        queries.push(format!("{k}\u{7f}"));
        queries.push(format!("{}~", &k[..k.len() - 1]));
        queries.push(k[..10].to_string());
    }
    queries.extend(["", "00", "99", "02same-run", "04same-run99999~"].map(String::from));
    let built = DictIndex::build_with_block(&keys, block).unwrap();
    let loaded = DictIndex::from_bytes(&built.to_bytes()).unwrap();
    assert!(loaded.ties.is_some(), "the long runs have tries");
    batch_matches(&built, &queries, "built");
    batch_matches(&loaded, &queries, "loaded");
    loaded.route_microblocks();
    batch_matches(&loaded, &queries, "loaded, routed");
}

/// [`DictIndex::ids_of`] answers `queries` as [`DictIndex::id`] does one at a time.
fn batch_matches(idx: &DictIndex, queries: &[String], what: &str) {
    let want: Vec<Option<u64>> = queries.iter().map(|q| idx.id(q)).collect();
    assert_eq!(idx.ids_of(queries), want, "{what}");
    // The ragged last lane, at every width that can end one.
    for take in [0usize, 1, LANES - 1, LANES, LANES + 1] {
        assert_eq!(
            idx.ids_of(&queries[..take]),
            want[..take].to_vec(),
            "{what} take {take}"
        );
    }
}

#[test]
fn the_prefixes_of_a_query_agree_with_a_linear_scan() {
    let keys = corpus();
    let ps = probes(&keys);
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        scan_prefixes(&idx, &keys, &ps, &format!("block {block}"));
    }
}

/// The keys that are prefixes of each of `ps`, against a scan of `keys` in rank order.
fn scan_prefixes(idx: &DictIndex, keys: &[String], ps: &[String], what: &str) {
    for q in ps {
        // Prefixes of `q` sort by length, so the linear scan in rank order is already
        // shortest first and its last element is the longest match.
        let want: Vec<(String, u64)> = keys
            .iter()
            .zip(0u64..)
            .filter(|(k, _)| q.starts_with(k.as_str()))
            .map(|(k, id)| (k.clone(), id))
            .collect();
        assert_eq!(&idx.common_prefix(q), &want, "{what} query {q:?}");
        assert_eq!(
            idx.longest_prefix(q),
            want.last().cloned(),
            "{what} query {q:?}"
        );
    }
}

/// Holds the shard size for as long as it is alive, and puts the real rule back after.
struct Shards;

impl Shards {
    fn of(blocks: usize) -> Self {
        SHARD_OVERRIDE.with(|c| c.set(blocks));
        Shards
    }
}

impl Drop for Shards {
    fn drop(&mut self) {
        SHARD_OVERRIDE.with(|c| c.set(0));
    }
}

/// `README.md` and `docs/design.md` both promise that the same keys give the same blob on any
/// thread count, and the phrase miner broke it: a pool keeps the candidates above its own
/// median, so a pool that held twice the shards kept a different half and the
/// vocabulary -- and every coded suffix under it -- came out different. Measured before the
/// fix, one to sixteen cores gave three blobs of a million urls and six of a million paths.
///
/// That cut is what has to fire for the promise to be tested at all, which is why the pool is
/// held down to a size a test can reach: keys drawn from a handful of spans so the miner has
/// candidates, one block a shard so there are pools to split, and every thread count from one
/// to more than there are pools.
#[test]
fn the_same_keys_give_the_same_blob_on_any_thread_count() {
    let _shards = Shards::of(1);
    let _pool = crate::phrase::Pool::of(48);
    let spans = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
    ];
    let mut keys: Vec<String> = (0..2048u32)
        .map(|i| {
            let (a, b, c) = (i as usize % 7, (i as usize / 7) % 7, (i as usize / 49) % 7);
            format!("{}/{}/{}/{i:06}", spans[a], spans[b], spans[c])
        })
        .collect();
    keys.sort();
    keys.dedup();
    let one = DictIndex::from_sorted_on(&keys, 32, micro_for(32), 1)
        .expect("a build of distinct sorted keys cannot fail")
        .to_bytes();
    for threads in [2usize, 3, 5, 8, 24] {
        let many = DictIndex::from_sorted_on(&keys, 32, micro_for(32), threads)
            .expect("a build of distinct sorted keys cannot fail")
            .to_bytes();
        assert_eq!(many.len(), one.len(), "{threads} threads");
        assert!(many == one, "{threads} threads");
    }
}

/// Keys of four characters with nothing repeating, from a fixed generator. A counter spelled in
/// the same letters is no such thing: its high places are one letter, and a symbol table takes
/// those keys 15–29 % below what the alphabet can.
fn four_letter_keys(n: usize, len: usize) -> Vec<String> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b"ACGT"[(x >> 61) as usize & 3] as char
                })
                .collect()
        })
        .collect()
}

/// A corpus of four characters is what the packed alphabet exists for: it must be the codec
/// the build settles on, every key must still answer, and the blob must survive a round trip —
/// the codes of a run are continuous, so an entry starts mid-byte and a reader that assumed
/// bytes would read the neighbour's.
#[test]
fn a_four_letter_corpus_packs_its_suffixes_and_still_answers() {
    let mut keys = four_letter_keys(4000, 12);
    keys.sort();
    keys.dedup();
    for block in [4usize, 32, 256] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        assert!(
            idx.codecs.iter().all(|c| matches!(c, Codec::Packed(_))),
            "block {block}"
        );
        check(&idx, &keys);
        let blob = idx.to_bytes();
        assert_eq!(blob.len(), idx.serialized_len(), "block {block}");
        let back = DictIndex::from_bytes(&blob).unwrap();
        assert_eq!(back.to_bytes(), blob, "block {block}");
        check(&back, &keys);
        // A packed blob is smaller than the same keys under the symbol table, which is the
        // only reason the tier exists.
        assert!(
            blob.len() < keys.iter().map(String::len).sum::<usize>(),
            "block {block}"
        );
    }
}

/// A corpus of one shape does not force the other's shards: a blob holding both must pick per
/// shard, and every key of both must answer.
///
/// One block per shard, and the default block: over 64 keys the alphabet beat the table by
/// two bytes in a hundred and seventy, which is inside what five run prologues round away —
/// the test then measured the microblock size rather than the shape of the keys.
#[test]
fn two_shapes_in_one_blob_settle_on_their_own_codecs() {
    let _held = Shards::of(1);
    let mut keys: Vec<String> = (0..600u32)
        .map(|i| format!("https://example.com/a/{i:09}/index.html"))
        .collect();
    keys.extend(four_letter_keys(600, 14));
    keys.sort();
    keys.dedup();
    let idx = DictIndex::build_with_block(&keys, 256).unwrap();
    let packed = idx
        .codecs
        .iter()
        .filter(|c| matches!(c, Codec::Packed(_)))
        .count();
    assert!(packed > 0 && packed < idx.codecs.len(), "{packed} shards");
    check(&idx, &keys);
    check(&DictIndex::from_bytes(&idx.to_bytes()).unwrap(), &keys);
}

/// Keys that all begin the same way put nothing in a sample of their first eight bytes, so the
/// sample is taken past what every head shares. Every key must still answer, and so must a
/// stranger that falls short of that prefix, one that runs past it, and one inside it — the
/// three the samples cannot place at all.
#[test]
fn a_shared_head_prefix_moves_the_sample_and_still_places_every_probe() {
    let mut keys: Vec<String> = (0..2000u32)
        .map(|i| format!("https://example.com/articles/{i:07}"))
        .collect();
    keys.sort();
    keys.dedup();
    for block in [4usize, 64, 256] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        assert!(
            idx.g >= "https://example.com/articles/0".len() - 1,
            "{}",
            idx.g
        );
        check(&idx, &keys);
        let blob = idx.to_bytes();
        let back = DictIndex::from_bytes(&blob).unwrap();
        assert_eq!(back.g, idx.g);
        check(&back, &keys);
        // The samples stop being one repeated word, which is the whole point of the offset.
        let distinct = {
            let mut s = idx.samples.clone();
            s.dedup();
            s.len()
        };
        assert!(
            distinct * 2 > idx.blocks_len(),
            "{distinct} of {}",
            idx.blocks_len()
        );
        for stranger in [
            "http",
            "https:/",
            "https://example.com/articl",
            "https://example.com/articles",
            "https://example.com/articles/",
            "https://example.com/articles/0000000/x",
            "https://example.com/artifacts/0000001",
            "https://example.net/",
            "zzz",
            "",
        ] {
            let want = keys.partition_point(|k| k.as_str() < stranger) as u64;
            assert_eq!(idx.lower_bound(stranger), want, "{stranger:?} at {block}");
            assert_eq!(idx.id(stranger), None, "{stranger:?} at {block}");
            assert_eq!(
                idx.ids_of(&[stranger.to_string()]),
                vec![None],
                "{stranger:?} at {block}"
            );
        }
    }
}

#[test]
fn runs_either_side_of_the_tie_minimum_place_every_probe() {
    // Keys that share their first eight bytes a run of them: at block 1 every key heads a
    // block, and the runs of equal samples are one block short of `MIN_RUN`, `MIN_RUN`, and
    // well past it.
    let mut keys: Vec<String> = Vec::new();
    for (sample, k) in [
        ("a-short-", MIN_RUN - 1),
        ("b-exact-", MIN_RUN),
        ("c-longer", 3 * MIN_RUN),
    ] {
        keys.extend((0..k).map(|i| format!("{sample}/{i:04}")));
    }
    let idx = DictIndex::build_with_block(&keys, 1).unwrap();
    assert_eq!(idx.g, 0);
    let ties = idx.ties.as_deref().expect("the runs split");
    assert!(ties.root(0).is_none());
    assert!(ties.root(MIN_RUN - 1).is_some());
    assert!(ties.root(2 * MIN_RUN - 1).is_some());
    let mut queries = keys.clone();
    queries.extend(probes(&keys));
    for block in [1usize, 2, 3] {
        let idx = DictIndex::build_with_block(&keys, block).unwrap();
        check(&idx, &keys);
        batch_matches(&idx, &queries, &format!("block {block}"));
    }
}

#[test]
fn a_table_a_shard_answers_every_key_and_survives_the_blob() {
    let keys = corpus();
    for block in [1usize, 3, 32, 256] {
        for shard in [1usize, 2, 7] {
            let _held = Shards::of(shard);
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            let want = keys.len().div_ceil(block).div_ceil(shard).max(1);
            assert_eq!(idx.codecs.len(), want, "block {block}, shard {shard}");
            assert_eq!(idx.shard, shard);
            check(&idx, &keys);
            let back = DictIndex::from_bytes(&idx.to_bytes()).unwrap();
            assert_eq!(back.codecs.len(), want);
            check(&back, &keys);
        }
    }
}

/// A corpus whose keys repeat a handful of long spans *after* the point where they diverge is
/// what the phrase section exists for — front coding has already taken the shared head, so
/// what is left is the same few spans over and over.
fn phrase_corpus() -> Vec<String> {
    let hosts = ["example.com", "example.org", "archive.example.net"];
    let paths = ["wiki/article", "blog/post", "docs/reference/manual"];
    let mut keys: Vec<String> = (0..MINE_MIN + 1000)
        .map(|i| {
            let (h, p) = (hosts[i % 3], paths[i % 7 % 3]);
            format!("{i:07}-https://{h}/{p}/index.html")
        })
        .collect();
    keys.sort();
    keys
}

/// The shards buy what such a corpus repeats, every key still answers, and both builds write
/// it alike.
#[test]
fn a_corpus_of_repeated_spans_buys_phrases() {
    let keys = phrase_corpus();
    let idx = DictIndex::build(&keys).unwrap();
    assert!(idx.sections().phrases > 0, "no phrase earned its bytes");
    assert!(
        idx.codecs
            .iter()
            .any(|c| matches!(c, Codec::Symbols { split: Some(_), .. })),
        "no shard bought the phrases it mined"
    );
    check(&idx, &keys);
    let back = DictIndex::from_bytes(&idx.to_bytes()).unwrap();
    assert_eq!(back.phrases.len(), idx.phrases.len());
    check(&back, &keys);

    let dir = scratch("dictphrases");
    let path = dir.join("idx.bdx");
    let feed: Vec<&str> = keys.iter().map(String::as_str).collect();
    DictIndex::build_to_file(&feed, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A loaded index reads its phrases where its blob holds them: a copy of the section was most of
/// what a mapped index kept on the heap.
#[test]
fn a_loaded_index_reads_its_phrases_in_place() {
    let keys = phrase_corpus();
    let blob = DictIndex::build(&keys).unwrap().to_bytes();
    let back = DictIndex::from_bytes(&blob).unwrap();
    assert!(back.phrases.len() > 0, "no phrase earned its bytes");
    let start = back.heads.as_ptr().addr() - HEADER;
    assert!((start..start + blob.len()).contains(&back.phrases.addr()));
    check(&back, &keys);
}

/// `blob` is either refused or answers everything it is asked without panicking. Nothing is
/// claimed about *what* it answers: the bytes behind it are an attacker's.
fn answers_or_is_refused(blob: &[u8], keys: &[String], probes: &[String], walk: bool) -> bool {
    let Ok(idx) = DictIndex::from_bytes(blob) else {
        return false;
    };
    let mut buf = String::new();
    for p in keys.iter().step_by(37).chain(probes) {
        let _ = (idx.id(p), idx.lower_bound(p), idx.contains(p));
    }
    for id in (0..keys.len() as u64).step_by(37) {
        let _ = (idx.key(id), idx.key_into(id, &mut buf));
    }
    if walk {
        assert!(idx.iter().count() <= keys.len());
    }
    true
}

/// A shard that bought phrases reads the dictionary as if it were its own bytes, so every
/// reference into it is checked where it is read. A dictionary that does not parse is refused
/// at the load; one that parses still answers — with somebody else's spans, which is what the
/// bound on each reference is for, not a claim about the content.
#[test]
fn a_corrupted_phrase_dictionary_never_panics() {
    let keys = phrase_corpus();
    let blob = DictIndex::build(&keys).unwrap().to_bytes();
    let at = layout(&blob, DEFAULT_BLOCK).phrases;
    let len = u32::from_le_bytes(blob[58..62].try_into().unwrap()) as usize;
    assert!(len > 0, "no dictionary to corrupt");
    let probes: Vec<String> = probes(&keys).into_iter().step_by(101).collect();
    let (mut loaded, mut refused) = (0, 0);
    for fill in [0x00u8, 0x01, 0x7F, 0xFE, 0xFF] {
        let mut b = blob.clone();
        b[at..at + len].fill(fill);
        reframe(&mut b);
        if answers_or_is_refused(&b, &keys, &probes, true) {
            loaded += 1;
        } else {
            refused += 1;
        }
    }
    // The count and the per-group tables sit at the front, the phrase bytes behind them; a
    // stride reaches both, and flipping the top bit of an end is what puts a phrase past its
    // group.
    for i in (0..len).step_by(len / 32 + 1) {
        for xor in [0x01u8, 0x80] {
            let mut b = blob.clone();
            b[at + i] ^= xor;
            reframe(&mut b);
            if answers_or_is_refused(&b, &keys, &probes, false) {
                loaded += 1;
            } else {
                refused += 1;
            }
        }
    }
    // Neither half of the check is allowed to go quiet: a dictionary nothing can corrupt into
    // loading would leave the decode untested, and one nothing can corrupt into a refusal
    // would say `Dict::read` validates nothing.
    assert!(
        loaded > 0 && refused > 0,
        "loaded {loaded}, refused {refused}"
    );
}

/// Paths under a few directories, each name reused under many parents: the shape whose
/// repeated trie labels a `BDX5` dictionary is for.
fn label_corpus(seed: u64, n: usize) -> Vec<String> {
    const DIRS: [&str; 6] = [
        "usr/share",
        "usr/lib",
        "home/me/src",
        "opt/tool",
        "var/log",
        "etc",
    ];
    const NAMES: [&str; 8] = [
        "locale/translations",
        "python3.14/site-packages",
        "icons/hicolor/48x48",
        "documentation",
        "node_modules/.bin",
        "x",
        "release-notes",
        "include/subdir",
    ];
    const FILES: [&str; 5] = ["index.html", "README.md", "__init__.py", "Makefile", "a"];
    let mut state = seed | 1;
    let mut next = |m: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % m as u64) as usize
    };
    let mut keys: Vec<String> = (0..n)
        .map(|i| {
            format!(
                "/{}/{}/{}-{}/{}",
                DIRS[next(DIRS.len())],
                NAMES[next(NAMES.len())],
                NAMES[next(NAMES.len())],
                i % 97,
                FILES[next(FILES.len())]
            )
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// A labelled blob answers every question the plain one does, at every block, through a load
/// and a mapping — and a build that tries labels and loses returns the plain blob.
#[test]
fn a_labelled_blob_answers_what_the_plain_one_does() {
    for (seed, n) in [(1u64, 4_000usize), (7, 900)] {
        let keys = label_corpus(seed, n);
        for block in [1usize, 16, 64, 256, 1024] {
            let plain = DictIndex::build_with_block(&keys, block).unwrap();
            let built =
                DictIndex::build_with(&keys, DictLayout::new(block).with_labels(true)).unwrap();
            let blob = built.to_bytes();
            if !built.has_labels() {
                assert_eq!(blob, plain.to_bytes(), "block {block}");
                continue;
            }
            assert_eq!(&blob[..4], LABELLED_MAGIC);
            assert!(blob.len() < plain.to_bytes().len(), "block {block}");
            check(&built, &keys);
            let back = DictIndex::from_bytes(&blob).unwrap();
            assert!(back.has_labels());
            assert_eq!(back.to_bytes(), blob, "block {block}");
            check(&back, &keys);
            for p in [
                "/usr",
                "/usr/share/x",
                "/home/me/src/documentation/",
                "/z",
                "",
            ] {
                assert_eq!(back.prefix_id_range(p), plain.prefix_id_range(p), "{p:?}");
            }
            let path = scratch("labelled").join(format!("idx-{seed}-{block}.bdx"));
            built.save(&path).unwrap();
            check(&DictIndex::load(&path).unwrap(), &keys);
            #[cfg(feature = "mmap")]
            {
                // SAFETY: the file is this test's own and nothing writes it while it is mapped.
                let mapped = unsafe { DictIndex::load_mmap(&path) }.unwrap();
                check(&mapped, &keys);
            }
        }
    }
    let labelled = DictIndex::build_with(
        label_corpus(2, 4_000),
        DictLayout::new(256).with_labels(true),
    )
    .unwrap();
    assert!(labelled.has_labels(), "the corpus is built to take labels");
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]
    /// Whatever the paths, a build that tries labels answers as the plain build does.
    #[test]
    fn a_build_with_labels_agrees_with_the_plain_build(
        seed in 1u64..u64::MAX,
        n in 50usize..1_500,
        block in proptest::sample::select(vec![4usize, 32, 256]),
    ) {
        let keys = label_corpus(seed, n);
        let idx = DictIndex::build_with(&keys, DictLayout::new(block).with_labels(true)).unwrap();
        check(&idx, &keys);
        check(&DictIndex::from_bytes(&idx.to_bytes()).unwrap(), &keys);
    }
}

/// The labels are a blob of their own inside the dictionary section, read by the loader every
/// blob goes through; a label that does not parse is refused at the load, and one that parses
/// still answers with somebody else's bytes, never past a section or outside the ids.
#[test]
fn a_corrupted_label_dictionary_never_panics() {
    let keys = label_corpus(2, 4_000);
    let idx = DictIndex::build_with(&keys, DictLayout::new(256).with_labels(true)).unwrap();
    assert!(idx.has_labels());
    let blob = idx.to_bytes();
    let at = layout(&blob, 256).phrases;
    let len = u32::from_le_bytes(blob[58..62].try_into().unwrap()) as usize;
    let probes: Vec<String> = probes(&keys).into_iter().step_by(53).collect();
    let (mut loaded, mut refused) = (0, 0);
    for i in (0..len).step_by(len / 97 + 1) {
        for xor in [0x01u8, 0x80, 0xFF] {
            let mut b = blob.clone();
            b[at + i] ^= xor;
            reframe(&mut b);
            if answers_or_is_refused(&b, &keys, &probes, false) {
                loaded += 1;
            } else {
                refused += 1;
            }
        }
    }
    // A nested blob is checked by its own header and payload checksums, so most edits are
    // refused; the stride also reaches the phrase half and the nested blob's sections.
    assert!(refused > 0, "loaded {loaded}, refused {refused}");
    // Ids past the labels: every reference byte of the block data set to the widest id.
    let data_len = u64::from_le_bytes(blob[24..32].try_into().unwrap()) as usize;
    let data_at = layout(&blob, 256).data;
    for fill in [0xFEu8, 0xFF, 0x80] {
        let mut b = blob.clone();
        b[data_at..data_at + data_len].fill(fill);
        reframe(&mut b);
        assert!(answers_or_is_refused(&b, &keys, &probes, true));
    }
}

/// Where a lookup reads every label a corpus takes often enough, they are all phrases: `BDX3`,
/// which answers like any other.
#[test]
fn a_dictionary_whose_labels_all_went_flat_is_no_labelled_blob() {
    let mut keys: Vec<String> = Vec::new();
    for dir in 0..20 {
        for name in ["first-of-the-names", "second-of-the-names", "third-of-them"] {
            keys.extend((0..100).map(|file| format!("d{dir:02}/{name}/{file:02}")));
        }
    }
    keys.sort();
    // No symbols to spell a byte in, so that the parse takes every label it can.
    let tables = [Table::train(&[])];
    let (block, micro) = (256, micro_for(256));
    let phrases = Phrases::none()
        .labelled(&keys, block, micro, &tables)
        .expect("the shared directories pay");
    assert!(!phrases.dict.has_labels());
    assert!(phrases.dict.phrases() > 0);
    let built = DictIndex::from_vocabulary(&keys, block, micro, 1, &tables, &phrases).unwrap();
    let blob = built.to_bytes();
    assert_eq!(&blob[..4], MAGIC);
    check(&built, &keys);
    check(&DictIndex::from_bytes(&blob).unwrap(), &keys);
}

/// A `BDX5` whose labels are themselves labelled or coded is no blob this crate writes.
#[test]
fn a_label_dictionary_that_names_labels_of_its_own_is_refused() {
    let keys = label_corpus(2, 4_000);
    let firsts = |idx: &DictIndex| SharedBytes::from_owned(vec![b'a'; idx.len()]);
    let inner = DictIndex::build_with(&keys, DictLayout::new(256).with_labels(true)).unwrap();
    assert!(inner.has_labels());
    assert!(
        crate::labels::Labels::of(
            DictIndex::build_with(&keys, DictLayout::new(256).with_labels(true)).unwrap(),
            firsts(&inner)
        )
        .is_none()
    );
    let plain = DictIndex::build_with_block(&keys, 16).unwrap();
    let short = SharedBytes::from_owned(vec![b'a'; plain.len() - 1]);
    assert!(
        crate::labels::Labels::of(DictIndex::build_with_block(&keys, 16).unwrap(), short).is_none()
    );
    let full = firsts(&plain);
    assert!(crate::labels::Labels::of(plain, full).is_some());
}

/// A crafted blob can nest labelled dictionaries one in another as deep as its bytes allow,
/// each level the same sections around the next. The nested dictionary is written plain only,
/// so the reader refuses any other by its magic before parsing it and recurses once whatever
/// the depth; parsed first, fifty thousand levels overflow a test thread's stack.
#[test]
fn a_label_dictionary_nested_without_end_is_refused_at_the_first_level() {
    let keys = label_corpus(1, 400);
    let mut view = view_of(&keys, 1);
    sort_distinct(&mut view, 1);
    let template = DictIndex::from_sorted_labelled(&view, 16, micro_for(16))
        .unwrap()
        .expect("the corpus's labels pay")
        .to_bytes();
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    // The nested blob is the one plain magic past the header, behind its length.
    let found: Vec<usize> = (HEADER..template.len() - 4)
        .filter(|&i| &template[i..i + 4] == MAGIC)
        .collect();
    assert_eq!(found.len(), 1);
    let at = found[0] - 4;
    let nested = u32_at(&template, at) as usize;
    let section = u32_at(&template, 58) as usize;
    let (head, inner, tail) = (
        &template[..at + 4],
        &template[at + 4..at + 4 + nested],
        &template[at + 4 + nested..],
    );
    const DEPTH: usize = 50_000;
    let mut blob = Vec::with_capacity(DEPTH * (head.len() + tail.len()) + inner.len());
    for k in 0..DEPTH {
        // Level `k` wraps the `DEPTH - k - 1` levels below it and the template's plain blob.
        let size = (DEPTH - k - 1) * (head.len() + tail.len()) + inner.len();
        let start = blob.len();
        blob.extend_from_slice(head);
        let level = &mut blob[start..];
        let section_len = section - nested + size;
        level[58..62].copy_from_slice(&u32::try_from(section_len).unwrap().to_le_bytes());
        level[at..at + 4].copy_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
        let check = crate::blob::hash_bytes(&level[..CHECKED]) as u32;
        level[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
    }
    blob.extend_from_slice(inner);
    for _ in 0..DEPTH {
        blob.extend_from_slice(tail);
    }
    let got = DictIndex::from_shared(SharedBytes::from_owned(blob), false);
    assert!(
        matches!(got, Err(IndexError::Format("dict: bad label dictionary"))),
        "{:?}",
        got.err()
    );
}

/// Every layout a labelled streamed build can take over `keys`, each in one run and spilled, against
/// the bytes the sorting build makes; the keys arrive shuffled and twice over.
fn assert_streams_labelled(name: &str, keys: &[String], layouts: &[DictLayout]) {
    let dir = std::env::temp_dir();
    let mut twice: Vec<&str> = keys.iter().map(String::as_str).collect();
    twice.extend(keys.iter().rev().map(String::as_str));
    for (i, &layout) in layouts.iter().enumerate() {
        let whole = DictIndex::build_with(keys, layout).unwrap().to_bytes();
        for budget in [1 << 24, 1 << 13] {
            let path = dir.join(format!(
                "lexindex_streamed_labels_{}_{name}_{i}_{budget}.bdx",
                std::process::id()
            ));
            let n =
                DictIndex::build_to_file_runs(&twice, &path, layout, || Ok(()), budget).unwrap();
            let written = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            assert_eq!(n, keys.len(), "{name} {i} {budget}");
            assert!(
                written == whole,
                "{name}: layout {i} at a budget of {budget} differs, {} vs {} bytes",
                written.len(),
                whole.len()
            );
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
                .filter(|f| f.contains(&format!("streamed_labels_{}_{name}_", std::process::id())))
                .collect();
            assert!(leftovers.is_empty(), "{name}: left behind {leftovers:?}");
        }
    }
}

#[test]
fn a_streamed_labelled_build_writes_the_blob_the_sorting_one_does() {
    let layouts = [
        DictLayout::new(256).with_labels(true),
        DictLayout::new(1024).with_labels(true),
        DictLayout::new(1024).with_micro(64).with_labels(true),
        DictLayout::new(32).with_labels(true),
    ];
    let wins = label_corpus(2, 4_000);
    assert_eq!(
        &DictIndex::build_with(&wins, layouts[0]).unwrap().to_bytes()[..4],
        LABELLED_MAGIC,
        "the corpus has to be one the labels win"
    );
    assert_streams_labelled("wins", &wins, &layouts);
    // Where the labels lose the plain blob has to come out, not the labelled one written first.
    let loses = label_corpus(1, 4_000);
    assert_streams_labelled("flip", &loses, &layouts);
    let words: Vec<String> = (0..3_000u32).map(|i| format!("{:x}", i * 7919)).collect();
    assert_streams_labelled("none", &words, &layouts);
    assert_streams_labelled("short", &["only".to_string()], &layouts);
    assert_streams_labelled("empty", &[], &layouts);
    // A character code takes no labels, sorted or streamed.
    let coded: Vec<String> = (0..2_000u32)
        .map(|i| {
            format!(
                "\u{4e2d}\u{6587}{}\u{5b57}{}",
                char::from_u32(0x4e00 + i % 512).unwrap(),
                i
            )
        })
        .collect();
    let mut coded = coded;
    coded.sort_unstable();
    assert!(
        DictIndex::build_with(&coded, layouts[0])
            .unwrap()
            .is_coded(),
        "the corpus has to be one a code wins"
    );
    assert_streams_labelled("coded", &coded, &layouts);
}

/// A parse names a block's labels under the block's own shard's symbols, whichever walk it is on.
#[test]
fn a_streamed_labelled_build_parses_a_block_under_its_own_shard() {
    let mut keys = label_corpus(2, 4_000);
    keys.sort_unstable();
    keys.dedup();
    for (block, shard) in [(32usize, 1usize), (32, 3), (8, 2), (4, 5)] {
        let _held = Shards::of(shard);
        let layout = DictLayout::new(block).with_labels(true);
        assert_streams_labelled(&format!("shard{block}-{shard}"), &keys, &[layout]);
    }
}

#[test]
fn a_streamed_build_takes_the_layout_the_sorting_one_does() {
    let keys = label_corpus(3, 1_500);
    let layouts = [
        DictLayout::new(256).with_micro(64),
        DictLayout::new(128).with_micro(128),
        DictLayout::new(1),
    ];
    assert_streams_labelled("plain", &keys, &layouts);
    let path = std::env::temp_dir().join(format!("lexindex_refused_{}", std::process::id()));
    for (layout, why) in [
        (DictLayout::new(0), "block"),
        (DictLayout::new(2048), "block"),
        (DictLayout::new(256).with_micro(3), "micro"),
        (DictLayout::new(256).with_micro(0), "micro"),
    ] {
        let err = DictIndex::build_to_file_with(&keys, &path, layout).unwrap_err();
        assert!(err.to_string().contains(why), "{err}");
        assert!(!path.exists());
    }
}

#[test]
fn a_late_word_from_the_caller_stops_a_labelled_build_with_nothing_published() {
    let keys = label_corpus(2, 2_000);
    let path = std::env::temp_dir().join(format!("lexindex_late_labels_{}", std::process::id()));
    std::fs::write(&path, b"what was there").unwrap();
    let mut asked = 0;
    let late = || {
        asked += 1;
        if asked == 2 {
            Err(IndexError::Format("the source failed"))
        } else {
            Ok(())
        }
    };
    let layout = DictLayout::new(256).with_labels(true);
    let err = DictIndex::build_to_file_runs(&keys, &path, layout, late, 1 << 13).unwrap_err();
    assert!(err.to_string().contains("the source failed"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), b"what was there");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_streamed_build_shards_its_tables_the_way_the_sorting_one_does() {
    let keys = corpus();
    let dir = scratch("dictshards");
    let path = dir.join("idx.bdx");
    let feed: Vec<&str> = keys.iter().map(String::as_str).collect();
    for (block, shard) in [(1usize, 3usize), (3, 2), (32, 2), (256, 1)] {
        let _held = Shards::of(shard);
        let want = DictIndex::build_with_block(&keys, block)
            .unwrap()
            .to_bytes();
        DictIndex::build_to_file_runs(&feed, &path, DictLayout::new(block), || Ok(()), 64).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            want,
            "block {block}, shard {shard}"
        );
        check(&DictIndex::load(&path).unwrap(), &keys);
    }
}

#[test]
fn the_encoding_does_not_depend_on_how_many_threads_ran() {
    let keys = corpus();
    for block in [1usize, 3, 32] {
        // A thread's range starts inside a shard as often as on one, and the encoder it picks
        // is the one the block's number names, so the split must not move a single byte.
        for shard in [0usize, 2] {
            let _held = Shards::of(shard);
            let want = DictIndex::from_sorted_on(&keys, block, micro_for(block), 1)
                .unwrap()
                .to_bytes();
            for threads in [2usize, 5, 64] {
                let got =
                    DictIndex::from_sorted_on(&keys, block, micro_for(block), threads).unwrap();
                assert_eq!(
                    got.to_bytes(),
                    want,
                    "block {block}, shard {shard}, {threads} threads"
                );
                check(&got, &keys);
            }
        }
    }
}

#[test]
fn a_sorted_build_is_byte_identical_to_the_sorting_one() {
    let keys = corpus();
    // The same key set scrambled with repeats and in order with adjacent repeats. The blob is
    // the contract, so compare the bytes rather than the answers -- and the scrambled input is
    // descending from its first pair, so the sorted builder must refuse it.
    let mut scrambled: Vec<&str> = keys.iter().map(String::as_str).collect();
    scrambled.reverse();
    scrambled.extend(keys.iter().step_by(3).map(String::as_str));
    let mut repeated: Vec<&str> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        repeated.push(k);
        if i % 3 == 0 {
            repeated.push(k);
        }
    }
    for block in [1usize, 3, 32, MAX_BLOCK] {
        let want = DictIndex::build_with_block(scrambled.iter().copied(), block).unwrap();
        let got = DictIndex::build_sorted_with_block(repeated.iter().copied(), block).unwrap();
        assert_eq!(got.to_bytes(), want.to_bytes(), "block {block}");
        check(&got, &keys);
        let err = DictIndex::build_sorted_with_block(scrambled.iter().copied(), block).unwrap_err();
        assert!(err.to_string().contains("below its predecessor"), "{err}");
    }
    assert_eq!(
        DictIndex::build_sorted(&keys).unwrap().to_bytes(),
        DictIndex::build(&keys).unwrap().to_bytes()
    );
}

#[test]
fn a_single_descent_anywhere_in_a_sorted_build_is_refused() {
    let keys = corpus();
    for at in [0usize, 1, keys.len() / 2, keys.len() - 2] {
        let mut swapped: Vec<&str> = keys.iter().map(String::as_str).collect();
        swapped.swap(at, at + 1);
        let err = DictIndex::build_sorted(swapped).unwrap_err();
        assert!(err.to_string().contains("below its predecessor"), "{err}");
    }
    assert!(
        DictIndex::build_sorted(Vec::<&str>::new())
            .unwrap()
            .is_empty()
    );
    assert_eq!(DictIndex::build_sorted(["a", "a", "a"]).unwrap().len(), 1);
}

#[test]
fn varints_round_trip_and_a_cut_or_oversized_one_is_refused() {
    for v in [
        0usize,
        1,
        127,
        128,
        300,
        1 << 20,
        usize::MAX >> 1,
        usize::MAX,
    ] {
        let mut out = Vec::new();
        put_varint(&mut out, v);
        out.push(0xAB);
        assert_eq!(get_varint(&out), Some((v, &[0xABu8][..])));
    }
    assert_eq!(get_varint(&[]), None);
    assert_eq!(get_varint(&[0x80]), None);
    assert_eq!(get_varint(&[0x80; 12]), None);
    // A run of two entries: the header codes come first and the suffixes after them, and a
    // pair the code cannot name is read from the head of its own suffix, not from between the
    // headers.
    let pairs = [(14usize, 14usize), (15, 3)];
    let mut sfx = Vec::from([b'a'; 14]);
    sfx.extend_from_slice(b"bcd");
    let code = Code::choose_cost(&[&pairs[..]]).0;
    // The run laid out by hand — the production writer, interleaved here rather than through
    // a whole shard, because what is under test is the reader and its bounds.
    let data = {
        let inverse = paircode::Inverse::of(&code);
        let mut writer = paircode::Writer::new(&inverse, &pairs, &mut paircode::Widths::default());
        let (mut out, mut body) = (Vec::new(), Vec::new());
        let mut off = 0;
        for &(l, len) in &pairs {
            writer.push(l, len, &mut body);
            body.extend_from_slice(&sfx[off..off + len]);
            off += len;
        }
        writer.finish(&mut out);
        out.extend_from_slice(&body);
        out
    };
    let codec = Codec::Symbols {
        table: Table::default(),
        split: None,
    };
    let mut entries = Entries::of(&code, &codec, &data, 3);
    assert_eq!(entries.head(), Some((14, 14)));
    assert_eq!(entries.piece(14).bytes(), &[b'a'; 14]);
    assert_eq!(entries.head(), Some((15, 3)));
    assert_eq!(entries.piece(3).bytes(), b"bcd");
    assert_eq!(entries.head(), None);
    // A stream that stops mid-entry gives short answers, never a panic.
    assert_eq!(Entries::of(&code, &codec, &[], 1).head(), None);
    assert_eq!(Entries::of(&code, &codec, &[], 2).head(), None);
    let mut cut = Entries::of(&code, &codec, &data[..data.len() - 2], 3);
    assert_eq!(cut.head(), Some((14, 14)));
    assert_eq!(cut.piece(14).bytes(), &[b'a'; 14]);
    assert_eq!(cut.head(), Some((15, 3)));
    assert_eq!(cut.piece(3).bytes(), b"b");
    let mut past = Entries::of(&code, &codec, &data, 3);
    assert_eq!(past.head(), Some((14, 14)));
    past.skip(usize::MAX);
    assert_eq!(past.piece(1).bytes(), b"");
}

#[test]
fn save_and_load() {
    let keys = corpus();
    let idx = DictIndex::build(&keys).unwrap();
    let dir = std::env::temp_dir().join(format!("lexindex-dict-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("keys.bdx");
    idx.save(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes());
    let back = DictIndex::load(&path).unwrap();
    check(&back, &keys);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(DictIndex::load(&path).is_err());
}

/// Where every section of a blob starts, read out of its own header.
struct Layout {
    codecs: usize,
    phrases: usize,
    head_bases: usize,
    head_deltas: usize,
    data: usize,
    block_bases: usize,
    block_deltas: usize,
    micro_deltas: usize,
    chars: usize,
    head_width: u32,
    block_width: u32,
    micro_width: u32,
    nb: usize,
}

fn layout(blob: &[u8], block: usize) -> Layout {
    let u64_at = |i: usize| u64::from_le_bytes(blob[i..i + 8].try_into().unwrap()) as usize;
    let (n, nb) = (u64_at(4), u64_at(4).div_ceil(block));
    let table_len = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
    let (head_width, block_width, shift, micro_width) = (
        u32::from(blob[44]),
        u32::from(blob[45]),
        u32::from(blob[46]),
        u32::from(blob[47]),
    );
    let micro = u16::from_le_bytes(blob[48..50].try_into().unwrap()) as usize;
    assert_eq!(shift, offsets::SHIFT);
    assert_eq!(micro, micro_for(block));
    assert!(head_width > 0 && block_width > 0, "a corpus with no spread");
    let nm = match nb {
        0 => 0,
        _ if micro >= block => 0,
        nb => (nb - 1) * block.div_ceil(micro) + (n - (nb - 1) * block).div_ceil(micro),
    };
    let head_bases = HEADER + u64_at(16);
    let head_deltas = head_bases + offsets::bases_len(nb, shift);
    let data = head_deltas + offsets::deltas_len(nb, head_width);
    let codecs = data + u64_at(24);
    let codes = codecs + table_len;
    let phrases = codes + u32::from_le_bytes(blob[52..56].try_into().unwrap()) as usize;
    let block_bases = phrases + u32::from_le_bytes(blob[58..62].try_into().unwrap()) as usize;
    let block_deltas = block_bases + offsets::bases_len(nb, shift);
    let micro_deltas = block_deltas + offsets::deltas_len(nb, block_width);
    let chars = micro_deltas + offsets::deltas_len(nm, micro_width);
    let chars_len = match &blob[..4] {
        m if m == CODED_MAGIC => u32::from_le_bytes(blob[62..66].try_into().unwrap()) as usize,
        _ => 0,
    };
    assert_eq!(chars + chars_len, blob.len());
    Layout {
        codecs,
        phrases,
        head_bases,
        head_deltas,
        data,
        block_bases,
        block_deltas,
        micro_deltas,
        chars,
        head_width,
        block_width,
        micro_width,
        nb,
    }
}

/// The base entry `i` is measured from, as it stands and as it is set.
fn base_of(blob: &[u8], at: usize, i: usize) -> u64 {
    let at = at + (i >> offsets::SHIFT) * 8;
    u64::from_le_bytes(blob[at..at + 8].try_into().unwrap())
}

fn set_base(blob: &mut [u8], at: usize, i: usize, v: u64) {
    let at = at + (i >> offsets::SHIFT) * 8;
    blob[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

/// Entry `i`'s delta, in place, so an edit names one entry rather than a run of bytes.
fn set_delta(blob: &mut [u8], at: usize, width: u32, i: usize, v: u64) {
    let bit = i * width as usize;
    let (at, off) = (at + bit / 8, bit % 8);
    let mask = ((1u64 << width) - 1) << off;
    let word = u64::from_le_bytes(blob[at..at + 8].try_into().unwrap());
    blob[at..at + 8].copy_from_slice(&(((word & !mask) | ((v << off) & mask)).to_le_bytes()));
}

/// Recompute both checksums after a deliberate edit, so the structural checks are reached.
fn reframe(blob: &mut [u8]) {
    let payload = crate::blob::hash_block(&blob[HEADER..]);
    blob[36..44].copy_from_slice(&payload.to_le_bytes());
    let check = crate::blob::hash_bytes(&blob[..CHECKED]) as u32;
    blob[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
}

/// `from_bytes` refuses `blob` naming `what`; the mapping's loader — framing only — refuses
/// it too unless the refusal is the payload checksum's or the layout walk's.
fn refused(blob: &[u8], what: &str) {
    let err = DictIndex::from_bytes(blob).unwrap_err().to_string();
    assert!(err.contains(what), "expected {what:?}, got {err:?}");
    let walked = [
        "payload checksum",
        "out of order",
        "does not cover",
        "head samples",
        "empty index",
    ];
    let framing = !walked.iter().any(|w| what.contains(w));
    let framed = DictIndex::from_shared(crate::blob::SharedBytes::from_owned(blob.to_vec()), false);
    assert_eq!(framed.is_err(), framing, "{what:?} on the mapping's path");
}

#[test]
fn every_length_and_table_in_a_blob_is_checked() {
    let keys = corpus();
    let blob = DictIndex::build_with_block(&keys, 2).unwrap().to_bytes();
    let heads_len = u64::from_le_bytes(blob[16..24].try_into().unwrap()) as usize;
    let l = layout(&blob, 2);
    let nb = l.nb;

    refused(&blob[..HEADER - 1], "truncated");
    let mut b = blob.clone();
    b[..4].copy_from_slice(b"BCL2");
    refused(&b, "bad magic");
    let mut b = blob.clone();
    b[4] ^= 1;
    refused(&b, "header checksum");
    let mut b = blob.clone();
    b[l.data] ^= 1;
    refused(&b, "payload checksum");

    let edited = |edit: &dyn Fn(&mut Vec<u8>), what: &str| {
        let mut b = blob.clone();
        edit(&mut b);
        reframe(&mut b);
        refused(&b, what);
    };
    edited(
        &|b| b[12..16].copy_from_slice(&0u32.to_le_bytes()),
        "block size",
    );
    edited(
        &|b| b[12..16].copy_from_slice(&1025u32.to_le_bytes()),
        "block size",
    );
    edited(
        &|b| b[4..12].copy_from_slice(&(keys.len() as u64 + 2).to_le_bytes()),
        "add up",
    );
    edited(
        &|b| b[16..24].copy_from_slice(&(heads_len as u64 + 1).to_le_bytes()),
        "add up",
    );
    edited(
        &|b| b[4..12].copy_from_slice(&u64::MAX.to_le_bytes()),
        "out of range",
    );
    edited(&|b| b[44] = 57, "offset widths out of range");
    edited(&|b| b[46] = 32, "offset widths out of range");
    edited(&|b| b[47] = 57, "offset widths out of range");
    edited(
        &|b| b[48..50].copy_from_slice(&0u16.to_le_bytes()),
        "microblock size out of range",
    );
    edited(
        &|b| b[48..50].copy_from_slice(&3u16.to_le_bytes()),
        "microblock size out of range",
    );
    edited(
        &|b| b[50..52].copy_from_slice(&0u16.to_le_bytes()),
        "symbol-table shard out of range",
    );
    edited(&|b| b[l.phrases] = 1, "bad phrase dictionary");
    edited(&|b| b[l.codecs] = 255, "bad symbol table");
    edited(&|b| b[l.codecs + 4] = 255, "bad symbol table");
    edited(
        &|b| set_base(b, l.head_bases, 0, u64::MAX),
        "head table out of order",
    );
    edited(
        &|b| {
            // The last superblock a byte lower: still in order, one byte short of the heads.
            let base = base_of(b, l.head_bases, nb - 1);
            set_base(b, l.head_bases, nb - 1, base - 1);
        },
        "does not cover",
    );
    edited(
        &|b| set_base(b, l.block_bases, 0, 1),
        "block table out of order",
    );
    edited(
        &|b| set_delta(b, l.block_deltas, l.block_width, 1, u64::MAX),
        "block table out of order",
    );
    edited(&|b| b.push(0), "add up");
    // A block of two is one microblock, so this blob stores no microblock starts; the two
    // edits to that array need a block that has one.
    assert_eq!(
        l.micro_deltas,
        blob.len(),
        "no microblock starts at one microblock a block"
    );
    let blob = DictIndex::build_with_block(&keys, 64).unwrap().to_bytes();
    let l = layout(&blob, 64);
    assert!(
        l.micro_deltas < blob.len(),
        "two microblocks a block store their starts"
    );
    let edited = |edit: &dyn Fn(&mut Vec<u8>), what: &str| {
        let mut b = blob.clone();
        edit(&mut b);
        reframe(&mut b);
        refused(&b, what);
    };
    edited(
        &|b| set_delta(b, l.micro_deltas, l.micro_width, 0, u64::MAX),
        "block table out of order",
    );
    edited(
        &|b| set_delta(b, l.micro_deltas, l.micro_width, 1, u64::MAX),
        "block table out of order",
    );

    let empty = DictIndex::build(Vec::<String>::new()).unwrap().to_bytes();
    let mut b = empty.clone();
    b[16..24].copy_from_slice(&1u64.to_le_bytes());
    b.insert(HEADER, b'k');
    reframe(&mut b);
    refused(&b, "empty index with key bytes");
}

/// Block data the crate did not write — every byte an escape, a code past the table, a header
/// claiming more than is there — answers wrong or short, never out of bounds.
#[test]
fn corrupted_block_data_never_panics() {
    let keys = corpus();
    let (block, micro) = (64, micro_for(64));
    assert!(
        micro < block && keys.len() > micro,
        "two microblocks in the first block"
    );
    let blob = DictIndex::build_with_block(&keys, block)
        .unwrap()
        .to_bytes();
    let data_len = u64::from_le_bytes(blob[24..32].try_into().unwrap()) as usize;
    let data_at = layout(&blob, block).data;
    for fill in [0x00u8, 0x0F, 0xF0, 0xFE, 0xFF] {
        let mut b = blob.clone();
        b[data_at..data_at + data_len].fill(fill);
        reframe(&mut b);
        let idx = DictIndex::from_bytes(&b).unwrap();
        for p in keys.iter().chain(probes(&keys).iter()) {
            let _ = (idx.id(p), idx.lower_bound(p));
        }
        let mut buf = String::new();
        for id in 0..keys.len() as u64 {
            let _ = (idx.key(id), idx.key_into(id, &mut buf));
        }
        assert!(idx.iter().count() <= keys.len());
    }
    // A restart run whose frame bases every shared-prefix length past the key so far, with
    // the rest of the block intact. A block opens on its restarts, so this one breaks the walk
    // at the second microblock and the first still answers.
    let mut b = blob.clone();
    b[data_at] = 127;
    reframe(&mut b);
    let idx = DictIndex::from_bytes(&b).unwrap();
    assert_eq!(idx.key(0).as_deref(), Some(keys[0].as_str()));
    let _ = idx.key(1);
    assert_eq!(idx.iter().count(), micro);
    assert_eq!(idx.key(micro as u64), None);
}

/// The mapping's loader takes the per-block arrays as they are, so every access bounds them:
/// an end out of order or past the heads, a start past the data, a sample that is not its
/// head's — wrong answers and short walks, never a panic, and the sections written back as
/// they were read.
#[test]
fn a_load_without_the_walk_bounds_what_the_arrays_say() {
    let keys = corpus();
    let blob = DictIndex::build_with_block(&keys, 4).unwrap().to_bytes();
    let l = layout(&blob, 4);
    let nb = l.nb;
    type Edit<'a> = &'a dyn Fn(&mut Vec<u8>);
    let edits: [Edit; 7] = [
        &|b| set_base(b, l.head_bases, 0, u64::MAX),
        &|b| {
            let base = base_of(b, l.head_bases, nb - 1);
            set_base(b, l.head_bases, nb - 1, base - 1);
        },
        &|b| set_delta(b, l.head_deltas, l.head_width, nb / 2, u64::MAX),
        &|b| set_base(b, l.block_bases, 0, u64::MAX),
        &|b| set_delta(b, l.block_deltas, l.block_width, nb / 2, u64::MAX),
        &|b| set_base(b, l.block_bases, nb - 1, 0),
        &|b| set_delta(b, l.block_deltas, l.block_width, nb - 1, u64::MAX),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut b = blob.clone();
        edit(&mut b);
        reframe(&mut b);
        assert!(
            DictIndex::from_bytes(&b).is_err(),
            "edit {i}: the walk refuses this"
        );
        let idx =
            DictIndex::from_shared(crate::blob::SharedBytes::from_owned(b.clone()), false).unwrap();
        assert_eq!(idx.len(), keys.len());
        for p in keys.iter().chain(probes(&keys).iter()) {
            let _ = (idx.id(p), idx.contains(p), idx.lower_bound(p));
        }
        let mut buf = String::new();
        for id in 0..=keys.len() as u64 {
            let _ = (idx.key(id), idx.key_into(id, &mut buf));
        }
        assert!(idx.iter().count() <= keys.len(), "edit {i}");
        assert_eq!(idx.to_bytes(), b, "edit {i}");
    }
}

/// `load_mmap` borrows every section from the file and answers like the owned index;
/// `load_mmap_verified` adds the checks `load` makes, so a flipped payload byte the plain
/// mapping takes is refused by it.
#[cfg(feature = "mmap")]
#[test]
fn load_mmap_borrows_the_file_and_answers_like_the_owned_index() {
    let keys = corpus();
    let idx = DictIndex::build_with_block(&keys, 3).unwrap();
    let path = std::env::temp_dir().join(format!("lexindex_dict_mmap_{}.bdx", std::process::id()));
    idx.save(&path).unwrap();
    // SAFETY: the file is written above and not touched while a mapping of it is alive.
    let mapped = unsafe { DictIndex::load_mmap(&path) }.unwrap();
    check(&mapped, &keys);
    assert_eq!(mapped.to_bytes(), idx.to_bytes());
    // SAFETY: as above.
    let verified = unsafe { DictIndex::load_mmap_verified(&path) }.unwrap();
    check(&verified, &keys);
    drop((mapped, verified));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 0x55; // block data: a suffix code
    std::fs::write(&path, &bytes).unwrap();
    // SAFETY: as above — the rewrite happened with no mapping alive.
    let mapped = unsafe { DictIndex::load_mmap(&path) }.unwrap();
    assert_eq!(mapped.len(), keys.len());
    // SAFETY: as above.
    let err = unsafe { DictIndex::load_mmap_verified(&path) }.unwrap_err();
    assert!(err.to_string().contains("payload checksum"), "{err}");
    assert!(DictIndex::load(&path).is_err());
    drop(mapped);
    std::fs::remove_file(&path).unwrap();
    // SAFETY: nothing to map; the open fails.
    assert!(unsafe { DictIndex::load_mmap(&path) }.is_err());
}

/// Bytes of every length to twenty, drawn from a fixed seed; the pair differs at `at`, or is
/// equal over the shorter one where `at` is past it.
fn differing_pair(seed: &mut u64, la: usize, lb: usize, at: usize) -> (Vec<u8>, Vec<u8>) {
    let mut byte = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 56) as u8
    };
    let a: Vec<u8> = (0..la).map(|_| byte()).collect();
    let mut b: Vec<u8> = a[..la.min(lb)].to_vec();
    while b.len() < lb {
        b.push(byte());
    }
    if at < la.min(lb) {
        b[at] = a[at].wrapping_add(1 + byte() % 255);
    }
    (a, b)
}

#[test]
fn lcp_agrees_with_the_byte_definition() {
    // Every pair of lengths to twenty bytes with the first differing byte at every position, or
    // none: the whole-word loop, the last-word tail, the two quads and the three bytes all get
    // their turn, in both orders.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for la in 0..=20 {
        for lb in 0..=20 {
            for at in 0..=la.min(lb) {
                let (a, b) = differing_pair(&mut seed, la, lb, at);
                let naive = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
                assert_eq!(lcp(&a, &b), naive, "{a:?} {b:?}");
                assert_eq!(lcp(&b, &a), naive, "{b:?} {a:?}");
            }
        }
    }
    assert_eq!(lcp(b"", b""), 0);
    assert_eq!(lcp(b"a\0", b"a"), 1);
    assert_eq!(lcp(b"a\0b", b"a\0c"), 2);
}
