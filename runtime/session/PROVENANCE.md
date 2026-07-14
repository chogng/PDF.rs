# Scope

`runtime/session` owns the Ready-state lifetime of one bounded, session-only
`ReadyStore`. The `ReadySessionOwner` is the unique store owner, lends immutable
warm values only through an exclusive actor-style borrow, rejects operations
after close, and synchronously drops retained values plus fixed metadata before
returning an idempotent close report.

This slice begins after a document has reached Ready. It does not implement
Created, Opening, WaitingForData, WaitingForPassword, Failed, asynchronous
Closing, requests, jobs, surfaces, scheduling, IPC, or event publication, and it
does not claim the complete protocol-visible Session state machine.

# Semantic owner

`runtime/session` owns one Ready-store instance and its Ready-to-Closed boundary.
`runtime/cache` continues to own complete keys, admission, byte accounting,
cancellation probes, borrowed hits, and deterministic eviction. A later complete
session actor will own requests and resources beyond this store and may publish
`SessionClosed` only after this owner and every other session resource have
completed close.

# Normative sources

- RPE-ARCH-001 section 9.1 assigns mutable object/cache metadata to one logical
  Document actor.
- RPE-STD-002 sections 2 and 5 require opaque session identity, idempotent close,
  immediate rejection after close, and terminal publication only after resources
  can no longer produce events.
- RPE-STD-002 section 10 requires session close to release session-only cache
  references.
- RPE-STD-005 sections 5 and 7 require true-owner accounting and deterministic
  close/cancellation behavior.

# Algorithms and ownership

- Construction accepts one complete `ReadyStoreBinding`; the owner derives its
  only session identity from that binding and privately constructs the only
  store. It never exposes a store reference, mutable store reference, or store
  extraction API.
- Public phases are Ready and Closed. This synchronous slice has no outstanding
  requests, jobs, surfaces, or callbacks, so the complete protocol's Closing
  drain is represented only as an unobservable internal linearization step.
- Lookup and admission match the owner state before invoking cache cancellation,
  key, or footprint logic. A closed owner therefore always returns the stable
  lifecycle `SessionClosed` result. Admission failures retain the complete lower
  cache error and return ownership of the successful move-only value.
- The first close samples the store's final allocator-capacity accounting, moves
  the unique store out of the Ready state, explicitly drops it, and returns a
  saved report. Repeated close returns the exact same report. `clear` is not used:
  it would retain precharged metadata, while close must release the whole store.
- A borrowed hit is tied to `&mut ReadySessionOwner`, so Rust prevents close while
  the hit remains live. Callers must keep that borrow within one synchronous actor
  turn rather than across an await, callback, or IPC boundary.
- Dropping an owner without explicit close recursively drops an active store as a
  resource-safety fallback. It does not publish `SessionClosed`; future protocol
  code must explicitly close all owners before emitting that terminal event.
- Close-report byte counts are ownership evidence derived from the cache's checked
  allocator-capacity accounting. They are not allocator telemetry, process RSS,
  or proof that an operating system immediately reclaimed physical pages.

# External observations and dependencies

No PDFium, external engine, third-party implementation source, or external output
was used. Product dependencies are the in-repository `runtime/cache` and
`core/document` crates. Bytes, object, syntax, and xref crates are test-only
dependencies used to assemble project-authored structural fixtures.

# Known deviations

- Session identity and session ID allocation, generation validation, and the no-reuse invariant
  within a Worker epoch remain the responsibility of a future Worker/session
  registry; this owner only retains an already-issued opaque identity.
- This slice does not cancel or drain requests, arbitrate late completions, reclaim
  surfaces, close queues, publish events, or enforce a close deadline.
- Parent Worker-to-Session budget reservation, cross-session aggregation,
  persistent or cross-session caches, decrypted-value security domains, stable
  failure caching, in-flight resolution coalescing, concurrent shards, and the
  section 9.4 small-object/multi-level policy remain open.
- Resource reports exclude source storage, stream payloads, allocator metadata,
  and RSS. Registered lifecycle model tests, deterministic scheduler tests, fuzz
  targets, browser/desktop E2E, and registered broad Native/PDFium differential
  evidence remain open before a complete session implementation can claim
  milestone exit.

# History

- 2026-07-14: Added the unique Ready-store owner, close-first lifecycle errors,
  move-preserving admission, zeroed post-close resource snapshots, and synchronous
  idempotent close that drops the complete store before returning.
