"""A string column to dense ids inside Polars: the lexindex plugin against what Polars already has.

Each method turns the same column of strings into the same kind of answer, an integer for every
row, over one vocabulary every row is in:

* ``lexindex-dict`` -- ``.lexindex.id`` over a ``DictIndex`` file. Ids are ranks.
* ``lexindex-hashed`` -- ``.lexindex.id_unchecked`` over a ``HashedDictIndex`` at zero fingerprint
  bits. Ids are ranks.
* ``lexindex-closed`` -- ``.lexindex.id`` over a ``ClosedHashIndex``. Ids are dense, not ranks.
* ``replace_strict`` -- ``Expr.replace_strict`` with the vocabulary and its ranks.
* ``join`` -- a left join on a two-column frame of the vocabulary and its ranks.
* ``enum`` -- a cast to ``pl.Enum`` over the sorted vocabulary, then ``to_physical``. Ids are ranks.
* ``categorical`` -- a cast to ``pl.Categorical``, then ``to_physical``. Ids are first-seen order,
  and nothing has to exist before the query: this is the baseline that needs no vocabulary.

The grid is rows x cardinality x method x engine. A cell's rows are drawn uniformly with a fixed
seed, so no row is hot: a skewed column would flatter every method that keeps a cache warm, and it
is the uniform one that says what a miss costs. ``engine`` matters because an elementwise plugin
runs on one thread for an in-memory single-chunk column and on every thread under the streaming
engine, while a join is parallel under both.

Every cell runs in a process of its own, so its peak resident memory is its own; ``fork`` is not
used, since Polars' thread pool does not survive one. An index is built in a process of its own too
and only its file reaches the query, because building is done once, offline, and the query's peak
should not carry it. The column is read from a Parquet file written by a third process, so the
vocabulary it was drawn from is not in a method's peak unless that method needs it. What a cell
records:

* ``build_s``, ``build_peak_mb``, ``blob_bytes`` -- the index, for the lexindex methods.
* ``setup_s`` -- what a method does in the query's process before its first row: reading the
  vocabulary for the Polars methods, nothing for the plugin, which reads its file on first use.
* ``first_s`` -- the first pass, and with it the plugin's load. ``map_s`` -- the later passes.
* ``column_peak_mb`` -- the process's high-water mark holding the column alone.
  ``peak_rss_mb`` -- the same at the end, so the difference is what the method added at its peak.
* ``output_bytes``, ``id_sum`` -- the answer. Every method whose ids are ranks must reach the same
  sum on one column, and the run refuses to write a file in which they do not.

The time columns need a quiet machine. Both wheels are abi3, and pip takes no abi3 wheel on a
free-threaded interpreter, hence ``--python 3.13``::

    uv run --no-project --python 3.13 --with dist/lexindex-*.whl --with dist/lexindex_polars-*.whl \
        python bench/polars_compare.py [--rows 1000000 10000000 100000000] [--corpus titles-en]

``--print-only`` writes no results file: a smoke run on a busy machine leaves nothing to be quoted.
"""

from __future__ import annotations

import argparse
import json
import os
import resource
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import _results
import corpora

LOW_CARDINALITY = 1_000
HIGH_CARDINALITY = 1_000_000
SEED = 7
# The plugin's methods, with the index each one's file holds.
INDEXES = {"lexindex-dict": "dict", "lexindex-hashed": "hashed", "lexindex-closed": "closed"}
METHODS = (*INDEXES, "replace_strict", "join", "enum", "categorical")
ENGINES = ("in-memory", "streaming")
# Each of these answers with the key's rank in the sorted vocabulary.
RANKS = {"lexindex-dict", "lexindex-hashed", "replace_strict", "join", "enum"}


def peak_mb() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024  # KB on Linux


def corpus_file(name: str, want: int) -> Path:
    """The smallest built file of a corpus holding at least `want` keys."""
    manifest = json.loads(corpora.MANIFEST.read_text(encoding="utf-8"))
    entry = manifest["corpora"].get(name)
    if entry is None:
        sys.exit(f"{name}: not in {corpora.MANIFEST}")
    for one in sorted(entry["files"], key=lambda f: f["keys"]):
        if one["keys"] >= want and (corpora.ROOT / one["file"]).exists():
            return corpora.ROOT / one["file"]
    sys.exit(f"{name}: no built file holds {want} keys (`python bench/corpora.py build {name}`)")


