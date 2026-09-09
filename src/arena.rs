//! Compact `slot → &str` storage: a contiguous data region plus an offset structure, so a key is a
//! slice of that region. Backs [`PerfectHashIndex`](crate::PerfectHashIndex), whose `id()` hot path
//! needs the stored key (zero-copy `&str`) to verify membership exactly. (`StringIndex` stores no
//! keys — it reconstructs them from the FST by a rank-walk — and `CompactHashIndex` stores only
//! fingerprints.) It views a [`SharedBytes`], so a memory-mapped load borrows it without copying.
//!
//! # Offset encoding
//!
//! Four encodings, chosen per arena at build time and named by the tag byte in the header. The two
//! flat ones are a table of `n + 1` absolute offsets. The two blocked ones cut the slots into
//! fixed-size runs and give each run a base plus a row of *cumulative* one- or two-byte offsets, so
//! a key is `data[base + off[k] .. base + off[k + 1]]` — two adjacent reads out of a header that is
//! one cache line wide.
//!
//! | tag | block header | slots/block | bytes/key |
//! |---|---|---|---|
//! | `0x11` | `[base u32][off u8 × 17]` | 16 | 1.31 |
//! | `0x12` | `[base u32][off u16 × 257]` | 256 | 2.02 |
//! | `0x31` | `[base u64][off u8 × 17]` | 16 | 1.56 |
//! | `0x32` | `[base u64][off u16 × 257]` | 256 | 2.04 |
//! | `4` | — | — | 4 |
//! | `8` | — | — | 8 |
//!
//! Bit `0x80` of the tag marks the same four layouts with one fingerprint byte per slot behind
//! the offsets — after a block's fencepost, or after a flat table — so `0x91` is
//! `[base u32][off u8 × 17][fp u8 × 16]`, 37 bytes a block. `PerfectHashIndex` asks for it when
//! built with fingerprints: a probe whose fingerprint does not match the slot's stops at the block
//! and never reads the key — the second cache miss of a lookup, which a workload of mostly-absent
//! keys otherwise pays for nothing. Behind the offsets rather than beside them so a member's
//! address path — the base and two offsets — is the plain layout's, byte for byte: interleaving
//! pushed the offset pair up to 36 bytes from the base instead of 20, one more line split in
//! four, and cost a member probe 6 ns against 3 for this layout.
//!
//! The build is optimistic: it lays out `0x11` and re-lays only when the lengths need more. A block
//! whose keys outgrow its offset width — sixteen of them past 255 bytes — no longer widens the
//! whole arena: bit `0x40` of the tag marks an arena with an *overflow table* behind its data, and
//! such a block keeps its place and stride, stores all ones in its first offset (zero in every
//! other block) with an index after it, and its real offsets sit in the table as base-wide words.
//! One 10 kB key among a million short ones costs 68 bytes and one more read on its own block;
//! before 1.2 it cost the arena 0.7 bytes per key, or 2.7 past 64 KiB. The layout is the cheapest
//! by the lengths once `0x11` does not fit as it is: 256-slot blocks when enough 16-key runs
//! overflow, the flat table only for a handful of keys none of which any block holds. Past 4 GiB
//! of data the bases widen to `u64` instead — bit `0x20` of the tag — and the offsets stay
//! blocked: 1.56 bytes per key where the flat `u64` table, the only encoding for that size before
//! 1.2, cost 8.
//!
//! This is the second narrowing of the same table, and both were worth what they cost. Before
//! 0.5.0 every offset was 8 bytes: on the 479 823-word dictionary that was 8.0 of
//! `PerfectHashIndex`'s 17.6 bytes per key, to address a 4.9 MB arena. Choosing 4 or 8 per arena
//! took the whole index to 13.62 bytes per key; the blocked layouts take it to **10.94**.
//!
//! Reading did not get slower for it. Alternated against the flat arena with
//! `CompactHashIndex::id_unchecked` — same key hash, no arena — as the control, `id` measured
//! **182 ns against 216** and `key` **38 against 52**, because 1.3 MB less index stays resident.
//! Only the batched `ids_of` had to be paid for, and only through the prefetch: see
//! [`prefetch_offsets`](StringArena::prefetch_offsets). Probes are shuffled in every measurement
//! here — a strided probe is learned by the L2 prefetcher and reverses layout rankings, which is
//! how the first version of `local/arenalayout` chose the wrong candidate.

use crate::IndexError;
use crate::blob::SharedBytes;

/// Byte width of a flat offset in an arena small enough for 32-bit offsets, and in one that is not.
/// Both double as their own tag byte, which is what they were before the blocked layouts existed.
const NARROW: usize = 4;
const WIDE: usize = 8;
/// Blocked layouts: 16 slots with one-byte cumulative offsets, 256 with two-byte ones.
const BLOCK_U8: u8 = 0x11;
const BLOCK_U16: u8 = 0x12;
const B_U8: usize = 16;
const B_U16: usize = 256;
/// Set on a blocked tag whose bases are `u64`: `0x31` is `0x11` past 4 GiB of data. Never on a
/// flat tag — the flat `u64` table is its own encoding, `8`.
const WIDE_BASE: u8 = 0x20;
/// The data length past which a `u32` base can no longer reach every block.
const NARROW_LIMIT: usize = u32::MAX as usize;
/// Set on a tag that stores a fingerprint byte per slot behind the offsets: `0x91` is the `0x11`
/// layout with fingerprints. The offsets keep their geometry; a block, or the flat table, grows
/// by one byte per slot at its end.
const FP: u8 = 0x80;
/// Set on a blocked tag whose arena ends in an overflow table — `[entries][len u64]` behind the
/// data, one entry of `slots + 1` base-wide cumulative offsets per block whose keys did not fit
/// the block's offset width. Such a block keeps its place and stride; its first offset, zero in
/// every other block, is all ones, and the entry's index follows it, base-wide. Never on a flat
/// tag.
const OVERFLOW: u8 = 0x40;
/// `[overflow_len: u64]`, the last bytes of an arena whose tag carries [`OVERFLOW`].
const TRAILER: usize = 8;
/// `[n_off: u64][tag: u8]`.
const HEADER: usize = 9;

/// A contiguous arena of UTF-8 strings addressable by index, viewing a shared byte source.
#[derive(Clone, Debug)]
pub(crate) struct StringArena {
    blob: SharedBytes, // [n_off: u64][tag: u8][offset structure][data][overflow table][trailer]
    n: usize,          // number of strings == n_off - 1
    data_start: usize, // HEADER + the offset structure's length
    data_end: usize,   // where the data stops: the overflow table's start, or the blob's end
    overflow_end: usize, // where the overflow table stops: before its trailer, or the blob's end
    tag: u8, // NARROW, WIDE, BLOCK_U8 or BLOCK_U16 (those two also `| WIDE_BASE`, `| OVERFLOW`), any `| FP`
}

impl StringArena {
    /// Build from strings in index order (`items[i]` becomes key `i`), deriving the count and the
    /// total byte length from a first pass over the iterator — hence the `Clone` bound.
    ///
    /// Knowing both totals up front is what lets the arena be assembled in one exactly-sized buffer
    /// with the data written straight into it. Collecting the data and the offsets separately and
    /// concatenating them afterwards, as this did before 0.10, held two copies of the whole corpus
    /// alive across the concatenation — on a 2 M-key
    /// [`PerfectHashIndex`](crate::PerfectHashIndex) build that was 36 MB of the peak.
    ///
    /// A blocked layout cannot write its header as the data lands — a block's offsets are only
    /// final once the block is — so the lengths are held in a transient `Vec<u32>` and the header
    /// is filled from it at the end. That is 4 bytes per key alive during the build, and the
    /// build's peak did not rise with it: 55.1 MB against 56.3 for the flat arena on the
    /// dictionary, three runs each.
    ///
    /// A caller that already knows the totals should use [`build_exact`](Self::build_exact): the
    /// first pass looks cheap (it reads string *lengths*, never their bytes) but it follows the
    /// caller's permutation, so it is one cache miss per key.
    #[cfg(test)]
    pub(crate) fn build<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        I::IntoIter: Clone,
        S: AsRef<str>,
    {
        let items = items.into_iter();
        let (n, data_len) = Self::totals(items.clone());
        Self::assemble(items, n, data_len, None, NARROW_LIMIT)
            .expect("totals came from the iterator itself")
    }

