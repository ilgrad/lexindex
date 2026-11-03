//! Deterministic, **version-stable** key hashes shared by the hash indexes.
//!
//! Stability across Rust versions and platforms is what lets a *serialised* MPH be reloaded and
//! queried — `std`'s `DefaultHasher` is explicitly not guaranteed stable, so it cannot back
//! persistence. Every word is read with `from_le_bytes`, so a blob written on one endianness reads
//! the same on the other.
//!
//! Both hashes read a key the same way — into a 128-bit [`state`] — and differ only in the
//! constants they [`merge`] it with, so [`hash_pair_bytes`] reads the key once. A key of 4..=16
//! bytes is two words from four overlapping 4-byte loads, 17..=32 bytes its first and last sixteen,
//! and a longer key runs 32-byte blocks and then one round over what is left. Below 4 bytes, one
//! word holds a fan-out of them. No loop and no branch on the length inside a class, so a stream of
//! keys in one class costs no branch mispredictions, where the hash 2.0–3.x shipped — one multiply
//! per 8-byte word, in a loop, then a tail switch — lost 6 of its 9.5 ns to them.
//!
//! Two distinct keys share both hashes only if their states collide, or two different states merge
//! alike: ≈ 2^-127 a pair of random keys. 4.x was at ≈ 2^-64 up to 16 bytes, where both hashes were
//! functions of one folded product, and a birthday search on that product found a chosen pair in
//! ≈ 2^32 steps (`DOUBLE_COLLIDING_PAIR`); above 32 bytes a zero factor erased the bytes beside it.
//! What shapes the state, each point a collision found by trying the simpler form:
//!
//! - **No product is folded before the merge.** A step that folds 128 free key bits to 64 collides
//!   in 2^32 steps whatever its constants: hold the rest of the key still and search on it alone.
//! - **Every word reaches two products.** An unfolded product can be refactored,
//!   `x * y == (x / 3) * (y * 3)`, and two different products over four words leave one to vary
//!   while the other stands still. A fifth word read by the third product alone gives the two others
//!   a free compensation in it, and a 2-dimensional lattice then cancels their linear change.
//! - **`r`'s factors are not a XOR of the words.** With `x = w0 ^ w2`, `y = w1 ^ w3`, swapping both
//!   products' factors at once keeps `p`, `q` and `r`: `(X, Y, U, U ^ X ^ Y)` against
//!   `(Y, X, U ^ X ^ Y, U)` under their masks collide for every `X`, `Y`, `U`, and a rotation only
//!   moves it to low-weight factor pairs `(s, s + 1)`. A multiply by an odd constant turns such a
//!   low-weight change into a word-wide one.
//! - **`lo ^ hi` is `!0` for every product divisible by 2^64 − 1**, whatever the factors, and that
//!   number has 128 divisors. An unfolded product has no such value: `x * y` is injective in `y` for
//!   any `x != 0`.
//! - **Text cannot spell a masked factor to zero**: the masks of `p` and `q` hold byte pairs no UTF-8
//!   has, a test checks it. So the state enters only `r`, which [`protected`] keeps from losing it. A
//!   crafted *byte* query can reach those zeros, and buys what any stranger's query can: a false
//!   positive.
//! - **One side of the state sums and the other XORs**, so that two equal products neither cancel
//!   nor fold into 64 bits.
//!
//! This is no proof, and not a cryptographic hash. The rounds have algebra left in them; a search
//! below the 2^64 of a birthday may exist, and none is known. Keys a third party chooses still
//! belong in a `PerfectHashIndex` or an ordered index.
//!
//! The constants are consecutive 64-bit words of π's fraction — nothing up the sleeve (PARI/GP:
//! `frac(Pi)` scaled by 2^64, the first word `0x243F6A8885A308D3`) — keeping the odd ones with 27
//! to 35 bits set. Different constants in every position matter: one mask for `p`'s and `q`'s factors
//! would let a key swap the two products outright.

/// The slot hash's constants: the masks of the factors of `p` and `q`, the merge's two, and the state
/// at the start of a key.
const SLOT: [u64; 8] = [
    0x243f_6a88_85a3_08d3,
    0x082e_fa98_ec4e_6c89,
    0x4528_21e6_38d0_1377,
    0xc0ac_29b7_c97c_50dd,
    0x3f84_d5b5_b547_0917,
    0x9216_d5d9_8979_fb1b,
    0xba7c_9045_f12c_7f99,
    0x24a1_9947_b391_6cf7,
];
/// The fingerprint's merge constants: two more such words, in the roles of `SLOT[4]` and
/// `SLOT[5]`.
const FP: [u64; 2] = [0xd715_77c1_bd31_4b27, 0xa154_86af_7c72_e993];
/// The state at the start of a key.
const START: (u64, u64) = (SLOT[7], SLOT[6]);

/// The full 128-bit product of two words as `(lo, hi)`. One `mul` on a 64-bit machine and a short
/// software product on a 32-bit one, the same value everywhere.
#[inline(always)]
fn wide(a: u64, b: u64) -> (u64, u64) {
    let p = a as u128 * b as u128;
    (p as u64, (p >> 64) as u64)
}

/// A state word and its masks into one output word: the product's halves folded, its operands added
/// back. The fold alone is `!0` at `y == u64::MAX` and `0` at `y == 0`, whatever `x`, which would
/// leave one 64-bit word to carry both hashes. Added back, `x` survives both, and no `y` cancels it:
/// an XOR would, at `y == 1`, where the fold is `x` itself.
#[inline(always)]
fn merge(x: u64, y: u64) -> u64 {
    let (lo, hi) = wide(x, y);
    (lo ^ hi).wrapping_add(x).wrapping_add(y)
}

/// Four bytes at `i`, little-endian, without a bounds check: every index the words below use
/// is in bounds by construction, and the six checks LLVM could not prove from `d = (n / 8) * 4`
/// and `n - p > 32` cost 6–16 % of the hash on real keys (`local/hashbench`, A-B-A-B).
///
/// # Safety
///
/// `i + 4 <= b.len()`.
#[inline(always)]
unsafe fn r4(b: &[u8], i: usize) -> u64 {
    debug_assert!(i + 4 <= b.len());
    // SAFETY: the caller's bound; a `[u8; 4]` has no alignment.
    u32::from_le_bytes(unsafe { *(b.as_ptr().add(i) as *const [u8; 4]) }) as u64
}

