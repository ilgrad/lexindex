//! Fingerprint minimal-perfect-hash dictionary: the smallest `string → dense id` map.
//!
//! Like [`PerfectHashIndex`](crate::PerfectHashIndex) but it stores only a small **fingerprint** per
//! key instead of the key itself, so it costs a byte-ish per key rather than tens. Two trade-offs:
//! membership is **probabilistic** — a non-member query that hashes to a member's slot *and* whose
//! fingerprint collides is a false positive, with probability `2^-fingerprint_bits` (6.25% at 4
//! bits, ≈ 0.4% at 8, ≈ 0.0015% at 16) — and there is **no reverse `id → key`** (the keys are not
//! stored). Use it
//! for a fixed vocabulary where the tiniest footprint matters and rare false positives are acceptable;
//! reach for [`PerfectHashIndex`](crate::PerfectHashIndex) (exact membership + reverse) or
//! [`StringIndex`](crate::StringIndex) (ordered) otherwise.

use crate::IndexError;
use crate::blob::SharedBytes;
use crate::hash::{fingerprint_full, hash_key, hash_pair};
use crate::mphf::Mphf;

/// Every format before this one embedded `ptr_hash`'s `epserde` image, whose private fields no
/// loader could validate — and this index, storing no keys, could not even recompute the bound that
/// made queries safe. 1.0 replaced the backend precisely so that a blob could be checked; the old
/// images cannot be read without the crate that is now gone, so they are refused by name.
const LEGACY_MAGICS: [&[u8; 4]; 5] = [b"BCH1", b"BCH2", b"BCH3", b"BCH4", b"BCH5"];
/// `[magic 4][n u64][fp_bits u32][mph_len u64][side_len u32][payload u64][check u32]`
const MAGIC_V6: &[u8; 4] = b"BCH6";
const HEADER_V6: usize = 40;
const CHECKED_V6: usize = 36; // header bytes the trailing check covers
const SIDE_ENTRY: usize = 20; // hash u64 + fingerprint u64 + id u32

/// One `(hash, second hash)` pair in a run or the merged file.
const PAIR_BYTES: usize = 16;
/// One `(slot u32, fingerprint u64)` record in a range file.
const SLOT_BYTES: usize = 12;
/// The largest fingerprint table the streaming builder fills in memory rather than through range
/// files: 256 MiB, 268 M keys at the default width.
const FP_MEMORY: usize = 256 << 20;
/// Slots per range file past that budget: 16 M, a 16 MB segment at eight bits, and a multiple
/// of eight so that every segment starts on a byte.
const RANGE_SLOTS: usize = 1 << 24;

/// What the streaming builder may hold; only the tests pass anything but [`Budget::DEFAULT`],
/// to route a corpus of thousands the way one of billions goes.
#[derive(Clone, Copy)]
pub(crate) struct Budget {
    /// Pairs held before a run is sorted and spilled; reserved whole at the first key.
    pub(crate) run_bytes: usize,
    /// The fingerprint table filled in memory up to this size; range files past it.
    pub(crate) fp_memory: usize,
    pub(crate) range_slots: usize,
}

impl Budget {
    pub(crate) const DEFAULT: Self = Self {
        run_bytes: crate::string_index::RUN_BYTES,
        fp_memory: FP_MEMORY,
        range_slots: RANGE_SLOTS,
    };
}

/// Process-wide counter in the scratch directory's name, so two builds in one process aimed at
/// the same path do not share one.
static SCRATCH_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The streaming builder's files, in a directory beside the output: the spilled runs, the merged
/// pairs, the range files. Created at the first spill, removed when this is dropped, whichever way
/// the build ends; a corpus that fits one run and whose table fits the budget leaves no trace.
struct Scratch {
    beside: std::path::PathBuf,
    dir: Option<std::path::PathBuf>,
    runs: usize,
}

impl Scratch {
    fn beside(target: &std::path::Path) -> Self {
        Self {
            beside: target.to_path_buf(),
            dir: None,
            runs: 0,
        }
    }

    fn dir(&mut self) -> Result<&std::path::Path, IndexError> {
        if self.dir.is_none() {
            let mut dir = self.beside.clone();
            let seq = SCRATCH_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dir.as_mut_os_string()
                .push(format!(".{}.{seq}.runs", std::process::id()));
            // `create_dir`, not `create_dir_all`: a directory already at this pid-and-counter
            // name is someone else's, and an error rather than a place to write.
            std::fs::create_dir(&dir)?;
            self.dir = Some(dir);
        }
        Ok(self.dir.as_deref().expect("just created"))
    }

    fn create(&mut self, name: &str) -> Result<std::io::BufWriter<std::fs::File>, IndexError> {
        let path = self.dir()?.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(std::io::BufWriter::with_capacity(1 << 20, file))
    }

    fn path(&mut self, name: &str) -> Result<std::path::PathBuf, IndexError> {
        Ok(self.dir()?.join(name))
    }

    /// `run`, sorted and distinct, as the next run file; the buffer is emptied for reuse.
    fn spill(&mut self, run: &mut Vec<(u64, u64)>) -> Result<(), IndexError> {
        use std::io::Write;
        run.sort_unstable();
        run.dedup();
        let mut w = self.create(&self.runs.to_string())?;
        for &(h, second) in run.iter() {
            w.write_all(&h.to_le_bytes())?;
            w.write_all(&second.to_le_bytes())?;
        }
        w.flush()?;
        run.clear();
        self.runs += 1;
        Ok(())
    }

    /// Every run as one ascending, distinct file of pairs, counting them and the same-hash
    /// leftovers on the way, so the perfect hash knows its size before it reads a key.
    fn merge(&mut self) -> Result<Pairs, IndexError> {
        use std::cmp::Reverse;
        use std::io::Write;
        let mut readers = Vec::with_capacity(self.runs);
        let mut heap = std::collections::BinaryHeap::with_capacity(self.runs);
        for i in 0..self.runs {
            let mut reader = Records::<PAIR_BYTES>::open(&self.path(&i.to_string())?)?;
            if let Some(pair) = reader.next()?.map(pair_of) {
                heap.push(Reverse((pair, i)));
            }
            readers.push(reader);
        }
        let path = self.path("merged")?;
        let mut w = self.create("merged")?;
        let (mut n, mut side) = (0usize, 0usize);
        let mut last: Option<(u64, u64)> = None;
        while let Some(Reverse((pair, i))) = heap.pop() {
            if let Some(next) = readers[i].next()?.map(pair_of) {
                heap.push(Reverse((next, i)));
            }
            if last == Some(pair) {
                continue;
            }
            if last.is_some_and(|l| l.0 == pair.0) {
                side += 1;
            }
            w.write_all(&pair.0.to_le_bytes())?;
            w.write_all(&pair.1.to_le_bytes())?;
            n += 1;
            last = Some(pair);
        }
        w.flush()?;
        Ok(Pairs::File { path, n, side })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

/// Fixed-size records read back from a scratch file, `None` at a clean end; a torn record is an
/// error, since this process wrote whole ones moments ago.
struct Records<const N: usize> {
    reader: std::io::BufReader<std::fs::File>,
}

impl<const N: usize> Records<N> {
    fn open(path: &std::path::Path) -> Result<Self, IndexError> {
        Ok(Self {
            reader: std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?),
        })
    }

    fn next(&mut self) -> std::io::Result<Option<[u8; N]>> {
        use std::io::Read;
        let mut buf = [0u8; N];
        let mut got = 0;
        while got < N {
            match self.reader.read(&mut buf[got..])? {
                0 if got == 0 => return Ok(None),
                0 => {
                    return Err(std::io::Error::other(
                        "compact-hash: a scratch file is torn",
                    ));
                }
                k => got += k,
            }
        }
        Ok(Some(buf))
    }
}

fn pair_of(rec: [u8; PAIR_BYTES]) -> (u64, u64) {
    (
        u64::from_le_bytes(rec[0..8].try_into().expect("8 bytes")),
        u64::from_le_bytes(rec[8..16].try_into().expect("8 bytes")),
    )
}

/// The distinct pairs of a streaming build, ascending: in memory when the source fit one run,
/// else the merged file, read back once for the perfect hash and once for the fingerprints.
enum Pairs {
    Memory(Vec<(u64, u64)>),
    File {
        path: std::path::PathBuf,
        n: usize,
        side: usize,
    },
}

impl Pairs {
    /// Distinct pairs, and how many of them share their hash with an earlier one.
    fn counts(&self) -> (usize, usize) {
        match self {
            Pairs::Memory(pairs) => (
                pairs.len(),
                pairs.windows(2).filter(|w| w[0].0 == w[1].0).count(),
            ),
            Pairs::File { n, side, .. } => (*n, *side),
        }
    }

    fn reps(&self) -> Result<Reps<'_>, IndexError> {
        Ok(Reps {
            from: match self {
                Pairs::Memory(pairs) => From::Memory(pairs),
                Pairs::File { path, .. } => From::File(Records::open(path)?),
            },
            last: None,
            error: None,
        })
    }

    fn scan(
        &self,
        mut each: impl FnMut((u64, u64)) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        match self {
            Pairs::Memory(pairs) => pairs.iter().try_for_each(|&p| each(p)),
            Pairs::File { path, .. } => {
                let mut records = Records::<PAIR_BYTES>::open(path)?;
                while let Some(rec) = records.next()? {
                    each(pair_of(rec))?;
                }
                Ok(())
            }
        }
    }
}

