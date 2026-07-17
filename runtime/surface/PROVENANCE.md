# Scope

`runtime/surface` owns the pure product lifecycle that turns complete Native pixel storage into
immutable worker/session-owned Surface publications. It models bounded private allocation, fake
shared-memory handles, atomic publication, acquire, one-shot transfer, idempotent release,
virtual-clock lease reclaim, session close, and Worker epoch invalidation.

# Trust boundary

All producer and consumer identities, dimensions, byte ranges, formats, access rights, handle
classes, generations, and epochs are validated before storage or bytes are exposed. The crate uses
only safe Rust and in-memory state; it performs no operating-system I/O, FFI, networking, PDF
parsing, rendering, IPC, or platform-handle creation.

# Ownership and state invariants

- A live Surface belongs to one nonzero Worker epoch, Session, viewport generation, and canonical
  RenderPlan/region identity.
- Allocation creates zero-initialized producer-private bytes and exactly one private read-write
  fake handle. Allocation failures do not charge an ID, handle, or byte.
- Publication is atomic: the complete pixel range and canonical protocol metadata validate before
  private write access becomes read-only. Published bytes are never exposed mutably.
- Transfer is one-shot. Import independently validates the fake handle table, class, access,
  extent, token, owner, Session, Worker epoch, generation, format, alpha mode, byte range,
  transport slot, and canonical protocol plan identity before consuming the transfer.
- Acquisition returns only an immutable borrow tied to the owner table lifetime.
- Explicit release is idempotent for the exact Surface/lease pair. Cancellation, producer failure,
  stale generation, lease expiry, Session close, and Worker restart drop retained storage.
- Surface IDs are never reused inside a Worker epoch. A restart invalidates the complete old epoch
  before its numeric counters may begin again.

# Bounds and clocks

`SurfaceLimits` precharges live Surfaces, fake handles, per-epoch IDs, Sessions, aggregate retained
bytes, and lease duration under hard ceilings. Dimension, stride, offset, length, retained-byte,
identifier, secret, and virtual-clock arithmetic is checked. Lease expiry depends only on the
caller-advanced virtual clock; the product crate reads no wall clock and creates no threads.

# Failure and diagnostics

Failures expose only stable `RPE-SURFACE-*` identifiers. Debug output redacts lease tokens,
transfer tokens, fake handle identities, transport details, and pixel storage. Failed producer or
consumer validation leaves the prior lifecycle state intact; in particular, a failed import does
not consume its one permitted transfer.

# Semantic owners

Canonical Surface wire metadata and typed identities come from `pdf-rs-protocol`. Immutable Native
RenderPlan identities come from `pdf-rs-policy`. This crate owns only the resource lifecycle and
does not fork either schema.

# History

- 2026-07-17: Added the M4-07 pure Surface owner and fake shared-memory lifecycle.
