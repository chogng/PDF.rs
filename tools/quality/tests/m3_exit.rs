use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_quality::case_contract::validate_case_file;
use pdf_rs_quality::manifest::CaseManifest;

#[path = "support/evidence.rs"]
mod evidence;

use evidence::{
    RootToml, array_table_records, split_subject_entry, validate_commit_id, validate_sha256,
    verify_reviewed_subjects, verify_subject_entries,
};

const COMPLETED_AT: &str = "2026-07-16";
const TRACE_VERSION: &str = "0.78.0";
const CAPABILITY_PROFILE_VERSION: &str = "0.4.0";
const DATA_LEDGER_VERSION: &str = "0.12.0";
const PROFILE_ID: &str = "m3.reference-raster-v1.v1";
const FEATURE_ID: &str = "core.reference-raster-v1";
const TARGET: &str = "pdf-rs/raster::ReferenceRenderJob";

const REFERENCE_IMPLEMENTATION_COMMIT: &str = "8c3e28c8ce4cbe5113cc565a36744158e283a7fb";
const REFERENCE_IMPLEMENTATION_TREE: &str = "724c2a646114a8aff0fabe29f6008a8b73802783";
const REFERENCE_LS_TREE_SHA256: &str =
    "0088e35c0824ab38b7e2ba41ff56c89d9bf246b611e968cee19cc36475327f5b";
const CANONICAL_RESULT_BYTES: u64 = 6617;
const CANONICAL_RESULT_SHA256: &str =
    "4a925df5d50c4c1c6bc585104d64cfa6e49f7ea5ad7b680c7b38a28a1225d67c";
const CANONICAL_ARTIFACT_TREE_ALGORITHM: &str = "sha256 over bytewise-sorted UTF-8 records: \
relative_path then #sha256: then lowercase_file_sha256 then LF; exactly 25 regular files";
const CANONICAL_ARTIFACT_TREE_BYTES: u64 = 2977;
const CANONICAL_ARTIFACT_TREE_SHA256: &str =
    "7a9c4540b3acf137384b8ca023c502c2b96716135c8273d3efa8d49d2522e611";
const CANONICAL_ARTIFACT_TOTAL_BYTES: u64 = 62253;

const M1_PAGE_TREE_SHA256: &str =
    "e680abd131a3a4da61262eb152820c3e4f6252c6396a15447039713da3a0f5e1";
const M1_PAGE_TREE_TEST_SHA256: &str =
    "aa8f4bbb5c4475d62a29a0cce3e8f798b17ea606e185b8b97017c2bc25e14374";

const REQUIREMENTS: [&str; 14] = [
    "ISO-32000-1:2008/8.4.2",
    "ISO-32000-1:2008/8.4.3",
    "ISO-32000-1:2008/8.5",
    "ISO-32000-1:2008/8.6",
    "ISO-32000-1:2008/8.9",
    "ISO-32000-1:2008/9.3",
    "ISO-32000-1:2008/9.4",
    "ISO-32000-1:2008/9.6.4",
    "ISO-32000-1:2008/11.3.2-11.3.4",
    "RPE-ARCH-001/5.8-5.9",
    "RPE-ARCH-001/6.1-6.2",
    "RPE-ARCH-001/6.4-6.7",
    "RPE-ARCH-001/8.1-8.3",
    "RPE-ARCH-001/15.3/M3",
];

const REFERENCE_SOURCES: [&str; 13] = [
    "pdf-rs/raster/src/lib.rs",
    "pdf-rs/raster/src/reference/color.rs",
    "pdf-rs/raster/src/reference/coverage.rs",
    "pdf-rs/raster/src/reference/error.rs",
    "pdf-rs/raster/src/reference/geometry.rs",
    "pdf-rs/raster/src/reference/glyph.rs",
    "pdf-rs/raster/src/reference/image.rs",
    "pdf-rs/raster/src/reference/limits.rs",
    "pdf-rs/raster/src/reference/mod.rs",
    "pdf-rs/raster/src/reference/model.rs",
    "pdf-rs/raster/src/reference/render.rs",
    "pdf-rs/raster/src/reference/stroke.rs",
    "pdf-rs/raster/src/reference/surface.rs",
];

const MATURITY_ARTIFACTS: [(&str, &str, &str, &str); 4] = [
    (
        "reference",
        "docs/traceability/evidence/m3/reference-raster-v1/reference.toml",
        "evidence.m3.reference-raster-v1.reference",
        "reference-implementation",
    ),
    (
        "o0",
        "docs/traceability/evidence/m3/reference-raster-v1/o0.toml",
        "evidence.m3.reference-raster-v1.o0",
        "o0-case",
    ),
    (
        "o1",
        "docs/traceability/evidence/m3/reference-raster-v1/o1.toml",
        "evidence.m3.reference-raster-v1.o1",
        "o1-case",
    ),
    (
        "independent_review",
        "docs/traceability/evidence/m3/reference-raster-v1/independent-review.toml",
        "evidence.m3.reference-raster-v1.independent-review",
        "independent-review",
    ),
];

const MATURITY_SUBJECT_REPORT: &str =
    "docs/traceability/evidence/m3/subjects/reference-raster-v1-independent-review.toml";
const NORMATIVE_REPLAY: &str =
    "docs/traceability/evidence/m3/reference-raster-gate/normative-replay.toml";
const FINAL_REVIEW: &str =
    "docs/traceability/evidence/m3/reference-raster-gate/independent-review.toml";
