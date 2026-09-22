#!/bin/bash -eu
# Every target under `fuzz/`, built as `fuzz.yml`'s `cargo fuzz run` builds it by default --
# optimised, with debug assertions and overflow checks -- under the sanitizer ClusterFuzzLite sets.
# LTO has to stay off, and only the environment outranks a cargo config that turns it on: under LTO
# the library is re-codegen'd from bitcode without the coverage instrumentation, and the fuzzer
# explores nothing while reporting no crashes.
cd "$SRC/lexindex/fuzz"
export CARGO_PROFILE_RELEASE_LTO=false
cargo fuzz build -O --debug-assertions

for source in fuzz_targets/*.rs; do
  target=$(basename "$source" .rs)
  cp "target/x86_64-unknown-linux-gnu/release/$target" "$OUT/"
  # The golden blobs `tests/golden.rs` checks: a header whose magic, checksums and lengths agree is
  # out of reach by mutation alone, so without them a target only ever meets its rejection paths.
  zip -q -j "$OUT/${target}_seed_corpus.zip" "$SRC"/lexindex/tests/data/*
done