/// Eight bytes at `i`, little-endian, without a bounds check.
///
/// # Safety
///
/// `i + 8 <= b.len()`.
#[inline(always)]
unsafe fn r8(b: &[u8], i: usize) -> u64 {
    debug_assert!(i + 8 <= b.len());
    // SAFETY: the caller's bound; a `[u8; 8]` has no alignment.
    u64::from_le_bytes(unsafe { *(b.as_ptr().add(i) as *const [u8; 8]) })
}

/// The two words of up to 16 bytes. From 4 bytes, four 4-byte loads at 0, `d`, `n - 4` and
/// `n - 4 - d`, where `d = ⌊n / 8⌋ · 4`: below 8 bytes `d` is 0 and the first and last four bytes
/// cover the key, to 15 it is 4 and the first and last eight do, at 16 it is 8 and the loads are the
/// four quarters — so at every length every byte is read, and with the length the words determine
/// the key. Below 4 bytes, `b[0]`, `b[n / 2]` and `b[n - 1]` in separate octets of one word, again
/// injective at each length, and all zeros for the empty key, which the length folded into the merge
/// keeps apart from `"\0"`.
#[inline(always)]
fn two_words(b: &[u8]) -> (u64, u64) {
    let n = b.len();
    debug_assert!(n <= 16);
    if n >= 4 {
        let d = (n >> 3) << 2;
        // SAFETY: n is 4..=16, so d is 0 below 8, 4 below 16 and 8 at 16: d + 4 <= n and
        // n - 4 - d >= 0 at every n, and the loads at 0 and n - 4 need only n >= 4.
        return unsafe {
            (
                r4(b, 0) | (r4(b, d) << 32),
                r4(b, n - 4) | (r4(b, n - 4 - d) << 32),
            )
        };
    }
    if n == 0 {
        return (0, 0);
    }
    (
        (b[0] as u64) | ((b[n / 2] as u64) << 16) | ((b[n - 1] as u64) << 32),
        0,
    )
}

/// The four words of 17..=32 bytes: the first and the last sixteen, which overlap below 32.
#[inline(always)]
fn four_words(b: &[u8]) -> [u64; 4] {
    let n = b.len();
    debug_assert!((17..=32).contains(&n));
    // SAFETY: n is 17..=32, so the loads at 8 and n - 16 leave eight bytes each.
    unsafe { [r8(b, 0), r8(b, 8), r8(b, n - 16), r8(b, n - 8)] }
}

/// The last product of a round and what it adds back, `(x * y + x, hi + y)`. At any fixed `y` this
/// still holds `x` (at `y == 1`, all of it but the top bit), and at any fixed `x` holds `y`, where a
/// plain product at a zero factor would lose the other. This is the one product whose factors a key
/// can spell to zero, and the one that carries the state.
#[inline(always)]
fn protected(x: u64, y: u64) -> (u64, u64) {
    let (lo, hi) = wide(x, y);
    (lo.wrapping_add(x), hi.wrapping_add(y))
}

/// Up to 16 bytes at the start of a key, where the state is a constant, nothing is there to lose,
/// and the masks keep `x` and `y` of the two products apart.
#[inline(always)]
fn round2(w: (u64, u64)) -> (u64, u64) {
    let (pl, ph) = wide(w.0 ^ SLOT[0], w.1 ^ SLOT[1]);
    let (rl, rh) = wide(w.0 ^ START.1, w.1 ^ START.0);
    (pl ^ rh, ph ^ rl)
}

/// [`round2`] with a state to keep: up to 16 bytes after other rounds.
#[inline(always)]
fn round2_chained(w: (u64, u64), s: (u64, u64)) -> (u64, u64) {
    let (pl, ph) = wide(w.0 ^ SLOT[0], w.1 ^ SLOT[1]);
    let (rl, rh) = protected(w.0 ^ s.1, w.1 ^ s.0);
    (pl ^ rh, ph ^ rl)
}

/// Four words and a state into the next state: three products, every word in two of them. A change
/// in the state is a change in a factor of `r`, kept by [`protected`], and to undo it a word has to
/// move `p` or `q`, whose other factor text cannot zero.
#[inline(always)]
fn round(w: [u64; 4], s: (u64, u64)) -> (u64, u64) {
    let (u, v) = (w[2] ^ SLOT[2], w[3] ^ SLOT[3]);
    let (x, y) = (
        w[0] ^ u.wrapping_mul(SLOT[3]) ^ s.1,
        w[1] ^ v.wrapping_mul(SLOT[2]) ^ s.0,
    );
    let (pl, ph) = wide(w[0] ^ SLOT[0], w[1] ^ SLOT[1]);
    let (ql, qh) = wide(u, v);
    let (a, c) = (pl.wrapping_add(qh), ph ^ ql);
    let (rl, rh) = protected(x, y);
    (a ^ rh, c.wrapping_add(rl))
}

/// The 128-bit state of any key: one round to 32 bytes, or a round over each leading 32-byte block
/// and one over what is left.
#[inline(always)]
fn state(b: &[u8]) -> (u64, u64) {
    let n = b.len();
    if n > 32 {
        let mut s = START;
        let mut rest = b;
        while rest.len() > 32 {
            // SAFETY: rest holds more than 32 bytes, so every load below leaves eight.
            let w = unsafe { [r8(rest, 0), r8(rest, 8), r8(rest, 16), r8(rest, 24)] };
            s = round(w, s);
            rest = &rest[32..];
        }
        return if rest.len() > 16 {
            round(four_words(rest), s)
        } else {
            round2_chained(two_words(rest), s)
        };
    }
    if n > 16 {
        round(four_words(b), START)
    } else {
        round2(two_words(b))
    }
}

