# Scope

`runtime/session` owns one pure bounded Range-request coalescer, three bounded owner slices, one
one-job composition, and one deliberately bounded M1 strict-document actor. `RangeRequestCoalescer`
binds exact resumable requests to one immutable source snapshot and emits deterministic merged host
ranges without performing transport or scheduling. `RangeResumeArbiter`
privately owns one snapshot-bound `RangeStore` and turns namespaced terminal tickets into an ordered
stream of arbiter-bound move-only resume or failure permits without running parser code inline.
`StrictBaseOpenJobOwner` privately owns one generation-bound strict-base opening job and consumes a
permit only after its issuer, ticket, job, checkpoint, and generation match the current suspension.
`StrictBaseOpenCoordinator` privately composes exactly one such arbiter and owner, closes parser
poll-to-registration and completion-to-consumption inside exclusive synchronous actor turns, and
hands Ready out only with the same source owner. `ReadySessionOwner` separately owns the Ready-state
lifetime of one bounded, session-only `ReadyStore` as its unique store owner, lends immutable warm
values through an exclusive borrow, and synchronously drops retained values plus fixed metadata
before returning an idempotent close report. `M1StrictDocumentSession` composes the strict opener,
the same Range owner after Ready, one shared attested-index root, one Ready-store owner, and exactly
one page-count plus one outline job slot. It exposes Created, Opening, WaitingForData, Ready,
Closing, Closed, and Failed; accepts caller-issued request/job/generation identities; and permits
parser work only through one bounded actor turn. Its read-only `M1OpeningParserAudit` reports the
opening job phase, cumulative strict-open statistics, and retained waiting checkpoint without
exposing the job, source, resume permit, or any execution capability.

This is the Session slice needed for the M1 page-count/outline and Range lifecycle exit, not the
complete product Session. It has no password, viewport, rendering, save, surface, generic job
registry, general priority scheduler, worker pool, transport, IPC, or event-publication port.

# Semantic owner

`runtime/session` owns the Range request-to-host-group planning boundary, the Range ticket-to-permit
ownership boundary, one strict-open job execution boundary, their one-job coordinator turn boundary,
one Ready-store instance and its Ready-to-Closed boundary, plus scheduling fairness between the two
fixed M1 service slots. The coalescer decides only deterministic grouping under a caller-supplied gap
threshold and budgets. The bounded actor decides
which of those two jobs may execute and validates their caller-issued identities. A future generic
scheduler and registry still owns arbitrary job kinds, priority classes, long-lived ID allocation,
and Worker-wide arbitration. The platform owns physical source transport.

The repository's loopback HTTP Range harness is test-only host evidence. It consumes pure
coalescer plans, sends strong-ETag `If-Range` requests to a bounded `std::net` fixture, validates
the complete response geometry, and routes exact member bytes back to the actor. It is not linked
into the product library and does not establish a reusable HTTP adapter or product transport owner.

`runtime/cache` continues to own complete keys, admission, byte accounting,
cancellation probes, borrowed hits, and deterministic eviction. A later complete
session actor will own requests and resources beyond this store and may publish
`SessionClosed` only after this owner and every other session resource have
completed close.

# Normative sources

- RPE-ARCH-001 section 9.1 assigns mutable object/cache metadata to one logical
  Document actor.
- RPE-ARCH-001 section 14.2 requires opaque generation-aware handles, out-of-order request
  disposition, idempotent close, and cancellation of active work.
- RPE-ARCH-001 section 15.3 requires M1 Range out-of-order, cancellation, and source-change E2E
  plus page-count and outline foundation services.
- RPE-ARCH-001 sections 5.1-5.2 define the synchronous parser `Pending` boundary and require data
  arrival to requeue rather than resume parser code inline.
- RPE-STD-002 sections 2 and 5 require opaque session identity, idempotent close,
  immediate rejection after close, and terminal publication only after resources
  can no longer produce events.
- RPE-STD-002 sections 6-7 require explicit job, generation, ticket, and checkpoint identity,
  cooperative cancellation, and one terminal disposition for late work.
- RPE-STD-002 section 10 requires session close to release session-only cache
  references.
- RPE-STD-005 sections 5, 7, and 8 require true-owner accounting and deterministic
  close/cancellation behavior.

# Algorithms and ownership

## Range request coalescer

- `RangeRequestCoalescer` is bound to one complete `SourceSnapshot`. Every submitted member carries
  that same snapshot, one opaque runtime request ID, and its complete `ReadRequest`; snapshot
  mismatch fails as `SourceChanged`, known-length overrun and duplicate IDs fail as caller input.
- Planning first admits the request-count and requested-byte budgets, then sorts by checked source
  geometry, descending urgency, and request ID. Output therefore does not depend on arrival order.
  Overlapping ranges always merge. Non-overlapping ranges merge only when their gap is strictly less
  than the caller-supplied threshold; adjacency therefore remains separate at threshold zero.
