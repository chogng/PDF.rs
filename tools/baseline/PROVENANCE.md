# Scope

Development/CI-only process protocol for black-box baseline observations. This
module is excluded from every product dependency graph.

# Semantic owner

Baseline/Release workstream.

# Normative sources

- RPE-ARCH-001 version 0.3, decisions AD-007, AD-008, AD-012 and sections
  11.2, 11.4, 12.14.
- RPE-STD-001 version 0.1, sections 6 and 10.
- RPE-STD-003 version 0.1, sections 6 and 14.
- RPE-STD-004 version 0.1, sections 2, 8, and 9.

# Algorithms and derivations

The fixed-width, big-endian frame is a project-defined tool protocol. Schema 2
uses a 96-byte request header and a 112-byte response header. A canonical
descriptor digest length-prefixes all string fields and binds build, flags,
environment, invocation, license-manifest, font, and color hashes. The invocation
digest separately binds the canonical executable path, argv, cleared/allowlisted
environment, working directory, stdin transport version, declared isolation
profile, byte ceilings, and watchdog. Requests bind the descriptor digest and the
verified PDF content hash; responses must echo both identities plus page and
geometry. Each Parse, Scene, Text, and Pixel channel is explicitly `produced`,
`unsupported`, or `failed`; an unavailable channel cannot be represented by a
synthetic empty payload. Identity and geometry are checked even on terminal
failure frames, and lengths are validated before allocation or slicing. This is
not the product Engine protocol.

The standard-library process supervisor launches an absolute canonical path
directly without implicitly inserting a shell, clears inherited environment
variables, preflights the encoded request, concurrently writes stdin and drains
stdout/stderr, retains only byte-limited stdout, and discards stderr content. A
monotonic watchdog, pipe-limit signal, or transport failure requests direct-child
termination and polls for exit/reaping for a fixed grace period. Failure to prove
direct-child exit or pipe-thread completion is reported as containment failure
rather than making the API call wait indefinitely.

# External observations

External results produced through this API have O4 authority only. They cannot
create or update a Native golden automatically.

On 2026-07-13, build readiness and the advertised `pdfium_test` command surface
were inspected at PDFium revision
`c040cf96106a87220b814a1a892649cf2d7f1934`. The review was limited to repository
metadata/build files, `README.md`, `testing/BUILD.gn`, the usage/options section of
`testing/pdfium_test.cc`, and output-path matches in `testing/helpers/write.cc`.
No PDFium implementation algorithm was copied or adapted during that source
review.

A separate upstream build-readiness exercise on 2026-07-13 synced that revision
in an isolated temporary checkout, built the stock `pdfium_unittests`,
`pdfium_test`, and `pdfium_diff` targets, passed 1034 upstream C++ unit tests and
one fixed upstream pixel test, and used `pdfium_test --show-pageinfo` to process
the project's generated one-page fixture. The redacted hashes and counts are in
`pdfium/evidence/pdfium-c040cf96-macos-arm64-build-readiness-v1.toml`; raw logs,
source, dependencies, binaries, and upstream data are not committed. This
exercise did not run the baseline protocol or adapter, consume the corpus
manifest, produce an O4 comparison, measure performance, establish containment
or runtime/license/font/color closure, register a baseline, or create release
evidence.

A separate probe on 2026-07-13 compiled the source-only public-C-API helper in
that isolated checkout and ran it through protocol schema 2. The fixed 4x4
`q Q` fixture produced two byte-identical white RGBA observations, and an inline
generated four-quadrant PDF exactly matched a separately derived top-down RGBA
expectation. One out-of-range page request and one malformed-PDF request each
mapped to the terminal diagnostic `RPE-BASELINE-0006`. The redacted hashes and
exact zero-diff summaries are in
`pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml`;
the helper binary, PDF bytes, RGBA bytes, and raw logs are not committed.

These are O1 analytic checks against PDFium O4 pixels. No Native engine was run,
so the report is not a Native/PDFium differential. The failure-bundle case's
declared oracle concerns synthetic artifacts and was not used as a real raster
golden. The probe did not consume the corpus manifest, establish the complete
case color/antialias profile, measure performance, close runtime/license/font/
color fingerprints, establish platform containment, register a baseline, or
create release evidence.

