//! `inspect` must be total on any bytes.
//!
//! It is the one entry point that promises an answer for a blob it does not verify — the kind, the
//! format and the sizes from the header alone — so every length it reads is a claim, and the
//! arithmetic over those claims (an overlay's live keys, the base region it parses in turn) is
//! where a crafted header can make it wrap, recurse or allocate. A panic, a hang or an
//! out-of-bounds read is a bug; an `Err` is the expected outcome for almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::inspect(data);
});
