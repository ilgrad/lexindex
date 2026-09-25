"""Parse frontier campaign logs from bench/frontier/run.sh into their JSON artifact and markdown.

    uv run --no-sync python bench/frontier/tables.py bench/results/frontier-1m-<...>.log
    uv run --no-sync python bench/frontier/tables.py --json bench/results/frontier-1m-<...>.log
    uv run --no-sync python bench/frontier/tables.py --json <campaign>.log <re-measurement>.log

The log is the evidence: every process's own output, under a header that names the machine, the
toolchain and every competitor's commit. `--json` writes the first log's name with a `.json`
suffix, one cell a structure and corpus holding each round's run, with the header as its
environment. A later log is a re-measurement of some of the campaign's corpora -- run.sh given
their names, after a round of theirs ran beside other work -- and its cells replace the campaign's
for those corpora. It has to come from the same commit, machine, toolchain and pins, and the
artifact names it under `replaced`. The markdown printed is what the docs quote, and what
bench/tables.py renders from the artifact: an overview, `HashedDictIndex` against XCDAT 15 where
the campaign ran it, the exact searches against XCDAT 15 where it ran a routed `DictIndex`, then a
table a corpus.

A cell's `id` and build times are the medians of its rounds, and its spread is the range of the `id`
times over that median; a structure that fails in any round has no numbers, only the reason. Sizes
are each structure's own account of itself: lexindex's serialised blob, `space_cost()` for the C²
benchmark's structures and `memory_in_bytes` for XCDAT. ART and C-ART count their nodes and not the
keys they point into, so they are printed for reference and kept out of every comparison. Each
run's peak resident memory is kept in the artifact and not tabled: it counts the driver's own copies
of the keys and the queries, which the C++ drivers and frontier_lex hold differently.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
from pathlib import Path

C2_CASES = {
    0: "C²-FST",
    1: "C²-CoCo",
    2: "C²-MARISA",
    3: "FST",
    4: "CoCo",
    5: "MARISA",
    6: "PDT",
    7: "ART",
    8: "C-ART",
}
LEXINDEX = {
    "dict32": "lexindex Dict 32",
    "dict256": "lexindex Dict 256",
    "dict1024": "lexindex Dict 1024",
    "routed32": "lexindex Dict 32 routed",
    "routed256": "lexindex Dict 256 routed",
    "routed1024": "lexindex Dict 1024 routed",
    "string": "lexindex StringIndex",
    "hashed0": "lexindex HashedDict closed",
    "hashed8": "lexindex HashedDict fp=8",
    "hashed16": "lexindex HashedDict fp=16",
    "da": "lexindex DoubleArray",
}
# `HashedDictIndex` against the structure that was the fastest on every corpus before it, and beside
# the dictionary it is built over.
HASHED = {
    "XCDAT 15": "XCDAT 15",
    "lexindex Dict 256": "`Dict` 256",
    "lexindex HashedDict closed": "`HashedDict` closed",
    "lexindex HashedDict fp=8": "fp=8",
    "lexindex HashedDict fp=16": "fp=16",
}
# lexindex's exact searches -- the ones that answer a stranger exactly, which the sidecar does
# not -- against the fastest structure from elsewhere.
SEARCH = {
    "XCDAT 15": "XCDAT 15",
    "lexindex Dict 256": "`Dict` 256",
    "lexindex Dict 32 routed": "32 routed",
    "lexindex Dict 256 routed": "256 routed",
    "lexindex Dict 1024 routed": "1024 routed",
    "lexindex StringIndex": "`StringIndex`",
    "lexindex DoubleArray": "`DoubleArray`",
}
REFERENCE = {"ART", "C-ART"}
# What GNU timeout exits with when it had to stop the process.
TIMED_OUT = 124
# The header lines a re-measurement may differ in. Any other line that differs is another commit,
# machine, toolchain or pin, and cells measured under it cannot stand in for the campaign's.
PER_RUN = {"date", "table", "load at start"}

HEADER = re.compile(r"^(?P<key>[\w/ -]+): (?P<value>.+)$")
CORPUS = re.compile(
    r"^##### (?P<name>\S+) \((?P<keys>\d+) lines, (?P<bytes>\d+) bytes\)   round (?P<round>\d+)/"
)
PROCESS = re.compile(r"^--- (?P<label>.+?)   load ")
SKIPPED = re.compile(r"^--- .+?   skipped: ")
# What C²'s protocol has no column for: the three warm passes frontier_lex times after the cold one.
LEX_WARM = re.compile(r"^lexindex .+, then mean (?P<mean>[\d.]+) / min (?P<min>[\d.]+) ns")
CSV = re.compile(r"^(?P<build>[\d.]+),(?P<mib>[\d.]+),(?P<ns>[\d.]+)$")
TIME = re.compile(
    r"^\[time (?P<seconds>[\d.]+) s, user (?P<user>[\d.]+) s, sys (?P<sys>[\d.]+) s, "
    r"maxrss (?P<kb>\d+) KB\]$"
)
BUSY = re.compile(r"^\[busy (?P<ticks>\d+) jiffies\]$")
EXIT = re.compile(r"^\[exit (?P<code>\d+)\]$")
SIGNAL = re.compile(r"Command terminated by signal (?P<signal>\d+)")
WHAT = re.compile(r"^\s*what\(\):\s+(?P<what>.+)$")
# frontier_lex's line for a corpus a structure cannot hold, printed before it exits.
REFUSED = re.compile(r"^lexindex .+ refused: (?P<why>.+)$")


def structure(label: str) -> str:
    """The row name for a process label that run.sh wrote."""
    kind, _, rest = label.partition(" ")
    if kind == "lexindex" and rest in LEXINDEX:
        return LEXINDEX[rest]
    if kind == "xcdat":
        return f"XCDAT {rest}"
    m = re.fullmatch(r"case (\d+) rec (\d+)", rest)
    if kind == "c2" and m:
        name = C2_CASES[int(m[1])]
        return name if m[2] == "0" else f"{name} ρ={m[2]}"  # noqa: RUF001 -- the paper's rho
    raise ValueError(f"unknown process label {label!r}")


def parse(text: str) -> tuple[dict[str, str], list[dict]]:
    """The header, and every corpus with each structure's runs in the order the rounds ran them."""
    environment: dict[str, str] = {}
    corpora: dict[str, dict] = {}
    corpus: dict | None = None
    run: dict | None = None
    round_no = 0
    for line in text.splitlines():
        if m := CORPUS.match(line):
            corpus = corpora.setdefault(
                m["name"],
                {"corpus": m["name"], "keys": int(m["keys"]), "bytes": int(m["bytes"]), "runs": {}},
            )
            round_no = int(m["round"])
            run = None
        elif corpus is None:
            if m := HEADER.match(line):
                environment[m["key"]] = m["value"]
        elif m := PROCESS.match(line):
            run = {"round": round_no}
            corpus["runs"].setdefault(structure(m["label"]), []).append(run)
        elif SKIPPED.match(line):
            run = None
        elif run is None:
            continue
        elif m := LEX_WARM.match(line):
            run.update(id_ns_warm_mean=float(m["mean"]), id_ns_warm_min=float(m["min"]))
        elif m := CSV.match(line):
            run.update(
                build_ms=float(m["build"]),
                bytes_per_key=float(m["mib"]) * 2**20 / corpus["keys"],
                id_ns=float(m["ns"]),
            )
        elif m := TIME.match(line):
            run.update(
                seconds=float(m["seconds"]),
                cpu_seconds=round(float(m["user"]) + float(m["sys"]), 2),
                maxrss_mib=int(m["kb"]) / 1024,
            )
        elif m := BUSY.match(line):
            run["busy_ticks"] = int(m["ticks"])
        elif m := EXIT.match(line):
            run["exit"] = int(m["code"])
        elif m := SIGNAL.search(line):
            run["signal"] = int(m["signal"])
        elif m := WHAT.match(line):
            run["what"] = m["what"]
        elif m := REFUSED.match(line):
            run["refused"] = m["why"]
    return environment, list(corpora.values())


