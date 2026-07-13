use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineErrorCode, BaselineRequest, BaselineRunner,
    OracleAuthority, PDFIUM_PIXEL_ADAPTER_PROFILE, PdfiumPixelAdapter, ProcessLimits, ProcessSpec,
};
use pdf_rs_digest::sha256;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-pdfium-adapter-{label}-{}-{sequence}",
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
fn pixel_profile_accepts_only_explicit_pixel_outcomes() {
    let directory = TestDirectory::new("outcomes");
    let produced = runner("pixel-only", &directory);
    assert_eq!(
        produced.describe().unwrap().id,
        PDFIUM_PIXEL_ADAPTER_PROFILE
    );
    let observation = produced.observe(&request()).unwrap();
    assert_eq!(observation.authority(), OracleAuthority::O4Observation);
    assert_eq!(observation.parse_json, BaselineChannel::Unsupported);
    assert_eq!(observation.scene_json, BaselineChannel::Unsupported);
    assert_eq!(observation.text_json, BaselineChannel::Unsupported);
    assert_eq!(observation.rgba, BaselineChannel::Produced(vec![0; 4]));

    let failed = runner("pixel-failed", &directory)
        .observe(&request())
        .unwrap();
    assert_eq!(failed.parse_json, BaselineChannel::Unsupported);
    assert_eq!(failed.scene_json, BaselineChannel::Unsupported);
    assert_eq!(failed.text_json, BaselineChannel::Unsupported);
    assert_eq!(failed.rgba, BaselineChannel::Failed);
}

#[test]
fn pixel_profile_rejects_a_helper_that_fabricates_semantic_channels() {
    let directory = TestDirectory::new("violation");
    let error = runner("profile-violation", &directory)
        .observe(&request())
        .err()
        .unwrap();
    assert_eq!(error.code, BaselineErrorCode::MalformedResponse);
    assert_eq!(error.diagnostic_id, "RPE-BASELINE-0005");
}

#[test]
fn pixel_profile_rejects_wrong_identity_and_helper_arguments() {
    let directory = TestDirectory::new("config");

    let (mut descriptor, process, limits) = parts("pixel-only", &directory, Vec::new());
    descriptor.engine = "not-pdfium".into();
    assert_eq!(
        PdfiumPixelAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let (mut descriptor, process, limits) = parts("pixel-only", &directory, Vec::new());
    descriptor.id = "different-profile".into();
    assert_eq!(
        PdfiumPixelAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let (descriptor, process, limits) = parts("pixel-only", &directory, vec!["unexpected".into()]);
    assert_eq!(
        PdfiumPixelAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );
}

fn runner(mode: &str, directory: &TestDirectory) -> PdfiumPixelAdapter {
    let (descriptor, process, limits) = parts(mode, directory, Vec::new());
    PdfiumPixelAdapter::new(descriptor, process, limits).unwrap()
}

fn parts(
    mode: &str,
    directory: &TestDirectory,
    arguments: Vec<String>,
) -> (BaselineDescriptor, ProcessSpec, ProcessLimits) {
    let limits = ProcessLimits::new(1024, 1024, 1024, Duration::from_secs(2)).unwrap();
    let process = ProcessSpec::new(
        PathBuf::from(env!("CARGO_BIN_EXE_pdf-rs-baseline-fixture")),
        arguments,
        vec![("PDF_RS_BASELINE_FIXTURE_MODE".into(), mode.into())],
        directory.path(),
        "test-only-direct-child-no-grandchildren",
    )
    .unwrap();
    let executable = fs::read(env!("CARGO_BIN_EXE_pdf-rs-baseline-fixture")).unwrap();
    let mut descriptor = BaselineDescriptor {
        id: PDFIUM_PIXEL_ADAPTER_PROFILE.into(),
        engine: "pdfium".into(),
        upstream_revision: "self-authored-contract-fixture".into(),
        build_hash: sha256(&executable).unwrap(),
        build_flags_hash: digest(b"test-only-no-pdfium-build"),
        environment_hash: digest(b"cleared-env-contract-fixture"),
        invocation_hash: [1; 32],
        license_manifest_hash: digest(b"self-authored-test-fixture-license"),
        fonts_hash: digest(b"no-fonts-test-fixture"),
        color_hash: digest(b"rgba8-test-fixture"),
        platform: format!("test-{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    };
    descriptor.invocation_hash = process.identity(limits).unwrap();
    (descriptor, process, limits)
}

fn request() -> BaselineRequest {
    let pdf = b"%PDF-1.7".to_vec();
    BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 0, 1, 1).unwrap()
}

fn digest(value: &[u8]) -> [u8; 32] {
    sha256(value).unwrap()
}