    fn totals<I, S>(items: I) -> (usize, usize)
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        items.fold((0usize, 0usize), |(n, len), s| {
            (n + 1, len + s.as_ref().len())
        })
    }

    /// [`build`](Self::build) for a caller that already knows how many strings there are and how
    /// many bytes they hold. `PerfectHashIndex` does: both fall out of the hashing pass it runs
    /// anyway, so handing them over removes a whole permuted walk over the keys.
    ///
    /// `fps` are the fingerprints to store, one per string in the same order, or `None` for a
    /// layout without them — see [`FP`].
    ///
    /// The totals are a shortcut, not a promise the arena relies on. If they turn out not to match
    /// what the iterator yields, the assembly is discarded and redone from totals the iterator
    /// itself supplies — a wrong hint costs time, never a malformed arena.
    pub(crate) fn build_exact<I, S>(items: I, n: usize, data_len: usize, fps: Option<&[u8]>) -> Self
    where
        I: IntoIterator<Item = S>,
        I::IntoIter: Clone,
        S: AsRef<str>,
    {
        Self::build_exact_limited(items, n, data_len, fps, NARROW_LIMIT)
    }

    /// [`build_exact`](Self::build_exact) with the data length past which bases are `u64`
    /// injected, so a test reaches the layouts a 4 GiB arena takes with a 40-byte one.
    pub(crate) fn build_exact_limited<I, S>(
        items: I,
        n: usize,
        data_len: usize,
        fps: Option<&[u8]>,
        limit: usize,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        I::IntoIter: Clone,
        S: AsRef<str>,
    {
        let items = items.into_iter();
        match Self::assemble(items.clone(), n, data_len, fps, limit) {
            Some(arena) => arena,
            None => {
                let (n, data_len) = Self::totals(items.clone());
                Self::assemble(items, n, data_len, fps, limit).expect(
                    "totals came from the iterator itself, one fingerprint per string, no key \
                     past u32::MAX bytes",
                )
            }
        }
    }

    /// Write the whole arena into one buffer sized for `n` strings holding `data_len` bytes.
    /// `None` if the iterator disagrees with either total, `fps` is not one per string, or a key
    /// is longer than `u32::MAX` bytes — the caller decides what to do about it.
    fn assemble<I, S>(
        items: I,
        n: usize,
        data_len: usize,
        fps: Option<&[u8]>,
        limit: usize,
    ) -> Option<Self>
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        if fps.is_some_and(|f| f.len() != n) {
            return None;
        }
        let blob = Self::lay_out_blocked(items, n, data_len, fps, limit)?;
        Some(
            Self::from_shared(SharedBytes::from_owned(blob)).expect("freshly built arena is valid"),
        )
    }

    /// Stream the data behind a header sized for `0x11` — or for `0x31` when the caller's total
    /// already says the data passes `limit`, so the common wide case moves nothing — then choose
    /// the encoding the lengths actually need and fill the header in place. An overflow table, if
    /// the lengths call for one, goes behind the data.
    ///
    /// A widening moves the data once, by one `copy_within`, and never re-walks the iterator: the
    /// source may be a permutation the caller cannot cheaply replay, and for `PerfectHashIndex` it
    /// is exactly that.
    fn lay_out_blocked<I, S>(
        items: I,
        n: usize,
        data_len: usize,
        fps: Option<&[u8]>,
        limit: usize,
    ) -> Option<Vec<u8>>
    where
        I: Iterator<Item = S>,
        S: AsRef<str>,
    {
        let wide = if data_len > limit { WIDE_BASE } else { 0 };
        let optimistic_tag = tag_with(BLOCK_U8 | wide, fps.is_some());
        let optimistic = prefix_len(optimistic_tag, n)?;
        let mut blob = Vec::with_capacity(optimistic + data_len);
        blob.resize(optimistic, 0);
        let mut lens: Vec<u32> = Vec::with_capacity(n);
        for s in items {
            if lens.len() == n {
                return None; // more strings than the header was sized for
            }
            let bytes = s.as_ref().as_bytes();
            blob.extend_from_slice(bytes);
            lens.push(u32::try_from(bytes.len()).ok()?); // no encoding holds a longer key
        }
        let written = blob.len() - optimistic;
        if lens.len() != n {
            return None;
        }
        let tag = tag_for(&lens, written, fps.is_some(), limit);
        let needed = prefix_len(tag, n)?;
        if needed != optimistic {
            move_data(&mut blob, optimistic, written, needed);
        }
        write_head(&mut blob, n, tag);
        let tail = write_offsets(&mut blob, tag, &lens, fps);
        blob.extend_from_slice(&tail);
        Some(blob)
    }

    /// The arena's fixed prefix — header plus offset structure — for strings whose lengths are
    /// known in index order but whose bytes are not yet available. Returns the prefix, the data
    /// length the offsets describe, the tag that encodes them, and the tail to write behind the
    /// data: the overflow table, empty unless the tag carries [`OVERFLOW`].
    ///
    /// This is what lets a builder place the keys without ever holding them: the offsets are
    /// derivable from the lengths alone, so the bytes can be written afterwards, out of order,
    /// straight into a mapped file. Read one back with [`span_at`](Self::span_at).
    ///
    /// Only `build_to_file` writes an arena this way, so this and its reader are behind `mmap` with
    /// it — otherwise an `mph`-without-`mmap` build carries two functions nothing can call.
    #[cfg(feature = "mmap")]
    pub(crate) fn prefix_for_lengths(
        lens: &[u32],
        fps: Option<&[u8]>,
    ) -> (Vec<u8>, usize, u8, Vec<u8>) {
        Self::prefix_for_lengths_limited(lens, fps, NARROW_LIMIT)
    }

    /// [`prefix_for_lengths`](Self::prefix_for_lengths) with the `u64`-base limit injected, as
    /// [`build_exact_limited`](Self::build_exact_limited).
    #[cfg(feature = "mmap")]
    pub(crate) fn prefix_for_lengths_limited(
        lens: &[u32],
        fps: Option<&[u8]>,
        limit: usize,
    ) -> (Vec<u8>, usize, u8, Vec<u8>) {
        let data_len: usize = lens.iter().map(|&l| l as usize).sum();
        let tag = tag_for(lens, data_len, fps.is_some(), limit);
        let mut blob =
            vec![0u8; prefix_len(tag, lens.len()).expect("lengths came from a real corpus")];
        write_head(&mut blob, lens.len(), tag);
        let tail = write_offsets(&mut blob, tag, lens, fps);
        (blob, data_len, tag, tail)
    }

    /// The `(start, end)` of slot `i` within the data region of a prefix built by
    /// [`prefix_for_lengths`](Self::prefix_for_lengths), `tail` being what the same call returned
    /// beside it.
    #[cfg(feature = "mmap")]
    pub(crate) fn span_at(prefix: &[u8], tail: &[u8], tag: u8, i: usize) -> (u64, u64) {
        let n = read_u64(prefix, 0).map_or(0, |n_off| n_off.saturating_sub(1) as usize);
        let overflow = &tail[..tail.len().saturating_sub(TRAILER)];
        let (lo, hi) = entry_of(prefix, || overflow, tag, n, i, None).map_or((0, 0), |e| e.0);
        (lo as u64, hi as u64)
    }

    /// Which of the four encodings this arena uses — for tests that must show they built the
    /// layout they say they are testing.
    #[cfg(test)]
    pub(crate) fn tag(&self) -> u8 {
        self.tag
    }

    /// Whether every entry carries a fingerprint — see [`FP`].
    #[inline(always)]
    pub(crate) fn has_fingerprints(&self) -> bool {
        self.tag & FP != 0
    }

    /// Number of stored strings.
    pub(crate) fn len(&self) -> usize {
        self.n
    }

    /// The string at index `i`, or `None` if out of range. Borrows the shared source — zero-copy.
    pub(crate) fn get(&self, i: usize) -> Option<&str> {
        self.str_at(self.span(i)?)
    }

    /// [`get`](Self::get) for a caller that knows the key's fingerprint: under a layout that
    /// stores them, a slot whose fingerprint is not `fp` answers `None` from the offset line
    /// alone, without the read of the key that is a lookup's second cache miss. Under any other
    /// layout `fp` is ignored.
    pub(crate) fn get_matching(&self, i: usize, fp: u8) -> Option<&str> {
        self.str_at(self.span_matching(i, fp)?)
    }

    /// Prefetch the lines slot `i`'s offsets are read from (pipelined batch lookups).
    ///
    /// Two of them under a blocked layout: a block header is 21 or 518 bytes, so its base and the
    /// offset pair 4 + k bytes in are regularly on different cache lines, and pulling in only the
    /// first leaves `ids_of` stalling on the second. Measured on the dictionary, prefetching the
    /// base alone cost `ids_of` 9 % against the flat table it replaced; with both it is level. A
    /// third names the fingerprint byte, which follows the offsets.
    #[inline(always)]
    pub(crate) fn prefetch_offsets(&self, i: usize) {
        if i >= self.n {
            return;
        }
        let bytes = self.blob.as_ref();
        let at = offsets_start(self.tag, i);
        crate::blob::prefetch_byte(bytes, at);
        let g = split(self.tag);
        let (slots, width) = match g.layout {
            BLOCK_U8 => (B_U8, 1),
            BLOCK_U16 => (B_U16, 2),
            w => {
                if g.fp == 1 {
                    crate::blob::prefetch_byte(bytes, HEADER + (self.n + 1) * w as usize + i);
                }
                return;
            }
        };
        crate::blob::prefetch_byte(bytes, at + g.base + (i % slots) * width);
        if g.fp == 1 {
            crate::blob::prefetch_byte(bytes, at + g.base + (slots + 1) * width + i % slots);
        }
    }

    /// Absolute `(start, end)` byte span of slot `i` in the blob, or `None` if out of range /
    /// corrupt. Splitting `get` into span + [`str_at`](Self::str_at) lets a batch caller prefetch
    /// the data bytes between the two.
    #[inline(always)]
    pub(crate) fn span(&self, i: usize) -> Option<(usize, usize)> {
        Some(self.entry(i, None)?.0)
    }

    /// [`span`](Self::span) with the fingerprint filter of [`get_matching`](Self::get_matching).
    #[inline(always)]
    pub(crate) fn span_matching(&self, i: usize, fp: u8) -> Option<(usize, usize)> {
        Some(self.entry(i, Some(fp))?.0)
    }

    /// Slot `i`'s span and fingerprint; `None` for a slot whose stored fingerprint is not `want`,
    /// decided before its offsets are read — see [`entry_of`].
    #[inline(always)]
    fn entry(&self, i: usize, want: Option<u8>) -> Option<((usize, usize), u8)> {
        if i >= self.n {
            return None;
        }
        let bytes = self.blob.as_ref();
        let overflow = || &bytes[self.data_end..self.overflow_end];
        let ((lo, hi), fp) = entry_of(bytes, overflow, self.tag, self.n, i, want)?;
        Some((
            (
                self.data_start.checked_add(lo)?,
                self.data_start.checked_add(hi)?,
            ),
            fp,
        ))
    }

    #[inline(always)]
    pub(crate) fn prefetch_span(&self, span: (usize, usize)) {
        crate::blob::prefetch_byte(self.blob.as_ref(), span.0);
    }

    #[inline(always)]
    pub(crate) fn str_at(&self, span: (usize, usize)) -> Option<&str> {
        std::str::from_utf8(self.blob.as_ref().get(span.0..span.1)?).ok()
    }

    /// The serialised layout, borrowed: the arena already *is* its own blob, so writing it out
    /// never needs a copy.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.blob.as_ref()
    }

    /// Serialise to the layout it already holds (a test convenience; production paths borrow
    /// [`as_bytes`](Self::as_bytes)).
    #[cfg(test)]
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        self.blob.as_ref().to_vec()
    }

    /// Parse an owned blob (copies once) — a test convenience; production loads go through
    /// [`StringArena::from_shared`] (owned or memory-mapped).
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()))
    }

    /// View a shared blob without copying, validating the header (untrusted input): the tag must be
    /// one this format defines, the offset structure must fit, an overflow table must fit behind
    /// the data, and the offsets' ends must span the data.
    /// Individual offsets are bounds-checked lazily in [`get`](StringArena::get), so a
    /// memory-mapped load stays instant (no `O(n)` scan).
    pub(crate) fn from_shared(blob: SharedBytes) -> Result<Self, IndexError> {
        let bytes = blob.as_ref();
        // Header-supplied, so converted checked: a count that does not fit this platform's `usize`
        // must fail here rather than truncate on a 32-bit target.
        let n_off = usize::try_from(read_u64(bytes, 0)?)
            .map_err(|_| IndexError::Format("arena: offset count out of range"))?;
        if n_off == 0 {
            return Err(IndexError::Format("arena: zero offsets (need at least 1)"));
        }
        let n = n_off - 1;
        // `4 | 8`: `NARROW` and `WIDE` as tag bytes; a flat table never carries `WIDE_BASE`.
        let tag = match bytes.get(8) {
            Some(&t) if matches!(split(t).layout, BLOCK_U8 | BLOCK_U16) => t,
            Some(&t) if matches!(t & !FP, 4 | 8) => t,
            _ => return Err(IndexError::Format("arena: unknown offset encoding")),
        };
        let data_start =
            prefix_len(tag, n).ok_or(IndexError::Format("arena: offset table too large"))?;
        if bytes.len() < data_start {
            return Err(IndexError::Format("arena: truncated offset table"));
        }
        let (data_end, overflow_end) = if tag & OVERFLOW != 0 {
            let trailer = bytes
                .len()
                .checked_sub(TRAILER)
                .filter(|&t| t >= data_start)
                .ok_or(IndexError::Format("arena: truncated overflow table"))?;
            let len = usize::try_from(read_u64(bytes, trailer)?)
                .ok()
                .filter(|&len| len <= trailer - data_start && len % overflow_entry_bytes(tag) == 0)
                .ok_or(IndexError::Format("arena: overflow table does not fit"))?;
            (trailer - len, trailer)
        } else {
            (bytes.len(), bytes.len())
        };
        let data_len = data_end - data_start;
        let overflow = || &bytes[data_end..overflow_end];
        // The two ends: the first key starts the data (so a blocked layout's first base is 0) and
        // the last one finishes it. Everything between is checked at the access that reads it.
        let ends = match n {
            0 => (0, 0),
            _ => {
                let first = entry_of(bytes, overflow, tag, n, 0, None)
                    .ok_or(IndexError::Format("arena: unreadable first offset"))?;
                let last = entry_of(bytes, overflow, tag, n, n - 1, None)
                    .ok_or(IndexError::Format("arena: unreadable last offset"))?;
                (first.0.0, last.0.1)
            }
        };
        if ends != (0, data_len) {
            return Err(IndexError::Format("arena: offsets do not span the data"));
        }
        Ok(Self {
            blob,
            n,
            data_start,
            data_end,
            overflow_end,
            tag,
        })
    }
}

