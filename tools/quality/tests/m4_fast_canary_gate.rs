use pdf_rs_digest::{hex_digest, sha256};

#[path = "support/evidence.rs"]
mod evidence_support;

use evidence_support::{RootToml, array_table_records};

const REGISTRY: &str = include_str!("../../../docs/traceability/canary-profiles.toml");
const PROMOTION: &str =
    include_str!("../../../docs/traceability/evidence/m4/fast-cpu-canary/promotion.toml");
const HOLDOUT: &[u8] =
    include_bytes!("../../../docs/traceability/evidence/m4/fast-cpu-canary/holdout.toml");
const FUZZ: &[u8] =
    include_bytes!("../../../docs/traceability/evidence/m4/fast-cpu-canary/fuzz-replay.toml");
const PERFORMANCE: &[u8] =
    include_bytes!("../../../docs/traceability/evidence/m4/fast-cpu-canary/performance.toml");
const PURITY: &[u8] =
    include_bytes!("../../../docs/traceability/evidence/m4/fast-cpu-canary/product-purity.toml");
const FAST_KERNELS: &[u8] = include_bytes!("../../../pdf-rs/fast-raster/src/fast/kernels.rs");
const FAST_RENDER: &[u8] = include_bytes!("../../../pdf-rs/fast-raster/src/fast/render.rs");
const FAST_OWNED: &[u8] = include_bytes!("../../../pdf-rs/fast-raster/src/fast/owned.rs");
const VIEWER: &[u8] = include_bytes!("../../../runtime/viewer/src/lib.rs");
const ELECTRON_BRIDGE_RUST: &[u8] = include_bytes!("../../../platform/electron-bridge/src/lib.rs");
const ELECTRON_BRIDGE_JS: &[u8] = include_bytes!("../../../platform/electron/src/bridge.mjs");
const ROLLBACK_TEST: &[u8] = include_bytes!("../../../platform/electron/test/bridge.test.mjs");
const SCHEMA_RECORD: &[u8] = include_bytes!("../../../protocol/generated/schema-hash.txt");
const READABLE_PDF: &[u8] = include_bytes!("../../../tests/desktop/readable-preview.pdf");

const PROMOTION_SHA256: &str = "589a198f59292c215fbb5842b7fd6a4c70d90f2b94986fadcee388791199e6af";

#[test]
fn registered_fast_canary_candidate_binds_qualification_and_stays_default_off() {
    let root = root(PROMOTION);
    root.expect_unsigned("schema", 1).expect("schema");
    root.expect_string("profile_id", "m4.fast-cpu-r0-basic-page.v1")
        .expect("profile");
    root.expect_string("state", "CANARY_CANDIDATE")
        .expect("candidate state");
    root.expect_bool("promotion_approved", false)
        .expect("promotion remains unapproved");
    root.expect_bool("enabled_by_default", false)
        .expect("candidate remains default-off");
    root.expect_string(
        "implementation_commit",
        "85a76d302e44fb7a49880f4846f74f6deab18772",
    )
    .expect("implementation commit");
    root.expect_string(
        "implementation_tree",
        "8fbd4ffc2947c5c262384a91b49304328f2e73ec",
    )
    .expect("implementation tree");

    assert_eq!(digest(PROMOTION.as_bytes()), PROMOTION_SHA256);
    let profiles = array_table_records(REGISTRY, "profile").expect("CANARY registry");
    assert_eq!(profiles.len(), 1);
    let registered = &profiles[0];
    registered
        .expect_string("id", "m4.fast-cpu-r0-basic-page.v1")
        .expect("registered profile");
    registered
        .expect_string("state", "CANARY_CANDIDATE")
        .expect("registered state");
    registered
        .expect_bool("enabled_by_default", false)
        .expect("registered default");
    registered
        .expect_bool("promotion_approved", false)
        .expect("registered approval");
    registered
        .expect_string(
            "promotion",
            &format!(
                "docs/traceability/evidence/m4/fast-cpu-canary/promotion.toml#sha256:{PROMOTION_SHA256}"
            ),
        )
        .expect("content-addressed promotion");

    let identity = table(PROMOTION, "identity");
    identity
        .expect_string("capability_flag", "PDF_RS_FAST_CPU_CANARY_V1")
        .expect("capability flag");
    identity
        .expect_string("cohort", "m4-r0-basic-page-local-v1")
        .expect("cohort");
    identity
        .expect_unsigned("renderer_epoch", 1)
        .expect("renderer epoch");
    identity
        .expect_string(
            "cohort_sha256",
            "cef88fcfcaa0d31cc74d9958e3185a9216cc7b638a3240ca7e3813a01b26ae42",
        )
        .expect("cohort digest");
    identity
        .expect_string(
            "canonical_schema_sha256",
            "466af0a81170c4e30677ce9fbff91d8a89fd62c9885b025519747bf29f4e8569",
        )
        .expect("canonical schema");

    let implementation = table(PROMOTION, "implementation");
    for (field, bytes) in [
        ("fast_kernels_sha256", FAST_KERNELS),
        ("fast_render_sha256", FAST_RENDER),
        ("fast_owned_sha256", FAST_OWNED),
        ("viewer_sha256", VIEWER),
        ("electron_bridge_rust_sha256", ELECTRON_BRIDGE_RUST),
        ("electron_bridge_js_sha256", ELECTRON_BRIDGE_JS),
        ("rollback_test_sha256", ROLLBACK_TEST),
        ("schema_record_sha256", SCHEMA_RECORD),
        ("readable_pdf_sha256", READABLE_PDF),
    ] {
        implementation
            .expect_string(field, &digest(bytes))
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }

    let evidence = table(PROMOTION, "evidence");
    for (field, bytes) in [
        ("holdout_sha256", HOLDOUT),
        ("fuzz_replay_sha256", FUZZ),
        ("performance_sha256", PERFORMANCE),
        ("product_purity_sha256", PURITY),
    ] {
        evidence
            .expect_string(field, &digest(bytes))
            .unwrap_or_else(|error| panic!("{field}: {error}"));
    }

    verify_holdout();
    verify_fuzz();
    verify_performance();
    verify_purity();
    verify_exposure();
    verify_pending_review();
}

