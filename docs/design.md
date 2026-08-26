# Design

lexindex is three build-once / query-many indexes over a set of strings, each a flat, relocatable blob.

## `StringIndex`

Keys are sorted and deduplicated on build, and each key's id is its **rank in sorted order**, so ids
are stable for the same key set. It is backed by a single structure — a **finite-state transducer**
([`fst`](https://crates.io/crates/fst)) — that serves *both* directions:

- **`key → id`.** The FST stores the sorted keys as a minimised automaton, sharing common prefixes
  *and* suffixes, mapping each key to its rank (an output value). It drives prefix, range, fuzzy
  (Levenshtein) and subsequence iteration by walking the automaton — never a full scan.
- **`id → key`: a rank-walk, with no stored reverse map.** Because a key's id equals the sum of the
  output values along its accepting path, `key(id)` reconstructs the key directly from the FST: start at
  the root with an accumulator of 0, and at each node take the **last** transition whose
  `accumulator + transition output ≤ id` (transitions are ordered, so their output prefix-sums are
  monotone); append that transition's byte, add its output, and descend. When a final state is reached
  with `accumulator + final output == id`, the accumulated bytes are the key. This is `O(key length)`
  and needs no auxiliary structure.

Dropping the separate reverse dictionary (a front-coded map in 0.2.0) roughly halves the real-world
blob — `/usr/share/dict/words` goes from **12.6 to 5.95 bytes/key** — because that map only reached its
advertised size on structured keys that share long prefixes, not on a natural vocabulary. The
serialised blob is now simply `[magic "BIX4"][fst bytes]`.

## `CompactHashIndex`

The smallest `string → dense id` map, and smaller than any installable trie. It pairs a minimal
perfect hash with **one small fingerprint per key and no stored keys at all**:

- **`key → id`.** The MPH ([`ptr_hash`](https://crates.io/crates/ptr_hash)) maps the key's
  version-stable 64-bit hash to a slot in `[0, n)`. That slot *is* the id — but an MPH returns a slot
  for any input, so a membership check is needed.
- **membership: a `k`-byte fingerprint.** Each slot stores a `fingerprint_bytes`-wide fingerprint
  computed from a **second, independent** hash of the key. `id(key)` accepts the slot only if the
  query's fingerprint matches the stored one. Independence of the two hashes makes the chance a
  non-member both lands on a used slot and matches its fingerprint `256^-fingerprint_bytes` — the
  tunable false-positive rate — exactly `2^-8k`: 0.390 625 % at 1 byte, 0.001 526 % at 2. Verified
  statistically on the 0.5.0 code, dictionary members with two non-member populations: 2 M random
  strings measured 0.384 % (z = −1.5 against theory) and 33/2 M at 2 bytes (z = +0.5); 50 000
  held-out *real words* measured 0.310 % (z = −2.9) — at or slightly below theory in every case,
  so the advertised rate is a ceiling in practice, not an average that can be exceeded.

Because the keys themselves are never stored, size is just the MPH (~0.3 B/key — PtrHash is ≈2.4
bits/key) plus the fingerprints (`fingerprint_bytes` B/key): **1.30 B/key at 1 byte, 2.30 at 2** on real
words — below `marisa-trie`'s 2.98. The trade for that footprint is the false-positive rate and the absence of any
`id → key`. The serialised blob is `[magic "BCH1"][n][fp_bytes][mph length][mph epserde bytes][fingerprints]`.

## `PerfectHashIndex`

A minimal perfect hash maps a *fixed* set of `n` distinct strings to distinct slots `[0, n)` with no
gaps and near-`O(1)` lookup in tiny space. lexindex builds the MPH with
[`ptr_hash`](https://crates.io/crates/ptr_hash), keyed on a **version-stable** 64-bit hash of each
string (FNV-1a + a splitmix64 finalizer — not `std`'s `DefaultHasher`, which is not guaranteed stable
and so cannot back a *serialised* MPH). A flat `slot → key` arena doubles as the membership check: an
MPH returns a slot for *any* input, so a query is a hit only if the stored key at that slot equals the
query. Build fails, rather than silently corrupting, if two distinct keys collide in the 64-bit hash —
and because the hash is deterministic and unseeded (that is what makes the serialised MPH reloadable),
a colliding key set can *never* build: the fix is `StringIndex` or changing the keys, not retrying.
The birthday bound `n(n-1)/2^65` says how often that happens (computed exactly, Maxima and PARI/GP
agreeing): **6.2×10⁻⁹** for the 479 823-word dictionary, **2.7×10⁻⁸** at 1 M keys, **2.7×10⁻⁶** at
10 M, **2.7×10⁻⁴** (1 in ~3 700) at 100 M, and **~2.7%** at 1 G — so "astronomically rare" is honest
below ~10 M keys and a real design consideration at 10⁸–10⁹.

`id_unchecked` skips the stored-key comparison — the fastest possible lookup, for a closed vocabulary
where membership is already guaranteed. The serialised blob is `[magic "BMP2"][n][mph length][mph
epserde bytes][arena bytes]`, and the arena is `[n+1][offset width][offsets][data]`. Offsets are
4 bytes unless the arena exceeds 4 GiB — at 8 bytes they were the single largest part of the index
(8.0 of 17.6 bytes per key on the dictionary, to address a 4.9 MB arena), so narrowing them cut the
whole structure to 13.63 B/key.

`PerfectHashIndex` stores full keys (exact membership + `id → key`) where `CompactHashIndex` stores only
a fingerprint (probabilistic, no reverse); the two share the same version-stable slot hash, so choosing
between them is purely a size-vs-exactness trade, not a different lookup path.

## Zero-copy `load_mmap`

All three indexes load two ways. `load` reads the whole blob into memory; `load_mmap` memory-maps the
file and **borrows** the index from the mapped pages — no read, no copy — so load time is independent of
the index size and the OS shares the pages across processes.

The mechanism is a single `SharedBytes` byte source: an owned `Arc<[u8]>` **or** an `Arc<memmap2::Mmap>`,
exposed as `AsRef<[u8]>` and `'static`. It backs the FST (`Map<SharedBytes>`), the fingerprint table and
the key arena, so `from_bytes` (owned) and `load_mmap` (mapped) share one code path with no
self-referential borrow and no `unsafe` beyond the single `Mmap::map`. Every field is read byte-wise
(`u64::from_le_bytes`, varints), so there is no alignment requirement — for `PerfectHashIndex` and
`CompactHashIndex`, `load_mmap` borrows the arena / fingerprint table (the bulk of the blob) zero-copy
and reads only the small MPH structure into memory, sidestepping the deserialiser's alignment concern entirely.

The one caveat is the usual mmap contract: the mapped file must not be mutated while an index borrows
it. Parsing is fully bounds-checked, so loading a truncated or corrupt blob fails cleanly and never
reads out of bounds.

## Cargo features

- `mph` (default) — `PerfectHashIndex` and `CompactHashIndex` (pulls `ptr_hash` + `epserde`).
- `mmap` (default) — the zero-copy `load_mmap` path (pulls `memmap2`).
- `python` — the PyO3 abi3 extension module.
- `--no-default-features` — an `fst`-only build: `StringIndex` with prefix/range/fuzzy/subsequence and
  owned `save`/`load`, depending on nothing but `fst`. It is also free of the informational RustSec
  advisories (unmaintained / unsound) that `ptr_hash`'s transitive dependency tree currently carries —
  `cargo audit` reports those as warnings, not vulnerabilities.
