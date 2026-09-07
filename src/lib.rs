//! lexindex: compact, immutable string↔id indexes for huge catalogs.
//!
//! Three complementary, build-once / query-many indexes over a set of strings (entity names, cluster
//! labels, document keys, vocabulary terms):
//!
//! - [`StringIndex`] — an **ordered** index backed by a finite-state transducer ([`fst`]). Exact
//!   `string → id` and `id → string` (the reverse reconstructed from the FST by a rank-walk, no stored
//!   map), plus **prefix**, **range**, **fuzzy** (Levenshtein), and **subsequence** iteration
//!   (automaton-driven; no separate key list to scan). Use it for autocomplete / fuzzy search /
//!   ordered scans.
//! - [`CompactHashIndex`] — the **smallest** `string → dense id` map: a minimal perfect hash
//!   (in-crate, the `mph` feature) plus a small fingerprint per key, storing no keys. ~1.3 B/key
//!   at the default 8-bit fingerprint (~0.8 at 4 bits), at the cost of probabilistic membership and
//!   no reverse lookup. Use it when footprint is paramount.
//! - [`PerfectHashIndex`] — a **minimal-perfect-hash** dictionary with **verified** membership and
//!   reverse lookup (keys stored). Fastest exact `string → dense id`; no ordering. Use it as a
//!   fixed-vocabulary token↔id map on a hot path.
//!
//! All three assign dense ids in `[0, n)`. None is mutable after building — they are immutable
//! summaries, like the clustering features in the companion `betula-cluster` crate.
//!
//! ```
//! use lexindex::StringIndex;
//! let idx = StringIndex::build(["apple", "apricot", "banana"]).unwrap();
//! assert_eq!(idx.id("banana"), Some(2));
//! assert_eq!(idx.key(0).as_deref(), Some("apple"));
//! assert_eq!(idx.prefix("ap").len(), 2);
//! ```

// The crate docs above link `PerfectHashIndex` / `CompactHashIndex`, which exist only under
// the default `mph` feature. docs.rs builds with default features, where the links resolve; on an
// `fst`-only build they can't, so silence the broken-link lint just there rather than downgrade the
// links to plain code spans.
#![cfg_attr(not(feature = "mph"), allow(rustdoc::broken_intra_doc_links))]
// Edition 2024 already warns; deny so an `unsafe` operation inside an `unsafe fn` must name its
// own justification in an `unsafe {}` block rather than ride on the signature.
#![deny(unsafe_op_in_unsafe_fn)]

// The 64-bit requirement outlived the dependency that imposed it: `ptr_hash`'s `sucds` refused any
// other width, and dropping it for the in-crate MPH removed that. What has *not* been done is the
// audit that would let this gate go — the MPH's slot arithmetic is `u64` throughout and narrows to
// `usize` in a handful of places, each of which is a truncation on a 32-bit target rather than an
// error. Until every one of those is checked and cross-built, say so here rather than ship a
// silently wrong index on wasm32.
#[cfg(all(feature = "mph", not(target_pointer_width = "64")))]
compile_error!(
    "lexindex's `mph` feature (PerfectHashIndex, CompactHashIndex) requires a 64-bit target: its \
     minimal perfect hash indexes slots as `u64` and narrows them to `usize`, which has not been \
     audited for a narrower width. Build with `--no-default-features` for the `fst`-only \
     `StringIndex`, which supports 32-bit targets including `wasm32-unknown-unknown`."
);

mod blob;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
mod mphf;
mod overlay;
mod string_index;
mod subsequence;

pub use overlay::{Overlay, OverlayBase, OverlayKeys};
pub use string_index::StringIndex;

// The minimal-perfect-hash indexes (`PerfectHashIndex`, `CompactHashIndex`) and their shared key hash
// and arena live behind the `mph` feature; `StringIndex` reconstructs `id → key` from the FST itself
// and needs none of them.
// The width is part of the gate so that a 32-bit build reports the `compile_error!` above and
// nothing else: without it, the modules would also fail on their own narrowing conversions, and the
// one message that explains what to do would be buried.
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
mod arena;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
mod compact_hash;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
mod hash;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
mod perfect_hash;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
pub use compact_hash::CompactHashIndex;
#[cfg(all(feature = "mph", target_pointer_width = "64"))]
pub use perfect_hash::PerfectHashIndex;

