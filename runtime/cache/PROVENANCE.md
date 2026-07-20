# Scope

`runtime/cache` owns a bounded session-scoped Ready store for successful immutable
`ResolvedReference` values. One logical document actor mutates the store through `&mut self`, and
warm lookup lends `&ResolvedReference`. This slice performs no parsing, I/O, scheduling, in-flight
work coalescing, persistence, or cross-session sharing.

# Semantic owner

`runtime/cache` owns cache-key, admission, accounting, and eviction semantics. The
`ReadySessionOwner` in `runtime/session` owns each Ready-store instance and drops it during its
synchronous close boundary; a future complete session actor must compose that owner with every
other session resource. Platform code may supply validated configuration but does not own cache
semantics. `pdf-rs/document` owns proof-bearing values, immutable revision identity, parser profiles,
resolution profiles, and value-owned footprint evidence. Complete keys and deterministic eviction
remain cache-owned; the cache does not weaken or reconstruct document proof.

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

# External observations and dependencies

No PDFium, external engine, third-party implementation source, or external output was used. Product
dependencies are the in-repository bytes, document, object, and syntax crates. The xref crate is a
test-only dependency used to assemble project-authored structural fixtures.

# Known deviations

- This is a session-only borrowed Ready store, not a persistent, process-wide, disk, or cross-session
  cache. It has no security-domain eligibility for decrypted values and makes no persistence claim.
- It caches only successful `ResolvedReference` values. It does not cache failures, standalone
  `AttestedObject` values, stream payloads, byte-source storage, decoded resources, Scene, text,
  tiles, or GPU objects.
- It does not coalesce `Resolving` work, retain subscribers, arbitrate completion/cancel/close races,
  own a parent Worker/Session budget hierarchy, or provide concurrent shards. Those belong to later
  complete session/request, scheduler, and budget slices.
- Its current eviction policy is strict LRU. The section 9.4 small-object retention preference and
  segmented policies require corpus evidence and remain a later multi-level-cache decision.
- Hard ceilings and defaults are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision.

# History

- 2026-07-14: Added complete session binding and keys, fixed metadata preallocation, borrowed Ready
  hits, move-preserving admission rejection, scoped budget evidence, checked resident accounting,
  cancellation-aware linear LRU planning, and explicit clear/drop ownership boundaries.
- 2026-07-14: Bound the drop boundary to `runtime/session::ReadySessionOwner` and exercised it in
  the canonical Native quality loop without changing cache-owned admission or eviction semantics.
