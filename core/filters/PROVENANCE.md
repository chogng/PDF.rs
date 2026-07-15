# Scope

`core/filters` is the first bounded Native product stream-decoding slice. It consumes one exact,
immutable `ByteSlice` and implements the internal no-filter identity path plus canonical
`ASCIIHexDecode`, `ASCII85Decode`, `RunLengthDecode`, and zlib-wrapped `FlateDecode` chains. A
successful `DecodedStream` cannot be cloned or separated from its sealed `DecodeAttestation`.
Canonical `FlateDecode` stages may additionally apply bounded TIFF Predictor 2 or PNG predictor
values at or above 10 with explicit defaulted parameters. `FilterPlan::from_pdf_dictionary`
provides the filters-owned shared direct-metadata canonicalizer used by object-stream composition
to bind a parsed stream dictionary to an attested plan; the separate source-xref bootstrap policy
is recorded below pending explicit reconciliation.

This crate performs no object resolution, Range polling, file/network access, async scheduling,
decryption, image decoding, cache insertion, or external-engine fallback.

# Semantic owner

Parser/Security owns strict direct `/Filter` and `/DecodeParms` canonicalization, filter and
predictor state machines, deterministic decode fuel, per-layer/cumulative/final output limits,
cancellation probes, and source-redacted errors. `core/bytes` owns immutable snapshot-backed
physical storage. `core/syntax` owns parsed direct dictionaries, checked physical spans, and object
references. Object/document layers remain responsible for validating a stream dictionary and
`/Length`, resolving its exact encoded `ByteSlice`, passing the parsed direct dictionary through
the filters-owned canonicalizer, exact-binding that plan to composition evidence, and deciding
capability policy before decoding. Runtime owns job generation, cancellation delivery,
session-wide budgets, and decoded-stream cache policy.

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
- [ISO 32000-1:2008, 7.4.4.4](https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf)
  defines the predictor parameters, TIFF horizontal differencing, and the rule that every PNG row
  carries the tag selecting its reconstruction algorithm for all `/Predictor` values at or above
  10.

The cited Adobe publication supplies the clause-level predictor semantics, but the repository does
not yet hash-pin an ISO 32000 snapshot and errata set or register a clause-level conformance corpus
for this filter profile. `M1StrictV1` therefore remains a bounded development profile and not an
ISO/O0 conformance claim.

# Algorithms and derivations

- `DecodeRequest::new` consumes the physical `ByteSlice` and rejects a source identity mismatch,
  a non-exact encoded `ByteSpan`, or dictionary/encoded geometry beyond a known snapshot length.
  The successful attestation retains the complete `SourceSnapshot`, explicit `SourceIdentity`,
  owner `ObjectRef`, dictionary span, encoded span and physical slice, canonical plan, profile,
  validated limits, fuel schedule and consumption, cumulative output, peak retained capacity, and
  final decoded length. It also records canonical-plan heap retention from the actual capacities
  of both long-lived `Vec<StreamFilter>` and `Vec<FilterStage>` allocations, separately from output
  capacity so downstream owners can add each allocation exactly once.
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
- `FilterPlan` retains a canonical `FilterStage` for every filter while its legacy constructors and
  `filters()` view remain parameter-free compatible. `PredictorParameters` makes the PDF defaults
  (`Predictor=1`, `Colors=1`, `BitsPerComponent=8`, `Columns=1`) explicit, accepts only positive
  dimensions, predictors 1, 2, and every integer from 10 through `i64::MAX` (the syntax layer's PDF
  integer ceiling), and component widths 1, 2, 4, 8, or 16. Invalid signs, zeroes, and row-width
  overflow are syntax failures; recognized values outside the component-width profile and
  parameters attached to a non-Flate filter are unsupported-capability failures.
- `FilterPlan::from_pdf_dictionary` accepts an absent `/Filter` only with absent `/DecodeParms`, a
  full canonical name, or a nonempty direct-name array. Metadata keys must be unique. A single
  filter accepts absent, `null`, or one direct parameter dictionary; an array filter accepts
  absent, `null`, or an equal-length direct array of `null` and direct dictionaries. Empty
  parameter dictionaries canonicalize to no parameters, while nonempty Flate dictionaries make
  `Predictor`, `Colors`, `BitsPerComponent`, and `Columns` defaults explicit. Indirect values,
  abbreviations, malformed shapes, unknown keys, and duplicate/noninteger predictor fields are
  rejected. `from_pdf_names` and direct-dictionary canonicalization share the private
  `canonical_pdf_filter` full-name mapper, so the mapping remains centralized.
