#![forbid(unsafe_code)]

//! Repository-local quality lane, case validation, purity, and synthetic bundle CLI.

mod bundle;
mod manifest;
mod purity;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use bundle::build_synthetic_failure_bundle;
use manifest::validate_manifest_file;
use purity::check_product_manifests;

struct Selection {
    lane: &'static str,
    reason: &'static str,
    checks: &'static str,
}

fn selection_for(lane: &str) -> Option<Selection> {
    match lane {
        "local" => Some(Selection {
            lane: "local",
            reason: "pre-submit feedback for code and deterministic T0 infrastructure",
            checks: "fmt,clippy,test,parser-mutation-smoke,case-manifests,product-purity,synthetic-failure-bundle",
        }),
        "pr" => Some(Selection {
            lane: "pr",
            reason: "merge gate for the complete required Rust quality baseline",
            checks: "fmt,clippy,test,parser-mutation-smoke,case-manifests,product-purity,synthetic-failure-bundle,doc",
        }),
        _ => None,
    }
}

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        return usage();
    };

    if let Some(selection) = selection_for(&command) {
        if arguments.next().is_some() {
            return usage();
        }
        print_selection(selection);
        return ExitCode::SUCCESS;
    }

    match command.as_str() {
        "validate-case" => {
            let Some(path) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            match validate_manifest_file(Path::new(&path)) {
                Ok(manifest) => {
                    println!("case_id={}", manifest.case_id());
                    println!("source_hash={}", manifest.source_sha256());
                    ExitCode::SUCCESS
                }
                Err(diagnostics) => {
                    for diagnostic in diagnostics {
                        eprintln!("{diagnostic}");
                    }
                    ExitCode::FAILURE
                }
            }
        }
        "validate-cases" => {
            let Some(root) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            validate_case_tree(Path::new(&root))
        }
        "check-product-purity" => {
            let root = arguments.next().unwrap_or_else(|| ".".into());
            if arguments.next().is_some() {
                return usage();
            }
            match check_product_manifests(Path::new(&root)) {
                Ok(count) => {
                    println!("scanned_cargo_manifests={count}");
                    println!("forbidden_manifest_tokens=0");
                    println!("scope=direct-manifest-preflight-not-resolved-release-proof");
                    ExitCode::SUCCESS
                }
                Err(violations) => {
                    for violation in violations {
                        eprintln!(
                            "{} manifest={} token={}",
                            violation.code,
                            violation.manifest.display(),
                            violation.token
                        );
                    }
                    ExitCode::FAILURE
                }
            }
        }
        "synthetic-bundle" => {
            let Some(manifest) = arguments.next() else {
                return usage();
            };
            let Some(output_root) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            match build_synthetic_failure_bundle(Path::new(&manifest), Path::new(&output_root)) {
                Ok(path) => {
                    println!("bundle={}", path.display());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("{error}");
                    for diagnostic in error.manifest_diagnostics() {
                        eprintln!("{diagnostic}");
                    }
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}

fn print_selection(selection: Selection) {
    println!("lane={}", selection.lane);
    println!("selection_reason={}", selection.reason);
    println!("checks={}", selection.checks);
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: pdf-rs-quality <local|pr|validate-case CASE.toml|validate-cases ROOT|check-product-purity [ROOT]|synthetic-bundle CASE.toml OUTPUT_DIR>"
    );
    ExitCode::from(2)
}

fn validate_case_tree(root: &Path) -> ExitCode {
    let mut paths = Vec::new();
    if collect_case_manifests(root, &mut paths).is_err() {
        eprintln!("RPE-MANIFEST-0001");
        return ExitCode::FAILURE;
    }
    paths.sort();
    if paths.is_empty() {
        eprintln!("RPE-MANIFEST-0018 no-case-manifests-selected");
        return ExitCode::FAILURE;
    }

    let mut failed = false;
    let mut identities: BTreeMap<String, PathBuf> = BTreeMap::new();
    for path in &paths {
        match validate_manifest_file(path) {
            Ok(manifest) => {
                let case_id = manifest.case_id();
                let relative_directory = path
                    .parent()
                    .and_then(|parent| parent.strip_prefix(root).ok())
                    .map(|path| {
                        path.components()
                            .map(|component| component.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/")
                    });
                if relative_directory.as_deref() != Some(case_id) {
                    failed = true;
                    eprintln!(
                        "RPE-MANIFEST-0020 case_id={} case_manifest={}",
                        case_id,
                        path.display()
                    );
                    continue;
                }
                if let Some(first) = identities.insert(case_id.into(), path.clone()) {
                    failed = true;
                    eprintln!(
                        "RPE-MANIFEST-0019 case_id={} first={} duplicate={}",
                        case_id,
                        first.display(),
                        path.display()
                    );
                    continue;
                }
                println!("case_id={case_id}");
            }
            Err(diagnostics) => {
                failed = true;
                eprintln!("case_manifest={}", path.display());
                for diagnostic in diagnostics {
                    eprintln!("{diagnostic}");
                }
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        println!("validated_cases={}", paths.len());
        ExitCode::SUCCESS
    }
}

fn collect_case_manifests(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_case_manifests(&path, output)?;
        } else if file_type.is_file() && entry.file_name() == "case.toml" {
            output.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::selection_for;

    #[test]
    fn recognizes_supported_lanes() {
        assert_eq!(
            selection_for("local").map(|selection| selection.lane),
            Some("local")
        );
        assert_eq!(
            selection_for("pr").map(|selection| selection.lane),
            Some("pr")
        );
    }

    #[test]
    fn rejects_unknown_lanes() {
        assert!(selection_for("nightly").is_none());
    }
}