def prepare(work: Path, source: Path, rows: int, cardinality: int) -> None:
    """The sorted vocabulary and the column of rows, in a process that is not any cell."""
    import polars as pl

    keys = source.read_text(encoding="utf-8").splitlines()[:cardinality]
    if len(keys) < cardinality:
        sys.exit(f"{source}: {len(keys)} keys, {cardinality} asked for")
    vocab = pl.DataFrame({"k": sorted(keys)})
    vocab.write_parquet(work / "vocab.parquet")
    drawn = vocab["k"].sample(n=rows, with_replacement=True, shuffle=True, seed=SEED)
    drawn.to_frame("k").write_parquet(work / "rows.parquet")


def build_index(work: Path, kind: str) -> dict[str, Any]:
    """One index over the vocabulary, saved, in a process that will not query it."""
    import lexindex as lx

    import polars as pl

    keys = pl.read_parquet(work / "vocab.parquet")["k"].to_list()
    path = work / f"{kind}.blob"
    started = time.perf_counter()
    if kind == "dict":
        lx.DictIndex(keys).save(path)
    elif kind == "hashed":
        lx.HashedDictIndex.from_dict(lx.DictIndex(keys), 0).save(path)
    elif kind == "closed":
        lx.ClosedHashIndex(keys).save(path)
    else:
        raise ValueError(kind)
    return {
        "build_s": time.perf_counter() - started,
        "build_peak_mb": peak_mb(),
        "blob_bytes": path.stat().st_size,
    }


def measure(work: Path, method: str, engine: str, reps: int) -> dict[str, Any]:
    """One method over the column: its setup, its first pass, and `reps` passes after that."""
    import lexindex_polars

    import polars as pl

    rows = pl.read_parquet(work / "rows.parquet")
    column_peak = peak_mb()
    started = time.perf_counter()
    col = pl.col("k")
    if method == "lexindex-dict":
        query = rows.lazy().select(col.lexindex.id(work / "dict.blob"))
    elif method == "lexindex-hashed":
        query = rows.lazy().select(col.lexindex.id_unchecked(work / "hashed.blob"))
    elif method == "lexindex-closed":
        query = rows.lazy().select(col.lexindex.id(work / "closed.blob"))
    elif method == "categorical":
        query = rows.lazy().select(col.cast(pl.Categorical).to_physical())
    else:
        vocab = pl.read_parquet(work / "vocab.parquet")["k"]
        ranks = pl.int_range(vocab.len(), dtype=pl.UInt32, eager=True)
        if method == "replace_strict":
            query = rows.lazy().select(col.replace_strict(vocab, ranks, return_dtype=pl.UInt32))
        elif method == "join":
            dim = pl.DataFrame({"k": vocab, "id": ranks})
            query = rows.lazy().join(dim.lazy(), on="k", how="left").select("id")
        elif method == "enum":
            query = rows.lazy().select(col.cast(pl.Enum(vocab)).to_physical())
        else:
            raise ValueError(method)
    setup = time.perf_counter() - started

    def one_pass() -> tuple[float, pl.Series]:
        started = time.perf_counter()
        out = query.collect(engine=engine).to_series()
        return time.perf_counter() - started, out

    first, out = one_pass()
    # An answer that is short or holds a null would make the time below a time for something else.
    if out.len() != rows.height or out.null_count():
        raise RuntimeError(f"{method}: {out.len()} rows, {out.null_count()} null")
    passes = [one_pass()[0] for _ in range(reps)]
    return {
        "setup_s": setup,
        "first_s": first,
        "map_s": _results.summary(passes),
        "column_peak_mb": column_peak,
        "peak_rss_mb": peak_mb(),
        "output_bytes": out.estimated_size(),
        "id_sum": int(out.cast(pl.UInt64).sum()),
        "threads": pl.thread_pool_size(),
        "plugin": lexindex_polars.__version__,
    }


