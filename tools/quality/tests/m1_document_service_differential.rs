mod m1_document_service_support;

use m1_document_service_support::{
    CASES, CaseKind, ServiceOutcome, build_fixture, canonical_service_json, case_contract,
    case_contract_from_manifest, reference_result, repository_expected, repository_input,
    repository_manifest, run_native,
};
use pdf_rs_digest::{hex_digest, sha256};

#[test]
fn full_m1_session_page_count_and_outline_match_independent_case_oracles() {
    for case in CASES {
        let contract = case_contract(case);
        let fixture = build_fixture(case);
        assert_eq!(
            fixture.bytes,
            repository_input(case),
            "{} input is pinned",
            case.id
        );
        let digest = sha256(&fixture.bytes).expect("bounded fixture fits digest framing");
        let digest = hex_digest(&digest);
        assert_eq!(
            format!("sha256:{digest}"),
            contract.input_sha256,
            "{} input hash is pinned",
            case.id
        );
        assert!(
            fixture.bytes.len() <= usize::try_from(contract.max_input_bytes).unwrap(),
            "{} input must fit its declared source budget",
            case.id
        );

        let expected_bytes = repository_expected(case);
        let expected_digest =
            sha256(expected_bytes).expect("bounded expected JSON fits digest framing");
        let expected_digest = hex_digest(&expected_digest);
        assert_eq!(
            format!("sha256:{expected_digest}"),
            contract.expected_sha256,
            "{} expected-service hash is pinned",
            case.id
        );

        let expected = reference_result(repository_input(case), &contract);
        assert_eq!(
            canonical_service_json(&expected).as_bytes(),
            expected_bytes,
            "{} reviewed expected JSON must match the independent case model",
            case.id
        );
        let first = run_native(case, &contract).expect("canonical case opens strictly");
        let second = run_native(case, &contract).expect("canonical replay opens strictly");
        assert_eq!(first, expected, "{} must match its O0/O1 oracle", case.id);
        assert_eq!(second, expected, "{} replay must be deterministic", case.id);
        for result in [&first, &second] {
            if let m1_document_service_support::ServiceOutcome::Ready(page) = &result.page_count {
                assert!(page.page_count <= contract.max_pages);
            }
            if let m1_document_service_support::ServiceOutcome::Ready(outline) = &result.outline {
                assert!(u64::try_from(outline.items.len()).unwrap() <= contract.max_outline_items);
            }
        }
        assert_eq!(
            canonical_service_json(&first).as_bytes(),
            expected_bytes,
            "{} Native service output must match the content-addressed expectation",
            case.id
        );
        assert_eq!(
            first, second,
            "{} Native replay must be byte-stable",
            case.id
        );
    }
}

#[test]
fn native_open_consumes_the_manifest_object_count_budget() {
    let case = CASES
        .into_iter()
        .find(|case| case.kind == CaseKind::NestedValid)
        .expect("nested service case is registered");
    let exact_manifest = repository_manifest(case).replace("max_objects = 16", "max_objects = 10");
    let exact = case_contract_from_manifest(case, &exact_manifest);
    assert!(run_native(case, &exact).is_ok());

    let one_less_manifest =
        repository_manifest(case).replace("max_objects = 16", "max_objects = 9");
    let one_less = case_contract_from_manifest(case, &one_less_manifest);
    assert_eq!(run_native(case, &one_less), Err("RPE-XREF-0019"));
}

#[test]
fn native_page_traversal_consumes_the_manifest_depth_budget() {
    let case = CASES
        .into_iter()
        .find(|case| case.kind == CaseKind::NestedValid)
        .expect("nested service case is registered");
    let exact_manifest =
        repository_manifest(case).replace("max_resolve_depth = 8", "max_resolve_depth = 3");
    let exact = case_contract_from_manifest(case, &exact_manifest);
    assert!(matches!(
        run_native(case, &exact)
            .expect("three-level page tree fits exact depth")
            .page_count,
        ServiceOutcome::Ready(_)
    ));

    let one_less_manifest =
        repository_manifest(case).replace("max_resolve_depth = 8", "max_resolve_depth = 2");
    let one_less = case_contract_from_manifest(case, &one_less_manifest);
    assert_eq!(
        run_native(case, &one_less)
            .expect("strict open succeeds before page traversal")
            .page_count,
        ServiceOutcome::Failed("RPE-DOCUMENT-0002")
    );
}
