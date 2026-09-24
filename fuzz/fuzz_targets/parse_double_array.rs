//! `DoubleArrayIndex`'s loader must survive any bytes at all, and what it loads must answer in
//! bounds.
//!
//! The framing -- magic, header checksum, the section lengths, the code table's labels and the
//! supplementary characters' order -- is checked first, then every slot: a row must end inside the
//! array and an id below the key count, which is what lets the walks read the slots without a
//! bounds check. The shim loads both ways, with the payload checksum and without it as `load_mmap`
//! does, and queries with probes spelled from the blob's own characters: ids inside `[0, n)`,
//! prefix matches in order on character boundaries, and the occurrence walk equal to the prefix
//! walk from every character. A panic, a hang or an out-of-bounds read is a bug; a rejection is the
//! expected outcome for almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::load_double_array(data);
});
