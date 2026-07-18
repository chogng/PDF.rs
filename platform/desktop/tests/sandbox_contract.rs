use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use pdf_rs_desktop::{
    DESKTOP_PRODUCT_SANDBOX_TARGET_ID, DesktopChildSupervisor, DesktopIpcErrorCode,
    DesktopProductSandboxAvailability, DesktopSupervisorConfig,
    desktop_product_sandbox_availability,
};

const HOST_ENTITLEMENTS: &str = include_str!("../macos/host.entitlements");
const WORKER_ENTITLEMENTS: &str = include_str!("../macos/worker.entitlements");
const TARGET: &str = include_str!("../macos/sandbox-target.toml");
const M4_PLAN: &str = include_str!("../../../plan/m4.toml");
const CI: &str = include_str!("../../../scripts/ci.sh");
const WORKER_ENTRY: &str = include_str!("../src/main.rs");

const EXPECTED_HOST_ENTITLEMENTS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
    "\"https://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    "<plist version=\"1.0\">\n",
    "<dict>\n",
    "    <key>com.apple.security.app-sandbox</key>\n",
    "    <true/>\n",
    "    <key>com.apple.security.files.user-selected.read-only</key>\n",
    "    <true/>\n",
    "</dict>\n",
    "</plist>\n",
);

const EXPECTED_WORKER_ENTITLEMENTS: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
    "\"https://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    "<plist version=\"1.0\">\n",
    "<dict>\n",
    "    <key>com.apple.security.app-sandbox</key>\n",
    "    <true/>\n",
    "    <key>com.apple.security.inherit</key>\n",
    "    <true/>\n",
    "</dict>\n",
    "</plist>\n",
);

fn assert_unique_assignment(document: &str, expected: &str) {
    let key = expected
        .split_once('=')
        .expect("target scalar assignment")
        .0
        .trim();
    let assignments = document
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| {
            line.split_once('=')
                .is_some_and(|(candidate, _)| candidate.trim() == key)
        })
        .collect::<Vec<_>>();
    assert_eq!(assignments, [expected], "target assignment for {key}");
}

struct ProgramSentinel {
    consumed: Arc<AtomicBool>,
}

impl From<ProgramSentinel> for String {
    fn from(sentinel: ProgramSentinel) -> Self {
        sentinel.consumed.store(true, Ordering::SeqCst);
        "/path/that-must-not-be-launched".to_owned()
    }
}

#[test]
fn selected_target_and_entitlements_are_exact_and_narrow() {
    assert_eq!(HOST_ENTITLEMENTS, EXPECTED_HOST_ENTITLEMENTS);
    assert_eq!(WORKER_ENTITLEMENTS, EXPECTED_WORKER_ENTITLEMENTS);
    assert_unique_assignment(
        TARGET,
        &format!("id = \"{DESKTOP_PRODUCT_SANDBOX_TARGET_ID}\""),
    );
    for exact in [
        "selected = true",
        "status = \"prerequisite_only\"",
        "release_eligible = false",
        "m4_09_status = \"in_progress\"",
        "network = \"none\"",
        "broad_filesystem = \"none\"",
        "app_groups = \"none\"",
        "transport_fixture_feature = \"transport-fixture\"",
        "transport_fixture_feature_default = false",
        "transport_fixture_feature_product_allowed = false",
        "powerbox_extension_to_worker = false",
        "worker_network_entitlement = false",
        "sandbox_tmpdir_fallback_required_when_posix_shm_is_denied = true",
        "read_only_reopen_required = true",
        "unlink_before_scm_rights_required = true",
        "worker_exit_zero_residual_objects_required = true",
        "app_group_shm_exception_allowed = false",
        "implemented = true",
        "wrapper_crate = \"platform/macos-spawn\"",
        "wrapper_api = \"spawn_desktop_worker\"",
        "real_probe = \"platform/macos-spawn/tests/darwin_spawn.rs\"",
        "child_lifecycle = \"owned positive PID with cached ExitStatus; rustix try_wait, SIGKILL, and wait; no implicit wrapper Drop reap; desktop post-spawn cleanup and final Host/Supervisor Drop abort if reap cannot be proven\"",
        "caller_attestation_accepted = false",
        "environment_attestation_accepted = false",
        "transport_fixture_is_product = false",
        "closure_scope = \"package_prerequisite\"",
        "prerequisite_complete = true",
        "cargo_package = \"pdf-rs-desktop\"",
        "cargo_binary = \"pdf-rs-desktop-worker\"",
        "release_profile = true",
        "default_features_enabled = false",
        "forbidden_features = [\"transport-fixture\"]",
        "required_fingerprint_features = \"[]\"",
        "required_declared_features = [\"default\", \"transport-fixture\"]",
        "fresh_external_target_required = true",
        "required_desktop_fingerprint_directories = 2",
        "single_library_fingerprint_required = true",
        "single_worker_fingerprint_required = true",
        "worker_feature_marker_required = \"no_default_features_v1\"",
        "worker_fixture_marker_forbidden = \"transport_fixture_v1\"",
        "worker_cargo_filename_association_required = true",
        "matching_worker_sha256_observation_required = true",
        "worker_content_provenance_proved = false",
        "fixture_launch_api_product_visible = false",
        "product_package_produced = false",
        "universal_binary_proof = false",
        "signed_package_proof = false",
        "package_verifier_implemented = true",
        "package_verifier_cli = \"pdf-rs-quality verify-macos-package ROOT PDF.rs.app\"",
        "package_approval_record = \"platform/desktop/macos/package-approval.toml\"",
        "package_approval_scope = \"external_release_trust_anchor\"",
        "package_approval_present_by_default = false",
        "package_bundle_name = \"PDF.rs.app\"",
        "package_host_executable = \"Contents/MacOS/PDF.rs\"",
        "package_worker_helper = \"Contents/Helpers/pdf-rs-desktop-worker\"",
        "package_host_identifier = \"rs.pdf.desktop\"",
        "package_worker_identifier = \"rs.pdf.desktop.worker\"",
        "package_exact_directories = 4",
        "package_exact_files = 4",
        "package_exact_executables = 2",
        "package_system_inspections = 18",
        "package_codesign_all_architectures = true",
        "package_slice_signing_metadata_exact = true",
        "package_slice_entitlements_exact = true",
        "package_slice_dependencies_exact = true",
        "package_architectures_exact = [\"arm64\", \"x86_64\"]",
        "package_tree_hash_external_anchor_required = true",
        "package_worker_hash_external_anchor_required = true",
        "package_inspection_snapshot_rechecked = true",
        "package_symlinks_hardlinks_special_files_allowed = false",
        "package_unknown_executables_allowed = false",
        "package_content_provenance_proved = false",
        "package_release_evidence_present = false",
        "package_verifier_scope = \"verification_capability_not_release_evidence\"",
    ] {
        assert_unique_assignment(TARGET, exact);
    }
    assert!(TARGET.contains(
        "an externally approved package-approval.toml plus an actual signed universal package passing the repository verifier"
    ));
}

