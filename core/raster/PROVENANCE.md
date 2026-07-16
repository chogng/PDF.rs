# Scope

`core/raster` is the first pure Native Reference pixel foundation. It consumes one immutable
`pdf-rs-scene::Scene`, explicitly traverses the current non-painting marked-content command
subset, and atomically publishes a value-only `CanonicalPixelBuffer`.

The initial output profile is top-down, opaque `sRGB-reference-v1`, straight-alpha RGBA8. This
label freezes only the pixel-value encoding and white empty-page behavior. It is not the final
`reference-raster-v1` algorithm: page-to-device mapping, curve flattening, edge construction,
coverage, clipping, antialiasing, compositing, color conversion, glyphs, and images remain open.

`CanonicalPixelBuffer` is not the worker/session-owned transferable `Surface` lifecycle. It owns
no `SurfaceId`, generation, epoch, acquire, transfer, release, timeout, shared memory, texture, or
platform handle. Those runtime and platform contracts belong to M4.

# Semantic owner

Graphics/Color owns the Reference output encoding, checked device-pixel geometry, deterministic
command-plus-pixel fuel, cooperative cancellation schedule, pixel allocation and retention
budgets, atomic publication, and terminal replay. `core/scene` owns immutable semantic commands,
resources, provenance, features, page geometry, and runtime source/page binding.

The product dependency is one-way from raster to Scene. Test-only dependencies on bytes and syntax
construct public Scene fixtures; they do not enter the product graph.

# Normative sources

- [RPE-ARCH-001, sections 6.4-6.7 and 8.1-8.3](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a project-owned deterministic Reference path consuming immutable Scene values, fixed
  output encoding, bounded checked work, and separation from platform graphics stacks.
- [RPE-STD-001, sections 3 and 5-10](../../docs/standards/coding-standard.md) requires one-way
  dependencies, checked arithmetic, fallible bounded allocation, deterministic output, and
  content-redacted diagnostics.
- [RPE-STD-003, sections 8-9 and 12](../../docs/standards/testing-standard.md) requires exact
  canonical pixels for self-authored Reference fixtures, boundary tests, deterministic replay,
  cancellation checks, and no automatic golden replacement.
- [RPE-STD-005, sections 4-7 and 11](../../docs/standards/security-and-resource-budget.md) requires
  independent dimension, pixel, work, retained-memory, and cancellation limits with no partial
  surface publication.

# Traceability profile boundaries

`m3.reference-pixel-foundation.v1` is registered as `PLANNED`. M3-01 completion means the bounded
value contract, current non-painting Scene consumer, resource policy, tests, product-purity
closure, and independent implementation review are present.

It is not a `REFERENCE` maturity promotion; it is not an O0/O1 pixel authority.
It is not the final `reference-raster-v1` algorithm or the M3 exit gate.

# Algorithms and derivations

- `ReferenceRenderConfig::opaque_srgb` accepts positive caller-selected pixel dimensions. M3-01
  deliberately does not derive those dimensions from Scene page geometry, zoom, rotation, crop,
  or device scale.
- One pixel is exactly four bytes in red, green, blue, alpha order. Rows are top-down, stride is
  exactly `width * 4`, and the complete semantic byte count is `stride * height`. Every product is
  checked in `u64`, then checked for host allocation representation.
- The current Scene schema contains only begin/end marked-content commands. The renderer matches
  those variants exhaustively and treats them as semantic non-painting operations. No wildcard
  arm may turn a future visible command into silent blank output.
- A complete empty or marked-content-only page is opaque white: every pixel is
  `[255, 255, 255, 255]`. The builder reserves the complete output before mutation, verifies
  allocator-reported capacity, fills private storage, checks cancellation immediately before
  allocation, once per at most 256 command-plus-pixel work units, and again immediately before
  publication, and only then moves the complete vector into an immutable buffer.
- Independent limits cover width, height, pixels, stride bytes, semantic output bytes, Scene
  commands, deterministic fuel, and allocator-reported retained capacity. Allocation failure is
  reported through a content-redacted resource record.
- `ReferenceRenderJob` consumes and releases its input Scene during the first poll, then retains
  either one immutable `Arc<CanonicalPixelBuffer>` or one copyable structured failure. Later polls
  replay that terminal without consulting cancellation or repeating command traversal,
  allocation, or pixel work.

# Tests

- Exact one-pixel and multi-row opaque-white RGBA output, stride, format, origin, profile, binding,
  and deterministic statistics.
- Empty versus balanced marked-content Scene pixel equality, with explicit command work retained.
- Equal pixel bytes across different runtime `SourceIdentity` values while each output preserves
  its own binding.
- Repeat-run equality, successful terminal `Arc` replay, failed terminal replay, and zero
  post-terminal cancellation work.
- Initial, pre-allocation, fixed-interval, and final-publication cancellation without partial
  output.
- Invalid dimensions and limit profiles; exact and one-less width, height, pixels, stride, output
  bytes, commands, fuel, and retained-capacity admission.
- Pixel-redacted buffer and error `Debug` output.

# Known deviations and unsupported cases

- No visible Scene command is supported yet. Paths, fills, strokes, clips, text, glyphs, images,
  colors, alpha, blend modes, groups, masks, shadings, patterns, bounds indexes, and renderer
  capability graphs remain later M3 work.
- The fixed white output is a value contract for the current non-painting Scene subset, not proof
  that arbitrary pages render successfully.
- The profile has no O0/O1 pixel authority, reviewed O3 golden, fuzz target, benchmark, maturity
  promotion, product Session integration, or M3 exit claim.
- The output label `sRGB-reference-v1` names the canonical byte encoding only. It does not claim
  ICC conformance or freeze the later Reference raster algorithm and renderer epoch.

# History

- 2026-07-16: Added the bounded value-only Reference pixel foundation with explicit non-painting
  Scene traversal, opaque canonical RGBA output, deterministic budgets, cooperative cancellation,
  atomic publication, and terminal replay.
