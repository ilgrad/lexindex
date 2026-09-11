//! PyO3 bindings: expose [`StringIndex`] and [`PerfectHashIndex`] to Python as `lexindex._core`.
//!
//! Thin wrappers over the Rust types — every method delegates to the core and maps [`IndexError`] to a
//! Python exception. Built as an abi3 extension (CPython ≥ 3.11) under the `python` feature.
//!
//! # Releasing the GIL
//!
//! Building, bulk queries, batch lookups and persistence run under [`Python::detach`], so a
//! threaded caller (a web worker, say) keeps making progress while a large index is built or
//! searched. This is sound because all three core types are `Send + Sync`, and it stays sound
//! without a separate assertion: each closure captures `&self`, so if a type ever lost `Sync` the
//! reference would stop being `Send` and this module would fail to compile.
//!
//! Single-key accessors (`id`, `key`, `contains`, `id_unchecked`, `successor`, `predecessor`,
//! `__len__`) deliberately do **not** release it: they take well under a microsecond, and dropping
//! and reacquiring the GIL would cost more than the work it protects.
//!
//! # Threads
//!
//! Every class here is `#[pyclass(frozen)]`, which is what makes the module's `gil_used = false`
//! declaration hold and keeps it holding: `frozen` rejects a `&mut self` method at compile time
//! (`type mismatch resolving <T as PyClass>::Frozen == False`), and a `&mut self` method is exactly
//! what raised `RuntimeError: Already borrowed` in seven threads out of eight on CPython 3.14t
//! before the iterator and [`PyOverlay`] moved their state behind a lock. The invariant is in the
//! type system rather than in a review checklist, so it cannot be given up by accident.
//!
//! # Borrowing the caller's strings
//!
//! Every method taking many keys (the constructors and `ids_of`) reads them as [`PyBackedStr`], a
//! view into the Python `str`, instead of copying each one into a `String`. `build` already copies
//! the keys it keeps, so the owned `Vec<String>` in between was pure overhead — it cost a third of
//! a build's peak RSS on the 479 823-word dictionary.
//!
//! [`PyBackedStr`] is `Send + Sync` (the string it views is immutable), so it crosses into
//! [`Python::detach`] like the rest; the `Vec` is only borrowed there, so the Python references are
//! released with the GIL held.

use crate::{DictIndex, IndexError, Overlay, StringIndex};
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::{PyBufferError, PyIOError, PyKeyError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedStr;
use pyo3::sync::MutexExt;
use pyo3::types::{PyBytes, PyDict, PyIterator, PyMemoryView, PyString, PyType};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

#[cfg(feature = "mph")]
use crate::{ClosedHashIndex, CompactHashIndex, PerfectHashIndex};

/// Collect any Python iterable of `str` — list, tuple, generator, an open file — into borrowed
/// strings. `Vec<PyBackedStr>` as a parameter would accept only sequences, which rules out building
/// straight from a generator over a large corpus.
fn collect_strs(items: &Bound<'_, PyAny>) -> PyResult<Vec<PyBackedStr>> {
    let mut out = Vec::with_capacity(items.len().unwrap_or(0));
    for item in items.try_iter()? {
        out.push(item?.extract()?);
    }
    Ok(out)
}

/// Adapt a Python iterable of `str` into a lazy Rust iterator of owned `String`.
///
/// Unlike [`collect_strs`] nothing accumulates: each key is decoded, handed to the builder and
/// dropped, which is the whole point of the sorted builds below — a Python list of 10 M keys is
/// 1 076 MB of `str` objects before any index exists.
///
/// A Python-level failure (a non-string item, an exception inside a generator) cannot travel
/// through `Iterator::next`, so it is parked in the shared cell and the iteration stops. That makes
/// a raising iterable indistinguishable from one that ended, which is why every caller must consult
/// the cell **before** acting on the result — for a build that writes a file, before the file is
/// published, not after.
fn stream_strs<'py>(
    mut it: Bound<'py, PyIterator>,
    err: Rc<RefCell<Option<PyErr>>>,
) -> impl Iterator<Item = String> + 'py {
    std::iter::from_fn(move || {
        if err.borrow().is_some() {
            return None;
        }
        let stop = |e: PyErr| {
            *err.borrow_mut() = Some(e);
            None
        };
        match it.next()? {
            Ok(obj) => match obj.extract::<String>() {
                Ok(s) => Some(s),
                Err(e) => stop(e),
            },
            Err(e) => stop(e),
        }
    })
}

/// Hash any Python iterable of `str` down to `CompactHashIndex` build pairs — 16 bytes per key,
/// after which the string is dropped, so building from a generator over a huge corpus never
/// materialises the strings on this side either (the other indexes must keep them: they store
/// keys). The hashing itself runs a chunk at a time with the GIL **released**, so other Python
/// threads keep running through what is otherwise a long CPU-bound stretch; only pulling the next
/// chunk out of the iterator (and dropping the previous one, which decrements refcounts) holds it.
#[cfg(feature = "mph")]
fn collect_pairs(items: &Bound<'_, PyAny>) -> PyResult<Vec<(u64, u64)>> {
    const CHUNK: usize = 4096;
    let py = items.py();
    let mut out = Vec::with_capacity(items.len().unwrap_or(0));
    let mut chunk: Vec<PyBackedStr> = Vec::with_capacity(CHUNK);
    let mut it = items.try_iter()?;
    loop {
        chunk.clear();
        for item in it.by_ref().take(CHUNK) {
            chunk.push(item?.extract()?);
        }
        if chunk.is_empty() {
            return Ok(out);
        }
        py.detach(|| out.extend(chunk.iter().map(|s| crate::hash::hash_pair(s))));
    }
}

fn to_py(e: IndexError) -> PyErr {
    match e {
        IndexError::Io(_) => PyIOError::new_err(e.to_string()),
        _ => PyValueError::new_err(e.to_string()),
    }
}

/// Ordered string↔id index (FST) with prefix / range / fuzzy / subsequence queries.
#[pyclass(name = "StringIndex", module = "lexindex._core", frozen)]
pub struct PyStringIndex {
    inner: Arc<StringIndex>,
}