const CLOSURE_INTENT: &str =
    "docs/traceability/evidence/m3/reference-raster-gate/m3-closure-intent.toml";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedFlags {
    parse: bool,
    scene: bool,
    text: bool,
    pixel: bool,
    diagnostic: bool,
    capability: bool,
    error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PixelAuthority {
    O0,
    O1,
    O3,
}

impl PixelAuthority {
    const fn label(self) -> &'static str {
        match self {
            Self::O0 => "O0",
            Self::O1 => "O1",
            Self::O3 => "O3",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OneLess {
    budget_key: &'static str,
    limit: u64,
    attempted: u64,
    limit_kind: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaseSpec {
    slug: &'static str,
    oracle: &'static str,
    pixel_authority: Option<PixelAuthority>,
    strict_expected: &'static str,
    outcome: &'static str,
    stage: &'static str,
    width: u64,
    height: u64,
    flags: ExpectedFlags,
    one_less: Option<OneLess>,
}

const READY_FLAGS: ExpectedFlags = ExpectedFlags {
    parse: true,
    scene: true,
    text: false,
    pixel: true,
    diagnostic: false,
    capability: false,
    error: false,
};

const CASES: [CaseSpec; 12] = [
    CaseSpec {
        slug: "valid-path-clip",
        oracle: "O0",
        pixel_authority: Some(PixelAuthority::O0),
        strict_expected: "success",
        outcome: "ready",
        stage: "reference-render",
        width: 2,
        height: 1,
        flags: READY_FLAGS,
        one_less: None,
    },
    CaseSpec {
        slug: "valid-stroke",
        oracle: "O1",
        pixel_authority: Some(PixelAuthority::O1),
        strict_expected: "success",
        outcome: "ready",
        stage: "reference-render",
        width: 8,
        height: 8,
        flags: READY_FLAGS,
        one_less: None,
    },
    CaseSpec {
        slug: "valid-image",
        oracle: "O1",
        pixel_authority: Some(PixelAuthority::O1),
        strict_expected: "success",
        outcome: "ready",
        stage: "reference-render",
        width: 2,
        height: 1,
        flags: READY_FLAGS,
        one_less: None,
    },
    CaseSpec {
        slug: "valid-font",
        oracle: "O1",
        pixel_authority: Some(PixelAuthority::O1),
        strict_expected: "success",
        outcome: "ready",
        stage: "reference-render",
        width: 8,
        height: 8,
        flags: READY_FLAGS,
        one_less: None,
    },
    CaseSpec {
        slug: "valid-mixed",
        oracle: "O1",
        pixel_authority: Some(PixelAuthority::O3),
        strict_expected: "success",
        outcome: "ready",
        stage: "reference-render",
        width: 8,
        height: 8,
        flags: READY_FLAGS,
        one_less: None,
    },
    CaseSpec {
        slug: "producer-unsupported-interpolated-image",
        oracle: "O0",
        pixel_authority: None,
        strict_expected: "RPE-CONTENT-UNSUPPORTED-0009",
        outcome: "unsupported",
        stage: "content-image",
        width: 8,
        height: 8,
        flags: ExpectedFlags {
            parse: true,
            scene: false,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: true,
            error: false,
        },
        one_less: None,
    },
    CaseSpec {
        slug: "invalid-content-state",
        oracle: "O0",
        pixel_authority: None,
        strict_expected: "RPE-CONTENT-VM-0007",
        outcome: "failed",
        stage: "content-vm",
        width: 8,
        height: 8,
        flags: ExpectedFlags {
            parse: true,
            scene: false,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: None,
    },
    CaseSpec {
        slug: "strict-invalid-xref",
        oracle: "O0",
        pixel_authority: None,
        strict_expected: "RPE-XREF-0011",
        outcome: "failed",
        stage: "strict-open",
        width: 8,
        height: 8,
        flags: ExpectedFlags {
            parse: false,
            scene: false,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: None,
    },
    CaseSpec {
        slug: "cancel-final-publication",
        oracle: "O1",
        pixel_authority: None,
        strict_expected: "RPE-RASTER-0004",
        outcome: "cancelled",
        stage: "reference-publication",
        width: 2,
        height: 1,
        flags: ExpectedFlags {
            parse: true,
            scene: true,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: None,
    },
    CaseSpec {
        slug: "source-change-after-pending",
        oracle: "O1",
        pixel_authority: None,
        strict_expected: "RPE-CONTENT-VM-0014",
        outcome: "source-changed",
        stage: "content-vm-resume",
        width: 8,
        height: 8,
        flags: ExpectedFlags {
            parse: true,
            scene: false,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: None,
    },
    CaseSpec {
        slug: "image-decoded-one-less",
        oracle: "O1",
        pixel_authority: None,
        strict_expected: "RPE-CONTENT-VM-0012",
        outcome: "resource-limited",
        stage: "content-image",
        width: 8,
        height: 8,
        flags: ExpectedFlags {
            parse: true,
            scene: false,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: Some(OneLess {
            budget_key: "max_total_decode_bytes",
            limit: 5,
            attempted: 6,
            limit_kind: "content-image-decoded-bytes",
        }),
    },
    CaseSpec {
        slug: "raster-output-one-less",
        oracle: "O1",
        pixel_authority: None,
        strict_expected: "RPE-RASTER-0005",
        outcome: "resource-limited",
        stage: "reference-preflight",
        width: 2,
        height: 1,
        flags: ExpectedFlags {
            parse: true,
            scene: true,
            text: false,
            pixel: false,
            diagnostic: true,
            capability: false,
            error: true,
        },
        one_less: Some(OneLess {
            budget_key: "max_raster_output_bytes",
            limit: 7,
            attempted: 8,
            limit_kind: "reference-output-bytes",
        }),
    },
];

#[test]
fn m3_reference_registry_is_exactly_twelve_cases_and_thirty_six_regular_files() {
    let root = repository_root();
    let case_root = root.join("tests/cases/raster/m3-reference");
    assert_regular_directory(&case_root, "M3 Reference case root");

    let actual = collect_regular_tree(&case_root);
    let expected = expected_case_tree();
    assert_eq!(
        actual.len(),
        36,
        "M3 Reference registry must contain exactly 36 regular files"
    );
    assert_eq!(
        actual, expected,
        "M3 Reference registry has an extra, missing, or misplaced file"
    );

    let actual_slugs = direct_directory_names(&case_root);
    let expected_slugs = CASES
        .iter()
        .map(|case| case.slug.to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_slugs, expected_slugs,
        "M3 Reference registry slugs are not exact"
    );
}

#[test]
fn every_m3_case_is_hash_bound_exact_and_matches_its_terminal_contract() {
    let root = repository_root();
    for case in CASES {
        let directory = case_directory(&root, case);
        let manifest_path = directory.join("case.toml");
        assert_regular_file(&manifest_path, "case manifest");
        let manifest = validate_case_file(&manifest_path).unwrap_or_else(|diagnostics| {
            panic!(
                "case={} failed the read-only linked-artifact contract: {diagnostics:?}",
                case.slug
            )
        });

        let expected_id = format!("raster/m3-reference/{}", case.slug);
        assert_eq!(
            manifest.case_id(),
            expected_id,
            "case={} identity.id is not directory-bound",
            case.slug
        );
        assert_eq!(
            manifest.string("provenance", "source"),
            Some(format!("tests/cases/raster/m3-reference/{}/input.pdf", case.slug).as_str()),
            "case={} provenance.source is not its committed input",
            case.slug
        );

        let input = read_regular_file(&directory.join("input.pdf"), "case input");
        assert_eq!(
            manifest.source_sha256(),
            digest_reference(&input),
            "case={} input digest differs from provenance.sha256",
            case.slug
        );
        assert_eq!(
            manifest.positive_u64("budget", "max_input_bytes"),
            Some(u64::try_from(input.len()).expect("bounded fixture length fits u64")),
            "case={} max_input_bytes is not exact",
            case.slug
        );
        assert_eq!(
            manifest.string_array("runners", "native"),
            Some(vec![
                "tools/quality::m3_reference_gate",
                "tools/quality::m3_reference_oracle_model",
            ]),
            "case={} Native runners are not the closed M3 runner set",
            case.slug
        );
        assert!(
            manifest
                .string_array("features", "ids")
                .is_some_and(|features| features.contains(&FEATURE_ID)),
            "case={} is not linked to {FEATURE_ID}",
            case.slug
        );
        assert_eq!(
            manifest.string("validity", "strict_expected"),
            Some(case.strict_expected),
            "case={} strict terminal diagnostic changed",
            case.slug
        );
        assert_eq!(
            manifest.string("oracle", "level"),
            Some(case.oracle),
            "case={} semantic oracle level changed",
            case.slug
        );
        assert_eq!(
            manifest.boolean("oracle", "reference_may_generate"),
            Some(false),
            "case={} semantic authority may not be generated by Reference",
            case.slug
        );
        assert_render_contract(case, &manifest);
        assert_expected_flags(case, &manifest);
        assert_terminal_derivation(case, &manifest);
        assert_pixel_contract(case, &manifest, &directory);
    }
}

#[test]
fn oracle_partition_has_four_o0_eight_o1_and_one_separately_reviewed_o3_pixel() {
    let root = repository_root();
    let mut semantic_o0 = BTreeSet::new();
    let mut semantic_o1 = BTreeSet::new();
    let mut pixel_o0 = BTreeSet::new();
    let mut pixel_o1 = BTreeSet::new();
    let mut pixel_o3 = BTreeSet::new();
    let mut no_pixel = BTreeSet::new();

    for case in CASES {
        match case.oracle {
            "O0" => {
                semantic_o0.insert(case.slug);
            }
            "O1" => {
                semantic_o1.insert(case.slug);
            }
            other => panic!("case={} has forbidden semantic oracle {other}", case.slug),
        }
        match case.pixel_authority {
            Some(PixelAuthority::O0) => {
                pixel_o0.insert(case.slug);
            }
            Some(PixelAuthority::O1) => {
                pixel_o1.insert(case.slug);
            }
            Some(PixelAuthority::O3) => {
                pixel_o3.insert(case.slug);
            }
            None => {
                no_pixel.insert(case.slug);
            }
        }

        let manifest = validate_case_file(&case_directory(&root, case).join("case.toml"))
            .unwrap_or_else(|diagnostics| panic!("case={} invalid: {diagnostics:?}", case.slug));
        assert_eq!(manifest.string("oracle", "level"), Some(case.oracle));
        assert_eq!(
            manifest.string("pixel_oracle", "level"),
            case.pixel_authority.map(PixelAuthority::label)
        );
    }

    assert_eq!(
        semantic_o0,
        string_set(&[
            "valid-path-clip",
            "producer-unsupported-interpolated-image",
            "invalid-content-state",
            "strict-invalid-xref",
        ])
    );
    assert_eq!(
        semantic_o1,
        string_set(&[
            "valid-stroke",
            "valid-image",
            "valid-font",
            "valid-mixed",
            "cancel-final-publication",
            "source-change-after-pending",
            "image-decoded-one-less",
            "raster-output-one-less",
        ])
    );
    assert_eq!(pixel_o0, string_set(&["valid-path-clip"]));
    assert_eq!(
        pixel_o1,
        string_set(&["valid-stroke", "valid-image", "valid-font"])
    );
    assert_eq!(pixel_o3, string_set(&["valid-mixed"]));
    assert_eq!(no_pixel.len(), 7);
}

#[test]
fn mixed_o3_identity_is_canonical_and_reproducible_from_the_reviewed_git_tree() {
    let root = repository_root();
    let mixed = CASES
        .iter()
        .copied()
        .find(|case| case.slug == "valid-mixed")
        .expect("mixed case is registered");
    let directory = case_directory(&root, mixed);
    let manifest = validate_case_file(&directory.join("case.toml"))
        .unwrap_or_else(|diagnostics| panic!("mixed case invalid: {diagnostics:?}"));

    let commit_object = git_output(
        &root,
        &[
            "cat-file",
            "-e",
            &format!("{REFERENCE_IMPLEMENTATION_COMMIT}^{{commit}}"),
        ],
    );
    assert!(
        commit_object.is_empty(),
        "git cat-file -e must not emit output"
    );
    let tree = String::from_utf8(git_output(
        &root,
        &[
            "rev-parse",
            &format!("{REFERENCE_IMPLEMENTATION_COMMIT}^{{tree}}"),
        ],
    ))
    .expect("git tree id is UTF-8");
    assert_eq!(
        tree.trim_end(),
        REFERENCE_IMPLEMENTATION_TREE,
        "reviewed commit no longer resolves to the frozen tree"
    );

    let ls_tree = git_output(
        &root,
        &[
            "ls-tree",
            "-r",
            "--full-tree",
            REFERENCE_IMPLEMENTATION_COMMIT,
            "--",
            "pdf-rs/raster",
        ],
    );
    assert!(
        ls_tree.ends_with(b"\n"),
        "frozen git ls-tree byte stream must retain its terminating LF"
    );
    assert_eq!(
        digest(&ls_tree),
        REFERENCE_LS_TREE_SHA256,
        "frozen pdf-rs/raster ls-tree fingerprint changed"
    );

    let identity_reference = manifest
        .string("pixel_oracle", "reference_identity")
        .expect("mixed O3 has a reference identity");
    let (identity_path, identity_hash) =
        split_content_reference(identity_reference).expect("mixed identity is content-addressed");
    let identity_bytes = read_regular_file(&directory.join(identity_path), "reference identity");
    assert_eq!(digest(&identity_bytes), identity_hash);
    let expected_identity = format!(
        "{{\"algorithm\":\"reference-raster-v1\",\"implementation_sha256\":\"sha256:{REFERENCE_LS_TREE_SHA256}\",\"schema\":1}}"
    );
    assert_eq!(
        identity_bytes,
        expected_identity.as_bytes(),
        "mixed O3 reference identity is not canonical"
    );

    let derivation_reference = manifest
        .string("pixel_oracle", "derivation")
        .expect("mixed O3 has a derivation");
    let (derivation_path, derivation_hash) = split_content_reference(derivation_reference)
        .expect("mixed derivation is content-addressed");
    let derivation = read_regular_file(&directory.join(derivation_path), "mixed derivation");
    assert_eq!(digest(&derivation), derivation_hash);
    let derivation = String::from_utf8(derivation).expect("mixed derivation is UTF-8");
    for marker in [
        REFERENCE_IMPLEMENTATION_COMMIT,
        REFERENCE_IMPLEMENTATION_TREE,
        REFERENCE_LS_TREE_SHA256,
        "git ls-tree -r --full-tree",
        "-- pdf-rs/raster",
        "including each terminating LF",
        "deliberately does not derive the final",
    ] {
        assert!(
            derivation.contains(marker),
            "mixed O3 derivation is missing {marker:?}"
        );
    }

    assert_eq!(
        manifest.string_array("pixel_oracle", "reviewers"),
        Some(vec!["spec-conformance", "parser-security"])
    );
    assert_eq!(
        manifest.boolean("pixel_oracle", "reference_may_generate"),
        Some(true)
    );
    let review_path = manifest
        .string("pixel_oracle", "review_evidence")
        .expect("mixed O3 has review evidence");
    let review_bytes = read_regular_file(&directory.join(review_path), "mixed O3 review");
    assert_eq!(
        digest_reference(&review_bytes),
        manifest
            .string("pixel_oracle", "review_evidence_sha256")
            .expect("mixed O3 has review hash")
    );
    assert_eq!(
        review_bytes,
        canonical_mixed_review(&manifest).as_bytes(),
        "mixed O3 review is not the canonical hash-bound independent review"
    );
}

#[test]
fn independent_oracle_model_is_read_only_and_cannot_generate_o0_or_o1_from_reference() {
    let root = repository_root();
    let model_path = root.join("tools/quality/tests/m3_reference_oracle_model.rs");
    let model = read_utf8(&model_path);

    for forbidden in [
        "pdf_rs_raster",
        "ReferenceRenderJob",
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT",
        "run_gate(",
        "fs::write",
        "File::create",
        "OpenOptions",
        "update-golden",
        "update_golden",
        "overwrite-expected",
        "accept-pixels",
    ] {
        assert!(
            !model.contains(forbidden),
            "independent oracle model contains forbidden token {forbidden:?}"
        );
    }
    for required in [
        "committed_inputs_match_the_frozen_literal_fixture_specs",
        "independent_ready_models_match_committed_pixels_and_hashes_exactly",
        "non_ready_contracts_pin_terminal_semantics_flags_and_budgets",
        "oracle_model_source_is_read_only_and_has_no_integrated_render_dependency",
        "ValidMixed",
        "O3",
        "reference_identity",
        "review_evidence",
    ] {
        assert!(
            model.contains(required),
            "independent oracle model is missing closure marker {required:?}"
        );
    }

    let fixture = read_utf8(&root.join("tools/quality/tests/m3_reference_gate_support/fixture.rs"));
    assert!(
        !fixture.contains("pdf_rs_raster") && !fixture.contains("ReferenceRenderJob"),
        "shared literal fixture builder must not import the Reference renderer"
    );
    assert!(
        !fixture.contains("fs::write") && !fixture.contains("File::create"),
        "shared literal fixture builder must not write committed fixtures"
    );
}

#[test]
fn integrated_gate_is_registry_driven_golden_exact_and_emits_schema_two_audits() {
    let root = repository_root();
    let gate = read_utf8(&root.join("tools/quality/tests/m3_reference_gate_support/mod.rs"));
    let registry =
        read_utf8(&root.join("tools/quality/tests/m3_reference_gate_support/registry.rs"));
    let artifact =
        read_utf8(&root.join("tools/quality/tests/m3_reference_gate_support/artifact.rs"));

    for required in [
        "mod registry;",
        "load_registry",
        "for contract in &registry",
        "run_case(contract",
        "contract.input",
    ] {
        assert!(
            gate.contains(required),
            "integrated gate is missing registry-driven marker {required:?}"
        );
    }
    assert!(
        !gate.contains("const CASES:"),
        "integrated gate must not own a second static case registry"
    );
    for required in [
        "fs::read_dir",
        "validate_case_file",
        "Case::from_id",
        "read_regular_file",
        "manifest_sha256",
        "expected_pixel_sha256",
        "pixel_oracle_level",
        "REFERENCE_IMPLEMENTATION_SHA256",
        "formal M3 Reference registry must contain exactly twelve cases",
    ] {
        assert!(
            registry.contains(required),
            "formal registry loader is missing {required:?}"
        );
    }
    for forbidden in ["fs::write", "File::create", "OpenOptions"] {
        assert!(
            !registry.contains(forbidden),
            "formal registry loader must remain read-only: {forbidden}"
        );
    }

    assert!(
        artifact.matches("\\\"schema\\\":2").count() >= 2,
        "both result and per-case audit artifacts must use schema 2"
    );
    for required in [
        "contract.assert_observed",
        "manifest_sha256",
        "oracle_level",
        "pixel_oracle_level",
        "expected_pixel_sha256",
        "exact",
    ] {
        assert!(
            artifact.contains(required),
            "schema-2 gate artifacts are missing {required:?}"
        );
    }
    assert!(
        artifact.contains("REFERENCE_IMPLEMENTATION_COMMIT")
            && artifact.contains("REFERENCE_IMPLEMENTATION_TREE"),
        "gate audit must retain the reviewed Reference commit and tree"
    );
}

#[test]
fn reference_maturity_profile_has_four_hash_closed_artifacts_and_exact_subject_partitions() {
    let root = repository_root();
    let profiles_text = read_utf8(&root.join("docs/traceability/capability-profiles.toml"));
    let profile_root = RootToml::parse(&profiles_text).expect("capability profile TOML is strict");
    profile_root
        .expect_string("version", CAPABILITY_PROFILE_VERSION)
        .expect("capability profile version is exact");
    let profile = record(&profiles_text, "profile", PROFILE_ID);
    profile
        .expect_string("owner", "graphics-color")
        .expect("profile owner");
    profile
        .expect_string("state", "REFERENCE")
        .expect("profile state");
    profile
        .expect_string("feature", FEATURE_ID)
        .expect("profile feature");
    profile
        .expect_string("target", TARGET)
        .expect("profile target");
    profile
        .expect_array("requirements", &REQUIREMENTS)
        .expect("profile requirements are the exact bounded M3 set");
    assert!(
        !profile.array("supported").expect("supported").is_empty(),
        "REFERENCE profile must state its supported subset"
    );
    assert!(
        !profile.array("excluded").expect("excluded").is_empty(),
        "REFERENCE profile must state its excluded subset"
    );
    for marker in ["bounded", "O0", "O1", "O3", "not", "CANARY"] {
        assert!(
            profile
                .string("policy")
                .expect("profile policy")
                .contains(marker),
            "REFERENCE policy is missing boundary marker {marker:?}"
        );
    }
    profile
        .expect_array("o2_adjudications", &[])
        .expect("REFERENCE has no O2");
    profile
        .expect_array("fuzz_targets", &[])
        .expect("REFERENCE has no DIFFERENTIAL fuzz evidence");
    for field in [
        "fuzz_minimizer",
        "holdout_manifest",
        "benchmark_report",
        "differential_report",
        "baseline_fingerprint",
    ] {
        profile
            .expect_string(field, "REQUIRED_BEFORE_DIFFERENTIAL")
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }

    let reference_ref = profile.string("reference").expect("reference artifact");
    let o0_refs = profile.array("o0_cases").expect("O0 artifacts");
    let o1_refs = profile.array("o1_cases").expect("O1 artifacts");
    assert_eq!(
        o0_refs.len(),
        1,
        "M3 profile aggregates O0 into one artifact"
    );
    assert_eq!(
        o1_refs.len(),
        1,
        "M3 profile aggregates O1 into one artifact"
    );
    let review_ref = profile
        .string("independent_review")
        .expect("independent review artifact");
    let profile_refs = BTreeMap::from([
        ("reference", reference_ref.to_owned()),
        ("o0", o0_refs[0].clone()),
        ("o1", o1_refs[0].clone()),
        ("independent_review", review_ref.to_owned()),
    ]);

    let ledger_text = read_utf8(&root.join("docs/traceability/data-ledger.toml"));
    RootToml::parse(&ledger_text)
        .expect("data ledger TOML is strict")
        .expect_string("version", DATA_LEDGER_VERSION)
        .expect("data ledger version is exact");
    let ledger = array_table_records(&ledger_text, "data").expect("data records are strict");

    let mut evidence_paths = BTreeSet::new();
    let mut evidence_refs = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    for (field, expected_path, expected_id, expected_role) in MATURITY_ARTIFACTS {
        let reference = profile_refs
            .get(field)
            .unwrap_or_else(|| panic!("profile is missing {field}"));
        let (path, hash) =
            split_content_reference(reference).expect("profile evidence is content-addressed");
        assert_eq!(
            path, expected_path,
            "profile field {field} points at the wrong evidence artifact"
        );
        assert!(
            evidence_paths.insert(path.to_owned()),
            "profile reuses evidence path {path}"
        );
        let bytes = read_regular_file(&root.join(path), "maturity evidence");
        assert_eq!(
            digest(&bytes),
            hash,
            "profile field {field} has a stale evidence digest"
        );
        let text = String::from_utf8(bytes).expect("maturity evidence is UTF-8");
        let artifact = RootToml::parse(&text).expect("maturity evidence is strict TOML");
        assert_maturity_artifact(
            &artifact,
            expected_id,
            expected_role,
            expected_oracle(expected_role),
            expected_subject_kind(expected_role),
        );
        verify_subject_entries(
            &root,
            artifact.array("subjects").expect("artifact subjects"),
        )
        .unwrap_or_else(|error| panic!("{expected_id} subject verification failed: {error}"));
        let artifact_subject_paths =
            subject_paths(artifact.array("subjects").expect("artifact subjects"));
        assert!(
            artifact_subject_paths.iter().all(|path| {
                !evidence_paths.contains(*path)
                    && !MATURITY_ARTIFACTS
                        .iter()
                        .any(|(_, candidate, _, _)| *path == *candidate)
            }),
            "{expected_id} may not self-reference or use another maturity artifact as a subject"
        );

        let matching_ledger = ledger
            .iter()
            .filter(|entry| entry.string("source").ok() == Some(path))
            .collect::<Vec<_>>();
        assert_eq!(
            matching_ledger.len(),
            1,
            "maturity evidence {path} must have exactly one ledger record"
        );
        let ledger_entry = matching_ledger[0];
        ledger_entry
            .expect_string("id", expected_id)
            .unwrap_or_else(|error| panic!("{expected_id} ledger id: {error}"));
        ledger_entry
            .expect_string("kind", "project-authored-maturity-evidence")
            .unwrap_or_else(|error| panic!("{expected_id} ledger kind: {error}"));
        ledger_entry
            .expect_string("source_hash", &format!("sha256:{hash}"))
            .unwrap_or_else(|error| panic!("{expected_id} ledger hash: {error}"));

        evidence_refs.insert(field, reference.clone());
        artifacts.insert(field, artifact);
    }

    assert_reference_subjects(
        artifacts
            .get("reference")
            .expect("reference artifact")
            .array("subjects")
            .expect("reference subjects"),
    );
    assert_case_subject_partition(
        artifacts
            .get("o0")
            .expect("O0 artifact")
            .array("subjects")
            .expect("O0 subjects"),
        "O0",
    );
    assert_case_subject_partition(
        artifacts
            .get("o1")
            .expect("O1 artifact")
            .array("subjects")
            .expect("O1 subjects"),
        "O1",
    );
    let review = artifacts
        .get("independent_review")
        .expect("review artifact");
    assert_eq!(
        review.array("cross_references").expect("review cross refs"),
        &[
            evidence_refs["reference"].clone(),
            evidence_refs["o0"].clone(),
            evidence_refs["o1"].clone(),
        ],
        "independent maturity review must cross-reference exactly reference/O0/O1"
    );
    let maturity_review_subjects =
        subject_paths(review.array("subjects").expect("review subjects"));
    assert!(
        maturity_review_subjects.contains(MATURITY_SUBJECT_REPORT),
        "independent maturity review is missing its validator-readable subject report"
    );
    for required in [
        "tools/quality/tests/m3_reference_gate.rs",
        "tools/quality/tests/m3_reference_oracle_model.rs",
    ] {
        assert!(
            maturity_review_subjects.contains(required),
            "independent maturity review is missing validator subject {required}"
        );
    }
    assert!(
        !maturity_review_subjects.contains("tools/quality/tests/m3_exit.rs"),
        "independent maturity review may not bind the exit test that validates it"
    );
    assert_independent_subject_report(&root, review);
}

#[test]
fn generic_maturity_validator_accepts_all_five_truthfully_promoted_profiles() {
    let root = repository_root();
    let output = Command::new(env!("CARGO_BIN_EXE_pdf-rs-quality"))
        .current_dir(&root)
        .args([
            "validate-m1-maturity",
            "docs/traceability/capability-profiles.toml",
        ])
        .output()
        .expect("quality maturity validator executes");
    assert!(
        output.status.success(),
        "generic maturity validator failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("maturity report is UTF-8");
    for line in ["profiles=5", "planned=0", "reference=3", "differential=2"] {
        assert!(
            stdout.lines().any(|candidate| candidate == line),
            "generic maturity report is missing {line:?}: {stdout}"
        );
    }
}

#[test]
fn m1_page_tree_and_m2_exit_evidence_remain_immutable_and_ci_precedes_m3() {
    let root = repository_root();
    assert_eq!(
        digest_file(&root.join("pdf-rs/document/src/page_tree.rs")),
        M1_PAGE_TREE_SHA256,
        "accepted M1 page-tree implementation changed"
    );
    assert_eq!(
        digest_file(&root.join("pdf-rs/document/tests/page_tree_count.rs")),
        M1_PAGE_TREE_TEST_SHA256,
        "accepted M1 page-tree-count test changed"
    );

    let m2_plan_text = read_utf8(&root.join("plan/m2.toml"));
    let m2_plan = RootToml::parse(&m2_plan_text).expect("M2 plan TOML is strict");
    m2_plan
        .expect_string("milestone", "M2")
        .expect("M2 milestone id");
    m2_plan
        .expect_string("status", "complete")
        .expect("M2 remains complete");
    m2_plan
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("M2 completion date");
    for ordinal in 1..=7 {
        let id = format!("M2-{ordinal:02}");
        let item = record(&m2_plan_text, "work_item", &id);
        item.expect_string("status", "complete")
            .unwrap_or_else(|error| panic!("{id} status: {error}"));
        item.expect_bare("completed_at", COMPLETED_AT)
            .unwrap_or_else(|error| panic!("{id} completed_at: {error}"));
    }

    let m2_replay_path =
        root.join("docs/traceability/evidence/m2/scene-gate/normative-replay.toml");
    let m2_review_path =
        root.join("docs/traceability/evidence/m2/scene-gate/independent-review.toml");
    let m2_replay =
        RootToml::parse(&read_utf8(&m2_replay_path)).expect("M2 normative replay is strict TOML");
    m2_replay
        .expect_string("role", "normative-replay")
        .expect("M2 replay role");
    m2_replay
        .expect_unsigned("case_count", 6)
        .expect("M2 case count");
    m2_replay
        .expect_array(
            "fresh_replays",
            &["debug-1", "debug-2", "release-1", "release-2"],
        )
        .expect("M2 fresh replays");
    m2_replay
        .expect_array(
            "comparisons",
            &[
                "debug-1=debug-2",
                "release-1=release-2",
                "debug-1=release-1",
            ],
        )
        .expect("M2 replay comparisons");
    m2_replay
        .expect_string("verdict", "pass")
        .expect("M2 replay verdict");

    let m2_review = RootToml::parse(&read_utf8(&m2_review_path)).expect("M2 review is strict TOML");
    m2_review
        .expect_string("role", "independent-review")
        .expect("M2 review role");
    m2_review
        .expect_unsigned("open_p0_p2", 0)
        .expect("M2 has no open P0-P2");
    m2_review
        .expect_string("verdict", "SHIP")
        .expect("M2 review verdict");

    let r0_plan_text = read_utf8(&root.join("plan/r0.toml"));
    let m2_milestone = record(&r0_plan_text, "milestone", "M2");
    m2_milestone
        .expect_string("status", "complete")
        .expect("R0 plan M2 status");
    m2_milestone
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("R0 plan M2 completion");

    let ci = read_utf8(&root.join("scripts/ci.sh"));
    let m2_debug = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/debug-1\"",
    );
    let m2_exit = position(&ci, "cargo test --locked -p pdf-rs-quality --test m2_exit");
    let m3_debug = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/debug-1\"",
    );
    assert!(
        m2_debug < m2_exit && m2_exit < m3_debug,
        "CI must finish the complete M2 replay/exit before the M3 Reference replay"
    );
    assert_ci_diff_triplet(&ci, "m2-scene-gate");
}

#[test]
fn m3_normative_replay_and_final_independent_review_are_complete_and_hash_bound() {
    let root = repository_root();
    assert_fresh_canonical_artifact_tree();
    let replay_text = read_utf8(&root.join(NORMATIVE_REPLAY));
    let replay = RootToml::parse(&replay_text).expect("M3 normative replay is strict TOML");
    replay.expect_unsigned("schema", 1).expect("replay schema");
    replay
        .expect_string("type", "milestone-evidence")
        .expect("replay type");
    replay
        .expect_string("id", "evidence.m3.reference-raster-gate.normative-replay")
        .expect("replay id");
    replay
        .expect_string("milestone", "M3")
        .expect("replay milestone");
    replay
        .expect_string("work_item", "M3-11")
        .expect("replay work item");
    replay
        .expect_string("profile", PROFILE_ID)
        .expect("replay profile");
    replay
        .expect_string("feature", FEATURE_ID)
        .expect("replay feature");
    replay
        .expect_string("role", "normative-replay")
        .expect("replay role");
    replay
        .expect_bool("registered", true)
        .expect("replay registration");
    replay.expect_bool("gating", true).expect("replay gate");
    replay
        .expect_bool("external_observation", false)
        .expect("replay is Native-only");
    replay
        .expect_bool("maturity_promotion", true)
        .expect("replay promotes the selected profile");
    replay
        .expect_unsigned("case_count", 12)
        .expect("replay case count");
    replay
        .expect_unsigned("ready_case_count", 5)
        .expect("replay Ready count");
    replay
        .expect_unsigned("non_ready_case_count", 7)
        .expect("replay non-Ready count");
    replay
        .expect_unsigned("o0_case_count", 4)
        .expect("replay O0 count");
    replay
        .expect_unsigned("o1_case_count", 8)
        .expect("replay O1 count");
    replay
        .expect_unsigned("o3_pixel_count", 1)
        .expect("replay O3 pixel count");
    replay
        .expect_array(
            "fresh_replays",
            &["debug-1", "debug-2", "release-1", "release-2"],
        )
        .expect("M3 fresh replay set");
    replay
        .expect_array(
            "comparisons",
            &[
                "debug-1=debug-2",
                "release-1=release-2",
                "debug-1=release-1",
            ],
        )
        .expect("M3 replay comparisons");
    replay
        .expect_string(
            "reference_implementation_commit",
            REFERENCE_IMPLEMENTATION_COMMIT,
        )
        .expect("replay implementation commit");
    replay
        .expect_string(
            "reference_implementation_tree",
            REFERENCE_IMPLEMENTATION_TREE,
        )
        .expect("replay implementation tree");
    replay
        .expect_string(
            "reference_implementation_sha256",
            &format!("sha256:{REFERENCE_LS_TREE_SHA256}"),
        )
        .expect("replay implementation fingerprint");
    replay
        .expect_string("verdict", "pass")
        .expect("replay verdict");

    let registered_case_tree = replay
        .array("registered_case_tree")
        .expect("replay content-addressed registered case tree");
    assert_eq!(
        registered_case_tree.len(),
        36,
        "normative replay must bind all 36 registered case files"
    );
    assert!(
        registered_case_tree.iter().all(|subject| {
            split_subject_entry(subject).is_ok_and(|(locator, _)| !locator.contains('@'))
        }),
        "normative replay must hash the live registered case tree that the gate executes"
    );
    assert_eq!(
        subject_paths(registered_case_tree)
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>(),
        expected_registered_case_paths(),
        "normative replay registered_case_tree is not the exact formal registry"
    );
    assert_eq!(
        verify_subject_entries(&root, registered_case_tree)
            .expect("normative replay registered case tree is hash-bound"),
        36
    );
    for legacy_partial in ["case_manifests", "input_hashes", "pixel_goldens"] {
        assert_root_key_absent(&replay_text, legacy_partial);
    }

    replay
        .expect_unsigned("canonical_result_schema", 2)
        .expect("canonical result schema is exact");
    replay
        .expect_unsigned("canonical_result_bytes", CANONICAL_RESULT_BYTES)
        .expect("canonical result byte count is exact");
    replay
        .expect_string(
            "canonical_result_sha256",
            &format!("sha256:{CANONICAL_RESULT_SHA256}"),
        )
        .expect("canonical result hash is exact");
    replay
        .expect_unsigned("artifact_count", 25)
        .expect("canonical artifact count is exact");
    replay
        .expect_unsigned("audit_artifact_count", 12)
        .expect("canonical audit artifact count is exact");
    replay
        .expect_unsigned("scene_artifact_count", 7)
        .expect("canonical Scene artifact count is exact");
    replay
        .expect_unsigned("pixel_artifact_count", 5)
        .expect("canonical pixel artifact count is exact");
    replay
        .expect_string(
            "canonical_artifact_tree_algorithm",
            CANONICAL_ARTIFACT_TREE_ALGORITHM,
        )
        .expect("canonical artifact-tree algorithm is exact");
    replay
        .expect_unsigned(
            "canonical_artifact_tree_bytes",
            CANONICAL_ARTIFACT_TREE_BYTES,
        )
        .expect("canonical artifact-tree byte count is exact");
    replay
        .expect_string(
            "canonical_artifact_tree_sha256",
            &format!("sha256:{CANONICAL_ARTIFACT_TREE_SHA256}"),
        )
        .expect("canonical artifact-tree hash is exact");
    replay
        .expect_unsigned(
            "canonical_artifact_total_bytes",
            CANONICAL_ARTIFACT_TOTAL_BYTES,
        )
        .expect("canonical artifact total byte count is exact");
    let artifact_paths = replay
        .array("artifact_paths")
        .expect("canonical artifact topology");
    assert_eq!(
        artifact_paths.len(),
        25,
        "canonical artifact topology contains duplicates or omissions"
    );
    assert_eq!(
        artifact_paths.iter().cloned().collect::<BTreeSet<_>>(),
        expected_artifact_paths(),
        "canonical schema-2 artifact topology is not exact"
    );
    replay
        .expect_array(
            "non_ready_diagnostics",
            &[
                "RPE-RASTER-0004",
                "RPE-CONTENT-VM-0012",
                "RPE-CONTENT-VM-0007",
                "RPE-CONTENT-UNSUPPORTED-0009",
                "RPE-RASTER-0005",
                "RPE-CONTENT-VM-0014",
                "RPE-XREF-0011",
            ],
        )
        .expect("non-Ready diagnostic sequence is exact");
    replay
        .expect_string(
            "output_root_policy",
            "absent-or-empty dedicated directory; never recursively delete caller-selected content",
        )
        .expect("output root policy is exact");

    let replay_commands = replay.array("commands").expect("replay commands");
    for required in [
        "validate-cases tests/cases",
        "--test m3_reference_oracle_model",
        "debug-1",
        "debug-2",
        "release-1",
        "release-2",
        "validate-m1-maturity docs/traceability/capability-profiles.toml",
        "PDF_RS_M3_REFERENCE_EXIT_INPUT=$PWD/target/ci-artifacts/m3-reference-gate/debug-1 cargo test --locked --package pdf-rs-quality --test m3_exit",
    ] {
        assert!(
            replay_commands
                .iter()
                .any(|command| command.contains(required)),
            "M3 normative replay is missing command marker {required:?}"
        );
    }

    let review_text = read_utf8(&root.join(FINAL_REVIEW));
    let review = RootToml::parse(&review_text).expect("M3 final review is strict TOML");
    review.expect_unsigned("schema", 1).expect("review schema");
    review
        .expect_string("type", "milestone-evidence")
        .expect("review type");
    review
        .expect_string("id", "evidence.m3.reference-raster-gate.independent-review")
        .expect("review id");
    review
        .expect_string("milestone", "M3")
        .expect("review milestone");
    review
        .expect_string("work_item", "M3-11")
        .expect("review work item");
    review
        .expect_string("profile", PROFILE_ID)
        .expect("review profile");
    review
        .expect_string("feature", FEATURE_ID)
        .expect("review feature");
    review
        .expect_string("role", "independent-review")
        .expect("review role");
    review
        .expect_bool("registered", true)
        .expect("review registration");
    review.expect_bool("gating", true).expect("review gate");
    review
        .expect_bool("external_observation", false)
        .expect("review is Native-only");
    review
        .expect_bool("maturity_promotion", true)
        .expect("review covers maturity promotion");
    review
        .expect_bare("reviewed_at", COMPLETED_AT)
        .expect("review date");
    review
        .expect_array("reviewer_roles", &["quality-corpus", "spec-conformance"])
        .expect("independent reviewer roles");
    review
        .expect_string(
            "reviewed_subject_resolution",
            "git-tree-at-reviewed-subject-commit-not-working-tree",
        )
        .expect("reviewed subject resolution");
    review
        .expect_array(
            "commands",
            &[
                "PDF_RS_M3_REFERENCE_GATE_OUTPUT=$PWD/target/ci-artifacts/m3-reference-gate/debug-1 cargo test --locked --package pdf-rs-quality --test m3_reference_gate",
                "PDF_RS_M3_REFERENCE_GATE_OUTPUT=$PWD/target/ci-artifacts/m3-reference-gate/release-1 cargo test --locked --release --package pdf-rs-quality --test m3_reference_gate",
                "cargo test --locked --package pdf-rs-quality --test m3_reference_oracle_model",
                "cargo run --quiet --package pdf-rs-quality -- validate-m1-maturity docs/traceability/capability-profiles.toml",
                "PDF_RS_M3_REFERENCE_EXIT_INPUT=$PWD/target/ci-artifacts/m3-reference-gate/debug-1 cargo test --locked --package pdf-rs-quality --test m3_exit",
                "cargo test --locked --package pdf-rs-quality --test m2_exit",
                "cargo fmt --all --check",
                "cargo clippy --workspace --all-targets --all-features -- -D warnings",
                "bash -n scripts/ci.sh",
            ],
        )
        .expect("independent review commands are exact");
    for severity in ["open_p0", "open_p1", "open_p2"] {
        review
            .expect_unsigned(severity, 0)
            .unwrap_or_else(|error| panic!("{severity}: {error}"));
    }
    review
        .expect_string("verdict", "SHIP")
        .expect("review verdict");

    let reviewed_commit = review
        .string("reviewed_subject_commit")
        .expect("reviewed subject commit");
    let reviewed_tree = review
        .string("reviewed_subject_tree")
        .expect("reviewed subject tree");
    validate_commit_id(reviewed_commit).expect("final review uses a full commit id");
    let expected_subject_paths = expected_final_review_subject_paths();
    let reviewed_subject_commit_map = review
        .array("reviewed_subject_commit_map")
        .expect("final reviewed subject commit map");
    assert_eq!(
        reviewed_subject_commit_map.len(),
        expected_subject_paths.len(),
        "every final reviewed subject must have exactly one commit-map entry"
    );
    assert_eq!(
        pinned_locator_paths(reviewed_subject_commit_map, reviewed_commit),
        expected_subject_paths,
        "final reviewed subject commit map is not the exact critical closure"
    );
    let reviewed_subjects = review
        .array("reviewed_subjects")
        .expect("final reviewed subjects");
    assert_eq!(
        reviewed_subject_paths(reviewed_subjects, reviewed_commit),
        expected_subject_paths,
        "final SHIP review does not bind the exact critical closure"
    );
    assert!(
        !reviewed_subject_paths(reviewed_subjects, reviewed_commit).contains(FINAL_REVIEW),
        "final SHIP review may not hash-bind itself"
    );
    assert_eq!(
        verify_reviewed_subjects(&root, &review, reviewed_commit, Some(reviewed_tree))
            .expect("final SHIP review subjects are commit/tree/hash-bound"),
        expected_subject_paths.len()
    );
}

#[test]
fn m3_plan_trace_and_ci_are_closed_only_after_replay_maturity_and_ship_review() {
    let root = repository_root();
    let intent = RootToml::parse(&read_utf8(&root.join(CLOSURE_INTENT)))
        .expect("M3 closure intent is strict TOML");
    intent.expect_unsigned("schema", 1).expect("intent schema");
    intent
        .expect_string("type", "milestone-closure-intent")
        .expect("intent type");
    intent
        .expect_string("id", "evidence.m3.reference-raster-gate.m3-closure-intent")
        .expect("intent id");
    intent
        .expect_string("milestone", "M3")
        .expect("intent milestone");
    intent
        .expect_string("milestone_status", "complete")
        .expect("intent milestone status");
    intent
        .expect_string("work_item_status", "complete")
        .expect("intent work-item status");
    intent
        .expect_bare("completed_at", COMPLETED_AT)
        .expect("intent completion");
    intent
        .expect_array(
            "completed_work_items",
            &[
                "M3-01", "M3-02", "M3-03", "M3-04", "M3-05", "M3-06", "M3-07", "M3-08", "M3-09",
                "M3-10", "M3-11",
            ],
        )
        .expect("intent completed work items");
    intent
        .expect_array("reviewed_by_roles", &["quality-corpus", "spec-conformance"])
        .expect("intent reviewed-by roles");
    intent
        .expect_string("feature", FEATURE_ID)
        .expect("intent feature");
    intent
        .expect_string("feature_state", "REFERENCE")
        .expect("intent feature state");
    intent
        .expect_string("spec_requirement", "RPE-ARCH-001/15.3/M3")
        .expect("intent spec requirement");
    intent
        .expect_string("spec_status", "covered")
        .expect("intent spec status");
    intent
        .expect_array(
            "required_evidence",
            &[NORMATIVE_REPLAY, FINAL_REVIEW, "tools/quality::m3_exit"],
        )
        .expect("intent required evidence");

    let m3_plan_text = read_utf8(&root.join("plan/m3.toml"));
    let m3_plan = RootToml::parse(&m3_plan_text).expect("M3 plan is strict TOML");
    m3_plan
        .expect_string(
            "milestone",
            intent.string("milestone").expect("intent milestone"),
        )
        .expect("M3 plan milestone");
    m3_plan
        .expect_string(
            "status",
            intent
                .string("milestone_status")
                .expect("intent milestone status"),
        )
        .expect("M3 plan status");
    m3_plan
        .expect_bare(
            "completed_at",
            intent.bare("completed_at").expect("intent completion"),
        )
        .expect("M3 plan completion");
    let completed_work_items = intent
        .array("completed_work_items")
        .expect("intent work items");
    let work_item_records =
        array_table_records(&m3_plan_text, "work_item").expect("M3 work items are strict");
    let live_work_item_ids = work_item_records
        .iter()
        .map(|item| item.string("id").expect("M3 work-item id").to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        work_item_records.len(),
        completed_work_items.len(),
        "M3 plan has duplicate or additional work-item records"
    );
    assert_eq!(
        live_work_item_ids,
        completed_work_items.iter().cloned().collect(),
        "M3 plan work-item set differs from the commit-bound closure intent"
    );
    for id in completed_work_items {
        let item = record(&m3_plan_text, "work_item", id);
        item.expect_string(
            "status",
            intent
                .string("work_item_status")
                .expect("intent work-item status"),
        )
        .unwrap_or_else(|error| panic!("{id} status: {error}"));
        item.expect_bare(
            "completed_at",
            intent.bare("completed_at").expect("intent completion"),
        )
        .unwrap_or_else(|error| panic!("{id} completed_at: {error}"));
    }

    let r0_plan_text = read_utf8(&root.join("plan/r0.toml"));
    let m3_milestone = record(&r0_plan_text, "milestone", "M3");
    m3_milestone
        .expect_string(
            "status",
            intent
                .string("milestone_status")
                .expect("intent milestone status"),
        )
        .expect("R0 M3 status");
    m3_milestone
        .expect_bare("started_at", COMPLETED_AT)
        .expect("R0 M3 start");
    m3_milestone
        .expect_bare(
            "completed_at",
            intent.bare("completed_at").expect("intent completion"),
        )
        .expect("R0 M3 completion");
    m3_milestone
        .expect_string("execution_plan", "plan/m3.toml")
        .expect("R0 M3 execution plan");
    m3_milestone
        .expect_array("reviewer_roles", &["quality-corpus", "spec-conformance"])
        .expect("R0 M3 reviewer roles");
    m3_milestone
        .expect_array("reviewed_by_roles", &["quality-corpus", "spec-conformance"])
        .expect("R0 M3 reviewed-by roles");
    let milestone_evidence = m3_milestone.array("evidence").expect("R0 M3 evidence");
    for required in ["12", "O0", "O1", "O3", "REFERENCE"] {
        assert!(
            milestone_evidence
                .iter()
                .any(|entry| entry.contains(required)),
            "R0 M3 milestone evidence is missing {required:?}"
        );
    }
    for required in intent
        .array("required_evidence")
        .expect("intent required evidence")
    {
        assert!(
            milestone_evidence
                .iter()
                .any(|entry| entry.contains(required)),
            "R0 M3 milestone evidence does not implement closure intent {required:?}"
        );
    }

    let feature_text = read_utf8(&root.join("docs/traceability/feature-map.toml"));
    RootToml::parse(&feature_text)
        .expect("feature map is strict TOML")
        .expect_string("version", TRACE_VERSION)
        .expect("feature map version");
    let feature = record(&feature_text, "feature", FEATURE_ID);
    feature
        .expect_string(
            "state",
            intent
                .string("feature_state")
                .expect("intent feature state"),
        )
        .expect("Reference feature state");
    feature
        .expect_string("profile", PROFILE_ID)
        .expect("Reference feature profile");
    feature
        .expect_array("clauses", &REQUIREMENTS)
        .expect("Reference feature requirements");
    let feature_tests = feature.array("tests").expect("Reference feature tests");
    for required in [
        "pdf-rs/raster::reference_integrated_renderer",
        "tools/quality::m3_reference_gate",
        "tools/quality::m3_reference_oracle_model",
        "tools/quality::m3_reference_raster_trace",
        "tools/quality::m3_exit",
    ] {
        assert!(
            feature_tests.iter().any(|test| test == required),
            "Reference feature is missing test {required}"
        );
    }

    let spec_text = read_utf8(&root.join("docs/traceability/spec-map.toml"));
    RootToml::parse(&spec_text)
        .expect("spec map is strict TOML")
        .expect_string("version", TRACE_VERSION)
        .expect("spec map version");
    for requirement_id in REQUIREMENTS {
        let requirement = record(&spec_text, "requirement", requirement_id);
        assert!(
            requirement
                .array("features")
                .expect("requirement features")
                .iter()
                .any(|feature| feature == FEATURE_ID),
            "{requirement_id} does not link the promoted feature"
        );
        let tests = requirement.array("tests").expect("requirement tests");
        for required in [
            "tools/quality::m3_reference_gate",
            "tools/quality::m3_reference_oracle_model",
            "tools/quality::m3_exit",
        ] {
            assert!(
                tests.iter().any(|test| test == required),
                "{requirement_id} is missing test {required}"
            );
        }
    }
    record(
        &spec_text,
        "requirement",
        intent
            .string("spec_requirement")
            .expect("intent spec requirement"),
    )
    .expect_string(
        "status",
        intent.string("spec_status").expect("intent spec status"),
    )
    .expect("M3 milestone requirement status");

    let ci = read_utf8(&root.join("scripts/ci.sh"));
    let validate_cases = position(&ci, "validate-cases tests/cases");
    let oracle_contract = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_raster_oracle_contract",
    );
    let m2_debug_1 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/debug-1\"",
    );
    let m2_debug_2 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/debug-2\"",
    );
    let m2_release_1 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/release-1\"",
    );
    let m2_release_2 = position(
        &ci,
        "PDF_RS_M2_SCENE_GATE_OUTPUT=\"$PWD/$m2_scene_gate_root/release-2\"",
    );
    let m2_debug_diff = ci_diff_position(&ci, "m2_scene_gate_root", "debug-1", "debug-2");
    let m2_release_diff = ci_diff_position(&ci, "m2_scene_gate_root", "release-1", "release-2");
    let m2_profile_diff = ci_diff_position(&ci, "m2_scene_gate_root", "debug-1", "release-1");
    let m2_exit = position(&ci, "cargo test --locked -p pdf-rs-quality --test m2_exit");
    let oracle_model = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_oracle_model",
    );
    let m3_debug_1 = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/debug-1\"",
    );
    let m3_debug_2 = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/debug-2\"",
    );
    let m3_release_1 = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/release-1\"",
    );
    let m3_release_2 = position(
        &ci,
        "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/release-2\"",
    );
    let m3_debug_diff = ci_diff_position(&ci, "m3_reference_gate_root", "debug-1", "debug-2");
    let m3_release_diff = ci_diff_position(&ci, "m3_reference_gate_root", "release-1", "release-2");
    let m3_profile_diff = ci_diff_position(&ci, "m3_reference_gate_root", "debug-1", "release-1");
    let m3_trace = position(
        &ci,
        "cargo test --locked --package pdf-rs-quality --test m3_reference_raster_trace",
    );
    let maturity = position(
        &ci,
        "validate-m1-maturity docs/traceability/capability-profiles.toml",
    );
    let m3_exit = exact_line_position(
        &ci,
        "PDF_RS_M3_REFERENCE_EXIT_INPUT=\"$PWD/$m3_reference_gate_root/debug-1\" cargo test --locked --package pdf-rs-quality --test m3_exit",
    );
    let purity = position(&ci, "check-product-purity .");
    assert!(
        validate_cases < oracle_contract
            && oracle_contract < m2_debug_1
            && m2_debug_1 < m2_debug_2
            && m2_debug_2 < m2_release_1
            && m2_release_1 < m2_release_2
            && m2_release_2 < m2_debug_diff
            && m2_debug_diff < m2_release_diff
            && m2_release_diff < m2_profile_diff
            && m2_profile_diff < m2_exit
            && m2_exit < oracle_model
            && oracle_model < m3_debug_1
            && m3_debug_1 < m3_debug_2
            && m3_debug_2 < m3_release_1
            && m3_release_1 < m3_release_2
            && m3_release_2 < m3_debug_diff
            && m3_debug_diff < m3_release_diff
            && m3_release_diff < m3_profile_diff
            && m3_profile_diff < m3_trace
            && m3_trace < maturity
            && maturity < m3_exit
            && m3_exit < purity,
        "CI order must be cases → oracle contract → M2 replay/exit → independent model → four M3 replays/three diffs → trace → maturity → exit → product closure"
    );
    for replay in ["debug-1", "debug-2", "release-1", "release-2"] {
        assert!(
            ci.contains(&format!(
                "PDF_RS_M3_REFERENCE_GATE_OUTPUT=\"$PWD/$m3_reference_gate_root/{replay}\""
            )),
            "CI is missing fresh M3 replay {replay}"
        );
    }
    assert_ci_diff_triplet(&ci, "m3-reference-gate");
    for forbidden in [
        "update-golden",
        "update_golden",
        "overwrite-expected",
        "accept-pixels",
    ] {
        assert!(
            !ci.contains(forbidden),
            "CI exposes forbidden expected-output mutation mode {forbidden}"
        );
    }
}

