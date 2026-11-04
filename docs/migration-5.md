# Upgrading to 5.0

5.0 changes what a program writes against `plan`, `Overlay` and `IndexError`. Most of this page fails
where it should be looked at: a compile error in Rust, a `TypeError` in Python, an unknown flag on
the command line. Two changes do not fail, and they are the ones to read: a bare `plan` now answers a
different question, and code that matched a refused build as `IndexError::Format` no longer matches.

| | 4.x | 5.0 |
|---|---|---|
| Default vocabulary of `plan` in Rust and Python | closed | **open**, as the command line already was |
| Asking for a closed one | the default | `closed_vocabulary()`, `closed_vocabulary=True`, `--closed-vocabulary` |
| `Needs::exact()`, `exact=True`, `--exact` | ask for an open one | **gone**: the default |
| `Kind` | five variants, exhaustive | seven, `#[non_exhaustive]` |
| `Needs`, `Estimate` | open to literals | `#[non_exhaustive]` |
| `Estimate` | `bytes`, `block`, `measured` | also `micro`, `resident_bytes`, `build_secs` |
| `DictProfile::Fast`, `block="fast"`, `--block fast` | block 32 | block 128 in microblocks of 8 |
| `DictProfile::block()` | the profile's block | **gone**: `layout()`, a `DictLayout` for `build_with` |
| Common-prefix and longest-prefix walks | asked for `prefix` | ask for `common_prefix` |
| `--index auto` | never a `HashedDictIndex` or a `DoubleArrayIndex` | may pick either |
| `Overlay::remove` over `CompactHashIndex` | compiled, and could retire a real key's id | does not compile; `TypeError` in Python |
| A refused build | `IndexError::Format` | `IndexError::Build` |
| `IndexError` | exhaustive | `#[non_exhaustive]` |
| A 4.x blob of a hash index | read | refused by name: [dump and build, or build from your keys](#which-4x-blobs-50-reads) |

## `plan` assumes an open vocabulary

A key the index never held now has to be answered as one, so a bare `plan` leaves out
`CompactHashIndex` and `ClosedHashIndex`. In 4.x, Rust and Python assumed a closed vocabulary and the
command line assumed an open one, so the same keys could get different answers.

```rust
// 4.x
let open = plan(&keys, Needs::default().exact())?;
let closed = plan(&keys, Needs::default())?;

// 5.0
let open = plan(&keys, Needs::default())?;
let closed = plan(&keys, Needs::default().closed_vocabulary())?;
```

```python
# 4.x
opened = lexindex.plan(keys, exact=True)
closed = lexindex.plan(keys)

# 5.0
opened = lexindex.plan(keys)
closed = lexindex.plan(keys, closed_vocabulary=True)
```

```console
$ lexindex plan keys.txt --exact                 # 4.x: the flag said what was already implied
$ lexindex plan keys.txt                         # 5.0
$ lexindex plan keys.txt --closed-vocabulary     # both: every key you will ask about is in this file
```

A program that took a 4.x bare `plan` to size a hash index gets a larger answer from the same call,
and no error: add `closed_vocabulary` if every key it will ask about is in `keys`.

## `Kind` has seven variants and may gain more

`plan` now prices `HashedDictIndex` and `DoubleArrayIndex`, so either can be the winner, and `Kind`
is `#[non_exhaustive]`: a `match` needs a wildcard arm, and that arm is where a kind this program
cannot build is said out loud.

```rust
// 4.x
let index = match plan(&keys, needs)?.best().kind {
    Kind::Compact => build_compact(&keys)?,
    Kind::Closed => build_closed(&keys)?,
    Kind::Perfect => build_perfect(&keys)?,
    Kind::String => build_string(&keys)?,
    Kind::Dict => build_dict(&keys)?,
};

// 5.0
let index = match plan(&keys, needs)?.best().kind {
    Kind::Compact => build_compact(&keys)?,
    Kind::Closed => build_closed(&keys)?,
    Kind::Perfect => build_perfect(&keys)?,
    Kind::String => build_string(&keys)?,
    Kind::Dict => build_dict(&keys)?,
    Kind::Hashed => build_hashed(&keys)?,
    Kind::DoubleArray => build_double_array(&keys)?,
    other => return Err(format!("this program cannot build {}", other.name()).into()),
};
```

`Kind::Hashed` is one row, at zero fingerprint bits and block 256, which is what `--index hashed`
builds. `Kind::DoubleArray` leaves the ranking on a corpus past what its 23-bit slots hold rather
than failing the plan. Python sees two more strings in `kind`. `lexindex build --index auto` can now
build either, so a script that read the blob back as one of five types should read `inspect` first.

A `HashedDictIndex` is priced exact at its dictionary's search, since at zero bits `id` is
`dict().id`, so an open vocabulary never ranks it above that dictionary. Under a closed one it is
priced at `id_unchecked`.

## `Needs` and `Estimate` are built by this crate

`#[non_exhaustive]` takes away struct literals and `..` updates outside the crate, and asks a pattern
for `..`. The builders are the way in, and reading a field is unchanged.

```rust
// 4.x
let needs = Needs { prefix: true, ..Needs::default() };
let Estimate { kind, bytes, block, measured } = plan.best();

// 5.0
let needs = Needs::default().prefix();
let Estimate { kind, bytes, block, measured, .. } = plan.best();
```

## A common-prefix walk asks for `common_prefix`

`DoubleArrayIndex` answers common-prefix and longest-prefix walks and enumerates no prefix, so one
`prefix` need would bar it from the one workload it leads. `Needs::common_prefix()`, `common_prefix=True`
and `--common-prefix` are new, and a `Workload` with `common_prefix` or `longest_prefix` weights now
implies that need rather than `prefix`.

```rust
// 4.x: `Plan::needs()` of such a workload read `prefix`
assert!(p.needs().prefix);

// 5.0
assert!(p.needs().common_prefix);
```

## What a plan tells you beside the winner

```rust
let p = plan(&keys, Needs::default().closed_vocabulary())?;
let best = p.best();
println!("{:?} at {:?} keys a microblock", best.block, best.micro);
println!("{} bytes held by a mapped load", best.resident_bytes);
println!("{:.1} s to build, on one machine's numbers", best.build_secs);
for row in p.pareto() {
    println!("{} {} bytes, {:.0} ns", row.kind.name(), row.bytes, p.nanos(&row));
}
```

```python
p = lexindex.plan(keys, closed_vocabulary=True)
p["best"]["micro"], p["best"]["resident_bytes"], p["best"]["build_secs"]
[(row["kind"], row["bytes"], row["nanos"]) for row in p["pareto"]]
```

`pareto` is the rows no other row beats on both `bytes` and the nanoseconds of this plan's objective,
smallest first; `best` is always on it. `build_secs` and the nanoseconds are one machine's numbers,
for ordering and never for predicting. The ladder `plan` prints marks the same rows with `+` and has
two more columns, so a script that parses it by column position should use the fields instead.

## `Overlay::remove` needs exact membership

Over `CompactHashIndex`, a key the index never held can match a live id, and removing it retired that
id and took a real key with it. That was documented; now it cannot be written. `remove` lives on
`Overlay<I: OverlayExact>`, which `StringIndex` and `PerfectHashIndex` implement. `retire_id` stays
on every base, and takes the id `add` handed back.

```rust
// 4.x
let mut ov = Overlay::new(CompactHashIndex::build(&keys, 4)?);
ov.add("typo");
ov.remove("typo");

// 5.0
let mut ov = Overlay::new(CompactHashIndex::build(&keys, 4)?);
let typo = ov.add("typo");
ov.retire_id(typo);
```

Python has no compile time, so the same call raises a `TypeError` that names the base:

```python
ov = lexindex.Overlay(lexindex.CompactHashIndex(keys, 4))
typo = ov.add("typo")
ov.remove("typo")     # TypeError: an overlay over a CompactHashIndex has no remove(): ...
ov.retire_id(typo)
```

A base id the program did not keep is `ov.id(key)`, which is the same question `remove` used to ask
and now has to be asked on purpose. An outside `OverlayBase` that has exact membership adds
`impl OverlayExact for MyBase {}` and sets `EXACT_MEMBERSHIP`; without the second, `remove` does not
build.

## A refused build is `IndexError::Build`

`Format` means a malformed blob and nothing else. A block out of range, a fingerprint width that
does not exist, more keys than an index numbers, a double array past its slots and a source that
failed part way through all answer `Build`, which is no longer behind the `mph` feature: the ordered
indexes refuse builds too.

```rust
// 4.x
match DictIndex::build_with_block(&keys, block) {
    Err(IndexError::Format(why)) => refuse(why),
    other => use_it(other?),
}

// 5.0
match DictIndex::build_with_block(&keys, block) {
    Err(IndexError::Build(why)) => refuse(why),
    other => use_it(other?),
}
```

`IndexError` is `#[non_exhaustive]`, so a `match` on it needs a wildcard arm, and a new error is no
longer a major version. The message starts
`build error:` rather than `format error:`. The C ABI answers `LEXINDEX_STATUS_BUILD` for these,
where it answered `LEXINDEX_STATUS_FORMAT`. Python raises `ValueError` for both, as it did.

## Keep the key list of a keyless index

`CompactHashIndex` and `ClosedHashIndex` store no keys, so `lexindex dump` has nothing to hand back
and says so, and a `DoubleArrayIndex` answers no `id -> key`. **A format change can only be crossed
from the key list these were built from**, so keep it wherever one of these is in production. Every
other index can give its keys back: run `lexindex dump old.blob > keys.txt` with the version that
wrote the blob, before upgrading, since a later version may refuse that blob by name.

## Which 4.x blobs 5.0 reads

The key hash changed, so a hash blob from 4.x would answer wrong ids. 5.0 refuses those blobs by
name, and the message gives the way across. The ordered indexes do not hash a key, so 5.0 reads
their 4.x blobs as they are.

| A blob 4.x wrote | 5.0 | The way across | Ids |
|---|---|---|---|
| `DictIndex` (`BDX3`, `BDX4`), `StringIndex` (`BIX4`), `DoubleArrayIndex` (`BDA1`) | reads it | none | the same |
| `Overlay` (`OVL2`) over one of those | reads it | none | the same |
| `HashedDictIndex` (`BHD1`) | refuses it | dump, then build | the same: they are ranks |
| `PerfectHashIndex` (`BMP8`) | refuses it | dump, then build | **new** |
| `CompactHashIndex` (`BCH8`), `ClosedHashIndex` (`BCL2`) | refuses it | build from your key list | **new** |
| `Overlay` (`OVL2`) over a hash index | refuses it, with its base's message | as its base | as its base |

Run the dump with the version that wrote the blob, since 5.0 refuses to read it:

```console
$ uvx --from 'lexindex<5' lexindex dump old.blob > keys.txt
$ lexindex build keys.txt new.blob --index perfect     # or --index hashed
```

The dump of an overlay holds the keys it added, so the new index holds them too.

`--index hashed` builds at block 256 with no fingerprint. A `HashedDictIndex` with another block or
width goes through `HashedDictIndex.from_dict(dictionary, bits)`, over a `DictIndex` built from the
same `keys.txt`.

A `PerfectHashIndex` gives every key a new id. The dump is in old-id order, so a line's number is
the key's old id, and a program that stored ids can build its map from that:

```python
new = lexindex.PerfectHashIndex.load("new.blob")
new_id = [new.id(line.rstrip("\n")) for line in open("keys.txt", encoding="utf-8", newline="\n")]
# new_id[old] is what `old` is now
```

`CompactHashIndex` and `ClosedHashIndex` hold no keys, so nothing can dump them. To keep stored ids
across, ask both versions about your key list: 4.x answers each key's old id, 5.0 its new one.

A `DictIndex` built by 5.0 can be smaller than the same keys' 4.x blob, and its ids are the same, so
a rebuild is optional.
