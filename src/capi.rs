//! The C ABI: one opaque handle over the five indexes, under the `capi` feature.
//!
//! Every function is `extern "C"`, prefixed `lexindex_`, and declared in `include/lexindex.h`,
//! which `cbindgen` generates from this file and CI regenerates to check. The surface is small on
//! purpose — open, build, save, `id`, `ids`, `contains`, `key`, free — because a C symbol is a
//! promise with a longer tail than a Rust signature: nothing like `cargo semver-checks` watches
//! it, and every caller is compiled against a header rather than a crate version.
//!
//! # One handle, five kinds
//!
//! A C caller opens a blob it may not have written, so the kind of index is a run-time fact, and
//! [`LexindexIndex`] is a sum over the five rather than five handle types. `lexindex_index_kind`
//! says which one a handle holds; what a kind cannot answer — `key` on the two indexes that store
//! no keys, `contains` on the bare perfect hash — is `LEXINDEX_STATUS_UNSUPPORTED` at run time
//! rather than a missing symbol at link time.
//!
//! # Status and message
//!
//! Every fallible function returns a [`LexindexStatus`], zero on success. A failure leaves its
//! message in a thread-local that `lexindex_last_error` reads, valid until the next failure on the
//! same thread; `NOT_FOUND` is an answer rather than a failure and leaves the message alone. A null
//! pointer where one is required is `INVALID_ARGUMENT` on every function that has a status to
//! return it in, not undefined behaviour. A panic inside the library aborts the process — there is
//! no unwinding into C — and none of the paths here has one on any input.
//!
//! # Ownership and threads
//!
//! A handle is freed by `lexindex_index_free` and nothing else. Keys and paths are borrowed for the
//! call and never retained; `lexindex_index_from_bytes` copies. A handle is immutable once made, so
//! any number of threads may query it at once: the five index types are `Send + Sync`, and the
//! message is the only mutable state, one per thread.
//!
//! # Versioning
//!
//! `LEXINDEX_ABI_VERSION` is the ABI the header describes and `lexindex_abi_version` the one the
//! library was built with. Within a number, symbols are only added, so a newer library serves an
//! older header; removing or changing one bumps it, and that is a major release of the crate.
//!
//! Not here on purpose, each additive when someone asks: `Overlay`, the mmap loaders, the automata
//! queries, and the fingerprint and block knobs of `build`.
// Only into the rlib: `tests/capi.rs` links these symbols the way C does, and a second copy in
// the unit-test binary would share their unmangled names -- see that file's header.
#![cfg(all(feature = "capi", not(test)))]

use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use crate::{
    BlobKind, ClosedHashIndex, CompactHashIndex, DictIndex, IndexError, PerfectHashIndex,
    StringIndex,
};

/// The ABI this header describes; `lexindex_abi_version` returns the library's.
pub const LEXINDEX_ABI_VERSION: u32 = 1;

/// What `lexindex_index_ids` writes for a key the index does not hold.
pub const LEXINDEX_NO_ID: u64 = 18_446_744_073_709_551_615;

/// The outcome of a call. Zero is success; every other value but `NOT_FOUND` leaves a message in
/// `lexindex_last_error`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexindexStatus {
    /// The call did what it says.
    Ok = 0,
    /// The key or id is not in the index. An answer, not a failure: the message is untouched.
    NotFound = 1,
    /// This kind of index cannot answer the question — see `LexindexKind`.
    Unsupported = 2,
    /// A null pointer where one is required, a key that is not UTF-8, or a path that is not.
    InvalidArgument = 3,
    /// The bytes are not a blob this version reads.
    Format = 4,
    /// The file could not be read or written.
    Io = 5,
    /// The perfect hash could not be built from these keys.
    Build = 6,
    /// The caller's buffer is too small; the size it needs was written to `len`.
    BufferTooSmall = 7,
}

