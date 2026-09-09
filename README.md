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
  with no separate key list to scan (exact/prefix/range seek directly; a broad fuzzy or subsequence
  pattern may still traverse most of the automaton) — in a compressed, serialisable, memory-mappable
  form. The only structure here that
  answers **ordered and typo-tolerant** queries. Use it for autocomplete, fuzzy search, browse, and
  ordered scans of a large catalog.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map: a minimal perfect hash
  (in-crate, no dependency) plus a small fingerprint per key, storing *no keys
  at all*. **1.30 bytes/key** on real dictionary words — **2.3× smaller than `marisa-trie`**, down to
  **0.80 bytes/key** at a 4-bit fingerprint (`fingerprint_bits=4`, 6.25% false-positive rate) — below
  every trie benchmarked (see [Benchmarks](#benchmarks)) — at the cost of **probabilistic membership**
  (a tunable `2^-bits` false-positive rate) and **no reverse lookup**. Use it when a fixed vocabulary's
  footprint is paramount and rare false positives are acceptable.
- **`PerfectHashIndex`** — a minimal-perfect-hash dictionary with **verified membership** (`id`) and
  **reverse lookup** (`key`); the arena stores full keys, so it is exact but larger. For a known-closed
  vocabulary, `id_unchecked` skips the membership comparison and is **faster than `std::HashMap`**. Use
  it as a fixed-vocabulary token↔id map on a hot path when you need exact membership and `id → key`.

All three assign dense ids in `[0, n)` and **serialise to a flat blob** (`save` / `load`, or zero-copy
`load_mmap`) — build once, persist, then reload and query many times. All are immutable after
building; **`Overlay`** sits on top of any of them to add and remove keys without a rebuild, keeping
every id stable, and folds the edits back into a fresh base with `compact()`.
The `mph` feature (on by default) provides the two hash indexes. Every configuration builds for
**32-bit targets**, `wasm32-unknown-unknown` included; `mmap` is the one to leave off there, since
there is nothing to memory-map.

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
lexindex = "1.0"
# fst-only (drop the memory-mapping and perfect-hash code):
# lexindex = { version = "1.0", default-features = false }
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

## Design notes

- **`StringIndex` is the FST alone — `id → key` is reconstructed by a rank-walk, with no stored reverse
  map.** Ids are the sorted rank of each key, which is exactly the FST's output value, so `key(id)`
  walks the automaton from the root, at each node taking the last transition whose accumulated output
  stays `≤ id`, and returns the path once the outputs sum to exactly `id`. That is `O(key length)` and
  needs no auxiliary structure, so the serialised blob is just `[magic "BIX4"][fst]` — half the size of
  the 0.2.0 front-coded layout on real words (12.6 → 5.95 B/key) and simpler to reason about.
  `from_bytes`/`load` validate the magic, verify the FST's stored checksum and spot-check the rank
  invariant (first value 0, rank-walk to `n - 1` succeeds — a full walk would cost 58× the load), so
  a truncated or corrupted owned blob is rejected at load rather than queried; `load_mmap` skips
  that `O(n)` scan to keep mapping constant-time, so a mapped file is trusted to be intact.
- **No Unicode normalisation, case folding, collation or grapheme segmentation.** Keys and queries
  are compared as UTF-8 byte strings, and "character" means a Unicode scalar value: `é` and
  `e\u{301}` are two different keys, an emoji ZWJ sequence is several characters to `fuzzy` and
  `subsequence`, and ordering is byte order, not any locale's. Normalise (NFC/NFKC, casefold) before
  building *and* before querying if the application needs it.
- **Every index builds deterministically.** The same key set produces the same blob, byte for byte,
  on any machine and any thread count — the perfect hash's construction is a fixed sequence of seeds,
  not a randomised search. Ids are still *arbitrary* (nothing about a key predicts its id) and they
  change whenever the key set does, so persist the **blob** rather than re-deriving it whenever an id
  is written down elsewhere. `StringIndex` ids are the sorted rank, reproducible by construction.
- **`CompactHashIndex` stores no keys — only a minimal perfect hash and one small fingerprint per
  slot.** `id(key)` hashes the key to a slot (the MPH), then compares the key's `b`-bit fingerprint —
  from a *second* hash with a different basis and multiplier — against the stored one; a match is a
  hit. The two hashes are uncorrelated for well-distributed keys, so a non-member survives both with
  probability about `2^-b`: a design rate measured against, not a proof, and no guarantee at all
  against queries chosen by an adversary (both hashes are deterministic and unseeded). It is the
  tunable false-positive rate
  (`fingerprint_bits` ∈ 1..=64, bit-packed). Dropping the key arena is what takes it below
  `marisa-trie`; the price is that membership is probabilistic and there is no `id → key`. The blob
  is `[magic "BCH6"][n][fp_bits][mph_len][side_len][payload][check][MPH blob][bit-packed
  fingerprints][side]` — the payload hash is verified on owned loads, so a corrupted blob fails
  cleanly. Its build **streams**: only a 16-byte `(hash, second hash)` pair is kept per key, never
  the strings. Blobs written before 1.0 (`BCH1`–`BCH5`) are **refused**: each embeds a `ptr_hash`
  image the crate no longer links, and with no keys stored there is nothing to convert — rebuild
  from the key list, which a caller of a keyless index necessarily has.
- **`PerfectHashIndex`** keys the MPH on a deterministic 64-bit hash of each string (so queries take
  `&str` without allocating), then verifies the hit against the stored key — an MPH returns a slot for
  *any* input, so verification is what turns it into a real membership test, and the stored keys give
  exact `id → key`. Two distinct keys colliding in the 64-bit hash cannot fail the build: the MPH is
  built over one representative per distinct hash value and the colliding leftovers are served — still
  exactly — from a tiny side table consulted only after the stored-key comparison has missed, so the
  hot path pays nothing. The expected number of colliding pairs is `n(n-1)/2^65` ≈ 2.7×10⁻⁸ at 1 M
  keys, 2.7×10⁻⁴ at 100 M — the table is almost always empty. The hash is **version-stable** (eight bytes at a
  time — one multiply-rotate round per word, then a splitmix64 finalizer — not `std`'s
  `DefaultHasher`), so a `save`d MPH reloads and queries
  identically on any build — the precondition for persistence. `CompactHashIndex` shares the
  same version-stable slot hash plus a second, uncorrelated one for the fingerprint, and resolves hash
  collisions the same way — its side table keeps the second hash at its **full 64 bits** whatever
  `fingerprint_bits` is set to, so only a pair colliding in **both** 64-bit hashes at once
  (`≈ 2^-128` per pair) would merge.
- **Zero-copy `load_mmap`** (the default `mmap` feature, `memmap2`) memory-maps a saved blob and
  borrows the index directly from the mapped pages — no read into RAM, so a multi-gigabyte index is
  ready instantly and the OS shares its pages across processes. `StringIndex` maps the whole FST;
  `CompactHashIndex` maps its fingerprint table; `PerfectHashIndex` maps the key arena (the bulk) and
  reads only the tiny MPH into memory. Every read is byte-wise, so there is no alignment gotcha. It is
  an **`unsafe fn`** — deliberately, since the mapped bytes are borrowed rather than copied, so a write
  to the file from *any* process while the index is alive is undefined behaviour and nothing in the
  library can check for it. lexindex blobs are written once and never updated in place, so publishing
  new versions under new paths discharges the obligation; the Python binding, which has no way to
  express it in the type system, states the same contract in its docstring. That is the *whole* of
  what `load_mmap` asks for: the bytes themselves are validated exactly as `from_bytes` validates
  them.
- **`from_bytes` and `load` are safe on every index, and that is why the perfect hash is in-crate.**
  Until 1.0 they were `unsafe fn` on both hash indexes: the embedded MPH was an `epserde` region
  whose pilot table `ptr_hash` read unchecked, and the fields that would have bounded that read were
  private to `ptr_hash`, so no amount of checking downstream could make a crafted blob safe. 1.0
  replaced that MPH with one whose every array length is written and checked by this crate, which
  turns a crafted blob from undefined behaviour into a wrong answer. The cost is that pre-1.0 blobs
  cannot be read at all — they are refused with a message naming the version that wrote them.
- **Blobs move forward, not backward — upgrade the reader first.** 1.1 reads every `BMP5` and
  every `BCH6` that 1.0 wrote, but what it writes is not readable by 1.0: `BMP6` is a magic 1.0 has
  never heard of, and a 1.1 `BCH6` carries the new `MPH2` perfect hash inside a container 1.0 does
  recognise, so 1.0 refuses both as malformed rather than as a version mismatch. `BIX4` and `OVL2`
  are byte-for-byte what 1.0 wrote, so a `StringIndex` or `Overlay` file crosses the two versions
  in either direction.
- `mph` is opt-in-by-default: with `--no-default-features` the crate depends only on `fst` (and keeps
  `StringIndex`). Enabling `mph` pulls **no dependency at all** — the perfect hash is in-crate — so
  the whole tree is `fst` plus `memmap2`, and `cargo audit` reports nothing on either build.

## Benchmarks

### Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|---:|
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | probabilistic | ✅ | **0.80** |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | probabilistic | ✅ | **1.30** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | probabilistic | ✅ | **2.30** |
| `marisa-trie` | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 |
| lexindex `PerfectHashIndex` | — | — | — | ✅ | ✅ | ✅ | 10.94 |
| DAWG (`dawg2`) | ✅ | — | — | — | ✅ | — | 23.96 |
| `datrie` | ✅ | — | — | — | ✅ | — | 30.91 |

<sub>Raw numbers and the machine that produced them:
[`bench/results/compare-2026-09-09-arz-2d23792.json`](bench/results/compare-2026-09-09-arz-2d23792.json)
— every cell's build samples, the false-positive measurement, the CPU, kernel, rustc, Python and the
load average at both ends of the run.</sub>

Two honest crowns, both scoped to what is measured above — libraries a Python or Rust project can
actually install. Research-grade C++ (CoCo-trie, XCDAT, PDT, SuRF) has no bindings to benchmark and
is not claimed against. **`CompactHashIndex` is the smallest `string → dense id` map here — 2.3×
below `marisa-trie` at the default 8-bit fingerprint, 3.7× at 4 bits** — when you can accept a bounded
false-positive rate (about `2^-fingerprint_bits` by design — the fingerprint comes from a second hash,
uncorrelated with the slot hash for well-distributed keys — measured **6.2530 %** at 4 bits and **1.5553 %** at 6 over 2 M non-member probes,
z = +0.18 / −0.83 against theory; **≈0.4 %** at 8 bits, **≈0.0015 %** at 16) and don't need
`id → key`. It is not a security primitive: both hashes are deterministic and unseeded, so an
adversary who chooses the queries can find false positives at will. It stays below
marisa's 2.98 B/key at every width up to 21 bits — the [width guide](docs/usage.md) tables the
trade-off. **`StringIndex` is the only
structure that answers fuzzy and range queries at all**, at 4× below a plain DAWG. `marisa-trie`
remains the pick when you need *exact* membership *and* ordering *and* the smallest such index —
lexindex doesn't claim that particular cell (see below for why).

### Against other Rust string indexes

`marisa-trie` is C++. Of the ordered Rust string indexes benchmarked here, **none is smaller than
`StringIndex`** — the double-array tries trade space for lookup speed, and no succinct LOUDS trie
(marisa / XCDAT / CoCo-trie-style) exists in Rust to depend on. So `StringIndex` at 5.95 B/key is the
**smallest of the pure-Rust ordered indexes measured below** — second only to a C++ library, and the
only one of them that does fuzzy and range. The comparison is against the four crates in the table,
not against all of crates.io, which no benchmark can settle. Same real words:

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

### Which one to pick, and how much the corpus decides it

Every size above is one corpus at one `n`, and the ranking is stable across neither. The reason is
structural: a trie's size depends on how much the keys share, and a fingerprint index's does not.
The same structures over three corpora built from the same word list, one process per cell
(`local/positioning.py`):

| bytes/key | 479 823 single words | 1 M `word.word` pairs, drawn at random | 1 M `word.word`, 1 000 × 1 000 grid |
|---|---:|---:|---:|
| the bare MPHF (no keys, no membership, no reverse) | 0.30 | 0.30 | 0.30 |
| **lexindex `CompactHashIndex`** (fp = 1 byte) | **1.30** | **1.30** | **1.30** |
| `marisa-trie` | 2.98 | 6.21 | 2.12 |
| **lexindex `StringIndex`** | 5.95 | 15.19 | 0.68 |
| lexindex `PerfectHashIndex` | 10.94 | 21.97 | 12.55 |

At 10 M the trie numbers move again — `marisa` 4.14 on random pairs against 2.36 on the grid,
`StringIndex` 12.44 against 2.00 — while `CompactHashIndex` stays at 1.30 and the bare MPHF at 0.30,
because their size is a function of `n` and the fingerprint width alone. The grid is a full cross
product and is the *most* favourable set a trie can be handed; it is what the scale table below uses,
and on it `StringIndex` at 0.68 B/key undercuts even a keyless perfect hash. Treat that as the
ceiling of what shared structure can buy, not as a headline.

So, in decision order:

- **Do the keys need to come back out, or be scanned in order?** If yes, the fingerprint indexes are
  out; `StringIndex` (ordered, prefix / range / fuzzy / subsequence) or `PerfectHashIndex` (exact
  membership, `id → key`, no ordering) are the candidates, and both pay for the keys they store.
- **Is a bounded false-positive rate acceptable?** If yes, `CompactHashIndex` is 2.3× smaller than
  `marisa-trie` on single words, 4.9× on random pairs and 3.3× at 10 M — and 1.7× *larger* than a bare
  MPHF, which is exactly the byte of fingerprint that buys the membership check.
- **Do the keys share a lot of structure** (a path namespace, a versioned catalogue, a cross product)?
  Then measure before choosing: that is the regime where an FST can beat a keyless hash outright.
- **A `dict` / `HashMap` is not in the table** because it has no serialised form to measure. It cost
  71–95 bytes per key above the key list itself across these corpora (58–60 at 10 M, where the table
  amortises better), and it has to be rebuilt from the keys on every process start; every structure
  here is mapped from a file instead.

### Lookup speed from Python, against `dict` and `marisa-trie`

`local/latency_py.py` — one process per corpus, every structure built up front, the seven lookup
forms rotated inside each round so none keeps the position that pays to warm the probe list.
**Measured on 1.1**, from the minimum over two agreeing 15-round passes per corpus. Ratios are
**quotients of the minima against `dict` on the same probe set**, and the gate is on *those*: worst
7.5 % / median 1.7 % (`words`), 4.7 % / 1.6 % (`random`), 6.8 % / 2.1 % (`grid`), against a bar of
10 %. It used to sit on the absolute minima and refused all three tables whose ratios agreed to
1–8 %, with `dict` the worst offender in every one — the minima carry the machine's thermal state,
which is exactly what the ratio divides out. `grid` needed a third pass: its second moved every
`marisa-trie` cell by 16 % while no other column moved 2 %, and passes 1 and 3 agree.

**Nothing about a lexindex *version* can be read out of this table**, and that is a measured claim,
not a disclaimer. Same session, alternating **A-B-B-A** (the order matters: A-B-A-B gives A both
early slots and this machine drifts), the released 1.0.0 wheel against this working tree — on
`random` every cell landed within 1.01–1.04×, on `words` within 0.98–1.09×, and the widest gaps in
both belong to `dict` and `marisa-trie`, code that is byte-identical in the two runs. Two readings
the cross-session numbers seemed to support die there: `PerfectHashIndex.id` did **not** gain 9 %
against `dict` between 1.0 and 1.1, and `CompactHashIndex.ids_of` did **not** lose 37 % on absent
pair-corpus probes — today's 1.0.0 wheel measures that cell at 62.0 ns against the 44.8 the 1.0
table published for it. The blocked arena's Rust-level win on `id` (216 → 182 ns) is real and
measured by `cargo run --release --example bench`; it does not survive the binding, because ~180 ns
of per-call overhead sits on top of every Python lookup.

| probe set | structure | 479 823 words | 1 M random pairs | 1 M grid pairs |
|---|---|---:|---:|---:|
| members | `dict` absolute, for scale | 267.8 ns | 270.6 ns | 227.3 ns |
| | `marisa-trie` | 2.16× | 4.20× | 2.79× |
| | `StringIndex.id` | 1.56× | 2.64× | 1.54× |
| | `PerfectHashIndex.id` | 1.00× | 1.20× | 1.19× |
| | **`CompactHashIndex.id`** | **0.70×** | **0.72×** | **0.61×** |
| | `PerfectHashIndex.ids_of` | 0.49× | 0.52× | 0.66× |
| | **`CompactHashIndex.ids_of`** | **0.34×** | **0.34×** | **0.47×** |
| absent | `dict` absolute, for scale | 180.0 ns | 242.6 ns | 252.0 ns |
| | `marisa-trie` | 2.72× | 4.38× | 2.35× |
| | `StringIndex.id` | 1.66× | 2.28× | 1.15× |
| | `PerfectHashIndex.id` | 0.70× | 0.79× | 0.81× |
| | **`CompactHashIndex.id`** | **0.39×** | **0.31×** | **0.29×** |
| | **`CompactHashIndex.ids_of`** | **0.31×** | **0.26×** | **0.24×** |

Below 1.00× is faster than `dict`. So: a `CompactHashIndex` answers a **present** key in 0.61–0.72×
the time of a `dict` and a **missing** one in well under half, batched `ids_of` in a quarter to just
under half — while occupying 1.30 bytes per key on disk against the `dict`'s 71–95 bytes per key in
RAM. `PerfectHashIndex` trades level with `dict` on single words, costs a fifth more on the pair
corpora, and wins on misses everywhere; `marisa-trie` costs 2.2–4.4× and `StringIndex` 1.2–2.6×, and
both swing with the corpus exactly as their sizes do.

<sub>Read the ratios, not the absolutes. This machine drifts 6–17 % over fifteen rounds **while
idle** — measured with a cache-resident integer loop that touches no memory — so absolute
nanoseconds here are a statement about one laptop's thermal envelope. (One `random` pass drifted
176 % when a neighbouring job woke up; its minima still agreed with the quiet pass to 3 %, which is
the case for taking the minimum rather than the mean.) The ratios divide that out *within* a session. Two caveats in `dict`'s favour,
both deliberate: CPython caches a string's hash inside the object, so a repeated probe over the same
`str` skips rehashing where lexindex hashes the bytes every call (~23 ns of the gap at 1 M); and
every column pays the same per-call binding overhead, which flatters the slower ones. **The
denominator's instability across sessions is a standing, unexplained property of this machine**, and
it has now been seen four times: `grid`'s `dict` read 251 ns, then 194–196 the next day, then 216,
and 227.3 here; `words`' read 327.9, then 258.5, then 267.8. Ruled out on the day it was first chased: the extension (the
released wheel measured the same as the working tree), the interpreter (one virtualenv throughout),
the corpus generator (unchanged) and background load (raising it did not restore the old figures).
The mechanism is still not identified. What follows for the protocol is that a *cross-session* delta
in this table means nothing on its own — not even as a shape across columns, which the A-B-B-A run
above tested directly and found empty. A version claim needs both builds in one session.</sub>

