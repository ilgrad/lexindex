"""ns/op for every Python-visible lookup, per corpus — the positioning table's missing column.

One process per (corpus, n), every structure built up front, the lookup forms alternated inside
each round, minimum over the rounds.

**`dict` is the reference column, not the stability control.** It was the control until
2026-09-06, when it spread 19–79 % between rounds while `StringIndex` spread 7 % on the very same
rounds. That is not machine noise — noise moves every column alike — it is `dict` itself: at 1 M
keys its table is tens of megabytes and DRAM-bound, where `CompactHashIndex` is 1.27 MB and
cache-resident. A stability control has to be the *most* stable thing in the room, and `dict` is
the least.

**This machine drifts, idle, by 13.7 % over twelve rounds.** Measured with `calibration()` below —
a cache-resident integer loop with no memory traffic — whose time rose monotonically 17.3 → 19.6 ms
as the CPU heated. So an absolute-spread criterion of any tightness is unreachable here, whatever
the load average says, and two things follow. The estimator is the **minimum** over rounds (the
least-throttled one). The publishable quantity is the **ratio to `dict` taken inside the same
round**, where the drift divides out; absolute minima are reported beside it, with the drift.

Every number carries Python's per-call overhead, which is the point: it is what a caller actually
pays, and it is the same for every column.

Every sample of a run goes to
`bench/results/latency-py-<corpus>-pass-<pass>-<date>-<host>-<commit>.json`.

Run: ``uv run --with <lexindex wheel> --with marisa-trie python bench/latency_py.py
<corpus> <n> <rounds> <pass>``
"""

import gc
import math
import os
import sys
import time

import _results
import lexindex
import marisa_trie

WORDS = os.environ.get("LEXINDEX_BENCH_WORDS", "/usr/share/dict/words")
REFERENCE = "dict (reference)"


def vocab() -> list[str]:
    return [w.strip() for w in open(WORDS) if w.strip()]


def _grid(n: int, v: list[str]) -> list[str]:
    m = min(math.ceil(math.sqrt(n)), len(v))
    return [f"{v[k % m]}.{v[(k // m) % m]}" for k in range(n)]


