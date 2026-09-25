# Security policy

lexindex builds compact, immutable string ↔ id indexes and persists them as single-file blobs. This
document says what the library defends against, what it deliberately does not, and how to report
something that crosses the line.

## Supported versions

| Version | Supported |
|---|---|
| 4.3.x | yes |
| 4.2.x | no — 4.3 reads every blob 4.2 wrote; where a character code pays it writes `BDX4`, which 4.2 refuses, so upgrade the readers before the writers |
| 4.1.x | no — 4.2 is a drop-in upgrade: it adds `StringIndex.occurrences` and reads every blob 4.1 wrote |
| 4.0.x | no — 4.1 is a drop-in upgrade: it adds `BHD1` and reads every blob 4.0 wrote |
| 3.0.x | no, and the upgrade is the least drop-in one so far: 4.0 computes a different key hash, so it refuses `BMP7`, `BCH7` and `BCL1` by name, and the `BDX2` dictionary with them. Four of the five indexes have to be rebuilt from their keys, and where those keys come from differs: `BMP7` and `BDX2` store theirs, so a 3.x process writes them out over `key(id)` before the upgrade, while `BCH7` and `BCL1` store no keys in any version and need the corpus they were built from. `lexindex dump` makes that a one-liner, but it is 4.0's subcommand — 3.0.0 shipped `plan`, `build` and `inspect` only. `BIX4` loads unchanged, and so does an `OVL2` over a `BIX4` base. [`docs/migration-4.md`](docs/migration-4.md) is the worked path |
| 2.1.x | no — everything in the row above, and the `BDX1` dictionary 2.1 wrote on top of it |
| 2.0.x | no — the same as 2.1 |
| 1.x | no — 2.0 already refused the hash blobs 1.x wrote, for the same reason 4.0 refuses 3.x's; `BIX4` loads as it is, and so does an `OVL2` over a `BIX4` base, but an `OVL2` over any hash base is refused with that base |
| 0.x | no — its blob formats are refused by 1.0 anyway, and the fix is to rebuild |

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on
[github.com/ilgrad/lexindex](https://github.com/ilgrad/lexindex/security/advisories/new) — please do
not open a public issue first. If that form is unavailable, open an issue asking for a private
channel and nothing else — no details. A report is most useful with the blob or key set that
reproduces it, the version, and the target (pointer width matters here). Expect an acknowledgement
within a week; this is a single-maintainer project, so that is a realistic figure rather than an SLA.

## Threat model

**A blob is data you own.** Every loader takes bytes it did not write, and since 1.0 all of them are
safe fns: `from_bytes` and `load` on every index, and `Overlay`'s. No input produces undefined
behaviour, an out-of-range id, or a read outside the blob. That is what the in-crate minimal perfect
hash bought — every array length is derived on load from the header's own scalars, so a loader cannot
be handed a length that disagrees with the table it describes.

**Refusal is an `Err` on the perfect-hash side, and can be a panic on the ordered one.** Arbitrary
bytes handed to `CompactHashIndex` or `PerfectHashIndex` come back as an `IndexError`, whatever they
contain, and so does an `Overlay`'s own framing — but an overlay hands its embedded base region to
the base's loader, so an overlay over a `StringIndex` inherits the exception below unless it is
loaded through `from_untrusted_bytes` (in Python) or `from_bytes_with(..,
StringIndex::from_untrusted_bytes)` (in Rust). `StringIndex` is backed by
[`fst`](https://docs.rs/fst), whose node decoder is safe Rust but not total, and the checksum in
front of it is public and recomputable — so a blob crafted to carry a matching one reaches that
decoder with an invalid body and can **panic** instead of returning. That is measured rather than
feared: a libFuzzer target over `from_bytes` produced such bytes in minutes, and the 111-byte
specimen it could not shrink further is committed as `tests/data/panicking-1.0.0-string.bix`, with a
test that it still panics. A panic is neither undefined behaviour nor an out-of-bounds read — it
unwinds, or aborts the process under `panic = "abort"` — but it is a denial of service for anything
that loads ordered blobs supplied by a stranger. For those, use `StringIndex::from_untrusted_bytes`:
it checks the transducer as a graph before answering — every reachable node once, in time
proportional to nodes and transitions rather than to the keys they spell, so that its values are
ranks and its keys are UTF-8 — and it measures every node *before* `fst` decodes it. The root
address the footer names and every transition target the walk reaches are checked against the blob
first, from the fields that decide a node's size: one claiming more bytes than sit below it, or a
transition whose delta would be subtracted past the blob's start, is refused before `fst` is handed
the address. The refusal is an `IndexError`, not a caught panic, and it stays one under
`panic = "abort"`, where there is nothing to catch with. A `catch_unwind` remains around the walk
as a backstop for a decode this crate has not modelled; should it fire the caller still gets an
`Err`, but the process-wide panic hook runs on the way out. The evidence is a libFuzzer run of the
`parse_string` target built with `panic = "abort"`, so that any panic still reached is a crash
rather than a caught error: 3.8 billion executions over 24 CPU-hours found none. `from_bytes` stays
the loader for blobs you produced yourself.

The guarantee is **soundness, not correctness**. A blob crafted by someone else can answer *wrong*
ids for keys it does not hold. It cannot answer ids outside `[0, n)`, allocate from a number it
merely claims, or index past a section. If you serve queries against a blob supplied by an untrusted
party, treat its answers as untrusted; the process is safe either way.

**The `load_mmap` family are the `unsafe fn`s, and their obligation is about the file, not the
bytes.** The index borrows the mapped pages, so another process writing that file while it is mapped
is undefined behaviour and nothing in the library can check for it. Use `load` if the file is not
yours alone. `load_mmap` also skips the payload checksum by design — verifying it reads every page.
`load_mmap_verified` does exactly that, once at load, for a file carried by someone else, and
`load_mmap_untrusted` runs `StringIndex`'s full validation over the mapping — both under the same
obligation, which weighs more for a stranger's file, since the check trusts what it saw once: map a
copy you own.

**The checksums are integrity, not authentication.** Both the header check and the payload hash are
public, deterministic and unkeyed, so anyone can recompute them after editing a blob. They catch a
flipped bit in transit or on disk; they say nothing about who wrote the file. The structural checks
past them — that side-table ids are exactly the tail range, that every remap entry lands in the
image, that overlay tombstones fall inside the id space — are what stand up to a blob that was
deliberately re-checksummed.

**The hashes are unseeded and deterministic**, because that is what makes a serialised index
reloadable. Someone who chooses the keys or the queries can therefore search offline for collisions
and for `CompactHashIndex` false positives. Neither is a memory-safety problem — a hash collision
costs a side-table probe, and construction cannot be made to fail by one — but lexindex is not a
HashDoS defence and must not be used as one. If your keys come from an adversary and lookup latency
is a resource you are protecting, put a keyed hash in front.

**`unsafe` is mapping, page allocation, reads whose bound is an invariant rather than a check,
and a call into code built for an instruction the CPU was checked for.** The library contains
twenty-one `unsafe fn`s. Twelve carry an obligation a caller outside the crate has to meet:
`load_mmap` and `load_mmap_verified` on the five indexes that have anything
to map (`ClosedHashIndex` is the perfect hash and nothing else), `load_mmap_untrusted` on
`StringIndex` and `load_mmap` on `DoubleArrayIndex`. The other nine are internal and their caller
is this crate — `hash::r4` and `hash::r8`, the key hash's unchecked loads; `room::commit`, which
extends a `Vec` over bytes just written into its spare capacity; `Pages::assume_init`; the double
array's `decode` and `slot`, a character of a `&str` and a slot read unchecked; and three builds
for an instruction the x86-64 baseline lacks, whose one obligation is that the CPU has it — the
perfect hash's `Remap::get_popcnt`, its remap lookup with `popcnt`, and the double array's
`place_avx2` and `slots_malformed_avx2`, its build's placement and its load's slot check with AVX2.
There are sixty-two `unsafe` blocks:
thirteen memory maps, counting the writable one `build_to_file` uses on
a temporary file it created itself; seven in the huge-page allocator; six that write into a `Vec`'s
spare capacity and then extend it; five unchecked loads inside the key hash, where the index is in bounds by the length
class that chose the load; four in `SharedBytes`, two of them cache prefetches; eleven
`get_unchecked` or `assume_init` reads across the perfect hash, the dictionary's stair and the
phrase trie a `BDX3` build walks — four of them in the perfect hash's remap, past a check that the
entry is below its length, on tables whose lengths and samples the loader has checked; one
`String::from_utf8_unchecked` over what the character
code of a `BDX4` blob decodes, which is whole characters out of a table built from `char`s
whatever the blob holds — a debug build checks it, and so the fuzz targets do; three calls into
those builds, on x86-64 only and each only once `is_x86_feature_detected!` has found its
instruction — every other target and Miri run the portable build, and tests hold each pair to
the same answers; and twelve in the double array's walks, which read a slot, the code table
and a character of the text unchecked, and
decode a text into buffers on the stack. A slot's bound is what the load's walk over every slot
settles — no row runs past the array, no id past the keys — and a debug build checks every slot
read against the array's length, so the fuzz targets do that too. Four `unsafe impl`s make
`SharedBytes` and `Pages` `Send` and `Sync` — the first reads, through its pointer, bytes that an immutable buffer or a read-only map
owns; the second owns its table outright, as a `Vec` does — and `pages::Zeroed` is an
`unsafe trait`, implemented for the four unsigned integers, whose all-zero bit pattern is a value.
The `python` feature adds twelve more blocks, each a one-line call from a `load_mmap*` binding
into the `unsafe fn` of the same name, forwarding the same obligation to the Python caller. The
`capi` feature adds the eleven `unsafe extern "C"` functions that take a pointer, five internal
`unsafe fn`s that read those pointers, and twenty-two `unsafe` blocks under them, each reading or
writing memory the C caller vouched for in the function's `# Safety` line — the header carries it
verbatim — after a null check on every function that has a status to report one in.
`unsafe_op_in_unsafe_fn` is denied, so every one names its own justification. Miri and
AddressSanitizer run weekly over the byte-range code and the C ABI, Miri also on a 32-bit
target, and libFuzzer daily over the eight parsers a target can hold to a return value: `BCH8`,
`BMP8`, `BCL2`, `BDX3`, `BDX4`, `BHD1` and `BDA1` — loaded and then queried, since a dictionary's
block data is bounds-checked and a sidecar's rank clamped on the read rather than at load — `OVL2`/`OVL1`, and
the standalone `MPH3`/`MPH2`/`MPH1` from inside, each run
starting from the
corpus the last one left. `inspect` has a target of its own, since it reads a header of *any*
of those formats and is the one entry point that is not a parser for a single one. `BIX4` is
fuzzed through `from_untrusted_bytes`, which holds it to a return value too — twice: on its own,
and as the base region of an overlay, which is the composition a caller loading a stranger's
overlay runs; a target over `from_bytes` would only re-find the panic above
every week and teach us to ignore a red job. A pull request that touches the code meets the same
targets before it is merged: ClusterFuzzLite, OSS-Fuzz's tooling on this repository's runners,
fuzzes it for ten minutes across all of them, starting from the seeds.

## Not vulnerabilities

- **A crafted blob that answers wrong ids.** Stated above; it is the documented contract.
- **A crafted blob that fails to load.** The perfect-hash and overlay loaders refuse with an `Err`
  on any input. A crafted `StringIndex` blob may panic `from_bytes` instead: documented above,
  bounded to a panic by `fst` being safe Rust, and answered by `from_untrusted_bytes`.
- **`CompactHashIndex` reporting a key it never held.** Its membership is probabilistic by
  construction, at the false-positive rate `fingerprint_bits` buys.
- **`Overlay::remove` retiring an id over a probabilistic base, and `Overlay::add` answering a
  stranger's id there.** Removal is by id, and a false positive can supply one; both methods
  document it, and the base alone would have answered the same. `Overlay::retire_id` takes the id
  and no key, so a caller that keeps the ids it was issued has a removal nothing can mislead.
- **Hash collisions found offline.** See above.
- **Memory exhaustion building a genuinely huge index.** Allocation is proportional to the key set
  you hand in, not to a number a blob claims.

## Where the details are

- [`docs/design.md`](docs/design.md) — what each blob contains, what is validated on load, and the
  compatibility policy.
- [`docs/usage.md`](docs/usage.md) — the loader contracts as a caller meets them.
