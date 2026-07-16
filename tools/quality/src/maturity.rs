use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use pdf_rs_digest::{hex_digest, sha256};

const ROOT_KEYS: &[&str] = &["schema", "version", "status"];
const PROFILE_KEYS: &[&str] = &[
    "id",
    "owner",
    "state",
    "feature",
    "requirements",
    "supported",
    "excluded",
    "policy",
    "target",
    "reference",
    "o0_cases",
    "o1_cases",
    "o2_adjudications",
    "independent_review",
    "fuzz_targets",
    "fuzz_minimizer",
    "holdout_manifest",
    "benchmark_report",
    "differential_report",
    "baseline_fingerprint",
];
const FEATURE_KEYS: &[&str] = &[
    "id",
    "owner",
    "state",
    "profile",
    "clauses",
    "modules",
    "tests",
    "fuzz_targets",
    "benchmarks",
    "introduced_in",
];
const REQUIREMENT_KEYS: &[&str] = &[
    "id",
    "snapshot_hash",
    "summary",
    "features",
    "implementation",
    "tests",
    "status",
    "notes",
    "component_notes",
    "repair_service_notes",
];
const DATA_KEYS: &[&str] = &[
    "id",
    "kind",
    "source",
    "acquired_at",
    "version",
    "source_hash",
    "license_expression",
    "redistribution",
    "contains_personal_data",
    "generated_by",
    "generator_revision",
    "generator_schema",
    "output_hash",
    "owner",
    "update_policy",
    "delete_policy",
    "authored_by",
    "format_schema",
    "validated_by",
];
const EVIDENCE_KEYS: &[&str] = &[
    "schema",
    "type",
    "id",
    "profile",
    "feature",
    "role",
    "oracle",
    "eligibility",
    "registered",
    "gating",
    "external_observation",
    "target",
    "requirements",
    "subject_kind",
    "subjects",
    "executed_tests",
    "cross_references",
    "fuzz_targets",
    "benchmarks",
    "verdict",
];
const SUBJECT_REPORT_KEYS: &[&str] = &[
    "schema",
    "type",
    "id",
    "evidence_kind",
    "feature",
    "target",
    "executed_tests",
    "fuzz_targets",
    "benchmarks",
    "case_ids",
    "reviewers",
    "independent",
    "raw_samples_ns",
    "performance_eligible",
    "release_gate_eligible",
    "external_comparison",
    "regression_threshold_eligible",
    "full_session",
    "fingerprint",
    "fingerprint_components",
    "minimizer",
    "dictionary",
    "crash_minimization",
    "owner",
    "invariant",
    "cargo_fuzz_version",
    "correction_commit",
    "pre_fix_output",
    "native_output",
    "reference_output",
    "commit",
    "cargo_profile",
    "cargo_flags",
    "rustc",
    "os",
    "cpu",
    "gpu",
    "memory_bytes",
    "browser",
    "renderer_epoch",
    "font_epoch",
    "color_epoch",
    "timing_scope",
    "memory_scope",
    "support_scope",
    "corpus_sha256",
    "cache_policy",
    "sample_count",
    "median_ns",
    "p95_ns",
    "p99_ns",
    "median_ci95_ns",
    "reference_checked",
    "commit_scope",
    "worktree_scope",
    "verdict",
];

const FEATURE_MAP_PATH: &str = "docs/traceability/feature-map.toml";
const SPEC_MAP_PATH: &str = "docs/traceability/spec-map.toml";
const DATA_LEDGER_PATH: &str = "docs/traceability/data-ledger.toml";
const MATURITY_EVIDENCE_KIND: &str = "project-authored-maturity-evidence";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaturityReport {
    pub(crate) profiles: usize,
    pub(crate) planned: usize,
    pub(crate) reference: usize,
    pub(crate) differential: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaturityDiagnostic {
    code: &'static str,
    profile: Option<String>,
    field: Option<String>,
    line: Option<usize>,
}

impl MaturityDiagnostic {
    fn syntax(line: usize) -> Self {
        Self {
            code: "RPE-MATURITY-0001",
            profile: None,
            field: None,
            line: Some(line),
        }
    }

    fn root(code: &'static str, field: impl Into<String>) -> Self {
        Self {
            code,
            profile: None,
            field: Some(field.into()),
            line: None,
        }
    }

    fn profile(code: &'static str, profile: &str, field: impl Into<String>) -> Self {
        Self {
            code,
            profile: Some(profile.into()),
            field: Some(field.into()),
            line: None,
        }
    }
}

impl fmt::Display for MaturityDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.code)?;
        if let Some(line) = self.line {
            write!(formatter, " line={line}")?;
        }
        if let Some(profile) = &self.profile {
            write!(formatter, " profile={profile}")?;
        }
        if let Some(field) = &self.field {
            write!(formatter, " field={field}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ParsedProfiles {
    root: BTreeMap<String, String>,
    profiles: Vec<BTreeMap<String, String>>,
}

#[derive(Debug)]
struct ParsedRecords {
    root: BTreeMap<String, String>,
    records: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MaturityState {
    Planned,
    Reference,
    Differential,
}

impl MaturityState {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("PLANNED") => Some(Self::Planned),
            Some("REFERENCE") => Some(Self::Reference),
            Some("DIFFERENTIAL") => Some(Self::Differential),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "PLANNED",
            Self::Reference => "REFERENCE",
            Self::Differential => "DIFFERENTIAL",
        }
    }
}

struct RepositoryMaps {
    features: BTreeMap<String, BTreeMap<String, String>>,
    requirements: BTreeMap<String, BTreeMap<String, String>>,
    ledger: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContentReference {
    relative: String,
    digest: String,
}

#[derive(Clone, Debug)]
struct EvidenceUse {
    field: String,
    reference: String,
    role: &'static str,
    oracle: &'static str,
}

#[derive(Debug)]
struct EvidenceArtifact {
    id: String,
    subjects: Vec<String>,
    executed_tests: Vec<String>,
    cross_references: Vec<String>,
    fuzz_targets: Vec<String>,
    benchmarks: Vec<String>,
}

struct VerifiedSubject {
    relative: String,
    bytes: Vec<u8>,
    package_registered: bool,
}

pub(crate) fn validate_maturity_file(
    path: &Path,
) -> Result<MaturityReport, Vec<MaturityDiagnostic>> {
    let input = fs::read_to_string(path)
        .map_err(|_| vec![MaturityDiagnostic::root("RPE-MATURITY-0002", "unreadable")])?;
    let parsed = parse_profiles(&input)?;
    let (report, mut diagnostics) = validate_profile_shape(&parsed);

    let repository_root = match derive_repository_root(path) {
        Ok(root) => root,
        Err(diagnostic) => {
            diagnostics.push(diagnostic);
            return Err(diagnostics);
        }
    };
    let maps = match load_repository_maps(&repository_root) {
        Ok(maps) => maps,
        Err(mut map_diagnostics) => {
            diagnostics.append(&mut map_diagnostics);
            return Err(diagnostics);
        }
    };

    validate_repository_links(&parsed, &repository_root, &maps, &mut diagnostics);
    if diagnostics.is_empty() {
        Ok(report)
    } else {
        Err(diagnostics)
    }
}

#[cfg(test)]
fn validate_maturity(input: &str) -> Result<MaturityReport, Vec<MaturityDiagnostic>> {
    let parsed = parse_profiles(input)?;
    let (report, diagnostics) = validate_profile_shape(&parsed);
    if diagnostics.is_empty() {
        Ok(report)
    } else {
        Err(diagnostics)
    }
}

fn validate_profile_shape(parsed: &ParsedProfiles) -> (MaturityReport, Vec<MaturityDiagnostic>) {
    let mut diagnostics = Vec::new();
    validate_root(&parsed.root, &mut diagnostics);
    if parsed.profiles.is_empty() {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0005", "profile"));
    }

    let mut identities = BTreeSet::new();
    let mut report = MaturityReport {
        profiles: parsed.profiles.len(),
        planned: 0,
        reference: 0,
        differential: 0,
    };
    for (index, profile) in parsed.profiles.iter().enumerate() {
        let identity = profile_identity(profile, index);
        for key in PROFILE_KEYS {
            if !profile.contains_key(*key) {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0006",
                    &identity,
                    *key,
                ));
            }
        }
        if !valid_id(&identity) || !identities.insert(identity.clone()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0007",
                &identity,
                "id",
            ));
        }
        for field in ["requirements", "supported", "excluded"] {
            if string_array(profile.get(field)).is_none_or(|values| values.is_empty()) {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0008",
                    &identity,
                    field,
                ));
            }
        }
        for field in ["owner", "feature", "policy", "target", "reference"] {
            if string(profile.get(field)).is_none_or(str::is_empty) {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0008",
                    &identity,
                    field,
                ));
            }
        }

        let state = MaturityState::parse(string(profile.get("state")));
        match state {
            Some(MaturityState::Planned) => report.planned += 1,
            Some(MaturityState::Reference) => {
                report.reference += 1;
                require_reference_evidence(profile, &identity, &mut diagnostics);
            }
            Some(MaturityState::Differential) => {
                report.differential += 1;
                require_reference_evidence(profile, &identity, &mut diagnostics);
                require_differential_evidence(profile, &identity, &mut diagnostics);
            }
            None => diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0009",
                &identity,
                "state",
            )),
        }
        if state != Some(MaturityState::Planned)
            && string(profile.get("target")).is_none_or(|target| !valid_symbolic_target(target))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0013",
                &identity,
                "target",
            ));
        }
    }
    (report, diagnostics)
}

fn validate_root(root: &BTreeMap<String, String>, diagnostics: &mut Vec<MaturityDiagnostic>) {
    for key in ROOT_KEYS {
        if !root.contains_key(*key) {
            diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0003", *key));
        }
    }
    if root.get("schema").map(String::as_str) != Some("1") {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0004", "schema"));
    }
    if root.get("status").and_then(|value| unquote(value)) != Some("active") {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0004", "status"));
    }
}

fn require_reference_evidence(
    profile: &BTreeMap<String, String>,
    identity: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    for field in ["o0_cases", "o1_cases"] {
        if string_array(profile.get(field)).is_none_or(|values| values.is_empty()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0010",
                identity,
                field,
            ));
        }
    }
    for field in ["target", "reference", "independent_review"] {
        if string(profile.get(field)).is_none_or(is_placeholder) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0010",
                identity,
                field,
            ));
        }
    }
}

fn require_differential_evidence(
    profile: &BTreeMap<String, String>,
    identity: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    for field in ["o2_adjudications", "fuzz_targets"] {
        if string_array(profile.get(field)).is_none_or(|values| values.is_empty()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0011",
                identity,
                field,
            ));
        }
    }
    for field in [
        "fuzz_minimizer",
        "holdout_manifest",
        "benchmark_report",
        "differential_report",
        "baseline_fingerprint",
    ] {
        if string(profile.get(field)).is_none_or(is_placeholder) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0011",
                identity,
                field,
            ));
        }
    }
}

fn validate_repository_links(
    parsed: &ParsedProfiles,
    repository_root: &Path,
    maps: &RepositoryMaps,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    for (index, profile) in parsed.profiles.iter().enumerate() {
        let identity = profile_identity(profile, index);
        let Some(state) = MaturityState::parse(string(profile.get("state"))) else {
            continue;
        };
        let Some(feature_id) = string(profile.get("feature")) else {
            continue;
        };
        let feature = maps.features.get(feature_id);
        match feature {
            Some(feature) => {
                if string(feature.get("profile")) != Some(identity.as_str()) {
                    diagnostics.push(MaturityDiagnostic::profile(
                        "RPE-MATURITY-0016",
                        &identity,
                        "feature.profile",
                    ));
                }
                if string(feature.get("state")) != Some(state.as_str()) {
                    diagnostics.push(MaturityDiagnostic::profile(
                        "RPE-MATURITY-0016",
                        &identity,
                        "feature.state",
                    ));
                }
            }
            None => diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0016",
                &identity,
                "feature",
            )),
        }
        if state == MaturityState::Planned {
            continue;
        }
        validate_requirement_links(profile, &identity, feature_id, maps, diagnostics);
        if let Some(feature) = feature {
            validate_promoted_evidence(
                profile,
                &identity,
                state,
                feature,
                repository_root,
                maps,
                diagnostics,
            );
        }
    }
}

fn validate_requirement_links(
    profile: &BTreeMap<String, String>,
    identity: &str,
    feature_id: &str,
    maps: &RepositoryMaps,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let Some(requirements) = string_array(profile.get("requirements")) else {
        return;
    };
    if has_duplicates(&requirements) {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0017",
            identity,
            "requirements",
        ));
    }
    for requirement in requirements {
        let Some(record) = maps.requirements.get(requirement) else {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0017",
                identity,
                format!("requirements:{requirement}"),
            ));
            continue;
        };
        if string_array(record.get("features"))
            .is_none_or(|features| !features.contains(&feature_id))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0017",
                identity,
                format!("requirements:{requirement}:feature"),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_promoted_evidence(
    profile: &BTreeMap<String, String>,
    identity: &str,
    state: MaturityState,
    feature: &BTreeMap<String, String>,
    repository_root: &Path,
    maps: &RepositoryMaps,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let feature_id = string(profile.get("feature")).unwrap_or_default();
    let target = string(profile.get("target")).unwrap_or_default();
    let requirements = owned_array(profile.get("requirements"));
    let profile_fuzz_targets = owned_array(profile.get("fuzz_targets"));
    let feature_fuzz_targets = owned_array(feature.get("fuzz_targets"));
    let feature_benchmarks = owned_array(feature.get("benchmarks"));
    let feature_tests = owned_array(feature.get("tests"));

    if state == MaturityState::Differential {
        validate_registered_ids(
            identity,
            "fuzz_targets",
            &profile_fuzz_targets,
            &feature_fuzz_targets,
            diagnostics,
        );
        if feature_benchmarks.is_empty()
            || has_owned_duplicates(&feature_benchmarks)
            || feature_benchmarks.iter().any(|value| !valid_id(value))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0020",
                identity,
                "feature.benchmarks",
            ));
        }
    }

    let uses = evidence_uses(profile, state);
    let mut references = BTreeSet::new();
    let mut relative_paths = BTreeSet::new();
    let mut artifacts = BTreeMap::new();
    let mut artifact_ids = BTreeSet::new();
    for evidence_use in &uses {
        if !references.insert(evidence_use.reference.clone()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0019",
                identity,
                &evidence_use.field,
            ));
            continue;
        }
        let content_reference = match parse_content_reference(&evidence_use.reference) {
            Ok(reference) => reference,
            Err(()) => {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0013",
                    identity,
                    &evidence_use.field,
                ));
                continue;
            }
        };
        if !relative_paths.insert(content_reference.relative.clone()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0019",
                identity,
                &evidence_use.field,
            ));
            continue;
        }
        let bytes = match read_content_addressed(
            repository_root,
            &content_reference,
            identity,
            &evidence_use.field,
            diagnostics,
        ) {
            Some(bytes) => bytes,
            None => continue,
        };
        let artifact = match parse_evidence_artifact(
            &bytes,
            evidence_use,
            identity,
            feature_id,
            target,
            &requirements,
            diagnostics,
        ) {
            Some(artifact) => artifact,
            None => continue,
        };
        validate_ledger_binding(
            maps,
            &content_reference,
            &artifact.id,
            identity,
            &evidence_use.field,
            diagnostics,
        );
        if !artifact_ids.insert(artifact.id.clone()) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0019",
                identity,
                format!("{}:id", evidence_use.field),
            ));
        }
        artifacts.insert(evidence_use.reference.clone(), artifact);
    }

    validate_evidence_graph(
        profile,
        identity,
        state,
        &uses,
        &artifacts,
        repository_root,
        &profile_fuzz_targets,
        &feature_benchmarks,
        diagnostics,
    );
    validate_artifact_subjects(
        repository_root,
        identity,
        feature_id,
        target,
        &uses,
        &artifacts,
        &feature_tests,
        &profile_fuzz_targets,
        &feature_benchmarks,
        diagnostics,
    );
}

