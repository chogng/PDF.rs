use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PRODUCT_ROOTS: &[&str] = &["core", "runtime", "platform"];
const FORBIDDEN_ENGINES: &[&str] = &[
    "pdfium", "pdf.js", "pdfjs", "mupdf", "poppler", "hayro", "vello",
];
const PROOF_MARKER: &str = ".pdf-rs-product-build-proof";
const PROOF_SCHEMA: &str = "1";
const MAX_PROOF_AGE: Duration = Duration::from_secs(60 * 60);
const CARGO_HASH_LENGTH: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProductPackage {
    manifest: &'static str,
    package_name: &'static str,
    crate_name: &'static str,
}

const PRODUCT_PACKAGE_COUNT: usize = 14;
const PRODUCT_PACKAGES: &[ProductPackage; PRODUCT_PACKAGE_COUNT] = &[
    ProductPackage {
        manifest: "core/bytes/Cargo.toml",
        package_name: "pdf-rs-bytes",
        crate_name: "pdf_rs_bytes",
    },
    ProductPackage {
        manifest: "core/content/Cargo.toml",
        package_name: "pdf-rs-content",
        crate_name: "pdf_rs_content",
    },
    ProductPackage {
        manifest: "core/document/Cargo.toml",
        package_name: "pdf-rs-document",
        crate_name: "pdf_rs_document",
    },
    ProductPackage {
        manifest: "core/filters/Cargo.toml",
        package_name: "pdf-rs-filters",
        crate_name: "pdf_rs_filters",
    },
    ProductPackage {
        manifest: "core/font/Cargo.toml",
        package_name: "pdf-rs-font",
        crate_name: "pdf_rs_font",
    },
    ProductPackage {
        manifest: "core/object/Cargo.toml",
        package_name: "pdf-rs-object",
        crate_name: "pdf_rs_object",
    },
    ProductPackage {
        manifest: "core/raster/Cargo.toml",
        package_name: "pdf-rs-raster",
        crate_name: "pdf_rs_raster",
    },
    ProductPackage {
        manifest: "core/scene/Cargo.toml",
        package_name: "pdf-rs-scene",
        crate_name: "pdf_rs_scene",
    },
    ProductPackage {
        manifest: "core/syntax/Cargo.toml",
        package_name: "pdf-rs-syntax",
        crate_name: "pdf_rs_syntax",
    },
    ProductPackage {
        manifest: "core/xref/Cargo.toml",
        package_name: "pdf-rs-xref",
        crate_name: "pdf_rs_xref",
    },
    ProductPackage {
        manifest: "runtime/cache/Cargo.toml",
        package_name: "pdf-rs-cache",
        crate_name: "pdf_rs_cache",
    },
    ProductPackage {
        manifest: "runtime/policy/Cargo.toml",
        package_name: "pdf-rs-policy",
        crate_name: "pdf_rs_policy",
    },
    ProductPackage {
        manifest: "runtime/protocol/Cargo.toml",
        package_name: "pdf-rs-protocol",
        crate_name: "pdf_rs_protocol",
    },
    ProductPackage {
        manifest: "runtime/session/Cargo.toml",
        package_name: "pdf-rs-session",
        crate_name: "pdf_rs_session",
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurityViolation {
    pub code: &'static str,
    pub manifest: PathBuf,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductManifestReport {
    pub scanned_cargo_manifests: usize,
    pub product_packages: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildClosureViolation {
    pub code: &'static str,
    pub path: PathBuf,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductBuildPreparation {
    pub product_packages: usize,
    pub proof_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductBuildClosureReport {
    pub product_packages: usize,
    pub depfiles: usize,
    pub artifact_files: usize,
    pub fingerprint_directories: usize,
    pub build_script_artifacts: usize,
    pub native_artifacts: usize,
    pub unknown_artifacts: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProofMarker {
    proof_id: String,
    repository: PathBuf,
    started_unix_nanos: u128,
}

pub fn check_product_manifests(
    repository: &Path,
) -> Result<ProductManifestReport, Vec<PurityViolation>> {
    let mut manifests = Vec::new();
    let workspace_manifest = repository.join("Cargo.toml");
    if workspace_manifest.is_file() {
        manifests.push(workspace_manifest);
    }
    let mut product_manifests = Vec::new();
    for root in PRODUCT_ROOTS {
        let path = repository.join(root);
        if path.is_dir() && collect_manifests(&path, &mut product_manifests).is_err() {
            return Err(vec![PurityViolation {
                code: "RPE-PURITY-0001",
                manifest: path,
                token: "unreadable-product-tree".into(),
            }]);
        }
    }
    product_manifests.sort();
    manifests.extend(product_manifests.iter().cloned());
    manifests.sort();

    let mut violations = validate_product_package_policy(repository, &product_manifests);
    for manifest in &manifests {
        let input = match fs::read_to_string(manifest) {
            Ok(input) => input,
            Err(_) => {
                violations.push(PurityViolation {
                    code: "RPE-PURITY-0001",
                    manifest: manifest.clone(),
                    token: "unreadable-manifest".into(),
                });
                continue;
            }
        };
        for line in input.lines() {
            let line = line
                .split('#')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            for forbidden in FORBIDDEN_ENGINES {
                if line.contains(forbidden) {
                    violations.push(PurityViolation {
                        code: "RPE-PURITY-0002",
                        manifest: manifest.clone(),
                        token: (*forbidden).into(),
                    });
                }
            }
            let normalized = line.replace('\\', "/");
            if normalized.contains("path") && normalized.contains("tools/") {
                violations.push(PurityViolation {
                    code: "RPE-PURITY-0003",
                    manifest: manifest.clone(),
                    token: "product-to-tools-path".into(),
                });
            }
        }
    }

    if violations.is_empty() {
        Ok(ProductManifestReport {
            scanned_cargo_manifests: manifests.len(),
            product_packages: product_manifests.len(),
        })
    } else {
        Err(violations)
    }
}

pub fn prepare_product_build_proof(
    repository: &Path,
    target: &Path,
    proof_id: &str,
) -> Result<ProductBuildPreparation, Vec<BuildClosureViolation>> {
    if let Err(violations) = check_product_manifests(repository) {
        return Err(manifest_violations_as_build_violations(violations));
    }
    if !valid_proof_id(proof_id) {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            target,
            "invalid-proof-id",
        )]);
    }
    if target.exists() {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            target,
            "target-must-not-exist",
        )]);
    }
    if fs::create_dir(target).is_err() {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            target,
            "cannot-create-fresh-target",
        )]);
    }

    let repository = match fs::canonicalize(repository) {
        Ok(path) => path,
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                repository,
                "cannot-canonicalize-repository",
            )]);
        }
    };
    let canonical_target = match fs::canonicalize(target) {
        Ok(path) => path,
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                target,
                "cannot-canonicalize-target",
            )]);
        }
    };
    if canonical_target.starts_with(&repository) {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            &canonical_target,
            "proof-target-must-be-outside-repository",
        )]);
    }
    let Some(repository_text) = repository.to_str() else {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            &repository,
            "repository-path-must-be-utf8",
        )]);
    };
    if repository_text.contains(['\n', '\r']) {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            &repository,
            "repository-path-contains-newline",
        )]);
    }
    let started_unix_nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                &canonical_target,
                "system-clock-before-unix-epoch",
            )]);
        }
    };
    let marker = format!(
        "schema={PROOF_SCHEMA}\nproof_id={proof_id}\nrepository={repository_text}\nstarted_unix_nanos={started_unix_nanos}\n"
    );
    let marker_path = canonical_target.join(PROOF_MARKER);
    if fs::write(&marker_path, marker).is_err() {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            &marker_path,
            "cannot-write-proof-marker",
        )]);
    }

    Ok(ProductBuildPreparation {
        product_packages: PRODUCT_PACKAGES.len(),
        proof_id: proof_id.into(),
    })
}

