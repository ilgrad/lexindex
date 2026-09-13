"""lexindex: compact, immutable string<->id indexes (FST + minimal perfect hash).

The companion indexing crate to ``betula-cluster``. Build once over a set of strings, then query
many times:

- :class:`StringIndex` — ordered FST: exact ``string <-> id`` plus prefix / range / fuzzy iteration.
- :class:`PerfectHashIndex` — fastest exact ``string -> dense id`` with membership + reverse.
- :class:`CompactHashIndex` — smallest ``string -> dense id`` (perfect hash + fingerprints);
  probabilistic membership, no reverse.
- :class:`ClosedHashIndex` — the perfect hash and nothing else, a fifth of that: ``id`` never
  says absent, for a vocabulary known to be closed.
- :class:`DictIndex` — ordered dictionary with the key stored for every id: exact
  ``string <-> rank`` both ways plus ``lower_bound``, no automata; about 3.5 B/key, a third of
  ``StringIndex``.

All serialise to a flat blob (``save`` / ``load``, or zero-copy ``load_mmap`` — memory-map a huge
index and borrow it instantly).

- :class:`Overlay` — add and remove keys on top of any of them without rebuilding.
- :func:`inspect` — what a blob is, from its header alone, without loading it.
- :func:`plan` — what each of them would cost on a set of keys, before one is built.
"""

from importlib.metadata import PackageNotFoundError, version
from typing import Literal, TypedDict

from lexindex._core import (
    ClosedHashIndex,
    CompactHashIndex,
    DictIndex,
    Overlay,
    PerfectHashIndex,
    StringIndex,
    inspect,
    plan,
)

try:
    __version__ = version("lexindex")
except PackageNotFoundError:  # pragma: no cover - source tree without install metadata
    __version__ = "0.0.0+unknown"


class BlobInfo(TypedDict):
    """What :func:`inspect` reads out of a blob's header; nothing in it is verified."""

    kind: Literal[
        "StringIndex",
        "PerfectHashIndex",
        "CompactHashIndex",
        "ClosedHashIndex",
        "DictIndex",
        "Mphf",
        "Overlay",
    ]
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


class Estimate(TypedDict):
    """What one index would weigh on the keys :func:`plan` was given."""

    kind: Literal[
        "CompactHashIndex",
        "ClosedHashIndex",
        "PerfectHashIndex",
        "StringIndex",
        "DictIndex",
    ]
    bytes: int
    bytes_per_key: float
    block: int | None
    measured: bool


class Plan(TypedDict):
    """What :func:`plan` priced: the ranking, the shape of the corpus and the caveats."""

    keys: int
    mean_length: float
    mean_lcp: float
    best: Estimate
    estimates: list[Estimate]
    close: bool
    thin: bool
    text: str


__all__ = [
    "BlobInfo",
    "ClosedHashIndex",
    "CompactHashIndex",
    "DictIndex",
    "Estimate",
    "Overlay",
    "OverlayInfo",
    "PerfectHashIndex",
    "Plan",
    "StringIndex",
    "__version__",
    "inspect",
    "plan",
]
