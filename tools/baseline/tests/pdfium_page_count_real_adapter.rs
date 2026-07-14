use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    AttestRevisionJob, AttestedRevisionIndex, CandidateRevisionIndex, DocumentError,
    DocumentErrorCode, NeverCancelled as DocumentNeverCancelled, PageCountPoll, PageTreeJobContext,
    PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionAttestationPoll, RevisionId,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    NeverCancelled as XrefNeverCancelled, OpenXrefJob, XrefJobContext, XrefLimits, XrefPoll,
    XrefSection,
};

const PDFIUM_REVISION: &str = "c040cf96106a87220b814a1a892649cf2d7f1934";
const PDFIUM_BUILD_ARGS: &str = "use_remoteexec=false is_debug=false symbol_level=0 target_cpu=\"arm64\" pdf_is_standalone=true pdf_enable_v8=false pdf_enable_xfa=false pdf_use_skia=false pdf_enable_fontations=false is_component_build=false";
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
const ENVIRONMENT_DECLARATION: &str =
    "env-clear direct-child local page-count probe; runtime and platform closure incomplete";
const LICENSE_DECLARATION: &str = "license closure incomplete; root license checked only";
const FONTS_DECLARATION: &str =
    "not-applicable-no-font-rendering; runtime font closure not evaluated";
const COLOR_DECLARATION: &str = "not-applicable-no-pixel-output";
const EXPECTED_HELPER_SHA256: &str =
    "a8675a5c15077b9a7843d1f3311091ecabcb25b5511a497655f7c977a1838b9c";
const EXPECTED_HELPER_BYTES: usize = 3_683_040;

const REVISION_ID: RevisionId = RevisionId::new(43);
const ATTEST_JOB: JobId = JobId::new(4_301);
const ATTEST_SCAN: ResumeCheckpoint = ResumeCheckpoint::new(4_302);
const ATTEST_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(4_303);
const ATTEST_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(4_304);
const PAGE_TREE_JOB: JobId = JobId::new(4_401);
const PAGE_TREE_ENVELOPE: ResumeCheckpoint = ResumeCheckpoint::new(4_402);
const PAGE_TREE_BOUNDARY: ResumeCheckpoint = ResumeCheckpoint::new(4_403);

struct Fixture {
    bytes: Vec<u8>,
    snapshot: SourceSnapshot,
}

#[test]
fn page_count_probe_fixtures_are_fixed_without_running_pdfium() {
    let single = single_page_fixture();
    let nested = nested_three_page_fixture(3);
    let mismatched = nested_three_page_fixture(4);

    assert_ne!(
        sha256(&single.bytes).unwrap(),
        sha256(&nested.bytes).unwrap()
    );
    assert_ne!(
        sha256(&nested.bytes).unwrap(),
        sha256(&mismatched.bytes).unwrap()
    );
    assert_eq!(
        canonical_page_count(native_page_count(&single).unwrap()),
        b"{\"schema\":1,\"page_count\":1}\n"
    );
    assert_eq!(
        canonical_page_count(native_page_count(&nested).unwrap()),
        b"{\"schema\":1,\"page_count\":3}\n"
    );
    assert_eq!(
        native_page_count(&mismatched).unwrap_err().code(),
        DocumentErrorCode::PageTreeCountMismatch
    );
}