enum From<'a> {
    Memory(&'a [(u64, u64)]),
    File(Records<PAIR_BYTES>),
}

/// One hash per distinct value — the first pair of each equal-hash run — for the perfect hash,
/// which cannot take a `Result`: a read error is parked and ends the stream, and the builder
/// asks for it before it trusts the table.
struct Reps<'a> {
    from: From<'a>,
    last: Option<u64>,
    error: Option<std::io::Error>,
}

impl Iterator for Reps<'_> {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        loop {
            let h = match &mut self.from {
                From::Memory(pairs) => {
                    let (&(h, _), rest) = pairs.split_first()?;
                    *pairs = rest;
                    h
                }
                From::File(records) => match records.next() {
                    Ok(Some(rec)) => pair_of(rec).0,
                    Ok(None) => return None,
                    Err(e) => {
                        self.error = Some(e);
                        return None;
                    }
                },
            };
            if self.last != Some(h) {
                self.last = Some(h);
                return Some(h);
            }
        }
    }
}

/// Header + owned sections (MPH buffer, side buffer) of a serialised blob.
type SerialisedParts = ([u8; HEADER_V6], Vec<u8>, Vec<u8>);

/// A writer that hashes what passes through it, for a payload written in pieces.
struct Hashed<'a, W: std::io::Write>(&'a mut W, &'a mut crate::blob::BlockHasher);

impl<W: std::io::Write> std::io::Write for Hashed<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf)?;
        self.1.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// The validated framing of a blob — every field a query will trust — with the MPH region located
/// but not parsed. Produced by `parse_frame` and consumed by `from_shared`; both are safe, because
/// the MPH region validates itself (see [`Mphf::from_bytes`]).
struct Frame {
    n: usize,
    fp_bits: u32,
    mph: std::ops::Range<usize>, // the MPH region; ignored when `n == 0`
    fps: SharedBytes,
    side: Vec<(u64, u64, u32)>,
}

/// The smallest string→dense-id dictionary: a minimal perfect hash plus one small fingerprint per key.
pub struct CompactHashIndex {
    mph: Option<Mphf>, // over one hash per distinct hash value; None iff empty
    fps: SharedBytes,  // m fingerprints of fp_bits each, bit-packed in slot order
    fp_bits: u32,      // 1..=64
    n: usize,
    // (hash, full 64-bit second hash, id) for every key whose 64-bit hash collides with another
    // key's, sorted; almost always empty. Keys here have tail ids [m, n) and no slot in the table
    // above. The second hash is stored untruncated regardless of fp_bits, so keys sharing the
    // collided hash are told apart with 64 fresh bits, not fp_bits of them.
    side: Vec<(u64, u64, u32)>,
}

