"""The probe set both benchmarks draw their lookups from.

Half members and half strangers, and the strangers are the harder half to get right. A miss spelled
with a character no key contains is rejected at the first node by a trie and still hashed in full by
a hash index, which is not a comparison; so a stranger here is a member with its last character
swapped for another the corpus uses, and it stays inside every structure's alphabet.

The order is shuffled with a fixed seed. A probe order that walks the keys at any fixed step is
learned by the L2 stride prefetcher, and that has reversed a ranking in this repository before.
"""

from __future__ import annotations

import random

SWAPS = 8  # tries at a last-character swap before the member is left without a stranger
GROWS = 4  # characters appended instead, in the second pass, for a corpus too dense to swap in


def _draw(keys: list[str], count: int, seed: int, grow: bool) -> tuple[list[str], list[str]]:
    member = set(keys)
    alphabet = sorted({k[-1] for k in keys})
    rng = random.Random(seed)
    probes: list[str] = []
    strangers: list[str] = []
    while len(probes) < count:
        key = keys[rng.randrange(len(keys))]
        probes.append(key)
        stranger = None
        for _ in range(SWAPS):
            swapped = key[:-1] + rng.choice(alphabet)
            if swapped not in member:
                stranger = swapped
                break
        if stranger is None and grow:
            grown = key
            for _ in range(GROWS):
                grown += alphabet[-1]
                if grown not in member:
                    stranger = grown
                    break
        if stranger is not None:
            probes.append(stranger)
            strangers.append(stranger)
    rng.shuffle(probes)
    return probes, strangers


def probe_set(keys: list[str], count: int, seed: int = 0x5EED) -> tuple[list[str], str]:
    """`count` probes, half of them members, and one stranger to check a lookup against.

    Two passes, and the second one runs for `numeric` alone: every last-digit swap of a dense
    decimal id is another dense decimal id, so no swap can ever succeed there and the strangers have
    to be grown by appending instead. Keeping that out of the first pass is deliberate — it consumes
    no random draws, so every corpus where swapping works draws exactly the probes it drew before
    this fallback existed, and the numbers already published stay reproducible."""
    for grow in (False, True):
        probes, strangers = _draw(keys, count, seed, grow)
        if strangers:
            return probes, strangers[0]
    raise ValueError("no stranger could be spelled in this corpus's alphabet")
