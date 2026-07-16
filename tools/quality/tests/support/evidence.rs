#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pdf_rs_digest::{hex_digest, sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
enum RootValue {
    String(String),
    Bool(bool),
    Unsigned(u64),
    Bare(String),
}

#[derive(Debug, Default)]
pub struct RootToml {
    values: BTreeMap<String, RootValue>,
    arrays: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TableHeader<'a> {
    Standard(&'a str),
    Array(&'a str),
}

impl RootToml {
    pub fn parse(document: &str) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut lines = document.lines().enumerate();
        let mut at_root = true;
        while let Some((index, line)) = lines.next() {
            let structural = structural_toml_line(line, index + 1)?;
            let trimmed = structural.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if parse_table_header(trimmed, index + 1)?.is_some() {
                at_root = false;
                continue;
            }
            if !at_root {
                continue;
            }

            let (key, raw_value) = trimmed
                .split_once('=')
                .ok_or_else(|| format!("line {} is not a root key/value", index + 1))?;
            let key = key.trim();
            validate_key(key, index + 1)?;
            if parsed.values.contains_key(key) || parsed.arrays.contains_key(key) {
                return Err(format!("duplicate root key {key}"));
            }

            let raw_value = raw_value.trim();
            if raw_value == "[" {
                let values = parse_multiline_array(&mut lines, key)?;
                parsed.arrays.insert(key.to_owned(), values);
            } else if raw_value.starts_with('[') {
                let values = parse_inline_array(raw_value)
                    .map_err(|error| format!("line {} root array {key}: {error}", index + 1))?;
                parsed.arrays.insert(key.to_owned(), values);
            } else {
                let value = parse_scalar(raw_value)
                    .map_err(|error| format!("line {} root scalar {key}: {error}", index + 1))?;
                parsed.values.insert(key.to_owned(), value);
            }
        }
        Ok(parsed)
    }

    pub fn string(&self, key: &str) -> Result<&str, String> {
        match self.values.get(key) {
            Some(RootValue::String(value)) => Ok(value),
            Some(_) => Err(format!("root value {key} is not a basic string")),
            None => Err(format!("missing root basic string {key}")),
        }
    }

    pub fn optional_string(&self, key: &str) -> Result<Option<&str>, String> {
        match self.values.get(key) {
            Some(RootValue::String(value)) => Ok(Some(value)),
            Some(_) => Err(format!("root value {key} is not a basic string")),
            None => {
                if self.arrays.contains_key(key) {
                    Err(format!("root value {key} is not a basic string"))
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub fn boolean(&self, key: &str) -> Result<bool, String> {
        match self.values.get(key) {
            Some(RootValue::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("root value {key} is not a boolean")),
            None => Err(format!("missing root boolean {key}")),
        }
    }

    pub fn unsigned(&self, key: &str) -> Result<u64, String> {
        match self.values.get(key) {
            Some(RootValue::Unsigned(value)) => Ok(*value),
            Some(_) => Err(format!("root value {key} is not a canonical u64")),
            None => Err(format!("missing root canonical u64 {key}")),
        }
    }

    pub fn bare(&self, key: &str) -> Result<&str, String> {
        match self.values.get(key) {
            Some(RootValue::Bare(value)) => Ok(value),
            Some(_) => Err(format!("root value {key} is not a bare scalar")),
            None => Err(format!("missing root bare scalar {key}")),
        }
    }

    pub fn array(&self, key: &str) -> Result<&[String], String> {
        self.arrays
            .get(key)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("missing root basic-string array {key}"))
    }

    pub fn expect_string(&self, key: &str, expected: &str) -> Result<(), String> {
        expect_equal(key, self.string(key)?, expected)
    }

    pub fn expect_bool(&self, key: &str, expected: bool) -> Result<(), String> {
        expect_equal(key, self.boolean(key)?, expected)
    }

    pub fn expect_unsigned(&self, key: &str, expected: u64) -> Result<(), String> {
        expect_equal(key, self.unsigned(key)?, expected)
    }

    pub fn expect_bare(&self, key: &str, expected: &str) -> Result<(), String> {
        expect_equal(key, self.bare(key)?, expected)
    }

    pub fn expect_array(&self, key: &str, expected: &[&str]) -> Result<(), String> {
        let actual = self.array(key)?;
        if actual
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
        {
            Ok(())
        } else {
            Err(format!(
                "root array {key} differs: expected {expected:?}, got {actual:?}"
            ))
        }
    }
}

pub fn array_table_records(document: &str, table: &str) -> Result<Vec<RootToml>, String> {
    validate_key(table, 0)?;
    let mut records = Vec::new();
    let mut current = None::<Vec<String>>;
    let mut array_depth = 0_usize;
    let mut inline_table_depth = 0_usize;

    for (index, line) in document.lines().enumerate() {
        let line_number = index + 1;
        let structural = structural_toml_line(line, line_number)?;
        let trimmed = structural.trim();
        if array_depth == 0
            && inline_table_depth == 0
            && let Some(header) = parse_table_header(trimmed, line_number)?
        {
            if let Some(body) = current.take() {
                records.push(parse_table_body(table, body)?);
            }
            current = match header {
                TableHeader::Array(name) if name == table => Some(Vec::new()),
                TableHeader::Array(_) | TableHeader::Standard(_) => None,
            };
            continue;
        }

        update_value_depths(
            structural,
            line_number,
            &mut array_depth,
            &mut inline_table_depth,
        )?;
        if let Some(body) = &mut current {
            body.push(line.to_owned());
        }
    }

    if array_depth != 0 || inline_table_depth != 0 {
        return Err(format!(
            "TOML document ends inside a value: array depth {array_depth}, inline-table depth {inline_table_depth}"
        ));
    }
    if let Some(body) = current {
        records.push(parse_table_body(table, body)?);
    }
    Ok(records)
}

fn parse_table_body(table: &str, body: Vec<String>) -> Result<RootToml, String> {
    RootToml::parse(&body.join("\n"))
        .map_err(|error| format!("cannot parse [[{table}]] record: {error}"))
}

fn structural_toml_line(line: &str, line_number: usize) -> Result<&str, String> {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut basic = false;
    let mut literal = false;
    let mut escaped = false;
    while index < bytes.len() {
        if !basic
            && !literal
            && index + 2 < bytes.len()
            && ((bytes[index] == b'"' && bytes[index + 1] == b'"' && bytes[index + 2] == b'"')
                || (bytes[index] == b'\''
                    && bytes[index + 1] == b'\''
                    && bytes[index + 2] == b'\''))
        {
            return Err(format!(
                "line {line_number} uses a multiline string outside the strict trace TOML subset"
            ));
        }
        let byte = bytes[index];
        if basic {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                basic = false;
            }
        } else if literal {
            if byte == b'\'' {
                literal = false;
            }
        } else {
            match byte {
                b'#' => return Ok(&line[..index]),
                b'"' => basic = true,
                b'\'' => literal = true,
                _ => {}
            }
        }
        index += 1;
    }
    if basic || literal || escaped {
        return Err(format!(
            "line {line_number} has an unclosed single-line string"
        ));
    }
    Ok(line)
}

fn parse_table_header(line: &str, line_number: usize) -> Result<Option<TableHeader<'_>>, String> {
    if !line.starts_with('[') {
        return Ok(None);
    }
    let (name, header) = if let Some(inner) = line
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        (inner, TableHeader::Array(inner))
    } else if let Some(inner) = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        (inner, TableHeader::Standard(inner))
    } else {
        return Err(format!("line {line_number} has a malformed table header"));
    };
    validate_key(name, line_number)?;
    Ok(Some(header))
}

