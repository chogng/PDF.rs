# Scope

`core/document` composes one already parsed traditional `XrefSection` in two explicit trust
states. `CandidateRevisionIndex` derives unauthenticated physical intervals. `AttestRevisionJob`
then consumes that candidate, validates a supported PDF header at source offset zero, frames every
in-use object in physical order, and scans the prefix and every gap through `startxref` for only
PDF whitespace and terminated comments. Only complete success publishes `AttestedRevisionIndex`.
That sealed typestate is the only public factory for bounded jobs that reparse one exact object or
iteratively follow a top-level direct-reference chain while preserving reproduced evidence.

The crate performs no file, network, data callback, async-runtime, stream decoding, or reference
graph traversal or caching. Its first resolver slice follows only whole-object reference aliases;
array, dictionary, and stream semantics remain leaf values. All source access is synchronous
polling through an injected `ByteSource`; all long-running CPU work uses an injected cooperative
cancellation probe.

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
  `AttestedRevisionIndex`. The index also retains the exact validated object and syntax profiles
  used for initial framing. No partial attested type exists.

## Proof-preserving object access

- `AttestedRevisionIndex::open_object` is the only public factory for
  `OpenAttestedObjectJob`. Exact missing, free, and generation-mismatch lookup runs before job
  configuration checks. The job copies one non-cloneable evidence record through a crate-private
  operation and privately constructs the raw target and lower object job; none are exposed.
- Before minting the child, the factory rechecks index and evidence revision, reference, xref
  offset, upper bound, source length, `startxref`, object/header/endobj containment, and stream
  data/endstream ordering. It then reuses the object and syntax profiles retained at initial
  attestation and applies the caller's explicit validated `ObjectWorkCaps`.
- Those caps bound one access job only. They are not a resolver-, document-, cache-, or session-wide
  aggregate and do not authorize unbounded repeated reopening; the future resolver and host
  scheduler must lend and account their own cumulative budgets across jobs.
- `Pending` forwards only the exact checkpoint corresponding to the lower child's current envelope
  or stream-boundary phase. Lower first-request charging and stats remain authoritative, so repeated
  polls of one pending range do not charge twice.
- Before `Ready`, the wrapper probes the full source snapshot and then cancellation, reconstructs
  evidence with the owning index's revision identity, and requires exact equality with the retained
  record plus the same snapshot and `startxref`. Only then does it publish `AttestedObject`, which
  owns the parsed lower object and exposes its value only by shared borrow while keeping evidence
  in the wrapper. There is no API that consumes the wrapper to return an owned
  `IndirectObjectValue` or lower `IndirectObject`, and there is no raw-target, lower-job, deref, or
  into-inner path. Callers can still copy or clone semantic subvalues where lower public types
  explicitly allow it; those values do not mint raw-target, object-job, or resolver authority.
- Source, resource, and cancellation failures retain the complete lower `ObjectError` and its
  stable policy. Any syntax, unsupported, configuration, or internal failure while reopening bytes
  already proven under the retained profile is an `AttestedObjectEvidenceMismatch` and also retains
  the lower error. Terminal failure is replayed exactly; completed jobs reject another poll.
- Proof preservation depends on the injected `ByteSource` honoring its immutable-snapshot contract.
  The job checks the complete `SourceSnapshot` before and after lower framing and lower slices check
  source identity, but Core does not cryptographically detect a deliberately non-conforming source
  that fabricates different bytes while claiming the same identity, revision, validator, and
  length. Such behavior is a host capability violation, not accepted alternate document input.

## Bounded top-level reference chains

- `AttestedRevisionIndex::resolve_reference_chain` is the only public factory for the one-shot
  chain job. The job borrows its attested index, starts in `Unresolved`, and uses only the index's
  proof-preserving object-access factory. It never constructs or exposes a raw target or lower
  object job.
- Resolution is deliberately narrow and iterative. When an opened object's entire direct value is
  one `SyntaxObject::Reference`, the job drops that intermediate parsed object and follows the
  exact next reference. Every other direct value and every stream is terminal. References nested
  in arrays, dictionaries, or stream dictionaries are not visited by this profile.
