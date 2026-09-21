---
name: A published number looks wrong
about: A benchmark in the README or docs does not reproduce for you
labels: benchmarks
---

Every number in this repository names the artifact it came from and the commit it was measured at,
so a disagreement is answerable rather than a matter of opinion. Please give:

**Which number**
The table and the cell -- e.g. "`DictIndex` 2.64 bytes/key on `words` in the README".

**What you measured instead**
```
# the command, and its output
```

**Your machine**
CPU, RAM, OS, and whether it was otherwise idle. Sizes should reproduce anywhere; **latency will
not** -- it is a statement about one machine's thermal envelope, which is why the tables publish
ratios against a control row.

**If it is a size**, this is a real disagreement and a size is deterministic: the corpus is the
likely difference. `python bench/corpora.py verify` re-hashes what is on disk.
