//! Build / query / size comparison of `lexindex` against the `std` maps it competes with.
//!
//! Run with `cargo run --release --example bench` (release matters — `lto` + `opt-level=3`). Numbers
//! are illustrative and machine-dependent; the *ratios* are the point. A deterministic stride walks
//! the keys in a non-sequential order so lookups are not pure cache hits, and a checksum is printed
//! so the optimiser cannot elide the queries.
//!
//! Keys are **real dictionary-word bigrams** (`word_i.word_j`, the same generator as
//! `bench/scale.py`), never synthetic sequences: sequential keys collapse the FST to a near-regular
//! automaton and make every structure look better than it is on natural data. The word list comes
//! from `$LEXINDEX_BENCH_WORDS` or `/usr/share/dict/words`; the benchmark refuses to run without
//! one rather than silently substituting synthetic keys.

use lexindex::{CompactHashIndex, PerfectHashIndex, StringIndex};
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

fn load_vocab() -> Vec<String> {
    let path = std::env::var("LEXINDEX_BENCH_WORDS")
        .unwrap_or_else(|_| "/usr/share/dict/words".to_string());
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "no word list at {path}; set LEXINDEX_BENCH_WORDS or install a system dictionary \
             (this benchmark refuses synthetic keys — they misrepresent every structure here)"
        );
        std::process::exit(1);
    };
    let mut words: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect();
    words.sort_unstable();
    words.dedup();
    words
}

/// `n` distinct realistic compound keys `word_i.word_j` — natural prefix sharing, high entropy.
fn make_keys(n: usize, vocab: &[String]) -> Vec<String> {
    let m = (n as f64).sqrt().ceil() as usize;
    let v = &vocab[..m.min(vocab.len())];
    let w = v.len();
    assert!(n <= w * w, "vocabulary too small for {n} distinct bigrams");
    (0..n)
        .map(|k| format!("{}.{}", v[k % w], v[(k / w) % w]))
        .collect()
}

fn bench<T>(label: &str, n: usize, build: impl FnOnce() -> T, query: impl Fn(&T) -> u64) {
    let t0 = Instant::now();
    let s = build();
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Warm once, then take the min of several timed passes (least-noisy estimate of ns per lookup).
    let mut checksum = query(&s);
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let t1 = Instant::now();
        checksum = query(&s);
        best = best.min(t1.elapsed().as_secs_f64() * 1e9 / n as f64);
    }
    println!(
        "{label:26} build {build_ms:8.1} ms   lookup {best:6.1} ns/op   (checksum {checksum})"
    );
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let vocab = load_vocab();
    let keys = make_keys(n, &vocab);
    // Non-sequential, full-coverage probe order: `i * STEP mod n` visits every index exactly once
    // because STEP = 2 654 435 761 is prime, hence coprime with every n below it.
    const STEP: usize = 0x9E37_79B1;
    let probe: Vec<usize> = (0..n).map(|i| (i.wrapping_mul(STEP)) % n).collect();

    let mean_len = keys.iter().map(String::len).sum::<usize>() as f64 / n as f64;
    println!("lexindex bench — n = {n} dictionary-word bigrams (mean len {mean_len:.1})\n");

    bench(
        "lexindex StringIndex (FST)",
        n,
        || StringIndex::build(&keys).unwrap(),
        |idx| probe.iter().map(|&i| idx.id(&keys[i]).unwrap_or(0)).sum(),
    );
    bench(
        "lexindex PerfectHashIndex",
        n,
        || PerfectHashIndex::build(&keys).unwrap(),
        |idx| {
            probe
                .iter()
                .map(|&i| idx.id(&keys[i]).map_or(0, u64::from))
                .sum()
        },
    );
    bench(
        "lexindex PHIndex unchecked",
        n,
        || PerfectHashIndex::build(&keys).unwrap(),
        |idx| {
            probe
                .iter()
                .map(|&i| u64::from(idx.id_unchecked(&keys[i])))
                .sum()
        },
    );
    bench(
        "lexindex CompactHash (fp=1)",
        n,
        || CompactHashIndex::build(&keys, 1).unwrap(),
        |idx| {
            probe
                .iter()
                .map(|&i| idx.id(&keys[i]).map_or(0, u64::from))
                .sum()
        },
    );
    bench(
        "std HashMap<String,u32>",
        n,
        || {
            keys.iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), i as u32))
                .collect::<HashMap<_, _>>()
        },
        |m| {
            probe
                .iter()
                .map(|&i| m.get(&keys[i]).copied().map_or(0, u64::from))
                .sum()
        },
    );
    bench(
        "std BTreeMap<String,u32>",
        n,
        || {
            keys.iter()
                .enumerate()
                .map(|(i, k)| (k.clone(), i as u32))
                .collect::<BTreeMap<_, _>>()
        },
        |m| {
            probe
                .iter()
                .map(|&i| m.get(&keys[i]).copied().map_or(0, u64::from))
                .sum()
        },
    );

    // Serialised size: the build-once / query-many payload you persist or mmap. Bigrams share more
    // prefixes than single words, so StringIndex compresses further here than the single-word
    // figures quoted in the README's size table — different corpus, different number.
    let si = StringIndex::build(&keys).unwrap();
    let ph = PerfectHashIndex::build(&keys).unwrap();
    let ch = CompactHashIndex::build(&keys, 1).unwrap();
    let raw = keys.iter().map(String::len).sum::<usize>();
    println!("\nserialised size (bytes/key):");
    println!(
        "  lexindex CompactHashIndex   {:6.2}",
        ch.to_bytes().unwrap().len() as f64 / n as f64
    );
    println!(
        "  lexindex StringIndex blob   {:6.2}",
        si.to_bytes().len() as f64 / n as f64
    );
    println!(
        "  lexindex PerfectHashIndex   {:6.2}",
        ph.to_bytes().unwrap().len() as f64 / n as f64
    );
    println!("  raw key bytes (no index)  {:6.2}", raw as f64 / n as f64);

    // A capability the maps do not have: typo-tolerant + prefix queries over the same structure.
    let sample = &keys[keys.len() / 2];
    let stem = &sample[..sample.len().min(4)];
    let t = Instant::now();
    let fuzzy = si.fuzzy(sample, 1).unwrap().len();
    let pfx = si.prefix(stem).len();
    println!(
        "\nStringIndex extras: fuzzy(\"{sample}\", 1) hit {fuzzy} match(es), \
         prefix(\"{stem}\") hit {pfx}, in {:.2} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
}
