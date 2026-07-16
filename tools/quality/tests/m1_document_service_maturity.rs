#[allow(dead_code)]
mod m1_document_service_support;

use std::hint::black_box;
use std::time::Instant;

use m1_document_service_support::{
    OutlineSummary, PageSummary, ServiceOutcome, ServiceResult, canonical_service_json,
    contract_from_manifest, reference_result, run_native_bytes,
};
use pdf_rs_digest::{hex_digest, sha256};

const NESTED_HOLDOUT_INPUT: &[u8] =
    include_bytes!("../../../tests/cases/document/m1-maturity-holdout/nested-four-pages/input.pdf");
const NESTED_HOLDOUT_MANIFEST: &str =
    include_str!("../../../tests/cases/document/m1-maturity-holdout/nested-four-pages/case.toml");
const NESTED_HOLDOUT_EXPECTED: &[u8] = include_bytes!(
    "../../../tests/cases/document/m1-maturity-holdout/nested-four-pages/expected/service.json"
);
const SEALED_HOLDOUT_INPUT: &[u8] = include_bytes!(
    "../../../tests/cases/document/m1-maturity-holdout/flat-two-pages-single-outline/input.pdf"
);
const SEALED_HOLDOUT_MANIFEST: &str = include_str!(
    "../../../tests/cases/document/m1-maturity-holdout/flat-two-pages-single-outline/case.toml"
);
const SEALED_HOLDOUT_EXPECTED: &[u8] = include_bytes!(
    "../../../tests/cases/document/m1-maturity-holdout/flat-two-pages-single-outline/expected/service.json"
);
const ILL_TYPED_ADJUDICATION_INPUT: &[u8] = include_bytes!(
    "../../../tests/cases/document/m1-adjudication/ill-typed-optional-references/input.pdf"
);
const ILL_TYPED_ADJUDICATION_MANIFEST: &str = include_str!(
    "../../../tests/cases/document/m1-adjudication/ill-typed-optional-references/case.toml"
);
const ILL_TYPED_ADJUDICATION_EXPECTED: &[u8] = include_bytes!(
    "../../../tests/cases/document/m1-adjudication/ill-typed-optional-references/expected/service.json"
);
const MATURITY_RUNNER: &str = "tools/quality::m1_document_service_maturity";
const CORRECTION_COMMIT: &str = "a5ec35b";
const PRE_FIX_OUTPUT_SHA256: &str =
    "sha256:a9769304a688face7c4d3256d4cdcf1770ff637cb25848619c4c03205f03aa8d";
const NATIVE_OUTPUT_SHA256: &str =
    "sha256:961a8fee74720e4c04f8ac8f50587b5ad8c836801c942c31a1aa2aa46f5c32dc";
const REFERENCE_OUTPUT_SHA256: &str =
    "sha256:0f7c1caf0e137625c3e104130c42e6525d602bdb2a8d63ff5c8b45420faf9837";

#[test]
fn independent_holdout_page_count_budget_boundary_replays() {
    let exact = contract_from_manifest(NESTED_HOLDOUT_MANIFEST, MATURITY_RUNNER);
    assert_case_hashes(NESTED_HOLDOUT_INPUT, NESTED_HOLDOUT_EXPECTED, &exact);
    let first = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &exact)
        .expect("exact object budget admits the independent holdout session");
    let second = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &exact)
        .expect("exact independent holdout budget replay is stable");
    assert_eq!(first, second);

    let one_less_manifest = NESTED_HOLDOUT_MANIFEST.replace("max_objects = 10", "max_objects = 9");
    let one_less = contract_from_manifest(&one_less_manifest, MATURITY_RUNNER);
    assert_eq!(
        run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &one_less),
        Err("RPE-XREF-0019")
    );
}

#[test]
fn independent_holdout_full_session_matches_reference_and_content_address() {
    let contract = contract_from_manifest(NESTED_HOLDOUT_MANIFEST, MATURITY_RUNNER);
    assert_case_hashes(NESTED_HOLDOUT_INPUT, NESTED_HOLDOUT_EXPECTED, &contract);
    let reference = reference_result(NESTED_HOLDOUT_INPUT, &contract);
    assert_eq!(
        canonical_service_json(&reference).as_bytes(),
        NESTED_HOLDOUT_EXPECTED,
        "the independent graph oracle must remain content-addressed"
    );
    let first = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &contract)
        .expect("independent Native holdout succeeds");
    let second = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &contract)
        .expect("independent Native holdout replay succeeds");
    assert_eq!(first, reference);
    assert_eq!(second, reference);
}

