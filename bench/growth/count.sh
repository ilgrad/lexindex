#!/usr/bin/env bash
# count.sh <op> <keys> <n> -> "<op> <keys> <n> <instructions>": user-space instructions retired
# inside the one operation, the program pinned to one CPU ($CPU, default 3).
set -euo pipefail
op=$1 keys=$2 n=$3
here=$(cd "$(dirname "$0")" && pwd)
bin=${GROWTH_BIN:-${CARGO_TARGET_DIR:-$here/target}/release/growth}
dir=${GROWTH_DATA:-/tmp/lexindex-growth}/blobs
mkdir -p "$dir"
fifo=$(mktemp -d)
mkfifo "$fifo/ctl" "$fifo/ack"
exec 10<>"$fifo/ctl" 11<>"$fifo/ack"
out=$(taskset -c "${CPU:-3}" perf stat -D -1 --control fd:10,11 -x, -e instructions:u -- \
  "$bin" "$op" "$keys" "$n" "$dir" 2>&1 >/dev/null | grep instructions)
exec 10>&- 11>&-
rm -rf "${fifo:?}"
echo "$op $(basename "$keys" .txt) $n ${out%%,*}"
