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
import random
import statistics
import sys
import time
from pathlib import Path

import _probes
import _results
import corpora
import lexindex
import sweep

# `DictIndex` is a curve in the block, so both ends of 32..=1024 are measured rather than
# interpolated from the middle.
BLOCKS = (32, 128, 256, 1024)
SIZES = (100_000, 1_000_000, 10_000_000)
SAMPLE = 100_000  # the plan's sample size, and where the model reads its intercept

# The op lanes `Objective::Workload` needs, past the mixed `id` the first model was fitted to.
# `prefix` is measured as `prefix_count` on purpose: what an enumeration costs is dominated by how
# many keys the prefix carries, which the planner cannot know, and the count is the part that is
# the structure's own. `batch` is measured at both ends of a decade, since a batch of sixteen and
# one of a thousand are not the same query.
OPS_PROBES = 20_000
CP_PROBES = 5_000
BATCHES = (16, 1_024)
ROUNDS = 3


class Probes:
    """One draw a lane a corpus, shared by every structure, so the lanes answer the same questions.

    The mixed set is `bench/_probes`' -- half members, half strangers, shuffled -- which is what
    the size model's `lookup_ns` was measured over and what keeps the two comparable. Every other
    lane draws a set of its own. Lanes cut from one set time each other's cache: while the hits and
    the misses were its halves and both batches its chunks, each of them ran over the structure
    memory the lane before had just pulled in, and `ClosedHashIndex` at ten million keys read 266 ns
    mixed, 121 on hits and 55 batched -- most of that spread was the cache and not the op.
    """

    def __init__(self, keys: list[str]) -> None:
        self.mixed, _ = _probes.probe_set(keys, OPS_PROBES)
        member = set(keys)
        # Hits and misses from two draws as well: a stranger is its member with the last character
        # swapped, so in an ordered index the two walk the same block.
        hits, _ = _probes.probe_set(keys, OPS_PROBES, 0x5EED + 1)
        misses, _ = _probes.probe_set(keys, OPS_PROBES, 0x5EED + 2)
        self.hits = [p for p in hits if p in member]
        self.misses = [p for p in misses if p not in member]
        self.batches = {
            size: _probes.probe_set(keys, OPS_PROBES, 0x5EED + 3 + i)[0]
            for i, size in enumerate(BATCHES)
        }
        rng = random.Random(0x5EED)
        self.ids = [rng.randrange(len(keys)) for _ in range(OPS_PROBES)]
        self.prefixes = [self.hits[rng.randrange(len(self.hits))][:3] for _ in range(OPS_PROBES)]
        # The `common_prefix` protocol of `docs/benchmarks.md`: a real key with one to three more
        # characters on it, except every fourth, which is characters alone and matches nothing.
        # Two sets, since `longest_prefix` over the same queries would walk the blocks
        # `common_prefix` had just read.
        alphabet = sorted({k[-1] for k in keys})

        def queries() -> list[str]:
            out = []
            for i in range(CP_PROBES):
                base = keys[rng.randrange(len(keys))]
                extra = "".join(rng.choice(alphabet) for _ in range(rng.randint(1, 3)))
                out.append(extra if i % 4 == 3 else base + extra)
            return out

        self.queries = queries()
        self.longest = queries()


# The order a round times its lanes in: every structure's pass at one op, then the next op.
OPS = (
    "mixed",
    "hit",
    "miss",
    *(f"batch{size}" for size in BATCHES),
    "key",
    "prefix_count",
    "common_prefix",
    "longest_prefix",
)


def _walk(fn, xs) -> None:
    for x in xs:
        fn(x)


def _touch(xs: list) -> None:
    """Read every probe once, so that a pass is not timed fetching its own inputs."""
    for x in xs:
        if isinstance(x, list):
            _touch(x)
        else:
            hash(x)


