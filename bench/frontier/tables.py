"""Parse a frontier campaign log from bench/frontier/run.sh into its JSON artifact and markdown.

    uv run --no-sync python bench/frontier/tables.py bench/results/frontier-1m-<...>.log
    uv run --no-sync python bench/frontier/tables.py --json bench/results/frontier-1m-<...>.log

The log is the evidence: every process's own output, under a header that names the machine, the
toolchain and every competitor's commit. `--json` writes the same name with a `.json` suffix, one
cell a structure and corpus with the header as its environment. The markdown printed is what the
docs quote: an overview, then a table a corpus.

Sizes are each structure's own account of itself: lexindex's serialised blob, `space_cost()` for the
C² benchmark's structures and `memory_in_bytes` for XCDAT. ART and C-ART count their nodes and not
the keys they point into, so they are printed for reference and kept out of every comparison.
"""

from __future__ import annotations

import argparse
import json
import re
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
    "string": "lexindex StringIndex",
}
REFERENCE = {"ART", "C-ART"}

HEADER = re.compile(r"^(?P<key>[\w/ -]+): (?P<value>.+)$")
CORPUS = re.compile(r"^##### (?P<name>\S+) \((?P<keys>\d+) lines, (?P<bytes>\d+) bytes\)")
PROCESS = re.compile(r"^--- (?P<label>.+?)   load ")
# What C²'s protocol has no column for: the three warm passes frontier_lex times after the cold one.
LEX_WARM = re.compile(r"^lexindex .+, then mean (?P<mean>[\d.]+) / min (?P<min>[\d.]+) ns")
CSV = re.compile(r"^(?P<build>[\d.]+),(?P<mib>[\d.]+),(?P<ns>[\d.]+)$")
TIME = re.compile(r"^\[time (?P<seconds>[\d.]+) s, maxrss (?P<kb>\d+) KB\]$")
EXIT = re.compile(r"^\[exit (?P<code>\d+)\]$")
FAILURE = re.compile(r"Command (?:terminated by signal|exited with non-zero status) \d+")


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
    environment: dict[str, str] = {}
    corpora: list[dict] = []
    cell: dict | None = None
    for line in text.splitlines():
        if m := CORPUS.match(line):
            corpora.append(
                {"corpus": m["name"], "keys": int(m["keys"]), "bytes": int(m["bytes"]), "cells": []}
            )
            cell = None
        elif not corpora:
            if m := HEADER.match(line):
                environment[m["key"]] = m["value"]
        elif m := PROCESS.match(line):
            cell = {"structure": structure(m["label"])}
            corpora[-1]["cells"].append(cell)
        elif cell is None:
            continue
        elif m := LEX_WARM.match(line):
            cell.update(id_ns_warm_mean=float(m["mean"]), id_ns_warm_min=float(m["min"]))
        elif m := CSV.match(line):
            cell.update(
                build_ms=float(m["build"]),
                bytes_per_key=float(m["mib"]) * 2**20 / corpora[-1]["keys"],
                id_ns=float(m["ns"]),
            )
        elif m := TIME.match(line):
            cell.update(seconds=float(m["seconds"]), maxrss_mib=int(m["kb"]) / 1024)
        elif m := EXIT.match(line):
            cell["exit"] = int(m["code"])
        elif m := FAILURE.search(line):
            cell["failure"] = m[0]
    return environment, corpora


def measured(cell: dict) -> bool:
    return cell.get("exit") == 0 and "bytes_per_key" in cell and "id_ns" in cell


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


def point(cell: dict) -> str:
    return f"{cell['bytes_per_key']:.2f} @ {cell['id_ns']:.0f}"


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


def corpus_table(corpus: dict) -> str:
    raw = raw_bytes_per_key(corpus)
    on_front = front(corpus["cells"])
    lines = [
        f"**`{corpus['corpus']}`** — {corpus['keys']:,} keys, {raw:.2f} bytes a key raw",
        "",
        "| structure | bytes/key | % of raw | build ms | `id` ns | peak MiB | front |",
        "|---|---:|---:|---:|---:|---:|:---:|",
    ]
    for cell in corpus["cells"]:
        name = cell["structure"]
        if not measured(cell):
            why = cell.get("failure") or f"exit {cell.get('exit')}"
            lines.append(f"| {name} | — | — | — | — | — | {why} |")
            continue
        mark = "ref" if name in REFERENCE else "●" if name in on_front else ""
        rss = f"{cell['maxrss_mib']:.0f}" if "maxrss_mib" in cell else "—"
        lines.append(
            f"| {name} | {cell['bytes_per_key']:.2f} | {100 * cell['bytes_per_key'] / raw:.1f} "
            f"| {cell['build_ms']:.0f} | {cell['id_ns']:.0f} | {rss} | {mark} |"
        )
    return "\n".join(lines)


def artifact(environment: dict[str, str], corpora: list[dict]) -> dict:
    cells = [
        {
            "corpus": corpus["corpus"],
            "keys": corpus["keys"],
            "raw_bytes_per_key": round(raw_bytes_per_key(corpus), 3),
            **cell,
        }
        for corpus in corpora
        for cell in corpus["cells"]
    ]
    return {"table": environment.get("table"), "environment": environment, "cells": cells}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("log", type=Path, help="a bench/results/frontier-*.log")
    parser.add_argument("--json", action="store_true", help="write the JSON artifact beside it")
    args = parser.parse_args()
    environment, corpora = parse(args.log.read_text(encoding="utf-8"))
    if args.json:
        out = args.log.with_suffix(".json")
        payload = json.dumps(artifact(environment, corpora), indent=2, ensure_ascii=False)
        out.write_text(payload + "\n", encoding="utf-8")
        print(f"wrote {out}\n")
    print(overview(corpora))
    for corpus in corpora:
        print()
        print(corpus_table(corpus))


if __name__ == "__main__":
    main()
