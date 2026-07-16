use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStoreLimitConfig, RangeStoreLimits, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId,
    SourceValidator, SourceValidatorKind,
};
use pdf_rs_cache::{ReadyStoreEpoch, ReadyStoreSessionId};
use pdf_rs_document::{
    DocumentLimitConfig, DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob,
    OutlineJobContext, OutlineLimitConfig, OutlineLimits, OutlineTargetKind, PageTreeJobContext,
    PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext,
    RevisionAttestationLimitConfig, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenError, StrictBaseOpenLimits,
};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_quality::manifest::validate_manifest;
use pdf_rs_session::{
    M1RequestId, M1RequestIdentity, M1Service, M1ServiceFailure, M1SessionClose, M1SessionFailure,
    M1SessionIngress, M1SessionPhase, M1SessionRun, M1SessionWait, M1StrictDocumentSession,
    RangeResumeGeneration, StrictBaseOpenCoordinatorFailure,
};
use pdf_rs_syntax::{SyntaxLimitConfig, SyntaxLimits};
use pdf_rs_xref::{XrefJobContext, XrefLimitConfig, XrefLimits};

const GENERATION: RangeResumeGeneration = RangeResumeGeneration::new(0xd1);
const OPEN_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xd1_01), JobId::new(0xd1_02), GENERATION);
const PAGE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xd1_11), JobId::new(0xd1_12), GENERATION);
const OUTLINE_REQUEST: M1RequestIdentity =
    M1RequestIdentity::new(M1RequestId::new(0xd1_21), JobId::new(0xd1_22), GENERATION);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseKind {
    SinglePageNoOutline,
    NestedValid,
    MismatchedRootPageCount,
    WrongOutlinePrev,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceCase {
    pub id: &'static str,
    pub seed: u8,
    pub kind: CaseKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseContract {
    pub input_sha256: String,
    pub expected_sha256: String,
    pub oracle_level: String,
    pub max_input_bytes: u64,
    pub max_objects: u64,
    pub max_resolve_depth: u64,
    pub max_stream_output_bytes: u64,
    pub max_total_decode_bytes: u64,
    pub operator_fuel: u64,
    pub decode_fuel: u64,
    pub max_pages: u64,
    pub max_outline_items: u64,
    pub max_range_resident_bytes: u64,
}

pub const CASES: [ServiceCase; 4] = [
    ServiceCase {
        id: "document/m1-services/single-page-no-outline",
        seed: 0xd1,
        kind: CaseKind::SinglePageNoOutline,
    },
    ServiceCase {
        id: "document/m1-services/nested-valid",
        seed: 0xd2,
        kind: CaseKind::NestedValid,
    },
    ServiceCase {
        id: "document/m1-services/page-count-mismatch",
        seed: 0xd3,
        kind: CaseKind::MismatchedRootPageCount,
    },
    ServiceCase {
        id: "document/m1-services/outline-wrong-prev",
        seed: 0xd4,
        kind: CaseKind::WrongOutlinePrev,
    },
];

pub fn repository_input(case: ServiceCase) -> &'static [u8] {
    match case.kind {
        CaseKind::SinglePageNoOutline => include_bytes!(
            "../../../../tests/cases/document/m1-services/single-page-no-outline/input.pdf"
        ),
        CaseKind::NestedValid => {
            include_bytes!("../../../../tests/cases/document/m1-services/nested-valid/input.pdf")
        }
        CaseKind::MismatchedRootPageCount => include_bytes!(
            "../../../../tests/cases/document/m1-services/page-count-mismatch/input.pdf"
        ),
        CaseKind::WrongOutlinePrev => include_bytes!(
            "../../../../tests/cases/document/m1-services/outline-wrong-prev/input.pdf"
        ),
    }
}

pub fn repository_manifest(case: ServiceCase) -> &'static str {
    match case.kind {
        CaseKind::SinglePageNoOutline => include_str!(
            "../../../../tests/cases/document/m1-services/single-page-no-outline/case.toml"
        ),
        CaseKind::NestedValid => {
            include_str!("../../../../tests/cases/document/m1-services/nested-valid/case.toml")
        }
        CaseKind::MismatchedRootPageCount => include_str!(
            "../../../../tests/cases/document/m1-services/page-count-mismatch/case.toml"
        ),
        CaseKind::WrongOutlinePrev => include_str!(
            "../../../../tests/cases/document/m1-services/outline-wrong-prev/case.toml"
        ),
    }
}

