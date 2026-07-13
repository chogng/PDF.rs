# Scope

`tools/generate` compiles deterministic, self-authored PDF fixture DSL sources for the PDF.rs test
system. The `m0.one-page-table.v1` profile emits PDF 1.7 with one page, a direct content-stream
length, a traditional cross-reference table, a trailer, and `startxref`.

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

- A bounded project-defined lexer/parser accepts the explicit `document`, `object`, `stream`, and
  `xref` grammar used by the M0 one-page profile. Source bytes, tokens, objects, decoded content,
  and emitted bytes each have caller-selected limits under fixed tool ceilings.
- The parser validates the fixed Catalog/Pages/Page/Stream topology and classifies other versions,
  filters, xref kinds, or object-graph variants as unsupported or invalid topology instead of
  silently approximating them.
- The byte stream is assembled independently from small project-authored literals in canonical
  indirect-object order.
- Each object offset is captured from the output byte count immediately before its object header.
  Conversion to `u64` is checked, then limited to the ten decimal digits available in a traditional
  xref entry.
- `/Length` is derived from the exact content byte slice and checked against the generator's
  non-negative PDF integer range.
- `startxref` is the checked byte position immediately before the `xref` keyword. `/Size` is the
  checked object count plus the mandatory free entry.
- Generated metadata binds the SHA-256 of the exact DSL source; the returned artifact also records
  the SHA-256 of the complete generated PDF.

# External observations

A post-implementation compatibility audit read the stream-termination handling in the local PDFium
checkout at revision `c040cf96106a87220b814a1a892649cf2d7f1934`, after the independent serializer
and delimiter test were already in place. The observation was not used as a normative source; no
PDFium code, constants, or state machine were copied or translated.
The same audit ran Poppler `pdfinfo` 26.05.0 over the ignored generated fixture and observed one
unencrypted, non-suspect PDF 1.7 page at 200 by 200 points. This behavioral observation was made
after implementation and was not treated as specification truth or a source for generator logic.

# Dependencies and generated data

- The crate uses the Rust standard library plus the local development-only `pdf-rs-digest` crate;
  it introduces no third-party dependency.
- All templates and source code are project-authored; no third-party data is embedded. Because the
  repository does not yet carry an approved project/test-data license, generated PDF bytes remain
  locally generated, non-redistributable, and ignored until the project owner records that approval.
- Generated PDFs contain `generated_by`, `input_hashes`, `generator_revision`, and `schema` PDF
  comments. `input_hashes` contains the exact DSL source SHA-256 for replay verification.
- The generator writes no generated fixture into the repository by default. The CLI requires an
  explicit DSL source and output path and reads the source through the same fixed input ceiling.

# Tests and fuzz targets

Unit tests cover byte-for-byte determinism and source/output identity, tokenization and string
escapes, configurable MediaBox/content, all indirect-object xref offsets, fixed-width xref records,
trailer size/root, `startxref`, exact stream `/Length`, malformed and unsupported input, stable
redacted errors, and exact source/token/object/content/output limit boundaries. An integration test
replays the repository `source.dsl` and compares it byte-for-byte with the canonical generator API;
CLI integration tests cover argument arity, source replay, source-redacted diagnostics, and rejection
of non-file and oversized inputs, plus symbolic-link rejection on Unix. CI then regenerates the
adjacent ignored PDF before manifest and bundle validation.

No continuous fuzz target exists in this M0 slice. Deterministic truncation and selected mutation
cases are covered in unit tests. The central `m0.parser-mutation-smoke.v1` quality integration test
also replays 103 fixed, bounded anchor mutations against the canonical DSL and checks exact outcome
repeatability without logging source bytes. It is not coverage-guided or release-fuzz evidence;
future grammar and corruption-mode work must add a registered fuzz target, seed corpus, dictionary,
timeout, and structure-aware minimizer.

# Known deviations and unsupported cases

- This is a deliberately narrow executable profile beneath the full DSL required by RPE-ARCH-001
  section 12.6. It emits only PDF 1.7, one fixed four-object page topology, generation-zero objects,
  direct stream length, an empty Resources dictionary, and a traditional xref table.
- Xref streams, hybrid references, indirect lengths, filters, object streams, incremental revisions,
  encryption, deliberate corruption, additional objects/resources, and object-graph variants are
  not implemented.
- The CLI writes the complete in-memory result to the explicit path; atomic destination replacement
  and output-budget configuration remain future tooling work. CLI source/output paths are trusted
  developer or CI inputs; robust no-follow opening for concurrently mutable untrusted directories is
  not implemented.

# History

- 2026-07-13: Added the deterministic one-page M0 generator, CLI, checked serialization, and unit
  tests.
- 2026-07-13: Added bounded `m0.one-page-table.v1` DSL compilation, source-bound metadata, stable
  diagnostics, and repository fixture replay.
