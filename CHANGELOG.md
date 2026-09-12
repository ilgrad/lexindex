# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A pinned corpus set, `bench/corpora.py`.** Every size this project has published is measured on
  one corpus, the Fedora word list, while bytes per key is a property of the keys at least as much as
  of the structure: `StringIndex` measures 0.68 on one corpus and 16.57 on another, and `marisa-trie`
  moves between 2.12 and 6.21 over the same span. Thirteen corpora now exist at 100 000 / 1 000 000 /
  10 000 000 keys wherever the source has them — Wikipedia titles in three scripts, the URLs those
  titles form, Tranco domains, PyPI package names, this filesystem's paths, vendored Rust
  identifiers, the word list, and seeded UUIDs, dense decimal ids, opaque base64url ids and DNA
  24-mers. The sizes are nested, so a difference between two of them is scale and not composition;
  the downloads are pinned to a dated dump or a permanent list id rather than to `latest`; and the
  SHA-256 of every download and every derived file is in `bench/corpora.json`, which
  `python bench/corpora.py verify` checks. The corpora are gitignored — 4.6 GB — and the manifest is
  what makes them checkable anyway. No benchmark is on the set yet.

- **`common_prefix` and `longest_prefix` on both ordered indexes**, in Rust and Python: every key
  that is a prefix *of* the query, shortest first, and the longest of them. This is the reverse of
  `prefix` and the query a longest-match tokeniser runs — a vocabulary and a position in a sentence
  give the entries that start there. It was the one operation `marisa-trie`, `dawg2` and `datrie`
  all had and lexindex did not, which the comparison tables now carry as a column of its own.
  `StringIndex` answers it in a single walk down the transducer, `O(query bytes)` whatever the index
  holds — the fastest `longest_prefix` in the comparison at **422 ns**, against `datrie`'s 482,
  `dawg2`'s 810 and `marisa-trie`'s 965, and within 3 % of `datrie` on `common_prefix` (726 against
  706) at a fifth of its bytes. `DictIndex` has no such walk and pays one order lookup per character
  boundary — 2 157 ns at 32 per block — except on `longest_prefix`, which stops at the first hit and
  so comes in under marisa at 863 ns. The documentation says which is which rather than implying
  they cost the same.

- **`DictIndex::build_to_file`** — the constructor for a corpus that does not fit in memory, and
  `DictIndex.build_to_file(items, path, block=32)` in Python. The keys arrive in any order, are
  sorted in runs spilled beside the output and merged back, and the block data goes into the file as
  it is encoded. The bytes are exactly what `build` followed by `save` would have written, which the
  tests assert at four block sizes over both the spilled and the single-run path. Measured over
  479 823 words streamed from a file: peak RSS **32.1 → 12.7 MiB** for the same 3.52 bytes per key on
  disk, at about 19 % more build time. What is still held is the block heads and the three per-block
  arrays — roughly `(mean head length + 20) / block` bytes per key, so a larger block holds less as
  well as storing less. The external sort it shares with `StringIndex` moved to its own module.

- **`DictIndex::build_sorted` and `build_sorted_with_block`**, for keys already in ascending byte
  order — a sorted file, a database cursor, the output of an external sort. Adjacent duplicates are
  dropped exactly as `build` drops them after sorting, so for the same key set the two produce
  byte-identical blobs. It is not the faster of the two and does not pretend to be: `build`'s sort is
  pattern-defeating, recognises an ascending run and returns almost at once (31.9 against 32.1 ms
  over 479 823 words). What it buys is the check — an unsorted input returns an error instead of an
  index whose every binary search is wrong, which is what `StringIndex::build_sorted` has given since
  0.11 and `DictIndex` did not.

### Changed

- **A batched `DictIndex` lookup is no longer a loop.** `ids_of` — and with it `ids_of_bytes` and
  `ids_of_arrow`, the Python and Arrow batch paths — ran `id` once per key, and a single `id` is a
  chain of dependent loads: the binary search over the block samples, then the block's head, then
  its data. Past the last-level cache each one waits on the last. Thirty-two lookups now advance in
  lockstep — every lane takes one step of its binary search before any lane takes its second, and
  the blocks they land on are prefetched whole before any lane is scanned. On real word bigrams,
  half the probes members, shuffled, against a loop of `id` in the same process: **861 → 535 ns a
  key at 10 M keys and 32 per block** (1.61×), 771 → 645 at 128 per block, 377 → 306 at 1 M, and
  277 → 281 at 200 k — where the index is 0.6 MB, nothing stalls, and there is nothing to hide.
  Prefetching the *heads* as well was tried and measured 7 % slower; it is not in the code.

- **`DictIndex::keys_of` walks a block once for the ids that ascend through it.** It called `key`
  per id, and `key` re-enters the block at its head and decodes forward to the entry, so a run of ids
  inside one block decoded the same prefix chain again for every one of them. A cursor now stays open
  while the next id is in the same block and above the current one — which is exactly the shape
  `prefix_id_range` hands it — and falls back to `key` for anything else, so an arbitrary order is
  answered as before and a test pins both against `key` over shuffled, reversed and repeated ids.
  Measured over 20 000 three-byte prefixes on 479 823 words, 763 matches each: a prefix's keys through
  `prefix_id_range` + `keys_of` go **135 595 → 71 915 ns** at 32 per block and **218 542 → 70 566** at
  128. That makes the id range the fastest way to enumerate a prefix here, ahead of `prefix` itself
  (103 716 ns, and it decodes an id for every match) and of `marisa-trie` (93 355) — the reverse of
  what `docs/benchmarks.md` recorded, which is corrected with the new artifact behind it. The columns
  the change cannot touch moved by up to 8 % between the two runs, the extension having been rebuilt
  in between, so the 1.9× and 3.1× are not that.

- **A `DictIndex` encodes its blocks on every core.** Once the symbol table is fixed a block depends
  on nothing outside itself, so contiguous ranges of blocks go to their own threads
  (`std::thread::scope`, no new dependency) and the parts are concatenated in order. The bytes do
  not depend on how many threads ran, which a test pins at 1/2/3/5/8/64 threads against the
  single-threaded blob — and the streaming builder checks from the other side, since its own
  encoding pass is serial. Measured on this 8-core machine, one core against sixteen: **126.2 →
  75.2 ms** over 909 776 real file paths and **34.0 → 24.6 ms** over 479 823 words. The threads take
  a `&[&str]` view rather than the caller's `&[S]`, which would have meant adding `S: Sync` to a
  signature that never asked for it; the view costs 16 bytes a key while the encoding runs, which on
  the paths corpus is *less* than the doubling the single `data` buffer did (41.6 against 42.4 MiB
  allocated) and on the shorter words costs 12.5 bytes a key.

- **Building a `DictIndex` no longer copies every key.** `build_with_block` collected
  `items.into_iter().map(|s| s.as_ref().to_owned())`, one owned `String` per key, before sorting.
  Sorting the caller's items through `AsRef` instead costs nothing and removes the copy, because the
  keys are copied into the blocks anyway. Measured with a counting allocator over 479 823 words, from
  a `&[String]` — which is what the Python constructor and every `build(&keys)` call pass: the build
  allocates **42.6 → 17.3 bytes per key** and takes **54.0 → 32.1 ms**. From an owned `Vec<String>`
  moved in, the allocation was never doubled (`Vec::into_iter().map(…).collect()` reuses the buffer
  in place and frees each source key as it copies) and the time goes 47.6 → 39.8 ms.

- **The comparison table carries a lookup-latency column.** Bytes per key on their own read as
  though the smallest structure were the best one. `bench/compare.py` now times one exact lookup per
  library over 100 000 probes, half of them plausible strangers, shuffled — every structure taking
  one pass per round so that none is measured with the caches still warm from its own build, and all
  of them resident while each one runs. Two costs are named instead of assumed: the Python call
  boundary is **49 ns**, and `.get` on a miss is each library's own miss path rather than a
  `try/except` around `__getitem__` — timed apart, a miss costs 0.85–0.91× a hit everywhere, while
  that wrapper written out by hand costs 1.61×, which matters because half the probes are misses. On
  this corpus the four keyless rows answer in **92–100 ns against a builtin `dict`'s 260**, and
  `DictIndex` at 128 per block is smaller *and* faster than every `marisa-trie` setting measured
  (2.89 B/key and 389 ns against 2.96–3.07 and 449–490).

- **`marisa-trie` is benchmarked as a curve, not a point, and `rsmarisa` joins the table.** Every
  marisa number this project published came from the default configuration, while marisa's own
  documentation says the right setting depends on the data — an open invitation to the objection
  that the baseline was untuned. Measured over the whole space on the same words: `num_tries`
  1/2/3/4/5/8/16/32 gives 3.380/2.997/2.978/2.977/2.977/2.980/2.986/2.998, flat from three and worse
  past eight; `cache_size` TINY…HUGE gives 2.957/2.964/2.978/3.008/3.066; `order` and `binary` do
  not move it. Its best is **2.955**, so `DictIndex` at 128 per block leads by **2.2 %**, not the
  3.0 % against the default, and the comparison tables carry three marisa rows.

  `rsmarisa` — a pure-Rust port of marisa-trie, BSD-2-Clause, first published 2026-01-26 — is in the
  Rust table for the first time. `docs/benchmarks.md` had said no succinct LOUDS trie existed in Rust
  to depend on and rested a "smallest pure-Rust ordered index" claim on it; **both were wrong**, and
  the second was wrong about this crate as well, since `DictIndex` shipped at 3.52 B/key in 2.0 and
  was never added to that table. Measured: `DictIndex` at 128 per block **dominates `rsmarisa` at its
  most compact setting outright** — 2.894 B/key against 3.003, 435 ns against 445 on members, 442
  against 473 on misses, and 38 ms against 174 to build — while at 32 per block it is the fastest
  structure there (323 ns) and larger than either `rsmarisa` setting. A frontier, not a crown.

- **The hash quality battery runs in CI.** It was `#[ignore]`d behind `bench-mphf` and CI only ran
  clippy on that feature, so the battery compiled and never executed and the committed artifact was
  a hand-run. It is a weekly job now. Bit independence covered the slot hash at one key length while
  avalanche covered both hashes at five, and the notes claimed the wider scope for both; it now runs
  both hashes over 8/16/24/80-byte keys, sampling at most 128 input bits per pair so the cost stays
  flat in key length. The cell count goes 346 112 → 1 894 400, so the bound moves 6.0 → 6.5 (PARI/GP:
  a family-wise 10⁻³ over that many wants 6.211; 6.5 leaves 1.5 × 10⁻⁴). Worst observed: avalanche
  4.49 unchanged, bit independence 4.23 → 4.54 over seven times the cells.

- **The queries 2.1 added are fuzzed, and `BDX1` is checked on a big-endian and a 32-bit target.**
  The fuzz shim asked for `id`, `lower_bound`, `key` and a walk; `prefix_id_range`, `prefix_count`,
  `prefix`, `range`, `range_count`, `successor`, `predecessor` and `iter_after` were untouched by any
  target. All of them are held to bounds on malformed input now, and on a blob that passed every
  check the whole ordered surface is cross-checked against a linear walk. The weekly Miri jobs gained
  `dict_index::`: the format stores each block head's first eight bytes as a *big-endian* word inside
  an otherwise little-endian blob, which is exactly what the s390x job exists to catch, and it had
  never run there. Both targets pass — coverage for what ships, not a fix.

### Fixed

- **Documentation claims a reader could have checked against the table beside them.** "`StringIndex`
  is the only structure that answers fuzzy *and range* queries at all" — `DictIndex` has answered
  range and prefix since 2.1, and only fuzzy and subsequence need the automaton; the capability
  matrix said it structurally too, merging prefix into one row with fuzzy so `DictIndex` read as
  having no prefix four lines above the paragraph saying it does. "`marisa-trie` remains the pick for
  exact membership and ordering and the smallest such index" — the table fifteen lines above it
  disagrees. `SECURITY.md` counted seven `unsafe fn`s and nine `unsafe` blocks; it is nine and
  eleven, "on every index" was never right since `ClosedHashIndex` has nothing to map, and the fuzz
  inventory listed eight targets where there are nine. And nothing said why `Overlay` has no
  `DictIndex` or `ClosedHashIndex` base, which matters now that the smallest row in the table is the
  one that cannot take an edit: an addition takes the id after the base's last, which is not a rank,
  and a closed vocabulary has no membership to ask.

- **The benchmark artifacts behind the tables are taken on a clean tree.** The one the README cited
  recorded `commit: 00857e2-dirty` and `lexindex: 2.0.0` while backing a 2.1.0 table, the hash
  battery's was `c597099-dirty`, and `docs/benchmarks.md` pointed at an artifact from 2026-09-09 that
  predated the rows above it. One run, one file, both pages citing it.

## [2.1.0] — 2026-09-11

### Added

- **`DictIndex` answers prefix and range queries**, in Rust and Python: `prefix`, `prefix_iter`,
  `prefix_id_range`, `prefix_count`, `range`, `range_iter`, `range_count`, `iter_after`,
  `successor`, `predecessor` — the ordered surface `StringIndex` already had, with the same names
  and the same answers (a test cross-checks the two index types on every one of them). The type had
  been documented as having none of this because it has no automaton, which conflated two things:
  a *fuzzy* query needs the automaton, a prefix does not. Keys sharing a prefix are adjacent in
  rank, so a prefix is one contiguous id range, and `prefix_id_range` finds it with two
  `lower_bound`s.

  That makes counting a different operation rather than a faster one: **`prefix_count` is 351 ns
  where `marisa-trie` takes 127 657**, because marisa has to enumerate all 763 matches of an
  average three-byte prefix to count them — its ids are not lexicographic ranks, so it has no
  arithmetic to do instead. At `block=128`, where the index stores **2.89 bytes per key against
  marisa's 2.98**, it is also ahead on autocomplete (2 435 ns for the first ten against 2 773) and
  level on full enumeration, while returning each match's rank as well.

- **`DictIndex::load_mmap` and `load_mmap_verified`**, in Rust and Python. The keys, the block
  data and the two offset arrays are held as the bytes they are serialised as, owned or mapped,
  an entry decoded where it is read; so a mapping borrows them and the load reads the header, the
  symbol table and the per-block samples — eight bytes a block, one byte per four keys at the
  default block. That last array is read rather than borrowed because two binary searches over it
  open every lookup and only `<[u64]>::partition_point` keeps those searches out of the branch
  predictor: it selects with `hint::select_unpredictable`, which needs an alignment a section of a
  blob does not have, and searching the bytes instead measured 110 ns against 26 for the pair.
  `load_mmap` skips the payload checksum and the walk over the arrays `load` makes, every access
  bounding what the arrays say instead — a crafted file answers wrong, never out of bounds — and
  `load_mmap_verified` runs both over the mapping. The `BDX1` blob is unchanged, and reading a
  section where it lies rather than as a decoded array costs `id` 303 → 308 ns and `key` 200 →
  209 on the dictionary at `block = 32` (`StringIndex` control 342 → 337; min of five rounds
  alternated in one process, shuffled probes).

- **A hash-quality battery** under `bench-mphf`, and its committed output in `bench/results/`.
  Strict avalanche and bit independence over both hashes and five key lengths, the two-byte
  differential scan that caught the pre-2.0 collision family, and per-corpus distribution tables
  over ten key shapes — words, bigrams, a shared prefix, a shared suffix, a dense numeric tail,
  decimal integers, UUIDs, Cyrillic, DNA and paths. Everything reads as a standard-normal `z`
  against one bound of 6; a chi-square goes through Wilson–Hilferty rather than
  `(x − k) / sqrt(2k)`, which understates the tail by two orders of magnitude at 255 degrees of
  freedom. Nothing in the battery is past the bound: the worst cell of 44 032 is 4.49, where the
  maximum of that many standard normals sits near 4.6, and the differential scan finds no double
  collision at all.

### Fixed

- **`inspect` is total on an overlay too.** Its live-key count was `base + additions − retired`
  on numbers straight from the header, which a crafted blob could make wrap (or panic in a debug
  build); the counts are checked now, and a header whose counts do not add up is an `IndexError`,
  as is one claiming more additions than its section could hold. A base region that is itself an
  overlay — nothing this crate writes, since a base tag names an index — was parsed by the same
  routine without a bound, so a few megabytes of nested headers overflowed the stack; it is
  refused by name. The tombstone words an overlay's retired count comes from are read 64 KiB at
  a time rather than as one allocation the size of the section (125 MB over a billion ids), and
  the docs say what an overlay inspection reads. A `parse_inspect` fuzz target holds `inspect`
  to a return value on any bytes.

### Changed

- **`DictIndex::key` and `key_into` decode a handful of entries instead of the whole block.** An
  entry stores what it shares with its predecessor, so an entry whose shared-prefix length is at
  least a later entry's contributes nothing that survives to the key being asked for; the ones that
  do form a strictly increasing staircase of that length, which a monotonic stack over the block's
  headers finds in the pass the walk already makes. Every header is still read — a header is what
  says where the next one begins — but the decode, the expensive half, runs on the staircase alone:
  2.5 entries deep on average over the dictionary and a path list, 12 at the deepest with a block of
  32 and 18 with a block of 1024, out of up to 1 023. **`key_into` on the dictionary: 207 → 175 ns
  at the default block, 697 → 463 at 128 per block, 1 361 → 851 at 256** (`id` and `lower_bound` are
  untouched). The blob, the format and the API are unchanged, so an existing `BDX1` file gets this
  by being read by the new version; past a staircase 32 deep the walk decodes every entry as before,
  which a block holding a chain like `a`, `aa`, `aaa` reaches and which is slower, not wrong.

