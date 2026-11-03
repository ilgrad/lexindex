//! The public queries over a parsed index.

use super::*;

impl DictIndex {
    /// Rank of `key` if it is a member.
    pub fn id(&self, key: &str) -> Option<u64> {
        match self.locate_query(key) {
            (rank, true) => Some(rank),
            _ => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.locate_query(key).1
    }

    /// The rank of the first key not below `key`: `key`'s own id if it is a member, otherwise
    /// the id it would have, `len()` past every key. Two of these bound a range of keys as a
    /// range of ids.
    pub fn lower_bound(&self, key: &str) -> u64 {
        self.locate_query(key).0
    }

    /// The smallest `(key, id)` with `key >= query` (the *successor*), or `None` if every key is
    /// smaller.
    pub fn successor(&self, query: &str) -> Option<(String, u64)> {
        let rank = self.locate_query(query).0;
        self.key(rank).map(|k| (k, rank))
    }

    /// The largest `(key, id)` with `key <= query` (the *predecessor*), or `None` if every key is
    /// larger. A present `query` is its own predecessor; otherwise the answer sits one rank below
    /// the first key above it, ids being the sorted rank.
    pub fn predecessor(&self, query: &str) -> Option<(String, u64)> {
        let (rank, found) = self.locate_query(query);
        let at = if found { rank } else { rank.checked_sub(1)? };
        self.key(at).map(|k| (k, at))
    }

    /// How many keys satisfy `lo <= key < hi`, without decoding any of them.
    pub fn range_count(&self, lo: &str, hi: &str) -> u64 {
        self.lower_bound(hi).saturating_sub(self.lower_bound(lo))
    }

    /// The **contiguous** id range of the keys starting with `prefix`, half-open.
    ///
    /// Ids are the lexicographic rank and keys sharing a prefix are adjacent in that order, so
    /// every match is an id in one interval — a prefix is a slice of the id space, not a set of
    /// ids to test one at a time. Two order lookups, whatever the number of matches. Empty
    /// (`start == end`) when nothing matches; `0..len` for an empty prefix.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// assert_eq!(idx.prefix_id_range("ap"), 0..2);
    /// assert_eq!(idx.prefix_id_range("z"), 3..3);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn prefix_id_range(&self, prefix: &str) -> std::ops::Range<u64> {
        self.probe(prefix, |bytes, probe| {
            let start = match probe {
                Probe::Exact => self.locate(bytes).0,
                // A prefix no key can carry: the empty range where it would have started.
                Probe::Absent => {
                    let at = self.locate(bytes).0;
                    return at..at;
                }
                Probe::Last => return self.n as u64..self.n as u64,
            };
            // The exclusive end is the first key that does not carry the prefix: the same bytes
            // with the last one incremented. UTF-8 never ends on `0xff`, but a prefix in a code of
            // whole bytes can, and the carry is what takes it to the byte before.
            let mut upper = bytes.to_vec();
            let end = loop {
                match upper.pop() {
                    Some(0xff) => continue,
                    Some(b) => {
                        upper.push(b + 1);
                        break self.locate(&upper).0;
                    }
                    None => break self.n as u64,
                }
            };
            start..end.max(start)
        })
    }

    /// How many keys start with `prefix` — [`prefix_id_range`](Self::prefix_id_range)'s width.
    pub fn prefix_count(&self, prefix: &str) -> u64 {
        let r = self.prefix_id_range(prefix);
        r.end - r.start
    }

    /// Batched [`id`](Self::id): one answer per key, aligned with `keys`.
    pub fn ids_of<S: AsRef<str>>(&self, keys: &[S]) -> Vec<Option<u64>> {
        match self.code.as_deref() {
            None => self.ids_of_spelled(keys.len(), |i| keys[i].as_ref().as_bytes()),
            Some(code) => self.ids_of_coded(code, keys.len(), |i| Some(keys[i].as_ref())),
        }
    }

