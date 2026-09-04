"""Scale benchmark: does lexindex hold up from 1M to 100M keys?

Measured on **real high-entropy keys** — `word_i.word_j` bigrams of the system dictionary, i.e.
realistic compound identifiers — never a synthetic `entity-{i}` sequence (which collapses the FST).
Each `(n, structure)` build runs in a fresh subprocess so peak resident memory is isolated per
configuration; an OOM-killed child is reported as such rather than crashing the run.

Reports, per structure and per **key source**: build time, serialised bytes/key, peak RSS during the
build, and rough point-lookup latency. The two sources answer different questions. Passing a *list*
includes the key list in the peak, which is what a caller who already holds the keys pays; passing a
*generator* is what `CompactHashIndex`'s streaming build is for, and is the only way to see its own
footprint rather than the corpus's.

Run: ``uv run --with <lexindex wheel> python bench/scale.py [N ...]``  (default: 1000000 10000000)
Pass e.g. `100000000` explicitly — that needs ~8 GB free for the key list alone.
"""

from __future__ import annotations

import math
import multiprocessing as mp
import os
import resource
import sys
import time
from collections.abc import Iterable, Iterator
from pathlib import Path


def load_vocab() -> list[str]:
    for p in (os.environ.get("LEXINDEX_BENCH_WORDS"), "/usr/share/dict/words"):
        if p and Path(p).exists():
            with open(p, encoding="utf-8", errors="ignore") as f:
                return sorted({line.strip() for line in f if line.strip()})
    sys.exit("no word list; set LEXINDEX_BENCH_WORDS or install a system dictionary.")


def iter_keys(n: int, vocab: list[str]) -> Iterator[str]:
    """`n` realistic compound keys `word_i.word_j` (natural prefix sharing, high entropy)."""
    m = min(math.ceil(math.sqrt(n)), len(vocab))
    v = vocab[:m]
    w = len(v)
    return (f"{v[k % w]}.{v[(k // w) % w]}" for k in range(n))


def make_keys(n: int, vocab: list[str]) -> list[str]:
    """[`iter_keys`] materialised — what a caller with the keys already in memory would pass."""
    return list(iter_keys(n, vocab))


def _build(kind: str, items: Iterable[str]) -> object:
    import lexindex

    if kind == "StringIndex":
        return lexindex.StringIndex(items)
    return lexindex.CompactHashIndex(items, 1)


def _bench_one(n: int, kind: str, source: str, q: mp.Queue) -> None:
    """One `(n, structure, source)` cell, in its own process so peak RSS is isolated.

    `source` is what the caller hands the constructor. A **list** is the honest floor when the keys
    are already in memory: the list itself dominates peak RSS at these sizes. A **generator** is the
    case the streaming build exists for — `CompactHashIndex` keeps a 16-byte pair per key and drops
    the string, so nothing ever holds the corpus. The other structures store their keys and will
    materialise them regardless; measuring them both ways is what shows which claim is which.
    """
    vocab = load_vocab()
    keys = make_keys(n, vocab) if source == "list" else iter_keys(n, vocab)
    t = time.perf_counter()
    idx = _build(kind, keys)
    build_ms = (time.perf_counter() - t) * 1e3
    del keys
    n_act = len(idx)  # distinct (build dedups) — cheaper than set(keys)
    bpk = len(idx.to_bytes()) / n_act
    # Regenerated rather than held: in the generator run there is no key list left to sample from,
    # and taking one would put the thing being measured back into the process.
    step = max(1, n // 10_000)
    sample = [k for i, k in enumerate(iter_keys(n, vocab)) if i % step == 0]
    t = time.perf_counter()
    for s in sample:
        idx.id(s)
    lookup_ns = (time.perf_counter() - t) / len(sample) * 1e9
    peak_mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024  # ru_maxrss is KB on Linux
    q.put((n_act, kind, source, build_ms, bpk, peak_mb, lookup_ns))


def main() -> None:
    ns = [int(a) for a in sys.argv[1:]] or [1_000_000, 10_000_000]
    print(
        f"{'n':>13}  {'structure':16} {'keys':9} {'build':>9} {'B/key':>7} "
        f"{'peak RSS':>10} {'lookup':>9}"
    )
    print("-" * 82)
    for n in ns:
        for kind in ("StringIndex", "CompactHashIndex"):
            for source in ("list", "generator"):
                q: mp.Queue = mp.Queue()
                p = mp.Process(target=_bench_one, args=(n, kind, source, q))
                p.start()
                p.join()
                if q.empty():
                    print(f"{n:>13,}  {kind:16} {source:9}  (failed — likely OOM at this n)")
                    continue
                n_act, k, src, build_ms, bpk, peak_mb, lookup_ns = q.get()
                print(
                    f"{n_act:>13,}  {k:16} {src:9} {build_ms:>7.0f}ms {bpk:>6.2f} "
                    f"{peak_mb:>8.0f}MB {lookup_ns:>7.0f}ns"
                )


if __name__ == "__main__":
    main()
