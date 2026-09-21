"""The front-page figure: lexindex against the smallest trie, on every corpus of the set.

    uv run --with matplotlib python bench/frontier_chart.py \
        bench/results/frontier-1m-<date>-<host>-<commit>.json

Reads one campaign artifact written by `bench/frontier/tables.py` and draws the comparison the
benchmarks page makes in prose: for each corpus, the smallest index this crate builds against the
smallest structure anyone else in the campaign built. Writes an SVG beside the docs, named after
the artifact's own commit, so a figure in the README can always be traced to the run behind it.

Three choices the figure makes, each of which could be made dishonestly:

* **Both sides at their best, which is the only symmetry available.** The other side is the best of
  eight structures, each at the configuration that suits that corpus -- MARISA at its `num_tries`,
  XCDAT at its best of four, CoCo and PDT as the C² benchmark builds them. Holding this side to one
  index while the other picks from eight is not modesty, it is a different measurement. So the bar
  is whichever index lexindex builds smallest, and the label says which one it is: `DictIndex` at
  its block on twelve corpora, `StringIndex` on `numeric`, where an fst folds a dense decimal id
  space into 301 bytes and the label prints that rather than a rounded 0.00.
* **The pick is the planner's, not the author's.** `lexindex plan` chooses the index off the keys
  alone, and choosing after seeing the answer would be the mirage this repository exists to refuse.
  Scored against the built blob on all 19 corpora of this set, its ranking of the two candidates a
  memory objective picks between is right on every one -- so the bar is what a caller gets without
  reading this figure.
* **A linear axis.** The corpora span a fraction of a byte to 21 bytes a key and a log axis would
  flatter the ratios; the numbers are printed on the bars instead.

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
    """One row a corpus: the smallest index this crate builds against the smallest structure
    anyone else built."""
    by_corpus: dict[str, list[dict]] = {}
    for cell in artifact["cells"]:
        if cell.get("bytes_per_key") is None or cell["structure"] in REFERENCE:
            continue
        by_corpus.setdefault(cell["corpus"], []).append(cell)

    out = []
    for corpus, cells in by_corpus.items():
        ours = [c for c in cells if c["structure"].startswith("lexindex")]
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
                "ours_label": best_ours["structure"].removeprefix("lexindex "),
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
        # Two decimals round an fst on a dense id space to 0.00, which reads as a missing bar
        # rather than the point of the row; under a hundredth of a byte the label goes to bytes.
        ours = f"{r['ours']:.2f}" if r["ours"] >= 0.01 else f"{r['ours'] * r['keys']:,.0f} B total"
        ax.text(
            r["ours"] + widest * 0.008,
            i - h / 2,
            f"{ours}  {r['ours_label']}",
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
        f"lexindex against the smallest trie anyone else built — "
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
    labels = ["lexindex, best index", "smallest other structure"]
    colours = [LEX, RIVAL]
    # The loss colour earns a key only when something is losing; carried unconditionally it
    # advertises a corpus the figure does not show.
    if won < len(data):
        labels.append("a corpus a trie still wins")
        colours.append(LOSS)
    # Below the axis, not inside it: at the bottom right the legend sits exactly where the last
    # row prints its margin.
    ax.legend(
        [plt.Rectangle((0, 0), 1, 1, color=c) for c in colours],
        labels,
        loc="upper center",
        bbox_to_anchor=(0.5, -0.09),
        ncol=len(labels),
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
