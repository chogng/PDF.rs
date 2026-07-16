use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn content_crate_has_only_the_approved_layered_dependencies() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read manifest");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("content manifest declares an explicit dependency table")
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
            r#"pdf-rs-document = { path = "../document" }"#,
            r#"pdf-rs-scene = { path = "../scene" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ]
    );
    let dev_dependency_body = manifest
        .split_once("[dev-dependencies]")
        .expect("content manifest declares an explicit development dependency table")
        .1
        .split("\n[")
        .next()
        .expect("development dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dev_dependency_body,
        [
            r#"pdf-rs-object = { path = "../object" }"#,
            r#"pdf-rs-xref = { path = "../xref" }"#,
        ]
    );
    for forbidden_table in ["[build-dependencies]", "[target."] {
        assert!(
            !manifest.contains(forbidden_table),
            "core/content must not declare {forbidden_table} dependencies"
        );
    }
    for forbidden in [
        "pdfium",
        "mupdf",
        "poppler",
        "reqwest",
        "tokio",
        "async-std",
        "hyper",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "forbidden dependency marker: {forbidden}"
        );
    }

    let document_manifest =
        fs::read_to_string(root.join("../document/Cargo.toml")).expect("read document manifest");
    let scene_manifest =
        fs::read_to_string(root.join("../scene/Cargo.toml")).expect("read Scene manifest");
    assert!(!document_manifest.contains("pdf-rs-content"));
    assert!(!scene_manifest.contains("pdf-rs-content"));
}

