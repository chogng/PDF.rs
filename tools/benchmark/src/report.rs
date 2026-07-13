use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::Path;

use pdf_rs_corpus::OnDiskCorpusManifest;
use pdf_rs_digest::{hex_digest, sha256};

use crate::{
    BenchmarkMetadata, BenchmarkMetadataInput, BenchmarkScenario, BenchmarkSchemaVersion,
    BenchmarkSummary, CacheState, MinimumSampleCount, RawNanosecondSamples, SampleAdequacy,
    TimingDomain,
};

/// Schema version of the canonical on-disk benchmark report.
pub const BENCHMARK_REPORT_SCHEMA: u32 = 1;

/// Only executable on-disk report profile in the M0 benchmark slice.
pub const SYNTHETIC_BENCHMARK_PROFILE: &str = "m0.synthetic-benchmark-replay.v1";

const SYNTHETIC_EVIDENCE_NAME: &str = "synthetic-pipeline-smoke";
const NOT_EVALUATED: &str = "not-evaluated";
const CONFIDENCE_NOT_IMPLEMENTED: &str = "not-implemented-m0";
const EXTERNAL_BASELINE_ABSENT: &str = "absent";
const SAMPLE_COUNT_INSUFFICIENT: &str = "insufficient";
const SAMPLE_COUNT_MEETS_MINIMUM: &str = "meets-configured-minimum";

const HARD_MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;
const HARD_MAX_LINES: usize = 4096;
const HARD_MAX_FEATURE_FLAGS: usize = 4096;
const HARD_MAX_SAMPLES: usize = 1_000_000;
const HARD_MAX_STRING_BYTES: usize = 64 * 1024;

const FIELDS: &[&str] = &[
    "schema",
    "id",
    "evidence_class",
    "commit",
    "profile",
    "feature_flags",
    "toolchain",
    "os",
    "cpu",
    "gpu",
    "memory",
    "browser",
    "corpus_id",
    "corpus_hash",
    "renderer_epoch",
    "font_epoch",
    "color_epoch",
    "cache_state",
    "scenario",
    "timing_domain",
    "warmup_iterations",
    "minimum_sample_count",
    "raw_samples_ns",
    "minimum_ns",
    "median_ns",
    "p95_ns",
    "p99_ns",
    "maximum_ns",
    "sample_count",
    "sample_count_status",
    "performance_eligible",
    "confidence_interval",
    "external_baseline",
    "verdict",
];

/// Deterministic resource ceilings for benchmark report decoding and encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BenchmarkReportLimits {
    max_report_bytes: usize,
    max_lines: usize,
    max_feature_flags: usize,
    max_samples: usize,
    max_string_bytes: usize,
}

impl BenchmarkReportLimits {
    /// Creates non-zero limits under the benchmark tool's fixed hard ceilings.
    pub fn new(
        max_report_bytes: usize,
        max_lines: usize,
        max_feature_flags: usize,
        max_samples: usize,
        max_string_bytes: usize,
    ) -> Result<Self, BenchmarkReportError> {
        if max_report_bytes == 0
            || max_report_bytes > HARD_MAX_REPORT_BYTES
            || max_lines == 0
            || max_lines > HARD_MAX_LINES
            || max_feature_flags == 0
            || max_feature_flags > HARD_MAX_FEATURE_FLAGS
            || max_samples == 0
            || max_samples > HARD_MAX_SAMPLES
            || max_string_bytes == 0
            || max_string_bytes > HARD_MAX_STRING_BYTES
        {
            return Err(report_error(BenchmarkReportErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_report_bytes,
            max_lines,
            max_feature_flags,
            max_samples,
            max_string_bytes,
        })
    }

    /// Returns the maximum accepted report bytes.
    pub const fn max_report_bytes(self) -> usize {
        self.max_report_bytes
    }

    /// Returns the maximum physical report lines.
    pub const fn max_lines(self) -> usize {
        self.max_lines
    }

    /// Returns the maximum feature flag count.
    pub const fn max_feature_flags(self) -> usize {
        self.max_feature_flags
    }

    /// Returns the maximum raw sample count.
    pub const fn max_samples(self) -> usize {
        self.max_samples
    }

    /// Returns the maximum decoded bytes in one string.
    pub const fn max_string_bytes(self) -> usize {
        self.max_string_bytes
    }
}

impl Default for BenchmarkReportLimits {
    fn default() -> Self {
        Self {
            max_report_bytes: 1024 * 1024,
            max_lines: 256,
            max_feature_flags: 128,
            max_samples: 100_000,
            max_string_bytes: 4096,
        }
    }
}

/// Exact machine-readable benchmark report failure code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BenchmarkReportErrorCode {
    /// Caller-supplied limits are zero or exceed a hard ceiling.
    InvalidLimits,
    /// Report bytes exceed the configured ceiling.
    ReportLimit,
    /// Physical lines exceed the configured ceiling.
    LineLimit,
    /// Feature flags exceed the configured ceiling.
    FeatureFlagLimit,
    /// Raw samples exceed the configured limit, or their declared minimum exceeds the schema ceiling.
    SampleLimit,
    /// A decoded string exceeds the configured ceiling.
    StringLimit,
    /// Report bytes are not valid UTF-8.
    InvalidUtf8,
    /// Text violates the supported TOML subset.
    InvalidSyntax,
    /// The report schema is not supported.
    UnsupportedSchema,
    /// A field is not defined by schema 1.
    UnknownField,
    /// A field appears more than once.
    DuplicateField,
    /// A mandatory field is absent.
    MissingField,
    /// A field value violates the synthetic profile contract.
    InvalidValue,
    /// Stored summary fields do not equal statistics recomputed from raw samples.
    StatisticsMismatch,
    /// Valid semantics were not encoded in the unique canonical byte form.
    NonCanonical,
    /// The report file is missing, symbolic, or not a regular file.
    ReportUnavailable,
    /// The report names a different corpus manifest.
    CorpusIdMismatch,
    /// The report binds a different corpus manifest byte identity.
    CorpusHashMismatch,
    /// Descriptive statistics failed despite validated sample limits.
    StatisticsFailed,
    /// SHA-256 framing failed inside configured byte ceilings.
    HashFailed,
}

/// Stable coarse category for benchmark report failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchmarkReportErrorCategory {
    /// Caller-supplied limits are invalid.
    Configuration,
    /// A deterministic resource ceiling was reached.
    ResourceLimit,
    /// Report text violates the supported grammar.
    Syntax,
    /// Report text requests an unsupported schema.
    Unsupported,
    /// Parsed data violates the report structure or executable profile.
    Structure,
    /// A required report file is unavailable.
    Availability,
    /// A content identity binding failed.
    Integrity,
    /// Checked internal statistics or hashing failed.
    Internal,
}

/// Stable recovery class for benchmark report failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchmarkReportRecoverability {
    /// Supply limits within the documented ceilings.
    CorrectConfiguration,
    /// Reduce the bounded report workload.
    ReduceInput,
    /// Correct and canonically re-encode the report.
    CorrectReport,
    /// Select an implemented report schema.
    SelectSupportedSchema,
    /// Restore the expected report file.
    RestoreReport,
    /// Select or restore the corpus manifest named by the report.
    CorrectCorpus,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Stable, report-content- and environment-redacted benchmark failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BenchmarkReportError {
    /// Exact machine-readable failure code.
    pub code: BenchmarkReportErrorCode,
    /// Coarse failure category.
    pub category: BenchmarkReportErrorCategory,
    /// Approved recovery class.
    pub recoverability: BenchmarkReportRecoverability,
    /// Stable project diagnostic identifier.
    pub diagnostic_id: &'static str,
    /// One-based report line when applicable.
    pub line: Option<usize>,
    detail: &'static str,
}

impl fmt::Display for BenchmarkReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.diagnostic_id, self.code, self.detail
        )?;
        if let Some(line) = self.line {
            write!(formatter, " at line {line}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BenchmarkReportError {}

/// Governance class of one executable on-disk benchmark report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum BenchmarkEvidenceClass {
    /// Project-authored numbers that test replay and validation, never performance.
    SyntheticPipelineSmoke,
}

impl BenchmarkEvidenceClass {
    /// Returns the stable report spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyntheticPipelineSmoke => SYNTHETIC_EVIDENCE_NAME,
        }
    }
}

