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
imported at the top of this file; the others import inside their build callable). Lookup latency
is deliberately *not* measured here — at the Python level it is dominated by the call boundary;
`cargo run --release --example bench` measures it in Rust.

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
        "marisa-trie",
        build_marisa,
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
    rows, cells = [], []
    for name, build, caps in CANDIDATES:
        try:
            obj, samples = _time(build)
        except Exception as e:  # missing dep or an API drift → skip, note it
            print(f"skip {name.replace(chr(10), ' ')}: {type(e).__name__}: {e}")
            cells.append({"library": name.replace(chr(10), " "), "skipped": f"{type(e).__name__}"})
            continue
        build_ms = statistics.median(samples)
        size = _serialised_size(obj)
        bpk = size / N if size else None
        rows.append((name, build_ms, bpk, caps))
        cells.append(
            {
                "library": name.replace(chr(10), " "),
                "build_ms": _results.summary(samples),
                "serialised_bytes": size,
                "bytes_per_key": bpk,
                "capabilities": caps,
            }
        )
        print(
            f"{name.replace(chr(10), ' '):32} build {build_ms:7.0f} ms (median of {REPS})   "
            f"size {bpk if bpk is None else round(bpk, 2)} bytes/key"
        )

    false_positives = _measure_false_positive_rate()
    _plot_size(rows)
    _plot_build(rows)
    _capability_table(rows)
    path = _results.write(
        "compare",
        cells,
        keys={
            "source": WORDS_FILE,
            "n": N,
            "raw_bytes_per_key": RAW,
        },
        false_positive_rate=false_positives,
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
    labelled = [(n, b) for n, _, b, _ in rows if b is not None]
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
    names = [n for n, _, _, _ in rows]
    vals = [ms for _, ms, _, _ in rows]
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


def _capability_table(rows) -> None:
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
    print("\n| library | " + " | ".join(c[1] for c in cols) + " | bytes/key |")
    print("|" + "---|" * (len(cols) + 2))
    for name, _, bpk, caps in rows:
        cells = [yes if caps[k] else no for k, _ in cols]
        print(
            f"| {name.replace(chr(10), ' ')} | "
            + " | ".join(cells)
            + " | "
            + (f"**{bpk:.2f}**" if bpk else "—")
            + " |"
        )
    print("| builtin `dict` | — | — | — | — | ✅ | — (in-RAM only) | — | — |")


if __name__ == "__main__":
    main()