fn validate_registered_ids(
    identity: &str,
    field: &str,
    requested: &[String],
    registered: &[String],
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let invalid = requested.is_empty()
        || registered.is_empty()
        || has_owned_duplicates(requested)
        || has_owned_duplicates(registered)
        || requested.iter().any(|value| !valid_id(value))
        || registered.iter().any(|value| !valid_id(value))
        || requested.iter().any(|value| !registered.contains(value));
    if invalid {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0020",
            identity,
            field,
        ));
    }
}

fn evidence_uses(profile: &BTreeMap<String, String>, state: MaturityState) -> Vec<EvidenceUse> {
    let mut uses = Vec::new();
    push_scalar_use(
        &mut uses,
        profile,
        "reference",
        "reference-implementation",
        "O1",
    );
    push_array_uses(&mut uses, profile, "o0_cases", "o0-case", "O0");
    push_array_uses(&mut uses, profile, "o1_cases", "o1-case", "O1");
    push_scalar_use(
        &mut uses,
        profile,
        "independent_review",
        "independent-review",
        "INTERNAL",
    );
    if state == MaturityState::Differential {
        push_array_uses(
            &mut uses,
            profile,
            "o2_adjudications",
            "o2-adjudication",
            "O2",
        );
        push_scalar_use(
            &mut uses,
            profile,
            "fuzz_minimizer",
            "fuzz-minimizer",
            "INTERNAL",
        );
        push_scalar_use(
            &mut uses,
            profile,
            "holdout_manifest",
            "holdout-manifest",
            "INTERNAL",
        );
        push_scalar_use(
            &mut uses,
            profile,
            "benchmark_report",
            "benchmark-report",
            "INTERNAL",
        );
        push_scalar_use(
            &mut uses,
            profile,
            "differential_report",
            "differential-report",
            "O2",
        );
        push_scalar_use(
            &mut uses,
            profile,
            "baseline_fingerprint",
            "reference-fingerprint",
            "O1",
        );
    }
    uses
}

fn push_scalar_use(
    uses: &mut Vec<EvidenceUse>,
    profile: &BTreeMap<String, String>,
    field: &str,
    role: &'static str,
    oracle: &'static str,
) {
    if let Some(reference) = string(profile.get(field))
        && !is_placeholder(reference)
    {
        uses.push(EvidenceUse {
            field: field.into(),
            reference: reference.into(),
            role,
            oracle,
        });
    }
}

fn push_array_uses(
    uses: &mut Vec<EvidenceUse>,
    profile: &BTreeMap<String, String>,
    field: &str,
    role: &'static str,
    oracle: &'static str,
) {
    if let Some(references) = string_array(profile.get(field)) {
        for (index, reference) in references.into_iter().enumerate() {
            uses.push(EvidenceUse {
                field: format!("{field}[{index}]"),
                reference: reference.into(),
                role,
                oracle,
            });
        }
    }
}

fn subject_kind_for_role(role: &str) -> &'static str {
    match role {
        "reference-implementation" => "reference-implementation-and-tests",
        "o0-case" => "normative-case",
        "o1-case" => "analytic-case",
        "independent-review" => "independent-review-report",
        "o2-adjudication" => "adjudication-report",
        "fuzz-minimizer" => "fuzz-target-and-minimized-corpus",
        "holdout-manifest" => "holdout-manifest",
        "benchmark-report" => "raw-sample-benchmark-report",
        "differential-report" => "full-session-differential-result",
        "reference-fingerprint" => "reference-fingerprint-report",
        _ => "invalid",
    }
}

fn parse_content_reference(value: &str) -> Result<ContentReference, ()> {
    let (relative, digest) = value.split_once("#sha256:").ok_or(())?;
    if relative.is_empty()
        || relative.contains(['#', '\\', ':'])
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(());
    }
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(());
    }
    Ok(ContentReference {
        relative: relative.into(),
        digest: digest.into(),
    })
}

fn read_content_addressed(
    repository_root: &Path,
    reference: &ContentReference,
    identity: &str,
    field: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) -> Option<Vec<u8>> {
    let mut current = repository_root.to_path_buf();
    let components: Vec<_> = Path::new(&reference.relative).components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0013",
                identity,
                field,
            ));
            return None;
        };
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0014",
                    identity,
                    field,
                ));
                return None;
            }
        };
        if metadata.file_type().is_symlink()
            || (index + 1 == components.len() && !metadata.file_type().is_file())
            || (index + 1 != components.len() && !metadata.file_type().is_dir())
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0014",
                identity,
                field,
            ));
            return None;
        }
    }
    let canonical_root = match fs::canonicalize(repository_root) {
        Ok(path) => path,
        Err(_) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0014",
                identity,
                field,
            ));
            return None;
        }
    };
    let canonical_file = match fs::canonicalize(&current) {
        Ok(path) => path,
        Err(_) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0014",
                identity,
                field,
            ));
            return None;
        }
    };
    if !canonical_file.starts_with(canonical_root) {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0014",
            identity,
            field,
        ));
        return None;
    }
    let bytes = match fs::read(&current) {
        Ok(bytes) => bytes,
        Err(_) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0014",
                identity,
                field,
            ));
            return None;
        }
    };
    let actual = match sha256(&bytes) {
        Ok(digest) => hex_digest(&digest),
        Err(_) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0014",
                identity,
                field,
            ));
            return None;
        }
    };
    if actual != reference.digest {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0014",
            identity,
            field,
        ));
        return None;
    }
    Some(bytes)
}

fn validate_ledger_binding(
    maps: &RepositoryMaps,
    reference: &ContentReference,
    artifact_id: &str,
    identity: &str,
    field: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let expected_hash = format!("sha256:{}", reference.digest);
    let matching: Vec<_> = maps
        .ledger
        .iter()
        .filter(|record| string(record.get("source")) == Some(reference.relative.as_str()))
        .collect();
    if matching.len() != 1
        || string(matching[0].get("id")) != Some(artifact_id)
        || string(matching[0].get("source_hash")) != Some(expected_hash.as_str())
        || string(matching[0].get("kind")) != Some(MATURITY_EVIDENCE_KIND)
    {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0018",
            identity,
            field,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_evidence_artifact(
    bytes: &[u8],
    evidence_use: &EvidenceUse,
    identity: &str,
    feature_id: &str,
    target: &str,
    requirements: &[String],
    diagnostics: &mut Vec<MaturityDiagnostic>,
) -> Option<EvidenceArtifact> {
    let input = match std::str::from_utf8(bytes) {
        Ok(input) => input,
        Err(_) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0015",
                identity,
                &evidence_use.field,
            ));
            return None;
        }
    };
    let values = match parse_flat(input, EVIDENCE_KEYS) {
        Ok(values) => values,
        Err(()) => {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0015",
                identity,
                &evidence_use.field,
            ));
            return None;
        }
    };
    for key in EVIDENCE_KEYS {
        if !values.contains_key(*key) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0015",
                identity,
                format!("{}:{key}", evidence_use.field),
            ));
        }
    }
    let id = string(values.get("id")).unwrap_or_default().to_owned();
    let expected_subject_kind = subject_kind_for_role(evidence_use.role);
    let semantic_invalid = values.get("schema").map(String::as_str) != Some("1")
        || string(values.get("type")) != Some("maturity-evidence")
        || !valid_id(&id)
        || string(values.get("profile")) != Some(identity)
        || string(values.get("feature")) != Some(feature_id)
        || string(values.get("role")) != Some(evidence_use.role)
        || string(values.get("oracle")) != Some(evidence_use.oracle)
        || string(values.get("oracle")) == Some("O4")
        || boolean(values.get("eligibility")) != Some(true)
        || boolean(values.get("registered")) != Some(true)
        || boolean(values.get("gating")) != Some(true)
        || boolean(values.get("external_observation")) != Some(false)
        || string(values.get("target")) != Some(target)
        || string(values.get("subject_kind")) != Some(expected_subject_kind)
        || string(values.get("verdict")) != Some("pass");
    let artifact_requirements = owned_array(values.get("requirements"));
    let requirements_invalid = artifact_requirements.is_empty()
        || has_owned_duplicates(&artifact_requirements)
        || as_set(&artifact_requirements) != as_set(requirements);
    if semantic_invalid || requirements_invalid {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0015",
            identity,
            &evidence_use.field,
        ));
    }
    let subjects = owned_array(values.get("subjects"));
    let executed_tests = owned_array(values.get("executed_tests"));
    let cross_references = owned_array(values.get("cross_references"));
    let fuzz_targets = owned_array(values.get("fuzz_targets"));
    let benchmarks = owned_array(values.get("benchmarks"));
    Some(EvidenceArtifact {
        id,
        subjects,
        executed_tests,
        cross_references,
        fuzz_targets,
        benchmarks,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_evidence_graph(
    profile: &BTreeMap<String, String>,
    identity: &str,
    state: MaturityState,
    uses: &[EvidenceUse],
    artifacts: &BTreeMap<String, EvidenceArtifact>,
    repository_root: &Path,
    profile_fuzz_targets: &[String],
    feature_benchmarks: &[String],
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let reference = string(profile.get("reference"))
        .unwrap_or_default()
        .to_owned();
    let o0 = owned_array(profile.get("o0_cases"));
    let o1 = owned_array(profile.get("o1_cases"));
    let mut reference_roots = vec![reference.clone()];
    reference_roots.extend(o0.clone());
    reference_roots.extend(o1.clone());

    let o2 = owned_array(profile.get("o2_adjudications"));
    let minimizer = string(profile.get("fuzz_minimizer"))
        .unwrap_or_default()
        .to_owned();
    let holdout = string(profile.get("holdout_manifest"))
        .unwrap_or_default()
        .to_owned();
    let benchmark = string(profile.get("benchmark_report"))
        .unwrap_or_default()
        .to_owned();
    let differential = string(profile.get("differential_report"))
        .unwrap_or_default()
        .to_owned();

    for evidence_use in uses {
        let Some(artifact) = artifacts.get(&evidence_use.reference) else {
            continue;
        };
        let expected_cross = match evidence_use.role {
            "o0-case" | "o1-case" | "reference-implementation" => Vec::new(),
            "independent-review" => reference_roots.clone(),
            "o2-adjudication" => reference_roots.clone(),
            "fuzz-minimizer" => o2.clone(),
            "holdout-manifest" => {
                let mut roots = o0.clone();
                roots.extend(o1.clone());
                roots
            }
            "benchmark-report" => vec![reference.clone()],
            "differential-report" => {
                let mut roots = vec![reference.clone()];
                roots.extend(o2.clone());
                roots.extend([minimizer.clone(), holdout.clone(), benchmark.clone()]);
                roots
            }
            "reference-fingerprint" => vec![reference.clone(), differential.clone()],
            _ => Vec::new(),
        };
        if has_owned_duplicates(&artifact.cross_references)
            || as_set(&artifact.cross_references) != as_set(&expected_cross)
            || artifact
                .cross_references
                .iter()
                .any(|value| value == &evidence_use.reference || !artifacts.contains_key(value))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0015",
                identity,
                format!("{}:cross_references", evidence_use.field),
            ));
        }
        let expected_fuzz = if evidence_use.role == "fuzz-minimizer" {
            profile_fuzz_targets
        } else {
            &[]
        };
        let expected_benchmarks = if evidence_use.role == "benchmark-report" {
            feature_benchmarks
        } else {
            &[]
        };
        if has_owned_duplicates(&artifact.fuzz_targets)
            || as_set(&artifact.fuzz_targets) != as_set(expected_fuzz)
            || has_owned_duplicates(&artifact.benchmarks)
            || as_set(&artifact.benchmarks) != as_set(expected_benchmarks)
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0020",
                identity,
                &evidence_use.field,
            ));
        }
    }
    if state == MaturityState::Differential && artifacts.len() != uses.len() {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0015",
            identity,
            "evidence_graph",
        ));
    }
    if state == MaturityState::Differential {
        validate_holdout_case_separation(identity, uses, artifacts, repository_root, diagnostics);
        validate_commit_anchor_consistency(identity, uses, artifacts, repository_root, diagnostics);
    }
}

fn validate_commit_anchor_consistency(
    identity: &str,
    uses: &[EvidenceUse],
    artifacts: &BTreeMap<String, EvidenceArtifact>,
    repository_root: &Path,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let benchmark_values = uses
        .iter()
        .find(|evidence_use| evidence_use.role == "benchmark-report")
        .and_then(|evidence_use| artifacts.get(&evidence_use.reference))
        .and_then(|artifact| artifact_report_values(repository_root, artifact, false));
    let environment_values = uses
        .iter()
        .find(|evidence_use| evidence_use.role == "reference-fingerprint")
        .and_then(|evidence_use| artifacts.get(&evidence_use.reference))
        .and_then(|artifact| artifact_report_values(repository_root, artifact, true));
    let anchors_match =
        benchmark_values
            .zip(environment_values)
            .is_some_and(|(benchmark, environment)| {
                [
                    "commit",
                    "commit_scope",
                    "worktree_scope",
                    "cargo_profile",
                    "cargo_flags",
                    "rustc",
                    "os",
                    "cpu",
                    "gpu",
                    "memory_bytes",
                    "browser",
                ]
                .iter()
                .all(|field| benchmark.get(*field) == environment.get(*field))
            });
    if !anchors_match {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0025",
            identity,
            "benchmark_report:commit_anchor",
        ));
    }
}

fn artifact_report_values(
    repository_root: &Path,
    artifact: &EvidenceArtifact,
    run_environment: bool,
) -> Option<BTreeMap<String, String>> {
    artifact.subjects.iter().find_map(|subject| {
        let reference = parse_content_reference(subject).ok()?;
        let bytes = fs::read(repository_root.join(reference.relative)).ok()?;
        let input = std::str::from_utf8(&bytes).ok()?;
        if run_environment {
            let lines = logical_lines(input).ok()?;
            let values: BTreeMap<_, _> = lines
                .into_iter()
                .filter_map(|(_, line)| {
                    split_assignment(&line).map(|(key, value)| (key.to_owned(), value.to_owned()))
                })
                .collect();
            (string(values.get("type")) == Some("m1-run-environment")).then_some(values)
        } else {
            let values = parse_subject_report(input).ok()?;
            (string(values.get("evidence_kind")) == Some("raw-sample-benchmark-report"))
                .then_some(values)
        }
    })
}

fn validate_holdout_case_separation(
    identity: &str,
    uses: &[EvidenceUse],
    artifacts: &BTreeMap<String, EvidenceArtifact>,
    repository_root: &Path,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let holdout_case_ids: BTreeSet<_> = uses
        .iter()
        .filter(|evidence_use| evidence_use.role == "holdout-manifest")
        .filter_map(|evidence_use| artifacts.get(&evidence_use.reference))
        .flat_map(|artifact| artifact_case_ids(repository_root, artifact))
        .collect();
    let development_case_ids: BTreeSet<_> = uses
        .iter()
        .filter(|evidence_use| {
            matches!(
                evidence_use.role,
                "reference-implementation" | "o0-case" | "o1-case" | "o2-adjudication"
            )
        })
        .filter_map(|evidence_use| artifacts.get(&evidence_use.reference))
        .flat_map(|artifact| artifact_case_ids(repository_root, artifact))
        .collect();
    if !holdout_case_ids.is_disjoint(&development_case_ids) {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0023",
            identity,
            "holdout_manifest:case_overlap",
        ));
    }

    let holdout_pdf_digests: BTreeSet<_> = uses
        .iter()
        .filter(|evidence_use| evidence_use.role == "holdout-manifest")
        .filter_map(|evidence_use| artifacts.get(&evidence_use.reference))
        .flat_map(|artifact| artifact_pdf_digests(artifact, "tests/cases/"))
        .collect();
    let fuzz_pdf_digests: BTreeSet<_> = uses
        .iter()
        .filter(|evidence_use| evidence_use.role == "fuzz-minimizer")
        .filter_map(|evidence_use| artifacts.get(&evidence_use.reference))
        .flat_map(|artifact| {
            artifact_pdf_digests(artifact, "tools/quality/fuzz/corpus/m1_document_services/")
        })
        .collect();
    if !holdout_pdf_digests.is_disjoint(&fuzz_pdf_digests) {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0024",
            identity,
            "holdout_manifest:fuzz_digest_overlap",
        ));
    }
}

