# Usage guide

Runnable snippets for every interface, in Python and Rust.

## `StringIndex` — ordered, FST-backed

```python
import lexindex

# Duplicates are removed and keys sorted; the id of a key is its rank in sorted order.
idx = lexindex.StringIndex(["banana", "apple", "apricot", "cherry", "apple"])

len(idx)                 # 4  (duplicate "apple" deduped)
idx.id("apple")          # 0
idx.id("missing")        # None
"cherry" in idx          # True
idx.key(2)               # "banana"  (id → string)

# ordered iteration — automaton-driven, never a full scan
idx.prefix("ap")         # [("apple", 0), ("apricot", 1)]
idx.range("apricot", "cherry")   # [("apricot", 1), ("banana", 2)]  — [lo, hi)
idx.fuzzy("aple", 1)     # [("apple", 0)]  — Levenshtein edit distance ≤ 1
idx.subsequence("ae")    # [("apple", 0)]  — "a…e" in order, not necessarily contiguous
```

### Persistence and zero-copy loading

```python
idx.save("catalog.bix")                            # write a flat, relocatable blob
idx = lexindex.StringIndex.load("catalog.bix")     # read it back into RAM
idx = lexindex.StringIndex.load_mmap("catalog.bix") # …or memory-map it: no read, borrowed zero-copy

data = idx.to_bytes()                              # or go through bytes directly
idx = lexindex.StringIndex.from_bytes(data)
```

`load_mmap` maps the file and borrows the index from the mapped pages, so load time is independent of
the index size and the pages are shared across processes. The mapped file must stay immutable while an
index borrows it. Loading an untrusted / truncated blob fails cleanly (`ValueError`), never corrupts.

## `PerfectHashIndex` — fastest exact lookup

```python
from lexindex import PerfectHashIndex

dict_ = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
i = dict_.id("POST")           # dense id in [0, n); membership is verified against the stored key
dict_.key(i)                   # "POST"
dict_.id("PATCH")              # None  — a verified miss, not a hash collision
dict_.id_unchecked("GET")      # fastest lookup; skips verification (closed-vocabulary hot path)

dict_.save("verbs.bmp")
dict_ = PerfectHashIndex.load_mmap("verbs.bmp")   # arena mapped zero-copy; tiny MPH read into RAM
```

Use `id_unchecked` only for a **fixed / closed vocabulary** where membership is already guaranteed —
it returns an arbitrary (but valid) slot for an unknown key. Use `id` everywhere else.

## Rust

```rust
use lexindex::{PerfectHashIndex, StringIndex};

let idx = StringIndex::build(["apple", "apricot", "banana"])?;
assert_eq!(idx.id("banana"), Some(2));
assert_eq!(idx.key(0).as_deref(), Some("apple")); // reconstructed → owned String
assert_eq!(idx.prefix("ap").len(), 2);
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);

idx.save("catalog.bix")?;
let idx = StringIndex::load_mmap("catalog.bix")?; // zero-copy; no read into RAM

let dict = PerfectHashIndex::build(["GET", "POST", "PUT"])?; // requires the default `mph` feature
assert_eq!(dict.key(dict.id("POST").unwrap()), Some("POST"));
# std::fs::remove_file("catalog.bix").ok();
# Ok::<(), lexindex::IndexError>(())
```

Cargo features: `mph` (default) adds `PerfectHashIndex`; `mmap` (default) adds `load_mmap`;
`--no-default-features` is an `fst`-only build (`StringIndex` only, no extra dependencies).

## Benchmark

`cargo run --release --example bench` compares both indexes against `std::HashMap` / `BTreeMap`
(build time, lookup latency, serialised size). `cargo run --release --example mmap_zero_copy` times
the owned `load` against the zero-copy `load_mmap`.
