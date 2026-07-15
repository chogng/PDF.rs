use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_xref_core_only_depends_on_bytes_and_syntax_and_has_no_platform_io() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("crate manifest must be readable during repository tests");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("crate manifest declares an explicit dependency table")
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dependency_body,
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ],
        "core/xref may depend only on the lower-level core/bytes and core/syntax crates"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/xref must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "core/xref source selection is non-empty"
    );

    let forbidden = [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "pdfium",
        "mupdf",
        "pdf.js",
    ];
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product xref-core token {token:?} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn traceability_maps_are_versioned_together_and_register_xref() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/xref has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable during repository tests");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec traceability map must be readable during repository tests");

    assert_eq!(top_level_version(&feature_map), Some("0.33.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.33.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps must advance as one semantic traceability version"
    );

    let feature = record_with_id(&feature_map, "feature", "core.traditional-xref")
        .expect("the traditional-xref feature record must exist");
    assert!(feature.contains("profile = \"m1.traditional-xref.v1\""));
    assert!(feature.contains("modules = [\"core/xref\"]"));
    assert!(feature.contains("core/xref::traditional_xref"));
    assert!(feature.contains("core/xref::limit_config"));
    assert!(feature.contains("core/xref::source_error_policy"));
    assert!(feature.contains("core/xref::repository_policy"));

    let attestation = record_with_id(
        &feature_map,
        "feature",
        "core.strict-base-revision-attestation",
    )
    .expect("the strict base-revision attestation feature record must exist");
    assert!(attestation.contains("profile = \"m1.strict-base-revision-attestation.v1\""));
    assert!(attestation.contains("modules = [\"core/document\"]"));
    assert!(attestation.contains("core/document::revision_attestation"));
    assert!(attestation.contains("core/document::revision_attestation_limit_config"));
    assert!(attestation.contains("tools/quality::native_object_loop"));

    let strict_open = record_with_id(&feature_map, "feature", "core.strict-base-open")
        .expect("the strict base-open feature record must exist");
    assert!(strict_open.contains("state = \"PLANNED\""));
    assert!(strict_open.contains("profile = \"m1.strict-base-open.v1\""));
    assert!(strict_open.contains("modules = [\"core/document\"]"));
    assert!(strict_open.contains("core/document::strict_base_open"));
    assert!(strict_open.contains("core/document::repository_policy"));
    assert!(strict_open.contains("tools/quality::native_object_loop"));
    assert!(strict_open.contains("tools/quality::native_range_resume_loop"));
    assert!(strict_open.contains("tools/quality::native_strict_open_runtime_loop"));

    let access = record_with_id(&feature_map, "feature", "core.attested-object-access")
        .expect("the proof-preserving object-access feature record must exist");
    assert!(access.contains("profile = \"m1.attested-object-access.v1\""));
    assert!(access.contains("modules = [\"core/document\"]"));
    assert!(access.contains("core/document::attested_object_access"));
    assert!(access.contains("tools/quality::native_object_loop"));

    let reference_chain = record_with_id(
        &feature_map,
        "feature",
        "core.attested-reference-chain-resolution",
    )
    .expect("the attested reference-chain feature record must exist");
    assert!(reference_chain.contains("profile = \"m1.attested-reference-chain.v1\""));
    assert!(reference_chain.contains("modules = [\"core/document\"]"));
    assert!(reference_chain.contains("core/document::reference_chain_resolution"));
    assert!(reference_chain.contains("core/document::reference_chain_limit_config"));
    assert!(reference_chain.contains("tools/quality::native_object_loop"));

    let resident_footprint =
        record_with_id(&feature_map, "feature", "core.attested-resident-footprint")
            .expect("the attested resident-footprint feature record must exist");
    assert!(resident_footprint.contains("profile = \"m1.attested-resident-footprint.v1\""));
    assert!(
        resident_footprint
            .contains("modules = [\"core/syntax\", \"core/object\", \"core/document\"]")
    );
    assert!(resident_footprint.contains("core/syntax::parser_behavior"));
    assert!(resident_footprint.contains("core/object::object_behavior"));
    assert!(resident_footprint.contains("core/document::attested_object_access"));
    assert!(resident_footprint.contains("core/document::reference_chain_resolution"));
    assert!(resident_footprint.contains("tools/quality::native_object_loop"));

    let ready_store = record_with_id(&feature_map, "feature", "runtime.session-ready-store")
        .expect("the session Ready-store feature record must exist");
    assert!(ready_store.contains("profile = \"m1.session-ready-store.v1\""));
    assert!(ready_store.contains("modules = [\"runtime/cache\"]"));
    assert!(ready_store.contains("runtime/cache::ready_store"));
    assert!(ready_store.contains("runtime/cache::repository_policy"));
    assert!(ready_store.contains("tools/quality::native_object_loop"));

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("the traditional-xref architecture requirement record must exist");
    assert!(requirement.contains("\"core.traditional-xref\""));
    assert!(requirement.contains("\"core/xref\""));
    assert!(requirement.contains("core/xref::traditional_xref"));
    assert!(requirement.contains("core/xref::limit_config"));
    assert!(requirement.contains("core/xref::source_error_policy"));
    assert!(requirement.contains("core/xref::repository_policy"));
    assert!(requirement.contains("\"core.strict-base-revision-attestation\""));
    assert!(requirement.contains("\"core.strict-base-open\""));
    assert!(requirement.contains("\"core.attested-object-access\""));
    assert!(requirement.contains("\"core.attested-reference-chain-resolution\""));
    assert!(requirement.contains("\"core.attested-resident-footprint\""));
    assert!(requirement.contains("core/document::revision_attestation"));
    assert!(requirement.contains("core/document::revision_attestation_limit_config"));
    assert!(requirement.contains("core/document::strict_base_open"));
    assert!(requirement.contains("core/document::attested_object_access"));
    assert!(requirement.contains("core/document::reference_chain_resolution"));
    assert!(requirement.contains("core/document::reference_chain_limit_config"));
    assert!(requirement.contains("tools/quality::native_object_loop"));
    assert!(requirement.contains("tools/quality::native_range_resume_loop"));
    assert!(requirement.contains("tools/quality::native_strict_open_runtime_loop"));
    assert!(requirement.contains("runtime.strict-base-open-job-owner"));
    assert!(requirement.contains("runtime/session::strict_base_open_owner"));
    assert!(requirement.contains("header-to-startxref"));
    assert!(requirement.contains("line-terminated comments"));
    assert!(requirement.contains("product entry that composes xref discovery, candidate construction, and attestation under one JobId and five distinct checkpoints"));
    assert!(requirement.contains("publishes only the sealed `AttestedRevisionIndex`"));
    assert!(
        requirement
            .contains("neither the xref section nor candidate index crosses the entry boundary")
    );
    assert!(requirement.contains("all five checkpoints"));
    assert!(requirement.contains("owner-mediated generation validation"));
    assert!(requirement.contains("explicit caller-lent work cap"));
    assert!(requirement.contains("never a raw target"));
    assert!(requirement.contains("top-level direct indirect-reference value"));
    assert!(requirement.contains("full closing chain"));
    assert!(requirement.contains("job-wide object, edge, depth, path-capacity, read, and parse"));
    assert!(requirement.contains("runtime inline Rust representation"));
    assert!(requirement.contains("cache-admission evidence only"));
    assert!(requirement.contains("stream payloads"));
    assert!(requirement.contains("not a complete object-graph resolver"));
    assert!(requirement.contains("nested semantic graph traversal"));
    assert!(requirement.contains("persistent Ready caching"));
    assert!(requirement.contains("cross-job/session aggregate work"));
    assert!(requirement.contains("Xref streams"));
    assert!(requirement.contains("hybrid files"));
    assert!(requirement.contains("Prev chains"));
    assert!(requirement.contains("object streams"));
    assert!(requirement.contains("repair"));
    assert!(requirement.contains("does not claim M1 exit"));
    assert!(requirement.contains("ISO clause coverage"));
    assert!(requirement.contains("R0 conformance"));
    assert!(requirement.contains("Native/PDFium semantic or pixel differential"));

    let ready_store_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/9.1")
        .expect("the session Ready-store architecture requirement record must exist");
    assert!(ready_store_requirement.contains("\"runtime.session-ready-store\""));
    assert!(ready_store_requirement.contains("\"runtime.ready-session-owner\""));
    assert!(ready_store_requirement.contains("\"runtime/cache\""));
    assert!(ready_store_requirement.contains("\"runtime/session\""));
    assert!(ready_store_requirement.contains("runtime/cache::ready_store"));
    assert!(ready_store_requirement.contains("runtime/cache::repository_policy"));
    assert!(ready_store_requirement.contains("tools/quality::native_object_loop"));
    assert!(ready_store_requirement.contains("session binding"));
    assert!(ready_store_requirement.contains("exact-key borrowed warm hit"));
    assert!(ready_store_requirement.contains("post-close resources are zero"));
    assert!(
        ready_store_requirement
            .contains("close report exactly matches the admitted resident total")
    );
    assert!(ready_store_requirement.contains("Persistent caching"));
    assert!(ready_store_requirement.contains("cross-session reuse"));
    assert!(ready_store_requirement.contains("runtime close ownership"));
    assert!(ready_store_requirement.contains("Native/PDFium semantic or pixel differential"));

    let quality_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M0")
        .expect("the M0 quality architecture requirement record must exist");
    assert!(quality_requirement.contains("tools/quality::native_object_loop"));
    assert!(quality_requirement.contains("ReadySessionOwner"));
    assert!(quality_requirement.contains("exact-key borrowed warm hit"));
    assert!(quality_requirement.contains("admitted released-resource total"));
    assert!(quality_requirement.contains("zero post-close resources"));
    assert!(quality_requirement.contains("persistent caching"));
    assert!(quality_requirement.contains("cross-session reuse"));
    assert!(quality_requirement.contains("complete Session actor"));
    assert!(quality_requirement.contains("broad corpus and pixel differential evidence"));
}

fn top_level_version(document: &str) -> Option<&str> {
    document
        .lines()
        .take_while(|line| !line.starts_with("[["))
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
}

fn record_with_id<'a>(document: &'a str, kind: &str, id: &str) -> Option<&'a str> {
    let header = format!("{kind}]]");
    let id_line = format!("id = \"{id}\"");
    document
        .split("\n[[")
        .find(|record| record.starts_with(&header) && record.lines().any(|line| line == id_line))
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory must be readable");
    for entry in entries {
        let entry = entry.expect("source directory entry must be readable");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("source file type must be readable");
        if file_type.is_dir() {
            collect_rust_sources(&path, output);
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "rs") {
            output.push(path);
        }
    }
}
