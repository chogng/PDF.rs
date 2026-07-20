use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
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
const DESKTOP_WORKER_BINARY: &str = "pdf-rs-desktop-worker";
const DESKTOP_WORKER_CRATE: &str = "pdf_rs_desktop_worker";
const DESKTOP_WORKER_MAX_BYTES: u64 = 64 * 1024 * 1024;
const DESKTOP_WORKER_PRODUCT_FEATURE_MARKER: &[u8] =
    b"PDF_RS_DESKTOP_FEATURE_CLOSURE:NO_DEFAULT_FEATURES:v1";
const DESKTOP_WORKER_FIXTURE_FEATURE_MARKER: &[u8] =
    b"PDF_RS_DESKTOP_FEATURE_CLOSURE:TRANSPORT_FIXTURE:v1";
const DESKTOP_MANIFEST_PATH: &str = "platform/desktop/Cargo.toml";
const DESKTOP_SANDBOX_TARGET_PATH: &str = "platform/desktop/macos/sandbox-target.toml";
#[cfg(test)]
const DESKTOP_SANDBOX_TARGET_CONTENT: &str =
    include_str!("../../../platform/desktop/macos/sandbox-target.toml");
const THIRD_PARTY_REGISTRY_PATH: &str = "platform/desktop/product-dependency-registry.toml";
const THIRD_PARTY_REGISTRY_CONTENT: &str =
    include_str!("../../../platform/desktop/product-dependency-registry.toml");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProductPackage {
    manifest: &'static str,
    package_name: &'static str,
    crate_name: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThirdPartyProductPackage {
    package_name: &'static str,
    crate_name: &'static str,
    version: &'static str,
    checksum: &'static str,
    allows_build_script: bool,
    max_release_artifact_bytes: u64,
}

const THIRD_PARTY_PRODUCT_PACKAGES: &[ThirdPartyProductPackage; 7] = &[
    ThirdPartyProductPackage {
        package_name: "bitflags",
        crate_name: "bitflags",
        version: "2.13.1",
        checksum: "b588b76d00fde79687d7646a9b5bdf3cc0f655e0bbd080335a95d7e96f3587da",
        allows_build_script: false,
        max_release_artifact_bytes: 4 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "errno",
        crate_name: "errno",
        version: "0.3.14",
        checksum: "39cab71617ae0d63f51a36d69f866391735b51691dbda63cf6f96d042b63efeb",
        allows_build_script: false,
        max_release_artifact_bytes: 4 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "libc",
        crate_name: "libc",
        version: "0.2.186",
        checksum: "68ab91017fe16c622486840e4c83c9a37afeff978bd239b5293d61ece587de66",
        allows_build_script: true,
        max_release_artifact_bytes: 16 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "linux-raw-sys",
        crate_name: "linux_raw_sys",
        version: "0.12.1",
        checksum: "32a66949e030da00e8c7d4434b251670a91556f4144941d37452769c25d58a53",
        allows_build_script: false,
        max_release_artifact_bytes: 16 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "rustix",
        crate_name: "rustix",
        version: "1.1.4",
        checksum: "b6fe4565b9518b83ef4f91bb47ce29620ca828bd32cb7e408f0062e9930ba190",
        allows_build_script: true,
        max_release_artifact_bytes: 32 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "windows-link",
        crate_name: "windows_link",
        version: "0.2.1",
        checksum: "f0805222e57f7521d6a62e36fa9163bc891acd422f971defe97d64e70d0a4fe5",
        allows_build_script: false,
        max_release_artifact_bytes: 4 * 1024 * 1024,
    },
    ThirdPartyProductPackage {
        package_name: "windows-sys",
        crate_name: "windows_sys",
        version: "0.61.2",
        checksum: "ae137229bcbd6cdf0f7b80a31df61766145077ddf49416a728b02cb3921ff3fc",
        allows_build_script: false,
        max_release_artifact_bytes: 16 * 1024 * 1024,
    },
];

const PRODUCT_PACKAGE_COUNT: usize = 24;
const PRODUCT_PACKAGES: &[ProductPackage; PRODUCT_PACKAGE_COUNT] = &[
    ProductPackage {
        manifest: "pdf-rs/bytes/Cargo.toml",
        package_name: "pdf-rs-bytes",
        crate_name: "pdf_rs_bytes",
    },
    ProductPackage {
        manifest: "pdf-rs/content/Cargo.toml",
        package_name: "pdf-rs-content",
        crate_name: "pdf_rs_content",
    },
    ProductPackage {
        manifest: "pdf-rs/document/Cargo.toml",
        package_name: "pdf-rs-document",
        crate_name: "pdf_rs_document",
    },
    ProductPackage {
        manifest: "pdf-rs/filters/Cargo.toml",
        package_name: "pdf-rs-filters",
        crate_name: "pdf_rs_filters",
    },
    ProductPackage {
        manifest: "pdf-rs/fast-raster/Cargo.toml",
        package_name: "pdf-rs-fast-raster",
        crate_name: "pdf_rs_fast_raster",
    },
    ProductPackage {
        manifest: "pdf-rs/font/Cargo.toml",
        package_name: "pdf-rs-font",
        crate_name: "pdf_rs_font",
    },
    ProductPackage {
        manifest: "pdf-rs/object/Cargo.toml",
        package_name: "pdf-rs-object",
        crate_name: "pdf_rs_object",
    },
    ProductPackage {
        manifest: "pdf-rs/raster/Cargo.toml",
        package_name: "pdf-rs-raster",
        crate_name: "pdf_rs_raster",
    },
    ProductPackage {
        manifest: "pdf-rs/scene/Cargo.toml",
        package_name: "pdf-rs-scene",
        crate_name: "pdf_rs_scene",
    },
    ProductPackage {
        manifest: "pdf-rs/syntax/Cargo.toml",
        package_name: "pdf-rs-syntax",
        crate_name: "pdf_rs_syntax",
    },
    ProductPackage {
        manifest: "pdf-rs/xref/Cargo.toml",
        package_name: "pdf-rs-xref",
        crate_name: "pdf_rs_xref",
    },
    ProductPackage {
        manifest: "platform/browser/worker/Cargo.toml",
        package_name: "pdf-rs-browser-worker",
        crate_name: "pdf_rs_browser_worker",
    },
    ProductPackage {
        manifest: "platform/desktop/Cargo.toml",
        package_name: "pdf-rs-desktop",
        crate_name: "pdf_rs_desktop",
    },
    ProductPackage {
        manifest: "platform/electron-bridge/Cargo.toml",
        package_name: "pdf-rs-electron-bridge",
        crate_name: "pdf_rs_electron_bridge",
    },
    ProductPackage {
        manifest: "platform/macos-spawn/Cargo.toml",
        package_name: "pdf-rs-macos-spawn",
        crate_name: "pdf_rs_macos_spawn",
    },
    ProductPackage {
        manifest: "runtime/cache/Cargo.toml",
        package_name: "pdf-rs-cache",
        crate_name: "pdf_rs_cache",
    },
    ProductPackage {
        manifest: "runtime/engine/Cargo.toml",
        package_name: "pdf-rs-engine",
        crate_name: "pdf_rs_engine",
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
        manifest: "runtime/scheduler/Cargo.toml",
        package_name: "pdf-rs-scheduler",
        crate_name: "pdf_rs_scheduler",
    },
    ProductPackage {
        manifest: "runtime/session/Cargo.toml",
        package_name: "pdf-rs-session",
        crate_name: "pdf_rs_session",
    },
    ProductPackage {
        manifest: "runtime/surface/Cargo.toml",
        package_name: "pdf-rs-surface",
        crate_name: "pdf_rs_surface",
    },
    ProductPackage {
        manifest: "runtime/tile-cache/Cargo.toml",
        package_name: "pdf-rs-tile-cache",
        crate_name: "pdf_rs_tile_cache",
    },
    ProductPackage {
        manifest: "runtime/viewer/Cargo.toml",
        package_name: "pdf-rs-viewer",
        crate_name: "pdf_rs_viewer",
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
    pub product_binaries: usize,
    pub desktop_feature_fingerprints: usize,
    pub desktop_fingerprint_directories: usize,
    pub desktop_worker_sha256: String,
    pub registered_third_party_packages: usize,
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
    violations.extend(validate_third_party_product_registry(repository));
    violations.extend(validate_desktop_product_feature_policy(repository));

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

fn validate_third_party_product_registry(repository: &Path) -> Vec<PurityViolation> {
    let registry_path = repository.join(THIRD_PARTY_REGISTRY_PATH);
    let mut violations = Vec::new();
    match fs::read_to_string(&registry_path) {
        Ok(input) if input == THIRD_PARTY_REGISTRY_CONTENT => {}
        Ok(_) => violations.push(PurityViolation {
            code: "RPE-PURITY-0005",
            manifest: registry_path,
            token: "third-party-registry-drift".into(),
        }),
        Err(_) => violations.push(PurityViolation {
            code: "RPE-PURITY-0005",
            manifest: registry_path,
            token: "missing-third-party-registry".into(),
        }),
    }

    let lock_path = repository.join("Cargo.lock");
    let lock = match fs::read_to_string(&lock_path) {
        Ok(input) => input,
        Err(_) => {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0005",
                manifest: lock_path,
                token: "missing-cargo-lock".into(),
            });
            return violations;
        }
    };
    for package in THIRD_PARTY_PRODUCT_PACKAGES {
        let Some(fields) = cargo_lock_package_fields(&lock, package.package_name) else {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0005",
                manifest: lock_path.clone(),
                token: format!("missing-third-party-lock={}", package.package_name),
            });
            continue;
        };
        let expected_source = "registry+https://github.com/rust-lang/crates.io-index";
        if fields.get("version").map(String::as_str) != Some(package.version)
            || fields.get("source").map(String::as_str) != Some(expected_source)
            || fields.get("checksum").map(String::as_str) != Some(package.checksum)
        {
            violations.push(PurityViolation {
                code: "RPE-PURITY-0005",
                manifest: lock_path.clone(),
                token: format!("third-party-lock-drift={}", package.package_name),
            });
        }
    }
    violations
}