/// What a tag says, split into the three things the arithmetic needs: which of the four encodings,
/// how wide a block's base is, and whether a fingerprint byte follows each slot — the last as `0`
/// or `1`, so the stride arithmetic can add it.
#[derive(Clone, Copy)]
struct Geometry {
    layout: u8,
    base: usize,
    fp: usize,
}

#[inline(always)]
fn split(tag: u8) -> Geometry {
    Geometry {
        layout: tag & !(FP | WIDE_BASE | OVERFLOW),
        base: if tag & WIDE_BASE != 0 { WIDE } else { NARROW },
        fp: usize::from(tag & FP != 0),
    }
}

fn tag_with(kind: u8, fingerprints: bool) -> u8 {
    if fingerprints { kind | FP } else { kind }
}

/// One block header: a base of `base` bytes, `slots + 1` offsets of `width` bytes and, with
/// fingerprints, one byte more per slot — behind the offsets, so the fencepost offset stays where
/// it is.
#[inline(always)]
const fn block_bytes(slots: usize, width: usize, base: usize, fp: usize) -> usize {
    base + (slots + 1) * width + slots * fp
}

/// Bytes from the start of the arena to the start of its data, or `None` if a header-supplied `n`
/// makes that overflow.
fn prefix_len(tag: u8, n: usize) -> Option<usize> {
    let g = split(tag);
    let table = match g.layout {
        BLOCK_U8 => n
            .div_ceil(B_U8)
            .checked_mul(block_bytes(B_U8, 1, g.base, g.fp))?,
        BLOCK_U16 => n
            .div_ceil(B_U16)
            .checked_mul(block_bytes(B_U16, 2, g.base, g.fp))?,
        w => n.checked_mul(w as usize + g.fp)?.checked_add(w as usize)?,
    };
    table.checked_add(HEADER)
}

/// One overflow entry: `slots + 1` cumulative offsets, each as wide as the block's base.
fn overflow_entry_bytes(tag: u8) -> usize {
    let g = split(tag);
    let slots = if g.layout == BLOCK_U16 { B_U16 } else { B_U8 };
    (slots + 1) * g.base
}

