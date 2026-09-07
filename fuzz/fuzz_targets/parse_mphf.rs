//! The minimal perfect hash's own blob, loaded and then queried on arbitrary bytes.
//!
//! `parse_compact` and `parse_perfect` reach this format only behind their own header, so a
//! mutation has to keep two checksums and a length identity intact before a byte of the MPH is
//! read — in practice they fuzz the framing and never the body. This target starts inside it.
//!
//! The assertion is not that a crafted blob is refused. Most are, and the ones that are not answer
//! wrong ids by design: the format is validated for soundness and trusted for correctness. What
//! must hold is that every answer lands in `[0, n)`, which is what makes `from_bytes` a safe fn.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::parse_mphf(data);
});