fn validate_desktop_product_feature_policy(repository: &Path) -> Vec<PurityViolation> {
    let manifest_path = repository.join(DESKTOP_MANIFEST_PATH);
    let target_path = repository.join(DESKTOP_SANDBOX_TARGET_PATH);
    let mut violations = Vec::new();

    let expected_features = BTreeMap::from([
        ("default".to_owned(), "[]".to_owned()),
        ("transport-fixture".to_owned(), "[]".to_owned()),
    ]);
    match fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|input| single_line_table_assignments(&input, "features"))
    {
        Some(features) if features == expected_features => {}
        Some(_) => violations.push(PurityViolation {
            code: "RPE-PURITY-0006",
            manifest: manifest_path.clone(),
            token: "desktop-feature-table-drift".into(),
        }),
        None => violations.push(PurityViolation {
            code: "RPE-PURITY-0006",
            manifest: manifest_path,
            token: "missing-or-malformed-desktop-feature-table".into(),
        }),
    }

    let expected_closure = BTreeMap::from([
        (
            "closure_scope".to_owned(),
            "\"package_prerequisite\"".to_owned(),
        ),
        ("prerequisite_complete".to_owned(), "true".to_owned()),
        ("cargo_package".to_owned(), "\"pdf-rs-desktop\"".to_owned()),
        (
            "cargo_binary".to_owned(),
            "\"pdf-rs-desktop-worker\"".to_owned(),
        ),
        ("release_profile".to_owned(), "true".to_owned()),
        ("default_features_enabled".to_owned(), "false".to_owned()),
        (
            "forbidden_features".to_owned(),
            "[\"transport-fixture\"]".to_owned(),
        ),
        (
            "required_fingerprint_features".to_owned(),
            "\"[]\"".to_owned(),
        ),
        (
            "required_declared_features".to_owned(),
            "[\"default\",\"transport-fixture\"]".to_owned(),
        ),
        (
            "fresh_external_target_required".to_owned(),
            "true".to_owned(),
        ),
        (
            "required_desktop_fingerprint_directories".to_owned(),
            "2".to_owned(),
        ),
        (
            "single_library_fingerprint_required".to_owned(),
            "true".to_owned(),
        ),
        (
            "single_worker_fingerprint_required".to_owned(),
            "true".to_owned(),
        ),
        (
            "worker_feature_marker_required".to_owned(),
            "\"no_default_features_v1\"".to_owned(),
        ),
        (
            "worker_fixture_marker_forbidden".to_owned(),
            "\"transport_fixture_v1\"".to_owned(),
        ),
        (
            "worker_cargo_filename_association_required".to_owned(),
            "true".to_owned(),
        ),
        (
            "matching_worker_sha256_observation_required".to_owned(),
            "true".to_owned(),
        ),
        (
            "worker_content_provenance_proved".to_owned(),
            "false".to_owned(),
        ),
        (
            "fixture_launch_api_product_visible".to_owned(),
            "false".to_owned(),
        ),
        ("product_package_produced".to_owned(), "false".to_owned()),
        ("universal_binary_proof".to_owned(), "false".to_owned()),
        ("signed_package_proof".to_owned(), "false".to_owned()),
    ]);
    match fs::read_to_string(&target_path)
        .ok()
        .and_then(|input| single_line_table_assignments(&input, "product_feature_closure"))
    {
        Some(closure) if closure == expected_closure => {}
        Some(_) => violations.push(PurityViolation {
            code: "RPE-PURITY-0006",
            manifest: target_path.clone(),
            token: "product-feature-closure-declaration-drift".into(),
        }),
        None => violations.push(PurityViolation {
            code: "RPE-PURITY-0006",
            manifest: target_path,
            token: "missing-or-malformed-product-feature-closure".into(),
        }),
    }

    violations
}

fn single_line_table_assignments(input: &str, table: &str) -> Option<BTreeMap<String, String>> {
    let expected_header = format!("[{table}]");
    let mut in_table = false;
    let mut found = false;
    let mut assignments = BTreeMap::new();
    for line in input.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') && line.ends_with(']') {
            if line == expected_header {
                if found {
                    return None;
                }
                found = true;
                in_table = true;
            } else {
                in_table = false;
            }
            continue;
        }
        if !in_table || line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let value = value
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        if value.is_empty() || assignments.insert(key.to_owned(), value).is_some() {
            return None;
        }
    }
    found.then_some(assignments)
}