#[test]
fn product_sources_exclude_unsafe_and_platform_io() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut joined = String::new();
    for entry in fs::read_dir(root).expect("read src") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read source");
        joined.push_str(&source);
        joined.push('\n');
        for forbidden in [
            "unsafe {",
            "unsafe fn",
            "unsafe impl",
            "unsafe extern",
            "std::fs",
            "std::net",
            "std::process",
            "File::open",
            "TcpStream",
            "Command::new",
            "pdfium",
            "mupdf",
            "poppler",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains forbidden marker {forbidden}",
                path.display()
            );
        }
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn m2_scanner_and_sealed_vm_remain_planned_after_the_bounded_gate_closes() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/content has a repository root two levels above it");
    let library = fs::read_to_string(crate_root.join("src/lib.rs"))
        .expect("content library source is readable");
    let model =
        fs::read_to_string(crate_root.join("src/model.rs")).expect("content model is readable");
    let scanner =
        fs::read_to_string(crate_root.join("src/scanner.rs")).expect("scanner is readable");
    let vm = fs::read_to_string(crate_root.join("src/vm.rs")).expect("VM is readable");
    let vm_model =
        fs::read_to_string(crate_root.join("src/vm_model.rs")).expect("VM model is readable");
    let vm_error =
        fs::read_to_string(crate_root.join("src/vm_error.rs")).expect("VM errors are readable");
    let vm_limits =
        fs::read_to_string(crate_root.join("src/vm_limits.rs")).expect("VM limits are readable");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml"))
            .expect("feature map is readable");
    let spec_map = fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml"))
        .expect("spec map is readable");
    let plan =
        fs::read_to_string(repository_root.join("plan/m2.toml")).expect("M2 plan is readable");

    assert_eq!(top_level_version(&feature_map), Some("0.70.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.70.0"));
    assert_eq!(
        top_level_version(&feature_map),
        top_level_version(&spec_map),
        "feature and specification maps advance together"
    );
    for required in [
        "pub struct DecodedContentStream",
        "pub struct ContentProgram",
        "pub struct ContentScanStats",
        "pub enum OperatorKind",
        "pub struct OperatorSpec",
        "pub enum ContentOperator",
        "Unknown(Vec<u8>)",
        "pub struct ContentOperatorSource",
    ] {
        assert!(
            library.contains(required) || model.contains(required),
            "content scanner boundary must contain {required:?}"
        );
    }
    for required in [
        "pub struct ContentScanJob",
        "pub enum ContentScanPoll",
        "pub fn scan_content_streams(",
        "ContentErrorCode::InvalidStreamOrder",
        "ContentErrorCode::DanglingOperands",
        "ContentLimitKind::TotalDecodedBytes",
        "ContentLimitKind::Tokens",
        "ContentLimitKind::TokenBytes",
        "ContentLimitKind::OperandsPerOperator",
        "ContentLimitKind::NestingDepth",
        "ContentLimitKind::Operators",
        "ContentLimitKind::Fuel",
        "ContentLimitKind::RetainedBytes",
    ] {
        assert!(
            scanner.contains(required),
            "content scanner implementation must contain {required:?}"
        );
    }

    for required in [
        "pub enum ContentVmPoll",
        "pub struct InterpretPageJob",
        "acquired: Option<AcquiredPageContent>",
        "pub fn new(\n        acquired: AcquiredPageContent",
        "pub fn poll(",
        "run_scan(",
        ".property_resolver(property_limits)",
        "OperatorKind::SaveGraphicsState",
        "OperatorKind::RestoreGraphicsState",
        "OperatorKind::ConcatMatrix",
        "current_ctm.checked_multiply(operand)",
        "OperatorKind::BeginText",
        "OperatorKind::EndText",
        "OperatorKind::BeginCompatibility",
        "OperatorKind::EndCompatibility",
        "OperatorKind::MarkedContentPoint",
        "OperatorKind::MarkedContentPointProperties",
        "OperatorKind::BeginMarkedContent",
        "OperatorKind::BeginMarkedContentProperties",
        "OperatorKind::EndMarkedContent",
        "ContentUnsupported::from_document",
        "ContentVmFailure::Document(error)",
        "SceneBuilder::new",
    ] {
        assert!(
            vm.contains(required),
            "sealed Content VM must contain {required:?}"
        );
    }
    for required in [
        "pub struct InterpretedPage",
        "acquired: AcquiredPageContent",
        "scene: Scene",
        "property_uses: Vec<ResolvedPropertyUse>",
        "final_ctm: Matrix",
    ] {
        assert!(
            vm_model.contains(required),
            "interpreted Page model must contain {required:?}"
        );
    }
    for required in [
        "pub enum ContentUnsupportedKind",
        "UnknownOperator",
        "MarkedContentPoint",
        "MarkedContentPointProperties",
        "DirectContentPropertyDictionary",
        "IndirectPageProperties",
        "DirectPagePropertyDictionary",
        "pub enum ContentVmFailure",
        "Content(crate::ContentError)",
        "Document(DocumentError)",
        "Scene(pdf_rs_scene::SceneError)",
        "Vm(ContentVmError)",
    ] {
        assert!(
            vm_error.contains(required),
            "VM outcome policy must contain {required:?}"
        );
    }
    for required in [
        "max_operators",
        "max_fuel",
        "max_graphics_state_depth",
        "max_compatibility_depth",
        "max_marked_content_depth",
        "max_property_uses",
        "max_retained_bytes",
        "ContentVmLimitKind::Allocation",
    ] {
        assert!(
            vm_limits.contains(required),
            "VM limit policy must contain {required:?}"
        );
    }
    for required in [
        "pub use vm::{ContentVmPoll, InterpretPageJob};",
        "ContentUnsupported",
        "ContentVmFailure",
        "InterpretedPage",
        "ResolvedPropertyUse",
    ] {
        assert!(
            library.contains(required),
            "content public boundary must export {required:?}"
        );
    }

    let scanner_feature = record_with_id(&feature_map, "feature", "core.content-operator-scanner")
        .expect("operator scanner feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.content-operator-scanner.v1\"",
        "ISO-32000-1:2008/7.8.2",
        "RPE-ARCH-001/4.3-4.5",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/content\"]",
        "core/content::scanner",
        "core/content::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            scanner_feature.contains(required),
            "operator-scanner feature must contain {required:?}"
        );
    }

    let property_feature = record_with_id(&feature_map, "feature", "core.page-property-lookup")
        .expect("page-property lookup feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.page-property-lookup.v1\"",
        "ISO-32000-1:2008/7.8.3",
        "ISO-32000-1:2008/14.6.2",
        "RPE-ARCH-001/5.8-5.9",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/document\"]",
        "core/document::page_properties",
        "core/document::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            property_feature.contains(required),
            "page-property feature must contain {required:?}"
        );
    }

    let vm_feature = record_with_id(&feature_map, "feature", "core.content-vm-scene-v1")
        .expect("Content VM feature is registered");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m2.content-vm-scene-v1\"",
        "ISO-32000-1:2008/7.8.2",
        "ISO-32000-1:2008/7.8.3",
        "ISO-32000-1:2008/8.4.2",
        "ISO-32000-1:2008/8.4.3",
        "ISO-32000-1:2008/9.4",
        "ISO-32000-1:2008/14.6",
        "ISO-32000-1:2008/14.6.1",
        "ISO-32000-1:2008/14.6.2",
        "RPE-ARCH-001/6.1-6.2",
        "RPE-ARCH-001/6.4-6.7",
        "RPE-ARCH-001/15.3/M2",
        "modules = [\"core/content\"]",
        "core/content::vm",
        "core/content::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            vm_feature.contains(required),
            "Content VM feature must contain {required:?}"
        );
    }

    let content_stream = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.8.2")
        .expect("content-stream requirement is registered");
    for required in [
        "core.page-content-acquisition",
        "core.content-operator-scanner",
        "core.content-vm-scene-v1",
        "core/document::page_content",
        "core/content::scanner",
        "core/content::vm",
        "strict attested proof",
        "stream boundaries as semantic whitespace",
        "proof-bearing AcquiredPageContent",
        "validates known operand shapes before state or unsupported policy",
        "scanner, document, and Scene failures retain their original structured diagnostic types",
        "Inline images, Forms, paths, painting, text showing",
    ] {
        assert!(
            content_stream.contains(required),
            "content-stream mapping must contain {required:?}"
        );
    }

    let page_resources = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/7.8.3")
        .expect("page resource requirement is registered");
    for required in [
        "core.page-property-lookup",
        "core.content-vm-scene-v1",
        "core/document::page_properties",
        "no-I/O",
        "without polling for bytes",
        "fixed-size PagePropertyReference evidence",
        "never opens or attests the selected target object",
        "retain the original lower DocumentError",
    ] {
        assert!(
            page_resources.contains(required),
            "page-resource mapping must contain {required:?}"
        );
    }

    let matrix = record_with_id(&spec_map, "requirement", "ISO-32000-1:2008/8.4.3")
        .expect("matrix-concatenation requirement is registered");
    for required in [
        "core.content-vm-scene-v1",
        "current × operand in the Scene column-matrix representation",
        "equivalent to PDF operand-prepend row semantics",
        "current_ctm.checked_multiply(operand)",
        "translation components 16 and 28",
    ] {
        assert!(
            matrix.contains(required),
            "matrix mapping must contain {required:?}"
        );
    }

    let interpreter = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.1-6.2")
        .expect("content interpreter requirement is registered");
    for required in [
        "core.content-operator-scanner",
        "core.page-property-lookup",
        "core.content-vm-scene-v1",
        "strict-attested AcquiredPageContent",
        "q/Q/cm, BT/ET, BX/EX, BMC, name-based BDC, and EMC",
        "independent operator, fuel, graphics-depth, compatibility-depth, marked-depth, property-use, allocation, and retained-state limits",
        "Unknown operators are ignored only inside compatibility sections",
        "MP, DP, direct BDC property dictionaries",
        "retain their original lower DocumentError",
        "M2-06 is complete",
        "status = \"partial\"",
    ] {
        assert!(
            interpreter.contains(required),
            "content-interpreter mapping must contain {required:?}"
        );
    }

    let scene_requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/6.4-6.7")
        .expect("Scene requirement is registered");
    assert!(scene_requirement.contains("core.content-vm-scene-v1"));
    assert!(scene_requirement.contains("M2-06 supplies one bounded producer"));
    assert!(scene_requirement.contains("quality.m2-scene-gate"));
    assert!(scene_requirement.contains("M2-07 now closes the bounded M2 exit gate"));
    assert!(scene_requirement.contains("All component and quality feature records remain PLANNED"));

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M2")
        .expect("M2 requirement is registered");
    for required in [
        "core.content-operator-scanner",
        "core.page-property-lookup",
        "core.content-vm-scene-v1",
        "quality.m2-scene-gate",
        "core/content::scanner",
        "core/content::vm",
        "tools/quality::m2_scene_gate",
        "tools/quality::m2_exit",
        "M2-05 is complete as two bounded PLANNED profiles",
        "M2-06 is complete as two additional bounded PLANNED profiles",
        "no-I/O page-property resolver",
        "sealed Content VM consumes only strict-attested AcquiredPageContent",
        "M2-07 completes the bounded exit",
        "All nine M2 feature records remain PLANNED",
        "status = \"covered\"",
    ] {
        assert!(
            milestone.contains(required),
            "M2 mapping must contain {required:?}"
        );
    }

    let m2_05 = record_with_id(&plan, "work_item", "M2-05").expect("M2-05 work item exists");
    assert!(m2_05.contains("status = \"complete\""));
    assert!(m2_05.contains("completed_at = 2026-07-16"));
    let m2_06 = record_with_id(&plan, "work_item", "M2-06").expect("M2-06 work item exists");
    assert!(m2_06.contains("status = \"complete\""));
    assert!(m2_06.contains("completed_at = 2026-07-16"));
    let m2_07 = record_with_id(&plan, "work_item", "M2-07").expect("M2-07 work item exists");
    assert!(m2_07.contains("status = \"complete\""));
    assert!(m2_07.contains("completed_at = 2026-07-16"));
    let milestone_header = plan
        .split("[[work_item]]")
        .next()
        .expect("M2 plan has a top-level milestone header");
    assert!(milestone_header.contains("status = \"complete\""));
    assert!(milestone_header.contains("completed_at = 2026-07-16"));
}
