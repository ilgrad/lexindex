//! `PerfectHashIndex`'s framing parser must survive any bytes at all.
//!
//! Same contract as `parse_compact`, over a wider surface: this one also validates a string arena —
//! an offset table whose ends must span its data — and the three blob versions still accepted.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Both settings on the same bytes, rather than stealing a byte of the input to choose: the seed
    // corpus is real blobs from `tests/data`, and a target that shifted them by one byte would
    // reject every seed on the magic and never get past the first branch. `verify` is the owned
    // load's payload checksum, which the memory-mapped path skips.
    let _ = lexindex::fuzzing::parse_perfect_frame(data, false);
    let _ = lexindex::fuzzing::parse_perfect_frame(data, true);
});
