"""Type stubs for lexindex."""

import os
from collections.abc import Iterable, Iterator, Sequence
from typing import final

__all__ = ["CompactHashIndex", "PerfectHashIndex", "StringIndex", "__version__"]

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
    def __len__(self) -> int: ...
    def __contains__(self, key: str, /) -> bool: ...
    def is_empty(self) -> bool: ...
    def id(self, key: str) -> int | None: ...
    def id_unchecked(self, key: str) -> int: ...
    def contains(self, key: str) -> bool: ...
    def key(self, id: int) -> str | None: ...
    def ids_of(self, keys: Sequence[str]) -> list[int | None]: ...
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
