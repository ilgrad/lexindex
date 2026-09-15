//! lexindex under the C² benchmark's protocol: build from a one-key-per-line file, then look every
//! key up once in one fixed shuffled order and report `build_ms,size_mib,latency_ns` from that
//! single pass with no warm-up, which is what `benchmark.cpp` there does. Three more passes follow,
//! and their mean and minimum are printed beside the cold number. Sizes are the serialised blob.
//! `bench/frontier/run.sh` runs one index kind a process.
use std::io::Read;
use std::time::Instant;

use lexindex::{DictIndex, StringIndex};

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn shuffled(n: usize, seed: u64) -> Vec<u32> {
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut s = seed | 1;
    for i in (1..n).rev() {
        let j = (xorshift(&mut s) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    order
}

trait Probe {
    fn build(keys: &[String], arg: usize) -> Self;
    fn bytes(&self) -> usize;
    fn probe(&self, key: &str) -> u64;
}

impl Probe for DictIndex {
    fn build(keys: &[String], block: usize) -> Self {
        DictIndex::build_with_block(keys, block).expect("dict build")
    }
    fn bytes(&self) -> usize {
        self.serialized_len()
    }
    fn probe(&self, key: &str) -> u64 {
        self.id(key).unwrap_or(u64::MAX)
    }
}

impl Probe for StringIndex {
    fn build(keys: &[String], _: usize) -> Self {
        StringIndex::build(keys).expect("string build")
    }
    fn bytes(&self) -> usize {
        self.serialized_len()
    }
    fn probe(&self, key: &str) -> u64 {
        self.id(key).unwrap_or(u64::MAX)
    }
}

fn run<T: Probe>(keys: &[String], arg: usize, label: &str) {
    let t0 = Instant::now();
    let index = T::build(keys, arg);
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let size_mib = index.bytes() as f64 / (1024.0 * 1024.0);

    let order = shuffled(keys.len(), 2);
    let queries: Vec<&str> = order.iter().map(|&i| keys[i as usize].as_str()).collect();

    let mut sink = 0u64;
    let t = Instant::now();
    for q in &queries {
        sink = sink.wrapping_add(index.probe(q));
    }
    let cold = t.elapsed().as_nanos() as f64 / queries.len() as f64;
    let mut passes = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        for q in &queries {
            sink = sink.wrapping_add(index.probe(q));
        }
        passes.push(t.elapsed().as_nanos() as f64 / queries.len() as f64);
    }
    let mean = passes.iter().sum::<f64>() / passes.len() as f64;
    let min = passes.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "{label}: build {build_ms:.0} ms, size {size_mib:.3} MiB ({:.3} B/key), latency cold {cold:.1} ns, then mean {mean:.1} / min {min:.1} ns (sink {})",
        index.bytes() as f64 / keys.len() as f64,
        sink & 1
    );
    println!("{build_ms:.3},{size_mib:.6},{cold:.3}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: frontier_lex <keys.txt> <dict32|dict256|dict1024|string>...");
        std::process::exit(2);
    }
    let mut text = String::new();
    std::fs::File::open(&args[1])
        .expect("open")
        .read_to_string(&mut text)
        .expect("read");
    let mut keys: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    eprintln!("{} keys", keys.len());
    for kind in &args[2..] {
        match kind.as_str() {
            "dict32" => run::<DictIndex>(&keys, 32, "lexindex DictIndex block 32"),
            "dict256" => run::<DictIndex>(&keys, 256, "lexindex DictIndex block 256"),
            "dict1024" => run::<DictIndex>(&keys, 1024, "lexindex DictIndex block 1024"),
            "string" => run::<StringIndex>(&keys, 0, "lexindex StringIndex"),
            other => {
                eprintln!("unknown kind {other}");
                std::process::exit(2);
            }
        }
    }
}
