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

<!-- table: compare bench/results/compare-2026-09-19-arz-999e933.json columns=prefix,common_prefix,range,fuzzy,reverse,exact,mmap -->
| library | prefix | common prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** | **ns/lookup** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|---:|---:|
| **lexindex `ClosedHashIndex`** | — | — | — | — | — | none (closed vocabulary) | — | **0.24** | 98 |
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | — | probabilistic | ✅ | **0.74** | 92 |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | — | probabilistic | ✅ | **1.24** | **85** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | — | probabilistic | ✅ | **2.24** | 92 |
| **lexindex `DictIndex` (512 per block)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.53** | 415 |
| **lexindex `DictIndex` (256 per block, default)** | ✅ | ✅ | ✅ | — | ✅ | ✅ | ✅ | **2.65** | 388 |
| `marisa-trie` (4 tries, tiny cache — its smallest here) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.96 | 490 |
| `marisa-trie` (default) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 | 473 |
| `marisa-trie` (huge cache) | ✅ | ✅ | — | — | ✅ | ✅ | ✅ | 3.07 | 446 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 | 310 |
| lexindex `PerfectHashIndex` | — | — | — | — | ✅ | ✅ | ✅ | 10.88 | 173 |
| DAWG (`dawg2`) | ✅ | ✅ | — | — | — | ✅ | — | 23.96 | 242 |
| `datrie` | ✅ | ✅ | — | — | — | ✅ | — | 30.91 | 589 |
| builtin `dict` | — | — | — | — | — | ✅ | — | — (in RAM only) | 241 |
<!-- /table -->

<sub>Every cell above is one run at `999e933` in a clean worktree, on a machine whose load
average went 0.11 into the run and 0.91 out of it
([`bench/results/compare-2026-09-19-arz-999e933.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-19-arz-999e933.json))
— every cell's build and lookup samples, the false-positive measurement, the CPU, kernel, rustc,
Python and the load average at both ends of the run. **Its Python call floor is 51 ns against the
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
at either block** — 2.53 B/key and 415 ns at 512, 2.65 and 388 at the default 256, against
2.96–3.07 and 446–490.

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
| **lexindex `DictIndex`** (512 per block) | **2.531** | 0.85× | 344 ns | 30 ms |
| **lexindex `DictIndex`** (256 per block, default) | **2.651** | 0.89× | 310 ns | 31 ms |
| **lexindex `DictIndex`** (64 per block) | **2.752** | 0.92× | **269 ns** | **34 ms** |
| `marisa-trie` (C++ reference, default) | 2.978 | 1.00× | — | — |
| `rsmarisa` (4 tries, tiny cache — its smallest) | 3.003 | 1.01× | 326 ns | 138 ms |
| `rsmarisa` 0.4.2 (default) | 3.168 | 1.06× | 300 ns | 148 ms |
| `rsmarisa` (3 tries, huge cache — its fastest) | 3.823 | 1.28× | 284 ns | 169 ms |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.63× | — | — |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | 5.95 | 2.00× | — | — |
| `yada` (double-array) | 15.98 | 5.4× | — | — |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× | — | — |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× | — | — |

<sub>`rsmarisa` and `DictIndex` from
[`bench/results/rsmarisa-2026-09-19-arz-30638ba.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-2026-09-19-arz-30638ba.txt)
— one process, every key probed as a member and with a digit appended as a miss, shuffled with a
fixed seed, seven rounds alternating in both directions, minimum of the last five; three such
processes, the table quoting the minimum of the three, which agree within 4 %. The other rows are
the older sweep with `crawdad` 0.4, `yada` 0.5, `fst` 0.4; size is serialised bytes ÷ keys
throughout, and `rsmarisa`'s `io_size()` is asserted equal to the file it saves. A fourth process,
run seconds after a `git commit`, read 8–20 % slower on **every** row including `rsmarisa`'s and was
discarded on that evidence rather than on its shape — which is the whole reason this page insists on
a control that the change under test cannot touch. None of these crates is a lexindex dependency:
the harness is a throwaway.</sub>

So the statement is a crown after all, with the frontier behind it. **`DictIndex` at its default
block dominates `rsmarisa` at its most compact setting outright** — 2.651 against 3.003 B/key, 310
against 326 ns, 4.5× faster to build — and at 64 per block it is the fastest structure here at 269
ns for 2.752, under `rsmarisa`'s *smallest* setting on both axes at once. Since `BDX3` every block
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
the bare-MPHF row is a `ClosedHashIndex` built over the same three key sets in the same session):

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
| `dna` | 24.0 | 0.24 | 1.24 | 4.49 | 4.40 | **4.24** | 17.74 | 7.52 | 7.52 | 7.52 | 7.56 | 7.71 |
| `domains` | 13.8 | 0.24 | 1.24 | 4.81 | 4.75 | **4.59** | 10.50 | 4.81 | 4.80 | 4.80 | 4.87 | 4.99 |
| `idents` | 17.3 | 0.24 | 1.24 | 5.37 | 5.30 | **5.13** | 10.55 | 5.45 | 5.34 | 5.30 | 5.62 | 5.74 |
| `numeric` | 5.9 | 0.24 | 1.24 | 1.07 | 1.03 | **0.93** | 0.00 | 1.62 | 1.62 | 1.62 | 1.64 | 1.72 |
| `opaque` | 16.0 | 0.24 | 1.24 | 10.46 | 10.42 | **10.30** | 21.44 | 18.23 | 15.55 | 15.55 | 18.82 | 19.04 |
| `paths` | 125.0 | 0.24 | 1.24 | 10.63 | 10.18 | 9.61 | 17.48 | 9.26 | 9.04 | **8.83** | 9.47 | 9.60 |
| `titles-en` | 21.0 | 0.24 | 1.24 | 7.47 | 7.40 | **7.23** | 17.28 | 7.79 | 7.52 | 7.49 | 8.19 | 8.37 |
| `titles-ru` | 35.8 | 0.24 | 1.24 | 7.95 | 7.81 | **7.57** | 31.75 | 8.07 | 7.67 | 7.61 | 8.61 | 8.77 |
| `titles-zh` | 16.9 | 0.24 | 1.24 | 6.35 | 6.29 | **6.11** | 17.52 | 6.33 | 6.22 | 6.22 | 6.54 | 6.70 |
| `urls` | 52.4 | 0.24 | 1.24 | 7.73 | 7.53 | **7.28** | 16.88 | 7.95 | 7.61 | 7.57 | 8.39 | 8.58 |
| `uuid` | 36.0 | 0.24 | 1.24 | 18.15 | 18.06 | **17.90** | 37.11 | 32.93 | 22.99 | 22.98 | 34.58 | 34.80 |

