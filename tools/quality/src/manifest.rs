use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

const SECTION_SCHEMAS: &[(&str, &[&str])] = &[
    (
        "identity",
        &["id", "title", "owner", "status", "introduced_in"],
    ),
    (
        "specification",
        &["document", "version", "clauses", "interpretation"],
    ),
    (
        "provenance",
        &[
            "kind",
            "source",
            "sha256",
            "license",
            "redistributable",
            "access",
        ],
    ),
    ("features", &["ids", "requirements"]),
    (
        "validity",
        &["class", "strict_expected", "recovery_expected"],
    ),
    (
        "expected",
        &[
            "parse",
            "scene",
            "text",
            "pixel",
            "diagnostic",
            "capability",
            "error",
        ],
    ),
    (
        "oracle",
        &[
            "level",
            "derivation",
            "reviewers",
            "reference_may_generate",
            "last_reviewed",
        ],
    ),
    (
        "budget",
        &[
            "max_input_bytes",
            "max_objects",
            "max_resolve_depth",
            "max_stream_output_bytes",
            "max_total_decode_bytes",
            "max_image_pixels",
            "max_path_segments",
            "max_scene_commands",
            "max_group_depth",
            "operator_fuel",
            "decode_fuel",
            "watchdog_ms",
        ],
    ),
    (
        "render",
        &[
            "width",
            "height",
            "dpr_milli",
            "color_profile",
            "alpha",
            "antialias",
            "renderer_epoch",
        ],
    ),
    ("tolerance", &["mode"]),
    ("runners", &["native", "external_observation"]),
    ("history", &["entries"]),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseManifest {
    values: BTreeMap<String, BTreeMap<String, String>>,
}

impl CaseManifest {
    pub fn case_id(&self) -> &str {
        self.string("identity", "id")
            .expect("a validated manifest always contains identity.id")
    }

    pub fn source_sha256(&self) -> &str {
        self.string("provenance", "sha256")
            .expect("a validated manifest always contains provenance.sha256")
    }

    pub fn raw(&self, section: &str, key: &str) -> Option<&str> {
        self.values
            .get(section)
            .and_then(|values| values.get(key))
            .map(String::as_str)
    }

    pub fn string(&self, section: &str, key: &str) -> Option<&str> {
        self.raw(section, key).and_then(unquote)
    }

    pub fn positive_u64(&self, section: &str, key: &str) -> Option<u64> {
        self.raw(section, key)
            .and_then(parse_canonical_positive_integer)
    }

    pub fn boolean(&self, section: &str, key: &str) -> Option<bool> {
        match self.raw(section, key) {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        }
    }

    pub fn string_array<'a>(&'a self, section: &str, key: &str) -> Option<Vec<&'a str>> {
        parse_string_array(self.raw(section, key)?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestDiagnostic {
    pub code: &'static str,
    pub line: Option<usize>,
    pub section: Option<String>,
    pub key: Option<String>,
}

impl ManifestDiagnostic {
    fn at_line(code: &'static str, line: usize) -> Self {
        Self {
            code,
            line: Some(line),
            section: None,
            key: None,
        }
    }

    fn field(code: &'static str, section: &str, key: &str) -> Self {
        Self {
            code,
            line: None,
            section: Some(section.into()),
            key: Some(key.into()),
        }
    }

    fn section(code: &'static str, section: &str) -> Self {
        Self {
            code,
            line: None,
            section: Some(section.into()),
            key: None,
        }
    }
}

impl fmt::Display for ManifestDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.code)?;
        if let Some(line) = self.line {
            write!(formatter, " line={line}")?;
        }
        if let Some(section) = &self.section {
            write!(formatter, " section={section}")?;
        }
        if let Some(key) = &self.key {
            write!(formatter, " key={key}")?;
        }
        Ok(())
    }
}

pub fn validate_manifest_file(path: &Path) -> Result<CaseManifest, Vec<ManifestDiagnostic>> {
    let input = fs::read_to_string(path).map_err(|_| {
        vec![ManifestDiagnostic {
            code: "RPE-MANIFEST-0001",
            line: None,
            section: None,
            key: None,
        }]
    })?;
    validate_manifest(&input)
}

