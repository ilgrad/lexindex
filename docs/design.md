# Design

lexindex is three build-once / query-many indexes over a set of strings, each a flat, relocatable blob.

## Keys are bytes

**No Unicode normalisation, case folding, collation or grapheme segmentation.** Keys and queries
are compared as UTF-8 byte strings, and "character" means a Unicode scalar value: `é` and
`e\u{301}` are two different keys, an emoji ZWJ sequence is several characters to `fuzzy` and
`subsequence`, and ordering is byte order, not any locale's. Normalise (NFC/NFKC, casefold) before
building *and* before querying if the application needs it.

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

- **`key → id`.** The MPH (in-crate, `src/mphf.rs`) maps the key's
  version-stable 64-bit hash to a slot in `[0, n)`. That slot *is* the id — but an MPH returns a slot
  for any input, so a membership check is needed.
- **membership: a `b`-bit fingerprint.** Each slot stores a `fingerprint_bits`-wide fingerprint
  computed from a **second** hash of the key, with a different basis and multiplier. `id(key)` accepts
  the slot only if the query's fingerprint matches the stored one. The two hashes are uncorrelated for
  well-distributed keys, which makes the chance a non-member both lands on a used slot and matches its
  fingerprint about `2^-fingerprint_bits` — the tunable false-positive rate — ≈`2^-8k` by design:
  0.390 625 % at 1 byte, 0.001 526 % at 2.
  That is a *design* rate, not a guarantee against an adversary: both hashes are deterministic and
  unseeded, so anyone who can choose the queries can search for a false positive offline. Verified
  statistically on the 0.5.0 code, dictionary members with two non-member populations: 2 M random
  strings measured 0.384 % (z = −1.5 against theory) and 33/2 M at 2 bytes (z = +0.5); 50 000
  held-out *real words* measured 0.310 % (z = −2.9) — at or slightly below theory in every case,
  so the advertised rate is a ceiling in practice, not an average that can be exceeded. The
  sub-byte widths hold to theory the same way: on the 0.6.0 code, 2 M random non-member probes
  measured 6.253 % at 4 bits (z = +0.18 against 2⁻⁴) and 1.555 % at 6 bits (z = −0.83).

Because the keys themselves are never stored, size is just the MPH (0.26 B/key — 2.089 bits/key,
measured, and flat in `n`: `8/λ` bits of seed plus what the few percent of bumped keys cost)
plus the fingerprints, bit-packed at exactly `fingerprint_bits/8` B/key: **0.76 B/key at 4 bits
(6.25% false positives), 1.26 at the 8-bit default (0.39%), 2.26 at 16 (0.0015%)** on real words — below `marisa-trie`'s 2.98. The trade for that footprint is the false-positive rate and the absence of any
`id → key`. The serialised blob is `[magic "BCH7"][n][fp_bits][mph length][side_len]
[payload][check][MPH blob][bit-packed fingerprints][side]` (`ceil(m·b/8)` bytes, fingerprint
*i* at bits `[i·b, (i+1)·b)`, little-endian, where `m` = `n` minus the side-table entries), with a
32-bit check over the lexindex header — it frames every section, so it is
not taken on trust from a blob that lost bytes in transit — and a 64-bit streaming hash of the whole
payload, verified on owned loads. The build **streams**: one pass keeps a `(hash, second hash)` pair
— 16 bytes — per key and never the strings, so build memory does not grow with key length. The pairs
are the peak, all but the perfect hash's own construction — fed the representatives straight from
the sorted pairs, a chunk at a time, exactly as the file build feeds it from its merged file, so
the two give the same table — and one chunk buffer per thread, a few megabytes each, which is a
fixed cost. Measured on real-word bigrams (`cargo run --release --example peak -- compact-listed
10000000`), the high-water mark on top of whatever holds the keys is **21.5 bytes per key** at 10 M
with the 8-bit default; at 2 M, where the buffers still show, 25.7, and 25.4 at 16 bits, 29.9 at
32 — the width shows up because the fingerprint table is allocated before the pairs go.
(`peak.rs` resets `VmHWM` after the key list is built; without that reset the transient of loading
the corpus stays in the mark and the build appears 11 MB cheaper than it is, which is how the
0.10.0 figures were taken.)
Keys that collide in the 64-bit hash get tail ids in a side table (see
`PerfectHashIndex` below) holding the **full 64-bit second hash** — not the table's truncated width —
so the fingerprint setting never decides whether two colliding keys stay distinct, and the side probe
runs *before* the fingerprint table (a side key's truncated bits may tie its representative's). The
one silent case left is a pair colliding in *both* 64-bit hashes at once (`≈ 2^-128` per pair), which
is indistinguishable from a duplicate key by construction and collapses into one entry. That rate
holds only if no *structured* difference collides in both, and the hash through 1.1 did not manage
it: its per-word round, a 64-bit multiply and a rotate, kept a difference in the top bits of one
word confined to the top bits of the product and dropped it into byte 3 of the next word, where
that word's own difference XORed it away — in both hashes, whatever their constants. Keys differing
at bytes 8i+7 and 8i+11 alone (`d`↔`t` with `e`↔`o`; a case flip with `e`↔`i`) collided with
13–100 % probability and merged into one id. 2.0's round folds the full 128-bit product, whose high
half depends on every input bit through the carries, so no difference keeps a fixed shape: a
single-bit scan over every position pair of a 24-byte key finds no weak pair where the old round had
35 (`local/collide.rs`). Every blob written before 2.0 (`BCH1`–`BCH6`) is refused; see below. On
load, side-table ids are structurally required to be exactly the tail range `[m, n)` — the
checksums vouch for transport, not construction.

