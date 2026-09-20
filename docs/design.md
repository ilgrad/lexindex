# Design

lexindex is five build-once / query-many indexes over a set of strings, each a flat, relocatable blob.

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

The smallest `string → dense id` map that can reject a non-member, and smaller than any installable
trie. (`ClosedHashIndex` is a fifth of it at 0.24 bytes a key, and answers for a stranger as though
it were a member — that is the whole difference.) It pairs a minimal perfect hash with **one small
fingerprint per key and no stored keys at all**:

- **`key → id`.** The MPH (in-crate, `src/mphf.rs`) maps the key's
  version-stable 64-bit hash to a slot in `[0, n)`. That slot *is* the id — but an MPH returns a slot
  for any input, so a membership check is needed.
- **membership: a `b`-bit fingerprint.** Each slot stores a `fingerprint_bits`-wide fingerprint
  computed from a **second** 64-bit hash of the key — the same two lane products folded under other
  constants, one multiply over the slot hash. `id(key)` accepts the slot only if the query's
  fingerprint matches the stored one. The two hashes are uncorrelated for well-distributed keys, which makes the chance a non-member both lands on a used slot and matches its
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

Because the keys themselves are never stored, size is just the MPH (0.24 B/key — 1.918 bits/key at
10 M, measured, and flat in `n`: `8/λ` bits of seed plus what the 1.3 % of bumped keys cost)
plus the fingerprints, bit-packed at exactly `fingerprint_bits/8` B/key: **0.74 B/key at 4 bits
(6.25% false positives), 1.24 at the 8-bit default (0.39%), 2.24 at 16 (0.0015%)** on real words — below `marisa-trie`'s 2.98. The trade for that footprint is the false-positive rate and the absence of any
`id → key`. The serialised blob is `[magic "BCH8"][n][fp_bits][mph length][side_len]
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
and the measured surface at 10 M real-word bigram hashes is flat around the shipped 4.5: 1.925
bits/key at 4.4, 1.917 at 4.5, 1.914 at 4.6, within a few nanoseconds per key of each other to
build, and the bumped share — the lookup's cost — rises through it, 1.0 % to 1.7 %. The table
has no load factor at all — every level's range is exactly its key count, and the
slack that lets the last buckets place is the bumping. A knob whose settings differ by two percent
one way and nothing the other is not worth the API surface; `fingerprint_bits` is the knob that
*does* have a monotone trade, and it is public.

**Every read the MPH makes is bounded by a length in its own header.** That is the whole reason it is
in-crate. `index` touches a seed table per level, the tail's, and the remap — an Elias–Fano stream of
the lower levels' values' high parts with a sample per 64 of them, and a packed array of their
low bits — and each of those lengths is *derived* on load from the seven scalars and the per-level rows of the `MPH3` header
(and of `MPH2`'s, the same header under the seed geometry 1.1 to 3.0 wrote) rather than read beside them, so a loader that recomputes them cannot be handed a length that
disagrees with the table it describes. The remap is the one table whose *contents* can leave the
image, so it is the one checked by value: the stream must hold one set bit per value, each sample
must sit on its block's first one, and every value it decodes must lie below `n`. What that buys is a `from_bytes` that is a safe fn on arbitrary bytes — a
crafted blob answers wrong ids, never out-of-range ones. The `MPH1` tables 1.0 wrote are read by
the same rule over their own eight scalars.

**Its tables ask for huge pages.** A first level of 9 M keys or more is a seed table past 2 MiB,
and read at random on 4 KiB pages it misses the TLB on most lookups. Every table of the MPH of a
huge page or more is allocated on a 2 MiB boundary and advised `MADV_HUGEPAGE` before its first
write, so a Linux kernel with transparent huge pages at `madvise` — the common default — or
`always` backs it with 2 MiB pages from the first touch. The advice is best effort: where it is
refused the table sits on small pages and reads the same, a table below 2 MiB is an ordinary
allocation, and the file-backed tables of `load_mmap` are outside its reach. The byte arrays an
index owns around it — a `PerfectHashIndex`'s arena, a `CompactHashIndex`'s fingerprints, a
`DictIndex`'s blocks — are allocated the same way, built or loaded from bytes: at 10 M keys a
`PerfectHashIndex::id` reads its arena 8–11 % faster for it, and a `key` 10–16 %.

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

**Partitioning the table was measured and not done.** Cutting the keys by their top bits into 32–128
parts, each its own `MPH2` over a remixed hash with a prefix offset per part, would bound the build's
peak memory by the part rather than by `n` — the one thing the monolith cannot offer past 10⁹ keys.
Measured on the 10 M bigram hashes (`mphf::spike::partitioned` under `bench-mphf`,
[`bench/results/mphf-partition-2026-09-13-arz-2a19584.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-partition-2026-09-13-arz-2a19584.txt)):
+1.0–1.6 % size, one-thread build +12–37 %, eight-thread build −8 % at 32 parts, and lookup
**25 → 39 ns**, because a monolith's pointer chain stays in L1 while a part's differs per query. A
flat layout — one seeds array and one slot space with per-part offsets — would win most of the
lookup back at the price of a new format for all three hash indexes, and the memory it saves is not
the binding constraint at the scale that would need it: at 10⁹ keys the MPHF's construction peak is
0.94 GB and the sort scratch 44 GB of disk.

