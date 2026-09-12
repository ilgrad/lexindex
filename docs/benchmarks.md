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
| **lexindex `ClosedHashIndex`** | — | — | — | — | — | none (closed vocabulary) | — | **0.26** | 98 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | — | probabilistic | ✅ | **0.76** | 95 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | — | probabilistic | ✅ | **1.26** | **92** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | — | probabilistic | ✅ | **2.26** | 97 |
| **lexindex `DictIndex` (512 per block)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.82** | 348 |
| **lexindex `DictIndex` (256 per block, default)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.85** | 342 |
| `marisa-trie` (4 tries, tiny cache — its smallest) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 496 |
| `marisa-trie` (default) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 472 |
| `marisa-trie` (huge cache) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 461 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 323 |
| lexindex `PerfectHashIndex` | — | — | — | — | ✅ | ✅ | ✅ | 10.90 | 216 |
| DAWG (`dawg2`) | ✅ | ✅ | — | — | — | ✅ | — | 23.96 | 242 |
| `datrie` | ✅ | ✅ | — | — | — | ✅ | — | 30.91 | 592 |
| builtin `dict` | — | — | — | — | — | ✅ | — | — (in RAM only) | 255 |

<sub>Every cell above is one run at `386b2e6`, the tree where a block is cut into microblocks of 32
and the default block is 256
([`bench/results/compare-2026-09-12-arz-386b2e6.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-12-arz-386b2e6.json))
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. **Its Python call floor is 48 ns against the
49 of the table published through 2.1.0**, so the two columns are comparable; a run earlier the same
day measured a floor of 100 and every row fifty nanoseconds higher, two hours apart and within 2 ns
of each other, which is the reproducibility problem this page documents further down and the reason
`bench/reproduce.sh` prints the floor beside the published one. `marisa-trie` appears three times
because it is a curve: its own documentation says the right configuration depends on the data, so the table carries
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
that does nothing — is **48 ns** in this run, which every row pays and no *difference* between
rows contains.
And `.get` on a miss is each library's own miss path, not `try: t[key] except KeyError: default`:
timed apart, a miss costs *less* than a hit everywhere (0.85–0.91×), while that wrapper written out
by hand over marisa's `__getitem__` costs 1.61× a hit
([`bench/results/lookup-fairness-2026-09-12-arz-bf5c1b9.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/lookup-fairness-2026-09-12-arz-bf5c1b9.txt)).
Half the probes are misses, so a library that paid for exceptions would have been charged for CPython
rather than for its own structure.

**The smallest rows are also the fastest, which is not a paradox.** `ClosedHashIndex` and the three
`CompactHashIndex` widths answer in 92–98 ns against a builtin `dict`'s 255, because they store no
keys at all: one hash, one probe, at most a fingerprint to compare. What they cannot do is tell a
stranger from a member with certainty, or give a key back for an id. Among the structures that do
keep their keys, `DictIndex` is **both smaller and faster than every `marisa-trie` setting measured
at either block** — 2.82 B/key and 348 ns at 512, 2.85 and 342 at the default 256, against
2.96–3.07 and 461–496.

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
needs no automaton for it. On this corpus `DictIndex` at its default block is smaller than
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
| **lexindex `DictIndex`** (512 per block) | **2.819** | 0.95× | 272 ns | 26 ms |
| **lexindex `DictIndex`** (256 per block, default) | **2.852** | 0.96× | 253 ns | 26 ms |
| `marisa-trie` (C++ reference, default) | 2.978 | 1.00× | — | — |
| `rsmarisa` (4 tries, tiny cache — its smallest) | 3.003 | 1.01× | 322 ns | 137 ms |
| **lexindex `DictIndex`** (64 per block) | 3.045 | 1.02× | **227 ns** | **26 ms** |
| `rsmarisa` 0.4.2 (default) | 3.168 | 1.06× | 301 ns | 148 ms |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.63× | — | — |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | 5.95 | 2.00× | — | — |
| `yada` (double-array) | 15.98 | 5.4× | — | — |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× | — | — |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× | — | — |

<sub>`rsmarisa` and `DictIndex` from
[`bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt)
— one process, every key probed as a member and with a digit appended as a miss, shuffled with a
fixed seed, seven rounds alternating in both directions, minimum of the last five. The other rows
are the older sweep with `crawdad` 0.4, `yada` 0.5, `fst` 0.4; size is serialised bytes ÷ keys
throughout, and `rsmarisa`'s `io_size()` is asserted equal to the file it saves. None of them is a
lexindex dependency — the harness is a throwaway crate.</sub>

So the statement is a crown after all, with the frontier behind it. **`DictIndex` at its default
block dominates `rsmarisa` at its most compact setting outright** — 2.852 against 3.003 B/key, 253
against 322 ns, 5.3× faster to build — and at 64 per block it is the fastest structure here at 227
ns for 3.045, still under `rsmarisa`'s default; every `rsmarisa` setting has a `DictIndex` block
that is both smaller and faster. The block size picks the axis.

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
(`local/positioning.py`, 2026-09-12 at `386b2e6`,
[`positioning-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/positioning-2026-09-12-arz-386b2e6.txt)):

