# Scope

`runtime/cache` owns two distinct bounded session-scoped stores. `ReadyStore` retains successful
immutable `ResolvedReference` values and lends `&ResolvedReference`. `TileCache` retains only
successful complete immutable Native pixel tiles under the exact policy-produced `TileContentKey`
and lends `&NativeTile`. One logical owner mutates either store through `&mut self`. This slice
performs no parsing, rasterization, I/O, scheduling, in-flight work coalescing, persistence, or
cross-session sharing.

# Semantic owner

`runtime/cache` owns cache-key, admission, accounting, and eviction semantics. The
`ReadySessionOwner` in `runtime/session` owns each Ready-store instance and drops it during its
synchronous close boundary; a future complete session actor must compose that owner with every
other session resource. The future Native render worker owns one `TileCache` for an exact
Worker/session/source/revision/renderer-epoch binding and must call its synchronous idempotent close
boundary before publishing zero-current-resource session evidence. Platform code may supply
validated configuration but does not own cache semantics. `core/document` owns proof-bearing
values, immutable revision identity, parser profiles, resolution profiles, and value-owned
footprint evidence. `runtime/policy` owns authentic complete tile-content identities. Complete
lookup/admission addresses, deterministic eviction, and immutable pixel ownership remain
cache-owned; the cache neither weakens document proof nor reconstructs a policy identity.

# Normative sources

- RPE-ARCH-001 sections 4.1-4.2, 8.6, and 9.1 place cache ownership in `runtime/cache`, require
  explicit scope and complete keys, and assign mutable object/cache metadata to a single logical
  Document actor.
- RPE-STD-001 sections 5-7 require complete cache keys, explicit state, checked arithmetic, stable
  errors, and ownership without a document-wide lock.
- RPE-STD-002 section 10 requires owner, scope, full key, byte accounting, eviction, cancellation
  policy, and session-close release; eviction may affect performance but not semantics.
- RPE-STD-005 sections 4-9 require pre-allocation checks, true-owner cache charging, checked
  aggregate accounting, bounded collections, and deterministic cancellation/error behavior.

# Algorithms and ownership

- `ReadyStoreBinding` fixes an opaque runtime-issued session identity, the complete immutable source
  snapshot, attested revision identity and `startxref`, object and syntax profiles, and a
  caller-supplied cache-policy/schema epoch. Entry keys add the requested root and exact
  `ReferenceChainLimits`, preventing a warm value produced under a larger cold-path budget from
  bypassing a stricter request. The epoch names the cache namespace; it is not resolver-production
  proof. The future Worker/session registry remains responsible for issuing and not reusing session
  handles; the current Ready-session owner only retains the already-issued identity.
- Construction checks `max_entries * size_of::<Entry>()` against the owner ceiling before calling
  `try_reserve_exact(max_entries)` once. Actual allocator-reported entry-vector capacity is then
  multiplied with checked arithmetic and charged as metadata before the store is published.
  Admissions never grow that vector.
- Metadata capacity already reserves inline `ResolvedReference` slots. Current resident accounting
  therefore adds only syntax heap and reference-path capacity for initialized values, avoiding a
  second charge for their inline representation. The per-value policy ceiling still uses the full
  document footprint.
- Admission validates binding, root, and resolution profile before mutation. Cancellation and
  internal failures return the move-only successful value. Oversized or mismatched values are
  normal policy rejections and also return the value, so cache policy cannot change semantics.
  Size rejections carry the charged session scope, safe exact root, limit, consumed bytes, and
  attempted bytes without document semantic content.
- Exact replacement removes the older entry. Capacity or byte pressure removes least-recently-used
  entries. The vector itself is maintained in exact least-to-most-recently-used order: a borrowed
  hit moves its entry to the tail, admission appends, and eviction uses one cancellation-aware
  linear planning pass followed by one no-fail linear commit. There is no recency counter to
  overflow and no repeated victim search/removal. No wall clock, randomness, hashing, platform
  callback, or async runtime is involved.
