//! External sort of a key stream: runs sorted and deduplicated in memory, spilled beside the
//! output, merged back as one ascending stream. Shared by the streaming builders, which cannot hold
//! the corpus and cannot assume it arrives in order.

use crate::IndexError;

/// Key bytes one run holds before it is sorted and spilled. `pub` only so that the public docs of
/// the builders that name it resolve; the module is private, so it is not reachable from outside.
pub const RUN_BYTES: usize = 256 << 20;

/// Readers one merge opens at once, and so the whole resident cost of merging: a file descriptor
/// and a 1 MiB buffer each. Above this the runs are collapsed in groups first, which costs one
/// extra pass over the spilled bytes per factor of 128 and buys a merge whose footprint does not
/// follow the corpus. A build spills a run per [`RUN_BYTES`], so the collapse starts at 32 GiB of
/// keys — and without it 2 000 runs would want 2 000 descriptors, past a 1 024 soft limit wherever
/// one is in force, and 2 GiB of read buffers inside a builder whose point is not to hold the
/// corpus.
const FAN_IN: usize = 128;

/// A fan-in of one carries every group forward unchanged, so the collapse would never end.
const _: () = assert!(FAN_IN >= 2);

#[cfg(test)]
thread_local! {
    /// Runs a merge may open while a test is running; zero is the real rule. Two collapse rounds
    /// need more than `FAN_IN²` runs, which is not a number a unit test can spill.
    static FAN_IN_OVERRIDE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Runs a merge may open, for a test that wants a collapse without spilling `FAN_IN²` of them.
/// Zero restores the real rule.
#[cfg(test)]
pub(crate) fn set_fan_in(runs: usize) {
    assert!(runs == 0 || runs >= 2, "a fan-in below two never collapses");
    FAN_IN_OVERRIDE.with(|c| c.set(runs));
}

/// [`FAN_IN`], or what a test asked for.
fn fan_in() -> usize {
    #[cfg(test)]
    {
        let n = FAN_IN_OVERRIDE.with(std::cell::Cell::get);
        if n > 0 {
            return n;
        }
    }
    FAN_IN
}

/// Process-wide counter in the runs directory's name, so two builds in one process aimed at the
/// same path do not share one.
static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One run: an arena of key bytes and a span per key, so a run
/// costs its bytes plus eight per key rather than a `String` and an allocation each. Both are
/// reserved once, at the first key, to the budget — the pages are only touched as they fill.
pub(crate) struct Run {
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
    budget: usize,
}

impl Run {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            bytes: Vec::new(),
            spans: Vec::new(),
            budget,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Whether `key` still fits: its bytes in the arena and its span in the table.
    pub(crate) fn fits(&self, key: &str) -> bool {
        self.bytes.len() + key.len() <= self.budget && self.spans.len() < self.budget / 8
    }

    pub(crate) fn push(&mut self, key: &str) {
        if self.bytes.capacity() == 0 {
            self.bytes.reserve_exact(self.budget);
            self.spans.reserve_exact(self.budget / 8);
        }
        // `fits` bounded the arena by `budget`, and the budget is a `usize` the caller chose;
        // `u32` spans hold a run up to 4 GiB, which `RUN_BYTES` is far below.
        self.spans.push((self.bytes.len() as u32, key.len() as u32));
        self.bytes.extend_from_slice(key.as_bytes());
    }

    /// The keys ascending and distinct, in place.
    pub(crate) fn sorted(&mut self) -> impl Iterator<Item = &str> {
        let bytes = &self.bytes;
        let key = |&(at, len): &(u32, u32)| &bytes[at as usize..(at + len) as usize];
        self.spans.sort_unstable_by(|a, b| key(a).cmp(key(b)));
        self.spans.dedup_by(|a, b| key(a) == key(b));
        self.spans
            .iter()
            .map(move |s| std::str::from_utf8(key(s)).expect("appended from a str"))
    }

    pub(crate) fn clear(&mut self) {
        self.bytes.clear();
        self.spans.clear();
    }
}

/// The spilled runs: a directory beside the output, one file per run — `[keys u64]`, then
/// `[len u32][bytes]` per key, ascending and distinct — removed when this is dropped, whichever
/// way the build ends. Nothing is created until the first spill, so a corpus that fits in one
/// run leaves no trace.
pub(crate) struct Runs {
    beside: std::path::PathBuf,
    dir: Option<std::path::PathBuf>,
    /// The runs still to be merged, by file name, in the order they were written. A collapse
    /// round replaces a group of these with the one run it merged them into.
    live: Vec<usize>,
    /// The next file name. Never reused, so a round cannot write over a run it is still reading.
    next: usize,
}

impl Runs {
    pub(crate) fn beside(target: &std::path::Path) -> Self {
        Self {
            beside: target.to_path_buf(),
            dir: None,
            live: Vec::new(),
            next: 0,
        }
    }

    /// Whether anything has been spilled — false when the corpus fitted in one run, which is the
    /// case the streaming builders can answer straight from memory.
    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    fn dir(&mut self) -> Result<&std::path::Path, IndexError> {
        if self.dir.is_none() {
            let mut dir = self.beside.clone();
            let seq = RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dir.as_mut_os_string()
                .push(format!(".{}.{seq}.runs", std::process::id()));
            // `create_dir`, not `create_dir_all`: a directory already at this pid-and-counter
            // name is someone else's, and an error rather than a place to write.
            std::fs::create_dir(&dir)?;
            self.dir = Some(dir);
        }
        Ok(self.dir.as_deref().expect("just created"))
    }

    pub(crate) fn spill<'a>(
        &mut self,
        keys: impl Iterator<Item = &'a str>,
    ) -> Result<(), IndexError> {
        let id = self.write_run(keys)?;
        self.live.push(id);
        Ok(())
    }

    /// Write one run file from an ascending key stream and return the name it took.
    fn write_run<S: AsRef<str>>(
        &mut self,
        keys: impl Iterator<Item = S>,
    ) -> Result<usize, IndexError> {
        use std::io::Write;
        let id = self.next;
        let path = self.dir()?.join(id.to_string());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, file);
        // The count goes first so the reader knows a run's end from a truncated key.
        w.write_all(&[0u8; 8])?;
        let mut n = 0u64;
        for key in keys {
            let key = key.as_ref();
            let len = u32::try_from(key.len())
                .map_err(|_| IndexError::Format("extsort: a key longer than 4 GiB"))?;
            w.write_all(&len.to_le_bytes())?;
            w.write_all(key.as_bytes())?;
            n += 1;
        }
        w.flush()?;
        let mut file = w.into_inner().map_err(|e| e.into_error())?;
        {
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&n.to_le_bytes())?;
        }
        self.next += 1;
        Ok(id)
    }

    /// Collapse the spilled runs until at most [`FAN_IN`] are left, so the merge that follows opens
    /// that many readers whatever the corpus was. Each round merges groups of `FAN_IN` into one run
    /// and deletes what it consumed, so the space this takes over the runs themselves is one group.
    ///
    /// A group of one is carried forward rather than copied, and below the threshold this does
    /// nothing at all — which is every build that spills less than `FAN_IN` runs. Idempotent.
    fn collapse(&mut self) -> Result<(), IndexError> {
        let fan_in = fan_in();
        while self.live.len() > fan_in {
            // The merge reads through this path and `write_run` wants `&mut self`, so the
            // directory is taken by value for the round rather than borrowed across it.
            let dir = self.dir.clone().expect("a run was spilled");
            let live = std::mem::take(&mut self.live);
            for group in live.chunks(fan_in) {
                let [only] = group else {
                    let failed = std::cell::RefCell::new(None);
                    let id = self.write_run(merge_runs(&dir, group, &failed)?)?;
                    if let Some(e) = failed.borrow_mut().take() {
                        return Err(IndexError::Io(e));
                    }
                    // Only once the merged run is whole on disk, so a failure above leaves every
                    // key still readable somewhere.
                    for consumed in group {
                        std::fs::remove_file(dir.join(consumed.to_string()))?;
                    }
                    self.live.push(id);
                    continue;
                };
                self.live.push(*only);
            }
        }
        Ok(())
    }

    /// Every run as one ascending stream; equal keys from different runs come out adjacent, for
    /// the sorted builder to drop. A read error is parked in `failed` and ends the stream.
    pub(crate) fn merge<'a>(
        &mut self,
        failed: &'a std::cell::RefCell<Option<std::io::Error>>,
    ) -> Result<impl Iterator<Item = String> + use<'a>, IndexError> {
        self.collapse()?;
        let dir = self.dir.as_deref().expect("a run was spilled");
        merge_runs(dir, &self.live, failed)
    }
}