pub fn repository_expected(case: ServiceCase) -> &'static [u8] {
    match case.kind {
        CaseKind::SinglePageNoOutline => include_bytes!(
            "../../../../tests/cases/document/m1-services/single-page-no-outline/expected/service.json"
        ),
        CaseKind::NestedValid => include_bytes!(
            "../../../../tests/cases/document/m1-services/nested-valid/expected/service.json"
        ),
        CaseKind::MismatchedRootPageCount => include_bytes!(
            "../../../../tests/cases/document/m1-services/page-count-mismatch/expected/service.json"
        ),
        CaseKind::WrongOutlinePrev => include_bytes!(
            "../../../../tests/cases/document/m1-services/outline-wrong-prev/expected/service.json"
        ),
    }
}

pub fn case_contract(case: ServiceCase) -> CaseContract {
    case_contract_from_manifest(case, repository_manifest(case))
}

pub fn case_contract_from_manifest(case: ServiceCase, input: &str) -> CaseContract {
    let contract = contract_from_manifest(input, "tools/quality::m1_document_service_differential");
    let manifest = validate_manifest(input).expect("document-service case manifest validates");
    assert_eq!(manifest.case_id(), case.id);
    let oracle_level = manifest
        .string("oracle", "level")
        .expect("validated manifest has an oracle level");
    let expected_oracle = match case.kind {
        CaseKind::SinglePageNoOutline => "O0",
        CaseKind::NestedValid | CaseKind::MismatchedRootPageCount | CaseKind::WrongOutlinePrev => {
            "O1"
        }
    };
    assert_eq!(oracle_level, expected_oracle);

    contract
}

pub fn contract_from_manifest(input: &str, required_runner: &str) -> CaseContract {
    let manifest = validate_manifest(input).expect("document-service case manifest validates");
    assert_eq!(
        manifest.string("expected", "service"),
        Some("expected/service.json")
    );
    assert!(
        manifest
            .string_array("runners", "native")
            .expect("validated manifest has Native runners")
            .contains(&required_runner)
    );
    let oracle_level = manifest
        .string("oracle", "level")
        .expect("validated manifest has an oracle level");

    CaseContract {
        input_sha256: manifest.source_sha256().to_owned(),
        expected_sha256: manifest
            .string("expected", "service_sha256")
            .expect("validated service manifest has an expected hash")
            .to_owned(),
        oracle_level: oracle_level.to_owned(),
        max_input_bytes: required_budget(&manifest, "max_input_bytes"),
        max_objects: required_budget(&manifest, "max_objects"),
        max_resolve_depth: required_budget(&manifest, "max_resolve_depth"),
        max_stream_output_bytes: required_budget(&manifest, "max_stream_output_bytes"),
        max_total_decode_bytes: required_budget(&manifest, "max_total_decode_bytes"),
        operator_fuel: required_budget(&manifest, "operator_fuel"),
        decode_fuel: required_budget(&manifest, "decode_fuel"),
        max_pages: required_budget(&manifest, "max_pages"),
        max_outline_items: required_budget(&manifest, "max_outline_items"),
        max_range_resident_bytes: required_budget(&manifest, "max_range_resident_bytes"),
    }
}

