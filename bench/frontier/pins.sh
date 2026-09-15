# shellcheck shell=bash disable=SC2034
# Every competitor the frontier harness builds, at the commit its published numbers were measured
# on. Sourced by build.sh and run.sh, so a pin moves in one place and a run refuses a stale build.
#
# C²'s repository names its dependencies in `.gitmodules` but records no gitlinks for them, so these
# are the commits that build together here (GCC 16.2, CMake 4.3, Boost 1.90), not commits C² chose.
# The submodules nested inside them -- ds2i's `succinct`, sdsl-lite's `googletest` and
# `libdivsufsort` -- are pinned by their parents' gitlinks.

# path under the build directory, repository, commit
PINS=(
  "c2 https://github.com/alexztc/C2 92c1b861456b2e5cb4085d317fc9ebb7c8e3d88e"
  "c2/lib/sdsl-lite https://github.com/vgteam/sdsl-lite 4312ea9c9370565375d884bd48374b22de52e985"
  "c2/lib/ds2i https://github.com/aboffa/ds2i 4f46606cc07756bc5d919fc68fd7d567b901fb16"
  "c2/lib/sux https://github.com/vigna/sux 5944f6b5c1c7911afe63b01534bec90b4bafafcd"
  "c2/lib/fsst https://github.com/cwida/fsst e638d4cf8c26129d73c242a4127b42b975de5b63"
  "c2/baseline_marisa/marisa https://github.com/s-yata/marisa-trie e54f296bb52d16693931c8b963744931ef1e37f7"
  "c2/baseline_ctriepp/ctriepp https://gitlab.com/ofek.gila1/ctriepp 52d2c16a7bb60ea72f0e1d18270b42b9dcb696cc"
  "xcdat https://github.com/kampersanda/xcdat 2451441dcc09e470dcb3ecd2c1bb304a7dc46347"
)

# CoCo-trie's `lib/adapted_code`: five headers its own build copies over ds2i, succinct and sux (a
# templated `compact_elias_fano` among them), without which C²'s CoCo structures do not compile.
# Fetched from that commit and checked by content, rather than cloning the repository for them.
COCO_COMMIT=ee860d4b1e62075b8c80e992e9c32de94ee4caa6
# header, destination directory under the build directory, sha256
ADAPTED=(
  "bit_vector.hpp c2/lib/ds2i/succinct ca9cec4e7c2c042b2111adbe320289a3d8fdf97e4c80355e7824d51114de58ff"
  "broadword.hpp c2/lib/ds2i/succinct bded85b52032087108b11e0fe992c7cbabcfd80252947f21b17e0c225f891d92"
  "intrinsics.hpp c2/lib/ds2i/succinct b3a0b234eb91ab3ad7a27da465fb948c1f521de1076ceb03f6d6454ab9632041"
  "compact_elias_fano.hpp c2/lib/ds2i 03faf154638eeedf86c2114d25885dc2fcb0fd8a2e2f88dea5dbca8814f34947"
  "Vector.hpp c2/lib/sux/sux/util 276e9a678bb07c549464131a2b6aee974604f01f5b4684f0401225870491bfe6"
)