/// Canonically decoded synthetic benchmark report and exact byte identity.
///
/// This type deliberately has no `Debug` implementation because it owns build
/// and environment metadata that is not approved diagnostic content.
#[derive(Clone, Eq, PartialEq)]
pub struct OnDiskBenchmarkReport {
    id: String,
    evidence_class: BenchmarkEvidenceClass,
    metadata: BenchmarkMetadata,
    scenario: BenchmarkScenario,
    timing_domain: TimingDomain,
    warmup_iterations: u32,
    minimum_sample_count: MinimumSampleCount,
    raw_samples: RawNanosecondSamples,
    summary: BenchmarkSummary,
    source_sha256: [u8; 32],
}

impl OnDiskBenchmarkReport {
    /// Returns the stable report identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the evidence governance class.
    pub const fn evidence_class(&self) -> BenchmarkEvidenceClass {
        self.evidence_class
    }

    /// Returns the validated environment and corpus metadata.
    pub fn metadata(&self) -> &BenchmarkMetadata {
        &self.metadata
    }

    /// Returns the benchmark scenario taxonomy value.
    pub const fn scenario(&self) -> BenchmarkScenario {
        self.scenario
    }

    /// Returns the timing boundary taxonomy value.
    pub const fn timing_domain(&self) -> TimingDomain {
        self.timing_domain
    }

    /// Returns the declared warm-up iteration count.
    pub const fn warmup_iterations(&self) -> u32 {
        self.warmup_iterations
    }

    /// Returns the count-only reporting minimum.
    pub const fn minimum_sample_count(&self) -> MinimumSampleCount {
        self.minimum_sample_count
    }

    /// Returns raw samples in producer order.
    pub fn raw_samples(&self) -> &RawNanosecondSamples {
        &self.raw_samples
    }

    /// Returns recomputed descriptive statistics and count-only adequacy.
    pub const fn summary(&self) -> BenchmarkSummary {
        self.summary
    }

    /// Returns the SHA-256 of the exact canonical report bytes.
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }

    /// Returns `false`; schema 1 synthetic reports can never gate performance.
    pub const fn performance_eligible(&self) -> bool {
        false
    }

    /// Returns the explicit non-verdict marker.
    pub const fn verdict(&self) -> &'static str {
        NOT_EVALUATED
    }

    /// Returns the explicit confidence-interval status.
    pub const fn confidence_interval_status(&self) -> &'static str {
        CONFIDENCE_NOT_IMPLEMENTED
    }

    /// Returns the explicit external-baseline status.
    pub const fn external_baseline_status(&self) -> &'static str {
        EXTERNAL_BASELINE_ABSENT
    }

    /// Returns the count-only sample status spelling.
    pub const fn sample_count_status(&self) -> &'static str {
        adequacy_name(self.summary.adequacy)
    }
}

/// Decodes only the canonical schema-1 synthetic benchmark report subset.
pub fn decode_report(
    input: &[u8],
    limits: BenchmarkReportLimits,
) -> Result<OnDiskBenchmarkReport, BenchmarkReportError> {
    if input.len() > limits.max_report_bytes {
        return Err(report_error(BenchmarkReportErrorCode::ReportLimit, None));
    }
    let text = std::str::from_utf8(input).map_err(|error| {
        report_error(
            BenchmarkReportErrorCode::InvalidUtf8,
            Some(line_for_offset(input, error.valid_up_to())),
        )
    })?;
    let (schema, schema_line) = detect_schema(text, limits)?;
    if schema != u64::from(BENCHMARK_REPORT_SCHEMA) {
        return Err(report_error(
            BenchmarkReportErrorCode::UnsupportedSchema,
            Some(schema_line),
        ));
    }
    let fields = parse_document(text, limits)?;
    let mut report = build_report(fields, limits)?;
    report.source_sha256 =
        sha256(input).map_err(|_| report_error(BenchmarkReportErrorCode::HashFailed, None))?;
    let canonical = encode_report(&report, limits)?;
    if canonical != input {
        return Err(report_error(BenchmarkReportErrorCode::NonCanonical, None));
    }
    Ok(report)
}

/// Encodes a decoded report into its unique canonical schema-1 TOML bytes.
pub fn encode_report(
    report: &OnDiskBenchmarkReport,
    limits: BenchmarkReportLimits,
) -> Result<Vec<u8>, BenchmarkReportError> {
    validate_encode_limits(report, limits)?;
    let mut output = ReportOutput::new(limits);
    write_u64_field(&mut output, "schema", u64::from(BENCHMARK_REPORT_SCHEMA))?;
    write_string_field(&mut output, "id", report.id())?;
    write_string_field(
        &mut output,
        "evidence_class",
        report.evidence_class().as_str(),
    )?;
    write_string_field(&mut output, "commit", report.metadata().commit())?;
    write_string_field(&mut output, "profile", report.metadata().profile())?;
    write_string_array_field(
        &mut output,
        "feature_flags",
        report.metadata().feature_flags(),
    )?;
    write_string_field(&mut output, "toolchain", report.metadata().toolchain())?;
    write_string_field(&mut output, "os", report.metadata().os())?;
    write_string_field(&mut output, "cpu", report.metadata().cpu())?;
    write_string_field(&mut output, "gpu", report.metadata().gpu())?;
    write_string_field(&mut output, "memory", report.metadata().memory())?;
    write_string_field(&mut output, "browser", report.metadata().browser())?;
    write_string_field(&mut output, "corpus_id", report.metadata().corpus_id())?;
    write_string_field(&mut output, "corpus_hash", report.metadata().corpus_hash())?;
    write_string_field(
        &mut output,
        "renderer_epoch",
        report.metadata().renderer_epoch(),
    )?;
    write_string_field(&mut output, "font_epoch", report.metadata().font_epoch())?;
    write_string_field(&mut output, "color_epoch", report.metadata().color_epoch())?;
    write_string_field(
        &mut output,
        "cache_state",
        report.metadata().cache_state().as_str(),
    )?;
    write_string_field(&mut output, "scenario", report.scenario().as_str())?;
    write_string_field(
        &mut output,
        "timing_domain",
        report.timing_domain().as_str(),
    )?;
    write_u64_field(
        &mut output,
        "warmup_iterations",
        u64::from(report.warmup_iterations()),
    )?;
    write_u64_field(
        &mut output,
        "minimum_sample_count",
        usize_to_u64(report.minimum_sample_count().get())?,
    )?;
    write_sample_array_field(&mut output, "raw_samples_ns", report.raw_samples())?;
    let statistics = report.summary().statistics;
    write_u64_field(&mut output, "minimum_ns", statistics.minimum.get())?;
    write_u64_field(&mut output, "median_ns", statistics.median.get())?;
    write_u64_field(&mut output, "p95_ns", statistics.p95.get())?;
    write_u64_field(&mut output, "p99_ns", statistics.p99.get())?;
    write_u64_field(&mut output, "maximum_ns", statistics.maximum.get())?;
    write_u64_field(
        &mut output,
        "sample_count",
        usize_to_u64(statistics.sample_count)?,
    )?;
    write_string_field(
        &mut output,
        "sample_count_status",
        report.sample_count_status(),
    )?;
    write_bool_field(&mut output, "performance_eligible", false)?;
    write_string_field(
        &mut output,
        "confidence_interval",
        report.confidence_interval_status(),
    )?;
    write_string_field(
        &mut output,
        "external_baseline",
        report.external_baseline_status(),
    )?;
    write_string_field(&mut output, "verdict", report.verdict())?;
    Ok(output.into_bytes())
}

/// Reads and decodes one canonical benchmark report without loading a corpus.
pub fn load_report_file(
    path: &Path,
    limits: BenchmarkReportLimits,
) -> Result<OnDiskBenchmarkReport, BenchmarkReportError> {
    let bytes = read_report_bytes(path, limits)?;
    decode_report(&bytes, limits)
}

