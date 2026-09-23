"""Chinese dictionary segmentation, timed: jieba's dictionary against lexindex's, in jieba's code.

jieba finds every dictionary word at every character of a block (`get_DAG`) and takes the most
probable route through them (`calc`). This harness keeps the whole of jieba 0.42.1 and answers those
two -- and the one question its HMM asks, whether a run of single characters is a word -- from each
structure in turn, on one lexicon and one text. Every variant's DAG and every token of both cuts
are checked against jieba's own before anything is timed.

Lexicon: jieba 0.42.1's `dict.txt` (MIT), the download `bench/corpora.py` pins by SHA-256.
Text: UD Chinese GSDSimp r2.18 (CC BY-SA 4.0), the `# text` line of every sentence of train, dev
and test, in that order: 4 997 sentences, fetched and checked by SHA-256 at run time, and not
redistributed here.

  dag      every word at every character of every block jieba cuts, as `get_DAG` returns it: the
           dictionary's share of segmentation
  cut      `lcut(HMM=False)`: jieba's cut, with the dictionary answered by the structure
  cut-hmm  `lcut()`, jieba's default: the same, and its HMM over each run of single characters
  load     the dictionary made ready in a fresh process: jieba building its prefix dict or reading
           its cache, each structure mapped or read from its file, with the frequency array a
           segmenter keeps beside the keys

A timing process runs every variant once a round, the order reversing every round, and the
processes run one after another; a cell is the minimum over every round of every process, with
the samples kept beside it. jieba over its own dictionary is the control row: no lexindex change
can move it. The run is only as good as the machine was quiet.

Run against a released wheel, so that the numbers belong to it, on a GIL build of CPython:
  uv run --no-project --python /usr/bin/python3.14 --with lexindex==4.2.0 --with jieba==0.42.1 \\
         --with marisa-trie --with dawg2 python bench/cjk_segment.py [--processes 3] [--rounds 5]
"""

from __future__ import annotations

import argparse
import array
import gc
import json
import logging
import math
import mmap
import random
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import _results
import corpora

UD_TAG = "r2.18"
UD = f"https://raw.githubusercontent.com/UniversalDependencies/UD_Chinese-GSDSimp/{UD_TAG}"
UD_FILES = {  # read in this order; each file's SHA-256 at the tag, fetched 2026-09-23
    "train": "956636fe612a1166e8b19e7413fee2e73d68231aca2f0455be2c616b947d629d",
    "dev": "d03f1eeb93b16071bfbbe6c76b971554be87c9a2307b3f3a820dd7c07f73fb63",
    "test": "3af8046a6f32477b4d5cf3dd06bbf38682a380fe77aade3f68de97e51ab94900",
}
LEXICON = f"jieba-{corpora.JIEBA_TAG}-dict.txt"
LEXICON_URL = f"https://raw.githubusercontent.com/fxsjy/jieba/{corpora.JIEBA_TAG}/jieba/dict.txt"
SEED = 0x5A32  # the order the sentences are cut in
MAXLEN = 16  # the lexicon's longest word, in characters; asserted
CONTROL = "jieba, own dictionary"
DAG_ONLY = "dawg2"  # keeps no ids, so a cut would add a dict lookup a match: timed on `dag` only
LOADS = (
    "jieba-build",
    "jieba-cache",
    "string-mmap",
    "dict256-mmap",
    "dict256-bytes",
    "marisa-mmap",
    "dawg-load",
)


def _pinned(url: str, name: str, sha256: str) -> Path:
    """The cached download, fetched if absent, and refused unless it hashes to `sha256`."""
    path = corpora._download(url, name)
    if (got := corpora._sha256(path)) != sha256:
        sys.exit(f"{path}: sha256 {got}, pinned {sha256}")
    return path


def lexicon() -> tuple[Path, list[str], dict[str, int], int]:
    """The file, its words in code-point order (= UTF-8 byte order = lexindex's ids), each word's
    frequency as jieba keeps it (the last line wins), and jieba's total (every line counts)."""
    pins = json.loads(corpora.MANIFEST.read_text(encoding="utf-8"))["downloads"]
    path = _pinned(LEXICON_URL, LEXICON, pins[LEXICON]["sha256"])
    freq: dict[str, int] = {}
    total = 0
    for line in path.read_text(encoding="utf-8").splitlines():
        if line:
            word, f = line.split(" ")[:2]
            freq[word] = int(f)
            total += int(f)
    words = sorted(freq)
    # jieba's DAG skips a word of frequency zero; no structure here would, so none may exist.
    assert max(map(len, words)) == MAXLEN and min(freq.values()) > 0
    return path, words, freq, total


