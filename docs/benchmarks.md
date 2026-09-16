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

<!-- table: compare bench/results/compare-2026-09-13-arz-8aaa0af.json columns=prefix,common_prefix,range,fuzzy,reverse,exact,mmap -->
| library | prefix | common prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** | **ns/lookup** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | — | none (closed vocabulary) | — | **0.26** | 97 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | — | probabilistic | ✅ | **0.76** | 94 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | — | probabilistic | ✅ | **1.26** | **89** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | — | probabilistic | ✅ | **2.26** | 95 |
| **lexindex `DictIndex` (512 per block)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.81** | 346 |
| **lexindex `DictIndex` (256 per block, default)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.84** | 334 |
| `marisa-trie` (4 tries, tiny cache — its smallest here) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 476 |
| `marisa-trie` (default) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 452 |
| `marisa-trie` (huge cache) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 428 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 306 |
| lexindex `PerfectHashIndex` | — | — | — | — | ✅ | ✅ | ✅ | 10.90 | 206 |
| DAWG (`dawg2`) | ✅ | ✅ | — | — | — | ✅ | — | 23.96 | 231 |
| `datrie` | ✅ | ✅ | — | — | — | ✅ | — | 30.91 | 581 |
| builtin `dict` | — | — | — | — | — | ✅ | — | — (in RAM only) | 240 |
<!-- /table -->

<sub>Every cell above is one run of the v3.0.0 tag, `8aaa0af`, in a clean worktree on a machine
rebooted minutes earlier
([`bench/results/compare-2026-09-13-arz-8aaa0af.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-13-arz-8aaa0af.json))
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. **Its Python call floor is 47 ns against the
49 of the table published through 2.1.0**, so the two columns are comparable; another session
measured a floor of 100 and every row fifty nanoseconds higher, two hours apart and within 2 ns of
each other, and a run half an hour before this one, on a machine still finishing a build campaign,
measured 58 — which is the reproducibility problem this page documents further down and the reason
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
`CompactHashIndex` widths answer in 89–97 ns against a builtin `dict`'s 240, because they store no
keys at all: one hash, one probe, at most a fingerprint to compare. What they cannot do is tell a
stranger from a member with certainty, or give a key back for an id. Among the structures that do
keep their keys, `DictIndex` is **both smaller and faster than every `marisa-trie` setting measured
at either block** — 2.81 B/key and 346 ns at 512, 2.84 and 334 at the default 256, against
2.96–3.07 and 428–476.

Two honest crowns, both scoped to what is measured above — libraries a Python or Rust project can
actually install. The research-grade C++ frontier (MARISA aside, which is installable) has no
binding and is not in these claims: it is listed with its papers and licences under
[the research frontier, cited](#the-research-frontier-cited) and measured on its own protocol under
[the research frontier, measured](#the-research-frontier-measured). **`CompactHashIndex` is the smallest `string → dense id` map here — 2.4×
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
| **lexindex `DictIndex`** (512 per block) | **2.806** | 0.94× | 272 ns | 26 ms |
| **lexindex `DictIndex`** (256 per block, default) | **2.838** | 0.95× | 253 ns | 26 ms |
| `marisa-trie` (C++ reference, default) | 2.978 | 1.00× | — | — |
| `rsmarisa` (4 tries, tiny cache — its smallest) | 3.003 | 1.01× | 322 ns | 137 ms |
| **lexindex `DictIndex`** (64 per block) | 3.029 | 1.02× | **227 ns** | **26 ms** |
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
throughout, and `rsmarisa`'s `io_size()` is asserted equal to the file it saves. The three
`DictIndex` sizes are `68f5336`'s, where the symbol table went one per shard of 65 536 keys — a size
is exact — and the same tree's A-B leaves the `id` column where it was
([`dict-shard-2026-09-12-arz-68f5336.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-shard-2026-09-12-arz-68f5336.txt));
the build column predates it and the change costs 17–23 % of it. None of them is a
lexindex dependency — the harness is a throwaway crate.</sub>

So the statement is a crown after all, with the frontier behind it. **`DictIndex` at its default
block dominates `rsmarisa` at its most compact setting outright** — 2.838 against 3.003 B/key, 253
against 322 ns, 5.3× faster to build — and at 64 per block it is the fastest structure here at 227
ns for 3.029, still under `rsmarisa`'s default; every `rsmarisa` setting has a `DictIndex` block
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
[`positioning-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/positioning-2026-09-12-arz-386b2e6.txt);
the `DictIndex` row re-measured at `68f5336`, where the symbol table went one per shard of 65 536
keys, in
[`dict-shard-2026-09-12-arz-68f5336.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-shard-2026-09-12-arz-68f5336.txt)):

| bytes/key | 479 823 single words | 1 M `word.word` pairs, drawn at random | 1 M `word.word`, 1 000 × 1 000 grid |
|---|---:|---:|---:|
| the bare MPHF (no keys, no membership, no reverse) | 0.26 | 0.26 | 0.26 |
| **lexindex `CompactHashIndex`** (fp = 1 byte) | **1.26** | **1.26** | **1.26** |
| **lexindex `DictIndex`** (256 per block) | **2.84** | 7.59 | 2.50 |
| `marisa-trie` | 2.98 | 6.21 | 2.12 |
| **lexindex `StringIndex`** | 5.95 | 15.19 | 0.68 |
| lexindex `PerfectHashIndex` | 10.90 | 21.93 | 12.52 |

The `DictIndex` row swaps places with the `marisa-trie` one inside a single table — under it on
single words, 1.22× over it on random pairs and 1.18× on the grid — which is the whole argument of
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

`bench/sweep.py` builds eleven structures on each corpus and measures serialised bytes per key,
build time and one lookup: three `DictIndex` block sizes against five `marisa-trie` configurations,
because both are curves and a single point of either invites the objection that the other was left
untuned. Marisa's curve runs to sixteen tries because four — its floor on an English word list, and
what this page took for its floor everywhere — is not where it bottoms out on keys that share more,
or on keys that share nothing. The two keyless indexes are there to show what a structure that stores no keys costs, which
turns out to be the only row a reader can carry to their own corpus without measuring it.

### Bytes per key at one million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa 4 | marisa 8 | marisa 16 | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 7.80 | 7.70 | 7.62 | 17.74 | 7.52 | 7.52 | 7.52 | 7.56 | 7.71 |
| `domains` | 13.8 | 0.26 | 1.26 | 5.14 | 5.07 | 5.01 | 10.50 | 4.81 | 4.80 | 4.80 | 4.87 | 4.99 |
| `idents` | 17.3 | 0.26 | 1.26 | 6.73 | 6.66 | 6.60 | 10.55 | 5.45 | 5.34 | 5.30 | 5.62 | 5.74 |
| `numeric` | 5.9 | 0.26 | 1.26 | 2.18 | 2.13 | 2.08 | 0.00 | 1.62 | 1.62 | 1.62 | 1.64 | 1.72 |
| `opaque` | 16.0 | 0.26 | 1.26 | 13.85 | 13.80 | 13.76 | 21.44 | 18.23 | 15.55 | 15.55 | 18.82 | 19.04 |
| `paths` | 125.0 | 0.26 | 1.26 | 15.11 | 14.67 | 14.34 | 17.48 | 9.26 | 9.04 | 8.83 | 9.47 | 9.60 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 9.46 | 9.37 | 9.31 | 17.28 | 7.79 | 7.52 | 7.49 | 8.19 | 8.37 |
| `titles-ru` | 35.8 | 0.26 | 1.26 | 11.50 | 11.36 | 11.27 | 31.75 | 8.07 | 7.67 | 7.61 | 8.61 | 8.77 |
| `titles-zh` | 16.9 | 0.26 | 1.26 | 8.35 | 8.28 | 8.23 | 17.52 | 6.33 | 6.22 | 6.22 | 6.54 | 6.70 |
| `urls` | 52.4 | 0.26 | 1.26 | 11.46 | 11.25 | 11.11 | 16.88 | 7.95 | 7.61 | 7.57 | 8.39 | 8.58 |
| `uuid` | 36.0 | 0.26 | 1.26 | 20.55 | 20.45 | 20.37 | 37.11 | 32.93 | 22.99 | 22.98 | 34.58 | 34.80 |