- Each group retains every exact member request for later ticket/request routing and publishes the
  highest member priority. Groups and members remain in deterministic source order. The planner does
  not issue a host request, complete a `DataTicket`, wake a job, or execute parser work.
- Fixed implementation ceilings bound request, group, and per-group member metadata. Caller budgets
  additionally bound checked aggregate requested bytes and merged bytes, including filled gaps.
  All vectors use fallible reservation, all byte sums and merged lengths are checked in `u64`, and
  cooperative cancellation is probed before admission, per member, after sorting, and per group.

## Range-resume arbiter

- `RangeResumeArbiter` constructs and exclusively retains one `RangeStore`; callers can borrow it
  only as `&dyn ByteSource` for one synchronous job poll. Supply, cancellation, dispatch, source
  change, and close require `&mut self`, so safe Rust prevents those transitions from racing the
  borrowed source inside one actor turn.
- Every returned `Pending` must be registered as the exact ticket plus a scheduler target containing
  `JobId`, `ResumeCheckpoint`, and `RangeResumeGeneration`. Exact re-registration is idempotent; one
  job cannot retain a different ticket, checkpoint, or generation until its prior registration is
  cancelled or dispatched. Registration capacity is bounded by the store profile's total
  subscription ceiling. Every `DataTicket` also carries a private `RangeStore` namespace, so a
  foreign store's same-number ticket cannot alias a local subscription.
- Host `supply` and snapshot observation first let the store settle its tickets, then take their
  complete subscriptions and mark matching registrations ready in deterministic completion order.
  Host `fail_ticket` records one exact ticket-local `SourceUnavailable` terminal outcome and one
  ordered failure permit per matching registration. These ingress methods report queued target
  counts but never invoke a parser, callback, or scheduler inline. `take_completion` removes the
  earliest completion exactly once and returns either an opaque resume permit or an opaque failure
  permit carrying the issuing arbiter identity, completed ticket, job, checkpoint, and captured
  generation. Taking either permit never executes parser work.
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
- `fail_waiting` applies the same exact identity validation to a move-only failure permit. A match
  retains and returns the source error and drops the job without polling the parser or probing
  cancellation; a stale or mismatched failure is consumed without changing parser phase or stats.
- A permitted Pending poll retains the exact ticket and target beside the job. A permitted Ready,
  Failed, or cancellation result drops the job and waiting metadata before publication. This owner
  has no internal queue, priority policy, transport, callbacks, or host I/O.
- Cancellation and source change between actor turns synchronously drop the queued or waiting job
  and return any target the caller must remove from the Range arbiter. Explicit close does the same,
  records exact released job/target counts, exposes zero current resources, and returns the saved
  report idempotently. Late permits after every terminal phase are consumed without parser work.

## Strict-base open coordinator

- Construction privately creates one `RangeResumeArbiter` and one `StrictBaseOpenJobOwner` for the
  same source and one fixed generation. Neither lower owner, byte source, job, nor move-only permit is
  exposed through the public coordinator API.
- Public `run_one` is the only parser entry. Its initial poll and any returned `Pending`
  registration finish before WaitingForData is published. On a later turn it takes at most one
  ordered completion and, when present, consumes the resulting resume or failure permit before
  returning, so host code cannot observe an unregistered suspension or steal a completion between
  lower-owner calls.
- `supply`, `observe_snapshot`, and `fail_data` are parser-free host ingress. Accepted bytes and
  ticket-local failure may queue a scheduler wake without polling parser code; a snapshot-integrity
  mismatch instead performs synchronous fail-closed teardown. In particular, a queued ticket-local
  host failure reaches the exact source terminal without a parser or cancellation probe.
- A successful parser result moves the `AttestedRevisionIndex` and the same private Range source
  owner into one opaque `StrictBaseOpenReady` handoff. The coordinator then reports zero current
  resources; the handoff continues to own cached bytes until it is transferred or its consuming
  `close` returns retained-owner release evidence.
- Cancellation, source change, close, runtime invariant failure, and parser failure each have one
  stable terminal. Duplicate byte supply may be idempotently accepted; foreign or duplicate ticket
  failure and late terminal ingress are classified rejections; snapshot mismatch commits
  SourceChanged. None is an inline parser transition. Terminal reports expose zero current jobs,
  waiting targets, registrations, queued completions, and cached source bytes.

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

## M1 strict-document actor

- Construction validates that the caller-issued opening job identity matches the strict-open
  context, creates the existing coordinator, and remains in Created without polling. `run_one` is
  the only parser entry. Opening `Pending` is registered inside the coordinator before publication;
  source ingress only mutates Range state and may request a later actor wake.