fn update_value_depths(
    line: &str,
    line_number: usize,
    array_depth: &mut usize,
    inline_table_depth: &mut usize,
) -> Result<(), String> {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut basic = false;
    let mut literal = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if basic {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                basic = false;
            }
        } else if literal {
            if byte == b'\'' {
                literal = false;
            }
        } else {
            match byte {
                b'#' => break,
                b'"' => basic = true,
                b'\'' => literal = true,
                b'[' => {
                    *array_depth = array_depth
                        .checked_add(1)
                        .ok_or_else(|| format!("line {line_number} array depth overflows"))?;
                }
                b']' => {
                    *array_depth = array_depth
                        .checked_sub(1)
                        .ok_or_else(|| format!("line {line_number} closes no open array"))?;
                }
                b'{' => {
                    *inline_table_depth = inline_table_depth.checked_add(1).ok_or_else(|| {
                        format!("line {line_number} inline-table depth overflows")
                    })?;
                }
                b'}' => {
                    *inline_table_depth = inline_table_depth
                        .checked_sub(1)
                        .ok_or_else(|| format!("line {line_number} closes no open inline table"))?;
                }
                _ => {}
            }
        }
        index += 1;
    }
    Ok(())
}

fn expect_equal<T>(key: &str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "root value {key} differs: expected {expected:?}, got {actual:?}"
        ))
    }
}

