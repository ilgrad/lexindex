//! External sort of a key stream: runs sorted and deduplicated in memory, spilled beside the
//! output, merged back as one ascending stream. Shared by the streaming builders, which cannot hold
//! the corpus and cannot assume it arrives in order.

use crate::IndexError;

/// Key bytes one run holds before it is sorted and spilled. `pub` only so that the public docs of
/// the builders that name it resolve; the module is private, so it is not reachable from outside.
pub const RUN_BYTES: usize = 256 << 20;

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
    count: usize,
}

impl Runs {
    pub(crate) fn beside(target: &std::path::Path) -> Self {
        Self {
            beside: target.to_path_buf(),
            dir: None,
            count: 0,
        }
    }

    /// Whether anything has been spilled — false when the corpus fitted in one run, which is the
    /// case the streaming builders can answer straight from memory.
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
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
        use std::io::Write;
        let name = self.count.to_string();
        let path = self.dir()?.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, file);
        // The count goes first so the reader knows a run's end from a truncated key.
        w.write_all(&[0u8; 8])?;
        let mut n = 0u64;
        for key in keys {
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
        self.count += 1;
        Ok(())
    }

    /// Every run as one ascending stream; equal keys from different runs come out adjacent, for
    /// the sorted builder to drop. A read error is parked in `failed` and ends the stream.
    pub(crate) fn merge<'a>(
        &self,
        failed: &'a std::cell::RefCell<Option<std::io::Error>>,
    ) -> Result<impl Iterator<Item = String> + 'a, IndexError> {
        use std::cmp::Reverse;
        let dir = self.dir.as_deref().expect("a run was spilled");
        let mut readers = Vec::with_capacity(self.count);
        let mut heap = std::collections::BinaryHeap::with_capacity(self.count);
        for i in 0..self.count {
            let mut reader = RunReader::open(&dir.join(i.to_string()))?;
            if let Some(key) = reader.next()? {
                heap.push(Reverse((key, i)));
            }
            readers.push(reader);
        }
        Ok(std::iter::from_fn(move || {
            let Reverse((key, i)) = heap.pop()?;
            match readers[i].next() {
                Ok(Some(next)) => heap.push(Reverse((next, i))),
                Ok(None) => {}
                Err(e) => {
                    *failed.borrow_mut() = Some(e);
                    heap.clear();
                }
            }
            Some(key)
        }))
    }
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
