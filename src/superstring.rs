//! One string every key occurs in, for the merged layout of [`StringArena`](crate::arena): a key
//! that occurs inside another costs no bytes, and two keys where one ends as the other begins share
//! the overlap. The greedy merge of the shortest-common-superstring literature, run on an
//! Aho–Corasick automaton over the keys in the manner of Ukkonen (1990).
//!
//! # Construction
//!
//! The keys come sorted and distinct. Their trie is numbered level by level, so the children of a
//! node are one contiguous run of the next level, sorted by their byte, and its nodes are laid down
//! in one pass over the keys; the failure links follow a level at a time, the link of a node only
//! reading nodes above it.
//!
//! A key is **contained** when it is a proper prefix of the key after it in sorted order, or when
//! its trie node is the failure link of some node — then it ends inside the key that node spells a
//! prefix of. The first such node in node order is where the key is placed.
//!
//! Every other key is **free**, and the free keys are chained greedily: overlaps of length `d` are
//! taken before any of length `d − 1`, and a key ending in a suffix that is a node `v` of depth `d`
//! may be followed by any free key under `v` that nothing precedes yet and that does not start its
//! own chain. A key's candidate overlaps are its failure chain, longest first, so it walks down
//! that chain one node at a time, and the free keys under a node are a list concatenated from its
//! children's, one level at a time from the deepest any key waits at up. The chains are then
//! written one after another, each key once, minus its overlap with the one before.
//!
//! **Ties.** Among the keys whose overlaps have the same length, the one in the lower slot is served
//! first, and it takes the smallest key in byte order that it may. Chains are written in the slot
//! order of their first key. So the superstring depends on the key set and the slot order alone,
//! and two builds of one key set give the same bytes on any number of threads.
//!
//! **Cost.** `O(N log σ)` for the automaton, `N` the bytes of all keys and `σ ≤ 256` the children a
//! node can have, searched by bisection; `O(N + n log n)` for the merge, the log from serving each
//! depth's keys in slot order. The nodes, the failure links, the contained keys and the writing of
//! the chains run on every thread; serving the overlaps, in the order the ties fix, runs on one.
//! Memory, all of it gone when this returns: one copy of the keys, nine bytes per trie node — there
//! are at most `N` — and a few dozen bytes per key.

/// "No key" and "no node": key indices and node ids are both below it.
const NONE: u32 = u32::MAX;

/// The superstring and where each key, by its sorted index, starts in it.
pub(crate) struct Superstring {
    pub(crate) data: Vec<u8>,
    pub(crate) starts: Vec<u64>,
}

/// The keys as one contiguous run of bytes, so the sweep reads each depth in one forward pass.
struct Text {
    bytes: Vec<u8>,
    ends: Vec<usize>,
}

impl Text {
    /// Copied on every thread: keys sorted apart from the order they were allocated in are a read
    /// that misses the cache each. `S` need not be `Sync`, so the threads read their bytes.
    fn new<S: AsRef<str>>(keys: &[S]) -> Self {
        let keys: Vec<&[u8]> = keys.iter().map(|k| k.as_ref().as_bytes()).collect();
        let mut ends = Vec::with_capacity(keys.len() + 1);
        ends.push(0);
        for k in &keys {
            ends.push(ends[ends.len() - 1] + k.len());
        }
        let mut bytes = vec![0u8; ends[keys.len()]];
        let copy = |keys: &[&[u8]], out: &mut [u8]| {
            let mut at = 0;
            for k in keys {
                out[at..at + k.len()].copy_from_slice(k);
                at += k.len();
            }
        };
        let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        if keys.len() < PARALLEL_KEYS || threads == 1 {
            copy(&keys, &mut bytes);
        } else {
            let per = keys.len().div_ceil(threads);
            std::thread::scope(|scope| {
                let mut rest = &mut bytes[..];
                for (c, run) in keys.chunks(per).enumerate() {
                    let width = ends[c * per + run.len()] - ends[c * per];
                    let (out, tail) = std::mem::take(&mut rest).split_at_mut(width);
                    rest = tail;
                    scope.spawn(move || copy(run, out));
                }
            });
        }
        Self { bytes, ends }
    }

    fn key(&self, i: usize) -> &[u8] {
        &self.bytes[self.ends[i]..self.ends[i + 1]]
    }

    fn len(&self, i: usize) -> usize {
        self.ends[i + 1] - self.ends[i]
    }
}

