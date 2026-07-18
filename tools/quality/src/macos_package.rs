//! Fail-closed inspection of the selected signed universal macOS package.
//!
//! This module validates an already-built package against external trust
//! anchors. It does not create those anchors, prove their provenance, or turn
//! a successful local inspection into App Sandbox enforcement evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use pdf_rs_digest::{Sha256, hex_digest, sha256};

/// Fixed basename of the selected desktop application bundle.
pub const APP_BUNDLE_NAME: &str = "PDF.rs.app";
/// Fixed path to the host executable inside the selected application bundle.
pub const HOST_EXECUTABLE_RELATIVE: &str = "Contents/MacOS/PDF.rs";
/// Fixed path to the Worker helper inside the selected application bundle.
pub const WORKER_HELPER_RELATIVE: &str = "Contents/Helpers/pdf-rs-desktop-worker";
/// Fixed signing and bundle identifier of the host application.
pub const HOST_IDENTIFIER: &str = "rs.pdf.desktop";
/// Fixed signing identifier of the embedded Worker helper.
pub const WORKER_IDENTIFIER: &str = "rs.pdf.desktop.worker";

const INFO_PLIST_RELATIVE: &str = "Contents/Info.plist";
const CODE_RESOURCES_RELATIVE: &str = "Contents/_CodeSignature/CodeResources";
const APPROVAL_RECORD_RELATIVE: &str = "platform/desktop/macos/package-approval.toml";
const APPROVAL_SCOPE: &str = "external_release_trust_anchor";
const PACKAGE_HASH_DOMAIN: &[u8] = b"PDF.rs/macos-package-tree/v1\0";
const REQUIRED_FEATURE_MARKER: &[u8] = b"PDF_RS_DESKTOP_FEATURE_CLOSURE:NO_DEFAULT_FEATURES:v1";
const FORBIDDEN_FIXTURE_MARKER: &[u8] = b"PDF_RS_DESKTOP_FEATURE_CLOSURE:TRANSPORT_FIXTURE:v1";
const FORBIDDEN_ENGINES: &[&[u8]] = &[
    b"pdfium", b"pdf.js", b"pdfjs", b"mupdf", b"poppler", b"hayro", b"vello",
];

const EXPECTED_DIRECTORIES: &[&str] = &[
    "Contents",
    "Contents/Helpers",
    "Contents/MacOS",
    "Contents/_CodeSignature",
];
const EXPECTED_FILES: &[&str] = &[
    INFO_PLIST_RELATIVE,
    WORKER_HELPER_RELATIVE,
    HOST_EXECUTABLE_RELATIVE,
    CODE_RESOURCES_RELATIVE,
];
const EXPECTED_ARCHITECTURES: &[&str] = &["arm64", "x86_64"];
const EXPECTED_HOST_ENTITLEMENTS: &[(&str, bool)] = &[
    ("com.apple.security.app-sandbox", true),
    ("com.apple.security.files.user-selected.read-only", true),
];
const EXPECTED_WORKER_ENTITLEMENTS: &[(&str, bool)] = &[
    ("com.apple.security.app-sandbox", true),
    ("com.apple.security.inherit", true),
];

const MAX_APPROVAL_BYTES: u64 = 16 * 1024;
const MAX_METADATA_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 136 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 16;
const MAX_COMMAND_STREAM_BYTES: u64 = 1024 * 1024;
const MAX_COMMANDS: usize = 18;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DEPENDENCIES: usize = 64;

const CODESIGN: &str = "/usr/bin/codesign";
const LIPO: &str = "/usr/bin/lipo";
const OTOOL: &str = "/usr/bin/otool";
const PLUTIL: &str = "/usr/bin/plutil";

/// A stable fail-closed package-verification diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacosPackageViolation {
    /// Stable diagnostic code.
    pub code: &'static str,
    /// Actual path associated with the failed observation.
    pub path: PathBuf,
    /// Content-free machine-readable reason.
    pub token: String,
}

