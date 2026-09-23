# Contributing

Thanks for your interest in lexindex. It is five immutable `string ↔ id` indexes — a Rust core
(`src/`), PyO3 bindings (`python/lexindex/`) built by [maturin](https://www.maturin.rs/), and a C
ABI (`src/capi.rs`, header `include/lexindex.h`).

Issues are welcome before pull requests, especially for anything that changes a blob format, a
public signature or a published number. Those three are the expensive ones to get wrong.

## Development setup

```bash
python -m venv .venv && . .venv/bin/activate
pip install maturin
maturin develop --release        # builds the extension into the venv
```

Rust 1.85 or newer (the MSRV, which CI pins from `rust-version` in `Cargo.toml`), Python 3.11+.

## The gates

CI runs all of these; these are the ones worth running before you push. The feature matrix is not
decoration — `default = ["mph", "mmap"]`, and a `--no-default-features` build is fst-only, so a
change can compile in one configuration and not in another.

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
cargo clippy --no-default-features --features mph --all-targets -- -D warnings
cargo clippy --features python -- -D warnings
cargo clippy --features capi --all-targets -- -D warnings

cargo test
cargo test --no-default-features
cargo test --release            # the perfect hash's debug_assert!s only fire in debug builds
cargo test --features fuzzing --test golden
cargo test --features capi --test capi

cargo llvm-cov --summary-only --fail-under-lines 95 --features capi   # CI enforces the 95 % floor
cargo semver-checks check-release                                     # against the published crate
```

Python, against the built extension:

```bash
ruff check python/ tests/test_python.py
ruff format --check python/ tests/test_python.py
LEXINDEX_REQUIRE_NUMPY=1 pytest tests/test_python.py -q
python -m mypy.stubtest lexindex     # the .pyi stubs must match the runtime
```

If you touched `src/capi.rs`, the committed header is generated and CI diffs it:

```bash
cbindgen --config cbindgen.toml src/capi.rs | diff -u include/lexindex.h -
```

If you touched `polars/`, it is a crate and a wheel of its own — `lexindex-polars`, the Polars
expression plugin — with its own gate. The tests need both wheels and a GIL-enabled interpreter,
because the extension is `abi3`:

```bash
cd polars
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings
maturin build --release --out dist                      # the plugin
(cd .. && maturin build --release --out polars/dist)    # lexindex itself, for the tests to build blobs
uv run --python 3.13 --with polars --with pytest --with dist/lexindex-*.whl \
  --with dist/lexindex_polars-*.whl pytest tests/test_polars.py -q
ruff check . && ruff format --check .
```

## Guidelines

- **Blob formats are frozen.** `BIX4`, `BDX3`, `BDX4`, `BHD1`, `MPH3`, `BMP8`, `BCH8`, `BCL2` and `OVL2`
  each have golden blobs in `tests/data`, refusal tests for the formats they replaced, and a
  migration page.
  A version that refuses a blob an earlier version wrote is a **major** release — see *Blob
  compatibility* in [`docs/design.md`](docs/design.md). A change that saves a tenth of a per cent is
  not a reason to move one.
- **Honest benchmarks, or none.** Every published number names the artifact under `bench/results/`
  that holds its samples and the commit it was measured at; `bench/reproduce.sh` refuses a dirty
  tree and a busy machine. Three rules that this repository learned the expensive way:
  - Measure on **real keys**. Sequential `entity-{i}` keys collapse an FST to a near-regular
    automaton and report a fictional ~0 bytes a key.
  - **Probe in a shuffled order, never at a stride.** A `step_by(37)` probe set is still a constant
    stride, the L2 prefetcher learns it, and it has reversed a ranking here.
  - **Sizes are deterministic; latency is not.** A size can be measured on a busy machine. A
    nanosecond cannot, and a cross-session latency comparison means nothing without both builds in
    one process.
- **Validate untrusted input once, at the boundary.** Every loader derives its section lengths from
  header scalars rather than trusting written lengths, so a crafted blob answers wrong ids and never
  out-of-range ones. New parsing code joins that contract and gets a `parse_*` fuzz target.
- **Fuzzing locally needs one environment variable**, or the instrumentation is silently dropped:
  ```bash
  CARGO_PROFILE_RELEASE_LTO=false cargo +nightly fuzz run parse_dict fuzz/corpus/parse_dict tests/data
  ```
  The corpus directory comes **first** — libFuzzer writes new units into the first directory it is
  given, and `tests/data` is read-only seeds. A pull request that touches `src/` or `fuzz/` is
  fuzzed by ClusterFuzzLite for ten minutes; a crash fails the check and attaches the input to the
  run as an artifact, and `cargo +nightly fuzz run <target> <file>` replays it.
- **No new dependencies without discussion.** The runtime tree is `fst` + `memmap2`, and the Python
  wheel has no runtime dependencies at all. That is a feature people choose this library for.
- **Every changed line should trace to the issue.** Adjacent cleanups make a diff harder to review
  and a regression harder to bisect.
- Conventional-commit messages (`feat:` / `fix:` / `perf:` / `refactor:` / `test:` / `docs:` /
  `chore:`), with the *why* in the body.

## Good first issues

Issues labelled [`good first issue`](https://github.com/ilgrad/lexindex/labels/good%20first%20issue)
are scoped to one file and one gate command, and each says in the body what "done" is measured by.

By contributing you agree that your contributions are licensed under the project's
[MIT license](LICENSE).