    /// [`ids_of`](Self::ids_of) over `n` keys given as bytes by position, for a caller whose keys
    /// are not `str`s — a lookup reading an Arrow buffer.
    ///
    /// A batch is not a loop here. A single lookup is a chain of dependent loads — the binary
    /// search over the block samples, then the block's head, then its data — and each one waits on
    /// the last, so a lookup into an index past the last-level cache spends most of its time
    /// stalled. Doing `LANES` of them in lockstep puts that many loads in flight at once: the
    /// binary searches advance one step for every lane before any lane takes its second, and the
    /// blocks the lanes landed on are prefetched whole before any lane is scanned.
    ///
    /// What that is worth is what is left stalled, so it grows with the index and shrinks with the
    /// block — a larger block leaves fewer samples to search and more bytes to scan, and only the
    /// search is fully hidden. Real word bigrams, half of the probes members, shuffled, a loop of
    /// [`id`](Self::id) as the control in the same process
    /// (`bench/results/quiet-ab-2026-10-28-arz-c18a6b1.txt`):
    ///
    /// | keys | block | index | `id` in a loop | `ids_of` |
    /// |---:|---:|---:|---:|---:|
    /// | 10 M | 32 | 29 MB | 598 ns | **430** |
    /// | 10 M | 128 | 26 MB | 543 | **410** |
    /// | 1 M | 32 | 2.2 MB | 330 | **292** |
    /// | 200 k | 32 | 0.4 MB | 207 | 237 |
    ///
    /// The last row is the point: an index that fits in cache has nothing to hide, and the batch
    /// is then the same work in a less obvious order, and slower for the lockstep's bookkeeping.
    #[cfg_attr(not(feature = "mph"), allow(dead_code))]
    pub(crate) fn ids_of_with<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        match self.code.as_deref() {
            None => self.ids_of_spelled(n, key),
            // Bytes that are not UTF-8 are no key.
            Some(code) => self.ids_of_coded(code, n, |i| std::str::from_utf8(key(i)).ok()),
        }
    }

    /// [`ids_of_with`](Self::ids_of_with) on a blob with a code, over keys that are `str`s where
    /// they are keys at all: `ids_of` hands over its own, which need no second look at their UTF-8.
    ///
    /// Each group of lanes is spelled first, into one arena the lanes then read several times. A
    /// key the code cannot spell, or that is not UTF-8, is searched as the empty probe and its
    /// answer dropped after. The arena grows by doubling and is never cut back, since a spelling
    /// writes every byte it keeps: growing it by each key's room and truncating it after was a
    /// `memset` call a key.
    fn ids_of_coded<'a, F: Fn(usize) -> Option<&'a str>>(
        &self,
        code: &CharCode,
        n: usize,
        key: F,
    ) -> Vec<Option<u64>> {
        let mut out = Vec::with_capacity(n);
        let mut arena = Vec::new();
        let mut spans = [(0, 0); LANES];
        let mut exact = [false; LANES];
        for base in (0..n).step_by(LANES) {
            let m = LANES.min(n - base);
            let mut at = 0;
            for j in 0..m {
                let start = at;
                let probe = match key(base + j) {
                    Some(k) => {
                        let room = at + 2 * k.len();
                        if arena.len() < room {
                            arena.resize(room.max(2 * arena.len()), 0);
                        }
                        let (probe, len) = code.probe_into(k, &mut arena[at..room]);
                        if probe == Probe::Exact {
                            at += len;
                        }
                        probe
                    }
                    None => Probe::Last,
                };
                spans[j] = (start, at);
                exact[j] = probe == Probe::Exact;
            }
            let first = out.len();
            self.spelled_into(m, |j| &arena[spans[j].0..spans[j].1], &mut out);
            for (id, &exact) in out[first..].iter_mut().zip(&exact[..m]) {
                if !exact {
                    *id = None;
                }
            }
        }
        out
    }

    /// [`ids_of_with`](Self::ids_of_with) over keys already spelled as the blob spells them.
    fn ids_of_spelled<'a, F: Fn(usize) -> &'a [u8]>(&self, n: usize, key: F) -> Vec<Option<u64>> {
        let mut out = Vec::with_capacity(n);
        self.spelled_into(n, key, &mut out);
        out
    }

    /// [`ids_of_spelled`](Self::ids_of_spelled), appending its answers to `out`.
    fn spelled_into<'a, F: Fn(usize) -> &'a [u8]>(
        &self,
        n: usize,
        key: F,
        out: &mut Vec<Option<u64>>,
    ) {
        if self.n == 0 {
            out.resize(out.len() + n, None);
            return;
        }
        let nb = self.blocks_len();
        // Enough rounds for the widest search: each one at least halves the remaining span.
        let rounds = usize::BITS as usize - nb.leading_zeros() as usize + 1;
        let mut s = [0u64; LANES];
        let (mut lo_at, mut lo_len) = ([0usize; LANES], [0usize; LANES]);
        let mut bound = [0usize; LANES];
        // The boundary of a lane the samples cannot place, and `usize::MAX` for one they can.
        let mut fixed = [0usize; LANES];
        for base in (0..n).step_by(LANES) {
            let m = LANES.min(n - base);
            for j in 0..m {
                match self.route(key(base + j)) {
                    Ok(sample) => {
                        s[j] = sample;
                        fixed[j] = usize::MAX;
                        (lo_at[j], lo_len[j]) = (0, nb);
                    }
                    Err(l) => {
                        fixed[j] = l;
                        (lo_at[j], lo_len[j]) = (0, 0);
                    }
                }
            }
            // `lo` is the first sample not below the probe's — the `partition_point` `locate`
            // makes, run for every lane at once. The first sample above it is not searched for
            // too: where no sample equals the probe's it is `lo`, so one compare says so, and a
            // second search would only repeat the first's loads.
            for _ in 0..rounds {
                for j in 0..m {
                    if lo_len[j] > 0 {
                        let half = lo_len[j] >> 1;
                        if self.samples[lo_at[j] + half] < s[j] {
                            lo_at[j] += half + 1;
                            lo_len[j] -= half + 1;
                        } else {
                            lo_len[j] = half;
                        }
                    }
                }
            }
            // As in `sample_boundary`: only a run of samples equal to the lane's has heads to
            // compare, and a run long enough to have a trie is placed by it, unmeasured.
            for j in 0..m {
                bound[j] = if fixed[j] != usize::MAX {
                    fixed[j]
                } else if self.samples.get(lo_at[j]) != Some(&s[j]) {
                    lo_at[j]
                } else {
                    let (lo, probe) = (lo_at[j], key(base + j));
                    let placed = (self.samples.get(lo + MIN_RUN - 1) == Some(&s[j]))
                        .then(|| self.tie_boundary(probe, lo))
                        .flatten();
                    match placed {
                        Some((b, _)) => b,
                        None => self.head_boundary(probe, lo, past_equal(&self.samples, lo, s[j])),
                    }
                };
            }
            // The block start is one load and the data it names another, so they are pulled in as
            // two passes rather than one: the second cannot be issued until the first has landed.
            for &b in bound.iter().take(m) {
                if b > 0 {
                    self.blocks.prefetch(b - 1);
                }
            }
            // A block's entries are contiguous, so the whole run is pulled in, not just its first
            // line: at 128 keys a block is several lines and the scan walks all of them.
            for &b in bound.iter().take(m) {
                if b > 0 {
                    let at = self.block_start(b - 1) as usize;
                    let end = if b < nb {
                        self.block_start(b) as usize
                    } else {
                        self.data.len()
                    };
                    for line in (at..end.min(at + 8 * 64)).step_by(64) {
                        crate::blob::prefetch_byte(&self.data, line);
                    }
                }
            }
            for (j, &b) in bound.iter().enumerate().take(m) {
                out.push(match self.locate_in(b, key(base + j)) {
                    (rank, true) => Some(rank),
                    _ => None,
                });
            }
        }
    }

    /// Decode the next entry of a block onto `cur`, which holds the previous one, moving `at`
    /// past it; `false` on data this crate did not write.
    #[inline]
    fn advance(&self, codec: &Codec, entries: &mut Entries<'_>, cur: &mut Vec<u8>) -> bool {
        let Some((l, len)) = entries.head() else {
            return false;
        };
        let piece = entries.piece(len);
        if l > cur.len() || piece.units(codec.unit()) != len {
            return false;
        }
        cur.truncate(l);
        codec.decode_into(&self.phrases, piece, cur, usize::MAX)
    }

    /// Take `out`, which holds a front-coded run's first key, `steps` entries along that run.
    ///
    /// An entry stores what it shares with its predecessor, so an entry whose `lcp` is at least a
    /// later entry's contributes nothing that survives to the key being asked for. The entries that
    /// do contribute form a strictly increasing staircase of `lcp`, and a monotonic stack over the
    /// headers finds it in the one pass the walk already makes. Every header is still read — the
    /// lengths before an entry are what place its suffix — but a handful of suffixes are decoded
    /// rather than one per entry, and the decode is the expensive half: 207 → 146 ns at 32 keys
    /// a block on the dictionary, 751 → 454 at 128, 464 → 265 on a path list.
    ///
    /// A staircase deeper than the stack falls back to decoding every entry, which is correct at
    /// any depth.
    fn climb(
        &self,
        run: CodedRun<'_>,
        steps: usize,
        out: &mut Vec<u8>,
        stair: &mut Stairs,
    ) -> bool {
        // Off the group's code, not off a reader opened to ask: opening one parses a run's
        // prologue, which is what the walk is about to do anyway.
        if matches!(run.code, Code::Frame(_)) {
            self.climb_as::<true>(run, steps, out, stair)
        } else {
            self.climb_as::<false>(run, steps, out, stair)
        }
    }

    /// [`climb`](Self::climb) for a run whose code kind is known, so that the header loop branches
    /// on it once rather than once an entry.
    fn climb_as<const FRAME: bool>(
        &self,
        run: CodedRun<'_>,
        steps: usize,
        out: &mut Vec<u8>,
        stair: &mut Stairs,
    ) -> bool {
        let mut entries = run.entries();
        if steps > entries.count {
            return false;
        }
        let mut depth = 0usize;
        for _ in 0..steps {
            let Some((l, len)) = entries.head_within::<FRAME>() else {
                return false;
            };
            // Unclamped: the one check after the walk refuses a run that ever passed its end, so
            // every stair kept is one the clamp would not have moved.
            let at = entries.reach();
            entries.skip(len);
            // SAFETY: `depth` stops at `STAIRS`, the return below it comes before a write past
            // that, and every slot below `depth` was written by the loop before this reads it.
            while depth > 0 && unsafe { stair.get_unchecked(depth - 1).assume_init() }.0 >= l {
                depth -= 1;
            }
            if depth == STAIRS {
                // Nothing has been written yet, so `out` still holds the run's first key.
                let mut entries = run.entries();
                return (0..steps).all(|_| self.advance(run.codec, &mut entries, out));
            }
            stair[depth].write((l, at, len));
            depth += 1;
        }
        if entries.reach() > entries.end_bits {
            return false;
        }
        let sfx = entries.sfx;
        let emit = |(l, at, len): (usize, usize, usize), cap: usize, out: &mut Vec<u8>| {
            if l > out.len() {
                return false;
            }
            out.truncate(l);
            let piece = Piece {
                stream: sfx,
                start: at,
                units: len,
            };
            run.codec.decode_into(&self.phrases, piece, out, cap)
        };
        // A stair keeps only the bytes the next one does not overwrite, since that one truncates
        // back to its own shared prefix; paired with its successor, the difference is the cap, and
        // it cannot underflow because a stair is kept only where its prefix is the shorter. The
        // last stair is the key's own tail, and asks for all of it — so that call, alone, carries
        // no cap at all.
        // SAFETY: as above -- `i` is below `depth`, and the walk wrote every slot below that.
        let kept = |i: usize| unsafe { stair.get_unchecked(i).assume_init() };
        let Some(last) = depth.checked_sub(1) else {
            return true;
        };
        for i in 0..last {
            let (a, b) = (kept(i), kept(i + 1));
            if !emit(a, b.0 - a.0, out) {
                return false;
            }
        }
        emit(kept(last), usize::MAX, out)
    }

    /// The key at rank `id` into `out`, cleared first; `false`, with `out` empty, past the last
    /// key.
    ///
    /// Two climbs, not one: the block's restart run up to the microblock the rank falls in, then
    /// that microblock up to the rank. Together they read `block / micro + micro − 2` headers
    /// where one level read `block − 1`.
    pub(crate) fn key_bytes_into(&self, id: u64, out: &mut Vec<u8>) -> bool {
        out.clear();
        let Ok(id) = usize::try_from(id) else {
            return false;
        };
        if id >= self.n {
            return false;
        }
        let b = id / self.block;
        let off = id % self.block;
        let (j, steps) = (off / self.micro, off % self.micro);
        out.extend_from_slice(self.head(b));
        let codec = self.codec_of(b);
        let codes = self.codes_of(b);
        // One buffer for both climbs: a fresh array a climb owns is 768 bytes a walk clears
        // before it writes the handful of stairs it keeps, and the walk is a few hundred.
        let mut stair: Stairs = [const { std::mem::MaybeUninit::uninit() }; STAIRS];
        let restarts = CodedRun {
            codec,
            code: &codes.1,
            data: self.restart_data(b),
            count: self.micros_in(b),
        };
        if j > 0 && !self.climb(restarts, j, out, &mut stair) {
            return false;
        }
        let micro = CodedRun {
            codec,
            code: &codes.0,
            data: self.micro_data(b, j),
            count: self.micro_count(b, j),
        };
        self.climb(micro, steps, out, &mut stair)
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
        if let (true, Some(code)) = (found, &self.code) {
            return match code.decode_owned(buf) {
                Some(s) => {
                    *out = s;
                    true
                }
                None => false,
            };
        }
        match String::from_utf8(buf) {
            Ok(s) => {
                *out = s;
                found
            }
            Err(_) => false,
        }
    }

    /// Batched [`key`](Self::key): one answer per id, aligned with `ids`, `None` where an id is
    /// past the last key or a corrupted blob decodes to something that is not UTF-8.
    ///
    /// **Ids that ascend within one block are answered by a single walk of it.** A block is a
    /// chain — every entry is coded against the one before it — so re-entering it per id re-reads
    /// the same headers, while walking it once decodes each entry once. That is exactly the shape
    /// [`prefix_id_range`](Self::prefix_id_range) hands over, and it is why going through that
    /// range used to be slower than [`prefix`](Self::prefix).
    ///
    /// Ids in no particular order cost what they always did: the per-id staircase, which reads the
    /// headers below the id but decodes only the few entries that contribute a byte. A walk is
    /// started only when the *next* id is in the same block and above this one, so a scattered
    /// batch never pays for entries it does not want.
    pub fn keys_of(&self, ids: &[u64]) -> Vec<Option<String>> {
        let mut out = Vec::with_capacity(ids.len());
        let mut buf: Vec<u8> = Vec::new();
        let mut entries = Entries::empty();
        // The microblock a walk is open on and how many of its entries it has consumed;
        // `usize::MAX` for none.
        let (mut open, mut consumed) = (usize::MAX, 0usize);
        for (i, &id) in ids.iter().enumerate() {
            if id >= self.n as u64 {
                out.push(None);
                continue;
            }
            let at = id as usize;
            let m = self.micro_of(at);
            let j = at % self.block % self.micro;
            if open != m || j < consumed {
                let more = ids.get(i + 1).is_some_and(|&next| {
                    next > id && next < self.n as u64 && self.micro_of(next as usize) == m
                });
                // Opening a walk costs the climb to the microblock's first key, so it only pays
                // when another id of the same microblock follows.
                if !more || !self.key_bytes_into((at - j) as u64, &mut buf) {
                    out.push(self.key(id));
                    open = usize::MAX;
                    continue;
                }
                let (b, j) = (at / self.block, at % self.block / self.micro);
                entries = Entries::of(
                    &self.codes_of(b).0,
                    self.codec_of(b),
                    self.micro_data(b, j),
                    self.micro_count(b, j),
                );
                (open, consumed) = (m, 0);
            }
            let mut ok = true;
            while consumed < j && ok {
                ok = self.advance(self.codec_of(at / self.block), &mut entries, &mut buf);
                consumed += 1;
            }
            if !ok {
                out.push(None);
                open = usize::MAX;
                continue;
            }
            out.push(self.spelled(&buf));
        }
        out
    }

    /// Every key with its id, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (String, u64)> + '_ {
        self.iter_from(0)
    }

    /// All `(key, id)` pairs whose key starts with `prefix`, in lexicographic order.
    ///
    /// A sorted dictionary needs no automaton for this: the matches are the contiguous run that
    /// [`prefix_id_range`](Self::prefix_id_range) names, so the cost is one order lookup plus one
    /// decode per key returned.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// assert_eq!(idx.prefix("ap"), [("apple".to_string(), 0), ("apricot".to_string(), 1)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn prefix(&self, prefix: &str) -> Vec<(String, u64)> {
        self.prefix_iter(prefix).collect()
    }

    /// Like [`prefix`](Self::prefix) but **lazy**: one key is decoded per step, so an autocomplete
    /// wanting the first handful never pays for the rest.
    pub fn prefix_iter<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (String, u64)> + 'a {
        self.iter_from(self.lower_bound(prefix))
            .take_while(move |(k, _)| k.starts_with(prefix))
    }

    /// Every key that is a **prefix of `query`**, shortest first, with its id — the reverse of
    /// [`prefix`](Self::prefix), which returns the keys `query` is a prefix of.
    ///
    /// This is the dictionary-matching query: given a vocabulary and a position in a sentence, it
    /// returns the entries that start there, and [`longest_prefix`](Self::longest_prefix) picks the
    /// one a longest-match tokeniser takes. The empty key, if the index holds it, is a prefix of
    /// everything and comes first.
    ///
    /// One order lookup per character boundary of `query`. A trie answers this in a single walk
    /// down the query; a sorted array has no such walk, and this is what that costs.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["a", "ap", "apple", "b"])?;
    /// assert_eq!(idx.common_prefix("apples"), [("a".to_string(), 0), ("ap".to_string(), 1), ("apple".to_string(), 2)]);
    /// assert_eq!(idx.longest_prefix("apples"), Some(("apple".to_string(), 2)));
    /// assert_eq!(idx.longest_prefix("zebra"), None);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn common_prefix(&self, query: &str) -> Vec<(String, u64)> {
        let mut found = Vec::new();
        if let Some(code) = &self.code {
            let (mut coded, mut ends) = (Vec::new(), Vec::new());
            code.prefixes(query, &mut coded, &mut ends);
            for (end, spelled) in ends {
                if let (id, true) = self.locate(&coded[..spelled]) {
                    found.push((query[..end].to_owned(), id));
                }
            }
            return found;
        }
        for end in 0..=query.len() {
            if !query.is_char_boundary(end) {
                continue;
            }
            if let Some(id) = self.id(&query[..end]) {
                found.push((query[..end].to_owned(), id));
            }
        }
        found
    }

    /// The longest key that is a prefix of `query`, or `None` if no key is — the match a
    /// longest-match tokeniser takes. See [`common_prefix`](Self::common_prefix).
    ///
    /// Walks down from `query` itself and stops at the first hit, so a long match is cheap and
    /// only a query that matches nothing pays for every boundary.
    pub fn longest_prefix(&self, query: &str) -> Option<(String, u64)> {
        if let Some(code) = &self.code {
            let (mut coded, mut ends) = (Vec::new(), Vec::new());
            code.prefixes(query, &mut coded, &mut ends);
            return ends.into_iter().rev().find_map(|(end, spelled)| {
                match self.locate(&coded[..spelled]) {
                    (id, true) => Some((query[..end].to_owned(), id)),
                    _ => None,
                }
            });
        }
        for end in (0..=query.len()).rev() {
            if !query.is_char_boundary(end) {
                continue;
            }
            if let Some(id) = self.id(&query[..end]) {
                return Some((query[..end].to_owned(), id));
            }
        }
        None
    }

    /// All `(key, id)` pairs with `lo <= key < hi`, in lexicographic order.
    pub fn range(&self, lo: &str, hi: &str) -> Vec<(String, u64)> {
        self.range_iter(lo, hi).collect()
    }

    /// Like [`range`](Self::range) but **lazy** — see [`prefix_iter`](Self::prefix_iter).
    pub fn range_iter<'a>(
        &'a self,
        lo: &'a str,
        hi: &'a str,
    ) -> impl Iterator<Item = (String, u64)> + 'a {
        self.iter_from(self.lower_bound(lo))
            .take_while(move |(k, _)| k.as_str() < hi)
    }

    /// [`iter`](Self::iter) resumed after `after`, which is excluded whether or not it is a key —
    /// what a cursor wants.
    ///
    /// ```
    /// use lexindex::DictIndex;
    /// let idx = DictIndex::build(["apple", "apricot", "banana"])?;
    /// let rest: Vec<_> = idx.iter_after("apricot").collect();
    /// assert_eq!(rest, [("banana".to_string(), 2)]);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn iter_after(&self, after: &str) -> impl Iterator<Item = (String, u64)> + '_ {
        let (rank, found) = self.locate_query(after);
        self.iter_from(rank + u64::from(found))
    }

    /// [`iter`](Self::iter) from rank `start` on.
    pub(crate) fn iter_from(&self, start: u64) -> impl Iterator<Item = (String, u64)> + '_ {
        let mut id = usize::try_from(start).unwrap_or(usize::MAX);
        let mut cur: Vec<u8> = Vec::new();
        let mut restart: Vec<u8> = Vec::new();
        let mut entries = Entries::empty();
        let mut restarts = Entries::empty();
        let mut primed = false;
        std::iter::from_fn(move || {
            if id >= self.n {
                return None;
            }
            let (b, off) = (id / self.block, id % self.block);
            let (j, k) = (off / self.micro, off % self.micro);
            let ok = if !primed || off == 0 {
                // A block opens on its head; a resume point inside one is reached by walking the
                // restart run to its microblock and that microblock to the key.
                restart.clear();
                restart.extend_from_slice(self.head(b));
                restarts = Entries::of(
                    &self.codes_of(b).1,
                    self.codec_of(b),
                    self.restart_data(b),
                    self.micros_in(b),
                );
                primed = true;
                let ok =
                    (0..j).all(|_| self.advance(self.codec_of(b), &mut restarts, &mut restart));
                cur.clear();
                cur.extend_from_slice(&restart);
                entries = Entries::of(
                    &self.codes_of(b).0,
                    self.codec_of(b),
                    self.micro_data(b, j),
                    self.micro_count(b, j),
                );
                ok && (0..k).all(|_| self.advance(self.codec_of(b), &mut entries, &mut cur))
            } else if k == 0 {
                // The next microblock opens on the next restart, not on the key just returned.
                let ok = self.advance(self.codec_of(b), &mut restarts, &mut restart);
                cur.clear();
                cur.extend_from_slice(&restart);
                entries = Entries::of(
                    &self.codes_of(b).0,
                    self.codec_of(b),
                    self.micro_data(b, j),
                    self.micro_count(b, j),
                );
                ok
            } else {
                self.advance(self.codec_of(b), &mut entries, &mut cur)
            };
            if !ok {
                id = self.n; // a stream this crate did not write ends the walk
                return None;
            }
            let this = id as u64;
            id += 1;
            let key = match &self.code {
                None => String::from_utf8_lossy(&cur).into_owned(),
                Some(_) => match self.spelled(&cur) {
                    Some(key) => key,
                    None => {
                        id = self.n;
                        return None;
                    }
                },
            };
            Some((key, this))
        })
    }
}