/// The keys' trie, level by level, with failure links.
struct Trie {
    /// The byte on the edge into each node; the root's is unused.
    label: Vec<u8>,
    /// Children of node `v` are the nodes `first[v] .. first[v + 1]`, one past the last node a
    /// sentinel.
    first: Vec<u32>,
    /// Longest proper suffix of each node's string that is also a node.
    fail: Vec<u32>,
    /// `level[d]` is the first node of depth `d`; the last entry is the node count.
    level: Vec<u32>,
}

/// The levels of a [`Trie`] a failure link of the level being built may read: every node above
/// it, with its children and links. Borrowed apart from that level's own links, which threads
/// write.
#[derive(Clone, Copy)]
struct Above<'a> {
    label: &'a [u8],
    first: &'a [u32],
    fail: &'a [u32],
}

impl Above<'_> {
    fn child(self, v: u32, c: u8) -> Option<u32> {
        let (lo, hi) = (self.first[v as usize], self.first[v as usize + 1]);
        let run = &self.label[lo as usize..hi as usize];
        run.binary_search(&c).ok().map(|k| lo + k as u32)
    }

    /// The node reached from `v` on byte `c`, falling back along the failure links.
    fn step(self, mut v: u32, c: u8) -> u32 {
        loop {
            if let Some(w) = self.child(v, c) {
                return w;
            }
            if v == 0 {
                return 0;
            }
            v = self.fail[v as usize];
        }
    }
}

/// A level narrower than this links its nodes on the calling thread: spawning costs more than
/// it saves.
const PARALLEL_LEVEL: usize = if cfg!(test) { 4 } else { 1 << 14 };

/// Fewer keys than this are written out on the calling thread.
const PARALLEL_KEYS: usize = if cfg!(test) { 2 } else { 1 << 16 };

/// Chains a thread takes at a time when it measures them.
const CHAINS_TAKEN: usize = 16;

/// A trie with fewer nodes than this is laid down on the calling thread.
const PARALLEL_NODES: usize = if cfg!(test) { 8 } else { 1 << 18 };

/// The failure links of the nodes `start .. end`, one depth, each of which holds its parent in
/// `fail` until its link replaces it. A link only reads the levels above — a node's link is
/// shorter than the node, so it and every node its fallback visits sit higher up — and those are
/// linked already, so the nodes are linked in any order, on as many threads as there are, to the
/// same links. These are reads that miss the cache once the automaton outgrows it, the part of the
/// merge most worth spreading.
fn link_level(trie: &mut Trie, start: usize, end: usize) {
    let (fail, here) = trie.fail.split_at_mut(start);
    let here = &mut here[..end - start];
    let above = Above {
        label: &trie.label,
        first: &trie.first,
        fail,
    };
    let labels = &trie.label[start..end];
    let link = |out: &mut [u32], labels: &[u8]| {
        for (f, &c) in out.iter_mut().zip(labels) {
            *f = above.step(above.fail[*f as usize], c);
        }
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if here.len() < PARALLEL_LEVEL || threads == 1 {
        return link(here, labels);
    }
    let chunk = here.len().div_ceil(threads);
    std::thread::scope(|scope| {
        for (out, labels) in here.chunks_mut(chunk).zip(labels.chunks(chunk)) {
            scope.spawn(move || link(out, labels));
        }
    });
}

impl Trie {
    fn depth(&self, v: u32) -> usize {
        self.level.partition_point(|&s| s <= v) - 1
    }
}

/// What the sweep finds besides the trie: each key's node, and for a key that occurs inside
/// another, which key and where.
struct Sweep {
    trie: Trie,
    term: Vec<u32>,
    /// Each key's shared prefix with the key before it.
    lcp: Vec<u32>,
    /// `(container, offset)` for a contained key, `(NONE, 0)` for a free one.
    inside: Vec<(u32, u32)>,
}

/// The greedy superstring of `keys` — sorted, distinct — whose slot order is `order`, a
/// permutation of the key indices. `None` when the trie has too many nodes for a `u32` id, a
/// corpus of several GiB, which keeps the layouts it had.
pub(crate) fn merge<S: AsRef<str>>(keys: &[S], order: &[u32]) -> Option<Superstring> {
    let n = keys.len();
    debug_assert_eq!(order.len(), n);
    debug_assert!(keys.windows(2).all(|w| w[0].as_ref() < w[1].as_ref()));
    if n == 0 || n >= NONE as usize {
        return None;
    }
    let text = Text::new(keys);
    let sweep = sweep(&text)?;
    let chains = chain(&text, &sweep, order);
    Some(lay_out(&text, &sweep, &chains, order))
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// `f(at, chunk)` over `out` cut into one chunk a thread, `at` the chunk's offset; on the calling
/// thread alone when `out` is shorter than `min`, where spawning costs more than it saves.
fn on_threads<T: Send>(out: &mut [T], min: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if out.len() < min || threads == 1 {
        return f(0, out);
    }
    let chunk = out.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|scope| {
        for (c, part) in out.chunks_mut(chunk).enumerate() {
            scope.spawn(move || f(c * chunk, part));
        }
    });
}

/// One run of keys' range of every depth: the bytes of its nodes there, and their parents.
#[derive(Default)]
struct RunNodes<'a> {
    labels: Vec<&'a mut [u8]>,
    parents: Vec<&'a mut [u32]>,
}