#[pymethods]
impl PyStringIndex {
    /// Build from an iterable of strings (duplicates removed; ids are sorted rank).
    #[new]
    fn new(py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<Self> {
        let items = collect_strs(items)?;
        let inner = py
            .detach(|| StringIndex::build(items.iter()))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Build from an iterable of strings that is **already in ascending byte order**, without
    /// materialising it — the constructor has to hold the whole corpus to sort it, this does not.
    /// Adjacent duplicates are dropped exactly as the constructor drops them after sorting; input
    /// that is not ascending raises `ValueError` rather than producing an index that answers wrongly.
    ///
    /// Keys composed from sorted parts are not themselves sorted unless the separator is below
    /// every byte that can follow a part — joining a sorted word list to itself with `"."` puts
    /// `'tween-decks.&c` before `'tween.ARU`, because `-` is below `.`.
    #[staticmethod]
    fn from_sorted(items: &Bound<'_, PyAny>) -> PyResult<Self> {
        let err = Rc::new(RefCell::new(None));
        let built = StringIndex::build_sorted(stream_strs(items.try_iter()?, Rc::clone(&err)));
        // Checked after the build rather than during it: nothing is published either way, and a
        // discarded partial index is not observable.
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        Ok(Self {
            inner: Arc::new(built.map_err(to_py)?),
        })
    }

    /// The constructor for a corpus that does not fit in memory, written straight to `path`: the
    /// keys, in any order, are sorted in runs that spill beside the output and merged into the
    /// transducer, so neither the corpus nor the index is ever held whole. Returns the number of
    /// distinct keys written.
    #[staticmethod]
    fn build_to_file(items: &Bound<'_, PyAny>, path: PathBuf) -> PyResult<usize> {
        let err = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&err);
        let written = StringIndex::build_to_file_checked(
            stream_strs(items.try_iter()?, Rc::clone(&err)),
            &path,
            move || {
                if seen.borrow().is_some() {
                    Err(crate::IndexError::Format(
                        "string-index: the input iterable raised before it ended",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        written.map_err(to_py)
    }

    /// [`from_sorted`] streamed straight to `path`, so neither the corpus nor the finished index
    /// has to fit in memory. Returns the number of keys written.
    #[staticmethod]
    fn build_sorted_to_file(items: &Bound<'_, PyAny>, path: PathBuf) -> PyResult<usize> {
        let err = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&err);
        // The check runs inside the atomic write, so an iterable that raises halfway aborts the
        // build with `path` untouched instead of publishing a truncated index and reporting the
        // error afterwards.
        let written = StringIndex::build_sorted_to_file_checked(
            stream_strs(items.try_iter()?, Rc::clone(&err)),
            &path,
            move || {
                if seen.borrow().is_some() {
                    Err(crate::IndexError::Format(
                        "string-index: the input iterable raised before it ended",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        written.map_err(to_py)
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Id of `key`, or `None` if absent.
    fn id(&self, key: &str) -> Option<u64> {
        self.inner.id(key)
    }

    /// Whether `key` is present.
    fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Dense id of `key`, raising `KeyError` if it is absent — the dict spelling of
    /// [`id`](Self::id), for a caller who wants a miss to be an error rather than a `None` to
    /// check.
    ///
    /// There is no `__setitem__`, no `keys` / `values` / `items` and no `Mapping` registration:
    /// this is an immutable `str -> int` lookup, and pretending to be a mapping would promise
    /// iteration semantics it does not have.
    fn __getitem__(&self, key: &str) -> PyResult<u64> {
        self.inner
            .id(key)
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))
    }

    /// Dense id of `key`, or `default` (`None` unless given) — `idx.get("apple", -1)`.
    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.id(key) {
            Some(id) => Ok(id.into_pyobject(py)?.into_any()),
            None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
        }
    }

    /// Key for `id`, or `None` if out of range.
    fn key(&self, id: u64) -> Option<String> {
        self.inner.key(id)
    }

    /// Batched [`id`](Self::id): one call for many keys, looping in Rust to amortise the Python↔Rust
    /// boundary. Returns a list aligned with `keys`, `None` where a key is absent. (Named `ids_of`, not
    /// `ids`/`keys`, so the class is not mistaken for a mapping by `dict(index)`.)
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u64>> {
        py.detach(|| keys.iter().map(|k| self.inner.id(k)).collect())
    }

    /// Batched [`id`](Self::id) packed into a `bytes` buffer instead of a list, for callers who
    /// hand the result to `numpy` or `array` rather than reading it item by item.
    ///
    /// One `8`-byte native-endian item per key, aligned with `keys`, [`MISSING_ID`](Self::MISSING_ID)
    /// where a key is absent. `np.frombuffer(buf, dtype=index.ID_DTYPE)` shares the memory rather
    /// than copying it; `ids_of` has to build one Python `int` per key, which is what this avoids.
    ///
    /// Native endianness — unlike the blobs, which are little-endian everywhere — because the
    /// buffer is meant for `np.frombuffer` on the machine that produced it, not for the wire.
    fn ids_of_bytes<'py>(&self, py: Python<'py>, keys: Vec<PyBackedStr>) -> Bound<'py, PyBytes> {
        let packed = py.detach(|| {
            let mut out = Vec::with_capacity(keys.len() * 8);
            for k in &keys {
                out.extend_from_slice(&self.inner.id(k).unwrap_or(u64::MAX).to_ne_bytes());
            }
            out
        });
        PyBytes::new(py, &packed)
    }

    /// Batched [`id`](Self::id) over an Arrow `utf8` / `large_utf8` column — a pyarrow `Array`
    /// or `ChunkedArray`, a pandas column of `ArrowDtype`, a polars `Series` — packed like
    /// [`ids_of_bytes`](Self::ids_of_bytes): one [`ID_DTYPE`](Self::ID_DTYPE) item per element,
    /// [`MISSING_ID`](Self::MISSING_ID) for an absent key and for a null. The keys are read from the
    /// column's offset and data buffers, so no Python string exists per key — building and
    /// borrowing those was half to two thirds of what the list forms cost.
    fn ids_of_arrow<'py>(
        &self,
        py: Python<'py>,
        column: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let chunks = utf8_chunks(column)?;
        let ids = self.arrow_ids(py, &chunks)?;
        Ok(PyBytes::new(py, &packed(&ids, u64::to_ne_bytes)))
    }

    /// [`ids_of_arrow`](Self::ids_of_arrow) written into memory the caller owns, as
    /// [`ids_into`](Self::ids_into) does for a list: `out` is a writable C-contiguous buffer of
    /// [`ID_DTYPE`](Self::ID_DTYPE) items at least as long as the column.
    fn ids_into_arrow(
        &self,
        py: Python<'_>,
        column: &Bound<'_, PyAny>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let chunks = utf8_chunks(column)?;
        let n = total_len(&chunks);
        if n == 0 {
            return writable_buffer(out);
        }
        let sink = id_sink::<u64>(out, n)?;
        let ids = self.arrow_ids(py, &chunks)?;
        write_ids(py, &sink, &ids)
    }

    /// [`ids_of_bytes`](Self::ids_of_bytes) written into memory the caller owns instead of a fresh
    /// `bytes` per call: `out` is any writable C-contiguous buffer of [`ID_DTYPE`](Self::ID_DTYPE)
    /// items — `np.empty(len(keys), dtype=index.ID_DTYPE)` is the usual one — so a hot loop can
    /// reuse one array. The first `len(keys)` items are written; the rest are left as they were.
    /// With no keys nothing is written and `out` need only be a writable buffer.
    ///
    /// A read-only, strided or mistyped buffer is a `BufferError` (a `uint32` array handed to this
    /// index is refused rather than half-filled); one shorter than `keys` is a `ValueError`.
    fn ids_into(
        &self,
        py: Python<'_>,
        keys: Vec<PyBackedStr>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if keys.is_empty() {
            return writable_buffer(out);
        }
        let sink = id_sink::<u64>(out, keys.len())?;
        let ids: Vec<u64> = py.detach(|| {
            keys.iter()
                .map(|k| self.inner.id(k).unwrap_or(u64::MAX))
                .collect()
        });
        write_ids(py, &sink, &ids)
    }

    /// The `numpy` dtype of one [`ids_of_bytes`](Self::ids_of_bytes) item, so a caller can read the
    /// buffer without hardcoding a width that differs between the index types.
    #[classattr]
    const ID_DTYPE: &'static str = "uint64";

    /// The [`ids_of_bytes`](Self::ids_of_bytes) item standing for an absent key. Ids are ranks below
    /// `len()`, and a `StringIndex` cannot hold `u64::MAX` keys, so the sentinel is never an id.
    #[classattr]
    const MISSING_ID: u64 = u64::MAX;

    /// Batched [`key`](Self::key): one call for many ids. Returns a list aligned with `ids`, `None`
    /// where an id is out of range.
    fn keys_of(&self, py: Python<'_>, ids: Vec<u64>) -> Vec<Option<String>> {
        py.detach(|| ids.iter().map(|&i| self.inner.key(i)).collect())
    }

    /// `(key, id)` pairs whose key starts with `prefix`, lexicographically ordered. `limit` stops
    /// after that many matches, walking no further — what autocomplete wants, since it needs ten of
    /// them, not every match.
    #[pyo3(signature = (prefix, limit=None))]
    fn prefix(&self, py: Python<'_>, prefix: &str, limit: Option<usize>) -> Vec<(String, u64)> {
        py.detach(|| {
            let it = self.inner.prefix_iter(prefix);
            match limit {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        })
    }

    /// `(key, id)` pairs with `lo <= key < hi`, lexicographically ordered. `limit` stops after that
    /// many matches.
    #[pyo3(signature = (lo, hi, limit=None))]
    fn range(
        &self,
        py: Python<'_>,
        lo: &str,
        hi: &str,
        limit: Option<usize>,
    ) -> Vec<(String, u64)> {
        py.detach(|| {
            let it = self.inner.range_iter(lo, hi);
            match limit {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        })
    }

    /// How many keys sort below `query` -- equivalently the id of the first key `>= query`, and
    /// the position `query` would be inserted at. Defined for any string, present or not, and
    /// never larger than `len`.
    fn lower_bound(&self, query: &str) -> u64 {
        self.inner.lower_bound(query)
    }

    /// How many keys satisfy `lo <= key < hi`, without decoding any of them.
    fn range_count(&self, lo: &str, hi: &str) -> u64 {
        self.inner.range_count(lo, hi)
    }

    /// How many keys start with `prefix`.
    fn prefix_count(&self, prefix: &str) -> u64 {
        self.inner.prefix_count(prefix)
    }

    /// The contiguous `(start, end)` id range of the keys starting with `prefix`, half-open. Ids
    /// follow lexicographic order, so a prefix is a slice of the id space rather than a set of ids
    /// to test one at a time -- usable directly as a `range()` or a bitset window.
    fn prefix_id_range(&self, prefix: &str) -> (u64, u64) {
        let r = self.inner.prefix_id_range(prefix);
        (r.start, r.end)
    }

    /// The smallest `(key, id)` with `key >= query`, or `None` if every key is smaller.
    fn successor(&self, query: &str) -> Option<(String, u64)> {
        self.inner.successor(query)
    }

    /// The largest `(key, id)` with `key <= query`, or `None` if every key is larger.
    fn predecessor(&self, query: &str) -> Option<(String, u64)> {
        self.inner.predecessor(query)
    }

    /// `(key, id)` pairs within Levenshtein edit distance `max_distance` of `query`.
    #[pyo3(signature = (query, max_distance, limit=None))]
    fn fuzzy(
        &self,
        py: Python<'_>,
        query: &str,
        max_distance: u32,
        limit: Option<usize>,
    ) -> PyResult<Vec<(String, u64)>> {
        py.detach(|| {
            let it = self.inner.fuzzy_iter(query, max_distance)?;
            Ok(match limit {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            })
        })
        .map_err(to_py)
    }

    /// `(key, id)` pairs whose key contains `query` as a subsequence. `limit` stops after that many
    /// matches.
    #[pyo3(signature = (query, limit=None))]
    fn subsequence(&self, py: Python<'_>, query: &str, limit: Option<usize>) -> Vec<(String, u64)> {
        py.detach(|| {
            let it = self.inner.subsequence_iter(query);
            match limit {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        })
    }

    /// Iterate every `(key, id)` in lexicographic (= id) order, **lazily** — a chunk of the
    /// transducer stream at a time, so no giant list is materialised the way `prefix("")` would.
    fn __iter__(slf: Bound<'_, Self>) -> StringIndexIterator {
        let remaining = slf.borrow().inner.len() as u64;
        StringIndexIterator {
            parent: slf.unbind(),
            state: std::sync::Mutex::new(IterState {
                buf: Vec::new().into_iter(),
                resume: None,
                remaining,
            }),
        }
    }

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes = py.detach(|| self.inner.to_bytes());
        PyBytes::new(py, &bytes)
    }

    /// Length of the `to_bytes` blob in bytes, without producing it.
    fn serialized_len(&self) -> usize {
        self.inner.serialized_len()
    }

    /// Pickle support: the blob, and the loader that reads it back.
    ///
    /// An index pickles by **copying its bytes**, including one opened with `load_mmap`, whose
    /// pages are borrowed from a file the unpickling process may not have — a path would not
    /// survive a `spawn`ed worker on another machine, and a borrowed mapping would not survive
    /// the file changing. Pickling a large index therefore costs its serialised size in the
    /// pickle; `save` + `load_mmap` is what to use when both ends can see the same file.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyBytes>,))> {
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py),)))
    }

    /// Reconstruct from a [`PyStringIndex::to_bytes`] blob.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py.detach(|| StringIndex::from_bytes(data)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Reconstruct from a blob **someone else wrote**, validating the transducer before any query
    /// can reach it — slower than `from_bytes`, and total where that one is not.
    ///
    /// `from_bytes` documents the one exception to "arbitrary bytes raise `ValueError`": the blob
    /// is an `fst` transducer whose node decoder is safe but not *total*, and the checksum in
    /// front of it is public, so bytes crafted to carry a matching one **panic** —
    /// `pyo3_runtime.PanicException`, not `ValueError`. This loader checks the transducer as a
    /// graph — every reachable node once, in time proportional to nodes and transitions rather
    /// than to the keys they spell — so that its values are ranks and its keys are UTF-8, and
    /// catches the panic at the load boundary, so a crafted blob raises `ValueError` like any
    /// other bad input.
    ///
    /// It costs two decodes of every node and the checksum: 22.9 ms against
    /// 0.7 ms for `from_bytes` on the 479 823-word `/usr/share/dict/words`, 32×.
    /// Worth paying once for a blob from a stranger, not worth paying for one of your own.
    ///
    /// Two things it cannot promise. The panic runs the process-wide hook on its way out, so the
    /// rejection normally prints a panic message to stderr before `ValueError` is raised — nothing
    /// suppresses it, because the hook is global. And a build with `panic = "abort"` has no
    /// unwinding to catch, which the published wheels do not use.
    #[staticmethod]
    fn from_untrusted_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py
            .detach(|| StringIndex::from_untrusted_bytes(data))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Write the index to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load an index previously written with `save`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| StringIndex::load(&path)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load` for a file **someone else wrote**: the bytes go through `from_untrusted_bytes`,
    /// whose validation and cost this inherits. For a file too large to read into memory there is
    /// `load_mmap_untrusted`, under `load_mmap`'s obligation.
    #[staticmethod]
    fn load_untrusted(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py
            .detach(|| StringIndex::load_untrusted(&path))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Zero-copy load: memory-map the file and borrow the index from it — no read into RAM, so a
    /// multi-gigabyte index is ready instantly and its pages are shared across processes.
    ///
    /// The mapped file must not be modified or truncated by any process while the index is alive:
    /// the bytes are borrowed, not copied, so a concurrent write is undefined behaviour rather
    /// than a stale answer. Python cannot express that obligation in the type system the way the
    /// Rust API does (where this is an `unsafe fn`), so it is the caller's contract. Use `load` if
    /// the file may change.
    #[staticmethod]
    fn load_mmap(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller, who is told in the docstring above that the file must
        // stay unmodified for the index's lifetime. There is no way to enforce it from Python.
        let inner = py
            .detach(|| unsafe { StringIndex::load_mmap(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load_mmap` plus the checksum `load` makes: one pass over the mapping at load, pages still
    /// shared, nothing copied. For a file you wrote but did not carry yourself.
    ///
    /// The same obligation as `load_mmap`: the file must not change while the index is alive. The
    /// checksum is computed once, at load, and says nothing about later.
    #[staticmethod]
    fn load_mmap_verified(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { StringIndex::load_mmap_verified(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load_mmap` for a file **someone else wrote** and too large to copy: the validation of
    /// `from_untrusted_bytes` over the mapping, pages shared, nothing copied.
    ///
    /// The same obligation as `load_mmap`, and it weighs more here: the validation reads the
    /// mapping once and trusts what it saw, so a file that changes afterwards — the stranger's, if
    /// they can still write it — is exactly what the obligation forbids. Map a copy you own.
    #[staticmethod]
    fn load_mmap_untrusted(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { StringIndex::load_mmap_untrusted(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

/// How many pairs one refill decodes. A `#[pyclass]` cannot hold an `fst` stream borrowing the
/// index across `__next__` calls, so the alternative to buffering is a rank-walk per key — which is
/// what this iterator used to do, at `O(key length)` each and no reuse between neighbours. One
/// refill costs a single `O(key length)` seek and then streams, so the chunk size only has to be
/// large enough to amortise that seek; 1024 pairs keeps the buffer well under a megabyte on any
/// realistic key.
const ITER_CHUNK: usize = 1024;

/// Lazy `(key, id)` iterator over a [`PyStringIndex`], in sorted order. Holds a reference to the parent
/// index and streams it a chunk at a time, so it never materialises the whole key set.
#[pyclass(frozen, name = "StringIndexIterator", module = "lexindex._core")]
pub struct StringIndexIterator {
    parent: Py<PyStringIndex>,
    /// Behind a lock because the class is `frozen`, and `frozen` because it must not be: on a
    /// free-threaded interpreter PyO3's borrow flag turns two threads calling `__next__` on one
    /// iterator into `RuntimeError: Already borrowed` -- measured, 7 of 8 threads. Sharing an
    /// iterator is a strange thing to do, but raising is a worse answer than serialising.
    state: std::sync::Mutex<IterState>,
}

struct IterState {
    buf: std::vec::IntoIter<(String, u64)>,
    /// Last key handed out, where the next refill resumes. `None` before the first one.
    resume: Option<String>,
    /// How many pairs may still be yielded — `len()` at the start, counted down by every refill.
    ///
    /// It is the termination proof, not an optimisation. The cursor is a `String`, so a key that is
    /// not valid UTF-8 — only reachable from a blob that was crafted or corrupted, which
    /// `from_bytes` does not promise to reject — comes back through `from_utf8_lossy` as a
    /// *different* byte string, and seeking past a cursor that sorts below the key it names would
    /// hand out that key again forever. Bounding the count turns that into a short answer, which is
    /// the failure mode the loader already documents, instead of a hang.
    remaining: u64,
}

impl IterState {
    /// Decode the next chunk, at most `remaining` pairs, and count them off.
    fn refill(&mut self, py: Python<'_>, parent: &Py<PyStringIndex>) {
        let want = ITER_CHUNK.min(self.remaining as usize);
        let parent = parent.clone_ref(py);
        let chunk: Vec<(String, u64)> = {
            let idx = parent.borrow(py);
            match &self.resume {
                Some(after) => idx.inner.iter_after(after).take(want).collect(),
                None => idx.inner.iter().take(want).collect(),
            }
        };
        self.remaining -= chunk.len() as u64;
        if let Some((last, _)) = chunk.last() {
            self.resume = Some(last.clone());
        }
        self.buf = chunk.into_iter();
    }
}

#[pymethods]
impl StringIndexIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> Option<(String, u64)> {
        let mut state = self
            .state
            .lock_py_attached(py)
            .expect("iterator state poisoned by an earlier panic");
        loop {
            if let Some(item) = state.buf.next() {
                return Some(item);
            }
            if state.remaining == 0 {
                return None;
            }
            // Every trip either yields, or strictly decreases `remaining`, or stops here — the
            // stream running dry before `len()` says it should is a malformed index, not a retry.
            let before = state.remaining;
            state.refill(py, &self.parent);
            if state.remaining == before {
                return None;
            }
        }
    }
}

impl PyStringIndex {
    /// The ids of every chunk in order, [`MISSING_ID`] where the column holds a null.
    fn arrow_ids(&self, py: Python<'_>, chunks: &[Utf8Chunk]) -> PyResult<Vec<u64>> {
        let inner = &self.inner;
        Ok(py.detach(|| {
            let mut out = Vec::with_capacity(total_len(chunks));
            for c in chunks {
                let ids = inner.ids_of_with(c.len, |i| c.key(i));
                out.extend(ids.into_iter().enumerate().map(|(i, id)| match id {
                    Some(id) if c.valid(i) => id,
                    _ => u64::MAX,
                }));
            }
            out
        }))
    }
}

/// Minimal-perfect-hash dictionary: exact `string → dense id` with reverse lookup and persistence.
#[cfg(feature = "mph")]
#[pyclass(name = "PerfectHashIndex", module = "lexindex._core", frozen)]
pub struct PyPerfectHashIndex {
    inner: Arc<PerfectHashIndex>,
}

#[cfg(feature = "mph")]
#[pymethods]
impl PyPerfectHashIndex {
    /// Build from an iterable of strings (duplicates removed; ids are arbitrary dense slots).
    /// `fingerprints=True` stores one more byte per key so that a lookup of an absent key stops
    /// after one cache miss instead of two — for a workload that is mostly misses.
    #[new]
    #[pyo3(signature = (items, *, fingerprints=false))]
    fn new(py: Python<'_>, items: &Bound<'_, PyAny>, fingerprints: bool) -> PyResult<Self> {
        let items = collect_strs(items)?;
        let inner = py
            .detach(|| {
                if fingerprints {
                    PerfectHashIndex::build_with_fingerprints(items.iter())
                } else {
                    PerfectHashIndex::build(items.iter())
                }
            })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Build straight to `path` **without ever holding the keys**, for a corpus that does not fit
    /// in memory. Returns the number of keys written.
    ///
    /// `source` is a zero-argument callable returning an iterable of `str`, and it is **called
    /// twice**: the build hashes every key first and can only place a key once the perfect hash
    /// exists, so a one-shot generator cannot serve — hand it `lambda: open(path)`-style factories,
    /// never the iterable itself (that is a `TypeError`). Keys must be distinct; a repeated key, or
    /// a second pass that yields different keys, is refused with `ValueError` and `path` is left
    /// untouched. Runs with the GIL held throughout: every key comes from a Python iterator.
    #[staticmethod]
    #[pyo3(signature = (source, path, *, fingerprints=false))]
    fn build_to_file<'py>(
        source: &Bound<'py, PyAny>,
        path: PathBuf,
        fingerprints: bool,
    ) -> PyResult<usize> {
        if !source.is_callable() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "build_to_file() takes a zero-argument callable that returns an iterable of str \
                 (it is called twice), not the iterable itself",
            ));
        }
        let err: Rc<RefCell<Option<PyErr>>> = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&err);
        let replay = || -> Box<dyn Iterator<Item = String> + 'py> {
            // After a Python-level failure there is nothing to replay: an empty pass makes the
            // build stop, and the recorded exception is what the caller sees.
            if err.borrow().is_some() {
                return Box::new(std::iter::empty());
            }
            match source.call0().and_then(|iterable| iterable.try_iter()) {
                Ok(it) => Box::new(stream_strs(it, Rc::clone(&err))),
                Err(e) => {
                    *err.borrow_mut() = Some(e);
                    Box::new(std::iter::empty())
                }
            }
        };
        let written = PerfectHashIndex::build_to_file_checked(
            &path,
            replay,
            || {
                if seen.borrow().is_some() {
                    Err(IndexError::Format(
                        "perfect-hash: the source raised before it ended",
                    ))
                } else {
                    Ok(())
                }
            },
            fingerprints,
        );
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        written.map_err(to_py)
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Whether the index was built with `fingerprints=True`.
    fn has_fingerprints(&self) -> bool {
        self.inner.has_fingerprints()
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Dense id of `key` (membership verified), or `None` if absent.
    fn id(&self, key: &str) -> Option<u32> {
        self.inner.id(key)
    }

    /// Dense id of `key` **without** membership verification — `key` must be in the dictionary, or the
    /// result is an arbitrary valid slot. Fastest lookup for a fixed vocabulary.
    fn id_unchecked(&self, key: &str) -> u32 {
        self.inner.id_unchecked(key)
    }

    /// Whether `key` is present.
    fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Dense id of `key`, raising `KeyError` if it is absent — the dict spelling of
    /// [`id`](Self::id), for a caller who wants a miss to be an error rather than a `None` to
    /// check.
    ///
    /// There is no `__setitem__`, no `keys` / `values` / `items` and no `Mapping` registration:
    /// this is an immutable `str -> int` lookup, and pretending to be a mapping would promise
    /// iteration semantics it does not have.
    fn __getitem__(&self, key: &str) -> PyResult<u32> {
        self.inner
            .id(key)
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))
    }

