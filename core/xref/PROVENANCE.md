# Scope

`core/xref` owns strict cross-reference primitives for the Native product core. Its resumable
bootstrap discovers the final `startxref` marker of a known-length immutable source and parses one
bounded traditional xref table and trailer. A separate synchronous primitive validates one
complete caller-supplied unfiltered xref-stream payload against source-bound dictionary metadata.
A third synchronous primitive validates and composes already-parsed revision candidates from
newest to oldest. None performs file, network, callback, filter-decoder, or async-runtime I/O.

# Semantic owner

Parser/Security owns bounded xref discovery, table geometry, entry validation, source identity,
and stable xref failures. It also owns representation-independent xref row precedence over parsed
revision candidates. `core/bytes` owns immutable source snapshots and byte delivery, while
`core/syntax` owns direct-object syntax. The sibling `core/object` crate validates one supplied
xref-derived target without introducing an `object -> xref` dependency; future document layers
own proof-bearing xref acquisition, reference resolution, object validation, and document services.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the one-way `bytes/syntax -> xref` boundary, resumable parser architecture, reverse
  `startxref` discovery, revision-chain direction, and validation before xref entries are trusted.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires one-way
  core dependencies, explicit parser states, stable structured errors, checked arithmetic,
  bounded allocation, and async-runtime-free core parsing.
- [RPE-STD-002, sections 6-7](../../docs/standards/lifecycle-and-concurrency.md) defines cooperative
  job cancellation, one terminal outcome, `DataTicket` subscription behavior, and explicit
  `ResumeCheckpoint` phase boundaries without parser resumption on data callbacks.
- [RPE-STD-004, sections 7-8](../../docs/standards/traceability-and-provenance.md) defines this
  module record and the independent-implementation boundary.
- [RPE-STD-005, sections 4-9](../../docs/standards/security-and-resource-budget.md) requires
  deterministic input, scan, entry, and allocation limits before work, cooperative cancellation
  checks in potentially long loops, immutable source validation, and bounded xref traversal.
- [Adobe PDF Reference 1.7, section 3.4.7](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/pdfreference1.7old.pdf)
  defines unknown xref-stream row types as null references and the hybrid lookup order of current
  traditional section, current `/XRefStm` supplement, then `/Prev`.

The repository does not yet bind this bootstrap xref profile to a pinned ISO 32000 snapshot,
errata set, or clause-level conformance cases. This module therefore makes no ISO or R0 semantic
coverage claim.

# Algorithms and derivations

- An xref operation binds one immutable `SourceSnapshot`. The initial bootstrap requires a known
  total length; an unknown length is reported as an explicit unsupported source shape rather than
  guessed from an end-of-file poll. The completed `XrefSection` retains that full snapshot,
  including length and redacted validator, so later object jobs cannot reuse it against merely a
  matching stable ID/revision with different immutable metadata.
- Final-marker discovery grows a suffix window from a 1-KiB default toward a 64-KiB default ceiling
  and selects the last validated `startxref`. Numeric offset parsing uses checked arithmetic, and
  the declared xref offset must remain inside the bound snapshot before another read is requested.
- Table acquisition starts with a 4-KiB default contiguous window at the declared offset and grows
  geometrically up to the known end of input and a 1-MiB default window ceiling. Cumulative exact
  reads and rescanned input each have a 4-MiB default ceiling.
- Traditional tables may contain multiple checked subsections. Subsection geometry, cumulative
  entry count, object-number ranges, fixed-width offsets and generations, common row line endings,
  duplicate object numbers, entry status, and in-use offsets are validated before records are
  adopted. A base table must cover every object number from zero through `/Size - 1`. Trailer
  syntax is delegated to the bounded direct-object parser and must produce a dictionary with
  validated `/Size` and `/Root` values. `/XRefStm` and `/Prev` request unsupported hybrid or
  incremental behavior and are rejected with stable capability errors.
