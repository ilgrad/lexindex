"""The latency model `plan_for` ranks by: measured here, fitted here.

`lexindex::plan_for(keys, needs, objective)` ranks its candidates, for `Objective::Latency` and
`Objective::Workload`, by a modelled cost of one operation -- `a + b*s + c*len + d*s*len`
nanoseconds per structure, op and `DictIndex` block, where `len` is the mean key length and `s` is
where the structure's blob sits against the cache (`_size`). This script is where the constants in
`src/estimate.rs` come from and the command that re-derives them.

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
from dataclasses import dataclass
from itertools import pairwise
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
SAMPLE = 100_000  # the plan's sample size; the model reads a smaller corpus as this many keys

# The op lanes `Objective::Workload` needs, past the mixed `id` the first model was fitted to.
# `prefix` is measured as `prefix_count` on purpose: what an enumeration costs is dominated by how
# many keys the prefix carries, which the planner cannot know, and the count is the part that is
# the structure's own. `batch` is measured at both ends of a decade, since a batch of sixteen and
# one of a thousand are not the same query.
OPS_PROBES = 20_000
CP_PROBES = 5_000
BATCHES = (16, 1_024)
# A round for each place in an op, so that `_time` puts every structure at every one: ten
# structures answer `id`.
ROUNDS = 10


def _fresh(xs: list[str]) -> list[str]:
    """`xs` as new strings, allocated in the order a pass asks them."""
    return [x.encode().decode() for x in xs]


class Probes:
    """One draw a lane a corpus, shared by every structure, so the lanes answer the same questions.

    Every lane draws a set of its own. Lanes cut from one set time each other's cache: while the
    hits and the misses were halves of the mixed set and both batches its chunks, each of them ran
    over the structure memory the lane before had just pulled in, and `ClosedHashIndex` at ten
    million keys read 266 ns mixed, 121 on hits and 55 batched -- most of that spread was the cache
    and not the op.

    No lane holds a member together with its stranger. `bench/_probes` spells a stranger from a
    member by swapping its last character, so an ordered index walks the pair through one block and
    finds it cached the second time: over 1 000 000 titles `StringIndex` read 572-575 ns on such
    pairs and 637-638 on members and strangers from two draws, `DictIndex 32` 426-433 and 452-455,
    and a hash index about 2 % apart. So the mixed lanes take their two halves from two draws.

    Every probe is a new string, allocated in the order it is asked, and every lane holds as many.
    A member drawn from `keys` is the corpus's own string, scattered through a heap as large as the
    corpus, and a pass paid for fetching it: over 1 000 000 titles `ClosedHashIndex` read 82-84 ns
    on hits drawn from it and 63-64 on hits copied, and `StringIndex` 635-648 and 596-608.
    """

    def __init__(self, keys: list[str]) -> None:
        member = set(keys)
        rng = random.Random(0x5EED)

        def draw(seed: int, members: bool) -> list[str]:
            probes, _ = _probes.probe_set(keys, 2 * OPS_PROBES, seed)
            return [p for p in probes if (p in member) == members][:OPS_PROBES]

        def mixed(seed: int) -> list[str]:
            half = OPS_PROBES // 2
            out = draw(seed, True)[:half] + draw(seed + 1, False)[:half]
            rng.shuffle(out)
            return out

        self.mixed = _fresh(mixed(0x5EED + 10))
        self.hits = _fresh(draw(0x5EED + 1, True))
        self.misses = _fresh(draw(0x5EED + 2, False))
        self.batches = {size: _fresh(mixed(0x5EED + 12 + 2 * i)) for i, size in enumerate(BATCHES)}
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
    """Read every probe's bytes, so that a pass is not timed fetching its own inputs.

    The bytes, not the hash: a string caches its hash in its header, so `hash` never reads the
    characters the binding does, and the first structure at each op paid for fetching them. Over
    100 000 titles `ClosedHashIndex`, first at `hit`, read 73-84 ns with `hash` and 68-69 with
    `encode`.
    """
    for x in xs:
        if isinstance(x, list):
            _touch(x)
        elif isinstance(x, str):
            x.encode()
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

    In op order rather than structure order, so no pass follows another of its own structure: the
    pass before it always read a different index, and a structure is as cold for its `hit` pass as
    for its `mixed` one. Timed in structure order, the ops of one index shared its warm cache down
    the row.

    Where in its op a pass runs moves it by a few per cent, and not the same way for every
    structure: over 100 000 titles `ClosedHashIndex` `mixed` read 72-80 ns as the first pass of
    every round and 67-72 moved along, where `PerfectHashIndex` `key` read a few per cent slower
    moved along. So each round starts every op one structure further along, there is a round for
    each place, and a structure keeps its fastest.
    """
    ops = [[lane for lane in lanes if lane[0][1] == op] for op in OPS]
    ops = [op for op in ops if op]
    assert all(len(op) <= ROUNDS for op in ops), "more structures at an op than rounds"
    passes = [
        lane for r in range(ROUNDS) for op in ops for lane in op[r % len(op) :] + op[: r % len(op)]
    ]
    assert all(a[0][0] != b[0][0] for a, b in pairwise(passes)), "a pass follows its own structure"
    best: dict[tuple[str, str], float] = {}
    for op in ops:
        for _, _, xs, fn in op:
            _walk(fn, xs)
    for key, calls, xs, fn in passes:
        _touch(xs)
        start = time.perf_counter_ns()
        _walk(fn, xs)
        ns = (time.perf_counter_ns() - start) / calls
        best[key] = min(best.get(key, float("inf")), ns)
    return best


