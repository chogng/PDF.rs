use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pdf_rs_baseline::{
    BaselineChannel, BaselineDescriptor, BaselineRequest, BaselineRunner, OracleAuthority,
    PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES, PDFIUM_PAGE_COUNT_ADAPTER_PROFILE,
    PdfiumPageCountAdapter, ProcessLimits, ProcessSpec, descriptor_identity,
};
use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_digest::sha256;
use pdf_rs_document::{
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex,
    NeverCancelled as DocumentNeverCancelled, PageCountPoll, PageTreeJobContext, PageTreeLimits,
    RevisionAttestationJobContext, RevisionAttestationLimits, RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const PDFIUM_REVISION: &str = "c040cf96106a87220b814a1a892649cf2d7f1934";
const PDFIUM_ARGS_GN: &str = concat!(
    "use_remoteexec = false\n",
    "is_debug = false\n",
    "symbol_level = 0\n",
    "target_cpu = \"arm64\"\n",
    "pdf_is_standalone = true\n",
    "pdf_enable_v8 = false\n",
    "pdf_enable_xfa = false\n",
    "pdf_use_skia = false\n",
    "pdf_enable_fontations = false\n",
    "is_component_build = false\n",
);
const ENVIRONMENT_DECLARATION: &str = "env-clear direct-child local page-count performance probe; runtime and platform closure incomplete";
const LICENSE_DECLARATION: &str = "license closure incomplete; root license checked only";
const FONTS_DECLARATION: &str =
    "not-applicable-no-font-rendering; runtime font closure not evaluated";
const COLOR_DECLARATION: &str = "not-applicable-no-pixel-output";
const EXPECTED_HELPER_SHA256: &str =
    "a8675a5c15077b9a7843d1f3311091ecabcb25b5511a497655f7c977a1838b9c";
const EXPECTED_HELPER_BYTES: usize = 3_683_040;
const EXPECTED_FIXTURE_SHA256: &str =
    "c5d8b0d76e9ea6267f8f1868d326a3f87a811db39422b259da1ac58d713fc6f4";

const WIDE_PAGE_COUNT: u32 = 128;
const WARMUP_RUNS: usize = 5;
const SAMPLE_RUNS: usize = 50;
const PERF_MAX_REQUEST_BYTES: u64 = 256 * 1024;
const PERF_MAX_STDOUT_BYTES: u64 = 176;
const PERF_MAX_STDERR_BYTES: u64 = 4 * 1024;
const PERF_WATCHDOG: Duration = Duration::from_secs(2);

const REVISION_ID: RevisionId = RevisionId::new(63);
const ATTEST_JOB: JobId = JobId::new(6_301);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(6_302);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(6_303);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(6_304);
const PAGE_TREE_JOB: JobId = JobId::new(6_401);
const PAGE_TREE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(6_402);
const PAGE_TREE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(6_403);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

struct SampleSummary {
    minimum_ns: u64,
    median_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    maximum_ns: u64,
    median_ci95_low_ns: u64,
    median_ci95_high_ns: u64,
}

#[test]
fn page_count_performance_fixture_is_fixed_without_pdfium() {
    let fixture = wide_page_fixture();
    assert_eq!(
        hex(&sha256(&fixture.bytes).unwrap()),
        EXPECTED_FIXTURE_SHA256
    );
    assert_eq!(native_page_count(&fixture), u64::from(WIDE_PAGE_COUNT));
    assert_eq!(
        canonical_page_count(u64::from(WIDE_PAGE_COUNT)),
        b"{\"schema\":1,\"page_count\":128}\n"
    );
}

#[test]
#[ignore = "requires release mode and PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER pointing to the pinned PDFium page-count helper"]
fn real_pdfium_page_count_wide_cold_process_performance_probe() {
    assert_release_test_binary();
    assert_eq!(std::env::consts::OS, "macos");
    assert_eq!(std::env::consts::ARCH, "aarch64");

    let executable = required_adapter_path();
    let helper_bytes = fs::read(&executable).unwrap();
    let helper_hash = sha256(&helper_bytes).unwrap();
    assert_eq!(helper_bytes.len(), EXPECTED_HELPER_BYTES);
    assert_eq!(hex(&helper_hash), EXPECTED_HELPER_SHA256);

    let limits = ProcessLimits::new(
        PERF_MAX_REQUEST_BYTES,
        PERF_MAX_STDOUT_BYTES,
        PERF_MAX_STDERR_BYTES,
        PERF_WATCHDOG,
    )
    .unwrap();
    let process = ProcessSpec::new(
        &executable,
        Vec::new(),
        Vec::new(),
        repository_root(),
        "uncontained-local-pdfium-page-count-performance-probe-v1",
    )
    .unwrap();
    let build_flags_hash = digest(PDFIUM_ARGS_GN);
    let environment_hash = digest(ENVIRONMENT_DECLARATION);
    let license_manifest_hash = digest(LICENSE_DECLARATION);
    let fonts_hash = digest(FONTS_DECLARATION);
    let color_hash = digest(COLOR_DECLARATION);
    let invocation_hash = process.identity(limits).unwrap();
    let descriptor = BaselineDescriptor {
        id: PDFIUM_PAGE_COUNT_ADAPTER_PROFILE.into(),
        engine: "pdfium".into(),
        upstream_revision: PDFIUM_REVISION.into(),
        build_hash: helper_hash,
        build_flags_hash,
        environment_hash,
        invocation_hash,
        license_manifest_hash,
        fonts_hash,
        color_hash,
        platform: "macos-arm64".into(),
    };
    let descriptor_hash = descriptor_identity(&descriptor).unwrap();
    let adapter = PdfiumPageCountAdapter::new(descriptor, process, limits).unwrap();
    let fixture = wide_page_fixture();
    let fixture_hash = sha256(&fixture.bytes).unwrap();
    assert_eq!(hex(&fixture_hash), EXPECTED_FIXTURE_SHA256);
    let expected = canonical_page_count(u64::from(WIDE_PAGE_COUNT));

    for _ in 0..WARMUP_RUNS {
        assert_eq!(
            native_page_count(black_box(&fixture)),
            u64::from(WIDE_PAGE_COUNT)
        );
        assert_eq!(observe(&adapter, black_box(&fixture.bytes)), expected);
    }

    let mut native_samples_ns = Vec::with_capacity(SAMPLE_RUNS);
    let mut pdfium_samples_ns = Vec::with_capacity(SAMPLE_RUNS);
    for iteration in 0..SAMPLE_RUNS {
        if iteration % 2 == 0 {
            native_samples_ns.push(measure_native(&fixture));
            pdfium_samples_ns.push(measure_pdfium(&adapter, &fixture, &expected));
        } else {
            pdfium_samples_ns.push(measure_pdfium(&adapter, &fixture, &expected));
            native_samples_ns.push(measure_native(&fixture));
        }
    }

    let native = summarize(&native_samples_ns);
    let pdfium = summarize(&pdfium_samples_ns);
    let median_ratio_milli = pdfium
        .median_ns
        .checked_mul(1_000)
        .unwrap()
        .checked_div(native.median_ns)
        .unwrap();

    println!("pdfium_revision={PDFIUM_REVISION}");
    println!("helper_sha256={}", hex(&helper_hash));
    println!("helper_bytes={}", helper_bytes.len());
    println!("build_flags_sha256={}", hex(&build_flags_hash));
    println!("environment_sha256={}", hex(&environment_hash));
    println!("invocation_sha256={}", hex(&invocation_hash));
    println!("license_manifest_sha256={}", hex(&license_manifest_hash));
    println!("fonts_sha256={}", hex(&fonts_hash));
    println!("color_sha256={}", hex(&color_hash));
    println!("descriptor_identity_sha256={}", hex(&descriptor_hash));
    println!("fixture_sha256={}", hex(&fixture_hash));
    println!("fixture_bytes={}", fixture.bytes.len());
    println!("page_count={WIDE_PAGE_COUNT}");
    println!("warmup_runs_per_engine={WARMUP_RUNS}");
    println!("sample_runs_per_engine={SAMPLE_RUNS}");
    println!("native_raw_ns={}", csv(&native_samples_ns));
    println!("pdfium_raw_ns={}", csv(&pdfium_samples_ns));
    print_summary("native", &native);
    print_summary("pdfium", &pdfium);
    println!("pdfium_to_native_median_ratio_milli={median_ratio_milli}");
    println!(
        "behavior_output_sha256={}",
        hex(&sha256(&expected).unwrap())
    );
    println!("behavior_counts_exact=true");
    println!("native_measurement_scope=full-in-memory-xref-attestation-and-strict-page-count");
    println!("pdfium_measurement_scope=schema-2-encode-cold-child-init-load-count-response-decode");
    println!("performance_scope_comparable=false");
    println!("performance_eligible=false");
    println!("baseline_registration_eligible=false");
}

fn measure_native(fixture: &Fixture) -> u64 {
    let started = Instant::now();
    let count = black_box(native_page_count(black_box(fixture)));
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap();
    assert_eq!(count, u64::from(WIDE_PAGE_COUNT));
    assert_ne!(elapsed, 0);
    elapsed
}

fn measure_pdfium(adapter: &PdfiumPageCountAdapter, fixture: &Fixture, expected: &[u8]) -> u64 {
    let started = Instant::now();
    let output = black_box(observe(adapter, black_box(&fixture.bytes)));
    let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap();
    assert_eq!(output, expected);
    assert_ne!(elapsed, 0);
    elapsed
}

fn summarize(samples: &[u64]) -> SampleSummary {
    assert_eq!(samples.len(), SAMPLE_RUNS);
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    SampleSummary {
        minimum_ns: sorted[0],
        median_ns: nearest_rank(&sorted, 50, 100),
        p95_ns: nearest_rank(&sorted, 95, 100),
        p99_ns: nearest_rank(&sorted, 99, 100),
        maximum_ns: sorted[SAMPLE_RUNS - 1],
        median_ci95_low_ns: sorted[17],
        median_ci95_high_ns: sorted[32],
    }
}

fn nearest_rank(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    let rank = sorted
        .len()
        .checked_mul(numerator)
        .unwrap()
        .checked_add(denominator - 1)
        .unwrap()
        / denominator;
    sorted[rank - 1]
}

fn print_summary(prefix: &str, summary: &SampleSummary) {
    println!("{prefix}_minimum_ns={}", summary.minimum_ns);
    println!("{prefix}_median_ns={}", summary.median_ns);
    println!("{prefix}_p95_ns={}", summary.p95_ns);
    println!("{prefix}_p99_ns={}", summary.p99_ns);
    println!("{prefix}_maximum_ns={}", summary.maximum_ns);
    println!("{prefix}_median_ci95_low_ns={}", summary.median_ci95_low_ns);
    println!(
        "{prefix}_median_ci95_high_ns={}",
        summary.median_ci95_high_ns
    );
}

fn csv(samples: &[u64]) -> String {
    samples
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn observe(adapter: &PdfiumPageCountAdapter, pdf: &[u8]) -> Vec<u8> {
    let request = BaselineRequest::new(sha256(pdf).unwrap(), pdf.to_vec(), 0, 1, 1).unwrap();
    let observation = adapter.observe(&request).unwrap();
    assert_eq!(observation.authority(), OracleAuthority::O4Observation);
    assert_eq!(observation.scene_json, BaselineChannel::Unsupported);
    assert_eq!(observation.text_json, BaselineChannel::Unsupported);
    assert_eq!(observation.rgba, BaselineChannel::Unsupported);
    match observation.parse_json {
        BaselineChannel::Produced(value) => {
            assert!(
                u64::try_from(value.len()).unwrap() <= PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES
            );
            assert_eq!(value.last(), Some(&b'\n'));
            std::str::from_utf8(&value).unwrap();
            value
        }
        BaselineChannel::Unsupported => panic!("PDFium page-count profile returned unsupported"),
        BaselineChannel::Failed => panic!("PDFium page-count profile failed"),
    }
}

fn wide_page_fixture() -> Fixture {
    let mut bodies = Vec::with_capacity(usize::try_from(WIDE_PAGE_COUNT + 2).unwrap());
    bodies.push((
        1,
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
    ));

    let mut kids = String::new();
    for page in 0..WIDE_PAGE_COUNT {
        write!(&mut kids, "{} 0 R ", page + 3).unwrap();
    }
    bodies.push((
        2,
        format!("2 0 obj\n<< /Type /Pages /Kids [{kids}] /Count {WIDE_PAGE_COUNT} >>\nendobj\n")
            .into_bytes(),
    ));
    for page in 0..WIDE_PAGE_COUNT {
        let object = page + 3;
        bodies.push((
            object,
            format!(
                "{object} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n"
            )
            .into_bytes(),
        ));
    }
    fixture(&bodies, WIDE_PAGE_COUNT + 3)
}

fn fixture(bodies: &[(u32, Vec<u8>)], size: u32) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut in_use = Vec::new();
    for (number, body) in bodies {
        let offset = u64::try_from(bytes.len()).unwrap();
        in_use.push((*number, offset));
        bytes.extend_from_slice(body);
    }
    let startxref = u64::try_from(bytes.len()).unwrap();
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else if let Some((_, offset)) = in_use.iter().find(|&&(entry, _)| entry == number) {
            format!("{offset:010} 00000 n \n")
        } else {
            "0000000000 00000 f \n".to_owned()
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new(sha256(&bytes).unwrap()),
            SourceRevision::new(63),
        ),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xf6; 32]),
    );
    Fixture { bytes, snapshot }
}

