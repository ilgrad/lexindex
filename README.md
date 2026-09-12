# lexindex

[![PyPI](https://img.shields.io/pypi/v/lexindex)](https://pypi.org/project/lexindex/)
[![Python](https://img.shields.io/pypi/pyversions/lexindex)](https://pypi.org/project/lexindex/)
[![crates.io](https://img.shields.io/crates/v/lexindex)](https://crates.io/crates/lexindex)
[![CI](https://github.com/ilgrad/lexindex/actions/workflows/ci.yml/badge.svg)](https://github.com/ilgrad/lexindex/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-mkdocs-blue.svg)](https://ilgrad.github.io/lexindex/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/ilgrad/lexindex/blob/main/LICENSE)
[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.22119002.svg)](https://doi.org/10.5281/zenodo.22119002)
[![Sponsor](https://img.shields.io/badge/sponsor-GitHub%20Sponsors-ea4aaa?logo=githubsponsors&logoColor=white)](https://github.com/sponsors/ilgrad)

**Compact, immutable string ↔ id indexes for huge catalogs**, with a Rust core and Python bindings.
Build once over a set of strings — entity names, document keys, vocabulary terms, cluster labels —
persist a flat blob, and query it many times, memory-mapped where the structure allows. Pairs with
[`betula-cluster`](https://github.com/ilgrad/betula-cluster) (string ids ↔ cluster ids, both ways)
but stands on its own.

## Five indexes

| | `StringIndex` | `DictIndex` | `CompactHashIndex` | `ClosedHashIndex` | `PerfectHashIndex` |
|---|:---:|:---:|:---:|:---:|:---:|
| `string → id` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `id → string` | ✅ | ✅ | — | — | ✅ |
| ordered ids, ranges, `lower_bound` | ✅ | ✅ | — | — | — |
| prefix | ✅ | ✅ | — | — | — |
| common prefix · longest prefix | ✅ | ✅ | — | — | — |
| fuzzy · subsequence | ✅ | — | — | — | — |
| membership | exact | exact | `2^-bits` false positives | none: closed vocabulary | exact |
| `Overlay` edits | ✅ | — | ✅ | — | ✅ |
| zero-copy `load_mmap` | ✅ | ✅ | ✅ | — | ✅ |
| **bytes/key**, 480 k English words | 5.95 | **3.52** | **1.26** · 0.76 at 4 bits | **0.26** | 10.90 |
| `id`, 1 M word bigrams | 424 ns | 507 ns | 130 ns | the bare perfect hash | 301 ns · `id_unchecked` 74 |
| Cargo feature | — | — | `mph` (default) | `mph` | `mph` |

- **`StringIndex`** — an **ordered** index that is the finite-state transducer
  ([`fst`](https://crates.io/crates/fst)) alone: exact `string ↔ id`, **prefix**, **common prefix**
  (the keys a query starts with, in one walk), **range**, **predecessor / successor**, **fuzzy**
  (bounded Levenshtein distance), **subsequence** and lazy in-order iteration, all automata over the
  FST with no key list to scan. Autocomplete, fuzzy search,
  ordered browse.
- **`DictIndex`** — an **ordered** dictionary with the key stored for every id: `string ↔ rank` both
  ways, `lower_bound`, `prefix`, `common_prefix`, `range`, in-order iteration — no automata, so no
  fuzzy. The sorted
  keys front-coded in blocks of 32, the suffixes under a symbol table trained on the index itself:
  **3.52 bytes/key**, 41 % below `StringIndex`, `id` 314–337 ns against its 346–363, `key_into`
  173–176 against its `key` at 504–521. A prefix is a range here, not an automaton walk, so
  `prefix_count` is two order lookups — **338 ns where `marisa-trie` must enumerate every match to
  count it (122 247)**. Blocks of 128 store **2.89 bytes/key, under `marisa-trie` at every setting it
  has** *on this corpus* — over the thirteen-corpus set the ranking goes both ways, marisa smaller
  wherever the keys share deep structure and `DictIndex` smaller where they do not, while
  `DictIndex` answers 1.9–3.1× faster on all of them. Exact queries, every id back to its key,
  small.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map: an in-crate minimal perfect
  hash plus a fingerprint per key, *no keys stored*. **1.26 bytes/key** on real words — **2.4× below
  `marisa-trie`** — and **0.76** at a 4-bit fingerprint (6.25 % false positives), for
  **probabilistic membership** (about `2^-bits`) and no reverse lookup. Footprint first, a rare
  false positive acceptable.
- **`ClosedHashIndex`** — the perfect hash **and nothing else**: `id(key) -> u32`, no `Option` — a
  member's id, and *some* id in `[0, n)` for anything else. **0.26 bytes/key**, a fifth of
  `CompactHashIndex`, and a lookup at `id_unchecked`'s cost (40 ns on the dictionary, against 68
  for the fingerprint-checked `id`). A token → id map where every query is a member by construction.
- **`PerfectHashIndex`** — the perfect hash with the keys stored: **verified membership** and
  **`id → key`**, no ordering. `id_unchecked` skips the compare and runs 3.9× as fast as
  `std::HashMap`; `fingerprints=True` adds one byte per key so an absent key stops after one cache
  miss instead of two (166 → 74 ns on the dictionary) — a stop list, a block list. A fixed-vocabulary
  token ↔ id map on a hot path.

All five assign dense ids in `[0, n)`, **build deterministically** and **serialise to a flat blob**:
`save` / `load` everywhere, zero-copy `load_mmap` where there is more than the perfect hash to map.
They are immutable; **`Overlay`** adds and removes keys on `StringIndex`, `CompactHashIndex` and
`PerfectHashIndex` without a rebuild, keeps every id stable, and folds the edits into a fresh base
with `compact()`. The other two are absent by design rather than omission: an overlay issues a
new key the next id after the base, which is exactly what `DictIndex` cannot accept — its ids
*are* the lexicographic rank, and a key added in the middle of the order would not get one —
and `ClosedHashIndex` has no membership to ask, so there is no "already in the base" for an
overlay to test against. Every configuration builds on 32-bit targets, `wasm32-unknown-unknown` included
(leave `mmap` off there — nothing to map).

## Install

```bash
pip install lexindex      # one abi3 wheel for CPython 3.11+, no runtime dependencies
```

```toml
[dependencies]
lexindex = "2.1"
# fst-only (drop the memory-mapping and perfect-hash code):
# lexindex = { version = "2.1", default-features = false }
```

## Python

```python
from lexindex import ClosedHashIndex, CompactHashIndex, DictIndex, PerfectHashIndex, StringIndex

idx = StringIndex(["apple", "apricot", "banana", "cherry"])
idx.id("banana")             # 2  (sorted rank)
idx.key(0)                   # "apple"  — reconstructed from the FST, no stored reverse map
idx.prefix("ap")             # [("apple", 0), ("apricot", 1)]
idx.fuzzy("aple", 1)         # [("apple", 0)]  — typo-tolerant
idx.successor("ba")          # ("banana", 2)   — nearest key >= query
idx.ids_of(["apple", "x"])   # [0, None]  — batched: one FFI call, not one per key
idx.save("catalog.bix")      # StringIndex.load("catalog.bix") reloads it; load_mmap borrows it zero-copy

c = CompactHashIndex(["GET", "POST", "PUT", "DELETE"])  # ~1.3 B/key at scale; fingerprint_bits=4 → ~0.8
c.id("POST")                 # dense id in [0, n); probabilistic membership, no id → key
c.id_unchecked("POST")       # fastest lookup for a known-closed vocabulary

z = ClosedHashIndex(["GET", "POST", "PUT", "DELETE"])   # the perfect hash alone, ~0.26 B/key
z.id("POST")                 # a member's id; any other string gets *some* id in [0, n)

w = DictIndex(["GET", "POST", "PUT", "DELETE"])         # ordered, keys stored, ~3.5 B/key
w.id("POST")                 # 2  (sorted rank); w.key(2) == "POST"; w.lower_bound("P") == 2

d = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])  # verified membership and id → key
d.key(d.id("POST"))          # "POST"; d.id("PATCH") is None
```

[`examples/quickstart.py`](https://github.com/ilgrad/lexindex/blob/main/examples/quickstart.py) runs
all five end to end; the [usage guide](https://ilgrad.github.io/lexindex/usage/) covers every
interface, including batched lookups into NumPy and Arrow buffers and free-threaded CPython.

**With `betula-cluster`:** the lexindex dense id is the embedding-matrix row, so `string id →
cluster` and `cluster → string ids` are both one lookup
([runnable](https://github.com/ilgrad/lexindex/blob/main/examples/bridge_clustering.py)):

```python
idx = PerfectHashIndex(doc_ids)                  # string id <-> dense [0, n) id
matrix[idx.id(doc_id)] = embedding[doc_id]       # row index == lexindex id
labels = betula_cluster.fit_predict(matrix, n_clusters=k)
cluster = labels[idx.id("doc-00042")]            # string id -> cluster
members = [idx.key(int(r)) for r in (labels == cluster).nonzero()[0]]  # cluster -> string ids
```

## Rust

```rust
use lexindex::StringIndex;

let idx = StringIndex::build(["apple", "apricot", "banana", "cherry"])?;
assert_eq!(idx.id("banana"), Some(2));                  // string → id (sorted rank)
assert_eq!(idx.key(0).as_deref(), Some("apple"));       // id → string, a rank-walk over the FST

// prefix / range / fuzzy / subsequence, all lexicographically ordered
let fruit: Vec<_> = idx.prefix("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(fruit, ["apple", "apricot"]);
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);                            // Levenshtein distance ≤ 1
let sub: Vec<_> = idx.subsequence("ap").into_iter().map(|(k, _)| k).collect();
assert_eq!(sub, ["apple", "apricot"]);

// a flat blob: reload it, or borrow it zero-copy from the file
idx.save("catalog.bix")?;
// SAFETY: nothing may modify the file while a mapped index borrows it (see `load_mmap`).
let idx = unsafe { StringIndex::load_mmap("catalog.bix") }?; // no read into RAM; pages shared
# drop(idx);
# std::fs::remove_file("catalog.bix").ok();
# Ok::<(), lexindex::IndexError>(())
```

```rust
use lexindex::{ClosedHashIndex, CompactHashIndex, DictIndex, PerfectHashIndex};

let verbs = ["GET", "POST", "PUT", "DELETE"];

// The smallest string → id map: an 8-bit fingerprint per key, ~1.3 B/key, ~0.4 % false positives.
let compact = CompactHashIndex::build(verbs, 1)?;
let id = compact.id("POST").unwrap();                  // Some(slot); a stranger may rarely read as present
assert_eq!(compact.id_unchecked("POST"), id);          // no fingerprint check, for a closed vocabulary

// The perfect hash alone, ~0.26 B/key: a member's id, and *some* id in [0, n) for anything else.
let closed = ClosedHashIndex::build(verbs)?;
assert!((closed.id("POST") as usize) < closed.len());

// Verified membership and id → key, the keys stored; ids survive save / load on every index.
let exact = PerfectHashIndex::build(verbs)?;
let id = exact.id("POST").unwrap();
assert_eq!(exact.key(id), Some("POST"));
assert_eq!(exact.id("PATCH"), None);
exact.save("verbs.bmp")?;
assert_eq!(PerfectHashIndex::load("verbs.bmp")?.id("POST"), Some(id));

// Ordered, the key stored for every id, ~3.5 B/key; prefix and range, no fuzzy.
let dict = DictIndex::build(verbs)?;
assert_eq!(dict.id("POST"), Some(2));                  // the sorted rank
assert_eq!(dict.key(2).as_deref(), Some("POST"));
assert_eq!(dict.lower_bound("P"), 2);                  // the "P…" keys are ids 2..lower_bound("Q")
# std::fs::remove_file("verbs.bmp").ok();
# Ok::<(), lexindex::IndexError>(())
```

## Design notes

One line each; the sections are in [the design notes](https://ilgrad.github.io/lexindex/design/).

- **`StringIndex` is the FST alone.** `id → key` is a rank-walk over the automaton, so the blob is
  `[magic "BIX4"][fst]` and there is no reverse map to store or keep in sync.
- **`DictIndex` is front coding under a symbol table.** Blocks of 32 sorted keys, the first whole and
  the rest as (shared-prefix length, suffix), the suffixes under a 255-symbol FSST-style table (its
  own format) trained on the index's own suffixes; `id` compares the stored suffixes against the
  probe without decoding them.
- **`CompactHashIndex` stores no keys.** A minimal perfect hash plus one `fingerprint_bits`-wide
  fingerprint per slot from a second, uncorrelated hash — a design rate of about `2^-bits`, not a
  defence against chosen queries. Its build streams 16 bytes per key, never the strings: 302 MB peak
  at 100 M keys against 8.8 GB for a list, 0.94 GB at 10⁹.
- **`ClosedHashIndex` is that perfect hash alone** — the same slot `CompactHashIndex::id_unchecked`
  gives, with a signature that says nothing can tell a member from a stranger.
- **`PerfectHashIndex` verifies every hit against the stored key.** The pair in a billion that
  collides in the 64-bit hash is served, still exactly, from a side table the hot path never reads.
- **Keys are bytes.** No Unicode normalisation, case folding or collation: normalise (NFC/NFKC,
  casefold) before building *and* before querying if the application needs it.
- **Every build is deterministic.** The same keys give the same blob, byte for byte, on any machine
  and thread count — within one version; ids are arbitrary and change whenever the key set does, so
  persist the blob rather than re-derive it.
- **Loading is safe; mapping is `unsafe`.** `from_bytes` and `load` take arbitrary bytes on every
  index — the reason the perfect hash is in-crate — and a crafted blob answers wrong ids, never
  out-of-range ones. `load_mmap` and its `_verified` / `_untrusted` forms borrow the mapped pages,
  so the file must not change while the index is alive.
- **Blobs move forward, not backward.** 2.0 replaced the key hash (the previous one had a two-word
  collision family on ordinary text), so every hash blob written before it (`BMP5`, `BMP6`, `BCH6`)
  is refused by name and rebuilt from the keys; `BIX4` crosses the versions unchanged, and an
  `OVL2` does when its base is one — an overlay embeds its base, so one over a 1.x hash blob is
  refused with it.
- **`--no-default-features` is `fst` only** (`StringIndex`, `DictIndex`, `Overlay`); `mph` adds no
  dependency, so the whole tree is `fst` plus `memmap2`, and `cargo audit` reports nothing on either.

## Benchmarks

### Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a
real vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** | **ns/lookup** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | none (closed vocabulary) | — | **0.26** | 100 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | probabilistic | ✅ | **0.76** | 98 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | probabilistic | ✅ | **1.26** | **92** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | probabilistic | ✅ | **2.26** | 100 |
| **lexindex `DictIndex` (128 per block)** | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.89** | 389 |
| `marisa-trie` (4 tries, tiny cache — its smallest) | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 490 |
| `marisa-trie` (default) | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 472 |
| `marisa-trie` (huge cache) | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 449 |
| **lexindex `DictIndex` (32 per block, default)** | ✅ | ✅ | — | ✅ | ✅ | ✅ | **3.52** | 285 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 317 |
| lexindex `PerfectHashIndex` | — | — | — | ✅ | ✅ | ✅ | 10.90 | 216 |
| DAWG (`dawg2`) | ✅ | — | — | — | ✅ | — | 23.96 | 246 |
| `datrie` | ✅ | — | — | — | ✅ | — | 30.91 | 590 |
| builtin `dict` | — | — | — | — | ✅ | — | — (in RAM only) | 260 |

<sub>Generated by `bench/compare.py` — raw numbers and the machine that produced them:
[`bench/results/compare-2026-09-12-arz-bf5c1b9.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-12-arz-bf5c1b9.json)
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. **`ns/lookup`** is one exact lookup through
Python over 100 000 probes, half of them plausible near-misses, shuffled — the counterweight to the
size column, since bytes alone read as though the smallest structure were the best one. Every row
pays a 49 ns call boundary, and the builtin `dict` is in the table because it is the thing being
replaced. The four smallest rows are also the fastest, and for the same reason: they store no keys,
so a miss is only probably detected and there is no `id → key` on offer. The two `DictIndex` rows are
one type at two block sizes; it builds in 134 ms against `marisa-trie`'s 239–250, and the larger
block trades reverse-lookup latency for the bytes. `marisa-trie` appears three times for the same reason it has tuning parameters: its
own documentation says the right setting depends on the data, so the table carries its compact end,
its default and its fast end rather than one point somebody could fairly call untuned.
[The benchmark notes](https://github.com/ilgrad/lexindex/blob/main/docs/benchmarks.md) table the
whole block-size curve, the prefix queries, and the same nine structures over a pinned set of
thirteen corpora at three scales — where the ranking between `DictIndex` and `marisa-trie` reverses
with how much the keys share, which one word list cannot show.</sub>

Two claims, scoped to libraries a Python or Rust project can install — research-grade C++ tries
(CoCo-trie, C², XCDAT, PDT, SuRF) are the published frontier and are cited, not claimed against,
since none has a binding to benchmark here.
**`CompactHashIndex` is the smallest `string → dense id` map here, 2.4× below `marisa-trie` at the
default 8-bit fingerprint and 3.9× at 4 bits**, when a bounded false-positive rate is acceptable:
about `2^-fingerprint_bits` by design, measured **6.2530 %** at 4 bits and **1.5553 %** at 6 over
2 M non-member probes (z = +0.18 / −0.83 against theory), ≈0.4 % at 8, ≈0.0015 % at 16. Both hashes
are deterministic and unseeded, so an adversary who chooses the queries can find false positives at
will — it is not a security primitive. **`StringIndex` is the only structure here that answers
*fuzzy* and subsequence queries**, at 4× below a plain DAWG; ordered range queries `DictIndex`
answers too, and more cheaply. On this corpus `DictIndex` at 128 keys per block is smaller than
`marisa-trie` while answering everything marisa does and `key(id)`, `lower_bound` and `range`
besides — but a trie's size swings 3× across corpora and marisa has tuning parameters of its
own, so that is a result about these words at these settings rather than a general ranking
([how it was measured](https://ilgrad.github.io/lexindex/benchmarks/#against-other-rust-string-indexes)).

### Which one to pick

Every size above is one corpus at one `n`, and the ranking is stable across neither: a trie's size
depends on how much the keys share, a fingerprint index's does not
([three corpora, and 10 M](https://ilgrad.github.io/lexindex/benchmarks/#which-one-to-pick-and-how-much-the-corpus-decides-it)).
In decision order:

- **Do the keys need to come back out, or be scanned in order?** Then the fingerprint indexes are
  out: `StringIndex` for prefix / range / fuzzy, `DictIndex` for exact `string ↔ rank` at 41 % less,
  `PerfectHashIndex` for `id → key` without ordering — and each pays for the keys it stores.
- **Is a bounded false-positive rate acceptable?** Then `CompactHashIndex`: 2.4× under `marisa-trie`
  on single words, 4.9× on random pairs, 3.3× at 10 M — and exactly one byte per key above the bare
  `ClosedHashIndex` (1.26 against 0.26), which is the fingerprint that buys the membership check.
- **Do the keys share a lot of structure** (a path namespace, a versioned catalogue, a cross product)?
  Measure before choosing: that is where an FST can beat a keyless hash outright.
- **A `dict` / `HashMap` is not in the table** because it has no serialised form: 71–95 bytes per key
  above the key list across these corpora (58–60 at 10 M), rebuilt from the keys on every process
  start, where every structure here is mapped from a file.

### Point-lookup latency vs the standard library

`cargo run --release --example bench` — 1 M **real dictionary-word bigrams** (`word_i.word_j`, mean
key 10.9 bytes; never a synthetic `entity-000…N` sequence, which arrives pre-sorted and
hash-degenerate). Measured on 2.0.0, the better of two runs on a rested machine, each lookup cell the
minimum of five passes after a warm-up
([`latency-rs-2026-09-10-arz-16c7abe.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-09-10-arz-16c7abe.txt)).
Absolute numbers are one machine on one day — this session reads the `std::HashMap` control 18 %
slower than the 1.1.0 session (245 → 289 ns), and `StringIndex`, unchanged since 0.5.1, moved from
1.30× to 1.47× of it — so read the **ratios within a column**, and a shift under ~15 % between
tables as the session. What did move: `CompactHashIndex` builds in 45 ms against 69 on 1.1.0, with
every other build 10–17 % slower — 2.0's placement on every thread.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~45 ms** | ~130 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~275 ms | **~74 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~208 ms | ~289 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~280 ms | ~301 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~271 ms | ~424 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (32 per block) | ~203 ms | ~507 ns | ordered, exact reverse; its worst case — a `word.word` cross product is what a transducer factors out (0.68 B/key against 3.19 here; on the dictionary 3.52 against 5.95, 314–337 ns against 346–363) |
| `std::BTreeMap<String, u32>` | ~226 ms | ~960 ns | in-RAM |

**Reading it:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the fastest
structure in the table — 3.9× as quick as the SipHash `HashMap` and 2.3× an FxHash one — *and*
compact and serialisable. `CompactHashIndex::id` keeps a probabilistic membership check and still
beats the `HashMap` 2.2× on lookup, and builds in a fifth of its time. Verified `id` pays one extra
cache line and a key compare; `StringIndex` trades latency for the queries a hash map cannot answer
at all. The other Rust string indexes, the three-corpus table, the Python-level table against `dict`
and `marisa-trie`, the 1 M / 10 M scale table and the protocol behind every number are in
[the benchmarks](https://ilgrad.github.io/lexindex/benchmarks/).

## Security

Every loader is a safe fn on arbitrary bytes since 1.0: a crafted blob answers wrong ids, never
out-of-range ones. The `load_mmap` family is what is `unsafe`, and its obligation is about the file,
not the bytes. The checksums are integrity and not authentication, and the hashes are unseeded, so
this is not a HashDoS defence — the threat model and the supported versions are in
[`SECURITY.md`](https://github.com/ilgrad/lexindex/blob/main/SECURITY.md).

## Sponsoring

If lexindex saves memory or latency in a system you run, consider
[sponsoring its development](https://github.com/sponsors/ilgrad). **Using it in production?**
Corporate sponsorship funds what keeps a library like this dependable — compatibility across Rust
and Python releases, the benchmark suite behind every number above, security hardening of the
loaders, and performance work at hundreds of millions of keys — and tells the maintainer which
workloads to measure next.

## Prior art

The minimal perfect hash under the three hash indexes is in-crate and follows **PHast**'s
map-or-bump construction, the successor of PTHash: keys grouped into buckets by a first hash, a
one-byte seed per bucket that slides the bucket's keys along a short slice of the table until every
one lands on a free value, the buckets no seed places *bumped* to a smaller table under a fresh hash,
and a remap that pulls every bumped key into a hole the first table left. Nothing is ever displaced,
which is what makes the build one streaming pass over sorted hashes.

- Giulio Ermanno Pibiri and Roberto Trani, *PTHash: Revisiting FCH Minimal Perfect Hashing*,
  SIGIR 2021 — [arXiv:2104.10402](https://arxiv.org/abs/2104.10402).
- Piotr Beling and Peter Sanders, *PHast — Perfect Hashing with fast evaluation*, 2025 —
  [arXiv:2504.17918](https://arxiv.org/abs/2504.17918).
- Ragnar Groot Koerkamp, *PtrHash: Minimal Perfect Hashing at RAM Throughput*, 2025 —
  [arXiv:2502.15539](https://arxiv.org/abs/2502.15539),
  [`ptr_hash`](https://github.com/RagnarGrootKoerkamp/PtrHash).

Until 1.0 the perfect hash **was** `ptr_hash`. Its pilot table was serialised behind private fields,
so a blob holding one could not be validated from outside the crate that owned it, and `from_bytes`
and `load_mmap` had to be `unsafe fn` on both hash indexes; an MPH whose every array length is
written and checked here makes those loaders safe, and that is the whole of the trade. 1.1's
PHast-shaped table builds 10 M real word-bigram hashes in **49 ns/key on one thread** (9 ns/key on
eight) at **2.09 bits/key**, against 280 ns/key and 2.39 bits for 1.0's, and its lookup costs
4.2 ns/key on in-order probes; the same-process comparison with `ptr_hash` and the PHast authors'
`ph` crate is [in the benchmarks](https://ilgrad.github.io/lexindex/benchmarks/#the-perfect-hash-against-ptrhash-and-phast).

## License

MIT © Ilia Gradina
