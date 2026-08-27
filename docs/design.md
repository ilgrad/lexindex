# Design

lexindex is three build-once / query-many indexes over a set of strings, each a flat, relocatable blob.

## `StringIndex`

Keys are sorted and deduplicated on build, and each key's id is its **rank in sorted order**, so ids
are stable for the same key set. It is backed by a single structure — a **finite-state transducer**
([`fst`](https://crates.io/crates/fst)) — that serves *both* directions:

- **`key → id`.** The FST stores the sorted keys as a minimised automaton, sharing common prefixes
  *and* suffixes, mapping each key to its rank (an output value). It drives prefix, range, fuzzy
  (Levenshtein) and subsequence iteration by walking the automaton — there is no separate
  materialised key list to scan. Prefix and range queries seek directly; a broad fuzzy or
  subsequence pattern can still visit most of the automaton's nodes, so those are linear in the
  index in the worst case, just with no second copy of the keys.
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
  tunable false-positive rate — `2^-8k` by construction: 0.390 625 % at 1 byte, 0.001 526 % at 2.
  That is a *design* rate, not a guarantee against an adversary: both hashes are deterministic and
  unseeded, so anyone who can choose the queries can search for a false positive offline. Verified
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
`id → key`. The serialised blob is `[magic "BCH5"][n][fp_bits][overflow_cap][mph length][side_len]
[payload][check][mph epserde bytes][bit-packed fingerprints][side]` (`ceil(m·b/8)` bytes, fingerprint
*i* at bits `[i·b, (i+1)·b)`, little-endian, where `m` = `n` minus the side-table entries), with a
32-bit check over the lexindex header — `overflow_cap` bounds an otherwise unchecked read, so it is
not taken on trust from a blob that lost bytes in transit — and a 64-bit streaming hash of the whole
payload, verified on owned loads. The build **streams**: one pass keeps a `(hash, second hash)` pair
— 16 bytes — per key and never the strings, so peak build memory is `16·n` bytes over the input
regardless of key length. Keys that collide in the 64-bit hash get tail ids in a side table (see
`PerfectHashIndex` below) holding the **full 64-bit second hash** — not the table's truncated width —
so the fingerprint setting never decides whether two colliding keys stay distinct, and the side probe
runs *before* the fingerprint table (a side key's truncated bits may tie its representative's). The
one silent case left is a pair colliding in *both* 64-bit hashes at once (`≈ 2^-128` per pair), which
is indistinguishable from a duplicate key by construction and collapses into one entry. 0.7 blobs
(`BCH3`) still load, as does a collision-free 0.8.0 `BCH4` (bit-identical to v5); a `BCH4` *with* a
side table stored truncated side fingerprints and is refused with a rebuild message. 0.5/0.6 blobs
(`BCH1`/`BCH2`) are refused; see below. On load, side-table ids are structurally required to be
exactly the tail range `[m, n)` — the checksums vouch for transport, not construction.

**The `overflow_cap` field guards ptr_hash's unchecked remap.** ptr_hash's minimal `index()` remaps
raw slots ≥ n through an internal Elias-Fano vector that only covers slots up to the last
member-occupied one, and reads it *unchecked* (`cacheline-ef`'s `index_unchecked`). A non-member
whose raw slot lands in the trailing free zone therefore indexes out of bounds — a debug assertion
at best, undefined behaviour in release. The cap recorded at build time is the remap's exact length
(the largest member `raw − n`, plus one, measured by streaming every member through
`index_no_remap`), and queries answer `None` outright for raw slots past it: those slots are
provably free, so no member can live there. Every query path is bounded by it, `id_unchecked`
included: that method skips the membership *comparison*, not the bounds.

Blobs written before the cap existed cannot be loaded as they are, since that would reinstate the
defect. `PerfectHashIndex` does not trust *any* stored cap — its arena holds every key, so the cap is
recomputed exactly on every load (O(m) hashes, paid once; the v4 format does not even carry the
field). `CompactHashIndex` stores no keys and has nothing to recompute from, so its checked header
cap is trusted and a `BCH1`/`BCH2` blob (which predates the field) is refused with a message naming
the fix. Soundness outranks compatibility with a two-day-old format.

## `PerfectHashIndex`

A minimal perfect hash maps a *fixed* set of `n` distinct strings to distinct slots `[0, n)` with no
gaps and near-`O(1)` lookup in tiny space. lexindex builds the MPH with
[`ptr_hash`](https://crates.io/crates/ptr_hash), keyed on a **version-stable** 64-bit hash of each
string (FNV-1a + a splitmix64 finalizer — not `std`'s `DefaultHasher`, which is not guaranteed stable
and so cannot back a *serialised* MPH). A flat `slot → key` arena doubles as the membership check: an
MPH returns a slot for *any* input, so a query is a hit only if the stored key at that slot equals the
query. Two distinct keys colliding in the 64-bit hash cannot fail the build. The hash is
deterministic and unseeded (that is what makes the serialised MPH reloadable), so a retry could never
help — instead the MPH is built over one representative per distinct hash value and the colliding
leftovers get tail ids `[m, n)` served from a **side table** (`(hash, id)` pairs, sorted), consulted
only after the stored-key comparison has already missed. Members are still answered exactly — the
side probe compares stored keys — and an index without collisions skips the probe with one
predictable branch, so the hot path pays nothing. The birthday bound `n(n-1)/2^65` says how often the
table is even non-empty (computed exactly, Maxima and PARI/GP agreeing): **6.2×10⁻⁹** for the
479 823-word dictionary, **2.7×10⁻⁸** at 1 M keys, **2.7×10⁻⁶** at 10 M, **2.7×10⁻⁴** (1 in ~3 700)
at 100 M, and **~2.7%** at 1 G — almost always empty, and no longer a failure mode at any scale.
`CompactHashIndex` resolves collisions the same way, with the fingerprint standing in for the stored
key in the side probe.

`id_unchecked` skips the stored-key comparison — the fastest possible lookup, for a closed vocabulary
where membership is already guaranteed. The serialised blob is `[magic "BMP4"][n][mph length]
[side_len][payload][check][mph epserde bytes][arena bytes][side]` — the payload hash covers
everything after the header and is verified on owned loads; `overflow_cap` is not stored at all,
because every load recomputes it from the arena (`BMP2`/`BMP3` blobs from 0.5–0.7 load the same way).
The arena is `[n+1][offset width][offsets][data]`. Offsets are
4 bytes unless the arena exceeds 4 GiB — at 8 bytes they were the single largest part of the index
(8.0 of 17.6 bytes per key on the dictionary, to address a 4.9 MB arena), so narrowing them cut the
whole structure to 13.60 B/key.

`PerfectHashIndex` stores full keys (exact membership + `id → key`) where `CompactHashIndex` stores only
a fingerprint (probabilistic, no reverse); the two share the same version-stable slot hash, so choosing
between them is purely a size-vs-exactness trade, not a different lookup path.

**Ids are not reproducible across builds.** The key *hash* is version-stable, but `ptr_hash`'s
construction is randomised, so building the same key set twice assigns different slots — measured on
50 000 keys, ~53 % kept their id. `save`/`load` of one built index is exact (the blob carries the
MPH itself), so an id written down anywhere outside the index must be paired with the blob that
produced it, never with the key list. `StringIndex` has no such caveat: its ids are the sorted rank.

**Blob portability.** The MPH region is `epserde`, which stores an in-memory layout: a blob moves
between machines of the same endianness and pointer width (every published wheel and CI target is
64-bit little-endian), not to a big-endian target. `StringIndex`'s blob is the `fst`, whose encoding
is little-endian by specification and byte-portable.

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
it.

The load-time trust boundary is worth stating precisely. Against **accidental** corruption — a
truncated download, a flipped byte, a lost header field — every owned `load`/`from_bytes` fails
cleanly: `StringIndex` verifies the FST's stored checksum, and the perfect-hash indexes verify a
streaming hash of their whole payload plus a check over the header's framing fields, so a corrupted
blob of any of the three is rejected rather than read. Against a **deliberately crafted** blob the
perfect-hash indexes remain *trust-your-own-blob*: every checksum involved is public and
deterministic (an attacker can recompute them), and the embedded minimal-perfect-hash is an `epserde`
region that `ptr_hash` reads **unchecked**, so a crafted payload can steer an out-of-bounds read no
checksum can catch. What narrows that: `overflow_cap` — the bound on the one otherwise-unchecked
remap read — is **recomputed from the arena on every `PerfectHashIndex` load**, never trusted from
any header, and side-table ids are structurally required to be exactly the tail range `[m, n)` on
every load, so no blob can hand `id()` a value at or past `len()`. `CompactHashIndex` stores no keys
to recompute from, so its cap is trusted from the checked header. `StringIndex` sits differently: the
FST checksum catches accidental corruption, and `fst` documents that even invalid input cannot
violate memory safety — a crafted, re-checksummed FST can at worst panic or answer wrongly, never
read out of bounds. `load_mmap` skips every checksum scan by design, trusting the mapped file
outright to keep mapping time independent of blob size.

## Cargo features

- `mph` (default) — `PerfectHashIndex` and `CompactHashIndex` (pulls `ptr_hash` + `epserde`).
- `mmap` (default) — the zero-copy `load_mmap` path (pulls `memmap2`).
- `python` — the PyO3 abi3 extension module.
- `--no-default-features` — an `fst`-only build: `StringIndex` with prefix/range/fuzzy/subsequence and
  owned `save`/`load`, depending on nothing but `fst`. It is also free of the informational RustSec
  advisories (unmaintained / unsound) that `ptr_hash`'s transitive dependency tree currently carries —
  `cargo audit` reports those as warnings, not vulnerabilities.
