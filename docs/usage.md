# Usage guide

Runnable snippets for every interface, in Python and Rust.

## `StringIndex` — ordered, FST-backed

```python
import lexindex

# Duplicates are removed and keys sorted; the id of a key is its rank in sorted order.
idx = lexindex.StringIndex(["banana", "apple", "apricot", "cherry", "apple"])

# Keys already in ascending byte order build without ever being materialised: the constructor
# has to hold the whole corpus to sort it, these consume the iterable lazily. Measured at 10 M
# keys, the streamed build peaks at 11.6 MB against the list build's 1053.4 -- 91x.
keys = (line.rstrip("\n") for line in open("sorted-keys.txt"))   # any lazy iterable
n = lexindex.StringIndex.build_sorted_to_file(keys, "keys.bix")  # -> number of keys written
idx2 = lexindex.StringIndex.from_sorted(["apple", "apricot", "banana"])  # or straight to memory

len(idx)                 # 4  (duplicate "apple" deduped)
idx.id("apple")          # 0
idx.id("missing")        # None
"cherry" in idx          # True
idx.key(2)               # "banana"  (id → string)

# the dict spelling of the same lookup, on all four classes: KeyError on a miss, or a default
idx["apple"]             # 0
idx.get("missing", -1)   # -1

# ordered iteration — automaton-driven: prefix/range seek directly; a broad fuzzy or
# subsequence pattern may still walk most of the FST
idx.prefix("ap")         # [("apple", 0), ("apricot", 1)]
idx.range("apricot", "cherry")   # [("apricot", 1), ("banana", 2)]  — [lo, hi)
idx.successor("ba")      # ("banana", 2)   — smallest key ≥ query
idx.predecessor("ba")    # ("apricot", 1)  — largest key ≤ query
idx.fuzzy("aple", 1)     # [("apple", 0)]  — Levenshtein edit distance ≤ 1
idx.subsequence("ae")    # [("apple", 0)]  — "a…e" in order, not necessarily contiguous
# "character" above means a Unicode scalar value: no normalisation, case folding or grapheme
# segmentation is applied — normalise before building and querying if you need it

# order statistics — answers about the id space without decoding any key, so the cost does not
# grow with the number of matches
idx.lower_bound("ba")            # 2  — how many keys sort below "ba"; an insertion point in 0..=len
idx.prefix_id_range("ap")        # (0, 2)  — a prefix is a contiguous *slice* of the id space
idx.prefix_count("ap")           # 2
idx.range_count("apricot", "cherry")  # 2  — what range() would return, uncounted

# lazy iteration in sorted (= id) order — streams the transducer a chunk at a time, so it never
# builds a giant list and never decodes a key twice
list(idx)                # [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
dict(idx)                # {"apple": 0, "apricot": 1, "banana": 2, "cherry": 3}

# every query takes a limit: stop after that many matches, walking no further
idx.prefix("ap", limit=1)        # [("apple", 0)]
idx.fuzzy("aple", 1, limit=1)    # [("apple", 0)]

# batched lookups — one Rust↔Python crossing instead of one per key (named ids_of / keys_of so the
# class is never mistaken for a mapping)
idx.ids_of(["banana", "x"])   # [2, None]
idx.keys_of([0, 2])           # ["apple", "banana"]
```

### What `limit` buys — and what it cannot

`limit` is not a faster walk; it is **not doing work whose result would be thrown away**. Without it,
`prefix("s")` on the 479 823-word system dictionary walks the whole `s` subtree of the FST and
materialises every match — 45 064 `(str, int)` tuples — even if the caller wanted ten. With it, the
walk stops at the tenth match. The saving is therefore proportional to how much of the result you
*discard*, and it varies enormously by query (same dictionary, idle machine, min of 9 runs):

