use std::error::Error;
use std::fmt;

/// Version of the benchmark metadata schema.
///
/// Version zero is reserved for missing or uninitialized data.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct BenchmarkSchemaVersion(u32);

impl BenchmarkSchemaVersion {
    /// Initial benchmark metadata schema.
    pub const V1: Self = Self(1);

    /// Creates a non-zero metadata schema version.
    #[must_use]
    pub const fn new(version: u32) -> Option<Self> {
        if version == 0 {
            None
        } else {
            Some(Self(version))
        }
    }

    /// Returns the numeric schema version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Cache state under which samples were collected.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CacheState {
    /// Process, code, document, font, object, and browser caches are intentionally cold.
    Cold,
    /// The scenario intentionally reuses declared caches from a prior run.
    Warm,
}

impl CacheState {
    /// Returns the stable metadata spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }
}

/// Versioned benchmark scenario taxonomy from RPE-ARCH-001 section 12.22.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BenchmarkScenario {
    /// Cold process, code, document, and cache open.
    ColdOpen,
    /// Reopen with explicitly declared warm caches.
    WarmReopen,
    /// Time until the first recognizable preview is available.
    FirstVisiblePreview,
    /// Time until every target tile in the current viewport reaches full quality.
    FirstFullQualityViewport,
    /// Stable continuous-scroll behavior.
    ContinuousScroll,
    /// Fast wheel or pinch behavior including stale-work handling.
    FastWheelOrPinch,
    /// Jump to a non-adjacent page or range.
    RandomJump,
    /// Large scanned-document path.
    LargeScan,
    /// Vector- or CAD-heavy path.
    VectorCad,
    /// CJK- or text-heavy path.
    CjkTextHeavy,
    /// Malformed-input repair or bounded rejection path.
    Malformed,
    /// Time until the first search result is available.
    SearchFirstResult,
}

impl BenchmarkScenario {
    /// Returns the stable report spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdOpen => "cold-open",
            Self::WarmReopen => "warm-reopen",
            Self::FirstVisiblePreview => "first-visible-preview",
            Self::FirstFullQualityViewport => "first-full-quality-viewport",
            Self::ContinuousScroll => "continuous-scroll",
            Self::FastWheelOrPinch => "fast-wheel-or-pinch",
            Self::RandomJump => "random-jump",
            Self::LargeScan => "large-scan",
            Self::VectorCad => "vector-cad",
            Self::CjkTextHeavy => "cjk-text-heavy",
            Self::Malformed => "malformed",
            Self::SearchFirstResult => "search-first-result",
        }
    }
}

/// Timing boundary used by a benchmark sample set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TimingDomain {
    /// Isolated component time, excluding the surrounding user path.
    Component,
    /// Engine time for a user path, excluding network transfer and host presentation.
    Engine,
    /// End-to-end user-path time including network transfer.
    IncludingNetwork,
}

impl TimingDomain {
    /// Returns the stable report spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Engine => "engine",
            Self::IncludingNetwork => "including-network",
        }
    }
}

/// Required metadata field that failed non-empty validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetadataField {
    /// Source commit identifier.
    Commit,
    /// Build or release profile.
    Profile,
    /// One feature flag entry.
    FeatureFlag,
    /// Compiler/toolchain identifier.
    Toolchain,
    /// Operating-system identifier.
    OperatingSystem,
    /// CPU identifier.
    Cpu,
    /// GPU identifier or explicit not-applicable marker.
    Gpu,
    /// Memory configuration.
    Memory,
    /// Browser identifier or explicit not-applicable marker.
    Browser,
    /// Corpus identity.
    CorpusId,
    /// Corpus content hash.
    CorpusHash,
    /// Renderer epoch.
    RendererEpoch,
    /// Font epoch.
    FontEpoch,
    /// Color epoch.
    ColorEpoch,
}

impl MetadataField {
    /// Returns the stable field name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Profile => "profile",
            Self::FeatureFlag => "feature_flag",
            Self::Toolchain => "toolchain",
            Self::OperatingSystem => "os",
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Memory => "memory",
            Self::Browser => "browser",
            Self::CorpusId => "corpus_id",
            Self::CorpusHash => "corpus_hash",
            Self::RendererEpoch => "renderer_epoch",
            Self::FontEpoch => "font_epoch",
            Self::ColorEpoch => "color_epoch",
        }
    }
}

/// Validation error for incomplete benchmark metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataError {
    /// Required field that was empty or whitespace-only.
    pub field: MetadataField,
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "benchmark metadata field must be non-empty: {}",
            self.field.as_str()
        )
    }
}

impl Error for MetadataError {}

