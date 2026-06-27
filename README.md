# betula-index

Compact, immutable **string↔id indexes for huge catalogs** — the indexing companion to
[`betula-cluster`](https://github.com/ilgrad/betula-cluster). Build once over a set of strings
(entity names, cluster labels, document keys, vocabulary terms); query many times.

Two complementary, build-once / query-many structures:

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer
  ([`fst`](https://crates.io/crates/fst)). Exact `string → id` and `id → string`, plus **prefix** and
  **range** iteration, in a compressed, serialisable (and memory-mappable-by-blob) form. Use it for
  autocomplete, browse, and ordered scans of a large catalog.
- **`PerfectHashIndex`** — a **minimal-perfect-hash** dictionary backed by
  [`ptr_hash`](https://crates.io/crates/ptr_hash) (the `mph` feature, on by default). Fastest exact
  `string → dense id` lookup with **verified membership** and reverse lookup; no ordering. Use it as a
  fixed-vocabulary token↔id map on a hot path.

Both assign dense ids in `[0, n)` and support reverse lookup. Both are immutable after building — they
are immutable summaries, like the clustering features in `betula-cluster`.

## Install

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
  silently corrupting) on the astronomically rare 64-bit hash collision between two distinct keys.
- `mph` is opt-in-by-default: with `--no-default-features` the crate depends only on `fst`. Enabling
  `mph` pulls `ptr_hash` and its dependency tree, which currently carries a few informational RustSec
  advisories (unmaintained / unsound) on transitive crates — `cargo audit` reports them as warnings,
  not vulnerabilities. The `fst`-only build is free of them.

## License

MIT © Ilia Gradina
