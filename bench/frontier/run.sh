#!/usr/bin/env bash
# The research-frontier campaign: lexindex, the nine structures of the C² benchmark and XCDAT's four
# tries on the corpus set, one process per structure and corpus, into bench/results/.
#
#   bench/frontier/run.sh                   # the thirteen corpora at 1 M keys, or all a corpus has
#   bench/frontier/run.sh --scale 10m       # the six corpora with a ten-million-key file
#   bench/frontier/run.sh urls-1000000      # named corpus files only
#   bench/frontier/run.sh --allow-dirty     # for development; the artifact is tagged <sha>-dirty
#
# One protocol for every row, C²'s `benchmark.cpp`: read the file, sort and deduplicate, build once,
# report the structure's own size, then look every key up once in one fixed shuffle -- a single timed
# pass, no warm-up -- and report the mean. `frontier_lex` and `xcdat_frontier` do the same for
# lexindex and XCDAT. Build everything first with bench/frontier/build.sh.
set -euo pipefail

LOAD_CEILING=1.0
# How long one corpus may wait for the load average to fall under the ceiling before the run stops.
SETTLE_SECONDS=1800
# A build that runs away fails instead of swapping the machine.
ADDRESS_SPACE_KB=28000000

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
# shellcheck source=bench/frontier/pins.sh
. bench/frontier/pins.sh
frontier=${LEXINDEX_FRONTIER:-$root/local/frontier}
corpora=${LEXINDEX_CORPORA:-$root/local/corpora}
c2=$frontier/c2/build/benchmark
xcdat=$frontier/xcdat_frontier
lex=$root/bench/frontier/frontier_lex/target/release/frontier_lex

allow_dirty=0
scale=1m
named=()
while [ $# -gt 0 ]; do
  case "$1" in
    --allow-dirty) allow_dirty=1 ;;
    --scale)
      scale=${2:-}
      shift
      ;;
    -h | --help)
      sed -n '2,13p' "$0" | cut -c3-
      exit 0
      ;;
    -*)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
    *) named+=("$1") ;;
  esac
  shift
done

case "$scale" in
  1m)
    stems=(words-full dna-1000000 domains-1000000 idents-1000000 numeric-1000000 opaque-1000000
      paths-1000000 pypi-full titles-en-1000000 titles-ru-1000000 titles-zh-1000000 urls-1000000
      uuid-1000000)
    limit=1800
    ;;
  10m)
    stems=(dna-10000000 numeric-10000000 opaque-10000000 titles-en-10000000 urls-10000000
      uuid-10000000)
    limit=7200
    ;;
  *)
    echo "--scale takes 1m or 10m, not '$scale'" >&2
    exit 2
    ;;
