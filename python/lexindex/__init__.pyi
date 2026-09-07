"""Type stubs for lexindex."""

import os
from collections.abc import Callable, Iterable, Iterator, Sequence
from typing import ClassVar, final

__all__ = ["CompactHashIndex", "Overlay", "PerfectHashIndex", "StringIndex", "__version__"]

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

    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int | None: ...
    def contains(self, key: str) -> bool: ...
    def key(self, id: int) -> str | None: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 8-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint64 here); the width differs between
    the index types, so read it from the class rather than hardcoding one."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def keys_of(self, ids: Sequence[int]) -> list[str | None]: ...
    def prefix(self, prefix: str, limit: int | None = None) -> list[tuple[str, int]]: ...
    def range(self, lo: str, hi: str, limit: int | None = None) -> list[tuple[str, int]]: ...
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
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> StringIndex: ...
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> StringIndex:
        """Memory-map the file and borrow the index from it — no read into RAM.

        The file must not be modified or truncated by any process while the index is alive: the
        bytes are borrowed, not copied, so a concurrent write is undefined behaviour rather than a
        stale answer (the Rust loader is ``unsafe fn``). Use ``load`` if the file may change.
        """

@final
class PerfectHashIndex:
    """Minimal-perfect-hash dictionary: fastest exact string->dense id, with persistence."""

    def __new__(cls, items: Iterable[str]) -> PerfectHashIndex: ...
    @staticmethod
    def build_to_file(source: Callable[[], Iterable[str]], path: str | os.PathLike[str]) -> int:
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
    def id(self, key: str) -> int | None: ...
    def id_unchecked(self, key: str) -> int: ...
    def contains(self, key: str) -> bool: ...
    def key(self, id: int) -> str | None: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 4-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

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
        """Reconstruct from a ``to_bytes`` blob **written by this library**.

        The framing is validated and checksummed, so accidental corruption fails cleanly, but the
        embedded perfect hash cannot be validated: a deliberately crafted blob is undefined
        behaviour (the Rust loader is ``unsafe fn``). Never pass bytes from an untrusted source.
        """
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> PerfectHashIndex:
        """Load a file **written by this library's** ``save`` — see ``from_bytes`` for why a crafted
        file cannot be rejected.
        """
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> PerfectHashIndex:
        """Memory-map a file **written by this library's** ``save`` and borrow it zero-copy.

        Two obligations: the file must be trusted (see ``from_bytes``), and it must not be modified
        or truncated by any process while the index is alive (see ``StringIndex.load_mmap``).
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
    @property
    def fingerprint_bits(self) -> int: ...
    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int | None: ...
    def id_unchecked(self, key: str) -> int: ...
    def contains(self, key: str) -> bool: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
    def ids_of_bytes(self, keys: Sequence[str]) -> bytes:
        """Batched ``id`` packed into a buffer instead of a list, for ``numpy`` / ``array`` callers.

        One 4-byte native-endian item per key, aligned with ``keys``, :attr:`MISSING_ID` where a
        key is absent. ``np.frombuffer(buf, dtype=index.ID_DTYPE)`` shares the memory rather than
        copying it; ``ids_of`` has to build one Python ``int`` per key, which is what this avoids.
        """

    ID_DTYPE: ClassVar[str]
    """``numpy`` dtype of one :meth:`ids_of_bytes` item (uint32 here); the width differs between
    the index types, so read it from the class rather than hardcoding one."""

    MISSING_ID: ClassVar[int]
    """The :meth:`ids_of_bytes` item standing for an absent key."""

    def to_bytes(self) -> bytes: ...
    def serialized_len(self) -> int: ...
    @staticmethod
    def from_bytes(data: bytes) -> CompactHashIndex:
        """Reconstruct from a ``to_bytes`` blob **written by this library**.

        The framing is validated and checksummed, so accidental corruption fails cleanly, but the
        embedded perfect hash cannot be validated: a deliberately crafted blob is undefined
        behaviour (the Rust loader is ``unsafe fn``). Never pass bytes from an untrusted source.
        """
    def save(self, path: str | os.PathLike[str]) -> None: ...
    @staticmethod
    def load(path: str | os.PathLike[str]) -> CompactHashIndex:
        """Load a file **written by this library's** ``save`` — see ``from_bytes`` for why a crafted
        file cannot be rejected.
        """
    @staticmethod
    def load_mmap(path: str | os.PathLike[str]) -> CompactHashIndex:
        """Memory-map a file **written by this library's** ``save`` and borrow it zero-copy.

        Two obligations: the file must be trusted (see ``from_bytes``), and it must not be modified
        or truncated by any process while the index is alive (see ``StringIndex.load_mmap``).
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

        **The trust contract is the base class's own, and this method inherits it.** The overlay's
        own framing is fully validated for every base and raises ``ValueError`` when malformed;
        what follows the header goes to the base class's loader, which is checked for
        :class:`StringIndex` and unchecked for :class:`PerfectHashIndex` and
        :class:`CompactHashIndex`. Over those two, load only blobs you wrote.
        """

    @staticmethod
    def load(
        path: str | os.PathLike[str],
        base: type[StringIndex] | type[PerfectHashIndex] | type[CompactHashIndex],
    ) -> Overlay:
        """:meth:`from_bytes` from a file, inheriting the same trust contract from ``base``."""
