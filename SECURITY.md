# Security policy

lexindex builds compact, immutable string ↔ id indexes and persists them as single-file blobs. This
document says what the library defends against, what it deliberately does not, and how to report
something that crosses the line.

## Supported versions

| Version | Supported |
|---|---|
| 1.0.x | yes |
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
safe fns: `from_bytes` and `load` on all three indexes, and `Overlay`'s. Arbitrary bytes produce an
`Err`, never undefined behaviour, an out-of-range id, or a read outside the blob. That is what the
in-crate minimal perfect hash bought — every array length is derived on load from the header's own
scalars, so a loader cannot be handed a length that disagrees with the table it describes.

The guarantee is **soundness, not correctness**. A blob crafted by someone else can answer *wrong*
ids for keys it does not hold. It cannot answer ids outside `[0, n)`, allocate from a number it
merely claims, or index past a section. If you serve queries against a blob supplied by an untrusted
party, treat its answers as untrusted; the process is safe either way.

**`load_mmap` is the one `unsafe fn`, and its obligation is about the file, not the bytes.** The
index borrows the mapped pages, so another process writing that file while it is mapped is undefined
behaviour and nothing in the library can check for it. Use `load` if the file is not yours alone.
`load_mmap` also skips the payload checksum by design — verifying it would read every page and
defeat the point.

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

**`unsafe` is confined to memory mapping and one prefetch.** The whole crate contains three
`unsafe fn`s — `load_mmap`, once per index — and five `unsafe` blocks: four memory maps, counting the
writable one `build_to_file` uses on a temporary file it created itself, and a cache prefetch that is
bounds-checked before it runs. `unsafe_op_in_unsafe_fn` is denied, so every one names its own
justification. Miri and AddressSanitizer run weekly over the byte-range code, Miri also on a 32-bit
target, and libFuzzer over all four blob parsers.

## Not vulnerabilities

- **A crafted blob that answers wrong ids.** Stated above; it is the documented contract.
- **A crafted blob that fails to load.** Every rejection path returns `Err`.
- **`CompactHashIndex` reporting a key it never held.** Its membership is probabilistic by
  construction, at the false-positive rate `fingerprint_bits` buys.
- **`Overlay::remove` retiring an id over a probabilistic base.** Removal is by id, and a false
  positive can supply one; the method documents it.
- **Hash collisions found offline.** See above.
- **Memory exhaustion building a genuinely huge index.** Allocation is proportional to the key set
  you hand in, not to a number a blob claims.

## Where the details are

- [`docs/design.md`](docs/design.md) — what each blob contains, what is validated on load, and the
  compatibility policy.
- [`docs/usage.md`](docs/usage.md) — the loader contracts as a caller meets them.
