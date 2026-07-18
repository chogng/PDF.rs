# Scope

`core/fast-raster` owns the independent Fast CPU product-tile profile. It consumes one immutable
`pdf-rs-policy::RenderPlan` and its exact immutable `pdf-rs-scene::Scene`, and atomically publishes
complete immutable tiles.

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
kernel, nearest decoded-image sampler, outline-glyph union, device-color conversion,
premultiplied-Q16 compositing, straight-alpha conversion, and halo crop. No platform graphics
stack, external PDF engine, filesystem, network, process, foreign-function, or unsafe dependency
enters this path.

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
differentially bind registered dash, cap, join, miter, hairline, and nonuniform-transform pixels
against the reviewed 8x8 Reference output, exercise clip and decoded-image kernels, prove
whole-page versus tiled composition and tile-order permutation metamorphics, verify source-ordered
bounds bins, replay exact/one-less resource boundaries (including many-subpath geometry), reject
incomplete or duplicate permutations, and demonstrate cancellation from long inner path loops
without publication. These tests are M4-04 implementation evidence; later M4 qualification still
owns the registered corpus differential review, CANARY evidence, and milestone promotion.
