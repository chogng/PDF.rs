# Scope

`pdf-rs/bytes` is the first Native product-core slice. It defines immutable source snapshots,
checked non-empty byte ranges, stable source-bound byte slices, synchronous resumable read polling,
and a bounded in-memory Range store. It performs no file, network, callback, scheduler, or
async-runtime I/O. `RangeResumeArbiter` in `runtime/session` is the first product runtime owner of
a private store; that wrapper does not move transport or scheduling semantics into this crate.

# Semantic owner

Parser/Security owns byte-range integrity, snapshot identity, deterministic limits, and source
change behavior. Runtime/Platform owns physical downloads, file access, RTT-aware merging,
scheduling, and requeueing parser jobs after ticket completion.

# Normative sources

- [RPE-ARCH-001, sections 4.3-4.5 and 5.1-5.2](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the bytes module boundary, synchronous `ByteSource` contract, immutable
  `SourceSnapshot`, and Range pause/requeue flow.
- [RPE-STD-001, sections 3-9](../../docs/standards/coding-standard.md) requires one-way core
  dependencies, newtyped identities, explicit state machines, stable errors, checked arithmetic,
  bounded allocation, and async-runtime-free parser code.
- [RPE-STD-002, sections 2 and 7](../../docs/standards/lifecycle-and-concurrency.md) defines source,
  job, ticket, and checkpoint identity plus the one-terminal ticket lifecycle.
- [RPE-STD-005, sections 4-8](../../docs/standards/security-and-resource-budget.md) requires
  deterministic input/read/resident budgets, pre-allocation checks, immutable snapshot validators,
  and `SourceChanged` termination rather than mixed-revision recovery.

This byte-infrastructure module intentionally owns no PDF syntax and therefore makes no ISO 32000
semantic-coverage claim; the separate syntax and traditional-xref modules consume its contract.

# Algorithms and derivations

- `ByteRange` rejects zero length and checks `start + len` before construction. All byte-count to
  platform-size conversions are checked before slicing or allocating. Even while total length is
  unknown, a requested or supplied exclusive end cannot exceed the configured input-byte ceiling.
- A snapshot binds a redacted stable source digest, monotonic revision, optional total length, and
  a hashed strong/frozen/host validator. If total length is initially unknown, the first complete
  response or metadata-only observation may bind an internal observed length exactly once,
  including zero. Pending exact reads that cross the newly known EOF are woken to re-poll, while
  later drift or historical cached bytes beyond that end poison the whole store.
- Cached segments remain sorted, disjoint, and non-adjacent. Equal overlapping responses are
  idempotent; differing overlap is an integrity failure. Adjacent or overlapping responses are
  coalesced only after final cache size, segment count, and peak resident reservations succeed.
- Backing bytes use shared immutable ownership. The full allocator-reported capacity of each host
  response enters atomic resident accounting before waiting on the state mutex, so excess staging
  capacity cannot be retained or queued without charge. Coalescing claims the remaining budget
  before allocation, validates the allocator-reported `Vec` capacity, retains that capacity
  without an infallible shrink, and returns unused reservation before adoption. Declaration-order
  destruction frees each backing before its reservation is returned, and the reservation remains
  held until the final store segment or `ByteSlice` reference drops. Repeated ready polls therefore
  share backing rather than allocate untracked copies.
- Missing ranges are canonical sorted disjoint intervals. Tickets deduplicate equal remaining
  ranges, retain bounded `(JobId, ResumeCheckpoint)` subscriptions, update their missing ranges
  after partial supply, and have one terminal state. Data arrival only returns ticket identities;
  it never calls or resumes parser code under the store lock.
- Source change is published through a one-way atomic flag before state-lock acquisition. Supply
  uses an atomic compare/exchange immediately before either success commit, giving source change
  priority when it was published first. Poisoning moves only pending tickets to `SourceChanged`;
  previously committed Ready, Failed, or Abandoned tickets retain their first terminal state while
  every later poll fails the poisoned session.
- Integrity-class ticket failures poison the complete snapshot. Runtime must take terminal
  subscriptions before releasing a ticket, so no job/checkpoint can be silently discarded.
- The runtime Range arbiter now consumes these public store operations through a private
  snapshot-bound `RangeStore`: it registers the store's `(ticket, JobId, ResumeCheckpoint)`
  subscription together with a runtime generation, supplies bytes only on later actor turns, and
  converts returned terminal ticket identities into one-shot scheduler targets without invoking
  parser code inline. Generation validation remains a scheduler responsibility outside this crate.

# External observations

No external PDF engine output or implementation source was used. The existing PDFium runner is a
separate development-only O4 observer and is not a dependency of this crate.

# Dependencies and generated data

The crate uses only the Rust standard library. It has no normal or development dependency, unsafe
code, generated table, embedded corpus object, filesystem access, network access, or async runtime.

# Tests and fuzz targets

Integration tests cover checked range boundaries, explicit priority rank, response geometry and
redaction, excess-capacity charging, two-part out-of-order supply, canonical holes,
identical/conflicting overlap, metadata-only and response-bound EOF discovery, unknown-length
binding and high-offset rejection,
identity/revision/validator drift, historical bytes beyond a newly observed EOF, ticket sharing,
partial-supply deduplication, checkpoint conflict, unsubscribe/abandon, terminal-state
preservation, integrity poisoning, EOF, read/cache/ticket/subscription/input/resident limits,
zero-copy slice lifetime, resident reclamation, a barrier-controlled supply/source-change
linearization race, and `Send + Sync`.

A repository policy test scans product source for forbidden filesystem, network, async-runtime, and
external-engine tokens and verifies the crate dependency table remains empty. The repository
quality lane drives this store through the runtime arbiter with a generated PDF,
upper-half-before-lower response delivery, exact cancellation, and source-change termination. No
coverage-guided fuzz target or platform/network Range E2E exists in this M1 bootstrap slice.

# Known deviations and unsupported cases

- This is byte infrastructure only. The separate syntax, traditional-xref, and indirect-object
  framing bootstraps consume it, while repair, document services, and Native rendering remain
  unimplemented; no Native/PDFium differential claim is made.
- Physical Range request merging, HTTP validation, local-file identity, retry policy, actual
  scheduler execution, and transport callbacks belong to future runtime/platform integration. The
  current session arbiter covers bounded registration, exact cancellation, and one-shot target
  production only.
- The store retains cached content until drop; it has no eviction policy. Cached and resident bytes
  are bounded. Backing capacity is charged directly, while the store's own segment, ticket,
  subscription, and missing-range allocator metadata remains bounded only indirectly by their count
  limits. The runtime arbiter separately charges its actual registration-vector capacity; that does
  not convert the store's internal metadata into direct byte accounting.
- A job/checkpoint conflict is enforced within one pending ticket. Runtime remains responsible for
  preventing one job from waiting on incompatible checkpoints across distinct tickets.
- The public immutable `SourceSnapshot` keeps `len = None` when length was unknown at session bind;
  a later observed total is internal Range-store validation state, not a snapshot mutation.
- Stable `std` does not expose a safe fallible exact-capacity boxed-slice allocator. Coalescing
  therefore reserves every remaining resident byte before `try_reserve_exact`, validates the
  reported capacity before initialization, and immediately rejects capacity beyond that
  reservation; a platform worker memory limit remains the outer bound for allocator behavior.
- Hard ceilings and the default limits are bootstrap values, not an R0 ReleaseProfile decision.

# History

- 2026-07-13: Added snapshot-bound byte identities, checked ranges, bounded shared backing,
  resumable tickets, source-change poisoning, structured limits, and deterministic integration
  tests.
- 2026-07-14: Recorded the runtime arbiter's private-store consumption, non-inline one-shot requeue
  boundary, and separate registration-versus-source backing accounting without assigning scheduler
  generation validation or transport ownership to `pdf-rs/bytes`.