- Lookup and admission scan at most the validated entry ceiling and probe cancellation at fixed
  intervals. Lookup returns only a shared borrow; it does not clone, detach, or transfer the
  proof-bearing value. `clear` releases every session value while retaining the already charged
  fixed metadata allocation for reuse. Dropping the store releases both values and metadata;
  `runtime/session::ReadySessionOwner` now performs that drop before its close returns. A future
  complete session may publish `SessionClosed` only after this owner and all other session resources
  have completed close.
- `TileCacheBinding` fixes an opaque Worker/renderer owner, session, immutable source identity,
  product document revision, exact `startxref`, and nonzero renderer epoch. `TileCacheAddress` adds
  the complete authentic policy `TileContentKey`; lookup validates owner/session/source/revision/
  epoch before exact key equality. A changed policy field, output field, tile coordinate, render
  configuration, or renderer epoch therefore cannot reuse pixels.
- `NativeTile` accepts only the exact tightly packed RGBA8 extent declared by its content key,
  retains private pixel storage, and exposes immutable borrows. Admission separately rejects
  incomplete, unsupported, cancelled, failed, and source-changed producer outcomes, as well as
  complete pixels under a foreign or stale address. Cache policy never converts a non-success
  terminal outcome into pixels.
- Tile construction preallocates one fixed entry vector and charges its actual allocator-reported
  metadata capacity. Every resident tile charges its actual pixel-vector capacity, not merely
  initialized length. Per-tile, aggregate-pixel, and metadata-plus-pixel ceilings use checked
  arithmetic before a no-fail publication commit.
- Tile entries retain segment membership and least-to-most-recent use order without clocks or
  counters. Pressure removes recent-use entries first, then protected-viewport entries, preserving
  deterministic viewport preference while remaining bounded when every resident tile is
  protected. Successfully admitting a protected tile atomically demotes protected entries from a
  different complete viewport identity. Exact replacement, viewport reclassification, and
  multi-victim pressure are planned before mutation.
- Tile-cache close drops pixels and the fixed metadata allocation, then permanently rejects
  admission. First and repeated close reports both prove zero current entries, metadata, and pixel
  capacity; a repeated call releases zero additional resources. A restarted renderer uses a new
  binding and old-epoch keys deterministically miss.

# External observations and dependencies

No PDFium, external engine, third-party implementation source, or external output was used. Product
dependencies are the in-repository bytes, document, object, policy, and syntax crates. The scene and
xref crates are test-only dependencies used to assemble project-authored policy and structural
fixtures.

# Known deviations

- This is a session-only borrowed Ready store, not a persistent, process-wide, disk, or cross-session
  cache. It has no security-domain eligibility for decrypted values and makes no persistence claim.
- The Ready store caches only successful `ResolvedReference` values. It does not cache failures,
  standalone `AttestedObject` values, stream payloads, byte-source storage, decoded resources,
  Scene, text, or GPU objects. The distinct tile store caches only successful complete Native CPU
  pixels and never GPU objects or platform handles.
- It does not coalesce `Resolving` work, retain subscribers, arbitrate completion/cancel/close races,
  own a parent Worker/Session budget hierarchy, or provide concurrent shards. Those belong to later
  complete session/request, scheduler, and budget slices.
- ReadyStore's current eviction policy is strict LRU. The section 9.4 small-object retention
  preference and additional Ready-cache segments require corpus evidence and remain a later
  multi-level-cache decision.
- Tile eviction has exactly the registered protected-viewport and recent-use segments. It does not
  predict future viewports, persist pixels, or share decrypted/session-bound pixels across owners.
- Hard ceilings and defaults are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.

# History

- 2026-07-14: Added complete session binding and keys, fixed metadata preallocation, borrowed Ready
  hits, move-preserving admission rejection, scoped budget evidence, checked resident accounting,
  cancellation-aware linear LRU planning, and explicit clear/drop ownership boundaries.
- 2026-07-14: Bound the drop boundary to `runtime/session::ReadySessionOwner` and exercised it in
  the canonical Native quality loop without changing cache-owned admission or eviction semantics.
- 2026-07-17: Added the distinct complete Native product tile cache with policy-owned content
  identities, exact owner/session/source/revision/epoch validation, checked metadata and pixel
  capacity, protected/recent deterministic eviction, atomic cancellation, and idempotent close
  evidence.