/// Which of the five indexes a handle holds, smallest first.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexindexKind {
    /// `ClosedHashIndex`: the perfect hash alone. Every key gets an id, membership is not tested,
    /// so `contains` is unsupported and `id` of a key outside the set is some other key's.
    Closed = 0,
    /// `CompactHashIndex`: the perfect hash and a one-byte fingerprint. No keys stored, so `key`
    /// is unsupported; a key outside the set answers as present with probability 2^-8.
    Compact = 1,
    /// `DictIndex`: ordered, every key stored, exact both ways; the id is the sorted rank.
    Dict = 2,
    /// `StringIndex`: an fst — ordered, exact both ways; the id is the sorted rank.
    String = 3,
    /// `PerfectHashIndex`: exact both ways, unordered.
    Perfect = 4,
}

/// An index of any of the five kinds. Opaque: made by `lexindex_index_open`,
/// `lexindex_index_from_bytes` or `lexindex_index_build`, freed by `lexindex_index_free`.
pub struct LexindexIndex(Any);

enum Any {
    String(StringIndex),
    Dict(DictIndex),
    Compact(CompactHashIndex),
    Closed(ClosedHashIndex),
    Perfect(PerfectHashIndex),
}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

/// Records `message` for `lexindex_last_error` and hands `status` back, so a failing arm reads
/// `return fail(status, message)`.
fn fail(status: LexindexStatus, message: impl Into<Vec<u8>>) -> LexindexStatus {
    let mut bytes = message.into();
    bytes.retain(|&b| b != 0);
    LAST_ERROR.with(|last| *last.borrow_mut() = CString::new(bytes).unwrap_or_default());
    status
}

fn failed(error: IndexError) -> LexindexStatus {
    let status = match &error {
        IndexError::Io(_) => LexindexStatus::Io,
        IndexError::Build(_) => LexindexStatus::Build,
        IndexError::Fst(_) | IndexError::Format(_) => LexindexStatus::Format,
        IndexError::Automaton(_) => LexindexStatus::InvalidArgument,
    };
    fail(status, error.to_string())
}

fn status(outcome: Result<(), LexindexStatus>) -> LexindexStatus {
    match outcome {
        Ok(()) => LexindexStatus::Ok,
        Err(status) => status,
    }
}

enum Bad {
    Null,
    NotUtf8(std::str::Utf8Error),
}

fn bad(what: impl std::fmt::Display, bad: Bad) -> LexindexStatus {
    match bad {
        Bad::Null => fail(LexindexStatus::InvalidArgument, format!("{what} is null")),
        Bad::NotUtf8(e) => fail(
            LexindexStatus::InvalidArgument,
            format!("{what} is not UTF-8: {e}"),
        ),
    }
}

/// # Safety
///
/// `ptr` is null or points to `len` readable bytes that outlive the returned borrow.
unsafe fn str_from<'a>(ptr: *const c_char, len: usize) -> Result<&'a str, Bad> {
    if ptr.is_null() {
        return Err(Bad::Null);
    }
    // SAFETY: the caller promised `len` readable bytes at `ptr`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    std::str::from_utf8(bytes).map_err(Bad::NotUtf8)
}

/// # Safety
///
/// Either `n` is zero, or `keys` and `lens` point to `n` entries each and `keys[i]` to `lens[i]`
/// readable bytes, all outliving the returned borrows.
unsafe fn strs_from<'a>(
    keys: *const *const c_char,
    lens: *const usize,
    n: usize,
) -> Result<Vec<&'a str>, LexindexStatus> {
    if n == 0 {
        return Ok(Vec::new());
    }
    if keys.is_null() || lens.is_null() {
        return Err(fail(
            LexindexStatus::InvalidArgument,
            "keys or lens is null",
        ));
    }
    // SAFETY: the caller promised `n` entries in each array.
    let (keys, lens) = unsafe {
        (
            std::slice::from_raw_parts(keys, n),
            std::slice::from_raw_parts(lens, n),
        )
    };
    keys.iter()
        .zip(lens)
        .enumerate()
        // SAFETY: the caller promised `len` readable bytes at each `key`.
        .map(|(i, (&key, &len))| {
            unsafe { str_from(key, len) }.map_err(|b| bad(format_args!("key {i}"), b))
        })
        .collect()
}

