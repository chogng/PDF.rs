# PDFium baseline adapter

This directory records configuration for a separately built, process-level PDFium
runner. The source checkout is available at `../pdfium` relative to the repository;
it is not linked, vendored, downloaded, or included in any PDF.rs product artifact.

The baseline crate now provides protocol schema 2 and a tested, deadline- and
byte-limited direct-child supervisor. It is exercised only with a self-authored
fixture; it is not a PDFium adapter or an approved sandbox. `runner_executable`,
invocation and build fingerprints, and isolation metadata remain M0-blocking
until a reviewed build and adapter are entered in
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
Parse/Scene/positioned-Text artifacts. A future adapter must report unsupported
channels explicitly or use a separately reviewed C-API helper; it must never fill
missing channels with synthetic or empty observations.
