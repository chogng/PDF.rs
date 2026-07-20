use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_corpus::{
    CorpusManifestLimits, CorpusManifestObject, load_manifest_file, verify_manifest_objects,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    DocumentErrorCode, DocumentLimits, NeverCancelled, OpenStrictBaseRevisionJob, PageCountPoll,
    PageTreeJobContext, PageTreeLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionId, StrictBaseOpenContext, StrictBaseOpenError, StrictBaseOpenLimits,
    StrictBaseOpenPoll,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefErrorCode, XrefJobContext, XrefLimits};

const PDFIUM_TESTS_REVISION: &str = "a0cdeeeac46f1b2272094ee498cd59a30ce1c073";
const CORPUS_ROOT_ENV: &str = "PDF_RS_PDFIUM_CORPUS_ROOT";
const DATA_LEDGER: &str = include_str!("../../../docs/traceability/data-ledger.toml");
const PROFILE: &str = include_str!("profile.toml");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Terminal {
    Ready { page_count: u64 },
    Xref(XrefErrorCode),
    Document(DocumentErrorCode),
}

const PDFIUM_EXPECTED_IMAGES: [(&str, &str); 4] = [
    (
        "pdfium/bug_493126_endobj_bug_weirdWS_expected.pdf.0.png",
        "08adfde4ff18208ec8adb74b9af888e7f292def93b2092764bb8f28ae66bea71",
    ),
    (
        "pdfium/bug_880920_expected.pdf.0.png",
        "fe9a7d44d74e97020fd22043be28e824e0410d78af7a220d52e587e6ae48a047",
    ),
    (
        "pdfium/bug_883026_expected.pdf.0.png",
        "c9092216a1b823abb0e89b1520184fded838bd186f8f51d448058a9149946358",
    ),
    (
        "pdfium/bug_583804_expected.pdf.0.png",
        "e073dffe4b3b8debf7b07f4fa88868931d8f82018899b0ac4647479323c1c83c",
    ),
];

#[test]
fn pdfrs_strict_open_profile_is_bound_to_the_pinned_pdfium_source_suite() {
    let path = manifest_path();
    let manifest = load_manifest_file(&path, CorpusManifestLimits::default())
        .expect("PDFium T1 manifest is valid");
    assert_eq!(manifest.manifest().id(), "pdfium-t1-2026-05-14-v1");
    assert_eq!(manifest.objects().len(), 4);
    for object in manifest.objects() {
        assert_eq!(object.entry().license().expression(), "BSD-3-Clause");
        assert!(
            object
                .entry()
                .license()
                .source()
                .contains(PDFIUM_TESTS_REVISION)
        );
    }
    let manifest_hash = hex_digest(
        &sha256(&fs::read(path).expect("PDFium T1 manifest remains readable"))
            .expect("PDFium T1 manifest fits SHA-256 framing"),
    );
    let ledger = data_record("corpus.pdfium-t1-2026-05-14-v1");
    assert!(ledger.contains(&format!("source_hash = \"sha256:{manifest_hash}\"")));
    assert!(ledger.contains("license_expression = \"BSD-3-Clause\""));
    assert!(ledger.contains(PDFIUM_TESTS_REVISION));

    let profile = load_profile();
    assert_eq!(profile.id, "pdfrs-strict-open-v1");
    assert_eq!(profile.source_suite, "pdfium-t1-2026-05-14-v1");
    assert_eq!(
        profile.source_manifest_hash,
        format!("sha256:{manifest_hash}")
    );
    assert_eq!(profile.cases.len(), manifest.objects().len());
    for object in manifest.objects() {
        assert!(
            profile
                .cases
                .iter()
                .any(|case| case.path == object.relative_path()),
            "the PDF.rs profile must explicitly classify every imported source sample: {}",
            object.relative_path()
        );
    }
}