fn required_budget(manifest: &pdf_rs_quality::manifest::CaseManifest, key: &str) -> u64 {
    manifest
        .positive_u64("budget", key)
        .unwrap_or_else(|| panic!("validated document-service manifest has budget.{key}"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceOutcome<T> {
    Ready(T),
    Failed(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageSummary {
    pub page_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineItemSummary {
    pub object_number: u32,
    pub parent_index: Option<usize>,
    pub depth: u64,
    pub title: String,
    pub declared_count: Option<i64>,
    pub target_kind: &'static str,
    pub direct_children: u64,
    pub visible_descendants_if_open: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineSummary {
    pub root_object_number: Option<u32>,
    pub root_count: Option<u64>,
    pub visible_items: u64,
    pub items: Vec<OutlineItemSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceResult {
    pub page_count: ServiceOutcome<PageSummary>,
    pub outline: ServiceOutcome<OutlineSummary>,
}

pub fn canonical_service_json(result: &ServiceResult) -> String {
    let mut output = String::from("{");
    match &result.outline {
        ServiceOutcome::Ready(outline) => {
            output.push_str("\"outline\":{\"items\":[");
            for (index, item) in outline.items.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str("{\"count\":");
                push_optional_i64(&mut output, item.declared_count);
                output.push_str(",\"depth\":");
                output.push_str(&item.depth.to_string());
                output.push_str(",\"direct_children\":");
                output.push_str(&item.direct_children.to_string());
                output.push_str(",\"object\":");
                output.push_str(&item.object_number.to_string());
                output.push_str(",\"parent_index\":");
                match item.parent_index {
                    Some(parent) => output.push_str(&parent.to_string()),
                    None => output.push_str("null"),
                }
                output.push_str(",\"target_kind\":");
                push_json_string(&mut output, item.target_kind);
                output.push_str(",\"title\":");
                push_json_string(&mut output, &item.title);
                output.push_str(",\"visible_descendants_if_open\":");
                output.push_str(&item.visible_descendants_if_open.to_string());
                output.push('}');
            }
            output.push_str("],\"root_count\":");
            push_optional_u64(&mut output, outline.root_count);
            output.push_str(",\"root_object_number\":");
            match outline.root_object_number {
                Some(number) => output.push_str(&number.to_string()),
                None => output.push_str("null"),
            }
            output.push_str(",\"visible_items\":");
            output.push_str(&outline.visible_items.to_string());
            output.push('}');
        }
        ServiceOutcome::Failed(diagnostic) => {
            output.push_str("\"outline_error\":");
            push_json_string(&mut output, diagnostic);
        }
    }
    output.push(',');
    match &result.page_count {
        ServiceOutcome::Ready(page) => {
            output.push_str("\"page_count\":");
            output.push_str(&page.page_count.to_string());
        }
        ServiceOutcome::Failed(diagnostic) => {
            output.push_str("\"page_count_error\":");
            push_json_string(&mut output, diagnostic);
        }
    }
    output.push('}');
    output.push('\n');
    output
}

fn push_optional_i64(output: &mut String, value: Option<i64>) {
    match value {
        Some(value) => output.push_str(&value.to_string()),
        None => output.push_str("null"),
    }
}

fn push_optional_u64(output: &mut String, value: Option<u64>) {
    match value {
        Some(value) => output.push_str(&value.to_string()),
        None => output.push_str("null"),
    }
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

pub struct Fixture {
    pub bytes: Vec<u8>,
    pub snapshot: SourceSnapshot,
}

pub fn build_fixture(case: ServiceCase) -> Fixture {
    let objects = match case.kind {
        CaseKind::SinglePageNoOutline => single_page_objects(),
        CaseKind::NestedValid => nested_objects(false, false),
        CaseKind::MismatchedRootPageCount => nested_objects(true, false),
        CaseKind::WrongOutlinePrev => nested_objects(false, true),
    };
    build_fixture_from_objects(case.seed, &objects)
}

pub fn build_fixture_from_objects(seed: u8, objects: &[(u32, String)]) -> Fixture {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (number, body) in objects {
        offsets.push((
            *number,
            u64::try_from(bytes.len()).expect("fixture offset fits u64"),
        ));
        bytes.extend_from_slice(body.as_bytes());
    }
    let startxref = u64::try_from(bytes.len()).expect("fixture length fits u64");
    let size = objects
        .last()
        .map(|(number, _)| number + 1)
        .expect("fixture has at least one object");
    bytes.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for number in 0..size {
        let row = if number == 0 {
            "0000000000 65535 f \n".to_owned()
        } else {
            let offset = offsets
                .iter()
                .find(|(candidate, _)| *candidate == number)
                .map(|(_, offset)| *offset)
                .expect("fixture object numbers are dense");
            format!("{offset:010} 00000 n \n")
        };
        assert_eq!(row.len(), 20);
        bytes.extend_from_slice(row.as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n")
            .as_bytes(),
    );
    fixture_from_bytes(seed, bytes)
}

pub fn fixture_from_bytes(seed: u8, bytes: Vec<u8>) -> Fixture {
    let len = u64::try_from(bytes.len()).expect("fixture length fits u64");
    Fixture {
        bytes,
        snapshot: SourceSnapshot::new(
            SourceIdentity::new(
                SourceStableId::new([seed; 32]),
                SourceRevision::new(u64::from(seed)),
            ),
            Some(len),
            SourceValidator::new(
                SourceValidatorKind::FrozenResponse,
                [seed.wrapping_add(1); 32],
            ),
        ),
    }
}

pub fn run_native(
    case: ServiceCase,
    contract: &CaseContract,
) -> Result<ServiceResult, &'static str> {
    let fixture = build_fixture(case);
    run_native_fixture(case.seed, &fixture, contract)
}

#[allow(dead_code)]
pub fn run_native_bytes(
    seed: u8,
    bytes: &[u8],
    contract: &CaseContract,
) -> Result<ServiceResult, &'static str> {
    let fixture = fixture_from_bytes(seed, bytes.to_vec());
    run_native_fixture(seed, &fixture, contract)
}

fn run_native_fixture(
    seed: u8,
    fixture: &Fixture,
    contract: &CaseContract,
) -> Result<ServiceResult, &'static str> {
    let mut session = M1StrictDocumentSession::new(
        ReadyStoreSessionId::new(0xd1_0000 + u64::from(seed)),
        OPEN_REQUEST,
        strict_job(fixture, contract),
        range_limits(contract),
        ReadyStoreEpoch::new(1),
        Default::default(),
    )
    .expect("M1 service session limits validate");
    drive_open_ready(&mut session, fixture, contract)?;
    session
        .request_page_count(PAGE_REQUEST, page_context(), page_limits(contract))
        .expect("the page-count slot is available");
    session
        .request_outline(OUTLINE_REQUEST, outline_context(), outline_limits(contract))
        .expect("the outline slot is available");

    let mut page_count = None;
    let mut outline = None;
    while page_count.is_none() || outline.is_none() {
        assert_session_budget(&session, contract);
        match session.run_one(&NeverCancelled) {
            M1SessionRun::PageCountReady { request, result } => {
                assert_eq!(request, PAGE_REQUEST);
                page_count = Some(ServiceOutcome::Ready(PageSummary {
                    page_count: result.page_count(),
                }));
            }
            M1SessionRun::OutlineReady { request, result } => {
                assert_eq!(request, OUTLINE_REQUEST);
                outline = Some(ServiceOutcome::Ready(OutlineSummary {
                    root_object_number: result.root().map(|reference| reference.number()),
                    root_count: result.root_count(),
                    visible_items: result.visible_items(),
                    items: result
                        .items()
                        .iter()
                        .map(|item| OutlineItemSummary {
                            object_number: item.reference().number(),
                            parent_index: item.parent_index(),
                            depth: item.depth(),
                            title: item.title().to_owned(),
                            declared_count: item.declared_count(),
                            target_kind: match item.target_kind() {
                                OutlineTargetKind::None => "none",
                                OutlineTargetKind::Destination => "destination",
                                OutlineTargetKind::Action => "action",
                            },
                            direct_children: item.direct_children(),
                            visible_descendants_if_open: item.visible_descendants_if_open(),
                        })
                        .collect(),
                }));
            }
            M1SessionRun::RequestFailed {
                service,
                request,
                failure: M1ServiceFailure::Document(error),
            } => match service {
                M1Service::PageCount => {
                    assert_eq!(request, PAGE_REQUEST);
                    page_count = Some(ServiceOutcome::Failed(error.diagnostic_id()));
                }
                M1Service::Outline => {
                    assert_eq!(request, OUTLINE_REQUEST);
                    outline = Some(ServiceOutcome::Failed(error.diagnostic_id()));
                }
            },
            M1SessionRun::WaitingForData { missing, .. } => {
                supply_reverse(&mut session, fixture, missing, contract);
            }
            other => panic!("both bounded services must reach a terminal result: {other:?}"),
        }
    }

    assert_eq!(session.close(), M1SessionClose::Queued);
    let report = match session.run_one(&NeverCancelled) {
        M1SessionRun::Closed(report) => report,
        other => panic!("the differential session must close on its own actor turn: {other:?}"),
    };
    assert_eq!(report.previous_phase(), M1SessionPhase::Ready);
    assert_eq!(session.resources().resident_bytes(), 0);
    assert_eq!(session.resources().service_jobs(), 0);
    Ok(ServiceResult {
        page_count: page_count.expect("page count reached a terminal result"),
        outline: outline.expect("outline reached a terminal result"),
    })
}

fn single_page_objects() -> Vec<(u32, String)> {
    vec![
        (
            1,
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_owned(),
        ),
        (
            2,
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_owned(),
        ),
        (
            3,
            "3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n".to_owned(),
        ),
    ]
}

fn nested_objects(mismatched_page_count: bool, wrong_prev: bool) -> Vec<(u32, String)> {
    let root_page_count = if mismatched_page_count { 4 } else { 3 };
    let second_prev = if wrong_prev { 9 } else { 8 };
    vec![
        (1, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R >>\nendobj\n".to_owned()),
        (2, format!("2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count {root_page_count} >>\nendobj\n")),
        (3, "3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n".to_owned()),
        (4, "4 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [5 0 R 6 0 R] /Count 2 >>\nendobj\n".to_owned()),
        (5, "5 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n".to_owned()),
        (6, "6 0 obj\n<< /Type /Page /Parent 4 0 R >>\nendobj\n".to_owned()),
        (7, "7 0 obj\n<< /Type /Outlines /First 8 0 R /Last 10 0 R /Count 3 >>\nendobj\n".to_owned()),
        (8, "8 0 obj\n<< /Title (Alpha) /Parent 7 0 R /Next 10 0 R /First 9 0 R /Last 9 0 R /Count 1 >>\nendobj\n".to_owned()),
        (9, "9 0 obj\n<< /Title (Child) /Parent 8 0 R >>\nendobj\n".to_owned()),
        (10, format!("10 0 obj\n<< /Title (Beta) /Parent 7 0 R /Prev {second_prev} 0 R >>\nendobj\n")),
    ]
}

fn strict_job(fixture: &Fixture, contract: &CaseContract) -> OpenStrictBaseRevisionJob {
    OpenStrictBaseRevisionJob::new(
        fixture.snapshot,
        RevisionId::new(0xd1),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0xd1_03),
                ResumeCheckpoint::new(0xd1_04),
            ),
            RevisionAttestationJobContext::new(
                OPEN_REQUEST.job(),
                ResumeCheckpoint::new(0xd1_05),
                ResumeCheckpoint::new(0xd1_06),
                ResumeCheckpoint::new(0xd1_07),
                RequestPriority::Metadata,
            ),
        ),
        StrictBaseOpenLimits::new(
            xref_limits(contract),
            document_limits(contract),
            attestation_limits(contract),
            object_limits(contract),
            syntax_limits(contract),
        ),
    )
    .expect("self-authored strict fixture job validates")
}

fn page_context() -> PageTreeJobContext {
    PageTreeJobContext::new(
        PAGE_REQUEST.job(),
        ResumeCheckpoint::new(0xd1_13),
        ResumeCheckpoint::new(0xd1_14),
        RequestPriority::Metadata,
    )
}

fn outline_context() -> OutlineJobContext {
    OutlineJobContext::new(
        OUTLINE_REQUEST.job(),
        ResumeCheckpoint::new(0xd1_23),
        ResumeCheckpoint::new(0xd1_24),
        RequestPriority::Metadata,
    )
}

fn range_limits(contract: &CaseContract) -> RangeStoreLimits {
    RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_read_bytes: contract.max_input_bytes,
        max_cached_bytes: contract.max_input_bytes,
        max_resident_bytes: contract.max_range_resident_bytes,
        ..RangeStoreLimitConfig::default()
    })
    .expect("case Range budgets satisfy hard ceilings")
}

fn xref_limits(contract: &CaseContract) -> XrefLimits {
    let entries = checked_add(contract.max_objects, 1, "xref free row");
    let source_work = checked_product(contract.max_input_bytes, 2, "xref source work");
    XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_tail_bytes: contract.max_input_bytes,
        max_tail_bytes: contract.max_input_bytes,
        initial_section_bytes: contract.max_input_bytes,
        max_section_bytes: contract.max_input_bytes,
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
        max_subsections: entries,
        max_entries: entries,
    })
    .expect("case xref budgets satisfy hard ceilings")
}