/// Where slot `i`'s offsets live: its block header, or its pair of flat entries.
#[inline(always)]
fn offsets_start(tag: u8, i: usize) -> usize {
    let g = split(tag);
    offsets_start_in(g.layout, g.base, g.fp, i)
}

#[inline(always)]
fn offsets_start_in(layout: u8, base: usize, fp: usize, i: usize) -> usize {
    match layout {
        BLOCK_U8 => HEADER + (i / B_U8) * block_bytes(B_U8, 1, base, fp),
        BLOCK_U16 => HEADER + (i / B_U16) * block_bytes(B_U16, 2, base, fp),
        w => HEADER + i * w as usize,
    }
}

/// Slot `i`'s `(start, end)` relative to the data region and, under a tag with [`FP`], its
/// fingerprint (`0` otherwise). `None` if the structure is too short for it or the pair runs
/// backwards — a corrupt blob answers "no such key", never a wild slice. `n` locates a flat
/// table's fingerprint row, which follows its `n + 1` offsets; `overflow` yields the arena's
/// overflow table, empty when its tag has none, for a block that marks its offsets as living
/// there — a closure, so the slice is made only on the path that reads it and the block the
/// other 999 999 keys take carries nothing for it. `want`, under a tag with [`FP`], is a
/// fingerprint the slot's must equal, checked before its offsets are read: a probe it rules out
/// costs the fingerprint byte alone. That is not only the work saved — an absent probe runs
/// about 255 instructions, the width of the reorder buffer, and it is only that short because
/// its one cache miss overlaps the next probe's; when a marker check and a longer function
/// pushed the path past that, absent probes under fingerprints slowed from 89 to 99 ns, and
/// deciding on the fingerprint first took them to 75.
///
/// The fingerprint and base-width bits are decided once, into a copy specialised on them, so the
/// layout without either keeps the constant strides it had before the bits existed.
#[inline(always)]
fn entry_of<'a>(
    bytes: &'a [u8],
    overflow: impl FnOnce() -> &'a [u8],
    tag: u8,
    n: usize,
    i: usize,
    want: Option<u8>,
) -> Option<((usize, usize), u8)> {
    let g = split(tag);
    match (g.fp, g.base) {
        (0, NARROW) => entry_in::<0, NARROW>(bytes, overflow, g.layout, n, i, want),
        (0, _) => entry_in::<0, WIDE>(bytes, overflow, g.layout, n, i, want),
        (_, NARROW) => entry_in::<1, NARROW>(bytes, overflow, g.layout, n, i, want),
        _ => entry_in::<1, WIDE>(bytes, overflow, g.layout, n, i, want),
    }
}

#[inline(always)]
fn entry_in<'a, const F: usize, const B: usize>(
    bytes: &'a [u8],
    overflow: impl FnOnce() -> &'a [u8],
    layout: u8,
    n: usize,
    i: usize,
    want: Option<u8>,
) -> Option<((usize, usize), u8)> {
    let at = offsets_start_in(layout, B, F, i);
    match layout {
        BLOCK_U8 => {
            let block = bytes.get(at..at.checked_add(block_bytes(B_U8, 1, B, F))?)?;
            let k = i % B_U8;
            let fp = if F == 1 { block[B + B_U8 + 1 + k] } else { 0 };
            if F == 1 && want.is_some_and(|w| w != fp) {
                return None;
            }
            let base = read_word::<B>(block)?;
            let span = if block[B] == 0 {
                span_from(base, block[B + k] as usize, block[B + k + 1] as usize)?
            } else {
                overflow_span::<B>(overflow(), &block[B + 1..], B_U8, k, base)?
            };
            Some((span, fp))
        }
        BLOCK_U16 => {
            let block = bytes.get(at..at.checked_add(block_bytes(B_U16, 2, B, F))?)?;
            let k = i % B_U16;
            let fp = if F == 1 {
                block[B + (B_U16 + 1) * 2 + k]
            } else {
                0
            };
            if F == 1 && want.is_some_and(|w| w != fp) {
                return None;
            }
            let base = read_word::<B>(block)?;
            let span = if (block[B] | block[B + 1]) == 0 {
                let p = B + k * 2;
                let lo = u16::from_le_bytes(block[p..p + 2].try_into().unwrap()) as usize;
                let hi = u16::from_le_bytes(block[p + 2..p + 4].try_into().unwrap()) as usize;
                span_from(base, lo, hi)?
            } else {
                overflow_span::<B>(overflow(), &block[B + 2..], B_U16, k, base)?
            };
            Some((span, fp))
        }
        w => {
            let width = w as usize;
            let fp = if F == 1 {
                *bytes.get(HEADER + (n + 1) * width + i)?
            } else {
                0
            };
            if F == 1 && want.is_some_and(|w| w != fp) {
                return None;
            }
            let lo = usize::try_from(read_offset(bytes, at, width).ok()?).ok()?;
            let hi = usize::try_from(read_offset(bytes, at + width, width).ok()?).ok()?;
            Some((span_from(0, lo, hi)?, fp))
        }
    }
}

/// The span of slot `k` in a block whose offsets live in the overflow table: `row` is the block's
/// offset row past its all-ones first offset, so the entry's index is its first base-wide word.
/// Out of line and cold: this is the one block in a million, so the hot path keeps its size and
/// its fall-through — without the attribute the compiler laid the call out as the straight path
/// and made every other block jump around it, which cost an absent probe under fingerprints
/// 12 ns of its 90.
#[cold]
#[inline(never)]
fn overflow_span<const B: usize>(
    overflow: &[u8],
    row: &[u8],
    slots: usize,
    k: usize,
    base: usize,
) -> Option<(usize, usize)> {
    let entry_bytes = (slots + 1) * B;
    let start = read_word::<B>(row)?.checked_mul(entry_bytes)?;
    let entry = overflow.get(start..start.checked_add(entry_bytes)?)?;
    let lo = read_word::<B>(&entry[k * B..])?;
    let hi = read_word::<B>(&entry[(k + 1) * B..])?;
    span_from(base, lo, hi)
}

/// A base-wide word — a block's base, an overflow entry's index or offset — `u32` or `u64` by `B`;
/// `None` where a `u64` one does not fit this platform's `usize`: a blob past 4 GiB answers "no
/// such key" on a 32-bit target, never a truncated address.
#[inline(always)]
fn read_word<const B: usize>(bytes: &[u8]) -> Option<usize> {
    if B == WIDE {
        usize::try_from(u64::from_le_bytes(bytes[..8].try_into().unwrap())).ok()
    } else {
        Some(u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize)
    }
}

#[inline(always)]
fn span_from(base: usize, lo: usize, hi: usize) -> Option<(usize, usize)> {
    if hi < lo {
        return None;
    }
    Some((base.checked_add(lo)?, base.checked_add(hi)?))
}

/// The encoding for these lengths. Blocks of 16 with one-byte offsets whenever no run of 16 keys
/// spans more than 255 bytes, as always. Otherwise the cheapest by bytes of: those blocks with an
/// overflow entry for every run that does not fit, blocks of 256 with two-byte offsets (and entries
/// of their own for a run past 64 KiB), or the flat table — ties to the narrower. So one long key
/// among short ones costs an entry, while a corpus of long keys takes the wider blocks it always
/// took. Bases, and with them entries and indices, are `u64` once the data passes `limit` (4 GiB
/// outside the tests).
fn tag_for(lens: &[u32], data_len: usize, fingerprints: bool, limit: usize) -> u8 {
    let wide = if data_len > limit { WIDE_BASE } else { 0 };
    let base = if wide != 0 { WIDE } else { NARROW };
    let fp = usize::from(fingerprints);
    let n = lens.len() as u64;
    let blocked = |slots: usize, width: usize, most: u64| {
        let overflowing = lens
            .chunks(slots)
            .filter(|c| c.iter().map(|&l| u64::from(l)).sum::<u64>() > most)
            .count() as u64;
        let bytes = n.div_ceil(slots as u64) * block_bytes(slots, width, base, fp) as u64
            + overflowing * ((slots + 1) * base) as u64
            + if overflowing > 0 { TRAILER as u64 } else { 0 };
        (bytes, if overflowing > 0 { OVERFLOW } else { 0 })
    };
    let (u8_bytes, u8_overflow) = blocked(B_U8, 1, u64::from(u8::MAX));
    let kind = if u8_overflow == 0 {
        BLOCK_U8 | wide
    } else {
        let (u16_bytes, u16_overflow) = blocked(B_U16, 2, u64::from(u16::MAX));
        let flat_bytes = (n + 1) * base as u64 + n * fp as u64;
        let mut best = (u8_bytes, BLOCK_U8 | wide | OVERFLOW);
        for candidate in [
            (u16_bytes, BLOCK_U16 | wide | u16_overflow),
            (flat_bytes, base as u8),
        ] {
            if candidate.0 < best.0 {
                best = candidate;
            }
        }
        best.1
    };
    tag_with(kind, fingerprints)
}

