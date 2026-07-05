//! Shared, immutable byte storage an index can borrow from without copying.
//!
//! [`SharedBytes`] is a cheaply-clonable (`Arc`-bump), range-limited, **`'static`** view of one byte
//! source — either an owned buffer or a read-only memory map. Because it owns an `Arc` to the source,
//! it can back an [`fst::Map`](fst::Map) or a front-coded dictionary directly, with no self-referential
//! borrow and no `unsafe` beyond the single documented `Mmap::map`. This is what lets `from_bytes`
//! (owned) and `load_mmap` (zero-copy) share one code path and one stored type.

use std::sync::Arc;

/// The backing store for a [`SharedBytes`]: an owned heap buffer or a read-only memory map.
#[derive(Clone)]
enum Source {
    Owned(Arc<[u8]>),
    #[cfg(feature = "mmap")]
    Mapped(Arc<memmap2::Mmap>),
}

impl Source {
    #[inline]
    fn bytes(&self) -> &[u8] {
        match self {
            Source::Owned(b) => b,
            #[cfg(feature = "mmap")]
            Source::Mapped(m) => m,
        }
    }
}

/// A cheap-to-clone, range-limited view into a shared byte source.
#[derive(Clone)]
pub(crate) struct SharedBytes {
    src: Source,
    start: usize,
    end: usize,
}

impl SharedBytes {
    /// Wrap an owned buffer (one heap copy at the boundary; querying never copies again).
    pub(crate) fn from_owned(bytes: Vec<u8>) -> Self {
        let end = bytes.len();
        Self {
            src: Source::Owned(Arc::from(bytes.into_boxed_slice())),
            start: 0,
            end,
        }
    }

    /// Wrap a read-only memory map — the zero-copy path. The `Arc` keeps the map alive for as long as
    /// any view (or `fst::Map`) borrows it, and lets the pages be shared across clones and processes.
    #[cfg(feature = "mmap")]
    pub(crate) fn from_mmap(mmap: Arc<memmap2::Mmap>) -> Self {
        let end = mmap.len();
        Self {
            src: Source::Mapped(mmap),
            start: 0,
            end,
        }
    }

    /// A sub-view `[start, end)`, measured within this view; `None` if it would fall out of range.
    pub(crate) fn subslice(&self, start: usize, end: usize) -> Option<Self> {
        if start > end {
            return None;
        }
        let abs_start = self.start.checked_add(start)?;
        let abs_end = self.start.checked_add(end)?;
        if abs_end > self.end {
            return None;
        }
        Some(Self {
            src: self.src.clone(),
            start: abs_start,
            end: abs_end,
        })
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }
}

impl AsRef<[u8]> for SharedBytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self.src.bytes()[self.start..self.end]
    }
}

impl std::ops::Deref for SharedBytes {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_ref()
    }
}

impl std::fmt::Debug for SharedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedBytes({} bytes)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_view_and_subslice() {
        let sb = SharedBytes::from_owned(b"hello world".to_vec());
        assert_eq!(sb.len(), 11);
        assert_eq!(sb.as_ref(), b"hello world");
        let world = sb.subslice(6, 11).unwrap();
        assert_eq!(world.as_ref(), b"world");
        // sub-view of a sub-view composes and stays in range
        assert_eq!(world.subslice(0, 3).unwrap().as_ref(), b"wor");
        assert!(world.subslice(0, 6).is_none()); // past the end of the view
        assert!(sb.subslice(5, 4).is_none()); // start > end
    }

    #[test]
    fn clone_is_cheap_and_shares() {
        let sb = SharedBytes::from_owned(vec![1, 2, 3, 4]);
        let a = sb.subslice(1, 3).unwrap();
        let b = a.clone();
        assert_eq!(a.as_ref(), b.as_ref());
        assert_eq!(&*b, &[2, 3]);
    }
}
