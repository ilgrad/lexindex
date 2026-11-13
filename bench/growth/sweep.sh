#!/usr/bin/env bash
# sweep.sh > results.txt: every build and load at n = 2^16 .. min(2^top, the corpus) on five of
# the corpora bench/corpora.py writes ($LEXINDEX_CORPORA, default local/corpora). Lines are
# "<build|load> <op> <corpus>-<e> 0 <instructions> <n>"; analyse.py turns them into the table.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
corpora=${LEXINDEX_CORPORA:-$here/../../local/corpora}
data=${GROWTH_DATA:-/tmp/lexindex-growth}
export GROWTH_DATA=$data
bin=${GROWTH_BIN:-${CARGO_TARGET_DIR:-$here/target}/release/growth}
mkdir -p "$data/keys" "$data/blobs"
builds="string dict32 dict256 dict1024 da ph phblocked compact compactfile closed hashed overlay plan"
loads="string dict32 dict256 dict1024 da ph phblocked compact closed hashed"
for spec in ${GROWTH_CORPORA:-words-full:18 titles-zh-full:21 paths-full:22 dna-10000000:23 urls-full:24}; do
  c=${spec%%:*} top=${spec##*:}
  for e in $(seq 16 "$top"); do
    n=$((1 << e)) keys=$data/keys/$c-$e.txt
    timeout 900 "$bin" draw "$corpora/$c.txt" "$n" "$keys" || { echo "draw failed $c $e" >&2; continue; }
    for op in ${GROWTH_BUILDS:-$builds}; do
      # The double array refuses past 2^23 slots, reached on these corpora above 2^21 keys.
      [ "$op" = da ] && [ "$e" -gt 21 ] && continue
      r=$(timeout 1800 "$here/count.sh" "$op" "$keys" 0) || r="$op $c-$e 0 FAILED"
      echo "build $r $n"
    done
    for k in ${GROWTH_LOADS:-$loads}; do
      [ "$k" = da ] && [ "$e" -gt 21 ] && continue
      if ! taskset -c "${CPU:-3}" timeout 1800 "$bin" "save-$k" "$keys" 0 "$data/blobs" > /dev/null 2>&1; then
        echo "load load-$k $c-$e 0 FAILED $n"
        continue
      fi
      r=$(timeout 600 "$here/count.sh" "load-$k" "$keys" 0) || r="load-$k $c-$e 0 FAILED"
      echo "load $r $n"
      rm -f "$data/blobs/$k.blob"
    done
    rm -f "$data/blobs/"*.blob "$keys"
  done
done
