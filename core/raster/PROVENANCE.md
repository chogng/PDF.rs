# Scope

`core/raster` is the first pure Native Reference pixel foundation. It consumes one immutable
`pdf-rs-scene::Scene`, explicitly traverses the current non-painting marked-content command
subset, and atomically publishes a value-only `CanonicalPixelBuffer`.

The initial output profile is top-down, opaque `sRGB-reference-v1`, straight-alpha RGBA8.
M3-05 and M3-06 additionally freeze the project-owned `reference-raster-v1` geometry kernel:
page-to-device mapping, checked fixed-point path flattening, 8x8 scalar coverage, registered
stroke construction, and nested clip-mask composition. M3-07 freezes `reference-color-v1` for
DeviceGray, DeviceRGB, DeviceCMYK, constant alpha, premultiplied arithmetic, Normal, Multiply,
Screen, and straight-alpha RGBA8 publication. Glyph, image, and full renderer integration remain
later M3 work.

`CanonicalPixelBuffer` is not the worker/session-owned transferable `Surface` lifecycle. It owns
no `SurfaceId`, generation, epoch, acquire, transfer, release, timeout, shared memory, texture, or
platform handle. Those runtime and platform contracts belong to M4.

# Semantic owner

Graphics/Color owns the Reference output encoding, checked device-pixel geometry, deterministic
device-color conversion, premultiplied-alpha arithmetic and blending, command-plus-pixel fuel,
cooperative cancellation schedule, pixel allocation and retention budgets, atomic publication,
and terminal replay. `core/scene` owns immutable semantic commands, resources, provenance,
features, page geometry, and runtime source/page binding.

The product dependency is one-way from raster to Scene. Test-only dependencies on bytes and syntax
construct public Scene fixtures; they do not enter the product graph.

# Normative sources

