# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **`StringIndex`'s reverse map (`id → key`) is now a front-coded string dictionary** instead of a flat
  arena of raw bytes + one 8-byte offset per key. Because ids are the sorted rank, keys are stored
  sorted and delta-encoded against their bucket predecessor (`(shared-prefix length, suffix)`, one
  pointer per 8-key bucket), so on a structured sorted catalog the serialised `StringIndex` blob shrinks
  from **~27 to ~6 bytes/key — below the raw key bytes**. `PerfectHashIndex` (unordered MPH slots, which
  cannot share prefixes) keeps the flat arena and is unchanged.
  - **Breaking:** `StringIndex::key(id)` now returns `Option<String>` (reconstructed on the fly) rather
    than `Option<&str>`; the Python `StringIndex.key` is unaffected (still returns `str | None`).
  - **Breaking:** the on-disk blob magic is now `BIX2`; `StringIndex` blobs written by 0.1.0 must be
    rebuilt (`PerfectHashIndex` blobs are unchanged).

## [0.1.0] — 2026-06-28

First public release — compact, immutable string<->id indexes for huge catalogs; a standalone Rust +
Python library that also pairs with `betula-cluster` (map string ids to cluster ids and back).

### Added

- **`StringIndex`** — ordered, FST-backed index: exact `string <-> id`, plus prefix, range, fuzzy
  (bounded Levenshtein edit distance), and subsequence iteration — all automaton-driven over the FST,
  never a full scan. Serialises to a flat, relocatable blob (`save` / `load` / `to_bytes` /
  `from_bytes`) with fully length- and offset-validated parsing (safe on untrusted input).
- **`PerfectHashIndex`** — minimal-perfect-hash dictionary (`ptr_hash`): verified-membership `id`,
  a faster `id_unchecked` for closed vocabularies (~1.25× faster than `std::HashMap` on point lookup),
  reverse lookup, and persistence (`save` / `load`) via `epserde`, keyed on a version-stable hash
  (FNV-1a + splitmix64) so a serialised MPH reloads and queries identically on any build.
- **Python bindings** (PyO3 abi3 extension, CPython 3.11+): `pip install lexindex`, zero runtime
  dependencies, typed (`py.typed` + stubs).
- **Feature gating** — `mph` (default) provides `PerfectHashIndex` (pulls `ptr_hash` + `epserde`);
  `--no-default-features` is an `fst`-only build, free of the informational RustSec advisories on the
  `ptr_hash` dependency tree. `fst`'s `levenshtein` is always on for fuzzy search.
- **Benchmark** — `cargo run --release --example bench` compares both indexes against
  `std::HashMap` / `BTreeMap` (build time, lookup latency, serialised size).