    /// Dense id of `key`, or `default` (`None` unless given) — `idx.get("apple", -1)`.
    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.id(key) {
            Some(id) => Ok(id.into_pyobject(py)?.into_any()),
            None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
        }
    }

    /// Key for `id`, or `None` if out of range.
    fn key<'py>(&self, py: Python<'py>, id: u32) -> Option<Bound<'py, PyString>> {
        self.inner.key(id).map(|k| PyString::new(py, k))
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys` (`None` where absent).
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u32>> {
        py.detach(|| self.inner.ids_of(&keys))
    }

    /// Batched [`id`](Self::id) packed into a `bytes` buffer instead of a list, for callers who
    /// hand the result to `numpy` or `array` rather than reading it item by item.
    ///
    /// One `4`-byte native-endian item per key, aligned with `keys`, [`MISSING_ID`](Self::MISSING_ID)
    /// where a key is absent. `np.frombuffer(buf, dtype=index.ID_DTYPE)` shares the memory rather
    /// than copying it; `ids_of` has to build one Python `int` per key, which is what this avoids.
    ///
    /// Native endianness — unlike the blobs, which are little-endian everywhere — because the
    /// buffer is meant for `np.frombuffer` on the machine that produced it, not for the wire.
    fn ids_of_bytes<'py>(
        &self,
        py: Python<'py>,
        keys: Vec<PyBackedStr>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let packed = py.detach(|| {
            let mut out = Vec::with_capacity(keys.len() * 4);
            for id in self.inner.ids_of(&keys) {
                out.extend_from_slice(&id.unwrap_or(u32::MAX).to_ne_bytes());
            }
            out
        });
        Ok(PyBytes::new(py, &packed))
    }

    /// Batched [`id`](Self::id) over an Arrow `utf8` / `large_utf8` column — a pyarrow `Array`
    /// or `ChunkedArray`, a pandas column of `ArrowDtype`, a polars `Series` — packed like
    /// [`ids_of_bytes`](Self::ids_of_bytes): one [`ID_DTYPE`](Self::ID_DTYPE) item per element,
    /// [`MISSING_ID`](Self::MISSING_ID) for an absent key and for a null. The keys are read from the
    /// column's offset and data buffers, so no Python string exists per key — building and
    /// borrowing those was half to two thirds of what the list forms cost.
    fn ids_of_arrow<'py>(
        &self,
        py: Python<'py>,
        column: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let chunks = utf8_chunks(column)?;
        let ids = self.arrow_ids(py, &chunks)?;
        Ok(PyBytes::new(py, &packed(&ids, u32::to_ne_bytes)))
    }

    /// [`ids_of_arrow`](Self::ids_of_arrow) written into memory the caller owns, as
    /// [`ids_into`](Self::ids_into) does for a list: `out` is a writable C-contiguous buffer of
    /// [`ID_DTYPE`](Self::ID_DTYPE) items at least as long as the column.
    fn ids_into_arrow(
        &self,
        py: Python<'_>,
        column: &Bound<'_, PyAny>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let chunks = utf8_chunks(column)?;
        let n = total_len(&chunks);
        if n == 0 {
            return writable_buffer(out);
        }
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let sink = id_sink::<u32>(out, n)?;
        let ids = self.arrow_ids(py, &chunks)?;
        write_ids(py, &sink, &ids)
    }

    /// [`ids_of_bytes`](Self::ids_of_bytes) written into memory the caller owns instead of a fresh
    /// `bytes` per call: `out` is any writable C-contiguous buffer of [`ID_DTYPE`](Self::ID_DTYPE)
    /// items — `np.empty(len(keys), dtype=index.ID_DTYPE)` is the usual one — so a hot loop can
    /// reuse one array. The first `len(keys)` items are written; the rest are left as they were.
    /// With no keys nothing is written and `out` need only be a writable buffer.
    ///
    /// A read-only, strided or mistyped buffer is a `BufferError` (a `uint64` array handed to this
    /// index is refused rather than half-filled); one shorter than `keys` is a `ValueError`.
    fn ids_into(
        &self,
        py: Python<'_>,
        keys: Vec<PyBackedStr>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if keys.is_empty() {
            return writable_buffer(out);
        }
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let sink = id_sink::<u32>(out, keys.len())?;
        let ids: Vec<u32> = py.detach(|| {
            self.inner
                .ids_of(&keys)
                .into_iter()
                .map(|id| id.unwrap_or(u32::MAX))
                .collect()
        });
        write_ids(py, &sink, &ids)
    }

    /// The `numpy` dtype of one [`ids_of_bytes`](Self::ids_of_bytes) item, so a caller can read the
    /// buffer without hardcoding a width that differs between the index types.
    #[classattr]
    const ID_DTYPE: &'static str = "uint32";

    /// The [`ids_of_bytes`](Self::ids_of_bytes) item standing for an absent key. Ids are below
    /// `len()`, so this is a real id only for an index of exactly `u32::MAX + 1` keys, which
    /// [`ids_of_bytes`](Self::ids_of_bytes) refuses rather than silently aliasing.
    #[classattr]
    const MISSING_ID: u32 = u32::MAX;

    /// Batched [`key`](Self::key): one call for many ids, aligned with `ids` (`None` where out of range).
    fn keys_of<'py>(&self, py: Python<'py>, ids: Vec<u32>) -> Vec<Option<Bound<'py, PyString>>> {
        // Two passes on purpose: the lookups are pure Rust and run with the GIL released, then the
        // arena slices become Python strings. Collecting `String`s in between would copy each key
        // once more, only to free it on the next line.
        let found: Vec<Option<&str>> =
            py.detach(|| ids.iter().map(|&i| self.inner.key(i)).collect());
        found
            .into_iter()
            .map(|k| k.map(|k| PyString::new(py, k)))
            .collect()
    }

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py.detach(|| self.inner.to_bytes()).map_err(to_py)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Length of the `to_bytes` blob in bytes, without producing it.
    fn serialized_len(&self) -> PyResult<usize> {
        self.inner.serialized_len().map_err(to_py)
    }

    /// Pickle support: the blob, and the loader that reads it back.
    ///
    /// An index pickles by **copying its bytes**, including one opened with `load_mmap`, whose
    /// pages are borrowed from a file the unpickling process may not have — a path would not
    /// survive a `spawn`ed worker on another machine, and a borrowed mapping would not survive
    /// the file changing. Pickling a large index therefore costs its serialised size in the
    /// pickle; `save` + `load_mmap` is what to use when both ends can see the same file.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyBytes>,))> {
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py)?,)))
    }

    /// Reconstruct from a [`PyPerfectHashIndex::to_bytes`] blob.
    ///
    /// Every length the index will read is validated against the bytes present, so arbitrary input
    /// raises rather than misbehaving. A blob written before 1.0 is refused: its perfect hash came
    /// from a crate this version no longer links, and the index has to be rebuilt from its keys.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py
            .detach(|| PerfectHashIndex::from_bytes(data))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Write the dictionary to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a dictionary previously written with `save`. Validated like `from_bytes`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| PerfectHashIndex::load(&path)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Memory-map the file and borrow the key arena zero-copy (only the small MPH is read into RAM).
    ///
    /// The mapped file must not be modified or truncated by any process while the dictionary is
    /// alive — the bytes are borrowed, so a concurrent write is undefined behaviour. See
    /// `StringIndex.load_mmap` for the full contract; use `load` if the file may change.
    #[staticmethod]
    fn load_mmap(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { PerfectHashIndex::load_mmap(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load_mmap` plus the payload checksum `load` makes: one pass over the mapping at load,
    /// the key arena still borrowed. For a file you wrote but did not carry yourself.
    ///
    /// The same obligation as `load_mmap`: the file must not change while the index is alive. The
    /// checksum is computed once, at load, and says nothing about later.
    #[staticmethod]
    fn load_mmap_verified(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { PerfectHashIndex::load_mmap_verified(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

/// The fingerprint width a `CompactHashIndex` constructor was asked for: `fingerprint_bytes`
/// (1, 2 or 4) or, keyword-only and instead of it, `fingerprint_bits` (1..=64).
#[cfg(feature = "mph")]
fn fingerprint_width(fingerprint_bytes: usize, fingerprint_bits: Option<u32>) -> PyResult<u32> {
    let bits = match fingerprint_bits {
        Some(bits) => {
            if fingerprint_bytes != 1 {
                return Err(PyValueError::new_err(
                    "pass fingerprint_bytes or fingerprint_bits, not both",
                ));
            }
            bits
        }
        None => {
            if !matches!(fingerprint_bytes, 1 | 2 | 4) {
                return Err(to_py(IndexError::Format(
                    "compact-hash: fingerprint_bytes must be 1, 2, or 4",
                )));
            }
            fingerprint_bytes as u32 * 8
        }
    };
    if !(1..=64).contains(&bits) {
        return Err(to_py(IndexError::Format(
            "compact-hash: fingerprint_bits must be in 1..=64",
        )));
    }
    Ok(bits)
}

impl PyPerfectHashIndex {
    /// The ids of every chunk in order, [`MISSING_ID`] where the column holds a null.
    fn arrow_ids(&self, py: Python<'_>, chunks: &[Utf8Chunk]) -> PyResult<Vec<u32>> {
        let inner = &self.inner;
        Ok(py.detach(|| {
            let mut out = Vec::with_capacity(total_len(chunks));
            for c in chunks {
                let ids = inner.ids_of_with(c.len, |i| c.key(i));
                out.extend(ids.into_iter().enumerate().map(|(i, id)| match id {
                    Some(id) if c.valid(i) => id,
                    _ => u32::MAX,
                }));
            }
            out
        }))
    }
}

/// Fingerprint minimal-perfect-hash dictionary: the smallest `string -> dense id` map. Membership is
/// probabilistic (false-positive rate `2 ** -fingerprint_bits`) and there is no reverse `id -> key`.
#[cfg(feature = "mph")]
#[pyclass(name = "CompactHashIndex", module = "lexindex._core", frozen)]
pub struct PyCompactHashIndex {
    inner: Arc<CompactHashIndex>,
}

#[cfg(feature = "mph")]
#[pymethods]
impl PyCompactHashIndex {
    /// Build from an iterable of strings, storing `fingerprint_bytes` (1, 2, or 4) per key, or —
    /// keyword-only — exactly `fingerprint_bits` (1..=64) per key. Fewer bits is smaller but raises
    /// the membership false-positive rate to `2 ** -fingerprint_bits` (6.25% at 4 bits, ≈ 0.4% at 8,
    /// ≈ 0.0015% at 16). Duplicates removed; ids are arbitrary dense slots.
    #[new]
    #[pyo3(signature = (items, fingerprint_bytes=1, *, fingerprint_bits=None))]
    fn new(
        py: Python<'_>,
        items: &Bound<'_, PyAny>,
        fingerprint_bytes: usize,
        fingerprint_bits: Option<u32>,
    ) -> PyResult<Self> {
        let bits = fingerprint_width(fingerprint_bytes, fingerprint_bits)?;
        // Unlike the key-storing indexes, this one needs only 16 hashed bytes per key — so the
        // items are hashed as they come off the iterator (under the GIL) and the strings dropped,
        // keeping a generator-fed build streaming on the Python side too.
        let pairs = collect_pairs(items)?;
        let inner = py
            .detach(|| CompactHashIndex::build_from_pairs(pairs, bits))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// The constructor for a corpus that does not fit in memory, written straight to `path`: the
    /// keys, in any order, are hashed as they come and their 16-byte pairs sorted in runs that
    /// spill beside the output; the perfect hash is built from the merged runs a chunk at a time
    /// and the fingerprints written at their slots, so neither the corpus, its hashes nor the
    /// finished table is ever held whole. The file is byte for byte what the constructor and
    /// `save` write. Returns the number of distinct keys written. Widths as in the constructor.
    #[staticmethod]
    #[pyo3(signature = (items, path, fingerprint_bytes=1, *, fingerprint_bits=None))]
    fn build_to_file(
        items: &Bound<'_, PyAny>,
        path: PathBuf,
        fingerprint_bytes: usize,
        fingerprint_bits: Option<u32>,
    ) -> PyResult<usize> {
        let bits = fingerprint_width(fingerprint_bytes, fingerprint_bits)?;
        let err = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&err);
        // The check runs once the input has ended and again inside the atomic write, so an
        // iterable that raises halfway aborts the build with `path` untouched.
        let written = CompactHashIndex::build_to_file_checked(
            stream_strs(items.try_iter()?, Rc::clone(&err)),
            &path,
            bits,
            move || {
                if seen.borrow().is_some() {
                    Err(crate::IndexError::Format(
                        "compact-hash: the input iterable raised before it ended",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        written.map_err(to_py)
    }

    /// Width of the stored fingerprints in bits; the false-positive rate is `2 ** -fingerprint_bits`.
    #[getter]
    fn fingerprint_bits(&self) -> u32 {
        self.inner.fingerprint_bits()
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Dense id of `key` (membership checked against the fingerprint), or `None`.
    fn id(&self, key: &str) -> Option<u32> {
        self.inner.id(key)
    }

    /// Dense id of `key` **without** the fingerprint check — `key` must be a member, or the result is
    /// an arbitrary valid slot. Fastest lookup for a fixed vocabulary.
    fn id_unchecked(&self, key: &str) -> u32 {
        self.inner.id_unchecked(key)
    }

    /// Dense id of `key`, raising `KeyError` if it is absent — the dict spelling of
    /// [`id`](Self::id), for a caller who wants a miss to be an error rather than a `None` to
    /// check.
    ///
    /// There is no `__setitem__`, no `keys` / `values` / `items` and no `Mapping` registration:
    /// this is an immutable `str -> int` lookup, and pretending to be a mapping would promise
    /// iteration semantics it does not have.
    fn __getitem__(&self, key: &str) -> PyResult<u32> {
        self.inner
            .id(key)
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))
    }

    /// Dense id of `key`, or `default` (`None` unless given) — `idx.get("apple", -1)`.
    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.id(key) {
            Some(id) => Ok(id.into_pyobject(py)?.into_any()),
            None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
        }
    }

    /// Whether `key` is present (subject to the false-positive rate).
    fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys` (`None` where absent).
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u32>> {
        py.detach(|| self.inner.ids_of(&keys))
    }

    /// Batched [`id`](Self::id) packed into a `bytes` buffer instead of a list, for callers who
    /// hand the result to `numpy` or `array` rather than reading it item by item.
    ///
    /// One `4`-byte native-endian item per key, aligned with `keys`, [`MISSING_ID`](Self::MISSING_ID)
    /// where a key is absent. `np.frombuffer(buf, dtype=index.ID_DTYPE)` shares the memory rather
    /// than copying it; `ids_of` has to build one Python `int` per key, which is what this avoids.
    ///
    /// Native endianness — unlike the blobs, which are little-endian everywhere — because the
    /// buffer is meant for `np.frombuffer` on the machine that produced it, not for the wire.
    fn ids_of_bytes<'py>(
        &self,
        py: Python<'py>,
        keys: Vec<PyBackedStr>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let packed = py.detach(|| {
            let mut out = Vec::with_capacity(keys.len() * 4);
            for id in self.inner.ids_of(&keys) {
                out.extend_from_slice(&id.unwrap_or(u32::MAX).to_ne_bytes());
            }
            out
        });
        Ok(PyBytes::new(py, &packed))
    }

    /// Batched [`id`](Self::id) over an Arrow `utf8` / `large_utf8` column — a pyarrow `Array`
    /// or `ChunkedArray`, a pandas column of `ArrowDtype`, a polars `Series` — packed like
    /// [`ids_of_bytes`](Self::ids_of_bytes): one [`ID_DTYPE`](Self::ID_DTYPE) item per element,
    /// [`MISSING_ID`](Self::MISSING_ID) for an absent key and for a null. The keys are read from the
    /// column's offset and data buffers, so no Python string exists per key — building and
    /// borrowing those was half to two thirds of what the list forms cost.
    fn ids_of_arrow<'py>(
        &self,
        py: Python<'py>,
        column: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let chunks = utf8_chunks(column)?;
        let ids = self.arrow_ids(py, &chunks)?;
        Ok(PyBytes::new(py, &packed(&ids, u32::to_ne_bytes)))
    }

    /// [`ids_of_arrow`](Self::ids_of_arrow) written into memory the caller owns, as
    /// [`ids_into`](Self::ids_into) does for a list: `out` is a writable C-contiguous buffer of
    /// [`ID_DTYPE`](Self::ID_DTYPE) items at least as long as the column.
    fn ids_into_arrow(
        &self,
        py: Python<'_>,
        column: &Bound<'_, PyAny>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let chunks = utf8_chunks(column)?;
        let n = total_len(&chunks);
        if n == 0 {
            return writable_buffer(out);
        }
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let sink = id_sink::<u32>(out, n)?;
        let ids = self.arrow_ids(py, &chunks)?;
        write_ids(py, &sink, &ids)
    }

    /// [`ids_of_bytes`](Self::ids_of_bytes) written into memory the caller owns instead of a fresh
    /// `bytes` per call: `out` is any writable C-contiguous buffer of [`ID_DTYPE`](Self::ID_DTYPE)
    /// items — `np.empty(len(keys), dtype=index.ID_DTYPE)` is the usual one — so a hot loop can
    /// reuse one array. The first `len(keys)` items are written; the rest are left as they were.
    /// With no keys nothing is written and `out` need only be a writable buffer.
    ///
    /// A read-only, strided or mistyped buffer is a `BufferError` (a `uint64` array handed to this
    /// index is refused rather than half-filled); one shorter than `keys` is a `ValueError`.
    fn ids_into(
        &self,
        py: Python<'_>,
        keys: Vec<PyBackedStr>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if keys.is_empty() {
            return writable_buffer(out);
        }
        if self.inner.len() > u32::MAX as usize {
            return Err(PyValueError::new_err(
                "index holds more than u32::MAX keys, so MISSING_ID is a real id here; use ids_of",
            ));
        }
        let sink = id_sink::<u32>(out, keys.len())?;
        let ids: Vec<u32> = py.detach(|| {
            self.inner
                .ids_of(&keys)
                .into_iter()
                .map(|id| id.unwrap_or(u32::MAX))
                .collect()
        });
        write_ids(py, &sink, &ids)
    }

    /// The `numpy` dtype of one [`ids_of_bytes`](Self::ids_of_bytes) item, so a caller can read the
    /// buffer without hardcoding a width that differs between the index types.
    #[classattr]
    const ID_DTYPE: &'static str = "uint32";

    /// The [`ids_of_bytes`](Self::ids_of_bytes) item standing for an absent key. Ids are below
    /// `len()`, so this is a real id only for an index of exactly `u32::MAX + 1` keys, which
    /// [`ids_of_bytes`](Self::ids_of_bytes) refuses rather than silently aliasing.
    #[classattr]
    const MISSING_ID: u32 = u32::MAX;

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let bytes = py.detach(|| self.inner.to_bytes()).map_err(to_py)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Length of the `to_bytes` blob in bytes, without producing it.
    fn serialized_len(&self) -> PyResult<usize> {
        self.inner.serialized_len().map_err(to_py)
    }

    /// Pickle support: the blob, and the loader that reads it back.
    ///
    /// An index pickles by **copying its bytes**, including one opened with `load_mmap`, whose
    /// pages are borrowed from a file the unpickling process may not have — a path would not
    /// survive a `spawn`ed worker on another machine, and a borrowed mapping would not survive
    /// the file changing. Pickling a large index therefore costs its serialised size in the
    /// pickle; `save` + `load_mmap` is what to use when both ends can see the same file.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyBytes>,))> {
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py)?,)))
    }

    /// Reconstruct from a [`PyCompactHashIndex::to_bytes`] blob.
    ///
    /// Every length the index will read is validated against the bytes present, so arbitrary input
    /// raises rather than misbehaving. A blob written before 1.0 is refused: its perfect hash came
    /// from a crate this version no longer links, and this index stores no keys, so the only route
    /// is to rebuild it from them.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py
            .detach(|| CompactHashIndex::from_bytes(data))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Write the dictionary to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a dictionary previously written with `save`. Validated like `from_bytes`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| CompactHashIndex::load(&path)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Zero-copy load: memory-map the file and borrow the fingerprint table.
    ///
    /// The mapped file must not be modified or truncated by any process while the dictionary is
    /// alive — the bytes are borrowed, so a concurrent write is undefined behaviour. See
    /// `StringIndex.load_mmap` for the full contract; use `load` if the file may change.
    #[staticmethod]
    fn load_mmap(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { CompactHashIndex::load_mmap(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load_mmap` plus the payload checksum `load` makes: one pass over the mapping at load,
    /// the fingerprint table still borrowed. For a file you wrote but did not carry yourself.
    ///
    /// The same obligation as `load_mmap`: the file must not change while the index is alive. The
    /// checksum is computed once, at load, and says nothing about later.
    #[staticmethod]
    fn load_mmap_verified(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { CompactHashIndex::load_mmap_verified(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

impl PyCompactHashIndex {
    /// The ids of every chunk in order, [`MISSING_ID`] where the column holds a null.
    fn arrow_ids(&self, py: Python<'_>, chunks: &[Utf8Chunk]) -> PyResult<Vec<u32>> {
        let inner = &self.inner;
        Ok(py.detach(|| {
            let mut out = Vec::with_capacity(total_len(chunks));
            for c in chunks {
                let ids = inner.ids_of_with(c.len, |i| c.key(i));
                out.extend(ids.into_iter().enumerate().map(|(i, id)| match id {
                    Some(id) if c.valid(i) => id,
                    _ => u32::MAX,
                }));
            }
            out
        }))
    }
}

/// Minimal perfect hash and nothing else: `string -> dense id` for a vocabulary known to be closed.
/// `id` never says "absent" -- a member's id, or some id in `[0, n)` for any other string -- and the
/// index is the perfect hash alone, about 0.26 bytes per key.
#[cfg(feature = "mph")]
#[pyclass(name = "ClosedHashIndex", module = "lexindex._core", frozen)]
pub struct PyClosedHashIndex {
    inner: Arc<ClosedHashIndex>,
}

#[cfg(feature = "mph")]
#[pymethods]
impl PyClosedHashIndex {
    /// Build from an iterable of strings. Duplicates removed; ids are arbitrary dense slots. The
    /// items are hashed as they come off the iterator and the strings dropped, so a generator-fed
    /// build streams on the Python side too.
    #[new]
    fn new(py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<Self> {
        let pairs = collect_pairs(items)?;
        let inner = py
            .detach(|| ClosedHashIndex::build_from_pairs(pairs))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Dense id of `key` if it is a member; **some** id in `[0, n)` otherwise, and `0` for an
    /// empty index. Nothing stored can tell the two apart, so nothing tries: every query must be
    /// a member by construction. There is no `__contains__` and no `__getitem__` -- a membership
    /// test this index cannot make would be a lie in the dict spelling.
    fn id(&self, key: &str) -> u32 {
        self.inner.id(key)
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys`.
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<u32> {
        py.detach(|| self.inner.ids_of(&keys))
    }

    /// Batched [`id`](Self::id) packed into a `bytes` buffer instead of a list: one 4-byte
    /// native-endian item per key, aligned with `keys`, for
    /// `np.frombuffer(buf, dtype=index.ID_DTYPE)`. No item stands for an absent key, because
    /// this index never reports one.
    fn ids_of_bytes<'py>(&self, py: Python<'py>, keys: Vec<PyBackedStr>) -> Bound<'py, PyBytes> {
        let packed = py.detach(|| {
            let mut out = Vec::with_capacity(keys.len() * 4);
            for id in self.inner.ids_of(&keys) {
                out.extend_from_slice(&id.to_ne_bytes());
            }
            out
        });
        PyBytes::new(py, &packed)
    }

    /// Batched [`id`](Self::id) over an Arrow `utf8` / `large_utf8` column — a pyarrow `Array`
    /// or `ChunkedArray`, a pandas column of `ArrowDtype`, a polars `Series` — packed like
    /// [`ids_of_bytes`](Self::ids_of_bytes): one [`ID_DTYPE`](Self::ID_DTYPE) item per element,
    /// a `ValueError` for an absent key and for a null. The keys are read from the
    /// column's offset and data buffers, so no Python string exists per key — building and
    /// borrowing those was half to two thirds of what the list forms cost.
    fn ids_of_arrow<'py>(
        &self,
        py: Python<'py>,
        column: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let chunks = utf8_chunks(column)?;
        let ids = self.arrow_ids(py, &chunks)?;
        Ok(PyBytes::new(py, &packed(&ids, u32::to_ne_bytes)))
    }

    /// [`ids_of_arrow`](Self::ids_of_arrow) written into memory the caller owns, as
    /// [`ids_into`](Self::ids_into) does for a list: `out` is a writable C-contiguous buffer of
    /// [`ID_DTYPE`](Self::ID_DTYPE) items at least as long as the column.
    fn ids_into_arrow(
        &self,
        py: Python<'_>,
        column: &Bound<'_, PyAny>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let chunks = utf8_chunks(column)?;
        let n = total_len(&chunks);
        if n == 0 {
            return writable_buffer(out);
        }
        let sink = id_sink::<u32>(out, n)?;
        let ids = self.arrow_ids(py, &chunks)?;
        write_ids(py, &sink, &ids)
    }

    /// [`ids_of_bytes`](Self::ids_of_bytes) written into memory the caller owns: `out` is any
    /// writable C-contiguous buffer of [`ID_DTYPE`](Self::ID_DTYPE) items at least `len(keys)`
    /// long; the first `len(keys)` items are written and the rest left as they were. A read-only,
    /// strided or mistyped buffer is a `BufferError`; one shorter than `keys` is a `ValueError`.
    fn ids_into(
        &self,
        py: Python<'_>,
        keys: Vec<PyBackedStr>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if keys.is_empty() {
            return writable_buffer(out);
        }
        let sink = id_sink::<u32>(out, keys.len())?;
        let ids = py.detach(|| self.inner.ids_of(&keys));
        write_ids(py, &sink, &ids)
    }

    /// The `numpy` dtype of one [`ids_of_bytes`](Self::ids_of_bytes) item.
    #[classattr]
    const ID_DTYPE: &'static str = "uint32";

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes = py.detach(|| self.inner.to_bytes());
        PyBytes::new(py, &bytes)
    }

    /// Length of the `to_bytes` blob in bytes, without producing it.
    fn serialized_len(&self) -> usize {
        self.inner.serialized_len()
    }

    /// Pickle support: the blob, and the loader that reads it back.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyBytes>,))> {
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py),)))
    }

    /// Reconstruct from a [`PyClosedHashIndex::to_bytes`] blob. Every length the index will read
    /// is validated against the bytes present, so arbitrary input raises rather than misbehaving.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py
            .detach(|| ClosedHashIndex::from_bytes(data))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Write the index to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a file written with `save`. Validated like `from_bytes`. There is no `load_mmap`: the
    /// whole blob is the perfect hash, which is read into memory whichever way it is loaded.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| ClosedHashIndex::load(&path)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

/// Ordered dictionary with the key stored for every id: exact `string ↔ rank`, about 3.5 B/key.
#[pyclass(name = "DictIndex", module = "lexindex._core", frozen)]
pub struct PyDictIndex {
    inner: Arc<DictIndex>,
}

#[pymethods]
impl PyDictIndex {
    /// Build from an iterable of strings, in any order; duplicates are removed and the ids are
    /// the ranks of the distinct keys in byte order. `block` keys share one stored head,
    /// `1..=1024`: a lookup scans up to `block - 1` entries and a reverse lookup decodes up to
    /// that many, so smaller blocks are faster and larger ones smaller.
    #[new]
    #[pyo3(signature = (items, block=32))]
    fn new(py: Python<'_>, items: &Bound<'_, PyAny>, block: usize) -> PyResult<Self> {
        let keys = collect_strs(items)?;
        let inner = py
            .detach(|| DictIndex::build_with_block(&keys, block))
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Keys per block, as given at build time.
    #[getter]
    fn block(&self) -> usize {
        self.inner.block()
    }

    fn __contains__(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Rank of `key`, or `None` if absent.
    fn id(&self, key: &str) -> Option<u64> {
        self.inner.id(key)
    }

    fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Rank of `key`, raising `KeyError` if it is absent — the dict spelling of [`id`](Self::id).
    /// No `__setitem__`, no `keys` / `values` / `items`: an immutable `str -> int` lookup.
    fn __getitem__(&self, key: &str) -> PyResult<u64> {
        self.inner
            .id(key)
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))
    }

    /// Rank of `key`, or `default` (`None` unless given).
    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.id(key) {
            Some(id) => Ok(id.into_pyobject(py)?.into_any()),
            None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
        }
    }

    /// The rank of the first key not below `key`: its own id if it is a member, otherwise the
    /// id it would have, `len(self)` past every key. Two of these bound a range of keys as a
    /// range of ids.
    fn lower_bound(&self, key: &str) -> u64 {
        self.inner.lower_bound(key)
    }

    /// Key at rank `id`, or `None` past the end.
    fn key(&self, id: u64) -> Option<String> {
        self.inner.key(id)
    }

    /// Batched [`key`](Self::key): a list aligned with `ids`, `None` where an id is out of
    /// range. One decode buffer serves the whole batch.
    fn keys_of(&self, py: Python<'_>, ids: Vec<u64>) -> Vec<Option<String>> {
        py.detach(|| {
            let mut buf = String::new();
            ids.iter()
                .map(|&i| self.inner.key_into(i, &mut buf).then(|| buf.clone()))
                .collect()
        })
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys`, `None` where a
    /// key is absent.
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u64>> {
        py.detach(|| self.inner.ids_of(&keys))
    }

    /// Batched [`id`](Self::id) packed into a `bytes` buffer instead of a list: one 8-byte
    /// native-endian item per key, aligned with `keys`, [`MISSING_ID`](Self::MISSING_ID) where a
    /// key is absent, for `np.frombuffer(buf, dtype=index.ID_DTYPE)`.
    fn ids_of_bytes<'py>(&self, py: Python<'py>, keys: Vec<PyBackedStr>) -> Bound<'py, PyBytes> {
        let packed = py.detach(|| {
            let mut out = Vec::with_capacity(keys.len() * 8);
            for id in self.inner.ids_of(&keys) {
                out.extend_from_slice(&id.unwrap_or(u64::MAX).to_ne_bytes());
            }
            out
        });
        PyBytes::new(py, &packed)
    }

    /// Batched [`id`](Self::id) over an Arrow `utf8` / `large_utf8` column — a pyarrow `Array`
    /// or `ChunkedArray`, a pandas column of `ArrowDtype`, a polars `Series` — packed like
    /// [`ids_of_bytes`](Self::ids_of_bytes): one [`ID_DTYPE`](Self::ID_DTYPE) item per element,
    /// [`MISSING_ID`](Self::MISSING_ID) for an absent key and for a null. The keys are read from
    /// the column's offset and data buffers, so no Python string exists per key.
    fn ids_of_arrow<'py>(
        &self,
        py: Python<'py>,
        column: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let chunks = utf8_chunks(column)?;
        let ids = self.arrow_ids(py, &chunks)?;
        Ok(PyBytes::new(py, &packed(&ids, u64::to_ne_bytes)))
    }

    /// [`ids_of_arrow`](Self::ids_of_arrow) written into memory the caller owns, as
    /// [`ids_into`](Self::ids_into) does for a list: `out` is a writable C-contiguous buffer of
    /// [`ID_DTYPE`](Self::ID_DTYPE) items at least as long as the column.
    fn ids_into_arrow(
        &self,
        py: Python<'_>,
        column: &Bound<'_, PyAny>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let chunks = utf8_chunks(column)?;
        let n = total_len(&chunks);
        if n == 0 {
            return writable_buffer(out);
        }
        let sink = id_sink::<u64>(out, n)?;
        let ids = self.arrow_ids(py, &chunks)?;
        write_ids(py, &sink, &ids)
    }

    /// [`ids_of_bytes`](Self::ids_of_bytes) written into memory the caller owns: `out` is any
    /// writable C-contiguous buffer of [`ID_DTYPE`](Self::ID_DTYPE) items at least `len(keys)`
    /// long; the first `len(keys)` items are written and the rest left as they were. A read-only,
    /// strided or mistyped buffer is a `BufferError`; one shorter than `keys` is a `ValueError`.
    fn ids_into(
        &self,
        py: Python<'_>,
        keys: Vec<PyBackedStr>,
        out: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        if keys.is_empty() {
            return writable_buffer(out);
        }
        let sink = id_sink::<u64>(out, keys.len())?;
        let ids: Vec<u64> = py.detach(|| {
            self.inner
                .ids_of(&keys)
                .into_iter()
                .map(|id| id.unwrap_or(u64::MAX))
                .collect()
        });
        write_ids(py, &sink, &ids)
    }

    /// The `numpy` dtype of one [`ids_of_bytes`](Self::ids_of_bytes) item.
    #[classattr]
    const ID_DTYPE: &'static str = "uint64";

    /// The [`ids_of_bytes`](Self::ids_of_bytes) item standing for an absent key.
    #[classattr]
    const MISSING_ID: u64 = u64::MAX;

    /// Iterate every `(key, id)` in key (= id) order, lazily, a chunk of decodes at a time.
    fn __iter__(slf: Bound<'_, Self>) -> DictIndexIterator {
        DictIndexIterator {
            parent: slf.unbind(),
            state: std::sync::Mutex::new(DictIterState {
                buf: Vec::new().into_iter(),
                next: 0,
            }),
        }
    }

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let bytes = py.detach(|| self.inner.to_bytes());
        PyBytes::new(py, &bytes)
    }

    /// Length of the `to_bytes` blob in bytes, without producing it.
    fn serialized_len(&self) -> usize {
        self.inner.serialized_len()
    }

    /// Pickle support: the blob, and the loader that reads it back.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyAny>, (Bound<'py, PyBytes>,))> {
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py),)))
    }

    /// Reconstruct from a [`PyDictIndex::to_bytes`] blob. Every length, both checksums, the
    /// symbol table and the per-block arrays are validated, so arbitrary input raises rather
    /// than misbehaving.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py.detach(|| DictIndex::from_bytes(data)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Write the index to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a file written with `save`, validated like `from_bytes`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| DictIndex::load(&path)).map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Zero-copy load: memory-map the file and borrow the keys, the block data and the two offset
    /// arrays from it — the header, the symbol table and the per-block samples (eight bytes a
    /// block) are what the load reads.
    ///
    /// The mapped file must not be modified or truncated by any process while the index is
    /// alive — the bytes are borrowed, so a concurrent write is undefined behaviour. See
    /// `StringIndex.load_mmap` for the full contract; use `load` if the file may change.
    #[staticmethod]
    fn load_mmap(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { DictIndex::load_mmap(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// `load_mmap` plus the checks `load` makes — the payload checksum and the walk over the
    /// per-block arrays — one pass over the mapping at load, the keys and the block data still
    /// borrowed. For a file you wrote but did not carry yourself.
    ///
    /// The same obligation as `load_mmap`: the file must not change while the index is alive. The
    /// checks run once, at load, and say nothing about later.
    #[staticmethod]
    fn load_mmap_verified(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { DictIndex::load_mmap_verified(&path) })
            .map_err(to_py)?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

/// [`PyDictIndex::__iter__`]: ids are dense, so the cursor is the next id and a refill is one
/// walk from it. Behind a lock for the reason [`StringIndexIterator`] gives.
#[pyclass(frozen, name = "DictIndexIterator", module = "lexindex._core")]
pub struct DictIndexIterator {
    parent: Py<PyDictIndex>,
    state: std::sync::Mutex<DictIterState>,
}

struct DictIterState {
    buf: std::vec::IntoIter<(String, u64)>,
    next: u64,
}

#[pymethods]
impl DictIndexIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> Option<(String, u64)> {
        let mut state = self
            .state
            .lock_py_attached(py)
            .expect("iterator state poisoned by an earlier panic");
        if let Some(item) = state.buf.next() {
            return Some(item);
        }
        let chunk: Vec<(String, u64)> = self
            .parent
            .get()
            .inner
            .iter_from(state.next)
            .take(ITER_CHUNK)
            .collect();
        // A walk that ends -- past the last key, or on a stream the crate did not write --
        // stays ended.
        state.next = chunk
            .last()
            .map_or(u64::MAX, |(_, id)| id.saturating_add(1));
        state.buf = chunk.into_iter();
        state.buf.next()
    }
}

impl PyDictIndex {
    /// The ids of every chunk in order, [`MISSING_ID`](Self::MISSING_ID) where the column holds
    /// a null or a key the index does not have.
    fn arrow_ids(&self, py: Python<'_>, chunks: &[Utf8Chunk]) -> PyResult<Vec<u64>> {
        let inner = &self.inner;
        Ok(py.detach(|| {
            let mut out = Vec::with_capacity(total_len(chunks));
            for c in chunks {
                let ids = inner.ids_of_with(c.len, |i| c.key(i));
                out.extend(ids.into_iter().enumerate().map(|(i, id)| match id {
                    Some(id) if c.valid(i) => id,
                    _ => u64::MAX,
                }));
            }
            out
        }))
    }
}

/// The three bases an [`PyOverlay`] can sit on. `Overlay<I>` is generic and a `#[pyclass]` cannot
/// be, so the choice becomes a runtime tag — and with it, `key`/`keys`/`compact` become a runtime
/// `TypeError` on a `CompactHashIndex` base where Rust refuses at compile time.
enum OverlayInner {
    String(Overlay<Arc<StringIndex>>),
    Perfect(Overlay<Arc<PerfectHashIndex>>),
    Compact(Overlay<Arc<CompactHashIndex>>),
}

/// Run `$body` against whichever overlay is inside. Every arm has to typecheck on its own, which is
/// what keeps the base-specific methods below from reaching the keyless base.
macro_rules! on_base {
    ($held:expr, |$ov:ident| $body:expr) => {
        match &*$held {
            OverlayInner::String($ov) => $body,
            OverlayInner::Perfect($ov) => $body,
            OverlayInner::Compact($ov) => $body,
        }
    };
    (mut $held:expr, |$ov:ident| $body:expr) => {
        match &mut *$held {
            OverlayInner::String($ov) => $body,
            OverlayInner::Perfect($ov) => $body,
            OverlayInner::Compact($ov) => $body,
        }
    };
}

/// Same, for the two bases that store their keys; the third answers with the error it earns.
macro_rules! on_keyed_base {
    ($held:expr, |$ov:ident| $body:expr) => {
        match &*$held {
            OverlayInner::String($ov) => Ok($body),
            OverlayInner::Perfect($ov) => Ok($body),
            OverlayInner::Compact(_) => Err(no_keys()),
        }
    };
}

fn no_keys() -> PyErr {
    PyTypeError::new_err(
        "a CompactHashIndex stores no keys, so an overlay over it has no key(), keys(), compact(), compact_to_file() or compact_with_remap()",
    )
}

/// Edits on top of an index that is expensive to rebuild.
///
/// `frozen` with the state behind a lock, not a plain `#[pyclass]` with `&mut self` methods: on a
/// free-threaded interpreter PyO3's borrow flag turns two threads calling `add` on one overlay into
/// `RuntimeError: Already borrowed` — measured, 7 of 8 threads. Concurrent edits to one overlay are
/// a reasonable thing for a Python caller to do, so they are serialised instead.
/// What [`PyOverlay::__reduce__`] hands pickle: the loader, the blob and the base's class.
type OverlayReduce<'py> = (Bound<'py, PyAny>, (Bound<'py, PyBytes>, Bound<'py, PyType>));

impl PyClosedHashIndex {
    /// The ids of every chunk in order, [`MISSING_ID`] where the column holds a null.
    fn arrow_ids(&self, py: Python<'_>, chunks: &[Utf8Chunk]) -> PyResult<Vec<u32>> {
        if chunks.iter().any(Utf8Chunk::has_nulls) {
            return Err(PyValueError::new_err(
                "a closed vocabulary has no id for a null: fill the nulls first",
            ));
        }
        let inner = &self.inner;
        Ok(py.detach(|| {
            let mut out = Vec::with_capacity(total_len(chunks));
            for c in chunks {
                out.extend(inner.ids_of_with(c.len, |i| c.key(i)));
            }
            out
        }))
    }
}

#[pyclass(frozen, name = "Overlay", module = "lexindex")]
pub struct PyOverlay {
    inner: std::sync::Mutex<OverlayInner>,
}

impl PyOverlay {
    fn wrap(inner: OverlayInner) -> Self {
        Self {
            inner: std::sync::Mutex::new(inner),
        }
    }

    /// Take the lock without deadlocking against the interpreter: `lock_py_attached` detaches
    /// before blocking, so a thread waiting here is not holding the runtime hostage.
    fn lock<'a>(&'a self, py: Python<'_>) -> std::sync::MutexGuard<'a, OverlayInner> {
        self.inner
            .lock_py_attached(py)
            .expect("overlay state poisoned by an earlier panic")
    }
}

#[pymethods]
impl PyOverlay {
    /// Wrap `index`. The index stays usable and is shared, not copied or taken.
    #[new]
    fn new(index: &Bound<'_, PyAny>) -> PyResult<Self> {
        if let Ok(i) = index.cast::<PyStringIndex>() {
            let base = Arc::clone(&i.borrow().inner);
            return Ok(Self::wrap(OverlayInner::String(Overlay::new(base))));
        }
        if let Ok(i) = index.cast::<PyPerfectHashIndex>() {
            let base = Arc::clone(&i.borrow().inner);
            return Ok(Self::wrap(OverlayInner::Perfect(Overlay::new(base))));
        }
        if let Ok(i) = index.cast::<PyCompactHashIndex>() {
            let base = Arc::clone(&i.borrow().inner);
            return Ok(Self::wrap(OverlayInner::Compact(Overlay::new(base))));
        }
        Err(PyTypeError::new_err(
            "Overlay takes a StringIndex, a PerfectHashIndex or a CompactHashIndex",
        ))
    }

    /// How many keys are live: base keys plus additions, less what has been removed.
    fn __len__(&self, py: Python<'_>) -> usize {
        on_base!(self.lock(py), |ov| ov.len())
    }

    /// Whether every key has been removed (or there were none).
    fn is_empty(&self, py: Python<'_>) -> bool {
        on_base!(self.lock(py), |ov| ov.is_empty())
    }

    /// How many ids have ever been issued. `key(id)` is `None` at or above this.
    fn id_space(&self, py: Python<'_>) -> u64 {
        on_base!(self.lock(py), |ov| ov.id_space())
    }

    /// The id of `key`, or `None` if it is absent or has been removed.
    fn id(&self, py: Python<'_>, key: &str) -> Option<u64> {
        on_base!(self.lock(py), |ov| ov.id(key))
    }

    /// Whether `key` is live.
    fn contains(&self, py: Python<'_>, key: &str) -> bool {
        on_base!(self.lock(py), |ov| ov.contains(key))
    }

    /// Dense id of `key`, raising `KeyError` if it is absent — the dict spelling of
    /// [`id`](Self::id), for a caller who wants a miss to be an error rather than a `None` to
    /// check.
    ///
    /// There is no `__setitem__`, no `keys` / `values` / `items` and no `Mapping` registration:
    /// this is an immutable `str -> int` lookup, and pretending to be a mapping would promise
    /// iteration semantics it does not have.
    fn __getitem__(&self, py: Python<'_>, key: &str) -> PyResult<u64> {
        self.id(py, key)
            .ok_or_else(|| PyKeyError::new_err(key.to_string()))
    }

    /// Dense id of `key`, or `default` (`None` unless given) — `idx.get("apple", -1)`.
    #[pyo3(signature = (key, default=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &str,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match self.id(py, key) {
            Some(id) => Ok(id.into_pyobject(py)?.into_any()),
            None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
        }
    }

    fn __contains__(&self, py: Python<'_>, key: &str) -> bool {
        self.contains(py, key)
    }

    /// Add `key` and return its id. An already-live key keeps the id it has; a removed one is
    /// revived with the id it had, rather than being issued a second one.
    fn add(&self, py: Python<'_>, key: &str) -> u64 {
        on_base!(mut self.lock(py), |ov| ov.add(key))
    }

    /// Remove `key`, returning whether it was there. The id is retired, never reissued.
    ///
    /// Over a `CompactHashIndex` base this inherits that index's false-positive rate: a `contains`
    /// that was never true of a real key can retire an id. Remove by a key you know is present.
    fn remove(&self, py: Python<'_>, key: &str) -> bool {
        on_base!(mut self.lock(py), |ov| ov.remove(key))
    }

    /// Key for `id`, or `None` if it is out of range or retired. Raises `TypeError` on a
    /// `CompactHashIndex` base, which stores no keys.
    fn key(&self, py: Python<'_>, id: u64) -> PyResult<Option<String>> {
        on_keyed_base!(self.lock(py), |ov| ov.key(id))
    }

    /// Every live key, base keys first. Raises `TypeError` on a `CompactHashIndex` base.
    fn keys(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let guard = self.lock(py);
        let inner = &*guard;
        py.detach(|| on_keyed_base!(inner, |ov| ov.keys()))
    }

    /// Fold the edits into a fresh base and return the result. This is the one operation that
    /// renumbers: ids do not survive it. Raises `TypeError` on a `CompactHashIndex` base.
    fn compact(&self, py: Python<'_>) -> PyResult<Self> {
        let guard = self.lock(py);
        let inner = &*guard;
        py.detach(|| {
            Ok(Self::wrap(match inner {
                OverlayInner::String(ov) => {
                    OverlayInner::String(ov.clone().compact().map_err(to_py)?)
                }
                OverlayInner::Perfect(ov) => {
                    OverlayInner::Perfect(ov.clone().compact().map_err(to_py)?)
                }
                OverlayInner::Compact(_) => return Err(no_keys()),
            }))
        })
    }

    /// `compact` written straight to `path` as the base's own blob, without the live keys ever
    /// being held in memory at once; load it with the base class's `load` or `load_mmap` and wrap
    /// it in a new `Overlay`. Returns how many keys the file holds. Raises `TypeError` on a
    /// `CompactHashIndex` base.
    fn compact_to_file(&self, py: Python<'_>, path: PathBuf) -> PyResult<usize> {
        let guard = self.lock(py);
        let inner = &*guard;
        py.detach(|| on_keyed_base!(inner, |ov| ov.compact_to_file(&path).map_err(to_py)?))
    }

    /// `compact`, and the renumbering it did: one native-endian `uint64` per id the overlay had
    /// issued (`np.frombuffer(remap, dtype="uint64")`), the new id of each old one, `2**64 - 1`
    /// where the id was retired -- so an id table kept elsewhere is carried across in one indexing
    /// pass. Raises `TypeError` on a `CompactHashIndex` base.
    fn compact_with_remap<'py>(&self, py: Python<'py>) -> PyResult<(Self, Bound<'py, PyBytes>)> {
        let guard = self.lock(py);
        let inner = &*guard;
        let (fresh, remap) = py.detach(|| match inner {
            OverlayInner::String(ov) => ov
                .clone()
                .compact_with_remap()
                .map(|(f, r)| (OverlayInner::String(f), r))
                .map_err(to_py),
            OverlayInner::Perfect(ov) => ov
                .clone()
                .compact_with_remap()
                .map(|(f, r)| (OverlayInner::Perfect(f), r))
                .map_err(to_py),
            OverlayInner::Compact(_) => Err(no_keys()),
        })?;
        let mut packed = Vec::with_capacity(remap.len() * 8);
        for id in remap {
            packed.extend_from_slice(&id.to_ne_bytes());
        }
        Ok((Self::wrap(fresh), PyBytes::new(py, &packed)))
    }

    /// The index underneath, unchanged and shared with this overlay.
    fn base(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(match &*self.lock(py) {
            OverlayInner::String(ov) => Py::new(
                py,
                PyStringIndex {
                    inner: Arc::clone(ov.base()),
                },
            )?
            .into_any(),
            OverlayInner::Perfect(ov) => Py::new(
                py,
                PyPerfectHashIndex {
                    inner: Arc::clone(ov.base()),
                },
            )?
            .into_any(),
            OverlayInner::Compact(ov) => Py::new(
                py,
                PyCompactHashIndex {
                    inner: Arc::clone(ov.base()),
                },
            )?
            .into_any(),
        })
    }

    /// Serialise the base, the additions and the retired ids to one `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let guard = self.lock(py);
        let inner = &*guard;
        let bytes = py
            .detach(|| on_base!(inner, |ov| ov.to_bytes()))
            .map_err(to_py)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Write [`to_bytes`](Self::to_bytes) to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        let guard = self.lock(py);
        let inner = &*guard;
        py.detach(|| on_base!(inner, |ov| ov.save(&path)))
            .map_err(to_py)
    }

    /// Read a blob written by [`to_bytes`](Self::to_bytes), rebuilding the base with `base`'s own
    /// loader — pass the class, not an instance.
    ///
    /// The blob records which base wrote it and a mismatch is refused, so the wrong class is an
    /// error rather than an unchecked read of bytes meant for something else.
    ///
    /// **Validated and checksummed throughout.** The header carries a check of its own and a hash
    /// of everything after it, verified before any of it is read, so a flipped bit anywhere — in an
    /// addition that stays valid UTF-8, in a tombstone word that would revive a removed id — raises
    /// `ValueError` instead of loading as something else. Past the checksums the framing is checked
    /// too, because a hash vouches for transport and not for what was written: the lengths, the
    /// additions and their UTF-8, the tombstones against the id space, and the base region by that
    /// class's own loader.
    ///
    /// A blob written by `0.12` still loads, and gets every check above except the two checksums,
    /// which that format does not carry. Saving it again writes the current format and it gains
    /// them.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8], base: &Bound<'_, PyType>) -> PyResult<Self> {
        Self::load_blob(py, data, base, BaseTrust::Own)
    }

    /// Pickle support: the blob, the loader, and the class of the base underneath it.
    ///
    /// The overlay's own loader takes that class — an `OVL2` blob records which base wrote it and
    /// refuses a mismatch — so it has to travel with the bytes. Everything else is as the indexes:
    /// the base is copied into the pickle, not referenced by path.
    fn __reduce__<'py>(&self, py: Python<'py>) -> PyResult<OverlayReduce<'py>> {
        // Scoped: `to_bytes` takes the same lock, and it is not reentrant.
        let base = {
            match &*self.lock(py) {
                OverlayInner::String(_) => py.get_type::<PyStringIndex>(),
                OverlayInner::Perfect(_) => py.get_type::<PyPerfectHashIndex>(),
                OverlayInner::Compact(_) => py.get_type::<PyCompactHashIndex>(),
            }
        };
        let from_bytes = py.get_type::<Self>().getattr("from_bytes")?;
        Ok((from_bytes, (self.to_bytes(py)?, base)))
    }

    /// [`from_bytes`](Self::from_bytes) for a blob **someone else wrote**.
    ///
    /// The overlay's own framing is checked identically either way — magic, both checksums, the
    /// section lengths, the additions and their UTF-8, the tombstones. What changes is the loader
    /// the *embedded base* is handed to, and it matters for exactly one base: a `StringIndex`
    /// region can panic `from_bytes` (`PanicException`, see `StringIndex.from_untrusted_bytes`),
    /// and an overlay frame passes every check it makes for itself before that region is reached.
    /// Over a `PerfectHashIndex` or `CompactHashIndex` base this is the same work as `from_bytes`,
    /// because those loaders are already total.
    #[staticmethod]
    fn from_untrusted_bytes(
        py: Python<'_>,
        data: &[u8],
        base: &Bound<'_, PyType>,
    ) -> PyResult<Self> {
        Self::load_blob(py, data, base, BaseTrust::Stranger)
    }

    /// [`from_bytes`](Self::from_bytes) from a file: checksummed and validated the same way.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf, base: &Bound<'_, PyType>) -> PyResult<Self> {
        let data = py
            .detach(|| std::fs::read(&path))
            .map_err(|e| PyIOError::new_err(e.to_string()))?;
        Self::load_blob(py, &data, base, BaseTrust::Own)
    }

    /// [`from_untrusted_bytes`](Self::from_untrusted_bytes) from a file: the overlay's framing
    /// checked as always, the embedded base handed to the strict loader.
    #[staticmethod]
    fn load_untrusted(py: Python<'_>, path: PathBuf, base: &Bound<'_, PyType>) -> PyResult<Self> {
        let data = py
            .detach(|| std::fs::read(&path))
            .map_err(|e| PyIOError::new_err(e.to_string()))?;
        Self::load_blob(py, &data, base, BaseTrust::Stranger)
    }
}

