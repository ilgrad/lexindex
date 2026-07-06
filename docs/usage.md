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
idx.successor("ba")      # ("banana", 2)   — smallest key ≥ query
idx.predecessor("ba")    # ("apricot", 1)  — largest key ≤ query
idx.fuzzy("aple", 1)     # [("apple", 0)]  — Levenshtein edit distance ≤ 1
idx.subsequence("ae")    # [("apple", 0)]  — "a…e" in order, not necessarily contiguous

# lazy iteration in sorted (= id) order — decodes one key at a time, never builds a giant list
list(idx)                # [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
dict(idx)                # {"apple": 0, "apricot": 1, "banana": 2, "cherry": 3}

# batched lookups — one Rust↔Python crossing instead of one per key (named ids_of / keys_of so the
# class is never mistaken for a mapping)
idx.ids_of(["banana", "x"])   # [2, None]
idx.keys_of([0, 2])           # ["apple", "banana"]
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

## `CompactHashIndex` — smallest footprint

```python
from lexindex import CompactHashIndex

# fingerprint_bytes ∈ {1, 2, 4}: 1 is smallest (~1.3 B/key) with a ~0.4% membership false-positive
# rate; 2 → ~0.0015%; 4 → effectively exact. No keys are stored, so there is no id → key.
dict_ = CompactHashIndex(["GET", "POST", "PUT", "DELETE"], fingerprint_bytes=1)
i = dict_.id("POST")           # dense id in [0, n); a non-member may rarely read as present
dict_.contains("GET")          # True
dict_.id_unchecked("GET")      # fastest lookup; no fingerprint check (closed-vocabulary hot path)

dict_.save("verbs.bch")
dict_ = CompactHashIndex.load_mmap("verbs.bch")   # fingerprint table mapped zero-copy
```

`CompactHashIndex` trades exactness for size: membership is correct except for a `256^-fingerprint_bytes`
false-positive chance on a non-member, and it cannot map an id back to a string. Reach for it when a
fixed vocabulary's on-disk / mmap footprint dominates; use `PerfectHashIndex` when you need exact
membership or `id → key`, or `StringIndex` when you need order or fuzzy/prefix.

## Rust

```rust
use lexindex::{CompactHashIndex, PerfectHashIndex, StringIndex};

let idx = StringIndex::build(["apple", "apricot", "banana"])?;
assert_eq!(idx.id("banana"), Some(2));
assert_eq!(idx.key(0).as_deref(), Some("apple")); // rank-walk over the FST → owned String
assert_eq!(idx.prefix("ap").len(), 2);
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);

idx.save("catalog.bix")?;
let idx = StringIndex::load_mmap("catalog.bix")?; // zero-copy; no read into RAM

let dict = PerfectHashIndex::build(["GET", "POST", "PUT"])?; // requires the default `mph` feature
assert_eq!(dict.key(dict.id("POST").unwrap()), Some("POST")); // exact reverse lookup

let small = CompactHashIndex::build(["GET", "POST", "PUT"], 1)?; // smallest; ~1.3 B/key, no reverse
assert!(small.contains("POST"));
# std::fs::remove_file("catalog.bix").ok();
# Ok::<(), lexindex::IndexError>(())
```

Cargo features: `mph` (default) adds `PerfectHashIndex` and `CompactHashIndex`; `mmap` (default) adds
`load_mmap`; `--no-default-features` is an `fst`-only build (`StringIndex` only, no extra dependencies).

## Benchmark

`python bench/compare.py` measures **serialised size** on real dictionary words against `marisa-trie`,
DAWG and datrie (the double-crown table above). `python bench/scale.py` measures **build time, peak
memory, and lookup latency from 1 M to 100 M** real keys. `cargo run --release --example bench` measures
**point-lookup latency** for `StringIndex` and `PerfectHashIndex` against `std::HashMap` / `BTreeMap`;
`cargo run --release --example mmap_zero_copy` times the owned `load` against the zero-copy `load_mmap`.
