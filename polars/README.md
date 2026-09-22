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

`null` in, `null` out, for all four.

## What each index kind answers

An index blob names its own kind, so one expression serves all six and refuses what a kind cannot
do — with a `ComputeError` naming the kind and the file, not a wrong answer.

| kind | `id` | `id_unchecked` | `contains` | `key` |
|---|---|---|---|---|
| `DictIndex` | exact | — | exact | ✅ |
| `StringIndex` | exact | — | exact | ✅ |
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

The file named by an expression is read whole on first use and cached for the process, keyed by
path, size and modification time. Rewriting the file replaces that entry, so a later chunk sees the
new index and an earlier one does not: **do not rebuild an index a running query is reading.**
Nothing stays mapped, and this crate writes no `unsafe`.

Paths are expanded and made absolute when the expression is built, not when it runs, so a query
keeps reading one file even if a symlink is swapped under it.

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
