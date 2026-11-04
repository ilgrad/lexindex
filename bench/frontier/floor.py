"""MARISA at its smallest against lexindex's, over this repository's corpus files, at one commit.

    cargo build --release --locked --manifest-path bench/frontier/frontier_lex/Cargo.toml
    uv run --no-project python bench/frontier/floor.py --suite ../string-index-benchmarks

The campaign builds MARISA at one to three tries under the default cache. Its smallest trie takes
more tries and the tiny cache, and github.com/ilgrad/string-index-benchmarks builds that grid, over
its own corpus files. Three of this repository's files are not the suite's, so this script runs the
suite's `marisa_floor` and `marisa_resident` over these files, and this repository's `resident` for
lexindex: its committed crate at this commit, since the suite depends on the published one and a
release candidate has none. It writes `bench/results/marisa-floor-1m-<date>-<host>-<commit>.json`,
which `bench/tables.py` renders and `bench/frontier_chart.py` reads.

Sizes only, so it runs on a busy machine: nothing is timed. A failure writes no artifact.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path

from tables import LEXINDEX

ROOT = Path(__file__).resolve().parent.parent.parent
CORPORA = Path(os.environ.get("LEXINDEX_CORPORA", ROOT / "local" / "corpora"))
RESIDENT = ROOT / "bench/frontier/frontier_lex/target/release/resident"
# The same thirteen as `bench/frontier/run.sh`, which holds them in the campaign's order.
STEMS = [
    "words-full",
    "dna-1000000",
    "domains-1000000",
    "idents-1000000",
    "numeric-1000000",
    "opaque-1000000",
    "paths-1000000",
    "pypi-full",
    "titles-en-1000000",
    "titles-ru-1000000",
    "titles-zh-1000000",
    "urls-1000000",
    "uuid-1000000",
]
KINDS = ("dict32", "dict256", "dict1024", "labels256", "labels1024", "string")
MARISA = "c2/baseline_marisa/marisa"
COUNTS = re.compile(r"^(?P<keys>\d+) keys, (?P<raw>\d+) raw bytes$")
CONTROL = ("default", 3)


def run(argv: list[str]) -> str:
    done = subprocess.run(argv, capture_output=True, text=True, check=False)
    if done.returncode != 0:
        raise SystemExit(
            f"refusing to write an artifact: {argv[0]} {argv[1]}: {done.stderr.strip()}"
        )
    return done.stdout


def git(*argv: str, at: Path) -> str:
    return run(["git", "-C", str(at), *argv]).strip()


def measure(stem: str, build: Path) -> dict:
    path = CORPORA / f"{stem}.txt"
    first, *table = run([str(build / "marisa_floor"), str(path)]).splitlines()
    counts = COUNTS.match(first)
    if counts is None:
        raise SystemExit(f"refusing to write an artifact: {stem}: no key count in {first!r}")
    keys = int(counts["keys"])
    configurations = [
        {
            "cache": row["cache"],
            "tries": int(row["tries"]),
            "tries_built": int(row["tries_built"]),
            "bytes_io": int(row["bytes_io"]),
            "bytes_total": int(row["bytes_total"]),
        }
        for row in csv.DictReader(table)
    ]
    # `min` keeps the first of equal sizes: past the depth where a corpus's tails run out, marisa
    # builds the same trie however many tries it is asked for.
    smallest_io = min(configurations, key=lambda c: c["bytes_io"])
    smallest_total = min(configurations, key=lambda c: c["bytes_total"])

    roles: dict[tuple[str, int], list[str]] = {}
    for role, at in (("smallest_io", smallest_io), ("smallest_total", smallest_total)):
        roles.setdefault((at["cache"], at["tries"]), []).append(role)
    roles.setdefault(CONTROL, []).append("default")
    argv = [str(build / "marisa_resident"), str(path), *(f"{c}:{t}" for c, t in roles)]
    _, *table = run(argv).splitlines()
    marisa = []
    for row in csv.DictReader(table):
        at = (row["cache"], int(row["tries"]))
        marisa.append(
            {
                "roles": roles[at],
                "cache": at[0],
                "tries": at[1],
                "tries_built": int(row["tries_built"]),
                "bytes_io": int(row["bytes_io"]),
                "bytes_total": int(row["bytes_total"]),
                "mmap_heap": int(row["mmap_heap"]),
                "load_heap": int(row["load_heap"]),
            }
        )
    if (
        next(m for m in marisa if "smallest_io" in m["roles"])["bytes_io"]
        != smallest_io["bytes_io"]
    ):
        raise SystemExit(f"refusing to write an artifact: {stem}: marisa's two binaries disagree")

    lexindex = [
        {
            "structure": LEXINDEX[row["kind"]].removeprefix("lexindex "),
            "blob": int(row["blob"]),
            "from_bytes_heap": int(row["from_bytes_heap"]),
            "mmap_heap": int(row["mmap_heap"]),
        }
        for row in csv.DictReader(run([str(RESIDENT), str(path), *KINDS]).splitlines())
    ]
    return {
        "corpus": stem,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "keys": keys,
        "raw_bytes": int(counts["raw"]),
        "floor": {
            "configurations": configurations,
            "smallest_io": smallest_io,
            "smallest_total": smallest_total,
        },
        "resident": {"lexindex": lexindex, "marisa": marisa},
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--suite", type=Path, required=True, help="a built string-index-benchmarks")
    parser.add_argument("--jobs", type=int, default=4, help="corpora built at once")
    parser.add_argument("--out", type=Path, default=ROOT / "bench/results")
    parser.add_argument("--allow-dirty", action="store_true", help="tagged <sha>-dirty")
    args = parser.parse_args()

    commit = git("rev-parse", "--short=7", "HEAD", at=ROOT)
    if git("status", "--porcelain", "--untracked-files=no", at=ROOT):
        if not args.allow_dirty:
            raise SystemExit("refusing: the tree is dirty, and an artifact names its commit")
        commit += "-dirty"
    build = args.suite / "build"
    pinned = re.search(
        rf'"{MARISA} \S+ (?P<commit>[0-9a-f]{{40}})"',
        (args.suite / "harness/pins.sh").read_text(encoding="utf-8"),
    )
    if pinned is None or git("rev-parse", "HEAD", at=build / MARISA) != pinned["commit"]:
        raise SystemExit("refusing: the suite's marisa is not at its pin; run its harness/build.sh")
    if not os.access(RESIDENT, os.X_OK):
        raise SystemExit(f"refusing: {RESIDENT} is missing; build frontier_lex first")

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        corpora = list(pool.map(lambda stem: measure(stem, build), STEMS))

    now = datetime.now().astimezone()
    uname = os.uname()
    table = "marisa-floor-1m"
    suite = git("rev-parse", "--short=7", "HEAD", at=args.suite)
    environment = {
        "date": now.isoformat(timespec="seconds"),
        "host": uname.nodename,
        "commit": commit,
        "table": table,
        "kernel": f"{uname.sysname} {uname.release} {uname.machine}",
        "binaries": "build/marisa_floor and build/marisa_resident of "
        f"github.com/ilgrad/string-index-benchmarks at {suite}, "
        "and bench/frontier/frontier_lex's resident at this commit",
        "marisa": f"marisa-trie at {pinned['commit'][:12]}, the suite's pin; configurations 1-32 "
        "tries under the tiny and the default cache",
        "lexindex": f"this repository at {commit}",
        "corpora": "bench/corpora.py's files at 1 M, sha256 per corpus. Ten are byte-identical to "
        "string-index-benchmarks' files; paths, idents and pypi are not.",
        "sizes": "bytes_io is io_size(), what save() writes; bytes_total is total_size(); "
        "from_bytes_heap, mmap_heap and load_heap are the bytes asked for by every allocation a "
        "load keeps, after a pass of lookups",
        "quiet-machine gates": "none: sizes are deterministic",
    }
    args.out.mkdir(parents=True, exist_ok=True)
    out = args.out / f"{table}-{now:%F}-{uname.nodename}-{commit}.json"
    artifact = {"table": table, "environment": environment, "corpora": corpora}
    out.write_text(json.dumps(artifact, indent=1) + "\n", encoding="utf-8")
    print(out)


if __name__ == "__main__":
    main()