/// The **slot** hash: what the perfect hash indexes by. Structured keys like `"key_0001"` differ
/// only in a few bytes, which the merge spreads across all 64 output bits before the MPH ever
/// sees them.
///
/// The length is not optional: a short key's words are zero-padded, so without it `"a"` and
/// `"a\0"` — both legal `&str` — would hash identically.
#[inline]
pub(crate) fn hash_key(s: &str) -> u64 {
    hash_key_bytes(s.as_bytes())
}

/// [`hash_key`] over the key's bytes: what a lookup reading an Arrow buffer holds.
#[inline]
pub fn hash_key_bytes(b: &[u8]) -> u64 {
    let (a, c) = state(b);
    merge(a ^ b.len() as u64 ^ SLOT[4], c ^ SLOT[5])
}

/// The **fingerprint** hash: a second 64-bit function of the key, uncorrelated with [`hash_key`]
/// for well-distributed keys — it merges the same state under its own constants and the length
/// rotated, so it costs one multiply over the hash (see [`hash_pair`]).
/// That decorrelation is what makes the chance a non-member both lands on a used slot and matches
/// the low `b` bits stored for that slot about `2^-b` — the tunable false-positive rate. It is an upper bound, not an equality: a non-member whose raw slot falls
/// past the remap is rejected before the fingerprint is ever compared. Not a security primitive —
/// both hashes are deterministic and unseeded, so an adversary who picks the queries can search for
/// collisions.
///
/// Callers keep all 64 bits and truncate to the table width themselves; the collision side table
/// stores the full value, so two distinct keys merge only when they collide in *both* 64-bit hashes
/// at once — ~`2^-127` a pair of random keys of any length, not the `2^-(64+b)` a truncated side
/// match would allow.
#[inline]
pub(crate) fn fingerprint_full(s: &str) -> u64 {
    fingerprint_full_bytes(s.as_bytes())
}

/// [`fingerprint_full`] over the key's bytes: what a lookup reading an Arrow buffer holds.
#[inline]
pub(crate) fn fingerprint_full_bytes(b: &[u8]) -> u64 {
    hash_pair_bytes(b).1
}

/// `(hash_key, fingerprint_full)`: the fingerprint is one more merge of the hash's own state, so it
/// costs a multiply and a few additions over the hash, not a second pass over the key. The pair is
/// a 128-bit function of the 128-bit state, on every path: two keys that agree on both hashes agree
/// on the state, at `2^-128` a pair, or merged alike from two states, at `2^-128` too. Nothing here
/// is a security primitive, but no search for such a pair is known below the 2^64 steps of a
/// birthday. Every `CompactHashIndex` path needs both hashes; `PerfectHashIndex::build` keeps using
/// [`hash_key`] alone, and `build_to_file` takes both, the second for its replay digest.
#[inline]
pub(crate) fn hash_pair(s: &str) -> (u64, u64) {
    hash_pair_bytes(s.as_bytes())
}

/// [`hash_pair`] over the key's bytes: what a lookup reading an Arrow buffer holds.
#[inline(always)]
pub fn hash_pair_bytes(b: &[u8]) -> (u64, u64) {
    let len = b.len() as u64;
    let (a, c) = state(b);
    (
        merge(a ^ len ^ SLOT[4], c ^ SLOT[5]),
        merge(a ^ len.rotate_left(32) ^ FP[0], c ^ FP[1]),
    )
}

/// Partition `hashes` (parallel to some key order) into the MPH's key set and the collided
/// leftovers. One representative per distinct hash value — the smallest original index — goes to
/// the MPH; every other member of a colliding group is returned as `(hash, original_index)` in
/// original-index order, so the caller can hand them deterministic tail ids. Almost always the
/// second vector is empty: a 64-bit collision needs ~10^8 keys before its probability is even
/// 10^-4. When it is not, the index still builds — colliding keys are served from a side table
/// instead of failing the build outright (the hash is deterministic, so a retry could never help).
pub(crate) fn split_collisions(hashes: &[u64]) -> (Vec<u64>, Vec<(u64, u32)>) {
    let mut pairs: Vec<(u64, u32)> = hashes
        .iter()
        .enumerate()
        .map(|(i, &h)| (h, i as u32))
        .collect();
    pairs.sort_unstable();
    let mut mph_hashes = Vec::with_capacity(pairs.len());
    let mut extras = Vec::new();
    for p in pairs.chunk_by(|a, b| a.0 == b.0) {
        mph_hashes.push(p[0].0);
        extras.extend_from_slice(&p[1..]);
    }
    extras.sort_unstable_by_key(|&(_, i)| i);
    (mph_hashes, extras)
}

/// Two distinct strings with equal [`hash_key`], found offline by a Pollard-rho birthday search
/// (`local/hashcollide`, ~2^32 map steps). They drive the side-table tests in both MPH indexes;
/// the golden test below pins the collision itself, so a changed hash breaks loudly here before
/// anything subtle happens in tests built on the pair.
#[cfg(test)]
pub(crate) const COLLIDING_PAIR: (&str, &str) = ("rsqnkjlzt2rdf", "u6c46pr2qm33h");

/// Two distinct 16-byte strings equal in **both** hashes under 4.x. To 16 bytes its lane product was
/// a constant, so both hashes were functions of one 64-bit product and the length, and a birthday
/// search on that product found the pair: Brent's cycle search over 16-character hex keys, 3.7·10⁹
/// steps on one core. No 4.x build could tell these keys from one key written twice.
#[cfg(test)]
pub(crate) const DOUBLE_COLLIDING_PAIR: (&str, &str) = ("33b6a90281824ded", "800df3e3dac02350");

/// The keys of the zero-product family of the 4.x hash: this prefix, one of the
/// [`ZERO_FACTOR_BYTES`], this suffix. The prefix's last eight bytes spell the lane state its first
/// 32 left, found by trying 89 066 URLs (`local/2026-10-25-research/h3-verify`).
#[cfg(test)]
const ZERO_FACTOR_PREFIX: &str = "https://00089066.example.com/abcHtUToJGz";
#[cfg(test)]
const ZERO_FACTOR_SUFFIX: &str = "/and/fifty-six/more/bytes/of/an/ordinary/looking/path/xy";
#[cfg(test)]
const ZERO_FACTOR_BYTES: [&str; 6] = [
    "AAAAAAAA", "victim01", "attacker", "zzzzzzzz", "12345678", "--------",
];