/// Verifies that a report names the exact canonical corpus manifest supplied.
pub fn validate_report_corpus(
    report: &OnDiskBenchmarkReport,
    corpus: &OnDiskCorpusManifest,
) -> Result<(), BenchmarkReportError> {
    if report.metadata().corpus_id() != corpus.manifest().id() {
        return Err(report_error(
            BenchmarkReportErrorCode::CorpusIdMismatch,
            None,
        ));
    }
    let expected_hash = format!("sha256:{}", hex_digest(&corpus.source_sha256()));
    if report.metadata().corpus_hash() != expected_hash {
        return Err(report_error(
            BenchmarkReportErrorCode::CorpusHashMismatch,
            None,
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct RawValue<'a> {
    value: &'a str,
    line: usize,
}

fn parse_document<'a>(
    text: &'a str,
    limits: BenchmarkReportLimits,
) -> Result<BTreeMap<&'a str, RawValue<'a>>, BenchmarkReportError> {
    let mut fields = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line_number > limits.max_lines {
            return Err(report_error(
                BenchmarkReportErrorCode::LineLimit,
                Some(line_number),
            ));
        }
        let (key, value) = split_assignment(line).ok_or_else(|| {
            report_error(BenchmarkReportErrorCode::InvalidSyntax, Some(line_number))
        })?;
        if !FIELDS.contains(&key) {
            return Err(report_error(
                BenchmarkReportErrorCode::UnknownField,
                Some(line_number),
            ));
        }
        if fields
            .insert(
                key,
                RawValue {
                    value,
                    line: line_number,
                },
            )
            .is_some()
        {
            return Err(report_error(
                BenchmarkReportErrorCode::DuplicateField,
                Some(line_number),
            ));
        }
    }
    if fields.len() != FIELDS.len() {
        return Err(report_error(BenchmarkReportErrorCode::MissingField, None));
    }
    Ok(fields)
}

fn detect_schema(
    text: &str,
    _limits: BenchmarkReportLimits,
) -> Result<(u64, usize), BenchmarkReportError> {
    let first_line = text
        .lines()
        .next()
        .ok_or_else(|| report_error(BenchmarkReportErrorCode::MissingField, None))?;
    let (key, value) = split_assignment(first_line)
        .ok_or_else(|| report_error(BenchmarkReportErrorCode::InvalidSyntax, Some(1)))?;
    if key != "schema" {
        return Err(report_error(
            BenchmarkReportErrorCode::MissingField,
            Some(1),
        ));
    }
    Ok((parse_u64(RawValue { value, line: 1 })?, 1))
}

fn build_report(
    fields: BTreeMap<&str, RawValue<'_>>,
    limits: BenchmarkReportLimits,
) -> Result<OnDiskBenchmarkReport, BenchmarkReportError> {
    let schema_value = field(&fields, "schema")?;
    let schema = parse_u64(schema_value)?;
    if schema != u64::from(BENCHMARK_REPORT_SCHEMA) {
        return Err(report_error(
            BenchmarkReportErrorCode::UnsupportedSchema,
            Some(schema_value.line),
        ));
    }

    let id_value = field(&fields, "id")?;
    let id = parse_string(id_value, limits)?;
    if !is_stable_id(&id) {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(id_value.line),
        ));
    }

    let evidence_value = field(&fields, "evidence_class")?;
    let evidence_name = parse_string(evidence_value, limits)?;
    if evidence_name != SYNTHETIC_EVIDENCE_NAME {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(evidence_value.line),
        ));
    }

    let profile_value = field(&fields, "profile")?;
    let profile = parse_string(profile_value, limits)?;
    if profile != SYNTHETIC_BENCHMARK_PROFILE {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(profile_value.line),
        ));
    }

    let feature_value = field(&fields, "feature_flags")?;
    let feature_flags = parse_string_array(feature_value, limits)?;
    if feature_flags.iter().any(|flag| !is_stable_token(flag))
        || feature_flags.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(feature_value.line),
        ));
    }

    let corpus_hash_value = field(&fields, "corpus_hash")?;
    let corpus_hash = parse_string(corpus_hash_value, limits)?;
    if !is_sha256_identity(&corpus_hash) {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(corpus_hash_value.line),
        ));
    }
    let corpus_id_value = field(&fields, "corpus_id")?;
    let corpus_id = parse_string(corpus_id_value, limits)?;
    if !is_stable_id(&corpus_id) {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(corpus_id_value.line),
        ));
    }

    let cache_value = field(&fields, "cache_state")?;
    let cache_name = parse_string(cache_value, limits)?;
    let cache_state = CacheState::parse(&cache_name).ok_or_else(|| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(cache_value.line),
        )
    })?;
    let scenario_value = field(&fields, "scenario")?;
    let scenario_name = parse_string(scenario_value, limits)?;
    let scenario = BenchmarkScenario::parse(&scenario_name).ok_or_else(|| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(scenario_value.line),
        )
    })?;
    if (scenario == BenchmarkScenario::ColdOpen && cache_state != CacheState::Cold)
        || (scenario == BenchmarkScenario::WarmReopen && cache_state != CacheState::Warm)
    {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(cache_value.line.min(scenario_value.line)),
        ));
    }
    let domain_value = field(&fields, "timing_domain")?;
    let domain_name = parse_string(domain_value, limits)?;
    let timing_domain = TimingDomain::parse(&domain_name).ok_or_else(|| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(domain_value.line),
        )
    })?;

    let warmup_value = field(&fields, "warmup_iterations")?;
    let warmup_iterations = u32::try_from(parse_u64(warmup_value)?).map_err(|_| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(warmup_value.line),
        )
    })?;
    let minimum_value = field(&fields, "minimum_sample_count")?;
    let minimum_u64 = parse_u64(minimum_value)?;
    let maximum_samples = u64::try_from(HARD_MAX_SAMPLES)
        .map_err(|_| report_error(BenchmarkReportErrorCode::StatisticsFailed, None))?;
    if minimum_u64 > maximum_samples {
        return Err(report_error(
            BenchmarkReportErrorCode::SampleLimit,
            Some(minimum_value.line),
        ));
    }
    let minimum_usize = usize::try_from(minimum_u64).map_err(|_| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(minimum_value.line),
        )
    })?;
    let minimum_sample_count = MinimumSampleCount::new(minimum_usize).ok_or_else(|| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(minimum_value.line),
        )
    })?;

    let samples_value = field(&fields, "raw_samples_ns")?;
    let raw_values = parse_u64_array(samples_value, limits)?;
    let raw_samples = RawNanosecondSamples::new(raw_values).map_err(|_| {
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(samples_value.line),
        )
    })?;
    let summary = raw_samples
        .summarize(minimum_sample_count)
        .map_err(|_| report_error(BenchmarkReportErrorCode::StatisticsFailed, None))?;
    validate_stored_summary(&fields, summary)?;

    let status_value = field(&fields, "sample_count_status")?;
    let status = parse_string(status_value, limits)?;
    if status != adequacy_name(summary.adequacy) {
        return Err(report_error(
            BenchmarkReportErrorCode::StatisticsMismatch,
            Some(status_value.line),
        ));
    }
    let eligible_value = field(&fields, "performance_eligible")?;
    if parse_bool(eligible_value)? {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(eligible_value.line),
        ));
    }
    require_fixed_string(
        &fields,
        "confidence_interval",
        CONFIDENCE_NOT_IMPLEMENTED,
        limits,
    )?;
    require_fixed_string(
        &fields,
        "external_baseline",
        EXTERNAL_BASELINE_ABSENT,
        limits,
    )?;
    require_fixed_string(&fields, "verdict", NOT_EVALUATED, limits)?;

    let metadata = BenchmarkMetadata::new(BenchmarkMetadataInput {
        schema: BenchmarkSchemaVersion::V1,
        commit: parse_field_string(&fields, "commit", limits)?,
        profile,
        feature_flags,
        toolchain: parse_field_string(&fields, "toolchain", limits)?,
        os: parse_field_string(&fields, "os", limits)?,
        cpu: parse_field_string(&fields, "cpu", limits)?,
        gpu: parse_field_string(&fields, "gpu", limits)?,
        memory: parse_field_string(&fields, "memory", limits)?,
        browser: parse_field_string(&fields, "browser", limits)?,
        corpus_id,
        corpus_hash,
        renderer_epoch: parse_field_string(&fields, "renderer_epoch", limits)?,
        font_epoch: parse_field_string(&fields, "font_epoch", limits)?,
        color_epoch: parse_field_string(&fields, "color_epoch", limits)?,
        cache_state,
    })
    .map_err(|error| {
        let field_name = if error.field.as_str() == "feature_flag" {
            "feature_flags"
        } else {
            error.field.as_str()
        };
        report_error(
            BenchmarkReportErrorCode::InvalidValue,
            fields.get(field_name).map(|raw| raw.line),
        )
    })?;

    Ok(OnDiskBenchmarkReport {
        id,
        evidence_class: BenchmarkEvidenceClass::SyntheticPipelineSmoke,
        metadata,
        scenario,
        timing_domain,
        warmup_iterations,
        minimum_sample_count,
        raw_samples,
        summary,
        source_sha256: [0; 32],
    })
}

