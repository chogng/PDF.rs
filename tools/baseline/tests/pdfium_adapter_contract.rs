use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineErrorCode, BaselineRequest, BaselineRunner,
    OracleAuthority, PDFIUM_OUTLINE_ADAPTER_MAX_PARSE_BYTES, PDFIUM_OUTLINE_ADAPTER_PROFILE,
    PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES, PDFIUM_PAGE_COUNT_ADAPTER_PROFILE,
    PDFIUM_PIXEL_ADAPTER_MAX_RGBA_BYTES, PDFIUM_PIXEL_ADAPTER_PROFILE, PdfiumOutlineAdapter,
    PdfiumPageCountAdapter, PdfiumPixelAdapter, ProcessLimits, ProcessSpec,
};
use pdf_rs_digest::sha256;

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static FIXTURE_SERIAL: Mutex<()> = Mutex::new(());

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
    let _fixture_serial = fixture_serial_guard();
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
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("violation");
    for mode in [
        "profile-violation",
        "pixel-parse-failed",
        "pixel-scene-failed",
        "pixel-text-failed",
        "pixel-unsupported",
    ] {
        let error = runner(mode, &directory).observe(&request()).err().unwrap();
        assert_eq!(error.code, BaselineErrorCode::MalformedResponse, "{mode}");
        assert_eq!(error.diagnostic_id, "RPE-BASELINE-0005", "{mode}");
    }
}

#[test]
fn pixel_profile_rejects_wrong_identity_and_helper_arguments() {
    let _fixture_serial = fixture_serial_guard();
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

    let (mut descriptor, process, limits) = parts("pixel-only", &directory, Vec::new());
    descriptor.build_hash[0] ^= 1;
    assert_eq!(
        PdfiumPixelAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let limits = ordinary_limits();
    let (descriptor, process, limits) = parts_with_limits(
        "pixel-only",
        &directory,
        Vec::new(),
        vec![("UNREVIEWED_BEHAVIOR".into(), "enabled".into())],
        limits,
    );
    assert_eq!(
        PdfiumPixelAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );
}

#[test]
fn pixel_geometry_is_rejected_before_the_helper_is_spawned() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("preflight-pipe");
    let limits = ProcessLimits::new(1024, 115, 1024, Duration::from_secs(2)).unwrap();
    let (descriptor, process, limits) = parts_with_limits(
        "pixel-only-marker",
        &directory,
        Vec::new(),
        Vec::new(),
        limits,
    );
    let adapter = PdfiumPixelAdapter::new(descriptor, process, limits).unwrap();
    let error = adapter.observe(&request()).err().unwrap();
    assert_eq!(error.code, BaselineErrorCode::OutputLimit);
    assert!(!directory.path().join("spawned").exists());

    let directory = TestDirectory::new("preflight-profile");
    let limits = ProcessLimits::new(1024, 128 * 1024 * 1024, 1024, Duration::from_secs(2)).unwrap();
    let (descriptor, process, limits) = parts_with_limits(
        "pixel-only-marker",
        &directory,
        Vec::new(),
        Vec::new(),
        limits,
    );
    let adapter = PdfiumPixelAdapter::new(descriptor, process, limits).unwrap();
    let width = 4097;
    let height = 4097;
    assert!(u64::from(width) * u64::from(height) * 4 > PDFIUM_PIXEL_ADAPTER_MAX_RGBA_BYTES);
    let error = adapter
        .observe(&request_with_geometry(width, height))
        .err()
        .unwrap();
    assert_eq!(error.code, BaselineErrorCode::OutputLimit);
    assert!(!directory.path().join("spawned").exists());
}

#[test]
fn page_count_profile_accepts_only_canonical_parse_output() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("page-count-outcome");
    let adapter = page_count_runner("page-count-only", &directory);
    assert_eq!(
        adapter.describe().unwrap().id,
        PDFIUM_PAGE_COUNT_ADAPTER_PROFILE
    );
    let observation = adapter.observe(&request()).unwrap();
    assert_eq!(observation.authority(), OracleAuthority::O4Observation);
    assert_eq!(
        observation.parse_json,
        BaselineChannel::Produced(b"{\"schema\":1,\"page_count\":1}\n".to_vec())
    );
    assert_eq!(observation.scene_json, BaselineChannel::Unsupported);
    assert_eq!(observation.text_json, BaselineChannel::Unsupported);
    assert_eq!(observation.rgba, BaselineChannel::Unsupported);
}

