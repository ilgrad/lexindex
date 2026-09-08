//! An `Overlay` whose embedded base is parsed by the untrusted loader, over arbitrary bytes.
//!
//! `parse_overlay` beside this one stubs the base out to fuzz the frame alone. This target does the
//! opposite half: the frame is still validated, and then the base region the header points at is
//! handed to `StringIndex::from_untrusted_bytes`, which is the composition a caller loading a
//! stranger's overlay actually runs. `OVL1` inputs are where the mutations land, since that format
//! carries no checksum for a mutation to break.
//!
//! The panic hook is replaced for the same reason as in `parse_string`, and the rule is in
//! `fuzz/Cargo.toml`: libfuzzer-sys installs a hook that aborts *before* unwinding, which fires
//! ahead of the `catch_unwind` the untrusted loader is built around, so without this block the
//! target crashes on the crate's own committed specimen and never fuzzes past it.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(
    init: {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let inside_fst = info
                .location()
                .is_some_and(|l| l.file().contains("/fst-"));
            if !inside_fst {
                default_hook(info);
                std::process::abort();
            }
        }));
    },
    |data: &[u8]| {
        let _ = lexindex::fuzzing::parse_overlay_untrusted(data);
    }
);
