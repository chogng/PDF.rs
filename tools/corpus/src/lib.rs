#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Deterministic corpus metadata, selection, and release holdout partitioning.

mod manifest;

pub use manifest::{
    CORPUS_MANIFEST_SCHEMA, CorpusManifestError, CorpusManifestErrorCategory,
    CorpusManifestErrorCode, CorpusManifestLimits, CorpusManifestObject,
    CorpusManifestRecoverability, CorpusManifestVerification, OnDiskCorpusManifest,
    decode_manifest, encode_manifest, load_manifest_file, validate_manifest_file,
    verify_manifest_objects,
};

use std::collections::BTreeSet;
use std::fmt;

/// Content identity of one immutable corpus PDF.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CorpusObjectId([u8; 32]);

impl CorpusObjectId {
    /// Wraps an already verified SHA-256 content digest.
    pub const fn from_sha256(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// Returns the immutable content digest.
    pub const fn sha256(self) -> [u8; 32] {
        self.0
    }
}

/// Execution tier defined by RPE-STD-003.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CorpusTier {
    /// Per-commit atomic and critical regression corpus.
    T0,
    /// Pull-request high-value module corpus.
    T1,
    /// Daily real-world sample corpus.
    T2,
    /// Large offline and long-running corpus.
    T3,
}

/// Who may access the underlying document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessPolicy {
    /// Object is approved for public access.
    Public,
    /// Object is available only inside the source repository boundary.
    Repository,
    /// Object requires an explicitly authorized shared store.
    Restricted,
    /// Object is private to its approved operator and storage boundary.
    Private,
}

/// Whether bytes may leave their authorized storage boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedistributionPolicy {
    /// License and privacy review permits copying the bytes.
    Allowed,
    /// Bytes must remain in their authorized storage boundary.
    Prohibited,
}

/// Source and SPDX/LicenseRef expression for one corpus object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LicenseRecord {
    expression: String,
    source: String,
}

impl LicenseRecord {
    /// Creates non-empty license metadata.
    pub fn new(
        expression: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<Self, CorpusError> {
        let expression = expression.into();
        let source = source.into();
        if expression.trim().is_empty() {
            return Err(CorpusError::new(CorpusErrorCode::MissingLicense));
        }
        if source.trim().is_empty() {
            return Err(CorpusError::new(CorpusErrorCode::MissingSource));
        }
        Ok(Self { expression, source })
    }

    /// Returns the caller-validated SPDX expression or project `LicenseRef`.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Returns the non-empty acquisition or generation source.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Validated immutable entry in a corpus manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusEntry {
    id: CorpusObjectId,
    tier: CorpusTier,
    page_count: u32,
    license: LicenseRecord,
    access: AccessPolicy,
    redistribution: RedistributionPolicy,
    features: Vec<String>,
}

impl CorpusEntry {
    /// Validates page count, privacy/redistribution, and structured feature IDs.
    pub fn new(
        id: CorpusObjectId,
        tier: CorpusTier,
        page_count: u32,
        license: LicenseRecord,
        access: AccessPolicy,
        redistribution: RedistributionPolicy,
        mut features: Vec<String>,
    ) -> Result<Self, CorpusError> {
        if page_count == 0 {
            return Err(CorpusError::new(CorpusErrorCode::ZeroPages));
        }
        if access == AccessPolicy::Private && redistribution == RedistributionPolicy::Allowed {
            return Err(CorpusError::new(CorpusErrorCode::PrivateRedistribution));
        }
        if features.iter().any(|feature| feature.trim().is_empty()) {
            return Err(CorpusError::new(CorpusErrorCode::InvalidFeature));
        }
        features.sort();
        features.dedup();
        Ok(Self {
            id,
            tier,
            page_count,
            license,
            access,
            redistribution,
            features,
        })
    }

    /// Returns the immutable content identity.
    pub const fn id(&self) -> CorpusObjectId {
        self.id
    }

