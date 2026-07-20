use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::{Component, Path, PathBuf};

use pdf_rs_digest::{Sha256, hex_digest, sha256};

use crate::{
    AccessPolicy, CorpusEntry, CorpusErrorCode, CorpusManifest, CorpusObjectId, CorpusTier,
    LicenseRecord, RedistributionPolicy, is_stable_id,
};

/// Schema version of the canonical on-disk corpus manifest.
pub const CORPUS_MANIFEST_SCHEMA: u32 = 1;

const HARD_MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const HARD_MAX_LINES: usize = 1_000_000;
const HARD_MAX_ENTRIES: usize = 100_000;
const HARD_MAX_FEATURES_PER_ENTRY: usize = 4096;
const HARD_MAX_STRING_BYTES: usize = 64 * 1024;
const HARD_MAX_OBJECT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const READ_BUFFER_BYTES: usize = 64 * 1024;

const ROOT_FIELDS: &[&str] = &["schema", "id", "version"];
const ENTRY_FIELDS: &[&str] = &[
    "sha256",
    "path",
    "tier",
    "page_count",
    "license_expression",
    "source",
    "access",
    "redistribution",
    "features",
    "max_bytes",
];

/// Deterministic resource ceilings for manifest decoding and object verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusManifestLimits {
    max_manifest_bytes: usize,
    max_lines: usize,
    max_entries: usize,
    max_features_per_entry: usize,
    max_string_bytes: usize,
    max_object_bytes: u64,
    max_total_object_bytes: u64,
}

impl CorpusManifestLimits {
    /// Creates non-zero limits under the corpus tool's fixed hard ceilings.
    pub fn new(
        max_manifest_bytes: usize,
        max_lines: usize,
        max_entries: usize,
        max_features_per_entry: usize,
        max_string_bytes: usize,
        max_object_bytes: u64,
        max_total_object_bytes: u64,
    ) -> Result<Self, CorpusManifestError> {
        if max_manifest_bytes == 0
            || max_manifest_bytes > HARD_MAX_MANIFEST_BYTES
            || max_lines == 0
            || max_lines > HARD_MAX_LINES
            || max_entries == 0
            || max_entries > HARD_MAX_ENTRIES
            || max_features_per_entry == 0
            || max_features_per_entry > HARD_MAX_FEATURES_PER_ENTRY
            || max_string_bytes == 0
            || max_string_bytes > HARD_MAX_STRING_BYTES
            || max_object_bytes == 0
            || max_object_bytes > HARD_MAX_OBJECT_BYTES
            || max_total_object_bytes == 0
            || max_total_object_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
        {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_manifest_bytes,
            max_lines,
            max_entries,
            max_features_per_entry,
            max_string_bytes,
            max_object_bytes,
            max_total_object_bytes,
        })
    }

    /// Returns the maximum accepted manifest bytes.
    pub const fn max_manifest_bytes(self) -> usize {
        self.max_manifest_bytes
    }

    /// Returns the maximum physical manifest lines.
    pub const fn max_lines(self) -> usize {
        self.max_lines
    }

    /// Returns the maximum manifest entry count.
    pub const fn max_entries(self) -> usize {
        self.max_entries
    }

    /// Returns the maximum feature tags on one entry.
    pub const fn max_features_per_entry(self) -> usize {
        self.max_features_per_entry
    }

    /// Returns the maximum decoded bytes in one manifest string.
    pub const fn max_string_bytes(self) -> usize {
        self.max_string_bytes
    }

    /// Returns the global ceiling for one declared object budget.
    pub const fn max_object_bytes(self) -> u64 {
        self.max_object_bytes
    }

    /// Returns the maximum verified bytes across all objects.
    pub const fn max_total_object_bytes(self) -> u64 {
        self.max_total_object_bytes
    }
}

impl Default for CorpusManifestLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 1024 * 1024,
            max_lines: 100_000,
            max_entries: 4096,
            max_features_per_entry: 128,
            max_string_bytes: 4096,
            max_object_bytes: 64 * 1024 * 1024,
            max_total_object_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Exact machine-readable on-disk manifest failure code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CorpusManifestErrorCode {
    /// Caller-supplied limits are zero or exceed a hard ceiling.
    InvalidLimits,
    /// Manifest bytes exceed the configured ceiling.
    ManifestLimit,
    /// Physical lines exceed the configured ceiling.
    LineLimit,
    /// Entry count exceeds the configured ceiling.
    EntryLimit,
    /// Feature count on one entry exceeds the configured ceiling.
    FeatureLimit,
    /// A decoded string exceeds the configured ceiling.
    StringLimit,
    /// Manifest bytes are not valid UTF-8.
    InvalidUtf8,
    /// Text violates the supported TOML subset.
    InvalidSyntax,
    /// The manifest schema is not supported.
    UnsupportedSchema,
    /// A field is not defined by schema 1.
    UnknownField,
    /// A field appears more than once in its scope.
    DuplicateField,
    /// A mandatory field is absent.
    MissingField,
    /// A field value violates its semantic contract.
    InvalidValue,
    /// Two entries declare the same content identity.
    DuplicateObject,
    /// Two entries declare the same repository-relative path.
    DuplicatePath,
    /// A path is absolute, non-normalized, escaping, or symbolic-link based.
    UnsafePath,
    /// Valid semantics were not encoded in the unique canonical byte form.
    NonCanonical,
    /// The manifest file is missing, symbolic, or not a regular file.
    ManifestUnavailable,
    /// The object root is missing, symbolic, or not a directory.
    ObjectRootUnavailable,
    /// A declared object or intermediate directory is unavailable.
    ObjectUnavailable,
    /// One object exceeds its declared or global byte ceiling.
    ObjectLimit,
    /// Verified object bytes exceed the manifest-wide ceiling.
    TotalObjectLimit,
    /// Object bytes do not match the declared SHA-256 identity.
    ObjectHashMismatch,
    /// SHA-256 framing failed inside configured byte ceilings.
    HashFailed,
}

/// Stable coarse category for manifest failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorpusManifestErrorCategory {
    /// Caller-supplied limits are invalid.
    Configuration,
    /// A deterministic resource ceiling was reached.
    ResourceLimit,
    /// Manifest text violates the supported grammar.
    Syntax,
    /// Manifest text requests an unsupported schema.
    Unsupported,
    /// Parsed metadata violates the schema's semantic structure.
    Structure,
    /// A required local file-system object is unavailable.
    Availability,
    /// Content identity verification failed.
    Integrity,
    /// Checked internal bookkeeping or hashing failed.
    Internal,
}

/// Stable recovery class for manifest failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorpusManifestRecoverability {
    /// Supply limits within the documented ceilings.
    CorrectConfiguration,
    /// Reduce the bounded manifest or object workload.
    ReduceInput,
    /// Correct and canonically re-encode the manifest.
    CorrectManifest,
    /// Select an implemented manifest schema.
    SelectSupportedSchema,
    /// Restore the expected object/root bytes under the authorized boundary.
    RestoreObject,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Stable, content- and path-redacted corpus manifest failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusManifestError {
    /// Exact machine-readable failure code.
    pub code: CorpusManifestErrorCode,
    /// Coarse failure category.
    pub category: CorpusManifestErrorCategory,
    /// Approved recovery class.
    pub recoverability: CorpusManifestRecoverability,
    /// Stable project diagnostic identifier.
    pub diagnostic_id: &'static str,
    /// One-based manifest line when applicable.
    pub line: Option<usize>,
    /// Zero-based entry index when applicable.
    pub entry_index: Option<usize>,
    detail: &'static str,
}

