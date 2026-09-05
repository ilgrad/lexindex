# Usage guide

Runnable snippets for every interface, in Python and Rust.

## `StringIndex` — ordered, FST-backed

```python
import lexindex

# Duplicates are removed and keys sorted; the id of a key is its rank in sorted order.
idx = lexindex.StringIndex(["banana", "apple", "apricot", "cherry", "apple"])

# Keys already in ascending byte order build without ever being materialised: the constructor
# has to hold the whole corpus to sort it, these consume the iterable lazily. Measured at 10 M
# keys, the streamed build peaks at 11.6 MB against the list build's 1053.4 -- 91x.
keys = (line.rstrip("\n") for line in open("sorted-keys.txt"))   # any lazy iterable
n = lexindex.StringIndex.build_sorted_to_file(keys, "keys.bix")  # -> number of keys written
idx2 = lexindex.StringIndex.from_sorted(["apple", "apricot", "banana"])  # or straight to memory

len(idx)                 # 4  (duplicate "apple" deduped)
idx.id("apple")          # 0
idx.id("missing")        # None
"cherry" in idx          # True
idx.key(2)               # "banana"  (id → string)

# ordered iteration — automaton-driven: prefix/range seek directly; a broad fuzzy or
# subsequence pattern may still walk most of the FST
idx.prefix("ap")         # [("apple", 0), ("apricot", 1)]
idx.range("apricot", "cherry")   # [("apricot", 1), ("banana", 2)]  — [lo, hi)
idx.successor("ba")      # ("banana", 2)   — smallest key ≥ query
idx.predecessor("ba")    # ("apricot", 1)  — largest key ≤ query
idx.fuzzy("aple", 1)     # [("apple", 0)]  — Levenshtein edit distance ≤ 1
idx.subsequence("ae")    # [("apple", 0)]  — "a…e" in order, not necessarily contiguous
# "character" above means a Unicode scalar value: no normalisation, case folding or grapheme
# segmentation is applied — normalise before building and querying if you need it

# lazy iteration in sorted (= id) order — streams the transducer a chunk at a time, so it never
# builds a giant list and never decodes a key twice
list(idx)                # [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
dict(idx)                # {"apple": 0, "apricot": 1, "banana": 2, "cherry": 3}

# every query takes a limit: stop after that many matches, walking no further
idx.prefix("ap", limit=1)        # [("apple", 0)]
idx.fuzzy("aple", 1, limit=1)    # [("apple", 0)]

# batched lookups — one Rust↔Python crossing instead of one per key (named ids_of / keys_of so the
# class is never mistaken for a mapping)
idx.ids_of(["banana", "x"])   # [2, None]
idx.keys_of([0, 2])           # ["apple", "banana"]
```

### What `limit` buys — and what it cannot

`limit` is not a faster walk; it is **not doing work whose result would be thrown away**. Without it,
`prefix("s")` on the 479 823-word system dictionary walks the whole `s` subtree of the FST and
materialises every match — 45 064 `(str, int)` tuples — even if the caller wanted ten. With it, the
walk stops at the tenth match. The saving is therefore proportional to how much of the result you
*discard*, and it varies enormously by query (same dictionary, idle machine, min of 9 runs):

| query | matches | full | `limit` | speedup |
|---|---:|---:|---:|---:|
| `prefix("s", limit=10)` | 45 064 | 10.96 ms | 0.004 ms | ~3 000× |
| `prefix("a", limit=10)` | 25 192 | 4.83 ms | 0.003 ms | ~1 500× |
| `prefix("un", limit=10)` | 20 358 | 4.09 ms | 0.006 ms | ~700× |
| `subsequence("abc", limit=10)` | 1 910 | 41.31 ms | 0.51 ms | ~80× |
| `fuzzy("hello", 2, limit=5)` | 242 | 1.52 ms | 0.26 ms | ~6× |

Three regimes hide in that column, and they tell you when to reach for `limit`:

- **Prefix / range: the walk is cheap, the discarded results were the cost.** Speedup tracks
  `matches ÷ limit` almost linearly. This is the autocomplete case, and it is why the numbers are
  in the thousands.
- **Subsequence: the walk itself is expensive.** The `.*a.*b.*c.*` automaton visits many FST nodes
  per match produced, so stopping early saves *traversal*, not just tuples — 80× despite discarding
  far fewer results than `prefix("un")`.
- **Fuzzy: a fixed cost dominates that `limit` cannot skip.** The Levenshtein automaton is built
  eagerly before the walk starts (deliberately, so a too-large distance raises up front rather than
  on first use). `limit` only trims the walk after that, hence single-digit gains.

There is also a floor under every query: one call costs ~3 µs of FFI plus stream construction, so
per-match cost at `limit=1` reads ~2 000 ns against a steady ~200 ns from `limit≈100` up. Asking
for one match costs about the same as asking for ten — batch your UI accordingly.

If you will consume **all** matches, `limit` (or omitting it) changes nothing: the eager form *is*
the lazy walk collected, measured within noise of each other. In Rust, prefer the `*_iter` forms
(`prefix_iter` / `range_iter` / `fuzzy_iter` / `subsequence_iter`) and `.take(n)` — same machinery,
no intermediate `Vec` at all.

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

