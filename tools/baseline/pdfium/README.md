# PDFium baseline adapter

This directory records configuration for a separately built, process-level PDFium
runner. The source checkout is available at `../pdfium` relative to the repository;
it is not linked, vendored, downloaded, or included in any PDF.rs product artifact.

No executable adapter is implemented in this slice. `runner_executable` and every
fingerprint placeholder remain M0-blocking until a reviewed build and process
adapter are entered in `docs/traceability/baseline-ledger.toml`. The adapter must
enforce output bounds, concurrent pipe draining, a watchdog, kill/reap cleanup,
and an approved sandbox policy. Any eventual output has O4 observational
authority only and cannot create or update a Native golden automatically.

The checkout recorded on 2026-07-13 has no synced standalone build dependencies
or build output. PDFium's stock `pdfium_test` can produce raster images and plain
text in separate invocations, but it does not provide this protocol's canonical
Parse/Scene/positioned-Text artifacts. A future adapter must report unsupported
channels explicitly or use a separately reviewed C-API helper; it must never fill
missing channels with synthetic or empty observations.
