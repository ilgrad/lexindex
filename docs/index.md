# lexindex

**Compact, immutable string↔id indexes for huge catalogs** — a from-scratch Rust core (finite-state
transducer + minimal perfect hash) with typed Python bindings and zero runtime dependencies.

Build once over a set of strings (entity names, cluster labels, vocabulary terms, document keys),
then query many times: exact `string ↔ id` both ways, plus **prefix**, **range**, **fuzzy**
(Levenshtein) and **subsequence** iteration — all automaton-driven over the FST, never a full scan.
The serialised blob is tiny (**~6 bytes/key** on a structured sorted catalog) and can be
**memory-mapped and borrowed zero-copy**, so a multi-gigabyte index is ready instantly and its pages
are shared across processes.

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

## Two indexes

- **`StringIndex`** — an **ordered** index backed by a finite-state transducer. Exact `string ↔ id`
  plus prefix / range / fuzzy / subsequence iteration. Use it for autocomplete, typo-tolerant search,
  ordered browse, or any catalog where order matters.
- **`PerfectHashIndex`** — a **minimal-perfect-hash** dictionary (`ptr_hash`). The fastest exact
  `string → dense id` with verified membership; no ordering. Use it as a fixed-vocabulary token↔id map
  on a hot path.

Both assign dense ids in `[0, n)`, support reverse lookup, and serialise to a flat, relocatable blob
(`save` / `load` / `load_mmap`). Neither is mutable after building — they are immutable summaries, like
the clustering features in the companion [`betula-cluster`](https://github.com/ilgrad/betula-cluster)
crate.

## What's here

- **[Usage guide](usage.md)** — every interface with runnable Python and Rust snippets.
- **[Design](design.md)** — how the FST, front-coded reverse map, minimal perfect hash and zero-copy
  memory-mapping work, and the serialised blob layout.
- **[API reference](api.md)** — the typed public surface.
- **[Changelog](changelog.md)**.