impl CompactHashIndex {
    /// Build from a collection of strings, storing `fingerprint_bytes` (1, 2, or 4) per key —
    /// byte-granular sugar for [`build_bits`](Self::build_bits) with `8 × fingerprint_bytes`.
    pub fn build<I, S>(items: I, fingerprint_bytes: usize) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !matches!(fingerprint_bytes, 1 | 2 | 4) {
            return Err(IndexError::Format(
                "compact-hash: fingerprint_bytes must be 1, 2, or 4",
            ));
        }
        Self::build_bits(items, fingerprint_bytes as u32 * 8)
    }

    /// Build storing exactly `fingerprint_bits` (1..=64) per key. Fewer bits ⇒ smaller index but a
    /// higher false-positive rate on membership: `2^-fingerprint_bits` by construction (6.25% at 4
    /// bits, ≈ 0.4% at 8, ≈ 0.0015% at 16; measured 6.2530% at 4 bits over 2 M non-member probes).
    /// That rate describes *random* non-members — both hashes are deterministic and unseeded, so it
    /// is not a defence against an adversary who chooses the queries. Duplicates are removed; ids
    /// are arbitrary dense slots in `[0, n)` with no defined order, but they are **reproducible**:
    /// the same key set always produces the same blob, byte for byte, whatever the machine's thread
    /// count — so a blob is a comparable artefact and rebuilding one is not a renumbering.
    ///
    /// The build **streams**: only a `(hash, second hash)` pair — 16 bytes — is kept per key, never
    /// the strings, so building from a lazy iterator costs the same whatever the keys weigh. Those
    /// pairs are the peak, all but the perfect hash's own construction, which is fed the
    /// representatives straight from them, and a chunk buffer per thread, a few megabytes each.
    /// The measured high-water mark, on top of whatever holds the keys, is **21.5 bytes per key**
    /// at n = 10 M with the 8-bit default; at 2 M, where the buffers still show, 25.7, and 25.4 at
    /// 16 bits, 29.9 at 32 (`examples/peak.rs`, real-word bigrams). Both hashes are kept at their full 64 bits here
    /// regardless of `fingerprint_bits` (the width only governs what the fingerprint *table*
    /// stores), so the one thing the build cannot tell from a duplicate is two *distinct* keys
    /// colliding in **both**
    /// 64-bit hashes at once — `≈ 2^-128` per pair, negligible at any reachable scale. Keys
    /// colliding in the slot hash alone are served exactly, from a side table keyed by the full
    /// second hash.
    pub fn build_bits<I, S>(items: I, fingerprint_bits: u32) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Rejected before a single item is pulled: the iterator may be huge, or endless, and
        // consuming it only to report a bad width would be a hang the caller cannot see into.
        check_fingerprint_bits(fingerprint_bits)?;
        let pairs = items.into_iter().map(|k| hash_pair(k.as_ref())).collect();
        Self::build_from_pairs(pairs, fingerprint_bits)
    }

    /// [`build_bits`](Self::build_bits) after the hashing pass: `pairs` holds
    /// `(hash_key, fingerprint_full)` per key, in any order. Split out so the Python binding can
    /// hash items one at a time while it still holds the GIL and hand over only the 16-byte pairs.
    pub(crate) fn build_from_pairs(
        mut pairs: Vec<(u64, u64)>,
        fingerprint_bits: u32,
    ) -> Result<Self, IndexError> {
        check_fingerprint_bits(fingerprint_bits)?;
        pairs.sort_unstable();
        // Duplicate keys produce identical pairs. Distinct keys deduplicate here only by colliding
        // in both full 64-bit hashes at once — never because the fingerprint table is narrow.
        pairs.dedup();
        let n = pairs.len();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "compact-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        if n == 0 {
            return Ok(Self {
                mph: None,
                fps: SharedBytes::from_owned(Vec::new()),
                fp_bits: fingerprint_bits,
                n: 0,
                side: Vec::new(),
            });
        }
        // One representative per distinct hash value builds the MPH and owns the slot; the (almost
        // always zero) same-hash leftovers get tail ids [m, n) in the side table. The perfect
        // hash is fed the representatives straight from the sorted pairs, a chunk at a time, as
        // the file build feeds it from its merged file — the same table, byte for byte — so the
        // pairs are the only thing per key beside its construction: no list of the
        // representatives' hashes, no staged fingerprints.
        let mut side: Vec<(u64, u64, u32)> = Vec::new();
        let mut m = 0usize;
        for run in pairs.chunk_by(|a, b| a.0 == b.0) {
            m += 1;
            for &(h, fp) in &run[1..] {
                side.push((h, fp, 0)); // ids assigned once m is known
            }
        }
        for (j, e) in side.iter_mut().enumerate() {
            e.2 = (m + j) as u32;
        }
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let mut reps = pairs.chunk_by(|a, b| a.0 == b.0).map(|run| run[0].0);
        let mph = Mphf::build_from_sorted(m as u64, &mut reps, threads)?;
        let mut fps = vec![0u8; fp_table_len(m, fingerprint_bits)?];
        // One bit per slot, not one byte: this only has to catch a construction that was not
        // minimal/perfect, and at 100 M keys a `Vec<bool>` would be 100 MB of the peak.
        let mut seen = vec![0u64; m.div_ceil(64)];
        for run in pairs.chunk_by(|a, b| a.0 == b.0) {
            let (h, fp) = run[0];
            let slot = mph.index(h) as usize;
            if slot >= m || seen[slot / 64] >> (slot % 64) & 1 == 1 {
                return Err(IndexError::Format(
                    "compact-hash: construction was not minimal/perfect",
                ));
            }
            seen[slot / 64] |= 1 << (slot % 64);
            write_fp(
                &mut fps,
                slot,
                fingerprint_bits,
                fp & fp_mask(fingerprint_bits),
            );
        }
        drop(pairs);
        Ok(Self {
            mph: Some(mph),
            fps: SharedBytes::from_owned(fps),
            fp_bits: fingerprint_bits,
            n,
            side,
        })
    }

    /// Build straight to a file, from a source that need not fit in memory: the file
    /// [`build`](Self::build) and [`save`](Self::save) would have written, byte for byte, without
    /// ever holding the keys, their hashes or the fingerprint table whole. Returns the number of
    /// distinct keys written. `fingerprint_bytes` is [`build`](Self::build)'s;
    /// [`build_bits_to_file`](Self::build_bits_to_file) takes a width in bits.
    ///
    /// One pass over `items` hashes each key to its 16-byte pair and drops the string; every
    /// 256 MiB of pairs is sorted, deduplicated and spilled as a run beside the output, and the
    /// runs are merged into one sorted file. The perfect hash is built from that file one
    /// first-level chunk at a time — the table `build` gives the same keys, since both feed the
    /// same placement — and a second read writes every fingerprint at its slot: into memory while
    /// the table is under 256 MiB (268 M keys at the default width), past that through range
    /// files of 16 M slots each, so that no byte of the output is ever written at a random
    /// offset. Peak memory is one run plus the perfect hash's own construction, whatever the key
    /// count; the transient disk beside the output is the distinct pairs twice, 32 bytes per key,
    /// and twelve more past the table budget, all removed on every exit path.
    ///
    /// The source is read once, so any iterator will do; unlike
    /// [`PerfectHashIndex::build_to_file`](crate::PerfectHashIndex::build_to_file) nothing is
    /// replayed, because nothing here depends on the slot order before the keys are hashed.
    pub fn build_to_file<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
        fingerprint_bytes: usize,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !matches!(fingerprint_bytes, 1 | 2 | 4) {
            return Err(IndexError::Format(
                "compact-hash: fingerprint_bytes must be 1, 2, or 4",
            ));
        }
        Self::build_bits_to_file(items, path, fingerprint_bytes as u32 * 8)
    }

    /// [`build_to_file`](Self::build_to_file) at exactly `fingerprint_bits` (1..=64) per key —
    /// [`build_bits`](Self::build_bits) written straight to `path`.
    pub fn build_bits_to_file<I, S>(
        items: I,
        path: impl AsRef<std::path::Path>,
        fingerprint_bits: u32,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build_to_file_checked(items, path, fingerprint_bits, || Ok(()))
    }

    /// [`build_bits_to_file`](Self::build_bits_to_file) with a last word from the caller, asked
    /// once the input has ended — before the merge, so a source that failed does not pay for one
    /// — and again **inside** the atomic write, before the rename that publishes the file. It
    /// exists for a source that cannot report failure through its iterator: the Python binding
    /// adapts an arbitrary iterable, and one that raises halfway simply stops.
    pub(crate) fn build_to_file_checked<I, S, C>(
        items: I,
        path: impl AsRef<std::path::Path>,
        fingerprint_bits: u32,
        check: C,
    ) -> Result<usize, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        C: FnMut() -> Result<(), IndexError>,
    {
        check_fingerprint_bits(fingerprint_bits)?;
        Self::build_to_file_with(
            items.into_iter().map(|s| hash_pair(s.as_ref())),
            path.as_ref(),
            fingerprint_bits,
            check,
            Budget::DEFAULT,
        )
    }

    /// The streaming build over hashed pairs, with its memory budget exposed.
    pub(crate) fn build_to_file_with(
        pairs: impl Iterator<Item = (u64, u64)>,
        path: &std::path::Path,
        fingerprint_bits: u32,
        mut check: impl FnMut() -> Result<(), IndexError>,
        budget: Budget,
    ) -> Result<usize, IndexError> {
        use std::io::{Seek, Write};
        check_fingerprint_bits(fingerprint_bits)?;
        debug_assert_eq!(budget.range_slots % 8, 0, "a range must start on a byte");

        // Pass one: hash, in runs sorted and spilled beside the output.
        let mut scratch = Scratch::beside(path);
        let cap = (budget.run_bytes / PAIR_BYTES).max(1);
        let mut run: Vec<(u64, u64)> = Vec::new();
        for pair in pairs {
            if run.capacity() == 0 {
                run.reserve_exact(cap);
            }
            run.push(pair);
            if run.len() == cap {
                scratch.spill(&mut run)?;
            }
        }
        check()?;
        let pairs = if scratch.runs == 0 {
            run.sort_unstable();
            run.dedup();
            Pairs::Memory(run)
        } else {
            if !run.is_empty() {
                scratch.spill(&mut run)?;
            }
            drop(run);
            scratch.merge()?
        };
        let (n, side_len) = pairs.counts();
        if n > u32::MAX as usize {
            return Err(IndexError::Format(
                "compact-hash: more than u32::MAX keys; ids are u32",
            ));
        }
        let m = n - side_len;

        // Pass two: the perfect hash over one hash per distinct value, a chunk at a time.
        let mph = if m == 0 {
            None
        } else {
            let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
            let mut reps = pairs.reps()?;
            let built = Mphf::build_from_sorted(m as u64, &mut reps, threads);
            if let Some(e) = reps.error.take() {
                return Err(e.into());
            }
            Some(built?)
        };

        // Pass three: every representative's fingerprint at its slot — into the table when it
        // fits the budget, else into the range file its slot falls in — and the rest of each
        // equal-hash run into the side table, with the tail ids `build` gives them.
        let mask = fp_mask(fingerprint_bits);
        let table_len = fp_table_len(m, fingerprint_bits)?;
        let in_memory = table_len <= budget.fp_memory;
        let ranges = if in_memory {
            0
        } else {
            m.div_ceil(budget.range_slots)
        };
        let mut fps = vec![0u8; if in_memory { table_len } else { 0 }];
        let mut range_files = Vec::with_capacity(ranges);
        for i in 0..ranges {
            range_files.push(scratch.create(&format!("r{i}"))?);
        }
        let mut side: Vec<(u64, u64, u32)> = Vec::with_capacity(side_len);
        let mut last: Option<u64> = None;
        pairs.scan(|(h, second)| {
            if last == Some(h) {
                side.push((h, second, (m + side.len()) as u32));
                return Ok(());
            }
            last = Some(h);
            let slot = mph
                .as_ref()
                .expect("a representative exists only when m > 0")
                .index(h) as usize;
            let fp = second & mask;
            if in_memory {
                write_fp(&mut fps, slot, fingerprint_bits, fp);
            } else {
                let w = &mut range_files[slot / budget.range_slots];
                w.write_all(&(slot as u32).to_le_bytes())?;
                w.write_all(&fp.to_le_bytes())?;
            }
            Ok(())
        })?;
        for w in &mut range_files {
            w.flush()?;
        }
        drop(range_files);
        debug_assert_eq!(side.len(), side_len);
        drop(pairs);

        let mph_len = mph.as_ref().map_or(0, Mphf::byte_len);
        let mut side_buf = Vec::with_capacity(side.len() * SIDE_ENTRY);
        for &(h, fp, id) in &side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&fp.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        // The header carries the payload's hash, so it goes in last, over the space left for it.
        crate::blob::write_atomically_with(path, |w| {
            let mut payload = crate::blob::BlockHasher::new();
            w.write_all(&[0u8; HEADER_V6])?;
            // Straight from the table: at 10⁹ keys its blob is a quarter of a gigabyte, and a
            // copy of it here would be the build's peak.
            if let Some(mph) = &mph {
                mph.write_into(&mut Hashed(w, &mut payload))?;
            }
            if in_memory {
                w.write_all(&fps)?;
                payload.update(&fps);
            } else {
                for i in 0..ranges {
                    let base = i * budget.range_slots;
                    let slots = budget.range_slots.min(m - base);
                    let mut segment = vec![0u8; fp_table_len(slots, fingerprint_bits)?];
                    let mut records =
                        Records::<SLOT_BYTES>::open(&scratch.path(&format!("r{i}"))?)?;
                    while let Some(rec) = records.next()? {
                        let slot = u32::from_le_bytes(rec[0..4].try_into().expect("4 bytes"));
                        let fp = u64::from_le_bytes(rec[4..12].try_into().expect("8 bytes"));
                        write_fp(&mut segment, slot as usize - base, fingerprint_bits, fp);
                    }
                    w.write_all(&segment)?;
                    payload.update(&segment);
                }
            }
            w.write_all(&side_buf)?;
            payload.update(&side_buf);
            check()?;
            let header = header_v6(n, fingerprint_bits, mph_len, side.len(), payload.finish());
            w.flush()?;
            w.get_mut().seek(std::io::SeekFrom::Start(0))?;
            w.write_all(&header)?;
            Ok(())
        })?;
        Ok(n)
    }

    /// Ids of keys whose 64-bit hash collides with another key's, matched by the **full** 64-bit
    /// second hash — off the hot path: it runs only when the table is non-empty. Entries under one
    /// hash carry pairwise-distinct second hashes (the build deduplicates on the pair), so the
    /// match is unambiguous, and it must run *before* the fingerprint table is consulted: a side
    /// key's truncated fingerprint may tie its representative's, and the table would then claim
    /// the query for the representative's id. Entries are sorted.
    #[cold]
    fn side_lookup(&self, h: u64, fp: u64) -> Option<u32> {
        let start = self.side.partition_point(|e| e.0 < h);
        self.side[start..]
            .iter()
            .take_while(|e| e.0 == h)
            .find_map(|e| (e.1 == fp).then_some(e.2))
    }

    /// Slot for a key hash; `None` only for an empty index. The MPH's remap covers every slot it
    /// can produce, so the answer is always a real fingerprint row and membership is decided by the
    /// fingerprint alone.
    #[inline]
    fn slot_for(&self, h: u64) -> Option<usize> {
        Some(self.mph.as_ref()?.index(h) as usize)
    }

    /// Width of the stored fingerprints in bits; the membership false-positive rate is
    /// `2^-fingerprint_bits`.
    pub fn fingerprint_bits(&self) -> u32 {
        self.fp_bits
    }

    /// Number of distinct keys.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Whether the dictionary has no keys.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Dense id of `key`, or `None`. Membership is checked against the stored fingerprint, so a `Some`
    /// result is correct except for a `2^-fingerprint_bits` false-positive chance on a non-member.
    pub fn id(&self, key: &str) -> Option<u32> {
        if self.side.is_empty() {
            // The overwhelming case (no hash collision anywhere in the index): one predicted
            // branch, then exactly the side-free lookup. Both hashes come from a single pass over
            // the key — a hit needs both, and only a non-member landing past the remap (rarer than
            // 1 in 100 queries) pays for a fingerprint it never compares.
            let (h, full) = hash_pair(key);
            let slot = self.slot_for(h)?;
            return (read_fp(self.fps.as_ref(), slot, self.fp_bits)?
                == full & fp_mask(self.fp_bits))
            .then_some(slot as u32);
        }
        self.id_with_side(key)
    }

    /// [`id`](Self::id) for an index that contains at least one hash collision: the side probe
    /// runs first (it is exact on the full second hash, and the truncated table could otherwise
    /// answer for a side key whose fingerprint bits tie its representative's), then the ordinary
    /// slot-and-fingerprint path.
    #[cold]
    fn id_with_side(&self, key: &str) -> Option<u32> {
        let (h, full) = hash_pair(key);
        if let Some(id) = self.side_lookup(h, full) {
            return Some(id);
        }
        let slot = self.slot_for(h)?;
        (read_fp(self.fps.as_ref(), slot, self.fp_bits)? == full & fp_mask(self.fp_bits))
            .then_some(slot as u32)
    }

    /// Dense id **without** checking the fingerprint — `key` must be a member, or the result is an
    /// arbitrary valid slot. The fastest lookup for a closed vocabulary. Returns `0` when empty, and
    /// for a non-member whose slot falls past the MPH's remap (which is bounded rather than read
    /// unchecked — being unsafe on a wrong key is not one of the trade-offs this method makes). In
    /// the rare index that contains a 64-bit hash collision, keys sharing the collided hash resolve
    /// through the side table (matched by the full second hash — exact for members even there);
    /// every other index skips that with one predictable branch.
    #[inline]
    pub fn id_unchecked(&self, key: &str) -> u32 {
        let h = hash_key(key);
        if !self.side.is_empty() {
            if let Some(id) = self.side_lookup(h, fingerprint_full(key)) {
                return id;
            }
        }
        self.slot_for(h).unwrap_or(0) as u32
    }

    /// Batched [`id`](Self::id): one call for many keys, aligned with the input (`None` where
    /// the fingerprint rejects). Three prefetch pipelines run ahead of three dependent misses —
    /// the *key bytes* before hashing, the MPH's own stream (32 queries in flight), and the
    /// fingerprint line before the compare — so what a one-at-a-time loop serialises, this
    /// overlaps.
    ///
    /// **How much that wins depends on where the caller's keys live**, and the honest number says
    /// so. Real-word bigrams at 10 M, against the per-key loop: **2.9×** when the batch's strings
    /// are scattered in memory (a `Vec<String>` accumulated over time, keys pulled out of a map,
    /// anything not allocated in probe order), holding at 3.0× at 1 M and 2.8× at 5 M. When they
    /// are contiguous the hardware prefetcher already sees them coming, the batch runs at
    /// ~50 ns/key either way, and this method's advantage is the one FFI crossing rather than the
    /// prefetch. Non-members do not change the picture — a 50/50 batch measures the same, because
    /// the cost being hidden is reaching the key at all, not what is done with it.
    ///
    /// The rare index holding a hash collision takes the per-key path instead — the side probe must
    /// precede the fingerprint compare, which defeats the batched layout.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u32>> {
        let Some(mph) = &self.mph else {
            return vec![None; keys.len()];
        };
        if !self.side.is_empty() {
            return keys.iter().map(|k| self.id_with_side(k.as_ref())).collect();
        }
        // Both hashes are computed in one pass over the keys, so the verify pass below never
        // touches the strings again — it is a pure fingerprint-table compare with the lines
        // prefetched ahead.
        // Prefetch distance, shared by the hashing pass and the fingerprint compare below: far
        // enough ahead that a DRAM miss has time to land, near enough that the line is still there.
        const AHEAD: usize = 32;
        let mut hashes = Vec::with_capacity(keys.len());
        let mut wanted = Vec::with_capacity(keys.len());
        let mask = fp_mask(self.fp_bits);
        for (i, k) in keys.iter().enumerate() {
            // The slice holds the `String` headers contiguously, but their bytes are wherever the
            // allocator put them, so hashing a batch is one dependent cache miss per key and the
            // hashes cannot start until each arrives. Pulling a later key's first line in now is
            // the one prefetch the per-key `id` cannot make — it has no next key to look at.
            if let Some(next) = keys.get(i + AHEAD) {
                crate::blob::prefetch_byte(next.as_ref().as_bytes(), 0);
            }
            let (h, full) = hash_pair(k.as_ref());
            hashes.push(h);
            wanted.push(full & mask);
        }
        // Every slot is a real fingerprint row — the MPH's remap covers its whole slot range —
        // so the two passes are a straight pipeline: pilots prefetched inside `index_all`, then
        // the fingerprint line prefetched ahead of the compare.
        let slots = mph.index_all(&hashes);
        let fps = self.fps.as_ref();
        (0..keys.len())
            .map(|i| {
                if let Some(&s) = slots.get(i + AHEAD) {
                    crate::blob::prefetch_byte(fps, (s * self.fp_bits as u64 / 8) as usize);
                }
                let slot = slots[i] as usize;
                read_fp(fps, slot, self.fp_bits)
                    .and_then(|f| (f == wanted[i]).then_some(slot as u32))
            })
            .collect()
    }

    /// Whether `key` is present (subject to the fingerprint false-positive rate).
    pub fn contains(&self, key: &str) -> bool {
        self.id(key).is_some()
    }

    /// Serialised header + owned sections (the fingerprint table is borrowed separately): shared by
    /// [`to_bytes`](Self::to_bytes) and the streaming [`save`](Self::save) so the two emit
    /// byte-identical blobs.
    fn serialised_parts(&self) -> Result<SerialisedParts, IndexError> {
        let mph_buf = match &self.mph {
            Some(mph) => mph.to_bytes(),
            None => Vec::new(),
        };
        let mut side_buf = Vec::with_capacity(self.side.len() * SIDE_ENTRY);
        for &(h, fp, id) in &self.side {
            side_buf.extend_from_slice(&h.to_le_bytes());
            side_buf.extend_from_slice(&fp.to_le_bytes());
            side_buf.extend_from_slice(&id.to_le_bytes());
        }
        let mut payload = crate::blob::BlockHasher::new();
        payload.update(&mph_buf);
        payload.update(self.fps.as_ref());
        payload.update(&side_buf);
        let header = header_v6(
            self.n,
            self.fp_bits,
            mph_buf.len(),
            self.side.len(),
            payload.finish(),
        );
        Ok((header, mph_buf, side_buf))
    }

    /// Serialise to `[magic "BCH6"][n u64][fp_bits u32][mph_len u64][side_len u32][payload u64]
    /// [check u32][MPH blob][bit-packed fingerprints][side entries]`. `check` is a hash of the
    /// preceding header bytes and `payload` a streaming hash of everything after it, verified on
    /// owned loads; the MPH region carries its own header and validates its own lengths, which is
    /// what makes [`from_bytes`](Self::from_bytes) a safe fn even though this index stores no keys
    /// to check an answer against.
    pub fn to_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        let fp = self.fps.as_ref();
        let mut out = Vec::with_capacity(HEADER_V6 + mph_buf.len() + fp.len() + side_buf.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&mph_buf);
        out.extend_from_slice(fp);
        out.extend_from_slice(&side_buf);
        Ok(out)
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it — for sizing
    /// a buffer or reporting bytes/key; [`save`](Self::save) writes exactly this many.
    pub fn serialized_len(&self) -> Result<usize, IndexError> {
        let mph = match &self.mph {
            Some(mph) => mph.byte_len(),
            None => 0,
        };
        Ok(HEADER_V6 + mph + self.fps.len() + self.side.len() * SIDE_ENTRY)
    }

    /// Reconstruct from [`CompactHashIndex::to_bytes`] output (copies the blob into owned memory).
    ///
    /// Safe on arbitrary bytes. Every array the index will read is bounded by a length this crate
    /// wrote and checks here — the header, the fingerprint table, the side ids and the MPH's own
    /// header alike — so a crafted blob is at worst *wrong*, never unsound. This index stores no
    /// keys, so "wrong" means it answers with ids for a table it did not build; owned loads verify
    /// a streaming checksum of the whole payload, which is what turns accidental corruption into a
    /// clean error rather than a wrong answer.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(bytes.to_vec()), true)
    }

    /// The lexindex framing of `blob`, parsed and bounds-validated — magic, header checksum, the
    /// payload checksum when `verify` (owned loads; off for mmap so mapping stays proportional to
    /// the MPH alone), lengths, the side table and the fingerprint range — with the MPH region
    /// located but **not** deserialised. Safe on arbitrary bytes: this is the half a property test
    /// fuzzes, and everything `from_shared` trusts comes out of here.
    /// Whether the framing of `bytes` parses. Exists for the libFuzzer target in `fuzz/`, which
    /// lives in its own crate and so cannot reach `parse_frame` (private, and returning a private
    /// type). See the `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_parse_frame(bytes: &[u8], verify: bool) -> bool {
        Self::parse_frame(&SharedBytes::from_owned(bytes.to_vec()), verify).is_ok()
    }

    fn parse_frame(blob: &SharedBytes, verify: bool) -> Result<Frame, IndexError> {
        let bytes = blob.as_ref();
        if bytes.len() >= 4
            && LEGACY_MAGICS.contains(&<&[u8; 4]>::try_from(&bytes[0..4]).expect("4 bytes"))
        {
            return Err(IndexError::Format(
                "compact-hash: blob written by lexindex < 1.0, whose minimal perfect hash came \
                 from a crate this version no longer links; the keys are not stored, so it cannot \
                 be converted - rebuild the index from its keys",
            ));
        }
        if bytes.len() < HEADER_V6 || &bytes[0..4] != MAGIC_V6 {
            return Err(IndexError::Format("bad magic or truncated header"));
        }
        let check = u32::from_le_bytes(bytes[CHECKED_V6..HEADER_V6].try_into().unwrap());
        if check != crate::blob::hash_bytes(&bytes[..CHECKED_V6]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        // Owned loads verify the whole payload — one streaming pass over everything after the
        // header — so a flipped byte in the MPH region, the fingerprint table or the side table is
        // rejected here rather than perturbing answers later.
        if verify {
            let stored = u64::from_le_bytes(bytes[28..36].try_into().unwrap());
            if stored != crate::blob::hash_block(&bytes[HEADER_V6..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
        }
        let header = HEADER_V6;
        let n64 = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
        if n64 > u32::MAX as u64 {
            return Err(IndexError::Format(
                "compact-hash: header claims more than u32::MAX keys",
            ));
        }
        let n = n64 as usize;
        let fp_bits = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if !(1..=64).contains(&fp_bits) {
            return Err(IndexError::Format("compact-hash: bad fingerprint width"));
        }
        // `mph_len` and the side-byte count are header-supplied; convert and multiply checked so a
        // fabricated length fails cleanly on every target width instead of truncating or wrapping
        // on a 32-bit one.
        let mph_len = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().unwrap()))
            .map_err(|_| IndexError::Format("mph length out of range"))?;
        let side_len = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
        if side_len > n || (side_len == n && n > 0) {
            return Err(IndexError::Format("side table length out of range"));
        }
        let m = n - side_len;
        let side_bytes = side_len
            .checked_mul(SIDE_ENTRY)
            .ok_or(IndexError::Format("side table length out of range"))?;
        let side_start = bytes
            .len()
            .checked_sub(side_bytes)
            .ok_or(IndexError::Format("side table length out of range"))?;
        let mph_end = header
            .checked_add(mph_len)
            .filter(|&e| e <= side_start)
            .ok_or(IndexError::Format("mph length out of range"))?;
        let mut side: Vec<(u64, u64, u32)> = bytes[side_start..]
            .chunks_exact(SIDE_ENTRY)
            .map(|e| {
                (
                    u64::from_le_bytes(e[0..8].try_into().unwrap()),
                    u64::from_le_bytes(e[8..16].try_into().unwrap()),
                    u32::from_le_bytes(e[16..20].try_into().unwrap()),
                )
            })
            .collect();
        side.sort_unstable(); // restore the binary-search invariant regardless of the blob
        // Side ids must be exactly the tail range [m, n): `id()` hands them out verbatim, so an
        // unvalidated blob could otherwise answer with an id at or past `len()`. Checked
        // structurally, not via the checksums — those only vouch for transport, not construction.
        let mut ids: Vec<u32> = side.iter().map(|e| e.2).collect();
        ids.sort_unstable();
        if !ids.iter().copied().eq(m as u32..n as u32) {
            return Err(IndexError::Format(
                "compact-hash: side-table ids are not the tail id range",
            ));
        }
        let fps = blob
            .subslice(mph_end, side_start)
            .ok_or(IndexError::Format("fingerprint range out of range"))?;
        // `n` is untrusted (read from the header), so guard the multiply — a fabricated huge `n` would
        // otherwise overflow and panic in a debug build instead of failing cleanly.
        let expected = (m as u64)
            .checked_mul(fp_bits as u64)
            .map(|bits| bits.div_ceil(8))
            .ok_or(IndexError::Format(
                "compact-hash: fingerprint length mismatch",
            ))?;
        if expected != fps.len() as u64 {
            return Err(IndexError::Format(
                "compact-hash: fingerprint length mismatch",
            ));
        }
        Ok(Frame {
            n,
            fp_bits,
            mph: header..mph_end,
            fps,
            side,
        })
    }

    /// Reconstruct from a shared source: the validated framing from
    /// [`parse_frame`](Self::parse_frame), then the MPH copied into memory; the fingerprint table
    /// (the bulk) is borrowed zero-copy, so `load_mmap` never copies it.
    fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let frame = Self::parse_frame(&blob, verify)?;
        let m = frame.n - frame.side.len();
        let mph = if frame.n == 0 {
            None
        } else {
            let mph = Mphf::from_bytes(&blob.as_ref()[frame.mph])?;
            if mph.n() != m as u64 {
                return Err(IndexError::Format("mph / header length mismatch"));
            }
            Some(mph)
        };
        Ok(Self {
            mph,
            fps: frame.fps,
            fp_bits: frame.fp_bits,
            n: frame.n,
            side: frame.side,
        })
    }

    /// Write the dictionary to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section, so saving peaks at the index's own memory plus the small MPH buffer
    /// rather than a full serialised copy.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    /// The [`to_bytes`](Self::to_bytes) blob streamed into `w`: the header, the hash and the side
    /// table from their serialised parts, the fingerprints from where they are.
    pub(crate) fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        let (header, mph_buf, side_buf) = self.serialised_parts()?;
        w.write_all(&header)?;
        w.write_all(&mph_buf)?;
        w.write_all(self.fps.as_ref())?;
        w.write_all(&side_buf)?;
        Ok(())
    }

    /// Load a dictionary previously written with [`CompactHashIndex::save`] (reads the whole file
    /// and verifies the payload checksum). Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and borrow the fingerprint table zero-copy (only the small MPH is read
    /// into memory). Skips the payload-checksum scan `load` performs — the mapped file is trusted
    /// intact.
    ///
    /// # Safety
    /// One obligation, and it is not about the bytes: the file must not be modified or truncated by
    /// any process while the returned index is alive, because the index borrows the mapping. A
    /// crafted file is *not* undefined behaviour here — the same validation
    /// [`from_bytes`](Self::from_bytes) performs runs on the mapping — it is merely wrong. See
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }

    /// [`load_mmap`](Self::load_mmap) plus the payload checksum [`load`](Self::load) makes: one
    /// pass over the mapping at load, pages still shared and the bulk still borrowed. For a file
    /// you wrote but did not carry yourself.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap): the file must not change while the
    /// index is alive. The checksum is computed once, at load, and says nothing about later.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_verified(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), true)
    }
}