<sub>The bold cell in a row is the smallest structure that keeps its keys. `words` and `pypi` have
no million-key file; their 100 000 grid, every build time, and the 100 000 rows for the rest are in
[`bench/results/sweep-2026-09-19-arz-a3f7503.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep-2026-09-19-arz-a3f7503.json)
— one run of the whole set, 264 cells, on a clean tree that began at a load average of
0.12 / 0.37 / 0.69. Every column comes from that one run, the three `DictIndex` blocks included, so
this table needs no reconciliation between harnesses.</sub>

**`DictIndex` is the smallest key-storing structure on ten of these eleven corpora**, measured
against marisa's *best* setting on each and not its default. The margin runs from 0.5 % on
`titles-ru` (7.57 against 7.61) and 2–4 % on `titles-zh`, `idents`, `titles-en`, `urls` and
`domains`, to 28 % on `uuid`, 51 % on `opaque`, 74 % on `numeric` and 77 % on `dna`. The eleventh is
`paths`, where a million paths run through a few thousand directories and marisa at sixteen tries
holds 8.83 against 9.61 — 9 % — which is what a LOUDS trie is for and what front coding in fixed
blocks still does not answer. Through 3.0.0 this table read the other way round, marisa smallest on
nine of the eleven; the `BDX3` codec is what turned it.

**Its floor is eight or sixteen tries on nine of the eleven, not the four this page quoted through
3.0.0.** The recursion keeps paying wherever the keys share — `titles-ru` 8.07 → 7.61, `urls`
7.95 → 7.57, `paths` 9.26 → 8.83, about 5 % each — and it pays most where they share least: `opaque`
18.23 → 15.55 and `uuid` 32.93 → 22.98, 15 % and **30 %** of the blob. The two corpora where four
tries is not beaten are `dna` and `numeric`, where all three settings agree to the hundredth; on
`domains` the gain is 0.3 % and on `titles-zh` 1.8 %, so "tune it" is worth measuring and not worth
assuming. What the tries cost is time, and the cost is where the gain is: `uuid` builds 1.37× and
answers 1.65× slower at sixteen tries than at four, `paths` answers 1.30× slower, and on the corpora
where the setting buys nothing the two are level. Marisa's smallest configuration is its slowest.

**The margin is still widest where the keys share least.** `DictIndex` leads by half a blob on
`opaque` (10.30 against 15.55) and by more than that on `dna` and `numeric` — keys with nothing to
share, where a trie pays for a node per character and front coding pays for a prefix that is not
there. Where the keys do share, the lead is a couple of per cent and a tuned marisa is the thing to
measure against: on `titles-ru`, `urls` and `titles-en` the two are within 4 % of each other, and
`paths` is still marisa's. On `words`, the corpus every table above is measured on, `DictIndex` at
its default block is 3.35 against marisa's 3.70 at 100 000 keys and 2.65 against 2.96 on the full
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
| `dna` | 24.0 | 77 | 117 | 391 | 379 | 382 | 586 | 890 | 905 | 951 | 904 | 922 |
| `domains` | 13.8 | 71 | 118 | 431 | 447 | 498 | 437 | 731 | 707 | 697 | 700 | 687 |
| `idents` | 17.3 | 75 | 119 | 480 | 490 | 539 | 474 | 934 | 959 | 992 | 893 | 829 |
| `numeric` | 5.9 | 61 | 92 | 346 | 346 | 369 | 176 | 290 | 285 | 288 | 280 | 257 |
| `opaque` | 16.0 | 97 | 121 | 379 | 387 | 420 | 436 | 1452 | 1520 | 1499 | 1156 | 1094 |
| `paths` | 125.0 | 179 | 205 | 1080 | 1068 | 1160 | 1777 | 4147 | 5112 | 4834 | 3701 | 3096 |
| `titles-en` | 21.0 | 98 | 118 | 506 | 501 | 547 | 594 | 1132 | 1250 | 1208 | 1104 | 1059 |
| `titles-ru` | 35.8 | 132 | 168 | 617 | 610 | 623 | 899 | 1607 | 1702 | 1681 | 1523 | 1499 |
| `titles-zh` | 16.9 | 103 | 130 | 503 | 508 | 561 | 518 | 993 | 1044 | 1027 | 964 | 907 |
| `urls` | 52.4 | 89 | 123 | 517 | 515 | 568 | 752 | 1191 | 1325 | 1471 | 1161 | 1098 |
| `uuid` | 36.0 | 111 | 124 | 449 | 446 | 491 | 559 | 1676 | 2618 | 2769 | 1267 | 1202 |

**`DictIndex` answers faster than every `marisa-trie` setting on ten of the eleven corpora** —
1.5–2.9× at its default block, against marisa's *fastest* configuration and not its smallest. The
eleventh is `numeric`, where marisa's fastest is 26 % ahead of the default block (257 ns against
346) and `StringIndex` ahead of both at 176: a million dense decimal ids are the corpus where a trie
has the least to walk. `DictIndex` also builds 1.3–2.9× faster than marisa's quickest build
everywhere except `numeric`, where marisa is 1.2× ahead. So the size table above is not the whole
trade, and the settings that make marisa smallest are the ones that make it slowest: on `paths`,
the one corpus it still wins on size, its sixteen-try floor is 1.09× smaller than `DictIndex` at
1024 a block, 4.2× slower to answer and 2.2× slower to build.

**The block is a smaller knob than it was.** A lookup scans one restart a microblock and then one
microblock whatever the block, so 128 → 1024 keys a block moves a lookup by −2 % (`dna`) to +16 %
(`domains`), where the one-level format paid 3.8× from 32 to 1024, and buys 0.14–1.02 bytes a key
over 128, every corpus in the same direction. It did not use to be: `paths` at 1024 keys a block
stored *more* than at 256 (16.13 against 15.89), and the cause was the symbol table rather than the
layout, whose per-block arrays only shrink as the block grows. The table trained on whole blocks, so
a larger block bought fewer neighbourhoods — 19 at 1024 against 157 at 128 — and a table trained on
19 of them is a lottery. A training run is now a constant 256 keys whatever the block, trained per
shard of 65 536 keys, and `paths` at 1024 reads 9.61. 256 is the default the README quotes.

<sub>Measured 2026-09-19 at `a3f7503`, from the same artifact as the table above. Lookups are one
run of three rounds of 20 000 probes a cell, so a column is comparable within itself and a cell
carries a few per cent: a second run of the same build read the same bytes to the last digit and
lookups 1–15 % higher on every structure, controls included.</sub>

### Ten million keys

| corpus | raw | `ClosedHash` | `CompactHash` | `Dict` 128 | `Dict` 256 | `Dict` 1024 | `String` | marisa 4 | marisa 8 | marisa 16 | marisa def. | marisa fast |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `dna` | 24.0 | 0.24 | 1.24 | 4.06 | 3.97 | **3.82** | 15.98 | 6.69 | 6.69 | 6.69 | 6.75 | 6.99 |
| `numeric` | 6.9 | 0.24 | 1.24 | 1.07 | 1.04 | **0.93** | 0.00 | 1.63 | 1.63 | 1.63 | 1.66 | 1.77 |
| `opaque` | 16.0 | 0.24 | 1.24 | 10.11 | 10.07 | **9.96** | 21.47 | 15.19 | 14.72 | 14.72 | 18.33 | 18.68 |
| `titles-en` | 21.0 | 0.24 | 1.24 | 5.89 | 5.82 | 5.64 | 13.25 | 5.57 | 5.49 | **5.48** | 5.71 | 5.87 |
| `urls` | 52.4 | 0.24 | 1.24 | 6.06 | 5.86 | 5.59 | 13.10 | 5.66 | 5.56 | **5.55** | 5.83 | 5.99 |
| `uuid` | 36.0 | 0.24 | 1.24 | 17.67 | 17.58 | **17.42** | 36.07 | 29.97 | 20.62 | 20.62 | 33.21 | 33.56 |

Ten times the keys moves every trie and neither hash: at its floor `marisa` goes 7.49 → 5.48 on
`titles-en` and 7.57 → 5.55 on `urls` as the sharing deepens, `DictIndex` at its default block
7.40 → 5.82 and 7.53 → 5.86, and the two keyless rows do not move at all. Here the ranking is
*not* the million-key table's: `titles-en` and `urls` are two of the ten corpora `DictIndex` is
smallest on at a million, and at ten million the trie takes both back — by 2.8 % and 0.7 %. Which
is smallest is decided by the corpus *and* by the scale, and the two corpora it turns on are the
ones whose keys share long fragments across the whole set rather than with their neighbours.

**Scale narrows every gap in the trie's favour.** `DictIndex` keeps four of the six at ten
million — `dna` 3.82 against marisa's tuned 6.69 and `numeric` 0.93 against 1.63, both 43 %,
`opaque` 9.96 against 14.72 and `uuid` 17.42 against 20.62, 32 % and 16 % — but every one of those
margins is narrower than the same corpus gives at a million, `uuid` most of all, 22 % there against
16 % here. The reason is the trie's, not ours: ten times the random identifiers share ten times more
three- and four-character fragments, and a recursive trie is built to find exactly that, while a
block of front-coded keys shares only with its own block. Read the margins as gaps that narrow with
n, and do not build a claim on a single one of them.

**The block buys less here.** 128 → 1024 keys a block is 0.14–0.47 bytes a key over these six,
against 0.14–1.02 at a million. Ten times the keys is ten times the blocks, so the per-block arrays
a bigger block saves were already a smaller share of the index; what is left is the coded suffixes,
and those are the symbol table's business, not the block's — which is why the table is trained on
runs of a constant length and per shard. With whole-block training over one table a bigger block
bought fewer neighbourhoods to train on, and `titles-en` at 1024 came out above its own 256.

<sub>Measured 2026-09-19 at `31375b3`
([`bench/results/sweep10m-2026-09-19-arz-31375b3.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/sweep10m-2026-09-19-arz-31375b3.json)),
`marisa-trie` 1.4.1, one build per cell at this size and three rounds of 20 000 lookups, started at
a load average of 0.04 / 0.45 / 0.69. Every marisa cell reads to the hundredth what the 2026-09-13
run read — a size is exact, so the two runs agree wherever the builder did not move, and what moved
is ours.</sub>

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
| `dna` | 7.61 | **4.26** | 1 669 ms | 119 ms | 834 ns | 326 ns (512) |
| `domains` | 4.87 | **4.60** | 903 ms | 248 ms | 578 ns | 413 ns (64) |
| `idents` | 5.50 | **5.14** | 800 ms | 283 ms | 828 ns | 459 ns (512) |
| `numeric` | 1.64 | **0.93** | 115 ms | 90 ms | **208 ns** | 325 ns (512) |
| `opaque` | 18.27 | **10.31** | 1 894 ms | 137 ms | 1 477 ns | 344 ns (512) |
| `paths` | 10.07 | **9.73** | 6 550 ms | 589 ms | 2 243 ns | 831 ns (512) |
| `titles-en` | 7.85 | **7.25** | 1 287 ms | 458 ms | 1 009 ns | 460 ns (512) |
| `titles-ru` | 8.22 | **7.60** | 1 873 ms | 522 ms | 1 339 ns | 508 ns (512) |
| `titles-zh` | 6.40 | **6.13** | 989 ms | 352 ms | 869 ns | 466 ns (512) |
| `urls` | 8.38 | **7.32** | 2 489 ms | 460 ms | 1 134 ns | 539 ns (512) |
| `uuid` | 33.01 | **17.93** | 2 387 ms | 238 ms | 1 583 ns | 502 ns (512) |

