//! An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways, in
//! about a third of what [`StringIndex`](crate::StringIndex) takes.
//!
//! The sorted keys are cut into blocks of `block` keys (32 by default). A block stores its first
//! key whole and every other as the length of the prefix it shares with its predecessor and the
//! suffix after it, the suffix under a static symbol table ([`fsst`]) trained on the index's own
//! suffixes. Beside the blocks sit three flat arrays with one entry per block: where its head key
//! ends, an eight-byte sample of that head, and where its entries start.
//!
//! `id` is a binary search over the samples, then over the heads of the few blocks whose sample
//! equals the probe's, then one block scanned without decoding anything: an entry's stored suffix
//! is compared against the probe symbol by symbol, and the shared-prefix length says on its own
//! when the probe has been passed. `key(id)` is the block's head and at most `block − 1` decodes,
//! each one eight-byte store per code. There are no automata: a prefix or fuzzy query is a
//! `StringIndex` question, and this index answers exact ones at 3–4 bytes per key.

use crate::IndexError;
use crate::fsst::{self, ESCAPE, Table};
use std::cmp::Ordering;

/// `[magic 4][n u64][block u32][heads u64][data u64][table u32][payload u64][check u32]`, then
/// the symbol table, the head keys end to end, the head ends (`u32` each), the head samples
/// (`u64`), the block starts (`u64`), and the block data.
const MAGIC: &[u8; 4] = b"BDX1";
const HEADER: usize = 48;
const CHECKED: usize = 44; // header bytes the trailing check covers
const PER_BLOCK: usize = 4 + 8 + 8; // bytes the three arrays hold per block
const DEFAULT_BLOCK: usize = 32;
const MAX_BLOCK: usize = 1024;
/// About how many suffixes the symbol table is trained on: every block's worth, from blocks spread
/// evenly over the index.
const TRAIN_PIECES: usize = 20_000;
/// Entries of the per-block arrays serialised at a time.
const CHUNK: usize = 4096;

/// An ordered dictionary with the key stored for every id: exact `string ↔ rank` both ways.
///
/// Ids are ranks. `id(key)` is the number of keys below it, `key(id)` the key at that rank, and
/// [`lower_bound`](Self::lower_bound) the rank a key would have, so every range of keys is a
/// range of ids. About 3.5 bytes per key on real words, against 5.95 for the transducer of
/// [`StringIndex`](crate::StringIndex) and 10.9 for [`PerfectHashIndex`](crate::PerfectHashIndex);
/// `id` costs a few hundred nanoseconds and `key` about two hundred, both dominated by the block
/// scan, which the `block` given at build time sets — smaller blocks are faster and larger.
///
/// Immutable once built, and built in memory: the keys are sorted and deduplicated, then encoded
/// block by block. Persisted with [`to_bytes`](Self::to_bytes) / [`save`](Self::save) and read
/// back by [`from_bytes`](Self::from_bytes) / [`load`](Self::load), which check every length and
/// both checksums; there is no `load_mmap`.
pub struct DictIndex {
    block: usize,
    n: usize,
    /// Every block's first key, whole, end to end; `head_ends[b]` closes block `b`'s.
    heads: Vec<u8>,
    head_ends: Vec<u32>,
    /// The first eight bytes of each head as a big-endian word, so a search compares heads only
    /// inside the run of blocks that share the probe's.
    samples: Vec<u64>,
    /// Where block `b`'s `block − 1` front-coded entries start in `data`.
    blocks: Vec<u64>,
    data: Vec<u8>,
    table: Table,
}

/// The sample the search runs on: a key's first eight bytes, zero-padded, in byte order.
#[inline(always)]
fn sample_of(key: &[u8]) -> u64 {
    fsst::word_at(key, 0).swap_bytes()
}

