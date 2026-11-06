"""The plugin's four expressions, against the same answers the lexindex package gives.

Every test builds a real blob and compares the column the engine produces with the per-key answer
from `lexindex` itself, so a disagreement is the plugin's and not a hand-written expectation.
"""

from __future__ import annotations

import os
from pathlib import Path
from types import SimpleNamespace

import lexindex as lx
import polars as pl
import pytest

import lexindex_polars  # the import registers the namespace

KEYS = ["alpha", "beta", "delta", "epsilon", "gamma", "omega", "zeta"]
STRANGERS = ["absent", "nothing", "zzz"]
SPECIMENS = Path(__file__).resolve().parents[2] / "tests" / "data"


def _save(index, tmp_path, name):
    path = tmp_path / name
    index.save(path)
    return path, index


@pytest.fixture
def built(tmp_path):
    """One blob of every kind over the same keys, with the index that wrote it."""
    dictionary = lx.DictIndex(KEYS)
    return {
        "string": _save(lx.StringIndex(KEYS), tmp_path, "keys.bst"),
        "dict": _save(dictionary, tmp_path, "keys.bdx"),
        "hashed": _save(lx.HashedDictIndex.from_dict(dictionary, 16), tmp_path, "keys.bhd"),
        "compact": _save(lx.CompactHashIndex(KEYS, fingerprint_bits=16), tmp_path, "keys.bch"),
        "closed": _save(lx.ClosedHashIndex(KEYS), tmp_path, "keys.bcl"),
        "perfect": _save(lx.PerfectHashIndex(KEYS), tmp_path, "keys.bmp"),
        "double_array": _save(lx.DoubleArrayIndex(KEYS), tmp_path, "keys.bda"),
    }


@pytest.fixture
def blobs(built):
    """Just the paths, which is all most of the tests need."""
    return {kind: path for kind, (path, _) in built.items()}


def _ids(path, expr, keys=KEYS):
    return pl.DataFrame({"k": keys}).select(expr(pl.col("k"), path))["k"].to_list()


KEY_STORING = ["string", "dict", "hashed", "perfect"]
# compact's fingerprint is 16 bits here, so no stranger passes
EXACT = [*KEY_STORING, "double_array", "compact"]


@pytest.mark.parametrize("kind", EXACT)
def test_id_matches_the_library_on_members(built, kind):
    path, reference = built[kind]
    assert _ids(path, lambda c, p: c.lexindex.id(p)) == [reference.id(k) for k in KEYS]


@pytest.mark.parametrize("kind", EXACT)
def test_id_is_null_for_a_stranger(blobs, kind):
    assert _ids(blobs[kind], lambda c, p: c.lexindex.id(p), STRANGERS) == [None] * len(STRANGERS)


def test_a_closed_hash_answers_every_key(blobs):
    ids = _ids(blobs["closed"], lambda c, p: c.lexindex.id(p), [*KEYS, *STRANGERS])
    assert all(i is not None and 0 <= i < len(KEYS) for i in ids)


@pytest.mark.parametrize("kind", ["compact", "closed", "perfect", "hashed"])
def test_id_unchecked_agrees_with_id_on_members(blobs, kind):
    path = blobs[kind]
    assert _ids(path, lambda c, p: c.lexindex.id_unchecked(p)) == _ids(
        path, lambda c, p: c.lexindex.id(p)
    )


@pytest.mark.parametrize("kind", ["string", "dict", "double_array"])
def test_id_unchecked_refuses_a_searching_index(blobs, kind):
    with pytest.raises(pl.exceptions.ComputeError, match="no unchecked lookup"):
        _ids(blobs[kind], lambda c, p: c.lexindex.id_unchecked(p))


@pytest.mark.parametrize("kind", EXACT)
def test_contains_is_exact_here(blobs, kind):
    path = blobs[kind]
    probes = [*KEYS, *STRANGERS]
    expected = [k in KEYS for k in probes]
    assert _ids(path, lambda c, p: c.lexindex.contains(p), probes) == expected


