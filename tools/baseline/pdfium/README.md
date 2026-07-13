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

The checkout recorded on 2026-07-13 has no synced standalone build dependencies
or build output. PDFium's stock `pdfium_test` can produce raster images and plain
text in separate invocations, but it does not provide this protocol's canonical
Parse/Scene/positioned-Text artifacts. A future adapter must report unsupported
channels explicitly or use a separately reviewed C-API helper; it must never fill
missing channels with synthetic or empty observations.
