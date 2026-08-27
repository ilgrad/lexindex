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
- **membership: a `b`-bit fingerprint.** Each slot stores a `fingerprint_bits`-wide fingerprint
  computed from a **second, independent** hash of the key. `id(key)` accepts the slot only if the
  query's fingerprint matches the stored one. Independence of the two hashes makes the chance a
  non-member both lands on a used slot and matches its fingerprint `2^-fingerprint_bits` — the
  tunable false-positive rate — exactly `2^-8k`: 0.390 625 % at 1 byte, 0.001 526 % at 2. Verified
  statistically on the 0.5.0 code, dictionary members with two non-member populations: 2 M random
  strings measured 0.384 % (z = −1.5 against theory) and 33/2 M at 2 bytes (z = +0.5); 50 000
  held-out *real words* measured 0.310 % (z = −2.9) — at or slightly below theory in every case,
  so the advertised rate is a ceiling in practice, not an average that can be exceeded. The
  sub-byte widths hold to theory the same way: on the 0.6.0 code, 2 M random non-member probes
  measured 6.253 % at 4 bits (z = +0.18 against 2⁻⁴) and 1.555 % at 6 bits (z = −0.83).

Because the keys themselves are never stored, size is just the MPH (~0.27 B/key — PtrHash is ≈2.2
bits/key with its compact λ=3.9 parameters; the build falls back to the default λ=3.5 ≈2.4 on the
rare compact-construction failure, and both serialise identically) plus the fingerprints, bit-packed at exactly `fingerprint_bits/8` B/key: **0.77 B/key at 4 bits
(6.25% false positives), 1.27 at the 8-bit default (0.39%), 2.27 at 16 (0.0015%)** on real words — below `marisa-trie`'s 2.98. The trade for that footprint is the false-positive rate and the absence of any
`id → key`. The serialised blob is `[magic "BCH3"][n][fp_bits][overflow_cap][mph length][mph
epserde bytes][bit-packed fingerprints]` (`ceil(n·b/8)` bytes, fingerprint *i* at bits
`[i·b, (i+1)·b)`, little-endian). A 0.5.x `BCH1` blob still loads — including zero-copy under
mmap — because its byte-aligned `k`-byte fingerprints are bit-identical to the packed layout at
`8k` bits, and a 0.6.0 `BCH2` blob is `BCH3` minus the cap field; new blobs are always `BCH3`.

**The `overflow_cap` field guards ptr_hash's unchecked remap.** ptr_hash's minimal `index()` remaps
raw slots ≥ n through an internal Elias-Fano vector that only covers slots up to the last
member-occupied one, and reads it *unchecked* (`cacheline-ef`'s `index_unchecked`). A non-member
whose raw slot lands in the trailing free zone therefore indexes out of bounds — a debug assertion
at best, undefined behaviour in release. The cap recorded at build time is the remap's exact length
(the largest member `raw − n`, plus one, measured by streaming every member through
`index_no_remap`), and queries answer `None` outright for raw slots past it: those slots are
provably free, so no member can live there. Blobs from versions that did not record the cap
(`BCH1`/`BCH2`/`BMP2`) load with the cap "unbounded" — exactly their original behaviour; rebuilding
is what closes the window for them.

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
where membership is already guaranteed. The serialised blob is `[magic "BMP3"][n][overflow_cap][mph
length][mph epserde bytes][arena bytes]` (`BMP2` blobs from 0.5/0.6 still load; the cap plays the
same role as in `CompactHashIndex` above), and the arena is `[n+1][offset width][offsets][data]`. Offsets are
4 bytes unless the arena exceeds 4 GiB — at 8 bytes they were the single largest part of the index
(8.0 of 17.6 bytes per key on the dictionary, to address a 4.9 MB arena), so narrowing them cut the
whole structure to 13.60 B/key.

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