<sub>`words` and `pypi` have no million-key file; their 100 000 grid, every build time, and the
100 000 rows for the rest are in
[`bench/results/sweep-2026-09-13-arz-4128d6a.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-13-arz-4128d6a.json)
— one run of the whole set, 264 cells, on a clean tree that began at a load average of
0.00 / 0.29 / 0.69. Every column comes from that one run, the three `DictIndex` blocks included, so
this table needs no reconciliation between harnesses.</sub>

**Where `marisa-trie` wins, it wins on shared structure.** It is the smallest key-storing structure
on nine of these eleven corpora, and the margin tracks how much the keys have in common: 1.62× on
`paths`, where a million paths run through a few thousand directories, 1.48× on `titles-ru`, 1.47×
on `urls`, 1.32× on `titles-zh`, 1.24× on `idents` and on `titles-en` — and within 5 % on `dna` and
`domains` (1.01× and 1.05×). That is what a LOUDS trie is for, and front coding in fixed blocks does
not answer it.

**Its floor is eight or sixteen tries on nine of the eleven, not the four this page quoted through
3.0.0.** The recursion keeps paying wherever the keys share — `titles-ru` 8.07 → 7.61, `urls`
7.95 → 7.57, `paths` 9.26 → 8.83, about 5 % each — and it pays most where they share least: `opaque`
18.23 → 15.55 and `uuid` 32.93 → 22.98, 15 % and **30 %** of the blob. The two corpora where four
tries is not beaten are `dna` and `numeric`, where all three settings agree to the hundredth; on
`domains` the gain is 0.3 % and on `titles-zh` 1.8 %, so "tune it" is worth measuring and not worth
assuming. What the tries cost is time, and the cost is where the gain is: `uuid` builds 1.37× and
answers 1.65× slower at sixteen tries than at four, `paths` answers 1.30× slower, and on the corpora
where the setting buys nothing the two are level. Marisa's smallest configuration is its slowest.

**Where lexindex wins, it wins on entropy — and by less than this page used to claim.** `DictIndex`
is the smaller of the two on `opaque` (13.76 against 15.55, **1.13×**) and on `uuid` (20.37 against
22.98, **1.13×**) — keys with nothing to share, where a trie pays for a node per character and front
coding pays for a prefix that is not there. Against marisa's four-try setting the same two margins
read 1.32× and 1.62×, and that is what this page published until the curve was measured to its end:
a tuned marisa takes back four fifths of the `uuid` gap. On `words`, the corpus every table above is
measured on, `DictIndex` at its default block is 3.48 against marisa's 3.70 at 100 000 keys, and
2.84 against 2.96 on the full 479 823 — and there four tries really is marisa's floor, eight and
sixteen reading 3.71 and 3.74. That is a real result on a real corpus, and it is not the general
case: it is the favourable end of a distribution whose other end is `paths`.

**The two keyless rows are flat and every other row is not.** `ClosedHashIndex` is 0.26 bytes a key
and `CompactHashIndex` 1.26 on all eleven corpora at all three sizes, because their size is a
function of `n` and the fingerprint width and of nothing about the keys. Against the best trie that
is a 7.0× margin on `paths` and 1.3× on `numeric` — the same two structures, neither of them
changed, and the whole spread between those two numbers belongs to the corpus.

**`StringIndex` on `numeric` reads 0.00 bytes a key, and it is not a bug.** Ten million dense
decimal ids compile to an FST of **356 bytes**: the automaton is very nearly regular, one path per
digit, and the keys are recovered from the transitions. Read it as the ceiling of what shared
structure can buy an FST and never as a size claim — it is exactly the ~0 B/key that
`bench/compare.py` refuses to report for synthetic `entity-{i}` keys, and it is in this table only
because dense ids are a corpus somebody really has.

### Lookups at one million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa 4 | marisa 8 | marisa 16 | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 86 | 133 | 388 | 409 | 362 | 621 | 947 | 968 | 1054 | 1035 | 1024 |
| `domains` | 13.8 | 78 | 125 | 410 | 401 | 419 | 447 | 708 | 713 | 718 | 686 | 676 |
| `idents` | 17.3 | 93 | 143 | 498 | 494 | 525 | 554 | 995 | 1031 | 1039 | 942 | 909 |
| `numeric` | 5.9 | 65 | 106 | 265 | 281 | 295 | 173 | 291 | 284 | 284 | 281 | 257 |
| `opaque` | 16.0 | 100 | 125 | 429 | 431 | 440 | 429 | 1425 | 1514 | 1530 | 1136 | 1093 |
| `paths` | 125.0 | 137 | 171 | 793 | 786 | 827 | 1324 | 3012 | 3532 | 3928 | 2488 | 2257 |
| `titles-en` | 21.0 | 127 | 162 | 547 | 537 | 547 | 619 | 1246 | 1338 | 1314 | 1205 | 1230 |
| `titles-ru` | 35.8 | 181 | 223 | 649 | 628 | 662 | 972 | 1630 | 1674 | 1812 | 1657 | 1616 |
| `titles-zh` | 16.9 | 97 | 139 | 486 | 483 | 522 | 534 | 1020 | 1054 | 1063 | 985 | 949 |
| `urls` | 52.4 | 113 | 159 | 738 | 720 | 735 | 792 | 1244 | 1367 | 1527 | 1264 | 1229 |
| `uuid` | 36.0 | 123 | 160 | 594 | 537 | 580 | 618 | 1691 | 2717 | 2787 | 1448 | 1386 |

**`DictIndex` answers faster than every `marisa-trie` setting on ten of the eleven corpora** —
1.7–2.9× at its default block, against marisa's *fastest* configuration and not its smallest. The
eleventh is `numeric`, where marisa's fastest is 9 % ahead of the default block (257 ns against 281)
and 3 % ahead of a block of 128 (265): a million dense decimal ids are the corpus where a trie has
the least to walk. `DictIndex` also builds 1.8–3.9× faster than marisa's quickest build everywhere
except `numeric`, where marisa is 1.2× ahead. So the size table above is not the whole trade, and
the settings that make marisa smallest are the ones that make it slowest: on `paths` its sixteen-try
floor is 1.62× smaller than `DictIndex` at 1024 a block, 4.7× slower to answer and 4.0× slower to
build.

**The block is a smaller knob than it was.** A lookup scans one restart a microblock and then one
microblock whatever the block, so 128 → 1024 keys a block moves a lookup by −7 % (`dna`) to +11 %
(`numeric`), where the one-level format paid 3.8× from 32 to 1024, and buys 0.09–0.77 bytes a key
over 128, every corpus in the same direction. It did not use to be: `paths` at 1024 keys a block
stored *more* than at 256 (16.13 against 15.89), and the cause was the symbol table rather than the
layout, whose per-block arrays only shrink as the block grows. The table trained on whole blocks, so
a larger block bought fewer neighbourhoods — 19 at 1024 against 157 at 128 — and a table trained on
19 of them is a lottery. A training run is now a constant 256 keys whatever the block, trained per
shard of 65 536 keys, and `paths` at 1024 reads 14.34. 256 is the default the README quotes.

<sub>Lookups are one run of three rounds of 20 000 probes a cell, so a column is comparable within
itself and a cell carries a few per cent: a second run of the same build read the same bytes to the
last digit and lookups 1–15 % higher on every structure, controls included.</sub>

### Ten million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa 4 | marisa 8 | marisa 16 | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.26 | 1.26 | 7.13 | 7.02 | 6.94 | 15.98 | 6.69 | 6.69 | 6.69 | 6.75 | 6.99 |
| `numeric` | 6.9 | 0.26 | 1.26 | 2.19 | 2.13 | 2.08 | 0.00 | 1.63 | 1.63 | 1.63 | 1.66 | 1.77 |
| `opaque` | 16.0 | 0.26 | 1.26 | 13.42 | 13.37 | 13.33 | 21.47 | 15.19 | 14.72 | 14.72 | 18.33 | 18.68 |
| `titles-en` | 21.0 | 0.26 | 1.26 | 7.74 | 7.65 | 7.59 | 13.25 | 5.57 | 5.49 | 5.48 | 5.71 | 5.87 |
| `urls` | 52.4 | 0.26 | 1.26 | 9.59 | 9.38 | 9.23 | 13.10 | 5.66 | 5.56 | 5.55 | 5.83 | 5.99 |
| `uuid` | 36.0 | 0.26 | 1.26 | 20.08 | 19.98 | 19.90 | 36.07 | 29.97 | 20.62 | 20.62 | 33.21 | 33.56 |

Ten times the keys moves every trie and neither hash: at its floor `marisa` goes 7.49 → 5.48 on
`titles-en` and 7.57 → 5.55 on `urls` as the sharing deepens, `DictIndex` at its default block
9.37 → 7.65 and 11.25 → 9.38, and the two keyless rows do not move at all. The ranking is the same
one the million-key table gives, so the answer to "which is smallest" is decided by the corpus and
not by the scale.

**Scale closes the one gap this page leans on.** `DictIndex` is still the smaller structure on
`uuid` and `opaque` at ten million, but barely: 19.90 against marisa's tuned 20.62 is 3.5 % smaller,
where a million keys gave 11 % and where four tries alone would have it at 34 %. The reason is the trie's, not ours — ten
times the random identifiers share ten times more three- and four-character fragments, and a
recursive trie is built to find exactly that, while a block of front-coded keys shares only with its
own block. Read `uuid` as a gap that closes with n, and do not build a claim on it.

**The block buys less here.** 128 → 1024 keys a block is 0.09–0.36 bytes a key over these six,
against 0.09–0.77 at a million. Ten times the keys is ten times the blocks, so the per-block arrays
a bigger block saves were already a smaller share of the index; what is left is the coded suffixes,
and those are the symbol table's business, not the block's — which is why the table is trained on
runs of a constant length and per shard, and why `titles-en` at 1024 reads 7.59 where whole-block
training over one table left it at 7.93, above its own 256.

<sub>Measured 2026-09-13 at `4128d6a`
([`bench/results/sweep10m-2026-09-13-arz-4128d6a.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-09-13-arz-4128d6a.json)),
`marisa-trie` 1.4.1, one build per cell at this size and three rounds of 20 000 lookups, started at
a load average of 0.07 / 0.42 / 0.66. Every non-marisa cell reads to the hundredth what the
2026-09-12 run read — a size is exact, so the two runs agree wherever the builder did not move, and
what moved is marisa's two new columns.</sub>

