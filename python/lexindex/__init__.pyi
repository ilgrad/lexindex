"""Type stubs for lexindex."""

import os
from collections.abc import Callable, Iterable, Iterator, Sequence
from typing import ClassVar, Literal, TypedDict, TypeVar, final

from typing_extensions import Buffer

__all__ = [
    "BlobInfo",
    "ClosedHashIndex",
    "CompactHashIndex",
    "DictIndex",
    "Overlay",
    "OverlayInfo",
    "PerfectHashIndex",
    "StringIndex",
    "__version__",
    "inspect",
]

_T = TypeVar("_T")

__version__: str

@final
class StringIndex:
    """Ordered string<->id index (FST) with prefix / range / fuzzy / subsequence queries."""

    def __new__(cls, items: Iterable[str]) -> StringIndex: ...
    @staticmethod
    def from_sorted(items: Iterable[str]) -> StringIndex:
        """Build from keys already in ascending byte order, without materialising them.

        The constructor has to hold the whole corpus to sort it; this consumes the iterable lazily,
        so a generator over a large corpus never exists as a list. Adjacent duplicates are dropped
        exactly as the constructor drops them after sorting, and input that is not ascending raises
        `ValueError` rather than producing an index that answers wrongly.

        Keys composed from sorted parts are not themselves sorted unless the separator is below
        every byte that can follow a part: joining a sorted word list to itself with "." puts
        "'tween-decks.&c" before "'tween.ARU", because "-" is below ".".
        """

    @staticmethod
    def build_sorted_to_file(items: Iterable[str], path: str | os.PathLike[str]) -> int:
        """`from_sorted` streamed straight to `path`; returns the number of keys written.

        Neither the corpus nor the finished index has to fit in memory. If the iterable raises, the
        build is abandoned with `path` untouched.
        """

    @staticmethod
    def build_to_file(items: Iterable[str], path: str | os.PathLike[str]) -> int:
        """The constructor for a corpus that does not fit in memory, written straight to `path`.

        The keys are taken in one pass, in any order: every 256 MiB of them is sorted in memory and
        spilled as a run to a temporary directory beside `path`, and the runs are merged into the
        transducer, so neither the corpus nor the index is ever held whole. The file is exactly what
        the constructor followed by ``save`` would have written. Returns the number of distinct
        keys. If the iterable raises, the build is abandoned with `path` untouched.
        """

    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int | None: ...
    def contains(self, key: str) -> bool: ...
    def __getitem__(self, key: str, /) -> int:
        """Dense id of ``key``, raising ``KeyError`` if it is absent — the dict spelling of
        :meth:`id`. There is no ``__setitem__`` and no ``keys`` / ``values`` / ``items``: this is an
        immutable ``str -> int`` lookup, not a mapping."""

    def get(self, key: str, default: _T | None = None) -> int | _T | None:
        """Dense id of ``key``, or ``default`` (``None`` unless given)."""

    def key(self, id: int) -> str | None: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 8-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

    def ids_into(self, keys: Sequence[str], out: Buffer) -> None:
        """:meth:`ids_of_bytes` written into memory the caller owns instead of a fresh ``bytes``.

        ``out`` is any writable C-contiguous buffer of :attr:`ID_DTYPE` items --
        ``np.empty(len(keys), dtype=index.ID_DTYPE)`` is the usual one -- so a hot loop can reuse
        one array. The first ``len(keys)`` items are written; the rest are left as they were. A
        read-only, strided or mistyped buffer raises ``BufferError``; one shorter than ``keys``
        raises ``ValueError``. With no keys nothing is written and ``out`` need only be a
        writable buffer.
        """

    def ids_of_arrow(self, column: object) -> bytes:
        """:meth:`ids_of_bytes` over an Arrow ``utf8``/``large_utf8`` column — a pyarrow ``Array``
        or ``ChunkedArray``, a pandas ``ArrowDtype`` column, a polars ``Series`` — read straight
        from its offset and data buffers, so no Python string exists per key. A null comes
        back as :attr:`MISSING_ID`."""

    def ids_into_arrow(self, column: object, out: Buffer) -> None:
        """:meth:`ids_of_arrow` written into ``out``, as :meth:`ids_into` does for a list."""

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint64 here); the width differs between
    the index types, so read it from the class rather than hardcoding one."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def keys_of(self, ids: Sequence[int]) -> list[str | None]: ...
    def prefix(self, prefix: str, limit: int | None = None) -> list[tuple[str, int]]: ...
    def range(self, lo: str, hi: str, limit: int | None = None) -> list[tuple[str, int]]: ...
    def lower_bound(self, query: str) -> int: ...
    def range_count(self, lo: str, hi: str) -> int: ...
    def prefix_count(self, prefix: str) -> int: ...
    def prefix_id_range(self, prefix: str) -> tuple[int, int]: ...
    def successor(self, query: str) -> tuple[str, int] | None: ...
    def predecessor(self, query: str) -> tuple[str, int] | None: ...
    def fuzzy(
        self, query: str, max_distance: int, limit: int | None = None
    ) -> list[tuple[str, int]]: ...
    def subsequence(self, query: str, limit: int | None = None) -> list[tuple[str, int]]: ...
    def __iter__(self) -> Iterator[tuple[str, int]]: ...
    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> StringIndex: ...
    @staticmethod
    def from_untrusted_bytes(data: bytes) -> StringIndex:
        """Read a blob **someone else wrote**, validating it before any query reaches it.

        ``from_bytes`` documents the one exception to "arbitrary bytes raise ``ValueError``": the
        blob is an ``fst`` transducer whose node decoder is safe but not *total*, so bytes crafted
        to carry a matching checksum raise ``pyo3_runtime.PanicException`` instead. This loader
        checks the transducer as a graph -- values are ranks, keys are UTF-8, in time proportional
        to nodes rather than to keys -- and catches the panic, so a crafted blob raises
        ``ValueError`` like any other bad input.

        32x the cost of ``from_bytes`` (22.9 ms against 0.7 on 479 823 words): worth paying once for
        a stranger's blob, not for one of your own. The contained panic still prints through the
        process-wide hook before the ``ValueError`` is raised.
        """

    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> StringIndex: ...
    @staticmethod
    def load_untrusted(path: str | os.PathLike[str]) -> StringIndex:
        """``load`` for a file **someone else wrote**: ``from_untrusted_bytes`` over its bytes."""
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> StringIndex:
        """Memory-map the file and borrow the index from it — no read into RAM.

        The file must not be modified or truncated by any process while the index is alive: the
        bytes are borrowed, not copied, so a concurrent write is undefined behaviour rather than a
        stale answer (the Rust loader is ``unsafe fn``). Use ``load`` if the file may change.
        The payload checksum is skipped by design; ``load_mmap_verified`` adds it back.
        """
    @staticmethod
    def load_mmap_verified(path: str | os.PathLike[str]) -> StringIndex:
        """``load_mmap`` plus the checksum ``load`` makes: one pass over the mapping at load,
        pages still shared, nothing copied. Same obligation as ``load_mmap``.
        """
    @staticmethod
    def load_mmap_untrusted(path: str | os.PathLike[str]) -> StringIndex:
        """``load_mmap`` for a file someone else wrote and too large to copy: the validation of
        ``from_untrusted_bytes`` over the mapping. Same obligation as ``load_mmap``, weighing more
        here -- the validation trusts what it saw once -- so map a copy you own.
        """

