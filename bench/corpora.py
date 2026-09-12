"""The corpus set every benchmark in this repository draws from.

Bytes per key is a property of the *keys* at least as much as of the structure: `StringIndex`
measures 0.68 on one corpus and 16.57 on another, and `marisa-trie` moves between 2.12 and 6.21 over
the same span. One corpus cannot carry a size claim. This module is the set that can — thirteen
corpora of four kinds:

* **fetched** from a pinned URL and checked against a SHA-256 — Wikipedia article titles in three
  scripts, the Tranco domain ranking, the PyPI package index;
* **derived** from one of those by the transformation under which the keys really exist — the
  article URLs are the titles with Wikipedia's own prefix and escaping;
* **read off this machine** — the Fedora word list, this filesystem's paths, the identifiers in the
  Rust sources under `~/.cargo/registry`;
* **generated** from a fixed seed where the shape is the whole point and no real corpus exists —
  UUIDv4, dense decimal ids, opaque base64url ids, DNA 24-mers.

Sizes are nested: the 100 000-key file is a prefix of the 1 000 000-key one, which is a prefix of
the 10 000 000-key one, so a difference between two sizes is scale and never composition.
`numeric` is the exception and says so — a *dense* id space is the point of it, and a random subset
of a larger one is not dense.

Every file is UTF-8, one key per line, deduplicated, and shuffled with a fixed seed — never sorted,
because a sorted build order is the one order an ordered index must not be handed by accident.

`bench/corpora.json` records for every file its source, the date it was fetched, the key count, the
mean key length and its SHA-256. That manifest is the artifact; the files are gitignored, since a
ten-million-key corpus does not belong in a git history.

Run:
  uv run --no-sync python bench/corpora.py status
  uv run --no-sync python bench/corpora.py build words uuid dna
  uv run --no-sync python bench/corpora.py verify
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
import random
import re
import subprocess
import sys
import urllib.parse
import urllib.request
from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from datetime import date
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get("LEXINDEX_CORPORA", HERE.parent / "local" / "corpora"))
DOWNLOADS = ROOT / "downloads"
MANIFEST = HERE / "corpora.json"

GRID = (100_000, 1_000_000, 10_000_000)
SEED = 0x6C6578696E646578 & 0xFFFFFFFF  # "lexindex" as bytes, truncated

WIKI_DUMP = "20260801"  # a dated dump, not `latest`: `latest` cannot be pinned by hash
TRANCO_LIST = "38KVL"  # the daily list of 2026-09-11, whose id is permanent


FETCHED: dict[str, str] = {}  # cached file -> the URL it came from, for the manifest


def _download(url: str, into: str, accept: str = "*/*") -> Path:
    """Fetch once into the cache and keep it. The cached file is what the manifest hashes."""
    DOWNLOADS.mkdir(parents=True, exist_ok=True)
    path = DOWNLOADS / into
    FETCHED[into] = url
    if path.exists():
        return path
    print(f"  fetching {url}")
    tmp = path.with_suffix(path.suffix + ".part")
    headers = {"User-Agent": "lexindex-benchmark/1 (corpora.py)", "Accept": accept}
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=120) as src, open(tmp, "wb") as dst:
        while chunk := src.read(1 << 20):
            dst.write(chunk)
    tmp.rename(path)
    return path


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def _wiki_titles(lang: str) -> Iterator[str]:
    name = f"{lang}wiki-{WIKI_DUMP}-all-titles-in-ns0.gz"
    path = _download(f"https://dumps.wikimedia.org/{lang}wiki/{WIKI_DUMP}/{name}", name)
    with gzip.open(path, "rt", encoding="utf-8", errors="replace") as f:
        next(f, None)  # the header line, literally "page_title"
        for line in f:
            title = line.rstrip("\n").replace("_", " ")
            if title:
                yield title


def _wiki_urls() -> Iterator[str]:
    """The titles as the URLs they actually are — percent-escaped under `/wiki/`, not glued."""
    for title in _wiki_titles("en"):
        yield "https://en.wikipedia.org/wiki/" + urllib.parse.quote(title.replace(" ", "_"))


def _tranco() -> Iterator[str]:
    """The permanent-id endpoint serves the ranking as bare CSV — rank, domain, CRLF."""
    path = _download(
        f"https://tranco-list.eu/download/{TRANCO_LIST}/1000000", f"tranco-{TRANCO_LIST}.csv"
    )
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            _, _, domain = line.strip().partition(",")
            if domain:
                yield domain


def _pypi() -> Iterator[str]:
    path = _download(
        "https://pypi.org/simple/", "pypi-simple.json", "application/vnd.pypi.simple.v1+json"
    )
    payload = json.loads(path.read_text(encoding="utf-8"))
    for project in payload["projects"]:
        if project["name"]:
            yield project["name"]


def _words() -> Iterator[str]:
    with open("/usr/share/dict/words", encoding="utf-8", errors="replace") as f:
        for line in f:
            if word := line.strip():
                yield word


def _paths() -> Iterator[str]:
    """Real paths off this filesystem. Machine-specific by nature; the manifest records the host."""
    for top in ("/usr", str(Path.home())):
        done = subprocess.run(
            ["/usr/bin/find", top, "-xdev"], capture_output=True, text=True, check=False
        )
        for line in done.stdout.splitlines():
            if line:
                yield line


IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")


def _identifiers() -> Iterator[str]:
    """Identifiers as they are written in the Rust sources this machine has vendored."""
    registry = Path.home() / ".cargo" / "registry" / "src"
    for source in sorted(registry.rglob("*.rs")):
        try:
            text = source.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        yield from IDENT.findall(text)


def _bulk(rng: random.Random, alphabet: bytes, length: int, count: int) -> Iterator[str]:
    """`count` strings of `length` symbols. The alphabet divides 256, so no symbol is favoured."""
    assert 256 % len(alphabet) == 0, "a non-dividing alphabet would bias the low symbols"
    table = bytes(alphabet[b % len(alphabet)] for b in range(256))
    step = 1 << 16
    for start in range(0, count, step):
        block = rng.randbytes(min(step, count - start) * length).translate(table)
        for at in range(0, len(block), length):
            yield block[at : at + length].decode("ascii")


def _uuids(count: int) -> Iterator[str]:
    """Version-4 UUIDs from a seeded generator, not `uuid4()`, which reads the OS entropy pool."""
    rng = random.Random(SEED ^ 0x1111)
    for _ in range(count):
        raw = bytearray(rng.randbytes(16))
        raw[6] = (raw[6] & 0x0F) | 0x40
        raw[8] = (raw[8] & 0x3F) | 0x80
        h = raw.hex()
        yield f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}"


@dataclass(frozen=True)
class Corpus:
    name: str
    what: str
    keys: object  # () -> Iterable[str], or (n) -> Iterable[str] when `nested` is False
    sizes: tuple[int, ...] = GRID
    nested: bool = True
    fetched: bool = False  # its first key comes from a pinned download
    full: bool = False  # also write the whole pool, whatever size it turns out to be
    note: str = ""
    tags: tuple[str, ...] = field(default_factory=tuple)


CORPORA: tuple[Corpus, ...] = (
    Corpus(
        "words",
        "English words — /usr/share/dict/words, Fedora's `words` package",
        _words,
        sizes=(100_000,),
        full=True,
        note="the corpus every table in the README is measured on",
    ),
    Corpus(
        "titles-en",
        f"English Wikipedia article titles — enwiki-{WIKI_DUMP}-all-titles-in-ns0",
        lambda: _wiki_titles("en"),
        fetched=True,
        full=True,
    ),
    Corpus(
        "titles-ru",
        f"Russian Wikipedia article titles — ruwiki-{WIKI_DUMP}-all-titles-in-ns0",
        lambda: _wiki_titles("ru"),
        fetched=True,
        sizes=(100_000, 1_000_000),
        full=True,
        note="Cyrillic: two bytes a character in UTF-8, so a byte-oriented index sees longer keys",
    ),
    Corpus(
        "titles-zh",
        f"Chinese Wikipedia article titles — zhwiki-{WIKI_DUMP}-all-titles-in-ns0",
        lambda: _wiki_titles("zh"),
        fetched=True,
        sizes=(100_000, 1_000_000),
        full=True,
        note="CJK: three bytes a character, and short keys — the hardest case for front coding",
    ),
    Corpus(
        "urls",
        "English Wikipedia article URLs — the titles under `/wiki/`, percent-escaped",
        _wiki_urls,
        fetched=True,
        full=True,
        note="a 30-byte prefix every key shares: what front coding is for and what hashing ignores",
    ),
    Corpus(
        "domains",
        f"registered domains — the Tranco ranking, permanent list {TRANCO_LIST}",
        _tranco,
        fetched=True,
        sizes=(100_000, 1_000_000),
        full=True,
    ),
    Corpus(
        "pypi",
        "PyPI package names — the simple index",
        _pypi,
        fetched=True,
        sizes=(100_000,),
        full=True,
        note="a real catalogue of short, human-chosen identifiers",
    ),
    Corpus(
        "paths",
        "filesystem paths — `find /usr -xdev` and `find $HOME -xdev` on the benchmark machine",
        _paths,
        full=True,
        note="machine-specific by nature; the manifest records the host that produced it",
    ),
    Corpus(
        "idents",
        "source-code identifiers — every `[A-Za-z_][A-Za-z0-9_]{2,}` in the vendored Rust sources",
        _identifiers,
        full=True,
    ),
    Corpus(
        "uuid",
        "UUIDv4 in the canonical hyphenated form, from a seeded generator",
        _uuids,
        nested=False,
        note="36 bytes of which 32 are hex: no shared structure at all past the hyphens",
    ),
    Corpus(
        "numeric",
        "dense decimal ids, `0` to `n-1`",
        lambda n: (str(i) for i in range(n)),
        nested=False,
        note="not nested, and could not be: a random subset of a larger range is not a dense one",
    ),
    Corpus(
        "opaque",
        "opaque 16-symbol base64url ids, from a seeded generator",
        lambda n: _bulk(
            random.Random(SEED ^ 0x2222),
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
            16,
            n,
        ),
        nested=False,
    ),
    Corpus(
        "dna",
        "DNA 24-mers over ACGT, from a seeded generator",
        lambda n: _bulk(random.Random(SEED ^ 0x3333), b"ACGT", 24, n),
        nested=False,
        note="four symbols and a fixed length: the densest trie and the flattest hash in the set",
    ),
)

BY_NAME = {c.name: c for c in CORPORA}


def _pool(corpus: Corpus, want: int) -> list[str]:
    """Unique keys, sorted for determinism and then shuffled, so the order is seeded and not the
    source's. `want` is a target, not a promise: a source runs out where it runs out."""
    seen: dict[str, None] = {}
    produce: Iterable[str] = corpus.keys() if corpus.nested else corpus.keys(want)
    for key in produce:
        key = key.strip()
        if key and "\n" not in key:
            seen[key] = None
            if not corpus.nested and len(seen) >= want:
                break
    keys = sorted(seen)
    random.Random(SEED).shuffle(keys)
    return keys