fn assert_render_contract(case: CaseSpec, manifest: &CaseManifest) {
    for (key, expected) in [
        ("width", case.width),
        ("height", case.height),
        ("dpr_milli", 1000),
    ] {
        assert_eq!(
            manifest.positive_u64("render", key),
            Some(expected),
            "case={} render.{key} changed",
            case.slug
        );
    }
    for (key, expected) in [
        ("color_profile", "srgb-reference-v1"),
        ("alpha", "straight"),
        ("antialias", "reference-v1"),
        ("renderer_epoch", "reference-raster-v1"),
    ] {
        assert_eq!(
            manifest.string("render", key),
            Some(expected),
            "case={} render.{key} changed",
            case.slug
        );
    }
    assert_eq!(
        manifest.string("tolerance", "mode"),
        Some("exact"),
        "case={} tolerance must remain exact",
        case.slug
    );

    let pixels = case
        .width
        .checked_mul(case.height)
        .expect("bounded render dimensions fit u64");
    assert!(
        manifest
            .positive_u64("budget", "max_image_pixels")
            .is_some_and(|limit| pixels <= limit),
        "case={} render dimensions exceed max_image_pixels",
        case.slug
    );
    let rgba_bytes = pixels.checked_mul(4).expect("bounded RGBA bytes fit u64");
    let output_limit = manifest
        .positive_u64("budget", "max_raster_output_bytes")
        .expect("case has a Raster output-byte budget");
    if case
        .one_less
        .is_some_and(|one_less| one_less.budget_key == "max_raster_output_bytes")
    {
        assert_eq!(output_limit + 1, rgba_bytes);
    } else {
        assert!(
            output_limit >= rgba_bytes,
            "case={} cannot preflight its configured RGBA output",
            case.slug
        );
    }
}

