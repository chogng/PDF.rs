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
        "pdf-rs/xref may depend only on the lower-level pdf-rs/bytes and pdf-rs/syntax crates"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "pdf-rs/xref must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "pdf-rs/xref source selection is non-empty"
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
fn decoded_xref_entry_remains_an_untrusted_semantic_boundary() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        fs::read_to_string(crate_root.join("Cargo.toml")).expect("xref manifest must be readable");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("xref library surface must be readable");
    let stream = fs::read_to_string(crate_root.join("src/stream.rs"))
        .expect("xref-stream parser source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("xref provenance must be readable");

    assert!(library.contains("parse_decoded_xref_stream"));
    assert!(library.contains("the decoded entry is an untrusted semantic parser"));
    assert!(stream.contains("fn parse_xref_stream_payload("));
    assert!(stream.contains("validate_unfiltered_source_geometry"));
    assert!(stream.contains("PayloadCoordinates::Decoded"));
    assert!(stream.contains("does not interpret either value or prove"));
    assert!(stream.contains("Identity payloads must use"));
    assert!(stream.contains("index.is_multiple_of(CANCELLATION_INTERVAL)"));
    assert!(!manifest.contains("pdf-rs-filters"));
    assert!(!library.contains("DecodedStreamAttestation"));
    assert!(!stream.contains("pub struct DecodedXrefAttestation"));
    assert!(provenance.contains("untrusted semantic parser"));
    assert!(provenance.contains("depend on `pdf-rs/filters`"));
    assert!(provenance.contains("sealed `DecodedStream` attestation"));
    assert!(provenance.contains("remains component-level plumbing"));
    assert!(provenance.contains("subsequent batch of at most 256"));
    assert!(provenance.contains("mismatch already observed in a batch wins"));
}

#[test]
fn anchored_revision_surface_cannot_relax_the_strict_base_entry() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("xref library surface must be readable");
    let strict_job = fs::read_to_string(crate_root.join("src/job.rs"))
        .expect("strict xref job source must be readable");
    let parser = fs::read_to_string(crate_root.join("src/parser.rs"))
        .expect("xref parser source must be readable");
    let anchored = fs::read_to_string(crate_root.join("src/traditional_revision.rs"))
        .expect("anchored revision source must be readable");
    let revision = fs::read_to_string(crate_root.join("src/revision.rs"))
        .expect("revision composer source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("xref provenance must be readable");

    assert!(library.contains("mod traditional_revision;"));
    assert!(library.contains("OpenTraditionalRevisionJob"));
    assert!(library.contains("TraditionalRevisionSection"));
    assert!(anchored.contains("parse_traditional_revision_section"));
    assert!(anchored.contains("upper_bound"));
    assert!(anchored.contains("TraditionalRevisionPoll::Pending"));
    assert!(parser.contains("finalize_base_section"));
    assert!(parser.contains("finalize_revision_section"));
    assert!(parser.contains("UnsupportedIncrementalRevision"));
    assert!(parser.contains("UnsupportedHybridXref"));
    assert!(revision.contains("root: Option<ObjectRef>"));
    assert!(revision.contains("xref_stream: Option<u64>"));
    assert!(revision.contains("impl From<TraditionalRevisionSection> for RevisionCandidate"));
    assert!(revision.contains("if base.root.is_none()"));
    assert!(revision.contains(".find_map(|revision| revision.root)"));
    assert!(revision.contains("revision.xref_stream != Some(supplement.startxref)"));
    assert!(revision.contains(".is_some_and(|previous| supplement.startxref <= previous)"));
    assert!(strict_job.contains("parse_section"));
    assert!(strict_job.contains("XrefPoll::Ready"));
    assert!(
        !strict_job.contains("parse_traditional_revision_section"),
        "OpenXrefJob must not adopt the sparse revision parser"
    );
    assert!(
        !anchored.contains("impl From<TraditionalRevisionSection> for XrefSection"),
        "a sparse candidate must not convert into the strict base proof"
    );
    assert!(provenance.contains("cannot be converted to `XrefSection`"));
    assert!(provenance.contains("but it does not"));
    assert!(provenance.contains("discover the final anchor"));
    assert!(provenance.contains("does not recharge"));
    assert!(provenance.contains("the oldest base must provide one"));
    assert!(provenance.contains("first explicit value found from newest to oldest"));
    assert!(provenance.contains("A base hybrid permits `XRefStm < current startxref`"));
    assert!(provenance.contains("only already-parsed candidates"));
}