def test_contains_refuses_a_closed_hash(blobs):
    with pytest.raises(pl.exceptions.ComputeError, match="cannot tell membership"):
        _ids(blobs["closed"], lambda c, p: c.lexindex.contains(p))


@pytest.mark.parametrize("kind", KEY_STORING)
def test_key_is_the_inverse_of_id(blobs, kind):
    path = blobs[kind]
    out = (
        pl.DataFrame({"k": KEYS})
        .with_columns(pl.col("k").lexindex.id(path).alias("id"))
        .with_columns(pl.col("id").lexindex.key(path).alias("back"))
    )
    assert out["back"].to_list() == KEYS


@pytest.mark.parametrize("kind", KEY_STORING)
def test_key_is_null_past_the_end(blobs, kind):
    out = pl.DataFrame({"id": [10_000]}).select(pl.col("id").lexindex.key(blobs[kind]))
    assert out["id"].to_list() == [None]


@pytest.mark.parametrize("kind", ["compact", "closed", "double_array"])
def test_key_refuses_an_index_without_keys(blobs, kind):
    with pytest.raises(pl.exceptions.ComputeError, match="no reverse lookup"):
        pl.DataFrame({"id": [0]}).select(pl.col("id").lexindex.key(blobs[kind]))


def test_null_in_null_out(blobs):
    frame = pl.DataFrame({"k": ["alpha", None, "beta"]})
    out = frame.select(
        pl.col("k").lexindex.id(blobs["dict"]).alias("id"),
        pl.col("k").lexindex.contains(blobs["dict"]).alias("has"),
    )
    assert out["id"].to_list() == [0, None, 1]
    assert out["has"].to_list() == [True, None, True]


def test_it_runs_in_a_lazy_plan(blobs):
    out = (
        pl.LazyFrame({"k": KEYS})
        .with_columns(pl.col("k").lexindex.id(blobs["hashed"]).alias("id"))
        .filter(pl.col("id") < 3)
        .collect()
    )
    assert out["k"].to_list() == KEYS[:3]


def test_the_id_column_is_unsigned_64_bit(blobs):
    out = pl.DataFrame({"k": KEYS}).select(pl.col("k").lexindex.id(blobs["dict"]))
    assert out.schema["k"] == pl.UInt64


def test_a_rebuilt_blob_at_the_same_path_is_picked_up(blobs, tmp_path):
    path = blobs["dict"]
    assert _ids(path, lambda c, p: c.lexindex.id(p), ["alpha"]) == [0]
    lx.DictIndex(["aardvark", *KEYS]).save(path)
    assert _ids(path, lambda c, p: c.lexindex.id(p), ["alpha"]) == [1]


@pytest.mark.skipif(os.name != "posix", reason="only unix has a stable file identity in std")
@pytest.mark.parametrize("pin", [True, False])
def test_a_blob_renamed_over_the_path_is_seen_at_the_same_length_and_timestamp(tmp_path, pin):
    path, replacement = tmp_path / "live.bdx", tmp_path / "next.bdx"
    lx.DictIndex(["aa", "bb"]).save(path)
    lx.DictIndex(["bb", "cc"]).save(replacement)
    assert _ids(path, lambda c, p: c.lexindex.id(p, pin=pin), ["bb"]) == [1]

    was = path.stat()
    os.utime(replacement, ns=(was.st_atime_ns, was.st_mtime_ns))
    os.replace(replacement, path)
    now = path.stat()
    assert (now.st_size, now.st_mtime_ns) == (was.st_size, was.st_mtime_ns)
    assert _ids(path, lambda c, p: c.lexindex.id(p, pin=pin), ["bb"]) == [0]


@pytest.fixture
def live(tmp_path):
    """A blob in which "alpha" is id 0, and what rebuilds it so that "alpha" is id 1."""
    path = tmp_path / "live.bdx"
    lx.DictIndex(KEYS).save(path)
    return path, lambda: lx.DictIndex(["aardvark", *KEYS]).save(path)