fn assert_expected_flags(case: CaseSpec, manifest: &CaseManifest) {
    for (key, expected) in [
        ("parse", case.flags.parse),
        ("scene", case.flags.scene),
        ("text", case.flags.text),
        ("pixel", case.flags.pixel),
        ("diagnostic", case.flags.diagnostic),
        ("capability", case.flags.capability),
        ("error", case.flags.error),
    ] {
        assert_eq!(
            manifest.boolean("expected", key),
            Some(expected),
            "case={} expected.{key} changed",
            case.slug
        );
    }
}

fn assert_terminal_derivation(case: CaseSpec, manifest: &CaseManifest) {
    let derivation = manifest
        .string("oracle", "derivation")
        .expect("validated case has an oracle derivation");
    if case.outcome == "ready" {
        assert_eq!(case.strict_expected, "success");
    } else {
        for marker in [
            format!("outcome={}", case.outcome),
            format!("stage={}", case.stage),
            format!("diagnostic={}", case.strict_expected),
        ] {
            assert!(
                derivation.contains(&marker),
                "case={} terminal derivation is missing {marker:?}",
                case.slug
            );
        }
    }
    if let Some(one_less) = case.one_less {
        assert_eq!(
            manifest.positive_u64("budget", one_less.budget_key),
            Some(one_less.limit),
            "case={} no longer pins the exact one-less budget",
            case.slug
        );
        for marker in [
            format!("limit-kind={}", one_less.limit_kind),
            format!("limit={}", one_less.limit),
            "consumed=0".to_owned(),
            format!("attempted={}", one_less.attempted),
        ] {
            assert!(
                derivation.contains(&marker),
                "case={} one-less derivation is missing {marker:?}",
                case.slug
            );
        }
    }
}