/// How far an overlay's *embedded base* is to be trusted. The overlay's own framing is checked the
/// same way either way; this only chooses the loader the base region is handed to, and it matters
/// for exactly one base — `StringIndex`, whose ordinary loader may panic on a crafted transducer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BaseTrust {
    Own,
    Stranger,
}

impl PyOverlay {
    fn load_blob(
        py: Python<'_>,
        data: &[u8],
        base: &Bound<'_, PyType>,
        trust: BaseTrust,
    ) -> PyResult<Self> {
        let inner = if base.is(py.get_type::<PyStringIndex>()) {
            OverlayInner::String(
                Overlay::from_bytes_with(data, |b| {
                    match trust {
                        BaseTrust::Own => StringIndex::from_bytes(b),
                        BaseTrust::Stranger => StringIndex::from_untrusted_bytes(b),
                    }
                    .map(Arc::new)
                })
                .map_err(to_py)?,
            )
        } else if base.is(py.get_type::<PyPerfectHashIndex>()) {
            OverlayInner::Perfect(
                Overlay::from_bytes_with(data, |b| PerfectHashIndex::from_bytes(b).map(Arc::new))
                    .map_err(to_py)?,
            )
        } else if base.is(py.get_type::<PyCompactHashIndex>()) {
            OverlayInner::Compact(
                Overlay::from_bytes_with(data, |b| CompactHashIndex::from_bytes(b).map(Arc::new))
                    .map_err(to_py)?,
            )
        } else {
            return Err(PyTypeError::new_err(
                "base must be StringIndex, PerfectHashIndex or CompactHashIndex (the class itself)",
            ));
        };
        Ok(Self::wrap(inner))
    }
}