pub fn validate_manifest(input: &str) -> Result<CaseManifest, Vec<ManifestDiagnostic>> {
    let parsed = match parse(input) {
        Ok(parsed) => parsed,
        Err(diagnostic) => return Err(vec![diagnostic]),
    };
    let mut diagnostics = Vec::new();

    match parsed.root.get("schema") {
        None => diagnostics.push(ManifestDiagnostic::field(
            "RPE-MANIFEST-0007",
            "root",
            "schema",
        )),
        Some(value) if value != "1" => diagnostics.push(ManifestDiagnostic::field(
            "RPE-MANIFEST-0008",
            "root",
            "schema",
        )),
        Some(_) => {}
    }

    for (section, required_keys) in SECTION_SCHEMAS {
        let Some(values) = parsed.sections.get(*section) else {
            diagnostics.push(ManifestDiagnostic::section("RPE-MANIFEST-0009", section));
            continue;
        };
        for key in *required_keys {
            if !values.contains_key(*key) {
                diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0010", section, key));
            }
        }
    }

    if diagnostics.is_empty() {
        validate_value_shapes(&parsed.sections, &mut diagnostics);
    }

    if diagnostics.is_empty() {
        Ok(CaseManifest {
            values: parsed.sections,
        })
    } else {
        Err(diagnostics)
    }
}

struct ParsedManifest {
    root: BTreeMap<String, String>,
    sections: BTreeMap<String, BTreeMap<String, String>>,
}

fn parse(input: &str) -> Result<ParsedManifest, ManifestDiagnostic> {
    let known: BTreeMap<&str, BTreeSet<&str>> = SECTION_SCHEMAS
        .iter()
        .map(|(section, keys)| (*section, keys.iter().copied().collect()))
        .collect();
    let mut root = BTreeMap::new();
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current_section: Option<String> = None;

    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line)
            .map_err(|_| ManifestDiagnostic::at_line("RPE-MANIFEST-0002", line_number))?
            .trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('[') {
            if !line.ends_with(']') || line.starts_with("[[") || line.ends_with("]]") {
                return Err(ManifestDiagnostic::at_line(
                    "RPE-MANIFEST-0002",
                    line_number,
                ));
            }
            let section = &line[1..line.len() - 1];
            if !known.contains_key(section) {
                return Err(ManifestDiagnostic::section("RPE-MANIFEST-0005", section));
            }
            if sections.contains_key(section) {
                return Err(ManifestDiagnostic::section("RPE-MANIFEST-0003", section));
            }
            sections.insert(section.into(), BTreeMap::new());
            current_section = Some(section.into());
            continue;
        }

        let (key, value) = split_assignment(line)
            .ok_or_else(|| ManifestDiagnostic::at_line("RPE-MANIFEST-0002", line_number))?;
        if !is_key(key) || value.is_empty() {
            return Err(ManifestDiagnostic::at_line(
                "RPE-MANIFEST-0002",
                line_number,
            ));
        }

        if let Some(section) = &current_section {
            let allowed = known
                .get(section.as_str())
                .expect("current sections are validated before insertion");
            if !allowed.contains(key) {
                return Err(ManifestDiagnostic::field("RPE-MANIFEST-0006", section, key));
            }
            let values = sections
                .get_mut(section)
                .expect("current section is inserted before its fields");
            if values.insert(key.into(), value.into()).is_some() {
                return Err(ManifestDiagnostic::field("RPE-MANIFEST-0004", section, key));
            }
        } else {
            if key != "schema" {
                return Err(ManifestDiagnostic::field("RPE-MANIFEST-0006", "root", key));
            }
            if root.insert(key.into(), value.into()).is_some() {
                return Err(ManifestDiagnostic::field("RPE-MANIFEST-0004", "root", key));
            }
        }
    }

    Ok(ParsedManifest { root, sections })
}

