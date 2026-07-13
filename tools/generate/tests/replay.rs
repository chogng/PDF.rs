use std::fs;
use std::path::PathBuf;

use pdf_rs_generate::{GenerateLimits, ONE_PAGE_DSL, compile_dsl, generate_one_page_pdf};

#[test]
fn repository_source_replays_the_canonical_fixture() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path =
        repository.join("tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl");
    let source = fs::read(source_path).unwrap();
    assert_eq!(source, ONE_PAGE_DSL.as_bytes());

    let replayed = compile_dsl(&source, GenerateLimits::default()).unwrap();
    let canonical = generate_one_page_pdf().unwrap();
    assert_eq!(replayed.bytes(), canonical);
}