fn cargo_lock_package_fields(input: &str, package_name: &str) -> Option<BTreeMap<String, String>> {
    input
        .split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let fields: BTreeMap<String, String> = block
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    let value = value
                        .trim()
                        .strip_prefix('"')?
                        .strip_suffix('"')?
                        .to_owned();
                    Some((key.trim().to_owned(), value))
                })
                .collect();
            (fields.get("name").map(String::as_str) == Some(package_name)).then_some(fields)
        })
        .next()
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
    let mut third_party_depfile_crates = BTreeSet::new();
    let mut third_party_artifact_crates = BTreeSet::new();
    let mut third_party_fingerprint_packages = BTreeSet::new();
    let mut third_party_build_packages = BTreeSet::new();
    let mut observed_third_party_packages = BTreeSet::new();
    let mut product_binary_artifacts =
        BTreeMap::<ProductBinaryArtifact, Vec<ProductBinaryArtifactObservation>>::new();
    let mut desktop_feature_fingerprints =
        BTreeMap::<DesktopFeatureFingerprint, Vec<DesktopFeatureFingerprintObservation>>::new();
    let mut desktop_fingerprint_directories = BTreeMap::<String, PathBuf>::new();
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
                if package.package_name == "pdf-rs-desktop" {
                    desktop_fingerprint_directories.insert(
                        fingerprint_cargo_hash(fingerprint)
                            .expect("validated product fingerprint directory")
                            .to_owned(),
                        target.join(directory),
                    );
                }
            } else if let Some(package) = third_party_package_from_fingerprint(fingerprint) {
                third_party_fingerprint_packages.insert(package.package_name);
                observed_third_party_packages.insert(package.package_name);
            } else {
                violations.push(build_violation(
                    "RPE-PURITY-0105",
                    &target.join(directory),
                    "unknown-fingerprint-package",
                ));
            }
        } else if relative.starts_with("release/build/") {
            match registered_build_directory(&relative) {
                Some(package) => {
                    build_script_artifacts += 1;
                    third_party_build_packages.insert(package.package_name);
                    observed_third_party_packages.insert(package.package_name);
                }
                None => {
                    build_script_artifacts += 1;
                    violations.push(build_violation(
                        "RPE-PURITY-0106",
                        &target.join(directory),
                        "unregistered-build-script-directory",
                    ));
                }
            }
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
        if matches!(
            relative_text.as_str(),
            PROOF_MARKER | "CACHEDIR.TAG" | ".rustc_info.json" | "release/.cargo-lock"
        ) {
            continue;
        }
        if relative_text.starts_with("release/build/") {
            build_script_artifacts += 1;
            if let Some(package) = registered_build_artifact(&relative_text) {
                third_party_build_packages.insert(package.package_name);
                observed_third_party_packages.insert(package.package_name);
                validate_third_party_artifact_size(&path, package, &mut violations);
                validate_registered_build_artifact_contents(&path, &mut violations);
            } else {
                violations.push(build_violation(
                    "RPE-PURITY-0106",
                    &path,
                    "unregistered-build-script-artifact",
                ));
            }
            continue;
        }
        if let Some((binary_artifact, cargo_hash)) =
            classify_product_binary_artifact(&relative_text)
        {
            artifact_files += 1;
            if binary_artifact.is_depfile() {
                validate_depfile(
                    repository,
                    &path,
                    product_package_by_name("pdf-rs-desktop"),
                    &mut violations,
                );
            } else {
                validate_product_binary_executable(&path, &mut violations);
            }
            product_binary_artifacts
                .entry(binary_artifact)
                .or_default()
                .push(ProductBinaryArtifactObservation {
                    path,
                    cargo_hash: cargo_hash.map(str::to_owned),
                });
            continue;
        }
        if is_native_artifact(&path) {
            native_artifacts += 1;
            violations.push(build_violation("RPE-PURITY-0107", &path, "native-artifact"));
            continue;
        }
        if let Some(name) = relative_text.strip_prefix("release/deps/") {
            artifact_files += 1;
            match classify_dep_artifact(name) {
                Some((crate_name, DepArtifactKind::Depfile)) => {
                    let package = package_by_crate(crate_name);
                    if let Some(package) = package {
                        validate_depfile(repository, &path, Some(package), &mut violations);
                        depfile_crates.insert(package.crate_name);
                    } else if let Some(package) = third_party_package_by_crate(crate_name) {
                        validate_third_party_depfile(&path, package, &mut violations);
                        validate_third_party_artifact_size(&path, package, &mut violations);
                        third_party_depfile_crates.insert(package.crate_name);
                        observed_third_party_packages.insert(package.package_name);
                    } else {
                        validate_depfile(repository, &path, None, &mut violations);
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
                    } else if let Some(package) = third_party_package_by_crate(crate_name) {
                        validate_third_party_artifact_size(&path, package, &mut violations);
                        third_party_artifact_crates.insert(package.crate_name);
                        observed_third_party_packages.insert(package.package_name);
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
                Some(package) if allowed_product_fingerprint_file(file_name, package) => {
                    if let Some(feature_fingerprint) =
                        classify_desktop_feature_fingerprint(file_name, package)
                    {
                        let metadata = validate_desktop_feature_fingerprint(
                            &path,
                            feature_fingerprint,
                            &mut violations,
                        );
                        desktop_feature_fingerprints
                            .entry(feature_fingerprint)
                            .or_default()
                            .push(DesktopFeatureFingerprintObservation {
                                path: path.clone(),
                                cargo_hash: fingerprint_cargo_hash(fingerprint)
                                    .expect("validated product fingerprint directory")
                                    .to_owned(),
                                metadata,
                            });
                    }
                    if let Some(binary_artifact) =
                        classify_product_binary_fingerprint(file_name, package)
                    {
                        product_binary_artifacts
                            .entry(binary_artifact)
                            .or_default()
                            .push(ProductBinaryArtifactObservation {
                                path: path.clone(),
                                cargo_hash: Some(
                                    fingerprint_cargo_hash(fingerprint)
                                        .expect("validated product fingerprint directory")
                                        .to_owned(),
                                ),
                            });
                    }
                }
                Some(_) => {
                    unknown_artifacts += 1;
                    violations.push(build_violation(
                        "RPE-PURITY-0108",
                        &path,
                        "unexpected-fingerprint-artifact",
                    ));
                }
                None => match third_party_package_from_fingerprint(fingerprint) {
                    Some(package)
                        if allowed_third_party_fingerprint_file(file_name, package.crate_name) =>
                    {
                        validate_third_party_artifact_size(&path, package, &mut violations);
                        observed_third_party_packages.insert(package.package_name);
                    }
                    Some(_) => {
                        unknown_artifacts += 1;
                        violations.push(build_violation(
                            "RPE-PURITY-0108",
                            &path,
                            "unexpected-third-party-fingerprint-artifact",
                        ));
                    }
                    None => violations.push(build_violation(
                        "RPE-PURITY-0105",
                        &path,
                        "unknown-fingerprint-package",
                    )),
                },
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
    for required in ProductBinaryArtifact::ALL {
        match product_binary_artifacts.get(&required).map(Vec::len) {
            None | Some(0) => violations.push(build_violation(
                "RPE-PURITY-0109",
                &target.join("release"),
                &format!("missing-product-binary-artifact={}", required.label()),
            )),
            Some(1) => {}
            Some(count) => violations.push(build_violation(
                "RPE-PURITY-0113",
                &target.join("release"),
                &format!(
                    "duplicate-product-binary-artifact={}:count={count}",
                    required.label()
                ),
            )),
        }
    }
    for required in DesktopFeatureFingerprint::ALL {
        match desktop_feature_fingerprints.get(&required).map(Vec::len) {
            None | Some(0) => violations.push(build_violation(
                "RPE-PURITY-0112",
                &target.join("release/.fingerprint"),
                &format!(
                    "missing-desktop-product-feature-fingerprint={}",
                    required.label()
                ),
            )),
            Some(1) => {}
            Some(count) => violations.push(build_violation(
                "RPE-PURITY-0112",
                &target.join("release/.fingerprint"),
                &format!(
                    "duplicate-desktop-product-feature-fingerprint={}:count={count}",
                    required.label()
                ),
            )),
        }
    }
    validate_desktop_fingerprint_directories(
        &desktop_fingerprint_directories,
        &desktop_feature_fingerprints,
        target,
        &mut violations,
    );
    validate_desktop_fingerprint_pair(&desktop_feature_fingerprints, &mut violations);
    let desktop_worker_sha256 = validate_desktop_worker_artifact_association(
        &product_binary_artifacts,
        &desktop_feature_fingerprints,
        &mut violations,
    );
    let expected_third_party: Vec<_> = expected_third_party_packages().collect();
    let expected_third_party_names: BTreeSet<_> = expected_third_party
        .iter()
        .map(|package| package.package_name)
        .collect();
    for observed in &observed_third_party_packages {
        if !expected_third_party_names.contains(observed) {
            violations.push(build_violation(
                "RPE-PURITY-0110",
                &target.join("release"),
                &format!("unexpected-target-third-party={observed}"),
            ));
        }
    }
    for package in expected_third_party {
        if !third_party_depfile_crates.contains(package.crate_name) {
            violations.push(build_violation(
                "RPE-PURITY-0110",
                &target.join("release/deps"),
                &format!("missing-third-party-depfile={}", package.crate_name),
            ));
        }
        if !third_party_artifact_crates.contains(package.crate_name) {
            violations.push(build_violation(
                "RPE-PURITY-0110",
                &target.join("release/deps"),
                &format!("missing-third-party-artifact={}", package.crate_name),
            ));
        }
        if !third_party_fingerprint_packages.contains(package.package_name) {
            violations.push(build_violation(
                "RPE-PURITY-0110",
                &target.join("release/.fingerprint"),
                &format!("missing-third-party-fingerprint={}", package.package_name),
            ));
        }
        if package.allows_build_script && !third_party_build_packages.contains(package.package_name)
        {
            violations.push(build_violation(
                "RPE-PURITY-0110",
                &target.join("release/build"),
                &format!("missing-third-party-build-script={}", package.package_name),
            ));
        }
    }

    if violations.is_empty() {
        Ok(ProductBuildClosureReport {
            product_packages: PRODUCT_PACKAGES.len(),
            product_binaries: 1,
            desktop_feature_fingerprints: desktop_feature_fingerprints.values().map(Vec::len).sum(),
            desktop_fingerprint_directories: desktop_fingerprint_directories.len(),
            desktop_worker_sha256: desktop_worker_sha256
                .expect("valid desktop closure has a bound worker digest"),
            registered_third_party_packages: observed_third_party_packages.len(),
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

fn validate_third_party_depfile(
    path: &Path,
    package: &ThirdPartyProductPackage,
    violations: &mut Vec<BuildClosureViolation>,
) {
    let input = match fs::read_to_string(path) {
        Ok(input) if input.len() <= 16 * 1024 * 1024 => input,
        Ok(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0111",
                path,
                "third-party-depfile-hard-byte-cap",
            ));
            return;
        }
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0101",
                path,
                "unreadable-third-party-depfile",
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
            "third-party-depfile-to-tools",
        ));
    }
    let source_marker = format!("/{0}-{1}/", package.package_name, package.version);
    let mut saw_own_source = false;
    for token in input.split_whitespace() {
        let token = token.trim_end_matches(['\\', ':']).replace('\\', "/");
        if !token.ends_with(".rs") {
            continue;
        }
        if !token.contains("/registry/src/") || !token.contains(&source_marker) {
            violations.push(build_violation(
                "RPE-PURITY-0105",
                path,
                "third-party-source-outside-registered-crate",
            ));
        } else {
            saw_own_source = true;
        }
    }
    if !saw_own_source {
        violations.push(build_violation(
            "RPE-PURITY-0110",
            path,
            "third-party-depfile-missing-own-source",
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DesktopFeatureFingerprint {
    Library,
    WorkerBinary,
}

impl DesktopFeatureFingerprint {
    const ALL: [Self; 2] = [Self::Library, Self::WorkerBinary];

    const fn label(self) -> &'static str {
        match self {
            Self::Library => "desktop-library",
            Self::WorkerBinary => "desktop-worker",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopFeatureFingerprintMetadata {
    rustc: u64,
    profile: u64,
    target: u64,
    path: u64,
    config: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopFeatureFingerprintObservation {
    path: PathBuf,
    cargo_hash: String,
    metadata: Option<DesktopFeatureFingerprintMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ProductBinaryArtifact {
    TopLevelExecutable,
    TopLevelDepfile,
    HashedExecutable,
    HashedDepfile,
    FingerprintExecutable,
    FingerprintJson,
    FingerprintDependency,
}

impl ProductBinaryArtifact {
    const ALL: [Self; 7] = [
        Self::TopLevelExecutable,
        Self::TopLevelDepfile,
        Self::HashedExecutable,
        Self::HashedDepfile,
        Self::FingerprintExecutable,
        Self::FingerprintJson,
        Self::FingerprintDependency,
    ];

    const fn is_depfile(self) -> bool {
        matches!(self, Self::TopLevelDepfile | Self::HashedDepfile)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::TopLevelExecutable => "desktop-worker",
            Self::TopLevelDepfile => "desktop-worker-depfile",
            Self::HashedExecutable => "desktop-worker-hashed",
            Self::HashedDepfile => "desktop-worker-hashed-depfile",
            Self::FingerprintExecutable => "desktop-worker-fingerprint",
            Self::FingerprintJson => "desktop-worker-fingerprint-json",
            Self::FingerprintDependency => "desktop-worker-fingerprint-dependency",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProductBinaryArtifactObservation {
    path: PathBuf,
    cargo_hash: Option<String>,
}

fn classify_desktop_feature_fingerprint(
    file: &str,
    package: &ProductPackage,
) -> Option<DesktopFeatureFingerprint> {
    if package.package_name != "pdf-rs-desktop" {
        return None;
    }
    match file {
        "lib-pdf_rs_desktop.json" => Some(DesktopFeatureFingerprint::Library),
        "bin-pdf-rs-desktop-worker.json" => Some(DesktopFeatureFingerprint::WorkerBinary),
        _ => None,
    }
}

fn validate_desktop_feature_fingerprint(
    path: &Path,
    fingerprint: DesktopFeatureFingerprint,
    violations: &mut Vec<BuildClosureViolation>,
) -> Option<DesktopFeatureFingerprintMetadata> {
    let input = match fs::read_to_string(path) {
        Ok(input) if input.len() <= 1024 * 1024 => input,
        Ok(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                path,
                "desktop-feature-fingerprint-hard-byte-cap",
            ));
            return None;
        }
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                path,
                "unreadable-desktop-feature-fingerprint",
            ));
            return None;
        }
    };

    match json_string_field(&input, "features") {
        Some(features) if features == "[]" => {}
        Some(features) => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-enabled-features={features}", fingerprint.label()),
        )),
        None => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-missing-enabled-features", fingerprint.label()),
        )),
    }
    match json_string_field(&input, "declared_features") {
        Some(features) if features == "[\"default\", \"transport-fixture\"]" => {}
        Some(features) => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-declared-feature-drift={features}", fingerprint.label()),
        )),
        None => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-missing-declared-features", fingerprint.label()),
        )),
    }

    let expected_dependencies = expected_desktop_fingerprint_dependencies(fingerprint);
    match json_cargo_dependency_names(&input, "deps") {
        Some(dependencies) if dependencies == expected_dependencies => {}
        Some(dependencies) => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!(
                "{}-dependency-drift={}",
                fingerprint.label(),
                dependencies.into_iter().collect::<Vec<_>>().join(",")
            ),
        )),
        None => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-missing-dependency-graph", fingerprint.label()),
        )),
    }

    match json_array_is_empty(&input, "rustflags") {
        Some(true) => {}
        Some(false) => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-nonempty-rustflags", fingerprint.label()),
        )),
        None => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-missing-or-malformed-rustflags", fingerprint.label()),
        )),
    }
    match json_u64_field(&input, "compile_kind") {
        Some(0) => {}
        Some(value) => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-non-native-compile-kind={value}", fingerprint.label()),
        )),
        None => violations.push(build_violation(
            "RPE-PURITY-0112",
            path,
            &format!("{}-missing-or-malformed-compile-kind", fingerprint.label()),
        )),
    }

    let rustc = required_nonzero_fingerprint_field(&input, path, fingerprint, "rustc", violations);
    let profile =
        required_nonzero_fingerprint_field(&input, path, fingerprint, "profile", violations);
    let target =
        required_nonzero_fingerprint_field(&input, path, fingerprint, "target", violations);
    let source_path =
        required_nonzero_fingerprint_field(&input, path, fingerprint, "path", violations);
    let config =
        required_nonzero_fingerprint_field(&input, path, fingerprint, "config", violations);
    Some(DesktopFeatureFingerprintMetadata {
        rustc: rustc?,
        profile: profile?,
        target: target?,
        path: source_path?,
        config: config?,
    })
}