#[test]
fn pdfrs_strict_open_profile_has_stable_behavior_on_its_imported_samples() {
    let Some(root) = env::var_os(CORPUS_ROOT_ENV) else {
        eprintln!(
            "{CORPUS_ROOT_ENV} is unset; run scripts/fetch-pdfium-corpus.sh before this lane"
        );
        return;
    };
    let root = PathBuf::from(root);
    let manifest = load_manifest_file(&manifest_path(), CorpusManifestLimits::default())
        .expect("PDFium T1 manifest is valid");
    let profile = load_profile();
    let verification = verify_manifest_objects(manifest, &root, CorpusManifestLimits::default())
        .expect("downloaded PDFium objects match the pinned manifest");
    verify_pdfium_expected_images(&root);

    for object in verification.manifest().objects() {
        let bytes = fs::read(root.join(object.relative_path()))
            .expect("verified PDFium corpus object remains readable");
        let first = strict_open_terminal(object, &bytes);
        let second = strict_open_terminal(object, &bytes);
        assert_eq!(
            first,
            second,
            "{} must produce the same terminal result across fresh Rust runs",
            object.relative_path()
        );
        assert_eq!(
            first,
            expected_rust_terminal(
                &profile,
                object.relative_path(),
                object.entry().page_count()
            ),
            "the PDF.rs strict-open product decision must be explicit: {}",
            object.relative_path()
        );
    }
}

fn strict_open_terminal(object: &CorpusManifestObject, bytes: &[u8]) -> Terminal {
    let source_len =
        u64::try_from(bytes.len()).expect("manifest-bound corpus byte length fits u64");
    let snapshot = SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new(object.entry().id().sha256()),
            SourceRevision::new(1),
        ),
        Some(source_len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            object.entry().id().sha256(),
        ),
    );
    let store = RangeStore::new(snapshot, Default::default()).expect("bounded source store opens");
    let full_range = ByteRange::new(0, source_len).expect("manifest-bound range is valid");
    store
        .supply(
            RangeResponse::new(snapshot, full_range, bytes.to_vec())
                .expect("corpus bytes match the complete response range"),
        )
        .expect("manifest-bound corpus object fits the source store");

    let context = StrictBaseOpenContext::new(
        XrefJobContext::new(
            JobId::new(0xc011),
            ResumeCheckpoint::new(0xc012),
            ResumeCheckpoint::new(0xc013),
        ),
        RevisionAttestationJobContext::new(
            JobId::new(0xc011),
            ResumeCheckpoint::new(0xc014),
            ResumeCheckpoint::new(0xc015),
            ResumeCheckpoint::new(0xc016),
            RequestPriority::Metadata,
        ),
    );
    let mut job = OpenStrictBaseRevisionJob::new(
        snapshot,
        RevisionId::new(0xc011),
        context,
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .expect("fixed strict-open context is valid");

    match job.poll(&store, &NeverCancelled) {
        StrictBaseOpenPoll::Ready(authority) => {
            let mut page_count = authority
                .count_pages(
                    PageTreeJobContext::new(
                        JobId::new(0xc021),
                        ResumeCheckpoint::new(0xc022),
                        ResumeCheckpoint::new(0xc023),
                        RequestPriority::Metadata,
                    ),
                    PageTreeLimits::default(),
                )
                .expect("fixed page-count context is valid");
            match page_count.poll(&store, &NeverCancelled) {
                PageCountPoll::Ready(count) => Terminal::Ready {
                    page_count: count.page_count(),
                },
                PageCountPoll::Failed(error) => Terminal::Document(error.code()),
                PageCountPoll::Pending { .. } => {
                    panic!("a complete corpus response must not leave page counting pending")
                }
            }
        }
        StrictBaseOpenPoll::Failed(StrictBaseOpenError::Xref(error)) => {
            Terminal::Xref(error.code())
        }
        StrictBaseOpenPoll::Failed(StrictBaseOpenError::Document(error)) => {
            Terminal::Document(error.code())
        }
        StrictBaseOpenPoll::Pending { .. } => {
            panic!("a complete corpus response must not leave strict open pending")
        }
    }
}

fn verify_pdfium_expected_images(root: &Path) {
    for (relative_path, expected_hash) in PDFIUM_EXPECTED_IMAGES {
        let bytes = fs::read(root.join(relative_path))
            .expect("pinned PDFium expected image remains available");
        assert_eq!(
            hex_digest(&sha256(&bytes).expect("expected image fits SHA-256 framing")),
            expected_hash,
            "PDFium expected image must match the pinned upstream revision: {relative_path}"
        );
    }
}

