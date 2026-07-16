# Scope

`core/scene` is the first immutable Native Scene v1 product foundation. It retains bounded page
geometry, ordered semantic marked-content commands, stable marked-content property resources,
one-to-one decoded-coordinate command provenance, a deterministic feature report, the complete
validated limit profile, allocator-reported retained-capacity accounting, and a bounded
content-redacted semantic diff. A bounded `SceneBuilder` is the only public Scene construction
path.

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

This Stage A slice does not claim an ISO 32000 conformance profile or the M2 Scene gate.

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
- A fallibly reserved builder-only hash index makes exact marked-content-property reuse independent
  of a quadratic resource-table scan. The index is never iterated, serialized, or retained by the
  Scene, so randomized table placement cannot affect first-command-use IDs or canonical bytes.
- Retained capacity covers allocator-visible command, resource, provenance, feature-tag, and
  decoded-name element storage using actual vector capacities. Inline headers, `Arc` control
  blocks, allocator metadata, document bytes, and runtime caches are outside this Scene-owned
  metric.
- Canonical JSON uses fixed lexical field order, declared semantic array order, lowercase
  hexadecimal PDF name bytes, and scaled decimal integers. The runtime `SourceIdentity` is
  deliberately omitted; canonical binding retains only page index, exact Page object, and revision
  anchor. No platform floating formatter, environment path, timestamp, thread identity, pointer,
  or allocator order enters the output.
- Canonical bytes have an independent checked output ceiling and fallible allocation policy.
  The writer grows in bounded geometric steps and reserves an entire hexadecimal name encoding
  before writing it, avoiding fragment-by-fragment or byte-by-byte reallocation. Canonical
  serialization never mutates the Scene.
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
- Source-identity-noise equality, schema/binding/geometry/feature/resource/command/provenance
  section coverage, stable changed/added/removed order, exact canonical diff golden bytes, every
  zero limit, one-less difference/retention/canonical budgets, fixed record size, redacted Debug,
  and repeat-build determinism. The exact golden is profile-independent and is exercised by each
  supported debug or release test invocation.

# Known deviations and unsupported cases

- Scene v1 Stage A contains only begin/end marked-content commands and marked-content properties
  resources. Paths, clips, graphics state, text, glyphs, images, color, transparency, groups,
  optional content, spatial indexes, and renderer adapters remain future work.
- The builder-only resource lookup table is fallibly allocated and count-bounded by
  `max_resources`, but its transient bucket allocation is not part of the published Scene
  `retained_bytes` metric. The future Content VM job budget must charge builder scratch storage
  separately from immutable Scene ownership.
- `FeatureReport` currently publishes only supported Stage A tags. Structured unsupported
  requirements and compatibility warnings arrive with the Content VM capability boundary.
- Source identity remains runtime metadata on `SceneBinding` but is not serialized. A future cache
  key must combine runtime binding with a hash of canonical semantic bytes; canonical bytes alone
  are not a cross-document authorization token.
- Canonical Scene and Scene-diff JSON are write-only in this slice. Schema parsing, minor-version
  skipping, IPC framing, and `tools/compare` integration remain later M2 work.

# History

- 2026-07-16: Added immutable bounded Scene v1 Stage A with fixed-point geometry, semantic
  marked-content commands, first-use resource IDs, paired provenance, feature reporting, and
  deterministic source-identity-free canonical JSON.
- 2026-07-16: Added bounded Scene v1 Stage B semantic comparison with fixed-size redacted records,
  stable section order, structured difference/retention/output limits, and exact canonical diff
  JSON.