#[test]
#[ignore = "requires PDF_RS_PDFIUM_PAGE_COUNT_ADAPTER pointing to the separately built PDFium page-count helper"]
fn real_pdfium_page_counts_match_native_and_record_strict_count_difference() {
    let executable = required_adapter_path();
    let helper_bytes = fs::read(&executable).unwrap();
    let helper_hash = sha256(&helper_bytes).unwrap();
    assert_eq!(
        helper_bytes.len(),
        EXPECTED_HELPER_BYTES,
        "replace the page-count helper byte-count PLACEHOLDER after the pinned build"
    );
    assert_eq!(
        hex(&helper_hash),
        EXPECTED_HELPER_SHA256,
        "replace the page-count helper SHA-256 PLACEHOLDER after the pinned build"
    );

    assert_eq!(std::env::consts::OS, "macos");
    assert_eq!(std::env::consts::ARCH, "aarch64");
    let repository = repository_root();
    let limits = ProcessLimits::new(4096, 176, 4096, Duration::from_secs(2)).unwrap();
    let process = ProcessSpec::new(
        &executable,
        Vec::new(),
        Vec::new(),
        &repository,
        "uncontained-local-pdfium-page-count-probe-v1",
    )
    .unwrap();
    let build_flags_hash = digest(PDFIUM_ARGS_GN);
    let build_args_command_hash = digest(PDFIUM_BUILD_ARGS);
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

    let single = single_page_fixture();
    let single_native = canonical_page_count(native_page_count(&single).unwrap());
    let single_first = observe(&adapter, &single.bytes);
    let single_second = observe(&adapter, &single.bytes);
    assert_eq!(single_first, single_second);
    assert_eq!(single_first, single_native);

    let nested = nested_three_page_fixture(3);
    let nested_native = canonical_page_count(native_page_count(&nested).unwrap());
    let nested_first = observe(&adapter, &nested.bytes);
    let nested_second = observe(&adapter, &nested.bytes);
    assert_eq!(nested_first, nested_second);
    assert_eq!(nested_first, nested_native);

    let mismatched = nested_three_page_fixture(4);
    let native_error = native_page_count(&mismatched).unwrap_err();
    assert_eq!(
        native_error.code(),
        DocumentErrorCode::PageTreeCountMismatch
    );
    let pdfium_mismatched_first = observe(&adapter, &mismatched.bytes);
    let pdfium_mismatched_second = observe(&adapter, &mismatched.bytes);
    assert_eq!(pdfium_mismatched_first, pdfium_mismatched_second);
    assert_eq!(
        pdfium_mismatched_first,
        b"{\"schema\":1,\"page_count\":4}\n"
    );

    println!("pdfium_revision={PDFIUM_REVISION}");
    println!("helper_sha256={}", hex(&helper_hash));
    println!("helper_bytes={}", helper_bytes.len());
    println!("build_flags_sha256={}", hex(&build_flags_hash));
    println!(
        "build_args_command_sha256={}",
        hex(&build_args_command_hash)
    );
    println!("environment_sha256={}", hex(&environment_hash));
    println!("invocation_sha256={}", hex(&invocation_hash));
    println!("license_manifest_sha256={}", hex(&license_manifest_hash));
    println!("fonts_sha256={}", hex(&fonts_hash));
    println!("color_sha256={}", hex(&color_hash));
    println!("descriptor_identity_sha256={}", hex(&descriptor_hash));
    println!(
        "single_fixture_sha256={}",
        hex(&sha256(&single.bytes).unwrap())
    );
    println!("single_fixture_bytes={}", single.bytes.len());
    println!(
        "nested_three_page_fixture_sha256={}",
        hex(&sha256(&nested.bytes).unwrap())
    );
    println!("nested_three_page_fixture_bytes={}", nested.bytes.len());
    println!(
        "mismatched_root_count_fixture_sha256={}",
        hex(&sha256(&mismatched.bytes).unwrap())
    );
    println!(
        "mismatched_root_count_fixture_bytes={}",
        mismatched.bytes.len()
    );
    println!(
        "single_page_count_json_sha256={}",
        hex(&sha256(&single_first).unwrap())
    );
    println!(
        "nested_page_count_json_sha256={}",
        hex(&sha256(&nested_first).unwrap())
    );
    println!(
        "mismatched_page_count_json_sha256={}",
        hex(&sha256(&pdfium_mismatched_first).unwrap())
    );
    println!("valid_repeat_outputs_identical=true");
    println!("valid_page_count_subset_exact=true");
    println!("different_valid_page_count_records=0");
    println!("mismatched_root_count_native=PageTreeCountMismatch");
    println!(
        "mismatched_root_count_native_diagnostic={}",
        native_error.diagnostic_id()
    );
    println!("mismatched_root_count_pdfium=page-count-4-produced");
    println!("mismatched_root_count_classification=expected-strictness-difference");
    println!("native_engine_exercised=true");
    println!("pdfium_engine_exercised=true");
    println!("native_vs_pdfium_differential=true");
    println!("baseline_registration_eligible=false");
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

fn single_page_fixture() -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()),
            (
                2,
                b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n"
                    .to_vec(),
            ),
        ],
        4,
    )
}

fn nested_three_page_fixture(root_count: u32) -> Fixture {
    fixture(
        &[
            (1, b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec()),
            (
                2,
                format!(
                    "2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count {root_count} >>\nendobj\n"
                )
                .into_bytes(),
            ),
            (
                3,
                b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n"
                    .to_vec(),
            ),
            (
                4,
                b"4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 >>\nendobj\n"
                    .to_vec(),
            ),
            (
                5,
                b"5 0 obj\n<< /Type /Page /Parent 4 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n"
                    .to_vec(),
            ),
            (
                6,
                b"6 0 obj\n<< /Type /Page /Parent 4 0 R /MediaBox [0 0 200 200] /Resources << >> >>\nendobj\n"
                    .to_vec(),
            ),
        ],
        7,
    )
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
            SourceRevision::new(53),
        ),
        Some(u64::try_from(bytes.len()).unwrap()),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xe4; 32]),
    );
    Fixture { bytes, snapshot }
}

fn native_page_count(fixture: &Fixture) -> Result<u64, Box<DocumentError>> {
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
        PageCountPoll::Ready(count) => Ok(count.page_count()),
        PageCountPoll::Pending { .. } => panic!("complete fixture must not suspend"),
        PageCountPoll::Failed(error) => Err(Box::new(error)),
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
            JobId::new(4_501),
            ResumeCheckpoint::new(4_502),
            ResumeCheckpoint::new(4_503),
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
