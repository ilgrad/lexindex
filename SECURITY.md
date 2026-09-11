# Security policy

lexindex builds compact, immutable string ↔ id indexes and persists them as single-file blobs. This
document says what the library defends against, what it deliberately does not, and how to report
something that crosses the line.

## Supported versions

| Version | Supported |
|---|---|
| 2.0.x | yes |
| 1.x | no — 2.0 refuses the hash blobs 1.x wrote (the key hash changed), so the fix is to upgrade and rebuild them; `BIX4` loads as it is, and so does an `OVL2` over a `BIX4` base; an `OVL2` over a 1.x hash base is refused with that base |
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
ranks and its keys are UTF-8 — and catches the decoder's panic at the load boundary, returning it as
an `IndexError`. It cannot *prevent* the panic — checking a node means decoding it, and `fst`'s
decoder is the only one there is — so under `panic = "abort"` a crafted blob still aborts, and the
rejection runs the process-wide panic hook on its way out. `from_bytes` stays the loader for blobs
you produced yourself.

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

**`unsafe` is confined to memory mapping and one prefetch.** The whole crate contains seven
`unsafe fn`s — `load_mmap` and `load_mmap_verified` on every index, `load_mmap_untrusted` on
`StringIndex` — and nine `unsafe` blocks: eight memory maps, counting the writable one
`build_to_file` uses on a temporary file it created itself, and a cache prefetch that is
bounds-checked before it runs. `unsafe_op_in_unsafe_fn` is denied, so every one names its own
justification. Miri and AddressSanitizer run weekly over the byte-range code, Miri also on a 32-bit
target, and libFuzzer over the six parsers a target can hold to a return value: `BCH7`, `BMP7`,
`BCL1`, `BDX1` — loaded and then queried, since its block data is bounds-checked on the read
rather than at load — `OVL2`/`OVL1`, and the standalone `MPH2`/`MPH1` from inside, each run
starting from the
corpus the last one left. `BIX4` is
fuzzed through `from_untrusted_bytes`, which holds it to a return value too — twice: on its own,
and as the base region of an overlay, which is the composition a caller loading a stranger's
overlay runs; a target over `from_bytes` would only re-find the panic above
every week and teach us to ignore a red job.

## Not vulnerabilities

- **A crafted blob that answers wrong ids.** Stated above; it is the documented contract.
- **A crafted blob that fails to load.** The perfect-hash and overlay loaders refuse with an `Err`
  on any input. A crafted `StringIndex` blob may panic `from_bytes` instead: documented above,
  bounded to a panic by `fst` being safe Rust, and answered by `from_untrusted_bytes`.
- **`CompactHashIndex` reporting a key it never held.** Its membership is probabilistic by
  construction, at the false-positive rate `fingerprint_bits` buys.
- **`Overlay::remove` retiring an id over a probabilistic base, and `Overlay::add` answering a
  stranger's id there.** Removal is by id, and a false positive can supply one; both methods
  document it, and the base alone would have answered the same.
- **Hash collisions found offline.** See above.
- **Memory exhaustion building a genuinely huge index.** Allocation is proportional to the key set
  you hand in, not to a number a blob claims.

## Where the details are

- [`docs/design.md`](docs/design.md) — what each blob contains, what is validated on load, and the
  compatibility policy.
- [`docs/usage.md`](docs/usage.md) — the loader contracts as a caller meets them.