## `ClosedHashIndex`

`CompactHashIndex` with the fingerprint table removed, and the `Option` with it. The minimal
perfect hash alone maps a key's 64-bit hash to a slot in `[0, n)`; nothing stored can say whether
the key was one of the build's, so `id` returns a `u32` and the contract is exactly what a perfect
hash offers: a member's id, and for any other string some id below `n`. It is a separate type
rather than `fingerprint_bits = 0` because a membership check that always says yes would be a
signature that lies, and because the hot path is then one call with no compare behind it — the
`id_unchecked` of the other two hash indexes as the only method. Size is the perfect hash and a
36-byte header: **0.24 B/key** on real words, a fifth of the smallest fingerprinted index and a
tenth of any trie, flat in `n`. The serialised blob is `[magic "BCL2"][n][mph length][side_len]
[payload][check][MPH blob][side]`, the `CompactHashIndex` layout without its fingerprint section,
under the same 64-bit hash collision rule: keys sharing a hash resolve through the side table on
their full second hash, exact for members. There is no `load_mmap`: the blob is the perfect hash,
which every loader reads into memory whichever way it is opened, so there is nothing to borrow.

## `DictIndex`

The sorted keys front-coded in blocks, so that an id is a rank and a rank is a place. A block of
`block` keys (256 by default, `1..=1024`) stores its first key whole and every other as the length
of the prefix it shares with its predecessor and the suffix after it.

**The headers are one stream a shard wide, not a byte an entry.** `BDX2` spent a byte on each
`(lcp, len)` pair, four bits each, and two varints on every pair that did not fit; the distribution
inside a single shard is far narrower than that byte. A run is coded either as a frame of reference
— `min lcp`, `min len` and a width for each, all-ones escaping to the varints at the head of that
entry's suffix — or against a learned table of the `2^w - 1` costliest pairs at one to ten bits
each. Which of the two wins is a property of the shard, not of the format, so one byte a group
picks it from every run of that shard at once. That is why a shard is collected before any of it is
written: the code cannot be chosen from a block. At block 1024 the A-B that justified it read paths
14.34 → 12.46 bytes a key, urls 11.25 → 9.28 and dna 7.70 → 7.27 — the codec's own share, measured
when it landed, not the format's current size; the sweep in `docs/benchmarks.md` has that.

