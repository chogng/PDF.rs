# Scope

`core/filters` is the first bounded Native product stream-decoding slice. It consumes one exact,
immutable `ByteSlice` and implements the internal no-filter identity path plus canonical
`ASCIIHexDecode`, `ASCII85Decode`, `RunLengthDecode`, and zlib-wrapped `FlateDecode` chains. A
successful `DecodedStream` cannot be cloned or separated from its sealed `DecodeAttestation`.

This crate performs no object resolution, Range polling, file/network access, async scheduling,
decryption, predictor processing, image decoding, cache insertion, or external-engine fallback.

# Semantic owner

Parser/Security owns filter-plan canonicalization, strict filter state machines, deterministic
decode fuel, per-layer/cumulative/final output limits, cancellation probes, and source-redacted
errors. `core/bytes` owns immutable snapshot-backed physical storage. `core/syntax` owns checked
physical spans and object references. Object/document layers remain responsible for validating a
stream dictionary and `/Length`, resolving its exact encoded `ByteSlice`, mapping `/Filter` values
to a canonical plan, and deciding capability policy before calling this crate. Runtime owns job
generation, cancellation delivery, session-wide budgets, and decoded-stream cache policy.

# Normative sources

- [RPE-ARCH-001, sections 4.2-4.5 and 5.6](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the `filters` product boundary, one-way core dependency direction, independent filter
  orchestration, foundational filter priority, cancellation, and decoding budgets.
- [RPE-ARCH-001, section 9.1 and 9.4](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  assigns immutable decode jobs to workers and requires decoded-stream cache charging by output
  bytes without caching resource-limit failures as success.
- [RPE-STD-001, sections 3-9](../../docs/standards/coding-standard.md) requires one-way product
  dependencies, sealed validated models, checked arithmetic, stable structured errors, bounded
  allocation, cooperative cancellation, and async-runtime-free core code.
- [RPE-STD-004, sections 7-8](../../docs/standards/traceability-and-provenance.md) defines this
  module record and the independent-implementation boundary.
- [RPE-STD-005, sections 4-10](../../docs/standards/security-and-resource-budget.md) requires
  versioned deterministic fuel, checks before work/allocation, cancellation at bounded fuel
  intervals, exact source binding, and filter-chain per-layer plus cumulative output accounting.
- [RFC 1950](https://www.rfc-editor.org/rfc/rfc1950.html) defines the zlib header, window
  declaration, Deflate payload envelope, and Adler-32 integrity check used by `FlateDecode`.
- [RFC 1951](https://www.rfc-editor.org/rfc/rfc1951.html) defines stored, fixed-Huffman, and
  dynamic-Huffman Deflate blocks, canonical codes, length/distance pairs, overlapping copies, and
  the 32 KiB maximum window.

The repository does not yet bind these filter state machines to a pinned ISO 32000 snapshot,
errata set, or clause-level conformance corpus. `M1StrictV1` is therefore a self-authored bounded
development profile and not an ISO/O0 conformance claim.

# Algorithms and derivations

- `DecodeRequest::new` consumes the physical `ByteSlice` and rejects a source identity mismatch,
  a non-exact encoded `ByteSpan`, or dictionary/encoded geometry beyond a known snapshot length.
  The successful attestation retains the complete `SourceSnapshot`, explicit `SourceIdentity`,
  owner `ObjectRef`, dictionary span, encoded span and physical slice, canonical plan, profile,
  validated limits, fuel schedule and consumption, cumulative output, peak retained capacity, and
  final decoded length.
- The public `StreamFilter` enum contains only the four implemented canonical filters. An empty
  plan selects a private identity copy path. The non-standard PDF name `Identity`, abbreviated
  names, and unknown names return source-redacted `UnsupportedFilter`; there is no external-engine
  retry path.
- `ASCIIHexDecode` ignores the six PDF whitespace bytes, accepts upper/lowercase hexadecimal
  digits, pads one final high nibble with zero, requires `>`, and rejects non-whitespace bytes after
  termination.
- `ASCII85Decode` ignores the six PDF whitespace bytes outside the `~>` pair, expands `z` only at a
  group boundary, validates base-85 groups against `u32`, pads a final two-to-four-digit group with
  `u`, rejects a one-digit final group, requires adjacent `~>`, and rejects non-whitespace trailing
  bytes.
- `RunLengthDecode` implements literal controls `0..=127`, repeat controls `129..=255`, and the
  `128` end marker. It checks each run extent before reading, requires the end marker, and rejects
  every byte after it because this binary filter has no trailing-whitespace exception.
- `FlateDecode` validates the zlib CM, CINFO, FCHECK, and no-preset-dictionary profile before
  decoding every RFC 1951 block kind. Fixed and dynamic canonical Huffman trees use fixed-size
  stack storage; malformed or unassigned paths, missing end-of-block, reserved literal/length or
  distance symbols, invalid repeats, and stored LEN/NLEN mismatches fail without partial success.
  Length/distance copies read the already-produced output one byte at a time, which preserves
  required overlap semantics while enforcing both the declared zlib window and the 32 KiB maximum.
  The decoder validates Adler-32 over all emitted bytes and, under this exact-slice strict profile,
  rejects any byte after the checksum as trailing data.
- Filter chains execute in declared source order. Each layer receives only the previous layer's
  owned bytes. Output is committed one byte at a time only after layer, cumulative, final, fuel,
  and capacity checks succeed. Arithmetic and platform-size conversions are checked or saturate
  into a deterministic limit failure.
- `DecodeFuelScheduleVersion::M1V1` charges one unit before each layer setup, consumed input byte,
  emitted output byte, and bounded codec algorithm step. Flate block setup, Huffman table entries
  and paths, and dynamic repeat expansion are therefore charged even when they emit no bytes. The
  cancellation probe runs before any decode work and again after at most the configured fuel
  interval. Cancellation, malformed input, unsupported capability, integrity failure, resource
  exhaustion, and internal failure remain separate categories.
- Each output vector grows with fallible exact-reserve requests. The selected capacity is checked
  before allocation and the allocator-reported capacity is checked after allocation. While a
  later layer is produced, the previous owned intermediate capacity and current output capacity
  are charged simultaneously; the successful attestation records the observed peak. The original
  physical `ByteSlice` backing remains charged by `RangeStore` and is not double-counted as filter
  output capacity.
- Decoded positions use `DecodedOffset` and `DecodedRange` only. Physical `ByteSpan` values are
  retained solely for the dictionary and encoded source evidence; no decoded byte is assigned a
  fabricated physical location.

# External observations

No external PDF engine output, implementation source, or copied codec table was used. The local
PDFium checkout and baseline runner were not invoked. They remain separate development-only O4
observers and are neither dependencies nor normative inputs of this crate. Three fixed zlib byte
fixtures were generated once by CPython 3.9.6 using its compiled and runtime zlib 1.2.12: level 0
for the stored block, `Z_FIXED` at level 9 for the fixed-Huffman block, and level 9 for the dynamic
block. They are codec-interoperability test inputs only; their self-authored expected plaintext and
failure policy are asserted independently and zlib is not treated as a normative oracle. The
32 KiB distance and reduced-CINFO fixtures are instead constructed by the repository's test-only
bit writer and Adler implementation.

# Dependencies and generated data

The only dependencies are in-repository `pdf-rs-bytes` and `pdf-rs-syntax` product primitives.
Decoders use the Rust standard library and contain no third-party codec dependency, unsafe code,
generated table, embedded corpus object, filesystem/network access, or async runtime. This slice
adds no third-party license or redistribution obligation.

# Tests and fuzz targets

Behavior tests cover exact source/snapshot/object/span attestation, decoded-relative slicing,
redacted debug output, internal identity fuel, strict canonical-name handling, ASCIIHex whitespace
and odd nibbles, ASCII85 full/partial/`z` groups and overflow, RunLength literal/repeat runs,
ordered filter composition, missing terminators, illegal bytes/groups/runs, trailing data,
source-change and physical-geometry rejection, invalid profiles, cancellation cadence, stored,
fixed-Huffman, and dynamic-Huffman zlib streams, the full 32 KiB distance and smaller declared
windows, invalid headers and stored framing, preset-dictionary rejection, Adler mismatch, trailing
data, truncation, expansion limits, codec-work cancellation, and structured failures for input,
filter-count, per-layer, cumulative, final, retained-capacity, and fuel limits.

`core/filters::repository_policy` enforces the two lower-level product dependencies, absence of
development/codec/platform dependencies, async/network/filesystem/external-engine isolation,
sealed non-Clone decoded products, and the absence of a public identity filter variant. There is
no coverage-guided fuzz target, pinned conformance corpus, or registered Native/external-engine
differential evidence in this stage.

# Known deviations and unsupported cases

- Predictors, LZW, DCT, CCITT, JPX, JBIG2, crypt filters, decode parameters, preset Flate
  dictionaries, and inline-image abbreviations are unsupported. This crate must not be advertised
  as general PDF stream support or as the complete architecture 5.6 P0 filter profile.
- Empty encoded streams cannot currently be represented by the non-empty `ByteRange`/`ByteSlice`
  primitive. Resolving that physical-input contract belongs to `core/bytes`; this crate does not
  synthesize an unattested empty slice.
- The object layer does not yet call this crate. Exact `/Length` and dictionary-to-plan validation,
  indirect decode parameters, stream decryption order, object-stream consumption, and decoded
  cache keys remain future integration work.
- The strict required-marker and trailing-data policy is intentionally not presented as pinned ISO
  conformance until clause-level authority and conformance fixtures are registered.
- Limits and `M1V1` fuel weights are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision. Runtime must still apply parent/session budgets and watchdogs.
- Completing this crate does not satisfy the M1 exit gate, does not promote page-count or outline
  to `DIFFERENTIAL`, and does not change baseline registration or gating status.

# History

- 2026-07-15: Added dependency-free strict zlib/Deflate decoding for stored, fixed-Huffman, and
  dynamic-Huffman blocks with declared-window checks, Adler-32, algorithm fuel, cancellation, and
  expansion-budget enforcement.
- 2026-07-15: Added sealed source-attested decoded streams, strict internal identity,
  ASCIIHex/ASCII85/RunLength chains, deterministic fuel and layered capacity/output limits,
  cancellation, source-change rejection, provenance, and repository policy tests.
