//! The C ABI must survive any bytes at all, and what it loads must answer in bounds.
//!
//! This is the seam the other targets do not reach: a panic in a library function is an `Err` or a
//! caught unwind to them, and an abort of the caller's process here, so `src/capi.rs`'s claim that
//! none of its paths has one on any input is only true of the loader each kind dispatches to and of
//! what its queries do with what it loaded. `lexindex_index_open` and `lexindex_index_from_bytes`
//! share that dispatch.
//!
//! The panic hook is `parse_string`'s, for the same reason: the untrusted loader contains a panic
//! raised inside `fst` with a `catch_unwind`, and libfuzzer-sys's hook would abort before it. Every
//! other panic stays a crash.

#![no_main]

// Nothing here names the crate, and an rlib no code names is not linked.
use lexindex as _;
use libfuzzer_sys::fuzz_target;
use std::ffi::{c_char, c_void};

const OK: i32 = 0;

unsafe extern "C" {
    fn lexindex_index_from_bytes(bytes: *const u8, len: usize, out: *mut *mut c_void) -> i32;
    fn lexindex_index_len(index: *const c_void) -> usize;
    fn lexindex_index_id(
        index: *const c_void,
        key: *const c_char,
        key_len: usize,
        out: *mut u64,
    ) -> i32;
    fn lexindex_index_contains(
        index: *const c_void,
        key: *const c_char,
        key_len: usize,
        out: *mut bool,
    ) -> i32;
    fn lexindex_index_key(
        index: *const c_void,
        id: u64,
        buf: *mut c_char,
        cap: usize,
        len: *mut usize,
    ) -> i32;
    fn lexindex_index_free(index: *mut c_void);
}

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
        let mut index = std::ptr::null_mut();
        // SAFETY: `data` is readable for its length and `index` is writable.
        if unsafe { lexindex_index_from_bytes(data.as_ptr(), data.len(), &mut index) } != OK {
            assert!(index.is_null(), "a refused blob left a handle");
            return;
        }
        // SAFETY: `index` is the handle just made, and it is freed only at the end. Every pointer
        // below is a live local or a slice of `data`, with its length beside it.
        unsafe {
            let n = lexindex_index_len(index) as u64;
            let mut buf = [0u8; 4096];
            for id in (0..n.min(64)).chain([n, n.saturating_add(1), u64::MAX]) {
                let mut len = 0usize;
                if lexindex_index_key(index, id, buf.as_mut_ptr().cast(), buf.len(), &mut len) == OK {
                    assert!(id < n, "key({id}) answered past {n} keys");
                    assert!(len <= buf.len(), "key({id}) wrote {len} into {}", buf.len());
                    ask(index, n, &buf[..len]);
                }
            }
            for probe in data.chunks(16).take(16) {
                ask(index, n, probe);
            }
            lexindex_index_free(index);
        }
    }
);

/// `id` and `contains` of `key`, which need not be UTF-8: that is a status, not a crash.
///
/// # Safety
///
/// `index` is a live handle.
unsafe fn ask(index: *const c_void, n: u64, key: &[u8]) {
    let mut id = 0u64;
    // SAFETY: the caller's handle, and `key` is readable for its length.
    if unsafe { lexindex_index_id(index, key.as_ptr().cast(), key.len(), &mut id) } == OK {
        assert!(id < n, "id answered {id} over {n} keys");
    }
    let mut held = false;
    // SAFETY: as above.
    let _ = unsafe { lexindex_index_contains(index, key.as_ptr().cast(), key.len(), &mut held) };
}
