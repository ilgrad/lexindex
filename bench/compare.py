"""Honest, fair comparison of lexindex against the string-index libraries a Python developer would
actually reach for (all `pip install`-able): marisa-trie, DAWG, datrie, and the builtin dict.

**Keys are real English words** (`/usr/share/dict/words` by default) — never a synthetic
`entity-{i}` sequence. Sequential structured keys collapse the FST to a near-regular automaton and
report a misleading ~0 bytes/key; only a natural, high-entropy vocabulary measures the real cost.

The modern academic state of the art in *pure compression* (CoCo-trie, XCDAT, PDT, SuRF) is
research-grade C++ with no Python bindings, so it is cited in the article, not benchmarked here.
Among installable libraries this measures the axes that matter — build time and **serialised
size** — and records which *capabilities* each one offers (ordered queries, reverse lookup, and
crucially whether membership is **exact** or **probabilistic**). Build time is the median of five
runs after a discarded warm-up, so no library is charged for its own first import (lexindex is
imported at the top of this file; the others import inside their build callable). **Lookup latency
is measured next to the size**, because bytes per key on their own invite the reading that the
smallest structure is the best one: it is the minimum over five rounds of a shuffled probe set that
is half members and half plausible strangers, every structure taking one pass per round so that none
of them is timed with the caches still warm from its own build. Every row pays the same Python call
boundary, so what the loop and the call cost alone is printed above the table rather than left as
the reason not to measure at all; the builtin `dict` is a row in that table and not its floor -- at
this key count a dict is a memory-bound lookup like any other, and several rows come in under it.
`cargo run --release --example bench` measures the same call without the boundary, in Rust.

Every number printed here is also written to `bench/results/compare-<date>-<host>-<commit>.json`
with the machine that produced it; the README table cites that file.

Run:
  uv run --with matplotlib --with marisa-trie --with datrie --with dawg2 \\
         --with <lexindex wheel> python bench/compare.py [WORDS_FILE]
"""

from __future__ import annotations

import os
import random
import statistics
import sys
import tempfile
import time
from pathlib import Path
from typing import NamedTuple

import _results
import lexindex
import matplotlib.pyplot as plt

OUT = Path(__file__).parent / "plots"
OUT.mkdir(exist_ok=True)


def _load_words() -> tuple[list[str], str]:
    """Real, high-entropy keys. Prefer an explicit path / env, else the system word list."""
    candidates = [
        sys.argv[1] if len(sys.argv) > 1 else None,
        os.environ.get("LEXINDEX_BENCH_WORDS"),
        "/usr/share/dict/words",
    ]
    for path in candidates:
        if path and Path(path).exists():
            with open(path, encoding="utf-8", errors="ignore") as f:
                words = list({line.strip() for line in f if line.strip()})
            # Build-order independence is part of what is measured: the ordered index sorts anyway,
            # and a hash index must not be handed the sorted list a dictionary file happens to be.
            random.Random(0).shuffle(words)
            if words:
                print(f"keys: {len(words):,} real words from {path}")
                return words, path
    sys.exit(
        "no word list found. Pass a path as argv[1], set LEXINDEX_BENCH_WORDS, or install a "
        "system dictionary (e.g. `words` / `words-en`). Synthetic keys are deliberately refused."
    )


KEYS, WORDS_FILE = _load_words()
N = len(KEYS)
RAW = sum(len(k.encode()) for k in KEYS) / N


REPS = 5


def _time(fn) -> tuple[object, list[float]]:
    """`REPS` builds after a discarded warm-up, all of them returned. The warm-up matters for
    fairness: every competitor imports its module inside its build callable, and charging that
    one-time import (and the allocator's first growth) to the library would flatter lexindex, which
    is imported at the top of this file. The printed figure is the median, so one scheduling hiccup
    cannot move the bar; the results file keeps the minimum and every sample beside it."""
    fn()
    times = []
    for _ in range(REPS):
        t = time.perf_counter()
        out = fn()
        times.append((time.perf_counter() - t) * 1e3)
    return out, times


def _serialised_size(obj) -> int | None:
    """Bytes on disk for whatever serialisation the library offers."""
    for attr in ("to_bytes", "tobytes"):
        if hasattr(obj, attr):
            return len(getattr(obj, attr)())
    if hasattr(obj, "save"):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        try:
            obj.save(path)
            return os.path.getsize(path)
        finally:
            os.unlink(path)
    return None