pub fn check_product_build_closure(
    repository: &Path,
    target: &Path,
    proof_id: &str,
) -> Result<ProductBuildClosureReport, Vec<BuildClosureViolation>> {
    if let Err(violations) = check_product_manifests(repository) {
        return Err(manifest_violations_as_build_violations(violations));
    }
    let repository = match fs::canonicalize(repository) {
        Ok(path) => path,
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                repository,
                "cannot-canonicalize-repository",
            )]);
        }
    };
    let target = match fs::canonicalize(target) {
        Ok(path) => path,
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                target,
                "missing-proof-target",
            )]);
        }
    };
    if target.starts_with(&repository) {
        return Err(vec![build_violation(
            "RPE-PURITY-0101",
            &target,
            "proof-target-must-be-outside-repository",
        )]);
    }

    let marker_path = target.join(PROOF_MARKER);
    let marker_input = match fs::read_to_string(&marker_path) {
        Ok(input) => input,
        Err(_) => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                &marker_path,
                "missing-proof-marker",
            )]);
        }
    };
    let marker = match parse_proof_marker(&marker_input) {
        Some(marker) => marker,
        None => {
            return Err(vec![build_violation(
                "RPE-PURITY-0101",
                &marker_path,
                "malformed-proof-marker",
            )]);
        }
    };
    let mut proof_violations = Vec::new();
    if marker.proof_id != proof_id {
        proof_violations.push(build_violation(
            "RPE-PURITY-0102",
            &marker_path,
            "proof-id-mismatch",
        ));
    }
    if marker.repository != repository {
        proof_violations.push(build_violation(
            "RPE-PURITY-0102",
            &marker_path,
            "repository-mismatch",
        ));
    }
    let now = SystemTime::now();
    let started = match system_time_from_unix_nanos(marker.started_unix_nanos) {
        Some(started) => started,
        None => {
            proof_violations.push(build_violation(
                "RPE-PURITY-0101",
                &marker_path,
                "invalid-proof-start-time",
            ));
            UNIX_EPOCH
        }
    };
    match now.duration_since(started) {
        Ok(age) if age <= MAX_PROOF_AGE => {}
        Ok(_) => proof_violations.push(build_violation(
            "RPE-PURITY-0103",
            &marker_path,
            "expired-proof-marker",
        )),
        Err(_) => proof_violations.push(build_violation(
            "RPE-PURITY-0103",
            &marker_path,
            "proof-marker-from-future",
        )),
    }
    let marker_modified = match fs::metadata(&marker_path).and_then(|value| value.modified()) {
        Ok(value) => value,
        Err(_) => {
            proof_violations.push(build_violation(
                "RPE-PURITY-0101",
                &marker_path,
                "unreadable-proof-marker-time",
            ));
            started
        }
    };
    if !proof_violations.is_empty() {
        return Err(proof_violations);
    }

    scan_build_inventory(&repository, &target, marker_modified)
}