/// Facts observed by a successful package verification.
///
/// The hashes in this report matched an external approval record. This report
/// deliberately contains no field that claims content provenance, release
/// approval, or live App Sandbox enforcement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacosPackageReport {
    /// Number of regular files in the exact package inventory.
    pub package_files: usize,
    /// Number of allowed Mach-O executables in the exact package inventory.
    pub package_executables: usize,
    /// SHA-256 observed for the embedded feature-free Worker.
    pub observed_worker_sha256: String,
    /// Canonical SHA-256 observed for the complete package tree.
    pub observed_package_sha256: String,
    /// Worker SHA-256 supplied by the external approval record.
    pub worker_external_trust_anchor_sha256: String,
    /// Package SHA-256 supplied by the external approval record.
    pub package_external_trust_anchor_sha256: String,
    /// Signing team identifier supplied by the external approval record.
    pub signing_team_identifier: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApprovalRecord {
    app_version: String,
    build_version: String,
    team_identifier: String,
    worker_sha256: String,
    package_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ParsedPlistValue {
    Boolean(bool),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SigningMetadata {
    executable: String,
    identifier: String,
    team_identifier: String,
    authorities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageEntry {
    relative: String,
    kind: EntryKind,
    mode: u32,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageInventory {
    entries: Vec<PackageEntry>,
    observed_package_sha256: String,
    observed_worker_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum InspectionKind {
    Plist,
    VerifyDeepStrict,
    VerifyHostStrict,
    VerifyWorkerStrict,
    HostSigningMetadataArm64,
    HostSigningMetadataX86_64,
    WorkerSigningMetadataArm64,
    WorkerSigningMetadataX86_64,
    HostEntitlementsArm64,
    HostEntitlementsX86_64,
    WorkerEntitlementsArm64,
    WorkerEntitlementsX86_64,
    HostArchitectures,
    WorkerArchitectures,
    HostDependenciesArm64,
    HostDependenciesX86_64,
    WorkerDependenciesArm64,
    WorkerDependenciesX86_64,
}

impl InspectionKind {
    const ALL: [Self; MAX_COMMANDS] = [
        Self::Plist,
        Self::VerifyDeepStrict,
        Self::VerifyHostStrict,
        Self::VerifyWorkerStrict,
        Self::HostSigningMetadataArm64,
        Self::HostSigningMetadataX86_64,
        Self::WorkerSigningMetadataArm64,
        Self::WorkerSigningMetadataX86_64,
        Self::HostEntitlementsArm64,
        Self::HostEntitlementsX86_64,
        Self::WorkerEntitlementsArm64,
        Self::WorkerEntitlementsX86_64,
        Self::HostArchitectures,
        Self::WorkerArchitectures,
        Self::HostDependenciesArm64,
        Self::HostDependenciesX86_64,
        Self::WorkerDependenciesArm64,
        Self::WorkerDependenciesX86_64,
    ];

    fn token(self) -> &'static str {
        match self {
            Self::Plist => "info-plist",
            Self::VerifyDeepStrict => "codesign-deep-strict",
            Self::VerifyHostStrict => "codesign-host-strict",
            Self::VerifyWorkerStrict => "codesign-worker-strict",
            Self::HostSigningMetadataArm64 => "host-signing-metadata-arm64",
            Self::HostSigningMetadataX86_64 => "host-signing-metadata-x86_64",
            Self::WorkerSigningMetadataArm64 => "worker-signing-metadata-arm64",
            Self::WorkerSigningMetadataX86_64 => "worker-signing-metadata-x86_64",
            Self::HostEntitlementsArm64 => "host-entitlements-arm64",
            Self::HostEntitlementsX86_64 => "host-entitlements-x86_64",
            Self::WorkerEntitlementsArm64 => "worker-entitlements-arm64",
            Self::WorkerEntitlementsX86_64 => "worker-entitlements-x86_64",
            Self::HostArchitectures => "host-architectures",
            Self::WorkerArchitectures => "worker-architectures",
            Self::HostDependenciesArm64 => "host-dependencies-arm64",
            Self::HostDependenciesX86_64 => "host-dependencies-x86_64",
            Self::WorkerDependenciesArm64 => "worker-dependencies-arm64",
            Self::WorkerDependenciesX86_64 => "worker-dependencies-x86_64",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InspectionRequest {
    kind: InspectionKind,
    target: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InspectionOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    output_too_large: bool,
}

trait Inspector {
    fn inspect(&self, request: &InspectionRequest) -> io::Result<InspectionOutput>;
}

struct SystemInspector;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RawObservation {
    outputs: BTreeMap<&'static str, String>,
}

/// Verifies an actual application bundle against the fixed package contract and
/// the repository's canonical external approval record.
///
/// The approval record must exist at
/// `platform/desktop/macos/package-approval.toml` below `repository_root`.
/// The repository intentionally does not ship that record before an external
/// release authority approves a concrete signed package.
pub fn verify_macos_package(
    repository_root: &Path,
    app: &Path,
) -> Result<MacosPackageReport, Vec<MacosPackageViolation>> {
    verify_macos_package_with(repository_root, app, &SystemInspector)
}

fn verify_macos_package_with(
    repository_root: &Path,
    app: &Path,
    inspector: &dyn Inspector,
) -> Result<MacosPackageReport, Vec<MacosPackageViolation>> {
    let mut violations = Vec::new();
    let approval_path = repository_root.join(APPROVAL_RECORD_RELATIVE);
    let approval = read_approval_record(&approval_path, &mut violations);
    let canonical_app = validate_app_root(app, &mut violations);
    let inventory = canonical_app
        .as_deref()
        .and_then(|path| collect_package_inventory(path, &mut violations));

    let (Some(approval), Some(canonical_app), Some(inventory)) =
        (approval, canonical_app, inventory)
    else {
        return Err(violations);
    };
    if !violations.is_empty() {
        return Err(violations);
    }

    validate_inventory_anchors(&canonical_app, &approval, &inventory, &mut violations);
    if !violations.is_empty() {
        return Err(violations);
    }

    let host = canonical_app.join(HOST_EXECUTABLE_RELATIVE);
    let worker = canonical_app.join(WORKER_HELPER_RELATIVE);
    let info = canonical_app.join(INFO_PLIST_RELATIVE);
    let requests = [
        InspectionRequest {
            kind: InspectionKind::Plist,
            target: info,
        },
        InspectionRequest {
            kind: InspectionKind::VerifyDeepStrict,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::VerifyHostStrict,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::VerifyWorkerStrict,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostSigningMetadataArm64,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostSigningMetadataX86_64,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerSigningMetadataArm64,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerSigningMetadataX86_64,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostEntitlementsArm64,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostEntitlementsX86_64,
            target: canonical_app.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerEntitlementsArm64,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerEntitlementsX86_64,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostArchitectures,
            target: host.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerArchitectures,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostDependenciesArm64,
            target: host.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::HostDependenciesX86_64,
            target: host,
        },
        InspectionRequest {
            kind: InspectionKind::WorkerDependenciesArm64,
            target: worker.clone(),
        },
        InspectionRequest {
            kind: InspectionKind::WorkerDependenciesX86_64,
            target: worker,
        },
    ];
    debug_assert_eq!(
        requests
            .iter()
            .map(|request| request.kind)
            .collect::<Vec<_>>(),
        InspectionKind::ALL
    );

    let observation = inspect_all(inspector, &requests, &mut violations);
    if let Some(observation) = observation {
        validate_observation(&canonical_app, &approval, &observation, &mut violations);
    }
    match collect_package_inventory(&canonical_app, &mut violations) {
        Some(final_inventory) if final_inventory == inventory => {}
        Some(_) => violation(
            &mut violations,
            "RPE-MACOS-PACKAGE-0003",
            canonical_app.clone(),
            "package-changed-during-inspection",
        ),
        None => {}
    }

    if violations.is_empty() {
        Ok(MacosPackageReport {
            package_files: EXPECTED_FILES.len(),
            package_executables: 2,
            observed_worker_sha256: inventory.observed_worker_sha256,
            observed_package_sha256: inventory.observed_package_sha256,
            worker_external_trust_anchor_sha256: approval.worker_sha256,
            package_external_trust_anchor_sha256: approval.package_sha256,
            signing_team_identifier: approval.team_identifier,
        })
    } else {
        Err(violations)
    }
}

fn validate_observation(
    app: &Path,
    approval: &ApprovalRecord,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    validate_info_plist(app, approval, observation, violations);
    validate_signing_metadata(app, approval, observation, violations);
    validate_entitlements(app, observation, violations);
    validate_architectures(app, observation, violations);
    validate_dependencies(app, observation, violations);
}

fn validate_info_plist(
    app: &Path,
    approval: &ApprovalRecord,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    let path = app.join(INFO_PLIST_RELATIVE);
    let Some(input) = observation.outputs.get(InspectionKind::Plist.token()) else {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0006",
            path,
            "missing-info-plist-output",
        );
        return;
    };
    let parsed = match parse_flat_plist(input) {
        Ok(parsed) => parsed,
        Err(token) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0005",
                path,
                format!("malformed-info-plist={token}"),
            );
            return;
        }
    };
    let expected = BTreeMap::from([
        (
            "CFBundleExecutable".to_owned(),
            ParsedPlistValue::String("PDF.rs".to_owned()),
        ),
        (
            "CFBundleIdentifier".to_owned(),
            ParsedPlistValue::String(HOST_IDENTIFIER.to_owned()),
        ),
        (
            "CFBundleName".to_owned(),
            ParsedPlistValue::String("PDF.rs".to_owned()),
        ),
        (
            "CFBundlePackageType".to_owned(),
            ParsedPlistValue::String("APPL".to_owned()),
        ),
        (
            "CFBundleShortVersionString".to_owned(),
            ParsedPlistValue::String(approval.app_version.clone()),
        ),
        (
            "CFBundleVersion".to_owned(),
            ParsedPlistValue::String(approval.build_version.clone()),
        ),
    ]);
    if parsed != expected {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0005",
            path,
            "info-plist-field-drift",
        );
    }
}

fn validate_signing_metadata(
    app: &Path,
    approval: &ApprovalRecord,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    let mut parsed = BTreeMap::new();
    for (kind, expected_identifier, expected_executable, path) in [
        (
            InspectionKind::HostSigningMetadataArm64,
            HOST_IDENTIFIER,
            app.join(HOST_EXECUTABLE_RELATIVE),
            app.to_path_buf(),
        ),
        (
            InspectionKind::HostSigningMetadataX86_64,
            HOST_IDENTIFIER,
            app.join(HOST_EXECUTABLE_RELATIVE),
            app.to_path_buf(),
        ),
        (
            InspectionKind::WorkerSigningMetadataArm64,
            WORKER_IDENTIFIER,
            app.join(WORKER_HELPER_RELATIVE),
            app.join(WORKER_HELPER_RELATIVE),
        ),
        (
            InspectionKind::WorkerSigningMetadataX86_64,
            WORKER_IDENTIFIER,
            app.join(WORKER_HELPER_RELATIVE),
            app.join(WORKER_HELPER_RELATIVE),
        ),
    ] {
        let Some(input) = observation.outputs.get(kind.token()) else {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                path,
                format!("missing-output={}", kind.token()),
            );
            continue;
        };
        match parse_signing_metadata(input) {
            Ok(metadata)
                if metadata.identifier == expected_identifier
                    && metadata.team_identifier == approval.team_identifier
                    && Path::new(&metadata.executable) == expected_executable =>
            {
                parsed.insert(kind.token(), metadata);
            }
            Ok(metadata) => {
                if metadata.identifier != expected_identifier {
                    violation(
                        violations,
                        "RPE-MACOS-PACKAGE-0004",
                        path.clone(),
                        format!("identifier-drift={}", kind.token()),
                    );
                }
                if metadata.team_identifier != approval.team_identifier {
                    violation(
                        violations,
                        "RPE-MACOS-PACKAGE-0004",
                        path.clone(),
                        format!("team-identifier-drift={}", kind.token()),
                    );
                }
                if Path::new(&metadata.executable) != expected_executable {
                    violation(
                        violations,
                        "RPE-MACOS-PACKAGE-0004",
                        path.clone(),
                        format!("signed-executable-path-drift={}", kind.token()),
                    );
                }
            }
            Err(token) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0004",
                path.clone(),
                format!("malformed-signing-metadata={}:{}", kind.token(), token),
            ),
        }
    }
    let host_arm = parsed.get(InspectionKind::HostSigningMetadataArm64.token());
    let host_x86 = parsed.get(InspectionKind::HostSigningMetadataX86_64.token());
    let worker_arm = parsed.get(InspectionKind::WorkerSigningMetadataArm64.token());
    let worker_x86 = parsed.get(InspectionKind::WorkerSigningMetadataX86_64.token());
    if let (Some(host_arm), Some(host_x86)) = (host_arm, host_x86)
        && host_arm != host_x86
    {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0004",
            app.to_path_buf(),
            "host-signing-metadata-slice-drift",
        );
    }
    if let (Some(worker_arm), Some(worker_x86)) = (worker_arm, worker_x86)
        && worker_arm != worker_x86
    {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0004",
            app.join(WORKER_HELPER_RELATIVE),
            "worker-signing-metadata-slice-drift",
        );
    }
    if let (Some(host), Some(worker)) = (host_arm, worker_arm)
        && host.authorities != worker.authorities
    {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0004",
            app.to_path_buf(),
            "host-worker-authority-chain-drift",
        );
    }
}

fn validate_entitlements(
    app: &Path,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    for (kind, expected, path) in [
        (
            InspectionKind::HostEntitlementsArm64,
            EXPECTED_HOST_ENTITLEMENTS,
            app.to_path_buf(),
        ),
        (
            InspectionKind::HostEntitlementsX86_64,
            EXPECTED_HOST_ENTITLEMENTS,
            app.to_path_buf(),
        ),
        (
            InspectionKind::WorkerEntitlementsArm64,
            EXPECTED_WORKER_ENTITLEMENTS,
            app.join(WORKER_HELPER_RELATIVE),
        ),
        (
            InspectionKind::WorkerEntitlementsX86_64,
            EXPECTED_WORKER_ENTITLEMENTS,
            app.join(WORKER_HELPER_RELATIVE),
        ),
    ] {
        let Some(input) = observation.outputs.get(kind.token()) else {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                path,
                format!("missing-output={}", kind.token()),
            );
            continue;
        };
        match parse_flat_plist(input) {
            Ok(parsed) if exact_boolean_plist(&parsed, expected) => {}
            Ok(_) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0005",
                path,
                format!("entitlement-drift={}", kind.token()),
            ),
            Err(token) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0005",
                path,
                format!("malformed-entitlements={}:{}", kind.token(), token),
            ),
        }
    }
}

