r"""N-gram hints: what followed a context in a training corpus, looked up by the context.

For n = 6, 10 and 20 tokens this builds a CompactHashIndex over every distinct n-token context of
a training slice, and beside it two arrays indexed by the index's id: the most frequent next
token (``uint16``) and one byte holding a count bucket and a purity bucket (the top token's share)
-- three bytes a context. A held-out slice then asks, at every position, for the longest context
that has a hint, the way an n-gram-hinted language model would.

A key is the context's token ids as characters: GPT-2's 50 257 ids are all below U+D800, so each id
*is* a Unicode scalar value; ``scalar`` below also moves ids past the surrogate gap for larger
vocabularies. See "Token sequences as keys" in docs/usage.md.

The input is a modded-nanogpt token shard, as in the Hugging Face dataset kjj0/fineweb100B-gpt2: a
1024-byte header of 256 little-endian int32 -- [0] = 20240520, [1] = 1, [2] = the token count --
then the tokens as little-endian uint16. Without a path, a small synthetic shard is written first::

    python examples/ngram_hints.py [SHARD.bin] [--train-tokens N] [--val-tokens N]
"""

from __future__ import annotations

import argparse
import array
import random
import tempfile
from collections import Counter
from pathlib import Path

from lexindex import CompactHashIndex

LEVELS = (6, 10, 20)
COUNT_EDGES = (1, 2, 4, 8, 16, 64)  # 7 buckets: 1, 2, 3-4, 5-8, 9-16, 17-64, more
PURITY_EDGES = (0.2, 0.4, 0.6, 0.8, 1.0)  # 6 buckets: below each edge, then exactly 1


def scalar(token: int) -> str:
    """One character per token id, injective and order-preserving up to 1 112 063."""
    return chr(token + 0x800 if token >= 0xD800 else token)


def token(char: str) -> int:
    """The inverse of ``scalar``."""
    return ord(char) - (0x800 if ord(char) >= 0xE000 else 0)


def read_shard(path: Path, limit: int) -> array.array:
    with path.open("rb") as f:
        header = array.array("i")
        header.frombytes(f.read(1024))
        if header[0] != 20240520 or header[1] != 1:
            raise ValueError(f"{path}: not a version-1 token shard (header {header[:3].tolist()})")
        tokens = array.array("H")
        tokens.frombytes(f.read(2 * min(limit, header[2])))
    return tokens


def write_synthetic(path: Path, n_tokens: int) -> None:
    """Documents drawn from a few recurring sentences, so long contexts repeat."""
    rng = random.Random(7)
    sentences = [[rng.randrange(50257) for _ in range(rng.randrange(8, 40))] for _ in range(200)]
    tokens = array.array("H")
    while len(tokens) < n_tokens:
        tokens.append(50256)  # GPT-2's <|endoftext|> between documents
        for _ in range(rng.randrange(3, 12)):
            tokens.extend(rng.choice(sentences))
    header = array.array("i", [20240520, 1, n_tokens] + [0] * 253)
    path.write_bytes(header.tobytes() + tokens[:n_tokens].tobytes())


def bucket(value: float, edges: tuple[float, ...]) -> int:
    return next((i for i, edge in enumerate(edges) if value <= edge), len(edges))


class Level:
    """The contexts of one length, their index and their hints."""

    def __init__(self, text: str, n: int, fingerprint_bits: int) -> None:
        self.n = n
        follow: dict[str, Counter[str]] = {}
        for j in range(len(text) - n):
            follow.setdefault(text[j : j + n], Counter())[text[j + n]] += 1
        contexts = list(follow)
        self.index = CompactHashIndex(contexts, fingerprint_bits=fingerprint_bits)
        self.next = array.array("H", bytes(2 * len(contexts)))
        self.stats = bytearray(len(contexts))  # count bucket << 3 | purity bucket
        for context, i in zip(contexts, self.index.ids_of(contexts), strict=True):
            (top, top_count), total = follow[context].most_common(1)[0], follow[context].total()
            purity = top_count / total
            self.next[i] = token(top)
            self.stats[i] = bucket(total, COUNT_EDGES) << 3 | bucket(purity, PURITY_EDGES)

    def nbytes(self) -> int:
        return self.index.serialized_len() + 2 * len(self.next) + len(self.stats)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("shard", nargs="?", type=Path)
    parser.add_argument("--train-tokens", type=int, default=2_000_000)
    parser.add_argument("--val-tokens", type=int, default=200_000)
    # A context never seen reads as present at 2 ** -bits, and most held-out contexts were never
    # seen unless the training slice is billions of tokens: at 8 bits (1.24 bytes a context) the
    # false hits of three levels outnumber the true ones on a 2 M-token slice. 16 bits is 2.24.
    parser.add_argument("--fingerprint-bits", type=int, default=16)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        shard = args.shard
        if shard is None:
            shard = Path(tmp) / "synthetic.bin"
            write_synthetic(shard, args.train_tokens + args.val_tokens)
        tokens = read_shard(shard, args.train_tokens + args.val_tokens)
    text = "".join(map(scalar, tokens))
    train, val = text[: args.train_tokens], text[args.train_tokens :]
    levels = [Level(train, n, args.fingerprint_bits) for n in LEVELS]

    hinted = correct = 0
    by_level = Counter()
    positions = range(max(LEVELS) - 1, len(val) - 1)
    for level in levels:
        n = level.n
        ids = level.index.ids_of([val[j - n + 1 : j + 1] for j in positions])
        level.hits = dict(zip(positions, ids, strict=True))
    for j in positions:
        for level in reversed(levels):  # the longest context with a hint wins
            i = level.hits[j]
            if i is not None:
                hinted += 1
                by_level[level.n] += 1
                correct += level.next[i] == token(val[j + 1])
                break
    for level in levels:
        per_token = level.nbytes() / len(train)
        share = by_level[level.n] / len(positions)
        print(
            f"n={level.n:2}: {len(level.index):>9,} contexts, {per_token:.2f} B a training token, "
            f"longest hint at {share:6.1%} of positions"
        )
    print(
        f"hinted {hinted / len(positions):.1%} of {len(positions):,} held-out positions, "
        f"top-1 right at {correct / max(hinted, 1):.1%} of those"
    )


if __name__ == "__main__":
    main()
