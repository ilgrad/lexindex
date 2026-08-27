# lexindex

[![PyPI](https://img.shields.io/pypi/v/lexindex)](https://pypi.org/project/lexindex/)
[![Python](https://img.shields.io/pypi/pyversions/lexindex)](https://pypi.org/project/lexindex/)
[![CI](https://github.com/ilgrad/lexindex/actions/workflows/ci.yml/badge.svg)](https://github.com/ilgrad/lexindex/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-mkdocs-blue.svg)](https://ilgrad.github.io/lexindex/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/ilgrad/lexindex/blob/main/LICENSE)
[![Rust core · PyO3](https://img.shields.io/badge/Rust%20core-PyO3-orange.svg)](https://github.com/ilgrad/lexindex)
[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.22119002.svg)](https://doi.org/10.5281/zenodo.22119002)

Compact, immutable **string↔id indexes for huge catalogs**, with a Rust core and Python bindings.
Build once over a set of strings (entity names, document keys, vocabulary terms, cluster labels);
query many times. Pairs naturally with [`betula-cluster`](https://github.com/ilgrad/betula-cluster) —
map string ids to cluster ids and back — but stands on its own.

Three complementary, build-once / query-many structures — pick by what you need to ask:

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer
  ([`fst`](https://crates.io/crates/fst)). Exact `string → id` and `id → string`, plus **prefix**,
  **range**, **predecessor / successor** (nearest key ≤ / ≥ a query), **fuzzy** (bounded Levenshtein
  edit distance), **subsequence**, and **lazy full iteration** — all driven by automata over the FST,
  never a full scan — in a compressed, serialisable, memory-mappable form. The only structure here that
  answers **ordered and typo-tolerant** queries. Use it for autocomplete, fuzzy search, browse, and
  ordered scans of a large catalog.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map: a minimal perfect hash
  ([`ptr_hash`](https://crates.io/crates/ptr_hash)) plus a small fingerprint per key, storing *no keys
  at all*. **1.27 bytes/key** on real dictionary words — **2.3× smaller than `marisa-trie`**, down to
  **0.77 bytes/key** at a 4-bit fingerprint (`fingerprint_bits=4`, 6.25% false-positive rate) — below
  every trie benchmarked (see [Benchmarks](#benchmarks)) — at the cost of **probabilistic membership**
  (a tunable `2^-bits` false-positive rate) and **no reverse lookup**. Use it when a fixed vocabulary's
  footprint is paramount and rare false positives are acceptable.
- **`PerfectHashIndex`** — a minimal-perfect-hash dictionary with **verified membership** (`id`) and
  **reverse lookup** (`key`); the arena stores full keys, so it is exact but larger. For a known-closed
  vocabulary, `id_unchecked` skips the membership comparison and is **faster than `std::HashMap`**. Use
  it as a fixed-vocabulary token↔id map on a hot path when you need exact membership and `id → key`.

All three assign dense ids in `[0, n)` and **serialise to a flat blob** (`save` / `load`, or zero-copy
`load_mmap`) — build once, persist, then reload and query many times. All are immutable after building.
The `mph` feature (on by default) provides the two hash indexes; `--no-default-features` is `fst`-only.

## Python

```bash
pip install lexindex
```

```python
from lexindex import CompactHashIndex, PerfectHashIndex, StringIndex

idx = StringIndex(["apple", "apricot", "banana", "cherry"])
idx.id("banana")             # 2  (sorted rank)
idx.key(0)                   # "apple"  — reconstructed from the FST, no stored reverse map
idx.prefix("ap")             # [("apple", 0), ("apricot", 1)]
idx.fuzzy("aple", 1)         # [("apple", 0)]  — typo-tolerant
idx.successor("ba")          # ("banana", 2)   — nearest key >= query
idx.predecessor("ba")        # ("apricot", 1)  — nearest key <= query
list(idx)                    # [("apple", 0), ...]  — lazy iteration in sorted order
idx.ids_of(["apple", "x"])   # [0, None]  — batched: one FFI call, not one per key
idx.save("catalog.bix")      # persist; StringIndex.load("catalog.bix") reloads it

c = CompactHashIndex(["GET", "POST", "PUT", "DELETE"])  # smallest string->id (~1.3 B/key at scale;
#   fingerprint_bits=4 halves that to ~0.8 at a 6.25% false-positive rate)
c.id("POST")                 # dense id in [0, n); probabilistic membership, no id->key
c.id_unchecked("POST")       # fastest lookup for a known-closed vocabulary

d = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
d.id("POST")                 # dense id in [0, n); membership verified, returns None if absent
d.key(d.id("POST"))          # "POST"  — exact reverse lookup (keys stored)
```

No runtime dependencies; a single abi3 wheel covers CPython 3.11+. See
[`examples/quickstart.py`](https://github.com/ilgrad/lexindex/blob/main/examples/quickstart.py) for all
three indexes end to end, and the [documentation site](https://ilgrad.github.io/lexindex/).

### Pairs with betula-cluster

`lexindex` owns the `string id ↔ dense id` mapping; [`betula-cluster`](https://github.com/ilgrad/betula-cluster)
clusters the numeric rows. Use the lexindex dense id as the embedding-matrix row index and you can go
both ways — `string id → cluster` and `cluster → string ids`:

```python
idx = PerfectHashIndex(doc_ids)                  # string id <-> dense [0, n) id
matrix[idx.id(doc_id)] = embedding[doc_id]       # row index == lexindex id
labels = betula_cluster.fit_predict(matrix, n_clusters=k)
cluster = labels[idx.id("doc-00042")]            # string id -> cluster
members = [idx.key(int(r)) for r in (labels == cluster).nonzero()[0]]  # cluster -> string ids
```

Runnable: [`examples/bridge_clustering.py`](https://github.com/ilgrad/lexindex/blob/main/examples/bridge_clustering.py).

## Rust

```toml
[dependencies]
lexindex = { git = "https://github.com/ilgrad/lexindex" }
# fst-only (drop the ptr_hash dependency):
# lexindex = { git = "...", default-features = false }
```

## Usage

```rust
use lexindex::StringIndex;

let idx = StringIndex::build(["apple", "apricot", "banana", "cherry"])?;

assert_eq!(idx.id("banana"), Some(2));     // string → id (sorted rank)
assert_eq!(idx.key(0).as_deref(), Some("apple")); // id → string
assert!(idx.contains("cherry"));

// prefix / range iteration, lexicographically ordered
let fruit: Vec<_> = idx.prefix("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(fruit, ["apple", "apricot"]);

// typo-tolerant fuzzy lookup (Levenshtein edit distance ≤ 1) and subsequence match
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);
let sub: Vec<_> = idx.subsequence("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(sub, ["apple", "apricot"]);

// serialise to a flat blob, then reload — or `load_mmap` to borrow it zero-copy from the file
idx.save("catalog.bix")?;
let idx = StringIndex::load_mmap("catalog.bix")?; // no read into RAM; pages shared across processes
# Ok::<(), lexindex::IndexError>(())
```

```rust
use lexindex::PerfectHashIndex;            // requires the default `mph` feature

let dict = PerfectHashIndex::build(["GET", "POST", "PUT", "DELETE"])?;
let id = dict.id("POST").unwrap();             // fastest exact lookup, dense id in [0, n)
assert_eq!(dict.key(id), Some("POST"));
assert_eq!(dict.id("PATCH"), None);            // membership is verified, not just hashed

// persist the MPH and reload it (the dense ids are preserved across save/load)
dict.save("verbs.bmp")?;
let dict = PerfectHashIndex::load("verbs.bmp")?;
assert_eq!(dict.id("POST"), Some(id));
# Ok::<(), lexindex::IndexError>(())
```

```rust
use lexindex::CompactHashIndex;           // requires the default `mph` feature

// The smallest string->id map: an 8-bit fingerprint/key ⇒ ~1.3 B/key, ~0.4% membership
// false-positive (build_bits(keys, 4) ⇒ ~0.8 B/key at 6.25%).
let dict = CompactHashIndex::build(["GET", "POST", "PUT", "DELETE"], 1)?;
let id = dict.id("POST").unwrap();             // Some(slot); a non-member may rarely read as present
assert!(dict.contains("GET"));
let raw = dict.id_unchecked("POST");           // no fingerprint check — for a known-closed vocabulary
assert_eq!(raw, id);
// no key(id): CompactHashIndex stores no keys. Use PerfectHashIndex when you need id → string.
# Ok::<(), lexindex::IndexError>(())
```

## Design notes

- **`StringIndex` is the FST alone — `id → key` is reconstructed by a rank-walk, with no stored reverse
  map.** Ids are the sorted rank of each key, which is exactly the FST's output value, so `key(id)`
  walks the automaton from the root, at each node taking the last transition whose accumulated output
  stays `≤ id`, and returns the path once the outputs sum to exactly `id`. That is `O(key length)` and
  needs no auxiliary structure, so the serialised blob is just `[magic "BIX4"][fst]` — half the size of
  the 0.2.0 front-coded layout on real words (12.6 → 5.95 B/key) and simpler to reason about.
  `from_bytes` validates the magic and hands the rest to `fst`, which is itself bounds-checked, so
  loading an untrusted blob can fail but never corrupts.
- **`CompactHashIndex` stores no keys — only a minimal perfect hash and one small fingerprint per
  slot.** `id(key)` hashes the key to a slot (the MPH), then compares the key's *independent*
  `b`-bit fingerprint against the stored one; a match is a hit. Because the two hashes are independent,
  a non-member survives both only with probability `2^-b`, the tunable false-positive rate
  (`fingerprint_bits` ∈ 1..=64, bit-packed). Dropping the key arena is what takes it below
  `marisa-trie`; the price is that membership is probabilistic and there is no `id → key`. The blob
  is `[magic "BCH2"][n][fp_bits][mph][bit-packed fingerprints]`; 0.5.x `BCH1` blobs still load,
  zero-copy included.
- **`PerfectHashIndex`** keys the MPH on a deterministic 64-bit hash of each string (so queries take
  `&str` without allocating), then verifies the hit against the stored key — an MPH returns a slot for
  *any* input, so verification is what turns it into a real membership test, and the stored keys give
  exact `id → key`. Build fails (rather than silently corrupting) if two distinct keys collide in
  the 64-bit hash — `n(n-1)/2^65` ≈ 2.7×10⁻⁸ at 1 M keys, 2.7×10⁻⁴ at 100 M; the hash is unseeded,
  so a colliding set needs `StringIndex`, not a retry. The hash is **version-stable** (FNV-1a + a splitmix64
  finalizer, not `std`'s `DefaultHasher`), so a `save`d MPH (the `ptr_hash` structure serialised via
  [`epserde`](https://crates.io/crates/epserde), alongside the arena) reloads and queries identically
  on any build — the precondition for persistence. `CompactHashIndex` shares the same version-stable
  slot hash plus a second independent one for the fingerprint.
- **Zero-copy `load_mmap`** (the default `mmap` feature, `memmap2`) memory-maps a saved blob and
  borrows the index directly from the mapped pages — no read into RAM, so a multi-gigabyte index is
  ready instantly and the OS shares its pages across processes. `StringIndex` maps the whole FST;
  `CompactHashIndex` maps its fingerprint table; `PerfectHashIndex` maps the key arena (the bulk) and
  reads only the tiny MPH into memory. Every read is byte-wise, so there is no alignment gotcha; the one
  caveat is the usual mmap contract — the file must not be mutated while an index borrows it.
- `mph` is opt-in-by-default: with `--no-default-features` the crate depends only on `fst` (and keeps
  `StringIndex`). Enabling `mph` pulls `ptr_hash` and its dependency tree, which currently carries a few
  informational RustSec advisories (unmaintained / unsound) on transitive crates — `cargo audit`
  reports them as warnings, not vulnerabilities. The `fst`-only build is free of them.

## Benchmarks

### Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|---:|
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | probabilistic | ✅ | **0.77** |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | probabilistic | ✅ | **1.27** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | probabilistic | ✅ | **2.27** |
| `marisa-trie` | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 |
| lexindex `PerfectHashIndex` | — | — | — | ✅ | ✅ | ✅ | 13.60 |
| DAWG (`dawg2`) | ✅ | — | — | — | ✅ | — | 23.96 |
| `datrie` | ✅ | — | — | — | ✅ | — | 30.69 |

Two honest crowns. **`CompactHashIndex` is the smallest `string → dense id` map — 2.3× below
`marisa-trie` at the default 8-bit fingerprint, 3.9× at 4 bits** — when you can accept a bounded
false-positive rate (exactly `2^-fingerprint_bits`: **6.25 %** at 4 bits, **≈0.4 %** at 8,
**≈0.0015 %** at 16, which the benchmark confirms) and don't need `id → key`. It stays below
marisa's 2.98 B/key at every width up to 21 bits — the [width guide](docs/usage.md) tables the
trade-off. **`StringIndex` is the only
structure that answers fuzzy and range queries at all**, at 4× below a plain DAWG. `marisa-trie`
remains the pick when you need *exact* membership *and* ordering *and* the smallest such index —
lexindex doesn't claim that particular cell (see below for why).

### Against other Rust string indexes

`marisa-trie` is C++. Among ordered string indexes you can `cargo add`, **none is smaller than
`StringIndex`** — the double-array tries trade space for lookup speed, and no succinct LOUDS trie
(marisa / XCDAT / CoCo-trie-style) exists in Rust to depend on. So `StringIndex` at 5.95 B/key is the
**smallest ordered `string → id` index available in pure Rust** — second only to a C++ library, and the
only one of them that does fuzzy and range. Same real words:

| Rust structure | bytes/key | vs marisa |
|---|---:|---:|
| `marisa-trie` (C++, reference) | 2.98 | 1.0× |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | **5.95** | 2.0× |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.6× |
| `yada` (double-array) | 15.98 | 5.4× |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× |

<sub>Measured with `crawdad` 0.4, `yada` 0.5, `fst` 0.4 over the same word list; size = serialised bytes
(`serialize_to_vec().len()`) ÷ key count. Not lexindex dependencies — reproduce in a throwaway crate.</sub>

Reaching `marisa`'s 2.98 needs its recursive succinct-trie label nesting, which the byte-oriented `fst`
automaton is ~1.6× away from by construction (even a bare `fst::Set`, which stores no ids at all, is
4.85) — so beating it on the *ordered* index means reimplementing marisa from scratch, not a bounded
tweak. `CompactHashIndex` takes the size crown the other way: by dropping the keys entirely.

### Point-lookup latency vs the standard library

`cargo run --release --example bench` — 1 M **real dictionary-word bigrams** (`word_i.word_j`, the
same key generator as `bench/scale.py`; mean key 10.9 bytes). Earlier editions of this table used
synthetic `entity-000…N` keys, which arrive pre-sorted and hash-degenerate — they flattered every
build time and violated this project's own benchmarking rule, so the whole table was re-measured on
real keys in one session (min of 12 runs, idle machine). Absolute numbers are machine-dependent; the
**ratios** are the point.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `PerfectHashIndex::id_unchecked` | ~302 ms | **~189 ns** | closed vocabulary, no membership check |
| lexindex `CompactHashIndex::id` (fp=1) | ~244 ms | ~237 ns | fingerprint-verified, `2^-8` false-positive rate |
| `std::HashMap<String, u32>` | ~239 ms | ~298 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~323 ms | ~327 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~270 ms | ~424 ns | *and* prefix / range / fuzzy |
| `std::BTreeMap<String, u32>` | ~231 ms | ~952 ns | in-RAM |

<sub>Run-to-run lookup spread was under 4% on every lexindex cell (builds vary more, up to ~12% on the `std` rows). Real keys move the numbers both ways versus the
old synthetic table: lookups favour lexindex *more* (shorter, realistic keys make its byte-wise FNV
cheaper relative to `HashMap`'s SipHash — the gap widened from ~1.25× to ~1.5×), while every `build`
reads higher because real input is not pre-sorted and sorting is part of the build.</sub>

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest** — ≈1.5× quicker than `HashMap` (no probing, no membership comparison) *and* compact +
serialisable. `CompactHashIndex::id` keeps a probabilistic membership check and *still* beats
`HashMap` while storing 10× less than `PerfectHashIndex`. Full verification (`id`) pays one extra
cache line + a key comparison; `StringIndex` trades more latency for **ordered / prefix / range /
fuzzy** queries the hash maps cannot answer at all. So: `CompactHashIndex` when footprint dominates
and a rare false positive is fine; `PerfectHashIndex::id` for exact membership + reverse;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map
with nothing persisted.

### Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.27 bytes/key holds constant** as
`n` grows:

| n | structure | build | bytes/key | peak RSS | lookup |
|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | 0.33 s | 0.68\* | 126 MB | 280 ns |
| 1 M | `CompactHashIndex` | 0.34 s | 1.27 | 161 MB | 209 ns |
| 10 M | `StringIndex` | 5.1 s | 2.00\* | 1.08 GB | 873 ns |
| 10 M | `CompactHashIndex` | 5.1 s | 1.27 | 1.35 GB | 372 ns |

<sub>\* bigram keys share far more prefixes than single words — at 1 M the generator draws on only
1 000 distinct words, which is why `StringIndex` compresses to an unrepresentative 0.68 B/key there;
the honest single-word figure is in the size table above. The whole table is one measurement session
on the 0.5.1 code (min of 3 runs per cell). Peak RSS includes the input key list, which dominates at
this scale and is why the column falls by 8-17% rather than by the 47-73% the build itself dropped
in 0.5.0. Linear extrapolation puts 100 M at ~50 s and ~13.5 GB (a big-memory box) — and at that
scale note `PerfectHashIndex`'s quantified hash-collision odds above; `StringIndex` and
`CompactHashIndex` are unaffected by them at any n (the fst build has no collision failure mode, and
the compact index tolerates fingerprint collisions by design).</sub>

## License

MIT © Ilia Gradina
