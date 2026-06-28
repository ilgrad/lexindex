"""End-to-end tests of the betula-index Python bindings."""

import betula_index
import pytest


def test_string_index_core():
    si = betula_index.StringIndex(["banana", "apple", "apricot", "cherry", "apple"])
    assert len(si) == 4  # duplicate "apple" deduped
    assert not si.is_empty()
    assert si.id("apple") == 0 and si.id("banana") == 2  # sorted rank
    assert si.id("missing") is None
    assert "cherry" in si and "durian" not in si
    assert si.contains("cherry")
    assert si.key(1) == "apricot"
    assert si.key(99) is None


def test_string_index_queries():
    si = betula_index.StringIndex(["apple", "apricot", "banana", "cherry"])
    assert [k for k, _ in si.prefix("ap")] == ["apple", "apricot"]
    assert [k for k, _ in si.range("apricot", "cherry")] == ["apricot", "banana"]
    assert [k for k, _ in si.fuzzy("aple", 1)] == ["apple"]  # one edit away
    assert [k for k, _ in si.subsequence("ae")] == ["apple"]  # a..e in order


def test_string_index_persistence(tmp_path):
    si = betula_index.StringIndex(["a", "b", "c"])
    assert betula_index.StringIndex.from_bytes(si.to_bytes()).id("b") == si.id("b")
    p = str(tmp_path / "idx.bix")
    si.save(p)
    assert betula_index.StringIndex.load(p).id("c") == si.id("c")


def test_string_index_empty_and_corrupt():
    si = betula_index.StringIndex([])
    assert si.is_empty() and si.id("x") is None and si.key(0) is None
    with pytest.raises(ValueError):
        betula_index.StringIndex.from_bytes(b"nope")


def test_perfect_hash_index():
    ph = betula_index.PerfectHashIndex(["alpha", "beta", "gamma", "delta", "alpha"])
    assert len(ph) == 4
    ids = set()
    for w in ["alpha", "beta", "gamma", "delta"]:
        i = ph.id(w)
        assert i is not None and ph.key(i) == w and ph.id_unchecked(w) == i
        assert w in ph
        ids.add(i)
    assert ids == {0, 1, 2, 3}  # dense bijection
    assert ph.id("epsilon") is None and "epsilon" not in ph


def test_perfect_hash_persistence(tmp_path):
    ph = betula_index.PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
    ph2 = betula_index.PerfectHashIndex.from_bytes(ph.to_bytes())
    for w in ["GET", "POST", "PUT", "DELETE"]:
        assert ph2.id(w) == ph.id(w)
    p = str(tmp_path / "dict.bmp")
    ph.save(p)
    assert betula_index.PerfectHashIndex.load(p).id("POST") == ph.id("POST")


def test_perfect_hash_empty_and_corrupt():
    ph = betula_index.PerfectHashIndex([])
    assert ph.is_empty() and ph.id("x") is None
    with pytest.raises(ValueError):
        betula_index.PerfectHashIndex.from_bytes(b"nope")
