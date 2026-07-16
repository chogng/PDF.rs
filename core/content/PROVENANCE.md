# Scope

`core/content` owns the bounded M2-05 scanner, the sealed M2-06 initial Content VM, and the
graphics-capable M3 Content profile. The scanner accepts an exact zero-based, caller-ordered
sequence of borrowed, already-decoded content streams. Each input carries the indirect stream
`ObjectRef`, stream ordinal, and complete decoded byte slice. A successful scan atomically
publishes an immutable owned `ContentProgram` containing source-ordered operands and operators
with decoded-coordinate provenance.

The only public VM entry consumes an exact move-only document-layer `AcquiredPageContent`. It
derives the scanner inputs internally, interprets the resulting program against the same
materialized Page and inherited Resources proof, and atomically publishes one
`Arc<InterpretedPage>`. That value owns the acquisition, immutable Scene, resolved property and
Image XObject proofs, final CTM, and scanner/VM/resource statistics. There is no public API that
accepts an arbitrary `ContentProgram` together with a separate Page.

The crate performs no Page-content acquisition, encryption, platform I/O, async scheduling,
shared-cache insertion, rendering, or external-engine fallback. The M3 image profile may drive
document-owned, proof-bound Image XObject acquisition and filter decoding through its sealed
resumable job; Content does not implement filters or open arbitrary objects itself.

# Semantic owner

The document layer owns `/Contents` direct/indirect shape validation, stream acquisition,
proof-bearing object and filter evidence, materialized page geometry, inherited resource scopes,
Page XObject lookup proofs, and resumable Image XObject decoding. Its no-I/O resource resolvers
validate exact inherited dictionary occurrences before a selected target is acquired. The filter
layer owns decoded stream production. The Scene layer owns fixed-point geometry and matrix
arithmetic, bounded semantic command/resource construction, canonical resource interning, and
immutable Scene publication.

`core/content` owns decoded lexical scanning, operator classification, known-operator operand
validation, exact content-number conversion, graphics/path state, compatibility policy,
property/image-use coordination, immutable semantic execution planning, final CTM, and the atomic
interpreted-Page boundary. Scanner, document resolver, Scene, and VM failures retain their
original structured types rather than being flattened.

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
- The stable table recognizes the initial state and marked-content operators plus the registered
  M3 path construction, path painting, clipping, line-state, DeviceGray/RGB/CMYK, and `Do`
  operators. Each `OperatorKind` exposes its exact token, operand range, structural context, base
  VM fuel declaration, exact operand shape, and post-validation failure policy. Scanner
  publication does not enforce those semantic arities.
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
- `InterpretPageJob` first checks source identity, cancellation, and source identity again. It
  builds exact borrowed descriptors from the acquired ordered streams and runs the same scanner
  internally exactly once. A scanner failure is published intact before any VM retained-program
  admission can replace it. Only a successful program is charged beside descriptor capacity to
  the VM retained peak.
- Every operator repeats the source-cancellation-source guard. Known operand count and type are
  validated before operator admission. Operator-specific numeric conversion follows its registered
  pre-admission rule: scalar operators convert before admission, while `d` admits the complete
  operator/fuel and dash-array traversal before converting individual entries. Conversion always
  precedes state mutation, resource lookup, or unsupported classification. Any fallback is
  rechecked through the same guard so source change precedes cancellation and cancellation
  precedes malformed, unsupported, resource, or state outcomes.
- The graphics profile performs that semantic VM traversal exactly once and freezes an immutable
  `ExecutionPlan`. The plan contains source-ordered marked-content, graphics save/restore, path,
  clip, paint, and image actions together with property proofs, Image XObject invocations, final
  CTM, and the committed VM accounting. Deterministic operand, numeric, state, and budget failures
  therefore precede every image lookup or decode and cannot be replaced by a later resource
  outcome.
- Plan retention charges nested graphics ownership in addition to action-container and copied-name
  capacity. Each `PathResource` allocator capacity transfers into the immutable plan once at path
  handoff; a `FillStroke` action and its pending `Clip` action share that single charge.
  Each `DashPattern` payload is charged once per unique dash ownership across every stroke action,
  remains charged after later dash changes or graphics-state restores, and stays stable while image
  acquisition is `Pending`.
- Image lookup walks only the plan's recorded image invocations; it never rescans the
  `ContentProgram`. Exact proof/decode keys select unique acquisition slots while repeated uses
  retain their own operator and lookup evidence. Unique jobs resume from one exact cursor across
  `Pending`, and successful decoded bytes are copied into bounded Scene image resources only after
  aggregate decoded-byte, cache-retention, probe, poll, and allocation checks pass.
- After all required images are Ready, one Scene materialization pass consumes the immutable plan.
  It does not repeat VM semantics, source lookup, or image polling. A failed, unsupported,
  cancelled, source-changed, or resource-limited outcome drops the plan/cache/builder and never
  publishes a partial Scene.