fn validate_key(key: &str, line: usize) -> Result<(), String> {
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Err(format!("line {line} has an invalid root key"))
    } else {
        Ok(())
    }
}

fn parse_scalar(value: &str) -> Result<RootValue, String> {
    if value.starts_with('"') {
        return parse_basic_string(value).map(RootValue::String);
    }
    if value == "true" {
        return Ok(RootValue::Bool(true));
    }
    if value == "false" {
        return Ok(RootValue::Bool(false));
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
            return Err(format!("non-canonical unsigned integer {value}"));
        }
        return value
            .parse::<u64>()
            .map(RootValue::Unsigned)
            .map_err(|error| format!("unsigned integer {value} is out of range: {error}"));
    }
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.chars().any(|character| {
            matches!(
                character,
                '"' | '\'' | '\\' | '[' | ']' | '{' | '}' | '#' | ','
            )
        })
    {
        return Err(format!("invalid bare scalar {value:?}"));
    }
    Ok(RootValue::Bare(value.to_owned()))
}

fn parse_basic_string(value: &str) -> Result<String, String> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| "expected one basic quoted string".to_owned())?;
    if inner
        .chars()
        .any(|character| matches!(character, '"' | '\\' | '\n' | '\r'))
    {
        return Err("escaped, quoted, or multiline basic strings are not accepted".to_owned());
    }
    Ok(inner.to_owned())
}

fn parse_multiline_array<'a>(
    lines: &mut impl Iterator<Item = (usize, &'a str)>,
    key: &str,
) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    loop {
        let (index, line) = lines
            .next()
            .ok_or_else(|| format!("root array {key} is not closed"))?;
        let entry = line.trim();
        if entry == "]" {
            return Ok(values);
        }
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let value = entry.strip_suffix(',').ok_or_else(|| {
            format!(
                "line {} in root array {key} lacks a trailing comma",
                index + 1
            )
        })?;
        values.push(
            parse_basic_string(value.trim())
                .map_err(|error| format!("line {} in root array {key}: {error}", index + 1))?,
        );
    }
}

fn parse_inline_array(value: &str) -> Result<Vec<String>, String> {
    if !value.ends_with(']') {
        return Err("inline array is not closed on the same line".to_owned());
    }
    let mut values = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 1;
    loop {
        skip_ascii_whitespace(bytes, &mut index);
        if index >= bytes.len() {
            return Err("inline array is not closed".to_owned());
        }
        if bytes[index] == b']' {
            index += 1;
            skip_ascii_whitespace(bytes, &mut index);
            return if index == bytes.len() {
                Ok(values)
            } else {
                Err("inline array has trailing content".to_owned())
            };
        }
        if bytes[index] != b'"' {
            return Err("inline arrays accept only basic strings".to_owned());
        }
        let start = index;
        index += 1;
        while index < bytes.len() && bytes[index] != b'"' {
            if matches!(bytes[index], b'\\' | b'\n' | b'\r') {
                return Err("inline strings cannot contain escapes or newlines".to_owned());
            }
            index += 1;
        }
        if index >= bytes.len() {
            return Err("inline string is not closed".to_owned());
        }
        index += 1;
        values.push(parse_basic_string(&value[start..index])?);
        skip_ascii_whitespace(bytes, &mut index);
        match bytes.get(index) {
            Some(b',') => {
                index += 1;
                skip_ascii_whitespace(bytes, &mut index);
                if bytes.get(index) == Some(&b']') {
                    index += 1;
                    skip_ascii_whitespace(bytes, &mut index);
                    return if index == bytes.len() {
                        Ok(values)
                    } else {
                        Err("inline array has trailing content".to_owned())
                    };
                }
            }
            Some(b']') => {
                index += 1;
                skip_ascii_whitespace(bytes, &mut index);
                return if index == bytes.len() {
                    Ok(values)
                } else {
                    Err("inline array has trailing content".to_owned())
                };
            }
            Some(_) => return Err("inline array strings must be comma-separated".to_owned()),
            None => return Err("inline array is not closed".to_owned()),
        }
    }
}