fn validate_architectures(
    app: &Path,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    for (kind, path) in [
        (
            InspectionKind::HostArchitectures,
            app.join(HOST_EXECUTABLE_RELATIVE),
        ),
        (
            InspectionKind::WorkerArchitectures,
            app.join(WORKER_HELPER_RELATIVE),
        ),
    ] {
        let Some(input) = observation.outputs.get(kind.token()) else {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                path,
                format!("missing-output={}", kind.token()),
            );
            continue;
        };
        match parse_architectures(input) {
            Ok(architectures)
                if architectures
                    == EXPECTED_ARCHITECTURES
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect() => {}
            Ok(_) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0007",
                path,
                format!("architecture-set-drift={}", kind.token()),
            ),
            Err(token) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0007",
                path,
                format!("malformed-architectures={}:{}", kind.token(), token),
            ),
        }
    }
}

fn validate_dependencies(
    app: &Path,
    observation: &RawObservation,
    violations: &mut Vec<MacosPackageViolation>,
) {
    let mut parsed = BTreeMap::new();
    for (kind, path) in [
        (
            InspectionKind::HostDependenciesArm64,
            app.join(HOST_EXECUTABLE_RELATIVE),
        ),
        (
            InspectionKind::HostDependenciesX86_64,
            app.join(HOST_EXECUTABLE_RELATIVE),
        ),
        (
            InspectionKind::WorkerDependenciesArm64,
            app.join(WORKER_HELPER_RELATIVE),
        ),
        (
            InspectionKind::WorkerDependenciesX86_64,
            app.join(WORKER_HELPER_RELATIVE),
        ),
    ] {
        let Some(input) = observation.outputs.get(kind.token()) else {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                path,
                format!("missing-output={}", kind.token()),
            );
            continue;
        };
        match parse_dependencies(input, &path) {
            Ok(dependencies)
                if dependencies
                    .iter()
                    .all(|dependency| allowed_system_dependency(dependency)) =>
            {
                parsed.insert(kind.token(), dependencies);
            }
            Ok(dependencies) => {
                for dependency in dependencies {
                    if !allowed_system_dependency(&dependency) {
                        let token = if contains_forbidden_engine(dependency.as_bytes()) {
                            format!("forbidden-pdf-engine-dependency={dependency}")
                        } else {
                            format!("unapproved-dynamic-dependency={dependency}")
                        };
                        violation(violations, "RPE-MACOS-PACKAGE-0008", path.clone(), token);
                    }
                }
            }
            Err(token) => violation(
                violations,
                "RPE-MACOS-PACKAGE-0008",
                path,
                format!("malformed-dependency-output={}:{}", kind.token(), token),
            ),
        }
    }
    for (arm, x86, path, token) in [
        (
            InspectionKind::HostDependenciesArm64,
            InspectionKind::HostDependenciesX86_64,
            app.join(HOST_EXECUTABLE_RELATIVE),
            "host-dynamic-dependency-slice-drift",
        ),
        (
            InspectionKind::WorkerDependenciesArm64,
            InspectionKind::WorkerDependenciesX86_64,
            app.join(WORKER_HELPER_RELATIVE),
            "worker-dynamic-dependency-slice-drift",
        ),
    ] {
        if let (Some(arm), Some(x86)) = (parsed.get(arm.token()), parsed.get(x86.token()))
            && arm != x86
        {
            violation(violations, "RPE-MACOS-PACKAGE-0008", path, token);
        }
    }
}

fn exact_boolean_plist(
    parsed: &BTreeMap<String, ParsedPlistValue>,
    expected: &[(&str, bool)],
) -> bool {
    if parsed.len() != expected.len() {
        return false;
    }
    expected.iter().all(|(key, value)| {
        let expected_value = ParsedPlistValue::Boolean(*value);
        parsed.get(*key) == Some(&expected_value)
    })
}

fn parse_signing_metadata(input: &str) -> Result<SigningMetadata, &'static str> {
    if contains_ascii_case_insensitive(input.as_bytes(), b"signature=adhoc")
        || contains_ascii_case_insensitive(input.as_bytes(), b"flags=adhoc")
        || contains_ascii_case_insensitive(input.as_bytes(), b"(adhoc)")
    {
        return Err("ad-hoc-signature");
    }
    let executable = single_prefixed_line(input, "Executable=")?;
    if !Path::new(executable).is_absolute() {
        return Err("non-absolute-signed-executable");
    }
    let identifier = single_prefixed_line(input, "Identifier=")?;
    let team_identifier = single_prefixed_line(input, "TeamIdentifier=")?;
    let code_directory = single_prefixed_line(input, "CodeDirectory ")?;
    if !code_directory.contains("flags=") || !code_directory.contains("(runtime)") {
        return Err("missing-hardened-runtime");
    }
    let signature_size = single_prefixed_line(input, "Signature size=")?;
    if signature_size
        .parse::<u64>()
        .ok()
        .filter(|size| *size > 0)
        .is_none()
    {
        return Err("invalid-signature-size");
    }
    let authorities: Vec<_> = input
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("Authority="))
        .map(str::to_owned)
        .collect();
    let unique_authorities: BTreeSet<_> = authorities.iter().collect();
    if authorities.is_empty()
        || authorities.len() != unique_authorities.len()
        || authorities.iter().any(String::is_empty)
        || authorities.len() > 4
    {
        return Err("invalid-authority-chain");
    }
    Ok(SigningMetadata {
        executable: executable.to_owned(),
        identifier: identifier.to_owned(),
        team_identifier: team_identifier.to_owned(),
        authorities,
    })
}

fn single_prefixed_line<'a>(input: &'a str, prefix: &str) -> Result<&'a str, &'static str> {
    let values: Vec<_> = input
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(prefix))
        .collect();
    match values.as_slice() {
        [value] if !value.is_empty() => Ok(value),
        [] => Err("missing-required-field"),
        _ => Err("duplicate-required-field"),
    }
}

fn parse_architectures(input: &str) -> Result<BTreeSet<String>, &'static str> {
    let values: Vec<_> = input.split_ascii_whitespace().collect();
    if values.is_empty() {
        return Err("missing-architecture");
    }
    if values.len() > EXPECTED_ARCHITECTURES.len() {
        return Err("surplus-or-duplicate-architecture");
    }
    let architectures: BTreeSet<_> = values.iter().map(|value| (*value).to_owned()).collect();
    if architectures.len() != values.len() {
        return Err("duplicate-architecture");
    }
    if architectures.iter().any(|value| {
        !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }) {
        return Err("invalid-architecture-token");
    }
    Ok(architectures)
}

fn parse_dependencies(input: &str, executable: &Path) -> Result<BTreeSet<String>, &'static str> {
    let mut lines = input.lines();
    let Some(header) = lines.next() else {
        return Err("missing-header");
    };
    let expected_header = format!("{}:", executable.display());
    if header.trim() != expected_header {
        return Err("executable-header-mismatch");
    }
    let mut dependencies = BTreeSet::new();
    let mut count = 0_usize;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        count = count.checked_add(1).ok_or("dependency-count-overflow")?;
        if count > MAX_DEPENDENCIES {
            return Err("dependency-count-limit");
        }
        let Some((path, metadata)) = line.split_once(" (") else {
            return Err("malformed-dependency-line");
        };
        if path.is_empty() || !metadata.ends_with(')') || !metadata.contains("version") {
            return Err("malformed-dependency-line");
        }
        if !dependencies.insert(path.to_owned()) {
            return Err("duplicate-dependency");
        }
    }
    if dependencies.is_empty() {
        return Err("missing-dependency");
    }
    Ok(dependencies)
}

fn allowed_system_dependency(path: &str) -> bool {
    let path_value = Path::new(path);
    !contains_forbidden_engine(path.as_bytes())
        && (path.starts_with("/usr/lib/") || path.starts_with("/System/Library/Frameworks/"))
        && path_value.is_absolute()
        && path_value
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn parse_flat_plist(input: &str) -> Result<BTreeMap<String, ParsedPlistValue>, &'static str> {
    if input.matches("<plist").count() != 1 || input.matches("</plist>").count() != 1 {
        return Err("plist-envelope-count");
    }
    let plist_start = input.find("<plist").ok_or("missing-plist")?;
    let plist_open_end = input[plist_start..]
        .find('>')
        .map(|offset| plist_start + offset + 1)
        .ok_or("malformed-plist-open")?;
    let plist_end = input[plist_open_end..]
        .find("</plist>")
        .map(|offset| plist_open_end + offset)
        .ok_or("missing-plist-close")?;
    let body = &input[plist_open_end..plist_end];
    if body.matches("<dict>").count() != 1 || body.matches("</dict>").count() != 1 {
        return Err("dict-count");
    }
    let dict_start = body.find("<dict>").ok_or("missing-dict")? + "<dict>".len();
    let dict_end = body[dict_start..]
        .find("</dict>")
        .map(|offset| dict_start + offset)
        .ok_or("missing-dict-close")?;
    if !body[..dict_start - "<dict>".len()].trim().is_empty()
        || !body[dict_end + "</dict>".len()..].trim().is_empty()
    {
        return Err("unexpected-plist-container-content");
    }
    let mut remaining = &body[dict_start..dict_end];
    let mut values = BTreeMap::new();
    loop {
        remaining = remaining.trim_start();
        if remaining.is_empty() {
            break;
        }
        let (key, rest) = parse_xml_text_element(remaining, "key")?;
        remaining = rest.trim_start();
        let (value, rest) = if let Some(rest) = remaining.strip_prefix("<true/>") {
            (ParsedPlistValue::Boolean(true), rest)
        } else if let Some(rest) = remaining.strip_prefix("<true />") {
            (ParsedPlistValue::Boolean(true), rest)
        } else if let Some(rest) = remaining.strip_prefix("<false/>") {
            (ParsedPlistValue::Boolean(false), rest)
        } else if let Some(rest) = remaining.strip_prefix("<false />") {
            (ParsedPlistValue::Boolean(false), rest)
        } else if remaining.starts_with("<string>") {
            let (value, rest) = parse_xml_text_element(remaining, "string")?;
            (ParsedPlistValue::String(value), rest)
        } else {
            return Err("unsupported-plist-value");
        };
        if key.is_empty() {
            return Err("empty-plist-key");
        }
        if values.insert(key, value).is_some() {
            return Err("duplicate-plist-key");
        }
        remaining = rest;
    }
    Ok(values)
}