The suffix goes under a static symbol table in the manner of FSST (Boncz, Neumann and Leis, VLDB
2020): up to 255 symbols of one to eight bytes, one-byte codes, an escape for what no symbol covers, trained in four rounds of
parse-and-count over a sample of the index's own suffixes and stored in the blob, about a
kilobyte. **One table covers a shard of 65 536 keys**, not the whole index: a shard is a whole
number of blocks, each table is trained on 10 000 pieces sampled inside its own shard, the tables
train in parallel, and a lookup divides the block number by the shard to pick one. The suffixes of
a million paths under `/usr/share` and of a million under `/home` are different languages, and one
table over both is a compromise — a table a shard is worth 1.25 bytes a key on a path list, 0.23 on
ten million article titles and 0.01 on the dictionary, against 0.015 for the tables themselves. The codec is the crate's own — 300 lines, the reference's encoder shape, decode at
parity with `fsst-rs`, which would have raised the MSRV — its serialised table is its own as well,
so a `BDX3` neither reads nor writes a reference FSST table — and the training is deterministic, so a
blob is a function of its keys like every other.

**A shard whose alphabet is narrow skips the symbols.** A symbol table spends eight bits on a code
and earns them back by naming runs of bytes; on a shard drawn from four characters, or sixteen, or
sixty-four, that is the wrong trade twice over — two bits already meet the order-0 bound, and no run
of bases is frequent enough to pay for a symbol. Each shard prices a fixed-width code of its own
alphabet, one to eight bits, against its table on its own bytes — the suffixes and the headers they
imply, since a packed `len` counts codes where a symbol-coded one counts bytes — and takes the
winner, so a blob holding both shapes gets both. The codes of a run are continuous: an entry's
suffix starts where the one before it ended, mid-byte, and padding each to a byte instead costs 0.32
bytes a key on DNA, which is the whole margin. At block 256: dna 7.36 → 4.34 bytes a key, opaque
13.24 → 10.36, numeric 1.39 → 0.98. `uuid` keeps its tables — seventeen characters do not fit a
nibble — and is unchanged.

**What the whole blob repeats is named once.** Front coding takes the head a key shares with the key
before it; what is left still repeats across blocks — `/index.html`, `.example.com/`, ` - Wikipedia`
— and no symbol reaches past eight bytes. A dictionary of such spans is mined over the blob's own
suffixes and stored once, and a shard that buys it gives up symbols for phrase codes: a split at `s`
symbols leaves `255 - s` byte codes, each naming 256 phrases in one further byte or 65 536 in two,
with 255 still the escape. A suffix is parsed by dynamic programming in one backward pass over the
cheapest coding in bits — a symbol eight, an escaped byte sixteen, a phrase eight times the bytes
its id takes — and the miner is three rounds of that parse and a count of windows of up to three
adjacent tokens covering three to thirty-two bytes, keeping only candidates whose gain clears six
times what they cost to store. Mining stops after the first round unless one of three sampled
shards would take a split by 2 % on its own bytes: what the miner ranks is what coding a span once
would save, and what decides the format is whether a shard would rather spend those byte codes on
symbols — on a million opaque keys 40 000 spans clear the first bar and no shard takes one. Each
shard then decides for itself and has its table retrained on the spans its phrases did not cover,
and a blob no shard bought stores no dictionary. At block 1024 over a million keys: urls 9.26 →
7.27 bytes a key, English titles 9.08 → 7.21, paths 12.48 → 9.32; dna, opaque and numeric buy
nothing.

**A block is not what a lookup scans.** It is cut into microblocks of `micro` keys — the largest
divisor of `block` at or below 32, so 32 at the default and at every power of two from 64 up — and
the first key of every microblock after the block's own head is a **restart**: front-coded against
the restart before it, not against the key before it. A block's data is its restart run, one header byte a restart and then
their suffixes, followed by each microblock's run in the same shape. A lookup walks the restarts to
the one microblock that can hold the probe and scans only that — `block / micro + micro − 2`
entries, 38 at the default where one level scanned 255. The square root of the block would minimise
that count, and 16 was the first rule; but a restart entry costs about four ordinary ones, its suffix
being coded against a key a microblock away, so 32 measured level with 16 on `id` at every block and
0.08 B/key smaller, and 64 cost 35–50 ns for 0.05 more. What the block sets is how many keys share a stored head, a sample and two offsets, which is the per-key
metadata; it is now free to grow without the scan growing with it, and that is the whole point,
because the two were one number before. A divisor keeps every microblock of a block full but the
last, which is what makes a restart's rank `j · micro` rather than a running sum. A block of 32 or
fewer, or a prime one, is a single microblock — the layout of one level, and such a blob stores no
microblock starts, since the one microblock starts where the block does.