fn skip_ascii_whitespace(bytes: &[u8], index: &mut usize) {
    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
        *index += 1;
    }
}

pub fn verify_reviewed_subjects(
    root: &Path,
    evidence: &RootToml,
    expected_commit: &str,
    expected_tree: Option<&str>,
) -> Result<usize, String> {
    validate_commit_id(expected_commit)?;
    evidence.expect_string("implementation_commit", expected_commit)?;
    evidence.expect_string("reviewed_subject_commit", expected_commit)?;

    let declared_tree = evidence.optional_string("reviewed_subject_tree")?;
    match (declared_tree, expected_tree) {
        (Some(actual), Some(expected)) if actual != expected => {
            return Err(format!(
                "reviewed_subject_tree differs: expected {expected}, got {actual}"
            ));
        }
        (None, Some(expected)) => {
            return Err(format!("missing required reviewed_subject_tree {expected}"));
        }
        _ => {}
    }

    verify_commit(root, expected_commit)?;
    let actual_tree = commit_tree(root, expected_commit)?;
    if let Some(declared) = declared_tree {
        validate_object_id(declared, "tree")?;
        if declared != actual_tree {
            return Err(format!(
                "reviewed subject tree does not match {expected_commit}^{{tree}}: \
                 expected {declared}, got {actual_tree}"
            ));
        }
    }
    if let Some(expected) = expected_tree {
        validate_object_id(expected, "tree")?;
        if expected != actual_tree {
            return Err(format!(
                "expected reviewed tree does not match {expected_commit}^{{tree}}: \
                 expected {expected}, got {actual_tree}"
            ));
        }
    }

    let mut commit_map = BTreeMap::new();
    for locator in evidence.array("reviewed_subject_commit_map")? {
        let (path, commit) = split_pinned_locator(locator)?;
        validate_relative_path(path)?;
        validate_commit_id(commit)?;
        if commit != expected_commit {
            return Err(format!(
                "commit map entry {path} is pinned to {commit}, not {expected_commit}"
            ));
        }
        if commit_map
            .insert(path.to_owned(), commit.to_owned())
            .is_some()
        {
            return Err(format!("duplicate commit-map path {path}"));
        }
    }
    if commit_map.is_empty() {
        return Err("reviewed_subject_commit_map must not be empty".to_owned());
    }

    let subjects = evidence.array("reviewed_subjects")?;
    let mut seen_paths = BTreeSet::new();
    let mut matched_map_paths = BTreeSet::new();
    let mut bound_subjects = Vec::with_capacity(subjects.len());
    for entry in subjects {
        let (locator, expected_hash) = split_subject_entry(entry)?;
        validate_sha256(expected_hash)?;
        let (path, explicit_commit) = split_optional_pinned_locator(locator)?;
        validate_relative_path(path)?;
        if !seen_paths.insert(path.to_owned()) {
            return Err(format!("duplicate reviewed-subject path {path}"));
        }

        let mapped_commit = commit_map.get(path).map(String::as_str);
        let effective_commit = match (explicit_commit, mapped_commit) {
            (Some(explicit), Some(mapped)) if explicit == mapped => Some(explicit),
            (Some(_), Some(_)) => {
                return Err(format!("reviewed subject {path} disagrees with commit map"));
            }
            (Some(_), None) => {
                return Err(format!(
                    "pinned reviewed subject {path} is absent from commit map"
                ));
            }
            (None, Some(_)) => {
                return Err(format!(
                    "commit-mapped reviewed subject {path} must carry an explicit @commit locator"
                ));
            }
            (None, None) => None,
        };
        if mapped_commit.is_some() {
            matched_map_paths.insert(path.to_owned());
        }
        bound_subjects.push((locator, path, effective_commit, expected_hash));
    }
    if matched_map_paths.len() != commit_map.len() {
        return Err("every commit-map entry must bind exactly one reviewed subject".to_owned());
    }

    let mut verified = 0;
    for (locator, path, commit, expected_hash) in bound_subjects {
        let actual_hash = digest_subject(root, path, commit)?;
        if actual_hash != expected_hash {
            return Err(format!(
                "evidence hash mismatch for {locator}: expected {expected_hash}, got {actual_hash}"
            ));
        }
        verified += 1;
    }
    Ok(verified)
}