/// Every node's byte and parent, the parent in `fail`, and each key's own node in `term`: key `i`
/// makes the next node of every depth past `lcp[i]` it reaches, and the nodes of a depth are
/// numbered in key order from `level`. The keys are cut into runs, one a thread; a run's nodes
/// of each depth are one contiguous range, after the ranges of the runs before it, so the runs
/// write apart, and the node of every depth a run's first key shares with the key before it is
/// the last one numbered before the run.
fn lay_nodes(
    text: &Text,
    lcp: &[u32],
    level: &[u32],
    label: &mut [u8],
    fail: &mut [u32],
    term: &mut [u32],
) {
    let n = lcp.len();
    let depths = level.len() - 1;
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let runs = if label.len() < PARALLEL_NODES {
        1
    } else {
        threads
    };
    let run_keys = n.div_ceil(runs).max(1);
    // Nodes each run makes at each depth, then where its range of each depth starts.
    let mut starts: Vec<Vec<u32>> = lcp
        .chunks(run_keys)
        .enumerate()
        .map(|(r, run)| {
            let mut opens = vec![0i64; depths + 1];
            for (j, &shared) in run.iter().enumerate() {
                opens[shared as usize + 1] += 1;
                opens[text.len(r * run_keys + j) + 1] -= 1;
            }
            let mut width = 0i64;
            let mut counts = vec![0u32; depths];
            for (count, &open) in counts.iter_mut().zip(&opens).skip(1) {
                width += open;
                *count = width as u32;
            }
            counts
        })
        .collect();
    let mut next: Vec<u32> = level[..depths].to_vec();
    for counts in &mut starts {
        for (count, at) in counts.iter_mut().zip(&mut next) {
            let width = *count;
            *count = *at;
            *at += width;
        }
    }
    let mut runs_nodes: Vec<RunNodes<'_>> = starts.iter().map(|_| RunNodes::default()).collect();
    let (mut label, mut fail) = (&mut label[1..], &mut fail[1..]);
    for d in 1..depths {
        let mut at = level[d];
        for (r, run) in runs_nodes.iter_mut().enumerate() {
            let end = starts.get(r + 1).map_or(level[d + 1], |s| s[d]);
            let width = (end - at) as usize;
            let (l, rest) = std::mem::take(&mut label).split_at_mut(width);
            let (f, rest_f) = std::mem::take(&mut fail).split_at_mut(width);
            run.labels.push(l);
            run.parents.push(f);
            (label, fail) = (rest, rest_f);
            at = end;
        }
    }
    let lay = |r: usize, run: &mut RunNodes<'_>, term: &mut [u32]| {
        let start = &starts[r];
        let first = r * run_keys;
        let mut path = vec![0u32; depths];
        for d in 1..=lcp[first] as usize {
            path[d] = start[d] - 1;
        }
        let mut fill = vec![0usize; depths];
        for (j, own) in term.iter_mut().enumerate() {
            let i = first + j;
            let key = text.key(i);
            for d in lcp[i] as usize + 1..=key.len() {
                let k = fill[d];
                fill[d] += 1;
                run.labels[d - 1][k] = key[d - 1];
                // The parent, until `link_level` puts the link in its place.
                run.parents[d - 1][k] = path[d - 1];
                path[d] = start[d] + k as u32;
            }
            *own = path[key.len()];
        }
    };
    std::thread::scope(|scope| {
        for (r, (run, term)) in runs_nodes
            .iter_mut()
            .zip(term.chunks_mut(run_keys))
            .enumerate()
        {
            let lay = &lay;
            if runs == 1 {
                lay(r, run, term);
            } else {
                scope.spawn(move || lay(r, run, term));
            }
        }
    });
}

