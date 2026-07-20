# Skia subsystem boundary

`skia/` is an independently developed 2D graphics subsystem and reusable
library. It owns portable geometry, paths, paints, image resources and codecs,
text-glyph drawing contracts, display lists, and CPU/GPU execution. It is **not** an
implementation detail of PDF.rs and it does not model PDF operators or objects.

```mermaid
flowchart LR
  pdf["PDF.rs adapter"] --> api["Skia public API\npdf-rs-skia facade"]
  other["Other library adapters"] --> api
  api --> geometry["Geometry"]
  api --> path["Path"]
  api --> image["Image resources"]
  api --> codec["Image codecs"]
  api --> core["Skia core semantics"]
  api --> cpu["Skia CPU executor"]
  api --> gpu["Skia GPU executor"]
```

## Dependency rule

- `skia/` (`pdf-rs-skia`) is the only public graphics API for consumers.
  `skia/error`, `skia/geometry`, `skia/path`, `skia/core`, `skia/image`, `skia/codec`, and
  executor crates are implementation crates;
  consumers must not depend on them directly. Skia crates may depend on each
  other, but never on a PDF.rs document crate or PDF semantic type.
- The facade exports an explicit, stable set of canvas, geometry, paint, path,
  image, text-outline, and error types. It does not expose display-list
  resource IDs, command representations, or backend command encoders.
- `skia/error` contains shared failure types; `skia/geometry` contains fixed
  point coordinates and affine transforms; `skia/path` contains immutable
  paths and path construction. Their dependencies flow only downward.
- `skia/core` contains paint and backend-neutral display-list semantics. It
  depends on the foundational crates but never on an executor, platform
  graphics API, PDF parser, document model, or Scene.
- `skia/image` owns the immutable RGBA8 resource representation. `skia/codec`
  translates encoded, general-purpose image bytes into that representation; it
  does not depend on rendering backends or PDF types. Future encoders belong in
  `skia/codec` as well, not in the resource crate.
- Every consumer, including PDF.rs, calls Skia only through its public API.
  Each consumer owns its source-domain adapter and reports its rendering
  intent, target description, and source data to the Skia upper integration
  layer. That layer owns resource lifetime and executor selection before
  calling lower Skia components.
- A Skia public type, method, error, or command must not mention PDF objects,
  operators, page state, or PDF-specific policy. Add an adapter in PDF.rs when
  such translation is required.

## Geometry and transforms

Paths are immutable geometry resources. `PathBuilder` constructs paths from
generic 2D primitives; it must not encode PDF path or graphics-state rules.
Canvas and display-list transforms are generic affine drawing state that apply
to subsequent drawing operations. They are not PDF `cm` commands. A consumer
that has a source-specific matrix is responsible for mapping it at its adapter
boundary.

Current primitive construction includes rectangles, circles, ellipses, rounded
rectangles, polygons, deterministic cardinal arcs, arbitrary-angle and rotated
ellipse arcs up to one full turn,
quadratic and rational-quadratic Béziers, and cubic Béziers. Paths can be
transformed, appended, reversed, and queried for both conservative
control-point bounds and curve-extrema-aware conservative bounds (with rational
quadratics retaining their control hull). `DisplayList` and the GPU encoder
expose both transform replacement and affine concatenation as generic
graphics-state operations. Boolean path operations, stroke-to-path expansion,
path effects, and tangent-/endpoint-defined arc variants remain separate
geometry-processing work; their design must stay independent of any consumer.

## Path implementation layout

The public `Path` and `PathBuilder` API is implemented in `skia/path/src/lib.rs`.
Algorithm families are split beneath it so construction contracts do not become
coupled to geometry queries or contour processing:

- `arc.rs` owns public ellipse-arc construction and continuation methods.
- `bounds.rs` owns conservative and polynomial-Bézier extrema bounds helpers.
- `reverse.rs` owns contour parsing and reverse traversal.
- `math.rs` owns checked fixed-point scalar operations shared by path code.