#[cfg(all(feature = "python", target_pointer_width = "64"))]
mod python;

/// Entry points for the fuzz targets in `fuzz/`, and **not public API**: the `fuzzing` feature is
/// off by default and this module may change or vanish in any release.
///
/// What it exposes is the framing half of the blob loaders — the parsers that validate magic,
/// lengths, checksums, the side table and the fingerprint range before the MPH region is read.
/// Those carry the branches arbitrary bytes actually reach, and they are `pub(crate)`; a libFuzzer
/// target lives in its own crate and cannot see them.
#[cfg(all(feature = "fuzzing", feature = "mph", target_pointer_width = "64"))]
#[doc(hidden)]
pub mod fuzzing {
    /// Parse the framing of a `CompactHashIndex` blob; `true` if it was accepted. The verdict is
    /// not the point — not panicking, hanging or reading out of bounds is.
    pub fn parse_compact_frame(bytes: &[u8], verify: bool) -> bool {
        crate::CompactHashIndex::fuzz_parse_frame(bytes, verify)
    }

    /// [`parse_compact_frame`] for a `PerfectHashIndex` blob, whose framing also has to validate an
    /// arena of offsets.
    pub fn parse_perfect_frame(bytes: &[u8], verify: bool) -> bool {
        crate::PerfectHashIndex::fuzz_parse_frame(bytes, verify)
    }

    /// Load a standalone `MPH1` blob and query the table it produced.
    ///
    /// The other targets reach this format only through a `BMP5`/`BCH6` header, which means a
    /// mutation has to keep two checksums and a length identity intact before the MPH region is
    /// read at all — so in practice they fuzz the framing and never the body. This one starts
    /// inside it.
    ///
    /// Loading is only half of what is asserted. `from_bytes` is a safe fn because every read the
    /// table makes is bounded by a length derived from its own header, and the claim that follows
    /// is that a crafted blob answers *wrong* ids rather than out-of-range ones. So the queries
    /// below are the actual property under test: an answer outside `[0, n)` fails here as a wrong
    /// number, which is a far easier signal to act on than the crash it would eventually become.
    pub fn parse_mphf(bytes: &[u8]) -> bool {
        let Ok(mph) = crate::mphf::Mphf::from_bytes(bytes) else {
            return false;
        };
        let n = mph.n();
        if n == 0 {
            return true;
        }
        for h in [
            0,
            1,
            u64::MAX,
            0x9e37_79b9_7f4a_7c15,
            0xdead_beef_dead_beef,
            0x0123_4567_89ab_cdef,
        ] {
            assert!(mph.index(h) < n, "id {} is outside [0, {n})", mph.index(h));
        }
        assert!(mph.index_all(&[0, 1, u64::MAX]).iter().all(|&i| i < n));
        true
    }

    /// Parse the framing of an `Overlay` blob: the magic, the header checksum, the base tag, the
    /// four header lengths, the payload checksum, the length-prefixed additions with their UTF-8
    /// and duplicate checks, and the tombstone words. Both formats reach this — `OVL2`, and the
    /// `OVL1` a `0.12` file carries, whose framing is the one without the checksums.
    ///
    /// **The embedded base blob is deliberately not parsed.** The loader closure ignores it and
    /// returns a fixed two-key index, so this target exercises the overlay's own framing and does
    /// not re-fuzz `StringIndex::from_bytes` — which was a target of its own, and was removed
    /// because it re-finds a panic in `fst`'s node decoder that this crate cannot fix (see
    /// `fuzz/Cargo.toml`). Everything the overlay validates about the base region — that its length
    /// is in range, and that the tag matches before the loader is called at all — still runs.
    pub fn parse_overlay_frame(bytes: &[u8]) -> bool {
        static BASE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        let blob = BASE.get_or_init(|| {
            crate::StringIndex::build(["a", "b"])
                .expect("a two-key index builds")
                .to_bytes()
        });
        crate::Overlay::<crate::StringIndex>::from_bytes_with(bytes, |_| {
            crate::StringIndex::from_bytes(blob)
        })
        .is_ok()
    }
}

