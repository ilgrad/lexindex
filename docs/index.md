# lexindex

**Compact, immutable string↔id indexes for huge catalogs** — a Rust core built on
[`fst`](https://crates.io/crates/fst) (finite-state transducer) and an in-crate minimal perfect hash,
with typed Python bindings and no Python runtime dependencies.

Build once over a set of strings (entity names, cluster labels, vocabulary terms, document keys),
then query many times: exact `string ↔ id` both ways, plus **prefix**, **range**, **fuzzy**
(Levenshtein) and **subsequence** iteration — all automaton-driven over the FST (exact, prefix and
range seek directly; a broad fuzzy or subsequence pattern may still traverse most of the automaton).
The blobs are tiny — on real dictionary words, **`CompactHashIndex` reaches 1.26 bytes/key, 2.4× below
`marisa-trie`**, and `StringIndex` 5.95 — and each can be **memory-mapped and borrowed zero-copy**, so
a multi-gigabyte index is ready instantly and its pages are shared across processes.

```bash
pip install lexindex
```

```python
import lexindex

idx = lexindex.StringIndex(["apple", "apricot", "banana"])
idx.id("banana")          # 2   — string → id (sorted rank)
idx.key(0)                # "apple"  — id → string
idx.prefix("ap")          # [("apple", 0), ("apricot", 1)]
idx.fuzzy("aple", 1)      # [("apple", 0)]  — typo-tolerant

idx.save("catalog.bix")
idx = lexindex.StringIndex.load_mmap("catalog.bix")   # zero-copy: no read into RAM
```

## Five indexes

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer. Exact `string ↔ id`
  plus prefix / range / fuzzy / subsequence iteration. The only one that answers ordered and
  typo-tolerant queries. Use it for autocomplete, fuzzy search, ordered browse.
- **`DictIndex`** — an **ordered** dictionary with the key stored for every id: exact `string ↔ rank`
  both ways, `lower_bound`, in-order iteration, no automata. The sorted keys front-coded in blocks
  with the suffixes under a static symbol table: 3.5 bytes/key, a third of `StringIndex`. Use it
  where the queries are exact and every id has to map back to its key.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map (a minimal perfect hash plus a
  fingerprint per key, no keys stored). 1.3 bytes/key, at the cost of probabilistic membership and no
  reverse lookup.
  Use it when a fixed vocabulary's footprint is paramount.
- **`ClosedHashIndex`** — the perfect hash **and nothing else**: `id(key) -> u32`, no `Option`, for
  a vocabulary known to be closed. A member's id, and for anything else some id in `[0, n)`.
  0.26 bytes/key, a fifth of `CompactHashIndex`. Use it as a token → id map where every query is a
  member by construction.
- **`PerfectHashIndex`** — a **minimal-perfect-hash** dictionary with verified membership and reverse
  lookup; exact `string → dense id`, and `id_unchecked` is the fastest lookup here for a vocabulary
  known to be closed. Use it as a fixed-vocabulary token↔id map on a hot path when you need exact
  membership and `id → key`. Built with `fingerprints=True`, one more byte per key lets a lookup of an
  absent key stop after one cache miss instead of two.

All five assign dense ids in `[0, n)` and serialise to a flat, relocatable blob
(`save` / `load`, and `load_mmap` where there is more than the perfect hash to map). None is mutable after building — they are immutable summaries, like
the clustering features in the companion [`betula-cluster`](https://github.com/ilgrad/betula-cluster)
crate.

## What's here

- **[Usage guide](usage.md)** — every interface with runnable Python and Rust snippets.
- **[Design](design.md)** — how the FST rank-walk, the fingerprint and minimal-perfect-hash
  dictionaries, and zero-copy memory-mapping work, and the serialised blob layout.
- **[API reference](api.md)** — the typed public surface.
- **[Changelog](changelog.md)**.
