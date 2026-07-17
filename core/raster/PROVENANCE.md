# Scope

`core/raster` is the pure Native integrated Reference renderer and also owns the Fast CPU
product-tile profile. The Reference path consumes one immutable `pdf-rs-scene::Scene`, preflights
its capability graph and resource table, dispatches the mounted graphics-command subset in source
order, and atomically publishes a value-only `CanonicalPixelBuffer`.

The initial output profile is top-down, opaque `sRGB-reference-v1`, straight-alpha RGBA8.
M3-05 and M3-06 additionally freeze the project-owned `reference-raster-v1` geometry kernel:
page-to-device mapping, checked fixed-point path flattening, 8x8 scalar coverage, registered
stroke construction, and nested clip-mask composition. M3-07 freezes `reference-color-v1` for
DeviceGray, DeviceRGB, DeviceCMYK, constant alpha, premultiplied arithmetic, Normal, Multiply,
Screen, and straight-alpha RGBA8 publication. M3-08 adds `reference-image-v1` basic unmasked image
sampling and M3-09 adds `reference-glyph-v1` embedded-outline painting. M3-10 mounts every one of
those project-owned kernels behind the versioned `reference-raster-v1` dispatch and identity.

`CanonicalPixelBuffer` is not the worker/session-owned transferable `Surface` lifecycle. It owns
no `SurfaceId`, generation, epoch, acquire, transfer, release, timeout, shared memory, texture, or
platform handle. Those runtime and platform contracts belong to M4.

# Semantic owner

Graphics/Color owns the Reference output encoding, checked device-pixel geometry, deterministic
device-color conversion, premultiplied-alpha arithmetic and blending, deterministic nested
preflight/initialization/raster/compositing/conversion fuel,
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
deterministic kernels, independently derived analytic expectations, resource-boundary tests, and
review evidence are present. M3-10 additionally dispatches those kernels through
`ReferenceRenderJob`; the dedicated `reference_geometry_kernel` harness remains as analytic
kernel evidence.

`m3.reference-color-compositing.v1` remains `PLANNED`. M3-07 completion means the fixed
conversion, premultiplication, separable blend, publication-rounding, literal layered-shape,
capability-rejection, and independent review evidence are present. M3-10 uses that same profile
for every mounted path, image, and glyph paint and for final byte publication.

M3-08 and M3-09 likewise do not independently promote the pixel profile. `reference-image-v1`
accepts only already decoded basic unmasked Scene images. `reference-glyph-v1` accepts only
already mapped, positioned, project-owned Scene glyph outlines. M3-10 mounts both without adding
external I/O, font lookup, shaping, hinting, codec fallback, platform antialiasing, or partial
publication.

This is not a `REFERENCE` maturity promotion and not an O0/O1 pixel authority. M3-10 is the
integrated `reference-raster-v1` renderer, while M3-11 still owns final milestone trace closure,
independent review, and the M3 exit decision.

# M4 Fast CPU product-tile profile

`fast-scalar-tiled-v1` is the product renderer introduced by M4-04. It consumes the immutable
`pdf-rs-policy::RenderPlan` unchanged together with the exact immutable Scene used to create that
plan. Construction revalidates the Scene binding, schema, decision cardinalities, plan identity,
tile identities, selected `FastCpu` backend, straight sRGB RGBA8 output, 4x4 coverage, nearest
image sampling, outline glyph sampling, Q16 compositing, and cancellation interval before pixel
allocation. It also reruns the policy evaluator with the decision's exact profile, document
revision, and admitted graph cardinalities, then requires byte-for-byte decision equality. A
different Scene with the same binding and counts therefore cannot be rendered under a foreign
Scene hash or tile identity.

The implementation is independent of the differential target: code under `src/fast` never calls
the Reference render job and never reads or slices a Reference pixel buffer. It owns a separate
page map, fixed-point cubic flattener, 4x4 fill and clip coverage, stroke-space dash and outline
kernel, nearest decoded-image sampler, outline-glyph union, device-color conversion, premultiplied-Q16
compositing, straight-alpha conversion, and halo crop. No platform graphics stack, external PDF
engine, filesystem, network, process, foreign-function, or unsafe dependency enters this path.

Command bounds are mapped conservatively into device coordinates. A two-pass bin builder first
counts every command/tile relationship, admits the complete aggregate, then allocates exact bins
and appends command indices by one outer traversal of Scene commands. Save, restore, and clip
commands are retained for every tile because they affect replay state; visible commands use their
conservative bounds. Every bin therefore preserves source order independent of scheduler or tile
execution order.