/// # Safety
///
/// `path` is null or a NUL-terminated string.
unsafe fn path_from<'a>(path: *const c_char) -> Result<&'a Path, LexindexStatus> {
    if path.is_null() {
        return Err(bad("path", Bad::Null));
    }
    // SAFETY: the caller promised a NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Path::new(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(Path::new)
            .map_err(|e| bad("path", Bad::NotUtf8(e)))
    }
}

/// # Safety
///
/// `index` is null or a handle this library returned and `lexindex_index_free` has not consumed.
unsafe fn any<'a>(index: *const LexindexIndex) -> Result<&'a Any, LexindexStatus> {
    if index.is_null() {
        return Err(bad("index", Bad::Null));
    }
    // SAFETY: a non-null handle is one `Box::into_raw` produced and nothing has freed.
    Ok(&unsafe { &*index }.0)
}

/// Runs `make` and writes the handle it produces to `out`; on failure nothing is written.
///
/// # Safety
///
/// `out` is null or writable.
unsafe fn emit(
    out: *mut *mut LexindexIndex,
    make: impl FnOnce() -> Result<Any, LexindexStatus>,
) -> LexindexStatus {
    if out.is_null() {
        return bad("out", Bad::Null);
    }
    status(make().map(|any| {
        // SAFETY: `out` is non-null and the caller promised it is writable.
        unsafe { out.write(Box::into_raw(Box::new(LexindexIndex(any)))) };
    }))
}

fn load(bytes: &[u8]) -> Result<Any, LexindexStatus> {
    let any = match crate::inspect(bytes).map_err(failed)?.kind {
        BlobKind::StringIndex => Any::String(StringIndex::from_bytes(bytes).map_err(failed)?),
        BlobKind::DictIndex => Any::Dict(DictIndex::from_bytes(bytes).map_err(failed)?),
        BlobKind::CompactHashIndex => {
            Any::Compact(CompactHashIndex::from_bytes(bytes).map_err(failed)?)
        }
        BlobKind::ClosedHashIndex => {
            Any::Closed(ClosedHashIndex::from_bytes(bytes).map_err(failed)?)
        }
        BlobKind::PerfectHashIndex => {
            Any::Perfect(PerfectHashIndex::from_bytes(bytes).map_err(failed)?)
        }
        BlobKind::Mphf => {
            return Err(fail(
                LexindexStatus::Unsupported,
                "a standalone perfect hash is not an index",
            ));
        }
        BlobKind::Overlay => {
            return Err(fail(
                LexindexStatus::Unsupported,
                "overlay blobs are outside the C ABI",
            ));
        }
    };
    Ok(any)
}

/// The ABI version this library was built with, to compare with `LEXINDEX_ABI_VERSION`.
#[unsafe(no_mangle)]
pub extern "C" fn lexindex_abi_version() -> u32 {
    LEXINDEX_ABI_VERSION
}

/// The crate version as a static NUL-terminated string, `"3.0.0"` and the like.
#[unsafe(no_mangle)]
pub extern "C" fn lexindex_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// The message of the most recent failure on this thread, NUL-terminated; empty before any. Valid
/// until the next failure on the same thread.
#[unsafe(no_mangle)]
pub extern "C" fn lexindex_last_error() -> *const c_char {
    LAST_ERROR.with(|last| last.borrow().as_ptr())
}