fn parse_xml_text_element<'a>(
    input: &'a str,
    name: &str,
) -> Result<(String, &'a str), &'static str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let Some(after_open) = input.strip_prefix(&open) else {
        return Err("missing-xml-element");
    };
    let Some(close_offset) = after_open.find(&close) else {
        return Err("missing-xml-element-close");
    };
    let encoded = &after_open[..close_offset];
    if encoded.contains('<') || encoded.contains('>') {
        return Err("nested-xml-text");
    }
    let decoded = decode_xml_text(encoded)?;
    Ok((decoded, &after_open[close_offset + close.len()..]))
}

fn decode_xml_text(input: &str) -> Result<String, &'static str> {
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(offset) = remaining.find('&') {
        output.push_str(&remaining[..offset]);
        let entity = &remaining[offset..];
        let (value, consumed) = if entity.starts_with("&amp;") {
            ('&', 5)
        } else if entity.starts_with("&lt;") {
            ('<', 4)
        } else if entity.starts_with("&gt;") {
            ('>', 4)
        } else if entity.starts_with("&quot;") {
            ('"', 6)
        } else if entity.starts_with("&apos;") {
            ('\'', 6)
        } else {
            return Err("unsupported-xml-entity");
        };
        output.push(value);
        remaining = &entity[consumed..];
    }
    output.push_str(remaining);
    Ok(output)
}

fn read_approval_record(
    path: &Path,
    violations: &mut Vec<MacosPackageViolation>,
) -> Option<ApprovalRecord> {
    let bytes = read_bounded_regular_file(
        path,
        MAX_APPROVAL_BYTES,
        "RPE-MACOS-PACKAGE-0002",
        "approval-record",
        violations,
    )?;
    let input = match String::from_utf8(bytes) {
        Ok(input) => input,
        Err(_) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0002",
                path.to_path_buf(),
                "approval-record-not-utf8",
            );
            return None;
        }
    };
    let assignments = match parse_exact_assignments(&input) {
        Ok(assignments) => assignments,
        Err(token) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0002",
                path.to_path_buf(),
                format!("malformed-approval-record={token}"),
            );
            return None;
        }
    };
    let expected_fields: BTreeSet<_> = [
        "schema",
        "scope",
        "app_version",
        "build_version",
        "team_identifier",
        "worker_sha256",
        "package_sha256",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if assignments.keys().cloned().collect::<BTreeSet<_>>() != expected_fields {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0002",
            path.to_path_buf(),
            "approval-record-field-set-drift",
        );
        return None;
    }
    if assignments.get("schema").map(String::as_str) != Some("1")
        || assignments.get("scope").map(String::as_str) != Some(APPROVAL_SCOPE)
    {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0002",
            path.to_path_buf(),
            "approval-record-schema-or-scope-drift",
        );
        return None;
    }
    let app_version = assignments.get("app_version")?.to_owned();
    let build_version = assignments.get("build_version")?.to_owned();
    let team_identifier = assignments.get("team_identifier")?.to_owned();
    let worker_sha256 = assignments.get("worker_sha256")?.to_owned();
    let package_sha256 = assignments.get("package_sha256")?.to_owned();
    if !valid_version(&app_version) || !valid_version(&build_version) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0002",
            path.to_path_buf(),
            "invalid-approved-version",
        );
    }
    if team_identifier.len() != 10
        || !team_identifier
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0002",
            path.to_path_buf(),
            "invalid-approved-team-identifier",
        );
    }
    for (name, value) in [
        ("worker", worker_sha256.as_str()),
        ("package", package_sha256.as_str()),
    ] {
        if !is_lower_sha256(value) {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0002",
                path.to_path_buf(),
                format!("invalid-approved-{name}-sha256"),
            );
        }
    }
    Some(ApprovalRecord {
        app_version,
        build_version,
        team_identifier,
        worker_sha256,
        package_sha256,
    })
}

fn parse_exact_assignments(input: &str) -> Result<BTreeMap<String, String>, &'static str> {
    let mut assignments = BTreeMap::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            return Err("comments-forbidden");
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err("missing-assignment");
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("invalid-key");
        }
        let value = if key == "schema" {
            if value != "1" {
                return Err("invalid-schema-value");
            }
            value.to_owned()
        } else {
            let Some(value) = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
            else {
                return Err("unquoted-string");
            };
            if value.is_empty()
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
            {
                return Err("invalid-string");
            }
            value.to_owned()
        };
        if assignments.insert(key.to_owned(), value).is_some() {
            return Err("duplicate-field");
        }
    }
    Ok(assignments)
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_app_root(app: &Path, violations: &mut Vec<MacosPackageViolation>) -> Option<PathBuf> {
    if app.file_name().and_then(OsStr::to_str) != Some(APP_BUNDLE_NAME) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.to_path_buf(),
            "app-bundle-basename-drift",
        );
        return None;
    }
    let metadata = match fs::symlink_metadata(app) {
        Ok(metadata) => metadata,
        Err(_) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.to_path_buf(),
                "missing-app-bundle",
            );
            return None;
        }
    };
    if metadata.file_type().is_symlink() {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.to_path_buf(),
            "app-bundle-symlink-forbidden",
        );
        return None;
    }
    if !metadata.is_dir() {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.to_path_buf(),
            "app-bundle-not-directory",
        );
        return None;
    }
    if entry_mode(&metadata) != 0o755 {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.to_path_buf(),
            "app-bundle-mode-drift",
        );
    }
    match fs::canonicalize(app) {
        Ok(path) => Some(path),
        Err(_) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.to_path_buf(),
                "app-bundle-canonicalization-failed",
            );
            None
        }
    }
}

fn collect_package_inventory(
    app: &Path,
    violations: &mut Vec<MacosPackageViolation>,
) -> Option<PackageInventory> {
    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    if !walk_package(app, app, &mut entries, &mut total_bytes, violations) {
        return None;
    }
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    debug_assert!(entries.len() <= MAX_PACKAGE_ENTRIES);
    debug_assert!(total_bytes <= MAX_PACKAGE_BYTES);

    let expected_directories: BTreeSet<_> = EXPECTED_DIRECTORIES.iter().copied().collect();
    let expected_files: BTreeSet<_> = EXPECTED_FILES.iter().copied().collect();
    let observed_directories: BTreeSet<_> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Directory)
        .map(|entry| entry.relative.as_str())
        .collect();
    let observed_files: BTreeSet<_> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| entry.relative.as_str())
        .collect();

    for path in expected_directories.difference(&observed_directories) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.join(path),
            "missing-package-directory",
        );
    }
    for path in observed_directories.difference(&expected_directories) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.join(path),
            "unexpected-package-directory",
        );
    }
    for path in expected_files.difference(&observed_files) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0001",
            app.join(path),
            "missing-package-file",
        );
    }
    for path in observed_files.difference(&expected_files) {
        let entry = entries
            .iter()
            .find(|entry| entry.relative == **path)
            .expect("observed file came from entries");
        let token = if entry.mode & 0o111 != 0 || has_macho_magic(&entry.bytes) {
            "unknown-package-executable"
        } else {
            "unexpected-package-file"
        };
        violation(violations, "RPE-MACOS-PACKAGE-0001", app.join(path), token);
    }

    for entry in &entries {
        if entry.kind == EntryKind::Directory {
            if entry.mode != 0o755 {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0001",
                    app.join(&entry.relative),
                    "package-directory-mode-drift",
                );
            }
            continue;
        }
        let executable = matches!(
            entry.relative.as_str(),
            HOST_EXECUTABLE_RELATIVE | WORKER_HELPER_RELATIVE
        );
        let expected_mode = if executable { 0o755 } else { 0o644 };
        if entry.mode != expected_mode {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.join(&entry.relative),
                "package-file-mode-drift",
            );
        }
        if executable && !has_fat_macho_magic(&entry.bytes) {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0007",
                app.join(&entry.relative),
                "missing-universal-mach-o-header",
            );
        }
        if contains_forbidden_engine(&entry.bytes)
            || contains_forbidden_engine(entry.relative.as_bytes())
        {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0008",
                app.join(&entry.relative),
                "forbidden-pdf-engine-package-content",
            );
        }
    }
    let worker = entries
        .iter()
        .find(|entry| entry.relative == WORKER_HELPER_RELATIVE)?;
    let observed_worker_sha256 = match sha256(&worker.bytes) {
        Ok(digest) => hex_digest(&digest),
        Err(_) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0003",
                app.join(WORKER_HELPER_RELATIVE),
                "worker-sha256-length-overflow",
            );
            return None;
        }
    };
    let observed_package_sha256 = match canonical_package_hash(&entries) {
        Ok(hash) => hash,
        Err(token) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0003",
                app.to_path_buf(),
                token,
            );
            return None;
        }
    };
    Some(PackageInventory {
        entries,
        observed_package_sha256,
        observed_worker_sha256,
    })
}

