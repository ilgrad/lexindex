//! The C ABI, exercised the way C exercises it: through `extern "C"` declarations of the exported
//! symbols and `#[repr(C)]` copies of the header's types, never through the Rust module. Besides
//! being the caller the ABI exists for, this is the only place the symbols can be measured: a
//! `#[no_mangle]` function compiled into the unit-test binary carries the same name as the copy in
//! the rlib that every integration test links, and `llvm-profdata` merges the two records into a
//! count of zero (68 % of `capi.rs` reported uncovered by twelve tests that call every function).
//! So the module is compiled only into the rlib (`not(test)`), and this file is its test.
#![cfg(feature = "capi")]

use std::ffi::{CStr, CString, c_char};
use std::ptr;

/// `LEXINDEX_ABI_VERSION` in the header.
const LEXINDEX_ABI_VERSION: u32 = 1;
/// `LEXINDEX_NO_ID` in the header.
const LEXINDEX_NO_ID: u64 = u64::MAX;

/// `LexindexStatus` in the header, value for value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexindexStatus {
    Ok = 0,
    NotFound = 1,
    Unsupported = 2,
    InvalidArgument = 3,
    Format = 4,
    Io = 5,
    Build = 6,
    BufferTooSmall = 7,
}

/// `LexindexKind` in the header, value for value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexindexKind {
    Closed = 0,
    Compact = 1,
    Dict = 2,
    String = 3,
    Perfect = 4,
}

/// The opaque handle: only ever behind a pointer.
#[repr(C)]
struct LexindexIndex {
    _private: [u8; 0],
}

unsafe extern "C" {
    safe fn lexindex_abi_version() -> u32;
    safe fn lexindex_version() -> *const c_char;
    safe fn lexindex_last_error() -> *const c_char;
    fn lexindex_index_open(path: *const c_char, out: *mut *mut LexindexIndex) -> LexindexStatus;
    fn lexindex_index_from_bytes(
        bytes: *const u8,
        len: usize,
        out: *mut *mut LexindexIndex,
    ) -> LexindexStatus;
    fn lexindex_index_build(
        kind: LexindexKind,
        keys: *const *const c_char,
        lens: *const usize,
        n: usize,
        out: *mut *mut LexindexIndex,
    ) -> LexindexStatus;
    fn lexindex_index_save(index: *const LexindexIndex, path: *const c_char) -> LexindexStatus;
    fn lexindex_index_kind(index: *const LexindexIndex) -> LexindexKind;
    fn lexindex_index_len(index: *const LexindexIndex) -> usize;
    fn lexindex_index_id(
        index: *const LexindexIndex,
        key: *const c_char,
        key_len: usize,
        out: *mut u64,
    ) -> LexindexStatus;
    fn lexindex_index_ids(
        index: *const LexindexIndex,
        keys: *const *const c_char,
        lens: *const usize,
        n: usize,
        out: *mut u64,
    ) -> LexindexStatus;
    fn lexindex_index_contains(
        index: *const LexindexIndex,
        key: *const c_char,
        key_len: usize,
        out: *mut bool,
    ) -> LexindexStatus;
    fn lexindex_index_key(
        index: *const LexindexIndex,
        id: u64,
        buf: *mut c_char,
        cap: usize,
        len: *mut usize,
    ) -> LexindexStatus;
    fn lexindex_index_free(index: *mut LexindexIndex);
}
const FRUIT: [&str; 4] = ["apple", "apricot", "banana", "cherry"];
const KINDS: [LexindexKind; 5] = [
    LexindexKind::Closed,
    LexindexKind::Compact,
    LexindexKind::Dict,
    LexindexKind::String,
    LexindexKind::Perfect,
];
const EXACT: [LexindexKind; 3] = [
    LexindexKind::Dict,
    LexindexKind::String,
    LexindexKind::Perfect,
];

fn arrays(keys: &[&str]) -> (Vec<*const c_char>, Vec<usize>) {
    (
        keys.iter().map(|k| k.as_ptr().cast()).collect(),
        keys.iter().map(|k| k.len()).collect(),
    )
}

fn message() -> String {
    unsafe { CStr::from_ptr(lexindex_last_error()) }
        .to_string_lossy()
        .into_owned()
}