### A cold mapping, and what is actually resident

Every size in this page is the blob. A process that maps one and answers a thousand queries never
reads most of it, and what decides whether an index fits inside a container limit is the resident
set. `local/coldmmap` drops each file's page cache with `posix_fadvise(POSIX_FADV_DONTNEED)` — no
root needed, and the resident set straight after `load_mmap` is the control that the drop worked —
then reads `/proc/self/smaps` after 1 000 random lookups and after a further million.

Ten million English Wikipedia titles, bytes per key except the latencies:

| structure | file | mapped | after 1 k | after 1 M | cold ns | warm ns |
|---|---:|---:|---:|---:|---:|---:|
| `DictIndex` 32 | 8.21 | 0.28 | 5.22 | 8.21 | 47 129 | 815 |
| `DictIndex` 32, `MADV_RANDOM` | 8.21 | 0.28 | **1.81** | 8.21 | 89 971 | 1 975 |
| `DictIndex` 256 | 7.65 | 0.06 | 4.57 | 7.65 | 45 477 | 719 |
| `DictIndex` 256, `MADV_RANDOM` | 7.65 | 0.06 | **0.83** | 7.65 | 96 360 | 1 997 |
| `DictIndex` 1024 | 7.59 | 0.03 | 4.60 | 7.59 | 46 612 | 695 |
| `CompactHashIndex` | 1.26 | 0.26 | 1.25 | 1.26 | **6 799** | **75** |
| `CompactHashIndex`, `MADV_RANDOM` | 1.26 | 0.26 | 0.70 | 1.26 | 51 651 | 177 |
| `StringIndex` | 13.25 | 0.01 | 10.10 | 13.25 | 94 367 | 1 039 |
| `StringIndex`, `MADV_RANDOM` | 13.25 | 0.01 | 2.27 | 13.25 | 335 430 | 3 067 |

**`load_mmap` really is lazy**, which the `mapped` column exists to prove: 0.01 to 0.26 bytes a key
resident before the first query, which is the header and the little the loader validates. Nothing
else is read until something asks for it.

**The first thousand queries cost far more pages than they need.** `DictIndex` at 256 ends them with
4.57 of its 7.65 bytes a key resident — 60 % of an index nobody has finished reading — while the
same thousand queries under `MADV_RANDOM` leave **0.83**, which is what they actually touch: about
two pages a lookup, the sample array and the block. The 5.5× between those two numbers is the
kernel's readahead, and it is buying latency with memory: turning it off costs 2.1× on the cold
lookups and **2.8× on the warm ones**, because the advice outlives the warm-up. The warm column is
also where the microblock shows on ten million keys: 1024 a block answers in 695 ns where the
one-level format took 2 108, in a session that read 30 % faster than this one. Readahead is the
right default here; `MADV_RANDOM` is for the case where a container limit, and not a latency budget,
is what binds.

**Cold start is where the smallest structure wins outright, and the mechanism is pages.**
`CompactHashIndex` answers its first thousand queries at **6.8 µs** against `DictIndex`'s 45.5 and
`StringIndex`'s 94.4 — 7× and 14× — because its whole file is 12.6 MB and a fault brings in a
useful fraction of it. On `uuid`, where its 1.26 bytes a key sit against `DictIndex`'s 20.06 and
`StringIndex`'s 36.07, the gap is 21× and 39× (5.9 µs against 125.3 and 231.0). A structure that
stores no keys has no keys to fault in.

**After a million queries every structure is fully resident**, to the last hundredth of a byte. The
distinctive answer to "how much memory does this index need" only exists during warm-up: past it,
against a workload that touches every key, the resident set *is* the file, and the size table above
is the steady-state RSS.

<sub>Measured 2026-09-12 on a clean tree, NVMe under LUKS on btrfs, 38 GB RAM — so "cold" means
this file's page cache was dropped and not that the machine was short of memory
([`bench/results/dict-shard-2026-09-12-arz-68f5336.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-shard-2026-09-12-arz-68f5336.txt),
which re-measured the table after the symbol table went one per shard and reads within 2 % of the
[run before it](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-09-12-arz-40ca73d.txt)
on both controls, and about 30 % slower than the one before that, so its latency column is read
within itself;
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
| `domains` | 4.87 | 5.03 | 680 ms | 57 ms | 466 ns | 267 ns (64) |
| `idents` | 5.50 | 6.62 | 652 ms | 61 ms | 609 ns | 342 ns (256) |
| `numeric` | 1.64 | 2.10 | 98 ms | 46 ms | 161 ns | 190 ns (64) |
| `opaque` | 18.27 | 13.77 | 1 177 ms | 82 ms | 1 047 ns | 299 ns (512) |
| `paths` | 10.09 | 14.46 | 5 642 ms | 95 ms | 1 710 ns | 664 ns (512) |
| `titles-en` | 7.85 | 9.33 | 975 ms | 68 ms | 827 ns | 348 ns (256) |
| `titles-ru` | 8.22 | 11.30 | 1 489 ms | 68 ms | 1 102 ns | 447 ns (256) |
| `titles-zh` | 6.40 | 8.25 | 759 ms | 71 ms | 683 ns | 344 ns (256) |
| `urls` | 8.38 | 11.15 | 2 011 ms | 73 ms | 855 ns | 542 ns (512) |
| `uuid` | 33.01 | 20.40 | 1 508 ms | 115 ms | 1 155 ns | 391 ns (512) |

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
**"Smallest" here is the smallest of its three cache levels at the default number of tries, not its
floor**: the sweep above shows the C++ library bottoming out at eight or sixteen tries on nine of
these corpora, by 30 % on `uuid`, and this harness does not turn that knob. Read the left column as
one point on a curve whose other end is not measured.

<sub>Measured 2026-09-12 on a clean tree
([`bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-12-arz-386b2e6.txt)),
`rsmarisa` 0.4.2, in one process per corpus, six lanes alternating within each of five rounds. The
`DictIndex` size column is `68f5336`'s, where the symbol table went one per shard of 65 536 keys;
its build column predates that and the change costs 17–23 % of it, while the same tree's A-B leaves
the lookups where they were
([`dict-shard-2026-09-12-arz-68f5336.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-shard-2026-09-12-arz-68f5336.txt)). It is
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
Measured 2026-09-13 at `8aaa0af`, the v3.0.0 tag in a clean worktree: six runs of the example back
to back, each lookup cell the minimum of five timed passes after a warm-up pass
([`bench/results/latency-rs-2026-09-13-arz-8aaa0af.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-09-13-arz-8aaa0af.txt)).
The table quotes the minimum over runs 3–6. Runs 1–2, minutes after a reboot, read 18–44 % quicker
on **every** row — `StringIndex` +34 %, `std::HashMap` +23 %, `BTreeMap` +28 %, `CompactHashIndex`
+44 % — which is the part holding its boost clock while it is cold, not eight structures improving
at once; runs 3–6 agree within 2.3 % of each other. Absolute numbers are machine-dependent — the
`std::HashMap` control reads 285 ns here against the 295 of the 2.1.0 table, the 289 of the 2.0.0
one and the 245 of the 1.1.0 one — so compare the **ratios**, only within a column, and read a shift
under ~15 % between tables as the session: against this `HashMap`, `CompactHashIndex::id` is 0.45×,
`id_unchecked` 0.26×, `PerfectHashIndex::id` 1.03×, `StringIndex` 1.47×, `DictIndex` 1.90×,
`BTreeMap` 3.24×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~48 ms** | ~128 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~289 ms | **~75 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~209 ms | ~285 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~284 ms | ~293 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~272 ms | ~420 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (256 per block) | ~174 ms | ~541 ns | ordered, exact reverse; 2.50 B/key here against the FST's 0.68 — a `word.word` cross product is what a transducer factors out, and what a block of front-coded keys does not (on the dictionary: 2.84 against 5.95, 298–302 ns against 265–272) |
| `std::BTreeMap<String, u32>` | ~229 ms | ~924 ns | in-RAM |