@final
class PerfectHashIndex:
    """Minimal-perfect-hash dictionary: exact string->dense id, with reverse lookup.

    ``fingerprints=True`` stores one more byte per key, a fingerprint from a second hash kept next
    to the key's offset, so a lookup of an **absent** key stops after one cache miss instead of two
    (the key itself is never read, 255 times in 256). Members cost the same and get the same ids.
    For a workload that is mostly misses -- a stop list, a block list, a "seen before" check.
    """

    def __new__(cls, items: Iterable[str], *, fingerprints: bool = False) -> PerfectHashIndex: ...
    @staticmethod
    def build_to_file(
        source: Callable[[], Iterable[str]],
        path: str | os.PathLike[str],
        *,
        fingerprints: bool = False,
    ) -> int:
        """Build straight to ``path`` without ever holding the keys; returns the number written.

        ``source`` is a zero-argument callable returning an iterable of ``str`` and is **called
        twice** -- the keys are hashed first and can only be placed once the perfect hash exists --
        so pass ``lambda: open(path)``-style factories, never the iterable itself (``TypeError``).
        Keys must be distinct; a repeated key, or a second pass yielding different keys, raises
        ``ValueError`` with ``path`` untouched. An exception raised by the iterable propagates as
        itself, also with ``path`` untouched.

        An output whose key arena exceeds 32 MB is written through a spill file alongside it, so
        roughly 2.2x the output size must be free in the target directory until the build finishes.
        """
    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def has_fingerprints(self) -> bool:
        """Whether the index was built with ``fingerprints=True``."""
    def id(self, key: str) -> int | None: ...
    def id_unchecked(self, key: str) -> int: ...
    def contains(self, key: str) -> bool: ...
    def __getitem__(self, key: str, /) -> int:
        """Dense id of ``key``, raising ``KeyError`` if it is absent — the dict spelling of
        :meth:`id`. There is no ``__setitem__`` and no ``keys`` / ``values`` / ``items``: this is an
        immutable ``str -> int`` lookup, not a mapping."""

    def get(self, key: str, default: _T | None = None) -> int | _T | None:
        """Dense id of ``key``, or ``default`` (``None`` unless given)."""

    def key(self, id: int) -> str | None: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 4-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

    def ids_into(self, keys: Sequence[str], out: Buffer) -> None:
        """:meth:`ids_of_bytes` written into memory the caller owns instead of a fresh ``bytes``.

        ``out`` is any writable C-contiguous buffer of :attr:`ID_DTYPE` items --
        ``np.empty(len(keys), dtype=index.ID_DTYPE)`` is the usual one -- so a hot loop can reuse
        one array. The first ``len(keys)`` items are written; the rest are left as they were. A
        read-only, strided or mistyped buffer raises ``BufferError``; one shorter than ``keys``
        raises ``ValueError``. With no keys nothing is written and ``out`` need only be a
        writable buffer.
        """

    def ids_of_arrow(self, column: object) -> bytes:
        """:meth:`ids_of_bytes` over an Arrow ``utf8``/``large_utf8`` column — a pyarrow ``Array``
        or ``ChunkedArray``, a pandas ``ArrowDtype`` column, a polars ``Series`` — read straight
        from its offset and data buffers, so no Python string exists per key. A null comes
        back as :attr:`MISSING_ID`."""

    def ids_into_arrow(self, column: object, out: Buffer) -> None:
        """:meth:`ids_of_arrow` written into ``out``, as :meth:`ids_into` does for a list."""

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint32 here); the width differs between
    the index types, so read it from the class rather than hardcoding one."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def keys_of(self, ids: Sequence[int]) -> list[str | None]: ...
    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> PerfectHashIndex:
        """Reconstruct from a ``to_bytes`` blob.

        Every length the index will read is checked against the bytes present, so arbitrary input
        raises ``ValueError`` rather than misbehaving. A blob written before 1.0 is refused: its
        perfect hash came from a crate this version no longer links, so rebuild from the keys.
        """
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> PerfectHashIndex:
        """Load a file written by ``save``. Validated like ``from_bytes``."""
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> PerfectHashIndex:
        """Memory-map a file written by ``save`` and borrow it zero-copy.

        One obligation, and it is about the mapping rather than the bytes: the file must not be
        modified or truncated by any process while the index is alive (see
        ``StringIndex.load_mmap``). The header is validated as in ``from_bytes``; the payload
        checksum is skipped by design, and ``load_mmap_verified`` adds it back.
        """
    @staticmethod
    def load_mmap_verified(path: str | os.PathLike[str]) -> PerfectHashIndex:
        """``load_mmap`` plus the payload checksum ``load`` makes: one pass over the mapping at
        load, the bulk still borrowed. Same obligation as ``load_mmap``.
        """

