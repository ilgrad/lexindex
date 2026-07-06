"""Scale benchmark: does lexindex hold up from 1M to 100M keys?

Measured on **real high-entropy keys** — `word_i.word_j` bigrams of the system dictionary, i.e.
realistic compound identifiers — never a synthetic `entity-{i}` sequence (which collapses the FST).
Each `(n, structure)` build runs in a fresh subprocess so peak resident memory is isolated per
configuration; an OOM-killed child is reported as such rather than crashing the run.

Reports, per structure: build time, serialised bytes/key, peak RSS during build (includes the input
key list — you need the keys in memory to build), and rough point-lookup latency.

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
from pathlib import Path


def load_vocab() -> list[str]:
    for p in (os.environ.get("LEXINDEX_BENCH_WORDS"), "/usr/share/dict/words"):
        if p and Path(p).exists():
            with open(p, encoding="utf-8", errors="ignore") as f:
                return sorted({line.strip() for line in f if line.strip()})
    sys.exit("no word list; set LEXINDEX_BENCH_WORDS or install a system dictionary.")


def make_keys(n: int, vocab: list[str]) -> list[str]:
    """`n` realistic compound keys `word_i.word_j` (natural prefix sharing, high entropy)."""
    m = min(math.ceil(math.sqrt(n)), len(vocab))
    v = vocab[:m]
    w = len(v)
    return [f"{v[k % w]}.{v[(k // w) % w]}" for k in range(n)]


def _bench_one(n: int, kind: str, q: mp.Queue) -> None:
    import lexindex

    keys = make_keys(n, load_vocab())
    t = time.perf_counter()
    idx = (
        lexindex.StringIndex(keys) if kind == "StringIndex" else lexindex.CompactHashIndex(keys, 1)
    )
    build_ms = (time.perf_counter() - t) * 1e3
    n_act = len(idx)  # distinct (build dedups) — cheaper than set(keys)
    bpk = len(idx.to_bytes()) / n_act
    sample = keys[:: max(1, len(keys) // 10_000)]
    t = time.perf_counter()
    for s in sample:
        idx.id(s)
    lookup_ns = (time.perf_counter() - t) / len(sample) * 1e9
    peak_mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024  # ru_maxrss is KB on Linux
    q.put((n_act, kind, build_ms, bpk, peak_mb, lookup_ns))


def main() -> None:
    ns = [int(a) for a in sys.argv[1:]] or [1_000_000, 10_000_000]
    print(f"{'n':>13}  {'structure':16} {'build':>9} {'B/key':>7} {'peak RSS':>10} {'lookup':>9}")
    print("-" * 70)
    for n in ns:
        for kind in ("StringIndex", "CompactHashIndex"):
            q: mp.Queue = mp.Queue()
            p = mp.Process(target=_bench_one, args=(n, kind, q))
            p.start()
            p.join()
            if q.empty():
                print(f"{n:>13,}  {kind:16}  (failed — likely OOM at this n)")
                continue
            n_act, k, build_ms, bpk, peak_mb, lookup_ns = q.get()
            print(
                f"{n_act:>13,}  {k:16} {build_ms:>7.0f}ms {bpk:>6.2f} "
                f"{peak_mb:>8.0f}MB {lookup_ns:>7.0f}ns"
            )


if __name__ == "__main__":
    main()
