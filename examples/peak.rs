//! Peak resident memory of a single index build, measured in-process on Linux.
//!
//! Run with `cargo run --release --example peak -- <string|perfect|compact> [n] [fp_bytes]`
//! (`fp_bytes` ∈ {1, 2, 4} applies to `compact` only). Peak RSS is a high-water mark, so one index
//! per process: measuring two in one run would report only the larger one. The driver that fills
//! the CHANGELOG table therefore invokes this example once per index.
//!
//! `build` is the wall time of the single `build` call, on its own in a fresh process — the same
//! operation `examples/bench.rs` times, but with nothing else in the address space to perturb the
//! allocator. Two memory numbers follow. **keys** is the high-water mark after the input
//! `Vec<String>` exists but before the build starts — the floor any builder has to pay for its
//! input. **peak** is the high-water mark after the index is built, so `peak - keys` is what the
//! build itself added on top, which is the number the build-memory work is trying to move.
//!
//! Keys are the same real dictionary-word bigrams `examples/bench.rs` uses; synthetic sequences
//! would understate the arena and flatter every structure.

use lexindex::StringIndex;
#[cfg(feature = "mph")]
use lexindex::{CompactHashIndex, PerfectHashIndex};

/// The kernel's own high-water mark for this process, in bytes (`VmHWM` from `/proc/self/status`).
/// Linux-only by design: this is a diagnostic example, not shipped library code.
fn peak_rss() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status")
        .expect("peak RSS is read from /proc/self/status; this example is Linux-only");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .expect("VmHWM line carries a kB value");
            return kb * 1024;
        }
    }
    panic!("no VmHWM line in /proc/self/status");
}

fn load_vocab() -> Vec<String> {
    let path = std::env::var("LEXINDEX_BENCH_WORDS")
        .unwrap_or_else(|_| "/usr/share/dict/words".to_string());
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!(
            "no word list at {path}; set LEXINDEX_BENCH_WORDS or install a system dictionary"
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

fn report(label: &str, n: usize, keys_rss: u64, blob: usize, build_ms: f64) {
    let peak = peak_rss();
    let per_key = |b: u64| b as f64 / n as f64;
    println!(
        "{label:<12} n {n:>9}  build {build_ms:>7.1} ms  keys {:>7.1} MB ({:>5.1} B/key)  \
         peak {:>7.1} MB ({:>5.1} B/key)  \
         build adds {:>7.1} MB ({:>5.1} B/key)  blob {:>5.2} B/key",
        keys_rss as f64 / 1e6,
        per_key(keys_rss),
        peak as f64 / 1e6,
        per_key(peak),
        peak.saturating_sub(keys_rss) as f64 / 1e6,
        per_key(peak.saturating_sub(keys_rss)),
        blob as f64 / n as f64,
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "string".to_string());
    let n: usize = args
        .next()
        .and_then(|a| a.parse().ok())
        .unwrap_or(2_000_000);
    // `compact` only: the fingerprint width, in bytes. The build's peak is expected to move by
    // exactly this much per key, which is how the documented formula gets checked.
    #[cfg(feature = "mph")]
    let fp_bytes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1);

    let keys = make_keys(n, &load_vocab());
    let keys_rss = peak_rss();

    match which.as_str() {
        "string" => {
            let t0 = std::time::Instant::now();
            let idx = StringIndex::build(&keys).unwrap();
            let build_ms = t0.elapsed().as_secs_f64() * 1e3;
            let blob = idx.serialized_len();
            report("StringIndex", n, keys_rss, blob, build_ms);
            std::hint::black_box(idx);
        }
        #[cfg(feature = "mph")]
        "perfect" => {
            let t0 = std::time::Instant::now();
            let idx = PerfectHashIndex::build(&keys).unwrap();
            let build_ms = t0.elapsed().as_secs_f64() * 1e3;
            let blob = idx.serialized_len().unwrap();
            report("PerfectHash", n, keys_rss, blob, build_ms);
            std::hint::black_box(idx);
        }
        #[cfg(feature = "mph")]
        "compact" => {
            let t0 = std::time::Instant::now();
            let idx = CompactHashIndex::build(&keys, fp_bytes).unwrap();
            let build_ms = t0.elapsed().as_secs_f64() * 1e3;
            let blob = idx.serialized_len().unwrap();
            report(
                &format!("CompactHash/{fp_bytes}"),
                n,
                keys_rss,
                blob,
                build_ms,
            );
            std::hint::black_box(idx);
        }
        other => {
            eprintln!("unknown index {other:?}; expected string, perfect or compact");
            std::process::exit(1);
        }
    }
}