fn validate_encode_limits(
    report: &OnDiskBenchmarkReport,
    limits: BenchmarkReportLimits,
) -> Result<(), BenchmarkReportError> {
    if report.metadata().feature_flags().len() > limits.max_feature_flags {
        return Err(report_error(
            BenchmarkReportErrorCode::FeatureFlagLimit,
            None,
        ));
    }
    if report.raw_samples().len() > limits.max_samples {
        return Err(report_error(BenchmarkReportErrorCode::SampleLimit, None));
    }
    let strings = [
        report.id(),
        report.evidence_class().as_str(),
        report.metadata().commit(),
        report.metadata().profile(),
        report.metadata().toolchain(),
        report.metadata().os(),
        report.metadata().cpu(),
        report.metadata().gpu(),
        report.metadata().memory(),
        report.metadata().browser(),
        report.metadata().corpus_id(),
        report.metadata().corpus_hash(),
        report.metadata().renderer_epoch(),
        report.metadata().font_epoch(),
        report.metadata().color_epoch(),
        report.metadata().cache_state().as_str(),
        report.scenario().as_str(),
        report.timing_domain().as_str(),
        report.sample_count_status(),
        report.confidence_interval_status(),
        report.external_baseline_status(),
        report.verdict(),
    ];
    if strings
        .iter()
        .copied()
        .chain(report.metadata().feature_flags().iter().map(String::as_str))
        .any(|value| value.len() > limits.max_string_bytes)
    {
        return Err(report_error(BenchmarkReportErrorCode::StringLimit, None));
    }
    Ok(())
}

fn validate_stored_summary(
    fields: &BTreeMap<&str, RawValue<'_>>,
    summary: BenchmarkSummary,
) -> Result<(), BenchmarkReportError> {
    let statistics = summary.statistics;
    for (name, expected) in [
        ("minimum_ns", statistics.minimum.get()),
        ("median_ns", statistics.median.get()),
        ("p95_ns", statistics.p95.get()),
        ("p99_ns", statistics.p99.get()),
        ("maximum_ns", statistics.maximum.get()),
        ("sample_count", usize_to_u64(statistics.sample_count)?),
    ] {
        let raw = field(fields, name)?;
        if parse_u64(raw)? != expected {
            return Err(report_error(
                BenchmarkReportErrorCode::StatisticsMismatch,
                Some(raw.line),
            ));
        }
    }
    Ok(())
}

fn require_fixed_string(
    fields: &BTreeMap<&str, RawValue<'_>>,
    name: &str,
    expected: &str,
    limits: BenchmarkReportLimits,
) -> Result<(), BenchmarkReportError> {
    let raw = field(fields, name)?;
    if parse_string(raw, limits)? != expected {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidValue,
            Some(raw.line),
        ));
    }
    Ok(())
}

fn parse_field_string(
    fields: &BTreeMap<&str, RawValue<'_>>,
    name: &str,
    limits: BenchmarkReportLimits,
) -> Result<String, BenchmarkReportError> {
    parse_string(field(fields, name)?, limits)
}

fn field<'a>(
    fields: &BTreeMap<&'a str, RawValue<'a>>,
    name: &str,
) -> Result<RawValue<'a>, BenchmarkReportError> {
    fields
        .get(name)
        .copied()
        .ok_or_else(|| report_error(BenchmarkReportErrorCode::MissingField, None))
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    if line.ends_with('\r') || line.trim().is_empty() {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty()
        || value.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    Some((key, value))
}

fn parse_string(
    raw: RawValue<'_>,
    limits: BenchmarkReportLimits,
) -> Result<String, BenchmarkReportError> {
    let value = raw.value;
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        ));
    }
    let mut characters = value[1..value.len() - 1].chars();
    let mut output = String::new();
    while let Some(character) = characters.next() {
        let decoded = if character == '\\' {
            match characters.next() {
                Some('"') => '"',
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('u') => {
                    let mut scalar = 0_u32;
                    for _ in 0..4 {
                        let digit = characters.next().and_then(|value| value.to_digit(16));
                        scalar = scalar
                            .checked_mul(16)
                            .and_then(|value| digit.and_then(|digit| value.checked_add(digit)))
                            .ok_or_else(|| {
                                report_error(
                                    BenchmarkReportErrorCode::InvalidSyntax,
                                    Some(raw.line),
                                )
                            })?;
                    }
                    char::from_u32(scalar).ok_or_else(|| {
                        report_error(BenchmarkReportErrorCode::InvalidSyntax, Some(raw.line))
                    })?
                }
                _ => {
                    return Err(report_error(
                        BenchmarkReportErrorCode::InvalidSyntax,
                        Some(raw.line),
                    ));
                }
            }
        } else {
            character
        };
        if decoded.is_control() {
            return Err(report_error(
                BenchmarkReportErrorCode::InvalidValue,
                Some(raw.line),
            ));
        }
        let mut encoded = [0; 4];
        let encoded = decoded.encode_utf8(&mut encoded);
        let next_len = output
            .len()
            .checked_add(encoded.len())
            .ok_or_else(|| report_error(BenchmarkReportErrorCode::StringLimit, Some(raw.line)))?;
        if next_len > limits.max_string_bytes {
            return Err(report_error(
                BenchmarkReportErrorCode::StringLimit,
                Some(raw.line),
            ));
        }
        output
            .try_reserve(encoded.len())
            .map_err(|_| report_error(BenchmarkReportErrorCode::StringLimit, Some(raw.line)))?;
        output.push_str(encoded);
    }
    Ok(output)
}

fn parse_string_array(
    raw: RawValue<'_>,
    limits: BenchmarkReportLimits,
) -> Result<Vec<String>, BenchmarkReportError> {
    let value = raw.value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        ));
    }
    let interior = value[1..value.len() - 1].trim();
    if interior.is_empty() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in interior.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                push_string_array_part(
                    &mut output,
                    interior[start..index].trim(),
                    raw.line,
                    limits,
                )?;
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted || escaped {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        ));
    }
    push_string_array_part(&mut output, interior[start..].trim(), raw.line, limits)?;
    Ok(output)
}

fn push_string_array_part(
    output: &mut Vec<String>,
    part: &str,
    line: usize,
    limits: BenchmarkReportLimits,
) -> Result<(), BenchmarkReportError> {
    if output.len() >= limits.max_feature_flags {
        return Err(report_error(
            BenchmarkReportErrorCode::FeatureFlagLimit,
            Some(line),
        ));
    }
    output
        .try_reserve(1)
        .map_err(|_| report_error(BenchmarkReportErrorCode::FeatureFlagLimit, Some(line)))?;
    output.push(parse_string(RawValue { value: part, line }, limits)?);
    Ok(())
}

fn parse_u64_array(
    raw: RawValue<'_>,
    limits: BenchmarkReportLimits,
) -> Result<Vec<u64>, BenchmarkReportError> {
    let value = raw.value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        ));
    }
    let interior = value[1..value.len() - 1].trim();
    if interior.is_empty() {
        return Ok(Vec::new());
    }
    let mut output = Vec::new();
    for part in interior.split(',') {
        if output.len() >= limits.max_samples {
            return Err(report_error(
                BenchmarkReportErrorCode::SampleLimit,
                Some(raw.line),
            ));
        }
        output
            .try_reserve(1)
            .map_err(|_| report_error(BenchmarkReportErrorCode::SampleLimit, Some(raw.line)))?;
        output.push(parse_u64(RawValue {
            value: part.trim(),
            line: raw.line,
        })?);
    }
    Ok(output)
}

