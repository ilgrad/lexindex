//! PyO3 bindings: expose [`StringIndex`] and [`PerfectHashIndex`] to Python as `betula_index._core`.
//!
//! Thin wrappers over the Rust types — every method delegates to the core and maps [`IndexError`] to a
//! Python exception. Built as an abi3 extension (CPython ≥ 3.11) under the `python` feature.

use crate::{IndexError, StringIndex};
use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

#[cfg(feature = "mph")]
use crate::PerfectHashIndex;

fn to_py(e: IndexError) -> PyErr {
    match e {
        IndexError::Io(_) => PyIOError::new_err(e.to_string()),
        _ => PyValueError::new_err(e.to_string()),
    }
}

/// Ordered string↔id index (FST) with prefix / range / fuzzy / subsequence queries.
#[pyclass(name = "StringIndex", module = "betula_index._core", frozen)]
pub struct PyStringIndex {
    inner: StringIndex,
}

#[pymethods]
impl PyStringIndex {
    /// Build from an iterable of strings (duplicates removed; ids are sorted rank).
    #[new]
    fn new(items: Vec<String>) -> PyResult<Self> {
        Ok(Self {
            inner: StringIndex::build(items).map_err(to_py)?,
        })
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
        self.inner.key(id).map(str::to_owned)
    }

    /// `(key, id)` pairs whose key starts with `prefix`, lexicographically ordered.
    fn prefix(&self, prefix: &str) -> Vec<(String, u64)> {
        self.inner.prefix(prefix)
    }

    /// `(key, id)` pairs with `lo <= key < hi`, lexicographically ordered.
    fn range(&self, lo: &str, hi: &str) -> Vec<(String, u64)> {
        self.inner.range(lo, hi)
    }

    /// `(key, id)` pairs within Levenshtein edit distance `max_distance` of `query`.
    fn fuzzy(&self, query: &str, max_distance: u32) -> PyResult<Vec<(String, u64)>> {
        self.inner.fuzzy(query, max_distance).map_err(to_py)
    }

    /// `(key, id)` pairs whose key contains `query` as a subsequence.
    fn subsequence(&self, query: &str) -> Vec<(String, u64)> {
        self.inner.subsequence(query)
    }

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.to_bytes())
    }

    /// Reconstruct from a [`PyStringIndex::to_bytes`] blob.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: StringIndex::from_bytes(data).map_err(to_py)?,
        })
    }

    /// Write the index to `path`.
    fn save(&self, path: &str) -> PyResult<()> {
        self.inner.save(path).map_err(to_py)
    }

    /// Load an index previously written with `save`.
    #[staticmethod]
    fn load(path: &str) -> PyResult<Self> {
        Ok(Self {
            inner: StringIndex::load(path).map_err(to_py)?,
        })
    }
}

/// Minimal-perfect-hash dictionary: fastest exact `string → dense id`, with persistence.
#[cfg(feature = "mph")]
#[pyclass(name = "PerfectHashIndex", module = "betula_index._core", frozen)]
pub struct PyPerfectHashIndex {
    inner: PerfectHashIndex,
}

#[cfg(feature = "mph")]
#[pymethods]
impl PyPerfectHashIndex {
    /// Build from an iterable of strings (duplicates removed; ids are arbitrary dense slots).
    #[new]
    fn new(items: Vec<String>) -> PyResult<Self> {
        Ok(Self {
            inner: PerfectHashIndex::build(items).map_err(to_py)?,
        })
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
    fn key(&self, id: u32) -> Option<String> {
        self.inner.key(id).map(str::to_owned)
    }

    /// Serialise to a `bytes` blob.
    fn to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        Ok(PyBytes::new(py, &self.inner.to_bytes().map_err(to_py)?))
    }

    /// Reconstruct from a [`PyPerfectHashIndex::to_bytes`] blob.
    #[staticmethod]
    fn from_bytes(data: &[u8]) -> PyResult<Self> {
        Ok(Self {
            inner: PerfectHashIndex::from_bytes(data).map_err(to_py)?,
        })
    }

    /// Write the dictionary to `path`.
    fn save(&self, path: &str) -> PyResult<()> {
        self.inner.save(path).map_err(to_py)
    }

    /// Load a dictionary previously written with `save`.
    #[staticmethod]
    fn load(path: &str) -> PyResult<Self> {
        Ok(Self {
            inner: PerfectHashIndex::load(path).map_err(to_py)?,
        })
    }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStringIndex>()?;
    #[cfg(feature = "mph")]
    m.add_class::<PyPerfectHashIndex>()?;
    Ok(())
}