fn artifact_case_ids(repository_root: &Path, artifact: &EvidenceArtifact) -> Vec<String> {
    artifact
        .subjects
        .iter()
        .filter_map(|subject| {
            let reference = parse_content_reference(subject).ok()?;
            reference.relative.ends_with("/case.toml").then_some(())?;
            let input = fs::read_to_string(repository_root.join(reference.relative)).ok()?;
            crate::manifest::validate_manifest(&input)
                .ok()
                .map(|manifest| manifest.case_id().to_owned())
        })
        .collect()
}

fn artifact_pdf_digests<'a>(
    artifact: &'a EvidenceArtifact,
    prefix: &'a str,
) -> impl Iterator<Item = String> + 'a {
    artifact.subjects.iter().filter_map(move |subject| {
        let reference = parse_content_reference(subject).ok()?;
        (reference.relative.starts_with(prefix) && reference.relative.ends_with(".pdf"))
            .then_some(reference.digest)
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_artifact_subjects(
    repository_root: &Path,
    identity: &str,
    feature_id: &str,
    target: &str,
    uses: &[EvidenceUse],
    artifacts: &BTreeMap<String, EvidenceArtifact>,
    feature_tests: &[String],
    profile_fuzz_targets: &[String],
    feature_benchmarks: &[String],
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    if feature_tests.is_empty() || has_owned_duplicates(feature_tests) {
        diagnostics.push(MaturityDiagnostic::profile(
            "RPE-MATURITY-0022",
            identity,
            "feature.tests",
        ));
    }
    let evidence_references: BTreeSet<_> = uses
        .iter()
        .map(|evidence_use| evidence_use.reference.as_str())
        .collect();
    let evidence_paths: BTreeSet<_> = evidence_references
        .iter()
        .filter_map(|reference| parse_content_reference(reference).ok())
        .map(|reference| reference.relative)
        .collect();
    let mut subject_owners: BTreeMap<String, &'static str> = BTreeMap::new();

    for evidence_use in uses {
        let Some(artifact) = artifacts.get(&evidence_use.reference) else {
            continue;
        };
        if artifact.executed_tests.is_empty()
            || has_owned_duplicates(&artifact.executed_tests)
            || artifact
                .executed_tests
                .iter()
                .any(|test| !feature_tests.contains(test))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0022",
                identity,
                format!("{}:executed_tests", evidence_use.field),
            ));
        }
        if artifact.subjects.is_empty() || has_owned_duplicates(&artifact.subjects) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0021",
                identity,
                format!("{}:subjects", evidence_use.field),
            ));
        }

        let mut local_paths = BTreeSet::new();
        let mut verified = Vec::new();
        for (index, subject) in artifact.subjects.iter().enumerate() {
            let field = format!("{}:subjects[{index}]", evidence_use.field);
            let content_reference = match parse_content_reference(subject) {
                Ok(reference) => reference,
                Err(()) => {
                    diagnostics.push(MaturityDiagnostic::profile(
                        "RPE-MATURITY-0013",
                        identity,
                        &field,
                    ));
                    continue;
                }
            };
            if evidence_references.contains(subject.as_str())
                || evidence_paths.contains(&content_reference.relative)
                || !local_paths.insert(content_reference.relative.clone())
            {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0021",
                    identity,
                    &field,
                ));
                continue;
            }
            let bytes = match read_content_addressed(
                repository_root,
                &content_reference,
                identity,
                &field,
                diagnostics,
            ) {
                Some(bytes) => bytes,
                None => continue,
            };
            if let Some(previous_role) = subject_owners.get(&content_reference.relative)
                && !shareable_subject(
                    &content_reference.relative,
                    &bytes,
                    previous_role,
                    evidence_use.role,
                )
            {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0019",
                    identity,
                    &field,
                ));
            } else {
                subject_owners
                    .entry(content_reference.relative.clone())
                    .or_insert(evidence_use.role);
            }
            verified.push(VerifiedSubject {
                package_registered: workspace_package_registered(
                    repository_root,
                    &content_reference.relative,
                ),
                relative: content_reference.relative,
                bytes,
            });
        }
        if !verified
            .iter()
            .any(|subject| executable_test_binds(subject, &artifact.executed_tests))
        {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0022",
                identity,
                format!("{}:executed_test_subject", evidence_use.field),
            ));
        }
        if !subject_binding_matches(
            evidence_use,
            artifact,
            &verified,
            feature_id,
            target,
            profile_fuzz_targets,
            feature_benchmarks,
        ) {
            diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0021",
                identity,
                format!("{}:subject_binding", evidence_use.field),
            ));
        }
    }
}

fn shareable_subject(relative: &str, bytes: &[u8], previous_role: &str, role: &str) -> bool {
    let source_roles = [
        "reference-implementation",
        "o0-case",
        "o1-case",
        "fuzz-minimizer",
        "differential-report",
        "reference-fingerprint",
        "o2-adjudication",
        "holdout-manifest",
        "benchmark-report",
        "independent-review",
    ];
    let multi_service_roles = [
        "o2-adjudication",
        "fuzz-minimizer",
        "holdout-manifest",
        "benchmark-report",
        "reference-fingerprint",
    ];
    is_executable_test_source(relative, bytes)
        || (previous_role == role && multi_service_roles.contains(&role))
        || ((previous_role == "reference-fingerprint" || role == "reference-fingerprint")
            && relative.starts_with("tests/cases/"))
        || ((matches!(previous_role, "holdout-manifest" | "o2-adjudication")
            && role == "benchmark-report"
            || previous_role == "benchmark-report"
                && matches!(role, "holdout-manifest" | "o2-adjudication"))
            && relative.starts_with("tests/cases/"))
        || (is_rust_source(relative, bytes)
            && source_roles.contains(&previous_role)
            && source_roles.contains(&role))
}

fn subject_binding_matches(
    evidence_use: &EvidenceUse,
    artifact: &EvidenceArtifact,
    subjects: &[VerifiedSubject],
    feature_id: &str,
    target: &str,
    profile_fuzz_targets: &[String],
    feature_benchmarks: &[String],
) -> bool {
    match evidence_use.role {
        "reference-implementation" => {
            subjects
                .iter()
                .any(|subject| implementation_binds_target(subject, target))
                && subjects
                    .iter()
                    .any(|subject| executable_test_binds(subject, &artifact.executed_tests))
        }
        "o0-case" => subjects.iter().any(|subject| {
            case_manifest_binds(subject, "O0", feature_id, &artifact.executed_tests)
        }),
        "o1-case" => subjects.iter().any(|subject| {
            case_manifest_binds(subject, "O1", feature_id, &artifact.executed_tests)
        }),
        "independent-review" => subjects.iter().any(|subject| {
            report_binds(
                subject,
                subject_kind_for_role(evidence_use.role),
                feature_id,
                target,
                artifact,
                profile_fuzz_targets,
                feature_benchmarks,
                ReportRequirement::IndependentReview,
            )
        }),
        "o2-adjudication" => {
            subjects.iter().any(|subject| {
                case_manifest_binds(subject, "O2", feature_id, &artifact.executed_tests)
                    && case_reviewers(subject).is_some_and(|reviewers| reviewers.len() >= 2)
            }) && subjects.iter().any(|subject| {
                executable_test_binds(subject, &artifact.executed_tests)
                    && rust_source(subject).is_some_and(|source| {
                        source.contains("frozen_pre_fix_projection")
                            && source.contains("run_native_bytes")
                            && source.contains("reference_result")
                            && source.contains("ill-typed-optional-references")
                    })
            }) && subjects.iter().any(|subject| {
                report_binds(
                    subject,
                    subject_kind_for_role(evidence_use.role),
                    feature_id,
                    target,
                    artifact,
                    profile_fuzz_targets,
                    feature_benchmarks,
                    ReportRequirement::Adjudication,
                )
            }) && adjudication_test_binds_report(subjects, artifact)
        }
        "fuzz-minimizer" => {
            profile_fuzz_targets
                .iter()
                .all(|target| fuzz_target_is_registered(subjects, target))
                && subjects.iter().any(|subject| {
                    fuzz_build_test_binds(subject, &artifact.executed_tests, profile_fuzz_targets)
                })
                && subjects.iter().any(|subject| {
                    subject.relative.contains("/fuzz/dictionaries/")
                        && subject.relative.ends_with(".dict")
                        && !subject.bytes.is_empty()
                })
                && subjects.iter().any(|subject| {
                    report_binds(
                        subject,
                        subject_kind_for_role(evidence_use.role),
                        feature_id,
                        target,
                        artifact,
                        profile_fuzz_targets,
                        feature_benchmarks,
                        ReportRequirement::Minimizer,
                    )
                })
        }
        "holdout-manifest" => {
            subjects.iter().any(|subject| {
                executable_test_binds(subject, &artifact.executed_tests)
                    && rust_source(subject).is_some_and(|source| {
                        source.contains("run_native_bytes")
                            && source.contains("reference_result")
                            && source.contains("assert_case_hashes")
                            && source.contains("m1-maturity-holdout")
                    })
            }) && subjects.iter().any(|subject| {
                report_binds(
                    subject,
                    subject_kind_for_role(evidence_use.role),
                    feature_id,
                    target,
                    artifact,
                    profile_fuzz_targets,
                    feature_benchmarks,
                    ReportRequirement::Holdout,
                )
            }) && holdout_cases_bind(subjects, artifact, feature_id)
        }
        "benchmark-report" => {
            subjects
                .iter()
                .any(|subject| benchmark_test_binds(subject, &artifact.executed_tests))
                && subjects.iter().any(|subject| {
                    report_binds(
                        subject,
                        subject_kind_for_role(evidence_use.role),
                        feature_id,
                        target,
                        artifact,
                        profile_fuzz_targets,
                        feature_benchmarks,
                        ReportRequirement::Benchmark,
                    )
                })
        }
        "differential-report" => {
            subjects
                .iter()
                .any(|subject| executable_test_binds(subject, &artifact.executed_tests))
                && subjects.iter().any(|subject| {
                    report_binds(
                        subject,
                        subject_kind_for_role(evidence_use.role),
                        feature_id,
                        target,
                        artifact,
                        profile_fuzz_targets,
                        feature_benchmarks,
                        ReportRequirement::Differential,
                    )
                })
        }
        "reference-fingerprint" => {
            subjects.iter().any(|subject| {
                report_binds(
                    subject,
                    subject_kind_for_role(evidence_use.role),
                    feature_id,
                    target,
                    artifact,
                    profile_fuzz_targets,
                    feature_benchmarks,
                    ReportRequirement::Fingerprint,
                )
            }) && run_environment_binds(subjects)
        }
        _ => false,
    }
}

fn implementation_binds_target(subject: &VerifiedSubject, target: &str) -> bool {
    let Some(source) = rust_source(subject) else {
        return false;
    };
    subject.package_registered
        && is_workspace_implementation_path(&subject.relative)
        && target_leaf(target).is_some_and(|leaf| source.contains(leaf))
}

