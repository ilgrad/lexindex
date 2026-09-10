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

Four complementary, build-once / query-many structures — pick by what you need to ask:

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer
  ([`fst`](https://crates.io/crates/fst)). Exact `string → id` and `id → string`, plus **prefix**,
  **range**, **predecessor / successor** (nearest key ≤ / ≥ a query), **fuzzy** (bounded Levenshtein
  edit distance), **subsequence**, and **lazy full iteration** — all driven by automata over the FST,
  with no separate key list to scan (exact/prefix/range seek directly; a broad fuzzy or subsequence
  pattern may still traverse most of the automaton) — in a compressed, serialisable, memory-mappable
  form. The only structure here that
  answers **ordered and typo-tolerant** queries. Use it for autocomplete, fuzzy search, browse, and
  ordered scans of a large catalog.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map: a minimal perfect hash
  (in-crate, no dependency) plus a small fingerprint per key, storing *no keys
  at all*. **1.26 bytes/key** on real dictionary words — **2.4× smaller than `marisa-trie`**, down to
  **0.76 bytes/key** at a 4-bit fingerprint (`fingerprint_bits=4`, 6.25% false-positive rate) — below
  every trie benchmarked (see [Benchmarks](#benchmarks)) — at the cost of **probabilistic membership**
  (a tunable `2^-bits` false-positive rate) and **no reverse lookup**. Use it when a fixed vocabulary's
  footprint is paramount and rare false positives are acceptable.
- **`ClosedHashIndex`** — the perfect hash **and nothing else**: `id(key) -> u32`, no `Option`,
  for a vocabulary known to be closed. A member's id, and for anything else *some* id in `[0, n)`.
  **0.26 bytes/key**, a fifth of `CompactHashIndex`, and a lookup as fast as `id_unchecked` (40 ns
  on the dictionary, against 68 for a fingerprint-checked `id`). Use it as a token → id map where
  every query is a member by construction.
- **`PerfectHashIndex`** — a minimal-perfect-hash dictionary with **verified membership** (`id`) and
  **reverse lookup** (`key`); the arena stores full keys, so it is exact but larger. For a known-closed
  vocabulary, `id_unchecked` skips the membership comparison and is **faster than `std::HashMap`**. Use
  it as a fixed-vocabulary token↔id map on a hot path when you need exact membership and `id → key`.
  Built with `fingerprints=True` (`build_with_fingerprints`), one more byte per key lets a lookup of
  an *absent* key stop after one cache miss instead of two — misses 1.8× faster, for a stop list or
  a block list.

All four assign dense ids in `[0, n)` and **serialise to a flat blob** (`save` / `load`, or zero-copy
`load_mmap` where there is more than the perfect hash to map) — build once, persist, then reload and
query many times. All are immutable after building; **`Overlay`** sits on top of the three that
check membership to add and remove keys without a rebuild, keeping
every id stable, and folds the edits back into a fresh base with `compact()`.
The `mph` feature (on by default) provides the two hash indexes. Every configuration builds for
**32-bit targets**, `wasm32-unknown-unknown` included; `mmap` is the one to leave off there, since
there is nothing to memory-map.

## Python

```bash
pip install lexindex
```

```python
from lexindex import ClosedHashIndex, CompactHashIndex, PerfectHashIndex, StringIndex

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

z = ClosedHashIndex(["GET", "POST", "PUT", "DELETE"])  # the perfect hash alone (~0.26 B/key)
z.id("POST")                 # a member's id; any other string gets *some* id in [0, n)

d = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
d.id("POST")                 # dense id in [0, n); membership verified, returns None if absent
d.key(d.id("POST"))          # "POST"  — exact reverse lookup (keys stored)
```

No runtime dependencies; a single abi3 wheel covers CPython 3.11+. See
[`examples/quickstart.py`](https://github.com/ilgrad/lexindex/blob/main/examples/quickstart.py) for all
four indexes end to end, and the [documentation site](https://ilgrad.github.io/lexindex/).

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
lexindex = "1.1"
# fst-only (drop the memory-mapping and perfect-hash code):
# lexindex = { version = "1.1", default-features = false }
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
// SAFETY: nothing may modify the file while a mapped index borrows it (see `load_mmap`).
let idx = unsafe { StringIndex::load_mmap("catalog.bix") }?; // no read into RAM; pages shared
# drop(idx);
# std::fs::remove_file("catalog.bix").ok();
# Ok::<(), lexindex::IndexError>(())
```

```rust
use lexindex::PerfectHashIndex;            // requires the default `mph` feature

let dict = PerfectHashIndex::build(["GET", "POST", "PUT", "DELETE"])?;
let id = dict.id("POST").unwrap();             // exact lookup, dense id in [0, n)
assert_eq!(dict.key(id), Some("POST"));
assert_eq!(dict.id("PATCH"), None);            // membership is verified, not just hashed

// persist the MPH and reload it (the dense ids are preserved across save/load)
dict.save("verbs.bmp")?;
let dict = PerfectHashIndex::load("verbs.bmp")?;
assert_eq!(dict.id("POST"), Some(id));
# std::fs::remove_file("verbs.bmp").ok();
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

```rust
use lexindex::ClosedHashIndex;            // requires the default `mph` feature

// The perfect hash and nothing else (~0.26 B/key), for a vocabulary known to be closed.
let vocab = ClosedHashIndex::build(["GET", "POST", "PUT", "DELETE"])?;
let id = vocab.id("POST");                     // a member's id; a stranger gets *some* id in [0, n)
assert!((id as usize) < vocab.len());
# Ok::<(), lexindex::IndexError>(())
```

## Design notes

Each of these is a section of [`docs/design.md`](docs/design.md); the one-line versions:

- **`StringIndex` is the FST alone.** `id → key` is a rank-walk over the automaton, so the blob is
  `[magic "BIX4"][fst]` and there is no reverse map to store or keep in sync.
- **No Unicode normalisation, case folding or collation.** Keys and queries are compared as UTF-8
  bytes; normalise (NFC/NFKC, casefold) before building *and* before querying if the application
  needs it.
- **Every index builds deterministically.** The same keys give the same blob, byte for byte, on any
  machine and any thread count — within one lexindex version; a release may change a hash or the
  perfect hash, and `docs/design.md` says which did. Ids are still arbitrary and change whenever
  the key set does, so persist the blob rather than re-deriving it.
- **`CompactHashIndex` stores no keys**: a minimal perfect hash plus one `fingerprint_bits`-wide
  fingerprint per slot, from a second hash uncorrelated with the first, so a non-member survives
  with probability about `2^-bits` — a design rate, not a defence against chosen queries — and
  there is no `id → key`. Its build streams: 16 bytes per key, never the strings — and
  `build_to_file` spills those beside the output: 302 MB peak at 100 M keys against 8.8 GB for a list,
  0.94 GB at 10⁹.
- **`ClosedHashIndex` is that perfect hash alone.** Nothing stored can tell a member from a
  stranger, so nothing tries: `id` is a `u32`, the same slot `CompactHashIndex::id_unchecked`
  gives over the same keys, and the type exists so that the signature says so.
- **`PerfectHashIndex` verifies every hit against the stored key.** The pair in a billion that
  collides in the 64-bit hash is served, still exactly, from a side table the hot path never reads.
- **`from_bytes` and `load` are safe on every index — the reason the perfect hash is in-crate** —
  and a crafted blob answers wrong ids, never out-of-range ones. **`load_mmap` and its `_verified`
  and `_untrusted` forms are the `unsafe fn`s**: they borrow the mapped pages, so the file must not
  change while the index is alive.
- **Blobs move forward, not backward.** 1.2 replaced the key hash — the round it shipped with had
  a two-word collision family on ordinary text — so every hash blob written before it (`BMP5`,
  `BMP6`, `BCH6`) is refused by name, and rebuilding from the keys is the migration. `BIX4` and
  `OVL2` are unchanged in either direction.
- With `--no-default-features` the crate is `fst` only; `mph` adds no dependency, so the whole tree
  is `fst` plus `memmap2`, and `cargo audit` reports nothing on either build.

## Benchmarks

### Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | none (closed vocabulary) | — | **0.26** |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | probabilistic | ✅ | **0.76** |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | probabilistic | ✅ | **1.26** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | probabilistic | ✅ | **2.26** |
| `marisa-trie` | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 |
| lexindex `PerfectHashIndex` | — | — | — | ✅ | ✅ | ✅ | 10.90 |
| DAWG (`dawg2`) | ✅ | — | — | — | ✅ | — | 23.96 |
| `datrie` | ✅ | — | — | — | ✅ | — | 30.92 |

<sub>Raw numbers and the machine that produced them:
[`bench/results/compare-2026-09-09-arz-0c637f6.json`](bench/results/compare-2026-09-09-arz-0c637f6.json)
— every cell's build samples, the false-positive measurement, the CPU, kernel, rustc, Python and the
load average at both ends of the run.</sub>

Two honest crowns, both scoped to what is measured above — libraries a Python or Rust project can
actually install. Research-grade C++ (CoCo-trie, XCDAT, PDT, SuRF) has no bindings to benchmark and
is not claimed against. **`CompactHashIndex` is the smallest `string → dense id` map here — 2.4×
below `marisa-trie` at the default 8-bit fingerprint, 3.9× at 4 bits** — when you can accept a bounded
false-positive rate (about `2^-fingerprint_bits` by design — the fingerprint comes from a second hash,
uncorrelated with the slot hash for well-distributed keys — measured **6.2530 %** at 4 bits and **1.5553 %** at 6 over 2 M non-member probes,
z = +0.18 / −0.83 against theory; **≈0.4 %** at 8 bits, **≈0.0015 %** at 16) and don't need
`id → key`. It is not a security primitive: both hashes are deterministic and unseeded, so an
adversary who chooses the queries can find false positives at will. It stays below
marisa's 2.98 B/key at every width up to 21 bits — the [width guide](docs/usage.md) tables the
trade-off. **`StringIndex` is the only
structure that answers fuzzy and range queries at all**, at 4× below a plain DAWG. `marisa-trie`
remains the pick when you need *exact* membership *and* ordering *and* the smallest such index —
lexindex doesn't claim that particular cell ([why](docs/benchmarks.md#against-other-rust-string-indexes)).

### Which one to pick

Every size above is one corpus at one `n`, and the ranking is stable across neither: a trie's size
depends on how much the keys share, a fingerprint index's does not. The same structures over three
corpora, and at 10 M, are tabled in
[`docs/benchmarks.md`](docs/benchmarks.md#which-one-to-pick-and-how-much-the-corpus-decides-it).
In decision order:

- **Do the keys need to come back out, or be scanned in order?** If yes, the fingerprint indexes are
  out; `StringIndex` (ordered, prefix / range / fuzzy / subsequence) or `PerfectHashIndex` (exact
  membership, `id → key`, no ordering) are the candidates, and both pay for the keys they store.
- **Is a bounded false-positive rate acceptable?** If yes, `CompactHashIndex` is 2.4× smaller than
  `marisa-trie` on single words, 4.9× on random pairs and 3.3× at 10 M — and one byte per key
  *larger* than a bare MPHF (1.26 against 0.26), which is exactly the fingerprint that buys the
  membership check.
- **Do the keys share a lot of structure** (a path namespace, a versioned catalogue, a cross product)?
  Then measure before choosing: that is the regime where an FST can beat a keyless hash outright.
- **A `dict` / `HashMap` is not in the table** because it has no serialised form to measure. It cost
  71–95 bytes per key above the key list itself across these corpora (58–60 at 10 M, where the table
  amortises better), and it has to be rebuilt from the keys on every process start; every structure
  here is mapped from a file instead.

### Point-lookup latency vs the standard library

`cargo run --release --example bench` — 1 M **real dictionary-word bigrams** (`word_i.word_j`, the
same key generator as `bench/scale.py`; mean key 10.9 bytes). Keys are never synthetic
`entity-000…N` sequences — those arrive pre-sorted and hash-degenerate and flatter every number.
Measured on 1.1.0 (one run of the example; each lookup cell is the minimum of five timed passes
after a warm-up pass) on a machine idle throughout (load 1.0). Absolute numbers are
machine-dependent — the `std::HashMap` control reads 21 % *faster* than in the session that
produced the previous table, which had an editor holding a core — so compare the **ratios**, and
only within a column: against that `HashMap`, `CompactHashIndex::id` is 0.44×, `id_unchecked`
0.27×, `PerfectHashIndex::id` 0.95×, `StringIndex` 1.30×, `BTreeMap` 3.19×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~69 ms** | ~106 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~246 ms | **~66 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~178 ms | ~245 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~255 ms | ~232 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~248 ms | ~317 ns | *and* prefix / range / fuzzy |
| `std::BTreeMap<String, u32>` | ~197 ms | ~779 ns | in-RAM |

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest of the structures in the table above** — 3.7× as quick as the SipHash `HashMap` and 2.2×
the FxHash one (no probing, no membership comparison) *and* compact + serialisable.
`CompactHashIndex::id` keeps a probabilistic membership check and *still* beats the SipHash
`HashMap` on lookup (2.3× here), and builds faster than it too. Full verification (`id`) pays one extra
cache line + a key comparison; `StringIndex` trades more latency for **ordered / prefix / range /
fuzzy** queries the hash maps cannot answer at all. So: `CompactHashIndex` when footprint dominates
and a rare false positive is fine; `PerfectHashIndex::id` for exact membership + reverse;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map
with nothing persisted.

The rest — the other Rust string indexes, the three-corpus table, the Python-level latency table
against `dict` and `marisa-trie`, the 1 M / 10 M scale table, and the measurement protocol behind
every number — is in [`docs/benchmarks.md`](docs/benchmarks.md).

## Security

Every loader is a safe fn on arbitrary bytes since 1.0, and the `load_mmap` family is what is not —
its obligation is about the file, not the bytes. What the blob formats do and do not defend against
is [`SECURITY.md`](SECURITY.md): a crafted blob answers wrong ids, never out-of-range ones; the
checksums are integrity and not authentication; and the hashes are unseeded, so this is not a
HashDoS defence.

## Prior art

`PerfectHashIndex` and `CompactHashIndex` are built on a minimal perfect hash implemented in this
crate, and its construction is **PHast's** map-or-bump: keys grouped into buckets by a first hash, a
one-byte seed per bucket that slides the bucket's keys along a short slice of the table until every
one lands on a free value, buckets that no seed places *bumped* to a smaller table under a fresh
hash, and a remap that pulls every bumped key into a hole the first table left. Nothing is ever
displaced, which is what makes the build one streaming pass over sorted hashes.

- Giulio Ermanno Pibiri and Roberto Trani, *PTHash: Revisiting FCH Minimal Perfect Hashing*,
  SIGIR 2021 — [arXiv:2104.10402](https://arxiv.org/abs/2104.10402).
- Piotr Beling and Peter Sanders, *PHast — Perfect Hashing with fast evaluation*, 2025 —
  [arXiv:2504.17918](https://arxiv.org/abs/2504.17918).
- Ragnar Groot Koerkamp, *PtrHash: Minimal Perfect Hashing at RAM Throughput*, 2025 —
  [arXiv:2502.15539](https://arxiv.org/abs/2502.15539),
  [`ptr_hash`](https://github.com/RagnarGrootKoerkamp/PtrHash).

Until 1.0 the perfect hash **was** `ptr_hash`. It was replaced because its pilot table was
serialised behind private fields, so a blob holding one could not be validated from outside the crate
that owned it, and `from_bytes` and `load_mmap` had to be `unsafe fn` on both hash indexes. An MPH
whose every array length is written and checked here makes those loaders safe, and that is the whole
of the trade. 1.0's own table was PtrHash-shaped and paid for the safety with a build about ten
times slower than `ptr_hash`'s; 1.1's is PHast-shaped, and over 10 M real word-bigram hashes it
builds in **49 ns/key on one thread** (0.49 s; 9 ns/key on eight) at **2.09 bits/key**, against
280 ns/key and 2.39 bits for 1.0, and its lookup costs 4.2 ns/key on in-order probes. Those are this crate's
numbers on this machine from the spike in `src/mphf.rs`; the same-process comparison with
`ptr_hash` and the PHast authors' `ph` crate is in [`docs/benchmarks.md`](docs/benchmarks.md).

## License

MIT © Ilia Gradina