- The weekly sanitizer workflow fuzzes `parse_inspect` too. The target shipped with the `inspect`
  fix but was never listed in the matrix, so the one entry point that reads a header of *any* of the
  five formats had no fuzz job of its own.

- Documentation: the `DictIndex` block size is measured across the curve rather than at three
  points — 4.35 bytes per key at 16 through 2.78 at 256, with `id`, `key_into` and `lower_bound`
  beside each — because 32 is the middle of that curve, not a limit, and `build_with_block` has
  always been the knob. At 128 the index stores **2.89 bytes per key, under the 2.98 `marisa-trie`
  takes on the same words**, and unlike marisa it answers `key(id)` and `lower_bound` at all, a
  marisa id not being the lexicographic rank. The comparison tables carry both block sizes, and the
  older claim that beating marisa on an ordered index meant reimplementing marisa is scoped to the
  trie route it was written about. No code changed.

- Documentation: an `OVL2` crosses 1.x → 2.0 only over a `BIX4` base, since an overlay embeds
  its base verbatim (README, `SECURITY.md`, `docs/design.md`); the `build_with_fingerprints`
  entry below said the blob stays `BMP6` — it is `BMP7`, like every 2.0 `PerfectHashIndex`
  blob; "three indexes" is five throughout; the `DictIndex` symbol table is called FSST-style,
  with a table format of its own, so that `BDX1` is not mistaken for a reader or writer of the
  reference format; the perfect-hash comparison scopes its claim to its harness (lexindex takes
  pre-hashed keys); the `build_to_file` replay digest is called what it is, a 64-bit
  probabilistic check; `DictIndex` is 41 % below `StringIndex`, not "a third of" it (3.52
  against 5.95); and the two links that pointed outside the docs tree (this file to
  `docs/usage.md`, `docs/design.md` to `SECURITY.md`) are absolute, so they resolve both on
  GitHub and on the docs site.

## [2.0.0] — 2026-09-10

### Changed

- **Breaking: the key hash is replaced, and every hash blob written before 2.0 is refused by
  name.** The per-word round of the 1.0 hash -- a 64-bit multiply and a rotate -- had a two-word
  collision family on ordinary text: a difference in the top bits of one word stays in the top
  bits of a 64-bit product, and the rotate dropped it into byte 3 of the next word, where that
  word's own difference cancelled it -- in *both* hashes, whatever their constants. Keys of 16
  bytes or more differing at bytes 8i+7 and 8i+11 alone collided with probability 13–100 %
  (`d`↔`t` with `e`↔`o`; a case flip with `e`↔`i`; an ASCII/non-ASCII flip with `a`↔`q`, always),
  and `CompactHashIndex` and `ClosedHashIndex` merged such pairs into one id, silently:
  `sering.dampening` and `sering.tamponing` were one key, and 10⁹ generated `word.word` keys held
  nine such pairs. The round is now the full 64×64→128 product folded to 64 bits (`lo ^ hi`): a
  single-bit scan over every position pair of a 24-byte key finds no weak pair where the old
  round had 35, the same 10⁸ keys collide nowhere, the 10⁹ set builds to all 997 504 005 of its
  keys where the old hash lost nine, and the distribution gates (avalanche, low-bit χ², slot ×
  fingerprint independence) read the same. Cost: the pair hash at 0.90–0.97× on 9–11-byte
  keys and 0.89× on 80-byte ones; in a lookup, process-alternated on the shuffled dictionary,
  `CompactHashIndex::id` moves by less than the harness's own layout noise — 71.4 → 74.3 ns with
  one build of the harness, 70.7 → 69.6 with another the same day, the hash's own latency 0.6 ns
  longer — batched `ids_of` 27.7 → 28.7 and 27.0 → 28.2 (a throughput path: 1.5 % more
  instructions), while the paths that compute one hash — `ClosedHashIndex::id`, `id_unchecked` —
  do not move. Magics move to **`BMP7`**
  and **`BCH7`** (`BCL1` never shipped and keeps its name); `BMP5`, `BMP6` and `BCH6` join the
  refused list with a message naming `lexindex < 2.0` and the rebuild, since a blob keyed on the
  old hash would answer wrong ids under the new one. `BIX4` and `OVL2` are untouched. Rebuilding
  from the keys is the migration. Found by the streaming builder's distinct-key count disagreeing
  with its input.
- The weekly libFuzzer job runs `parse_closed` as well, and every target now starts from the
  corpus the previous run left — an Actions cache per target — rather than from the seeds alone.

### Added

- **`DictIndex`** (Python: `DictIndex`): an ordered dictionary with the key stored for every id —
  exact `string ↔ rank` both ways, `lower_bound`, in-order iteration, and no automata. The sorted
  keys front-coded in blocks of 32 (a build parameter, `1..=1024`), the suffixes under a static
  symbol table — an in-crate FSST-style codec with a table format of its own, 300 lines, trained
  deterministically on the index's own
  suffixes — so a blob is a function of its keys. On the dictionary: 3.52 B/key against
  `StringIndex`'s 5.95, `id` 300 ns against 345, `key` 197 against 505 (`key_into` decodes into a
  string the caller keeps); `id` compares the stored suffixes against the probe without decoding
  them. Blob `BDX1`, checked on load like the others and fuzzed after loading (`parse_dict`, in the
  weekly job); `inspect` names it; no feature needed, so an `fst`-only build has it too.
  `bench/results/dict-2026-09-10-arz-64b0d35.txt`.
- **Arrow columns as batch input** (Python): `ids_of_arrow(column)` and `ids_into_arrow(column,
  out)` on every index take an Arrow `utf8`/`large_utf8` column — a pyarrow `Array` or
  `ChunkedArray`, a pandas `ArrowDtype` column, a polars `Series` — and read the keys straight
  from its offset and data buffers, so no Python string is built or borrowed per key. Packed like
  `ids_of_bytes`, `MISSING_ID` for a null (`ClosedHashIndex` raises on one). Through the buffer
  protocol, not the C Data Interface: no dependency, no `unsafe`, one copy of the buffers per
  call. On the shuffled dictionary, per key: `CompactHashIndex` 48 ns against 145 (`ids_of`) and
  105 (`ids_of_bytes`), `PerfectHashIndex` 55 against 153 / 121, `ClosedHashIndex` 20 against
  116 / 87, `StringIndex` 264 against 468 / 435.
  `bench/results/arrow-2026-09-10-arz-b6ae463.txt`.
- **`ClosedHashIndex`** (Python: `ClosedHashIndex`): the minimal perfect hash and nothing else,
  for a vocabulary known to be closed. `id(key) -> u32`, no `Option` -- a member's id, and for any
  other string some id in `[0, n)`, which is what a perfect hash answers and all this index
  promises; `ids_of`, `ids_of_bytes` and `ids_into` alongside, and no `contains`, `[]` or `in`.
  The same hash as `CompactHashIndex` over the same keys, so the ids agree with its
  `id_unchecked`; without the fingerprint table the blob is **0.26 bytes per key** on the
  480 k-word dictionary, a fifth of the smallest fingerprinted index, and a lookup is one
  perfect-hash probe with no compare behind it: 40 ns against 68 for `CompactHashIndex::id`
  over the shuffled dictionary, batched `ids_of` 14 against 28 (`local/closedbench`, nine
  alternated rounds in one process, minimum). A new type rather than `fingerprint_bits = 0`,
  because a membership check that always says yes would be a signature that lies. Blob magic
  `BCL1`; `inspect` names it (`BlobKind::ClosedHashIndex`); a `parse_closed` fuzz target covers
  its framing. No `load_mmap`: the whole blob is the perfect hash, read into memory either way.
- **`CompactHashIndex::build_to_file`** and `build_bits_to_file` (Python:
  `CompactHashIndex.build_to_file(items, path, fingerprint_bytes=1, *, fingerprint_bits=None)`):
  the constructor for a corpus that does not fit in memory, written straight to a file. One pass
  hashes the keys to their 16-byte pairs, sorted in 256 MiB runs spilled beside the output and
  merged; the perfect hash is built from the merged file one first-level chunk at a time -- the
  first level's chunks are now pulled from a feed, a slice or a sequential reader, so the
  in-memory build runs the same placement and the file is byte for byte what `build` and `save`
  write -- and the fingerprints are written at their slots, in memory under 256 MiB of table and
  through range files past it. Measured at 100 M real-word pairs, one index per process:
  **302 MB peak against 8 834 MB** for the same keys handed to `build` as a list (254 against
  903 at 10 M); the streamed build took 52 s, 36 s of it generating the keys, against 10 s for
  the list build with its keys already made. At 10⁹ keys, where the list would need about 90 GB,
  the streamed build peaks at **0.94 GB** — the perfect hash's own construction, 0.9 bytes per
  key, 0.6 of them the table being built and the keys its first level bumped — and takes ten
  minutes, six of them the generator. Returns the number of distinct keys; an iterable
  that raises aborts the build with the target untouched; nothing is left beside the output on
  any exit path.
- **`PerfectHashIndex::build_with_fingerprints`**, and `build_to_file_with_fingerprints` (Python:
  `PerfectHashIndex(keys, fingerprints=True)`, `build_to_file(..., fingerprints=True)`,
  `has_fingerprints()`): one more byte per key -- a fingerprint from the second hash, stored
  behind each block's offsets -- so a lookup of an **absent** key stops after one cache miss
  instead of two, the key itself never read, 255 times in 256. For a workload that is mostly
  misses: a stop list, a block list, a "seen before" check. Measured on the 480 k-word
  dictionary, one index per process, six alternations, minimum: an absent probe 166 → 74 ns, a
  member 163 → 171; batched `ids_of` over absent keys 70 → 47; the index 10.90 → 11.90 B/key. Ids
  are unchanged -- it is the same perfect hash -- and an `Overlay` compaction keeps the
  fingerprints (`OverlayKeys::rebuild_like`, with a default). The magic is `BMP7` either way: the arena tag
  carries the bit, so the format needs no new name for it.
- **Path forms of the strict loader, and checked forms of the mapping.** `StringIndex::load_untrusted`
  is `from_untrusted_bytes` over a file (Python: `StringIndex.load_untrusted`, and
  `Overlay.load_untrusted(path, base)`). `load_mmap_verified`, on all three indexes, is `load_mmap`
  plus the payload checksum `load` makes -- one pass over the mapping at load, pages still shared,
  nothing copied -- for a file you wrote but did not carry yourself. `load_mmap_untrusted`, on
  `StringIndex`, runs the full validation over the mapping, for a stranger's file too large to copy.
  All three mapped forms carry `load_mmap`'s obligation, and the two checked ones say why it weighs
  more for them: the check trusts what it saw once, so map a copy you own. `docs/usage.md` has the
  loader matrix. The stubs no longer say a mapped hash index is "validated as in `from_bytes`": its
  header is, its payload checksum is skipped by design.
- **`Overlay::compact_to_file(path)` and `compact_with_remap()`.** The first writes the rebuilt
  base straight to a file, streaming the live keys from where the base holds them -- `StringIndex`
  merges its ordered iterator with the sorted additions into `build_sorted_to_file`,
  `PerfectHashIndex` replays them into `build_to_file` -- so a base too large to hold twice can
  still be compacted; the second is `compact` plus the old-id → new-id table (`u64::MAX` for a
  retired id). Behind them, `OverlayKeys::rebuild_to_file`, with a default that materialises and
  rebuilds. Python: `Overlay.compact_to_file(path)` and `compact_with_remap()`, the table as
  native-endian `uint64` bytes.
- **`bench/mphf_vs`**, a one-process harness that builds lexindex's perfect hash, `ph` 0.11's PHast
  and PHast+, and `ptr_hash` 2.1's three parameter sets over the same keys and looks them up in the
  same order. Its own crate outside the workspace, so the competitors are not dependencies of
  lexindex; behind it, `Mphf` is exported -- hidden, and only under the `bench-mphf` feature.
- **Python `ids_into(keys, out)`** on all three index types: `ids_of_bytes` written into a buffer
  the caller owns -- a `numpy` array, an `array.array`, a writable `memoryview` -- so a hot loop can
  reuse one allocation. A read-only, strided or wrongly typed buffer is a `BufferError`, one shorter
  than `keys` a `ValueError`.
- **`inspect(bytes)` and `inspect_file(path)`** (Python: `inspect(path | bytes)`): what a blob
  is, from its header alone -- the kind, the format, the key count, and the sizes a caller would
  otherwise have to load it to learn: the perfect hash's region, the key arena or fingerprint
  table, the side table, an overlay's additions and retired ids with its base inspected in turn.
  Over a path only the header and the footer are read, so an index of gigabytes inspects in
  microseconds; nothing is decoded or verified, and a blob from before 1.0 is an error naming the
  type to rebuild. `BlobInfo`, `BlobKind` and `OverlayInfo` are `#[non_exhaustive]`, so a field
  can be added later without a major.
- **`StringIndex::build_to_file(items, path)`** (Python: `StringIndex.build_to_file`): `build` for
  a corpus that does not fit in memory. The keys come in any order; every 256 MiB of them is sorted
  and deduplicated in memory and spilled as one run beside the output, and the runs are merged --
  one buffered reader each -- into `build_sorted_to_file`, so the blob is byte for byte what `build`
  then `save` writes. Peak memory is one run plus a 1 MiB buffer per run, whatever the key count;
  the transient disk is the distinct keys once, removed on every exit path. A run is an arena of
  key bytes and a span per key, not a `String` each, so the budget is what it says. Measured at
  100 M real-word pairs: 414 MB peak against 11 985 MB for `build` over the same generator, 150 s
  against 211, blobs identical.

### Removed

- `IndexError::Serde`. Nothing had constructed it since 1.0 replaced the `epserde` loader; a
  malformed perfect-hash blob is `IndexError::Format`. Removed outright rather than deprecated
  first: the key hash above already makes this release a major one.

### Fixed

- **`StringIndex::from_untrusted_bytes` costs the graph, not the language, and refuses a key that
  is not UTF-8.** The 1.1.0 validator streamed every key to check its rank, so a blob packing an
  astronomical language into a few hundred bytes -- `fst` documents a billion strings in 896 -- was
  a denial of service against the one loader meant for a stranger's bytes; and it never looked at
  the bytes it streamed, so a transducer over non-UTF-8 keys, which `fst` permits and this crate's
  builder never writes, loaded and then answered `None` from `key(id)` for a live id. The validator
  now checks the transducer as a graph, in time proportional to its nodes and transitions: every
  reachable node once, transitions pointing strictly below their node and in increasing byte
  order, every accepted path valid UTF-8 -- the set of decoder states each node is reachable in is
  propagated parents-first, so a node shared between a character boundary and the inside of a
  character is caught -- and outputs that are ranks by construction: a final node carries none,
  each transition carries the count of keys its node spells before it, and the root's count must
  be the footer's length. On the 479 823-word dictionary it costs 22.9 ms against
  0.7 ms for the owned load, down from 48.1 ms; on a 65 536-key transducer of a few dozen
  nodes it costs microseconds. The `parse_string` fuzz target now also asserts that `key(rank)`
  decodes back to the scanned key, and a property test builds raw `fst` maps over arbitrary bytes
  and requires the loader to accept exactly the UTF-8 ones.

- **`PerfectHashIndex::build_to_file` could publish a file whose keys were not the source's.** Pass
  two checked each replayed key by its slot hash and length only, so a source that replayed two
  keys sharing a 64-bit hash at equal length -- `hash::COLLIDING_PAIR` is such a pair -- in the
  other order, or one in place of the other, was accepted, and the file answered each with the
  other's id. Both passes now fold a second, independent 64-bit hash of every key into an ordered
  digest that is compared before the rename; a replay that differs in any key or any position is
  refused with `IndexError::Build` and the target is left untouched.

### Changed

- **The streamed build's merge workers share one handle per run**: each reads its range of every
  run at its own offset (`pread`; `seek_read` on Windows), so the workers are as many as the
  ranges however many runs there are, where a budget of open readers had held them to three at
  10^9 keys (60 runs). Byte-identical (blob digests at 10 M and 10^8 keys), same peak. 10^9
  real-word pairs: merge 25–27 → 20 s (I/O-bound from there: 16 GB read and 16 GB written), the
  whole build **457 → 438 s**; at 3·10^8 (18 runs, eight workers before as after) 3.5 s either way.
  `bench/results/peak-compact-stream-pread-2026-09-10-arz-5678a0f.txt`.
