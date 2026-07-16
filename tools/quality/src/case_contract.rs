use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use pdf_rs_compare::{PixelArtifactDecodeError, decode_canonical_pixel_artifact};
use pdf_rs_digest::{hex_digest, sha256};

use crate::manifest::{CaseManifest, ManifestDiagnostic, validate_manifest_file};

const MAX_DERIVATION_BYTES: u64 = 64 * 1024;
const MAX_REFERENCE_IDENTITY_BYTES: u64 = 16 * 1024;
const MAX_REVIEW_EVIDENCE_BYTES: u64 = 64 * 1024;
const PIXEL_JSON_OVERHEAD_BYTES: u64 = 128;

/// A stable failure while validating one case manifest and its linked artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaseContractDiagnostic {
    /// The case manifest itself failed canonical structural validation.
    Manifest(ManifestDiagnostic),
    /// A linked case file failed a stable read-only contract check.
    Linked {
        /// Stable `RPE-CASE-*` diagnostic code.
        code: &'static str,
        /// Manifest section that owns the linked file.
        section: &'static str,
        /// Manifest key that names or hashes the linked file.
        key: &'static str,
        /// Resolved repository-local path, when resolution reached a concrete path.
        path: PathBuf,
    },
}

impl CaseContractDiagnostic {
    fn linked(code: &'static str, section: &'static str, key: &'static str, path: PathBuf) -> Self {
        Self::Linked {
            code,
            section,
            key,
            path,
        }
    }

    /// Returns the stable diagnostic code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Manifest(diagnostic) => diagnostic.code,
            Self::Linked { code, .. } => code,
        }
    }
}

impl fmt::Display for CaseContractDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(diagnostic) => diagnostic.fmt(formatter),
            Self::Linked {
                code,
                section,
                key,
                path,
            } => write!(
                formatter,
                "{code} section={section} key={key} path={}",
                path.display()
            ),
        }
    }
}

/// Validates one canonical case manifest and every linked pixel-oracle artifact.
///
/// Validation is read-only. Linked files must remain below their manifest-derived preflight
/// limits, resolve entirely within the case directory without symlinks, match their declared
/// SHA-256 digests, and use their exact canonical encodings.
///
/// # Errors
///
/// Returns stable manifest or linked-artifact diagnostics. No expected file is created,
/// overwritten, renamed, or otherwise mutated.
pub fn validate_case_file(path: &Path) -> Result<CaseManifest, Vec<CaseContractDiagnostic>> {
    reject_manifest_symlink(path)?;
    let manifest = validate_manifest_file(path).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(CaseContractDiagnostic::Manifest)
            .collect::<Vec<_>>()
    })?;
    let case_directory = path.parent().unwrap_or_else(|| Path::new("."));
    let mut diagnostics = Vec::new();

    if let (Some(pixel_path), Some(pixel_hash)) = (
        manifest.string("expected", "pixel_artifact"),
        manifest.string("expected", "pixel_sha256"),
    ) {
        validate_pixel_artifact(
            &manifest,
            case_directory,
            pixel_path,
            pixel_hash,
            &mut diagnostics,
        );
    }

    if let Some(derivation) = manifest.string("pixel_oracle", "derivation") {
        validate_derivation(case_directory, derivation, &mut diagnostics);
    }

    if let Some(reference_identity) = manifest.string("pixel_oracle", "reference_identity") {
        validate_reference_identity(case_directory, reference_identity, &mut diagnostics);
    }

    if let (Some(evidence_path), Some(evidence_hash)) = (
        manifest.string("pixel_oracle", "review_evidence"),
        manifest.string("pixel_oracle", "review_evidence_sha256"),
    ) {
        validate_review_evidence(
            &manifest,
            case_directory,
            evidence_path,
            evidence_hash,
            &mut diagnostics,
        );
    }

    if diagnostics.is_empty() {
        Ok(manifest)
    } else {
        Err(diagnostics)
    }
}

fn reject_manifest_symlink(path: &Path) -> Result<(), Vec<CaseContractDiagnostic>> {
    if path_components_are_not_symlinks(path)
        && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
    {
        Ok(())
    } else {
        Err(vec![CaseContractDiagnostic::linked(
            "RPE-CASE-0001",
            "root",
            "manifest",
            path.to_path_buf(),
        )])
    }
}