impl fmt::Display for CorpusManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.diagnostic_id, self.code, self.detail
        )?;
        if let Some(line) = self.line {
            write!(formatter, " at line {line}")?;
        }
        if let Some(entry) = self.entry_index {
            write!(formatter, " for entry {entry}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CorpusManifestError {}

/// One validated on-disk object record.
///
/// This type deliberately has no `Debug` implementation because repository
/// paths are not approved diagnostic content.
#[derive(Clone, Eq, PartialEq)]
pub struct CorpusManifestObject {
    entry: CorpusEntry,
    relative_path: String,
    max_bytes: u64,
}

impl CorpusManifestObject {
    /// Returns the validated corpus metadata.
    pub fn entry(&self) -> &CorpusEntry {
        &self.entry
    }

    /// Returns the normalized repository-relative object path.
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the declared maximum object bytes.
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

/// Canonically decoded schema-1 corpus manifest and exact byte identity.
///
/// This type deliberately has no `Debug` implementation because it owns object
/// paths and provenance strings.
#[derive(Clone, Eq, PartialEq)]
pub struct OnDiskCorpusManifest {
    manifest: CorpusManifest,
    objects: Vec<CorpusManifestObject>,
    source_sha256: [u8; 32],
}

impl OnDiskCorpusManifest {
    /// Returns the deterministic in-memory governance model.
    pub fn manifest(&self) -> &CorpusManifest {
        &self.manifest
    }

    /// Returns objects in content-identity order.
    pub fn objects(&self) -> &[CorpusManifestObject] {
        &self.objects
    }

    /// Returns the SHA-256 of the exact canonical manifest bytes.
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
}

/// Successful manifest and object verification evidence.
///
/// This type deliberately has no `Debug` implementation because it retains the
/// decoded manifest metadata.
pub struct CorpusManifestVerification {
    manifest: OnDiskCorpusManifest,
    verified_objects: u64,
    verified_bytes: u64,
}

impl CorpusManifestVerification {
    /// Returns the verified decoded manifest.
    pub fn manifest(&self) -> &OnDiskCorpusManifest {
        &self.manifest
    }

    /// Returns the number of content identities re-hashed successfully.
    pub const fn verified_objects(&self) -> u64 {
        self.verified_objects
    }

    /// Returns the cumulative bytes read and re-hashed.
    pub const fn verified_bytes(&self) -> u64 {
        self.verified_bytes
    }
}

/// Decodes only the canonical schema-1 TOML subset under deterministic limits.
pub fn decode_manifest(
    input: &[u8],
    limits: CorpusManifestLimits,
) -> Result<OnDiskCorpusManifest, CorpusManifestError> {
    if input.len() > limits.max_manifest_bytes {
        return Err(manifest_error(
            CorpusManifestErrorCode::ManifestLimit,
            None,
            None,
        ));
    }
    let text = std::str::from_utf8(input).map_err(|error| {
        manifest_error(
            CorpusManifestErrorCode::InvalidUtf8,
            Some(line_for_offset(input, error.valid_up_to())),
            None,
        )
    })?;
    let parsed = parse_document(text, limits)?;
    let mut decoded = build_manifest(parsed, limits)?;
    decoded.source_sha256 = sha256(input)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::HashFailed, None, None))?;
    let canonical = encode_manifest(&decoded, limits)?;
    if canonical != input {
        return Err(manifest_error(
            CorpusManifestErrorCode::NonCanonical,
            None,
            None,
        ));
    }
    Ok(decoded)
}

/// Encodes a decoded manifest into its unique canonical schema-1 TOML bytes.
pub fn encode_manifest(
    manifest: &OnDiskCorpusManifest,
    limits: CorpusManifestLimits,
) -> Result<Vec<u8>, CorpusManifestError> {
    let mut output = ManifestOutput::new(limits);
    output.push_str(&format!("schema = {CORPUS_MANIFEST_SCHEMA}\n"))?;
    write_string_field(&mut output, "id", manifest.manifest.id())?;
    write_string_field(&mut output, "version", manifest.manifest.version())?;

    for object in &manifest.objects {
        output.push_str("\n[[entry]]\n")?;
        write_string_field(
            &mut output,
            "sha256",
            &format!("sha256:{}", hex_digest(&object.entry.id().sha256())),
        )?;
        write_string_field(&mut output, "path", &object.relative_path)?;
        write_string_field(&mut output, "tier", tier_name(object.entry.tier()))?;
        output.push_str(&format!("page_count = {}\n", object.entry.page_count()))?;
        write_string_field(
            &mut output,
            "license_expression",
            object.entry.license().expression(),
        )?;
        write_string_field(&mut output, "source", object.entry.license().source())?;
        write_string_field(&mut output, "access", access_name(object.entry.access()))?;
        write_string_field(
            &mut output,
            "redistribution",
            redistribution_name(object.entry.redistribution()),
        )?;
        output.push_str("features = [")?;
        for (index, feature) in object.entry.features().iter().enumerate() {
            if index != 0 {
                output.push_str(", ")?;
            }
            write_quoted(&mut output, feature)?;
        }
        output.push_str("]\n")?;
        output.push_str(&format!("max_bytes = {}\n", object.max_bytes))?;
    }

    Ok(output.into_bytes())
}

/// Reads and decodes one canonical manifest without verifying object files.
pub fn load_manifest_file(
    path: &Path,
    limits: CorpusManifestLimits,
) -> Result<OnDiskCorpusManifest, CorpusManifestError> {
    let input = read_manifest_bytes(path, limits)?;
    decode_manifest(&input, limits)
}

/// Reads a canonical manifest and verifies every declared object below `object_root`.
pub fn validate_manifest_file(
    manifest_path: &Path,
    object_root: &Path,
    limits: CorpusManifestLimits,
) -> Result<CorpusManifestVerification, CorpusManifestError> {
    let manifest = load_manifest_file(manifest_path, limits)?;
    verify_manifest_objects(manifest, object_root, limits)
}

/// Re-hashes every declared object through bounded streaming reads.
pub fn verify_manifest_objects(
    manifest: OnDiskCorpusManifest,
    object_root: &Path,
    limits: CorpusManifestLimits,
) -> Result<CorpusManifestVerification, CorpusManifestError> {
    let root_metadata = fs::symlink_metadata(object_root)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ObjectRootUnavailable, None, None))?;
    if !root_metadata.file_type().is_dir() {
        return Err(manifest_error(
            CorpusManifestErrorCode::ObjectRootUnavailable,
            None,
            None,
        ));
    }

    let mut verified_bytes = 0_u64;
    for (entry_index, object) in manifest.objects.iter().enumerate() {
        let object_path = inspect_object_path(object_root, &object.relative_path, entry_index)?;
        let metadata = fs::symlink_metadata(&object_path).map_err(|_| {
            manifest_error(
                CorpusManifestErrorCode::ObjectUnavailable,
                None,
                Some(entry_index),
            )
        })?;
        if !metadata.file_type().is_file() {
            return Err(manifest_error(
                CorpusManifestErrorCode::ObjectUnavailable,
                None,
                Some(entry_index),
            ));
        }
        if metadata.len() > object.max_bytes || metadata.len() > limits.max_object_bytes {
            return Err(manifest_error(
                CorpusManifestErrorCode::ObjectLimit,
                None,
                Some(entry_index),
            ));
        }
        let predicted_total = verified_bytes.checked_add(metadata.len()).ok_or_else(|| {
            manifest_error(
                CorpusManifestErrorCode::TotalObjectLimit,
                None,
                Some(entry_index),
            )
        })?;
        if predicted_total > limits.max_total_object_bytes {
            return Err(manifest_error(
                CorpusManifestErrorCode::TotalObjectLimit,
                None,
                Some(entry_index),
            ));
        }

        let remaining_total = limits.max_total_object_bytes - verified_bytes;
        let allowed = object
            .max_bytes
            .min(limits.max_object_bytes)
            .min(remaining_total);
        let take_limit = allowed.checked_add(1).ok_or_else(|| {
            manifest_error(
                CorpusManifestErrorCode::ObjectLimit,
                None,
                Some(entry_index),
            )
        })?;
        let file = File::open(&object_path).map_err(|_| {
            manifest_error(
                CorpusManifestErrorCode::ObjectUnavailable,
                None,
                Some(entry_index),
            )
        })?;
        let mut reader: Take<File> = file.take(take_limit);
        let mut hasher = Sha256::new();
        let mut object_bytes = 0_u64;
        let mut buffer = [0_u8; READ_BUFFER_BYTES];
        loop {
            let read = reader.read(&mut buffer).map_err(|_| {
                manifest_error(
                    CorpusManifestErrorCode::ObjectUnavailable,
                    None,
                    Some(entry_index),
                )
            })?;
            if read == 0 {
                break;
            }
            object_bytes = object_bytes
                .checked_add(u64::try_from(read).map_err(|_| {
                    manifest_error(
                        CorpusManifestErrorCode::ObjectLimit,
                        None,
                        Some(entry_index),
                    )
                })?)
                .ok_or_else(|| {
                    manifest_error(
                        CorpusManifestErrorCode::ObjectLimit,
                        None,
                        Some(entry_index),
                    )
                })?;
            if object_bytes > allowed {
                let code = if object.max_bytes.min(limits.max_object_bytes) <= remaining_total {
                    CorpusManifestErrorCode::ObjectLimit
                } else {
                    CorpusManifestErrorCode::TotalObjectLimit
                };
                return Err(manifest_error(code, None, Some(entry_index)));
            }
            hasher.update(&buffer[..read]).map_err(|_| {
                manifest_error(CorpusManifestErrorCode::HashFailed, None, Some(entry_index))
            })?;
        }
        let actual = hasher.finalize().map_err(|_| {
            manifest_error(CorpusManifestErrorCode::HashFailed, None, Some(entry_index))
        })?;
        if actual != object.entry.id().sha256() {
            return Err(manifest_error(
                CorpusManifestErrorCode::ObjectHashMismatch,
                None,
                Some(entry_index),
            ));
        }
        verified_bytes = verified_bytes.checked_add(object_bytes).ok_or_else(|| {
            manifest_error(
                CorpusManifestErrorCode::TotalObjectLimit,
                None,
                Some(entry_index),
            )
        })?;
    }

    let verified_objects = u64::try_from(manifest.objects.len())
        .map_err(|_| manifest_error(CorpusManifestErrorCode::EntryLimit, None, None))?;
    Ok(CorpusManifestVerification {
        manifest,
        verified_objects,
        verified_bytes,
    })
}