fn assert_pixel_contract(case: CaseSpec, manifest: &CaseManifest, directory: &Path) {
    let pixel_path = manifest.string("expected", "pixel_artifact");
    let pixel_hash = manifest.string("expected", "pixel_sha256");
    let pixel_oracle = manifest.string("pixel_oracle", "level");
    match case.pixel_authority {
        None => {
            assert_eq!(
                (pixel_path, pixel_hash, pixel_oracle),
                (None, None, None),
                "case={} must not publish or register a pixel oracle",
                case.slug
            );
        }
        Some(authority) => {
            assert_eq!(pixel_path, Some("expected/pixel.json"));
            assert_eq!(pixel_oracle, Some(authority.label()));
            let pixel = read_regular_file(
                &directory.join("expected/pixel.json"),
                "expected pixel artifact",
            );
            assert_eq!(
                pixel_hash,
                Some(digest_reference(&pixel).as_str()),
                "case={} committed pixel digest is stale",
                case.slug
            );
            assert_eq!(
                manifest.boolean("pixel_oracle", "reference_may_generate"),
                Some(authority == PixelAuthority::O3),
                "case={} Reference-generation policy differs from its oracle level",
                case.slug
            );
            if authority != PixelAuthority::O3 {
                assert!(
                    manifest
                        .string_array("pixel_oracle", "reviewers")
                        .is_some_and(|reviewers| !reviewers.is_empty()),
                    "case={} independent pixel authority has no reviewer",
                    case.slug
                );
                assert_eq!(
                    manifest.raw("pixel_oracle", "reference_identity"),
                    None,
                    "O0/O1 pixel authority must not bind a Reference identity"
                );
                assert_eq!(
                    manifest.raw("pixel_oracle", "review_evidence"),
                    None,
                    "O0/O1 pixel authority must not masquerade as O3 review"
                );
            }
        }
    }
}