fn validate_value_shapes(
    sections: &BTreeMap<String, BTreeMap<String, String>>,
    diagnostics: &mut Vec<ManifestDiagnostic>,
) {
    let string_fields = [
        ("identity", "id"),
        ("identity", "title"),
        ("identity", "owner"),
        ("identity", "status"),
        ("identity", "introduced_in"),
        ("specification", "document"),
        ("specification", "version"),
        ("specification", "interpretation"),
        ("provenance", "kind"),
        ("provenance", "source"),
        ("provenance", "sha256"),
        ("provenance", "license"),
        ("provenance", "access"),
        ("validity", "class"),
        ("validity", "strict_expected"),
        ("validity", "recovery_expected"),
        ("oracle", "level"),
        ("oracle", "derivation"),
        ("oracle", "last_reviewed"),
        ("render", "color_profile"),
        ("render", "alpha"),
        ("render", "antialias"),
        ("render", "renderer_epoch"),
        ("tolerance", "mode"),
    ];
    for (section, key) in string_fields {
        let value = value(sections, section, key);
        if unquote(value).is_none_or(|value| value.trim().is_empty()) {
            diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0011", section, key));
        }
    }

    let array_fields = [
        ("specification", "clauses", true),
        ("features", "ids", true),
        ("features", "requirements", true),
        ("oracle", "reviewers", true),
        ("runners", "native", true),
        ("runners", "external_observation", false),
        ("history", "entries", false),
    ];
    for (section, key, must_not_be_empty) in array_fields {
        if !is_string_array(value(sections, section, key), must_not_be_empty) {
            diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0016", section, key));
        }
    }

    let bool_fields = [
        ("provenance", "redistributable"),
        ("expected", "parse"),
        ("expected", "scene"),
        ("expected", "text"),
        ("expected", "pixel"),
        ("expected", "diagnostic"),
        ("expected", "capability"),
        ("expected", "error"),
        ("oracle", "reference_may_generate"),
    ];
    for (section, key) in bool_fields {
        if !matches!(value(sections, section, key), "true" | "false") {
            diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0015", section, key));
        }
    }

    let positive_integer_fields = [
        ("budget", "max_input_bytes"),
        ("budget", "max_objects"),
        ("budget", "max_resolve_depth"),
        ("budget", "max_stream_output_bytes"),
        ("budget", "max_total_decode_bytes"),
        ("budget", "max_image_pixels"),
        ("budget", "max_path_segments"),
        ("budget", "max_scene_commands"),
        ("budget", "max_group_depth"),
        ("budget", "operator_fuel"),
        ("budget", "decode_fuel"),
        ("budget", "watchdog_ms"),
        ("render", "width"),
        ("render", "height"),
        ("render", "dpr_milli"),
    ];
    for (section, key) in positive_integer_fields {
        if parse_canonical_positive_integer(value(sections, section, key)).is_none() {
            diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0014", section, key));
        }
    }

    let case_id = unquote(value(sections, "identity", "id")).unwrap_or_default();
    if case_id.is_empty()
        || case_id.starts_with('/')
        || case_id.ends_with('/')
        || !case_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-._/".contains(&byte)
        })
    {
        diagnostics.push(ManifestDiagnostic::field(
            "RPE-MANIFEST-0017",
            "identity",
            "id",
        ));
    }

    let hash = unquote(value(sections, "provenance", "sha256")).unwrap_or_default();
    if hash.len() != 71
        || !hash.starts_with("sha256:")
        || !hash[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        diagnostics.push(ManifestDiagnostic::field(
            "RPE-MANIFEST-0012",
            "provenance",
            "sha256",
        ));
    }

    validate_enum(
        sections,
        diagnostics,
        "oracle",
        "level",
        &["O0", "O1", "O2", "O3", "O4"],
    );
    validate_enum(
        sections,
        diagnostics,
        "validity",
        "class",
        &["valid", "invalid", "ambiguous", "real-world-tolerated"],
    );
    validate_enum(
        sections,
        diagnostics,
        "tolerance",
        "mode",
        &["exact", "coverage-aware", "color-aware", "manual-review"],
    );

    let validity = unquote(value(sections, "validity", "class"));
    let strict = unquote(value(sections, "validity", "strict_expected"));
    let expects_error = value(sections, "expected", "error") == "true";
    if validity == Some("valid") && strict == Some("success") && expects_error {
        diagnostics.push(ManifestDiagnostic::field(
            "RPE-MANIFEST-0021",
            "expected",
            "error",
        ));
    }

    if unquote(value(sections, "identity", "status")) == Some("active") {
        if parse_string_array(value(sections, "oracle", "reviewers")).is_some_and(|reviewers| {
            reviewers
                .iter()
                .any(|reviewer| reviewer.to_ascii_lowercase().contains("pending"))
        }) {
            diagnostics.push(ManifestDiagnostic::field(
                "RPE-MANIFEST-0022",
                "oracle",
                "reviewers",
            ));
        }
        let last_reviewed = unquote(value(sections, "oracle", "last_reviewed")).unwrap_or_default();
        if !is_canonical_date(last_reviewed) {
            diagnostics.push(ManifestDiagnostic::field(
                "RPE-MANIFEST-0023",
                "oracle",
                "last_reviewed",
            ));
        }
    }
}

fn is_canonical_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[..4].parse::<u32>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..].parse::<u32>().ok();
    let (Some(year), Some(month), Some(day)) = (year, month, day) else {
        return false;
    };
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

fn validate_enum(
    sections: &BTreeMap<String, BTreeMap<String, String>>,
    diagnostics: &mut Vec<ManifestDiagnostic>,
    section: &str,
    key: &str,
    allowed: &[&str],
) {
    let parsed = unquote(value(sections, section, key));
    if !parsed.is_some_and(|value| allowed.contains(&value)) {
        diagnostics.push(ManifestDiagnostic::field("RPE-MANIFEST-0013", section, key));
    }
}

