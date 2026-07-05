# lexindex

**Compact, immutable string↔id indexes for huge catalogs** — a from-scratch Rust core (finite-state
transducer + minimal perfect hash) with typed Python bindings and zero runtime dependencies.

Build once over a set of strings (entity names, cluster labels, vocabulary terms, document keys),
then query many times: exact `string ↔ id` both ways, plus **prefix**, **range**, **fuzzy**
(Levenshtein) and **subsequence** iteration — all automaton-driven over the FST, never a full scan.
The blobs are tiny — on real dictionary words, **`CompactHashIndex` reaches 1.3 bytes/key, 2.3× below
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

## Three indexes

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer. Exact `string ↔ id`
  plus prefix / range / fuzzy / subsequence iteration. The only one that answers ordered and
  typo-tolerant queries. Use it for autocomplete, fuzzy search, ordered browse.
- **`CompactHashIndex`** — the **smallest** `string → dense id` map (`ptr_hash` + a fingerprint per
  key, no keys stored). ~1.3 bytes/key, at the cost of probabilistic membership and no reverse lookup.
  Use it when a fixed vocabulary's footprint is paramount.
- **`PerfectHashIndex`** — a **minimal-perfect-hash** dictionary with verified membership and reverse
  lookup; the fastest exact `string → dense id`. Use it as a fixed-vocabulary token↔id map on a hot
  path when you need exact membership and `id → key`.

All three assign dense ids in `[0, n)` and serialise to a flat, relocatable blob
(`save` / `load` / `load_mmap`). None is mutable after building — they are immutable summaries, like
the clustering features in the companion [`betula-cluster`](https://github.com/ilgrad/betula-cluster)
crate.

## What's here

- **[Usage guide](usage.md)** — every interface with runnable Python and Rust snippets.
- **[Design](design.md)** — how the FST rank-walk, the fingerprint and minimal-perfect-hash
  dictionaries, and zero-copy memory-mapping work, and the serialised blob layout.
- **[API reference](api.md)** — the typed public surface.
- **[Changelog](changelog.md)**.
