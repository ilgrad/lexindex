//! A table on transparent huge pages.
//!
//! A table of a few megabytes read at random misses the TLB on nearly every lookup when it sits
//! on 4 KiB pages — thousands of them — and the page walk that follows is a chain of loads beside
//! the one that matters. On 2 MiB pages the same table is a dozen entries. Linux backs a range
//! with huge pages at its first touch if the range was advised so before it (under the common
//! `transparent_hugepage/enabled = madvise`), so a [`Pages`] of a huge page or more is allocated
//! on a huge-page boundary, advised, and only then written. Below that size it is an ordinary
//! allocation; where the advice is refused — huge pages off, another operating system — the
//! table sits on small pages and reads the same.

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};

/// A transparent huge page: 2 MiB on x86-64, and on aarch64 over 4 KiB base pages.
pub(crate) const HUGE: usize = 1 << 21;

/// A type of which the all-zero bit pattern is a value.
///
/// # Safety
///
/// Implement only for types every bit of which may be zero: integers and arrays of them.
pub(crate) unsafe trait Zeroed: Copy {}

unsafe impl Zeroed for u8 {}
unsafe impl Zeroed for u32 {}
unsafe impl Zeroed for u64 {}

/// A fixed-length table of `T`, on huge pages when it is long enough and the kernel offers them.
pub(crate) struct Pages<T> {
    ptr: NonNull<T>,
    len: usize,
}

impl<T: Zeroed> Pages<T> {
    /// `len` zeros.
    #[cfg_attr(not(feature = "mph"), allow(dead_code))]
    pub(crate) fn zeroed(len: usize) -> Self {
        let table = Self::uninit(len);
        // SAFETY: `ptr` is `len` writable `T`s of the table's own allocation, and zero is a `T`.
        unsafe { ptr::write_bytes(table.ptr.as_ptr(), 0, len) };
        table
    }

    /// A copy of `values`.
    pub(crate) fn from_slice(values: &[T]) -> Self {
        let table = Self::uninit(values.len());
        // SAFETY: `ptr` is `values.len()` writable `T`s of the table's own allocation, which
        // `values` cannot overlap.
        unsafe { ptr::copy_nonoverlapping(values.as_ptr(), table.ptr.as_ptr(), values.len()) };
        table
    }

    /// The allocation, advised but unwritten: the kernel sizes a page when it is first touched,
    /// so the advice has to come before the table's first write.
    fn uninit(len: usize) -> Self {
        let Some(layout) = Self::layout(len) else {
            return Self {
                ptr: NonNull::dangling(),
                len,
            };
        };
        // SAFETY: the layout is not zero-sized.
        let raw = unsafe { alloc(layout) };
        let Some(ptr) = NonNull::new(raw.cast::<T>()) else {
            handle_alloc_error(layout)
        };
        if layout.align() == HUGE {
            advise_huge(raw, layout.size());
        }
        Self { ptr, len }
    }
}

impl<T> Pages<T> {
    /// The layout of `len` values: on a huge-page boundary from a huge page up, and `None` when
    /// there is nothing to allocate.
    fn layout(len: usize) -> Option<Layout> {
        let layout = Layout::array::<T>(len).expect("a table's bytes fit an address");
        if layout.size() == 0 {
            None
        } else if layout.size() >= HUGE {
            Some(
                layout
                    .align_to(HUGE)
                    .expect("a huge page is a power of two"),
            )
        } else {
            Some(layout)
        }
    }
}

/// Asks the kernel for huge pages over `bytes` from `ptr`, a huge-page-aligned range of a live
/// allocation. Best effort: a kernel without them refuses, and the table reads the same.
#[cfg(all(target_os = "linux", not(miri)))]
fn advise_huge(ptr: *mut u8, bytes: usize) {
    use std::ffi::{c_int, c_void};
    unsafe extern "C" {
        fn madvise(addr: *mut c_void, length: usize, advice: c_int) -> c_int;
    }
    const MADV_HUGEPAGE: c_int = 14;
    // SAFETY: a range of this process's own mapping, and the advice writes no byte of it.
    let _ = unsafe { madvise(ptr.cast(), bytes, MADV_HUGEPAGE) };
}

#[cfg(not(all(target_os = "linux", not(miri))))]
fn advise_huge(_ptr: *mut u8, _bytes: usize) {}