fn document_limits(contract: &CaseContract) -> DocumentLimits {
    let entries = checked_add(contract.max_objects, 1, "document free row");
    DocumentLimits::validate(DocumentLimitConfig {
        max_total_entries: entries,
        max_in_use_entries: contract.max_objects,
        max_logical_index_bytes: object_work_bytes(contract),
        max_sort_steps: checked_product(entries, entries, "document sort work"),
    })
    .expect("case document-index budgets satisfy hard ceilings")
}

fn attestation_limits(contract: &CaseContract) -> RevisionAttestationLimits {
    let object_work = object_work_bytes(contract);
    RevisionAttestationLimits::validate(RevisionAttestationLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        max_objects: contract.max_objects,
        scan_chunk_bytes: contract.max_input_bytes,
        max_trivia_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_evidence_bytes: object_work,
    })
    .expect("case revision-attestation budgets satisfy hard ceilings")
}

fn object_limits(contract: &CaseContract) -> ObjectLimits {
    let source_work = checked_product(contract.max_input_bytes, 2, "object source work");
    ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_envelope_bytes: contract.max_input_bytes,
        max_envelope_bytes: contract.max_input_bytes,
        initial_boundary_bytes: contract.max_input_bytes,
        max_boundary_bytes: contract.max_input_bytes,
        max_stream_bytes: contract
            .max_stream_output_bytes
            .min(contract.max_input_bytes),
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
    })
    .expect("case object budgets satisfy hard ceilings")
}

