"""End-to-end tests of the lexindex Python bindings."""

import itertools
import random
import time

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
    [lexindex.StringIndex, lexindex.PerfectHashIndex, lexindex.CompactHashIndex],
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
