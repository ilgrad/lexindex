"""Quickstart: the three lexindex structures and when to reach for each.

One vocabulary, three indexes, each answering a different question:

  - StringIndex       ordered + typo-tolerant: autocomplete, fuzzy, range, exact both ways
  - CompactHashIndex  smallest string -> id (probabilistic membership, no reverse)
  - PerfectHashIndex  exact membership + reverse id -> string, fastest closed-vocabulary lookup

Run::

    pip install lexindex
    python examples/quickstart.py
"""

from __future__ import annotations

import tempfile
from pathlib import Path

from lexindex import CompactHashIndex, PerfectHashIndex, StringIndex

VOCAB = [
    "apple",
    "apricot",
    "avocado",
    "banana",
    "blackberry",
    "blueberry",
    "cherry",
    "cranberry",
    "grape",
    "grapefruit",
]


def string_index_demo() -> None:
    """Ordered + fuzzy queries — the only index that answers these."""
    idx = StringIndex(VOCAB)

    # exact, both directions (id is the sorted rank; id -> key is a rank-walk over the FST)
    assert idx.id("cherry") == 6
    assert idx.key(6) == "cherry"

    # autocomplete: every key under a prefix, no full scan
    assert [k for k, _ in idx.prefix("gr")] == ["grape", "grapefruit"]

    # typo tolerance: Levenshtein edit distance <= 1 ("bananna" -> delete one 'n' -> "banana")
    assert [k for k, _ in idx.fuzzy("bananna", 1)] == ["banana"]

    # lexicographic range [lo, hi)
    assert [k for k, _ in idx.range("blackberry", "cherry")] == ["blackberry", "blueberry"]

    print("StringIndex:  prefix('gr') ->", [k for k, _ in idx.prefix("gr")])
    print("              fuzzy('bananna', 1) ->", [k for k, _ in idx.fuzzy("bananna", 1)])


def compact_hash_demo() -> None:
    """Smallest string -> id, when a rare false positive is fine and you never need id -> key."""
    tokens = CompactHashIndex(VOCAB, fingerprint_bytes=2)  # ~0.0015% false-positive rate

    # dense id in [0, n); use it as an embedding-row / feature index
    ids = {w: tokens.id(w) for w in VOCAB}
    assert sorted(ids.values()) == list(range(len(VOCAB)))
    assert tokens.contains("blueberry")
    assert not tokens.contains("durian")  # almost surely a true miss at 2 fingerprint bytes

    # Per-key size is only meaningful at scale (MPH overhead dominates 10 keys): on the 479k-word
    # system dictionary this is ~2.3 B/key at fp=2, 1.3 at fp=1 — below marisa-trie's 2.98.
    print("CompactHashIndex:  id('grape') ->", ids["grape"], "(dense [0, n); ~1.3 B/key)")


def perfect_hash_demo() -> None:
    """Exact membership + reverse lookup, fastest closed-vocabulary map."""
    d = PerfectHashIndex(VOCAB)
    i = d.id("avocado")
    assert i is not None and d.key(i) == "avocado"  # exact round-trip
    assert d.id("durian") is None  # verified miss, never a false positive
    assert d.id_unchecked("grape") == d.id("grape")  # skip the check on a known member
    print("PerfectHashIndex:  id('avocado') ->", i, "-> key ->", d.key(i))


def persistence_demo() -> None:
    """Build once, persist, then memory-map and borrow zero-copy — load time independent of size."""
    with tempfile.TemporaryDirectory() as tmp:
        path = str(Path(tmp) / "vocab.bix")
        StringIndex(VOCAB).save(path)
        mapped = StringIndex.load_mmap(path)  # no read into RAM; pages shared across processes
        assert mapped.id("cherry") == 6
        print("persistence:  load_mmap round-trip ok, key(6) ->", mapped.key(6))


if __name__ == "__main__":
    string_index_demo()
    compact_hash_demo()
    perfect_hash_demo()
    persistence_demo()
    print("\nquickstart OK")