- Dictionary canonicalization applies the validated `max_filters` limit before allocating the two
  exact vectors owned by the returned `FilterPlan`, creates no additional
  filter-count-proportional temporary vector, validates retained plan heap against the same
  profile, and probes the caller's `DecodeCancellation` before and throughout every outer
  dictionary, filter-array, parameter-array, and parameter-dictionary walk. The temporary name
  and mapped-filter vectors used by older constructors were eliminated; one private full-name
  mapper is shared by `from_pdf_names` and direct-dictionary canonicalization.
- TIFF Predictor 2 reconstructs packed samples rather than bytes, adds the previous same-color
  sample modulo the component width, resets history at every row, and leaves unused row-padding
  bits unchanged. It covers 1-, 2-, 4-, 8-, and 16-bit components without allocating a side row:
  already-budgeted output bytes are rewritten only after sample work is charged.
- Every PNG predictor value at or above 10 uses each row's tag from 0 through 4 to select None, Sub,
  Up, Average, or Paeth; the numeric `/Predictor` value does not fix a particular PNG algorithm.
  Reconstruction uses checked row-byte and byte-per-pixel geometry, exact tagged-row framing,
  wrapping byte arithmetic, and prior output bytes; truncated, overlong, and unknown-tag rows fail
  without partial success.
- Filter chains execute in declared source order. Each layer receives only the previous layer's
  owned bytes. Output is committed one byte at a time only after layer, cumulative, final, fuel,
  and capacity checks succeed. Arithmetic and platform-size conversions are checked or saturate
  into a deterministic limit failure.
- `DecodeFuelScheduleVersion::M1V1` charges one unit before each layer setup, consumed input byte,
  emitted output byte, and bounded codec algorithm step. Flate block setup, Huffman table entries
  and paths, dynamic repeat expansion, predictor row/tag work, TIFF packed-bit reconstruction, and
  PNG byte prediction are therefore charged even when they emit no bytes. TIFF charges separately
  before sample bookkeeping, every packed input/output bit read, arithmetic, and every bit
  mutation; PNG charges separately before each neighbor lookup, prediction, and reconstruction
  addition. These operations do not use bulk fuel charges that could cross a one-unit cancellation
  interval. The cancellation probe runs before any decode work and again after at most the
  configured fuel interval. Cancellation, malformed input, unsupported capability, integrity
  failure, resource exhaustion, and internal failure remain separate categories.
- Each output vector grows with fallible exact-reserve requests. The selected capacity is checked
  before allocation and the allocator-reported capacity is checked after allocation. While a
  later layer is produced, the previous owned intermediate capacity and current output capacity
  are charged simultaneously; the successful attestation records the observed peak. The original
  physical `ByteSlice` backing remains charged by `RangeStore` and is not double-counted as filter
  output capacity.
- `FilterPlan::retained_heap_bytes` uses checked platform-size conversion, multiplication, and
  addition over both actual vector capacities. `FilterPlan::retained_heap_upper_bound` is the
  single public derivation of the corresponding `max_filters` and type-width ceiling for parent
  pre-admission. Invalid filter ceilings are configuration errors; conversion or arithmetic
  overflow is an internal invariant failure. Constructors reject allocator capacity beyond the
  fixed hard-filter-count type-width bound, and `decode_stream` rechecks the actual capacity against
  the request's `max_filters`-derived `FilterPlanBytes` resource bound before codec work. Constructor-local
  canonicalization vectors do not survive in `DecodedStream` and therefore are not publication
  retention; their element counts remain bounded by the same hard filter-count ceiling.
- Decoded positions use `DecodedOffset` and `DecodedRange` only. Physical `ByteSpan` values are
  retained solely for the dictionary and encoded source evidence; no decoded byte is assigned a
  fabricated physical location.

# External observations

The implementation author produced the initial predictor code without external PDF-engine output
or implementation source. During independent review, a separate reviewer read-only inspected
`../pdfium/core/fxcodec/flate/flatemodule.cpp` predictor dispatch while checking the reported PNG
semantic mismatch. No PDFium code or table was copied, no PDFium binary or baseline was run, and
the corrected rule is derived from ISO 32000-1:2008 section 7.4.4.4 rather than that observation.
A reviewer not exposed to that PDFium source independently re-reviewed the corrected product diff
against ISO 32000-1:2008 section 7.4.4.4 and reported zero blocker, high, medium, or low findings.
PDFium remains a development-only O4, unregistered, non-gating observer; it is neither a dependency,
normative input, nor correctness oracle for this crate. Three fixed zlib byte fixtures
were generated once by CPython 3.9.6 using its compiled and runtime zlib 1.2.12: level 0 for the
stored block, `Z_FIXED` at level 9 for the fixed-Huffman block, and level 9 for the dynamic block.
They are codec-interoperability test inputs only; their self-authored expected plaintext and failure
policy are asserted independently and zlib is not treated as a normative oracle. The 32 KiB
distance and reduced-CINFO fixtures are instead constructed by the repository's test-only bit
writer and Adler implementation.