fn validate_product_package_policy(
    repository: &Path,
    manifests: &[PathBuf],
) -> Vec<PurityViolation> {
    let mut violations = Vec::new();
    let expected: BTreeMap<&str, &ProductPackage> = PRODUCT_PACKAGES
        .iter()
        .map(|package| (package.manifest, package))
        .collect();
    let mut observed = BTreeSet::new();

    for manifest in manifests {
        let relative = manifest
            .strip_prefix(repository)
            .ok()
            .map(path_with_forward_slashes);
        let Some(relative) = relative else {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0004",
                manifest: manifest.clone(),
                token: "product-manifest-outside-repository".into(),
            });
            continue;
        };
        let Some(package) = expected.get(relative.as_str()) else {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0004",
                manifest: manifest.clone(),
                token: "unexpected-product-manifest".into(),
            });
            continue;
        };
        observed.insert(relative);
        match fs::read_to_string(manifest)
            .ok()
            .and_then(|input| package_name(&input))
        {
            Some(name) if name == package.package_name => {}
            Some(name) => violations.push(PurityViolation {
                code: "RPE-PURITY-0004",
                manifest: manifest.clone(),
                token: format!("package-name={name};expected={}", package.package_name),
            }),
            None => violations.push(PurityViolation {
                code: "RPE-PURITY-0004",
                manifest: manifest.clone(),
                token: "missing-package-name".into(),
            }),
        }
    }

    for package in PRODUCT_PACKAGES {
        if !observed.contains(package.manifest) {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0004",
                manifest: repository.join(package.manifest),
                token: "missing-allowlisted-product-manifest".into(),
            });
        }
    }
    violations
}

fn package_name(input: &str) -> Option<String> {
    let mut in_package = false;
    for line in input.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "name" {
            let value = value.trim();
            return value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .map(str::to_owned);
        }
    }
    None
}