- [RPE-ARCH-001, sections 6.4-6.7 and 8.1-8.3](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a project-owned deterministic Reference path consuming immutable Scene values, fixed
  output encoding, bounded checked work, and separation from platform graphics stacks.
- [RPE-STD-001, sections 3 and 5-11](../../docs/standards/coding-standard.md) requires one-way
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

`m3.reference-geometry-coverage.v1` and `m3.reference-stroke-clip.v1` remain `PLANNED`. Their
completion means the deterministic kernels, independently derived analytic expectations,
resource-boundary tests, and review evidence are present. The kernels are compiled through the
dedicated `reference_geometry_kernel` integration harness until M3-10 connects them to
`ReferenceRenderJob`.

`m3.reference-color-compositing.v1` remains `PLANNED`. M3-07 completion means the fixed
conversion, premultiplication, separable blend, publication-rounding, literal layered-shape,
capability-rejection, and independent review evidence are present. The color kernel remains
separate from `ReferenceRenderJob` until M3-10.

This is not a `REFERENCE` maturity promotion and not an O0/O1 pixel authority. It is not the
integrated `reference-raster-v1` renderer or the M3 exit gate.

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
- Geometry uses signed Q32.32 fixed point. Conversion from Scene billionths, matrix products,
  interpolation, averages, and division use checked integer arithmetic with exact half-way cases
  rounded away from zero. Overflow, singular transforms, and invalid geometry fail closed.
- `PageDeviceMap` maps the Scene crop box into caller-selected top-down device dimensions for all
  four canonical page rotations. Crop translation, rotation, scaling, and Y inversion are one
  checked affine; later path transforms compose in a fixed order.
- Cubics use adaptive De Casteljau subdivision with a device-space flatness test, a fixed
  1/256-pixel tolerance, and recursion depth 16. A curve that cannot meet the tolerance within the
  registered depth is rejected rather than silently approximated more coarsely.
- Fill construction ignores horizontal and zero-length edges, closes each segmented subpath, and
  uses a half-open vertex rule. Nonzero winding and even-odd parity share the same fixed edge test.
  Each candidate pixel is sampled at the centers of an 8x8 grid, at offsets
  `(2 * sample + 1) / 16`; its 64-bit sample mask is the canonical scalar coverage value.
- Stroke dashes are resolved in stroke user space, including odd-pattern duplication, normalized
  phase, zero-length entries, closed seams, and degenerate subpaths. Butt, round, and square caps;
  miter, bevel, and round joins; miter fallback; close-path joins; and exact reversals are built
  from checked polygons, circles, and round sectors. A zero-width hairline is exactly one device
  pixel wide before 8x8 coverage sampling.
- Clip state stores the same 64-bit sample masks as fill and stroke coverage. Intersections use
  bitwise AND in save/restore order. Depth, pixels, fuel, current and saved capacities, replacement
  allocations, transient bytes, retained bytes, and peak retained bytes are independently
  checked; cancellation and one-byte-short failures preserve the prior clip state.
- Geometry vectors and the clip save stack use a logical minimum-four, power-of-two capacity
  schedule derived only from logical length. Move fuel, cancellation probes, and one-less work
  boundaries therefore do not depend on allocator overcapacity. Physical `Vec::capacity()` is
  consulted only for transient, retained, and peak byte admission.
- Unknown geometry vectors grow on that logical schedule in private replacement storage.
  Admission accounts for the old allocation and the complete new allocation while both coexist,
  verifies allocator-reported capacity, then subtracts the replaced capacity only after the move.
  `GeometryWork::geometry_bytes` is deliberately a conservative retained upper bound: capacities
  from dropped temporary geometry remain charged for the rest of that render attempt. This can
  reject early but cannot undercount live geometry; coverage and clip masks additionally maintain
  their own exact allocator-capacity and peak-retained limits.
- `reference-color-v1` represents normalized channels as endpoint-inclusive Q16 integers in
  `[0, 65_536]`. Scene `u16` endpoints are converted with nearest rounding; exact half-way
  products and publication conversions round toward positive infinity.
- DeviceGray replicates one channel and DeviceRGB preserves channel order. DeviceCMYK uses the
  frozen additive-black rule `RGB = 1 - min(1, CMY + K)` independently per channel, without ICC
  profiles, platform color management, rendering intents, overprint, or hidden fallback.
- Working pixels are premultiplied project-sRGB Q16 and enforce `color <= alpha`; alpha zero
  canonicalizes hidden color to transparent black. Constant alpha multiplies every premultiplied
  channel and alpha with one rounded Q16 product.
- Normal, Multiply, and Screen use the registered separable source-over equations. Each channel
  constructs one nonnegative Q32 numerator and rounds once to Q16; output alpha is
  `As + Ab * (1 - As)`. The kernels are allocation-free and fixed work per pixel.
- Publication first unpremultiplies nonzero-alpha channels to Q16, then rounds each channel to
  RGBA8. Alpha zero publishes `[0, 0, 0, 0]`; the exact half-intensity Q16 boundary publishes
  eight-bit 128.
- Scene values outside the typed device colors and three blend modes are never coerced into a
  fallback paint. Unsupported color, alpha, blend, mask, or group requirements remain structured
  capability outcomes before the staged color kernel or pixel publication.

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
- Exact crop and rotation mapping for all canonical rotations; affine inverse and transform-order
  checks; repeatable adaptive flattening; recursion, segment, edge, fuel, geometry-byte, and
  cancellation boundaries.
- Independently enumerated 8x8 fill masks for rectangles, a half-pixel triangle, shared extrema,
  reversed subpaths, and nonzero/even-odd divergence.
- Analytic stroke expectations for half-pixel rows, zero-width hairlines, caps, joins, reversals,
  dash phase, zero-length dash entries, closed paths, nonuniform transforms, and independent
  dash/run/primitive/sample/coverage budgets.
- Exact nested clip intersection, save, restore, application, retained-capacity accounting,
  constant-work retained queries, cancellation, depth limits, and one-byte-short transactional
  failures.
- Literal DeviceGray, DeviceRGB, DeviceCMYK, saturation, endpoint, and Scene-unit conversion
  vectors; exhaustive gray/RGB equivalence across all Scene `u16` values.
- Premultiplied invariants, constant-alpha ties, exact unpremultiply/RGBA8 half-up boundaries, and
  transparent-black canonicalization.
- Independent boundary grids for Normal, Multiply, and Screen; literal multi-layer channel
  vectors; commutativity and channel-permutation metamorphics where mathematically applicable.
- An independently enumerated 3x3 layered-shape fixture combining Normal red, Multiply blue, and
  half-alpha Screen green over opaque white with literal final RGBA8 pixels.
- Structured unsupported outcomes for unsupported device-color, constant-alpha, blend, and group
  requirements before visible command dispatch or pixel publication.

# Known deviations and unsupported cases

- No visible Scene command is supported yet by `ReferenceRenderJob`. The path, fill, stroke, and
  clip and color/compositing kernels are frozen and reviewed but intentionally remain outside that
  job until the integrated M3-10 command pipeline. Text, glyphs, images, groups, masks, shadings,
  patterns, bounds indexes, and integrated renderer capability decisions remain later M3 work.
- The fixed white output is a value contract for the current non-painting Scene subset, not proof
  that arbitrary pages render successfully.
- The profile has no O0/O1 pixel authority, reviewed O3 golden, fuzz target, benchmark, maturity
  promotion, product Session integration, or M3 exit claim.
- The output label `sRGB-reference-v1` names the canonical byte encoding only. It does not claim
  ICC conformance or freeze the later Reference raster algorithm and renderer epoch.

# History

- 2026-07-16: Added staged `reference-color-v1` DeviceGray/RGB/CMYK conversion,
  premultiplied constant alpha, Normal/Multiply/Screen source-over, exact straight RGBA8
  publication, literal layered-shape expectations, and structured unsupported capability tests.
- 2026-07-16: Added the staged `reference-raster-v1` Q32.32 geometry, 8x8 fill coverage,
  registered stroke, and nested clip-mask kernels with deterministic fuel, transient and retained
  memory accounting, cancellation, and analytic boundary tests.
- 2026-07-16: Added the bounded value-only Reference pixel foundation with explicit non-painting
  Scene traversal, opaque canonical RGBA output, deterministic budgets, cooperative cancellation,
  atomic publication, and terminal replay.