`BDX3` changed the shape of this table. Through 3.0.0 `rsmarisa` was the smaller structure on
**nine** of these eleven corpora — everything but `opaque` and `uuid`, the two whose keys share
nothing for a trie to fold. It is now **larger on all eleven**: 3.5 % on `paths` and 4.4 % on
`titles-zh` at the narrow end, and 77–84 % on `dna`, `opaque`, decimal ids and UUIDs at the wide
one, while `DictIndex` builds
**1.3–14× faster** and answers **1.4–4.3× faster** on ten of the eleven. `numeric` is the one
exception left, and only on latency: `rsmarisa` answers it in 208 ns against 325, having lost the
size there (1.64 against 0.93) and the build (115 ms against 90). One thing the right-hand column shows that
the word list cannot: on ten of eleven corpora the *largest* block is now the fastest, because the
eight-byte samples stop settling the binary search over the block heads and every step of it reads
a head, so fewer blocks are fewer dependent misses, and that outweighs a scan ten entries longer.
`domains`, whose keys are short enough for the samples to keep deciding, is the one that still
wants 64. At
nominally the same configuration `rsmarisa` is larger than the C++ marisa measured above — on
`words`, 1.6 % at the smallest setting, 6.4 % at the default and 24 % at the fastest — so the flag
words evidently do not mean quite the same thing, and each library's own curve is what to read.
**"Smallest" here is the smallest of its three cache levels at the default number of tries, not its
floor**: the sweep above shows the C++ library bottoming out at eight or sixteen tries on nine of
these corpora, by 30 % on `uuid`, and this harness does not turn that knob. Read the left column as
one point on a curve whose other end is not measured.

