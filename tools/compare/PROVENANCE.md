# Scope

Deterministic M0 parse, Scene, text, and RGBA pixel comparison artifacts,
canonical JSON serialization, exact diff summaries, and a minimal PNG encoder
for synthetic Native/baseline/failure-bundle output.

# Semantic owner

Quality/Corpus owns artifact stability and diff semantics. Graphics/Color reviews
Scene and pixel meaning; Specification/Conformance reviews oracle usage.

# Normative sources

- RPE-ARCH-001 v0.3, sections 12.1 and 12.10-12.14: M0 comparison tooling,
  Scene/Text/Pixel diff layering, and oracle boundaries.
- RPE-STD-003 v0.1, sections 7-8: canonical golden artifacts and exact assertion
  ordering.
- RPE-STD-001 v0.1, sections 5, 9, and 14: explicit invariants, checked
  arithmetic, deterministic output, and public contract documentation.
- RFC 8259: JSON string escaping and UTF-8 JSON text.
- PNG Specification, third edition: PNG signature, IHDR/IDAT/IEND chunks,
  chunk CRC, RGBA8 color type 6, and scanline filter type 0.
- RFC 1950 and RFC 1951: zlib framing, Adler-32, and DEFLATE stored blocks.

# Algorithms and derivations

- JSON object keys are emitted in a fixed lexical order; arrays retain declared
  semantic order. Parse objects and diagnostics are sorted at construction
  because discovery order is non-semantic.
- Scene transforms and text geometry use caller-supplied fixed-point integers;
  this crate does not canonicalize platform floating-point strings.
- Semantic summaries perform exact aligned record comparison with explicit
  changed, missing, unexpected, and first-difference counts.
- Pixel comparison uses unsigned per-channel absolute differences over RGBA8 and
  checked counters. The semantic diff retains all four deltas; its visualization
  stores RGB deltas and makes changed pixels opaque so equal input alpha cannot
  hide a real difference. Alpha-only changes are visualized as neutral intensity.
- PNG output is deliberately minimal: RGBA8, filter 0 on each row, one IDAT
  chunk, zlib header `0x78 0x01`, stored DEFLATE blocks of at most 65,535 bytes,
  Adler-32 payload checksum, and IEEE CRC-32 chunk checksums.

# External observations

No external PDF engine output or implementation source was used to define these
formats. Baseline images remain O4 observations unless independently adjudicated.

# Dependencies and generated data

The crate has no external dependencies, unsafe code, generated tables, or
embedded corpus data.

# Tests and fuzz targets

- JSON syntax/control-character escaping, Unicode preservation, and stable field
  ordering.
- Exact and changed Parse/Scene/Text summaries.
- Exact RGBA comparison, channel/pixel statistics, dimension mismatch, buffer
  mismatch, zero dimensions, and arithmetic overflow.
- PNG signature, chunk order, dimensions, CRC-32, Adler-32, stored-block
  reconstruction, multi-block output, visible diff alpha, and byte determinism.
- Debug formatting for Text/Pixel/PNG values exposes only schemas, dimensions,
  and counts; content bytes and Unicode are redacted.

No fuzz target is registered in this initial M0 package.

# Known deviations and unsupported cases

- The PNG encoder supports only non-empty RGBA8 images and filter type 0.
- Compression is stored DEFLATE only; output size is intentionally traded for a
  small auditable deterministic implementation.
- Semantic comparison is exact and ordered. Tolerance-aware geometry, edge, and
  color comparison remains a later capability and must not silently alter these
  exact summaries.

# History

- 2026-07-13: initial M0 canonical comparison and PNG artifact implementation.