#[cfg(test)]
mod golden {
    use super::{fingerprint_full, hash_key};

    #[test]
    fn the_pinned_collision_pair_still_collides() {
        let (a, b) = super::COLLIDING_PAIR;
        assert_ne!(a, b);
        assert_eq!(hash_key(a), hash_key(b));
        assert_eq!(hash_key(a), 0xed29_8489_4a3b_1962);
        // The pair collides in the slot hash only — the independent fingerprint tells them apart.
        assert_ne!(fingerprint_full(a), fingerprint_full(b));
    }

    /// Equal in both hashes under every hash 4.0 to 4.x computed, where up to 16 bytes both were
    /// functions of one 64-bit product.
    #[test]
    fn the_pair_that_shared_one_64_bit_product_no_longer_collides() {
        let (a, b) = super::DOUBLE_COLLIDING_PAIR;
        assert_ne!(a, b);
        let ((ha, fa), (hb, fb)) = (super::hash_pair(a), super::hash_pair(b));
        assert!(ha != hb && fa != fb, "{a:?} {b:?}: {ha:#x} {fa:#x}");
    }

    /// Six keys that shared one `(hash, fingerprint)` under 4.x: their second block starts with
    /// the lane state the first one left, which zeroed that lane's product, so bytes 40..48 reached
    /// neither hash.
    #[test]
    fn the_family_a_zero_product_erased_no_longer_collides() {
        let mut seen = Vec::new();
        for free in super::ZERO_FACTOR_BYTES {
            let key = format!(
                "{}{free}{}",
                super::ZERO_FACTOR_PREFIX,
                super::ZERO_FACTOR_SUFFIX
            );
            assert_eq!(key.len(), 104);
            seen.push(super::hash_pair(&key));
        }
        for (i, x) in seen.iter().enumerate() {
            for y in &seen[i + 1..] {
                assert!(x.0 != y.0 && x.1 != y.1, "{x:x?} {y:x?}");
            }
        }
    }

    /// Every key of a family reads apart from every other in both hashes.
    fn all_apart(what: &str, keys: &[Vec<u8>]) {
        let hashes: Vec<(u64, u64)> = keys.iter().map(|k| super::hash_pair_bytes(k)).collect();
        for (i, x) in hashes.iter().enumerate() {
            for (j, y) in hashes.iter().enumerate().skip(i + 1) {
                assert!(
                    keys[i] == keys[j] || (x.0 != y.0 && x.1 != y.1),
                    "{what}: {:x?} and {:x?} share a hash",
                    keys[i],
                    keys[j]
                );
            }
        }
    }

    /// The bytes of the 16-byte key whose words are `w`: the inverse of `two_words` at that length.
    fn key16(w: (u64, u64)) -> Vec<u8> {
        let mut k = Vec::with_capacity(16);
        k.extend_from_slice(&(w.0 as u32).to_le_bytes());
        k.extend_from_slice(&((w.1 >> 32) as u32).to_le_bytes());
        k.extend_from_slice(&((w.0 >> 32) as u32).to_le_bytes());
        k.extend_from_slice(&(w.1 as u32).to_le_bytes());
        assert_eq!(super::two_words(&k), w);
        k
    }

