# Scope

`core/object` is the strict single-indirect-object framing bootstrap for the Native product core.
It validates one xref-derived target against its exact object header, parses one direct value, and
frames a directly sized stream with bounded resumable reads. It performs no file, network,
callback, filter decoding, or async-runtime I/O.

# Semantic owner

Parser/Security owns indirect-object header validation, direct `/Length` policy, exact stream
framing, source identity, deterministic budgets, cancellation, and stable object failures.
`core/bytes` owns immutable source snapshots and byte delivery, while `core/syntax` owns direct
object and keyword syntax. `core/xref` is a sibling consumer of syntax rather than an object-crate
dependency. `core/document` now composes one validated traditional base section into candidate
physical intervals and supplies their bounds through `IndirectObjectTarget`; it does not yet
attest top-level object placement. Future document/revision work owns trusted reference graphs,
revision precedence, and caching.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.1-5.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the one-way `bytes -> syntax -> {xref, object}` dependency boundary, resumable
  ByteSource jobs, source-located object values, exact xref-header validation, and stream spans.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires
  one-way core dependencies, explicit parser states, stable structured errors, checked arithmetic,
  bounded allocation, cancellation, and async-runtime-free core parsing.
- [RPE-STD-002, sections 6-7](../../docs/standards/lifecycle-and-concurrency.md) defines cooperative
  job cancellation, terminal outcomes, ticket behavior, and distinct resume checkpoints without
  parser execution on data callbacks.
- [RPE-STD-004, sections 7-8 and 13](../../docs/standards/traceability-and-provenance.md) defines this
  module record, independent-implementation boundary, and dependency-direction governance.
- [RPE-STD-005, sections 4-10](../../docs/standards/security-and-resource-budget.md) requires
  deterministic input, stream, retry-work and allocation limits, fixed-interval cancellation,
  immutable source validation, and checked object offsets and lengths.

The repository does not yet bind this bootstrap profile to a pinned ISO 32000 snapshot, errata
set, or clause-level conformance cases. This module therefore makes no ISO or R0 semantic coverage
claim.

# Algorithms and derivations

- An object job binds a complete known-length `SourceSnapshot`, expected `ObjectRef`, xref offset,
  an independent exclusive physical upper bound, and the revision `startxref`. It requires
  `xref_offset < object_upper_bound <= startxref < source_len`, rejects inconsistent geometry
  before reading, and compares the complete snapshot on every poll. Envelope, payload-end, and
  terminal-boundary work is capped by the physical bound. The completed object retains the full
  snapshot and that bound.
- The envelope range includes the byte immediately before a nonzero xref offset. That byte must be
  one of the PDF whitespace bytes NUL, horizontal tab, line feed, form feed, carriage return, or
  space; closing delimiters and all other bytes are rejected. The first parsed object-number span
  must also begin exactly at the xref offset. Together with exact number, generation, and `obj`
  checks, this prevents accepting leading trivia, obvious adjacent token continuations, or an
  offset into the middle of a longer numeric token. It does not prove global top-level context.
- Envelope windows grow geometrically from a bounded initial size. The parser recognizes one
  direct value and a borrowed terminal keyword without speculative rollback. `endobj` completes a
  direct object. Only a dictionary may precede `stream`, which must have a strict line ending.
- A stream dictionary must contain exactly one `/Length`. A nonnegative direct integer is checked
  against the stream budget and revision/source geometry; an indirect reference is a stable
  unsupported capability, while missing, duplicate, negative, or ill-typed values are malformed.
- The framing algorithm never creates a request proportional to the declared `/Length` and never
  requires the complete payload to be resident or contiguous. A bounded envelope window can
  opportunistically overlap a payload prefix (or all of a small payload) because `data_start` is
  unknown until the dictionary is parsed. After computing the payload span with checked
  arithmetic, the job requests a separate small range beginning at the exact `data_end`. It
  requires LF or CRLF, an immediately adjacent `endstream`, and a later `endobj`, and does not scan
  nearby bytes to repair an incorrect length. The result retains only the payload span; callers
  request and decode that range separately.
- Envelope and terminal-boundary phases have distinct caller-owned checkpoints. Pending re-polls
  preserve the same logical range/ticket and do not recharge read work. The first attempt of each
  new logical exact request and each complete parse window is charged cumulatively across
  geometric retries, including requests satisfied from an existing cache or ending in source
  failure.
- Cancellation is checked before source polls and phase transitions. Syntax loops use fixed
  256-iteration probes, and the object-owned `/Length` scan applies the same bound. Cancellation,
  malformed input, unsupported behavior, resource exhaustion, source failure, and internal
  failure remain separate terminal policies.
- Syntax, a declared stream payload, or terminal framing that would require bytes beyond the
  supplied candidate interval fails as `ObjectCrossesPhysicalBound` with diagnostic
  `RPE-OBJECT-0021`, category `Syntax`, recovery `CorrectInput`, and the exclusive bound as its
  offset. This distinguishes candidate-index geometry failure from source EOF, ordinary malformed
  stream framing, and configured resource exhaustion.