fn scan_build_inventory(
    repository: &Path,
    target: &Path,
    not_before: SystemTime,
) -> Result<ProductBuildClosureReport, Vec<BuildClosureViolation>> {
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut violations = Vec::new();
    collect_build_entries(
        target,
        target,
        &mut files,
        &mut directories,
        &mut violations,
    );
    files.sort();
    directories.sort();

    let mut depfile_crates = BTreeSet::new();
    let mut artifact_crates = BTreeSet::new();
    let mut fingerprint_packages = BTreeSet::new();
    let mut artifact_files = 0;
    let mut build_script_artifacts = 0;
    let mut native_artifacts = 0;
    let mut unknown_artifacts = 0;

    for directory in &directories {
        let relative = path_with_forward_slashes(directory);
        if let Some(fingerprint) = relative.strip_prefix("release/.fingerprint/") {
            if fingerprint.contains('/') {
                unknown_artifacts += 1;
                violations.push(build_violation(
                    "RPE-PURITY-0108",
                    &target.join(directory),
                    "unexpected-fingerprint-directory",
                ));
            } else if let Some(package) = package_from_fingerprint(fingerprint) {
                fingerprint_packages.insert(package.package_name);
            } else {
                violations.push(build_violation(
                    "RPE-PURITY-0105",
                    &target.join(directory),
                    "unknown-fingerprint-package",
                ));
            }
        } else if relative.starts_with("release/build/") {
            build_script_artifacts += 1;
            violations.push(build_violation(
                "RPE-PURITY-0106",
                &target.join(directory),
                "build-script-directory",
            ));
        } else if !matches!(
            relative.as_str(),
            "release"
                | "release/.fingerprint"
                | "release/deps"
                | "release/build"
                | "release/examples"
                | "release/incremental"
        ) {
            unknown_artifacts += 1;
            violations.push(build_violation(
                "RPE-PURITY-0108",
                &target.join(directory),
                "unexpected-build-directory",
            ));
        }
    }

    for relative in &files {
        let path = target.join(relative);
        let relative_text = path_with_forward_slashes(relative);
        if relative_text != PROOF_MARKER {
            match fs::metadata(&path).and_then(|value| value.modified()) {
                Ok(modified) if modified >= not_before => {}
                Ok(_) => violations.push(build_violation(
                    "RPE-PURITY-0103",
                    &path,
                    "artifact-predates-proof-marker",
                )),
                Err(_) => violations.push(build_violation(
                    "RPE-PURITY-0101",
                    &path,
                    "unreadable-artifact-time",
                )),
            }
        }
        let normalized = relative_text.to_ascii_lowercase().replace('\\', "/");
        for engine in FORBIDDEN_ENGINES {
            if normalized.contains(engine) {
                violations.push(build_violation("RPE-PURITY-0104", &path, engine));
            }
        }
        if normalized.contains("tools/") {
            violations.push(build_violation(
                "RPE-PURITY-0104",
                &path,
                "product-closure-to-tools",
            ));
        }
        if is_native_artifact(&path) {
            native_artifacts += 1;
            violations.push(build_violation("RPE-PURITY-0107", &path, "native-artifact"));
            continue;
        }

        if matches!(
            relative_text.as_str(),
            PROOF_MARKER | "CACHEDIR.TAG" | ".rustc_info.json" | "release/.cargo-lock"
        ) {
            continue;
        }
        if relative_text.starts_with("release/build/") {
            build_script_artifacts += 1;
            violations.push(build_violation(
                "RPE-PURITY-0106",
                &path,
                "build-script-artifact",
            ));
            continue;
        }
        if let Some(name) = relative_text.strip_prefix("release/deps/") {
            artifact_files += 1;
            match classify_dep_artifact(name) {
                Some((crate_name, DepArtifactKind::Depfile)) => {
                    let package = package_by_crate(crate_name);
                    validate_depfile(repository, &path, package, &mut violations);
                    if let Some(package) = package {
                        depfile_crates.insert(package.crate_name);
                    } else {
                        violations.push(build_violation(
                            "RPE-PURITY-0105",
                            &path,
                            "unknown-depfile-crate",
                        ));
                    }
                }
                Some((crate_name, DepArtifactKind::RustArtifact)) => {
                    if let Some(package) = package_by_crate(crate_name) {
                        artifact_crates.insert(package.crate_name);
                    } else {
                        violations.push(build_violation(
                            "RPE-PURITY-0105",
                            &path,
                            "unknown-rust-artifact-crate",
                        ));
                    }
                }
                None => {
                    unknown_artifacts += 1;
                    violations.push(build_violation(
                        "RPE-PURITY-0108",
                        &path,
                        "unexpected-deps-artifact",
                    ));
                }
            }
            continue;
        }
        if let Some(rest) = relative_text.strip_prefix("release/.fingerprint/") {
            artifact_files += 1;
            let Some((fingerprint, file_name)) = rest.split_once('/') else {
                unknown_artifacts += 1;
                violations.push(build_violation(
                    "RPE-PURITY-0108",
                    &path,
                    "fingerprint-file-without-directory",
                ));
                continue;
            };
            match package_from_fingerprint(fingerprint) {
                Some(package) if allowed_fingerprint_file(file_name, package.crate_name) => {}
                Some(_) => {
                    unknown_artifacts += 1;
                    violations.push(build_violation(
                        "RPE-PURITY-0108",
                        &path,
                        "unexpected-fingerprint-artifact",
                    ));
                }
                None => violations.push(build_violation(
                    "RPE-PURITY-0105",
                    &path,
                    "unknown-fingerprint-package",
                )),
            }
            continue;
        }
        if let Some(crate_name) = classify_top_level_rust_artifact(&relative_text) {
            artifact_files += 1;
            if let Some(package) = package_by_crate(crate_name) {
                artifact_crates.insert(package.crate_name);
            } else {
                violations.push(build_violation(
                    "RPE-PURITY-0105",
                    &path,
                    "unknown-top-level-crate",
                ));
            }
            continue;
        }

        unknown_artifacts += 1;
        violations.push(build_violation(
            "RPE-PURITY-0108",
            &path,
            "unexpected-build-artifact",
        ));
    }

    for package in PRODUCT_PACKAGES {
        if !depfile_crates.contains(package.crate_name) {
            violations.push(build_violation(
                "RPE-PURITY-0109",
                &target.join("release/deps"),
                &format!("missing-depfile={}", package.crate_name),
            ));
        }
        if !artifact_crates.contains(package.crate_name) {
            violations.push(build_violation(
                "RPE-PURITY-0109",
                &target.join("release/deps"),
                &format!("missing-rust-artifact={}", package.crate_name),
            ));
        }
        if !fingerprint_packages.contains(package.package_name) {
            violations.push(build_violation(
                "RPE-PURITY-0109",
                &target.join("release/.fingerprint"),
                &format!("missing-fingerprint={}", package.package_name),
            ));
        }
    }

    if violations.is_empty() {
        Ok(ProductBuildClosureReport {
            product_packages: PRODUCT_PACKAGES.len(),
            depfiles: depfile_crates.len(),
            artifact_files,
            fingerprint_directories: fingerprint_packages.len(),
            build_script_artifacts,
            native_artifacts,
            unknown_artifacts,
        })
    } else {
        Err(violations)
    }
}