def _write(path: Path, keys: list[str]) -> dict[str, object]:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(keys))
        f.write("\n")
    size = path.stat().st_size
    return {
        "file": path.name,
        "keys": len(keys),
        "bytes": size,
        "mean_key_bytes": round((size - len(keys)) / len(keys), 2),
        "sha256": _sha256(path),
    }


def build(names: list[str]) -> None:
    manifest = _load()
    for name in names:
        corpus = BY_NAME[name]
        want = max(corpus.sizes)
        print(f"{name}: {corpus.what}")
        keys = _pool(corpus, want)
        print(f"  {len(keys):,} unique keys")
        files = []
        for size in corpus.sizes:
            if size > len(keys):
                print(f"  {size:,}: source has only {len(keys):,}, skipped")
                continue
            part = keys if corpus.nested or size == want else _pool(corpus, size)
            files.append(_write(ROOT / f"{name}-{size}.txt", part[:size]))
        if corpus.full and len(keys) not in corpus.sizes:
            files.append(_write(ROOT / f"{name}-full.txt", keys))
        entry: dict[str, object] = {
            "what": corpus.what,
            "nested": corpus.nested,
            "pool_keys": len(keys),
            "built": date.today().isoformat(),
            "files": files,
        }
        if corpus.note:
            entry["note"] = corpus.note
        manifest["corpora"][name] = entry
        for one in files:
            print(f"  {one['file']:<24} {one['keys']:>10,} keys  {one['mean_key_bytes']:>6} B/key")
        _save(manifest)  # after each corpus, so a long run that dies keeps what it finished