ALPHA = pl.DataFrame({"k": ["alpha"]})


def test_one_query_never_mixes_two_generations(live):
    path, rebuild = live
    expr = pl.col("k").lexindex.id(path)

    def rebuilt_under_the_query(frame):
        rebuild()
        return frame

    out = (
        ALPHA.lazy()
        .with_columns(expr.alias("before"))
        .map_batches(rebuilt_under_the_query)
        .with_columns(expr.alias("after"))
        .collect()
    )
    assert out.row(0) == ("alpha", 0, 0)


def test_a_pinned_hit_touches_no_file(live):
    path, _ = live
    expr = pl.col("k").lexindex.id(path)
    assert ALPHA.select(expr).item() == 0
    path.unlink()
    assert ALPHA.select(expr).item() == 0


def test_an_expression_refuses_a_generation_it_can_no_longer_get(live):
    path, rebuild = live
    expr = pl.col("k").lexindex.id(path)
    rebuild()
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since this expression was built"):
        ALPHA.select(expr)


def test_a_new_expression_takes_the_new_generation_and_the_old_one_then_refuses(live):
    path, rebuild = live
    old = pl.col("k").lexindex.id(path)
    assert ALPHA.select(old).item() == 0
    rebuild()
    assert ALPHA.select(pl.col("k").lexindex.id(path)).item() == 1
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since this expression was built"):
        ALPHA.select(old)


def test_a_pinned_expression_reads_its_generation_again_after_an_eviction(live):
    path, _ = live
    expr = pl.col("k").lexindex.id(path)
    assert ALPHA.select(expr).item() == 0
    lexindex_polars.clear_cache()
    loads = lexindex_polars.cache_info()["loads"]
    assert ALPHA.select(expr).item() == 0
    assert lexindex_polars.cache_info()["loads"] == loads + 1


def test_a_pin_past_the_signed_range_reaches_the_plugin(live, monkeypatch):
    # overlayfs keeps a layer's number in an inode's top bits. Pickled as it is, such a number stops
    # the plugin's decoder ("could not parse kwargs"); here it must arrive and simply not match.
    path, _ = live
    real = path.stat()
    stat = SimpleNamespace(
        st_size=real.st_size, st_mtime_ns=real.st_mtime_ns, st_dev=2**64 - 1, st_ino=2**63 + 5
    )
    monkeypatch.setattr(lexindex_polars, "os", SimpleNamespace(stat=lambda _: stat, name="posix"))
    expr = pl.col("k").lexindex.id(path)
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since"):
        ALPHA.select(expr)


def test_pin_false_reads_what_the_file_holds_when_a_chunk_runs(live):
    path, rebuild = live
    expr = pl.col("k").lexindex.id(path, pin=False)
    assert ALPHA.select(expr).item() == 0
    rebuild()
    assert ALPHA.select(expr).item() == 1


def test_a_file_absent_when_the_expression_is_built_is_read_when_it_appears(tmp_path):
    path = tmp_path / "later.bdx"
    expr = pl.col("k").lexindex.id(path)
    lx.DictIndex(KEYS).save(path)
    assert ALPHA.select(expr).item() == 0


@pytest.mark.parametrize("name", ["id", "contains"])
def test_the_forward_expressions_all_take_pin(live, name):
    path, rebuild = live
    expr = getattr(pl.col("k").lexindex, name)(path)
    rebuild()
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since"):
        ALPHA.select(expr)
    assert ALPHA.select(getattr(pl.col("k").lexindex, name)(path, pin=False)).height == 1


def test_id_unchecked_takes_pin(tmp_path):
    path = tmp_path / "live.bhd"
    lx.HashedDictIndex.from_dict(lx.DictIndex(KEYS), 0).save(path)
    expr = pl.col("k").lexindex.id_unchecked(path)
    lx.HashedDictIndex.from_dict(lx.DictIndex(["aardvark", *KEYS]), 0).save(path)
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since"):
        ALPHA.select(expr)
    assert ALPHA.select(pl.col("k").lexindex.id_unchecked(path, pin=False)).item() == 1