<sub>Measured 2026-09-19 on a clean tree at `0b42720`
([`bench/results/rsmarisa-corpora-2026-09-19-arz-0b42720.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/rsmarisa-corpora-2026-09-19-arz-0b42720.txt)),
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
Measured 2026-09-19 at `b9d84e8` in a clean worktree: six runs of the example back to back, each
lookup cell the minimum of five timed passes after a warm-up pass
([`bench/results/latency-rs-2026-09-19-arz-b9d84e8.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/latency-rs-2026-09-19-arz-b9d84e8.txt)).
The table quotes the minimum over all six. The machine was **not** rebooted first — the v3.0.0 run
was, and its first two runs read 18–44 % quicker on every row, which is a part holding its boost
clock while it is cold rather than eight structures improving at once — and without that head start
the six agree within 1 % on every row but `StringIndex`, which alternates between 318 and 373 ns
with no trend, a 17 % spread no other row shows. Absolute numbers are machine-dependent — the
`std::HashMap` control reads 241 ns here against the 285 of the v3.0.0 table, the 295 of 2.1.0, the
289 of 2.0.0 and the 245 of 1.1.0 — so compare the **ratios**, only within a column, and read a
shift under ~15 % between tables as the session: against this `HashMap`, `CompactHashIndex::id` is
0.25×, `id_unchecked` 0.21×, `PerfectHashIndex::id` 0.50×, `StringIndex` 1.32×, `DictIndex` 1.93×,
`BTreeMap` 3.01×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `PerfectHashIndex::id_unchecked` | ~248 ms | **~51 ns** | closed vocabulary, no membership check |
| lexindex `CompactHashIndex::id` (fp=1) | **~36 ms** | ~61 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id` (verified) | ~243 ms | ~122 ns | one extra cache line + full key compare |
| `std::HashMap<String, u32>` | ~185 ms | ~241 ns | in-RAM, not serialisable |
| lexindex `StringIndex` (FST) | ~252 ms | ~318 ns | *and* prefix / range / fuzzy |
| lexindex `DictIndex` (256 per block) | ~202 ms | ~465 ns | ordered, exact reverse; 1.95 B/key here against the FST's 0.68 — a `word.word` cross product is what a transducer factors out, and what a block of front-coded keys does not (on the dictionary: 2.65 against 5.95, 346–353 ns against 262–280) |
| `std::BTreeMap<String, u32>` | ~200 ms | ~725 ns | in-RAM |

<sub>**The memory-bound rows swing between sessions; the three hash rows halved, and that is not a
session.** Over five sessions the two rows whose code has not moved since 0.5.1 read `StringIndex`
1.30× → 1.47× → 1.32× → 1.47× → **1.32×** of `HashMap` and `BTreeMap` 3.19× → 3.34× → 2.63× →
3.24× → **3.01×**, with no trend and no single outlier, while the FxHash map sits at 0.586× → 0.60×
→ 0.58× → 0.60× → **0.60×** throughout. The two that move are exactly the two that miss to DRAM,
they move together and in the same direction, and they move across sessions in which their code did
not change — the signature of the machine, not of a release. Against that flat background the three
rows the key hash reaches moved, by more than any session ever has: `PerfectHashIndex::id` 0.95×
→ 1.04× → 1.02× → 1.03× → **0.50×**, `id_unchecked` 0.269× → 0.26× → 0.24× → 0.26× → **0.21×**,
`CompactHashIndex::id` 0.435× → 0.45× → 0.45× → 0.45× → **0.25×**. Verified `id` is now half the
`HashMap` it used to trail. Three controls holding still while three rows halve is what the new key
hash, the `MPH3` seed layout and the prefetched second level look like from outside. `DictIndex`
reads **465 ns for 1.95 B/key** at the default of 256 keys a block, where 2.0.0's 32 a block read
507 for 3.19 — the packed offsets and the microblocks together. `CompactHashIndex` builds in 36 ms
against 48 on 3.0.0 and 69 on 1.1.0. This corpus is its worst case by
construction: the 1 M keys are 1 000 words crossed with 1 000, which the transducer stores once per
factor (0.68 B/key) and a block of front-coded keys stores once per key. Real keys move lookups in
lexindex's favour versus synthetic ones, while every `build` reads higher than a synthetic sequence
would, because real input is not pre-sorted and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~144 ns** and `PerfectHashIndex::id_unchecked`
**~51 ns** — so on a closed vocabulary the perfect hash is about **2.8× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash, and then 4.0's replacement of both.
`CompactHashIndex::id` (~61 ns) is **2.4× faster** than the FxHash map and still carries the
membership check and the 1.24 B/key blob.

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
Both at 8-bit seeds and bucket size 4.5, as here. `ptr_hash` 2.1.1 is PtrHash's three parameter
sets. Bits and lookups are the 1-thread process's; the 8-thread process builds the same functions to
within 0.02 bits.

**1 M keys** — 241 KB of table, inside L2:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | **1.929** | **42.8 ns/key** | **12.3 ns/key** | **1.9 ns** | **2.2 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.160 | 55.4 | 17.9 | 3.5 | — |
| `ph` PHast (`SeedOnly`) | 1.933 | 581.3 | 100.5 | 2.8 | — |
| `ptr_hash` compact | 2.144 | 148.1 | 81.5 | 4.0 | 4.2 |
| `ptr_hash` balanced | 2.378 | 92.3 | 47.8 | 4.1 | 4.2 |
| `ptr_hash` fast | 2.990 | 72.0 | 64.3 | **1.9** | 2.4 |

**10 M keys** — 2.4 MB, inside the 16 MB L3:

| function | bits/key | build, 1 thread | build, 8 threads | lookup | batch |
|---|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | **1.918** | **37.5 ns/key** | **10.6 ns/key** | **2.6 ns** | **2.5 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.147 | 60.2 | 15.4 | 5.4 | — |
| `ph` PHast (`SeedOnly`) | 1.921 | 589.4 | 91.6 | 4.2 | — |
| `ptr_hash` compact | 2.143 | 162.7 | 44.2 | 5.5 | 4.7 |
| `ptr_hash` balanced | 2.378 | 104.0 | 28.1 | 5.6 | 4.7 |
| `ptr_hash` fast | 2.990 | 149.6 | 137.4 | 2.9 | 2.8 |

**100 M keys** — 24 MB, every scalar lookup a DRAM miss:

| function | bits/key | build, 1 thread | build, 8 threads | build peak | lookup | batch |
|---|---:|---:|---:|---:|---:|---:|
| **lexindex `MPH3`** | **1.914** | **37.5 ns/key** | **8.7 ns/key** | **262 MB** | **10.0 ns** | **3.5 ns** |
| `ph` PHast+ (`ShiftOnlyWrapped`) | 2.146 | 67.9 | 16.5 | 1 526 | 17.1 | — |
| `ph` PHast (`SeedOnly`) | 1.920 | 603.4 | 92.9 | 1 526 | 13.3 | — |
| `ptr_hash` compact | 2.143 | 165.4 | 40.5 | 906 | 16.8 | 6.2 |
| `ptr_hash` balanced | 2.378 | 105.9 | 24.3 | 886 | 17.5 | 6.2 |
| `ptr_hash` fast | 2.990 | 247.5 | 234.0 | 1 477 | 10.7 | 5.0 |

`MPH3` is the smallest function in every table — 1.929 / 1.918 / 1.914 bits against regular
PHast's 1.933 / 1.921 / 1.920 — and its lookups lead at 10 M and 100 M, single and batch: 2.6 and
10.0 ns against PtrHash's fast set's 2.9 and 10.7, and 2.5 and 3.5 in a batch against its 2.8 and
5.0. At 1 M its batch leads too, 2.2 against 2.4, and its single lookup is level with fast's at
1.9 ns, where the run before had fast a tenth of a nanosecond ahead, inside both rows' spreads:
3 bits a key, a table half again as large whose one probe is a seed byte with no bumped keys' chain
behind it. Among the rows near 2 bits `MPH3`'s lookups are the fastest at every size — PtrHash
compact's 4.0 / 5.5 / 16.8 ns single and 4.2 / 4.7 / 6.2 in a batch, PHast+'s 3.5 / 5.4 / 17.1,
and regular PHast's 2.8 / 4.2 / 13.3 at fourteen to sixteen times the build and without a batch
form. The bits column is the serialised table, which `MPH3` loads as it is — the rank and select
counts `MPH2` derived in memory, 0.023 bits/key, went with its remap.

`MPH3`'s build is the fastest at every size on either thread count: 42.8 / 37.5 / 37.5 ns/key on one
thread against PHast+'s 55.4 / 60.2 / 67.9 and PtrHash fast's 72.0 at 1 M, and 12.3 / 10.6 / 8.7 on
eight against PHast+'s 17.9 / 15.4 / 16.5. Against PtrHash's compact set `MPH3` builds 3.5–4.4×
faster on one thread and 4.2–6.6× on eight. The run before, at `e147b05`, had PHast+ ahead on one
thread at every size, 57 / 57 / 68 ns against 75 / 71 / 70, over the same tables: since then a
bucket's seed is searched over fixed lanes, one function a bucket width from 1 to 16 keys with every
loop's count known to the compiler, a mode's keys that could meet are found by their residues modulo
the slice, and the buckets wait for placement in a bitmap of priority slots taken from its lowest
set bit, not in a queue a size class under a heap. The construction peak is the column apart: 30 MB
at 10 M and 262 MB at 100 M above the keys, against 152–156 MB and 886–1 526 MB — the table is built
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
over strings with a general-purpose string hash. One process a corpus, three rounds; a round builds
every row first and then times the rows in turn within each pass, each turn preceded by an untimed
sweep over the first 2^20 probe keys, so that a machine warming through a round slows every row
alike and no row holds the caches to itself for a turn of its own. Every key is looked up once in a
shuffled order with the key read from the corpus, the minimum of nine passes; batch is `ids_of`
against `index_stream` in chunks of 4096; ×8 is the wall time a key with eight lookup threads. Each
cell is lookup / batch / ×8 in ns. Measured 2026-09-17 at `2a9acef`, on a machine at 53 °C and a
load of 0.38.

| corpus, keys (mean bytes) | **lexindex `ClosedHashIndex`** | `ptr_hash` fast, the same hash | `ptr_hash` fast, xxh3 |
|---|---:|---:|---:|
| words, 480 k (9) | 8.3 / **5.9** / 1.6 | **7.6** / 6.7 / **1.5** | 17.7 / 13.5 / 3.0 |
| URLs, 1 M (52) | **38.5** / **12.8** / **6.3** | 45.7 / 30.3 / 7.0 | 51.5 / 56.4 / 8.0 |
| UUIDs, 1 M (36) | **36.0** / **10.9** / **5.6** | 38.9 / 28.5 / 5.9 | 48.8 / 50.5 / 7.1 |
| titles, 1 M (21) | 26.0 / **9.7** / 3.8 | **25.2** / 19.9 / 3.8 | 38.1 / 37.6 / 5.2 |
| paths, 1 M (125) | **60.2** / **41.2** / **11.8** | 64.0 / 55.1 / 12.2 | 99.3 / 100.8 / 14.2 |
| URLs, 10 M (52) | **43.1** / **14.6** / **6.8** | 55.0 / 33.7 / 8.0 | 61.8 / 63.0 / 8.9 |
| UUIDs, 10 M (36) | **40.8** / **13.4** / **6.4** | 47.9 / 32.4 / 7.2 | 59.3 / 58.8 / 8.5 |
| titles, 10 M (21) | **35.2** / **11.4** / **5.6** | 38.4 / 24.4 / 5.9 | 49.3 / 50.1 / 7.2 |

With the key read from memory the hash is most of the cost, and the same hash under both perfect
hashes is what the middle column holds fixed: `ClosedHashIndex` leads the fast set on six of the
eight corpora, by 2.9 to 11.9 ns, and the lead grows with the corpus — at 480 k words and 1 M
titles, where every table in the process still fits in the last level beside the others, `fast` is
0.7 and 0.8 ns ahead; at 10 M, where they do not, `MPH3`'s 1.92 bits a key against 2.99 is 3.2 to
11.9 ns. The hash decides the rest: the same set over xxh3 is 9 to 39 ns behind. The batch column is
the crate's alone: `ids_of` hashes the next keys and pulls in both ends of each key while the
current ones' lines arrive, 12.8 / 10.9 / 9.7 ns on 1 M URLs / UUIDs / titles against
`index_stream`'s 30.3 / 28.5 / 19.9 over the same hash and 56 / 51 / 38 over xxh3, which does not
overlap the hashing — 2.0 to 2.6× on every corpus but the word list, whose keys are 9 bytes, and
the 125-byte paths. The `CompactHashIndex` row's single lookup is `id_unchecked`, the same
perfect-hash probe without the fingerprint compare, within 0.4 ns of `ClosedHashIndex` on every
corpus; its batch is `ids_of`, which does compare, and pays the fingerprint line's miss, 1.8 to
6.3 ns above it.

The table on this page before this run was measured on 2026-09-16 at `7215616`, with the protocol
that timed each row's lookups right after that row's own build. The two files are read against each
other through the `ptr_hash` over xxh3 rows: no code of this crate is in their path, and they read
within 3 % across them (1 M URLs 54.9 → 51.5 ns single and 58.3 → 56.4 batch, 10 M titles
49.5 → 49.3 and 50.6 → 50.1), so the interleave did not move the level. Against that file every
lexindex row is quicker — 1 M URLs 43.0 → 38.5 ns single and 28.7 → 12.8 in a batch, 10 M URLs
48.1 → 43.1 and 30.9 → 14.6 — over eighteen commits to `src/`, the batch column following the one
that prefetches a key's last line as well as its first; and the interleave can only have cost it,
since `MPH3`'s table now shares the last level with six others for a whole pass instead of holding
it through a turn of its own. The key hash's own gain was measured the same way one commit earlier:
against the `b4544db` file over the hash 2.0–3.x shipped, in the protocol both of those runs used,
every lexindex row was 10–30 % quicker — 1 M URLs 54.5 → 43.0 ns, paths 92 → 65, titles 32 → 28,
UUIDs 46 → 39 — while the xxh3 rows, the same code in both, read 3–15 % slower in the newer one, so
that gain too is at least the difference shown. Results files:
[`bench/results/mphf-strings-2026-09-17-arz-2a9acef.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-09-17-arz-2a9acef.txt),
[`bench/results/mphf-strings-2026-09-16-arz-7215616.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-strings-2026-09-16-arz-7215616.txt).

The lexindex row ran once more at each size in a process with transparent huge pages turned off
(`prctl(PR_SET_THP_DISABLE)`), the control for the `MADV_HUGEPAGE` its tables ask for from 2 MiB
up: 1.9 / 2.1 ns at 1 M against 1.9 / 2.2 with them (nothing to get below one huge page), 2.9 /
2.6 against 2.6 / 2.5 at 10 M and 10.8 / 4.1 against 10.0 / 3.5 at 100 M — 12 % on the single
lookup and 4 % on the batch at 10 M, 8 % and 17 % at 100 M, where the control process ran last,
near 90 °C, and the run before read 4 % and 3 % — with the results file's `huge MB` column showing
the pages the process holds after the build, 40 MB at 100 M against none without them.

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

`MPH2`'s row is its 2026-09-13 measurement, taken beside the Consensus run and over sorted keys
(below); `MPH3` in the tables above is 1.929 / 1.918 bits at 1.9 / 2.6 ns. The trade is 0.46 bits
a key — on a `CompactHashIndex` at 1.24 bytes a key, 5 % of the index — for 8–89× the build and
33–60× the lookup at 10 M. That is the right answer for an archive written once and read rarely;
this crate's tables are read on every `id`, and 1.92 bits is where its lookup stays first.

What this campaign does not have. A second CPU: one Ryzen 7 5800HS, a mobile part with 16 MB of
L3, and the 100 M rows are where its memory system shows — a server part with a larger cache and
more channels moves every row there. A 1 B row in the tables: one process and one round, eight
build and eight lookup threads, the probe order sampled to 10 M keys because the full one is
another 8 GB beside a 14 GB construction peak, started at 48.1 °C and ended at 83 °C
([`bench/results/mphf-vs-1b-2026-09-17-arz-08094a1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-1b-2026-09-17-arz-08094a1.txt)).
It built `MPH3` over 10⁹ keys in 7.7 s at 1.914 bits and **2.6 GB above the keys**, against
26.5–433 s and 8.1–14.4 GB for PtrHash's three sets and 21–100 s and 9.7 GB for the `ph` rows. Its
batch is the fastest there, 7.5 ns against PtrHash fast's 8.0 and compact's 8.4, and its single
lookup the fastest near 2 bits, 14.3 ns against PHast's 19.6 and compact's 24.8, with fast's
0.3 ns ahead at 14.0. With eight lookup threads it is behind: fast's single lookups read 4.0 ns
against 4.3, and PtrHash compact's and balanced's batches 3.2 and 3.3 against 4.1. Those two
columns are the least settled here — over the same lookup code they moved by up to 58 % between
processes: the run at `e147b05` read `MPH3` at 3.1 / 2.6 ns single / batch against fast's 3.0 and
compact's 2.5, and a second process after this run, lexindex and compact only, 3.2 / 2.8 against
compact's 3.9 / 2.2 — so compact's eight-thread batch is ahead in all three. The 1 B run before
the harness corrections above, on a hot machine, read 30.0 and 9.9 ns for `MPH3`'s single and
batch lookups over sorted keys, behind every PtrHash set. Confidence intervals: in their place,
every cell's minimum with the spread of its repeats, in the results file — builds repeat within
6 % on one thread and within 7 % on eight but for lexindex's 12 ms build at 1 M (38 %); lookups
within 10 % at 1 M but for PtrHash fast's (15 %), within 6 % at 10 M but for lexindex's (19 %:
passes of 26 ms) and within 5 % at 100 M; batches within 8 % but for lexindex's at 1 M (18 %:
passes of 2.2 ms).