fn verify_holdout() {
    let document = std::str::from_utf8(HOLDOUT).expect("holdout UTF-8");
    let root = root(document);
    root.expect_string("status", "passed")
        .expect("holdout status");
    root.expect_string("cohort", "m4-r0-basic-page-local-v1")
        .expect("holdout cohort");
    let denominators = table(document, "denominators");
    denominators
        .expect_unsigned("eligible_pages", 1_000)
        .expect("eligible pages");
    denominators
        .expect_unsigned("excluded_pages", 0)
        .expect("excluded pages");
    let outcomes = table(document, "outcomes");
    for field in [
        "panic",
        "hang",
        "partial_pixels",
        "unexpected_unsupported",
        "external_engine_fallback",
        "critical_findings",
        "major_findings",
        "maximum_channel_delta",
        "changed_channels",
    ] {
        outcomes
            .expect_unsigned(field, 0)
            .unwrap_or_else(|error| panic!("holdout {field}: {error}"));
    }
}

fn verify_fuzz() {
    let document = std::str::from_utf8(FUZZ).expect("fuzz UTF-8");
    let root = root(document);
    root.expect_string("status", "passed").expect("fuzz status");
    root.expect_string("target", "m4fastraster")
        .expect("fuzz target");
    let replay = table(document, "replay");
    replay
        .expect_unsigned("libfuzzer_seed", 20_260_718)
        .expect("fuzz seed");
    replay.expect_unsigned("runs", 64).expect("fuzz runs");
    for field in [
        "crash_artifacts",
        "panics",
        "timeouts",
        "partial_tile_sets",
        "pixel_mismatches",
    ] {
        replay
            .expect_unsigned(field, 0)
            .unwrap_or_else(|error| panic!("fuzz {field}: {error}"));
    }
}

fn verify_performance() {
    let document = std::str::from_utf8(PERFORMANCE).expect("performance UTF-8");
    let root = root(document);
    root.expect_string("status", "passed")
        .expect("performance status");
    root.expect_bool("external_engine_comparison", false)
        .expect("same-Native baseline");

    for (section, expected_count) in [
        ("viewer.fast_first_preview", 21),
        ("viewer.reference_first_preview", 21),
        ("viewer.fast_first_full_viewport", 21),
        ("viewer.reference_first_full_viewport", 21),
        ("component.first_tile", 21),
        ("component.full_owned_job", 21),
        ("component.tile_poll", 84),
        ("component.cold_bins_full_render", 21),
        ("component.reused_bins_full_render", 21),
        ("component.cancellation", 21),
    ] {
        verify_sample_statistics(document, section, expected_count);
    }

    let fast_preview = unsigned(document, "viewer.fast_first_preview", "p95_ns");
    let reference_preview = unsigned(document, "viewer.reference_first_preview", "p95_ns");
    let fast_viewport = unsigned(document, "viewer.fast_first_full_viewport", "p95_ns");
    let reference_viewport = unsigned(document, "viewer.reference_first_full_viewport", "p95_ns");
    assert!(u128::from(fast_preview) * 100 <= u128::from(reference_preview) * 110);
    assert!(u128::from(fast_viewport) * 100 <= u128::from(reference_viewport) * 110);
    assert_eq!(
        unsigned(document, "component.cancellation", "p95_ns"),
        25_250
    );
}

fn verify_purity() {
    let document = std::str::from_utf8(PURITY).expect("purity UTF-8");
    let root = root(document);
    root.expect_string("status", "passed")
        .expect("purity status");
    root.expect_bool("external_engine_fallback", false)
        .expect("no fallback");
    let manifest = table(document, "manifest_preflight");
    manifest
        .expect_unsigned("allowlisted_product_packages", 24)
        .expect("product packages");
    manifest
        .expect_unsigned("forbidden_manifest_tokens", 0)
        .expect("manifest purity");
    let release = table(document, "release_closure");
    release
        .expect_bool("fresh_external_target", true)
        .expect("fresh release target");
    release
        .expect_unsigned("product_packages", 24)
        .expect("release packages");
    for field in ["native_artifacts", "unknown_artifacts"] {
        release
            .expect_unsigned(field, 0)
            .unwrap_or_else(|error| panic!("purity {field}: {error}"));
    }
}