Beside the blocks sit four flat arrays: an eight-byte sample of each head in byte order (`u64`),
taken at `g` — the bytes every head in the blob shares, two bytes of header and none per block —
because a million URLs all begin `https://example.com/` and the sample would otherwise be the same
word for 3 899 of 3 907 blocks, leaving the binary search that opens every lookup with nothing to
say. Measured there: 165 samples duplicate their neighbour against 3 899, a probe's run of candidate
blocks is 2.02 against 3 271, and an `id` compares 1.13 heads against 11.54. Sorting is unchanged,
since every head shares those bytes; a probe that does not share them cannot be placed by the
samples at all and does not need to be — it is below every head or above every one, and the routing
answers with that boundary rather than a search. Then come the arrays saying where each head ends,
where each block's restart run starts, and where each microblock's entries start. The last three are not one word an entry. The two block-level ones only ever grow, so each keeps
one `u64` base every 64 entries and a delta of the width the corpus asks for — ten and sixteen bits
on the dictionary at the default block, 3.5 bytes a block against the eight the sample spends. The
microblock starts keep **no base at all**: each is an offset inside its own block, and the block's
start is a word the lookup reads before it needs them, so a base of their own would be a load and a
bit an entry for nothing. Eleven bits there, 1.375 bytes a microblock. The three together are
**0.0997 bytes a key and 3.8 % of the blob**, where the block the default used to be spent 0.348
and 11 %; and it is also why none of the three has a four-gigabyte ceiling — the two bases are full
words, and a microblock start is measured from one of them. A head, a restart run or a microblock is
read as the span between two entries, and neighbours share the word their deltas are cut from, and a
base where there is one, so the pair costs what one entry costs.

**A run's headers come first and its suffixes after**, rather than each header before its own
suffix — and since `BDX3` the headers of a whole shard are one stream of their own, ahead of every
suffix in it. The split is worth its bookkeeping because a scan rules most entries out by the
shared-prefix length alone, which lives in the header: 127 headers are two cache lines here and were
spread over the seven of a 128-key block before. Measured on the dictionary when the split landed —
then still a byte an entry, so the file was byte for byte the same length — `id` fell 9 % at 128
keys a block and 15 % at 256, with `key_into` unchanged.

`id` is a binary search over the samples — a flat array, eight bytes a block, built at load — then over the
heads of the few blocks whose sample equals the probe's, then the block's restarts and one of its
microblocks, neither decoding anything: an entry's stored suffix is compared against the probe
symbol by symbol, eight bytes at a time, and the shared-prefix length alone decides most entries —
shorter than what the probe has matched so far means the entry is past the probe, longer means it is
still below with nothing new matched. A restart run and a microblock are the same walk under the
same rule, which is why one function scans both. `key(id)` is two climbs from the head of block
`id / block`: over the restarts to the microblock the rank falls in, then over that microblock's
entries to the rank. A climb takes the entries whose shared-prefix length strictly increases — an
entry whose prefix is at least a later entry's writes nothing that survives, so a monotonic stack
over the headers finds the few that do — and decodes only those, one eight-byte store per code, into
a string the caller can keep (`key_into`). Past a staircase 32 deep a climb stops tracking it and
decodes every entry, which is slower and not wrong. On the dictionary at `block = 256`:
**2.64 B/key** (`StringIndex` 5.95), `id` 346–353 ns against 262–280, `key_into` 198–200 against
`key`'s 260–266; 128 and 512 give 2.66 and 2.52 B/key at 325–353 and 386–412 ns, and 1024 gives
2.51 at 426–436. Against one level at the same size the reverse lookup halves and `id` does not move:
244–246 ns and 392–399 for 2.819 B/key, where one level at 128 keys a block stored 2.827 for
460–464 and 393. The one-level format still reaches further down — 2.701 at 1024 keys a block —
but pays 3 190 ns of `key_into` and 984 of `id` for it, which is the trade the second level
removes.

