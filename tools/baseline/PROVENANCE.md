# Scope

Development/CI-only process protocol for black-box baseline observations. This
module is excluded from every product dependency graph.

# Semantic owner

Baseline/Release workstream.

# Normative sources

- RPE-ARCH-001 decisions AD-007, AD-008, AD-012 and sections 11.2, 11.4, 12.14.
- RPE-STD-003 sections 6 and 14.
- RPE-STD-004 sections 2, 8, and 9.

# Algorithms and derivations

The fixed-width, big-endian frame is a project-defined tool protocol. A canonical
descriptor digest length-prefixes all string fields and binds build, flags,
environment, license-manifest, font, and color hashes. Requests bind that digest
and the verified PDF content hash; responses must echo both identities plus page
and geometry. Lengths are validated before allocation or slicing. It is not the
product Engine protocol.

# External observations

None recorded. Results produced through this API always have O4 authority.

# Dependencies and generated data

Rust standard library plus the local development-only `pdf-rs-digest` crate. No
external engine is linked or vendored.

# Tests and fuzz targets

Unit tests cover deterministic identity-bound request encoding, successful
decoding, malformed/failed/oversized responses, identity mismatches, pixel-size
validation, and geometry limits. A streaming decoder fuzz target is planned
before this protocol handles a real baseline build.

# Known deviations and unsupported cases

This slice deliberately exposes no process launcher: a concrete adapter must
concurrently drain stdout/stderr, enforce a watchdog, kill and reap on every
transport failure, and apply a reviewed sandbox policy. The local PDFium source
checkout is not built or distributed by this crate. A separately reviewed
executable, complete fingerprint, and baseline-ledger entry are required before
differential CI. Until then this is a protocol boundary, not the M0 external
baseline runner exit condition.

# History

- 2026-07-13: Introduced process-isolation protocol schema version 1.
