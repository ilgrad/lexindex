//! `Overlay`'s framing parser must survive any bytes at all.
//!
//! The overlay is the crate's mutable layer, so its blob is the one most likely to be rewritten,
//! copied between machines and handed back truncated. Everything the parser validates — the base
//! tag, two header lengths, per-addition length prefixes, UTF-8, duplicates against each other and
//! against an exact base, and the tombstone words against the id space — runs on arbitrary input
//! here. A panic, a hang or an out-of-bounds read is a bug; a rejection is the expected outcome for
//! almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::parse_overlay_frame(data);
});