### Point-lookup latency vs the standard library

`cargo run --release --example bench` — 1 M **real dictionary-word bigrams** (`word_i.word_j`, the
same key generator as `bench/scale.py`; mean key 10.9 bytes). Keys are never synthetic
`entity-000…N` sequences — those arrive pre-sorted and hash-degenerate and flatter every number.
Measured on the 1.1 code (min of 12 runs, idle machine, four seconds between runs so clocks settle),
and **validated against a second independent session**: every lookup row's minimum agrees within
3.1 %, the `std::HashMap` control within 2.0 %. Absolute numbers are machine-dependent — that control
reads 3.4 % slower than in the 1.0 session that produced the previous table — so compare the
**ratios**, and only within a column.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~169 ms** | ~170 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~352 ms | **~78 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~194 ms | ~272 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~358 ms | ~274 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~258 ms | ~392 ns | *and* prefix / range / fuzzy |
| `std::BTreeMap<String, u32>` | ~211 ms | ~871 ns | in-RAM |

<sub>**This pair of sessions is the tightest yet, and it resolves one change cleanly.** Four rows
whose code did not move between 1.0 and 1.1 shifted their ratio to `HashMap` by at most 2 %:
`StringIndex` 1.472× → 1.443×, `BTreeMap` 3.259× → 3.207×, the FxHash map 0.593× → 0.584×, and
`id_unchecked` 0.285× → 0.288×. Against that bar **`PerfectHashIndex::id` went 1.103× → 1.010× of
`HashMap`**, 290 → 274 ns on a control that got *slower* — the blocked arena, and only it: `id`
reads the key out of the arena to verify it, `id_unchecked` never touches the arena at all, which is
exactly why one moved and the other did not. The one row that moved the wrong way is
`CompactHashIndex::id`, 0.597× → 0.627×, on code 1.1 did not touch and in the noisiest row of the
table (its run-to-run spread is 38–40 % against the control's 9 %); it is not resolvable here.
Builds still read 1.5–1.8× higher than the `ptr_hash` backend of 0.12, which is the price of a
header a loader can actually check and was accepted as such. Real keys move lookups in lexindex's
favour versus synthetic ones, while every `build` reads higher because real input is not pre-sorted
and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~159 ns** and `PerfectHashIndex::id_unchecked`
**~78 ns** — so on a closed vocabulary the perfect hash is about **2× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash. `CompactHashIndex::id` (~170 ns) is
within 7 % of the FxHash map and still carries the membership check and the 1.30 B/key blob.

**Two things the table above cannot show, both measured on 0.11 with an independent harness
(`local/latency/`, one process, all forms alternated per round, min of 12):**

- **Roughly 90 ns of every number in it is reaching the probe key, not looking it up.** The bench
  probes `keys[i * STEP % n]` — the original allocations in strided order, which is what a
  long-lived key list looks like. Hand the same index a probe list allocated in probe order and
  `PerfectHashIndex::id_unchecked` falls from 109 to **18 ns/op** at 1 M, `CompactHashIndex::id`
  from 126 to 33, while `PerfectHashIndex::id` barely moves (262 → 192; it fetches a stored key
  either way). The batched `ids_of` is layout-insensitive by construction — 39.5 scattered against
  38.2 contiguous — because its software prefetch does for scattered keys what the hardware does for
  contiguous ones. So read any sub-100 ns lookup figure, here or anywhere, as a statement about the
  caller's key layout as much as about the index.
- **On a miss-heavy workload `std::HashMap` wins, until its table outgrows the cache.** An absent key
  costs the SipHash map 32 ns at 1 M against `CompactHashIndex::id`'s 41 — it fails on an empty
  bucket after one cache line, while a fingerprint index runs the whole perfect hash and reads a
  fingerprint before it can say no. At 10 M the map's table no longer fits and the order reverses
  (105 ns against 56 for `ids_of`). lexindex's lookup advantage is on **members**, and at scale.

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest of the structures in the table above** — roughly twice as quick as the SipHash `HashMap`
(1.7–2.2× depending on the session; no probing, no membership comparison) *and* compact +
serialisable. `CompactHashIndex::id` keeps a probabilistic membership check and *still* beats that
`HashMap` on lookup (~1.6× here), and builds faster than it too. Full verification (`id`) pays one extra
cache line + a key comparison; `StringIndex` trades more latency for **ordered / prefix / range /
fuzzy** queries the hash maps cannot answer at all. So: `CompactHashIndex` when footprint dominates
and a rare false positive is fine; `PerfectHashIndex::id` for exact membership + reverse;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map
with nothing persisted.

### Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.30 bytes/key holds constant** as
`n` grows. Each row is measured twice: handing the constructor a **list** of keys, and handing it a
**generator**. The second is what `CompactHashIndex`'s streaming build exists for — it keeps a
16-byte pair per key and drops the string — and it is the only way to see the index's own footprint
rather than the corpus's:

| n | structure | keys | build | bytes/key | peak RSS | lookup |
|---|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | list | 0.39 s | 0.68\* | 154 MB | 205 ns |
| 1 M | `StringIndex` | generator | 0.55 s | 0.68\* | 147 MB | 214 ns |
| 1 M | `CompactHashIndex` | list | 0.22 s | 1.30 | 151 MB | 162 ns |
| 1 M | `CompactHashIndex` | **generator** | 0.34 s | 1.30 | **84 MB** | 151 ns |
| 10 M | `StringIndex` | list | 5.6 s | 2.00\* | 1108 MB | 746 ns |
| 10 M | `StringIndex` | generator | 7.7 s | 2.00\* | 1031 MB | 838 ns |
| 10 M | `CompactHashIndex` | list | 1.9 s | 1.30 | 985 MB | 256 ns |
| 10 M | `CompactHashIndex` | **generator** | 3.3 s | 1.30 | **297 MB** | 272 ns |

<sub>Measured on the 1.1 code
([`bench/results/scale-2026-09-09-arz-cca2d20.json`](bench/results/scale-2026-09-09-arz-cca2d20.json)),
one process per cell and the **minimum of five** per cell, on a machine idle at the start
(load 1.05). Unlike the 1.0 → 0.10 pair, this table *is* comparable cell by cell with the 1.0 one it
replaces: `StringIndex`, whose build and lookup code has not changed since 0.5.1 and is therefore
the control, holds within 2 % on both `list` rows. So the cells that moved are readable. At 1 M
`CompactHashIndex` reads 0.26 → 0.22 s and 223 → 162 ns on code 1.1 did not touch — the 1.0 table's
cells were single samples and these are minima of five, so that is old noise leaving, not a
speed-up. One cell moved the other way and is left standing rather than smoothed: 10 M
`StringIndex` from a **generator** went 6.5 → 7.7 s and 728 → 838 ns while the `list` row beside it
held, its five samples are tight (7.7–8.0 s), and the diff on that path between the two tags is
additive. It is unexplained.
\* bigram keys share far more prefixes than single words — at 1 M the generator draws on only
1 000 distinct words, which is why `StringIndex` compresses to an unrepresentative 0.68 B/key there;
the honest single-word figure is in the size table above. Read the
`peak RSS` and `bytes/key` columns, which are not clock-dependent, and read the times only against
each other. Peak RSS in the *list* rows is dominated by the Python key list; the *generator* rows are
the index's own cost, which is why `CompactHashIndex` falls 3.3× there and `StringIndex` barely
moves — it has to keep the keys. The extrapolation this table used to end on — ~35 s and ~3 GB for a
streamed `CompactHashIndex` at 100 M — has since been measured instead of left standing: **35.9 /
36.1 s at a 2 452 MB peak**, the same **1.30 B/key**, and a point lookup that does not move with `n`
(298–344 ns against 302–330 at 10 M). That is a separate and quieter session on 0.11 code, which is
why it is stated here rather than added as a row above. Hash collisions do not change the picture at any n: since 0.8 both perfect-hash
indexes absorb them into a side table instead of failing the build, and the fst build has no
collision failure mode at all.</sub>

## Security

Every loader is a safe fn on arbitrary bytes since 1.0, and `load_mmap` is the one that is not — its
obligation is about the file, not the bytes. What the blob formats do and do not defend against is
[`SECURITY.md`](SECURITY.md): a crafted blob answers wrong ids, never out-of-range ones; the
checksums are integrity and not authentication; and the hashes are unseeded, so this is not a HashDoS
defence.

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
builds in **61 ns/key on one thread** (0.61 s; 13 ns/key on eight) at **2.12 bits/key**, against
280 ns/key and 2.39 bits for 1.0 — its lookup unchanged at 5.4 ns/key. Those are this crate's
numbers on this machine from the spike in `src/mphf.rs`; nothing here is a claim about the
libraries above.

## License

MIT © Ilia Gradina