- **Both hash builds compute their slots on every thread, and a fingerprint of a shipped width is
  one store**: once the perfect hash is built, each thread overwrites its share of the sorted
  pairs' hashes with their slots — the order is the hash's, so a chunk's first pair continues
  the one before it exactly when the hashes are equal — and one pass then writes the fingerprints
  with the row a later key needs pulled into cache. `write_fp` at 8, 16 and 32 bits is a single
  store instead of a `memcpy` call whose length is only known at run time; that call was 2.4 ns
  of every key placed, in the streamed build's output and fingerprint pass as much as here.
  Byte-identical (blob digests at 10 M and 10^8 keys), same peak. 10 M real-word keys in memory:
  placement 102 → 16 + 41 ms, the whole build **580 → 526–530 ms**.
  `bench/results/peak-compact-listed-place-2026-09-10-arz-8cb6f73.txt`.
- **The streamed build places its fingerprints on every thread when they go to range files**:
  the merge cuts as many ranges as the machine has threads, and the fingerprint pass then takes
  one merged segment per thread, staging
  its range records in a share of one allocation and writing them under each file's lock a
  stage at a time; a range record is the slot and the fingerprint at its own width, 5 bytes at
  the default instead of 12. The table that fits memory is still filled by one thread — below a
  byte, a row shares its byte with its neighbours. Byte-identical (blob digests at 10 M and
  10^8 keys). At 10^9 real-word pairs the fingerprint pass takes 12 s against 46, the output
  13.5 against 15, the whole build **457 s against 504** (six minutes of either being the
  example's key generator) at the same 943 MB peak; at 3·10^8 the pass takes 2.1 s against 10.3.
  `bench/results/peak-compact-stream-fp-2026-09-10-arz-3ddad05.txt`.
- **The streamed build merges its runs on every thread**: each run counts its pairs per
  top-bits bin as it is spilled; the merge cuts the hash space into ranges holding equal shares
  of the pairs, as many as threads, and merges each range out of every run into its own
  segment. The
  segments are read back as one stream decoded a buffer at a time rather than a record at a
  time, which is where the previous reader lost 5 ns a key behind the perfect hash's `dyn`
  iterator; and every buffer of the merge lives in one allocation released whole, so the
  build's high-water mark stays where it was. Byte-identical (blob digests at 10 M and 10^8
  keys). At 10^9 real-word pairs: merge 59 → 25 s, fingerprint pass 49 → 46 s, the whole build
  **548 → 504 s** with six minutes of either being the example's key generator, peak 942 → 943 MB;
  at 10^8, alternated twice: merge 4.2 → 1.3 s, total 46.4/47.4 → 43.7/43.6 s.
  `bench/results/peak-compact-stream-merge-2026-09-10-arz-b687f89.txt`.
- **Both hash builds sort their pairs on every thread, and the streamed build merges runs and places
  fingerprints with less work per pair**: a run — or the in-memory pair list — is partitioned in
  place by the top of the hash into one part per thread and each part is sorted on its own
  thread (the parts abut, so there is no merge and no second buffer); the run merge replaces the
  heap's top in place instead of popping and pushing, takes whole records straight out of its
  read buffer and writes a pair in one call, and a run is spilled a 64 KiB slab at a time rather
  than a pair per call (212 → 193 ms per 256 MiB run; the rest is the disk); the fingerprint
  pass sends its representatives
  through `index_all` a thousand at a time with each fingerprint row prefetched ahead.
  Byte-identical (blob digests at 10 M and 10^8 keys before and after). At 10^8 real-word pairs
  the streamed build's own share of a 51 s run — the rest is the example's key generator — falls
  from about 16 s to 12 s (run sort 0.63 → 0.31 s per 16.7 M pairs, merge 5.6 → 4.2 s, perfect
  hash 1.55 → 1.27 s, fingerprint pass 2.85 → 2.2 s); the in-memory `CompactHashIndex::build` at
  10 M takes **575–591 ms against 762–783** (alternated, two rounds), its memory unchanged.
  `bench/results/peak-compact-stream-time-2026-09-10-arz-0e177fd.txt`.
- **The perfect hash's construction holds a level's pieces only while the gaps beside them are
  placed**: a chunk's seeds go into the level table as it is placed, its occupancy map into the
  level's once both neighbouring gaps have read it, and the keys it bumped are listed in chunk
  order as chunks finish, instead of every piece staying alive until a merge that copied all
  three again; the next level groups by level hash alone and keeps its input by `retain`, the
  holes are yielded into their encoder rather than listed, and a table on its way to a file is
  written section by section instead of through a `to_bytes` copy. Byte-identical (golden
  fixtures, and blob digests at 10 M and 10^8 keys before and after). The streamed
  `CompactHashIndex` build at 10^9 real-word pairs peaks at **942 MB, down from 1 717** (0.9 bytes
  per key: 0.6 after the first level, 0.87 at the second level's grouping), in 587 s against 604;
  the in-memory build's time is unchanged. `bench/results/peak-compact-mem-2026-09-10-arz-71363e5.txt`.
- **`CompactHashIndex::build` and `ClosedHashIndex::build` feed the perfect hash straight from
  the sorted pairs**, a chunk at a time as the file build does, rather than extracting the
  representatives' hashes and staged fingerprints first. Same blob. At 10 M real-word pairs the
  build adds **21.5 bytes per key** above the key list, down from 25.0, and takes 774 ms against
  843 (alternated, three rounds); at 2 M the chunk buffers the feed keeps, one per thread, are
  still visible and the peak is a wash (25.7 / 25.4 / 29.9 bytes per key at 8 / 16 / 32
  fingerprint bits against 23.9 / 25.9 / 28.0). `bench/results/peak-compact-inmem-2026-09-10-arz-e3e910a.txt`.

- **An arena past 4 GiB keeps its blocked offsets.** The `u32` block base that could not reach
  the data past 4 GiB widens to `u64` (arena tags `0x31` / `0x32`, bit `0x20`) instead of the
  layout falling back to the flat `u64` table: 1.56 bytes per key of offsets instead of 8 -- 0.78 GB
  instead of 4 for 500 M ten-byte keys. The flat table remains only for a 256-key run past 64 KiB.
  Below 4 GiB nothing changes, byte for byte, beyond the magic the key hash above moves; a reader
  from before 2.0 refuses the wide-base tags as an unknown encoding, the same way it refuses the fingerprinted
  ones. The threshold is injectable in the tests, so both layouts are exercised on a 40-byte
  arena. `PerfectHashIndex::build` now refuses a key longer than `u32::MAX` bytes with the error
  `build_to_file` already gave, instead of laying out a truncated length.
- **One long key no longer widens the whole arena.** A block whose 16 keys outgrew its one-byte
  offsets used to move every block to two-byte offsets (0.7 bytes per key more), and a key past
  64 KiB the whole arena to the flat table (2.7 more). Now such a block keeps its place and
  stride, marks its first offset and puts its real offsets in an overflow table behind the data
  (arena tag bit `0x40`): 76 bytes for one 10 kB key among a million short ones. An arena no
  block of which overflows is unchanged byte for byte; a corpus of long keys still takes the
  wider blocks, and the flat table remains for a handful of keys none of which any block holds --
  once the one-byte blocks do not fit, the layout is the cheapest by the lengths. The file build
  writes the table behind the data too, and a reader from before 2.0 refuses the tag. Alongside,
  an absent probe under fingerprints is decided on the fingerprint byte before the slot's offsets
  are read, which took it from 91 to 75 ns: that path is about as long as the reorder buffer is
  wide, and its one cache miss overlaps the next probe's only while it stays that short.
- **The perfect hash looks up 20 % faster.** `MPH2` has had one seed family since 1.1.0, but every
  lookup still decoded the seed byte into a family and a shift and took the key's offset from a
  family-dependent run of its hash bits -- a variable shift on the hot path. The value is now
  `start + ((offset + stride × seed) mod slice)` and nothing else; the family machinery is gone from
  the builder too. Blobs are unchanged and the golden 1.1.0 fixtures are byte-identical: the same
  seeds, 2.089 bits/key. In the sweep harness, 10 M bigram hashes, A-B-A-B against 1.1.1's tree:
  lookup 34.4 → 27.5 ns, build 50.6 → 46.1 ns/key single-threaded; in `bench/mphf_vs` against
  PHast and PtrHash, lookup 33 → 28.8 ns and build 47.7 → 44.8 ns/key (8.1 → 7.6 on 8 threads).
- **`Overlay::save` streams.** The base goes to the file through the new
  `OverlayBase::write_base` -- a default through `base_to_bytes`, overridden by the three indexes to
  write from the bytes they already hold -- then the additions and the tombstones, and the header
  last over its place once the lengths and the payload hash are known. Saving an overlay over a
  mapped base of a gigabyte no longer makes two copies of it. `to_bytes` assembles the same
  sections into one `Vec`, sized by the new `OverlayBase::base_serialized_len`; both produce the
  bytes 1.1 wrote.
- The sizes the perfect hash derives from its load factors -- buckets per level, the tail's buckets
  and range, the held share at a run's start -- are integer arithmetic over ratios (`9/2`, `13/2`,
  `24/25`, a fixed-point `0.966`) rather than `f64`. The output is byte-identical, and the golden
  blobs say so; what changes is that the determinism promise no longer rests on every target
  rounding a division the same way, which IEEE 754 guarantees but no test could show.
- `bench/compare.py` records the competitors' installed versions (`marisa-trie`, `dawg2`,
  `datrie`) in its results file, next to lexindex's own.

### Documentation

- `docs/benchmarks.md` gains the perfect hash's head-to-head with PtrHash and PHast, re-measured
  after the lookup change: at 2.09 bits it builds 1.8× faster than the PHast+ it follows and 4.2×
  faster than PtrHash's compact set, with the fastest lookup of the three; regular PHast is smaller
  (1.92 bits) at 14× the build.
- `SECURITY.md` lists 1.1.x as the supported line and no longer says an `Overlay` answers arbitrary
  bytes with an `Err`: its own framing does, but an overlay over a `StringIndex` hands the base
  region to the base's loader and inherits that loader's exception unless loaded through
  `from_untrusted_bytes`.
- Contradictions removed: `StringIndex::from_bytes` pointed at no check for a stranger's blob
  while `from_untrusted_bytes` is that check; `build_to_file` said its file need not match
  `build` + `save` byte for byte while a test holds it to exactly that, short of a hash collision;
  `ids_of_bytes` said its buffer is native-endian "like the blobs", which are little-endian
  everywhere; `blob::hash_bytes` said it is the perfect hash's slot hash, which is
  `hash::hash_key`; the crate doc said the perfect hash implements PTHash's construction, which
  1.1.0 replaced with PHast's; the README's determinism note now says "within one lexindex
  version", as `docs/design.md` already did; the 1.1.0 entry below said `from_untrusted_bytes`
  had no Python binding, and it shipped one.

## [1.1.0] — 2026-09-09

### Fixed

- **`SECURITY.md` said every loader answers arbitrary bytes with an `Err`; `StringIndex` can panic
  instead.** The claim was true of the perfect-hash and overlay loaders and had never been true of
  the ordered one: `fst`'s node decoder is safe Rust but not total, and the checksum in front of it
  is public, so a blob crafted to carry a matching one reaches that decoder with an invalid body.
  A libFuzzer target measured it — 44 bytes in ten minutes, panicking inside the rank spot-check
  the load itself runs — and `docs/design.md` and the `from_bytes` docstring had said so since
  0.12.1 while the security policy still promised otherwise. The policy now states the exception,
  and names the four parsers a fuzz target actually covers rather than implying all five.

- **An `Overlay` stored every added key twice.** `add` owned each string once in a `Vec<String>`,
  so `key(id)` could index, and again as the key of a `HashMap<String, u64>`, so `id(key)` could
  hash. The additions now live once, as the length-prefixed records the blob format already uses,
  indexed by a map from the key's hash to the record's position — the layout the perfect-hash
  indexes have always had, down to the side list for the 64-bit collision that essentially never
  happens, pinned by a test over a real colliding pair since a branch guarding a 2.7e-8 event is
  otherwise never executed. Measured over a million ten-byte additions on a 100 000-word base with
  shuffled probes: the resident cost of holding an addition falls from 147 to 54 bytes, `add` runs
  about three times faster, and `id` on an addition is 278 ns against 295 before. Base keys are
  untouched, since the base answers first. The length sits inside the record rather than in a
  side table on purpose: a lookup is then the map and the bytes, two dependent accesses, and the
  version with a side table measured 40 % slower on shuffled probes.

- **`Overlay::key` could answer someone else's key on a 32-bit target.** An id above `usize::MAX`
  was narrowed with `as` before the bounds check, so it wrapped onto a real addition instead of
  returning `None`. Reachable only where `usize` is 32 bits and only for an id no `add` ever
  issued, which is why no test had caught it; the conversion is now `try_from`.

- **`PerfectHashIndex` was documented as the "fastest exact `string → dense id`" in seven places.**
  Its own benchmark says otherwise: `id` costs about what a `std::HashMap` lookup does, and the
  claim belongs to `id_unchecked`, which skips the membership comparison for a closed vocabulary.

### Changed

- **The perfect hash is rebuilt as PHast's map-or-bump, and its blob format moves to `MPH2`.**
  1.0's table was PtrHash-shaped — buckets placed largest-first, with the buckets in the way
  evicted and re-placed — and three quarters of its build was that eviction pass reading a random
  slot-owner table. The new builder never displaces anything: each bucket's one-byte seed slides
  its keys two values at a time along a 1024-value slice of the table, the seed whose values are
  lowest is taken, since low values are what the buckets still to come cannot use, and a bucket no
  seed places is bumped to a smaller table under a fresh hash, down to a tail of a few hundred
  keys placed exhaustively. Bumped keys reach the first table's holes through a rank bit vector
  over the lower tables' values and an Elias–Fano list of the holes, which is what keeps a bumped
  key at about eight bits instead of the sixteen a plain offset table costs. Buckets are placed in
  fixed chunks with the gaps between them placed serially afterwards, so the table is the same
  whatever the thread count, and within a chunk the buckets follow PHast's size-weighted priority
  scaled with the slice, which is what lets a small table place as well as a large one. Measured
  on 10 M real word-bigram hashes: **49 ns/key on one thread against 280** (85 ms on eight threads
  against 625), and **2.09 bits/key against 2.39** — `CompactHashIndex` at 1.26 B/key on the
  dictionary against 1.30; in-order lookups cost 4.2 ns/key. Small tables
  gain the most, and nobody had measured them before: 2.78 bits/key at 1 000 keys against 3.19,
  2.19 at 10 000 against 2.37. Every `BMP6` and `BCH6` blob written from here on carries an `MPH2`
  table, which 1.0 cannot read; the `MPH1` tables 1.0 wrote still load. The 1.0 hash fixtures are
  now held to what a reader must promise them, and 1.1 fixtures take over the byte-identity pin.

- **Both latency tables are re-measured on 1.1, and the Python one now says what it can support.**
  The Rust table resolves the arena change cleanly: `PerfectHashIndex::id` went 1.103× → 1.010× of
  `std::HashMap` (290 → 274 ns on a control that got 3.4 % *slower*) while four rows whose code did
  not move shifted by at most 2 % — `StringIndex`, `BTreeMap`, the FxHash map, and `id_unchecked`,
  which is flat at 0.285× → 0.288× precisely because it never reads the arena. Two independent
  12-run sessions agree on every lookup minimum within 3.1 %.

  The Python table cannot support a version claim at all, which took an A-B-B-A against the released
  1.0.0 wheel to establish: in one session every cell landed within 1.01–1.04× on `random` and
  0.98–1.09× on `words`, and the widest gaps belong to `dict` and `marisa-trie` — code identical in
  both runs. That retires two readings the cross-session numbers had seemed to support, that
  `PerfectHashIndex.id` gained 9 % and that `CompactHashIndex.ids_of` lost 37 % on absent pair-corpus
  probes; today's 1.0.0 wheel measures the latter cell at 62.0 ns against the 44.8 the 1.0 table
  published. The Rust-level win is real and simply does not survive ~180 ns of per-call binding
  overhead. `local/latency_py.py`'s publish gate moved from the absolute minima to the ratios for
  the same reason it exists: it had refused three tables whose ratios agreed to 1–8 %, with the
  `dict` column — the drift the ratio divides out — the worst offender in every one.

- **The scale table is re-measured on 1.1**, as the minimum of five runs per cell rather than the
  single run behind the 1.0 table, and the file it came from is committed. It is comparable cell by
  cell with the table it replaces, which the 1.0 one could not claim: `StringIndex`, whose build and
  lookup code has not changed since 0.5.1, holds within 2 % on both `list` rows and is therefore a
  usable control. Two cells moved on code 1.1 never touched — `CompactHashIndex` at 1 M reads
  0.26 → 0.22 s and 223 → 162 ns, which is the old single sample's noise leaving — and one moved the
  wrong way and is published rather than smoothed: 10 M `StringIndex` from a generator went
  6.5 → 7.7 s with five tight samples and an additive diff on that path. `datrie`'s size in the
  comparison table is 30.91 B/key, not the 30.69 measured for 1.0.