fn write_head(blob: &mut [u8], n: usize, tag: u8) {
    blob[..8].copy_from_slice(&((n as u64) + 1).to_le_bytes());
    blob[8] = tag;
}

/// Move the data region from `from` to `to`, growing or shrinking the buffer to match. The bytes
/// vacated are the offset structure's, and every one of them is written before the arena is read.
fn move_data(blob: &mut Vec<u8>, from: usize, data_len: usize, to: usize) {
    if to > from {
        blob.resize(to + data_len, 0);
    }
    blob.copy_within(from..from + data_len, to);
    blob.truncate(to + data_len);
}

/// Fill an already-sized offset structure from the lengths it was sized for, and from the
/// fingerprints when the tag carries them; returns the tail to write behind the data — the
/// overflow table and its trailer under a tag with [`OVERFLOW`], nothing otherwise. Slots past
/// `n` in the last block repeat that block's total, so a read past the end sees an empty span
/// rather than an inverted one, and carry a zero fingerprint: every header byte is written,
/// because a widening leaves stale data bytes where the wider header now is.
fn write_offsets(blob: &mut [u8], tag: u8, lens: &[u32], fps: Option<&[u8]>) -> Vec<u8> {
    let g = split(tag);
    debug_assert_eq!(fps.is_some(), g.fp == 1);
    let (b, width, most) = match g.layout {
        BLOCK_U8 => (B_U8, 1usize, u64::from(u8::MAX)),
        BLOCK_U16 => (B_U16, 2usize, u64::from(u16::MAX)),
        w => {
            let width = w as usize;
            let mut at = 0u64;
            write_offset(blob, HEADER, width, 0);
            for (i, &l) in lens.iter().enumerate() {
                at += l as u64;
                write_offset(blob, HEADER + (i + 1) * width, width, at);
            }
            if let Some(fps) = fps {
                let row = HEADER + (lens.len() + 1) * width;
                blob[row..row + fps.len()].copy_from_slice(fps);
            }
            return Vec::new();
        }
    };
    let mut overflow = Vec::new();
    let mut base = 0u64;
    for (j, chunk) in lens.chunks(b).enumerate() {
        let at = HEADER + j * block_bytes(b, width, g.base, g.fp);
        blob[at..at + g.base].copy_from_slice(&base.to_le_bytes()[..g.base]);
        let row = at + g.base;
        let total: u64 = chunk.iter().map(|&l| u64::from(l)).sum();
        if total > most {
            debug_assert!(tag & OVERFLOW != 0);
            let index = (overflow.len() / ((b + 1) * g.base)) as u64;
            blob[row..row + width].fill(0xFF);
            blob[row + width..row + width + g.base].copy_from_slice(&index.to_le_bytes()[..g.base]);
            blob[row + width + g.base..row + (b + 1) * width].fill(0);
            let mut off = 0u64;
            for k in 0..=b {
                overflow.extend_from_slice(&off.to_le_bytes()[..g.base]);
                off += u64::from(chunk.get(k).copied().unwrap_or(0));
            }
        } else {
            let mut off = 0u32;
            for k in 0..=b {
                let p = row + k * width;
                match width {
                    1 => blob[p] = off as u8,
                    _ => blob[p..p + 2].copy_from_slice(&(off as u16).to_le_bytes()),
                }
                off += chunk.get(k).copied().unwrap_or(0);
            }
        }
        if let Some(fps) = fps {
            let row = row + (b + 1) * width;
            for k in 0..b {
                blob[row + k] = fps.get(j * b + k).copied().unwrap_or(0);
            }
        }
        base += total;
    }
    if tag & OVERFLOW != 0 {
        let len = overflow.len() as u64;
        overflow.extend_from_slice(&len.to_le_bytes());
    }
    overflow
}

/// Write one offset of a flat table's width, into the table the caller has already reserved, so an
/// out-of-range `at` is a bug in this file.
fn write_offset(bytes: &mut [u8], at: usize, width: usize, off: u64) {
    match width {
        NARROW => bytes[at..at + NARROW].copy_from_slice(&(off as u32).to_le_bytes()),
        _ => bytes[at..at + WIDE].copy_from_slice(&off.to_le_bytes()),
    }
}