#[derive(Clone)]
struct RawValue {
    value: String,
    line: usize,
}

struct ParsedDocument {
    root: BTreeMap<String, RawValue>,
    entries: Vec<BTreeMap<String, RawValue>>,
}

fn parse_document(
    text: &str,
    limits: CorpusManifestLimits,
) -> Result<ParsedDocument, CorpusManifestError> {
    let mut root = BTreeMap::new();
    let mut entries: Vec<BTreeMap<String, RawValue>> = Vec::new();
    let mut current_entry = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line_number > limits.max_lines {
            return Err(manifest_error(
                CorpusManifestErrorCode::LineLimit,
                Some(line_number),
                current_entry,
            ));
        }
        let line = strip_comment(raw_line)
            .map_err(|_| {
                manifest_error(
                    CorpusManifestErrorCode::InvalidSyntax,
                    Some(line_number),
                    current_entry,
                )
            })?
            .trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[entry]]" {
            if entries.len() >= limits.max_entries {
                return Err(manifest_error(
                    CorpusManifestErrorCode::EntryLimit,
                    Some(line_number),
                    Some(entries.len()),
                ));
            }
            entries.push(BTreeMap::new());
            current_entry = Some(entries.len() - 1);
            continue;
        }
        if line.starts_with('[') {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidSyntax,
                Some(line_number),
                current_entry,
            ));
        }

        let (key, value) = split_assignment(line).ok_or_else(|| {
            manifest_error(
                CorpusManifestErrorCode::InvalidSyntax,
                Some(line_number),
                current_entry,
            )
        })?;
        let allowed = if current_entry.is_some() {
            ENTRY_FIELDS
        } else {
            ROOT_FIELDS
        };
        if !allowed.contains(&key) {
            return Err(manifest_error(
                CorpusManifestErrorCode::UnknownField,
                Some(line_number),
                current_entry,
            ));
        }
        let values = if let Some(entry) = current_entry {
            entries
                .get_mut(entry)
                .expect("current entry was inserted before its fields")
        } else {
            &mut root
        };
        if values
            .insert(
                key.into(),
                RawValue {
                    value: value.into(),
                    line: line_number,
                },
            )
            .is_some()
        {
            return Err(manifest_error(
                CorpusManifestErrorCode::DuplicateField,
                Some(line_number),
                current_entry,
            ));
        }
    }

    Ok(ParsedDocument { root, entries })
}

fn build_manifest(
    mut parsed: ParsedDocument,
    limits: CorpusManifestLimits,
) -> Result<OnDiskCorpusManifest, CorpusManifestError> {
    let schema = take_required(&mut parsed.root, "schema", None)?;
    let schema_value = parse_canonical_u64(&schema, None)?;
    if schema_value != u64::from(CORPUS_MANIFEST_SCHEMA) {
        return Err(manifest_error(
            CorpusManifestErrorCode::UnsupportedSchema,
            Some(schema.line),
            None,
        ));
    }
    let id = parse_string(&take_required(&mut parsed.root, "id", None)?, limits, None)?;
    let version = parse_string(
        &take_required(&mut parsed.root, "version", None)?,
        limits,
        None,
    )?;
    if parsed.entries.is_empty() {
        return Err(manifest_error(
            CorpusManifestErrorCode::MissingField,
            None,
            None,
        ));
    }

    let mut objects = Vec::new();
    objects
        .try_reserve(parsed.entries.len())
        .map_err(|_| manifest_error(CorpusManifestErrorCode::EntryLimit, None, None))?;
    let mut paths = BTreeSet::new();
    for (entry_index, mut fields) in parsed.entries.into_iter().enumerate() {
        let digest = parse_digest(
            &take_required(&mut fields, "sha256", Some(entry_index))?,
            limits,
            entry_index,
        )?;
        let path = parse_string(
            &take_required(&mut fields, "path", Some(entry_index))?,
            limits,
            Some(entry_index),
        )?;
        validate_relative_path(&path, entry_index)?;
        if !paths.insert(path.clone()) {
            return Err(manifest_error(
                CorpusManifestErrorCode::DuplicatePath,
                None,
                Some(entry_index),
            ));
        }
        let tier_value = take_required(&mut fields, "tier", Some(entry_index))?;
        let tier = parse_tier(
            &parse_string(&tier_value, limits, Some(entry_index))?,
            tier_value.line,
            entry_index,
        )?;
        if !matches!(tier, CorpusTier::T0 | CorpusTier::T1) {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                Some(tier_value.line),
                Some(entry_index),
            ));
        }
        let page_value = take_required(&mut fields, "page_count", Some(entry_index))?;
        let page_count = u32::try_from(parse_canonical_u64(&page_value, Some(entry_index))?)
            .map_err(|_| {
                manifest_error(
                    CorpusManifestErrorCode::InvalidValue,
                    Some(page_value.line),
                    Some(entry_index),
                )
            })?;
        if page_count == 0 {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                Some(page_value.line),
                Some(entry_index),
            ));
        }
        let license_expression = parse_string(
            &take_required(&mut fields, "license_expression", Some(entry_index))?,
            limits,
            Some(entry_index),
        )?;
        let source = parse_string(
            &take_required(&mut fields, "source", Some(entry_index))?,
            limits,
            Some(entry_index),
        )?;
        let access_value = take_required(&mut fields, "access", Some(entry_index))?;
        let access = parse_access(
            &parse_string(&access_value, limits, Some(entry_index))?,
            access_value.line,
            entry_index,
        )?;
        let redistribution_value = take_required(&mut fields, "redistribution", Some(entry_index))?;
        let redistribution = parse_redistribution(
            &parse_string(&redistribution_value, limits, Some(entry_index))?,
            redistribution_value.line,
            entry_index,
        )?;
        if access != AccessPolicy::Repository
            || (tier == CorpusTier::T0 && redistribution != RedistributionPolicy::Prohibited)
        {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                Some(access_value.line.min(redistribution_value.line)),
                Some(entry_index),
            ));
        }
        let feature_value = take_required(&mut fields, "features", Some(entry_index))?;
        let features = parse_string_array(&feature_value, limits, entry_index)?;
        if features.iter().any(|feature| !is_stable_id(feature)) {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                Some(feature_value.line),
                Some(entry_index),
            ));
        }
        let max_value = take_required(&mut fields, "max_bytes", Some(entry_index))?;
        let max_bytes = parse_canonical_u64(&max_value, Some(entry_index))?;
        if max_bytes == 0 || max_bytes > limits.max_object_bytes {
            return Err(manifest_error(
                CorpusManifestErrorCode::ObjectLimit,
                Some(max_value.line),
                Some(entry_index),
            ));
        }

        let license = LicenseRecord::new(license_expression, source).map_err(|_| {
            manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                None,
                Some(entry_index),
            )
        })?;
        let entry = CorpusEntry::new(
            CorpusObjectId::from_sha256(digest),
            tier,
            page_count,
            license,
            access,
            redistribution,
            features,
        )
        .map_err(|_| {
            manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                None,
                Some(entry_index),
            )
        })?;
        objects.push(CorpusManifestObject {
            entry,
            relative_path: path,
            max_bytes,
        });
    }

    objects.sort_by_key(|object| object.entry.id());
    let manifest = CorpusManifest::new(
        id,
        version,
        objects.iter().map(|object| object.entry.clone()).collect(),
    )
    .map_err(|error| {
        let code = if error.code == CorpusErrorCode::DuplicateObject {
            CorpusManifestErrorCode::DuplicateObject
        } else {
            CorpusManifestErrorCode::InvalidValue
        };
        manifest_error(code, None, None)
    })?;
    Ok(OnDiskCorpusManifest {
        manifest,
        objects,
        source_sha256: [0; 32],
    })
}