/// What `ids_into` asks of `out` when there is nothing to write: a buffer, and a writable one. Not
/// the typed view `id_sink` takes -- an empty `array.array` hands out a pointer that fails the
/// alignment check on some interpreter builds, and there is no item for alignment to matter to.
/// One chunk of an Arrow `utf8` / `large_utf8` column, copied out of the three buffers a pyarrow
/// array exposes — validity, offsets, data — so a lookup can run with the GIL released over
/// memory it owns. The copy is the price of the buffer protocol over the C Data Interface, and it
/// is a `memcpy` against a hash and a cache miss per key; no per-key Python object exists at all.
struct Utf8Chunk {
    len: usize,
    /// The array's `offset`: the index of its first element in `offsets` and `validity`.
    first: usize,
    /// `large_utf8`: eight-byte offsets.
    wide: bool,
    validity: Option<Vec<u8>>,
    offsets: Vec<u8>,
    data: Vec<u8>,
}

impl Utf8Chunk {
    fn of(array: &Bound<'_, PyAny>) -> PyResult<Self> {
        let py = array.py();
        let kind = array.getattr("type")?.str()?.to_string_lossy().into_owned();
        let wide = match kind.as_str() {
            "string" => false,
            "large_string" => true,
            other => {
                return Err(PyTypeError::new_err(format!(
                    "expected an Arrow utf8 or large_utf8 column, got {other}"
                )));
            }
        };
        let len = array.len()?;
        let empty = Self {
            len: 0,
            first: 0,
            wide,
            validity: None,
            offsets: Vec::new(),
            data: Vec::new(),
        };
        if len == 0 {
            return Ok(empty);
        }
        let first: usize = array.getattr("offset")?.extract()?;
        let null_count: usize = array.getattr("null_count")?.extract()?;
        let buffers: Vec<Option<Bound<'_, PyAny>>> = array.call_method0("buffers")?.extract()?;
        let [validity, offsets, data] = <[_; 3]>::try_from(buffers)
            .map_err(|_| PyTypeError::new_err("an Arrow utf8 column has three buffers"))?;
        let copy = |b: &Bound<'_, PyAny>| -> PyResult<Vec<u8>> {
            let view = PyMemoryView::from(b)?.call_method1("cast", ("B",))?;
            PyBuffer::<u8>::get(&view)?.to_vec(py)
        };
        let validity = match (null_count, validity) {
            (0, _) => None,
            (_, Some(v)) => Some(copy(&v)?),
            (_, None) => {
                return Err(PyValueError::new_err(
                    "the column reports nulls but has no validity buffer",
                ));
            }
        };
        let offsets = match offsets {
            Some(o) => copy(&o)?,
            None => return Err(PyValueError::new_err("the column has no offsets buffer")),
        };
        let data = match data {
            Some(d) => copy(&d)?,
            None => Vec::new(),
        };
        let chunk = Self {
            len,
            first,
            wide,
            validity,
            offsets,
            data,
        };
        chunk.check()?;
        Ok(chunk)
    }

    fn width(&self) -> usize {
        if self.wide { 8 } else { 4 }
    }

    /// Offset number `i` as the buffer holds it, sign and all.
    fn raw_offset(&self, i: usize) -> i64 {
        let at = i * self.width();
        if self.wide {
            i64::from_ne_bytes(self.offsets[at..at + 8].try_into().expect("8 bytes"))
        } else {
            i64::from(i32::from_ne_bytes(
                self.offsets[at..at + 4].try_into().expect("4 bytes"),
            ))
        }
    }

    /// Every offset this chunk will read exists, is non-negative, ascends, and stays inside the
    /// data — checked once here so that `key` can slice without a fallible path per element.
    fn check(&self) -> PyResult<()> {
        let end = self.first + self.len;
        if self.offsets.len() < (end + 1) * self.width() {
            return Err(PyValueError::new_err(
                "the column's offsets buffer is shorter than the column",
            ));
        }
        let mut prev = 0i64;
        for i in self.first..=end {
            let o = self.raw_offset(i);
            if o < prev || o > self.data.len() as i64 {
                return Err(PyValueError::new_err(
                    "the column's offsets do not ascend within its data buffer",
                ));
            }
            prev = o;
        }
        if self.validity.as_ref().is_some_and(|v| v.len() * 8 < end) {
            return Err(PyValueError::new_err(
                "the column's validity buffer is shorter than the column",
            ));
        }
        Ok(())
    }

    fn key(&self, i: usize) -> &[u8] {
        let a = self.raw_offset(self.first + i) as usize;
        let b = self.raw_offset(self.first + i + 1) as usize;
        &self.data[a..b]
    }

    fn valid(&self, i: usize) -> bool {
        let at = self.first + i;
        self.validity
            .as_ref()
            .is_none_or(|v| v[at / 8] >> (at % 8) & 1 == 1)
    }

    fn has_nulls(&self) -> bool {
        (0..self.len).any(|i| !self.valid(i))
    }
}