PROBES = 100_000


def _probe_set() -> tuple[list[str], str]:
    """Half members, half strangers, shuffled with a fixed seed.

    Shuffled and not strided: a probe order that walks the keys at any fixed step is learned by the
    L2 stride prefetcher, and that has reversed a ranking in this repository before. The strangers
    are a member with its last character swapped for another the corpus uses, which keeps them
    inside every trie's alphabet -- a miss spelled with a character no key contains is rejected at
    the first node by a trie and still hashed in full by a hash index, which is not a comparison."""
    member = set(KEYS)
    alphabet = sorted({k[-1] for k in KEYS})
    rng = random.Random(0x5EED)
    probes: list[str] = []
    strangers: list[str] = []
    while len(probes) < PROBES:
        k = KEYS[rng.randrange(N)]
        probes.append(k)
        for _ in range(8):
            stranger = k[:-1] + rng.choice(alphabet)
            if stranger not in member:
                probes.append(stranger)
                strangers.append(stranger)
                break
    rng.shuffle(probes)
    return probes, strangers[0]


LOOKUPS, A_MISS = _probe_set()


def _lookup_fn(obj, exact: bool):
    """The exact-lookup call for whichever library built `obj`: `id` on a lexindex index, `get` on
    marisa-trie, dawg2 and datrie. It has to be *total* -- a `KeyError` on a stranger would time
    Python's exception machinery instead of the library's miss path -- so the resolved callable is
    tried on one member and one stranger before it is timed, and a library whose lookup cannot be
    resolved that way gets an empty cell rather than a number measured from something else."""
    for attr in ("id", "get"):
        fn = getattr(obj, attr, None)
        if not callable(fn):
            continue
        try:
            if fn(KEYS[0]) is None:
                return None
            missed = fn(A_MISS)
        except Exception:
            return None
        # A probabilistic index answers a stranger with an id by design; an exact one must not.
        return None if exact and missed is not None else fn
    return None


def _one_pass(fn) -> float:
    """One pass over the whole probe set, nanoseconds per lookup."""
    t = time.perf_counter_ns()
    for probe in LOOKUPS:
        fn(probe)
    return (time.perf_counter_ns() - t) / len(LOOKUPS)


def _lookup_latency(lanes: list[tuple[str, object]]) -> dict[str, list[float]]:
    """`REPS` rounds in which every lane takes one pass, after one discarded round.

    Alternating the lanes is the point. Run one library's five passes back to back and it is timed
    with the allocator and the caches still holding what its own build left there, while the next
    one starts cold -- the failure this repository has already been caught by twice on A/B work.
    Every structure is also alive while every other one is measured, which is a harder memory
    environment than holding one at a time and, unlike that, the same for every row."""
    for _, fn in lanes:
        _one_pass(fn)
    samples: dict[str, list[float]] = {name: [] for name, _ in lanes}
    for _ in range(REPS):
        for name, fn in lanes:
            samples[name].append(_one_pass(fn))
    return samples


CALL, DICT = "(the call alone)", "builtin dict"


class Built(NamedTuple):
    name: str
    obj: object | None
    caps: dict[str, int]
    build_ms: list[float] | None
    skipped: str | None


class Row(NamedTuple):
    name: str
    build_ms: float
    bytes_per_key: float | None
    lookup_ns: float | None
    caps: dict[str, int]


# (name, build-callable -> object, capabilities dict). Each build is wrapped so a missing/renamed
# dependency degrades to "skipped" instead of crashing the whole comparison.
def build_lexindex_string():
    return lexindex.StringIndex(KEYS)


def build_lexindex_mph():
    return lexindex.PerfectHashIndex(KEYS)


def build_lexindex_dict():
    return lexindex.DictIndex(KEYS)


def build_lexindex_dict128():
    # The block size is a knob, not a constant: 128 per block is where an ordered, reverse-capable
    # index goes under marisa on this corpus, paying for it in reverse-lookup latency.
    return lexindex.DictIndex(KEYS, block=128)


def build_lexindex_closed():
    return lexindex.ClosedHashIndex(KEYS)


def build_lexindex_compact4bit():
    return lexindex.CompactHashIndex(KEYS, fingerprint_bits=4)


def build_lexindex_compact1():
    return lexindex.CompactHashIndex(KEYS, 1)