/// Unvalidated input used to construct immutable benchmark metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkMetadataInput {
    /// Metadata schema version.
    pub schema: BenchmarkSchemaVersion,
    /// Exact source commit identifier.
    pub commit: String,
    /// Build or release profile identifier.
    pub profile: String,
    /// Enabled build feature flags, in the producer's canonical order.
    pub feature_flags: Vec<String>,
    /// Exact compiler/toolchain identifier.
    pub toolchain: String,
    /// Operating-system image or version identifier.
    pub os: String,
    /// CPU model and relevant topology identifier.
    pub cpu: String,
    /// GPU/driver identifier or an explicit not-applicable marker.
    pub gpu: String,
    /// Installed/available memory configuration.
    pub memory: String,
    /// Browser engine/image identifier or an explicit not-applicable marker.
    pub browser: String,
    /// Stable corpus identity.
    pub corpus_id: String,
    /// Content hash of the exact corpus manifest.
    pub corpus_hash: String,
    /// Renderer epoch or implementation version.
    pub renderer_epoch: String,
    /// Font environment epoch.
    pub font_epoch: String,
    /// Color environment epoch.
    pub color_epoch: String,
    /// Declared cache state.
    pub cache_state: CacheState,
}

/// Validated, immutable, versioned benchmark metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkMetadata {
    schema: BenchmarkSchemaVersion,
    commit: String,
    profile: String,
    feature_flags: Vec<String>,
    toolchain: String,
    os: String,
    cpu: String,
    gpu: String,
    memory: String,
    browser: String,
    corpus_id: String,
    corpus_hash: String,
    renderer_epoch: String,
    font_epoch: String,
    color_epoch: String,
    cache_state: CacheState,
}

impl BenchmarkMetadata {
    /// Validates all mandatory text fields while preserving their exact values and flag order.
    pub fn new(input: BenchmarkMetadataInput) -> Result<Self, MetadataError> {
        validate_text(&input.commit, MetadataField::Commit)?;
        validate_text(&input.profile, MetadataField::Profile)?;
        for feature_flag in &input.feature_flags {
            validate_text(feature_flag, MetadataField::FeatureFlag)?;
        }
        validate_text(&input.toolchain, MetadataField::Toolchain)?;
        validate_text(&input.os, MetadataField::OperatingSystem)?;
        validate_text(&input.cpu, MetadataField::Cpu)?;
        validate_text(&input.gpu, MetadataField::Gpu)?;
        validate_text(&input.memory, MetadataField::Memory)?;
        validate_text(&input.browser, MetadataField::Browser)?;
        validate_text(&input.corpus_id, MetadataField::CorpusId)?;
        validate_text(&input.corpus_hash, MetadataField::CorpusHash)?;
        validate_text(&input.renderer_epoch, MetadataField::RendererEpoch)?;
        validate_text(&input.font_epoch, MetadataField::FontEpoch)?;
        validate_text(&input.color_epoch, MetadataField::ColorEpoch)?;

        Ok(Self {
            schema: input.schema,
            commit: input.commit,
            profile: input.profile,
            feature_flags: input.feature_flags,
            toolchain: input.toolchain,
            os: input.os,
            cpu: input.cpu,
            gpu: input.gpu,
            memory: input.memory,
            browser: input.browser,
            corpus_id: input.corpus_id,
            corpus_hash: input.corpus_hash,
            renderer_epoch: input.renderer_epoch,
            font_epoch: input.font_epoch,
            color_epoch: input.color_epoch,
            cache_state: input.cache_state,
        })
    }

    /// Returns the metadata schema version.
    #[must_use]
    pub const fn schema(&self) -> BenchmarkSchemaVersion {
        self.schema
    }

    /// Returns the exact source commit identifier.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// Returns the profile identifier.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Returns enabled feature flags in preserved producer order.
    #[must_use]
    pub fn feature_flags(&self) -> &[String] {
        &self.feature_flags
    }

    /// Returns the exact toolchain identifier.
    #[must_use]
    pub fn toolchain(&self) -> &str {
        &self.toolchain
    }

    /// Returns the operating-system identifier.
    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    /// Returns the CPU identifier.
    #[must_use]
    pub fn cpu(&self) -> &str {
        &self.cpu
    }

    /// Returns the GPU/driver identifier.
    #[must_use]
    pub fn gpu(&self) -> &str {
        &self.gpu
    }

    /// Returns the memory configuration.
    #[must_use]
    pub fn memory(&self) -> &str {
        &self.memory
    }

    /// Returns the browser identifier.
    #[must_use]
    pub fn browser(&self) -> &str {
        &self.browser
    }

    /// Returns the stable corpus identity.
    #[must_use]
    pub fn corpus_id(&self) -> &str {
        &self.corpus_id
    }

    /// Returns the exact corpus manifest hash.
    #[must_use]
    pub fn corpus_hash(&self) -> &str {
        &self.corpus_hash
    }

    /// Returns the renderer epoch.
    #[must_use]
    pub fn renderer_epoch(&self) -> &str {
        &self.renderer_epoch
    }

