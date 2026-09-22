"""The plugin's four expressions, against the same answers the lexindex package gives.

Every test builds a real blob and compares the column the engine produces with the per-key answer
from `lexindex` itself, so a disagreement is the plugin's and not a hand-written expectation.
"""

from __future__ import annotations

import lexindex as lx
import polars as pl
import pytest

import lexindex_polars  # noqa: F401  -- the import registers the namespace

KEYS = ["alpha", "beta", "delta", "epsilon", "gamma", "omega", "zeta"]
STRANGERS = ["absent", "nothing", "zzz"]


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
        "compact": _save(lx.CompactHashIndex(KEYS, 2), tmp_path, "keys.bch"),
        "closed": _save(lx.ClosedHashIndex(KEYS), tmp_path, "keys.bcl"),
        "perfect": _save(lx.PerfectHashIndex(KEYS), tmp_path, "keys.bmp"),
    }


@pytest.fixture
def blobs(built):
    """Just the paths, which is all most of the tests need."""
    return {kind: path for kind, (path, _) in built.items()}


def _ids(path, expr, keys=KEYS):
    return pl.DataFrame({"k": keys}).select(expr(pl.col("k"), path))["k"].to_list()


KEY_STORING = ["string", "dict", "hashed", "perfect"]
EXACT = [*KEY_STORING, "compact"]  # compact's fingerprint is 16 bits here, so no stranger passes


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


@pytest.mark.parametrize("kind", ["string", "dict"])
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


@pytest.mark.parametrize("kind", ["compact", "closed"])
def test_key_refuses_an_index_without_keys(blobs, kind):
    with pytest.raises(pl.exceptions.ComputeError, match="stores no keys"):
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


def test_a_missing_file_names_itself(tmp_path):
    with pytest.raises(pl.exceptions.ComputeError, match="cannot open"):
        _ids(tmp_path / "nope.bdx", lambda c, p: c.lexindex.id(p))


def test_a_file_that_is_not_an_index_is_refused(tmp_path):
    path = tmp_path / "junk.bdx"
    path.write_bytes(b"not an index at all")
    with pytest.raises(pl.exceptions.ComputeError, match="not a readable index"):
        _ids(path, lambda c, p: c.lexindex.id(p))


def test_a_non_string_column_names_its_type(blobs):
    # Every error a plugin raises reaches Python as a ComputeError, whatever it was in Rust; the
    # message is what carries the kind.
    with pytest.raises(pl.exceptions.ComputeError, match="expected a string column, got i64"):
        pl.DataFrame({"k": [1, 2]}).select(pl.col("k").lexindex.id(blobs["dict"]))