# Dependencies and generated data

The only dependencies are in-repository `pdf-rs-bytes` and `pdf-rs-syntax` product primitives.
Decoders use the Rust standard library and contain no third-party codec dependency, unsafe code,
generated table, embedded corpus object, filesystem/network access, or async runtime. This slice
adds no third-party license or redistribution obligation.

# Tests and fuzz targets

Behavior tests cover strict direct-dictionary canonicalization for absent, single, and ordered
array filters; null, empty, and predictor parameter forms; explicit defaults; exact filter-count
limits; cooperative cancellation during metadata walking; and malformed, duplicate, indirect,
unknown, wrong-shape, and wrong-arity adversaries. Decode behavior tests cover exact
source/snapshot/object/span attestation, decoded-relative slicing,
redacted debug output, internal identity fuel, strict canonical-name handling, ASCIIHex whitespace
and odd nibbles, ASCII85 full/partial/`z` groups and overflow, RunLength literal/repeat runs,
ordered filter composition, missing terminators, illegal bytes/groups/runs, trailing data,
source-change and physical-geometry rejection, invalid profiles, cancellation cadence, stored,
fixed-Huffman, and dynamic-Huffman zlib streams, the full 32 KiB distance and smaller declared
windows, invalid headers and stored framing, preset-dictionary rejection, Adler mismatch, trailing
data, truncation, expansion limits, codec-work cancellation, TIFF packed samples at every supported
component width, row-reset and padding behavior, all five PNG row-tag algorithms across predictor
values 10, 12, 15, 16, and `i64::MAX`, packed PNG pixel widths, explicit defaults and full-plan
attestation, row truncation/extra bytes, illegal tags and parameters, predictor cancellation, and
an interval-one cancellation between TIFF bit mutations plus within PNG row work, and structured
predictor final, cumulative, retained-capacity, and exact one-less fuel failures, in addition to
the general input, filter-count, per-layer, cumulative, final, retained-capacity, and fuel limit
coverage.

`core/filters::repository_policy` enforces the two lower-level product dependencies, absence of
development/codec/platform dependencies, async/network/filesystem/external-engine isolation,
sealed non-Clone decoded products, and the absence of a public identity filter variant. There is
no coverage-guided fuzz target, pinned conformance corpus, or registered Native/external-engine
differential evidence in this stage.

# Known deviations and unsupported cases

- LZW, DCT, CCITT, JPX, JBIG2, crypt filters, decode parameters other than the implemented Flate
  predictor tuple, preset Flate dictionaries, and inline-image abbreviations are unsupported. This
  crate must not be advertised as general PDF stream support or as the complete architecture 5.6
  P0 filter profile.
- Empty encoded streams cannot currently be represented by the non-empty `ByteRange`/`ByteSlice`
  primitive. Resolving that physical-input contract belongs to `core/bytes`; this crate does not
  synthesize an unattested empty slice.
- The object-stream layer now uses the direct dictionary canonicalizer and exact-compares its
  result with sealed decode evidence. Exact `/Length` acquisition, indirect `/DecodeParms`
  resolution, stream decryption order, source-driven decode scheduling, and decoded cache keys
  remain composition work. The existing source-xref acquisition path still carries a separate,
  overlapping in-module bootstrap canonicalizer whose single-filter scalar/array shape policy is
  not identical; any migration or tightening requires an explicit compatibility decision and
  regressions rather than being described as non-semantic cleanup.
- The strict required-marker and trailing-data policy is intentionally not presented as pinned ISO
  conformance until clause-level authority and conformance fixtures are registered.
- Limits and `M1V1` fuel weights are bootstrap values, not a released `FuelSchedule` or
  `ReleaseProfile` decision. Runtime must still apply parent/session budgets and watchdogs.
- Completing this crate does not satisfy the M1 exit gate, does not promote page-count or outline
  to `DIFFERENTIAL`, and does not change baseline registration or gating status.

# History

- 2026-07-15: Added the filters-owned, cancellable, limit-bound strict direct dictionary
  canonicalizer used for exact metadata-to-attestation composition checks.
- 2026-07-15: Added bounded TIFF and PNG predictor decoding with canonical per-stage parameters,
  packed-sample reconstruction, strict row tags/framing, algorithm fuel, cancellation, and layered
  output/capacity enforcement.
- 2026-07-15: Added dependency-free strict zlib/Deflate decoding for stored, fixed-Huffman, and
  dynamic-Huffman blocks with declared-window checks, Adler-32, algorithm fuel, cancellation, and
  expansion-budget enforcement.
- 2026-07-15: Added sealed source-attested decoded streams, strict internal identity,
  ASCIIHex/ASCII85/RunLength chains, deterministic fuel and layered capacity/output limits,
  cancellation, source-change rejection, provenance, and repository policy tests.
