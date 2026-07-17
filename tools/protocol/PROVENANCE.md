# Scope

`tools/protocol` parses the canonical `protocol/engine.protocol` schema and deterministically emits
the Rust protocol declarations and typed codec, browser TypeScript types, validators, and typed
codec, desktop field registry, complete wire-identity record, compatibility vectors, invalid-input
vectors, and payload/hash known-answer vectors.

# Semantic owner

The Runtime/Protocol workstream owns the canonical schema and generator. The schema is the single
source of truth for message IDs, payload fields, correlation shapes, flags, transfer-slot bounds,
capability bits, and lifecycle events.

# Algorithms and derivations

- The parser accepts only bounded ASCII, LF-terminated canonical text and rejects alternate spacing,
  duplicate definitions, unknown types/outcomes, signed enum/union representations, reserved or
  overflowing union discriminants, oversized enum/union collections, missing union-field privacy,
  and values outside fixed schema or message limits.
- The generator uses one parsed model for all seven output files. Output order, spelling, paths, and
  the package-version identity embedded in each output class are fixed; generation has no
  timestamp, environment, or discovery-order input.
- The wire identity is a domain-separated SHA-256 over the codec ABI, codec name, and exact
  canonical schema bytes. The 16-byte handshake identity is the first 16 digest bytes under the
  named `sha256-first-16-bytes` policy; the raw schema SHA-256 is recorded separately for audit.
- Browser runtime trust anchors never share mutable exports: handshake/transcript validation uses
  private canonical schema-hash bytes, while generated enum and descriptor registries are frozen
  recursively before either validators or browser admission can observe them. The exported
  `SCHEMA_HASH` byte array is only a caller-owned convenience copy.
- Browser handshake and sequence authenticity call module-captured WeakSet intrinsics. Sequence
  trackers require exact construction, carry a private brand and private mutable counter, reject
  subclasses and prototype forgeries, and freeze both instance and prototype; envelope validation
  invokes the captured original pending operation rather than dynamically dispatching through
  caller-controlled state.
- `fixed_le_v1` codecs are generated for both Rust and TypeScript. CapabilityDecision and
  RenderPlanManifest hash preimages are exactly `UTF-8 domain || 0x00 || u64LE(payload length) ||
  fixed_le_v1 payload`; generated known answers freeze both framing and digest. Rust hash-preimage
  entry points also expose a bounded fragment observer so product cancellation reaches collection
  encoding and chunked preimage construction without changing canonical bytes.
- `generate <repository-root>` is the only writing mode. `--check <repository-root>` regenerates in
  memory and compares exact bytes without replacing files or changing modification times. Writing
  mode stages and syncs the complete output set under one advisory lock, records a durable
  prepared/committed journal, and recovers either the complete old or complete new generation after
  interruption. Every writer locks the repository root directory's stable inode; the ignored
  transaction journal exists only during an active or interrupted writing run. An oversized,
  truncated, foreign-path, or otherwise invalid journal fails closed before any output is touched;
  because no replacement starts before a complete prepared journal is synced, those cases retain
  the complete old generation for explicit inspection.

# Dependencies and generated data

The crate has zero dependencies, no `build.rs`, forbids unsafe code, and performs no network or
external-engine access. Generated files are committed so product builds never execute code
generation implicitly.

The fixed outputs are:

- `runtime/protocol/src/generated.rs`
- `platform/browser/generated/engine-protocol.ts`
- `platform/desktop/generated/engine-protocol.registry`
- `protocol/generated/schema-hash.txt`
- `protocol/generated/compatibility-vectors.json`
- `protocol/generated/invalid-vectors.json`
- `protocol/generated/payload-codec-vectors.json`

# Security and trust boundary

The CLI operates only on an explicit developer or CI checkout. It rejects absolute, parent-relative,
or symlinked schema/output path components and preflights the schema metadata against the 1 MiB
ceiling before reading. These checks do not provide a race-free untrusted-directory sandbox; the
repository root remains a trusted checkout controlled by the caller.

This development-only crate is not a product runtime dependency, worker, renderer, or transport
implementation. It generates product payload codecs but cannot negotiate a connection or make a
support/capability decision itself.

# Tests

Tests cover canonical parser replay and rejection, SHA-256 known answers, deterministic codegen,
compatibility, invalid-vector, payload-codec, and hash-KAT replay, the fixed output allowlist,
transaction rollback and lock rejection, symlink rejection, zero dependencies/no build script,
strict CLI arity with usage exit 2, oversized-schema preflight, real subprocess interruption,
between-rename recovery, torn/foreign/oversized journal fail-closed behavior, and a real
temporary-checkout proof that check mode preserves generated bytes and modification times. Browser
runtime tests execute generated validators and typed codecs for message/header/correlation/transfer
binding and Surface stride/range invariants, including malicious top-level and nested handshake
getter vectors that must be rejected without invoking accessors, exported schema-hash mutation, and
attempted mutation of message, error-policy, field, and capability descriptor registries. Sequence
tests additionally cover prototype pollution, forged/subclass trackers, and duplicate rejection
through the Browser command boundary.

# History

- 2026-07-17: Added the canonical Engine protocol, explicit zero-dependency generator, seven
  committed output classes, strict transactional generate/check CLI, typed codecs, and
  repository/runtime validation tests.