def failure(run: dict) -> str | None:
    """Why a run has no numbers, or None where it has them."""
    if "what" in run:
        return f"aborted: {run['what']}"
    if "refused" in run:
        return f"refused: {run['refused']}"
    if "signal" in run:
        return f"signal {run['signal']}"
    if run.get("exit") == TIMED_OUT:
        return "timed out"
    if run.get("exit") != 0:
        return f"exit {run.get('exit')}"
    if "bytes_per_key" not in run or "id_ns" not in run:
        return "no result line"
    return None


def summarise(name: str, runs: list[dict], ticks: int) -> dict:
    """One structure on one corpus: the medians of its rounds, or why it has none."""
    for run in runs:
        if run.get("seconds") and "busy_ticks" in run:
            others = run["busy_ticks"] / ticks - run["cpu_seconds"]
            run["others_busy_cpus"] = round(others / run["seconds"], 2)
    cell: dict = {"structure": name}
    failures = [(run["round"], why) for run in runs if (why := failure(run))]
    if failures:
        round_no, why = failures[0]
        cell["failure"] = why if round_no == 1 else f"{why}, round {round_no}"
    else:
        sizes = {run["bytes_per_key"] for run in runs}
        if len(sizes) != 1:
            raise ValueError(f"{name}: the size differs between rounds, {sorted(sizes)}")
        times = [run["id_ns"] for run in runs]
        median = statistics.median(times)
        cell.update(
            bytes_per_key=sizes.pop(),
            id_ns=median,
            id_ns_spread=(max(times) - min(times)) / median,
            build_ms=statistics.median(run["build_ms"] for run in runs),
            maxrss_mib=max(run["maxrss_mib"] for run in runs),
        )
    cell["runs"] = runs
    return cell