The RenderConfig hash retained by `FastRasterIdentity` binds backend, quality, output profile, tile
dimensions, tile halo, antialias mode, curve flatness and recursion, image and glyph sampling,
compositing, and cancellation interval. Implementation labels additionally name the scalar,
clipping, image, glyph, and compositing algorithms. A configuration that selects an unimplemented
profile fails closed instead of silently changing pixels.

Pixels, commands, aggregate bin entries, durable bin/pixel retention, simultaneous private
intermediates, deterministic fuel, and maximum cancellation interval are independently validated
and tested at exact and one-less boundaries. Raster work uses a renderer-owned fuel schedule and
checks cancellation initially, at the configured interval, and immediately before publication.
Each expanded halo surface, clip stack, path, coverage mask, and output vector is fallibly reserved
and admitted using allocator-reported capacity before use. Published tiles retain that already
admitted vector allocation behind a read-only API, avoiding an infallible shrink or copy at the
publication boundary.

`FastRasterJob::render_all` accepts only a complete tile-order permutation. It renders every tile
into job-private storage, crops the halo into a complete exact-length RGBA8 value, performs a final
cancellation probe, and returns the `FastTileSet` only after all tiles succeed. Any invalid
identity, unsupported configuration, resource limit, allocation failure, cancellation, or later
tile failure drops all private storage, so no partial tile set crosses the atomic publication
boundary.

Fast tests compare a literal integer-aligned fixture to independently enumerated RGBA bytes,
differentially bound registered dash, cap, join, miter, hairline, and nonuniform-transform pixels
against the reviewed 8x8 Reference output, exercise clip and decoded-image kernels, prove
whole-page versus tiled composition and tile-order permutation metamorphics, verify source-ordered
bounds bins, replay exact/one-less resource boundaries (including many-subpath geometry), reject
incomplete or duplicate permutations, and demonstrate cancellation from long inner path loops
without publication. These tests are M4-04 implementation evidence; later M4 qualification still
owns the registered corpus differential review, CANARY evidence, and milestone promotion.

# Algorithms and derivations

- `ReferenceRenderConfig::opaque_srgb` accepts positive caller-selected pixel dimensions. M3-01
  deliberately does not derive those dimensions from Scene page geometry, zoom, rotation, crop,
  or device scale.
- One pixel is exactly four bytes in red, green, blue, alpha order. Rows are top-down, stride is
  exactly `width * 4`, and the complete semantic byte count is `stride * height`. Every product is
  checked in `u64`, then checked for host allocation representation.
- Marked-content variants remain exhaustively matched semantic no-ops. Graphics commands are
  independently and exhaustively matched as save, restore, clip, fill, stroke, fill-then-stroke,
  basic image, embedded glyph run, or a structured unsupported group command. No wildcard arm may
  turn a future command into silent blank output.
- A complete empty or marked-content-only page is opaque white: every pixel is
  `[255, 255, 255, 255]`. Visible commands mutate one job-private premultiplied Q16 surface in
  source order. The surface allocation is reserved first, then initialized white in fixed
  256-pixel chunks through renderer-owned fuel and cancellation; empty-page fuel therefore charges
  one unit for initialization and one for final conversion per pixel. The final RGBA vector is
  reserved only after successful dispatch, verified against allocator-reported capacity, filled
  from the private surface, cancellation-checked immediately before publication, and only then
  moved into an immutable buffer. Allocator overcapacity is recorded in actual component and
  simultaneous-working peaks before postflight rejection, while failed output never reports
  published retained bytes.
- Independent aggregate limits cover dimensions, pixels, output bytes, commands, resources,
  requirements and dependency edges; geometry, stroke, clip, image and glyph work; recursion,
  fuel and cancellation; private surface, operation-local coverage/geometry, clip replacements,
  simultaneous working bytes, and published retained capacity. Allocation failure is reported
  through a content-redacted resource record.
- `ReferenceRenderJob` consumes and releases its input Scene during the first poll, then retains
  either one immutable `Arc<CanonicalPixelBuffer>` or one copyable structured failure. Later polls
  replay that terminal without consulting cancellation or repeating command traversal,
  allocation, or pixel work.
