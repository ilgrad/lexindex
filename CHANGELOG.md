# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] — 2026-08-26

### Added

- **Bounded and lazy queries on `StringIndex`.** Python `prefix` / `range` / `fuzzy` /
  `subsequence` take a `limit`, and Rust gains `prefix_iter` / `range_iter` / `fuzzy_iter` /
  `subsequence_iter` returning lazy iterators. An autocomplete asking for ten matches now walks ten
  keys instead of materialising every match: on the 479 823-word dictionary,
  `prefix("s", limit=10)` is **0.026 ms against 17.59 ms — 669× faster** — and allocates 10 tuples
  rather than 45 064. `prefix("a", limit=10)` is 310×, `fuzzy("hello", 2, limit=5)` 3.7×.
  - The eager forms are now `.collect()` over the lazy ones, so there is one walk implementation
    rather than two. Measured with both variants compiled into one binary and alternated A-B-A-B
    (the machine was loaded, and in-process alternation is what makes the comparison meaningful):
    the change is **not** a regression — five runs gave −7.4 %, −0.3 %, −2.7 %, −1.6 %, −1.9 %.
  - `fuzzy_iter` still builds its automaton eagerly, so a too-large edit distance errors up front
    rather than on first use.

- **`lexindex.__version__`** in the Python package, read from the installed distribution
  metadata (so it cannot drift from `pyproject.toml`) with a `0.0.0+unknown` fallback when
  imported from a source tree that was never installed.

### Changed

- **The Python bindings borrow the caller's strings instead of copying them.** The constructors and
  `ids_of` read their keys as `PyBackedStr` — a view into the Python `str` — where they previously
  extracted an owned `Vec<String>`. `build` already copies the keys it keeps, so that intermediate
  vector was pure overhead: on the 479 823-word dictionary the **peak RSS of a build drops from
  44.6 MB to 29.9 MB (−33%)**, and the build itself is **1.08×** faster, `ids_of` **1.05×**
  (both implementations compiled into one extension and alternated A-B-A-B; four independent
  process pairs for the memory figure, which agreed to within 0.3 MB).
  - **No API change.** `PyBackedStr` accepts exactly what `String` did; the observable contract —
    accepted types, rejected types and every error message — was diffed against a build of the
    previous code and is identical. Serialised sizes are unchanged (`CompactHashIndex` 1.301 /
    2.301, `StringIndex` 5.953, `PerfectHashIndex` 17.625 bytes/key).

- **`PerfectHashIndex.key` / `keys_of` no longer copy each key twice.** Its keys live in an arena,
  so `key` returns a `&str`; both methods then copied that into a `String` only for PyO3 to copy it
  again into a Python `str` and drop it. They now build the Python string straight from the arena
  slice. `keys_of` is **1.29×** faster (250 -> 194 ns/key on the 479 823-word dictionary, A-B-A-B
  in one extension) and allocates nothing per key; the single-key `key` is 1.03×, the rest of its
  cost being the Python call itself. `keys_of` still runs its lookups under `Python::detach` — only
  the string construction, which needs the GIL either way, happens with it held.

- **The Python bindings release the GIL** (`Python::detach`) around building, bulk queries
  (`prefix` / `range` / `fuzzy` / `subsequence`), batch lookups (`ids_of` / `keys_of`) and
  persistence (`save` / `load` / `load_mmap` / `to_bytes` / `from_bytes`), so a threaded caller
  keeps making progress instead of freezing the interpreter. Previously a background thread got
  **1 scheduler tick during a 268 ms build** of the 479 823-word dictionary; it now runs
  throughout.
  - Single-key accessors (`id`, `key`, `contains`, `id_unchecked`, `successor`, `predecessor`,
    `__len__`) deliberately **keep** the GIL: they take well under a microsecond, so releasing and
    reacquiring it would cost more than the work it protects. Their code is untouched, and the
    serialised size of every index is unchanged (`CompactHashIndex` 1.30 B/key, `StringIndex`
    5.95, `PerfectHashIndex` 17.62 — bit for bit, since no core or serialisation code was
    modified).

- **The minimum supported Rust version is now 1.85**, declared as `rust-version` in `Cargo.toml`
  and enforced by a CI job that derives its toolchain from that field, so the declaration cannot
  drift from what is actually built. The crate moved to **edition 2024**, whose floor is exactly
  1.85; `cargo fix --edition` required no source changes in any feature configuration, so the
  only user-visible effect is the toolchain requirement itself.
  - **Rust consumers on a toolchain older than 1.85 must upgrade** — `cargo` will refuse to build
    lexindex rather than fail obscurely. This is why the release is 0.5.0 and not 0.4.1.
  - **Python users are unaffected.** The published wheels are abi3 and carry no toolchain
    requirement; `requires-python` is unchanged at `>=3.11`.

## [0.4.0] — 2026-07-06

### Added

- **Ordered navigation on `StringIndex`** — `successor(query)` (smallest key `>=` query) and
  `predecessor(query)` (largest key `<=` query), each `O(query length)` by seeking the FST (no scan),
  plus **lazy iteration**: `for key, id in index` in Rust `StringIndex::iter()` decodes one key per step
  by the rank-walk, so it never materialises the whole key set the way `prefix("")` would.
- **Batched lookups** — `ids_of(keys)` and `keys_of(ids)` on `StringIndex` and `PerfectHashIndex`, plus
  `ids_of(keys)` on `CompactHashIndex`. Each loops in Rust and crosses the Python↔Rust boundary once
  instead of per key, so a bulk `string → id` / `id → string` mapping avoids the per-call FFI overhead.
  Returns a list aligned with the input, `None` where a key/id is absent. Named `ids_of`/`keys_of` (not
  `keys`) so a class is never mistaken for a mapping — `dict(index)` builds `{key: id}` from the
  iterator instead.
