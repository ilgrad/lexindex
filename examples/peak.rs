//! Peak resident memory of a single index build, measured in-process on Linux.
//!
//! Run with `cargo run --release --example peak -- <string|string-sorted|perfect|compact> [n]
//! [fp_bytes]` (`fp_bytes` ∈ {1, 2, 4} applies to `compact` only). Peak RSS is a high-water mark, so
//! one index per process: measuring two in one run would report only the larger one. The driver that
//! fills the CHANGELOG table therefore invokes this example once per index.
//!
//! `string-sorted` is the odd one out and the reason is the point of it: it never builds a key list
//! at all, streaming an ascending generator through `build_sorted_to_file`, so its **keys** column is
//! the process baseline rather than the corpus.
//!
//! The high-water mark is reset once the word list is loaded, so **keys** and **peak** measure this
//! run's own allocations rather than the transient the vocabulary load leaves behind.
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

/// Reset the kernel's high-water mark to the current RSS (`CLEAR_REFS_MM_HIWATER_RSS`).
///
/// Without this the baseline is not a baseline: loading the word list reads the whole file into one
/// `String` and splits it into ~480 000 more, so its *transient* peak sits well above the steady
/// state it leaves behind, and every later allocation smaller than that headroom is invisible. That
/// is not a rounding error — it is the difference between "the generator costs nothing" and "the
/// generator costs less than the slack the vocabulary load happened to leave".
fn reset_peak_rss() {
    // Best effort: the file exists on Linux ≥ 4.0 and this is a Linux-only example, but a refusal
    // must not fail the run — it only means the baseline keeps the load's transient in it, which is
    // the behaviour every measurement here had before.
    let _ = std::fs::write("/proc/self/clear_refs", b"5");
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

/// The same key shape as [`make_keys`] in **ascending** order, produced lazily — nothing
/// materialises, which is the point: a sorted build that had to hold its input would not be a
/// streaming build.
///
/// `v[i].v[j]` with `i` varying slowest is lexicographic *only if* no word can be extended past a
/// prefix of another by a character below the separator. The system dictionary breaks that outright:
/// `'tween-decks.&c` sorts before `'tween.ARU` because `-` (0x2D) is below `.` (0x2E), so a naive
/// row-major walk hands the transducer a descending pair and the build rightly refuses it. Dropping
/// the words that contain any byte at or below the separator restores the property for any
/// vocabulary; on `/usr/share/dict/words` it costs the apostrophe and hyphen forms, and the keys
/// that remain have the same shape as every other benchmark here.
fn iter_sorted_keys(n: usize, vocab: &[String]) -> impl Iterator<Item = String> + '_ {
    let mut v: Vec<&String> = vocab
        .iter()
        .filter(|w| w.bytes().all(|b| b > b'.'))
        .collect();
    let m = (n as f64).sqrt().ceil() as usize;
    v.truncate(m.min(v.len()));
    let w = v.len();
    assert!(n <= w * w, "vocabulary too small for {n} distinct bigrams");
    (0..n).map(move |k| format!("{}.{}", v[k / w], v[k % w]))
}

/// splitmix64, so the sparse generator below picks the same second words on every run.
fn mix(a: u64, b: u64) -> u64 {
    let mut z = a
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(b.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Ascending keys that are **not** a cross product: every word of the filtered vocabulary gets a
/// pseudo-random handful of second words rather than all of them.
///
/// [`iter_sorted_keys`] is the grid the rest of this repo benchmarks on, and `docs/design.md` now
/// says outright that a grid is the most favourable set a transducer can be handed. A memory claim
/// measured only there would be a claim about the corpus, so this generator exists to check the same
/// number on a set with the regularity taken out — still lazily ascending, because the first word
/// ascends and the second words within it are sorted.
fn iter_sparse_sorted_keys(n: usize, vocab: &[String]) -> impl Iterator<Item = String> + '_ {
    let v: Vec<&String> = vocab
        .iter()
        .filter(|w| w.bytes().all(|b| b > b'.'))
        .collect();
    let w = v.len();
    assert!(w > 0, "no vocabulary left after filtering");
    let per = n.div_ceil(w);
    (0..w).flat_map(move |i| {
        let mut js: Vec<usize> = (0..per)
            .map(|t| (mix(i as u64, t as u64) % w as u64) as usize)
            .collect();
        js.sort_unstable();
        js.dedup();
        js.into_iter()
            .map(|j| format!("{}.{}", v[i], v[j]))
            .collect::<Vec<_>>()
            .into_iter()
    })
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

/// Consume a key stream without building anything, returning how many keys went past.
fn drain_keys(keys: impl Iterator<Item = String>) -> usize {
    let mut n = 0;
    for k in keys {
        std::hint::black_box(&k);
        n += 1;
    }
    n
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

    // Two dispatches, split by the thing being measured: the streaming modes below never let a key
    // list exist, so their `keys` column is the process baseline and not the corpus. The
    // materialising modes after them all start from one.
    if let Some(sorted) = which.strip_prefix("string-sorted") {
        let sparse = sorted.starts_with("-sparse");
        let drain = sorted.ends_with("-gen");
        let vocab = load_vocab();
        reset_peak_rss();
        let keys_rss = peak_rss();
        // `LEXINDEX_PEAK_DIR` because `temp_dir()` is tmpfs on many Linux systems, and a
        // "streamed to disk" claim measured with the bytes landing in RAM invites the obvious
        // objection — process RSS is the same either way, but the run should be reproducible on a
        // real filesystem.
        let dir = std::env::var("LEXINDEX_PEAK_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        let path = dir.join(format!("lexindex_peak_{}.bix", std::process::id()));
        let t0 = std::time::Instant::now();
        // `-gen` drains the generator and builds nothing. A generator that allocates a `String` per
        // key leaves the allocator holding freed arenas, and that retention is charged to the
        // process just as the builder's own memory is; measuring it separately is the only way to
        // say which of the two the streaming build's peak actually is.
        let (written, blob) = match (sparse, drain) {
            (false, true) => (drain_keys(iter_sorted_keys(n, &vocab)), 0),
            (true, true) => (drain_keys(iter_sparse_sorted_keys(n, &vocab).take(n)), 0),
            (false, false) => {
                let w =
                    StringIndex::build_sorted_to_file(iter_sorted_keys(n, &vocab), &path).unwrap();
                (
                    w,
                    std::fs::metadata(&path).map(|m| m.len() as usize).unwrap(),
                )
            }
            (true, false) => {
                let w = StringIndex::build_sorted_to_file(
                    iter_sparse_sorted_keys(n, &vocab).take(n),
                    &path,
                )
                .unwrap();
                (
                    w,
                    std::fs::metadata(&path).map(|m| m.len() as usize).unwrap(),
                )
            }
        };
        let build_ms = t0.elapsed().as_secs_f64() * 1e3;
        let label = match (sparse, drain) {
            (false, false) => "sorted/grid",
            (true, false) => "sorted/sparse",
            (false, true) => "generator/grid",
            (true, true) => "generator/sparse",
        };
        report(label, written, keys_rss, blob, build_ms);
        std::fs::remove_file(&path).ok();
        return;
    }

    let vocab = load_vocab();
    let keys = make_keys(n, &vocab);
    // The vocabulary is dead once the keys exist, and the reset comes after both facts so that
    // `keys_rss` is the key list at rest rather than the largest the process ever happened to be.
    drop(vocab);
    reset_peak_rss();
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
            eprintln!(
                "unknown index {other:?}; expected string, string-sorted[-sparse][-gen], \
                 perfect or compact"
            );
            std::process::exit(1);
        }
    }
}
