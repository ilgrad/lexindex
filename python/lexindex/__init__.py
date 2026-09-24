"""lexindex: compact, immutable string<->id indexes (FST + minimal perfect hash).

The companion indexing crate to ``betula-cluster``. Build once over a set of strings, then query
many times:

- ``StringIndex`` — ordered FST: exact ``string <-> id`` plus prefix / range / fuzzy iteration.
- ``PerfectHashIndex`` — fastest exact ``string -> dense id`` with membership + reverse.
- ``CompactHashIndex`` — smallest ``string -> dense id`` that can reject a non-member
  (perfect hash + fingerprints);
  probabilistic membership, no reverse.
- ``ClosedHashIndex`` — the perfect hash and nothing else, a fifth of that: ``id`` never
  says absent, for a vocabulary known to be closed.
- ``DictIndex`` — ordered dictionary with the key stored for every id: exact
  ``string <-> rank`` both ways plus ``lower_bound``, prefix and range, no automata; 2.64 B/key on
  real words, 56 % below ``StringIndex``.
- ``HashedDictIndex`` — a ``DictIndex`` with a hash sidecar: ``id`` at a hash index's
  cost, with the dictionary's ranks, and its ordered queries through ``.dict``.

All serialise to a flat blob (``save`` / ``load``). Every one but ``ClosedHashIndex`` also
has ``load_mmap``, which maps a huge index and borrows it instantly; ``DictIndex`` builds its
per-block samples and a few tables at load and borrows the rest.

- ``Overlay`` — add and remove keys on top of ``StringIndex``,
  ``PerfectHashIndex`` or ``CompactHashIndex`` without rebuilding.
- ``inspect`` — what a blob is, from its header alone, without loading it.
- ``plan`` — what each of them would cost on a set of keys, before one is built.
"""

from importlib.metadata import PackageNotFoundError, version
from typing import Literal, TypedDict

from lexindex._core import (
    ClosedHashIndex,
    CompactHashIndex,
    DictIndex,
    HashedDictIndex,
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
    """What ``inspect`` reads out of a blob's header; nothing in it is verified."""

    kind: Literal[
        "StringIndex",
        "PerfectHashIndex",
        "CompactHashIndex",
        "ClosedHashIndex",
        "DictIndex",
        "Mphf",
        "Overlay",
        "HashedDictIndex",
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
    """What one index would weigh on the keys ``plan`` was given."""

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
    """What ``plan`` priced: the ranking, the shape of the corpus and the caveats."""

    keys: int
    mean_length: float
    mean_lcp: float
    best: Estimate
    estimates: list[Estimate]
    close: bool
    thin: bool
    text: str


class Workload(TypedDict, total=False):
    """How often each operation is asked of the index, for ``plan`` to rank by."""

    hits: int
    misses: int
    reverse: int
    prefix: int
    common_prefix: int
    longest_prefix: int
    batch: int


__all__ = [
    "BlobInfo",
    "ClosedHashIndex",
    "CompactHashIndex",
    "DictIndex",
    "Estimate",
    "HashedDictIndex",
    "Overlay",
    "OverlayInfo",
    "PerfectHashIndex",
    "Plan",
    "StringIndex",
    "Workload",
    "__version__",
    "inspect",
    "plan",
]