    /// Returns the execution tier.
    pub const fn tier(&self) -> CorpusTier {
        self.tier
    }

    /// Returns the non-zero declared page count.
    pub const fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Returns who may access the underlying bytes.
    pub const fn access(&self) -> AccessPolicy {
        self.access
    }

    /// Returns whether the bytes may leave their authorized store.
    pub const fn redistribution(&self) -> RedistributionPolicy {
        self.redistribution
    }

    /// Returns the source and license record.
    pub fn license(&self) -> &LicenseRecord {
        &self.license
    }

    /// Returns sorted, deduplicated feature identifiers.
    pub fn features(&self) -> &[String] {
        &self.features
    }
}

/// Versioned, deterministically ordered corpus manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusManifest {
    id: String,
    version: String,
    entries: Vec<CorpusEntry>,
}

impl CorpusManifest {
    /// Validates stable identity, uniqueness, and aggregate page arithmetic.
    pub fn new(
        id: impl Into<String>,
        version: impl Into<String>,
        mut entries: Vec<CorpusEntry>,
    ) -> Result<Self, CorpusError> {
        let id = id.into();
        let version = version.into();
        if !is_stable_id(&id) || version.trim().is_empty() {
            return Err(CorpusError::new(CorpusErrorCode::InvalidManifestIdentity));
        }
        entries.sort_by_key(CorpusEntry::id);
        let mut identities = BTreeSet::new();
        let mut pages = 0_u64;
        for entry in &entries {
            if !identities.insert(entry.id()) {
                return Err(CorpusError::new(CorpusErrorCode::DuplicateObject));
            }
            pages = pages
                .checked_add(u64::from(entry.page_count()))
                .ok_or_else(|| CorpusError::new(CorpusErrorCode::PageCountOverflow))?;
        }
        let _ = pages;
        Ok(Self {
            id,
            version,
            entries,
        })
    }

    /// Returns the stable manifest identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the caller-supplied immutable manifest revision.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns entries in deterministic content-digest order.
    pub fn entries(&self) -> &[CorpusEntry] {
        &self.entries
    }

    /// Selects requested tiers in content-ID order.
    pub fn select(&self, tiers: &[CorpusTier]) -> Vec<&CorpusEntry> {
        let selected: BTreeSet<_> = tiers.iter().copied().collect();
        self.entries
            .iter()
            .filter(|entry| selected.contains(&entry.tier()))
            .collect()
    }

    /// Returns checked file/page totals.
    pub fn summary(&self) -> Result<CorpusSummary, CorpusError> {
        let files = u64::try_from(self.entries.len())
            .map_err(|_| CorpusError::new(CorpusErrorCode::FileCountOverflow))?;
        let pages = self.entries.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(u64::from(entry.page_count()))
                .ok_or_else(|| CorpusError::new(CorpusErrorCode::PageCountOverflow))
        })?;
        Ok(CorpusSummary { files, pages })
    }

    /// Splits release tuning and holdout objects without depending on input order.
    pub fn release_partition(&self, rate: HoldoutRate) -> ReleasePartition<'_> {
        let (holdout, tuning): (Vec<_>, Vec<_>) = self
            .entries
            .iter()
            .partition(|entry| rate.is_holdout(entry.id()));
        ReleasePartition { tuning, holdout }
    }
}

/// Checked aggregate corpus counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusSummary {
    /// Number of immutable document objects.
    pub files: u64,
    /// Checked sum of declared document pages.
    pub pages: u64,
}

/// A ratio in basis points, inclusive of 0 and 10,000.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HoldoutRate(u16);

impl HoldoutRate {
    /// Creates a release holdout rate from 0 through 10,000 basis points.
    pub fn from_basis_points(basis_points: u16) -> Result<Self, CorpusError> {
        if basis_points > 10_000 {
            return Err(CorpusError::new(CorpusErrorCode::InvalidHoldoutRate));
        }
        Ok(Self(basis_points))
    }

