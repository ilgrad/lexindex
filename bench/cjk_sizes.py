"""Every structure a Chinese dictionary segmenter could keep its lexicon in, on jieba's lexicon.

jieba and its ports (jieba-rs, jieba-wasm, cppjieba) find every dictionary word at every character
of a text and pick the most probable route through them, so the lexicon is the one structure the
segmenter cannot do without. This prices it: the words of jieba's `dict.txt`, from the download
`bench/corpora.py` pins by SHA-256, in each structure that can answer the query a segmenter asks.

The serialised sizes are deterministic, and so it needs no quiet machine; the two structures that
live only in a Python heap are the bytes `tracemalloc` sees them take, which move by a few dozen
bytes between runs. The segmenter also keeps a frequency a word, priced below the table because
every structure here needs it beside the keys. `marisa-trie` is a curve, as in `bench/sweep.py`:
the row is its smallest configuration, and names it.

libdatrie maps its alphabet onto an 8-bit trie character: datrie 0.8.3 builds 255 distinct
characters and corrupts the heap at 256, and this lexicon has 12 045. It runs in a child process,
so its death is the answer rather than the end of the run.

Run:
  uv run --no-sync --with marisa-trie --with dawg2 --with datrie --with jieba==0.42.1 \\
         python bench/cjk_sizes.py
"""

from __future__ import annotations

import gc
import hashlib
import json
import subprocess
import sys
import tracemalloc
from collections.abc import Callable
from pathlib import Path

import _results
import corpora
import dawg
import jieba
import lexindex
import marisa_trie

DOWNLOAD = f"jieba-{corpora.JIEBA_TAG}-dict.txt"
DATRIE_CHILD = """
import os, sys, tempfile, datrie
words = sorted({l.split(" ")[0] for l in open(sys.argv[1], encoding="utf-8") if l.strip()})
alphabet = {c for w in words for c in w}
t = datrie.Trie(ranges=[(min(alphabet), max(alphabet))])
for w in words:
    t[w] = 0
with tempfile.TemporaryDirectory() as d:
    t.save(os.path.join(d, "trie"))
    print(os.path.getsize(os.path.join(d, "trie")))
"""


def _lexicon() -> Path:
    """The pinned download, refused unless it hashes to what the manifest recorded."""
    want = json.loads(corpora.MANIFEST.read_text(encoding="utf-8"))["downloads"][DOWNLOAD]
    path = corpora.DOWNLOADS / DOWNLOAD
    if not path.exists():
        sys.exit(f"{path} missing: `uv run --no-sync python bench/corpora.py build jieba-dict`")
    got = hashlib.sha256(path.read_bytes()).hexdigest()
    if got != want["sha256"]:
        sys.exit(f"{path}: sha256 {got}, the manifest pins {want['sha256']}")
    return path


def _heap(build: Callable[[], object]) -> int:
    gc.collect()
    tracemalloc.start()
    before = tracemalloc.get_traced_memory()[0]
    kept = build()
    gc.collect()
    after = tracemalloc.get_traced_memory()[0]
    tracemalloc.stop()
    del kept
    return after - before


def main() -> int:
    path = _lexicon()
    lines = [line.split(" ") for line in path.read_text(encoding="utf-8").splitlines() if line]
    words = sorted({e[0] for e in lines})
    n = len(words)
    cells: list[dict[str, object]] = []

    def cell(structure: str, size: int | None, note: str = "") -> None:
        cells.append({"structure": structure, "bytes": size, "note": note})

    cell("lexindex StringIndex", len(lexindex.StringIndex(words).to_bytes()), "prefix walk")
    dicts = {b: lexindex.DictIndex(words, block=b) for b in (32, 256, 1024)}
    for b, d in dicts.items():
        cell(f"lexindex DictIndex {b}", len(d.to_bytes()), "one order lookup a boundary")
    for bits in (0, 8):
        h = lexindex.HashedDictIndex.from_dict(dicts[256], bits)
        cell(f"lexindex HashedDictIndex 256 fp={bits}", len(h.to_bytes()), "exact id by hash")
    cell(
        "lexindex CompactHashIndex fp=8",
        len(lexindex.CompactHashIndex(words).to_bytes()),
        "holds no keys: cannot enumerate prefixes",
    )
    tries = {t: len(marisa_trie.Trie(words, num_tries=t).tobytes()) for t in (1, 2, 3, 4, 5, 6, 8)}
    best = min(tries, key=tries.__getitem__)
    cell("marisa-trie, smallest", tries[best], f"num_tries={best}; default 3: {tries[3]:,}")
    cell("dawg2 DAWG", len(dawg.DAWG(words).tobytes()))
    child = subprocess.run(
        [sys.executable, "-c", DATRIE_CHILD, str(path)], capture_output=True, text=True, check=False
    )
    if child.returncode == 0:
        cell("datrie", int(child.stdout.split()[-1]))
    else:
        tail = (child.stderr.strip().splitlines() or ["no output"])[-1]
        cell("datrie", None, f"died, signal {-child.returncode}: {tail[:60]}")

    tok = jieba.Tokenizer()
    pf: dict[str, object] = {}

    def prefix_dict() -> object:
        with open(path, "rb") as f:
            pf["freq"], _ = tok.gen_pfdict(f)
        return pf["freq"]

    size = _heap(prefix_dict)
    bare = sum(1 for v in pf["freq"].values() if v == 0)
    cell(
        "jieba prefix dict (Python heap)",
        size,
        f"{len(pf['freq']):,} entries, {bare:,} of them bare prefixes",
    )
    # Parsed inside the count, as `gen_pfdict` parses inside it: strings built beforehand would be
    # left out of it.
    size = _heap(
        lambda: {
            (p := line.split(" "))[0]: int(p[1])
            for line in path.read_text(encoding="utf-8").splitlines()
            if line
        }
    )
    cell("python dict word -> freq", size, "no prefixes; keys parsed inside the count")

    print(f"jieba {corpora.JIEBA_TAG} dict.txt: {len(lines):,} lines, {n:,} distinct words")
    print(f"{'structure':<36} {'bytes':>11} {'B/word':>7}  note")
    for c in cells:
        b = c["bytes"]
        shown = f"{b:>11,} {b / n:7.2f}" if isinstance(b, int) else f"{'--':>11} {'--':>7}"
        print(f"{c['structure']:<36} {shown}  {c['note']}")
    print(f"beside any of them, a frequency a word: {4 * n:,} B as u32, {8 * n:,} B as f64")
    out = _results.write(
        "cjk-sizes",
        cells,
        corpus=DOWNLOAD,
        words=n,
        competitors=_results.versions("marisa-trie", "dawg2", "datrie", "jieba"),
    )
    print(f"results → {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
