//! What places a probe among block heads whose samples tie: a trie of eight-byte words over each
//! long run of equal samples, derived at load and held beside the index, in no blob.
//!
//! A [`DictIndex`](crate::DictIndex) routes a probe off one eight-byte sample a block, taken past
//! the prefix every head shares. Where a run of blocks shares the sample as well, the heads
//! themselves were compared, a binary search of whole-key compares. On a million file paths that
//! run is the index: 3 899 of 3 907 samples tie at the default block, the longest run is 3 572
//! blocks, and a lookup made 11.4 head compares, a quarter of its instructions.
//!
//! One more sample level does not end it: paths nest, so the eight bytes past a run's shared
//! prefix tie again on the next directory, and 3 784 of the 3 907 blocks are still tied one level
//! down. So the level recurses. A node covers a run of blocks and holds the prefix `p` their heads
//! share, then one *edge* for each distinct word of eight bytes at `p` among them, in order, with
//! the blocks that word covers: one block, or a run of them that is a node of its own. On paths at
//! the default block that is 1 136 nodes and 5 035 edges, nine levels at the deepest and 4.7 on
//! the average probe.
//!
//! The descent reads nothing but the probe and the trie: at each node it takes the probe's word at
//! `p` and searches the node's edges for it, without asking whether the probe shares the `p` bytes
//! the word is taken past. That is checked once, at the end, against the one head the descent
//! arrived at. A probe that shares the prefix of the last node shares every prefix above it, and
//! the edges placed it; one that does not left the trie at the first node whose prefix it does
//! not share, and it is then below or above all of that node's blocks, as it is below or above
//! the head it was compared with. That leaves one head compare a lookup where there were eleven.
//!
//! A trie pays for a long run and costs a short one. Over a run of a few blocks the head compares
//! read two or three heads that sit side by side, where a descent reads `roots`, a node, and an
//! edge's word, start and kid out of three more arrays. Ten million DNA reads at block 32 hold
//! every one of the 4^8 possible samples, 65 536 runs of 3 to 7 blocks, and tries over all of
//! them — 6.3 MB beside the index — took 50 instructions off a lookup and added 6.4 L2 misses and
//! 2.3 page walks. So only a run of [`MIN_RUN`] blocks or more is given a trie, which leaves that
//! index none and its lookups counting what they counted before the tries. Eight is where the
//! counters turn on ten million English titles at block 32, four passes over every key: tries over
//! runs of 2 and 3 blocks cost 10 instructions and 2.2 L2 misses a lookup, over runs of 4 to 7 they
//! are a wash, and from 8 up every run left to the head compares costs instructions — 5 a lookup at
//! a minimum of 16, 15 at 32, 28 at 64 — and saves no misses. URLs count the same instructions;
//! their page walks, the noisiest counter here, run the other way, 0.45 a lookup more at 8 than at
//! 32. The paths the tries were built for lose only their shortest run, 2 of 3 907 blocks at the
//! default block, and a lookup there still counts 3 932 instructions where the head compares took
//! 4 696.
//!
//! Runs shorter than [`MIN_RUN`] aside, only a run whose words do not split — heads that differ
//! past `p` by NUL bytes a shorter head's padding matches — and a node [`MAX_DEPTH`] levels down
//! leave their blocks to the head compares.

use crate::dict_index::{lcp, sample_at};

/// An edge that covers one block.
const LEAF: u32 = u32::MAX;
/// An edge that covers several blocks the trie does not split, whose heads are compared.
const FLAT: u32 = u32::MAX - 1;
/// Levels a run is split into before its sub-runs are left to the head compares. It bounds the
/// derivation at this many passes over the blocks and a descent at this many nodes: a legal key
/// set can nest deeper than any real one — heads that each extend the one before by a byte — and
/// paths, the deepest measured, reach nine.
const MAX_DEPTH: usize = 32;
/// Blocks a run of equal samples must span to be given a trie; a shorter run's heads are compared.
/// Eight is where a lookup's counters turn, as the module notes say.
pub(crate) const MIN_RUN: usize = 8;