fn value<'a>(
    sections: &'a BTreeMap<String, BTreeMap<String, String>>,
    section: &str,
    key: &str,
) -> &'a str {
    sections
        .get(section)
        .and_then(|values| values.get(key))
        .map(String::as_str)
        .expect("required fields are checked before value validation")
}

fn strip_comment(line: &str) -> Result<&str, ()> {
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'#' {
            return Ok(&line[..index]);
        }
    }
    if in_string || escaped {
        Err(())
    } else {
        Ok(line)
    }
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let index = line.find('=')?;
    let key = line[..index].trim();
    let value = line[index + 1..].trim();
    Some((key, value))
}

fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn unquote(value: &str) -> Option<&str> {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        if inner
            .bytes()
            .any(|byte| byte == b'"' || byte == b'\\' || byte.is_ascii_control())
        {
            None
        } else {
            Some(inner)
        }
    } else {
        None
    }
}

fn parse_canonical_positive_integer(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

fn is_string_array(value: &str, must_not_be_empty: bool) -> bool {
    parse_string_array(value).is_some_and(|values| !must_not_be_empty || !values.is_empty())
}

fn parse_string_array(value: &str) -> Option<Vec<&str>> {
    if value.len() < 2 || !value.starts_with('[') || !value.ends_with(']') {
        return None;
    }
    let inner = value[1..value.len() - 1].trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|item| unquote(item.trim()).filter(|value| !value.trim().is_empty()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::validate_manifest;

    const VALID: &str = r#"
schema = 1
[identity]
id = "infrastructure/synthetic-failure-bundle-001"
title = "Synthetic failure bundle"
owner = "quality-corpus"
status = "active"
introduced_in = "0.1.0"
[specification]
document = "RPE-ARCH-001"
version = "0.3"
clauses = ["15.3/M0"]
interpretation = "Exercise every synthetic artifact channel."
[provenance]
kind = "self-authored-generated"
source = "tools/generate"
sha256 = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
license = "LicenseRef-PDF.rs-SelfAuthored-Test"
redistributable = false
access = "repository"
[features]
ids = ["quality.failure-bundle"]
requirements = ["m0.synthetic-artifacts.v1"]
[validity]
class = "valid"
strict_expected = "success"
recovery_expected = "not-applicable"
[expected]
parse = true
scene = true
text = true
pixel = true
diagnostic = true
capability = true
error = false
[oracle]
level = "O1"
derivation = "expected/oracle.md"
reviewers = ["spec-conformance"]
reference_may_generate = false
last_reviewed = "2026-07-13"
[budget]
max_input_bytes = 65536
max_objects = 64
max_resolve_depth = 16
max_stream_output_bytes = 1048576
max_total_decode_bytes = 1048576
max_image_pixels = 4096
max_path_segments = 4096
max_scene_commands = 4096
max_group_depth = 8
operator_fuel = 20000
decode_fuel = 1048576
watchdog_ms = 500
[render]
width = 4
height = 4
dpr_milli = 1000
color_profile = "srgb-reference-v1"
alpha = "straight"
antialias = "reference-v1"
renderer_epoch = "synthetic-v1"
[tolerance]
mode = "exact"
[runners]
native = ["synthetic-m0"]
external_observation = []
[history]
entries = ["2026-07-13: introduced"]
"#;

    #[test]
    fn accepts_the_canonical_manifest() {
        let manifest = validate_manifest(VALID).unwrap();
        assert_eq!(
            manifest.case_id(),
            "infrastructure/synthetic-failure-bundle-001"
        );
    }

    #[test]
    fn rejects_each_missing_section() {
        for section in [
            "identity",
            "specification",
            "provenance",
            "features",
            "validity",
            "expected",
            "oracle",
            "budget",
            "render",
            "tolerance",
            "runners",
            "history",
        ] {
            let marker = format!("[{section}]");
            let start = VALID.find(&marker).unwrap();
            let rest = &VALID[start + marker.len()..];
            let end = rest
                .find("\n[")
                .map_or(VALID.len(), |offset| start + marker.len() + offset);
            let input = format!("{}{}", &VALID[..start], &VALID[end..]);
            let errors = validate_manifest(&input).unwrap_err();
            assert!(errors.iter().any(|error| {
                error.code == "RPE-MANIFEST-0009" && error.section.as_deref() == Some(section)
            }));
        }
    }

    #[test]
    fn rejects_duplicate_fields_and_malformed_lines() {
        let duplicate = VALID.replace(
            "title = \"Synthetic failure bundle\"",
            "title = \"Synthetic failure bundle\"\ntitle = \"Again\"",
        );
        assert_eq!(
            validate_manifest(&duplicate).unwrap_err()[0].code,
            "RPE-MANIFEST-0004"
        );
        let malformed = VALID.replace("schema = 1", "schema: 1");
        assert_eq!(
            validate_manifest(&malformed).unwrap_err()[0].code,
            "RPE-MANIFEST-0002"
        );
    }

    #[test]
    fn rejects_hash_oracle_and_budget_shape_errors() {
        let malformed_hash = VALID.replace(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:nope",
        );
        assert!(
            validate_manifest(&malformed_hash)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0012")
        );

        let invalid_oracle = VALID.replace("level = \"O1\"", "level = \"O9\"");
        assert!(
            validate_manifest(&invalid_oracle)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0013")
        );

        for bad in ["0", "many"] {
            let invalid_budget = VALID.replace("max_objects = 64", &format!("max_objects = {bad}"));
            assert!(
                validate_manifest(&invalid_budget)
                    .unwrap_err()
                    .iter()
                    .any(|error| error.code == "RPE-MANIFEST-0014")
            );
        }
    }

    #[test]
    fn rejects_missing_license_hash_oracle_and_budget_fields() {
        for line in [
            "license = \"LicenseRef-PDF.rs-SelfAuthored-Test\"\n",
            "sha256 = \"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n",
            "level = \"O1\"\n",
            "max_input_bytes = 65536\n",
        ] {
            let input = VALID.replace(line, "");
            assert!(
                validate_manifest(&input)
                    .unwrap_err()
                    .iter()
                    .any(|error| error.code == "RPE-MANIFEST-0010")
            );
        }
    }

    #[test]
    fn rejects_blank_required_strings_and_array_items() {
        for input in [
            VALID.replace(
                "license = \"LicenseRef-PDF.rs-SelfAuthored-Test\"",
                "license = \"\"",
            ),
            VALID.replace(
                "derivation = \"expected/oracle.md\"",
                "derivation = \"   \"",
            ),
            VALID.replace(
                "reviewers = [\"spec-conformance\"]",
                "reviewers = [\"   \"]",
            ),
        ] {
            assert!(validate_manifest(&input).is_err());
        }
    }

    #[test]
    fn preserves_hash_characters_inside_strings() {
        let input = VALID.replace(
            "Exercise every synthetic artifact channel.",
            "Exercise #1 synthetic artifact channel.",
        );
        assert!(validate_manifest(&input).is_ok());
    }

    #[test]
    fn rejects_concatenated_strings_and_noncanonical_integers() {
        let adjacent = VALID.replace(
            "title = \"Synthetic failure bundle\"",
            "title = \"Synthetic\" \"failure bundle\"",
        );
        assert!(
            validate_manifest(&adjacent)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0011")
        );

        let leading_zero = VALID.replace("max_objects = 64", "max_objects = 064");
        assert!(
            validate_manifest(&leading_zero)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0014")
        );
    }

    #[test]
    fn rejects_successful_valid_input_that_expects_an_error() {
        let contradictory = VALID.replace("error = false", "error = true");
        assert!(
            validate_manifest(&contradictory)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0021")
        );
    }

    #[test]
    fn requires_completed_oracle_review_for_active_cases() {
        let pending_reviewer = VALID.replace(
            "reviewers = [\"spec-conformance\"]",
            "reviewers = [\"pending-independent-review\"]",
        );
        assert!(
            validate_manifest(&pending_reviewer)
                .unwrap_err()
                .iter()
                .any(|error| error.code == "RPE-MANIFEST-0022")
        );

        for date in ["pending", "2026-02-30", "2026-7-15"] {
            let pending_date = VALID.replace(
                "last_reviewed = \"2026-07-13\"",
                &format!("last_reviewed = \"{date}\""),
            );
            assert!(
                validate_manifest(&pending_date)
                    .unwrap_err()
                    .iter()
                    .any(|error| error.code == "RPE-MANIFEST-0023")
            );
        }

        let draft = VALID
            .replace("status = \"active\"", "status = \"draft\"")
            .replace(
                "reviewers = [\"spec-conformance\"]",
                "reviewers = [\"pending-independent-review\"]",
            )
            .replace(
                "last_reviewed = \"2026-07-13\"",
                "last_reviewed = \"pending\"",
            );
        assert!(validate_manifest(&draft).is_ok());
    }
}
