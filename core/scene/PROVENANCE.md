# Scope

`core/scene` owns the immutable Native Scene v1 foundation and the incompatible graphics-capable
Scene v2 schema. It retains bounded page geometry, ordered semantic commands, stable first-use
resources (including project-owned glyph outlines), one-to-one decoded-coordinate command
provenance, explicit capability requirements, validated limit profiles, allocator-reported
retained-capacity accounting, and a bounded content-redacted semantic diff. `SceneBuilder` and
`GraphicsSceneBuilder` are the only public construction paths for their respective schemas.

The crate performs no PDF byte acquisition, object resolution, content scanning, operator
interpretation, resource inheritance, rendering, cache insertion, file or network access, async
scheduling, or external-engine fallback.

# Semantic owner

Graphics/Color owns Scene schema, numeric normalization, immutable command/resource ownership,
stable identifiers, feature reporting, canonical semantic bytes, and Scene-specific budgets.
It also owns positional semantic diff ordering, fixed-size redacted difference records, and
canonical diff bytes.
`core/bytes` owns runtime source identity. `core/syntax` owns `ObjectRef`. Document and content
layers will later validate PDF semantics and supply commands; renderers will only consume the
published immutable Scene.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 6.4-6.7](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a backend-neutral immutable Scene, stable resource identifiers, command provenance,
  capability reporting, deterministic canonical output, and one-way product dependencies.
- [RPE-STD-001, sections 5-9 and 14](../../docs/standards/coding-standard.md) requires checked
  arithmetic, explicit NaN/infinity/negative-zero policy, bounded allocation, deterministic
  output, redacted diagnostics, and documented canonicalization.
- [RPE-STD-003, sections 7-8](../../docs/standards/testing-standard.md) requires stable Scene
  ordering and IDs without allocator, path, time, thread, or environment noise.
- [RPE-STD-005, sections 4-6 and 11](../../docs/standards/security-and-resource-budget.md) requires
  pre-work command/resource/depth/memory budgets and immutable Scene publication without partial
  success.

The separately registered `m2.scene-v1.v1` and `m2.scene-semantic-diff.v1` profiles own the
bounded M2-04 foundation only. They do not claim an ISO 32000 conformance profile, complete
sections 6.4-6.7, ownership of Content VM production, renderer integration, or ownership of the
separate M2 normative Scene gate.

# Traceability profile boundaries

- `m2.scene-v1.v1` owns immutable Scene schema, exact fixed-point page geometry, bounded
  begin/end marked-content commands, first-use marked-content-property resource IDs, supported
  feature tags, one-to-one command provenance, allocator-reported retained capacity, and
  source-identity-free canonical Scene JSON.
- `m2.scene-semantic-diff.v1` independently owns fixed-order semantic comparison, fixed-size
  content-redacted changed/added/removed records, complete-result difference and retained-capacity
  admission, and canonical Scene-diff JSON under a separate output ceiling.
- `m3.scene-graphics-v2.v1` owns the incompatible bounded path/paint/clip/image/glyph/group command
  schema. Its glyph slice retains exact font/decode source identity, font-local glyph ID,
  units-per-em, deterministic outline geometry, positioned character codes and glyph-to-page
  transforms; it does not own PDF text semantics, font parsing, shaping, hinting, or rasterization.
- Both profiles remain `PLANNED` maturity records even though plan item M2-04 is complete.
  Completion means the bounded implementation, normative empty/structural/invalid/budget tests,
  provenance, and repository registration are present; it does not promote either capability to
  REFERENCE. The bounded M2 milestone closes only through the separately owned M2-06 Content VM
  producer and M2-07 normative Scene gate, and milestone completion is not a maturity promotion.

# Algorithms and derivations

- `SceneScalar` is a signed nine-decimal fixed-point integer. Decimal construction accepts no
  exponent or ignored whitespace, rejects more than nine fractional digits, normalizes every
  negative-zero spelling to zero, and rejects values outside the exact `i64` scaled range. Scene
  values therefore cannot contain NaN or infinity.
- Matrix multiplication uses checked `i128` products and sums. Each output component rounds once
  after its complete product sum, with exact half units rounded away from zero. Overflow is a
  structured numeric failure rather than saturation or platform-dependent floating behavior.
  Singular matrices remain valid PDF semantics and are retained unchanged; this foundation does
  not expose matrix inversion.
- `PathResourceBuilder::try_push_quadratic` converts one exact quadratic segment to one cubic
  segment without floating point. Each cubic control component is derived as
  `(endpoint + 2 * quadratic_control) / 3` in checked integer fixed-point arithmetic, rounded to
  nearest with exact ties away from zero. Content therefore hands TrueType quadratic outlines to
  Scene through one deterministic, platform-independent representation.
- `SceneRect` admits only positive-area `[left, bottom, right, top]` coordinates whose width and
  height are representable `SceneScalar` values. `PageGeometry` separately retains MediaBox,
  CropBox, and one canonical quarter-turn rotation.
- Marked-content properties are interned only when their first semantic command is appended.
  Resource IDs are the zero-based first-use order, independent of allocator addresses or map
  iteration. Repeated use of the same exact defining `ObjectRef` reuses the existing ID.
- Every command append reserves both the command and its `CommandSource` slot before publication.
  The builder first rejects any append whose minimum next retained footprint already exceeds the
  Scene budget, then grows retained vectors in checked geometric steps and checks their actual
  allocator-reported capacities before semantic mutation. Final Scene publication defensively
  rechecks one-to-one pairing, resource IDs, resource references, command balance, depth, count,
  name, and retained-capacity limits. An unclosed, underflowed, or over-budget marked-content
  sequence never publishes a partial Scene.
- Graphics-v2 glyph append is one transaction. Before any command/resource/stat publication it
  admits the combined live peak of the persistent builder, caller glyph storage and outline
  backing during indexing, requested and pending resource tables, resource IDs, capability input,
  auto-generated requirements, positioned-glyph storage, and every simultaneously live
  replacement vector. Planned capacities are checked before reserve and allocator-reported actual
  capacities are checked immediately afterward. After the command callback consumes caller glyph
  storage, only the interned pending outline payload remains charged. Equal glyph-outline values,
  including values backed by distinct allocations, receive one first-use resource ID while every
  positioned glyph remains in the command. Any one-less retained boundary leaves commands,
  resources, requirements, and glyph counters unpublished. Because the public retained limit also
  bounds this working peak, the minimum successful transaction limit can exceed the final
  `GraphicsSceneStats::retained_bytes` value; that statistic reports published ownership only.
- A fallibly reserved builder-only index stores `(ObjectRef, ResourceId)` entries in `ObjectRef`
  order. Hand-written binary search gives deterministic logarithmic lookup while resource IDs
  remain the zero-based first-command-use order in the separate resource vector. Each lookup
  charges its deterministic worst-case binary-search comparison bound, and each entry shifted by
  ordered insertion consumes one additional unit from the independent
  `max_resource_index_work` budget. This bounds repeated lookups and the index's linear insertion
  path without relying on randomized hashing or hidden tree allocation.
- Charged construction retention combines allocator-visible command, resource, resource-index,
  provenance, and decoded-name storage using actual vector capacities with the exact final
  feature-tag slot requirement. The resource index is dropped before immutable Scene publication,
  so published `retained_bytes` covers only command, resource, provenance, feature-tag, and
  decoded-name storage. Inline headers, `Arc` control blocks, allocator metadata, document bytes,
  and runtime caches are outside this metric.
- Canonical JSON uses fixed lexical field order, declared semantic array order, lowercase
  hexadecimal PDF name bytes, and scaled decimal integers. The runtime `SourceIdentity` is
  deliberately omitted; canonical binding retains only page index, exact Page object, and revision
  anchor. No platform floating formatter, environment path, timestamp, thread identity, pointer,
  or allocator order enters the output.
- Canonical bytes have an independent checked output ceiling and fallible allocation policy.
  The writer grows in bounded geometric steps and emits hexadecimal payloads in fixed 256-byte
  input chunks, avoiding byte-by-byte reallocation. The observed canonical API invokes a
  caller-owned observer before each bounded output fragment and publishes no byte vector if the
  observer interrupts; the original API uses the same writer without an observer and remains
  byte-identical. Canonical serialization never mutates the Scene.
- `compare_scenes` ignores only runtime `SourceIdentity`; it compares schema major/minor, page
  index, exact Page object, revision anchor, geometry, feature decision and ordered tags, stable
  resources, semantic commands, and their paired provenance. Scalar fields are visited in fixed
  schema order. Ordered sections compare shared positions as changed and represent only trailing
  length imbalance as ascending added or removed records.
- Every `SceneDifference` is a fixed eight-byte enum/index record with no names, object values,
  source digest, or document bytes. Comparison first counts the complete result under
  `max_differences`, then admits and fallibly reserves the complete fixed-size record capacity
  under `max_retained_bytes`, and only then publishes the immutable diff. Exceeding either limit
  is a structured `ResourceLimit`; differences are never silently truncated.
- Canonical Scene-diff JSON emits only field, index, relationship, section, schema, and aggregate
  counts in fixed order. It has an independent `max_canonical_bytes` ceiling and uses the same
  checked writer policy as canonical Scene JSON.

# Tests

- Exact empty-Scene canonical JSON and stable schema/field order.
- Exact populated-Scene canonical JSON covering command, resource, feature, provenance, and nested
  object field order.
- Canonical equality across distinct runtime source identities.
- Byte-identical observed/unobserved canonical output and observer interruption without
  publication.
- Stable first-use resource IDs, duplicate reuse, feature tags, raw-name hexadecimal encoding,
  and repeat-build determinism.
- Negative-zero normalization, exact extrema, precision rejection, syntax rejection,
  half-away-from-zero arithmetic, checked add/subtract overflow, matrix identity/composition,
  singular-matrix retention, and numeric overflow.
- Invalid geometry, marked-content underflow/unclosed state, command/depth/name/resource/retained
  limits, pre-allocation retained-footprint rejection without partial mutation, canonical-byte
  limits, command/provenance pairing, and content-redacted Debug output.
- Repository policy checks for dependency direction, product I/O exclusion, no external engines,
  and canonical-source identity omission.
- Deterministic quadratic-to-cubic conversion, repeated-glyph interning across shared and distinct
  outline backing, exact combined transaction-retention admission, one-less rejection, failure
  atomicity, and post-failure glyph-counter reuse.
- Source-identity-noise equality, schema/binding/geometry/feature/resource/command/provenance
  section coverage, stable changed/added/removed order, exact canonical diff golden bytes, every
  zero limit, one-less difference/retention/canonical budgets, fixed record size, redacted Debug,
  and repeat-build determinism. The exact golden is profile-independent and is exercised by each
  supported debug or release test invocation.

# Known deviations and unsupported cases

- Scene v1 remains the deliberately narrow begin/end marked-content schema. Scene v2 adds bounded
  paths, clips, graphics state, basic images, positioned embedded-glyph outlines, device color,
  blend/alpha requirements, and isolated groups. PDF text interpretation, font acquisition and
  parsing, shaping, hinting, system fonts, optional content, spatial indexes, and renderer adapters
  remain outside this crate.
- The builder-only resource lookup index is transient and therefore absent from the published
  Scene `retained_bytes` statistic, but its actual vector capacity is charged while the builder is
  live and is exposed by `SceneBuilder::retained_bytes` for a future combined Content VM state
  budget.
- `FeatureReport` currently publishes only supported Stage A tags. Structured unsupported
  requirements and compatibility warnings arrive with the Content VM capability boundary.
- Source identity remains runtime metadata on `SceneBinding` but is not serialized. A future cache
  key must combine runtime binding with a hash of canonical semantic bytes; canonical bytes alone
  are not a cross-document authorization token.
- Canonical Scene and Scene-diff JSON are write-only in this slice. Schema parsing, minor-version
  skipping, IPC framing, and `tools/compare` integration remain future work beyond bounded M2.
- Scene v1 and its semantic diff do not close the M2 exit gate by themselves. The separately owned
  M2-05 content-stream acquisition, M2-06 Content VM producer, and M2-07 registered normative
  Scene gate now close the bounded M2 milestone while every Scene and quality feature remains
  `PLANNED`.

# History

- 2026-07-17: Added bounded canonical-output observation so upper product layers can cooperatively
  interrupt long Scene serialization without changing canonical bytes or dependency direction.
- 2026-07-16: Added the Scene-v2 glyph handoff with deterministic quadratic-to-cubic conversion,
  first-use outline interning, positioned glyph runs, combined transient/final retained admission,
  and exact one-less atomicity coverage for shared and distinct outline backing.
- 2026-07-16: Replaced the builder-only hash table with a deterministic ordered resource index,
  charged its actual vector capacity to retained memory, and bounded comparison and insertion
  work independently.
- 2026-07-16: Added immutable bounded Scene v1 Stage A with fixed-point geometry, semantic
  marked-content commands, first-use resource IDs, paired provenance, feature reporting, and
  deterministic source-identity-free canonical JSON.
- 2026-07-16: Added bounded Scene v1 Stage B semantic comparison with fixed-size redacted records,
  stable section order, structured difference/retention/output limits, and exact canonical diff
  JSON.
- 2026-07-16: Completed the bounded M2-04 foundation by registering independent Scene v1 and
  semantic-diff profiles, executable normative case families, exact scope exclusions, and the
  then-open Content VM and M2 exit-gate dependencies.
- 2026-07-16: Closed the bounded M2 milestone through the separately owned Content VM and
  profile-stable normative Scene gate without promoting the Scene profiles beyond `PLANNED`.