| bytes/key | 479 823 single words | 1 M `word.word` pairs, drawn at random | 1 M `word.word`, 1 000 × 1 000 grid |
|---|---:|---:|---:|
| the bare MPHF (no keys, no membership, no reverse) | 0.26 | 0.26 | 0.26 |
| **lexindex `CompactHashIndex`** (fp = 1 byte) | **1.26** | **1.26** | **1.26** |
| **lexindex `DictIndex`** (256 per block) | **2.85** | 7.58 | 2.47 |
| `marisa-trie` | 2.98 | 6.21 | 2.12 |
| **lexindex `StringIndex`** | 5.95 | 15.19 | 0.68 |
| lexindex `PerfectHashIndex` | 10.90 | 21.93 | 12.52 |

The `DictIndex` row swaps places with the `marisa-trie` one inside a single table — under it on
single words, 1.22× over it on random pairs and 1.17× on the grid — which is the whole argument of
this page in three columns: the two are different shapes, and which is smaller is a property of the
keys. At 10 M the trie numbers move again — `marisa` 4.14 on random pairs against 2.36 on the grid,
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
has vendored, which is a sample of Rust, not of source code. Both are *samples* of a filesystem that
keeps moving, this repository's own build directory included, so `build` keeps what is on disk when
it matches the manifest and only `build --force` draws a new one: two sweeps a week apart otherwise
compare different `paths` corpora and report the difference as a change in the index.

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

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 7.81 | 7.71 | 7.62 | 17.74 | 7.52 | 7.56 | 7.71 |
| `domains` | 13.8 | 0.26 | 1.26 | 5.10 | 5.03 | 5.00 | 10.50 | 4.81 | 4.87 | 4.99 |
| `idents` | 17.3 | 0.26 | 1.26 | 7.35 | 7.28 | 7.22 | 10.55 | 5.45 | 5.62 | 5.74 |
| `numeric` | 5.9 | 0.26 | 1.26 | 2.17 | 2.12 | 2.07 | 0.00 | 1.62 | 1.64 | 1.72 |
| `opaque` | 16.0 | 0.26 | 1.26 | 13.87 | 13.82 | 13.79 | 21.44 | 18.23 | 18.82 | 19.04 |
| `paths` | 125.0 | 0.26 | 1.26 | 16.35 | 15.89 | 16.13 | 17.48 | 9.26 | 9.47 | 9.60 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 9.63 | 9.58 | 9.43 | 17.28 | 7.79 | 8.19 | 8.37 |
| `titles-ru` | 35.8 | 0.26 | 1.26 | 12.29 | 12.06 | 12.07 | 31.75 | 8.07 | 8.61 | 8.77 |
| `titles-zh` | 16.9 | 0.26 | 1.26 | 8.81 | 8.79 | 8.75 | 17.52 | 6.33 | 6.54 | 6.70 |
| `urls` | 52.4 | 0.26 | 1.26 | 11.62 | 11.41 | 11.34 | 16.88 | 7.95 | 8.39 | 8.58 |
| `uuid` | 36.0 | 0.26 | 1.26 | 20.57 | 20.46 | 20.33 | 37.11 | 32.93 | 34.58 | 34.80 |