/// The chunks of whatever holds an Arrow string column: a pyarrow `Array` (one) or `ChunkedArray`
/// (its `chunks`), anything with `__arrow_array__` (a pandas `ArrowDtype` column's `.array`, or
/// the column itself), or a polars `Series` through `to_arrow()`.
fn utf8_chunks(column: &Bound<'_, PyAny>) -> PyResult<Vec<Utf8Chunk>> {
    if column.hasattr("buffers")? {
        return Ok(vec![Utf8Chunk::of(column)?]);
    }
    if column.hasattr("chunks")? {
        let chunks: Vec<Bound<'_, PyAny>> = column.getattr("chunks")?.extract()?;
        return chunks.iter().map(Utf8Chunk::of).collect();
    }
    if column.hasattr("__arrow_array__")? {
        return utf8_chunks(&column.call_method0("__arrow_array__")?);
    }
    if let Ok(inner) = column.getattr("array") {
        if inner.hasattr("__arrow_array__")? {
            return utf8_chunks(&inner.call_method0("__arrow_array__")?);
        }
    }
    if column.hasattr("to_arrow")? {
        return utf8_chunks(&column.call_method0("to_arrow")?);
    }
    Err(PyTypeError::new_err(
        "expected a pyarrow utf8/large_utf8 Array or ChunkedArray, a pandas ArrowDtype column, \
         or a polars Series",
    ))
}

