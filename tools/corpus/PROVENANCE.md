# Scope

Typed corpus metadata, canonical schema-1 TOML manifests, bounded local object
verification, deterministic tier selection, aggregate accounting, and stable
release holdout partitioning. Object bytes are streamed only for identity checks
and are never retained by the manifest model.

# Semantic owner

Quality/Corpus workstream.

# Normative sources

- RPE-ARCH-001 sections 2.6, 12.19-12.20, and 15.3.
- RPE-STD-003 sections 16 and 20.
- RPE-STD-004 sections 11-12 and 14.
- RPE-STD-005 sections 4-5 and 17.

# Algorithms and derivations

Entries are ordered by their verified SHA-256 content identity. Holdout assignment
maps the first 64 digest bits uniformly into 10,000 deterministic buckets using
`u128` arithmetic; it never depends on insertion order or runtime randomness.

The on-disk codec accepts a deliberately strict TOML subset and requires its
unique canonical encoding: root and entry fields have a fixed order, entries and
feature tags are content-sorted, integers are canonical decimal, hashes are
lowercase `sha256:` identities, and the file ends with one newline. The exact
canonical bytes are SHA-256 bound before use.

Each object record carries a normalized repository-relative path and a positive
byte ceiling. Verification rejects absolute, parent, non-normalized, symbolic,
missing, and non-file paths, then streams the object through the local SHA-256
implementation under per-object and cumulative limits. Errors expose only stable
diagnostics, line numbers, and entry indices; they do not include paths, hashes,
or object bytes.

# External observations

None.

# Dependencies and generated data

The crate uses the Rust standard library plus the local `pdf-rs-digest` crate.
Repository replay tests use the local `pdf-rs-generate` crate as a development-only
dependency. No third-party dependency, network fetch, or external corpus is added.

`tests/corpus/manifests/t0-bootstrap-v1.toml` is project-authored schema-1 metadata
for the existing generated fixture. It records prohibited redistribution and does
not make the ignored PDF bytes redistributable; CI regenerates those bytes before
the corpus CLI re-hashes them. Its `repository` access value is the same repository
storage boundary recorded by the case manifest; case `redistributable = false`
maps to corpus `redistribution = "prohibited"`.

# Tests and fuzz targets

Model tests cover missing source/license, zero pages, private redistribution,
feature canonicalization, duplicate IDs, stable ordering/selection, summary
accounting, holdout-rate boundaries, and insertion-order independence.

Manifest tests cover canonical round trips and exact byte identity, schema/field/
value failures, duplicate identities and paths, normalized-path enforcement,
source/line/entry/feature/string/object/total limit boundaries, bounded file
loading, streaming object hashes, hash mismatch, missing/non-file/symbolic objects,
the full diagnostic/category/recovery mapping, and path/hash-redacted diagnostics.
CLI tests cover command shape, successful verification evidence, and error
redaction. A repository test
replays the generator DSL in memory and cross-checks its identities, license,
source, access, redistribution, and byte ceiling across the T0 manifest, case
manifest, and data ledger without depending on the ignored PDF being present
before tests run.

# Known deviations and unsupported cases

The executable profile is limited to canonical, locally rooted T0 manifests.
Schema 1 accepts only `T0`, repository access, and prohibited redistribution;
broader policy variants remain represented by the in-memory model but are not
authorized by this file validator.
External acquisition, content-addressed remote stores, private-object authorization,
T1-T3 scale, stratified/page-access sampling, release holdout evidence, and evidence
export remain open.

Manifest and object roots are trusted developer/CI paths. Symbolic links are
rejected by metadata checks, but robust no-follow handles for concurrently mutable
untrusted directories, cancellation, and a wall-clock watchdog are not implemented.
Successful re-hashing is integrity evidence only; it does not grant access or
redistribution authority, and a downstream consumer must not assume the path still
names the verified bytes after this call returns.

# History

- 2026-07-13: Added corpus schema primitives and deterministic holdout partitioning.
- 2026-07-13: Added canonical schema-1 T0 manifests, bounded local object
  verification, CLI evidence, and generator replay binding.
