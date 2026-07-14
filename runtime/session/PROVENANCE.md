# Scope

`runtime/session` owns three bounded actor-style product slices. `RangeResumeArbiter` privately owns
one snapshot-bound `RangeStore` and turns registered terminal tickets into arbiter-bound move-only
permits without running parser code inline. `StrictBaseOpenJobOwner` privately owns one generation-
bound strict-base opening job and consumes a permit only after its issuer, ticket, job, checkpoint,
and generation match the current suspension. `ReadySessionOwner` separately owns the Ready-state
lifetime of one bounded, session-only `ReadyStore` as its unique store owner, lends immutable warm
values through an exclusive borrow, and synchronously drops retained values plus fixed metadata
before returning an idempotent close report.

These slices do not yet form one complete Session. They do not implement the full Created,
Opening, WaitingForData, WaitingForPassword, Ready, Failed, asynchronous Closing, request, job,
surface, scheduler, transport, IPC, or event-publication state machine.
This crate does not claim the complete protocol-visible Session state machine.

# Semantic owner

`runtime/session` owns the Range ticket-to-permit ownership boundary, one strict-open job execution
boundary, plus one Ready-store instance and its Ready-to-Closed boundary. The strict-open owner
retains its current generation and decides whether one consumed permit may execute that job; a
future generic scheduler and registry must own generations and arbitration across all other jobs.
The platform still owns physical source transport.

`runtime/cache` continues to own complete keys, admission, byte accounting,
cancellation probes, borrowed hits, and deterministic eviction. A later complete
session actor will own requests and resources beyond this store and may publish
`SessionClosed` only after this owner and every other session resource have
completed close.

# Normative sources

- RPE-ARCH-001 section 9.1 assigns mutable object/cache metadata to one logical
  Document actor.
- RPE-ARCH-001 sections 5.1-5.2 define the synchronous parser `Pending` boundary and require data
  arrival to requeue rather than resume parser code inline.
- RPE-STD-002 sections 2 and 5 require opaque session identity, idempotent close,
  immediate rejection after close, and terminal publication only after resources
  can no longer produce events.
- RPE-STD-002 sections 6-7 require explicit job, generation, ticket, and checkpoint identity,
  cooperative cancellation, and one terminal disposition for late work.
- RPE-STD-002 section 10 requires session close to release session-only cache
  references.
- RPE-STD-005 sections 5 and 7 require true-owner accounting and deterministic
  close/cancellation behavior.

# Algorithms and ownership

## Range-resume arbiter

- `RangeResumeArbiter` constructs and exclusively retains one `RangeStore`; callers can borrow it
  only as `&dyn ByteSource` for one synchronous job poll. Supply, cancellation, dispatch, source
  change, and close require `&mut self`, so safe Rust prevents those transitions from racing the
  borrowed source inside one actor turn.
- Every returned `Pending` must be registered as the exact ticket plus a scheduler target containing
  `JobId`, `ResumeCheckpoint`, and `RangeResumeGeneration`. Exact re-registration is idempotent; one
  job cannot retain a different ticket, checkpoint, or generation until its prior registration is
  cancelled or dispatched. Registration capacity is bounded by the store profile's total
  subscription ceiling.
- Host `supply` and snapshot observation first let the store settle its tickets, then take their
  complete subscriptions and mark matching registrations ready in deterministic completion order.
  They report queued target counts but never invoke a parser, callback, or scheduler inline.
  `take_requeue` removes the earliest ready registration exactly once and returns an opaque,
  arbiter-bound move-only permit carrying the issuing arbiter identity, completed ticket, job,
  checkpoint, and captured generation. Taking a permit never executes parser work.
- Cancellation matches one exact job and generation. It removes a completed-but-undispatched target
  or unsubscribes only that job from a pending ticket; other subscribers sharing the same ticket
  remain live, while cancelling the sole subscriber abandons and releases the ticket. Repeated
  cancellation is a stable `NotPending` result.
- Source-integrity failure transitions once to `SourceChanged`; subscription inconsistency
  transitions once to `Failed`; explicit close transitions once to `Closed`. Each transition saves
  release evidence, drops the private store and all registrations, exposes zero current resources,
  and preserves the winning terminal on later operations and close.
