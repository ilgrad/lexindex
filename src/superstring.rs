//! One string every key occurs in, for the merged layout of [`StringArena`](crate::arena): a key
//! that occurs inside another costs no bytes, and two keys where one ends as the other begins share
//! the overlap. The greedy merge of the shortest-common-superstring literature, run on an
//! Aho–Corasick automaton over the keys in the manner of Ukkonen (1990).
//!
//! # Construction
//!
//! The keys come sorted and distinct. Their trie is laid out level by level, so the children of a
//! node are one contiguous run of the next level, sorted by their byte; one sweep per depth builds
//! the nodes and their failure links at once, the link of a node only reading nodes above it.
//!
//! A key is **contained** when it is a proper prefix of the key after it in sorted order, or when
//! its trie node is the failure link of some node — then it ends inside the key that node spells a
//! prefix of. The first such node the sweep meets is where the key is placed.
//!
//! Every other key is **free**, and the free keys are chained greedily: overlaps of length `d` are
//! taken before any of length `d − 1`, and a key ending in a suffix that is a node `v` of depth `d`
//! may be followed by any free key under `v` that nothing precedes yet and that does not start its
//! own chain. A key's candidate overlaps are its failure chain, longest first, so it walks down
//! that chain one node at a time, and the free keys under a node are a list concatenated from its
//! children's, one level at a time from the deepest up. The chains are then written one after
//! another, each key once, minus its overlap with the one before.
//!
//! **Ties.** Among the keys whose overlaps have the same length, the one in the lower slot is served
//! first, and it takes the smallest key in byte order that it may. Chains are written in the slot
//! order of their first key. So the superstring depends on the key set and the slot order alone,
//! and two builds of one key set give the same bytes on any number of threads.
//!
//! **Cost.** `O(N log σ)` for the automaton, `N` the bytes of all keys and `σ ≤ 256` the children a
//! node can have, searched by bisection; `O(N + n log n)` for the merge, the log from serving each
//! depth's keys in slot order. Memory, all of it gone when this returns: one copy of the keys, nine
//! bytes per trie node — there are at most `N` — and a few dozen bytes per key.

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
    fn new<S: AsRef<str>>(keys: &[S]) -> Self {
        let total = keys.iter().map(|k| k.as_ref().len()).sum();
        let mut bytes = Vec::with_capacity(total);
        let mut ends = Vec::with_capacity(keys.len() + 1);
        ends.push(0);
        for k in keys {
            bytes.extend_from_slice(k.as_ref().as_bytes());
            ends.push(bytes.len());
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

impl Trie {
    fn child(&self, v: u32, c: u8) -> Option<u32> {
        let (lo, hi) = (self.first[v as usize], self.first[v as usize + 1]);
        let run = &self.label[lo as usize..hi as usize];
        run.binary_search(&c).ok().map(|k| lo + k as u32)
    }

    /// The node reached from `v` on byte `c`, falling back along the failure links.
    fn step(&self, mut v: u32, c: u8) -> u32 {
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

    fn depth(&self, v: u32) -> usize {
        self.level.partition_point(|&s| s <= v) - 1
    }
}

/// What the sweep finds besides the trie: each key's node, and for a key that occurs inside
/// another, which key and where.
struct Sweep {
    trie: Trie,
    term: Vec<u32>,
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
    let mut slot_of = vec![0u32; n];
    for (slot, &k) in order.iter().enumerate() {
        slot_of[k as usize] = slot as u32;
    }
    let chains = chain(&text, &sweep, &slot_of);
    Some(lay_out(&text, &sweep, &chains, order))
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Build the trie level by level, its failure links with it, and find the contained keys.
fn sweep(text: &Text) -> Option<Sweep> {
    let n = text.ends.len() - 1;
    let mut lcp = vec![0u32; n];
    let mut nodes = 1u64;
    let mut longest = 0usize;
    for (i, shared) in lcp.iter_mut().enumerate() {
        if i > 0 {
            *shared = common_prefix(text.key(i - 1), text.key(i)) as u32;
        }
        nodes += (text.len(i) - *shared as usize) as u64;
        longest = longest.max(text.len(i));
    }
    if nodes >= u64::from(NONE) {
        return None;
    }
    let nodes = nodes as usize;
    let mut trie = Trie {
        label: vec![0; nodes],
        first: vec![NONE; nodes + 1],
        fail: vec![0; nodes],
        level: Vec::with_capacity(longest + 2),
    };
    trie.level.extend([0, 1]);
    trie.first[0] = 1;
    let mut term = vec![0u32; n];
    // Terminal nodes in creation order, which is node order, so a lookup bisects.
    let mut terminals: Vec<(u32, u32)> = Vec::new();
    let mut is_term = vec![0u64; nodes.div_ceil(64)];
    let mut inside = vec![(NONE, 0u32); n];
    // The node of each active key's prefix one byte shorter than the depth being built.
    let mut at = vec![0u32; n];
    let mut active: Vec<u32> = (0..n as u32)
        .filter(|&i| text.len(i as usize) > 0)
        .collect();
    let mut next = 1u32;
    for d in 1..=longest {
        let mut parent_seen = NONE;
        let mut node = NONE;
        let mut kept = 0;
        for a in 0..active.len() {
            let i = active[a] as usize;
            // A key sharing this depth with the key before it — which is then the previous
            // active one — shares its node.
            if d > lcp[i] as usize {
                let u = next;
                next += 1;
                let c = text.key(i)[d - 1];
                let p = at[i];
                trie.label[u as usize] = c;
                if p != parent_seen {
                    trie.first[p as usize] = u;
                    parent_seen = p;
                }
                let f = if d == 1 {
                    0
                } else {
                    trie.step(trie.fail[p as usize], c)
                };
                trie.fail[u as usize] = f;
                if (is_term[f as usize / 64] >> (f % 64)) & 1 == 1 {
                    let k = terminals[terminals.partition_point(|t| t.0 < f)].1 as usize;
                    if inside[k].0 == NONE {
                        inside[k] = (i as u32, (d - text.len(k)) as u32);
                    }
                }
                if text.len(i) == d {
                    term[i] = u;
                    terminals.push((u, i as u32));
                    is_term[u as usize / 64] |= 1 << (u % 64);
                }
                node = u;
            }
            at[i] = node;
            if text.len(i) > d {
                active[kept] = i as u32;
                kept += 1;
            }
        }
        active.truncate(kept);
        // Depth `d - 1` has all its children now: a node without any gets the start of the next
        // one's, so every run ends where the next begins.
        let mut end = next;
        for v in (trie.level[d - 1]..trie.level[d]).rev() {
            match trie.first[v as usize] {
                NONE => trie.first[v as usize] = end,
                s => end = s,
            }
        }
        // The first node of depth `d` is where its children, at `d + 1`, will start, and it
        // bounds the last run of depth `d - 1`, which the next sweep's links may read.
        trie.first[trie.level[d] as usize] = next;
        trie.level.push(next);
    }
    debug_assert_eq!(next as usize, nodes);
    let last = trie.level[trie.level.len() - 2];
    for v in last..next {
        trie.first[v as usize] = next;
    }
    trie.first[nodes] = next;
    // A key that is a proper prefix of another is a prefix of the key right after it.
    for i in 0..n.saturating_sub(1) {
        if lcp[i + 1] as usize == text.len(i) {
            inside[i] = (i as u32 + 1, 0);
        }
    }
    Some(Sweep { trie, term, inside })
}

/// The greedy chains over the free keys: `(next, overlap)` per key, `NONE` where a chain ends.
fn chain(text: &Text, sweep: &Sweep, slot_of: &[u32]) -> Vec<(u32, u32)> {
    let n = slot_of.len();
    let trie = &sweep.trie;
    let free = |k: usize| sweep.inside[k].0 == NONE;
    let mut link = vec![(NONE, 0u32); n];
    // A chain's head knows its tail and its tail its head; a key alone is both.
    let mut other_end: Vec<u32> = (0..n as u32).collect();
    let mut preceded = vec![false; n];
    let depths = trie.level.len() - 1;
    // Keys waiting at each depth for an overlap that long, as linked lists.
    let mut waiting = vec![NONE; depths];
    let mut waiting_next = vec![NONE; n];
    let mut cur = vec![0u32; n];
    for k in 0..n {
        if !free(k) {
            continue;
        }
        let v = trie.fail[sweep.term[k] as usize];
        if v != 0 {
            let d = trie.depth(v);
            cur[k] = v;
            waiting_next[k] = waiting[d];
            waiting[d] = k as u32;
        }
    }
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
    // Free keys by their node, which is node order.
    let mut ending: Vec<(u32, u32)> = (0..n)
        .filter(|&k| free(k))
        .map(|k| (sweep.term[k], k as u32))
        .collect();
    ending.sort_unstable();
    let mut ending_at = ending.len();
    let mut batch: Vec<u32> = Vec::new();
    for d in (1..depths).rev() {
        let (lo, hi) = (trie.level[d], trie.level[d + 1]);
        let child_lo = hi;
        let first_ending = ending.partition_point(|e| e.0 < lo);
        let mut e = first_ending;
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
        ending_at = first_ending;
        batch.clear();
        let mut k = waiting[d];
        while k != NONE {
            batch.push(k);
            k = waiting_next[k as usize];
        }
        batch.sort_unstable_by_key(|&k| slot_of[k as usize]);
        for &s in &batch {
            let v = cur[s as usize];
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
                        let fd = trie.depth(f);
                        cur[s as usize] = f;
                        waiting_next[s as usize] = waiting[fd];
                        waiting[fd] = s;
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
    let mut data = Vec::with_capacity(total);
    let mut starts = vec![u64::MAX; n];
    for &head in order {
        let mut k = head as usize;
        if !free(k) || preceded[k] {
            continue;
        }
        starts[k] = data.len() as u64;
        data.extend_from_slice(text.key(k));
        while link[k].0 != NONE {
            let (t, o) = link[k];
            let t = t as usize;
            starts[t] = (data.len() - o as usize) as u64;
            data.extend_from_slice(&text.key(t)[o as usize..]);
            k = t;
        }
    }
    debug_assert_eq!(data.len(), total);
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
