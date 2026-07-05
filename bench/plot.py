"""Zero-copy mmap load scaling for lexindex — the persistence figure for the article.

`load` reads the whole blob into RAM, so its time grows with the index; `load_mmap` memory-maps the
file and borrows it, so load time is ~constant in the index size. This races the two as the key
count grows, on a real vocabulary. **Size and capability comparisons vs marisa-trie / DAWG / datrie
live in `compare.py`** — this script is only the mmap-load-scaling plot.

Run: ``uv run --with matplotlib --with <lexindex wheel> python bench/plot.py [WORDS_FILE]``
Absolute timings are machine-dependent; the *shape* (flat mmap vs rising owned load) is the point.
"""

from __future__ import annotations

import os
import sys
import tempfile
import time
from pathlib import Path

import lexindex
import matplotlib.pyplot as plt

OUT = Path(__file__).parent / "plots"
OUT.mkdir(exist_ok=True)


def _load_words() -> list[str]:
    for path in (
        sys.argv[1] if len(sys.argv) > 1 else None,
        os.environ.get("LEXINDEX_BENCH_WORDS"),
        "/usr/share/dict/words",
    ):
        if path and Path(path).exists():
            with open(path, encoding="utf-8", errors="ignore") as f:
                words = sorted({line.strip() for line in f if line.strip()})
            if words:
                return words
    sys.exit("no word list found; pass a path as argv[1] or set LEXINDEX_BENCH_WORDS.")


def _best(fn, rounds: int = 5) -> float:
    """Min wall-clock (ms) over a few rounds — the least-noisy estimate."""
    best = float("inf")
    for _ in range(rounds):
        t = time.perf_counter()
        fn()
        best = min(best, (time.perf_counter() - t) * 1e3)
    return best


def fig_mmap_load(words: list[str]) -> None:
    ns = [n for n in (10_000, 30_000, 100_000, 300_000, len(words)) if n <= len(words)]
    owned_ms, mmap_ms = [], []
    with tempfile.TemporaryDirectory() as d:
        path = str(Path(d) / "idx.bix")
        for n in ns:
            lexindex.StringIndex(words[:n]).save(path)
            owned_ms.append(_best(lambda: lexindex.StringIndex.load(path)))
            mmap_ms.append(_best(lambda: lexindex.StringIndex.load_mmap(path)))
    fig, ax = plt.subplots(figsize=(7, 4.2))
    ax.plot(ns, owned_ms, "o-", color="#3949ab", label="StringIndex.load  (read into RAM)")
    ax.plot(ns, mmap_ms, "s-", color="#00897b", label="StringIndex.load_mmap  (zero-copy)")
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("number of keys (n)")
    ax.set_ylabel("load time (ms, min of 5)")
    ax.set_title("Zero-copy mmap load is ~O(1) in the index size")
    ax.legend(frameon=False)
    ax.grid(True, which="both", ls=":", alpha=0.4)
    ax.spines[["top", "right"]].set_visible(False)
    fig.tight_layout()
    fig.savefig(OUT / "mmap_load.png", dpi=140)
    plt.close(fig)
    speedup = owned_ms[-1] / mmap_ms[-1]
    print(
        f"mmap_load.png: at n={ns[-1]:,}, load {owned_ms[-1]:.2f} ms vs load_mmap "
        f"{mmap_ms[-1]:.3f} ms ({speedup:.0f}x faster)"
    )


if __name__ == "__main__":
    fig_mmap_load(_load_words())
    print(f"plot written to {OUT}/")
