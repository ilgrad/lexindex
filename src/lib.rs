//! betula-index: compact, immutable string↔id indexes for huge catalogs.
//!
//! Two complementary, build-once / query-many indexes over a set of strings (entity names, cluster
//! labels, document keys, vocabulary terms):
//!
//! - [`StringIndex`] — an **ordered** index backed by a finite-state transducer ([`fst`]). Exact
//!   `string → id` and `id → string`, plus **prefix** and **range** iteration, in a compressed,
//!   serialisable form. Use it for autocomplete / browse / ordered scans of a large catalog.
//! - [`PerfectHashIndex`] — a **minimal-perfect-hash** dictionary backed by [`ptr_hash`] (the `mph`
//!   feature, on by default). Fastest exact `string → dense id` lookup with verified membership; no
//!   ordering. Use it as a fixed-vocabulary token↔id map on a hot path.
//!
//! Both assign dense ids in `[0, n)` and support reverse lookup. Neither is mutable after building —
//! they are immutable summaries, like the clustering features in the companion `betula-cluster` crate.
//!
//! ```
//! use betula_index::StringIndex;
//! let idx = StringIndex::build(["apple", "apricot", "banana"]).unwrap();
//! assert_eq!(idx.id("banana"), Some(2));
//! assert_eq!(idx.key(0), Some("apple"));
//! assert_eq!(idx.prefix("ap").len(), 2);
//! ```

mod arena;
mod string_index;

pub use string_index::StringIndex;

#[cfg(feature = "mph")]
mod perfect_hash;
#[cfg(feature = "mph")]
pub use perfect_hash::PerfectHashIndex;

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
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::Fst(e) => write!(f, "fst error: {e}"),
            IndexError::Io(e) => write!(f, "io error: {e}"),
            IndexError::Format(m) => write!(f, "format error: {m}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Fst(e) => Some(e),
            IndexError::Io(e) => Some(e),
            IndexError::Format(_) => None,
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
