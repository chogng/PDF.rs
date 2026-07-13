#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Deterministic benchmark metadata and descriptive timing statistics for PDF.rs.
//!
//! This crate preserves raw integer nanosecond samples and computes descriptive nearest-rank
//! quantiles. It deliberately does not decide CI or release acceptance and does not infer statistical
//! significance from a sample count alone.

mod metadata;
mod report;
mod statistics;

pub use metadata::{
    BenchmarkMetadata, BenchmarkMetadataInput, BenchmarkScenario, BenchmarkSchemaVersion,
    CacheState, MetadataError, MetadataField, TimingDomain,
};
pub use report::{
    BENCHMARK_REPORT_SCHEMA, BenchmarkEvidenceClass, BenchmarkReportError,
    BenchmarkReportErrorCategory, BenchmarkReportErrorCode, BenchmarkReportLimits,
    BenchmarkReportRecoverability, OnDiskBenchmarkReport, SYNTHETIC_BENCHMARK_PROFILE,
    decode_report, encode_report, load_report_file, validate_report_corpus,
};
pub use statistics::{
    BenchmarkSummary, EmptySamples, MinimumSampleCount, Nanoseconds, NativeBaselineRatio,
    RatioError, RawNanosecondSamples, SampleAdequacy, SampleStatistics, StatisticsError,
};
