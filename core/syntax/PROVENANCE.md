# Scope

`core/syntax` is the bounded lexical and direct-object syntax layer for the Native product core.
It recognizes a PDF header and strict direct-object syntax from one contiguous immutable byte
window, preserves exact source locations, and distinguishes an incomplete window from malformed
final input. It performs no file, network, callback, or async-runtime I/O.

# Semantic owner

Parser/Security owns lexical recognition, direct-object grammar, exact source spans, strict
stream-line boundary recognition, deterministic syntax limits, cooperative cancellation probes,
and stable syntax failures.
`core/bytes` owns immutable source identity and byte delivery. `core/xref` owns bounded final-marker
and traditional-table acquisition with retry checkpoints. Future object/revision jobs own
indirect-object framing, stream extents, revision chains, and repair policy.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.3](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the one-way `bytes -> syntax -> xref/object` boundary, synchronous resumable parser
  architecture, and bounded lexical/direct-object responsibilities.
- [RPE-STD-001, sections 3, 5-6, and 8-9](../../docs/standards/coding-standard.md) requires one-way
  core dependencies, explicit parser state, stable structured errors, checked arithmetic,
  pre-allocation validation, and an async-runtime-free core parser.
- [RPE-STD-004, sections 7-8](../../docs/standards/traceability-and-provenance.md) defines this
  module record and the independent-implementation boundary.
- [RPE-STD-005, sections 4-7 and 9](../../docs/standards/security-and-resource-budget.md) requires
  deterministic token, string, name, container, recursion, and allocation limits before work,
  with cancellation checks at bounded intervals in potentially long loops.

The repository does not yet bind this bootstrap syntax profile to a pinned ISO 32000 snapshot,
errata set, or clause-level conformance cases. This module therefore makes no ISO/O0 semantic
coverage claim.

# Algorithms and derivations

- Each parser attempt binds a `SourceIdentity`, absolute base offset, contiguous borrowed bytes,
  and an explicit `InputExtent`. Checked spans retain absolute source positions, including empty
  stream boundaries; no parser path fetches bytes or treats unavailable bytes as malformed syntax.
- A strict byte-oriented scanner skips bounded whitespace and comments and recognizes names,
  numeric lexemes, literal and hexadecimal strings, delimiters, keywords, arrays, dictionaries,
  booleans, null, and indirect-reference syntax. Names and strings remain arbitrary bytes rather
  than being assumed to be UTF-8.
- Literal-string nesting, escapes, octal bytes, and line continuation are handled by an explicit
  scan. Hex strings ignore syntax whitespace and pad one final nibble. Real numbers retain their
  validated source lexeme and notation instead of introducing a floating-point canonicalization.
- Arrays and dictionaries preserve source order and exact child locations. Duplicate dictionary
  keys remain observable rather than being silently normalized during syntax parsing.
- An input boundary reached before a token or compound object is decidable returns `NeedMore` when
  the window is non-final. The same truncation in final input becomes a stable redacted syntax
  error. A caller retries from its declared idempotent boundary; this crate does not own hidden
  asynchronous continuations.
- Input, token, comment, decoded name, string source/decoded bytes, cumulative owned scalar bytes,
  token count, container entries, and container depth have validated soft limits beneath fixed
  hard ceilings. Arithmetic is checked and owned scalar allocation is fallible and charged before
  adoption. Container entry fuel is charged before child parsing or allocation. Every array and
  dictionary reserve preflights the complete selected next capacity against the container-byte
  ceiling, then uses an exact reserve request and records the allocator-reported capacity delta
  multiplied by its element size with checked arithmetic. Growth beyond the configured
  container-byte ceiling is rejected before a value is published, and failed-attempt stats retain
  the actual allocator-reported capacity for audit. A
  successful parse reports the exact vector capacity retained by all returned containers without
  changing the scalar owned-byte limit. Each attempt exposes its complete window size so future
  ByteSource jobs can conservatively sum retry work.
- Callers may bind a cooperative `SyntaxCancellation` probe. Every public parser operation checks
  cancellation before work, and every unbounded scanner/container loop checks again after at most
  256 iterations. Cancellation remains distinct from malformed input and resource exhaustion.
- Keyword framing can be consumed as borrowed `RawBytes`, allowing higher object layers to
  distinguish terminal markers without allocation, speculative rollback, or raw-buffer access.

# External observations

No external PDF engine output or implementation source was used for this module. The repository's
PDFium runner and local PDFium checkout remain separate development-only O4 observers and are not
dependencies or normative inputs of this crate.

# Dependencies and generated data

The only crate dependency is the in-repository `pdf-rs-bytes` product primitive for immutable
source identity. The implementation otherwise uses the Rust standard library. It has no
development dependency, external PDF/2D engine, generated table, embedded corpus object,
filesystem access, network access, or async runtime.

No third-party code or data is introduced by this crate, so it adds no third-party license or
redistribution obligation beyond those already recorded for the repository.

# Tests and fuzz targets

Syntax behavior tests exercise complete and truncated headers/tokens, absolute spans, numbers,
name escapes, nested and escaped literal strings, odd-nibble hex strings, arrays, ordered
dictionaries with duplicate keys, indirect references, strict stream boundaries, redacted
diagnostics, borrowed keyword retry semantics, stable cancellation policy and fixed-interval
    scanner probes, exact allocator-reported array/dictionary capacity accounting across empty,
    nested, failed-boundary, and retried parses, an exact/one-less allocator-reported container-byte
    ceiling including failed-attempt stats, and boundary/equality/excess cases for deterministic
    limits.

`core/syntax::repository_policy` scans product source for forbidden filesystem, network,
async-runtime, and external-engine tokens and verifies that the crate depends only on
`core/bytes`. No coverage-guided fuzz target, pinned conformance corpus, or Native/external-engine
differential is claimed in this bootstrap slice.

# Known deviations and unsupported cases

- This is a strict direct-syntax bootstrap, not complete M1 object support. The separate
  `core/xref` bootstrap supports one traditional table, and `core/object` frames one indirect
  object with a direct stream length. Xref streams, hybrid and incremental revisions, indirect
  length resolution, object streams, R0/R1 repair, document services, and rendering remain outside
  this crate.
- Parsing operates on one contiguous window. ByteSource polling, bounded growth, retry scheduling,
  cumulative budgets across retries, and runtime cancellation delivery belong to composing
  xref/object jobs; this crate owns only the synchronous parser-side cancellation checks.
- The accepted grammar is not yet tied to a pinned ISO 32000 snapshot or errata collection, so the
  feature remains pre-conformance and must not be advertised as an R0 syntax capability.
- The project bootstrap numeric grammar retains exponent notation as required by architecture
  section 5.3. This is not an ISO strict-number claim and remains subject to the future pinned
  conformance profile.
- Strict mode is the only policy. There is no tolerant lexical recovery or repair provenance.
- Hard ceilings and default limits are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.
- No fuzz, mutation, external corpus, or O4 differential evidence exists for this module yet.

# History

- 2026-07-13: Added the bounded, source-located strict syntax bootstrap, cooperative cancellation,
  borrowed keyword framing, and repository purity guard.
- 2026-07-13: Added checked allocator-capacity accounting for every retained array and dictionary
  vector while preserving the existing scalar owned-byte and container-entry limit semantics.
- 2026-07-15: Promoted allocator-reported container capacity from an evidence-only statistic to a
  validated public limit with complete selected-growth preflight and exact failed-attempt
  accounting.