/// Build the trie, its failure links, and find the contained keys.
///
/// The nodes are numbered level by level, but laid down in one pass over the keys in order: a
/// key makes a node at every depth past the prefix it shares with the key before it, so the
/// count of each level, and where it starts, is known from those shared prefixes first, and each
/// key then takes the next number of every level it reaches, so the keys are read once, front to
/// back. The links follow, a level at a time.
fn sweep(text: &Text) -> Option<Sweep> {
    let n = text.ends.len() - 1;
    let mut lcp = vec![0u32; n];
    on_threads(&mut lcp, PARALLEL_KEYS, |at, out| {
        for (i, shared) in (at..).zip(out) {
            if i > 0 {
                *shared = common_prefix(text.key(i - 1), text.key(i)) as u32;
            }
        }
    });
    let longest = (0..n).map(|i| text.len(i)).max().unwrap_or(0);
    // Key `i` makes one node at each depth `lcp[i] + 1 ..= len(i)`: the width of a level is a
    // running sum of where those ranges open and close.
    let mut opens = vec![0i64; longest + 2];
    for (i, &shared) in lcp.iter().enumerate() {
        opens[shared as usize + 1] += 1;
        opens[text.len(i) + 1] -= 1;
    }
    let mut level = Vec::with_capacity(longest + 2);
    level.extend([0u32, 1]);
    let (mut width, mut nodes) = (0i64, 1u64);
    for &open in &opens[1..=longest] {
        width += open;
        nodes += width as u64;
        if nodes >= u64::from(NONE) {
            return None;
        }
        level.push(nodes as u32);
    }
    let nodes = nodes as usize;
    let mut label = vec![0u8; nodes];
    let mut fail = vec![0u32; nodes];
    let mut term = vec![0u32; n];
    lay_nodes(text, &lcp, &level, &mut label, &mut fail, &mut term);
    // The parents run in node order, so a node's children start at the first node whose parent
    // is not before it — and a node without any gets the start of the next one's, so every run
    // ends where the next begins.
    let mut first = vec![0u32; nodes + 1];
    first[nodes] = nodes as u32;
    on_threads(&mut first[..nodes], 1 << 16, |at, out| {
        let mut u = 1 + fail[1..].partition_point(|&p| (p as usize) < at);
        for (v, f) in (at..).zip(out) {
            while u < nodes && (fail[u] as usize) < v {
                u += 1;
            }
            *f = u as u32;
        }
    });
    let mut is_term = vec![0u64; nodes.div_ceil(64)];
    for (i, &u) in term.iter().enumerate() {
        if text.len(i) > 0 {
            is_term[u as usize / 64] |= 1 << (u % 64);
        }
    }
    let mut trie = Trie {
        label,
        first,
        fail,
        level,
    };
    // Every link at depth 1 is the root, which is also the parent there.
    for d in 2..trie.level.len() - 1 {
        let (start, end) = (trie.level[d] as usize, trie.level[d + 1] as usize);
        link_level(&mut trie, start, end);
    }
    // The key ending at each terminal node, by its rank among them: one read and a popcount, not
    // a bisection over the terminals, which would miss the cache at every step.
    let mut terms_before = Vec::with_capacity(is_term.len());
    let mut count = 0u32;
    for word in &is_term {
        terms_before.push(count);
        count += word.count_ones();
    }
    let rank = |u: u32| {
        let (word, bit) = (u as usize / 64, u % 64);
        terms_before[word] + (is_term[word] & ((1u64 << bit) - 1)).count_ones()
    };
    let is_term = |u: u32| (is_term[u as usize / 64] >> (u % 64)) & 1 == 1;
    let mut key_at = vec![0u32; count as usize];
    for (i, &u) in term.iter().enumerate() {
        if text.len(i) > 0 {
            key_at[rank(u) as usize] = i as u32;
        }
    }
    // The first node in node order whose link ends a key is where that key is placed: the nodes
    // are read on every thread, and the lowest any thread finds for a key wins. The nodes of
    // depth 1 all link to the root, which `is_term` leaves out.
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    let placed: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(NONE)).collect();
    let find = |from: usize, to: usize| {
        for u in from..to {
            let f = trie.fail[u];
            if is_term(f) {
                let k = key_at[rank(f) as usize] as usize;
                // A thread reads its nodes in order, so past its first, this load turns it away.
                if placed[k].load(Relaxed) > u as u32 {
                    placed[k].fetch_min(u as u32, Relaxed);
                }
            }
        }
    };
    let below_root = trie.level.get(2).map_or(nodes, |&s| s as usize);
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if nodes - below_root < PARALLEL_NODES || threads == 1 {
        find(below_root, nodes);
    } else {
        let chunk = (nodes - below_root).div_ceil(threads);
        std::thread::scope(|scope| {
            for from in (below_root..nodes).step_by(chunk) {
                scope.spawn(move || find(from, (from + chunk).min(nodes)));
            }
        });
    }
    // Inside the smallest key under that node, which its leftmost path down reaches — a node that
    // ends no key has children.
    let mut inside = vec![(NONE, 0u32); n];
    on_threads(&mut inside, PARALLEL_KEYS, |at, out| {
        for (k, place) in (at..).zip(out) {
            let u = placed[k].load(Relaxed);
            if u == NONE {
                continue;
            }
            let mut v = u;
            while !is_term(v) {
                v = trie.first[v as usize];
            }
            *place = (
                key_at[rank(v) as usize],
                (trie.depth(u) - text.len(k)) as u32,
            );
        }
    });
    drop(placed);
    // A key that is a proper prefix of another is a prefix of the key right after it.
    for i in 0..n.saturating_sub(1) {
        if lcp[i + 1] as usize == text.len(i) {
            inside[i] = (i as u32 + 1, 0);
        }
    }
    Some(Sweep {
        trie,
        term,
        lcp,
        inside,
    })
}