fn canonical_mixed_review(manifest: &CaseManifest) -> String {
    let derivation = manifest
        .string("pixel_oracle", "derivation")
        .expect("mixed derivation");
    let pixel_path = manifest
        .string("expected", "pixel_artifact")
        .expect("mixed pixel path");
    let pixel_hash = manifest
        .string("expected", "pixel_sha256")
        .expect("mixed pixel hash");
    let identity = manifest
        .string("pixel_oracle", "reference_identity")
        .expect("mixed identity");
    format!(
        "{{\"case_id\":\"{}\",\"derivation\":\"{derivation}\",\"independent\":true,\"pixel_reference\":\"{pixel_path}#{pixel_hash}\",\"reference_identity\":\"{identity}\",\"reviewers\":[\"spec-conformance\",\"parser-security\"],\"schema\":1,\"verdict\":\"pass\"}}",
        manifest.case_id()
    )
}

fn expected_case_tree() -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    for case in CASES {
        for relative in ["case.toml", "input.pdf"] {
            expected.insert(format!("{}/{relative}", case.slug));
        }
        if case.pixel_authority.is_some() {
            for relative in ["expected/oracle.md", "expected/pixel.json"] {
                expected.insert(format!("{}/{relative}", case.slug));
            }
        }
        if case.pixel_authority == Some(PixelAuthority::O3) {
            for relative in ["evidence/reference-identity.json", "evidence/review.json"] {
                expected.insert(format!("{}/{relative}", case.slug));
            }
        }
    }
    expected
}

