# Scope

`runtime/protocol` owns the dependency-free Native Engine wire contract consumed by runtime and
platform adapters. The checked-in `src/generated.rs` file is generated from the canonical Engine
schema and is the only source of wire message, identifier, descriptor, and payload definitions.
Handwritten code in this crate validates untrusted desktop frames, handshake compatibility,
correlation shape, sequence order, bounded data-ticket ownership, transfer slots, and Surface
metadata before exposing a payload or platform resource to runtime code.

The crate does not parse PDF syntax, interpret document semantics, allocate platform shared
memory, authenticate an IPC peer, or implement browser or desktop UI behavior. Platform adapters
remain responsible for transporting bytes and handles, authenticating peers, and checking native
handle permissions. The protocol validator checks the schema-visible identity, bounds, slot,
owner, generation, and epoch contract before such an adapter imports a resource.

# Semantic owner

Runtime/Platform owns frame validation, protocol compatibility, correlation, transfer ownership,
and Surface envelope validation. The canonical schema generator owns every generated wire type,
message identifier, field descriptor, and schema hash. Handwritten code must consume those
generated definitions and must not maintain a second command, event, or field registry.

# Normative sources

- `RPE-PROTO-001`, sections 3-7, 12-13, 16-18, and 20-24.
- `RPE-ARCH-001`, sections 4.4-4.5, 9.3, 9.12, and 14.1-14.6.
- `RPE-STD-001`, sections 3, 5-11, and 15.
- `RPE-STD-002`, sections 2, 4-8, 11-15.
- `RPE-STD-005`, sections 4-7, 11, 15-17, and 20.

# Security and resource rules

- Raw desktop payload bytes are prepared only after the fixed header, version, message descriptor,
  flags, payload length, complete frame length, global and message limits, transfer count, and
  receive-direction sequence candidate have all been accepted. The sequence is committed only after
  generated typed decoding, correlation/state validation, and OOB resource validation succeed.
- Bootstrap frames are accepted only for current exact handshake message IDs and the compiled
  `(major, minor, codec ABI)` registry. A post-handshake decoder can be created only from
  `CompatibleHandshake`; callers cannot inject a raw version or a second message policy.
- Browser handshake negotiation captures exact ordinary own data descriptors once before semantic
  validation. Accessors, exotic prototypes, shared mutable schema-hash views, and values that drift
  between validation and snapshot cannot enter the WeakSet-authenticated connection context.
- The exported browser schema-hash array is a convenience copy, never the handshake trust anchor.
  Message, correlation, outcome, EngineError policy, field, enum, and capability metadata used by
  runtime validation are recursively frozen before export and before admission builds its lookup.
- Browser sequence validation accepts only exact privately branded trackers, calls a
  module-captured original pending operation, and freezes tracker instances, their prototype, and
  constructor. Forged/subclass trackers and own/prototype method replacement fail closed; commit
  still rechecks the private high-watermark after all business validation.
- Receive-direction sequence trackers are independent. Sequences start at one. Gaps are allowed;
  zero, duplicates, and regressions are rejected without moving the accepted sequence.
- All sizes, strides, offsets, and exclusive ends use checked arithmetic. A Surface byte range must
  fit both protocol limits and the declared and actual resource extent. Browser bitmap alpha and
  dimensions, fixed-length ArrayBuffer/SAB requirements, SAB publication fence, negotiated
  capability, desktop slot, LocalMemory epoch, and nonzero Surface lease are transport-specific
  fail-closed checks.
- ProvideData ranges are non-empty with checked exclusive ends. Declared range and byte lengths,
  canonical slot order, and actual transfer byte lengths must all match before bytes are exposed.
- The explicitly bounded data-ticket ledger first binds one complete immutable source descriptor
  (identity, known length, and validator) to each exact worker/session pair. An exact active rebind
  is idempotent; descriptor drift, capacity exhaustion, or a poisoned session fails closed. NeedData
  registration resolves only this persistent binding, and known source length bounds every range.
  Validator bytes are bound opaquely rather than reinterpreted with a Rust-only rule; validator
  admissibility remains a canonical cross-language contract. Session and worker invalidation remove
  their ticket and source-binding state together.
- Each accepted NeedData retains its exact worker, session, request, ticket, canonical requested
  ranges, and resume checkpoint. ProvideData must reproduce that partition and source identity
  exactly. FailData accepts an observed identity only for non-retryable SourceChanged, where it must
  differ from the expected immutable snapshot.
- A prepared data terminal does not consume its ticket. Commit is a separate post-business step
  bound to the exact ledger instance and entry epoch. It owns the validated ProvideData or FailData
  command, complete source descriptor, and immutable worker/session/request/ticket/checkpoint owner
  snapshot, so caller mutation, resource-adoption failure, lifecycle invalidation, replay,
  cross-ledger tokens, and same-key ABA cannot change or terminate another ticket.
- Valid SourceChanged preparation immediately moves the session binding into a fail-closed observed
  state, blocking every new or previously prepared individual data terminal. The actor must then
  prioritize `commit_source_changed`, which atomically removes every session ticket and leaves a
  poisoned binding tombstone. This dedicated path prevents ProvideData from winning after source
  drift has been observed, regardless of data/source-change commit order.
- Header message identity, decoded command/event variant, correlation shape, and duplicated payload
  identities are accepted together. SurfaceReady correlation and actual resource validation use one
  receiver operation; shape-only correlation validation is not dispatch authorization.
- Surface handles and payload bytes never appear in `Debug`, `Display`, or stable protocol errors.
- Protocol failures never invoke an external PDF engine and never convert malformed input into a
  successful empty payload or Surface.

# Generated-data boundary

`src/generated.rs` is generated data. It must be reproduced by the pinned protocol generator from
the canonical schema and must not be edited as handwritten implementation. Repository tests bind
the generated marker, generator version, wire identity, nested codec registry, compatibility,
invalid, payload-codec, and hash known-answer vectors, and handwritten validation adapter so schema
or codec drift fails closed.