#[test]
fn page_count_profile_rejects_missing_malformed_or_extra_channels() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("page-count-violation");
    for mode in [
        "page-count-parse-unsupported",
        "page-count-parse-failed",
        "page-count-scene-produced",
        "page-count-text-produced",
        "page-count-pixel-produced",
        "page-count-invalid-utf8",
        "page-count-no-newline",
    ] {
        let error = page_count_runner(mode, &directory)
            .observe(&request())
            .err()
            .unwrap();
        assert_eq!(error.code, BaselineErrorCode::MalformedResponse, "{mode}");
        assert_eq!(error.diagnostic_id, "RPE-BASELINE-0005", "{mode}");
    }
}

#[test]
fn page_count_profile_rejects_wrong_identity_and_process_configuration() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("page-count-identity");

    let (mut descriptor, process, limits) = page_count_parts(
        "page-count-only",
        &directory,
        Vec::new(),
        Vec::new(),
        page_count_limits(),
    );
    descriptor.engine = "not-pdfium".into();
    assert_eq!(
        PdfiumPageCountAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let (mut descriptor, process, limits) = page_count_parts(
        "page-count-only",
        &directory,
        Vec::new(),
        Vec::new(),
        page_count_limits(),
    );
    descriptor.id = "different-profile".into();
    assert_eq!(
        PdfiumPageCountAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let (mut descriptor, process, limits) = page_count_parts(
        "page-count-only",
        &directory,
        Vec::new(),
        Vec::new(),
        page_count_limits(),
    );
    descriptor.build_hash[0] ^= 1;
    assert_eq!(
        PdfiumPageCountAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    for (arguments, environment) in [
        (vec!["unexpected".into()], Vec::new()),
        (
            Vec::new(),
            vec![("UNREVIEWED_BEHAVIOR".into(), "enabled".into())],
        ),
    ] {
        let (descriptor, process, limits) = page_count_parts(
            "page-count-only",
            &directory,
            arguments,
            environment,
            page_count_limits(),
        );
        assert_eq!(
            PdfiumPageCountAdapter::new(descriptor, process, limits)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::InvalidProcessConfig
        );
    }
}

#[test]
fn page_count_profile_rejects_noncanonical_requests_and_output_ceilings() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("page-count-config");
    let adapter = page_count_runner("page-count-only", &directory);
    let pdf = b"%PDF-1.7".to_vec();
    let nonzero_page = BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 1, 1, 1).unwrap();
    assert_eq!(
        adapter.observe(&nonzero_page).err().unwrap().code,
        BaselineErrorCode::InvalidRequest
    );
    assert_eq!(
        adapter
            .observe(&request_with_geometry(2, 1))
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidRequest
    );

    let limits = ProcessLimits::new(
        1024,
        PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES + 113,
        1024,
        Duration::from_secs(2),
    )
    .unwrap();
    let (descriptor, process, limits) = page_count_parts(
        "page-count-only",
        &directory,
        Vec::new(),
        Vec::new(),
        limits,
    );
    assert_eq!(
        PdfiumPageCountAdapter::new(descriptor, process, limits)
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidProcessConfig
    );

    let oversized = page_count_runner("page-count-too-large", &directory)
        .observe(&request())
        .err()
        .unwrap();
    assert_eq!(oversized.code, BaselineErrorCode::OutputLimit);
}

#[test]
fn outline_profile_accepts_only_canonical_parse_output() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("outline-outcome");
    let adapter = outline_runner("outline-only", &directory);
    assert_eq!(
        adapter.describe().unwrap().id,
        PDFIUM_OUTLINE_ADAPTER_PROFILE
    );
    let observation = adapter.observe(&request()).unwrap();
    assert_eq!(observation.authority(), OracleAuthority::O4Observation);
    assert_eq!(
        observation.parse_json,
        BaselineChannel::Produced(b"{\"schema\":1,\"items\":[]}\n".to_vec())
    );
    assert_eq!(observation.scene_json, BaselineChannel::Unsupported);
    assert_eq!(observation.text_json, BaselineChannel::Unsupported);
    assert_eq!(observation.rgba, BaselineChannel::Unsupported);
}

#[test]
fn outline_profile_rejects_missing_malformed_or_extra_channels() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("outline-violation");
    for mode in [
        "outline-parse-failed",
        "outline-scene-produced",
        "outline-text-produced",
        "outline-pixel-produced",
        "outline-invalid-utf8",
        "outline-no-newline",
    ] {
        let error = outline_runner(mode, &directory)
            .observe(&request())
            .err()
            .unwrap();
        assert_eq!(error.code, BaselineErrorCode::MalformedResponse, "{mode}");
        assert_eq!(error.diagnostic_id, "RPE-BASELINE-0005", "{mode}");
    }
}

