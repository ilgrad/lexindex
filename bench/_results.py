"""Every timed table in the README is a claim about one machine on one day.

This module is what lets the claim be checked: `compare.py` and `scale.py` write their numbers to
`bench/results/<table>-<date>-<host>-<commit>.json` alongside the environment that produced them,
and the README cites the file under the table it filled. A number whose machine, kernel, toolchain
and load average are recorded can be re-measured and refuted; one printed to a terminal and pasted
into a table cannot.

The estimator per cell is the **minimum** of the repeats: the fastest run is the one least
contaminated by everything else on the machine. The median and the raw samples are recorded next to
it, so a run taken on a busy machine is visible in the file rather than averaged into it -- which is
the failure this module exists to make impossible to hide.
"""

from __future__ import annotations

import json
import os
import platform
import shutil
import socket
import statistics
import subprocess
import sys
from collections.abc import Sequence
from datetime import date
from pathlib import Path
from typing import Any

RESULTS = Path(__file__).parent / "results"

# Sampled at import, which is the start of the run: a file that records the load only at the end
# cannot show a benchmark that began on a busy machine and finished on a quiet one.
_LOADAVG_AT_IMPORT = os.getloadavg()


def _cpu_model() -> str | None:
    try:
        for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        pass
    return platform.processor() or None


def _run(*argv: str) -> str | None:
    """First line of `argv`'s output, or None if the tool is absent or fails. A missing rustc is
    recorded as null rather than crashing a Python benchmark that does not need one."""
    exe = shutil.which(argv[0])
    if exe is None:
        return None
    try:
        done = subprocess.run(  # argv list, never a shell string
            [exe, *argv[1:]], capture_output=True, text=True, timeout=30, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    return done.stdout.strip().splitlines()[0] if done.stdout.strip() else None


def _commit() -> str:
    """`<short sha>` for a clean tree, `<short sha>-dirty` otherwise. The suffix is the point: a
    result measured on uncommitted code is not attributable to the commit it names.

    Sampled once at import, like the load average, and for the same reason: the state that matters
    is the code the run started on. Reading it again at write time would tag a clean measurement
    `-dirty` for an edit made to an unrelated file while the benchmark was running."""
    sha = _run("git", "-C", str(Path(__file__).parent.parent), "rev-parse", "--short", "HEAD")
    if sha is None:
        return "nogit"
    # `--untracked-files=no`: the run's own results file is untracked, and a plain `--porcelain`
    # therefore reported the *next* run dirty for the output of the previous one.
    root = str(Path(__file__).parent.parent)
    dirty = _run("git", "-C", root, "status", "--porcelain", "--untracked-files=no")
    return f"{sha}-dirty" if dirty else sha


_COMMIT_AT_IMPORT = _commit()


def environment() -> dict[str, Any]:
    import lexindex

    return {
        "date": date.today().isoformat(),
        "host": socket.gethostname(),
        "commit": _COMMIT_AT_IMPORT,
        "cpu": _cpu_model(),
        "cpus": os.cpu_count(),
        "kernel": f"{platform.system()} {platform.release()}",
        "python": sys.version.split()[0],
        "rustc": _run("rustc", "--version"),
        "lexindex": getattr(lexindex, "__version__", None),
        "loadavg_start": [round(x, 2) for x in _LOADAVG_AT_IMPORT],
        "loadavg_end": [round(x, 2) for x in os.getloadavg()],
    }


def versions(*distributions: str) -> dict[str, str | None]:
    """The installed version of each competitor, ``None`` where it is not installed."""
    from importlib.metadata import PackageNotFoundError, version

    out: dict[str, str | None] = {}
    for name in distributions:
        try:
            out[name] = version(name)
        except PackageNotFoundError:
            out[name] = None
    return out


def summary(samples: Sequence[float]) -> dict[str, Any]:
    """One cell: the minimum, with the spread it came from kept next to it."""
    return {
        "min": min(samples),
        "median": statistics.median(samples),
        "reps": len(samples),
        "samples": list(samples),
    }


def write(table: str, cells: list[dict[str, Any]], **extra: Any) -> Path:
    """Write one table's cells, tagged with the machine that produced them."""
    env = environment()
    RESULTS.mkdir(exist_ok=True)
    path = RESULTS / f"{table}-{env['date']}-{env['host']}-{env['commit']}.json"
    payload = {"table": table, "environment": env, **extra, "cells": cells}
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return path