fn syntax_limits(contract: &CaseContract) -> SyntaxLimits {
    assert!(contract.max_total_decode_bytes >= contract.max_stream_output_bytes);
    let max_container_depth = u16::try_from(contract.max_resolve_depth)
        .expect("case resolve depth fits the syntax hard type");
    SyntaxLimits::validate(SyntaxLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_token_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_name_bytes: contract.max_input_bytes,
        max_string_source_bytes: contract.max_input_bytes,
        max_string_decoded_bytes: contract.max_stream_output_bytes,
        max_owned_bytes: contract
            .max_total_decode_bytes
            .max(contract.max_input_bytes),
        max_total_tokens: contract.operator_fuel.min(contract.decode_fuel),
        max_container_entries: contract.max_objects,
        max_container_bytes: object_work_bytes(contract),
        max_container_depth,
    })
    .expect("case syntax budgets satisfy hard ceilings")
}

fn page_limits(contract: &CaseContract) -> PageTreeLimits {
    let object_work = object_work_bytes(contract);
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: contract.max_objects,
        max_depth: contract.max_resolve_depth,
        max_pages: contract.max_pages,
        max_kids_per_node: contract.max_objects,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_traversal_bytes: object_work,
    })
    .expect("case page-count budgets satisfy hard ceilings")
}

