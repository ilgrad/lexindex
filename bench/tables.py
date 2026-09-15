"""Render the published headline tables from the artifact they cite, and check they still match.

A generated table lives between two markers and is not written by hand:

    <!-- table: compare bench/results/compare-2026-09-14-arz-8aaa0af.json columns=prefix,range -->
    | library | prefix | range | **bytes/key** | **ns/lookup** |
    ...
    <!-- /table -->

`--check` re-renders every marked block and reports the ones that differ; CI runs it, so a cell
edited by hand fails the build. `--write` rewrites them in place. The check also reads the caption
that follows a block, up to the next block: a table may only be captioned with the artifact it was
rendered from, which is the bug this script exists to make unrepeatable -- the 2.1.0 and 3.0.0
tables cited one commit while some of their cells came from another.

    uv run --no-sync python bench/tables.py --check
    uv run --no-sync python bench/tables.py --write

Two kinds of table. A `compare` table is `bench/compare.py`'s: its numbers come from the artifact;
three things cannot, and are declared here rather than inferred: the display label of a row, the
wording of a membership cell that is neither yes nor no, and whether a library answers
common-prefix queries -- a capability `bench/compare.py` does not measure. A `frontier` table is a
research-frontier campaign's overview, or with `corpus=` one corpus's table, rendered by
`bench/frontier/tables.py` itself, so the docs cannot drift from what the campaign printed.
"""

import argparse
import importlib.util
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = ("README.md", "docs/benchmarks.md")

OPEN = re.compile(r"^<!-- table: (?P<kind>[\w-]+) (?P<artifact>\S+)(?P<opts>[^>]*)-->$", re.M)
CLOSE = "<!-- /table -->"
# How far past a block the caption is looked for; a citation further away than this is prose.
CAPTION_LINES = 20
ARTIFACT_LINK = re.compile(r"bench/results/((?:compare|frontier)-[\w.-]+\.json)")

# Loaded by path: its module name, `tables`, is this file's own.
_spec = importlib.util.spec_from_file_location("frontier_tables", ROOT / "bench/frontier/tables.py")
FRONTIER = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(FRONTIER)

# The artifact's `library` string -> how the published tables spell it. A row the map does not
# know is an error: a new competitor has to be named here before it can be published.
LABELS = {
    "lexindex ClosedHashIndex": "**lexindex `ClosedHashIndex`**",
    "lexindex CompactHashIndex (fp=4 bits)": "**lexindex `CompactHashIndex` (fp=4 bits)**",
    "lexindex CompactHashIndex (fp=1)": "**lexindex `CompactHashIndex` (fp=1)**",
    "lexindex CompactHashIndex (fp=2)": "**lexindex `CompactHashIndex` (fp=2)**",
    "lexindex DictIndex (512 per block)": "**lexindex `DictIndex` (512 per block)**",
    "lexindex DictIndex (256 per block, default)": (
        "**lexindex `DictIndex` (256 per block, default)**"
    ),
    "marisa-trie (4 tries, tiny cache)": "`marisa-trie` (4 tries, tiny cache — its smallest here)",
    "marisa-trie (default)": "`marisa-trie` (default)",
    "marisa-trie (huge cache)": "`marisa-trie` (huge cache)",
    "lexindex StringIndex": "**lexindex `StringIndex`**",
    "lexindex PerfectHashIndex": "lexindex `PerfectHashIndex`",
    "DAWG (dawg2)": "DAWG (`dawg2`)",
    "datrie": "`datrie`",
}
# Membership is three-valued in the published table and one bit in the artifact.
MEMBERSHIP = {
    "lexindex ClosedHashIndex": "none (closed vocabulary)",
    "lexindex CompactHashIndex (fp=4 bits)": "probabilistic",
    "lexindex CompactHashIndex (fp=1)": "probabilistic",
    "lexindex CompactHashIndex (fp=2)": "probabilistic",
}
# Not measured by `bench/compare.py`: which libraries answer "the keys that are prefixes of this
# query" -- `common_prefix` here, `common_prefix_keys` in marisa, `prefixes` in dawg2 and datrie.
COMMON_PREFIX = {
    "lexindex StringIndex",
    "lexindex DictIndex (512 per block)",
    "lexindex DictIndex (256 per block, default)",
    "marisa-trie (4 tries, tiny cache)",
    "marisa-trie (default)",
    "marisa-trie (huge cache)",
    "DAWG (dawg2)",
    "datrie",
}
COLUMNS = {
    "prefix": ("prefix", lambda c, lib: c["prefix"]),
    "common_prefix": ("common prefix", lambda c, lib: lib in COMMON_PREFIX),
    "range": ("range", lambda c, lib: c["rangeq"]),
    "fuzzy": ("fuzzy", lambda c, lib: c["fuzzy"]),
    "reverse": ("reverse id→str", lambda c, lib: c["reverse"]),
    "exact": ("exact membership", lambda c, lib: c["exact"]),
    "mmap": ("zero-copy mmap", lambda c, lib: c["mmap"]),
}
YES, NO = "✅", "—"


def _cell(name: str, caps: dict, library: str) -> str:
    if name == "exact" and library in MEMBERSHIP:
        return MEMBERSHIP[library]
    return YES if COLUMNS[name][1](caps, library) else NO