def text() -> list[str]:
    sentences: list[str] = []
    for part, sha in UD_FILES.items():
        name = f"zh_gsdsimp-ud-{part}.conllu"
        path = _pinned(f"{UD}/{name}", f"ud-{UD_TAG}-{name}", sha)
        lines = path.read_text(encoding="utf-8").splitlines()
        sentences += [line[len("# text = ") :] for line in lines if line.startswith("# text = ")]
    random.Random(SEED).shuffle(sentences)
    return sentences


def blocks_of(sentences: list[str]) -> list[str]:
    """The runs jieba hands its dictionary: the pieces of each sentence its own pattern matches."""
    import jieba

    han = jieba.re_han_default
    return [b for s in sentences for b in han.split(s) if b and han.match(b)]


def write_files(work: Path, words: list[str], freq: dict[str, int]) -> None:
    """Every structure, written once: the timing and load processes map or read them from here."""
    import dawg
    import lexindex
    import marisa_trie

    lexindex.StringIndex(words).save(work / "string.bix")
    lexindex.DictIndex(words).save(work / "dict256.bdx")
    marisa_trie.Trie(words).save(str(work / "words.marisa"))
    dawg.DAWG(words).save(str(work / "words.dawg"))
    logf = array.array("d", [math.log(freq[w]) for w in words])
    (work / "logf.f64").write_bytes(logf.tobytes())


def _logf(work: Path) -> memoryview:
    with open(work / "logf.f64", "rb") as f:
        return memoryview(mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)).cast("d")


class Words:
    """What jieba's HMM asks of its `FREQ`: whether a run of single characters is a word with a
    frequency. Every word here has one (asserted), so being a key is the answer."""

    def __init__(self, id_of: Callable[[str], int | None]) -> None:
        self.id_of = id_of

    def get(self, word: str, default: object = None) -> object:
        return default if self.id_of(word) is None else 1


def variants(work: Path, lexicon_path: Path, words: list[str], total: int) -> dict[str, Any]:
    """jieba, and jieba with its dictionary answered by each structure; ids index frequencies."""
    import dawg
    import jieba
    import lexindex
    import marisa_trie

    jieba.setLogLevel(logging.WARNING)
    base = jieba.Tokenizer(dictionary=str(lexicon_path))
    base.cache_file = str(work / "jieba.cache")
    base.initialize()
    assert base.total == total

    logf_rank = [*_logf(work), 0.0]  # the last is log(1): jieba's `FREQ.get(frag) or 1`
    none = len(logf_rank) - 1

    class Backed(jieba.Tokenizer):
        """jieba with `get_DAG` and `calc` answered by `walk(text) -> [(word, id)]`, one call a
        character, and its HMM's word check by `words`."""

        def __init__(self, walk, logf, words: Words) -> None:
            super().__init__()
            self.walk, self.logf, self.total, self.initialized = walk, logf, total, True
            self.FREQ = words

        def get_DAG(self, sentence):
            walk, dag = self.walk, {}
            for k in range(len(sentence)):
                ends = [(k + len(w) - 1, i) for w, i in walk(sentence[k : k + MAXLEN])]
                dag[k] = ends or [(k, none)]
            return dag

        def calc(self, sentence, DAG, route):
            n = len(sentence)
            route[n] = (0, 0)
            logtotal = math.log(self.total)
            logf = self.logf
            for idx in range(n - 1, -1, -1):
                route[idx] = max((logf[i] - logtotal + route[x + 1][0], x) for x, i in DAG[idx])

    class Whole(Backed):
        """The same, with the DAG from one `occurrences(block)` call instead of one a character."""

        def get_DAG(self, sentence):
            dag = {k: [] for k in range(len(sentence))}
            for start, end, i in self.walk(sentence):
                dag[start].append((end - 1, i))
            for k, ends in dag.items():
                if not ends:
                    ends.append((k, none))
            return dag

    s = lexindex.StringIndex.load_mmap(work / "string.bix")
    d = lexindex.DictIndex.load_mmap(work / "dict256.bdx")
    m = marisa_trie.Trie()
    m.mmap(str(work / "words.marisa"))
    # marisa numbers its keys its own way, so it gets a frequency table in its own order.
    logf_marisa = [0.0] * (len(words) + 1)
    for i, w in enumerate(words):
        logf_marisa[m[w]] = logf_rank[i]
    g = dawg.DAWG()
    g.load(str(work / "words.dawg"))
    return {
        CONTROL: base,
        "lexindex StringIndex occurrences": Whole(s.occurrences, logf_rank, Words(s.id)),
        "lexindex StringIndex common_prefix": Backed(s.common_prefix, logf_rank, Words(s.id)),
        "lexindex DictIndex 256 common_prefix": Backed(d.common_prefix, logf_rank, Words(d.id)),
        "marisa-trie": Backed(
            lambda q: list(m.iter_prefixes_with_ids(q)), logf_marisa, Words(m.get)
        ),
        DAG_ONLY: Backed(
            lambda q: [(w, none) for w in g.prefixes(q)],
            logf_rank,
            Words(lambda w: 0 if w in g else None),
        ),
    }