#[test]
fn final_and_anchor_jobs_remain_classification_only_primitives() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("xref library surface must be readable");
    let strict_job = fs::read_to_string(crate_root.join("src/job.rs"))
        .expect("strict xref job source must be readable");
    let final_anchor = fs::read_to_string(crate_root.join("src/final_anchor.rs"))
        .expect("final-anchor source must be readable");
    let anchor = fs::read_to_string(crate_root.join("src/anchor.rs"))
        .expect("anchor-classifier source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("xref provenance must be readable");

    for required in [
        "mod final_anchor;",
        "mod anchor;",
        "OpenFinalStartXrefJob",
        "FinalStartXref",
        "OpenXrefAnchorJob",
        "XrefAnchorKind",
    ] {
        assert!(
            library.contains(required),
            "xref surface must retain {required:?}"
        );
    }
    assert!(strict_job.contains("OpenFinalStartXrefJob"));
    assert!(strict_job.contains("FinalStartXrefPoll"));
    assert!(!strict_job.contains("parse_tail"));
    assert!(final_anchor.contains("parse_tail"));
    assert!(!final_anchor.contains("parse_section"));
    assert!(anchor.contains("XrefAnchorKind::Traditional"));
    assert!(anchor.contains("XrefAnchorKind::StreamObject"));
    assert!(anchor.contains("XrefLimitKind::AnchorBytes"));
    assert!(anchor.contains("self.range.end_exclusive() == source_len"));
    assert!(anchor.contains("self.range.end_exclusive() == self.upper_bound"));
    assert!(!anchor.contains("parse_unfiltered_xref_stream"));
    assert!(!anchor.contains("pdf_rs_object"));
    assert!(provenance.contains("there is no second tail implementation"));
    assert!(provenance.contains("only header evidence"));
    assert!(provenance.contains("snapshot's actual source length"));
    assert!(provenance.contains("earlier caller physical bound is invalid anchor"));
    assert!(provenance.contains("These tests do not"));
    assert!(provenance.contains("frame or decode a stream"));
    assert!(provenance.contains("jobs do not acquire a"));
    assert!(provenance.contains("revision section"));
}