fn validate_pixel_artifact(
    manifest: &CaseManifest,
    case_directory: &Path,
    relative_path: &str,
    expected_hash: &str,
    diagnostics: &mut Vec<CaseContractDiagnostic>,
) {
    let Some(max_pixels) = manifest.positive_u64("budget", "max_image_pixels") else {
        return;
    };
    let Some(max_rgba_bytes_u64) = max_pixels.checked_mul(4) else {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0002",
            "budget",
            "max_image_pixels",
            case_directory.to_path_buf(),
        ));
        return;
    };
    let Some(max_file_bytes) = max_rgba_bytes_u64
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(PIXEL_JSON_OVERHEAD_BYTES))
    else {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0002",
            "budget",
            "max_image_pixels",
            case_directory.to_path_buf(),
        ));
        return;
    };
    let Ok(max_rgba_bytes) = usize::try_from(max_rgba_bytes_u64) else {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0002",
            "budget",
            "max_image_pixels",
            case_directory.to_path_buf(),
        ));
        return;
    };
    let Some(bytes) = read_linked_file(
        case_directory,
        relative_path,
        max_file_bytes,
        "expected",
        "pixel_artifact",
        diagnostics,
    ) else {
        return;
    };
    let resolved = case_directory.join(relative_path);
    if !hash_matches(&bytes, expected_hash) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0005",
            "expected",
            "pixel_sha256",
            resolved,
        ));
        return;
    }
    let artifact = match decode_canonical_pixel_artifact(&bytes, max_rgba_bytes) {
        Ok(artifact) => artifact,
        Err(PixelArtifactDecodeError::RgbaLimitExceeded { .. }) => {
            diagnostics.push(CaseContractDiagnostic::linked(
                "RPE-CASE-0008",
                "budget",
                "max_image_pixels",
                resolved,
            ));
            return;
        }
        Err(_) => {
            diagnostics.push(CaseContractDiagnostic::linked(
                "RPE-CASE-0006",
                "expected",
                "pixel_artifact",
                resolved,
            ));
            return;
        }
    };
    let width = manifest
        .positive_u64("render", "width")
        .and_then(|value| u32::try_from(value).ok());
    let height = manifest
        .positive_u64("render", "height")
        .and_then(|value| u32::try_from(value).ok());
    if width != Some(artifact.width()) || height != Some(artifact.height()) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0007",
            "expected",
            "pixel_artifact",
            resolved,
        ));
    }
    let rendered_pixels = u64::from(artifact.width()) * u64::from(artifact.height());
    if rendered_pixels > max_pixels {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0008",
            "budget",
            "max_image_pixels",
            case_directory.join(relative_path),
        ));
    }
}

fn validate_derivation(
    case_directory: &Path,
    content_reference: &str,
    diagnostics: &mut Vec<CaseContractDiagnostic>,
) {
    let Some((relative_path, digest)) = content_reference.rsplit_once("#sha256:") else {
        return;
    };
    let expected_hash = format!("sha256:{digest}");
    let Some(bytes) = read_linked_file(
        case_directory,
        relative_path,
        MAX_DERIVATION_BYTES,
        "pixel_oracle",
        "derivation",
        diagnostics,
    ) else {
        return;
    };
    if bytes.is_empty() || !hash_matches(&bytes, &expected_hash) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0009",
            "pixel_oracle",
            "derivation",
            case_directory.join(relative_path),
        ));
    }
}

fn validate_reference_identity(
    case_directory: &Path,
    content_reference: &str,
    diagnostics: &mut Vec<CaseContractDiagnostic>,
) {
    let Some((relative_path, digest)) = content_reference.rsplit_once("#sha256:") else {
        return;
    };
    let expected_hash = format!("sha256:{digest}");
    let Some(bytes) = read_linked_file(
        case_directory,
        relative_path,
        MAX_REFERENCE_IDENTITY_BYTES,
        "pixel_oracle",
        "reference_identity",
        diagnostics,
    ) else {
        return;
    };
    let resolved = case_directory.join(relative_path);
    if !hash_matches(&bytes, &expected_hash) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0012",
            "pixel_oracle",
            "reference_identity",
            resolved,
        ));
        return;
    }
    if !is_canonical_reference_identity(&bytes) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0013",
            "pixel_oracle",
            "reference_identity",
            case_directory.join(relative_path),
        ));
    }
}

fn validate_review_evidence(
    manifest: &CaseManifest,
    case_directory: &Path,
    relative_path: &str,
    expected_hash: &str,
    diagnostics: &mut Vec<CaseContractDiagnostic>,
) {
    let Some(bytes) = read_linked_file(
        case_directory,
        relative_path,
        MAX_REVIEW_EVIDENCE_BYTES,
        "pixel_oracle",
        "review_evidence",
        diagnostics,
    ) else {
        return;
    };
    let resolved = case_directory.join(relative_path);
    if !hash_matches(&bytes, expected_hash) {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0010",
            "pixel_oracle",
            "review_evidence_sha256",
            resolved,
        ));
        return;
    }
    if bytes != canonical_review_evidence(manifest).as_bytes() {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0011",
            "pixel_oracle",
            "review_evidence",
            case_directory.join(relative_path),
        ));
    }
}

