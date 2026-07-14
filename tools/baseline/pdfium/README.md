# PDFium baseline adapter

This directory records configuration for a separately built, process-level PDFium
runner. The source checkout is available at `../pdfium` relative to the repository;
it is not linked, vendored, downloaded, or included in any PDF.rs product artifact.

The baseline crate provides protocol schema 2, a tested deadline- and
byte-limited direct-child supervisor, and a source-only PDFium public-C-API
pixel adapter. The adapter links only inside a separately synced PDFium checkout;
it is never a PDF.rs product dependency. Its initial profile reports Parse,
Scene, and positioned Text as explicitly unsupported and produces exact-size,
top-down, straight-alpha RGBA8 pixels. It does not draw form/widget overlays.

The helper is not an approved sandbox. `runner_executable`, invocation and
complete build/runtime fingerprints, and isolation metadata remain M0-blocking
until a reviewed build and adapter environment are entered in
`docs/traceability/baseline-ledger.toml`. A real adapter must add platform
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
and clears the helper's environment before launch:

```sh
PDF_RS_PDFIUM_ADAPTER="$PDFIUM_ROOT/out/Adapter/pdf_rs_pdfium_adapter" \
  cargo test --package pdf-rs-baseline --test pdfium_real_adapter -- \
  --ignored --exact real_pdfium_adapter_matches_analytic_pixel_probes --nocapture
```

This manual probe is not a Native/PDFium differential. Its analytic pixel checks
only validate the transport and pixel adapter against self-authored, directly
derivable inputs. Any recorded PDFium output remains O4 observation data.