- Renderer-owned preflight validates canonical requirement/resource identifiers, backward-only
  dependency order, requirement contexts, resource types, balanced save/restore, every glyph
  outline lookup, the exact supported capability parameters, and every command family before the
  private pixel surface is allocated. Producer `Supported` status is never treated as sufficient
  without the renderer's independent exact-profile decision. After O(1) outer-table count
  admission, a fuelled and cancellation-bounded outer pass aggregates dependency and positioned
  glyph slice lengths before any nested entry is visited; the semantic pass then charges every
  dependency edge and glyph resource lookup individually.
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
  checked. Applying a clip to a caller-owned mask admits the live incoming allocation plus the
  semantic replacement before allocation, then postflights the allocator-reported replacement
  capacity and records that actual peak before any handoff. Cancellation, one-byte-short, and
  allocator-overcapacity failures preserve the prior clip and target-mask state.
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
  their own exact allocator-capacity and peak-retained limits. `peak_geometry_bytes` records the
  greatest old-plus-replacement capacity observed before each transactional handoff.
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
- `reference-image-v1` maps the transformed image unit square through the same Q32.32 page/device
  affine, samples nearest-neighbor texels at the canonical 8x8 positions, converts registered
  DeviceGray/RGB/CMYK bytes through `reference-color-v1`, applies clip/alpha/blend per sample, and
  averages once into a private Q16 result. Singular image transforms are valid no-ops.
- The mounted image form writes into the job-private Q16 surface without cloning or allocating a
  second full-page backdrop. Image source pixels, stride, decoded bytes, samples, conversions,
  fuel, and cancellation are aggregated across commands before final publication.
- `reference-glyph-v1` resolves every positioned glyph identifier against the immutable Scene
  resource table. Its transform order is page-to-device, glyph-to-page, then design units divided
  by `units_per_em`. The result uses the registered Q32.32 1/256-pixel flattener and nonzero 8x8
  fill coverage.
- Glyph outlines in one `GlyphRun` are unioned at the 64-bit sample-mask level before applying the
  run's single paint. This avoids hidden per-glyph alpha accumulation and allocates no temporary
  full-page mask per outline. Clip masks use bitwise intersection with the same sample positions;
  constant paint and Normal/Multiply/Screen compositing use `reference-color-v1`.
- Glyph limits independently admit positioned glyphs, resource lookups, source outline segments,
  flattened segments, edges, samples, mask bytes, output pixels, covered-sample composites,
  geometry bytes, aggregate live retained bytes, curve recursion, geometry fuel, and orchestration
  fuel. After the allocator reports the coverage capacity, the glyph kernel tightens geometry to
  the lesser of its independent limit and the aggregate retained budget remaining after coverage.
  Geometry replacement capacity is admitted and recorded at its transient peak. Aggregate retained
  statistics are the greater of coverage plus peak geometry and coverage plus output pixels, so a
  retained-limit failure is never mislabeled as an independent geometry-limit failure. Both the
  glyph and shared geometry work schedules probe cancellation at fixed fuel intervals. The mounted
  outline-cardinality preflight charges and probes once per positioned glyph without prematurely
  committing glyph, lookup, or outline completed counters. The standalone result reports the
  greater of its exact coverage-plus-geometry and coverage-plus-allocator-reported-pixel working
  stages.
- The mounted glyph form likewise writes into the private Q16 surface and retains only its union
  coverage plus transient geometry. Its child retained budget is the exact global working budget
  remaining after the live surface and clip; the parent records that aggregate once, without
  double-counting geometry.
- Path and clip dispatch use the same combined working admission. Geometry growth, coverage-mask
  allocation, saved-clip copies, incoming masks, and clip replacement storage are checked before
  allocation against the remaining surface-plus-clip working budget. Exact and one-byte-short
  profiles therefore share one deterministic boundary. Each child records its actual simultaneous
  private working peak as allocation events occur; allocator overcapacity is observed before
  postflight rejection. Success and failure both merge that peak and all work already executed by
  private geometry, image, and glyph children—including split glyph orchestration/geometry fuel
  and cancellation probes—before the child is dropped. Mounted children receive the exact remaining
  aggregate allowance, including zero, while standalone kernel constructors retain their strict
  nonzero limit contract; a zero-work child can therefore complete, and the first actual work
  charge rejects an exhausted dimension without a fabricated one-unit allowance. Every child merge
  first validates all checked totals, fuel, and cancellation counts and only then commits the full
  statistics update, so no late field can leave a partial public merge. Public
  `coverage_bytes` is current live transient coverage and is therefore zero after each command or
  terminal unwind; `peak_coverage_bytes` retains the greatest observed mask, while successful clip
  masks remain represented by `clip_bytes` and `peak_clip_bytes`.
