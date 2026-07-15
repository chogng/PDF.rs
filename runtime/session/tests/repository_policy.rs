use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> PathBuf {
    crate_root()
        .parent()
        .and_then(Path::parent)
        .expect("runtime/session has a repository root two levels above it")
        .to_path_buf()
}

fn rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("source directory must be readable") {
        let path = entry.expect("source entry must be readable").path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
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

fn assert_opaque_move_only_permit(source: &str, name: &str, expected_fields: &[&str]) {
    let struct_marker = format!("pub struct {name} {{");
    let implementation_marker = format!("impl {name}");
    let struct_start = source
        .find(&struct_marker)
        .unwrap_or_else(|| panic!("{name} declaration must remain present"));
    let declaration_start = source[..struct_start]
        .rfind("#[derive(")
        .unwrap_or_else(|| panic!("{name} declaration must retain an explicit derive policy"));
    let declaration_end = source[struct_start..]
        .find(&implementation_marker)
        .map(|offset| struct_start + offset)
        .unwrap_or_else(|| panic!("{name} implementation must follow its declaration"));
    let declaration = &source[declaration_start..declaration_end];
    let fields = declaration
        .split(&struct_marker)
        .nth(1)
        .and_then(|body| body.split('}').next())
        .unwrap_or_else(|| panic!("{name} fields must remain inspectable by repository policy"));

    assert!(declaration.contains("#[derive(Debug, Eq, PartialEq)]"));
    assert!(!declaration.contains("Clone"));
    assert!(!declaration.contains("Copy"));
    for private_field in expected_fields {
        assert!(fields.lines().any(|line| line.trim() == *private_field));
    }
    assert!(!fields.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("pub ") || line.starts_with("pub(")
    }));
}

#[test]
fn product_source_remains_exclusive_runtime_owner_code_without_io_or_async() {
    let mut sources = Vec::new();
    rust_sources(&crate_root().join("src"), &mut sources);
    sources.sort();
    assert!(!sources.is_empty());

    let joined = sources
        .iter()
        .map(|path| fs::read_to_string(path).expect("product source must be UTF-8"))
        .collect::<Vec<_>>()
        .join("\n");
    let lowercase = joined.to_ascii_lowercase();
    for forbidden in [
        "std::fs", "std::net", "async fn", "tokio", "reqwest", "unsafe {", "pdfium", "mupdf",
        "pdf.js",
    ] {
        assert!(
            !lowercase.contains(forbidden),
            "session product source contains forbidden token {forbidden:?}"
        );
    }
    for forbidden_escape in ["store(", "store_mut(", "into_store("] {
        assert!(
            !joined.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix("pub fn ")
                    .or_else(|| line.strip_prefix("pub const fn "))
                    .is_some_and(|signature| signature.starts_with(forbidden_escape))
            }),
            "runtime owner must not expose {forbidden_escape:?}"
        );
    }
    let strict_open_owner = fs::read_to_string(crate_root().join("src/strict_base_open_owner.rs"))
        .expect("strict-open owner source must be UTF-8");
    for forbidden_escape in [
        "job(",
        "job_mut(",
        "into_job(",
        "arbiter(",
        "arbiter_mut(",
        "into_arbiter(",
    ] {
        assert!(
            !strict_open_owner.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix("pub fn ")
                    .or_else(|| line.strip_prefix("pub const fn "))
                    .is_some_and(|signature| signature.starts_with(forbidden_escape))
            }),
            "StrictBaseOpenJobOwner must not expose {forbidden_escape:?}"
        );
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
}

#[test]
fn range_resume_permits_remain_opaque_and_move_only() {
    let source = fs::read_to_string(crate_root().join("src/range_resume.rs"))
        .expect("Range-resume source must be readable");
    assert_opaque_move_only_permit(
        &source,
        "RangeResumePermit",
        &[
            "arbiter_id: RangeResumeArbiterId,",
            "ticket: DataTicket,",
            "target: RangeResumeTarget,",
        ],
    );
    assert_opaque_move_only_permit(
        &source,
        "RangeResumeFailurePermit",
        &[
            "arbiter_id: RangeResumeArbiterId,",
            "ticket: DataTicket,",
            "target: RangeResumeTarget,",
            "error: SourceError,",
        ],
    );
}