| query | matches | full | `limit` | speedup |
|---|---:|---:|---:|---:|
| `prefix("s", limit=10)` | 45 064 | 10.96 ms | 0.004 ms | ~3 000× |
| `prefix("a", limit=10)` | 25 192 | 4.83 ms | 0.003 ms | ~1 500× |
| `prefix("un", limit=10)` | 20 358 | 4.09 ms | 0.006 ms | ~700× |
| `subsequence("abc", limit=10)` | 1 910 | 41.31 ms | 0.51 ms | ~80× |
| `fuzzy("hello", 2, limit=5)` | 242 | 1.52 ms | 0.26 ms | ~6× |

Three regimes hide in that column, and they tell you when to reach for `limit`:

- **Prefix / range: the walk is cheap, the discarded results were the cost.** Speedup tracks
  `matches ÷ limit` almost linearly. This is the autocomplete case, and it is why the numbers are
  in the thousands.
- **Subsequence: the walk itself is expensive.** The `.*a.*b.*c.*` automaton visits many FST nodes
  per match produced, so stopping early saves *traversal*, not just tuples — 80× despite discarding
  far fewer results than `prefix("un")`.
- **Fuzzy: a fixed cost dominates that `limit` cannot skip.** The Levenshtein automaton is built
  eagerly before the walk starts (deliberately, so a too-large distance raises up front rather than
  on first use). `limit` only trims the walk after that, hence single-digit gains.

There is also a floor under every query: one call costs ~3 µs of FFI plus stream construction, so
per-match cost at `limit=1` reads ~2 000 ns against a steady ~200 ns from `limit≈100` up. Asking
for one match costs about the same as asking for ten — batch your UI accordingly.

If you will consume **all** matches, `limit` (or omitting it) changes nothing: the eager form *is*
the lazy walk collected, measured within noise of each other. In Rust, prefer the `*_iter` forms
(`prefix_iter` / `range_iter` / `fuzzy_iter` / `subsequence_iter`) and `.take(n)` — same machinery,
no intermediate `Vec` at all.

### Persistence and zero-copy loading

```python
idx.save("catalog.bix")                            # write a flat, relocatable blob
idx = lexindex.StringIndex.load("catalog.bix")     # read it back into RAM
idx = lexindex.StringIndex.load_mmap("catalog.bix") # …or memory-map it: no read, borrowed zero-copy

data = idx.to_bytes()                              # or go through bytes directly
idx = lexindex.StringIndex.from_bytes(data)

import pickle                                      # every class pickles, including Overlay
idx = pickle.loads(pickle.dumps(idx))              # …so one can be sent to a spawned worker
```

**Pickling copies the blob.** An index goes onto the wire as its `to_bytes()` and comes back through
`from_bytes`, which is what makes it work across a `multiprocessing` `spawn` — a path would not
survive a worker that cannot see the same filesystem, and a memory-mapped index would not survive
the file changing. So a mapped index pickles by value like any other, and a large one costs its
serialised size in the pickle: when both ends can see the same file, `save` plus `load_mmap` shares
the pages instead of copying them.

`load_mmap` maps the file and borrows the index from the mapped pages, so load time is independent of
the index size and the pages are shared across processes. The mapped file must stay immutable while an
index borrows it.

A truncated or corrupt blob is refused with `ValueError`, and no load can produce undefined
behaviour. `StringIndex` has one documented exception, and it is the reason `SECURITY.md` states a
threat model rather than a promise: its blob is an `fst` transducer whose node decoder is safe Rust
but not *total*, and the checksum in front of that decoder is public, so bytes crafted to carry a
matching one can **panic** instead of returning. In Python that surfaces as
`pyo3_runtime.PanicException`, which derives from `BaseException` and so slips past
`except ValueError`.

```python
idx = lexindex.StringIndex.from_untrusted_bytes(data)          # a blob someone else wrote
ov = lexindex.Overlay.from_untrusted_bytes(data, lexindex.StringIndex)   # …or one wrapping it
```