fn build(kind: LexindexKind, keys: &[&str]) -> *mut LexindexIndex {
    let (ptrs, lens) = arrays(keys);
    let mut out = ptr::null_mut();
    let status =
        unsafe { lexindex_index_build(kind, ptrs.as_ptr(), lens.as_ptr(), keys.len(), &mut out) };
    assert_eq!(status, LexindexStatus::Ok, "{kind:?}: {}", message());
    assert!(!out.is_null());
    out
}

fn id(index: *const LexindexIndex, key: &str) -> Result<u64, LexindexStatus> {
    let mut out = u64::MAX;
    match unsafe { lexindex_index_id(index, key.as_ptr().cast(), key.len(), &mut out) } {
        LexindexStatus::Ok => Ok(out),
        status => Err(status),
    }
}

fn ids(index: *const LexindexIndex, keys: &[&str]) -> Result<Vec<u64>, LexindexStatus> {
    let (ptrs, lens) = arrays(keys);
    let mut out = vec![7u64; keys.len()];
    match unsafe {
        lexindex_index_ids(
            index,
            ptrs.as_ptr(),
            lens.as_ptr(),
            keys.len(),
            out.as_mut_ptr(),
        )
    } {
        LexindexStatus::Ok => Ok(out),
        status => Err(status),
    }
}

fn contains(index: *const LexindexIndex, key: &str) -> Result<bool, LexindexStatus> {
    let mut out = false;
    match unsafe { lexindex_index_contains(index, key.as_ptr().cast(), key.len(), &mut out) } {
        LexindexStatus::Ok => Ok(out),
        status => Err(status),
    }
}

/// Calls `key` with a buffer of `cap` bytes (null when zero) and returns the status, the bytes
/// written, and the length reported.
fn key(index: *const LexindexIndex, id: u64, cap: usize) -> (LexindexStatus, Vec<u8>, usize) {
    let mut buf = vec![0xAAu8; cap];
    let ptr = if cap == 0 {
        ptr::null_mut()
    } else {
        buf.as_mut_ptr().cast()
    };
    let mut len = usize::MAX;
    let status = unsafe { lexindex_index_key(index, id, ptr, cap, &mut len) };
    (status, buf, len)
}

fn free(index: *mut LexindexIndex) {
    unsafe { lexindex_index_free(index) }
}

fn kind_of(index: *const LexindexIndex) -> LexindexKind {
    unsafe { lexindex_index_kind(index) }
}

fn len_of(index: *const LexindexIndex) -> usize {
    unsafe { lexindex_index_len(index) }
}

#[test]
fn every_kind_builds_and_answers_id() {
    for kind in KINDS {
        let index = build(kind, &["cherry", "apple", "banana", "apricot", "apple"]);
        assert_eq!(kind_of(index), kind);
        assert_eq!(len_of(index), 4, "{kind:?}");
        let singles: Vec<u64> = FRUIT.iter().map(|k| id(index, k).unwrap()).collect();
        assert!(singles.iter().all(|&i| i < 4), "{kind:?}: {singles:?}");
        assert_eq!(ids(index, &FRUIT).unwrap(), singles, "{kind:?}");
        if matches!(kind, LexindexKind::Dict | LexindexKind::String) {
            assert_eq!(singles, [0, 1, 2, 3], "{kind:?}: the id is the sorted rank");
        }
        let mut sorted = singles.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "{kind:?}: four keys, four ids");
        free(index);
    }
}

#[test]
fn a_miss_is_not_found_where_the_kind_can_tell() {
    for kind in EXACT {
        let index = build(kind, &FRUIT);
        assert_eq!(
            id(index, "durian"),
            Err(LexindexStatus::NotFound),
            "{kind:?}"
        );
        assert_eq!(
            ids(index, &["banana", "durian"]).unwrap()[1],
            LEXINDEX_NO_ID,
            "{kind:?}"
        );
        free(index);
    }
    // The perfect hash alone maps every key somewhere inside the set.
    let closed = build(LexindexKind::Closed, &FRUIT);
    assert!(id(closed, "durian").unwrap() < 4);
    free(closed);
    // One fingerprint byte: a miss answers as present one time in 256, so only agreement between
    // the single and the batch form is a promise.
    let compact = build(LexindexKind::Compact, &FRUIT);
    let single = id(compact, "durian").ok().unwrap_or(LEXINDEX_NO_ID);
    assert_eq!(ids(compact, &["durian"]).unwrap(), [single]);
    free(compact);
}