<sub>**The memory-bound rows swing between sessions; nothing else does.** Over four sessions the two
rows whose code has not moved since 0.5.1 read `StringIndex` 1.30× → 1.47× → 1.32× → **1.47×** of
`HashMap` and `BTreeMap` 3.19× → 3.34× → 2.63× → **3.24×**, with no trend and no single outlier,
while the FxHash map sits at 0.586× → 0.60× → 0.58× → **0.60×** throughout. The two that move are
exactly the two that miss to DRAM, they move together and in the same direction, and they move
across sessions in which their code did not change — the signature of the machine, not of a
release. The rows 2.0's key hash reaches stayed put across all four: `PerfectHashIndex::id` 0.95×
→ 1.04× → 1.02× → **1.03×**, `id_unchecked` 0.269× → 0.26× → 0.24× → **0.26×**,
`CompactHashIndex::id` 0.435× → 0.45× → 0.45× → **0.45×**. `DictIndex` is the one row that moved
for a reason other than the session: at the default of 256 keys a block it reads **541 ns for
2.50 B/key** where 2.0.0's 32 a block read 507 for 3.19 — the packed offsets and the microblocks
together, 22 % of the bytes for 7 % of the lookup. `CompactHashIndex` builds in 48 ms against 69 on
1.1.0, the one build that outran the session. This corpus is its worst case by
construction: the 1 M keys are 1 000 words crossed with 1 000, which the transducer stores once per
factor (0.68 B/key) and a block of front-coded keys stores once per key. Real keys move lookups in
lexindex's favour versus synthetic ones, while every `build` reads higher than a synthetic sequence
would, because real input is not pre-sorted and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~172 ns** and `PerfectHashIndex::id_unchecked`
**~75 ns** — so on a closed vocabulary the perfect hash is about **2.3× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash. `CompactHashIndex::id` (~128 ns) is
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
dependency of lexindex: `cd bench/mphf_vs && cargo run --release -- <keys> <rounds> <threads>`. One
process builds every function over the **same distinct splitmix64 keys** in turn, three rounds
A-B-A-B (builds: the minimum), and looks every key up in **one shuffled probe order** (the minimum
of nine passes) — the keys copied into an array in that order, so that a query reads its own key
from a sequential stream, and each function's lookup inlined into a loop of its own, so that the
column is the function alone. Beside every minimum the harness prints its spread, the slowest
repeat over the fastest, and a batch column: lexindex's `index_all` and `ptr_hash`'s
`index_stream::<32, _>` over chunks of 4096 keys, the two forms that pull a later key's cache line
in while the current one resolves (`ph` has no batch form). The construction peak is the process's
high-water mark over the build above what it held before it — the memory a build needs beyond the
keys — exact where the tables are large enough to be mapped whole (10 M and 100 M). `ph` 0.11.0
is the PHast authors' crate, with its `wyhash` feature on (its default hasher is std's SipHash,
which nobody benchmarks it with): `Function2` with `ShiftOnlyWrapped` is PHast+ with wrapping, the
design this crate's `MPH3` grew out of; `Function` with `SeedOnly` is regular PHast. Both at 8-bit
seeds and bucket size 4.5, as here. `ptr_hash` 2.1.1 is PtrHash's three parameter sets. Bits and
lookups are the 1-thread process's; the 8-thread process builds the same functions to within
0.02 bits.

**1 M keys** — 246 KB of table, inside L2:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.966 | **55.7 ns/key** | **11.1 ns/key** | 2.6 ns | 2.8 ns |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.160 | 58.3 | 17.7 | 3.5 | — |
| `ph` PHast (`SeedOnly`) | **1.933** | 573.3 | 99.7 | 2.9 | — |
| `ptr_hash` compact | 2.144 | 147.8 | 80.7 | 4.0 | 4.2 |
| `ptr_hash` balanced | 2.378 | 92.7 | 48.1 | 4.1 | 4.2 |
| `ptr_hash` fast | 2.990 | 72.9 | 64.6 | **1.8** | **2.4** |

**10 M keys** — 2.4 MB, inside the 16 MB L3:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.953 | **54.4 ns/key** | **9.2 ns/key** | 3.8 ns | 3.0 ns |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.147 | 61.3 | 15.4 | 5.3 | — |
| `ph` PHast (`SeedOnly`) | **1.921** | 584.1 | 90.9 | 4.2 | — |
| `ptr_hash` compact | 2.143 | 163.0 | 44.0 | 5.5 | 4.7 |
| `ptr_hash` balanced | 2.378 | 103.8 | 28.2 | 5.6 | 4.7 |
| `ptr_hash` fast | 2.990 | 149.0 | 136.5 | **2.9** | **2.8** |

**100 M keys** — 24 MB, every scalar lookup a DRAM miss:

| function | bits/key | build, 1 thread | build, 8 threads | build peak | lookup | batch |
|---|---:|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.949 | **56.2 ns/key** | **9.0 ns/key** | **60 MB** | 13.7 ns | **3.8 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.146 | 71.9 | 17.6 | 1 526 | 17.3 | — |
| `ph` PHast (`SeedOnly`) | **1.920** | 607.7 | 93.6 | 1 525 | 13.5 | — |
| `ptr_hash` compact | 2.143 | 164.5 | 42.9 | 906 | 16.5 | 6.1 |
| `ptr_hash` balanced | 2.378 | 105.0 | 26.1 | 887 | 17.3 | 6.1 |
| `ptr_hash` fast | 2.990 | 241.0 | 223.0 | 1 477 | **10.7** | 5.0 |

