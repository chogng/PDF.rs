#![forbid(unsafe_code)]

//! Repository-local quality lane, case validation, purity, and synthetic bundle CLI.

mod bundle;
mod maturity;
mod purity;

pub(crate) use pdf_rs_quality::manifest;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use bundle::build_synthetic_failure_bundle;
use maturity::validate_maturity_file;
use pdf_rs_quality::case_contract::validate_case_file;
use purity::{check_product_build_closure, check_product_manifests, prepare_product_build_proof};

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
            checks: "fmt,clippy,test,parser-mutation-smoke,case-manifests,m3-raster-oracle-contract,m3-content-graphics-trace,m3-reference-geometry-trace,m3-reference-color-trace,m3-basic-image-trace,m3-basic-text-trace,m2-scene-gate,m2-exit,m1-maturity,product-purity,product-release-closure,synthetic-failure-bundle",
        }),
        "pr" => Some(Selection {
            lane: "pr",
            reason: "merge gate for the complete required Rust quality baseline",
            checks: "fmt,clippy,test,parser-mutation-smoke,case-manifests,m3-raster-oracle-contract,m3-content-graphics-trace,m3-reference-geometry-trace,m3-reference-color-trace,m3-basic-image-trace,m3-basic-text-trace,m2-scene-gate,m2-exit,m1-maturity,product-purity,product-release-closure,synthetic-failure-bundle,doc",
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
            match validate_case_file(Path::new(&path)) {
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
        "validate-m1-maturity" => {
            let Some(path) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            match validate_maturity_file(Path::new(&path)) {
                Ok(report) => {
                    println!("profiles={}", report.profiles);
                    println!("planned={}", report.planned);
                    println!("reference={}", report.reference);
                    println!("differential={}", report.differential);
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
        "check-product-purity" => {
            let root = arguments.next().unwrap_or_else(|| ".".into());
            if arguments.next().is_some() {
                return usage();
            }
            match check_product_manifests(Path::new(&root)) {
                Ok(report) => {
                    println!("scanned_cargo_manifests={}", report.scanned_cargo_manifests);
                    println!("allowlisted_product_packages={}", report.product_packages);
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
        "prepare-product-build-proof" => {
            let Some(root) = arguments.next() else {
                return usage();
            };
            let Some(target) = arguments.next() else {
                return usage();
            };
            let Some(proof_id) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            match prepare_product_build_proof(Path::new(&root), Path::new(&target), &proof_id) {
                Ok(preparation) => {
                    println!("proof_id={}", preparation.proof_id);
                    println!(
                        "allowlisted_product_packages={}",
                        preparation.product_packages
                    );
                    println!("target_was_absent=true");
                    println!("build_profile=release");
                    println!("scope=fresh-release-product-build-preparation");
                    ExitCode::SUCCESS
                }
                Err(violations) => {
                    print_build_closure_violations(violations);
                    ExitCode::FAILURE
                }
            }
        }
        "check-product-build-closure" => {
            let Some(root) = arguments.next() else {
                return usage();
            };
            let Some(target) = arguments.next() else {
                return usage();
            };
            let Some(proof_id) = arguments.next() else {
                return usage();
            };
            if arguments.next().is_some() {
                return usage();
            }
            match check_product_build_closure(Path::new(&root), Path::new(&target), &proof_id) {
                Ok(report) => {
                    println!("proof_id={proof_id}");
                    println!("release_product_packages={}", report.product_packages);
                    println!("release_depfiles={}", report.depfiles);
                    println!("release_artifact_files={}", report.artifact_files);
                    println!(
                        "release_fingerprint_directories={}",
                        report.fingerprint_directories
                    );
                    println!(
                        "release_build_script_artifacts={}",
                        report.build_script_artifacts
                    );
                    println!("release_native_artifacts={}", report.native_artifacts);
                    println!("release_unknown_artifacts={}", report.unknown_artifacts);
                    println!("scope=fresh-release-product-build-closure");
                    ExitCode::SUCCESS
                }
                Err(violations) => {
                    print_build_closure_violations(violations);
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
        "usage: pdf-rs-quality <local|pr|validate-case CASE.toml|validate-cases ROOT|validate-m1-maturity PROFILES.toml|check-product-purity [ROOT]|prepare-product-build-proof ROOT TARGET PROOF_ID|check-product-build-closure ROOT TARGET PROOF_ID|synthetic-bundle CASE.toml OUTPUT_DIR>\nlocal/pr checks include the M3 raster-oracle, Content graphics, Reference geometry, Reference color, basic Image, and basic Text contracts, M2 Scene profile replay, and M2 exit closure"
    );
    ExitCode::from(2)
}

fn print_build_closure_violations(violations: Vec<purity::BuildClosureViolation>) {
    for violation in violations {
        eprintln!(
            "{} path={} token={}",
            violation.code,
            violation.path.display(),
            violation.token
        );
    }
}

fn validate_case_tree(root: &Path) -> ExitCode {
    let mut paths = Vec::new();
    if let Err(path) = collect_case_manifests(root, &mut paths) {
        eprintln!("RPE-CASE-0014 path={}", path.display());
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
        match validate_case_file(path) {
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

fn collect_case_manifests(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), PathBuf> {
    let entries = fs::read_dir(root).map_err(|_| root.to_path_buf())?;
    for entry in entries {
        let entry = entry.map_err(|_| root.to_path_buf())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|_| path.clone())?;
        if file_type.is_symlink() {
            return Err(path);
        }
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