fn native_page_count(fixture: &Fixture) -> u64 {
    let index = ready_index(fixture);
    let store = supplied_store(fixture);
    let mut job = index
        .count_pages(
            PageTreeJobContext::new(
                PAGE_TREE_JOB,
                PAGE_TREE_ENVELOPE,
                PAGE_TREE_BOUNDARY,
                RequestPriority::Metadata,
            ),
            PageTreeLimits::default(),
        )
        .unwrap();
    match job.poll(&store, &DocumentNeverCancelled) {
        PageCountPoll::Ready(count) => count.page_count(),
        PageCountPoll::Pending { .. } => panic!("complete fixture must not suspend"),
        PageCountPoll::Failed(error) => panic!("fixture page tree must count: {error}"),
    }
}

fn ready_index(fixture: &Fixture) -> AttestedRevisionIndex {
    let store = supplied_store(fixture);
    let mut job = AttestRevisionJob::new(
        CandidateRevisionIndex::from_xref(
            &parsed_xref(fixture),
            REVISION_ID,
            pdf_rs_document::DocumentLimits::default(),
            &DocumentNeverCancelled,
        )
        .unwrap(),
        RevisionAttestationJobContext::new(
            ATTEST_JOB,
            ATTEST_SCAN,
            ATTEST_ENVELOPE,
            ATTEST_BOUNDARY,
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &DocumentNeverCancelled) {
        RevisionAttestationPoll::Ready(index) => index,
        RevisionAttestationPoll::Pending { .. } => panic!("complete fixture must not suspend"),
        RevisionAttestationPoll::Failed(error) => panic!("fixture must attest: {error}"),
    }
}

fn parsed_xref(fixture: &Fixture) -> XrefSection {
    let store = supplied_store(fixture);
    let mut job = OpenXrefJob::new(
        fixture.snapshot,
        XrefJobContext::new(
            JobId::new(6_501),
            ResumeCheckpoint::new(6_502),
            ResumeCheckpoint::new(6_503),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .unwrap();
    match job.poll(&store, &XrefNeverCancelled) {
        XrefPoll::Ready(section) => section,
        XrefPoll::Pending { .. } => panic!("complete fixture must not suspend"),
        XrefPoll::Failed(error) => panic!("fixture xref must parse: {error}"),
    }
}

fn supplied_store(fixture: &Fixture) -> RangeStore {
    let store = RangeStore::new(fixture.snapshot, Default::default()).unwrap();
    store
        .supply(
            RangeResponse::new(
                fixture.snapshot,
                ByteRange::new(0, u64::try_from(fixture.bytes.len()).unwrap()).unwrap(),
                fixture.bytes.clone(),
            )
            .unwrap(),
        )
        .unwrap();
    store
}

fn canonical_page_count(page_count: u64) -> Vec<u8> {
    format!("{{\"schema\":1,\"page_count\":{page_count}}}\n").into_bytes()
}

fn required_adapter_path() -> PathBuf {
    PathBuf::from(
        std::env::var_os("PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER").unwrap_or_else(|| {
            panic!("PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER is required for this ignored test")
        }),
    )
}

fn assert_release_test_binary() {
    let executable = std::env::current_exe().unwrap();
    assert!(
        executable
            .components()
            .any(|component| component.as_os_str() == "release"),
        "run this probe with cargo test --release"
    );
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn digest(value: &str) -> [u8; 32] {
    sha256(value.as_bytes()).unwrap()
}

fn hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in value {
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}