fn validate_depfile(
    repository: &Path,
    path: &Path,
    package: Option<&ProductPackage>,
    violations: &mut Vec<BuildClosureViolation>,
) {
    let input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0101",
                path,
                "unreadable-depfile",
            ));
            return;
        }
    };
    let normalized = input.to_ascii_lowercase().replace('\\', "/");
    for engine in FORBIDDEN_ENGINES {
        if normalized.contains(engine) {
            violations.push(build_violation("RPE-PURITY-0104", path, engine));
        }
    }
    if normalized.contains("tools/") {
        violations.push(build_violation(
            "RPE-PURITY-0104",
            path,
            "product-closure-to-tools",
        ));
    }

    let expected_source_root = package.map(|package| {
        Path::new(package.manifest)
            .parent()
            .expect("allowlisted manifest has a parent")
    });
    let mut saw_expected_source = false;
    for token in input.split_whitespace() {
        let token = token.trim_end_matches(['\\', ':']);
        if !token.ends_with(".rs") {
            continue;
        }
        let source = Path::new(token);
        let relative = if source.is_absolute() {
            match source.strip_prefix(repository) {
                Ok(relative) => relative,
                Err(_) => {
                    violations.push(build_violation(
                        "RPE-PURITY-0105",
                        path,
                        "source-outside-repository",
                    ));
                    continue;
                }
            }
        } else {
            source
        };
        if !path_is_under_product_root(relative) {
            violations.push(build_violation(
                "RPE-PURITY-0105",
                path,
                "source-outside-product-roots",
            ));
        }
        if expected_source_root.is_some_and(|root| relative.starts_with(root)) {
            saw_expected_source = true;
        }
    }
    if package.is_some() && !saw_expected_source {
        violations.push(build_violation(
            "RPE-PURITY-0109",
            path,
            "depfile-missing-own-source",
        ));
    }
}

fn collect_build_entries(
    root: &Path,
    current: &Path,
    files: &mut Vec<PathBuf>,
    directories: &mut Vec<PathBuf>,
    violations: &mut Vec<BuildClosureViolation>,
) {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0101",
                current,
                "unreadable-build-tree",
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                violations.push(build_violation(
                    "RPE-PURITY-0101",
                    current,
                    "unreadable-build-entry",
                ));
                continue;
            }
        };
        let path = entry.path();
        let relative = match path.strip_prefix(root) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => continue,
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                violations.push(build_violation(
                    "RPE-PURITY-0101",
                    &path,
                    "unreadable-build-entry-type",
                ));
                continue;
            }
        };
        if file_type.is_symlink() {
            violations.push(build_violation(
                "RPE-PURITY-0108",
                &path,
                "symlink-in-build-proof",
            ));
        } else if file_type.is_dir() {
            directories.push(relative);
            collect_build_entries(root, &path, files, directories, violations);
        } else if file_type.is_file() {
            files.push(relative);
        } else {
            violations.push(build_violation(
                "RPE-PURITY-0108",
                &path,
                "special-file-in-build-proof",
            ));
        }
    }
}