esac
table=frontier-$scale
if [ ${#named[@]} -gt 0 ]; then
  stems=("${named[@]}")
  table="frontier-named"
fi

commit=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  if [ "$allow_dirty" -eq 0 ]; then
    echo "refusing: the working tree has uncommitted changes, so the result would not be" >&2
    echo "attributable to the commit it is named after. Commit, stash, or --allow-dirty." >&2
    exit 1
  fi
  commit=$commit-dirty
fi

load=$(cut -d' ' -f1 /proc/loadavg)
if awk -v l="$load" -v c="$LOAD_CEILING" 'BEGIN { exit !(l > c) }'; then
  echo "refusing: load average $load is above $LOAD_CEILING -- a benchmark on a busy machine" >&2
  echo "measures the machine. Wait for it to settle." >&2
  exit 1
fi

if [ ! -x /usr/bin/time ]; then
  echo "refusing: /usr/bin/time (GNU time) is needed for each process's peak memory" >&2
  exit 1
fi
for binary in "$c2" "$xcdat"; do
  if [ ! -x "$binary" ]; then
    echo "refusing: $binary is missing; run bench/frontier/build.sh" >&2
    exit 1
  fi
done
for pin in "${PINS[@]}"; do
  read -r path _ pinned <<< "$pin"
  at=$(git -C "$frontier/$path" rev-parse HEAD 2> /dev/null || echo none)
  if [ "$at" != "$pinned" ]; then
    echo "refusing: $path is at $at, not the pinned $pinned; run bench/frontier/build.sh" >&2
    exit 1
  fi
done
for stem in "${stems[@]}"; do
  if [ ! -r "$corpora/$stem.txt" ]; then
    echo "refusing: no corpus file $corpora/$stem.txt; see bench/corpora.py" >&2
    exit 1
  fi
done
uv run --no-sync python bench/corpora.py verify
# The tree is clean, so this is the commit the artifact is named after.
cargo build --release --locked --quiet --manifest-path bench/frontier/frontier_lex/Cargo.toml

host=$(uname -n)
log=bench/results/$table-$(date +%F)-$host-$commit.log

# C²'s fourth argument is the depth of the recursion its paper ablates; for the MARISA baseline it is
# the number of tries less one, so depth 0 is a one-level MARISA and depth 2 marisa's 3-try default.
c2_runs=("0 0" "0 1" "0 2" "1 0" "1 1" "1 2" "2 0" "2 1" "2 2" "3 0" "4 0" "5 0" "5 1" "5 2"
  "6 0" "7 0" "8 0")

header() {
  local l2 l3 changed
  l2=$(getconf LEVEL2_CACHE_SIZE 2> /dev/null || true)
  l3=$(getconf LEVEL3_CACHE_SIZE 2> /dev/null || true)
  echo "frontier campaign"
  echo "date: $(date -Is)"
  echo "host: $host"
  echo "commit: $commit"
  echo "table: $table"
  echo "lexindex: $(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
  echo "kernel: $(uname -srm)"
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs)"
  echo "cores/threads: $(grep -m1 'cpu cores' /proc/cpuinfo | cut -d: -f2 | xargs) / $(nproc --all)"
  echo "caches: L2 $((${l2:-0} / 1024)) KiB, L3 $((${l3:-0} / 1024)) KiB"
  if [ -r /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]; then
    echo "governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
  fi
  if [ -r /sys/kernel/mm/transparent_hugepage/enabled ]; then
    echo "transparent hugepages: $(cat /sys/kernel/mm/transparent_hugepage/enabled)"
  fi
  echo "compiler: $(c++ --version | head -1)"
  echo "rustc: $(rustc --version)"
  echo "load at start: $(cut -d' ' -f1-3 /proc/loadavg)"
  echo "address space cap: $ADDRESS_SPACE_KB KB"
  echo "timeout: $limit s a process"
  for pin in "${PINS[@]}"; do
    read -r path url pinned <<< "$pin"
    changed=$(git -C "$frontier/$path" status --porcelain --untracked-files=no | awk '{ print $2 }' |
      paste -sd, -)
    echo "pin $path: ${pinned:0:12} $url${changed:+ (modified: $changed)}"
  done
  for path in c2/lib/ds2i/succinct c2/lib/sdsl-lite/external/googletest \
    c2/lib/sdsl-lite/external/libdivsufsort; do
    echo "pin $path: $(git -C "$frontier/$path" rev-parse HEAD | cut -c1-12) (gitlink)"
  done
  echo "pin coco adapted headers: ${COCO_COMMIT:0:12} https://github.com/aboffa/CoCo-trie (by sha256)"
}

settle() {
  local waited=0
  until awk -v c="$LOAD_CEILING" '{ exit !($1 < c) }' /proc/loadavg; do
    if [ "$waited" -ge "$SETTLE_SECONDS" ]; then
      echo "refusing: the load average stayed above $LOAD_CEILING for $SETTLE_SECONDS s" >&2
      exit 1
    fi
    sleep 15
    waited=$((waited + 15))
  done
}

# One process: its own output, its wall time and peak memory from GNU time, and its exit status.
measure() {
  local label=$1 status=0
  shift
  echo "--- $label   load $(cut -d' ' -f1-3 /proc/loadavg)"
  /usr/bin/time -f '[time %e s, maxrss %M KB]' timeout "$limit" "$@" 2>&1 || status=$?
  echo "[exit $status]"
}

campaign() {
  local file keys
  header
  ulimit -v "$ADDRESS_SPACE_KB"
  for stem in "${stems[@]}"; do
    file=$corpora/$stem.txt
    settle
    keys=$(wc -l < "$file")
    printf '\n##### %s (%s lines, %s bytes)   load %s   %s\n' "$stem" "$keys" \
      "$(stat -c %s "$file")" "$(cut -d' ' -f1-3 /proc/loadavg)" "$(date -Is)"
    for kind in dict32 dict256 dict1024 string; do
      measure "lexindex $kind" "$lex" "$file" "$kind"
    done
    for run in "${c2_runs[@]}"; do
      read -r which depth <<< "$run"
      measure "c2 case $which rec $depth" "$c2" "$file" "$which" 0 "$depth" 0
    done
    for type in 7 8 15 16; do
      measure "xcdat $type" "$xcdat" "$file" "$type"
    done
  done
  echo
  echo "campaign done $(date -Is), load $(cut -d' ' -f1-3 /proc/loadavg)"
}

campaign 2>&1 | tee "$log"
uv run --no-sync python bench/frontier/tables.py --json "$log"
git status --porcelain --untracked-files=normal -- bench/results