/// How many leading bytes `a` and `b` share.
#[inline]
fn lcp(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = fsst::word_at(a, i) ^ fsst::word_at(b, i);
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

fn put_varint(out: &mut Vec<u8>, mut v: usize) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// The varint at the start of `data` and the bytes after it; `None` if it is cut short or would
/// not fit a `usize`.
fn get_varint(mut data: &[u8]) -> Option<(usize, &[u8])> {
    let mut v = 0usize;
    let mut shift = 0u32;
    loop {
        let (&b, rest) = data.split_first()?;
        data = rest;
        let part = (b & 0x7F) as usize;
        if shift >= usize::BITS || part.checked_shl(shift)? >> shift != part {
            return None;
        }
        v |= part << shift;
        if b < 0x80 {
            return Some((v, data));
        }
        shift += 7;
    }
}

/// An entry's header: one byte `lcp << 4 | len` when both are below 15, else `0xFF` and the two
/// as varints.
fn put_header(out: &mut Vec<u8>, lcp: usize, len: usize) {
    if lcp < 15 && len < 15 {
        out.push(((lcp << 4) | len) as u8);
    } else {
        out.push(0xFF);
        put_varint(out, lcp);
        put_varint(out, len);
    }
}

/// The header at the start of `data` as (lcp, len, the bytes after it); `None` if it is cut short.
#[inline(always)]
fn get_header(data: &[u8]) -> Option<(usize, usize, &[u8])> {
    let (&b, rest) = data.split_first()?;
    if b != 0xFF {
        return Some(((b >> 4) as usize, (b & 0xF) as usize, rest));
    }
    let (lcp, rest) = get_varint(rest)?;
    let (len, rest) = get_varint(rest)?;
    Some((lcp, len, rest))
}

impl DictIndex {
    /// Build from a collection of strings, in any order; duplicates are removed and the ids are
    /// the ranks of the distinct keys in byte order. Blocks of 32 keys.
    pub fn build<I, S>(items: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_with_block(items, DEFAULT_BLOCK)
    }

    /// [`build`](Self::build) with `block` keys per block, `1..=1024`. A lookup scans up to
    /// `block − 1` entries and a reverse lookup decodes up to that many, so smaller blocks are
    /// faster; larger ones share more and store less. On real words 16 / 32 / 64 give
    /// 4.35 / 3.52 / 3.10 bytes per key, `id` at 283 / 301 / 345 ns.
    pub fn build_with_block<I, S>(items: I, block: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block must be in 1..=1024"));
        }
        let mut keys: Vec<String> = items.into_iter().map(|s| s.as_ref().to_owned()).collect();
        keys.sort_unstable();
        keys.dedup();
        Self::from_sorted(&keys, block)
    }

    fn from_sorted(keys: &[String], block: usize) -> Result<Self, IndexError> {
        let n = keys.len();
        let nb = n.div_ceil(block);
        // Train on the suffixes a spread of blocks would store.
        let step = (nb * (block - 1) / TRAIN_PIECES).max(1);
        let mut pieces: Vec<&[u8]> = Vec::new();
        for (b, chunk) in keys.chunks(block).enumerate() {
            if b % step != 0 {
                continue;
            }
            for w in chunk.windows(2) {
                let l = lcp(w[0].as_bytes(), w[1].as_bytes());
                pieces.push(&w[1].as_bytes()[l..]);
            }
        }
        let table = Table::train(&pieces);
        let encoder = table.encoder();
        let mut heads = Vec::new();
        let mut head_ends = Vec::with_capacity(nb);
        let mut samples = Vec::with_capacity(nb);
        let mut blocks = Vec::with_capacity(nb);
        let mut data = Vec::new();
        let mut packed = Vec::with_capacity(64);
        for chunk in keys.chunks(block) {
            let head = chunk[0].as_bytes();
            heads.extend_from_slice(head);
            let end = u32::try_from(heads.len()).map_err(|_| {
                IndexError::Format("dict: the block heads exceed 4 GiB; use a larger block")
            })?;
            head_ends.push(end);
            samples.push(sample_of(head));
            blocks.push(data.len() as u64);
            for w in chunk.windows(2) {
                let l = lcp(w[0].as_bytes(), w[1].as_bytes());
                packed.clear();
                encoder.encode_into(&w[1].as_bytes()[l..], &mut packed);
                put_header(&mut data, l, packed.len());
                data.extend_from_slice(&packed);
            }
        }
        Ok(Self {
            block,
            n,
            heads,
            head_ends,
            samples,
            blocks,
            data,
            table,
        })
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Keys per block, as given at build time.
    pub fn block(&self) -> usize {
        self.block
    }

    #[inline(always)]
    fn head(&self, b: usize) -> &[u8] {
        let start = if b == 0 {
            0
        } else {
            self.head_ends[b - 1] as usize
        };
        &self.heads[start..self.head_ends[b] as usize]
    }

    #[inline(always)]
    fn block_data(&self, b: usize) -> &[u8] {
        let end = self
            .blocks
            .get(b + 1)
            .map_or(self.data.len(), |&e| e as usize);
        &self.data[self.blocks[b] as usize..end]
    }

    /// How many leading bytes an entry's stored suffix shares with `rest`, and how the suffix
    /// orders against it — read off the codes, nothing decoded. A stream this crate did not write
    /// ends the suffix where it stops making sense.
    #[inline]
    fn compare_piece(&self, packed: &[u8], rest: &[u8]) -> (usize, Ordering) {
        let mut c = 0;
        let mut i = 0;
        while i < packed.len() {
            let (word, len) = if packed[i] == ESCAPE {
                let Some(&b) = packed.get(i + 1) else {
                    break;
                };
                i += 2;
                (u64::from(b), 1)
            } else {
                let Some(sym) = self.table.symbol(packed[i]) else {
                    break;
                };
                i += 1;
                sym
            };
            let m = len.min(rest.len() - c);
            let theirs = fsst::word_at(rest, c);
            let x = (word ^ theirs) & fsst::low_mask(m);
            if x != 0 {
                let d = (x.trailing_zeros() / 8) as usize;
                let (a, b) = ((word >> (8 * d)) as u8, (theirs >> (8 * d)) as u8);
                return (c + d, a.cmp(&b));
            }
            c += m;
            if m < len {
                return (c, Ordering::Greater);
            }
        }
        let ord = if c == rest.len() {
            Ordering::Equal
        } else {
            Ordering::Less
        };
        (c, ord)
    }

    /// The rank of the first key not below `probe`, and whether that key is `probe`.
    fn locate(&self, probe: &[u8]) -> (u64, bool) {
        if self.n == 0 {
            return (0, false);
        }
        // The blocks whose heads share the probe's first eight bytes, and the one before them:
        // the probe can only be in one of these.
        let s = sample_of(probe);
        let lo = self.samples.partition_point(|&x| x < s);
        let hi = self.samples.partition_point(|&x| x <= s);
        // The last block in [from, hi) whose head is not past the probe.
        let (mut l, mut r) = (lo.saturating_sub(1), hi);
        while l < r {
            let m = l + (r - l) / 2;
            if self.head(m) <= probe {
                l = m + 1;
            } else {
                r = m;
            }
        }
        if l == 0 {
            return (0, false);
        }
        let b = l - 1;
        let base = (b * self.block) as u64;
        let head = self.head(b);
        if head == probe {
            return (base, true);
        }
        let count = (self.n - b * self.block).min(self.block);
        // The probe is above the previous entry and shares `matched` bytes with it.
        let mut matched = lcp(head, probe);
        let mut at = self.block_data(b);
        for j in 1..count as u64 {
            let Some((l, len, rest)) = get_header(at) else {
                return (base + j, false);
            };
            let (piece, tail) = rest.split_at(len.min(rest.len()));
            at = tail;
            // This entry differs from the previous one at `l`. Below `matched` the probe agreed
            // with the previous entry, so a shorter shared prefix puts this entry past the probe;
            // a longer one keeps it below, with nothing new matched.
            if l < matched {
                return (base + j, false);
            }
            if l > matched {
                continue;
            }
            let (c, ord) = self.compare_piece(piece, &probe[matched..]);
            match ord {
                Ordering::Equal => return (base + j, true),
                Ordering::Greater => return (base + j, false),
                Ordering::Less => matched += c,
            }
        }
        (base + count as u64, false)
    }

    /// Rank of `key` if it is a member.
    pub fn id(&self, key: &str) -> Option<u64> {
        self.id_bytes(key.as_bytes())
    }

    pub(crate) fn id_bytes(&self, key: &[u8]) -> Option<u64> {
        match self.locate(key) {
            (rank, true) => Some(rank),
            _ => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.locate(key.as_bytes()).1
    }

    /// The rank of the first key not below `key`: `key`'s own id if it is a member, otherwise
    /// the id it would have, `len()` past every key. Two of these bound a range of keys as a
    /// range of ids.
    pub fn lower_bound(&self, key: &str) -> u64 {
        self.locate(key.as_bytes()).0
    }

    /// Batched [`id`](Self::id): one answer per key, aligned with `keys`.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u64>> {
        self.ids_of_with(keys.len(), |i| keys[i].as_ref().as_bytes())
    }

    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        (0..n).map(|i| self.id_bytes(key(i))).collect()
    }

    /// Decode the next entry of a block onto `cur`, which holds the previous one, moving `at`
    /// past it; `false` on data this crate did not write.
    #[inline]
    fn advance(&self, at: &mut &[u8], cur: &mut Vec<u8>) -> bool {
        let Some((l, len, rest)) = get_header(at) else {
            return false;
        };
        if l > cur.len() || len > rest.len() {
            return false;
        }
        cur.truncate(l);
        let (piece, tail) = rest.split_at(len);
        *at = tail;
        self.table.decode_into(piece, cur)
    }

    /// The key at rank `id` into `out`, cleared first; `false`, with `out` empty, past the last
    /// key.
    fn key_bytes_into(&self, id: u64, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        if id >= self.n {
            return false;
        }
        let b = id / self.block;
        out.extend_from_slice(self.head(b));
        let mut at = self.block_data(b);
        (0..id % self.block).all(|_| self.advance(&mut at, out))
    }

    /// The key at rank `id`; `None` at or past `len()`.
    pub fn key(&self, id: u64) -> Option<String> {
        let mut out = String::new();
        self.key_into(id, &mut out).then_some(out)
    }

    /// [`key`](Self::key) into a string the caller keeps, so a loop over ids allocates nothing:
    /// `out` is cleared and, when the answer is `true`, holds the key. `false` at or past
    /// `len()`, and for the key a corrupted blob decodes to something that is not UTF-8.
    pub fn key_into(&self, id: u64, out: &mut String) -> bool {
        let mut buf = std::mem::take(out).into_bytes();
        let found = self.key_bytes_into(id, &mut buf);
        match String::from_utf8(buf) {
            Ok(s) => {
                *out = s;
                found
            }
            Err(_) => false,
        }
    }

    /// Every key with its id, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (String, u64)> + '_ {
        self.iter_from(0)
    }

    /// [`iter`](Self::iter) from rank `start` on.
    pub(crate) fn iter_from(&self, start: u64) -> impl Iterator<Item = (String, u64)> + '_ {
        let mut id = usize::try_from(start).unwrap_or(usize::MAX);
        let mut cur: Vec<u8> = Vec::new();
        let mut at: &[u8] = &[];
        let mut primed = false;
        std::iter::from_fn(move || {
            if id >= self.n {
                return None;
            }
            let (b, j) = (id / self.block, id % self.block);
            let ok = if !primed || j == 0 {
                cur.clear();
                cur.extend_from_slice(self.head(b));
                at = self.block_data(b);
                let skip = if primed { 0 } else { j };
                primed = true;
                (0..skip).all(|_| self.advance(&mut at, &mut cur))
            } else {
                self.advance(&mut at, &mut cur)
            };
            if !ok {
                id = self.n; // a stream this crate did not write ends the walk
                return None;
            }
            let this = id as u64;
            id += 1;
            Some((String::from_utf8_lossy(&cur).into_owned(), this))
        })
    }

    /// The payload sections in order, each handed to `f` once; the per-block arrays go out
    /// `CHUNK` entries at a time so nothing the size of the index is copied.
    fn sections(
        &self,
        mut f: impl FnMut(&[u8]) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        let mut table = Vec::with_capacity(self.table.serialized_len());
        self.table.write_to(&mut table);
        f(&table)?;
        f(&self.heads)?;
        let mut buf = Vec::with_capacity(8 * CHUNK);
        for chunk in self.head_ends.chunks(CHUNK) {
            buf.clear();
            chunk
                .iter()
                .for_each(|e| buf.extend_from_slice(&e.to_le_bytes()));
            f(&buf)?;
        }
        for array in [&self.samples, &self.blocks] {
            for chunk in array.chunks(CHUNK) {
                buf.clear();
                chunk
                    .iter()
                    .for_each(|e| buf.extend_from_slice(&e.to_le_bytes()));
                f(&buf)?;
            }
        }
        f(&self.data)
    }

    fn header(&self) -> [u8; HEADER] {
        let mut hasher = crate::blob::BlockHasher::new();
        self.sections(|s| {
            hasher.update(s);
            Ok(())
        })
        .expect("hashing the sections cannot fail");
        let mut h = [0u8; HEADER];
        h[0..4].copy_from_slice(MAGIC);
        h[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        h[12..16].copy_from_slice(&(self.block as u32).to_le_bytes());
        h[16..24].copy_from_slice(&(self.heads.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&(self.data.len() as u64).to_le_bytes());
        h[32..36].copy_from_slice(&(self.table.serialized_len() as u32).to_le_bytes());
        h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
        let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        h
    }

    /// Serialise to `[magic "BDX1"][n][block][head bytes][data bytes][table bytes][payload]
    /// [check]`, then the symbol table, the head keys, the three per-block arrays and the block
    /// data. `check` is a hash of the preceding header bytes and `payload` a hash of everything
    /// after it, both verified on load.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_len());
        out.extend_from_slice(&self.header());
        self.sections(|s| {
            out.extend_from_slice(s);
            Ok(())
        })
        .expect("appending the sections cannot fail");
        out
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it.
    pub fn serialized_len(&self) -> usize {
        HEADER
            + self.table.serialized_len()
            + self.heads.len()
            + self.blocks.len() * PER_BLOCK
            + self.data.len()
    }

    /// Reconstruct from [`DictIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes: the magic, both checksums, every section length, the symbol
    /// table and the three per-block arrays are checked before anything is trusted, so a
    /// crafted blob is at worst *wrong* — a key that is not the one built, a shorter walk —
    /// never out of bounds. The block data itself is read with every access bounded.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED..HEADER].try_into().unwrap());
        if check != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        let stored = u64::from_le_bytes(bytes[36..44].try_into().unwrap());
        if stored != crate::blob::hash_block(&bytes[HEADER..]) {
            return Err(IndexError::Format("payload checksum mismatch"));
        }
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let n = usize::try_from(u64_at(4))
            .map_err(|_| IndexError::Format("dict: key count out of range"))?;
        let block = u32_at(12) as usize;
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block size out of range"));
        }
        let heads_len = usize::try_from(u64_at(16))
            .map_err(|_| IndexError::Format("dict: head bytes out of range"))?;
        let data_len = usize::try_from(u64_at(24))
            .map_err(|_| IndexError::Format("dict: block data out of range"))?;
        let table_len = u32_at(32) as usize;
        let nb = n.div_ceil(block);
        let arrays = nb
            .checked_mul(PER_BLOCK)
            .ok_or(IndexError::Format("dict: block count out of range"))?;
        let total = HEADER
            .checked_add(table_len)
            .and_then(|t| t.checked_add(heads_len))
            .and_then(|t| t.checked_add(arrays))
            .and_then(|t| t.checked_add(data_len));
        if total != Some(bytes.len()) {
            return Err(IndexError::Format(
                "dict: the section lengths do not add up to the blob",
            ));
        }
        let mut at = HEADER;
        let mut take = |len: usize| {
            at += len;
            &bytes[at - len..at]
        };
        let table = Table::from_bytes(take(table_len))
            .ok_or(IndexError::Format("dict: bad symbol table"))?;
        let heads = take(heads_len).to_vec();
        let head_ends = take(nb * 4)
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let words = |bytes: &[u8]| -> Vec<u64> {
            bytes
                .chunks_exact(8)
                .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                .collect()
        };
        let samples = words(take(nb * 8));
        let blocks = words(take(nb * 8));
        let data = take(data_len).to_vec();
        let idx = Self {
            block,
            n,
            heads,
            head_ends,
            samples,
            blocks,
            data,
            table,
        };
        idx.check_layout()?;
        Ok(idx)
    }

    /// The array invariants every query relies on: heads and blocks in order and inside their
    /// sections, every sample the one its head gives.
    fn check_layout(&self) -> Result<(), IndexError> {
        let nb = self.blocks.len();
        if nb == 0 {
            return if self.heads.is_empty() && self.data.is_empty() {
                Ok(())
            } else {
                Err(IndexError::Format("dict: an empty index with key bytes"))
            };
        }
        let mut prev = 0;
        for &end in &self.head_ends {
            let end = end as usize;
            if end < prev || end > self.heads.len() {
                return Err(IndexError::Format("dict: head table out of order"));
            }
            prev = end;
        }
        if prev != self.heads.len() {
            return Err(IndexError::Format(
                "dict: head table does not cover the head bytes",
            ));
        }
        let mut prev = 0;
        for &start in &self.blocks {
            if start < prev || start > self.data.len() as u64 {
                return Err(IndexError::Format("dict: block table out of order"));
            }
            prev = start;
        }
        if self.blocks[0] != 0 {
            return Err(IndexError::Format("dict: block table out of order"));
        }
        if (0..nb).any(|b| self.samples[b] != sample_of(self.head(b))) {
            return Err(IndexError::Format(
                "dict: head samples do not match the heads",
            ));
        }
        Ok(())
    }

    /// Whether `bytes` loads, and whether what loaded answers without panicking. Exists for the
    /// libFuzzer target in `fuzz/`; see the `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_load_and_query(bytes: &[u8]) -> bool {
        let Ok(idx) = Self::from_bytes(bytes) else {
            return false;
        };
        let n = idx.len() as u64;
        for probe in ["", "a", "zzzzzzzzzzzzzzzzz", "\u{10FFFF}"] {
            assert!(idx.id(probe).is_none_or(|id| id < n), "{probe:?}");
            assert!(idx.lower_bound(probe) <= n, "{probe:?}");
        }
        for id in [0, 1, n / 2, n.saturating_sub(1), n, u64::MAX] {
            assert!(id < n || idx.key(id).is_none(), "key({id}) past {n}");
        }
        assert!(idx.iter().take(64).count() as u64 <= n);
        true
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        w.write_all(&self.header())?;
        self.sections(|s| Ok(w.write_all(s)?))
    }

    /// Load an index previously written with [`DictIndex::save`]. Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_bytes(&std::fs::read(path)?)
    }
}

