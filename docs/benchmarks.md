# Benchmarks

Every number here is a claim about one machine on one day, and every timed table cites the file under
[`bench/results/`](https://github.com/ilgrad/lexindex/tree/main/bench/results) that holds its raw
samples and the environment that produced them. The README carries the two headline tables; this
page holds the rest and the protocol behind every number — read the ratios, not the absolutes.

## Serialised size on real English words

`python bench/compare.py` on `/usr/share/dict/words` (479 823 words, 9.3 B/key raw). **Keys are a real
vocabulary, never a synthetic `entity-{i}` sequence** — sequential keys collapse the FST to a
near-regular automaton and report a misleading ~0 B/key, so the benchmark refuses them. Smaller is
better; the capability columns are why you would still pick a larger one.

| library | prefix | range | fuzzy | reverse id→str | exact membership | zero-copy mmap | **bytes/key** |
|---|:---:|:---:|:---:|:---:|:---:|:---:|---:|
| **lexindex `CompactHashIndex` (fp=4 bits)** | — | — | — | — | probabilistic | ✅ | **0.76** |
| **lexindex `CompactHashIndex` (fp=1)** | — | — | — | — | probabilistic | ✅ | **1.26** |
| **lexindex `CompactHashIndex` (fp=2)** | — | — | — | — | probabilistic | ✅ | **2.26** |
| `marisa-trie` | ✅ | — | — | ✅ | ✅ | ✅ | 2.98 |
| **lexindex `StringIndex`** | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 5.95 |
| lexindex `PerfectHashIndex` | — | — | — | ✅ | ✅ | ✅ | 10.90 |
| DAWG (`dawg2`) | ✅ | — | — | — | ✅ | — | 23.96 |
| `datrie` | ✅ | — | — | — | ✅ | — | 30.92 |

<sub>Raw numbers and the machine that produced them:
[`bench/results/compare-2026-09-09-arz-0c637f6.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/compare-2026-09-09-arz-0c637f6.json)
— every cell's build samples, the false-positive measurement, the CPU, kernel, rustc, Python and the
load average at both ends of the run.</sub>

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
trade-off. **`StringIndex` is the only
structure that answers fuzzy and range queries at all**, at 4× below a plain DAWG. `marisa-trie`
remains the pick when you need *exact* membership *and* ordering *and* the smallest such index —
lexindex doesn't claim that particular cell (see below for why).

## Against other Rust string indexes

`marisa-trie` is C++. Of the ordered Rust string indexes benchmarked here, **none is smaller than
`StringIndex`** — the double-array tries trade space for lookup speed, and no succinct LOUDS trie
(marisa / XCDAT / CoCo-trie-style) exists in Rust to depend on. So `StringIndex` at 5.95 B/key is the
**smallest of the pure-Rust ordered indexes measured below** — second only to a C++ library, and the
only one of them that does fuzzy and range. The comparison is against the four crates in the table,
not against all of crates.io, which no benchmark can settle. Same real words:

| Rust structure | bytes/key | vs marisa |
|---|---:|---:|
| `marisa-trie` (C++, reference) | 2.98 | 1.0× |
| **lexindex `StringIndex`** (ordered + fuzzy + reverse) | **5.95** | 2.0× |
| `fst::Set` (membership only — no ids, no reverse) | 4.85 | 1.6× |
| `yada` (double-array) | 15.98 | 5.4× |
| `crawdad::MpTrie` (minimal-prefix) | 19.63 | 6.6× |
| `crawdad::Trie` (double-array) | 26.22 | 8.8× |

<sub>Measured with `crawdad` 0.4, `yada` 0.5, `fst` 0.4 over the same word list; size = serialised bytes
(`serialize_to_vec().len()`) ÷ key count. Not lexindex dependencies — reproduce in a throwaway crate.</sub>

Reaching `marisa`'s 2.98 needs its recursive succinct-trie label nesting, which the byte-oriented `fst`
automaton is ~1.6× away from by construction (even a bare `fst::Set`, which stores no ids at all, is
4.85) — so beating it on the *ordered* index means reimplementing marisa from scratch, not a bounded
tweak. `CompactHashIndex` takes the size crown the other way: by dropping the keys entirely.

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
  out; `StringIndex` (ordered, prefix / range / fuzzy / subsequence) or `PerfectHashIndex` (exact
  membership, `id → key`, no ordering) are the candidates, and both pay for the keys they store.
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
Measured on 1.1.0 (one run of the example; each lookup cell is the minimum of five timed passes
after a warm-up pass) on a machine idle throughout (load 1.0). Absolute numbers are
machine-dependent — the `std::HashMap` control reads 21 % *faster* than in the session that
produced the previous table, which had an editor holding a core — so compare the **ratios**, and
only within a column: against that `HashMap`, `CompactHashIndex::id` is 0.44×, `id_unchecked`
0.27×, `PerfectHashIndex::id` 0.95×, `StringIndex` 1.30×, `BTreeMap` 3.19×.

| structure | build | lookup | note |
|---|---|---|---|
| lexindex `CompactHashIndex::id` (fp=1) | **~69 ms** | ~106 ns | fingerprint-verified, `2^-8` false-positive rate |
| lexindex `PerfectHashIndex::id_unchecked` | ~246 ms | **~66 ns** | closed vocabulary, no membership check |
| `std::HashMap<String, u32>` | ~178 ms | ~245 ns | in-RAM, not serialisable |
| lexindex `PerfectHashIndex::id` (verified) | ~255 ms | ~232 ns | one extra cache line + full key compare |
| lexindex `StringIndex` (FST) | ~248 ms | ~317 ns | *and* prefix / range / fuzzy |
| `std::BTreeMap<String, u32>` | ~197 ms | ~779 ns | in-RAM |

<sub>**The rows whose code did not move hold their ratio to `HashMap` within 6 % of the previous
table**: `StringIndex` 1.38× → 1.30×, `BTreeMap` 3.13× → 3.19×, the FxHash map 0.581× → 0.586×. The
rows the new perfect hash reaches moved by about the same amount and in its direction —
`PerfectHashIndex::id` 1.003× → 0.95×, `id_unchecked` 0.290× → 0.269×, `CompactHashIndex::id`
0.456× → 0.435× — which matches the 7 % faster in-order lookup the hash measured on its own
(4.46 → 4.16 ns/key, A-B-A-B in one process) but is no larger than the `StringIndex` control's own
drift, so read it as a direction, not a size. Every **build** cell fell 20–22 %, `HashMap`'s
included: that is the session, not the code — `CompactHashIndex` still builds in 0.39× of
`HashMap`'s time, as it did. Real keys move lookups in lexindex's favour versus synthetic ones,
while every `build` reads higher than a synthetic sequence would, because real input is not
pre-sorted and sorting is part of the build.</sub>

**`HashMap` here is the `std` one, which hashes with SipHash** — hardened against hash-flooding and
correspondingly slow on short keys. That is the map most Rust code actually uses, so it is the right
default comparison, but it is not the fastest map available: the same `HashMap` with a
non-cryptographic hasher is much quicker, and `cargo run --release --example bench` prints that row
too (FxHash, written out in the example rather than added as a dependency). In the same session
as the table above, `HashMap` + FxHash reads **~143 ns** and `PerfectHashIndex::id_unchecked`
**~66 ns** — so on a closed vocabulary the perfect hash is about **2.2× faster than a
fast-hashed map**, not merely level with it. That reverses what this README said through 0.12, where
two 12-run sessions on a *shared* machine put FxHash at 196/200 ns against `id_unchecked`'s 216/216
and concluded the latency advantage was gone. What changed is not the measurement conditions but the
code: 1.0's own perfect hash and its 8-byte-at-a-time key hash. `CompactHashIndex::id` (~106 ns) is
now 26 % *faster* than the FxHash map and still carries the membership check and the 1.26 B/key
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
**fastest of the structures in the table above** — 3.7× as quick as the SipHash `HashMap` and 2.2×
the FxHash one (no probing, no membership comparison) *and* compact + serialisable.
`CompactHashIndex::id` keeps a probabilistic membership check and *still* beats the SipHash
`HashMap` on lookup (2.3× here), and builds faster than it too. Full verification (`id`) pays one extra
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

At the size this crate chose, about 2.1 bits, `MPH2` builds 1.8× faster than the PHast+ it is
modelled on and 4.2× faster than PtrHash's compact set, and its lookup is the fastest of the three;
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
process**, so the two arenas never compete for the cache: five alternations of the five modes,
ten passes each, the minimum. The in-process A-B-A-B (all three indexes resident, seven rounds)
gives the batched rows and the 50/50 mix.

| probe | plain | with fingerprints | |
|---|---:|---:|---|
| `id`, member | 168 ns | 171 ns | one index per process |
| `id`, absent | 167 ns | **91 ns** | one index per process |
| `id`, 50 % absent | 168 ns | 136 ns | in-process |
| `ids_of`, member | 71 ns | 76 ns | in-process |
| `ids_of`, absent | 70 ns | **44 ns** | in-process |
| `CompactHashIndex::id_unchecked` (control) | 42 ns | 42 ns | |
| bytes per key | 10.90 | 11.90 | exactly +1 |

An absent probe stops at the block: the offset line says no key with that fingerprint is in the
slot, and the key — the second cache miss — is never read. A member pays the second hash and one
compare, three nanoseconds. The fingerprint bytes follow the block's offsets; interleaving them
with the offsets was measured first and cost a member 6 ns instead of 3, because the offset pair
moved up to 36 bytes from the base and split cache lines more often.

<sub>Measured 2026-09-10 on the tree that adds the layout
([`bench/results/negfp-2026-09-10-arz-8f136d3.txt`](https://github.com/ilgrad/lexindex/blob/main/bench/results/negfp-2026-09-10-arz-8f136d3.txt)),
Ryzen 7 5800HS, load 0.8–1.2 with an editor open.</sub>

## Scaling to millions of keys

`python bench/scale.py` on real high-entropy keys (dictionary-word bigrams). Build time and memory grow
linearly, lookups stay sub-microsecond, and `CompactHashIndex`'s **1.26 bytes/key holds constant** as
`n` grows. Each row is measured twice: handing the constructor a **list** of keys, and handing it a
**generator**. The second is what `CompactHashIndex`'s streaming build exists for — it keeps a
16-byte pair per key and drops the string — and it is the only way to see the index's own footprint
rather than the corpus's:

| n | structure | keys | build | bytes/key | peak RSS | lookup |
|---|---|---|---:|---:|---:|---:|
| 1 M | `StringIndex` | list | 0.34 s | 0.68\* | 154 MB | 195 ns |
| 1 M | `StringIndex` | generator | 0.47 s | 0.68\* | 147 MB | 198 ns |
| 1 M | `CompactHashIndex` | list | 0.11 s | 1.26 | 144 MB | 75 ns |
| 1 M | `CompactHashIndex` | **generator** | 0.22 s | 1.26 | **77 MB** | 76 ns |
| 10 M | `StringIndex` | list | 4.4 s | 2.00\* | 1108 MB | 655 ns |
| 10 M | `StringIndex` | generator | 6.0 s | 2.00\* | 1032 MB | 659 ns |
| 10 M | `CompactHashIndex` | list | 0.91 s | 1.26 | 982 MB | 271 ns |
| 10 M | `CompactHashIndex` | **generator** | 2.1 s | 1.26 | **297 MB** | 245 ns |

<sub>Measured on 1.1.0
([`bench/results/scale-2026-09-09-arz-0c637f6.json`](https://github.com/ilgrad/lexindex/blob/main/bench/results/scale-2026-09-09-arz-0c637f6.json)),
one process per cell and the **minimum of five** per cell, on a machine idle throughout (load 1.0
at both ends). `StringIndex`, whose build and lookup code has not changed since 0.5.1 and is
therefore the control, builds 11–16 % faster than in the previous table (0.38 → 0.34 s, 5.2 →
4.4 s), so that much of every cell is the session; `CompactHashIndex`'s builds fell by the same
18 % and no more, which is what a perfect hash that is a small part of a build dominated by hashing
and sorting the keys looks like. Its blob is 1.26 B/key at both sizes against 1.27 and 1.26 before,
from the 2.09-bit perfect hash. Lookups held at 10 M; the two 1 M `CompactHashIndex` cells halved
(152 → 75 ns) with their five samples spread over 75–150 ns against 150–197 before, which is below
what a per-call Python loop resolves, and is not read as a code change. Peak memory is
unchanged.</sub>