fn walk_package(
    app: &Path,
    directory: &Path,
    entries: &mut Vec<PackageEntry>,
    total_bytes: &mut u64,
    violations: &mut Vec<MacosPackageViolation>,
) -> bool {
    let iterator = match fs::read_dir(directory) {
        Ok(iterator) => iterator,
        Err(_) => {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                directory.to_path_buf(),
                "package-directory-read-failed",
            );
            return false;
        }
    };
    let mut children = Vec::new();
    for child in iterator {
        let child = match child {
            Ok(child) => child,
            Err(_) => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0001",
                    directory.to_path_buf(),
                    "package-entry-read-failed",
                );
                return false;
            }
        };
        children.push(child.path());
        if children.len() > MAX_PACKAGE_ENTRIES {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                directory.to_path_buf(),
                "package-directory-entry-limit",
            );
            return false;
        }
    }
    children.sort();
    for child in children {
        if entries.len() >= MAX_PACKAGE_ENTRIES {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.to_path_buf(),
                format!(
                    "package-entry-limit={}:max={MAX_PACKAGE_ENTRIES}",
                    entries.len().saturating_add(1)
                ),
            );
            return false;
        }
        let relative = match child.strip_prefix(app).ok().and_then(path_to_slash) {
            Some(relative) => relative,
            None => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0001",
                    child,
                    "non-utf8-or-invalid-package-path",
                );
                return false;
            }
        };
        if relative.is_empty()
            || relative.starts_with('/')
            || relative
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                child,
                "invalid-package-relative-path",
            );
            return false;
        }
        let metadata = match fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(_) => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0001",
                    child,
                    "package-entry-metadata-failed",
                );
                return false;
            }
        };
        if metadata.file_type().is_symlink() {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                child,
                "package-symlink-forbidden",
            );
            continue;
        }
        if metadata.is_dir() {
            entries.push(PackageEntry {
                relative,
                kind: EntryKind::Directory,
                mode: entry_mode(&metadata),
                bytes: Vec::new(),
            });
            if !walk_package(app, &child, entries, total_bytes, violations) {
                return false;
            }
            continue;
        }
        if !metadata.is_file() {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                child,
                "package-special-file-forbidden",
            );
            continue;
        }
        if file_link_count(&metadata) != 1 {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                child.clone(),
                "package-hardlink-forbidden",
            );
        }
        let maximum = if matches!(
            relative.as_str(),
            HOST_EXECUTABLE_RELATIVE | WORKER_HELPER_RELATIVE
        ) {
            MAX_EXECUTABLE_BYTES
        } else {
            MAX_METADATA_FILE_BYTES
        };
        if metadata.len() > maximum {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                child,
                format!("package-file-byte-limit={}:max={maximum}", metadata.len()),
            );
            continue;
        }
        let Some(projected_total) = total_bytes.checked_add(metadata.len()) else {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.to_path_buf(),
                "package-byte-count-overflow",
            );
            return false;
        };
        if projected_total > MAX_PACKAGE_BYTES {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0001",
                app.to_path_buf(),
                format!("package-byte-limit={projected_total}:max={MAX_PACKAGE_BYTES}"),
            );
            return false;
        }
        let bytes = match read_exact_metadata_size(&child, &metadata, maximum) {
            Ok(bytes) => bytes,
            Err(_) => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0001",
                    child,
                    "package-file-read-failed",
                );
                continue;
            }
        };
        *total_bytes = projected_total;
        entries.push(PackageEntry {
            relative,
            kind: EntryKind::File,
            mode: entry_mode(&metadata),
            bytes,
        });
    }
    true
}

fn path_to_slash(path: &Path) -> Option<String> {
    let mut output = String::new();
    for component in path.components() {
        let component = component.as_os_str().to_str()?;
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(component);
    }
    Some(output)
}

fn read_exact_metadata_size(path: &Path, metadata: &Metadata, maximum: u64) -> io::Result<Vec<u8>> {
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "file length does not fit usize")
    })?;
    let mut input = Vec::with_capacity(capacity);
    File::open(path)?
        .take(
            maximum.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "file limit overflow")
            })?,
        )
        .read_to_end(&mut input)?;
    if input.len() as u64 != metadata.len() || input.len() as u64 > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed while reading",
        ));
    }
    Ok(input)
}

fn read_bounded_regular_file(
    path: &Path,
    maximum: u64,
    code: &'static str,
    label: &str,
    violations: &mut Vec<MacosPackageViolation>,
) -> Option<Vec<u8>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            violation(
                violations,
                code,
                path.to_path_buf(),
                format!("missing-{label}"),
            );
            return None;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        violation(
            violations,
            code,
            path.to_path_buf(),
            format!("{label}-must-be-regular-file"),
        );
        return None;
    }
    if file_link_count(&metadata) != 1 {
        violation(
            violations,
            code,
            path.to_path_buf(),
            format!("{label}-hardlink-forbidden"),
        );
        return None;
    }
    if metadata.len() > maximum {
        violation(
            violations,
            code,
            path.to_path_buf(),
            format!("{label}-byte-limit={}:max={maximum}", metadata.len()),
        );
        return None;
    }
    match read_exact_metadata_size(path, &metadata, maximum) {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            violation(
                violations,
                code,
                path.to_path_buf(),
                format!("{label}-read-failed"),
            );
            None
        }
    }
}

fn canonical_package_hash(entries: &[PackageEntry]) -> Result<String, &'static str> {
    let mut hasher = Sha256::new();
    hasher
        .update(PACKAGE_HASH_DOMAIN)
        .map_err(|_| "package-sha256-length-overflow")?;
    for entry in entries {
        let path_len =
            u32::try_from(entry.relative.len()).map_err(|_| "package-path-length-overflow")?;
        hasher
            .update(&path_len.to_be_bytes())
            .and_then(|()| hasher.update(entry.relative.as_bytes()))
            .and_then(|()| {
                hasher.update(&[match entry.kind {
                    EntryKind::Directory => b'd',
                    EntryKind::File => b'f',
                }])
            })
            .and_then(|()| hasher.update(&entry.mode.to_be_bytes()))
            .map_err(|_| "package-sha256-length-overflow")?;
        let byte_len =
            u64::try_from(entry.bytes.len()).map_err(|_| "package-byte-length-overflow")?;
        hasher
            .update(&byte_len.to_be_bytes())
            .and_then(|()| hasher.update(&entry.bytes))
            .map_err(|_| "package-sha256-length-overflow")?;
    }
    hasher
        .finalize()
        .map(|digest| hex_digest(&digest))
        .map_err(|_| "package-sha256-length-overflow")
}

fn validate_inventory_anchors(
    app: &Path,
    approval: &ApprovalRecord,
    inventory: &PackageInventory,
    violations: &mut Vec<MacosPackageViolation>,
) {
    let worker = inventory
        .entries
        .iter()
        .find(|entry| entry.relative == WORKER_HELPER_RELATIVE)
        .expect("complete inventory owns Worker helper");
    if !contains_bytes(&worker.bytes, REQUIRED_FEATURE_MARKER) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0003",
            app.join(WORKER_HELPER_RELATIVE),
            "worker-missing-feature-free-marker",
        );
    }
    if contains_bytes(&worker.bytes, FORBIDDEN_FIXTURE_MARKER) {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0003",
            app.join(WORKER_HELPER_RELATIVE),
            "worker-forbidden-fixture-marker",
        );
    }
    if inventory.observed_worker_sha256 != approval.worker_sha256 {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0003",
            app.join(WORKER_HELPER_RELATIVE),
            format!(
                "worker-external-anchor-mismatch=observed:{}",
                inventory.observed_worker_sha256
            ),
        );
    }
    if inventory.observed_package_sha256 != approval.package_sha256 {
        violation(
            violations,
            "RPE-MACOS-PACKAGE-0003",
            app.to_path_buf(),
            format!(
                "package-external-anchor-mismatch=observed:{}",
                inventory.observed_package_sha256
            ),
        );
    }
}

fn contains_bytes(input: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && input.windows(needle.len()).any(|window| window == needle)
}

fn contains_forbidden_engine(input: &[u8]) -> bool {
    FORBIDDEN_ENGINES
        .iter()
        .any(|engine| contains_ascii_case_insensitive(input, engine))
}

fn contains_ascii_case_insensitive(input: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && input.windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle.iter())
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

fn has_macho_magic(input: &[u8]) -> bool {
    matches!(
        input.get(..4),
        Some(
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    )
}

fn has_fat_macho_magic(input: &[u8]) -> bool {
    matches!(
        input.get(..4),
        Some(
            [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    )
}

#[cfg(unix)]
fn entry_mode(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;

    metadata.mode() & 0o7777
}

#[cfg(not(unix))]
fn entry_mode(_metadata: &Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn file_link_count(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.nlink()
}

#[cfg(not(unix))]
fn file_link_count(_metadata: &Metadata) -> u64 {
    1
}

fn inspect_all(
    inspector: &dyn Inspector,
    requests: &[InspectionRequest; MAX_COMMANDS],
    violations: &mut Vec<MacosPackageViolation>,
) -> Option<RawObservation> {
    let mut outputs = BTreeMap::new();
    for request in requests {
        let output = match inspector.inspect(request) {
            Ok(output) => output,
            Err(_) => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0006",
                    request.target.clone(),
                    format!("inspection-command-unavailable={}", request.kind.token()),
                );
                continue;
            }
        };
        if output.timed_out {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                request.target.clone(),
                format!("inspection-command-timeout={}", request.kind.token()),
            );
            continue;
        }
        if output.output_too_large
            || output.stdout.len() as u64 > MAX_COMMAND_STREAM_BYTES
            || output.stderr.len() as u64 > MAX_COMMAND_STREAM_BYTES
        {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                request.target.clone(),
                format!("inspection-output-byte-limit={}", request.kind.token()),
            );
            continue;
        }
        if !output.success {
            let (code, token) = if matches!(
                request.kind,
                InspectionKind::VerifyDeepStrict
                    | InspectionKind::VerifyHostStrict
                    | InspectionKind::VerifyWorkerStrict
            ) {
                (
                    "RPE-MACOS-PACKAGE-0004",
                    format!("codesign-verification-failed={}", request.kind.token()),
                )
            } else {
                (
                    "RPE-MACOS-PACKAGE-0006",
                    format!("inspection-command-failed={}", request.kind.token()),
                )
            };
            violation(violations, code, request.target.clone(), token);
            continue;
        }
        let text = match combine_command_output(&output) {
            Ok(text) => text,
            Err(token) => {
                violation(
                    violations,
                    "RPE-MACOS-PACKAGE-0006",
                    request.target.clone(),
                    format!(
                        "invalid-inspection-output={}:{}",
                        request.kind.token(),
                        token
                    ),
                );
                continue;
            }
        };
        if outputs.insert(request.kind.token(), text).is_some() {
            violation(
                violations,
                "RPE-MACOS-PACKAGE-0006",
                request.target.clone(),
                format!("duplicate-inspection-output={}", request.kind.token()),
            );
        }
    }
    Some(RawObservation { outputs })
}

