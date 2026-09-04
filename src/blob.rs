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

// Process-wide temp-name counter (module scope so a test can predict the next name); keeps two
// threads saving to one path from colliding.
static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write to `path` by streaming `write`'s output into a sibling temporary file and renaming it into
/// place, so a crash, a full disk, or a power loss mid-write leaves the previous file intact rather
/// than a truncated index that still has a valid magic. `rename` within a directory is atomic on
/// both POSIX and Windows; the temporary is opened `O_EXCL` (never following a planted symlink),
/// and on Unix the parent directory is fsynced so the rename itself is durable. Streaming section
/// by section means `save` peaks at the index's own memory, not index + a full serialised copy.
pub(crate) fn write_atomically_with(
    path: &std::path::Path,
    // `IndexError`, not `io::Result`: a streaming builder writing through this closure fails on the
    // caller's input (keys out of order) as readily as on the disk, and laundering that through
    // `io::Error::other` would report a precondition violation as an I/O fault.
    write: impl FnOnce(&mut std::io::BufWriter<&mut std::fs::File>) -> Result<(), crate::IndexError>,
) -> Result<(), crate::IndexError> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("index"));
    // Open the temporary with O_CREAT|O_EXCL (`create_new`) rather than `File::create`: an attacker
    // who plants a symlink at the — predictable — temp name in a shared directory would otherwise
    // have it followed and the target truncated. `create_new` refuses any existing path, symlink
    // included, so the write can only land on a fresh file we made. The pid+counter name keeps two
    // threads (or a stale temp from a crashed run) from colliding; on the rare collision we retry
    // with the next counter, bounded so a hostile racer cannot spin us forever.
    let (tmp, mut file) = {
        let mut attempt = Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        for _ in 0..128 {
            let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut tmp = dir.join(stem);
            tmp.as_mut_os_string()
                .push(format!(".{}.{seq}.tmp", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
            {
                Ok(f) => {
                    attempt = Ok((tmp, f));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        attempt?
    };
    // Replacing an existing file should keep its permissions, not silently widen them to the umask
    // default (best effort, Unix only — ACLs and xattrs are out of scope).
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(meta.permissions().mode()));
    }
    // Ordered by what has to survive: bytes durable first, then the rename that publishes them.
    let written = (|| -> Result<(), crate::IndexError> {
        let mut w = std::io::BufWriter::new(&mut file);
        write(&mut w)?;
        w.flush()?;
        drop(w);
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    written?;
    // fsync the directory so the rename entry itself — not just the file's bytes — survives a power
    // loss. A no-op on Windows, where directories are not fsync targets and rename is durable.
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
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
        write_atomically_with(&path, |w| Ok(std::io::Write::write_all(w, b"first")?)).unwrap();
        write_atomically_with(&path, |w| Ok(std::io::Write::write_all(w, b"second")?)).unwrap();
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

    /// A symlink planted at the predictable temp name must not be followed and made to truncate its
    /// target: `create_new` refuses the pre-existing path, so the save still lands correctly.
    #[cfg(unix)]
    #[test]
    fn atomic_write_does_not_follow_a_planted_temp_symlink() {
        use std::sync::atomic::Ordering;
        let dir = std::env::temp_dir().join(format!("lexindex_symlink_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, b"do not truncate me").unwrap();
        let target = dir.join("catalog.bin");
        // Plant a symlink at the exact next temp name the writer will try.
        let seq = WRITE_SEQ.load(Ordering::Relaxed);
        let planted = dir.join(format!("catalog.bin.{}.{seq}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        write_atomically_with(&target, |w| Ok(std::io::Write::write_all(w, b"payload")?)).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"payload"); // save landed
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not truncate me"); // victim untouched
        std::fs::remove_dir_all(&dir).ok();
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