- The decoded xref-stream primitive accepts a complete payload whose physical envelope span is
  already known and whose dictionary was parsed from the same immutable snapshot. It requires a
  unique `/Type /XRef`, positive `/Size`, exactly three bounded `/W` widths, and an optional ordered
  non-overlapping `/Index` array inside `/Size`; absent `/Index` normalizes to `[0 /Size]`. Optional
  `/Root` and `/Prev` values are checked and retained for later composition. `/Filter` and
  `/DecodeParms` are rejected at this explicitly unfiltered boundary.
- Xref-stream row geometry must match the complete payload exactly. Big-endian fields implement
  the type-zero default rule and preserve null, free, uncompressed, and compressed row semantics;
  an unknown future type is retained as a null row rather than reviving an older definition.
  Physical payload geometry remains a source `ByteSpan`, but each row records only a relative
  `DecodedXrefSpan`; a decoded byte offset is never published as a physical source offset. Decoded
  bytes, entries, `/Index` pairs, field widths, and allocator-reported retained entry capacity are
  independently bounded with fallible allocation and cooperative cancellation.
- The pure revision composer consumes primary candidates from newest to oldest. It binds every
  primary and supplement to one complete `SourceSnapshot`, requires exact backward `/Prev` links,
  nondecreasing `/Size`, unique in-range anchors, a complete traditional base but sparse updates,
  and `Prev < XRefStm < current startxref` for traditional hybrid updates. Primary xref-stream
  candidates prove a typed self entry at their own anchor; hybrid anchors may be defined by the
  current table or supplement. Supplemental `/Prev` is retained but never drives the primary chain.
- Lookup checks the current primary before its hybrid supplement and only then visits the older
  revision. A free or null winning row hides every older definition. A newest trailer root must
  resolve to a live generation-compatible row and cannot become visible only through the current
  hybrid supplement. Composition bounds revisions, primary-plus-supplement sections, total rows,
  retained vector capacity, and cancellation loops. These are candidate invariants, not proof of
  source acquisition, filter output, object headers, object-stream contents, or resolver state.
- Missing bytes are parser control flow. Byte acquisition is expressed through the synchronous
  `ByteSource` polling contract; Pending returns the ticket, canonical missing ranges, and caller
  checkpoint without charging parse work. Retryable work restarts only from explicit tail or table
  phase boundaries, and data callbacks never execute xref parsing.
- The owning runtime supplies a cooperative cancellation probe. The job checks it before source
  polling, and reverse discovery and table parsing check it at bounded loop intervals; cancellation
  is a stable terminal job result distinct from malformed input and resource exhaustion. Runtime
  still owns subscription removal and the completion/cancel/close race.
- Tail/window bytes, retry work, subsection and entry counts, decoded xref-stream payloads and
  records, and retained records are subject to validated limits with checked size conversions and
  fallible allocation. The traditional resumable job still rejects xref-stream targets because it
  does not yet own stream acquisition or decoding. Diagnostics expose stable codes, recovery
  policies, and distinct source/decoded offsets without logging source bytes.

# External observations

No external PDF engine output or implementation source was used for this module. The repository's
PDFium runner and local PDFium checkout remain separate development-only O4 observers and are not
dependencies, normative inputs, or golden sources for this crate.

# Dependencies and generated data

The only crate dependencies are the in-repository `pdf-rs-bytes` and `pdf-rs-syntax` lower-level
product primitives. The implementation otherwise uses the Rust standard library. It has no
development dependency, external PDF/2D engine, generated table, committed corpus object,
filesystem access, network access, or async runtime. The behavior test assembles a project-authored
612-byte structural PDF in memory; it reproduces the documented M0 generator's canonical object
offset geometry without importing generator code, metadata, or output.

No third-party code or data is introduced by this crate, so it adds no third-party license or
redistribution obligation beyond those already recorded for the repository.

# Tests and fuzz targets