class _Unchecked:
    """A `HashedDictIndex` timed through `id_unchecked` under the name `_lanes` looks up.

    At zero fingerprint bits `id` is the dictionary's own search, so its lanes would time
    `DictIndex 256` a second time; the closed-vocabulary path is the one worth a lane. It has no
    batch form, so this lane has no `ids_of`, and the index's own keys and prefixes are its
    dictionary's, already timed.
    """

    def __init__(self, index) -> None:
        self.id = index.id_unchecked
        self.to_bytes = index.to_bytes


def _hashed(keys: list[str], bits: int):
    return lexindex.HashedDictIndex.from_dict(lexindex.DictIndex(keys, block=256), bits)


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
    # Measured beside the rest so that the constants a planner would price it by come from the same
    # cache conditions; `fit` leaves them out until `plan` has a `Kind` to file them under.
    lanes.append(("HashedDictIndex 256 fp=8", lambda k: _hashed(k, 8), True))
    lanes.append(("HashedDictIndex 256 unchecked", lambda k: _Unchecked(_hashed(k, 0)), True))
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
    """Normal equations with partial pivoting. Four unknowns over thirty points."""
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


# Below this a blob is priced as if it were this large: `s` is zero there.
HINGE = 64 * 1024


def _size(cell: dict) -> float:
    """`s`: `log2` of the structure's blob over `HINGE`, and zero below it.

    The blob and not the key count, because what a lookup waits on is how far its bytes are from
    the CPU, and a key count says that only once it is multiplied by bytes a key that vary 60-fold
    between the corpora here. A corpus under `SAMPLE` keys is read at its `SAMPLE`-key equivalent,
    as the plan reads it: nothing below that was measured, and a smaller corpus is not slower.
    """
    blob = cell["bytes_per_key"] * max(cell["keys"], SAMPLE)
    return max(math.log2(blob / HINGE), 0.0)


def _features(cell: dict, floors: tuple[float, float] = (0.0, 0.0)) -> list[float]:
    s = max(_size(cell), floors[0])
    # `raw_bytes_per_key` is the mean key length, which is what `Plan::shape` gives the model.
    n = max(cell["raw_bytes_per_key"], floors[1])
    return [1.0, s, n, s * n]


def _nonnegative(rows: list[list[float]], y: list[float]) -> list[float]:
    """Least squares on each cell's error as a share of it, with every slope at zero or above.

    A share, because the cells run from 50 ns to 4 000: fitted on nanoseconds, the slowest cells
    decide the constants, and a ranking needs the fast ones as right as the slow. No slope below
    zero, because no op gets cheaper as its blob or its keys grow: a negative one is the fit bending
    to noise, and the model would carry it to every corpus it prices. The most negative is dropped
    and the rest refitted until none is left.
    """
    weighted = [[x / t for x in row] for row, t in zip(rows, y, strict=True)]
    active = list(range(len(rows[0])))
    while True:
        solved = _lstsq([[row[i] for i in active] for row in weighted], [1.0] * len(y))
        constants = [0.0] * len(rows[0])
        for i, v in zip(active, solved, strict=True):
            constants[i] = v
        negative = [i for i in active if i > 0 and constants[i] < 0]
        if not negative:
            return constants
        active.remove(min(negative, key=lambda i: constants[i]))


