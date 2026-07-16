# PDFium baseline adapter

This directory records configuration for a separately built, process-level PDFium
runner. The source checkout is available at `../pdfium` relative to the repository;
it is not linked, vendored, downloaded, or included in any PDF.rs product artifact.

The baseline crate provides protocol schema 2, a tested deadline- and
byte-limited direct-child supervisor, a source-only PDFium public-C-API pixel
adapter, and separate source-only Outline and page-count probes. The helpers link only inside a
separately synced PDFium checkout; they are never PDF.rs product dependencies.
The pixel profile reports Parse, Scene, and positioned Text as explicitly
unsupported and produces exact-size, top-down, straight-alpha RGBA8 pixels. It
does not draw form/widget overlays. The Outline and page-count profiles each produce one bounded
canonical JSON parse artifact and report Scene, Text, and Pixel as unsupported.

The helper is not an approved sandbox. For M0, protocol v2 and the generic
supervisor provide only a process-level black box for fixed, self-authored,
hash-bound inputs; this closes the external-runner infrastructure boundary but
does not register PDFium as a baseline. `runner_executable`, invocation and
complete build/runtime fingerprints, reviewed isolation metadata, and a
`docs/traceability/baseline-ledger.toml` entry remain blocking for registered
DIFFERENTIAL CI or untrusted inputs. Such an adapter must add platform
enforcement for descendants, CPU, memory, private per-invocation filesystem and
temporary storage, syscalls, and network access around the direct-child harness.
Any eventual output has O4 observational authority only and cannot create or
update a Native golden automatically.