<sub>`words` and `pypi` have no million-key file; their 100 000 grid, every build time, and the
100 000 rows for the rest are in
[`bench/results/sweep-2026-09-12-arz-386b2e6-dirty.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-12-arz-386b2e6-dirty.json)
(the tree is the commit; what was uncommitted is the pages being re-measured and the result files
this run wrote).</sub>

**Where `marisa-trie` wins, it wins on shared structure.** It is the smallest key-storing structure
on nine of these eleven corpora, and the margin tracks how much the keys have in common: 1.72× on
`paths`, where a million paths run through a few thousand directories, 1.49× on `titles-ru`, 1.43×
on `urls`, 1.33× on `idents` — and within 4 % on `dna` and `domains` (1.01× and 1.04×). That is
what a LOUDS trie is for, and front coding in fixed blocks does not answer it.

**Where lexindex wins, it wins on entropy.** `DictIndex` is the smaller of the two on `opaque`
(13.79 against 18.23, 1.32×) and on `uuid` (20.33 against 32.93, 1.62×) — keys with nothing to
share, where a trie pays for a node per character and front coding pays for a prefix that is not
there. On `words`, the corpus every table above is measured on, `DictIndex` at its default block is
3.48 against marisa's 3.70 at 100 000 keys, and 2.85 against 2.96 on the full 479 823. That is a
real result on a real corpus, and it is not the general case: it is the favourable end of a
distribution whose other end is `paths`.

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

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 108 | 150 | 424 | 410 | 399 | 702 | 1090 | 1091 | 1049 |
| `domains` | 13.8 | 120 | 156 | 430 | 424 | 421 | 485 | 796 | 799 | 756 |
| `idents` | 17.3 | 91 | 148 | 467 | 487 | 531 | 509 | 999 | 924 | 961 |
| `numeric` | 5.9 | 110 | 143 | 314 | 379 | 366 | 223 | 354 | 388 | 341 |
| `opaque` | 16.0 | 144 | 177 | 510 | 542 | 558 | 558 | 2232 | 1617 | 1592 |
| `paths` | 125.0 | 209 | 204 | 978 | 966 | 1025 | 1641 | 3791 | 3080 | 2781 |
| `titles-en` | 21.0 | 195 | 165 | 632 | 633 | 665 | 831 | 1578 | 1471 | 1440 |
| `titles-ru` | 35.8 | 239 | 281 | 749 | 764 | 796 | 1147 | 2127 | 1973 | 1916 |
| `titles-zh` | 16.9 | 214 | 256 | 661 | 678 | 721 | 794 | 1471 | 1412 | 1346 |
| `urls` | 52.4 | 101 | 150 | 694 | 668 | 689 | 800 | 1335 | 1279 | 1220 |
| `uuid` | 36.0 | 156 | 181 | 683 | 653 | 709 | 809 | 2544 | 1859 | 1776 |

**`DictIndex` answers faster than every `marisa-trie` setting on ten of the eleven corpora** —
1.8–2.9× at its default block, against marisa's *fastest* configuration and not its smallest. The
eleventh is `numeric`, where marisa's fastest is 10 % ahead of the default block (341 ns against
379) and 9 % behind a block of 128 (314): a million dense decimal ids are the corpus where a trie
has the least to walk. `DictIndex` also builds 2.3–4.3× faster everywhere except `numeric`, where
the two are level. So the size table above is not the whole trade: on `paths`, marisa is 1.72×
smaller and 2.9× slower to answer and 4.3× slower to build.

**The block is a smaller knob than it was.** A lookup scans one restart a microblock and then one
microblock whatever the block, so 128 → 1024 keys a block moves a lookup by −6 % (`dna`) to +16 %
(`numeric`), where the one-level format paid 3.8× from 32 to 1024, and buys 0.06–0.28 bytes a key
over 128 — but on `paths` 1024 stores *more* than 256 (16.13 against 15.89). That is not the
layout, whose per-block arrays only shrink as the block grows; the suspect is the symbol table,
which trains on whole blocks spread over the index and so on a quarter as many neighbourhoods at
1024, and `paths` is the corpus whose neighbourhoods differ most. It is not yet measured, and the
cell stays as it read. 256 is the default the README quotes.

<sub>Lookups are one run of three rounds of 20 000 probes a cell, so a column is comparable within
itself and a cell carries a few per cent: a second run of the same build read the same bytes to the
last digit and lookups 1–15 % higher on every structure, controls included.</sub>

### Ten million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa small | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 7.13 | 7.03 | 6.94 | 15.98 | 6.69 | 6.75 | 6.99 |
| `numeric` | 6.9 | 0.26 | 1.26 | 2.18 | 2.12 | 2.07 | 0.00 | 1.63 | 1.66 | 1.77 |
| `opaque` | 16.0 | 0.26 | 1.26 | 13.44 | 13.39 | 13.35 | 21.47 | 15.19 | 18.33 | 18.68 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 7.95 | 7.92 | 7.93 | 13.25 | 5.57 | 5.71 | 5.87 |
| `urls` | 52.4 | 0.26 | 1.26 | 9.89 | 9.58 | 9.52 | 13.10 | 5.66 | 5.83 | 5.99 |
| `uuid` | 36.0 | 0.26 | 1.26 | 20.08 | 19.97 | 19.86 | 36.07 | 29.97 | 33.21 | 33.56 |

Ten times the keys moves every trie and neither hash: at its smallest setting `marisa` goes
7.79 → 5.57 on `titles-en` and 7.95 → 5.66 on `urls` as the sharing deepens, `DictIndex` at its
default block 9.58 → 7.92 and 11.41 → 9.58, and the two keyless rows do not move at all. The
ranking is the same one the million-key table gives, so the answer to "which is smallest" is
decided by the corpus and not by the scale.

**The block buys almost nothing here.** 128 → 1024 keys a block is 0.02–0.37 bytes a key over these
six, against 0.06–0.28 at a million, and on `titles-en` 1024 is level with 256 (7.93 against 7.92).
Ten times the keys is ten times the blocks, so the per-block arrays a bigger block saves were
already a smaller share of the index; what is left is the coded suffixes, and those are the symbol
table's business, not the block's.

<sub>Measured 2026-09-12
([`bench/results/sweep10m-2026-09-12-arz-386b2e6-dirty.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-09-12-arz-386b2e6-dirty.json)),
`marisa-trie` 1.4.1. Two builds per cell, three rounds of 20 000 lookups.</sub>

### A cold mapping, and what is actually resident

Every size in this page is the blob. A process that maps one and answers a thousand queries never
reads most of it, and what decides whether an index fits inside a container limit is the resident
set. `local/coldmmap` drops each file's page cache with `posix_fadvise(POSIX_FADV_DONTNEED)` — no
root needed, and the resident set straight after `load_mmap` is the control that the drop worked —
then reads `/proc/self/smaps` after 1 000 random lookups and after a further million.

Ten million English Wikipedia titles, bytes per key except the latencies:

| structure | file | mapped | after 1 k | after 1 M | cold ns | warm ns |
|---|---:|---:|---:|---:|---:|---:|
| `DictIndex` 32 | 8.44 | 0.26 | 5.28 | 8.44 | 42 636 | 530 |
| `DictIndex` 32, `MADV_RANDOM` | 8.44 | 0.26 | **1.82** | 8.44 | 97 074 | 1 788 |
| `DictIndex` 256 | 7.92 | 0.05 | 4.62 | 7.92 | 46 285 | 553 |
| `DictIndex` 256, `MADV_RANDOM` | 7.92 | 0.05 | **0.86** | 7.92 | 111 287 | 1 958 |
| `DictIndex` 1024 | 7.93 | 0.02 | 4.66 | 7.93 | 43 995 | 538 |
| `CompactHashIndex` | 1.26 | 0.26 | 1.25 | 1.26 | **6 294** | **47** |
| `CompactHashIndex`, `MADV_RANDOM` | 1.26 | 0.26 | 0.70 | 1.26 | 55 040 | 159 |
| `StringIndex` | 13.25 | 0.01 | 10.10 | 13.25 | 87 117 | 849 |
| `StringIndex`, `MADV_RANDOM` | 13.25 | 0.01 | 2.27 | 13.25 | 368 457 | 3 068 |

