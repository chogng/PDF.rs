# PDF.rs strict-open v1 product profile

This is a PDF.rs-owned regression suite, not an upstream compatibility
manifest. `profile.toml` records the strict-open behavior that PDF.rs commits
to for the samples sourced from `tests/pdfium/t1/`.

- `ready.page-count` means PDF.rs supports the strict-open/page-count path and
  commits to the stated page count.
- Any other terminal is an intentional, stable capability boundary. It is a
  passing result only when it is deterministic and exact; it is not counted as
  PDF rendering compatibility.

When a capability is implemented, first change its profile entry to the new
supported behavior, add a minimized PDF.rs-authored case under `tests/cases/`,
and retain the upstream sample as an independent compatibility regression.

Run it with the same downloaded source corpus:

```bash
PDF_RS_PDFIUM_CORPUS_ROOT=/private/tmp/pdf-rs-pdfium-tests \
  cargo test --locked --package pdf-rs-quality --test pdfrs_strict_open_v1
```