/// The `BCH6` header over its scalars and the payload hash, checksummed.
fn header_v6(
    n: usize,
    fp_bits: u32,
    mph_len: usize,
    side_len: usize,
    payload: u64,
) -> [u8; HEADER_V6] {
    let mut header = [0u8; HEADER_V6];
    header[0..4].copy_from_slice(MAGIC_V6);
    header[4..12].copy_from_slice(&(n as u64).to_le_bytes());
    header[12..16].copy_from_slice(&fp_bits.to_le_bytes());
    header[16..24].copy_from_slice(&(mph_len as u64).to_le_bytes());
    header[24..28].copy_from_slice(&(side_len as u32).to_le_bytes());
    header[28..36].copy_from_slice(&payload.to_le_bytes());
    let check = crate::blob::hash_bytes(&header[..CHECKED_V6]) as u32;
    header[CHECKED_V6..].copy_from_slice(&check.to_le_bytes());
    header
}

fn check_fingerprint_bits(bits: u32) -> Result<(), IndexError> {
    if (1..=64).contains(&bits) {
        Ok(())
    } else {
        Err(IndexError::Format(
            "compact-hash: fingerprint_bits must be in 1..=64",
        ))
    }
}

/// Bytes a bit-packed table of `count` fingerprints of `bits` bits occupies. `count ≤ u32::MAX`
/// and `bits ≤ 64`, so the product fits a `u64`; whether the table fits *this platform's* address
/// space is answered here rather than by a truncating cast on a 32-bit target.
fn fp_table_len(count: usize, bits: u32) -> Result<usize, IndexError> {
    usize::try_from((count as u64 * bits as u64).div_ceil(8))
        .map_err(|_| IndexError::Format("compact-hash: fingerprint table too large"))
}