/// The named runs in `dir` as one ascending stream. A read error is parked in `failed` and ends
/// the stream, since it cannot travel through `Iterator::next`.
fn merge_runs<'a>(
    dir: &std::path::Path,
    ids: &[usize],
    failed: &'a std::cell::RefCell<Option<std::io::Error>>,
) -> Result<impl Iterator<Item = String> + use<'a>, IndexError> {
    use std::cmp::Reverse;
    let mut readers = Vec::with_capacity(ids.len());
    let mut heap = std::collections::BinaryHeap::with_capacity(ids.len());
    for (slot, id) in ids.iter().enumerate() {
        let mut reader = RunReader::open(&dir.join(id.to_string()))?;
        if let Some(key) = reader.next()? {
            heap.push(Reverse((key, slot)));
        }
        readers.push(reader);
    }
    Ok(std::iter::from_fn(move || {
        let Reverse((key, slot)) = heap.pop()?;
        match readers[slot].next() {
            Ok(Some(next)) => heap.push(Reverse((next, slot))),
            Ok(None) => {}
            Err(e) => {
                *failed.borrow_mut() = Some(e);
                heap.clear();
            }
        }
        Some(key)
    }))
}

impl Drop for Runs {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

struct RunReader {
    reader: std::io::BufReader<std::fs::File>,
    left: u64,
}

impl RunReader {
    fn open(path: &std::path::Path) -> Result<Self, IndexError> {
        use std::io::Read;
        let mut reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut count = [0u8; 8];
        reader.read_exact(&mut count)?;
        Ok(Self {
            reader,
            left: u64::from_le_bytes(count),
        })
    }