Where the lookup spends that time was measured two ways in one process on an idle machine
([`bench/results/dict-routing-2026-09-13-arz-dab25e3.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-routing-2026-09-13-arz-dab25e3.txt)):
by timing the routing itself — the sample search and the head boundary, on the code `locate` runs —
and by timing an `id` handed the block it must search. On the dictionary at `block = 256` the two
agree, 45.1 ns directly against 44.2 by difference, of a 273 ns lookup; on ten million titles the
routing is 85.1 ns of 615 directly and 137 by difference, the gap being the cache the two halves
take from each other once the index is past the last level of it. **So the routing is a sixth of a
lookup and the in-block scan is the rest** — 37.1 ns of it the two binary searches over the
samples, 48 the head boundary. A sparser probe stream moves the second and not the first: at 20 000
probes rather than two million the head boundary nearly doubles, the heads and the block starts no
longer staying resident between lookups, while the sample search does not move.

That split is the other half of the block ladder: a larger block spends less on the search and more
on the scan, one for one, so past a million keys `id` barely moves across the ladder while on the
dictionary it rises steadily. Neither half of the routing is collected by a better structure over
the samples. A summary level removes no cold miss — 312 KB of samples at ten million is L2-resident,
so what it removes are hits — and an A-B of exactly that landed inside the control's own drift. A
cache-line-wide tree of eight-`u64` nodes, five levels where the binary search takes fifteen steps,
is **43 % slower** than the two `partition_point`s it replaces, and 16 % slower with AVX2 nodes:
the first eight steps of a binary search re-read the same 2 KB for every probe, `partition_point`
selects branchlessly, and its two searches are independent chains where a descent is strictly
serial.

The serialised blob is a 72-byte header — `[magic "BDX3"][n][block][head bytes][data bytes]
[codec bytes][payload][offset widths][micro][shard][header-code bytes][g][dictionary bytes][check]`
— then the heads, the packed head ends, the block data, the suffix codecs, the header
codes, the phrase dictionary, the packed block starts and the packed microblock starts; the loader
checks every length, both checksums, the codecs, the dictionary and the arrays' order before
anything is trusted, and the block data — bounded on every read rather than validated up front — is
what the fuzz target queries after loading. Everything from the codecs on comes after the data
because nothing decides it until the data is encoded: which codec each shard settled on, which code
its headers took, whether any shard bought the dictionary, and how wide the two start arrays are.
That is what lets a streamed build write every section once and in order. One delta width serves a
whole array, and a width per superblock was measured rather than assumed: over thirteen corpora at three
blocks each it is worth at best **0.10 % of the blob** (`pypi` at 32 keys a block), and it is
*negative* on six of the thirteen, the table of widths costing about what the narrowing saves. A
third of the arrays clear the obvious gate — 5 % of their superblocks could drop two bits or more —
and they are the block-level arrays, which have between two and fifty superblocks in the first
place; `micro_offsets`, thirty times as many entries, cleared it twice in twenty-six, microblock
spans being uniform by construction, and it has no superblocks left to narrow. The distribution says
yes and the bytes say no. `load_mmap` borrows
every section; the per-block samples that two binary searches read on every lookup (below) are not
a section at all — a sample is eight bytes of a head, and the load reads them out of the heads
rather than out of a copy the blob carried, which is eight bytes a block off every blob for a load
and a lookup that did not move (2.666 bytes a key to 2.635 on words at the default block, 1.131 to
1.068 on a million numeric keys at 128);
there are no automata, so a fuzzy question is `StringIndex`'s — but prefix and range are not
automaton questions here, they are two `lower_bound`s and a walk, and this index answers them
itself: `prefix_id_range` costs two order lookups whatever the number of matches.

## `PerfectHashIndex`

A minimal perfect hash maps a *fixed* set of `n` distinct strings to distinct slots `[0, n)` with no
gaps and near-`O(1)` lookup in tiny space. lexindex builds the MPH itself (`src/mphf.rs`), keyed on
a **version-stable** 64-bit hash of each string (branch-free over three length
classes — four overlapping 4-byte loads to 16 bytes, the first and last sixteen to 32, two
multiply lanes over 32-byte blocks beyond — mixed by 64×64→128 multiplies folded to 64 bits with
the length; 3.9 ns a dictionary word against 7.3 for the hash 2.0–3.x shipped, whose word loop and
tail switch mispredicted on every length change — not
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
where membership is already guaranteed. The serialised blob is `[magic "BMP8"][n][mph length]
[side_len][payload][check][MPH blob][arena bytes][side]` — the payload hash covers everything after
the header and is verified on owned loads. The arena is `[n+1][tag][offsets][data]`, and the tag
names one of four encodings.

Addressing the keys was for a long time the largest part of this index. Before 0.5.0 every offset was
a `u64`: 8.0 of 17.6 bytes per key on the dictionary, to address a 4.9 MB arena. Choosing 4 or 8
bytes per arena took the whole structure to 13.62. Since 1.1 the offsets are **blocked**: 16 slots
share a `u32` base and carry one-byte *cumulative* offsets after it (tag `0x11`, 21 bytes per block),
so a key is `data[base + off[k] .. base + off[k + 1]]` — 1.31 bytes per key instead of 4, and the
whole index is **10.88 B/key**. A corpus whose 16-key runs do not fit in 255 bytes gets 256-slot
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
`BMP8`; the tag alone would already stop a reader from before 2.0, as an unknown arena encoding.

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
data and the three offset arrays are read where they lie, an array entry decoded where it is read, and
what the load reads is the header, the symbol tables and the heads, out of which it builds the
per-block samples — eight bytes a block, one byte per thirty-two keys at the default block, and not
a section of the blob. The samples are held rather than borrowed because two
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

## Choosing an index: `plan`

Five structures with five corpus-specific size curves is a table nobody can read an answer out of.
The spread between them on one corpus is larger than the spread of any one of them across corpora,
so the only honest recommendation is one computed on the keys in hand. `plan` does that, and the
question it had to answer first was how much of the answer can be *derived* rather than built.

Most of it. One walk over the sorted keys gives the count, the mean length, the mean shared prefix
with the previous key and the same for the keys 32 apart — which is exactly what a front-coded
block is paid in — and the trie-node count an fst is paid in. Both come out of the same identity:
with the keys sorted, the number of adjacent pairs sharing at least `d` bytes is `n − D(d)`, where
`D(d)` counts the distinct `d`-byte prefixes, so summing `1 − D(d)/n` over the depths gives the mean
adjacent prefix, and `Σ (len − lcp)` gives the nodes a trie needs. Nothing there is a sample.

The sort is not an extra cost: a plan is followed by a build, and the build needs the keys sorted
anyway, so the same copy serves both. That is what makes measuring the exact statistics affordable
and it is why the sketching route was dropped. A HyperLogLog per depth estimates `D(d)` without
sorting — 2^16 registers a depth hold `lcp₁` to ±1.3 % — but it never became cheaper than the sort it
replaced, and it has a sharp edge the sort does not: `d lcp₁ / d ln n = maxdepth − lcp₁`, which is 39
on the 480 k-word dictionary, so an approximate `n` moves the answer far more than an approximate
`D`. A second finding survived the experiment even though the route did not: the mean prefix shared
by keys `k` apart is linear in `ln k`, and a random 1-in-`step` sample reads that curve at
`k = step·e^{−γ}` rather than at `step` — the offset is the Euler–Mascheroni constant, and for pairs
`j` apart in the sample it is `ln step + ψ(j)`.

What no statistic gives is what a *compressor* will do: the ratio the FSST symbol table squeezes a
suffix into, the bytes an fst actually spends per trie node once it has merged what it can, and the
bits the perfect hash spends per key. Those are read off builds of two 100 000-key draws — one
uniform over the corpus, one of runs of consecutive keys — and below that size there is nothing to
model: the candidates are built and reported at what they weigh. Scored against the built blob on
23 corpora of half a million to ten million keys, at each of the three priced blocks, the
`DictIndex` estimate lands within **1.0 % median, 4.2 % at the 90th percentile and 7.2 % at worst**.

The two draws answer two different questions, and one draw answering both was the estimate's largest
error. What a suffix compresses to is a property of the whole key set, so the uniform draw reads it;
what a stored `(lcp, len)` pair costs is a property of the neighbours a key is coded against, and a
uniform draw puts those `n / 100 000` positions apart — it read a million decimal ids 0.51 bytes a
header where the blob spends 0.34. Both draws are decided by hash, so `plan_file` makes them over a
merged stream exactly as `plan` makes them over the sorted keys.

**The sample is built with the corpus's economics, not its own.** The miner admits a phrase on what
it saves over the whole blob against what storing it costs once, so a hundred thousand keys afford a
tenth of the dictionary a million do: built as an index of its own the sample read the suffix ratio
0.71 where the million-key blob spends 0.53 on article titles, and no slope fitted below the sample
reaches the corpus — the vocabulary is flat below a hundred thousand keys and falls convexly above
them. So the sample is mined at the corpus's key count and over every suffix it holds, which took a
million urls from 14 % high to 0.9 % low. That vocabulary is bought **once for the plan**, not once a
block and not once a draw: over eleven corpora the three priced blocks read the ratio within 0.4 % of
each other, mining at each of them was two thirds of what a plan spends, and it was also *less*
accurate — the smallest block's own draw reads the ratio 21 % high on decimal ids. What the whole
channel costs is a fixed 0.35 s and 44 MB a plan, whatever the corpus, so the ratio is worst on the
cheapest plan and disappears on the largest: 1.30× the wall time and 1.39× the peak on a million
urls, 1.22× and 1.62× on a million Chinese titles, 1.06× and 1.12× on ten million English ones, and
**no change either way** on the streamed 7 343 721-line path list, where the sort's own run budget
is already higher than the miner ever reaches.

The two places it does not hold are reported rather than papered over. `StringIndex` is looser —
3.0 % median, 7.6 % at the 90th percentile, 32 % on a corpus of file paths — because an fst merges
equal suffixes and how much it merges is a property of the whole key set, not of a sample of it: the
sample sees fewer sharable tails than the corpus has, so the estimate runs high exactly where the
corpus is most repetitive. And a corpus whose mean suffix is under two bytes — ten million decimal
numbers, the worst cell in the `DictIndex` score at 4.1 % high — is flagged, not quoted. So is a pair of
candidates within 1.3× of each other, which is inside what an estimate can separate. In both cases
the plan says to build both and measure, which is the same advice this document gives everywhere
else.

## Versioning

Semantic versioning, with one qualification that matters more here than the API does: **a blob format
is part of the contract**. A version that refuses a format an earlier one wrote is a major release,
even when every public signature is unchanged — which is exactly what 1.0 is. `cargo semver-checks`
runs in CI and passed clean across that release, because nothing in the API shrank; it catches the
half of compatibility that lives in signatures, and the CHANGELOG's "Changed — breaking" section
catches the half that lives in bytes.

**A new format ships beside a reader for the old one.** 1.0 and 3.0 each refused what the previous
major wrote, and each cost every user of that format a rebuild. The rule from 3.0 on: a writer
change lands as a *minor* release whose loader still reads the format the previous one wrote, so
upgrading never fails on a file; dropping a reader is reserved for a major, and only once the
old blob can be turned back into the key list that rebuilds it. `BDX3` is the first format written
under it: it is a third smaller than `BDX2` on the corpora the dictionary is aimed at and shares no
section layout with it, so 4.0 refuses `BDX2` by name rather than carry a second reader. The way
back is that `BDX2` stores every key: 3.x answers `key(id)` over the whole blob, so a loop on the
installed 3.x writes the key list and 4.0 builds from it. `lexindex dump` makes that loop a
subcommand, but it is 4.0's — 3.0.0 shipped `plan`, `build` and `inspect` only — so the rule buys
the *next* format change a one-liner, not this one. [Upgrading to 4.0](migration-4.md) is the
worked path.

**The key hash moved in 4.0 as well, and that is where the rule stops.** `BMP7`, `BCH7` and `BCL1`
are keyed on a hash this version no longer computes, so every id they would answer is wrong; all
three are refused by name. For `BMP7` the way back is the rule's — it stores its keys, so 3.x can
write them out and 4.0 can read them back — but `BCH7` and `BCL1` store no keys in any version, so
nothing can dump them and the way back is the corpus they were built from. The ids move either way:
a perfect hash's id is a slot, and both the hash and the seed geometry changed. A hash
change is a heavier break than a format change for exactly that reason, which is why it waits for a
major rather than riding one.

The minimum supported Rust version is the `rust-version` field in `Cargo.toml`, currently **1.85**,
and a CI job derives its toolchain from that field so the two cannot drift. Raising it is a minor
release, not a patch.

The C ABI has a number of its own, `LEXINDEX_ABI_VERSION`, because nothing like `cargo semver-checks`
watches a C header. Within a number the ABI is append-only — a newer library serves an older header —
and removing or changing a symbol bumps it, which is a major release of the crate. The header is
generated from `src/capi.rs` by `cbindgen` and CI regenerates it to compare, so the two cannot drift.

## Security

What is validated on load and what is merely trusted is spelled out per format above; the threat
model that ties it together — soundness versus correctness, why the checksums are integrity and not
authentication, and where the crate's `unsafe` lives — is [`SECURITY.md`](https://github.com/ilgrad/lexindex/blob/main/SECURITY.md).

## Blob compatibility

Every blob starts with a four-byte magic whose last character is the format version. A format change
bumps it, and the loader decides one of three things about the old one: read it, refuse it by name,
or — never — read it wrong.

| Magic | Written by | Structure | Older formats |
|---|---|---|---|
| `BIX4` | 1.0 | `StringIndex` | unchanged since 0.5; every published `BIX4` loads |
| `BMP8` | 4.0 | `PerfectHashIndex` | `BMP1`–`BMP7` **refused by name** |
| `BCH8` | 4.0 | `CompactHashIndex` | `BCH1`–`BCH7` **refused by name** |
| `BCL2` | 4.0 | `ClosedHashIndex` | `BCL1` (2.0–3.x) **refused by name** |
| `BDX3` | 4.0 | `DictIndex` | `BDX1` (2.0) and `BDX2` (2.2–3.x) **refused by name** — `BDX1` had no microblocks and unpacked per-block arrays, `BDX2` a header byte an entry, one codec and no phrase dictionary |
| `OVL2` | 1.0 | `Overlay` | `OVL1` **read**; saving again writes `OVL2` |
| `MPH3` | 4.0 | the minimal perfect hash, inside `BMP8`, `BCH8` and `BCL2` | `MPH2` (1.1–3.0) **read**, inside those containers and standalone, under its own seed geometry; `MPH1` (1.0) **read** as a standalone blob |

**The policy is that a refusal must say which version wrote the file.** A blob refused on a bare "bad
magic" sends someone hunting for disk corruption when the file is intact and merely old, so all three
hash-index loaders carry the list of magics they used to write and answer with a sentence naming
`lexindex < 4.0` and the fix. That is worth more than a conversion path would have been, because for
these formats there is no conversion path to offer: every hash blob before 4.0 is keyed on a hash
this version does not compute — 2.0 replaced the round, 4.0 the shape (and the pre-1.0 ones embed a `ptr_hash` image it no longer links),
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
- `capi` — the C ABI: one opaque handle over the five indexes and fourteen `lexindex_*` functions,
  declared in `include/lexindex.h`. Pulls `mph`, no dependency. `cargo build --release --features
  capi` exports it from the `cdylib`; `cargo rustc --release --features capi --crate-type staticlib`
  gives the archive.
- `--no-default-features` — an `fst`-only build: `StringIndex` with prefix/range/fuzzy/subsequence and
  owned `save`/`load`, depending on nothing but `fst`. The full default build depends on `fst` and
  `memmap2` and nothing else, and `cargo audit` reports nothing on either. CI cross-checks `i686`
  and `wasm32` on both.
