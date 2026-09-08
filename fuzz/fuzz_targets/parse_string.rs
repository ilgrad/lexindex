//! `StringIndex::from_untrusted_bytes` must return on any bytes at all.
//!
//! The other four targets fuzz framing this crate wrote and can therefore validate field by field.
//! This one fuzzes a format it does not own: past the magic and the CRC sits `fst`'s node decoder,
//! which is safe Rust but not total, and a blob carrying a recomputed checksum reaches it with an
//! invalid body. The loader under test answers that with a full walk inside `catch_unwind`, so the
//! property here is the one the security policy states — a rejection, never a panic — plus the
//! consistency of anything that does load.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::parse_string(data);
});
