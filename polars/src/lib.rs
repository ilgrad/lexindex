//! A Polars expression plugin over lexindex.
//!
//! Four expressions — `id`, `id_unchecked`, `contains` and `key` — each naming an index blob by
//! path. The engine calls them per chunk, on its own threads, with no GIL held; this crate writes
//! no `unsafe` of its own.
//!
//! # The blob is read once
//!
//! A path is opened on first use and the whole blob read into memory, then kept in a process-wide
//! cache keyed by path and generation: size, modification time and, on unix, device and inode. Every
//! later chunk — and every thread — shares that one `Arc`, because an index is immutable once
//! loaded. No file stays mapped: a mapping's contract is that nothing writes the file, which a
//! plugin cannot promise about a file any process may rebuild.
//!
//! # One query, one generation
//!
//! An expression carries the generation it saw when it was built, and a chunk gets exactly that one:
//! from the cache without touching the file, or from the file while it still is that generation, or
//! a `ComputeError`. Ids from two generations of an index are not comparable, so a rebuild during a
//! query must not reach only its later chunks. Loading a new generation drops the old one for that
//! path, so the obvious loop does not hold every generation in memory, and a query still running on
//! the old one then fails rather than mixes. `pin=False` asks for whatever the file holds when a
//! chunk runs, and a file absent when the expression was built is read the same way.
//!
//! # The cache
//!
//! The threads of a first query wait for one read rather than each making their own. The cache
//! holds one generation a path, and any number of paths unless `LEXINDEX_POLARS_CACHE_BYTES` names
//! a limit, past which the least recently used blobs go.
//!
//! # What a kind cannot answer
//!
//! The six index types do not all answer all four questions: a keyless hash has no `key`, a closed
//! hash cannot tell membership, and only the three hash-backed kinds have an unchecked `id`. Those
//! are `ComputeError`s naming the kind and the blob, raised when the expression runs.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::UNIX_EPOCH;

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

/// What every one of the four expressions takes: the blob to answer from, and the generation of it
/// the expression saw when it was built. `None` where the file was absent then or `pin=False`.
#[derive(Deserialize)]
struct Args {
    path: String,
    pin: Option<Generation>,
}

/// Which file a path held. A rename brings a new inode even where the length and the timestamp
/// are the same, and rename is how a blob is replaced atomically.
///
/// `dev` and `ino` are the bit patterns as signed, because an expression's kwargs arrive pickled and
/// the decoder refuses an integer past `i64::MAX`, where an inode can lie (overlayfs keeps a layer's
/// number in its top bits). They are only ever compared.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
struct Generation {
    len: u64,
    mtime_ns: i64,
    dev: i64,
    ino: i64,
}

impl Generation {
    fn of(meta: &fs::Metadata) -> Self {
        let mtime_ns = meta
            .modified()
            .map_or(0, |at| match at.duration_since(UNIX_EPOCH) {
                Ok(after) => after.as_nanos() as i64,
                Err(before) => -(before.duration().as_nanos() as i64),
            });
        #[cfg(unix)]
        let (dev, ino) = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev() as i64, meta.ino() as i64)
        };
        // std has no stable file identity elsewhere; length and timestamp are what is left there.
        #[cfg(not(unix))]
        let (dev, ino) = (0, 0);
        Self {
            len: meta.len(),
            mtime_ns,
            dev,
            ino,
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    generation: Generation,
}

/// One cached blob, and when it was last asked for.
struct Entry {
    index: Arc<Index>,
    bytes: u64,
    used: AtomicU64,
}

static CACHE: LazyLock<RwLock<HashMap<CacheKey, Entry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
/// Held across a read and a parse, so that the threads of a first query read a blob once rather
/// than each hold a copy of it at the same moment. One lock for every path: a load waits for a load
/// of another file and a cache hit waits for nothing, and a lock a path is more code for a rare case.
static LOADING: Mutex<()> = Mutex::new(());
/// Ticks handed to `Entry::used`, each to one entry: the smallest is the least recently used.
static CLOCK: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static LOADS: AtomicU64 = AtomicU64::new(0);

const BUDGET_VAR: &str = "LEXINDEX_POLARS_CACHE_BYTES";

/// The bytes of blobs the cache may hold, if the environment names a limit. Read at every load
/// rather than once, so a process can set it after the plugin is first used.
fn budget() -> PolarsResult<Option<u64>> {
    let Some(raw) = std::env::var_os(BUDGET_VAR) else {
        return Ok(None);
    };
    raw.to_str()
        .and_then(|text| text.trim().parse().ok())
        .map(Some)
        .ok_or_else(|| polars_err!(ComputeError: "lexindex: {BUDGET_VAR} must be a number of bytes, not {raw:?}"))
}

