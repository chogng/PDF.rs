# Scope

`runtime/protocol` owns the dependency-free Native Engine wire contract consumed by runtime and
platform adapters. The checked-in `src/generated.rs` file is generated from the canonical Engine
schema and is the only source of wire message, identifier, descriptor, and payload definitions.
Handwritten code in this crate validates untrusted desktop frames, handshake compatibility,
correlation shape, sequence order, transfer slots, and Surface metadata before exposing a payload
or platform resource to runtime code.

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

- Raw desktop payload bytes are borrowed only after the fixed header, version, message descriptor,
  flags, payload length, complete frame length, global and message limits, transfer count, and
  receive-direction sequence have all been accepted. A decoded command or event is not dispatched
  until its generated payload schema and correlation shape are also accepted.
- Receive-direction sequence trackers are independent. Sequences start at one. Gaps are allowed;
  zero, duplicates, and regressions are rejected without moving the accepted sequence.
- All sizes, strides, offsets, and exclusive ends use checked arithmetic. A Surface byte range must
  fit both protocol limits and the declared shared-memory region.
- ProvideData ranges are non-empty with checked exclusive ends. Declared range and byte lengths,
  canonical slot order, and actual transfer byte lengths must all match before bytes are exposed.
- Surface handles and payload bytes never appear in `Debug`, `Display`, or stable protocol errors.
- Protocol failures never invoke an external PDF engine and never convert malformed input into a
  successful empty payload or Surface.

# Generated-data boundary

`src/generated.rs` is generated data. It must be reproduced by the pinned protocol generator from
the canonical schema and must not be edited as handwritten implementation. Repository tests bind
the generated marker, generator version, schema hash, nested codec registry, compatibility and
invalid vectors, and handwritten validation adapter so schema drift fails closed.