fn verify_exposure() {
    let bridge_js = std::str::from_utf8(ELECTRON_BRIDGE_JS).expect("bridge JS");
    assert!(
        bridge_js.contains("export const FAST_CPU_CANARY_COHORT = \"m4-r0-basic-page-local-v1\";")
    );
    assert!(bridge_js.contains("delete environment.PDF_RS_FAST_CPU_CANARY_V1;"));
    assert!(bridge_js.contains("invalid-renderer-cohort"));

    let bridge_rust = std::str::from_utf8(ELECTRON_BRIDGE_RUST).expect("bridge Rust");
    assert!(bridge_rust.contains("PDF_RS_FAST_CPU_CANARY_V1"));
    assert!(bridge_rust.contains("m4-r0-basic-page-local-v1"));
    assert!(bridge_rust.contains("_ => NativeRendererKind::ReferenceCpu"));

    let viewer = std::str::from_utf8(VIEWER).expect("viewer Rust");
    assert!(viewer.contains(
        "self.render_page_with_renderer(page_index, width, NativeRendererKind::ReferenceCpu)"
    ));
    let rollback = std::str::from_utf8(ROLLBACK_TEST).expect("rollback test");
    assert!(rollback.contains("Fast CPU CANARY rolls back to Reference"));
    assert!(rollback.contains("assert.equal(surface.renderer, \"fast-cpu-v1\")"));
    assert!(rollback.contains("assert.equal(surface.renderer, \"reference-cpu-v1\")"));

    let exposure = table(PROMOTION, "exposure");
    exposure
        .expect_bool("rollback_rehearsed", true)
        .expect("rollback rehearsal");
    exposure
        .expect_bool("unsupported_semantics_unchanged", true)
        .expect("unsupported semantics");
    exposure
        .expect_bool("automatic_rollout", false)
        .expect("no automatic rollout");
    exposure
        .expect_bool("remote_rollout", false)
        .expect("no remote rollout");
}

fn verify_pending_review() {
    let review = table(PROMOTION, "review");
    review
        .expect_array("required_roles", &["graphics-color", "quality-corpus"])
        .expect("review roles");
    review
        .expect_array("completed_reviewers", &[])
        .expect("no fabricated reviewers");
    review
        .expect_bool("independent_review_complete", false)
        .expect("review remains pending");
    review
        .expect_string("decision", "PENDING")
        .expect("review decision");
}

fn verify_sample_statistics(document: &str, section: &str, expected_count: usize) {
    let mut samples = numeric_array(document, section, "raw_samples_ns");
    assert_eq!(samples.len(), expected_count, "{section} sample count");
    samples.sort_unstable();
    assert_eq!(
        unsigned(document, section, "median_ns"),
        samples[samples.len() / 2],
        "{section} median"
    );
    assert_eq!(
        unsigned(document, section, "p95_ns"),
        samples[(samples.len() * 95).div_ceil(100) - 1],
        "{section} p95"
    );
    assert_eq!(
        unsigned(document, section, "p99_ns"),
        samples[(samples.len() * 99).div_ceil(100) - 1],
        "{section} p99"
    );
}

fn table(document: &str, name: &str) -> RootToml {
    RootToml::parse(table_body(document, name))
        .unwrap_or_else(|error| panic!("[{name}] cannot be parsed: {error}"))
}

fn root(document: &str) -> RootToml {
    let end = document.find("\n[").unwrap_or(document.len());
    RootToml::parse(&document[..end]).expect("document root cannot be parsed")
}

fn table_body<'a>(document: &'a str, name: &str) -> &'a str {
    let header = format!("[{name}]\n");
    let start = document
        .find(&header)
        .unwrap_or_else(|| panic!("missing table [{name}]"))
        + header.len();
    let rest = &document[start..];
    let end = rest.find("\n[").map_or(rest.len(), |offset| offset + 1);
    &rest[..end]
}

fn unsigned(document: &str, section: &str, key: &str) -> u64 {
    let body = table_body(document, section);
    let prefix = format!("{key} = ");
    body.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing [{section}] {key}"))
        .parse()
        .unwrap_or_else(|_| panic!("[{section}] {key} is not a u64"))
}

fn numeric_array(document: &str, section: &str, key: &str) -> Vec<u64> {
    let body = table_body(document, section);
    let prefix = format!("{key} = [");
    let start = body
        .find(&prefix)
        .unwrap_or_else(|| panic!("missing [{section}] {key}"))
        + prefix.len();
    let values = &body[start..];
    let end = values
        .find(']')
        .unwrap_or_else(|| panic!("unclosed [{section}] {key}"));
    values[..end]
        .split(',')
        .filter_map(|value| {
            let value = value.trim();
            (!value.is_empty()).then(|| {
                value
                    .parse()
                    .unwrap_or_else(|_| panic!("[{section}] {key} contains a non-u64"))
            })
        })
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    hex_digest(&sha256(bytes).expect("bounded SHA-256"))
}