<sub>The three tables were measured 2026-09-17 at `08094a1`
([`bench/results/mphf-vs-2026-09-17-arz-08094a1.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/mphf-vs-2026-09-17-arz-08094a1.txt)),
on mains with an editor open, after a wait for Tctl under 60 °C and a load under 0.7 (49.0 °C and
0.26 at the start, 52 minutes after a reboot). The run before, at `e147b05`
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
| bytes per key | 5.95 | 2.90 | 2.75 | 2.69 | **2.65** | 2.53 | 2.52 |
| of which per-block arrays | — | 0.199 | 0.152 | 0.128 | 0.115 | 0.062 | 0.058 |
| microblocks a block | — | 2 | 4 | 8 | 16 | 16 | 32 |
| entries a lookup scans | — | 16 | 18 | 22 | **30** | 46 | 62 |
| build | 107–108 ms | 45–46 | 33–34 | 32 | 31–32 | 31 | 38 |
| `id`, member | 262–280 ns | 305–321 | 307–312 | 325–353 | **346–353** | 386–412 | 426–436 |
| `id`, stranger | 211–218 ns | 251–253 | 256–259 | 277–278 | 300–303 | 335–339 | 371–374 |
| `key_into` (no allocation) | — | 143 | 153–156 | 173 | **198–200** | 256 | 305 |
| `key` (owned string) | 438–494 ns | 194–207 | 211–213 | 235–236 | 260–266 | 321–322 | 370–379 |
| `lower_bound`, stranger | — | 250–268 | 255–258 | 275–276 | 297–300 | 332–337 | 368–373 |
| `prefix`, 2 000 three-byte prefixes of ~760 keys | — | 31.2–31.8 µs | 31.3–31.7 | 31.5–31.7 | 31.5–31.7 | 30.8–31.2 | 31.0–31.4 |

Three runs of the same ladder; where they differ the table gives the range. At the default 256,
`DictIndex` answers `id` 24–35 % slower than `StringIndex` and `key_into` at less than half its
`key`, while storing **55 % less**. The ladder no longer crosses the control anywhere: `BDX3`'s
codes cost more to decode than the varint pairs `BDX2` stored, so even at 32 per block `id` runs
9–23 % behind the transducer where it used to lead by 6 %. That is the trade the format made —
every block got 6–10 % smaller and every `id` 17–24 % slower. The sequential lane does not move
with the block at all.

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

<sub>Measured 2026-09-19 at `999e933`
([`bench/results/dictbench-2026-09-19-arz-999e933.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/dictbench-2026-09-19-arz-999e933.txt)),
three runs of the whole ladder in one session, each waiting for the machine to go quiet and each the
minimum of five alternated rounds; the table gives the range over the three. The bytes, the
per-block arrays and the two structural rows are exact, not measured. `StringIndex` is the control
and reads 262–280 ns against the 265–283 of the session these rows replace, so the two are
comparable and the `DictIndex` rows really did move.</sub>

