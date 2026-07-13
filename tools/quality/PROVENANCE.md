# Scope

Repository quality-lane selection, strict case-manifest validation, product
dependency-purity scanning, content addressing, and deterministic synthetic
failure-bundle generation.

# Semantic owner

Quality/Corpus workstream.

# Normative sources

- RPE-ARCH-001 decisions AD-004, AD-012 and sections 12.3-12.6, 12.18, 15.3.
- RPE-STD-001 sections 2, 5, 6, and 9.
- RPE-STD-003 sections 4-8, 19, and 20.
- RPE-STD-004 sections 4, 11, 12, and 15.

# Algorithms and derivations

The manifest reader intentionally accepts a canonical TOML v1 subset: root
schema, named tables, scalar values, and single-line string arrays. It rejects
unknown/duplicate fields before checking every required semantic group.

Bundle addresses hash a domain separator followed by lexically sorted artifact
names and length-prefixed bytes, including the validated source `case.toml` and
verified adjacent generated input. `manifest.toml` records but is not included
in the payload hash, avoiding a circular digest. The synthetic runner enforces
its exact O1 expected-artifact contract plus input, geometry, object, resolve,
scene-command, and operator-fuel bounds. SHA-256 is provided by the local
`pdf-rs-digest` tooling crate.

# External observations

None. The M0 bundle's `baseline` files are deliberately synthetic O1 analytic
counterparts used to exercise the artifact channel; they are not PDFium output.

# Dependencies and generated data

- Rust standard library.
- Local tooling crates `pdf-rs-generate`, `pdf-rs-compare`, and `pdf-rs-digest`.
- No external packages, engines, fonts, color data, or user documents.

# Tests and fuzz targets

Tests cover required manifest groups, malformed syntax, duplicates, hash/oracle/
budget validation, deterministic bundle addressing, source/render/contract
binding, artifact completeness, mismatch diagnostics, idempotent writes, and
rejection of product-to-tools or full-engine dependencies. A manifest
parser fuzz target and interrupted-write recovery are planned before T1 inputs.

# Known deviations and unsupported cases

- The manifest parser does not claim general TOML compatibility; multiline
  arrays, dotted keys, inline tables, escapes within array elements, and comments
  after unterminated strings are rejected by the canonical v1 subset.
- Scene/protocol artifacts are uncompressed in M0; `.zst` storage is deferred
  until a compression dependency and its resource/license review are approved.
- The synthetic environment record is deterministic and does not replace a real
  runner fingerprint in T1/nightly/release bundles.
- The synthetic bundle command imposes a 16 MiB input ceiling and a 1,048,576
  pixel ceiling in addition to the case-declared limits so an untrusted manifest
  cannot select unbounded allocations.
- Product purity is a direct workspace/product manifest preflight. Resolved Cargo
  closures, binaries, Wasm imports, dynamic libraries, packages, and network
  manifests remain separate release-blocking scans; this command does not claim
  that broader proof.

# History

- 2026-07-13: Added canonical manifest schema 1, SHA-256 addressing, and complete
  synthetic parse/Scene/Text/Pixel failure bundles.