- Registration storage is fallibly preallocated and charged from its actual vector capacity. Source
  backing and in-flight/coalescing capacity come separately from the store; current and release
  reports expose both components and their checked sum. RangeStore allocator metadata outside
  backing buffers remains indirectly bounded by store count ceilings rather than measured directly.

## Strict-base open job owner

- Construction takes exclusive ownership of one `OpenStrictBaseRevisionJob`, its fixed runtime
  generation, and the only `RangeResumeArbiter` identity allowed to issue resume permits. The job is
  never exposed or extracted.
- Exactly one initial `start` poll is permitted from Queued. Every later poll requires consuming a
  move-only permit and matching its issuing arbiter, completed ticket, job, checkpoint, and
  generation against the current WaitingForData state. A stale or mismatched permit is discarded
  without polling parser code or changing the saved parser phase and cumulative stats.
- A permitted Pending poll retains the exact ticket and target beside the job. A permitted Ready,
  Failed, or cancellation result drops the job and waiting metadata before publication. This owner
  has no internal queue, priority policy, transport, callbacks, or host I/O.
- Cancellation and source change between actor turns synchronously drop the queued or waiting job
  and return any target the caller must remove from the Range arbiter. Explicit close does the same,
  records exact released job/target counts, exposes zero current resources, and returns the saved
  report idempotently. Late permits after every terminal phase are consumed without parser work.

## Ready-store owner

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

# Tests

Range-resume component tests cover reverse response and ticket completion order, exact idempotent
registration, move-only one-shot permits, issuer and captured-generation evidence, cancellation
before and after ticket completion, shared-ticket cancellation, bounded registration rollback,
source-change and close terminal stability, lower source-error preservation, zero post-terminal
resources, and fail-closed subscription mismatch. Strict-open-owner tests cover the five parser
checkpoints, exact permit execution, foreign-arbiter, stale-generation, job, checkpoint, and ticket
mismatches, no-poll rejection, and cancel/source-change/close release. Ready-owner tests cover
admission, borrowed lookup, close-first lifecycle rejection, resource release, and idempotent close.

# External observations and dependencies

No PDFium, external engine, third-party implementation source, or external output
was used. Product dependencies are the in-repository `core/bytes`, `runtime/cache`, and
`core/document` crates. Object, syntax, and xref crates are test-only dependencies used to assemble
project-authored structural fixtures.

# Known deviations

- Session identity, session ID allocation, generations for jobs other than this one strict-open
  owner, viewport generations, and the no-reuse invariant within a Worker epoch remain the
  responsibility of a future Worker/session registry and generic scheduler.
- The Range, strict-open, and Ready owners are not yet joined by a complete Session actor. The
  strict-open slice stores and polls exactly one parser job, but does not implement a generic job
  queue, priority, fairness, backpressure, cross-job arbitration, transport I/O, merged physical
  requests, general request drain, surface reclamation, platform queue close, event publication, or
  a close deadline.
- Parent Worker-to-Session budget reservation, cross-session aggregation,
  persistent or cross-session caches, decrypted-value security domains, stable
  failure caching, in-flight resolution coalescing, concurrent shards, and the
  section 9.4 small-object/multi-level policy remain open.
- Ready-store reports exclude source storage, stream payloads, allocator metadata, and RSS. Range
  reports include source backing capacity and actual registration-vector capacity but exclude the
  RangeStore's internal allocator metadata and RSS. Registered lifecycle model tests, a generic
  multi-job generation registry and scheduler, fuzz targets, browser/desktop E2E, and
  registered broad Native/PDFium differential
  evidence remain open before a complete session implementation can claim
  milestone exit.

# History

- 2026-07-14: Bound Range completion to arbiter-issued move-only permits and added the single-job
  strict-base opening owner with exact issuer/ticket/job/checkpoint/generation validation,
  stale-permit discard, and cancel/source-change/close release without claiming a generic scheduler
  or complete Session.
- 2026-07-14: Added the unique Ready-store owner, close-first lifecycle errors,
  move-preserving admission, zeroed post-close resource snapshots, and synchronous
  idempotent close that drops the complete store before returning.
- 2026-07-14: Added the private-store Range-resume arbiter with exact
  ticket/job/checkpoint/generation registrations, non-inline one-shot requeues, exact shared-ticket
  cancellation, stable source-change and close terminals, and separate registration/source backing
  accounting without claiming a complete Session, scheduler, transport, or M1 exit.
