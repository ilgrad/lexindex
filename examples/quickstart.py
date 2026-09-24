"""Quickstart: the seven lexindex structures and when to reach for each.

One vocabulary, seven indexes, each answering a different question:

  - StringIndex       ordered + typo-tolerant: autocomplete, fuzzy, range, exact both ways
  - DictIndex         ordered, every key stored: exact both ways + lower_bound, a third of the size
  - HashedDictIndex   a DictIndex whose string -> id is a hash: the same ranks, no search
  - CompactHashIndex  smallest string -> id (probabilistic membership, no reverse)
  - ClosedHashIndex   the perfect hash alone: string -> id for a vocabulary known to be closed
  - PerfectHashIndex  exact membership + reverse id -> string, fastest closed-vocabulary lookup
  - DoubleArrayIndex  every key occurring in a text, a character a step: segmentation, tagging

Run::

    pip install lexindex
    python examples/quickstart.py
"""

from __future__ import annotations

import tempfile
from pathlib import Path

from lexindex import (
    ClosedHashIndex,
    CompactHashIndex,
    DictIndex,
    DoubleArrayIndex,
    HashedDictIndex,
    PerfectHashIndex,
    StringIndex,
)

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
    # system dictionary this is 2.24 B/key at fp=2, 1.24 at fp=1 — below marisa-trie's 2.98.
    print("CompactHashIndex:  id('grape') ->", ids["grape"], "(dense [0, n); 1.24 B/key)")


def closed_hash_demo() -> None:
    """The perfect hash and nothing else, when every query is a member by construction."""
    vocab = ClosedHashIndex(VOCAB)

    # the same ids CompactHashIndex gives over the same keys, without the fingerprint table:
    # ~0.24 B/key at scale, and a lookup with no compare behind it
    assert vocab.id("grape") == CompactHashIndex(VOCAB).id_unchecked("grape")
    assert sorted(vocab.ids_of(VOCAB)) == list(range(len(VOCAB)))

    # nothing stored can tell a stranger from a member, so `id` never says "absent" -- a
    # stranger gets *some* id in [0, n), and there is no `in` / `contains` to pretend otherwise
    assert 0 <= vocab.id("durian") < len(VOCAB)
    print("ClosedHashIndex:  id('grape') ->", vocab.id("grape"), "(dense [0, n); ~0.24 B/key)")


def dict_index_demo() -> None:
    """Ordered, every key stored, 56 % below StringIndex: exact both ways, plus lower_bound."""
    words = DictIndex(VOCAB)
    lo, hi = words.lower_bound("b"), words.lower_bound("c")  # the "b..." keys as an id range
    print("DictIndex:        id('cherry') ->", words.id("cherry"), " key(0) ->", words.key(0))
    print(
        "DictIndex:        keys in ['b', 'c') ->",
        words.keys_of(list(range(lo, hi))),
        "(2.64 B/key on the 479k-word dictionary)",
    )


def hashed_dict_demo() -> None:
    """A DictIndex with a hash sidecar: its ranks, answered by a hash rather than a search."""
    words = HashedDictIndex.from_dict(DictIndex(VOCAB), fingerprint_bits=8)
    assert words.id("cherry") == words.dict.id("cherry") == 6  # the dictionary's own ranks
    assert words.dict.key(6) == "cherry"  # ordered queries and id -> key go to the dictionary
    closed = HashedDictIndex.from_dict(DictIndex(VOCAB), fingerprint_bits=0)
    assert closed.id_unchecked("cherry") == 6  # the closed path: no fingerprint stored
    assert closed.id("durian") is None  # at zero bits `id` is the dictionary's exact search
    print("HashedDictIndex:  id('cherry') ->", words.id("cherry"), "(one hash, no search)")


def perfect_hash_demo() -> None:
    """Exact membership + reverse lookup, fastest closed-vocabulary map."""
    d = PerfectHashIndex(VOCAB)
    i = d.id("avocado")
    assert i is not None and d.key(i) == "avocado"  # exact round-trip
    assert d.id("durian") is None  # verified miss, never a false positive
    assert d.id_unchecked("grape") == d.id("grape")  # skip the check on a known member
    print("PerfectHashIndex:  id('avocado') ->", i, "-> key ->", d.key(i))


def double_array_demo() -> None:
    """Every key occurring in a text, one load a character: a segmenter's question."""
    words = ["北京", "北京大学", "大学", "大学生", "生活"]
    lexicon = DoubleArrayIndex(words)
    text = "北京大学生活"
    # (start, end, id) in characters, starts ascending and the shortest key first at each start
    found = lexicon.occurrences(text)
    assert [text[s:e] for s, e, _ in found] == ["北京", "北京大学", "大学", "大学生", "生活"]
    assert lexicon.longest_prefix(text) == ("北京大学", 1)
    # the ids are the keys' ranks, the ones StringIndex gives the same keys
    assert lexicon.id("大学") == StringIndex(words).id("大学") == 2
    print("DoubleArrayIndex:  occurrences ->", [text[s:e] for s, e, _ in found])


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
    closed_hash_demo()
    dict_index_demo()
    hashed_dict_demo()
    perfect_hash_demo()
    double_array_demo()
    persistence_demo()
    print("\nquickstart OK")