fn cached(key: &CacheKey) -> Option<Arc<Index>> {
    let cache = read_cache();
    let entry = cache.get(key)?;
    entry
        .used
        .store(CLOCK.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
    HITS.fetch_add(1, Ordering::Relaxed);
    Some(Arc::clone(&entry.index))
}

/// Takes a freshly read blob in, and lets the least recently used go until `budget` holds. The
/// newest always stays, whatever its size: a chunk's own `Arc` keeps an evicted index alive to the
/// end of that chunk, so eviction can cost a later chunk a read and never a wrong answer.
fn remember(key: CacheKey, index: Arc<Index>, bytes: u64, budget: Option<u64>) {
    let mut cache = write_cache();
    // A rebuilt blob at the same path replaces its entry rather than joining it, so the obvious
    // loop -- build, query, rebuild, query -- does not hold every generation in memory.
    cache.retain(|k, _| k.path != key.path);
    let used = AtomicU64::new(CLOCK.fetch_add(1, Ordering::Relaxed));
    cache.insert(key, Entry { index, bytes, used });
    let Some(budget) = budget else {
        return;
    };
    let mut held: u64 = cache.values().map(|entry| entry.bytes).sum();
    // A scan an eviction: a process holds a handful of indexes, too few for a linked list to pay.
    while held > budget && cache.len() > 1 {
        let Some(oldest) = cache
            .values()
            .map(|entry| entry.used.load(Ordering::Relaxed))
            .min()
        else {
            break;
        };
        cache.retain(|_, entry| {
            let keep = entry.used.load(Ordering::Relaxed) != oldest;
            if !keep {
                held -= entry.bytes;
            }
            keep
        });
    }
}

fn parse(bytes: &[u8], path: &Path) -> PolarsResult<Index> {
    let fail = |e: lexindex::IndexError| polars_err!(ComputeError: "lexindex: {} is not a readable index: {e}", path.display());
    let kind = lexindex::inspect(bytes).map_err(fail)?.kind;
    Ok(match kind {
        // A file named by a query may be anyone's, and `from_bytes` can panic on a crafted transducer.
        // The walk this adds is paid once a generation.
        BlobKind::StringIndex => {
            Index::String(StringIndex::from_untrusted_bytes(bytes).map_err(fail)?)
        }
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
    // A pinned chunk asks for a generation and not for whatever the file holds now, so a hit touches
    // no file: a rebuild during the query changes nothing the query reads.
    if let Some(generation) = args.pin
        && let Some(hit) = cached(&CacheKey {
            path: path.clone(),
            generation,
        })
    {
        return Ok(hit);
    }

    // One open file answers both questions below. A stat and a separate read by path could file a
    // new blob under the old one's key if the path were replaced between the two.
    let mut file = File::open(&path)
        .map_err(|e| polars_err!(ComputeError: "lexindex: cannot open {}: {e}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| polars_err!(ComputeError: "lexindex: cannot open {}: {e}", path.display()))?;
    let generation = Generation::of(&meta);
    if args.pin.is_some_and(|pin| pin != generation) {
        polars_bail!(
            ComputeError: "lexindex: {} was rebuilt since this expression was built, and the blob it saw is not cached any more -- build the expression again, or pass pin=False to read whatever the file holds when a chunk runs",
            path.display(),
        );
    }
    let key = CacheKey {
        path: path.clone(),
        generation,
    };

    if let Some(hit) = cached(&key) {
        return Ok(hit);
    }

    let _loading = LOADING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = cached(&key) {
        return Ok(hit);
    }
    let budget = budget()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| polars_err!(ComputeError: "lexindex: cannot read {}: {e}", path.display()))?;
    let index = Arc::new(parse(&bytes, &path)?);
    LOADS.fetch_add(1, Ordering::Relaxed);
    remember(key, Arc::clone(&index), bytes.len() as u64, budget);
    Ok(index)
}

// Nothing here panics while the lock is held, so the poison can only come from elsewhere; taking
// the map back is still better than propagating a panic into the engine's worker thread.
fn read_cache() -> std::sync::RwLockReadGuard<'static, HashMap<CacheKey, Entry>> {
    CACHE.read().unwrap_or_else(|e| e.into_inner())
}

fn write_cache() -> std::sync::RwLockWriteGuard<'static, HashMap<CacheKey, Entry>> {
    CACHE.write().unwrap_or_else(|e| e.into_inner())
}

/// What `lexindex_polars.cache_info` reads, as one column in the order it names them: entries,
/// bytes, hits, loads. An expression because the engine's dlopen is the only way into this library:
/// it has no Python module, and a C entry point of its own would be this crate's first `unsafe`.
#[polars_expr(output_type=UInt64)]
fn lexindex_cache_info(_inputs: &[Series]) -> PolarsResult<Series> {
    let cache = read_cache();
    let bytes = cache.values().map(|entry| entry.bytes).sum();
    let info = [
        cache.len() as u64,
        bytes,
        HITS.load(Ordering::Relaxed),
        LOADS.load(Ordering::Relaxed),
    ];
    Ok(UInt64Chunked::from_slice("info".into(), &info).into_series())
}

/// Drops every cached blob for `lexindex_polars.clear_cache`. A chunk running now keeps its `Arc`.
#[polars_expr(output_type=Boolean)]
fn lexindex_clear_cache(_inputs: &[Series]) -> PolarsResult<Series> {
    write_cache().clear();
    Ok(BooleanChunked::from_slice("cleared".into(), &[true]).into_series())
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