- **`PerfectHashIndex` addresses its keys in blocks: 13.62 → 10.94 bytes per key**, and writes
  `BMP6`. The arena's `n + 1` four-byte offsets are gone; sixteen slots now share a `u32` base and
  carry one-byte *cumulative* offsets after it, 21 bytes per block, so a key is
  `data[base + off[k] .. base + off[k + 1]]` — two adjacent reads out of one header, 1.31 bytes per
  key instead of 4. A corpus whose 16-key runs do not fit in 255 bytes gets 256-slot blocks with
  two-byte offsets (2.02 B/key; the 1 M random-bigram corpus is one, 23.95 → 21.97), and one that
  fits neither, or exceeds 4 GiB, keeps the flat table — which is also what removes the old 4 GiB
  cliff, since a `u32` base cannot reach past it. The encoding is chosen in the same pass that
  writes the data, from the lengths it saw, and recorded in the arena's tag byte.

  Reading did not get slower; it got faster, because the whole index is 1.3 MB smaller on the
  479 823-word dictionary and stays resident. Alternated A-B-A-B against the flat arena with
  `CompactHashIndex::id_unchecked` — same key hash, no arena — as the control: `id` **216 → 182 ns**
  and `key` **52 → 38**, with the control moving under 5 %. Batched `ids_of` was the one path that
  had to be paid for and the one that had to be fixed: a block header is 21 bytes, so its base and
  the offset pair regularly land in different cache lines, and prefetching only the base cost 9 %.
  Prefetching both puts it level. Build peak did not rise either — 55.1 MB against 56.3 — although
  the build now holds one transient `u32` per key, since a block's offsets are only final once the
  block is.

  Six layouts were measured before this one was written (`local/arenalayout`), and the obvious
  candidate lost: a length prefix in the data with a periodic sample costs +17 to +37 ns, because
  its "sequential" length reads are hops through the data region rather than through an array.
  Cumulative offsets are what removes the summing loop altogether.

- **`BMP5` blobs still load**, and are held to it by a test: `golden-1.0.0-perfect.bmp` now has to
  answer every key with the id a fresh build assigns, refuse every non-member and map zero-copy,
  while `golden-1.1.0-perfect.bmp` takes over the byte-identity assertion. The new magic exists so
  that a 1.0 reader refuses a 1.1 blob by name instead of failing inside the arena with "unknown
  offset encoding".

- **`build_to_file` now writes bytes identical to `build` + `save`**, and is tested that way rather
  than by behaviour: both derive the arena's encoding from the same key lengths in the same slot
  order, so the only way the two writers can disagree is a bug in one of them.

- The PyPI classifier is `Development Status :: 5 - Production/Stable`; 1.0 shipped as `4 - Beta`.

### Added

- **Benchmark results are files**, not terminal output pasted into a table. `bench/compare.py` and
  `bench/scale.py` write `bench/results/<table>-<date>-<host>-<commit>.json` carrying the CPU model,
  kernel, rustc, Python, lexindex version, the load average at both ends of the run, and the commit
  — suffixed `-dirty` when the tree was not clean, because a result measured on uncommitted code is
  not attributable to the commit it names. Per cell the file keeps the minimum of the repeats with
  the median and every raw sample beside it, so a run taken on a busy machine is visible in the file
  instead of averaged into it. The README tables cite the file that filled them.

- **`from_untrusted_bytes` in Python**, on `StringIndex` and on `Overlay`. The gap it closes is
  sharper there than in Rust: `pyo3_runtime.PanicException` derives from `BaseException`, so the
  panic a crafted `BIX4` blob raises slips straight past `except ValueError`. Both bindings are
  tested against the committed specimens — the 111-byte one for the index, and the same bytes
  sealed into a real `OVL2` frame for the overlay, which the Rust side writes and pins because both
  checksums are the crate's and a blob spliced together from Python would be refused by a hash long
  before its base region was read. The overlay test asserts that the ordinary loader *panics* on
  that fixture, so a binding wired to the wrong loader fails rather than passing quietly.
  `docs/usage.md` no longer says the binding does not exist.

- **An overlay is only as trustworthy as the base loader it is handed**, and the loader's docstring
  now says so instead of claiming that "every base loader since 1.0" is safe on arbitrary bytes.
  It is not: `StringIndex::from_bytes` may panic on a crafted transducer, and an `OVL2` frame
  passes every check it makes for itself before handing the base region over — so the 111-byte
  specimen sealed into an overlay panics through it, which a test now pins alongside the `Err` that
  `from_untrusted_bytes` returns for the same bytes. No new entry point: `from_bytes_with` already
  takes the loader as a closure, so the fix was to name the right one rather than to add a second
  door.

- **A sixth fuzz target, `parse_overlay_untrusted`**, parsing the embedded base as well as the
  frame — the seam neither existing target reaches, since the overlay derives the base region's
  bounds from its own header. `OVL1` is what makes it fuzzable: no checksum for a mutation to
  break. 8.5 M executions in three minutes locally, 674 edges, no crashes. Every fuzz job now runs
  its seed corpus with `-runs=0` *before* the timed run, because a target that dies on its own
  corpus reads exactly like a real finding — `parse_string` shipped that way once — and
  `fuzz/Cargo.toml` carries the rule that a target surviving a contained panic must replace
  libfuzzer-sys's aborting hook.

- **The byte formats are proved on a big-endian target**, in the weekly sanitizer workflow: Miri
  interprets `s390x-unknown-linux-gnu`, so no runner is needed, and `blob::`, `arena::` and a new
  `MPH1` fixture test run there in twelve seconds. Every scalar in every blob is little-endian by
  construction, but `to_ne_bytes` compiles, passes on x86 and would silently write a different file
  elsewhere — flipping the arena's count field to it passes `cargo test` and fails eight tests
  under the interpreter, which is how the job was checked rather than assumed. `mphf::` as a whole
  is out of reach there (its blob tests build a 263 000-key table, hours under interpretation), so
  the new test reads the committed fixture and writes it back byte for byte instead.

- **Dict-style lookup in Python**: `idx["apple"]` raises `KeyError` on a miss, `idx.get("apple")`
  returns `None` or whatever default is passed, on all four classes. Nothing further — no
  `__setitem__`, no `keys` / `values` / `items`, no `Mapping` registration — because these indexes
  are immutable `str -> int` lookups and registering as mappings would promise iteration semantics
  three of the four do not have. One consequence is pinned by a test rather than left to be
  discovered: defining `__getitem__` revives the legacy sequence protocol, so `list(index)` on the
  two hash indexes raises `TypeError` from the key type instead of iterating.

- **Every Python class pickles.** `__reduce__` on `StringIndex`, `PerfectHashIndex`,
  `CompactHashIndex` and `Overlay` names the class's own `from_bytes` and hands it the blob — and,
  for an overlay, the class of the base underneath, since `OVL2` records which base wrote it and its
  loader takes that class. An index therefore travels to a `multiprocessing` worker under `spawn`,
  which shares nothing, and the tests check exactly that rather than a same-process round trip.
  Pickling copies the bytes, a memory-mapped index included: a path would not survive a worker that
  cannot see the same filesystem, and a borrowed mapping would not survive the file changing. When
  both ends *can* see the file, `save` plus `load_mmap` still shares the pages instead of copying
  them, and `docs/usage.md` says so.

- **`StringIndex::from_untrusted_bytes`**, the loader for a blob someone else wrote. It walks the
  whole transducer instead of spot-checking it -- every reachable node decoded once, every
  transition required to point strictly below the node holding it, which is how `fst` lays nodes
  out and what makes the walk terminate on bytes that were not -- then streams every key and
  requires its value to be its rank, so a blob whose values are a *permutation* of the ranks is
  refused here and accepted by `from_bytes`. Decoding a malformed node still panics inside `fst`,
  because that decoder is the only one there is; the panic is caught at the load boundary and
  returned as `IndexError::Format`. Two caveats it documents: under `panic = "abort"` a crafted
  blob aborts rather than returning, and the rejection prints through the process-wide panic hook.
  `tests/data/panicking-1.0.0-string.bix` is a real 111-byte specimen, found by libFuzzer against
  `from_bytes` and kept so the security policy's exception stays a measured fact. The validation
  costs 50.8 ms against 1.2 ms for the owned load on the 479 823-word `/usr/share/dict/words`, or
  106 ns per key. In Python it is `StringIndex.from_untrusted_bytes`, and
  `Overlay.from_untrusted_bytes(data, StringIndex)` for an overlay wrapping one.

- **Order statistics on `StringIndex`**: `lower_bound`, `range_count`, `prefix_count` and
  `prefix_id_range`, in Rust and Python. Ids are lexicographic ranks, so keys sharing a prefix
  occupy a *contiguous* id range -- `prefix_id_range` hands back that interval, which turns a
  prefix into an array slice or a bitset window instead of a set of ids to test one at a time.
  None of them decodes a key, so counting three million matches costs what counting three does:
  420 ns for `prefix_id_range` on a three-byte prefix against 238 ns for `id` on a whole word,
  and 479 ns for `lower_bound`, on the 479 823-word `/usr/share/dict/words`. A property test
  checks all four against a sorted `Vec` searched by hand, on an alphabet with two-, three- and
  four-byte characters, since the ids follow byte order and a prefix bound is built by
  incrementing one.

- **A `parse_string` fuzz target**, replacing the one removed before 1.0 for re-finding a panic the
  crate could not then fix. It loads through `from_untrusted_bytes` and asserts the loaded index
  agrees with itself -- every key's id equals its rank, every point lookup agrees with the scan --
  so the target now fails on a wrong answer, not only on a crash. It is in the weekly matrix.

- **Feature badges on docs.rs**: the "Available on crate feature `mph`/`mmap` only" markers that say
  which parts of the API a `--no-default-features` build does not have.

## [1.0.0] — 2026-09-08

### Changed — breaking

- **The minimal perfect hash is now this crate's own, and every pre-1.0 hash-index blob is
  refused.**
  `PerfectHashIndex` and `CompactHashIndex` were built on `ptr_hash`, whose pilot table is read
  unchecked and whose bounding fields are private, so a blob holding one could not be validated
  from outside the crate that owned it. The new backend writes every array length into its own
  header (`MPH1`) and derives them again on load, which is what a loader needs to bound every read
  it makes. `BMP2`/`BMP3`/`BMP4` and `BCH1`–`BCH5` therefore cannot be read at all: each embeds an
  image the crate no longer links. They are refused with a message naming the version that wrote
  them; the migration is to rebuild from the key list.

- **`from_bytes` and `load` are safe fns on both hash indexes** — in Rust and in Python, where
  there was never an `unsafe` marker to warn anyone. A crafted blob is now a wrong answer rather
  than undefined behaviour. `load_mmap` stays `unsafe`, for the one obligation that is actually
  about mapping: do not modify the file under the map.

- **Ids are reproducible.** Construction is deterministic and independent of thread count, so the
  same key set produces the same blob byte for byte. `PerfectHashIndex::build`'s docstring
  previously promised the opposite, and `CompactHashIndex::build_bits`' still did — both say what
  is true now, and a property test over arbitrary key sets holds all three indexes to it.

- **The overlay is checksummed: new format `OVL2`.** `OVL1` was the one blob in this library with
  no integrity check of its own — the base carried one for its own region and the additions and
  tombstones carried none — so a flipped bit in an addition that stayed valid UTF-8 loaded as a
  different key, and one in a tombstone word revived a removed id, silently. `OVL2` puts every
  section length in the header, adds a header check and a hash over everything after it, and
  verifies both before reading any of it. `OVL1` blobs still load, without the checks that format
  does not carry; saving one again writes `OVL2` and it gains them. It is the only pre-1.0 format
  this version still reads — nothing about it was undecodable, so refusing it would have cost a
  rebuild for nothing.

- **Both key hashes read eight bytes at a time.** The byte-at-a-time FNV-1a chain that shipped
  through 0.12 cost one multiply per *byte*; the replacement costs one multiply-rotate per 8-byte
  word plus a splitmix64 finalizer with the key's length folded in. A-B-A-B in one process against
  the old implementation, with a control that touches the same keys: **1.5×** on a 9.3-byte
  dictionary word, **1.7×** on a 10.9-byte bigram, **6.2–6.6×** on an 80-byte URI-like key. Every
  hash value therefore changed, which is the second reason no pre-1.0 blob can be read. Held to the
  same statistics as the hash it replaces before shipping: strict-avalanche worst |z| 4.50 against
  4.75, low-bit chi-square over 480 k real words and 1 M bigrams within ±1, slot-against-fingerprint
  independence within ±1, and the false-positive rate re-measured at 2 M probes — 7 732 accepted
  against 7 812 expected at 8 fingerprint bits, z = −0.91.

- **New blob formats: `BMP5` and `BCH6`.** `BCH6` drops the `overflow_cap` field — it existed to
  bound `ptr_hash`'s unchecked remap, and the new one covers its whole slot range — so its header
  is 40 bytes rather than 48.

### Added

- **`cargo semver-checks` as a CI job, and a versioning policy that says what it cannot see.** It
  passed clean across this release: nothing in the public API shrank, and removing `unsafe` from a
  signature widens rather than breaks it. What makes 1.0 major is the on-disk format, which no API
  tool can inspect — so the policy in `docs/design.md` states that a blob format is part of the
  contract, and the CHANGELOG section above is the half of compatibility a human writes.

- **The 1.0 blobs are pinned by their bytes, not by invariants.** `golden-1.0.0-compact.bch` and
  `golden-1.0.0-perfect.bmp` are now asserted byte-identical to a fresh build from the same key
  list. No earlier release could do this: pre-1.0 ids were not reproducible across builds, so every
  golden test could only check the properties a correct load must satisfy. These two files are also
  what seeds the `parse_compact` and `parse_perfect` fuzz targets, and a seed that still parses but
  no longer resembles what the writer emits is the exact failure this release already hit once.

- **`SECURITY.md`, as a threat model rather than a form letter.** What the loaders guarantee
  (soundness, not correctness — a crafted blob answers wrong ids, never out-of-range ones), why
  `load_mmap` is the one `unsafe fn` and what its obligation is about, why the checksums are
  integrity and not authentication, and that the unseeded hashes make this no defence against an
  adversary who picks the keys. The README and `docs/design.md` link it rather than restate it.

- **A blob compatibility policy** (`docs/design.md`): the magic table, and the rule that a refusal
  must name the version that wrote the file. `OVL1` is read where `BMP*`/`BCH*` are refused because
  it is decodable and they are not — compatibility is broken where it cannot be kept, not where
  keeping it is inconvenient.

- **The whole library builds on 32-bit targets, `wasm32` included — `mph` too.** Two things had kept
  the hash indexes off them: `ptr_hash` pulled in `sucds`, which refuses any other pointer width,
  and the MPH's own `u64 → usize` narrowings had never been audited. The dependency left with the
  backend in this release, and the audit found one narrowing that mattered — the remap's entry count
  in `Mphf::from_bytes`, where a fabricated `slots` would have truncated into a plausible length
  instead of failing. Everything else on a load path was already `try_from`. The `compile_error!`
  that explained the restriction is gone, and CI cross-checks `i686` and `wasm32`. Leave `mmap` off
  on `wasm32`: there is nothing there to memory-map.

- **A libFuzzer target over the MPH's own blob (`parse_mphf`), and a seed for it.** The two index
  targets reach that format only behind their own header, where a mutation has to keep two
  checksums and a length identity intact first — so in practice they fuzz the framing and never the
  body. The new target starts inside it, and asserts more than "does not crash": every id it gets
  back must land in `[0, n)`, which is the property that makes `from_bytes` a safe fn.

### Fixed

- **The fuzz seed corpus had gone stale, and nothing said so.** 1.0's blob formats made every
  committed seed a blob refused on its magic, so all three fuzz targets would have explored one
  branch and reported a clean run. The test that exists to catch exactly this is behind the
  `fuzzing` feature, which no CI job built — it does now, along with a clippy pass over the same
  feature, and the seeds are current.

### Removed

- **`ptr_hash` and `epserde`, and 129 crates with them.** The `mph` feature now has no dependency
  at all; the crate's whole tree is `fst` plus `memmap2`. `cargo audit` goes from three allowed
  warnings to none — every one came from that tree.

### Changed

- The blob-integrity primitives — the header hash and the streaming payload hash — moved from the
  `mph`-gated `hash` module to `blob`, which every format shares. `StringIndex`-only builds could
  not reach them before, which is why the overlay had none.

- Loading a `PerfectHashIndex` no longer hashes every key in the arena. That pass existed to
  recompute `overflow_cap` on each load and was O(n) on the blob's size.
- `CompactHashIndex` costs 1.30 B/key at the 8-bit default and 0.80 at 4 bits, against 1.27 and
  0.77 on the old backend: the in-crate MPH is 2.390 bits/key where `ptr_hash` was 2.169. Measured
  on `/usr/share/dict/words` (479 823 keys).

