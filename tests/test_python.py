"""End-to-end tests of the lexindex Python bindings."""

import random

import lexindex
import pytest


def test_string_index_core():
    si = lexindex.StringIndex(["banana", "apple", "apricot", "cherry", "apple"])
    assert len(si) == 4  # duplicate "apple" deduped
    assert not si.is_empty()
    assert si.id("apple") == 0 and si.id("banana") == 2  # sorted rank
    assert si.id("missing") is None
    assert "cherry" in si and "durian" not in si
    assert si.contains("cherry")
    assert si.key(1) == "apricot"
    assert si.key(99) is None


def test_string_index_queries():
    si = lexindex.StringIndex(["apple", "apricot", "banana", "cherry"])
    assert [k for k, _ in si.prefix("ap")] == ["apple", "apricot"]
    assert [k for k, _ in si.range("apricot", "cherry")] == ["apricot", "banana"]
    assert [k for k, _ in si.fuzzy("aple", 1)] == ["apple"]  # one edit away
    assert [k for k, _ in si.subsequence("ae")] == ["apple"]  # a..e in order


def test_string_index_persistence(tmp_path):
    si = lexindex.StringIndex(["a", "b", "c"])
    assert lexindex.StringIndex.from_bytes(si.to_bytes()).id("b") == si.id("b")
    p = str(tmp_path / "idx.bix")
    si.save(p)
    assert lexindex.StringIndex.load(p).id("c") == si.id("c")


def test_string_index_empty_and_corrupt():
    si = lexindex.StringIndex([])
    assert si.is_empty() and si.id("x") is None and si.key(0) is None
    with pytest.raises(ValueError):
        lexindex.StringIndex.from_bytes(b"nope")


def test_perfect_hash_index():
    ph = lexindex.PerfectHashIndex(["alpha", "beta", "gamma", "delta", "alpha"])
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
    ph = lexindex.PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
    ph2 = lexindex.PerfectHashIndex.from_bytes(ph.to_bytes())
    for w in ["GET", "POST", "PUT", "DELETE"]:
        assert ph2.id(w) == ph.id(w)
    p = str(tmp_path / "dict.bmp")
    ph.save(p)
    assert lexindex.PerfectHashIndex.load(p).id("POST") == ph.id("POST")


def test_perfect_hash_empty_and_corrupt():
    ph = lexindex.PerfectHashIndex([])
    assert ph.is_empty() and ph.id("x") is None
    with pytest.raises(ValueError):
        lexindex.PerfectHashIndex.from_bytes(b"nope")


def test_string_index_load_mmap(tmp_path):
    si = lexindex.StringIndex(["apple", "apricot", "banana", "cherry"])
    p = str(tmp_path / "idx.bix")
    si.save(p)
    mapped = lexindex.StringIndex.load_mmap(p)  # zero-copy: borrows the mapped file
    assert len(mapped) == len(si)
    assert mapped.id("banana") == si.id("banana")
    assert mapped.key(0) == "apple"
    assert [k for k, _ in mapped.prefix("ap")] == ["apple", "apricot"]


def test_perfect_hash_load_mmap(tmp_path):
    ph = lexindex.PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
    p = str(tmp_path / "dict.bmp")
    ph.save(p)
    mapped = lexindex.PerfectHashIndex.load_mmap(p)
    assert len(mapped) == len(ph)
    for w in ["GET", "POST", "PUT", "DELETE"]:
        i = mapped.id(w)
        assert i == ph.id(w) and mapped.key(i) == w
    assert "MISSING" not in mapped


def test_compact_hash_index():
    # 4-byte fingerprint => 1/2**32 false-positive rate, so membership is effectively exact here.
    ch = lexindex.CompactHashIndex(["alpha", "beta", "gamma", "delta", "alpha"], 4)
    assert len(ch) == 4 and not ch.is_empty()  # duplicate "alpha" deduped
    ids = set()
    for w in ["alpha", "beta", "gamma", "delta"]:
        i = ch.id(w)
        assert i is not None and ch.id_unchecked(w) == i
        assert w in ch and ch.contains(w)
        ids.add(i)
    assert ids == {0, 1, 2, 3}  # dense slots in [0, n)
    assert ch.id("epsilon") is None and "epsilon" not in ch


def test_compact_hash_default_fingerprint():
    ch = lexindex.CompactHashIndex(["x", "y", "z"])  # fingerprint_bytes defaults to 1
    assert all(ch.contains(w) for w in ["x", "y", "z"])


def test_compact_hash_invalid_fingerprint_bytes():
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex(["a", "b"], 3)  # only 1, 2, 4 allowed


