//! lexindex: compact, immutable string↔id indexes for huge catalogs.
//!
//! Five complementary, build-once / query-many indexes over a set of strings (entity names, cluster
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
//! - [`ClosedHashIndex`] — the perfect hash **and nothing else**: `id(key) -> u32`, no
//!   `Option`, for a vocabulary known to be closed. A member's id, and for anything else some id
//!   in `[0, n)`. 0.26 B/key, a fifth of `CompactHashIndex`, as a token → id map where every
//!   query is a member by construction.
//! - [`PerfectHashIndex`] — a **minimal-perfect-hash** dictionary with **verified** membership and
//!   reverse lookup (keys stored); no ordering. `id` costs about what a `std::HashMap` lookup does,
//!   at 10.9 B/key; `id_unchecked`, which skips the membership comparison, is the fastest lookup in
//!   the crate for a vocabulary known to be closed. Use it as a token↔id map on a hot path.
//! - [`DictIndex`] — an **ordered** dictionary with the key stored for every id: exact
//!   `string ↔ rank` both ways, plus `lower_bound` and in-order iteration, and nothing else — no
//!   automata, so no prefix or fuzzy queries. The sorted keys front-coded in blocks with the
//!   suffixes under a static symbol table: about 3.5 B/key on real words, a third of
//!   `StringIndex`. Use it where the queries are exact and the index has to be small.
//!
//! All five assign dense ids in `[0, n)`. None is mutable after building — they are immutable
//! summaries, like the clustering features in the companion `betula-cluster` crate.
//!
//! The minimal perfect hash under the two hash indexes implements [PHast]'s map-or-bump
//! construction, the successor of [PTHash]; the crate depended on [`ptr_hash`] for the latter until
//! 1.0, and the README says why that changed.
//!
//! ```
//! use lexindex::StringIndex;
//! let idx = StringIndex::build(["apple", "apricot", "banana"]).unwrap();
//! assert_eq!(idx.id("banana"), Some(2));
//! assert_eq!(idx.key(0).as_deref(), Some("apple"));
//! assert_eq!(idx.prefix("ap").len(), 2);
//! ```
//!
//! [PTHash]: https://arxiv.org/abs/2104.10402
//! [PHast]: https://arxiv.org/abs/2504.17918
//! [`ptr_hash`]: https://arxiv.org/abs/2502.15539

// The crate docs above link `PerfectHashIndex` / `CompactHashIndex`, which exist only under
// the default `mph` feature. docs.rs builds with default features, where the links resolve; on an
// `fst`-only build they can't, so silence the broken-link lint just there rather than downgrade the
// links to plain code spans.
#![cfg_attr(not(feature = "mph"), allow(rustdoc::broken_intra_doc_links))]
// Edition 2024 already warns; deny so an `unsafe` operation inside an `unsafe fn` must name its
// own justification in an `unsafe {}` block rather than ride on the signature.
#![deny(unsafe_op_in_unsafe_fn)]
// Feature badges on docs.rs. `doc_cfg` is nightly-only and docs.rs builds on nightly with the
// `--cfg docsrs` its metadata in `Cargo.toml` asks for; no other build ever sees the cfg, so the
// attribute costs a stable compiler nothing.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod blob;
#[cfg(feature = "mph")]
mod mphf;
/// The perfect hash on its own, for a harness that compares it with other MPHFs over the same
/// hashes. A measurement export, not API: it exists only under `bench-mphf` and is hidden.
#[cfg(feature = "bench-mphf")]
#[doc(hidden)]
pub use mphf::Mphf;
mod dict_index;
mod fsst;
mod inspect;
mod overlay;
mod string_index;
mod subsequence;

pub use dict_index::DictIndex;
pub use inspect::{BlobInfo, BlobKind, OverlayInfo, inspect, inspect_file};
pub use overlay::{Overlay, OverlayBase, OverlayKeys};
pub use string_index::StringIndex;

// The minimal-perfect-hash indexes (`PerfectHashIndex`, `CompactHashIndex`) and their shared key hash
// and arena live behind the `mph` feature; `StringIndex` reconstructs `id → key` from the FST itself
// and needs none of them.
//
// They were 64-bit-only until 1.0, for two reasons that both went away: `ptr_hash` pulled in `sucds`,
// which refuses any other width, and the MPH's own `u64 → usize` narrowings had not been audited.
// The dependency left with the backend, and the audit is done — every length a blob supplies is
// converted with `try_from` and every build-path narrowing is bounded by the address space it would
// have to exhaust first. `cargo check` on `i686` and `wasm32` is a CI job, so it stays that way.
#[cfg(feature = "mph")]
mod arena;
#[cfg(feature = "mph")]
mod closed_hash;
#[cfg(feature = "mph")]
mod compact_hash;
#[cfg(feature = "mph")]
mod hash;
#[cfg(feature = "mph")]
mod perfect_hash;
#[cfg(feature = "mph")]
#[cfg_attr(docsrs, doc(cfg(feature = "mph")))]
pub use closed_hash::ClosedHashIndex;
#[cfg(feature = "mph")]
#[cfg_attr(docsrs, doc(cfg(feature = "mph")))]
pub use compact_hash::CompactHashIndex;
#[cfg(feature = "mph")]
#[cfg_attr(docsrs, doc(cfg(feature = "mph")))]
pub use perfect_hash::PerfectHashIndex;