Traditional-xref behavior tests cover the canonical trailer root and five entries, absolute
section and trailer spans, two-phase Pending/resume after partial Range supplies, equality and
one-less source/entry/cumulative-work limits, complete limit-profile validation, multiple and
invalid subsection layouts, malformed and truncated fixed-width rows, common row endings, exact
tail line boundaries, complete base-table `/Size`, precise xref-stream classification, `/Prev` and
`/XRefStm` policy, lifecycle/context/source-error classification, cancellation, snapshot mismatch,
and redacted section diagnostics. A `tools/quality` integration test runs this job over the
canonical generated PDF and feeds every in-use entry into the sibling object-framing job without
adding a product dependency between the two crates.

Decoded-xref-stream tests cover canonical null, free, uncompressed, and compressed rows; `/W` type
defaults; `/Index` object-number selection; malformed widths, index geometry, row types, and exact
payload length; unsupported filter metadata; separate source and decoded error coordinates;
source identity/geometry mismatch; cancellation; stable recovery policy; equality and one-less
decoded-byte and retained-capacity limits; and invalid limit profiles. These are component tests
over already-decoded payload bytes, not a Range, stream-framing, filter-decode, or revision-chain
E2E.

Revision-chain tests cover traditional, primary-stream, and hybrid candidate layers; exact table,
supplement, and older-revision lookup order; newer replacement, free, and null masking; stream
self anchors; ignored supplemental `/Prev`; strict primary `/Prev`, anchor, `/Size`, entry, source,
and root geometry; complete traditional-base versus sparse-update shape; equality and one-less
revision/section/entry/retained-capacity limits; cancellation; and stable recovery policy. They do
not acquire or decode any xref section and are not a strict-open or document-service E2E.

`core/xref::repository_policy` scans product source for forbidden filesystem, network,
async-runtime, and external-engine tokens and verifies that the crate depends only on
`core/bytes` and `core/syntax`. No registered coverage-guided fuzz target, pinned conformance
corpus, Range platform E2E, or Native/external-engine differential is claimed in this bootstrap
slice.

# Known deviations and unsupported cases

- The resumable open profile supports one strict traditional xref table only. The decoded
  xref-stream table primitive is not wired into that job: containing-object acquisition and
  framing, direct or indirect `/Length` validation, filter decoding, pause/resume, `/XRefStm`
  and sparse traditional-update parsing remain unimplemented. The pure composer can validate
  already-parsed hybrid and `/Prev` candidates with latest-wins lookup, but no proof-bearing job
  currently produces those candidates or hands them to document services.
- Entry offsets are structurally bounded. The separate object framing job validates all four
  supplied canonical targets in a test-only quality composition loop, but a product-owned physical
  interval index for composed candidates, object-header validation across revisions, reference
  resolution, object streams, and caching remain future document/revision-layer work.
- Only known-length immutable snapshots are accepted. Unknown-length discovery, platform Range
  scheduling, request coalescing, cancellation delivery and ticket unsubscription, terminal
  completion/cancel/close arbitration, and browser/desktop E2E remain future runtime/platform
  work. The crate implements only the parser-side cooperative cancellation probe and terminal
  classification.
- No R0/R1 repair behavior is implemented or claimed. Strict failure in this project bootstrap is
  not a standards-conformance statement.
- Hard ceilings and default limits are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.
- No fuzz, mutation, external corpus, or O4 differential evidence exists for this module yet.

# History

- 2026-07-13: Added the bounded traditional-xref bootstrap, resumable ByteSource job, behavior
  suite, governance record, and repository purity contract.
- 2026-07-13: Bound the canonical generated section to the sibling object job in a test-only
  quality integration loop.
- 2026-07-15: Added bounded validation for complete caller-supplied unfiltered decoded
  xref-stream tables with distinct decoded coordinates and stable recovery policy; acquisition,
  decoding, and revision composition remain outside this slice.
- 2026-07-15: Added bounded pure composition for already-parsed traditional, primary-stream, and
  hybrid revision candidates, including null/free masking and strict lookup precedence; parsing,
  proof-bearing acquisition, object resolution, and product integration remain outside this slice.