/// A run of blocks: the prefix their heads share, its edges, and where the run ends.
#[derive(Clone, Copy, Default)]
struct Node {
    p: u32,
    first: u32,
    count: u32,
    hi: u32,
}

/// Where a descent stopped, before the head it arrived at confirms it.
enum End {
    /// Between two edges, or past the last: the boundary itself.
    At(usize),
    /// On an edge of one block, whose head decides between it and the block after.
    Leaf(usize),
    /// On an edge of several blocks the trie does not split, `[a, c)`.
    Flat(usize, usize),
}

/// The tries of every run of [`MIN_RUN`] or more equal samples that splits, one root a run.
#[derive(Default)]
pub(crate) struct Ties {
    /// The first block of each run, ascending; the root of `roots[i]`'s run is node `i`.
    roots: Vec<u32>,
    nodes: Vec<Node>,
    /// Each node's edges, `first..first + count`: the word, ascending ...
    words: Vec<u64>,
    /// ... the first block the edge covers ...
    starts: Vec<u32>,
    /// ... and the node those blocks are, or [`LEAF`] or [`FLAT`].
    kids: Vec<u32>,
}

impl Ties {
    /// The tries over the heads `head(b)` whose samples are `samples`: `None` where no run of
    /// [`MIN_RUN`] or more equal samples splits, and past what a `u32` numbers.
    pub(crate) fn derive<'h>(
        samples: &[u64],
        head: impl Fn(usize) -> &'h [u8],
    ) -> Option<Box<Self>> {
        let mut ties = Ties::default();
        let nb = samples.len();
        u32::try_from(nb).ok()?;
        // The prefix `first` and `last` share, where the words past it split the heads between
        // them: those are in order, so the first and the last word differ exactly when two of
        // them do. Heads that differ past it only by NUL bytes a shorter one's padding matches do
        // not split.
        let splits = |first: &[u8], last: &[u8]| {
            let p = lcp(first, last);
            let p32 = u32::try_from(p).ok()?;
            (sample_at(first, p) != sample_at(last, p)).then_some(p32)
        };
        // The roots first, so that run `r`'s root is node `r`. Most indexes have no such run, and
        // a scan for the next sample equal to the one `MIN_RUN - 1` past it is what it costs
        // them: the samples are in order, so the two are equal exactly when the `MIN_RUN` from
        // the first on all are, and the first such sample is where its run starts. A probe makes
        // the same test.
        let mut at = 0;
        while let Some(k) = samples[at..]
            .windows(MIN_RUN)
            .position(|w| w[0] == w[MIN_RUN - 1])
        {
            let lo = at + k;
            let hi = lo
                + MIN_RUN
                + samples[lo + MIN_RUN..]
                    .iter()
                    .take_while(|&&s| s == samples[lo])
                    .count();
            if let Some(p) = splits(head(lo), head(hi - 1)) {
                ties.roots.push(lo as u32);
                ties.nodes.push(Node {
                    p,
                    hi: hi as u32,
                    ..Node::default()
                });
            }
            at = hi;
        }
        if ties.roots.is_empty() {
            return None;
        }
        // Then each root's trie, breadth first, so that a node's number is known when its
        // parent's edge is written. A run's heads are read once, not once a level. A run of `k`
        // blocks has at least `k` edges and a real one seldom many more, so that is what the
        // edges are given room for; the scratch is the longest run.
        let runs = (0..ties.roots.len()).map(|r| (ties.nodes[r].hi - ties.roots[r]) as usize);
        let (tied, longest) = runs.fold((0, 0), |(t, l), k| (t + k, l.max(k)));
        ties.words.reserve_exact(tied);
        ties.starts.reserve_exact(tied);
        ties.kids.reserve_exact(tied);
        let (mut heads, mut words) = (Vec::with_capacity(longest), Vec::with_capacity(longest));
        let mut queue = std::collections::VecDeque::new();
        for r in 0..ties.roots.len() {
            let (base, end) = (ties.roots[r] as usize, ties.nodes[r].hi as usize);
            heads.clear();
            heads.extend((base..end).map(&head));
            queue.push_back((r, ties.nodes[r].p, base, end, 1));
            while let Some((i, p, lo, hi, depth)) = queue.pop_front() {
                let run = &heads[lo - base..hi - base];
                words.clear();
                words.extend(run.iter().map(|h| sample_at(h, p as usize)));
                let first = ties.words.len() as u32;
                let mut a = 0;
                while a < words.len() {
                    let c = a + words[a..].iter().take_while(|&&w| w == words[a]).count();
                    let kid = match c - a {
                        1 => LEAF,
                        _ if depth >= MAX_DEPTH => FLAT,
                        _ => match splits(run[a], run[c - 1]) {
                            Some(p) => {
                                ties.nodes.push(Node::default());
                                let kid = ties.nodes.len() - 1;
                                queue.push_back((kid, p, lo + a, lo + c, depth + 1));
                                kid as u32
                            }
                            None => FLAT,
                        },
                    };
                    ties.push_edge(words[a], lo + a, kid);
                    a = c;
                }
                ties.nodes[i] = Node {
                    p,
                    first,
                    count: ties.words.len() as u32 - first,
                    hi: hi as u32,
                };
            }
        }
        ties.roots.shrink_to_fit();
        ties.nodes.shrink_to_fit();
        ties.words.shrink_to_fit();
        ties.starts.shrink_to_fit();
        ties.kids.shrink_to_fit();
        Some(Box::new(ties))
    }

    fn push_edge(&mut self, word: u64, start: usize, kid: u32) {
        self.words.push(word);
        self.starts.push(start as u32);
        self.kids.push(kid);
    }

    /// The root of the run that starts at block `lo`, if it is one.
    #[inline]
    pub(crate) fn root(&self, lo: usize) -> Option<usize> {
        let lo = u32::try_from(lo).ok()?;
        self.roots.binary_search(&lo).ok()
    }

    /// Where edge `j` of `node` ends: where the next one starts, or where the node does.
    #[inline(always)]
    fn end_of(&self, node: Node, j: usize) -> usize {
        if j + 1 < node.count as usize {
            self.starts[node.first as usize + j + 1] as usize
        } else {
            node.hi as usize
        }
    }

    /// The edge of `node` whose word is the probe's at the node's prefix, or where it would go.
    #[inline(always)]
    fn edge(&self, node: Node, probe: &[u8]) -> (usize, bool) {
        let first = node.first as usize;
        let words = &self.words[first..first + node.count as usize];
        let w = sample_at(probe, node.p as usize);
        let j = words.partition_point(|&x| x < w);
        (j, words.get(j) == Some(&w))
    }

    /// The first block of the run rooted at `root` whose head is above `probe`, or the run's end.
    /// `head` reads a block's head; `flat(a, c)` compares heads over a run the trie does not split.
    ///
    /// With it, where the head the descent compared the probe with is the one just below that
    /// boundary, the bytes the two share and that head's length: the block's scan starts from
    /// both, and would otherwise read the head and compare it again.
    #[inline]
    pub(crate) fn boundary<'h>(
        &self,
        root: usize,
        probe: &[u8],
        head: impl Fn(usize) -> &'h [u8],
        flat: impl FnOnce(usize, usize) -> usize,
    ) -> (usize, Option<(usize, usize)>) {
        let mut node = self.nodes[root];
        let (end, at) = loop {
            let (j, equal) = self.edge(node, probe);
            let first = node.first as usize;
            if !equal {
                let lo = self.starts[first] as usize;
                let l = if j < node.count as usize {
                    self.starts[first + j] as usize
                } else {
                    node.hi as usize
                };
                break (End::At(l), if l > lo { l - 1 } else { lo });
            }
            let a = self.starts[first + j] as usize;
            match self.kids[first + j] {
                LEAF => break (End::Leaf(a), a),
                FLAT => break (End::Flat(a, self.end_of(node, j)), a),
                kid => node = self.nodes[kid as usize],
            }
        };
        let h = head(at);
        let shared = lcp(probe, h);
        let below = match (probe.get(shared), h.get(shared)) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x < y,
        };
        let l = if shared < node.p as usize {
            self.correct(root, probe, shared, below)
        } else {
            match end {
                End::At(l) => l,
                End::Leaf(b) => b + usize::from(!below),
                End::Flat(a, c) => flat(a, c),
            }
        };
        (l, (l == at + 1).then_some((shared, h.len())))
    }

    /// The boundary for a probe that left the trie inside a prefix: the first node on its path
    /// whose prefix it does not share — `shared` bytes are all it shares with the head it was
    /// compared with, which is under every node of the path — has all of its blocks on the side
    /// of the probe that head is on.
    #[cold]
    fn correct(&self, root: usize, probe: &[u8], shared: usize, below: bool) -> usize {
        let mut node = self.nodes[root];
        let (mut lo, mut hi) = (self.starts[node.first as usize] as usize, node.hi as usize);
        while node.p as usize <= shared {
            let (j, equal) = self.edge(node, probe);
            if !equal {
                break;
            }
            let first = node.first as usize;
            (lo, hi) = (self.starts[first + j] as usize, self.end_of(node, j));
            match self.kids[first + j] {
                LEAF | FLAT => break,
                kid => node = self.nodes[kid as usize],
            }
        }
        if below { lo } else { hi }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The boundary a binary search over every head gives, which is what the tries must give.
    fn plain(heads: &[Vec<u8>], probe: &[u8]) -> usize {
        heads.partition_point(|h| h.as_slice() <= probe)
    }

    /// Every run of equal samples of `heads` at `g`, placed by the tries against the plain search.
    /// How many probes a trie placed.
    fn check(heads: &[Vec<u8>], g: usize, probes: &[Vec<u8>]) -> usize {
        let samples: Vec<u64> = heads.iter().map(|h| sample_at(h, g)).collect();
        let ties = Ties::derive(&samples, |b| &heads[b]);
        let ties = ties.as_deref();
        let mut placed = 0;
        for probe in probes {
            if probe.get(..g) != heads[0].get(..g) {
                continue;
            }
            let s = sample_at(probe, g);
            let lo = samples.partition_point(|&x| x < s);
            let hi = samples.partition_point(|&x| x <= s);
            if hi - lo < 2 {
                continue;
            }
            let flat = |a: usize, c: usize| a + plain(&heads[a..c], probe);
            // A run that does not split, or is shorter than `MIN_RUN`, has no root, and its heads
            // are compared.
            let root = ties.and_then(|t| Some((t, t.root(lo)?)));
            assert!(root.is_none() || hi - lo >= MIN_RUN, "a run of {}", hi - lo);
            let (got, known) = match root {
                Some((ties, root)) => {
                    placed += 1;
                    ties.boundary(root, probe, |b| &heads[b], flat)
                }
                None => (flat(lo, hi), None),
            };
            assert_eq!(got, plain(heads, probe), "probe {probe:?}");
            if let Some((shared, len)) = known {
                let below = &heads[got - 1];
                assert_eq!(
                    (shared, len),
                    (lcp(probe, below), below.len()),
                    "probe {probe:?}"
                );
            }
        }
        placed
    }

    fn probes_of(heads: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for h in heads {
            out.push(h.clone());
            for cut in 0..h.len() {
                out.push(h[..cut].to_vec());
                let mut up = h[..=cut].to_vec();
                up[cut] = up[cut].wrapping_add(1);
                out.push(up);
                let mut down = h[..=cut].to_vec();
                down[cut] = down[cut].wrapping_sub(1);
                out.push(down);
            }
            for tail in [&b"\0"[..], b"\0\0", b"a", b"\xff", b"/"] {
                out.push([h.as_slice(), tail].concat());
            }
        }
        out
    }

    #[test]
    fn nested_paths_place_every_probe_as_a_plain_search_does() {
        let mut heads: Vec<Vec<u8>> = Vec::new();
        for a in [
            "home/ilgrad/Documents",
            "home/ilgrad/.cache",
            "usr/share/doc",
            "usr/lib64",
        ] {
            for b in [
                "projects/betula",
                "projects/spacelab",
                "x",
                "deeply/nested/dir/one",
            ] {
                for c in 0..5 {
                    heads.push(format!("/{a}/{b}/file{c}.rs").into_bytes());
                }
            }
        }
        heads.sort();
        heads.dedup();
        let probes = probes_of(&heads);
        assert!(check(&heads, 1, &probes) > 0);
        assert!(check(&heads, 0, &probes) > 0);
    }

    #[test]
    fn nul_bytes_and_prefixes_of_heads_are_placed_too() {
        let mut heads: Vec<Vec<u8>> = vec![
            b"ab".to_vec(),
            b"ab\0".to_vec(),
            b"ab\0\0".to_vec(),
            b"ab\0\0\0\0\0\0\0\0x".to_vec(),
            b"ab\0\0\0\0\0\0\0\0y".to_vec(),
            b"abcdefghij".to_vec(),
            b"abcdefghijk".to_vec(),
            b"abcdefghijkl\0".to_vec(),
            b"abcdefghijklmnopqrstuvwxyz".to_vec(),
            b"abcdefghijklmnopqrstuvwxyz0".to_vec(),
            b"abcdefghijklmnopqrstuvwxyz1".to_vec(),
        ];
        // Both runs made longer than `MIN_RUN` by heads past their last: the first still does not
        // split, and the second has a trie.
        for i in 0..MIN_RUN {
            heads.push(format!("ab\0\0\0\0\0\0\0\0y{i:03}").into_bytes());
            heads.push(format!("abcdefghijklmnopqrstuvwxyz1{i:03}").into_bytes());
        }
        heads.sort();
        let probes = probes_of(&heads);
        assert!(check(&heads, 0, &probes) > 0);
    }

    #[test]
    fn a_chain_deeper_than_the_cap_still_places_every_probe() {
        let heads: Vec<Vec<u8>> = (1..=3 * MAX_DEPTH)
            .map(|k| std::iter::repeat_n(b'a', 8 * k).collect())
            .collect();
        let probes = probes_of(&heads);
        assert!(check(&heads, 0, &probes) > 0);
    }

    #[test]
    fn a_run_one_block_short_of_the_minimum_has_no_trie_and_is_placed_all_the_same() {
        // Two runs whose heads split past the sample they share, the second exactly `MIN_RUN`
        // blocks long and the last in the index.
        let mut heads: Vec<Vec<u8>> = Vec::new();
        for (sample, k) in [("a short ", MIN_RUN - 1), ("b long r", MIN_RUN)] {
            heads.extend((0..k).map(|i| format!("{sample}/{i:04}").into_bytes()));
        }
        let samples: Vec<u64> = heads.iter().map(|h| sample_at(h, 0)).collect();
        let ties = Ties::derive(&samples, |b| &heads[b]).expect("the long run splits");
        assert!(ties.root(0).is_none());
        assert!(ties.root(MIN_RUN - 1).is_some());
        assert!(check(&heads, 0, &probes_of(&heads)) > 0);
    }

    #[test]
    fn unsorted_heads_derive_and_answer_without_panicking() {
        // Repeated to a run at least `MIN_RUN` long, so that it is given a trie.
        let heads: Vec<Vec<u8>> = [&b"zz"[..], b"aa", b"", b"zzzzzzzzzq", b"zz", b"a\0"]
            .repeat(MIN_RUN.div_ceil(6))
            .into_iter()
            .map(|h| h.to_vec())
            .collect();
        let samples: Vec<u64> = vec![7; heads.len()];
        let ties = Ties::derive(&samples, |b| &heads[b]).expect("the first and last heads differ");
        let root = ties.root(0).expect("the run starts at block 0");
        for probe in probes_of(&heads) {
            let (got, _) = ties.boundary(root, &probe, |b| &heads[b], |a, _| a);
            assert!(got <= heads.len());
        }
    }
}
