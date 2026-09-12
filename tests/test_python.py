"""End-to-end tests of the lexindex Python bindings."""

import array
import itertools
import multiprocessing
import os
import pickle
import random
import sys
import threading
import time
import types

import lexindex
import pytest


def _numpy_or_skip():
    """Import numpy, or skip — unless the environment says numpy must be there.

    `importorskip` alone made the zero-copy check an untested claim: neither CI nor the gate
    command installed numpy, so the one test that verifies `np.frombuffer` shares the buffer was
    skipped in every automated environment and nobody saw it. CI now installs numpy and sets
    `LEXINDEX_REQUIRE_NUMPY`, which turns a skip back into a failure if the job ever loses it.
    """
    if os.environ.get("LEXINDEX_REQUIRE_NUMPY"):
        import numpy

        return numpy
    return pytest.importorskip("numpy")


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


def test_compact_hash_fingerprint_bits():
    keys = [f"key-{i:03d}" for i in range(200)]
    ch = lexindex.CompactHashIndex(keys, fingerprint_bits=4)
    assert ch.fingerprint_bits == 4
    assert sorted(ch.id(k) for k in keys) == list(range(200))  # no false negatives, dense ids
    restored = lexindex.CompactHashIndex.from_bytes(ch.to_bytes())
    assert restored.fingerprint_bits == 4
    probes = keys + [f"miss-{i}" for i in range(50)]
    assert restored.ids_of(probes) == [ch.id(p) for p in probes]
    # bytes form reports its width in bits
    assert lexindex.CompactHashIndex(keys, 2).fingerprint_bits == 16
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex(keys, fingerprint_bits=0)
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex(keys, fingerprint_bits=65)
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex(keys, 2, fingerprint_bits=8)  # ambiguous: both widths given


def test_compact_hash_keeps_colliding_keys_distinct_at_any_width():
    # This pair collides in the 64-bit slot hash (pinned in the Rust suite); the side table matches
    # on the full second hash, so even a 1-bit fingerprint table must keep them two distinct ids —
    # and a generator input exercises the streaming (hash-as-you-go) construction path.
    a, b = "x5iojurfgtipm", "7gvob4sxctomf"
    ch = lexindex.CompactHashIndex((k for k in [a, b, "filler"]), fingerprint_bits=1)
    assert len(ch) == 3
    assert ch.id(a) is not None
    assert ch.id(b) is not None
    assert ch.id(a) != ch.id(b)


def test_compact_hash_persistence(tmp_path):
    ch = lexindex.CompactHashIndex(["GET", "POST", "PUT", "DELETE"], 2)
    ch2 = lexindex.CompactHashIndex.from_bytes(ch.to_bytes())
    for w in ["GET", "POST", "PUT", "DELETE"]:
        assert ch2.id(w) == ch.id(w)
    p = str(tmp_path / "dict.bch")
    ch.save(p)
    assert lexindex.CompactHashIndex.load(p).id("POST") == ch.id("POST")


def test_serialized_len_matches_to_bytes():
    keys = ["alpha", "beta", "gamma"]
    for idx in (
        lexindex.StringIndex(keys),
        lexindex.PerfectHashIndex(keys),
        lexindex.CompactHashIndex(keys),
        lexindex.StringIndex([]),
        lexindex.CompactHashIndex([], 2),
    ):
        assert idx.serialized_len() == len(idx.to_bytes())


def test_compact_hash_build_to_file_writes_what_the_constructor_would(tmp_path):
    keys = [f"tok-{i % 700}" for i in range(1000)]  # duplicates fold
    for kwargs in ({}, {"fingerprint_bytes": 2}, {"fingerprint_bits": 4}):
        p = tmp_path / "dict.bch"  # a pathlib.Path, not a str
        written = lexindex.CompactHashIndex.build_to_file((k for k in keys), p, **kwargs)
        assert written == 700
        assert p.read_bytes() == lexindex.CompactHashIndex(keys, **kwargs).to_bytes()
        assert lexindex.CompactHashIndex.load(p).id("tok-5") is not None
    assert [e.name for e in tmp_path.iterdir()] == ["dict.bch"]  # no scratch left beside it
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex.build_to_file(keys, tmp_path / "x.bch", 3)
    with pytest.raises(ValueError):
        lexindex.CompactHashIndex.build_to_file(keys, tmp_path / "x.bch", 2, fingerprint_bits=8)


def test_compact_hash_build_to_file_aborts_when_the_iterable_raises(tmp_path):
    p = tmp_path / "dict.bch"
    p.write_bytes(b"previous")

    def raising():
        yield "alpha"
        yield "beta"
        raise RuntimeError("source failed")

    with pytest.raises(RuntimeError, match="source failed"):
        lexindex.CompactHashIndex.build_to_file(raising(), p)
    assert p.read_bytes() == b"previous"
    with pytest.raises(TypeError):
        lexindex.CompactHashIndex.build_to_file([1, 2], p)
    assert [e.name for e in tmp_path.iterdir()] == ["dict.bch"]


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


def test_closed_hash_index():
    ch = lexindex.ClosedHashIndex(["alpha", "beta", "gamma", "delta", "alpha"])
    assert len(ch) == 4 and not ch.is_empty()  # duplicate "alpha" deduped
    assert sorted(ch.id(w) for w in ["alpha", "beta", "gamma", "delta"]) == [0, 1, 2, 3]
    # A stranger gets *some* id below n: the index stores nothing that could say otherwise, so
    # there is no membership spelling to promise one.
    assert 0 <= ch.id("epsilon") < 4
    assert not hasattr(ch, "__contains__") and not hasattr(ch, "__getitem__")
    probes = ["delta", "alpha", "nope"]
    assert ch.ids_of(probes) == [ch.id(w) for w in probes]
    assert ch.ids_of([]) == []
    assert ch.ID_DTYPE == "uint32" and not hasattr(ch, "MISSING_ID")


def test_closed_hash_keeps_colliding_keys_distinct():
    a, b = "x5iojurfgtipm", "7gvob4sxctomf"  # collide in the 64-bit slot hash (pinned in Rust)
    ch = lexindex.ClosedHashIndex(k for k in [a, b, "filler"])
    assert len(ch) == 3
    assert sorted([ch.id(a), ch.id(b), ch.id("filler")]) == [0, 1, 2]
    back = lexindex.ClosedHashIndex.from_bytes(ch.to_bytes())
    assert (back.id(a), back.id(b)) == (ch.id(a), ch.id(b))


def test_closed_hash_persistence_and_size(tmp_path):
    words = [f"token-{i}" for i in range(20_000)]
    ch = lexindex.ClosedHashIndex(words)
    assert ch.serialized_len() == len(ch.to_bytes()) < 0.30 * len(words)
    # The same perfect hash as CompactHashIndex over the same keys, and only that.
    compact = lexindex.CompactHashIndex(words, 1)
    assert [ch.id(w) for w in words[:500]] == [compact.id_unchecked(w) for w in words[:500]]
    p = tmp_path / "vocab.bcl"  # a pathlib.Path, not a str
    ch.save(p)
    assert lexindex.ClosedHashIndex.load(p).ids_of(words[:100]) == ch.ids_of(words[:100])
    assert not hasattr(lexindex.ClosedHashIndex, "load_mmap")
    with pytest.raises(ValueError):
        lexindex.ClosedHashIndex.from_bytes(b"nope")
    empty = lexindex.ClosedHashIndex([])
    assert empty.is_empty() and empty.id("x") == 0 and empty.ids_of(["x"]) == [0]
    assert lexindex.ClosedHashIndex.from_bytes(empty.to_bytes()).is_empty()


def test_closed_hash_ids_of_bytes_and_ids_into():
    ch = lexindex.ClosedHashIndex(["alpha", "bravo", "charlie", "delta"])
    probes = ["delta", "zulu", "alpha"]
    raw = ch.ids_of_bytes(probes)
    assert len(raw) == 12
    unpacked = [int.from_bytes(raw[i * 4 : (i + 1) * 4], sys.byteorder) for i in range(3)]
    assert unpacked == ch.ids_of(probes)
    out = array.array("I", [7] * 5)
    assert ch.ids_into(probes, out) is None
    assert list(out) == [*ch.ids_of(probes), 7, 7]
    with pytest.raises(ValueError, match="1 items but 3 keys"):
        ch.ids_into(probes, array.array("I", [0]))
    with pytest.raises(BufferError):
        ch.ids_into(probes, array.array("Q", [0] * 3))  # 8-byte items for a uint32 index
    ch.ids_into([], bytearray())
    with pytest.raises(BufferError):
        ch.ids_into([], b"")  # read-only is still refused
    assert ch.ids_of_bytes([]) == b""