fn expected_registered_case_paths() -> BTreeSet<String> {
    expected_case_tree()
        .into_iter()
        .map(|path| format!("tests/cases/raster/m3-reference/{path}"))
        .collect()
}

fn expected_artifact_paths() -> BTreeSet<String> {
    let mut expected = BTreeSet::from(["result.json".to_owned()]);
    for case in CASES {
        let prefix = format!("raster/m3-reference/{}", case.slug);
        expected.insert(format!("{prefix}/audit.json"));
        if case.flags.scene {
            expected.insert(format!("{prefix}/scene.json"));
        }
        if case.flags.pixel {
            expected.insert(format!("{prefix}/pixel.json"));
        }
    }
    assert_eq!(expected.len(), 25, "formal artifact topology changed");
    expected
}

fn assert_fresh_canonical_artifact_tree() {
    let Some(output_root) = env::var_os("PDF_RS_M3_REFERENCE_EXIT_INPUT") else {
        return;
    };
    let output_root = PathBuf::from(output_root);
    assert!(
        !output_root.as_os_str().is_empty()
            && output_root
                .components()
                .all(|component| matches!(component, Component::RootDir | Component::Normal(_))),
        "fresh M3 exit input must be a normal absolute or repository-relative path"
    );
    assert_regular_directory(&output_root, "fresh M3 replay root");
    let artifact_paths = collect_regular_tree(&output_root);
    assert_eq!(
        artifact_paths,
        expected_artifact_paths(),
        "fresh M3 replay is not the exact 25-file canonical topology"
    );

    let result = read_regular_file(&output_root.join("result.json"), "fresh canonical result");
    assert_eq!(
        u64::try_from(result.len()).expect("result length fits u64"),
        CANONICAL_RESULT_BYTES,
        "fresh canonical result byte count changed"
    );
    assert_eq!(
        digest(&result),
        CANONICAL_RESULT_SHA256,
        "fresh canonical result hash changed"
    );

    let mut tree = Vec::new();
    let mut total_bytes = 0_u64;
    for relative in &artifact_paths {
        let bytes = read_regular_file(
            &output_root.join(relative),
            "fresh canonical artifact-tree file",
        );
        total_bytes = total_bytes
            .checked_add(u64::try_from(bytes.len()).expect("artifact length fits u64"))
            .expect("canonical artifact total byte count fits u64");
        tree.extend_from_slice(format!("{relative}#sha256:{}\n", digest(&bytes)).as_bytes());
    }
    assert_eq!(
        u64::try_from(tree.len()).expect("artifact-tree length fits u64"),
        CANONICAL_ARTIFACT_TREE_BYTES,
        "fresh canonical artifact-tree byte count changed"
    );
    assert_eq!(
        digest(&tree),
        CANONICAL_ARTIFACT_TREE_SHA256,
        "fresh canonical artifact-tree hash changed"
    );
    assert_eq!(
        total_bytes, CANONICAL_ARTIFACT_TOTAL_BYTES,
        "fresh canonical artifact total byte count changed"
    );
}

fn expected_final_review_subject_paths() -> BTreeSet<String> {
    let mut expected = expected_registered_case_paths();
    expected.extend(
        [
            "tools/quality/tests/m3_reference_gate.rs",
            "tools/quality/tests/m3_reference_gate_support/mod.rs",
            "tools/quality/tests/m3_reference_gate_support/artifact.rs",
            "tools/quality/tests/m3_reference_gate_support/fixture.rs",
            "tools/quality/tests/m3_reference_gate_support/pending.rs",
            "tools/quality/tests/m3_reference_gate_support/registry.rs",
            "tools/quality/tests/m3_reference_oracle_model.rs",
            "tools/quality/tests/m3_raster_oracle_contract.rs",
            "tools/quality/tests/m3_reference_raster_trace.rs",
            "tools/quality/tests/m3_exit.rs",
            "tools/quality/tests/m2_exit.rs",
            "tools/quality/tests/support/evidence.rs",
            "tools/quality/src/case_contract.rs",
            "tools/quality/src/main.rs",
            "tools/quality/src/manifest.rs",
            "tools/quality/src/maturity.rs",
            "tools/quality/Cargo.toml",
            "tools/quality/PROVENANCE.md",
            "Cargo.lock",
            "docs/traceability/capability-profiles.toml",
            "docs/traceability/data-ledger.toml",
            "docs/traceability/feature-map.toml",
            "docs/traceability/spec-map.toml",
            "docs/traceability/evidence/m2/scene-gate/normative-replay.toml",
            "docs/traceability/evidence/m3/reference-raster-integration/independent-review.toml",
            "plan/m3.toml",
            "plan/r0.toml",
            "scripts/ci.sh",
            NORMATIVE_REPLAY,
            CLOSURE_INTENT,
            MATURITY_SUBJECT_REPORT,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    expected.extend(REFERENCE_SOURCES.into_iter().map(str::to_owned));
    expected.insert("pdf-rs/raster/tests/reference_integrated_renderer.rs".to_owned());
    expected.extend(
        MATURITY_ARTIFACTS
            .iter()
            .map(|(_, path, _, _)| (*path).to_owned()),
    );
    assert_eq!(
        expected.len(),
        85,
        "formal final-review subject closure changed"
    );
    expected
}

fn expected_case_subjects(oracle: &str) -> BTreeSet<String> {
    expected_case_tree()
        .into_iter()
        .filter(|path| {
            let slug = path.split('/').next().expect("case path has a slug");
            CASES
                .iter()
                .find(|case| case.slug == slug)
                .is_some_and(|case| case.oracle == oracle)
        })
        .map(|path| format!("tests/cases/raster/m3-reference/{path}"))
        .collect()
}

fn assert_case_subject_partition(subjects: &[String], oracle: &str) {
    let actual = subject_paths(subjects)
        .into_iter()
        .filter(|path| path.starts_with("tests/cases/raster/m3-reference/"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual,
        expected_case_subjects(oracle),
        "{oracle} maturity artifact does not bind its exact corpus partition"
    );
    for required in [
        "tools/quality/tests/m3_reference_gate.rs",
        "tools/quality/tests/m3_reference_gate_support/artifact.rs",
        "tools/quality/tests/m3_reference_gate_support/fixture.rs",
        "tools/quality/tests/m3_reference_gate_support/mod.rs",
        "tools/quality/tests/m3_reference_gate_support/pending.rs",
        "tools/quality/tests/m3_reference_gate_support/registry.rs",
        "tools/quality/tests/m3_reference_oracle_model.rs",
    ] {
        assert!(
            subject_paths(subjects).contains(required),
            "{oracle} maturity artifact is missing runner subject {required}"
        );
    }
}

fn assert_reference_subjects(subjects: &[String]) {
    let paths = subject_paths(subjects);
    for required in REFERENCE_SOURCES {
        assert!(
            paths.contains(required),
            "reference artifact is missing implementation source {required}"
        );
    }
    assert!(
        paths.contains("pdf-rs/raster/tests/reference_integrated_renderer.rs"),
        "reference artifact is missing its executable integration test"
    );
    let production = paths
        .iter()
        .filter(|path| {
            path.starts_with("pdf-rs/raster/src/reference/") || **path == "pdf-rs/raster/src/lib.rs"
        })
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        production,
        REFERENCE_SOURCES.into_iter().collect(),
        "reference artifact implementation source set is not exact"
    );
}

fn assert_maturity_artifact(
    artifact: &RootToml,
    id: &str,
    role: &str,
    oracle: &str,
    subject_kind: &str,
) {
    artifact.expect_unsigned("schema", 1).expect("schema");
    artifact
        .expect_string("type", "maturity-evidence")
        .expect("type");
    artifact.expect_string("id", id).expect("id");
    artifact
        .expect_string("profile", PROFILE_ID)
        .expect("profile");
    artifact
        .expect_string("feature", FEATURE_ID)
        .expect("feature");
    artifact.expect_string("role", role).expect("role");
    artifact.expect_string("oracle", oracle).expect("oracle");
    artifact
        .expect_bool("eligibility", true)
        .expect("eligibility");
    artifact
        .expect_bool("registered", true)
        .expect("registered");
    artifact.expect_bool("gating", true).expect("gating");
    artifact
        .expect_bool("external_observation", false)
        .expect("external observation");
    artifact.expect_string("target", TARGET).expect("target");
    artifact
        .expect_array("requirements", &REQUIREMENTS)
        .expect("requirements");
    artifact
        .expect_string("subject_kind", subject_kind)
        .expect("subject kind");
    assert!(
        !artifact.array("subjects").expect("subjects").is_empty(),
        "{id} has no subjects"
    );
    assert!(
        !artifact
            .array("executed_tests")
            .expect("executed tests")
            .is_empty(),
        "{id} has no executed tests"
    );
    artifact
        .expect_array("fuzz_targets", &[])
        .expect("fuzz targets");
    artifact
        .expect_array("benchmarks", &[])
        .expect("benchmarks");
    artifact.expect_string("verdict", "pass").expect("verdict");
    if role != "independent-review" {
        artifact
            .expect_array("cross_references", &[])
            .expect("source artifact cross references");
    }

    let expected_tests: &[&str] = match role {
        "reference-implementation" => &["pdf-rs/raster::reference_integrated_renderer"],
        "o0-case" | "o1-case" => &[
            "tools/quality::m3_reference_gate",
            "tools/quality::m3_reference_oracle_model",
        ],
        "independent-review" => &[
            "tools/quality::m3_reference_gate",
            "tools/quality::m3_reference_oracle_model",
        ],
        _ => panic!("unexpected maturity role {role}"),
    };
    artifact
        .expect_array("executed_tests", expected_tests)
        .unwrap_or_else(|error| panic!("{id} executed tests: {error}"));
}

fn assert_independent_subject_report(root: &Path, artifact: &RootToml) {
    let report = RootToml::parse(&read_utf8(&root.join(MATURITY_SUBJECT_REPORT)))
        .expect("maturity subject report is strict TOML");
    report.expect_unsigned("schema", 1).expect("report schema");
    report
        .expect_string("type", "maturity-subject-report")
        .expect("report type");
    report
        .expect_string("id", "subject.m3.reference-raster-v1.independent-review")
        .expect("report id");
    report
        .expect_string("evidence_kind", "independent-review-report")
        .expect("report kind");
    report
        .expect_string("feature", FEATURE_ID)
        .expect("report feature");
    report
        .expect_string("target", TARGET)
        .expect("report target");
    assert_eq!(
        report.array("executed_tests").expect("report tests"),
        artifact.array("executed_tests").expect("artifact tests"),
        "independent report and artifact executed tests differ"
    );
    report
        .expect_array("fuzz_targets", &[])
        .expect("report fuzz targets");
    report
        .expect_array("benchmarks", &[])
        .expect("report benchmarks");
    report
        .expect_array("reviewers", &["spec-conformance", "parser-security"])
        .expect("report reviewers");
    report
        .expect_bool("independent", true)
        .expect("report independence");
    report
        .expect_string("verdict", "pass")
        .expect("report verdict");
}

fn expected_oracle(role: &str) -> &'static str {
    match role {
        "reference-implementation" => "O1",
        "o0-case" => "O0",
        "o1-case" => "O1",
        "independent-review" => "INTERNAL",
        _ => "INVALID",
    }
}

fn expected_subject_kind(role: &str) -> &'static str {
    match role {
        "reference-implementation" => "reference-implementation-and-tests",
        "o0-case" => "normative-case",
        "o1-case" => "analytic-case",
        "independent-review" => "independent-review-report",
        _ => "invalid",
    }
}