**`load_mmap` really is lazy**, which the `mapped` column exists to prove: 0.01 to 0.26 bytes a key
resident before the first query, which is the header and the little the loader validates. Nothing
else is read until something asks for it.

**The first thousand queries cost far more pages than they need.** `DictIndex` at 256 ends them with
4.62 of its 7.92 bytes a key resident — 58 % of an index nobody has finished reading — while the
same thousand queries under `MADV_RANDOM` leave **0.86**, which is what they actually touch: about
two pages a lookup, the sample array and the block. The 5.4× between those two numbers is the
kernel's readahead, and it is buying latency with memory: turning it off costs 2.4× on the cold
lookups and **3.5× on the warm ones**, because the advice outlives the warm-up. The warm column is
also where the microblock shows on ten million keys: 1024 a block answers in 538 ns where the
one-level format took 2 108. Readahead is the
right default here; `MADV_RANDOM` is for the case where a container limit, and not a latency budget,
is what binds.

**Cold start is where the smallest structure wins outright, and the mechanism is pages.**
`CompactHashIndex` answers its first thousand queries at **6.3 µs** against `DictIndex`'s 46.3 and
`StringIndex`'s 87.1 — 7× and 14× — because its whole file is 12.6 MB and a fault brings in a
useful fraction of it. On `uuid`, where its 1.26 bytes a key sit against `DictIndex`'s 20.06 and
`StringIndex`'s 36.07, the gap is 21× and 39× (5.9 µs against 125.3 and 231.0). A structure that
stores no keys has no keys to fault in.

**After a million queries every structure is fully resident**, to the last hundredth of a byte. The
distinctive answer to "how much memory does this index need" only exists during warm-up: past it,
against a workload that touches every key, the resident set *is* the file, and the size table above
is the steady-state RSS.

<sub>Measured 2026-09-12 on a clean tree, NVMe under LUKS on btrfs, 38 GB RAM — so "cold" means
this file's page cache was dropped and not that the machine was short of memory
([`bench/results/coldmmap-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-09-12-arz-386b2e6.txt);
the million-key run and the `uuid` one are in the earlier
[`coldmmap-2026-09-12-arz-bb1473c.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-09-12-arz-bb1473c.txt),
on the one-level format). `MADV_RANDOM` is applied by the harness
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
| 1 | 589 | 35.3 | 855 |
| 2 | 299 (1.97×) | 19.0 (1.86×) | 419 (2.04×) |
| 4 | 149 (3.96×) | 9.7 (3.65×) | 206 (4.14×) |
| 8 | 76 (7.75×) | 5.2 (6.74×) | 106 (8.10×) |
| 16 | 48 (12.18×) | 3.7 (9.43×) | 63 (13.50×) |
| **M lookups/s at 16** | **20.7** | **267.5** | **15.8** |

**Eight cores give 6.7–8.1×**, 84–101 % of them, on a mobile part whose clock falls as cores light
up — the number already carries that, so a machine with a flatter boost curve can only do better.

**The sixteen-thread row is SMT and it is worth having.** 9.4–13.5× over one thread, well past the
eight physical cores, because a point lookup is a chain of dependent loads and a core spends most of
it waiting. A second thread on the same core fills those stalls with someone else's work.

**`StringIndex` scales best because it stalls most** — 2.04× on two threads and 13.50× on sixteen.
One core can only keep a handful of cache misses in flight; two cores have twice the
memory-level parallelism, and a transducer walk is nothing but dependent misses. At a million keys,
where a 17 MB transducer is nearly cache-resident, the same effect is weaker (1.91× and 10.59×).

**`CompactHashIndex` saturates first**, at 9.43×, and it is the one structure that has run out of
something other than cores: 268 M lookups a second over a 12.6 MB index that fits this machine's
16 MB L3, at 3.7 ns a lookup. The others are answering out of DRAM and have latency left to overlap;
this one does not.

