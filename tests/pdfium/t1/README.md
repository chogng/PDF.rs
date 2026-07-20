# PDFium T1 compatibility suite

This directory is the Rust-facing view of a small, pinned subset of the
upstream `pdfium_tests` corpus. It intentionally keeps the test definition
beside its upstream manifest instead of hiding it in a tooling crate.

- `manifest.toml` names the downloaded PDFs, hashes, expected page counts,
  source revision, license, and feature family.
- The matching PDFium expected PNG files are bound by hash during the PDF.rs
  product-profile replay. The PDF.rs behavior contract itself lives separately
  in `tests/pdfrs/strict-open-v1/`, so upstream source provenance and product
  behavior can evolve independently.

Download the pinned objects with:

```bash
scripts/fetch-pdfium-corpus.sh /private/tmp/pdf-rs-pdfium-tests
PDF_RS_PDFIUM_CORPUS_ROOT=/private/tmp/pdf-rs-pdfium-tests \
  cargo test --locked --package pdf-rs-quality --test pdfrs_strict_open_v1
```
