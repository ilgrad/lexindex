# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

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

- **The usage guide now explains what `limit` buys — and what it cannot** (["What `limit` buys"](docs/usage.md)),
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