- **Every timed table in the README was re-measured on 1.0**, in one idle session after the last
  code change. Three findings, and one non-finding stated as such:

  - `PerfectHashIndex::id_unchecked` went from 0.451× to **0.287×** of `std::HashMap` — 111 → 75 ns
    at 1 M bigrams, on a control that got 6.7% *slower*. Against a `HashMap` with FxHash it is now
    ~2× faster (75 against 156 ns), where through 0.12 this README said the latency advantage
    against a fast-hashed map was gone. That claim is retracted; the perfect hash and the 8-byte
    key hash are what changed, not the conditions.
  - **Builds cost more.** `CompactHashIndex` at 1 M went 114 → 175 ms and `PerfectHashIndex`
    291 → 378 ms: the in-crate backend builds 1.5–1.8× slower than `ptr_hash` did, which was
    accepted as the price of a header a loader can check. At 10 M the picture reverses in the
    scale table, because that build is eight-threaded and the older session was core-contended.
  - From Python, `PerfectHashIndex` moved 1.04× where `marisa-trie`, `StringIndex` and
    `CompactHashIndex` all moved 1.18–1.20× on the same corpus — a ~14% gain relative to
    everything else in the room.
  - The **`dict` denominator moved again** and the Python table is therefore not comparable cell by
    cell with the one it replaces: 327.9 ns then, 258.5 now, on identical keys and untouched code.
    Its absolute values are now printed in the table so the next session can see the denominator
    rather than guess at it. The mechanism remains unidentified after having been chased once
    already.

## [0.12.1] — 2026-09-07

### Fixed

- **A crafted overlay blob could panic a safe loader.** `Overlay::from_bytes_with` read the
  tombstone word count out of the blob and computed `count * 8` on it unchecked. A 40-byte blob
  claiming `1 << 61` words panicked with an arithmetic overflow in a debug build; in release the
  product wrapped to zero, the length identity `bytes.len() - at == count * 8` then *passed*, and
  the parser went on to collect `2^61` elements — a capacity-overflow panic where a clean
  `IndexError::Format` was owed. Reproduced in both profiles before the fix, and both regression
  tests fail against the old parser.

  Every count the blob carries is now narrowed with `usize::try_from` and combined with
  `checked_mul` / `checked_add`, and the tombstones are read from the byte slice itself
  (`chunks_exact(8)`) after its length has been checked — so a number the blob merely claims can no
  longer size an allocation. The same `as usize` casts truncated `base_len` and the addition count
  on a 32-bit target, where `Overlay<StringIndex>` is available; `StringArena` already used
  `try_from` here and the overlay now matches it.

- **`Overlay::save` was not atomic.** It was `std::fs::write`, so a crash, a full disk or a kill
  mid-write left a truncated blob under the real name. It now goes through the same writer the three
  indexes use — temporary + `O_EXCL` + fsync + rename + directory fsync — which matters most here:
  the overlay is the crate's mutable layer, and so the file most often rewritten over a live one.

- **A crafted blob could make `len()` disagree with the keys.** Additions were checked against each
  other but never against the base, so a hand-made blob could add a key the base already held:
  `len()` counted it twice while `id()` could only ever answer the base's, leaving the addition's id
  unreachable. `add` consults the base first and revives its id rather than issuing a second one, so
  no blob this crate writes looks like that. The parser now rejects it over a base whose membership
  is exact — a new `OverlayBase::EXACT_MEMBERSHIP`, `true` for `StringIndex` and
  `PerfectHashIndex`. `CompactHashIndex` keeps the old behaviour, since its false positives would
  otherwise reject sound blobs, and the constant defaults to `false` so an outside implementation of
  the trait still compiles.

- **`PerfectHashIndex::build_to_file` verified pass two by hash alone.** The docs promised "a second
  pass that yields different keys is refused", which a 64-bit hash cannot deliver on its own: an
  equal-hash key of a different length reached `copy_from_slice` with mismatched lengths — a panic —
  on the direct path, and wrote a short record that desynchronised the spill's framing on the
  windowed one. Both paths now share one `replay_span`, which checks the slot's length as well. The
  length is free (pass one sized the slot from it); a second hash would have cost 8 B/key of build
  memory for an adversarial case, and was not worth it.

### Added

- **A libFuzzer target and a property test for the overlay's framing.** `parse_overlay` joins
  `parse_compact` and `parse_perfect`, and a seeded-mutation property test works outward from a real
  blob — one byte flipped, the blob cut short, or one of its three claimed counts replaced by a
  hostile value — since random bytes never spell `OVL1`. Both deliberately stub out the base loader
  rather than re-parse the embedded blob: that is `StringIndex::from_bytes`'s own property, and
  fuzzing it here would only rediscover the `fst` node-decoder panic that `fuzz/Cargo.toml` already
  records as out of this crate's scope.

### Changed

- **`Overlay`'s Python loaders document whose trust contract they inherit.** `Overlay.from_bytes`
  and `Overlay.load` validate the overlay's own framing for every base, then hand the rest to the
  base class's loader — checked for `StringIndex`, trust-your-own-blob for the two hash indexes. The
  signature cannot say that and Python has no `unsafe` marker, so the docstrings and the stubs do,
  and a test asserts the sentence is still there.

- **The README's size claim is qualified to what was measured.** "The smallest ordered `string → id`
  index available in pure Rust" was a universal claim over crates.io that a benchmark of four crates
  cannot establish; it now reads "the smallest of the pure-Rust ordered indexes measured below".

## [0.12.0] — 2026-09-07

### Added

- **Free-threading audit, and the two fixes it turned up.** PyO3 0.29 declares
  `Py_MOD_GIL_NOT_USED` for a `#[pymodule]` by default, so the extension has been telling
  free-threaded CPython it does not need the GIL without anyone having checked. Measured on 3.14t
  with eight threads: sharing any of the three index types is sound — they are immutable after
  building and nothing raised — but sharing a `StringIndex` iterator or an `Overlay` failed in seven
  threads out of eight with `RuntimeError: Already borrowed`, PyO3's borrow flag refusing two
  simultaneous `&mut self` calls.

  Both are now `#[pyclass(frozen)]` with their mutable state behind a lock taken through PyO3's
  `lock_py_attached`, which detaches from the interpreter before blocking and so cannot deadlock
  against it. Sharing one of them serialises instead of raising: a shared iterator hands each key to
  exactly one thread, and concurrent `Overlay.add` calls each get their own id. `gil_used = false`
  is now spelled out in the source next to the reasoning that backs it, rather than left to a
  default. The suite runs on 3.14t as well as 3.14, and the two new concurrency tests fail against
  the previous build.

- **`ids_of_bytes` on all three Python index classes** — the batched `id` packed into a `bytes`
  buffer instead of a list. `ids_of` has to build one Python `int` per key, which for a batch headed
  straight into `numpy` is the whole cost; `np.frombuffer(buf, dtype=index.ID_DTYPE)` shares the
  memory instead. One native-endian fixed-width item per key, aligned with the input, with the new
  `MISSING_ID` class attribute where a key is absent — a buffer cannot carry `None`. `ID_DTYPE` and
  `MISSING_ID` are class attributes rather than one module constant because the width differs
  between the index types (`StringIndex` ids are 64-bit, the hash indexes' are 32-bit), so generic
  code can read them off the class. On the one index size where the sentinel would be a real id —
  exactly `u32::MAX + 1` keys — the method refuses instead of silently aliasing.

  Returned as `bytes` rather than as a buffer-protocol `#[pyclass]`. Both work under `abi3-py311`
  (`Py_bf_getbuffer` has been in the limited API since 3.11), so the choice is surface, not
  portability: `bytes` adds no public class and no exported-buffer lifetime to get wrong, and it is
  immutable, which is what an answer should be. What it costs is one `memcpy` of 4 bytes per key.

  Measured on 200 000 real-word keys, minimum of nine rounds, machine drift 3.0 % on a calibration
  loop with no memory traffic: `ids_of` **117.5 ns/key**, `ids_of_bytes` **87.9 ns/key** — a
  **1.34×** ratio, saving **29.6 ns/key**. That saving is the Python `int` construction and nothing
  else, which the decomposition shows rather than assumes: `arr.tolist()`, which builds the same
  `int`s from an array with no lookup at all, costs **31.6 ns/key** — an upper bound, since it also
  walks the array. The plan predicted 15–25 ns/key and understated it.

- **`Overlay<I>`** — edits on top of any of the three indexes without rebuilding them. All three are
  immutable by construction: an FST and a minimal perfect hash are both built once from the whole key
  set, so adding one key has always meant rebuilding for the whole corpus. An overlay wraps a base
  index with a map of keys added since and a bitset of ids retired from it, so `add` and `remove` are
  O(1) and the base is untouched. Ids span both in one `u64` space — base ids keep their values,
  additions continue above `base.len()` — and are stable: an id is never reissued, removal never
  renumbers anything, and re-adding a removed key revives its original id rather than issuing a
  second one for the same string. `compact()` rebuilds the base from the live keys when the additions
  have grown enough to be worth folding in, and is the one operation that renumbers.

  What a base can do is expressed in the type rather than in a runtime error: `key`, `keys` and
  `compact` need `OverlayKeys`, which `CompactHashIndex` cannot implement because it stores no keys,
  so an overlay over it offers membership and nothing else. Removal over a probabilistic base carries
  that base's false-positive rate — a `contains` that was never true of a real key can retire an id —
  which is documented on `remove` and is why the property test excludes it.

  The base may be shared: `OverlayBase` is implemented for `Arc<I>`, so an overlay can wrap an index
  the caller still holds and several overlays can sit on one base.

  A lookup costs the base plus one bitset probe, and that is what it measures: at 10 M keys, with the
  tombstone bitset fully allocated (1.25 MB), `Overlay<PerfectHashIndex>::id` is **363.0 ns** against
  the bare index's **354.9 ns** on the same deserialised structure — **1.023×**, +8.0 ns, minimum of
  nine A-B-A-B rounds with a machine drift of 1.5 %.

  `to_bytes`/`save` serialise the base blob, the additions and the tombstones together; `from_bytes_with`/
  `load_with` take the base's own loader as a closure. That is deliberate: `StringIndex::from_bytes`
  is safe while the perfect-hash loaders are `unsafe fn`, and a single generic loader would have had
  to be `unsafe` for all three to accommodate two of them. Neither `len` nor the live/dead split is
  stored — both are derived on load, so a blob cannot disagree with itself about how many keys it
  holds — and a malformed blob is rejected with a named `Format` error rather than half-loaded. The
  header records which base wrote the blob and the loader checks it *before* handing the bytes over,
  so a perfect-hash loader is never pointed at bytes written by something else.

  Exposed to Python as `lexindex.Overlay`, taking any of the three index classes. A `#[pyclass]`
  cannot be generic, so the base is a runtime tag there and `key`/`keys`/`compact` raise `TypeError`
  on a `CompactHashIndex` base where Rust refuses to compile. `Overlay.load(path, base)` and
  `Overlay.from_bytes(data, base)` take the base *class*, since each index loads itself; the base
  tag turns the wrong class into an error instead of an unchecked read.

### Fixed

- **The `grid` column of the Python lookup table did not reproduce, and is restated.** Re-measured
  one day after 0.11.0 published it, from two fresh 11-round passes agreeing to 4.3 % at worst and
  0.5 % at the median. The corrected figures: `marisa-trie` 3.08×/2.75× (was 2.50×/2.16×),
  `StringIndex.id` 1.73×/1.37× (was 1.41×/1.06×), `CompactHashIndex.id` 0.71×/0.41× (was
  0.61×/0.32×), its `ids_of` 0.45×/0.26× (was 0.38×/0.20×). `PerfectHashIndex.id` reproduced.
  The published ranges move with it: a missing key costs `CompactHashIndex` 0.33–0.41× a `dict`
  (was 0.32–0.36×), batched `ids_of` on members 0.31–0.45× (was 0.31–0.38×), and `StringIndex`
  1.37–2.61× (was 1.06–2.61×). The `words` and `random` columns reproduce to within 1 % and stand.

  Five runs on the new day — three round counts, idle and under load, on the released 0.11.0 wheel
  as well as the working tree — agree with each other and disagree with the old column by up to
  30 %. That rules out the extension, the interpreter, the corpus generator and background load.
  What moved is the `dict` denominator, 251 ns then against 194–196 ns now on identical keys; the
  mechanism is not identified and the README says so rather than guessing. The protocol defect is
  identified: the two passes that validated the old column ran minutes apart inside one session,
  which bounds within-session noise and nothing else, so the second pass now has to come from a
  separate session.

## [0.11.0] — 2026-09-06

### Added

- **`StringIndex::build_sorted` and `build_sorted_to_file`** — build from keys that are already in
  ascending order, without materialising them. `build` has to collect its input into a `Vec` before
  it can sort it, so a caller who already has the keys ordered (a sorted file, a database cursor, an
  external sort) pays for a second copy of the corpus; these stream straight into the transducer, and
  `build_sorted_to_file` streams the finished index to disk as well, so neither the corpus nor the
  index has to fit in memory. Measured with `examples/peak.rs` on the same word-bigram grid: at 10 M
  keys the streaming build peaks at **49.6 MB against `build`'s 721.9** (14.6×; 20.0 MB of build
  memory against 129.5 plus a 592 MB key list), and **100 M keys build with a 63.3 MB peak**, of
  which 29.6 MB is the loaded word list — 32× under the 2 GB the task set as its bar. The peaks
  reproduce to the tenth of a megabyte across runs; the wall time does not, and is left out for that
  reason (the same 100 M build took 52 s on an idle machine and 101 s under a load average of 7). Adjacent
  duplicates are dropped exactly as `build` drops them after sorting, so the two produce byte-identical
  blobs for the same key set, which is what a test asserts rather than a similarity check: ids are
  ranks, so any disagreement would renumber every key after the first difference. An input that is not
  ascending is refused by the transducer builder rather than producing an index that answers wrongly.

- **`PerfectHashIndex::build_to_file`** — builds straight to a file **without ever holding the
  keys**, for a corpus that does not fit in memory. Measured on the same key set both ways
  (`examples/peak.rs`): the process peaks at **471 MB at 10 M keys against 1 272 MB** for `build`
  handed a list of those keys, 87 against 146 at 1 M. Of the streamed build's 44.1 bytes per key,
  23.5 are the output file itself — the arena is filled through a mapping, so its pages stay
  resident until the kernel writes them back; the anonymous part is 20.6 bytes per key and flat in
  `n`, which is what the design predicts (eight for the hash, four for the length, eight for the
  sorted copy that looks for collisions).

  `source` is a **factory**, called twice, and that is the shape of the problem rather than an
  inconvenience: the arena stores keys in slot order, slot order needs the perfect hash, and the
  perfect hash needs every key's hash first. Pass one hashes and records lengths, the offset table
  follows from the lengths alone, pass two replays the corpus and writes each key into its place. A
  one-shot iterator cannot be passed by construction — the signature states the requirement instead
  of documenting it — and a source that replays different keys is refused rather than silently
  mis-built.

  **Keys must be distinct**, unlike `build`, which sorts and deduplicates. A repeated hash in pass
  one is either a duplicate key or a genuine 64-bit collision, and which one it is changes `n`, the
  tail ids and therefore every offset already computed. Requiring distinct keys makes a repeat a
  collision by definition; a duplicate is caught by comparing the two written entries, and the file
  is never published.

  **The arena is filled one 32 MB window at a time, through a spill file**, and the reason is worth
  a caller's attention because it costs disk space. Keys are stored in slot order, which is random
  with respect to file offset, so filling the arena in a single pass keeps the whole mapping dirty
  for the pass's whole duration — a 4 KB page holds ~178 keys at these lengths, so once the kernel
  starts writing pages back the pass re-dirties them and the build writes its own output dozens of
  times over. Measured that way on a real filesystem, page cache drained before every run, it cost
  2.7× the wall time from 5 M keys up and did not finish at 100 M at all: killed after fifty minutes
  at 18 % CPU, having written 187.7 GiB for a 2.34 GB file. Pass two therefore appends each key to
  the region of a sibling spill file belonging to the window its slot falls in, and each window is
  read back sequentially, filled inside itself and flushed.

  | keys | one pass | windowed | bytes written / output size |
  |---|---|---|---|
  | 5 M | 12.2–16.4 s | **4.2–5.2 s** | 55–75× → **1.00–1.01×** |
  | 10 M | 25.3–34.2 s | **8.5–8.6 s** | 55–91× → **1.00×** |
  | 20 M | 60.6–61.4 s | **17.7–17.9 s** | 68–72× → **1.00–1.01×** |
  | 100 M | did not finish in 50 min | **94.2–102.3 s** | 86× → **1.85×** |

  What identifies the write *order* as the cause, rather than the filesystem or the machine, is a
  control that holds everything else fixed — the same file, the same offsets, the same bytes, in
  sequential against scattered order: 1.0 s against 155–167 s, at 140–151× the bytes. On a RAM
  filesystem the same reordering costs 1.7× and no I/O at all. 32 MB is the largest window that held
  1.0× reproducibly; 64 MB measured 1.4–2.3× and 128 MB 2.9–23×. Re-reading the source once per
  window instead of spilling was measured and rejected: one pass over a 100 M source costs 32.8 s,
  and a 2.3 GB arena needs 74 windows.

  The **cost is transient disk space** — about 2.2× the output size has to be free in the target
  directory until the build finishes, so a build can fail with `ENOSPC` on a volume sized exactly
  for its output. Below roughly 20 M keys the spill never reaches the disk at all, being written and
  consumed within seconds; an arena that fits in one window skips the spill entirely. The times are
  *below* what the single-pass fill measured writing into a RAM filesystem (4.2 / 8.5 / 17.7 against
  5.2 / 10.7 / 22.6), because filling one window at a time is easier on the TLB as well as the disk.

  `StringIndex::build_sorted_to_file` never had the problem — it writes in ascending key order
  through a buffered writer, measured at 1.02× at 100 M on the same filesystem the same day — and
  `CompactHashIndex` has no arena to fill.