pub fn verify_subject_entries(root: &Path, entries: &[String]) -> Result<usize, String> {
    let mut verified = 0;
    let mut seen_locators = BTreeSet::new();
    for entry in entries {
        let (locator, expected_hash) = split_subject_entry(entry)?;
        if !seen_locators.insert(locator.to_owned()) {
            return Err(format!("duplicate evidence locator {locator}"));
        }
        validate_sha256(expected_hash)?;
        let (path, commit) = split_optional_pinned_locator(locator)?;
        let actual_hash = digest_subject(root, path, commit)?;
        if actual_hash != expected_hash {
            return Err(format!(
                "evidence hash mismatch for {locator}: expected {expected_hash}, got {actual_hash}"
            ));
        }
        verified += 1;
    }
    Ok(verified)
}

pub fn split_subject_entry(entry: &str) -> Result<(&str, &str), String> {
    let (locator, hash) = entry
        .split_once("#sha256:")
        .ok_or_else(|| format!("evidence subject lacks SHA-256: {entry}"))?;
    if locator.is_empty() || hash.is_empty() || hash.contains("#sha256:") {
        return Err(format!("invalid evidence subject: {entry}"));
    }
    Ok((locator, hash))
}

fn split_pinned_locator(locator: &str) -> Result<(&str, &str), String> {
    let (path, commit) = locator
        .rsplit_once('@')
        .ok_or_else(|| format!("commit-map locator is not pinned: {locator}"))?;
    if path.contains('@') {
        return Err(format!("ambiguous pinned locator: {locator}"));
    }
    Ok((path, commit))
}

fn split_optional_pinned_locator(locator: &str) -> Result<(&str, Option<&str>), String> {
    if locator.contains('@') {
        let (path, commit) = split_pinned_locator(locator)?;
        validate_commit_id(commit)?;
        Ok((path, Some(commit)))
    } else {
        Ok((locator, None))
    }
}

pub fn validate_sha256(hash: &str) -> Result<(), String> {
    validate_hex_id(hash, 64, "SHA-256")
}

pub fn validate_commit_id(commit: &str) -> Result<(), String> {
    validate_hex_id(commit, 40, "commit")
}

fn validate_object_id(object: &str, kind: &str) -> Result<(), String> {
    validate_hex_id(object, 40, kind)
}

fn validate_hex_id(value: &str, length: usize, kind: &str) -> Result<(), String> {
    if value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(format!("non-canonical {kind} id {value}"))
    }
}

pub fn validate_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty()
        || path.contains('\\')
        || path.starts_with('/')
        || path.ends_with('/')
        || path.split('/').any(|component| {
            component.is_empty()
                || matches!(component, "." | "..")
                || component.contains(':')
                || component.contains('@')
        })
    {
        return Err(format!("non-canonical evidence path {path}"));
    }
    let relative = PathBuf::from(path);
    if relative.is_absolute()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("non-normal evidence path {path}"));
    }
    Ok(relative)
}

