# Getting started

Five minutes from `pip install` to an index on disk that answers lookups both ways. Every command
below was run as shown, and its output is pasted unedited.

## Install

```console
$ pip install lexindex
$ lexindex --version
lexindex 4.5.2
```

The wheel carries the `lexindex` command line as well as the Python module, and has no runtime
dependencies.

## A file of keys

A keys file is one key per line, UTF-8, in any order. Duplicates and empty lines are dropped. Any
such file will do. This page uses the system word list, which most Linux and macOS installs carry:

```console
$ cp /usr/share/dict/words keys.txt
$ wc -l keys.txt
479823 keys.txt
```

Your list may differ, and so will the numbers below. The ones here come from Fedora's `words`
package.

## Choose an index

Say what the index must answer. Here that is `key(id)` as well as `id(key)` (`--reverse`), and
prefix queries (`--prefix`). `plan` prices every index that can answer them against these keys,
the cheapest first:

```console
$ lexindex plan keys.txt --reverse --prefix
479823 keys, mean length 9.3, mean shared prefix 6.3, ranked by size
* DictIndex              1190567 bytes   2.48 B/key    384 ns  estimated at block 1024
  DictIndex              1252952 bytes   2.61 B/key    336 ns  estimated at block 256
  DictIndex              1357175 bytes   2.83 B/key    311 ns  estimated at block 32
  StringIndex            2882400 bytes   6.01 B/key    297 ns  estimated
the nanoseconds are a model of this crate's own machine, not a measurement of yours
```

The sizes are estimates, and the times come from a model. Treat both as a ranking,
not as a promise.

## Build and save

`build` with the same needs asks the same question and builds the winner, marked `*`:

```console
$ lexindex build keys.txt words.bdx --reverse --prefix
479823 keys, mean length 9.3, mean shared prefix 6.3, ranked by size
* DictIndex              1190567 bytes   2.48 B/key    384 ns  estimated at block 1024
  DictIndex              1252952 bytes   2.61 B/key    336 ns  estimated at block 256
  DictIndex              1357175 bytes   2.83 B/key    311 ns  estimated at block 32
  StringIndex            2882400 bytes   6.01 B/key    297 ns  estimated
the nanoseconds are a model of this crate's own machine, not a measurement of yours
wrote words.bdx: DictIndex over 479823 keys, 1205985 bytes (2.51 B/key)
```

The same build from Python writes the same bytes:

```python
import lexindex

keys = open("keys.txt", encoding="utf-8").read().splitlines()
index = lexindex.DictIndex(keys, block=1024)
index.save("words-py.bdx")
print(index.serialized_len())
```

```console
1205985
```

`inspect` reads a blob's header without loading it:

```console
$ lexindex inspect words.bdx
kind: DictIndex
format: BDX3
bytes: 1205985
keys: 479823
arena_bytes: 1173645
```

## Load, look up, look back

`load_mmap` maps the file rather than reading it, so it is ready at once, and processes that map
the same file share its pages:

```python
import lexindex

words = lexindex.DictIndex.load_mmap("words.bdx")
print(len(words))
print(words.id("index"))
print(words.key(words.id("index")))
print(words.id("lexindexx"))
print(words.prefix("lexic")[:5])
print(words.ids_of(["apple", "banana", "cherry"]))
```

```console
479823
238969
index
None
[('lexic', 259323), ('lexica', 259324), ('lexical', 259325), ('lexicalic', 259326), ('lexicality', 259327)]
[98320, 107640, 135434]
```

A `DictIndex` id is the key's rank in sorted order. The ids are dense, from `0` to `len - 1`, so
they index an array directly. A key the index does not hold gets `None`, not someone else's id.
`ids_of` looks up a batch in one call.

## Where next

- [Usage guide](usage.md): every index, what each one answers, and the rest of the API.
- [Polars plugin](polars.md): the same lookups as Polars expressions over a column.
- [Benchmarks](benchmarks.md): how the indexes compare with each other and with other libraries,
  and how that was measured.
