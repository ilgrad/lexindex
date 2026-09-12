# Benchmarks

Every number here is a claim about one machine on one day, and every timed table cites the file under
[`bench/results/`](https://github.com/ilgrad/lexindex/tree/main/bench/results) that holds its raw
samples and the environment that produced them. The README carries the two headline tables; this
page holds the rest and the protocol behind every number — read the ratios, not the absolutes,
and [what the error bars are](#error-bars-and-where-a-lookup-number-comes-from) before reading
a lookup time closely.

`bench/reproduce.sh` is the other half of that promise: it refuses a tree whose code does not match
the commit the artifact will be named after, pins the competitor versions, verifies every corpus
against its hash, records the machine, and finishes by putting the run's Python call floor beside
the published one — the number no change to this library can move, and therefore the one that says
whether your machine is comparable to the one in the tables at all.

## Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | common prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** | **ns/lookup** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | — | none (closed vocabulary) | — | **0.26** | 100 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | — | probabilistic | ✅ | **0.76** | 98 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | — | probabilistic | ✅ | **1.26** | **92** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | — | probabilistic | ✅ | **2.26** | 100 |
| **lexindex `DictIndex` (128 per block)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.83** | 389 |
| `marisa-trie` (4 tries, tiny cache — its smallest) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 490 |
| `marisa-trie` (default) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 472 |
| `marisa-trie` (huge cache) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 449 |
| **lexindex `DictIndex` (32 per block, default)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **3.24** | 285 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 317 |
| lexindex `PerfectHashIndex` | — | — | — | — | ✅ | ✅ | ✅ | 10.90 | 216 |
| DAWG (`dawg2`) | ✅ | ✅ | — | — | — | ✅ | — | 23.96 | 246 |
| `datrie` | ✅ | ✅ | — | — | — | ✅ | — | 30.91 | 590 |
| builtin `dict` | — | — | — | — | — | ✅ | — | — (in RAM only) | 260 |

<sub>The two `DictIndex` sizes are re-measured at `6c699c0`, where the per-block offsets are packed
([`bench/results/compare-2026-09-12-arz-6c699c0.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-12-arz-6c699c0.json));
the `ns/lookup` column is not, because three runs that day put the Python call floor at 93, 99 and
102 ns against the 49 the published column was measured at — internally consistent runs, but not
comparable ones, which is the reproducibility problem this page documents further down. Everything
else, and the machine that produced it:
[`bench/results/compare-2026-09-12-arz-bf5c1b9.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-12-arz-bf5c1b9.json)
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. `marisa-trie` appears three times because it is
a curve: its own documentation says the right configuration depends on the data, so the table carries
its compact end, its default and its fast end rather than one point somebody could call untuned.</sub>

**What `ns/lookup` measures.** One exact lookup through Python — `id` on a lexindex index, `get` on
the tries and on the dict — over 100 000 probes, half members and half strangers made by swapping a
member's last character for another the corpus uses. Shuffled with a fixed seed, because a strided
walk through keys laid out in insertion order is learned by the L2 prefetcher and has reversed a
ranking on this machine before. The strangers stay inside every trie's alphabet on purpose: a miss
spelled with a character no key contains is rejected at the first node by a trie and still hashed in
full by a hash index, which is not a comparison.

Every structure takes one pass per round, five rounds, minimum per cell, with all the others resident
while it runs — a harder memory environment than holding one at a time, and the same one for every
row. Running each candidate to completion in turn is not the same measurement and is what this
benchmark did first: every library was then timed immediately after its own six builds, and
`CompactHashIndex` at a 2-byte fingerprint came out 88 ns in one run and 136 ns in the next. Two
consecutive runs of the interleaved form agree within 1.9 % on ten of the thirteen rows; the other
three differ by 10.7 %, 5.5 % and 3.7 %, and are the cells nearest the call boundary, where a few
nanoseconds is a few per cent.

Two costs are named rather than assumed. The Python call boundary — the same loop calling a function
that does nothing — is **49 ns**, which every row pays and no *difference* between rows contains.
And `.get` on a miss is each library's own miss path, not `try: t[key] except KeyError: default`:
timed apart, a miss costs *less* than a hit everywhere (0.85–0.91×), while that wrapper written out
by hand over marisa's `__getitem__` costs 1.61× a hit
([`bench/results/lookup-fairness-2026-09-12-arz-bf5c1b9.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/lookup-fairness-2026-09-12-arz-bf5c1b9.txt)).
Half the probes are misses, so a library that paid for exceptions would have been charged for CPython
rather than for its own structure.

**The smallest rows are also the fastest, which is not a paradox.** `ClosedHashIndex` and the three
`CompactHashIndex` widths answer in 92–100 ns against a builtin `dict`'s 260, because they store no
keys at all: one hash, one probe, at most a fingerprint to compare. What they cannot do is tell a
stranger from a member with certainty, or give a key back for an id. Among the structures that do
keep their keys, `DictIndex` at 128 per block is **both smaller and faster than every `marisa-trie`
setting measured** — 2.83 B/key and 389 ns against 2.96–3.07 and 449–490 — and at 32 per block it
answers in 285 ns for 3.24 bytes.

Two honest crowns, both scoped to what is measured above — libraries a Python or Rust project can
actually install. Research-grade C++ (CoCo-trie, XCDAT, PDT, SuRF) has no bindings to benchmark and
is not claimed against. **`CompactHashIndex` is the smallest `string → dense id` map here — 2.4×
below `marisa-trie` at the default 8-bit fingerprint, 3.9× at 4 bits** — when you can accept a bounded
false-positive rate (about `2^-fingerprint_bits` by design — the fingerprint comes from a second hash,
uncorrelated with the slot hash for well-distributed keys — measured **6.2530 %** at 4 bits and **1.5553 %** at 6 over 2 M non-member probes,
z = +0.18 / −0.83 against theory; **≈0.4 %** at 8 bits, **≈0.0015 %** at 16) and don't need
`id → key`. It is not a security primitive: both hashes are deterministic and unseeded, so an
adversary who chooses the queries can find false positives at will. It stays below
marisa's 2.98 B/key at every width up to 21 bits — the [width guide](usage.md) tables the
trade-off. **`StringIndex` is the only structure here that answers *fuzzy* and subsequence
queries**, at 4× below a plain DAWG; an ordered range is a range of ids on `DictIndex`, which
needs no automaton for it. On this corpus `DictIndex` at 128 keys per block is smaller than
`marisa-trie` and answers everything marisa does plus `key(id)`, `lower_bound` and `range` — a
result about these words at these settings, not a general ranking: a trie's size swings 3×
across corpora and marisa carries tuning parameters of its own.

## Against other Rust string indexes

`marisa-trie` is C++, but since 2026-01 it has a pure-Rust port: [`rsmarisa`](https://crates.io/crates/rsmarisa),
BSD-2-Clause, with the same operations — exact lookup, reverse lookup, common-prefix and predictive
search, mmap, and binary compatibility with the C++ format. **This page used to say no succinct
LOUDS trie existed in Rust to depend on, and rested a "smallest pure-Rust ordered index" claim on
it. Both were wrong**, and the second was wrong about this crate too: `DictIndex` shipped at
3.52 B/key in 2.0 and was never added to the table below. Measured rather than argued, same real
words, one process:

| Rust structure | bytes/key | vs C++ marisa | `id` member | build |
|---|---:|---:|---:|---:|
| **lexindex `DictIndex`** (128 per block) | **2.827** | 0.95× | 435 ns | 38 ms |
| `marisa-trie` (C++ reference, default) | 2.978 | 1.00× | — | — |
| `rsmarisa` (4 tries, tiny cache — its smallest) | 3.003 | 1.01× | 445 ns | 174 ms |
| `rsmarisa` 0.4.2 (default) | 3.168 | 1.06× | 426 ns | 202 ms |
| **lexindex `DictIndex`** (32 per block, default) | 3.245 | 1.09× | **323 ns** | **39 ms** |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.63× | — | — |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | 5.95 | 2.00× | — | — |
| `yada` (double-array) | 15.98 | 5.4× | — | — |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× | — | — |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× | — | — |

<sub>`rsmarisa` and `DictIndex` from
[`bench/results/rsmarisa-2026-09-12-arz-d82e296.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-12-arz-d82e296.txt)
— one process, every key probed as a member and with a digit appended as a miss, shuffled with a
fixed seed, seven rounds alternating in both directions, minimum of the last five. The other rows
are the older sweep with `crawdad` 0.4, `yada` 0.5, `fst` 0.4; size is serialised bytes ÷ keys
throughout, and `rsmarisa`'s `io_size()` is asserted equal to the file it saves. None of them is a
lexindex dependency — the harness is a throwaway crate. The two `DictIndex` sizes are the current
ones, re-measured at `6c699c0` after the per-block offsets were packed; the latencies are the
artifact's, on the tree before it, where `id` measures the same at every block size
([`dict-offsets-2026-09-12-arz-6c699c0.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-offsets-2026-09-12-arz-6c699c0.txt)).</sub>

So the honest statement is a frontier rather than a crown. **`DictIndex` at 128 per block dominates
`rsmarisa` at its most compact setting outright** — smaller, faster on members and misses, and 4.6×
faster to build — and at 32 per block it is the fastest structure here at 323 ns, while being larger
than either `rsmarisa` setting. The block size picks the axis; neither choice is beaten on both.

Two further things the table settles. The Rust port is *larger* than the C++ original on this corpus
(+6.4 % at the default, +1.6 % at its smallest), so a pure-Rust project pays for the port. And
reaching `marisa`'s 2.98 *as a trie* remains closed to the `fst` route: the byte-oriented automaton
is ~1.6× away by construction, and even a bare `fst::Set` storing no ids at all is 4.85. `DictIndex`
gets under marisa by not being a trie — front-coded blocks under one FSST table — and
`CompactHashIndex` takes the size crown the third way, by dropping the keys entirely.

## Which one to pick, and how much the corpus decides it

Every size above is one corpus at one `n`, and the ranking is stable across neither. The reason is
structural: a trie's size depends on how much the keys share, and a fingerprint index's does not.
The same structures over three corpora built from the same word list, one process per cell
(`local/positioning.py`):

| bytes/key | 479 823 single words | 1 M `word.word` pairs, drawn at random | 1 M `word.word`, 1 000 × 1 000 grid |
|---|---:|---:|---:|
| the bare MPHF (no keys, no membership, no reverse) | 0.26 | 0.26 | 0.26 |
| **lexindex `CompactHashIndex`** (fp = 1 byte) | **1.26** | **1.26** | **1.26** |
| `marisa-trie` | 2.98 | 6.21 | 2.12 |
| **lexindex `DictIndex`** (32 per block) | **3.24** | (not measured) | 2.91 |
| **lexindex `StringIndex`** | 5.95 | 15.19 | 0.68 |
| lexindex `PerfectHashIndex` | 10.90 | 21.93 | 12.52 |

At 10 M the trie numbers move again — `marisa` 4.14 on random pairs against 2.36 on the grid,
`StringIndex` 12.44 against 2.00 — while `CompactHashIndex` stays at 1.26 and the bare MPHF at 0.26,
because their size is a function of `n` and the fingerprint width alone. The grid is a full cross
product and is the *most* favourable set a trie can be handed; it is what the scale table below uses,
and on it `StringIndex` at 0.68 B/key undercuts even a keyless perfect hash. Treat that as the
ceiling of what shared structure can buy, not as a headline.

So, in decision order:

- **Do the keys need to come back out, or be scanned in order?** If yes, the fingerprint indexes are
  out; `StringIndex` (ordered, prefix / range / fuzzy / subsequence), `DictIndex` (ordered,
  `id → key`, `lower_bound`, `prefix`, `range`, no automata so no fuzzy — the smallest of the
  three on single words) or
  `PerfectHashIndex` (exact membership, `id → key`, no ordering) are the candidates, and all three
  pay for the keys they store.
- **Is a bounded false-positive rate acceptable?** If yes, `CompactHashIndex` is 2.4× smaller than
  `marisa-trie` on single words, 4.9× on random pairs and 3.3× at 10 M — and one byte per key
  *larger* than a bare MPHF (1.26 against 0.26), which is exactly the fingerprint that buys the
  membership check.
- **Do the keys share a lot of structure** (a path namespace, a versioned catalogue, a cross product)?
  Then measure before choosing: that is the regime where an FST can beat a keyless hash outright.
- **A `dict` / `HashMap` is not in the table** because it has no serialised form to measure. It cost
  71–95 bytes per key above the key list itself across these corpora (58–60 at 10 M, where the table
  amortises better), and it has to be rebuilt from the keys on every process start; every structure
  here is mapped from a file instead.

## The corpus set

Everything above is one corpus at one `n`, which is why the section before it exists. The set that
can settle the question is built by `bench/corpora.py`: thirteen corpora at 100 000, 1 000 000 and
10 000 000 keys wherever the source has them, nested so that the small file is a prefix of the large
one and a difference between two sizes is scale and never composition.

| corpus | keys available | B/key | what it is |
|---|---:|---:|---|
| `words` | 479 823 | 9.3 | the Fedora word list — every table above is this one |
| `titles-en` | 19 217 770 | 21.0 | English Wikipedia article titles |
| `titles-ru` | 4 988 075 | 35.8 | Russian titles: Cyrillic, two bytes a character |
| `titles-zh` | 2 997 733 | 16.9 | Chinese titles: three bytes a character, and short |
| `urls` | 19 217 771 | 52.4 | those titles as `https://en.wikipedia.org/wiki/…` |
| `domains` | 1 000 000 | 13.8 | registered domains, the Tranco ranking |
| `pypi` | 889 864 | 13.3 | PyPI package names |
| `paths` | 7 311 150 | 125.1 | this machine's filesystem |
| `idents` | 1 047 267 | 17.3 | identifiers in the vendored Rust sources |
| `uuid` | generated | 36.0 | UUIDv4, hyphenated |
| `numeric` | generated | 4.9–6.9 | dense decimal ids, `0` to `n-1` |
| `opaque` | generated | 16.0 | 16-symbol base64url ids |
| `dna` | generated | 24.0 | 24-mers over ACGT |

The four generated corpora are seeded rather than random: the same file comes out of the same
version of the script, on any machine. The fetched ones are pinned — a *dated* Wikipedia dump and
not `latest`, a Tranco list by its permanent id — and both the download and every file derived from
it carry a SHA-256 in
[`bench/corpora.json`](https://github.com/ilgrad/lexindex/blob/main/bench/corpora.json).
`python bench/corpora.py verify` re-hashes what is on disk and says which files have moved. The
corpora themselves are gitignored: 4.6 GB of keys has no place in a git history, and the manifest is
what makes them checkable without it.

Two of them are honest about their limits. `paths` is this machine's `/usr` and `$HOME`, so it is
reproducible nowhere else — the manifest records the host. `idents` is whatever crates this machine
has vendored, which is a sample of Rust, not of source code.

Nothing on this page is measured on the set yet; every table here is still `words`. Moving them over
is the next benchmark task, and the first thing it should settle is the claim no single corpus can
support: which structure is smallest, and *where*.

## Every structure over the whole set

`bench/sweep.py` builds nine structures on each corpus and measures serialised bytes per key, build
time and one lookup: three `DictIndex` block sizes against three `marisa-trie` configurations,
because both are curves and a single point of either invites the objection that the other was left
untuned. The two keyless indexes are there to show what a structure that stores no keys costs, which
turns out to be the only row a reader can carry to their own corpus without measuring it.

### Bytes per key at one million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 32 | `Dict` 128 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 8.37 | 7.72 | 7.53 | 17.74 | 7.52 | 7.56 | 7.71 |
| `domains` | 13.8 | 0.26 | 1.26 | 5.48 | 5.01 | 4.90 | 10.50 | 4.81 | 4.87 | 4.99 |
| `idents` | 17.3 | 0.26 | 1.26 | 7.66 | 7.21 | 7.06 | 10.55 | 5.45 | 5.62 | 5.74 |
| `numeric` | 5.9 | 0.26 | 1.26 | 2.46 | 2.12 | 2.02 | 0.00 | 1.62 | 1.64 | 1.72 |
| `opaque` | 16.0 | 0.26 | 1.26 | 14.10 | 13.78 | 13.69 | 21.44 | 18.23 | 18.82 | 19.04 |
| `paths` | 125.0 | 0.26 | 1.26 | 18.80 | 15.97 | 15.58 | 17.50 | 9.28 | 9.49 | 9.61 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 10.04 | 9.51 | 9.33 | 17.28 | 7.79 | 8.19 | 8.37 |
| `titles-ru` | 35.8 | 0.26 | 1.26 | 12.92 | 12.14 | 11.90 | 31.75 | 8.07 | 8.61 | 8.77 |
| `titles-zh` | 16.9 | 0.26 | 1.26 | 9.12 | 8.67 | 8.60 | 17.52 | 6.33 | 6.54 | 6.70 |
| `urls` | 52.4 | 0.26 | 1.26 | 12.73 | 11.49 | 11.20 | 16.88 | 7.95 | 8.39 | 8.58 |
| `uuid` | 36.0 | 0.26 | 1.26 | 21.10 | 20.48 | 20.25 | 37.11 | 32.93 | 34.58 | 34.80 |

<sub>`words` and `pypi` have no million-key file; their 100 000 grid, every build time, and the
100 000 rows for the rest are in
[`bench/results/sweep-2026-09-12-arz-6c699c0-dirty.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-12-arz-6c699c0-dirty.json)
(the tree is the commit; what was uncommitted is the result files this run and the two before it
wrote).</sub>

**Where `marisa-trie` wins, it wins on shared structure.** It is the smallest key-storing structure
on eight of these eleven corpora, and the margin tracks how much the keys have in common: 1.70× on
`paths`, where a million paths run through a few thousand directories, 1.48× on `titles-ru`, 1.41×
on `urls`, 1.30× on `idents` — and level on `dna` and `domains` (1.00× and 1.02×). That is what a
LOUDS trie is for, and front coding in fixed blocks does not answer it.

**Where lexindex wins, it wins on entropy.** `DictIndex` is the smaller of the two on `opaque`
(13.70 against 18.23, 1.33×) and on `uuid` (20.25 against 32.93, 1.63×) — keys with nothing to
share, where a trie pays for a node per character and front coding pays for a prefix that is not
there. On `words`, the corpus every table above is measured on, `DictIndex` at 128 per block is 3.46
against marisa's 3.70 at 100 000 keys, and 2.83 against 2.96 on the full 479 823. That is a real
result on a real corpus, and it is not the general case: it is the favourable end of a distribution
whose other end is `paths`.

**The two keyless rows are flat and every other row is not.** `ClosedHashIndex` is 0.26 bytes a key
and `CompactHashIndex` 1.26 on all eleven corpora at all three sizes, because their size is a
function of `n` and the fingerprint width and of nothing about the keys. Against the best trie that
is a 7.3× margin on `paths` and 1.3× on `numeric` — the same two structures, neither of them
changed, and the whole spread between those two numbers belongs to the corpus.

**`StringIndex` on `numeric` reads 0.00 bytes a key, and it is not a bug.** Ten million dense
decimal ids compile to an FST of **356 bytes**: the automaton is very nearly regular, one path per
digit, and the keys are recovered from the transitions. Read it as the ceiling of what shared
structure can buy an FST and never as a size claim — it is exactly the ~0 B/key that
`bench/compare.py` refuses to report for synthetic `entity-{i}` keys, and it is in this table only
because dense ids are a corpus somebody really has.

### Lookups at one million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 32 | `Dict` 128 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 83 | 126 | 339 | 395 | 1216 | 672 | 957 | 937 | 921 |
| `domains` | 13.8 | 80 | 130 | 344 | 433 | 1406 | 485 | 745 | 718 | 694 |
| `idents` | 17.3 | 89 | 138 | 439 | 550 | 2017 | 524 | 973 | 906 | 873 |
| `numeric` | 5.9 | 66 | 100 | 239 | 312 | 1166 | 170 | 291 | 287 | 264 |
| `opaque` | 16.0 | 103 | 131 | 367 | 518 | 1490 | 489 | 1556 | 1194 | 1134 |
| `paths` | 125.1 | 142 | 170 | 747 | 922 | 2808 | 1449 | 3097 | 2566 | 2284 |
| `titles-en` | 21.0 | 104 | 134 | 415 | 585 | 2006 | 690 | 1213 | 1155 | 1108 |
| `titles-ru` | 35.8 | 139 | 166 | 514 | 708 | 2654 | 942 | 1681 | 1553 | 1491 |
| `titles-zh` | 16.9 | 115 | 146 | 413 | 579 | 1972 | 618 | 1071 | 1034 | 985 |
| `urls` | 52.4 | 104 | 141 | 617 | 779 | 2436 | 866 | 1308 | 1220 | 1187 |
| `uuid` | 36.0 | 117 | 133 | 438 | 633 | 2247 | 665 | 1769 | 1333 | 1276 |

**`DictIndex` answers faster than every `marisa-trie` setting on every corpus in the set** — 1.1×
on `numeric` and 1.9–3.1× on the other ten, against marisa's *fastest* configuration and not its
smallest. It also builds 2.3–4.4× faster everywhere except `numeric`, where the two are level. So
the size table above is not the whole trade: on `paths`, marisa is 1.70× smaller and 3.1× slower to
answer and 4.4× slower to build.

**The block size is the knob that buys the bytes.** On `paths`, 32 → 1024 per block takes the index
from 18.80 to 15.58 bytes a key (17 % smaller) and a lookup from 747 to 2808 ns (3.8× slower),
because a block is walked from its head and a longer block is a longer walk. 128 is the middle the
README quotes; 1024 is the end of the curve, not a recommendation.

### Ten million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 32 | `Dict` 128 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 7.72 | 7.05 | 6.85 | 15.98 | 6.69 | 6.75 | 6.99 |
| `numeric` | 6.9 | 0.26 | 1.26 | 2.49 | 2.13 | 2.02 | 0.00 | 1.63 | 1.66 | 1.77 |
| `opaque` | 16.0 | 0.26 | 1.26 | 13.68 | 13.35 | 13.26 | 21.47 | 15.19 | 18.33 | 18.68 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 8.44 | 7.82 | 7.78 | 13.25 | 5.57 | 5.71 | 5.87 |
| `urls` | 52.4 | 0.26 | 1.26 | 10.95 | 9.74 | 9.35 | 13.10 | 5.66 | 5.83 | 5.99 |
| `uuid` | 36.0 | 0.26 | 1.26 | 20.62 | 20.00 | 19.77 | 36.07 | 29.97 | 33.21 | 33.56 |

Ten times the keys moves every trie and neither hash: at its smallest setting `marisa` goes
7.79 → 5.57 on `titles-en` and 7.95 → 5.66 on `urls` as the sharing deepens, `DictIndex` 9.51 → 7.82 and 11.49 → 9.74, and the two
keyless rows do not move at all. The ranking is the same one the million-key table gives, so the
answer to "which is smallest" is decided by the corpus and not by the scale.

<sub>Measured 2026-09-12
([`bench/results/sweep10m-2026-09-12-arz-6c699c0-dirty.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-09-12-arz-6c699c0-dirty.json)),
`marisa-trie` 1.3. One build per cell at this size, five rounds of lookups.</sub>

### A cold mapping, and what is actually resident

Every size in this page is the blob. A process that maps one and answers a thousand queries never
reads most of it, and what decides whether an index fits inside a container limit is the resident
set. `local/coldmmap` drops each file's page cache with `posix_fadvise(POSIX_FADV_DONTNEED)` — no
root needed, and the resident set straight after `load_mmap` is the control that the drop worked —
then reads `/proc/self/smaps` after 1 000 random lookups and after a further million.

Ten million English Wikipedia titles, bytes per key except the latencies:

| structure | file | mapped | after 1 k | after 1 M | cold ns | warm ns |
|---|---:|---:|---:|---:|---:|---:|
| `DictIndex` 32 | 8.69 | 0.26 | 5.53 | 8.69 | 50 442 | 501 |
| `DictIndex` 32, `MADV_RANDOM` | 8.69 | 0.26 | **1.92** | 8.69 | 94 363 | 1 837 |
| `DictIndex` 128 | 7.88 | 0.08 | 4.69 | 7.88 | 51 519 | 642 |
| `DictIndex` 128, `MADV_RANDOM` | 7.88 | 0.08 | **0.82** | 7.88 | 89 071 | 2 072 |
| `DictIndex` 1024 | 7.79 | 0.02 | 4.61 | 7.79 | 52 720 | 2 108 |
| `CompactHashIndex` | 1.26 | 0.26 | 1.25 | 1.26 | **6 290** | **49** |
| `CompactHashIndex`, `MADV_RANDOM` | 1.26 | 0.26 | 0.70 | 1.26 | 77 906 | 164 |
| `StringIndex` | 13.25 | 0.01 | 10.10 | 13.25 | 89 262 | 866 |
| `StringIndex`, `MADV_RANDOM` | 13.25 | 0.01 | 2.27 | 13.25 | 374 698 | 3 105 |

**`load_mmap` really is lazy**, which the `mapped` column exists to prove: 0.01 to 0.26 bytes a key
resident before the first query, which is the header and the little the loader validates. Nothing
else is read until something asks for it.

**The first thousand queries cost far more pages than they need.** `DictIndex` at 128 ends them with
4.69 of its 7.88 bytes a key resident — 60 % of an index nobody has finished reading — while the
same thousand queries under `MADV_RANDOM` leave **0.82**, which is what they actually touch: about
two pages a lookup, the sample array and the block. The 5.7× between those two numbers is the
kernel's readahead, and it is buying latency with memory: turning it off costs 1.7× on the cold
lookups and **3.2× on the warm ones**, because the advice outlives the warm-up. Readahead is the
right default here; `MADV_RANDOM` is for the case where a container limit, and not a latency budget,
is what binds.

**Cold start is where the smallest structure wins outright, and the mechanism is pages.**
`CompactHashIndex` answers its first thousand queries at **6.3 µs** against `DictIndex`'s 51.5 and
`StringIndex`'s 89.3 — 8× and 14× — because its whole file is 12.6 MB and a fault brings in a
useful fraction of it. On `uuid`, where its 1.26 bytes a key sit against `DictIndex`'s 20.06 and
`StringIndex`'s 36.07, the gap is 21× and 39× (5.9 µs against 125.3 and 231.0). A structure that
stores no keys has no keys to fault in.

**After a million queries every structure is fully resident**, to the last hundredth of a byte. The
distinctive answer to "how much memory does this index need" only exists during warm-up: past it,
against a workload that touches every key, the resident set *is* the file, and the size table above
is the steady-state RSS.

<sub>Measured 2026-09-12 on a clean tree, NVMe under LUKS on btrfs, 38 GB RAM — so "cold" means
this file's page cache was dropped and not that the machine was short of memory
([`bench/results/coldmmap-2026-09-12-arz-bb1473c.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-09-12-arz-bb1473c.txt),
which also carries the million-key run and the `uuid` one). `MADV_RANDOM` is applied by the harness
through `/proc/self/maps`; `load_mmap` does not set it, and on this evidence should not.</sub>

### One mapping, many readers

Every index here is immutable once built. A reader needs no lock, no copy and no per-thread
instance: one `load_mmap` is shared by reference and answers from as many threads as there are
cores. `local/readscale` is whether that is near-linear — 8 Mi random member probes split evenly,
the mapping warmed first so this measures the structure and not page faults, the ladder run three
times and the best kept per thread count.

Ten million English Wikipedia titles, nanoseconds a lookup with the speedup over one thread:

| threads | `DictIndex` 128 | `CompactHashIndex` | `StringIndex` |
|---|---:|---:|---:|
| 1 | 630 | 32.6 | 813 |
| 2 | 323 (1.95×) | 17.8 (1.83×) | 401 (2.03×) |
| 4 | 163 (3.87×) | 9.3 (3.51×) | 199 (4.08×) |
| 8 | 84 (7.48×) | 4.9 (6.71×) | 103 (7.93×) |
| 16 | 56 (11.36×) | 3.7 (8.72×) | 61 (13.23×) |
| **M lookups/s at 16** | **18.0** | **267.9** | **16.3** |

**Eight cores give 6.7–7.9×**, which is 84–99 % of them, and this is a mobile part whose clock falls
as cores light up — the number already carries that, so a machine with a flatter boost curve can
only do better.

**The sixteen-thread row is SMT and it is worth having.** 8.7–13.2× over one thread, well past the
eight physical cores, because a point lookup is a chain of dependent loads and a core spends most of
it waiting. A second thread on the same core fills those stalls with someone else's work.

**`StringIndex` scales best because it stalls most** — 2.03× on two threads and 13.23× on sixteen.
One core can only keep a handful of cache misses in flight; two cores have twice the
memory-level parallelism, and a transducer walk is nothing but dependent misses. The same effect at
100 000 keys is stronger still (2.27× and 12.99×).

**`CompactHashIndex` saturates first**, at 8.72×, and it is the one structure that has run out of
something other than cores: 268 M lookups a second over a 12.6 MB index that fits this machine's
16 MB L3, at 3.7 ns a lookup. The others are answering out of DRAM and have latency left to overlap;
this one does not.

<sub>Measured 2026-09-12 on a clean tree
([`bench/results/readscale-2026-09-12-arz-694ee95.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/readscale-2026-09-12-arz-694ee95.txt),
which carries the million-key ladder as well), Ryzen 7 5800HS, 8 cores and 16 hardware threads.
Shared by `&index` across `std::thread::scope`; no index is cloned and none is rebuilt per
thread.</sub>

### `rsmarisa`, the pure-Rust port

The same question in Rust, where `marisa-trie` is a C++ library with no binding a Rust project would
take. `rsmarisa` 0.4.2 is a pure-Rust port of it, BSD-2-Clause, and `local/rsmarisacmp` puts its
three cache levels against the same three block sizes on every corpus:

| corpus | rsmarisa smallest | `DictIndex` 1024 | rsmarisa build | `DictIndex` build | rsmarisa fastest | `DictIndex` 32 |
|---|---:|---:|---:|---:|---:|---:|
| `dna` | 7.61 | 7.54 | 1 349 ms | 60 ms | 748 ns | 291 ns |
| `domains` | 4.87 | 4.91 | 704 ms | 55 ms | 502 ns | 289 ns |
| `idents` | 5.50 | 7.07 | 663 ms | 60 ms | 637 ns | 346 ns |
| `numeric` | 1.64 | 2.02 | 99 ms | 46 ms | 164 ns | 181 ns |
| `opaque` | 18.27 | 13.70 | 1 501 ms | 76 ms | 1 075 ns | 317 ns |
| `paths` | 10.07 | 15.72 | 5 824 ms | 97 ms | 1 747 ns | 680 ns |
| `titles-en` | 7.85 | 9.34 | 976 ms | 68 ms | 887 ns | 357 ns |
| `titles-ru` | 8.22 | 11.91 | 1 510 ms | 68 ms | 1 122 ns | 434 ns |
| `titles-zh` | 6.40 | 8.61 | 740 ms | 66 ms | 734 ns | 337 ns |
| `urls` | 8.38 | 11.21 | 2 055 ms | 72 ms | 970 ns | 558 ns |
| `uuid` | 33.01 | 20.25 | 2 010 ms | 110 ms | 1 204 ns | 404 ns |

The shape is the one the C++ library gives: `rsmarisa` is smaller on the corpora whose keys share
structure and larger on the ones whose keys do not, while `DictIndex` builds **11–60× faster** and
answers **1.7–3.4× faster** on ten of the eleven. `numeric` is the exception on every axis:
`rsmarisa` is smaller there, marginally quicker to answer, and only 2.2× slower to build. At
nominally the same configuration `rsmarisa` is larger than the C++ marisa measured above — on
`words`, 0.2 % at the smallest setting, 3.1 % at the default and 12 % at the fastest — so the flag
words evidently do not mean quite the same thing, and each library's own curve is what to read.

<sub>Measured 2026-09-12 on a clean tree
([`bench/results/rsmarisa-2026-09-12-arz-4762dc4.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-12-arz-4762dc4.txt)),
`rsmarisa` 0.4.2, in one process per corpus, six lanes alternating within each of five rounds. It is
a throwaway crate under `local/`: `rsmarisa` is not a lexindex dependency and is not proposed as
one. The Rust build times are not comparable with the Python ones above — that harness copies every
key across the language boundary and this one does not.</sub>

## Lookup speed from Python, against `dict` and `marisa-trie`

`local/latency_py.py` — one process per corpus, every structure built up front, the seven lookup
forms rotated inside each round so none keeps the position that pays to warm the probe list.
**Measured on 1.1**, from the minimum over two agreeing 15-round passes per corpus. Ratios are
**quotients of the minima against `dict` on the same probe set**, and the gate is on *those*: worst
7.5 % / median 1.7 % (`words`), 4.7 % / 1.6 % (`random`), 6.8 % / 2.1 % (`grid`), against a bar of
10 %. It used to sit on the absolute minima and refused all three tables whose ratios agreed to
1–8 %, with `dict` the worst offender in every one — the minima carry the machine's thermal state,
which is exactly what the ratio divides out. `grid` needed a third pass: its second moved every
`marisa-trie` cell by 16 % while no other column moved 2 %, and passes 1 and 3 agree.

**Nothing about a lexindex *version* can be read out of this table**, and that is a measured claim,
not a disclaimer. Same session, alternating **A-B-B-A** (the order matters: A-B-A-B gives A both
early slots and this machine drifts), the released 1.0.0 wheel against this working tree — on
`random` every cell landed within 1.01–1.04×, on `words` within 0.98–1.09×, and the widest gaps in
both belong to `dict` and `marisa-trie`, code that is byte-identical in the two runs. Two readings
the cross-session numbers seemed to support die there: `PerfectHashIndex.id` did **not** gain 9 %
against `dict` between 1.0 and 1.1, and `CompactHashIndex.ids_of` did **not** lose 37 % on absent
pair-corpus probes — today's 1.0.0 wheel measures that cell at 62.0 ns against the 44.8 the 1.0
table published for it. The blocked arena's Rust-level win on `id` (216 → 182 ns) is real and
measured by `cargo run --release --example bench`; it does not survive the binding, because ~180 ns
of per-call overhead sits on top of every Python lookup.

| probe set | structure | 479 823 words | 1 M random pairs | 1 M grid pairs |
|---|---|---:|---:|---:|
| members | `dict` absolute, for scale | 267.8 ns | 270.6 ns | 227.3 ns |
| | `marisa-trie` | 2.16× | 4.20× | 2.79× |
| | `StringIndex.id` | 1.56× | 2.64× | 1.54× |
| | `PerfectHashIndex.id` | 1.00× | 1.20× | 1.19× |
| | **`CompactHashIndex.id`** | **0.70×** | **0.72×** | **0.61×** |
| | `PerfectHashIndex.ids_of` | 0.49× | 0.52× | 0.66× |
| | **`CompactHashIndex.ids_of`** | **0.34×** | **0.34×** | **0.47×** |
| absent | `dict` absolute, for scale | 180.0 ns | 242.6 ns | 252.0 ns |
| | `marisa-trie` | 2.72× | 4.38× | 2.35× |
| | `StringIndex.id` | 1.66× | 2.28× | 1.15× |
| | `PerfectHashIndex.id` | 0.70× | 0.79× | 0.81× |
| | **`CompactHashIndex.id`** | **0.39×** | **0.31×** | **0.29×** |
| | **`CompactHashIndex.ids_of`** | **0.31×** | **0.26×** | **0.24×** |

Below 1.00× is faster than `dict`. So: a `CompactHashIndex` answers a **present** key in 0.61–0.72×
the time of a `dict` and a **missing** one in well under half, batched `ids_of` in a quarter to just
under half — while occupying 1.26 bytes per key on disk against the `dict`'s 71–95 bytes per key in
RAM. `PerfectHashIndex` trades level with `dict` on single words, costs a fifth more on the pair
corpora, and wins on misses everywhere; `marisa-trie` costs 2.2–4.4× and `StringIndex` 1.2–2.6×, and
both swing with the corpus exactly as their sizes do.

<sub>Read the ratios, not the absolutes. This machine drifts 6–17 % over fifteen rounds **while
idle** — measured with a cache-resident integer loop that touches no memory — so absolute
nanoseconds here are a statement about one laptop's thermal envelope. (One `random` pass drifted
176 % when a neighbouring job woke up; its minima still agreed with the quiet pass to 3 %, which is
the case for taking the minimum rather than the mean.) The ratios divide that out *within* a session. Two caveats in `dict`'s favour,
both deliberate: CPython caches a string's hash inside the object, so a repeated probe over the same
`str` skips rehashing where lexindex hashes the bytes every call (~23 ns of the gap at 1 M); and
every column pays the same per-call binding overhead, which flatters the slower ones. **The
denominator's instability across sessions is a standing, unexplained property of this machine**, and
it has now been seen four times: `grid`'s `dict` read 251 ns, then 194–196 the next day, then 216,
and 227.3 here; `words`' read 327.9, then 258.5, then 267.8. Ruled out on the day it was first chased: the extension (the
released wheel measured the same as the working tree), the interpreter (one virtualenv throughout),
the corpus generator (unchanged) and background load (raising it did not restore the old figures).
The mechanism is still not identified. What follows for the protocol is that a *cross-session* delta
in this table means nothing on its own — not even as a shape across columns, which the A-B-B-A run
above tested directly and found empty. A version claim needs both builds in one session.</sub>

## Point-lookup latency vs the standard library

`cargo run --release --example bench` — 1 M **real dictionary-word bigrams** (`word_i.word_j`, the
same key generator as `bench/scale.py`; mean key 10.9 bytes). Keys are never synthetic
`entity-000…N` sequences — those arrive pre-sorted and hash-degenerate and flatter every number.
Measured on 2.0.0 (the better of two runs of the example on a rested machine, load 0.9–1.1;
each lookup cell is the minimum of five timed passes after a warm-up pass;
[`bench/results/latency-rs-2026-09-10-arz-16c7abe.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-09-10-arz-16c7abe.txt)).
Absolute numbers are machine-dependent — this session reads the `std::HashMap` control 18 %
*slower* than the one that produced the 1.1.0 table (245 → 289 ns), and `StringIndex`, whose
lookup code has not changed since 0.5.1, moved from 1.30× to 1.47× of it — so compare the
**ratios**, only within a column, and read a shift under ~15 % between tables as the session:
against that `HashMap`, `CompactHashIndex::id` is 0.45×, `id_unchecked` 0.26×,
`PerfectHashIndex::id` 1.04×, `StringIndex` 1.47×, `DictIndex` 1.76×, `BTreeMap` 3.34×. What did
move past the drift is a build: `CompactHashIndex` builds in 45 ms against 69 on 1.1.0 while every
other build cell reads 10–17 % slower than there — 2.0's placement on every thread.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~45 ms** | ~130 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~275 ms | **~74 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~208 ms | ~289 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~280 ms | ~301 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~271 ms | ~424 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (32 per block) | ~203 ms | ~507 ns | ordered, exact reverse; 2.91 B/key here against the FST's 0.68 — a `word.word` cross product is what a transducer factors out, and what a block of front-coded keys does not (on the dictionary: 3.24 against 5.95, 307–311 ns against 333–353) |
| `std::BTreeMap<String, u32>` | ~226 ms | ~960 ns | in-RAM |

<sub>**Against the 1.1.0 table, the rows whose code did not move drifted together**: `StringIndex`
1.30× → 1.47× of `HashMap`, `BTreeMap` 3.19× → 3.34×, the FxHash map 0.586× → 0.60× — the
memory-bound structures lost more to this session than the cache-friendlier map did, which is
what the rested-but-warmer laptop looks like. The rows 2.0's hash reaches moved by less than that
control: `PerfectHashIndex::id` 0.95× → 1.04×, `id_unchecked` 0.269× → 0.26×, `CompactHashIndex::id`
0.435× → 0.45× — so the new key hash costs nothing a table can see, as the in-process A-B-A-B on the
shuffled dictionary said (`CompactHashIndex::id` within its harness's own layout noise). The
**build** column is where 2.0 shows: `CompactHashIndex` 69 → 45 ms while `HashMap`'s build reads
178 → 208 and `StringIndex`'s 248 → 271 — the slots computed on every thread and a fingerprint
written as one store. `DictIndex` is new in the table, and this corpus is its worst case by
construction: the 1 M keys are 1 000 words crossed with 1 000, which the transducer stores once per
factor (0.68 B/key) and a block of front-coded keys stores once per key. Real keys move lookups in
lexindex's favour versus synthetic ones, while every `build` reads higher than a synthetic sequence
would, because real input is not pre-sorted and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~173 ns** and `PerfectHashIndex::id_unchecked`
**~74 ns** — so on a closed vocabulary the perfect hash is about **2.3× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash. `CompactHashIndex::id` (~130 ns) is
25 % *faster* than the FxHash map and still carries the membership check and the 1.26 B/key
blob.

**Two things the table above cannot show, both measured on 0.11 with an independent harness
(`local/latency/`, one process, all forms alternated per round, min of 12):**

- **Roughly 90 ns of every number in it is reaching the probe key, not looking it up.** The bench
  probes `keys[i * STEP % n]` — the original allocations in strided order, which is what a
  long-lived key list looks like. Hand the same index a probe list allocated in probe order and
  `PerfectHashIndex::id_unchecked` falls from 109 to **18 ns/op** at 1 M, `CompactHashIndex::id`
  from 126 to 33, while `PerfectHashIndex::id` barely moves (262 → 192; it fetches a stored key
  either way). The batched `ids_of` is layout-insensitive by construction — 39.5 scattered against
  38.2 contiguous — because its software prefetch does for scattered keys what the hardware does for
  contiguous ones. So read any sub-100 ns lookup figure, here or anywhere, as a statement about the
  caller's key layout as much as about the index.
- **On a miss-heavy workload `std::HashMap` wins, until its table outgrows the cache.** An absent key
  costs the SipHash map 32 ns at 1 M against `CompactHashIndex::id`'s 41 — it fails on an empty
  bucket after one cache line, while a fingerprint index runs the whole perfect hash and reads a
  fingerprint before it can say no. At 10 M the map's table no longer fits and the order reverses
  (105 ns against 56 for `ids_of`). lexindex's lookup advantage is on **members**, and at scale.

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest of the structures in the table above** — 3.9× as quick as the SipHash `HashMap` and 2.3×
the FxHash one (no probing, no membership comparison) *and* compact + serialisable.
`CompactHashIndex::id` keeps a probabilistic membership check and *still* beats the SipHash
`HashMap` on lookup (2.2× here), and builds in a fifth of its time. Full verification (`id`) pays one extra
cache line + a key comparison; `StringIndex` trades more latency for **ordered / prefix / range /
fuzzy** queries the hash maps cannot answer at all. So: `CompactHashIndex` when footprint dominates
and a rare false positive is fine; `PerfectHashIndex::id` for exact membership + reverse;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map
with nothing persisted.

## The perfect hash against PtrHash and PHast

`bench/mphf_vs` is its own crate, outside the workspace, so that neither competitor becomes a
dependency of lexindex: `cd bench/mphf_vs && cargo run --release -- 10000000 3 <threads>`. One
process builds every function over the **same 10 M distinct splitmix64 keys** in turn, three rounds
A-B-A-B (builds: the minimum), and looks every key up in **one shuffled probe order** (the minimum
of three passes). `ph` 0.11.0 is the PHast authors' crate: `Function2` with `ShiftOnlyWrapped` is
PHast+ with wrapping, the design this crate's `MPH2` follows; `Function` with `SeedOnly` is regular
PHast. Both at 8-bit seeds and bucket size 4.5, as here. `ptr_hash` 2.1.1 is PtrHash's three
parameter sets.

| function | bits/key | build, 1 thread | build, 8 threads | lookup |
|---|---:|---:|---:|---:|
| **lexindex `MPH2`** | **2.088** | **44.8 ns/key** | **7.6 ns/key** | **29 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.148 | 82.2 | 18.1 | 74 |
| `ph` PHast (`SeedOnly`) | 1.922 | 640.1 | 102.4 | 70 |
| `ptr_hash` compact | 2.143 | 186.7 | 48.0 | 46 |
| `ptr_hash` balanced | 2.378 | 120.0 | 29.6 | 47 |
| `ptr_hash` fast | 2.990 | 174.3 | 160.2 | 26 |

In this harness, at the size this crate chose, about 2.1 bits, `MPH2` builds 1.8× faster than the
PHast+ it is modelled on and 4.2× faster than PtrHash's compact set, and its lookup is the fastest
of the three — with the asymmetry below inside those ratios;
regular PHast is the smallest function here, 1.92 bits, at 14× the build; PtrHash's fast set has
the fastest lookup, 26 ns, at 3 bits. One asymmetry is in the numbers and should be read out of
them: lexindex takes the keys as 64-bit hashes (its indexes hash the string once, before), while
`ph` hashes each key with wyhash on build and on every lookup level and `ptr_hash` with one
multiply — a few nanoseconds of the gap on the `ph` rows is that. PTHash and ConsensusRecSplit are
C++ and are not in the harness.

<sub>Measured 2026-09-10 on the tree that removes the seed-family decode from the lookup
([`bench/results/mphf-vs-2026-09-10-arz-5ba9f36.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-10-arz-5ba9f36.txt)),
Ryzen 7 5800HS, load 1.0–1.3 with an editor open; lexindex's rows match its idle measurements from
`src/mphf.rs`'s sweep (46.3 / 27.5 ns). Against the 2026-09-09 file
([`mphf-vs-2026-09-09-arz-1e24d81.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-09-arz-1e24d81.txt))
the competitors' builds repeat within 2 % and their lookups read 1–7 % slower, while lexindex's
lookup went from 33 to 29 ns: the change, not the machine.</sub>

## A fingerprinted `PerfectHashIndex` on mostly-absent keys

`local/negfp`, a throwaway harness beside the crate, builds the dictionary twice — plain and with
`build_with_fingerprints` — and a `CompactHashIndex` as the control, then probes the 480 k words as
members and, with a digit appended (no dictionary word ends in one, so the length distribution is
the members'), as absent keys, in a shuffled order. The headline rows are taken **one index per
process**, so the two arenas never compete for the cache: six alternations of the five modes in
both orders, ten passes each, the minimum — after three warm-up pairs, because the part is a
mobile one and runs a third faster for the first minute after an idle spell. The in-process
A-B-A-B (all three indexes resident, seven rounds) gives the batched rows and the 50/50 mix.

| probe | plain | with fingerprints | |
|---|---:|---:|---|
| `id`, member | 163 ns | 171 ns | one index per process |
| `id`, absent | 166 ns | **74 ns** | one index per process |
| `id`, 50 % absent | 166 ns | 135 ns | in-process |
| `ids_of`, member | 72 ns | 76 ns | in-process |
| `ids_of`, absent | 70 ns | **47 ns** | in-process |
| `CompactHashIndex::id_unchecked` (control) | 41 ns | 41 ns | |
| bytes per key | 10.90 | 11.90 | exactly +1 |

An absent probe stops at the block: the offset line says no key with that fingerprint is in the
slot, and the key — the second cache miss — is never read. A member pays the second hash and one
compare, a few nanoseconds. The fingerprint bytes follow the block's offsets; interleaving them
with the offsets was measured first and cost a member 6 ns instead of 3, because the offset pair
moved up to 36 bytes from the base and split cache lines more often. And the fingerprint is
compared before the slot's offsets are read: an absent probe is then about 255 instructions long,
the width of the reorder buffer, which is what lets its one cache miss overlap the next probe's.
A three-instruction marker check added for the overflow blocks pushed the path past that and
cost 11 ns with no change in cache or branch misses; moving the fingerprint first shortened it by
the whole span computation and took the absent probe from 91 to 74.

<sub>Measured 2026-09-10 on the tree that adds the overflow blocks and the fingerprint-first check
([`bench/results/negfp-2026-09-10-arz-679a8c6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/negfp-2026-09-10-arz-679a8c6.txt)),
Ryzen 7 5800HS, load 1.0–1.1 with an editor open; the fingerprint layout itself was chosen on
[`negfp-2026-09-10-arz-8f136d3.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/negfp-2026-09-10-arz-8f136d3.txt).</sub>

## `ClosedHashIndex`: the perfect hash alone

`local/closedbench` builds the dictionary as a `ClosedHashIndex` and as a `CompactHashIndex` at the
default width, checks that every word gets the same id from both, then probes the 480 k words in a
shuffled order: nine rounds in one process, the order of the modes reversed on every other round,
the minimum per row. The two `id_unchecked`-style lookups are the same perfect-hash probe and
measure the same; the fingerprint compare on top of it is what `CompactHashIndex::id` pays, and
the batched `ids_of` loses the fingerprint line's prefetch and compare.

| | `ClosedHashIndex` | `CompactHashIndex` (fp=1) |
|---|---:|---:|
| `id`, member | **40 ns** | 68 ns (`id`), 40 ns (`id_unchecked`) |
| `ids_of`, member | **14 ns** | 28 ns |
| bytes per key | **0.263** | 1.263 |

<sub>Measured 2026-09-10 on the tree that adds the type
([`bench/results/closed-2026-09-10-arz-50f240c.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/closed-2026-09-10-arz-50f240c.txt)),
Ryzen 7 5800HS, load about 0.9 with an editor open.</sub>

## `DictIndex`: the ordered dictionary against `StringIndex`

`local/dictbench` builds the dictionary as a `StringIndex` and as a `DictIndex` at five block sizes,
then probes all 479 823 words in a shuffled order — members, strangers (each word with a byte
appended), and every id for the reverse lookup — five rounds in one process, the variants alternated
within each round, the minimum per cell; `StringIndex` is the control.

| | `StringIndex` | `DictIndex` 16 | `DictIndex` **32** | `DictIndex` 64 | `DictIndex` 128 | `DictIndex` 256 |
|---|---:|---:|---:|---:|---:|---:|
| bytes per key | 5.95 | 3.79 | **3.24** | 2.97 | 2.83 | 2.75 |
| of which per-block arrays | — | 0.688 | 0.348 | 0.176 | 0.089 | 0.045 |
| build | 127–139 ms | 21–28 | 20–23 | 19–21 | 20–22 | 21–25 |
| `id`, member | 333–353 ns | 291–297 | **307–311** | 332–333 | 387–388 | 486–489 |
| `id`, stranger | 236–246 ns | 193–197 | 209–214 | 238–240 | 294–295 | 391–392 |
| `key_into` (no allocation) | — | 117 | **168** | 266–267 | 467–469 | 865–866 |
| `key` (owned string) | 493–515 ns | 163–169 | 224 | 330–331 | 533–537 | 933–937 |
| `lower_bound`, stranger | — | 197–200 | 210–214 | 239 | 295–296 | 394–395 |

The two runs in the results file take the block sizes in opposite orders; where they differ the
table gives both. At the default 32, `DictIndex` keeps `id` under `StringIndex` and `key_into` at
a third of its `key` while storing 45 % less; 64 per block matches `StringIndex` on `id` at
2.97 B/key.

**The reverse lookup does not decode the whole block.** An entry stores what it shares with its
predecessor, so an entry whose shared-prefix length is at least a later entry's contributes nothing
that survives to the key being asked for. The ones that do contribute form a strictly increasing
staircase of that length, and a monotonic stack over the block's headers finds it in the pass the
walk already makes: every header is still read — a header is what says where the next one begins —
but only the staircase is decoded. Measured over the dictionary and a path list, the staircase is
2.5 entries deep on average and 12 at the deepest with a block of 32, 18 with a block of 1024, out
of up to 1 023 entries; past 32 the walk stops tracking it and decodes each entry, which is slower
and not wrong. That is worth **−16 % on `key_into` at the default block, −33 % at 128 and −37 % at
256** ([`dict-stair-2026-09-11-arz.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-stair-2026-09-11-arz.txt)
holds the before and after), and it costs no bytes and no format change — the win grows with the
block because the entries it stops decoding are the ones a larger block adds.

**32 is a middle of the curve, not a limit**, and
[`build_with_block`](https://docs.rs/lexindex/latest/lexindex/struct.DictIndex.html#method.build_with_block)
is the knob. The size falls because doubling the block halves the per-block arrays — 0.348 bytes
per key at 32, 0.045 at 256 — while the front-coded block data barely notices, 2.604 against
2.669; at 128 the arrays and the block heads together are 5.7 % of the blob and everything else is
suffixes. The price is that the scan and the header walk are linear in the block. At 128 the index
is **2.83 bytes per key, under the 2.98 marisa stores on this corpus** — and unlike marisa it
answers `key(id)` and `lower_bound` at all, since a marisa id is not the lexicographic rank (7 051
of 19 999 consecutive sorted pairs come back with a decreasing id). Pick 16 or 32 if the reverse
lookup is hot, 128 if the bytes are.

<sub>Measured 2026-09-12 on the tree that packs the per-block offsets
([`bench/results/dict-offsets-2026-09-12-arz-6c699c0.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-offsets-2026-09-12-arz-6c699c0.txt),
which holds the same ladder on the tree before it), Ryzen 7 5800HS, load about 1 with an editor
open. The `StringIndex` control is in every row that has one: it moves with the session and the
`DictIndex` cells move with it.</sub>

## Prefix queries against the tries

A prefix is a range of a sorted dictionary, so `DictIndex` answers one without an automaton:
`prefix_id_range` is two `lower_bound`s and `prefix` walks the run they name. `local/prefixbench.py`
puts that against the tries a Python project can install, on the same words — 20 000 three-byte
prefixes drawn from random words, so the average prefix carries 763 keys.

| | bytes/key | `prefix_count` | first 10 | every match + ids | keys only |
|---|---:|---:|---:|---:|---:|
| **lexindex `DictIndex` 32** | 3.24 | **338 ns** | 1 759 ns | 103 089 ns | 71 915 ns |
| **lexindex `DictIndex` 128** | **2.83** | 511 | 2 348 | 103 716 | 70 566 |
| lexindex `StringIndex` | 5.95 | 579 | 4 608 | 165 623 | 338 080 |
| `marisa-trie` | 2.98 | 122 247 | 2 632 | 97 821 | 93 355 |
| `dawg2` | 23.96 | 67 832 | **1 538** | **49 276** | **49 335** |
| `datrie` | 30.69 | 723 692 | 731 251 | 735 884 | 729 880 |

**Counting is where the structures differ in kind rather than by a constant.** `prefix_count` costs
two order lookups whatever the prefix carries, so it runs 239× faster than marisa's at block 128 and
362× at the default — marisa has to enumerate all 763 matches to count them, its ids not being
lexicographic ranks, so there is no arithmetic to do instead. At the block size that goes under
marisa on *size*, `DictIndex` is also ahead on autocomplete (2 348 ns against 2 632) and on handing
back a prefix's keys (70 566 against 93 355) — with a rank for each, which marisa has none to give.

**Those keys come fastest through the id range, which inverts what this page said until 2026-09-12.**
`prefix_id_range` and then `keys_of` costs 70 566 ns at 128 per block, against 103 716 for `prefix`,
which decodes an id for every match as well. `keys_of` used to re-enter the block for every id and
was the *slower* of the two by 2.1×; it now keeps one walk open across the ids that ascend through a
block, which is exactly the shape an id range hands it. The inversion is `DictIndex`-only:
`StringIndex` has no block to stay inside, its `keys_of` walks the transducer once per id, and
`prefix` is still the API for this query there (165 623 against 338 080). It is also why there is no
`prefix_keys` method: it would be these two calls, and the table says what a third one would buy —
32 %, which two existing calls already have.

**The last two columns are this benchmark's error bar as well as a result.** A trie has no ids to
return, so for `marisa-trie`, `dawg2` and `datrie` they are one call measured twice — and they come
out 4.8 %, 0.1 % and 0.8 % apart. Nothing narrower than that is a finding here: `DictIndex` at 32
against 128 on full enumeration (103 089 against 103 716) is one such non-difference, while the 1.32×
over marisa on keys alone is well outside it.

`dawg2` enumerates about 1.4× faster at 8.3× the bytes, with no reverse lookup and no mmap: a
different point on the curve, not a smaller one. `datrie` is a double-array built for point lookups;
prefix walking is not what it is for.

<sub>Measured 2026-09-12, the minimum over three clean-tree runs
([`bench/results/prefix-2026-09-12-arz-91af71d.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/prefix-2026-09-12-arz-91af71d.txt)),
`marisa-trie` 1.3, `dawg2`, `datrie` over the same word list, Ryzen 7 5800HS, load 1.2–2.1. None
of the three is a lexindex dependency — reproduce in a throwaway environment.</sub>

## Common-prefix queries — the other direction

`prefix` returns the keys a query starts. The reverse question — which *keys* are prefixes of the
query — is what a longest-match tokeniser asks of a vocabulary, and it was the one operation
`marisa-trie`, `dawg2` and `datrie` all answered and lexindex did not. `local/cpbench.py` measures
it on the same words: 20 000 queries, each a real word with one to three more letters on it except
every fourth, which is random letters. Every structure is first held to marisa's answer on 2 000 of
them, so the timings compare the same work.

| | bytes/key | `common_prefix` | `longest_prefix` |
|---|---:|---:|---:|
| `datrie` | 30.69 | **706 ns** | 482 ns |
| **lexindex `StringIndex`** | 5.95 | 726 | **422** |
| `dawg2` | 23.96 | 784 | 810 |
| `marisa-trie` | 2.98 | 1 240 | 965 |
| **lexindex `DictIndex` 32** | 3.24 | 2 157 | 863 |
| **lexindex `DictIndex` 128** | **2.83** | 3 353 | 1 303 |

**This is the query a transducer is shaped for, and the numbers say so.** The query *is* the path:
one walk down the FST, every final state on it a match, `O(query bytes)` whatever the index holds.
`StringIndex` is the fastest structure here on `longest_prefix` and within 3 % of the fastest on
`common_prefix` — at **a fifth of `datrie`'s bytes and a quarter of `dawg2`'s**, and ahead of
`marisa-trie` on both columns at twice marisa's size. It is the *largest* of the ordered indexes in
this crate, and on this one query that is where the bytes went.

**`DictIndex` has no walk to make and the table shows what that costs.** One order lookup per
character boundary, so a ten-character query is ten binary searches where the trie made one
descent: 1.7× marisa's time at block 32, 2.7× at 128. `longest_prefix` is the exception — it starts
at the query and stops at the first hit, so it never pays for the boundaries under the match, and
at block 32 it comes in *under* marisa (863 ns against 965) though at 3.24 bytes per key against
2.98. At block 128, where `DictIndex` is the smaller of the two, marisa is the faster. A caller who
asks this question often should hold a `StringIndex`; one who asks it occasionally, alongside the
ranks and ranges only `DictIndex` gives, can have it for a binary search per character.

<sub>Measured 2026-09-12
([`bench/results/common-prefix-2026-09-12-arz-4fc92ac.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-09-12-arz-4fc92ac.txt)),
three runs agreeing within 2 %, minimum of five alternated rounds each, `marisa-trie` 1.3, `dawg2`,
`datrie`, Ryzen 7 5800HS, load about 1.2. Nanoseconds per query through Python; the call overhead
is in every row, and the three tries are C extensions too. None of them is a lexindex
dependency — reproduce in a throwaway environment.</sub>

## Hash quality

Size and speed both rest on the key hash being indistinguishable from random on *real* keys, so the
claim is measured rather than argued. The battery lives with the hash it tests (`src/hash.rs`,
behind `bench-mphf`) and runs as one test:

```bash
cargo test --release --features bench-mphf -- --ignored --nocapture hash_quality
```

Every statistic is reported as a standard-normal `z`, so one bound reads across the whole battery.
A chi-square goes through the Wilson–Hilferty transform rather than the textbook
`(x − k) / sqrt(2k)`, which is wrong exactly where a threshold sits: at 255 degrees of freedom and
a true `z` of 6.00 the two read 5.99 and 7.07 (checked against `incgam` in PARI/GP). The bound is
`|z| < 6`, a 10⁻⁹ tail per cell; since the battery runs about 44 000 avalanche cells and forty
tables, the largest `|z|` it *should* produce is near `sqrt(2 ln N) ≈ 4.6`.

| test | what it would catch | result |
|---|---|---|
| strict avalanche, both hashes, key lengths 4–33 | an input bit that does not reach every output bit | worst 4.49 over 44 032 cells |
| bit independence, 2 016 output pairs per input bit | two output bits that flip together | worst 4.23 over 258 048 cells |
| two-byte differential, every position pair and bit pair | the pre-2.0 collision family: keys differing at bytes `8i+7` and `8i+11` collided in **both** hashes 13–100 % of the time through 1.1 | **0** double collisions over 17 664 combinations |
| per-corpus distribution — slot-hash collisions, top 12 bits, low 12 bits, the 8-bit fingerprint, and the joint (slot, fingerprint) table | a family of keys the hash folds together, and any correlation between the two hashes | ten corpora, no collisions, every value under 2.4 |

The ten corpora are the shapes that break hashes in practice: dictionary words, word bigrams, a
shared prefix (`https://example.com/a/b/…`), a shared suffix (`…@mail.example.com`), a dense
numeric tail (`key_000000001`), plain decimal integers, UUIDs, Cyrillic, DNA, and filesystem paths.

<sub>Committed output:
[`bench/results/hash-quality-2026-09-12-arz-d82e296.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-09-12-arz-d82e296.txt).
The two hashes are deterministic and unseeded, so none of this is a statement about an adversary
who picks the queries — see `SECURITY.md`.</sub>

## Error bars, and where a lookup number comes from

Every timed number on this page is the minimum of a few alternated passes. A minimum is the right
estimator for a laptop — it is the sample least contaminated by whatever else the machine was doing
— but it carries no width, and a claim about a frontier is a claim about an *ordering*, which needs
one. `local/statbench` runs 30 independent passes a structure with the six lanes round-robin, so
drift lands on all of them equally, and reports the median with p5/p95 and a bootstrap interval
around the median. The interval is a percentile interval over 10 000 resamples rather than
mean ± t·s: the samples are not normal, since a pass that met a scheduler tick sits far above the
rest and nothing at all sits below the hardware.

479 823 English words, 200 000 probes a pass, half of them members, ns a lookup:

| structure | B/key | min | median | p5 | p95 | 95 % CI of the median |
|---|---:|---:|---:|---:|---:|---:|
| `ClosedHashIndex` | 0.26 | 24.1 | 32.0 | 24.7 | 39.0 | 31.3 – 33.3 |
| `CompactHashIndex` fp=1 | 1.26 | 31.3 | 44.1 | 36.4 | 56.8 | 42.1 – 45.5 |
| `PerfectHashIndex` | 10.90 | 151.8 | 157.2 | 152.0 | 166.9 | 154.3 – 162.3 |
| `DictIndex` 32 | 3.24 | 305.3 | 311.8 | 305.8 | 322.5 | 310.1 – 314.3 |
| `StringIndex` | 5.95 | 328.6 | 333.0 | 328.9 | 345.9 | 332.0 – 335.9 |
| `DictIndex` 128 | 2.83 | 428.0 | 433.1 | 429.0 | 439.5 | 432.0 – 434.2 |

**A slow structure is a quiet one.** Past 150 ns the p5–p95 band is 2.4–9.5 % of the median and the
minimum sits 1–4 % under it, so quoting the minimum costs nothing. Under 50 ns the band is 45 % and
the minimum is 25–29 % under the median — same probe set, same binary, thirty consecutive passes.

**The probe set is part of the working set.** Only the number of probes a pass changes here:

| probes a pass | `Closed` | `Compact` fp=1 | `Perfect` | `Dict` 32 | `String` | `Dict` 128 |
|---|---:|---:|---:|---:|---:|---:|
| 100 000 | 21.0 | 31.4 | 137.1 | 285.8 | 317.5 | 417.9 |
| 200 000 | 32.0 | 44.1 | 157.2 | 311.8 | 333.0 | 433.1 |
| 400 000 | 40.7 | 68.1 | 165.8 | 329.4 | 342.3 | 446.4 |

`ClosedHashIndex` doubles; `DictIndex` 128 moves 7 %. The probe array is a second structure the loop
walks: 100 000 `String` headers are 2.4 MB, 400 000 are 9.6 MB before the bytes they point at, and
past some point between the two the probe stops being in cache and starts being fetched at the same
cost as the lookup it pays for. The smaller the index, the larger the share of the total that is.
The counters say it in one line — **the instruction count does not move and the cycle count does**:

| structure | instr | cycles at 100 k | at 400 k | branch misses | L1 fills | LLC misses |
|---|---:|---:|---:|---:|---:|---:|
| `ClosedHashIndex` | 108 | 38.6 | 153.3 | 0.96 | 2.50 | 1.2–1.4 |
| `CompactHashIndex` fp=1 | 211 | 108.5 | 267.0 | 0.85 | 3.74 | 2.3–2.5 |
| `PerfectHashIndex` | 365 | 445.4 | 653.4 | 3.14 | 5.67 | 4.4–4.7 |
| `DictIndex` 32 | 1671 | 1178.2 | 1347.4 | 12.55 | 13.16 | 6.1–6.3 |
| `StringIndex` | 2237 | 1234.8 | 1382.5 | 13.26 | 15.17 | 7.7–8.1 |
| `DictIndex` 128 | 3226 | 1714.0 | 1791.1 | 18.63 | 15.86 | 6.6–7.0 |

Three things fall out of that table that no nanosecond showed:

- **`DictIndex` 128 is slower than `DictIndex` 32 in the ALU, not in the cache.** 3226 instructions
  against 1671 — a block scan is linear in the block, and 128 keys a block is twice the scanning of
  32 — while its LLC misses are the *same* 6–7. The 2.83 B/key that 128 buys is paid for in
  instructions, which is why the gap hardly moves with the probe set and why a bigger cache will not
  close it.
- **`PerfectHashIndex` is pure latency.** 365 instructions, 4.4 LLC misses, IPC 0.56–0.82: it does
  almost no work and waits for all of it.
- **`StringIndex` misses the most, 7.7–8.1 a lookup.** That is the same fact as its thread scaling
  above: dependent misses are what a transducer walk is made of, and they are also what another core
  can overlap.

A second mechanism arrives between 200 000 and 400 000 probes. Data-TLB misses a lookup go from
0.002 to 0.393 for `ClosedHashIndex` and from 0.008 to 0.544 for `DictIndex` 128: the probe array
has outgrown a 2048-entry L2 TLB, and every structure now pays for a page walk it did not pay for
before. `PerfectHashIndex` is the one that misses at every size (0.185 at 100 000), its 5.2 MB arena
being spread over more pages than the TLB holds to begin with.

**The Python table is worse than any of this, and it is worth saying where.** `bench/compare.py`
was run twice from clean trees of the same commit on this machine, hours and a reboot apart, with
the same word list and byte-identical probes. Every lane moved: 1.27× on the slow rows, 2.2× on the
fast ones, and the empty Python call that no library can influence went 49 → 86 ns. Both runs are
internally tight — five passes within 2 % of each other in each — and the Rust ladders above
reproduced across the same span to within a few per cent, so it is neither noise nor the machine
but something in one process's heap that the next one does not repeat. The ordering held: of
fourteen rows, only two adjacent pairs swapped, and both pairs were inside each other's spread
already. Read that table as a ranking with sizes attached, not as a stopwatch reading —
`bench/reproduce.sh` prints the call floor beside the published one for exactly this reason.

What survives all of it is the ordering. At all three probe counts the six bootstrap intervals are
disjoint and in the same order, which is what a frontier table claims and all it claims. The
absolute nanoseconds belong as much to the harness as to the structure: `bench/compare.py` draws
100 000 probes, so that is the column its published numbers sit in, and a lookup time quoted without
its probe set is half a number.

<sub>Measured 2026-09-12 on a clean tree
([`bench/results/stats-2026-09-12-arz-e979999.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/stats-2026-09-12-arz-e979999.txt),
which carries all thirty samples of every row), Ryzen 7 5800HS. **The two `DictIndex` rows are that
commit's**: the bytes are today's, but its lookups and instruction counts are from before a block
put its headers first and before the per-block offsets were packed, which took `id` down 9–15 % at
the larger blocks. What the section is about — the spread, and the probe set being part of the
working set — is the same either way; the current per-block-size numbers are the ladder above.
Counters are `perf stat -r 3` over
20 passes minus a build-only control, so the build's own cycles and faults stay out of the lookup's;
in those runs every structure is built but only one is probed, which is the *kinder* case — the
table above has six rotating through the cache.</sub>

## Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.26 bytes/key holds constant** as
`n` grows. Each row is measured twice: handing the constructor a **list** of keys, and handing it a
**generator**. The second is what `CompactHashIndex`'s streaming build exists for — it keeps a
16-byte pair per key and drops the string — and it is the only way to see the index's own footprint
rather than the corpus's:

| n | structure | keys | build | bytes/key | peak RSS | lookup |
|---|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | list | 0.33 s | 0.68\* | 155 MB | 188 ns |
| 1 M | `StringIndex` | generator | 0.48 s | 0.68\* | 147 MB | 187 ns |
| 1 M | `CompactHashIndex` | list | 0.09 s | 1.26 | 145 MB | 78 ns |
| 1 M | `CompactHashIndex` | **generator** | 0.20 s | 1.26 | **78 MB** | 79 ns |
| 10 M | `StringIndex` | list | 4.4 s | 2.00\* | 1108 MB | 703 ns |
| 10 M | `StringIndex` | generator | 6.0 s | 2.00\* | 1032 MB | 637 ns |
| 10 M | `CompactHashIndex` | list | 0.66 s | 1.26 | 952 MB | 242 ns |
| 10 M | `CompactHashIndex` | **generator** | 1.9 s | 1.26 | **264 MB** | 244 ns |

<sub>Measured on 2.0.0
([`bench/results/scale-2026-09-10-arz-16c7abe-dirty.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/scale-2026-09-10-arz-16c7abe-dirty.json)
— the tree of 16c7abe plus the release's own edits, none of them in a measured path), one process
per cell and the **minimum of five** per cell, on a machine idle throughout (load 0.4–1.0 at both
ends). `StringIndex`, whose build and lookup code has not changed since 0.5.1 and is therefore
the control, reads the 1.1.0 table back within its noise: 0.34 → 0.33 s and 0.47 → 0.48 at 1 M,
4.4 and 6.0 s at 10 M unchanged, lookups 195 → 188 and 655 → 703 ns. Against that flat control,
`CompactHashIndex`'s builds fell — 0.11 → 0.09 s and 0.22 → 0.20 at 1 M, **0.91 → 0.66 s** and
2.1 → 1.9 at 10 M — and the generator build's peak at 10 M fell 297 → 264 MB: the slots computed on
every thread, a fingerprint written as one store, and the spill in slabs, which are 2.0's build
changes. Its blob stays 1.26 B/key at both sizes, and its lookups hold (75 → 78 and 271 → 242 ns,
inside what a per-call Python loop resolves).</sub>