@final
class CompactHashIndex:
    """Smallest string->dense id map: minimal perfect hash + per-key fingerprints.

    Membership is probabilistic (false-positive rate ``2 ** -fingerprint_bits``) and there is no
    reverse ``id -> key``. Use it when only ``string -> id`` is needed and size is paramount.
    """

    def __new__(
        cls,
        items: Iterable[str],
        fingerprint_bytes: int = 1,
        *,
        fingerprint_bits: int | None = None,
    ) -> CompactHashIndex: ...
    @staticmethod
    def build_to_file(
        items: Iterable[str],
        path: str | os.PathLike[str],
        fingerprint_bytes: int = 1,
        *,
        fingerprint_bits: int | None = None,
    ) -> int:
        """The constructor for a corpus that does not fit in memory, written straight to ``path``.

        The keys, in any order, are hashed as they come and their 16-byte pairs sorted in runs
        that spill beside the output; the perfect hash is built from the merged runs a chunk at a
        time and the fingerprints written at their slots, so neither the corpus, its hashes nor
        the finished table is ever held whole. Byte for byte what the constructor and ``save``
        write. Returns the number of distinct keys written; an iterable that raises aborts the
        build with ``path`` untouched.
        """
    @property
    def fingerprint_bits(self) -> int: ...
    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int | None: ...
    def id_unchecked(self, key: str) -> int: ...
    def contains(self, key: str) -> bool: ...
    def __getitem__(self, key: str, /) -> int:
        """Dense id of ``key``, raising ``KeyError`` if it is absent — the dict spelling of
        :meth:`id`. There is no ``__setitem__`` and no ``keys`` / ``values`` / ``items``: this is an
        immutable ``str -> int`` lookup, not a mapping."""

    def get(self, key: str, default: _T | None = None) -> int | _T | None:
        """Dense id of ``key``, or ``default`` (``None`` unless given)."""

    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 4-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

    def ids_into(self, keys: Sequence[str], out: Buffer) -> None:
        """:meth:`ids_of_bytes` written into memory the caller owns instead of a fresh ``bytes``.

        ``out`` is any writable C-contiguous buffer of :attr:`ID_DTYPE` items --
        ``np.empty(len(keys), dtype=index.ID_DTYPE)`` is the usual one -- so a hot loop can reuse
        one array. The first ``len(keys)`` items are written; the rest are left as they were. A
        read-only, strided or mistyped buffer raises ``BufferError``; one shorter than ``keys``
        raises ``ValueError``. With no keys nothing is written and ``out`` need only be a
        writable buffer.
        """

    def ids_of_arrow(self, column: object) -> bytes:
        """:meth:`ids_of_bytes` over an Arrow ``utf8``/``large_utf8`` column — a pyarrow ``Array``
        or ``ChunkedArray``, a pandas ``ArrowDtype`` column, a polars ``Series`` — read straight
        from its offset and data buffers, so no Python string exists per key. A null comes
        back as :attr:`MISSING_ID`."""

    def ids_into_arrow(self, column: object, out: Buffer) -> None:
        """:meth:`ids_of_arrow` written into ``out``, as :meth:`ids_into` does for a list."""

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint32 here); the width differs between
    the index types, so read it from the class rather than hardcoding one."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> CompactHashIndex:
        """Reconstruct from a ``to_bytes`` blob.

        Every length the index will read is checked against the bytes present, so arbitrary input
        raises ``ValueError`` rather than misbehaving. A blob written before 1.0 is refused: its
        perfect hash came from a crate this version no longer links, so rebuild from the keys.
        """
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> CompactHashIndex:
        """Load a file written by ``save``. Validated like ``from_bytes``."""
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> CompactHashIndex:
        """Memory-map a file written by ``save`` and borrow it zero-copy.

        One obligation, and it is about the mapping rather than the bytes: the file must not be
        modified or truncated by any process while the index is alive (see
        ``StringIndex.load_mmap``). The header is validated as in ``from_bytes``; the payload
        checksum is skipped by design, and ``load_mmap_verified`` adds it back.
        """
    @staticmethod
    def load_mmap_verified(path: str | os.PathLike[str]) -> CompactHashIndex:
        """``load_mmap`` plus the payload checksum ``load`` makes: one pass over the mapping at
        load, the bulk still borrowed. Same obligation as ``load_mmap``.
        """