def test_dict_index_core():
    di = lexindex.DictIndex(["banana", "apple", "apricot", "cherry", "apple"])
    assert len(di) == 4 and not di.is_empty() and di.block == 256
    assert di.id("apple") == 0 and di.id("banana") == 2  # sorted rank
    assert di.id("missing") is None and di.get("missing") is None
    assert di.get("missing", -1) == -1 and di.get("cherry") == 3
    assert "cherry" in di and "durian" not in di and di.contains("cherry")
    assert di["cherry"] == 3
    with pytest.raises(KeyError):
        di["durian"]
    assert [di.key(i) for i in range(5)] == ["apple", "apricot", "banana", "cherry", None]
    assert di.keys_of([3, 0, 9]) == ["cherry", "apple", None]
    assert (di.lower_bound("apple"), di.lower_bound("b"), di.lower_bound("zzz")) == (0, 2, 4)
    assert list(di) == [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
    assert di.ids_of(["cherry", "x", "apple"]) == [3, None, 0]
    assert di.ID_DTYPE == "uint64" and di.MISSING_ID == 2**64 - 1
    empty = lexindex.DictIndex([])
    assert empty.is_empty() and empty.id("") is None and empty.key(0) is None
    assert list(empty) == [] and empty.lower_bound("x") == 0


def test_dict_index_ordered_queries():
    """A sorted dictionary answers prefix and range as id ranges -- no automaton, no scan of
    anything it does not return. Cross-checked against StringIndex, which answers the same
    questions over an FST."""
    words = ["apple", "apricot", "banana", "blueberry", "cherry"]
    di = lexindex.DictIndex(words)
    si = lexindex.StringIndex(words)
    for q in ["", "a", "ap", "apple", "b", "bl", "z", "apricots"]:
        assert di.prefix(q) == si.prefix(q), q
        assert di.prefix_count(q) == si.prefix_count(q), q
        assert di.prefix_id_range(q) == si.prefix_id_range(q), q
        assert di.successor(q) == si.successor(q), q
        assert di.predecessor(q) == si.predecessor(q), q
    for lo, hi in [("a", "b"), ("apricot", "cherry"), ("z", "zz"), ("", "\uffff")]:
        assert di.range(lo, hi) == si.range(lo, hi), (lo, hi)
        assert di.range_count(lo, hi) == si.range_count(lo, hi), (lo, hi)
    assert di.prefix("a", limit=2) == [("apple", 0), ("apricot", 1)]
    assert di.range("a", "z", limit=1) == [("apple", 0)]
    # The 128-key block is the configuration that goes under marisa on size; it answers the same.
    big = lexindex.DictIndex(words, block=128)
    assert big.prefix("ap") == di.prefix("ap") and big.prefix_id_range("b") == (2, 4)
    empty = lexindex.DictIndex([])
    assert empty.prefix("a") == [] and empty.prefix_id_range("a") == (0, 0)
    assert empty.successor("a") is None and empty.predecessor("a") is None


def test_dict_index_block_argument_and_persistence(tmp_path):
    words = sorted({f"token-{i * 7919 % 10007:05}" for i in range(20_000)})
    for block in (1, 5, 32, 1024):
        di = lexindex.DictIndex(words, block=block)
        assert di.block == block and len(di) == len(words)
        assert [di.id(w) for w in words[:300]] == list(range(300))
        assert di.keys_of(list(range(len(words) - 300, len(words)))) == words[-300:]
    with pytest.raises(ValueError, match="block"):
        lexindex.DictIndex(words, block=0)
    with pytest.raises(ValueError, match="block"):
        lexindex.DictIndex(words, 1025)
    di = lexindex.DictIndex(words)
    blob = di.to_bytes()
    assert blob[:4] == b"BDX3" and len(blob) == di.serialized_len() < 4.5 * len(words)
    back = lexindex.DictIndex.from_bytes(blob)
    assert back.ids_of(words[:100]) == list(range(100)) and back.to_bytes() == blob
    p = tmp_path / "words.bdx"  # a pathlib.Path, not a str
    di.save(p)
    assert lexindex.DictIndex.load(p).key(17) == words[17]
    mapped = lexindex.DictIndex.load_mmap(p)  # zero-copy: every section borrowed from the file
    assert mapped.key(17) == words[17] and mapped.ids_of(words[:100]) == list(range(100))
    assert lexindex.DictIndex.load_mmap_verified(p).to_bytes() == blob
    with pytest.raises(ValueError):
        lexindex.DictIndex.from_bytes(b"nope")
    # The lazy iterator crosses its refill boundary without repeating or skipping.
    walked = list(itertools.islice(lexindex.DictIndex(words), 1500))
    assert walked == [(w, i) for i, w in enumerate(words[:1500])]


def test_common_prefix_and_longest_prefix_on_both_ordered_indexes():
    keys = sorted({"", "a", "ap", "app", "apple", "apples", "b", "é", "éc", "école"})
    for idx in (lexindex.StringIndex(keys), lexindex.DictIndex(keys, block=3)):
        for q in ["", "a", "app", "apples!", "éc", "école!", "z", "apple\U0010ffff"]:
            want = [(k, i) for i, k in enumerate(keys) if q.startswith(k)]
            assert idx.common_prefix(q) == want, (idx, q)
            assert idx.longest_prefix(q) == (want[-1] if want else None), (idx, q)
    # Without the empty key nothing matches a query that starts no entry.
    for idx in (lexindex.StringIndex(["b"]), lexindex.DictIndex(["b"])):
        assert idx.common_prefix("a") == [] and idx.longest_prefix("a") is None


def test_dict_index_build_to_file_writes_what_the_constructor_would(tmp_path):
    words = [f"token-{i * 7919 % 10007:05}" for i in range(20_000)]  # duplicates fold
    distinct = sorted(set(words))
    for block in (1, 32, 1024):
        p = tmp_path / "words.bdx"  # a pathlib.Path, not a str
        written = lexindex.DictIndex.build_to_file((w for w in words), p, block=block)
        assert written == len(distinct)
        assert p.read_bytes() == lexindex.DictIndex(words, block=block).to_bytes()
        assert lexindex.DictIndex.load(p).key(17) == distinct[17]
    assert [e.name for e in tmp_path.iterdir()] == ["words.bdx"]  # no scratch left beside it
    with pytest.raises(ValueError, match="block"):
        lexindex.DictIndex.build_to_file(words, tmp_path / "x.bdx", block=0)


def test_dict_index_build_to_file_aborts_when_the_iterable_raises(tmp_path):
    p = tmp_path / "words.bdx"
    p.write_bytes(b"previous")

    def raising():
        yield "alpha"
        yield "beta"
        raise RuntimeError("source failed")

    with pytest.raises(RuntimeError, match="source failed"):
        lexindex.DictIndex.build_to_file(raising(), p)
    assert p.read_bytes() == b"previous"
    with pytest.raises(TypeError):
        lexindex.DictIndex.build_to_file([1, 2], p)
    assert [e.name for e in tmp_path.iterdir()] == ["words.bdx"]


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


def test_string_index_order_statistics():
    si = lexindex.StringIndex(["apple", "apricot", "banana", "cherry"])
    # lower_bound answers for any string, present or not, and never exceeds len
    assert si.lower_bound("apple") == 0
    assert si.lower_bound("ba") == 2
    assert si.lower_bound("zzz") == len(si) == 4
    # a prefix is a contiguous slice of the id space, not a set of ids
    assert si.prefix_id_range("ap") == (0, 2)
    assert si.prefix_count("ap") == 2
    assert [si.id(k) for k, _ in si.prefix("ap")] == list(range(*si.prefix_id_range("ap")))
    # a prefix nothing carries is empty rather than absent
    assert si.prefix_id_range("z") == (4, 4)
    assert si.prefix_count("z") == 0
    # an empty prefix is the whole id space
    assert si.prefix_id_range("") == (0, 4)
    # counting a range agrees with listing it, without decoding the keys
    assert si.range_count("apricot", "cherry") == len(si.range("apricot", "cherry")) == 2
    assert si.range_count("cherry", "apricot") == 0


def test_string_index_order_statistics_are_byte_ordered():
    # Ids follow UTF-8 byte order, so a multibyte key sorts after every ASCII one -- and the
    # exclusive end of a prefix range is built by incrementing a byte, which is where a
    # char-shaped assumption would break.
    si = lexindex.StringIndex(["zebra", "\u00e9clair", "\u00e9t\u00e9", "\u4e2d\u6587"])
    assert [k for k, _ in si] == ["zebra", "\u00e9clair", "\u00e9t\u00e9", "\u4e2d\u6587"]
    assert si.prefix_id_range("\u00e9") == (1, 3)
    assert si.prefix_count("\u00e9") == 2
    assert si.lower_bound("\u00e9") == 1
    assert si.prefix_id_range("\u4e2d") == (3, 4)


def test_string_index_iter():
    si = lexindex.StringIndex(["banana", "apple", "apricot", "cherry"])
    # __iter__ yields every (key, id) in sorted (= id) order, lazily
    assert list(si) == [("apple", 0), ("apricot", 1), ("banana", 2), ("cherry", 3)]
    # a fresh iterator each time — iteration is repeatable
    assert [k for k, _ in si] == ["apple", "apricot", "banana", "cherry"]
    assert dict(si)["banana"] == 2
    assert list(lexindex.StringIndex([])) == []


def test_string_index_iter_crosses_the_refill_boundary():
    # The iterator streams the transducer a chunk at a time and resumes from the last key it handed
    # out, so the seam between chunks is the edge case: a key repeated or dropped there would be
    # invisible on the four-key index above. 2 500 keys crosses it twice.
    keys = [f"item-{i:05}" for i in range(2500)]
    si = lexindex.StringIndex(keys)
    assert list(si) == [(k, i) for i, k in enumerate(sorted(keys))]
    # Still lazy: taking the first few must not walk the rest.
    assert list(itertools.islice(iter(si), 3)) == [
        ("item-00000", 0),
        ("item-00001", 1),
        ("item-00002", 2),
    ]
    # Exactly one chunk, and one key past it, are the off-by-one candidates.
    for n in (1023, 1024, 1025, 2048, 2049):
        assert len(lexindex.StringIndex(keys[:n]).__iter__().__next__()) == 2
        assert len(list(lexindex.StringIndex(keys[:n]))) == n


def test_string_index_from_sorted(tmp_path):
    keys = ["apple", "apricot", "apricot", "banana"]
    si = lexindex.StringIndex.from_sorted(iter(keys))
    # Same index the constructor builds from the same key set: ids are ranks, so any disagreement
    # would renumber everything after the first difference.
    assert si.to_bytes() == lexindex.StringIndex(keys).to_bytes()
    assert list(si) == [("apple", 0), ("apricot", 1), ("banana", 2)]

    path = tmp_path / "sorted.bix"
    assert lexindex.StringIndex.build_sorted_to_file(iter(keys), path) == 3
    assert lexindex.StringIndex.load(path).to_bytes() == si.to_bytes()

    # A generator is consumed lazily -- this one would blow up if it were materialised first.
    def huge():
        for i in range(200_000):
            yield f"k{i:08}"

    assert lexindex.StringIndex.build_sorted_to_file(huge(), path) == 200_000
    assert len(lexindex.StringIndex.load(path)) == 200_000


def test_string_index_from_sorted_rejects_bad_input(tmp_path):
    with pytest.raises(ValueError):
        lexindex.StringIndex.from_sorted(["b", "a"])
    with pytest.raises(TypeError):
        lexindex.StringIndex.from_sorted(["a", 7])

    path = tmp_path / "aborted.bix"
    lexindex.StringIndex.build_sorted_to_file(["x", "y"], path)
    before = path.read_bytes()

    def raising():
        yield "a"
        raise RuntimeError("boom")

    # An iterable that raises halfway looks to the builder like one that ended. The build must
    # abandon the write rather than publish a truncated index over what was already there.
    with pytest.raises(RuntimeError):
        lexindex.StringIndex.build_sorted_to_file(raising(), path)
    assert path.read_bytes() == before
    assert not list(path.parent.glob("*.tmp"))


def test_perfect_hash_build_to_file(tmp_path):
    words = [f"w{i:05}.{i * 7919 % 1000:03}" for i in range(5_000)]
    path = tmp_path / "streamed.bmp"
    calls = 0

    def source():
        nonlocal calls
        calls += 1
        return (w for w in words)

    # The factory is called twice: once to hash the keys, once to place them.
    assert lexindex.PerfectHashIndex.build_to_file(source, path) == len(words)
    assert calls == 2
    idx = lexindex.PerfectHashIndex.load(path)
    assert len(idx) == len(words)
    ids = [idx.id(w) for w in words]
    assert sorted(ids) == list(range(len(words)))  # every key present, ids a permutation
    assert all(idx.key(i) == w for w, i in zip(words, ids, strict=True))
    assert idx.id("w99999.000") is None
    assert lexindex.PerfectHashIndex.load_mmap(path).id(words[0]) == ids[0]
    assert lexindex.PerfectHashIndex.build_to_file(lambda: iter(()), tmp_path / "empty.bmp") == 0


def test_perfect_hash_build_to_file_rejects_bad_input(tmp_path):
    path = tmp_path / "kept.bmp"
    lexindex.PerfectHashIndex.build_to_file(lambda: ["x", "y"], path)
    before = path.read_bytes()

    # The iterable itself instead of a factory is the likely mistake, and the message names it.
    with pytest.raises(TypeError, match="callable"):
        lexindex.PerfectHashIndex.build_to_file(["a", "b"], path)
    with pytest.raises(ValueError, match="distinct"):
        lexindex.PerfectHashIndex.build_to_file(lambda: ["a", "b", "a"], path)
    with pytest.raises(TypeError):
        lexindex.PerfectHashIndex.build_to_file(lambda: ["a", 7], path)

    # A second pass that yields different keys cannot be placed and must be refused.
    passes = iter([["a", "b"], ["a", "c"]])
    with pytest.raises(ValueError, match="replay"):
        lexindex.PerfectHashIndex.build_to_file(lambda: next(passes), path)

    # An exception in either pass propagates as itself, and the target stays byte-identical.
    def raising_first():
        yield "a"
        raise RuntimeError("boom")

    with pytest.raises(RuntimeError, match="boom"):
        lexindex.PerfectHashIndex.build_to_file(raising_first, path)

    calls = 0

    def raising_second():
        nonlocal calls
        calls += 1
        yield "a"
        yield "b"
        if calls == 2:
            raise RuntimeError("late boom")

    with pytest.raises(RuntimeError, match="late boom"):
        lexindex.PerfectHashIndex.build_to_file(raising_second, path)
    assert path.read_bytes() == before
    assert not list(path.parent.glob("*.tmp"))


def test_version_is_exposed():
    v = lexindex.__version__
    assert isinstance(v, str) and v  # non-empty string
    assert v[0].isdigit() and "." in v  # looks like a real version (installed metadata)


def test_build_releases_the_gil():
    """A background thread must keep running while a large index is built.

    Without ``Python::detach`` in the bindings the interpreter is frozen for the whole build and
    the counter barely moves (measured: 1 tick over 268 ms). The threshold is deliberately loose
    so a loaded CI runner cannot make this flaky -- it separates "released" from "not released",
    not one speed from another.

    The key count is sized so the build stays well above the "long enough to mean anything" guard
    below; 400 000 keys used to take 268 ms and now take 49, so it was raised rather than letting
    the test start passing vacuously.
    """
    import threading

    keys = [f"gil-probe-{i:07d}" for i in range(1_500_000)]
    ticks = 0
    stop = threading.Event()

    def spin():
        nonlocal ticks
        while not stop.is_set():
            ticks += 1
            time.sleep(0.001)

    th = threading.Thread(target=spin, daemon=True)
    th.start()
    try:
        time.sleep(0.05)  # let the spinner get going
        before = ticks
        start = time.perf_counter()
        lexindex.StringIndex(keys)
        elapsed = time.perf_counter() - start
    finally:
        stop.set()
        th.join(timeout=5)

    during = ticks - before
    assert elapsed > 0.05, f"build too fast ({elapsed * 1e3:.0f} ms) to tell anything"
    assert during >= 5, f"GIL held during build: {during} ticks over {elapsed * 1e3:.0f} ms"


def test_query_limit_truncates_and_matches_unlimited():
    si = lexindex.StringIndex([f"word-{i:04d}" for i in range(500)])
    full = si.prefix("word-01")
    assert len(full) == 100  # word-0100..word-0199
    # limit returns exactly the first n of the unlimited result, in the same order
    for n in (0, 1, 7, 99, 100, 250):
        assert si.prefix("word-01", limit=n) == full[:n]
    # a limit larger than the match count is not an error, just everything
    assert si.prefix("word-01", limit=10_000) == full
    # limit=None is the default and means unlimited
    assert si.prefix("word-01", limit=None) == full

    r = si.range("word-0100", "word-0200")
    assert si.range("word-0100", "word-0200", limit=5) == r[:5]
    s = si.subsequence("w0")
    assert si.subsequence("w0", limit=3) == s[:3]
    f = si.fuzzy("word-0100", 1)
    assert si.fuzzy("word-0100", 1, limit=2) == f[:2]


@pytest.mark.parametrize(
    "ctor",
    [
        lexindex.StringIndex,
        lexindex.PerfectHashIndex,
        lexindex.CompactHashIndex,
        lexindex.ClosedHashIndex,
        lexindex.DictIndex,
    ],
)
def test_bulk_arguments_reject_non_strings(ctor):
    """Keys are read as borrowed views of the Python `str` rather than copied into `String`.

    That is a different extractor, so pin the type contract it must keep: `str` only, and a clear
    `TypeError` — never a silent coercion of `bytes` or an integer.
    """
    idx = ctor(["delta", "alpha", "charlie", "bravo"])
    with pytest.raises(TypeError):
        ctor([1, 2])
    with pytest.raises(TypeError):
        ctor([b"alpha"])
    with pytest.raises(TypeError):
        idx.ids_of([b"alpha"])


def test_multibyte_keys_survive_the_borrowed_path():
    """The borrowed view is the Python string's UTF-8; non-ASCII must round-trip byte for byte."""
    words = ["\u65e5\u672c\u8a9e", "\u0451\u0436", "na\u00efve", "a b"]
    si = lexindex.StringIndex(words)
    assert sorted(words) == [si.key(i) for i in range(len(si))]
    assert si.ids_of(words) == [si.id(w) for w in words]

    ph = lexindex.PerfectHashIndex(words)
    assert ph.keys_of(ph.ids_of(words)) == words


@pytest.mark.parametrize(
    "ctor",
    [
        lexindex.StringIndex,
        lexindex.DictIndex,
        lexindex.PerfectHashIndex,
        lambda items: lexindex.CompactHashIndex(items, 1),
    ],
)
def test_builds_from_a_generator_and_takes_pathlike(ctor, tmp_path):
    """Constructors take any iterable (a generator over a corpus, not just a materialised list),
    and every path argument takes `os.PathLike` as well as `str`."""
    words = ["delta", "alpha", "charlie", "bravo"]
    idx = ctor(w for w in words)
    assert len(idx) == 4
    path = tmp_path / "idx.bin"  # a pathlib.Path, not a str
    idx.save(path)
    assert type(idx).load(path).id("alpha") == idx.id("alpha")
    assert type(idx).load_mmap(path).id("alpha") == idx.id("alpha")


def test_subsequence_matches_whole_characters():
    """A multi-byte query character must not match its bytes scattered across two characters:
    'é' is [C3 A9] and 'àΩ' is [C3 A0 CE A9]."""
    idx = lexindex.StringIndex(["\u00e0\u03a9", "caf\u00e9", "\u00e8\u00e9"])
    assert [k for k, _ in idx.subsequence("\u00e9")] == ["caf\u00e9", "\u00e8\u00e9"]


@pytest.mark.parametrize(
    "ctor",
    [
        lexindex.StringIndex,
        lexindex.PerfectHashIndex,
        lambda items: lexindex.CompactHashIndex(items, 4),
        lexindex.DictIndex,
    ],
)
def test_ids_of_bytes_matches_ids_of(ctor):
    """The packed buffer carries the same answers as the list, with MISSING_ID for absent keys."""
    words = ["alpha", "bravo", "charlie", "delta"]
    idx = ctor(words)
    probes = ["delta", "zulu", "alpha", "bravo"]
    cls = type(idx)
    width = 8 if cls.ID_DTYPE == "uint64" else 4
    raw = idx.ids_of_bytes(probes)
    assert len(raw) == len(probes) * width
    unpacked = [
        int.from_bytes(raw[i * width : (i + 1) * width], sys.byteorder) for i in range(len(probes))
    ]
    expected = [cls.MISSING_ID if i is None else i for i in idx.ids_of(probes)]
    assert unpacked == expected
    assert unpacked[1] == cls.MISSING_ID  # "zulu" was never a key


def test_ids_of_bytes_is_empty_for_no_keys():
    idx = lexindex.PerfectHashIndex(["alpha"])
    assert idx.ids_of_bytes([]) == b""


def test_ids_of_bytes_reads_zero_copy_through_numpy():
    np = _numpy_or_skip()
    words = [f"w{i}" for i in range(1000)]
    idx = lexindex.PerfectHashIndex(words)
    probes = [*words[::3], "absent"]
    buf = idx.ids_of_bytes(probes)
    arr = np.frombuffer(buf, dtype=idx.ID_DTYPE)
    assert arr.shape == (len(probes),)
    assert arr[-1] == idx.MISSING_ID
    present = arr[arr != idx.MISSING_ID]
    assert present.tolist() == [idx.id(k) for k in probes if idx.id(k) is not None]
    # `frombuffer` shares the bytes rather than copying them.
    assert arr.base is buf


@pytest.mark.parametrize(
    "ctor",
    [
        lexindex.StringIndex,
        lexindex.PerfectHashIndex,
        lambda items: lexindex.CompactHashIndex(items, 4),
        lexindex.DictIndex,
    ],
)
def test_ids_into_writes_the_head_of_a_buffer_and_leaves_the_tail(ctor):
    """Same items as ids_of_bytes, written in place; a longer buffer keeps its tail."""
    words = ["alpha", "bravo", "charlie", "delta"]
    idx = ctor(words)
    probes = ["delta", "zulu", "alpha", "bravo"]
    cls = type(idx)
    out = array.array("Q" if cls.ID_DTYPE == "uint64" else "I", [7] * (len(probes) + 2))
    assert idx.ids_into(probes, out) is None
    expected = [cls.MISSING_ID if i is None else i for i in idx.ids_of(probes)]
    assert out[: len(probes)].tolist() == expected
    assert out[len(probes) :].tolist() == [7, 7]
    assert out.tobytes()[: len(probes) * out.itemsize] == idx.ids_of_bytes(probes)
    # Nothing to write: any writable buffer will do, even an empty `array.array`, whose buffer
    # pointer is unaligned on some interpreter builds.
    idx.ids_into([], array.array(out.typecode))
    idx.ids_into([], bytearray())
    with pytest.raises(BufferError):
        idx.ids_into([], b"")  # read-only is still refused
    with pytest.raises(TypeError):
        idx.ids_into([], 42)  # not a buffer at all


def test_ids_into_refuses_a_short_readonly_or_mistyped_buffer():
    idx = lexindex.PerfectHashIndex(["alpha", "bravo"])
    with pytest.raises(ValueError, match="1 items but 2 keys"):
        idx.ids_into(["alpha", "bravo"], array.array("I", [0]))
    with pytest.raises(BufferError, match="read-only"):
        idx.ids_into(["alpha"], memoryview(bytearray(4)).toreadonly().cast("I"))
    with pytest.raises(BufferError):
        idx.ids_into(["alpha"], array.array("Q", [0]))  # 8-byte items for a uint32 index
    with pytest.raises(BufferError):
        idx.ids_into(["alpha"], bytearray(4))  # bytes-typed, not uint32-typed
    with pytest.raises(BufferError):
        lexindex.StringIndex(["alpha"]).ids_into(["alpha"], array.array("I", [0]))


def test_ids_into_fills_a_numpy_array_in_place():
    np = _numpy_or_skip()
    words = [f"w{i}" for i in range(1000)]
    idx = lexindex.StringIndex(words)
    probes = [*words[::3], "absent"]
    out = np.empty(len(probes), dtype=idx.ID_DTYPE)
    idx.ids_into(probes, out)
    assert out[-1] == idx.MISSING_ID
    assert out[:-1].tolist() == [idx.id(k) for k in probes[:-1]]
    with pytest.raises(BufferError, match="C-contiguous"):
        idx.ids_into(probes[: len(probes) // 2], out[::2])
    with pytest.raises(BufferError):
        idx.ids_into(probes, np.empty(len(probes), dtype="uint32"))


def test_overlay_core():
    ov = lexindex.Overlay(lexindex.StringIndex(["apple", "banana"]))
    cherry = ov.add("cherry")
    assert cherry == 2 and len(ov) == 3 and ov.id_space() == 3
    assert ov.key(cherry) == "cherry" and "cherry" in ov
    assert ov.remove("apple") and ov.id("apple") is None and len(ov) == 2
    assert not ov.remove("apple")  # removing twice is not a second removal
    assert ov.keys() == ["banana", "cherry"]
    assert ov.add("durian") == 3, "a retired id is never handed to the next addition"
    assert ov.add("apple") == 0, "re-adding revives the original id"


def test_overlay_shares_the_base_rather_than_taking_it():
    si = lexindex.StringIndex(["apple", "banana"])
    ov = lexindex.Overlay(si)
    ov.remove("apple")
    assert si.id("apple") == 0, "the base the caller still holds is untouched"
    assert len(si) == 2
    assert isinstance(ov.base(), lexindex.StringIndex)
    assert ov.base().id("apple") == 0


def test_overlay_rejects_something_that_is_not_an_index():
    with pytest.raises(TypeError):
        lexindex.Overlay(["apple"])


@pytest.mark.parametrize(
    "ctor", [lexindex.StringIndex, lexindex.PerfectHashIndex], ids=["string", "perfect"]
)
def test_overlay_matches_a_set_through_every_edit(ctor):
    """The Python mirror of the Rust property test, on a fixed seed.

    A rebuilt index numbers differently by construction, so "equals the index rebuilt from the same
    operations" is not checkable. What is, and what a caller relies on: the key set matches, every
    live key reads back through its id, and an id once issued never comes to mean a different key.
    """
    rng = random.Random(20260906)
    universe = [f"k{i}" for i in range(12)]
    initial = universe[:5]
    ov = lexindex.Overlay(ctor(initial))
    model = set(initial)
    issued = {ov.id(k): k for k in initial}

    for _ in range(200):
        key = rng.choice(universe)
        if rng.random() < 0.5:
            issued.setdefault(ov.add(key), key)
            model.add(key)
        else:
            assert ov.remove(key) == (key in model)
            model.discard(key)
        assert len(ov) == len(model)
        for k in universe:
            assert (k in ov) == (k in model)
            if (i := ov.id(k)) is not None:
                assert ov.key(i) == k
        for i, k in issued.items():
            back = ov.key(i)
            assert back == k or (back is None and k not in model)

    assert set(ov.keys()) == model
    assert set(ov.compact().keys()) == model


@pytest.mark.parametrize(
    "ctor", [lexindex.StringIndex, lexindex.PerfectHashIndex], ids=["string", "perfect"]
)
def test_overlay_blob_round_trips(ctor, tmp_path):
    ov = lexindex.Overlay(ctor(["apple", "banana", "cherry"]))
    ov.add("durian")
    assert ov.remove("banana")
    for back in (
        lexindex.Overlay.from_bytes(ov.to_bytes(), ctor),
        _saved_and_loaded(ov, tmp_path / "ov.bin", ctor),
    ):
        assert len(back) == len(ov)
        assert back.id_space() == ov.id_space(), "the retired ids survive the blob"
        assert back.keys() == ov.keys()
        assert back.id("banana") is None
        assert back.add("elderberry") == ov.id_space()


def _saved_and_loaded(ov, path, ctor):
    ov.save(path)
    return lexindex.Overlay.load(path, ctor)


def _edited_overlay(ctor):
    ov = lexindex.Overlay(ctor(["apple", "banana", "cherry"]))
    ov.add("durian")
    ov.add("fig")
    assert ov.remove("banana") and ov.remove("fig")
    return ov


@pytest.mark.parametrize(
    "ctor", [lexindex.StringIndex, lexindex.PerfectHashIndex], ids=["string", "perfect"]
)
def test_overlay_compact_to_file_writes_the_compacted_base(ctor, tmp_path):
    ov = _edited_overlay(ctor)
    assert ov.compact_to_file(tmp_path / "base.bin") == len(ov) == 3
    assert (tmp_path / "base.bin").read_bytes() == ov.compact().base().to_bytes()
    back = lexindex.Overlay(ctor.load(tmp_path / "base.bin"))
    assert sorted(back.keys()) == ["apple", "cherry", "durian"]


@pytest.mark.parametrize(
    "ctor", [lexindex.StringIndex, lexindex.PerfectHashIndex], ids=["string", "perfect"]
)
def test_overlay_compact_with_remap_carries_every_live_id(ctor):
    ov = _edited_overlay(ctor)
    old = {i: ov.key(i) for i in range(ov.id_space())}
    fresh, remap = ov.compact_with_remap()
    assert len(remap) == 8 * ov.id_space()
    ids = [int.from_bytes(remap[i * 8 : (i + 1) * 8], sys.byteorder) for i in old]
    for i, key in old.items():
        if key is None:
            assert ids[i] == 2**64 - 1
        else:
            assert fresh.key(ids[i]) == key
    assert len(fresh) == 3 and len(ov) == 3, "the overlay it was taken from is untouched"


def test_overlay_compact_family_needs_a_keyed_base(tmp_path):
    ov = lexindex.Overlay(lexindex.CompactHashIndex(["apple"], 2))
    with pytest.raises(TypeError, match="stores no keys"):
        ov.compact_to_file(tmp_path / "base.bin")
    with pytest.raises(TypeError, match="stores no keys"):
        ov.compact_with_remap()


def test_overlay_blob_refuses_the_wrong_base():
    ov = lexindex.Overlay(lexindex.StringIndex(["apple"]))
    blob = ov.to_bytes()
    with pytest.raises(ValueError, match="different base index"):
        lexindex.Overlay.from_bytes(blob, lexindex.PerfectHashIndex)
    with pytest.raises(TypeError):
        lexindex.Overlay.from_bytes(blob, dict)


def test_overlay_loaders_are_checksummed():
    """The overlay blob carries a header check and a payload hash, like every other blob here.

    A flipped bit in an addition that stays valid UTF-8 used to load as a different key, and one in
    a tombstone word used to revive a removed id — silently. Both are refused now, and this asserts
    it from the Python side, where there is no way to reach the Rust unit tests. Every byte is
    tried, so the header, the embedded base blob, the additions and the tombstones are all covered.
    """
    ov = lexindex.Overlay(lexindex.StringIndex(["apple", "banana", "cherry"]))
    ov.add("date")
    ov.remove("banana")
    blob = ov.to_bytes()
    assert blob[:4] == b"OVL2"
    assert len(lexindex.Overlay.from_bytes(blob, lexindex.StringIndex)) == 3

    for pos in range(len(blob)):
        bad = bytearray(blob)
        bad[pos] ^= 0x01
        with pytest.raises(ValueError):
            lexindex.Overlay.from_bytes(bytes(bad), lexindex.StringIndex)


def test_overlay_save_replaces_an_existing_file_whole(tmp_path):
    """``save`` goes through the crate's atomic writer, like the three indexes."""
    path = tmp_path / "overlay.bin"
    big = lexindex.Overlay(lexindex.StringIndex(["apple", "banana", "cherry"]))
    big.add("date")
    big.save(path)
    assert path.stat().st_size > 0

    # A shorter overlay over the same path: a non-atomic rewrite could leave the longer one's tail.
    small = lexindex.Overlay(lexindex.StringIndex(["apple"]))
    small.save(path)
    assert lexindex.Overlay.load(path, lexindex.StringIndex).keys() == ["apple"]
    assert not list(tmp_path.glob("*.tmp"))


def test_overlay_over_a_compact_hash_has_membership_and_nothing_else():
    ov = lexindex.Overlay(lexindex.CompactHashIndex(["alpha", "beta"], 4))
    assert ov.add("gamma") == 2
    assert ov.remove("alpha") and ov.id("alpha") is None and len(ov) == 2
    assert "beta" in ov and "gamma" in ov
    for call in (lambda: ov.key(0), ov.keys, ov.compact):
        with pytest.raises(TypeError, match="stores no keys"):
            call()
    back = lexindex.Overlay.from_bytes(ov.to_bytes(), lexindex.CompactHashIndex)
    assert len(back) == 2 and back.id("alpha") is None


def _hammer(fn, threads=8):
    """Run `fn(worker_index)` on `threads` threads released together, re-raising the first failure.

    On a free-threaded interpreter these run genuinely in parallel; on a GIL build they interleave.
    The tests below are written to hold either way, so one suite covers both.
    """
    barrier = threading.Barrier(threads)
    failures = []
    results = [None] * threads

    def run(i):
        try:
            barrier.wait()
            results[i] = fn(i)
        except BaseException as e:  # re-raised below, in the caller's thread
            failures.append(e)

    workers = [threading.Thread(target=run, args=(i,)) for i in range(threads)]
    for w in workers:
        w.start()
    for w in workers:
        w.join()
    if failures:
        raise failures[0]
    return results


@pytest.mark.parametrize(
    "ctor",
    [
        lexindex.StringIndex,
        lexindex.PerfectHashIndex,
        lambda items: lexindex.CompactHashIndex(items, 4),
        lexindex.DictIndex,
    ],
)
def test_an_index_is_safe_to_share_across_threads(ctor):
    """The indexes are immutable after building, which is what lets the module tell CPython it does
    not need the GIL. Eight threads querying one index must all get the right answers."""
    words = [f"w{i:04d}" for i in range(2000)]
    idx = ctor(words)

    def query(_):
        return [idx.id(w) for w in words[::20]]

    seen = _hammer(query)
    assert all(r == seen[0] for r in seen)
    assert None not in seen[0]


def test_a_shared_iterator_serialises_rather_than_raising():
    """A shared iterator is a strange thing to build, but `RuntimeError: Already borrowed` is a
    worse answer than serialising: every key comes out exactly once, split across the threads."""
    words = [f"w{i:04d}" for i in range(2000)]
    it = iter(lexindex.StringIndex(words))

    def drain(_):
        out = []
        while (item := next(it, None)) is not None:
            out.append(item[0])
        return out

    taken = [k for chunk in _hammer(drain) for k in chunk]
    assert sorted(taken) == words
    assert len(taken) == len(set(taken)), "a key was handed out twice"


def test_a_shared_overlay_serialises_its_edits():
    ov = lexindex.Overlay(lexindex.StringIndex(["seed"]))

    def edit(i):
        return [ov.add(f"t{i}-{j}") for j in range(50)]

    ids = [i for chunk in _hammer(edit) for i in chunk]
    assert len(ids) == len(set(ids)), "two threads were handed the same id"
    assert len(ov) == 1 + 8 * 50
    assert ov.id_space() == 1 + 8 * 50


def _ids_in_child(indexes, keys):
    """Run in a spawned worker: every index arrives by pickle, nothing is inherited."""
    return [[idx.id(k) for k in keys] for idx in indexes]


def test_pickle_round_trips_every_class():
    words = ["apple", "apricot", "banana", "cherry"]
    si = lexindex.StringIndex(words)
    ph = lexindex.PerfectHashIndex(words)
    ch = lexindex.CompactHashIndex(words, 1)
    cl = lexindex.ClosedHashIndex(words)
    di = lexindex.DictIndex(words)
    ov = lexindex.Overlay(si)
    ov.add("durian")
    ov.remove("apple")

    # Protocol 2 as well as the default: `__reduce__` names a static method by qualname, which is
    # the part of the protocol that differs between them.
    for protocol in (2, pickle.HIGHEST_PROTOCOL):
        for original in (si, ph, ch, cl, di, ov):
            back = pickle.loads(pickle.dumps(original, protocol=protocol))
            assert type(back) is type(original)
            assert len(back) == len(original)
            for key in [*words, "durian", "absent"]:
                assert back.id(key) == original.id(key), (type(original), key, protocol)

    # The reverse map survives too, where there is one.
    back = pickle.loads(pickle.dumps(ph))
    assert [back.key(i) for i in range(len(ph))] == [ph.key(i) for i in range(len(ph))]


def test_pickle_copies_a_memory_mapped_index_by_value(tmp_path):
    """A mapped index pickles its bytes, so the copy outlives the file it was borrowing."""
    path = tmp_path / "words.bix"
    lexindex.StringIndex(["apple", "banana"]).save(path)
    mapped = lexindex.StringIndex.load_mmap(path)
    blob = pickle.dumps(mapped)
    del mapped
    path.unlink()

    back = pickle.loads(blob)
    assert back.id("banana") == 1
    assert back.key(0) == "apple"


def test_pickle_survives_a_spawned_worker():
    """`spawn` shares nothing, so the worker gets each index only if `__reduce__` is right."""
    words = ["apple", "apricot", "banana", "cherry"]
    si = lexindex.StringIndex(words)
    indexes = [
        si,
        lexindex.PerfectHashIndex(words),
        lexindex.CompactHashIndex(words, 1),
        lexindex.Overlay(si),
    ]
    ctx = multiprocessing.get_context("spawn")
    with ctx.Pool(1) as pool:
        got = pool.apply_async(_ids_in_child, (indexes, words)).get(timeout=120)
    assert got == [[idx.id(k) for k in words] for idx in indexes]


def test_dict_style_access_on_every_class():
    words = ["apple", "apricot", "banana"]
    si = lexindex.StringIndex(words)
    ov = lexindex.Overlay(si)
    ov.add("durian")
    # 4-byte fingerprint on the compact index: a miss below is then a miss, not a 1-in-256
    # false positive that would make this test flaky.
    for idx in (si, lexindex.PerfectHashIndex(words), lexindex.CompactHashIndex(words, 4), ov):
        assert idx["apple"] == idx.id("apple")
        assert idx.get("apple") == idx.id("apple")
        assert idx.get("nope") is None
        assert idx.get("nope", -1) == -1
        assert idx.get("nope", "absent") == "absent"
        with pytest.raises(KeyError, match="nope"):
            idx["nope"]
    assert ov["durian"] == ov.id("durian")


def test_the_indexes_are_not_mappings():
    """`__getitem__` is a lookup, not a promise of iteration: nothing here walks by index."""
    ph = lexindex.PerfectHashIndex(["apple", "banana"])
    for absent in ("items", "keys", "values", "__setitem__"):
        assert not hasattr(ph, absent), absent
    # Defining `__getitem__` alone revives the legacy sequence protocol, so `list()` reaches for
    # `ph[0]` and gets a TypeError from the key type rather than iterating anything.
    with pytest.raises(TypeError):
        list(ph)
    # StringIndex does iterate, and through `__iter__` -- ordered pairs, not integer indexing.
    assert next(iter(lexindex.StringIndex(["apple", "banana"]))) == ("apple", 0)


def _specimen() -> bytes:
    """The 111-byte `StringIndex` blob that panics the ordinary loader (see tests/golden.rs)."""
    path = os.path.join(os.path.dirname(__file__), "data", "panicking-1.0.0-string.bix")
    with open(path, "rb") as f:
        blob = f.read()
    assert len(blob) == 111
    return blob


def test_untrusted_loader_refuses_the_specimen_with_a_value_error():
    """The blob `from_bytes` panics on is a `ValueError` here -- the whole point of the binding."""
    blob = _specimen()
    with pytest.raises(ValueError):
        lexindex.StringIndex.from_untrusted_bytes(blob)

    # And the exception is not the panic: PanicException derives from BaseException, so a bare
    # `except ValueError` would miss it and a caller would see the process's panic path instead.
    try:
        lexindex.StringIndex.from_untrusted_bytes(blob)
    except BaseException as e:  # the type is exactly what is under test
        assert type(e).__name__ == "ValueError", type(e).__name__


def test_untrusted_loader_accepts_a_real_blob():
    words = ["apple", "apricot", "banana"]
    blob = lexindex.StringIndex(words).to_bytes()
    idx = lexindex.StringIndex.from_untrusted_bytes(blob)
    assert [idx.key(i) for i in range(len(idx))] == words
    assert idx.id("banana") == 2


def test_overlay_untrusted_loader_refuses_a_hostile_base():
    """An overlay frame can be perfect and its base still hostile: the loader is the whole defence.

    The fixture is the 111-byte specimen sealed into a real `OVL2` frame (written by the Rust test
    that pins it, since both checksums are the crate's). Every check the overlay makes for itself
    passes, which is what the first assertion proves: the ordinary loader gets far enough to panic.
    """
    path = os.path.join(os.path.dirname(__file__), "data", "panicking-1.0.0-overlay.ovl")
    with open(path, "rb") as f:
        hostile = f.read()

    with pytest.raises(BaseException) as panicked:  # PanicException is not an Exception
        lexindex.Overlay.from_bytes(hostile, lexindex.StringIndex)
    assert type(panicked.value).__name__ == "PanicException", (
        "the frame no longer reaches the base loader, so this test proves nothing about it"
    )

    with pytest.raises(ValueError):
        lexindex.Overlay.from_untrusted_bytes(hostile, lexindex.StringIndex)


def test_overlay_untrusted_loader_accepts_a_real_blob():
    ov = lexindex.Overlay(lexindex.StringIndex(["apple", "banana"]))
    ov.add("cherry")
    back = lexindex.Overlay.from_untrusted_bytes(ov.to_bytes(), lexindex.StringIndex)
    assert len(back) == 3
    assert back.id("cherry") == ov.id("cherry")


def test_path_forms_of_the_strict_loader_refuse_the_specimen_file(tmp_path):
    p = str(tmp_path / "hostile.bix")
    with open(p, "wb") as f:
        f.write(_specimen())
    with pytest.raises(ValueError):
        lexindex.StringIndex.load_untrusted(p)
    with pytest.raises(ValueError):
        lexindex.StringIndex.load_mmap_untrusted(p)
    hostile = os.path.join(os.path.dirname(__file__), "data", "panicking-1.0.0-overlay.ovl")
    with pytest.raises(ValueError):
        lexindex.Overlay.load_untrusted(hostile, lexindex.StringIndex)


def test_path_forms_of_the_strict_loader_accept_a_real_file(tmp_path):
    words = ["apple", "apricot", "banana"]
    p = str(tmp_path / "own.bix")
    lexindex.StringIndex(words).save(p)
    for idx in [
        lexindex.StringIndex.load_untrusted(p),
        lexindex.StringIndex.load_mmap_untrusted(p),
        lexindex.StringIndex.load_mmap_verified(p),
    ]:
        assert [idx.key(i) for i in range(len(idx))] == words
    ov = lexindex.Overlay(lexindex.StringIndex(words))
    ov.add("cherry")
    q = str(tmp_path / "own.ovl")
    ov.save(q)
    assert lexindex.Overlay.load_untrusted(q, lexindex.StringIndex).id("cherry") == ov.id("cherry")


@pytest.mark.parametrize(
    "ctor",
    [
        lambda keys: lexindex.StringIndex(keys),
        lambda keys: lexindex.DictIndex(keys),
        lambda keys: lexindex.PerfectHashIndex(keys),
        lambda keys: lexindex.CompactHashIndex(keys, 4),
    ],
)
def test_load_mmap_verified_refuses_a_flipped_byte_the_plain_mapping_takes(tmp_path, ctor):
    idx = ctor(["apple", "banana"])
    p = str(tmp_path / "idx.blob")
    idx.save(p)
    assert len(type(idx).load_mmap_verified(p)) == 2
    with open(p, "rb") as f:
        data = bytearray(f.read())
    data[-1] ^= 0x55  # the last byte is payload: a CRC byte, a code, a key byte, a fingerprint
    with open(p, "wb") as f:
        f.write(data)
    assert len(type(idx).load_mmap(p)) == 2  # no payload checksum, by design
    with pytest.raises(ValueError):
        type(idx).load_mmap_verified(p)
    with pytest.raises(ValueError):
        type(idx).load(p)


def test_inspect_reads_the_header_of_every_index(tmp_path):
    keys = ["apple", "banana", "cherry"]
    for idx, kind, fmt in [
        (lexindex.StringIndex(keys), "StringIndex", "BIX4"),
        (lexindex.PerfectHashIndex(keys), "PerfectHashIndex", "BMP7"),
        (lexindex.CompactHashIndex(keys, 2), "CompactHashIndex", "BCH7"),
        (lexindex.ClosedHashIndex(keys), "ClosedHashIndex", "BCL1"),
        (lexindex.DictIndex(keys), "DictIndex", "BDX3"),
    ]:
        blob = idx.to_bytes()
        info = lexindex.inspect(blob)
        assert (info["kind"], info["format"], info["keys"], info["bytes"]) == (
            kind,
            fmt,
            3,
            len(blob),
        )
        assert info["overlay"] is None
        path = tmp_path / f"{fmt}.blob"
        idx.save(path)
        assert lexindex.inspect(path) == info
        assert lexindex.inspect(str(path)) == info
    compact = lexindex.inspect(lexindex.CompactHashIndex(keys, 2).to_bytes())
    assert (compact["fingerprint_bits"], compact["arena_bytes"]) == (16, 6)
    assert compact["bytes"] == 40 + compact["mph_bytes"] + 6 + 20 * compact["side_entries"]
    info = lexindex.inspect(_edited_overlay(lexindex.StringIndex).to_bytes())
    assert (info["kind"], info["format"], info["keys"]) == ("Overlay", "OVL2", 3)
    overlay = info["overlay"]
    assert (overlay["base_tag"], overlay["additions"], overlay["retired"]) == (1, 2, 2)
    assert (overlay["base"]["kind"], overlay["base"]["keys"]) == ("StringIndex", 3)


def test_inspect_refuses_what_is_not_a_blob(tmp_path):
    with pytest.raises(ValueError, match="unknown magic"):
        lexindex.inspect(b"nope")
    with pytest.raises(ValueError, match="PerfectHashIndex"):
        lexindex.inspect(b"BMP4 from before 1.0")
    with pytest.raises(ValueError, match="truncated"):
        lexindex.inspect(b"BCH7 cut short")
    with pytest.raises(TypeError):
        lexindex.inspect(42)
    with pytest.raises(OSError):
        lexindex.inspect(tmp_path / "missing.blob")


def test_build_to_file_writes_what_the_constructor_would_save(tmp_path):
    keys = [f"k{(i * 7919) % 500:03}" for i in range(500)] * 2
    path = tmp_path / "ext.bix"
    assert lexindex.StringIndex.build_to_file(iter(keys), path) == 500
    assert path.read_bytes() == lexindex.StringIndex(keys).to_bytes()
    assert lexindex.StringIndex.load(path).id("k499") == 499
    assert [p.name for p in tmp_path.iterdir()] == ["ext.bix"]


def test_build_to_file_abandons_the_file_when_the_iterable_raises(tmp_path):
    path = tmp_path / "ext.bix"
    path.write_bytes(b"previous")

    def keys():
        yield "a"
        raise RuntimeError("boom")

    with pytest.raises(RuntimeError, match="boom"):
        lexindex.StringIndex.build_to_file(keys(), path)
    assert path.read_bytes() == b"previous"
    with pytest.raises(TypeError):
        lexindex.StringIndex.build_to_file([1, 2], path)
    assert [p.name for p in tmp_path.iterdir()] == ["ext.bix"]


def test_perfect_hash_fingerprints(tmp_path):
    keys = [f"key-{i}" for i in range(2000)]
    plain = lexindex.PerfectHashIndex(keys)
    fp = lexindex.PerfectHashIndex(keys, fingerprints=True)
    assert fp.has_fingerprints() and not plain.has_fingerprints()
    assert all(fp.id(k) == plain.id(k) for k in keys)
    assert fp.ids_of(keys) == plain.ids_of(keys)
    strangers = [k + "x" for k in keys]
    assert all(fp.id(k) is None for k in strangers)
    assert fp.ids_of(strangers) == [None] * len(strangers)
    assert len(fp.to_bytes()) == len(plain.to_bytes()) + len(keys)  # one byte per slot
    restored = lexindex.PerfectHashIndex.from_bytes(fp.to_bytes())
    assert restored.has_fingerprints() and restored.id("key-7") == fp.id("key-7")
    p = tmp_path / "fp.bmp"
    assert lexindex.PerfectHashIndex.build_to_file(lambda: iter(keys), p, fingerprints=True) == 2000
    assert p.read_bytes() == fp.to_bytes()
    assert lexindex.PerfectHashIndex.load_mmap(p).has_fingerprints()
    q = tmp_path / "plain.bmp"
    lexindex.PerfectHashIndex.build_to_file(lambda: iter(keys), q)
    assert not lexindex.PerfectHashIndex.load(q).has_fingerprints()


class _Utf8Column:
    """What ``ids_of_arrow`` reads off a pyarrow string array, without pyarrow: ``type``,
    ``offset``, ``null_count``, ``buffers()`` and ``len``. ``lead`` elements sit before ``offset``
    as they do in a sliced array; ``nulls`` are positions within the visible keys."""

    def __init__(self, keys, *, nulls=(), large=False, lead=0):
        full = ["padding"] * lead + list(keys)
        self.type = "large_string" if large else "string"
        self.offset = lead
        self._len = len(keys)
        data = bytearray()
        offsets = [0]
        for k in full:
            data += k.encode()
            offsets.append(len(data))
        self._offsets = array.array("q" if large else "i", offsets).tobytes()
        self._data = bytes(data)
        self.null_count = len(nulls)
        self._validity = None
        if nulls:
            bits = bytearray((len(full) + 7) // 8)
            for i in range(len(full)):
                if i - lead not in nulls:
                    bits[i // 8] |= 1 << (i % 8)
            self._validity = bytes(bits)

    def __len__(self):
        return self._len

    def buffers(self):
        return [self._validity, self._offsets, self._data]


_ARROW_CLASSES = [
    lexindex.CompactHashIndex,
    lexindex.PerfectHashIndex,
    lexindex.StringIndex,
    lexindex.ClosedHashIndex,
    lexindex.DictIndex,
]


def _unpack(idx, raw):
    width = 8 if idx.ID_DTYPE == "uint64" else 4
    return [
        int.from_bytes(raw[i * width : (i + 1) * width], sys.byteorder)
        for i in range(len(raw) // width)
    ]


def _expected(idx, probes):
    ids = idx.ids_of(probes)
    if isinstance(idx, lexindex.ClosedHashIndex):
        return ids
    return [idx.MISSING_ID if v is None else v for v in ids]


@pytest.mark.parametrize("cls", _ARROW_CLASSES)
def test_ids_of_arrow_reads_the_column_buffers(cls):
    keys = ["alpha", "bravo", "charlie", "delta", "échelle", ""]
    idx = cls(keys)
    probes = ["delta", "zulu", "", "échelle", "alpha", "alphabet"]
    expected = _expected(idx, probes)
    code = "Q" if idx.ID_DTYPE == "uint64" else "I"
    for col in (
        _Utf8Column(probes),
        _Utf8Column(probes, large=True),
        _Utf8Column(probes, lead=3),
        _Utf8Column(probes, large=True, lead=5),
        # Past a whole validity byte: the buffers are trimmed to the window this column names,
        # and only a lead of eight or more moves where that trimming starts.
        _Utf8Column(probes, lead=11),
        _Utf8Column(probes, large=True, lead=16),
    ):
        assert _unpack(idx, idx.ids_of_arrow(col)) == expected
        out = array.array(code, [7] * (len(probes) + 2))
        assert idx.ids_into_arrow(col, out) is None
        assert list(out) == [*expected, 7, 7]
    # a ChunkedArray, a polars Series, an object with the Arrow protocol, a pandas column
    chunked = types.SimpleNamespace(
        chunks=[_Utf8Column(probes[:2]), _Utf8Column(probes[2:], lead=1)]
    )
    assert _unpack(idx, idx.ids_of_arrow(chunked)) == expected
    polars_like = types.SimpleNamespace(to_arrow=lambda: _Utf8Column(probes))
    assert _unpack(idx, idx.ids_of_arrow(polars_like)) == expected
    protocol = types.SimpleNamespace(__arrow_array__=lambda: _Utf8Column(probes))
    assert _unpack(idx, idx.ids_of_arrow(protocol)) == expected
    pandas_like = types.SimpleNamespace(array=protocol)
    assert _unpack(idx, idx.ids_of_arrow(pandas_like)) == expected
    # empty, and the buffer checks ids_into makes
    assert idx.ids_of_arrow(_Utf8Column([])) == b""
    idx.ids_into_arrow(_Utf8Column([]), bytearray())
    with pytest.raises(BufferError):
        idx.ids_into_arrow(_Utf8Column([]), b"")
    with pytest.raises(ValueError, match="2 items but 6 keys"):
        idx.ids_into_arrow(_Utf8Column(probes), array.array(code, [0, 0]))
    wrong = _Utf8Column(probes)
    wrong.type = "int64"
    with pytest.raises(TypeError, match="int64"):
        idx.ids_of_arrow(wrong)
    with pytest.raises(TypeError, match="pyarrow"):
        idx.ids_of_arrow(["not", "a", "column"])


def test_ids_of_arrow_nulls_are_missing():
    keys = ["a", "b", "c"]
    for cls in _ARROW_CLASSES[:3]:
        idx = cls(keys)
        # `lead=13` puts the first visible key five bits into the second validity byte, which is
        # where the trimmed bitmap and the whole one have to agree.
        for lead in (2, 13):
            col = _Utf8Column(["a", "b", "c", "zz"], nulls={1, 3}, lead=lead)
            assert _unpack(idx, idx.ids_of_arrow(col)) == [
                idx.id("a"),
                idx.MISSING_ID,
                idx.id("c"),
                idx.MISSING_ID,
            ]
    closed = lexindex.ClosedHashIndex(keys)
    with pytest.raises(ValueError, match="null"):
        closed.ids_of_arrow(_Utf8Column(["a", "b"], nulls={0}))


def test_ids_of_arrow_refuses_a_broken_column():
    idx = lexindex.CompactHashIndex(["a"])
    col = _Utf8Column(["a", "bb"])
    col._offsets = array.array("i", [0, 5, 3]).tobytes()
    with pytest.raises(ValueError, match="ascend"):
        idx.ids_of_arrow(col)
    col._offsets = array.array("i", [0, 1]).tobytes()
    with pytest.raises(ValueError, match="shorter"):
        idx.ids_of_arrow(col)
    col = _Utf8Column(["a"])
    col.null_count = 1
    with pytest.raises(ValueError, match="validity"):
        idx.ids_of_arrow(col)


def test_ids_of_arrow_with_pyarrow():
    pa = pytest.importorskip("pyarrow")
    np = _numpy_or_skip()
    keys = ["alpha", "bravo", "charlie", "delta"]
    probes = ["delta", "zulu", "alpha", "", "charlie"]
    for cls in _ARROW_CLASSES:
        idx = cls(keys)
        expected = _expected(idx, probes)
        for col in (
            pa.array(probes),
            pa.array(probes, pa.large_string()),
            pa.chunked_array([probes[:2], probes[2:]]),
            pa.array(["x", "y", *probes])[2:],
        ):
            got = np.frombuffer(idx.ids_of_arrow(col), dtype=idx.ID_DTYPE)
            assert got.tolist() == expected
            out = np.full(len(probes) + 1, 7, dtype=idx.ID_DTYPE)
            idx.ids_into_arrow(col, out)
            assert out.tolist() == [*expected, 7]
        with pytest.raises(TypeError, match="int64"):
            idx.ids_of_arrow(pa.array([1, 2]))
    idx = lexindex.CompactHashIndex(keys)
    with_null = pa.array([probes[0], None, *probes[1:]])
    got = np.frombuffer(idx.ids_of_arrow(with_null), dtype=idx.ID_DTYPE).tolist()
    assert got == [_expected(idx, probes)[0], idx.MISSING_ID, *_expected(idx, probes)[1:]]
