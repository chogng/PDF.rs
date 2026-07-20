# Purpose

`pdf-rs-policy` owns the product `CapabilityDecision`, Native `RenderConfig`, immutable
`RenderPlan`, generation-independent `TileContentKey`, and generation-bound
`PlannedTileIdentity`. It runs after immutable Scene publication and before high-resolution raster
allocation, scheduling, caching, or Surface publication.

# Dependency direction

The product dependency direction is
`runtime/policy -> pdf-rs/bytes + pdf-rs/scene + runtime/protocol`. The protocol dependency is limited
to generated value types, bounds, hash domains, framing helpers, and `fixed_le_v1` encoders; policy
does not depend on protocol transport state or platform adapters. The crate has no product
dependency on `pdf-rs/raster`, platform code, tools, or an external PDF engine.

# Canonical identity

All identities use complete SHA-256 digests. `CapabilityDecisionHash` is computed only from the
generated `CapabilityDecision` projection, and `RenderPlanHash` only from the generated
`RenderPlanManifest` projection. Each uses its generated domain and hash-preimage helper over the
exact generated `fixed_le_v1` payload; no policy-owned field ordering, integer byte order, omitted
wire field, or in-memory layout participates.

Scene, geometry, RenderConfig, tile-content, and planned-tile identities retain the internal
PDF.rs canonical-identity framing: a fixed marker, schema version, distinct type domain,
fixed-width big-endian scalars, explicit collection counts, and canonical order. These internal
identities are never substituted for either protocol hash.

The SHA-256 compression equations, initial values, and round constants are transcribed from
NIST FIPS 180-4. The implementation is local so a product crate does not depend on
`tools/pdf-rs-digest` or an external package. Published FIPS short-vector behavior is covered by
the existing tooling implementation; this crate additionally consumes both generated protocol
hash known-answer records and verifies their exact payload, preimage, and digest. The hash is an
identity primitive, not a PDF security-handler API.

# Capability policy

Profile ID 1, profile version 1, policy version 1 independently implements the registered M3
requirement predicate. It does not call or import the M3 Reference renderer. Evaluation validates
canonical requirement/resource IDs, contexts, dependency order and backward closure, then
evaluates every requirement, parameter, dependency, command, and resource.

Directly unsupported requirements and requirements whose dependency closure is unsupported are
retained in canonical requirement-ID order. Missing requirements, contributors, and structured
locations use independently bounded canonical prefixes with exact totals and explicit
`Complete`/`Truncated` state. The decision-level summary retains zero or one first location, so its
completeness is exact only when that retained count equals the total; per-requirement and
per-contributor locations remain independently bounded by the location-retention limit. Resource
limits and cancellation are `PolicyError` outcomes;
malformed graphs and explicit bounded-wire fanout prohibitions are `Rejected` decisions.
Scene v1 and any other non-graphics schema are explicitly `Rejected` with structured schema,
command, and resource evidence; they are never treated as empty supported pages.

Zero is not a product document revision and is rejected before cancellation polling, graph
walking, canonical allocation, or hashing. Requirement, parameter, and aggregate dependency
cardinalities are likewise admitted before Scene canonicalization. Canonical Scene serialization,
canonical hashing, retained-decision/wire projection, generated `fixed_le_v1` payload encoding,
bounded preimage copying, and final sealing observe the same bounded cooperative-cancellation work
counter, with a final check immediately before publication.

# Render planning

Only an exact `Supported` decision bound to the current Scene may create tiles. The generated
`RenderPlanManifest` binds its plan schema version, document revision, RenderConfig hash, renderer
epoch, nonzero generation/plan ID, Scene/decision/geometry hashes, complete viewport identity
(clip, zoom, DPR, rotation, optional content, and annotation revision), backend, output profile,
quality, row-major Surface regions, and the corresponding complete tile-content hashes. The
generation-independent tile key binds source, source revision, document revision, xref anchor,
page object, Scene, the complete CapabilityDecision policy identity, geometry, clip, zoom, DPR,
rotation, optional content, annotations, tile coordinates, output, RenderConfig, Native backend,
and renderer epoch. Viewport generation is deliberately excluded so identical immutable content
under the same accepted policy can be reused; the enclosing plan and `PlannedTileIdentity` bind
generation explicitly for stale suppression.

All tile sets are checked and emitted in row-major order. Construction, projection, generated
encoding, hashing, and sealing share the configured cancellation interval even at the 1,024-region
wire maximum. Native retry can select only the
`ReferenceCpu` or `FastCpu` enum and must change RenderConfig identity or renderer epoch to count
as a distinct retry.

# Known limitations

- The first profile covers the registered M3 graphics subset only.
- The dependency fanout accepted by a publishable decision is capped at the generated protocol
  maximum of 32 IDs per missing requirement.
- The first output profile is deterministic straight-alpha sRGB RGBA8.
- Render planning defines identities and tile layout; Fast CPU raster, cache, scheduler, Surface,
  desktop transport, and browser Worker execution remain later M4/M5 work items.
- Decoded content-stream offsets are not mislabeled as absolute source offsets, so the first
  profile leaves `CapabilityLocation::source_offset` empty.