fn take_required(
    values: &mut BTreeMap<String, RawValue>,
    key: &str,
    entry_index: Option<usize>,
) -> Result<RawValue, CorpusManifestError> {
    values
        .remove(key)
        .ok_or_else(|| manifest_error(CorpusManifestErrorCode::MissingField, None, entry_index))
}

fn parse_string(
    raw: &RawValue,
    limits: CorpusManifestLimits,
    entry_index: Option<usize>,
) -> Result<String, CorpusManifestError> {
    if raw.value.len() < 2 || !raw.value.starts_with('"') || !raw.value.ends_with('"') {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidSyntax,
            Some(raw.line),
            entry_index,
        ));
    }
    let interior = &raw.value[1..raw.value.len() - 1];
    let mut characters = interior.chars();
    let mut output = String::new();
    while let Some(character) = characters.next() {
        let decoded = match character {
            '"' => {
                return Err(manifest_error(
                    CorpusManifestErrorCode::InvalidSyntax,
                    Some(raw.line),
                    entry_index,
                ));
            }
            '\\' => match characters.next() {
                Some('"') => '"',
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('u') => {
                    let mut scalar = 0_u32;
                    for _ in 0..4 {
                        let digit = characters
                            .next()
                            .and_then(|value| value.to_digit(16))
                            .ok_or_else(|| {
                                manifest_error(
                                    CorpusManifestErrorCode::InvalidSyntax,
                                    Some(raw.line),
                                    entry_index,
                                )
                            })?;
                        scalar = scalar * 16 + digit;
                    }
                    char::from_u32(scalar).ok_or_else(|| {
                        manifest_error(
                            CorpusManifestErrorCode::InvalidSyntax,
                            Some(raw.line),
                            entry_index,
                        )
                    })?
                }
                _ => {
                    return Err(manifest_error(
                        CorpusManifestErrorCode::InvalidSyntax,
                        Some(raw.line),
                        entry_index,
                    ));
                }
            },
            value if value.is_control() => {
                return Err(manifest_error(
                    CorpusManifestErrorCode::InvalidSyntax,
                    Some(raw.line),
                    entry_index,
                ));
            }
            value => value,
        };
        if decoded.is_control() {
            return Err(manifest_error(
                CorpusManifestErrorCode::InvalidValue,
                Some(raw.line),
                entry_index,
            ));
        }
        output.push(decoded);
        if output.len() > limits.max_string_bytes {
            return Err(manifest_error(
                CorpusManifestErrorCode::StringLimit,
                Some(raw.line),
                entry_index,
            ));
        }
    }
    Ok(output)
}

fn parse_string_array(
    raw: &RawValue,
    limits: CorpusManifestLimits,
    entry_index: usize,
) -> Result<Vec<String>, CorpusManifestError> {
    let value = raw.value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidSyntax,
            Some(raw.line),
            Some(entry_index),
        ));
    }
    let interior = value[1..value.len() - 1].trim();
    if interior.is_empty() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
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
                if parts.len() >= limits.max_features_per_entry {
                    return Err(manifest_error(
                        CorpusManifestErrorCode::FeatureLimit,
                        Some(raw.line),
                        Some(entry_index),
                    ));
                }
                parts.try_reserve(1).map_err(|_| {
                    manifest_error(
                        CorpusManifestErrorCode::FeatureLimit,
                        Some(raw.line),
                        Some(entry_index),
                    )
                })?;
                parts.push(interior[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted || escaped {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidSyntax,
            Some(raw.line),
            Some(entry_index),
        ));
    }
    if parts.len() >= limits.max_features_per_entry {
        return Err(manifest_error(
            CorpusManifestErrorCode::FeatureLimit,
            Some(raw.line),
            Some(entry_index),
        ));
    }
    parts.try_reserve(1).map_err(|_| {
        manifest_error(
            CorpusManifestErrorCode::FeatureLimit,
            Some(raw.line),
            Some(entry_index),
        )
    })?;
    parts.push(interior[start..].trim());
    if parts.iter().any(|part| part.is_empty()) {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidSyntax,
            Some(raw.line),
            Some(entry_index),
        ));
    }
    parts
        .into_iter()
        .map(|part| {
            parse_string(
                &RawValue {
                    value: part.into(),
                    line: raw.line,
                },
                limits,
                Some(entry_index),
            )
        })
        .collect()
}

fn parse_digest(
    raw: &RawValue,
    limits: CorpusManifestLimits,
    entry_index: usize,
) -> Result<[u8; 32], CorpusManifestError> {
    let value = parse_string(raw, limits, Some(entry_index))?;
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(raw.line),
            Some(entry_index),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(raw.line),
            Some(entry_index),
        ));
    }
    let mut digest = [0_u8; 32];
    for (target, pair) in digest.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *target = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    Ok(digest)
}

fn parse_canonical_u64(
    raw: &RawValue,
    entry_index: Option<usize>,
) -> Result<u64, CorpusManifestError> {
    if raw.value.is_empty()
        || !raw.value.bytes().all(|byte| byte.is_ascii_digit())
        || (raw.value.len() > 1 && raw.value.starts_with('0'))
    {
        return Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(raw.line),
            entry_index,
        ));
    }
    raw.value.parse().map_err(|_| {
        manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(raw.line),
            entry_index,
        )
    })
}

fn parse_tier(
    value: &str,
    line: usize,
    entry_index: usize,
) -> Result<CorpusTier, CorpusManifestError> {
    match value {
        "T0" => Ok(CorpusTier::T0),
        "T1" => Ok(CorpusTier::T1),
        "T2" => Ok(CorpusTier::T2),
        "T3" => Ok(CorpusTier::T3),
        _ => Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(line),
            Some(entry_index),
        )),
    }
}

fn parse_access(
    value: &str,
    line: usize,
    entry_index: usize,
) -> Result<AccessPolicy, CorpusManifestError> {
    match value {
        "public" => Ok(AccessPolicy::Public),
        "repository" => Ok(AccessPolicy::Repository),
        "restricted" => Ok(AccessPolicy::Restricted),
        "private" => Ok(AccessPolicy::Private),
        _ => Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(line),
            Some(entry_index),
        )),
    }
}

