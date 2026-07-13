use std::fs;
use std::path::PathBuf;

use pdf_rs_corpus::{
    AccessPolicy, CorpusManifestLimits, CorpusTier, RedistributionPolicy, encode_manifest,
    load_manifest_file,
};
use pdf_rs_digest::hex_digest;
use pdf_rs_generate::{
    DSL_SCHEMA, GENERATOR_REVISION, GENERATOR_SCHEMA, GenerateLimits, ONE_PAGE_DSL, compile_dsl,
};

#[test]
fn repository_t0_manifest_binds_the_replayed_generator_object() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest_path = repository.join("tests/corpus/manifests/t0-bootstrap-v1.toml");
    let source_path =
        repository.join("tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl");
    let case_path =
        repository.join("tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml");
    let ledger_path = repository.join("docs/traceability/data-ledger.toml");

    let manifest = load_manifest_file(&manifest_path, CorpusManifestLimits::default()).unwrap();
    let source = fs::read(source_path).unwrap();
    let case = fs::read_to_string(case_path).unwrap();
    let ledger = fs::read_to_string(ledger_path).unwrap();
    assert_eq!(source, ONE_PAGE_DSL.as_bytes());
    let generated = compile_dsl(&source, GenerateLimits::default()).unwrap();
    let source_hash = hex_digest(&generated.source_sha256());
    let output_hash = hex_digest(&generated.output_sha256());
    let manifest_hash = hex_digest(&manifest.source_sha256());

    assert_eq!(manifest.manifest().id(), "t0-bootstrap-v1");
    assert_eq!(manifest.objects().len(), 1);
    let object = &manifest.objects()[0];
    let entry = object.entry();
    assert_eq!(
        object.relative_path(),
        "tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf"
    );
    assert_eq!(entry.id().sha256(), generated.output_sha256());
    assert_eq!(entry.tier(), CorpusTier::T0);
    assert_eq!(entry.page_count(), 1);
    assert_eq!(entry.license().expression(), LICENSE);
    assert_eq!(entry.license().source(), FIXTURE_LEDGER_ID);
    assert_eq!(entry.access(), AccessPolicy::Repository);
    assert_eq!(entry.redistribution(), RedistributionPolicy::Prohibited);
    assert_eq!(object.max_bytes(), 65_536);
    assert_eq!(
        encode_manifest(&manifest, CorpusManifestLimits::default()).unwrap(),
        fs::read(manifest_path).unwrap()
    );

    assert_line(&case, &format!("id = \"{CASE_ID}\""));
    assert_line(
        &case,
        &format!(
            "source = \"tools/generate@{GENERATOR_REVISION} dsl-schema-{DSL_SCHEMA} generator-schema-{GENERATOR_SCHEMA} source-sha256:{source_hash}\""
        ),
    );
    assert_line(&case, &format!("sha256 = \"sha256:{output_hash}\""));
    assert_line(&case, &format!("license = \"{LICENSE}\""));
    assert_line(&case, "redistributable = false");
    assert_line(&case, "access = \"repository\"");
    assert_line(&case, &format!("max_input_bytes = {}", object.max_bytes()));

    let fixture_record = data_record(&ledger, FIXTURE_LEDGER_ID);
    assert_line(
        fixture_record,
        "source = \"tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl\"",
    );
    assert_line(
        fixture_record,
        &format!("source_hash = \"sha256:{source_hash}\""),
    );
    assert_line(
        fixture_record,
        &format!("output_hash = \"sha256:{output_hash}\""),
    );
    assert_line(
        fixture_record,
        &format!("license_expression = \"{LICENSE}\""),
    );
    assert_line(
        fixture_record,
        "redistribution = \"disabled; generated locally and ignored pending project-owner license approval\"",
    );
    assert_line(fixture_record, "contains_personal_data = false");

    let manifest_record = data_record(&ledger, MANIFEST_LEDGER_ID);
    assert_line(
        manifest_record,
        "source = \"tests/corpus/manifests/t0-bootstrap-v1.toml\"",
    );
    assert_line(
        manifest_record,
        &format!("source_hash = \"sha256:{manifest_hash}\""),
    );
    assert_line(
        manifest_record,
        &format!("license_expression = \"{LICENSE}\""),
    );
    assert_line(
        manifest_record,
        "redistribution = \"disabled pending project-owner license approval; referenced object remains prohibited\"",
    );
    assert_line(manifest_record, "contains_personal_data = false");
    assert_line(manifest_record, "authored_by = \"quality-corpus\"");
    assert_line(manifest_record, "format_schema = 1");
    assert_line(
        manifest_record,
        "validated_by = \"cargo run --quiet --package pdf-rs-corpus -- validate tests/corpus/manifests/t0-bootstrap-v1.toml .\"",
    );
    assert!(
        !manifest_record
            .lines()
            .any(|line| line.starts_with("generated_by ="))
    );
    assert!(
        !manifest_record
            .lines()
            .any(|line| line.starts_with("output_hash ="))
    );
}

const CASE_ID: &str = "infrastructure/synthetic-failure-bundle-001";
const FIXTURE_LEDGER_ID: &str = "fixture.infrastructure.synthetic-failure-bundle-001";
const MANIFEST_LEDGER_ID: &str = "corpus.manifest.t0-bootstrap-v1";
const LICENSE: &str = "LicenseRef-PDF.rs-SelfAuthored-Test";

fn data_record<'a>(ledger: &'a str, id: &str) -> &'a str {
    let expected = format!("id = \"{id}\"");
    ledger
        .split("[[data]]")
        .skip(1)
        .find(|record| record.lines().any(|line| line == expected))
        .unwrap_or_else(|| panic!("missing data-ledger record for {id}"))
}

fn assert_line(document: &str, expected: &str) {
    assert!(
        document.lines().any(|line| line == expected),
        "missing metadata line: {expected}"
    );
}