impl<T> Drop for Pages<T> {
    fn drop(&mut self) {
        if let Some(layout) = Self::layout(self.len) {
            // SAFETY: allocated in `uninit` under this same layout, and freed here once.
            unsafe { dealloc(self.ptr.as_ptr().cast(), layout) };
        }
    }
}

impl<T> Deref for Pages<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: `len` initialised `T`s — written whole by `zeroed` or `from_slice`, or none —
        // that live as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl<T> DerefMut for Pages<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: as in `deref`, and `&mut self` is the one path to them.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

// SAFETY: the table owns its values outright, as a `Vec<T>` does.
unsafe impl<T: Send> Send for Pages<T> {}
// SAFETY: shared access is to `&[T]`.
unsafe impl<T: Sync> Sync for Pages<T> {}

impl<'a, T> IntoIterator for &'a Pages<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Self {
            ptr: NonNull::dangling(),
            len: 0,
        }
    }
}

impl<T: Zeroed> Clone for Pages<T> {
    fn clone(&self) -> Self {
        Self::from_slice(self)
    }
}

impl<T: PartialEq> PartialEq for Pages<T> {
    fn eq(&self, other: &Self) -> bool {
        self[..] == other[..]
    }
}

impl<T: Eq> Eq for Pages<T> {}

impl<T: fmt::Debug> fmt::Debug for Pages<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self[..], f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_holds_what_it_was_given_at_every_length_around_a_huge_page() {
        assert_eq!(Pages::<u64>::default().len(), 0);
        assert_eq!(&*Pages::<u8>::zeroed(0), &[]);
        for len in [
            1,
            7,
            64,
            HUGE / 8 - 1,
            HUGE / 8,
            HUGE / 8 + 1,
            2 * HUGE / 8 + 3,
        ] {
            let values: Vec<u64> = (0..len as u64).map(|j| j * 3 + 1).collect();
            let mut table = Pages::from_slice(&values);
            assert_eq!(&*table, &values[..]);
            assert_eq!(table.clone(), table);
            if len <= 64 {
                assert_eq!(format!("{table:?}"), format!("{values:?}"));
            }
            table[len - 1] = 0;
            assert_ne!(&*table, &values[..]);
            assert!(Pages::<u64>::zeroed(len).iter().all(|&v| v == 0));
        }
    }

    #[test]
    fn a_table_of_a_huge_page_or_more_starts_on_a_huge_page_boundary() {
        for len in [HUGE, HUGE + 1, 3 * HUGE + 12_345] {
            let table = Pages::<u8>::zeroed(len);
            assert_eq!((table.as_ptr() as usize % HUGE, table.len()), (0, len));
        }
        assert_eq!(Pages::<u64>::zeroed(HUGE / 8).as_ptr() as usize % HUGE, 0);
    }

    /// `AnonHugePages` of the mapping that holds a 4-huge-page table, from `/proc/self/smaps`.
    #[cfg(target_os = "linux")]
    fn huge_kb_of(table: &[u8]) -> Option<usize> {
        let start = table.as_ptr() as usize;
        let smaps = std::fs::read_to_string("/proc/self/smaps").ok()?;
        let mut inside = false;
        for line in smaps.lines() {
            let range = line.split(' ').next().and_then(|r| r.split_once('-'));
            if let Some((lo, hi)) = range {
                if let (Ok(lo), Ok(hi)) =
                    (usize::from_str_radix(lo, 16), usize::from_str_radix(hi, 16))
                {
                    inside = lo <= start && start < hi;
                    continue;
                }
            }
            if inside {
                if let Some(rest) = line.strip_prefix("AnonHugePages:") {
                    return rest.trim().trim_end_matches(" kB").parse().ok();
                }
            }
        }
        None
    }

    #[test]
    #[ignore = "needs a Linux kernel with transparent huge pages set to madvise or always"]
    #[cfg(target_os = "linux")]
    fn the_kernel_backs_a_large_table_with_huge_pages() {
        let table = Pages::<u8>::zeroed(4 * HUGE);
        let huge_kb = huge_kb_of(&table).expect("the table's mapping in smaps");
        assert!(huge_kb >= 3 * HUGE / 1024, "{huge_kb} kB of huge pages");
    }
}
