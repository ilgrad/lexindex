//! A Polars expression plugin over lexindex.
//!
//! Four expressions — `id`, `id_unchecked`, `contains` and `key` — each naming an index blob by
//! path. The engine calls them per chunk, on its own threads, with no GIL held; this crate writes
//! no `unsafe` of its own.
//!
//! # The blob is read once
//!
//! A path is opened on first use and the whole blob read into memory, then kept in a process-wide
//! cache keyed by path, size and modification time. Every later chunk — and every thread — shares
//! that one `Arc`, because an index is immutable once loaded. Rewriting the file changes the key,
//! so the next chunk loads the new blob and the old entry for that path is dropped; a rewrite
//! *during* a query is therefore visible to later chunks and not to earlier ones, which is why an
//! index a query reads should not be rebuilt under it.
//!
//! # What a kind cannot answer
//!
//! The six index types do not all answer all four questions: a keyless hash has no `key`, a closed
//! hash cannot tell membership, and only the three hash-backed kinds have an unchecked `id`. Those
//! are `ComputeError`s naming the kind and the blob, raised when the expression runs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::SystemTime;

use lexindex::{
    BlobKind, ClosedHashIndex, CompactHashIndex, DictIndex, HashedDictIndex, PerfectHashIndex,
    StringIndex,
};
use polars::prelude::*;
use pyo3_polars::derive::polars_expr;
use serde::Deserialize;

/// One loaded index, whichever kind the blob turned out to hold.
enum Index {
    String(StringIndex),
    Dict(DictIndex),
    Compact(CompactHashIndex),
    Closed(ClosedHashIndex),
    Perfect(PerfectHashIndex),
    HashedDict(HashedDictIndex),
}

impl Index {
    /// What to call this kind in an error message.
    fn kind(&self) -> &'static str {
        match self {
            Index::String(_) => "StringIndex",
            Index::Dict(_) => "DictIndex",
            Index::Compact(_) => "CompactHashIndex",
            Index::Closed(_) => "ClosedHashIndex",
            Index::Perfect(_) => "PerfectHashIndex",
            Index::HashedDict(_) => "HashedDictIndex",
        }
    }

    fn id(&self, key: &str) -> Option<u64> {
        match self {
            Index::String(i) => i.id(key),
            Index::Dict(i) => i.id(key),
            Index::Compact(i) => i.id(key).map(u64::from),
            Index::Closed(i) => Some(u64::from(i.id(key))),
            Index::Perfect(i) => i.id(key).map(u64::from),
            Index::HashedDict(i) => i.id(key),
        }
    }

    /// The three hash-backed kinds skip the membership check; a search cannot.
    fn has_unchecked(&self) -> bool {
        matches!(
            self,
            Index::Compact(_) | Index::Closed(_) | Index::Perfect(_) | Index::HashedDict(_)
        )
    }

    fn id_unchecked(&self, key: &str) -> u64 {
        match self {
            Index::Compact(i) => u64::from(i.id_unchecked(key)),
            Index::Closed(i) => u64::from(i.id(key)),
            Index::Perfect(i) => u64::from(i.id_unchecked(key)),
            Index::HashedDict(i) => i.id_unchecked(key),
            // Unreachable: `has_unchecked` gates every call. `u64::MAX` rather than a plausible
            // id, so a future caller that skips the gate gets an answer it cannot mistake for one.
            Index::String(_) | Index::Dict(_) => u64::MAX,
        }
    }

    /// A closed hash answers every key with an id and has nothing to compare against.
    fn has_membership(&self) -> bool {
        !matches!(self, Index::Closed(_))
    }

    fn contains(&self, key: &str) -> bool {
        match self {
            Index::String(i) => i.contains(key),
            Index::Dict(i) => i.contains(key),
            Index::Compact(i) => i.contains(key),
            Index::Perfect(i) => i.contains(key),
            Index::HashedDict(i) => i.contains(key),
            Index::Closed(_) => false,
        }
    }

    /// The two fingerprint kinds keep no keys, so they have no reverse lookup.
    fn stores_keys(&self) -> bool {
        !matches!(self, Index::Compact(_) | Index::Closed(_))
    }

    fn key(&self, id: u64) -> Option<String> {
        match self {
            Index::String(i) => i.key(id),
            Index::Dict(i) => i.key(id),
            Index::HashedDict(i) => i.dict().key(id),
            Index::Perfect(i) => u32::try_from(id)
                .ok()
                .and_then(|id| i.key(id))
                .map(str::to_owned),
            Index::Compact(_) | Index::Closed(_) => None,
        }
    }
}

/// What every one of the four expressions takes: the blob to answer from.
#[derive(Deserialize)]
struct Args {
    path: String,
}

#[derive(PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

