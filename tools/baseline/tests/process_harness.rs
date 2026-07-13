use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineErrorCode, BaselineRequest, BaselineRunner,
    OracleAuthority, ProcessBaselineRunner, ProcessLimits, ProcessSpec,
};
use pdf_rs_digest::sha256;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-baseline-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn observes_produced_and_explicitly_unavailable_channels_as_o4() {
    let directory = TestDirectory::new("channels");
    let limits = ordinary_limits();
    let (runner, descriptor) = build_runner(&directory, &["ok"], &[], limits);
    let request = request(32);
    let observation = runner.observe(&request).unwrap();
    assert_eq!(observation.authority(), OracleAuthority::O4Observation);
    assert_eq!(observation.descriptor, descriptor);
    assert_eq!(
        observation.parse_json,
        BaselineChannel::Produced(b"{}".to_vec())
    );
    assert!(observation.scene_json.is_produced());
    assert!(observation.text_json.is_produced());
    assert_eq!(observation.rgba, BaselineChannel::Produced(vec![0; 4]));

    let (runner, _) = build_runner(&directory, &["unsupported"], &[], limits);
    let observation = runner.observe(&request).unwrap();
    assert_eq!(observation.parse_json, BaselineChannel::Unsupported);
    assert_eq!(observation.scene_json, BaselineChannel::Unsupported);
    assert!(observation.text_json.is_produced());

    let (runner, _) = build_runner(&directory, &["channel-failed"], &[], limits);
    let observation = runner.observe(&request).unwrap();
    assert_eq!(observation.parse_json, BaselineChannel::Failed);
}

#[test]
fn passes_literal_argv_and_only_allowlisted_environment_without_a_shell() {
    let directory = TestDirectory::new("argv");
    let sentinel = directory.path().join("sentinel");
    let limits = ordinary_limits();
    let (runner, _) = build_runner(
        &directory,
        &["inspect", "; $(touch sentinel) spaced"],
        &[("PDF_RS_ALLOWED", "yes")],
        limits,
    );
    runner.observe(&request(32)).unwrap();
    assert!(!sentinel.exists());
}

#[test]
fn concurrently_moves_large_stdin_stdout_and_stderr_without_deadlock() {
    let directory = TestDirectory::new("duplex");
    let limits =
        ProcessLimits::new(512 * 1024, 256 * 1024, 256 * 1024, Duration::from_secs(2)).unwrap();
    let (runner, _) = build_runner(&directory, &["emit", "131072", "131072"], &[], limits);
    let observation = runner.observe(&request(256 * 1024)).unwrap();
    let BaselineChannel::Produced(parse) = observation.parse_json else {
        panic!("parse channel was not produced");
    };
    assert_eq!(parse.len(), 131_072);
}

#[test]
fn enforces_exact_stdout_and_stderr_boundaries() {
    let directory = TestDirectory::new("pipe-limits");
    let request = request(32);

    let exact_stdout = ProcessLimits::new(1024, 122, 1024, Duration::from_secs(2)).unwrap();
    let (runner, _) = build_runner(&directory, &["ok"], &[], exact_stdout);
    runner.observe(&request).unwrap();
    let too_small_stdout = ProcessLimits::new(1024, 121, 1024, Duration::from_secs(2)).unwrap();
    let (runner, _) = build_runner(&directory, &["ok"], &[], too_small_stdout);
    assert_eq!(
        runner.observe(&request).err().unwrap().code,
        BaselineErrorCode::OutputLimit
    );

    let exact_stderr = ProcessLimits::new(1024, 1024, 4096, Duration::from_secs(2)).unwrap();
    let (runner, _) = build_runner(&directory, &["emit", "2", "4096"], &[], exact_stderr);
    runner.observe(&request).unwrap();
    let too_small_stderr = ProcessLimits::new(1024, 1024, 4095, Duration::from_secs(2)).unwrap();
    let (runner, _) = build_runner(&directory, &["emit", "2", "4096"], &[], too_small_stderr);
    assert_eq!(
        runner.observe(&request).err().unwrap().code,
        BaselineErrorCode::OutputLimit
    );
}

