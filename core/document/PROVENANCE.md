# Scope

`core/document` provides `OpenStrictBaseRevisionJob` as the resumable product composition for one
strict traditional base revision. It runs traditional-xref opening, derives an unauthenticated
`CandidateRevisionIndex`, and consumes that candidate through `AttestRevisionJob` without
publishing the intermediate from the composition job. Attestation validates a supported PDF
header at source offset zero, frames every in-use object in physical order, and scans the prefix
and every gap through `startxref` for only PDF whitespace and terminated comments. Only complete
success publishes `AttestedRevisionIndex`.
An explicit local-repair planning surface separately retains a proof-bearing local-xref result,
derives original candidate targets, accepts only effective offsets minted from proof-bearing local
objects, and atomically rebuilds every physical interval before attestation. Its rebuilt wrapper is
deliberately unauthenticated and cannot be used as an `AttestedRevisionIndex`. A consuming local
attestation job then reuses the complete header/object/gap scanner over effective geometry and
publishes only `LocallyRepairedRevisionIndex`, which retains both xref and object repair evidence
without exposing or converting to the internal strict-shaped proof.
`OpenLocallyRepairedBaseRevisionJob` is the formal core R1 composition: it alone owns local xref
discovery, every declared-geometry object probe, complete proof-plan allocation, atomic rebuild,
and final attestation without publishing any intermediate typestate.
`OpenSourceXrefStreamJob` is a separate partial M1 acquisition primitive for an already-classified
stream-object anchor. It frames that exact indirect object, accepts only direct `/Length`, requests
the exact encoded payload from the same immutable snapshot, validates the declared terminal
boundary, and publishes the existing unfiltered `XrefStream` only inside a non-cloneable wrapper
that also retains the complete framed container.
That sealed typestate is the only public factory for bounded jobs that reparse one exact object,
iteratively follow a top-level direct-reference chain, or validate a strict Catalog and count its
complete page tree or enumerate its bounded strict outline while preserving the attested-object
access boundary.

The crate performs no file, network, data callback, async-runtime, stream decoding, general object
graph traversal, or caching. Its first resolver slice follows only whole-object reference aliases;
an independent revision-aware slice can frame one effective uncompressed definition, resolve an
uncompressed direct-integer stream `/Length` dependency, and bind one predecoded object-stream
entry to its latest-wins compressed xref row from an already-composed revision chain;
the separate page-count slice interprets only Catalog and Page/Pages structural fields, while the
outline slice interprets only the optional Catalog `Outlines` reference and strict linked outline
dictionaries reachable from it. Neither publishes page handles, inherited resources, a reusable
object graph, resolved destinations, or executable actions. All source access is
synchronous polling through an injected `ByteSource`; all long-running CPU work uses an injected
cooperative cancellation probe. A separate pure document-semantic helper decodes already lexical
PDF strings under the bounded ISO 32000-1 text-string rules used by the outline slice and available
to future metadata services.

# Semantic owner

Parser/Security owns revision composition, physical object indexing, and top-level attestation.
It also owns this strict-base Catalog, outline-enumeration, and page-count validation slice;
broader page indexing, resource inheritance, destination resolution, action interpretation, and
page services remain future document-model work.
`core/xref` owns traditional xref parsing, `core/syntax` owns the supported header and direct-object
grammar, `core/object` owns bounded indirect-object framing, and `core/bytes` owns immutable
snapshot-bound exact reads. This crate composes those sibling results without moving their lower
semantic responsibilities.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires one-way core dependencies, revision identity, reverse xref composition, and validation
  of offset, generation, object number, and object header before use.
