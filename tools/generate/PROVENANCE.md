# Scope

`tools/generate` creates deterministic, self-authored PDF fixtures for the PDF.rs test system. The
M0 implementation emits one PDF 1.7 page with a direct content-stream length, a traditional
cross-reference table, a trailer, and `startxref`.

# Semantic owner

The Quality/Corpus workflow owns this tool. It is test infrastructure and must not become a product
runtime dependency.

# Normative sources

- [RPE-ARCH-001, sections 3.2-3.3](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines the independent-implementation boundary and provenance duties.
- [RPE-ARCH-001, sections 12.6-12.7](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires a deterministic minimal-PDF generator and enumerates future variation dimensions.
- [RPE-ARCH-001, section 15.3](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  places the generator in the M0 quality-infrastructure milestone.
- [RPE-STD-001, sections 2 and 9](../../docs/standards/coding-standard.md) requires reproducible
  generation and checked arithmetic for offsets and lengths.
- [RPE-STD-003, sections 2, 5, and 9](../../docs/standards/testing-standard.md) defines deterministic
  fixture, provenance, boundary, and overflow test expectations.
- [RPE-STD-004, sections 7 and 11](../../docs/standards/traceability-and-provenance.md) defines this
  record's sections and generated-artifact metadata.
- [RPE-STD-005, section 14](../../docs/standards/security-and-resource-budget.md) requires checked
  writer offsets, object counts, xref values, lengths, and accumulated output.

The emitted structure targets ISO 32000-2:2020 clauses 7.5 (file structure) and 7.7 (document
structure). A licensed normative snapshot and hash have not yet been registered, so this bootstrap
module does not claim a `covered` traceability status for those clauses.

# Algorithms and derivations

- The byte stream is assembled independently from small project-authored literals in ascending
  indirect-object order.
- Each object offset is captured from the output byte count immediately before its object header.
  Conversion to `u64` is checked, then limited to the ten decimal digits available in a traditional
  xref entry.
- `/Length` is derived from the exact content byte slice and checked against the generator's
  non-negative PDF integer range.
- `startxref` is the checked byte position immediately before the `xref` keyword. `/Size` is the
  checked object count plus the mandatory free entry.

# External observations

None. No external engine was run, and no PDFium or other external implementation source was read,
copied, or translated for this module.

# Dependencies and generated data

- The crate uses only the Rust standard library and introduces no third-party dependency.
- All templates and source code are project-authored; no third-party data is embedded. Because the
  repository does not yet carry an approved project/test-data license, generated PDF bytes remain
  locally generated, non-redistributable, and ignored until the project owner records that approval.
- Generated PDFs contain `generated_by`, `input_hashes`, `generator_revision`, and `schema` PDF
  comments. `input_hashes=none` denotes the fixed, self-contained M0 template.
- The generator writes no generated fixture into the repository by default. The CLI requires an
  explicit output path.

# Tests and fuzz targets

Unit tests cover byte-for-byte determinism and metadata, all indirect-object xref offsets,
fixed-width xref records, trailer size/root, `startxref`, exact stream `/Length`, the xref offset
limit, the PDF integer length limit, and object-count overflow.

No fuzz target exists in this M0 slice. Future DSL and corruption-mode work must add property and
fuzz coverage before accepting unbounded or externally supplied generator inputs.

# Known deviations and unsupported cases

- This is the bootstrap beneath the full DSL required by RPE-ARCH-001 section 12.6. It currently
  emits only PDF 1.7, one fixed page, generation-zero objects, direct stream length, and a traditional
  xref table.
- Xref streams, hybrid references, indirect lengths, filters, object streams, incremental revisions,
  encryption, deliberate corruption, and configurable page/content objects are not implemented.
- The CLI writes the complete in-memory result to the explicit path; atomic destination replacement
  and output-budget configuration remain future tooling work.

# History

- 2026-07-13: Added the deterministic one-page M0 generator, CLI, checked serialization, and unit
  tests.