@final
class ClosedHashIndex:
    """Minimal perfect hash and nothing else: ``string -> dense id`` for a closed vocabulary.

    ``id`` never says absent: a member's id, or some id in ``[0, n)`` for any other string. The
    index is the perfect hash alone, about 0.26 bytes per key. Use it when every query is a
    member by construction; ``CompactHashIndex`` the moment a stranger can ask.
    """

    def __new__(cls, items: Iterable[str]) -> ClosedHashIndex: ...
    def __len__(self) -> int: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int:
        """Dense id of ``key`` if it is a member; some id in ``[0, n)`` otherwise, ``0`` for an
        empty index. Nothing stored can tell the two apart, so there is no ``__contains__`` and
        no ``__getitem__``."""

    def ids_of(self, keys: Sequence[str]) -> list[int]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer: one 4-byte native-endian item per key, aligned
        with ``keys``, for ``np.frombuffer(buf, dtype=index.ID_DTYPE)``."""

    def ids_into(self, keys: Sequence[str], out: Buffer) -> None:
        """:meth:`ids_of_bytes` written into memory the caller owns: any writable C-contiguous
        buffer of :attr:`ID_DTYPE` items at least ``len(keys)`` long. A read-only, strided or
        mistyped buffer raises ``BufferError``; one shorter than ``keys`` raises ``ValueError``."""

    def ids_of_arrow(self, column: object) -> bytes:
        """:meth:`ids_of_bytes` over an Arrow ``utf8``/``large_utf8`` column — a pyarrow ``Array``
        or ``ChunkedArray``, a pandas ``ArrowDtype`` column, a polars ``Series`` — read straight
        from its offset and data buffers, so no Python string exists per key. A null raises
        ``ValueError``: a closed vocabulary has no id for it."""

    def ids_into_arrow(self, column: object, out: Buffer) -> None:
        """:meth:`ids_of_arrow` written into ``out``, as :meth:`ids_into` does for a list."""

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint32)."""

    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> ClosedHashIndex:
        """Reconstruct from a ``to_bytes`` blob; arbitrary input raises ``ValueError``."""
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> ClosedHashIndex:
        """Load a file written by ``save``. Validated like ``from_bytes``. There is no
        ``load_mmap``: the whole blob is the perfect hash, read into memory either way."""