fn classify_dep_artifact(name: &str) -> Option<(&str, DepArtifactKind)> {
    if let Some(stem) = name.strip_suffix(".d") {
        return strip_cargo_hash(stem).map(|crate_name| (crate_name, DepArtifactKind::Depfile));
    }
    for suffix in [".rlib", ".rmeta"] {
        if let Some(stem) = name
            .strip_suffix(suffix)
            .and_then(|stem| stem.strip_prefix("lib"))
        {
            return strip_cargo_hash(stem)
                .map(|crate_name| (crate_name, DepArtifactKind::RustArtifact));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DepArtifactKind {
    Depfile,
    RustArtifact,
}

fn classify_top_level_rust_artifact(relative: &str) -> Option<&str> {
    let file = relative.strip_prefix("release/lib")?;
    file.strip_suffix(".rlib")
        .or_else(|| file.strip_suffix(".d"))
}

fn strip_cargo_hash(stem: &str) -> Option<&str> {
    let (name, hash) = stem.rsplit_once('-')?;
    (hash.len() == CARGO_HASH_LENGTH && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(name)
}

fn package_from_fingerprint(value: &str) -> Option<&'static ProductPackage> {
    let (package, hash) = value.rsplit_once('-')?;
    if hash.len() != CARGO_HASH_LENGTH || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.package_name == package)
}

fn package_by_crate(crate_name: &str) -> Option<&'static ProductPackage> {
    PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.crate_name == crate_name)
}

fn allowed_fingerprint_file(file: &str, crate_name: &str) -> bool {
    file == "invoked.timestamp"
        || file == format!("lib-{crate_name}")
        || file == format!("lib-{crate_name}.json")
        || file == format!("dep-lib-{crate_name}")
        || file == format!("output-lib-{crate_name}")
}

fn is_native_artifact(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("a" | "so" | "dylib" | "dll" | "lib" | "o" | "obj" | "wasm")
    )
}

fn path_is_under_product_root(path: &Path) -> bool {
    match path.components().next() {
        Some(Component::Normal(component)) => PRODUCT_ROOTS
            .iter()
            .any(|root| component == std::ffi::OsStr::new(root)),
        _ => false,
    }
}

fn parse_proof_marker(input: &str) -> Option<ProofMarker> {
    let fields: BTreeMap<&str, &str> = input
        .lines()
        .map(|line| line.split_once('='))
        .collect::<Option<_>>()?;
    if fields.len() != 4 || fields.get("schema") != Some(&PROOF_SCHEMA) {
        return None;
    }
    let proof_id = *fields.get("proof_id")?;
    if !valid_proof_id(proof_id) {
        return None;
    }
    Some(ProofMarker {
        proof_id: proof_id.into(),
        repository: PathBuf::from(*fields.get("repository")?),
        started_unix_nanos: fields.get("started_unix_nanos")?.parse().ok()?,
    })
}

fn system_time_from_unix_nanos(value: u128) -> Option<SystemTime> {
    let seconds = value / 1_000_000_000;
    let nanoseconds = (value % 1_000_000_000) as u32;
    let seconds = u64::try_from(seconds).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))
}

fn valid_proof_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn manifest_violations_as_build_violations(
    violations: Vec<PurityViolation>,
) -> Vec<BuildClosureViolation> {
    violations
        .into_iter()
        .map(|violation| BuildClosureViolation {
            code: violation.code,
            path: violation.manifest,
            token: violation.token,
        })
        .collect()
}

fn build_violation(code: &'static str, path: &Path, token: &str) -> BuildClosureViolation {
    BuildClosureViolation {
        code,
        path: path.to_path_buf(),
        token: token.into(),
    }
}