<sub>Measured 2026-09-12 at `386b2e6`, the tree differing from it in documentation only
([`bench/results/readscale-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/readscale-2026-09-12-arz-386b2e6.txt),
which carries the million-key ladder as well), Ryzen 7 5800HS, 8 cores and 16 hardware threads.
Shared by `&index` across `std::thread::scope`; no index is cloned and none is rebuilt per
thread.</sub>

### `rsmarisa`, the pure-Rust port

The same question in Rust, where `marisa-trie` is a C++ library with no binding a Rust project would
take. `rsmarisa` 0.4.2 is a pure-Rust port of it, BSD-2-Clause, and `local/rsmarisacmp` puts its
three cache levels against three `DictIndex` block sizes, 64, 256 and 512, on every corpus:

| corpus | rsmarisa smallest | `DictIndex` 512 | rsmarisa build | `DictIndex` build | rsmarisa fastest | `DictIndex` fastest (block) |
|---|---:|---:|---:|---:|---:|---:|
| `dna` | 7.61 | 7.65 | 1 308 ms | 64 ms | 608 ns | 238 ns (512) |
| `domains` | 4.87 | 5.00 | 680 ms | 57 ms | 466 ns | 267 ns (64) |
| `idents` | 5.50 | 7.31 | 652 ms | 61 ms | 609 ns | 342 ns (256) |
| `numeric` | 1.64 | 2.09 | 98 ms | 46 ms | 161 ns | 190 ns (64) |
| `opaque` | 18.27 | 13.80 | 1 177 ms | 82 ms | 1 047 ns | 299 ns (512) |
| `paths` | 10.09 | 15.89 | 5 642 ms | 95 ms | 1 710 ns | 664 ns (512) |
| `titles-en` | 7.85 | 9.52 | 975 ms | 68 ms | 827 ns | 348 ns (256) |
| `titles-ru` | 8.22 | 12.08 | 1 489 ms | 68 ms | 1 102 ns | 447 ns (256) |
| `titles-zh` | 6.40 | 8.67 | 759 ms | 71 ms | 683 ns | 344 ns (256) |
| `urls` | 8.38 | 11.33 | 2 011 ms | 73 ms | 855 ns | 542 ns (512) |
| `uuid` | 33.01 | 20.39 | 1 508 ms | 115 ms | 1 155 ns | 391 ns (512) |

The shape is the one the C++ library gives: `rsmarisa` is smaller on the corpora whose keys share
structure and larger on the ones whose keys do not, while `DictIndex` builds **11–59× faster** and
answers **1.6–3.5× faster** on ten of the eleven. `numeric` is the exception on every axis:
`rsmarisa` is smaller there, quicker to answer, and only 2.1× slower to build. One thing the
right-hand column shows that the word list cannot: on the corpora whose keys are long — `dna`,
`opaque`, `paths`, `urls`, `uuid` — the *largest* block is the fastest, because there the
eight-byte samples stop settling the binary search over the block heads and every step of it reads
a head, so fewer blocks are fewer dependent misses, and that outweighs a scan ten entries longer. At
nominally the same configuration `rsmarisa` is larger than the C++ marisa measured above — on
`words`, 0.2 % at the smallest setting, 3.1 % at the default and 12 % at the fastest — so the flag
words evidently do not mean quite the same thing, and each library's own curve is what to read.

<sub>Measured 2026-09-12 on a clean tree
([`bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt)),
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
Measured 2026-09-12 at `386b2e6` (the better of two runs of the example back to back, load 0.6–0.8;
each lookup cell is the minimum of five timed passes after a warm-up pass;
[`bench/results/latency-rs-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-09-12-arz-386b2e6.txt)).
Absolute numbers are machine-dependent — the `std::HashMap` control reads 295 ns here against the
289 of the 2.0.0 table and the 245 of the 1.1.0 one — so compare the **ratios**, only within a
column, and read a shift under ~15 % between tables as the session: against this `HashMap`,
`CompactHashIndex::id` is 0.45×, `id_unchecked` 0.24×, `PerfectHashIndex::id` 1.02×, `StringIndex`
1.32×, `DictIndex` 1.83×, `BTreeMap` 2.63×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~50 ms** | ~133 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~287 ms | **~72 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~222 ms | ~295 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~283 ms | ~301 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~243 ms | ~390 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (256 per block) | ~150 ms | ~539 ns | ordered, exact reverse; 2.47 B/key here against the FST's 0.68 — a `word.word` cross product is what a transducer factors out, and what a block of front-coded keys does not (on the dictionary: 2.85 against 5.95, 298–302 ns against 265–272) |
| `std::BTreeMap<String, u32>` | ~220 ms | ~774 ns | in-RAM |

<sub>**The 2.0.0 session was the outlier, not this one.** The two rows whose code has not moved since
0.5.1 come back to where the 1.1.0 table had them: `StringIndex` 1.30× → 1.47× → **1.32×** of
`HashMap` and `BTreeMap` 3.19× → 3.34× → **2.63×**, the FxHash map steady at 0.586× → 0.60× →
0.58×. Both of the rows that moved are the memory-bound ones, and they moved together and in the
same direction as each other in both sessions — which is the signature of the machine, not of a
release. The rows 2.0's key hash reaches stayed put across all three: `PerfectHashIndex::id` 0.95×
→ 1.04× → 1.02×, `id_unchecked` 0.269× → 0.26× → 0.24×, `CompactHashIndex::id` 0.435× → 0.45× →
0.45×. `DictIndex` is the one row that moved for a reason other than the session: at the default of
256 keys a block it reads **539 ns for 2.47 B/key** where 2.0.0's 32 a block read 507 for 3.19 —
the packed offsets and the microblocks together, 23 % of the bytes for 6 % of the lookup.
`CompactHashIndex` builds in 50 ms against 69 on 1.1.0, the one build that outran the session. This corpus is its worst case by
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
**~72 ns** — so on a closed vocabulary the perfect hash is about **2.4× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash. `CompactHashIndex::id` (~133 ns) is
23 % *faster* than the FxHash map and still carries the membership check and the 1.26 B/key
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
**fastest of the structures in the table above** — 4.1× as quick as the SipHash `HashMap` and 2.4×
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

`local/dictbench` builds the dictionary as a `StringIndex` and as a `DictIndex` at six block sizes,
then probes all 479 823 words in a shuffled order — members, strangers (each word with a byte
appended), and every id for the reverse lookup — five rounds in one process, the variants alternated
within each round, the minimum per cell; `StringIndex` is the control.

| | `StringIndex` | `DictIndex` 32 | `DictIndex` 64 | `DictIndex` 128 | `DictIndex` **256** | `DictIndex` 512 | `DictIndex` 1024 |
|---|---:|---:|---:|---:|---:|---:|---:|
| bytes per key | 5.95 | 3.25 | 3.05 | 2.92 | **2.85** | 2.82 | 2.80 |
| of which per-block arrays | — | 0.348 | 0.231 | 0.144 | 0.100 | 0.077 | 0.066 |
| microblocks a block | — | 1 | 2 | 4 | 8 | 16 | 32 |
| entries a lookup scans | — | 31 | 32 | 34 | **38** | 46 | 62 |
| build | 105–106 ms | 17–22 | 16–20 | 16–17 | 16 | 16–18 | 17–23 |
| `id`, member | 265–283 ns | 252–254 | 272–284 | 285–288 | **298–302** | 316–322 | 344–347 |
| `id`, stranger | 215–219 ns | 204 | 223–224 | 234–238 | 255–257 | 267–275 | 292–302 |
| `key_into` (no allocation) | — | 154–155 | 169–172 | 184–188 | **207** | 227–232 | 263–272 |
| `key` (owned string) | 456–474 ns | 205–212 | 232–234 | 249–258 | 278 | 293–307 | 340–348 |
| `lower_bound`, stranger | — | 203–206 | 222–224 | 234–239 | 255–257 | 264–274 | 290–303 |
| `prefix`, 2 000 three-byte prefixes of ~760 keys | — | 29.1–29.6 µs | 28.7–30.1 | 28.6–30.2 | 30.2–30.3 | 29.1–30.2 | 29.1–30.3 |

Two runs of the same ladder in opposite orders; where they differ the table gives both. At the
default 256, `DictIndex` answers `id` within 10 % of `StringIndex` and `key_into` at less than half
its `key` while storing **52 % less**; at 64 per block it is level with `StringIndex` on `id` at
3.05 B/key, and at 32 it is 6 % ahead. The sequential lane does not move with the block at all.

**A lookup scans a microblock, not a block.** A block is cut into microblocks of 32, and the first
key of each is a restart front-coded against the restart before it, so a lookup walks the restarts
to one microblock and scans that: `block / micro + micro − 2` entries, which is 38 at the default
where the block holds 255. That is what unties the size from the latency. Against the
one-level format at the same size the reverse lookup halves and `id` does not move: **244–246 ns
and 392–399 for 2.819 B/key, where one level at 128 keys a block stored 2.827 for 460–464 and
393**. The one-level format reaches 0.10 B/key further down, and that is what it costs: its floor
of 2.701 at 1024 keys a block answers `key_into` in 3 190 ns and `id` in 984, against 287–289 and
420–426 for 2.804 here. Both ladders ran alternated by process in one session, the base built from
the commit before microblocks in a worktree, with `StringIndex` at 340–353 ns as the control
([`dict-onelevel-ab-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-onelevel-ab-2026-09-12-arz-386b2e6.txt)).
That session sits about 25 % above the table at the top of this section — its control says so, at
340–353 ns where the table's reads 265–283 — so read each comparison inside its own session and
neither against the other.

