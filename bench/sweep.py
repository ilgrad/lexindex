"""Every structure over the whole corpus set, because a size is a claim about keys.

`bench/compare.py` answers "how big is each structure on English words". This answers the question
that one cannot: **where** is each structure smallest. A trie's size depends on how much the keys
share, a fingerprint index's does not, and the gap between those two facts is the whole ranking —
`marisa-trie` measures 2.12 bytes a key on one corpus and 6.21 on another while `CompactHashIndex`
sits at 1.26 on both.

Reads the corpora `bench/corpora.py` builds, at the sizes asked for, and measures each structure's
serialised bytes per key, its build time, and one lookup. `marisa-trie` appears three times because
it is a curve — its own documentation says the right configuration depends on the data, and this
is the script that can show the data it depends on. `DictIndex` appears at three block sizes for the
same reason: the block is a knob, not a constant.

Run:
  uv run --no-sync --with marisa-trie python bench/sweep.py                    # 10^5 and 10^6
  uv run --no-sync --with marisa-trie python bench/sweep.py --sizes 10000000
  uv run --no-sync --with marisa-trie python bench/sweep.py --corpora dna uuid
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from pathlib import Path

import _probes
import _results
import corpora
import lexindex

PROBES = 20_000
ROUNDS = 3


def _structures():
    """Name, builder, and whether the keys are kept — which is what the whole table is about."""
    import marisa_trie

    return [
        ("ClosedHashIndex", lexindex.ClosedHashIndex, False),
        ("CompactHashIndex fp=1", lambda keys: lexindex.CompactHashIndex(keys, 1), False),
        ("DictIndex 128", lambda keys: lexindex.DictIndex(keys, block=128), True),
        ("DictIndex 256", lambda keys: lexindex.DictIndex(keys, block=256), True),
        ("DictIndex 1024", lambda keys: lexindex.DictIndex(keys, block=1024), True),
        ("StringIndex", lexindex.StringIndex, True),
        (
            "marisa compact",
            lambda keys: marisa_trie.Trie(keys, num_tries=4, cache_size=marisa_trie.TINY_CACHE),
            True,
        ),
        ("marisa default", marisa_trie.Trie, True),
        (
            "marisa fast",
            lambda keys: marisa_trie.Trie(keys, cache_size=marisa_trie.HUGE_CACHE),
            True,
        ),
    ]


def _size(obj) -> int:
    for attr in ("to_bytes", "tobytes"):
        if hasattr(obj, attr):
            return len(getattr(obj, attr)())
    raise TypeError(f"{type(obj).__name__} has no serialised form")


def _lookup(obj, a_miss: str, sample: str, exact: bool):
    """`id` on a lexindex index, `get` on marisa; checked on a member and a stranger first, so a
    `KeyError` path is never timed as if it were the structure's own."""
    for attr in ("id", "get"):
        fn = getattr(obj, attr, None)
        if not callable(fn):
            continue
        try:
            if fn(sample) is None:
                return None
            missed = fn(a_miss)
        except Exception:
            return None
        return None if exact and missed is not None else fn
    return None


def _latency(lanes, probes: list[str]) -> dict[str, float]:
    """One pass per lane per round, lanes alternating, minimum kept — never one lane to completion,
    which would time it with the caches still warm from its own build."""
    best: dict[str, float] = {}
    for _, fn in lanes:
        for probe in probes:
            fn(probe)
    for _ in range(ROUNDS):
        for name, fn in lanes:
            start = time.perf_counter_ns()
            for probe in probes:
                fn(probe)
            ns = (time.perf_counter_ns() - start) / len(probes)
            best[name] = min(best.get(name, float("inf")), ns)
    return best


def _one(path: Path, corpus: str, size: int, repeats: int) -> list[dict]:
    keys = path.read_text(encoding="utf-8").splitlines()
    raw = sum(len(k.encode()) for k in keys) / len(keys)
    print(f"\n{corpus} {len(keys):,} keys, raw {raw:.2f} B/key  ({path.name})")
    probes, a_miss = _probes.probe_set(keys, PROBES)
    cells, lanes, alive = [], [], []
    for name, build, keeps in _structures():
        times = []
        for _ in range(repeats):
            start = time.perf_counter()
            obj = build(keys)
            times.append((time.perf_counter() - start) * 1e3)
        bytes_per_key = _size(obj) / len(keys)
        alive.append(obj)
        fn = _lookup(obj, a_miss, keys[0], exact=keeps)
        if fn is not None:
            lanes.append((name, fn))
        cells.append(
            {
                "corpus": corpus,
                "size": size,
                "keys": len(keys),
                "raw_bytes_per_key": raw,
                "structure": name,
                "keeps_keys": keeps,
                "bytes_per_key": bytes_per_key,
                "build_ms": min(times),
            }
        )
    latency = _latency(lanes, probes)
    for cell in cells:
        cell["lookup_ns"] = latency.get(cell["structure"])
        shown = "—" if cell["lookup_ns"] is None else f"{cell['lookup_ns']:5.0f}"
        print(
            f"  {cell['structure']:<24}{cell['bytes_per_key']:8.3f} B/key"
            f"{cell['build_ms']:9.0f} ms   {shown} ns"
        )
    alive.clear()
    return cells


def _table(cells: list[dict]) -> None:
    names = list(dict.fromkeys(c["structure"] for c in cells))
    groups = list(dict.fromkeys((c["corpus"], c["keys"]) for c in cells))
    by = {(c["corpus"], c["keys"], c["structure"]): c for c in cells}
    print("\n### bytes per key\n")
    print("| corpus | keys | raw | " + " | ".join(names) + " |")
    print("|---|---:|---:|" + "---:|" * len(names))
    for corpus, keys in groups:
        raw = by[(corpus, keys, names[0])]["raw_bytes_per_key"]
        row = []
        for name in names:
            cell = by.get((corpus, keys, name))
            row.append("—" if cell is None else f"{cell['bytes_per_key']:.2f}")
        print(f"| `{corpus}` | {keys:,} | {raw:.1f} | " + " | ".join(row) + " |")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--sizes", type=int, nargs="*", default=[100_000, 1_000_000])
    parser.add_argument("--repeats", type=int, default=2, help="builds per cell; 1 for 10^7")
    parser.add_argument("--corpora", nargs="*", default=[])
    parser.add_argument("--out", default="sweep")
    args = parser.parse_args()

    manifest = json.loads(corpora.MANIFEST.read_text(encoding="utf-8"))
    wanted = args.corpora or list(manifest["corpora"])
    cells: list[dict] = []
    for name in wanted:
        entry = manifest["corpora"].get(name)
        if entry is None:
            sys.exit(f"{name}: not in {corpora.MANIFEST}")
        for one in entry["files"]:
            if one["keys"] not in args.sizes:
                continue
            path = corpora.ROOT / one["file"]
            if not path.exists():
                print(f"{name}: {one['file']} not built, skipped")
                continue
            cells.extend(_one(path, name, one["keys"], args.repeats))
    if not cells:
        sys.exit("nothing measured: build the corpora first (`python bench/corpora.py build`)")
    _table(cells)
    path = _results.write(
        args.out,
        cells,
        corpora=str(corpora.ROOT),
        build_repeats=args.repeats,
        probes=PROBES,
        rounds=ROUNDS,
        competitors=_results.versions("marisa-trie"),
    )
    print(f"\nresults → {path}")
    median = statistics.median(c["build_ms"] for c in cells)
    print(f"median build over {len(cells)} cells: {median:.0f} ms")
    return 0


if __name__ == "__main__":
    sys.exit(main())
