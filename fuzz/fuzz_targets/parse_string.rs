//! `StringIndex::from_untrusted_bytes` must survive any bytes at all, and what it loads must agree
//! with itself.
//!
//! The target replaces libfuzzer-sys's panic hook. That hook aborts the process *before* unwinding
//! so that a panic is reported as a crash, which is right for every panic but one: `fst`'s node
//! decoder is safe Rust but not total, the untrusted loader exists to contain exactly that panic
//! with a `catch_unwind` at the load boundary, and an aborting hook fires before the unwinding
//! that the catch depends on. Without this block the target crashes on the crate's own committed
//! specimen (`tests/data/panicking-1.0.0-string.bix`) and can never fuzz past it.
//!
//! So a panic raised inside `fst` is let through to the library's catch, silently -- printing it
//! per input would flood the log and slow the run -- and every other panic keeps the aborting
//! behaviour: it is a bug in this crate, and libFuzzer should see the frames where it happened.

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
        let _ = lexindex::fuzzing::parse_string(data);
    }
);