    /// Returns the configured rate in basis points.
    pub const fn basis_points(self) -> u16 {
        self.0
    }

    /// Deterministically assigns one content identity to the holdout partition.
    pub fn is_holdout(self, id: CorpusObjectId) -> bool {
        let digest = id.sha256();
        let prefix = u64::from_be_bytes([
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
        ]);
        let bucket = (u128::from(prefix) * 10_000) / (u128::from(u64::MAX) + 1);
        bucket < u128::from(self.0)
    }
}

/// Stable release tuning/holdout views.
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasePartition<'a> {
    /// Objects visible to implementation tuning and ordinary regression work.
    pub tuning: Vec<&'a CorpusEntry>,
    /// Objects withheld for release evaluation.
    pub holdout: Vec<&'a CorpusEntry>,
}

/// Stable corpus-governance failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorpusErrorCode {
    /// License expression is empty.
    MissingLicense,
    /// Acquisition or generation source is empty.
    MissingSource,
    /// Page count is zero.
    ZeroPages,
    /// Private bytes were marked redistributable.
    PrivateRedistribution,
    /// A structured feature identifier is empty.
    InvalidFeature,
    /// Manifest identifier or version is invalid.
    InvalidManifestIdentity,
    /// Two entries have the same immutable content identity.
    DuplicateObject,
    /// File count cannot be represented by the evidence schema.
    FileCountOverflow,
    /// Aggregate page count overflowed.
    PageCountOverflow,
    /// Holdout rate exceeds 10,000 basis points.
    InvalidHoldoutRate,
}

/// Stable, content-free corpus metadata validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusError {
    /// Machine-classifiable failure category.
    pub code: CorpusErrorCode,
    /// Stable project diagnostic identifier.
    pub diagnostic_id: &'static str,
}

impl CorpusError {
    fn new(code: CorpusErrorCode) -> Self {
        let diagnostic_id = match code {
            CorpusErrorCode::MissingLicense => "RPE-CORPUS-0001",
            CorpusErrorCode::MissingSource => "RPE-CORPUS-0002",
            CorpusErrorCode::ZeroPages => "RPE-CORPUS-0003",
            CorpusErrorCode::PrivateRedistribution => "RPE-CORPUS-0004",
            CorpusErrorCode::InvalidFeature => "RPE-CORPUS-0005",
            CorpusErrorCode::InvalidManifestIdentity => "RPE-CORPUS-0006",
            CorpusErrorCode::DuplicateObject => "RPE-CORPUS-0007",
            CorpusErrorCode::FileCountOverflow => "RPE-CORPUS-0008",
            CorpusErrorCode::PageCountOverflow => "RPE-CORPUS-0009",
            CorpusErrorCode::InvalidHoldoutRate => "RPE-CORPUS-0010",
        };
        Self {
            code,
            diagnostic_id,
        }
    }
}

impl fmt::Display for CorpusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl std::error::Error for CorpusError {}

