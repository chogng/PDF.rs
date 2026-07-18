# Scope

`runtime/tile-cache` owns the bounded session-scoped cache for successful complete immutable Native
pixel tiles. It retains tiles under the exact policy-produced `TileContentKey` and lends
`&NativeTile`. One logical worker owner mutates the cache through `&mut self`. This crate performs
no parsing, rasterization, I/O, scheduling, persistence, or cross-session sharing.

# Semantic owner

`runtime/tile-cache` owns tile-cache key validation, admission, accounting, eviction, and close
semantics. The Native render worker owns one `TileCache` for an exact
Worker/session/source/revision/renderer-epoch binding and calls its synchronous idempotent close
boundary before publishing zero-current-resource session evidence. `runtime/policy` owns authentic
complete tile-content identities. The cache neither weakens nor reconstructs policy identity.

# Normative sources

- RPE-ARCH-001 sections 5.6, 6.7, 9.4, and 12 require immutable complete cache keys, bounded
  ownership, deterministic eviction, and lifecycle cleanup.
- RPE-STD-001 sections 5, 8, and 11 require checked arithmetic, explicit ownership, and
  deterministic behavior.
- RPE-STD-005 sections 4, 5, 7, and 11 require independent retained-memory limits, cancellation,
  atomic publication, and cleanup evidence.

# M4 complete Native tile cache

- `TileCacheBinding` fixes an opaque Worker/renderer owner, session, immutable source identity,
  product document revision, exact `startxref`, and nonzero renderer epoch. `TileCacheAddress` adds
  the complete authentic policy `TileContentKey`; lookup validates owner/session/source/revision/
  epoch before exact key equality.
- `NativeTile` accepts only the exact tightly packed RGBA8 extent declared by its content key,
  retains private pixel storage, and exposes immutable borrows. Admission rejects incomplete,
  unsupported, cancelled, failed, and source-changed producer outcomes.
- Construction preallocates one fixed entry vector and charges its actual allocator-reported
  metadata capacity. Every resident tile charges its actual pixel-vector capacity. Per-tile,
  aggregate-pixel, and metadata-plus-pixel ceilings use checked arithmetic before publication.
- Entries retain segment membership and least-to-most-recent use order without clocks or counters.
  Pressure removes recent-use entries first, then protected-viewport entries. Exact replacement,
  viewport reclassification, and multi-victim pressure are planned before mutation.
- Close drops pixels and fixed metadata, permanently rejects admission, and returns idempotent
  zero-current-resource evidence. A restarted renderer uses a new binding, so old-epoch keys miss.

# External observations and dependencies

No PDFium, external engine, third-party implementation source, or external output was used.
Product dependencies are the repository-owned bytes and policy crates. Scene and syntax are
test-only dependencies used to build self-authored policy fixtures.

# Known deviations

- The cache is session-only and memory-only. It does not persist pixels or share decrypted,
  session-bound pixels across owners.
- It stores only successful complete Native CPU pixels, never GPU objects or platform handles.
- Eviction has exactly protected-viewport and recent-use segments; it does not predict viewports.
- Hard ceilings and defaults are bootstrap values, not a released `FuelSchedule` decision.

# Change log

- 2026-07-17: Added complete policy-bound Native product tile ownership, exact owner and epoch
  validation, checked metadata and pixel capacity, deterministic protected/recent eviction, atomic
  cancellation, and idempotent close evidence.
- 2026-07-18: Isolated the M4 tile cache from the frozen M1 Ready-store crate so later product
  dependencies cannot alter the M1 fuzz lock or reviewed Ready-store subject.
