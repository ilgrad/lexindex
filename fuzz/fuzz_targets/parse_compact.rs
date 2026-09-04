//! `CompactHashIndex`'s framing parser must survive any bytes at all.
//!
//! It is the half of the loader that runs before anything unsafe: magic, version, lengths, the
//! header and payload checksums, the fingerprint width and the side table are all validated here,
//! on input that may be truncated, corrupted or hostile. A panic, a hang or an out-of-bounds read
//! is a bug; a rejection is the expected outcome for almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Both settings on the same bytes, rather than stealing a byte of the input to choose: the seed
    // corpus is real blobs from `tests/data`, and a target that shifted them by one byte would
    // reject every seed on the magic and never get past the first branch. `verify` is the owned
    // load's payload checksum, which the memory-mapped path skips.
    let _ = lexindex::fuzzing::parse_compact_frame(data, false);
    let _ = lexindex::fuzzing::parse_compact_frame(data, true);
});