## Prefix queries against the tries

A prefix is a range of a sorted dictionary, so `DictIndex` answers one without an automaton:
`prefix_id_range` is two `lower_bound`s and `prefix` walks the run they name. `local/prefixbench.py`
puts that against the tries a Python project can install, on the same words — 20 000 three-byte
prefixes drawn from random words, so the average prefix carries 763 keys.

| | bytes/key | `prefix_count` | first 10 | every match + ids | keys only |
|---|---:|---:|---:|---:|---:|
| **lexindex `DictIndex` 256** (default) | **2.65** | **483 ns** | 2 213 ns | 106 493 ns | 78 211 ns |
| **lexindex `DictIndex` 512** | **2.53** | 511 | 2 347 | 104 242 | 75 428 |
| lexindex `StringIndex` | 5.95 | 567 | 4 325 | 160 970 | 321 626 |
| `marisa-trie` | 2.98 | 119 840 | 2 525 | 94 630 | 94 624 |
| `dawg2` | 23.96 | 68 261 | **1 456** | **49 240** | **49 271** |
| `datrie` | 30.69 | 727 644 | 730 578 | 731 963 | 729 360 |

**Counting is where the structures differ in kind rather than by a constant.** `prefix_count` costs
two order lookups whatever the prefix carries, so it runs 234× faster than marisa's at 512 per block
and 248× at the default — marisa has to enumerate all 763 matches to count them, its ids not being
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
`prefix` is still the API for this query there (160 970 against 321 626). It is also why there is no
`prefix_keys` method: it would be these two calls, and the table says what a third one would buy —
27 %, which two existing calls already have.

**The last two columns are this benchmark's error bar as well as a result.** A trie has no ids to
return, so for `marisa-trie`, `dawg2` and `datrie` they are one call measured twice — and within a
run they come out 0.006 %, 0.06 % and 0.2 % apart. Nothing narrower than that is a finding here:
`DictIndex` at 256 against 512 on full enumeration (106 493 against 104 242) is a 2 % difference and
near it, while the 1.21× over marisa on keys alone is well outside it.

`dawg2` enumerates about 1.4× faster at 8.3× the bytes, with no reverse lookup and no mmap: a
different point on the curve, not a smaller one. `datrie` is a double-array built for point lookups;
prefix walking is not what it is for.