    fn key_of_words(words: &[u64]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A factor at 0, 1 or `!0` is what a product can lose its other factor to.
    const DEGENERATE: [u64; 3] = [0, 1, u64::MAX];
    /// What the other factor varies over: a bit at a time, and two ordinary words.
    fn others() -> Vec<u64> {
        (0..64)
            .map(|i| 1u64 << i)
            .chain([0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210])
            .collect()
    }

    /// Up to 16 bytes both products read both words, so a degenerate factor in one leaves the other
    /// to hold them. Only bytes reach these factors; see `no_text_spells_a_mask`.
    #[test]
    fn a_degenerate_factor_loses_nothing_up_to_16_bytes() {
        use super::{SLOT, START};
        for d in DEGENERATE {
            for (name, mask0, mask1) in [("p", SLOT[0], SLOT[1]), ("r", START.1, START.0)] {
                let first: Vec<Vec<u8>> = others().iter().map(|&o| key16((d ^ mask0, o))).collect();
                all_apart(&format!("{name} first {d:#x}"), &first);
                let second: Vec<Vec<u8>> =
                    others().iter().map(|&o| key16((o, d ^ mask1))).collect();
                all_apart(&format!("{name} second {d:#x}"), &second);
            }
        }
    }

    /// `p`, `q` and `r` each at a degenerate factor, over 32 bytes: the other words still reach
    /// two products, so nothing they hold goes.
    #[test]
    fn a_degenerate_factor_loses_nothing_up_to_32_bytes() {
        use super::{SLOT, START};
        for d in DEGENERATE {
            let mut families: Vec<(&str, Vec<Vec<u8>>)> = Vec::new();
            for &free in &[0x1111_1111_1111_1111u64, 0x2222_2222_2222_2222] {
                let fixed = |w: [u64; 4]| key_of_words(&w);
                families.push((
                    "p first",
                    others()
                        .iter()
                        .map(|&o| fixed([d ^ SLOT[0], o, free, free]))
                        .collect(),
                ));
                families.push((
                    "q second",
                    others()
                        .iter()
                        .map(|&o| fixed([o, free, free, d ^ SLOT[3]]))
                        .collect(),
                ));
                // The word that makes `x` the degenerate value, for each `w[2]`.
                families.push((
                    "r first",
                    others()
                        .iter()
                        .map(|&o| {
                            let u = o ^ SLOT[2];
                            let w0 = d ^ u.wrapping_mul(SLOT[3]) ^ START.1;
                            fixed([w0, free, o, free])
                        })
                        .collect(),
                ));
                // The word that makes `y` the degenerate value, for each `w[3]`.
                families.push((
                    "r second",
                    others()
                        .iter()
                        .map(|&o| {
                            let v = o ^ SLOT[3];
                            let w1 = d ^ v.wrapping_mul(SLOT[2]) ^ START.0;
                            fixed([free, w1, free, o])
                        })
                        .collect(),
                ));
            }
            for (name, keys) in &families {
                all_apart(&format!("{name} {d:#x}"), keys);
            }
        }
    }

    /// The state is what carries the earlier blocks, and `r` is the one product that reads it, so a
    /// tail making one of `r`'s factors degenerate is the 4.x family again if that product loses
    /// the other. Two states equal on the side that makes the factor, and apart on the other,
    /// reading the same tail: reaching them through keys takes a 2^32 search, reaching them here
    /// does not.
    #[test]
    fn a_degenerate_factor_keeps_the_state() {
        use super::{SLOT, round, round2_chained};
        let word = 0x1234_5678_9abc_def0u64;
        let (u, v) = (word ^ SLOT[2], word ^ SLOT[3]);
        let (from_u, from_v) = (u.wrapping_mul(SLOT[3]), v.wrapping_mul(SLOT[2]));
        for d in DEGENERATE {
            for other in others() {
                let delta = (other >> 1).max(1);
                let (one, two) = (other, other ^ delta);
                // `x` is `d` under both states, and `y` differs. Then `y` is `d`, and `x` differs.
                let x_side = (one, 0x5555_5555_5555_5555);
                let w = [d ^ from_u ^ x_side.1, word, word, word];
                let (s0, s1) = (x_side, (two, x_side.1));
                assert_ne!(round(w, s0), round(w, s1), "x = {d:#x}, {delta:#x}");
                let w = [d ^ x_side.1, word];
                assert_ne!(
                    round2_chained((w[0], w[1]), s0),
                    round2_chained((w[0], w[1]), s1),
                    "x = {d:#x}, {delta:#x}"
                );
                let y_side = (0x5555_5555_5555_5555, one);
                let w = [word, d ^ from_v ^ y_side.0, word, word];
                let (s0, s1) = (y_side, (y_side.0, two));
                assert_ne!(round(w, s0), round(w, s1), "y = {d:#x}, {delta:#x}");
                let w = (word, d ^ y_side.0);
                assert_ne!(
                    round2_chained(w, s0),
                    round2_chained(w, s1),
                    "y = {d:#x}, {delta:#x}"
                );
            }
        }
    }

    /// `merge` keeps its first operand where the fold alone would not: a fold is `!0` at `y == !0`,
    /// and `0` at `y == 0`, for every `x`.
    #[test]
    fn merge_keeps_x_at_a_degenerate_y() {
        for y in DEGENERATE {
            let mut merged: Vec<u64> = others().iter().map(|&x| super::merge(x, y)).collect();
            merged.sort_unstable();
            merged.dedup();
            assert_eq!(merged.len(), others().len(), "y = {y:#x}");
        }
    }

    /// Could these bytes stand somewhere inside valid UTF-8? Up to three bytes of a character the
    /// slice starts in the middle of, then characters, then the start of one it stops in.
    fn spellable(bytes: &[u8]) -> bool {
        let tail = bytes
            .iter()
            .take(3)
            .take_while(|b| (0x80..0xc0).contains(*b))
            .count();
        (0..=tail).any(|skip| match std::str::from_utf8(&bytes[skip..]) {
            Ok(_) => true,
            Err(e) => e.error_len().is_none(),
        })
    }

    /// What keeps a text key from zeroing `p` or `q`: the module's masks hold byte pairs no UTF-8
    /// has. Above 16 bytes a word is 8 contiguous bytes of the key. Up to 16 it is two 4-byte
    /// loads from anywhere in it, so one half has to be unspellable by itself.
    #[test]
    fn no_text_spells_a_mask() {
        use super::SLOT;
        for k in &SLOT[..4] {
            assert!(!spellable(&k.to_le_bytes()), "{k:#x}");
        }
        for k in [SLOT[0], SLOT[1], SLOT[6], SLOT[7]] {
            let b = k.to_le_bytes();
            assert!(!spellable(&b[..4]) || !spellable(&b[4..]), "{k:#x}");
        }
        assert!(spellable("héllo".as_bytes()));
        assert!(spellable(&"é中".as_bytes()[1..4]));
        assert!(!spellable(b"\xd3\x08"));
    }

    /// `f` over every input against its pinned value, all of them reported at once when any
    /// moved: what a deliberate change needs to repin the table in one run.
    fn pinned(f: fn(&str) -> u64, table: &[(&str, u64)]) {
        let actual: Vec<String> = table
            .iter()
            .map(|&(s, want)| format!("({s:?}, {:#018x}) wanted {want:#018x}", f(s)))
            .collect();
        assert!(
            table.iter().all(|&(s, want)| f(s) == want),
            "pinned values moved:\n{}",
            actual.join("\n")
        );
    }

    /// Every serialised MPH blob is keyed on these hashes, so a hash that silently changed — a
    /// tweaked constant, a reordered merge, a byte-order slip in a refactor — would make every
    /// previously-saved index load wrong without any test failing. These pinned values turn that
    /// into a loud CI failure instead. **Do not "fix" them to match new output: changing the hash
    /// is a breaking blob-format change and must bump the format magic, not this table.** 2.0 did
    /// exactly that — `BMP7` and `BCH7` — when the round was replaced, and 4.0 again — `BMP8`,
    /// `BCH8`, `BCL2` — when the shape did, and 5.0 again — `BMP9`, `BCH9`, `BCL3`, `BHD2` — when the
    /// state went to 128 bits.
    #[test]
    fn hash_key_is_stable() {
        pinned(
            hash_key,
            &[
                ("", 0x9658_a78b_6c7a_8915),
                ("a", 0x1ae7_fdc6_6539_7a7a),
                ("GET", 0x76cc_686d_b6c1_3a07),
                ("four", 0x781f_35bc_a64a_4dc9),
                ("apple", 0xf822_3492_c926_6f5e),
                ("é中🎉", 0x46ee_c03e_55ae_12dc),
                ("member-00042", 0xbafd_4268_35d7_41ba),
                ("sixteen bytes!!!", 0x7a43_26ab_bbaa_d659),
                ("a key of 17 bytes", 0x1ebf_7994_9833_2915),
                ("thirty-two bytes, the class edge", 0xe971_4047_07ed_228a),
                ("thirty-three bytes: one block in!", 0xa374_d354_e36d_2eab),
                (
                    "a key long enough for the block loop to run twice over it, and then some",
                    0x6705_eccd_c7ee_c074,
                ),
            ],
        );
    }

    #[test]
    fn fingerprint_is_stable() {
        pinned(
            fingerprint_full,
            &[
                ("", 0xf120_6051_e4ea_c778),
                ("GET", 0x8a24_de41_4b8b_45fa),
                ("apple", 0x8bd7_a46b_7637_3ea3),
                ("é中🎉", 0x55ce_ae48_ee9d_43e5),
                ("member-00042", 0x771b_93ab_2476_7e25),
                ("a key of 17 bytes", 0x1456_53e5_d0ab_1564),
                ("thirty-three bytes: one block in!", 0x9ffe_8533_f9e0_d0d6),
                (
                    "a key long enough for the block loop to run twice over it, and then some",
                    0x2042_371b_af9a_45e0,
                ),
            ],
        );
    }

    /// The fused pass is an optimisation, not a second hash function: it must agree with the two
    /// it replaces on every input, or every blob written by one version reads wrong under another.
    #[test]
    fn the_fused_pass_equals_the_two_separate_hashes() {
        let cases: Vec<String> = ["", "a", "apple", "GET", "é中🎉", "member-00042"]
            .iter()
            .map(|s| s.to_string())
            .chain((0..2_000).map(|i| format!("key-{i:07}-{}", "x".repeat(i % 97))))
            .collect();
        for s in &cases {
            assert_eq!(
                super::hash_pair(s),
                (hash_key(s), fingerprint_full(s)),
                "{s:?}"
            );
        }
    }

    /// Every byte of every length to 100 is read by both hashes — flipping it moves them — and a
    /// trailing zero byte moves them too: the coverage the word layout promises.
    #[test]
    fn every_byte_moves_both_hashes() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for len in 0..=100usize {
            let key: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let (s, f) = super::hash_pair_bytes(&key);
            for i in 0..len {
                let mut k = key.clone();
                k[i] ^= 1 << (next() % 8);
                let (s2, f2) = super::hash_pair_bytes(&k);
                assert!(s2 != s && f2 != f, "byte {i} of {len} does not move a hash");
            }
            let mut k = key.clone();
            k.push(0);
            let (s2, f2) = super::hash_pair_bytes(&k);
            assert!(
                s2 != s && f2 != f,
                "a trailing zero does not move a hash at {len}"
            );
        }
    }
}