fn parse_u64(raw: RawValue<'_>) -> Result<u64, BenchmarkReportError> {
    if raw.value.is_empty()
        || !raw.value.bytes().all(|byte| byte.is_ascii_digit())
        || (raw.value.len() > 1 && raw.value.starts_with('0'))
    {
        return Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        ));
    }
    raw.value
        .parse()
        .map_err(|_| report_error(BenchmarkReportErrorCode::InvalidValue, Some(raw.line)))
}

fn parse_bool(raw: RawValue<'_>) -> Result<bool, BenchmarkReportError> {
    match raw.value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(report_error(
            BenchmarkReportErrorCode::InvalidSyntax,
            Some(raw.line),
        )),
    }
}

const fn adequacy_name(value: SampleAdequacy) -> &'static str {
    match value {
        SampleAdequacy::Insufficient { .. } => SAMPLE_COUNT_INSUFFICIENT,
        SampleAdequacy::MeetsConfiguredMinimum { .. } => SAMPLE_COUNT_MEETS_MINIMUM,
    }
}

fn is_stable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

fn is_stable_token(value: &str) -> bool {
    is_stable_id(value)
}

fn is_sha256_identity(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

struct ReportOutput {
    value: String,
    lines: usize,
    limits: BenchmarkReportLimits,
}

impl ReportOutput {
    fn new(limits: BenchmarkReportLimits) -> Self {
        Self {
            value: String::new(),
            lines: 0,
            limits,
        }
    }

    fn push_str(&mut self, value: &str) -> Result<(), BenchmarkReportError> {
        let next_len = self
            .value
            .len()
            .checked_add(value.len())
            .ok_or_else(|| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
        if next_len > self.limits.max_report_bytes {
            return Err(report_error(BenchmarkReportErrorCode::ReportLimit, None));
        }
        let added_lines = value.bytes().filter(|byte| *byte == b'\n').count();
        let next_lines = self
            .lines
            .checked_add(added_lines)
            .ok_or_else(|| report_error(BenchmarkReportErrorCode::LineLimit, None))?;
        if next_lines > self.limits.max_lines {
            return Err(report_error(BenchmarkReportErrorCode::LineLimit, None));
        }
        self.value
            .try_reserve(value.len())
            .map_err(|_| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
        self.value.push_str(value);
        self.lines = next_lines;
        Ok(())
    }

    fn push_char(&mut self, value: char) -> Result<(), BenchmarkReportError> {
        let mut encoded = [0; 4];
        self.push_str(value.encode_utf8(&mut encoded))
    }

    fn into_bytes(self) -> Vec<u8> {
        self.value.into_bytes()
    }
}

fn write_string_field(
    output: &mut ReportOutput,
    key: &str,
    value: &str,
) -> Result<(), BenchmarkReportError> {
    output.push_str(key)?;
    output.push_str(" = ")?;
    write_quoted(output, value)?;
    output.push_str("\n")
}

fn write_string_array_field(
    output: &mut ReportOutput,
    key: &str,
    values: &[String],
) -> Result<(), BenchmarkReportError> {
    output.push_str(key)?;
    output.push_str(" = [")?;
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push_str(", ")?;
        }
        write_quoted(output, value)?;
    }
    output.push_str("]\n")
}

fn write_sample_array_field(
    output: &mut ReportOutput,
    key: &str,
    samples: &RawNanosecondSamples,
) -> Result<(), BenchmarkReportError> {
    output.push_str(key)?;
    output.push_str(" = [")?;
    for (index, sample) in samples.values().iter().enumerate() {
        if index != 0 {
            output.push_str(", ")?;
        }
        output.push_str(&sample.get().to_string())?;
    }
    output.push_str("]\n")
}

fn write_u64_field(
    output: &mut ReportOutput,
    key: &str,
    value: u64,
) -> Result<(), BenchmarkReportError> {
    output.push_str(key)?;
    output.push_str(" = ")?;
    output.push_str(&value.to_string())?;
    output.push_str("\n")
}

fn write_bool_field(
    output: &mut ReportOutput,
    key: &str,
    value: bool,
) -> Result<(), BenchmarkReportError> {
    output.push_str(key)?;
    output.push_str(" = ")?;
    output.push_str(if value { "true" } else { "false" })?;
    output.push_str("\n")
}

fn write_quoted(output: &mut ReportOutput, value: &str) -> Result<(), BenchmarkReportError> {
    output.push_str("\"")?;
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\"")?,
            '\\' => output.push_str("\\\\")?,
            '\n' => output.push_str("\\n")?,
            '\r' => output.push_str("\\r")?,
            '\t' => output.push_str("\\t")?,
            value if value.is_control() => {
                output.push_str(&format!("\\u{:04x}", u32::from(value)))?;
            }
            value => output.push_char(value)?,
        }
    }
    output.push_str("\"")
}

fn read_report_bytes(
    path: &Path,
    limits: BenchmarkReportLimits,
) -> Result<Vec<u8>, BenchmarkReportError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportUnavailable, None))?;
    if !metadata.file_type().is_file() {
        return Err(report_error(
            BenchmarkReportErrorCode::ReportUnavailable,
            None,
        ));
    }
    let maximum = u64::try_from(limits.max_report_bytes)
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
    if metadata.len() > maximum {
        return Err(report_error(BenchmarkReportErrorCode::ReportLimit, None));
    }
    let initial_capacity = usize::try_from(metadata.len())
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportLimit, None))?
        .checked_add(1)
        .ok_or_else(|| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
    let read_limit = limits
        .max_report_bytes
        .checked_add(1)
        .ok_or_else(|| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
    let mut input = Vec::new();
    input
        .try_reserve_exact(initial_capacity)
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportLimit, None))?;
    let file = File::open(path)
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportUnavailable, None))?;
    let mut reader: Take<File> = file.take(
        u64::try_from(read_limit)
            .map_err(|_| report_error(BenchmarkReportErrorCode::ReportLimit, None))?,
    );
    reader
        .read_to_end(&mut input)
        .map_err(|_| report_error(BenchmarkReportErrorCode::ReportUnavailable, None))?;
    if input.len() > limits.max_report_bytes {
        return Err(report_error(BenchmarkReportErrorCode::ReportLimit, None));
    }
    Ok(input)
}

fn usize_to_u64(value: usize) -> Result<u64, BenchmarkReportError> {
    u64::try_from(value).map_err(|_| report_error(BenchmarkReportErrorCode::StatisticsFailed, None))
}