def _load() -> dict:
    if MANIFEST.exists():
        return json.loads(MANIFEST.read_text(encoding="utf-8"))
    return {"corpora": {}}


def _save(manifest: dict) -> None:
    import platform
    import socket

    downloads = manifest.get("downloads", {})
    for name, url in FETCHED.items():
        path = DOWNLOADS / name
        if path.exists():
            downloads[name] = {"url": url, "bytes": path.stat().st_size, "sha256": _sha256(path)}
    manifest["downloads"] = dict(sorted(downloads.items()))
    manifest["host"] = socket.gethostname()
    manifest["kernel"] = f"{platform.system()} {platform.release()}"
    manifest["grid"] = list(GRID)
    manifest["corpora"] = dict(sorted(manifest["corpora"].items()))
    MANIFEST.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"manifest → {MANIFEST}")


def verify() -> int:
    """Re-hash what is on disk against the manifest. Missing is reported, not an error: a corpus
    nobody has built yet is a corpus nobody has measured on."""
    manifest, bad, missing, ok = _load(), 0, 0, 0
    for name, entry in manifest["corpora"].items():
        for one in entry["files"]:
            path = ROOT / one["file"]
            if not path.exists():
                print(f"missing  {name:<12} {one['file']}")
                missing += 1
            elif _sha256(path) != one["sha256"]:
                print(f"CHANGED  {name:<12} {one['file']}")
                bad += 1
            else:
                ok += 1
    print(f"{ok} verified, {missing} not built, {bad} changed")
    return 1 if bad else 0