#[test]
fn product_release_commands_disable_default_features_explicitly() {
    let release_closure = CI
        .split("prepare-product-build-proof")
        .nth(1)
        .expect("product proof preparation")
        .split("check-product-build-closure")
        .next()
        .expect("product closure check");
    let build_marker = "CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=\"$product_target\" cargo build \\\n";
    let build_blocks = release_closure
        .split(build_marker)
        .skip(1)
        .map(|block| {
            block
                .split(build_marker)
                .next()
                .expect("bounded product build block")
        })
        .collect::<Vec<_>>();
    assert_eq!(build_blocks.len(), 2, "fresh product release build count");
    for block in build_blocks {
        assert!(
            block.contains("    --no-default-features \\\n"),
            "product release build did not disable default features: {block}"
        );
    }
    assert_eq!(
        CI.matches("cargo test --locked --doc --package pdf-rs-desktop --no-default-features")
            .count(),
        1,
        "default product API compile-fail gate"
    );
}

#[test]
fn macos_signal_restore_remains_before_feature_marker_observation() {
    let restore = WORKER_ENTRY
        .find("pdf_rs_macos_spawn::restore_desktop_worker_signal_state()")
        .expect("macOS worker signal restore hook");
    let marker = WORKER_ENTRY
        .find("std::hint::black_box(DESKTOP_WORKER_FEATURE_CLOSURE_MARKER)")
        .expect("worker feature marker observation");
    assert!(
        restore < marker,
        "macOS signal restore must remain the worker's first safe entry hook"
    );
}

#[test]
fn product_supervisor_fails_closed_without_packaging_attestation() {
    let expected = if cfg!(target_os = "macos") {
        DesktopProductSandboxAvailability::PackagingProofRequired
    } else {
        DesktopProductSandboxAvailability::UnsupportedTarget
    };
    assert_eq!(desktop_product_sandbox_availability(), expected);

    let consumed = Arc::new(AtomicBool::new(false));

    let failure = DesktopChildSupervisor::start_product_macos(
        ProgramSentinel {
            consumed: Arc::clone(&consumed),
        },
        DesktopSupervisorConfig::default(),
        (),
    )
    .err()
    .expect("unsigned workspace must fail before spawning");
    assert_eq!(failure.code(), DesktopIpcErrorCode::IsolationUnavailable);
    assert!(
        !consumed.load(Ordering::SeqCst),
        "product gate consumed the program before attestation"
    );
}

#[test]
fn plan_keeps_isolation_open_and_transport_fixture_explicit() {
    let m4_09 = M4_PLAN
        .split("[[work_item]]")
        .find(|item| item.contains("id = \"M4-09\""))
        .expect("M4-09 work item");
    assert!(m4_09.contains("status = \"in_progress\""));
    assert!(
        m4_09.contains("selected_target_record = \"platform/desktop/macos/sandbox-target.toml\"")
    );
    assert!(m4_09.contains("signed parent app"));
    assert!(m4_09.contains("fresh external release graph with --no-default-features"));
    assert!(m4_09.contains("This does not prove adversarial content provenance"));
    assert!(m4_09.contains("is not signed, universal, or packaged-app evidence"));
    assert!(m4_09.contains("signed universal product package"));
    assert!(m4_09.contains("package-approval.toml"));
    assert!(m4_09.contains("verification capability only"));
    assert!(m4_09.contains("complete package hash remain unproved release evidence"));
    assert!(m4_09.contains("Darwin default-close spawn file actions"));
    assert!(m4_09.contains("real inherited-FD allowlist probe"));
    assert!(m4_09.contains("safe desktop crate retains forbid(unsafe_code)"));
    assert!(m4_09.contains("launch identities are consumed before attempts"));
    assert!(m4_09.contains("Drop cleanup aborts instead of discarding child ownership"));
    assert!(m4_09.contains("inherited sandbox TMPDIR fallback"));
    assert!(m4_09.contains("zero residual objects without app-group widening"));

    for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
        .expect("desktop product sources")
    {
        let path = entry.expect("source entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read product source");
        for forbidden in ["sandbox-exec", "sandbox_init", "seatbelt"] {
            assert!(
                !source.to_ascii_lowercase().contains(forbidden),
                "{} imports prohibited sandbox mechanism {forbidden}",
                path.display()
            );
        }
    }
}