@final
class DictIndex:
    """Ordered dictionary with the key stored for every id: exact ``string <-> rank`` both ways.

    Ids are ranks: ``id(key)`` is the number of keys below it, ``key(id)`` the key at that rank,
    ``lower_bound(key)`` the rank a key would have, so every range of keys is a range of ids. The
    sorted keys are front-coded in blocks with the suffixes under a static symbol table: about
    3.5 bytes per key on real words, a third of ``StringIndex``, and no automata -- no prefix or
    fuzzy queries. Built in memory; ``save`` / ``load`` only, no ``load_mmap``.
    """

    def __new__(cls, items: Iterable[str], block: int = 32) -> DictIndex:
        """``block`` keys per block, ``1..=1024``: a lookup scans up to ``block - 1`` entries and a
        reverse lookup decodes up to that many, so smaller blocks are faster and larger ones
        smaller (16 / 32 / 64 gave 4.35 / 3.52 / 3.10 bytes per key on the dictionary)."""

    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    @property
    def block(self) -> int:
        """Keys per block, as given at build time."""

    def id(self, key: str) -> int | None: ...
    def contains(self, key: str) -> bool: ...
    def __getitem__(self, key: str, /) -> int:
        """Rank of ``key``, raising ``KeyError`` if it is absent — the dict spelling of
        :meth:`id`. There is no ``__setitem__`` and no ``keys`` / ``values`` / ``items``: this is an
        immutable ``str -> int`` lookup, not a mapping."""

    def get(self, key: str, default: _T | None = None) -> int | _T | None:
        """Rank of ``key``, or ``default`` (``None`` unless given)."""

    def lower_bound(self, key: str) -> int:
        """The rank of the first key not below ``key``: its own id if it is a member, otherwise
        the id it would have, ``len(self)`` past every key. Two of these bound a range of keys as
        a range of ids."""

    def key(self, id: int) -> str | None: ...
    def keys_of(self, ids: Sequence[int]) -> list[str | None]: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer: one 8-byte native-endian item per key, aligned
        with ``keys``, :attr:`MISSING_ID` where a key is absent, for
        ``np.frombuffer(buf, dtype=index.ID_DTYPE)``."""

    def ids_into(self, keys: Sequence[str], out: Buffer) -> None:
        """:meth:`ids_of_bytes` written into memory the caller owns: any writable C-contiguous
        buffer of :attr:`ID_DTYPE` items at least ``len(keys)`` long. A read-only, strided or
        mistyped buffer raises ``BufferError``; one shorter than ``keys`` raises ``ValueError``."""

    def ids_of_arrow(self, column: object) -> bytes:
        """:meth:`ids_of_bytes` over an Arrow ``utf8``/``large_utf8`` column — a pyarrow ``Array``
        or ``ChunkedArray``, a pandas ``ArrowDtype`` column, a polars ``Series`` — read straight
        from its offset and data buffers, so no Python string exists per key. A null comes
        back as :attr:`MISSING_ID`."""

    def ids_into_arrow(self, column: object, out: Buffer) -> None:
        """:meth:`ids_of_arrow` written into ``out``, as :meth:`ids_into` does for a list."""

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint64)."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def __iter__(self) -> Iterator[tuple[str, int]]: ...
    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> DictIndex:
        """Reconstruct from a ``to_bytes`` blob; arbitrary input raises ``ValueError``."""
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> DictIndex:
        """Load a file written by ``save``, validated like ``from_bytes``."""
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> DictIndex:
        """Memory-map a file written by ``save`` and borrow the keys, the block data and the two
        offset arrays from it; the header, the symbol table and the per-block samples (eight bytes
        a block) are what the load reads.

        One obligation, and it is about the mapping rather than the bytes: the file must not be
        modified or truncated by any process while the index is alive (see
        ``StringIndex.load_mmap``). The framing is validated as in ``from_bytes``; the payload
        checksum and the walk over the per-block arrays are skipped by design, every access
        bounded instead, and ``load_mmap_verified`` adds them back.
        """
    @staticmethod
    def load_mmap_verified(path: str | os.PathLike[str]) -> DictIndex:
        """``load_mmap`` plus the checks ``load`` makes: one pass over the mapping at load, the
        keys and the block data still borrowed. Same obligation as ``load_mmap``.
        """

@final
class Overlay:
    """Add and remove keys on top of an index that is expensive to rebuild.

    All three indexes are built once from the whole key set, so adding one key has always meant
    rebuilding for the whole corpus. An overlay wraps one with the keys added since and the ids
    retired from it, leaving the base untouched and still usable.

    Ids are stable: an id is never reissued, removing a key does not renumber anything, and
    re-adding a removed key revives its original id. :meth:`compact` is the one operation that
    renumbers.
    """

    def __new__(cls, index: StringIndex | PerfectHashIndex | CompactHashIndex) -> Overlay: ...
    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id_space(self) -> int:
        """How many ids have ever been issued; :meth:`key` is ``None`` at or above this."""

    def id(self, key: str) -> int | None: ...
    def contains(self, key: str) -> bool: ...
    def __getitem__(self, key: str, /) -> int:
        """Dense id of ``key``, raising ``KeyError`` if it is absent — the dict spelling of
        :meth:`id`. There is no ``__setitem__`` and no ``keys`` / ``values`` / ``items``: this is an
        immutable ``str -> int`` lookup, not a mapping."""

    def get(self, key: str, default: _T | None = None) -> int | _T | None:
        """Dense id of ``key``, or ``default`` (``None`` unless given)."""

    def add(self, key: str) -> int:
        """Add ``key`` and return its id, reviving the id a removed key used to have."""

    def remove(self, key: str) -> bool:
        """Remove ``key``, returning whether it was there.

        Over a :class:`CompactHashIndex` base this inherits that index's false-positive rate: a
        ``contains`` that was never true of a real key can retire an id.
        """

    def key(self, id: int) -> str | None:
        """Key for ``id``. Raises ``TypeError`` on a :class:`CompactHashIndex` base, which stores
        no keys."""

    def keys(self) -> list[str]:
        """Every live key. Raises ``TypeError`` on a :class:`CompactHashIndex` base."""

    def compact(self) -> Overlay:
        """Fold the edits into a fresh base. This renumbers: ids do not survive it. Raises
        ``TypeError`` on a :class:`CompactHashIndex` base."""

    def compact_to_file(self, path: str | os.PathLike[str]) -> int:
        """:meth:`compact` written straight to ``path`` as the base's own blob, without the live
        keys ever being held in memory at once; load it with the base class's ``load`` or
        ``load_mmap`` and wrap it in a new :class:`Overlay`. Returns how many keys the file holds.
        Raises ``TypeError`` on a :class:`CompactHashIndex` base."""

    def compact_with_remap(self) -> tuple[Overlay, bytes]:
        """:meth:`compact`, and the renumbering it did: one native-endian ``uint64`` per id the
        overlay had issued (``np.frombuffer(remap, dtype="uint64")``), the new id of each old one,
        ``2**64 - 1`` where the id was retired. Raises ``TypeError`` on a
        :class:`CompactHashIndex` base."""

    def base(self) -> StringIndex | PerfectHashIndex | CompactHashIndex:
        """The index underneath, unchanged and shared with this overlay."""

    def to_bytes(self) -> bytes: ...
    def save(self, path: str | os.PathLike[str]) -> None:
        """Write :meth:`to_bytes` to ``path``, atomically: a crash or a full disk leaves the
        previous file intact rather than a truncated one under the real name."""

    @staticmethod
    def from_bytes(
        data: bytes, base: type[StringIndex] | type[PerfectHashIndex] | type[CompactHashIndex]
    ) -> Overlay:
        """Read a blob, rebuilding the base with ``base``'s own loader — pass the class itself.

        The blob records which base wrote it and a mismatch is refused, so the wrong class is an
        error rather than an unchecked read of bytes meant for something else.

        **Validated and checksummed throughout.** The header carries a check of its own and a hash
        of everything after it, verified before any of it is read, so a flipped bit anywhere raises
        ``ValueError`` rather than loading as a different key or a revived id. The framing is
        checked past the checksums too — the lengths, the additions and their UTF-8, the tombstones
        against the id space — and so is the base region, by that class's own loader.

        A blob written by ``0.12`` still loads, without the two checksums it does not carry; saving
        it again writes the current format and it gains them.
        """

    @staticmethod
    def from_untrusted_bytes(
        data: bytes, base: type[StringIndex] | type[PerfectHashIndex] | type[CompactHashIndex]
    ) -> Overlay:
        """:meth:`from_bytes` for a blob **someone else wrote**.

        The overlay's own framing is checked identically either way. What changes is the loader the
        *embedded base* is handed to, and it matters for exactly one base: a ``StringIndex`` region
        can panic ``from_bytes`` (see :meth:`StringIndex.from_untrusted_bytes`), and an overlay
        frame passes every check it makes for itself before that region is reached. Over the two
        hash bases this is the same work as :meth:`from_bytes`, whose loaders are already total.
        """

    @staticmethod
    def load(
        path: str | os.PathLike[str],
        base: type[StringIndex] | type[PerfectHashIndex] | type[CompactHashIndex],
    ) -> Overlay:
        """:meth:`from_bytes` from a file: checksummed and validated the same way."""
    @staticmethod
    def load_untrusted(
        path: str | os.PathLike[str],
        base: type[StringIndex] | type[PerfectHashIndex] | type[CompactHashIndex],
    ) -> Overlay:
        """:meth:`from_untrusted_bytes` from a file."""

class BlobInfo(TypedDict):
    """What :func:`inspect` reads out of a blob's header.

    Every field comes from the framing and none is checked against the contents: a blob that
    inspects cleanly may still fail to load, and the sizes are what the header claims. ``keys`` is
    the key count -- for an overlay, the live keys -- and is ``None`` only for an overlay over a
    base this library did not write. ``mph_bytes`` is the perfect hash's region where there is one
    (``8 * mph_bytes / keys`` is its bits per key); ``arena_bytes`` is the key arena of a
    ``PerfectHashIndex``, the fingerprint table of a ``CompactHashIndex`` or the keys and block
    data of a ``DictIndex``; ``side_entries`` the
    keys in a hash index's collision side table; ``fingerprint_bits`` the width a
    ``CompactHashIndex`` was built with.
    """

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
    overlay: OverlayInfo | None

class OverlayInfo(TypedDict):
    """An overlay's own sections, and its base inspected in turn.

    ``base_tag`` is the base type's tag in the blob (1 ``StringIndex``, 2 ``PerfectHashIndex``,
    3 ``CompactHashIndex``); ``base`` is that region inspected, ``None`` for a tag this library
    does not know; ``additions`` the keys added on top of the base and ``retired`` the ids a
    removal retired.
    """

    base_tag: int
    base: BlobInfo | None
    additions: int
    retired: int

def inspect(blob: str | os.PathLike[str] | bytes) -> BlobInfo:
    """What a blob is, from its header alone: kind, format and sizes, without loading it.

    Over a path only the header and the footer are read, so an index of gigabytes inspects in
    microseconds; an overlay's tombstone words are read too, to count its retired ids. Nothing
    is decoded or verified. Bytes that are not a lexindex blob, or a header
    whose lengths run past the end, raise ``ValueError``; so does a blob from before 1.0, with the
    type to rebuild it in the message.
    """
