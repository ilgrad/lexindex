"""Instructions a key by op, corpus and n, and the growth over the top two doublings.

`analyse.py results.txt`: a FLAG where both of the last two doublings grow the count a key by more
than 10 %, which a log factor (about 5 % a doubling at these sizes) does not reach.
"""

import collections
import sys

rows: dict[tuple[str, str], dict[int, float]] = collections.defaultdict(dict)
with open(sys.argv[1], encoding="utf-8") as results:
    for line in results:
        f = line.split()
        if len(f) < 6 or not f[4].isdigit() or f[4] == "0":
            print("skip:", line.strip())
            continue
        op, tag, ins, n = f[1], f[2], int(f[4]), int(f[5])
        corpus = tag.rsplit("-", 1)[0]
        rows[(op, corpus)][n] = ins / n
head = " ".join(f"2^{e:<6}" for e in range(16, 25))
print(f"{'op':15} {'corpus':16} {head}  top-2 growth/doubling")
for (op, corpus), by in sorted(rows.items()):
    ns = sorted(by)
    cells = " ".join(f"{by[1 << e]:8.1f}" if (1 << e) in by else " " * 8 for e in range(16, 25))
    growth = ""
    if len(ns) >= 3:
        a, b, c = (by[x] for x in ns[-3:])
        g1, g2 = b / a - 1, c / b - 1
        flag = " FLAG" if min(g1, g2) > 0.10 else (" watch" if max(g1, g2) > 0.10 else "")
        growth = f"{100 * g1:+6.1f}% {100 * g2:+6.1f}%{flag}"
    print(f"{op:15} {corpus:16} {cells}  {growth}")