That loader walks every reachable node, requires every transition to point below the node holding
it, streams every key to check its rank, and catches the panic at the load boundary, so a crafted
blob raises `ValueError` like any other bad input. It costs 42× `from_bytes` (50.8 ms against
1.2 ms on 479 823 words), which is the trade: pay it once for a stranger's blob, never for your
own. The overlay form exists for the same reason — an overlay's own framing is checksummed and
validated either way, but the base region inside it is handed to a base loader, and over a
`StringIndex` base that is exactly the choice above. The two hash indexes need no such call: their
loaders are total, so `Overlay.from_untrusted_bytes` over a hash base does the same work as
`from_bytes`.

One thing neither loader can promise: the contained panic still runs the process-wide hook on its
way out, so a rejection normally prints a panic message to stderr before the `ValueError` arrives.

## `PerfectHashIndex` — exact lookup with `id → key`

```python
from lexindex import PerfectHashIndex

dict_ = PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
i = dict_.id("POST")           # dense id in [0, n); membership is verified against the stored key
dict_.key(i)                   # "POST"
dict_.id("PATCH")              # None  — a verified miss, not a hash collision
dict_.id_unchecked("GET")      # fastest lookup; skips verification (closed-vocabulary hot path)

dict_.save("verbs.bmp")
dict_ = PerfectHashIndex.load_mmap("verbs.bmp")   # arena mapped zero-copy; tiny MPH read into RAM

# a corpus that does not fit in memory: hand it a factory — it is called twice — never the iterable
n = PerfectHashIndex.build_to_file(lambda: (line.rstrip("\n") for line in open("keys.txt")), "keys.bmp")
```

Use `id_unchecked` only for a **fixed / closed vocabulary** where membership is already guaranteed —
it returns an arbitrary (but valid) slot for an unknown key. Use `id` everywhere else.

## `CompactHashIndex` — smallest footprint

```python
from lexindex import CompactHashIndex

# fingerprint_bytes ∈ {1, 2, 4}; or a keyword-only fingerprint_bits ∈ 1..=64 for finer control.
# 8 bits (the default) is ~1.3 B/key with a ~0.4% membership false-positive
# rate; 2 → ~0.0015%; 4 → effectively exact. No keys are stored, so there is no id → key.
dict_ = CompactHashIndex(["GET", "POST", "PUT", "DELETE"], fingerprint_bytes=1)
i = dict_.id("POST")           # dense id in [0, n); a non-member may rarely read as present
dict_.contains("GET")          # True
dict_.id_unchecked("GET")      # fastest lookup; no fingerprint check (closed-vocabulary hot path)

dict_.save("verbs.bch")
dict_ = CompactHashIndex.load_mmap("verbs.bch")   # fingerprint table mapped zero-copy
```

### Choosing the fingerprint width

Size is the minimal perfect hash (0.26 B/key, flat in `n`) plus exactly `fingerprint_bits/8` bytes
per key, and
the membership false-positive rate is about `2^-fingerprint_bits` — a design rate for
well-distributed keys, measured 6.2530 % at 4 bits and 1.5553 % at 6 over 2 M non-member probes;
not a defence against an adversary who chooses the queries, since both hashes are deterministic and
unseeded — every width is a point on the
same trade-off (measured on `/usr/share/dict/words`, 479 823 keys):

| `fingerprint_bits` | bytes/key | false-positive rate | false hits per 1 M non-member probes |
|---:|---:|---:|---:|
| 4 | **0.76** | 6.25% | 62 500 |
| 6 | 1.01 | 1.56% | 15 625 |
| 8 (= `fingerprint_bytes=1`, default) | 1.26 | 0.39% | 3 906 |
| 12 | 1.76 | 0.024% | 244 |
| 16 (= `fingerprint_bytes=2`) | 2.26 | 0.0015% | 15 |
| 32 (= `fingerprint_bytes=4`) | 4.26 | 2.3×10⁻⁸% | ~0 |

Pick by the probe mix, not the key count: the rate is per *non-member* lookup, so a workload that
only ever queries members never sees a false positive at any width, while a filter in front of a
network hop wants the rate priced against the cost of a wasted hop. For scale: marisa-trie's exact
index costs 2.98 B/key on this corpus — `CompactHashIndex` is below it at *every* width up to
21 bits (rate 2⁻²¹ ≈ 5×10⁻⁵%).