#[cfg(feature = "python")]
mod python;

/// Entry points for the fuzz targets in `fuzz/`, and **not public API**: the `fuzzing` feature is
/// off by default and this module may change or vanish in any release.
///
/// What it exposes is the framing half of the blob loaders — the parsers that validate magic,
/// lengths, checksums, the side table and the fingerprint range before the MPH region is read.
/// Those carry the branches arbitrary bytes actually reach, and they are `pub(crate)`; a libFuzzer
/// target lives in its own crate and cannot see them.
#[cfg(all(feature = "fuzzing", feature = "mph"))]
#[doc(hidden)]
pub mod fuzzing {
    /// Parse the framing of a `CompactHashIndex` blob; `true` if it was accepted. The verdict is
    /// not the point — not panicking, hanging or reading out of bounds is.
    pub fn parse_compact_frame(bytes: &[u8], verify: bool) -> bool {
        crate::CompactHashIndex::fuzz_parse_frame(bytes, verify)
    }

    /// [`parse_compact_frame`] for a `ClosedHashIndex` blob, the framing with the fewest fields:
    /// no fingerprint table, and the payload checksum always verified.
    pub fn parse_closed_frame(bytes: &[u8]) -> bool {
        crate::ClosedHashIndex::fuzz_parse_frame(bytes)
    }

    /// Load a `DictIndex` blob and query what loaded; `true` if it loaded. Its loader checks
    /// the framing and the three per-block arrays, but the front-coded block data is read as the
    /// queries reach it, each access bounded — so this target queries: ids inside `[0, n)`,
    /// `lower_bound` at most `n`, `key` `None` past the end, a walk that ends.
    pub fn load_dict(bytes: &[u8]) -> bool {
        crate::DictIndex::fuzz_load_and_query(bytes)
    }

    /// [`parse_compact_frame`] for a `PerfectHashIndex` blob, whose framing also has to validate an
    /// arena of offsets.
    pub fn parse_perfect_frame(bytes: &[u8], verify: bool) -> bool {
        crate::PerfectHashIndex::fuzz_parse_frame(bytes, verify)
    }

    /// Load a standalone `MPH2` (or 1.0's `MPH1`) blob and query the table it produced.
    ///
    /// The other targets reach this format only through a `BMP7`/`BCH7` header, which means a
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

    /// [`parse_overlay_frame`] with the embedded base parsed too, by the loader a caller who does
    /// not trust the blob has to use.
    ///
    /// The seam is the one neither other target reaches: the overlay derives the base region's
    /// bounds from its own header and hands that slice to a closure, so a blob can be framed
    /// perfectly and still deliver a *mis-sized* or hostile base. `OVL1` is what makes this
    /// fuzzable at all — it carries no checksums, so a mutation reaches the framing instead of
    /// dying at a hash it cannot recompute.
    ///
    /// Same claim as [`parse_string`]: **no input may panic out of this function**. The
    /// contained panic `from_untrusted_bytes` catches is why this target, like that one, has to
    /// replace libfuzzer-sys's aborting hook.
    pub fn parse_overlay_untrusted(bytes: &[u8]) -> bool {
        crate::Overlay::<crate::StringIndex>::from_bytes_with(
            bytes,
            crate::StringIndex::from_untrusted_bytes,
        )
        .is_ok()
    }

    /// Load an ordered blob the way a caller who does not trust it would, and query what comes
    /// back.
    ///
    /// This is the target `fuzz/Cargo.toml` used to say could not exist. It could not while the
    /// only loader was `StringIndex::from_bytes`, which is allowed to panic on a crafted FST — a
    /// target over it re-found that every week and taught us to ignore a red job.
    /// `from_untrusted_bytes` makes the panic an `Err`, and that is a claim worth a fuzzer:
    /// **no input may panic out of this function**, and any index that does load must answer its
    /// own keys with their own ranks and decode each of them back, because the loader checked
    /// every node's outputs and bytes to say so.
    pub fn parse_string(bytes: &[u8]) -> bool {
        let Ok(idx) = crate::StringIndex::from_untrusted_bytes(bytes) else {
            return false;
        };
        let mut n = 0u64;
        for (rank, (key, id)) in idx.iter().enumerate() {
            assert_eq!(id, rank as u64, "loaded index answers {key:?} with {id}");
            assert_eq!(
                idx.id(&key),
                Some(id),
                "point lookup disagrees with the scan"
            );
            assert_eq!(
                idx.key(id).as_deref(),
                Some(key.as_str()),
                "reverse lookup disagrees with the scan"
            );
            n += 1;
        }
        assert_eq!(n, idx.len() as u64, "the scan and the length disagree");
        true
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
}