fn expected_rust_terminal(
    profile: &Profile,
    relative_path: &str,
    pdfium_page_count: u32,
) -> Terminal {
    let expected = profile
        .cases
        .iter()
        .find(|case| case.path == relative_path)
        .unwrap_or_else(|| panic!("unmapped PDF.rs strict-open case: {relative_path}"));
    match expected.terminal.as_str() {
        "ready.page-count" => {
            assert!(expected.supported, "a ready profile case must be supported");
            assert_eq!(
                expected.page_count,
                Some(pdfium_page_count),
                "a supported PDF.rs case must retain PDFium's declared page count"
            );
            Terminal::Ready {
                page_count: u64::from(pdfium_page_count),
            }
        }
        "xref.invalid-entry" => {
            assert!(
                !expected.supported,
                "a refusal profile case must be unsupported"
            );
            Terminal::Xref(XrefErrorCode::InvalidEntry)
        }
        "xref.unsupported-xref-stream" => {
            assert!(
                !expected.supported,
                "a refusal profile case must be unsupported"
            );
            Terminal::Xref(XrefErrorCode::UnsupportedXrefStream)
        }
        "document.unsupported-object-framing" => {
            assert!(
                !expected.supported,
                "a refusal profile case must be unsupported"
            );
            Terminal::Document(DocumentErrorCode::UnsupportedObjectFraming)
        }
        terminal => panic!("unknown PDF.rs strict-open terminal: {terminal}"),
    }
}

#[derive(Debug)]
struct Profile {
    id: String,
    source_suite: String,
    source_manifest_hash: String,
    cases: Vec<ProfileCase>,
}

#[derive(Debug)]
struct ProfileCase {
    path: String,
    terminal: String,
    supported: bool,
    page_count: Option<u32>,
}

fn load_profile() -> Profile {
    parse_profile(PROFILE)
}

fn parse_profile(profile: &str) -> Profile {
    let mut id = None;
    let mut source_suite = None;
    let mut source_manifest_hash = None;
    let mut cases = Vec::new();
    let mut current: Option<ProfileCaseBuilder> = None;

    for (line_number, raw_line) in profile.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[case]]" {
            if let Some(case) = current.take() {
                cases.push(case.finish(line_number + 1));
            }
            current = Some(ProfileCaseBuilder::default());
            continue;
        }
        let (key, raw_value) = line
            .split_once(" = ")
            .unwrap_or_else(|| panic!("invalid profile line {}", line_number + 1));
        if let Some(case) = current.as_mut() {
            case.set(key, raw_value, line_number + 1);
        } else {
            match key {
                "id" => set_once(
                    &mut id,
                    quoted(raw_value, line_number + 1),
                    key,
                    line_number + 1,
                ),
                "source_suite" => set_once(
                    &mut source_suite,
                    quoted(raw_value, line_number + 1),
                    key,
                    line_number + 1,
                ),
                "source_manifest_hash" => set_once(
                    &mut source_manifest_hash,
                    quoted(raw_value, line_number + 1),
                    key,
                    line_number + 1,
                ),
                "schema" => assert_eq!(raw_value, "1", "unsupported profile schema"),
                _ => panic!("unknown profile field {key} at line {}", line_number + 1),
            }
        }
    }
    if let Some(case) = current {
        cases.push(case.finish(profile.lines().count()));
    }
    assert!(!cases.is_empty(), "PDF.rs strict-open profile has no cases");
    let unique_paths = cases
        .iter()
        .map(|case| case.path.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unique_paths.len(),
        cases.len(),
        "duplicate PDF.rs strict-open profile path"
    );
    Profile {
        id: id.expect("profile id is required"),
        source_suite: source_suite.expect("profile source_suite is required"),
        source_manifest_hash: source_manifest_hash
            .expect("profile source_manifest_hash is required"),
        cases,
    }
}

#[derive(Default)]
struct ProfileCaseBuilder {
    path: Option<String>,
    terminal: Option<String>,
    supported: Option<bool>,
    page_count: Option<u32>,
}