#[test]
fn keys_come_back_where_they_are_stored() {
    for kind in EXACT {
        let index = build(kind, &FRUIT);
        for want in FRUIT {
            let i = id(index, want).unwrap();
            let (status, buf, len) = key(index, i, want.len() + 1);
            assert_eq!(status, LexindexStatus::Ok, "{kind:?}: {}", message());
            assert_eq!(len, want.len());
            assert_eq!(&buf[..len], want.as_bytes());
            assert_eq!(buf[len], 0, "NUL-terminated");
        }
        let apricot = id(index, "apricot").unwrap();
        let (status, buf, len) = key(index, apricot, 7);
        assert_eq!(status, LexindexStatus::BufferTooSmall, "{kind:?}");
        assert_eq!(
            len, 7,
            "the length is reported even when nothing is written"
        );
        assert!(buf.iter().all(|&b| b == 0xAA), "nothing written");
        assert!(message().contains("8 bytes"), "{}", message());
        let (status, _, len) = key(index, apricot, 0);
        assert_eq!(
            (status, len),
            (LexindexStatus::BufferTooSmall, 7),
            "size query"
        );
        let (status, _, len) = key(index, 99, 32);
        assert_eq!(status, LexindexStatus::NotFound, "{kind:?}");
        assert_eq!(len, usize::MAX, "untouched on a miss");
        free(index);
    }
    for kind in [LexindexKind::Compact, LexindexKind::Closed] {
        let index = build(kind, &FRUIT);
        let (status, _, _) = key(index, 0, 32);
        assert_eq!(status, LexindexStatus::Unsupported, "{kind:?}");
        assert_eq!(message(), "this index stores no keys");
        free(index);
    }
}

#[test]
fn contains_is_exact_probabilistic_or_unsupported() {
    for kind in EXACT {
        let index = build(kind, &FRUIT);
        assert_eq!(contains(index, "banana"), Ok(true), "{kind:?}");
        assert_eq!(contains(index, "durian"), Ok(false), "{kind:?}");
        free(index);
    }
    let compact = build(LexindexKind::Compact, &FRUIT);
    assert!(FRUIT.iter().all(|k| contains(compact, k) == Ok(true)));
    free(compact);
    let closed = build(LexindexKind::Closed, &FRUIT);
    assert_eq!(contains(closed, "banana"), Err(LexindexStatus::Unsupported));
    assert_eq!(message(), "a closed hash index cannot tell membership");
    free(closed);
}

#[test]
fn save_open_and_from_bytes_round_trip_every_kind() {
    for kind in KINDS {
        let path = std::env::temp_dir().join(format!(
            "lexindex_capi_{}_{kind:?}.blob",
            std::process::id()
        ));
        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        let built = build(kind, &FRUIT);
        assert_eq!(
            unsafe { lexindex_index_save(built, c_path.as_ptr()) },
            LexindexStatus::Ok,
            "{kind:?}: {}",
            message()
        );
        let mut opened = ptr::null_mut();
        assert_eq!(
            unsafe { lexindex_index_open(c_path.as_ptr(), &mut opened) },
            LexindexStatus::Ok,
            "{kind:?}: {}",
            message()
        );
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut parsed = ptr::null_mut();
        assert_eq!(
            unsafe { lexindex_index_from_bytes(bytes.as_ptr(), bytes.len(), &mut parsed) },
            LexindexStatus::Ok,
            "{kind:?}: {}",
            message()
        );
        for copy in [opened, parsed] {
            assert_eq!(kind_of(copy), kind);
            assert_eq!(len_of(copy), 4);
            assert_eq!(
                ids(copy, &FRUIT).unwrap(),
                ids(built, &FRUIT).unwrap(),
                "{kind:?}"
            );
            free(copy);
        }
        free(built);
    }
}