**Construction is deterministic, so a blob *is* a reproducible artefact.** Two `build` calls over
the same key set produce the same bytes, on any thread count and any machine: the seed sequence is
fixed and every part is placed independently of the others. Two nodes building the same corpus agree
on ids, and a blob can be checksummed against a rebuild. `build_to_file` writes the same bytes as `build` + `save`
for the same key set, and is tested that way: both derive the arena's encoding from the same key
lengths in the same slot order.

**The MPH's parameters are not exposed, and that is a measurement, not an omission.** `λ`, the
keys per bucket, sets `8/λ` bits of seed against the fraction of keys bumped to a further level,
and the measured surface at 10 M real-word bigram hashes is flat around the shipped 4.5: 2.154
bits/key at 4.15, 2.089 at 4.5, 2.070 at 4.7, within three nanoseconds per key of each other to
build. The table has no load factor at all — every level's range is exactly its key count, and the
slack that lets the last buckets place is the bumping. A knob whose settings differ by two percent
one way and nothing the other is not worth the API surface; `fingerprint_bits` is the knob that
*does* have a monotone trade, and it is public.

**Every read the MPH makes is bounded by a length in its own header.** That is the whole reason it is
in-crate. `index` touches a seed table per level, the tail's, and the remap — a rank bit vector
over the lower levels' values and the Elias–Fano hole list's three arrays — and each of those
lengths is *derived* on load from the seven scalars and the per-level rows of the `MPH2` header
rather than read beside them, so a loader that recomputes them cannot be handed a length that
disagrees with the table it describes. The remap is the one table whose *contents* can leave the
image, so it is the one checked by value: every rank sample must count what it claims, and every
hole must lie below `n`. What that buys is a `from_bytes` that is a safe fn on arbitrary bytes — a
crafted blob answers wrong ids, never out-of-range ones. The `MPH1` tables 1.0 wrote are read by
the same rule over their own eight scalars.

**Blobs from before 2.0 are refused, by name.** Every slot in a `BMP5`, `BMP6` or `BCH6` blob is
keyed on the 1.0 hash, a value this version does not compute — loaded under the new hash it would
answer wrong ids, silently — and every `BMP*`/`BCH*` format before 1.0 embedded a `ptr_hash` image
on top, from a crate no longer linked. So the refusal names the version that wrote the file and
says to rebuild, rather than reporting a bad magic on an intact one. There is no conversion path
for either index: `PerfectHashIndex`'s arena is readable but its slots came from the old hash, and
`CompactHashIndex` stores no keys at all. Rebuilding from the key list is the migration, and it is
the only one a keyless index could ever have had. Soundness outranks compatibility: 1.0 paid that
debt for the perfect hash, 2.0 for the key hash.

