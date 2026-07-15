# Scope

`core/xref` owns strict cross-reference primitives for the Native product core. Its resumable
bootstrap discovers the final `startxref` marker of a known-length immutable source and parses one
bounded traditional xref table and trailer. A separate resumable anchored job parses one
caller-selected traditional revision section, retains optional `/Prev`, `/XRefStm`, and `/Root`
metadata, and permits sparse update rows without changing the strict base bootstrap. A separate
synchronous primitive validates one
complete caller-supplied unfiltered xref-stream payload against source-bound dictionary metadata.
A third synchronous primitive validates and composes already-parsed revision candidates from
newest to oldest. An explicit local-repair sibling first runs the unchanged strict job, then
handles only bounded fixed-width row whitespace and nearby final-anchor deviations. None performs
file, network, callback, filter-decoder, or async-runtime I/O.

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
  syntax is delegated to the bounded direct-object parser. The strict base finalizer requires
  validated `/Size` and `/Root`, complete `0..Size` coverage, a canonical object-zero row, and a
  live same-section root; `/XRefStm` and `/Prev` remain stable unsupported failures on that path.
- `OpenTraditionalRevisionJob` starts from an exact caller-supplied anchor and an exclusive
  physical upper bound. It reuses the same row/trailer parser but publishes a distinct
  `TraditionalRevisionSection`: `/Size` remains required, `/Root` is optional and never
  fabricated, `/Prev` and `/XRefStm` are retained only after checked backward ordering, and sparse
  rows remain sorted, unique, in-range, and physically before the current table anchor. Object zero
  is optional for an update but retains its canonical free-generation shape when present. This
  candidate type cannot be converted to `XrefSection`, and `OpenXrefJob` plus local R1 repair keep
  using only the strict base finalizer.
- The anchored job grows one section window geometrically without crossing its supplied upper
  bound. Its exact read, rescanned parse, section-window, subsection, entry, source, and allocation
  work reuse validated xref limits; a pending ticket retains one explicit section checkpoint and
  does not recharge until a new larger window is installed. Snapshot mismatch, source change,
  cancellation, and completion are stable terminal states.
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
- The local-repair sibling enters repair only after strict `InvalidEntry` or
  `InvalidXrefKeyword`; Unsupported, resource exhaustion, cancellation, source failure, and
  internal errors remain terminal strict outcomes. It scans only a fixed delta around the final
  tail declaration for token-boundary `xref` anchors, retains a bounded candidate set, and rejects
  more than one normally validated candidate instead of choosing the first or nearest.
- Row repair preserves every ten-digit offset, five-digit generation, entry status, and 20-byte
  row width. Only PDF horizontal whitespace in the two field separators and the horizontal byte
  before a CR/LF row ending may be canonicalized. Comments, arbitrary bytes, moved line endings,
  digit/status damage, and subsection/trailer semantics are not repaired. The canonicalized view
  is submitted to the existing section parser, whose source spans remain accessible only through
  the proof-bearing local result that also owns every repair diagnostic.
- Repair-only scan bytes, canonical-copy/row-evidence workspace, candidates, whitespace edits,
  diagnostics, and allocator-reported diagnostic capacity have independent hard-bounded profiles.
  Pending retries retain exact repair checkpoints without recharging a window; snapshot mismatch
  and cancellation are stable terminal outcomes. Diagnostics contain only source identity,
  coordinates, counts, and work cost, never source row or trailer content.

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

Anchored traditional-revision tests cover sparse update rows, optional older `/Root`, base and
incremental hybrid metadata, backward `Prev < XRefStm < startxref` geometry, duplicate and invalid
trailer fields, conditional object-zero validation, row bounds, upper-half-before-lower Range
delivery, no duplicate pending charge, cancellation, snapshot mismatch, source change, one-shot
completion, invalid source/anchor shapes, and equality/one-less source, subsection, `/Size`,
section-window, cumulative-read, and cumulative-parse limits. Existing strict and local-repair
suites separately prove that `/Prev`, `/XRefStm`, sparse bases, and repaired incremental inputs do
not cross the strict `XrefSection` boundary.

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

Local-repair tests pair canonical, whitespace-only, final-offset, and combined inputs; require an
empty diagnostic ledger for strict success and one source-bound record per accepted repair; reject
semantic row damage, illegal whitespace, ambiguous valid anchors, and out-of-delta candidates;
exercise exact scan/workspace/edit/candidate/diagnostic budgets and hard limit configuration;
resume the repair checkpoint without duplicate charging; and prove Unsupported, resource,
cancellation, and snapshot failures never enter repair. This is an xref component, not object
repair or a repaired document open.

`core/xref::repository_policy` scans product source for forbidden filesystem, network,
async-runtime, and external-engine tokens and verifies that the crate depends only on
`core/bytes` and `core/syntax`. No registered coverage-guided fuzz target, pinned conformance
corpus, Range platform E2E, or Native/external-engine differential is claimed in this bootstrap
slice.

# Known deviations and unsupported cases

- The final-marker bootstrap and product strict-open profile still support one strict traditional
  base table only. The separate anchored job now produces one source-bound, locally validated
  sparse traditional revision candidate and retains `/Prev` and `/XRefStm`, but it does not
  discover the final anchor, traverse `/Prev`, acquire or decode the hybrid supplement, frame an
  xref-stream container, compose the chain, or hand it to document services. The decoded
  xref-stream primitive remains unwired to containing-object framing, direct or indirect
  `/Length`, filter decoding, and Range acquisition.
- Entry offsets are structurally bounded. The separate object framing job validates all four
  supplied canonical targets in a test-only quality composition loop, but a product-owned physical
  interval index for composed candidates, object-header validation across revisions, reference
  resolution, object streams, and caching remain future document/revision-layer work.
- Only known-length immutable snapshots are accepted. Unknown-length discovery, platform Range
  scheduling, request coalescing, cancellation delivery and ticket unsubscription, terminal
  completion/cancel/close arbitration, and browser/desktop E2E remain future runtime/platform
  work. The crate implements only the parser-side cooperative cancellation probe and terminal
  classification.
- The explicit R1 xref sibling covers only fixed-row whitespace and the final traditional-xref
  anchor. It does not repair subsection/trailer semantics, acquire xref streams, traverse
  revisions, probe object headers, repair stream lengths, rebuild effective document geometry, or
  publish a repaired document index. Strict APIs remain unchanged, and this component is not a
  complete R0/R1 or standards-conformance claim.
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
- 2026-07-15: Added the explicit bounded local traditional-xref sibling with strict-first
  allowlisting, unique nearby-anchor selection, fixed-row whitespace canonicalization, normal
  parser revalidation, proof-bearing diagnostics, and independent repair budgets.
- 2026-07-15: Added bounded Range-resumable parsing of one caller-anchored sparse traditional
  revision section with explicit optional trailer metadata while preserving strict-base and R1
  rejection behavior.