fn parse_redistribution(
    value: &str,
    line: usize,
    entry_index: usize,
) -> Result<RedistributionPolicy, CorpusManifestError> {
    match value {
        "allowed" => Ok(RedistributionPolicy::Allowed),
        "prohibited" => Ok(RedistributionPolicy::Prohibited),
        _ => Err(manifest_error(
            CorpusManifestErrorCode::InvalidValue,
            Some(line),
            Some(entry_index),
        )),
    }
}

fn validate_relative_path(path: &str, entry_index: usize) -> Result<(), CorpusManifestError> {
    let parsed = Path::new(path);
    if path.is_empty()
        || path.chars().any(char::is_control)
        || path.contains('\\')
        || path.contains("//")
        || path.ends_with('/')
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(manifest_error(
            CorpusManifestErrorCode::UnsafePath,
            None,
            Some(entry_index),
        ));
    }
    Ok(())
}

fn inspect_object_path(
    root: &Path,
    relative: &str,
    entry_index: usize,
) -> Result<PathBuf, CorpusManifestError> {
    validate_relative_path(relative, entry_index)?;
    let mut current = root.to_path_buf();
    let path = Path::new(relative);
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(manifest_error(
                CorpusManifestErrorCode::UnsafePath,
                None,
                Some(entry_index),
            ));
        };
        current.push(segment);
        let metadata = fs::symlink_metadata(&current).map_err(|_| {
            manifest_error(
                CorpusManifestErrorCode::ObjectUnavailable,
                None,
                Some(entry_index),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(manifest_error(
                CorpusManifestErrorCode::UnsafePath,
                None,
                Some(entry_index),
            ));
        }
        let final_component = components.peek().is_none();
        if (!final_component && !metadata.file_type().is_dir())
            || (final_component && !metadata.file_type().is_file())
        {
            return Err(manifest_error(
                CorpusManifestErrorCode::ObjectUnavailable,
                None,
                Some(entry_index),
            ));
        }
    }
    Ok(current)
}

fn read_manifest_bytes(
    path: &Path,
    limits: CorpusManifestLimits,
) -> Result<Vec<u8>, CorpusManifestError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestUnavailable, None, None))?;
    if !metadata.file_type().is_file() {
        return Err(manifest_error(
            CorpusManifestErrorCode::ManifestUnavailable,
            None,
            None,
        ));
    }
    if metadata.len()
        > u64::try_from(limits.max_manifest_bytes)
            .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?
    {
        return Err(manifest_error(
            CorpusManifestErrorCode::ManifestLimit,
            None,
            None,
        ));
    }
    let initial_capacity = usize::try_from(metadata.len())
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?
        .checked_add(1)
        .ok_or_else(|| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?;
    let read_limit = limits
        .max_manifest_bytes
        .checked_add(1)
        .ok_or_else(|| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?;
    let mut input = Vec::new();
    input
        .try_reserve_exact(initial_capacity)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?;
    let file = File::open(path)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestUnavailable, None, None))?;
    let mut reader: Take<File> = file.take(
        u64::try_from(read_limit)
            .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?,
    );
    reader
        .read_to_end(&mut input)
        .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestUnavailable, None, None))?;
    if input.len() > limits.max_manifest_bytes {
        return Err(manifest_error(
            CorpusManifestErrorCode::ManifestLimit,
            None,
            None,
        ));
    }
    Ok(input)
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
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

fn strip_comment(line: &str) -> Result<&str, ()> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return Ok(&line[..index]),
            _ => {}
        }
    }
    if quoted || escaped { Err(()) } else { Ok(line) }
}

fn line_for_offset(input: &[u8], offset: usize) -> usize {
    input[..offset.min(input.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

struct ManifestOutput {
    value: String,
    lines: usize,
    limits: CorpusManifestLimits,
}

impl ManifestOutput {
    fn new(limits: CorpusManifestLimits) -> Self {
        Self {
            value: String::new(),
            lines: 0,
            limits,
        }
    }

    fn push_str(&mut self, value: &str) -> Result<(), CorpusManifestError> {
        let next_len =
            self.value.len().checked_add(value.len()).ok_or_else(|| {
                manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None)
            })?;
        if next_len > self.limits.max_manifest_bytes {
            return Err(manifest_error(
                CorpusManifestErrorCode::ManifestLimit,
                None,
                None,
            ));
        }
        let added_lines = value.bytes().filter(|byte| *byte == b'\n').count();
        let next_lines = self
            .lines
            .checked_add(added_lines)
            .ok_or_else(|| manifest_error(CorpusManifestErrorCode::LineLimit, None, None))?;
        if next_lines > self.limits.max_lines {
            return Err(manifest_error(
                CorpusManifestErrorCode::LineLimit,
                None,
                None,
            ));
        }
        self.value
            .try_reserve(value.len())
            .map_err(|_| manifest_error(CorpusManifestErrorCode::ManifestLimit, None, None))?;
        self.value.push_str(value);
        self.lines = next_lines;
        Ok(())
    }

    fn push_char(&mut self, value: char) -> Result<(), CorpusManifestError> {
        let mut encoded = [0; 4];
        self.push_str(value.encode_utf8(&mut encoded))
    }

    fn into_bytes(self) -> Vec<u8> {
        self.value.into_bytes()
    }
}

fn write_string_field(
    output: &mut ManifestOutput,
    key: &str,
    value: &str,
) -> Result<(), CorpusManifestError> {
    output.push_str(key)?;
    output.push_str(" = ")?;
    write_quoted(output, value)?;
    output.push_str("\n")
}

fn write_quoted(output: &mut ManifestOutput, value: &str) -> Result<(), CorpusManifestError> {
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

const fn tier_name(value: CorpusTier) -> &'static str {
    match value {
        CorpusTier::T0 => "T0",
        CorpusTier::T1 => "T1",
        CorpusTier::T2 => "T2",
        CorpusTier::T3 => "T3",
    }
}

const fn access_name(value: AccessPolicy) -> &'static str {
    match value {
        AccessPolicy::Public => "public",
        AccessPolicy::Repository => "repository",
        AccessPolicy::Restricted => "restricted",
        AccessPolicy::Private => "private",
    }
}

const fn redistribution_name(value: RedistributionPolicy) -> &'static str {
    match value {
        RedistributionPolicy::Allowed => "allowed",
        RedistributionPolicy::Prohibited => "prohibited",
    }
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => 0,
    }
}