- The active unique prefix is reserved fallibly before the job is published. Actual allocator
  capacity is charged against an explicit retained-path byte limit. The terminal reference remains
  scalar, so a cycle-closing or rejected reference can be retained without allocating on the
  failure path.
- Each traversed edge is charged before continuation. Exact `ObjectRef` membership in the bounded
  active prefix detects self and multi-object cycles; `ReferenceCycle` retains the complete prefix
  and repeated closing reference. Missing, free, and generation-mismatch targets retain the full
  attempted chain together with the original document lookup failure.
- The job exposes `Unresolved`, `Resolving`, `Ready`, and `Failed` phases. A successful result moves
  the retained chain and terminal `AttestedObject` out together. A failure owns its move-only chain
  inside the terminal job state and is returned by shared borrow, so repeated polls replay the same
  error without `Arc`, `Box`, clone, or another allocation.

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
- A reference-chain job independently bounds object starts, reference edges, unique depth,
  allocator-reported retained path capacity, and aggregate child reads and parses. Each child cap
  is the smaller of the retained per-object profile and the job's remaining aggregate allowance.
  Child stats deltas are charged exactly once across `Pending` polls; scoped cap exhaustion maps to
  the job-wide limit while retaining the complete lower `ObjectError`.
- Reference-chain limits apply to one job only. Repeated jobs do not share a budget owner, resident
  cache, or reservation ledger. A future persistent resolver must add complete retained-object
  accounting plus cross-job/session ownership before it can cache `Ready` values or coalesce work.
- A scan request is charged before `ByteSource::poll`. `Pending` preserves its exact range,
  checkpoint, lexical state, and charged flag, so re-polling does not charge again. Source snapshot
  equality is checked before cancellation on every active loop. Scanning probes cancellation after
  at most 256 bytes and publication probes once more.
- Attestation and access terminal failures store and replay the same copyable, source-redacted
  error. Reference-chain failure stores one move-only error and lends the same stable reference on
  every poll. Lower `ObjectError`, `SourceError`, and `SyntaxError` values remain available without
  retaining source content. Completed one-shot jobs return a stable `JobAlreadyComplete` error if
  polled again.

# Trust boundary

`CandidateRevisionIndex`, its intervals, and its crate-private raw targets remain untrusted.
`AttestedRevisionIndex` is privately constructed and exposes neither the candidate, a raw
`IndirectObjectTarget`, nor an `OpenObjectJob`. The only document-layer proof-bearing access path
is the private attested-index-to-access-job-to-`AttestedObject` chain. Borrowing or cloning a lower
semantic subvalue is not equivalent to obtaining that proof or a resolver capability. The index
proves only this strict top-level physical decomposition for the bound immutable snapshot and
retained parser profile.

The bounded chain job is a consumer of that proof, not an authority expansion. Its returned
terminal object was reopened through the same retained profile, and its chain records identities
only. It does not publish a mutable object graph, cache entry, raw byte capability, or reusable
child-job constructor.

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
Access geometry unit tests reject individual index/evidence field mismatches, invalid object span
containment, and invalid stream span order. Reference-chain tests cover terminal roots, multi-hop
aliases, nested-reference non-traversal, self and multi-object cycles, lookup failures, exact and
one-less limits, pending idempotence, source and cancellation priority, stable terminal replay, and
redacted diagnostics. Repository policy checks the sibling dependency allowlist and absence of
platform/external-engine APIs.

# Known deviations and unsupported cases

- Only one strict traditional base revision is accepted. Revision chains, `/Prev`, xref streams,
  hybrid references, object streams, and revision precedence remain unsupported.
- Attestation eagerly frames every in-use object. The access job can reparse one proven value, and
  the chain job can follow top-level whole-object aliases with cycle detection. They are not a lazy
  document model or complete resolver: nested semantic references, persistent dependency states,
  concurrent work coalescing, retained-value caching, and cross-job aggregate ownership remain
  unsupported.
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
- 2026-07-13: Added proof-preserving object access minted only by the attested index, retained
  parser profiles, explicit per-access work caps, exact evidence reproduction, and redacted
  move-only value wrappers without exposing raw targets or lower jobs.
- 2026-07-13: Added an iterative attested top-level reference-chain job with exact cycle chains,
  fallibly retained paths, and job-wide object, edge, depth, read, and parse budgets.