def check(vs: dict[str, Any], sentences: list[str], blocks: list[str]) -> None:
    """Every variant's DAG ends and every token of both cuts equal jieba's, or nothing is timed."""
    base = vs[CONTROL]
    for name, v in vs.items():
        if v is base:
            continue
        for b in blocks:
            got = {k: [x for x, _ in ends] for k, ends in v.get_DAG(b).items()}
            if got != base.get_DAG(b):
                sys.exit(f"{name}: DAG differs from jieba's on {b!r}")
        if name == DAG_ONLY:
            continue
        for s in sentences:
            if v.lcut(s, HMM=False) != base.lcut(s, HMM=False) or v.lcut(s) != base.lcut(s):
                sys.exit(f"{name}: cut differs from jieba's on {s!r}")


def timed(fn: Callable[[], object]) -> int:
    gc.collect()
    gc.disable()
    t0 = time.perf_counter_ns()
    fn()
    t = time.perf_counter_ns() - t0
    gc.enable()
    return t


def time_child(work: Path, rounds: int) -> dict[str, list[int]]:
    """One timing process: ns a column a variant, one sample a round."""
    path, words, _, total = lexicon()
    sentences = text()
    blocks = blocks_of(sentences)
    vs = variants(work, path, words, total)
    names = list(vs)
    samples: dict[str, list[int]] = {}
    for r in range(rounds):
        for n in names if r % 2 == 0 else names[::-1]:
            v = vs[n]
            runs: dict[str, Callable[[], object]] = {
                "dag": lambda v=v: [v.get_DAG(b) for b in blocks]
            }
            if n != DAG_ONLY:
                runs["cut"] = lambda v=v: [v.lcut(s, HMM=False) for s in sentences]
                runs["cut-hmm"] = lambda v=v: [v.lcut(s) for s in sentences]
            for column, fn in runs.items():
                samples.setdefault(f"{column}|{n}", []).append(timed(fn))
    return samples


def load_child(work: Path, lexicon_path: Path, name: str) -> int:
    """Time making one dictionary ready, in this fresh process; imports are outside the clock."""
    import dawg
    import jieba
    import lexindex
    import marisa_trie

    jieba.setLogLevel(logging.WARNING)
    if name.startswith("jieba-"):
        tok = jieba.Tokenizer(dictionary=str(lexicon_path))
        building = name == "jieba-build"
        tok.cache_file = str(work / ("jieba-build.cache" if building else "jieba.cache"))
        t0 = time.perf_counter_ns()
        tok.initialize()
        t = time.perf_counter_ns() - t0
        if building:
            Path(tok.cache_file).unlink()
        return t
    t0 = time.perf_counter_ns()
    idx: object
    if name == "string-mmap":
        idx = lexindex.StringIndex.load_mmap(work / "string.bix")
    elif name == "dict256-mmap":
        idx = lexindex.DictIndex.load_mmap(work / "dict256.bdx")
    elif name == "dict256-bytes":
        idx = lexindex.DictIndex.from_bytes((work / "dict256.bdx").read_bytes())
    elif name == "marisa-mmap":
        idx = marisa_trie.Trie()
        idx.mmap(str(work / "words.marisa"))
    else:
        idx = dawg.DAWG()
        idx.load(str(work / "words.dawg"))
    logf = _logf(work)  # the frequencies beside the keys: mapped, not copied
    t = time.perf_counter_ns() - t0
    assert len(logf) > 0 and idx is not None
    return t