fn required_nonzero_fingerprint_field(
    input: &str,
    path: &Path,
    fingerprint: DesktopFeatureFingerprint,
    field: &str,
    violations: &mut Vec<BuildClosureViolation>,
) -> Option<u64> {
    match json_u64_field(input, field) {
        Some(value) if value != 0 => Some(value),
        Some(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                path,
                &format!("{}-zero-{field}", fingerprint.label()),
            ));
            None
        }
        None => {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                path,
                &format!("{}-missing-or-malformed-{field}", fingerprint.label()),
            ));
            None
        }
    }
}

fn expected_desktop_fingerprint_dependencies(
    fingerprint: DesktopFeatureFingerprint,
) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::from([
        "pdf_rs_bytes".to_owned(),
        "pdf_rs_engine".to_owned(),
        "pdf_rs_fast_raster".to_owned(),
        "pdf_rs_policy".to_owned(),
        "pdf_rs_protocol".to_owned(),
        "pdf_rs_scene".to_owned(),
        "pdf_rs_surface".to_owned(),
        "pdf_rs_syntax".to_owned(),
        "rustix".to_owned(),
    ]);
    if cfg!(target_os = "macos") {
        dependencies.insert("pdf_rs_macos_spawn".to_owned());
    }
    if fingerprint == DesktopFeatureFingerprint::WorkerBinary {
        dependencies.insert("pdf_rs_desktop".to_owned());
    }
    dependencies
}

fn json_string_field(input: &str, field: &str) -> Option<String> {
    let start = json_field_value_start(input, field)?;
    let bytes = input.as_bytes();
    let (value, end) = parse_json_string(bytes, start)?;
    json_value_has_top_level_terminator(bytes, end).then_some(value)
}

fn json_cargo_dependency_names(input: &str, field: &str) -> Option<BTreeSet<String>> {
    let start = json_field_value_start(input, field)?;
    let bytes = input.as_bytes();
    let mut cursor = start;
    consume_json_byte(bytes, &mut cursor, b'[')?;
    skip_json_whitespace(bytes, &mut cursor);
    let mut values = BTreeSet::new();
    if bytes.get(cursor) == Some(&b']') {
        cursor += 1;
        return json_value_has_top_level_terminator(bytes, cursor).then_some(values);
    }
    loop {
        consume_json_byte(bytes, &mut cursor, b'[')?;
        parse_json_u64_at(bytes, &mut cursor)?;
        consume_json_byte(bytes, &mut cursor, b',')?;
        skip_json_whitespace(bytes, &mut cursor);
        let (dependency, next) = parse_json_string(bytes, cursor)?;
        cursor = next;
        if !values.insert(dependency) {
            return None;
        }
        consume_json_byte(bytes, &mut cursor, b',')?;
        consume_json_literal(bytes, &mut cursor, b"false")?;
        consume_json_byte(bytes, &mut cursor, b',')?;
        parse_json_u64_at(bytes, &mut cursor)?;
        consume_json_byte(bytes, &mut cursor, b']')?;
        skip_json_whitespace(bytes, &mut cursor);
        match bytes.get(cursor) {
            Some(b',') => {
                cursor += 1;
                skip_json_whitespace(bytes, &mut cursor);
            }
            Some(b']') => {
                cursor += 1;
                break;
            }
            _ => return None,
        }
    }
    json_value_has_top_level_terminator(bytes, cursor).then_some(values)
}

fn json_array_is_empty(input: &str, field: &str) -> Option<bool> {
    let start = json_field_value_start(input, field)?;
    let bytes = input.as_bytes();
    let mut cursor = start;
    consume_json_byte(bytes, &mut cursor, b'[')?;
    skip_json_whitespace(bytes, &mut cursor);
    if bytes.get(cursor) != Some(&b']') {
        return Some(false);
    }
    cursor += 1;
    json_value_has_top_level_terminator(bytes, cursor).then_some(true)
}

fn json_u64_field(input: &str, field: &str) -> Option<u64> {
    let start = json_field_value_start(input, field)?;
    let bytes = input.as_bytes();
    let mut cursor = start;
    let value = parse_json_u64_at(bytes, &mut cursor)?;
    json_value_has_top_level_terminator(bytes, cursor).then_some(value)
}

fn json_field_value_start(input: &str, field: &str) -> Option<usize> {
    let needle = format!("\"{field}\"");
    let mut matches = input.match_indices(&needle);
    let (first, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let bytes = input.as_bytes();
    let mut cursor = first + needle.len();
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b':') {
        return None;
    }
    cursor += 1;
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    Some(cursor)
}

fn parse_json_u64_at(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    skip_json_whitespace(bytes, cursor);
    let start = *cursor;
    while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
        *cursor += 1;
    }
    if *cursor == start {
        return None;
    }
    std::str::from_utf8(&bytes[start..*cursor])
        .ok()?
        .parse()
        .ok()
}

fn consume_json_byte(bytes: &[u8], cursor: &mut usize, expected: u8) -> Option<()> {
    skip_json_whitespace(bytes, cursor);
    if bytes.get(*cursor) != Some(&expected) {
        return None;
    }
    *cursor += 1;
    Some(())
}

fn consume_json_literal(bytes: &[u8], cursor: &mut usize, expected: &[u8]) -> Option<()> {
    skip_json_whitespace(bytes, cursor);
    if !bytes.get(*cursor..)?.starts_with(expected) {
        return None;
    }
    *cursor += expected.len();
    Some(())
}

fn skip_json_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn json_value_has_top_level_terminator(bytes: &[u8], mut cursor: usize) -> bool {
    skip_json_whitespace(bytes, &mut cursor);
    matches!(bytes.get(cursor), Some(b',' | b'}'))
}

fn parse_json_string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut output = String::new();
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => return Some((output, cursor + 1)),
            b'\\' => {
                cursor += 1;
                let escaped = match *bytes.get(cursor)? {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    _ => return None,
                };
                output.push(escaped);
            }
            byte if byte.is_ascii_control() || !byte.is_ascii() => return None,
            byte => output.push(char::from(byte)),
        }
        cursor += 1;
    }
    None
}

fn validate_desktop_fingerprint_directories(
    directories: &BTreeMap<String, PathBuf>,
    fingerprints: &BTreeMap<DesktopFeatureFingerprint, Vec<DesktopFeatureFingerprintObservation>>,
    target: &Path,
    violations: &mut Vec<BuildClosureViolation>,
) {
    if directories.len() != DesktopFeatureFingerprint::ALL.len() {
        violations.push(build_violation(
            "RPE-PURITY-0112",
            &target.join("release/.fingerprint"),
            &format!(
                "desktop-product-fingerprint-directory-count={}:expected={}",
                directories.len(),
                DesktopFeatureFingerprint::ALL.len()
            ),
        ));
    }
    let observed_unit_hashes = fingerprints
        .values()
        .filter(|observations| observations.len() == 1)
        .map(|observations| observations[0].cargo_hash.as_str())
        .collect::<BTreeSet<_>>();
    for (hash, path) in directories {
        if !observed_unit_hashes.contains(hash.as_str()) {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                path,
                "desktop-product-fingerprint-directory-without-feature-record",
            ));
        }
    }
}