/// The greedy chains over the free keys: `(next, overlap)` per key, `NONE` where a chain ends.
fn chain(text: &Text, sweep: &Sweep, order: &[u32]) -> Vec<(u32, u32)> {
    let n = order.len();
    let trie = &sweep.trie;
    let free = |k: usize| sweep.inside[k].0 == NONE;
    let mut link = vec![(NONE, 0u32); n];
    // A chain's head knows its tail and its tail its head; a key alone is both.
    let mut other_end: Vec<u32> = (0..n as u32).collect();
    let mut preceded = vec![false; n];
    let depths = trie.level.len() - 1;
    // Where each free key waits first, by slot, found on every thread: the link of its node, the
    // longest overlap it could have, and that node's depth.
    let mut first_wait = vec![(0u32, 0u32); n];
    on_threads(&mut first_wait, PARALLEL_KEYS, |at, out| {
        for (wait, &k) in out.iter_mut().zip(&order[at..]) {
            if free(k as usize) {
                let v = trie.fail[sweep.term[k as usize] as usize];
                *wait = (v, trie.depth(v) as u32);
            }
        }
    });
    // Keys waiting at each depth for an overlap that long, as `(slot, key, node)`, each depth's
    // in slot order.
    let mut waiting: Vec<Vec<(u32, u32, u32)>> = vec![Vec::new(); depths];
    for (slot, (&k, &(v, d))) in order.iter().zip(&first_wait).enumerate() {
        if v != 0 {
            waiting[d as usize].push((slot as u32, k, v));
        }
    }
    drop(first_wait);
    // The free keys under each node of the level at hand, and of the level below it: a list
    // threaded through `list_next`, in byte order.
    let mut list_next = vec![NONE; n];
    let widest = trie
        .level
        .windows(2)
        .map(|w| w[1] - w[0])
        .max()
        .unwrap_or(0) as usize;
    let mut here = vec![(NONE, NONE); widest];
    let mut below = vec![(NONE, NONE); widest];
    // Free keys by their node, which is node order: a key's node is on the level of its length,
    // and the nodes of a level are numbered in key order — a counting sort by length.
    let mut at_len = vec![0usize; depths + 1];
    for k in (0..n).filter(|&k| free(k)) {
        at_len[text.len(k) + 1] += 1;
    }
    for d in 1..=depths {
        at_len[d] += at_len[d - 1];
    }
    let mut ending = vec![(0u32, 0u32); at_len[depths]];
    for k in (0..n).filter(|&k| free(k)) {
        let at = &mut at_len[text.len(k)];
        ending[*at] = (sweep.term[k], k as u32);
        *at += 1;
    }
    drop(at_len);
    debug_assert!(ending.windows(2).all(|w| w[0] < w[1]));
    let mut ending_at = ending.len();
    // No key waits below the deepest overlap any key starts at, so the lists there are never
    // read. That level's lists come from the keys in order instead — a node's free keys are the
    // keys under it, which are consecutive — and the levels below it are never visited.
    let deepest = (1..depths)
        .rev()
        .find(|&d| !waiting[d].is_empty())
        .unwrap_or(0);
    for d in (1..=deepest).rev() {
        let (lo, hi) = (trie.level[d], trie.level[d + 1]);
        let child_lo = hi;
        let first_ending = ending.partition_point(|e| e.0 < lo);
        let mut e = first_ending;
        if d == deepest {
            // Nodes of depth `d` made so far: each key past it either makes the next or shares
            // the last.
            let mut made = 0usize;
            for (k, &shared) in sweep.lcp.iter().enumerate() {
                if text.len(k) < d {
                    continue;
                }
                made += usize::from((shared as usize) < d);
                if !free(k) {
                    continue;
                }
                list_next[k] = NONE;
                let list = &mut here[made - 1];
                if list.0 == NONE {
                    list.0 = k as u32;
                } else {
                    list_next[list.1 as usize] = k as u32;
                }
                list.1 = k as u32;
            }
        } else {
            for v in lo..hi {
                let (mut head, mut tail) = (NONE, NONE);
                if e < ending_at && ending[e].0 == v {
                    let k = ending[e].1;
                    list_next[k as usize] = NONE;
                    (head, tail) = (k, k);
                    e += 1;
                }
                let (c_lo, c_hi) = (trie.first[v as usize], trie.first[v as usize + 1]);
                for c in c_lo..c_hi {
                    let (h, t) = below[(c - child_lo) as usize];
                    if h == NONE {
                        continue;
                    }
                    if head == NONE {
                        head = h;
                    } else {
                        list_next[tail as usize] = h;
                    }
                    tail = t;
                }
                here[(v - lo) as usize] = (head, tail);
            }
        }
        ending_at = first_ending;
        // A few runs in slot order already — the keys that wait here first, then those each
        // deeper level turned away, in the order it served them — which a merge sort finds and
        // merges rather than sorting afresh.
        let mut batch = std::mem::take(&mut waiting[d]);
        batch.sort_by_key(|&(slot, _, _)| slot);
        for &(slot, s, v) in &batch {
            let list = &mut here[(v - lo) as usize];
            match take(list, &mut list_next, &preceded, other_end[s as usize]) {
                Some(t) => {
                    link[s as usize] = (t, d as u32);
                    preceded[t as usize] = true;
                    let (head, tail) = (other_end[s as usize], other_end[t as usize]);
                    other_end[head as usize] = tail;
                    other_end[tail as usize] = head;
                }
                None => {
                    let f = trie.fail[v as usize];
                    if f != 0 {
                        waiting[trie.depth(f)].push((slot, s, f));
                    }
                }
            }
        }
        std::mem::swap(&mut here, &mut below);
    }
    debug_assert!((0..n).all(|k| link[k].0 == NONE || text.len(k) > link[k].1 as usize));
    link
}