fn manifest_error(
    code: CorpusManifestErrorCode,
    line: Option<usize>,
    entry_index: Option<usize>,
) -> CorpusManifestError {
    let (diagnostic_id, detail, category, recoverability) = match code {
        CorpusManifestErrorCode::InvalidLimits => (
            "RPE-CORPUS-MANIFEST-0001",
            "manifest limits are invalid",
            CorpusManifestErrorCategory::Configuration,
            CorpusManifestRecoverability::CorrectConfiguration,
        ),
        CorpusManifestErrorCode::ManifestLimit => (
            "RPE-CORPUS-MANIFEST-0002",
            "manifest bytes exceed their limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::LineLimit => (
            "RPE-CORPUS-MANIFEST-0003",
            "manifest lines exceed their limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::EntryLimit => (
            "RPE-CORPUS-MANIFEST-0004",
            "manifest entries exceed their limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::FeatureLimit => (
            "RPE-CORPUS-MANIFEST-0005",
            "entry features exceed their limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::StringLimit => (
            "RPE-CORPUS-MANIFEST-0006",
            "decoded manifest string exceeds its limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::InvalidUtf8 => (
            "RPE-CORPUS-MANIFEST-0007",
            "manifest is not valid UTF-8",
            CorpusManifestErrorCategory::Syntax,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::InvalidSyntax => (
            "RPE-CORPUS-MANIFEST-0008",
            "manifest syntax is invalid",
            CorpusManifestErrorCategory::Syntax,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::UnsupportedSchema => (
            "RPE-CORPUS-MANIFEST-0009",
            "manifest schema is unsupported",
            CorpusManifestErrorCategory::Unsupported,
            CorpusManifestRecoverability::SelectSupportedSchema,
        ),
        CorpusManifestErrorCode::UnknownField => (
            "RPE-CORPUS-MANIFEST-0010",
            "manifest field is unknown",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::DuplicateField => (
            "RPE-CORPUS-MANIFEST-0011",
            "manifest field is duplicated",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::MissingField => (
            "RPE-CORPUS-MANIFEST-0012",
            "mandatory manifest field is missing",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::InvalidValue => (
            "RPE-CORPUS-MANIFEST-0013",
            "manifest field value is invalid",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::DuplicateObject => (
            "RPE-CORPUS-MANIFEST-0014",
            "manifest object identity is duplicated",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::DuplicatePath => (
            "RPE-CORPUS-MANIFEST-0015",
            "manifest object path is duplicated",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::UnsafePath => (
            "RPE-CORPUS-MANIFEST-0016",
            "manifest object path is unsafe",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::NonCanonical => (
            "RPE-CORPUS-MANIFEST-0017",
            "manifest bytes are not canonical",
            CorpusManifestErrorCategory::Structure,
            CorpusManifestRecoverability::CorrectManifest,
        ),
        CorpusManifestErrorCode::ManifestUnavailable => (
            "RPE-CORPUS-MANIFEST-0018",
            "manifest file is unavailable",
            CorpusManifestErrorCategory::Availability,
            CorpusManifestRecoverability::RestoreObject,
        ),
        CorpusManifestErrorCode::ObjectRootUnavailable => (
            "RPE-CORPUS-MANIFEST-0019",
            "object root is unavailable",
            CorpusManifestErrorCategory::Availability,
            CorpusManifestRecoverability::RestoreObject,
        ),
        CorpusManifestErrorCode::ObjectUnavailable => (
            "RPE-CORPUS-MANIFEST-0020",
            "manifest object is unavailable",
            CorpusManifestErrorCategory::Availability,
            CorpusManifestRecoverability::RestoreObject,
        ),
        CorpusManifestErrorCode::ObjectLimit => (
            "RPE-CORPUS-MANIFEST-0021",
            "manifest object exceeds its byte limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::TotalObjectLimit => (
            "RPE-CORPUS-MANIFEST-0022",
            "verified corpus bytes exceed their total limit",
            CorpusManifestErrorCategory::ResourceLimit,
            CorpusManifestRecoverability::ReduceInput,
        ),
        CorpusManifestErrorCode::ObjectHashMismatch => (
            "RPE-CORPUS-MANIFEST-0023",
            "manifest object identity does not match its bytes",
            CorpusManifestErrorCategory::Integrity,
            CorpusManifestRecoverability::RestoreObject,
        ),
        CorpusManifestErrorCode::HashFailed => (
            "RPE-CORPUS-MANIFEST-0024",
            "bounded corpus hashing failed",
            CorpusManifestErrorCategory::Internal,
            CorpusManifestRecoverability::DoNotRetry,
        ),
    };
    CorpusManifestError {
        code,
        category,
        recoverability,
        diagnostic_id,
        line,
        entry_index,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const HASH: &str = "9c819e549afcc89d03b380c3c1bd47128aa2b70ae30a35245e6a0e30132875db";
    const ABC_HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    fn canonical(path: &str, hash: &str, max_bytes: u64) -> String {
        format!(
            "schema = 1\nid = \"t0-bootstrap-v1\"\nversion = \"1\"\n\n[[entry]]\nsha256 = \"sha256:{hash}\"\npath = \"{path}\"\ntier = \"T0\"\npage_count = 1\nlicense_expression = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\nsource = \"fixture.infrastructure.synthetic-failure-bundle-001\"\naccess = \"repository\"\nredistribution = \"prohibited\"\nfeatures = [\"syntax.core\", \"xref.table\"]\nmax_bytes = {max_bytes}\n"
        )
    }

    #[test]
    fn parses_a_basic_string_and_canonical_manifest() {
        let raw = RawValue {
            value: format!("\"sha256:{HASH}\""),
            line: 6,
        };
        assert_eq!(
            parse_string(&raw, CorpusManifestLimits::default(), Some(0)).unwrap(),
            format!("sha256:{HASH}")
        );

        let source = canonical("object.pdf", HASH, 65_536);
        let parsed = parse_document(&source, CorpusManifestLimits::default()).unwrap();
        assert_eq!(
            parsed.entries[0]["sha256"].value,
            format!("\"sha256:{HASH}\"")
        );
        let decoded = decode_manifest(source.as_bytes(), CorpusManifestLimits::default()).unwrap();
        assert_eq!(decoded.manifest().id(), "t0-bootstrap-v1");
        assert_eq!(decoded.objects().len(), 1);
        assert_eq!(
            encode_manifest(&decoded, CorpusManifestLimits::default()).unwrap(),
            source.as_bytes()
        );
        assert_eq!(decoded.source_sha256(), sha256(source.as_bytes()).unwrap());
        assert_eq!(decoded.objects()[0].relative_path(), "object.pdf");
        assert_eq!(decoded.objects()[0].max_bytes(), 65_536);
    }

    #[test]
    fn rejects_noncanonical_schema_fields_and_values() {
        let source = canonical("object.pdf", HASH, 65_536);
        assert_eq!(
            decode_manifest(
                source.replacen("schema = 1", "schema=1", 1).as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::NonCanonical
        );
        assert_eq!(
            decode_manifest(
                source.replacen("schema = 1", "schema = 2", 1).as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::UnsupportedSchema
        );
        assert_eq!(
            decode_manifest(
                source
                    .replacen("version = \"1\"", "version = \"1\"\nid = \"again\"", 1)
                    .as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::DuplicateField
        );
        assert_eq!(
            decode_manifest(
                source.replace("path = \"object.pdf\"\n", "").as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::MissingField
        );
        assert_eq!(
            decode_manifest(
                source
                    .replacen("page_count = 1", "unknown = 1\npage_count = 1", 1)
                    .as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::UnknownField
        );
        assert_eq!(
            decode_manifest(
                source
                    .replacen(HASH, &HASH.to_ascii_uppercase(), 1)
                    .as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::InvalidValue
        );
        assert_eq!(
            decode_manifest(
                source
                    .replacen(
                        "features = [\"syntax.core\", \"xref.table\"]",
                        "features = [\"xref.table\", \"syntax.core\"]",
                        1,
                    )
                    .as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::NonCanonical
        );
        assert_eq!(
            decode_manifest(
                source
                    .replacen(
                        "source = \"fixture.infrastructure.synthetic-failure-bundle-001\"",
                        "source = \"bad\\nvalue\"",
                        1,
                    )
                    .as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::InvalidValue
        );
        assert_eq!(
            decode_manifest(
                b"schema = 1\nid = \"empty\"\nversion = \"1\"\n",
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::MissingField
        );
        let invalid_utf8 = [0xff];
        let error = decode_manifest(&invalid_utf8, CorpusManifestLimits::default())
            .err()
            .unwrap();
        assert_eq!(error.code, CorpusManifestErrorCode::InvalidUtf8);
        assert_eq!(error.line, Some(1));
    }

    #[test]
    fn schema_one_admits_t1_repository_corpora_but_rejects_unapproved_tiers_or_access() {
        let source = canonical("object.pdf", HASH, 65_536);
        for (from, to) in [
            ("tier = \"T0\"", "tier = \"T2\""),
            ("tier = \"T0\"", "tier = \"T3\""),
            ("access = \"repository\"", "access = \"public\""),
            ("access = \"repository\"", "access = \"restricted\""),
            ("access = \"repository\"", "access = \"private\""),
        ] {
            let error = decode_manifest(
                source.replacen(from, to, 1).as_bytes(),
                CorpusManifestLimits::default(),
            )
            .err()
            .unwrap();
            assert_eq!(error.code, CorpusManifestErrorCode::InvalidValue);
            assert_eq!(error.category, CorpusManifestErrorCategory::Structure);
            assert_eq!(
                error.recoverability,
                CorpusManifestRecoverability::CorrectManifest
            );
            assert_eq!(error.diagnostic_id, "RPE-CORPUS-MANIFEST-0013");
            assert!(!error.to_string().contains(to));
        }

        let t1 = source
            .replacen("tier = \"T0\"", "tier = \"T1\"", 1)
            .replacen(
                "redistribution = \"prohibited\"",
                "redistribution = \"allowed\"",
                1,
            );
        assert!(decode_manifest(t1.as_bytes(), CorpusManifestLimits::default()).is_ok());

        let t0_redistributable = source.replacen(
            "redistribution = \"prohibited\"",
            "redistribution = \"allowed\"",
            1,
        );
        assert_eq!(
            decode_manifest(
                t0_redistributable.as_bytes(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::InvalidValue
        );
    }

    #[test]
    fn rejects_unsafe_paths_without_disclosing_them() {
        for path in [
            "/absolute.pdf",
            "../escape.pdf",
            "dir/../escape.pdf",
            "dir//object.pdf",
        ] {
            let source = canonical(path, HASH, 65_536);
            let error = decode_manifest(source.as_bytes(), CorpusManifestLimits::default())
                .err()
                .unwrap();
            assert_eq!(error.code, CorpusManifestErrorCode::UnsafePath);
            assert!(!error.to_string().contains(path));
            assert!(!format!("{error:?}").contains(path));
        }
        let backslash = canonical("dir\\object.pdf", HASH, 65_536);
        assert_eq!(
            decode_manifest(backslash.as_bytes(), CorpusManifestLimits::default())
                .err()
                .unwrap()
                .code,
            CorpusManifestErrorCode::InvalidSyntax
        );
    }

    #[test]
    fn deterministic_limits_accept_exact_boundaries() {
        let source = canonical("object.pdf", HASH, 65_536);
        let line_count = source.lines().count();
        let exact_limits = limits(source.len(), line_count, 1, 2, 71, 65_536, 65_536);
        let decoded = decode_manifest(source.as_bytes(), exact_limits).unwrap();
        assert_eq!(
            encode_manifest(&decoded, exact_limits).unwrap(),
            source.as_bytes()
        );
        assert_eq!(
            encode_manifest(
                &decoded,
                limits(source.len() - 1, line_count, 1, 2, 71, 65_536, 65_536,),
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ManifestLimit
        );
        assert_eq!(
            encode_manifest(
                &decoded,
                limits(source.len(), line_count - 1, 1, 2, 71, 65_536, 65_536,),
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::LineLimit
        );
        assert_eq!(
            decode_manifest(
                source.as_bytes(),
                limits(source.len() - 1, line_count, 1, 2, 71, 65_536, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ManifestLimit
        );
        assert_eq!(
            decode_manifest(
                source.as_bytes(),
                limits(source.len(), line_count - 1, 1, 2, 71, 65_536, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::LineLimit
        );
        assert_eq!(
            decode_manifest(
                source.as_bytes(),
                limits(source.len(), line_count, 1, 1, 71, 65_536, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::FeatureLimit
        );
        let excess_features = source.replacen(
            "features = [\"syntax.core\", \"xref.table\"]",
            "features = [\"syntax.core\", invalid, still-invalid]",
            1,
        );
        assert_eq!(
            decode_manifest(
                excess_features.as_bytes(),
                limits(excess_features.len(), line_count, 1, 1, 71, 65_536, 65_536,),
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::FeatureLimit
        );
        assert_eq!(
            decode_manifest(
                source.as_bytes(),
                limits(source.len(), line_count, 1, 2, 70, 65_536, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::StringLimit
        );
        assert_eq!(
            decode_manifest(
                source.as_bytes(),
                limits(source.len(), line_count, 1, 2, 71, 65_535, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ObjectLimit
        );

        let two_entries = format!(
            "{}\n{}",
            source.trim_end(),
            entry_block("second.pdf", ABC_HASH, 65_536)
        );
        assert_eq!(
            decode_manifest(
                two_entries.as_bytes(),
                limits(two_entries.len(), 30, 1, 2, 71, 65_536, 131_072)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::EntryLimit
        );
    }

    #[test]
    fn rejects_duplicate_object_and_path_records() {
        let source = canonical("first.pdf", HASH, 65_536);
        let duplicate_object = format!(
            "{}\n{}",
            source.trim_end(),
            entry_block("second.pdf", HASH, 65_536)
        );
        assert_eq!(
            decode_manifest(duplicate_object.as_bytes(), CorpusManifestLimits::default())
                .err()
                .unwrap()
                .code,
            CorpusManifestErrorCode::DuplicateObject
        );

        let duplicate_path = format!(
            "{}\n{}",
            source.trim_end(),
            entry_block("first.pdf", ABC_HASH, 65_536)
        );
        assert_eq!(
            decode_manifest(duplicate_path.as_bytes(), CorpusManifestLimits::default())
                .err()
                .unwrap()
                .code,
            CorpusManifestErrorCode::DuplicatePath
        );
    }

    #[test]
    fn streams_object_hashes_at_exact_and_exceeded_limits() {
        let directory = TempDir::new();
        let object_path = directory.path().join("object.pdf");
        fs::write(&object_path, b"abc").unwrap();

        let manifest = decode_manifest(
            canonical("object.pdf", ABC_HASH, 3).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        let verified = verify_manifest_objects(
            manifest,
            directory.path(),
            limits(4096, 64, 1, 2, 128, 3, 3),
        )
        .unwrap();
        assert_eq!(verified.verified_objects(), 1);
        assert_eq!(verified.verified_bytes(), 3);

        let object_limited = decode_manifest(
            canonical("object.pdf", ABC_HASH, 2).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(
                object_limited,
                directory.path(),
                limits(4096, 64, 1, 2, 128, 3, 3)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ObjectLimit
        );

        let total_limited = decode_manifest(
            canonical("object.pdf", ABC_HASH, 3).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(
                total_limited,
                directory.path(),
                limits(4096, 64, 1, 2, 128, 3, 2)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::TotalObjectLimit
        );

        let second_hash = hex_digest(&sha256(b"def").unwrap());
        fs::write(directory.path().join("second.pdf"), b"def").unwrap();
        let two_objects = format!(
            "{}\n\n{}",
            canonical("object.pdf", ABC_HASH, 3).trim_end(),
            entry_block("second.pdf", &second_hash, 3)
        );
        let exact_total = decode_manifest(
            two_objects.as_bytes(),
            limits(two_objects.len(), 30, 2, 2, 128, 3, 6),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(
                exact_total,
                directory.path(),
                limits(two_objects.len(), 30, 2, 2, 128, 3, 6),
            )
            .unwrap()
            .verified_bytes(),
            6
        );
        let exceeded_total = decode_manifest(
            two_objects.as_bytes(),
            limits(two_objects.len(), 30, 2, 2, 128, 3, 6),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(
                exceeded_total,
                directory.path(),
                limits(two_objects.len(), 30, 2, 2, 128, 3, 5),
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::TotalObjectLimit
        );
    }

    #[test]
    fn rejects_missing_mismatched_and_symbolic_objects() {
        let directory = TempDir::new();
        fs::write(directory.path().join("object.pdf"), b"abc").unwrap();

        let mismatch = decode_manifest(
            canonical("object.pdf", HASH, 3).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        let mismatch_error =
            verify_manifest_objects(mismatch, directory.path(), CorpusManifestLimits::default())
                .err()
                .unwrap();
        assert_eq!(
            mismatch_error.code,
            CorpusManifestErrorCode::ObjectHashMismatch
        );
        assert!(!mismatch_error.to_string().contains(HASH));

        let missing = decode_manifest(
            canonical("missing.pdf", ABC_HASH, 3).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(missing, directory.path(), CorpusManifestLimits::default())
                .err()
                .unwrap()
                .code,
            CorpusManifestErrorCode::ObjectUnavailable
        );

        fs::create_dir(directory.path().join("nested")).unwrap();
        let directory_object = decode_manifest(
            canonical("nested", ABC_HASH, 3).as_bytes(),
            CorpusManifestLimits::default(),
        )
        .unwrap();
        assert_eq!(
            verify_manifest_objects(
                directory_object,
                directory.path(),
                CorpusManifestLimits::default()
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ObjectUnavailable
        );

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("object.pdf", directory.path().join("link.pdf")).unwrap();
            let symbolic = decode_manifest(
                canonical("link.pdf", ABC_HASH, 3).as_bytes(),
                CorpusManifestLimits::default(),
            )
            .unwrap();
            assert_eq!(
                verify_manifest_objects(
                    symbolic,
                    directory.path(),
                    CorpusManifestLimits::default()
                )
                .err()
                .unwrap()
                .code,
                CorpusManifestErrorCode::UnsafePath
            );

            fs::create_dir(directory.path().join("real-directory")).unwrap();
            fs::write(directory.path().join("real-directory/nested.pdf"), b"abc").unwrap();
            std::os::unix::fs::symlink("real-directory", directory.path().join("linked-directory"))
                .unwrap();
            let intermediate_symbolic = decode_manifest(
                canonical("linked-directory/nested.pdf", ABC_HASH, 3).as_bytes(),
                CorpusManifestLimits::default(),
            )
            .unwrap();
            assert_eq!(
                verify_manifest_objects(
                    intermediate_symbolic,
                    directory.path(),
                    CorpusManifestLimits::default()
                )
                .err()
                .unwrap()
                .code,
                CorpusManifestErrorCode::UnsafePath
            );

            let root_link = directory.path().with_extension("root-link");
            std::os::unix::fs::symlink(directory.path(), &root_link).unwrap();
            let root_symbolic = decode_manifest(
                canonical("object.pdf", ABC_HASH, 3).as_bytes(),
                CorpusManifestLimits::default(),
            )
            .unwrap();
            assert_eq!(
                verify_manifest_objects(root_symbolic, &root_link, CorpusManifestLimits::default())
                    .err()
                    .unwrap()
                    .code,
                CorpusManifestErrorCode::ObjectRootUnavailable
            );
            fs::remove_file(root_link).unwrap();
        }
    }

    #[test]
    fn bounded_manifest_file_loading_rejects_symlinks() {
        let directory = TempDir::new();
        let source = canonical("object.pdf", HASH, 65_536);
        let manifest_path = directory.path().join("manifest.toml");
        fs::write(&manifest_path, &source).unwrap();
        assert!(load_manifest_file(&manifest_path, CorpusManifestLimits::default()).is_ok());
        assert_eq!(
            load_manifest_file(
                &manifest_path,
                limits(source.len() - 1, 64, 1, 2, 128, 65_536, 65_536)
            )
            .err()
            .unwrap()
            .code,
            CorpusManifestErrorCode::ManifestLimit
        );

        #[cfg(unix)]
        {
            let link = directory.path().join("manifest-link.toml");
            std::os::unix::fs::symlink(&manifest_path, &link).unwrap();
            assert_eq!(
                load_manifest_file(&link, CorpusManifestLimits::default())
                    .err()
                    .unwrap()
                    .code,
                CorpusManifestErrorCode::ManifestUnavailable
            );
        }
    }

    #[test]
    fn invalid_limit_configurations_are_stable() {
        for result in [
            CorpusManifestLimits::new(0, 1, 1, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 0, 1, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 0, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 0, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, 0, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, 1, 0, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, 1, 1, 0),
            CorpusManifestLimits::new(HARD_MAX_MANIFEST_BYTES + 1, 1, 1, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, HARD_MAX_LINES + 1, 1, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, HARD_MAX_ENTRIES + 1, 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, HARD_MAX_FEATURES_PER_ENTRY + 1, 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, HARD_MAX_STRING_BYTES + 1, 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, 1, HARD_MAX_OBJECT_BYTES + 1, 1),
            CorpusManifestLimits::new(1, 1, 1, 1, 1, 1, HARD_MAX_TOTAL_OBJECT_BYTES + 1),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code, CorpusManifestErrorCode::InvalidLimits);
            assert_eq!(error.category, CorpusManifestErrorCategory::Configuration);
            assert_eq!(
                error.recoverability,
                CorpusManifestRecoverability::CorrectConfiguration
            );
            assert_eq!(error.diagnostic_id, "RPE-CORPUS-MANIFEST-0001");
        }
    }

    #[test]
    fn diagnostic_category_and_recovery_contract_is_stable() {
        use CorpusManifestErrorCategory::{
            Availability, Configuration, Integrity, Internal, ResourceLimit, Structure, Syntax,
            Unsupported,
        };
        use CorpusManifestErrorCode::{
            DuplicateField, DuplicateObject, DuplicatePath, EntryLimit, FeatureLimit, HashFailed,
            InvalidLimits, InvalidSyntax, InvalidUtf8, InvalidValue, LineLimit, ManifestLimit,
            ManifestUnavailable, MissingField, NonCanonical, ObjectHashMismatch, ObjectLimit,
            ObjectRootUnavailable, ObjectUnavailable, StringLimit, TotalObjectLimit, UnknownField,
            UnsafePath, UnsupportedSchema,
        };
        use CorpusManifestRecoverability::{
            CorrectConfiguration, CorrectManifest, DoNotRetry, ReduceInput, RestoreObject,
            SelectSupportedSchema,
        };

        let cases = [
            (InvalidLimits, "0001", Configuration, CorrectConfiguration),
            (ManifestLimit, "0002", ResourceLimit, ReduceInput),
            (LineLimit, "0003", ResourceLimit, ReduceInput),
            (EntryLimit, "0004", ResourceLimit, ReduceInput),
            (FeatureLimit, "0005", ResourceLimit, ReduceInput),
            (StringLimit, "0006", ResourceLimit, ReduceInput),
            (InvalidUtf8, "0007", Syntax, CorrectManifest),
            (InvalidSyntax, "0008", Syntax, CorrectManifest),
            (
                UnsupportedSchema,
                "0009",
                Unsupported,
                SelectSupportedSchema,
            ),
            (UnknownField, "0010", Structure, CorrectManifest),
            (DuplicateField, "0011", Structure, CorrectManifest),
            (MissingField, "0012", Structure, CorrectManifest),
            (InvalidValue, "0013", Structure, CorrectManifest),
            (DuplicateObject, "0014", Structure, CorrectManifest),
            (DuplicatePath, "0015", Structure, CorrectManifest),
            (UnsafePath, "0016", Structure, CorrectManifest),
            (NonCanonical, "0017", Structure, CorrectManifest),
            (ManifestUnavailable, "0018", Availability, RestoreObject),
            (ObjectRootUnavailable, "0019", Availability, RestoreObject),
            (ObjectUnavailable, "0020", Availability, RestoreObject),
            (ObjectLimit, "0021", ResourceLimit, ReduceInput),
            (TotalObjectLimit, "0022", ResourceLimit, ReduceInput),
            (ObjectHashMismatch, "0023", Integrity, RestoreObject),
            (HashFailed, "0024", Internal, DoNotRetry),
        ];
        for (code, suffix, category, recoverability) in cases {
            let error = manifest_error(code, Some(7), Some(3));
            assert_eq!(error.category, category);
            assert_eq!(error.recoverability, recoverability);
            assert_eq!(error.diagnostic_id, format!("RPE-CORPUS-MANIFEST-{suffix}"));
            assert_eq!(error.line, Some(7));
            assert_eq!(error.entry_index, Some(3));
        }
    }

    fn entry_block(path: &str, hash: &str, max_bytes: u64) -> String {
        let entry = canonical(path, hash, max_bytes)
            .split_once("[[entry]]\n")
            .unwrap()
            .1
            .to_string();
        format!("[[entry]]\n{entry}")
    }

    fn limits(
        manifest_bytes: usize,
        lines: usize,
        entries: usize,
        features: usize,
        string_bytes: usize,
        object_bytes: u64,
        total_bytes: u64,
    ) -> CorpusManifestLimits {
        CorpusManifestLimits::new(
            manifest_bytes,
            lines,
            entries,
            features,
            string_bytes,
            object_bytes,
            total_bytes,
        )
        .unwrap()
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pdf-rs-corpus-manifest-{}-{sequence}",
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
