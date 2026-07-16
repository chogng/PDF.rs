# Scope

`core/content` is the pure M2-05 operator-scanning boundary. It accepts an exact zero-based,
caller-ordered sequence of borrowed, already-decoded content streams. Each input carries the
indirect stream `ObjectRef`, stream ordinal, and complete decoded byte slice. A successful scan
atomically publishes an immutable owned `ContentProgram` containing source-ordered operands and
operators with decoded-coordinate provenance.

The crate performs no content-stream acquisition, PDF object resolution, filter decoding,
encryption, inherited resource lookup, graphics/text interpretation, Scene construction, cache
insertion, platform I/O, async scheduling, or external-engine fallback.

# Semantic owner

The document layer owns `/Contents` direct/indirect shape validation, stream acquisition,
proof-bearing object and filter evidence, and page resource scopes. The filter layer owns decoded
stream production. `core/content` owns only decoded lexical scanning, operator classification,
owned operand representation, decoded coordinates, deterministic work/ownership limits, and
terminal scan replay. The later Content VM consumes `ContentProgram`; it, rather than this scanner,
validates known-operator arity, structural state, resources, and Scene semantics.

`core/syntax` owns `ObjectRef`. This crate otherwise has no product dependency.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 6.4-6.7](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a one-way document-to-content-to-Scene dependency, ordered content interpretation,
  bounded operand/operator state, stable command provenance, and explicit unsupported outcomes.
- [RPE-STD-001, sections 5-9 and 14](../../docs/standards/coding-standard.md) requires checked
  arithmetic, deterministic parsing, bounded allocation, content-redacted diagnostics, and no
  platform I/O in pure core crates.
- [RPE-STD-003, sections 7-8](../../docs/standards/testing-standard.md) requires stable token and
  operator order, exact boundary tests, deterministic replay, malformed/unsupported separation,
  and metamorphic coverage.
- [RPE-STD-005, sections 4-6 and 11](../../docs/standards/security-and-resource-budget.md) requires
  independent decoded-byte, token, depth, fuel, result-count, and retained-memory ceilings before
  publication.

# Algorithms and derivations

- Input ordinals must be exactly `0..N` in slice order. Every stream keeps an independent decoded
  offset and object identity. A stream boundary acts as semantic PDF whitespace: regular tokens,
  names, literal strings, hexadecimal strings, dictionary delimiters, and operators never join
  across it, while operand groups and array/dictionary structures may continue in the next
  stream. A comment reaching a boundary ends there.
- The scanner recognizes PDF whitespace and comments, `null`, booleans, checked signed integers,
  validated decimal/exponent reals retaining their raw lexeme, decoded names, decoded literal and
  hexadecimal strings, ordered arrays, and ordered dictionaries preserving duplicate keys.
  Literal escapes, line continuation, CR/CRLF normalization, nested parentheses, name `#xx`
  escapes, odd hexadecimal nibbles, and dictionary-name keys are handled explicitly.
- The stable initial table recognizes `q`, `Q`, `cm`, `BT`, `ET`, `BX`, `EX`, `MP`, `DP`, `BMC`,
  `BDC`, and `EMC`. Each `OperatorKind` exposes its exact token, operand range, structural context,
  and base VM fuel declaration. Scanner publication does not enforce those semantic arities.
- Any nonempty regular token that is neither an operand keyword nor a number is a lexically valid
  operator. A token absent from the stable table is published as `ContentOperator::Unknown` with
  owned redacted bytes. Invalid number-shaped tokens, invalid escapes, invalid hexadecimal data,
  wrong dictionary keys, mismatched/unclosed delimiters, unterminated strings, and final dangling
  operands are structured malformed outcomes rather than unknown operators.
- Every operator retains its exact object, stream ordinal, decoded token start/length, and
  zero-based page-global operator ordinal. Primitive operands retain exact single-stream extents.
  Arrays and dictionaries retain start/end positions that can cross stream objects, and every
  nested value/key retains its own decoded evidence.
- Independent validated limits cover stream count, aggregate decoded bytes, token count, raw
  bytes per token, top-level operands per operator, array/dictionary depth, operator count,
  deterministic scanner fuel, and allocator-reported retained capacity. All counters and offsets
  use checked arithmetic.
- Fuel charges every scanned decoded byte and every lexical token. Trivia is therefore bounded
  even when it produces no retained values. Cooperative cancellation is checked before work, at
  least once per 256 fuel units, and immediately before publication.
- Owned vector and scalar-buffer growth is fallible and preflighted. Actual allocator-reported
  capacity deltas for the operator vector, every operand group, every nested array/dictionary,
  every decoded name/string, every retained real lexeme, and every unknown operator token are
  accumulated in `retained_bytes`. Inline value headers, the optional job `Arc` control block,
  allocator metadata, borrowed decoded streams, and caller-owned proof/filter data are outside
  this metric.
- Scanner state is private until the complete stream sequence has passed syntax, cancellation,
  and all budgets. `ContentScanJob` stores either one immutable `Arc<ContentProgram>` or one
  copyable structured failure. Later polls clone only the `Arc` or copy the error and charge no
  additional scanner work.
- Diagnostics retain stable codes, policy categories, recovery guidance, decoded coordinates, and
  numeric budget context only. They never retain or format names, strings, operator bytes,
  comments, or decoded content. Sensitive model `Debug` output is similarly redacted.

# Tests

- Single-stream coverage of every direct operand family, known operator table entry, one unknown
  operator, ordered operand grouping, decoded values, and exact page-global provenance.
- Multi-stream operand groups and arrays with independent decoded offsets and cross-stream extents.
- PDF whitespace, comments ending at stream boundaries, literal escape/continuation/newline rules,
  name escapes, odd hexadecimal strings, nested operands, and duplicate-preserving dictionaries.
- Unknown regular operators versus invalid numbers, escapes, strings, hexadecimal bytes,
  dictionary keys, delimiters, and dangling operands.
- Exact and one-less tests for streams, aggregate decoded bytes, tokens, token bytes, operands per
  operator, nesting, operators, fuel, and retained capacity.
- Pre-cancelled terminal failure, successful terminal `Arc` replay, zero-work replay statistics,
  invalid stream ordering, and invalid limit configuration.
- Repeated-scan equality, content-redacted `Debug`, and split/merge metamorphism when no lexical
  token crosses the inserted stream boundaries.
- Repository policy checks for the single approved dependency, no external-engine marker, no
  unsafe block, and no filesystem, network, or process API in product sources.

# Known deviations and unsupported cases

- The scanner consumes already-decoded streams only. It does not yet accept `/Contents` object
  forms, retain encoded source spans/filter chains, or prove the relationship between decoded
  bytes and a document snapshot; the M2-05 document acquisition adapter owns that separate work.
- Inline-image `BI`/`ID`/`EI` byte framing is not in this bounded scanner profile. `BI` is currently
  a lexically valid unknown operator, and arbitrary inline-image payload bytes are not accepted as
  an opaque token. The later registered profile must either add bounded inline-image framing or
  classify that feature before this scanner is selected.
- Indirect references are not operand syntax in this content profile. A lexical `R` is an unknown
  operator after its preceding numeric operands.
- The scanner preserves duplicate dictionary keys for later VM/resource policy. It does not decide
  last-wins semantics.
- Known operator arity and context are declarative only. Stack balance, compatibility behavior,
  marked-content semantics, resource lookup, matrix composition, unsupported-resource reporting,
  and Scene command production belong to M2-06.

# History

- 2026-07-16: Added the pure bounded M2-05 decoded-stream scanner, owned direct operands, initial
  operator table, exact decoded provenance, structured malformed/unknown separation, independent
  budgets, cooperative cancellation, atomic terminal replay, and deterministic boundary tests.