/// The hash's own quality battery: what a change to [`state`], [`merge`] or a constant has to
/// survive before it ships, and the source of the committed `bench/results/hash-quality-*.txt`.
///
/// Run it with
/// `cargo test --release --features bench-mphf -- --ignored --nocapture hash_quality`.
/// It is `#[ignore]`d and behind a feature because it runs for tens of seconds and because an
/// ignored test's body still counts against the crate's coverage floor.
///
/// Every statistic is reported as a standard-normal `z`, so one bound reads across the whole
/// battery. Chi-square goes through the Wilson–Hilferty transform rather than the textbook
/// `(x − k) / sqrt(2k)`, which is wrong exactly where the thresholds live: at 255 degrees of
/// freedom and a true `z` of 6.00 the two read 5.99 and 7.07 (checked against `incgam` in
/// PARI/GP). The battery runs some forty tables, 88 064 avalanche cells and 1 806 336
/// bit-independence cells, so `|z| < 6.5` is the bound throughout — a family-wise false-alarm
/// rate of 1.5 × 10⁻⁴ over all 1 894 400 of them.
#[cfg(all(test, feature = "bench-mphf"))]
mod quality {
    use super::{fingerprint_full_bytes, hash_key_bytes, hash_pair_bytes};

    /// `|z|` a single cell of the battery may reach before it is called a failure. Derived in
    /// PARI/GP from the cell count, which is what a bound like this is a function of: 88 064
    /// avalanche cells (688 input bits over five lengths × 64 output bits × two hashes) and
    /// 1 806 336 bit-independence cells (448 input bits × two hashes × 2 016 output pairs) come
    /// to 1 894 400, and a family-wise rate of 10⁻³ over that many wants a two-sided `z` of
    /// 6.211; 6.5 leaves it at 1.5 × 10⁻⁴. The tables want far less. An earlier bound of 6.0 was
    /// derived from 44 032 cells, which counted one hash and no bit-independence cell at all.
    const Z_BOUND: f64 = 6.5;