/// Reads the blob at `path` (NUL-terminated) into a handle of whatever kind it holds, with the
/// validation `load` does in Rust: framing and checksums. Overlay and standalone perfect-hash blobs
/// are `LEXINDEX_STATUS_UNSUPPORTED`.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_open(
    path: *const c_char,
    out: *mut *mut LexindexIndex,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    unsafe {
        emit(out, || {
            let bytes = std::fs::read(path_from(path)?).map_err(|e| failed(IndexError::Io(e)))?;
            load(&bytes)
        })
    }
}

/// Parses a blob from `len` bytes at `bytes`, copying them: the buffer may go once this returns.
///
/// # Safety
///
/// `bytes` points to `len` readable bytes and `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_from_bytes(
    bytes: *const u8,
    len: usize,
    out: *mut *mut LexindexIndex,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    unsafe {
        emit(out, || {
            if bytes.is_null() {
                return Err(bad("bytes", Bad::Null));
            }
            load(std::slice::from_raw_parts(bytes, len))
        })
    }
}

/// Builds an index of `kind` from `n` keys, `keys[i]` being `lens[i]` bytes of UTF-8, in any order
/// and with duplicates collapsed. `LEXINDEX_KIND_COMPACT` gets a one-byte fingerprint and
/// `LEXINDEX_KIND_DICT` the default block, which is what `plan` prices them at.
///
/// # Safety
///
/// `kind` is one of the enumerators; either `n` is zero or `keys` and `lens` hold `n` entries each
/// and `keys[i]` points to `lens[i]` readable bytes; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_build(
    kind: LexindexKind,
    keys: *const *const c_char,
    lens: *const usize,
    n: usize,
    out: *mut *mut LexindexIndex,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    unsafe {
        emit(out, || {
            let keys = strs_from(keys, lens, n)?;
            Ok(match kind {
                LexindexKind::String => Any::String(StringIndex::build(keys).map_err(failed)?),
                LexindexKind::Dict => Any::Dict(DictIndex::build(keys).map_err(failed)?),
                LexindexKind::Compact => {
                    Any::Compact(CompactHashIndex::build(keys, 1).map_err(failed)?)
                }
                LexindexKind::Closed => Any::Closed(ClosedHashIndex::build(keys).map_err(failed)?),
                LexindexKind::Perfect => {
                    Any::Perfect(PerfectHashIndex::build(keys).map_err(failed)?)
                }
            })
        })
    }
}

/// Writes the index to `path` (NUL-terminated) the way `save` does in Rust: to a temporary file
/// beside it, renamed into place once complete.
///
/// # Safety
///
/// `index` is a live handle and `path` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_save(
    index: *const LexindexIndex,
    path: *const c_char,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    let (any, path) = match unsafe { (any(index), path_from(path)) } {
        (Ok(any), Ok(path)) => (any, path),
        (Err(status), _) | (_, Err(status)) => return status,
    };
    let saved = match any {
        Any::String(i) => i.save(path),
        Any::Dict(i) => i.save(path),
        Any::Compact(i) => i.save(path),
        Any::Closed(i) => i.save(path),
        Any::Perfect(i) => i.save(path),
    };
    status(saved.map_err(failed))
}

/// Which kind of index the handle holds.
///
/// # Safety
///
/// `index` is a live handle; there is no status to report a null one in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_kind(index: *const LexindexIndex) -> LexindexKind {
    // SAFETY: the caller promised a live handle.
    match unsafe { &(*index).0 } {
        Any::String(_) => LexindexKind::String,
        Any::Dict(_) => LexindexKind::Dict,
        Any::Compact(_) => LexindexKind::Compact,
        Any::Closed(_) => LexindexKind::Closed,
        Any::Perfect(_) => LexindexKind::Perfect,
    }
}

/// How many keys the index holds.
///
/// # Safety
///
/// `index` is a live handle; there is no status to report a null one in.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_len(index: *const LexindexIndex) -> usize {
    // SAFETY: the caller promised a live handle.
    match unsafe { &(*index).0 } {
        Any::String(i) => i.len(),
        Any::Dict(i) => i.len(),
        Any::Compact(i) => i.len(),
        Any::Closed(i) => i.len(),
        Any::Perfect(i) => i.len(),
    }
}

