# Scope

Deterministic SHA-256 content addressing shared by development/CI tools. This is
not a PDF Security Handler or a general cryptographic service.

# Semantic owner

Quality/Corpus workstream.

# Normative sources

FIPS PUB 180-4 defines the SHA-256 equations, initial values, round constants,
padding, and digest encoding.

# Algorithms and derivations

The safe-Rust streaming implementation was independently written from FIPS PUB
180-4. Input length uses checked 64-bit byte/bit framing; blocks and words use
the standard wrapping arithmetic.

# External observations

None.

# Dependencies and generated data

Rust standard library only. Round constants are normative standard data.

# Tests and fuzz targets

Published empty, short, and multi-block vectors plus incremental chunk-boundary
equivalence. Longer property/fuzz coverage is planned before hashing unbounded
external corpus streams.

# Known deviations and unsupported cases

Inputs whose bit length cannot fit the SHA-256 64-bit length field are rejected.
This crate does not provide HMAC, signatures, secret handling, or side-channel
claims.

# History

- 2026-07-13: Introduced SHA-256 tooling digest schema 1.