    /// splitmix64, so the corpora and the probe sets are the same on every machine and every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn bytes(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.next() as u8).collect()
        }
    }

    /// A chi-square statistic over `counts` as a standard-normal `z`, by Wilson–Hilferty.
    ///
    /// `counts` must have at least two cells and enough mass that the expected count per cell is
    /// comfortably above five — every caller here keeps it above twenty.
    fn chi2_z(counts: &[u64]) -> f64 {
        let k = counts.len();
        let total: u64 = counts.iter().sum();
        let e = total as f64 / k as f64;
        assert!(
            k > 1 && e >= 5.0,
            "chi-square wants {k} cells with mass, got e = {e}"
        );
        let chi2: f64 = counts
            .iter()
            .map(|&c| {
                let d = c as f64 - e;
                d * d / e
            })
            .sum();
        let k = (k - 1) as f64;
        let t = 2.0 / (9.0 * k);
        ((chi2 / k).cbrt() - (1.0 - t)) / t.sqrt()
    }

    /// A count of successes out of `n` trials with success probability `p`, as a standard-normal
    /// `z`. Used where a chi-square would have two cells.
    fn binom_z(hits: u64, n: u64, p: f64) -> f64 {
        let mean = n as f64 * p;
        (hits as f64 - mean) / (mean * (1.0 - p)).sqrt()
    }

    /// The dictionary if this machine has one, else `None` — the battery reports what it ran on.
    fn dictionary() -> Option<Vec<String>> {
        let text = std::fs::read_to_string("/usr/share/dict/words").ok()?;
        let words: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect();
        (words.len() > 10_000).then_some(words)
    }

    /// The key families a real index meets, each one a shape that has broken a hash somewhere:
    /// a shared prefix, a shared suffix, a dense numeric tail, a small alphabet, multi-byte
    /// characters, and the pre-sorted integer sequence a synthetic benchmark generates.
    fn families(n: usize) -> Vec<(&'static str, Vec<Vec<u8>>)> {
        let mut rng = Rng(0x5eed_0007);
        let mut out: Vec<(&'static str, Vec<Vec<u8>>)> = Vec::new();
        // Every family spelled from words is built here or not at all: without a dictionary there
        // is nothing to spell with, and a stand-in word makes the family one key repeated `n`
        // times, which the collision count then reports as a broken hash.
        if let Some(words) = dictionary() {
            let word = |i: usize| words[i % words.len()].as_str();
            out.push((
                "words",
                words
                    .iter()
                    .take(n)
                    .map(|w| w.as_bytes().to_vec())
                    .collect(),
            ));
            out.push((
                "bigrams",
                (0..n)
                    .map(|i| format!("{}.{}", word(i * 7), word(i * 13 + 1)).into_bytes())
                    .collect(),
            ));
            out.push((
                "prefixed",
                (0..n)
                    .map(|i| format!("https://example.com/a/b/{}", word(i)).into_bytes())
                    .collect(),
            ));
            out.push((
                "suffixed",
                (0..n)
                    .map(|i| format!("{}@mail.example.com", word(i)).into_bytes())
                    .collect(),
            ));
            out.push((
                "paths",
                (0..n)
                    .map(|i| {
                        format!("/usr/lib/{}/{}/{}.so", word(i), word(i * 3), word(i * 5))
                            .into_bytes()
                    })
                    .collect(),
            ));
        }
        out.push((
            "numeric",
            (0..n).map(|i| format!("key_{i:09}").into_bytes()).collect(),
        ));
        out.push((
            "decimal",
            (0..n).map(|i| i.to_string().into_bytes()).collect(),
        ));
        out.push((
            "uuid",
            (0..n)
                .map(|_| {
                    let (a, b) = (rng.next(), rng.next());
                    format!(
                        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                        a >> 32,
                        (a >> 16) & 0xffff,
                        a & 0xffff,
                        b >> 48,
                        b & 0xffff_ffff_ffff
                    )
                    .into_bytes()
                })
                .collect(),
        ));
        out.push((
            "cyrillic",
            (0..n)
                .map(|i| {
                    let mut s = String::new();
                    let mut x = i;
                    for _ in 0..6 {
                        s.push(char::from_u32(0x430 + (x % 32) as u32).expect("in range"));
                        x /= 32;
                    }
                    s.into_bytes()
                })
                .collect(),
        ));
        out.push((
            "dna",
            (0..n)
                .map(|_| (0..40).map(|_| b"ACGT"[rng.below(4)]).collect())
                .collect(),
        ));
        for (name, keys) in &out {
            let mut distinct: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(
                distinct.len(),
                keys.len(),
                "{name} repeats a key: its collisions would measure this generator, not the hash"
            );
        }
        out
    }

    /// Every output bit must flip with probability ½ when any one input bit flips: the strict
    /// avalanche criterion, cell by cell, for both hashes and five key lengths.
    ///
    /// Returns `(worst |z|, where)`.
    fn avalanche(trials: usize) -> (f64, String) {
        let mut rng = Rng(0xa1a1_0001);
        let mut worst = (0.0f64, String::from("none"));
        for len in [4usize, 9, 16, 24, 33] {
            let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
            for bit in 0..len * 8 {
                let mut flips = [[0u64; 64]; 2];
                for key in &keys {
                    let base = hash_pair_bytes(key);
                    let mut other = key.clone();
                    other[bit / 8] ^= 1 << (bit % 8);
                    let moved = hash_pair_bytes(&other);
                    for (h, (a, b)) in [(0, (base.0, moved.0)), (1, (base.1, moved.1))] {
                        let x = a ^ b;
                        for (out, cell) in flips[h].iter_mut().enumerate() {
                            *cell += (x >> out) & 1;
                        }
                    }
                }
                for (h, name) in [(0, "slot"), (1, "fingerprint")] {
                    for (out, &f) in flips[h].iter().enumerate() {
                        let z = binom_z(f, trials as u64, 0.5).abs();
                        if z > worst.0 {
                            worst = (z, format!("{name} len {len} in-bit {bit} out-bit {out}"));
                        }
                    }
                }
            }
        }
        worst
    }

    /// How many input bits one (length, hash) pair contributes. A 640-bit key would otherwise
    /// cost five times what a 128-bit one does for no extra statistical reach: the criterion is
    /// about the *output* pairs, and every input bit tests all 2 016 of them. Sampling the input
    /// bits with the battery's own fixed RNG keeps the cost flat in key length and the cell
    /// count — which `Z_BOUND` is a function of — a constant.
    const BIC_IN_BITS: usize = 128;

    /// Two output bits must flip independently of each other: the bit-independence criterion, as
    /// a 2×2 table per (input bit, output pair) and a chi-square on it — for **both** hashes over
    /// four key lengths, the same surface avalanche covers.
    ///
    /// Returns `(worst |z|, where)`.
    fn bit_independence(trials: usize) -> (f64, String) {
        let mut rng = Rng(0xb1c0_0002);
        let mut worst = (0.0f64, String::from("none"));
        for len in [8usize, 16, 24, 80] {
            let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
            let mut bits: Vec<usize> = (0..len * 8).collect();
            // A deterministic partial shuffle, so which bits are tested is fixed across machines.
            for i in 0..bits.len().min(BIC_IN_BITS) {
                let j = i + rng.below(bits.len() - i);
                bits.swap(i, j);
            }
            bits.truncate(BIC_IN_BITS);
            for &bit in &bits {
                let mut joint = [vec![[0u64; 4]; 64 * 64], vec![[0u64; 4]; 64 * 64]];
                for key in &keys {
                    let mut other = key.clone();
                    other[bit / 8] ^= 1 << (bit % 8);
                    let (a0, a1) = hash_pair_bytes(key);
                    let (b0, b1) = hash_pair_bytes(&other);
                    for (h, x) in [(0usize, a0 ^ b0), (1, a1 ^ b1)] {
                        for i in 0..64 {
                            let bi = ((x >> i) & 1) as usize;
                            for j in i + 1..64 {
                                let bj = ((x >> j) & 1) as usize;
                                joint[h][i * 64 + j][bi * 2 + bj] += 1;
                            }
                        }
                    }
                }
                for (h, name) in [(0usize, "slot"), (1, "fingerprint")] {
                    for i in 0..64 {
                        for j in i + 1..64 {
                            let z = chi2_z(&joint[h][i * 64 + j]).abs();
                            if z > worst.0 {
                                worst = (
                                    z,
                                    format!("{name} len {len} in-bit {bit} out-bits ({i},{j})"),
                                );
                            }
                        }
                    }
                }
            }
        }
        worst
    }

    /// The scan that caught the pre-2.0 collision family: keys differing in exactly two bytes.
    ///
    /// Through 1.1 the round was a multiply and a rotate, and a difference in the top bits of one
    /// word could be cancelled by a difference in another — keys differing at bytes `8i+7` and
    /// `8i+11` collided in *both* hashes with probability 13–100 %, and `CompactHashIndex` merged
    /// them into one id. Every ordered pair of byte positions and every pair of single-bit deltas
    /// is tried here against random base keys; a double collision is a catastrophe, a collision in
    /// one hash alone is expected about `trials · 2⁻⁶⁴` times, i.e. never.
    ///
    /// Returns `(worst double-collision count, combos tried, where)`.
    fn two_byte_differential(len: usize, trials: usize) -> (u64, usize, String) {
        let mut rng = Rng(0xd1ff_0003);
        let keys: Vec<Vec<u8>> = (0..trials).map(|_| rng.bytes(len)).collect();
        let mut worst = (0u64, String::from("none"));
        let mut combos = 0usize;
        for i in 0..len {
            for j in i + 1..len {
                for bi in 0..8u8 {
                    for bj in 0..8u8 {
                        combos += 1;
                        let mut both = 0u64;
                        for key in &keys {
                            let mut other = key.clone();
                            other[i] ^= 1 << bi;
                            other[j] ^= 1 << bj;
                            if hash_pair_bytes(key) == hash_pair_bytes(&other) {
                                both += 1;
                            }
                        }
                        if both > worst.0 {
                            worst = (both, format!("bytes ({i},{j}) bits ({bi},{bj})"));
                        }
                    }
                }
            }
        }
        (worst.0, combos, worst.1)
    }

    /// The report. Prints every statistic and fails on the first one past the bound, so the
    /// committed artifact and the pass/fail are the same run.
    #[test]
    #[ignore = "runs for tens of seconds; the committed output is bench/results/hash-quality-*.txt"]
    fn hash_quality() {
        println!("lexindex hash quality — bound |z| < {Z_BOUND}\n");
        println!(
            "corpus source: {}",
            if dictionary().is_some() {
                "/usr/share/dict/words plus generated families"
            } else {
                "generated families only (no /usr/share/dict/words on this machine)"
            }
        );

        let (z, where_) = avalanche(8192);
        println!("\navalanche (strict, 8192 keys per cell, 5 lengths, both hashes)");
        println!("  worst |z| {z:6.2}   {where_}");
        assert!(z < Z_BOUND, "avalanche: |z| {z:.2} at {where_}");

        let (z, where_) = bit_independence(2048);
        println!(
            "\nbit independence (2048 keys, lengths 8/16/24/80, both hashes, \u{2264}{BIC_IN_BITS} input bits each)"
        );
        println!("  worst |z| {z:6.2}   {where_}");
        assert!(z < Z_BOUND, "bit independence: |z| {z:.2} at {where_}");

        let (both, combos, where_) = two_byte_differential(24, 2048);
        println!("\ntwo-byte differential (24-byte keys, 2048 bases per combo)");
        println!("  {combos} combos, worst double collision {both}/2048   {where_}");
        assert_eq!(both, 0, "two-byte differential: {both} at {where_}");

        println!("\nper corpus (n = 100 000 where the family allows it)");
        println!(
            "  {:<10} {:>7} {:>6} {:>9} {:>9} {:>9} {:>9}",
            "family", "n", "coll", "z top12", "z low12", "z fp8", "z joint"
        );
        for (name, keys) in families(100_000) {
            let n = keys.len();
            let mut slots: Vec<u64> = Vec::with_capacity(n);
            let mut top = vec![0u64; 4096];
            let mut low = vec![0u64; 4096];
            let mut fp8 = vec![0u64; 256];
            let mut joint = vec![0u64; 4096];
            for key in &keys {
                let (slot, fp) = hash_pair_bytes(key);
                assert_eq!(
                    (slot, fp),
                    (hash_key_bytes(key), fingerprint_full_bytes(key))
                );
                slots.push(slot);
                top[(slot >> 52) as usize] += 1;
                low[(slot & 0xfff) as usize] += 1;
                fp8[(fp & 0xff) as usize] += 1;
                joint[(((slot & 0x3f) << 6) | (fp & 0x3f)) as usize] += 1;
            }
            slots.sort_unstable();
            let coll = n - 1 - slots.windows(2).filter(|w| w[0] != w[1]).count();
            let zs = [chi2_z(&top), chi2_z(&low), chi2_z(&fp8), chi2_z(&joint)];
            println!(
                "  {name:<10} {n:>7} {coll:>6} {:>9.2} {:>9.2} {:>9.2} {:>9.2}",
                zs[0], zs[1], zs[2], zs[3]
            );
            assert_eq!(coll, 0, "{name}: {coll} slot-hash collisions in {n} keys");
            for (z, what) in zs.iter().zip(["top12", "low12", "fp8", "joint"]) {
                assert!(z.abs() < Z_BOUND, "{name} {what}: |z| {:.2}", z.abs());
            }
        }
        println!("\nall cells within the bound");
    }
}