fn outline_limits(contract: &CaseContract) -> OutlineLimits {
    let object_work = object_work_bytes(contract);
    let item_input = checked_product(
        contract.max_input_bytes,
        contract.max_outline_items,
        "outline title input",
    );
    let item_utf8 = checked_product(
        contract.max_total_decode_bytes,
        contract.max_outline_items,
        "outline title UTF-8",
    );
    OutlineLimits::validate(OutlineLimitConfig {
        max_items: contract.max_outline_items,
        max_depth: contract.max_resolve_depth.min(contract.max_outline_items),
        max_siblings_per_level: contract.max_outline_items,
        max_title_input_bytes: contract.max_input_bytes,
        max_title_utf8_bytes: contract.max_total_decode_bytes,
        max_total_title_input_bytes: item_input,
        max_total_title_utf8_bytes: item_utf8,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_bytes: object_work,
    })
    .expect("case outline budgets satisfy hard ceilings")
}

fn object_work_bytes(contract: &CaseContract) -> u64 {
    checked_product(
        contract.max_input_bytes,
        contract.max_objects,
        "per-object source work",
    )
}

fn checked_product(left: u64, right: u64, label: &str) -> u64 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic!("case budget overflow: {label}"))
}

fn checked_add(left: u64, right: u64, label: &str) -> u64 {
    left.checked_add(right)
        .unwrap_or_else(|| panic!("case budget overflow: {label}"))
}

