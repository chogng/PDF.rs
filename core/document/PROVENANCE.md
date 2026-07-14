# Scope

`core/document` currently builds only a `CandidateRevisionIndex` from one already parsed
traditional `XrefSection`. It derives a strict physical upper bound for every in-use candidate and
offers an explicitly named `unattested_target` handoff to `core/object`. It performs no file,
network, data callback, async-runtime, object-body, or reference-resolution work; its only injected
call is a bounded cooperative cancellation probe.

# Semantic owner

Parser/Security owns revision composition, physical object indexing, and future object-header
attestation. `core/xref` owns traditional xref parsing; `core/object` owns bounded indirect-object
framing. This crate is the product composition layer between those siblings and does not move
either lower-level responsibility.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires one-way core dependencies, revision identity, reverse xref composition, and validation
  of offset, generation, object number, and object header before use.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires stable
  structured errors, checked arithmetic, fallible bounded allocation, and redacted diagnostics.
- [RPE-STD-002, sections 6-7](../../docs/standards/lifecycle-and-concurrency.md) requires cooperative
  cancellation in long-running core work.
- [RPE-STD-005, sections 4-9](../../docs/standards/security-and-resource-budget.md) requires
  deterministic entry, memory, and work budgets before processing untrusted structure.

This first slice does not claim an ISO 32000 conformance profile or R0 resolver coverage.

# Algorithms and derivations

- Construction validates total and in-use row ceilings before retaining records. A conservative
  fixed charge covers each allocator-reported logical-row and physical-interval capacity slot;
  compile-time-sized tests ensure each charge dominates its Rust entry type. Both vectors use
  `try_reserve_exact`, then validate their actual reported capacity, so allocator over-reservation
  and allocation failure become structured resource exhaustion rather than hidden retained bytes
  or panic.
- In-use candidates are sorted by absolute offset with an in-repository heapsort. Comparisons and
  swaps consume `max_sort_steps`; cancellation is probed initially, after at most 256 sort steps,
  at completion, and at equivalent intervals in all other long loops.
- Every in-use offset must be below the revision `startxref`, and duplicate physical offsets are
  rejected. The next larger in-use offset becomes the exclusive object bound; `startxref` bounds
  the last candidate. This prevents a candidate object job from reading past the next claimed
  in-use offset; it does not authenticate either claim as a top-level object.
- The logical index retains free rows and exact generations. `interval` reports distinct missing,
  free, and generation-mismatch failures. The trailer `/Root` is independently required to match
  one exact-generation in-use row before the candidate index is returned.
- `unattested_target` supplies all five `IndirectObjectTarget` fields: immutable snapshot, exact
  reference, xref offset, derived physical upper bound, and revision `startxref`.

# Trust boundary

This index is deliberately a candidate artifact. Sorting xref offsets and deriving non-overlapping
physical bounds does **not** prove that an offset occurs at PDF top level: an attacker-controlled
xref may still point after whitespace inside a comment, literal string, hex string, or stream
payload. A later document-owned attestation phase must establish top-level object-header context
and revision semantics. Until that phase exists, `CandidateRevisionIndex`, its intervals, and its
unattested targets must not be supplied to a trusted resolver or described as authenticated
objects.

# External observations

No PDFium, other PDF engine, third-party implementation source, or external output was used to
derive this index.

# Dependencies and generated data

The only dependencies are the in-repository `pdf-rs-bytes`, `pdf-rs-syntax`, `pdf-rs-xref`, and
`pdf-rs-object` crates. There are no development dependencies, generated tables, corpus objects,
platform I/O APIs, or async runtimes.

# Tests

Unit tests cover physical heapsort ordering, exact sort-budget exhaustion, the 256-step
cancellation ceiling, and checked conservative index accounting. Limit tests cover defaults,
positive minima, equality at all hard ceilings, every zero field, inconsistent entry ceilings,
and every hard-ceiling-plus-one case. Public-path revision tests parse project-authored xref bytes
through `RangeStore` and `OpenXrefJob`, then cover canonical and out-of-object-number-order
intervals, duplicate/out-of-revision offsets, missing/free/generation lookup, pre-cancellation,
and every five-field unattested target value. The quality loop also composes the canonical
generator, xref parser, candidate index, and bounded object jobs. Repository policy checks the
dependency allowlist, absence of development dependencies and platform/external-engine APIs, and
crate-level unsafe-code and missing-documentation policy.

# Known deviations and unsupported cases

- Only one already parsed traditional candidate revision is indexed. Revision chains, `/Prev`,
  xref streams, hybrid references, object streams, and revision precedence remain unsupported.
- Top-level object-header attestation and a trusted resolver are intentionally absent.
- Candidate index construction is synchronous CPU work. Runtime scheduling and cancellation race
  arbitration remain platform/runtime responsibilities.
- Hard ceilings and default limits are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.

# History

- 2026-07-13: Added candidate-only single-revision physical indexing, bounded cancellable sort,
  exact lookup errors, and unattested five-field object targets.