- Ready handoff first derives the complete cache binding from the move-only attested index, then
  consumes that index into a private `SharedAttestedRevisionIndex`. The same Range arbiter is moved
  from opening into Ready. Public code receives neither the arbiter, byte source, candidate index,
  naked attested index, nor a move-only Range permit.
- Ready admits at most one page-count and one outline job. Every request supplies its own
  `M1RequestId`, `JobId`, and `RangeResumeGeneration`; context/job mismatch, stale generation,
  duplicate active request/job identity, and occupied slots are rejected before job construction.
  Exact cancellation matches all three identity fields and removes an uncollected Range
  registration or a privately held completion without a parser poll.
- A service `Pending` is registered with the same arbiter before publication. On later turns the
  actor privately collects at most two terminal permits, validates issuer, ticket, job, checkpoint,
  and generation against retained waiting state, then chooses between runnable slots by strict
  alternating round-robin. One turn polls at most one job. A ticket-local host failure completes
  only its service request; Range integrity or ownership failure terminates the old session.
- `supply`, `observe_snapshot`, `fail_data`, explicit source change, cancellation, and close never
  poll a parser inline. Close ingress only commits Closing. The following actor turn removes service
  jobs and registrations/held permits, closes and drops the cache, drops the shared-index root, and
  finally closes the Range arbiter. Closed and Failed resource snapshots are all zero; repeated
  close returns the saved report.
- Explicit source-change follows the same upper-owner-to-source release order. If the Range owner
  itself detects a snapshot-integrity mismatch while accepting source ingress, it atomically
  poisons and releases its source state before returning the error; the actor then releases every
  remaining service, cache, and index owner. That fail-closed exception claims zero terminal
  resources and no inline parser work, not the normal-path release order.
- Resource reporting counts only true owners: opening/service jobs, waiting targets, held permits,
  Range registrations/backing, Ready-cache entries/bytes, and the private shared-index root handle.
  Shared index heap retained transitively by active jobs is bounded by the already-attested index
  profile but is not allocator telemetry and is not added again to cache/Range byte totals.
- `opening_parser_audit` borrows immutable actor state and returns only value-owned phase,
  cumulative-statistics, and checkpoint evidence while the opening coordinator exists. It neither
  wakes nor polls a job and becomes absent when ownership moves to Ready or a terminal. The
  loopback E2E compares the complete snapshot before and after every host ingress, then requires a
  later explicit `run_one` turn to change it or complete opening.

## Test-only loopback Range host

- The fixture server binds only an ephemeral loopback address, accepts a fixed request count, and
  requires a closed byte Range plus strong `If-Range`. A matching validator returns one exact 206;
  a changed validator returns one complete 200 so the host can report the new snapshot instead of
  supplying bytes under the old identity.
- Response parsing admits at most 16 KiB of headers and 1 MiB of body, checks their aggregate size,
  rejects duplicate required headers, reads exactly `Content-Length` plus EOF, and accepts only a
  strong entity tag. A 206 must carry the requested `Content-Range`, the bound snapshot's exact
  total length, and a matching body length. A 200 must omit `Content-Range` and carry the complete
  expected source length.
- Coalesced response routing uses checked relative geometry and bounds-checked slices for every
  exact member, then supplies members in reverse source order. No HTTP callback holds a parser,
  calls `run_one`, or mints a Range ticket. The test host has fixed timeouts but is not hardened for
  chunked encoding, TLS, authentication, retries, redirects, connection reuse, proxy behavior, or
  platform cancellation.

# Tests

Range-coalescer tests cover arrival-order independence, overlap, strict gap-threshold boundaries,
priority promotion, retained request identities, checked geometry and aggregate overflow, cooperative
cancellation, count/member/byte budgets, known-length bounds, duplicate IDs, and snapshot isolation.
Range-resume component tests cover reverse response and ordered resume/failure completion, private
ticket namespaces, exact idempotent registration, move-only one-shot permits, issuer and
captured-generation evidence, cancellation before and after ticket completion, shared-ticket
cancellation, bounded registration rollback, source-change and close terminal stability, lower
source-error preservation, zero post-terminal resources, and fail-closed subscription mismatch.
Strict-open-owner tests cover the five parser checkpoints, exact resume and failure permit
execution, foreign-arbiter, stale-generation, job, checkpoint, and ticket mismatches, no-poll
rejection, and cancel/source-change/close release. Coordinator tests cover atomic registration and
completion consumption, duplicate/foreign/late ingress, resume/failure/cancellation terminal
precedence, opaque Ready source ownership, exact close release, and zero terminal resources. The
generated quality test separately drives the public coordinator through all five checkpoints and a
host failure without parser or cancellation polling. Ready-owner tests cover admission, borrowed
lookup, close-first lifecycle rejection, resource release, and idempotent close.
M1 actor E2E tests drive generated strict PDF bytes through reverse-order split Range delivery,
page-count/outline two-slot round-robin completion, stale generation and mismatched cancellation,
opening cancellation, source snapshot change, host ticket failure, pending-open close, active
service close, both explicit and Range-detected Ready source change, terminal ingress rejection,
idempotent close, and zero post-terminal resources.
The loopback suite additionally drives coalescer groups through real loopback sockets with strong
ETag/`If-Range`, reverse exact-member supply, and a complete strict-open/page-count/outline actor
path. It proves every ingress leaves the read-only parser audit unchanged until a later actor turn,
rejects a gated real 206 after cancellation, caches one old-snapshot byte before a changed validator
forces `SourceChanged`, and checks all twelve resource fields plus nested release reports. Separate
negative cases reject oversized headers or bodies and malformed, mismatched, or length-inconsistent
`Content-Range` responses.