- [RPE-ARCH-001, sections 5.8-5.9](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a lazy document boundary and page-tree protection against cycles, duplicate children,
  false counts, excessive depth, and non-Page/Pages objects.
- [ISO 32000-1:2008, 7.9.2.2 and Annex D.3](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines the `FE FF` UTF-16BE selection rule, supplementary-character requirement, and the
  PDFDocEncoding-to-Unicode mapping used by human-readable text strings. The authorized Adobe
  snapshot acquired on 2026-07-14 has SHA-256
  `9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff`.
- [ISO 32000-1:2008, 7.3.9 and 7.3.10](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines null-as-omission and the default semantic equivalence of permitted direct and indirect
  object values. The outline bootstrap implements direct null omission but deliberately stops at
  indirect semantic values and does not implement undefined-reference or reference-to-null
  equivalence. The same authorized snapshot and SHA-256 above apply.
- [ISO 32000-1:2008, 7.3.8.2 and 7.5.4](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines direct or indirect stream Length and cross-reference entry selection across updates. The
  revision-aware slice implements only effective uncompressed object framing and an uncompressed
  direct-integer Length target; it does not decode object streams or acquire revision sections.
  The same authorized snapshot and SHA-256 above apply.
- [ISO 32000-1:2008, 7.5.7](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines generation-zero compressed entries, object-stream container/index lookup, and decoded
  entry boundaries. The revision-aware slice binds already-validated unfiltered object streams but
  does not acquire or filter-decode their payloads. The same authorized snapshot and SHA-256 above
  apply.
- [ISO 32000-1:2008, 7.5.8](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines cross-reference stream dictionaries, entries, and the requirement that a primary stream
  identify its own uncompressed container. The source-acquisition slice implements exact unfiltered
  direct-Length framing and primary self-entry validation; hybrid ownership may instead be proved by
  its traditional primary during later composition. The same authorized snapshot and SHA-256 above
  apply.
- [ISO 32000-1:2008, 7.7.2 and 12.3.3](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines the Catalog's optional indirect `Outlines` entry and the outline root/item dictionaries,
  linked-list topology, text titles, targets, and signed visible-item counts used by the strict
  outline bootstrap. The same authorized Adobe snapshot acquired on 2026-07-14 has SHA-256
  `9de0ca9e8570d6209e8bd48a355be8eb6ec376acfc3fc3ae97cd8730351417ff`.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires stable
  structured errors, checked arithmetic, fallible bounded allocation, and redacted diagnostics.
- [RPE-STD-002, sections 6-7](../../docs/standards/lifecycle-and-concurrency.md) requires cooperative
  cancellation and stable terminal state for resumable core jobs.
- [RPE-STD-005, sections 4-9](../../docs/standards/security-and-resource-budget.md) requires
  deterministic entry, memory, scan, and child-work budgets before processing untrusted structure.

This slice does not claim an ISO 32000 conformance profile or R0 resolver coverage.
An executable strict-base regression also proves that a valid object header one byte away from the
xref-derived target remains `ObjectAttestationFailure`; the composition job does not silently enter
repair or publish partially attested state.

# Algorithms and derivations

## Source-framed unfiltered xref streams

- The constructor receives the complete classified anchor geometry rather than caller payload
  bytes. `IndirectObjectTarget::at_xref_stream_anchor` distinguishes primary geometry from a hybrid
  supplement whose exclusive object bound is the owning traditional primary anchor.
  `OpenObjectEnvelopeJob` then proves the exact object number, generation, `obj` header, dictionary,
  stream line ending, and payload start without crossing that physical bound.
- Only a direct nonnegative `/Length` may continue. An indirect declaration returns
  `UnsupportedIndirectLength` with the exact dependency and operand offset because resolving it
  would require revision precedence that this bootstrap is still acquiring. The direct claim is
  consumed by `OpenStreamBoundaryJob`, while one exact `ByteSource` request acquires the payload.
  Boundary and payload bytes may arrive in any physical order, but the state machine exposes only
  one active `Pending` ticket at a time: payload is completed first and boundary validation second.
  Re-polling an unresolved request preserves its job, checkpoint, ticket, missing ranges, and work
  charge, so it remains compatible with the single-waiting-target Range arbiter contract.
- The final transition defensively rechecks snapshot, explicit xref-stream target kind, container
  reference, anchor, upper bound, owning revision anchor, header/object/end-object containment,
  direct claim, dictionary source, and exact payload span. It passes only that source-owned
  `ByteSlice` to `parse_unfiltered_xref_stream`; caller-provided payload bytes never become proof.
- A primary stream must contain one matching uncompressed self row at its exact anchor and
  generation. A hybrid supplement may omit that row because the owning traditional primary can
  define it, but any self row it does contain must still be exact. The non-cloneable
  `SourceAcquiredXrefStream` retains the complete framed container, parsed table, object work,
  exact payload-read accounting, combined retained-proof bytes, and xref-stream stats. Public
  queries expose only scalar metadata and borrowed rows.
  The wrapper does not publicly lend the cloneable naked `XrefStream`; a crate-private borrow
  remains available to the future proof-preserving mixed-revision coordinator.
- Unfiltered decoded length is capped before the exact payload Range request by both the object
  stream-byte ceiling and the xref decoded-byte ceiling. The payload `ByteSlice` is transient and
  is dropped before publication. Ready retained proof has no variable child fan-out: it is exactly
  one framed dictionary plus one xref entry vector, each already independently bounded. Their
  checked actual sum is retained in stats and rechecked against the derived sum of syntax-owned,
  syntax-container, and xref-entry ceilings, so this fixed two-child composition needs no separate
  caller-supplied aggregate profile.
- `SourceXrefStreamError` does not route failures through `DocumentError`. It preserves complete
  lower `ObjectError`, `XrefStreamError`, or `SourceError` values and has stable policy for the
  acquisition-only checks. Cancellation and source change are terminal, a successful one-shot job
  rejects replay, and no source bytes appear in diagnostics or debug output.
- This slice does not decode filters, resolve indirect `/Length`, discover or follow `/Prev`, acquire
  a traditional primary or `/XRefStm` partner, compose revision precedence, publish a revision
  chain, integrate strict-open or repaired services, schedule object streams, or provide Session
  ownership. It is therefore partial M1 evidence rather than a source revision or M1 exit.

## Strict base-revision opening

- `OpenStrictBaseRevisionJob` owns the complete xref-to-candidate-to-attestation transition for one
  immutable source snapshot and caller-assigned revision identity. Its validated profile bundles
  the existing xref, candidate-index, attestation, object-framing, and direct-syntax limits; it does
  not replace or weaken any child limit.
- The xref and attestation contexts must carry one identical `JobId`. Their tail, xref-section,
  top-level scan, object-envelope, and stream-boundary checkpoints must be five pairwise-distinct
  values, so each `Pending` result identifies the exact child phase to requeue.
- Xref success is synchronously converted into a candidate index, which is immediately consumed by
  a newly constructed attestation child. Neither the `XrefSection` nor candidate is returned by the
  composition job. Xref and document failures retain their complete lower structured error, while
  terminal failure is stable and a repeated poll after success returns `JobAlreadyComplete`.
- `Pending` forwards the child's exact ticket, canonical missing ranges, and checkpoint unchanged.
  Cumulative stats retain the latest xref work, the completed candidate-index accounting, and the
  latest attestation work. One injected `DocumentCancellation` is adapted for xref work and used
  directly by candidate construction and attestation, preserving cancellation across every phase.

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

## Local-repair geometry planning

- `LocalRepairPlanningRevision` consumes and retains `LocallyParsedXrefSection` so xref repair
  evidence cannot be detached while object offsets are probed. It exposes only original
  `PhysicalObjectInterval` metadata and explicitly unattested targets bound to that exact snapshot,
  reference, declared offset, next-object bound, and effective xref anchor; it exposes no bare
  candidate index.
- `EffectiveObjectOffset::from_locally_framed` is the only public constructor for one plan record.
  It requires a `LocallyFramedObject`, rechecks header/object spans and every inseparable repair
  diagnostic, and retains the original bound plus revision anchor. A canonical strict child mints
  declared-equals-effective evidence without a diagnostic; an offset-repaired child must carry
  exactly one matching source-bound object-offset diagnostic. Direct stream-length diagnostics do
  not change physical offset geometry, but the fixed plan record validates and retains them for the
  later attestation ledger.
- Rebuild requires exactly one proof in original physical-interval order for every in-use object.
  Snapshot, reference, declared offset, original upper bound, and revision anchor must all match
  before any effective offset is installed. Incomplete, reordered, foreign, widened-bound, or
  out-of-revision evidence fails without publishing geometry.
- All effective offsets are installed before one second bounded cancellable sort. The sort swaps
  each proof record together with its physical interval, so a real effective-order change cannot
  bind evidence to the wrong object. The sort starts
  from the original candidate's already-consumed step count, so declared and effective sorts share
  `max_sort_steps`; duplicate effective offsets are rejected, logical-to-physical slots are rebuilt,
  and every next-object or `startxref` upper bound is recomputed only after the complete sort.
  Allocator-reported plan capacity is admitted together with the candidate index under the existing
  logical-index byte ceiling.
- `LocallyRebuiltCandidateRevision` retains xref diagnostics, the complete original-to-effective
  proof list, aggregate geometry stats, and the rebuilt candidate. Its API labels intervals as
  rebuilt but unauthenticated and exposes no raw candidate or attested-object authority.
- `AttestLocalRepairRevisionJob` consumes that wrapper, validates seven distinct scan/strict/
  candidate/repair checkpoints, and reruns the same PDF-header and top-level trivia coverage as
  strict attestation. Offset-only objects reopen strictly at their effective offsets; only objects
  carrying a planned direct-length repair may use the strict-first local child. All final child
  reads, including repair scans, and parser windows share revision-level aggregate caps.
- Final direct-length replay must match the planned snapshot, reference, kind, declared value, and
  effective value. Scan-byte and candidate-count observations may differ after upper bounds are
  rebuilt, so they remain planning provenance rather than semantic equality fields. The final
  stream length claim and payload span are separately rebound to the effective value. No new
  offset or unplanned length repair may appear during publication.
- `LocallyRepairedRevisionIndex` privately owns the complete top-level proof while exposing only a
  repaired typestate, fixed-size object attestations, xref diagnostics, the original repair plan,
  and geometry/attestation stats. It provides no `Deref`, `AsRef`, consuming conversion, strict
  object reopen, or raw candidate access.
- `OpenLocallyRepairedBaseRevisionJob` validates one shared `JobId`, equal first/final object
  priority, and seventeen globally distinct xref, first-pass, and final-attestation checkpoints.
  It sequentially drives local xref discovery, converts every original physical interval through
  `OpenLocalObjectJob`, rebuilds all effective geometry, and starts final attestation in one
  non-recursive one-shot state machine. Pending passes through with the exact active checkpoint;
  lower xref, object, and document errors remain separately inspectable and stable.
- `LocalRepairProbeLimits` bounds first-pass object count, cumulative read/parse work,
  repair-only scan bytes, header and boundary candidates, and allocator-reported fixed-proof
  capacity. Evidence capacity is checked and fallibly reserved before any object child starts.
  Each child borrows the remaining validation and zero-capable repair-only slices before polling,
  so one child cannot overshoot the parent ceiling. Aggregate failures retain both the document
  limit and original lower object limit; exact exhaustion still permits later strict-valid
  objects because repair-only caps may be zero.

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
- A completed `AttestedRevisionIndex` may be consumed into `SharedAttestedRevisionIndex`, whose
  private `Arc` is cloneable for service ownership. There is no constructor from a candidate,
  parsed xref, or raw `Arc`; every shared handle therefore retains exactly one already-complete
  attestation proof and the same immutable snapshot and parser profiles.

## Revision-aware object resolution

- `RevisionObjectIndex` consumes one already-validated `RevisionChain`. It fallibly retains every
  primary and hybrid xref anchor plus every uncompressed entry offset, charges actual vector
  capacity, sorts with bounded cancellable heapsort, and deduplicates only after work accounting.
  Each effective uncompressed interval ends at the nearest greater retained object or xref anchor.
- Lookup preserves the chain's exact newest-primary, same-revision-supplement, then older-revision
  precedence. A winning free or unknown-type null row hides all older definitions. Exact generation
  mismatch never falls back. Compressed definitions retain their object-stream number and decoded
  entry index without fabricating a physical source span.
- `ResolveObjectJob` validates the selected uncompressed number, generation, offset, preceding
  whitespace, `obj` header, value, and `endobj` framing through `core/object`. A primary xref-stream
  self entry remains unsupported because the ordinary target model is bounded before a revision's
  `startxref`; the resolver does not weaken that invariant.
- A stream envelope with indirect `/Length` performs a second latest-wins exact-generation lookup.
  Only an effective uncompressed object whose complete direct value is a nonnegative integer can
  mint `ResolvedStreamLength`; its snapshot, reference, and physical integer span are checked again
  before the original envelope resumes at the exact payload end. Self-dependency is a stable cycle.
  Missing, free, null, generation-mismatch, compressed, stream-valued, negative, and non-integer
  dependencies are terminal and never trigger older-definition fallback.
- Four pairwise-distinct checkpoints identify target envelope, target boundary, dependency envelope,
  and the dependency boundary reserved for future profile expansion. Pending tickets and canonical
  missing ranges pass through unchanged. Cancellation, source change, and every lower error enter a
  stable terminal state. `RevisionResolverLimits` is the explicit parent profile: its cumulative
  read and parse ceilings are exactly twice one validated child-object ceiling, and at most two
  children run sequentially, so neither child receives more than half the parent scope. Aggregate
  resolver stats report checked work without claiming a session-wide budget.
- `RevisionObjectIndex::resolve_compressed` accepts only a complete `core/object::ObjectStream`
  proof. It rechecks the immutable snapshot, generation-zero container identity, latest effective
  uncompressed container locator, exact physical offset/bound/revision anchor, payload containment,
  xref decoded index, and embedded object number. The returned `ResolvedCompressedObject` borrows
  both the whole stream proof and exact entry, so neither container provenance nor latest-wins
  compressed provenance can be discarded while retaining the decoded value.
- The derived intervals remain xref metadata plus exact local header/framing validation. Unlike
  `AttestedRevisionIndex`, this slice does not linearly prove top-level trivia/object coverage. It
  also does not acquire `/Prev` or `/XRefStm`, decode filters, schedule object-stream acquisition,
  implement repair, cache values, integrate strict-open/page-count/outline, or establish a complete
  resolver or M1 exit.

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

## Successful-value resident footprint

- `AttestedObject` treats its contained `IndirectObject` as the single source of syntax-heap
  evidence. Before publication, object stats and the returned object's retained-heap value must
  agree. Its checked footprint is `size_of::<AttestedObject>() + syntax_heap_bytes`; the chain
  component is zero.
- `ResolvedReference` computes its footprint from the value that owns the allocations rather than
  storing a second total. Its checked formula is `size_of::<ResolvedReference>() + terminal syntax
  heap bytes + chain prefix capacity * size_of::<ObjectRef>()`. The top-level inline size already
  contains `AttestedObject`, `ReferenceChain`, vector headers, and stats, so none of those inline
  values are added again. Intermediate reference objects are dropped and do not contribute.
- A successful `ResolvedReference` retains the complete validated `ReferenceChainLimits` profile
  that produced it. A future warm lookup must include that profile in its key; otherwise a value
  created under a larger object, edge, depth, retained-path, read, or parse budget could bypass the
  colder request's stricter failure boundary.
- Every successful `AttestedObject`, including the terminal object inside a resolved reference,
  retains the exact validated object-framing and direct-syntax profiles that produced it. This
  keeps cache admission able to reject a value created under a different parser profile after the
  one-shot access job and its borrowed attested index are gone.
- Every `usize` conversion, capacity multiplication, and component sum is checked. Arithmetic
  failure maps to the existing internal/do-not-retry policy because this API measures a value; it
  does not decide a resource budget. The components include Rust inline storage and
  allocator-reported syntax/reference-path backing capacity. They exclude allocator metadata,
  source and byte-cache storage, opaque stream payloads, and any future outer cache container.
- The footprint is admission evidence only. It does not reserve bytes, establish a resident owner,
  publish an eviction policy, cache failures, coalesce work, or make active-job historical stats
  equivalent to current ownership.

## Strict Catalog and bounded page count

- `AttestedRevisionIndex::count_pages` remains the borrowed factory for the one-shot page-count job,
  while `SharedAttestedRevisionIndex::count_pages_owned` clones the proof handle into an otherwise
  identical job suitable for later registry ownership. The job first reopens the trailer root
  through the proof-preserving object-access API and accepts
  only a direct dictionary with one structural `/Type /Catalog` and one exact `/Pages` reference.
  It neither follows a whole-object Catalog alias nor accepts a stream as the Catalog. The pure
  Catalog parser is shared within `core/document`, but optional service fields remain lazily owned:
  malformed or duplicate `/Outlines` data does not affect a page-count request that never asks for
  an outline.
- Page-tree traversal is iterative depth-first work over exact Page/Pages references. Every node
  is reopened through the same attested index. `/Type` is mandatory and explicit; the job never
  infers node kind from `/Kids`. Root Pages must omit `/Parent`; every Page and non-root Pages node
  must name its exact traversed parent.
- Every interpreted structural key is unique under this strict profile. Catalog checks `Type` and
  `Pages`; Pages checks `Type`, `Parent`, `Kids`, and `Count`; Page checks `Type` and `Parent`.
  Duplicate unrelated extension keys remain outside this slice.
- A deterministic preallocated open-addressing table records every discovered node identity while
  a separate bounded vector retains active Pages ancestors. A child already on the active path is
  `PageTreeCycle`; any other repeated child is `DuplicatePageTreeNode`, including a repeated entry
  in one Kids array or a shared DAG node reached from two parents.
- `/Count` is never trusted for allocation, skipping, or the result. A postorder finish record
  compares each declared nonnegative integer against the number of validated leaf Page objects
  actually reached in that exact subtree. Empty trees with a declared count of zero are valid;
  every mismatch is terminal and no partial count is published.
- Work-stack, seen-table, and active-path capacities are derived only from validated limits,
  computed with checked arithmetic, fallibly reserved before the job is returned, and rechecked
  against actual allocator capacity. The allocations are released before either successful or
  failed terminal publication; stats retain their historical reservation size for evidence.
- This result is a source- and revision-bound Catalog summary plus a scalar count. It is not a
  persistent Catalog cache, random-access PageIndex, page handle, inherited-resource result, or
  claim of general PDF page-tree compatibility.

## Strict Catalog and bounded outline enumeration

- Outline enumeration lazily interprets the shared strict Catalog parser's optional `Outlines`
  service field. A missing or null field yields an empty outline. A present field must be unique
  and contain an exact indirect reference; page counting never inspects or validates it.
- The outline root and every item are reopened through the proof-preserving attested-object API and
  must be direct dictionaries rather than streams or whole-object aliases. The root and each item
  require paired `First` and `Last` child boundaries when either is present. Each sibling chain is
  traversed from `First` through `Next` to the exact `Last`, with exact `Parent` and `Prev` links,
  missing initial `Prev`, missing terminal `Next`, and global identity tracking that rejects cycles
  and duplicate items. Optional direct null values are treated as omitted before these rules.
- Every item requires a direct text-string `Title`, decoded through the bounded ISO 32000-1 text
  helper. `Count` is recomputed from validated descendants and checked with its sign: an item's
  absolute value reflects its direct children plus the positive visible contributions of open
  children. A nonempty root requires a nonnegative `Count` equal to every visible item, including a
  closed item itself but excluding that item's hidden descendants; an empty root requires `Count`
  omission. Declared counts are never used for allocation, skipping, or traversal.
- `Dest` and `A` are optional direct semantic values and are mutually exclusive. The result records
  only their target kind; it does not resolve a destination or inspect, interpret, or execute an
  action. Section 7.3.10 permits an indirect outline-root `Type` and indirect `Title`, `Count`,
  `Dest`, and `A` forms, but this profile returns `UnsupportedOutlineRepresentation` before
  dereferencing and therefore does not judge whether the referenced target has a valid type or
  value.
- Success publishes only a bounded owned preorder with decoded titles, declared count evidence,
  target kind, and work statistics; title-bearing values redact content from `Debug`. It is not a
  mutable outline graph, persistent outline cache, navigation service, destination index, or ISO
  conformance result.

## Bounded PDF text strings

- `decode_text_string` consumes only an already lexical-decoded `PdfString`; literal/hexadecimal
  escaping remains owned by `core/syntax`. A leading `FE FF` selects UTF-16BE and every other byte
  sequence selects PDFDocEncoding, including `FF FE`.
- The PDFDocEncoding mapping is a manual transcription of ISO 32000-1:2008 Annex D.3, Table D.2,
  rather than an external implementation table. Bytes marked undefined by that normative table
  are rejected; defined TAB, LF, and CR controls remain valid.
- UTF-16BE validation requires an even payload and exact high/low surrogate pairing, including
  supplementary Unicode scalars. The BOM is consumed but not published. No lossy replacement or
  Unicode normalization changes the decoded scalar sequence.
- Decoding first measures exact logical UTF-8 length, then fallibly reserves the result and checks
  actual allocator capacity against the same validated ceiling before materializing characters.
  Input bytes and UTF-8 bytes are independently bounded, all arithmetic is checked, and both passes
  probe cancellation after at most 256 input code units.
- Successful values expose encoding and scalar/capacity sizes while redacting text from `Debug`;
  errors expose only stable policy, a relative decoded-string byte offset, and bounded resource
  context. The helper does not retain a source snapshot or grant object-resolution authority.

# Resource accounting and resumability

- Strict base opening adds no independent unbounded work or allocation pool. Its stats expose the
  three existing child accounting domains separately; repeated child `Pending` polls retain the
  lower layer's exact first-request charging rather than charging the composition transition again.
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
- A page-count job independently bounds Page/Pages nodes, depth, leaf pages, per-node Kids,
  traversal capacity, and aggregate child reads and parses. The Catalog child participates in
  aggregate object work but is not counted as a Page/Pages node. Child caps use the smaller of the
  retained object profile and remaining aggregate allowance; repeated Pending polls charge only
  new lower deltas, and scoped exhaustion retains the complete lower object error.
- An outline job independently bounds item count, depth, siblings per level, title input and UTF-8
  bytes, cumulative child reads and parses, and allocator-reported traversal/result retention.
  Root and item dictionaries participate in aggregate object work. Child caps are lent from the
  remaining aggregate allowance, `Pending` polls charge only newly observed lower deltas, and no
  untrusted `Count` or linked-list value determines a reservation size. Title decoding first
  measures input and logical UTF-8 work without allocating output, checks aggregate logical and
  requested retained capacity, and only then materializes the string; actual allocator capacity is
  rechecked before publication.
- Reference-chain limits apply to one job only. Repeated jobs do not share a budget owner, resident
  cache, or reservation ledger. A future persistent resolver must add complete retained-object
  ownership, admission/reservation, and eviction before it can cache `Ready` values or coalesce
  work. The successful-value footprint supplies only the value-owned measurement needed by such
  an owner.
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

The separate `ResolvedObject` wrapper retains its effective locator and exposes the lower framed
object only by shared borrow; it has no consuming into-object path. This preserves latest-wins
evidence for downstream inspection, but it is not interchangeable with `AttestedObject`: its
xref-derived interval has not received the strict base path's linear top-level coverage proof.

The page-count job is another consumer of the same proof. It may borrow the attested index or own a
clone of its sealed shared handle; both forms execute the same state machine and mint child access
only through that proof. Its summary copies only immutable source, revision, Catalog,
page-tree-root, scalar count, and work evidence. It exposes neither parsed dictionaries nor a
constructor capable of reopening arbitrary objects after the job and its proof handle are gone.

The outline job is likewise only a proof consumer and supports the same borrowed or sealed-handle
ownership forms. Its owned preorder copies decoded titles and scalar/link-derived evidence after
all traversed dictionaries have been reopened and validated. It exposes neither parsed target
objects nor byte, resolution, navigation, or action-execution authority after the job and its proof
handle are gone.

The proof is deliberately closed-world: bytes between indexed in-use objects must be trivia.
Stale free-object bodies, unindexed top-level objects, or other top-level tokens are rejected even
when another PDF implementation might tolerate them. Opaque stream payload bytes need not be
resident and are not decoded or semantically validated; their checked direct length and terminal
framing only establish lexical extent.

# External observations

No PDFium, other PDF engine, third-party implementation source, or external output was used to
derive the candidate index, revision-aware resolver, attestation state machine, text-string mapping
and decoder, page-tree traversal, or outline topology and count validation. After that
implementation boundary was fixed, a separate `tools/baseline` O4 probe compared the public PDFium
bookmark surface with Native on self-authored fixtures: the valid observable subset matched
exactly, while a wrong `/Prev` produced the expected
strictness difference because PDFium does not expose that backlink. A separate public page-count
probe matched Native exactly and repeatably on valid one-page and nested three-page fixtures. On an
otherwise identical nested fixture whose positive root Count was 4 rather than the recomputed 3,
Native returned `RPE-DOCUMENT-0033` while PDFium produced `page_count=4`, an expected strictness
difference. Both probes are non-gating and unregistered; the relevant feature states remain
`PLANNED`, and there is no canonical broad corpus, registered/contained PDFium baseline, or M1 exit
claim in this slice.

# Dependencies and generated data

The only dependencies are the in-repository `pdf-rs-bytes`, `pdf-rs-syntax`, `pdf-rs-xref`, and
`pdf-rs-object` crates. The PDFDocEncoding match table is manually encoded from the hash-pinned
normative snapshot; it is not generated data. There are no development dependencies, platform
I/O APIs, external PDF engines, or async runtimes.

# Tests

Strict-base-open tests cover complete product-entry publication, all five distinct checkpoints,
same-job context validation, reverse physical Range delivery, unchanged `Pending` replay and
charging, xref and document error preservation, cancellation in both xref and attestation phases,
snapshot mismatch, cumulative stats, and stable successful and failed terminals.
Source-xref-stream tests cover primary and hybrid geometry, exact direct Length and caller bounds,
indirect-Length and filtered-stream Unsupported policy, malformed Type/container/self evidence,
single-ticket Pending replay with boundary bytes delivered before payload bytes, cancellation,
source change, lower byte-source failure preservation, stable terminal replay, and object/xref
size and work limits.
Revision-resolver tests cover nearest cross-revision anchors, primary and hybrid-supplement
provenance, primary target plus supplement-only indirect Length, supplement self-container bounds,
older effective revision bounds, latest free/null/compressed/generation terminal states, primary
xref-stream self-entry rejection, exact integer Length evidence, three-checkpoint sparse resume
without payload residency, cancellation, source change, self-dependency, stable terminals, explicit
two-child parent work limits, and 256-step entry-count/dedup cancellation.
Compressed-resolver tests obtain the container through `ResolveObjectJob` and an exact RangeStore
payload slice, then cover valid generation-zero entries, mismatched decoded index/object number,
explicit not-compressed lookup classification, same-revision and newest-revision free/null masking,
stale replacement containers, nested compressed containers, nonzero requested generation, and
foreign snapshot rejection.
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
redacted diagnostics. Page-count tests cover strict Catalog shape, valid flat and nested trees,
zero pages, structural duplicate keys, wrong node shapes, exact parent links, self and multi-level
cycles, duplicate children, per-node Count agreement, borrowed and owned proof handles, Pending
idempotence, source change, cancellation, terminal replay, and every traversal limit boundary.
Repository policy checks the sibling dependency
allowlist and absence of platform/external-engine APIs. Resident-footprint tests cover checked
component and capacity overflow, portable runtime inline sizes, scalar and allocated syntax values,
identical small/large stream-dictionary footprints, nonzero pre-reserved root-chain capacity,
multi-hop terminal-only syntax ownership, and exact component totals without fixed
platform-specific byte constants. Text-string tests construct every input through the public
syntax parser and cover the complete non-identity PDFDocEncoding mapping, defined and undefined
controls, undefined high bytes, UTF-16BE BMP and supplementary scalars, malformed surrogate
structure, BOM selection, exact input/output capacity limits, cancellation, and redaction.
Outline behavior tests cover absent and empty outlines, owned jobs surviving caller-handle release,
owned cancellation, valid nested linked lists, structural
topology/count/title/target failures, root Count presence and omission, indirect semantic-value
boundaries, representative traversal limits, pre-allocation aggregate title checks, and redacted
diagnostics.
Outline limit-configuration tests cover defaults, valid boundaries, zero and hard-ceiling
rejection, and independent aggregate resource dimensions. Repository policy also verifies the
strict-outline feature plus its ISO text, null, indirect-object, Catalog, outline-dictionary, and
document-architecture requirement links and explicit partial-scope boundaries.
Local-repair geometry tests drive a real local-xref result and every xref-derived target through the
proof-bearing local object job, repair one shifted object header and one direct stream length,
require a complete ordered plan, and verify the final effective sort, logical remap, and every
rebuilt upper bound. A separate fixture causes the declared and effective physical orders to differ
and proves paired interval/evidence sorting through final publication. Regressions reject
incomplete, reordered, widened-bound, cancelled, context-conflicting, and semantically changed
plans; exact and one-less retained-plan, two-sort, and final aggregate child-work ceilings prove
pre-publication resource boundaries. Final tests assert a complete repaired ledger, redacted debug,
stable terminal replay, and the absence of a strict typestate conversion.
Local repaired-open tests use one four-object fixture whose single job repairs final `startxref`,
traditional-row whitespace, one object offset, and one direct stream length before a trailing
strict object. They prove the complete xref/object ledger, all seven first-pass aggregate exact and
one-less boundaries, nested candidate read/parse exhaustion with retained lower evidence,
zero-repair strict success, pre-probe evidence admission, seventeen-checkpoint context validation,
stable cancellation/source mismatch, and sparse upper-before-lower Range delivery through xref,
first-pass, and final-attestation phases without repeated charging.

# Known deviations and unsupported cases

- The product strict-open path still accepts only one traditional base revision. A separate
  already-composed-chain resolver implements latest-wins uncompressed lookup and binds validated
  unfiltered object streams, but `/Prev` and `/XRefStm` source acquisition, filtered stream decode,
  object-stream scheduling/ownership, and service integration remain unsupported.
- The formal opening entry remains a synchronous resumable core job. It does not own a Range store,
  physical transport, scheduler, session lifecycle, or parser requeue loop, and therefore does not
  by itself establish M1 exit.
- Local R1 now has one aggregate-bounded core repaired-open coordinator and returns a proof-bearing
  repaired document only after complete top-level attestation. The coordinator remains a
  synchronous injected-`ByteSource` job: it does not own Range permits, transport, runtime queues,
  request/generation lifecycle, page-count/outline service jobs, or a complete Session, and does
  not establish M1 exit.
- Attestation eagerly frames every in-use object. The access job can reparse one proven value, the
  chain job can follow top-level whole-object aliases, and the count job can traverse strict
  Page/Pages dictionaries. They are not a complete resolver or reusable lazy document model:
  general nested semantic references, persistent dependency states, concurrent work coalescing,
  retained-value caching, negative caching, admission/reservation, eviction, and cross-job/session
  resident ownership remain unsupported. Successful footprints are measurement evidence only.
- Indirect stream `/Length` is supported only when the effective dependency is an uncompressed
  direct nonnegative integer in the separate revision-aware job. Compressed or aliased Length,
  repair, encrypted object interpretation, filters, decoded stream payloads, random-access page
  indexing, inherited resources, page handles, name-tree services, writer behavior, and document
  actions remain unsupported.
- The separate page-count O4 comparison covers only two fixed valid counts and one mismatched
  positive root Count. It is exact and repeatable on the valid fixtures, but remains non-gating and
  unregistered and cannot adjudicate the Native validator's Parent, cycle, duplicate, or recursive
  Count evidence. It does not advance the feature from `PLANNED` or establish M1 exit.
- Outline enumeration is limited to the direct-semantic-value `m1.strict-outline.v1` bootstrap.
  Indirect outline-root `Type`, indirect `Title`, `Count`, `Dest`, and `A` values, indirect root or
  item aliases, undefined-reference and reference-to-null omission semantics, styles, destination
  resolution, action inspection or execution, persistent outline ownership, canonical corpus
  fixtures, registered/contained PDFium baseline execution, and broad Native/PDFium Outline
  differential evidence remain unsupported. The separate non-gating O4 probe covers only the
  normalized public-bookmark intersection and one expected `/Prev` strictness difference.
- Text-string decoding implements the ISO 32000-1:2008 PDFDocEncoding and UTF-16BE profile only. It
  preserves rather than interprets embedded Unicode language escape sequences, does not normalize
  Unicode, and does not implement the PDF 2.0 UTF-8 text-string extension.
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
- 2026-07-13: Added checked value-owned resident footprints for proof-bearing objects and resolved
  references without introducing a cache, reservation ledger, or eviction policy.
- 2026-07-14: Retained the complete successful reference-resolution limit profile as cache-key
  evidence without adding warm lookup or persistent ownership.
- 2026-07-14: Retained successful object and syntax profiles on proof-bearing values so a future
  session owner can validate parser-profile-equivalent admission.
- 2026-07-14: Added strict Catalog validation and a resumable bounded page-count job with exact
  Parent and Count checks, deterministic cycle/duplicate detection, and precharged traversal
  storage released before terminal publication.
- 2026-07-14: Added bounded cancellable ISO 32000-1 PDF text-string decoding for PDFDocEncoding and
  BOM-selected UTF-16BE with supplementary scalar support and redacted diagnostics.
- 2026-07-14: Added strict bounded outline enumeration with exact linked-list topology, recursively
  validated signed counts, direct decoded titles and target kinds, aggregate work and retained-byte
  limits, and explicit no-action/no-destination-resolution boundaries.
- 2026-07-14: Added the formal resumable strict-base opening entry, composing xref discovery,
  candidate indexing, and attestation under one job identity, five distinct checkpoints, preserved
  child errors and stats, and one cancellation source without publishing an intermediate index.
- 2026-07-15: Added bounded effective revision lookup, derived cross-revision physical anchors, and
  Range-resumable uncompressed object framing with same-snapshot indirect stream-Length evidence.
- 2026-07-15: Bound validated unfiltered decoded object streams to effective generation-zero
  container definitions and exact compressed xref indices without fabricating physical spans.
- 2026-07-15: Added multi-revision stale/masked/nested container regression evidence and a distinct
  generation-zero not-compressed lookup policy.
- 2026-07-15: Added proof-bound local repair planning, complete effective-offset collection,
  aggregate second-sort and retained-plan budgets, and atomic candidate-interval rebuild while
  keeping the result explicitly unauthenticated.
- 2026-07-15: Added paired effective-interval/proof sorting and complete repaired top-level
  attestation with semantic length replay, aggregate child work, and a sealed repaired typestate.
- 2026-07-15: Added the single core R1 repaired-open coordinator with seventeen-checkpoint identity,
  preallocated proof plans, parent-lent first-pass aggregate caps, and sparse Range replay evidence.
- 2026-07-15: Added a sealed cloneable attested-index ownership handle and owned page-count and
  outline job constructors without adding a Session, registry, or alternate proof-minting path.
