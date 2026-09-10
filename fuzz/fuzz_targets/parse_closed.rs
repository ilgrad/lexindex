//! `ClosedHashIndex`'s framing parser must survive any bytes at all.
//!
//! The smallest framing of the three hash blobs: magic, both checksums, two lengths and the side
//! table, all validated before the MPH region is read. A panic, a hang or an out-of-bounds read is
//! a bug; a rejection is the expected outcome for almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::parse_closed_frame(data);
});