def _random_pairs(n: int, v: list[str]) -> list[str]:
    w = len(v)
    out: dict[str, None] = {}
    state = 0x243F6A8885A308D3
    while len(out) < n:
        state = (state + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        z ^= z >> 31
        out[f"{v[z % w]}.{v[(z >> 32) % w]}"] = None
    return list(out)


def make_keys(corpus: str, n: int, v: list[str]) -> list[str]:
    if corpus == "words":
        return v[:n]
    return _random_pairs(n, v) if corpus == "random" else _grid(n, v)


def calibration(iters: int = 200_000) -> int:
    """A cache-resident integer loop with no memory traffic: how much its time moves between
    rounds is the machine's own drift, which nothing measured here can cause or avoid."""
    x = 0
    for _ in range(iters):
        x = (x * 1103515245 + 12345) & 0xFFFFFFFF
    return x


def probes(keys: list[str], pct: int) -> list[str]:
    """`pct`% members, in an order unrelated to the build order.

    Members are the *existing* string objects, which is what a Python caller has; absent keys are
    freshly built. No layout axis here — CPython strings are objects on the heap either way.

    Note what this hands `dict` and withholds from the Rust indexes: CPython caches a `str`'s hash
    inside the object, so from the second round on `dict` looks its keys up without rehashing,
    while lexindex rehashes the bytes on every call (measured separately: ~23 ns of the gap at
    1 M). That is the *favourable* reading for `dict`, which is the honest one to publish against.
    """
    step = 999_983  # coprime with any n used here, so it visits every index once
    out = []
    for i in range(len(keys)):
        k = keys[(i * step) % len(keys)]
        out.append(k if (i % 100) < pct else k + "~absent")
    return out


def main() -> None:
    corpus = sys.argv[1] if len(sys.argv) > 1 else "grid"
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 1_000_000
    rounds = int(sys.argv[3]) if len(sys.argv) > 3 else 7
    pass_no = int(sys.argv[4]) if len(sys.argv) > 4 else 1
    v = vocab()
    keys = make_keys(corpus, n, v)
    n = len(keys)

    built = {
        REFERENCE: {k: i for i, k in enumerate(keys)},
        "marisa": marisa_trie.Trie(keys),
        "StringIndex": lexindex.StringIndex(keys),
        "PerfectHashIndex": lexindex.PerfectHashIndex(keys),
        "CompactHashIndex": lexindex.CompactHashIndex(keys, 1),
    }
    lookups = {
        REFERENCE: lambda s, ps: [s.get(k) for k in ps],
        "marisa": lambda s, ps: [s.get(k) for k in ps],
        "StringIndex": lambda s, ps: [s.id(k) for k in ps],
        "PerfectHashIndex": lambda s, ps: [s.id(k) for k in ps],
        "CompactHashIndex": lambda s, ps: [s.id(k) for k in ps],
    }
    batched = {
        "PerfectHashIndex": lambda s, ps: s.ids_of(ps),
        "CompactHashIndex": lambda s, ps: s.ids_of(ps),
    }

    sets = [("members", 100), ("50% members", 50), ("absent", 0)]
    # Built once, outside the rounds: a fresh probe list per round would allocate n new absent
    # strings each time, so the allocator state — not the lookup — would differ between rounds.
    probe_sets = {label: probes(keys, pct) for label, pct in sets}
    names = list(built) + [f"{k}.ids_of" for k in batched]
    forms = list(lookups.items()) + [(f"{k}.ids_of", f) for k, f in batched.items()]
    per_round: dict[tuple[str, str], list[float]] = {}
    drift: list[float] = []
    was_enabled = gc.isenabled()
    gc.collect()
    gc.disable()  # a collection landing inside one variant is indistinguishable from its cost
    try:
        for r in range(rounds):
            t = time.perf_counter()
            calibration()
            drift.append(time.perf_counter() - t)
            for label, _pct in sets:
                ps = probe_sets[label]
                # Rotated every round so no form keeps the first slot, which is the one that pays
                # to pull the probe list back into cache after the previous set displaced it.
                order = forms[r % len(forms) :] + forms[: r % len(forms)]
                for name, fn in order:
                    s = built[name.removesuffix(".ids_of")]
                    t = time.perf_counter()
                    out = fn(s, ps)
                    dt = (time.perf_counter() - t) / n * 1e9
                    assert len(out) == n
                    per_round.setdefault((label, name), []).append(dt)
    finally:
        if was_enabled:
            gc.enable()

    def spread(xs: list[float]) -> float:
        return (max(xs) - min(xs)) / min(xs) * 100

    ratios = {
        (label, name): [
            per_round[(label, name)][i] / per_round[(label, REFERENCE)][i] for i in range(rounds)
        ]
        for label, _ in sets
        for name in names
    }

    written = _results.write(
        f"latency-py-{corpus}-pass-{pass_no}",
        [
            {"probe_set": label, "form": name, **_results.summary(per_round[(label, name)])}
            for label, _ in sets
            for name in names
        ],
        corpus=corpus,
        keys=n,
        calibration_s=drift,
        competitors=_results.versions("marisa-trie"),
    )
    print(f"samples in {written}")
    print(f"corpus={corpus} n={n} rounds={rounds} load={os.getloadavg()[0]:.1f}")
    print(
        f"machine drift over the run (calibration loop, no memory traffic): {spread(drift):.1f}%"
        f"  [{min(drift) * 1e3:.1f}–{max(drift) * 1e3:.1f} ms]"
    )
    print("\nmin ns/op (spread over rounds — bounded below by the drift above)")
    print(f"{'probe set':<14}" + "".join(f"{m:>26}" for m in names))
    for label, _ in sets:
        row = f"{label:<14}"
        for name in names:
            row += (
                f"{min(per_round[(label, name)]):>19.1f} ±{spread(per_round[(label, name)]):>4.1f}%"
            )
        print(row, flush=True)

    # Ratio to `dict`, both timed in the same round, so the thermal drift that moves every
    # absolute number divides out. This is the quantity the README can carry.
    # Ratio **of the minima**, not the minimum of the per-round ratios: the latter picks the round
    # where the numerator happened to be low *and* the denominator high, so it is biased low and
    # moved 40 % between two passes of the same configuration. Each minimum is separately robust,
    # so their quotient is the stable quantity. The per-round ratio spread stays as information.
    print("\nratio of minima to dict (per-round ratio spread beside it, for information)")
    print(f"{'probe set':<14}" + "".join(f"{m:>26}" for m in names))
    for label, _ in sets:
        row = f"{label:<14}"
        for name in names:
            q = min(per_round[(label, name)]) / min(per_round[(label, REFERENCE)])
            row += f"{q:>19.2f}x±{spread(ratios[(label, name)]):>4.1f}%"
        print(row, flush=True)

    # Neither absolutes nor ratios can be gated on their round-to-round spread here. The drift
    # loop puts the machine's floor at 4–28 %, and `dict` — the ratio's denominator — is itself the
    # least stable column, because at these sizes its table sits right at the L3 boundary (~16 MB
    # at 480 k keys) where `CompactHashIndex` (0.6 MB) is fully resident. So the estimator is the
    # minimum and the validation is the one the Rust table already used: **two independent runs
    # must agree on the ratios**. Set LEXINDEX_LAT_JSON to write this run; a second run against the
    # same file prints the agreement and the verdict.
    worst_ratio = max(spread(ratios[(label, name)]) for label, _ in sets for name in names)
    print(f"\nworst ratio spread {worst_ratio:.1f}% (information, not a gate)", flush=True)

    out_path = os.environ.get("LEXINDEX_LAT_JSON")
    if out_path:
        import json

        # The gate is on the **ratios**, because the ratios are what gets published. Gating the
        # absolute minima instead refused three tables whose ratios agreed to 1-8 %: the minima
        # carried the machine's thermal state, and `dict` — documented above as the least stable
        # column here — was the worst offender in every one of them, which is precisely the drift
        # the ratio divides out. The minima agreement stays, as information about the session.
        key = f"{corpus}/{n}"
        mins = {
            f"{label}|{name}": min(per_round[(label, name)]) for label, _ in sets for name in names
        }
        quots = {
            f"{label}|{name}": min(per_round[(label, name)]) / min(per_round[(label, REFERENCE)])
            for label, _ in sets
            for name in names
        }
        store = {}
        if os.path.exists(out_path):
            with open(out_path) as f:
                store = json.load(f)
        prev = store.get(key)
        if prev and "ratios" in prev:

            def agreement(now: dict, before: dict) -> tuple[str, float, float]:
                d = {k: abs(now[k] - before[k]) / min(now[k], before[k]) * 100 for k in now}
                worst = max(d, key=d.get)
                return worst, d[worst], sorted(d.values())[len(d) // 2]

            mk, mworst, mmed = agreement(mins, prev["mins"])
            rk, rworst, rmed = agreement(quots, prev["ratios"])
            print(
                f"\ncross-run agreement vs the previous run of {key}:"
                f"\n  minima (information): worst {mworst:.1f}% on {mk}, median {mmed:.1f}%"
                f"\n  ratios (the gate):    worst {rworst:.1f}% on {rk}, median {rmed:.1f}%"
            )
            ok = rworst <= 10.0
            print("PUBLISHABLE" if ok else "NOT PUBLISHABLE — ratios disagree between runs")
        else:
            print(f"\n{key} recorded; run again against the same file to validate")
        store[key] = {"mins": mins, "ratios": quots}
        with open(out_path, "w") as f:
            json.dump(store, f, indent=1)
    print(f"load at exit {os.getloadavg()[0]:.1f}", flush=True)


if __name__ == "__main__":
    main()