def summarised(log: Path) -> tuple[dict[str, str], list[dict]]:
    """A log's header, and its corpora with one summarised cell a structure."""
    environment, corpora = parse(log.read_text(encoding="utf-8"))
    ticks = int(environment["clock ticks"])
    for corpus in corpora:
        corpus["cells"] = [
            summarise(name, runs, ticks) for name, runs in corpus.pop("runs").items()
        ]
    return environment, corpora


def measured(cell: dict) -> bool:
    return "id_ns" in cell


def front(cells: list[dict]) -> set[str]:
    """The structures no other one beats on both bytes and `id` time."""
    points = {
        c["structure"]: (c["bytes_per_key"], c["id_ns"])
        for c in cells
        if measured(c) and c["structure"] not in REFERENCE
    }
    return {
        name
        for name, (size, ns) in points.items()
        if not any(s <= size and t <= ns and (s, t) != (size, ns) for s, t in points.values())
    }


def raw_bytes_per_key(corpus: dict) -> float:
    return (corpus["bytes"] - corpus["keys"]) / corpus["keys"]


def size(bytes_per_key: float) -> str:
    """Two decimals, or two significant digits below a tenth of a byte."""
    return f"{bytes_per_key:.2f}" if bytes_per_key >= 0.1 else f"{bytes_per_key:.2g}"


def point(cell: dict) -> str:
    return f"{size(cell['bytes_per_key'])} @ {cell['id_ns']:.0f}"


def quietness(corpora: list[dict]) -> str:
    """How much else the machine did while the campaign's processes ran."""
    others = sorted(
        run["others_busy_cpus"]
        for corpus in corpora
        for cell in corpus["cells"]
        for run in cell["runs"]
        if "others_busy_cpus" in run
    )
    if not others:
        return "No process ran."
    return (
        f"Other work while the {len(others)} processes ran: median "
        f"{statistics.median(others):.2f} busy CPUs, most {others[-1]:.2f}."
    )


def overview(corpora: list[dict]) -> str:
    lines = [
        "| corpus | `Dict` 256 | smallest | fastest | lexindex on the front |",
        "|---|---:|---|---|---|",
    ]
    for corpus in corpora:
        cells = {
            c["structure"]: c
            for c in corpus["cells"]
            if measured(c) and c["structure"] not in REFERENCE
        }
        if not cells:
            continue
        smallest = min(cells.values(), key=lambda c: c["bytes_per_key"])
        fastest = min(cells.values(), key=lambda c: c["id_ns"])
        ours = sorted(
            name.removeprefix("lexindex ")
            for name in front(corpus["cells"])
            if name.startswith("lexindex")
        )
        dict256 = cells.get("lexindex Dict 256")
        lines.append(
            f"| `{corpus['corpus']}` | {point(dict256) if dict256 else '—'} "
            f"| {smallest['structure']} {point(smallest)} "
            f"| {fastest['structure']} {point(fastest)} "
            f"| {', '.join(ours) or 'none'} |"
        )
    return "\n".join(lines)


def columns(corpora: list[dict], names: dict[str, str]) -> str:
    """The structures `names` holds that the campaign ran, a column each, bytes a key @
    nanoseconds a lookup."""
    ran = {cell["structure"] for corpus in corpora for cell in corpus["cells"]}
    names = {name: title for name, title in names.items() if name in ran}
    lines = [
        "| corpus | " + " | ".join(names.values()) + " |",
        "|---|" + "---:|" * len(names),
    ]
    for corpus in corpora:
        cells = {c["structure"]: c for c in corpus["cells"] if measured(c)}
        row = " | ".join(point(cells[name]) if name in cells else "—" for name in names)
        lines.append(f"| `{corpus['corpus']}` | {row} |")
    return "\n".join(lines)


