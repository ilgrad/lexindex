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

use crate::{IndexError, StringIndex};
use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedStr;
use pyo3::types::{PyBytes, PyString};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

#[cfg(feature = "mph")]
use crate::{CompactHashIndex, PerfectHashIndex};

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
fn stream_strs<'a>(
    items: &'a Bound<'a, PyAny>,
    err: Rc<RefCell<Option<PyErr>>>,
) -> PyResult<impl Iterator<Item = String> + 'a> {
    let mut it = items.try_iter()?;
    Ok(std::iter::from_fn(move || {
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
    }))
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
    inner: StringIndex,
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
        Ok(Self { inner })
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
        let built = StringIndex::build_sorted(stream_strs(items, Rc::clone(&err))?);
        // Checked after the build rather than during it: nothing is published either way, and a
        // discarded partial index is not observable.
        if let Some(e) = err.borrow_mut().take() {
            return Err(e);
        }
        Ok(Self {
            inner: built.map_err(to_py)?,
        })
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
            stream_strs(items, Rc::clone(&err))?,
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
            buf: Vec::new().into_iter(),
            resume: None,
            remaining,
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

    /// Reconstruct from a [`PyStringIndex::to_bytes`] blob.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        let inner = py.detach(|| StringIndex::from_bytes(data)).map_err(to_py)?;
        Ok(Self { inner })
    }

    /// Write the index to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load an index previously written with `save`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        let inner = py.detach(|| StringIndex::load(&path)).map_err(to_py)?;
        Ok(Self { inner })
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
        Ok(Self { inner })
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
#[pyclass(name = "StringIndexIterator", module = "lexindex._core")]
pub struct StringIndexIterator {
    parent: Py<PyStringIndex>,
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

impl StringIndexIterator {
    /// Decode the next chunk, at most `remaining` pairs, and count them off.
    fn refill(&mut self, py: Python<'_>) {
        let want = ITER_CHUNK.min(self.remaining as usize);
        let parent = self.parent.clone_ref(py);
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

    fn __next__(&mut self, py: Python<'_>) -> Option<(String, u64)> {
        loop {
            if let Some(item) = self.buf.next() {
                return Some(item);
            }
            if self.remaining == 0 {
                return None;
            }
            // Every trip either yields, or strictly decreases `remaining`, or stops here — the
            // stream running dry before `len()` says it should is a malformed index, not a retry.
            let before = self.remaining;
            self.refill(py);
            if self.remaining == before {
                return None;
            }
        }
    }
}

/// Minimal-perfect-hash dictionary: fastest exact `string → dense id`, with persistence.
#[cfg(feature = "mph")]
#[pyclass(name = "PerfectHashIndex", module = "lexindex._core", frozen)]
pub struct PyPerfectHashIndex {
    inner: PerfectHashIndex,
}

#[cfg(feature = "mph")]
#[pymethods]
impl PyPerfectHashIndex {
    /// Build from an iterable of strings (duplicates removed; ids are arbitrary dense slots).
    #[new]
    fn new(py: Python<'_>, items: &Bound<'_, PyAny>) -> PyResult<Self> {
        let items = collect_strs(items)?;
        let inner = py
            .detach(|| PerfectHashIndex::build(items.iter()))
            .map_err(to_py)?;
        Ok(Self { inner })
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

    /// Key for `id`, or `None` if out of range.
    fn key<'py>(&self, py: Python<'py>, id: u32) -> Option<Bound<'py, PyString>> {
        self.inner.key(id).map(|k| PyString::new(py, k))
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys` (`None` where absent).
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u32>> {
        py.detach(|| self.inner.ids_of(&keys))
    }

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

    /// Reconstruct from a [`PyPerfectHashIndex::to_bytes`] blob.
    ///
    /// The blob must have been produced by this library. Its framing is validated, but the embedded
    /// perfect hash cannot be — a deliberately crafted blob is undefined behaviour.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { PerfectHashIndex::from_bytes(data) })
            .map_err(to_py)?;
        Ok(Self { inner })
    }

    /// Write the dictionary to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a dictionary previously written with `save`.
    ///
    /// The file must have been written by this library — see `from_bytes` for why a crafted blob
    /// cannot be rejected.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { PerfectHashIndex::load(&path) })
            .map_err(to_py)?;
        Ok(Self { inner })
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
        Ok(Self { inner })
    }
}

/// Fingerprint minimal-perfect-hash dictionary: the smallest `string -> dense id` map. Membership is
/// probabilistic (false-positive rate `2 ** -fingerprint_bits`) and there is no reverse `id -> key`.
#[cfg(feature = "mph")]
#[pyclass(name = "CompactHashIndex", module = "lexindex._core", frozen)]
pub struct PyCompactHashIndex {
    inner: CompactHashIndex,
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
        // Unlike the key-storing indexes, this one needs only 16 hashed bytes per key — so the
        // items are hashed as they come off the iterator (under the GIL) and the strings dropped,
        // keeping a generator-fed build streaming on the Python side too.
        let pairs = collect_pairs(items)?;
        let inner = py
            .detach(|| CompactHashIndex::build_from_pairs(pairs, bits))
            .map_err(to_py)?;
        Ok(Self { inner })
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

    /// Whether `key` is present (subject to the false-positive rate).
    fn contains(&self, key: &str) -> bool {
        self.inner.contains(key)
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with `keys` (`None` where absent).
    fn ids_of(&self, py: Python<'_>, keys: Vec<PyBackedStr>) -> Vec<Option<u32>> {
        py.detach(|| self.inner.ids_of(&keys))
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

    /// Reconstruct from a [`PyCompactHashIndex::to_bytes`] blob.
    ///
    /// The blob must have been produced by this library. Its framing is validated, but the embedded
    /// perfect hash cannot be — a deliberately crafted blob is undefined behaviour.
    #[staticmethod]
    fn from_bytes(py: Python<'_>, data: &[u8]) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { CompactHashIndex::from_bytes(data) })
            .map_err(to_py)?;
        Ok(Self { inner })
    }

    /// Write the dictionary to `path`.
    fn save(&self, py: Python<'_>, path: PathBuf) -> PyResult<()> {
        py.detach(|| self.inner.save(&path)).map_err(to_py)
    }

    /// Load a dictionary previously written with `save`.
    ///
    /// The file must have been written by this library — see `from_bytes` for why a crafted blob
    /// cannot be rejected.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> PyResult<Self> {
        // SAFETY: forwarded to the caller (see the docstring); unenforceable from Python.
        let inner = py
            .detach(|| unsafe { CompactHashIndex::load(&path) })
            .map_err(to_py)?;
        Ok(Self { inner })
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
        Ok(Self { inner })
    }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStringIndex>()?;
    m.add_class::<StringIndexIterator>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyPerfectHashIndex>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyCompactHashIndex>()?;
    Ok(())
}