fn subject_paths(subjects: &[String]) -> BTreeSet<&str> {
    subjects
        .iter()
        .map(|subject| {
            let (locator, _) = split_subject_entry(subject)
                .unwrap_or_else(|error| panic!("invalid subject reference {subject:?}: {error}"));
            if let Some((path, commit)) = locator.rsplit_once('@') {
                assert!(
                    !path.contains('@'),
                    "subject reference has multiple commit pins: {subject:?}"
                );
                validate_commit_id(commit).unwrap_or_else(|error| {
                    panic!("invalid subject commit in {subject:?}: {error}")
                });
                path
            } else {
                locator
            }
        })
        .collect()
}

fn reviewed_subject_paths(subjects: &[String], expected_commit: &str) -> BTreeSet<String> {
    subjects
        .iter()
        .map(|subject| {
            let (locator, _) = split_subject_entry(subject)
                .unwrap_or_else(|error| panic!("invalid reviewed subject {subject:?}: {error}"));
            pinned_locator_path(locator, expected_commit).to_owned()
        })
        .collect()
}

fn pinned_locator_paths(locators: &[String], expected_commit: &str) -> BTreeSet<String> {
    locators
        .iter()
        .map(|locator| pinned_locator_path(locator, expected_commit).to_owned())
        .collect()
}

fn pinned_locator_path<'a>(locator: &'a str, expected_commit: &str) -> &'a str {
    let (path, commit) = locator
        .rsplit_once('@')
        .unwrap_or_else(|| panic!("reviewed subject locator is not commit-pinned: {locator:?}"));
    assert!(
        !path.contains('@'),
        "reviewed subject locator has multiple commit pins: {locator:?}"
    );
    validate_commit_id(commit)
        .unwrap_or_else(|error| panic!("invalid reviewed subject commit in {locator:?}: {error}"));
    assert_eq!(
        commit, expected_commit,
        "reviewed subject locator is pinned to the wrong commit: {locator:?}"
    );
    path
}

fn split_content_reference(reference: &str) -> Result<(&str, &str), String> {
    let (path, hash) = reference
        .split_once("#sha256:")
        .ok_or_else(|| format!("content reference lacks SHA-256: {reference}"))?;
    if path.is_empty() || path.contains(['#', '\\', ':']) {
        return Err(format!("invalid content-reference path {path:?}"));
    }
    if Path::new(path).is_absolute()
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(format!("non-normal content-reference path {path:?}"));
    }
    validate_sha256(hash)?;
    Ok((path, hash))
}

fn assert_root_key_absent(document: &str, forbidden: &str) {
    let present = document
        .lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .filter_map(|line| line.split_once('='))
        .any(|(key, _)| key.trim() == forbidden);
    assert!(
        !present,
        "normative replay must use one exact content-addressed registered_case_tree, not legacy root field {forbidden}"
    );
}

fn record(document: &str, table: &str, id: &str) -> RootToml {
    let header = format!("[[{table}]]");
    let id_line = format!("id = \"{id}\"");
    let matching = document
        .split(&header)
        .skip(1)
        .map(|chunk| chunk.split("\n[[").next().unwrap_or(chunk))
        .filter(|body| body.lines().any(|line| line.trim() == id_line))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one [[{table}]] record {id}, found {}",
        matching.len()
    );
    let found = RootToml::parse(matching[0])
        .unwrap_or_else(|error| panic!("cannot parse [[{table}]] record {id}: {error}"));
    assert!(
        found.string("id").is_ok_and(|candidate| candidate == id),
        "[[{table}]] record identity changed while parsing {id}"
    );
    found
}

fn case_directory(root: &Path, case: CaseSpec) -> PathBuf {
    root.join("tests/cases/raster/m3-reference").join(case.slug)
}

fn collect_regular_tree(root: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    collect_regular_tree_inner(root, root, &mut files);
    files
}

fn collect_regular_tree_inner(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
    assert_regular_directory(directory, "case-tree directory");
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("case-tree entry is readable"))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("cannot inspect {}: {error}", path.display()));
        assert!(
            !metadata.file_type().is_symlink(),
            "M3 case tree may not contain symlink {}",
            path.display()
        );
        if metadata.file_type().is_dir() {
            collect_regular_tree_inner(root, &path, files);
        } else {
            assert!(
                metadata.file_type().is_file(),
                "M3 case tree entry is not a regular file: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(root)
                .expect("case-tree path remains below root")
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            assert!(files.insert(relative), "duplicate case-tree path");
        }
    }
}

fn direct_directory_names(root: &Path) -> BTreeSet<String> {
    fs::read_dir(root)
        .expect("case root is readable")
        .map(|entry| {
            let entry = entry.expect("case root entry is readable");
            let metadata = entry.file_type().expect("case root entry type is readable");
            assert!(
                !metadata.is_symlink() && metadata.is_dir(),
                "{} must be a regular case directory",
                entry.path().display()
            );
            entry.file_name().into_string().expect("case slug is UTF-8")
        })
        .collect()
}

fn assert_regular_directory(path: &Path, label: &str) {
    assert_no_symlink_components(path);
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("{label} {} is not inspectable: {error}", path.display()));
    assert!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "{label} {} is not a regular directory",
        path.display()
    );
}

fn assert_regular_file(path: &Path, label: &str) {
    assert_no_symlink_components(path);
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("{label} {} is not inspectable: {error}", path.display()));
    assert!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "{label} {} is not a regular file",
        path.display()
    );
}

fn assert_no_symlink_components(path: &Path) {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Normal(_))
            && let Ok(metadata) = fs::symlink_metadata(&current)
        {
            assert!(
                !metadata.file_type().is_symlink(),
                "path crosses symbolic link {}",
                current.display()
            );
        }
    }
}

fn read_regular_file(path: &Path, label: &str) -> Vec<u8> {
    assert_regular_file(path, label);
    fs::read(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn read_utf8(path: &Path) -> String {
    String::from_utf8(read_regular_file(path, "repository file"))
        .unwrap_or_else(|error| panic!("{} is not UTF-8: {error}", path.display()))
}

fn digest(bytes: &[u8]) -> String {
    hex_digest(&sha256(bytes).expect("bounded repository bytes fit SHA-256 framing"))
}

fn digest_reference(bytes: &[u8]) -> String {
    format!("sha256:{}", digest(bytes))
}

fn digest_file(path: &Path) -> String {
    digest(&read_regular_file(path, "digest subject"))
}

fn git_output(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("git executes");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn position(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing ordered CI marker {needle:?}"))
}

fn exact_line_position(document: &str, expected: &str) -> usize {
    let mut offset = 0_usize;
    for line in document.split_inclusive('\n') {
        if line.trim() == expected {
            return offset;
        }
        offset += line.len();
    }
    panic!("missing exact ordered CI command line {expected:?}");
}

fn assert_ci_diff_triplet(ci: &str, gate: &str) {
    let variable = match gate {
        "m2-scene-gate" => "m2_scene_gate_root",
        "m3-reference-gate" => "m3_reference_gate_root",
        _ => panic!("unregistered CI gate {gate}"),
    };
    for (left, right) in [
        ("debug-1", "debug-2"),
        ("release-1", "release-2"),
        ("debug-1", "release-1"),
    ] {
        let command = ci_diff_command(variable, left, right);
        assert_eq!(
            ci.matches(command.as_str()).count(),
            1,
            "CI must contain exactly one {gate} comparison command for {left}/{right}"
        );
    }
    assert_eq!(
        ci.matches("diff --recursive --brief").count(),
        6,
        "CI must contain exactly three M2 and three M3 recursive byte comparisons"
    );
}

fn ci_diff_position(ci: &str, variable: &str, left: &str, right: &str) -> usize {
    position(ci, &ci_diff_command(variable, left, right))
}

fn ci_diff_command(variable: &str, left: &str, right: &str) -> String {
    format!(
        "diff --recursive --brief \\\n    \"${variable}/{left}\" \\\n    \"${variable}/{right}\""
    )
}

fn string_set<'a>(values: &'a [&'a str]) -> BTreeSet<&'a str> {
    values.iter().copied().collect()
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root is canonical")
}