fn executable_test_binds(subject: &VerifiedSubject, executed_tests: &[String]) -> bool {
    if !subject.package_registered || !is_executable_test_source(&subject.relative, &subject.bytes)
    {
        return false;
    }
    integration_test_id(&subject.relative).is_some_and(|test_target| {
        executed_tests.iter().any(|test| {
            test == &test_target
                || test
                    .strip_prefix(&test_target)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
    })
}

fn fuzz_target_is_registered(subjects: &[VerifiedSubject], target: &str) -> bool {
    let Some(target_name) = registration_leaf(target) else {
        return false;
    };
    subjects.iter().any(|manifest| {
        let Some(prefix) = manifest.relative.strip_suffix("/fuzz/Cargo.toml") else {
            return false;
        };
        let Ok(input) = std::str::from_utf8(&manifest.bytes) else {
            return false;
        };
        let Some(registrations) = parse_fuzz_registrations(input) else {
            return false;
        };
        let Some(path) = registrations.get(target_name) else {
            return false;
        };
        let expected_source = format!("{prefix}/fuzz/{path}");
        subjects.iter().any(|source| {
            source.relative == expected_source
                && source.package_registered
                && is_workspace_fuzz_target_path(&source.relative)
                && rust_source(source).is_some_and(|source| {
                    source.contains("#![no_main]")
                        && source.contains("use libfuzzer_sys::fuzz_target;")
                        && source.contains("fuzz_target!")
                })
        })
    })
}

fn fuzz_build_test_binds(
    subject: &VerifiedSubject,
    executed_tests: &[String],
    fuzz_targets: &[String],
) -> bool {
    executable_test_binds(subject, executed_tests)
        && rust_source(subject).is_some_and(|source| {
            source.contains("Command::new(\"cargo\")")
                && source.contains("\"check\"")
                && source.contains("\"--locked\"")
                && source.contains("\"--manifest-path\"")
                && source.contains("fuzz/Cargo.toml")
                && source.contains("\"fuzz\"")
                && source.contains("\"run\"")
                && source.contains("\"cmin\"")
                && source.contains("\"--version\"")
                && source.contains("cargo-fuzz 0.13.2")
                && source.contains(".github/workflows/ci.yml")
                && source.contains("cargo install --locked --version 0.13.2 cargo-fuzz")
                && source.contains("./scripts/ci.sh pr")
                && source.contains("\"--fuzz-dir\"")
                && source.contains("\"--sanitizer\"")
                && source.contains("\"none\"")
                && source.contains("-seed=424242")
                && source.contains("-runs=64")
                && source.contains("-max_len=1048576")
                && source.contains("-timeout=1")
                && source.contains("-rss_limit_mb=512")
                && source.contains("-dict=")
                && source.matches("-artifact_prefix=").count() >= 2
                && source.contains("pdf-rs-m1-fuzz-cmin-artifacts")
                && source.contains("get_args()")
                && source.contains("exactly one corpus positional argument")
                && ["minimal.pdf", "truncated-header.pdf", "nested-outline.pdf"]
                    .iter()
                    .all(|seed| source.contains(seed))
                && source.matches("for seed in FUZZ_SEEDS").count() >= 2
                && source.contains("m1-services/nested-valid/input.pdf")
                && fuzz_targets.iter().all(|target| {
                    registration_leaf(target).is_some_and(|target| source.contains(target))
                })
                && source.contains(".success()")
        })
}

fn benchmark_test_binds(subject: &VerifiedSubject, executed_tests: &[String]) -> bool {
    executable_test_binds(subject, executed_tests)
        && rust_source(subject).is_some_and(|source| {
            source.contains("Instant::now()")
                && source.contains("Vec::with_capacity(21)")
                && source.contains("for _ in 0..21")
                && source.contains("run_native_bytes")
                && source.contains("reference_result")
                && source.contains("benchmark warmup")
                && source.contains("assert_eq!(result, expected")
                && source.contains("samples.push")
                && source.contains("black_box")
        })
}

fn parse_fuzz_registrations(input: &str) -> Option<BTreeMap<String, String>> {
    let mut registrations = BTreeMap::new();
    let mut in_bin = false;
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut section = "";
    let mut publish_disabled = false;
    let mut cargo_fuzz = false;
    let mut libfuzzer_dependency = false;
    for (_, line) in logical_lines(input).ok()? {
        if line == "[[bin]]" {
            if in_bin {
                insert_fuzz_registration(&mut registrations, name.take()?, path.take()?)?;
            }
            in_bin = true;
            section = "bin";
            continue;
        }
        if line.starts_with('[') {
            if in_bin {
                insert_fuzz_registration(&mut registrations, name.take()?, path.take()?)?;
                in_bin = false;
            }
            section = match line.as_str() {
                "[package]" => "package",
                "[package.metadata]" => "package.metadata",
                "[dependencies]" => "dependencies",
                _ => "other",
            };
            continue;
        }
        if section == "package" {
            if let Some((key, value)) = split_assignment(&line)
                && key == "publish"
                && value == "false"
            {
                publish_disabled = true;
            }
            continue;
        }
        if section == "package.metadata" {
            let (key, value) = line.split_once('=')?;
            if key.trim() == "cargo-fuzz" && value.trim() == "true" {
                cargo_fuzz = true;
            }
            continue;
        }
        if section == "dependencies" {
            let (key, value) = line.split_once('=')?;
            if key.trim() == "libfuzzer-sys" && unquote(value.trim()) == Some("=0.4.13") {
                libfuzzer_dependency = true;
            }
            continue;
        }
        if !in_bin {
            continue;
        }
        let (key, value) = split_assignment(&line)?;
        match key {
            "name" => {
                if name.replace(unquote(value)?.to_owned()).is_some() {
                    return None;
                }
            }
            "path" => {
                if path.replace(unquote(value)?.to_owned()).is_some() {
                    return None;
                }
            }
            "test" | "doc" | "bench" if value == "false" => {}
            _ => return None,
        }
    }
    if in_bin {
        insert_fuzz_registration(&mut registrations, name?, path?)?;
    }
    (publish_disabled && cargo_fuzz && libfuzzer_dependency && !registrations.is_empty())
        .then_some(registrations)
}

fn insert_fuzz_registration(
    registrations: &mut BTreeMap<String, String>,
    name: String,
    path: String,
) -> Option<()> {
    if !valid_source_stem(&name)
        || !path.ends_with(".rs")
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || registrations.values().any(|registered| registered == &path)
        || registrations.insert(name, path).is_some()
    {
        return None;
    }
    Some(())
}

fn is_rust_source(relative: &str, bytes: &[u8]) -> bool {
    relative.ends_with(".rs") && std::str::from_utf8(bytes).is_ok()
}

fn is_executable_test_source(relative: &str, bytes: &[u8]) -> bool {
    integration_test_id(relative).is_some()
        && std::str::from_utf8(bytes).is_ok_and(|source| source.contains("#[test]"))
}

fn integration_test_id(relative: &str) -> Option<String> {
    let components: Vec<_> = relative.split('/').collect();
    if components.len() != 4
        || !matches!(components[0], "core" | "runtime" | "tools")
        || components[2] != "tests"
    {
        return None;
    }
    let test = components[3].strip_suffix(".rs")?;
    valid_source_stem(test).then(|| format!("{}/{}::{test}", components[0], components[1]))
}

fn is_workspace_implementation_path(relative: &str) -> bool {
    let components: Vec<_> = relative.split('/').collect();
    components.len() >= 4
        && matches!(components[0], "core" | "runtime" | "tools")
        && components[2] == "src"
        && relative.ends_with(".rs")
}

fn is_workspace_fuzz_target_path(relative: &str) -> bool {
    let components: Vec<_> = relative.split('/').collect();
    components.len() == 5
        && matches!(components[0], "core" | "runtime" | "tools")
        && components[2] == "fuzz"
        && components[3] == "fuzz_targets"
        && components[4]
            .strip_suffix(".rs")
            .is_some_and(valid_source_stem)
}

fn valid_source_stem(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn workspace_package_registered(repository_root: &Path, relative: &str) -> bool {
    let mut components = relative.split('/');
    let Some(group) = components.next() else {
        return false;
    };
    let Some(package) = components.next() else {
        return false;
    };
    if !matches!(group, "core" | "runtime" | "tools")
        || !valid_source_stem(package.replace('-', "_").as_str())
    {
        return false;
    }
    fs::symlink_metadata(repository_root.join(group).join(package).join("Cargo.toml"))
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn rust_source(subject: &VerifiedSubject) -> Option<&str> {
    subject
        .relative
        .ends_with(".rs")
        .then(|| std::str::from_utf8(&subject.bytes).ok())
        .flatten()
}

fn target_leaf(value: &str) -> Option<&str> {
    value
        .rsplit([':', '/'])
        .find(|part| !part.is_empty())
        .filter(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn valid_symbolic_target(value: &str) -> bool {
    !value.is_empty()
        && value.contains("::")
        && !value.contains(['#', '\\'])
        && !value.starts_with(['/', '.'])
        && !value.ends_with(['/', ':'])
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':')
        })
        && target_leaf(value).is_some()
}

fn registration_leaf(value: &str) -> Option<&str> {
    value
        .rsplit([':', '/', '.', '-'])
        .find(|part| !part.is_empty())
        .filter(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn case_manifest_binds(
    subject: &VerifiedSubject,
    oracle: &str,
    feature_id: &str,
    executed_tests: &[String],
) -> bool {
    if !subject.relative.ends_with("case.toml") {
        return false;
    }
    let Ok(input) = std::str::from_utf8(&subject.bytes) else {
        return false;
    };
    let Ok(manifest) = crate::manifest::validate_manifest(input) else {
        return false;
    };
    manifest.string("oracle", "level") == Some(oracle)
        && manifest.boolean("oracle", "reference_may_generate") == Some(false)
        && manifest
            .string_array("features", "ids")
            .is_some_and(|features| features.contains(&feature_id))
        && manifest
            .string_array("runners", "native")
            .is_some_and(|runners| {
                executed_tests
                    .iter()
                    .all(|test| runners.contains(&test.as_str()))
            })
        && case_reviewers(subject).is_some_and(|reviewers| {
            let reviewers: Vec<_> = reviewers.iter().map(String::as_str).collect();
            actual_reviewers(&reviewers)
        })
}

fn case_reviewers(subject: &VerifiedSubject) -> Option<Vec<String>> {
    let input = std::str::from_utf8(&subject.bytes).ok()?;
    let manifest = crate::manifest::validate_manifest(input).ok()?;
    manifest
        .string_array("oracle", "reviewers")
        .map(|reviewers| reviewers.into_iter().map(str::to_owned).collect())
}

fn holdout_cases_bind(
    subjects: &[VerifiedSubject],
    artifact: &EvidenceArtifact,
    feature_id: &str,
) -> bool {
    let case_ids = subjects.iter().find_map(|subject| {
        let input = std::str::from_utf8(&subject.bytes).ok()?;
        let values = parse_subject_report(input).ok()?;
        (string(values.get("evidence_kind")) == Some("holdout-manifest"))
            .then(|| owned_array(values.get("case_ids")))
    });
    let Some(case_ids) = case_ids else {
        return false;
    };
    if case_ids.len() < 2 || has_owned_duplicates(&case_ids) {
        return false;
    }
    case_ids.iter().all(|case_id| {
        let prefix = format!("tests/cases/{case_id}");
        let manifest_path = format!("{prefix}/case.toml");
        let input_path = format!("{prefix}/input.pdf");
        let expected_path = format!("{prefix}/expected/service.json");
        let Some(manifest_subject) = subjects
            .iter()
            .find(|subject| subject.relative == manifest_path)
        else {
            return false;
        };
        let Some(input_subject) = subjects
            .iter()
            .find(|subject| subject.relative == input_path)
        else {
            return false;
        };
        let Some(expected_subject) = subjects
            .iter()
            .find(|subject| subject.relative == expected_path)
        else {
            return false;
        };
        let Ok(manifest_input) = std::str::from_utf8(&manifest_subject.bytes) else {
            return false;
        };
        let Ok(manifest) = crate::manifest::validate_manifest(manifest_input) else {
            return false;
        };
        let input_hash = hex_digest(&sha256(&input_subject.bytes).expect("bounded subject hash"));
        let expected_hash =
            hex_digest(&sha256(&expected_subject.bytes).expect("bounded subject hash"));
        manifest.case_id() == case_id
            && manifest.source_sha256() == format!("sha256:{input_hash}")
            && manifest.string("expected", "service") == Some("expected/service.json")
            && manifest.string("expected", "service_sha256")
                == Some(format!("sha256:{expected_hash}").as_str())
            && manifest
                .string_array("features", "ids")
                .is_some_and(|features| features.contains(&feature_id))
            && manifest
                .string("oracle", "level")
                .is_some_and(|oracle| matches!(oracle, "O1" | "O2"))
            && manifest
                .string_array("oracle", "reviewers")
                .is_some_and(|reviewers| actual_reviewers(&reviewers))
            && manifest
                .string_array("runners", "native")
                .is_some_and(|runners| {
                    artifact
                        .executed_tests
                        .iter()
                        .all(|test| runners.contains(&test.as_str()))
                })
    })
}

fn adjudication_test_binds_report(
    subjects: &[VerifiedSubject],
    artifact: &EvidenceArtifact,
) -> bool {
    let report_values = subjects.iter().find_map(|subject| {
        let input = std::str::from_utf8(&subject.bytes).ok()?;
        let values = parse_subject_report(input).ok()?;
        (string(values.get("evidence_kind")) == Some("adjudication-report")).then_some(values)
    });
    let Some(report_values) = report_values else {
        return false;
    };
    let bound_values: Vec<_> = [
        "correction_commit",
        "pre_fix_output",
        "native_output",
        "reference_output",
    ]
    .iter()
    .filter_map(|field| string(report_values.get(*field)))
    .collect();
    bound_values.len() == 4
        && subjects.iter().any(|subject| {
            executable_test_binds(subject, &artifact.executed_tests)
                && rust_source(subject)
                    .is_some_and(|source| bound_values.iter().all(|value| source.contains(value)))
        })
}

fn run_environment_binds(subjects: &[VerifiedSubject]) -> bool {
    subjects.iter().any(|subject| {
        if !subject.relative.contains("run-environment") || !subject.relative.ends_with(".toml") {
            return false;
        }
        let Ok(input) = std::str::from_utf8(&subject.bytes) else {
            return false;
        };
        let Ok(lines) = logical_lines(input) else {
            return false;
        };
        let mut values = BTreeMap::new();
        for (_, line) in lines {
            let Some((key, value)) = split_assignment(&line) else {
                return false;
            };
            if values.insert(key.to_owned(), value.to_owned()).is_some() {
                return false;
            }
        }
        values.get("schema").map(String::as_str) == Some("1")
            && string(values.get("type")) == Some("m1-run-environment")
            && string(values.get("commit")).is_some_and(valid_commit_id)
            && string(values.get("cargo_profile")) == Some("release")
            && string(values.get("cargo_flags"))
                .is_some_and(|flags| flags.contains("--release") && flags.contains("--locked"))
            && [
                "rustc",
                "os",
                "cpu",
                "gpu",
                "browser",
                "commit_scope",
                "worktree_scope",
            ]
            .iter()
            .all(|field| string(values.get(*field)).is_some_and(|value| !value.is_empty()))
            && values
                .get("memory_bytes")
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|value| value > 0)
            && string(values.get("cargo_fuzz_version")) == Some("0.13.2")
            && boolean(values.get("external_observation")) == Some(false)
    })
}

#[derive(Clone, Copy)]
enum ReportRequirement {
    IndependentReview,
    Adjudication,
    Minimizer,
    Holdout,
    Benchmark,
    Differential,
    Fingerprint,
}

#[allow(clippy::too_many_arguments)]
fn report_binds(
    subject: &VerifiedSubject,
    expected_kind: &str,
    feature_id: &str,
    target: &str,
    artifact: &EvidenceArtifact,
    profile_fuzz_targets: &[String],
    feature_benchmarks: &[String],
    requirement: ReportRequirement,
) -> bool {
    if !subject.relative.ends_with(".toml") {
        return false;
    }
    let Ok(input) = std::str::from_utf8(&subject.bytes) else {
        return false;
    };
    let Ok(values) = parse_subject_report(input) else {
        return false;
    };
    let executed_tests = owned_array(values.get("executed_tests"));
    let common = values.get("schema").map(String::as_str) == Some("1")
        && string(values.get("type")) == Some("maturity-subject-report")
        && string(values.get("id")).is_some_and(valid_id)
        && string(values.get("evidence_kind")) == Some(expected_kind)
        && string(values.get("feature")) == Some(feature_id)
        && string(values.get("target")) == Some(target)
        && !has_owned_duplicates(&executed_tests)
        && !executed_tests.is_empty()
        && as_set(&executed_tests) == as_set(&artifact.executed_tests)
        && string(values.get("verdict")) == Some("pass");
    if !common {
        return false;
    }
    match requirement {
        ReportRequirement::IndependentReview => {
            boolean(values.get("independent")) == Some(true)
                && actual_reviewer_array(values.get("reviewers"))
        }
        ReportRequirement::Adjudication => {
            string_array(values.get("reviewers"))
                .is_some_and(|reviewers| reviewers.len() >= 2 && actual_reviewers(&reviewers))
                && nonempty_unique_array(values.get("case_ids"))
                && string(values.get("correction_commit")).is_some_and(valid_commit_id)
                && ["pre_fix_output", "native_output", "reference_output"]
                    .iter()
                    .all(|field| string(values.get(*field)).is_some_and(valid_prefixed_digest))
                && string(values.get("pre_fix_output")) != string(values.get("native_output"))
                && string(values.get("pre_fix_output")) != string(values.get("reference_output"))
        }
        ReportRequirement::Minimizer => {
            let fuzz_targets = owned_array(values.get("fuzz_targets"));
            !fuzz_targets.is_empty()
                && !has_owned_duplicates(&fuzz_targets)
                && as_set(&fuzz_targets) == as_set(profile_fuzz_targets)
                && fuzz_seed_case_ids_bind(&values, artifact)
                && string(values.get("minimizer")) == Some("cargo-fuzz-cmin")
                && string(values.get("dictionary")).is_some_and(|dictionary| {
                    parse_content_reference(dictionary).is_ok()
                        && artifact
                            .subjects
                            .iter()
                            .any(|subject| subject == dictionary)
                })
                && string(values.get("crash_minimization"))
                    == Some("not-applicable:no-crash-observed")
                && string(values.get("owner")).is_some_and(|owner| !owner.is_empty())
                && string(values.get("invariant")).is_some_and(|invariant| !invariant.is_empty())
                && string(values.get("cargo_fuzz_version")) == Some("0.13.2")
        }
        ReportRequirement::Holdout => string_array(values.get("case_ids"))
            .is_some_and(|cases| cases.len() >= 2 && !has_duplicates(&cases)),
        ReportRequirement::Benchmark => {
            let benchmarks = owned_array(values.get("benchmarks"));
            let samples = parse_u64_array(
                values
                    .get("raw_samples_ns")
                    .map(String::as_str)
                    .unwrap_or(""),
            );
            !benchmarks.is_empty()
                && !has_owned_duplicates(&benchmarks)
                && as_set(&benchmarks) == as_set(feature_benchmarks)
                && samples.as_ref().is_some_and(|samples| {
                    samples.len() == 21
                        && samples.iter().all(|sample| *sample > 0)
                        && benchmark_statistics_bind(&values, samples)
                })
                && boolean(values.get("performance_eligible")) == Some(true)
                && boolean(values.get("release_gate_eligible")) == Some(false)
                && boolean(values.get("external_comparison")) == Some(false)
                && boolean(values.get("regression_threshold_eligible")) == Some(false)
                && boolean(values.get("reference_checked")) == Some(true)
                && string(values.get("commit")).is_some_and(valid_commit_id)
                && string(values.get("cargo_profile")) == Some("release")
                && string(values.get("cargo_flags"))
                    .is_some_and(|flags| flags.contains("--release") && flags.contains("--locked"))
                && [
                    "rustc",
                    "os",
                    "cpu",
                    "gpu",
                    "browser",
                    "renderer_epoch",
                    "font_epoch",
                    "color_epoch",
                    "timing_scope",
                    "memory_scope",
                    "support_scope",
                    "cache_policy",
                    "commit_scope",
                    "worktree_scope",
                ]
                .iter()
                .all(|field| string(values.get(*field)).is_some_and(|value| !value.is_empty()))
                && values
                    .get("memory_bytes")
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|value| value > 0)
                && string(values.get("corpus_sha256")).is_some_and(valid_prefixed_digest)
                && string(values.get("corpus_sha256")).is_some_and(|corpus_hash| {
                    artifact.subjects.iter().any(|subject| {
                        parse_content_reference(subject).is_ok_and(|reference| {
                            reference.relative.ends_with("/input.pdf")
                                && corpus_hash == format!("sha256:{}", reference.digest)
                        })
                    })
                })
        }
        ReportRequirement::Differential => {
            boolean(values.get("full_session")) == Some(true)
                && nonempty_unique_array(values.get("case_ids"))
        }
        ReportRequirement::Fingerprint => reference_fingerprint_binds(&values, artifact),
    }
}

fn fuzz_seed_case_ids_bind(values: &BTreeMap<String, String>, artifact: &EvidenceArtifact) -> bool {
    let case_ids = owned_array(values.get("case_ids"));
    let expected_ids: BTreeSet<_> = ["minimal.pdf", "truncated-header.pdf", "nested-outline.pdf"]
        .iter()
        .map(|seed| format!("fuzz.m1-document-services.{seed}"))
        .collect();
    let subject_ids: BTreeSet<_> = artifact
        .subjects
        .iter()
        .filter_map(|subject| parse_content_reference(subject).ok())
        .filter_map(|reference| {
            reference
                .relative
                .strip_prefix("tools/quality/fuzz/corpus/m1_document_services/")
                .map(|seed| format!("fuzz.m1-document-services.{seed}"))
        })
        .collect();
    !has_owned_duplicates(&case_ids)
        && as_set(&case_ids) == expected_ids.iter().map(String::as_str).collect()
        && subject_ids == expected_ids
}

fn parse_subject_report(input: &str) -> Result<BTreeMap<String, String>, ()> {
    let allowed: BTreeSet<_> = SUBJECT_REPORT_KEYS.iter().copied().collect();
    let mut values = BTreeMap::new();
    for (_, line) in logical_lines(input).map_err(|_| ())? {
        let (key, value) = split_assignment(&line).ok_or(())?;
        let valid = if matches!(key, "raw_samples_ns" | "median_ci95_ns") {
            parse_u64_array(value).is_some()
        } else {
            valid_value(value)
        };
        if !allowed.contains(key)
            || !valid
            || values.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return Err(());
        }
    }
    Ok(values)
}

fn parse_u64_array(value: &str) -> Option<Vec<u64>> {
    let body = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    let body = body.strip_suffix(',').unwrap_or(body).trim_end();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',')
        .map(|item| item.trim().parse::<u64>().ok())
        .collect()
}

fn nonempty_unique_array(value: Option<&String>) -> bool {
    string_array(value).is_some_and(|values| !values.is_empty() && !has_duplicates(&values))
}

fn actual_reviewer_array(value: Option<&String>) -> bool {
    string_array(value).is_some_and(|reviewers| actual_reviewers(&reviewers))
}

fn actual_reviewers(reviewers: &[&str]) -> bool {
    !reviewers.is_empty()
        && !has_duplicates(reviewers)
        && reviewers.iter().all(|reviewer| {
            let normalized = reviewer.to_ascii_lowercase();
            !["pending", "required", "todo", "tbd"]
                .iter()
                .any(|placeholder| normalized.contains(placeholder))
        })
}

fn valid_prefixed_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn valid_commit_id(value: &str) -> bool {
    (7..=40).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn benchmark_statistics_bind(values: &BTreeMap<String, String>, samples: &[u64]) -> bool {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let percentile = |percent: usize| {
        let rank = percent
            .checked_mul(sorted.len())
            .and_then(|value| value.checked_add(99))
            .map(|value| value / 100)
            .unwrap_or(0)
            .max(1);
        sorted[rank - 1]
    };
    let median = sorted[sorted.len() / 2];
    let ci = values
        .get("median_ci95_ns")
        .and_then(|value| parse_u64_array(value));
    values
        .get("sample_count")
        .and_then(|value| value.parse::<usize>().ok())
        == Some(samples.len())
        && values
            .get("median_ns")
            .and_then(|value| value.parse::<u64>().ok())
            == Some(median)
        && values
            .get("p95_ns")
            .and_then(|value| value.parse::<u64>().ok())
            == Some(percentile(95))
        && values
            .get("p99_ns")
            .and_then(|value| value.parse::<u64>().ok())
            == Some(percentile(99))
        && ci.is_some_and(|ci| ci == [sorted[5], sorted[15]])
}

fn reference_fingerprint_binds(
    values: &BTreeMap<String, String>,
    artifact: &EvidenceArtifact,
) -> bool {
    let components = owned_array(values.get("fingerprint_components"));
    if components.len() < 5
        || has_owned_duplicates(&components)
        || components.iter().any(|component| {
            parse_content_reference(component).is_err() || !artifact.subjects.contains(component)
        })
    {
        return false;
    }
    let paths: Vec<_> = components
        .iter()
        .filter_map(|component| parse_content_reference(component).ok())
        .map(|component| component.relative)
        .collect();
    if !paths.iter().any(|path| path.ends_with("/reference.rs"))
        || !paths.iter().any(|path| path.ends_with("/case.toml"))
        || !paths.iter().any(|path| path.ends_with("/input.pdf"))
        || !paths
            .iter()
            .any(|path| path.contains("/expected/") && path.ends_with(".json"))
        || !paths.iter().any(|path| path.contains("run-environment"))
    {
        return false;
    }
    let mut canonical = components;
    canonical.sort();
    let canonical = canonical.join("\n") + "\n";
    let digest = hex_digest(&sha256(canonical.as_bytes()).expect("bounded fingerprint framing"));
    string(values.get("fingerprint")) == Some(format!("sha256:{digest}").as_str())
}

fn derive_repository_root(path: &Path) -> Result<PathBuf, MaturityDiagnostic> {
    if path.file_name().and_then(|value| value.to_str()) != Some("capability-profiles.toml") {
        return Err(MaturityDiagnostic::root(
            "RPE-MATURITY-0012",
            "capability-profiles.toml",
        ));
    }
    let traceability = path
        .parent()
        .ok_or_else(|| MaturityDiagnostic::root("RPE-MATURITY-0012", "docs/traceability"))?;
    if traceability.file_name().and_then(|value| value.to_str()) != Some("traceability") {
        return Err(MaturityDiagnostic::root(
            "RPE-MATURITY-0012",
            "docs/traceability",
        ));
    }
    let docs = traceability
        .parent()
        .ok_or_else(|| MaturityDiagnostic::root("RPE-MATURITY-0012", "docs"))?;
    if docs.file_name().and_then(|value| value.to_str()) != Some("docs") {
        return Err(MaturityDiagnostic::root(
            "RPE-MATURITY-0012",
            "docs/traceability",
        ));
    }
    let root = docs
        .parent()
        .ok_or_else(|| MaturityDiagnostic::root("RPE-MATURITY-0012", "repository-root"))?;
    if root.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(root.to_path_buf())
    }
}

fn load_repository_maps(repository_root: &Path) -> Result<RepositoryMaps, Vec<MaturityDiagnostic>> {
    let feature_records = load_records(
        repository_root,
        FEATURE_MAP_PATH,
        "[[feature]]",
        FEATURE_KEYS,
    )?;
    let requirement_records = load_records(
        repository_root,
        SPEC_MAP_PATH,
        "[[requirement]]",
        REQUIREMENT_KEYS,
    )?;
    let ledger_records = load_records(repository_root, DATA_LEDGER_PATH, "[[data]]", DATA_KEYS)?;
    let mut diagnostics = Vec::new();
    validate_companion_root(&feature_records.root, FEATURE_MAP_PATH, &mut diagnostics);
    validate_companion_root(&requirement_records.root, SPEC_MAP_PATH, &mut diagnostics);
    validate_companion_root(&ledger_records.root, DATA_LEDGER_PATH, &mut diagnostics);
    validate_ledger_records(&ledger_records.records, &mut diagnostics);
    let features = index_records(feature_records.records, FEATURE_MAP_PATH, &mut diagnostics);
    let requirements = index_records(requirement_records.records, SPEC_MAP_PATH, &mut diagnostics);
    if diagnostics.is_empty() {
        Ok(RepositoryMaps {
            features,
            requirements,
            ledger: ledger_records.records,
        })
    } else {
        Err(diagnostics)
    }
}

fn validate_ledger_records(
    records: &[BTreeMap<String, String>],
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    let mut identities = BTreeSet::new();
    for record in records {
        let Some(identity) = string(record.get("id")) else {
            diagnostics.push(MaturityDiagnostic::root(
                "RPE-MATURITY-0018",
                DATA_LEDGER_PATH,
            ));
            continue;
        };
        if !valid_id(identity) || !identities.insert(identity) {
            diagnostics.push(MaturityDiagnostic::root(
                "RPE-MATURITY-0018",
                DATA_LEDGER_PATH,
            ));
        }
    }
}

fn load_records(
    repository_root: &Path,
    relative: &str,
    header: &str,
    allowed_record: &[&str],
) -> Result<ParsedRecords, Vec<MaturityDiagnostic>> {
    let input = fs::read_to_string(repository_root.join(relative))
        .map_err(|_| vec![MaturityDiagnostic::root("RPE-MATURITY-0012", relative)])?;
    parse_records(&input, header, allowed_record).map_err(|line| {
        vec![MaturityDiagnostic::root(
            "RPE-MATURITY-0012",
            format!("{relative}:line={line}"),
        )]
    })
}

fn validate_companion_root(
    root: &BTreeMap<String, String>,
    relative: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) {
    if root.get("schema").map(String::as_str) != Some("1")
        || root.get("status").and_then(|value| unquote(value)) != Some("active")
        || string(root.get("version")).is_none_or(str::is_empty)
    {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0012", relative));
    }
}

fn index_records(
    records: Vec<BTreeMap<String, String>>,
    relative: &str,
    diagnostics: &mut Vec<MaturityDiagnostic>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut indexed = BTreeMap::new();
    for record in records {
        let Some(identity) = string(record.get("id")).map(str::to_owned) else {
            diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0012", relative));
            continue;
        };
        if indexed.insert(identity, record).is_some() {
            diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0012", relative));
        }
    }
    indexed
}

fn parse_profiles(input: &str) -> Result<ParsedProfiles, Vec<MaturityDiagnostic>> {
    let parsed = parse_records(input, "[[profile]]", PROFILE_KEYS)
        .map_err(|line| vec![MaturityDiagnostic::syntax(line)])?;
    Ok(ParsedProfiles {
        root: parsed.root,
        profiles: parsed.records,
    })
}

fn parse_records(
    input: &str,
    record_header: &str,
    allowed_record: &[&str],
) -> Result<ParsedRecords, usize> {
    let allowed_root: BTreeSet<&str> = ROOT_KEYS.iter().copied().collect();
    let allowed_record: BTreeSet<&str> = allowed_record.iter().copied().collect();
    let mut root = BTreeMap::new();
    let mut records: Vec<BTreeMap<String, String>> = Vec::new();
    let mut in_record = false;

    for (line_number, line) in logical_lines(input)? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == record_header {
            records.push(BTreeMap::new());
            in_record = true;
            continue;
        }
        let (key, value) = split_assignment(line).ok_or(line_number)?;
        if value.is_empty() || !valid_companion_value(value) {
            return Err(line_number);
        }
        let (allowed, destination) = if in_record {
            (
                &allowed_record,
                records.last_mut().expect("a record was pushed"),
            )
        } else {
            (&allowed_root, &mut root)
        };
        if !allowed.contains(key) || destination.insert(key.into(), value.into()).is_some() {
            return Err(line_number);
        }
    }
    Ok(ParsedRecords { root, records })
}

