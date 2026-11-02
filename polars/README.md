# lexindex-polars

A [Polars](https://pola.rs) expression plugin over
[lexindex](https://github.com/ilgrad/lexindex): map a string column to dense ids from an index
built once and stored in a file, and map the ids back to their keys.

```console
pip install lexindex-polars
```

```python
import lexindex as lx
import polars as pl
import lexindex_polars  # noqa: F401  -- the import registers the namespace

lx.DictIndex(sorted(set(catalog))).save("tracks.bdx")  # once, offline

df.with_columns(
    pl.col("track").lexindex.id("tracks.bdx").alias("track_id"),
)
```

[`examples/polars_ids.py`](https://github.com/ilgrad/lexindex/blob/main/examples/polars_ids.py)
runs end to end: a million-row string column to ids and back, with the sizes of both and of the
index file.

The expressions are elementwise, so they run in the engine's own threads, inside a lazy plan and
under the streaming engine, and hold no GIL. The blob is read once per process and shared by every
chunk and thread.

## The four expressions

| expression | takes | gives | on a key the index does not hold |
|---|---|---|---|
| `.lexindex.id(path)` | a string column | `UInt64` | `null` |
| `.lexindex.id_unchecked(path)` | a string column | `UInt64` | some id below the key count |
| `.lexindex.contains(path)` | a string column | `Boolean` | `false` |
| `.lexindex.key(path)` | an integer column | `String` | `null` |

`null` in, `null` out, for all four. Each also takes `pin=`; see [Reading the blob](#reading-the-blob).

## What each index kind answers

An index blob names its own kind, so one expression serves all seven and refuses what a kind cannot
do — with a `ComputeError` naming the kind and the file, not a wrong answer.

| kind | `id` | `id_unchecked` | `contains` | `key` |
|---|---|---|---|---|
| `DictIndex` | exact | — | exact | ✅ |
| `StringIndex` | exact | — | exact | ✅ |
| `DoubleArrayIndex` | exact | — | exact | — |
| `HashedDictIndex` | exact at 0 bits, else `2^-bits` | ✅ | same | ✅ |
| `PerfectHashIndex` | exact | ✅ | exact | ✅ |
| `CompactHashIndex` | `2^-bits` | ✅ | `2^-bits` | — |
| `ClosedHashIndex` | every key gets one | ✅ | — | — |

`2^-bits` is the rate at which a key the index does not hold is answered as though it did — the
fingerprint width chosen when the index was built. Where the table says `—`, the expression raises.

Which kind to build is [lexindex's own
question](https://github.com/ilgrad/lexindex#which-index): `DictIndex` when the ids must be the key
order and size matters, `HashedDictIndex` when they must be the key order and `id` is the hot path,
`CompactHashIndex` when only `string → id` is needed and a rare false positive is acceptable.
`lexindex plan keys.txt` prices every one of them against your own keys.

## Reading the blob

The file named by an expression is read whole on first use and cached for the process. Nothing
stays mapped, because a mapping's contract is that nothing writes the file, and a plugin cannot
promise that about a file any process may rebuild; this crate writes no `unsafe`.

**An expression is pinned to the file it saw when it was built**: its size, modification time and,
on unix, device and inode. A chunk gets that generation from the cache without touching the file,
or reads the file while it still is that generation, or raises a `ComputeError` saying the index was
rebuilt. Ids from two generations are not comparable, so one query never sees both. After a rebuild,
build the expression again. That reads the new file and drops the old generation from the cache, so
a query still running on it raises rather than mixes. `pin=False` on any expression reads whatever
the file holds when a chunk runs, so a rebuild reaches a running query's later chunks. A file absent
when the expression is built is read that way too, and a plan serialized and run elsewhere carries
its pin, so it needs `pin=False` if the copy there is another file.

A file may be anyone's: a crafted one is a `ComputeError` naming it. A `StringIndex` goes through
lexindex's `from_untrusted_bytes` for that, one walk over its transducer each time a file is read.

Paths are expanded and made absolute when the expression is built, not when it runs, so a query
keeps reading one file even if a symlink is swapped under it.

The threads of a first query wait for one read rather than each making their own. The cache holds
one generation a path, keyed by the same fields, and has no limit of its own. To give it one, set
`LEXINDEX_POLARS_CACHE_BYTES`: past that many bytes of blobs, counted by their files' sizes, the
least recently used go, and the newest stays whatever its size. Eviction costs a later chunk a read,
and never a wrong answer.

```python
lexindex_polars.cache_info()  # {"entries": 1, "bytes": 2636721, "hits": 12, "loads": 1}
lexindex_polars.clear_cache()  # what no running chunk holds goes back
```

## Versions

The plugin is compiled against polars' own FFI: the wheel names the polars it was built and tested
against (`polars>=1.44`), and a much older polars will refuse to load it. `lexindex` itself is a
normal dependency — it is what writes the blobs this reads.

The wheel is `abi3` for CPython 3.11 and up, which is what every polars plugin ships. On a
**free-threaded** interpreter (`cp314t`) pip will not take an `abi3` wheel, so build it from source
there — `pip install 'lexindex-polars @ git+https://github.com/ilgrad/lexindex#subdirectory=polars'`,
which needs a Rust toolchain.

## Licence

MIT, the same as lexindex. Issues and discussion live in the
[lexindex repository](https://github.com/ilgrad/lexindex).
