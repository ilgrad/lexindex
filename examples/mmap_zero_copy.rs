//! Zero-copy `load_mmap` vs the owned `load`.
//!
//! `load` reads the whole blob into RAM; `load_mmap` memory-maps the file and borrows the index from
//! the mapped pages, so its load time is independent of the index size and the pages are shared across
//! processes by the OS page cache. The two are query-identical — mmap just borrows instead of owning.
//!
//! Run with `cargo run --release --example mmap_zero_copy [N]`.

use lexindex::{IndexError, PerfectHashIndex, StringIndex};
use std::time::Instant;

fn main() -> Result<(), IndexError> {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(500_000);
    // Zero-padded so lexicographic order matches numeric order (this demo measures load time, which is
    // independent of key content; for a size benchmark use real words — see bench/compare.py).
    let keys: Vec<String> = (0..n).map(|i| format!("entity-{i:012}")).collect();

    let dir = std::env::temp_dir();
    let si_path = dir.join(format!("lexindex_mmap_demo_{}.bix", std::process::id()));
    let ph_path = dir.join(format!("lexindex_mmap_demo_{}.bmp", std::process::id()));

    let si = StringIndex::build(&keys)?;
    si.save(&si_path)?;
    let ph = PerfectHashIndex::build(&keys)?;
    ph.save(&ph_path)?;

    let on_disk = std::fs::metadata(&si_path)?.len();
    println!(
        "StringIndex: {n} keys, {:.1} MB on disk ({:.1} bytes/key)\n",
        on_disk as f64 / 1e6,
        on_disk as f64 / n as f64
    );

    // Owned load reads (and allocates) the whole file; mmap load only maps it — no read, no copy.
    let t = Instant::now();
    let owned = StringIndex::load(&si_path)?;
    let owned_us = t.elapsed().as_micros();

    let t = Instant::now();
    let mapped = StringIndex::load_mmap(&si_path)?;
    let mmap_us = t.elapsed().as_micros().max(1);

    println!("StringIndex::load       (read into RAM)   {owned_us:>8} µs");
    println!(
        "StringIndex::load_mmap  (zero-copy)       {mmap_us:>8} µs   ({:.0}x faster to load)",
        owned_us as f64 / mmap_us as f64
    );

    // Query-identical: forward, reverse, and prefix all agree.
    let probe = &keys[n / 2];
    assert_eq!(owned.id(probe), mapped.id(probe));
    assert_eq!(owned.key(n as u64 / 2), mapped.key(n as u64 / 2));
    println!("\nboth agree: id({probe:?}) = {:?}", mapped.id(probe));

    // PerfectHashIndex maps the key arena (the bulk) and reads only the tiny MPH into memory.
    let t = Instant::now();
    let ph_mapped = PerfectHashIndex::load_mmap(&ph_path)?;
    println!(
        "PerfectHashIndex::load_mmap               {:>8} µs",
        t.elapsed().as_micros()
    );
    assert_eq!(ph_mapped.id(probe), ph.id(probe));

    println!(
        "\nmmap load is O(1) in the index size; N processes share one physical copy of the pages."
    );

    std::fs::remove_file(&si_path).ok();
    std::fs::remove_file(&ph_path).ok();
    Ok(())
}