#[test]
fn traceability_maps_are_versioned_together_and_register_xref() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("pdf-rs/xref has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable during repository tests");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec traceability map must be readable during repository tests");

    assert_eq!(top_level_version(&feature_map), Some("0.78.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.78.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps must advance as one semantic traceability version"
    );

    let feature = record_with_id(&feature_map, "feature", "core.traditional-xref")
        .expect("the traditional-xref feature record must exist");
    assert!(feature.contains("profile = \"m1.traditional-xref.v1\""));
    assert!(feature.contains("modules = [\"pdf-rs/xref\"]"));
    assert!(feature.contains("pdf-rs/xref::traditional_xref"));
    assert!(feature.contains("pdf-rs/xref::traditional_revision"));
    assert!(feature.contains("pdf-rs/xref::limit_config"));
    assert!(feature.contains("pdf-rs/xref::source_error_policy"));
    assert!(feature.contains("pdf-rs/xref::repository_policy"));

    let stream_feature = record_with_id(&feature_map, "feature", "core.decoded-xref-stream-table")
        .expect("the decoded xref-stream table feature record must exist");
    assert!(stream_feature.contains("state = \"PLANNED\""));
    assert!(stream_feature.contains("profile = \"m1.decoded-xref-stream-table.v1\""));
    assert!(stream_feature.contains("modules = [\"pdf-rs/xref\"]"));
    assert!(stream_feature.contains("pdf-rs/xref::xref_stream"));
    assert!(stream_feature.contains("fuzz_targets = []"));
    assert!(stream_feature.contains("benchmarks = []"));

    let chain_feature = record_with_id(&feature_map, "feature", "core.xref-revision-chain")
        .expect("the xref revision-chain feature record must exist");
    assert!(chain_feature.contains("state = \"PLANNED\""));
    assert!(chain_feature.contains("profile = \"m1.xref-revision-chain.v1\""));
    assert!(chain_feature.contains("modules = [\"pdf-rs/xref\"]"));
    assert!(chain_feature.contains("pdf-rs/xref::revision_chain"));
    assert!(chain_feature.contains("pdf-rs/xref::traditional_revision"));
    assert!(chain_feature.contains("fuzz_targets = []"));
    assert!(chain_feature.contains("benchmarks = []"));

    let anchor_feature = record_with_id(&feature_map, "feature", "core.source-xref-anchor")
        .expect("the source xref-anchor feature record must exist");
    assert!(anchor_feature.contains("state = \"PLANNED\""));
    assert!(anchor_feature.contains("profile = \"m1.source-xref-anchor.v1\""));
    assert!(anchor_feature.contains("modules = [\"pdf-rs/xref\", \"pdf-rs/object\"]"));
    assert!(anchor_feature.contains("pdf-rs/xref::final_startxref"));
    assert!(anchor_feature.contains("pdf-rs/xref::xref_anchor"));
    assert!(anchor_feature.contains("pdf-rs/object::object_behavior"));
    assert!(anchor_feature.contains("fuzz_targets = []"));
    assert!(anchor_feature.contains("benchmarks = []"));

    let attestation = record_with_id(
        &feature_map,
        "feature",
        "core.strict-base-revision-attestation",
    )
    .expect("the strict base-revision attestation feature record must exist");
    assert!(attestation.contains("profile = \"m1.strict-base-revision-attestation.v1\""));
    assert!(attestation.contains("modules = [\"pdf-rs/document\"]"));
    assert!(attestation.contains("pdf-rs/document::revision_attestation"));
    assert!(attestation.contains("pdf-rs/document::revision_attestation_limit_config"));
    assert!(attestation.contains("tools/quality::native_object_loop"));

    let strict_open = record_with_id(&feature_map, "feature", "core.strict-base-open")
        .expect("the strict base-open feature record must exist");
    assert!(strict_open.contains("state = \"PLANNED\""));
    assert!(strict_open.contains("profile = \"m1.strict-base-open.v1\""));
    assert!(strict_open.contains("modules = [\"pdf-rs/document\"]"));
    assert!(strict_open.contains("pdf-rs/document::strict_base_open"));
    assert!(strict_open.contains("pdf-rs/document::repository_policy"));
    assert!(strict_open.contains("tools/quality::native_object_loop"));
    assert!(strict_open.contains("tools/quality::native_range_resume_loop"));
    assert!(strict_open.contains("tools/quality::native_strict_open_runtime_loop"));

    let access = record_with_id(&feature_map, "feature", "core.attested-object-access")
        .expect("the proof-preserving object-access feature record must exist");
    assert!(access.contains("profile = \"m1.attested-object-access.v1\""));
    assert!(access.contains("modules = [\"pdf-rs/document\"]"));
    assert!(access.contains("pdf-rs/document::attested_object_access"));
    assert!(access.contains("tools/quality::native_object_loop"));

    let reference_chain = record_with_id(
        &feature_map,
        "feature",
        "core.attested-reference-chain-resolution",
    )
    .expect("the attested reference-chain feature record must exist");
    assert!(reference_chain.contains("profile = \"m1.attested-reference-chain.v1\""));
    assert!(reference_chain.contains("modules = [\"pdf-rs/document\"]"));
    assert!(reference_chain.contains("pdf-rs/document::reference_chain_resolution"));
    assert!(reference_chain.contains("pdf-rs/document::reference_chain_limit_config"));
    assert!(reference_chain.contains("tools/quality::native_object_loop"));

    let resident_footprint =
        record_with_id(&feature_map, "feature", "core.attested-resident-footprint")
            .expect("the attested resident-footprint feature record must exist");
    assert!(resident_footprint.contains("profile = \"m1.attested-resident-footprint.v1\""));
    assert!(
        resident_footprint
            .contains("modules = [\"pdf-rs/syntax\", \"pdf-rs/object\", \"pdf-rs/document\"]")
    );
    assert!(resident_footprint.contains("pdf-rs/syntax::parser_behavior"));
    assert!(resident_footprint.contains("pdf-rs/object::object_behavior"));
    assert!(resident_footprint.contains("pdf-rs/document::attested_object_access"));
    assert!(resident_footprint.contains("pdf-rs/document::reference_chain_resolution"));
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
    assert!(requirement.contains("\"core.source-xref-anchor\""));
    assert!(requirement.contains("\"core.decoded-xref-stream-table\""));
    assert!(requirement.contains("\"core.xref-revision-chain\""));
    assert!(requirement.contains("\"pdf-rs/xref\""));
    assert!(requirement.contains("explicit base roots, update inheritance"));
    assert!(requirement.contains("validated base hybrids"));
    assert!(requirement.contains("pdf-rs/xref::traditional_xref"));
    assert!(requirement.contains("pdf-rs/xref::final_startxref"));
    assert!(requirement.contains("pdf-rs/xref::xref_anchor"));
    assert!(requirement.contains("pdf-rs/xref::traditional_revision"));
    assert!(requirement.contains("pdf-rs/xref::xref_stream"));
    assert!(requirement.contains("pdf-rs/xref::revision_chain"));
    assert!(requirement.contains("pdf-rs/xref::limit_config"));
    assert!(requirement.contains("pdf-rs/xref::source_error_policy"));
    assert!(requirement.contains("pdf-rs/xref::repository_policy"));
    assert!(requirement.contains("\"core.strict-base-revision-attestation\""));
    assert!(requirement.contains("\"core.strict-base-open\""));
    assert!(requirement.contains("\"core.attested-object-access\""));
    assert!(requirement.contains("\"core.attested-reference-chain-resolution\""));
    assert!(requirement.contains("\"core.attested-resident-footprint\""));
    assert!(requirement.contains("pdf-rs/document::revision_attestation"));
    assert!(requirement.contains("pdf-rs/document::revision_attestation_limit_config"));
    assert!(requirement.contains("pdf-rs/document::strict_base_open"));
    assert!(requirement.contains("pdf-rs/document::attested_object_access"));
    assert!(requirement.contains("pdf-rs/document::reference_chain_resolution"));
    assert!(requirement.contains("pdf-rs/document::reference_chain_limit_config"));
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
    assert!(
        requirement
            .contains("Resume execution and source-failure disposition require exact arbiter")
    );
    assert!(requirement.contains("caller-lent work cap"));
    assert!(requirement.contains("one-shot reopen jobs under retained profiles"));
    assert!(requirement.contains("follows only top-level whole-object aliases"));
    assert!(requirement.contains("exact cycle chains and aggregate limits"));
    assert!(requirement.contains("value-owned footprint evidence"));
    assert!(requirement.contains("for later cache admission"));
    assert!(requirement.contains("payload containment"));
    assert!(requirement.contains("general graph traversal"));
    assert!(requirement.contains("persistent reuse or coalescing"));
    assert!(requirement.contains("aggregate reads and parses remain capped"));
    assert!(
        requirement.contains("strict unfiltered entry and an untrusted decoded semantic entry")
    );
    assert!(
        requirement
            .contains("physical encoded length to differ from caller-supplied decoded length")
    );
    assert!(requirement.contains("relative decoded spans rather than physical source ByteSpan"));
    assert!(requirement.contains("does not interpret the filter plan or mint decode proof"));
    assert!(requirement.contains("sealed DecodedStream attestation"));
    assert!(requirement.contains("Proof-bound filtered object-stream decoding"));
    assert!(requirement.contains("current traditional primary, current hybrid supplement"));
    assert!(
        requirement
            .contains("primary free and xref-stream unknown-type null rows hide older definitions")
    );
    assert!(requirement.contains("hybrid geometry, unique anchors"));
    assert!(requirement.contains("already-composed chain"));
    assert!(requirement.contains("OpenSourceRevisionChainJob"));
    assert!(requirement.contains("optional hybrid anchor classification"));
    assert!(requirement.contains("anchored traditional or source-framed stream acquisition"));
    assert!(requirement.contains("every raw source proof"));
    assert!(requirement.contains("m1.source-xref-stream-acquisition.v1"));
    assert!(requirement.contains("single active Pending ticket"));
    assert!(requirement.contains("Indirect Length remains unsupported"));
    assert!(
        requirement
            .contains("filtered payloads require the explicit foundational-filter constructor")
    );
    assert!(requirement.contains("strict base parser still rejects sparse incremental tables"));
    assert!(requirement.contains("m1.source-xref-anchor.v1"));
    assert!(requirement.contains("line-terminated traditional table header"));
    assert!(requirement.contains("classification result only identifies an ObjectRef"));
    assert!(!requirement.contains("final-anchor classification for stream primaries"));
    assert!(requirement.contains("source-bound final and revision anchors"));
    assert!(
        requirement.contains(
            "line-terminated traditional table header or an exact indirect-object header"
        )
    );
    assert!(requirement.contains("`/Prev` traversal"));
    assert!(
        requirement.contains("schedules exact filtered or unfiltered object-stream payload reads")
    );
    assert!(requirement.contains("repair"));
    assert!(requirement.contains("contribute to the covered M1 byte-and-object gate"));
    assert!(requirement.contains("ISO conformance"));
    assert!(requirement.contains("a release profile"));
    assert!(requirement.contains("Native/PDFium pixel maturity"));

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