- The public one-shot poll keeps its ready value inline. A documented Clippy exception avoids an
  additional infallible, untracked heap allocation solely to equalize enum variant sizes.
  Object results and polls are move-only rather than exposing an unbudgeted deep `Clone`.
  Diagnostics and Debug output expose references, offsets, spans, limits, and stable codes but not
  object, dictionary, string, or stream bytes.

# External observations

No external PDF engine output or implementation source was used for this module. The repository's
PDFium runner and local PDFium checkout remain separate development-only O4 observers and are not
dependencies, normative inputs, or golden sources for this crate.

# Dependencies and generated data

The only crate dependencies are the in-repository `pdf-rs-bytes` and `pdf-rs-syntax` lower-level
product primitives. In particular, this crate does not depend on its sibling `core/xref`, which
preserves the architecture's declared dependency direction without an ADR. The implementation
otherwise uses the Rust standard library and has no development dependency, external PDF/2D
engine, generated table, committed corpus object, filesystem access, network access, unsafe code,
or async runtime.

Behavior tests assemble project-authored structural PDF bytes in memory, including the canonical
612-byte generator geometry and a synthetic large stream whose middle payload is deliberately not
supplied. No third-party code or data is introduced, so this crate adds no redistribution
obligation beyond those already recorded for the repository.

# Tests and fuzz targets

Object behavior tests cover canonical objects 1-4, direct scalar/array/dictionary values, exact
header and stream spans, xref-number/generation/token-boundary checks, two-phase Pending and stable
tickets, disconnected envelope/boundary supply with a missing payload middle, request priority,
direct `/Length` policies, strict LF/CRLF boundaries, incorrect lengths without repair scanning,
cumulative read/parse budgets, cancellation, full snapshot mismatch, one-shot lifecycle, and
redacted diagnostics. A framing matrix exhausts every initial envelope/boundary split and
direct/stream candidate physical-bound cut for the bootstrap fixtures,
and exact and one-less runtime resource boundaries. Physical-bound cases assert stable
`RPE-OBJECT-0021` policy and terminal re-poll behavior.
Separate tests cover all limit-profile relationships and hard ceilings, lower source-error policy
mapping, and repository dependency/purity rules. A `tools/quality` integration test generates the
canonical PDF, parses its traditional xref section, and frames every in-use target while checking
the expected physical ordering and stream payload span.

No registered coverage-guided fuzz target, pinned conformance corpus, Range platform E2E, or
Native/external-engine differential is claimed in this bootstrap slice.

# Known deviations and unsupported cases

- This is one strict framing job, not a complete object resolver. Indirect `/Length`, dependency
  states, cycle/reference-chain diagnostics, object caches, object streams, xref streams, hybrid
  files, incremental revision precedence, encryption, filters, content interpretation, and
  document services remain unimplemented.
- `IndirectObjectTarget` carries xref-derived geometry but is publicly constructible so the object
  crate remains independent of its xref sibling. The object job therefore treats every target as
  untrusted and revalidates source geometry, its independent physical upper bound, the preceding
  PDF-whitespace byte, and the full object header. The product `core/document` bootstrap now
  derives candidate intervals for one strict traditional base section, while the quality loop
  exercises the canonical xref-to-object path.
- The one-byte predecessor check proves only an obvious local token boundary. It cannot prove that
  a syntactically matching header is top-level rather than embedded after whitespace in a comment,
  string, or stream. The candidate document index prevents a framed object from crossing the next
  indexed physical offset but does not authenticate the offset's lexical context. Top-level
  attestation remains a security-relevant gate before treating this bootstrap as a complete
  resolver.
- Only known-length immutable snapshots and strict behavior are accepted. R1/R2 repair, unknown
  length, platform Range scheduling/coalescing, cancellation delivery and ticket unsubscription,
  terminal completion/cancel/close arbitration, and browser/desktop E2E remain future work.
- Hard ceilings and default limits are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.
- The lower syntax model still implements deep `Clone` for source-located objects. New object-layer
  results are move-only, but callers can currently clone the borrowed lower model through its
  public API without object-budget charging; a later syntax API cleanup must remove or replace
  those clones with an explicitly fallible, budgeted operation.

# History

- 2026-07-13: Added bounded indirect-object and direct-length stream framing, resumable two-range
  ByteSource state, deterministic limits, behavior tests, and repository purity governance.
- 2026-07-13: Added canonical generated-PDF composition coverage through the external
  `tools/quality` test boundary without changing the product dependency graph.
- 2026-07-13: Added independent candidate physical bounds, PDF-whitespace predecessor policy,
  stable `RPE-OBJECT-0021` crossing diagnostics, and document-index governance without claiming
  top-level attestation.