- The initial state profile supports `q`/`Q`, `cm`, `BT`/`ET`, `BX`/`EX`, `BMC`, name-based `BDC`,
  and `EMC`. Graphics saves retain the current CTM; `cm` applies the PDF prepend rule as
  `current × operand` in Scene's column-matrix representation; text objects
  cannot nest; compatibility and marked-content depths are bounded and terminally balanced.
  Terminal balance errors are selected in graphics, text, compatibility, then marked-content
  order.
- Unknown operators are ignored only while a compatibility section is active. `MP` and `DP`
  validate their declared operand shapes and then return structured Unsupported even inside
  compatibility sections. A direct `BDC` property dictionary is also structured Unsupported.
  Name-based `BDC` preflights property-use and VM retention before invoking the document resolver.
  Unsupported indirect `/Properties` and direct selected property dictionaries preserve the lower
  document error; invalid or duplicate resource syntax remains a document failure.
- Scene construction emits the registered marked-content, path, clip, paint, and basic image
  commands. Command provenance uses the exact stream object, stream ordinal, decoded operator
  span, and page-global ordinal. Repeated property, path, and image targets follow Scene's stable
  first-command-use interning while every `BDC` and `Do` retains its own resolved proof.
- Independent VM limits cover operators, deterministic fuel, graphics depth, compatibility depth,
  marked-content depth, property/image-use counts, graphics/path/dash state, action-plan capacity,
  and VM retention. Independent image limits cover planning operators, lookup/acquisition polls,
  cache probes, unique images, aggregate decoded bytes, plan/cache retention, and allocations.
  Acquired content, lower document jobs, decoded payloads, and Scene capacities remain governed by
  their sealed budgets and are not counted again.
- `ContentVmPoll::Pending` exposes only the lower proof-bound acquisition ticket, missing ranges,
  and checkpoint. Once semantic planning succeeds the scanned program is dropped; repeated polls
  retain only the immutable plan, exact cache, active acquisition, and cursor without repeating
  completed work. Ready, Unsupported, and Failed outcomes replay exactly without source polling or
  additional work. Only Ready owns a Scene; every other path drops unpublished state.
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
- Real strict-open fixtures covering Page indexing, inherited materialization, content acquisition,
  and sealed VM interpretation; successful state execution; empty Pages; matrix composition;
  every state underflow and imbalance; declared operand shapes; numeric precision and overflow;
  compatibility behavior; marked-content Scene commands; property proof retention and resource
  interning; invalid, duplicate, and unsupported resource shapes; source/cancellation precedence;
  terminal replay; ownership beyond source lifetimes; and content-redacted Debug output.
- Exact-measured and one-less tests for every VM budget, plus intact scanner, document-resolver,
  and Scene resource failure evidence.
- Graphics-v2 tests cover every registered path, paint, clip, line, color, matrix, `q`/`Q`, and
  `Do` operator; equivalent path/matrix formulations; first-use Scene interning; image
  identity/Flate acquisition; duplicate exact-cache uses; multiple consecutive Pending outcomes;
  semantic-failure-before-resource ordering; cancellation between distant non-image operators;
  terminal replay; and exact/one-less aggregate image budgets.
- Repository policy checks the approved one-way bytes/document/Scene/syntax dependency boundary,
  test-only strict-fixture dependencies, no external-engine marker, no unsafe block, and no
  filesystem, network, or process API in product sources.

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
- Text showing, shadings, inline images, Form XObjects, fonts, masks, patterns, advanced color
  spaces, and transparency groups remain outside this bounded profile. Unsupported Image
  XObject filters, masks, decode arrays, interpolation modes, color spaces, and bit depths retain
  structured document/content capability outcomes rather than fabricated pixels.
- `MP` and `DP` are registered structured Unsupported outcomes. Direct `BDC` property dictionaries,
  indirect Page `/Properties` dictionaries, and direct selected property dictionaries are also
  outside this bounded profile.
- Only indirect references selected from a direct `/Properties` dictionary are admitted. The VM
  retains their syntax/provenance proof and Scene resource identity but deliberately does not open
  or interpret the target object.

# History

- 2026-07-16: Added the pure bounded M2-05 decoded-stream scanner, owned direct operands, initial
  operator table, exact decoded provenance, structured malformed/unknown separation, independent
  budgets, cooperative cancellation, atomic terminal replay, and deterministic boundary tests.
- 2026-07-16: Added the sealed M2-06 acquired-Page interpreter, exact fixed-point CTM execution,
  bounded state stacks, inherited marked-content property proofs, semantic Scene production,
  structured Unsupported outcomes, lower-error preservation, independent VM budgets, atomic
  terminal replay, and real strict-pipeline integration tests.
- 2026-07-16: Added the M3 graphics-v2 path, clip, paint, line, and device-color VM with complete
  graphics-state restoration, checked fixed-point conversion, exact command provenance, and
  bounded Scene-v2 publication.
- 2026-07-16: Added proof-bound basic Image XObjects through one immutable semantic execution
  plan, exact-key resumable acquisition, single Scene materialization, aggregate image/cache
  limits, deterministic Pending replay, and semantic-failure-before-resource ordering.