- Scene values outside the typed device colors and three blend modes are never coerced into a
  fallback paint. Unsupported color, alpha, blend, mask, or group requirements remain structured
  capability outcomes before the mounted color kernel or pixel publication.

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
- Literal integrated pixels for save/clip/fill/restore source order, two-texel image orientation,
  and embedded glyph runs; mounted stroke/fill-stroke counter coverage; renderer identity and
  successful capability decisions.
- Aggregate requirements, dependency edges, resources, image dimensions, and simultaneous
  working-memory exact/one-less boundaries across mounted path/clip, image, and glyph commands.
- Aggregate nested-length admission before dependency/glyph traversal; fixed-fuel cancellation of
  large outer tables, dependency edges, and repeated glyph lookups; exact terminal replay.
- Empty-surface initialization plus final conversion exact/one-less fuel, mid-initialization
  cancellation, allocator-overcapacity-safe semantic completion, and actual-capacity failure peaks.
- Mid-child cancellation and one-less failure statistics for path, clip, image, and glyph work,
  including exact callback counts, combined glyph fuel error context, zero current coverage, and
  frozen terminal replay.
- Cancellation at the final post-conversion publication probe proves late failure never exposes a
  partial buffer; success and failure terminals replay without additional cancellation or work.
- Exact one- and two-pixel image orientation, transforms, rotations, texel boundaries, clip,
  alpha/blend, singular transforms, conservative admission, every-budget exact/one-less limits,
  and cancellation fixtures.
- Analytic one-em square, half-width, quarter-em, half-sample triangle, two-by-two translation, all
  page rotations, clip, alpha/blend, overlapping-run union, empty-outline, singular-transform,
  invalid-resource, curve-recursion, every-budget exact/one-less, coverage-plus-transient-geometry
  aggregate retention, independent geometry retention, and cancellation glyph fixtures.

# Known deviations and unsupported cases

- `ReferenceRenderJob` supports the exact registered path fill/stroke, nested clip, Device
  Gray/RGB/CMYK, constant alpha, Normal/Multiply/Screen, decoded basic image, and embedded-outline
  glyph command subset. Advanced fonts/text, isolated groups, soft masks, shadings, patterns, and
  bounds-index acceleration remain structured unsupported or later work.
- The profile has no O0/O1 pixel authority, reviewed O3 golden, fuzz target, benchmark, maturity
  promotion, product Session integration, or M3 exit claim.
- The output label `sRGB-reference-v1` names the canonical byte encoding only and does not claim
  ICC conformance. `ReferenceRenderIdentity` separately freezes `reference-raster-v1`,
  `reference-color-v1`, `reference-image-v1`, and `reference-glyph-v1` ownership.

# History

- 2026-07-16: Mounted the complete M3 visible-command subset behind `reference-raster-v1` with
  renderer-owned exhaustive preflight, source-order dispatch, one private Q16 surface, in-place
  image/glyph painting, aggregate limits and statistics, exact combined working-memory admission,
  late-failure atomic publication, and deterministic terminal replay.
- 2026-07-16: Added staged `reference-glyph-v1` project-owned outline lookup, font-unit/glyph/page
  transforms, shared nonzero 8x8 sample-mask union, clip and color compositing, independent work,
  aggregate/transient memory, recursion, and cancellation limits, and analytic glyph fixtures
  without system fonts.
- 2026-07-16: Added staged `reference-image-v1` basic unmasked image mapping, nearest-neighbor 8x8
  sampling, DeviceGray/RGB/CMYK conversion, clip/alpha/blend compositing, and bounded exact tests.
- 2026-07-16: Added staged `reference-color-v1` DeviceGray/RGB/CMYK conversion,
  premultiplied constant alpha, Normal/Multiply/Screen source-over, exact straight RGBA8
  publication, literal layered-shape expectations, and structured unsupported capability tests.
- 2026-07-16: Added the staged `reference-raster-v1` Q32.32 geometry, 8x8 fill coverage,
  registered stroke, and nested clip-mask kernels with deterministic fuel, transient and retained
  memory accounting, cancellation, and analytic boundary tests.
- 2026-07-16: Added the bounded value-only Reference pixel foundation with explicit non-painting
  Scene traversal, opaque canonical RGBA output, deterministic budgets, cooperative cancellation,
  atomic publication, and terminal replay.
