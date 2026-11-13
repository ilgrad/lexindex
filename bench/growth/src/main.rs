//! Instructions a key of every build and load, by n: the harness behind `bench/growth/README.md`.
//!
//! `growth <op> <keys> <n> <dir>`. Run under `perf stat -D -1 --control fd:10,11` (`count.sh`),
//! with fds 10 and 11 the control and ack FIFOs: the counters start disabled, and this program
//! enables them around the one operation it measures, so drawing and reading the keys is not
//! counted. Keys: `n` distinct lines drawn from the file by a fixed-seed partial Fisher-Yates, then
//! sorted (a sorted corpus's prefix would change the key distribution with `n`); `n = 0` reads a
//! file `draw` wrote. Pinned to one CPU, every build runs on one thread and its count is
//! deterministic.
use std::hint::black_box;
use std::io::{Read, Write};
use std::path::PathBuf;

fn counters(on: bool) {
    let Ok(mut ctl) = std::fs::OpenOptions::new()
        .write(true)
        .open("/proc/self/fd/10")
    else {
        return;
    };
    ctl.write_all(if on { b"enable\n" } else { b"disable\n" })
        .unwrap();
    let mut ack = std::fs::File::open("/proc/self/fd/11").unwrap();
    let mut b = [0u8; 5];
    ack.read_exact(&mut b).unwrap();
}

fn draw(path: &str, n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    // n = 0: the file is a draw already written by `draw`.
    if n == 0 {
        return text.lines().map(str::to_owned).collect();
    }
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines.dedup();
    assert!(
        n <= lines.len(),
        "{path} has {} distinct lines",
        lines.len()
    );
    let mut x: u64 = 0x5CA1E;
    for i in 0..n {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let j = i + (z % (lines.len() - i) as u64) as usize;
        lines.swap(i, j);
    }
    let mut keys: Vec<String> = lines[..n].iter().map(|s| (*s).to_owned()).collect();
    keys.sort_unstable();
    keys
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (op, corpus, n, dir) = (a[1].as_str(), a[2].as_str(), a[3].parse().unwrap(), &a[4]);
    let file = |kind: &str| PathBuf::from(dir).join(format!("{kind}.blob"));
    // `save-<kind>` writes the blob a `load-<kind>` maps; neither counts the draw.
    if let Some(kind) = op.strip_prefix("load-") {
        let p = file(kind);
        counters(true);
        // SAFETY: the blob is this run's own file, and nothing writes it while it is mapped.
        let len = unsafe {
            match kind {
                "string" => lexindex::StringIndex::load_mmap(&p).unwrap().len(),
                "dict32" | "dict256" | "dict1024" => {
                    lexindex::DictIndex::load_mmap(&p).unwrap().len()
                }
                "da" => lexindex::DoubleArrayIndex::load_mmap(&p).unwrap().len(),
                "ph" | "phblocked" => lexindex::PerfectHashIndex::load_mmap(&p).unwrap().len(),
                "compact" => lexindex::CompactHashIndex::load_mmap(&p).unwrap().len(),
                "closed" => lexindex::ClosedHashIndex::load(&p).unwrap().len(),
                "hashed" => lexindex::HashedDictIndex::load_mmap(&p).unwrap().len(),
                _ => panic!("unknown kind {kind}"),
            }
        };
        counters(false);
        println!("{len}");
        return;
    }
    let keys = draw(corpus, n);
    if op == "draw" {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&a[4]).unwrap());
        for k in &keys {
            writeln!(w, "{k}").unwrap();
        }
        return;
    }
    let save = op.strip_prefix("save-");
    let kind = save.unwrap_or(op);
    // Inputs a phase takes as given are made before the counters start.
    let dict = (kind == "hashed").then(|| lexindex::DictIndex::build(&keys).unwrap());
    let overlay = (kind == "overlay").then(|| {
        let base: Vec<&String> = keys.iter().step_by(2).collect();
        lexindex::Overlay::new(lexindex::PerfectHashIndex::build(base).unwrap())
    });
    let blob = file(kind);
    counters(save.is_none());
    let bytes = match kind {
        "string" => lexindex::StringIndex::build_sorted(&keys)
            .unwrap()
            .to_bytes(),
        "dict32" => lexindex::DictIndex::build_with_block(&keys, 32)
            .unwrap()
            .to_bytes(),
        "dict256" => lexindex::DictIndex::build_with_block(&keys, 256)
            .unwrap()
            .to_bytes(),
        "dict1024" => lexindex::DictIndex::build_with_block(&keys, 1024)
            .unwrap()
            .to_bytes(),
        "da" => lexindex::DoubleArrayIndex::build(&keys).unwrap().to_bytes(),
        "ph" => lexindex::PerfectHashIndex::build(&keys)
            .unwrap()
            .to_bytes()
            .unwrap(),
        "phblocked" => {
            lexindex::PerfectHashIndex::build_to_file(&blob, || keys.iter()).unwrap();
            Vec::new()
        }
        "compact" => lexindex::CompactHashIndex::build_bits(&keys, 8)
            .unwrap()
            .to_bytes()
            .unwrap(),
        "compactfile" => {
            lexindex::CompactHashIndex::build_bits_to_file(&keys, &blob, 8).unwrap();
            Vec::new()
        }
        "closed" => lexindex::ClosedHashIndex::build(&keys).unwrap().to_bytes(),
        "hashed" => lexindex::HashedDictIndex::from_dict(dict.unwrap(), 8)
            .unwrap()
            .to_bytes(),
        "overlay" => {
            let mut o = overlay.unwrap();
            for k in keys.iter().skip(1).step_by(2) {
                o.add(k);
            }
            black_box(o.compact().unwrap().len());
            Vec::new()
        }
        "plan" => {
            let p = lexindex::plan(&keys, lexindex::Needs::default()).unwrap();
            black_box(&p);
            Vec::new()
        }
        _ => panic!("unknown op {op}"),
    };
    counters(false);
    if save.is_some() && !bytes.is_empty() {
        std::fs::write(&blob, &bytes).unwrap();
    }
    println!("{}", bytes.len());
}
