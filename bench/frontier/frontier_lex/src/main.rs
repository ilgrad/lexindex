//! lexindex under the C² benchmark's protocol: build from a one-key-per-line file, then look every
//! key up once in one fixed shuffled order and report `build_ms,size_mib,latency_ns` from that
//! single pass with no warm-up, which is what `benchmark.cpp` there does. Three more passes follow,
//! and their mean and minimum are printed beside the cold number. Sizes are the serialised blob.
//! `bench/frontier/run.sh` runs one index kind a process.
use std::io::BufRead;
use std::time::Instant;

use lexindex::{DictIndex, StringIndex};

/// The longest key libstdc++'s `std::string` keeps inside the object rather than behind a pointer.
const SSO: usize = 15;

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

    // The queries laid out as `benchmark.cpp`'s shuffled copy of the keys lays them out. A copied
    // `std::string` holds a key of up to 15 bytes inside itself, so the shuffle carries those bytes
    // into probe order, and allocates a longer key's buffer in sorted order, where the shuffle leaves
    // it. Borrowing every query from `keys` charged each lookup here a fetch of its key from a random
    // place in the heap, which the C++ rows pay only past 15 bytes.
    let order = shuffled(keys.len(), 2);
    let long: Vec<Option<String>> = keys
        .iter()
        .map(|key| (key.len() > SSO).then(|| key.clone()))
        .collect();
    let mut inline = vec![[0u8; SSO]; keys.len()];
    for (slot, &i) in inline.iter_mut().zip(&order) {
        let key = keys[i as usize].as_bytes();
        if key.len() <= SSO {
            slot[..key.len()].copy_from_slice(key);
        }
    }
    let queries: Vec<&str> = order
        .iter()
        .zip(&inline)
        .map(|(&i, slot)| match &long[i as usize] {
            Some(copy) => copy.as_str(),
            None => std::str::from_utf8(&slot[..keys[i as usize].len()]).expect("a key is UTF-8"),
        })
        .collect();

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
    // Four passes over keys whose ids are the ranks 0..n: any other sum is a wrong answer.
    let n = keys.len() as u64;
    assert_eq!(
        sink,
        (n * n.saturating_sub(1) / 2).wrapping_mul(4),
        "{label} answered a wrong id"
    );
    println!(
        "{label}: build {build_ms:.0} ms, size {size_mib:.3} MiB ({:.3} B/key), latency cold {cold:.1} ns, then mean {mean:.1} / min {min:.1} ns",
        index.bytes() as f64 / keys.len() as f64,
    );
    println!("{build_ms:.3},{size_mib:.6},{cold:.3}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: frontier_lex <keys.txt> <dict32|dict256|dict1024|string>...");
        std::process::exit(2);
    }
    // A line at a time, as `std::getline` reads it: the whole file held beside the keys would count
    // into this process's peak memory and no C++ row's.
    let file = std::fs::File::open(&args[1]).expect("open");
    let mut keys: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map(|line| line.expect("read"))
        .filter(|line| !line.is_empty())
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
