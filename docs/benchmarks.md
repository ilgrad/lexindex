# Benchmarks

Every number here is a claim about one machine on one day, and every timed table cites the file under
[`bench/results/`](https://github.com/ilgrad/lexindex/tree/main/bench/results) that holds its raw
samples and the environment that produced them, save the Python one in
[Lookup speed from Python](#lookup-speed-from-python-against-dict-and-marisa-trie), measured on 1.1,
which holds its old rows until its recount agrees with itself. The README carries the two headline tables; this
page holds the rest and the protocol behind every number — read the ratios, not the absolutes,
and [what the error bars are](#error-bars-and-where-a-lookup-number-comes-from) before reading
a lookup time closely.

`bench/reproduce.sh` is the other half of that promise: it refuses a tree whose code does not match
the commit the artifact will be named after, pins the competitor versions, verifies every corpus
against its hash, records the machine, and finishes by putting the run's Python call floor beside
the published one — the number no change to this library can move, and therefore the one that says
whether your machine is comparable to the one in the tables at all.

> **This page was recounted on 2026-11-03, at `82f2cb1`, on main after 4.5.3.** One window of 10 h 12
> ran 613 cells on one kernel and one compiler, every CPU on the **`performance`** governor, with no
> swap in use ([`recount-env-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/recount-env-2026-11-03-arz-82f2cb1.txt), [`recount-campaign-2026-11-03-arz-82f2cb1.log`](https://github.com/ilgrad/lexindex/blob/main/bench/results/recount-campaign-2026-11-03-arz-82f2cb1.log)). **The older
> dated rows ran on `powersave`, as every September artifact that names a governor records.** Do not subtract the one from the other: at an unchanged version,
> the competitors' medians read 0.93–0.98× of 2026-09-25 and MARISA's 1.00–1.02×, so a gap of that
> size across the two proves nothing about a library. Each table names its run.
>
> Not re-measured, and still dated: the Python table
> ([why](#lookup-speed-from-python-against-dict-and-marisa-trie)), `positioning`, the A/Bs of old
> builds (`mphf-cool-ab`, `dict-onelevel-ab`, `dict-stair`, `id-ab`), the `uuid` cold figures, the
> `MPH3` figures with huge pages off at 100 M, and the size floor of
> [MARISA at its smallest](#marisa-at-its-smallest). A version claim is never a comparison of
> two rows: it needs both builds in one session, as the
> [`rel-1m-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rel-1m-2026-11-03-arz-82f2cb1.txt) run of 4.5.3 against this commit does.

## How old each section is

Read off this page by `bench/tables.py`, which CI runs, so a section cannot move without this table
moving: the artifacts each section cites, and the oldest and newest of them with the newest release
dated by that day, or main past the last one. A size needs no quiet machine and a time does, so a
section's newest artifact can be younger than its timed columns; each table says which version it
timed. 4.5.3 changed wheels and no lookup, so 4.5.2's numbers stand for it.

<!-- table: freshness docs/benchmarks.md -->
| section | artifacts cited | oldest | newest |
|---|---:|---|---|
| Every competitor, and the version measured | 1 | 2026-10-31 (main after 4.5.3) | 2026-10-31 (main after 4.5.3) |
| Serialised size on real English words | 2 | 2026-11-03 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| Against other Rust string indexes | 2 | 2026-10-31 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| Which one to pick, and how much the corpus decides it | 1 | 2026-09-19 (3.0.0) | 2026-09-19 (3.0.0) |
| Where lexindex loses | 0 | — | — |
| The corpus set | 0 | — | — |
| Every structure over the whole set | 11 | 2026-09-20 (3.0.0) | 2026-11-03 (main after 4.5.3) |
| Lookup speed from Python, against `dict` and `marisa-trie` | 1 | 2026-11-03 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| Point-lookup latency vs the standard library | 2 | 2026-11-03 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| The perfect hash against PtrHash and PHast | 23 | 2026-09-10 (2.0.0) | 2026-11-03 (main after 4.5.3) |
| A fingerprinted `PerfectHashIndex` on mostly-absent keys | 3 | 2026-09-10 (2.0.0) | 2026-11-03 (main after 4.5.3) |
| `ClosedHashIndex`: the perfect hash alone | 1 | 2026-11-03 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| `DictIndex`: the ordered dictionary against `StringIndex` | 4 | 2026-09-11 (2.1.0) | 2026-11-03 (main after 4.5.3) |
| Prefix queries against the tries | 1 | 2026-11-03 (main after 4.5.3) | 2026-11-03 (main after 4.5.3) |
| Common-prefix queries — the other direction | 10 | 2026-09-24 (4.4.2) | 2026-11-03 (main after 4.5.3) |
| Hash quality | 3 | 2026-09-12 (2.1.0) | 2026-09-16 (3.0.0) |
| Error bars, and where a lookup number comes from | 4 | 2026-09-19 (3.0.0) | 2026-11-03 (main after 4.5.3) |
| The research frontier, cited | 0 | — | — |
| The research frontier, measured | 9 | 2026-09-22 (4.1.0) | 2026-11-03 (main after 4.5.3) |
| Scaling to millions of keys | 2 | 2026-09-19 (3.0.0) | 2026-11-03 (main after 4.5.3) |
<!-- /table -->

## Every competitor, and the version measured

Every library this page compares against, at its latest release on 2026-10-31, or at its latest
commit where it has none. Bumping one moved no size on this page: each was rebuilt at the new
version, and every size came out the same to the last digit published. The recount of 2026-11-03
timed them at these versions, and the last column names the exceptions.

| competitor | language | structure | latest | in the timed tables |
|---|---|---|---|---|
| MARISA, [`s-yata/marisa-trie`](https://github.com/s-yata/marisa-trie) | C++ | recursive LOUDS Patricia trie | 0.3.1, `3e87d53`, 2025-07-20 | 0.3.1; the floor table's sizes are 0.2.6's, `e54f296`, the same at 0.3.1 (2026-10-12) |
| `marisa-trie` (PyPI) | Python over C++ | the same trie | 1.4.1, 2026-04-08 | 1.4.1, which bundles C++ marisa 0.2.7, as its main branch does |
| `rsmarisa` | Rust | a port of MARISA | 0.4.2, 2026-06-01 | 0.4.2 |
| XCDAT, [`kampersanda/xcdat`](https://github.com/kampersanda/xcdat) | C++ | compressed double-array trie | 0.2.2, `2451441`, 2024-09-10 | the same |
| C²-FST, C²-CoCo, C²-MARISA, [`alexztc/C2`](https://github.com/alexztc/C2) | C++ | succinct tries with cache-conscious bit vectors and unary-path compression | `92c1b86`, 2026-06-20 | the same |
| FST, CoCo-trie, PDT, ART, C-ART, as C² carries them | C++ | LOUDS-Dense/Sparse trie (SuRF); collapsed-node succinct trie; path-decomposed trie; adaptive radix tree | C² `92c1b86`; CoCo-trie's adapted headers at `ee860d4`, 2026-04-09 | the same |
| sdsl-lite, sux (C++), ds2i, FSST: C²'s libraries | C++ | succinct structures; string compression | `0b861c7` 2026-10-10, `568903f` 2024-10-07, `4f46606` 2021-01-21, `e638d4c` 2026-04-06 | the same |
| `fst` | Rust | minimal acyclic finite-state transducer | 0.4.7, 2021-06-06 | 0.4.7, also `StringIndex`'s own dependency |
| `yada` | Rust | byte-wise double-array trie | 0.7.0, 2026-06-06 | 0.7.0; the word-list sizes were 0.5's, the same at 0.7.0 ([`rust-tries-sizes-2026-10-31-arz.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rust-tries-sizes-2026-10-31-arz.txt)) |
| `crawdad` `Trie`, `MpTrie` | Rust | character-wise double-array trie; minimal-prefix trie | 0.4.1, 2026-08-12 | 0.4.1 |
| `daachorse` | Rust | character-wise double-array Aho–Corasick automaton | 5.1.0, 2026-10-18 | 5.1.0 |
| `cedarwood` | Rust | cedar's reduced double-array trie | 0.6.1, 2026-09-07 | 0.6.1 |
| darts-clone, [`s-yata/darts-clone`](https://github.com/s-yata/darts-clone) | C++ | double-array trie | 0.32 at `87b71af`, 2025-05-11 | the same |
| `dawg2`, `datrie` | Python over C++ / C | DAWG (dawgdic); double-array trie (libdatrie) | 0.13.3, 2025-10-08; 0.8.3, 2025-08-28 | the same |
| jieba, `jieba-rs` | Python, Rust | the segmenter whose lexicon and DAG the Chinese runs use | 0.42.1, 2020-01-20; 0.11.0, 2026-09-16 | the same |
| `ptr_hash` | Rust | PtrHash minimal perfect hash, its Elias–Fano from `sucds` 0.8.3, the major version it requires (0.11.0 is the latest) | 2.1.3, 2026-10-21 | 2.1.3 |
| `ph` | Rust | PHast and PHast+ minimal perfect hashes | 0.11.0, 2026-02-12 | the same |
| `sux` (Rust) | Rust | PHastR minimal perfect hash | 0.15.3, 2026-10-29 | 0.15.3 |

Python's `dict` and Rust's `HashMap` are the interpreter's and the toolchain's, named beside each
table. The research-frontier tables come from `bench/frontier/` here and from the
[string-index-benchmarks](https://github.com/ilgrad/string-index-benchmarks) suite, whose
`pins.sh` files hold the same C++ commits.

## Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

<!-- table: compare bench/results/compare-2026-11-03-arz-82f2cb1.json columns=prefix,common_prefix,range,fuzzy,reverse,exact,mmap -->
| library | prefix | common prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** | **ns/lookup** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | — | none (closed vocabulary) | — | **0.24** | 75 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | — | probabilistic | ✅ | **0.74** | 75 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | — | probabilistic | ✅ | **1.24** | **69** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | — | probabilistic | ✅ | **2.24** | 73 |
| **lexindex `DictIndex` (512 per block)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.52** | 318 |
| **lexindex `DictIndex` (256 per block, default)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.64** | 294 |
| `marisa-trie` (4 tries, tiny cache — its smallest here) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 459 |
| `marisa-trie` (default) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 440 |
| `marisa-trie` (huge cache) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 416 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 181 |
| **lexindex `HashedDictIndex` (fp=8)** | ✅ | ✅ | ✅ | — | ✅ | probabilistic | ✅ | 6.25 | 81 |
| lexindex `PerfectHashIndex` | — | — | — | — | ✅ | ✅ | ✅ | 9.25 | 131 |
| DAWG (`dawg2`) | ✅ | ✅ | — | — | — | ✅ | — | 23.96 | 196 |
| `datrie` | ✅ | ✅ | — | — | — | ✅ | — | 30.91 | 490 |
| builtin `dict` | — | — | — | — | — | ✅ | — | — (in RAM only) | 258 |
<!-- /table -->

<sub>Every cell above is one run of a wheel built from `82f2cb1`, main after 4.5.3, on `performance`,
in the window of 2026-11-03, whose load average went 0.32 into the run and 0.82 out of it
([`bench/results/compare-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-11-03-arz-82f2cb1.json))
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. **Its Python call floor is 43 ns**, against the
51 of the `powersave` run of 2026-09-24 at `3a73ed5` this table carried until now, and the controls
did not move together: the builtin `dict` read 4.6 % slower, `marisa-trie` 2.2–2.5 % faster, DAWG
and `datrie` 15–16 % faster. So read this table against itself. Beside that run the lexindex rows
read faster too, `DictIndex` 9 %, `CompactHashIndex` and `HashedDictIndex` 18–20 %, `StringIndex`
22 %, `PerfectHashIndex` 25 %, with no cause separated from the floor's move; the one row whose bytes
moved is `PerfectHashIndex`, 9.25 a key against 10.88, the merged arena. Another session on this page measured a floor
of 100 and every row fifty nanoseconds higher, two hours apart and within 2 ns of each other; a run
on the same corpus on 2026-09-22, with an editor session on the machine, measured 61 and read
15–20 % slower on every row including the builtin `dict`. That is the reproducibility problem this
page documents further down and the reason `bench/reproduce.sh` prints the floor beside the published one.
`marisa-trie` appears three times because it is a curve: its own documentation says the right
configuration depends on the data, so the table carries its compact end, its default and its fast
end rather than one point somebody could call untuned.</sub>

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
that does nothing — is **43 ns** in this run, which every row pays and no *difference* between
rows contains.
And `.get` on a miss is each library's own miss path, not `try: t[key] except KeyError: default`:
timed apart, a miss costs *less* than a hit everywhere (0.68–0.86×), while that wrapper written out
by hand over marisa's `__getitem__` costs 1.66× a hit
([`bench/results/lookup-fairness-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/lookup-fairness-2026-11-03-arz-82f2cb1.txt)).
Half the probes are misses, so a library that paid for exceptions would have been charged for CPython
rather than for its own structure.

**The smallest rows are also the fastest, which is not a paradox.** `ClosedHashIndex` and the three
`CompactHashIndex` widths answer in 69–75 ns against a builtin `dict`'s 258, because they store no
keys at all: one hash, one probe, at most a fingerprint to compare. What they cannot do is tell a
stranger from a member with certainty, or give a key back for an id. Among the structures that do
keep their keys, `DictIndex` is **both smaller and faster than every `marisa-trie` setting measured
at either block** — 2.52 B/key and 318 ns at 512, 2.64 and 294 at the default 256, against
2.96–3.07 and 416–459 — and the fastest is `HashedDictIndex`, that default-block dictionary with a
hash beside it: 81 ns at 6.25 B/key, under a third of the builtin `dict`'s time with every query the
dictionary answers still on offer, though its `id` tells a stranger from a member only to within
`2^-8`.

Two honest crowns, both scoped to what is measured above — libraries a Python or Rust project can
actually install. The research-grade C++ frontier (MARISA aside, which is installable) has no
binding and is not in these claims: it is listed with its papers and licences under
[the research frontier, cited](#the-research-frontier-cited) and measured on its own protocol under
[the research frontier, measured](#the-research-frontier-measured). **`CompactHashIndex` is the smallest `string → dense id` map here that
can reject a non-member — 2.4× below `marisa-trie` at the default 8-bit fingerprint, 3.9× at 4
bits** — when you can accept a bounded
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

| Rust structure | bytes/key | vs C++ marisa | `id`, half strangers | build |
|---|---:|---:|---:|---:|
| **lexindex `DictIndex`** (512 per block) | **2.519** | 0.85× | 270 ns | **27 ms** |
| **lexindex `DictIndex`** (256 per block, default) | **2.635** | 0.88× | 245 ns | 28 ms |
| **lexindex `DictIndex`** (64 per block) | **2.721** | 0.91× | **216 ns** | 29 ms |
| `marisa-trie` (C++ reference, default) | 2.978 | 1.00× | — | — |
| `rsmarisa` (4 tries, tiny cache — its smallest) | 3.003 | 1.01× | 323 ns | 139 ms |
| `rsmarisa` 0.4.2 (default) | 3.168 | 1.06× | 300 ns | 147 ms |
| `rsmarisa` (3 tries, huge cache — its fastest) | 3.823 | 1.28× | 281 ns | 168 ms |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.63× | — | — |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | 5.95 | 2.00× | — | — |
| `yada` (double-array) | 15.98 | 5.4× | — | — |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× | — | — |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× | — | — |

<sub>`rsmarisa` and `DictIndex` from
[`bench/results/rsmarisa-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-11-03-arz-82f2cb1.txt)
— one process: 20 000 probes, half members and half strangers spelled in the corpus's own
alphabet, shuffled with a fixed seed; five rounds with the six lanes alternating and reversing on
odd rounds, the first discarded and the minimum of the other four kept. Three such processes, on
`performance`, the table quoting the minimum of the three. The other rows are sizes from the older
sweep with `crawdad` 0.4, `yada` 0.5 and `fst` 0.4, the same to the hundredth at 0.4.1, 0.7.0 and
0.4.7
([`rust-tries-sizes-2026-10-31-arz.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rust-tries-sizes-2026-10-31-arz.txt));
size is serialised bytes ÷ keys throughout, and `rsmarisa`'s `io_size()` is asserted equal to
the file it saves. `rsmarisa` is the control across sessions: its lanes read 1–2 % over the `powersave`
run of 2026-09-23, and `DictIndex`'s 17–18 % under it; the search changes of 4.3.3 and 4.4 lie
between the two runs, and two sessions cannot separate them from the governor. In the session before that a further
process, run seconds after a `git commit`, read 8–20 % slower on **every** row including
`rsmarisa`'s and was discarded on that evidence rather than on its shape — which is the whole reason
this page insists on a control that the change under test cannot touch. None of these crates is a
lexindex dependency: the harness is a throwaway.</sub>

So the statement is a crown after all, with the frontier behind it. **`DictIndex` at its default
block dominates `rsmarisa` at its most compact setting outright** — 2.635 against 3.003 B/key, 245
against 323 ns, 5.0× faster to build — and at 64 per block it is the fastest structure here at 216
ns for 2.721, under `rsmarisa`'s *smallest* setting on both axes at once. Since `BDX3` every block
from 64 to 512 is smaller than every `rsmarisa` setting, so the block size now picks only the
latency axis, not the size one.

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
(`local/positioning.py`, 2026-09-19 at `999e933`,
[`positioning-2026-09-19-arz-999e933.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/positioning-2026-09-19-arz-999e933.txt);
the bare-MPHF row is a `ClosedHashIndex` built over the same three key sets in the same session;
sizes only, not re-measured on 2026-11-03, and the `PerfectHashIndex` row predates its overlapped
arena, which holds single words in 9.25):

| bytes/key | 479 823 single words | 1 M `word.word` pairs, drawn at random | 1 M `word.word`, 1 000 × 1 000 grid |
|---|---:|---:|---:|
| the bare MPHF (no keys, no membership, no reverse) | 0.24 | 0.24 | 0.24 |
| **lexindex `CompactHashIndex`** (fp = 1 byte) | **1.24** | **1.24** | **1.24** |
| **lexindex `DictIndex`** (256 per block) | **2.65** | 6.84 | **1.95** |
| `marisa-trie` | 2.98 | 6.21 | 2.12 |
| **lexindex `StringIndex`** | 5.95 | 15.19 | 0.68 |
| lexindex `PerfectHashIndex` | 10.88 | 21.91 | 12.49 |

The `DictIndex` row swaps places with the `marisa-trie` one inside a single table — under it on
single words and on the grid, 1.10× over it on random pairs — which is the whole argument of this
page in three columns: the two are different shapes, and which is smaller is a property of the keys.
The grid is where `BDX3` changed the answer: at 2.50 bytes a key it sat 1.18× over the trie, at 1.95
it is 8 % under. At 10 M the trie numbers move again — `marisa` 4.14 on random pairs against 2.36 on
the grid, `StringIndex` 12.44 against 2.00, `DictIndex` 5.51 against 2.58 — while `CompactHashIndex`
stays at 1.24 and the bare MPHF at 0.24,
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
  *larger* than a bare MPHF (1.24 against 0.24), which is exactly the fingerprint that buys the
  membership check.
- **Do the keys share a lot of structure** (a path namespace, a versioned catalogue, a cross product)?
  Then measure before choosing: that is the regime where an FST can beat a keyless hash outright.
- **A `dict` / `HashMap` is not in the table** because it has no serialised form to measure. It cost
  71–95 bytes per key above the key list itself across these corpora (58–60 at 10 M, where the table
  amortises better), and it has to be rebuilt from the keys on every process start; every structure
  here loads from a file instead, and every one but `ClosedHashIndex` by mapping it.

## Where lexindex loses

Every loss measured so far, in one place; each line links to the section that measured it.

- **Exact `id` against XCDAT 15.** At a million keys lexindex's fastest exact search over sorted ids
  is behind on eleven corpora of thirteen: by 5–13 % on `urls`, `uuid`, `paths`, `words` and
  `titles-ru`, by 1.19–1.45× on the other six. `words` is a single cold pass and not traced
  ([measured](#the-research-frontier-measured)). At ten million keys `opaque` is 1.36× behind, 436 ns
  against 320, and `uuid` level. `HashedDictIndex` is 3.7–9.1× faster than XCDAT in a free loop and
  1.7–3.8× when each lookup waits on the last, through `id_unchecked`, which takes a key's membership
  on trust, so it is not an exact search ([against XCDAT](#hasheddictindex-against-xcdat)).
- **Size on `paths` against MARISA at its smallest.** 8.81 bytes a key at 23 tries under the tiny
  cache, against `DictIndex` block 1024's 9.32: MARISA is 5.8 % smaller by file and 7.9 % once both
  are mapped. At lexindex 4.5.2, at ten million keys and at 19.2 million, mapped MARISA was 0.4–1.8 %
  smaller on `titles-en` and `urls`, where `DictIndex` is the smaller by file
  ([MARISA at its smallest](#marisa-at-its-smallest)).
- **Bits against PHastR built on one thread.** `MPH3` takes 1.925 / 1.915 / 1.911 bits a key at
  1 M / 10 M / 100 M, PHastR 1.919 / 1.904 / 1.902 at bucket size 4.5 and 1.850 / 1.823 / 1.819
  with 10-bit seeds ([the perfect hash](#the-perfect-hash-against-ptrhash-and-phast)).
- **The eight-thread build at 1 M against PHastR.** `MPH3` reads 11.8 ns a key against PHastR's 10.3,
  at the higher bits above ([the perfect hash](#the-perfect-hash-against-ptrhash-and-phast)).
- **The eight-thread batch at a billion keys.** `MPH3`'s batch is 19 % behind PtrHash compact on
  the same memory: it fetches 0.98 lines a key from DRAM against compact's 0.73
  ([the perfect hash](#the-perfect-hash-against-ptrhash-and-phast)).
- **The double array's build.** 0.157 s on the jieba dictionary, 2.9× darts-clone's time and 1.6×
  cedar's ([on Chinese running text](#on-chinese-running-text-from-rust-and-from-python)).

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
| `dna` | 24.0 | 0.24 | 1.24 | 4.45 | 4.37 | **4.23** | 17.74 | 7.52 | 7.52 | 7.52 | 7.56 | 7.71 |
| `domains` | 13.8 | 0.24 | 1.24 | 4.54 | 4.50 | **4.36** | 10.50 | 4.81 | 4.80 | 4.80 | 4.87 | 4.99 |
| `idents` | 17.3 | 0.24 | 1.24 | 5.29 | 5.23 | **5.07** | 10.55 | 5.45 | 5.34 | 5.30 | 5.62 | 5.74 |
| `numeric` | 5.9 | 0.24 | 1.24 | 1.04 | 1.01 | **0.92** | 0.00 | 1.62 | 1.62 | 1.62 | 1.64 | 1.72 |
| `opaque` | 16.0 | 0.24 | 1.24 | 10.43 | 10.40 | **10.29** | 21.44 | 18.23 | 15.55 | 15.55 | 18.82 | 19.04 |
| `paths` | 125.0 | 0.24 | 1.24 | 10.32 | 9.87 | 9.32 | 17.48 | 9.26 | 9.04 | **8.83** | 9.47 | 9.60 |
| `titles-en` | 21.0 | 0.24 | 1.24 | 7.44 | 7.37 | **7.21** | 17.28 | 7.79 | 7.52 | 7.49 | 8.19 | 8.37 |
| `titles-ru` | 35.8 | 0.24 | 1.24 | 6.92 | 6.86 | **6.70** | 31.75 | 8.07 | 7.67 | 7.61 | 8.61 | 8.77 |
| `titles-zh` | 16.9 | 0.24 | 1.24 | 5.89 | 5.85 | **5.69** | 17.52 | 6.33 | 6.22 | 6.22 | 6.54 | 6.70 |
| `urls` | 52.4 | 0.24 | 1.24 | 7.70 | 7.51 | **7.27** | 16.88 | 7.95 | 7.61 | 7.57 | 8.39 | 8.58 |
| `uuid` | 36.0 | 0.24 | 1.24 | 18.11 | 18.04 | **17.89** | 37.11 | 32.93 | 22.99 | 22.98 | 34.58 | 34.80 |

<sub>The bold cell in a row is the smallest structure that keeps its keys. `words`, `pypi` and
`jieba-dict` have no million-key file; their 100 000 grid and the 100 000 rows for the rest are in
[`bench/results/sweep-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-11-03-arz-82f2cb1.json)
— one run of the whole set at `82f2cb1`, 275 cells, and every column above comes from it, the three
`DictIndex` blocks included, so this table needs no reconciliation between harnesses. All 275 read
the same to the fourth decimal as the 4.3.0 run before it
([`sweep-2026-09-23-arz-4f2a0e9.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-23-arz-4f2a0e9.json)). Of the 264 cells that
run shares with the 4.0.0 one
([`sweep-2026-09-20-arz-8a8778a.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-20-arz-8a8778a.json)),
252 read the same to the byte; the twelve that moved are the `DictIndex` cells of `titles-ru` and
`titles-zh`.</sub>

**`DictIndex` is the smallest key-storing structure on ten of these eleven corpora**, measured
against marisa's *best* setting on each and not its default. The margin runs from 3.9 % on
`titles-en` (7.21 against 7.49) and 4–5 % on `urls` and `idents`, through 9 % on `titles-zh`, 10 %
on `domains` and 14 % on `titles-ru`, to 28 % on `uuid`, 51 % on `opaque`, 77 % on `numeric` and
78 % on `dna`. The eleventh is
`paths`, where a million paths run through a few thousand directories and marisa at sixteen tries
holds 8.83 against 9.32 — 5.3 % — which is what a LOUDS trie is for and what front coding in fixed
blocks still does not answer. Through 3.0.0 this table read the other way round, marisa smallest on
nine of the eleven; the `BDX3` codec is what turned it, and 4.0.0's phrase work moved all 72
`DictIndex` cells of the sweep again — 71 of them down, `domains` by 4.9 % and `paths` by 3.0 %,
with `titles-zh` at 1024 keys a block the one cell that grew, by 0.05 %. 4.3.0's character code
moved twelve and nothing else: `titles-ru` 10.1–11.1 % smaller at a million keys and 13.8–14.7 % at
100 000, `titles-zh` 6.7–7.0 % and 9.2–9.3 %, which took their margins from 2.2 % and 1.6 % to 14 %
and 9 %. A code can only pay where two bytes in fifteen or more are outside ASCII, so the Latin
corpora keep their blobs byte for byte.

**Its floor is eight or sixteen tries on nine of the eleven, not the four this page quoted through
3.0.0.** The recursion keeps paying wherever the keys share — `titles-ru` 8.07 → 7.61, `urls`
7.95 → 7.57, `paths` 9.26 → 8.83, about 5 % each — and it pays most where they share least: `opaque`
18.23 → 15.55 and `uuid` 32.93 → 22.98, 15 % and **30 %** of the blob. The two corpora where four
tries is not beaten are `dna` and `numeric`, where all three settings agree to the hundredth; on
`domains` the gain is 0.3 % and on `titles-zh` 1.8 %, so "tune it" is worth measuring and not worth
assuming. What the tries cost is time, and the cost is where the gain is: `uuid` builds 1.36× and
answers 1.62× slower at sixteen tries than at four, `paths` answers 1.35× slower, and on the corpora
where the setting buys nothing the two are level. Marisa's smallest configuration is its slowest.

**The margin is still widest where the keys share least.** `DictIndex` leads by half a blob on
`opaque` (10.29 against 15.55) and by more than that on `dna` and `numeric` — keys with nothing to
share, where a trie pays for a node per character and front coding pays for a prefix that is not
there. Where the keys do share, the lead is a couple of per cent and a tuned marisa is the thing to
measure against: on `urls` and `titles-en` the two are within 5 % of each other, and `paths` is
still marisa's. On `words`, the corpus every table above is measured on, `DictIndex` at
its default block is 3.33 against marisa's 3.70 at 100 000 keys and 2.64 against 2.96 on the full
479 823 — and there four tries really is marisa's floor, eight and sixteen reading 3.71 and 3.74.
Read the spread between `dna` and `paths` as the honest range: which is smaller is a property of the
keys, and the corpus set is there so that neither end can be quoted alone.

**The two keyless rows are flat and every other row is not.** `ClosedHashIndex` is 0.24 bytes a key
and `CompactHashIndex` 1.24 on all eleven corpora at all three sizes, because their size is a
function of `n` and the fingerprint width and of nothing about the keys. Against the best trie that
is a 7.1× margin on `paths` and 1.3× on `numeric` — the same two structures, neither of them
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
| `dna` | 24.0 | 61 | 112 | 370 | 358 | 364 | 520 | 944 | 972 | 952 | 939 | 899 |
| `domains` | 13.8 | 60 | 104 | 407 | 402 | 451 | 355 | 679 | 711 | 703 | 675 | 651 |
| `idents` | 17.3 | 62 | 117 | 447 | 458 | 506 | 402 | 939 | 985 | 1014 | 895 | 834 |
| `numeric` | 5.9 | 49 | 71 | 296 | 297 | 312 | 91 | 270 | 262 | 259 | 258 | 234 |
| `opaque` | 16.0 | 77 | 105 | 356 | 345 | 357 | 377 | 1341 | 1405 | 1377 | 1135 | 1087 |
| `paths` | 125.0 | 82 | 132 | 730 | 723 | 753 | 869 | 2800 | 3508 | 3784 | 2360 | 2074 |
| `titles-en` | 21.0 | 72 | 106 | 465 | 458 | 495 | 482 | 1098 | 1109 | 1232 | 1088 | 1049 |
| `titles-ru` | 35.8 | 80 | 125 | 525 | 521 | 549 | 645 | 1477 | 1565 | 1580 | 1409 | 1359 |
| `titles-zh` | 16.9 | 65 | 106 | 459 | 464 | 511 | 417 | 929 | 940 | 946 | 935 | 940 |
| `urls` | 52.4 | 69 | 108 | 468 | 470 | 503 | 543 | 1162 | 1314 | 1453 | 1145 | 1123 |
| `uuid` | 36.0 | 83 | 102 | 420 | 398 | 442 | 460 | 1562 | 2535 | 2537 | 1272 | 1228 |

**`DictIndex` answers faster than every `marisa-trie` setting on ten of the eleven corpora** —
1.6–3.2× at its default block, against marisa's *fastest* setting on each and not its smallest. The
eleventh is `numeric`, where marisa's fastest is 21 % ahead of the default block (234 ns against
297) and `StringIndex` ahead of both at 91: a million dense decimal ids are the corpus where a trie
has the least to walk. `DictIndex` also builds 2.3–6.8× faster than marisa's quickest build on all
eleven, `numeric` included, where marisa was 1.2× ahead through the 2026-09-19 run. So the size
table above is not the whole trade, and the settings that make marisa smallest are the ones that
make it slowest: on `paths`, the one corpus it still wins on size, its sixteen-try floor is 1.06×
smaller than `DictIndex` at 1024 a block, 5.0× slower to answer and 3.8× slower to build.

**The block is a smaller knob than it was.** A lookup scans one restart a microblock and then one
microblock whatever the block, so 128 → 1024 keys a block moves a lookup by −2 % (`dna`) to +13 %
(`idents`), where the one-level format paid 3.8× from 32 to 1024, and buys 0.12–1.00 bytes a key
over 128, every corpus in the same direction. It did not use to be: `paths` at 1024 keys a block
stored *more* than at 256 (16.13 against 15.89), and the cause was the symbol table rather than the
layout, whose per-block arrays only shrink as the block grows. The table trained on whole blocks, so
a larger block bought fewer neighbourhoods — 19 at 1024 against 157 at 128 — and a table trained on
19 of them is a lottery. A training run is now a constant 256 keys whatever the block, trained per
shard of 65 536 keys, and `paths` at 1024 reads 9.32. 256 is the default the README quotes.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`, from
[`sweep-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-11-03-arz-82f2cb1.json), whose load average went 0.34 into the run and 1.03 out of it. Lookups are
one run of three rounds of 20 000 probes a cell, so a column is comparable within itself and a cell
carries a few per cent: in September a second run of one build read the same bytes to the last
digit and lookups 1–15 % higher on every structure, controls included.</sub>

### Ten million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa 4 | marisa 8 | marisa 16 | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.24 | 1.24 | 4.03 | 3.95 | **3.81** | 15.98 | 6.69 | 6.69 | 6.69 | 6.75 | 6.99 |
| `numeric` | 6.9 | 0.24 | 1.24 | 1.04 | 1.02 | **0.92** | 0.00 | 1.63 | 1.63 | 1.63 | 1.66 | 1.77 |
| `opaque` | 16.0 | 0.24 | 1.24 | 10.08 | 10.05 | **9.95** | 21.47 | 15.19 | 14.72 | 14.72 | 18.33 | 18.68 |
| `titles-en` | 21.0 | 0.24 | 1.24 | 5.61 | 5.54 | **5.37** | 13.25 | 5.57 | 5.49 | 5.48 | 5.71 | 5.87 |
| `urls` | 52.4 | 0.24 | 1.24 | 5.88 | 5.70 | **5.44** | 13.10 | 5.66 | 5.56 | 5.55 | 5.83 | 5.99 |
| `uuid` | 36.0 | 0.24 | 1.24 | 17.63 | 17.56 | **17.42** | 36.07 | 29.97 | 20.62 | 20.62 | 33.21 | 33.56 |

Ten times the keys moves every trie and neither hash: at its floor `marisa` goes 7.49 → 5.48 on
`titles-en` and 7.57 → 5.55 on `urls` as the sharing deepens, `DictIndex` at its default block
7.37 → 5.54 and 7.51 → 5.70, and the two keyless rows do not move at all. Through 3.0.0 this was
where the ranking turned over: `titles-en` and `urls` are two of the ten corpora `DictIndex` leads
at a million, and at ten million the trie took both back, by 2.8 % and 0.7 %. It no longer does. At
1024 keys a block `titles-en` reads **5.375 against 5.484** and `urls` **5.436 against 5.548**, a
2.0 % lead on each, where the 4.3.0 run read 0.11 % and 0.33 %: since then a build past about 2.6
million keys mines its phrases from more of its sample, 1.5–1.9 % smaller on these two corpora and
the same bytes on the other four. At the default block of 256 the trie's floor still holds both, 5.54 against 5.48 and
5.70 against 5.55. Read each lead as what a build setting can move, not as a comfortable win.

**Scale narrows every gap in the trie's favour.** `DictIndex` is smallest on all six at ten
million, four of them by a distance — `dna` 3.81 against marisa's tuned 6.69 and `numeric` 0.92
against 1.63, both 43 %, `opaque` 9.95 against 14.72 and `uuid` 17.42 against 20.62, 32 % and
16 % — but every one of those four margins is narrower than the same corpus gives at a million,
`uuid` most of all, 22 % there against 16 % here. The reason is the trie's, not ours: ten times the random identifiers share ten times more
three- and four-character fragments, and a recursive trie is built to find exactly that, while a
block of front-coded keys shares only with its own block. Read the margins as gaps that narrow with
n, and do not build a claim on a single one of them.

**The block buys less here.** 128 → 1024 keys a block is 0.12–0.45 bytes a key over these six,
against 0.12–1.00 at a million. Ten times the keys is ten times the blocks, so the per-block arrays
a bigger block saves were already a smaller share of the index; what is left is the coded suffixes,
and those are the symbol table's business, not the block's — which is why the table is trained on
runs of a constant length and per shard. With whole-block training over one table a bigger block
bought fewer neighbourhoods to train on, and `titles-en` at 1024 came out above its own 256.

<sub>Measured 2026-11-03 at `82f2cb1`
([`bench/results/sweep10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-11-03-arz-82f2cb1.json)),
`marisa-trie` 1.4.1, one build per cell at this size and three rounds of 20 000 lookups. **Sizes
only**: this table has no timed column. Every marisa cell reads to the hundredth what the runs of
2026-09-13, 09-19 and 09-23 read: a size is exact, so runs agree wherever the builder did not move.
Against the 4.3.0 run
([`sweep10m-2026-09-23-arz-4f2a0e9.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-09-23-arz-4f2a0e9.json)),
60 of the 66 cells read the same to the byte; the six that moved are the three `DictIndex` blocks on
`titles-en` and `urls`.</sub>

### A cold mapping, and what is actually resident

Every size in this page is the blob. A process that maps one and answers a thousand queries never
reads most of it, and what decides whether an index fits inside a container limit is the resident
set. `local/coldmmap` drops each file's page cache with `posix_fadvise(POSIX_FADV_DONTNEED)` — no
root needed, and the resident set straight after `load_mmap` is the control that the drop worked —
then reads `/proc/self/smaps` after 1 000 random lookups and after a further million.

Ten million English Wikipedia titles, bytes per key except the latencies:

| structure | file | mapped | after 1 k | after 1 M | cold ns | warm ns |
|---|---:|---:|---:|---:|---:|---:|
| `DictIndex` 32 | 6.02 | 0.81 | 4.57 | 6.02 | 27 102 | 584 |
| `DictIndex` 32, `MADV_RANDOM` | 6.02 | 0.81 | **1.81** | 6.02 | 62 303 | 1 282 |
| `DictIndex` 256 | 5.54 | 0.18 | 4.07 | 5.54 | 28 486 | 557 |
| `DictIndex` 256, `MADV_RANDOM` | 5.54 | 0.18 | **1.06** | 5.54 | 74 885 | 1 303 |
| `DictIndex` 1024 | 5.37 | 0.12 | 3.96 | 5.38 | 27 628 | 575 |
| `DictIndex` 1024, `MADV_RANDOM` | 5.37 | 0.12 | **1.16** | 5.38 | 100 435 | 1 322 |
| `CompactHashIndex` | 1.24 | 0.24 | 1.24 | 1.24 | **5 817** | **29** |
| `CompactHashIndex`, `MADV_RANDOM` | 1.24 | 0.24 | 0.70 | 1.24 | 49 380 | 125 |
| `StringIndex` | 13.25 | 0.01 | 10.10 | 13.25 | 86 047 | 662 |
| `StringIndex`, `MADV_RANDOM` | 13.25 | 0.01 | 2.27 | 13.25 | 358 266 | 2 649 |

**`load_mmap` really is lazy**, which the `mapped` column exists to prove: 0.01 to 0.81 bytes a key
resident before the first query, which is the header and the little the loader validates. Nothing
else is read until something asks for it. The figure rises as the block shrinks because a smaller
block means more per-block arrays, and `BDX3` validates their framing at load.

**The first thousand queries cost far more pages than they need.** `DictIndex` at 256 ends them with
4.07 of its 5.54 bytes a key resident — 73 % of an index nobody has finished reading — while the
same thousand queries under `MADV_RANDOM` leave **1.06**, which is what they actually touch: about
two pages a lookup, the sample array and the block. The 3.8× between those two numbers is the
kernel's readahead, and it is buying latency with memory: turning it off costs 2.6× on the cold
lookups and **2.3× on the warm ones**, because the advice outlives the warm-up. The warm column is
also where the microblock shows on ten million keys: 1024 a block answers in 575 ns where the
one-level format took 2 108. Readahead is the right default here; `MADV_RANDOM` is for the case where
a container limit, and not a latency budget, is what binds.

**Cold start is where the smallest structure wins outright, and the mechanism is pages.**
`CompactHashIndex` answers its first thousand queries at **5.8 µs** against `DictIndex`'s 28.5 and
`StringIndex`'s 86.0 — 4.9× and 15× — because its whole file is 12.4 MB and a fault brings in a
useful fraction of it. On `uuid`, not re-run in the recount, where its 1.24 bytes a key sit against
`DictIndex`'s 17.56 and `StringIndex`'s 36.07, the `powersave` run of 2026-09-22 read 19× and 46×
(5.2 µs against 99.9 and 240.4). A structure that stores no keys has no keys to fault in.

**After a million queries every structure is fully resident**, to within a hundredth of a byte. The
distinctive answer to "how much memory does this index need" only exists during warm-up: past it,
against a workload that touches every key, the resident set *is* the file, and the size table above
is the steady-state RSS.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`, NVMe under LUKS on btrfs, 38 GB RAM — so
"cold" means this file's page cache was dropped and not that the machine was short of memory
([`bench/results/coldmmap-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-11-03-arz-82f2cb1.txt); the `uuid` run is in
[`coldmmap-2026-09-22-arz-41478d0.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/coldmmap-2026-09-22-arz-41478d0.txt)).
`StringIndex` is the control: its resident columns are identical to the 2026-09-22 run's to the
hundredth (0.01 mapped, 10.10 after a thousand, 13.25 after a million), and its latencies read 2 %
quicker cold and 21 % warm; this run cannot separate the governor from later work on its reader.
`DictIndex` moved its bytes for a reason: at 256, 5.65 → 5.54 on disk
and 4.17 → 4.07 resident after a thousand queries. `CompactHashIndex`'s cold column read 9 %
slower, 5 336 → 5 817 ns, and its warm one 28 → 29, with no change in its lookup, so a cold cell
carries about that much. `MADV_RANDOM` is applied by the harness through `/proc/self/maps`;
`load_mmap` does not set it, and on this evidence should not.</sub>

### One mapping, many readers

Every index here is immutable once built. A reader needs no lock, no copy and no per-thread
instance: one `load_mmap` is shared by reference and answers from as many threads as there are
cores. `local/readscale` is whether that is near-linear — 8 Mi random member probes split evenly,
the mapping warmed first so this measures the structure and not page faults, the ladder run three
times and the best kept per thread count.

Ten million English Wikipedia titles, nanoseconds a lookup with the speedup over one thread:

| threads | `DictIndex` 128 | `CompactHashIndex` | `StringIndex` |
|---|---:|---:|---:|
| 1 | 555 | 16.1 | 610 |
| 2 | 282 (1.97×) | 9.1 (1.77×) | 300 (2.03×) |
| 4 | 143 (3.88×) | 5.7 (2.82×) | 154 (3.97×) |
| 8 | 73 (7.64×) | 3.2 (5.03×) | 81 (7.53×) |
| 16 | 44 (12.62×) | 2.6 (6.19×) | 46 (13.31×) |
| **M lookups/s at 16** | **22.7** | **379** | **21.8** |

**Eight cores give 5.0–7.6×**, 63–96 % of them, on a mobile part whose clock falls as cores light
up — the number already carries that, so a machine with a flatter boost curve can only do better.

**The sixteen-thread row is SMT and it is worth having.** 12.6× and 13.3× over one thread for the
two structures that answer out of DRAM, well past the eight physical cores, because a point lookup
is a chain of dependent loads and a core spends most of it waiting. A second thread on the same core
fills those stalls with someone else's work.

**`StringIndex` and `DictIndex` scale alike** — 2.03× and 1.97× on two threads, 13.31× and 12.62×
on sixteen. Both walks are dependent misses, and two cores keep twice as many of them in flight as
one. At a million keys, where the 17 MB transducer and the 7.4 MB dictionary sit mostly in cache,
there is less to overlap: 1.93× and 1.89× on two, 12.70× and 10.79× on sixteen.

**`CompactHashIndex` saturates first**, at 6.19×, and it is the one structure that has run out of
something other than cores: 379 M lookups a second over a 12.4 MB index that fits this machine's
16 MB L3, at 2.6 ns a lookup. The others are answering out of DRAM and have latency left to overlap;
this one does not. It saturates **earlier** than it did on 3.0.0 — 9.43× then, 6.19× now — for the
good reason that its single-thread lane got over 50 % quicker while its sixteen-thread lane, already
bandwidth-bound, moved 3.7 → 2.6 ns.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/readscale-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/readscale-2026-11-03-arz-82f2cb1.txt),
which carries the million-key ladder as well), Ryzen 7 5800HS, 8 cores and 16 hardware threads.
Shared by `&index` across `std::thread::scope`; no index is cloned and none is rebuilt per thread.
Three processes and the minimum per cell; `CompactHashIndex` at ten million is the column that
needed them, reading 27.3 / 16.8 / 16.1 ns on one thread and 3.4 / 2.6 / 2.6 on sixteen across the
three, where `StringIndex` spread 7 % on one thread and every other cell at most 3 %. The `powersave` run of 2026-09-23
([`readscale-2026-09-23-arz-fdc28bc.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/readscale-2026-09-23-arz-fdc28bc.txt)) read
`DictIndex` 128 at 605 ns over 5.71 bytes a key, `StringIndex` at 803 and `CompactHashIndex` at 15.9
on one thread.</sub>

### `rsmarisa`, the pure-Rust port

The same question in Rust, where `marisa-trie` is a C++ library with no binding a Rust project would
take. `rsmarisa` 0.4.2 is a pure-Rust port of it, BSD-2-Clause, and `local/rsmarisacmp` puts its
three cache levels against three `DictIndex` block sizes, 64, 256 and 512, on every corpus:

| corpus | rsmarisa smallest | `DictIndex` 512 | rsmarisa build | `DictIndex` build | rsmarisa fastest | `DictIndex` fastest (block) |
|---|---:|---:|---:|---:|---:|---:|
| `dna` | 7.61 | **4.25** | 1 305 ms | 70 ms | 611 ns | 233 ns (64) |
| `domains` | 4.87 | **4.37** | 698 ms | 164 ms | 459 ns | 269 ns (64) |
| `idents` | 5.50 | **5.08** | 649 ms | 190 ms | 610 ns | 312 ns (64) |
| `numeric` | 1.64 | **0.92** | 102 ms | 46 ms | **168 ns** | 256 ns (256) |
| `opaque` | 18.27 | **10.29** | 1 563 ms | 90 ms | 998 ns | 243 ns (256) |
| `paths` | 10.07 | **9.43** | 5 607 ms | 397 ms | 1 656 ns | 566 ns (256) |
| `titles-en` | 7.85 | **7.22** | 1 028 ms | 328 ms | 801 ns | 329 ns (256) |
| `titles-ru` | 8.22 | **6.71** | 1 509 ms | 281 ms | 1 069 ns | 382 ns (256) |
| `titles-zh` | 6.40 | **5.69** | 800 ms | 208 ms | 664 ns | 324 ns (64) |
| `urls` | 8.38 | **7.31** | 2 105 ms | 339 ms | 835 ns | 383 ns (256) |
| `uuid` | 33.01 | **17.91** | 2 101 ms | 149 ms | 1 121 ns | 284 ns (512) |

`BDX3` changed the shape of this table. Through 3.0.0 `rsmarisa` was the smaller structure on
**nine** of these eleven corpora — everything but `opaque` and `uuid`, the two whose keys share
nothing for a trie to fold. It is now **larger on all eleven**: 6.8 % on `paths` and 8 % on `idents`
at the narrow end, and 77–84 % on `dna`, `opaque`, decimal ids and UUIDs at the wide
one, while `DictIndex` builds
**2.2–19× faster** and answers **1.7–4.1× faster** on ten of the eleven. `numeric` is the one
exception left, and only on latency: `rsmarisa` answers it in 168 ns against 256, having lost the
size there (1.64 against 0.92) and the build (102 ms against 46). The right-hand column has no single
answer for the block: the fastest is 64 on four corpora, 256 on six and 512 on one, and on six the
three are within 10 % of each other. The ends are what it teaches. Short keys — `domains`,
`idents` — are 14–18 % slower at 512 than at 64, where the scan is what a lookup waits on; `uuid`,
whose 18 MB blob is the one larger than the L3, is 30 % slower at 64 than at 512, where fewer
blocks are fewer dependent misses in the search over their heads. At
nominally the same configuration `rsmarisa` is larger than the C++ marisa measured above — on
`words`, 1.6 % at the smallest setting, 6.4 % at the default and 24 % at the fastest — so the flag
words evidently do not mean quite the same thing, and each library's own curve is what to read.
**"Smallest" here is the smallest of its three cache levels at the default number of tries, not its
floor**: the sweep above shows the C++ library bottoming out at eight or sixteen tries on nine of
these corpora, by 30 % on `uuid`, and this harness does not turn that knob. Read the left column as
one point on a curve whose other end is not measured.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/rsmarisa-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-11-03-arz-82f2cb1.txt)),
`rsmarisa` 0.4.2, in one process per corpus, six lanes alternating within each of five rounds, on
corpora checked against the manifest's hashes. `rsmarisa`'s lanes, whose code has not changed, read
−10…+7 % against the `powersave` run of 2026-09-23
([`rsmarisa-corpora-2026-09-23-arz-fdc28bc.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-corpora-2026-09-23-arz-fdc28bc.txt)),
and `DictIndex`'s −1…−24 %. It is
a throwaway crate under `local/`: `rsmarisa` is not a lexindex dependency and is not proposed as
one. The Rust build times are not comparable with the Python ones above — that harness copies every
key across the language boundary and this one does not.</sub>

## Lookup speed from Python, against `dict` and `marisa-trie`

`bench/latency_py.py` — one process per corpus, every structure built up front, the seven lookup
forms rotated inside each round so none keeps the position that pays to warm the probe list.
**Measured on 1.1, and not replaced by the recount of 2026-11-03:** that run's two `words` passes
disagree past the gate below, and a table with one refreshed corpus is worse than a dated one, so it
waits for a third pass on a quiet machine ([`latency-py-agreement-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-py-agreement-2026-11-03-arz-82f2cb1.json) holds the gate's
reading). The rows below are the minimum over two agreeing 15-round passes per corpus. Ratios are
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
under half — while occupying 1.24 bytes per key on disk against the `dict`'s 71–95 bytes per key in
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
Measured 2026-11-03 at `82f2cb1`, main after 4.5.3, on `performance`: six runs of the example back
to back, each lookup cell the minimum of five timed passes after a warm-up pass
([`bench/results/latency-rs-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-11-03-arz-82f2cb1.txt)).
The table quotes the minimum over all six. They agree within 2 % on `CompactHashIndex`, the closed
`HashedDictIndex` and `PerfectHashIndex::id_unchecked`, within 3 % on `HashedDictIndex::id`, within
1 % on both `HashMap` controls; `PerfectHashIndex::id` spreads 6 %, `BTreeMap` 6 %, `DoubleArrayIndex`
7 %, `DictIndex` 7 %, with no trend, and `StringIndex` reads 253–255 ns on most runs and 284 on the
third. Absolute numbers are
machine-dependent — the `std::HashMap` control reads 230 ns here against the 233 of the 4.4 table, the
234 of the 4.1 table, the 241 of the 4.0 table, the 285 of v3.0.0, the 295 of 2.1.0, the 289 of 2.0.0
and the 245 of 1.1.0 — so compare the **ratios**, only within a column, and read a shift under ~15 %
between tables as the session: against this `HashMap`, `PerfectHashIndex::id_unchecked` is 0.22×,
`CompactHashIndex::id` 0.26×, `HashedDictIndex::id_unchecked` 0.27× and its `id` 0.34×,
`PerfectHashIndex::id` 0.45×, `DoubleArrayIndex` 0.95×, `StringIndex` 1.10×, `DictIndex` 1.69×,
`BTreeMap` 3.04×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `PerfectHashIndex::id_unchecked` | ~336 ms | **~51 ns** | closed vocabulary, no membership check |
| lexindex `CompactHashIndex::id` (fp=1) | **~37 ms** | ~59 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `HashedDictIndex::id_unchecked` | ~167 ms | ~63 ns | closed vocabulary; the id is the key's rank, so `key(id)`, prefix and range stay on the same index |
| lexindex `HashedDictIndex::id` (8 bits) | ~177 ms | ~77 ns | fingerprint-checked, `2^-8`; builds are the dictionary's and the sidecar's together |
| lexindex `PerfectHashIndex::id` (verified) | ~319 ms | ~104 ns | one extra cache line + full key compare |
| lexindex `DoubleArrayIndex::id` | ~351 ms | ~219 ns | a character a load, 17.67 B/key here; 0.95× its `HashMap` |
| `std::HashMap<String, u32>` | ~169 ms | ~230 ns | in-RAM, not serialisable |
| lexindex `StringIndex` (FST) | ~233 ms | ~253 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (256 per block) | ~110 ms | ~389 ns | ordered, exact reverse; 1.93 B/key here against the FST's 0.68 — a `word.word` cross product is what a transducer factors out, and what a block of front-coded keys does not (on the dictionary: 2.64 against 5.95, 281–283 ns against 167–173) |
| `std::BTreeMap<String, u32>` | ~196 ms | ~699 ns | in-RAM |

<sub>**The memory-bound rows swing between sessions; the three hash rows halved at 4.0, and that
was not a session.** Over seven sessions `BTreeMap` read 3.19× → 3.34× → 2.63× → 3.24× → 3.01× →
3.06× → 3.04× → **3.04×** of `HashMap`, and `StringIndex`, while its code stood still from 0.5.1 to 4.2,
1.30× → 1.47× → 1.32× → 1.47× → 1.32× → 1.34×, with no trend and no single outlier, while the
FxHash map sits at 0.586× → 0.60× → 0.58× → 0.60× → 0.60× → 0.60× → 0.61× → **0.60×** throughout. The two
that move are exactly the two that miss to DRAM, they move together and in the same direction, and
they moved across sessions in which their code did not change — the signature of the machine, not
of a release. Against that background `StringIndex` now reads **1.10×**, below every session
before it, which is 4.3's reader of its transducer; and the three rows the key hash reaches moved
at 4.0 by more than any session ever has: `PerfectHashIndex::id` 0.95× → 1.04× → 1.02× → 1.03× →
0.50× → 0.47× → 0.47× → **0.45×**, `id_unchecked` 0.269× → 0.26× → 0.24× → 0.26× → 0.21× → 0.21× →
0.21× → **0.22×**, `CompactHashIndex::id` 0.435× → 0.45× → 0.45× → 0.45× → 0.25× → 0.25× → 0.25× →
**0.26×**; 4.1, 4.4 and this commit leave all three within 6 %. Verified `id` is now under half
the `HashMap` it used to trail. Three controls holding still while three rows halve is what the new
key hash, the `MPH3` seed layout and the prefetched second level look like from outside.
`HashedDictIndex` reads 63 ns closed and 77 at eight fingerprint bits, as on 4.1 — 6.2× and 5.1×
its own dictionary's `id` — and builds in the dictionary's 110 ms and 57–66 more for the sidecar.
`DictIndex` reads **389 ns for 1.93 B/key** at the default of 256 keys a block, against 442 on 4.1
— the search changes of 4.3.3 and 4.4 lie between — and 2.0.0's 507 for 3.19 at 32 a block. `CompactHashIndex`
builds in 37 ms against 48 on 3.0.0 and 69 on 1.1.0. This corpus is `DictIndex`'s worst case by
construction: the 1 M keys are 1 000 words crossed with 1 000, which the transducer stores once per
factor (0.68 B/key) and a block of front-coded keys stores once per key. Real keys move lookups in
lexindex's favour versus synthetic ones, while every `build` reads higher than a synthetic sequence
would, because real input is not pre-sorted and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~138 ns** and `PerfectHashIndex::id_unchecked`
**~51 ns** — so on a closed vocabulary the perfect hash is about **2.7× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash, and then 4.0's replacement of both.
`CompactHashIndex::id` (~59 ns) is **2.3× faster** than the FxHash map and still carries the
membership check and the 1.24 B/key blob; `HashedDictIndex::id` (~77 ns) is 1.8× faster with the
same check and the keys kept, in order.

**Two things the table above cannot show, both from an independent harness (`local/latency/`, one
process, all forms alternated per round, min of 12,
[`latency-forms-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-forms-2026-11-03-arz-82f2cb1.txt)):**

- **20–35 ns of every number in it is reaching the probe key, not looking it up.** The bench
  probes `keys[i * STEP % n]` — the original allocations in strided order, which is what a
  long-lived key list looks like. Hand the same index a probe list allocated in probe order and
  `PerfectHashIndex::id_unchecked` falls from 28.4 to **7.1 ns/op** at 1 M, `CompactHashIndex::id`
  from 36.0 to 11.3, and `PerfectHashIndex::id`, which fetches a stored key either way, from 69.3 to
  34.0. The batched `ids_of` is the layout-insensitive one at 10 M, 28.5 scattered against 25.2
  contiguous on `CompactHashIndex`, because its software prefetch does for scattered keys what the
  hardware does for contiguous ones; at 1 M its cells carry a spread of 14–56 %. So read any sub-100
  ns lookup figure, here or anywhere, as a statement about the caller's key layout as much as about
  the index.
- **On a miss-heavy workload `std::HashMap` no longer wins.** An absent key costs the SipHash map
  35.4 ns at 1 M against `CompactHashIndex::id`'s 11.1 and `PerfectHashIndex::id`'s 17.3; at 10 M,
  60.8 against 22.5 and 44.8. On 0.11 this read the other way at 1 M, 32 ns against 41: the map fails
  on an empty bucket after one cache line, where a fingerprint index runs the whole perfect hash and
  reads a fingerprint before it can say no, and that whole path has since become cheaper than the
  SipHash of the key.

**Honest reading:** for a **fixed / closed vocabulary**, `PerfectHashIndex::id_unchecked` is the
**fastest of the structures in the table above** — 4.5× as quick as the SipHash `HashMap` and 2.7×
the FxHash one (no probing, no membership comparison) *and* compact + serialisable.
`CompactHashIndex::id` keeps a probabilistic membership check and *still* beats the SipHash
`HashMap` on lookup (3.9× here), and builds in a fifth of its time. Full verification (`id`) pays one extra
cache line + a key comparison; `StringIndex` trades more latency for **ordered / prefix / range /
fuzzy** queries the hash maps cannot answer at all. So: `CompactHashIndex` when footprint dominates
and a rare false positive is fine; `HashedDictIndex` when the ids must be the key order and `id` is
the hot path; `PerfectHashIndex::id` for exact membership + reverse;
`StringIndex` when order or fuzzy/prefix matters; `HashMap` when you just need a general in-RAM map
with nothing persisted.

## The perfect hash against PtrHash and PHast

`bench/mphf_vs` is its own crate, outside the workspace, so that neither competitor becomes a
dependency of lexindex: `cd bench/mphf_vs && cargo run --release -- <keys> <rounds> <threads>`. One
process builds every function in turn over the **same distinct splitmix64 keys, in the order they
were generated**, three rounds A-B-A-B (builds: the minimum), and looks every key up in **one
shuffled probe order** (the minimum of nine passes) — the keys copied into an array in that order,
so that a query reads its own key from a sequential stream, and each function's lookup inlined into
a loop of its own, so that the column is the function alone. Beside every minimum the harness prints
its spread, the slowest repeat over the fastest, and a batch column: lexindex's `index_all` and
`ptr_hash`'s `index_stream::<32, _>` over chunks of 4096 keys, the two forms that pull a later key's
cache line in while the current one resolves (`ph` has no batch form). The construction peak is the
process's high-water mark over the build above what it held before it — the memory a build needs
beyond the keys — exact where the tables are large enough to be mapped whole (10 M and 100 M). `ph`
0.11.0 is the PHast authors' crate, with its `wyhash` feature on (its default hasher is std's
SipHash, which nobody benchmarks it with): `Function2` with `ShiftOnlyWrapped` is PHast+ with
wrapping, the design this crate's `MPH3` grew out of; `Function` with `SeedOnly` is regular PHast.
Both at 8-bit seeds and bucket size 4.5, as here. `ptr_hash` 2.1.3 is PtrHash's three parameter
sets, and `sux` 0.15.3 is PHastR at five settings. Bits and lookups are the 1-thread process's; the
8-thread process builds PtrHash and `MPH3` to the same bits, `ph` to within 0.02 and PHastR to within
0.04, which the prose below takes up.

**1 M keys** — 241 KB of table, inside L2:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.925 | **38.5 ns/key** | 11.8 ns/key | **1.6 ns** | **1.9 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.160 | 54.4 | 17.3 | 3.4 | — |
| `ph` PHast (`SeedOnly`) | 1.933 | 571.7 | 98.6 | 2.8 | — |
| `ptr_hash` compact | 2.144 | 143.5 | 76.9 | 3.6 | 4.2 |
| `ptr_hash` balanced | 2.378 | 89.2 | 47.2 | 3.7 | 4.2 |
| `ptr_hash` fast | 2.990 | 71.3 | 63.2 | 1.8 | 2.5 |
| `sux` PHastR (8-bit, 4.25) | 1.960 | 53.2 | **10.3** | 3.1 | — |
| `sux` PHastR (8-bit, 4.5) | 1.919 | 49.5 | **10.3** | 3.3 | — |
| `sux` PHastR (8-bit, 4.75) | 1.915 | 47.8 | 11.1 | 3.8 | — |
| `sux` PHastR (10-bit, 5.75) | **1.850** | 99.1 | 20.8 | 3.8 | — |
| `sux` PHastR (10-bit, 6) | 1.852 | 94.2 | 21.2 | 4.3 | — |

**10 M keys** — 2.4 MB, inside the 16 MB L3:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.915 | **34.6 ns/key** | **9.4 ns/key** | **2.4 ns** | **2.4 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.147 | 60.5 | 14.8 | 4.9 | — |
| `ph` PHast (`SeedOnly`) | 1.921 | 579.4 | 89.3 | 3.6 | — |
| `ptr_hash` compact | 2.143 | 157.4 | 42.9 | 5.1 | 4.5 |
| `ptr_hash` balanced | 2.378 | 99.6 | 26.7 | 5.1 | 4.5 |
| `ptr_hash` fast | 2.990 | 144.5 | 132.3 | 2.8 | 2.8 |
| `sux` PHastR (8-bit, 4.25) | 1.943 | 57.8 | 10.4 | 4.4 | — |
| `sux` PHastR (8-bit, 4.5) | 1.904 | 53.7 | 10.4 | 4.8 | — |
| `sux` PHastR (8-bit, 4.75) | 1.899 | 52.1 | 9.9 | 5.6 | — |
| `sux` PHastR (10-bit, 5.75) | **1.823** | 103.9 | 18.1 | 5.8 | — |
| `sux` PHastR (10-bit, 6) | **1.823** | 99.1 | 17.3 | 6.4 | — |

**100 M keys** — 24 MB, every scalar lookup a DRAM miss:

| function | bits/key | build, 1 thread | build, 8 threads | build peak | lookup | batch |
|---|---:|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.911 | **34.6 ns/key** | **7.8 ns/key** | **262 MB** | **8.7 ns** | **3.5 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.146 | 65.0 | 15.5 | 1 526 | 16.3 | — |
| `ph` PHast (`SeedOnly`) | 1.920 | 585.2 | 88.9 | 1 526 | 13.3 | — |
| `ptr_hash` compact | 2.143 | 156.1 | 39.5 | 906 | 16.2 | 5.8 |
| `ptr_hash` balanced | 2.378 | 99.2 | 23.0 | 870 | 17.2 | 5.9 |
| `ptr_hash` fast | 2.990 | 200.5 | 207.2 | 1 434 | 10.6 | 4.8 |
| `sux` PHastR (8-bit, 4.25) | 1.942 | 61.3 | 10.3 | 917 | 15.7 | — |
| `sux` PHastR (8-bit, 4.5) | 1.902 | 57.5 | 10.1 | 924 | 16.3 | — |
| `sux` PHastR (8-bit, 4.75) | 1.897 | 56.2 | 10.2 | 931 | 17.8 | — |
| `sux` PHastR (10-bit, 5.75) | **1.819** | 108.1 | 17.2 | 900 | 18.4 | — |
| `sux` PHastR (10-bit, 6) | **1.819** | 102.0 | 16.9 | 912 | 18.8 | — |

`MPH3` is **not** the smallest function in these tables. `sux` 0.15's PHastR (Vigna,
[arXiv:2610.17833](https://arxiv.org/abs/2610.17833)), built on one thread over the same keys in
the same process and hashing each with its own `ToSig` for `u64` (xxh3), takes fewer bits at
1 M / 10 M / 100 M keys: 1.919 / 1.904 / 1.902 at expected bucket size 4.5 and 1.915 / 1.899 /
1.897 at 4.75, against `MPH3`'s 1.925 / 1.915 / 1.911, and 1.850 / 1.823 / 1.819
with 10-bit seeds. Only its default bucket size, 4.25, is the larger: 1.960 / 1.943 / 1.942. Regular
PHast is the larger too, 1.933 / 1.921 / 1.920. What those bits cost is the time: at bucket size 4.5
PHastR answers in 3.3 / 4.8 / 16.3 ns against 1.6 / 2.4 / 8.7, and with 10-bit seeds in 3.8–4.3 /
5.8–6.4 / 18.4–18.8, and builds on one thread in 49.5 / 53.7 / 57.5 ns/key against 38.5 / 34.6 /
34.6, at 94–108 with 10-bit seeds. Built on eight threads, PHastR cuts its levels into more chunks
than `MPH3` does, and each chunk costs bits: at 1 M it reads 1.943 at bucket size 4.5 and 1.935 at
4.75, above `MPH3`'s 1.925, and 1.879–1.886 with 10-bit seeds, still under it; at 10 M and 100 M it
stays under `MPH3` at 4.5 and 4.75 by 0.008–0.014 bits
([`mphf-vs-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-11-03-arz-82f2cb1.txt)).

`MPH3`'s lookups lead at every size, single and batch: 1.6 / 2.4 / 8.7 ns against PtrHash's fast
set's 1.8 / 2.8 / 10.6, and 1.9 / 2.4 / 3.5 in a batch against its 2.5 / 2.8 / 4.8. That set is the
one that comes near, on 3 bits a key, a table half again as large whose one probe is a seed byte
with no bumped keys' chain behind it. 4.5.1 took the single lookup ahead: in a cool A/B of these two
rows alone on 2026-09-25, on `powersave`, one lookup thread, six processes a commit
([`bench/results/mphf-cool-ab-2026-09-25-arz-e523ecd.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-cool-ab-2026-09-25-arz-e523ecd.txt)),
`MPH3`'s went 1.8 → 1.65 ns at 1 M against fast's 1.8, and 2.5 → 2.4 at 10 M against 2.9, and its
batch 2.0 → 1.9 and 2.5 → 2.45 against 2.4 and 2.8. Among the rows near 2 bits `MPH3`'s lookups are the fastest at every size — PtrHash
compact's 3.6 / 5.1 / 16.2 ns single and 4.2 / 4.5 / 5.8 in a batch, PHast+'s 3.4 / 4.9 / 16.3,
and regular PHast's 2.8 / 3.6 / 13.3 at fifteen to seventeen times the build and without a batch
form. The bits column is the serialised table, which `MPH3` loads as it is — the rank and select
counts `MPH2` derived in memory, 0.023 bits/key, went with its remap.

`MPH3`'s build is the fastest on one thread at every size: 38.5 / 34.6 / 34.6 ns/key against
PHastR's 47.8 / 52.1 / 56.2 at its quickest, PHast+'s 54.4 / 60.5 / 65.0 and PtrHash fast's 71.3 at
1 M. On eight threads it reads 11.8 / 9.4 / 7.8 against PHast+'s 17.3 / 14.8 / 15.5 and PHastR's
10.3 / 9.9 / 10.1: **PHastR builds faster at 1 M**, at the higher bits above, with a spread of up to
34 % against `MPH3`'s 23 %, and `MPH3` at 10 M and 100 M. Against PtrHash's compact set `MPH3` builds
3.7–4.5× faster on one thread and 4.6–6.5× on eight. The run before, at `e147b05`, had PHast+ ahead on one
thread at every size, 57 / 57 / 68 ns against 75 / 71 / 70, over the same tables: since then a
bucket's seed is searched over fixed lanes, one function a bucket width from 1 to 16 keys with every
loop's count known to the compiler, a mode's keys that could meet are found by their residues modulo
the slice, and the buckets wait for placement in a bitmap of priority slots taken from its lowest
set bit, not in a queue a size class under a heap. The construction peak is the column apart: 30 MB
at 10 M and 262 MB at 100 M above the keys, against 77–156 MB and 870–1 526 MB — the table is built
a chunk of buckets at a time, the keys of a quarter of the chunks copied out at once, and nothing
the size of the key set is ever held beside it. Every table here before 2026-09-17 timed the builds
over keys the harness had sorted for its dedup: `MPH3` skips its own sort on sorted hashes, and the
other rows bucket their keys whatever the order (PtrHash's compact and fast sets read within 3 %
either way, alternated), so those tables read `MPH3`'s build at 55–58 ns/key on one thread and 9–11
on eight with a peak of 6 MB at 10 M and 60 MB at 100 M — the path a caller handing over sorted
hashes takes, as every index in this crate does, but not the one the other rows were timed on. One
asymmetry is in the numbers and should be read out of them: lexindex takes the keys as 64-bit hashes
(its indexes hash the string once, before), while `ph` hashes each key with wyhash on build and on
every lookup level and `ptr_hash` with one multiply — a nanosecond or two of the gap on the `ph`
rows is that.

**String keys.** The tables above take the keys as 64-bit hashes. `bench/mphf_vs`'s `strings`
binary runs the path a user of string keys sees, end to end, on real corpora (`local/corpora`, not
in the repository: an English word list, URLs, UUIDs, English Wikipedia titles, file paths):
`ClosedHashIndex::id` — the key hash, then `MPH3` — against PtrHash's fast set over the same key
hash, which isolates the perfect hash, and over xxh3 of the bytes, PtrHash as one would build it
over strings with a general-purpose string hash. The key hash is lexindex's own `hash_key_bytes`
([the design notes](design.md#perfecthashindex)); xxh3 is the 64-bit XXH3 of `xxhash-rust`,
PtrHash's `hash::Xxh3`, which its default features build. PtrHash's documentation recommends
`StringHash` for strings instead, which is GxHash and needs its `gxhash` feature. No row here uses
it, and GxHash's 128-bit form, the `StringHash128` PtrHash recommends past 10⁹ keys, has a
collision that no seed removes: two 88-character strings that differ in five positions share their
hash under each of a million seeds
([Groot Koerkamp, 2026-07](https://curiouscoding.nl/posts/gxhash-collision/)), so a build over a
set that holds the pair cannot tell them apart. One process a corpus, three rounds; a round builds
every row first and then times the rows in turn within each pass, each turn preceded by an untimed
sweep over the first 2^20 probe keys, so that a machine warming through a round slows every row
alike and no row holds the caches to itself for a turn of its own. Every key is looked up once in a
shuffled order with the key read from the corpus, the minimum of nine passes; batch is `ids_of`
against `index_stream` in chunks of 4096; ×8 is the wall time a key with eight lookup threads. Each
cell is lookup / batch / ×8 in ns. Measured 2026-11-03 at `82f2cb1`, on `performance`.

| corpus, keys (mean bytes) | **lexindex `ClosedHashIndex`** | `ptr_hash` fast, the same hash | `ptr_hash` fast, xxh3 |
|---|---:|---:|---:|
| words, 480 k (9) | 8.1 / **5.9** / 1.6 | 8.1 / 7.2 / 1.6 | 24.1 / 19.0 / 3.9 |
| URLs, 1 M (52) | **38.7** / **12.5** / **6.5** | 45.8 / 31.8 / 7.6 | 69.7 / 72.5 / 10.0 |
| UUIDs, 1 M (36) | **35.2** / **10.4** / **5.8** | 40.7 / 29.3 / 6.5 | 62.6 / 58.6 / 8.6 |
| titles, 1 M (21) | 24.9 / **9.6** / 4.0 | **24.7** / 20.0 / **3.9** | 50.9 / 47.1 / 7.4 |
| paths, 1 M (125) | 61.4 / **41.8** / 13.0 | **60.0** / 52.9 / **12.7** | 115.4 / 123.6 / 17.4 |
| URLs, 10 M (52) | **43.1** / **14.8** / **7.1** | 54.6 / 35.5 / 8.3 | 79.1 / 81.8 / 10.9 |
| UUIDs, 10 M (36) | **40.3** / **13.3** / **6.4** | 53.8 / 33.3 / 7.9 | 75.1 / 69.8 / 10.3 |
| titles, 10 M (21) | **35.3** / **11.3** / **5.8** | 40.7 / 25.8 / 6.3 | 74.1 / 68.6 / 10.0 |

With the key read from memory the hash is most of the cost, and the same hash under both perfect
hashes is what the middle column holds fixed: `ClosedHashIndex` leads the fast set on five of the
eight corpora, by 5.4 to 13.5 ns, and the lead grows with the corpus — on 480 k words the two tie,
and at 1 M titles and 1 M paths, where every table in the process still fits in the last level
beside the others, `fast` is 0.2 and 1.4 ns ahead; at 10 M, where they do not, `MPH3`'s 1.92 bits a
key against 2.99 is 5.4 to 13.5 ns. The hash decides the rest: the same set over xxh3 is 16 to 55 ns
behind. The batch column is the crate's alone: `ids_of` hashes the next keys and pulls in both ends
of each key while the current ones' lines arrive, 12.5 / 10.4 / 9.6 ns on 1 M URLs / UUIDs / titles
against `index_stream`'s 31.8 / 29.3 / 20.0 over the same hash and 72.5 / 58.6 / 47.1 over xxh3,
which does not overlap the hashing — 2.1 to 2.8× on every corpus but the word list, whose keys are
9 bytes, and the 125-byte paths. The `CompactHashIndex` row's single lookup is `id_unchecked`, the
same perfect-hash probe without the fingerprint compare, within 3.1 ns of `ClosedHashIndex` on every
corpus; its batch is `ids_of`, which does compare, and pays the fingerprint line's miss, 2.9 to
6.2 ns above it.

**The xxh3 column moved and nothing here explains it.** Against the `powersave` run of 2026-09-17
([`mphf-strings-2026-09-17-arz-2a9acef.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-09-17-arz-2a9acef.txt)) the fast
set over xxh3 reads 16–50 % slower on the single lookup, 1 M URLs 51.5 → 69.7 ns and 10 M titles
49.3 → 74.1, while the same set over lexindex's hash reads −6…+12 %, 1 M URLs 45.7 → 45.8. No code of this crate is in that path, and the cause is not traced. So do not read the
wider gap to xxh3 as a gain of this crate: the smaller gaps of September are as good a reading of
what a general-purpose string hash costs.

The earlier history of this table is in the older files, read against each other through the
`ptr_hash` over xxh3 rows, which then agreed within 3 % across two runs. Between 2026-09-16 at
`7215616` and 2026-09-17 at `2a9acef` every lexindex row got quicker — 1 M URLs 43.0 → 38.5 ns single
and 28.7 → 12.8 in a batch, 10 M URLs 48.1 → 43.1 and 30.9 → 14.6 — over eighteen commits to `src/`,
the batch column following the one that prefetches a key's last line as well as its first. The key
hash's own gain was measured the same way one commit earlier:
against the `b4544db` file over the hash 2.0–3.x shipped, in the protocol both of those runs used,
every lexindex row was 10–30 % quicker — 1 M URLs 54.5 → 43.0 ns, paths 92 → 65, titles 32 → 28,
UUIDs 46 → 39 — while the xxh3 rows, the same code in both, read 3–15 % slower in the newer one, so
that gain too is at least the difference shown. Results files:
[`bench/results/mphf-strings-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-11-03-arz-82f2cb1.txt),
[`bench/results/mphf-strings-2026-09-17-arz-2a9acef.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-09-17-arz-2a9acef.txt),
[`bench/results/mphf-strings-2026-09-16-arz-7215616.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-09-16-arz-7215616.txt).

The lexindex row ran once more at each size in a process with transparent huge pages turned off
(`prctl(PR_SET_THP_DISABLE)`), the control for the `MADV_HUGEPAGE` its tables ask for from 2 MiB
up: 1.6 / 1.9 ns at 1 M, as with them (nothing to get below one huge page), and 2.6 / 2.6 against
2.4 / 2.4 at 10 M, 8 % on both. The recount has no such process at 100 M: on 2026-09-17, on
`powersave`, it read 10.8 / 4.1 against 10.0 / 3.5, 8 % and 17 %, with the results file's `huge MB`
column showing the pages the process holds after the build, 40 MB at 100 M against none without them.

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
Since then the placed lookup lost nine of its 32 instructions — the seed's mode and shift come out
of one shift and one mask, the wrap is a cold call — and the single lookup went 2.6 → 2.1 ns at
1 M, 3.8 → 3.1 at 10 M and 13.7 → 12.0 at 100 M, the 1 M batch 2.8 → 2.3. Since then a bucket's
seed is chosen by the product of its keys' positions, which bumps a sixth fewer keys, a bumped
key's remap is a sampled Elias–Fano stream read in two steps, a first level past L3 has each seed
pulled in three times, and a seed's mode shift is read from a table: the single lookup went
2.1 → 1.9 ns at 1 M, 3.1 → 2.6 at 10 M and 12.0 → 10.2 at 100 M, the batch 2.3 → 2.2, 2.9 → 2.5
and 4.0 → 3.6.

**ConsensusRecSplit** ([Lehmann, Sanders, Walzer, Ziegler 2025](https://arxiv.org/abs/2502.05613)),
the space record at 1.444 + ε bits, is C++20 under the GPL, so it ran in its own process on the
authors' harness at their commit `11deea6`: XorShift64 keys, one construction timed in
milliseconds, the queries drawn with replacement into a plan array (as above, no key fetch inside
the query), single-threaded, one run per configuration, on the same machine in the same window as
the tables above ([`mphf-consensus-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-consensus-2026-11-03-arz-82f2cb1.txt)). `k` is the bucket size, ε the overhead over the bound, `-o` the query-optimised variant.

| function | 1 M: bits/key | build | lookup | 10 M: bits/key | build | lookup |
|---|---:|---:|---:|---:|---:|---:|
| ConsensusRecSplit k = 256, ε = 0.1 | 1.583 | 271 ns/key | 51 ns | 1.579 | 268 ns/key | 87 ns |
| … k = 256, ε = 0.1, `-o` | 1.580 | 382 | 69 | 1.575 | 392 | 77 |
| … k = 256, ε = 0.01 | 1.479 | 1 597 | 52 | — | — | — |
| … k = 32 768, ε = 0.1 | 1.620 | 635 | 95 | — | — | — |
| … k = 32 768, ε = 0.01 | 1.493 | 1 909 | 94 | **1.459** | 1 979 | 141 |
| … k = 32 768, ε = 0.01, `-o` | 1.491 | 3 002 | 118 | **1.459** | 3 067 | 131 |
| **lexindex `MPH3`**, one thread | 1.925 | **38.5** | **1.6** | 1.915 | **34.6** | **2.4** |

The last row is the tables' own `MPH3`; the `MPH2` row this table carried went with its format, gone
since 4.0. The trade is 0.46 bits
a key — on a `CompactHashIndex` at 1.24 bytes a key, 5 % of the index — for 8–89× the build and
32–59× the lookup at 10 M. That is the right answer for an archive written once and read rarely;
this crate's tables are read on every `id`, and 1.92 bits is where its lookup stays first.

What this campaign does not have. A second CPU: one Ryzen 7 5800HS, a mobile part with 16 MB of L3,
and the 100 M rows are where its memory system shows — a server part with a larger cache and more
channels moves every row there. A 1 B row in the tables — it has runs of its own, eight build and
eight lookup threads with the probe order sampled to 10 M keys, because the full one is another 8 GB
beside a 14 GB construction peak. The recount, on 2026-11-03 at `82f2cb1` on `performance`, is three
processes with four copies of each table and where each copy landed read beside it, and a fourth
with huge pages off
([`bench/results/mphf-vs-1b-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-1b-2026-11-03-arz-82f2cb1.txt)):
`MPH3` builds over 10⁹ keys in 7.4 s at 1.911 bits and **2.6 GB above the keys**, against 24–427 s and
8.0–14.5 GB for PtrHash's three sets. Its single lookup is the fastest of the four rows there, 12.7 ns
against PtrHash fast's 13.7 and compact's 24.3, and so is its batch, 7.1 against 7.9 and 8.2 (medians
over twelve copies). With eight lookup threads every row waits on how fast the memory under its table
answers random loads, and on this machine — two unequal modules, the top 16 GiB of memory interleaved
across both channels — that can be where the table lands. On 2026-09-26, on `powersave`
([`bench/results/mphf-vs-1b-2026-09-26-arz-8537f35.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-1b-2026-09-26-arz-8537f35.txt)),
`MPH3`'s tables, on the huge pages they ask for, landed on memory 21–23 % slower for random loads than
PtrHash's on small pages. Here they did not: all four rows read 3.46–3.71 ns a load, `MPH3` 3.54. So
with eight threads its lookup reads 3.9 ns against fast's 3.8 and compact's 4.5, and its batch 3.7
against compact's 3.2, balanced's 3.3 and fast's 3.5. With huge pages off the rows land within 3 % of
each other, 3.54–3.66, and the lookup reads 4.0 against fast's 4.0 — level — but the batch 3.7
against compact's 3.1 and balanced's 3.2: **the eight-thread batch is where `MPH3` loses at a billion
keys, 19 % behind PtrHash compact on the same memory.** Hardware counters over the two batches in a
separate harness say what that is made of
([`bench/results/mphf-lanes-1b-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-lanes-1b-2026-11-03-arz-82f2cb1.txt)):
with huge pages off, compact's batch fetches 0.73 lines a key from DRAM against `MPH3`'s 0.98 and
finds 0.17 in the L3 against 0.10. Its bucket function is skewed, so the pilots of its dense buckets
answer a large share of the keys and stay in the 16 MB cache; `MPH3`'s buckets are uniform, and over
its 240 MB about one line in fifteen can be cached. Without huge pages its one-thread batch also
reads 7 % faster here and its single lookup 10 % slower; a machine with matched modules has no
slower part for huge pages to land in, and this campaign has none to measure that on. On 2026-09-17
([`bench/results/mphf-vs-1b-2026-09-17-arz-08094a1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-1b-2026-09-17-arz-08094a1.txt))
the eight-thread columns moved by up to 58 % between processes over the same code, and that is the
size of this: `MPH3`'s copies moved as much with where they landed, 32–58 % between the table
built and its clones in a run that read the sequential rate only. That run also holds the `ph` rows,
21–100 s and 9.7 GB to build, whose PHast answered a lookup in 19.6 ns. The 1 B run before the harness
corrections above, on a hot machine, read 30.0 and 9.9 ns for `MPH3`'s single and batch lookups over
sorted keys, behind every PtrHash set. [The `mphf-lanes-1b` file of 2026-09-26](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-lanes-1b-2026-09-26-arz-6969644.txt) read compact's batch at 0.83 lines on `powersave`. Confidence intervals: in their place, every cell's minimum with the spread of its repeats, in the
results file — builds repeat within 8 % on one thread but for PHastR's default at 1 M (12 %), and
within 15 % on eight but for `MPH3` (23 %) and PHastR's default (34 %), both at 1 M; lookups within
13 % at 1 M, within 11 % at 10 M but for PHast's 14 %, and within 8 % at 100 M; batches within 7 %
at 10 M and 100 M and within 16 % at 1 M (passes of about 2 ms).

<sub>The three tables were measured 2026-11-03 at `82f2cb1`
([`bench/results/mphf-vs-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-11-03-arz-82f2cb1.txt)),
on `performance`, in one window with no editor open, 40 °C at the start. The run before, on
`powersave`, is 2026-09-17 at `08094a1`
([`bench/results/mphf-vs-2026-09-17-arz-08094a1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-17-arz-08094a1.txt)),
on mains with an editor open, after a wait for Tctl under 60 °C and a load under 0.7 (49.0 °C and
0.26 at the start, 52 minutes after a reboot); this file's comparisons below are to that one. The run before it, at `e147b05`
([`mphf-vs-2026-09-17-arz-e147b05.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-17-arz-e147b05.txt)),
51.9 °C and 0.29 at the start, built the same tables before the seed search's lanes: `MPH3`'s
builds read 75.2 / 70.6 / 70.3 ns/key on one thread and 16.5 / 13.6 / 12.3 on eight, its lookups
within 0.2 ns of these. Its other rows' builds on one thread agree with this file's within 2.3 %
but for PHast+'s at 10 M, 57.2 against 60.2, and PtrHash fast's at 100 M, 194.1 against 247.5; on
eight within 2.4 % at 1 M and up to 10 % faster at 10 M and 100 M; their single lookups within
0.5 ns, and PtrHash compact's and balanced's batches 0.4–0.7 ns faster (compact's 3.6 / 4.0 / 5.7
against 4.2 / 4.7 / 6.2). The run before that, at `ba0580b`
([`mphf-vs-2026-09-16-arz-ba0580b.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-16-arz-ba0580b.txt)),
is the last over sorted keys: its `MPH3` rows read 1.966 / 1.953 / 1.949 bits, builds of
57.7 / 55.6 / 57.6 ns/key on one thread and 11.1 / 9.2 / 9.0 on eight, and single lookups of
2.1 / 3.1 / 12.0 ns. Its other rows' builds agree with the `e147b05` file's within 4 % but for
PHast+'s 16 ms build on eight threads at 1 M (11 %), the 100 M builds on eight threads, 4–8 %
slower there, and PtrHash fast's on one, 249.7 against 194.1 ns — the same row read 233–242 in
four more processes within the hour of the `e147b05` run, the keys in generation order and sorted
alternated, so the key order does not move it — and their lookups within 0.9 ns. The morning's
run at `b4544db`
([`mphf-vs-2026-09-16-arz-b4544db.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-16-arz-b4544db.txt)),
two minutes after a reboot at Tctl 49 °C, read 2.6 / 3.8 / 13.7 ns for `MPH3`'s single lookup
before the placed path was cut; its other rows agree with the `ba0580b` file's within their
spreads. Two
corrections to the harness are in that commit, and they change how the earlier files read. `ph`'s
default hasher is std's SipHash-1-3 — its `seedable_hash` dependency has default features off —
so every earlier file's `ph` rows timed a configuration the crate does not intend (PHast+ 8.8 /
12.9 / 42.5 ns in
[`mphf-vs-2026-09-16-arz-c1ff98b.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-16-arz-c1ff98b.txt)
against 3.5 / 5.3 / 17.3 with wyhash, its build 69.6 / 77.7 / 102.7 against 58.3 / 61.3 / 71.9),
and their headers' "wyhash" is wrong. And the lookup loops were inlined into one large function
beside each row's build, which cost lexindex 0.7–3.6 ns a lookup and no other row anything
measurable: a cool run of the previous harness the same morning read 3.3 / 4.8 / 17.3 against
2.6 / 3.8 / 13.7 in that file. The old files stay as they were measured; the `MPH2` of the c1ff98b
tables (2.099 / 2.088 / 2.086 bits, 42.9 / 40.8 / 41.2 ns builds, over sorted keys) is what 1.1 to
3.0 write, and `MPH3`'s build over the same sorted keys at `ba0580b` took 1.3× its time for 6 %
fewer bits and half the bumped keys. The file before,
[`mphf-vs-2026-09-13-arz-a5ad812.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-13-arz-a5ad812.txt),
is the one the Consensus table's former `MPH2` row and
[`mphf-consensus-2026-09-13-arz-a5ad812.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-consensus-2026-09-13-arz-a5ad812.txt)
were measured beside. The 2026-09-10 file's lookups
([`mphf-vs-2026-09-10-arz-5ba9f36.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-10-arz-5ba9f36.txt))
are five times those — 29 ns for `MPH2` against the c1ff98b file's 5.8, 46 against 6.6 for PtrHash
compact — because that harness read each probe key through `keys[order[i]]`, a random 8-byte fetch
from an 80 MB array per query, and so charged every row one DRAM miss that was the caller's, not
the function's; the builds agree within 7 %.</sub>

### Misses a lookup, not only nanoseconds

A lookup column says a row is slower. It does not say whether the row touches another cache line or
merely does more work between two loads, and those are different findings with different fixes. With
`MPHF_VS_MISSES=1` the harness opens a `PERF_COUNT_HW_CACHE_MISSES` counter on the timing thread
through `perf_event_open(2)` -- there is no `/proc` file for it -- and resets and reads it around
each single-lookup pass. It is off by default: an `ioctl` either side of a timed pass is a syscall
this page's nanoseconds are not taken with.

The probe order is read sequentially at eight keys a 64-byte line, so **0.125 of every count below
is the key fetch, not the function**. Net of it, over 300 million keys, where no table is resident:

| function | bits/key | LLC misses a lookup | net of the key fetch | lookup ns | batch ns |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | 1.911 | **1.073** | **0.948** | **11.1** | **5.7** |
| `sux` 0.15 PHastR (8-bit, 4.25) | 1.942 | 1.197 | 1.072 | 19.8 | — |
| `ph` 0.11 PHast (SeedOnly) | 1.921 | 1.222 | 1.097 | 17.2 | — |
| `ptr_hash` 2.1.3 compact | 2.143 | 1.223 | 1.098 | 21.8 | 6.9 |
| `sux` 0.15 PHastR (8-bit, 4.5) | 1.902 | 1.246 | 1.121 | 21.3 | — |
| `ptr_hash` 2.1.3 fast | 2.990 | 1.263 | 1.138 | 12.5 | 6.9 |
| `ptr_hash` 2.1.3 balanced | 2.378 | 1.276 | 1.151 | 22.0 | 7.0 |
| `sux` 0.15 PHastR (8-bit, 4.75) | 1.898 | 1.319 | 1.194 | 23.9 | — |
| `sux` 0.15 PHastR (10-bit, 5.75) | 1.820 | 1.387 | 1.262 | 24.1 | — |
| `ph` 0.11 PHast+ (ShiftOnlyWrapped) | 2.146 | 1.447 | 1.322 | 23.6 | — |
| `sux` 0.15 PHastR (10-bit, 6) | **1.819** | 1.450 | 1.325 | 26.1 | — |

<sub>[`mphf-misses-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-misses-2026-11-03-arz-82f2cb1.txt)
— the same three sizes in one file, 10 M, 100 M and 300 M, one process each, eight build threads and
one round; the cell is the fastest of three passes. The
nanoseconds in this table are taken with the counter running and are not the ones the tables above
publish.</sub>

The survey's figures for the same quantity are PtrHash 1.0 lines a query and PHast 1.1, and these
agree with them. **`MPH3` is the only row under one line**, and that is the reading of its
single-lookup lead over `ptr_hash` fast at this size — 11.1 ns against 12.5, on 17 % fewer misses,
36 % fewer bits and a build 50 times faster. On the same two rows the `powersave` run of 2026-09-20
([`mphf-misses-2026-09-20-arz-1a9003e.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-misses-2026-09-20-arz-1a9003e.txt)) read a tie,
16.3 against 16.2. At 100 million keys the tables are 24–37 MB against a 16 MB L3 and
partly resident, and `MPH3`, PtrHash's sets, PHast and PHastR's 8-bit rows at 4.25 and 4.5 sit between 1.039 and 1.084 — the ordering
there says nothing, and the gap only opens once nothing is resident.

### What an instruction costs inside the miss

A lookup that waits on memory is often said to get its arithmetic for free. It does not, and the
price is measurable: `local/uopspad` welds `PAD` padding instructions into the probe loop between
one query and the next, with `PAD` a const generic so the padding unrolls rather than becoming a
loop of its own. Four shapes — a `nop`, which needs no port and no operand; independent `add`s on
four rotating registers; a chain of `add`s on one register; and dependent loads inside 4 KiB that
never leave L1.

| n | table | 0 | 4 | 16 | 64 | 128 | ns an instruction |
|---|---:|---:|---:|---:|---:|---:|---:|
| 100 M | 22.8 MB | 9.10 | 11.59 | 15.89 | 23.01 | 30.17 | **0.11** (`nop`) |
| 100 M | 22.8 MB | 9.11 | 16.20 | 21.11 | 32.33 | 48.53 | 0.25 (independent `add`) |
| 100 M | 22.8 MB | 10.11 | 13.13 | 19.35 | 34.66 | 66.53 | 0.50 (a chain) |
| 10 M | 2.3 MB | 2.43 | 3.08 | 4.10 | 6.75 | 9.87 | **0.049** (`nop`) |

<sub>[`uops-pad-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/uops-pad-2026-11-03-arz-82f2cb1.txt)
— ns a lookup, the minimum of three rounds taken A-B-A-B over the pad counts, and the slope over
the last two. Three 100 M runs read the `nop` slope at 0.1118, 0.1107 and 0.1121: ±0.7 %. The
control is the probe order: the third of those is the generation order rather than shuffled, so the
loop waits on the table and not on the probe array, which is read sequentially either way.</sub>

The loop is not paying for the instruction's execution. It is paying because the instruction
displaces another query from the window, and the window is what buys the memory-level parallelism:
one dependent load at a time over a region the size of the table takes **43.7 ns** at 22.8 MB and
10.6 at 2.3 MB, while the loop answers a key in 9.1 ns, so it keeps **4.8 misses in flight**.
Latency over the slope reads 389–401 instructions at 100 M — above Zen 3's 256-entry reorder
buffer, so take it as an upper bound on the window rather than a reading of it, since some nops are
eliminated at rename. At 10 M the same arithmetic gives 217 and means nothing: that loop is
throughput-bound, not window-bound, which is exactly why its slope is under half the other's.

The `powersave` run of 2026-09-20
([`uops-pad-2026-09-20-arz-db32225.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/uops-pad-2026-09-20-arz-db32225.txt)) read every price
about twice this, 0.23 ns a `nop` and 85.3 ns a dependent load, on the same table at 15.0 ns a key.
The two runs differ in governor, kernel and compiler, and this harness does not separate them.

Two things follow. A **dependent** instruction costs 0.50 ns a step against an independent one's
0.25 and a `nop`'s 0.11, so a chain welded onto a lookup path costs what two independent
instructions or four `nop`s do — which is the argument against trading a table load for a longer
arithmetic chain, not for it. And **0.11 ns an instruction is the exchange rate for the last of the
lookup column**: at 300 million keys `MPH3` answers in 11.1 ns against `ptr_hash` fast's 12.5 with the
counter running, so that gap is worth about twelve instructions, and `MPH3` reaches it on 17 % fewer
misses.

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
| `id`, member | 45 ns | 50 ns | one index per process |
| `id`, absent | 38 ns | **29 ns** | one index per process |
| `id`, 50 % absent | 50 ns | 52 ns | in-process |
| `ids_of`, member | 50 ns | 53 ns | in-process |
| `ids_of`, absent | 46 ns | **34 ns** | in-process |
| `CompactHashIndex::id_unchecked` (control) | 21 ns | 21 ns | |
| bytes per key | 9.25 | 10.25 | exactly +1 |

This dictionary now builds the merged arena, 9.25 bytes a key, whose fingerprints sit in a row of
their own ahead of the entries: an absent probe reads that table and stops, and the key — the second
cache miss — is never read. A member pays the second hash and one compare, a few nanoseconds.

**On the blocked layout**, which a key set that does not merge still takes, an absent probe stops at
the block: the offset line says no key with that fingerprint is in the slot. Measured there on
2026-09-10, the fingerprint bytes follow the block's offsets; interleaving them
with the offsets cost a member 6 ns instead of 3, because the offset pair
moved up to 36 bytes from the base and split cache lines more often. And the fingerprint is
compared before the slot's offsets are read: an absent probe was then about 255 instructions long,
the width of the reorder buffer, which is what lets its one cache miss overlap the next probe's.
A three-instruction marker check added for the overflow blocks pushed the path past that and
cost 11 ns with no change in cache or branch misses; moving the fingerprint first shortened it by
the whole span computation and took the absent probe from 91 to 74.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/negfp-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/negfp-2026-11-03-arz-82f2cb1.txt)).
**The control moved**: `CompactHashIndex::id_unchecked` reads 21 ns here against 41 on 2026-09-10,
over 4.0's key hash and a change of governor, so no row here compares with the table this replaces.
The blocked figures are
[`bench/results/negfp-2026-09-10-arz-679a8c6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/negfp-2026-09-10-arz-679a8c6.txt),
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
| `id`, member | **18.7 ns** | 22.8 ns (`id`), 18.8 ns (`id_unchecked`) |
| `ids_of`, member | **7.8 ns** | 18.0 ns |
| bytes per key | **0.242** | 1.242 |

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`, three processes back to back and the minimum
of the three, which agree within 3 %
([`bench/results/closed-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/closed-2026-11-03-arz-82f2cb1.txt)),
Ryzen 7 5800HS. Both `id` rows halved against the 2.0.0 table
that first published them (40 → 18.7 and 68 → 22.8), which is the new key hash and the `MPH3` seed
layout, and both sizes fell 0.021 B/key with the hash; the `powersave` run of 2026-09-19 read 19.8 and
24.7. Within a process the rows alternate between the two mode orders the harness reverses on
alternate rounds — whichever lane runs second finds the key array warm — so the minimum is the warm
one, in this table as in the one it replaces.</sub>

## `DictIndex`: the ordered dictionary against `StringIndex`

`local/dictbench` builds the dictionary as a `StringIndex` and as a `DictIndex` at six block sizes,
then probes all 479 823 words in a shuffled order — members, strangers (each word with a byte
appended), and every id for the reverse lookup — five rounds in one process, the variants alternated
within each round, the minimum per cell; `StringIndex` is the control.

| | `StringIndex` | `DictIndex` 32 | `DictIndex` 64 | `DictIndex` 128 | `DictIndex` **256** | `DictIndex` 512 | `DictIndex` 1024 |
|---|---:|---:|---:|---:|---:|---:|---:|
| bytes per key | 5.95 | 2.85 | 2.72 | 2.66 | **2.64** | 2.52 | 2.51 |
| of which per-block arrays | — | 0.152 | 0.121 | 0.105 | 0.100 | 0.050 | 0.051 |
| microblocks a block | — | 2 | 4 | 8 | 16 | 16 | 32 |
| entries a lookup scans | — | 16 | 18 | 22 | **30** | 46 | 62 |
| build | 101–105 ms | 32 | 29–30 | 27–28 | 27–28 | 26 | 32–33 |
| `id`, member | 167–173 ns | 253–254 | 250–254 | 263–265 | **281–283** | 312–322 | 341–342 |
| `id`, stranger | 124–126 ns | 207–209 | 206–209 | 219–222 | 241 | 269–271 | 297–299 |
| `key_into` (no allocation) | — | 141–142 | 151–152 | 169–171 | **198–199** | 259–261 | 307 |
| `key` (owned string) | 446–458 ns | 191–193 | 208–210 | 231–237 | 265–270 | 326–329 | 375–379 |
| `lower_bound`, stranger | — | 204–206 | 205–206 | 217–220 | 238–240 | 267–268 | 295–296 |
| `prefix`, 2 000 three-byte prefixes of ~760 keys | — | 32.2–32.8 µs | 32.1–32.3 | 32.2–32.3 | 32.2–32.4 | 31.5–31.9 | 31.8–31.9 |
| **routed**: restart words, bytes a key in memory | — | 0.50 | 0.50 | 0.50 | 0.50 | 0.25 | 0.25 |
| routed `id`, member | — | 229–231 | 222–224 | 214–215 | **213–215** | 235–240 | 236–238 |
| routed `id`, stranger | — | 184–185 | 176–178 | 170 | 167–168 | 193–194 | 193–195 |
| routed `lower_bound`, stranger | — | 182 | 174–177 | 167–170 | 164–167 | 192–194 | 191–194 |

Three runs of the same ladder, each once as a load leaves the index and once after
`route_microblocks()`; where the runs differ the table gives the range. At the default 256,
`DictIndex` answers `id` 63–69 % slower than `StringIndex` in the same process and `key_into` at
less than half its `key`, while storing **56 % less**. The ladder does not cross the control
anywhere, routed or not, and the gap has widened since 4.1: 4.3's reader of the transducer took 22 %
off `StringIndex`'s `id`, and main's took more since, up to 14.5 % against 4.5.3 on a million keys;
the search changes of 4.3.3 and 4.4 took 11–13 % off the dictionary's. `BDX3`'s codes had cost more to decode than the varint pairs `BDX2` stored — every
block got 6–10 % smaller and every `id` 17–24 % slower when it landed. The sequential lane does not
move with the block at all.

**Routed, the block stops costing the search.** The words take the walk over a block's restarts out
of the lookup and leave the one microblock's scan, so from 128 keys a block up `id` no longer pays
for the block's length: 214–215 ns at 128 and 213–215 at 256, both microblocks of 16 keys, and
235–240 at 512 and 1024, microblocks of 32. Against the same block unrouted, paired run by run, that
is 23–25 % off `id` at 256 and 30–31 % at 1024, 28–36 % off a stranger's `id` and `lower_bound`
from 256 up, and 9–10 % off `id` at 32, where the run is a single restart. Beside `StringIndex` in
the same process the gap at 256 falls from 63–69 % to 24–28 %. The words are paid in memory —
`8 / micro` bytes a key, 0.5 up to 256 and 0.25 at 512 and 1024, in no blob — so routed, the default
is no longer the obvious point: **1024 keys a block, routed, holds 2.76 bytes a key and answers `id`
in 236–238 ns, faster than any block unrouted, 32 included, and still under every `marisa-trie`
setting's 2.96–3.07**, while the default block routed, at 213–215 ns, holds 3.14 and is no longer.

**A lookup scans a microblock, not a block.** A block is cut into microblocks of 16 to 32 — the
smallest divisor of the block at or above its square root — and the first key of each is a restart
front-coded against the restart before it, so a lookup walks the restarts to one microblock and
scans that: `block / micro + micro − 2` entries, which is 30 at the default where the block holds
255. That is what unties the size from the latency. Against the
one-level format at the same size the reverse lookup halves and `id` does not move: **244–246 ns
and 392–399 for 2.819 B/key, where one level at 128 keys a block stored 2.827 for 460–464 and
393**. The one-level format reaches 0.10 B/key further down, and that is what it costs: its floor
of 2.701 at 1024 keys a block answers `key_into` in 3 190 ns and `id` in 984, against 287–289 and
420–426 for 2.80 here. Both ladders ran alternated by process in one session, the base built from
the commit before microblocks in a worktree, with `StringIndex` at 340–353 ns as the control
([`dict-onelevel-ab-2026-09-12-arz-386b2e6.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dict-onelevel-ab-2026-09-12-arz-386b2e6.txt)).
Its control read 340–353 ns on `powersave`, where this section's table reads 167–173 on a transducer
reader made faster since, so read each comparison inside its own session and neither against the other.

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
is the knob. The size falls because a larger block spreads what it stores once — its first key
whole, where that key ends, where the block starts, and an offset for each microblock — over more
keys: 0.44 bytes a key at 32, 0.06 at 1024. The front-coded data moves the other way and less, 2.39
to 2.48 at the default as keys that were heads are coded against their predecessors instead, and
back to 2.43 at 512, where microblocks double to 32 keys and a restart comes half as often. What
the block costs is the scan, 16 entries at 32 and 62 at 1024, where one level scanned `block − 1`.
At 512 the index is **2.52 bytes
per key, well under the 2.98 marisa stores on this corpus** — and unlike marisa it answers `key(id)`
and `lower_bound` at all, since a marisa id is not the lexicographic rank (7 051 of 19 999
consecutive sorted pairs come back with a decreasing id). Pick 32 or 64 if the lookups are hot,
512 or 1024 if the bytes are — or name the point instead of the number: `DictProfile::Fast`,
`Balanced` and `Compact` in Rust, `block="fast" | "balanced" | "compact"` in Python, are 32,
256 and 1024. Where a quarter of a byte a key of memory costs less than the scan, `Compact` routed
is both smaller and faster than `Fast`: 2.76 bytes a key at 236–238 ns against 2.85 at 253–254.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/dictbench-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dictbench-2026-11-03-arz-82f2cb1.txt)),
three runs of the whole ladder, each the minimum of five alternated rounds, unrouted and
routed in two processes whose order alternates between runs; the table gives the range over the
three, and the `prefix` matches a prefix come from the prefix stage. The bytes, the per-block arrays,
the restart words and the two structural rows are exact,
not measured. `StringIndex` is the control inside every process — 167–173 ns in the unrouted ones,
167–179 in the routed — but no longer across releases: it read 200–210 in the `powersave` session of 2026-09-24
([`dictbench-2026-09-24-arz-3a73ed5.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dictbench-2026-09-24-arz-3a73ed5.txt)),
and 256–270 before 4.3 changed its reader. `key_into` and `key` do not route and read the
same in both processes; the table gives the unrouted ones.</sub>

## Prefix queries against the tries

A prefix is a range of a sorted dictionary, so `DictIndex` answers one without an automaton:
`prefix_id_range` is two `lower_bound`s and `prefix` walks the run they name. `local/prefixbench.py`
puts that against the tries a Python project can install, on the same words — 20 000 three-byte
prefixes drawn from random words, so the average prefix carries 763 keys.

| | bytes/key | `prefix_count` | first 10 | every match + ids | keys only |
|---|---:|---:|---:|---:|---:|
| **lexindex `DictIndex` 256** (default) | **2.64** | **382 ns** | 2 170 ns | 100 164 ns | 73 358 ns |
| **lexindex `DictIndex` 512** | **2.52** | 414 | 2 306 | 99 775 | 70 767 |
| lexindex `StringIndex` | 5.95 | 551 | 4 094 | 154 057 | 313 535 |
| `marisa-trie` | 2.98 | 119 453 | 2 529 | 91 924 | 92 098 |
| `dawg2` | 23.96 | 66 839 | **1 425** | **47 135** | **47 126** |
| `datrie` | 30.69 | 504 994 | 504 989 | 505 276 | 505 545 |

**Counting is where the structures differ in kind rather than by a constant.** `prefix_count` costs
two order lookups whatever the prefix carries, so it runs 289× faster than marisa's at 512 per block
and 313× at the default — marisa has to enumerate all 763 matches to count them, its ids not being
lexicographic ranks, so there is no arithmetic to do instead. At every block that goes under marisa
on *size* — which since microblocks is the default too — `DictIndex` is also ahead on autocomplete
(2 170–2 306 ns against 2 529) and on handing back a prefix's keys (70 767–73 358 against 92 098) —
with a rank for each, which marisa has none to give.

**Those keys come fastest through the id range, which inverts what this page said until 2026-09-12.**
`prefix_id_range` and then `keys_of` costs 73 358 ns at the default block, against 100 164 for `prefix`,
which decodes an id for every match as well. `keys_of` used to re-enter the block for every id and
was the *slower* of the two by 2.1×; it now keeps one walk open across the ids that ascend through a
block, which is exactly the shape an id range hands it. The inversion is `DictIndex`-only:
`StringIndex` has no block to stay inside, its `keys_of` walks the transducer once per id, and
`prefix` is still the API for this query there (154 057 against 313 535). It is also why there is no
`prefix_keys` method: it would be these two calls, and the table says what a third one would buy —
27 %, which two existing calls already have.

**The last two columns are this benchmark's error bar as well as a result.** A trie has no ids to
return, so for `marisa-trie`, `dawg2` and `datrie` they are one call measured twice — and within a
run they come out 0.19 %, 0.02 % and 0.05 % apart. Nothing narrower than that is a finding here:
`DictIndex` at 256 against 512 on full enumeration (100 164 against 99 775) is a 0.4 % difference and
near it, while the 1.26× over marisa on keys alone is well outside it.

`dawg2` enumerates about 1.6× faster at 9.1× the bytes, with no reverse lookup and no mmap: a
different point on the curve, not a smaller one. `datrie` is a double-array built for point lookups;
prefix walking is not what it is for.

<sub>Measured 2026-11-03 at `82f2cb1`, one run on `performance`
([`bench/results/prefix-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/prefix-2026-11-03-arz-82f2cb1.txt)),
`marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3 over the same word list, Ryzen 7 5800HS. None
of the three is a lexindex dependency — reproduce in a throwaway environment.</sub>

## Common-prefix queries — the other direction

`prefix` returns the keys a query starts. The reverse question — which *keys* are prefixes of the
query — is what a longest-match tokeniser asks of a vocabulary, and it was the one operation
`marisa-trie`, `dawg2` and `datrie` all answered and lexindex did not. `local/cpbench.py` measures
it on the same words: 20 000 queries, each a real word with one to three more letters on it except
every fourth, which is random letters. Every structure is first held to marisa's answer on 2 000 of
them, both calls, and every row returns `(key, id)` pairs where the structure has that form —
`dawg2`'s `IntCompletionDAWG` has only the keys — so the timings compare the same work.

| | bytes/key | `common_prefix` | `longest_prefix` |
|---|---:|---:|---:|
| **lexindex `DoubleArrayIndex`** | 24.42 | **487 ns** | **235 ns** |
| **lexindex `StringIndex`** | 5.95 | 488 | 262 |
| `dawg2`, keys only | 23.96 | 537 | 578 |
| `datrie` | 30.69 | 715 | 371 |
| `marisa-trie` | 2.98 | 1 060 | 1 167 |
| **lexindex `DictIndex` 256** (default) | **2.64** | 2 277 | 914 |
| **lexindex `DictIndex` 512** | **2.52** | 2 469 | 992 |
| `datrie`, keys only | 30.69 | 285 | 198 |

**`DoubleArrayIndex` and `StringIndex` are level on `common_prefix`, at a quarter of the bytes for the
transducer, and the double array leads on `longest_prefix`.** The double array walks a character a
load and reads a match's id from the slot that reaches it: paired run by run, −1.6…+0.6 % against
the transducer on `common_prefix` and 8–12 % faster on `longest_prefix`, and 1.45–1.47× and 1.57–1.59×
faster than `datrie`, the other double array, returning the same pairs. Through 4.5.2 the first figure
read 8–11 % in the double array's favour; the transducer's reader is what moved. The query *is* the path, for both:
one walk, every key it passes a match, `O(query characters)` whatever the index holds. The last row
is the form the 4.4.2 table timed for `datrie`: the keys without their values, less work than a
pair — 285 ns against its own 715 — and quicker than any row that returns ids; lexindex has no
keys-only call. Held to that form, 4.4.2's table had `StringIndex` and `datrie` level.

**Out of cache the transducer leads the first column.** The table times each call after an untimed
pass of its own, the steady state of a tokeniser. Timed instead right after another structure's
pass — the protocol of the 4.4.2 table — `DoubleArrayIndex`'s `common_prefix` reads 672 ns against
`StringIndex`'s 515: its array is 11.7 MB on these words against the transducer's 2.9, and comes back
from memory. `datrie`, at 14.7 MB, reads 947. The second column, timed there right after the
structure's own first pass, reads level, 266 against 263.

**`DictIndex` has no walk to make and the table shows what that costs.** One order lookup per
character boundary, so a ten-character query is ten binary searches where the trie made one
descent: 2.1× marisa's time at the default block, 2.3× at 512. `longest_prefix` is the exception —
it starts at the query and stops at the first hit, so it never pays for the boundaries under the
match, and both blocks land below marisa (914 and 992 ns against 1 167) while storing fewer bytes
than it. A caller who asks this question often should hold a `DoubleArrayIndex` or a `StringIndex`;
one who asks it occasionally, alongside the ranks and ranges only `DictIndex` gives, can have it for
a binary search per character.

<sub>Measured 2026-11-03 on a wheel built from `82f2cb1`, on `performance`
([`bench/results/common-prefix-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-11-03-arz-82f2cb1.txt)):
three runs in each regime, each the minimum of five rounds; a cell is the minimum of the three. The
three agree within 4 % on every cell of the table, and within 5 % in the other regime. The harness is
the 4.4.2 table's
([`bench/results/common-prefix-2026-09-24-arz-b101076.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-09-24-arz-b101076.txt))
with the `DoubleArrayIndex` row, `(key, id)` forms for `datrie` (`prefix_items`,
`longest_prefix_item`) and `marisa-trie` (its prefix iterator with ids; for the longest, which it
has no call for, its key list and then the last key's id), a check on every `longest_prefix` answer,
and the warm regime; 4.5.0's run on `powersave` is
[`common-prefix-2026-09-25-arz-902e4b4.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-09-25-arz-902e4b4.txt). Sizes are
each structure's saved file, built as the harness builds it. Nanoseconds
per query through Python 3.14.8; the call overhead is in every row, and the three tries are C
extensions too. `marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3; none is a lexindex dependency —
reproduce in a throwaway environment.</sub>

### On Chinese running text, from Rust and from Python

A Chinese segmenter asks this question at every character: jieba builds a DAG of every word that
starts at each position of a sentence. The run below asks it at all 178 338 positions inside the
blocks jieba segments — runs of Han characters, ASCII letters and digits, its `re_han_default` — of
UD Chinese GSDSimp r2.18's 4 997 sentences, against jieba 0.42.1's 349 045-word lexicon, each query
running to the end of its block. Before any timing, every structure gave the same set of (length,
id) pairs at every position: 237 465 matches.

| Rust, ns a character | bytes/word | in text order | shuffled |
|---|---:|---:|---:|
| **lexindex `DoubleArrayIndex`**, `for_each_occurrence` | **15.62** | **10.4** | **10.9** |
| **lexindex `DoubleArrayIndex`**, a walk a position | **15.62** | **13.0** | 17.7 |
| lexindex `DoubleArrayIndex`, mapped, a walk a position | 15.62 | 13.6 | 18.5 |
| daachorse 5.1.0, charwise, a block at a time | 47.95 | 14.9 | 15.9 |
| crawdad 0.4.1 `Trie` | 18.40 | 15.6 | 20.9 |
| crawdad 0.4.1 `MpTrie` | 22.17 | 17.7 | 23.5 |
| yada 0.7.0 | 17.75 | 27.1 | 33.8 |
| darts-clone 0.32 | 17.75 | 27.6 | 33.9 |
| cedarwood 0.6.1 | 60.57 | 29.1 | 35.9 |
| lexindex `StringIndex` | 9.24 | 54.0 | 65.5 |

**On this walk lexindex is now first, in every lane.** `DoubleArrayIndex` labels a character, not a
byte, by its frequency in the lexicon, and its eight-byte slot carries everything a step reads, the
matched word's id included, so a step is one load and a match costs none past it ([the design
notes](design.md#doublearrayindex)). Walked a position at a time, as the rivals are, it takes
0.47–0.48 of darts-clone's time in text order and 0.52–0.53 shuffled, 0.44–0.49 of cedar's,
0.83–0.85 of crawdad's `Trie`, the fastest per-position rival, and 0.72–0.75 of its `MpTrie` — each
ratio paired within a process. daachorse answers a whole block at a time, so its like is
`for_each_occurrence`, which decodes a block once and walks from every character: 0.66–0.71 of
daachorse's time, at a third of its bytes. That like matters shuffled, where daachorse's block scan
is ahead of the per-position walk, 15.9 against 17.7 ns: a block at a time spreads what a lone
position pays. Mapped from its file rather than held in the huge pages a build or a `load` puts it
in, the walk costs under a nanosecond more. And the index is the smallest double array here: 15.62
bytes a word against 17.75 for darts-clone and yada, 18.40 for crawdad; cedar's figure is its
resident heap, its array growing by doubling so that a quarter of its slots sit idle, and its 0.6
file form saves 60.55 bytes a word
([`da-sizes-2026-10-31-arz.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/da-sizes-2026-10-31-arz.txt)).

**`StringIndex` is the other end of the same trade.** Its walk takes 3.7–4.2× the double array's,
at 59 % of its bytes: at 4.4.1 it ran 1 025 instructions a character against cedar's 150 and
darts-clone's 151, 5.9 steps down the transducer, each reading a node's header afresh — its state,
the widths its fields are packed in, a target and an output — where a double array reads one slot.
4.4.2's table of the state past every first character took that to 581
([`bench/results/prefix-walk-counts-2026-09-24-arz-dda19a7.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/prefix-walk-counts-2026-09-24-arz-dda19a7.txt)).
The table is 0.29 of the 9.24 bytes a word, held in memory only; the file is 8.95. What the
transducer has and the double array does not is everything but this walk: a key back from its id,
prefix enumeration, ranges, fuzzy matching.

| jieba's lexicon | build | load |
|---|---:|---:|
| **lexindex `DoubleArrayIndex`** | 0.157 s | **0.37** ms mapped · 1.69 read |
| darts-clone 0.32 | **0.055** | 0.41 read |
| cedarwood 0.6.1 | 0.100 | 39.61 read |
| yada 0.7.0 | 0.323 | 3.00 read |
| crawdad 0.4.1 `Trie` | 1.267 | 1.10 read |
| crawdad 0.4.1 `MpTrie` | 1.614 | 1.16 read |
| daachorse 5.1.0 | 1.551 | 4.29 read unchecked · 5.86 read |
| lexindex `StringIndex` | 0.082 from sorted keys | 0.37 mapped · 1.10 read, each with its first walk |

**Where the double array loses: the build.** It builds from the words in 0.157 s — under half of
yada's time, an eighth to a tenth of crawdad's and daachorse's — but in 2.9× darts-clone's time and
1.6× cedar's. Every row takes the lowest base where it fits, found over bitmaps; the window of open
blocks darts-clone searches instead, and a start that only moves up, each cost 9–10 % of the size
on this lexicon. 4.5.1 brought the build down from 0.354 s: the trie is built from the keys in byte
order, each decoded once, and the search for a row's base tests a sparse stretch word by word and a
dense one four words an instruction where the CPU has AVX2. **The load no longer loses.** It walks
every slot once, so that no slot names a row past the array or an id past the keys and no query
checks a bound, and since 4.5.1 that walk runs on the slots' 32-bit halves, eight slots an
instruction with AVX2: 0.37 ms mapped, 1.69 read, which copies the file and checks its hash as
well, where darts-clone's `open`, which reads the file and checks nothing, took 0.41 ms. In a
pairwise run on 2026-09-25, both timed in each of four processes, the mapped load read 0.62 ms against `open`'s
0.59–0.66 across the four: level. darts-clone's `set_array`, which takes a mapped array as it lies, costs nothing at
all, not timed here. crawdad and daachorse deserialise, in 1.1–5.9 ms, and cedarwood's file form, new in 0.6, reads in 39.6.

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/cjk-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-2026-11-03-arz-82f2cb1.txt), which holds this page's three Chinese tables and every version, hash and
flag): three processes. A process ran 20
rounds, the rows in a seeded shuffle each round and each timed after two untimed passes of its own —
the steady state of a segmenter, whose lexicon stays in cache; a cell is the minimum over the three.
"In text order" walks each block's positions in order with the sentences in a seeded shuffle;
"shuffled" takes the same positions in a seeded random order; `for_each_occurrence` and daachorse
take whole blocks, in the same two orders. The lexindex rows spread 0.2–1.5 % across the processes
and the rivals 0–1.7 %, but for daachorse's 4.7–5.7 %. The
control row, jieba-rs 0.11.0's `cut_all`, spread 1.8 %.
The table before 2026-09-25 ran the rows alternated, each timed after whatever the rows before it
had left in cache, which favoured the rows nearest the turn of a round: it had darts-clone at 41.1
and 34.8 ns and `StringIndex` at 63.9 and 76.5. A cold regime, 64 MiB written before every pass,
spread the control by 16.5 % and is not quoted. The build and load table ran one process each, the
minimum of 7 builds and 30 loads, the structures in a
shuffled order each round, the files in the page cache, and each load followed by one lookup so that
nothing it defers escapes the timing; a `StringIndex` load is timed with its first walk, which
derives the first-character table. The `powersave` runs of 2026-09-25 are
[`cjk-walk-2026-09-25-arz-902e4b4.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-walk-2026-09-25-arz-902e4b4.txt),
[`cjk-build-load-2026-09-25-arz-6623184.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-build-load-2026-09-25-arz-6623184.txt) and
[`cjk-build-load-2026-09-25-arz-902e4b4.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-build-load-2026-09-25-arz-902e4b4.txt), the last two holding
the double array's own before and after, 1.03 → 0.62 ms mapped, pairwise; between those two runs the
loads read from the page cache moved by up to 41 % (crawdad's `Trie` 1.17 → 1.64 ms), so a load
across runs is weak evidence. rustc 1.99.1; darts-clone at `87b71af`, built with g++ 16.2.1
`-O3`. The harness is not in the repository. No competitor here is a lexindex dependency.</sub>

**Through Python the walk is the small part of the cost.** Plugged into jieba 0.42.1 in place of
its own dictionary, one `occurrences` call a block rather than one `common_prefix` call a position
takes the DAG from 833 to 482 ns a character over the double array, and from 878 to 537 over the
transducer — the same walks, fewer calls. Every backend also answers jieba's one other dictionary
read, the `FREQ.get` that decides whether a run of single characters goes to the HMM:

| Python, ns a character | DAG | `lcut(HMM=False)` | `lcut()` |
|---|---:|---:|---:|
| **`DoubleArrayIndex.occurrences`** | **482** | **1 012** | **1 508** |
| `StringIndex.occurrences` | 537 | 1 071 | 1 578 |
| jieba's own dictionary | 623 | 1 448 | 1 974 |
| `DoubleArrayIndex.common_prefix` | 833 | 1 331 | 1 828 |
| `StringIndex.common_prefix` | 878 | 1 361 | 1 862 |
| `dawg2` `prefixes` | 966 | — | — |
| `marisa-trie` | 1 402 | 1 894 | 2 446 |
| `DictIndex` 256 | 2 013 | 2 417 | 2 965 |

Over `DoubleArrayIndex.occurrences` a cut takes 30 % less time than over jieba's own dictionary, and
24 % less with the HMM, jieba's default, which adds 500–550 ns a character to every row. Against
`StringIndex.occurrences` the double array takes 10–11 % off the DAG but 4–6 % off a cut, paired
within each process: in Rust its walk takes about a quarter of the transducer's time, and through
Python that saving is a few tens of nanoseconds in a call that spends most of its time building
tuples and jieba's DAG. `DictIndex` is the slowest here for the reason the English table gives: a
binary search a character boundary.

<sub>Measured 2026-11-03 on a wheel built from `82f2cb1`, on `performance`
([`bench/results/cjk-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-2026-11-03-arz-82f2cb1.txt)):
three processes, a cell the minimum over the three. The DAG column is per character in blocks
(178 338), the cut columns per character of text (195 832). Before any timing every backend gave
jieba's own DAG on every block and its cuts of every sentence. The control row, jieba's own
dictionary, spread 2–3 % across the processes and reads 3–5 % under the `powersave` run of 2026-09-25
([`cjk-prefix-py-2026-09-25-arz-902e4b4.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/cjk-prefix-py-2026-09-25-arz-902e4b4.txt)). CPython 3.14.8 with the GIL;
`marisa-trie` 1.4.1, `dawg2` 0.13.3. That run's harness is not in the repository;
[`bench/cjk_segment.py`](https://github.com/ilgrad/lexindex/blob/main/bench/cjk_segment.py) times the
same backends after the same check. No competitor here is a lexindex dependency.</sub>

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
| per-corpus distribution — slot-hash collisions, top 12 bits, low 12 bits, the 8-bit fingerprint, and the joint (slot, fingerprint) table | a family of keys the hash folds together, and any correlation between the two hashes | ten corpora, no collisions, every value under 2.7 |

The ten corpora are the shapes that break hashes in practice: dictionary words, word bigrams, a
shared prefix (`https://example.com/a/b/…`), a shared suffix (`…@mail.example.com`), a dense
numeric tail (`key_000000001`), plain decimal integers, UUIDs, Cyrillic, DNA, and filesystem paths.

<sub>Committed output:
[`bench/results/hash-quality-2026-11-04-arz-8ca5a3c-dirty.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-11-04-arz-8ca5a3c-dirty.txt)
(5.0's 128-bit state: avalanche worst 4.53 over 88 064 cells, bit independence worst 4.82 over
1 806 336, against this battery's bound today of `|z| < 6.5`; no double collision, no slot collision,
per-corpus largest 1.61), and
[`bench/results/hash-quality-2026-09-16-arz-166ac74.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-09-16-arz-166ac74.txt)
(the 4.0 hash with its derived fingerprint; the run before the fingerprint was derived is
[`hash-quality-2026-09-16-arz-67650b3.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/hash-quality-2026-09-16-arz-67650b3.txt),
and the 2.0–3.x hash's run of the same battery is
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
| `ClosedHashIndex` | 0.24 | 11.4 | 11.5 | 11.4 | 12.4 | 11.5 – 11.9 |
| `CompactHashIndex` fp=1 | 1.24 | 13.7 | 13.8 | 13.7 | 14.3 | 13.8 – 13.9 |
| `PerfectHashIndex` | 9.25 | 41.5 | 43.5 | 41.6 | 47.3 | 41.8 – 44.6 |
| `StringIndex` | 5.95 | 139.8 | 140.9 | 140.0 | 157.9 | 140.5 – 142.1 |
| `DictIndex` 256 | 2.64 | 252.3 | 252.7 | 252.4 | 254.5 | 252.6 – 253.0 |
| `DictIndex` 512 | 2.52 | 278.7 | 279.4 | 278.7 | 284.5 | 279.1 – 279.6 |

**The minimum is within 5 % of the median everywhere**, and the p5–p95 band is 1–13 % of it, so
quoting the minimum costs nothing — same probe set, same binary, thirty consecutive passes. An
earlier session of the same harness put the band under 50 ns at 45 % and the minimum 25–29 % under
the median while the slow lanes kept their 2–10 %: a fast lane's width is the machine's that day,
not the structure's, and the interval column is what says which day it was.

**The probe set is part of the working set.** Only the number of probes a pass changes here:

| probes a pass | `Closed` | `Compact` fp=1 | `Perfect` | `String` | `Dict` 256 | `Dict` 512 |
|---|---:|---:|---:|---:|---:|---:|
| 100 000 | 9.7 | 14.4 | 40.7 | 139.1 | 251.2 | 278.9 |
| 200 000 | 11.5 | 13.8 | 43.5 | 140.9 | 252.7 | 279.4 |
| 400 000 | 24.7 | 24.9 | 52.9 | 172.9 | 273.5 | 303.2 |

`ClosedHashIndex` goes 2.5×; `DictIndex` 256 moves 9 %. The probe array is a second structure the loop
walks: 100 000 `String` headers are 2.4 MB, 400 000 are 9.6 MB before the bytes they point at, and
past some point between the two the probe stops being in cache and starts being fetched at the same
cost as the lookup it pays for. The smaller the index, the larger the share of the total that is.
The counters say it in one line — **the instruction count does not move and the cycle count does**:

| structure | instr | cycles at 100 k | at 400 k | branch misses | L1 fills | LLC misses |
|---|---:|---:|---:|---:|---:|---:|
| `ClosedHashIndex` | 100 | 40.2 | 72.6 | 0.06 | 2.36 | 1.16–1.27 |
| `CompactHashIndex` fp=1 | 112 | 61.5 | 91.2 | 0.06 | 3.85 | 2.03–2.22 |
| `PerfectHashIndex` | 154 | 124.2 | 194.1 | 0.70 | 4.93 | 3.86–4.01 |
| `StringIndex` | 1011 | 591.1 | 730.5 | 4.07 | 14.57 | 7.03–7.57 |
| `DictIndex` 256 | 2190 | 1092.1 | 1177.2 | 15.18 | 17.82 | 3.82–4.32 |
| `DictIndex` 512 | 2588 | 1216.6 | 1290.1 | 17.77 | 16.67 | 3.57–3.96 |

Three things fall out of that table that no nanosecond showed:

- **`DictIndex` does the most work and misses about half of what `StringIndex` does, and neither is
  what it waits for.** 2 190 instructions at 256 against `StringIndex`'s 1 011, and
  3.8–4.3 LLC misses against 7.0–7.6. On 2026-09-12 the one-level format at 128 keys a block spent 3 226 instructions and
  missed 6.6 a lookup; the microblock cut both by a fifth and a third, and at equal size the
  latency did not move — because what a lookup waits for is a chain of dependent misses, the sample
  search then the restart run then the microblock, which is the same length whatever the scan, and
  the instructions and the streamed misses of the scan overlap under it. IPC 1.9–2.1 says the core
  is busy; the chain says with what. It is also why a block of 32 keys and one of 512 sit about a
  quarter apart on `id` and not 2.9× apart on the entries they scan.
- **`PerfectHashIndex` is pure latency.** 154 instructions, 3.9–4.0 LLC misses, IPC 0.8–1.2: it
  does almost no work and waits for all of it.
- **`StringIndex` misses the most, 7.0–7.6 a lookup.** That is the same fact as its thread scaling
  above: dependent misses are what a transducer walk is made of, and they are also what another core
  can overlap.

A second mechanism arrives between 200 000 and 400 000 probes, on the counters of 2026-09-19, which
the recount did not read again. Data-TLB misses a lookup go from
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

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/stats-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/stats-2026-11-03-arz-82f2cb1.txt),
which carries all thirty samples of every row;
counters [`statperf-100-100000-2026-11-03-arz-82f2cb1.csv`](https://github.com/ilgrad/lexindex/blob/main/bench/results/statperf-100-100000-2026-11-03-arz-82f2cb1.csv) and [`statperf-100-400000-2026-11-03-arz-82f2cb1.csv`](https://github.com/ilgrad/lexindex/blob/main/bench/results/statperf-100-400000-2026-11-03-arz-82f2cb1.csv)),
Ryzen 7 5800HS. Counters are `perf stat` over
100 passes minus a build-only control, so the build's own cycles and faults stay out of the lookup's
— at 20 passes a 30 ns lane's difference sat inside the build's own run-to-run noise;
in those runs every structure is built but only one is probed, which is the *kinder* case — the
table above has six rotating through the cache. The counter columns read 100 000 probes, and LLC is
the range over 100 000 and 400 000; the table of 2026-09-19
([`stats-2026-09-19-arz-a3f7503.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/stats-2026-09-19-arz-a3f7503.txt)) read 200 000 and three
counts.</sub>

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
is a filter with no `id` to time, and libCSD and IBiS are not in that harness yet. The
[standalone suite](https://github.com/ilgrad/string-index-benchmarks) has libCSD's sizes in
`sib baselines`, and its times are still owed.

## The research frontier, measured

The C² paper ships one benchmark that builds its three cache-conscious structures — C²-FST, C²-CoCo
and C²-MARISA — beside everything it measures them against: FST, CoCo-trie, MARISA, PDT, ART and
C-ART. `bench/frontier/build.sh` builds that benchmark with every dependency at a pinned commit, and
XCDAT's four trie types beside it; `bench/frontier/run.sh` runs all of them against `DictIndex` at
blocks of 32, 256 and 1024 keys — each once as a load leaves it and once routed by 4.4's
`route_microblocks()` — `HashedDictIndex` at three widths, `StringIndex` and 4.5's
`DoubleArrayIndex`, over the corpus set above at a million keys and, where a corpus has them, ten
million. Nothing of the competitors is vendored or linked: it is fetched into a gitignored
directory, which is what lets the campaign include GPLv3 and non-commercial code.

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
lexindex — for a routed `DictIndex`, the blob and the restart words it holds beside it, which no
file carries — `space_cost()` for the benchmark's structures, `memory_in_bytes` for XCDAT. ART and
C-ART count their nodes and not the keys they point to, so they are kept out of every comparison. ρ
is the recursion depth `benchmark.cpp` takes as `max_recursion` — for MARISA, one less than its
number of tries. A structure is on the *front* when no other one is at least as small and at least
as fast.

<!-- table: frontier bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `words-full` | 2.64 @ 235 | lexindex Dict 1024 2.51 @ 292 | lexindex HashedDict closed 5.25 @ 14 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `dna-1000000` | 4.37 @ 303 | lexindex Dict 1024 4.23 @ 327 | lexindex HashedDict closed 7.12 @ 42 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `domains-1000000` | 4.50 @ 304 | lexindex Dict 1024 4.36 @ 359 | lexindex HashedDict closed 7.24 @ 29 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `idents-1000000` | 5.23 @ 361 | lexindex Dict 1024 5.07 @ 425 | lexindex HashedDict closed 7.97 @ 38 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `numeric-1000000` | 1.01 @ 237 | lexindex StringIndex 0.0003 @ 52 | lexindex HashedDict closed 3.75 @ 14 | HashedDict closed, StringIndex |
| `opaque-1000000` | 10.40 @ 322 | lexindex Dict 1024 10.29 @ 354 | lexindex HashedDict closed 13.14 @ 46 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `paths-1000000` | 9.87 @ 687 | lexindex Dict 1024 9.32 @ 733 | lexindex HashedDict closed 12.61 @ 75 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `pypi-full` | 4.17 @ 310 | lexindex Dict 1024 4.02 @ 366 | lexindex HashedDict closed 6.91 @ 28 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `titles-en-1000000` | 7.37 @ 379 | lexindex Dict 1024 7.21 @ 436 | lexindex HashedDict closed 10.11 @ 42 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `titles-ru-1000000` | 6.86 @ 458 | lexindex Dict 1024 6.70 @ 510 | lexindex HashedDict closed 9.60 @ 51 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `titles-zh-1000000` | 5.85 @ 364 | lexindex Dict 1024 5.69 @ 433 | lexindex HashedDict closed 8.59 @ 34 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `urls-1000000` | 7.51 @ 433 | lexindex Dict 1024 7.27 @ 486 | lexindex HashedDict closed 10.25 @ 58 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `uuid-1000000` | 18.04 @ 403 | lexindex Dict 1024 17.89 @ 427 | lexindex HashedDict closed 20.78 @ 55 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
<!-- /table -->

<sub>A million keys, measured 2026-11-03 at `82f2cb1`, the code after 4.5.3
([`bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json)),
Ryzen 7 5800HS, GCC 16.2.1, rustc 1.99.1, on the `performance` governor. A cell reads bytes a key @
nanoseconds a lookup. Its competitors are pinned as they were in the 2026-09-25 campaign at
`6969644`, on `powersave`, and they are the control:
all 260 of their cells come back at byte-identical sizes and read 0.87–1.07 of it, median 0.96, with
each family's median at 0.93–0.98 and MARISA's at 1.02. That is what the governor moved, and a
lexindex row has to move further than that to have moved. `DictIndex` reads medians of 0.96–0.98 of
that campaign at the same sizes, and `HashedDictIndex` closed 0.93–1.04 (median 0.99).
`StringIndex` reads 0.62–1.01 (median 0.94), the low end `numeric`, 84 → 52 ns. `DoubleArrayIndex`'s
cold pass is the one row that rose; see below. Other work while the 1 247 processes behind the table
ran: median 0.02 busy CPUs, most 0.26.</sub>

lexindex is on the front on all thirteen corpora — `DictIndex` on twelve, `StringIndex` on
`numeric`, `HashedDictIndex` on every one — and `DictIndex` builds before every other structure on
all thirteen, ahead of the fastest build from elsewhere by 1.8× on `words`, 1.6× on `titles-en`,
1.9× on `numeric` and 2.2–7.7× on the rest. It is the smallest structure the campaign times on
twelve of them, by 1.5 % on `paths` up to 31 % on `dna`: 2.51 bytes a key at block 1024 on `words`
against MARISA's 2.98 at ρ=2, 4.23 on `dna` against CoCo-trie's 6.14, 6.70 on `titles-ru` against
8.61, 17.89 on `uuid` against PDT's 21.44 from a 16.3-second build. `paths` is both the thinnest
margin and the cell that turned over in 4.0: block 1024's 9.32 against MARISA's 9.47 at ρ=2, which
reads 3.1 times slower and builds 3.8 times slower. MARISA at its smallest, which the campaign does not time, takes it back: 8.81 bytes a key at
23 tries under the tiny cache ([below](#marisa-at-its-smallest)).
Only `numeric` keeps a trie ahead of `DictIndex` on size, and there the trie is beside the point:
CoCo-trie's 0.52 against block 1024's 0.92, while `StringIndex` folds a million decimal ids into
about 300 bytes. The fastest structure on every corpus is `HashedDictIndex` closed — `id_unchecked`,
which takes a key's membership on trust ([below](#hasheddictindex-against-xcdat)) — at 14–75 ns a
lookup, 3.7–8.1 times as fast as XCDAT 15. XCDAT 15 is the fastest structure from elsewhere on all
thirteen corpora, ART aside, which does not count its keys and reads 178 ns on `opaque` against
198; XCDAT 15 is at 1.9–7.0 times the bytes of `DictIndex` at its default block, and on the front of
none. C²-MARISA at ρ=2 is the one structure from elsewhere on
any front, on `titles-ru` and `titles-zh`, always larger — 9.14 bytes a key on `titles-ru` against
6.70. At 4.3.3 it shared six: on `words`, `domains`, `idents` and `pypi` a routed `DictIndex` is now
smaller and faster than every depth of it that was on the front there. `DoubleArrayIndex` is on no
front: it holds six of the thirteen corpora, and on each of them `HashedDictIndex` closed is both
smaller and faster.

**Where this loses, and by how much.** Size is the axis `DictIndex` is built to win and latency is
the axis it pays on. The table puts lexindex's exact searches — the ones that answer a stranger
exactly, which a hash does not — beside XCDAT 15: `DictIndex` at its default block as a load leaves
it, the three blocks routed, `StringIndex`, and `DoubleArrayIndex` where it can hold the corpus.

<!-- table: frontier bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json view=search -->
| corpus | XCDAT 15 | `Dict` 256 | 32 routed | 256 routed | 1024 routed | `StringIndex` | `DoubleArray` | `PerfectHash`, hash-order ids |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `words-full` | 7.30 @ 76 | 2.64 @ 235 | 3.35 @ 178 | 3.14 @ 169 | 2.76 @ 189 | 5.95 @ 127 | 24.41 @ 85 | 9.25 @ 29 |
| `dna-1000000` | 22.46 @ 273 | 4.37 @ 303 | 5.50 @ 292 | 4.88 @ 254 | 4.48 @ 261 | 17.74 @ 406 | — | 17.75 @ 78 |
| `domains-1000000` | 10.33 @ 141 | 4.50 @ 304 | 5.32 @ 257 | 5.00 @ 229 | 4.61 @ 252 | 10.50 @ 201 | 65.16 @ 590 | 15.45 @ 61 |
| `idents-1000000` | 13.78 @ 194 | 5.23 @ 361 | 6.12 @ 313 | 5.73 @ 296 | 5.32 @ 332 | 10.55 @ 231 | 64.40 @ 749 | 16.60 @ 70 |
| `numeric-1000000` | 7.05 @ 52 | 1.01 @ 237 | 1.72 @ 193 | 1.51 @ 174 | 1.17 @ 173 | 0.0003 @ 52 | 8.00 @ 42 | 4.12 @ 27 |
| `opaque-1000000` | 20.02 @ 198 | 10.40 @ 322 | 11.14 @ 304 | 10.90 @ 279 | 10.54 @ 293 | 21.44 @ 285 | — | 16.49 @ 80 |
| `paths-1000000` | 25.11 @ 606 | 9.87 @ 687 | 13.49 @ 653 | 10.37 @ 668 | 9.57 @ 729 | 17.48 @ 774 | — | 123.56 @ 158 |
| `pypi-full` | 9.92 @ 132 | 4.17 @ 310 | 4.98 @ 261 | 4.67 @ 239 | 4.27 @ 280 | 10.00 @ 191 | 48.68 @ 459 | 13.05 @ 52 |
| `titles-en-1000000` | 17.13 @ 234 | 7.37 @ 379 | 8.31 @ 331 | 7.87 @ 313 | 7.46 @ 341 | 17.28 @ 313 | — | 22.85 @ 80 |
| `titles-ru-1000000` | 23.08 @ 352 | 6.86 @ 458 | 7.80 @ 412 | 7.36 @ 399 | 6.95 @ 420 | 31.75 @ 535 | — | 33.81 @ 89 |
| `titles-zh-1000000` | 13.58 @ 185 | 5.85 @ 364 | 6.61 @ 322 | 6.35 @ 292 | 5.94 @ 326 | 17.52 @ 263 | 33.15 @ 235 | 18.24 @ 75 |
| `urls-1000000` | 18.14 @ 348 | 7.51 @ 433 | 9.32 @ 390 | 8.01 @ 366 | 7.52 @ 395 | 16.88 @ 412 | — | 54.63 @ 93 |
| `uuid-1000000` | 38.88 @ 326 | 18.04 @ 403 | 19.10 @ 379 | 18.54 @ 347 | 18.14 @ 354 | 37.11 @ 410 | — | 34.97 @ 85 |
<!-- /table -->

<sub>From the same campaign
([`bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json)).
A routed cell's bytes are the blob's and the restart words' together — 0.5 a key at blocks 32 and
256, 0.25 at 1024 — which no file holds and every process that routes pays in memory. A
`DoubleArray` cell is a dash where the corpus's trie needs more slots than 23-bit bases
address. The `PerfectHash` column is new in this campaign: it answers a stranger exactly too, but in
hash-order ids, so it is not the contract of the sorted searches and no count below includes it. It
is the fastest exact answer on all thirteen, at 0.58× (`numeric`) to 4.9× (`paths`) XCDAT 15's
bytes.</sub>

At a million keys lexindex's fastest exact search beats XCDAT 15 on two corpora of thirteen.
`DoubleArrayIndex` takes `numeric`, 42 ns against 52 at 8.00 bytes a key against 7.05, and block 256
routed takes `dna`, 254 ns against 273 at 4.88 bytes a key against 22.46. `words` reads 1.12: it
was `DoubleArrayIndex`'s at 4.5.2, 74 against 78, and this table's cell is 85 against 76. A cell is
the single cold pass, and `DoubleArrayIndex`'s three cold runs read 72.3, 84.7 and 85.1 against
XCDAT 15's 75.3–77.8, while its warm pass in the same runs is faster than on 2026-09-25 (60.7 → 50.2
ns). Why the cold pass rose as the warm pass fell is not traced, so this table's reading on `words`
is neither a win nor a regression. The same split shows on `numeric` (cold 29.0–29.7 → 38.0–47.1,
warm 22.5 → 19.9) and, smaller, on `domains` and `pypi`; a run of 4.5.3 against this commit holds
`DoubleArrayIndex` level
([`rel-sentinels-2026-11-03-arz-82f2cb1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rel-sentinels-2026-11-03-arz-82f2cb1.txt)).
The rest trail by 5–13 % on `urls`, `uuid`, `paths` and `titles-ru`, routed at block 256 each time
but on `paths`, where block 32 routed is faster; and by 1.19× to 1.45× on the other six —
`DoubleArrayIndex` on `titles-zh`, 235 ns against 185, block 256 routed on `titles-en` and `opaque`,
and `StringIndex` on `idents`, `domains` and `pypi`. Against ART, which reads faster than XCDAT 15
on `opaque` alone and does not count its keys, `opaque` is 1.57×. `DoubleArrayIndex` is the fast end
only where its trie is small: the other seven corpora would need more than 2²³ slots, which its
23-bit bases cannot address, and on the long keys of `domains`, `idents` and `pypi`, at 49–65 bytes
a key, it is the slowest exact search lexindex has. Routing is most of the rest: at 4.3.3 the
fastest `DictIndex` row read 1.2× to 3.0× behind XCDAT 15 on the twelve corpora other than `numeric`,
and now it reads 0.93× to 2.24× there — block 256 routed the fastest `DictIndex` on eleven, and
13–28 % faster than the same block unrouted on those, a comparison across processes. `paths` is the
corpus it cannot help: its restart keys tie in the eight bytes the words hold, and block 256 routed
reads 668 ns where block 256 reads 687. What moved there is 4.4.1's tie trie, which places a probe
among blocks whose samples tie — 3 899 of 3 907 at block 256 — by a trie of their eight-byte words,
and which 4.5.1 keeps for runs of eight or more: on `powersave`, block 256 read 794 ns in 4.4.0's
campaign and 699 in 4.5.2's, 4.4.1's in-process A/B put it at 768 → 704, and the fastest `DictIndex`,
block 32 routed, is 1.08× behind XCDAT where it was 1.20×.
The rest of the bill is structural.
Unrouted, a probe walks a block's restarts to one microblock and scans it — `block / micro + micro
− 2` header decodes, 62 at block 1024 — and routed it still scans the microblock, where a
double-array trie takes one indexed load per byte of the key and decodes nothing. Front coding buys
its bytes by making a comparison cost work, and that is the bill; routing pays part of it back in
memory, which costs the size lead where it was thinnest — block 256 routed is larger than MARISA at
ρ=2 on `words`, `domains`, `idents` and `pypi`, and every routed block is on `paths`.
`HashedDictIndex`, which answers `id` without the search, does not pay the bill at all; what it
costs instead is [measured against XCDAT below](#hasheddictindex-against-xcdat).

The bill shrinks with `n`, and then it stops shrinking. Unrouted, the same ratio is 3.1× on `words`
at half a million keys, 1.6× on `titles-en` at a million and 1.14× on `titles-en` at ten million;
with 4.0's `DictIndex`, on `powersave`, which is what the campaigns past ten million keys ran, it is
1.17× on all 19.2 million English titles, while `urls` goes from 1.07× at ten million to
1.08× whole. At a hundred million keys 4.0's `DictIndex` is 1.21× behind on `uuid` (1.24× at ten
million) and 1.59× on `opaque` (1.72×); `dna` goes from a 9 % lead at ten million to a tie, 844 ns
against 864 and inside what memory placement alone moves a lookup on this machine; and `StringIndex`
answers `numeric` in 156 ns against 408. This paragraph used to end on a prediction: that past the
last level of cache a blob a third the size takes a third of the misses, so the `dna` crossing would
generalise at a hundred million keys. That campaign has run, and it did not. What the numbers fit
instead is one step — from a million keys, where a structure this size still sits largely in the 16
MiB last-level cache, to ten million, where every one of them pays DRAM misses — after which the
ratio is set by how many dependent misses a probe takes, not by how many bytes it could touch. That
is a reading, not a measurement; a miss profile of both lookup paths is what would settle it. The
claim for the rows above is the measured one: **smallest on every corpus but `paths`, where MARISA at
its smallest, untimed and last counted at 4.5.2, is 5.8 % smaller, and on the front of all of them;
at ten million keys, routed, faster than XCDAT 15 on `numeric`, `dna`, `urls` and `titles-en`, level
on `uuid` and 1.36× behind on `opaque`; at a hundred million, with 4.0 and no routing,
faster on `numeric`, level on `dna` and 1.2× and 1.6× behind on `uuid` and `opaque`.** The 19.2 M
and 100 M numbers are the standalone benchmark suite's, at its `a6c13d0` with lexindex 4.0.0 from
crates.io:
[`frontier-full-2026-09-21-arz-a6c13d0.json`](https://github.com/ilgrad/string-index-benchmarks/blob/main/results/frontier-full-2026-09-21-arz-a6c13d0.json)
runs every structure over both corpora whole, and
[`frontier-100m-2026-09-21-arz-a6c13d0.json`](https://github.com/ilgrad/string-index-benchmarks/blob/main/results/frontier-100m-2026-09-21-arz-a6c13d0.json)
runs only `DictIndex` at blocks 256 and 1024, `StringIndex` and XCDAT 15, since the full set at a
hundred million keys would run for two days.

<!-- table: frontier bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `dna-10000000` | 3.95 @ 488 | lexindex Dict 1024 3.81 @ 474 | lexindex HashedDict closed 7.19 @ 59 | Dict 1024, Dict 1024 routed, HashedDict closed |
| `numeric-10000000` | 1.02 @ 291 | lexindex StringIndex 3.6e-05 @ 61 | lexindex HashedDict closed 4.25 @ 36 | HashedDict closed, StringIndex |
| `opaque-10000000` | 10.05 @ 561 | lexindex Dict 1024 9.95 @ 542 | lexindex HashedDict closed 13.29 @ 70 | Dict 1024, Dict 1024 routed, HashedDict closed |
| `titles-en-10000000` | 5.54 @ 636 | lexindex Dict 1024 5.37 @ 670 | lexindex HashedDict closed 8.78 @ 65 | Dict 1024, Dict 1024 routed, Dict 256, Dict 256 routed, HashedDict closed |
| `urls-10000000` | 5.70 @ 709 | lexindex Dict 1024 5.44 @ 736 | lexindex HashedDict closed 8.93 @ 78 | Dict 1024, Dict 1024 routed, HashedDict closed |
| `uuid-10000000` | 17.56 @ 646 | lexindex Dict 1024 17.42 @ 605 | lexindex HashedDict closed 20.80 @ 77 | Dict 1024, Dict 1024 routed, Dict 256 routed, HashedDict closed |
<!-- /table -->

<sub>Ten million keys, measured 2026-11-03 at `82f2cb1`
([`bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json)),
on the `performance` governor, with two hours allowed a process and none reaching it. This is the
first campaign at this scale with every lexindex row beside every competitor. The competitors read
0.89–1.16 of the 2026-09-20 campaign at `7e42c43`, on `powersave`, median 0.98, MARISA's
median 1.04, every size byte-identical. Other work while the 548 processes ran: median 0.01 busy
CPUs, most 0.32.</sub>

At ten times the keys lexindex is on the front of all six corpora, and nothing from elsewhere is on
any of them: `HashedDictIndex` closed on all six, `DictIndex` on five, and `StringIndex` on
`numeric`, where it is the smallest and the fastest exact search. `DictIndex` is the smallest
structure on the other five: `dna` at 3.81 bytes a key at block 1024 against C²-CoCo's 5.98 at ρ=1,
`opaque` 9.95 against PDT's 13.73 and `uuid` 17.42 against PDT's 20.59 — builds of 111 and 125
seconds against 0.9 and 1.2 — and, by a narrower 5.9 % and 6.8 %, `titles-en` and `urls` against
MARISA at ρ=2. On `dna` block 1024 as a load leaves it reads 474 ns to XCDAT 15's 533, so it is
smaller and faster than everything from elsewhere. `DictIndex` builds before every one of them
again, in 0.5–2.3 s, where the next is MARISA at 2.9–11.4 times that (PDT, at 2.4 times, on
`numeric`), C²'s structures take 10–31 s and PDT up to 125 s. Only `numeric` keeps a trie smaller
than `DictIndex`: CoCo-trie's 0.51 bytes a key against block 1024's 0.92, and neither is near
`StringIndex`. `urls`, the corpus with the longest keys, is
below whole.

<!-- table: frontier bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json corpus=urls-10000000 -->
**`urls-10000000`** — 10,000,000 keys, 52.37 bytes a key raw

| structure | bytes/key | % of raw | build ms | `id` ns | spread | front |
|---|---:|---:|---:|---:|---:|:---:|
| lexindex Dict 32 | 7.03 | 13.4 | 2283 | 745 | 0 % |  |
| lexindex Dict 256 | 5.70 | 10.9 | 2320 | 709 | 3 % |  |
| lexindex Dict 1024 | 5.44 | 10.4 | 2278 | 736 | 1 % | ● |
| lexindex Dict 32 routed | 7.53 | 14.4 | 2313 | 692 | 3 % |  |
| lexindex Dict 256 routed | 6.20 | 11.8 | 2311 | 614 | 2 % |  |
| lexindex Dict 1024 routed | 5.69 | 10.9 | 2268 | 613 | 1 % | ● |
| lexindex StringIndex | 13.10 | 25.0 | 8798 | 721 | 6 % |  |
| lexindex HashedDict closed | 8.93 | 17.1 | 3433 | 78 | 1 % | ● |
| lexindex HashedDict fp=8 | 9.93 | 19.0 | 3418 | 98 | 1 % |  |
| lexindex HashedDict fp=16 | 10.93 | 20.9 | 3470 | 99 | 2 % |  |
| lexindex DoubleArray | — | — | — | — | — | refused: format error: double-array: more than 8 388 608 keys; ids are 23 bits |
| lexindex PerfectHash (ids in hash order) | 52.89 | 101.0 | 4252 | 132 | 2 % |  |
| C²-FST | 9.41 | 18.0 | 11884 | 1511 | 2 % |  |
| C²-FST ρ=1 | 8.20 | 15.7 | 13966 | 1788 | 1 % |  |
| C²-FST ρ=2 | 8.20 | 15.7 | 14396 | 1792 | 0 % |  |
| C²-CoCo | 9.92 | 18.9 | 28079 | 1527 | 1 % |  |
| C²-CoCo ρ=1 | 8.71 | 16.6 | 30025 | 1749 | 0 % |  |
| C²-CoCo ρ=2 | 8.71 | 16.6 | 30650 | 1750 | 3 % |  |
| C²-MARISA | 8.34 | 15.9 | 11833 | 872 | 1 % |  |
| C²-MARISA ρ=1 | 6.92 | 13.2 | 14188 | 1157 | 1 % |  |
| C²-MARISA ρ=2 | 6.92 | 13.2 | 14802 | 1154 | 1 % |  |
| FST | 9.54 | 18.2 | 15433 | 1917 | 1 % |  |
| CoCo | — | — | — | — | — | aborted: std::bad_alloc |
| MARISA | 8.66 | 16.5 | 10820 | 1155 | 3 % |  |
| MARISA ρ=1 | 6.23 | 11.9 | 12364 | 1568 | 2 % |  |
| MARISA ρ=2 | 5.83 | 11.1 | 12638 | 1688 | 2 % |  |
| PDT | 7.34 | 14.0 | 26890 | 1136 | 7 % |  |
| ART | 35.80 | 68.4 | 3509 | 838 | 15 % | ref |
| C-ART | 16.85 | 32.2 | 4201 | 956 | 6 % | ref |
| XCDAT 7 | 12.37 | 23.6 | 12628 | 856 | 1 % |  |
| XCDAT 8 | 11.96 | 22.8 | 12288 | 812 | 4 % |  |
| XCDAT 15 | 14.75 | 28.2 | 12637 | 684 | 4 % |  |
| XCDAT 16 | 15.43 | 29.5 | 12290 | 739 | 2 % |  |
<!-- /table -->

<sub>From the same artifact
([`bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json)).
`spread` is the range of a cell's three rounds over their median, and `ref` marks the two structures
that do not count their keys. Three rows are on the front: block 1024, the smallest structure here
at 5.44 bytes a key against MARISA's 5.83 at ρ=2; block 1024 routed, 613 ns at 5.69 bytes against
XCDAT 15's 684 at 14.75; and `HashedDictIndex` closed, 78 ns at 8.93. `bench/frontier/tables.py`
prints this table for every corpus at both scales from the logs.</sub>

These are the same campaign's exact searches:

<!-- table: frontier bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json view=search -->
| corpus | XCDAT 15 | `Dict` 256 | 32 routed | 256 routed | 1024 routed | `StringIndex` | `DoubleArray` | `PerfectHash`, hash-order ids |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna-10000000` | 16.00 @ 533 | 3.95 @ 488 | 5.08 @ 588 | 4.45 @ 437 | 4.06 @ 414 | 15.98 @ 688 | — | 16.47 @ 113 |
| `numeric-10000000` | 7.06 @ 197 | 1.02 @ 291 | 1.76 @ 325 | 1.52 @ 253 | 1.17 @ 219 | 3.6e-05 @ 61 | — | 4.61 @ 76 |
| `opaque-10000000` | 21.66 @ 320 | 10.05 @ 561 | 10.79 @ 524 | 10.55 @ 459 | 10.20 @ 436 | 21.47 @ 468 | — | 16.33 @ 116 |
| `titles-en-10000000` | 13.95 @ 559 | 5.54 @ 636 | 6.52 @ 597 | 6.04 @ 534 | 5.62 @ 556 | 13.25 @ 622 | — | 21.59 @ 114 |
| `urls-10000000` | 14.75 @ 684 | 5.70 @ 709 | 7.53 @ 692 | 6.20 @ 614 | 5.69 @ 613 | 13.10 @ 721 | — | 52.89 @ 132 |
| `uuid-10000000` | 38.58 @ 540 | 17.56 @ 646 | 18.64 @ 625 | 18.06 @ 541 | 17.67 @ 551 | 36.07 @ 530 | — | 34.64 @ 118 |
<!-- /table -->

<sub>From the same campaign
([`bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json)).
The `DoubleArray` column is empty because the format stops at 8 388 608 keys: an id takes 23 bits.
XCDAT 15 reads 0.98–1.03 of 4.4's subset of 2026-09-24 at `3a73ed5`, every `DictIndex` row 0.95–1.05,
`StringIndex` 0.97–1.06 but on `numeric`, 95 → 61 ns. Sizes match it to the byte on `dna`, `numeric`,
`opaque` and `uuid`; on `titles-en` and `urls` a `DictIndex` is 1.9 % and 1.5 % smaller, block 256 at
5.54 and 5.70 where it was 5.65 and 5.78, since a build past about 2.6 million keys mines its
phrases from more of its sample.</sub>

Routed, the exact search is ahead of XCDAT 15 on four corpora of six at ten million keys: `numeric`,
where `StringIndex` reads 61 ns against 197 and holds all ten million keys in 356 bytes; `dna`,
block 1024 routed at 414 against 533; `urls`, block 1024 routed at 613 against 684; and `titles-en`,
block 256 routed at 534 against 559, though XCDAT 15's three rounds there read 534, 559 and 561. On
`uuid` it is level: `StringIndex` 530 and block 256 routed 541 against 540. `opaque` is the loss:
436 ns for block 1024 routed against 320, 1.36×. ART and C-ART, which do not count their keys, read
faster than XCDAT 15 on three of the six, and against them `numeric` is 0.44 (ART's 140 ns), `uuid`
1.05 (C-ART's 505) and `opaque` 1.73 (ART's 251). Unrouted, block 256 is ahead only on `dna` and
trails by 1.04–1.75× on the other five. Routing keeps the size lead on every one of the five that a
`DictIndex` holds — block 1024 routed is still under every structure from elsewhere — and it is
thinnest on `titles-en`, 5.62 bytes a key against MARISA's 5.71 at ρ=2, where 4.4's 5.73 was over
it.

Not every structure runs everywhere, and a cell that failed says why instead of carrying a number.
C²'s three structures crash with a segmentation fault on `numeric` at every depth and both scales,
and on `dna` at ten million keys at ρ=2. CoCo-trie stops with `Sequence is not sorted` on the three
title corpora at a million keys, and runs out of the 28 GB of address space each process is allowed
on `uuid` at a million and on every corpus but `numeric` at ten million. `DoubleArrayIndex` refuses
seven corpora at a million keys and all six at ten million, and says which limit it hit.

The campaign puts the rest of the frontier beside `marisa-trie`, and what it finds there has moved:
no compressed trie here builds faster than blocks of front-coded keys, and none the campaign times
is smaller either, bar CoCo-trie on `numeric`, where `StringIndex` holds the whole corpus in 301
bytes at a million keys and 356 at ten million and beats both by three to four orders of magnitude.
A block still shares only with its own neighbours, so where long fragments repeat across the whole
key set — URLs, titles, paths — the lead is narrowest: 5.9 % on `titles-en` and 6.8 % on `urls` at
ten million keys, and 1.5 % over MARISA at ρ=2 on `paths` at a million, the cell 4.0 turned over.
Against MARISA at its smallest, last counted at 4.5.2, those were 0.11 %, 0.30 % and a loss
([below](#marisa-at-its-smallest)).
Where C²'s cache-conscious MARISA reads faster than `DictIndex` as a load leaves it, on nine of the
twelve corpora it runs at a million keys, it is larger. Routed, `DictIndex` reads faster than it on
seven of those nine; the two left, `titles-ru` and `titles-zh`, are the two it shares the front on.
On `titles-ru` `HashedDictIndex` closed reads 51 ns against its 374, at 9.60 bytes a key against
9.14.

### MARISA at its smallest

The campaign builds MARISA as `benchmark.cpp` does, at ρ of 0 to 2 — one to three tries — under the
default cache, and that is not marisa's smallest trie: at the same depth the tiny cache is smaller on
every corpus here, and most go on shrinking past three tries. The
[standalone benchmark suite](https://github.com/ilgrad/string-index-benchmarks#marisa-at-its-smallest)
builds the pinned marisa at 1 to 32 tries under both caches and keeps the smallest file, and counts
what each loaded index holds on the heap beside its file. Three of this repository's corpus files —
`paths`, `idents` and `pypi` — are not the suite's, so the suite's binaries were run over these
files. Nothing is timed: marisa's own documentation says a deeper recursion "degrades the search
performance", so these configurations have no lookup column and no place on a front.

<!-- table: marisa-floor bench/results/marisa-floor-1m-2026-09-26-arz-f5a486e.json -->
| corpus | lexindex, smallest | MARISA, smallest | margin | mapped: lexindex | mapped: MARISA | margin |
|---|---|---|---:|---:|---:|---:|
| `words-full` | **2.51** (Dict 1024) | 2.96 (4 tries, tiny) | +15.0 % | **2.56** | 2.96 | +13.5 % |
| `dna-1000000` | **4.23** (Dict 1024) | 7.52 (3 tries, tiny) | +43.8 % | **4.25** | 7.53 | +43.5 % |
| `domains-1000000` | **4.36** (Dict 1024) | 4.80 (8 tries, tiny) | +9.0 % | **4.43** | 4.80 | +7.7 % |
| `idents-1000000` | **5.07** (Dict 1024) | 5.30 (11 tries, tiny) | +4.4 % | **5.15** | 5.31 | +3.1 % |
| `numeric-1000000` | **301 B** (StringIndex) | 1.62 (1 try, tiny) | +99.981 % | **333 B** | 1.62 | +99.980 % |
| `opaque-1000000` | **10.29** (Dict 1024) | 15.55 (6 tries, tiny) | +33.8 % | **10.31** | 15.55 | +33.7 % |
| `paths-1000000` | 9.32 (Dict 1024) | **8.81** (23 tries, tiny) | -5.8 % | 9.54 | **8.84** | -7.9 % |
| `pypi-full` | **4.02** (Dict 1024) | 4.39 (9 tries, tiny) | +8.3 % | **4.09** | 4.40 | +6.9 % |
| `titles-en-1000000` | **7.21** (Dict 1024) | 7.49 (15 tries, tiny) | +3.8 % | **7.34** | 7.51 | +2.3 % |
| `titles-ru-1000000` | **6.70** (Dict 1024) | 7.61 (19 tries, tiny) | +12.0 % | **6.95** | 7.63 | +9.0 % |
| `titles-zh-1000000` | **5.69** (Dict 1024) | 6.22 (12 tries, tiny) | +8.5 % | **6.02** | 6.23 | +3.4 % |
| `urls-1000000` | **7.27** (Dict 1024) | 7.57 (20 tries, tiny) | +3.9 % | **7.40** | 7.59 | +2.5 % |
| `uuid-1000000` | **17.89** (Dict 1024) | 22.98 (10 tries, tiny) | +22.1 % | **17.94** | 22.99 | +22.0 % |
<!-- /table -->

<sub>A million keys, 2026-09-26, sizes only, not re-measured on 2026-11-03: the artifact names no
governor, and a size does not depend on one
([`bench/results/marisa-floor-1m-2026-09-26-arz-f5a486e.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/marisa-floor-1m-2026-09-26-arz-f5a486e.json)):
the suite's `marisa_floor`, `marisa_resident` and `resident` at its `156b1a7`, marisa at `e54f296`,
lexindex 4.5.2 from crates.io, every blob at a million keys the size the campaign above logged. A MARISA size is its
file, `io_size()`, 220 to 270 bytes a trie more than the `space_cost()` the campaign counts.
*Mapped* adds the heap a load keeps after a pass of lookups, counted as the bytes its allocations
asked for: the tables `load_mmap` derives beside a `DictIndex` blob, and what marisa's `Trie::mmap`
keeps.</sub>

Against its smallest, MARISA takes `paths`: 8.81 bytes a key at 23 tries under the tiny cache,
against block 1024's 9.32 — 5.8 % smaller, and 7.9 % once both are mapped. It is the one corpus here
where anything from elsewhere is smaller than lexindex's smallest index. On the other twelve the
margins narrow and hold: by file from 3.8 % on `titles-en`; mapped, where a `DictIndex` also holds
the 0.02–0.33 bytes a key it derives at load — the most on `titles-zh` and `titles-ru`, whose
character-code tables are the largest — against MARISA's 1–26 KB, from 2.3 % on `titles-en`. At ten
million keys this repository's six corpus files are the suite's, byte for byte, as are its two
19.2-million-key files, so the suite's artifacts answer for them
([`resident-10m`](https://github.com/ilgrad/string-index-benchmarks/blob/main/results/resident-10m-2026-09-26-arz-156b1a7.json),
[`resident-full`](https://github.com/ilgrad/string-index-benchmarks/blob/main/results/resident-full-2026-09-26-arz-156b1a7.json)): `DictIndex` is 0.11 %
smaller than MARISA at its smallest on `titles-en` and 0.30 % on `urls` by file, and mapped MARISA
is 1.8 % and 1.5 % smaller; at 19.2 million, 1.0 % and 1.3 % by file, and mapped MARISA 0.7 % and
0.4 %. Those two ten-million margins are 4.5.2's: a `DictIndex` on `titles-en` and `urls` is 1.9 % and
1.5 % smaller since, and the floor has not been counted against it. `dna`, `opaque` and `uuid` keep
15–43 % either way, and `numeric` is `StringIndex`'s, in a few
hundred bytes.

### `HashedDictIndex` against XCDAT

`HashedDictIndex` (4.1) keeps a `DictIndex` whole and answers `id` without searching it: a minimal
perfect hash over the keys' hashes and a bit-packed table holding each key's rank at its slot, so a
lookup is two hashes of the key and two reads ([the design](design.md#hasheddictindex)). The
campaign runs it at three widths: closed, which is `id_unchecked` at zero fingerprint bits, and `id`
checking eight and sixteen bits. Its size is the whole blob, the dictionary included, and its build
is both, from the keys. At a million keys it is part of the full campaign above, and the first table
below takes its rows from that run, beside `Dict` 256, the dictionary it is built over, and
XCDAT 15, the fastest structure from elsewhere that counts its keys on every corpus. Before that campaign it had run at a million keys only in a subset run of
2026-09-22 ([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-subset-2026-09-22-arz-8e04b0b.json)),
against which XCDAT 15 read 0.97–1.05 in the 4.3.3 campaign and the sidecar's own cells 0.94–1.14 —
the widest on `pypi` at sixteen bits, 32 → 36 ns, where a nanosecond is 3 % — the 4.4 campaign reads
the sidecar at 0.93–1.10 of 4.3.3's, and 4.5.2's at 0.89–1.11 of 4.4's, 4.5.1's faster perfect hash.
The campaign of 2026-11-03 reads closed at 0.93–1.04 of 4.5.2's, median 0.99.

At ten million keys it ran only in subsets until 2026-11-03, on the campaign's protocol — a process
a structure and corpus, three rounds, every other one in reverse order. The first ran on 2026-09-22
beside `Dict` 256 and XCDAT 15 alone, twice: on the working tree that added the index
([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-subset-2026-09-22-arz-41478d0-dirty.json)), then on the committed code
after a reboot ([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-subset-2026-09-22-arz-8e04b0b.json)); 4.4's of 2026-09-24
put every lexindex row beside XCDAT 15
([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-subset-2026-09-24-arz-3a73ed5.json)). The second table below is the full
campaign's. Against 4.4's subset XCDAT 15 reads 0.98–1.03 and `HashedDict` closed 0.93–1.01. Every
ratio below is taken inside one run.

<!-- table: frontier bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json view=hashed -->
| corpus | XCDAT 15 | `Dict` 256 | `HashedDict` closed | fp=8 | fp=16 |
|---|---:|---:|---:|---:|---:|
| `words-full` | 7.30 @ 76 | 2.64 @ 235 | 5.25 @ 14 | 6.25 @ 17 | 7.25 @ 18 |
| `dna-1000000` | 22.46 @ 273 | 4.37 @ 303 | 7.12 @ 42 | 8.12 @ 57 | 9.12 @ 58 |
| `domains-1000000` | 10.33 @ 141 | 4.50 @ 304 | 7.24 @ 29 | 8.24 @ 34 | 9.24 @ 33 |
| `idents-1000000` | 13.78 @ 194 | 5.23 @ 361 | 7.97 @ 38 | 8.97 @ 45 | 9.97 @ 50 |
| `numeric-1000000` | 7.05 @ 52 | 1.01 @ 237 | 3.75 @ 14 | 4.75 @ 18 | 5.75 @ 24 |
| `opaque-1000000` | 20.02 @ 198 | 10.40 @ 322 | 13.14 @ 46 | 14.14 @ 56 | 15.14 @ 56 |
| `paths-1000000` | 25.11 @ 606 | 9.87 @ 687 | 12.61 @ 75 | 13.61 @ 104 | 14.61 @ 106 |
| `pypi-full` | 9.92 @ 132 | 4.17 @ 310 | 6.91 @ 28 | 7.91 @ 34 | 8.91 @ 35 |
| `titles-en-1000000` | 17.13 @ 234 | 7.37 @ 379 | 10.11 @ 42 | 11.11 @ 49 | 12.11 @ 51 |
| `titles-ru-1000000` | 23.08 @ 352 | 6.86 @ 458 | 9.60 @ 51 | 10.60 @ 63 | 11.60 @ 62 |
| `titles-zh-1000000` | 13.58 @ 185 | 5.85 @ 364 | 8.59 @ 34 | 9.59 @ 41 | 10.59 @ 43 |
| `urls-1000000` | 18.14 @ 348 | 7.51 @ 433 | 10.25 @ 58 | 11.25 @ 69 | 12.25 @ 72 |
| `uuid-1000000` | 38.88 @ 326 | 18.04 @ 403 | 20.78 @ 55 | 21.78 @ 67 | 22.78 @ 67 |
<!-- /table -->

<sub>A million keys, from the full campaign measured 2026-11-03 at `82f2cb1`
([`bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json)).
A cell reads bytes a key @ nanoseconds a lookup, the median of three rounds.</sub>

<!-- table: frontier bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json view=hashed -->
| corpus | XCDAT 15 | `Dict` 256 | `HashedDict` closed | fp=8 | fp=16 |
|---|---:|---:|---:|---:|---:|
| `dna-10000000` | 16.00 @ 533 | 3.95 @ 488 | 7.19 @ 59 | 8.19 @ 77 | 9.19 @ 78 |
| `numeric-10000000` | 7.06 @ 197 | 1.02 @ 291 | 4.25 @ 36 | 5.25 @ 50 | 6.25 @ 52 |
| `opaque-10000000` | 21.66 @ 320 | 10.05 @ 561 | 13.29 @ 70 | 14.29 @ 76 | 15.29 @ 76 |
| `titles-en-10000000` | 13.95 @ 559 | 5.54 @ 636 | 8.78 @ 65 | 9.78 @ 72 | 10.78 @ 73 |
| `urls-10000000` | 14.75 @ 684 | 5.70 @ 709 | 8.93 @ 78 | 9.93 @ 98 | 10.93 @ 99 |
| `uuid-10000000` | 38.58 @ 540 | 17.56 @ 646 | 20.80 @ 77 | 21.80 @ 96 | 22.80 @ 97 |
<!-- /table -->

<sub>Ten million keys, from the full campaign measured 2026-11-03 at `82f2cb1`
([`bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json)).
Other work while the 548 processes ran: median 0.01 busy CPUs, most 0.32.</sub>

**On all nineteen, `HashedDictIndex` is both faster than XCDAT and smaller.** Closed,
`id_unchecked` is 3.7× faster (`numeric` at a million keys) to 9.1× (`dna` at ten million), median
5.5×, at 1.39× (`words`) to 3.16× (`dna` at a million) less space; `id` at eight fingerprint bits is
2.8–7.8× faster and 1.17–2.77× smaller; at sixteen, 2.1–7.6× faster, and the size lead narrows to
0.7 % on `words`, 7.25 bytes a key against 7.30. At all three widths it is faster than every
structure either campaign measured, the searches that beat XCDAT included: on `numeric` at a million
keys 14 ns against `DoubleArrayIndex`'s 42, on `dna` at ten million 59 against block 1024 routed's
414, on `numeric` there 36 against `StringIndex`'s 61. Against the search of the dictionary it
carries, the hash is 7.1–17.3× faster closed and 5.3–13.8× at eight bits — 6.1–12.4× and 4.5–9.9×
against the same dictionary routed — for 2.6–3.2 bytes a key more: 2.62 on `words`, 2.74 at a
million keys and 3.24 at ten million, where a rank takes 24 bits. And it builds before XCDAT on every
corpus, from 1.29× on `words` to 6.5× on `dna` at ten million, in 1.28–2.68× the time of the
dictionary alone.

**A loop whose lookups can overlap flatters a hash.** Those ratios are the cold pass, a free loop in
which the next lookup starts before the last one's misses return. In the three chained passes these
rows time after it, each query's slot waits on the answer before it: the latency a dependent caller
sees. XCDAT 15 reads 0.99–1.08 of its cold pass there and `Dict` 256 0.95–1.00, and `HashedDict`
closed 1.6–3.2× its own. It stays ahead on all nineteen, by less: XCDAT 15 is 1.74× (`opaque` at ten
million) to 3.84× (`titles-en` at ten million) behind closed, median 2.8×, 1.68–3.71× behind eight
bits and 1.64–3.60× behind sixteen; `Dict` 256 is 2.57–8.3× behind closed.

<!-- table: frontier bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json view=chained -->
| corpus, chained | XCDAT 15 | `Dict` 256 | `HashedDict` closed | fp=8 | fp=16 |
|---|---:|---:|---:|---:|---:|
| `words-full` | 7.30 @ 77 | 2.64 @ 234 | 5.25 @ 28 | 6.25 @ 30 | 7.25 @ 31 |
| `dna-1000000` | 22.46 @ 272 | 4.37 @ 299 | 7.12 @ 116 | 8.12 @ 124 | 9.12 @ 127 |
| `domains-1000000` | 10.33 @ 139 | 4.50 @ 289 | 7.24 @ 48 | 8.24 @ 52 | 9.24 @ 47 |
| `idents-1000000` | 13.78 @ 202 | 5.23 @ 354 | 7.97 @ 70 | 8.97 @ 70 | 9.97 @ 74 |
| `numeric-1000000` | 7.05 @ 56 | 1.01 @ 237 | 3.75 @ 29 | 4.75 @ 33 | 5.75 @ 33 |
| `opaque-1000000` | 20.02 @ 203 | 10.40 @ 317 | 13.14 @ 115 | 14.14 @ 117 | 15.14 @ 122 |
| `paths-1000000` | 25.11 @ 625 | 9.87 @ 665 | 12.61 @ 164 | 13.61 @ 176 | 14.61 @ 180 |
| `pypi-full` | 9.92 @ 137 | 4.17 @ 295 | 6.91 @ 46 | 7.91 @ 49 | 8.91 @ 50 |
| `titles-en-1000000` | 17.13 @ 233 | 7.37 @ 375 | 10.11 @ 88 | 11.11 @ 88 | 12.11 @ 97 |
| `titles-ru-1000000` | 23.08 @ 350 | 6.86 @ 450 | 9.60 @ 125 | 10.60 @ 130 | 11.60 @ 129 |
| `titles-zh-1000000` | 13.58 @ 185 | 5.85 @ 357 | 8.59 @ 64 | 9.59 @ 69 | 10.59 @ 66 |
| `urls-1000000` | 18.14 @ 357 | 7.51 @ 424 | 10.25 @ 142 | 11.25 @ 142 | 12.25 @ 152 |
| `uuid-1000000` | 38.88 @ 329 | 18.04 @ 400 | 20.78 @ 136 | 21.78 @ 140 | 22.78 @ 141 |
<!-- /table -->

<sub>A million keys ([`bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-11-03-arz-82f2cb1.json)): bytes a key @ the median of the rounds' chained
means.</sub>

<!-- table: frontier bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json view=chained -->
| corpus, chained | XCDAT 15 | `Dict` 256 | `HashedDict` closed | fp=8 | fp=16 |
|---|---:|---:|---:|---:|---:|
| `dna-10000000` | 16.00 @ 543 | 3.95 @ 488 | 7.19 @ 188 | 8.19 @ 196 | 9.19 @ 201 |
| `numeric-10000000` | 7.06 @ 197 | 1.02 @ 291 | 4.25 @ 83 | 5.25 @ 92 | 6.25 @ 98 |
| `opaque-10000000` | 21.66 @ 325 | 10.05 @ 555 | 13.29 @ 186 | 14.29 @ 194 | 15.29 @ 198 |
| `titles-en-10000000` | 13.95 @ 605 | 5.54 @ 633 | 8.78 @ 157 | 9.78 @ 163 | 10.78 @ 168 |
| `urls-10000000` | 14.75 @ 722 | 5.70 @ 706 | 8.93 @ 204 | 9.93 @ 211 | 10.93 @ 216 |
| `uuid-10000000` | 38.58 @ 565 | 17.56 @ 646 | 20.80 @ 200 | 21.80 @ 204 | 22.80 @ 208 |
<!-- /table -->

<sub>Ten million keys ([`bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-11-03-arz-82f2cb1.json)), the same.</sub>

**Its build needs no more memory than the dictionary's.** The sidecar holds a 16-byte hash pair a
key and the table, and builds after the dictionary has freed its own buffers, so the dictionary's
build is the high-water mark. Measured on the thirteen corpora at a million keys and six at ten
million (`cargo run --release --example peak -- hashed 0 <bits>` with `LEXINDEX_PEAK_KEYS` naming
the corpus file, one process a cell), the build adds 88–191 bytes a key over the key list at a
million keys and 31–73 at ten million, and at ten million every width's peak is its dictionary
phase's to the byte. At a million keys the sidecar raises the peak on nine corpora of thirteen, by
0.1–5.1 bytes a key, the most on `titles-zh`; the same dictionary built in two processes reads up
to 8 bytes a key apart, so the gap is the allocator's size, not the sidecar's
([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/peak-hashed-2026-10-28-arz-8c92679.txt)).

**What it does not do.** XCDAT answers a stranger exactly. `HashedDictIndex` does only through the
dictionary's search — `id` at zero bits, the `Dict` 256 column, which `dict().route_microblocks()`
routes like any other `DictIndex` — so for a stranger the exact searches earlier in this section are
the comparison. Unrouted, XCDAT leads by 1.11–4.6× at a million keys and 1.04–1.75× at ten million on
five corpora of six, `dna` there the one cell that turns over (0.91×); routed at block 256, the
dictionary is ahead on `dna` at both scales and on `urls` and `titles-en` at ten million, level on
`uuid` there, and 1.05–3.4× behind everywhere else. With fingerprint bits, `id` answers a stranger
as present at `2^-bits` — 0.4 % at eight, 0.0015 % at sixteen, `CompactHashIndex`'s rate — and with
none, `id_unchecked` gives it some rank below `n`. The sidecar is also bytes the dictionary does not
spend: where size is the only column, `DictIndex` is still the answer, and the smallest structure on
every corpus above but `numeric`, which `StringIndex` holds.

## Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.24 bytes/key holds constant** as
`n` grows. Each row is measured twice: handing the constructor a **list** of keys, and handing it a
**generator**. The second is what `CompactHashIndex`'s streaming build exists for — it keeps a
16-byte pair per key and drops the string — and it is the only way to see the index's own footprint
rather than the corpus's:

| n | structure | keys | build | bytes/key | peak RSS | lookup |
|---|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | list | 0.32 s | 0.68\* | 155 MB | 137 ns |
| 1 M | `StringIndex` | generator | 0.46 s | 0.68\* | 148 MB | 136 ns |
| 1 M | `CompactHashIndex` | list | 0.08 s | 1.24 | 146 MB | 65 ns |
| 1 M | `CompactHashIndex` | **generator** | 0.19 s | 1.24 | **79 MB** | 69 ns |
| 10 M | `StringIndex` | list | 4.2 s | 2.00\* | 1109 MB | 679 ns |
| 10 M | `StringIndex` | generator | 5.7 s | 2.00\* | 1033 MB | 514 ns |
| 10 M | `CompactHashIndex` | list | 0.57 s | 1.24 | 953 MB | 116 ns |
| 10 M | `CompactHashIndex` | **generator** | 1.7 s | 1.24 | **267 MB** | 296 ns |

<sub>Measured 2026-11-03 at `82f2cb1`, on `performance`
([`bench/results/scale-2026-11-03-arz-82f2cb1.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/scale-2026-11-03-arz-82f2cb1.json)), one
process per cell and the **minimum of five** per cell, at load 0.33 and 0.93 at its two ends.
Against 2026-09-19 at `4ee80d2`
([artifact](https://github.com/ilgrad/lexindex/blob/main/bench/results/scale-2026-09-19-arz-4ee80d2.json)), `StringIndex`'s build is the control and reads
back within its noise: 0.33 → 0.32 and 0.48 → 0.46 s at 1 M, 4.3 → 4.2 and 5.9 → 5.7 s at 10 M. Its
lookup is no control now, since 4.3 changed the reader. `CompactHashIndex`'s builds read 0.08, 0.20 →
0.19 s at 1 M and 0.58 → 0.57, 1.8 → 1.7 s at 10 M, and its blob stays **1.24 B/key**. Lookups at 1 M
read 189 → 137 and 72 → 65 ns. **The lookup column at 10 M is not a measurement of the structures**:
the same `CompactHashIndex` blob's five samples read 116–406 ns from the list, median 351, and
296–390 from the generator, and `StringIndex`'s 679–763 and 514–749, so a minimum of five says little
there and 116 against 2026-09-19's 210 is not an improvement. Why they scatter is not traced. The
generator build's peak at 10 M is 267 MB against the corpus-holding build's 953 — that gap, not the
blob, is what the streaming constructor exists for.</sub>