    /// Returns the font epoch.
    #[must_use]
    pub fn font_epoch(&self) -> &str {
        &self.font_epoch
    }

    /// Returns the color epoch.
    #[must_use]
    pub fn color_epoch(&self) -> &str {
        &self.color_epoch
    }

    /// Returns the declared cold/warm cache state.
    #[must_use]
    pub const fn cache_state(&self) -> CacheState {
        self.cache_state
    }
}

fn validate_text(value: &str, field: MetadataField) -> Result<(), MetadataError> {
    if value.trim().is_empty() {
        return Err(MetadataError { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BenchmarkMetadata, BenchmarkMetadataInput, BenchmarkScenario, BenchmarkSchemaVersion,
        CacheState, MetadataError, MetadataField, TimingDomain,
    };

    #[test]
    fn validated_metadata_preserves_every_required_value() {
        let input = complete_input(CacheState::Cold);
        let expected = input.clone();
        let metadata = BenchmarkMetadata::new(input)
            .unwrap_or_else(|error| panic!("complete metadata failed: {error}"));

        assert_eq!(metadata.schema(), expected.schema);
        assert_eq!(metadata.commit(), expected.commit);
        assert_eq!(metadata.profile(), expected.profile);
        assert_eq!(metadata.feature_flags(), expected.feature_flags);
        assert_eq!(metadata.toolchain(), expected.toolchain);
        assert_eq!(metadata.os(), expected.os);
        assert_eq!(metadata.cpu(), expected.cpu);
        assert_eq!(metadata.gpu(), expected.gpu);
        assert_eq!(metadata.memory(), expected.memory);
        assert_eq!(metadata.browser(), expected.browser);
        assert_eq!(metadata.corpus_id(), expected.corpus_id);
        assert_eq!(metadata.corpus_hash(), expected.corpus_hash);
        assert_eq!(metadata.renderer_epoch(), expected.renderer_epoch);
        assert_eq!(metadata.font_epoch(), expected.font_epoch);
        assert_eq!(metadata.color_epoch(), expected.color_epoch);
        assert_eq!(metadata.cache_state(), expected.cache_state);
    }

    #[test]
    fn empty_mandatory_metadata_is_rejected() {
        let mut input = complete_input(CacheState::Warm);
        input.cpu = "  ".to_owned();
        assert_eq!(
            BenchmarkMetadata::new(input),
            Err(MetadataError {
                field: MetadataField::Cpu
            })
        );

        let mut input = complete_input(CacheState::Warm);
        input.feature_flags.push(String::new());
        assert_eq!(
            BenchmarkMetadata::new(input),
            Err(MetadataError {
                field: MetadataField::FeatureFlag
            })
        );
    }

    #[test]
    fn cold_warm_and_engine_network_taxonomies_remain_distinct() {
        assert_ne!(CacheState::Cold, CacheState::Warm);
        assert_eq!(CacheState::Cold.as_str(), "cold");
        assert_eq!(CacheState::Warm.as_str(), "warm");
        assert_ne!(BenchmarkScenario::ColdOpen, BenchmarkScenario::WarmReopen);
        assert_ne!(TimingDomain::Engine, TimingDomain::IncludingNetwork);
        assert_eq!(TimingDomain::Engine.as_str(), "engine");
        assert_eq!(TimingDomain::IncludingNetwork.as_str(), "including-network");
    }

    #[test]
    fn schema_version_zero_is_reserved() {
        assert_eq!(BenchmarkSchemaVersion::new(0), None);
        assert_eq!(
            BenchmarkSchemaVersion::new(1),
            Some(BenchmarkSchemaVersion::V1)
        );
        assert_eq!(BenchmarkSchemaVersion::V1.get(), 1);
    }

    fn complete_input(cache_state: CacheState) -> BenchmarkMetadataInput {
        BenchmarkMetadataInput {
            schema: BenchmarkSchemaVersion::V1,
            commit: "0123456789abcdef".to_owned(),
            profile: "release-lto".to_owned(),
            feature_flags: vec!["fast-cpu".to_owned(), "text".to_owned()],
            toolchain: "rustc 1.93.0".to_owned(),
            os: "linux-x86_64@sha256:os".to_owned(),
            cpu: "example-cpu/8c".to_owned(),
            gpu: "example-gpu/driver-1".to_owned(),
            memory: "32-GiB".to_owned(),
            browser: "chromium@sha256:browser".to_owned(),
            corpus_id: "t1-2026-07".to_owned(),
            corpus_hash: "sha256:corpus".to_owned(),
            renderer_epoch: "fast-cpu-v1".to_owned(),
            font_epoch: "fonts-v1".to_owned(),
            color_epoch: "srgb-reference-v1".to_owned(),
            cache_state,
        }
    }
}