#[test]
fn blobs_outside_the_abi_are_unsupported() {
    let overlay = lexindex::Overlay::new(lexindex::StringIndex::build(FRUIT).unwrap());
    let bytes = overlay.to_bytes().unwrap();
    let mut out = ptr::null_mut();
    let status = unsafe { lexindex_index_from_bytes(bytes.as_ptr(), bytes.len(), &mut out) };
    assert_eq!(status, LexindexStatus::Unsupported);
    assert_eq!(message(), "overlay blobs are outside the C ABI");
    assert!(out.is_null(), "nothing written on failure");

    let dict = lexindex::DictIndex::build(FRUIT).unwrap();
    let bytes = lexindex::HashedDictIndex::from_dict(dict, 8)
        .unwrap()
        .to_bytes();
    let status = unsafe { lexindex_index_from_bytes(bytes.as_ptr(), bytes.len(), &mut out) };
    assert_eq!(status, LexindexStatus::Unsupported);
    assert_eq!(message(), "HashedDictIndex blobs are outside the C ABI");
    assert!(out.is_null(), "nothing written on failure");
}

#[test]
fn garbage_is_a_format_error() {
    let mut out = ptr::null_mut();
    for bytes in [
        &b"not a blob at all"[..],
        &lexindex::StringIndex::build(FRUIT).unwrap().to_bytes()[..20],
    ] {
        let status = unsafe { lexindex_index_from_bytes(bytes.as_ptr(), bytes.len(), &mut out) };
        assert_eq!(status, LexindexStatus::Format);
        // The framing rejects the first, the fst inside rejects the second.
        assert!(message().contains("error: "), "{}", message());
        assert!(out.is_null());
    }
}

#[test]
fn a_missing_file_is_io() {
    let path = c"/nonexistent/lexindex-capi/none.blob";
    let mut out = ptr::null_mut();
    assert_eq!(
        unsafe { lexindex_index_open(path.as_ptr(), &mut out) },
        LexindexStatus::Io
    );
    assert!(message().starts_with("io error: "), "{}", message());
    assert!(out.is_null());
}

#[test]
fn nulls_and_bad_utf8_are_invalid_arguments() {
    fn check(status: LexindexStatus, want: &str) {
        assert_eq!(status, LexindexStatus::InvalidArgument, "{want}");
        assert_eq!(message(), want);
    }
    let index = build(LexindexKind::Dict, &FRUIT);
    let (ptrs, lens) = arrays(&FRUIT);
    let mut out = ptr::null_mut();
    let mut n = 0u64;
    let mut held = false;
    let mut len = 0usize;
    let bad_utf8: [u8; 2] = [0xFF, 0x41];
    let not_utf8 = "is not UTF-8: invalid utf-8 sequence of 1 bytes from index 0";
    unsafe {
        check(lexindex_index_open(ptr::null(), &mut out), "path is null");
        check(
            lexindex_index_open(c"x".as_ptr(), ptr::null_mut()),
            "out is null",
        );
        check(
            lexindex_index_from_bytes(ptr::null(), 4, &mut out),
            "bytes is null",
        );
        check(
            lexindex_index_build(LexindexKind::Dict, ptr::null(), lens.as_ptr(), 4, &mut out),
            "keys or lens is null",
        );
        check(
            lexindex_index_build(
                LexindexKind::Dict,
                [bad_utf8.as_ptr().cast()].as_ptr(),
                [2usize].as_ptr(),
                1,
                &mut out,
            ),
            &format!("key 0 {not_utf8}"),
        );
        check(
            lexindex_index_save(ptr::null(), c"x".as_ptr()),
            "index is null",
        );
        check(lexindex_index_save(index, ptr::null()), "path is null");
        check(
            lexindex_index_id(index, ptr::null(), 0, &mut n),
            "key is null",
        );
        check(
            lexindex_index_id(index, c"apple".as_ptr(), 5, ptr::null_mut()),
            "out is null",
        );
        check(
            lexindex_index_ids(index, ptrs.as_ptr(), lens.as_ptr(), 4, ptr::null_mut()),
            "out is null",
        );
        check(
            lexindex_index_contains(index, bad_utf8.as_ptr().cast(), 2, &mut held),
            &format!("key {not_utf8}"),
        );
        check(
            lexindex_index_key(index, 0, ptr::null_mut(), 16, &mut len),
            "buf is null",
        );
        check(
            lexindex_index_key(index, 0, ptr::null_mut(), 0, ptr::null_mut()),
            "len is null",
        );
    }
    assert!(out.is_null(), "no constructor wrote a handle");
    free(index);
}