/// The first key of `list` that nothing precedes yet and that is not `forbidden` — the head of
/// the chain the taker ends, which would close it into a cycle — unlinked from the list. Keys
/// that something already precedes are dropped from the list as they are met: they never become
/// takeable again.
fn take(list: &mut (u32, u32), next: &mut [u32], preceded: &[bool], forbidden: u32) -> Option<u32> {
    while list.0 != NONE && preceded[list.0 as usize] {
        list.0 = next[list.0 as usize];
    }
    if list.0 == NONE {
        list.1 = NONE;
        return None;
    }
    let head = list.0;
    if head != forbidden {
        list.0 = next[head as usize];
        if list.0 == NONE {
            list.1 = NONE;
        }
        return Some(head);
    }
    let prev = head;
    loop {
        let c = next[prev as usize];
        if c == NONE {
            return None;
        }
        next[prev as usize] = next[c as usize];
        if list.1 == c {
            list.1 = prev;
        }
        if !preceded[c as usize] {
            return Some(c);
        }
    }
}

/// Write the chains in the slot order of their heads and place every key.
fn lay_out(text: &Text, sweep: &Sweep, link: &[(u32, u32)], order: &[u32]) -> Superstring {
    let n = order.len();
    let free = |k: usize| sweep.inside[k].0 == NONE;
    let mut preceded = vec![false; n];
    let mut total = 0usize;
    for (k, &(t, o)) in link.iter().enumerate() {
        if free(k) {
            total += text.len(k);
        }
        if t != NONE {
            preceded[t as usize] = true;
            total -= o as usize;
        }
    }
    // A chain's bytes are known only by walking it, a dependent read a key, so the chains are
    // walked on every thread twice: once for their lengths, which place them, then to write them.
    // Chains run from one key to a third of all of them, so the lengths are handed out a few
    // chains at a time, and the writing is cut by bytes.
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
    let heads: Vec<u32> = order
        .iter()
        .copied()
        .filter(|&k| free(k as usize) && !preceded[k as usize])
        .collect();
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let threads = if n < PARALLEL_KEYS { 1 } else { threads };
    let lens: Vec<AtomicU64> = heads.iter().map(|_| AtomicU64::new(0)).collect();
    let taken = AtomicUsize::new(0);
    let measure = || {
        loop {
            let from = taken.fetch_add(CHAINS_TAKEN, Relaxed);
            if from >= heads.len() {
                break;
            }
            for j in from..(from + CHAINS_TAKEN).min(heads.len()) {
                let mut k = heads[j] as usize;
                let mut len = text.len(k);
                loop {
                    let (t, o) = link[k];
                    if t == NONE {
                        break;
                    }
                    k = t as usize;
                    len += text.len(k) - o as usize;
                }
                lens[j].store(len as u64, Relaxed);
            }
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(measure);
        }
        measure();
    });
    let mut at = Vec::with_capacity(heads.len() + 1);
    at.push(0usize);
    for len in lens {
        at.push(at[at.len() - 1] + len.into_inner() as usize);
    }
    debug_assert_eq!(at[heads.len()], total);
    let mut data = vec![0u8; total];
    let starts: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(u64::MAX)).collect();
    let write = |heads: &[u32], at: &[usize], out: &mut [u8]| {
        let base = at[0];
        for (&head, &from) in heads.iter().zip(at) {
            let mut k = head as usize;
            let mut pos = from - base;
            let key = text.key(k);
            out[pos..pos + key.len()].copy_from_slice(key);
            starts[k].store(from as u64, Relaxed);
            pos += key.len();
            loop {
                let (t, o) = link[k];
                if t == NONE {
                    break;
                }
                k = t as usize;
                let tail = &text.key(k)[o as usize..];
                starts[k].store((base + pos - o as usize) as u64, Relaxed);
                out[pos..pos + tail.len()].copy_from_slice(tail);
                pos += tail.len();
            }
        }
    };
    // Group `g` takes the chains that start in its share of the bytes.
    let cuts: Vec<usize> = (0..=threads)
        .map(|g| {
            at.partition_point(|&b| b < total * g / threads)
                .min(heads.len())
        })
        .collect();
    std::thread::scope(|scope| {
        let mut rest = &mut data[..];
        for g in 0..threads {
            let (lo, hi) = (
                cuts[g],
                if g + 1 == threads {
                    heads.len()
                } else {
                    cuts[g + 1]
                },
            );
            let (out, tail) = std::mem::take(&mut rest).split_at_mut(at[hi] - at[lo]);
            rest = tail;
            let (group, at, write) = (&heads[lo..hi], &at[lo..=hi], &write);
            scope.spawn(move || write(group, at, out));
        }
    });
    let mut starts: Vec<u64> = starts.into_iter().map(AtomicU64::into_inner).collect();
    // A contained key sits where its container puts it; a container is strictly longer, so the
    // walk up ends at a free key.
    let mut path = Vec::new();
    for k in 0..n {
        let mut c = k;
        while starts[c] == u64::MAX {
            path.push(c);
            c = sweep.inside[c].0 as usize;
        }
        while let Some(p) = path.pop() {
            let (container, offset) = sweep.inside[p];
            starts[p] = starts[container as usize] + u64::from(offset);
        }
    }
    Superstring { data, starts }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(n: usize) -> Vec<u32> {
        (0..n as u32).collect()
    }

    fn sorted(keys: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
        v.sort();
        v.dedup();
        v
    }

    fn check(keys: &[String], order: &[u32]) -> Superstring {
        let s = merge(keys, order).unwrap();
        for (i, k) in keys.iter().enumerate() {
            let at = s.starts[i] as usize;
            assert_eq!(&s.data[at..at + k.len()], k.as_bytes(), "key {k:?}");
        }
        assert!(std::str::from_utf8(&s.data).is_ok());
        s
    }

    /// Contained keys cost nothing and overlaps are shared: `"ab"` is inside `"cab"`, and
    /// `"cab"` → `"abd"` overlaps by two.
    #[test]
    fn contained_keys_are_free_and_overlaps_are_shared() {
        let keys = sorted(&["cab", "abd", "ab", "b"]);
        let s = check(&keys, &identity(keys.len()));
        assert_eq!(s.data, b"cabd");
    }

    /// The tie the module documents: `"xa"` and `"ya"` both end in `"a"`, which both `"ab"` and
    /// `"ac"` start with. The key in the lower slot is served first and takes the smaller key in
    /// byte order; the chains are written in the slot order of their heads.
    #[test]
    fn ties_go_to_the_lower_slot_and_the_smaller_key() {
        let keys = sorted(&["ab", "ac", "xa", "ya"]); // indices 0 ab, 1 ac, 2 xa, 3 ya
        let s = check(&keys, &[3, 2, 1, 0]); // slots: ya, xa, ac, ab
        assert_eq!(s.data, b"yabxac");
        let s = check(&keys, &[2, 3, 0, 1]); // slots: xa, ya, ab, ac
        assert_eq!(s.data, b"xabyac");
    }

    /// The longest overlap wins over a shorter one met first, and a chain never closes on itself.
    #[test]
    fn longer_overlaps_go_first_and_chains_stay_acyclic() {
        let keys = sorted(&["abc", "bcd", "cda", "dab"]);
        let s = check(&keys, &identity(keys.len()));
        assert_eq!(s.data.len(), 6); // a cycle of overlap 2 cut once: 3 + 1 + 1 + 1
        let keys = sorted(&["aa"]);
        assert_eq!(check(&keys, &[0]).data, b"aa");
    }

    /// Multi-byte keys: every key is found where its start says, and the superstring is UTF-8.
    #[test]
    fn multibyte_keys_stay_whole() {
        let keys = sorted(&["中文", "文字", "字中", "é中", "", "中"]);
        let s = check(&keys, &identity(keys.len()));
        assert!(s.data.len() < keys.iter().map(String::len).sum::<usize>());
    }

    /// The same greedy written the obvious way, quadratic and over strings: contained keys by
    /// `contains`, then for each overlap length from the longest down, the keys that may still
    /// be followed in slot order, each taking the smallest key in byte order that it may.
    fn naive(keys: &[String], order: &[u32]) -> Vec<u8> {
        let n = keys.len();
        let free: Vec<bool> = (0..n)
            .map(|i| !(0..n).any(|j| j != i && keys[j].contains(keys[i].as_str())))
            .collect();
        let mut slot_of = vec![0usize; n];
        for (s, &k) in order.iter().enumerate() {
            slot_of[k as usize] = s;
        }
        let mut by_slot: Vec<usize> = (0..n).filter(|&k| free[k]).collect();
        by_slot.sort_by_key(|&k| slot_of[k]);
        let mut link = vec![None; n];
        let mut preceded = vec![false; n];
        let mut head: Vec<usize> = (0..n).collect(); // head of the chain each key ends
        let longest = keys.iter().map(String::len).max().unwrap_or(0);
        for d in (1..longest).rev() {
            for &s in &by_slot {
                let key = keys[s].as_bytes();
                if link[s].is_some() || key.len() <= d {
                    continue;
                }
                let suffix = &key[key.len() - d..];
                let t = (0..n).find(|&t| {
                    free[t]
                        && !preceded[t]
                        && t != head[s]
                        && keys[t].len() > d
                        && keys[t].as_bytes().starts_with(suffix)
                });
                if let Some(t) = t {
                    link[s] = Some((t, d));
                    preceded[t] = true;
                    let mut end = t;
                    while let Some((u, _)) = link[end] {
                        end = u;
                    }
                    head[end] = head[s];
                }
            }
        }
        let mut data = Vec::new();
        for &k in order {
            let mut k = k as usize;
            if !free[k] || preceded[k] {
                continue;
            }
            data.extend_from_slice(keys[k].as_bytes());
            while let Some((t, o)) = link[k] {
                data.extend_from_slice(&keys[t].as_bytes()[o..]);
                k = t;
            }
        }
        data
    }

    /// Against the obvious version of the same greedy, on small random sets over a four-letter
    /// alphabet of one to three bytes a letter: the same superstring, byte for byte, with every
    /// key where its start says.
    #[test]
    fn random_small_sets_agree_with_the_naive_greedy() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..2000 {
            let n = (next() % 30) as usize;
            let mut keys: Vec<String> = (0..n)
                .map(|_| {
                    let len = (next() % 6) as usize;
                    (0..len)
                        .map(|_| ['a', 'b', 'é', '中'][(next() % 4) as usize])
                        .collect()
                })
                .collect();
            keys.sort();
            keys.dedup();
            if keys.is_empty() {
                continue;
            }
            let mut order = identity(keys.len());
            for i in (1..order.len()).rev() {
                order.swap(i, (next() % (i as u64 + 1)) as usize);
            }
            let s = check(&keys, &order);
            assert_eq!(
                s.data,
                naive(&keys, &order),
                "{keys:?} in slot order {order:?}"
            );
        }
    }
}