#[test]
fn sealed_second_holdout_matches_reference_and_content_address() {
    let contract = contract_from_manifest(SEALED_HOLDOUT_MANIFEST, MATURITY_RUNNER);
    assert_case_hashes(SEALED_HOLDOUT_INPUT, SEALED_HOLDOUT_EXPECTED, &contract);
    let reference = reference_result(SEALED_HOLDOUT_INPUT, &contract);
    assert_eq!(
        canonical_service_json(&reference).as_bytes(),
        SEALED_HOLDOUT_EXPECTED,
        "the sealed graph oracle must remain content-addressed"
    );
    let first = run_native_bytes(0xe3, SEALED_HOLDOUT_INPUT, &contract)
        .expect("sealed independent Native holdout succeeds");
    let second = run_native_bytes(0xe3, SEALED_HOLDOUT_INPUT, &contract)
        .expect("sealed independent Native holdout replay succeeds");
    assert_eq!(first, reference);
    assert_eq!(second, reference);
}

#[test]
fn same_input_ill_typed_optional_reference_adjudication_replays() {
    let contract = contract_from_manifest(ILL_TYPED_ADJUDICATION_MANIFEST, MATURITY_RUNNER);
    assert_case_hashes(
        ILL_TYPED_ADJUDICATION_INPUT,
        ILL_TYPED_ADJUDICATION_EXPECTED,
        &contract,
    );
    let native = run_native_bytes(0xe2, ILL_TYPED_ADJUDICATION_INPUT, &contract)
        .expect("ill-typed optional values are service-level failures");
    let independent_reference = reference_result(ILL_TYPED_ADJUDICATION_INPUT, &contract);
    let frozen_pre_fix_projection = ServiceResult {
        page_count: ServiceOutcome::Ready(PageSummary { page_count: 1 }),
        outline: ServiceOutcome::Ready(OutlineSummary {
            root_object_number: None,
            root_count: None,
            visible_items: 0,
            items: Vec::new(),
        }),
    };

    println!("same_input_native={native:?}");
    println!("same_input_reference={independent_reference:?}");
    assert_eq!(CORRECTION_COMMIT, "a5ec35b");
    assert!(matches!(native.page_count, ServiceOutcome::Failed(_)));
    assert!(matches!(native.outline, ServiceOutcome::Failed(_)));
    assert!(matches!(
        independent_reference.page_count,
        ServiceOutcome::Failed("RPE-REFERENCE-0002")
    ));
    assert!(matches!(
        independent_reference.outline,
        ServiceOutcome::Failed("RPE-REFERENCE-0002")
    ));
    assert_ne!(native, frozen_pre_fix_projection);
    assert_ne!(independent_reference, frozen_pre_fix_projection);
    assert_eq!(
        digest_json(&canonical_service_json(&frozen_pre_fix_projection)),
        PRE_FIX_OUTPUT_SHA256
    );
    assert_eq!(
        digest_json(&canonical_service_json(&native)),
        NATIVE_OUTPUT_SHA256
    );
    assert_eq!(
        digest_json(&canonical_service_json(&independent_reference)),
        REFERENCE_OUTPUT_SHA256
    );
    assert_eq!(
        canonical_service_json(&native).as_bytes(),
        ILL_TYPED_ADJUDICATION_EXPECTED,
        "the corrected Native dual-failure output must remain content-addressed"
    );
}

#[test]
fn native_document_service_benchmark_emits_positive_same_scope_samples() {
    let contract = contract_from_manifest(NESTED_HOLDOUT_MANIFEST, MATURITY_RUNNER);
    let expected = reference_result(NESTED_HOLDOUT_INPUT, &contract);
    let warmup = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &contract)
        .expect("benchmark warmup holdout session succeeds");
    assert_eq!(warmup, expected, "untimed warmup must match reference");
    black_box(warmup);
    let mut samples = Vec::with_capacity(21);
    for _ in 0..21 {
        let start = Instant::now();
        let result = run_native_bytes(0xe1, NESTED_HOLDOUT_INPUT, &contract)
            .expect("benchmark holdout session succeeds");
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        assert_eq!(result, expected, "every timed result must match reference");
        black_box(result);
        samples.push(elapsed.max(1));
    }
    assert_eq!(samples.len(), 21);
    assert!(samples.iter().all(|sample| *sample > 0));
    println!("m1_native_document_service_samples_ns={samples:?}");
}

fn assert_case_hashes(
    input: &[u8],
    expected: &[u8],
    contract: &m1_document_service_support::CaseContract,
) {
    assert_eq!(
        format!(
            "sha256:{}",
            hex_digest(&sha256(input).expect("bounded input fits digest framing"))
        ),
        contract.input_sha256
    );
    assert_eq!(
        format!(
            "sha256:{}",
            hex_digest(&sha256(expected).expect("bounded expected output fits digest framing"))
        ),
        contract.expected_sha256
    );
    assert!(input.len() <= usize::try_from(contract.max_input_bytes).unwrap());
}

fn digest_json(json: &str) -> String {
    format!(
        "sha256:{}",
        hex_digest(
            &sha256(json.as_bytes()).expect("bounded adjudication output fits digest framing")
        )
    )
}