**The reverse lookup does not decode the whole run.** An entry stores what it shares with its
predecessor, so an entry whose shared-prefix length is at least a later entry's contributes nothing
that survives to the key being asked for. The ones that do contribute form a strictly increasing
staircase of that length, and a monotonic stack over the run's headers finds it in the pass the
walk already makes: every header is still read — a header is what says where the next one begins —
but only the staircase is decoded. Measured over the dictionary and a path list when it landed, the
staircase is 2.5 entries deep on average and 12 at the deepest with a block of 32, 18 with a block
of 1024; past 32 the walk stops tracking it and decodes each entry, which is slower and not wrong.
That was worth **−16 % on `key_into` at 32 keys a block, −33 % at 128 and −37 % at 256**
([`dict-stair-2026-09-11-arz.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-stair-2026-09-11-arz.txt)
holds the before and after), and it costs no bytes and no format change.

**256 is a middle of the curve, not a limit**, and
[`build_with_block`](https://docs.rs/lexindex/latest/lexindex/struct.DictIndex.html#method.build_with_block)
is the knob. The size falls because a larger block spreads one stored head, one sample and two
offsets over more keys — the arrays go from 0.348 bytes a key at 32 to 0.066 at 1024 — while the
front-coded data moves the other way and less, 2.60 to 2.73, a restart every 32 keys being coded
against a key that far away. What the block costs is now `block / 32 + 30` entries scanned rather
than `block − 1`: 1024 scans 62 where 32 scans 31. At 512 the index is **2.82 bytes
per key, well under the 2.98 marisa stores on this corpus** — and unlike marisa it answers `key(id)`
and `lower_bound` at all, since a marisa id is not the lexicographic rank (7 051 of 19 999
consecutive sorted pairs come back with a decreasing id). Pick 32 or 64 if the lookups are hot,
512 or 1024 if the bytes are.

<sub>Measured 2026-09-12 at `386b2e6`
([`bench/results/dict-micro32-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-micro32-2026-09-12-arz-386b2e6.txt)),
two runs of the ladder in opposite orders, the table giving both where they differ; the per-block
arrays from `local/dictsize` on the same tree.</sub>

