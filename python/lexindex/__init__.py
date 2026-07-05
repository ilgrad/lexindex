"""lexindex: compact, immutable string<->id indexes (FST + minimal perfect hash).

The companion indexing crate to ``betula-cluster``. Build once over a set of strings, then query
many times: exact ``string <-> id``, plus prefix / range / fuzzy iteration (:class:`StringIndex`),
and a fastest exact-lookup dictionary (:class:`PerfectHashIndex`). Both serialise to a flat blob
(``save`` / ``load``, or zero-copy ``load_mmap`` — memory-map a huge index and borrow it instantly).
"""

from lexindex._core import PerfectHashIndex, StringIndex

__all__ = ["PerfectHashIndex", "StringIndex"]
