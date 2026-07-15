use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_object_core_only_depends_on_bytes_and_syntax_and_has_no_platform_io() {
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
        "core/object may depend only on the lower-level core/bytes and core/syntax crates"
    );
    assert!(
        !manifest.contains("pdf-rs-xref"),
        "core/object must remain a sibling of core/xref rather than depending on it"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/object must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(
        !sources.is_empty(),
        "core/object source selection is non-empty"
    );

    let forbidden = [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "ureq",
        "pdfium",
        "mupdf",
        "poppler",
        "ghostscript",
        "pdf.js",
        "pdf_rs_xref",
        "unsafe fn",
        "unsafe impl",
        "unsafe {",
        "#[allow(unsafe_code)]",
        "#![allow(unsafe_code)]",
        "extern \"c\"",
    ];
    let mut forbids_unsafe = false;
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        forbids_unsafe |= lowercase.contains("#![forbid(unsafe_code)]");
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product object-core token {token:?} in {}",
                path.display()
            );
        }
    }
    assert!(
        forbids_unsafe,
        "core/object must forbid unsafe code at its crate boundary"
    );
}

#[test]
fn xref_stream_anchor_geometry_cannot_relax_ordinary_entry_targets() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("object library surface must be readable");
    let model = fs::read_to_string(crate_root.join("src/model.rs"))
        .expect("object target model must be readable");
    let repair = fs::read_to_string(crate_root.join("src/repair.rs"))
        .expect("object local-repair source must be readable");
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md"))
        .expect("object provenance must be readable");

    assert!(library.contains("IndirectObjectTargetKind"));
    assert!(model.contains("IndirectObjectTargetKind::XrefEntry"));
    assert!(model.contains("IndirectObjectTargetKind::XrefStreamAnchor"));
    assert!(model.contains("pub fn at_xref_stream_anchor("));
    assert!(model.contains("object_upper_bound > revision_startxref"));
    assert!(model.contains("revision_startxref < startxref"));
    assert!(model.contains("object_upper_bound != revision_startxref"));
    assert!(model.contains("target_kind: target.kind"));
    assert!(repair.contains("target.kind() != IndirectObjectTargetKind::XrefEntry"));
    assert!(repair.contains("ObjectErrorCode::UnsupportedRepairTarget"));
    assert!(provenance.contains("does not relax or share the ordinary constructor's inequality"));
    assert!(provenance.contains("must equal the exclusive `object_upper_bound`"));
    assert!(provenance.contains("`XrefStreamAnchor` at construction"));
    assert!(provenance.contains("silently\n  downgrading its geometry authority"));
    assert!(provenance.contains("not itself prove `/Type /XRef`"));
    assert!(provenance.contains("target kind survives into a completed object"));
}

#[test]
fn traceability_registers_staged_stream_length_without_claiming_a_resolver() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/object has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature traceability map must be readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("specification traceability map must be readable");
    assert_eq!(top_level_version(&feature_map), Some("0.54.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.54.0"));

    let anchor_feature = record_with_id(&feature_map, "feature", "core.source-xref-anchor")
        .expect("source xref-anchor feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.source-xref-anchor.v1\"",
        "modules = [\"core/xref\", \"core/object\"]",
        "core/xref::final_startxref",
        "core/xref::xref_anchor",
        "core/object::object_behavior",
        "core/object::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            anchor_feature.contains(required),
            "source xref-anchor feature must contain {required:?}"
        );
    }

    let feature = record_with_id(&feature_map, "feature", "core.staged-stream-length-framing")
        .expect("staged stream-length feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.staged-stream-length.v1\"",
        "modules = [\"core/object\"]",
        "core/object::staged_length",
        "core/object::object_behavior",
        "core/object::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "staged stream-length feature must contain {required:?}"
        );
    }

    let syntax_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.3")
        .expect("syntax architecture requirement must exist");
    let object_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("object architecture requirement must exist");
    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("M1 architecture requirement must exist");
    for requirement in [syntax_requirement, object_requirement, milestone] {
        assert!(requirement.contains("core.staged-stream-length-framing"));
        assert!(requirement.contains("core/object::staged_length"));
        assert!(requirement.contains("same-snapshot"));
        assert!(requirement.contains("resolver"));
        assert!(requirement.contains("M1 exit"));
    }
    assert!(syntax_requirement.contains("does not itself resolve or attest"));
    assert!(object_requirement.contains("m1.revision-aware-uncompressed-resolver.v1"));
    assert!(object_requirement.contains("effective uncompressed direct nonnegative integer"));
    assert!(object_requirement.contains("core.object-stream-resolution"));
    assert!(object_requirement.contains("m1.unfiltered-object-stream-resolution.v1"));
    assert!(object_requirement.contains("DecodedObjectSpan"));
    assert!(object_requirement.contains("latest effective uncompressed container"));
    assert!(object_requirement.contains("object-stream scheduling and ownership"));
    assert!(milestone.contains("connects those already-composed inputs"));
    assert!(milestone.contains(
        "proof-preserving unfiltered direct-Length source revision-chain discovery and traversal"
    ));
    assert!(milestone.contains("does not decode filtered or indirect-Length xref streams"));
    assert!(milestone.contains("profiles remain PLANNED"));

    let repair = record_with_id(&feature_map, "feature", "core.local-repair")
        .expect("local-repair feature record must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.r1-local-repair.v1\"",
        "core/object::local_repair",
        "core/object::object_behavior",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            repair.contains(required),
            "local repair must contain {required:?}"
        );
    }
    assert!(milestone.contains("object R1 sibling"));
    assert!(milestone.contains("repair-only scan and candidate caps"));
    assert!(milestone.contains("replays planned direct-length semantics"));
    assert!(milestone.contains("LocallyRepairedRevisionIndex"));
    assert!(milestone.contains("single core repaired-open coordinator"));
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
