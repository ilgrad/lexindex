# Changelog

`lexindex-polars` is versioned separately from `lexindex`: it tracks polars' plugin ABI, which
moves on polars' cadence and not on the library's.

## [0.2.0] — 2026-11-06

### Added

- **A `DoubleArrayIndex` blob is served.** `id` and `contains` are exact; `id_unchecked` raises, as
  on the other kinds that answer by searching, and `key` raises, since a double array walks one way
  only. Until now the blob was refused as a kind the plugin did not know.
- **A `PerfectHashIndex` with a merged arena is served**, the layout lexindex 5.0.0 writes for most
  keys; 0.1.1 refuses it as a format it cannot read.
- **A `DictIndex` with trie labels is served**, the blob lexindex 5.0.0 writes for `labels=True` where
  it is the smaller one; 0.1.1 refuses it as a format it cannot read.
- **The cache can be bounded, read and emptied.** `LEXINDEX_POLARS_CACHE_BYTES` names the bytes of
  blobs the process may hold, past which the least recently used go; unset, there is no limit, as
  before. `lexindex_polars.cache_info()` reports the entries, their bytes, the hits and the loads,
  and `lexindex_polars.clear_cache()` empties it. An evicted blob costs a later chunk a read and
  never a wrong answer, since a chunk holds its own reference.

### Changed

- **The wheel is built against lexindex 5.0.0 and requires it.** 5.0 replaced the key hash, so the
  plugin refuses the hash blobs 4.x wrote — `BMP8`, `BCH8`, `BCL2`, `BHD1` — as lexindex 5.0 does,
  and reads the `BMP9`, `BCH9`, `BCL3` and `BHD2` that replace them. `StringIndex`,
  `DictIndex` and `DoubleArrayIndex` blobs read as before. lexindex's `docs/migration-5.md` converts
  a perfect hash or a hashed dictionary from 4.x; the two that store no keys are rebuilt from their
  corpus.
- **An expression is pinned to the file it saw when it was built.** A rebuild during a query used to
  reach its later chunks and not its earlier ones, and nothing said so: two columns of one result
  could hold ids from two generations of an index, which are not comparable. A chunk now gets the
  generation its expression saw: from the cache without touching the file, or from the file while
  it still is that generation, or a `ComputeError` that says the index was rebuilt. **An expression
  object kept across a rebuild now raises where it used to read the new file**; build it again, or
  pass `pin=False` to any of the four for the old behaviour. An expression built anew after each
  rebuild, the usual loop, sees no difference.

### Fixed

- **A first query reads a blob once.** Every engine thread that arrived before the first read
  finished read the file itself, so a first query could hold as many copies of the blob at one
  moment as it had threads. A miss now waits for the read in progress. A load waits for a load of
  another file; a hit waits for neither.

- **A crafted `StringIndex` blob is refused by name.** The plugin loaded a `StringIndex` with
  `from_bytes`, which `SECURITY.md` keeps for blobs you wrote yourself and which can panic inside
  `fst`'s decoder. The engine caught the panic, so the process lived and the query failed, but with
  "the plugin panicked" and no file named; the panic hook ran first. It now loads with
  `from_untrusted_bytes`, and such a file is the same `ComputeError` as any other unreadable one.
  The walk this adds to a `StringIndex`'s load is paid once a generation.
- **A blob renamed over the path is seen even at the same length and timestamp.** The cache key was
  path, size and modification time, so such a replacement went on answering from the old blob for
  the life of the process. The key now holds the device and inode as well, on unix. And the size,
  the timestamp and the bytes now come from one open file, where a replacement between a stat and a
  read by path could file the new blob under the old one's key.

## [0.1.1] — 2026-09-24

### Changed

- **The wheel is built against lexindex 4.4.0; 0.1.0's was built against 4.1.0.** The library is
  compiled into the plugin, so its lookups are that version's: `StringIndex`'s are faster since
  lexindex 4.3.0, and `DictIndex`'s since 4.3.3 and 4.4.0. lexindex's own changelog has 4.4.0's
  `DictIndex` `id` taking 4.9–15.5 % less time than 4.3.3's at the default block of 256, on twelve
  corpora of thirteen at a million keys, in its Rust harness; nothing was timed through the plugin.
  No blob format 0.1.0 read has changed, so every index file it read still reads, with the same
  answers.

### Fixed

- **A `DictIndex` in a character code is read rather than refused.** lexindex 4.3 and later may
  spell a dictionary whose keys are mostly outside ASCII — Chinese or Russian titles, say — in a
  code of its own and save it as a `BDX4` blob. 0.1.0 predates that format and refused such a file
  as not a readable index, and with it a `HashedDictIndex` whose dictionary is one, though the
  `lexindex>=4.1` it depends on admits the versions that write them.

## [0.1.0] — 2026-09-22

### Added

- **The first release: four expressions over a lexindex blob.** `.lexindex.id`,
  `.lexindex.id_unchecked`, `.lexindex.contains` on a string column and `.lexindex.key` on an id
  column, each naming an index file by path. They are Polars *plugin* expressions rather than a
  `map_batches` callback, so they run in the engine's threads, inside a lazy plan and under the
  streaming engine, without the GIL: the alternative — a Python function over
  `lexindex`'s existing `ids_of_arrow` — would have cost nothing in dependencies and given up all
  three.
- **One expression serves all six index kinds.** The blob says which kind it holds, so the plugin
  loads it and answers; what a kind cannot do — `key` on the two that store no keys, `contains` on
  a closed hash, `id_unchecked` on the two that answer by searching — is a `ComputeError` naming
  the kind and the file, checked once per chunk rather than per row.
- **The blob is read once per process**, into a cache keyed by path, size and modification time,
  and shared by every chunk and thread behind an `Arc`. A rebuilt file at the same path replaces
  its entry instead of joining it, so a build-query-rebuild loop does not accumulate generations.
  Nothing is memory-mapped: the crate writes no `unsafe`, and no file has to stay unchanged for a
  mapping's sake.