fn validate_desktop_fingerprint_pair(
    fingerprints: &BTreeMap<DesktopFeatureFingerprint, Vec<DesktopFeatureFingerprintObservation>>,
    violations: &mut Vec<BuildClosureViolation>,
) {
    let Some(library) =
        single_desktop_feature_fingerprint(fingerprints, DesktopFeatureFingerprint::Library)
    else {
        return;
    };
    let Some(worker) =
        single_desktop_feature_fingerprint(fingerprints, DesktopFeatureFingerprint::WorkerBinary)
    else {
        return;
    };
    let (Some(library_metadata), Some(worker_metadata)) = (&library.metadata, &worker.metadata)
    else {
        return;
    };

    if library.cargo_hash == worker.cargo_hash {
        violations.push(build_violation(
            "RPE-PURITY-0112",
            &worker.path,
            "desktop-lib-worker-fingerprint-hash-collision",
        ));
    }
    for (field, same) in [
        ("rustc", library_metadata.rustc == worker_metadata.rustc),
        (
            "release-profile",
            library_metadata.profile == worker_metadata.profile,
        ),
        (
            "cargo-config",
            library_metadata.config == worker_metadata.config,
        ),
    ] {
        if !same {
            violations.push(build_violation(
                "RPE-PURITY-0112",
                &worker.path,
                &format!("desktop-lib-worker-{field}-mismatch"),
            ));
        }
    }
    if library_metadata.target == worker_metadata.target {
        violations.push(build_violation(
            "RPE-PURITY-0112",
            &worker.path,
            "desktop-lib-worker-target-identity-collision",
        ));
    }
    if library_metadata.path == worker_metadata.path {
        violations.push(build_violation(
            "RPE-PURITY-0112",
            &worker.path,
            "desktop-lib-worker-source-path-identity-collision",
        ));
    }
}

fn validate_desktop_worker_artifact_association(
    artifacts: &BTreeMap<ProductBinaryArtifact, Vec<ProductBinaryArtifactObservation>>,
    fingerprints: &BTreeMap<DesktopFeatureFingerprint, Vec<DesktopFeatureFingerprintObservation>>,
    violations: &mut Vec<BuildClosureViolation>,
) -> Option<String> {
    let worker_fingerprint =
        single_desktop_feature_fingerprint(fingerprints, DesktopFeatureFingerprint::WorkerBinary)?;
    let fingerprint_json =
        single_product_binary_artifact(artifacts, ProductBinaryArtifact::FingerprintJson)?;
    if fingerprint_json.path != worker_fingerprint.path {
        violations.push(build_violation(
            "RPE-PURITY-0113",
            &fingerprint_json.path,
            "desktop-worker-feature-fingerprint-path-mismatch",
        ));
    }

    for kind in [
        ProductBinaryArtifact::HashedExecutable,
        ProductBinaryArtifact::HashedDepfile,
        ProductBinaryArtifact::FingerprintExecutable,
        ProductBinaryArtifact::FingerprintJson,
        ProductBinaryArtifact::FingerprintDependency,
    ] {
        let Some(observation) = single_product_binary_artifact(artifacts, kind) else {
            continue;
        };
        if observation.cargo_hash.as_deref() != Some(worker_fingerprint.cargo_hash.as_str()) {
            violations.push(build_violation(
                "RPE-PURITY-0113",
                &observation.path,
                &format!("desktop-worker-cargo-hash-mismatch={}", kind.label()),
            ));
        }
    }

    let top_level =
        single_product_binary_artifact(artifacts, ProductBinaryArtifact::TopLevelExecutable)?;
    let hashed =
        single_product_binary_artifact(artifacts, ProductBinaryArtifact::HashedExecutable)?;
    let top_level_inspection = match inspect_desktop_worker(&top_level.path) {
        Ok(inspection) => inspection,
        Err(token) => {
            violations.push(build_violation("RPE-PURITY-0113", &top_level.path, token));
            return None;
        }
    };
    let hashed_inspection = match inspect_desktop_worker(&hashed.path) {
        Ok(inspection) => inspection,
        Err(token) => {
            violations.push(build_violation("RPE-PURITY-0113", &hashed.path, token));
            return None;
        }
    };
    validate_desktop_worker_feature_marker(
        top_level,
        ProductBinaryArtifact::TopLevelExecutable,
        &top_level_inspection,
        violations,
    );
    validate_desktop_worker_feature_marker(
        hashed,
        ProductBinaryArtifact::HashedExecutable,
        &hashed_inspection,
        violations,
    );
    if top_level_inspection.sha256 != hashed_inspection.sha256 {
        violations.push(build_violation(
            "RPE-PURITY-0113",
            &top_level.path,
            "desktop-worker-top-level-hash-mismatch",
        ));
        return None;
    }
    Some(top_level_inspection.sha256)
}

fn single_desktop_feature_fingerprint(
    fingerprints: &BTreeMap<DesktopFeatureFingerprint, Vec<DesktopFeatureFingerprintObservation>>,
    kind: DesktopFeatureFingerprint,
) -> Option<&DesktopFeatureFingerprintObservation> {
    let observations = fingerprints.get(&kind)?;
    (observations.len() == 1).then(|| &observations[0])
}

fn single_product_binary_artifact(
    artifacts: &BTreeMap<ProductBinaryArtifact, Vec<ProductBinaryArtifactObservation>>,
    kind: ProductBinaryArtifact,
) -> Option<&ProductBinaryArtifactObservation> {
    let observations = artifacts.get(&kind)?;
    (observations.len() == 1).then(|| &observations[0])
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DesktopWorkerBinaryInspection {
    sha256: String,
    has_product_feature_marker: bool,
    has_fixture_feature_marker: bool,
}

fn inspect_desktop_worker(path: &Path) -> Result<DesktopWorkerBinaryInspection, &'static str> {
    let metadata =
        fs::metadata(path).map_err(|_| "unreadable-desktop-worker-for-feature-inspection")?;
    if metadata.len() > DESKTOP_WORKER_MAX_BYTES {
        return Err("desktop-worker-feature-inspection-hard-byte-cap");
    }
    let file = File::open(path).map_err(|_| "unreadable-desktop-worker-for-feature-inspection")?;
    let mut input = Vec::with_capacity(
        usize::try_from(metadata.len())
            .map_err(|_| "desktop-worker-feature-inspection-hard-byte-cap")?,
    );
    file.take(DESKTOP_WORKER_MAX_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|_| "unreadable-desktop-worker-for-feature-inspection")?;
    let input_len = u64::try_from(input.len())
        .map_err(|_| "desktop-worker-feature-inspection-hard-byte-cap")?;
    if input_len > DESKTOP_WORKER_MAX_BYTES {
        return Err("desktop-worker-feature-inspection-hard-byte-cap");
    }
    let digest =
        pdf_rs_digest::sha256(&input).map_err(|_| "desktop-worker-hash-length-overflow")?;
    Ok(DesktopWorkerBinaryInspection {
        sha256: pdf_rs_digest::hex_digest(&digest),
        has_product_feature_marker: input
            .windows(DESKTOP_WORKER_PRODUCT_FEATURE_MARKER.len())
            .any(|window| window == DESKTOP_WORKER_PRODUCT_FEATURE_MARKER),
        has_fixture_feature_marker: input
            .windows(DESKTOP_WORKER_FIXTURE_FEATURE_MARKER.len())
            .any(|window| window == DESKTOP_WORKER_FIXTURE_FEATURE_MARKER),
    })
}

fn validate_desktop_worker_feature_marker(
    artifact: &ProductBinaryArtifactObservation,
    kind: ProductBinaryArtifact,
    inspection: &DesktopWorkerBinaryInspection,
    violations: &mut Vec<BuildClosureViolation>,
) {
    if !inspection.has_product_feature_marker {
        violations.push(build_violation(
            "RPE-PURITY-0113",
            &artifact.path,
            &format!(
                "desktop-worker-missing-product-feature-marker={}",
                kind.label()
            ),
        ));
    }
    if inspection.has_fixture_feature_marker {
        violations.push(build_violation(
            "RPE-PURITY-0113",
            &artifact.path,
            &format!(
                "desktop-worker-forbidden-fixture-feature-marker={}",
                kind.label()
            ),
        ));
    }
}

fn classify_product_binary_artifact(
    relative: &str,
) -> Option<(ProductBinaryArtifact, Option<&str>)> {
    let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
    if relative == format!("release/{DESKTOP_WORKER_BINARY}{executable_suffix}") {
        return Some((ProductBinaryArtifact::TopLevelExecutable, None));
    }
    if relative == format!("release/{DESKTOP_WORKER_BINARY}.d") {
        return Some((ProductBinaryArtifact::TopLevelDepfile, None));
    }

    let file = relative.strip_prefix("release/deps/")?;
    if let Some(stem) = file.strip_suffix(".d") {
        let (name, hash) = split_cargo_hash(stem)?;
        return (name == DESKTOP_WORKER_CRATE)
            .then_some((ProductBinaryArtifact::HashedDepfile, Some(hash)));
    }
    let stem = if cfg!(windows) {
        file.strip_suffix(".exe")?
    } else {
        file
    };
    let (name, hash) = split_cargo_hash(stem)?;
    (name == DESKTOP_WORKER_CRATE).then_some((ProductBinaryArtifact::HashedExecutable, Some(hash)))
}

