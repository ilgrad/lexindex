"""The latency model behind `Objective::Latency`: measured here, fitted here.

`lexindex::plan_for(keys, needs, Objective::Latency)` ranks its candidates by a modelled cost of
one `id(key)` -- `a + b*log2(n / 100_000) + c*mean_len` nanoseconds, per structure and per
`DictIndex` block. This script is where the constants in `src/estimate.rs` come from and the
command that re-derives them.

`bench/sweep.py` is the published size table and leaves `PerfectHashIndex` out; a ranking model
needs it. Constants fitted across two runs would be constants fitted across two cache conditions --
the lanes alive during a pass are part of what each lane measures -- so every structure is timed
here in one process, against one set of rivals. The published sweep is then an independent set:
`fit` scores the model on it, and it was not fitted to it.

The numbers are timed through the Python binding, so each carries that call. The overhead falls on
every candidate alike, which is why the fit is quoted as an ordering and never as a latency.

Run:
  uv run --no-sync python bench/latency_model.py measure
  uv run --no-sync python bench/latency_model.py fit
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from pathlib import Path

import _probes  # noqa: F401  (imported for the side effect sweep.py depends on)
import _results
import corpora
import lexindex
import sweep

# `DictIndex` is a curve in the block, so both ends of 32..=1024 are measured rather than
# interpolated from the middle.
BLOCKS = (32, 128, 256, 1024)
SIZES = (100_000, 1_000_000, 10_000_000)
SAMPLE = 100_000  # the plan's sample size, and where the model reads its intercept


def structures():
    lanes = [
        ("ClosedHashIndex", lexindex.ClosedHashIndex, False),
        ("CompactHashIndex fp=1", lambda k: lexindex.CompactHashIndex(k, 1), False),
        ("PerfectHashIndex", lexindex.PerfectHashIndex, True),
    ]
    for block in BLOCKS:
        lanes.append(
            (f"DictIndex {block}", lambda k, b=block: lexindex.DictIndex(k, block=b), True)
        )
    lanes.append(("StringIndex", lexindex.StringIndex, True))
    return lanes


def measure() -> int:
    sweep._structures = structures
    manifest = json.loads(corpora.MANIFEST.read_text(encoding="utf-8"))
    cells: list[dict] = []
    for name, entry in manifest["corpora"].items():
        for one in entry["files"]:
            if one["keys"] not in SIZES:
                continue
            path = corpora.ROOT / one["file"]
            if not path.exists():
                print(f"{name}: {one['file']} not built, skipped")
                continue
            cells.extend(sweep._one(path, name, one["keys"], 1))
    if not cells:
        sys.exit("nothing measured: build the corpora first (`python bench/corpora.py build`)")
    path = _results.write(
        "latency-model",
        cells,
        corpora=str(corpora.ROOT),
        build_repeats=1,
        probes=sweep.PROBES,
        rounds=sweep.ROUNDS,
    )
    print(f"\nresults -> {path}")
    median = statistics.median(c["build_ms"] for c in cells)
    print(f"median build over {len(cells)} cells: {median:.0f} ms")
    return 0


def _lstsq(rows: list[list[float]], y: list[float]) -> list[float]:
    """Normal equations with partial pivoting. Three unknowns over thirty points."""
    n = len(rows[0])
    m = [
        [sum(rows[k][i] * rows[k][j] for k in range(len(rows))) for j in range(n)]
        + [sum(rows[k][i] * y[k] for k in range(len(rows)))]
        for i in range(n)
    ]
    for i in range(n):
        p = max(range(i, n), key=lambda r: abs(m[r][i]))
        m[i], m[p] = m[p], m[i]
        for r in range(n):
            if r != i:
                f = m[r][i] / m[i][i]
                for c in range(i, n + 1):
                    m[r][c] -= f * m[i][c]
    return [m[i][n] / m[i][i] for i in range(n)]


def _features(cell: dict) -> list[float]:
    # `raw_bytes_per_key` is the mean key length, which is what `Plan::shape` gives the model.
    return [1.0, math.log2(cell["size"] / SAMPLE), cell["raw_bytes_per_key"]]


def _newest(pattern: str) -> Path:
    found = sorted(_results.RESULTS.glob(pattern))
    if not found:
        sys.exit(f"no {pattern} in {_results.RESULTS}")
    return found[-1]


def fit(artifact: Path | None) -> int:
    path = artifact or _newest("latency-model-*.json")
    cells = [c for c in json.loads(path.read_text(encoding="utf-8"))["cells"] if c["lookup_ns"]]
    print(f"fitting {path.name}: {len(cells)} cells\n")
    fits: dict[str, list[float]] = {}
    print(f"{'structure':<24}{'a':>9}{'b':>9}{'c':>8}   mean err   worst")
    for name in sorted({c["structure"] for c in cells}):
        rows = [c for c in cells if c["structure"] == name]
        a, b, c = _lstsq([_features(r) for r in rows], [r["lookup_ns"] for r in rows])
        fits[name] = [a, b, c]
        err = [
            abs(r["lookup_ns"] - (a + b * _features(r)[1] + c * _features(r)[2])) / r["lookup_ns"]
            for r in rows
        ]
        print(
            f"{name:<24}{a:9.1f}{b:9.2f}{c:8.2f}"
            f"{sum(err) / len(err) * 100:9.1f}%{max(err) * 100:8.1f}%"
        )

    print("\nsrc/estimate.rs:")
    for name, (a, b, c) in fits.items():
        print(f"    // {name}\n    Cost {{ a: {a:.1f}, b: {b:.2f}, c: {c:.2f} }},")

    published = [
        c
        for f in (_newest("sweep-*.json"), _newest("sweep10m-*.json"))
        for c in json.loads(f.read_text(encoding="utf-8"))["cells"]
        if c["structure"] in fits and c["lookup_ns"]
    ]
    # Three scores, not one. Over every lane the published sweep holds, `ClosedHashIndex` wins
    # almost every cell outright, so that number flatters the model; the comparisons that decide
    # anything are the ones between structures a caller is actually choosing between.
    print()
    everything = sorted({c["structure"] for c in published})
    _score(fits, published, everything, "every lane")
    _score(fits, published, ["DictIndex 256", "StringIndex"], "ordered: Dict 256 or String")
    _score(fits, published, [s for s in everything if s.startswith("Dict")], "which Dict block")
    return 0


def _score(fits: dict, cells: list[dict], lanes: list[str], label: str) -> None:
    """How often the model names the structure the measurement ranks fastest, over `lanes`."""
    groups: dict[tuple[str, int], dict[str, float]] = {}
    shape: dict[tuple[str, int], dict] = {}
    for c in cells:
        if c["structure"] not in lanes:
            continue
        groups.setdefault((c["corpus"], c["size"]), {})[c["structure"]] = c["lookup_ns"]
        shape[(c["corpus"], c["size"])] = c
    ok = total = 0
    per: dict[int, list[int]] = {}
    for cell, row in sorted(groups.items()):
        if len(row) < len(lanes):
            continue
        total += 1
        per.setdefault(cell[1], [0, 0])[1] += 1
        at = _features(shape[cell])
        measured = min(row, key=row.get)
        modelled = min(lanes, key=lambda s: sum(f * x for f, x in zip(fits[s], at, strict=True)))
        if measured == modelled:
            ok += 1
            per[cell[1]][0] += 1
    spread = " ".join(f"{k // 1000}k: {v[0]}/{v[1]}" for k, v in sorted(per.items()))
    print(f"{label:<30} {ok:>2}/{total}   {spread}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("measure", help="time every structure over every corpus at every size")
    fitter = sub.add_parser("fit", help="least squares, and the score on the published sweep")
    fitter.add_argument("artifact", nargs="?", type=Path)
    args = parser.parse_args()
    return measure() if args.command == "measure" else fit(args.artifact)


if __name__ == "__main__":
    sys.exit(main())