fn total_len(chunks: &[Utf8Chunk]) -> usize {
    chunks.iter().map(|c| c.len).sum()
}

fn packed<T: Copy, const W: usize>(ids: &[T], bytes: impl Fn(T) -> [u8; W]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ids.len() * W);
    for &id in ids {
        out.extend_from_slice(&bytes(id));
    }
    out
}

fn writable_buffer(out: &Bound<'_, PyAny>) -> PyResult<()> {
    let view = PyMemoryView::from(out)?;
    if view.getattr("readonly")?.is_truthy()? {
        return Err(PyBufferError::new_err("out is read-only"));
    }
    Ok(())
}

/// The buffer `ids_into` fills: `T`-typed, writable, C-contiguous and at least `n` items long.
fn id_sink<T: pyo3::buffer::Element>(out: &Bound<'_, PyAny>, n: usize) -> PyResult<PyBuffer<T>> {
    let buf = PyBuffer::<T>::get(out)?;
    if buf.readonly() {
        return Err(PyBufferError::new_err("out is read-only"));
    }
    if !buf.is_c_contiguous() {
        return Err(PyBufferError::new_err("out is not C-contiguous"));
    }
    if buf.item_count() < n {
        return Err(PyValueError::new_err(format!(
            "out holds {} items but {n} keys were given",
            buf.item_count()
        )));
    }
    Ok(buf)
}