## Prefix queries against the tries

A prefix is a range of a sorted dictionary, so `DictIndex` answers one without an automaton:
`prefix_id_range` is two `lower_bound`s and `prefix` walks the run they name. `local/prefixbench.py`
puts that against the tries a Python project can install, on the same words — 20 000 three-byte
prefixes drawn from random words, so the average prefix carries 763 keys.

| | bytes/key | `prefix_count` | first 10 | every match + ids | keys only |
|---|---:|---:|---:|---:|---:|
| **lexindex `DictIndex` 256** (default) | **2.85** | **375 ns** | 2 015 ns | 102 081 ns | 72 541 ns |
| **lexindex `DictIndex` 512** | **2.82** | 396 | 2 159 | 102 532 | 73 384 |
| lexindex `StringIndex` | 5.95 | 556 | 4 465 | 162 897 | 330 497 |
| `marisa-trie` | 2.98 | 123 444 | 2 571 | 94 218 | 94 065 |
| `dawg2` | 23.96 | 68 251 | **1 449** | **48 544** | **48 503** |
| `datrie` | 30.69 | 731 859 | 729 729 | 724 271 | 725 551 |

**Counting is where the structures differ in kind rather than by a constant.** `prefix_count` costs
two order lookups whatever the prefix carries, so it runs 312× faster than marisa's at 512 per block
and 329× at the default — marisa has to enumerate all 763 matches to count them, its ids not being
lexicographic ranks, so there is no arithmetic to do instead. At every block that goes under marisa
on *size* — which since microblocks is the default too — `DictIndex` is also ahead on autocomplete
(2 015–2 159 ns against 2 571) and on handing back a prefix's keys (72 541–73 384 against 94 065) —
with a rank for each, which marisa has none to give.

**Those keys come fastest through the id range, which inverts what this page said until 2026-09-12.**
`prefix_id_range` and then `keys_of` costs 72 541 ns at the default block, against 102 081 for `prefix`,
which decodes an id for every match as well. `keys_of` used to re-enter the block for every id and
was the *slower* of the two by 2.1×; it now keeps one walk open across the ids that ascend through a
block, which is exactly the shape an id range hands it. The inversion is `DictIndex`-only:
`StringIndex` has no block to stay inside, its `keys_of` walks the transducer once per id, and
`prefix` is still the API for this query there (162 897 against 330 497). It is also why there is no
`prefix_keys` method: it would be these two calls, and the table says what a third one would buy —
27 %, which two existing calls already have.

**The last two columns are this benchmark's error bar as well as a result.** A trie has no ids to
return, so for `marisa-trie`, `dawg2` and `datrie` they are one call measured twice — and within a
run they come out 0.3 %, 0.4 % and 3.3 % apart. Nothing narrower than that is a finding here:
`DictIndex` at 256 against 512 on full enumeration (102 081 against 102 532) is one such
non-difference, while the 1.30× over marisa on keys alone is well outside it.

`dawg2` enumerates about 1.4× faster at 8.3× the bytes, with no reverse lookup and no mmap: a
different point on the curve, not a smaller one. `datrie` is a double-array built for point lookups;
prefix walking is not what it is for.

