//! Uninitialised room at the end of a vector, for a decoder that writes every byte it keeps.
//!
//! Growing the vector with zeros first writes the answer twice, and for the byte or two a stair
//! usually decodes `Vec::resize` is a call into libc's `memset` that costs more than the bytes do:
//! it was a fifth of the whole packed decode on a million numbers, where a suffix is one digit.
//!
//! The decoders that fill it overshoot on purpose — a symbol is stored eight bytes wide and a
//! phrase thirty-two, whatever their length — so the room asked for is the widest reading of the
//! codes and the length committed is where the walk actually ended.

use std::mem::MaybeUninit;

/// Room for `n` bytes past `out`'s end, none of them written.
pub(crate) struct Room<'a> {
    slots: &'a mut [MaybeUninit<u8>],
}

impl<'a> Room<'a> {
    #[inline(always)]
    pub(crate) fn of(out: &'a mut Vec<u8>, n: usize) -> Self {
        out.reserve(n);
        Self {
            slots: &mut out.spare_capacity_mut()[..n],
        }
    }

    /// One byte at `at`.
    #[inline(always)]
    pub(crate) fn byte(&mut self, at: usize, b: u8) {
        self.slots[at].write(b);
    }

    /// The `n` slots from `at` on, for a decoder that fills a stretch of them in one loop.
    #[inline(always)]
    pub(crate) fn slots(&mut self, at: usize, n: usize) -> &mut [MaybeUninit<u8>] {
        &mut self.slots[at..at + n]
    }

    /// `src` at `at`. `N` is a constant, so this is one store of a width the compiler knows rather
    /// than the `memcpy` call a run-time length would take.
    #[inline(always)]
    pub(crate) fn put<const N: usize>(&mut self, at: usize, src: &[u8; N]) {
        let dst = &mut self.slots[at..at + N];
        // SAFETY: the slicing bounds the destination to `N` slots, `u8` has no drop to skip, and a
        // caller's array cannot overlap a vector's own spare capacity.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast::<u8>(), N) }
    }
}

/// Takes `n` bytes of the room last handed out of `out` into its length.
///
/// # Safety
///
/// The first `n` bytes of that room must have been written, and `n` must be at most the room asked
/// for.
#[inline(always)]
pub(crate) unsafe fn commit(out: &mut Vec<u8>, n: usize) {
    debug_assert!(out.len() + n <= out.capacity());
    // SAFETY: the caller wrote the first `n` bytes past the end, and `reserve` made them capacity.
    unsafe { out.set_len(out.len() + n) }
}