fn canonical_review_evidence(manifest: &CaseManifest) -> String {
    let reviewers = manifest
        .string_array("pixel_oracle", "reviewers")
        .unwrap_or_default();
    let mut output = String::new();
    output.push_str("{\"case_id\":\"");
    output.push_str(manifest.case_id());
    output.push_str("\",\"derivation\":\"");
    output.push_str(
        manifest
            .string("pixel_oracle", "derivation")
            .unwrap_or_default(),
    );
    output.push_str("\",\"independent\":true,\"pixel_reference\":\"");
    output.push_str(
        manifest
            .string("expected", "pixel_artifact")
            .unwrap_or_default(),
    );
    output.push('#');
    output.push_str(
        manifest
            .string("expected", "pixel_sha256")
            .unwrap_or_default(),
    );
    output.push_str("\",\"reference_identity\":\"");
    output.push_str(
        manifest
            .string("pixel_oracle", "reference_identity")
            .unwrap_or_default(),
    );
    output.push_str("\",\"reviewers\":[");
    for (index, reviewer) in reviewers.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(reviewer);
        output.push('"');
    }
    output.push_str("],\"schema\":1,\"verdict\":\"pass\"}");
    output
}

fn read_linked_file(
    case_directory: &Path,
    relative_path: &str,
    max_bytes: u64,
    section: &'static str,
    key: &'static str,
    diagnostics: &mut Vec<CaseContractDiagnostic>,
) -> Option<Vec<u8>> {
    let resolved = match resolve_without_symlinks(case_directory, relative_path) {
        Ok(path) => path,
        Err(path) => {
            diagnostics.push(CaseContractDiagnostic::linked(
                "RPE-CASE-0003",
                section,
                key,
                path,
            ));
            return None;
        }
    };
    let metadata = match fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            diagnostics.push(CaseContractDiagnostic::linked(
                "RPE-CASE-0003",
                section,
                key,
                resolved,
            ));
            return None;
        }
    };
    if metadata.len() > max_bytes {
        diagnostics.push(CaseContractDiagnostic::linked(
            "RPE-CASE-0004",
            section,
            key,
            resolved,
        ));
        return None;
    }
    match fs::read(&resolved) {
        Ok(bytes) if u64::try_from(bytes.len()).is_ok_and(|length| length <= max_bytes) => {
            Some(bytes)
        }
        _ => {
            diagnostics.push(CaseContractDiagnostic::linked(
                "RPE-CASE-0003",
                section,
                key,
                resolved,
            ));
            None
        }
    }
}

fn resolve_without_symlinks(
    case_directory: &Path,
    relative_path: &str,
) -> Result<PathBuf, PathBuf> {
    if !path_components_are_not_symlinks(case_directory) {
        return Err(case_directory.to_path_buf());
    }
    let mut resolved = case_directory.to_path_buf();
    for component in Path::new(relative_path).components() {
        let Component::Normal(component) = component else {
            return Err(resolved.join(component.as_os_str()));
        };
        resolved.push(component);
        let metadata = fs::symlink_metadata(&resolved).map_err(|_| resolved.clone())?;
        if metadata.file_type().is_symlink() {
            return Err(resolved);
        }
    }
    Ok(resolved)
}

fn path_components_are_not_symlinks(path: &Path) -> bool {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        resolved.push(component.as_os_str());
        if matches!(component, Component::Normal(_))
            && fs::symlink_metadata(&resolved)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return false;
        }
    }
    true
}

fn hash_matches(bytes: &[u8], expected: &str) -> bool {
    let Ok(digest) = sha256(bytes) else {
        return false;
    };
    expected
        .strip_prefix("sha256:")
        .is_some_and(|expected| hex_digest(&digest) == expected)
}

fn is_canonical_reference_identity(bytes: &[u8]) -> bool {
    const PREFIX: &[u8] =
        b"{\"algorithm\":\"reference-raster-v1\",\"implementation_sha256\":\"sha256:";
    const SUFFIX: &[u8] = b"\",\"schema\":1}";
    let Some(remainder) = bytes.strip_prefix(PREFIX) else {
        return false;
    };
    let Some(digest) = remainder.strip_suffix(SUFFIX) else {
        return false;
    };
    digest.len() == 64
        && digest
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}