fn classify_product_binary_fingerprint(
    file: &str,
    package: &ProductPackage,
) -> Option<ProductBinaryArtifact> {
    if package.package_name != "pdf-rs-desktop" {
        return None;
    }
    match file {
        "bin-pdf-rs-desktop-worker" => Some(ProductBinaryArtifact::FingerprintExecutable),
        "bin-pdf-rs-desktop-worker.json" => Some(ProductBinaryArtifact::FingerprintJson),
        "dep-bin-pdf-rs-desktop-worker" => Some(ProductBinaryArtifact::FingerprintDependency),
        _ => None,
    }
}

fn classify_top_level_rust_artifact(relative: &str) -> Option<&str> {
    let file = relative.strip_prefix("release/lib")?;
    file.strip_suffix(".rlib")
        .or_else(|| file.strip_suffix(".d"))
}

fn strip_cargo_hash(stem: &str) -> Option<&str> {
    split_cargo_hash(stem).map(|(name, _)| name)
}

fn split_cargo_hash(stem: &str) -> Option<(&str, &str)> {
    let (name, hash) = stem.rsplit_once('-')?;
    (hash.len() == CARGO_HASH_LENGTH
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some((name, hash))
}

fn fingerprint_cargo_hash(value: &str) -> Option<&str> {
    split_cargo_hash(value).map(|(_, hash)| hash)
}

fn package_from_fingerprint(value: &str) -> Option<&'static ProductPackage> {
    let (package, _) = split_cargo_hash(value)?;
    PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.package_name == package)
}

fn package_by_crate(crate_name: &str) -> Option<&'static ProductPackage> {
    PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.crate_name == crate_name)
}

fn product_package_by_name(package_name: &str) -> Option<&'static ProductPackage> {
    PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.package_name == package_name)
}

fn third_party_package_from_fingerprint(value: &str) -> Option<&'static ThirdPartyProductPackage> {
    let (package, _) = split_cargo_hash(value)?;
    THIRD_PARTY_PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.package_name == package)
}

fn third_party_package_by_crate(crate_name: &str) -> Option<&'static ThirdPartyProductPackage> {
    THIRD_PARTY_PRODUCT_PACKAGES
        .iter()
        .find(|candidate| candidate.crate_name == crate_name)
}

fn expected_third_party_packages() -> impl Iterator<Item = &'static ThirdPartyProductPackage> {
    THIRD_PARTY_PRODUCT_PACKAGES
        .iter()
        .filter(|package| match package.package_name {
            "bitflags" | "errno" | "rustix" => true,
            "libc" => cfg!(unix),
            "linux-raw-sys" => cfg!(any(target_os = "linux", target_os = "android")),
            "windows-link" | "windows-sys" => cfg!(windows),
            _ => false,
        })
}

fn allowed_fingerprint_file(file: &str, crate_name: &str) -> bool {
    file == "invoked.timestamp"
        || file == format!("lib-{crate_name}")
        || file == format!("lib-{crate_name}.json")
        || file == format!("dep-lib-{crate_name}")
        || file == format!("output-lib-{crate_name}")
}

fn allowed_product_fingerprint_file(file: &str, package: &ProductPackage) -> bool {
    allowed_fingerprint_file(file, package.crate_name)
        || (package.package_name == "pdf-rs-desktop"
            && matches!(
                file,
                "bin-pdf-rs-desktop-worker"
                    | "bin-pdf-rs-desktop-worker.json"
                    | "dep-bin-pdf-rs-desktop-worker"
                    | "output-bin-pdf-rs-desktop-worker"
            ))
}

fn allowed_third_party_fingerprint_file(file: &str, crate_name: &str) -> bool {
    allowed_fingerprint_file(file, crate_name)
        || matches!(
            file,
            "build-script-build-script-build"
                | "build-script-build-script-build.json"
                | "dep-build-script-build-script-build"
                | "run-build-script-build-script-build"
                | "run-build-script-build-script-build.json"
        )
}

fn registered_build_directory(relative: &str) -> Option<&'static ThirdPartyProductPackage> {
    let rest = relative.strip_prefix("release/build/")?;
    let (fingerprint, child) = rest
        .split_once('/')
        .map_or((rest, None), |(fingerprint, child)| {
            (fingerprint, Some(child))
        });
    let package = third_party_package_from_fingerprint(fingerprint)?;
    if !package.allows_build_script || !matches!(child, None | Some("out")) {
        return None;
    }
    Some(package)
}

fn registered_build_artifact(relative: &str) -> Option<&'static ThirdPartyProductPackage> {
    let rest = relative.strip_prefix("release/build/")?;
    let (fingerprint, file) = rest.split_once('/')?;
    let package = third_party_package_from_fingerprint(fingerprint)?;
    if !package.allows_build_script {
        return None;
    }
    let executable_stem = file
        .strip_prefix("build_script_build-")
        .and_then(|value| value.strip_suffix(".exe").or(Some(value)))
        .and_then(|value| value.strip_suffix(".d").or(Some(value)));
    let executable_is_bounded_hash = executable_stem.is_some_and(|hash| {
        hash.len() == CARGO_HASH_LENGTH && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    let allowed = matches!(
        file,
        "build-script-build" | "invoked.timestamp" | "output" | "root-output" | "stderr"
    ) || executable_is_bounded_hash
        || (package.package_name == "rustix" && file == "out/rustix_test_can_compile");
    allowed.then_some(package)
}

fn validate_third_party_artifact_size(
    path: &Path,
    package: &ThirdPartyProductPackage,
    violations: &mut Vec<BuildClosureViolation>,
) {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() <= package.max_release_artifact_bytes => {}
        Ok(_) => violations.push(build_violation(
            "RPE-PURITY-0111",
            path,
            &format!("third-party-artifact-budget={}", package.package_name),
        )),
        Err(_) => violations.push(build_violation(
            "RPE-PURITY-0101",
            path,
            "unreadable-third-party-artifact",
        )),
    }
}

fn validate_product_binary_executable(path: &Path, violations: &mut Vec<BuildClosureViolation>) {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0101",
                path,
                "unreadable-product-binary",
            ));
            return;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            violations.push(build_violation(
                "RPE-PURITY-0109",
                path,
                "product-binary-is-not-executable",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
}

fn validate_registered_build_artifact_contents(
    path: &Path,
    violations: &mut Vec<BuildClosureViolation>,
) {
    let input = match fs::read(path) {
        Ok(input) if input.len() <= 32 * 1024 * 1024 => input,
        Ok(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0111",
                path,
                "registered-build-artifact-hard-byte-cap",
            ));
            return;
        }
        Err(_) => {
            violations.push(build_violation(
                "RPE-PURITY-0101",
                path,
                "unreadable-registered-build-artifact",
            ));
            return;
        }
    };
    let normalized = String::from_utf8_lossy(&input).to_ascii_lowercase();
    for engine in FORBIDDEN_ENGINES {
        if normalized.contains(engine) {
            violations.push(build_violation("RPE-PURITY-0104", path, engine));
        }
    }
    if normalized.contains("tools/") {
        violations.push(build_violation(
            "RPE-PURITY-0104",
            path,
            "registered-build-artifact-to-tools",
        ));
    }
}

