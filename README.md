# betula-index

[![PyPI](https://img.shields.io/pypi/v/betula-index)](https://pypi.org/project/betula-index/)
[![Python](https://img.shields.io/pypi/pyversions/betula-index)](https://pypi.org/project/betula-index/)
[![CI](https://github.com/ilgrad/betula-index/actions/workflows/ci.yml/badge.svg)](https://github.com/ilgrad/betula-index/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/ilgrad/betula-index/blob/main/LICENSE)
[![Rust core · PyO3](https://img.shields.io/badge/Rust%20core-PyO3-orange.svg)](https://github.com/ilgrad/betula-index)

Compact, immutable **string↔id indexes for huge catalogs** — the indexing companion to
[`betula-cluster`](https://github.com/ilgrad/betula-cluster). Build once over a set of strings
(entity names, cluster labels, document keys, vocabulary terms); query many times.

Two complementary, build-once / query-many structures:

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer
  ([`fst`](https://crates.io/crates/fst)). Exact `string → id` and `id → string`, plus **prefix**,
  **range**, **fuzzy** (bounded Levenshtein edit distance), and **subsequence** iteration — all driven
  by automata over the FST, never a full scan — in a compressed, serialisable (and
  memory-mappable-by-blob) form. Use it for autocomplete, typo-tolerant search, browse, and ordered
  scans of a large catalog.
- **`PerfectHashIndex`** — a **minimal-perfect-hash** dictionary backed by
  [`ptr_hash`](https://crates.io/crates/ptr_hash) (the `mph` feature, on by default). Exact
  `string → dense id` with **verified membership** (`id`) and reverse lookup; no ordering. For a
  known-closed vocabulary, `id_unchecked` skips the membership comparison and is **faster than
  `std::HashMap`** (see [Benchmarks](#benchmarks)). Use it as a fixed-vocabulary token↔id map on a hot path.

Both assign dense ids in `[0, n)`, support reverse lookup, and **serialise to a flat blob**
(`save`/`load`) — build once, persist, then `load`/mmap and query many times. Both are immutable after
building, like the clustering features in `betula-cluster`.

## Python

```bash
pip install betula-index
```

```python
from betula_index import PerfectHashIndex, StringIndex

idx = StringIndex(["apple", "apricot", "banana", "cherry"])
idx.id("banana")             # 2  (sorted rank)
idx.key(0)                   # "apple"
idx.prefix("ap")             # [("apple", 0), ("apricot", 1)]
idx.fuzzy("aple", 1)         # [("apple", 0)]  — typo-tolerant
idx.save("catalog.bix")      # persist; StringIndex.load("catalog.bix") reloads it

d = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
d.id("POST")                 # dense id in [0, n); membership verified, returns None if absent
d.id_unchecked("POST")       # fastest lookup for a known-closed vocabulary
```

No runtime dependencies; a single abi3 wheel covers CPython 3.11+.

## Rust

```toml
[dependencies]
betula-index = { git = "https://github.com/ilgrad/betula-index" }
# fst-only (drop the ptr_hash dependency):
# betula-index = { git = "...", default-features = false }
```

## Usage

```rust
use betula_index::StringIndex;

let idx = StringIndex::build(["apple", "apricot", "banana", "cherry"])?;

assert_eq!(idx.id("banana"), Some(2));     // string → id (sorted rank)
assert_eq!(idx.key(0), Some("apple"));     // id → string
assert!(idx.contains("cherry"));

// prefix / range iteration, lexicographically ordered
let fruit: Vec<_> = idx.prefix("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(fruit, ["apple", "apricot"]);

// typo-tolerant fuzzy lookup (Levenshtein edit distance ≤ 1) and subsequence match
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);
let sub: Vec<_> = idx.subsequence("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(sub, ["apple", "apricot"]);

// serialise to a flat blob and reload (e.g. mmap the file, then `from_bytes`)
idx.save("catalog.bix")?;
let idx = StringIndex::load("catalog.bix")?;
# Ok::<(), betula_index::IndexError>(())
```

```rust
use betula_index::PerfectHashIndex;            // requires the default `mph` feature

let dict = PerfectHashIndex::build(["GET", "POST", "PUT", "DELETE"])?;
let id = dict.id("POST").unwrap();             // fastest exact lookup, dense id in [0, n)
assert_eq!(dict.key(id), Some("POST"));
assert_eq!(dict.id("PATCH"), None);            // membership is verified, not just hashed

// persist the MPH and reload it (the dense ids are preserved across save/load)
dict.save("verbs.bmp")?;
let dict = PerfectHashIndex::load("verbs.bmp")?;
assert_eq!(dict.id("POST"), Some(id));
# Ok::<(), betula_index::IndexError>(())
```

## Design notes

- **`StringIndex`** keeps the FST (`key → id`, prefix/range) plus a string arena (`id → key`: one
  contiguous byte buffer + offsets, no per-`String` overhead). Ids are the sorted rank of each key, so
  they are stable for the same key set. The serialised blob is `[magic][fst][arena]`; `from_bytes`
  validates every length and offset, so loading an untrusted blob can fail but never corrupts.
- **`PerfectHashIndex`** keys the MPH on a deterministic 64-bit hash of each string (so queries take
  `&str` without allocating), then verifies the hit against the stored key — an MPH returns a slot for
  *any* input, so verification is what turns it into a real membership test. Build fails (rather than
  silently corrupting) on the astronomically rare 64-bit hash collision between two distinct keys. The
  hash is **version-stable** (FNV-1a + a splitmix64 finalizer, not `std`'s `DefaultHasher`), so a
  `save`d MPH (the `ptr_hash` structure serialised via [`epserde`](https://crates.io/crates/epserde),
  alongside the arena) reloads and queries identically on any build — the precondition for persistence.
- `mph` is opt-in-by-default: with `--no-default-features` the crate depends only on `fst`. Enabling
  `mph` pulls `ptr_hash` and its dependency tree, which currently carries a few informational RustSec
  advisories (unmaintained / unsound) on transitive crates — `cargo audit` reports them as warnings,
  not vulnerabilities. The `fst`-only build is free of them.

## Benchmarks

`cargo run --release --example bench` (1 M keys, 19 bytes each). Absolute numbers are
machine-dependent; the **ratios** and the trade-off are the point.

| structure | build | lookup | serialised |
|---|---|---|---|
| betula `PerfectHashIndex::id_unchecked` | ~310 ms | **~232 ns** | 27 B/key |
| `std::HashMap<String, u32>` | ~205 ms | ~290 ns | — (in-RAM, not serialisable) |
| betula `PerfectHashIndex::id` (verified) | ~376 ms | ~377 ns | 27 B/key |
| betula `StringIndex` (FST) | ~138 ms | ~386 ns | 27 B/key |
| `std::BTreeMap<String, u32>` | ~39 ms | ~833 ns | — (in-RAM) |

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest** — ≈1.25× quicker than `HashMap` (no probing, no membership comparison) *and* compact +
serialisable. Add membership verification (`id`) and you pay one extra cache line + a key comparison;
use `StringIndex` and you trade more latency for **ordered / prefix / range / fuzzy** queries the hash
maps cannot answer at all. So: `id_unchecked` for a known-closed token→id map on a hot path;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map with
membership and nothing persisted.

## License

MIT © Ilia Gradina