<sub>Measured 2026-09-12, the minimum over three clean-tree runs
([`bench/results/prefix-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/prefix-2026-09-12-arz-386b2e6.txt)),
`marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3 over the same word list, Ryzen 7 5800HS,
load 0.5–1.0 at the three starts. None
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
| `datrie` | 30.69 | **549 ns** | **247 ns** |
| **lexindex `StringIndex`** | 5.95 | 666 | 367 |
| `dawg2` | 23.96 | 695 | 617 |
| `marisa-trie` | 2.98 | 1 138 | 877 |
| **lexindex `DictIndex` 256** (default) | **2.85** | 2 502 | 984 |
| **lexindex `DictIndex` 512** | **2.82** | 2 642 | 1 042 |

**This is the query a transducer is shaped for, and the numbers say so.** The query *is* the path:
one walk down the FST, every final state on it a match, `O(query bytes)` whatever the index holds.
`StringIndex` is second only to `datrie` on both columns — at **a fifth of `datrie`'s bytes and a
quarter of `dawg2`'s**, ahead of `dawg2` on both and of `marisa-trie` by 1.7× and 2.4× at twice
marisa's size. It is the *largest* of the ordered indexes in
this crate, and on this one query that is where the bytes went.

**`DictIndex` has no walk to make and the table shows what that costs.** One order lookup per
character boundary, so a ten-character query is ten binary searches where the trie made one
descent: 2.2× marisa's time at the default block, 2.3× at 512. `longest_prefix` is the exception —
it starts at the query and stops at the first hit, so it never pays for the boundaries under the
match, and both blocks land within 12–19 % of marisa (984 and 1 042 ns against 877) while storing
fewer bytes than it. A caller who
asks this question often should hold a `StringIndex`; one who asks it occasionally, alongside the
ranks and ranges only `DictIndex` gives, can have it for a binary search per character.

<sub>Measured 2026-09-12
([`bench/results/common-prefix-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-09-12-arz-386b2e6.txt)),
the minimum of three runs that agree within 6 %, each the minimum of five alternated rounds,
`marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3, Ryzen 7 5800HS, load 0.5–0.6 at the three
starts. Nanoseconds per query through Python; the call overhead
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
| `ClosedHashIndex` | 0.26 | 14.8 | 15.2 | 14.8 | 17.2 | 15.1 – 15.6 |
| `CompactHashIndex` fp=1 | 1.26 | 27.0 | 27.2 | 27.0 | 28.9 | 27.1 – 27.3 |
| `PerfectHashIndex` | 10.90 | 88.4 | 90.4 | 88.6 | 102.0 | 89.8 – 92.0 |
| `StringIndex` | 5.95 | 232.4 | 234.0 | 232.5 | 249.4 | 233.5 – 235.5 |
| `DictIndex` 256 | 2.85 | 263.8 | 265.6 | 264.1 | 276.7 | 265.4 – 265.9 |
| `DictIndex` 512 | 2.82 | 283.9 | 286.0 | 284.1 | 296.0 | 285.7 – 286.6 |

**The minimum is within 3 % of the median everywhere**, and the p5–p95 band is 4–16 % of it, so
quoting the minimum costs nothing — same probe set, same binary, thirty consecutive passes. An
earlier session of the same harness put the band under 50 ns at 45 % and the minimum 25–29 % under
the median while the slow lanes kept their 2–10 %: a fast lane's width is the machine's that day,
not the structure's, and the interval column is what says which day it was.

**The probe set is part of the working set.** Only the number of probes a pass changes here:

| probes a pass | `Closed` | `Compact` fp=1 | `Perfect` | `String` | `Dict` 256 | `Dict` 512 |
|---|---:|---:|---:|---:|---:|---:|
| 100 000 | 13.3 | 28.3 | 88.0 | 230.9 | 265.5 | 284.1 |
| 200 000 | 15.2 | 27.2 | 90.4 | 234.0 | 265.6 | 286.0 |
| 400 000 | 25.5 | 36.1 | 114.3 | 260.7 | 285.4 | 305.1 |

`ClosedHashIndex` nearly doubles; `DictIndex` 256 moves 7 %. The probe array is a second structure the loop
walks: 100 000 `String` headers are 2.4 MB, 400 000 are 9.6 MB before the bytes they point at, and
past some point between the two the probe stops being in cache and starts being fetched at the same
cost as the lookup it pays for. The smaller the index, the larger the share of the total that is.
The counters say it in one line — **the instruction count does not move and the cycle count does**:

| structure | instr | cycles at 100 k | at 400 k | branch misses | L1 fills | LLC misses |
|---|---:|---:|---:|---:|---:|---:|
| `ClosedHashIndex` | 108 | 55.6 | 86.6 | 0.90 | 2.46 | 1.15–1.30 |
| `CompactHashIndex` fp=1 | 212 | 109.1 | 155.3 | 0.90 | 3.68 | 2.17–2.36 |
| `PerfectHashIndex` | 364 | 292.0 | 460.2 | 3.14 | 5.56 | 4.41–4.64 |
| `StringIndex` | 2236 | 960.9 | 1132.5 | 13.18 | 14.81 | 7.45–7.95 |
| `DictIndex` 256 | 2708 | 1118.1 | 1232.8 | 17.61 | 12.04 | 4.16–4.51 |
| `DictIndex` 512 | 2973 | 1191.5 | 1310.4 | 19.46 | 10.49 | 3.85–4.21 |

Three things fall out of that table that no nanosecond showed:

- **`DictIndex` does the most work and misses the least of the structures that keep their keys,
  and neither is what it waits for.** 2 708 instructions at 256 against `StringIndex`'s 2 236, and
  4.2 LLC misses against 7.5. The one-level format at 128 keys a block spent 3 226 instructions and
  missed 6.6 a lookup; the microblock cut both by a fifth and a third, and at equal size the
  latency did not move — because what a lookup waits for is a chain of dependent misses, the sample
  search then the restart run then the microblock, which is the same length whatever the scan, and
  the instructions and the streamed misses of the scan overlap under it. IPC 2.2–2.5 says the core
  is busy; the chain says with what. It is also why a block of 32 keys and one of 512 sit 12 % apart
  on `id` and not 16× apart on the entries they scan.
- **`PerfectHashIndex` is pure latency.** 364 instructions, 4.4 LLC misses, IPC 0.8–1.2: it does
  almost no work and waits for all of it.
- **`StringIndex` misses the most, 7.5–8.0 a lookup.** That is the same fact as its thread scaling
  above: dependent misses are what a transducer walk is made of, and they are also what another core
  can overlap.

A second mechanism arrives between 200 000 and 400 000 probes. Data-TLB misses a lookup go from
0.001 to 0.393 for `ClosedHashIndex` and from 0.004 to 0.523 for `DictIndex` 256: the probe array
has outgrown a 2048-entry L2 TLB, and every structure now pays for a page walk it did not pay for
before. `PerfectHashIndex` is the one that misses at every size (0.195 at 100 000), its 5.2 MB arena
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
([`bench/results/stats-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/stats-2026-09-12-arz-386b2e6.txt),
which carries all thirty samples of every row), Ryzen 7 5800HS. Counters are `perf stat -r 3` over
100 passes minus a build-only control, so the build's own cycles and faults stay out of the lookup's
— at 20 passes a 30 ns lane's difference sat inside the build's own run-to-run noise;
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