def build_lexindex_compact2():
    return lexindex.CompactHashIndex(KEYS, 2)


def build_marisa():
    import marisa_trie

    return marisa_trie.Trie(KEYS)


# marisa is a curve, not a point, and every lexindex table until 2.1.1 quoted only the middle of it.
# Its own documentation says the right configuration depends on the data, so a single row invites
# the fair objection that the baseline was left untuned. Measured over the whole space on these
# words: `num_tries` 1/2/3/4/5/8/16/32 gives 3.380/2.997/2.978/2.977/2.977/2.980/2.986/2.998 --
# flat from three and worse past eight, not the monotone shrink the docs suggest -- `cache_size`
# TINY/SMALL/NORMAL/LARGE/HUGE gives 2.957/2.964/2.978/3.008/3.066, and `order` and `binary` do
# nothing at all. So the knobs worth a row are cache size, and the joint best is `num_tries=4` with
# `TINY_CACHE` at 2.955.
def build_marisa_compact():
    import marisa_trie

    return marisa_trie.Trie(KEYS, num_tries=4, cache_size=marisa_trie.TINY_CACHE)


def build_marisa_fast():
    import marisa_trie

    return marisa_trie.Trie(KEYS, cache_size=marisa_trie.HUGE_CACHE)


def build_dawg():
    import dawg  # provided by the `dawg2` distribution

    return dawg.IntCompletionDAWG(zip(KEYS, range(N), strict=True))


def build_datrie():
    import datrie

    alphabet = "".join(sorted({ch for k in KEYS for ch in k}))  # cover every character present
    t = datrie.Trie(alphabet)
    for i, k in enumerate(KEYS):
        t[k] = i
    return t


