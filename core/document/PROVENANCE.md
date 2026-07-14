# Scope

`core/document` composes one already parsed traditional `XrefSection` in two explicit trust
states. `CandidateRevisionIndex` derives unauthenticated physical intervals. `AttestRevisionJob`
then consumes that candidate, validates a supported PDF header at source offset zero, frames every
in-use object in physical order, and scans the prefix and every gap through `startxref` for only
PDF whitespace and terminated comments. Only complete success publishes `AttestedRevisionIndex`.

The crate performs no file, network, data callback, async-runtime, stream decoding, or reference
resolution work. All source access is synchronous polling through an injected `ByteSource`; all
long-running CPU work uses an injected cooperative cancellation probe.

# Semantic owner

Parser/Security owns revision composition, physical object indexing, and top-level attestation.
`core/xref` owns traditional xref parsing, `core/syntax` owns the supported header and direct-object
grammar, `core/object` owns bounded indirect-object framing, and `core/bytes` owns immutable
snapshot-bound exact reads. This crate composes those sibling results without moving their lower
semantic responsibilities.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires one-way core dependencies, revision identity, reverse xref composition, and validation
  of offset, generation, object number, and object header before use.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires stable
  structured errors, checked arithmetic, fallible bounded allocation, and redacted diagnostics.
- [RPE-STD-002, sections 6-7](../../docs/standards/lifecycle-and-concurrency.md) requires cooperative
  cancellation and stable terminal state for resumable core jobs.
- [RPE-STD-005, sections 4-9](../../docs/standards/security-and-resource-budget.md) requires
  deterministic entry, memory, scan, and child-work budgets before processing untrusted structure.

This slice does not claim an ISO 32000 conformance profile or R0 resolver coverage.

# Algorithms and derivations

## Candidate physical index

- Construction validates total and in-use row ceilings before retaining records. Conservative
  fixed charges cover allocator-reported logical-row and physical-interval capacity; both vectors
  use `try_reserve_exact`, then validate actual capacity.
- In-use candidates are sorted by absolute offset with an in-repository heapsort. Comparisons and
  swaps consume `max_sort_steps`; cancellation is probed initially, after at most 256 sort steps,
  at completion, and at equivalent intervals in all other long loops.
- Every in-use offset must be below `startxref`, duplicate offsets are rejected, the next larger
  in-use offset bounds each candidate, and `startxref` bounds the last. Exact logical lookup keeps
  missing, free, and generation-mismatch outcomes distinct.

## Top-level attestation

- `AttestRevisionJob` consumes, rather than borrows, a candidate. Its scan, object-envelope, and
  object-boundary checkpoints must be pairwise distinct. A known immutable source length, object
  count, parser-profile consistency, and retained-evidence reservation are checked before polling.
- The first exact prefix request validates the existing syntax crate's supported eight-byte
  `%PDF-x.y` header at absolute offset zero and independently requires byte 8 to be CR or LF. The
  eight header bytes count toward exact prefix-read work but are not classified as a comment and do
  not count toward trivia-scan or comment work.
- After the header, the scanner accepts only the six PDF whitespace bytes (`NUL`, HT, LF, FF, CR,
  and space) or `%` comments. A comment ends only when an actual CR or LF is consumed. Comment
  state, start offset, length, and first-request charging survive chunk and `Pending` boundaries.
  Reaching an object offset or `startxref` while still inside a comment is a stable syntax failure.
- Objects are authenticated only in increasing physical-offset order. Before object `i` begins,
  the complete prefix through its xref offset has an accepted decomposition of supported header,
  top-level trivia, and every earlier exact object frame plus top-level trivia. `OpenObjectJob`
  validates the exact number, generation, `obj` header, direct value or directly sized stream, and
  terminal `endobj` without crossing the next candidate or `startxref` bound.
- After each object is ready, its source/ref/offset/bounds and terminal spans are defensively
  rechecked. A fixed-size, non-cloneable `ObjectAttestation` records only spans and a scalar syntax
  kind and is exposed only by reference from its snapshot-owning index. Stream evidence retains
  only the opaque payload and `endstream` spans; no parsed value or source bytes remain. Gaps are
  scanned from exact `object_span.end_exclusive()` to the next physical boundary.