fn parse_flat(input: &str, allowed: &[&str]) -> Result<BTreeMap<String, String>, ()> {
    let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
    let mut values = BTreeMap::new();
    for (_, line) in logical_lines(input).map_err(|_| ())? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && !line.starts_with("[]") {
            return Err(());
        }
        let (key, value) = split_assignment(line).ok_or(())?;
        if !allowed.contains(key)
            || !valid_value(value)
            || values.insert(key.into(), value.into()).is_some()
        {
            return Err(());
        }
    }
    Ok(values)
}

fn logical_lines(input: &str) -> Result<Vec<(usize, String)>, usize> {
    let mut lines = Vec::new();
    let mut pending: Option<(usize, String)> = None;

    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        let fragment = strip_comment(raw_line).ok_or(line_number)?.trim();
        if fragment.is_empty() {
            continue;
        }
        if let Some((start, value)) = &mut pending {
            value.push(' ');
            value.push_str(fragment);
            if array_value_is_complete(value).ok_or(*start)? {
                let (start, value) = pending.take().expect("pending value exists");
                lines.push((start, value));
            }
            continue;
        }
        if let Some((_, value)) = split_assignment(fragment)
            && value.starts_with('[')
            && !array_value_is_complete(value).ok_or(line_number)?
        {
            pending = Some((line_number, fragment.to_owned()));
        } else {
            lines.push((line_number, fragment.to_owned()));
        }
    }

    if let Some((line, _)) = pending {
        Err(line)
    } else {
        Ok(lines)
    }
}