static CACHE: LazyLock<RwLock<HashMap<CacheKey, Arc<Index>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn parse(bytes: &[u8], path: &Path) -> PolarsResult<Index> {
    let fail = |e: lexindex::IndexError| polars_err!(ComputeError: "lexindex: {} is not a readable index: {e}", path.display());
    let kind = lexindex::inspect(bytes).map_err(fail)?.kind;
    Ok(match kind {
        BlobKind::StringIndex => Index::String(StringIndex::from_bytes(bytes).map_err(fail)?),
        BlobKind::DictIndex => Index::Dict(DictIndex::from_bytes(bytes).map_err(fail)?),
        BlobKind::CompactHashIndex => {
            Index::Compact(CompactHashIndex::from_bytes(bytes).map_err(fail)?)
        }
        BlobKind::ClosedHashIndex => {
            Index::Closed(ClosedHashIndex::from_bytes(bytes).map_err(fail)?)
        }
        BlobKind::PerfectHashIndex => {
            Index::Perfect(PerfectHashIndex::from_bytes(bytes).map_err(fail)?)
        }
        BlobKind::HashedDictIndex => {
            Index::HashedDict(HashedDictIndex::from_bytes(bytes).map_err(fail)?)
        }
        BlobKind::Mphf => {
            polars_bail!(ComputeError: "lexindex: {} is a standalone perfect hash, not an index", path.display())
        }
        BlobKind::Overlay => {
            polars_bail!(ComputeError: "lexindex: {} is an overlay, which this plugin does not serve", path.display())
        }
        // `BlobKind` is `#[non_exhaustive]`: a kind added by a later lexindex is a blob this build
        // has no loader for, and saying so beats guessing at it.
        _ => {
            polars_bail!(ComputeError: "lexindex: {} holds a kind this plugin does not know ({kind:?})", path.display())
        }
    })
}

/// The index for this path, from the cache or read into it.
fn index_for(args: &Args) -> PolarsResult<Arc<Index>> {
    let path = PathBuf::from(&args.path);
    let meta = fs::metadata(&path)
        .map_err(|e| polars_err!(ComputeError: "lexindex: cannot open {}: {e}", path.display()))?;
    let key = CacheKey {
        path: path.clone(),
        len: meta.len(),
        modified: meta.modified().ok(),
    };

    if let Some(hit) = read_cache().get(&key) {
        return Ok(Arc::clone(hit));
    }

    let bytes = fs::read(&path)
        .map_err(|e| polars_err!(ComputeError: "lexindex: cannot read {}: {e}", path.display()))?;
    let index = Arc::new(parse(&bytes, &path)?);

    let mut cache = write_cache();
    // A rebuilt blob at the same path replaces its entry rather than joining it, so the obvious
    // loop -- build, query, rebuild, query -- does not hold every generation in memory.
    cache.retain(|k, _| k.path != path);
    cache.insert(key, Arc::clone(&index));
    Ok(index)
}

// Nothing here panics while the lock is held, so the poison can only come from elsewhere; taking
// the map back is still better than propagating a panic into the engine's worker thread.
fn read_cache() -> std::sync::RwLockReadGuard<'static, HashMap<CacheKey, Arc<Index>>> {
    CACHE.read().unwrap_or_else(|e| e.into_inner())
}

fn write_cache() -> std::sync::RwLockWriteGuard<'static, HashMap<CacheKey, Arc<Index>>> {
    CACHE.write().unwrap_or_else(|e| e.into_inner())
}

/// The keys of a string column, or an error naming the type it got instead.
fn keys(inputs: &[Series]) -> PolarsResult<&StringChunked> {
    inputs[0].str().map_err(
        |_| polars_err!(SchemaMismatch: "lexindex: expected a string column, got {}", inputs[0].dtype()),
    )
}

/// The id of every key, null where the index does not hold it — and null in, null out.
#[polars_expr(output_type=UInt64)]
fn lexindex_id(inputs: &[Series], kwargs: Args) -> PolarsResult<Series> {
    let index = index_for(&kwargs)?;
    let ca = keys(inputs)?;
    let out: UInt64Chunked = ca.iter().map(|key| key.and_then(|k| index.id(k))).collect();
    Ok(out.with_name(ca.name().clone()).into_series())
}

/// The id of every key without checking membership: a stranger gets some id below the key count.
#[polars_expr(output_type=UInt64)]
fn lexindex_id_unchecked(inputs: &[Series], kwargs: Args) -> PolarsResult<Series> {
    let index = index_for(&kwargs)?;
    let ca = keys(inputs)?;
    if !index.has_unchecked() {
        polars_bail!(
            ComputeError: "lexindex: a {} has no unchecked lookup -- use `id`, which is exact for it ({})",
            index.kind(), kwargs.path,
        );
    }
    let out: UInt64Chunked = ca
        .iter()
        .map(|key| key.map(|k| index.id_unchecked(k)))
        .collect();
    Ok(out.with_name(ca.name().clone()).into_series())
}

/// Whether the index holds the key. Probabilistic on the fingerprint kinds, exact on the rest.
#[polars_expr(output_type=Boolean)]
fn lexindex_contains(inputs: &[Series], kwargs: Args) -> PolarsResult<Series> {
    let index = index_for(&kwargs)?;
    let ca = keys(inputs)?;
    if !index.has_membership() {
        polars_bail!(
            ComputeError: "lexindex: a {} cannot tell membership -- every key has an id ({})",
            index.kind(), kwargs.path,
        );
    }
    let out: BooleanChunked = ca
        .iter()
        .map(|key| key.map(|k| index.contains(k)))
        .collect();
    Ok(out.with_name(ca.name().clone()).into_series())
}

/// The key at every id, null where the index has no such id.
#[polars_expr(output_type=String)]
fn lexindex_key(inputs: &[Series], kwargs: Args) -> PolarsResult<Series> {
    let index = index_for(&kwargs)?;
    if !index.stores_keys() {
        polars_bail!(
            ComputeError: "lexindex: a {} stores no keys, so it has no reverse lookup ({})",
            index.kind(), kwargs.path,
        );
    }
    let ids = inputs[0].cast(&DataType::UInt64).map_err(
        |_| polars_err!(SchemaMismatch: "lexindex: expected an integer id column, got {}", inputs[0].dtype()),
    )?;
    let ca = ids.u64()?;
    let out: StringChunked = ca
        .iter()
        .map(|id| id.and_then(|id| index.key(id)))
        .collect();
    Ok(out.with_name(inputs[0].name().clone()).into_series())
}