- The last gap is the tail through `startxref`. A final cancellation probe precedes the single
  transition that moves the candidate, supported header, and complete evidence vector into
  `AttestedRevisionIndex`. No partial attested type exists.

# Resource accounting and resumability

- Revision-attestation limits independently bound source length, object count, exact scan chunk,
  cumulative prefix/gap reads, one comment, aggregate child reads, aggregate child parses, and
  retained evidence. Evidence is prechecked, reserved fallibly, charged by actual allocator
  capacity with a conservative per-record constant, and guarded against `size_of` growth.
- Every child receives `ObjectWorkCaps` equal to the smaller of its configured per-object total and
  the parent's remaining aggregate budget. Object stats deltas are accumulated once per poll.
  When the scoped cap is strictly smaller than the per-object cap, exhaustion maps to the parent
  aggregate limit while retaining the complete lower `ObjectError`; an equal cap remains a lower
  per-object failure.
- A scan request is charged before `ByteSource::poll`. `Pending` preserves its exact range,
  checkpoint, lexical state, and charged flag, so re-polling does not charge again. Source snapshot
  equality is checked before cancellation on every active loop. Scanning probes cancellation after
  at most 256 bytes and publication probes once more.
- Terminal failure stores and replays the same copyable, source-redacted error. Lower
  `ObjectError`, `SourceError`, and `SyntaxError` values remain available without retaining source
  content. A completed one-shot job returns a stable `JobAlreadyComplete` error if polled again.

# Trust boundary

`CandidateRevisionIndex`, its intervals, and its crate-private raw targets remain untrusted.
`AttestedRevisionIndex` is privately constructed and exposes neither the candidate, a raw
`IndirectObjectTarget`, nor an `OpenObjectJob`. It proves only this strict top-level physical
decomposition for the bound immutable snapshot and current parser profile.

The proof is deliberately closed-world: bytes between indexed in-use objects must be trivia.
Stale free-object bodies, unindexed top-level objects, or other top-level tokens are rejected even
when another PDF implementation might tolerate them. Opaque stream payload bytes need not be
resident and are not decoded or semantically validated; their checked direct length and terminal
framing only establish lexical extent.

# External observations

No PDFium, other PDF engine, third-party implementation source, or external output was used to
derive the candidate index or attestation state machine.

# Dependencies and generated data

The only dependencies are the in-repository `pdf-rs-bytes`, `pdf-rs-syntax`, `pdf-rs-xref`, and
`pdf-rs-object` crates. There are no development dependencies, generated tables, platform I/O
APIs, external PDF engines, or async runtimes.

# Tests

Candidate tests cover physical sort ordering, exact sort-budget exhaustion, the 256-step
cancellation ceiling, checked conservative accounting, exact logical lookup outcomes, duplicate
and out-of-revision offsets, and trailer-root policy. Attestation unit tests guard fixed evidence
size accounting, the exact six-byte whitespace set, eight-byte header parser minima, and aggregate
versus per-object cap ties. Public behavior tests cover successful direct/stream kind evidence,
header and gap policy, comment state across chunks, embedded candidate offsets, tail closure,
pending idempotence, cancellation, snapshot mismatch, stable failure replay, and limit boundaries.
Repository policy checks the sibling dependency allowlist and absence of platform/external-engine
APIs.

# Known deviations and unsupported cases

- Only one strict traditional base revision is accepted. Revision chains, `/Prev`, xref streams,
  hybrid references, object streams, and revision precedence remain unsupported.
- Attestation eagerly frames every in-use object. It is not the lazy document model or a complete
  resolver; it does not validate reference targets, detect reference cycles, or retain object
  values for later semantic access.
- Indirect stream `/Length`, repair, encrypted object interpretation, filters, decoded stream
  payloads, catalog/page services, writer behavior, and document actions remain unsupported.
- The closed-world trivia-gap policy is intentionally stricter than general PDF compatibility and
  is not an ISO or R0 conformance claim.
- Hard ceilings and defaults are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.

# History

- 2026-07-13: Added candidate-only single-revision physical indexing, bounded cancellable sort,
  exact lookup errors, and crate-private five-field object-target construction.
- 2026-07-13: Added resumable physical-order top-level attestation, strict header/trivia closure,
  aggregate child work caps, fixed-size retained evidence, and atomic attested typestate.