# fingerprint_bytes ∈ {1, 2, 4}; or a keyword-only fingerprint_bits ∈ 1..=64 for finer control.
# 8 bits (the default) is ~1.3 B/key with a ~0.4% membership false-positive
# rate; 2 → ~0.0015%; 4 → effectively exact. No keys are stored, so there is no id → key.
dict_ = CompactHashIndex(["GET", "POST", "PUT", "DELETE"], fingerprint_bytes=1)
i = dict_.id("POST")           # dense id in [0, n); a non-member may rarely read as present
dict_.contains("GET")          # True
dict_.id_unchecked("GET")      # fastest lookup; no fingerprint check (closed-vocabulary hot path)

dict_.save("verbs.bch")
dict_ = CompactHashIndex.load_mmap("verbs.bch")   # fingerprint table mapped zero-copy
```

### Choosing the fingerprint width

Size is the minimal perfect hash (~0.27 B/key) plus exactly `fingerprint_bits/8` bytes per key, and
the membership false-positive rate is about `2^-fingerprint_bits` — a design rate for
well-distributed keys, measured 6.2530 % at 4 bits and 1.5553 % at 6 over 2 M non-member probes;
not a defence against an adversary who chooses the queries, since both hashes are deterministic and
unseeded — every width is a point on the
same trade-off (measured on `/usr/share/dict/words`, 479 823 keys):

| `fingerprint_bits` | bytes/key | false-positive rate | false hits per 1 M non-member probes |
|---:|---:|---:|---:|
| 4 | **0.77** | 6.25% | 62 500 |
| 6 | 1.02 | 1.56% | 15 625 |
| 8 (= `fingerprint_bytes=1`, default) | 1.27 | 0.39% | 3 906 |
| 12 | 1.77 | 0.024% | 244 |
| 16 (= `fingerprint_bytes=2`) | 2.27 | 0.0015% | 15 |
| 32 (= `fingerprint_bytes=4`) | 4.27 | 2.3×10⁻⁸% | ~0 |

Pick by the probe mix, not the key count: the rate is per *non-member* lookup, so a workload that
only ever queries members never sees a false positive at any width, while a filter in front of a
network hop wants the rate priced against the cost of a wasted hop. For scale: marisa-trie's exact
index costs 2.98 B/key on this corpus — `CompactHashIndex` is below it at *every* width up to
21 bits (rate 2⁻²¹ ≈ 5×10⁻⁵%).

```python
tiny = CompactHashIndex(keys, fingerprint_bits=4)   # 0.77 B/key, 1-in-16 false positives
tiny.fingerprint_bits                               # -> 4
```

`CompactHashIndex` trades exactness for size: membership is correct except for a `2^-fingerprint_bits`
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

// `save` / `load` / `load_mmap` take anything that is `AsRef<Path>`.
let path = std::env::temp_dir().join("lexindex-usage-catalog.bix");
idx.save(&path)?;
// SAFETY: nothing may modify the file while a mapped index borrows it (see `load_mmap`).
let idx = unsafe { StringIndex::load_mmap(&path) }?; // zero-copy; no read into RAM

let dict = PerfectHashIndex::build(["GET", "POST", "PUT"])?; // requires the default `mph` feature
assert_eq!(dict.key(dict.id("POST").unwrap()), Some("POST")); // exact reverse lookup

let small = CompactHashIndex::build(["GET", "POST", "PUT"], 1)?; // ~1.3 B/key, no reverse
let tiny = CompactHashIndex::build_bits(["GET", "POST", "PUT"], 4)?; // ~0.8 B/key, 6.25% FP rate
assert!(small.contains("POST"));
# drop(idx);
# std::fs::remove_file(&path).ok();
# Ok::<(), lexindex::IndexError>(())
```

Cargo features: `mph` (default) adds `PerfectHashIndex` and `CompactHashIndex`; `mmap` (default) adds
`load_mmap`; `--no-default-features` is an `fst`-only build (`StringIndex` only, no extra dependencies)
and the only one that compiles for 32-bit targets, including `wasm32-unknown-unknown` — `mph` needs a
64-bit pointer width and refuses anything else at compile time.

## Benchmark

`python bench/compare.py` measures **serialised size** on real dictionary words against `marisa-trie`,
DAWG and datrie (the double-crown table above). `python bench/scale.py` measures **build time, peak
memory, and lookup latency from 1 M to 100 M** real keys, each cell once with the keys handed over as
a list and once as a generator — the second is what `CompactHashIndex`'s streaming build exists for,
and the only way to see its own footprint rather than the corpus's. `cargo run --release --example
bench` measures **point-lookup latency** for all three indexes against `std::HashMap` (SipHash and
FxHash) and `BTreeMap` on real dictionary-word bigrams (it refuses to run without a word list rather
than substitute synthetic keys); `cargo run --release --example peak` reports the **peak resident
memory and wall time of one build**, one index per process; `cargo run --release --example
mmap_zero_copy` times the owned `load` against the zero-copy `load_mmap`.