fn is_native_artifact(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("a" | "so" | "dylib" | "dll" | "exe" | "lib" | "o" | "obj" | "wasm")
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
    const DESKTOP_LIBRARY_HASH: &str = "1111111111111111";
    const ALT_HASH: &str = "fedcba9876543210";

    #[test]
    fn ignores_tool_only_baseline_and_accepts_native_product_manifests() {
        assert_eq!(PRODUCT_PACKAGES.len(), PRODUCT_PACKAGE_COUNT);
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "pdf-rs/content/Cargo.toml",
            package_name: "pdf-rs-content",
            crate_name: "pdf_rs_content",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "pdf-rs/fast-raster/Cargo.toml",
            package_name: "pdf-rs-fast-raster",
            crate_name: "pdf_rs_fast_raster",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "pdf-rs/raster/Cargo.toml",
            package_name: "pdf-rs-raster",
            crate_name: "pdf_rs_raster",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "pdf-rs/font/Cargo.toml",
            package_name: "pdf-rs-font",
            crate_name: "pdf_rs_font",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "platform/browser/worker/Cargo.toml",
            package_name: "pdf-rs-browser-worker",
            crate_name: "pdf_rs_browser_worker",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "platform/desktop/Cargo.toml",
            package_name: "pdf-rs-desktop",
            crate_name: "pdf_rs_desktop",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "platform/electron-bridge/Cargo.toml",
            package_name: "pdf-rs-electron-bridge",
            crate_name: "pdf_rs_electron_bridge",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "platform/macos-spawn/Cargo.toml",
            package_name: "pdf-rs-macos-spawn",
            crate_name: "pdf_rs_macos_spawn",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/engine/Cargo.toml",
            package_name: "pdf-rs-engine",
            crate_name: "pdf_rs_engine",
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
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/tile-cache/Cargo.toml",
            package_name: "pdf-rs-tile-cache",
            crate_name: "pdf_rs_tile_cache",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/viewer/Cargo.toml",
            package_name: "pdf-rs-viewer",
            crate_name: "pdf_rs_viewer",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/scheduler/Cargo.toml",
            package_name: "pdf-rs-scheduler",
            crate_name: "pdf_rs_scheduler",
        }));
        assert!(PRODUCT_PACKAGES.contains(&ProductPackage {
            manifest: "runtime/surface/Cargo.toml",
            package_name: "pdf-rs-surface",
            crate_name: "pdf_rs_surface",
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
    fn rejects_third_party_registry_and_lock_drift() {
        let root = temp_dir("third-party-registry-drift");
        write_product_manifests(&root);
        fs::write(
            root.join(THIRD_PARTY_REGISTRY_PATH),
            format!("{THIRD_PARTY_REGISTRY_CONTENT}\n# unreviewed"),
        )
        .unwrap();
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0005" && value.token == "third-party-registry-drift"
        }));

        write_manifest(
            &root.join(THIRD_PARTY_REGISTRY_PATH),
            THIRD_PARTY_REGISTRY_CONTENT,
        );
        let lock_path = root.join("Cargo.lock");
        let lock = fs::read_to_string(&lock_path).unwrap().replace(
            THIRD_PARTY_PRODUCT_PACKAGES[0].checksum,
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );
        fs::write(&lock_path, lock).unwrap();
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0005"
                && value.token
                    == format!(
                        "third-party-lock-drift={}",
                        THIRD_PARTY_PRODUCT_PACKAGES[0].package_name
                    )
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_desktop_default_feature_drift() {
        let root = temp_dir("desktop-default-feature-drift");
        write_product_manifests(&root);
        let manifest = root.join(DESKTOP_MANIFEST_PATH);
        let input = fs::read_to_string(&manifest)
            .unwrap()
            .replace("default = []", "default = [\"transport-fixture\"]");
        fs::write(&manifest, input).unwrap();

        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0006"
                && violation.manifest == manifest
                && violation.token == "desktop-feature-table-drift"
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_desktop_product_feature_closure_declaration_drift() {
        let root = temp_dir("desktop-product-feature-declaration-drift");
        write_product_manifests(&root);
        let target = root.join(DESKTOP_SANDBOX_TARGET_PATH);
        let input = fs::read_to_string(&target).unwrap().replace(
            "signed_package_proof = false",
            "signed_package_proof = true",
        );
        fs::write(&target, input).unwrap();

        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0006"
                && violation.manifest == target
                && violation.token == "product-feature-closure-declaration-drift"
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
        let raster_manifest = root.join("pdf-rs/raster/Cargo.toml");
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
        assert_eq!(report.product_binaries, 1);
        assert_eq!(
            report.desktop_feature_fingerprints,
            DesktopFeatureFingerprint::ALL.len()
        );
        assert_eq!(
            report.desktop_fingerprint_directories,
            DesktopFeatureFingerprint::ALL.len()
        );
        assert_eq!(
            report.desktop_worker_sha256,
            pdf_rs_digest::hex_digest(
                &pdf_rs_digest::sha256(&synthetic_desktop_worker_bytes())
                    .expect("bounded synthetic worker")
            )
        );
        assert_eq!(
            report.registered_third_party_packages,
            expected_third_party_packages().count()
        );
        assert_eq!(report.depfiles, PRODUCT_PACKAGES.len());
        assert_eq!(report.fingerprint_directories, PRODUCT_PACKAGES.len());
        assert_eq!(
            report.build_script_artifacts,
            expected_third_party_packages()
                .filter(|package| package.allows_build_script)
                .count()
                * 2
        );
        assert_eq!(report.native_artifacts, 0);
        assert_eq!(report.unknown_artifacts, 0);

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_desktop_fingerprint_feature_and_declaration_drift() {
        let root = temp_dir("desktop-fingerprint-feature-drift-repository");
        let proof_root = temp_dir("desktop-fingerprint-feature-drift-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-feature-drift").unwrap();
        write_valid_build_inventory(&target);

        let library = target.join(format!(
            "release/.fingerprint/pdf-rs-desktop-{DESKTOP_LIBRARY_HASH}/lib-pdf_rs_desktop.json"
        ));
        let input = fs::read_to_string(&library).unwrap().replace(
            r#""declared_features":"[\"default\", \"transport-fixture\"]""#,
            r#""declared_features":"[]""#,
        );
        fs::write(&library, input).unwrap();
        let worker = target.join(format!(
            "release/.fingerprint/pdf-rs-desktop-{HASH}/bin-pdf-rs-desktop-worker.json"
        ));
        let input = fs::read_to_string(&worker).unwrap().replace(
            r#""features":"[]""#,
            r#""features":"[\"transport-fixture\"]""#,
        );
        fs::write(&worker, input).unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-feature-drift").unwrap_err();
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0112"
                && violation
                    .token
                    .starts_with("desktop-library-declared-feature-drift=")
        }));
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0112"
                && violation
                    .token
                    .starts_with("desktop-worker-enabled-features=")
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_desktop_fingerprint_dependency_rustflags_and_profile_drift() {
        let root = temp_dir("desktop-fingerprint-metadata-drift-repository");
        let proof_root = temp_dir("desktop-fingerprint-metadata-drift-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-metadata-drift").unwrap();
        write_valid_build_inventory(&target);

        let worker = target.join(format!(
            "release/.fingerprint/pdf-rs-desktop-{HASH}/bin-pdf-rs-desktop-worker.json"
        ));
        let input = fs::read_to_string(&worker)
            .unwrap()
            .replace(
                r#""deps":["#,
                r#""deps":[[999,"transport_fixture_launcher",false,999],"#,
            )
            .replace(
                r#""rustflags":[]"#,
                r#""rustflags":["--cfg","transport_fixture"]"#,
            )
            .replace(r#""profile":2"#, r#""profile":999"#);
        fs::write(&worker, input).unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-metadata-drift").unwrap_err();
        for token in [
            "desktop-worker-dependency-drift=",
            "desktop-worker-nonempty-rustflags",
            "desktop-lib-worker-release-profile-mismatch",
        ] {
            assert!(
                violations.iter().any(|violation| {
                    violation.code == "RPE-PURITY-0112"
                        && (violation.token == token || violation.token.starts_with(token))
                }),
                "missing desktop fingerprint mutation violation {token:?}: {violations:?}"
            );
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_duplicate_desktop_feature_fingerprint_configuration() {
        let root = temp_dir("duplicate-desktop-fingerprint-repository");
        let proof_root = temp_dir("duplicate-desktop-fingerprint-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-duplicate-fingerprint").unwrap();
        write_valid_build_inventory(&target);

        let duplicate = target.join(format!("release/.fingerprint/pdf-rs-desktop-{ALT_HASH}"));
        fs::create_dir_all(&duplicate).unwrap();
        fs::write(
            duplicate.join("lib-pdf_rs_desktop.json"),
            synthetic_desktop_fingerprint_json(DesktopFeatureFingerprint::Library),
        )
        .unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-duplicate-fingerprint").unwrap_err();
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0112"
                && violation.token
                    == "duplicate-desktop-product-feature-fingerprint=desktop-library:count=2"
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_desktop_fingerprint_directory_without_feature_record() {
        let root = temp_dir("orphan-desktop-fingerprint-directory-repository");
        let proof_root = temp_dir("orphan-desktop-fingerprint-directory-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-orphan-fingerprint-directory").unwrap();
        write_valid_build_inventory(&target);

        let orphan = target.join(format!("release/.fingerprint/pdf-rs-desktop-{ALT_HASH}"));
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("invoked.timestamp"), b"fresh").unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-orphan-fingerprint-directory")
                .unwrap_err();
        for token in [
            "desktop-product-fingerprint-directory-count=3:expected=2",
            "desktop-product-fingerprint-directory-without-feature-record",
        ] {
            assert!(
                violations.iter().any(|violation| {
                    violation.code == "RPE-PURITY-0112" && violation.token == token
                }),
                "missing orphan fingerprint directory violation {token:?}: {violations:?}"
            );
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_substituted_top_level_desktop_worker() {
        let root = temp_dir("substituted-desktop-worker-repository");
        let proof_root = temp_dir("substituted-desktop-worker-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-worker-substitution").unwrap();
        write_valid_build_inventory(&target);
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        fs::write(
            target.join(format!(
                "release/{DESKTOP_WORKER_BINARY}{executable_suffix}"
            )),
            b"substituted desktop worker",
        )
        .unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-worker-substitution").unwrap_err();
        assert!(violations.iter().any(|violation| {
            violation.code == "RPE-PURITY-0113"
                && violation.token == "desktop-worker-top-level-hash-mismatch"
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_paired_fixture_worker_substitution_even_when_hashes_match() {
        let root = temp_dir("paired-fixture-worker-substitution-repository");
        let proof_root = temp_dir("paired-fixture-worker-substitution-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-paired-worker-substitution").unwrap();
        write_valid_build_inventory(&target);
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        let mut fixture_worker = b"paired substituted worker\0".to_vec();
        fixture_worker.extend_from_slice(DESKTOP_WORKER_FIXTURE_FEATURE_MARKER);
        for worker in [
            target.join(format!(
                "release/{DESKTOP_WORKER_BINARY}{executable_suffix}"
            )),
            target.join(format!(
                "release/deps/{DESKTOP_WORKER_CRATE}-{HASH}{executable_suffix}"
            )),
        ] {
            fs::write(worker, &fixture_worker).unwrap();
        }

        let violations =
            check_product_build_closure(&root, &target, "proof-paired-worker-substitution")
                .unwrap_err();
        assert!(
            !violations.iter().any(|violation| {
                violation.code == "RPE-PURITY-0113"
                    && violation.token == "desktop-worker-top-level-hash-mismatch"
            }),
            "paired mutation must exercise marker rejection, not hash inequality: {violations:?}"
        );
        for artifact in ["desktop-worker", "desktop-worker-hashed"] {
            for token in [
                format!("desktop-worker-missing-product-feature-marker={artifact}"),
                format!("desktop-worker-forbidden-fixture-feature-marker={artifact}"),
            ] {
                assert!(
                    violations.iter().any(|violation| {
                        violation.code == "RPE-PURITY-0113" && violation.token == token
                    }),
                    "missing paired fixture substitution violation {token:?}: {violations:?}"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_desktop_worker_cargo_hash_suffix_drift() {
        let root = temp_dir("desktop-worker-hash-suffix-drift-repository");
        let proof_root = temp_dir("desktop-worker-hash-suffix-drift-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-worker-hash-suffix").unwrap();
        write_valid_build_inventory(&target);
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        for suffix in [executable_suffix, ".d"] {
            fs::rename(
                target.join(format!(
                    "release/deps/{DESKTOP_WORKER_CRATE}-{HASH}{suffix}"
                )),
                target.join(format!(
                    "release/deps/{DESKTOP_WORKER_CRATE}-{ALT_HASH}{suffix}"
                )),
            )
            .unwrap();
        }

        let violations =
            check_product_build_closure(&root, &target, "proof-worker-hash-suffix").unwrap_err();
        for artifact in ["desktop-worker-hashed", "desktop-worker-hashed-depfile"] {
            assert!(violations.iter().any(|violation| {
                violation.code == "RPE-PURITY-0113"
                    && violation.token == format!("desktop-worker-cargo-hash-mismatch={artifact}")
            }));
        }
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
    fn rejects_missing_desktop_worker_binary() {
        let root = temp_dir("missing-desktop-worker-repository");
        let proof_root = temp_dir("missing-desktop-worker-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-missing-desktop-worker").unwrap();
        write_valid_build_inventory(&target);
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        fs::remove_file(target.join(format!(
            "release/{DESKTOP_WORKER_BINARY}{executable_suffix}"
        )))
        .unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-missing-desktop-worker")
                .unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0109"
                && value.token == "missing-product-binary-artifact=desktop-worker"
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_unknown_product_executable() {
        let root = temp_dir("unknown-product-executable-repository");
        let proof_root = temp_dir("unknown-product-executable-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-unknown-product-executable").unwrap();
        write_valid_build_inventory(&target);
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        let unknown = target.join(format!("release/unregistered-worker{executable_suffix}"));
        fs::write(&unknown, b"unregistered executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&unknown, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let violations =
            check_product_build_closure(&root, &target, "proof-unknown-product-executable")
                .unwrap_err();
        assert!(violations.iter().any(|value| {
            if cfg!(windows) {
                value.code == "RPE-PURITY-0107" && value.token == "native-artifact"
            } else {
                value.code == "RPE-PURITY-0108" && value.token == "unexpected-build-artifact"
            }
        }));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(proof_root).unwrap();
    }

    #[test]
    fn rejects_registered_leaf_artifacts_for_the_wrong_build_target() {
        let root = temp_dir("wrong-target-leaf-repository");
        let proof_root = temp_dir("wrong-target-leaf-proof-root");
        let target = proof_root.join("target");
        write_product_manifests(&root);
        prepare_product_build_proof(&root, &target, "proof-wrong-target").unwrap();
        write_valid_build_inventory(&target);
        let expected: BTreeSet<_> = expected_third_party_packages()
            .map(|package| package.package_name)
            .collect();
        let foreign = THIRD_PARTY_PRODUCT_PACKAGES
            .iter()
            .find(|package| !expected.contains(package.package_name))
            .expect("registry includes at least one target-conditional foreign leaf");
        let registry_source = format!(
            "/synthetic/.cargo/registry/src/index/{0}-{1}/src/lib.rs",
            foreign.package_name, foreign.version
        );
        fs::write(
            target.join(format!("release/deps/{}-{HASH}.d", foreign.crate_name)),
            format!("target: {registry_source}\n"),
        )
        .unwrap();
        fs::write(
            target.join(format!(
                "release/deps/lib{}-{HASH}.rlib",
                foreign.crate_name
            )),
            b"rlib",
        )
        .unwrap();
        fs::write(
            target.join(format!(
                "release/deps/lib{}-{HASH}.rmeta",
                foreign.crate_name
            )),
            b"rmeta",
        )
        .unwrap();

        let violations =
            check_product_build_closure(&root, &target, "proof-wrong-target").unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0110"
                && value.token == format!("unexpected-target-third-party={}", foreign.package_name)
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
            let features = if package.package_name == "pdf-rs-desktop" {
                "\n[features]\ndefault = []\ntransport-fixture = []\n"
            } else {
                ""
            };
            write_manifest(
                &root.join(package.manifest),
                &format!("[package]\nname = \"{}\"\n{features}", package.package_name),
            );
        }
        write_manifest(
            &root.join(DESKTOP_SANDBOX_TARGET_PATH),
            DESKTOP_SANDBOX_TARGET_CONTENT,
        );
        write_manifest(
            &root.join(THIRD_PARTY_REGISTRY_PATH),
            THIRD_PARTY_REGISTRY_CONTENT,
        );
        let mut lock = String::from("# synthetic Cargo lock for purity tests\nversion = 4\n");
        for package in THIRD_PARTY_PRODUCT_PACKAGES {
            lock.push_str(&format!(
                "\n[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n",
                package.package_name, package.version, package.checksum
            ));
        }
        write_manifest(&root.join("Cargo.lock"), &lock);
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
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        let desktop_worker = target.join(format!(
            "release/{DESKTOP_WORKER_BINARY}{executable_suffix}"
        ));
        let desktop_worker_bytes = synthetic_desktop_worker_bytes();
        fs::write(&desktop_worker, &desktop_worker_bytes).unwrap();
        fs::write(
            target.join(format!("release/{DESKTOP_WORKER_BINARY}.d")),
            "target: platform/desktop/src/main.rs\n",
        )
        .unwrap();
        let hashed_desktop_worker = target.join(format!(
            "release/deps/{DESKTOP_WORKER_CRATE}-{HASH}{executable_suffix}"
        ));
        fs::write(&hashed_desktop_worker, &desktop_worker_bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            for executable in [&desktop_worker, &hashed_desktop_worker] {
                fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fs::write(
            target.join(format!("release/deps/{DESKTOP_WORKER_CRATE}-{HASH}.d")),
            "target: platform/desktop/src/main.rs\n",
        )
        .unwrap();
        for package in PRODUCT_PACKAGES {
            let package_hash = if package.package_name == "pdf-rs-desktop" {
                DESKTOP_LIBRARY_HASH
            } else {
                HASH
            };
            let source_root = Path::new(package.manifest).parent().unwrap();
            let source = source_root.join("src/lib.rs");
            let depfile = target.join(format!(
                "release/deps/{}-{package_hash}.d",
                package.crate_name
            ));
            fs::write(&depfile, format!("target: {}\n", source.display())).unwrap();
            fs::write(
                target.join(format!(
                    "release/deps/lib{}-{package_hash}.rlib",
                    package.crate_name
                )),
                b"rlib",
            )
            .unwrap();
            fs::write(
                target.join(format!(
                    "release/deps/lib{}-{package_hash}.rmeta",
                    package.crate_name
                )),
                b"rmeta",
            )
            .unwrap();
            let fingerprint = target.join(format!(
                "release/.fingerprint/{}-{package_hash}",
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
                if package.package_name == "pdf-rs-desktop" {
                    synthetic_desktop_fingerprint_json(DesktopFeatureFingerprint::Library)
                        .into_bytes()
                } else {
                    b"{}".to_vec()
                },
            )
            .unwrap();
        }
        let worker_fingerprint = target.join(format!("release/.fingerprint/pdf-rs-desktop-{HASH}"));
        fs::create_dir_all(&worker_fingerprint).unwrap();
        fs::write(worker_fingerprint.join("invoked.timestamp"), b"fresh").unwrap();
        fs::write(
            worker_fingerprint.join("bin-pdf-rs-desktop-worker"),
            b"fingerprint",
        )
        .unwrap();
        fs::write(
            worker_fingerprint.join("bin-pdf-rs-desktop-worker.json"),
            synthetic_desktop_fingerprint_json(DesktopFeatureFingerprint::WorkerBinary),
        )
        .unwrap();
        fs::write(
            worker_fingerprint.join("dep-bin-pdf-rs-desktop-worker"),
            b"fingerprint",
        )
        .unwrap();
        for package in expected_third_party_packages() {
            let registry_source = format!(
                "/synthetic/.cargo/registry/src/index/{0}-{1}/src/lib.rs",
                package.package_name, package.version
            );
            fs::write(
                target.join(format!("release/deps/{}-{HASH}.d", package.crate_name)),
                format!("target: {registry_source}\n"),
            )
            .unwrap();
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
            if package.allows_build_script {
                let build = target.join(format!("release/build/{}-{HASH}", package.package_name));
                fs::create_dir_all(&build).unwrap();
                fs::write(build.join("invoked.timestamp"), b"fresh").unwrap();
            }
        }
    }

    fn synthetic_desktop_fingerprint_json(fingerprint: DesktopFeatureFingerprint) -> String {
        let dependencies = expected_desktop_fingerprint_dependencies(fingerprint)
            .into_iter()
            .enumerate()
            .map(|(index, dependency)| {
                let identity = index + 1;
                format!("[{identity},\"{dependency}\",false,{identity}]")
            })
            .collect::<Vec<_>>()
            .join(",");
        let (target, path) = match fingerprint {
            DesktopFeatureFingerprint::Library => (101_u64, 201_u64),
            DesktopFeatureFingerprint::WorkerBinary => (102_u64, 202_u64),
        };
        format!(
            "{{\"rustc\":1,\"features\":\"[]\",\"declared_features\":\"[\\\"default\\\", \\\"transport-fixture\\\"]\",\"target\":{target},\"profile\":2,\"path\":{path},\"deps\":[{dependencies}],\"local\":[],\"rustflags\":[],\"config\":3,\"compile_kind\":0}}"
        )
    }

    fn synthetic_desktop_worker_bytes() -> Vec<u8> {
        let mut worker = b"synthetic desktop worker\0".to_vec();
        worker.extend_from_slice(DESKTOP_WORKER_PRODUCT_FEATURE_MARKER);
        worker
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
