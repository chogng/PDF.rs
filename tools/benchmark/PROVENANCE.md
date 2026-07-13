# Scope

`tools/benchmark` records immutable benchmark environment metadata, benchmark scenario and timing
domains, non-empty raw nanosecond samples, deterministic descriptive quantiles, count-only sample
adequacy, and exact Native/baseline duration endpoints with a finite display ratio.

# Semantic owner

The Quality/Corpus workflow owns this development-only crate. It summarizes measurements supplied
by benchmark runners; it does not collect clocks, execute engines, choose corpora, or decide CI and
release acceptance.

# Normative sources

- [RPE-ARCH-001, sections 12.21-12.23](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  defines required environment metadata, scenario distinctions, descriptive percentiles, controlled
  comparisons, and performance-governance boundaries.
- [RPE-ARCH-001, section 2.6 and appendix C](../../docs/architecture/independent_rust_pdf_engine_development_spec.md)
  requires separate engine/network timings, raw sample counts, cache state, corpus identity, and
  versioned benchmark result structure.
- [RPE-STD-003, section 17](../../docs/standards/testing-standard.md) requires commit, profile,
  feature, toolchain, OS, CPU/GPU, memory, browser, corpus, renderer/font/color epoch, cache state,
  raw samples, and median/p95/p99 evidence.
- [RPE-STD-001, sections 9 and 17](../../docs/standards/coding-standard.md) requires explicit numeric
  handling and median/p95/p99 reporting without quality/support-range distortion.
- [RPE-STD-004, sections 7 and 14](../../docs/standards/traceability-and-provenance.md) defines this
  provenance record and reproducible release evidence.

# Algorithms and derivations

- Samples remain integer nanoseconds and are preserved in producer order. Statistics sort a clone,
  so report generation cannot rewrite the raw evidence.
- Percentiles use the deterministic one-based nearest-rank definition: for percentile `p` and
  non-empty sample count `n`, select sorted rank `ceil(p * n / 100)`. Median uses `p=50`; p95 and p99
  use `95` and `99`.
- Rank calculation decomposes `n` into quotient/remainder around 100 and uses checked integer
  arithmetic. No floating-point value participates in ordering or quantile selection.
- Native/baseline ratios reject a zero baseline and retain both exact `u64` endpoints. The `f64`
  ratio is only a checked finite presentation; exact endpoint ordering remains available when large
  adjacent integers round to the same floating-point value.
- Sample adequacy compares only raw count with an explicit non-zero minimum. Its names and API do
  not expose a pass/fail or release-significance conclusion.

# External observations

None. This crate does not invoke PDFium, browsers, GPU drivers, or any other benchmark target. No
external implementation source or output was used to define its statistics.

# Dependencies and generated data

- The crate uses only the Rust standard library and forbids unsafe code.
- It contains no generated tables, fixtures, external datasets, or third-party dependencies.
- All metadata examples and raw samples in unit tests are synthetic and project-authored.

# Tests and fuzz targets

Unit tests cover nearest-rank median/p95/p99/min/max/count, input-order invariance, raw-order
preservation, empty input, count insufficiency, zero baseline, zero Native duration, `u64` precision
edges, finite ratio presentation, exact endpoint ordering, full metadata preservation, mandatory
metadata validation, schema versioning, cold/warm distinction, and engine/network timing distinction.

No fuzz target exists in this M0 slice. The public constructors bound state through validation and
checked arithmetic; report schema serialization and external runner ingestion will require fuzzing
when introduced.

# Known deviations and unsupported cases

- Confidence intervals, warm-up policy, outlier policy, noise models, hardware-pool validation, and
  historical-distribution regression analysis are not implemented.
- `SampleAdequacy::MeetsConfiguredMinimum` means only that a configured count floor was reached. It
  must not be interpreted as statistical significance, a CI pass, or a release gate.
- This crate does not compare correctness, peak memory, supported scope, visible area, or environment
  equivalence. Callers must establish those prerequisites before interpreting Native/baseline ratios.
- Serialization and a canonical on-disk benchmark result schema are deferred; all required metadata
  is retained in typed memory today.

# History

- 2026-07-13: Added versioned metadata, scenario/timing taxonomies, validated raw nanosecond samples,
  nearest-rank statistics, count-only adequacy, and checked Native/baseline ratios.
