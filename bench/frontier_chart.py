"""The front-page figure: `DictIndex` against the smallest trie, on every corpus of the set.

    uv run --with matplotlib python bench/frontier_chart.py \
        bench/results/frontier-1m-<date>-<host>-<commit>.json

Reads one campaign artifact written by `bench/frontier/tables.py` and draws the comparison the
benchmarks page makes in prose: for each corpus, the smallest `DictIndex` block against the
smallest structure anyone else in the campaign built. Writes an SVG beside the docs, named after
the artifact's own commit, so a figure in the README can always be traced to the run behind it.

Three choices the figure makes, each of which could be made dishonestly:

* **`DictIndex` only, not lexindex's best.** `StringIndex` is smaller than every trie on `numeric`
  (0.0003 bytes a key -- an FST minimises dense decimal ids almost entirely away), and putting that
  in a size headline is the mirage this repository's benchmarks exist to refuse. The comparison is
  between structures that store their keys and answer the same questions, so it is the dictionary's.
* **The competitor's best configuration, not its default.** MARISA at the `num_tries` that suits
  that corpus, XCDAT at its best of four, CoCo and PDT as the C² benchmark builds them. A win
  against a badly-tuned baseline is not a win.
* **A linear axis.** The corpora span 0.5 to 21 bytes a key and a log axis would flatter the ratios;
  the numbers are printed on the bars instead.

`ART` and `C-ART` are excluded, as everywhere else: they count their nodes and not the keys those
nodes point into, so they are not comparable on size.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import matplotlib

# The backend must be chosen before pyplot is imported, so this import is not at the top.
matplotlib.use("Agg")
import matplotlib.pyplot as plt

# Structures whose reported size is not the whole dictionary, so they never enter a comparison.
REFERENCE = {"ART", "C-ART"}
LEX = "#1e40af"
RIVAL = "#94a3b8"
LOSS = "#b45309"


def rows(artifact: dict) -> list[dict]:
    """One row a corpus: the smallest `DictIndex` block against the smallest structure
    anyone else built."""
    by_corpus: dict[str, list[dict]] = {}
    for cell in artifact["cells"]:
        if cell.get("bytes_per_key") is None or cell["structure"] in REFERENCE:
            continue
        by_corpus.setdefault(cell["corpus"], []).append(cell)

    out = []
    for corpus, cells in by_corpus.items():
        ours = [c for c in cells if c["structure"].startswith("lexindex Dict")]
        theirs = [c for c in cells if not c["structure"].startswith("lexindex")]
        if not ours or not theirs:
            continue
        best_ours = min(ours, key=lambda c: c["bytes_per_key"])
        best_theirs = min(theirs, key=lambda c: c["bytes_per_key"])
        out.append(
            {
                "corpus": corpus.replace("-1000000", "").replace("-full", ""),
                "keys": best_ours["keys"],
                "ours": best_ours["bytes_per_key"],
                "ours_label": best_ours["structure"].removeprefix("lexindex Dict "),
                "theirs": best_theirs["bytes_per_key"],
                "theirs_label": best_theirs["structure"],
                "margin": (best_theirs["bytes_per_key"] - best_ours["bytes_per_key"])
                / best_theirs["bytes_per_key"]
                * 100,
            }
        )
    return sorted(out, key=lambda r: -r["margin"])


def draw(data: list[dict], env: dict, out: Path) -> None:
    fig, ax = plt.subplots(figsize=(10.5, 6.4))
    y = range(len(data))
    h = 0.38
    widest = max(max(r["ours"], r["theirs"]) for r in data)

    for i, r in enumerate(data):
        won = r["margin"] > 0
        ax.barh(i + h / 2, r["theirs"], height=h, color=RIVAL, zorder=2)
        ax.barh(i - h / 2, r["ours"], height=h, color=LEX if won else LOSS, zorder=2)
        ax.text(
            r["theirs"] + widest * 0.008,
            i + h / 2,
            f"{r['theirs']:.2f}  {r['theirs_label']}",
            va="center",
            fontsize=7.5,
            color="#475569",
        )
        ax.text(
            r["ours"] + widest * 0.008,
            i - h / 2,
            f"{r['ours']:.2f}  Dict {r['ours_label']}",
            va="center",
            fontsize=7.5,
            color=LEX if won else LOSS,
            fontweight="bold",
        )
        ax.text(
            widest * 1.30,
            i,
            f"{r['margin']:+.1f} %",
            va="center",
            ha="right",
            fontsize=8.5,
            color=LEX if won else LOSS,
            fontweight="bold",
        )

    ax.set_yticks(list(y))
    ax.set_yticklabels([f"{r['corpus']}\n{r['keys'] / 1e6:.2f} M keys" for r in data], fontsize=8)
    ax.invert_yaxis()
    ax.set_xlim(0, widest * 1.32)
    ax.set_xlabel("serialised bytes per key — smaller is better", fontsize=9)
    ax.grid(axis="x", ls=":", alpha=0.35, zorder=0)
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.tick_params(axis="y", length=0)

    won = sum(1 for r in data if r["margin"] > 0)
    ax.set_title(
        f"lexindex `DictIndex` against the smallest trie anyone else built — "
        f"smaller on {won} of {len(data)} corpora",
        fontsize=12,
        fontweight="bold",
        loc="left",
        pad=26,
    )
    ax.text(
        0,
        1.015,
        "MARISA · XCDAT · CoCo-trie · PDT · C²-MARISA · C²-CoCo · C²-FST · FST, each at its own "
        "best configuration.\n"
        f"One process a structure and corpus, median of {env.get('rounds', '3').split(',')[0]} "
        f"rounds, {env.get('cpu', 'unknown CPU')}. "
        f"Artifact {env.get('table', '?')} at {env.get('commit', '?')}, "
        f"{env.get('date', '?')[:10]}.",
        transform=ax.transAxes,
        fontsize=7.5,
        color="#64748b",
        va="bottom",
    )
    handles = [
        plt.Rectangle((0, 0), 1, 1, color=LEX),
        plt.Rectangle((0, 0), 1, 1, color=RIVAL),
        plt.Rectangle((0, 0), 1, 1, color=LOSS),
    ]
    # Below the axis, not inside it: at the bottom right the legend sits exactly where the one
    # losing corpus prints its margin.
    ax.legend(
        handles,
        ["lexindex DictIndex", "smallest other structure", "the one corpus a trie still wins"],
        loc="upper center",
        bbox_to_anchor=(0.5, -0.09),
        ncol=3,
        frameon=False,
        fontsize=8,
    )
    fig.tight_layout()
    fig.savefig(out, format="svg", bbox_inches="tight")
    plt.close(fig)


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__.strip().splitlines()[2].strip())
    artifact = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    env = artifact["environment"]
    data = rows(artifact)
    out = Path("docs/assets") / f"frontier-{env['table'].removeprefix('frontier-')}.svg"
    out.parent.mkdir(parents=True, exist_ok=True)
    draw(data, env, out)
    won = sum(1 for r in data if r["margin"] > 0)
    print(
        f"{out}: {won}/{len(data)} corpora, "
        f"margins {data[0]['margin']:+.1f} % to {data[-1]['margin']:+.1f} %"
    )


if __name__ == "__main__":
    # Text as paths: the figure renders the same on GitHub, on PyPI and in a PDF, on a machine that
    # has none of the fonts this one does.
    matplotlib.rcParams["svg.fonttype"] = "path"
    main()