fn assert_session_budget(session: &M1StrictDocumentSession, contract: &CaseContract) {
    assert!(session.resources().resident_bytes() <= contract.max_range_resident_bytes);
}

fn drive_open_ready(
    session: &mut M1StrictDocumentSession,
    fixture: &Fixture,
    contract: &CaseContract,
) -> Result<(), &'static str> {
    loop {
        assert_session_budget(session, contract);
        match session.run_one(&NeverCancelled) {
            M1SessionRun::WaitingForData {
                owner: M1SessionWait::Opening(request),
                missing,
                ..
            } => {
                assert_eq!(request, OPEN_REQUEST);
                supply_reverse(session, fixture, missing, contract);
            }
            M1SessionRun::Ready => break,
            M1SessionRun::Failed(M1SessionFailure::Opening(
                StrictBaseOpenCoordinatorFailure::Parser(error),
            )) => {
                assert_eq!(session.phase(), M1SessionPhase::Failed);
                assert_eq!(session.resources().resident_bytes(), 0);
                return Err(match error {
                    StrictBaseOpenError::Xref(error) => error.diagnostic_id(),
                    StrictBaseOpenError::Document(error) => error.diagnostic_id(),
                });
            }
            other => panic!("self-authored strict fixture must reach Ready: {other:?}"),
        }
    }
    assert_eq!(session.phase(), M1SessionPhase::Ready);
    Ok(())
}

fn supply_reverse(
    session: &mut M1StrictDocumentSession,
    fixture: &Fixture,
    missing: SmallRanges,
    contract: &CaseContract,
) {
    let mut pieces = Vec::new();
    for range in missing.as_slice().iter().copied() {
        if range.len() == 1 {
            pieces.push(range);
        } else {
            let lower_len = range.len() / 2;
            pieces.push(ByteRange::new(range.start(), lower_len).expect("lower split is valid"));
            pieces.push(
                ByteRange::new(range.start() + lower_len, range.len() - lower_len)
                    .expect("upper split is valid"),
            );
        }
    }
    pieces.sort_by_key(|range| std::cmp::Reverse(range.start()));
    let last = pieces
        .len()
        .checked_sub(1)
        .expect("Pending has missing bytes");
    for (index, range) in pieces.into_iter().enumerate() {
        let start = usize::try_from(range.start()).expect("fixture offset fits usize");
        let end = usize::try_from(range.end_exclusive()).expect("fixture offset fits usize");
        let response =
            RangeResponse::new(fixture.snapshot, range, fixture.bytes[start..end].to_vec())
                .expect("fixture response geometry validates");
        match session.supply(response) {
            M1SessionIngress::Accepted { wake_scheduler, .. } => {
                assert_eq!(wake_scheduler, index == last);
            }
            other => panic!("reverse source response must be accepted: {other:?}"),
        }
        assert_session_budget(session, contract);
    }
}
mod reference;

pub use reference::reference_result;