- **`musllinux_1_2` wheels** (x86_64 + aarch64) for Alpine / musl-based containers, alongside the
  existing manylinux, macOS, and Windows wheels.
- **Scale benchmark** (`bench/scale.py`) measuring build time, peak memory, and lookup latency from 1M
  to 100M real keys.

### Fixed

- `CompactHashIndex::from_bytes` guards the fingerprint-table length check with a checked multiply, so a
  corrupt blob with a fabricated huge `n` fails cleanly instead of overflowing `usize` (a debug-build
  panic; release builds already wrapped to a clean error). Documented the trust boundary shared by both
  minimal-perfect-hash blobs: `from_bytes` / `load` validate the lexindex framing but deserialise the
  embedded MPH via `epserde`, which does not bound-check a corrupted MPH region — feed only blobs you
  produced (the same contract as `load_mmap`). `StringIndex` blobs are fully validated and unaffected.

### Testing

- **Property-based tests** (`proptest`, dev-dependency only): the rank-walk `id ↔ key` round-trip over
  random prefix-nested and multibyte key sets; the `PerfectHashIndex` bijection onto `[0, n)`;
  `CompactHashIndex` never false-negatives a member; and every `from_bytes` deserialiser rejects
  arbitrary or (for lexindex-owned bytes) single-byte-flipped input cleanly — never panics or reads out
  of bounds. Line coverage rose to 97.0%.

## [0.3.0] — 2026-07-05

### Added

- **`CompactHashIndex` — the smallest `string → dense id` map, and smaller than any installable
  alternative.** A minimal perfect hash ([`ptr_hash`](https://crates.io/crates/ptr_hash)) plus a
  `k`-byte fingerprint per key, storing **no keys at all**. On the real `/usr/share/dict/words`
  (479 823 words) it serialises to **1.30 bytes/key** at `fingerprint_bytes=1` and **2.30** at `2` —
  **2.3× smaller than `marisa-trie` (2.98)** and far below every trie benchmarked. The trade-offs are
  explicit: membership is **probabilistic** (a non-member reads as present with probability
  `256^-fingerprint_bytes` — measured 0.36 % at 1 byte, 0.001 % at 2) and there is **no reverse
  `id → key`** (the keys are not stored). Reach for it when a fixed vocabulary's footprint is paramount
  and rare false positives are acceptable; use `PerfectHashIndex` for exact membership + reverse, or
  `StringIndex` for ordered/fuzzy queries. Exposed to Python as
  `CompactHashIndex(items, fingerprint_bytes=1)` with `id` / `id_unchecked` / `contains` / `to_bytes` /
  `from_bytes` / `save` / `load` / `load_mmap`; in Rust behind the default `mph` feature.

### Changed

- **`StringIndex` dropped its stored reverse map — `id → key` is now reconstructed from the FST by a
  rank-walk.** Each id is the key's rank, i.e. the FST's output, so `key(id)` walks the automaton from
  the root, at each node taking the last transition whose accumulated output stays `≤ id`, and returns
  the path once the outputs sum to exactly `id` (`O(key length)`, no auxiliary structure). This
  **deletes the front-coded reverse dictionary** added in 0.2.0: the serialised blob is now just
  `[magic][fst]`. The effect on real-world size is large — on `/usr/share/dict/words` the `StringIndex`
  blob shrinks from **12.61 to 5.95 bytes/key (−53 %)**, because 0.2.0's front-coded map only reached
  its advertised "~6 B/key" on *structured* keys that share long prefixes, not on a natural vocabulary.
  Full prefix / range / fuzzy / subsequence are retained.
  - **Breaking:** the on-disk blob magic is now `BIX4`; `StringIndex` blobs written by 0.1.x / 0.2.0
    must be rebuilt. `PerfectHashIndex` blobs are unchanged.
- **Benchmarks are now measured on real English words**, not a synthetic `entity-{i}` catalog.
  Sequential structured keys collapse the FST to a near-regular automaton and report a misleading ~0
  bytes/key; `bench/compare.py` refuses synthetic keys and compares `size` and `build` against
  `marisa-trie`, DAWG and datrie on `/usr/share/dict/words`.

## [0.2.0] — 2026-07-05

### Added

- **Zero-copy `load_mmap`** on `StringIndex` and `PerfectHashIndex` (new default `mmap` feature, backed
  by `memmap2`): memory-map a saved blob and borrow the index from the mapped pages instead of reading
  it into RAM, so a multi-gigabyte index loads instantly and its pages are shared across processes.
  `StringIndex` maps the whole blob (FST + front-coded dictionary); `PerfectHashIndex` maps the key
  arena (the bulk) and reads only the small MPH into memory. Exposed to Python as
  `StringIndex.load_mmap` / `PerfectHashIndex.load_mmap`. Reads are byte-wise (no alignment
  requirement); the mapped file must stay immutable while an index borrows it. `--no-default-features`
  (the `fst`-only build) omits it.
- **MkDocs documentation site** at <https://ilgrad.github.io/lexindex/> (Material + mkdocstrings API
  reference), and a `mmap_zero_copy` example that times the owned `load` against the zero-copy
  `load_mmap`.
- CI now enforces a **95% line-coverage floor** (`cargo llvm-cov`) on the Rust core.

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
