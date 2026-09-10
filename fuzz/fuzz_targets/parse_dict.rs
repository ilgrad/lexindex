//! `DictIndex`'s loader must survive any bytes at all, and what it loads must answer in bounds.
//!
//! The framing -- magic, both checksums, five lengths, the symbol table and the three per-block
//! arrays -- is checked before anything is trusted; the front-coded block data is not, it is read
//! with every access bounded as a query reaches it. So the target does not stop at loading: it
//! asks for ids, lower bounds, keys and a walk, and the shim asserts each answer is in range. A
//! panic, a hang or an out-of-bounds read is a bug; a rejection is the expected outcome for almost
//! every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::load_dict(data);
});