@dataclass(frozen=True)
class Model:
    """Every structure's constants at every op it answers, and where the evidence for them stops.

    Below the smallest `s` a structure was measured at, and below the shortest keys of any corpus,
    the model holds the value at that edge rather than carry a slope past it. Above, it carries
    them: a longer key is more bytes to hash and compare, and a larger blob is further from the CPU.
    """

    len_floor: float
    floors: dict[str, float]
    constants: dict[tuple[str, str], list[float]]

    @classmethod
    def fit(cls, cells: list[dict]) -> Model:
        rows = {
            c["structure"]: [r for r in cells if r["structure"] == c["structure"]] for c in cells
        }
        return cls(
            len_floor=min(c["raw_bytes_per_key"] for c in cells),
            floors={name: min(_size(c) for c in own) for name, own in rows.items()},
            constants={
                (name, op): _nonnegative(
                    [_features(c) for c in own if c["ops"].get(op)],
                    [c["ops"][op] for c in own if c["ops"].get(op)],
                )
                for name, own in rows.items()
                for op in OPS
                if any(c["ops"].get(op) for c in own)
            },
        )

    def nanos(self, name: str, op: str, cell: dict) -> float:
        at = _features(cell, (self.floors[name], self.len_floor))
        return sum(k * x for k, x in zip(self.constants[(name, op)], at, strict=True))


def _newest(pattern: str) -> Path:
    found = sorted(_results.RESULTS.glob(pattern))
    if not found:
        sys.exit(f"no {pattern} in {_results.RESULTS}")
    return found[-1]


def fit(artifact: Path | None) -> int:
    path = artifact or _newest("latency-model-*.json")
    cells = [
        c
        for c in json.loads(path.read_text(encoding="utf-8"))["cells"]
        if c.get("ops") and not c["structure"].startswith("HashedDictIndex")
    ]
    print(f"fitting {path.name}: {len(cells)} cells\n")
    model = Model.fit(cells)
    print(f"{'structure':<24}{'op':<16}{'a':>8}{'b':>8}{'c':>8}{'d':>8}   mean err   worst")
    for (name, op), (a, b, c, d) in sorted(
        model.constants.items(), key=lambda item: (item[0][0], OPS.index(item[0][1]))
    ):
        rows = [r for r in cells if r["structure"] == name and r["ops"].get(op)]
        err = [abs(r["ops"][op] - model.nanos(name, op, r)) / r["ops"][op] for r in rows]
        print(
            f"{name:<24}{op:<16}{a:8.1f}{b:8.2f}{c:8.3f}{d:8.3f}"
            f"{sum(err) / len(err) * 100:9.1f}%{max(err) * 100:8.1f}%"
        )

    print(f"\nsrc/estimate.rs:\n{_rust(model)}\n")

    published = [
        c
        for f in (_newest("sweep-*.json"), _newest("sweep10m-*.json"))
        for c in json.loads(f.read_text(encoding="utf-8"))["cells"]
        if c["structure"] in model.floors and c["lookup_ns"]
    ]
    # Three scores, not one. Over every lane the published sweep holds, `ClosedHashIndex` wins
    # almost every cell outright, so that number flatters the model; the comparisons that decide
    # anything are the ones between structures a caller is actually choosing between.
    everything = sorted({c["structure"] for c in published})
    _score(model, published, everything, "every lane")
    _score(model, published, ["DictIndex 256", "StringIndex"], "ordered: Dict 256 or String")
    _score(model, published, [s for s in everything if s.startswith("Dict")], "which Dict block")
    print()
    _held_out(cells)
    return 0


def _score(model: Model, cells: list[dict], lanes: list[str], label: str) -> None:
    """How often the model names the structure the measurement ranks fastest, over `lanes`, and
    what its worst pick costs against that fastest."""
    groups: dict[tuple[str, int], dict[str, dict]] = {}
    for c in cells:
        if c["structure"] in lanes:
            groups.setdefault((c["corpus"], c["size"]), {})[c["structure"]] = c
    ok = total = 0
    worst = 1.0
    per: dict[int, list[int]] = {}
    for (_, size), row in sorted(groups.items()):
        if len(row) < len(lanes):
            continue
        total += 1
        per.setdefault(size, [0, 0])[1] += 1
        measured = min(row, key=lambda s: row[s]["lookup_ns"])
        modelled = min(row, key=lambda s: model.nanos(s, "mixed", row[s]))
        worst = max(worst, row[modelled]["lookup_ns"] / row[measured]["lookup_ns"])
        if measured == modelled:
            ok += 1
            per[size][0] += 1
    spread = " ".join(f"{k // 1000}k: {v[0]}/{v[1]}" for k, v in sorted(per.items()))
    print(f"{label:<30} {ok:>2}/{total}   worst {worst:.3f}   {spread}")


