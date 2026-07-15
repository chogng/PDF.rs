use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

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

    fn root(code: &'static str, field: &str) -> Self {
        Self {
            code,
            profile: None,
            field: Some(field.into()),
            line: None,
        }
    }

    fn profile(code: &'static str, profile: &str, field: &str) -> Self {
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

pub(crate) fn validate_maturity_file(
    path: &Path,
) -> Result<MaturityReport, Vec<MaturityDiagnostic>> {
    let input = fs::read_to_string(path)
        .map_err(|_| vec![MaturityDiagnostic::root("RPE-MATURITY-0002", "unreadable")])?;
    validate_maturity(&input)
}

fn validate_maturity(input: &str) -> Result<MaturityReport, Vec<MaturityDiagnostic>> {
    let parsed = parse(input)?;
    let mut diagnostics = Vec::new();

    for key in ROOT_KEYS {
        if !parsed.root.contains_key(*key) {
            diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0003", key));
        }
    }
    if parsed.root.get("schema").map(String::as_str) != Some("1") {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0004", "schema"));
    }
    if parsed.root.get("status").and_then(|value| unquote(value)) != Some("active") {
        diagnostics.push(MaturityDiagnostic::root("RPE-MATURITY-0004", "status"));
    }
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
        let identity = profile
            .get("id")
            .and_then(|value| unquote(value))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("<profile-{index}>"));
        for key in PROFILE_KEYS {
            if !profile.contains_key(*key) {
                diagnostics.push(MaturityDiagnostic::profile(
                    "RPE-MATURITY-0006",
                    &identity,
                    key,
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

        match string(profile.get("state")) {
            Some("PLANNED") => report.planned += 1,
            Some("REFERENCE") => {
                report.reference += 1;
                require_reference_evidence(profile, &identity, &mut diagnostics);
            }
            Some("DIFFERENTIAL") => {
                report.differential += 1;
                require_reference_evidence(profile, &identity, &mut diagnostics);
                require_differential_evidence(profile, &identity, &mut diagnostics);
            }
            _ => diagnostics.push(MaturityDiagnostic::profile(
                "RPE-MATURITY-0009",
                &identity,
                "state",
            )),
        }
    }

    if diagnostics.is_empty() {
        Ok(report)
    } else {
        Err(diagnostics)
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

fn parse(input: &str) -> Result<ParsedProfiles, Vec<MaturityDiagnostic>> {
    let allowed_root: BTreeSet<&str> = ROOT_KEYS.iter().copied().collect();
    let allowed_profile: BTreeSet<&str> = PROFILE_KEYS.iter().copied().collect();
    let mut root = BTreeMap::new();
    let mut profiles: Vec<BTreeMap<String, String>> = Vec::new();
    let mut in_profile = false;

    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line)
            .ok_or_else(|| vec![MaturityDiagnostic::syntax(line_number)])?
            .trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[profile]]" {
            profiles.push(BTreeMap::new());
            in_profile = true;
            continue;
        }
        let (key, value) =
            split_assignment(line).ok_or_else(|| vec![MaturityDiagnostic::syntax(line_number)])?;
        if value.is_empty() || !valid_value(value) {
            return Err(vec![MaturityDiagnostic::syntax(line_number)]);
        }
        let (allowed, destination) = if in_profile {
            (
                &allowed_profile,
                profiles.last_mut().expect("a profile was pushed"),
            )
        } else {
            (&allowed_root, &mut root)
        };
        if !allowed.contains(key) || destination.insert(key.into(), value.into()).is_some() {
            return Err(vec![MaturityDiagnostic::syntax(line_number)]);
        }
    }
    Ok(ParsedProfiles { root, profiles })
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
    value.parse::<u64>().is_ok() || unquote(value).is_some() || parse_string_array(value).is_some()
}

fn string(value: Option<&String>) -> Option<&str> {
    value.and_then(|value| unquote(value))
}

fn string_array(value: Option<&String>) -> Option<Vec<&str>> {
    parse_string_array(value?)
}

fn unquote(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.contains(['"', '\n', '\r']))
}

fn parse_string_array(value: &str) -> Option<Vec<&str>> {
    let body = value.strip_prefix('[')?.strip_suffix(']')?.trim();
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

#[cfg(test)]
mod tests {
    use super::{validate_maturity, validate_maturity_file};
    use std::path::Path;

    #[test]
    fn repository_profiles_are_valid_and_truthfully_planned() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/traceability/capability-profiles.toml");
        let report = validate_maturity_file(&path).unwrap();
        assert_eq!(report.profiles, 4);
        assert_eq!(report.planned, 4);
        assert_eq!(report.reference, 0);
        assert_eq!(report.differential, 0);
    }

    #[test]
    fn rejects_paper_reference_promotion() {
        let input = include_str!("../../../docs/traceability/capability-profiles.toml").replacen(
            "state = \"PLANNED\"",
            "state = \"REFERENCE\"",
            1,
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
        let input = include_str!("../../../docs/traceability/capability-profiles.toml").replacen(
            "state = \"PLANNED\"",
            "state = \"DIFFERENTIAL\"",
            1,
        );
        let diagnostics = validate_maturity(&input).unwrap_err();
        for field in [
            "o2_adjudications",
            "fuzz_targets",
            "holdout_manifest",
            "benchmark_report",
            "differential_report",
            "baseline_fingerprint",
        ] {
            assert!(diagnostics.iter().any(|value| {
                value.to_string()
                    == format!("RPE-MATURITY-0011 profile=m1.r0-strict.v1 field={field}")
            }));
        }
    }
}