fn is_stable_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-._/@".contains(&byte)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn license() -> LicenseRecord {
        LicenseRecord::new("LicenseRef-self-authored", "project generator").unwrap()
    }

    fn entry(first_byte: u8, tier: CorpusTier, pages: u32) -> CorpusEntry {
        let mut digest = [0; 32];
        digest[0] = first_byte;
        CorpusEntry::new(
            CorpusObjectId::from_sha256(digest),
            tier,
            pages,
            license(),
            AccessPolicy::Repository,
            RedistributionPolicy::Prohibited,
            vec!["syntax.core".into()],
        )
        .unwrap()
    }

    #[test]
    fn validates_license_source_pages_features_and_privacy() {
        assert_eq!(
            LicenseRecord::new("", "source").unwrap_err().code,
            CorpusErrorCode::MissingLicense
        );
        assert_eq!(
            LicenseRecord::new("MIT", "").unwrap_err().code,
            CorpusErrorCode::MissingSource
        );
        assert_eq!(
            CorpusEntry::new(
                CorpusObjectId::from_sha256([0; 32]),
                CorpusTier::T0,
                0,
                license(),
                AccessPolicy::Repository,
                RedistributionPolicy::Prohibited,
                vec![],
            )
            .unwrap_err()
            .code,
            CorpusErrorCode::ZeroPages
        );
        assert_eq!(
            CorpusEntry::new(
                CorpusObjectId::from_sha256([1; 32]),
                CorpusTier::T2,
                1,
                license(),
                AccessPolicy::Private,
                RedistributionPolicy::Allowed,
                vec![],
            )
            .unwrap_err()
            .code,
            CorpusErrorCode::PrivateRedistribution
        );
    }

    #[test]
    fn canonicalizes_entries_features_and_tier_selection() {
        let mut second = entry(2, CorpusTier::T1, 3);
        second.features = vec!["text.core".into(), "syntax.core".into(), "text.core".into()];
        let second = CorpusEntry::new(
            second.id,
            second.tier,
            second.page_count,
            second.license,
            second.access,
            second.redistribution,
            second.features,
        )
        .unwrap();
        let manifest = CorpusManifest::new(
            "t0-bootstrap-v1",
            "1",
            vec![second, entry(1, CorpusTier::T0, 2)],
        )
        .unwrap();
        assert_eq!(manifest.entries()[0].id(), entry(1, CorpusTier::T0, 2).id());
        assert_eq!(
            manifest.entries()[1].features(),
            ["syntax.core", "text.core"]
        );
        assert_eq!(manifest.select(&[CorpusTier::T1]).len(), 1);
        assert_eq!(
            manifest.summary().unwrap(),
            CorpusSummary { files: 2, pages: 5 }
        );
    }

    #[test]
    fn rejects_duplicate_objects_and_invalid_identity() {
        let duplicate = entry(1, CorpusTier::T0, 1);
        assert_eq!(
            CorpusManifest::new("t0", "1", vec![duplicate.clone(), duplicate])
                .unwrap_err()
                .code,
            CorpusErrorCode::DuplicateObject
        );
        assert_eq!(
            CorpusManifest::new("Not Stable", "1", vec![])
                .unwrap_err()
                .code,
            CorpusErrorCode::InvalidManifestIdentity
        );
    }

    #[test]
    fn holdout_is_deterministic_and_honors_boundaries() {
        let none = HoldoutRate::from_basis_points(0).unwrap();
        let all = HoldoutRate::from_basis_points(10_000).unwrap();
        let twenty_percent = HoldoutRate::from_basis_points(2_000).unwrap();
        let low = CorpusObjectId::from_sha256([0; 32]);
        let high = CorpusObjectId::from_sha256([255; 32]);
        assert!(!none.is_holdout(low));
        assert!(all.is_holdout(high));
        assert!(twenty_percent.is_holdout(low));
        assert!(!twenty_percent.is_holdout(high));
        assert_eq!(
            HoldoutRate::from_basis_points(10_001).unwrap_err().code,
            CorpusErrorCode::InvalidHoldoutRate
        );
    }

    #[test]
    fn partition_is_independent_of_input_order() {
        let first = CorpusManifest::new(
            "release-r0-v1",
            "1",
            vec![entry(250, CorpusTier::T2, 1), entry(0, CorpusTier::T2, 1)],
        )
        .unwrap();
        let second = CorpusManifest::new(
            "release-r0-v1",
            "1",
            vec![entry(0, CorpusTier::T2, 1), entry(250, CorpusTier::T2, 1)],
        )
        .unwrap();
        let rate = HoldoutRate::from_basis_points(2_000).unwrap();
        assert_eq!(
            first.release_partition(rate),
            second.release_partition(rate)
        );
    }
}