- **`StringIndex.from_sorted` and `StringIndex.build_sorted_to_file` in Python**, the binding for
  the sorted streaming builds above. This is where the case is strongest: a Python list of 10 M keys
  is 1 076 MB of `str` objects before any index exists, so the caller who most needs a build that
  never materialises its input was the one who could not reach it. Handed a generator, the streamed
  build peaks at **11.6 MB against the list build's 1053.4** at 10 M (91×) and 4.2 against 105.4 at
  1 M, measured with `VmHWM` reset after the corpus loader so the baseline is the steady state.

  A Python iterable that raises halfway is indistinguishable, from Rust, from one that ended, so
  `build_sorted_to_file` asks a caller-supplied check **inside** the atomic write and before the
  rename that publishes the file. Without it a generator that blew up mid-corpus would have left a
  truncated but perfectly valid index at `path`, over whatever was there, with the exception
  arriving afterwards. A test asserts the file is byte-identical to what it was before the failed
  build, and that no temporary is left behind.

- **`PerfectHashIndex.build_to_file(source, path)` in Python**, the binding for the streaming
  build above. `source` is a zero-argument callable returning an iterable and it is called twice —
  the factory shape the Rust signature enforces, spelled the Python way (`lambda: open(path)`).
  A Python exception in either pass surfaces as that exception with the file at `path` left
  byte-identical, through the same in-write check the sorted build uses — a deterministic failure
  would otherwise stop both passes at the same key, and the two passes would agree on a truncated
  index. A second pass that yields different keys, or a repeated key, is refused with
  `ValueError`; handing it the iterable instead of a factory is the likely mistake, so that is a
  `TypeError` naming the fix.

- **`StringIndex.iter_after`** (Rust) — `iter()` resumed after a cursor key, which is the one range
  the existing API could not express: `range_iter(lo, hi)` needs an upper bound and `prefix_iter("")`
  cannot skip. It exists because the Python iterator needs it, and it is public because a caller
  paginating a scan across requests needs exactly the same thing.

### Changed

- **The whole build table at 100 M keys**, which is the row the streamed perfect-hash build had to
  be fixed before anyone could take. One process per cell, on a real filesystem, page cache drained
  before every run:

  | build | keys | wall | peak RSS | blob |
  |---|---|---|---|---|
  | `PerfectHashIndex::build_to_file` | streamed | 94.2 / 97.6 / 102.3 s | 4 070–4 080 MB | 23.44 B/key |
  | `CompactHashIndex::build` (fp = 1) | list | 18.3 s | 8 371 MB | **1.27 B/key** |
  | `StringIndex::build_sorted_to_file`, word grid | streamed | 29.9 / 31.6 s | **62.6 MB** | 4.39 B/key |
  | `StringIndex::build_sorted_to_file`, sparse pairs | streamed | 92.9 / 99.4 s | 77.2 / 77.3 MB | 12.51 B/key |
  | the generator alone, building nothing | streamed | 32.6 / 33.0 s | 33.5 MB | — |

  Two design claims survive a tenfold extrapolation past the largest `n` previously measured.
  `CompactHashIndex` holds **1.27 bytes per key at 1 M, 10 M and 100 M**, to three digits, and the
  memory its build adds above the key list is 25.0 B/key against 25.7 at 10 M — 2.7 % *below*, where
  the claim was flatness within 10 %. The streamed transducer build peaks at **0.6 bytes per key** at
  100 M, half of which is the loaded word list.

  The two `StringIndex` corpora are listed separately on purpose: the 63.3 MB published for this
  build in 0.11 is the **word grid**, and the sparse-pair generator gives 77.2 MB and a 2.8× larger
  blob for the same `n`. Every trie number is a statement about its corpus, and a table that folded
  the two together would read as a contradiction.

  The `PerfectHashIndex` row is also a correctness result: 100 M keys built across 74 arena windows,
  then **every one of them checked** — `id` answers, `key(id)` returns the key, and the ids are
  exactly the dense range `[0, n)`.

  From Python, through the same `bench/scale.py` cell the published table uses, a `CompactHashIndex`
  built from a **generator** of 100 M keys takes **35.9 / 36.1 s at a 2 452 MB peak** — which
  retires the extrapolation the README's scale table used to end on (~35 s, ~3 GB) by measuring it.
  The point lookup does not move with `n`: 298–344 ns at 100 M against 302–330 at 10 M.

- **`docs/design.md` records that `ptr_hash` construction is not deterministic.** Two
  `PerfectHashIndex::build` calls over the same key set, in one process, serialise to different
  bytes. The index is equally correct and every key answers, but the ids are not stable across
  builds, so a blob cannot be checksummed against a rebuild and two nodes building the same corpus
  will not agree on ids. Found while writing a byte-identity test for `build_to_file` that could
  never have passed — byte-identity is not a property `build` itself has. Anything that needs stable
  ids must build once and distribute the blob.

  **`build_to_file`'s own documentation said the opposite** — that the file it writes is
  byte-identical to what `build` + `save` would have produced. It was written in this cycle and
  never shipped, so it is corrected here rather than listed as a fix. It now says what is true: the blob answers exactly the same, key for key, but the bytes and the ids
  differ. Two neighbouring claims that read as byte-identity are *correct* and were left alone:
  `to_bytes` against `save` (one index, two serialisation paths) and `StringIndex::build_sorted`
  against `build` (the transducer is deterministic, and a test asserts it).

- **The Python lookup table, owed since 0.10 and finally taken** (`local/latency_py.py`, new README
  section): `dict`, `marisa-trie` and all three indexes over three corpora and three member/miss
  mixes. `CompactHashIndex.id` answers a present key in **0.61–0.71×** a `dict`'s time and a missing
  one in **0.32–0.36×**; batched `ids_of` in 0.31–0.38× and 0.20–0.29×. `PerfectHashIndex.id`
  trades level on members (1.05–1.33×) and wins on misses (0.83–0.96×); `marisa-trie` costs
  1.85–4.29× and `StringIndex` 1.06–2.61×.

  **Three protocol defects had to be fixed before any of it was publishable, and the third
  invalidates the criterion the plan had been using.** The harness gated on `dict`'s round-to-round
  spread as its stability control, and that spread was 19–79 % while `StringIndex` sat at 7 % on the
  very same rounds — not machine noise, which moves every column alike, but `dict` itself: at these
  sizes its table straddles the L3 boundary (~16 MB at 480 k keys) where `CompactHashIndex` is
  0.6 MB and fully resident. The least stable object in the table had been made its control. Second,
  the forms were alternated in a fixed order, so whichever ran first paid to pull the probe list
  back into cache after the previous set displaced it; they are rotated now. Third and decisive,
  **an idle machine is not a quiet one**: a cache-resident integer loop touching no memory drifts
  13.7 % over twelve rounds, climbing monotonically as the CPU heats, so the "control spread under
  2 %" bar was unreachable here whatever the load average said, and four sessions of blaming a
  sibling process were half wrong. The estimator is now the minimum over rounds and the criterion is
  **agreement between two independent runs** — worst 7.3 %, median 1.0 % on the published numbers.
  A fourth would-be defect was caught by that criterion rather than by inspection: the first run of
  a corpus, minutes after a build, disagreed by up to 24 % on two cells, so a settling period is
  real and the run is excluded.

- **The ns/op table the 0.10 sessions could not take** (`local/latency/`): every lookup form of both
  hash indexes over four member/non-member mixes and two key layouts, `std::HashMap` as the
  stability control, all forms alternated inside each round. Two findings reached the README. First,
  **~90 ns of every published lookup number is reaching the probe key**: `examples/bench.rs` probes
  the original allocations in strided order, and handing the same index a contiguous probe list
  takes `PerfectHashIndex::id_unchecked` from 109 to **18 ns/op** at 1 M while `ids_of` does not move
  at all (39.5 scattered against 38.2 contiguous — the software prefetch does for scattered keys what
  the hardware does for contiguous ones). Second, **on misses `std::HashMap` wins until its table
  outgrows the cache** — 32 ns against `CompactHashIndex::id`'s 41 at 1 M, reversing to 105 against
  56 at 10 M. The harness also reproduces the README's own table from different code: at 1 M
  scattered it measures 125.6 / 261.8 / 108.9 / 237.2 ns against the published ~151 / ~273 / ~111 /
  ~246.

- **`ids_of` prefetches the key bytes, not just the tables it looks them up in** — **2.6×** on
  `CompactHashIndex` and **2.1×** on `PerfectHashIndex` over 10 M real-word bigrams, A-B-A-B against
  the unmodified tree with the per-key `id` loops as controls (both flat: 200 → 203 ns/key and
  427 → 416). Against that per-key loop the batch is now **2.9×** and **3.6×**, where it was 1.1×
  and 1.8×: batching a `CompactHashIndex` lookup used to buy essentially nothing.

  The missing prefetch was the first one. A batch's `String` headers are contiguous but their bytes
  are wherever the allocator put them, so the hashing pass was one dependent cache miss per key with
  nothing overlapping it, and the careful prefetching downstream was hiding the cheaper halves of
  the problem. The per-key `id` cannot make this prefetch at all — it has no next key to look at —
  which is what the batched form is *for*.

  **The number depends on where the caller's keys live, and the docs now say so.** Contiguous keys —
  a `Vec` built in probe order — are already visible to the hardware prefetcher: that batch runs at
  ~50 ns/key with or without the change (−6 % to +2 % across 1 M / 5 M / 10 M, no consistent
  direction), and the win there is the single FFI crossing. A 50/50 member/non-member batch measures
  the same as an all-member one, because what is being hidden is reaching the key at all.

- **`StringIndex.__iter__` (Python) streams the transducer instead of rank-walking every key** —
  **2.9× faster** over 1 M real-word bigrams (635 → 221 ns/key, three rounds each; the control,
  `id()` on the same index, held at 367 ns across both). It buffers 1 024 pairs per refill and resumes through
  `iter_after`, because a `#[pyclass]` cannot hold a stream that borrows the index across `__next__`.

  The plan asked for 10× and 2.9× is what the ceiling allows, which the measurement says rather than
  the estimate that set the bar: on the Rust side the same walk is **569 → 97 ns/key (5.9×)**, and
  the binding adds ~127 ns/key of CPython `str` and tuple construction that no iteration strategy
  removes — `prefix("")`, which builds the whole list in one call with no per-item `__next__` at all,
  costs *more* (399 ns/key) than the lazy iterator. So the rank-walk is gone entirely and what is
  left is object construction. Setting the bar as a round multiple, without a measured floor, is the
  same defect as the 2 GB bar in the entry above, from the other side.

### Fixed

- **The 0.10.0 build-memory figures were measured against a masked baseline and are corrected here,
  not silently swapped.** `examples/peak.rs` reports `VmHWM − (VmHWM at the moment the key list is
  ready)`, and `VmHWM` is a high-water mark: the transient of loading the word list and sorting the
  bigrams stayed in it, so every build had to climb past that transient before it registered. The
  example now resets the mark (`echo 5 > /proc/self/clear_refs`) once the key list is built. The
  understatement is a constant 11.0–11.8 MB in every cell — flat in `n` and in the structure, which
  is what identifies it as the corpus transient rather than anything about the builds. Re-measured
  with the corrected baseline on both sides (0.9.1 built from the same example), bytes per key above
  the key list:

  | build | published 0.10.0 | corrected |
  |---|---|---|
  | `PerfectHashIndex`, 2 M | 79.0 → 44.9 (−43 %) | 83.0 → **50.8** (−38.8 %) |
  | `PerfectHashIndex`, 10 M | 84.7 → 50.8 (−40 %) | 85.9 → **51.9** (−39.6 %) |
  | `CompactHashIndex` 8-bit, 2 M | 42.5 → 25.2 (−41 %) | 48.2 → **30.9** (−35.9 %) |
  | `CompactHashIndex` 8-bit, 10 M | 47.6 → 24.6 (−48 %) | 48.7 → **25.7** (−47.2 %) |
  | `CompactHashIndex` 16-bit, 2 M | → 27.3 | 48.2 → **33.0** |
  | `CompactHashIndex` 32-bit, 2 M | → 29.2 | 48.2 → **34.9** |

  The *absolute* savings are unchanged, because the constant cancels in a difference: 340 MB off a
  10 M `PerfectHashIndex` build (published 338) and 230 MB off a 10 M `CompactHashIndex` build
  (published 230). What moves is every per-key figure and therefore every percentage, by 1–5 points.
  `docs/design.md` and the `CompactHashIndex::build_bits` docstring carry the corrected numbers; the
  0.10.0 section below is left as it was published. One 0.10.0 claim gains rather than loses: the
  build peak was said to rise with the fingerprint width, and on the corrected baseline the 0.9.1
  build does not move with it at all (48.2 B/key at 8, 16 and 32 bits), so the width-proportional
  staging is visible only in the new code.

- **The README's `dict` build cost had the same defect from the Python side** — `local/positioning.py`
  took its baseline right after `_random_pairs` returned, while the dict that generator uses to
  deduplicate was still counted in the high-water mark. It resets the mark too now: a `dict` costs
  **71–95 bytes per key** above the key list on the three corpora in the README's table, not the
  35–96 the range said, and 58–60 at 10 M.

## [0.10.0] — 2026-09-04

### Added

- **Golden blobs from every published format** (`tests/data/`, regenerated by `local/gen_golden.py`
  from the PyPI wheels of 0.5.1, 0.7.0, 0.8.0, 0.8.1 and 0.9.1) are loaded and queried by
  `tests/golden.rs`. Every other test in the repo writes a blob and reads it back with the same
  code, so all of them would keep passing if a dependency bump changed the `epserde` image embedded
  in an MPH blob. These cover `BMP2`/`BMP3`/`BMP4` and `BCH1`/`BCH3`/`BCH4`/`BCH5`, including the
  documented *refusal* of a 0.5.1 `BCH1`.
- **`examples/peak.rs`** reports the peak resident memory (`VmHWM`) and wall time of a single build,
  one index per process, on the same real-word bigrams `examples/bench.rs` uses. The build-memory
  numbers below were measured with it.
- **`examples/bench.rs` gained an FxHash baseline.** `std::collections::HashMap` is a SipHash map,
  which is the wrong single number to compare a perfect hash against; the same map with the
  `rustc`/Firefox hash (written out in the example, not added as a dependency) is the honest one.
- **libFuzzer targets for the blob framing parsers** (`fuzz/`, run weekly by `sanitize.yml`,
  seeded from `tests/data/`). They cover the safe half of the MPH loaders — the code that validates
  magic, versions, lengths, both checksums, the collision side table, the fingerprint width and the
  arena's offset table before anything unsafe happens, which is the only place untrusted bytes reach
  code this crate owns. 29.0 M and 37.5 M executions with no crash on the first run.
- **A positioning section in the README: which index to pick, and how much the corpus decides it.**
  Five structures — a bare `ptr_hash` MPHF, `CompactHashIndex`, `marisa-trie`, `StringIndex`,
  `PerfectHashIndex` — over three corpora built from the same word list, at 479 823 / 1 M / 10 M keys.
  The structures that store no keys are flat at 1.27 and 0.27 bytes per key on every corpus; the ones
  that do swing 3–8× (`marisa` 2.12 to 6.21, `StringIndex` 0.68 to 16.57), so the section says which
  corpus each number comes from and what shared key structure buys.
- **`bench/scale.py` measures a list and a generator separately.** Passing a list puts the corpus in
  the peak, which is what a caller holding the keys pays; passing a generator is what
  `CompactHashIndex`'s streaming build exists for and is the only way to see its own footprint.

### Changed

- **`StringIndex::from_bytes` no longer claims that checksum verification rules out a panic.** It
  guards accidental corruption, which is all a public, deterministic checksum can do; a blob crafted
  to carry a matching one reaches `fst`'s node decoder with an invalid body. A third fuzz target
  demonstrated it in ten minutes with 44 bytes that panic inside the load's own rank spot-check, and
  that target is deliberately not kept: `docs/design.md` already documented this as the accepted
  limit of the `StringIndex` trust boundary, so the defect was in the docstring, not in the code.
  `fst` is safe Rust throughout, so the worst case remains a panic or a wrong answer rather than an
  out-of-bounds read — which is why this loader stays safe while the perfect-hash ones are `unsafe`.

