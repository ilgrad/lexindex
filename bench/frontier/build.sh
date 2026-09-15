#!/usr/bin/env bash
# Build the research-frontier harness: the C² benchmark (C²-FST, C²-CoCo and C²-MARISA beside FST,
# CoCo-trie, MARISA, PDT, ART and C-ART), XCDAT under the same protocol, and `frontier_lex` for
# lexindex, every competitor at the commit in bench/frontier/pins.sh.
#
#   bench/frontier/build.sh    # into local/frontier, or $LEXINDEX_FRONTIER
#
# Nothing is vendored into this repository or linked into lexindex: the competitors are fetched at
# build time into a gitignored directory, because several carry licences this repository cannot --
# CoCo-trie is GPLv3 and PDT is for non-commercial use only. Needs git, curl, CMake, a C++20
# compiler, Boost (unit_test_framework, iostreams, system, filesystem) and cargo; measured with
# GCC 16.2, CMake 4.3 and Boost 1.90.
set -euo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
# shellcheck source=bench/frontier/pins.sh
. "$root/bench/frontier/pins.sh"
frontier=${LEXINDEX_FRONTIER:-$root/local/frontier}
jobs=${JOBS:-$(nproc --all)}

for tool in git curl cmake c++ ar sha256sum cargo; do
  if ! command -v "$tool" > /dev/null; then
    echo "refusing: $tool is not on PATH" >&2
    exit 1
  fi
done

step() { printf '\n=== %s\n' "$1"; }

# A shallow fetch of exactly the pinned commit, or a full one where a server will not serve a commit
# by id; either way the checkout is compared with the pin afterwards rather than trusted.
fetch() {
  local dir=$1 url=$2 commit=$3
  if [ ! -d "$dir/.git" ]; then
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" remote add origin "$url"
  fi
  if [ "$(git -C "$dir" rev-parse -q --verify HEAD || true)" != "$commit" ]; then
    git -C "$dir" fetch -q --depth 1 origin "$commit" || git -C "$dir" fetch -q origin
    git -C "$dir" checkout -q --force --detach "$commit"
  fi
  if [ "$(git -C "$dir" rev-parse HEAD)" != "$commit" ]; then
    echo "refusing: $dir is at $(git -C "$dir" rev-parse HEAD), not the pinned $commit" >&2
    exit 1
  fi
  printf '%-40s %s  %s\n' "${dir#"$frontier"/}" "${commit:0:12}" "$url"
}

# A step's output goes to its own log; where it fails, the tail says why.
logged() {
  local log=$1
  shift
  if ! "$@" > "$log" 2>&1; then
    tail -n 30 "$log" >&2
    echo "failed: $* (whole log: $log)" >&2
    exit 1
  fi
}

step "competitors"
for pin in "${PINS[@]}"; do
  read -r path url commit <<< "$pin"
  fetch "$frontier/$path" "$url" "$commit"
done
for nested in "c2/lib/ds2i succinct" "c2/lib/sdsl-lite external/googletest" \
  "c2/lib/sdsl-lite external/libdivsufsort"; do
  read -r parent path <<< "$nested"
  git -C "$frontier/$parent" submodule update -q --init --depth 1 -- "$path" ||
    git -C "$frontier/$parent" submodule update -q --init -- "$path"
  printf '%-40s %s  (gitlink)\n' "$parent/$path" \
    "$(git -C "$frontier/$parent/$path" rev-parse HEAD | cut -c1-12)"
done

step "CoCo-trie's adapted headers, ${COCO_COMMIT:0:12}"
fetched=$(mktemp)
trap 'rm -f "$fetched"' EXIT
for entry in "${ADAPTED[@]}"; do
  read -r header dest sum <<< "$entry"
  curl -fsSL -o "$fetched" \
    "https://raw.githubusercontent.com/aboffa/CoCo-trie/$COCO_COMMIT/lib/adapted_code/$header"
  if [ "$(sha256sum < "$fetched" | cut -d' ' -f1)" != "$sum" ]; then
    echo "refusing: CoCo-trie's $header does not match its pinned sha256" >&2
    exit 1
  fi
  # Copied only when it differs: a fresh mtime would recompile everything that includes it.
  cmp -s "$fetched" "$frontier/$dest/$header" || cp "$fetched" "$frontier/$dest/$header"
  echo "$dest/$header"
done

step "MARISA 0.2.6"
# The version C²'s own MARISA headers (baseline_marisa/include) are written against. C²'s
# build_marisa.sh builds it with autotools; its sources compiled directly need a compiler alone, and
# take the flags C² builds everything else with.
marisa=$frontier/c2/baseline_marisa/marisa
rm -rf "$marisa/obj"
mkdir "$marisa/obj"
while read -r source; do
  object=${source#"$marisa/lib/"}
  c++ -O3 -march=native -std=c++17 -fPIC -w -I"$marisa/include" -I"$marisa/lib" \
    -c "$source" -o "$marisa/obj/${object//\//_}.o"
done < <(find "$marisa/lib/marisa" -name '*.cc' | sort)
rm -f "$frontier/c2/baseline_marisa/libmarisa.a"
ar rcs "$frontier/c2/baseline_marisa/libmarisa.a" "$marisa"/obj/*.o
echo "c2/baseline_marisa/libmarisa.a"

step "C² benchmark"
# succinct and libdivsufsort declare `cmake_minimum_required` 2.6 and 2.4.4, which CMake 4 refuses
# to configure without a policy floor.
logged "$frontier/c2-cmake.log" cmake -S "$frontier/c2" -B "$frontier/c2/build" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5
logged "$frontier/c2-build.log" cmake --build "$frontier/c2/build" --target benchmark -j "$jobs"
echo "c2/build/benchmark"

step "XCDAT under the C² protocol"
# Header-only. Compiled with C²'s flags rather than XCDAT's own `-O3`, so that no C++ structure in
# the campaign is built for a lesser machine than another.
logged "$frontier/xcdat-build.log" c++ -O3 -march=native -DNDEBUG -std=c++17 \
  -I"$frontier/xcdat/include" "$root/bench/frontier/xcdat_frontier.cpp" -o "$frontier/xcdat_frontier"
echo "xcdat_frontier"

step "frontier_lex"
cargo build --release --locked --manifest-path "$root/bench/frontier/frontier_lex/Cargo.toml"
echo "bench/frontier/frontier_lex/target/release/frontier_lex"