fn combine_command_output(output: &InspectionOutput) -> Result<String, &'static str> {
    let stdout = std::str::from_utf8(&output.stdout).map_err(|_| "stdout-not-utf8")?;
    let stderr = std::str::from_utf8(&output.stderr).map_err(|_| "stderr-not-utf8")?;
    if stdout.is_empty() {
        return Ok(stderr.to_owned());
    }
    if stderr.is_empty() {
        return Ok(stdout.to_owned());
    }
    Ok(format!("{stdout}\n{stderr}"))
}

impl Inspector for SystemInspector {
    fn inspect(&self, request: &InspectionRequest) -> io::Result<InspectionOutput> {
        let (program, arguments) = inspection_command(request);
        run_bounded_command(program, &arguments)
    }
}

fn inspection_command(request: &InspectionRequest) -> (&'static str, Vec<OsString>) {
    let target = request.target.as_os_str().to_owned();
    match request.kind {
        InspectionKind::Plist => (
            PLUTIL,
            strings(&["-convert", "xml1", "-o", "-"])
                .into_iter()
                .chain([target])
                .collect(),
        ),
        InspectionKind::VerifyDeepStrict => (
            CODESIGN,
            strings(&[
                "--verify",
                "--deep",
                "--strict",
                "--all-architectures",
                "--verbose=4",
            ])
            .into_iter()
            .chain([target])
            .collect(),
        ),
        InspectionKind::VerifyHostStrict | InspectionKind::VerifyWorkerStrict => (
            CODESIGN,
            strings(&["--verify", "--strict", "--all-architectures", "--verbose=4"])
                .into_iter()
                .chain([target])
                .collect(),
        ),
        InspectionKind::HostSigningMetadataArm64 | InspectionKind::WorkerSigningMetadataArm64 => (
            CODESIGN,
            strings(&["--display", "--architecture", "arm64", "--verbose=4"])
                .into_iter()
                .chain([target])
                .collect(),
        ),
        InspectionKind::HostSigningMetadataX86_64 | InspectionKind::WorkerSigningMetadataX86_64 => {
            (
                CODESIGN,
                strings(&["--display", "--architecture", "x86_64", "--verbose=4"])
                    .into_iter()
                    .chain([target])
                    .collect(),
            )
        }
        InspectionKind::HostEntitlementsArm64 | InspectionKind::WorkerEntitlementsArm64 => (
            CODESIGN,
            strings(&[
                "--display",
                "--architecture",
                "arm64",
                "--entitlements",
                "-",
                "--xml",
            ])
            .into_iter()
            .chain([target])
            .collect(),
        ),
        InspectionKind::HostEntitlementsX86_64 | InspectionKind::WorkerEntitlementsX86_64 => (
            CODESIGN,
            strings(&[
                "--display",
                "--architecture",
                "x86_64",
                "--entitlements",
                "-",
                "--xml",
            ])
            .into_iter()
            .chain([target])
            .collect(),
        ),
        InspectionKind::HostArchitectures | InspectionKind::WorkerArchitectures => (
            LIPO,
            strings(&["-archs"]).into_iter().chain([target]).collect(),
        ),
        InspectionKind::HostDependenciesArm64 | InspectionKind::WorkerDependenciesArm64 => (
            OTOOL,
            strings(&["-arch", "arm64", "-L"])
                .into_iter()
                .chain([target])
                .collect(),
        ),
        InspectionKind::HostDependenciesX86_64 | InspectionKind::WorkerDependenciesX86_64 => (
            OTOOL,
            strings(&["-arch", "x86_64", "-L"])
                .into_iter()
                .chain([target])
                .collect(),
        ),
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn run_bounded_command(program: &str, arguments: &[OsString]) -> io::Result<InspectionOutput> {
    debug_assert!(Path::new(program).is_absolute());
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err(io::Error::other("missing child stdout"));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return Err(io::Error::other("missing child stderr"));
    };
    let stdout_reader = spawn_bounded_reader(stdout);
    let stderr_reader = spawn_bounded_reader(stderr);
    let Some(deadline) = Instant::now().checked_add(COMMAND_TIMEOUT) else {
        terminate_child(&mut child);
        let _ = join_reader(stdout_reader);
        let _ = join_reader(stderr_reader);
        return Err(io::Error::other("command deadline overflow"));
    };
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait();
            }
            Err(error) => {
                terminate_child(&mut child);
                break Err(error);
            }
        }
    };
    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);
    let status = status?;
    let stdout = stdout?;
    let stderr = stderr?;
    let output_too_large = stdout.len() as u64 > MAX_COMMAND_STREAM_BYTES
        || stderr.len() as u64 > MAX_COMMAND_STREAM_BYTES;
    Ok(InspectionOutput {
        success: status.success() && !timed_out && !output_too_large,
        stdout,
        stderr,
        timed_out,
        output_too_large,
    })
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_bounded_reader(
    reader: impl Read + Send + 'static,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut output = Vec::new();
        reader
            .take(MAX_COMMAND_STREAM_BYTES + 1)
            .read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("command output reader panicked"))?
}

