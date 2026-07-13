# Scope

`tools/benchmark` records immutable benchmark environment metadata, benchmark scenario and timing
domains, non-empty raw nanosecond samples, deterministic descriptive quantiles, count-only sample
adequacy, exact Native/baseline duration endpoints with a finite display ratio, and canonical
schema-1 synthetic replay reports bound to a corpus manifest identity.

# Semantic owner

The Quality/Corpus workflow owns this development-only crate. It summarizes measurements supplied
by benchmark runners; it does not collect clocks, execute engines, choose corpora, or decide CI and
release acceptance. The executable M0 report profile validates only project-authored pipeline
smoke data and cannot represent a performance verdict.

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
  not expose a pass/fail or release-significance conclusion. The declared minimum is bounded by the
  fixed schema ceiling, independently of the caller's raw-sample resource limit; the latter limits
  only the samples actually decoded.
- The on-disk codec accepts a strict canonical TOML subset with fixed fields and ordering, sorted
  feature flags, canonical integers, bounded raw samples, and one final newline. Decode recomputes
  min/median/p95/p99/max/count from producer-order samples and rejects every stored-summary mismatch.
- Schema 1 accepts only `m0.synthetic-benchmark-replay.v1` with
  `performance_eligible = false`, `verdict = "not-evaluated"`, no external baseline, and an explicit
  unimplemented confidence-interval marker. It binds the exact canonical corpus manifest SHA-256.

# External observations

None. This crate does not invoke PDFium, browsers, GPU drivers, or any other benchmark target. No
external implementation source or output was used to define its statistics.

# Dependencies and generated data

- The crate uses the Rust standard library plus local `pdf-rs-corpus` and `pdf-rs-digest` crates,
  and forbids unsafe code. It has no third-party or network dependency.
- `tests/performance/m0-synthetic-benchmark-replay-v1.toml` is project-authored data that exercises
  serialization, summary recomputation, policy rejection, and T0 corpus identity binding. Its tiny
  integer samples are not clock measurements and are not product or baseline performance evidence.
- The fixture uses the pending project `LicenseRef-PDF.rs-SelfAuthored-Test`; redistribution remains
  disabled until project-owner approval, and this local record grants no distribution permission.
- The fixture contains explicit not-measured/not-applicable environment markers and no document
  bytes, user data, generated tables, or external datasets.

# Tests and fuzz targets

Unit tests cover nearest-rank median/p95/p99/min/max/count, input-order invariance, raw-order
preservation, empty input, count insufficiency, zero baseline, zero Native duration, `u64` precision
edges, finite ratio presentation, exact endpoint ordering, full metadata preservation, mandatory
metadata validation, schema versioning, cold/warm distinction, and engine/network timing distinction.

Report tests cover canonical byte identity, stored-statistic tampering, profile and non-verdict
policy enforcement, corpus ID/hash mismatch, schema/field/value failures, truncation, UTF-8,
file/line/string/feature/sample limits, symlink rejection, stable diagnostic policy, and redaction.
CLI tests cover command shape, successful non-verdict evidence, corpus binding failures, and
environment/value/path redaction. A repository test binds the report hash, canonical corpus hash,
data-ledger governance metadata, specification snapshot, and CI validation order.

No fuzz target exists in this M0 slice. The central `m0.parser-mutation-smoke.v1` quality integration
test replays 103 fixed, bounded anchor mutations against the canonical report under an explicit
2,048-byte decode limit. It checks exact outcome repeatability, canonical re-encoding on success,
non-internal failures, and environment/secret redaction. This ordinary regression test is not
coverage-guided or release-fuzz evidence; registered continuous fuzzing and automated minimization
remain required before measured or externally supplied reports are accepted.

# Known deviations and unsupported cases

- Confidence intervals, warm-up policy, outlier policy, noise models, hardware-pool validation, and
  historical-distribution regression analysis are not implemented.
- `SampleAdequacy::MeetsConfiguredMinimum` means only that a configured count floor was reached. It
  must not be interpreted as statistical significance, a CI pass, or a release gate.
- This crate does not compare correctness, peak memory, supported scope, visible area, or environment
  equivalence. Callers must establish those prerequisites before interpreting Native/baseline ratios.
- The on-disk schema cannot represent measured benchmark evidence. Real clocks, runner/context
  capture, warm-up and outlier policies, confidence intervals, hardware pools, Native product paths,
  correctness/memory/support companions, and PDFium comparison remain open.
- Report file paths are trusted developer/CI inputs. Symbolic report files are rejected, but robust
  no-follow handles for concurrent replacement and a wall-clock watchdog are not implemented.
- Successful validation proves canonical structure, recomputed descriptive statistics, policy
  markers, and corpus-manifest identity only. It is never a speed, regression, or release verdict.

# History

- 2026-07-13: Added versioned metadata, scenario/timing taxonomies, validated raw nanosecond samples,
  nearest-rank statistics, count-only adequacy, and checked Native/baseline ratios.
- 2026-07-13: Added bounded canonical synthetic replay reports, corpus binding, CLI evidence, and
  repository governance replay.