```python
tiny = CompactHashIndex(keys, fingerprint_bits=4)   # 0.76 B/key, 1-in-16 false positives
tiny.fingerprint_bits                               # -> 4
```

`CompactHashIndex` trades exactness for size: membership is correct except for a `2^-fingerprint_bits`
false-positive chance on a non-member, and it cannot map an id back to a string. Reach for it when a
fixed vocabulary's on-disk / mmap footprint dominates; use `PerfectHashIndex` when you need exact
membership or `id → key`, or `StringIndex` when you need order or fuzzy/prefix.

## `Overlay` — edits without a rebuild

All three indexes are built once from the whole key set, so adding a single key has always meant
rebuilding for the whole corpus. An overlay wraps one with the keys added since and the ids retired
from it. The base is shared, not copied: it stays usable, and several overlays can sit on one index.

```python
from lexindex import Overlay, PerfectHashIndex

base = PerfectHashIndex.load_mmap("vocab.bmp")   # untouched by anything below
ov = Overlay(base)
new_id = ov.add("neologism")                     # ids continue above len(base)
ov.remove("typo")                                # the id is retired, nothing is renumbered
ov.save("vocab.ovl")

back = Overlay.load("vocab.ovl", PerfectHashIndex)   # pass the base class, not an instance
folded = back.compact()                              # rebuild the base; this renumbers
```

An id is never reissued and removal never renumbers, so an id held elsewhere keeps its meaning until
`compact()` — which is explicit for exactly that reason. `load` and `from_bytes` take the base class
because each index loads itself; the blob records which base wrote it, so passing the wrong class is
an error rather than an unchecked read of bytes meant for something else. `save` is atomic, like the
indexes' own: a crash or a full disk leaves the previous file whole rather than a truncated one.

**`Overlay.load` and `Overlay.from_bytes` are checksummed and validated throughout.** The header
carries a check of its own and a hash of everything after it, both verified before any of it is
read, so a flipped bit anywhere raises `ValueError` rather than loading as a different key or a
revived id. The framing is checked past the checksums too — the lengths, the additions and their
UTF-8, the tombstones against the id space — and the base region goes to the base class's loader,
which validates its own. A blob written by `0.12` still loads, without the two checksums that format
does not carry; saving it again writes the current one and it gains them.

`key`, `keys` and `compact` need the base to store its keys. `CompactHashIndex` does not, so an
overlay over it answers membership and raises `TypeError` for the rest — in Rust that same absence is
a compile error, since the methods live on a trait the keyless index cannot implement. Removal over
that base also inherits its false-positive rate: a `contains` that was never true of a real key can
retire an id, so remove by a key you know is present.

### Batched lookups into a buffer

`ids_of` returns a list, which means one Python `int` per key. When the ids are headed for `numpy`
or `array` rather than for a loop, `ids_of_bytes` skips that: it returns the same answers packed as
native-endian fixed-width items, and `np.frombuffer` shares the memory instead of copying it.

```python
import numpy as np

buf = idx.ids_of_bytes(probes)              # 4 bytes/key here, 8 for StringIndex
ids = np.frombuffer(buf, dtype=idx.ID_DTYPE)
found = ids[ids != idx.MISSING_ID]          # absent keys come back as MISSING_ID
```

