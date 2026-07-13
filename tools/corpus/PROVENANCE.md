# Scope

Typed corpus metadata, deterministic tier selection, aggregate accounting, and
stable release holdout partitioning. This module does not read user documents.

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

# External observations

None.

# Dependencies and generated data

Rust standard library only. The module stores caller-verified hashes and license
metadata; it neither fetches nor redistributes corpus bytes.

# Tests and fuzz targets

Tests cover missing source/license, zero pages, private redistribution, feature
canonicalization, duplicate IDs, stable ordering/selection, summary accounting,
holdout rate boundaries, and insertion-order independence. Parser and large-scale
sampling property tests are planned with the on-disk manifest codec.

# Known deviations and unsupported cases

M0 defines the in-memory governance model only. TOML I/O, content re-hashing,
private-object authorization, sampling strata, and release evidence export remain
required before T1/T2 or release corpus use.

# History

- 2026-07-13: Added corpus schema primitives and deterministic holdout partitioning.