- **`PerfectHashIndex` builds in 43 % less memory** — 79.0 → 44.9 bytes per key above the input at
  2 M real-word bigrams, and 84.7 → 50.8 at 10 M, where it is **338 MB** off the peak of a single
  build. Four changes, none of which touch the blob: the slot table holds a `u32` key index instead
  of a 16-byte `Option<&str>`; the `(hash, index)` partition that existed to find hash collisions is
  gone from the common path, replaced by a sorted scratch copy that also feeds the perfect hash and
  is dropped before the arena is built (a real collision falls back to the old partition, on a
  `#[cold]` path); the arena is assembled in one exactly-sized buffer with its offsets written in
  place, instead of collecting the data and the offsets separately and concatenating them, which
  held two copies of the corpus across the concatenation; and the arena takes its two totals from
  the build's own hashing pass rather than re-deriving them in slot order, which measured 4 M extra
  data-TLB misses at 2 M keys. The build also retires 5.6 % fewer instructions and takes 11 % fewer
  page faults than 0.9.1 (`perf stat`, three runs).
- **`CompactHashIndex` builds in 41 % less memory** — 42.5 → 25.2 bytes per key above the input at
  the 8-bit default, and 47.6 → 24.6 at 10 M (**230 MB** off a single build). The representatives'
  hashes and their *truncated* fingerprints are lifted out of the `(hash, second hash)` pairs before
  the perfect hash is built, so the pairs are dropped first rather than sitting next to ptr_hash's
  construction memory alongside a full 64-bit fingerprint per key. The documentation claimed `16 · n`
  for the peak, which was the pairs alone: the measured high-water mark is 25.2 bytes per key at the
  8-bit default, rising with the fingerprint width (27.3 at 16 bits, 29.2 at 32), and the docs now
  say so.
- **README: "roughly twice as quick as `HashMap`" is now scoped to the SipHash map it was measured
  against.** `std::collections::HashMap` hashes with SipHash; the same map with FxHash measured
  196 / 200 ns against `PerfectHashIndex::id_unchecked`'s 216 / 216 in two independent 12-run
  sessions. Against a fast-hashed map lexindex's lookup advantage is gone, and the README says so —
  what remains is the footprint and the serialisable, memory-mappable blob. The scale table is
  regenerated on this code with both key sources; the streamed `CompactHashIndex` at 10 M peaks at
  **304 MB against 988** for the same build handed a list.
- **`ptr_hash` and `epserde` are bounded to a minor series (`~1.1`, `~0.8`).** Their layout is
  embedded verbatim in every MPH blob this crate has written, so a change there does not break a
  build — it stops old files from loading. Moving either is now a decision with a golden-blob test
  attached rather than something `cargo update` can do.
- **A 32-bit build with the `mph` feature now fails with one sentence naming the feature to turn
  off.** `ptr_hash` 1.1 depends on `sucds` unconditionally, and `sucds` `compile_error!`s on any
  pointer width other than 64, so the build used to die inside a transitive dependency with a
  dozen errors that named neither lexindex nor anything the caller could change — and a
  `compile_error!` of our own would never have been reached, because cargo compiles dependencies
  first. The minimal-perfect-hash dependencies are therefore declared under
  `[target.'cfg(target_pointer_width = "64")'.dependencies]`: off 64-bit they are simply not in
  the graph, the modules that name their types are not compiled, and the only diagnostic is ours.
  The dependency graph on 64-bit targets is unchanged (`Cargo.lock` untouched). The `fst`-only
  build has no such constraint — checked for `i686-unknown-linux-gnu` and
  `wasm32-unknown-unknown` — and README and the usage guide now say so.

## [0.9.1] — 2026-09-04

### Added

- **`serialized_len()`** on all three indexes: the byte length `to_bytes` / `save` would produce,
  without producing it (the minimal perfect hash is measured through a discarding sink).
- **`docs/usage.md` is compiled as doctests**, like the README, so its Rust block cannot drift from
  the API again — it had been calling `load_mmap` without `unsafe` since 0.9.0.

### Changed

- **The unsafe loaders are split into a safe framing parser and the unsafe `epserde` step.**
  `parse_frame` validates everything a query will trust — magic, header and payload checksums,
  lengths, the side table, the fingerprint range or the arena — on *any* bytes, and the property
  tests now fuzz exactly that half (plus every truncation of a real blob); the MPH region is
  deserialised only afterwards, behind the caller's contract. The proptests that called the unsafe
  loaders on arbitrary bytes are gone: they exercised what the contract excludes.
- **`StringIndex` owned loads spot-check the rank invariant**: the first key's value must be 0 and
  the rank-walk to `len - 1` must succeed. A full walk measured 0.7 → 40 ms on the 479 823-word
  dictionary (58× the owned load), so a *permutation* of the ranks is deliberately not detected —
  it cannot violate memory safety, and `key(id)` now uses checked arithmetic on the blob-supplied
  outputs, answering `None` instead of wrapping.
- **`UnicodeSubsequence` no longer allocates a per-byte rewind table**: the rewind target is found
  by walking back over UTF-8 continuation bytes, the same answer with no allocation per query.
- **Docs**: README's install snippet says `0.9` (0.9.0 shipped saying `0.8`; the release preflight
  now checks it); "never a full scan" and "`2^-b` by construction" scoped honestly in the usage
  guide and the site index, which no longer calls the core "from-scratch"; a note on Unicode
  semantics (scalar values; no normalisation, case folding or grapheme segmentation); the MPH
  `load_mmap` `# Safety` sections state the trusted-blob precondition alongside the immutability
  one; `bench/compare.py` shuffles its keys instead of handing every library the pre-sorted
  dictionary, and no longer claims to measure lookup latency; the `examples/bench.rs` probe stride
  is documented as prime (full coverage for every `n` below it), not merely odd.

### Fixed

- **Width-dependent casts**: the fingerprint-table length in `build_bits`, the arena's offset count
  and its per-key offsets are converted with `usize::try_from` instead of `as` casts, so a
  header-supplied value that does not fit the platform's `usize` fails cleanly instead of
  truncating (`mph_len` and the side-byte count already did). Defence in depth rather than a live
  bug: a 32-bit build of the `mph` feature does not compile today — `ptr_hash` pulls `sucds`, which
  refuses any target that is not 64-bit — and the `fst`-only build contains none of this code. The
  `fst`-only build is checked for `i686-unknown-linux-gnu`.
- **`release.yml`**: a `workflow_dispatch` run built neither wheels nor the sdist, because both
  depended on the tag-only preflight and a skipped dependency skips its dependants; they now run
  unless preflight actually failed. Tags are gated on the full CI matrix (fmt, clippy, tests on
  Linux/macOS/Windows, Python 3.11–3.14, MSRV, coverage) through `workflow_call`, and the native
  wheels are installed and exercised on their runner before anything is published.

## [0.9.0] — 2026-08-28

### Changed

- **`from_bytes` and `load` are now `unsafe fn` on `PerfectHashIndex` and `CompactHashIndex`**
  (**breaking**). A function that is unsound for some input belongs behind `unsafe fn`, and these
  accept arbitrary bytes: the blob framing is validated and checksummed — accidental corruption
  still fails cleanly — but the embedded minimal perfect hash is an `epserde` region whose pilot
  table `ptr_hash` reads unchecked, and the fields that would bound that read are private to
  `ptr_hash`, so no validation on this side can reject a crafted blob. Upstream reached the same
  conclusion independently: `epserde` 0.13 made `deserialize_full` an `unsafe fn`, and PtrHash
  declined a checked `try_index()` for the same reason. `StringIndex::from_bytes`/`load` stay safe —
  `fst` validates its own structure and guarantees invalid input cannot violate memory safety. The
  Python bindings keep their signatures and state the contract in their docstrings.
- **`load_mmap` is now an `unsafe fn`** on all three indexes (**breaking**). The mapped bytes are
  borrowed, not copied, so a write to the file from any process while the index is alive is
  undefined behaviour — which is exactly why `memmap2::Mmap::map` is itself `unsafe`. Wrapping it
  in a safe function hid a precondition that ordinary safe code (another handle writing the same
  path) can violate, so the obligation is now stated in a `# Safety` section and the call sites
  carry it. `load` is unchanged and remains safe. The Python `load_mmap` keeps its signature —
  Python has no way to express the obligation — and documents the same contract in its docstring.
- **`CompactHashIndex` hashes each key once instead of twice.** Both of its hashes are now produced
  by a single pass over the key's bytes, bit-for-bit identical to the two functions it replaces (a
  golden test asserts the equality on 2 000 keys, so no stored blob changes meaning). Measured in
  isolation: **21.3 → 18.2 ns/key** on real-word bigrams, **126 → 77 ns/key** on 80-byte URI-like
  keys.
- **The Python `CompactHashIndex` constructor releases the GIL while hashing.** Items are pulled in
  4 096-key chunks and hashed with the GIL dropped, so other Python threads keep running through
  what was previously one long GIL-held stretch; memory behaviour is unchanged (still 16 bytes per
  key, strings released as they are consumed).

### Fixed

- **`build_bits` rejects a bad `fingerprint_bits` before touching the iterator.** Since 0.8.1 the
  width was validated after the input had been collected, so an invalid width consumed (and hashed)
  the entire iterator first — and never returned at all for an endless one.

### Documentation

- **The README's Rust examples are now compiled and run as doctests.** They had never been checked
  by anything, so an API change could silently invalidate the first code a reader sees. A
  `#[cfg(doctest)]` item includes the file, which runs the snippets without pulling the prose into
  the API docs.
- The fingerprint's false-positive rate is described as a design rate from two *uncorrelated*
  hashes rather than as exact probability from *independent* ones, matching the more careful
  wording already in the code.
- **The point-lookup table is re-measured on this release's code** (min of 12 runs, idle machine,
  four seconds between runs). The whole session runs ~19% faster than the 0.8.0 one — the
  `std::HashMap` control moved with it — so the table now says which spreads were observed per row
  and warns that the ratio to `HashMap` is session-dependent (`id_unchecked` measured 2.2× here,
  1.7× before) rather than presenting it as a constant.

## [0.8.1] — 2026-08-27

### Fixed

- **`CompactHashIndex` no longer merges two distinct keys that collide in the 64-bit slot hash when
  their *truncated* fingerprints also tie.** The 0.8.0 build deduplicated on
  `(hash, fingerprint_bits-wide fingerprint)`, so at narrow widths a genuine hash collision had a
  `2^-fingerprint_bits` chance of silently collapsing into one id — the repository's own pinned
  collision pair reproduces it at 1 bit (`len() == 501` for 502 distinct keys). The collision side
  table now stores the **full 64-bit second hash** regardless of the table width (the build pairs
  were already 16 bytes, so build memory is unchanged), and the side probe runs *before* the
  fingerprint table, which could otherwise answer for a side key whose truncated bits tie its
  representative's. Two distinct keys now merge only by colliding in **both** 64-bit hashes at once
  (`≈ 2^-128` per pair), independent of `fingerprint_bits`. The blob magic moves to **`BCH5`** (same
  layout): a collision-free 0.8.0 `BCH4` is bit-identical and still loads, one holding a side table
  is refused with a message naming the rebuild — its truncated side fingerprints cannot be widened
  without the keys.
- **Side-table ids are validated on load.** Both perfect-hash loaders now require the side ids to be
  exactly the tail range `[m, n)` — structurally, not via the checksums, which vouch for transport
  rather than construction. A malformed blob could previously hand `CompactHashIndex::id()` an id at
  or past `len()`.
- **Header-length arithmetic is checked for 32-bit targets.** The side-table byte count is computed
  with `checked_mul` and `mph_len` converted with a checked cast, so a fabricated length fails
  cleanly instead of wrapping or truncating on a 32-bit build (the published wheels are 64-bit, but
  the crate is not).

### Changed

- **The Python `CompactHashIndex` constructor streams.** Items are hashed to their 16-byte pair one
  at a time as they come off the iterator (under the GIL) and each string is released immediately,
  so a generator-fed build no longer materialises the whole corpus on the binding side. The other
  two constructors still collect — those indexes store the keys.

## [0.8.0] — 2026-08-27

### Added

- **A 64-bit hash collision can no longer fail a build — any key set now builds.** Both perfect-hash
  indexes build the MPH over one representative per distinct hash value; colliding leftovers get
  tail ids served from a tiny **side table**, consulted only after the main lookup has already
  missed, so an index without collisions (every index below ~10^8 keys, in practice) pays one
  predictable branch and nothing more. `PerfectHashIndex` stays exact even for collided keys (the
  side probe compares stored keys); `CompactHashIndex` matches side keys by fingerprint, leaving
  only a pair colliding in *both* hashes at once (`2^-(64+fingerprint_bits)` per pair) to collapse
  as if it were a duplicate. Previously such key sets failed the build permanently — the hash is
  deterministic, so no retry could ever help; at 1 G keys that was a ~2.7 % build-failure rate.
- **Whole-payload checksum on the perfect-hash blobs.** Owned `load`/`from_bytes` now verify a
  streaming 64-bit hash over everything after the header, so a flipped byte anywhere — MPH region,
  arena, fingerprint table, side table — is rejected at load instead of perturbing answers (or
  aborting inside `epserde`) later. This makes the *accidental*-corruption story uniform across all
  three indexes: every owned load verifies the entire blob. (`load_mmap` still trusts the mapped
  file; a crafted blob remains outside the contract — the hashes are public and deterministic.)
- **`CompactHashIndex::build`/`build_bits` stream.** One pass keeps a 16-byte `(hash, fingerprint)`
  pair per key and never materialises the strings, so building from a lazy iterator peaks at
  `16 × n` bytes over the input regardless of key length — and sorting pairs instead of strings
  **halved build time** (249 → 120 ms at 1 M real-word bigrams, same-session A/B), putting it ~2×
  below `std::HashMap`'s build. Lookup cost is unchanged (A/B: 245.2 vs 245.2 ns);
  `PerfectHashIndex::id_unchecked` measured 8 % faster (193 → 178 ns) and the verified
  `PerfectHashIndex::id` ~3 % slower (343 → 353 ns — the side-table entry branch, on the one path
  that already pays a full key compare). Serialised sizes are unchanged to two decimals
  (+4/+12 bytes per *blob*, not per key).
- **Windows and macOS test jobs in CI.** The release workflow already shipped wheels for both;
  their platform-specific paths (`save`'s rename-over-existing, permissions, mmap) now run the
  test suite on every push, not just a cross-build on tags.

### Changed

- **Blob formats: `PerfectHashIndex` writes `BMP4`, `CompactHashIndex` writes `BCH4`** — the v3
  layouts plus the side table and the payload checksum; `BMP4` also *drops* the stored
  `overflow_cap`, which every load has recomputed from the arena since 0.7.1 anyway. 0.7 blobs
  (`BMP3`/`BCH3`, and `BMP2` from 0.5/0.6) still load; `BCH1`/`BCH2` remain refused. Older
  lexindex versions cannot read v4 blobs.
- **`save` streams.** All three indexes write the blob section by section through the atomic
  writer instead of assembling a serialised copy first, halving peak save memory; the bytes
  written are identical to `to_bytes`.

### Fixed

- **`save` no longer follows a symlink planted at its temporary path.** The atomic write opened its
  sibling temp with `File::create`, which follows an existing symlink — so an attacker able to write
  to the target directory could pre-create a symlink at the predictable `<name>.<pid>.<seq>.tmp` and
  redirect the write, truncating an arbitrary file the process could reach. The temp is now opened
  `O_CREAT | O_EXCL` (`create_new`), which refuses any pre-existing path, symlink included; on a name
  collision the write retries with the next counter (bounded, so a hostile racer cannot spin it).
  On Unix the parent directory is now fsynced after the rename so the publish is durable across power
  loss, and an existing file's permissions are preserved rather than reset to the umask default.
- **`StringIndex` owned loads verify the FST checksum.** `from_bytes` / `load` handed the body to
  `fst::Map::new`, which checks length and version but not the stored CRC, so a corrupt owned blob
  could load and only fail — or mislead — on a later query. Owned loads now call `Fst::verify()`, an
  `O(n)` CRC scan, and reject a bad blob at load; `load_mmap` still skips it to keep mapping
  constant-time (a mapped file is trusted intact, as before).
- **`PerfectHashIndex` never trusts the header's `overflow_cap`.** It is now recomputed from the
  arena on every load, `BMP3` included, so a blob whose framing checksum was forged alongside a
  crafted cap still cannot steer a query past the true remap length. `CompactHashIndex` stores no
  keys and cannot recompute it, so it remains a *trust-your-own-blob* format (documented).

### Added

- **Golden hash-stability tests.** `hash_key` and `fingerprint_bits` pin exact outputs for a fixed
  key set (ASCII and multibyte), so a silently changed constant — which would make every previously
  saved MPH blob load wrong — fails CI loudly instead. Changing a hash is a format break that must
  bump the magic, not the table.

### Changed

- **Trust-boundary wording made precise** in `README.md` and `docs/design.md`: lexindex framing is
  bounds-checked and `StringIndex` owned loads verify the FST CRC, but the perfect-hash indexes'
  embedded `epserde` MPH (which `ptr_hash` reads unchecked) stays a *trust-your-own-blob* payload —
  the header checksum and recomputed `overflow_cap` guard accidental corruption, not a crafted blob.

## [0.7.0] — 2026-08-27

### Fixed

- **Non-member queries can no longer read ptr_hash's remap out of bounds** — a debug-build panic
  and, in release builds, undefined behaviour (`get_unchecked` past the remap vector), present in
  every version since the minimal-perfect-hash indexes shipped. ptr_hash's minimal `index()` remaps
  raw slots ≥ n through an internal vector that only covers slots up to the last member-occupied
  one, and reads it *unchecked*; a non-member key whose raw slot lands in the trailing free zone —
  a zone that exists for a few percent of built indexes, depending on construction entropy —
  indexed past it. Surfaced by CI as a flaky `assertion failed: rank < self.count_ones()` in the
  new 4-bit fingerprint test (reproduced locally in 23 runs; the backtrace pins
  `ptr_hash::pack::Packed::index` → `cacheline-ef::index_unchecked`). Both indexes now record the
  remap's exact length at build time (`overflow_cap` — the largest member `raw slot − n`, plus
  one) and answer `None` outright for raw slots past it: those slots are provably free, so no
  member can live there. The repro loop went from 1 failure in 23 runs to 0 in 120; a regression
  test rebuilds until the zone occurs and asserts the guard. Lookup cost is unchanged (A-B against
  the 0.6.0 binary, `std::HashMap` control: PH 330 vs 331 ns, CHI 244 vs 244); builds pay ~1% for
  measuring the cap (one streamed `index_no_remap` pass over the members).
