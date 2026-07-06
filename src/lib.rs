//! lexindex: compact, immutable string↔id indexes for huge catalogs.
//!
//! Three complementary, build-once / query-many indexes over a set of strings (entity names, cluster
//! labels, document keys, vocabulary terms):
//!
//! - [`StringIndex`] — an **ordered** index backed by a finite-state transducer ([`fst`]). Exact
//!   `string → id` and `id → string` (the reverse reconstructed from the FST by a rank-walk, no stored
//!   map), plus **prefix**, **range**, **fuzzy** (Levenshtein), and **subsequence** iteration
//!   (automaton-driven, no full scan). Use it for autocomplete / fuzzy search / ordered scans.
//! - [`CompactHashIndex`] — the **smallest** `string → dense id` map: a minimal perfect hash
//!   ([`ptr_hash`], the `mph` feature) plus a small fingerprint per key, storing no keys. ~1.3 B/key,
//!   at the cost of probabilistic membership and no reverse lookup. Use it when footprint is paramount.
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

// The crate docs above link `PerfectHashIndex` / `CompactHashIndex` / `ptr_hash`, which exist only under
// the default `mph` feature. docs.rs builds with default features, where the links resolve; on an
// `fst`-only build they can't, so silence the broken-link lint just there rather than downgrade the
// links to plain code spans.
#![cfg_attr(not(feature = "mph"), allow(rustdoc::broken_intra_doc_links))]

mod blob;
mod string_index;

pub use string_index::StringIndex;

// The minimal-perfect-hash indexes (`PerfectHashIndex`, `CompactHashIndex`) and their shared key hash
// and arena live behind the `mph` feature; `StringIndex` reconstructs `id → key` from the FST itself
// and needs none of them.
#[cfg(feature = "mph")]
mod arena;
#[cfg(feature = "mph")]
mod compact_hash;
#[cfg(feature = "mph")]
mod hash;
#[cfg(feature = "mph")]
mod perfect_hash;
#[cfg(feature = "mph")]
pub use compact_hash::CompactHashIndex;
#[cfg(feature = "mph")]
pub use perfect_hash::PerfectHashIndex;

#[cfg(feature = "python")]
mod python;

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