#[test]
fn strict_open_coordinator_keeps_execution_owners_private_and_ready_move_only() {
    let source = fs::read_to_string(crate_root().join("src/strict_base_open_coordinator.rs"))
        .expect("strict-open coordinator source must be readable");
    let public_signatures = source
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("pub fn ") || line.starts_with("pub const fn "))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "ByteSource",
        "RangeResumeArbiter",
        "RangeResumePermit",
        "RangeResumeFailurePermit",
        "StrictBaseOpenJobOwner",
        "register_pending",
        "take_completion",
    ] {
        assert!(
            !public_signatures.contains(forbidden),
            "coordinator public API must not expose {forbidden:?}"
        );
    }
    for callback in [
        "pub fn supply(&mut self, response: RangeResponse)",
        "pub fn observe_snapshot(&mut self, observed: SourceSnapshot)",
        "pub fn fail_data(&mut self, ticket: DataTicket)",
    ] {
        assert!(source.contains(callback));
    }
    assert_eq!(
        source.matches("pub fn run_one(").count(),
        1,
        "exactly one public coordinator method may enter parser execution"
    );

    let ready_start = source
        .find("pub struct StrictBaseOpenReady {")
        .expect("Ready handoff declaration must remain present");
    let ready_end = source[ready_start..]
        .find("impl StrictBaseOpenReady")
        .map(|offset| ready_start + offset)
        .expect("Ready handoff implementation must follow its declaration");
    let declaration = &source[ready_start..ready_end];
    assert!(!declaration.contains("Clone"));
    assert!(!declaration.contains("Copy"));
    let fields = declaration
        .split("pub struct StrictBaseOpenReady {")
        .nth(1)
        .and_then(|body| body.split('}').next())
        .expect("Ready handoff fields must remain inspectable");
    for private_field in [
        "index: AttestedRevisionIndex,",
        "source_owner: RangeResumeArbiter,",
    ] {
        assert!(fields.lines().any(|line| line.trim() == private_field));
    }
    assert!(!fields.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("pub ") || line.starts_with("pub(")
    }));
}

#[test]
fn product_dependencies_are_only_bytes_cache_and_direct_signature_document_types() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml"))
        .expect("session manifest must be readable");
    let dependency_block = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|text| text.split("[dev-dependencies]").next())
        .expect("manifest must contain product dependencies");
    let names: BTreeSet<_> = dependency_block
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["pdf-rs-bytes", "pdf-rs-cache", "pdf-rs-document"])
    );
    assert!(!manifest.contains("tools/"));
    assert!(!manifest.contains("[build-dependencies]"));
    assert!(!manifest.contains("[target."));
}