PDFium's active upstream is the
[googlesource repository](https://pdfium.googlesource.com/pdfium/); the
[`chromium/pdfium`](https://github.com/chromium/pdfium) GitHub repository is an
archived read-only mirror. On 2026-07-13, revision
`c040cf96106a87220b814a1a892649cf2d7f1934` was synced and built in an isolated
temporary checkout without modifying `../pdfium`. GN generated 597 targets, the
initial Ninja build completed 1409 steps, all 1034 C++ unit tests passed, the
fixed `rectangles_clipped.in` upstream pixel test passed, and stock
`pdfium_test --show-pageinfo` processed the project's generated one-page fixture.
The pixel runner required PDFium's depot_tools-managed `vpython3`; direct system
Python is not the documented dependency environment.

The redacted, hash-bound result is recorded in
`evidence/pdfium-c040cf96-macos-arm64-build-readiness-v1.toml`. It is only
upstream build-readiness evidence. It is not a protocol-adapter run, an O4
comparison, product-correctness evidence, a performance result, a complete
runtime or license closure, a registered baseline, or a release gate. Raw logs,
upstream fixtures, source, dependencies, and binaries are not committed. The
baseline ledger therefore remains empty and every adapter fingerprint remains
blocking.

The public-C-API helper was then compiled from the committed source overlay and
run through protocol schema 2. The canonical 4x4 `q Q` fixture produced the
same 64-byte white RGBA output twice, and a generated four-quadrant diagnostic
matched its analytic color/channel/row-order expectation exactly. Both checks
reported zero different pixels and channels; one page-out-of-range request and
one malformed-PDF request each mapped to terminal diagnostic
`RPE-BASELINE-0006`. The exact hashes and zero-diff summaries are recorded in
`evidence/pdfium-c040cf96-macos-arm64-o4-pixel-adapter-probe-v1.toml`.

That second report is an O1 analytic check against PDFium O4 pixels, not a
Native/PDFium comparison. It does not exercise the synthetic failure bundle's
declared artifact oracle, establish the case's complete color/antialias render
profile, measure performance, close fonts/runtime/licenses, establish a platform
sandbox, or register a baseline. All correctness, differential, performance,
registration, and release-gate eligibility fields remain false.

A third, separately identity-bound helper uses only PDFium's public bookmark APIs to observe a
bounded preorder of depth, normalized title, signed item Count, and target kind.
On 2026-07-14, the valid three-item nested fixture matched Native byte-for-byte
on that observable intersection, and the PDFium output repeated identically.
For a second fixture with a deliberately wrong `/Prev`, Native returned
`RPE-DOCUMENT-0041` (`OutlineSiblingMismatch`) while PDFium produced the same
observable outline. That difference is expected: the public bookmark API does
not expose or validate `/Prev`.

The hash-bound result is recorded in
`evidence/pdfium-c040cf96-macos-arm64-o4-outline-differential-probe-v1.toml`.
It is a real, non-gating Native/PDFium O4 comparison over the explicitly named
observable subset. It cannot adjudicate root Count, `/Last`, `/Parent`, `/Prev`,
raw `/Dest` versus `/A` shape, or missing-versus-invalid empty roots, and it is
not a registered baseline, a golden, product-correctness evidence, or a release
gate. The baseline ledger remains empty until containment and complete runtime
and license fingerprints are reviewed.

A fourth identity-bound helper uses PDFium's public page-count API and the same schema-2 process
boundary. Valid self-authored one-page and nested three-page fixtures matched Native exactly, and
both PDFium outputs repeated byte-for-byte. For an otherwise identical nested fixture whose
positive root `/Count` is 4 instead of the Native-recomputed 3, Native returned
`RPE-DOCUMENT-0033` (`PageTreeCountMismatch`) while PDFium produced `page_count=4`. This is recorded
as an expected strictness difference rather than allowing the external observation to weaken the
Native structural rule.

The hash-bound result is recorded in
`evidence/pdfium-c040cf96-macos-arm64-o4-page-count-differential-probe-v1.toml`.

The page-count comparison is real but remains non-gating and unregistered. It is not a golden,
product-correctness evidence, a release gate, or a contributor to the separately registered
project-owned `core.strict-page-count` DIFFERENTIAL promotion and bounded M1 exit gate. The older
one-page `pdfium_test --show-pageinfo` execution remains a separate build-readiness smoke
observation.

A release-mode follow-up at PDF.rs revision
`0f6cbde39e8e49dbcd3f784a07684a2ff7302c2c` reused the exact hash-bound page-count helper on one
self-authored 128-page traditional-xref fixture. Two independent trials each performed five
warmups and 50 interleaved timed samples per engine. All 100 timed and ten warmup comparisons
returned the exact canonical count of 128. Native's full in-memory RangeStore/xref/attestation/
strict-page-count path recorded trial medians of 0.378 ms and 0.360 ms; the schema-2 PDFium cold
direct-child/init/load/count/response boundary recorded 7.882 ms and 7.896 ms. Raw nanosecond
samples, p95, p99, and conservative median confidence intervals are recorded in
`evidence/pdfium-c040cf96-macos-arm64-o4-page-count-boundary-performance-probe-v1.toml`.

Those timings intentionally have `performance_eligible=false`: the measured scopes are different,
the local Mac is not a fixed performance pool, CPU affinity/background load were uncontrolled,
peak memory was not measured, and the helper remains uncontained with incomplete runtime/license
closure. The 20.848x-21.919x ratio is therefore a reproducible development-boundary observation,
not a PDFium-kernel-versus-Native-kernel performance claim or release threshold.

Stock `pdfium_test` can produce raster images and plain text in separate
invocations, but it does not provide this protocol's canonical
Parse/Scene/positioned-Text artifacts. The direct helper therefore reports those
channels as unsupported rather than filling them with synthetic or empty data.

## Build the source-only helper

Use a disposable, fully synced PDFium checkout at the pinned revision. Do not
apply this overlay to the canonical `../pdfium` source checkout. From the PDF.rs
repository root, with `PDFIUM_ROOT` naming that disposable checkout:

```sh
mkdir -p "$PDFIUM_ROOT/tools/pdf_rs_baseline_adapter"
cp tools/baseline/pdfium/helper/BUILD.gn \
  tools/baseline/pdfium/helper/pdf_rs_pdfium_adapter.cc \
  "$PDFIUM_ROOT/tools/pdf_rs_baseline_adapter/"
git -C "$PDFIUM_ROOT" apply \
  "$PWD/tools/baseline/pdfium/helper/pdfium-root.patch"
```

Generate and build the fixed Agg/FreeType, V8/XFA/Skia/Fontations-disabled
configuration:

```sh
cd "$PDFIUM_ROOT"
buildtools/mac/gn gen out/Adapter --args='use_remoteexec=false is_debug=false symbol_level=0 target_cpu="arm64" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false'
third_party/ninja/ninja -C out/Adapter pdf_rs_pdfium_adapter
```

The overlay only makes the helper target reachable by GN. The product workspace
does not link PDFium, and no helper binary is copied back into this repository.

## Run the explicit real-engine probe

The real-engine test is ignored by default, requires a separately built helper,
and clears the helper's environment before launch. A fresh checkout must first
replay the ignored canonical fixture:

```sh
cargo run --quiet --package pdf-rs-generate -- \
  tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl \
  tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf

PDF_RS_PDFIUM_ADAPTER="$PDFIUM_ROOT/out/Adapter/pdf_rs_pdfium_adapter" \
  cargo test --package pdf-rs-baseline --test pdfium_real_adapter -- \
  --ignored --exact real_pdfium_adapter_matches_analytic_pixel_probes --nocapture
```

This manual probe is not a Native/PDFium differential. Its analytic pixel checks
only validate the transport and pixel adapter against self-authored, directly
derivable inputs. Any recorded PDFium output remains O4 observation data.

## Build and run the Outline differential probe

Apply this overlay after the pixel overlay above so the previously evidence-bound
pixel `BUILD.gn` and root patch remain byte-for-byte unchanged. The Outline
evidence binds the prerequisite pixel evidence, build definition, helper source,
and root patch by SHA-256; the two-step overlay is therefore explicit rather than
an unrecorded checkout precondition:

```sh
mkdir -p "$PDFIUM_ROOT/tools/pdf_rs_outline_adapter"
cp tools/baseline/pdfium/helper/outline.BUILD.gn \
  "$PDFIUM_ROOT/tools/pdf_rs_outline_adapter/BUILD.gn"
cp tools/baseline/pdfium/helper/pdf_rs_pdfium_outline_probe.cc \
  "$PDFIUM_ROOT/tools/pdf_rs_outline_adapter/"
git -C "$PDFIUM_ROOT" apply \
  "$PWD/tools/baseline/pdfium/helper/pdfium-outline-root.patch"

cd "$PDFIUM_ROOT"
buildtools/mac/gn gen out/Adapter --args='use_remoteexec=false is_debug=false symbol_level=0 target_cpu="arm64" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false'
third_party/ninja/ninja -C out/Adapter pdf_rs_pdfium_outline_probe
```

The host and SDK prerequisites follow Chromium's official
[macOS build instructions](https://chromium.googlesource.com/chromium/src/+/main/docs/mac_build_instructions.md).
Run the explicit ignored comparison with:

```sh
PDF_RS_PDFIUM_OUTLINE_ADAPTER="$PDFIUM_ROOT/out/Adapter/pdf_rs_pdfium_outline_probe" \
  cargo test --package pdf-rs-baseline --test pdfium_outline_real_adapter -- \
  --ignored --exact real_pdfium_outline_observable_subset_matches_native --nocapture
```

This comparison is suitable as non-gating development baseline evidence for the
named observable subset. PDFium remains O4 authority and cannot override the
strict Native topology rules or ISO-derived expectations.

## Build and run the page-count differential probe

Apply the page-count overlay after the pixel and Outline overlays above. Its root patch deliberately
binds that prerequisite ordering so previously recorded helper inputs remain unchanged:

```sh
mkdir -p "$PDFIUM_ROOT/tools/pdf_rs_page_count_adapter"
cp tools/baseline/pdfium/helper/page_count.BUILD.gn \
  "$PDFIUM_ROOT/tools/pdf_rs_page_count_adapter/BUILD.gn"
cp tools/baseline/pdfium/helper/pdf_rs_pdfium_page_count_probe.cc \
  "$PDFIUM_ROOT/tools/pdf_rs_page_count_adapter/"
git -C "$PDFIUM_ROOT" apply \
  "$PWD/tools/baseline/pdfium/helper/pdfium-page-count-root.patch"

cd "$PDFIUM_ROOT"
buildtools/mac/gn gen out/Adapter --args='use_remoteexec=false is_debug=false symbol_level=0 target_cpu="arm64" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false'
third_party/ninja/ninja -C out/Adapter pdf_rs_pdfium_page_count_probe
```

Run the explicit ignored comparison with:

```sh
PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER="$PDFIUM_ROOT/out/Adapter/pdf_rs_pdfium_page_count_probe" \
  cargo test --package pdf-rs-baseline --test pdfium_page_count_real_adapter -- \
  --ignored --exact real_pdfium_page_counts_match_native_and_record_strict_count_difference --nocapture
```

The valid results are exact and repeatable only for the two fixed fixtures. The mismatched positive
root Count is an expected strictness difference, and the probe remains outside CI and every product
or release path.

## Run the page-count boundary-performance probe

Use a clean PDF.rs checkout detached at the revision recorded by the evidence, plus the exact
already-built page-count helper. The test refuses a debug build and remains ignored by default:

```sh
PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER="$PDFIUM_ROOT/out/Adapter/pdf_rs_pdfium_page_count_probe" \
  cargo test --release --package pdf-rs-baseline \
  --test pdfium_page_count_performance -- \
  --ignored --exact real_pdfium_page_count_wide_cold_process_performance_probe \
  --nocapture --test-threads=1
```

Run the command twice to reproduce the recorded batch. Each command validates the fixture and
helper hashes, performs five untimed warmups per engine, then emits 50 raw samples per engine with
the exact behavior result and summaries. Do not compare the reported ratio with an engine-only
benchmark: the Native and PDFium measurement scopes are explicitly different.
