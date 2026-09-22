//! `HashedDictIndex`'s loader must survive any bytes at all, and what it loads must answer in
//! bounds.
//!
//! The sidecar's framing -- magic, header checksum, five lengths, the rank table's size and the
//! side table's ranks -- is checked before anything is trusted, and the dictionary inside goes to
//! `DictIndex`'s own loader over its region. The shim loads both ways and asserts that every rank
//! `id`, `id_unchecked` and `ids_of` answer is below the key count. A panic, a hang or an
//! out-of-bounds read is a bug; a rejection is the expected outcome for almost every input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lexindex::fuzzing::load_hashed_dict(data);
});