def test_key_takes_pin(live):
    path, rebuild = live
    expr = pl.col("id").lexindex.key(path)
    rebuild()
    zero = pl.DataFrame({"id": [0]})
    with pytest.raises(pl.exceptions.ComputeError, match="rebuilt since"):
        zero.select(expr)
    assert zero.select(pl.col("id").lexindex.key(path, pin=False)).item() == "aardvark"


def _read(path):
    _ids(path, lambda c, p: c.lexindex.id(p), ["alpha"])


@pytest.fixture
def three(tmp_path):
    """Three blobs of one size, in a cache that starts empty."""
    paths = [tmp_path / f"{name}.bdx" for name in "abc"]
    for path in paths:
        lx.DictIndex(KEYS).save(path)
    lexindex_polars.clear_cache()
    return paths


def test_a_first_query_on_many_threads_reads_the_blob_once(tmp_path):
    # Far more expressions than threads, over a blob too large to read before the next one starts:
    # every thread misses at once. Each expression's own column keeps them from being merged.
    path = tmp_path / "large.bdx"
    lx.DictIndex(f"key-{i:07d}" for i in range(400_000)).save(path)
    columns = [f"k{i}" for i in range(4 * pl.thread_pool_size())]
    frame = pl.DataFrame({c: ["key-0000001"] for c in columns})
    before = lexindex_polars.cache_info()["loads"]
    out = frame.select(pl.col(c).lexindex.id(path) for c in columns)
    assert out.row(0) == (1,) * len(columns)
    assert lexindex_polars.cache_info()["loads"] == before + 1


def test_cache_info_counts_what_the_cache_did(three):
    a = three[0]
    was = lexindex_polars.cache_info()
    assert (was["entries"], was["bytes"]) == (0, 0)
    _read(a)
    _read(a)
    now = lexindex_polars.cache_info()
    assert (now["entries"], now["bytes"]) == (1, a.stat().st_size)
    assert now["loads"] == was["loads"] + 1
    assert now["hits"] == was["hits"] + 1


def test_clear_cache_forgets_the_blobs_and_keeps_the_counters(three):
    a = three[0]
    _read(a)
    loads = lexindex_polars.cache_info()["loads"]
    lexindex_polars.clear_cache()
    now = lexindex_polars.cache_info()
    assert (now["entries"], now["bytes"], now["loads"]) == (0, 0, loads)
    _read(a)
    assert lexindex_polars.cache_info()["loads"] == loads + 1


def test_without_a_budget_every_path_stays(three, monkeypatch):
    monkeypatch.delenv("LEXINDEX_POLARS_CACHE_BYTES", raising=False)
    for path in three:
        _read(path)
    assert lexindex_polars.cache_info()["entries"] == 3


def test_the_budget_lets_the_least_recently_used_go(three, monkeypatch):
    a, b, c = three
    monkeypatch.setenv("LEXINDEX_POLARS_CACHE_BYTES", str(2 * a.stat().st_size))
    _read(a)
    _read(b)
    _read(a)  # now b is the older of the two
    _read(c)
    now = lexindex_polars.cache_info()
    assert (now["entries"], now["bytes"]) == (2, 2 * a.stat().st_size)
    _read(a)
    _read(c)
    assert lexindex_polars.cache_info()["loads"] == now["loads"]
    _read(b)
    assert lexindex_polars.cache_info()["loads"] == now["loads"] + 1


def test_a_blob_over_the_budget_stays_until_another_takes_its_place(three, monkeypatch):
    a, b, _ = three
    monkeypatch.setenv("LEXINDEX_POLARS_CACHE_BYTES", "1")
    _read(a)
    now = lexindex_polars.cache_info()
    assert (now["entries"], now["bytes"]) == (1, a.stat().st_size)
    _read(a)
    assert lexindex_polars.cache_info()["loads"] == now["loads"]
    _read(b)
    assert lexindex_polars.cache_info()["entries"] == 1
    _read(a)
    assert lexindex_polars.cache_info()["loads"] == now["loads"] + 2