impl std::fmt::Debug for DictIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictIndex")
            .field("len", &self.n)
            .field("block", &self.block)
            .field("bytes", &self.serialized_len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden keys plus the shapes the encoding has cases for: the empty key, NUL bytes,
    /// multibyte characters, a prefix of another key, a shared prefix and a suffix past the
    /// one-byte header's fifteen, and a key past eight bytes that shares its sample with others.
    fn corpus() -> Vec<String> {
        let mut keys: Vec<String> = include_str!("../tests/data/golden-keys.txt")
            .lines()
            .map(str::to_owned)
            .collect();
        let long = "x".repeat(20);
        keys.extend(
            [
                "",
                "a",
                "a\0",
                "a\0b",
                "é",
                "école",
                "écoles",
                "ka",
                "ka-",
                "ka-00",
                "ka-0000-and-then-some-more-bytes-than-fifteen",
                "ka-0000-and-then-some-more-bytes-than-sixteen",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        keys.push(long.clone());
        keys.push(format!("{long}a"));
        keys.push(format!("{long}b{}", "y".repeat(40)));
        keys.push(format!("{long}b{}z", "y".repeat(40)));
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn probes(keys: &[String]) -> Vec<String> {
        let mut out = vec![
            String::new(),
            "\u{10FFFF}".into(),
            "kb".into(),
            "ka-00000".into(),
        ];
        for k in keys.iter().step_by(7) {
            out.push(format!("{k}x"));
            out.push(k[..k.len() - k.chars().last().map_or(0, char::len_utf8)].to_owned());
            out.push(format!("{k}\0"));
        }
        out
    }

    fn check(idx: &DictIndex, keys: &[String]) {
        assert_eq!(idx.len(), keys.len());
        let mut buf = String::from("scratch");
        for (rank, k) in keys.iter().enumerate() {
            let rank = rank as u64;
            assert_eq!(idx.id(k), Some(rank), "{k:?}");
            assert!(idx.contains(k), "{k:?}");
            assert_eq!(idx.lower_bound(k), rank, "{k:?}");
            assert_eq!(idx.key(rank).as_deref(), Some(k.as_str()), "{rank}");
            assert!(idx.key_into(rank, &mut buf), "{rank}");
            assert_eq!(&buf, k, "{rank}");
        }
        for p in probes(keys) {
            let expect = keys.partition_point(|k| k.as_bytes() < p.as_bytes()) as u64;
            let member = keys.get(expect as usize).is_some_and(|k| *k == p);
            assert_eq!(idx.id(&p), member.then_some(expect), "{p:?}");
            assert_eq!(idx.contains(&p), member, "{p:?}");
            assert_eq!(idx.lower_bound(&p), expect, "{p:?}");
        }
        let n = keys.len() as u64;
        assert_eq!(idx.key(n), None);
        assert_eq!(idx.key(u64::MAX), None);
        assert!(!idx.key_into(n, &mut buf) && buf.is_empty());
        let walked: Vec<(String, u64)> = idx.iter().collect();
        let expect: Vec<(String, u64)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u64))
            .collect();
        assert_eq!(walked, expect);
        assert_eq!(idx.ids_of(&keys[..5]), (0..5).map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn answers_every_key_and_stranger_at_every_block_size() {
        let keys = corpus();
        for block in [1, 2, 3, 7, 32, 500, 1024] {
            let idx = DictIndex::build_with_block(&keys, block).unwrap();
            assert_eq!(idx.block(), block);
            check(&idx, &keys);
            let blob = idx.to_bytes();
            assert_eq!(blob.len(), idx.serialized_len(), "block {block}");
            assert_eq!(&blob[..4], b"BDX1");
            let back = DictIndex::from_bytes(&blob).unwrap();
            assert_eq!(back.to_bytes(), blob, "block {block}");
            check(&back, &keys);
        }
    }

    #[test]
    fn iter_from_resumes_inside_a_block() {
        let keys = corpus();
        let idx = DictIndex::build_with_block(&keys, 5).unwrap();
        for start in [
            0u64,
            1,
            4,
            5,
            6,
            17,
            keys.len() as u64 - 1,
            keys.len() as u64,
            u64::MAX,
        ] {
            let got: Vec<u64> = idx.iter_from(start).map(|(_, id)| id).collect();
            let expect: Vec<u64> = (start.min(keys.len() as u64)..keys.len() as u64).collect();
            assert_eq!(got, expect, "from {start}");
            assert!(
                idx.iter_from(start).all(|(k, id)| Some(k) == idx.key(id)),
                "from {start}"
            );
        }
    }

    #[test]
    fn duplicates_and_input_order_do_not_matter() {
        let a = DictIndex::build(["pear", "apple", "fig", "apple"]).unwrap();
        let b = DictIndex::build(["fig", "pear", "apple"]).unwrap();
        assert_eq!(a.to_bytes(), b.to_bytes());
        assert_eq!(a.len(), 3);
        assert_eq!(a.id("apple"), Some(0));
        assert_eq!(a.id("pear"), Some(2));
        assert_eq!(a.lower_bound("banana"), 1);
        assert_eq!(a.lower_bound("zebra"), 3);
    }

    #[test]
    fn an_empty_index_answers_and_round_trips() {
        let idx = DictIndex::build(Vec::<String>::new()).unwrap();
        assert!(idx.is_empty());
        assert_eq!(
            (idx.id(""), idx.key(0), idx.lower_bound("x")),
            (None, None, 0)
        );
        assert!(!idx.contains(""));
        assert_eq!(idx.iter().count(), 0);
        let blob = idx.to_bytes();
        assert_eq!(blob.len(), idx.serialized_len());
        let back = DictIndex::from_bytes(&blob).unwrap();
        assert!(back.is_empty() && back.to_bytes() == blob);
    }

    #[test]
    fn a_block_size_outside_the_range_is_refused() {
        for block in [0, MAX_BLOCK + 1] {
            let err = DictIndex::build_with_block(["a"], block).unwrap_err();
            assert!(err.to_string().contains("block must be"), "{err}");
        }
    }

    #[test]
    fn varints_round_trip_and_a_cut_or_oversized_one_is_refused() {
        for v in [
            0usize,
            1,
            127,
            128,
            300,
            1 << 20,
            usize::MAX >> 1,
            usize::MAX,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            out.push(0xAB);
            assert_eq!(get_varint(&out), Some((v, &[0xABu8][..])));
        }
        assert_eq!(get_varint(&[]), None);
        assert_eq!(get_varint(&[0x80]), None);
        assert_eq!(get_varint(&[0x80; 12]), None);
        let mut out = Vec::new();
        put_header(&mut out, 14, 14);
        assert_eq!(out, [0xEE]);
        assert_eq!(get_header(&out), Some((14, 14, &[][..])));
        out.clear();
        put_header(&mut out, 15, 3);
        assert_eq!(out, [0xFF, 15, 3]);
        assert_eq!(get_header(&out), Some((15, 3, &[][..])));
        assert_eq!(get_header(&[]), None);
        assert_eq!(get_header(&[0xFF, 15]), None);
    }

    #[test]
    fn save_and_load() {
        let keys = corpus();
        let idx = DictIndex::build(&keys).unwrap();
        let dir = std::env::temp_dir().join(format!("lexindex-dict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.bdx");
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes());
        let back = DictIndex::load(&path).unwrap();
        check(&back, &keys);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(DictIndex::load(&path).is_err());
    }

    /// Recompute both checksums after a deliberate edit, so the structural checks are reached.
    fn reframe(blob: &mut [u8]) {
        let payload = crate::blob::hash_block(&blob[HEADER..]);
        blob[36..44].copy_from_slice(&payload.to_le_bytes());
        let check = crate::blob::hash_bytes(&blob[..CHECKED]) as u32;
        blob[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
    }

    fn refused(blob: &[u8], what: &str) {
        let err = DictIndex::from_bytes(blob).unwrap_err().to_string();
        assert!(err.contains(what), "expected {what:?}, got {err:?}");
    }

    #[test]
    fn every_length_and_table_in_a_blob_is_checked() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 2).unwrap().to_bytes();
        let u64_at = |i: usize| u64::from_le_bytes(blob[i..i + 8].try_into().unwrap()) as usize;
        let table_len = u32::from_le_bytes(blob[32..36].try_into().unwrap()) as usize;
        let (heads_len, nb) = (u64_at(16), keys.len().div_ceil(2));
        let heads_at = HEADER + table_len;
        let ends_at = heads_at + heads_len;
        let samples_at = ends_at + nb * 4;
        let blocks_at = samples_at + nb * 8;
        let data_at = blocks_at + nb * 8;
        assert_eq!(data_at + u64_at(24), blob.len());

        refused(&blob[..HEADER - 1], "truncated");
        let mut b = blob.clone();
        b[..4].copy_from_slice(b"BCL1");
        refused(&b, "bad magic");
        let mut b = blob.clone();
        b[4] ^= 1;
        refused(&b, "header checksum");
        let mut b = blob.clone();
        b[data_at] ^= 1;
        refused(&b, "payload checksum");

        let edited = |edit: &dyn Fn(&mut Vec<u8>), what: &str| {
            let mut b = blob.clone();
            edit(&mut b);
            reframe(&mut b);
            refused(&b, what);
        };
        edited(
            &|b| b[12..16].copy_from_slice(&0u32.to_le_bytes()),
            "block size",
        );
        edited(
            &|b| b[12..16].copy_from_slice(&1025u32.to_le_bytes()),
            "block size",
        );
        edited(
            &|b| b[4..12].copy_from_slice(&(keys.len() as u64 + 2).to_le_bytes()),
            "add up",
        );
        edited(
            &|b| b[16..24].copy_from_slice(&(heads_len as u64 + 1).to_le_bytes()),
            "add up",
        );
        edited(
            &|b| b[4..12].copy_from_slice(&u64::MAX.to_le_bytes()),
            "out of range",
        );
        edited(&|b| b[HEADER] = 255, "bad symbol table");
        edited(&|b| b[HEADER + 1] = 0, "bad symbol table");
        edited(
            &|b| b[ends_at..ends_at + 4].copy_from_slice(&u32::MAX.to_le_bytes()),
            "head table out of order",
        );
        edited(
            &|b| {
                let last = ends_at + (nb - 1) * 4;
                b[last..last + 4].copy_from_slice(&(heads_len as u32 - 1).to_le_bytes());
            },
            "does not cover",
        );
        edited(&|b| b[samples_at] ^= 1, "head samples");
        edited(&|b| b[blocks_at] = 1, "block table out of order");
        edited(
            &|b| b[blocks_at + 8..blocks_at + 16].copy_from_slice(&u64::MAX.to_le_bytes()),
            "block table out of order",
        );
        edited(&|b| b.push(0), "add up");

        let empty = DictIndex::build(Vec::<String>::new()).unwrap().to_bytes();
        let mut b = empty.clone();
        b[16..24].copy_from_slice(&1u64.to_le_bytes());
        b.push(b'k');
        reframe(&mut b);
        refused(&b, "empty index with key bytes");
    }

    /// Block data the crate did not write — every byte an escape, a code past the table, a header
    /// claiming more than is there — answers wrong or short, never out of bounds.
    #[test]
    fn corrupted_block_data_never_panics() {
        let keys = corpus();
        let blob = DictIndex::build_with_block(&keys, 8).unwrap().to_bytes();
        let data_len = u64::from_le_bytes(blob[24..32].try_into().unwrap()) as usize;
        let data_at = blob.len() - data_len;
        for fill in [0x00u8, 0x0F, 0xF0, 0xFE, 0xFF] {
            let mut b = blob.clone();
            b[data_at..].fill(fill);
            reframe(&mut b);
            let idx = DictIndex::from_bytes(&b).unwrap();
            for p in keys.iter().chain(probes(&keys).iter()) {
                let _ = (idx.id(p), idx.lower_bound(p));
            }
            let mut buf = String::new();
            for id in 0..keys.len() as u64 {
                let _ = (idx.key(id), idx.key_into(id, &mut buf));
            }
            assert!(idx.iter().count() <= keys.len());
        }
        // A header whose lcp exceeds the key so far, with the rest of the block intact.
        let mut b = blob.clone();
        b[data_at] = 0xF0 | (b[data_at] & 0x0F);
        reframe(&mut b);
        let idx = DictIndex::from_bytes(&b).unwrap();
        assert_eq!(idx.key(0).as_deref(), Some(keys[0].as_str()));
        let _ = idx.key(1);
        assert_eq!(idx.iter().count(), 1);
    }
}