fn id_of(any: &Any, key: &str) -> Option<u64> {
    match any {
        Any::String(i) => i.id(key),
        Any::Dict(i) => i.id(key),
        Any::Compact(i) => i.id(key).map(u64::from),
        Any::Closed(i) => Some(u64::from(i.id(key))),
        Any::Perfect(i) => i.id(key).map(u64::from),
    }
}

/// The id of `key` (`key_len` bytes of UTF-8) into `out`, or `LEXINDEX_STATUS_NOT_FOUND`. What
/// "found" means is the kind's: exact for string, dict and perfect; probabilistic for compact,
/// which answers a key outside the set as present with probability 2^-8; every key for closed,
/// the perfect hash alone, which maps a key it never saw to some other key's id.
///
/// # Safety
///
/// `index` is a live handle, `key` points to `key_len` readable bytes, `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_id(
    index: *const LexindexIndex,
    key: *const c_char,
    key_len: usize,
    out: *mut u64,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    let (any, key) = match unsafe { (any(index), str_from(key, key_len)) } {
        (Ok(any), Ok(key)) => (any, key),
        (Err(status), _) => return status,
        (_, Err(b)) => return bad("key", b),
    };
    if out.is_null() {
        return bad("out", Bad::Null);
    }
    match id_of(any, key) {
        // SAFETY: `out` is non-null and the caller promised it is writable.
        Some(id) => unsafe {
            out.write(id);
            LexindexStatus::Ok
        },
        None => LexindexStatus::NotFound,
    }
}

/// The ids of `n` keys at once, `LEXINDEX_NO_ID` where a key is not held. This is the fast path —
/// the hash indexes prefetch across the batch. A key that is not UTF-8 fails the whole call before
/// anything is written.
///
/// # Safety
///
/// `index` is a live handle; either `n` is zero or `keys` and `lens` hold `n` entries each,
/// `keys[i]` points to `lens[i]` readable bytes, and `out` has room for `n` ids.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_ids(
    index: *const LexindexIndex,
    keys: *const *const c_char,
    lens: *const usize,
    n: usize,
    out: *mut u64,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    let (any, keys) = match unsafe { (any(index), strs_from(keys, lens, n)) } {
        (Ok(any), Ok(keys)) => (any, keys),
        (Err(status), _) | (_, Err(status)) => return status,
    };
    if n == 0 {
        return LexindexStatus::Ok;
    }
    if out.is_null() {
        return bad("out", Bad::Null);
    }
    let ids: Vec<u64> = match any {
        Any::String(i) => keys
            .iter()
            .map(|k| i.id(k).unwrap_or(LEXINDEX_NO_ID))
            .collect(),
        Any::Dict(i) => i
            .ids_of(&keys)
            .into_iter()
            .map(|id| id.unwrap_or(LEXINDEX_NO_ID))
            .collect(),
        Any::Compact(i) => i
            .ids_of(&keys)
            .into_iter()
            .map(|id| id.map_or(LEXINDEX_NO_ID, u64::from))
            .collect(),
        Any::Closed(i) => i.ids_of(&keys).into_iter().map(u64::from).collect(),
        Any::Perfect(i) => i
            .ids_of(&keys)
            .into_iter()
            .map(|id| id.map_or(LEXINDEX_NO_ID, u64::from))
            .collect(),
    };
    for (i, id) in ids.into_iter().enumerate() {
        // SAFETY: the caller promised room for `n` ids at `out`, and `i < n`.
        unsafe { out.add(i).write(id) };
    }
    LexindexStatus::Ok
}