def child(*argv: str, timeout: float) -> dict[str, Any] | None:
    """This file again in a fresh process; `None` where it died, which an OOM kill is."""
    done = subprocess.run(  # argv list, never a shell string
        [sys.executable, __file__, *argv],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if done.returncode != 0:
        print(f"  failed ({done.returncode}): {done.stderr.strip()[-400:]}", file=sys.stderr)
        return None
    return json.loads(done.stdout.splitlines()[-1])


def grid(args: argparse.Namespace) -> list[dict[str, Any]]:
    cells: list[dict[str, Any]] = []
    for rows in args.rows:
        for cardinality in args.cardinality:
            source = corpus_file(args.corpus, cardinality)
            with tempfile.TemporaryDirectory(prefix="polars-compare-", dir=args.tmp) as tmp:
                prepared = child(
                    "--prepare", tmp, str(source), str(rows), str(cardinality), timeout=args.timeout
                )
                if prepared is None:
                    sys.exit("the column could not be written")
                builds = {
                    kind: child("--build", tmp, kind, timeout=args.timeout)
                    for method, kind in INDEXES.items()
                    if method in args.methods
                }
                sums: dict[int, set[str]] = {}
                for method in args.methods:
                    for engine in args.engines:
                        got = child(
                            "--measure", tmp, method, engine, str(args.reps), timeout=args.timeout
                        )
                        cell = {
                            "rows": rows,
                            "cardinality": cardinality,
                            "method": method,
                            "engine": engine,
                            **(builds.get(INDEXES.get(method, "")) or {}),
                            **(got or {"failed": "no result (OOM?)"}),
                        }
                        cells.append(cell)
                        print(line(cell), flush=True)
                        if got is not None and method in RANKS:
                            sums.setdefault(got["id_sum"], set()).add(method)
                if len(sums) > 1:
                    sys.exit(f"{rows} rows, {cardinality} keys: rank methods disagree: {sums}")
    return cells


def line(cell: dict[str, Any]) -> str:
    head = f"{cell['rows']:>11,} {cell['cardinality']:>9,} {cell['method']:<16}{cell['engine']:<10}"
    if "failed" in cell:
        return f"{head} {cell['failed']}"
    blob = f"{cell['blob_bytes'] / 1e6:>8.1f}" if "blob_bytes" in cell else f"{'-':>8}"
    return (
        f"{head} {blob} {cell['setup_s']:>7.3f} {cell['first_s']:>8.3f} "
        f"{cell['map_s']['min']:>8.3f} {cell['column_peak_mb']:>8.0f} {cell['peak_rss_mb']:>8.0f}"
    )


def main() -> int:
    # The three halves a cell runs in; each prints one JSON line for `child` to read.
    if len(sys.argv) > 1 and sys.argv[1] == "--prepare":
        prepare(Path(sys.argv[2]), Path(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5]))
        print("{}")
        return 0
    if len(sys.argv) > 1 and sys.argv[1] == "--build":
        print(json.dumps(build_index(Path(sys.argv[2]), sys.argv[3])))
        return 0
    if len(sys.argv) > 1 and sys.argv[1] == "--measure":
        work, method, engine, reps = Path(sys.argv[2]), sys.argv[3], sys.argv[4], int(sys.argv[5])
        print(json.dumps(measure(work, method, engine, reps)))
        return 0

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rows", type=int, nargs="*", default=[1_000_000, 10_000_000, 100_000_000])
    parser.add_argument(
        "--cardinality", type=int, nargs="*", default=[LOW_CARDINALITY, HIGH_CARDINALITY]
    )
    parser.add_argument("--corpus", default="titles-en")
    parser.add_argument("--methods", nargs="*", default=list(METHODS), choices=METHODS)
    parser.add_argument("--engines", nargs="*", default=list(ENGINES), choices=ENGINES)
    parser.add_argument("--reps", type=int, default=5, help="passes after the first")
    parser.add_argument("--timeout", type=float, default=3600.0, help="seconds a process may take")
    parser.add_argument("--tmp", default=os.environ.get("TMPDIR"), help="where the column is put")
    parser.add_argument("--print-only", action="store_true", help="write no results file")
    args = parser.parse_args()

    print(
        f"{'rows':>11} {'distinct':>9} {'method':<16}{'engine':<10} {'blob MB':>8} {'setup':>7} "
        f"{'first s':>8} {'map s':>8} {'col MB':>8} {'peak MB':>8}"
    )
    cells = grid(args)
    if args.print_only:
        print("--print-only: no results file written")
        return 0
    path = _results.write(
        "polars-compare",
        cells,
        corpus=args.corpus,
        seed=SEED,
        reps=args.reps,
        competitors=_results.versions("polars", "lexindex-polars"),
    )
    print(f"results → {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