`build_to_file` is that streaming build carried past memory: the pairs go to runs of 256 MiB,
sorted and spilled beside the output, the runs are merged into one sorted file, and the perfect
hash is built from it one first-level chunk at a time — the level's chunks are pulled from a
feed, a slice in memory or a sequential reader cutting at bucket boundaries, so `build` runs
the same placement over its slice and the file is byte for byte what `build` and `save` write.
The fingerprints are then written at their slots, into memory while the table is under 256 MiB
and past that through range files of 16 M slots each, read back one at a time, so that no byte
of the output is written at a random offset — the lesson the perfect-hash arena taught. Measured
at 100 M real-word pairs: 302 MB peak against 8 834 MB for the list, the run buffer and the
perfect hash's construction plus the table within 5 MB of each other; at 10⁹, 0.94 GB, which is
the perfect hash's construction alone at 0.9 bytes per key — 0.6 of it the table and the keys its
first level bumped, since a level's pieces go into the table as they finish rather than being kept
for a merge, so what a first level holds beyond that is the chunks in flight.

## `ClosedHashIndex`

`CompactHashIndex` with the fingerprint table removed, and the `Option` with it. The minimal
perfect hash alone maps a key's 64-bit hash to a slot in `[0, n)`; nothing stored can say whether
the key was one of the build's, so `id` returns a `u32` and the contract is exactly what a perfect
hash offers: a member's id, and for any other string some id below `n`. It is a separate type
rather than `fingerprint_bits = 0` because a membership check that always says yes would be a
signature that lies, and because the hot path is then one call with no compare behind it — the
`id_unchecked` of the other two hash indexes as the only method. Size is the perfect hash and a
36-byte header: **0.26 B/key** on real words, a fifth of the smallest fingerprinted index and a
tenth of any trie, flat in `n`. The serialised blob is `[magic "BCL1"][n][mph length][side_len]
[payload][check][MPH blob][side]`, the `CompactHashIndex` layout without its fingerprint section,
under the same 64-bit hash collision rule: keys sharing a hash resolve through the side table on
their full second hash, exact for members. There is no `load_mmap`: the blob is the perfect hash,
which every loader reads into memory whichever way it is opened, so there is nothing to borrow.

## `DictIndex`

The sorted keys front-coded in blocks, so that an id is a rank and a rank is a place. A block of
`block` keys (32 by default, `1..=1024`) stores its first key whole and every other as the length
of the prefix it shares with its predecessor and the suffix after it — one header byte
`lcp << 4 | len` when both are below fifteen, else a marker and two varints — with the suffix
under a static symbol table in the manner of FSST (Boncz, Neumann and Leis, VLDB 2020): up to 255 symbols of one
to eight bytes, one-byte codes, an escape for what no symbol covers, trained in five rounds of
parse-and-count over a sample of the index's own suffixes and stored in the blob, about a
kilobyte. The codec is the crate's own — 300 lines, the reference's encoder shape, decode at
parity with `fsst-rs`, which would have raised the MSRV — its serialised table is its own as well,
so a `BDX1` neither reads nor writes a reference FSST table — and the training is deterministic, so a
blob is a function of its keys like every other. Beside the blocks sit three flat arrays with one
entry per block: where its head ends (`u32`), an eight-byte sample of the head in byte order
(`u64`), and where its entries start (`u64`, so the block data may pass 4 GiB).

`id` is a binary search over the samples — a flat array, eight bytes a block — then over the
heads of the few blocks whose sample equals the probe's, then one block scanned without decoding
anything: an entry's stored suffix is compared against the probe symbol by symbol, eight bytes at
a time, and the shared-prefix length alone decides most entries — shorter than what the probe has
matched so far means the entry is past the probe, longer means it is still below with nothing new
matched. `key(id)` is the head of block `id / block` and up to `block − 1` decodes, one eight-byte
store per code, into a string the caller can keep (`key_into`). On the dictionary at
`block = 32`: **3.52 B/key** (`StringIndex` 5.95), `id` 300 ns against 345, `key_into` 197
against `key`'s 505; 16 and 64 per block give 4.35 and 3.10 B/key at 283 and 345 ns. The serialised blob is
`[magic "BDX1"][n][block][head bytes][data bytes][table bytes][payload][check]`, then the table,
the heads, the three arrays and the data; the loader checks every length, both checksums, the
table and the arrays' order before anything is trusted, and the block data — bounded on every
read rather than validated up front — is what the fuzz target queries after loading. There is no
`load_mmap`, and there are no automata: a prefix or fuzzy question is `StringIndex`'s.