# External observations and dependencies

No PDFium, external engine, third-party implementation source, external output, or external or
non-loopback network service was used. Product dependencies are the in-repository `core/bytes`, `runtime/cache`, and
`core/document` crates. Object, syntax, and xref crates are test-only dependencies used to assemble
project-authored structural fixtures; the loopback host uses only the Rust standard library and
project-authored bytes.

# Known deviations

- The lower `StrictBaseOpenCoordinator` remains a composition for one parser job, and
  `ReadySessionOwner` remains separate as a reusable component even though the M1 actor owns both.
  A future generic scheduler and registry must provide a generic job queue, registry, priority, fairness, backpressure,
  and cross-job arbitration. This bounded actor does not claim the complete protocol-visible Session state machine.
- Session identity and request/job ID allocation, viewport generations, and the no-reuse invariant
  across completed requests within a Worker epoch remain the responsibility of a future
  Worker/session registry. Opaque session ID allocation is therefore outside this crate. The M1 actor
  validates caller identities but does not mint or persist an unbounded history of them.
- `M1StrictDocumentSession` is intentionally limited to one opening job and two fixed services. It
  does not implement a generic job queue, five-level priority scheduler, worker pool, backpressure,
  cross-session arbitration, transport I/O, host-request submission or cancellation, password flow,
  surfaces, rendering, save, platform queue close, event port, or a close deadline. Its sibling
  coalescer computes bounded host-range groups but does not integrate a transport or become a
  scheduler. The loopback test consumes those groups outside the product crate and therefore does
  not close any of these product-transport or scheduler gaps.
- Parent Worker-to-Session budget reservation, cross-session aggregation,
  persistent or cross-session caches, decrypted-value security domains, stable
  failure caching, in-flight resolution coalescing, concurrent shards, and the
  section 9.4 small-object/multi-level policy remain open.
- Ready-store reports exclude source storage, stream payloads, allocator metadata, and RSS. Range
  reports include source backing capacity and actual registration-vector capacity but exclude the
  RangeStore's internal allocator metadata and RSS. Project-owned registered Native-reference
  evidence for the bounded page-count and outline services now closes the M1 differential gate.
  Broader lifecycle model tests, a generic multi-job generation registry and scheduler, wider fuzz
  campaigns, and browser/desktop E2E remain open, while PDFium stays an unregistered, non-gating O4
  observer. Broader product-Session and rendering gates remain later work; they are not prerequisites
  for the bounded M1 byte/object actor slice itself.

# History

- 2026-07-15: Added a read-only strict-opening parser audit and test-only bounded loopback HTTP
  Range evidence for coalesced reverse delivery, parser-free ingress, late-response cancellation,
  changed-validator teardown with previously cached bytes, exact terminal release accounting, and
  malformed response rejection without claiming a product transport or generic scheduler.
- 2026-07-15: Added a pure snapshot-bound Range request coalescer with strict gap-threshold merging,
  deterministic source ordering, highest-member priority, exact request-ID retention, checked byte
  accounting, fallible bounded metadata, and cooperative cancellation without transport or generic
  scheduling claims.

- 2026-07-15: Added `M1StrictDocumentSession` with Created-to-Failed/Closed lifecycle, caller-owned
  request identities, strict-open-to-Ready proof handoff, the same Range arbiter, one page-count and
  one outline slot, exact permit validation, strict two-slot round-robin, parser-free ingress, and
  ordered explicit close/source-change release plus fail-closed Range-integrity teardown without
  claiming a general scheduler or product Session.
- 2026-07-15: Added the one-job strict-base open coordinator with exclusive actor-turn registration
  and completion consumption, parser-free host ingress with queued accepted completions, opaque Ready source handoff, stable
  cancellation/source-change/close terminals, and generated-PDF success/failure evidence without
  claiming a generic scheduler, complete Session, or M1 exit.
- 2026-07-15: Namespaced `DataTicket` values to their issuing Range store and added ordered
  ticket-local source-failure permits that the strict-open owner consumes without parser or
  cancellation polling.
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