#[test]
fn traceability_registers_the_owner_and_bounded_lifecycle_claim() {
    let root = repository_root();
    let feature_map = fs::read_to_string(root.join("docs/traceability/feature-map.toml"))
        .expect("feature map must be readable");
    let spec_map = fs::read_to_string(root.join("docs/traceability/spec-map.toml"))
        .expect("spec map must be readable");
    assert_eq!(top_level_version(&feature_map), Some("0.37.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.37.0"));

    let feature = record_with_id(&feature_map, "feature", "runtime.ready-session-owner")
        .expect("Ready-session owner feature must exist");
    for required in [
        "profile = \"m1.ready-session-owner.v1\"",
        "RPE-ARCH-001/9.1",
        "RPE-ARCH-001/14.2",
        "RPE-STD-002/5",
        "RPE-STD-002/10",
        "modules = [\"runtime/session\"]",
        "runtime/session::ready_owner",
        "runtime/session::repository_policy",
        "tools/quality::native_object_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    let actor = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/9.1")
        .expect("Document actor requirement must exist");
    for required in [
        "runtime.ready-session-owner",
        "runtime/session",
        "runtime/session::ready_owner",
        "synchronously drops values plus fixed metadata",
        "post-close resource snapshot is zero",
        "not RSS evidence",
        "session ID allocation and Worker-epoch non-reuse",
        "Native/PDFium semantic or pixel differential",
    ] {
        assert!(
            actor.contains(required),
            "actor mapping must contain {required:?}"
        );
    }

    let lifecycle = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/14.2")
        .expect("handle lifecycle requirement must exist");
    for required in [
        "runtime.ready-session-owner",
        "runtime/session::ready_owner",
        "same close report",
        "drops the complete Ready store before returning",
        "Created/Opening/Waiting/Failed orchestration",
        "before publishing",
        "SessionClosed",
        "partial",
    ] {
        assert!(
            lifecycle.contains(required),
            "lifecycle mapping must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_range_resume_and_strict_open_execution_as_partial() {
    let root = repository_root();
    let feature_map = fs::read_to_string(root.join("docs/traceability/feature-map.toml"))
        .expect("feature map must be readable");
    let spec_map = fs::read_to_string(root.join("docs/traceability/spec-map.toml"))
        .expect("spec map must be readable");

    let feature = record_with_id(&feature_map, "feature", "runtime.range-resume-arbiter")
        .expect("Range-resume arbiter feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.range-resume-arbiter.v1\"",
        "RPE-ARCH-001/5.1-5.2",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"runtime/session\"]",
        "runtime/session::range_resume",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "Range-resume feature must contain {required:?}"
        );
    }

    let owner = record_with_id(
        &feature_map,
        "feature",
        "runtime.strict-base-open-job-owner",
    )
    .expect("strict-base open job-owner feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.strict-base-open-job-owner.v1\"",
        "RPE-ARCH-001/5.1-5.2",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"runtime/session\"]",
        "runtime/session::strict_base_open_owner",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            owner.contains(required),
            "strict-base open owner feature must contain {required:?}"
        );
    }

    let coordinator = record_with_id(
        &feature_map,
        "feature",
        "runtime.strict-base-open-coordinator",
    )
    .expect("strict-base open coordinator feature must exist");
    for required in [
        "owner = \"runtime-platform\"",
        "state = \"PLANNED\"",
        "profile = \"m1.strict-base-open-coordinator.v1\"",
        "RPE-ARCH-001/5.1-5.2",
        "RPE-ARCH-001/5.4",
        "RPE-ARCH-001/14.2",
        "RPE-ARCH-001/15.3/M1",
        "RPE-STD-002/5-7",
        "RPE-STD-005/5",
        "RPE-STD-005/8",
        "modules = [\"runtime/session\"]",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            coordinator.contains(required),
            "strict-base open coordinator feature must contain {required:?}"
        );
    }

    let quality = record_with_id(
        &feature_map,
        "feature",
        "quality.native-strict-open-runtime-loop",
    )
    .expect("Native strict-open runtime-loop feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.native-strict-open-runtime-loop.v1\"",
        "RPE-ARCH-001/12.6",
        "RPE-ARCH-001/15.3/M1",
        "modules = [\"tools/quality\"]",
        "tests = [\"tools/quality::native_strict_open_runtime_loop\"]",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            quality.contains(required),
            "Native strict-open runtime-loop feature must contain {required:?}"
        );
    }

    let byte_access = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.1-5.2")
        .expect("Native byte-access requirement must exist");
    for required in [
        "runtime.range-resume-arbiter",
        "runtime.strict-base-open-job-owner",
        "runtime.strict-base-open-coordinator",
        "quality.native-strict-open-runtime-loop",
        "runtime/session::range_resume",
        "runtime/session::strict_base_open_owner",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "status = \"partial\"",
        "runtime caller registers each returned Pending ticket with its job, checkpoint, and generation",
        "unified ordered completion stream",
        "arbiter-bound move-only resume permit",
        "exact ticket-local source-failure permit",
        "Host supply",
        "snapshot observation",
        "ticket failure",
        "never invoke parser code inline",
        "consumes only identity-matching resume or failure permits",
        "stale or mismatched permits are consumed without parser work",
        "public run_one method is the only parser entry",
        "every host ingress as queue-only work",
        "without polling the parser or probing cancellation",
        "opaque move-only handoff",
        "same private source owner",
        "exact five checkpoints",
        "upper-half-before-lower out-of-order delivery",
        "generic multi-job scheduler with priority, fairness, backpressure, and generation registry",
        "complete Session/request/Worker ownership",
        "all features stay PLANNED",
        "does not claim M1 exit",
    ] {
        assert!(
            byte_access.contains(required),
            "byte-access mapping must contain {required:?}"
        );
    }

    let xref = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.4")
        .expect("strict base-revision architecture requirement must exist");
    for required in [
        "runtime.strict-base-open-coordinator",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_strict_open_runtime_loop",
        "makes public run_one the only parser entry",
        "queued resume or failure completion",
        "Host ingress never polls",
        "failure completion without parser or cancellation polling",
        "opaque move-only handoff",
        "same private source owner",
        "generic multi-job scheduler and complete Session lifecycle",
        "does not claim M1 exit",
    ] {
        assert!(
            xref.contains(required),
            "strict base-revision mapping must contain {required:?}"
        );
    }

    let lifecycle = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/14.2")
        .expect("handle lifecycle requirement must exist");
    for required in [
        "runtime.range-resume-arbiter",
        "runtime.strict-base-open-job-owner",
        "runtime.strict-base-open-coordinator",
        "runtime/session::range_resume",
        "runtime/session::strict_base_open_owner",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "status = \"partial\"",
        "exact job/checkpoint/generation registrations",
        "arbiter-bound move-only resume or failure permits",
        "Data arrival only queues a permit; it does not run parser code",
        "validates every resume or failure permit's issuer, ticket, job, checkpoint, and generation",
        "Late or mismatched permits are consumed without parser work",
        "Public run_one is its only parser entry",
        "Host supply, snapshot observation, and failure ingress only queue work",
        "a failure turn does not poll the parser or probe cancellation",
        "opaque move-only handoff",
        "same private source owner",
        "not one complete Session",
        "generic job queue and scheduler",
    ] {
        assert!(
            lifecycle.contains(required),
            "lifecycle mapping must contain {required:?}"
        );
    }

    let milestone = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/15.3/M1")
        .expect("M1 byte-and-object milestone requirement must exist");
    for required in [
        "runtime.range-resume-arbiter",
        "runtime.strict-base-open-job-owner",
        "runtime.strict-base-open-coordinator",
        "quality.native-range-resume-loop",
        "quality.native-strict-open-runtime-loop",
        "runtime/session::range_resume",
        "runtime/session::strict_base_open_owner",
        "runtime/session::strict_base_open_coordinator",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "tools/quality::native_strict_open_runtime_loop",
        "status = \"partial\"",
        "one-job strict-open coordinator",
        "Coordinator public run_one is the only parser entry",
        "host ingress only mutates Range state and may queue completion",
        "never polls parser code",
        "later exclusive actor turn",
        "consumes one exact failure completion",
        "without a parser poll or cancellation probe",
        "opaque move-only Ready handoff",
        "same private source owner",
        "coordinator then reports zero resources",
        "consuming close returns exact owner-release evidence",
        "all five parser checkpoints",
        "upper-half-before-lower out-of-order supply",
        "generic multi-job scheduler with queue priority, fairness, backpressure, and a job registry",
        "not a complete Session",
        "viewport generations",
        "R0/R1 repair implementations are also absent",
        "not registered DIFFERENTIAL evidence",
        "does not claim M1 exit",
    ] {
        assert!(
            milestone.contains(required),
            "M1 mapping must contain {required:?}"
        );
    }
    for required in [
        "The sibling direct lower-owner path",
        "arbiter-bound move-only dispatch",
        "exact issuer/ticket/job/checkpoint/generation validation",
        "stale-generation rejection without parser work",
    ] {
        assert!(
            milestone.contains(required),
            "M1 direct-owner evidence must contain {required:?}"
        );
    }
}

#[test]
fn provenance_bounds_each_runtime_owner_without_a_complete_session_claim() {
    let provenance = fs::read_to_string(crate_root().join("PROVENANCE.md"))
        .expect("session provenance must be readable");
    for required in [
        "unique store owner",
        "idempotent close report",
        "arbiter-bound move-only",
        "StrictBaseOpenJobOwner",
        "matching its issuing arbiter",
        "completed ticket, job, checkpoint",
        "A stale or mismatched permit is discarded",
        "without polling parser code or changing the saved parser phase and cumulative stats",
        "Public `run_one` is the only parser entry",
        "at most one",
        "queue-only host ingress",
        "StrictBaseOpenReady",
        "same private Range source",
        "one parser job",
        "`ReadySessionOwner` remains separate",
        "future generic",
        "scheduler and registry",
        "generic job",
        "queue, registry, priority, fairness, backpressure",
        "cross-job",
        "arbitration",
        "does not claim the complete protocol-visible Session state machine",
        "session ID allocation",
        "does not publish `SessionClosed`",
        "not allocator telemetry, process RSS",
        "No PDFium",
        "registered broad Native/PDFium differential",
    ] {
        assert!(
            provenance.contains(required),
            "provenance must contain {required:?}"
        );
    }
}
