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
safe fns: `from_bytes` and `load` on all three indexes, and `Overlay`'s. No input produces undefined
behaviour, an out-of-range id, or a read outside the blob. That is what the in-crate minimal perfect
hash bought — every array length is derived on load from the header's own scalars, so a loader cannot
be handed a length that disagrees with the table it describes.

**Refusal is an `Err` on the perfect-hash side, and can be a panic on the ordered one.** Arbitrary
bytes handed to `CompactHashIndex`, `PerfectHashIndex` or `Overlay` come back as an `IndexError`,
whatever they contain. `StringIndex` is backed by [`fst`](https://docs.rs/fst), whose node decoder is
safe Rust but not total, and the checksum in front of it is public and recomputable — so a blob
crafted to carry a matching one reaches that decoder with an invalid body and can **panic** instead
of returning. That is measured rather than feared: a libFuzzer target over `from_bytes` produced 44
such bytes in ten minutes, and they panic inside the rank spot-check the load itself runs. A panic is
neither undefined behaviour nor an out-of-bounds read — it unwinds, or aborts the process under
`panic = "abort"` — but it is a denial of service for anything that loads ordered blobs supplied by
a stranger. Load `StringIndex` blobs you produced yourself.

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
target, and libFuzzer over the four parsers a target can hold to a return value: `BCH6`, `BMP5`,
`OVL2`, and the standalone `MPH1` from inside. `BIX4` has none, because the panic above is the
accepted answer there and a target that re-finds it every week would only teach us to ignore a red
job.

## Not vulnerabilities

- **A crafted blob that answers wrong ids.** Stated above; it is the documented contract.
- **A crafted blob that fails to load.** The perfect-hash and overlay loaders refuse with an `Err`
  on any input. A crafted `StringIndex` blob may panic instead: documented above, bounded to a panic
  by `fst` being safe Rust, and avoided by not loading ordered blobs from strangers.
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