fn path_with_forward_slashes(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn collect_manifests(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_manifests(&path, output)?;
        } else if file_type.is_file() && entry.file_name() == "Cargo.toml" {
            output.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    const HASH: &str = "0123456789abcdef";

    #[test]
    fn ignores_tool_only_baseline_and_accepts_native_product_manifests() {
        assert_eq!(PRODUCT_PACKAGES.len(), PRODUCT_PACKAGE_COUNT);
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "core/content/Cargo.toml",
            package_name: "pdf-rs-content",
            crate_name: "pdf_rs_content",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "core/raster/Cargo.toml",
            package_name: "pdf-rs-raster",
            crate_name: "pdf_rs_raster",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "core/font/Cargo.toml",
            package_name: "pdf-rs-font",
            crate_name: "pdf_rs_font",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/policy/Cargo.toml",
            package_name: "pdf-rs-policy",
            crate_name: "pdf_rs_policy",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/protocol/Cargo.toml",
            package_name: "pdf-rs-protocol",
            crate_name: "pdf_rs_protocol",
        }));
        let root = temp_dir("isolated");
        write_product_manifests(&root);
        write_manifest(
            &root.join("tools/baseline/Cargo.toml"),
            "[package]\nname = \"pdfium-wrapper\"\n",
        );
        assert_eq!(
            check_product_manifests(&root),
            Ok(ProductManifestReport {
                scanned_cargo_manifests: PRODUCT_PACKAGES.len(),
                product_packages: PRODUCT_PACKAGES.len(),
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_external_engine_and_harmlessly_named_tool_path() {
        let root = temp_dir("forbidden");
        write_product_manifests(&root);
        write_manifest(
            &root.join("runtime/session/Cargo.toml"),
            "[package]\nname = \"pdf-rs-session\"\n[dependencies]\nrender = { package = \"pdfium\", version = \"1\" }\nharmless = { path = \"../../tools/opaque\" }\n",
        );
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|value| value.code == "RPE-PURITY-0002")
        );
        assert!(
            violations
                .iter()
                .any(|value| value.code == "RPE-PURITY-0003")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_forbidden_workspace_dependencies() {
        let root = temp_dir("workspace-forbidden");
        write_product_manifests(&root);
        write_manifest(
            &root.join("Cargo.toml"),
            "[workspace.dependencies]\nrender = { package = \"pdfium\", path = \"../pdfium\" }\n",
        );
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0002" && value.manifest == root.join("Cargo.toml")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_product_manifest_not_in_bidirectional_allowlist() {
        let root = temp_dir("unexpected-product");
        write_product_manifests(&root);
        write_manifest(
            &root.join("platform/new-backend/Cargo.toml"),
            "[package]\nname = \"pdf-rs-new-backend\"\n",
        );
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0004" && value.token == "unexpected-product-manifest"
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_raster_manifest_from_bidirectional_allowlist() {
        let root = temp_dir("missing-raster");
        write_product_manifests(&root);
        let raster_manifest = root.join("core/raster/Cargo.toml");
        fs::remove_file(&raster_manifest).unwrap();

        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0004"
                && value.manifest == raster_manifest
                && value.token == "missing-allowlisted-product-manifest"
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepare_rejects_non_fresh_target() {
        let root = temp_dir("non-fresh-repository");
        let target = temp_dir("non-fresh-target");
        write_product_manifests(&root);
        let violations = prepare_product_build_proof(&root, &target, "proof-1").unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0101" && value.token == "target-must-not-exist"
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn accepts_complete_fresh_release_product_closure() {
        let root = temp_dir("complete-repository");
        let proof_root = temp_dir("complete-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-2").unwrap();
        write_valid_build_inventory(&target);

        let report = check_product_build_closure(&root, &target, "proof-2").unwrap();
        assert_eq!(report.product_packages, PRODUCT_PACKAGES.len());
        assert_eq!(report.depfiles, PRODUCT_PACKAGES.len());
        assert_eq!(report.fingerprint_directories, PRODUCT_PACKAGES.len());
        assert_eq!(report.build_script_artifacts, 0);
        assert_eq!(report.native_artifacts, 0);
        assert_eq!(report.unknown_artifacts, 0);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_harmlessly_named_indirect_tool_depfile() {
        let root = temp_dir("indirect-tool-repository");
        let proof_root = temp_dir("indirect-tool-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-3").unwrap();
        write_valid_build_inventory(&target);
        fs::write(
            target.join(format!("release/deps/harmless_dependency-{HASH}.d")),
            "target: tools/opaque/src/lib.rs\n",
        )
        .unwrap();

        let violations = check_product_build_closure(&root, &target, "proof-3").unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0104" && value.token == "product-closure-to-tools"
        }));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_unknown_release_artifact() {
        let root = temp_dir("unknown-artifact-repository");
        let proof_root = temp_dir("unknown-artifact-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-4").unwrap();
        write_valid_build_inventory(&target);
        fs::write(target.join("release/deps/unexplained.bin"), b"unknown").unwrap();

        let violations = check_product_build_closure(&root, &target, "proof-4").unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0108" && value.token == "unexpected-deps-artifact"
        }));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_release_closure_missing_raster_outputs() {
        let root = temp_dir("missing-raster-build-repository");
        let proof_root = temp_dir("missing-raster-build-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-raster").unwrap();
        write_valid_build_inventory(&target);
        fs::remove_file(target.join(format!("release/deps/pdf_rs_raster-{HASH}.d"))).unwrap();
        fs::remove_file(target.join(format!("release/deps/libpdf_rs_raster-{HASH}.rlib"))).unwrap();
        fs::remove_file(target.join(format!("release/deps/libpdf_rs_raster-{HASH}.rmeta")))
            .unwrap();
        fs::remove_dir_all(target.join(format!("release/.fingerprint/pdf-rs-raster-{HASH}")))
            .unwrap();

        let violations = check_product_build_closure(&root, &target, "proof-raster").unwrap_err();
        for token in [
            "missing-depfile=pdf_rs_raster",
            "missing-rust-artifact=pdf_rs_raster",
            "missing-fingerprint=pdf-rs-raster",
        ] {
            assert!(
                violations
                    .iter()
                    .any(|value| value.code == "RPE-PURITY-0109" && value.token == token),
                "missing expected raster closure violation {token:?}: {violations:?}"
            );
        }

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_release_closure_missing_protocol_outputs() {
        let root = temp_dir("missing-protocol-build-repository");
        let proof_root = temp_dir("missing-protocol-build-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-protocol").unwrap();
        write_valid_build_inventory(&target);
        fs::remove_file(target.join(format!("release/deps/pdf_rs_protocol-{HASH}.d"))).unwrap();
        fs::remove_file(target.join(format!("release/deps/libpdf_rs_protocol-{HASH}.rlib")))
            .unwrap();
        fs::remove_file(target.join(format!("release/deps/libpdf_rs_protocol-{HASH}.rmeta")))
            .unwrap();
        fs::remove_dir_all(target.join(format!("release/.fingerprint/pdf-rs-protocol-{HASH}")))
            .unwrap();

        let violations = check_product_build_closure(&root, &target, "proof-protocol").unwrap_err();
        for token in [
            "missing-depfile=pdf_rs_protocol",
            "missing-rust-artifact=pdf_rs_protocol",
            "missing-fingerprint=pdf-rs-protocol",
        ] {
            assert!(
                violations
                    .iter()
                    .any(|value| value.code == "RPE-PURITY-0109" && value.token == token),
                "missing expected protocol closure violation {token:?}: {violations:?}"
            );
        }

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_artifact_older_than_freshness_boundary() {
        let root = temp_dir("stale-repository");
        let target = temp_dir("stale-target");
        write_product_manifests(&root);
        fs::write(target.join(PROOF_MARKER), b"test-only").unwrap();
        write_valid_build_inventory(&target);
        let future_boundary = SystemTime::now()
            .checked_add(Duration::from_secs(1))
            .unwrap();

        let violations = scan_build_inventory(&root, &target, future_boundary).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0103" && value.token == "artifact-predates-proof-marker"
        }));

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(target).unwrap();
    }

    fn write_product_manifests(root: &Path) {
        for package in PRODUCT_PACKAGES {
            write_manifest(
                &root.join(package.manifest),
                &format!("[package]\nname = \"{}\"\n", package.package_name),
            );
        }
    }

    fn write_valid_build_inventory(target: &Path) {
        for directory in [
            "release/deps",
            "release/.fingerprint",
            "release/build",
            "release/examples",
            "release/incremental",
        ] {
            fs::create_dir_all(target.join(directory)).unwrap();
        }
        fs::write(target.join("CACHEDIR.TAG"), b"cargo cache").unwrap();
        fs::write(target.join(".rustc_info.json"), b"{}").unwrap();
        fs::write(target.join("release/.cargo-lock"), b"").unwrap();
        for package in PRODUCT_PACKAGES {
            let source_root = Path::new(package.manifest).parent().unwrap();
            let source = source_root.join("src/lib.rs");
            let depfile = target.join(format!("release/deps/{}-{HASH}.d", package.crate_name));
            fs::write(&depfile, format!("target: {}\n", source.display())).unwrap();
            fs::write(
                target.join(format!(
                    "release/deps/lib{}-{HASH}.rlib",
                    package.crate_name
                )),
                b"rlib",
            )
            .unwrap();
            fs::write(
                target.join(format!(
                    "release/deps/lib{}-{HASH}.rmeta",
                    package.crate_name
                )),
                b"rmeta",
            )
            .unwrap();
            let fingerprint = target.join(format!(
                "release/.fingerprint/{}-{HASH}",
                package.package_name
            ));
            fs::create_dir_all(&fingerprint).unwrap();
            fs::write(fingerprint.join("invoked.timestamp"), b"fresh").unwrap();
            fs::write(
                fingerprint.join(format!("lib-{}", package.crate_name)),
                b"fingerprint",
            )
            .unwrap();
            fs::write(
                fingerprint.join(format!("lib-{}.json", package.crate_name)),
                b"{}",
            )
            .unwrap();
        }
    }

    fn write_manifest(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-purity-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
