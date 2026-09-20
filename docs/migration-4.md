# Upgrading to 4.0

4.0 refuses four of the five blob families 3.x wrote. This page is the whole break on one screen:
what still loads, what has to be rebuilt, how to get the keys back out of a 3.x file, and which ids
survive the rebuild. The reasoning behind the policy — why a hash change waits for a major, why a
refusal names the version that wrote the file — is in
[design, versioning and compatibility](design.md#versioning-and-compatibility).

## What loads unchanged

| 3.x structure | format | 4.0 |
|---|---|---|
| `StringIndex` | `BIX4` | **loads**, including `load_mmap`; unchanged since 0.5 |
| `Overlay` over a `StringIndex` | `OVL1` / `OVL2` | **loads**; saving again writes `OVL2` |

Nothing about these files changes on disk, so a 3.x `.bix` can be mapped by a 4.0 process while a
3.x process still has it open.

## What has to be rebuilt

| 3.x structure | format | why | where the keys come from |
|---|---|---|---|
| `DictIndex` | `BDX2` | `BDX3` shares no section layout with it | the blob itself — it stores every key |
| `PerfectHashIndex` | `BMP7` | the key hash moved, so every id would be wrong | the blob itself |
| `CompactHashIndex` | `BCH7` | the key hash moved | **the original corpus** — it stores no keys |
| `ClosedHashIndex` | `BCL1` | the key hash moved | **the original corpus** |
| `Overlay` over any of those | — | the base is refused | rebuild the base, then re-apply |

All four are refused by name, with an error that says which version wrote the file and that it needs
rebuilding — not a bare "bad magic", which sends people hunting for disk corruption in an intact
file.

## Getting the keys out of a 3.x blob

The three structures that store their keys can hand them back **under 3.x**, through the API that
release already has. `lexindex dump` is a 4.0 subcommand, so on a 3.0.0 install the loop is the
path:

```python
# with lexindex 3.0.0 installed
import lexindex

old = lexindex.DictIndex.load("catalog.bdx")   # or StringIndex / PerfectHashIndex
with open("keys.txt", "w", encoding="utf-8") as out:
    for i in range(len(old)):
        out.write(old.key(i) + "\n")
```

```rust
// with lexindex 3.0.0 as the dependency
let old = lexindex::DictIndex::load("catalog.bdx")?;
let mut out = std::io::BufWriter::new(std::fs::File::create("keys.txt")?);
for id in 0..old.len() as u64 {
    writeln!(out, "{}", old.key(id).unwrap())?;
}
```

Then rebuild on 4.0, from the CLI or the API:

```bash
pip install --upgrade lexindex      # 4.0
lexindex build keys.txt catalog.bdx --index dict
```

```python
import lexindex
keys = open("keys.txt", encoding="utf-8").read().splitlines()
lexindex.DictIndex(keys).save("catalog.bdx")
```

From 4.0 onwards the loop is a subcommand — `lexindex dump old.blob | lexindex build - new.blob` —
so the next format change costs one line rather than a script.

`CompactHashIndex` and `ClosedHashIndex` store no keys in any version. There is no dump for them at
3.x or at 4.0, and the way back is the corpus they were built from.

## What happens to the ids

**This is the part to check before rebuilding anything that stores an id outside the blob.**

| structure | id after a rebuild from the same keys |
|---|---|
| `StringIndex`, `DictIndex` | **unchanged** — the id is the lexicographic rank of the key |
| `PerfectHashIndex`, `CompactHashIndex`, `ClosedHashIndex` | **changes** — the id is a slot in a perfect hash, and both the hash and the seed geometry moved in 4.0 |

So a database column holding `DictIndex` ids survives the upgrade, and one holding
`PerfectHashIndex` ids does not: the id for a key will differ, silently and in a way nothing in the
blob can detect. Either re-derive those ids from the key after the rebuild, or keep a mapping — the
old blob answers `key(id)` until you delete it, so building `old_id → key → new_id` is one pass
while both versions are installed.

An `Overlay` over a rebuilt base is in the same position: its own ids are assigned above the base's,
so they move with the base.

## Memory-mapped files

`load_mmap` borrows the file, so a refused format fails at the header rather than after mapping it —
the error arrives before any page is touched. A 3.x `.bix` maps in 4.0 unchanged. For the four
refused families, unmap and delete the old file only after the rebuilt one is verified; nothing in
4.0 reads them again.

## Checking what you have

`lexindex inspect` reads the header alone and names the format without loading the blob, in 3.x and
4.0 both:

```bash
lexindex inspect catalog.bdx
```

```python
import lexindex
lexindex.inspect(open("catalog.bdx", "rb").read())["format"]   # 'BDX2' or 'BDX3'
```

That is the fastest way to find which of a directory's blobs the upgrade will refuse.
