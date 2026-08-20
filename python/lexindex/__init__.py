"""lexindex: compact, immutable string<->id indexes (FST + minimal perfect hash).

The companion indexing crate to ``betula-cluster``. Build once over a set of strings, then query
many times:

- :class:`StringIndex` — ordered FST: exact ``string <-> id`` plus prefix / range / fuzzy iteration.
- :class:`PerfectHashIndex` — fastest exact ``string -> dense id`` with membership + reverse.
- :class:`CompactHashIndex` — smallest ``string -> dense id`` (perfect hash + fingerprints);
  probabilistic membership, no reverse.

All serialise to a flat blob (``save`` / ``load``, or zero-copy ``load_mmap`` — memory-map a huge
index and borrow it instantly).
"""

from importlib.metadata import PackageNotFoundError, version

from lexindex._core import CompactHashIndex, PerfectHashIndex, StringIndex

try:
    __version__ = version("lexindex")
except PackageNotFoundError:  # pragma: no cover - source tree without install metadata
    __version__ = "0.0.0+unknown"

__all__ = ["CompactHashIndex", "PerfectHashIndex", "StringIndex", "__version__"]