fn line_for_offset(input: &[u8], offset: usize) -> usize {
    input[..offset.min(input.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn report_error(code: BenchmarkReportErrorCode, line: Option<usize>) -> BenchmarkReportError {
    use BenchmarkReportErrorCategory::{
        Availability, Configuration, Integrity, Internal, ResourceLimit, Structure, Syntax,
        Unsupported,
    };
    use BenchmarkReportRecoverability::{
        CorrectConfiguration, CorrectCorpus, CorrectReport, DoNotRetry, ReduceInput, RestoreReport,
        SelectSupportedSchema,
    };

    let (diagnostic_id, detail, category, recoverability) = match code {
        BenchmarkReportErrorCode::InvalidLimits => (
            "RPE-BENCHMARK-REPORT-0001",
            "benchmark report limits are invalid",
            Configuration,
            CorrectConfiguration,
        ),
        BenchmarkReportErrorCode::ReportLimit => (
            "RPE-BENCHMARK-REPORT-0002",
            "benchmark report bytes exceed their limit",
            ResourceLimit,
            ReduceInput,
        ),
        BenchmarkReportErrorCode::LineLimit => (
            "RPE-BENCHMARK-REPORT-0003",
            "benchmark report lines exceed their limit",
            ResourceLimit,
            ReduceInput,
        ),
        BenchmarkReportErrorCode::FeatureFlagLimit => (
            "RPE-BENCHMARK-REPORT-0004",
            "benchmark feature flags exceed their limit",
            ResourceLimit,
            ReduceInput,
        ),
        BenchmarkReportErrorCode::SampleLimit => (
            "RPE-BENCHMARK-REPORT-0005",
            "benchmark samples or their declared minimum exceed the applicable limit",
            ResourceLimit,
            ReduceInput,
        ),
        BenchmarkReportErrorCode::StringLimit => (
            "RPE-BENCHMARK-REPORT-0006",
            "benchmark report string exceeds its limit",
            ResourceLimit,
            ReduceInput,
        ),
        BenchmarkReportErrorCode::InvalidUtf8 => (
            "RPE-BENCHMARK-REPORT-0007",
            "benchmark report is not valid UTF-8",
            Syntax,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::InvalidSyntax => (
            "RPE-BENCHMARK-REPORT-0008",
            "benchmark report syntax is invalid",
            Syntax,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::UnsupportedSchema => (
            "RPE-BENCHMARK-REPORT-0009",
            "benchmark report schema is unsupported",
            Unsupported,
            SelectSupportedSchema,
        ),
        BenchmarkReportErrorCode::UnknownField => (
            "RPE-BENCHMARK-REPORT-0010",
            "benchmark report field is unknown",
            Structure,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::DuplicateField => (
            "RPE-BENCHMARK-REPORT-0011",
            "benchmark report field is duplicated",
            Structure,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::MissingField => (
            "RPE-BENCHMARK-REPORT-0012",
            "mandatory benchmark report field is missing",
            Structure,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::InvalidValue => (
            "RPE-BENCHMARK-REPORT-0013",
            "benchmark report field value is invalid",
            Structure,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::StatisticsMismatch => (
            "RPE-BENCHMARK-REPORT-0014",
            "stored benchmark statistics do not match raw samples",
            Integrity,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::NonCanonical => (
            "RPE-BENCHMARK-REPORT-0015",
            "benchmark report bytes are not canonical",
            Structure,
            CorrectReport,
        ),
        BenchmarkReportErrorCode::ReportUnavailable => (
            "RPE-BENCHMARK-REPORT-0016",
            "benchmark report file is unavailable",
            Availability,
            RestoreReport,
        ),
        BenchmarkReportErrorCode::CorpusIdMismatch => (
            "RPE-BENCHMARK-REPORT-0017",
            "benchmark report corpus identity does not match",
            Integrity,
            CorrectCorpus,
        ),
        BenchmarkReportErrorCode::CorpusHashMismatch => (
            "RPE-BENCHMARK-REPORT-0018",
            "benchmark report corpus hash does not match",
            Integrity,
            CorrectCorpus,
        ),
        BenchmarkReportErrorCode::StatisticsFailed => (
            "RPE-BENCHMARK-REPORT-0019",
            "bounded benchmark statistics failed",
            Internal,
            DoNotRetry,
        ),
        BenchmarkReportErrorCode::HashFailed => (
            "RPE-BENCHMARK-REPORT-0020",
            "bounded benchmark report hashing failed",
            Internal,
            DoNotRetry,
        ),
    };
    BenchmarkReportError {
        code,
        category,
        recoverability,
        diagnostic_id,
        line,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use pdf_rs_corpus::{CorpusManifestLimits, decode_manifest};

    use super::*;

    const CORPUS_HASH: &str = "4268cb945b6056d7732f22b0e90d9629f6d31ab2ba6f013e7011735989859d8e";
    const OBJECT_HASH: &str = "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn canonical_report_round_trips_and_preserves_raw_order() {
        let source = canonical_report();
        let report = decode_report(source.as_bytes(), BenchmarkReportLimits::default()).unwrap();

        assert_eq!(report.id(), "m0-synthetic-benchmark-replay-v1");
        assert_eq!(
            report.evidence_class(),
            BenchmarkEvidenceClass::SyntheticPipelineSmoke
        );
        assert_eq!(report.metadata().profile(), SYNTHETIC_BENCHMARK_PROFILE);
        assert_eq!(report.metadata().corpus_id(), "t0-bootstrap-v1");
        assert_eq!(report.scenario(), BenchmarkScenario::ColdOpen);
        assert_eq!(report.timing_domain(), TimingDomain::Engine);
        assert_eq!(report.warmup_iterations(), 0);
        assert_eq!(report.minimum_sample_count().get(), 5);
        assert_eq!(
            report
                .raw_samples()
                .values()
                .iter()
                .map(|sample| sample.get())
                .collect::<Vec<_>>(),
            vec![100, 80, 120, 90, 110]
        );
        assert_eq!(report.summary().statistics.minimum.get(), 80);
        assert_eq!(report.summary().statistics.median.get(), 100);
        assert_eq!(report.summary().statistics.p95.get(), 120);
        assert_eq!(report.summary().statistics.p99.get(), 120);
        assert_eq!(report.summary().statistics.maximum.get(), 120);
        assert_eq!(report.summary().statistics.sample_count, 5);
        assert_eq!(report.sample_count_status(), SAMPLE_COUNT_MEETS_MINIMUM);
        assert!(!report.performance_eligible());
        assert_eq!(report.verdict(), NOT_EVALUATED);
        assert_eq!(report.source_sha256(), sha256(source.as_bytes()).unwrap());
        assert_eq!(
            encode_report(&report, BenchmarkReportLimits::default()).unwrap(),
            source.as_bytes()
        );
    }

    #[test]
    fn rejects_schema_structure_and_noncanonical_bytes() {
        let source = canonical_report();
        assert_code(
            source.replacen("schema = 1", "schema = 2", 1).as_bytes(),
            BenchmarkReportErrorCode::UnsupportedSchema,
        );
        assert_code(
            source
                .replacen("schema = 1", "schema = 2\nfuture_field = \"value\"", 1)
                .as_bytes(),
            BenchmarkReportErrorCode::UnsupportedSchema,
        );
        assert_code(
            b"schema = 2\n\n[[future-table]]\nvalue = \"future\"\n",
            BenchmarkReportErrorCode::UnsupportedSchema,
        );
        assert_code(
            source.replacen("schema = 1", "schema=1", 1).as_bytes(),
            BenchmarkReportErrorCode::NonCanonical,
        );
        assert_code(
            source
                .replacen("id = ", "unknown = \"x\"\nid = ", 1)
                .as_bytes(),
            BenchmarkReportErrorCode::UnknownField,
        );
        assert_code(
            source
                .replacen(
                    "evidence_class = ",
                    "id = \"duplicate\"\nevidence_class = ",
                    1,
                )
                .as_bytes(),
            BenchmarkReportErrorCode::DuplicateField,
        );
        assert_code(
            source
                .replace("browser = \"not-applicable\"\n", "")
                .as_bytes(),
            BenchmarkReportErrorCode::MissingField,
        );
        assert_code(&[0xff], BenchmarkReportErrorCode::InvalidUtf8);
    }

    #[test]
    fn rejects_summary_tampering_and_performance_claims() {
        let source = canonical_report();
        for (from, to) in [
            ("minimum_ns = 80", "minimum_ns = 81"),
            (
                "raw_samples_ns = [100, 80, 120, 90, 110]",
                "raw_samples_ns = [101, 80, 120, 90, 110]",
            ),
            ("median_ns = 100", "median_ns = 101"),
            ("p95_ns = 120", "p95_ns = 119"),
            ("p99_ns = 120", "p99_ns = 119"),
            ("maximum_ns = 120", "maximum_ns = 121"),
            ("\nsample_count = 5\n", "\nsample_count = 4\n"),
            (
                "sample_count_status = \"meets-configured-minimum\"",
                "sample_count_status = \"insufficient\"",
            ),
        ] {
            assert_code(
                source.replacen(from, to, 1).as_bytes(),
                BenchmarkReportErrorCode::StatisticsMismatch,
            );
        }

        for (from, to) in [
            (
                "evidence_class = \"synthetic-pipeline-smoke\"",
                "evidence_class = \"measured\"",
            ),
            (
                "profile = \"m0.synthetic-benchmark-replay.v1\"",
                "profile = \"release-performance-gate\"",
            ),
            (
                "performance_eligible = false",
                "performance_eligible = true",
            ),
            ("verdict = \"not-evaluated\"", "verdict = \"pass\""),
            (
                "confidence_interval = \"not-implemented-m0\"",
                "confidence_interval = \"available\"",
            ),
            (
                "external_baseline = \"absent\"",
                "external_baseline = \"pdfium\"",
            ),
        ] {
            let error = decode_report(
                source.replacen(from, to, 1).as_bytes(),
                BenchmarkReportLimits::default(),
            )
            .err()
            .unwrap();
            assert_eq!(error.code, BenchmarkReportErrorCode::InvalidValue);
            assert!(!error.to_string().contains(to));
            assert!(!format!("{error:?}").contains(to));
        }
    }

    #[test]
    fn rejects_invalid_identity_taxonomy_and_feature_order() {
        let source = canonical_report();
        for (from, to) in [
            (
                "id = \"m0-synthetic-benchmark-replay-v1\"",
                "id = \"NOT STABLE\"",
            ),
            (
                "corpus_hash = \"sha256:4268cb945b6056d7732f22b0e90d9629f6d31ab2ba6f013e7011735989859d8e\"",
                "corpus_hash = \"sha256:4268CB945B6056D7732F22B0E90D9629F6D31AB2BA6F013E7011735989859D8E\"",
            ),
            (
                "corpus_id = \"t0-bootstrap-v1\"",
                "corpus_id = \"NOT STABLE\"",
            ),
            ("cache_state = \"cold\"", "cache_state = \"warm\""),
            ("scenario = \"cold-open\"", "scenario = \"unknown\""),
            ("timing_domain = \"engine\"", "timing_domain = \"host\""),
            (
                "feature_flags = [\"generator\", \"table-xref\"]",
                "feature_flags = [\"table-xref\", \"generator\"]",
            ),
        ] {
            assert_code(
                source.replacen(from, to, 1).as_bytes(),
                BenchmarkReportErrorCode::InvalidValue,
            );
        }
    }

    #[test]
    fn insufficient_sample_count_remains_an_explicit_non_verdict() {
        let source = canonical_report()
            .replacen("minimum_sample_count = 5", "minimum_sample_count = 6", 1)
            .replacen(
                "sample_count_status = \"meets-configured-minimum\"",
                "sample_count_status = \"insufficient\"",
                1,
            );
        let report = decode_report(source.as_bytes(), BenchmarkReportLimits::default()).unwrap();
        assert_eq!(report.sample_count_status(), SAMPLE_COUNT_INSUFFICIENT);
        assert!(!report.performance_eligible());
        assert_eq!(report.verdict(), NOT_EVALUATED);
        let tight = limits(source.len(), source.lines().count(), 2, 5, 71);
        assert!(decode_report(source.as_bytes(), tight).is_ok());
    }

    #[test]
    fn every_taxonomy_value_round_trips_through_the_report_codec() {
        for &cache_state in CacheState::ALL {
            let scenario = if cache_state == CacheState::Warm {
                BenchmarkScenario::WarmReopen
            } else {
                BenchmarkScenario::ColdOpen
            };
            assert_taxonomy_round_trip(cache_state, scenario, TimingDomain::Engine);
        }
        for &scenario in BenchmarkScenario::ALL {
            let cache_state = if scenario == BenchmarkScenario::WarmReopen {
                CacheState::Warm
            } else {
                CacheState::Cold
            };
            assert_taxonomy_round_trip(cache_state, scenario, TimingDomain::Engine);
        }
        for &timing_domain in TimingDomain::ALL {
            assert_taxonomy_round_trip(
                CacheState::Cold,
                BenchmarkScenario::ColdOpen,
                timing_domain,
            );
        }
    }

    #[test]
    fn metadata_errors_retain_the_originating_line() {
        let source = canonical_report().replacen(
            "toolchain = \"not-measured-synthetic\"",
            "toolchain = \"   \"",
            1,
        );
        let error = decode_report(source.as_bytes(), BenchmarkReportLimits::default())
            .err()
            .unwrap();
        assert_eq!(error.code, BenchmarkReportErrorCode::InvalidValue);
        assert_eq!(error.line, Some(7));
    }

    #[test]
    fn minimum_sample_count_obeys_the_fixed_sample_ceiling() {
        let source = canonical_report().replacen(
            "minimum_sample_count = 5",
            "minimum_sample_count = 1000001",
            1,
        );
        let error = decode_report(source.as_bytes(), BenchmarkReportLimits::default())
            .err()
            .unwrap();
        assert_eq!(error.code, BenchmarkReportErrorCode::SampleLimit);
        assert_eq!(error.line, Some(22));
    }

    #[test]
    fn corpus_binding_checks_id_and_exact_manifest_hash() {
        let report = decode_report(
            canonical_report().as_bytes(),
            BenchmarkReportLimits::default(),
        )
        .unwrap();
        let corpus = decode_manifest(
            corpus_manifest().as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        validate_report_corpus(&report, &corpus).unwrap();

        let wrong_id = decode_report(
            canonical_report()
                .replacen(
                    "corpus_id = \"t0-bootstrap-v1\"",
                    "corpus_id = \"another-corpus\"",
                    1,
                )
                .as_bytes(),
            BenchmarkReportLimits::default(),
        )
        .unwrap();
        assert_eq!(
            validate_report_corpus(&wrong_id, &corpus).unwrap_err().code,
            BenchmarkReportErrorCode::CorpusIdMismatch
        );

        let wrong_hash = decode_report(
            canonical_report()
                .replacen(CORPUS_HASH, &"0".repeat(64), 1)
                .as_bytes(),
            BenchmarkReportLimits::default(),
        )
        .unwrap();
        assert_eq!(
            validate_report_corpus(&wrong_hash, &corpus)
                .unwrap_err()
                .code,
            BenchmarkReportErrorCode::CorpusHashMismatch
        );
    }

    #[test]
    fn every_canonical_truncation_is_rejected() {
        let source = canonical_report();
        for end in 0..source.len() {
            let error = decode_report(&source.as_bytes()[..end], BenchmarkReportLimits::default())
                .err()
                .unwrap();
            assert!(!error.to_string().contains("not-measured-synthetic"));
        }
    }

    #[test]
    fn deterministic_limits_accept_exact_boundaries() {
        let source = canonical_report();
        let line_count = source.lines().count();
        let exact = limits(source.len(), line_count, 2, 5, 71);
        let report = decode_report(source.as_bytes(), exact).unwrap();
        assert_eq!(encode_report(&report, exact).unwrap(), source.as_bytes());

        assert_code_with_limits(
            source.as_bytes(),
            limits(source.len() - 1, line_count, 2, 5, 71),
            BenchmarkReportErrorCode::ReportLimit,
        );
        assert_code_with_limits(
            source.as_bytes(),
            limits(source.len(), line_count - 1, 2, 5, 71),
            BenchmarkReportErrorCode::LineLimit,
        );
        assert_code_with_limits(
            source.as_bytes(),
            limits(source.len(), line_count, 1, 5, 71),
            BenchmarkReportErrorCode::FeatureFlagLimit,
        );
        assert_code_with_limits(
            source.as_bytes(),
            limits(source.len(), line_count, 2, 4, 71),
            BenchmarkReportErrorCode::SampleLimit,
        );
        assert_code_with_limits(
            source.as_bytes(),
            limits(source.len(), line_count, 2, 5, 70),
            BenchmarkReportErrorCode::StringLimit,
        );
        assert_eq!(
            encode_report(&report, limits(source.len() - 1, line_count, 2, 5, 71))
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::ReportLimit
        );
        assert_eq!(
            encode_report(&report, limits(source.len(), line_count - 1, 2, 5, 71))
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::LineLimit
        );
        assert_eq!(
            encode_report(&report, limits(source.len(), line_count, 1, 5, 71))
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::FeatureFlagLimit
        );
        assert_eq!(
            encode_report(&report, limits(source.len(), line_count, 2, 4, 71))
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::SampleLimit
        );
        assert_eq!(
            encode_report(&report, limits(source.len(), line_count, 2, 5, 70))
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::StringLimit
        );
    }

    #[test]
    fn invalid_limit_configurations_are_stable() {
        for result in [
            BenchmarkReportLimits::new(0, 1, 1, 1, 1),
            BenchmarkReportLimits::new(1, 0, 1, 1, 1),
            BenchmarkReportLimits::new(1, 1, 0, 1, 1),
            BenchmarkReportLimits::new(1, 1, 1, 0, 1),
            BenchmarkReportLimits::new(1, 1, 1, 1, 0),
            BenchmarkReportLimits::new(HARD_MAX_REPORT_BYTES + 1, 1, 1, 1, 1),
            BenchmarkReportLimits::new(1, HARD_MAX_LINES + 1, 1, 1, 1),
            BenchmarkReportLimits::new(1, 1, HARD_MAX_FEATURE_FLAGS + 1, 1, 1),
            BenchmarkReportLimits::new(1, 1, 1, HARD_MAX_SAMPLES + 1, 1),
            BenchmarkReportLimits::new(1, 1, 1, 1, HARD_MAX_STRING_BYTES + 1),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code, BenchmarkReportErrorCode::InvalidLimits);
            assert_eq!(error.category, BenchmarkReportErrorCategory::Configuration);
            assert_eq!(
                error.recoverability,
                BenchmarkReportRecoverability::CorrectConfiguration
            );
            assert_eq!(error.diagnostic_id, "RPE-BENCHMARK-REPORT-0001");
        }
    }

    #[test]
    fn bounded_file_loading_rejects_non_files_and_symlinks() {
        let directory = TempDir::new();
        let source = canonical_report();
        let report_path = directory.path().join("report.toml");
        fs::write(&report_path, &source).unwrap();
        assert!(load_report_file(&report_path, BenchmarkReportLimits::default()).is_ok());
        assert_eq!(
            load_report_file(
                &report_path,
                limits(source.len() - 1, source.lines().count(), 2, 5, 71)
            )
            .err()
            .unwrap()
            .code,
            BenchmarkReportErrorCode::ReportLimit
        );
        assert_eq!(
            load_report_file(directory.path(), BenchmarkReportLimits::default())
                .err()
                .unwrap()
                .code,
            BenchmarkReportErrorCode::ReportUnavailable
        );

        #[cfg(unix)]
        {
            let link = directory.path().join("report-link.toml");
            std::os::unix::fs::symlink(&report_path, &link).unwrap();
            assert_eq!(
                load_report_file(&link, BenchmarkReportLimits::default())
                    .err()
                    .unwrap()
                    .code,
                BenchmarkReportErrorCode::ReportUnavailable
            );
        }
    }

    #[test]
    fn diagnostic_category_and_recovery_contract_is_stable() {
        use BenchmarkReportErrorCategory::{
            Availability, Configuration, Integrity, Internal, ResourceLimit, Structure, Syntax,
            Unsupported,
        };
        use BenchmarkReportErrorCode::{
            CorpusHashMismatch, CorpusIdMismatch, DuplicateField, FeatureFlagLimit, HashFailed,
            InvalidLimits, InvalidSyntax, InvalidUtf8, InvalidValue, LineLimit, MissingField,
            NonCanonical, ReportLimit, ReportUnavailable, SampleLimit, StatisticsFailed,
            StatisticsMismatch, StringLimit, UnknownField, UnsupportedSchema,
        };
        use BenchmarkReportRecoverability::{
            CorrectConfiguration, CorrectCorpus, CorrectReport, DoNotRetry, ReduceInput,
            RestoreReport, SelectSupportedSchema,
        };

        let cases = [
            (InvalidLimits, "0001", Configuration, CorrectConfiguration),
            (ReportLimit, "0002", ResourceLimit, ReduceInput),
            (LineLimit, "0003", ResourceLimit, ReduceInput),
            (FeatureFlagLimit, "0004", ResourceLimit, ReduceInput),
            (SampleLimit, "0005", ResourceLimit, ReduceInput),
            (StringLimit, "0006", ResourceLimit, ReduceInput),
            (InvalidUtf8, "0007", Syntax, CorrectReport),
            (InvalidSyntax, "0008", Syntax, CorrectReport),
            (
                UnsupportedSchema,
                "0009",
                Unsupported,
                SelectSupportedSchema,
            ),
            (UnknownField, "0010", Structure, CorrectReport),
            (DuplicateField, "0011", Structure, CorrectReport),
            (MissingField, "0012", Structure, CorrectReport),
            (InvalidValue, "0013", Structure, CorrectReport),
            (StatisticsMismatch, "0014", Integrity, CorrectReport),
            (NonCanonical, "0015", Structure, CorrectReport),
            (ReportUnavailable, "0016", Availability, RestoreReport),
            (CorpusIdMismatch, "0017", Integrity, CorrectCorpus),
            (CorpusHashMismatch, "0018", Integrity, CorrectCorpus),
            (StatisticsFailed, "0019", Internal, DoNotRetry),
            (HashFailed, "0020", Internal, DoNotRetry),
        ];
        for (code, suffix, category, recoverability) in cases {
            let error = report_error(code, Some(7));
            assert_eq!(error.category, category);
            assert_eq!(error.recoverability, recoverability);
            assert_eq!(
                error.diagnostic_id,
                format!("RPE-BENCHMARK-REPORT-{suffix}")
            );
            assert_eq!(error.line, Some(7));
        }
    }

    fn canonical_report() -> String {
        format!(
            "schema = 1\nid = \"m0-synthetic-benchmark-replay-v1\"\nevidence_class = \"synthetic-pipeline-smoke\"\ncommit = \"not-applicable-synthetic-fixture\"\nprofile = \"m0.synthetic-benchmark-replay.v1\"\nfeature_flags = [\"generator\", \"table-xref\"]\ntoolchain = \"not-measured-synthetic\"\nos = \"not-measured-synthetic\"\ncpu = \"not-measured-synthetic\"\ngpu = \"not-applicable\"\nmemory = \"not-measured-synthetic\"\nbrowser = \"not-applicable\"\ncorpus_id = \"t0-bootstrap-v1\"\ncorpus_hash = \"sha256:{CORPUS_HASH}\"\nrenderer_epoch = \"synthetic-v1\"\nfont_epoch = \"not-applicable\"\ncolor_epoch = \"srgb-reference-v1\"\ncache_state = \"cold\"\nscenario = \"cold-open\"\ntiming_domain = \"engine\"\nwarmup_iterations = 0\nminimum_sample_count = 5\nraw_samples_ns = [100, 80, 120, 90, 110]\nminimum_ns = 80\nmedian_ns = 100\np95_ns = 120\np99_ns = 120\nmaximum_ns = 120\nsample_count = 5\nsample_count_status = \"meets-configured-minimum\"\nperformance_eligible = false\nconfidence_interval = \"not-implemented-m0\"\nexternal_baseline = \"absent\"\nverdict = \"not-evaluated\"\n"
        )
    }

    fn assert_taxonomy_round_trip(
        cache_state: CacheState,
        scenario: BenchmarkScenario,
        timing_domain: TimingDomain,
    ) {
        let source = canonical_report()
            .replacen(
                "cache_state = \"cold\"",
                &format!("cache_state = \"{}\"", cache_state.as_str()),
                1,
            )
            .replacen(
                "scenario = \"cold-open\"",
                &format!("scenario = \"{}\"", scenario.as_str()),
                1,
            )
            .replacen(
                "timing_domain = \"engine\"",
                &format!("timing_domain = \"{}\"", timing_domain.as_str()),
                1,
            );
        let report = decode_report(source.as_bytes(), BenchmarkReportLimits::default()).unwrap();
        assert_eq!(report.metadata().cache_state(), cache_state);
        assert_eq!(report.scenario(), scenario);
        assert_eq!(report.timing_domain(), timing_domain);
        assert_eq!(
            encode_report(&report, BenchmarkReportLimits::default()).unwrap(),
            source.as_bytes()
        );
    }

    fn corpus_manifest() -> String {
        format!(
            "schema = 1\nid = \"t0-bootstrap-v1\"\nversion = \"1\"\n\n[[entry]]\nsha256 = \"sha256:{OBJECT_HASH}\"\npath = \"tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf\"\ntier = \"T0\"\npage_count = 1\nlicense_expression = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\nsource = \"fixture.infrastructure.synthetic-failure-bundle-001\"\naccess = \"repository\"\nredistribution = \"prohibited\"\nfeatures = [\"syntax.core\", \"xref.table\"]\nmax_bytes = 65536\n"
        )
    }

    fn assert_code(input: &[u8], expected: BenchmarkReportErrorCode) {
        assert_code_with_limits(input, BenchmarkReportLimits::default(), expected);
    }

    fn assert_code_with_limits(
        input: &[u8],
        limits: BenchmarkReportLimits,
        expected: BenchmarkReportErrorCode,
    ) {
        assert_eq!(decode_report(input, limits).err().unwrap().code, expected);
    }

    fn limits(
        report_bytes: usize,
        lines: usize,
        feature_flags: usize,
        samples: usize,
        string_bytes: usize,
    ) -> BenchmarkReportLimits {
        BenchmarkReportLimits::new(report_bytes, lines, feature_flags, samples, string_bytes)
            .unwrap()
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pdf-rs-benchmark-report-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