def render_compare(artifact: dict, opts: dict[str, str]) -> str:
    """The size-and-capability table, sorted by bytes a key, with the builtin `dict` last."""
    columns = opts["columns"].split(",") if opts.get("columns") else []
    unknown = [c for c in columns if c not in COLUMNS]
    if unknown:
        raise ValueError(f"{unknown} are not columns")
    rows = sorted(artifact["cells"], key=lambda c: c["bytes_per_key"])
    unknown = [r["library"] for r in rows if r["library"] not in LABELS]
    if unknown:
        raise SystemExit(f"bench/tables.py: no published label for {unknown} — add it to LABELS")
    fastest = min(r["lookup_ns"]["min"] for r in rows)
    # Bold marks where lexindex leads on the size axis: smaller than the smallest thing it is
    # measured against. `StringIndex` and `PerfectHashIndex` buy their bytes back in capability.
    rivals = (r for r in rows if not r["library"].startswith("lexindex"))
    smallest_rival = min(r["bytes_per_key"] for r in rivals)
    head = ["library", *(COLUMNS[c][0] for c in columns), "**bytes/key**", "**ns/lookup**"]
    align = ["---", *(":---:" for _ in columns), "---:", "---:"]
    lines = ["| " + " | ".join(head) + " |", "|" + "|".join(align) + "|"]
    for row in rows:
        lib = row["library"]
        ns = row["lookup_ns"]["min"]
        bytes_per_key = f"{row['bytes_per_key']:.2f}"
        cells = [
            LABELS[lib],
            *(_cell(c, row["capabilities"], lib) for c in columns),
            f"**{bytes_per_key}**"
            if lib.startswith("lexindex") and row["bytes_per_key"] < smallest_rival
            else bytes_per_key,
            f"**{ns:.0f}**" if ns == fastest else f"{ns:.0f}",
        ]
        lines.append("| " + " | ".join(cells) + " |")
    dict_ns = artifact["builtin_dict_ns"]["min"]
    lines.append(
        "| builtin `dict` | "
        + " | ".join(NO if c != "exact" else YES for c in columns)
        + f" | — (in RAM only) | {dict_ns:.0f} |"
    )
    return "\n".join(lines)


def render_frontier(artifact: dict, opts: dict[str, str]) -> str:
    """A frontier campaign's overview, or with `corpus=` that corpus's table."""
    corpora = FRONTIER.corpora_of(artifact)
    if "corpus" not in opts:
        return FRONTIER.overview(corpora)
    for corpus in corpora:
        if corpus["corpus"] == opts["corpus"]:
            return FRONTIER.corpus_table(corpus)
    raise ValueError(f"the artifact has no corpus `{opts['corpus']}`")


RENDERERS = {"compare": render_compare, "frontier": render_frontier}


def blocks(text: str):
    """Every generated block: the opening marker's fields and the span its body occupies."""
    for m in OPEN.finditer(text):
        body_at = m.end() + 1
        end = text.find(CLOSE, body_at)
        if end == -1:
            raise SystemExit(f"bench/tables.py: `{m.group(0)}` is never closed")
        opts = dict(kv.split("=", 1) for kv in m.group("opts").split() if "=" in kv)
        yield m, body_at, end, opts


def caption_cites(text: str, after: int, artifact: str) -> str | None:
    """What the prose under a block cites, up to the next block, if it names an artifact at all."""
    tail = []
    for line in text[after:].splitlines()[:CAPTION_LINES]:
        # The next block's marker names its own artifact: a caption ends where that block starts.
        if OPEN.match(line):
            break
        tail.append(line)
    named = set(ARTIFACT_LINK.findall("\n".join(tail)))
    if not named:
        return "no artifact cited under the table"
    if named != {artifact}:
        return f"the caption cites {sorted(named)}, the table was rendered from {artifact}"
    return None


def process(write: bool) -> int:
    bad = 0
    for name in FILES:
        path = ROOT / name
        text = path.read_text(encoding="utf-8")
        out, moved = text, 0
        for m, body_at, end, opts in blocks(text):
            kind, artifact = m.group("kind"), m.group("artifact")
            if kind not in RENDERERS:
                raise SystemExit(f"{name}: no renderer for a `{kind}` table")
            data = json.loads((ROOT / artifact).read_text(encoding="utf-8"))
            try:
                want = RENDERERS[kind](data, opts) + "\n"
            except ValueError as e:
                raise SystemExit(f"{name}: {e}") from e
            have = text[body_at:end]
            why = caption_cites(text, end, Path(artifact).name)
            if why:
                print(f"{name}: {why}")
                bad += 1
            if have == want:
                continue
            bad += 1
            if write:
                out = out[: body_at + moved] + want + out[end + moved :]
                moved += len(want) - len(have)
                print(f"{name}: rewrote the {kind} table from {artifact}")
            else:
                print(f"{name}: the {kind} table does not match {artifact}")
                for line in _diff(have, want):
                    print(f"  {line}")
        if write and out != text:
            path.write_text(out, encoding="utf-8")
    if bad and not write:
        print("\nRun `uv run --no-sync python bench/tables.py --write` after a new measurement.")
    return 1 if bad and not write else 0


def _diff(have: str, want: str) -> list[str]:
    import difflib

    return [
        line.rstrip()
        for line in difflib.unified_diff(
            have.splitlines(), want.splitlines(), "published", "rendered", n=0, lineterm=""
        )
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="fail if a table is out of date")
    mode.add_argument("--write", action="store_true", help="re-render every table in place")
    args = parser.parse_args()
    return process(write=args.write)


if __name__ == "__main__":
    sys.exit(main())