`MPH3` has the fastest build in every table and the flattest: 54–56 ns/key from 1 M to 100 M on
one thread and 9–11 on eight, where PHast+ goes 58 → 72 and PtrHash's fast set 73 → 241. Against
the PHast+ it grew out of that is 1.05–1.3× on one thread and 1.6–2.0× on eight; against PtrHash's
compact set 2.7–3.0× and 4.7–7.3×. The construction peak is the column apart: 6 MB at 10 M and
60 MB at 100 M above the keys, against 152–156 MB and 887–1 526 MB — the table is built in chunks
of keys, and nothing the size of the key set is ever held beside it. Among the rows near 2 bits its
lookups are the fastest at every size, single and batch: 2.6 / 3.8 / 13.7 ns against PtrHash
compact's 4.0 / 5.5 / 16.5 and PHast+'s 3.5 / 5.3 / 17.3; and its batch is the fastest of all at
100 M, 3.8 ns against fast's 5.0 and compact's 6.1, second at 1 M and 10 M (2.8 / 3.0 against
fast's 2.4 / 2.8). What it does not have: PtrHash's fast set answers a single lookup faster at
every size, 1.8 / 2.9 / 10.7 ns, at 3 bits a key — a table half again as large whose one probe is
a seed byte with no bumped keys' chain behind it; and regular PHast is the smallest function,
1.92 bits, within 0.4 ns of `MPH3`'s single lookup at 1 M and 10 M and level with it at 100 M
(13.5 against 13.7), at ten times the build and without a batch form. The bits column is the
serialised table, which `MPH3` loads as it is — the rank and select counts `MPH2` derived in
memory, 0.023 bits/key, went with the line remap. One asymmetry is in the numbers and should be
read out of them: lexindex takes the keys as 64-bit hashes (its indexes hash the string once,
before), while `ph` hashes each key with wyhash on build and on every lookup level and `ptr_hash`
with one multiply — a nanosecond or two of the gap on the `ph` rows is that.

The lexindex row ran once more at each size in a process with transparent huge pages turned off
(`prctl(PR_SET_THP_DISABLE)`), the control for the `MADV_HUGEPAGE` its tables ask for from 2 MiB
up: 2.7 / 3.0 ns at 1 M (nothing to get below one huge page), 4.2 / 3.3 against 3.8 / 3.0 at 10 M
and 14.1 / 4.1 against 13.7 / 3.8 at 100 M — 10 % on the single lookup and the batch at 10 M, 3 %
and 7 % at 100 M — with the results file's `huge MB` column showing the pages granted: 2 of the
2.4 MB at 10 M, 20 of the 24 at 100 M.

How the batch got there. Before this measurement `index_all` prefetched only the first level's
seed, sixteen keys ahead, and stopped at 13.5 ns at 100 M against `index_stream`'s 6.9: the ~3 %
of keys the first level bumps made four more dependent misses each — their next level's seed, the
rank words, the select sample, the high words — and a prefetch that names one line cannot hide a
chain. The keys go through the first level in blocks of 1024 now, each seed pulled in 64 keys
ahead; the bumped keys of a block are answered after it in five stages that run eight keys apart,
each stage pulling in what the next one reads. Below 2^18 first-level seeds (about 1.2 M keys) the
level sits in L2 and the batch is the single lookup in a loop, which is why the 1 M batch is the
single lookup plus the cost of writing its answers out. The single lookup gained from the same
work: the remap's rank reads a count per word
instead of counting a block's words in a loop, and the Elias–Fano select compares a window of
counts at once and finds the bit inside its word without a loop, where the old scan's mispredicted
exits were most of a bumped key's cost (3.7 → 3.2, 5.8 → 4.9 and 20.1 → 15.8 ns). Since
then the remap became one line of Elias–Fano (one prefetch stage fewer), the batch keeps the
buckets it finds in a ring and reads the geometry as immediates (54 → 47 instructions a key), and
the tables sit on huge pages: the 100 M batch went 5.8 → 3.8 ns and the single lookup 15.8 → 13.7.

**ConsensusRecSplit** ([Lehmann, Sanders, Walzer, Ziegler 2025](https://arxiv.org/abs/2502.05613)),
the space record at 1.444 + ε bits, is C++20 under the GPL, so it ran in its own process on the
authors' harness at their commit `11deea6`: XorShift64 keys, one construction timed in
milliseconds, the queries drawn with replacement into a plan array (as above, no key fetch inside
the query), single-threaded, one run per configuration, on the same machine right after the tables
above. `k` is the bucket size, ε the overhead over the bound, `-o` the query-optimised variant.

| function | 1 M: bits/key | build | lookup | 10 M: bits/key | build | lookup |
|---|---:|---:|---:|---:|---:|---:|
| ConsensusRecSplit k = 256, ε = 0.1 | 1.583 | 288 ns/key | 61 ns | 1.579 | 300 ns/key | 97 ns |
| … k = 256, ε = 0.1, `-o` | 1.580 | 419 | 80 | 1.575 | 433 | 86 |
| … k = 256, ε = 0.01 | 1.478 | 1 706 | 55 | — | — | — |
| … k = 32 768, ε = 0.1 | 1.620 | 654 | 106 | — | — | — |
| … k = 32 768, ε = 0.01 | 1.493 | 2 083 | 105 | **1.459** | 2 140 | 156 |
| … k = 32 768, ε = 0.01, `-o` | 1.491 | 3 229 | 130 | **1.459** | 3 353 | 146 |
| **lexindex `MPH2`**, one thread | 2.099 | **44.9** | **3.7** | 2.088 | **43.3** | **5.8** |

`MPH2`'s row is its 2026-09-13 measurement, taken beside the Consensus run; `MPH3` in the tables
above is 1.966 / 1.953 bits at 2.6 / 3.8 ns. The trade is 0.5 bits a key — on a `CompactHashIndex`
at 1.24 bytes a key, 5 % of the index — for 5.5–62× the build and 23–41× the lookup at 10 M. That
is the right answer for an archive written once and read rarely; this crate's tables are read on
every `id`, and 1.95 bits is where its build and lookup stay first.

What this campaign does not have. A second CPU: one Ryzen 7 5800HS, a mobile part with 16 MB of
L3, and the 100 M rows are where its memory system shows — a server part with a larger cache and
more channels moves every row there. A 1 B row in the tables: one run on a warm machine, the probe
order sampled to 10 M keys because the full one is another 8 GB beside a 15 GB construction peak,
built `MPH3` over 10⁹ keys in 10.5 s on eight threads and **600 MB above the keys**, against
8.2–14.5 GB for PtrHash's three sets and 9.7 GB for the `ph` rows, at 1.948 bits — and put its
lookups behind PtrHash's there (30 against 26 ns single, 9.9 against 8.4 in a batch, where 100 M
reads 13.7 against 16.5 and 3.8 against 6.1): the bumped keys' lines leave the cache between 100 M
and 1 B. That is being worked on, and the row waits for a cool run after it. Confidence intervals:
in their place, every cell's minimum with the spread of its repeats, in the results file — builds
repeat within 5 % on one thread and 8 % on eight but for the 11–18 ms builds at 1 M (lexindex
33 %, PHast+ 13 %), lookups within 16 % at 1 M and 15 % at 10 M (3–40 ms a pass) and 3 % at
100 M, batches within 15 % below 100 M and 3.3 % there.

<sub>The three tables were measured 2026-09-16 at `b4544db`
([`bench/results/mphf-vs-2026-09-16-arz-b4544db.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-16-arz-b4544db.txt)),
two minutes after a reboot with only an editor open, Tctl 49 °C and load 0.3 at the start. Two
corrections to the harness are in that commit, and they change how the earlier files read. `ph`'s
default hasher is std's SipHash-1-3 — its `seedable_hash` dependency has default features off —
so every earlier file's `ph` rows timed a configuration the crate does not intend (PHast+ 8.8 /
12.9 / 42.5 ns in
[`mphf-vs-2026-09-16-arz-c1ff98b.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-16-arz-c1ff98b.txt)
against 3.5 / 5.3 / 17.3 with wyhash, its build 69.6 / 77.7 / 102.7 against 58.3 / 61.3 / 71.9),
and their headers' "wyhash" is wrong. And the lookup loops were inlined into one large function
beside each row's build, which cost lexindex 0.7–3.6 ns a lookup and no other row anything
measurable: a cool run of the previous harness the same morning read 3.3 / 4.8 / 17.3 against
2.6 / 3.8 / 13.7 here. The old files stay as they were measured; the `MPH2` of the c1ff98b tables
(2.099 / 2.088 / 2.086 bits, 42.9 / 40.8 / 41.2 ns builds) is what 1.1 to 3.0 write, and `MPH3`'s
build is 1.3× its for 6 % fewer bits and half the bumped keys. The file before,
[`mphf-vs-2026-09-13-arz-a5ad812.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-13-arz-a5ad812.txt),
is the one the Consensus table's `MPH2` row and
[`mphf-consensus-2026-09-13-arz-a5ad812.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-consensus-2026-09-13-arz-a5ad812.txt)
were measured beside. The 2026-09-10 file's lookups
([`mphf-vs-2026-09-10-arz-5ba9f36.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-10-arz-5ba9f36.txt))
are five times those — 29 ns for `MPH2` against the c1ff98b file's 5.8, 46 against 6.6 for PtrHash
compact — because that harness read each probe key through `keys[order[i]]`, a random 8-byte fetch
from an 80 MB array per query, and so charged every row one DRAM miss that was the caller's, not
the function's; the builds agree within 7 %.</sub>

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
| bytes per key | 5.95 | 3.23 | 3.03 | 2.90 | **2.84** | 2.81 | 2.79 |
| of which per-block arrays | — | 0.348 | 0.231 | 0.144 | 0.100 | 0.077 | 0.066 |
| microblocks a block | — | 1 | 2 | 4 | 8 | 16 | 32 |
| entries a lookup scans | — | 31 | 32 | 34 | **38** | 46 | 62 |
| build | 129–135 ms | 23–30 | 25–26 | 24–25 | 24 | 23–27 | 29–36 |
| `id`, member | 265–283 ns | 252–254 | 272–284 | 285–288 | **298–302** | 316–322 | 344–347 |
| `id`, stranger | 215–219 ns | 204 | 223–224 | 234–238 | 255–257 | 267–275 | 292–302 |
| `key_into` (no allocation) | — | 154–155 | 169–172 | 184–188 | **207** | 227–232 | 263–272 |
| `key` (owned string) | 456–474 ns | 205–212 | 232–234 | 249–258 | 278 | 293–307 | 340–348 |
| `lower_bound`, stranger | — | 203–206 | 222–224 | 234–239 | 255–257 | 264–274 | 290–303 |
| `prefix`, 2 000 three-byte prefixes of ~760 keys | — | 29.1–29.6 µs | 28.7–30.1 | 28.6–30.2 | 30.2–30.3 | 29.1–30.2 | 29.1–30.3 |

Two runs of the same ladder in opposite orders; where they differ the table gives both. At the
default 256, `DictIndex` answers `id` within 10 % of `StringIndex` and `key_into` at less than half
its `key` while storing **52 % less**; at 64 per block it is level with `StringIndex` on `id` at
3.03 B/key, and at 32 it is 6 % ahead. The sequential lane does not move with the block at all.

**A lookup scans a microblock, not a block.** A block is cut into microblocks of 32, and the first
key of each is a restart front-coded against the restart before it, so a lookup walks the restarts
to one microblock and scans that: `block / micro + micro − 2` entries, which is 38 at the default
where the block holds 255. That is what unties the size from the latency. Against the
one-level format at the same size the reverse lookup halves and `id` does not move: **244–246 ns
and 392–399 for 2.819 B/key, where one level at 128 keys a block stored 2.827 for 460–464 and
393**. The one-level format reaches 0.10 B/key further down, and that is what it costs: its floor
of 2.701 at 1024 keys a block answers `key_into` in 3 190 ns and `id` in 984, against 287–289 and
420–426 for 2.80 here. Both ladders ran alternated by process in one session, the base built from
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
front-coded data moves the other way and less, 2.57 to 2.70, a restart every 32 keys being coded
against a key that far away. What the block costs is now `block / 32 + 30` entries scanned rather
than `block − 1`: 1024 scans 62 where 32 scans 31. At 512 the index is **2.81 bytes
per key, well under the 2.98 marisa stores on this corpus** — and unlike marisa it answers `key(id)`
and `lower_bound` at all, since a marisa id is not the lexicographic rank (7 051 of 19 999
consecutive sorted pairs come back with a decreasing id). Pick 32 or 64 if the lookups are hot,
512 or 1024 if the bytes are — or name the point instead of the number: `DictProfile::Fast`,
`Balanced` and `Compact` in Rust, `block="fast" | "balanced" | "compact"` in Python, are 32,
256 and 1024.

<sub>The latency rows are 2026-09-12 at `386b2e6`
([`bench/results/dict-micro32-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-micro32-2026-09-12-arz-386b2e6.txt)),
two runs of the ladder in opposite orders, the table giving both where they differ. The bytes and
the per-block arrays are exact and come from `68f5336`, where the symbol table went one per shard of
65 536 keys, and so does the build row
([`bench/results/dict-shard-2026-09-12-arz-68f5336.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-shard-2026-09-12-arz-68f5336.txt)) —
that session reads about 25 % above the latency rows', its `StringIndex` control building in
129–135 ms where theirs took 105–106, and the same artifact's A-B puts the change's own share of
the build at 17–23 %. That A-B moves `id` by +0.5 / −2.0 / −2.4 % on three corpora against a control
moving −0.7 / −2.5 / −0.4 %, which is why the latency rows are the earlier session's and stand.</sub>

## Prefix queries against the tries

A prefix is a range of a sorted dictionary, so `DictIndex` answers one without an automaton:
`prefix_id_range` is two `lower_bound`s and `prefix` walks the run they name. `local/prefixbench.py`
puts that against the tries a Python project can install, on the same words — 20 000 three-byte
prefixes drawn from random words, so the average prefix carries 763 keys.

| | bytes/key | `prefix_count` | first 10 | every match + ids | keys only |
|---|---:|---:|---:|---:|---:|
| **lexindex `DictIndex` 256** (default) | **2.84** | **375 ns** | 2 015 ns | 102 081 ns | 72 541 ns |
| **lexindex `DictIndex` 512** | **2.81** | 396 | 2 159 | 102 532 | 73 384 |
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
| **lexindex `DictIndex` 256** (default) | **2.84** | 2 502 | 984 |
| **lexindex `DictIndex` 512** | **2.81** | 2 642 | 1 042 |

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
| strict avalanche, both hashes, key lengths 4–33 | an input bit that does not reach every output bit | worst 4.31 over 44 032 cells |
| bit independence, 2 016 output pairs per input bit | two output bits that flip together | worst 4.52 over 258 048 cells |
| two-byte differential, every position pair and bit pair | the pre-2.0 collision family: keys differing at bytes `8i+7` and `8i+11` collided in **both** hashes 13–100 % of the time through 1.1 | **0** double collisions over 17 664 combinations |
| per-corpus distribution — slot-hash collisions, top 12 bits, low 12 bits, the 8-bit fingerprint, and the joint (slot, fingerprint) table | a family of keys the hash folds together, and any correlation between the two hashes | ten corpora, no collisions, every value under 2.4 |

The ten corpora are the shapes that break hashes in practice: dictionary words, word bigrams, a
shared prefix (`https://example.com/a/b/…`), a shared suffix (`…@mail.example.com`), a dense
numeric tail (`key_000000001`), plain decimal integers, UUIDs, Cyrillic, DNA, and filesystem paths.

<sub>Committed output:
[`bench/results/hash-quality-2026-09-16-arz-67650b3.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-09-16-arz-67650b3.txt)
(the 4.0 hash; the 2.0–3.x hash's run of the same battery is
[`hash-quality-2026-09-12-arz-d82e296.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-09-12-arz-d82e296.txt)).
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
| `DictIndex` 256 | 2.84 | 263.8 | 265.6 | 264.1 | 276.7 | 265.4 – 265.9 |
| `DictIndex` 512 | 2.81 | 283.9 | 286.0 | 284.1 | 296.0 | 285.7 – 286.6 |

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

## The research frontier, cited

The tables above measure libraries a Python or Rust project can install. The academic state of the
art in compressed string dictionaries is research-grade C++ with no such binding, so it is listed
here and [measured in a harness of its own](#the-research-frontier-measured) rather than claimed
against in those tables. Each entry says what it is, where the code is, and under what
licence — the licence matters, because a benchmark harness that vendors GPL or non-commercial code
is not something this repository can carry.

| Structure | Paper | Code, licence | What it is |
|---|---|---|---|
| MARISA | Yata, 2011 (the reference implementation's own notes) | [`s-yata/marisa-trie`](https://github.com/s-yata/marisa-trie), BSD-2-Clause / LGPL-2.1 | recursive nested patricia tries with tail sharing; the one competitor above that *is* installable, as `marisa-trie` on PyPI |
| PDT | Grossi, Ottaviano, *Fast compressed tries through path decompositions*, JEA 2014 | [`ot/path_decomposed_tries`](https://github.com/ot/path_decomposed_tries), MSR-LA — **non-commercial** | centroid path decomposition, labels vbyte- or csp-coded |
| FST, SuRF | Zhang et al., *SuRF: Practical range query filtering with fast succinct tries*, SIGMOD 2018 | [`efficient/SuRF`](https://github.com/efficient/SuRF) and [`kampersanda/fast_succinct_trie`](https://github.com/kampersanda/fast_succinct_trie), Apache-2.0 | LOUDS-Dense over LOUDS-Sparse. SuRF is a **filter** with false positives, not a dictionary — it answers membership and range, never `id → key` |
| XCDAT | Kanda, Morita, Fuketa, *Compressed double-array tries for string dictionaries supporting fast lookup*, KAIS 2017 | [`kampersanda/xcdat`](https://github.com/kampersanda/xcdat), MIT | xor-compressed double-array trie; the fast-lookup end of the space |
| CoCo-trie | Boffa, Ferragina, Tosoni, Vinciguerra, SPIRE 2022 and *Information Systems* 2024 | [`aboffa/CoCo-trie`](https://github.com/aboffa/CoCo-trie), **GPLv3** | subtries collapsed by a bottom-up optimisation over a pool of integer codes |
| C² | Zhang, Zhao, Xu, *Cache-conscious succinct tries with adaptive unary path compression*, arXiv:2606.16104, 2026 | [`alexztc/C2`](https://github.com/alexztc/C2), MIT | a cache-conscious layout and an FSST/Re-Pair tail container applied to FST, CoCo and MARISA; ships one harness that builds all of the above |
| libCSD | Martínez-Prieto, Brisaboa, Cánovas, Claude, Navarro, *Practical compressed string dictionaries*, Information Systems 2016 | [`migumar2/libCSD`](https://github.com/migumar2/libCSD), LGPL-2.1 | front coding (PFC, HTFC), Re-Pair and FM-index dictionaries — **this is `DictIndex`'s lineage**: front coding in blocks with a restart every *k* keys is that paper's PFC |
| IBiS | Brisaboa, Cerdeira-Pena, de Bernardo, Navarro, *Improved compressed string dictionaries*, CIKM 2019 | [gitlab.lbd.udc.es/gdebernardo/improved-csd](https://gitlab.lbd.udc.es/gdebernardo/improved-csd) | hierarchical front coding with Re-Pair and DACs; the direct descendant of the row above |

Two structures in that list are what `DictIndex` is measured against in spirit: PFC/HTFC, which it
descends from, and MARISA, which it is benchmarked against throughout this page because it is the
one with a binding. The next section runs six of them on the same corpora under one protocol; SuRF
is a filter with no `id` to time, and libCSD and IBiS are not in that harness yet.

## The research frontier, measured

The C² paper ships one benchmark that builds its three cache-conscious structures — C²-FST, C²-CoCo
and C²-MARISA — beside everything it measures them against: FST, CoCo-trie, MARISA, PDT, ART and
C-ART. `bench/frontier/build.sh` builds that benchmark with every dependency at a pinned commit, and
XCDAT's four trie types beside it; `bench/frontier/run.sh` runs all of them against `DictIndex` at
blocks of 32, 256 and 1024 keys and `StringIndex`, over the corpus set above at a million keys and,
where a corpus has them, ten million. Nothing of the competitors is vendored or linked: it is fetched
into a gitignored directory, which is what lets the campaign include GPLv3 and non-commercial code.

The protocol is `benchmark.cpp`'s, not this page's: read the file, sort and deduplicate it, build
once, take the structure's own account of its size, then look every key up **once**, in one fixed
shuffle, and report the mean of that single pass. There is no warm-up, so these numbers do not
compare with the warm tables above. `xcdat_frontier` and `frontier_lex` repeat the protocol line for
line, down to where the query bytes live: `benchmark.cpp` looks up a shuffled
`std::vector<std::string>`, which keeps a key of up to 15 bytes inside the string object, and
`frontier_lex` lays such keys out in probe order too — borrowing every query from its key vector
charged each lookup a fetch from a random place in the heap that the C++ rows do not pay, and read
`DictIndex` 14–30 % slow on the corpora of short keys. The C++ is compiled with the flags C² builds
with, `-O3 -march=native`; lexindex is built as it ships, without `target-cpu=native`.

Every structure runs in its own process, three rounds of them, every other round in reverse order,
and each process first waits until the rest of the machine keeps less than one CPU busy for a
second. A cell is the median of its three rounds; the logs keep every run, and how busy other work
kept the machine while it ran. Sizes are each structure's own account: the serialised blob for
lexindex, `space_cost()` for the benchmark's structures, `memory_in_bytes` for XCDAT. ART and C-ART
count their nodes and not the keys they point to, so they are kept out of every comparison. ρ is the
recursion depth `benchmark.cpp` takes as `max_recursion` — for MARISA, one less than its number of
tries. A structure is on the *front* when no other one is at least as small and at least as fast.

<!-- table: frontier bench/results/frontier-1m-2026-09-15-arz-3d107c4.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `words-full` | 2.84 @ 252 | lexindex Dict 1024 2.79 @ 292 | XCDAT 15 7.30 @ 78 | Dict 1024, Dict 256, Dict 32 |
| `dna-1000000` | 7.70 @ 338 | CoCo 6.14 @ 357 | XCDAT 15 22.46 @ 277 | Dict 1024, Dict 256, Dict 32 |
| `domains-1000000` | 5.07 @ 306 | MARISA ρ=2 4.87 @ 537 | XCDAT 15 10.33 @ 147 | Dict 1024, Dict 256, Dict 32 |
| `idents-1000000` | 6.66 @ 382 | MARISA ρ=2 5.61 @ 717 | XCDAT 15 13.78 @ 197 | Dict 256, Dict 32 |
| `numeric-1000000` | 2.13 @ 193 | lexindex StringIndex 0.0003 @ 111 | XCDAT 15 7.05 @ 54 | StringIndex |
| `opaque-1000000` | 13.80 @ 412 | CoCo 11.11 @ 508 | XCDAT 16 20.16 @ 209 | Dict 1024, Dict 256, Dict 32 |
| `paths-1000000` | 14.67 @ 769 | MARISA ρ=2 9.47 @ 2239 | XCDAT 15 25.11 @ 636 | Dict 1024, Dict 256, Dict 32 |
| `pypi-full` | 4.91 @ 318 | MARISA ρ=2 4.51 @ 535 | XCDAT 15 9.92 @ 138 | Dict 1024, Dict 256, Dict 32 |
| `titles-en-1000000` | 9.37 @ 410 | MARISA ρ=2 8.19 @ 878 | XCDAT 15 17.13 @ 244 | Dict 1024, Dict 256, Dict 32 |
| `titles-ru-1000000` | 11.36 @ 482 | MARISA ρ=2 8.61 @ 1229 | XCDAT 15 23.08 @ 364 | none |
| `titles-zh-1000000` | 8.28 @ 374 | MARISA ρ=2 6.54 @ 752 | XCDAT 15 13.58 @ 187 | none |
| `urls-1000000` | 11.25 @ 651 | MARISA ρ=2 8.39 @ 1011 | XCDAT 15 18.14 @ 360 | none |
| `uuid-1000000` | 20.45 @ 499 | lexindex Dict 1024 20.37 @ 556 | XCDAT 15 38.88 @ 340 | Dict 1024, Dict 256, Dict 32 |
<!-- /table -->

<sub>A million keys, measured 2026-09-15 at `3d107c4`
([`bench/results/frontier-1m-2026-09-15-arz-3d107c4.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-09-15-arz-3d107c4.json)),
Ryzen 7 5800HS, GCC 16.2.1, rustc 1.98.1. A cell reads bytes a key @ nanoseconds a lookup.
`paths`, `urls` and `uuid` were measured a second time at the same commit
([`frontier-named-2026-09-15-arz-3d107c4.log`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-named-2026-09-15-arz-3d107c4.log))
after one of their rounds in the campaign ran beside up to 1.5 busy CPUs of other work, and those
runs replace the campaign's, whose own stay in its log. Other work while the 949 processes behind
the table ran: median 0.07 busy CPUs, most 0.73.</sub>

lexindex is on the front on ten of the thirteen corpora — `DictIndex` on nine, `StringIndex` on
`numeric` — and `DictIndex` builds before every compressed trie on all thirteen: 1.4× ahead of the
next on `words` and `numeric`, 2.3–8× on the rest. It is the smallest structure outright on `words`,
2.79 bytes a key at block 1024 against MARISA's 2.98 at ρ=2, and on `uuid`, 20.37 against PDT's
21.44 from a 17-second build; `StringIndex` is on `numeric`, where the automaton folds a million
decimal ids into about 300 bytes. Everywhere else a trie is smaller: MARISA at ρ=2, by 3 % on
`domains` up to 34 % on `paths`, and CoCo-trie on `dna` and `opaque`, by 19 % at 109–219 times the
build. XCDAT is the fastest structure on every corpus, at 1.46–3.3 times the bytes of `DictIndex` at
its default block. On `titles-ru`, `titles-zh` and `urls`, C²-MARISA at ρ=1 or 2 is both smaller and
faster than `DictIndex` at any block, and lexindex is off the front.

<!-- table: frontier bench/results/frontier-10m-2026-09-15-arz-3d107c4.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `dna-10000000` | 7.02 @ 606 | C²-CoCo ρ=1 5.98 @ 940 | lexindex Dict 1024 6.94 @ 522 | Dict 1024 |
| `numeric-10000000` | 2.13 @ 341 | lexindex StringIndex 3.6e-05 @ 133 | lexindex StringIndex 3.6e-05 @ 133 | StringIndex |
| `opaque-10000000` | 13.37 @ 684 | lexindex Dict 1024 13.33 @ 670 | XCDAT 15 21.66 @ 333 | Dict 1024 |
| `titles-en-10000000` | 7.65 @ 684 | MARISA ρ=2 5.71 @ 1506 | XCDAT 15 13.95 @ 582 | Dict 1024 |
| `urls-10000000` | 9.38 @ 966 | MARISA ρ=2 5.83 @ 1635 | XCDAT 15 14.75 @ 698 | none |
| `uuid-10000000` | 19.98 @ 753 | lexindex Dict 1024 19.90 @ 768 | XCDAT 15 38.58 @ 552 | Dict 1024, Dict 256 |
<!-- /table -->

<sub>Ten million keys, measured 2026-09-15 at `3d107c4`
([`bench/results/frontier-10m-2026-09-15-arz-3d107c4.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-09-15-arz-3d107c4.json)),
the same machine and toolchain, with two hours allowed a process and none reaching it. Other work
while the 416 processes ran: median 0.03 busy CPUs, most 0.37.</sub>

At ten times the keys lexindex stays on the front of five corpora out of six, `DictIndex` on four.
It is the smallest structure on `opaque` and `uuid`, by 3 % over PDT, whose builds there take 115
and 124 seconds against 1.5 and 2.6, and at block 1024 the fastest on `dna`, 522 ns to XCDAT's 567;
`StringIndex` is both the smallest and the fastest on `numeric`. `DictIndex` builds before every
compressed trie again, in 0.8–2.6 s, where the next is MARISA at 3–8.5 times that (PDT, at 1.5
times, on `numeric`), C²'s structures take 10–31 s and PDT up to two minutes. The tries' lead on
shared fragments grows with the keys: MARISA at ρ=2 is 25 % smaller on `titles-en` and 37 % on
`urls`, C²-CoCo 14 % smaller on `dna`, and on `urls` C²-MARISA is smaller and faster than every
block — the one corpus off the front, whole below.

<!-- table: frontier bench/results/frontier-10m-2026-09-15-arz-3d107c4.json corpus=urls-10000000 -->
**`urls-10000000`** — 10,000,000 keys, 52.37 bytes a key raw

| structure | bytes/key | % of raw | build ms | `id` ns | spread | front |
|---|---:|---:|---:|---:|---:|:---:|
| lexindex Dict 32 | 10.75 | 20.5 | 1235 | 1020 | 1 % |  |
| lexindex Dict 256 | 9.38 | 17.9 | 1236 | 966 | 2 % |  |
| lexindex Dict 1024 | 9.23 | 17.6 | 1237 | 936 | 1 % |  |
| lexindex StringIndex | 13.10 | 25.0 | 9330 | 1018 | 3 % |  |
| C²-FST | 9.41 | 18.0 | 12112 | 1558 | 1 % |  |
| C²-FST ρ=1 | 8.20 | 15.7 | 14146 | 1852 | 0 % |  |
| C²-FST ρ=2 | 8.20 | 15.7 | 14662 | 1858 | 1 % |  |
| C²-CoCo | 9.92 | 18.9 | 28766 | 1597 | 3 % |  |
| C²-CoCo ρ=1 | 8.71 | 16.6 | 30740 | 1850 | 0 % |  |
| C²-CoCo ρ=2 | 8.71 | 16.6 | 31267 | 1847 | 0 % |  |
| C²-MARISA | 8.34 | 15.9 | 12049 | 902 | 0 % | ● |
| C²-MARISA ρ=1 | 6.92 | 13.2 | 14359 | 1198 | 1 % | ● |
| C²-MARISA ρ=2 | 6.92 | 13.2 | 15001 | 1199 | 1 % |  |
| FST | 9.54 | 18.2 | 15692 | 1998 | 0 % |  |
| CoCo | — | — | — | — | — | aborted: std::bad_alloc |
| MARISA | 8.66 | 16.5 | 10559 | 1118 | 2 % |  |
| MARISA ρ=1 | 6.23 | 11.9 | 11787 | 1517 | 2 % | ● |
| MARISA ρ=2 | 5.83 | 11.1 | 11960 | 1635 | 1 % | ● |
| PDT | 7.34 | 14.0 | 27578 | 1181 | 2 % | ● |
| ART | 35.80 | 68.4 | 3575 | 883 | 9 % | ref |
| C-ART | 16.85 | 32.2 | 4165 | 914 | 3 % | ref |
| XCDAT 7 | 12.37 | 23.6 | 12639 | 874 | 2 % |  |
| XCDAT 8 | 11.96 | 22.8 | 12276 | 838 | 1 % | ● |
| XCDAT 15 | 14.75 | 28.2 | 12602 | 698 | 2 % | ● |
| XCDAT 16 | 15.43 | 29.5 | 12299 | 753 | 2 % |  |
<!-- /table -->

<sub>From the same artifact
([`bench/results/frontier-10m-2026-09-15-arz-3d107c4.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-09-15-arz-3d107c4.json)).
`spread` is the range of a cell's three rounds over their median, and `ref` marks the two structures
that do not count their keys. C²-MARISA builds the same 6.92-byte structure at ρ=1 and ρ=2, so the
front mark between those two rows is one nanosecond. `bench/frontier/tables.py` prints this table
for every corpus at both scales from the logs.</sub>

Not every structure runs everywhere, and a cell that failed says why instead of carrying a number.
C²'s three structures crash with a segmentation fault on `numeric` at every depth and both scales,
and on `dna` at ten million keys at ρ=2. CoCo-trie stops with `Sequence is not sorted` on the three
title corpora at a million keys, and runs out of the 28 GB of address space each process is allowed
on `uuid` at a million and on every corpus but `numeric` at ten million.

The campaign puts the rest of the frontier beside `marisa-trie` without changing what the size
tables above found: no compressed trie here builds faster than blocks of front-coded keys, and a
recursive trie is smaller wherever long fragments repeat across the key set — paths, URLs, titles —
because a block shares only with its own neighbours. Where C²'s cache-conscious MARISA reads faster
as well, on URLs and on the Russian and Chinese titles, `DictIndex` is off the front.

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

