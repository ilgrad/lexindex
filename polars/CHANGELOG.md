# Changelog

`lexindex-polars` is versioned separately from `lexindex`: it tracks polars' plugin ABI, which
moves on polars' cadence and not on the library's.

## [0.1.0] — 2026-09-22

### Added

- **The first release: four expressions over a lexindex blob.** `.lexindex.id`,
  `.lexindex.id_unchecked`, `.lexindex.contains` on a string column and `.lexindex.key` on an id
  column, each naming an index file by path. They are Polars *plugin* expressions rather than a
  `map_batches` callback, so they run in the engine's threads, inside a lazy plan and under the
  streaming engine, without the GIL: the alternative — a Python function over
  `lexindex`'s existing `ids_of_arrow` — would have cost nothing in dependencies and given up all
  three.
- **One expression serves all six index kinds.** The blob says which kind it holds, so the plugin
  loads it and answers; what a kind cannot do — `key` on the two that store no keys, `contains` on
  a closed hash, `id_unchecked` on the two that answer by searching — is a `ComputeError` naming
  the kind and the file, checked once per chunk rather than per row.
- **The blob is read once per process**, into a cache keyed by path, size and modification time,
  and shared by every chunk and thread behind an `Arc`. A rebuilt file at the same path replaces
  its entry instead of joining it, so a build-query-rebuild loop does not accumulate generations.
  Nothing is memory-mapped: the crate writes no `unsafe`, and no file has to stay unchanged for a
  mapping's sake.
