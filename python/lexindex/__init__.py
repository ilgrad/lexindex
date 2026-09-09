"""lexindex: compact, immutable string<->id indexes (FST + minimal perfect hash).

The companion indexing crate to ``betula-cluster``. Build once over a set of strings, then query
many times:

- :class:`StringIndex` — ordered FST: exact ``string <-> id`` plus prefix / range / fuzzy iteration.
- :class:`PerfectHashIndex` — fastest exact ``string -> dense id`` with membership + reverse.
- :class:`CompactHashIndex` — smallest ``string -> dense id`` (perfect hash + fingerprints);
  probabilistic membership, no reverse.

All serialise to a flat blob (``save`` / ``load``, or zero-copy ``load_mmap`` — memory-map a huge
index and borrow it instantly).

- :class:`Overlay` — add and remove keys on top of any of them without rebuilding.
- :func:`inspect` — what a blob is, from its header alone, without loading it.
"""

from importlib.metadata import PackageNotFoundError, version
from typing import Literal, TypedDict

from lexindex._core import CompactHashIndex, Overlay, PerfectHashIndex, StringIndex, inspect

try:
    __version__ = version("lexindex")
except PackageNotFoundError:  # pragma: no cover - source tree without install metadata
    __version__ = "0.0.0+unknown"


class BlobInfo(TypedDict):
    """What :func:`inspect` reads out of a blob's header; nothing in it is verified."""

    kind: Literal["StringIndex", "PerfectHashIndex", "CompactHashIndex", "Mphf", "Overlay"]
    format: str
    bytes: int
    keys: int | None
    fingerprint_bits: int | None
    mph_bytes: int | None
    arena_bytes: int | None
    side_entries: int | None
    overlay: "OverlayInfo | None"


class OverlayInfo(TypedDict):
    """An overlay's own sections, and its base inspected in turn."""

    base_tag: int
    base: BlobInfo | None
    additions: int
    retired: int


__all__ = [
    "BlobInfo",
    "CompactHashIndex",
    "Overlay",
    "OverlayInfo",
    "PerfectHashIndex",
    "StringIndex",
    "__version__",
    "inspect",
]
