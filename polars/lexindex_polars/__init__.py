"""Polars expressions over a `lexindex <https://github.com/ilgrad/lexindex>`_ index.

Importing this module registers the ``lexindex`` expression namespace::

    import polars as pl
    import lexindex_polars  # noqa: F401  -- the import is the registration

    df.with_columns(pl.col("track").lexindex.id("tracks.bdx"))

Every expression names an index blob by path and is elementwise, so it runs in the engine's own
threads, in a lazy plan and in the streaming engine, without holding the GIL. The blob is read once
per process and shared by every chunk; see the plugin's module docs for what happens when the file
is rewritten under a running query.
"""

from __future__ import annotations

from pathlib import Path

import polars as pl
from polars.plugins import register_plugin_function

__all__ = ["LexindexExpr", "__version__"]

__version__ = "0.1.0"

_LIB = Path(__file__).parent


def _path(index: str | Path) -> str:
    """The blob's path as the plugin will stat it: expanded and absolute.

    Resolved once, when the expression is built, so a query keeps reading the same file even if a
    symlink is swapped or the process changes directory while it runs.
    """
    return str(Path(index).expanduser().resolve())


@pl.api.register_expr_namespace("lexindex")
class LexindexExpr:
    """The ``lexindex`` namespace on a Polars expression."""

    def __init__(self, expr: pl.Expr) -> None:
        self._expr = expr

    def id(self, index: str | Path) -> pl.Expr:
        """Map a string column to the index's ids, null where a key is not in it.

        Exact on ``StringIndex``, ``DictIndex``, ``PerfectHashIndex`` and a ``HashedDictIndex``
        built at zero fingerprint bits; on the fingerprint kinds a stranger passes at about
        ``2^-fingerprint_bits``. A ``ClosedHashIndex`` has no membership to check and answers every
        key with an id.
        """
        return register_plugin_function(
            plugin_path=_LIB,
            function_name="lexindex_id",
            args=[self._expr],
            kwargs={"path": _path(index)},
            is_elementwise=True,
        )

    def id_unchecked(self, index: str | Path) -> pl.Expr:
        """Map a string column to ids without checking membership — a closed vocabulary.

        A key the index does not hold gets some id below the key count rather than null, which is
        the price of skipping the compare. Available on the hash-backed kinds only
        (``CompactHashIndex``, ``ClosedHashIndex``, ``PerfectHashIndex``, ``HashedDictIndex``); on
        the two that answer by searching, ``id`` is already exact and this raises.
        """
        return register_plugin_function(
            plugin_path=_LIB,
            function_name="lexindex_id_unchecked",
            args=[self._expr],
            kwargs={"path": _path(index)},
            is_elementwise=True,
        )

    def contains(self, index: str | Path) -> pl.Expr:
        """Whether the index holds each key, as a boolean column.

        Exact on the kinds that store their keys, probabilistic at ``2^-fingerprint_bits`` on
        ``CompactHashIndex`` and a fingerprinted ``HashedDictIndex``. A ``ClosedHashIndex`` cannot
        tell membership at all and raises.
        """
        return register_plugin_function(
            plugin_path=_LIB,
            function_name="lexindex_contains",
            args=[self._expr],
            kwargs={"path": _path(index)},
            is_elementwise=True,
        )

    def key(self, index: str | Path) -> pl.Expr:
        """Map an id column back to its keys, null where the index has no such id.

        The two fingerprint kinds store no keys and raise; the rest answer from the same blob the
        forward direction uses.
        """
        return register_plugin_function(
            plugin_path=_LIB,
            function_name="lexindex_key",
            args=[self._expr],
            kwargs={"path": _path(index)},
            is_elementwise=True,
        )