# What `_held_out` scores the model on: a label, the weight of each op, and the ops a candidate has
# to answer besides, which is how "ordered" narrows `id` to the structures with a prefix query.
WORKLOADS = (
    ("id", {"mixed": 1}, ()),
    ("id, ordered", {"mixed": 1}, ("prefix_count",)),
    ("9 hits to a miss", {"hit": 9, "miss": 1}, ()),
    ("9 hits to a miss, ordered", {"hit": 9, "miss": 1}, ("prefix_count",)),
    ("batches of 1 024", {"batch1024": 1}, ()),
    ("batches of 1 024, ordered", {"batch1024": 1}, ("prefix_count",)),
    ("a key(id) to a hit", {"key": 1, "hit": 1}, ()),
    ("9 prefix counts to a hit", {"prefix_count": 9, "hit": 1}, ()),
    ("9 common_prefix to a hit", {"common_prefix": 9, "hit": 1}, ()),
    ("longest_prefix", {"longest_prefix": 1}, ()),
)


def _held_out(cells: list[dict]) -> None:
    """Fitted without a corpus, what does the model's pick cost on it against the fastest?

    For each workload over every corpus and size: how often the pick is the fastest, and the mean
    and worst of what it costs over what the fastest does -- which is what a wrong pick costs.
    """
    corpora_ = sorted({c["corpus"] for c in cells})
    held = {name: Model.fit([c for c in cells if c["corpus"] != name]) for name in corpora_}
    print(f"{'held out, a corpus at a time':<34}{'fastest':>9}{'mean':>7}{'worst':>7}")
    for label, weights, needs in WORKLOADS:
        right, ratios, worst = 0, [], (1.0, "")
        for name, model in held.items():
            rows: dict[int, dict[str, dict]] = {}
            for c in cells:
                if c["corpus"] == name and all(c["ops"].get(op) for op in (*weights, *needs)):
                    rows.setdefault(c["size"], {})[c["structure"]] = c
            for size, row in sorted(rows.items()):
                cost = {
                    s: sum(w * c["ops"][op] for op, w in weights.items()) for s, c in row.items()
                }
                guess = {
                    s: sum(w * model.nanos(s, op, c) for op, w in weights.items())
                    for s, c in row.items()
                }
                best, pick = min(cost, key=cost.__getitem__), min(guess, key=guess.__getitem__)
                right += best == pick
                ratios.append(cost[pick] / cost[best])
                if ratios[-1] > worst[0]:
                    worst = (ratios[-1], f"{name} {size:,}: {pick} for {best}")
        print(
            f"  {label:<32}{right:>4}/{len(ratios):<4}"
            f"{sum(ratios) / len(ratios):7.3f}{worst[0]:7.3f}  {worst[1]}"
        )


# `src/estimate.rs` files a structure's constants under its `Kind`, and an op under its `Ops` field.
KINDS = {
    "ClosedHashIndex": "Closed",
    "CompactHashIndex fp=1": "Compact",
    "PerfectHashIndex": "Perfect",
    "StringIndex": "String",
}
FIELDS = {op: "prefix" if op == "prefix_count" else op for op in OPS}
OPTIONAL = ("key", "prefix_count", "common_prefix", "longest_prefix")


def _rust(model: Model) -> str:
    """The tables of `src/estimate.rs`, laid out as `cargo fmt` lays them."""

    def row(head: str, name: str) -> list[str]:
        out = ["    (", f"        {head},", "        Ops {"]
        out.append(f"            floor: {model.floors[name]:.2f},")
        for op in OPS:
            constants = model.constants.get((name, op))
            value = (
                "None"
                if constants is None
                else "cost({:.1f}, {:.2f}, {:.3f}, {:.3f})".format(*constants)
            )
            if constants is not None and op in OPTIONAL:
                value = f"Some({value})"
            out.append(f"            {FIELDS[op]}: {value},")
        return [*out, "        },", "    ),"]

    blocks = sorted(int(name.split()[1]) for name in model.floors if name.startswith("DictIndex "))
    lines = [f"const LEN_FLOOR: f64 = {model.len_floor:.2f};", ""]
    lines.append(f"const COST: [(Kind, Ops); {len(KINDS)}] = [")
    for name, kind in KINDS.items():
        lines += row(f"Kind::{kind}", name)
    lines += ["];", "", f"const DICT_COST: [(usize, Ops); {len(blocks)}] = ["]
    for block in blocks:
        lines += row(str(block), f"DictIndex {block}")
    return "\n".join([*lines, "];"])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("measure", help="time every structure over every corpus at every size")
    fitter = sub.add_parser(
        "fit", help="the constants, and their scores on the published sweep and held-out corpora"
    )
    fitter.add_argument("artifact", nargs="?", type=Path)
    args = parser.parse_args()
    return measure() if args.command == "measure" else fit(args.artifact)


if __name__ == "__main__":
    sys.exit(main())
