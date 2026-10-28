"""How close `DictIndex` comes to general-purpose compressors on the same keys: sizes only.

For each corpus, sorted and deduplicated as every index here stores it, in bytes a key:

  raw          the keys' own bytes
  xz           `xz -9e` (liblzma, preset 9 extreme) over the whole sorted list, one key a line: no
               random access and unlimited context, the figure a reader reaches for first
  fc-xz        front coding restarted every `--micro` keys, so that a microblock decodes alone,
               its two streams `xz`-ed apart: the suffixes, then the `(lcp, len)` pairs as LEB128
               varints -- no random access again, but the split a front-coded dictionary stores
  pair-H0      the empirical order-0 entropy of the joint `(lcp, len)` pair, and of the two
               marginals summed (the larger: a joint is never above its marginals' sum)
  zstd-micro   every microblock's suffixes compressed alone by zstd-19, with a 256 KiB dictionary
               trained on them and amortised over the keys: a general compressor held to the same
               unit of random access as the index
  DictIndex    the blob `lexindex.DictIndex(keys, block=b)` writes, at each `--dict-block`

The microblock defaults to 16 keys, `DictIndex`'s at block 256. Every size is deterministic and
needs no quiet machine. `numeric` (0 .. n-1 in decimal) is all structure and no text: `xz` sees the
counter, and no random-access structure can follow it there.

Run:
  uv run --no-project --with zstandard --with lexindex==4.5.3 python bench/codec_bound.py \\
         --corpora ../string-index-benchmarks/corpora words-full urls-1000000 ...
"""

from __future__ import annotations

import argparse
import collections
import lzma
import math
import sys
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import _results
import lexindex
import zstandard

XZ = {"format": lzma.FORMAT_XZ, "check": lzma.CHECK_NONE, "preset": 9 | lzma.PRESET_EXTREME}
ZSTD_LEVEL = 19
ZSTD_DICT = 256 * 1024
ZSTD_FRAME = 6  # the magic and frame header a raw stream would not need, taken off each microblock
TRAIN_SAMPLES = 20_000


def xz_len(data: bytes) -> int:
    return len(lzma.compress(data, **XZ))


def leb128(values: Iterable[int]) -> bytes:
    out = bytearray()
    for v in values:
        while v >= 0x80:
            out.append(v & 0x7F | 0x80)
            v >>= 7
        out.append(v)
    return bytes(out)


def entropy_bits(counts: collections.Counter[Any], n: int) -> float:
    return -sum(c / n * math.log2(c / n) for c in counts.values())


def lcp(a: bytes, b: bytes) -> int:
    n = min(len(a), len(b))
    i = 0
    while i < n and a[i] == b[i]:
        i += 1
    return i


def front_code(keys: list[bytes], micro: int) -> tuple[list[int], list[bytes]]:
    """Each key's shared prefix with the previous one, zero at every restart, and its suffix."""
    lcps, suffixes = [], []
    for i, k in enumerate(keys):
        shared = 0 if i % micro == 0 else lcp(keys[i - 1], k)
        lcps.append(shared)
        suffixes.append(k[shared:])
    return lcps, suffixes


def zstd_micro(suffixes: list[bytes], micro: int) -> tuple[int, int]:
    """Compressed bytes of every microblock alone, and the trained dictionary's own bytes."""
    blocks = [b"".join(suffixes[i : i + micro]) for i in range(0, len(suffixes), micro)]
    step = len(blocks) // TRAIN_SAMPLES + 1
    trained = zstandard.train_dictionary(ZSTD_DICT, blocks[::step], level=ZSTD_LEVEL)
    cctx = zstandard.ZstdCompressor(
        level=ZSTD_LEVEL,
        dict_data=trained,
        write_content_size=False,
        write_checksum=False,
        write_dict_id=False,
    )
    total = sum(len(cctx.compress(b)) - ZSTD_FRAME for b in blocks)
    return total, len(trained.as_bytes())


def corpus(path: Path, micro: int, dict_blocks: list[int]) -> dict[str, Any]:
    keys = sorted({k for k in path.read_bytes().split(b"\n") if k})
    n = len(keys)
    lcps, suffixes = front_code(keys, micro)
    lens = [len(s) for s in suffixes]
    micro_bytes, dict_bytes = zstd_micro(suffixes, micro)
    text = [k.decode("utf-8") for k in keys]
    blobs = {b: len(lexindex.DictIndex(text, block=b).to_bytes()) for b in dict_blocks}
    per = 1 / n
    return {
        "corpus": path.stem,
        "keys": n,
        "raw": sum(map(len, keys)) * per,
        "xz": xz_len(b"\n".join(keys) + b"\n") * per,
        "fc_xz_suffixes": xz_len(b"".join(suffixes)) * per,
        "fc_xz_pairs": xz_len(leb128(v for p in zip(lcps, lens, strict=True) for v in p)) * per,
        "pair_h0_joint": entropy_bits(collections.Counter(zip(lcps, lens, strict=True)), n) / 8,
        "pair_h0_marginals": (
            entropy_bits(collections.Counter(lcps), n) + entropy_bits(collections.Counter(lens), n)
        )
        / 8,
        "zstd_micro": micro_bytes * per,
        "zstd_dict": dict_bytes * per,
        **{f"dict_{b}": size * per for b, size in blobs.items()},
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("names", nargs="+", help="corpus names, each a `<name>.txt` in --corpora")
    ap.add_argument("--corpora", type=Path, required=True, help="directory of the corpus files")
    ap.add_argument("--micro", type=int, default=16, help="microblock restart, in keys")
    ap.add_argument("--dict-block", type=int, action="append", help="DictIndex block (repeatable)")
    args = ap.parse_args()
    dict_blocks = args.dict_block or [256, 1024]

    cells = []
    for name in args.names:
        c = corpus(args.corpora / f"{name}.txt", args.micro, dict_blocks)
        cells.append(c)
        print(
            f"{c['corpus']:<20} raw {c['raw']:7.3f}  xz {c['xz']:6.3f}  "
            f"fc-xz {c['fc_xz_suffixes'] + c['fc_xz_pairs']:6.3f} "
            f"({c['fc_xz_suffixes']:.3f} + {c['fc_xz_pairs']:.3f})  "
            f"pair-H0 {c['pair_h0_joint']:.3f}  "
            f"zstd-micro {c['zstd_micro']:6.3f} + {c['zstd_dict']:.3f}  "
            + "  ".join(f"Dict {b} {c[f'dict_{b}']:6.3f}" for b in dict_blocks),
            flush=True,
        )
    out = _results.write(
        "codec-bound",
        cells,
        unit="bytes a key",
        micro=args.micro,
        xz="liblzma preset 9 | PRESET_EXTREME, FORMAT_XZ, CHECK_NONE",
        zstd=f"level {ZSTD_LEVEL}, {ZSTD_DICT} B trained dictionary, {ZSTD_FRAME} B a frame off",
        versions=_results.versions("lexindex", "zstandard"),
        zstd_library=".".join(map(str, zstandard.ZSTD_VERSION)),
    )
    print(f"\nwrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