impl ProfileCaseBuilder {
    fn set(&mut self, key: &str, raw_value: &str, line_number: usize) {
        match key {
            "path" => set_once(
                &mut self.path,
                quoted(raw_value, line_number),
                key,
                line_number,
            ),
            "terminal" => set_once(
                &mut self.terminal,
                quoted(raw_value, line_number),
                key,
                line_number,
            ),
            "supported" => {
                let value = match raw_value {
                    "true" => true,
                    "false" => false,
                    _ => panic!("invalid supported value at line {line_number}"),
                };
                assert!(
                    self.supported.replace(value).is_none(),
                    "duplicate {key} at line {line_number}"
                );
            }
            "page_count" => {
                let value = raw_value
                    .parse::<u32>()
                    .unwrap_or_else(|_| panic!("invalid page_count at line {line_number}"));
                assert!(
                    self.page_count.replace(value).is_none(),
                    "duplicate {key} at line {line_number}"
                );
            }
            _ => panic!("unknown case field {key} at line {line_number}"),
        }
    }

    fn finish(self, line_number: usize) -> ProfileCase {
        let path = self.path.expect("profile case path is required");
        assert!(
            path.starts_with("pdfium/") && path.ends_with(".pdf"),
            "unsafe profile path at line {line_number}"
        );
        let supported = self.supported.expect("profile case supported is required");
        let page_count = self.page_count;
        assert_eq!(
            supported,
            page_count.is_some(),
            "only supported profile cases declare page_count"
        );
        ProfileCase {
            path,
            terminal: self.terminal.expect("profile case terminal is required"),
            supported,
            page_count,
        }
    }
}

fn quoted(raw_value: &str, line_number: usize) -> String {
    raw_value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("expected non-empty quoted value at line {line_number}"))
        .to_owned()
}

fn set_once(slot: &mut Option<String>, value: String, key: &str, line_number: usize) {
    assert!(
        slot.replace(value).is_none(),
        "duplicate {key} at line {line_number}"
    );
}

fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/pdfium/t1/manifest.toml")
}

fn data_record(id: &str) -> &str {
    let identity = format!("id = \"{id}\"");
    DATA_LEDGER
        .split("[[data]]")
        .skip(1)
        .find(|record| record.lines().any(|line| line == identity))
        .unwrap_or_else(|| panic!("missing data-ledger record: {id}"))
}

#[test]
#[should_panic(expected = "only supported profile cases declare page_count")]
fn pdfrs_profile_rejects_a_refusal_with_a_success_page_count() {
    parse_profile(
        "schema = 1\n\
         id = \"test\"\n\
         source_suite = \"test-suite\"\n\
         source_manifest_hash = \"sha256:test\"\n\
         \n\
         [[case]]\n\
         path = \"pdfium/refusal.pdf\"\n\
         terminal = \"xref.invalid-entry\"\n\
         supported = false\n\
         page_count = 1\n",
    );
}

#[test]
#[should_panic(expected = "unsafe profile path")]
fn pdfrs_profile_rejects_a_path_outside_the_imported_source_suite() {
    parse_profile(
        "schema = 1\n\
         id = \"test\"\n\
         source_suite = \"test-suite\"\n\
         source_manifest_hash = \"sha256:test\"\n\
         \n\
         [[case]]\n\
         path = \"../outside.pdf\"\n\
         terminal = \"xref.invalid-entry\"\n\
         supported = false\n",
    );
}

#[test]
#[should_panic(expected = "duplicate PDF.rs strict-open profile path")]
fn pdfrs_profile_rejects_duplicate_source_samples() {
    parse_profile(
        "schema = 1\n\
         id = \"test\"\n\
         source_suite = \"test-suite\"\n\
         source_manifest_hash = \"sha256:test\"\n\
         \n\
         [[case]]\n\
         path = \"pdfium/duplicate.pdf\"\n\
         terminal = \"xref.invalid-entry\"\n\
         supported = false\n\
         \n\
         [[case]]\n\
         path = \"pdfium/duplicate.pdf\"\n\
         terminal = \"xref.invalid-entry\"\n\
         supported = false\n",
    );
}