#[inline(always)]
fn fp_mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// Fingerprint of `slot` from the bit-packed table, or `None` if the table is too short.
#[inline(always)]
fn read_fp(bytes: &[u8], slot: usize, bits: u32) -> Option<u64> {
    let bitpos = (slot as u64).checked_mul(bits as u64)?;
    let byte = (bitpos / 8) as usize;
    let off = (bitpos % 8) as u32;
    if let Some(chunk) = bytes.get(byte..byte + 8) {
        let w = u64::from_le_bytes(chunk.try_into().unwrap());
        let v = if off + bits <= 64 {
            w >> off
        } else {
            // off ≥ 1 here (bits ≤ 64), so the shift below is < 64
            (w >> off) | ((*bytes.get(byte + 8)? as u64) << (64 - off))
        };
        Some(v & fp_mask(bits))
    } else {
        // Within 8 bytes of the table's end (≤ 7 bytes available from `byte`): accumulate the
        // covering bytes without reading past the end. Only the last few slots ever land here.
        let last = ((bitpos + bits as u64 - 1) / 8) as usize;
        let mut v: u64 = 0;
        for (j, i) in (byte..=last).enumerate() {
            v |= (*bytes.get(i)? as u64) << (8 * j as u32);
        }
        Some((v >> off) & fp_mask(bits))
    }
}