// Compiles and runs the README's Rust snippets as doctests without pulling its prose into the API
// docs, so a signature change that invalidates an example fails the test suite rather than shipping.
#[cfg(all(doctest, feature = "mph", feature = "mmap"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

// Same for the usage guide, whose Rust block exercises every index and the unsafe loaders.
#[cfg(all(doctest, feature = "mph", feature = "mmap"))]
#[doc = include_str!("../docs/usage.md")]
struct UsageDoctests;

use std::fmt;

/// Errors from building, querying, or (de)serialising an index.
#[derive(Debug)]
pub enum IndexError {
    /// An error from the underlying finite-state transducer.
    Fst(fst::Error),
    /// An I/O error from [`StringIndex::save`] / [`StringIndex::load`].
    Io(std::io::Error),
    /// A malformed serialised buffer (bad magic, version, length, or offsets).
    Format(&'static str),
    /// A fuzzy/automaton query could not be compiled (e.g. the Levenshtein automaton for the given
    /// query and edit distance would be too large).
    Automaton(String),
    /// (De)serialisation of a [`PerfectHashIndex`] blob failed (corrupt or incompatible MPH bytes).
    #[cfg(feature = "mph")]
    Serde(String),
    /// Constructing the minimal perfect hash failed after exhausting its retry seeds — extremely
    /// rare; rebuilding with a different key set is the only recourse.
    #[cfg(feature = "mph")]
    Build(&'static str),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::Fst(e) => write!(f, "fst error: {e}"),
            IndexError::Io(e) => write!(f, "io error: {e}"),
            IndexError::Format(m) => write!(f, "format error: {m}"),
            IndexError::Automaton(m) => write!(f, "automaton error: {m}"),
            #[cfg(feature = "mph")]
            IndexError::Serde(m) => write!(f, "serde error: {m}"),
            #[cfg(feature = "mph")]
            IndexError::Build(m) => write!(f, "build error: {m}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Fst(e) => Some(e),
            IndexError::Io(e) => Some(e),
            IndexError::Format(_) | IndexError::Automaton(_) => None,
            #[cfg(feature = "mph")]
            IndexError::Serde(_) => None,
            #[cfg(feature = "mph")]
            IndexError::Build(_) => None,
        }
    }
}

impl From<fst::Error> for IndexError {
    fn from(e: fst::Error) -> Self {
        IndexError::Fst(e)
    }
}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        IndexError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn index_error_display_and_source() {
        // Format / Automaton carry a message and have no underlying source.
        let fmt = IndexError::Format("bad blob");
        assert!(fmt.to_string().contains("bad blob"));
        assert!(fmt.source().is_none());
        let auto = IndexError::Automaton("automaton too large".into());
        assert!(auto.to_string().contains("automaton too large"));
        assert!(auto.source().is_none());

        // Io wraps a std::io::Error (with a source), reachable through the `From` impl.
        let io: IndexError = std::io::Error::new(std::io::ErrorKind::NotFound, "nope").into();
        assert!(io.to_string().contains("io error"));
        assert!(io.source().is_some());

        // Fst wraps an fst::Error (with a source): an out-of-order insert triggers one.
        let mut b = fst::MapBuilder::memory();
        b.insert("b", 1).unwrap();
        let fst_err: IndexError = b.insert("a", 0).unwrap_err().into();
        assert!(fst_err.to_string().contains("fst error"));
        assert!(fst_err.source().is_some());
    }

    #[cfg(feature = "mph")]
    #[test]
    fn serde_error_display_has_no_source() {
        let e = IndexError::Serde("corrupt mph".into());
        assert!(e.to_string().contains("serde error") && e.to_string().contains("corrupt mph"));
        assert!(e.source().is_none());
    }
}