- **`id_unchecked` is bounded too.** It documents skipping the membership *comparison*, not the
  bounds, but it called `mph.index()` directly and so kept the defect above for any caller who
  passed a key that was not in fact a member. It now resolves through the same guarded path and
  returns a valid slot for any input.
- **`subsequence` matches characters, not bytes.** `fst`'s `Subsequence` automaton advances one
  byte at a time, so a query character matched if its bytes appeared anywhere in order:
  `subsequence("é")` (`[C3 A9]`) matched `"àΩ"` (`[C3 A0 CE A9]`), which contains neither
  character. Every non-ASCII subsequence query was affected. Replaced with an automaton that
  rewinds a partial character on mismatch — exactly correct rather than conservative, because
  UTF-8's lead and continuation byte classes are disjoint.
- **`save` is atomic.** All three indexes write a sibling temporary and rename it into place, so a
  crash or a full disk leaves the previous file intact instead of a truncated one that still has a
  valid magic. `load` no longer copies the file buffer a second time.

### Changed

- **Blob formats: `PerfectHashIndex` writes `BMP3`, `CompactHashIndex` writes `BCH3`** — the
  previous layouts plus the `overflow_cap` field and a 32-bit check over the lexindex header. The
  check is not decoration: `overflow_cap` *bounds an otherwise unchecked read*, so a header that
  lost bytes in transit must fail loudly rather than steer queries.
- **Blobs written before 0.7 are healed or refused, never loaded unbounded.** A `BMP2`
  (`PerfectHashIndex`) blob is healed: its arena holds every key, so the bound is recomputed
  exactly at load, costing O(n) hashes once. A `BCH1`/`BCH2` (`CompactHashIndex`) blob stores no
  keys, cannot be repaired, and is refused with a message naming the fix — loading it would
  reinstate the out-of-bounds read. Rebuild those indexes on 0.7.
- **Construction failure is an error, not a panic.** `IndexError::Build` replaces the
  `unwrap_or_else` fallback path when both parameter sets fail. Loading now also rejects a header
  claiming more than `u32::MAX` keys (ids are `u32`) and cross-checks the deserialised MPH's own
  key count against the header's.
- **Python: any iterable of keys, any `os.PathLike` path.** Constructors took only sequences, so
  building from a generator raised `TypeError`; paths took only `str`, so a `pathlib.Path` had to
  be stringified at every call site. The stubs claimed `list[str]` where tuples were always
  accepted, and were widened to match reality.
- **Honest wording for measured behaviour.** Perfect-hash ids are documented as *not* reproducible
  across builds (ptr_hash's construction is randomised — measured on 50 k keys, ~53 % keep their
  id when the same key set is rebuilt); the fingerprint false-positive rate is documented as a
  design rate against random non-members, not a defence against chosen queries (both hashes are
  deterministic and unseeded); an `epserde` blob is documented as portable only across machines of
  the same endianness and pointer width.
- `bench/compare.py` timed each competitor's first `import` as part of its build, which flattered
  lexindex — imported at module scope. Builds are now the median of five runs after a warm-up.

### Added

- A weekly `sanitize` workflow: AddressSanitizer over the whole suite (the class of defect fixed
  above is invisible to a normal `cargo test`) and Miri over the fst-only build. CI also runs
  `cargo test --release`, and a release preflight refuses a tag whose version does not match
  `Cargo.toml`, `pyproject.toml`, `CITATION.cff` and a dated `CHANGELOG.md` section.

## [0.6.0] — 2026-08-27

### Added

- **Sub-byte fingerprints: `CompactHashIndex::build_bits` / `fingerprint_bits=` (1..=64 bits).**
  The fingerprint table is bit-packed, so size is exactly `fingerprint_bits/8` bytes per key on top
  of the ~0.27 B/key minimal perfect hash, and the membership false-positive rate is exactly
  `2^-fingerprint_bits`. On the 479 823-word dictionary: **0.77 B/key at 4 bits (6.25% FP, 3.9×
  smaller than marisa-trie)**, 1.02 at 6 bits (1.56%), 1.77 at 12 bits (0.024%) — the existing
  byte widths keep their exact sizes and rates (1.27 / 2.27 / 4.27 B/key at 8 / 16 / 32 bits).
  `CompactHashIndex` stays below marisa-trie's 2.98 B/key at every width up to 21 bits. The
  advertised rate is measured, not assumed: 2 M random non-member probes landed at 6.253 %
  (z = +0.18) for 4 bits and 1.555 % (z = −0.83) for 6. Python: keyword-only `fingerprint_bits=`
  on the constructor plus a `fingerprint_bits` property; `fingerprint_bytes` keeps its byte
  semantics unchanged. The docs gained a width-choice table (rate priced per non-member probe).

### Changed

- **Blob format: `CompactHashIndex` now writes `BCH2`** (width field counts bits, table is
  bit-packed). **0.5.x `BCH1` blobs still load — including zero-copy under mmap** — because their
  byte-aligned fingerprints are bit-identical to the packed layout at 8× the width; 0.5.x cannot
  read the new `BCH2` blobs, hence the 0.6.0 version bump. The default 8-bit path is not taxed by
  the generality: byte-aligned widths take a straight byte-copy fast path when building, and A-B
  against the 0.5.1 binary (12 alternated runs, `std::HashMap` control) put both build (222 vs
  225 ms/1 M keys) and lookup (166.4 vs 166.0 ns) inside the control's noise. A 4-bit index
  answers `id()` as fast as an 8-bit one (86.9 vs 87.7 ns on the dictionary) and streams `ids_of`
  faster (47.6 vs 55.1 ns/key — half the table, better cache residency).


## [0.5.1] — 2026-08-27

### Added

- **Citation metadata** (`CITATION.cff` + `.zenodo.json`): GitHub shows "Cite this repository", and
  once the repository is enabled in Zenodo's GitHub integration, each release from the next tag on
  is archived with a DOI. Metadata validated against the CFF 1.2.0 schema.

### Changed

- **The minimal perfect hash is built with ptr_hash's `default_compact` parameters (λ=3.9).**
  Tighter pilot buckets take the MPH from 2.41 to **2.17 bits/key** on the 479 823-word
  dictionary: `CompactHashIndex` fp=1 drops **1.301 → 1.272 bytes/key** (2.34× smaller than
  marisa-trie's 2.98), fp=2 2.301 → 2.272, `PerfectHashIndex` 13.625 → 13.596. Query time is
  unchanged (A-B in one binary at 480 k and 5 M keys, plus process-level A-B-A-B with a
  `std::HashMap` control); builds pay ~+10 ms per million keys (~3–4%) — a build-once/query-many
  trade. Compact construction can occasionally fail (pilot eviction chains grow too long), so it
  falls back to the default parameters automatically. Both parameter sets serialise the same type,
  so **blobs stay compatible in both directions** — no format change.

- **Batch `ids_of` on `PerfectHashIndex` and `CompactHashIndex` streams its MPH lookups.**
  Per-key `id()` walks hash → slot → verify serially, stalling on a cache miss at every step.
  The batch path now drives ptr_hash's `index_stream` (software-prefetched slot resolution) and
  prefetches the verification data (arena offsets and spans, fingerprint bytes) a fixed distance
  ahead, so the memory latency of key *i+16* overlaps the compare of key *i*. Measured against the
  per-key loop in the same binary: `PerfectHashIndex.ids_of` **1.55×** on the 479 823-word
  dictionary and **1.83×** on 5 M real-word bigrams; `CompactHashIndex.ids_of` **1.10×** /
  **1.21×** (its fingerprint compare was already a single byte load, so only the slot stream and
  fingerprint prefetch help). The Python `ids_of` of both classes routes through the streamed core
  with the GIL released; misses still come back as `None`, pinned by tests on both sides.

- **The rank-walk (`id → key`) picks each FST transition by binary search instead of a linear
  scan.** Transitions are stored in increasing byte order, which makes their subtree-minimum ranks
  non-decreasing — the walk's invariant already guaranteed the order, the scan just wasn't using
  it. Near the root of a dictionary FST a node fans out ~50 ways, so the saving concentrates
  exactly where every reverse lookup must pass: `StringIndex.keys_of` over the whole 479 823-word
  dictionary drops from **775 to 423 ns/key (1.83×)**, measured back-to-back against the published
  0.5.0 wheel on the same machine, with the reconstructed keys verified equal to the sorted
  dictionary in both. Everything reverse benefits — `key`, `keys_of`, `dict(index)` iteration.

- **The speed benchmark (`examples/bench.rs`) now uses real dictionary-word bigrams**, the same key
  generator as `bench/scale.py`, and refuses to run without a word list rather than substitute
  synthetic keys — the same rule `bench/compare.py` has always enforced. The old
  `entity-000…N` keys arrived pre-sorted and hash-degenerate, flattering every build time. On real
  keys the README table moved both ways and was re-measured whole (one session, min of 12 runs):
  every `build` reads higher because sorting real input is part of the job, while the lookup gap
  over `std::HashMap` **widened from ~1.25× to ~1.5×** (realistic short keys make the byte-wise FNV
  hash cheaper relative to SipHash). A `CompactHashIndex::id` row was added — measured **~238 ns**,
  it beats `HashMap` while keeping its fingerprint membership check.

### Fixed

- **The hash-collision build error no longer suggests a retry that cannot work.** Both MPH builds
  said "64-bit key-hash collision; rebuild or use StringIndex" — but the hash is deterministic and
  unseeded (that is what makes a serialised MPH reloadable), so rebuilding the same key set fails
  identically, forever. The message now says so and points at `StringIndex` or changing the keys.

### Documentation

- **The collision odds behind "build fails on a 64-bit hash collision" are now quantified** instead
  of called astronomically rare: `n(n-1)/2^65`, computed exactly (Maxima and PARI/GP agreeing) —
  6.2×10⁻⁹ for the dictionary, 2.7×10⁻⁶ at 10 M keys, **2.7×10⁻⁴ at 100 M**, ~2.7% at 1 G. Honest
  below ~10 M; a real design consideration at 10⁸–10⁹, where `StringIndex` has no such failure mode.

- **The `256^-k` false-positive rate is now statistically verified, not just asserted.** On the
  0.5.0 code, dictionary members with two non-member populations: 2 M random strings measured
  0.384% at fp=1 (z = −1.5 against the exact 0.390 625%) and 33/2 M at fp=2 (z = +0.5); 50 000
  held-out real words measured 0.310% (z = −2.9). At or below theory in every case — the advertised
  rate is a ceiling in practice.

- **The usage guide now explains what `limit` buys — and what it cannot** (["What `limit` buys"](https://github.com/ilgrad/lexindex/blob/main/docs/usage.md)),
  replacing 0.5.0's single headline number with the measured behaviour. The speedup is the work *not
  done*, so it spans three regimes: prefix/range scale with `matches ÷ limit` (measured ~3 000× for
  `prefix("s", limit=10)` on an idle machine — the 669× in the 0.5.0 notes was the same query on a
  loaded one, i.e. conservative), subsequence gains ~80× because early stop saves the expensive
  traversal itself, and fuzzy gains only ~6× because the eagerly-built Levenshtein automaton is a
  fixed cost `limit` cannot skip. Also documented: the ~3 µs per-call floor (asking for 1 match
  costs the same as 10), and that consuming *all* matches gains nothing by construction.

## [0.5.0] — 2026-08-26

**Upgrading:** rebuild any saved `PerfectHashIndex` blob — its format changed (see below) and 0.5.0
rejects the old one rather than misreading it. `StringIndex` and `CompactHashIndex` blobs load
unchanged. Rust consumers need a 1.85 toolchain. Python users need nothing beyond `pip install -U`.

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

- **`PerfectHashIndex` is 23% smaller: its key arena now uses 4-byte offsets.** The arena addresses
  each stored key by an offset into a flat buffer, and those offsets were `u64` — 8 bytes per key to
  address a 4.9 MB buffer. They were the single largest part of the structure: **8.0 of its 17.625
  bytes per key** on the 479 823-word dictionary. Offsets are now 4 bytes, taking the index to
  **13.625 B/key (−22.7%)**.
  - **The width is chosen per arena, not capped.** An arena above 4 GiB still gets 8-byte offsets,
    recorded in a header byte, so no corpus that built before will fail to build now.
  - **Lookups got faster, not slower.** Halving the offset table halves the cache footprint of the
    two reads every verified lookup makes, which more than pays for the width branch — and the
    branch is on a field fixed for the life of the index, so it predicts. `PerfectHashIndex::id`,
    the only path that touches the arena, measured **−3.7%** (386.8 → 372.4 ns, min of 12 runs on an
    idle machine). The controls that cannot touch the arena — `id_unchecked`, `StringIndex`,
    `std::HashMap`, `std::BTreeMap` — moved +0.0%, +0.2%, +0.7% and −0.3%, which is what makes the
    −3.7% readable as the change rather than the machine.
  - Building is cheaper too, since the offset table is assembled in memory before it is written:
    the peak RSS of a `PerfectHashIndex` build on the dictionary falls a further **38.1 → 29.9 MB**
    on top of the saving below, for **~87 → 29.9 MB (−66%)** across the release.
  - **Breaking:** the `PerfectHashIndex` blob magic is now `BMP2`; blobs written by 0.1–0.4 must be
    rebuilt. `StringIndex` (`BIX4`) and `CompactHashIndex` (`BCH1`) blobs are untouched, as are
    their sizes.

- **`build` no longer copies the corpus to sort it.** All three constructors collected their input
  into an owned `Vec<String>` before sorting and deduplicating, even though every key is copied
  again into the structure being built. They now sort the caller's items in place, comparing
  through `AsRef<str>`. `PerfectHashIndex` additionally held a *third* copy: its slot table cloned
  each key only for the arena to copy it once more, and now borrows instead. On the 479 823-word
  dictionary (peak RSS of the build itself, one process per variant, order alternated across four
  pairs; timings A-B-A-B with both implementations in one binary):

  | | peak RSS | build |
  |---|---|---|
  | `StringIndex` | 29.9 → **8.0 MB** (−73%) | 1.04× |
  | `PerfectHashIndex` | 72.5 → **38.1 MB** (−47%) | **1.99×** |

  Rust callers get the same saving: passing `&[String]` or an iterator of `&str` now costs one
  pointer per key instead of a copy of the corpus. Ids, key order and every serialised size are
  unchanged — this only removes intermediates.
  - `test_build_releases_the_gil` grew its key count: at 400 000 keys the build now takes 49 ms
    rather than 268, which tripped the test's own "too fast to tell anything" guard. It refused to
    pass vacuously, which is what that guard is for.

- **The Python bindings borrow the caller's strings instead of copying them.** The constructors and
  `ids_of` read their keys as `PyBackedStr` — a view into the Python `str` — where they previously
  extracted an owned `Vec<String>`. `build` already copies the keys it keeps, so that intermediate
  vector was pure overhead: on the 479 823-word dictionary the **peak RSS of a build drops from
  44.6 MB to 29.9 MB (−33%)**, and the build itself is **1.08×** faster, `ids_of` **1.05×**
  (both implementations compiled into one extension and alternated A-B-A-B; four independent
  process pairs for the memory figure, which agreed to within 0.3 MB).
  - **No API change.** `PyBackedStr` accepts exactly what `String` did; the observable contract —
    accepted types, rejected types and every error message — was diffed against a build of the
    previous code and is identical. This change left every serialised size untouched; the only size
    that moves in this release is `PerfectHashIndex`, from the arena change above.

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
    reacquiring it would cost more than the work it protects. Their code is untouched, and this
    change altered no serialised byte of any index.

- **The minimum supported Rust version is now 1.85**, declared as `rust-version` in `Cargo.toml`
  and enforced by a CI job that derives its toolchain from that field, so the declaration cannot
  drift from what is actually built. The crate moved to **edition 2024**, whose floor is exactly
  1.85; `cargo fix --edition` required no source changes in any feature configuration, so the
  only user-visible effect is the toolchain requirement itself.
  - **Rust consumers on a toolchain older than 1.85 must upgrade** — `cargo` will refuse to build
    lexindex rather than fail obscurely.
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
