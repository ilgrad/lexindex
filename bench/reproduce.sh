#!/usr/bin/env bash
# Reproduce the tables the README and docs/benchmarks.md cite, from a checkout, in one command.
#
# Every published number names the artifact under bench/results/ that holds its samples and the
# machine that produced them. This script is the other half of that promise: it pins the competitor
# versions, verifies the corpora against their hashes, records the machine, and refuses to run at
# all on a tree whose code does not match the commit the artifact will be named after.
#
#   bench/reproduce.sh                # the comparison table, on /usr/share/dict/words
#   bench/reproduce.sh --sweep        # and every structure over the whole corpus set (long)
#   bench/reproduce.sh --allow-dirty  # for development; the artifact is tagged <sha>-dirty
#
# The Rust-side comparisons (rsmarisa, cold mappings, thread scaling) are separate harnesses under
# local/ and are not part of this script; their artifacts cite the commit they were measured on.
set -euo pipefail

MARISA=1.4.1
DAWG2=0.13.3
DATRIE=0.8.3
LOAD_CEILING=1.0
# The empty Python call over the same probe set, from the artifact the README cites. It is the one
# number in the table no change to this library can move, so a run whose floor is somewhere else is
# a run on a different machine -- whatever `uptime` says about it.
REFERENCE_FLOOR_NS=49
FLOOR_TOLERANCE=1.25

# The probe set is drawn from a set of words, and a set iterates strings in an order that changes
# every process unless this is fixed.
export PYTHONHASHSEED=0

root=$(git rev-parse --show-toplevel)
cd "$root"

allow_dirty=0
sweep=0
words=${LEXINDEX_WORDS:-/usr/share/dict/words}
for arg in "$@"; do
  case "$arg" in
    --allow-dirty) allow_dirty=1 ;;
    --sweep) sweep=1 ;;
    -h | --help)
      sed -n '2,14p' "$0" | cut -c3-
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

step() { printf '\n=== %s\n' "$1"; }

step "the tree"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  if [ "$allow_dirty" -eq 0 ]; then
    echo "refusing: the working tree has uncommitted changes, so the result would not be" >&2
    echo "attributable to the commit it is named after. Commit, stash, or --allow-dirty." >&2
    exit 1
  fi
  echo "WARNING: dirty tree, the artifact will be tagged -dirty and is not citable"
fi
git --no-pager log -1 --format='%h %ad %s' --date=short

step "the machine"
uname -srm
grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs
printf 'cores/threads: %s / %s\n' "$(grep -m1 'cpu cores' /proc/cpuinfo | cut -d: -f2 | xargs)" \
  "$(nproc --all)"
if [ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]; then
  printf 'governor:      %s\n' "$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
fi
uptime
load=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$load" -v c="$LOAD_CEILING" 'BEGIN { exit !(l > c) }'; then
  echo "refusing: load average $load is above $LOAD_CEILING -- a benchmark on a busy machine" >&2
  echo "measures the machine. Wait for it to settle." >&2
  exit 1
fi
rustc --version
uv --version

step "the corpora"
uv run --no-sync python bench/corpora.py verify
if [ "$sweep" -eq 1 ]; then
  uv run --no-sync python bench/corpora.py build
fi
if [ ! -r "$words" ]; then
  echo "refusing: no word list at $words. Install a system dictionary, or set LEXINDEX_WORDS." >&2
  exit 1
fi

step "the wheel"
rm -f target/wheels/lexindex-*.whl
uv run --no-sync --with maturin maturin build --release --out target/wheels
shopt -s nullglob
wheels=(target/wheels/lexindex-*.whl)
shopt -u nullglob
if [ ${#wheels[@]} -ne 1 ]; then
  echo "expected one wheel in target/wheels, found ${#wheels[@]}" >&2
  exit 1
fi
wheel=${wheels[0]}
echo "built $wheel"

run() {
  uv run --no-project --python "$(cat .python-version)" --with "$wheel" --with matplotlib \
    --with "marisa-trie==$MARISA" --with "dawg2==$DAWG2" --with "datrie==$DATRIE" \
    python "$@"
}

step "the comparison table"
run bench/compare.py "$words"

if [ "$sweep" -eq 1 ]; then
  step "every structure over the corpus set"
  run bench/sweep.py
fi

step "is this run comparable?"
uv run --no-sync python - "$REFERENCE_FLOOR_NS" "$FLOOR_TOLERANCE" <<'PY'
import json
import sys
from pathlib import Path

reference, tolerance = float(sys.argv[1]), float(sys.argv[2])
newest = max(Path("bench/results").glob("compare-*.json"), key=lambda p: p.stat().st_mtime)
floor = json.loads(newest.read_text(encoding="utf-8"))["python_call_floor_ns"]["min"]
ratio = floor / reference
print(f"{newest.name}: call floor {floor:.0f} ns against the published {reference:.0f} ns")
if not 1 / tolerance <= ratio <= tolerance:
    print(
        f"WARNING: the floor is {ratio:.2f}x the published one. Every row moves with it, so this\n"
        "         run is internally consistent but not comparable to the tables in the README.",
    )
PY

step "what was written"
git status --porcelain --untracked-files=normal -- bench/results
