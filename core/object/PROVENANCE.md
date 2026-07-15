# Scope

`core/object` is the strict single-indirect-object framing bootstrap for the Native product core.
It validates one xref-derived target against its exact object header, parses one direct value, and
frames a stream with bounded resumable reads. A staged path may stop after the stream dictionary,
publish a direct value or indirect `/Length` dependency, and resume exact boundary validation only
after a same-snapshot resolver supplies the referenced integer metadata. A separate strict entry
parses one complete unfiltered object-stream payload only from a fully framed stream container and
an exact source-bound `ByteSlice`, preserving decoded coordinates separately from physical spans.
It performs no file, network, callback, filter decoding, or async-runtime I/O.

# Semantic owner

Parser/Security owns indirect-object header validation, `/Length` declaration classification,
same-snapshot length-claim checks, exact stream framing, source identity, deterministic budgets,
cancellation, object-stream header/index validation, decoded-coordinate values, and stable object
failures.
`core/bytes` owns immutable source snapshots and byte delivery, while `core/syntax` owns direct
object and keyword syntax. `core/xref` is a sibling consumer of syntax rather than an object-crate
dependency. `core/document` composes one validated traditional base section into candidate
physical intervals, attests top-level placement, and supplies proven bounds through
`IndirectObjectTarget`. Future document/revision work owns complete reference graphs, revision
precedence, and caching.

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
- [ISO 32000-1:2008, 7.5.7](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines object-stream `/Type`, `/N`, `/First`, optional `/Extends`, generation-zero containers
  and embedded objects, header object-number/relative-offset pairs, and the prohibition on stream
  objects inside object streams. The authorized Adobe snapshot acquired on 2026-07-14 has SHA-256
  `9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff`.

The repository does not yet bind this bootstrap profile to an approved errata set or registered
clause-level conformance cases. The hash-pinned source informs the implementation but this module
makes no ISO or R0 semantic coverage claim.

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
- A stream dictionary must contain exactly one `/Length`. Missing, duplicate, negative, or
  ill-typed values are malformed. The compatibility `OpenObjectJob` accepts a nonnegative direct
  integer and preserves `UnsupportedIndirectLength` for an indirect reference. The staged
  `OpenObjectEnvelopeJob` instead returns `DeclaredStreamLength`, retaining the exact operand span
  and either the checked direct value or referenced object without computing a payload end.
- `StreamEnvelope::direct_length_claim` binds a direct value to the envelope's snapshot and owner.
  For an indirect declaration, `ResolvedStreamLength::from_uncompressed_object` accepts only a
  header-validated direct nonnegative integer `IndirectObject` and derives its snapshot, reference,
  value, and physical value span; there is no public raw-value constructor. The envelope rejects a
  different snapshot or reference as `RPE-OBJECT-0022`. A future document resolver remains
  responsible for proving that this object is the effective revision definition selected for the
  declared reference. `OpenStreamBoundaryJob` rechecks the claim against the declaration,
  stream budget, checked payload-end arithmetic, and physical object bound before any source poll.
  The envelope seals the original job context, object and syntax profiles, cumulative work caps,
  and already-consumed stats; the boundary phase must inherit them and continues charging from the
  envelope totals rather than resetting a second per-object budget.
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
- A successful envelope combines the syntax parser's allocator-reported scalar and container
  capacity with checked arithmetic. `ObjectStats::retained_heap_bytes` is a historical gauge for
  the latest accepted envelope rather than cumulative parse work or proof that the completed job
  still owns the allocation, and the same value travels with a returned `IndirectObject`.
  Discarded geometric attempts are not accumulated. A stream retains only its parsed dictionary
  capacity while the boundary phase runs; after a later boundary failure the stats preserve the
  accepted-envelope measurement even though the state has released the dictionary. Its opaque
  payload length is reported separately and never counted as retained heap bytes.
- `ObjectWorkCaps` lets a parent composition job lend a nonzero cumulative read/parse slice that
  cannot exceed either the fixed object-work hard ceiling or the object's configured totals. The
  legacy constructor supplies the configured totals unchanged. A scoped read is charged before
  `ByteSource::poll`, and a scoped parse window is charged before parser invocation, so exhausted
  parent work cannot perform the rejected lower-layer operation. The scoped caps do not weaken
  per-object envelope, boundary, stream, source, or syntax limits.
- Cancellation is checked before source polls and phase transitions. Syntax loops use fixed
  256-iteration probes, and the object-owned `/Length` scan applies the same bound. Cancellation,
  malformed input, unsupported behavior, resource exhaustion, source failure, and internal
  failure remain separate terminal policies.
- Syntax, a declared stream payload, or terminal framing that would require bytes beyond the
  supplied candidate interval fails as `ObjectCrossesPhysicalBound` with diagnostic
  `RPE-OBJECT-0021`, category `Syntax`, recovery `CorrectInput`, and the exclusive bound as its
  offset. This distinguishes candidate-index geometry failure from source EOF, ordinary malformed
  stream framing, and configured resource exhaustion.
- `parse_unfiltered_object_stream` accepts only a complete `IndirectObjectValue::Stream` plus a
  `ByteSlice` whose source identity and exact range equal the framed payload span. The container
  must have generation zero and unique `/Type /ObjStm`, nonnegative `/N` and `/First`, no `/Filter`
  or `/DecodeParms`, and an optional generation-zero `/Extends` reference. `/Extends` is retained
  as provenance only and never changes xref lookup order; an immediate self-reference is rejected.
- The decoded header begins with `/N` nonzero object-number/relative-offset pairs. Any remaining
  bytes before `/First` are retained as an uninterpreted decoded-coordinate extension span instead
  of being mistaken for additional standard pairs. The first relative offset is zero; later offsets
  are strictly increasing, all object numbers are unique, and every computed entry slot remains
  inside the complete payload. Duplicate detection uses fallibly reserved working vectors plus
  cancellable heapsort rather than hidden hash-table allocation.
- Each entry slot is parsed as exactly one supported direct syntax object followed only by PDF
  whitespace/comments. A trailing `stream` construct or second object is rejected. Physical
  `Located` values are consumed internally and converted into `DecodedObjectSpan`,
  `DecodedLocatedObject`, `DecodedArray`, and `DecodedDictionary`; no decoded offset is published as
  a physical `ByteSpan`. Scalar capacity, allocator-reported syntax and decoded container capacity,
  entry capacity, header/index working capacity, cumulative syntax windows, limits, and
  fixed-interval cancellation are accounted separately. Syntax container capacity is bounded by a
  real child limit, and recursive conversion reserves the still-live syntax container bytes when
  checking its decoded-value peak. Lower syntax resource failures retain their exact kind, limit,
  consumed, and attempted evidence while exposing their position only as a decoded coordinate.
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
cumulative read/parse budgets, exact retained scalar/container capacity for direct and stream
objects, discarded-retry isolation, cancellation, full snapshot mismatch, one-shot lifecycle,
and redacted diagnostics. Scoped-work tests cover positive minima and hard ceilings, zero and
hard-ceiling-plus-one rejection, caps above configured totals, exact and one-less read/parse work
for both direct and stream objects, getters, and equivalence of the legacy constructor with
explicit configured-total caps. A framing matrix exhausts every initial envelope/boundary split and
direct/stream candidate physical-bound cut for the bootstrap fixtures,
and exact and one-less runtime resource boundaries. Physical-bound cases assert stable
`RPE-OBJECT-0021` policy and terminal re-poll behavior. Staged-length tests cover direct versus
indirect declaration classification, same-snapshot and exact-reference claim binding, stable
`RPE-OBJECT-0022` mismatch policy, sparse envelope and exact-boundary Pending/resume checkpoints,
a deliberately unsupplied large payload tail, terminal source change, cancellation, retained
resolved-value provenance, and exact versus one-less aggregate work across both phases.
Object-stream tests build containers through the public RangeStore and `OpenObjectJob` path, then
cover exact physical payload binding, independent decoded coordinates, nested arrays/dictionaries
and references, `/Extends` validation and self-loop rejection, uninterpreted header-extension
bytes, duplicate numbers, zero/nonincreasing offsets, slot crossing, top-level indirect-reference
and embedded-stream rejection, unsupported filters, foreign source slices, exact/one-less working,
retained-entry, retained-value, and cumulative-syntax limits, immediate cancellation, and
fixed-interval cancellation during long numeric header scans and recursive conversion without
partial publication. Child syntax exhaustion verifies decoded coordinates and preserved lower
resource-limit evidence.
Separate tests cover all limit-profile relationships and hard ceilings, lower source-error policy
mapping, and repository dependency/purity rules. A `tools/quality` integration test generates the
canonical PDF, parses its traditional xref section, and frames every in-use target while checking
the expected physical ordering and stream payload span.

No registered coverage-guided fuzz target, pinned conformance corpus, Range platform E2E, or
Native/external-engine differential is claimed in this bootstrap slice.

# Known deviations and unsupported cases

- This is an object-framing and unfiltered object-stream component, not a complete object resolver.
  A separate document slice now supplies effective uncompressed `/Length` evidence and binds
  compressed xref rows to these decoded entries, but filtered payload decoding, source-driven xref
  and revision acquisition, aliased or compressed `/Length`, general cycle state, caching,
  encryption, content interpretation, and document-service integration remain unimplemented.
- `/Extends` is retained and an immediate self-loop is rejected, but this component does not acquire
  predecessor object streams or validate a transitive `/Extends` graph for cycles.
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
- Only known-length immutable snapshots and strict behavior are accepted. Executable strict-policy
  regressions prove that neither a correct object header one byte away nor a correct stream
  boundary one byte beyond a wrong direct `/Length` is searched automatically. R1/R2 repair, unknown
  length, platform Range scheduling/coalescing, cancellation delivery and ticket unsubscription,
  terminal completion/cancel/close arbitration, and browser/desktop E2E remain future work.
- Hard ceilings and default limits are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.
- The lower syntax model still implements deep `Clone` for source-located objects. New object-layer
  results are move-only, but callers can currently clone the borrowed lower model through its
  public API without object-budget charging; a later syntax API cleanup must remove or replace
  those clones with an explicitly fallible, budgeted operation.
- No object cache, eviction policy, cross-job resident owner, or reservation is implemented here.
  The successful-envelope heap gauge is only an admission-accounting input for a future owner.

# History

- 2026-07-13: Added bounded indirect-object and direct-length stream framing, resumable two-range
  ByteSource state, deterministic limits, behavior tests, and repository purity governance.
- 2026-07-13: Added canonical generated-PDF composition coverage through the external
  `tools/quality` test boundary without changing the product dependency graph.
- 2026-07-13: Added independent candidate physical bounds, PDF-whitespace predecessor policy,
  stable `RPE-OBJECT-0021` crossing diagnostics, and document-index governance without claiming
  top-level attestation.
- 2026-07-13: Added parent-lent cumulative object work caps with pre-poll/pre-parse charging and
  exact direct/stream boundary coverage.
- 2026-07-13: Propagated successful syntax scalar/container heap capacity through object stats and
  returned values without counting discarded retries or opaque stream payloads.
- 2026-07-15: Added staged stream envelopes, explicit direct/indirect `/Length` dependencies,
  same-snapshot resolver claim metadata, and resumable exact-boundary validation while preserving
  the legacy direct-only framing contract.
- 2026-07-15: Added bounded unfiltered object-stream parsing from exact framed source evidence,
  separate decoded-coordinate values, and strict header, entry, capacity, and cancellation checks.
- 2026-07-15: Added uninterpreted header-extension provenance, conversion-peak accounting and
  cancellation evidence, numeric-scan probes, proof-preserving child limit mapping, strict
  top-level member rules, and exact/one-less resource boundaries.