## `PerfectHashIndex`

A minimal perfect hash maps a *fixed* set of `n` distinct strings to distinct slots `[0, n)` with no
gaps and near-`O(1)` lookup in tiny space. lexindex builds the MPH itself (`src/mphf.rs`), keyed on
a **version-stable** 64-bit hash of each string (eight bytes at a time: one
multiply-rotate round per word, then a splitmix64 finalizer with the length folded in — not
`std`'s `DefaultHasher`, which is not guaranteed stable and so cannot back a *serialised* MPH). A flat `slot → key` arena doubles as the membership check: an
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
where membership is already guaranteed. The serialised blob is `[magic "BMP7"][n][mph length]
[side_len][payload][check][MPH blob][arena bytes][side]` — the payload hash covers everything after
the header and is verified on owned loads. The arena is `[n+1][tag][offsets][data]`, and the tag
names one of four encodings.

Addressing the keys was for a long time the largest part of this index. Before 0.5.0 every offset was
a `u64`: 8.0 of 17.6 bytes per key on the dictionary, to address a 4.9 MB arena. Choosing 4 or 8
bytes per arena took the whole structure to 13.62. Since 1.1 the offsets are **blocked**: 16 slots
share a `u32` base and carry one-byte *cumulative* offsets after it (tag `0x11`, 21 bytes per block),
so a key is `data[base + off[k] .. base + off[k + 1]]` — 1.31 bytes per key instead of 4, and the
whole index is **10.94 B/key**. A corpus whose 16-key runs do not fit in 255 bytes gets 256-slot
blocks with two-byte offsets (`0x12`, 2.02 B/key) — but a few long keys among short ones do not
decide the layout: since 2.0 a block whose keys outgrow its offsets keeps its place and stride,
stores all ones in its first offset (zero in every other block) with an index after it, and its
real offsets sit in an *overflow table* behind the data (tag bit `0x40`, 68 bytes per such block
plus an 8-byte trailer), so one 10 kB key among a million costs 76 bytes where it used to cost 0.7
bytes per key. Past 4 GiB of data the blocks keep their offsets and widen their bases to `u64`
(`0x31` / `0x32`, 1.56 / 2.04 B/key — the flat `u64` table that size took before 2.0 cost 8), and
only a handful of keys none of which any block holds keeps the flat table. The encoding is chosen
once at build time and recorded in the tag, so reading it is a branch that never changes for the
life of the index.

`build_with_fingerprints` sets bit `0x80` of the tag and stores one byte per slot behind the offsets —
a fingerprint from the second hash, the one `CompactHashIndex` keeps — so a probe whose fingerprint
does not match stops at the block and never reads the key. A lookup of an absent key is then one cache
miss instead of two: on the dictionary 166 → 74 ns, a member 163 → 171, the index 10.90 → 11.90 B/key,
the ids unchanged because the perfect hash is. The bytes follow the offsets rather than interleave with
them: an interleaved row pushed the offset pair up to 36 bytes from the base instead of 20, and the
extra line splits cost a member probe 6 ns against 3 for this layout. The blob's magic is 2.0's
`BMP7`; the tag alone would already stop a reader from before 2.0, as an unknown arena encoding.

`PerfectHashIndex` stores full keys (exact membership + `id → key`) where `CompactHashIndex` stores only
a fingerprint (probabilistic, no reverse); the two share the same version-stable slot hash, so choosing
between them is purely a size-vs-exactness trade, not a different lookup path.

**Ids are reproducible across builds.** The key hash is version-stable and construction is
deterministic, so the same key set gives the same ids and the same bytes. They remain *arbitrary* —
nothing about a key predicts its id, and any change to the key set reshuffles all of them — so an id
written down outside the index must still be paired with the blob that produced it, not with a
promise that a rebuild will agree. `StringIndex` ids are the sorted rank and survive an unchanged
key set trivially.

**Blob portability.** Every field of every blob is written little-endian and read byte-wise, so a
blob moves between machines of any endianness or pointer width — and since 1.0 that includes 32-bit
ones, `wasm32` among them. What made `mph` 64-bit-only was first `ptr_hash`'s `sucds`, which refuses
any other width, and then the MPH's own `u64 → usize` narrowings: the dependency left with the
backend, and every length a blob supplies is now converted with `try_from`, so a fabricated one fails
identically at either width instead of truncating at one of them. `StringIndex`'s blob is the `fst`,
whose encoding is little-endian by specification.

## `Overlay`

The indexes are build-once summaries: a transducer, front-coded blocks and a minimal perfect hash all
have to be rebuilt to admit one new key. An overlay keeps the base as it is, holds the keys added
after it in a map, and marks the ids retired from it in a bitset — so a catalog that mostly grows at
the edges is not rebuilt on every change. Removal is **by id, not by key**, which is what makes it
well defined over a probabilistic base: a tombstone is tested only after the base has already said
yes, so a `CompactHashIndex` false positive that lands on a tombstoned id reads as absent, which is
the contract that index already has.

The serialised blob is `[magic "OVL2"][base tag][base blob len][addition count][addition bytes]
[tombstone words][payload][check][base blob][additions][tombstone words]`. Every section length is in
the header, which is what lets the loader bound each region before reading a byte of it — and what
makes the payload hash checkable *before* any of the contents are trusted. Neither the live count nor
the live/dead split is stored; both are derived, so a blob cannot disagree with itself about how many
keys it holds. The base is embedded verbatim and validated by its own loader, so the overlay inherits
exactly its base's guarantees over that region and adds its own over the rest.

`OVL1`, which `0.12` wrote, is the one pre-1.0 format this version still reads. Nothing about it was
undecodable — it is the same three sections with the tombstone count in the body instead of the
header — so refusing it would have cost users a rebuild for nothing. What it cannot have is either
checksum: a flipped bit in an addition that stayed valid UTF-8 loaded as a different key, and one in
a tombstone word revived a removed id, silently. Saving a loaded `OVL1` writes `OVL2` and it gains
them; that is the whole migration.

## Zero-copy `load_mmap`

Every index but `ClosedHashIndex`, whose blob is the perfect hash and nothing to map, loads two ways. `load` reads the whole blob into memory; `load_mmap` memory-maps the
file and **borrows** the index from the mapped pages — no read, no copy — so load time is independent of
the index size and the OS shares the pages across processes. Two shades of the mapping exist for
files that were not carried by their author: `load_mmap_verified` is the same mapping with the
payload checksum `load` makes, one pass at load; `load_mmap_untrusted` (`StringIndex`) runs
`from_untrusted_bytes`'s validation over the mapping.

The mechanism is a single `SharedBytes` byte source: an owned `Arc<[u8]>` **or** an `Arc<memmap2::Mmap>`,
exposed as `AsRef<[u8]>` and `'static`. It backs the FST (`Map<SharedBytes>`), the fingerprint table and
the key arena, so `from_bytes` (owned) and `load_mmap` (mapped) share one code path with no
self-referential borrow and no `unsafe` beyond the single `Mmap::map`. Every field is read byte-wise
(`u64::from_le_bytes`, varints), so there is no alignment requirement — for `PerfectHashIndex` and
`CompactHashIndex`, `load_mmap` borrows the arena / fingerprint table (the bulk of the blob) zero-copy
and reads only the small MPH structure into memory. `DictIndex` is the same shape: the keys, the block
data and the two offset arrays are read where they lie, an array entry decoded where it is read, and
what the load reads is the header, the symbol table and the per-block samples — eight bytes a block,
one byte per four keys at the default block. The samples are read rather than borrowed because two
binary searches over them open every lookup, and the only form of that search that keeps its steps
out of the branch predictor is the standard library's, which selects with `hint::select_unpredictable`
over a `u64` slice; a section of a blob is not aligned, and searching the bytes measured 110 ns against
26 for the pair. In place of the walk over the arrays that `load` makes, every access bounds what they
say, so a crafted file answers wrong, never out of bounds.

The one caveat is the usual mmap contract: the mapped file must not be mutated while an index
borrows it. That obligation is the caller's, so the `load_mmap` family are **`unsafe fn`s** on every
index that has them — `memmap2::Mmap::map` is `unsafe` for precisely this reason, and wrapping it in a
safe function would hide a precondition that a perfectly ordinary safe program (another handle
writing to the same path) can violate.

The load-time trust boundary is worth stating precisely. Against **accidental** corruption — a
truncated download, a flipped byte, a lost header field — every owned `load`/`from_bytes` fails
cleanly: `StringIndex` verifies the FST's stored checksum and spot-checks that its values are the
sorted ranks (first value 0, rank-walk to `n - 1`), and the perfect-hash indexes and `DictIndex` verify a
streaming hash of their whole payload plus a check over the header's framing fields, so a corrupted
blob of any of them is rejected rather than read. Against a **deliberately crafted** blob every
checksum involved is public and deterministic, so an attacker can recompute them — and since 1.0 that
no longer matters for soundness. The perfect-hash indexes validate structure, not just transport:
every array length is derived from the header and checked against the bytes present, side-table ids
are structurally required to be exactly the tail range `[m, n)`, and the MPH's own remap is checked
by value so it cannot point outside `[0, n)`. A crafted blob therefore answers *wrong*, never out of
bounds. `StringIndex` sits differently and worse: the FST checksum catches accidental corruption, and
`fst` documents that even invalid input cannot violate memory safety — but a crafted, re-checksummed
FST can panic. That is measured, not assumed: a libFuzzer target over `from_bytes` produced such
bytes in minutes, and the 111-byte specimen it could not shrink further is committed under
`tests/data/` with a test that it still panics. The `from_bytes` docstring used to promise the
checksum ruled that out; it no longer does. `from_untrusted_bytes` is the answer for a blob from a
stranger: it checks the transducer as a graph, in two sweeps over the nodes reachable from the root
— each transition must point strictly below the node that holds it, which is how `fst` lays nodes
out and what makes the sweeps terminate on bytes that were not laid out that way; every accepted
path must spell valid UTF-8, tracked as the set of decoder states each node is reachable in; and
the outputs must be ranks by construction, a final node carrying none and each transition carrying
the count of keys its node spells before it, with the root's count equal to the footer's length —
all inside a `catch_unwind` that turns the decoder's panic into an `IndexError`. Nothing streams
the keys, so the cost is the graph's and not the language's: `fst` can spell a billion strings in
896 bytes. The catch is load-bearing, not a backstop: a node cannot be checked without decoding
it, and the specimen above is rejected by the catch, not by the address check. It costs
32× the owned load (22.9 ms against 0.7 ms on 479 823 words), which
is the right price once and the wrong price every time. `load_mmap`
skips the payload checksum scan by design, trusting the mapped file outright to keep mapping time
independent of blob size — the structural checks still run.

That is what decides the signatures. `from_bytes` and `load` are **safe fns on every index**,
because none of them is unsound on any input. `load_mmap` stays an `unsafe fn` everywhere, for the
mapping obligation alone: the index borrows the pages, so a concurrent write to the file is undefined
behaviour and nothing in the library can check for it. Until 1.0 the two perfect-hash loaders were
`unsafe` as well, and that was not a gap waiting for more validation — `ptr_hash` read its pilot
table unchecked and the fields that would have bounded that read were private, so a downstream crate
could not check them at any price. Upstream agreed: `epserde` 0.13 made `deserialize_full` an
`unsafe fn`, and PtrHash declined a checked `try_index()` for the same reason. The fix was not more
checking but a different MPH.

## Versioning

Semantic versioning, with one qualification that matters more here than the API does: **a blob format
is part of the contract**. A version that refuses a format an earlier one wrote is a major release,
even when every public signature is unchanged — which is exactly what 1.0 is. `cargo semver-checks`
runs in CI and passed clean across that release, because nothing in the API shrank; it catches the
half of compatibility that lives in signatures, and the CHANGELOG's "Changed — breaking" section
catches the half that lives in bytes.

The minimum supported Rust version is the `rust-version` field in `Cargo.toml`, currently **1.85**,
and a CI job derives its toolchain from that field so the two cannot drift. Raising it is a minor
release, not a patch.

## Security

What is validated on load and what is merely trusted is spelled out per format above; the threat
model that ties it together — soundness versus correctness, why the checksums are integrity and not
authentication, and where the crate's `unsafe` lives — is [`SECURITY.md`](../SECURITY.md).

## Blob compatibility

Every blob starts with a four-byte magic whose last character is the format version. A format change
bumps it, and the loader decides one of three things about the old one: read it, refuse it by name,
or — never — read it wrong.

| Magic | Written by | Structure | Older formats |
|---|---|---|---|
| `BIX4` | 1.0 | `StringIndex` | unchanged since 0.5; every published `BIX4` loads |
| `BMP7` | 2.0 | `PerfectHashIndex` | `BMP1`–`BMP6` **refused by name** |
| `BCH7` | 2.0 | `CompactHashIndex` | `BCH1`–`BCH6` **refused by name** |
| `BCL1` | 2.0 | `ClosedHashIndex` | new in 2.0 |
| `BDX1` | 2.0 | `DictIndex` | new in 2.0 |
| `OVL2` | 1.0 | `Overlay` | `OVL1` **read**; saving again writes `OVL2` |
| `MPH2` | 1.1 | the minimal perfect hash, inside `BMP7`, `BCH7` and `BCL1` | `MPH1` (1.0) **read** as a standalone blob |

**The policy is that a refusal must say which version wrote the file.** A blob refused on a bare "bad
magic" sends someone hunting for disk corruption when the file is intact and merely old, so both
hash-index loaders carry the list of magics they used to write and answer with a sentence naming
`lexindex < 2.0` and the fix. That is worth more than a conversion path would have been, because for
these two formats there is no conversion path to offer: every blob before 2.0 is keyed on a hash
this version does not compute (and the pre-1.0 ones embed a `ptr_hash` image it no longer links),
`PerfectHashIndex`'s arena survives but its slots came from that hash, and `CompactHashIndex`
stores no keys at all. **Rebuilding from the key list is the migration.**

`OVL1` is the exception that proves the rule: it is decodable — the same three sections with the
tombstone count in a different place — so it is read rather than refused. Compatibility is broken
where it cannot be kept, not where keeping it is merely inconvenient.

**Blobs move forward, not backward — upgrade the reader first.** 1.1 read every `BMP5` and
every `BCH6` that 1.0 wrote; 2.0 refuses all three by name, and what it writes — `BMP7`, `BCH7` —
is a magic neither has heard of, so they refuse it as malformed rather than as a version mismatch.
`BIX4` is byte-for-byte what 1.0 wrote, so a `StringIndex` file crosses the versions in either
direction, and so does an `OVL2` over a `StringIndex` base. An overlay embeds its base verbatim,
so an `OVL2` over a `BMP5`, `BMP6` or `BCH6` base is refused by 2.0 with that base, and the
migration is the base's: list its live keys on the old version, rebuild on 2.0, and put a fresh
overlay over it.

What a blob does *not* promise is that it will load into the same **ids** across a format change.
Since 1.0 construction is deterministic, so the same keys rebuilt on the same version give the same
blob byte for byte; across versions that changed the key hash or the MPH, they do not. An id written
down outside the index belongs with the blob that produced it.

## Cargo features

- `mph` (default) — `PerfectHashIndex`, `CompactHashIndex` and the in-crate MPH behind them. No
  dependency, any pointer width.
- `mmap` (default) — the zero-copy `load_mmap` path (pulls `memmap2`). The one feature with a target
  it cannot serve: there is nothing to map on `wasm32`.
- `python` — the PyO3 abi3 extension module.
- `--no-default-features` — an `fst`-only build: `StringIndex` with prefix/range/fuzzy/subsequence and
  owned `save`/`load`, depending on nothing but `fst`. The full default build depends on `fst` and
  `memmap2` and nothing else, and `cargo audit` reports nothing on either. CI cross-checks `i686`
  and `wasm32` on both.
