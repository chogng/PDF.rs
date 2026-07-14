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
dependency; a future document/revision layer composes validated xref entries into
`IndirectObjectTarget` values and owns reference graphs, revision precedence, and caching.

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
  and revision `startxref` upper bound. It rejects inconsistent geometry before reading and
  compares the complete snapshot on every poll. The completed object retains the full snapshot.
- The envelope range includes the byte immediately before a nonzero xref offset. That byte must be
  whitespace or a closing delimiter that can separate a top-level token; comment, name, string,
  hexadecimal, array, and other opening delimiters are rejected. The first parsed object-number
  span must also begin exactly at the xref offset. Together with exact number, generation, and
  `obj` checks, this prevents accepting leading trivia, obvious opening-context offsets, or an
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
redacted diagnostics. A framing matrix exhausts every initial envelope/boundary split and logical
truncation point for the bootstrap fixtures, plus exact and one-less runtime resource boundaries.
Separate tests cover all limit-profile relationships and hard ceilings, lower source-error policy
mapping, and repository dependency/purity rules.

No registered coverage-guided fuzz target, pinned conformance corpus, Range platform E2E, or
Native/external-engine differential is claimed in this bootstrap slice.

# Known deviations and unsupported cases

- This is one strict framing job, not a complete object resolver. Indirect `/Length`, dependency
  states, cycle/reference-chain diagnostics, object caches, object streams, xref streams, hybrid
  files, incremental revision precedence, encryption, filters, content interpretation, and
  document services remain unimplemented.
- `IndirectObjectTarget` carries xref-derived geometry but is publicly constructible so the object
  crate remains independent of its xref sibling. The object job therefore treats every target as
  untrusted and revalidates source geometry, the preceding token boundary, and the full object
  header. End-to-end xref-to-object composition remains a future document/revision-layer test.
- The one-byte predecessor check proves only an obvious local token boundary. It cannot prove that
  a syntactically matching header is top-level rather than embedded after whitespace in a comment,
  string, or stream. Rejecting overlapping/embedded targets requires the future physical object
  interval index and document/revision composition and remains a security-relevant gate before
  treating this bootstrap as a complete resolver.
- The revision `startxref` is the current upper bound because no physical next-object offset index
  exists yet. This prevents crossing into the xref section but does not prove that a declared
  stream span avoids every intervening object; a future revision index must provide that bound.
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
