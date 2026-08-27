//! Shared, immutable byte storage an index can borrow from without copying.
//!
//! [`SharedBytes`] is a cheaply-clonable (`Arc`-bump), range-limited, **`'static`** view of one byte
//! source — either an owned buffer or a read-only memory map. Because it owns an `Arc` to the source,
//! it can back an [`fst::Map`](fst::Map), a fingerprint table, or a key arena directly, with no
//! self-referential borrow and no `unsafe` beyond the single documented `Mmap::map`. This is what lets
//! `from_bytes` (owned) and `load_mmap` (zero-copy) share one code path and one stored type.

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

/// Best-effort prefetch of the cache line holding `data[i]`; no-op off x86_64 or out of range.
#[inline(always)]
#[cfg(feature = "mph")]
pub(crate) fn prefetch_byte(data: &[u8], i: usize) {
    #[cfg(target_arch = "x86_64")]
    if i < data.len() {
        // SAFETY: prefetch has no observable effect beyond cache state, and the pointer is in
        // bounds by the check above.
        unsafe {
            std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(
                data.as_ptr().add(i).cast(),
            )
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (data, i);
}

/// Write `bytes` to `path` by writing a sibling temporary file and renaming it into place. A crash
/// or a full disk mid-write then leaves the previous file intact instead of a truncated index that
/// still has a valid magic; `rename` within a directory is atomic on both POSIX and Windows.
pub(crate) fn write_atomically(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(), crate::IndexError> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("index"));
    // pid + a process-wide counter: two threads saving to one path must not share a temporary.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = dir.join(stem);
    tmp.as_mut_os_string()
        .push(format!(".{}.{seq}.tmp", std::process::id()));
    let mut file = std::fs::File::create(&tmp)?;
    // Ordered by what has to survive: bytes durable first, then the rename that publishes them.
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    written?;
    Ok(())
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
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lexindex_atomic_{}.bin", std::process::id()));
        write_atomically(&path, b"first").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("lexindex_atomic_") && name.ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_file(&path).ok();
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