/// Write fingerprint `fp` (already masked to `bits`) for `slot` into the zeroed bit-packed table.
#[inline]
fn write_fp(fps: &mut [u8], slot: usize, bits: u32, fp: u64) {
    if bits % 8 == 0 {
        // Byte-aligned widths (including the 8-bit default) take a straight copy: the generic
        // OR-in loop below costs a measurable ~2.5% of build time at 1 M keys.
        let k = (bits / 8) as usize;
        let start = slot * k;
        fps[start..start + k].copy_from_slice(&fp.to_le_bytes()[..k]);
        return;
    }
    let bitpos = slot as u64 * bits as u64;
    let byte = (bitpos / 8) as usize;
    let off = (bitpos % 8) as u32;
    // Bytes of `fp << off` past the fingerprint's own span are zero, so skipping the ones that
    // fall past the table's end drops nothing.
    for (j, &b) in (fp << off).to_le_bytes().iter().enumerate() {
        if let Some(dst) = fps.get_mut(byte + j) {
            *dst |= b;
        }
    }
    if off > 0 && off + bits > 64 {
        fps[byte + 8] |= (fp >> (64 - off)) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand: the loader is safe now, and every blob below is either produced by this crate
    /// or a deliberate corruption of one.
    fn from_bytes(bytes: &[u8]) -> Result<CompactHashIndex, IndexError> {
        CompactHashIndex::from_bytes(bytes)
    }

    /// The safe half of the loader must never panic on arbitrary bytes — only `Ok`/`Err` — which
    /// is where the "garbage fails cleanly" property lives now that the loaders are `unsafe`.
    #[test]
    fn parse_frame_never_panics() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(&prop::collection::vec(any::<u8>(), 0..256), |data| {
                let _ = CompactHashIndex::parse_frame(&SharedBytes::from_owned(data), true);
                Ok(())
            })
            .unwrap();
    }

    /// Every truncation of a real blob is rejected by the framing alone, with or without the
    /// payload checksum — so the MPH region is never reached on a short read.
    #[test]
    fn parse_frame_rejects_every_truncation() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let blob = idx.to_bytes().unwrap();
        for verify in [true, false] {
            for k in 0..blob.len() {
                let cut = SharedBytes::from_owned(blob[..k].to_vec());
                assert!(
                    CompactHashIndex::parse_frame(&cut, verify).is_err(),
                    "truncated to {k} bytes (verify={verify}) parsed"
                );
            }
            assert!(
                CompactHashIndex::parse_frame(&SharedBytes::from_owned(blob.clone()), verify)
                    .is_ok()
            );
        }
    }

    #[test]
    fn serialized_len_matches_to_bytes() {
        for keys in [vec![], vec!["alpha"], vec!["alpha", "beta", "gamma"]] {
            let idx = CompactHashIndex::build(&keys, 1).unwrap();
            assert_eq!(idx.serialized_len().unwrap(), idx.to_bytes().unwrap().len());
        }
    }

    #[test]
    fn build_lookup_and_membership() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma", "delta", "alpha"], 2).unwrap();
        assert_eq!(idx.len(), 4);
        assert!(!idx.is_empty());
        let mut ids = Vec::new();
        for w in ["alpha", "beta", "gamma", "delta"] {
            let id = idx.id(w).expect("present");
            assert!((id as usize) < idx.len());
            assert_eq!(idx.id_unchecked(w), id);
            assert!(idx.contains(w));
            ids.push(id);
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4); // dense bijection onto [0, n)
        assert_eq!(idx.id("epsilon"), None);
    }

    #[test]
    fn false_positive_rate_is_bounded() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = CompactHashIndex::build(&members, 2).unwrap();
        for m in &members {
            assert!(idx.contains(m)); // no false negatives, ever
        }
        let fp = (0..20_000)
            .filter(|i| idx.id(&format!("stranger-{i:06}")).is_some())
            .count();
        // 2-byte fingerprint ⇒ ~1/65536 per non-member; comfortably a handful over 20k probes.
        assert!(
            fp < 50,
            "false positives {fp}/20000 too high for a 2-byte fingerprint"
        );
    }

    /// Batch equals singular on every probe, members and misses alike; a fingerprint false
    /// positive would show up as a batch/singular disagreement, not just a wrong answer.
    #[test]
    fn batch_matches_singular_including_misses() {
        let keys: Vec<String> = (0..3_000).map(|i| format!("k{i}")).collect();
        let idx = CompactHashIndex::build(&keys, 2).unwrap();
        let probes: Vec<String> = keys
            .iter()
            .cloned()
            .chain((0..500).map(|i| format!("miss{i}")))
            .collect();
        assert_eq!(
            idx.ids_of(&probes),
            probes.iter().map(|p| idx.id(p)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn much_smaller_than_perfect_hash_index() {
        let words: Vec<String> = (0..5_000).map(|i| format!("token-{i:05}")).collect();
        let compact = CompactHashIndex::build(&words, 1)
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        let exact = crate::PerfectHashIndex::build(&words)
            .unwrap()
            .to_bytes()
            .unwrap()
            .len();
        assert!(compact * 3 < exact, "compact {compact} vs exact {exact}");
    }

    #[test]
    fn round_trips_and_rejects_corrupt() {
        let idx = CompactHashIndex::build(["GET", "POST", "PUT", "DELETE"], 2).unwrap();
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        for w in ["GET", "POST", "PUT", "DELETE"] {
            assert_eq!(restored.id(w), idx.id(w));
        }
        assert_eq!(restored.id("PATCH"), None);
        assert!(from_bytes(b"nope").is_err());
        assert!(CompactHashIndex::build(["a"], 3).is_err()); // bad fingerprint width
    }

    #[test]
    fn from_bytes_rejects_bad_width_and_length() {
        let good = CompactHashIndex::build(["a", "bb", "ccc"], 2)
            .unwrap()
            .to_bytes()
            .unwrap();

        // The width field (u32 at bytes 12..16) outside 1..=64 bits is rejected...
        for w in [0u8, 65] {
            let mut bad_width = good.clone();
            bad_width[12] = w;
            assert!(matches!(from_bytes(&bad_width), Err(IndexError::Format(_))));
        }
        // Dropping a byte makes the table length disagree with ceil(n * fp_bits / 8).
        assert!(matches!(
            from_bytes(&good[..good.len() - 1]),
            Err(IndexError::Format(_))
        ));
    }

    /// This index stores no keys, so a pre-1.0 blob cannot even be converted — the refusal has to
    /// say so, and say which lexindex wrote it, rather than report a bad magic on an intact file.
    #[test]
    fn a_pre_1_0_blob_is_refused_by_name() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let good = idx.to_bytes().unwrap();
        assert_eq!(&good[0..4], b"BCH6");
        for magic in LEGACY_MAGICS {
            let mut old = good.clone();
            old[0..4].copy_from_slice(magic);
            let err = match from_bytes(&old) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("{} was accepted", std::str::from_utf8(magic).unwrap()),
            };
            assert!(err.contains("lexindex < 1.0"), "{err}");
            assert!(err.contains("rebuild"), "{err}");
        }
    }

    /// A header that lost bytes in transit must be refused rather than used to frame sections, and
    /// so must a flipped byte anywhere in the payload, caught by the whole-payload checksum on
    /// owned loads.
    #[test]
    fn corrupt_headers_and_payloads_are_refused() {
        let idx = CompactHashIndex::build(["alpha", "beta", "gamma"], 1).unwrap();
        let good = idx.to_bytes().unwrap();
        assert!(from_bytes(&good).is_ok());
        for pos in [4, 12, 15, 16, 23, 24, 27, 28, 35, 36, 39] {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "header byte {pos} was accepted");
        }
        for pos in (HEADER_V6..good.len()).step_by(5) {
            let mut bad = good.clone();
            bad[pos] ^= 0x40;
            assert!(from_bytes(&bad).is_err(), "payload byte {pos} was accepted");
        }
    }

    /// A real 64-bit hash collision (the pinned pair from `crate::hash`) must build and resolve
    /// both keys — the fingerprint stands in for the stored key in the side probe. Whatever the
    /// width, no member may ever be a false negative.
    #[test]
    fn colliding_keys_build_and_resolve_by_fingerprint() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys: Vec<String> = (0..500).map(|i| format!("filler-{i:03}")).collect();
        keys.push(a.to_string());
        keys.push(b.to_string());
        let idx = CompactHashIndex::build_bits(&keys, 64).unwrap();
        assert_eq!(idx.len(), 502);
        assert_eq!(idx.side.len(), 1);
        let (ia, ib) = (idx.id(a).unwrap(), idx.id(b).unwrap());
        assert_ne!(ia, ib);
        assert_eq!(idx.id_unchecked(a), ia);
        assert_eq!(idx.id_unchecked(b), ib);
        assert_eq!(idx.ids_of(&[a, b]), vec![Some(ia), Some(ib)]);
        let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k).unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            keys.len(),
            "ids must stay a bijection onto [0, n)"
        );
        let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
        assert_eq!(restored.side, idx.side);
        assert_eq!(restored.id(a), Some(ia));
        assert_eq!(restored.id(b), Some(ib));
        // The side table stores the full second hash, so the table width must never decide whether
        // the pair stays two keys: the pinned pair's low fingerprint bits tie at 1 bit, which the
        // 0.8.0 truncated side table silently merged into one id.
        for bits in [1u32, 2, 4, 8, 16] {
            let idx = CompactHashIndex::build_bits(&keys, bits).unwrap();
            assert_eq!(idx.len(), 502, "bits={bits}");
            assert_eq!(idx.side.len(), 1, "bits={bits}");
            let (ia, ib) = (idx.id(a).unwrap(), idx.id(b).unwrap());
            assert_ne!(ia, ib, "bits={bits}");
            assert_eq!(idx.id_unchecked(a), ia, "bits={bits}");
            assert_eq!(idx.id_unchecked(b), ib, "bits={bits}");
            assert_eq!(idx.ids_of(&[a, b]), vec![Some(ia), Some(ib)], "bits={bits}");
            for k in &keys {
                assert!(idx.contains(k), "false negative on {k:?} at {bits} bits");
            }
            let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
            assert_eq!(restored.id(a), Some(ia), "bits={bits}");
            assert_eq!(restored.id(b), Some(ib), "bits={bits}");
        }
    }

    /// A bad fingerprint width is rejected before the iterator is touched — it may be endless, and
    /// hashing it to completion just to report the width would hang instead of returning `Err`.
    #[test]
    fn a_bad_fingerprint_width_is_rejected_before_the_iterator_runs() {
        let mut pulled = 0usize;
        let items = std::iter::repeat_with(|| {
            pulled += 1;
            "x"
        })
        .take(1_000);
        assert!(CompactHashIndex::build_bits(items, 0).is_err());
        assert_eq!(pulled, 0, "the iterator must not be consumed");
    }

    /// Side ids are handed out verbatim by `id()`, so the loader must pin them to the tail range
    /// [m, n) structurally — the checksums vouch for transport, not for what was written. A blob
    /// with a re-checksummed out-of-range or duplicate id is refused, never served.
    #[test]
    fn tampered_side_ids_are_refused_even_with_valid_checksums() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let idx = CompactHashIndex::build([a, b, "filler"], 1).unwrap();
        assert_eq!(idx.side.len(), 1);
        let good = idx.to_bytes().unwrap();
        // The lone side entry's id lives in the blob's last 4 bytes.
        for bad_id in [0u32, 1, 3, u32::MAX] {
            let mut bad = good.clone();
            let at = bad.len() - 4;
            bad[at..].copy_from_slice(&bad_id.to_le_bytes());
            let payload = crate::blob::hash_block(&bad[HEADER_V6..]);
            bad[28..36].copy_from_slice(&payload.to_le_bytes());
            let check = crate::blob::hash_bytes(&bad[..CHECKED_V6]) as u32;
            bad[CHECKED_V6..HEADER_V6].copy_from_slice(&check.to_le_bytes());
            let err = match from_bytes(&bad) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("side id {bad_id} was accepted"),
            };
            assert!(err.contains("side-table ids"), "{err}");
        }
    }

    /// `save` streams sections instead of assembling one buffer; the file must still be
    /// byte-identical to `to_bytes`.
    #[test]
    fn save_streams_the_same_bytes_as_to_bytes() {
        let words: Vec<String> = (0..500).map(|i| format!("w{i}")).collect();
        let idx = CompactHashIndex::build(&words, 2).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_chstream_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), idx.to_bytes().unwrap());
        std::fs::remove_file(&path).ok();
    }

    /// `id_unchecked` skips the fingerprint comparison, not the remap bound.
    #[test]
    fn id_unchecked_is_bounded_for_strangers() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        for round in 0..60 {
            let idx = CompactHashIndex::build(&members, 1).unwrap();
            for probe in 0..2_000 {
                let s = format!("stranger-{round}-{probe}");
                assert!((idx.id_unchecked(&s) as usize) < idx.len());
            }
        }
    }

    /// A non-member is a valid input to a minimal perfect hash — it simply lands on some other
    /// key's slot. Every path must stay inside the fingerprint table for a stranger, and the two
    /// verifying paths must agree, at a rate the 16-bit fingerprint makes almost always `None`.
    #[test]
    fn strangers_land_on_a_real_row_and_are_rejected() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = CompactHashIndex::build_bits(&members, 16).unwrap();
        let strangers: Vec<String> = (0..20_000).map(|i| format!("stranger-{i:05}")).collect();
        let batch = idx.ids_of(&strangers);
        let mut accepted = 0;
        for (s, b) in strangers.iter().zip(&batch) {
            assert!((idx.id_unchecked(s) as usize) < idx.len());
            assert_eq!(idx.id(s), *b, "the batch must agree with the per-key path");
            accepted += usize::from(b.is_some());
        }
        // 20 000 probes at 2^-16 expect 0.3 false positives; 10 is far outside any plausible run.
        assert!(accepted <= 10, "{accepted} of 20 000 strangers accepted");
    }

    /// Every width round-trips through build, serde and the batch path; the fingerprints for the
    /// last slots sit within 8 bytes of the table's end, covering the tail read path.
    #[test]
    fn sub_byte_and_odd_widths_round_trip() {
        let keys: Vec<String> = (0..300).map(|i| format!("key-{i:03}")).collect();
        for bits in [1u32, 3, 4, 6, 8, 12, 33, 64] {
            let idx = CompactHashIndex::build_bits(&keys, bits).unwrap();
            assert_eq!(idx.fingerprint_bits(), bits);
            let mut ids: Vec<u32> = keys.iter().map(|k| idx.id(k).expect("member")).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), keys.len(), "bits={bits}: ids not dense");
            let restored = from_bytes(&idx.to_bytes().unwrap()).unwrap();
            assert_eq!(restored.fingerprint_bits(), bits);
            let probes: Vec<String> = keys
                .iter()
                .cloned()
                .chain((0..100).map(|i| format!("miss-{i}")))
                .collect();
            let singular: Vec<Option<u32>> = probes.iter().map(|p| idx.id(p)).collect();
            assert_eq!(restored.ids_of(&probes), singular, "bits={bits}");
            assert_eq!(idx.ids_of(&probes), singular, "bits={bits}");
        }
        assert!(CompactHashIndex::build_bits(&keys, 0).is_err());
        assert!(CompactHashIndex::build_bits(&keys, 65).is_err());
    }

    /// The packed table read back equals the reference: every fingerprint extracted at every
    /// width, against a naive bit-by-bit reader.
    #[test]
    fn packed_table_matches_naive_reference() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::default();
        runner
            .run(
                &(1u32..=64, prop::collection::vec(any::<u64>(), 1..50)),
                |(bits, raw)| {
                    let masked: Vec<u64> = raw.iter().map(|f| f & fp_mask(bits)).collect();
                    let n = masked.len();
                    let mut table = vec![0u8; (n as u64 * bits as u64).div_ceil(8) as usize];
                    for (i, &f) in masked.iter().enumerate() {
                        write_fp(&mut table, i, bits, f);
                    }
                    for (i, &f) in masked.iter().enumerate() {
                        prop_assert_eq!(
                            read_fp(&table, i, bits),
                            Some(f),
                            "slot {} bits {}",
                            i,
                            bits
                        );
                        let mut naive: u64 = 0;
                        for b in 0..bits as u64 {
                            let pos = i as u64 * bits as u64 + b;
                            let bit = (table[(pos / 8) as usize] >> (pos % 8)) & 1;
                            naive |= (bit as u64) << b;
                        }
                        prop_assert_eq!(naive, f);
                    }
                    Ok(())
                },
            )
            .unwrap();
    }

    /// The same promise `PerfectHashIndex` makes, at a scale a property test cannot afford: the
    /// same keys give the same blob byte for byte, so a rebuild is not a renumbering and two blobs
    /// can be compared to tell whether a corpus changed. The fingerprint width is part of the
    /// input, so it is varied rather than left at the default.
    #[test]
    fn the_same_keys_always_produce_the_same_blob() {
        let words: Vec<String> = (0..50_000).map(|i| format!("word-{i:05}")).collect();
        for bits in [1u32, 8, 17] {
            let first = CompactHashIndex::build_bits(&words, bits)
                .unwrap()
                .to_bytes()
                .unwrap();
            let again = CompactHashIndex::build_bits(&words, bits)
                .unwrap()
                .to_bytes()
                .unwrap();
            assert_eq!(first, again, "{bits} fingerprint bits");
        }
    }

    /// At 4 bits the advertised false-positive rate is 2^-4 = 6.25%; check it statistically
    /// (20 000 probes ⇒ expect 1 250, σ ≈ 34; the bound below is ≈ +7σ, far outside noise).
    #[test]
    fn four_bit_false_positive_rate_is_bounded() {
        let members: Vec<String> = (0..2_000).map(|i| format!("member-{i:05}")).collect();
        let idx = CompactHashIndex::build_bits(&members, 4).unwrap();
        for m in &members {
            assert!(idx.contains(m));
        }
        let fp = (0..20_000)
            .filter(|i| idx.id(&format!("stranger-{i:06}")).is_some())
            .count();
        assert!(
            fp < 1_500,
            "false positives {fp}/20000 too high for a 4-bit fingerprint (expect ~1250)"
        );
    }

    #[test]
    fn from_bytes_rejects_overflowing_n_without_panicking() {
        // A fabricated huge `n` in the header (with the MPH region left intact) must fail cleanly, not
        // overflow `n * fp_bytes` — which would panic in a debug build.
        let mut blob = CompactHashIndex::build(["a", "bb", "ccc"], 4)
            .unwrap()
            .to_bytes()
            .unwrap();
        blob[11] ^= 0x40; // n: 3 -> 2^62, so `n * 4` would wrap u64
        assert!(matches!(from_bytes(&blob), Err(IndexError::Format(_))));
    }

    /// `[0, 0)` has no inhabitant, so the MPH has no table and `Mphf::index` would panic on one.
    /// Every query path has to notice that before it asks — including the batch, which is the one
    /// that allocates an answer per key.
    #[test]
    fn empty_round_trips() {
        let empty = CompactHashIndex::build_bits(Vec::<String>::new(), 4).unwrap();
        assert_eq!(empty.fingerprint_bits(), 4);
        assert!(empty.is_empty() && empty.id("x").is_none());
        let empty = CompactHashIndex::build(Vec::<String>::new(), 1).unwrap();
        assert!(empty.is_empty() && empty.id("x").is_none() && empty.id_unchecked("x") == 0);
        assert_eq!(empty.ids_of(&["x", "y"]), vec![None, None]);
        let restored = from_bytes(&empty.to_bytes().unwrap()).unwrap();
        assert!(restored.is_empty());
        assert_eq!(restored.ids_of(&["x"]), vec![None]);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let idx = CompactHashIndex::build(["a", "b", "c"], 1).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_ch_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        assert_eq!(CompactHashIndex::load(&path).unwrap().id("b"), idx.id("b"));
        std::fs::remove_file(&path).ok();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lexindex_chf_{}_{name}", std::process::id()))
    }

    /// Nothing of the build is left beside its output.
    fn no_scratch_beside(path: &std::path::Path) {
        let stem = path.file_name().unwrap().to_string_lossy().into_owned();
        let left: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&stem) && name != &stem)
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    /// Byte for byte the blob `build` and `save` write, whichever way the budget routes it: one
    /// run in memory or many spilled and merged, the fingerprint table in memory or through
    /// range files, at five widths, over keys with duplicates and a real 64-bit hash collision.
    #[test]
    fn build_to_file_writes_the_blob_build_would_have() {
        let (a, b) = crate::hash::COLLIDING_PAIR;
        let mut keys: Vec<String> = (0..30_000).map(|i| format!("key-{}", i % 20_000)).collect();
        keys.push(a.to_string());
        keys.push(b.to_string());
        let budgets = [
            Budget::DEFAULT,
            Budget {
                run_bytes: PAIR_BYTES * 1000,
                fp_memory: 0,
                range_slots: 1000,
            },
            Budget {
                run_bytes: PAIR_BYTES * 4096,
                fp_memory: usize::MAX,
                range_slots: 8,
            },
            Budget {
                run_bytes: PAIR_BYTES * 100_000,
                fp_memory: 0,
                range_slots: 8,
            },
        ];
        for bits in [1u32, 4, 8, 16, 33] {
            let expected = CompactHashIndex::build_bits(&keys, bits)
                .unwrap()
                .to_bytes()
                .unwrap();
            for (i, budget) in budgets.iter().enumerate() {
                let path = tmp(&format!("blob-{bits}-{i}.bch"));
                let n = CompactHashIndex::build_to_file_with(
                    keys.iter().map(|k| hash_pair(k)),
                    &path,
                    bits,
                    || Ok(()),
                    *budget,
                )
                .unwrap();
                assert_eq!(n, 20_002, "bits {bits}, budget {i}");
                let written = std::fs::read(&path).unwrap();
                assert!(written == expected, "bits {bits}, budget {i}");
                no_scratch_beside(&path);
                std::fs::remove_file(&path).ok();
            }
        }
    }

    /// Enough keys for the first level to be several chunks, fed from the merged file: the
    /// chunk feed's cutting at bucket boundaries is what this exercises, against `build`.
    #[test]
    fn build_to_file_streams_the_perfect_hash_from_the_merged_file() {
        let keys: Vec<String> = (0..200_000).map(|i| format!("k{i}")).collect();
        let expected = CompactHashIndex::build(&keys, 1)
            .unwrap()
            .to_bytes()
            .unwrap();
        let path = tmp("chunks.bch");
        let n = CompactHashIndex::build_to_file_with(
            keys.iter().map(|k| hash_pair(k)),
            &path,
            8,
            || Ok(()),
            Budget {
                run_bytes: PAIR_BYTES * 50_000,
                fp_memory: 0,
                range_slots: 65_536,
            },
        )
        .unwrap();
        assert_eq!(n, keys.len());
        assert!(std::fs::read(&path).unwrap() == expected);
        let back = CompactHashIndex::load(&path).unwrap();
        assert!(keys.iter().all(|k| back.contains(k)));
        no_scratch_beside(&path);
        std::fs::remove_file(&path).ok();
    }

    /// The public forms: bytes and bits, and a width refused before the source is touched.
    #[test]
    fn build_to_file_public_forms_match_build() {
        let keys = ["alpha", "beta", "gamma", "beta"];
        let path = tmp("public.bch");
        assert_eq!(CompactHashIndex::build_to_file(keys, &path, 2).unwrap(), 3);
        assert!(
            std::fs::read(&path).unwrap()
                == CompactHashIndex::build(keys, 2)
                    .unwrap()
                    .to_bytes()
                    .unwrap()
        );
        assert_eq!(
            CompactHashIndex::build_bits_to_file(keys, &path, 5).unwrap(),
            3
        );
        assert!(
            std::fs::read(&path).unwrap()
                == CompactHashIndex::build_bits(keys, 5)
                    .unwrap()
                    .to_bytes()
                    .unwrap()
        );
        std::fs::remove_file(&path).ok();
        let never = std::iter::from_fn(|| -> Option<&str> { panic!("the source was read") });
        assert!(CompactHashIndex::build_to_file(never, &path, 3).is_err());
        let never = std::iter::from_fn(|| -> Option<&str> { panic!("the source was read") });
        assert!(CompactHashIndex::build_bits_to_file(never, &path, 0).is_err());
        assert!(!path.exists());
        let empty = tmp("empty.bch");
        assert_eq!(
            CompactHashIndex::build_to_file(Vec::<String>::new(), &empty, 1).unwrap(),
            0
        );
        assert!(
            std::fs::read(&empty).unwrap()
                == CompactHashIndex::build(Vec::<String>::new(), 1)
                    .unwrap()
                    .to_bytes()
                    .unwrap()
        );
        std::fs::remove_file(&empty).ok();
    }

    /// A failing check aborts before the merge and, asked again inside the write, before the
    /// rename: the target is untouched either way and the scratch is gone.
    #[test]
    fn build_to_file_aborts_on_the_check_and_leaves_nothing() {
        let keys: Vec<String> = (0..3000).map(|i| format!("k{i}")).collect();
        let path = tmp("aborted.bch");
        std::fs::write(&path, b"previous").unwrap();
        let spilled = Budget {
            run_bytes: PAIR_BYTES * 1000,
            fp_memory: 0,
            range_slots: 1000,
        };
        let mut asked = 0;
        let err = CompactHashIndex::build_to_file_with(
            keys.iter().map(|k| hash_pair(k)),
            &path,
            8,
            || {
                asked += 1;
                Err(IndexError::Format("the source raised"))
            },
            spilled,
        )
        .unwrap_err();
        assert!(err.to_string().contains("the source raised"));
        assert_eq!(asked, 1, "refused before the merge");
        let mut asked = 0;
        let err = CompactHashIndex::build_to_file_with(
            keys.iter().map(|k| hash_pair(k)),
            &path,
            8,
            || {
                asked += 1;
                if asked == 2 {
                    Err(IndexError::Format("the source raised late"))
                } else {
                    Ok(())
                }
            },
            spilled,
        )
        .unwrap_err();
        assert!(err.to_string().contains("raised late"));
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        no_scratch_beside(&path);
        std::fs::remove_file(&path).ok();
    }

    /// `load_mmap` skips the payload checksum by design; `load_mmap_verified` is the same mapping
    /// with it, so a flipped payload byte the plain mapping serves is refused.
    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_verified_refuses_a_flipped_payload_byte_the_plain_mapping_takes() {
        let idx = CompactHashIndex::build(["GET", "POST", "PUT", "DELETE"], 2).unwrap();
        let path = std::env::temp_dir().join(format!("lexindex_mmapv_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        // SAFETY: this test owns the file and nothing writes to it while a map is alive.
        let mapped = unsafe { CompactHashIndex::load_mmap_verified(&path) }.unwrap();
        assert_eq!(mapped.id("POST"), idx.id("POST"));
        drop(mapped);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        std::fs::write(&path, &bytes).unwrap();
        assert!(unsafe { CompactHashIndex::load_mmap(&path) }.is_ok());
        assert!(unsafe { CompactHashIndex::load_mmap_verified(&path) }.is_err());
        assert!(CompactHashIndex::load(&path).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[cfg(feature = "mmap")]
    #[test]
    fn load_mmap_matches_owned() {
        let words: Vec<String> = (0..128).map(|i| format!("k{i:03}")).collect();
        let idx = CompactHashIndex::build(&words, 2).unwrap();
        let path =
            std::env::temp_dir().join(format!("lexindex_ch_mmap_{}.bch", std::process::id()));
        idx.save(&path).unwrap();
        let mapped = unsafe { CompactHashIndex::load_mmap(&path) }.unwrap();
        assert_eq!(mapped.len(), idx.len());
        for w in &words {
            assert_eq!(mapped.id(w), idx.id(w));
        }
        assert!(!mapped.contains("k999"));
        std::fs::remove_file(&path).ok();
    }
}