def _child(*argv: str) -> str:
    done = subprocess.run(
        [sys.executable, "-W", "ignore", __file__, *argv],
        capture_output=True,
        text=True,
        check=True,
    )
    return done.stdout.strip().splitlines()[-1]


def _hottest() -> float | None:
    """The hottest thermal zone, in degrees C: a throttled core answers slower and says nothing."""
    temps = []
    for zone in Path("/sys/class/thermal").glob("thermal_zone*/temp"):
        try:
            temps.append(int(zone.read_text()) / 1000)
        except (OSError, ValueError):
            continue
    return max(temps, default=None)


def main() -> int:
    if sys.argv[1:2] == ["_time"]:
        print(json.dumps(time_child(Path(sys.argv[2]), int(sys.argv[3]))))
        return 0
    if sys.argv[1:2] == ["_load"]:
        print(load_child(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4]))
        return 0
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--processes", type=int, default=3, help="timing processes, run in turn")
    ap.add_argument("--rounds", type=int, default=5, help="rounds a timing process")
    ap.add_argument("--loads", type=int, default=5, help="fresh processes a load row")
    args = ap.parse_args()

    import lexindex

    hot_start = _hottest()
    path, words, freq, total = lexicon()
    sentences = text()
    blocks = blocks_of(sentences)
    chars = {"text": sum(map(len, sentences)), "blocks": sum(map(len, blocks))}
    work = Path(tempfile.mkdtemp(prefix="lexindex-cjk-segment-"))
    try:
        write_files(work, words, freq)
        check(variants(work, path, words, total), sentences, blocks)
        print(
            f"checked: every DAG equals jieba's on {len(blocks):,} blocks, every cut with and "
            f"without the HMM on {len(sentences):,} sentences",
            flush=True,
        )
        samples: dict[str, list[int]] = {}
        for p in range(args.processes):
            for key, got in json.loads(_child("_time", str(work), str(args.rounds))).items():
                samples.setdefault(key, []).extend(got)
            print(f"timing process {p + 1}/{args.processes} done", flush=True)
        loads: dict[str, list[int]] = {n: [] for n in LOADS}
        for r in range(args.loads):
            for n in LOADS if r % 2 == 0 else LOADS[::-1]:
                loads[n].append(int(_child("_load", str(work), str(path), n)))
    finally:
        shutil.rmtree(work)
    hot_end = _hottest()

    cells: list[dict[str, Any]] = []
    for key, got in samples.items():
        column, variant = key.split("|", 1)
        per = chars["blocks"] if column == "dag" else chars["text"]
        cells.append(
            {
                "variant": variant,
                "column": column,
                "unit": "ns/char",
                **_results.summary([t / per for t in got]),
            }
        )
    for n, got in loads.items():
        cells.append(
            {
                "variant": n,
                "column": "load",
                "unit": "ms",
                **_results.summary([t / 1e6 for t in got]),
            }
        )

    by = {(c["column"], c["variant"]): c["min"] for c in cells}
    print(f"\n{'variant':<38} {'dag':>7} {'cut':>7} {'cut-hmm':>8}  ns/char, min")
    for n in dict.fromkeys(v for _, v in by):
        if ("dag", n) not in by:
            continue
        row = [by.get((col, n)) for col in ("dag", "cut", "cut-hmm")]
        print(f"{n:<38} " + " ".join(f"{x:7.0f}" if x else f"{'--':>7}" for x in row))
    print(f"\n{'load, fresh process':<38} {'ms':>9}")
    for n in LOADS:
        print(f"{n:<38} {by['load', n]:9.3f}")

    out = _results.write(
        "cjk-segment",
        cells,
        lexicon=LEXICON,
        words=len(words),
        text=f"UD Chinese GSDSimp {UD_TAG}, train+dev+test, shuffled with seed {SEED:#x}",
        sentences=len(sentences),
        characters=chars,
        processes=args.processes,
        rounds=args.rounds,
        loads=args.loads,
        gil=not sysconfig.get_config_var("Py_GIL_DISABLED"),
        lexindex_file=lexindex.__file__,
        thermal_c={"start": hot_start, "end": hot_end},
        competitors=_results.versions("jieba", "marisa-trie", "dawg2"),
    )
    print(f"results → {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