On 2026-07-14, a separate public-C-API Outline helper was incrementally built at
the same pinned revision and run through protocol schema 2 beside the Native
strict Outline job. For a valid three-item nested outline, both sides produced
the same 197-byte canonical observable summary, and PDFium repeated the result
byte-for-byte. For an otherwise identical fixture with a deliberately wrong
`/Prev`, Native returned `RPE-DOCUMENT-0041` while PDFium produced the same
summary. This is classified as an expected strictness difference because the
public bookmark API neither exposes nor validates `/Prev`.

The hash-bound result is in
`pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1.toml`.
It is a real, non-gating Native/PDFium O4 comparison only over preorder depth,
normalized title, signed item Count, and action-before-effective-destination
target kind. It is not a registered baseline or correctness oracle and cannot
adjudicate outline-root Count, `/Last`, `/Parent`, `/Prev`, raw target shape, or
missing-versus-invalid empty roots. No helper binary, PDF bytes, canonical JSON
bytes, PDFium source, dependency payload, or raw log is committed.

On 2026-07-14, a separate public-C-API page-count helper was built at the same pinned revision and
run through protocol schema 2 beside the Native strict page-tree job. The valid one-page and nested
three-page fixtures matched exactly, and each PDFium result repeated byte-for-byte. For the same
nested structure with a deliberately mismatched positive root `/Count` of 4, Native recomputed
three leaves and returned `RPE-DOCUMENT-0033` (`PageTreeCountMismatch`), while PDFium produced
`page_count=4`. The disagreement is classified as an expected strictness difference.

The hash-bound result is in
`pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-page-count-differential-probe-v1.toml`.

This is a real but non-gating and unregistered Native/PDFium O4 comparison over the scalar page
count of three fixed, self-authored inputs. It does not turn PDFium into a correctness oracle,
weaken Native's Parent/Count/cycle/duplicate checks, advance `core.strict-page-count` beyond
`PLANNED`, or establish M1 exit. No helper binary, PDF bytes, canonical JSON bytes, PDFium source,
dependency payload, or raw log is committed.

A separate release-mode boundary-performance probe then reused that exact helper at PDF.rs
revision `0f6cbde39e8e49dbcd3f784a07684a2ff7302c2c` from a clean detached local clone. One hash-fixed
128-page traditional-xref fixture received five warmups and 50 interleaved timed samples per engine
in each of two independent command runs. All 100 timed and ten warmup behavior comparisons returned
the exact canonical page count. Native's complete in-memory RangeStore/xref/attestation/strict-count
path recorded 0.378 ms and 0.360 ms trial medians; the schema-2 PDFium cold-child/init/load/count/
response boundary recorded 7.882 ms and 7.896 ms. The report commits every raw timing sample,
nearest-rank p95/p99, and conservative sign-test median intervals in
`pdfium/evidence/pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1.toml`.

This is empirical development-boundary evidence with `performance_eligible=false`, not an
engine-kernel performance differential. The scopes are different, the machine is not a fixed
performance pool, CPU affinity/background load were not controlled, peak memory was not measured,
and the helper remains uncontained with incomplete runtime and license closure. The raw command
logs remain uncommitted, while their hashes and byte counts bind the committed sample vectors.

# Dependencies and generated data

Runtime code uses the Rust standard library plus the local development-only
`pdf-rs-digest` crate. Real-adapter tests also use local comparison, generator,
byte-source, syntax, xref, object, and document crates to exercise the Native
side of explicitly ignored comparisons. No external engine is linked or
vendored into the workspace.

# Tests and fuzz targets