#[test]
fn not_found_leaves_the_message_alone() {
    let index = build(LexindexKind::String, &FRUIT);
    assert_eq!(
        unsafe { lexindex_index_id(index, ptr::null(), 0, ptr::null_mut()) },
        LexindexStatus::InvalidArgument
    );
    assert_eq!(id(index, "durian"), Err(LexindexStatus::NotFound));
    assert_eq!(key(index, 44, 8).0, LexindexStatus::NotFound);
    assert_eq!(
        message(),
        "key is null",
        "two answers later, the last failure still reads"
    );
    free(index);
}

#[test]
fn versions_empty_batches_and_null_frees() {
    assert_eq!(lexindex_abi_version(), LEXINDEX_ABI_VERSION);
    let version = unsafe { CStr::from_ptr(lexindex_version()) }
        .to_str()
        .unwrap();
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    free(ptr::null_mut());
    let index = build(LexindexKind::Perfect, &FRUIT);
    assert_eq!(
        unsafe { lexindex_index_ids(index, ptr::null(), ptr::null(), 0, ptr::null_mut()) },
        LexindexStatus::Ok,
        "nothing to look up, nothing to write, nothing to check"
    );
    free(index);
    for kind in [LexindexKind::String, LexindexKind::Dict] {
        let empty = build(kind, &[]);
        assert_eq!(len_of(empty), 0);
        assert_eq!(id(empty, "apple"), Err(LexindexStatus::NotFound));
        free(empty);
    }
}

#[test]
fn the_message_is_per_thread() {
    let mut out = ptr::null_mut();
    unsafe { lexindex_index_open(ptr::null(), &mut out) };
    assert_eq!(message(), "path is null");
    let elsewhere = std::thread::spawn(message).join().unwrap();
    assert_eq!(elsewhere, "", "a thread that never failed has no message");
    assert_eq!(message(), "path is null");
}

/// The copies above are the header's, value for value: every enumerator and both constants.
#[test]
fn the_declarations_here_are_the_headers() {
    let header = include_str!("../include/lexindex.h");
    let value_of = |name: &str| -> u64 {
        let line = header
            .lines()
            .find(|l| l.split_whitespace().any(|w| w == name))
            .unwrap_or_else(|| panic!("{name} is not in the header"));
        let text = line.split(['=', ' ']).rfind(|w| !w.is_empty()).unwrap();
        text.trim_end_matches(',')
            .trim_end_matches("ull")
            .parse()
            .unwrap()
    };
    let statuses = [
        ("LEXINDEX_STATUS_OK", LexindexStatus::Ok),
        ("LEXINDEX_STATUS_NOT_FOUND", LexindexStatus::NotFound),
        ("LEXINDEX_STATUS_UNSUPPORTED", LexindexStatus::Unsupported),
        (
            "LEXINDEX_STATUS_INVALID_ARGUMENT",
            LexindexStatus::InvalidArgument,
        ),
        ("LEXINDEX_STATUS_FORMAT", LexindexStatus::Format),
        ("LEXINDEX_STATUS_IO", LexindexStatus::Io),
        ("LEXINDEX_STATUS_BUILD", LexindexStatus::Build),
        (
            "LEXINDEX_STATUS_BUFFER_TOO_SMALL",
            LexindexStatus::BufferTooSmall,
        ),
    ];
    for (name, status) in statuses {
        assert_eq!(value_of(name), status as u64, "{name}");
    }
    let kinds = [
        ("LEXINDEX_KIND_CLOSED", LexindexKind::Closed),
        ("LEXINDEX_KIND_COMPACT", LexindexKind::Compact),
        ("LEXINDEX_KIND_DICT", LexindexKind::Dict),
        ("LEXINDEX_KIND_STRING", LexindexKind::String),
        ("LEXINDEX_KIND_PERFECT", LexindexKind::Perfect),
    ];
    for (name, kind) in kinds {
        assert_eq!(value_of(name), kind as u64, "{name}");
    }
    assert_eq!(
        value_of("LEXINDEX_ABI_VERSION"),
        u64::from(LEXINDEX_ABI_VERSION)
    );
    assert_eq!(value_of("LEXINDEX_NO_ID"), LEXINDEX_NO_ID);
    let declarations = header
        .lines()
        .filter(|l| !l.starts_with([' ', '*', '/']) && l.contains("lexindex_") && l.contains('('))
        .count();
    assert_eq!(declarations, 14, "fourteen functions declared");
}