fn digest_subject(root: &Path, path: &str, commit: Option<&str>) -> Result<String, String> {
    validate_relative_path(path)?;
    let bytes = match commit {
        Some(commit) => read_commit_blob(root, path, commit)?,
        None => read_repository_file(root, path)?,
    };
    let digest = sha256(&bytes).map_err(|error| format!("cannot hash {path}: {error}"))?;
    Ok(hex_digest(&digest))
}

pub fn read_repository_file(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    let relative = validate_relative_path(path)?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("cannot canonicalize root: {error}"))?;
    let mut current = canonical_root.clone();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            return Err(format!("non-normal evidence path {path}"));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("cannot inspect {}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "evidence path crosses a symlink: {}",
                current.display()
            ));
        }
        if index + 1 == components.len() {
            if !metadata.is_file() {
                return Err(format!("evidence subject is not a regular file: {path}"));
            }
        } else if !metadata.is_dir() {
            return Err(format!(
                "evidence path component is not a directory: {path}"
            ));
        }
    }
    let canonical = fs::canonicalize(&current)
        .map_err(|error| format!("cannot canonicalize evidence subject {path}: {error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("evidence subject escapes repository root: {path}"));
    }
    fs::read(&canonical).map_err(|error| format!("cannot read evidence subject {path}: {error}"))
}

pub fn read_commit_blob(root: &Path, path: &str, commit: &str) -> Result<Vec<u8>, String> {
    validate_relative_path(path)?;
    verify_commit(root, commit)?;

    let tree = git_output(root, &["ls-tree", "-z", "--full-tree", commit, "--", path])?;
    let records = tree
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(format!(
            "pinned subject is not one exact regular blob: {commit}:{path}"
        ));
    }
    let record = records[0];
    let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
        return Err(format!("unexpected ls-tree framing for {commit}:{path}"));
    };
    let metadata = std::str::from_utf8(&record[..tab])
        .map_err(|_| format!("non-UTF-8 ls-tree metadata for {commit}:{path}"))?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3
        || !matches!(fields[0], "100644" | "100755")
        || fields[1] != "blob"
        || &record[tab + 1..] != path.as_bytes()
    {
        return Err(format!(
            "pinned subject is not one exact regular blob: {commit}:{path}"
        ));
    }
    let object = format!("{commit}:{path}");
    git_output(root, &["cat-file", "blob", &object])
}

fn verify_commit(root: &Path, commit: &str) -> Result<(), String> {
    validate_commit_id(commit)?;
    let commit_object = format!("{commit}^{{commit}}");
    let verified = git_output(root, &["rev-parse", "--verify", "--quiet", &commit_object])?;
    if String::from_utf8_lossy(&verified).trim() != commit {
        return Err(format!("commit id did not resolve canonically: {commit}"));
    }
    let ancestor = Command::new("git")
        .args(["merge-base", "--is-ancestor", commit, "HEAD"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("cannot verify commit ancestry for {commit}: {error}"))?;
    if !ancestor.success() {
        return Err(format!(
            "evidence commit is not an ancestor of HEAD: {commit}"
        ));
    }
    Ok(())
}

fn commit_tree(root: &Path, commit: &str) -> Result<String, String> {
    let tree_object = format!("{commit}^{{tree}}");
    let output = git_output(root, &["rev-parse", "--verify", "--quiet", &tree_object])?;
    let tree = String::from_utf8(output)
        .map_err(|_| format!("tree id for {commit} is not UTF-8"))?
        .trim()
        .to_owned();
    validate_object_id(&tree, "tree")?;
    Ok(tree)
}

pub fn git_output(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run git {}: {error}", arguments.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

pub fn git_revision(root: &Path, revision: &str) -> String {
    let output =
        git_output(root, &["rev-parse", "--verify", revision]).expect("resolve Git revision");
    String::from_utf8(output)
        .expect("Git revision is UTF-8")
        .trim()
        .to_owned()
}

pub struct TestDirectory(PathBuf);

impl TestDirectory {
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path).expect("remove stale test directory");
        }
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