#[test]
fn kills_and_reaps_a_direct_child_after_the_watchdog() {
    let directory = TestDirectory::new("watchdog");
    let limits = ProcessLimits::new(1024, 1024, 1024, Duration::from_millis(50)).unwrap();
    let (runner, _) = build_runner(&directory, &["hang"], &[], limits);
    let started = Instant::now();
    assert_eq!(
        runner.observe(&request(32)).err().unwrap().code,
        BaselineErrorCode::WatchdogExpired
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn rejects_inherited_pipe_handles_without_waiting_indefinitely() {
    let directory = TestDirectory::new("inherited-pipes");
    let (runner, _) = build_runner(&directory, &["inherit-pipes"], &[], ordinary_limits());
    let started = Instant::now();
    assert_eq!(
        runner.observe(&request(32)).err().unwrap().code,
        BaselineErrorCode::ContainmentFailed
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn classifies_process_protocol_and_identity_failures_without_stderr_content() {
    let directory = TestDirectory::new("failures");
    let request = request(32);
    for (mode, expected) in [
        ("nonzero", BaselineErrorCode::RunnerFailed),
        ("protocol-fail", BaselineErrorCode::RunnerFailed),
        ("malformed", BaselineErrorCode::MalformedResponse),
        ("wrong-page", BaselineErrorCode::IdentityMismatch),
    ] {
        let (runner, _) = build_runner(&directory, &[mode], &[], ordinary_limits());
        let error = runner.observe(&request).err().unwrap();
        assert_eq!(error.code, expected, "mode={mode}");
        assert!(!error.to_string().contains("private-stderr-canary"));
        assert!(!format!("{error:?}").contains("private-stderr-canary"));
        assert!(!error.to_string().contains("%PDF"));
    }
}

#[test]
fn rejects_request_bytes_before_spawning_the_child() {
    let directory = TestDirectory::new("preflight");
    let marker = directory.path().join("spawned");
    let limits = ProcessLimits::new(96, 1024, 1024, Duration::from_secs(2)).unwrap();
    let marker_argument = marker.to_str().unwrap();
    let (runner, _) = build_runner(&directory, &["mark", marker_argument], &[], limits);
    assert_eq!(
        runner.observe(&request(32)).err().unwrap().code,
        BaselineErrorCode::RequestLimit
    );
    assert!(!marker.exists());
}

#[test]
fn rejects_descriptor_invocation_mismatch() {
    let directory = TestDirectory::new("identity");
    let limits = ordinary_limits();
    let process = process_spec(&directory, &["ok"], &[]);
    let mut descriptor = descriptor(&process, limits);
    descriptor.invocation_hash[0] ^= 1;
    assert_eq!(
        ProcessBaselineRunner::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );
}

fn build_runner(
    directory: &TestDirectory,
    arguments: &[&str],
    environment: &[(&str, &str)],
    limits: ProcessLimits,
) -> (ProcessBaselineRunner, BaselineDescriptor) {
    let process = process_spec(directory, arguments, environment);
    let descriptor = descriptor(&process, limits);
    let runner = ProcessBaselineRunner::new(descriptor.clone(), process, limits).unwrap();
    (runner, descriptor)
}

fn process_spec(
    directory: &TestDirectory,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> ProcessSpec {
    let isolation_profile = if arguments.first() == Some(&"inherit-pipes") {
        "test-only-deliberate-descendant-containment-failure"
    } else {
        "test-only-direct-child-no-grandchildren"
    };
    ProcessSpec::new(
        PathBuf::from(env!("CARGO_BIN_EXE_pdf-rs-baseline-fixture")),
        arguments.iter().map(|value| (*value).into()).collect(),
        environment
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect(),
        directory.path(),
        isolation_profile,
    )
    .unwrap()
}

fn descriptor(process: &ProcessSpec, limits: ProcessLimits) -> BaselineDescriptor {
    let executable = fs::read(env!("CARGO_BIN_EXE_pdf-rs-baseline-fixture")).unwrap();
    BaselineDescriptor {
        id: "fixture-process-v2".into(),
        engine: "self-authored-fixture".into(),
        upstream_revision: env!("CARGO_PKG_VERSION").into(),
        build_hash: sha256(&executable).unwrap(),
        build_flags_hash: digest(b"cargo-test-build-flags-v1"),
        environment_hash: digest(b"cleared-env-test-host-v1"),
        invocation_hash: process.identity(limits).unwrap(),
        license_manifest_hash: digest(b"self-authored-not-distributed-v1"),
        fonts_hash: digest(b"no-fonts-v1"),
        color_hash: digest(b"rgba8-fixture-v1"),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

fn digest(value: &[u8]) -> [u8; 32] {
    sha256(value).unwrap()
}

fn request(length: usize) -> BaselineRequest {
    assert!(length >= 8);
    let mut pdf = Vec::new();
    pdf.try_reserve_exact(length).unwrap();
    pdf.extend_from_slice(b"%PDF-2.0");
    pdf.resize(length, b'X');
    BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 0, 1, 1).unwrap()
}

fn ordinary_limits() -> ProcessLimits {
    ProcessLimits::new(
        1024 * 1024,
        1024 * 1024,
        1024 * 1024,
        Duration::from_secs(2),
    )
    .unwrap()
}