/// Writes `ids` over the head of a buffer `id_sink` accepted.
fn write_ids<T: pyo3::buffer::Element>(
    py: Python<'_>,
    buf: &PyBuffer<T>,
    ids: &[T],
) -> PyResult<()> {
    let cells = buf
        .as_mut_slice(py)
        .ok_or_else(|| PyBufferError::new_err("out is not writable"))?;
    for (cell, &id) in cells.iter().zip(ids) {
        cell.set(id);
    }
    Ok(())
}

/// `gil_used = false` is spelled out rather than left to PyO3's default, which is already `false`:
/// the claim ships either way, so it should be one somebody checked. What backs it, measured on
/// CPython 3.14t with eight threads: the three index types are immutable after building and are
/// `Send + Sync`, so sharing one and calling `id`/`contains`/`ids_of` from every thread is sound and
/// raises nothing; the two types that do hold mutable state — the `StringIndex` iterator and
/// `Overlay` — are `frozen` with that state behind a lock, so sharing one of those serialises
/// instead of raising `Already borrowed`.
/// What a blob is, from its header alone: its kind, its format and the sizes a caller would
/// otherwise have to load it to learn. `blob` is a path or `bytes`; over a path only the header
/// and the footer are read, so an index of gigabytes inspects in microseconds (an overlay's
/// tombstone words are read too, to count its retired ids). Nothing is decoded
/// or verified -- a blob that inspects cleanly may still fail to load, and the sizes are what the
/// header claims. A blob from before 1.0 is a `ValueError` naming the type to rebuild.
#[pyfunction(name = "inspect")]
fn py_inspect<'py>(py: Python<'py>, blob: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let info = if let Ok(bytes) = blob.cast::<PyBytes>() {
        crate::inspect(bytes.as_bytes()).map_err(to_py)?
    } else {
        let path: PathBuf = blob
            .extract()
            .map_err(|_| PyTypeError::new_err("inspect() takes a path or bytes"))?;
        py.detach(|| crate::inspect_file(&path)).map_err(to_py)?
    };
    blob_info(py, &info)
}

fn blob_info<'py>(py: Python<'py>, info: &crate::BlobInfo) -> PyResult<Bound<'py, PyDict>> {
    use crate::BlobKind;
    let d = PyDict::new(py);
    d.set_item(
        "kind",
        match info.kind {
            BlobKind::StringIndex => "StringIndex",
            BlobKind::PerfectHashIndex => "PerfectHashIndex",
            BlobKind::CompactHashIndex => "CompactHashIndex",
            BlobKind::ClosedHashIndex => "ClosedHashIndex",
            BlobKind::DictIndex => "DictIndex",
            BlobKind::Mphf => "Mphf",
            BlobKind::Overlay => "Overlay",
        },
    )?;
    d.set_item("format", &info.format)?;
    d.set_item("bytes", info.bytes)?;
    d.set_item("keys", info.keys)?;
    d.set_item("fingerprint_bits", info.fingerprint_bits)?;
    d.set_item("mph_bytes", info.mph_bytes)?;
    d.set_item("arena_bytes", info.arena_bytes)?;
    d.set_item("side_entries", info.side_entries)?;
    let overlay = match &info.overlay {
        Some(o) => {
            let od = PyDict::new(py);
            od.set_item("base_tag", o.base_tag)?;
            let base = match &o.base {
                Some(b) => Some(blob_info(py, b)?),
                None => None,
            };
            od.set_item("base", base)?;
            od.set_item("additions", o.additions)?;
            od.set_item("retired", o.retired)?;
            Some(od)
        }
        None => None,
    };
    d.set_item("overlay", overlay)?;
    Ok(d)
}

#[pymodule(gil_used = false)]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStringIndex>()?;
    m.add_class::<StringIndexIterator>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyPerfectHashIndex>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyCompactHashIndex>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyClosedHashIndex>()?;
    m.add_class::<PyOverlay>()?;
    m.add_class::<PyDictIndex>()?;
    m.add_class::<DictIndexIterator>()?;
    m.add_function(wrap_pyfunction!(py_inspect, m)?)?;
    Ok(())
}