    fn next(&mut self) -> std::io::Result<Option<String>> {
        use std::io::Read;
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let mut len = [0u8; 4];
        self.reader.read_exact(&mut len)?;
        let mut key = vec![0u8; u32::from_le_bytes(len) as usize];
        self.reader.read_exact(&mut key)?;
        // Written from a `str` by this process moments ago: anything else is the disk lying.
        String::from_utf8(key)
            .map(Some)
            .map_err(|_| std::io::Error::other("extsort: a run file is corrupt"))
    }
}

/// A sorted, distinct key stream a caller can walk more than once — either the single in-memory
/// [`Run`] the corpus fitted in, or a merge of the [`Runs`] it spilled. `DictIndex::build_to_file`
/// walks one three times; `plan_file` walks one once.
pub(crate) trait Replay {
    fn each(&mut self, f: &mut dyn FnMut(&str) -> Result<(), IndexError>)
    -> Result<(), IndexError>;
}

impl Replay for &mut Run {
    fn each(
        &mut self,
        f: &mut dyn FnMut(&str) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        for key in self.sorted() {
            f(key)?;
        }
        Ok(())
    }
}

impl Replay for &mut Runs {
    fn each(
        &mut self,
        f: &mut dyn FnMut(&str) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        // The merge interleaves the runs but deduplicates only within one, so equal keys arrive
        // adjacent; dropping them here is what makes this stream the one `build` sorts to.
        let failed = std::cell::RefCell::new(None);
        let mut prev = String::new();
        let mut seen = false;
        for key in self.merge(&failed)? {
            if seen && prev == key {
                continue;
            }
            f(&key)?;
            prev.clear();
            prev.push_str(&key);
            seen = true;
        }
        if let Some(e) = failed.borrow_mut().take() {
            return Err(IndexError::Io(e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run per key, so the merge has as many as the caller asks for without spilling anything
    /// like a real corpus.
    fn spill_one_per_key(runs: &mut Runs, keys: &[String]) {
        for key in keys {
            runs.spill(std::iter::once(key.as_str())).unwrap();
        }
    }

    fn drained(runs: &mut Runs) -> Vec<String> {
        let failed = std::cell::RefCell::new(None);
        let out: Vec<String> = runs.merge(&failed).unwrap().collect();
        assert!(failed.borrow().is_none(), "the merge reported a read error");
        out
    }

    fn tmpdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lexindex-extsort-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every key, in order, out of far more runs than a merge is allowed to open at once.
    #[test]
    fn a_merge_over_more_runs_than_it_may_open_yields_them_all_in_order() {
        FAN_IN_OVERRIDE.with(|c| c.set(4));
        let dir = tmpdir();
        let target = dir.join("out.bin");
        // 200 runs against a fan-in of four is four rounds, which the real 128 would need
        // 268 million runs to reach.
        let keys: Vec<String> = (0..200).map(|i| format!("key-{i:04}")).collect();
        let mut runs = Runs::beside(&target);
        spill_one_per_key(&mut runs, &keys);
        assert_eq!(runs.live.len(), 200);
        let out = drained(&mut runs);
        let mut want = keys.clone();
        want.sort_unstable();
        assert_eq!(out, want);
        // The collapse left the merge within its budget, and a second merge is the same stream.
        assert!(runs.live.len() <= 4, "{} runs survived", runs.live.len());
        assert_eq!(drained(&mut runs), want);
        drop(runs);
        FAN_IN_OVERRIDE.with(|c| c.set(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Duplicates across runs survive the collapse and arrive adjacent, which is the contract the
    /// builders deduplicate against.
    #[test]
    fn equal_keys_from_different_runs_stay_adjacent_through_a_collapse() {
        FAN_IN_OVERRIDE.with(|c| c.set(2));
        let dir = tmpdir();
        let target = dir.join("dupes.bin");
        let keys: Vec<String> = (0..40).map(|i| format!("k{:02}", i % 8)).collect();
        let mut runs = Runs::beside(&target);
        spill_one_per_key(&mut runs, &keys);
        let out = drained(&mut runs);
        assert_eq!(out.len(), keys.len());
        assert!(out.windows(2).all(|w| w[0] <= w[1]), "{out:?}");
        let mut distinct = out.clone();
        distinct.dedup();
        assert_eq!(distinct.len(), 8, "{out:?}");
        drop(runs);
        FAN_IN_OVERRIDE.with(|c| c.set(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The point of the collapse: what a merge holds open is the fan-in, not the corpus. Counted
    /// rather than argued from the code — and counted only over this test's own directory, since
    /// `/proc/self/fd` is the whole process and the suite runs in parallel.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_merge_holds_the_fan_in_open_and_not_a_reader_per_run() {
        let dir = tmpdir();
        let open_here = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|e| std::fs::read_link(e.unwrap().path()).ok())
                .filter(|target| target.starts_with(&dir))
                .count()
        };
        FAN_IN_OVERRIDE.with(|c| c.set(4));
        let target = dir.join("fds.bin");
        let keys: Vec<String> = (0..300).map(|i| format!("key-{i:04}")).collect();
        let mut runs = Runs::beside(&target);
        spill_one_per_key(&mut runs, &keys);
        assert_eq!(open_here(), 0, "a spill left a file open");
        let failed = std::cell::RefCell::new(None);
        let mut stream = runs.merge(&failed).unwrap();
        assert_eq!(stream.next().as_deref(), Some("key-0000"));
        let during = open_here();
        assert!(
            during <= 4,
            "the merge holds {during} run files open, past a fan-in of four"
        );
        drop(stream);
        assert_eq!(open_here(), 0, "the merge left a run file open");
        drop(runs);
        FAN_IN_OVERRIDE.with(|c| c.set(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The shipped fan-in rather than a test's: two thousand runs is the case the bound exists for,
    /// and before the collapse it would have opened two thousand files to merge them.
    #[test]
    fn two_thousand_runs_merge_at_the_shipped_fan_in() {
        let dir = tmpdir();
        let target = dir.join("many.bin");
        let keys: Vec<String> = (0..2_000).map(|i| format!("key-{i:04}")).collect();
        let mut runs = Runs::beside(&target);
        spill_one_per_key(&mut runs, &keys);
        assert_eq!(runs.live.len(), 2_000);
        let mut want = keys.clone();
        want.sort_unstable();
        assert_eq!(drained(&mut runs), want);
        assert!(
            runs.live.len() <= FAN_IN,
            "{} runs survived",
            runs.live.len()
        );
        drop(runs);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Below the threshold nothing is rewritten: the runs the caller spilled are the runs merged.
    #[test]
    fn a_handful_of_runs_is_merged_where_it_lies() {
        let dir = tmpdir();
        let target = dir.join("few.bin");
        let mut runs = Runs::beside(&target);
        spill_one_per_key(
            &mut runs,
            &["b".to_string(), "a".to_string(), "c".to_string()],
        );
        let names = runs.live.clone();
        assert_eq!(drained(&mut runs), ["a", "b", "c"]);
        assert_eq!(runs.live, names, "a small merge rewrote its runs");
        assert_eq!(runs.next, 3, "a small merge wrote a file it did not need");
        drop(runs);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