Unit tests cover deterministic identity-bound request encoding, partial channel
states, successful decoding, malformed/failed/oversized responses, identity
mismatches (including failed frames), pixel-size validation, geometry limits,
limit validation, and bounded stderr discard. Integration tests launch a
self-authored fixture executable and cover literal argv handling, inherited
environment removal, large concurrent stdin/stdout/stderr movement, exact output
ceilings, request preflight before spawn, watchdog kill/reap, process/protocol
failure classification, invocation mismatch rejection, inherited-pipe containment
failure, stable error category/recovery policy, and redacted diagnostics. A
default-ignored pixel test covers repeated blank-page pixels, generated
color/channel/row-order pixels, out-of-range pages, and malformed documents. A
second default-ignored test compares the bounded PDFium bookmark observation
with Native on valid topology and records the expected `/Prev` strictness
difference. A third default-ignored test compares Native and PDFium page counts for valid one-page
and nested three-page trees and records the expected mismatched-root-Count strictness difference.
The fourth default-ignored test is release-only and records two explicit raw-sample trials for the
fixed 128-page behavior and unequal-scope Native/PDFium development boundaries.
Contract tests require both semantic profiles to produce bounded UTF-8, newline-terminated parse
output while every non-parse channel is unsupported. A streaming decoder fuzz target remains
planned before baseline registration or untrusted corpus execution.

# Known deviations and unsupported cases

Safe `std::process` supervision covers only the direct child. It does not create a
process group/job object or enforce descendant, CPU, memory, filesystem, syscall,
or network containment. The declared isolation-profile string is identity
evidence, not enforcement. The runner also cannot derive or verify the full
runtime-closure digest represented by `build_hash`; a reviewed build pipeline
must establish that evidence and protect the executable from replacement between
verification and spawn.

If a descendant inherits a pipe or direct-child termination cannot be proven,
`ContainmentFailed` is returned after the fixed grace period, but safe
`std::process` cannot forcibly cancel a blocked pipe thread or guarantee that the
child/descendant was reaped. Such a failure can leave helper threads, buffered
request/response bytes, or external processes alive until the inherited handles
close.
The caller must therefore supply an approved platform sandbox/container, a
private per-invocation filesystem policy, and process-tree teardown; the generic
supervisor alone is not approved for untrusted, externally sourced, or
non-fixed PDF input.

The local PDFium source checkout is not built or distributed by this crate. The
recorded helper executable was built only in a disposable checkout and remains
probe-only. A platform containment wrapper, complete runtime and license
fingerprints, replacement protection, and baseline-ledger entry are required
before differential CI. Until then this is a tested partial process boundary,
not the M0 external baseline runner exit condition.

PDFium's public bookmark API normalizes titles, returns the raw item Count, may
resolve destination names or GoTo actions into an effective destination, and
does not expose outline-root Count, `/Last`, `/Parent`, or `/Prev`. It also
collapses several missing, invalid, and empty-root states into a null first
bookmark. The Outline probe therefore compares only its declared observable
intersection and cannot weaken or replace Native's stricter structural checks.

The public PDFium page-count surface returns one scalar and does not expose the traversal evidence
needed to adjudicate Parent links, cycles, duplicate children, or recursively recomputed Counts.
Its `page_count=4` observation on the mismatched positive root Count therefore remains an expected
strictness difference and cannot replace the Native page-tree validator.

The boundary-performance probe cannot isolate PDFium's engine time from schema-2 encode/decode,
process creation, supervisor polling, library initialization, document load, response I/O, and
child reaping. Its Native side likewise includes RangeStore materialization, xref open, candidate
construction, base attestation, and strict traversal. The reported ratio must not be presented as
algorithm parity or used as a performance gate.

# History

- 2026-07-13: Introduced process-isolation protocol schema version 1.
- 2026-07-13: Added schema version 2, explicit channel outcomes, invocation
  identity, and a deadline/byte-limited direct-child supervision harness.
- 2026-07-13: Recorded a non-baseline PDFium upstream build-readiness exercise.
- 2026-07-13: Added the source-only PDFium pixel adapter and recorded a
  non-gating O4 pixel probe with separately computed, unreviewed analytic checks.
- 2026-07-14: Added the source-only PDFium Outline adapter and recorded a
  non-gating Native/PDFium observable-subset differential with an expected
  `/Prev` strictness difference.
- 2026-07-14: Added the source-only PDFium page-count adapter and a non-gating Native/PDFium
  comparison with exact repeatable valid counts and an expected root-Count strictness difference.
- 2026-07-14: Recorded two raw-sample 128-page page-count boundary-performance trials with exact
  behavior agreement and explicit `performance_eligible=false` scope separation.