def test_a_budget_that_is_not_a_number_is_refused_by_name(three, monkeypatch):
    monkeypatch.setenv("LEXINDEX_POLARS_CACHE_BYTES", "lots")
    with pytest.raises(pl.exceptions.ComputeError, match="LEXINDEX_POLARS_CACHE_BYTES"):
        _read(three[0])


def test_a_missing_file_names_itself(tmp_path):
    with pytest.raises(pl.exceptions.ComputeError, match="cannot open"):
        _ids(tmp_path / "nope.bdx", lambda c, p: c.lexindex.id(p))


def test_a_file_that_is_not_an_index_is_refused(tmp_path):
    path = tmp_path / "junk.bdx"
    path.write_bytes(b"not an index at all")
    with pytest.raises(pl.exceptions.ComputeError, match="not a readable index"):
        _ids(path, lambda c, p: c.lexindex.id(p))


@pytest.mark.parametrize("name", ["panicking-1.0.0-string.bix", "unfitting-4.0.0-string.bix"])
def test_a_crafted_string_index_is_refused_by_name(name):
    # `from_bytes` panics on the first of these inside fst's decoder. The engine catches a plugin's
    # panic too, but then all it can say is "the plugin panicked", about no file in particular.
    with pytest.raises(pl.exceptions.ComputeError, match="not a readable index") as refused:
        _ids(SPECIMENS / name, lambda c, p: c.lexindex.id(p))
    assert name in str(refused.value)
    assert "panicked" not in str(refused.value)


@pytest.mark.parametrize(
    "name", ["golden-5.0.0-perfect-merged.bmp", "golden-5.0.0-perfect-merged-fp.bmp"]
)
def test_a_merged_arena_answers_every_key(name):
    path = SPECIMENS / name
    reference = lx.PerfectHashIndex.load(path)
    keys = [reference.key(i) for i in range(len(reference))]
    assert _ids(path, lambda c, p: c.lexindex.id(p), keys) == list(range(len(keys)))


def test_a_labelled_dictionary_answers_both_ways(tmp_path):
    # Names that recur under different parents, the shape labels pay on: on any other the build
    # keeps the plain blob, and this would test nothing.
    dirs = ["usr/share", "usr/lib", "home/me/src", "opt/tool", "var/log", "etc"]
    names = [
        "locale/translations",
        "python3.14/site-packages",
        "icons/hicolor/48x48",
        "documentation",
        "node_modules/.bin",
        "x",
        "release-notes",
        "include/subdir",
    ]
    files = ["index.html", "README.md", "__init__.py", "Makefile", "a"]
    state = 3

    def pick(items):
        nonlocal state
        state ^= (state << 13) & 0xFFFFFFFFFFFFFFFF
        state ^= state >> 7
        state ^= (state << 17) & 0xFFFFFFFFFFFFFFFF
        return items[state % len(items)]

    keys = sorted(
        {f"/{pick(dirs)}/{pick(names)}/{pick(names)}-{i % 97}/{pick(files)}" for i in range(4_000)}
    )
    path = tmp_path / "labelled.bdx"
    lx.DictIndex(keys, block=256, labels=True).save(path)
    assert path.read_bytes()[:4] == b"BDX5"
    assert _ids(path, lambda c, p: c.lexindex.id(p), keys) == list(range(len(keys)))
    frame = pl.DataFrame({"i": list(range(len(keys)))}, schema={"i": pl.UInt64})
    assert frame.select(pl.col("i").lexindex.key(path))["i"].to_list() == keys


def test_a_non_string_column_names_its_type(blobs):
    # Every error a plugin raises reaches Python as a ComputeError, whatever it was in Rust; the
    # message is what carries the kind.
    with pytest.raises(pl.exceptions.ComputeError, match="expected a string column, got i64"):
        pl.DataFrame({"k": [1, 2]}).select(pl.col("k").lexindex.id(blobs["dict"]))