`ID_DTYPE` and `MISSING_ID` are class attributes because the width differs between the index types
(`StringIndex` ids are 64-bit, the hash indexes' are 32-bit) — read them from the class rather than
hardcoding one. A buffer cannot carry `None`, so `MISSING_ID` stands in for an absent key; it is the
largest value of the width, which is never a real id, and `ids_of_bytes` refuses outright on the one
index size where it would be.

### Threads, including free-threaded CPython

The module tells CPython it does not need the GIL, and the guarantee behind that is:

- **The three index types are immutable after building.** Share one across as many threads as you
  like and call `id`, `contains`, `key`, `ids_of` from all of them. Building, batch lookups and
  persistence release the GIL, so other threads keep running while a large index is built or queried.
- **The two types that hold mutable state — the `StringIndex` iterator and `Overlay` — serialise.**
  Sharing one of those between threads is safe: the calls take an internal lock and queue up rather
  than raising. A shared iterator hands each key to exactly one thread; concurrent `Overlay.add`
  calls each get their own id.

Verified on CPython 3.14t with eight threads. Without the lock, PyO3's borrow flag turns a shared
iterator or overlay into `RuntimeError: Already borrowed` on a free-threaded build — seven of eight
threads failed before the fix, which is what `tests/test_python.py` now pins.

The published wheels are `abi3` and free-threaded CPython has no stable ABI before 3.15, so on a
`3.13t` / `3.14t` interpreter today the extension is built from the sdist. That build is
version-specific, and everything above holds for it.

## Rust

```rust
use lexindex::{CompactHashIndex, PerfectHashIndex, StringIndex};

let idx = StringIndex::build(["apple", "apricot", "banana"])?;
assert_eq!(idx.id("banana"), Some(2));
assert_eq!(idx.key(0).as_deref(), Some("apple")); // rank-walk over the FST → owned String
assert_eq!(idx.prefix("ap").len(), 2);
let near: Vec<_> = idx.fuzzy("aple", 1)?.into_iter().map(|(k, _)| k).collect();
assert_eq!(near, ["apple"]);

// `save` / `load` / `load_mmap` take anything that is `AsRef<Path>`.
let path = std::env::temp_dir().join("lexindex-usage-catalog.bix");
idx.save(&path)?;
// SAFETY: nothing may modify the file while a mapped index borrows it (see `load_mmap`).
let idx = unsafe { StringIndex::load_mmap(&path) }?; // zero-copy; no read into RAM

let dict = PerfectHashIndex::build(["GET", "POST", "PUT"])?; // requires the default `mph` feature
assert_eq!(dict.key(dict.id("POST").unwrap()), Some("POST")); // exact reverse lookup

let small = CompactHashIndex::build(["GET", "POST", "PUT"], 1)?; // ~1.3 B/key, no reverse
let tiny = CompactHashIndex::build_bits(["GET", "POST", "PUT"], 4)?; // ~0.8 B/key, 6.25% FP rate
assert!(small.contains("POST"));
# drop(idx);
# std::fs::remove_file(&path).ok();
# Ok::<(), lexindex::IndexError>(())
```

### Building a corpus that does not fit in memory

Every index has a build that never holds the keys, and each takes the shape its structure allows:

```rust
use lexindex::{CompactHashIndex, PerfectHashIndex, StringIndex};
# let dir = std::env::temp_dir();
# let (bix, bmp) = (dir.join("lexindex-usage-stream.bix"), dir.join("lexindex-usage-stream.bmp"));

// `CompactHashIndex` keeps a 16-byte pair per key and drops the string: any iterator will do.
let small = CompactHashIndex::build(["a", "b", "c"].iter(), 1)?;

// `StringIndex` needs the keys in ascending byte order and streams them into the transducer.
StringIndex::build_sorted_to_file(["a", "b", "c"], &bix)?;

// `PerfectHashIndex` stores its keys in slot order, and slot order is only known once the perfect
// hash is built -- so it takes a *factory* and reads the source twice. Keys must be distinct.
PerfectHashIndex::build_to_file(&bmp, || ["c", "a", "b"])?;
# assert!(small.contains("a"));
# std::fs::remove_file(&bix).ok();
# std::fs::remove_file(&bmp).ok();
# Ok::<(), lexindex::IndexError>(())
```

At 10 M real-word pairs the streamed `PerfectHashIndex` build peaks at 471 MB against 1 272 MB for
the same keys handed to `build` as a list; the streamed `StringIndex` build peaks at 49.6 MB against
721.9. The perfect hash's number includes the output file, which it fills through a mapping — its
anonymous memory is 20.6 bytes per key and does not grow with `n`.

The perfect hash also needs **transient disk space**: an output whose key arena exceeds 32 MB is
filled window by window through a spill file next to it, so about 2.2× the output size has to be
free in the target directory until the build finishes. Filling the arena in one pass instead is
what made the build rewrite its own file dozens of times over.

Cargo features: `mph` (default) adds `PerfectHashIndex` and `CompactHashIndex`; `mmap` (default) adds
`load_mmap`; `--no-default-features` is an `fst`-only build (`StringIndex` only, no extra
dependencies). All of them compile for 32-bit targets, `wasm32-unknown-unknown` included — leave
`mmap` off there, since there is nothing to memory-map.

### Editing without a rebuild — `Overlay`

All three indexes are built once from the whole key set, so adding a single key has always meant
rebuilding for the whole corpus. `Overlay<I>` wraps one with the keys added since and the ids retired
from it, making `add` and `remove` O(1) while the base stays untouched:

```rust
use lexindex::{Overlay, StringIndex};
let mut ov = Overlay::new(StringIndex::build(["apple", "banana"])?);
let cherry = ov.add("cherry");            // ids continue above base.len()
assert!(ov.remove("apple"));              // the id is retired, nothing is renumbered
assert_eq!(ov.id("apple"), None);
assert_eq!(ov.key(cherry).as_deref(), Some("cherry"));

let blob = ov.to_bytes()?;                // base + additions + tombstones in one file
let back = Overlay::from_bytes_with(&blob, StringIndex::from_bytes)?;
assert_eq!(back.len(), 2);

let folded = back.compact()?;             // rebuild the base from the live keys — this renumbers
assert_eq!(folded.len(), 2);
# Ok::<(), lexindex::IndexError>(())
```

An id is never reissued and removal never renumbers, so an id held elsewhere keeps its meaning for
the overlay's lifetime; `compact()` is the one call that breaks that, and it is explicit for exactly
that reason. Lookups cost one base lookup plus a bitset probe, and a miss in the base costs a hash
map probe on top.

The base may be shared: `OverlayBase` is implemented for `Arc<I>`, so `Overlay<Arc<StringIndex>>`
leaves the index usable and lets several overlays sit on one base. That is what the Python bindings
use.

`from_bytes_with` takes the base's loader rather than picking one, because `Overlay<I>` is generic
over the base and each base parses its own blob: `Overlay::from_bytes_with(&blob,
PerfectHashIndex::from_bytes)`. All three base loaders are safe fns, so the closure could now be a
trait method — it stays a closure because that is also how a base loaded some other way, or a stub
that skips the base entirely, gets in: the fuzz target and the framing property tests both need it,
and the wrong loader is already refused by the base tag rather than by the type.

`key`, `keys` and `compact` need the base to store its keys, which `CompactHashIndex` does not — an
overlay over it answers membership and nothing else, and the absence is a compile error rather than a
runtime one. Removal over that base also inherits its false-positive rate: a `contains` that was
never true of a real key can retire an id, so remove by a key you know is present.

The blob records which base wrote it, so `from_bytes_with` refuses a mismatch before calling the
loader — handing perfect-hash bytes to the wrong deserialiser is not something a caller can do by
mistake.

## Benchmark

`python bench/compare.py` measures **serialised size** on real dictionary words against `marisa-trie`,
DAWG and datrie (the double-crown table above). `python bench/scale.py` measures **build time, peak
memory, and lookup latency from 1 M to 100 M** real keys, each cell once with the keys handed over as
a list and once as a generator — the second is what `CompactHashIndex`'s streaming build exists for,
and the only way to see its own footprint rather than the corpus's. `cargo run --release --example
bench` measures **point-lookup latency** for all three indexes against `std::HashMap` (SipHash and
FxHash) and `BTreeMap` on real dictionary-word bigrams (it refuses to run without a word list rather
than substitute synthetic keys); `cargo run --release --example peak` reports the **peak resident
memory and wall time of one build**, one index per process; `cargo run --release --example
mmap_zero_copy` times the owned `load` against the zero-copy `load_mmap`.
