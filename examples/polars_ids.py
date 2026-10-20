r"""A string column to dense ids and back, with the lexindex-polars expressions.

The ids are a fraction of the strings, and the dictionary that maps them back is one file that
any process or query can name. Run from a clean environment::

    uv run --no-project --with polars --with lexindex --with lexindex-polars \
        python examples/polars_ids.py
"""

from __future__ import annotations

import random
import tempfile
from pathlib import Path

import lexindex as lx
import lexindex_polars  # noqa: F401  -- the import registers the `.lexindex` namespace

import polars as pl

WORDS = Path("/usr/share/dict/words")
if WORDS.exists():
    vocab = sorted({w for w in WORDS.read_text().splitlines() if w})
else:  # no word list on this system: stand-in keys of the same order of size
    vocab = [f"term-{i:06d}" for i in range(100_000)]
random.seed(7)
events = pl.DataFrame({"word": random.choices(vocab, k=1_000_000)})

with tempfile.TemporaryDirectory() as tmp:
    # Built once, offline. Zero fingerprint bits: `id` is exact, ids are the keys' ranks.
    blob = Path(tmp) / "words.bhd"
    lx.HashedDictIndex.from_dict(lx.DictIndex(vocab), 0).save(blob)
    path = str(blob)

    ids = events.select(pl.col("word").lexindex.id_unchecked(path).cast(pl.UInt32).alias("id"))
    print(events.head(3), ids.head(3), sep="\n")

    back = ids.select(pl.col("id").lexindex.key(path).alias("word"))
    assert back["word"].equals(events["word"]), "every id maps back to its word"

    probe = pl.DataFrame({"word": [vocab[0], "qwxzzy"]})
    print(
        probe.with_columns(
            pl.col("word").lexindex.id(path).alias("id"),
            pl.col("word").lexindex.contains(path).alias("known"),
        )
    )

    words_mb = events.estimated_size() / 1e6
    ids_mb = ids.estimated_size() / 1e6
    print(f"{events.height} rows over {len(vocab)} distinct words")
    print(f"string column  {words_mb:7.2f} MB")
    print(f"id column      {ids_mb:7.2f} MB  ({ids_mb / words_mb:.0%} of the strings)")
    print(f"index file     {blob.stat().st_size / 1e6:7.2f} MB  (the whole dictionary, both ways)")