def test_compact_hash_persistence(tmp_path):
    ch = lexindex.CompactHashIndex(["GET", "POST", "PUT", "DELETE"], 2)
    ch2 = lexindex.CompactHashIndex.from_bytes(ch.to_bytes())
    for w in ["GET", "POST", "PUT", "DELETE"]:
        assert ch2.id(w) == ch.id(w)
    p = str(tmp_path / "dict.bch")
    ch.save(p)
    assert lexindex.CompactHashIndex.load(p).id("POST") == ch.id("POST")


def test_compact_hash_empty_and_corrupt():
    ch = lexindex.CompactHashIndex([])
    assert ch.is_empty() and ch.id("x") is None and "x" not in ch
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex.from_bytes(b"nope")


def test_compact_hash_load_mmap(tmp_path):
    ch = lexindex.CompactHashIndex(["GET", "POST", "PUT", "DELETE"], 4)
    p = str(tmp_path / "dict.bch")
    ch.save(p)
    mapped = lexindex.CompactHashIndex.load_mmap(p)
    assert len(mapped) == len(ch)
    for w in ["GET", "POST", "PUT", "DELETE"]:
        assert mapped.id(w) == ch.id(w) and mapped.contains(w)
    assert "MISSING" not in mapped


def test_compact_hash_false_positive_rate_bounded():
    # A 2-byte fingerprint bounds the membership false-positive rate to 1/65536; over 50k random
    # non-member probes the expected count is < 1, so a handful is already deeply in the tail.
    members = [f"token-{i}" for i in range(2000)]
    ch = lexindex.CompactHashIndex(members, 2)
    assert all(ch.contains(m) for m in members)
    member_set = set(members)
    rng = random.Random(0)
    trials = fp = 0
    while trials < 50_000:
        s = "".join(chr(rng.randint(97, 122)) for _ in range(rng.randint(4, 10)))
        if s in member_set:
            continue
        trials += 1
        fp += ch.contains(s)
    assert fp <= 10, f"false-positive rate too high: {fp}/{trials}"


def test_string_index_batch():
    si = lexindex.StringIndex(["apple", "apricot", "banana", "cherry"])
    assert si.ids_of(["banana", "missing", "apple"]) == [2, None, 0]
    assert si.keys_of([0, 2, 99]) == ["apple", "banana", None]
    # the batch form agrees with the singular accessors, element for element
    ws = ["cherry", "apricot", "nope"]
    assert si.ids_of(ws) == [si.id(w) for w in ws]
    assert si.keys_of([3, 1, 0]) == [si.key(i) for i in (3, 1, 0)]
    assert si.ids_of([]) == [] and si.keys_of([]) == []


def test_perfect_hash_batch():
    ph = lexindex.PerfectHashIndex(["GET", "POST", "PUT", "DELETE"])
    assert ph.ids_of(["POST", "PATCH", "GET"]) == [ph.id("POST"), None, ph.id("GET")]
    ids = [ph.id(w) for w in ["GET", "POST"]]
    assert ph.keys_of(ids) == ["GET", "POST"]
    assert ph.keys_of([999]) == [None]


def test_compact_hash_batch():
    ch = lexindex.CompactHashIndex(["GET", "POST", "PUT", "DELETE"], 4)
    ws = ["POST", "GET", "DELETE"]
    assert ch.ids_of(ws) == [ch.id(w) for w in ws]  # batch == singular, in order
    assert all(i is not None for i in ch.ids_of(ws))
    assert ch.ids_of([]) == []


def test_string_index_neighbours():
    si = lexindex.StringIndex(["apple", "apricot", "banana", "cherry"])
    # successor: smallest key >= query
    assert si.successor("apple") == ("apple", 0)  # present -> itself
    assert si.successor("ba") == ("banana", 2)  # between apricot and banana
    assert si.successor("zzz") is None  # after all
    # predecessor: largest key <= query
    assert si.predecessor("cherry") == ("cherry", 3)  # present -> itself
    assert si.predecessor("ba") == ("apricot", 1)  # between apricot and banana
    assert si.predecessor("a") is None  # before all


def test_string_index_iter():
    si = lexindex.StringIndex(["banana", "apple", "apricot", "cherry"])
    # __iter__ yields every (key, id) in sorted (= id) order, lazily
    assert list(si) == [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
    # a fresh iterator each time — iteration is repeatable
    assert [k for k, _ in si] == ["apple", "apricot", "banana", "cherry"]
    assert dict(si)["banana"] == 2
    assert list(lexindex.StringIndex([])) == []


def test_version_is_exposed():
    v = lexindex.__version__
    assert isinstance(v, str) and v  # non-empty string
    assert v[0].isdigit() and "." in v  # looks like a real version (installed metadata)