fn read_offset(bytes: &[u8], at: usize, width: usize) -> Result<u64, IndexError> {
    let end = at
        .checked_add(width)
        .ok_or(IndexError::Format("arena: offset overflow"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(IndexError::Format("arena: unexpected end of buffer"))?;
    Ok(match width {
        NARROW => u32::from_le_bytes(slice.try_into().unwrap()) as u64,
        _ => u64::from_le_bytes(slice.try_into().unwrap()),
    })
}

fn read_u64(bytes: &[u8], at: usize) -> Result<u64, IndexError> {
    read_offset(bytes, at, WIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block header a corpus of two short keys produces, byte for byte. The serialised layout
    /// is a format, not an implementation detail: every blob any released version wrote is parsed
    /// by [`from_shared`](StringArena::from_shared), so a build that quietly changed a byte would
    /// break `load` on existing files.
    #[test]
    fn small_arenas_use_one_byte_cumulative_offsets() {
        let arena = StringArena::build(["apple", "banana"]);
        assert_eq!(arena.tag(), BLOCK_U8);
        let mut want = Vec::new();
        want.extend_from_slice(&3u64.to_le_bytes()); // n_off = 2 keys + 1
        want.push(BLOCK_U8);
        want.extend_from_slice(&0u32.to_le_bytes()); // the one block's base
        want.extend_from_slice(&[0, 5, 11]); // and its cumulative offsets,
        want.extend_from_slice(&[11; 14]); // padded past the end with the block total
        want.extend_from_slice(b"applebanana");
        assert_eq!(arena.to_bytes(), want);
        assert_eq!(want.len(), HEADER + block_bytes(B_U8, 1, NARROW, 0) + 11);
    }

    #[test]
    fn build_get_and_roundtrip() {
        let arena = StringArena::build(["apple", "banana", "", "cherry"]);
        assert_eq!(arena.len(), 4);
        assert_eq!(arena.get(0), Some("apple"));
        assert_eq!(arena.get(2), Some("")); // empty string is a valid entry
        assert_eq!(arena.get(3), Some("cherry"));
        assert_eq!(arena.get(4), None); // out of range
        let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
        assert_eq!(restored.len(), 4);
        assert_eq!(restored.get(1), Some("banana"));
    }

    #[test]
    fn empty_arena_roundtrips() {
        let arena = StringArena::build(Vec::<&str>::new());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.get(0), None);
        assert_eq!(arena.to_bytes().len(), HEADER); // no blocks at all
        assert_eq!(StringArena::from_bytes(&arena.to_bytes()).unwrap().len(), 0);
    }

    /// Every key in a blocked layout is addressed off its own block's base, so the arithmetic is
    /// only exercised by a corpus that crosses one. 16 keys fill a block exactly, 17 start a
    /// second, and the block-relative offset of key 16 is 0 while its absolute one is not.
    #[test]
    fn keys_are_read_across_block_boundaries() {
        for n in [15usize, 16, 17, 33] {
            let keys: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
            let arena = StringArena::build(&keys);
            assert_eq!(arena.tag(), BLOCK_U8, "n={n}");
            assert_eq!(
                arena.to_bytes().len(),
                HEADER
                    + n.div_ceil(B_U8) * block_bytes(B_U8, 1, NARROW, 0)
                    + keys.iter().map(String::len).sum::<usize>(),
                "n={n}"
            );
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(arena.get(i), Some(key.as_str()), "n={n}, key {i}");
            }
            assert_eq!(arena.get(n), None, "n={n}");
        }
    }

    /// An empty entry between two others, at each end, and on both sides of a block boundary: a
    /// zero-length key is the case where two consecutive offsets are equal, which is where an
    /// off-by-one in the fill loop shows up.
    #[test]
    fn empty_keys_keep_their_slots() {
        let arena = StringArena::build(["", "a", "", "bc", ""]);
        assert_eq!(arena.len(), 5);
        let got: Vec<Option<&str>> = (0..6).map(|i| arena.get(i)).collect();
        assert_eq!(
            got,
            [Some(""), Some("a"), Some(""), Some("bc"), Some(""), None]
        );

        let mut keys: Vec<String> = (0..20).map(|i| format!("k{i}")).collect();
        keys[15] = String::new(); // last slot of block 0
        keys[16] = String::new(); // first slot of block 1
        let arena = StringArena::build(&keys);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(arena.get(i), Some(key.as_str()), "key {i}");
        }
    }

    /// The totals a caller passes to `build_exact` are a shortcut, not a promise: whatever they
    /// say, the arena must come out exactly as `build` would have derived it. A count that is too
    /// small is caught mid-fill, one that is too large at the end, and a byte total that is merely
    /// wrong only mis-sizes the initial allocation. A hint above 4 GiB is *not* exercised here:
    /// the buffer is reserved from the hint, and a test that asks for 4 GiB of address space fails
    /// on the first machine without overcommit; the layouts it selects are reached by injecting
    /// the limit instead.
    #[test]
    fn a_wrong_hint_never_changes_the_arena() {
        let items = ["apple", "banana", "", "cherry"]; // 17 bytes over 4 strings
        let want = StringArena::build(items).to_bytes();
        for (n, data_len) in [(4, 17), (3, 17), (5, 17), (0, 0), (4, 0), (4, 999)] {
            let got = StringArena::build_exact(items, n, data_len, None);
            assert_eq!(got.to_bytes(), want, "hint n={n}, data_len={data_len}");
            assert_eq!(got.get(3), Some("cherry"));
        }
    }

    /// Two hundred 20-byte keys span 320 bytes per block of 16, so the one-byte offsets describe
    /// no block and the build re-lays to `0x12` — the case the optimistic first pass exists to
    /// make cheap, and the one that must not corrupt the data it has already written. Forty of
    /// them are cheaper as a flat table (164 bytes against a 518-byte block) and take it.
    #[test]
    fn a_corpus_of_long_keys_widens_to_two_byte_offsets() {
        let keys: Vec<String> = (0..200).map(|i| format!("{i:020}")).collect();
        let arena = StringArena::build(&keys);
        assert_eq!(arena.tag(), BLOCK_U16);
        assert_eq!(
            arena.to_bytes().len(),
            HEADER + block_bytes(B_U16, 2, NARROW, 0) + 4000 // one block of 256 slots, 200 used
        );
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(arena.get(i), Some(key.as_str()), "key {i}");
        }
        let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
        assert_eq!(restored.get(199).unwrap(), keys[199]);
        assert_eq!(restored.get(200), None);

        let few = StringArena::build(&keys[..40]);
        assert_eq!(few.tag(), NARROW as u8);
        assert_eq!(few.to_bytes().len(), HEADER + 41 * NARROW + 800);
        assert_eq!(few.get(39), Some(keys[39].as_str()));
    }

    /// One long key among short ones costs its block an overflow entry — 17 base-wide offsets
    /// behind the data, an all-ones first offset and the entry's index where the block's offsets
    /// were — not the whole arena a wider layout. Read back through the block, through
    /// `from_bytes`, with fingerprints, and through the prefix `build_to_file` lays out.
    #[test]
    fn one_long_key_costs_an_overflow_entry_not_a_layout() {
        let mut keys: Vec<String> = (0..1000).map(|i| format!("k{i}")).collect();
        keys[500] = "x".repeat(10_000);
        let n = keys.len();
        let data_len: usize = keys.iter().map(String::len).sum();
        let fps: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
        for fp in [None, Some(&fps[..])] {
            let arena = StringArena::build_exact(&keys, n, data_len, fp);
            let want_tag = tag_with(BLOCK_U8 | OVERFLOW, fp.is_some());
            assert_eq!(arena.tag(), want_tag);
            let blocks = n.div_ceil(B_U8);
            assert_eq!(
                arena.to_bytes().len(),
                HEADER
                    + blocks * block_bytes(B_U8, 1, NARROW, fp.map_or(0, |_| 1))
                    + data_len
                    + (B_U8 + 1) * NARROW
                    + TRAILER
            );
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(arena.get(i), Some(key.as_str()), "{want_tag:#x} key {i}");
                if let Some(fps) = fp {
                    assert_eq!(arena.get_matching(i, fps[i]), Some(key.as_str()));
                    assert_eq!(arena.get_matching(i, fps[i] ^ 1), None);
                }
            }
            assert_eq!(arena.get(n), None);
            let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
            assert_eq!(restored.tag(), want_tag);
            assert_eq!(restored.get(500), Some(keys[500].as_str()));
            assert_eq!(restored.get(511), Some(keys[511].as_str()));
            #[cfg(feature = "mmap")]
            {
                let lens: Vec<u32> = keys.iter().map(|k| k.len() as u32).collect();
                let (prefix, len, tag, tail) = StringArena::prefix_for_lengths(&lens, fp);
                assert_eq!((len, tag), (data_len, want_tag));
                let bytes = arena.to_bytes();
                assert_eq!(prefix[..], bytes[..prefix.len()]);
                assert_eq!(tail[..], bytes[bytes.len() - tail.len()..]);
                for (i, key) in keys.iter().enumerate() {
                    let (lo, hi) = StringArena::span_at(&prefix, &tail, tag, i);
                    assert_eq!((hi - lo) as usize, key.len(), "{want_tag:#x} key {i}");
                }
            }
        }
    }

    /// The overflow layout byte for byte, so the reader is pinned independently of the writer: the
    /// block keeps its base and its stride, its first offset is all ones and the entry's index
    /// follows; the entry behind the data holds the 17 cumulative offsets as `u32`s, padded with
    /// the block's total like any block; the trailer says how long the table is. Thirty-two keys,
    /// because with fewer the flat table is cheaper than a block and an entry.
    #[test]
    fn an_overflowing_block_is_marked_in_its_first_offset() {
        let long = "x".repeat(300);
        let mut keys = vec!["ab", long.as_str(), "c"];
        keys.extend(std::iter::repeat_n("d", 29));
        let arena = StringArena::build(&keys);
        assert_eq!(arena.tag(), BLOCK_U8 | OVERFLOW);
        let mut want = Vec::new();
        want.extend_from_slice(&33u64.to_le_bytes());
        want.push(BLOCK_U8 | OVERFLOW);
        want.extend_from_slice(&0u32.to_le_bytes()); // block 0: base
        want.push(0xFF); // the marker
        want.extend_from_slice(&0u32.to_le_bytes()); // the entry's index
        want.extend_from_slice(&[0; 12]); // the rest of the offset row
        want.extend_from_slice(&316u32.to_le_bytes()); // block 1: base
        want.extend(0..=16u8); // sixteen one-byte keys
        want.extend_from_slice(b"ab");
        want.extend_from_slice(long.as_bytes());
        want.extend_from_slice(b"c");
        want.extend_from_slice(&[b'd'; 29]);
        for off in [0u32, 2, 302].into_iter().chain(303..=316) {
            want.extend_from_slice(&off.to_le_bytes());
        }
        want.extend_from_slice(&68u64.to_le_bytes());
        assert_eq!(arena.to_bytes(), want);
        assert_eq!(arena.get(0), Some("ab"));
        assert_eq!(arena.get(1), Some(long.as_str()));
        assert_eq!(arena.get(2), Some("c"));
        assert_eq!(arena.get(15), Some("d"));
        assert_eq!(arena.get(16), Some("d"));
        assert_eq!(arena.get(31), Some("d"));
        assert_eq!(arena.get(32), None);
    }

    /// An overflow table is validated at load like the offset structure: the trailer must be
    /// there, the length it names must fit behind the data in whole entries, an index past the
    /// table reads as no key, a marker in an arena whose tag has no table is corrupt, and a flat
    /// table never has one.
    #[test]
    fn an_overflow_table_is_validated_like_the_offsets() {
        let long = "x".repeat(300);
        let mut keys = vec!["ab", long.as_str(), "c"];
        keys.extend(std::iter::repeat_n("d", 29));
        let good = StringArena::build(&keys).to_bytes();
        assert_eq!(good[8], BLOCK_U8 | OVERFLOW);
        let data_start = HEADER + 2 * block_bytes(B_U8, 1, NARROW, 0);
        let err = |blob: &[u8]| StringArena::from_bytes(blob).unwrap_err().to_string();
        assert!(err(&good[..data_start + 3]).contains("truncated overflow table"));
        let mut bad = good.clone();
        let trailer = bad.len() - TRAILER;
        bad[trailer..].copy_from_slice(&67u64.to_le_bytes());
        assert!(err(&bad).contains("overflow table does not fit"));
        bad[trailer..].copy_from_slice(&(good.len() as u64).to_le_bytes());
        assert!(err(&bad).contains("overflow table does not fit"));
        // An index past the only entry: the block's keys are unreadable, and the first key is
        // one of them.
        let mut bad = good.clone();
        bad[HEADER + NARROW + 1] = 1;
        assert!(err(&bad).contains("unreadable first offset"));
        // The marker without the table: no tag bit, so the table is empty and the block corrupt.
        let mut bad = good.clone();
        bad[8] = BLOCK_U8;
        assert!(StringArena::from_bytes(&bad).is_err());
        let mut flat = Vec::new();
        flat.extend_from_slice(&3u64.to_le_bytes());
        flat.push(NARROW as u8 | OVERFLOW);
        for o in [0u32, 5, 11] {
            flat.extend_from_slice(&o.to_le_bytes());
        }
        flat.extend_from_slice(b"applebanana");
        assert!(err(&flat).contains("unknown offset encoding"));
    }

    /// A key longer than 65 535 bytes overflows a block of 256 as well, and for two keys the flat
    /// table is the cheapest of what is left: 12 bytes against a block and its entry. The same
    /// corpus through `build_exact` with a true hint and a false one, because the widening happens
    /// after the hint has already sized the buffer.
    #[test]
    fn a_key_too_long_for_any_block_falls_back_to_the_flat_table() {
        let long = "x".repeat(70_000);
        let items = [long.as_str(), "tail"];
        let arena = StringArena::build(items);
        assert_eq!(arena.tag(), NARROW as u8);
        assert_eq!(arena.to_bytes().len(), HEADER + 3 * NARROW + 70_004);
        assert_eq!(arena.get(0), Some(long.as_str()));
        assert_eq!(arena.get(1), Some("tail"));
        for (n, data_len) in [(2, 70_004), (2, 0), (9, 70_004)] {
            assert_eq!(
                StringArena::build_exact(items, n, data_len, None).to_bytes(),
                arena.to_bytes(),
                "hint n={n}, data_len={data_len}"
            );
        }
    }

    /// The wide path cannot be reached by building a >4 GiB arena in a test, so drive it through
    /// the parser: a hand-written wide blob must load and read back identically. The narrow flat
    /// blob beside it is what every arena written before 1.1 looks like.
    #[test]
    fn flat_offsets_round_trip() {
        for width in [NARROW, WIDE] {
            let mut blob = Vec::new();
            blob.extend_from_slice(&3u64.to_le_bytes());
            blob.push(width as u8);
            for o in [0u64, 5, 11] {
                blob.extend_from_slice(&o.to_le_bytes()[..width]);
            }
            blob.extend_from_slice(b"applebanana");
            let arena = StringArena::from_bytes(&blob).unwrap();
            assert_eq!(arena.tag(), width as u8);
            assert_eq!(arena.get(0), Some("apple"));
            assert_eq!(arena.get(1), Some("banana"));
            assert_eq!(arena.get(2), None);
        }
    }

    /// A hand-written `0x12` blob, so the reader is pinned independently of the writer that
    /// normally produces one.
    #[test]
    fn two_byte_blocks_round_trip() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&3u64.to_le_bytes());
        blob.push(BLOCK_U16);
        blob.extend_from_slice(&0u32.to_le_bytes());
        for off in [0u16, 5, 11] {
            blob.extend_from_slice(&off.to_le_bytes());
        }
        blob.resize(HEADER + block_bytes(B_U16, 2, NARROW, 0), 11); // padded with the block total, as the writer does
        blob.extend_from_slice(b"applebanana");
        let arena = StringArena::from_bytes(&blob).unwrap();
        assert_eq!(arena.get(0), Some("apple"));
        assert_eq!(arena.get(1), Some("banana"));
        assert_eq!(arena.get(2), None);
    }

    #[test]
    fn rejects_corrupt_headers() {
        assert!(StringArena::from_bytes(b"short").is_err()); // < 8-byte header
        let mut good = StringArena::build(["a", "b"]).to_bytes();
        good[0] = 0xff; // absurd offset count → truncated block region
        assert!(StringArena::from_bytes(&good).is_err());

        let mut bad_tag = StringArena::build(["a", "b"]).to_bytes();
        bad_tag[8] = 3; // no such encoding
        assert!(StringArena::from_bytes(&bad_tag).is_err());

        let full = StringArena::build(["a", "b"]).to_bytes();
        assert!(StringArena::from_bytes(&full[..HEADER + 4]).is_err()); // block header cut short

        let mut short_last = full.clone();
        short_last[HEADER + 4 + 2] = 1; // last offset no longer reaches the end of the data
        assert!(StringArena::from_bytes(&short_last).is_err());

        let mut moved_base = full.clone();
        moved_base[HEADER] = 1; // the first key no longer starts the data
        assert!(StringArena::from_bytes(&moved_base).is_err());
    }

    /// An offset pair that runs backwards is in range and passes the two end checks, so it is
    /// caught where it is read: that slot has no key, and no slice is taken from it.
    #[test]
    fn a_backwards_offset_pair_yields_no_key() {
        let mut blob = StringArena::build(["apple", "banana", "cherry"]).to_bytes();
        blob[HEADER + 4 + 2] = 3; // off[2]: 11 → 3, below off[1] = 5
        let arena = StringArena::from_bytes(&blob).unwrap();
        assert_eq!(arena.get(0), Some("apple"));
        assert_eq!(arena.get(1), None);
        assert_eq!(arena.span(1), None);
    }

    /// The fingerprinted layout, byte for byte, beside
    /// [`small_arenas_use_one_byte_cumulative_offsets`]: the same block, then one fingerprint
    /// byte per slot, zero for the padding slots.
    #[test]
    fn fingerprints_follow_the_offsets() {
        let arena = StringArena::build_exact(["apple", "banana"], 2, 11, Some(&[0xA5, 0x5A][..]));
        assert_eq!(arena.tag(), BLOCK_U8 | FP);
        assert!(arena.has_fingerprints());
        let mut want = Vec::new();
        want.extend_from_slice(&3u64.to_le_bytes()); // n_off = 2 keys + 1
        want.push(BLOCK_U8 | FP);
        want.extend_from_slice(&0u32.to_le_bytes()); // the block's base
        want.extend_from_slice(&[0, 5, 11]); // its cumulative offsets, padded as without
        want.extend_from_slice(&[11; 14]); // fingerprints ...
        want.extend_from_slice(&[0xA5, 0x5A]); // ... then the fingerprints, zero past the keys
        want.extend_from_slice(&[0; 14]);
        want.extend_from_slice(b"applebanana");
        assert_eq!(arena.to_bytes(), want);
        assert_eq!(want.len(), HEADER + block_bytes(B_U8, 1, NARROW, 1) + 11);
        assert_eq!(arena.get(0), Some("apple")); // `get` never filters
        assert_eq!(arena.get_matching(0, 0xA5), Some("apple"));
        assert_eq!(arena.get_matching(0, 0xA4), None);
        assert_eq!(arena.get_matching(1, 0x5A), Some("banana"));
        assert_eq!(arena.get_matching(2, 0), None);
        assert_eq!(arena.span_matching(1, 0x5A), arena.span(1));
        assert_eq!(arena.span_matching(1, 0x5B), None);
    }

    /// The filter under every layout the writer produces — one-byte blocks, two-byte blocks, the
    /// flat table — each exactly one byte per slot larger than its plain twin, with the prefix
    /// `build_to_file` lays out for the same lengths identical to the header `build_exact` wrote.
    #[test]
    fn every_layout_stores_and_checks_fingerprints() {
        let long = "x".repeat(70_000);
        let corpora: [(Vec<String>, u8, usize); 3] = [
            (
                (0..40).map(|i| format!("k{i}")).collect(),
                BLOCK_U8,
                3 * B_U8,
            ),
            (
                (0..200).map(|i| format!("{i:020}")).collect(),
                BLOCK_U16,
                B_U16,
            ),
            (vec![long, "tail".into(), String::new()], NARROW as u8, 3),
        ];
        for (keys, kind, extra) in corpora {
            let n = keys.len();
            let fps: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let data_len = keys.iter().map(String::len).sum();
            let plain = StringArena::build(&keys);
            let arena = StringArena::build_exact(&keys, n, data_len, Some(&fps[..]));
            assert_eq!(plain.tag(), kind);
            assert_eq!(arena.tag(), kind | FP);
            assert_eq!(arena.to_bytes().len(), plain.to_bytes().len() + extra);
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(arena.get(i), Some(key.as_str()), "{kind:#x} key {i}");
                assert_eq!(arena.get_matching(i, fps[i]), Some(key.as_str()));
                assert_eq!(arena.get_matching(i, fps[i] ^ 1), None);
                // Nothing stored, nothing filtered.
                assert_eq!(plain.get_matching(i, fps[i] ^ 1), Some(key.as_str()));
            }
            assert_eq!(arena.get_matching(n, fps[0]), None);
            let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
            assert!(restored.has_fingerprints());
            assert_eq!(
                restored.get_matching(n - 1, fps[n - 1]),
                Some(keys[n - 1].as_str())
            );
            #[cfg(feature = "mmap")]
            {
                let lens: Vec<u32> = keys.iter().map(|k| k.len() as u32).collect();
                let (prefix, len, tag, tail) =
                    StringArena::prefix_for_lengths(&lens, Some(&fps[..]));
                assert_eq!((len, tag), (data_len, kind | FP));
                assert_eq!(prefix[..], arena.to_bytes()[..prefix.len()]);
                for (i, key) in keys.iter().enumerate() {
                    let (lo, hi) = StringArena::span_at(&prefix, &tail, tag, i);
                    assert_eq!((hi - lo) as usize, key.len(), "{kind:#x} key {i}");
                }
            }
        }
    }

    /// A wrong count beside fingerprints: the assembly is redone from the iterator's own totals
    /// and the fingerprints stay attached to their strings.
    #[test]
    fn a_wrong_hint_keeps_the_fingerprints() {
        let items = ["apple", "banana", "", "cherry"];
        let fps = [1u8, 2, 3, 4];
        let want = StringArena::build_exact(items, 4, 17, Some(&fps[..])).to_bytes();
        for (n, data_len) in [(3, 17), (5, 17), (4, 0), (4, 999)] {
            let got = StringArena::build_exact(items, n, data_len, Some(&fps[..]));
            assert_eq!(got.to_bytes(), want, "hint n={n}, data_len={data_len}");
            assert_eq!(got.get_matching(3, 4), Some("cherry"));
            assert_eq!(got.get_matching(3, 5), None);
        }
    }

    /// The bit on a layout that does not exist is still an unknown encoding, and a fingerprinted
    /// header cut short is still a truncated one.
    #[test]
    fn a_fingerprinted_header_is_validated_like_a_plain_one() {
        let mut blob =
            StringArena::build_exact(["apple", "banana"], 2, 11, Some(&[1, 2][..])).to_bytes();
        blob[8] = 0x13 | FP;
        assert!(StringArena::from_bytes(&blob).is_err());
        blob[8] = BLOCK_U8 | FP;
        assert!(StringArena::from_bytes(&blob).is_ok());
        blob.truncate(HEADER + 10);
        assert!(StringArena::from_bytes(&blob).is_err());
    }

    /// Past the `u32` limit a block's base is a `u64` and the offsets stay blocked — 1.56 or 2.04
    /// bytes per key where the flat `u64` table cost 8. The limit is injected, so a small arena
    /// takes the layout a 4 GiB one would; a long key among short ones gets an entry of `u64`s,
    /// and two keys of which one passes 64 KiB still get the flat `u64` table.
    #[test]
    fn past_the_narrow_limit_the_bases_widen_and_the_offsets_stay_blocked() {
        let corpora: [(Vec<String>, u8, usize, usize); 2] = [
            (
                (0..40).map(|i| format!("k{i}")).collect(),
                BLOCK_U8,
                B_U8,
                1,
            ),
            (
                (0..200).map(|i| format!("{i:020}")).collect(),
                BLOCK_U16,
                B_U16,
                2,
            ),
        ];
        for (keys, layout, slots, width) in corpora {
            let n = keys.len();
            let data_len: usize = keys.iter().map(String::len).sum();
            let fps: Vec<u8> = (0..n).map(|i| i as u8 ^ 0x5A).collect();
            for fp in [None, Some(&fps[..])] {
                let arena = StringArena::build_exact_limited(&keys, n, data_len, fp, 16);
                let want_tag = tag_with(layout | WIDE_BASE, fp.is_some());
                assert_eq!(arena.tag(), want_tag);
                let blocks = n.div_ceil(slots);
                assert_eq!(
                    arena.to_bytes().len(),
                    HEADER
                        + blocks * block_bytes(slots, width, WIDE, fp.map_or(0, |_| 1))
                        + data_len
                );
                // The same corpus below the limit: `u32` bases, each block 4 bytes shorter.
                let narrow = StringArena::build_exact_limited(&keys, n, data_len, fp, data_len);
                assert_eq!(narrow.tag(), tag_with(layout, fp.is_some()));
                assert_eq!(arena.to_bytes().len(), narrow.to_bytes().len() + 4 * blocks);
                for (i, key) in keys.iter().enumerate() {
                    assert_eq!(arena.get(i), Some(key.as_str()), "{want_tag:#x} key {i}");
                    if let Some(fps) = fp {
                        assert_eq!(arena.get_matching(i, fps[i]), Some(key.as_str()));
                        assert_eq!(arena.get_matching(i, fps[i] ^ 1), None);
                    }
                }
                assert_eq!(arena.get(n), None);
                let restored = StringArena::from_bytes(&arena.to_bytes()).unwrap();
                assert_eq!(restored.tag(), want_tag);
                assert_eq!(restored.get(n - 1), Some(keys[n - 1].as_str()));
                #[cfg(feature = "mmap")]
                {
                    let lens: Vec<u32> = keys.iter().map(|k| k.len() as u32).collect();
                    let (prefix, len, tag, tail) =
                        StringArena::prefix_for_lengths_limited(&lens, fp, 16);
                    assert_eq!((len, tag), (data_len, want_tag));
                    assert_eq!(prefix[..], arena.to_bytes()[..prefix.len()]);
                    for (i, key) in keys.iter().enumerate() {
                        let (lo, hi) = StringArena::span_at(&prefix, &tail, tag, i);
                        assert_eq!((hi - lo) as usize, key.len(), "{want_tag:#x} key {i}");
                    }
                }
            }
        }
        let mut keys: Vec<String> = (0..100).map(|i| format!("k{i}")).collect();
        keys[50] = "x".repeat(300);
        let data_len: usize = keys.iter().map(String::len).sum();
        let arena = StringArena::build_exact_limited(&keys, 100, data_len, None, 16);
        assert_eq!(arena.tag(), BLOCK_U8 | WIDE_BASE | OVERFLOW);
        assert_eq!(
            arena.to_bytes().len(),
            HEADER + 7 * block_bytes(B_U8, 1, WIDE, 0) + data_len + (B_U8 + 1) * WIDE + TRAILER
        );
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(arena.get(i), Some(key.as_str()), "key {i}");
        }
        let long = "x".repeat(70_000);
        let items = [long.as_str(), "tail"];
        let arena = StringArena::build_exact_limited(items, 2, 70_004, None, 16);
        assert_eq!(arena.tag(), WIDE as u8);
        assert_eq!(arena.to_bytes().len(), HEADER + 3 * WIDE + 70_004);
        assert_eq!(arena.get(0), Some(long.as_str()));
        assert_eq!(arena.get(1), Some("tail"));
    }

    /// The wide-base bit means nothing on a flat table, and on a blocked one it changes the
    /// block's size — so a tag that claims it over bytes laid out without it is refused, not read
    /// off by one base width.
    #[test]
    fn a_wide_base_bit_is_refused_where_it_does_not_belong() {
        let mut flat = Vec::new();
        flat.extend_from_slice(&3u64.to_le_bytes());
        flat.push(NARROW as u8 | WIDE_BASE);
        for o in [0u32, 5, 11] {
            flat.extend_from_slice(&o.to_le_bytes());
        }
        flat.extend_from_slice(b"applebanana");
        assert!(StringArena::from_bytes(&flat).is_err());
        let mut blocked = StringArena::build(["apple", "banana"]).to_bytes();
        blocked[8] = BLOCK_U8 | WIDE_BASE;
        assert!(StringArena::from_bytes(&blocked).is_err());
    }
}
