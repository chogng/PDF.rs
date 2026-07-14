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
    for forbidden_escape in ["pub fn store(", "pub fn store_mut(", "pub fn into_store("] {
        assert!(
            !joined.contains(forbidden_escape),
            "ReadySessionOwner must not expose {forbidden_escape:?}"
        );
    }
    assert!(joined.contains("#![forbid(unsafe_code)]"));
    assert!(joined.contains("#![deny(missing_docs)]"));
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
    assert_eq!(top_level_version(&feature_map), Some("0.29.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.29.0"));

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
        "before publishing `SessionClosed`",
        "partial",
    ] {
        assert!(
            lifecycle.contains(required),
            "lifecycle mapping must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_range_resume_as_planned_scheduler_handoff() {
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
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "Range-resume feature must contain {required:?}"
        );
    }

    let byte_access = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.1-5.2")
        .expect("Native byte-access requirement must exist");
    for required in [
        "runtime.range-resume-arbiter",
        "runtime/session::range_resume",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "status = \"partial\"",
        "runtime caller registers each returned Pending ticket with its job, checkpoint, and generation",
        "converts terminal tickets into one-shot scheduler records only after the store call returns",
        "releasing exact subscriptions on cancellation without disturbing shared waiters",
        "scheduler must still compare the generation carried by a taken record before executing it",
        "tail, xref-section, prefix-scan, object-envelope, and stream-boundary checkpoints",
        "upper-half-before-lower out-of-order delivery",
        "exact one-shot requeue",
        "all features stay PLANNED",
        "does not claim M1 exit",
    ] {
        assert!(
            byte_access.contains(required),
            "byte-access mapping must contain {required:?}"
        );
    }

    let lifecycle = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/14.2")
        .expect("handle lifecycle requirement must exist");
    for required in [
        "runtime.range-resume-arbiter",
        "runtime/session::range_resume",
        "runtime/session::repository_policy",
        "tools/quality::native_range_resume_loop",
        "status = \"partial\"",
        "exact job/checkpoint/generation registrations",
        "completed scheduler records",
        "Data arrival only queues a record; it does not run parser code",
        "external scheduler remains responsible for rejecting a taken record whose captured generation is stale",
        "two isolated synchronous owners, not one complete Session",
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
        "quality.native-range-resume-loop",
        "runtime/session::range_resume",
        "tools/quality::native_range_resume_loop",
        "status = \"partial\"",
        "all five parser checkpoints",
        "upper-half-before-lower out-of-order supply",
        "exact one-shot dispatch",
        "an executing scheduler that validates current generations",
        "R0/R1 repair profiles are also absent",
        "not registered DIFFERENTIAL evidence",
        "does not claim M1 exit",
    ] {
        assert!(
            milestone.contains(required),
            "M1 mapping must contain {required:?}"
        );
    }
}

#[test]
fn provenance_bounds_close_to_the_ready_store_only() {
    let provenance = fs::read_to_string(crate_root().join("PROVENANCE.md"))
        .expect("session provenance must be readable");
    for required in [
        "unique store owner",
        "idempotent close report",
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