fn violation(
    violations: &mut Vec<MacosPackageViolation>,
    code: &'static str,
    path: PathBuf,
    token: impl Into<String>,
) {
    violations.push(MacosPackageViolation {
        code,
        path,
        token: token.into(),
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    const TEAM_IDENTIFIER: &str = "ABCDE12345";
    const APP_VERSION: &str = "0.1.0";
    const BUILD_VERSION: &str = "1";

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        repository: PathBuf,
        app: PathBuf,
        approval: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "pdf-rs-macos-package-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create unique fixture root");
            let repository = root.join("repository");
            let approval = repository.join(APPROVAL_RECORD_RELATIVE);
            fs::create_dir_all(
                approval
                    .parent()
                    .expect("approval record has parent directory"),
            )
            .expect("create approval directory");
            let app = root.join("artifact").join(APP_BUNDLE_NAME);
            for directory in [
                app.clone(),
                app.join("Contents"),
                app.join("Contents/Helpers"),
                app.join("Contents/MacOS"),
                app.join("Contents/_CodeSignature"),
            ] {
                fs::create_dir_all(&directory).expect("create package directory");
                set_mode(&directory, 0o755);
            }

            let mut host = vec![0xca, 0xfe, 0xba, 0xbe];
            host.extend_from_slice(b"PDF.rs host universal fixture");
            write_file(&app.join(HOST_EXECUTABLE_RELATIVE), &host, 0o755);

            let mut worker = vec![0xca, 0xfe, 0xba, 0xbe];
            worker.extend_from_slice(REQUIRED_FEATURE_MARKER);
            worker.extend_from_slice(b":worker universal fixture");
            write_file(&app.join(WORKER_HELPER_RELATIVE), &worker, 0o755);
            write_file(
                &app.join(INFO_PLIST_RELATIVE),
                b"fixture Info.plist bytes are independently hash-bound",
                0o644,
            );
            write_file(
                &app.join(CODE_RESOURCES_RELATIVE),
                b"fixture sealed resources",
                0o644,
            );

            let fixture = Self {
                root,
                repository,
                app,
                approval,
            };
            fixture.refresh_approval();
            fixture
        }

        fn canonical_app(&self) -> PathBuf {
            fs::canonicalize(&self.app).expect("canonical fixture app")
        }

        fn refresh_approval(&self) {
            let app = self.canonical_app();
            let mut violations = Vec::new();
            let inventory =
                collect_package_inventory(&app, &mut violations).expect("complete fixture");
            assert_eq!(violations, Vec::<MacosPackageViolation>::new());
            let record = format!(
                concat!(
                    "schema = 1\n",
                    "scope = \"external_release_trust_anchor\"\n",
                    "app_version = \"{}\"\n",
                    "build_version = \"{}\"\n",
                    "team_identifier = \"{}\"\n",
                    "worker_sha256 = \"{}\"\n",
                    "package_sha256 = \"{}\"\n",
                ),
                APP_VERSION,
                BUILD_VERSION,
                TEAM_IDENTIFIER,
                inventory.observed_worker_sha256,
                inventory.observed_package_sha256
            );
            write_file(&self.approval, record.as_bytes(), 0o644);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .expect("set fixture permissions");
    }

    fn write_file(path: &Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).expect("write fixture file");
        set_mode(path, mode);
    }

    fn info_plist() -> String {
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<plist version=\"1.0\"><dict>",
            "<key>CFBundleExecutable</key><string>PDF.rs</string>",
            "<key>CFBundleIdentifier</key><string>rs.pdf.desktop</string>",
            "<key>CFBundleName</key><string>PDF.rs</string>",
            "<key>CFBundlePackageType</key><string>APPL</string>",
            "<key>CFBundleShortVersionString</key><string>0.1.0</string>",
            "<key>CFBundleVersion</key><string>1</string>",
            "</dict></plist>",
        )
        .to_owned()
    }

    fn host_entitlements() -> String {
        concat!(
            "Executable=/fixture/PDF.rs\n",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<plist version=\"1.0\"><dict>",
            "<key>com.apple.security.app-sandbox</key><true/>",
            "<key>com.apple.security.files.user-selected.read-only</key><true/>",
            "</dict></plist>",
        )
        .to_owned()
    }

    fn worker_entitlements() -> String {
        concat!(
            "Executable=/fixture/pdf-rs-desktop-worker\n",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<plist version=\"1.0\"><dict>",
            "<key>com.apple.security.app-sandbox</key><true/>",
            "<key>com.apple.security.inherit</key><true/>",
            "</dict></plist>",
        )
        .to_owned()
    }

    fn signing_metadata(executable: &Path, identifier: &str, authorities: &[&str]) -> String {
        let mut output = format!(
            concat!(
                "Executable={}\n",
                "Identifier={}\n",
                "CodeDirectory v=20500 size=256 flags=0x10000(runtime) ",
                "hashes=4+2 location=embedded\n",
                "Signature size=4096\n",
            ),
            executable.display(),
            identifier
        );
        for authority in authorities {
            output.push_str("Authority=");
            output.push_str(authority);
            output.push('\n');
        }
        output.push_str("TeamIdentifier=");
        output.push_str(TEAM_IDENTIFIER);
        output.push('\n');
        output
    }

    fn dependencies(executable: &Path, dependency: &str) -> String {
        format!(
            "{}:\n\t{dependency} (compatibility version 1.0.0, current version 1.0.0)\n",
            executable.display()
        )
    }

    struct FakeInspector {
        app: PathBuf,
        overrides: RefCell<BTreeMap<&'static str, InspectionOutput>>,
        calls: RefCell<Vec<InspectionKind>>,
        mutation_path: Option<PathBuf>,
        mutated: Cell<bool>,
    }

    impl FakeInspector {
        fn new(app: &Path) -> Self {
            Self {
                app: fs::canonicalize(app).expect("canonical inspector app"),
                overrides: RefCell::new(BTreeMap::new()),
                calls: RefCell::new(Vec::new()),
                mutation_path: None,
                mutated: Cell::new(false),
            }
        }

        fn mutating(app: &Path, mutation_path: PathBuf) -> Self {
            Self {
                mutation_path: Some(mutation_path),
                ..Self::new(app)
            }
        }

        fn override_text(&self, kind: InspectionKind, text: impl Into<String>) {
            self.overrides
                .borrow_mut()
                .insert(kind.token(), successful_output(text.into()));
        }

        fn override_failure(&self, kind: InspectionKind) {
            self.overrides.borrow_mut().insert(
                kind.token(),
                InspectionOutput {
                    success: false,
                    stdout: Vec::new(),
                    stderr: b"rejected".to_vec(),
                    timed_out: false,
                    output_too_large: false,
                },
            );
        }

        fn override_timeout(&self, kind: InspectionKind) {
            self.overrides.borrow_mut().insert(
                kind.token(),
                InspectionOutput {
                    success: false,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: true,
                    output_too_large: false,
                },
            );
        }

        fn override_too_large(&self, kind: InspectionKind) {
            self.overrides.borrow_mut().insert(
                kind.token(),
                InspectionOutput {
                    success: false,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: false,
                    output_too_large: true,
                },
            );
        }

        fn calls(&self) -> Vec<InspectionKind> {
            self.calls.borrow().clone()
        }

        fn default_output(&self, request: &InspectionRequest) -> InspectionOutput {
            let host = self.app.join(HOST_EXECUTABLE_RELATIVE);
            let worker = self.app.join(WORKER_HELPER_RELATIVE);
            let authorities = [
                "Developer ID Application: PDF.rs",
                "Developer ID Certification Authority",
                "Apple Root CA",
            ];
            let text = match request.kind {
                InspectionKind::Plist => info_plist(),
                InspectionKind::VerifyDeepStrict
                | InspectionKind::VerifyHostStrict
                | InspectionKind::VerifyWorkerStrict => String::new(),
                InspectionKind::HostSigningMetadataArm64
                | InspectionKind::HostSigningMetadataX86_64 => {
                    signing_metadata(&host, HOST_IDENTIFIER, &authorities)
                }
                InspectionKind::WorkerSigningMetadataArm64
                | InspectionKind::WorkerSigningMetadataX86_64 => {
                    signing_metadata(&worker, WORKER_IDENTIFIER, &authorities)
                }
                InspectionKind::HostEntitlementsArm64 | InspectionKind::HostEntitlementsX86_64 => {
                    host_entitlements()
                }
                InspectionKind::WorkerEntitlementsArm64
                | InspectionKind::WorkerEntitlementsX86_64 => worker_entitlements(),
                InspectionKind::HostArchitectures | InspectionKind::WorkerArchitectures => {
                    "arm64 x86_64\n".to_owned()
                }
                InspectionKind::HostDependenciesArm64 | InspectionKind::HostDependenciesX86_64 => {
                    dependencies(&host, "/usr/lib/libSystem.B.dylib")
                }
                InspectionKind::WorkerDependenciesArm64
                | InspectionKind::WorkerDependenciesX86_64 => {
                    dependencies(&worker, "/usr/lib/libSystem.B.dylib")
                }
            };
            successful_output(text)
        }
    }

    impl Inspector for FakeInspector {
        fn inspect(&self, request: &InspectionRequest) -> io::Result<InspectionOutput> {
            self.calls.borrow_mut().push(request.kind);
            let output = self
                .overrides
                .borrow()
                .get(request.kind.token())
                .cloned()
                .unwrap_or_else(|| self.default_output(request));
            if request.kind == InspectionKind::WorkerDependenciesX86_64
                && !self.mutated.replace(true)
                && let Some(path) = &self.mutation_path
            {
                fs::write(path, b"fixture resources changed while inspecting")?;
            }
            Ok(output)
        }
    }

    fn successful_output(text: String) -> InspectionOutput {
        InspectionOutput {
            success: true,
            stdout: text.into_bytes(),
            stderr: Vec::new(),
            timed_out: false,
            output_too_large: false,
        }
    }

    fn verify(
        fixture: &Fixture,
        inspector: &FakeInspector,
    ) -> Result<MacosPackageReport, Vec<MacosPackageViolation>> {
        verify_macos_package_with(&fixture.repository, &fixture.app, inspector)
    }

    fn tokens(result: Result<MacosPackageReport, Vec<MacosPackageViolation>>) -> Vec<String> {
        result
            .expect_err("verification must fail")
            .into_iter()
            .map(|violation| violation.token)
            .collect()
    }

    fn has_token(tokens: &[String], expected: &str) -> bool {
        tokens.iter().any(|token| token == expected)
    }

    fn has_token_prefix(tokens: &[String], expected: &str) -> bool {
        tokens.iter().any(|token| token.starts_with(expected))
    }

    #[test]
    fn exact_valid_package_passes_all_eighteen_observations() {
        let fixture = Fixture::new();
        let inspector = FakeInspector::new(&fixture.app);
        let report = verify(&fixture, &inspector).expect("valid package report");

        assert_eq!(report.package_files, 4);
        assert_eq!(report.package_executables, 2);
        assert_eq!(
            report.observed_worker_sha256,
            report.worker_external_trust_anchor_sha256
        );
        assert_eq!(
            report.observed_package_sha256,
            report.package_external_trust_anchor_sha256
        );
        assert_eq!(report.signing_team_identifier, TEAM_IDENTIFIER);
        assert_eq!(inspector.calls(), InspectionKind::ALL);
    }

    #[test]
    fn missing_or_mismatched_external_anchor_stops_before_system_inspection() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.approval).expect("remove approval");
        let inspector = FakeInspector::new(&fixture.app);
        let missing = tokens(verify(&fixture, &inspector));
        assert!(has_token(&missing, "missing-approval-record"));
        assert!(inspector.calls().is_empty());

        fixture.refresh_approval();
        let worker = fixture.app.join(WORKER_HELPER_RELATIVE);
        let mut replacement = vec![0xca, 0xfe, 0xba, 0xbe];
        replacement.extend_from_slice(REQUIRED_FEATURE_MARKER);
        replacement.extend_from_slice(b":crafted paired substitution");
        write_file(&worker, &replacement, 0o755);
        let inspector = FakeInspector::new(&fixture.app);
        let mismatch = tokens(verify(&fixture, &inspector));
        assert!(has_token_prefix(
            &mismatch,
            "worker-external-anchor-mismatch="
        ));
        assert!(has_token_prefix(
            &mismatch,
            "package-external-anchor-mismatch="
        ));
        assert!(inspector.calls().is_empty());
    }

    #[test]
    fn externally_anchored_package_still_requires_feature_marker_and_native_purity() {
        {
            let fixture = Fixture::new();
            write_file(
                &fixture.app.join(WORKER_HELPER_RELATIVE),
                &[&[0xca, 0xfe, 0xba, 0xbe][..], &b"worker without marker"[..]].concat(),
                0o755,
            );
            fixture.refresh_approval();
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "worker-missing-feature-free-marker"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            let worker = [
                &[0xca, 0xfe, 0xba, 0xbe],
                REQUIRED_FEATURE_MARKER,
                FORBIDDEN_FIXTURE_MARKER,
            ]
            .concat();
            write_file(&fixture.app.join(WORKER_HELPER_RELATIVE), &worker, 0o755);
            fixture.refresh_approval();
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "worker-forbidden-fixture-marker"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            let host = [
                &[0xca, 0xfe, 0xba, 0xbe][..],
                &b"host with forbidden pdfium dependency marker"[..],
            ]
            .concat();
            write_file(&fixture.app.join(HOST_EXECUTABLE_RELATIVE), &host, 0o755);
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "forbidden-pdf-engine-package-content"));
            assert!(inspector.calls().is_empty());
        }
    }

    #[test]
    fn package_inventory_rejects_symlinks_hardlinks_unknown_files_and_special_modes() {
        {
            let fixture = Fixture::new();
            write_file(
                &fixture.app.join("Contents/unknown-executable"),
                &[0xca, 0xfe, 0xba, 0xbe],
                0o755,
            );
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "unknown-package-executable"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            set_mode(&fixture.app.join(WORKER_HELPER_RELATIVE), 0o1755);
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "package-file-mode-drift"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            let resources = fixture.app.join(CODE_RESOURCES_RELATIVE);
            fs::remove_file(&resources).expect("remove resources");
            symlink(fixture.app.join(INFO_PLIST_RELATIVE), &resources)
                .expect("install package symlink");
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "package-symlink-forbidden"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            let resources = fixture.app.join(CODE_RESOURCES_RELATIVE);
            fs::remove_file(&resources).expect("remove resources");
            fs::hard_link(fixture.app.join(INFO_PLIST_RELATIVE), &resources)
                .expect("install package hardlink");
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(&observed, "package-hardlink-forbidden"));
            assert!(inspector.calls().is_empty());
        }
        {
            let fixture = Fixture::new();
            for index in 0..9 {
                write_file(
                    &fixture
                        .app
                        .join("Contents")
                        .join(format!("extra-{index:02}")),
                    b"bounded extra entry",
                    0o644,
                );
            }
            let inspector = FakeInspector::new(&fixture.app);
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token_prefix(&observed, "package-entry-limit="));
            assert!(inspector.calls().is_empty());
        }
    }

    #[test]
    fn package_change_during_external_inspection_is_rejected() {
        let fixture = Fixture::new();
        let inspector =
            FakeInspector::mutating(&fixture.app, fixture.app.join(CODE_RESOURCES_RELATIVE));
        let observed = tokens(verify(&fixture, &inspector));
        assert!(has_token(&observed, "package-changed-during-inspection"));
        assert_eq!(inspector.calls(), InspectionKind::ALL);
    }

    #[test]
    fn signing_metadata_requires_exact_ordered_chain_across_slices_and_binaries() {
        let fixture = Fixture::new();
        let inspector = FakeInspector::new(&fixture.app);
        let worker = fixture.canonical_app().join(WORKER_HELPER_RELATIVE);
        inspector.override_text(
            InspectionKind::WorkerSigningMetadataX86_64,
            signing_metadata(
                &worker,
                WORKER_IDENTIFIER,
                &[
                    "Apple Root CA",
                    "Developer ID Certification Authority",
                    "Developer ID Application: PDF.rs",
                ],
            ),
        );
        let observed = tokens(verify(&fixture, &inspector));
        assert!(has_token(&observed, "worker-signing-metadata-slice-drift"));

        let fixture = Fixture::new();
        let inspector = FakeInspector::new(&fixture.app);
        let worker = fixture.canonical_app().join(WORKER_HELPER_RELATIVE);
        inspector.override_text(
            InspectionKind::WorkerSigningMetadataArm64,
            signing_metadata(
                &worker,
                WORKER_IDENTIFIER,
                &["Different Leaf", "Different Intermediate", "Different Root"],
            ),
        );
        inspector.override_text(
            InspectionKind::WorkerSigningMetadataX86_64,
            signing_metadata(
                &worker,
                WORKER_IDENTIFIER,
                &["Different Leaf", "Different Intermediate", "Different Root"],
            ),
        );
        let observed = tokens(verify(&fixture, &inspector));
        assert!(has_token(&observed, "host-worker-authority-chain-drift"));
    }

    #[test]
    fn entitlements_architectures_and_dependencies_are_exact_per_slice() {
        {
            let fixture = Fixture::new();
            let inspector = FakeInspector::new(&fixture.app);
            inspector.override_text(
                InspectionKind::WorkerEntitlementsX86_64,
                concat!(
                    "<plist version=\"1.0\"><dict>",
                    "<key>com.apple.security.app-sandbox</key><true/>",
                    "<key>com.apple.security.inherit</key><true/>",
                    "<key>com.apple.security.network.client</key><true/>",
                    "</dict></plist>",
                ),
            );
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(
                &observed,
                "entitlement-drift=worker-entitlements-x86_64"
            ));
        }
        {
            let fixture = Fixture::new();
            let inspector = FakeInspector::new(&fixture.app);
            inspector.override_text(InspectionKind::HostArchitectures, "arm64\n");
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(
                &observed,
                "architecture-set-drift=host-architectures"
            ));
        }
        {
            let fixture = Fixture::new();
            let inspector = FakeInspector::new(&fixture.app);
            let host = fixture.canonical_app().join(HOST_EXECUTABLE_RELATIVE);
            inspector.override_text(
                InspectionKind::HostDependenciesArm64,
                dependencies(&host, "@rpath/libunapproved.dylib"),
            );
            let observed = tokens(verify(&fixture, &inspector));
            assert!(has_token(
                &observed,
                "unapproved-dynamic-dependency=@rpath/libunapproved.dylib"
            ));
        }
    }

    #[test]
    fn command_failure_timeout_and_output_limit_are_distinct_fail_closed_states() {
        let fixture = Fixture::new();
        let inspector = FakeInspector::new(&fixture.app);
        inspector.override_failure(InspectionKind::VerifyDeepStrict);
        inspector.override_timeout(InspectionKind::HostArchitectures);
        inspector.override_too_large(InspectionKind::WorkerDependenciesX86_64);
        let observed = tokens(verify(&fixture, &inspector));
        assert!(has_token(
            &observed,
            "codesign-verification-failed=codesign-deep-strict"
        ));
        assert!(has_token(
            &observed,
            "inspection-command-timeout=host-architectures"
        ));
        assert!(has_token(
            &observed,
            "inspection-output-byte-limit=worker-dependencies-x86_64"
        ));
    }

    #[test]
    fn parsers_reject_ad_hoc_signing_surplus_plist_and_unsafe_dependencies() {
        assert_eq!(
            parse_signing_metadata(
                "Executable=/tmp/x\nIdentifier=x\nCodeDirectory flags=0x2(adhoc)\n\
                 Signature size=1\nAuthority=A\nTeamIdentifier=ABCDE12345\n"
            ),
            Err("ad-hoc-signature")
        );
        assert_eq!(
            parse_flat_plist("<plist><dict><key>a</key><true/><key>a</key><true/></dict></plist>"),
            Err("duplicate-plist-key")
        );
        assert_eq!(
            parse_architectures("arm64 arm64"),
            Err("duplicate-architecture")
        );
        assert!(!allowed_system_dependency("/usr/lib/example/.."));
        assert!(!allowed_system_dependency(
            "/System/Library/Frameworks/../Private.framework/Private"
        ));
        assert!(!allowed_system_dependency("/usr/lib/libpdfium.dylib"));
        assert!(allowed_system_dependency("/usr/lib/libSystem.B.dylib"));
    }

    #[test]
    fn inspection_commands_use_absolute_tools_and_explicit_architecture_scope() {
        let target = PathBuf::from("/tmp/PDF.rs.app");
        for kind in InspectionKind::ALL {
            let request = InspectionRequest {
                kind,
                target: target.clone(),
            };
            let (program, arguments) = inspection_command(&request);
            assert!(Path::new(program).is_absolute());
            assert_eq!(arguments.last(), Some(&target.as_os_str().to_owned()));
            let arguments: Vec<_> = arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            match kind {
                InspectionKind::VerifyDeepStrict
                | InspectionKind::VerifyHostStrict
                | InspectionKind::VerifyWorkerStrict => {
                    assert!(arguments.iter().any(|value| value == "--all-architectures"));
                }
                InspectionKind::HostSigningMetadataArm64
                | InspectionKind::WorkerSigningMetadataArm64
                | InspectionKind::HostEntitlementsArm64
                | InspectionKind::WorkerEntitlementsArm64
                | InspectionKind::HostDependenciesArm64
                | InspectionKind::WorkerDependenciesArm64 => {
                    assert!(arguments.iter().any(|value| value == "arm64"));
                }
                InspectionKind::HostSigningMetadataX86_64
                | InspectionKind::WorkerSigningMetadataX86_64
                | InspectionKind::HostEntitlementsX86_64
                | InspectionKind::WorkerEntitlementsX86_64
                | InspectionKind::HostDependenciesX86_64
                | InspectionKind::WorkerDependenciesX86_64 => {
                    assert!(arguments.iter().any(|value| value == "x86_64"));
                }
                InspectionKind::Plist
                | InspectionKind::HostArchitectures
                | InspectionKind::WorkerArchitectures => {}
            }
        }
    }
}