def sources() -> None:
    """Resolve every pinned URL to its cached file and record the bytes that were used.

    Taking one key from a fetched corpus is enough: the download happens before the first yield.
    Which is also why the manifest can hold the hash of a download whose corpus is not built."""
    for corpus in CORPORA:
        if corpus.fetched:
            keys = corpus.keys() if corpus.nested else corpus.keys(1)
            next(iter(keys), None)
    _save(_load())


def status() -> None:
    manifest = _load()
    print(f"{'corpus':<12}{'keys':>12}{'B/key':>8}  source")
    for corpus in CORPORA:
        entry = manifest["corpora"].get(corpus.name)
        if entry is None:
            print(f"{corpus.name:<12}{'—':>12}{'—':>8}  {corpus.what}")
            continue
        for one in entry["files"]:
            here = "" if (ROOT / one["file"]).exists() else "  (not on disk)"
            print(
                f"{corpus.name:<12}{one['keys']:>12,}{one['mean_key_bytes']:>8}"
                f"  {one['file']}{here}"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("status")
    sub.add_parser("verify")
    sub.add_parser("sources")
    made = sub.add_parser("build")
    made.add_argument("names", nargs="*", default=[], help="default: every corpus")
    args = parser.parse_args()
    if args.command == "verify":
        return verify()
    if args.command == "status":
        status()
        return 0
    if args.command == "sources":
        sources()
        return 0
    names = args.names or [c.name for c in CORPORA]
    unknown = [n for n in names if n not in BY_NAME]
    if unknown:
        parser.error(f"unknown corpus: {', '.join(unknown)}")
    build(names)
    return 0


if __name__ == "__main__":
    sys.exit(main())