def hashed(corpora: list[dict]) -> str:
    """`HashedDictIndex` beside its dictionary and XCDAT 15."""
    return columns(corpora, HASHED)


def search(corpora: list[dict]) -> str:
    """lexindex's exact searches, the routed dictionaries among them, beside XCDAT 15."""
    return columns(corpora, SEARCH)


def corpus_table(corpus: dict) -> str:
    raw = corpus["raw_bytes_per_key"]
    on_front = front(corpus["cells"])
    lines = [
        f"**`{corpus['corpus']}`** — {corpus['keys']:,} keys, {raw:.2f} bytes a key raw",
        "",
        "| structure | bytes/key | % of raw | build ms | `id` ns | spread | front |",
        "|---|---:|---:|---:|---:|---:|:---:|",
    ]
    for cell in corpus["cells"]:
        name = cell["structure"]
        if not measured(cell):
            lines.append(f"| {name} | — | — | — | — | — | {cell['failure']} |")
            continue
        mark = "ref" if name in REFERENCE else "●" if name in on_front else ""
        bpk = cell["bytes_per_key"]
        lines.append(
            f"| {name} | {size(bpk)} | {100 * bpk / raw:.1f} | {cell['build_ms']:.0f} "
            f"| {cell['id_ns']:.0f} | {100 * cell['id_ns_spread']:.0f} % | {mark} |"
        )
    return "\n".join(lines)


def artifact(logs: list[Path]) -> dict:
    """The first log's campaign, with each later log's corpora in place of the campaign's own."""
    environment, corpora = summarised(logs[0])
    by_name = {corpus["corpus"]: corpus for corpus in corpora}
    replaced: dict[str, str] = {}
    for log in logs[1:]:
        other, again = summarised(log)
        differ = sorted(
            key
            for key in environment.keys() | other.keys()
            if key not in PER_RUN and environment.get(key) != other.get(key)
        )
        if differ:
            raise SystemExit(f"{log.name} differs from {logs[0].name} in: {', '.join(differ)}")
        for corpus in again:
            mine = by_name.get(corpus["corpus"])
            if mine is None:
                raise SystemExit(f"{log.name} measured {corpus['corpus']}; the campaign did not")
            if (mine["keys"], mine["bytes"]) != (corpus["keys"], corpus["bytes"]):
                raise SystemExit(f"{log.name} measured another {corpus['corpus']} file")
            by_name[corpus["corpus"]] = corpus
            replaced[corpus["corpus"]] = log.name
    cells = [
        {
            "corpus": corpus["corpus"],
            "keys": corpus["keys"],
            "raw_bytes_per_key": round(raw_bytes_per_key(corpus), 3),
            **cell,
        }
        for corpus in by_name.values()
        for cell in corpus["cells"]
    ]
    return {
        "table": environment.get("table"),
        "environment": environment,
        "replaced": replaced,
        "cells": cells,
    }


def corpora_of(artifact: dict) -> list[dict]:
    """The artifact's cells grouped back into corpora, in the order the campaign ran them."""
    corpora: dict[str, dict] = {}
    for cell in artifact["cells"]:
        corpus = corpora.setdefault(
            cell["corpus"],
            {
                "corpus": cell["corpus"],
                "keys": cell["keys"],
                "raw_bytes_per_key": cell["raw_bytes_per_key"],
                "cells": [],
            },
        )
        corpus["cells"].append(cell)
    return list(corpora.values())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "logs",
        type=Path,
        nargs="+",
        metavar="log",
        help="a bench/results/frontier-*.log, then any re-measurement of its corpora",
    )
    parser.add_argument(
        "--json", action="store_true", help="write the JSON artifact beside the first log"
    )
    args = parser.parse_args()
    data = artifact(args.logs)
    if args.json:
        out = args.logs[0].with_suffix(".json")
        out.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"wrote {out}\n")
    corpora = corpora_of(data)
    print(quietness(corpora))
    print()
    print(overview(corpora))
    if any(cell["structure"].startswith("lexindex HashedDict") for cell in data["cells"]):
        print()
        print(hashed(corpora))
    if any(cell["structure"].endswith(" routed") for cell in data["cells"]):
        print()
        print(search(corpora))
    for corpus in corpora:
        print()
        print(corpus_table(corpus))


if __name__ == "__main__":
    main()