# caps keys: prefix, range, fuzzy, reverse (id->str), exact (exact vs probabilistic membership),
# serialise, mmap
CANDIDATES = [
    (
        "lexindex\nClosedHashIndex",
        build_lexindex_closed,
        dict(prefix=0, rangeq=0, fuzzy=0, reverse=0, exact=0, serialise=1, mmap=0),
    ),
    (
        "lexindex\nCompactHashIndex\n(fp=4 bits)",
        build_lexindex_compact4bit,
        dict(prefix=0, rangeq=0, fuzzy=0, reverse=0, exact=0, serialise=1, mmap=1),
    ),
    (
        "lexindex\nCompactHashIndex\n(fp=1)",
        build_lexindex_compact1,
        dict(prefix=0, rangeq=0, fuzzy=0, reverse=0, exact=0, serialise=1, mmap=1),
    ),
    (
        "lexindex\nCompactHashIndex\n(fp=2)",
        build_lexindex_compact2,
        dict(prefix=0, rangeq=0, fuzzy=0, reverse=0, exact=0, serialise=1, mmap=1),
    ),
    (
        "lexindex\nDictIndex\n(128 per block)",
        build_lexindex_dict128,
        dict(prefix=1, rangeq=1, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "marisa-trie\n(4 tries, tiny cache)",
        build_marisa_compact,
        dict(prefix=1, rangeq=0, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "marisa-trie\n(default)",
        build_marisa,
        dict(prefix=1, rangeq=0, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "marisa-trie\n(huge cache)",
        build_marisa_fast,
        dict(prefix=1, rangeq=0, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "lexindex\nDictIndex\n(32 per block, default)",
        build_lexindex_dict,
        dict(prefix=1, rangeq=1, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "lexindex\nStringIndex",
        build_lexindex_string,
        dict(prefix=1, rangeq=1, fuzzy=1, reverse=1, exact=1, serialise=1, mmap=1),
    ),
    (
        "DAWG\n(dawg2)",
        build_dawg,
        dict(prefix=1, rangeq=0, fuzzy=0, reverse=0, exact=1, serialise=1, mmap=0),
    ),
    (
        "datrie",
        build_datrie,
        dict(prefix=1, rangeq=0, fuzzy=0, reverse=0, exact=1, serialise=1, mmap=0),
    ),
    (
        "lexindex\nPerfectHashIndex",
        build_lexindex_mph,
        dict(prefix=0, rangeq=0, fuzzy=0, reverse=1, exact=1, serialise=1, mmap=1),
    ),
]


def main() -> None:
    built: list[Built] = []
    for name, build, caps in CANDIDATES:
        try:
            obj, samples = _time(build)
        except Exception as e:  # missing dep or an API drift → skip, note it
            print(f"skip {name.replace(chr(10), ' ')}: {type(e).__name__}: {e}")
            built.append(Built(name, None, caps, None, type(e).__name__))
            continue
        built.append(Built(name, obj, caps, samples, None))

    lanes: list[tuple[str, object]] = [
        (CALL, lambda _probe: None),
        (DICT, dict(zip(KEYS, range(N), strict=True)).get),
    ]
    for one in built:
        if one.obj is None:
            continue
        fn = _lookup_fn(one.obj, exact=bool(one.caps["exact"]))
        if fn is not None:
            lanes.append((one.name, fn))
    latency = _lookup_latency(lanes)
    floor, dict_ns = min(latency[CALL]), min(latency[DICT])
    print(
        f"\nPython call boundary: {floor:.0f} ns for the loop and the call alone -- every row "
        f"below pays it, the differences between rows do not.\nA builtin dict answers the same "
        f"probes in {dict_ns:.0f} ns, which is a lookup and not a floor."
    )

    rows: list[Row] = []
    cells = []
    for one in built:
        if one.obj is None or one.build_ms is None:
            cells.append({"library": one.name.replace(chr(10), " "), "skipped": one.skipped})
            continue
        build_ms = statistics.median(one.build_ms)
        size = _serialised_size(one.obj)
        bpk = size / N if size else None
        passes = latency.get(one.name)
        ns = min(passes) if passes else None
        rows.append(Row(one.name, build_ms, bpk, ns, one.caps))
        cells.append(
            {
                "library": one.name.replace(chr(10), " "),
                "build_ms": _results.summary(one.build_ms),
                "serialised_bytes": size,
                "bytes_per_key": bpk,
                "lookup_ns": _results.summary(passes) if passes else None,
                "capabilities": one.caps,
            }
        )
        shown = "—" if ns is None else f"{ns:5.0f} ns"
        print(
            f"{one.name.replace(chr(10), ' '):32} build {build_ms:7.0f} ms (median of {REPS})   "
            f"size {bpk if bpk is None else round(bpk, 2)} bytes/key   lookup {shown}"
        )

    false_positives = _measure_false_positive_rate()
    _plot_size(rows)
    _plot_build(rows)
    _plot_lookup(rows, floor, dict_ns)
    _capability_table(rows, dict_ns)
    path = _results.write(
        "compare",
        cells,
        keys={
            "source": WORDS_FILE,
            "n": N,
            "raw_bytes_per_key": RAW,
        },
        false_positive_rate=false_positives,
        python_call_floor_ns=_results.summary(latency[CALL]),
        builtin_dict_ns=_results.summary(latency[DICT]),
        competitors=_results.versions("marisa-trie", "dawg2", "datrie"),
    )
    print(f"\nplots → {OUT}/  (raw keys = {RAW:.1f} bytes/key, n = {N:,})")
    print(f"results → {path}")


def _measure_false_positive_rate() -> list[dict]:
    """The one honest cost of CompactHashIndex: a bounded chance a non-member reads as present."""
    member = set(KEYS)
    rng = random.Random(1234)
    probes = []
    while len(probes) < 100_000:
        s = "".join(chr(rng.randint(97, 122)) for _ in range(rng.randint(3, 12)))
        if s not in member:
            probes.append(s)
    print("\nCompactHashIndex membership false-positive rate (100k non-member probes):")
    measured = []
    for fp in (1, 2):
        ch = lexindex.CompactHashIndex(KEYS, fp)
        fps = sum(ch.contains(s) for s in probes)
        measured.append(
            {
                "fingerprint_bits": fp * 8,
                "probes": len(probes),
                "false_positives": fps,
                "rate": fps / len(probes),
                "theory": 256.0**-fp,
            }
        )
        print(
            f"  fp={fp}: {fps}/{len(probes)} = {fps / len(probes) * 100:.3f}%  "
            f"(theory {100 / 256**fp:.3f}%)"
        )
    return measured


def _plot_size(rows) -> None:
    labelled = [(r.name, r.bytes_per_key) for r in rows if r.bytes_per_key is not None]
    labelled.sort(key=lambda t: t[1])  # ascending: smallest index first
    fig, ax = plt.subplots(figsize=(9.5, 4.6))
    names = [n for n, _ in labelled] + ["raw keys\n(no index)"]
    vals = [b for _, b in labelled] + [RAW]
    colors = [
        "#00897b" if "CompactHash" in n else "#3949ab" if "lexindex" in n else "#9aa0a6"
        for n, _ in labelled
    ] + ["#cfcfcf"]
    bars = ax.bar(names, vals, color=colors, width=0.66)
    ax.bar_label(bars, fmt="%.2f", padding=3, fontsize=9)
    ax.set_ylabel("serialised bytes / key")
    ax.set_title(f"Serialised size on real English words (n = {N:,}, raw {RAW:.1f} B/key)")
    ax.spines[["top", "right"]].set_visible(False)
    ax.margins(y=0.16)
    ax.tick_params(axis="x", labelsize=8)
    fig.tight_layout()
    fig.savefig(OUT / "compare_size.png", dpi=140)
    plt.close(fig)


def _plot_build(rows) -> None:
    fig, ax = plt.subplots(figsize=(9.5, 4.6))
    names = [r.name for r in rows]
    vals = [r.build_ms for r in rows]
    bars = ax.bar(names, vals, color="#5e35b1", width=0.66)
    ax.bar_label(bars, fmt="%.0f ms", padding=3, fontsize=9)
    ax.set_ylabel("build time (ms)")
    ax.set_title(f"Build time on real English words (n = {N:,})")
    ax.spines[["top", "right"]].set_visible(False)
    ax.margins(y=0.16)
    ax.tick_params(axis="x", labelsize=8)
    fig.tight_layout()
    fig.savefig(OUT / "compare_build.png", dpi=140)
    plt.close(fig)


def _plot_lookup(rows, floor: float, dict_ns: float) -> None:
    """The counterweight to the size plot: what one lookup costs in the structure that small."""
    labelled = [(r.name, r.lookup_ns) for r in rows if r.lookup_ns is not None]
    labelled.sort(key=lambda t: t[1])
    fig, ax = plt.subplots(figsize=(9.5, 4.6))
    names = [n for n, _ in labelled] + ["builtin dict\n(in-RAM only)"]
    vals = [ns for _, ns in labelled] + [dict_ns]
    colors = [
        "#00897b" if "CompactHash" in n else "#3949ab" if "lexindex" in n else "#9aa0a6"
        for n, _ in labelled
    ] + ["#cfcfcf"]
    bars = ax.bar(names, vals, color=colors, width=0.66)
    ax.bar_label(bars, fmt="%.0f", padding=3, fontsize=9)
    ax.axhline(floor, color="#c62828", linewidth=1, linestyle="--")
    ax.text(
        len(names) - 0.5,
        floor,
        f" {floor:.0f} ns: the Python call itself",
        color="#c62828",
        fontsize=8,
        va="bottom",
        ha="right",
    )
    ax.set_ylabel("ns / lookup (through Python)")
    ax.set_title(
        f"Lookup latency on real English words (n = {N:,}, {PROBES:,} probes, half of them misses)"
    )
    ax.spines[["top", "right"]].set_visible(False)
    ax.margins(y=0.16)
    ax.tick_params(axis="x", labelsize=8)
    fig.tight_layout()
    fig.savefig(OUT / "compare_lookup.png", dpi=140)
    plt.close(fig)


def _capability_table(rows, dict_ns: float) -> None:
    cols = [
        ("prefix", "prefix"),
        ("rangeq", "range"),
        ("fuzzy", "fuzzy (edit dist.)"),
        ("reverse", "reverse id→str"),
        ("exact", "exact membership"),
        ("serialise", "serialisable"),
        ("mmap", "zero-copy mmap"),
    ]
    yes, no = "✅", "—"
    print("\n| library | " + " | ".join(c[1] for c in cols) + " | bytes/key | ns/lookup |")
    print("|" + "---|" * (len(cols) + 3))
    for row in rows:
        cells = [yes if row.caps[k] else no for k, _ in cols]
        print(
            f"| {row.name.replace(chr(10), ' ')} | "
            + " | ".join(cells)
            + " | "
            + (f"**{row.bytes_per_key:.2f}**" if row.bytes_per_key else "—")
            + " | "
            + ("—" if row.lookup_ns is None else f"{row.lookup_ns:.0f}")
            + " |"
        )
    print(f"| builtin `dict` | — | — | — | — | ✅ | — (in-RAM only) | — | — | {dict_ns:.0f} |")


if __name__ == "__main__":
    main()