fn array_value_is_complete(value: &str) -> Option<bool> {
    let mut quoted = false;
    let mut depth = 0_u32;
    for byte in value.bytes() {
        match byte {
            b'"' => quoted = !quoted,
            b'[' if !quoted => depth = depth.checked_add(1)?,
            b']' if !quoted => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    (!quoted).then_some(depth == 0)
}

fn strip_comment(line: &str) -> Option<&str> {
    let mut quoted = false;
    for (index, byte) in line.bytes().enumerate() {
        if byte == b'"' {
            quoted = !quoted;
        } else if byte == b'#' && !quoted {
            return Some(&line[..index]);
        }
    }
    (!quoted).then_some(line)
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit())
    {
        return None;
    }
    Some((key, value))
}

fn valid_value(value: &str) -> bool {
    value.parse::<u64>().is_ok()
        || matches!(value, "true" | "false")
        || unquote(value).is_some()
        || parse_string_array(value).is_some()
}

fn valid_companion_value(value: &str) -> bool {
    valid_value(value) || valid_local_date(value)
}

fn valid_local_date(value: &str) -> bool {
    value.len() == 10
        && value.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn string(value: Option<&String>) -> Option<&str> {
    value.and_then(|value| unquote(value))
}

fn boolean(value: Option<&String>) -> Option<bool> {
    match value.map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

fn string_array(value: Option<&String>) -> Option<Vec<&str>> {
    parse_string_array(value?)
}

fn owned_array(value: Option<&String>) -> Vec<String> {
    string_array(value)
        .unwrap_or_default()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn unquote(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.contains(['"', '\n', '\r']))
}

fn parse_string_array(value: &str) -> Option<Vec<&str>> {
    let body = value.strip_prefix('[')?.strip_suffix(']')?.trim();
    let body = body.strip_suffix(',').unwrap_or(body).trim_end();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',').map(|item| unquote(item.trim())).collect()
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn is_placeholder(value: &str) -> bool {
    value.is_empty() || value.contains("REQUIRED") || value.to_ascii_lowercase().contains("pending")
}

fn profile_identity(profile: &BTreeMap<String, String>, index: usize) -> String {
    profile
        .get("id")
        .and_then(|value| unquote(value))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("<profile-{index}>"))
}

fn has_duplicates(values: &[&str]) -> bool {
    let mut unique = BTreeSet::new();
    values.iter().any(|value| !unique.insert(*value))
}

fn has_owned_duplicates(values: &[String]) -> bool {
    let mut unique = BTreeSet::new();
    values.iter().any(|value| !unique.insert(value.as_str()))
}

fn as_set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        VerifiedSubject, benchmark_test_binds, derive_repository_root, fuzz_build_test_binds,
        parse_content_reference, parse_fuzz_registrations, subject_kind_for_role,
        valid_symbolic_target, validate_maturity, validate_maturity_file,
    };
    use pdf_rs_digest::{hex_digest, sha256};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Mutation {
        None,
        MissingPath,
        NonRegularPath,
        HashMismatch,
        OracleO4,
        Ineligible,
        Unregistered,
        NonGating,
        ExternalObservation,
        ArtifactSchema,
        CrossReference,
        FeatureState,
        SpecFeature,
        LedgerHash,
        LedgerId,
        DuplicateLedgerId,
        DuplicateReference,
        FuzzRegistration,
        EmptySubjects,
        SubjectHash,
        WrongSubjectKind,
        UnregisteredTest,
        FuzzCargoRegistration,
        HoldoutCaseOverlap,
        HoldoutFuzzDigestOverlap,
        PlaceholderReviewer,
        CommitAnchorMismatch,
    }

    struct TestRepository {
        root: PathBuf,
        profile_path: PathBuf,
    }

    impl TestRepository {
        fn differential(mutation: Mutation) -> Self {
            let root = std::env::temp_dir().join(format!(
                "pdf-rs-quality-maturity-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let traceability = root.join("docs/traceability");
            let evidence = root.join("evidence");
            fs::create_dir_all(&traceability).unwrap();
            fs::create_dir_all(&evidence).unwrap();
            for package in ["core/test", "tools/quality"] {
                fs::create_dir_all(root.join(package)).unwrap();
                fs::write(
                    root.join(package).join("Cargo.toml"),
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
                )
                .unwrap();
            }

            let mut ledger = Vec::new();
            let reference = emit_artifact(
                &root,
                &mut ledger,
                ArtifactSpec {
                    name: "reference",
                    role: "reference-implementation",
                    oracle: if mutation == Mutation::OracleO4 {
                        "O4"
                    } else {
                        "O1"
                    },
                    eligibility: mutation != Mutation::Ineligible,
                    registered: mutation != Mutation::Unregistered,
                    gating: mutation != Mutation::NonGating,
                    external_observation: mutation == Mutation::ExternalObservation,
                    schema: if mutation == Mutation::ArtifactSchema {
                        2
                    } else {
                        1
                    },
                    subject_kind: if mutation == Mutation::WrongSubjectKind {
                        Some("analytic-case")
                    } else {
                        None
                    },
                    empty_subjects: mutation == Mutation::EmptySubjects,
                    subject_hash_mismatch: mutation == Mutation::SubjectHash,
                    executed_test: if mutation == Mutation::UnregisteredTest {
                        "tools/quality::unregistered_test"
                    } else {
                        "tools/quality::registered_test"
                    },
                    fuzz_registration_drift: false,
                    holdout_case_overlap: false,
                    holdout_fuzz_overlap: false,
                    placeholder_reviewer: false,
                    commit_anchor_mismatch: false,
                    cross_references: &[],
                    fuzz_targets: &[],
                    benchmarks: &[],
                },
            );
            let o0 = emit_artifact(
                &root,
                &mut ledger,
                ArtifactSpec::new("o0", "o0-case", "O0", &[]),
            );
            let o1 = emit_artifact(
                &root,
                &mut ledger,
                ArtifactSpec::new("o1", "o1-case", "O1", &[]),
            );
            let review_cross = if mutation == Mutation::CrossReference {
                vec![reference.clone(), o0.clone()]
            } else {
                vec![reference.clone(), o0.clone(), o1.clone()]
            };
            let mut review_spec =
                ArtifactSpec::new("review", "independent-review", "INTERNAL", &review_cross);
            review_spec.placeholder_reviewer = mutation == Mutation::PlaceholderReviewer;
            let review = emit_artifact(&root, &mut ledger, review_spec);
            let o2_cross = vec![reference.clone(), o0.clone(), o1.clone()];
            let o2 = emit_artifact(
                &root,
                &mut ledger,
                ArtifactSpec::new("o2", "o2-adjudication", "O2", &o2_cross),
            );
            let minimizer_cross = vec![o2.clone()];
            let mut minimizer_spec =
                ArtifactSpec::new("minimizer", "fuzz-minimizer", "INTERNAL", &minimizer_cross);
            minimizer_spec.fuzz_targets = &["fuzz.m1"];
            minimizer_spec.executed_test = "tools/quality::registered_fuzz_test";
            minimizer_spec.fuzz_registration_drift = mutation == Mutation::FuzzCargoRegistration;
            let minimizer = emit_artifact(&root, &mut ledger, minimizer_spec);
            let holdout_cross = vec![o0.clone(), o1.clone()];
            let mut holdout_spec =
                ArtifactSpec::new("holdout", "holdout-manifest", "INTERNAL", &holdout_cross);
            holdout_spec.holdout_case_overlap = mutation == Mutation::HoldoutCaseOverlap;
            holdout_spec.holdout_fuzz_overlap = mutation == Mutation::HoldoutFuzzDigestOverlap;
            holdout_spec.placeholder_reviewer = mutation == Mutation::PlaceholderReviewer;
            let holdout = emit_artifact(&root, &mut ledger, holdout_spec);
            let benchmark_cross = vec![reference.clone()];
            let mut benchmark_spec = ArtifactSpec::new(
                "benchmark",
                "benchmark-report",
                "INTERNAL",
                &benchmark_cross,
            );
            benchmark_spec.benchmarks = &["bench.m1"];
            benchmark_spec.executed_test = "tools/quality::registered_benchmark_test";
            let benchmark = emit_artifact(&root, &mut ledger, benchmark_spec);
            let differential_cross = vec![
                reference.clone(),
                o2.clone(),
                minimizer.clone(),
                holdout.clone(),
                benchmark.clone(),
            ];
            let differential = emit_artifact(
                &root,
                &mut ledger,
                ArtifactSpec::new(
                    "differential",
                    "differential-report",
                    "O2",
                    &differential_cross,
                ),
            );
            let fingerprint_cross = vec![reference.clone(), differential.clone()];
            let mut fingerprint_spec = ArtifactSpec::new(
                "fingerprint",
                "reference-fingerprint",
                "O1",
                &fingerprint_cross,
            );
            fingerprint_spec.commit_anchor_mismatch = mutation == Mutation::CommitAnchorMismatch;
            let fingerprint = emit_artifact(&root, &mut ledger, fingerprint_spec);

            if mutation == Mutation::LedgerHash {
                ledger[0].digest = "0".repeat(64);
            } else if mutation == Mutation::LedgerId {
                ledger[0].name = "evidence.other".to_owned();
            } else if mutation == Mutation::DuplicateLedgerId {
                ledger[1].name = ledger[0].name.clone();
            }
            let feature_state = if mutation == Mutation::FeatureState {
                "REFERENCE"
            } else {
                "DIFFERENTIAL"
            };
            let feature_fuzz = if mutation == Mutation::FuzzRegistration {
                "fuzz.other"
            } else {
                "fuzz.m1"
            };
            fs::write(
                traceability.join("feature-map.toml"),
                format!(
                    "schema = 1\nversion = \"test\"\nstatus = \"active\"\n\n[[feature]]\nid = \"core.test\"\nstate = \"{feature_state}\"\nprofile = \"m1.test.v1\"\ntests = [\"tools/quality::registered_test\", \"tools/quality::registered_fuzz_test\", \"tools/quality::registered_benchmark_test\"]\nfuzz_targets = [\"{feature_fuzz}\"]\nbenchmarks = [\"bench.m1\"]\n"
                ),
            )
            .unwrap();
            let spec_feature = if mutation == Mutation::SpecFeature {
                "core.other"
            } else {
                "core.test"
            };
            fs::write(
                traceability.join("spec-map.toml"),
                format!(
                    "schema = 1\nversion = \"test\"\nstatus = \"active\"\n\n[[requirement]]\nid = \"RPE-TEST/1\"\nfeatures = [\"{spec_feature}\"]\n"
                ),
            )
            .unwrap();
            fs::write(
                traceability.join("data-ledger.toml"),
                ledger_document(&ledger),
            )
            .unwrap();

            let profile_reference = match mutation {
                Mutation::MissingPath => with_path(&reference, "evidence/missing.toml"),
                Mutation::NonRegularPath => with_path(&reference, "evidence"),
                Mutation::HashMismatch => with_digest(&reference, &"0".repeat(64)),
                _ => reference.clone(),
            };
            let profile_o1 = if mutation == Mutation::DuplicateReference {
                o0.clone()
            } else {
                o1.clone()
            };
            let profile_path = traceability.join("capability-profiles.toml");
            fs::write(
                &profile_path,
                format!(
                    "schema = 1\nversion = \"test\"\nstatus = \"active\"\n\n[[profile]]\nid = \"m1.test.v1\"\nowner = \"quality-corpus\"\nstate = \"DIFFERENTIAL\"\nfeature = \"core.test\"\nrequirements = [\"RPE-TEST/1\"]\nsupported = [\"strict test\"]\nexcluded = [\"other behavior\"]\npolicy = \"Only registered evidence may promote this fixture.\"\ntarget = \"core/test::run\"\nreference = \"{profile_reference}\"\no0_cases = [\"{o0}\"]\no1_cases = [\"{profile_o1}\"]\no2_adjudications = [\"{o2}\"]\nindependent_review = \"{review}\"\nfuzz_targets = [\"fuzz.m1\"]\nfuzz_minimizer = \"{minimizer}\"\nholdout_manifest = \"{holdout}\"\nbenchmark_report = \"{benchmark}\"\ndifferential_report = \"{differential}\"\nbaseline_fingerprint = \"{fingerprint}\"\n"
                ),
            )
            .unwrap();

            Self { root, profile_path }
        }

        fn diagnostics(&self) -> Vec<super::MaturityDiagnostic> {
            validate_maturity_file(&self.profile_path).unwrap_err()
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct ArtifactSpec<'a> {
        name: &'a str,
        role: &'a str,
        oracle: &'a str,
        eligibility: bool,
        registered: bool,
        gating: bool,
        external_observation: bool,
        schema: u8,
        subject_kind: Option<&'a str>,
        empty_subjects: bool,
        subject_hash_mismatch: bool,
        executed_test: &'a str,
        fuzz_registration_drift: bool,
        holdout_case_overlap: bool,
        holdout_fuzz_overlap: bool,
        placeholder_reviewer: bool,
        commit_anchor_mismatch: bool,
        cross_references: &'a [String],
        fuzz_targets: &'a [&'a str],
        benchmarks: &'a [&'a str],
    }

    impl<'a> ArtifactSpec<'a> {
        fn new(
            name: &'a str,
            role: &'a str,
            oracle: &'a str,
            cross_references: &'a [String],
        ) -> Self {
            Self {
                name,
                role,
                oracle,
                eligibility: true,
                registered: true,
                gating: true,
                external_observation: false,
                schema: 1,
                subject_kind: None,
                empty_subjects: false,
                subject_hash_mismatch: false,
                executed_test: "tools/quality::registered_test",
                fuzz_registration_drift: false,
                holdout_case_overlap: false,
                holdout_fuzz_overlap: false,
                placeholder_reviewer: false,
                commit_anchor_mismatch: false,
                cross_references,
                fuzz_targets: &[],
                benchmarks: &[],
            }
        }
    }

    struct LedgerEntry {
        name: String,
        path: String,
        digest: String,
    }

    fn emit_artifact(root: &Path, ledger: &mut Vec<LedgerEntry>, spec: ArtifactSpec<'_>) -> String {
        let path = format!("evidence/{}.toml", spec.name);
        let mut subjects = emit_subjects(root, &spec);
        if spec.empty_subjects {
            subjects.clear();
        } else if spec.subject_hash_mismatch {
            subjects[0] = with_digest(&subjects[0], &"0".repeat(64));
        }
        let subjects = quoted_array(subjects.iter().map(String::as_str));
        let executed_tests = quoted_array(std::iter::once(spec.executed_test));
        let cross_references = quoted_array(spec.cross_references.iter().map(String::as_str));
        let fuzz_targets = quoted_array(spec.fuzz_targets.iter().copied());
        let benchmarks = quoted_array(spec.benchmarks.iter().copied());
        let contents = format!(
            "schema = {}\ntype = \"maturity-evidence\"\nid = \"evidence.{}\"\nprofile = \"m1.test.v1\"\nfeature = \"core.test\"\nrole = \"{}\"\noracle = \"{}\"\neligibility = {}\nregistered = {}\ngating = {}\nexternal_observation = {}\ntarget = \"core/test::run\"\nrequirements = [\"RPE-TEST/1\"]\nsubject_kind = \"{}\"\nsubjects = {}\nexecuted_tests = {}\ncross_references = {}\nfuzz_targets = {}\nbenchmarks = {}\nverdict = \"pass\"\n",
            spec.schema,
            spec.name,
            spec.role,
            spec.oracle,
            spec.eligibility,
            spec.registered,
            spec.gating,
            spec.external_observation,
            spec.subject_kind
                .unwrap_or_else(|| subject_kind_for_role(spec.role)),
            subjects,
            executed_tests,
            cross_references,
            fuzz_targets,
            benchmarks,
        );
        fs::write(root.join(&path), &contents).unwrap();
        let digest = hex_digest(&sha256(contents.as_bytes()).unwrap());
        ledger.push(LedgerEntry {
            name: spec.name.to_owned(),
            path: path.clone(),
            digest: digest.clone(),
        });
        format!("{path}#sha256:{digest}")
    }

    fn emit_subjects(root: &Path, spec: &ArtifactSpec<'_>) -> Vec<String> {
        let test_name = spec
            .executed_test
            .rsplit("::")
            .next()
            .expect("test identity has a final component");
        let test_source = if spec.role == "fuzz-minimizer" {
            format!(
                "const FUZZ_SEEDS: [&str; 3] = [\"minimal.pdf\", \"truncated-header.pdf\", \"nested-outline.pdf\"];\n#[test]\nfn {test_name}() {{\n    for seed in FUZZ_SEEDS {{ let _ = seed; }}\n    for seed in FUZZ_SEEDS {{ let _ = seed; }}\n    let registered_outline = \"m1-services/nested-valid/input.pdf\";\n    let ci_workflow = \".github/workflows/ci.yml cargo install --locked --version 0.13.2 cargo-fuzz ./scripts/ci.sh pr\";\n    let artifact_prefixes = [\"-artifact_prefix=/tmp/run/\", \"-artifact_prefix=/tmp/cmin/\"];\n    let cmin_artifact_directory = \"pdf-rs-m1-fuzz-cmin-artifacts\";\n    let check = std::process::Command::new(\"cargo\")\n        .arg(\"check\").arg(\"--locked\").arg(\"--manifest-path\")\n        .arg(\"tools/quality/fuzz/Cargo.toml\").status().unwrap();\n    assert!(check.success());\n    let version = std::process::Command::new(\"cargo\").arg(\"fuzz\").arg(\"--version\").output().unwrap();\n    assert_eq!(String::from_utf8(version.stdout).unwrap().trim(), \"cargo-fuzz 0.13.2\");\n    for mode in [\"run\", \"cmin\"] {{\n        let mut command = std::process::Command::new(\"cargo\");\n        command.arg(\"fuzz\").arg(mode).arg(\"--fuzz-dir\").arg(\"tools/quality/fuzz\")\n            .arg(\"--sanitizer\").arg(\"none\").arg(\"m1\").arg(\"corpus\").arg(\"--\")\n            .arg(\"-seed=424242\").arg(\"-runs=64\").arg(\"-max_len=1048576\")\n            .arg(\"-timeout=1\").arg(\"-rss_limit_mb=512\").arg(\"-dict=pdf.dict\");\n        assert_eq!(command.get_args().filter(|arg| *arg == \"corpus\").count(), 1, \"exactly one corpus positional argument\");\n        assert!(command.status().unwrap().success());\n    }}\n    let _ = (registered_outline, ci_workflow, artifact_prefixes, cmin_artifact_directory);\n}}\n"
            )
        } else if spec.role == "benchmark-report" {
            format!(
                "#[test]\nfn {test_name}() {{\n    let expected = reference_result(input, contract);\n    let warmup = run_native_bytes(1, input, contract).expect(\"benchmark warmup\");\n    assert_eq!(warmup, expected);\n    black_box(warmup);\n    let mut samples = Vec::with_capacity(21);\n    for _ in 0..21 {{\n        let start = Instant::now();\n        let result = run_native_bytes(1, input, contract).unwrap();\n        let elapsed = start.elapsed().as_nanos();\n        assert_eq!(result, expected, \"every timed result matches reference\");\n        black_box(result);\n        samples.push(elapsed);\n    }}\n}}\n"
            )
        } else {
            format!(
                "#[test]\nfn {test_name}() {{\n    let frozen_pre_fix_projection = \"sha256:{}\";\n    let native_output = \"sha256:{}\";\n    let reference_output = \"sha256:{}\";\n    let correction_commit = \"a5ec35b\";\n    let adjudication = \"m1-adjudication/ill-typed-optional-references\";\n    let holdout = \"m1-maturity-holdout\";\n    let _ = (frozen_pre_fix_projection, native_output, reference_output, correction_commit, adjudication, holdout);\n    let _ = (run_native_bytes, reference_result, assert_case_hashes);\n}}\n",
                "a".repeat(64),
                "b".repeat(64),
                "c".repeat(64),
            )
        };
        let test_subject = emit_subject(
            root,
            &format!("tools/quality/tests/{test_name}.rs"),
            &test_source,
        );
        match spec.role {
            "reference-implementation" => vec![
                emit_subject(root, "core/test/src/lib.rs", "pub fn run() {}\n"),
                test_subject,
            ],
            "o0-case" | "o1-case" => {
                vec![test_subject, emit_case_manifest_subject(root, spec)]
            }
            "o2-adjudication" => vec![
                test_subject,
                emit_case_manifest_subject(root, spec),
                emit_report_subject(root, spec, None, None, &[]),
            ],
            "fuzz-minimizer" => {
                let dictionary = emit_subject(
                    root,
                    "tools/quality/fuzz/dictionaries/pdf.dict",
                    "header=\"%PDF-1.7\\x0a\"\n",
                );
                let seed_subjects: Vec<_> =
                    ["minimal.pdf", "truncated-header.pdf", "nested-outline.pdf"]
                        .iter()
                        .map(|seed| {
                            emit_subject(
                                root,
                                &format!("tools/quality/fuzz/corpus/m1_document_services/{seed}"),
                                &format!("%PDF-1.7\n% fuzz seed {seed}\n%%EOF\n"),
                            )
                        })
                        .collect();
                let mut subjects = vec![
                    test_subject,
                    emit_subject(
                        root,
                        "tools/quality/fuzz/fuzz_targets/m1.rs",
                        "#![no_main]\nuse libfuzzer_sys::fuzz_target;\nfuzz_target!(|data: &[u8]| { let _ = data; });\n",
                    ),
                    emit_subject(
                        root,
                        "tools/quality/fuzz/Cargo.toml",
                        &format!(
                            "[package]\nname = \"fixture-fuzz\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[package.metadata]\ncargo-fuzz = true\n\n[dependencies]\nlibfuzzer-sys = \"=0.4.13\"\n\n[[bin]]\nname = \"{}\"\npath = \"fuzz_targets/m1.rs\"\ntest = false\ndoc = false\nbench = false\n",
                            if spec.fuzz_registration_drift {
                                "other"
                            } else {
                                "m1"
                            }
                        ),
                    ),
                    dictionary.clone(),
                ];
                subjects.extend(seed_subjects);
                subjects.push(emit_report_subject(
                    root,
                    spec,
                    Some(&dictionary),
                    None,
                    &[],
                ));
                subjects
            }
            "holdout-manifest" => {
                let first_case_id = if spec.holdout_case_overlap {
                    "document/o2"
                } else {
                    "document/holdout-a"
                };
                let mut subjects = vec![test_subject];
                subjects.extend(emit_holdout_case_subjects(
                    root,
                    first_case_id,
                    spec.executed_test,
                    "O1",
                    spec.holdout_fuzz_overlap
                        .then_some("%PDF-1.7\n% fuzz seed minimal.pdf\n%%EOF\n"),
                    spec.placeholder_reviewer,
                ));
                subjects.extend(emit_holdout_case_subjects(
                    root,
                    "document/holdout-b",
                    spec.executed_test,
                    "O2",
                    None,
                    spec.placeholder_reviewer,
                ));
                subjects.push(emit_report_subject(root, spec, None, None, &[]));
                subjects
            }
            "benchmark-report" => {
                let input = emit_subject(
                    root,
                    "tests/cases/document/benchmark/input.pdf",
                    "%PDF-1.7\n%%EOF\n",
                );
                vec![
                    test_subject,
                    input.clone(),
                    emit_report_subject(root, spec, None, Some(&input), &[]),
                ]
            }
            "reference-fingerprint" => {
                let components = vec![
                    emit_subject(
                        root,
                        "tools/quality/tests/support/reference.rs",
                        "pub fn reference_result() {}\n",
                    ),
                    emit_subject(
                        root,
                        "tests/cases/document/fingerprint/case.toml",
                        "fingerprint case manifest\n",
                    ),
                    emit_subject(
                        root,
                        "tests/cases/document/fingerprint/input.pdf",
                        "%PDF-1.7\n%%EOF\n",
                    ),
                    emit_subject(
                        root,
                        "tests/cases/document/fingerprint/expected/service.json",
                        "{}\n",
                    ),
                    emit_subject(
                        root,
                        "evidence/run-environment.toml",
                        &format!(
                            "schema = 1\ntype = \"m1-run-environment\"\ncommit = \"{}\"\ncommit_scope = \"Native implementation anchor only\"\nworktree_scope = \"test harness and evidence are separately content-addressed\"\ncargo_profile = \"release\"\ncargo_flags = \"--release --locked\"\nrustc = \"rustc fixture\"\nos = \"fixture-os\"\ncpu = \"fixture-cpu\"\ngpu = \"not-applicable\"\nmemory_bytes = 1024\nbrowser = \"not-applicable\"\ncargo_fuzz_version = \"0.13.2\"\nexternal_observation = false\n",
                            if spec.commit_anchor_mismatch {
                                "e".repeat(40)
                            } else {
                                "d".repeat(40)
                            }
                        ),
                    ),
                ];
                let mut subjects = vec![test_subject];
                subjects.extend(components.clone());
                subjects.push(emit_report_subject(root, spec, None, None, &components));
                subjects
            }
            "differential-report" => vec![
                test_subject,
                emit_report_subject(root, spec, None, None, &[]),
            ],
            _ => vec![
                test_subject,
                emit_report_subject(root, spec, None, None, &[]),
            ],
        }
    }

    fn emit_report_subject(
        root: &Path,
        spec: &ArtifactSpec<'_>,
        dictionary: Option<&str>,
        benchmark_input: Option<&str>,
        fingerprint_components: &[String],
    ) -> String {
        let fuzz_targets = quoted_array(spec.fuzz_targets.iter().copied());
        let benchmarks = quoted_array(spec.benchmarks.iter().copied());
        let mut contents = format!(
            "schema = 1\ntype = \"maturity-subject-report\"\nid = \"subject.{}\"\nevidence_kind = \"{}\"\nfeature = \"core.test\"\ntarget = \"core/test::run\"\nexecuted_tests = [\"{}\"]\nfuzz_targets = {}\nbenchmarks = {}\nverdict = \"pass\"\n",
            spec.name,
            subject_kind_for_role(spec.role),
            spec.executed_test,
            fuzz_targets,
            benchmarks,
        );
        match spec.role {
            "independent-review" => {
                let reviewer = if spec.placeholder_reviewer {
                    "pending-independent-review"
                } else {
                    "independent-reviewer"
                };
                contents.push_str(&format!(
                    "reviewers = [\"{reviewer}\"]\nindependent = true\n"
                ));
            }
            "o2-adjudication" => {
                contents.push_str(&format!(
                    "case_ids = [\"document/o2\"]\nreviewers = [\"spec-reviewer\", \"quality-reviewer\"]\ncorrection_commit = \"a5ec35b\"\npre_fix_output = \"sha256:{}\"\nnative_output = \"sha256:{}\"\nreference_output = \"sha256:{}\"\n",
                    "a".repeat(64),
                    "b".repeat(64),
                    "c".repeat(64),
                ));
            }
            "fuzz-minimizer" => {
                contents.push_str(&format!(
                    "case_ids = [\"fuzz.m1-document-services.minimal.pdf\", \"fuzz.m1-document-services.truncated-header.pdf\", \"fuzz.m1-document-services.nested-outline.pdf\"]\nminimizer = \"cargo-fuzz-cmin\"\ndictionary = \"{}\"\ncrash_minimization = \"not-applicable:no-crash-observed\"\nowner = \"quality-corpus\"\ninvariant = \"bounded Native parser invariant\"\ncargo_fuzz_version = \"0.13.2\"\n",
                    dictionary.expect("minimizer fixture has a dictionary")
                ));
            }
            "holdout-manifest" => {
                let first_case_id = if spec.holdout_case_overlap {
                    "document/o2"
                } else {
                    "document/holdout-a"
                };
                contents.push_str(&format!(
                    "case_ids = [\"{first_case_id}\", \"document/holdout-b\"]\n"
                ));
            }
            "benchmark-report" => {
                let input = parse_content_reference(
                    benchmark_input.expect("benchmark fixture has an input subject"),
                )
                .unwrap();
                contents.push_str(&format!(
                    "raw_samples_ns = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700, 1800, 1900, 2000, 2100]\nsample_count = 21\nmedian_ns = 1100\np95_ns = 2000\np99_ns = 2100\nmedian_ci95_ns = [600, 1600]\nperformance_eligible = true\nrelease_gate_eligible = false\nexternal_comparison = false\nregression_threshold_eligible = false\nreference_checked = true\ncommit = \"{}\"\ncargo_profile = \"release\"\ncargo_flags = \"--release --locked\"\nrustc = \"rustc fixture\"\nos = \"fixture-os\"\ncpu = \"fixture-cpu\"\ngpu = \"not-applicable\"\nmemory_bytes = 1024\nbrowser = \"not-applicable\"\nrenderer_epoch = \"not-applicable\"\nfont_epoch = \"not-applicable\"\ncolor_epoch = \"not-applicable\"\ntiming_scope = \"Native full in-memory document-service session\"\nmemory_scope = \"bounded session resources\"\nsupport_scope = \"every sample matches reference\"\ncorpus_sha256 = \"sha256:{}\"\ncache_policy = \"one untimed warmup; timed samples use fresh sessions\"\ncommit_scope = \"Native implementation anchor only\"\nworktree_scope = \"test harness and evidence are separately content-addressed\"\n",
                    "d".repeat(40),
                    input.digest,
                ));
            }
            "differential-report" => {
                contents.push_str("case_ids = [\"full-session-case\"]\nfull_session = true\n");
            }
            "reference-fingerprint" => {
                let mut canonical = fingerprint_components.to_vec();
                canonical.sort();
                let canonical_input = canonical.join("\n") + "\n";
                let fingerprint =
                    hex_digest(&sha256(canonical_input.as_bytes()).expect("fixture fingerprint"));
                contents.push_str(&format!(
                    "fingerprint_components = {}\nfingerprint = \"sha256:{fingerprint}\"\n",
                    quoted_array(fingerprint_components.iter().map(String::as_str)),
                ));
            }
            _ => {}
        }
        emit_subject(root, &format!("{}-report.toml", spec.name), &contents)
    }

    fn emit_case_manifest_subject(root: &Path, spec: &ArtifactSpec<'_>) -> String {
        let oracle = match spec.role {
            "o0-case" => "O0",
            "o2-adjudication" => "O2",
            _ => "O1",
        };
        let reviewers = if spec.role == "o2-adjudication" {
            "[\"spec-reviewer\", \"quality-reviewer\"]"
        } else {
            "[\"independent-reviewer\"]"
        };
        let contents = format!(
            "schema = 1\n\n[identity]\nid = \"document/{}\"\ntitle = \"Maturity subject case\"\nowner = \"quality-corpus\"\nstatus = \"active\"\nintroduced_in = \"0.1.0\"\n\n[specification]\ndocument = \"RPE-TEST\"\nversion = \"1\"\nclauses = [\"RPE-TEST/1\"]\ninterpretation = \"Exercise the registered maturity subject.\"\n\n[provenance]\nkind = \"self-authored-generated\"\nsource = \"tools/quality/tests/registered_test.rs\"\nsha256 = \"sha256:{}\"\nlicense = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\nredistributable = false\naccess = \"repository\"\n\n[features]\nids = [\"core.test\"]\nrequirements = [\"RPE-TEST/1\"]\n\n[validity]\nclass = \"valid\"\nstrict_expected = \"success\"\nrecovery_expected = \"not-applicable\"\n\n[expected]\nparse = true\nscene = true\ntext = true\npixel = true\ndiagnostic = true\ncapability = true\nerror = false\n\n[oracle]\nlevel = \"{oracle}\"\nderivation = \"Finite project-authored case.\"\nreviewers = {reviewers}\nreference_may_generate = false\nlast_reviewed = \"2026-07-15\"\n\n[budget]\nmax_input_bytes = 4096\nmax_objects = 16\nmax_resolve_depth = 8\nmax_stream_output_bytes = 4096\nmax_total_decode_bytes = 4096\nmax_image_pixels = 16\nmax_path_segments = 16\nmax_scene_commands = 16\nmax_group_depth = 4\noperator_fuel = 100\ndecode_fuel = 100\nwatchdog_ms = 500\n\n[render]\nwidth = 1\nheight = 1\ndpr_milli = 1000\ncolor_profile = \"srgb-reference-v1\"\nalpha = \"straight\"\nantialias = \"reference-v1\"\nrenderer_epoch = \"test-v1\"\n\n[tolerance]\nmode = \"exact\"\n\n[runners]\nnative = [\"{}\"]\nexternal_observation = []\n\n[history]\nentries = [\"2026-07-15: activated\"]\n",
            spec.name,
            "1".repeat(64),
            spec.executed_test
        );
        emit_subject(
            root,
            &format!("tests/cases/{}/case.toml", spec.name),
            &contents,
        )
    }

    fn emit_holdout_case_subjects(
        root: &Path,
        case_id: &str,
        executed_test: &str,
        oracle: &str,
        input_override: Option<&str>,
        placeholder_reviewer: bool,
    ) -> Vec<String> {
        let prefix = format!("tests/cases/{case_id}");
        let input_contents = input_override
            .map(str::to_owned)
            .unwrap_or_else(|| format!("%PDF-1.7\n% holdout {case_id}\n%%EOF\n"));
        let input = emit_subject(root, &format!("{prefix}/input.pdf"), &input_contents);
        let expected = emit_subject(root, &format!("{prefix}/expected/service.json"), "{}\n");
        let input_digest = parse_content_reference(&input).unwrap().digest;
        let expected_digest = parse_content_reference(&expected).unwrap().digest;
        let reviewers = if placeholder_reviewer {
            "[\"pending-final-independent-review\"]"
        } else if oracle == "O2" {
            "[\"spec-reviewer\", \"quality-reviewer\"]"
        } else {
            "[\"independent-reviewer\"]"
        };
        let manifest = format!(
            "schema = 1\n\n[identity]\nid = \"{case_id}\"\ntitle = \"Independent maturity holdout\"\nowner = \"quality-corpus\"\nstatus = \"active\"\nintroduced_in = \"0.1.0\"\n\n[specification]\ndocument = \"RPE-TEST\"\nversion = \"1\"\nclauses = [\"RPE-TEST/1\"]\ninterpretation = \"Independent content-addressed holdout.\"\n\n[provenance]\nkind = \"self-authored-generated\"\nsource = \"{prefix}/input.pdf\"\nsha256 = \"sha256:{input_digest}\"\nlicense = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\nredistributable = false\naccess = \"repository\"\n\n[features]\nids = [\"core.test\"]\nrequirements = [\"RPE-TEST/1\"]\n\n[validity]\nclass = \"valid\"\nstrict_expected = \"success\"\nrecovery_expected = \"not-applicable\"\n\n[expected]\nparse = true\nscene = false\ntext = false\npixel = false\ndiagnostic = false\ncapability = false\nerror = false\nservice = \"expected/service.json\"\nservice_sha256 = \"sha256:{expected_digest}\"\n\n[oracle]\nlevel = \"{oracle}\"\nderivation = \"Finite holdout evaluation.\"\nreviewers = {reviewers}\nreference_may_generate = false\nlast_reviewed = \"2026-07-15\"\n\n[budget]\nmax_input_bytes = 4096\nmax_objects = 16\nmax_resolve_depth = 8\nmax_stream_output_bytes = 4096\nmax_total_decode_bytes = 4096\nmax_image_pixels = 1\nmax_path_segments = 1\nmax_scene_commands = 1\nmax_group_depth = 1\noperator_fuel = 100\ndecode_fuel = 100\nwatchdog_ms = 500\nmax_pages = 8\nmax_outline_items = 8\nmax_range_resident_bytes = 4096\n\n[render]\nwidth = 1\nheight = 1\ndpr_milli = 1000\ncolor_profile = \"not-applicable\"\nalpha = \"not-applicable\"\nantialias = \"not-applicable\"\nrenderer_epoch = \"test-v1\"\n\n[tolerance]\nmode = \"exact\"\n\n[runners]\nnative = [\"{executed_test}\"]\nexternal_observation = []\n\n[history]\nentries = [\"2026-07-15: activated\"]\n"
        );
        let manifest = emit_subject(root, &format!("{prefix}/case.toml"), &manifest);
        vec![manifest, input, expected]
    }

    fn emit_subject(root: &Path, name: &str, contents: &str) -> String {
        let relative = if name.contains('/') {
            name.to_owned()
        } else {
            format!("subjects/{name}")
        };
        fs::create_dir_all(
            root.join(&relative)
                .parent()
                .expect("subject has a parent directory"),
        )
        .unwrap();
        fs::write(root.join(&relative), contents).unwrap();
        let digest = hex_digest(&sha256(contents.as_bytes()).unwrap());
        format!("{relative}#sha256:{digest}")
    }

    fn quoted_array<'a>(values: impl Iterator<Item = &'a str>) -> String {
        let values: Vec<_> = values.map(|value| format!("\"{value}\"")).collect();
        format!("[{}]", values.join(", "))
    }

    fn ledger_document(entries: &[LedgerEntry]) -> String {
        let mut document = "schema = 1\nversion = \"test\"\nstatus = \"active\"\n".to_owned();
        for entry in entries {
            document.push_str(&format!(
                "\n[[data]]\nid = \"evidence.{}\"\nkind = \"project-authored-maturity-evidence\"\nsource = \"{}\"\nsource_hash = \"sha256:{}\"\n",
                entry.name, entry.path, entry.digest
            ));
        }
        document
    }

    fn with_path(reference: &str, replacement: &str) -> String {
        let (_, digest) = reference.split_once("#sha256:").unwrap();
        format!("{replacement}#sha256:{digest}")
    }

    fn with_digest(reference: &str, digest: &str) -> String {
        let (path, _) = reference.split_once("#sha256:").unwrap();
        format!("{path}#sha256:{digest}")
    }

    fn replace_profile_field(
        document: &str,
        profile_id: &str,
        field: &str,
        replacement: &str,
    ) -> String {
        let profile_marker = format!("[[profile]]\nid = \"{profile_id}\"");
        let profile_start = document
            .find(&profile_marker)
            .unwrap_or_else(|| panic!("missing profile {profile_id}"));
        let profile_end = document[profile_start + profile_marker.len()..]
            .find("\n[[profile]]")
            .map_or(document.len(), |offset| {
                profile_start + profile_marker.len() + offset
            });
        let field_marker = format!("\n{field} = ");
        let field_start = document[profile_start..profile_end]
            .find(&field_marker)
            .map(|offset| profile_start + offset + 1)
            .unwrap_or_else(|| panic!("missing {field} in profile {profile_id}"));
        let field_end = document[field_start..profile_end]
            .find('\n')
            .map_or(profile_end, |offset| field_start + offset);
        format!(
            "{}{} = {}{}",
            &document[..field_start],
            field,
            replacement,
            &document[field_end..]
        )
    }

    fn assert_rejected(mutation: Mutation, code: &str) {
        let repository = TestRepository::differential(mutation);
        let diagnostics = repository.diagnostics();
        assert!(
            diagnostics.iter().any(|diagnostic| diagnostic.code == code),
            "{mutation:?} did not emit {code}: {diagnostics:?}"
        );
    }

    #[test]
    fn repository_profiles_are_valid_and_truthfully_promoted() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/traceability/capability-profiles.toml");
        let report = validate_maturity_file(&path).unwrap();
        assert_eq!(report.profiles, 4);
        assert_eq!(report.planned, 0);
        assert_eq!(report.reference, 2);
        assert_eq!(report.differential, 2);
    }

    #[test]
    fn rejects_paper_reference_promotion() {
        let input = replace_profile_field(
            include_str!("../../../docs/traceability/capability-profiles.toml"),
            "m1.r0-strict.v1",
            "o0_cases",
            "[]",
        );
        let input = replace_profile_field(
            &input,
            "m1.r0-strict.v1",
            "independent_review",
            "\"REQUIRED_BEFORE_REFERENCE\"",
        );
        let diagnostics = validate_maturity(&input).unwrap_err();
        assert!(diagnostics.iter().any(|value| {
            value.to_string() == "RPE-MATURITY-0010 profile=m1.r0-strict.v1 field=o0_cases"
        }));
        assert!(diagnostics.iter().any(|value| {
            value.to_string()
                == "RPE-MATURITY-0010 profile=m1.r0-strict.v1 field=independent_review"
        }));
    }

    #[test]
    fn rejects_paper_differential_promotion() {
        let profile_id = "m1.strict-page-count.v1";
        let mut input =
            include_str!("../../../docs/traceability/capability-profiles.toml").to_owned();
        for (field, replacement) in [
            ("o2_adjudications", "[]"),
            ("fuzz_targets", "[]"),
            ("fuzz_minimizer", "\"REQUIRED_BEFORE_DIFFERENTIAL\""),
            ("holdout_manifest", "\"REQUIRED_BEFORE_DIFFERENTIAL\""),
            ("benchmark_report", "\"REQUIRED_BEFORE_DIFFERENTIAL\""),
            ("differential_report", "\"REQUIRED_BEFORE_DIFFERENTIAL\""),
            ("baseline_fingerprint", "\"REQUIRED_BEFORE_DIFFERENTIAL\""),
        ] {
            input = replace_profile_field(&input, profile_id, field, replacement);
        }
        let diagnostics = validate_maturity(&input).unwrap_err();
        for field in [
            "o2_adjudications",
            "fuzz_targets",
            "fuzz_minimizer",
            "holdout_manifest",
            "benchmark_report",
            "differential_report",
            "baseline_fingerprint",
        ] {
            assert!(diagnostics.iter().any(|value| {
                value.to_string() == format!("RPE-MATURITY-0011 profile={profile_id} field={field}")
            }));
        }
    }

    #[test]
    fn accepts_complete_content_addressed_differential_evidence() {
        let repository = TestRepository::differential(Mutation::None);
        let report = validate_maturity_file(&repository.profile_path).unwrap();
        assert_eq!(report.profiles, 1);
        assert_eq!(report.planned, 0);
        assert_eq!(report.reference, 0);
        assert_eq!(report.differential, 1);
    }

    #[test]
    fn rejects_fake_missing_nonregular_and_mismatched_content() {
        assert_rejected(Mutation::MissingPath, "RPE-MATURITY-0014");
        assert_rejected(Mutation::NonRegularPath, "RPE-MATURITY-0014");
        assert_rejected(Mutation::HashMismatch, "RPE-MATURITY-0014");
    }

    #[test]
    fn rejects_holdout_cases_reused_by_o0_o1_or_o2_evidence() {
        assert_rejected(Mutation::HoldoutCaseOverlap, "RPE-MATURITY-0023");
    }

    #[test]
    fn rejects_holdout_bytes_reused_by_the_fuzz_training_corpus() {
        assert_rejected(Mutation::HoldoutFuzzDigestOverlap, "RPE-MATURITY-0024");
    }

    #[test]
    fn rejects_placeholder_reviewer_identities() {
        let repository = TestRepository::differential(Mutation::PlaceholderReviewer);
        let diagnostics = repository.diagnostics();
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "RPE-MATURITY-0021"
                && diagnostic.field.as_deref() == Some("holdout_manifest:subject_binding")
        }));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "RPE-MATURITY-0021"
                && diagnostic.field.as_deref() == Some("independent_review:subject_binding")
        }));
    }

    #[test]
    fn rejects_benchmark_and_run_environment_commit_mismatch() {
        assert_rejected(Mutation::CommitAnchorMismatch, "RPE-MATURITY-0025");
    }

    #[test]
    fn rejects_absolute_traversal_and_noncanonical_content_references() {
        let digest = "0".repeat(64);
        for reference in [
            format!("/tmp/evidence.toml#sha256:{digest}"),
            format!("../evidence.toml#sha256:{digest}"),
            format!("./evidence.toml#sha256:{digest}"),
            format!("evidence/./file.toml#sha256:{digest}"),
            format!("evidence//file.toml#sha256:{digest}"),
            format!("evidence/file.toml/#sha256:{digest}"),
            format!("C:/evidence.toml#sha256:{digest}"),
            format!("evidence.toml#sha256:{}", "A".repeat(64)),
        ] {
            assert!(parse_content_reference(&reference).is_err(), "{reference}");
        }
    }

    #[test]
    fn derives_current_directory_for_repository_relative_profile_path() {
        let root = derive_repository_root(Path::new("docs/traceability/capability-profiles.toml"))
            .unwrap();
        assert_eq!(root, Path::new("."));
    }

    #[test]
    fn keeps_targets_symbolic_instead_of_content_addressed_or_file_shaped() {
        assert!(valid_symbolic_target(
            "core/document::AttestedRevisionIndex::count_pages"
        ));
        for target in [
            "tests/evidence/report.toml",
            "/core/document::count_pages",
            "core/document::count_pages#sha256:deadbeef",
            "core/document/count_pages.rs",
        ] {
            assert!(!valid_symbolic_target(target), "{target}");
        }
    }

    #[test]
    fn rejects_ambiguous_fuzz_bin_registrations() {
        let prefix = "[package]\npublish = false\n\n[package.metadata]\ncargo-fuzz = true\n\n[dependencies]\nlibfuzzer-sys = \"=0.4.13\"\n\n";
        let duplicate_name = format!(
            "{prefix}[[bin]]\nname = \"one\"\npath = \"fuzz_targets/one.rs\"\n\n[[bin]]\nname = \"one\"\npath = \"fuzz_targets/two.rs\"\n"
        );
        let duplicate_path = format!(
            "{prefix}[[bin]]\nname = \"one\"\npath = \"fuzz_targets/one.rs\"\n\n[[bin]]\nname = \"two\"\npath = \"fuzz_targets/one.rs\"\n"
        );
        let unknown_key = format!(
            "{prefix}[[bin]]\nname = \"one\"\npath = \"fuzz_targets/one.rs\"\nmagic = true\n"
        );
        for manifest in [&duplicate_name, &duplicate_path, &unknown_key] {
            assert!(parse_fuzz_registrations(manifest).is_none());
        }
    }

    #[test]
    fn fuzz_binding_rejects_compile_only_paper_evidence() {
        let executed_tests = vec!["tools/quality::m1_document_service_fuzz".to_owned()];
        let fuzz_targets = vec!["fuzz.m1documentservices".to_owned()];
        let real = VerifiedSubject {
            relative: "tools/quality/tests/m1_document_service_fuzz.rs".to_owned(),
            bytes: include_bytes!("../tests/m1_document_service_fuzz.rs").to_vec(),
            package_registered: true,
        };
        assert!(fuzz_build_test_binds(&real, &executed_tests, &fuzz_targets));

        let compile_only = VerifiedSubject {
            relative: real.relative.clone(),
            bytes: b"#[test]\nfn paper() { let status = std::process::Command::new(\"cargo\").arg(\"check\").arg(\"--manifest-path\").arg(\"fuzz/Cargo.toml\").status().unwrap(); assert!(status.success()); }\n".to_vec(),
            package_registered: true,
        };
        assert!(!fuzz_build_test_binds(
            &compile_only,
            &executed_tests,
            &fuzz_targets
        ));
    }

    #[test]
    fn benchmark_binding_rejects_raw_sample_only_paper_evidence() {
        let executed_tests = vec!["tools/quality::m1_document_service_maturity".to_owned()];
        let real = VerifiedSubject {
            relative: "tools/quality/tests/m1_document_service_maturity.rs".to_owned(),
            bytes: include_bytes!("../tests/m1_document_service_maturity.rs").to_vec(),
            package_registered: true,
        };
        assert!(benchmark_test_binds(&real, &executed_tests));

        let paper = VerifiedSubject {
            relative: real.relative,
            bytes: b"#[test]\nfn paper() { let raw_samples_ns = [1_u64; 21]; assert_eq!(raw_samples_ns.len(), 21); }\n".to_vec(),
            package_registered: true,
        };
        assert!(!benchmark_test_binds(&paper, &executed_tests));
    }

    #[test]
    fn rejects_ineligible_external_or_nonverdict_evidence() {
        for mutation in [
            Mutation::OracleO4,
            Mutation::Ineligible,
            Mutation::Unregistered,
            Mutation::NonGating,
            Mutation::ExternalObservation,
        ] {
            assert_rejected(mutation, "RPE-MATURITY-0015");
        }
    }

    #[test]
    fn rejects_artifact_schema_and_cross_reference_drift() {
        assert_rejected(Mutation::ArtifactSchema, "RPE-MATURITY-0015");
        assert_rejected(Mutation::CrossReference, "RPE-MATURITY-0015");
    }

    #[test]
    fn rejects_feature_spec_ledger_and_registration_drift() {
        assert_rejected(Mutation::FeatureState, "RPE-MATURITY-0016");
        assert_rejected(Mutation::SpecFeature, "RPE-MATURITY-0017");
        assert_rejected(Mutation::LedgerHash, "RPE-MATURITY-0018");
        assert_rejected(Mutation::LedgerId, "RPE-MATURITY-0018");
        assert_rejected(Mutation::DuplicateLedgerId, "RPE-MATURITY-0018");
        assert_rejected(Mutation::FuzzRegistration, "RPE-MATURITY-0020");
    }

    #[test]
    fn rejects_duplicate_profile_evidence_references() {
        assert_rejected(Mutation::DuplicateReference, "RPE-MATURITY-0019");
    }

    #[test]
    fn rejects_empty_drifted_or_wrongly_marked_subjects_and_unregistered_tests() {
        assert_rejected(Mutation::EmptySubjects, "RPE-MATURITY-0021");
        assert_rejected(Mutation::SubjectHash, "RPE-MATURITY-0014");
        assert_rejected(Mutation::WrongSubjectKind, "RPE-MATURITY-0015");
        assert_rejected(Mutation::UnregisteredTest, "RPE-MATURITY-0022");
        assert_rejected(Mutation::FuzzCargoRegistration, "RPE-MATURITY-0021");
    }
}