/// Whether `key` is in the index, into `out`: exact for string, dict and perfect, probabilistic for
/// compact (the same 2^-8 as `id`), and `LEXINDEX_STATUS_UNSUPPORTED` for closed, which cannot tell.
///
/// # Safety
///
/// `index` is a live handle, `key` points to `key_len` readable bytes, `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_contains(
    index: *const LexindexIndex,
    key: *const c_char,
    key_len: usize,
    out: *mut bool,
) -> LexindexStatus {
    // SAFETY: the caller's own promises, forwarded.
    let (any, key) = match unsafe { (any(index), str_from(key, key_len)) } {
        (Ok(any), Ok(key)) => (any, key),
        (Err(status), _) => return status,
        (_, Err(b)) => return bad("key", b),
    };
    if out.is_null() {
        return bad("out", Bad::Null);
    }
    let held = match any {
        Any::String(i) => i.contains(key),
        Any::Dict(i) => i.contains(key),
        Any::Compact(i) => i.contains(key),
        Any::Perfect(i) => i.contains(key),
        Any::Closed(_) => {
            return fail(
                LexindexStatus::Unsupported,
                "a closed hash index cannot tell membership",
            );
        }
    };
    // SAFETY: `out` is non-null and the caller promised it is writable.
    unsafe { out.write(held) };
    LexindexStatus::Ok
}

/// The key of `id`, written NUL-terminated into the `cap` bytes at `buf`, its length without the
/// terminator into `len`. `LEXINDEX_STATUS_BUFFER_TOO_SMALL` when `cap < length + 1`, with `len`
/// still set — so `buf = NULL, cap = 0` asks for the size; `NOT_FOUND` for an id the index never
/// gave out; `UNSUPPORTED` for compact and closed, which store no keys.
///
/// # Safety
///
/// `index` is a live handle, `buf` points to `cap` writable bytes (or `cap` is zero), `len` is
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_key(
    index: *const LexindexIndex,
    id: u64,
    buf: *mut c_char,
    cap: usize,
    len: *mut usize,
) -> LexindexStatus {
    // SAFETY: the caller's own promise, forwarded.
    let any = match unsafe { any(index) } {
        Ok(any) => any,
        Err(status) => return status,
    };
    if len.is_null() {
        return bad("len", Bad::Null);
    }
    if buf.is_null() && cap > 0 {
        return bad("buf", Bad::Null);
    }
    // Borrowed where the index lends the key, owned where it has to be decoded.
    let key: Option<Cow<str>> = match any {
        Any::String(i) => i.key(id).map(Cow::Owned),
        Any::Dict(i) => i.key(id).map(Cow::Owned),
        Any::Perfect(i) => u32::try_from(id)
            .ok()
            .and_then(|id| i.key(id))
            .map(Cow::Borrowed),
        Any::Compact(_) | Any::Closed(_) => {
            return fail(LexindexStatus::Unsupported, "this index stores no keys");
        }
    };
    let Some(key) = key else {
        return LexindexStatus::NotFound;
    };
    // SAFETY: `len` is non-null and the caller promised it is writable.
    unsafe { len.write(key.len()) };
    if cap < key.len() + 1 {
        return fail(
            LexindexStatus::BufferTooSmall,
            format!(
                "the key needs {} bytes, the buffer has {cap}",
                key.len() + 1
            ),
        );
    }
    // SAFETY: `cap >= key.len() + 1` writable bytes at `buf`, so the key and its terminator fit.
    unsafe {
        std::ptr::copy_nonoverlapping(key.as_ptr(), buf.cast::<u8>(), key.len());
        buf.add(key.len()).write(0);
    }
    LexindexStatus::Ok
}

/// Frees a handle; null is a no-op. Every handle is freed exactly once, here.
///
/// # Safety
///
/// `index` is null or a handle this library returned that nothing has freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lexindex_index_free(index: *mut LexindexIndex) {
    if !index.is_null() {
        // SAFETY: a non-null handle is one `Box::into_raw` produced and nothing has freed.
        drop(unsafe { Box::from_raw(index) });
    }
}
