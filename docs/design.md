# Design

lexindex is two build-once / query-many indexes over a set of strings, each a flat, relocatable blob.

## `StringIndex`

Keys are sorted and deduplicated on build, and each key's id is its **rank in sorted order**, so ids
are stable for the same key set. Two structures back it:

- **`key → id`: a finite-state transducer** ([`fst`](https://crates.io/crates/fst)). The FST stores the
  sorted keys as a minimised automaton, sharing common prefixes *and* suffixes, and drives prefix,
  range, fuzzy (Levenshtein) and subsequence iteration by walking the automaton — never a full scan.
- **`id → key`: a front-coded string dictionary.** Because the keys are sorted, adjacent keys share
  long prefixes (`"entity-000123"`, `"entity-000124"`), so the reverse map stores keys in fixed-size
  buckets: the first key of a bucket is verbatim, and each later key is a `(shared-prefix length,
  suffix)` delta against its predecessor, with **one pointer per bucket** instead of one 8-byte offset
  per key. On a structured sorted catalog that collapses the reverse map from ~27 to **~6 bytes/key —
  below the raw key bytes**. A random `key(id)` decodes up to a bucket of deltas and returns an owned
  `String`.

The serialised blob is `[magic "BIX2"][fst length][fst bytes][front-coded dict bytes]`.

## `PerfectHashIndex`

A minimal perfect hash maps a *fixed* set of `n` distinct strings to distinct slots `[0, n)` with no
gaps and near-`O(1)` lookup in tiny space. lexindex builds the MPH with
[`ptr_hash`](https://crates.io/crates/ptr_hash), keyed on a **version-stable** 64-bit hash of each
string (FNV-1a + a splitmix64 finalizer — not `std`'s `DefaultHasher`, which is not guaranteed stable
and so cannot back a *serialised* MPH). A flat `slot → key` arena doubles as the membership check: an
MPH returns a slot for *any* input, so a query is a hit only if the stored key at that slot equals the
query. Build fails, rather than silently corrupting, on the astronomically rare 64-bit hash collision
between two distinct keys.

`id_unchecked` skips the stored-key comparison — the fastest possible lookup, for a closed vocabulary
where membership is already guaranteed. The serialised blob is `[magic "BMP1"][n][mph length][mph
epserde bytes][arena bytes]`.

The MPH keeps the flat arena (not the front-coded dictionary) on purpose: its ids are unordered slots,
so there are no shared prefixes to exploit, and its `id()` hot path needs the arena's zero-copy `&str`
for the membership comparison.

## Zero-copy `load_mmap`

Both indexes load two ways. `load` reads the whole blob into memory; `load_mmap` memory-maps the file
and **borrows** the index from the mapped pages — no read, no copy — so load time is independent of the
index size and the OS shares the pages across processes.

The mechanism is a single `SharedBytes` byte source: an owned `Arc<[u8]>` **or** an `Arc<memmap2::Mmap>`,
exposed as `AsRef<[u8]>` and `'static`. It backs the FST (`Map<SharedBytes>`), the front-coded
dictionary and the arena, so `from_bytes` (owned) and `load_mmap` (mapped) share one code path with no
self-referential borrow and no `unsafe` beyond the single `Mmap::map`. Every field is read byte-wise
(`u64::from_le_bytes`, varints), so there is no alignment requirement — for `PerfectHashIndex`,
`load_mmap` borrows the key arena (the bulk of the blob) zero-copy and reads only the small MPH
structure into memory, sidestepping the deserialiser's alignment concern entirely.

The one caveat is the usual mmap contract: the mapped file must not be mutated while an index borrows
it. Parsing is fully bounds-checked, so loading a truncated or corrupt blob fails cleanly and never
reads out of bounds.

## Cargo features

- `mph` (default) — `PerfectHashIndex` (pulls `ptr_hash` + `epserde`).
- `mmap` (default) — the zero-copy `load_mmap` path (pulls `memmap2`).
- `python` — the PyO3 abi3 extension module.
- `--no-default-features` — an `fst`-only build: `StringIndex` with prefix/range/fuzzy/subsequence and
  owned `save`/`load`, depending on nothing but `fst`. It is also free of the informational RustSec
  advisories (unmaintained / unsound) that `ptr_hash`'s transitive dependency tree currently carries —
  `cargo audit` reports those as warnings, not vulnerabilities.
