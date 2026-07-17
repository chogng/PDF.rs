//! Explicit, dependency-free generator for the canonical Engine protocol.
//!
//! The binary exposes two fail-closed modes: `generate <repository-root>` writes the fixed
//! generated-file allowlist, while `--check <repository-root>` compares those files without
//! modifying the checkout.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod generate;
mod hash;
mod model;
mod parser;

use std::path::{Path, PathBuf};

use generate::{GeneratedFile, check_generated, generated_files, write_generated};
use parser::parse_schema;

fn load_generated(root: &Path) -> Result<Vec<GeneratedFile>, String> {
    let schema_relative = Path::new("protocol/engine.protocol");
    generate::reject_symlink_path(root, schema_relative)?;
    let schema_path = root.join(schema_relative);
    let schema_bytes = std::fs::metadata(&schema_path)
        .map_err(|error| format!("inspect {}: {error}", schema_path.display()))?
        .len();
    if schema_bytes > parser::MAX_SCHEMA_BYTES as u64 {
        return Err(format!(
            "schema exceeds {} byte limit: {}",
            parser::MAX_SCHEMA_BYTES,
            schema_path.display()
        ));
    }
    let schema = std::fs::read_to_string(&schema_path)
        .map_err(|error| format!("read {}: {error}", schema_path.display()))?;
    let protocol = parse_schema(&schema).map_err(|error| error.to_string())?;
    Ok(generated_files(&protocol, &schema))
}

/// Regenerates every protocol binding and registry under `root`.
///
/// Output paths are a fixed internal allowlist. Symlinked path components and non-canonical
/// schemas are rejected before a generated target is replaced.
pub fn generate_repository(root: &Path) -> Result<(), String> {
    let files = load_generated(root)?;
    write_generated(root, &files)
}

/// Checks every generated protocol file under `root` without writing to the filesystem.
///
/// The error lists any missing, stale, or symlinked generated targets.
pub fn check_repository(root: &Path) -> Result<(), String> {
    let files = load_generated(root)?;
    check_generated(root, &files).map_err(|paths| {
        let joined = paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("generated protocol files are missing or stale: {joined}")
    })
}

/// Parses the strict command line accepted by the generator binary.
///
/// Only `generate <repository-root>` and `--check <repository-root>` are accepted. The returned
/// boolean is `true` for check mode.
pub fn parse_cli(arguments: impl IntoIterator<Item = String>) -> Result<(bool, PathBuf), String> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .ok_or_else(|| "missing command and repository root".to_owned())?;
    let root = arguments
        .next()
        .ok_or_else(|| format!("missing repository root for {command}"))?;
    let check = match command.as_str() {
        "--check" => true,
        "generate" => false,
        _ => return Err(format!("unknown command: {command}")),
    };
    if let Some(extra) = arguments.next() {
        return Err(format!("unexpected argument: {extra}"));
    }
    Ok((check, PathBuf::from(root)))
}

#[cfg(test)]
mod tests {
    use super::parse_cli;
    use std::path::PathBuf;

    #[test]
    fn cli_only_supports_explicit_generate_and_check() {
        assert_eq!(
            parse_cli(["generate".into(), ".".into()]).unwrap(),
            (false, PathBuf::from("."))
        );
        assert_eq!(
            parse_cli(["--check".into(), ".".into()]).unwrap(),
            (true, PathBuf::from("."))
        );
        assert!(parse_cli([]).is_err());
        assert!(parse_cli([".".into()]).is_err());
        assert!(parse_cli(["generate".into()]).is_err());
        assert!(parse_cli(["--check".into()]).is_err());
        assert!(parse_cli(["generate".into(), ".".into(), "extra".into()]).is_err());
        assert!(parse_cli(["--unknown".into()]).is_err());
    }
}
