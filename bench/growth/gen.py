"""Key sets shaped against the double array's placer: `gen.py <kind> <n> <out>`.

Kinds: `two-level` (a root of 4 096 children, each with children from 20 000 CJK characters),
`shared-first` (rows {a, x, y} sharing a first label), `wide-rows` (rows of 8 children, every
label set distinct), `chain` (single-child chains of 12), `alpha<k>` (strings of 4 to 12
characters over an alphabet of k).
"""

import random
import sys

kind, n, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
r = random.Random(0x5CA1E)
cjk = [chr(0x4E00 + i) for i in range(20000)]
keys: set[str] = set()
if kind == "two-level":
    heads = cjk[:4096]
    while len(keys) < n:
        keys.add(r.choice(heads) + r.choice(cjk))
elif kind == "shared-first":
    while len(keys) < n:
        p = "".join(r.choice(cjk) for _ in range(3))
        for c in ("a", r.choice(cjk), r.choice(cjk)):
            keys.add(p + c)
elif kind == "wide-rows":
    while len(keys) < n:
        p = "".join(r.choice(cjk) for _ in range(3))
        for c in r.sample(cjk, 8):
            keys.add(p + c)
elif kind == "chain":
    while len(keys) < n:
        keys.add("".join(r.choice(cjk) for _ in range(12)))
elif kind.startswith("alpha"):
    a = [chr(0x4E00 + i) for i in range(int(kind[5:]))]
    while len(keys) < n:
        keys.add("".join(r.choice(a) for _ in range(r.randint(4, 12))))
else:
    sys.exit(f"unknown kind {kind}")
with open(out, "w", encoding="utf-8") as f:
    f.write("\n".join(sorted(keys)[:n]) + "\n")