<sub>Measured 2026-09-19 at `999e933`, one clean-tree run on a quiet machine
([`bench/results/prefix-2026-09-19-arz-999e933.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/prefix-2026-09-19-arz-999e933.txt)),
`marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3 over the same word list, Ryzen 7 5800HS,
load 0.48 at the start. None
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
| `datrie` | 30.69 | **546 ns** | **271 ns** |
| **lexindex `StringIndex`** | 5.95 | 636 | 357 |
| `dawg2` | 23.96 | 688 | 607 |
| `marisa-trie` | 2.98 | 1 104 | 861 |
| **lexindex `DictIndex` 256** (default) | **2.65** | 2 925 | 1 143 |
| **lexindex `DictIndex` 512** | **2.53** | 3 252 | 1 265 |

**This is the query a transducer is shaped for, and the numbers say so.** The query *is* the path:
one walk down the FST, every final state on it a match, `O(query bytes)` whatever the index holds.
`StringIndex` is second only to `datrie` on both columns — at **a fifth of `datrie`'s bytes and a
quarter of `dawg2`'s**, ahead of `dawg2` on both and of `marisa-trie` by 1.7× and 2.4× at twice
marisa's size. It is the *largest* of the ordered indexes in
this crate, and on this one query that is where the bytes went.

**`DictIndex` has no walk to make and the table shows what that costs.** One order lookup per
character boundary, so a ten-character query is ten binary searches where the trie made one
descent: 2.6× marisa's time at the default block, 2.9× at 512. `longest_prefix` is the exception —
it starts at the query and stops at the first hit, so it never pays for the boundaries under the
match, and both blocks land within 33–47 % of marisa (1 143 and 1 265 ns against 861) while storing
fewer bytes than it. A caller who
asks this question often should hold a `StringIndex`; one who asks it occasionally, alongside the
ranks and ranges only `DictIndex` gives, can have it for a binary search per character.

<sub>Measured 2026-09-19 at `999e933`
([`bench/results/common-prefix-2026-09-19-arz-999e933.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/common-prefix-2026-09-19-arz-999e933.txt)),
the minimum of three runs a minute and a half apart, each the minimum of five alternated rounds.
They agree within 3 % on every row but `datrie`'s `longest_prefix`, where they spread 10 % — read
that cell as the one with the error bar. `marisa-trie` 1.4.1, `dawg2` 0.13.3, `datrie` 0.8.3,
Ryzen 7 5800HS, load 0.04–0.28 at the three starts. Nanoseconds per query through Python; the call overhead
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
| per-corpus distribution — slot-hash collisions, top 12 bits, low 12 bits, the 8-bit fingerprint, and the joint (slot, fingerprint) table | a family of keys the hash folds together, and any correlation between the two hashes | ten corpora, no collisions, every value under 2.7 |

The ten corpora are the shapes that break hashes in practice: dictionary words, word bigrams, a
shared prefix (`https://example.com/a/b/…`), a shared suffix (`…@mail.example.com`), a dense
numeric tail (`key_000000001`), plain decimal integers, UUIDs, Cyrillic, DNA, and filesystem paths.

<sub>Committed output:
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
| `ClosedHashIndex` | 0.24 | 10.9 | 11.3 | 10.9 | 13.6 | 11.1 – 11.3 |
| `CompactHashIndex` fp=1 | 1.24 | 14.1 | 14.2 | 14.1 | 15.2 | 14.2 – 14.5 |
| `PerfectHashIndex` | 10.88 | 42.2 | 43.6 | 42.3 | 45.2 | 42.9 – 44.0 |
| `StringIndex` | 5.95 | 230.7 | 233.7 | 231.0 | 238.4 | 232.5 – 233.9 |
| `DictIndex` 256 | 2.65 | 311.9 | 314.8 | 312.6 | 324.3 | 313.8 – 316.4 |
| `DictIndex` 512 | 2.53 | 346.4 | 349.8 | 347.2 | 361.6 | 349.0 – 350.8 |

**The minimum is within 4 % of the median everywhere**, and the p5–p95 band is 3–24 % of it, so
quoting the minimum costs nothing — same probe set, same binary, thirty consecutive passes. An
earlier session of the same harness put the band under 50 ns at 45 % and the minimum 25–29 % under
the median while the slow lanes kept their 2–10 %: a fast lane's width is the machine's that day,
not the structure's, and the interval column is what says which day it was.

**The probe set is part of the working set.** Only the number of probes a pass changes here:

| probes a pass | `Closed` | `Compact` fp=1 | `Perfect` | `String` | `Dict` 256 | `Dict` 512 |
|---|---:|---:|---:|---:|---:|---:|
| 100 000 | 9.9 | 15.3 | 41.5 | 230.1 | 315.5 | 349.6 |
| 200 000 | 11.3 | 14.2 | 43.6 | 233.7 | 314.8 | 349.8 |
| 400 000 | 27.3 | 27.5 | 59.2 | 271.3 | 344.2 | 383.7 |

`ClosedHashIndex` nearly triples; `DictIndex` 256 moves 9 %. The probe array is a second structure the loop
walks: 100 000 `String` headers are 2.4 MB, 400 000 are 9.6 MB before the bytes they point at, and
past some point between the two the probe stops being in cache and starts being fetched at the same
cost as the lookup it pays for. The smaller the index, the larger the share of the total that is.
The counters say it in one line — **the instruction count does not move and the cycle count does**:

| structure | instr | cycles at 100 k | at 400 k | branch misses | L1 fills | LLC misses |
|---|---:|---:|---:|---:|---:|---:|
| `ClosedHashIndex` | 109 | 40.4 | 77.5 | 0.07 | 2.35 | 1.15–1.25 |
| `CompactHashIndex` fp=1 | 121 | 58.6 | 102.0 | 0.06 | 3.42 | 2.04–2.23 |
| `PerfectHashIndex` | 181 | 120.4 | 227.9 | 0.70 | 5.27 | 3.86–4.12 |
| `StringIndex` | 2225 | 954.1 | 1136.5 | 13.28 | 14.79 | 7.37–7.89 |
| `DictIndex` 256 | 2952 | 1362.5 | 1472.2 | 18.04 | 21.32 | 4.40–4.72 |
| `DictIndex` 512 | 3475 | 1497.6 | 1639.3 | 20.90 | 20.17 | 4.10–4.33 |

Three things fall out of that table that no nanosecond showed:

- **`DictIndex` does the most work and misses the least of the structures that keep their keys,
  and neither is what it waits for.** 2 952 instructions at 256 against `StringIndex`'s 2 225, and
  4.4–4.7 LLC misses against 7.4–7.9. The one-level format at 128 keys a block spent 3 226 instructions and
  missed 6.6 a lookup; the microblock cut both by a fifth and a third, and at equal size the
  latency did not move — because what a lookup waits for is a chain of dependent misses, the sample
  search then the restart run then the microblock, which is the same length whatever the scan, and
  the instructions and the streamed misses of the scan overlap under it. IPC 2.0–2.2 says the core
  is busy; the chain says with what. It is also why a block of 32 keys and one of 512 sit 27 % apart
  on `id` and not 2.9× apart on the entries they scan.
- **`PerfectHashIndex` is pure latency.** 181 instructions, 3.9–4.1 LLC misses, IPC 0.8–1.5: it
  does almost no work and waits for all of it.
- **`StringIndex` misses the most, 7.4–7.9 a lookup.** That is the same fact as its thread scaling
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

<sub>Measured 2026-09-19 at `a3f7503`
([`bench/results/stats-2026-09-19-arz-a3f7503.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/stats-2026-09-19-arz-a3f7503.txt),
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

<!-- table: frontier bench/results/frontier-1m-2026-09-19-arz-ef8996e.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `words-full` | 2.65 @ 304 | lexindex Dict 1024 2.52 @ 370 | XCDAT 15 7.30 @ 77 | Dict 1024, Dict 256, Dict 32 |
| `dna-1000000` | 4.40 @ 357 | lexindex Dict 1024 4.24 @ 386 | XCDAT 15 22.46 @ 289 | Dict 1024, Dict 256 |
| `domains-1000000` | 4.75 @ 347 | lexindex Dict 1024 4.59 @ 422 | XCDAT 15 10.33 @ 146 | Dict 1024, Dict 256, Dict 32 |
| `idents-1000000` | 5.30 @ 410 | lexindex Dict 1024 5.13 @ 484 | XCDAT 15 13.78 @ 203 | Dict 1024, Dict 256, Dict 32 |
| `numeric-1000000` | 1.03 @ 274 | lexindex StringIndex 0.0003 @ 116 | XCDAT 15 7.05 @ 54 | StringIndex |
| `opaque-1000000` | 10.42 @ 360 | lexindex Dict 1024 10.30 @ 409 | XCDAT 15 20.02 @ 205 | Dict 1024, Dict 256, Dict 32 |
| `paths-1000000` | 10.18 @ 787 | MARISA ρ=2 9.47 @ 2227 | XCDAT 15 25.11 @ 632 | Dict 1024, Dict 256 |
| `pypi-full` | 4.36 @ 359 | lexindex Dict 1024 4.21 @ 427 | XCDAT 15 9.92 @ 138 | Dict 1024, Dict 256, Dict 32 |
| `titles-en-1000000` | 7.40 @ 436 | lexindex Dict 1024 7.23 @ 497 | XCDAT 15 17.13 @ 242 | Dict 1024, Dict 256, Dict 32 |
| `titles-ru-1000000` | 7.81 @ 498 | lexindex Dict 1024 7.57 @ 545 | XCDAT 15 23.08 @ 361 | Dict 1024, Dict 256, Dict 32 |
| `titles-zh-1000000` | 6.29 @ 405 | lexindex Dict 1024 6.11 @ 471 | XCDAT 15 13.58 @ 204 | Dict 1024, Dict 256, Dict 32 |
| `urls-1000000` | 7.53 @ 489 | lexindex Dict 1024 7.28 @ 554 | XCDAT 15 18.14 @ 366 | Dict 1024, Dict 256, Dict 32 |
| `uuid-1000000` | 18.06 @ 448 | lexindex Dict 1024 17.90 @ 488 | XCDAT 15 38.88 @ 343 | Dict 1024, Dict 256 |
<!-- /table -->

<sub>A million keys, measured 2026-09-19 at `ef8996e`
([`bench/results/frontier-1m-2026-09-19-arz-ef8996e.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-1m-2026-09-19-arz-ef8996e.json)),
Ryzen 7 5800HS, GCC 16.2.1, rustc 1.98.1. A cell reads bytes a key @ nanoseconds a lookup.
`dna`, `titles-zh`, `urls` and `uuid` were measured a second time at the same commit
([`frontier-named-2026-09-19-arz-ef8996e.log`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-named-2026-09-19-arz-ef8996e.log))
after their first round in the campaign ran beside up to 1.4 busy CPUs of other work, and those
runs replace the campaign's, whose own stay in its log. Other work while the 949 processes behind
the table ran: median 0.07 busy CPUs, most 0.77.</sub>

lexindex is on the front on all thirteen corpora — `DictIndex` on twelve, `StringIndex` on
`numeric` — and `DictIndex` builds before every other structure on all thirteen: 1.2× ahead of the
next on `words` and `numeric`, 1.3–5.9× on the rest. It is the smallest structure outright on eleven
of them, by 6 % on `domains` up to 45 % on `dna`: 2.52 bytes a key at block 1024 on `words` against
MARISA's 2.98 at ρ=2, 7.57 on `titles-ru` against 8.61, 17.90 on `uuid` against PDT's 21.44 from a
16.7-second build. `StringIndex` is the smallest on `numeric`, where the automaton folds a million
decimal ids into about 300 bytes. Two corpora keep a trie ahead of `DictIndex` on size: `paths`,
where MARISA at ρ=2 is 1.5 % smaller at 2.7 times the lookup and 2.7 times the build, and `numeric`,
where CoCo-trie's 0.52 beats block 1024's 0.93 but not `StringIndex`. XCDAT is the fastest structure
on every corpus, at 1.9–6.8 times the bytes of `DictIndex` at its default block; on `titles-ru`,
`titles-zh` and `urls` C²-MARISA is faster too, and larger — 9.14 bytes a key at ρ=2 on `titles-ru`
against 7.57 — so both are on the front.

<!-- table: frontier bench/results/frontier-10m-2026-09-19-arz-5783460.json -->
| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |
|---|---:|---|---|---|
| `dna-10000000` | 3.97 @ 536 | lexindex Dict 1024 3.82 @ 521 | lexindex Dict 1024 3.82 @ 521 | Dict 1024 |
| `numeric-10000000` | 1.04 @ 344 | lexindex StringIndex 3.6e-05 @ 138 | lexindex StringIndex 3.6e-05 @ 138 | StringIndex |
| `opaque-10000000` | 10.07 @ 581 | lexindex Dict 1024 9.96 @ 587 | XCDAT 15 21.66 @ 341 | Dict 1024, Dict 256 |
| `titles-en-10000000` | 5.82 @ 666 | lexindex Dict 1024 5.64 @ 689 | XCDAT 15 13.95 @ 581 | Dict 1024, Dict 256 |
| `urls-10000000` | 5.86 @ 771 | lexindex Dict 1024 5.59 @ 766 | XCDAT 15 14.75 @ 707 | Dict 1024 |
| `uuid-10000000` | 17.58 @ 697 | lexindex Dict 1024 17.42 @ 673 | XCDAT 15 38.58 @ 556 | Dict 1024 |
<!-- /table -->

<sub>Ten million keys, measured 2026-09-19 at `5783460`
([`bench/results/frontier-10m-2026-09-19-arz-5783460.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-09-19-arz-5783460.json)),
the same machine and toolchain, with two hours allowed a process and none reaching it. Other work
while the 416 processes ran: median 0.03 busy CPUs, most 0.62.</sub>

At ten times the keys lexindex is on the front of all six corpora, `DictIndex` on five. It is the
smallest structure on five of them: `dna` at 3.82 bytes a key at block 1024 against C²-CoCo's 5.98
at ρ=1, `opaque` 9.96 against PDT's 13.73 and `uuid` 17.42 against PDT's 20.59 — builds of 115 and
126 seconds against 1.3 and 1.7 — and, by a narrower 1 % and 4 %, `titles-en` and `urls` against
MARISA at ρ=2; `StringIndex` is both the smallest and the fastest on `numeric`. On `dna` block 1024
is the fastest row as well as the smallest, 521 ns to XCDAT's 571, so it dominates every other
structure on that corpus outright. `DictIndex` builds before every one of them again, in 0.9–2.6 s,
where the next is MARISA at 2.7–7.2 times that (PDT, at 1.4 times, on `numeric`), C²'s structures
take 10.5–31 s and PDT up to two minutes. Only `numeric` keeps a trie smaller than `DictIndex`:
CoCo-trie's 0.51 bytes a key against block 1024's 0.93, and neither is near `StringIndex`. `urls`,
the corpus with the longest keys, is below whole.

<!-- table: frontier bench/results/frontier-10m-2026-09-19-arz-5783460.json corpus=urls-10000000 -->
**`urls-10000000`** — 10,000,000 keys, 52.37 bytes a key raw

| structure | bytes/key | % of raw | build ms | `id` ns | spread | front |
|---|---:|---:|---:|---:|---:|:---:|
| lexindex Dict 32 | 7.22 | 13.8 | 2535 | 792 | 2 % |  |
| lexindex Dict 256 | 5.86 | 11.2 | 2586 | 771 | 2 % |  |
| lexindex Dict 1024 | 5.59 | 10.7 | 2512 | 766 | 1 % | ● |
| lexindex StringIndex | 13.10 | 25.0 | 9334 | 950 | 3 % |  |
| C²-FST | 9.41 | 18.0 | 12126 | 1563 | 1 % |  |
| C²-FST ρ=1 | 8.20 | 15.7 | 14085 | 1850 | 1 % |  |
| C²-FST ρ=2 | 8.20 | 15.7 | 14685 | 1854 | 1 % |  |
| C²-CoCo | 9.92 | 18.9 | 28942 | 1588 | 1 % |  |
| C²-CoCo ρ=1 | 8.71 | 16.6 | 30896 | 1848 | 2 % |  |
| C²-CoCo ρ=2 | 8.71 | 16.6 | 31431 | 1866 | 0 % |  |
| C²-MARISA | 8.34 | 15.9 | 11990 | 900 | 1 % |  |
| C²-MARISA ρ=1 | 6.92 | 13.2 | 14319 | 1197 | 0 % |  |
| C²-MARISA ρ=2 | 6.92 | 13.2 | 15035 | 1197 | 0 % |  |
| FST | 9.54 | 18.2 | 15776 | 1993 | 0 % |  |
| CoCo | — | — | — | — | — | aborted: std::bad_alloc |
| MARISA | 8.66 | 16.5 | 10657 | 1125 | 2 % |  |
| MARISA ρ=1 | 6.23 | 11.9 | 11790 | 1545 | 0 % |  |
| MARISA ρ=2 | 5.83 | 11.1 | 11943 | 1623 | 1 % |  |
| PDT | 7.34 | 14.0 | 27693 | 1182 | 1 % |  |
| ART | 35.80 | 68.4 | 3615 | 968 | 11 % | ref |
| C-ART | 16.85 | 32.2 | 4289 | 989 | 4 % | ref |
| XCDAT 7 | 12.37 | 23.6 | 12728 | 888 | 4 % |  |
| XCDAT 8 | 11.96 | 22.8 | 12336 | 822 | 3 % |  |
| XCDAT 15 | 14.75 | 28.2 | 12652 | 707 | 5 % | ● |
| XCDAT 16 | 15.43 | 29.5 | 12343 | 753 | 2 % |  |
<!-- /table -->

<sub>From the same artifact
([`bench/results/frontier-10m-2026-09-19-arz-5783460.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/frontier-10m-2026-09-19-arz-5783460.json)).
`spread` is the range of a cell's three rounds over their median, and `ref` marks the two structures
that do not count their keys. Two rows are on the front: block 1024, the smallest structure here at
5.59 bytes a key against MARISA's 5.83 at ρ=2, and XCDAT 15, 8 % faster at 2.6 times the bytes.
`bench/frontier/tables.py` prints this table for every corpus at both scales from the logs.</sub>

Not every structure runs everywhere, and a cell that failed says why instead of carrying a number.
C²'s three structures crash with a segmentation fault on `numeric` at every depth and both scales,
and on `dna` at ten million keys at ρ=2. CoCo-trie stops with `Sequence is not sorted` on the three
title corpora at a million keys, and runs out of the 28 GB of address space each process is allowed
on `uuid` at a million and on every corpus but `numeric` at ten million.

The campaign puts the rest of the frontier beside `marisa-trie`, and what it finds there has moved:
no compressed trie here builds faster than blocks of front-coded keys, and none is smaller either,
bar MARISA at ρ=2 on `paths` at a million keys, by 1.5 %, and CoCo-trie on `numeric`, where
`StringIndex` beats both by four orders of magnitude. A block still shares only with its own
neighbours, so where long fragments repeat across the whole key set — URLs, titles — the lead is
narrow: 1 % on `titles-en` and 4 % on `urls` at ten million keys. Where C²'s cache-conscious MARISA
reads faster, on URLs and on the Russian and Chinese titles, it is larger, and both are on the
front.

## Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.24 bytes/key holds constant** as
`n` grows. Each row is measured twice: handing the constructor a **list** of keys, and handing it a
**generator**. The second is what `CompactHashIndex`'s streaming build exists for — it keeps a
16-byte pair per key and drops the string — and it is the only way to see the index's own footprint
rather than the corpus's:

| n | structure | keys | build | bytes/key | peak RSS | lookup |
|---|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | list | 0.33 s | 0.68\* | 155 MB | 189 ns |
| 1 M | `StringIndex` | generator | 0.48 s | 0.68\* | 148 MB | 191 ns |
| 1 M | `CompactHashIndex` | list | 0.08 s | 1.24 | 146 MB | 72 ns |
| 1 M | `CompactHashIndex` | **generator** | 0.20 s | 1.24 | **79 MB** | 76 ns |
| 10 M | `StringIndex` | list | 4.3 s | 2.00\* | 1109 MB | 661 ns |
| 10 M | `StringIndex` | generator | 5.9 s | 2.00\* | 1033 MB | 593 ns |
| 10 M | `CompactHashIndex` | list | 0.58 s | 1.24 | 956 MB | 210 ns |
| 10 M | `CompactHashIndex` | **generator** | 1.8 s | 1.24 | **268 MB** | 228 ns |

<sub>Measured at `4ee80d2`
([`bench/results/scale-2026-09-19-arz-4ee80d2.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/scale-2026-09-19-arz-4ee80d2.json)),
one process per cell and the **minimum of five** per cell, on a machine at load 0.88–0.99 at both
ends. `StringIndex`, whose build and lookup code has not changed since 0.5.1 and is therefore the
control, reads the 2.0.0 table back within its noise: 0.33 and 0.48 s at 1 M unchanged, 4.4 → 4.3
and 6.0 → 5.9 at 10 M, lookups 188 → 189 and 703 → 661 ns. Against that flat control,
`CompactHashIndex`'s builds fell again — 0.09 → 0.08 s at 1 M and **0.66 → 0.58 s** at 10 M — and
its blob is **1.24 B/key** at both sizes, where 2.0.0 wrote 1.26: the same key-hash and `MPH3` work
that halved the Rust-level lookup table above. Its lookups here move far less (78 → 72 and 242 →
210 ns) because a per-call Python loop is most of what they measure. The generator build's peak at
10 M is 268 MB against the corpus-holding build's 956 — that gap, not the blob, is what the
streaming constructor exists for.</sub>