#[test]
fn outline_profile_rejects_noncanonical_requests_and_output_ceilings() {
    let _fixture_serial = fixture_serial_guard();
    let directory = TestDirectory::new("outline-config");
    let adapter = outline_runner("outline-only", &directory);
    let pdf = b"%PDF-1.7".to_vec();
    let nonzero_page = BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 1, 1, 1).unwrap();
    assert_eq!(
        adapter.observe(&nonzero_page).err().unwrap().code,
        BaselineErrorCode::InvalidRequest
    );
    assert_eq!(
        adapter
            .observe(&request_with_geometry(2, 1))
            .err()
            .unwrap()
            .code,
        BaselineErrorCode::InvalidRequest
    );

    let limits = ProcessLimits::new(
        1024,
        PDFIUM_OUTLINE_ADAPTER_MAX_PARSE_BYTES + 113,
        1024,
        Duration::from_secs(2),
    )
    .unwrap();
    let (mut descriptor, process, limits) =
        parts_with_limits("outline-only", &directory, Vec::new(), Vec::new(), limits);
    descriptor.id = PDFIUM_OUTLINE_ADAPTER_PROFILE.into();
    assert_eq!(
        PdfiumOutlineAdapter::new(descriptor, process, limits)
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

fn outline_runner(mode: &str, directory: &TestDirectory) -> PdfiumOutlineAdapter {
    let (mut descriptor, process, limits) = parts(mode, directory, Vec::new());
    descriptor.id = PDFIUM_OUTLINE_ADAPTER_PROFILE.into();
    PdfiumOutlineAdapter::new(descriptor, process, limits).unwrap()
}

fn page_count_runner(mode: &str, directory: &TestDirectory) -> PdfiumPageCountAdapter {
    let (descriptor, process, limits) =
        page_count_parts(mode, directory, Vec::new(), Vec::new(), page_count_limits());
    PdfiumPageCountAdapter::new(descriptor, process, limits).unwrap()
}

fn page_count_parts(
    mode: &str,
    directory: &TestDirectory,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    limits: ProcessLimits,
) -> (BaselineDescriptor, ProcessSpec, ProcessLimits) {
    let (mut descriptor, process, limits) =
        parts_with_limits(mode, directory, arguments, environment, limits);
    descriptor.id = PDFIUM_PAGE_COUNT_ADAPTER_PROFILE.into();
    (descriptor, process, limits)
}

fn parts(
    mode: &str,
    directory: &TestDirectory,
    arguments: Vec<String>,
) -> (BaselineDescriptor, ProcessSpec, ProcessLimits) {
    parts_with_limits(mode, directory, arguments, Vec::new(), ordinary_limits())
}

fn parts_with_limits(
    mode: &str,
    directory: &TestDirectory,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    limits: ProcessLimits,
) -> (BaselineDescriptor, ProcessSpec, ProcessLimits) {
    let executable = fixture_executable(mode, directory);
    let process = ProcessSpec::new(
        &executable,
        arguments,
        environment,
        directory.path(),
        "test-only-direct-child-no-grandchildren",
    )
    .unwrap();
    let executable_bytes = fs::read(executable).unwrap();
    let mut descriptor = BaselineDescriptor {
        id: PDFIUM_PIXEL_ADAPTER_PROFILE.into(),
        engine: "pdfium".into(),
        upstream_revision: "self-authored-contract-fixture".into(),
        build_hash: sha256(&executable_bytes).unwrap(),
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
    request_with_geometry(1, 1)
}

fn request_with_geometry(width: u32, height: u32) -> BaselineRequest {
    let pdf = b"%PDF-1.7".to_vec();
    BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 0, width, height).unwrap()
}

fn ordinary_limits() -> ProcessLimits {
    ProcessLimits::new(1024, 1024, 1024, Duration::from_secs(2)).unwrap()
}

fn page_count_limits() -> ProcessLimits {
    ProcessLimits::new(
        1024,
        PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES + 112,
        1024,
        Duration::from_secs(2),
    )
    .unwrap()
}

fn fixture_executable(mode: &str, directory: &TestDirectory) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_BIN_EXE_pdf-rs-baseline-fixture"));
    let destination = directory
        .path()
        .join(format!("pdf-rs-baseline-fixture-{mode}"));

    // Keep one copied inode under exactly one mode-bearing path. Multiple hard
    // links to the built inode make macOS `current_exe()` path selection
    // ambiguous when these tests run concurrently. Renaming the per-test copy
    // preserves executable validation while making the selected mode stable.
    let existing = fs::read_dir(directory.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pdf-rs-baseline-fixture-"))
        });
    match existing {
        Some(path) if path != destination => fs::rename(path, &destination).unwrap(),
        Some(_) => {}
        None => {
            fs::copy(source, &destination).unwrap();
        }
    }
    destination
}

fn fixture_serial_guard() -> MutexGuard<'static, ()> {
    FIXTURE_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn digest(value: &[u8]) -> [u8; 32] {
    sha256(value).unwrap()
}