def _lanes(obj, probe: Probes) -> list[tuple[str, int, list, object]]:
    """`(op, calls, probes, fn)` for each op the structure answers: a pass calls `fn` per probe."""
    out: list[tuple[str, int, list, object]] = []
    ident = getattr(obj, "id", None)
    if ident is not None:
        for op, xs in (("mixed", probe.mixed), ("hit", probe.hits), ("miss", probe.misses)):
            out.append((op, len(xs), xs, ident))
    ids_of = getattr(obj, "ids_of", None)
    if ids_of is not None:
        for size, xs in probe.batches.items():
            chunks = [xs[i : i + size] for i in range(0, len(xs), size)]
            out.append((f"batch{size}", len(xs), chunks, ids_of))
    for op, xs in (
        ("key", probe.ids),
        ("prefix_count", probe.prefixes),
        ("common_prefix", probe.queries),
        ("longest_prefix", probe.longest),
    ):
        fn = getattr(obj, op, None)
        if fn is not None:
            out.append((op, len(xs), xs, fn))
    return out


def _time(
    lanes: list[tuple[tuple[str, str], int, list, object]],
) -> dict[tuple[str, str], float]:
    """One pass a lane a round, the minimum of the rounds kept.

    In op order rather than structure order, so no lane follows another of its own structure: the
    pass before it always read a different index, and a structure is as cold for its `hit` pass as
    for its `mixed` one. Timed in structure order, the ops of one index shared its warm cache down
    the row.
    """
    lanes = sorted(lanes, key=lambda lane: OPS.index(lane[0][1]))
    best: dict[tuple[str, str], float] = {}
    for _, _, xs, fn in lanes:
        _walk(fn, xs)
    for _ in range(ROUNDS):
        for key, calls, xs, fn in lanes:
            _touch(xs)
            start = time.perf_counter_ns()
            _walk(fn, xs)
            ns = (time.perf_counter_ns() - start) / calls
            best[key] = min(best.get(key, float("inf")), ns)
    return best


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


def _cell(path: Path, corpus: str, size: int) -> list[dict]:
    """Every structure over one corpus, built once and then timed on every op it answers."""
    keys = path.read_text(encoding="utf-8").splitlines()
    raw = sum(len(k.encode()) for k in keys) / len(keys)
    print(f"\n{corpus} {len(keys):,} keys, raw {raw:.2f} B/key  ({path.name})")
    probe = Probes(keys)
    cells, lanes, alive = [], [], []
    for name, build, keeps in structures():
        start = time.perf_counter()
        obj = build(keys)
        build_ms = (time.perf_counter() - start) * 1e3
        alive.append(obj)
        lanes.extend(((name, op), calls, xs, fn) for op, calls, xs, fn in _lanes(obj, probe))
        cells.append(
            {
                "corpus": corpus,
                "size": size,
                "keys": len(keys),
                "raw_bytes_per_key": raw,
                "structure": name,
                "keeps_keys": keeps,
                "bytes_per_key": sweep._size(obj) / len(keys),
                "build_ms": build_ms,
            }
        )
    timed = _time(lanes)
    for cell in cells:
        ops = {op: ns for (name, op), ns in timed.items() if name == cell["structure"]}
        cell["ops"] = ops
        cell["lookup_ns"] = ops.get("mixed")
        print(
            f"  {cell['structure']:<24}{cell['bytes_per_key']:8.3f} B/key"
            f"{cell['build_ms']:9.0f} ms   " + "  ".join(f"{op} {ns:.0f}" for op, ns in ops.items())
        )
    alive.clear()
    return cells


def measure() -> int:
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
            cells.extend(_cell(path, name, one["keys"]))
    if not cells:
        sys.exit("nothing measured: build the corpora first (`python bench/corpora.py build`)")
    path = _results.write(
        "latency-model",
        cells,
        corpora=str(corpora.ROOT),
        build_repeats=1,
        probes=OPS_PROBES,
        common_prefix_probes=CP_PROBES,
        batches=list(BATCHES),
        rounds=ROUNDS,
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
