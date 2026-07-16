use std::env;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, JobId, RangeResponse, RangeStore, RangeStoreLimitConfig,
    RangeStoreLimits, ReadPoll, ReadRequest, RequestPriority, ResumeCheckpoint, SourceIdentity,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_content::{
    ContentLimitConfig, ContentLimits, ContentVmLimitConfig, ContentVmLimits, ContentVmPoll,
    InterpretPageJob,
};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_document::{
    AcquiredPageContent, DocumentCancellation, DocumentLimitConfig, DocumentLimits, NeverCancelled,
    OpenStrictBaseRevisionJob, PageContentJobContext, PageContentLimitConfig, PageContentLimits,
    PageContentPoll, PageIndexBuildPoll, PageIndexLimits, PageLookupPoll,
    PageMaterializationJobContext, PageMaterializationLimitConfig, PageMaterializationLimits,
    PageMaterializationPoll, PagePropertyLookupLimitConfig, PagePropertyLookupLimits,
    PageTreeJobContext, PageTreeLimitConfig, PageTreeLimits, RevisionAttestationJobContext,
    RevisionAttestationLimitConfig, RevisionAttestationLimits, RevisionId, StrictBaseOpenContext,
    StrictBaseOpenLimits, StrictBaseOpenPoll,
};
use pdf_rs_filters::{DecodeLimitConfig, DecodeLimits};
use pdf_rs_object::{ObjectLimitConfig, ObjectLimits};
use pdf_rs_quality::manifest::{CaseManifest, validate_manifest};
use pdf_rs_scene::{
    CommandSource, PageGeometry, PageRotation, Scene, SceneBinding, SceneBuilder, SceneDiffLimits,
    SceneLimitConfig, SceneLimits, SceneRect, SceneScalar, compare_scenes,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimitConfig, SyntaxLimits};
use pdf_rs_xref::{XrefJobContext, XrefLimitConfig, XrefLimits};

const REQUIRED_RUNNER: &str = "tools/quality::m2_scene_gate";
const PAGE_OBJECT: u32 = 3;
const CONTENT_OBJECT: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    ValidStateAndMarkedContent,
    InvalidUnbalancedGraphicsState,
    UnsupportedMarkedPoint,
    ResourceMarkedContentProperties,
    CancelBeforePublication,
    SourceChangeBeforeResume,
}

const CASES: [Case; 6] = [
    Case::ValidStateAndMarkedContent,
    Case::InvalidUnbalancedGraphicsState,
    Case::UnsupportedMarkedPoint,
    Case::ResourceMarkedContentProperties,
    Case::CancelBeforePublication,
    Case::SourceChangeBeforeResume,
];

impl Case {
    const fn id(self) -> &'static str {
        match self {
            Self::ValidStateAndMarkedContent => "content/m2-scene/valid-state-and-marked-content",
            Self::InvalidUnbalancedGraphicsState => {
                "content/m2-scene/invalid-unbalanced-graphics-state"
            }
            Self::UnsupportedMarkedPoint => "content/m2-scene/unsupported-marked-point",
            Self::ResourceMarkedContentProperties => {
                "content/m2-scene/resource-marked-content-properties"
            }
            Self::CancelBeforePublication => "content/m2-scene/cancel-before-publication",
            Self::SourceChangeBeforeResume => "content/m2-scene/source-change-before-resume",
        }
    }

    const fn seed(self) -> u8 {
        match self {
            Self::ValidStateAndMarkedContent => 0xa1,
            Self::InvalidUnbalancedGraphicsState => 0xa2,
            Self::UnsupportedMarkedPoint => 0xa3,
            Self::ResourceMarkedContentProperties => 0xa4,
            Self::CancelBeforePublication => 0xa5,
            Self::SourceChangeBeforeResume => 0xa6,
        }
    }

    const fn startxref(self) -> u64 {
        match self {
            Self::ValidStateAndMarkedContent => 317,
            Self::InvalidUnbalancedGraphicsState => 285,
            Self::UnsupportedMarkedPoint => 276,
            Self::ResourceMarkedContentProperties => 347,
            Self::CancelBeforePublication | Self::SourceChangeBeforeResume => 283,
        }
    }

    const fn input(self) -> &'static [u8] {
        match self {
            Self::ValidStateAndMarkedContent => include_bytes!(
                "../../../../tests/cases/content/m2-scene/valid-state-and-marked-content/input.pdf"
            ),
            Self::InvalidUnbalancedGraphicsState => include_bytes!(
                "../../../../tests/cases/content/m2-scene/invalid-unbalanced-graphics-state/input.pdf"
            ),
            Self::UnsupportedMarkedPoint => include_bytes!(
                "../../../../tests/cases/content/m2-scene/unsupported-marked-point/input.pdf"
            ),
            Self::ResourceMarkedContentProperties => include_bytes!(
                "../../../../tests/cases/content/m2-scene/resource-marked-content-properties/input.pdf"
            ),
            Self::CancelBeforePublication => include_bytes!(
                "../../../../tests/cases/content/m2-scene/cancel-before-publication/input.pdf"
            ),
            Self::SourceChangeBeforeResume => include_bytes!(
                "../../../../tests/cases/content/m2-scene/source-change-before-resume/input.pdf"
            ),
        }
    }

    const fn manifest(self) -> &'static str {
        match self {
            Self::ValidStateAndMarkedContent => include_str!(
                "../../../../tests/cases/content/m2-scene/valid-state-and-marked-content/case.toml"
            ),
            Self::InvalidUnbalancedGraphicsState => include_str!(
                "../../../../tests/cases/content/m2-scene/invalid-unbalanced-graphics-state/case.toml"
            ),
            Self::UnsupportedMarkedPoint => include_str!(
                "../../../../tests/cases/content/m2-scene/unsupported-marked-point/case.toml"
            ),
            Self::ResourceMarkedContentProperties => include_str!(
                "../../../../tests/cases/content/m2-scene/resource-marked-content-properties/case.toml"
            ),
            Self::CancelBeforePublication => include_str!(
                "../../../../tests/cases/content/m2-scene/cancel-before-publication/case.toml"
            ),
            Self::SourceChangeBeforeResume => include_str!(
                "../../../../tests/cases/content/m2-scene/source-change-before-resume/case.toml"
            ),
        }
    }

    const fn golden_scene(self) -> Option<&'static [u8]> {
        match self {
            Self::ValidStateAndMarkedContent => Some(include_bytes!(
                "../../../../tests/cases/content/m2-scene/valid-state-and-marked-content/expected/scene.json"
            )),
            Self::ResourceMarkedContentProperties => Some(include_bytes!(
                "../../../../tests/cases/content/m2-scene/resource-marked-content-properties/expected/scene.json"
            )),
            Self::InvalidUnbalancedGraphicsState
            | Self::UnsupportedMarkedPoint
            | Self::CancelBeforePublication
            | Self::SourceChangeBeforeResume => None,
        }
    }

    const fn expected(self) -> ExpectedOutcome {
        match self {
            Self::ValidStateAndMarkedContent | Self::ResourceMarkedContentProperties => {
                ExpectedOutcome::Ready
            }
            Self::InvalidUnbalancedGraphicsState => ExpectedOutcome::Failed("RPE-CONTENT-VM-0007"),
            Self::UnsupportedMarkedPoint => {
                ExpectedOutcome::Unsupported("RPE-CONTENT-UNSUPPORTED-0002")
            }
            Self::CancelBeforePublication => ExpectedOutcome::Failed("RPE-CONTENT-VM-0011"),
            Self::SourceChangeBeforeResume => ExpectedOutcome::Failed("RPE-CONTENT-VM-0014"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedOutcome {
    Ready,
    Unsupported(&'static str),
    Failed(&'static str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CaseContract {
    input_sha256: String,
    scene_sha256: Option<String>,
    max_input_bytes: u64,
    max_objects: u64,
    max_resolve_depth: u64,
    max_stream_output_bytes: u64,
    max_total_decode_bytes: u64,
    max_scene_commands: u64,
    max_group_depth: u64,
    operator_fuel: u64,
    decode_fuel: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutcomeKind {
    Ready,
    Unsupported,
    Failed,
}

impl OutcomeKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NormalizedOutcome {
    kind: OutcomeKind,
    diagnostic_id: Option<String>,
    diagnostic: Option<String>,
    scene_sha256: Option<String>,
    scene: Option<Vec<u8>>,
    diff: Option<Vec<u8>>,
}

struct AcquiredFixture {
    acquired: AcquiredPageContent,
    store: RangeStore,
    snapshot: SourceSnapshot,
}

pub fn run_gate() {
    let mut outcomes = Vec::with_capacity(CASES.len());
    for case in CASES {
        let contract = contract(case);
        let first = run_case(case, &contract, 0);
        let second = run_case(case, &contract, 1);
        assert_eq!(
            first,
            second,
            "case={} must normalize identically across two fresh strict pipelines",
            case.id()
        );
        outcomes.push((case, first));
    }

    if let Some(output) = env::var_os("PDF_RS_M2_SCENE_GATE_OUTPUT") {
        write_outputs(Path::new(&output), &outcomes);
    }
}

fn contract(case: Case) -> CaseContract {
    let manifest =
        validate_manifest(case.manifest()).expect("normative M2 Scene manifest validates");
    assert_eq!(manifest.case_id(), case.id());
    assert!(
        manifest
            .string_array("runners", "native")
            .expect("validated manifest has native runners")
            .contains(&REQUIRED_RUNNER)
    );
    assert_eq!(
        manifest.boolean("expected", "scene"),
        Some(matches!(case.expected(), ExpectedOutcome::Ready))
    );
    assert_eq!(
        manifest.string("validity", "strict_expected"),
        Some(match case.expected() {
            ExpectedOutcome::Ready => "success",
            ExpectedOutcome::Unsupported(diagnostic) | ExpectedOutcome::Failed(diagnostic) => {
                diagnostic
            }
        })
    );

    let scene_artifact = manifest
        .string("expected", "scene_artifact")
        .map(str::to_owned);
    let scene_sha256 = manifest
        .string("expected", "scene_sha256")
        .map(str::to_owned);
    if matches!(case.expected(), ExpectedOutcome::Ready) {
        assert_eq!(scene_artifact.as_deref(), Some("expected/scene.json"));
        assert!(scene_sha256.is_some());
    } else {
        assert!(scene_artifact.is_none());
        assert!(scene_sha256.is_none());
    }

    CaseContract {
        input_sha256: manifest.source_sha256().to_owned(),
        scene_sha256,
        max_input_bytes: budget(&manifest, "max_input_bytes"),
        max_objects: budget(&manifest, "max_objects"),
        max_resolve_depth: budget(&manifest, "max_resolve_depth"),
        max_stream_output_bytes: budget(&manifest, "max_stream_output_bytes"),
        max_total_decode_bytes: budget(&manifest, "max_total_decode_bytes"),
        max_scene_commands: budget(&manifest, "max_scene_commands"),
        max_group_depth: budget(&manifest, "max_group_depth"),
        operator_fuel: budget(&manifest, "operator_fuel"),
        decode_fuel: budget(&manifest, "decode_fuel"),
    }
}

fn budget(manifest: &CaseManifest, key: &str) -> u64 {
    manifest
        .positive_u64("budget", key)
        .unwrap_or_else(|| panic!("validated M2 Scene manifest has budget.{key}"))
}

fn run_case(case: Case, contract: &CaseContract, replay: u64) -> NormalizedOutcome {
    verify_input(case, contract);
    let acquired = acquire_strict_page(case, contract, replay);
    let scan_limits = content_limits(contract);
    let vm_limits = vm_limits(contract);
    let property_limits = property_limits(contract);
    let scene_limits = scene_limits(contract);
    let mut job = InterpretPageJob::new(
        acquired.acquired,
        scan_limits,
        vm_limits,
        property_limits,
        scene_limits,
    );

    let outcome = match case {
        Case::CancelBeforePublication => {
            const FINAL_PUBLICATION_SNAPSHOT_THRESHOLD: usize = 9;
            let snapshot_probes = AtomicUsize::new(0);
            let source = CountingSource {
                store: &acquired.store,
                snapshot_probes: &snapshot_probes,
            };
            let cancellation = FinalPublicationCancellation {
                snapshot_probes: &snapshot_probes,
                first_cancel_snapshot: AtomicUsize::new(usize::MAX),
                scanner_probes_at_stable_snapshot: AtomicUsize::new(0),
                threshold: FINAL_PUBLICATION_SNAPSHOT_THRESHOLD,
            };
            let outcome = job.poll(&source, &cancellation);
            assert_eq!(
                cancellation.first_cancel_snapshot.load(Ordering::Acquire),
                FINAL_PUBLICATION_SNAPSHOT_THRESHOLD,
                "cancellation must first become visible after scene.finish at the final publication guard"
            );
            assert_eq!(
                snapshot_probes.load(Ordering::Acquire),
                FINAL_PUBLICATION_SNAPSHOT_THRESHOLD + 1,
                "the cancelling final guard rechecks source identity after observing cancellation"
            );
            assert!(
                cancellation
                    .scanner_probes_at_stable_snapshot
                    .load(Ordering::Acquire)
                    > 0,
                "the pure scanner must probe cancellation while the source snapshot probe count stays unchanged"
            );
            outcome
        }
        Case::SourceChangeBeforeResume => {
            acquired
                .store
                .signal_source_changed()
                .expect("the source-change schedule invalidates the fully supplied store");
            let changed = ChangedSnapshotSource {
                store: &acquired.store,
                snapshot: replacement_snapshot(acquired.snapshot, case.seed()),
            };
            // RangeStore poisoning and source identity are separate observations. This adapter
            // models the resumed host generation: the store is signalled first, then the VM sees
            // the replacement immutable snapshot before any possible source poll.
            job.poll(&changed, &NeverCancelled)
        }
        _ => job.poll(&acquired.store, &NeverCancelled),
    };
    normalize(case, contract, acquired.snapshot, scene_limits, outcome)
}

fn verify_input(case: Case, contract: &CaseContract) {
    let input_len = u64::try_from(case.input().len()).expect("normative input length fits u64");
    assert!(input_len <= contract.max_input_bytes);
    let digest = format!(
        "sha256:{}",
        hex_digest(&sha256(case.input()).expect("normative input fits SHA-256 framing"))
    );
    assert_eq!(digest, contract.input_sha256);
}

fn acquire_strict_page(case: Case, contract: &CaseContract, replay: u64) -> AcquiredFixture {
    let source_len = u64::try_from(case.input().len()).expect("normative input length fits u64");
    let snapshot = snapshot(source_len, case.seed());
    let store = RangeStore::new(snapshot, range_limits(contract))
        .expect("normative Range store limits validate");
    let full_range = ByteRange::new(0, source_len).expect("normative input is non-empty");
    store
        .supply(
            RangeResponse::new(snapshot, full_range, case.input().to_vec())
                .expect("one full response is snapshot-bound"),
        )
        .expect("the complete normative input fits the Range store");

    let base = u64::from(case.seed())
        .checked_mul(10_000)
        .and_then(|value| value.checked_add(replay * 100))
        .expect("test runtime identity arithmetic fits u64");
    let open_job = JobId::new(base + 1);
    let mut open = OpenStrictBaseRevisionJob::new(
        snapshot,
        RevisionId::new(u32::from(case.seed())),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                open_job,
                ResumeCheckpoint::new(base + 2),
                ResumeCheckpoint::new(base + 3),
            ),
            RevisionAttestationJobContext::new(
                open_job,
                ResumeCheckpoint::new(base + 4),
                ResumeCheckpoint::new(base + 5),
                ResumeCheckpoint::new(base + 6),
                RequestPriority::VisiblePage,
            ),
        ),
        strict_limits(contract),
    )
    .expect("normative strict-open configuration validates");
    let authority = match open.poll(&store, &NeverCancelled) {
        StrictBaseOpenPoll::Ready(authority) => authority,
        StrictBaseOpenPoll::Pending { .. } => {
            panic!("one fully supplied normative input must not suspend strict open")
        }
        StrictBaseOpenPoll::Failed(error) => {
            let document = error.document();
            panic!(
                "normative input must strictly open: {error}; limit={:?}; object={:?}",
                document.and_then(|document| document.limit()),
                document.and_then(|document| document.object_error())
            )
        }
    };
    assert_eq!(authority.startxref(), case.startxref());

    let tree_limits = page_tree_limits(contract);
    let index_limits = PageIndexLimits::new(1, object_work_bytes(contract))
        .expect("one-page index limits validate");
    let mut build = authority
        .build_page_index(tree_context(base + 10), tree_limits, index_limits)
        .expect("strict authority mints a cold page-index job");
    let cold = match build.poll(&store, &NeverCancelled) {
        PageIndexBuildPoll::Ready(index) => index,
        PageIndexBuildPoll::Pending { .. } => {
            panic!("one fully supplied normative input must not suspend cold index construction")
        }
        PageIndexBuildPoll::Failed(error) => {
            panic!("normative cold page index must build: {error}")
        }
    };

    let mut lookup = authority
        .lookup_page(&cold, 0, tree_context(base + 20), tree_limits)
        .expect("strict authority mints a page-zero lookup");
    let lookup = match lookup.poll(&store, &NeverCancelled) {
        PageLookupPoll::Ready(lookup) => lookup,
        PageLookupPoll::Pending { .. } => {
            panic!("one fully supplied normative input must not suspend page lookup")
        }
        PageLookupPoll::Failed(error) => {
            panic!("normative page zero must resolve: {error}")
        }
    };
    let (index, handle) = lookup.into_parts();
    assert_eq!(handle.index(), 0);
    assert_eq!(handle.object(), object_ref(PAGE_OBJECT));
    index
        .validate_handle(handle)
        .expect("the refined index validates its exact page handle");

    let mut materialize = authority
        .materialize_page(
            &index,
            handle,
            materialization_context(base + 30),
            materialization_limits(contract),
        )
        .expect("strict authority mints page materialization");
    let page = match materialize.poll(&store, &NeverCancelled) {
        PageMaterializationPoll::Ready(page) => page,
        PageMaterializationPoll::Pending { .. } => {
            panic!("one fully supplied normative input must not suspend materialization")
        }
        PageMaterializationPoll::Failed(error) => {
            panic!("normative page values must materialize: {error}")
        }
    };

    let mut content = authority
        .acquire_page_content(
            &index,
            page,
            content_context(base + 40),
            page_content_limits(contract),
        )
        .expect("strict authority mints Page content acquisition");
    let acquired = match content.poll(&store, &NeverCancelled) {
        PageContentPoll::Ready(acquired) => acquired,
        PageContentPoll::Pending { .. } => {
            panic!("one fully supplied normative input must not suspend content acquisition")
        }
        PageContentPoll::Failed(error) => {
            panic!("normative Page content must acquire: {error}")
        }
    };
    assert_eq!(acquired.handle(), handle);
    assert_eq!(acquired.len(), 1);
    assert_eq!(
        acquired.streams()[0].reference(),
        object_ref(CONTENT_OBJECT)
    );

    AcquiredFixture {
        acquired,
        store,
        snapshot,
    }
}

fn normalize(
    case: Case,
    contract: &CaseContract,
    snapshot: SourceSnapshot,
    scene_limits: SceneLimits,
    outcome: ContentVmPoll,
) -> NormalizedOutcome {
    match (case.expected(), outcome) {
        (ExpectedOutcome::Ready, ContentVmPoll::Ready(page)) => {
            let model = model_scene(case, snapshot, scene_limits);
            let diff = compare_scenes(&model, page.scene(), SceneDiffLimits::default())
                .expect("bounded semantic Scene comparison succeeds");
            assert!(
                diff.is_exact(),
                "case={} actual Scene differs from the independent model: {:?}",
                case.id(),
                diff.differences()
            );
            let actual = page
                .scene()
                .canonical_json_bytes()
                .expect("actual Scene canonicalization stays within the case budget");
            let expected = model
                .canonical_json_bytes()
                .expect("model Scene canonicalization stays within the case budget");
            assert_eq!(actual, expected);

            let golden = case
                .golden_scene()
                .expect("a Ready normative case has a committed Scene golden");
            assert!(
                !golden.ends_with(b"\n"),
                "case={} Scene golden must have no trailing newline",
                case.id()
            );
            assert_eq!(actual, golden, "case={} Scene golden drift", case.id());
            let scene_sha256 = format!(
                "sha256:{}",
                hex_digest(&sha256(&actual).expect("canonical Scene fits SHA-256 framing"))
            );
            assert_eq!(
                contract.scene_sha256.as_deref(),
                Some(scene_sha256.as_str())
            );
            let diff = diff
                .canonical_json_bytes()
                .expect("exact semantic diff canonicalization succeeds");
            NormalizedOutcome {
                kind: OutcomeKind::Ready,
                diagnostic_id: None,
                diagnostic: None,
                scene_sha256: Some(scene_sha256),
                scene: Some(actual),
                diff: Some(diff),
            }
        }
        (ExpectedOutcome::Unsupported(expected), ContentVmPoll::Unsupported(error)) => {
            let diagnostic_id = error.diagnostic_id();
            let diagnostic = error.to_string();
            assert_eq!(diagnostic_id, expected);
            assert_eq!(diagnostic, expected);
            NormalizedOutcome {
                kind: OutcomeKind::Unsupported,
                diagnostic_id: Some(diagnostic_id.to_owned()),
                diagnostic: Some(diagnostic),
                scene_sha256: None,
                scene: None,
                diff: None,
            }
        }
        (ExpectedOutcome::Failed(expected), ContentVmPoll::Failed(error)) => {
            let diagnostic_id = error.diagnostic_id();
            let diagnostic = error.to_string();
            assert_eq!(diagnostic_id, expected);
            assert_eq!(diagnostic, expected);
            NormalizedOutcome {
                kind: OutcomeKind::Failed,
                diagnostic_id: Some(diagnostic_id.to_owned()),
                diagnostic: Some(diagnostic),
                scene_sha256: None,
                scene: None,
                diff: None,
            }
        }
        (expected, actual) => {
            panic!(
                "case={} expected {expected:?}, received {actual:?}; failure/unsupported must not publish a Scene",
                case.id()
            )
        }
    }
}

fn model_scene(case: Case, snapshot: SourceSnapshot, limits: SceneLimits) -> Scene {
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(200_000_000_000),
        SceneScalar::from_scaled(300_000_000_000),
    ])
    .expect("independent model geometry has positive area");
    let mut builder = SceneBuilder::new(
        SceneBinding::new(
            snapshot.identity(),
            case.startxref(),
            0,
            object_ref(PAGE_OBJECT),
        ),
        PageGeometry::new(media, media, PageRotation::Degrees0),
        limits,
    );
    match case {
        Case::ValidStateAndMarkedContent => {
            builder
                .begin_marked_content(b"Span", None, command_source(40, 8))
                .expect("independent valid model admits Span BMC");
            builder
                .end_marked_content(command_source(44, 9))
                .expect("independent valid model admits EMC");
        }
        Case::ResourceMarkedContentProperties => {
            builder
                .begin_marked_content(b"Span", Some(object_ref(5)), command_source(9, 0))
                .expect("independent resource model admits Span/P BDC");
            builder
                .end_marked_content(command_source(13, 1))
                .expect("independent resource model admits EMC");
        }
        _ => panic!("only successful cases have an independent Scene model"),
    }
    builder
        .finish()
        .expect("independent balanced Scene model publishes")
}

fn command_source(decoded_start: u64, operator_index: u32) -> CommandSource {
    CommandSource::new(
        object_ref(CONTENT_OBJECT),
        0,
        decoded_start,
        3,
        operator_index,
    )
    .expect("independent command provenance is representable")
}

fn write_outputs(root: &Path, outcomes: &[(Case, NormalizedOutcome)]) {
    assert!(
        root.file_name().is_some(),
        "M2 Scene output must name a dedicated directory"
    );
    if root.exists() {
        assert!(
            root.is_dir(),
            "existing M2 Scene output root must be a directory"
        );
        assert!(
            fs::read_dir(root)
                .expect("existing M2 Scene output root is readable")
                .next()
                .is_none(),
            "existing M2 Scene output root must be empty"
        );
    }
    fs::create_dir_all(root).expect("M2 Scene gate output root is writable");
    fs::write(root.join("result.json"), result_json(outcomes))
        .expect("canonical M2 Scene gate result is writable");
    for (case, outcome) in outcomes {
        if outcome.kind != OutcomeKind::Ready {
            assert!(outcome.scene.is_none());
            assert!(outcome.diff.is_none());
            continue;
        }
        let directory = root.join(case.id());
        fs::create_dir_all(&directory).expect("successful case output directory is writable");
        fs::write(
            directory.join("scene.json"),
            outcome
                .scene
                .as_deref()
                .expect("Ready normalization retains canonical Scene"),
        )
        .expect("successful canonical Scene output is writable");
        fs::write(
            directory.join("diff.json"),
            outcome
                .diff
                .as_deref()
                .expect("Ready normalization retains canonical semantic diff"),
        )
        .expect("successful canonical Scene diff output is writable");
    }
}

fn result_json(outcomes: &[(Case, NormalizedOutcome)]) -> Vec<u8> {
    let mut output = String::from("{\"cases\":[");
    for (index, (case, outcome)) in outcomes.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str("{\"case_id\":\"");
        output.push_str(case.id());
        output.push_str("\",\"diagnostic\":");
        push_optional_json_string(&mut output, outcome.diagnostic.as_deref());
        output.push_str(",\"diagnostic_id\":");
        push_optional_json_string(&mut output, outcome.diagnostic_id.as_deref());
        output.push_str(",\"outcome\":\"");
        output.push_str(outcome.kind.label());
        output.push_str("\",\"scene_sha256\":");
        push_optional_json_string(&mut output, outcome.scene_sha256.as_deref());
        output.push('}');
    }
    output.push_str("],\"schema\":1}");
    output.into_bytes()
}

fn push_optional_json_string(output: &mut String, value: Option<&str>) {
    match value {
        Some(value) => {
            assert!(
                value
                    .bytes()
                    .all(|byte| !byte.is_ascii_control() && !b"\\\"".contains(&byte)),
                "stable diagnostics and hashes require no JSON escaping"
            );
            output.push('"');
            output.push_str(value);
            output.push('"');
        }
        None => output.push_str("null"),
    }
}

fn snapshot(source_len: u64, seed: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        Some(source_len),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn replacement_snapshot(original: SourceSnapshot, seed: u8) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed ^ 0xff; 32]),
            SourceRevision::new(u64::from(seed) + 1),
        ),
        original.len(),
        SourceValidator::new(
            SourceValidatorKind::FrozenResponse,
            [seed.wrapping_add(2); 32],
        ),
    )
}

struct ChangedSnapshotSource<'store> {
    store: &'store RangeStore,
    snapshot: SourceSnapshot,
}

impl ByteSource for ChangedSnapshotSource<'_> {
    fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        self.store.poll(request)
    }
}

struct CountingSource<'store, 'counter> {
    store: &'store RangeStore,
    snapshot_probes: &'counter AtomicUsize,
}

impl ByteSource for CountingSource<'_, '_> {
    fn snapshot(&self) -> SourceSnapshot {
        let _ = self.snapshot_probes.fetch_add(1, Ordering::AcqRel);
        self.store.snapshot()
    }

    fn poll(&self, request: ReadRequest) -> ReadPoll<ByteSlice> {
        self.store.poll(request)
    }
}

struct FinalPublicationCancellation<'counter> {
    snapshot_probes: &'counter AtomicUsize,
    first_cancel_snapshot: AtomicUsize,
    scanner_probes_at_stable_snapshot: AtomicUsize,
    threshold: usize,
}

impl DocumentCancellation for FinalPublicationCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        let snapshot_probes = self.snapshot_probes.load(Ordering::Acquire);
        if snapshot_probes == 2 {
            let _ = self
                .scanner_probes_at_stable_snapshot
                .fetch_add(1, Ordering::AcqRel);
        }
        if snapshot_probes < self.threshold {
            return false;
        }
        let _ = self.first_cancel_snapshot.compare_exchange(
            usize::MAX,
            snapshot_probes,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        true
    }
}

fn object_ref(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("normative object reference is nonzero")
}

fn tree_context(seed: u64) -> PageTreeJobContext {
    PageTreeJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn materialization_context(seed: u64) -> PageMaterializationJobContext {
    PageMaterializationJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn content_context(seed: u64) -> PageContentJobContext {
    PageContentJobContext::new(
        JobId::new(seed),
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        ResumeCheckpoint::new(seed + 3),
        RequestPriority::VisiblePage,
    )
}

fn range_limits(contract: &CaseContract) -> RangeStoreLimits {
    RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_read_bytes: contract.max_input_bytes,
        max_cached_bytes: contract.max_input_bytes,
        max_resident_bytes: checked_product(contract.max_input_bytes, 2, "Range residency"),
        ..RangeStoreLimitConfig::default()
    })
    .expect("normative Range limits validate")
}

fn strict_limits(contract: &CaseContract) -> StrictBaseOpenLimits {
    StrictBaseOpenLimits::new(
        xref_limits(contract),
        document_limits(contract),
        attestation_limits(contract),
        object_limits(contract),
        syntax_limits(contract),
    )
}

fn xref_limits(contract: &CaseContract) -> XrefLimits {
    let entries = contract
        .max_objects
        .checked_add(1)
        .expect("xref free row fits u64");
    let source_work = checked_product(contract.max_input_bytes, 2, "xref source work");
    XrefLimits::validate(XrefLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_tail_bytes: contract.max_input_bytes,
        max_tail_bytes: contract.max_input_bytes,
        initial_section_bytes: contract.max_input_bytes,
        max_section_bytes: contract.max_input_bytes,
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
        max_subsections: entries,
        max_entries: entries,
    })
    .expect("normative xref limits validate")
}

fn document_limits(contract: &CaseContract) -> DocumentLimits {
    let entries = contract
        .max_objects
        .checked_add(1)
        .expect("document free row fits u64");
    DocumentLimits::validate(DocumentLimitConfig {
        max_total_entries: entries,
        max_in_use_entries: contract.max_objects,
        max_logical_index_bytes: object_work_bytes(contract),
        max_sort_steps: checked_product(entries, entries, "document sort work"),
    })
    .expect("normative document-index limits validate")
}

fn attestation_limits(contract: &CaseContract) -> RevisionAttestationLimits {
    let object_work = object_work_bytes(contract);
    RevisionAttestationLimits::validate(RevisionAttestationLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        max_objects: contract.max_objects,
        scan_chunk_bytes: contract.max_input_bytes,
        max_trivia_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_evidence_bytes: object_work,
    })
    .expect("normative revision-attestation limits validate")
}

fn object_limits(contract: &CaseContract) -> ObjectLimits {
    let source_work = checked_product(contract.max_input_bytes, 2, "object source work");
    ObjectLimits::validate(ObjectLimitConfig {
        max_source_bytes: contract.max_input_bytes,
        initial_envelope_bytes: contract.max_input_bytes,
        max_envelope_bytes: contract.max_input_bytes,
        initial_boundary_bytes: contract.max_input_bytes,
        max_boundary_bytes: contract.max_input_bytes,
        max_stream_bytes: contract
            .max_stream_output_bytes
            .min(contract.max_input_bytes),
        max_total_read_bytes: source_work,
        max_total_parse_bytes: source_work,
    })
    .expect("normative object limits validate")
}

fn syntax_limits(contract: &CaseContract) -> SyntaxLimits {
    SyntaxLimits::validate(SyntaxLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_token_bytes: contract.max_input_bytes,
        max_comment_bytes: contract.max_input_bytes,
        max_name_bytes: contract.max_input_bytes,
        max_string_source_bytes: contract.max_input_bytes,
        max_string_decoded_bytes: contract.max_stream_output_bytes,
        max_owned_bytes: contract
            .max_total_decode_bytes
            .max(contract.max_input_bytes),
        max_total_tokens: contract.operator_fuel.min(contract.decode_fuel),
        max_container_entries: contract.operator_fuel,
        max_container_bytes: object_work_bytes(contract),
        max_container_depth: u16::try_from(contract.max_resolve_depth)
            .expect("resolve depth fits syntax type"),
    })
    .expect("normative syntax limits validate")
}

fn page_tree_limits(contract: &CaseContract) -> PageTreeLimits {
    let object_work = object_work_bytes(contract);
    PageTreeLimits::validate(PageTreeLimitConfig {
        max_nodes: contract.max_objects,
        max_depth: contract.max_resolve_depth,
        max_pages: 1,
        max_kids_per_node: contract.max_objects,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_traversal_bytes: object_work,
    })
    .expect("normative page-tree limits validate")
}

fn materialization_limits(contract: &CaseContract) -> PageMaterializationLimits {
    let object_work = object_work_bytes(contract);
    PageMaterializationLimits::validate(PageMaterializationLimitConfig {
        max_ancestor_depth: contract.max_resolve_depth,
        max_objects: contract.max_objects,
        max_reference_edges: contract.max_objects,
        max_total_object_read_bytes: object_work,
        max_total_object_parse_bytes: object_work,
        max_retained_state_bytes: object_work,
    })
    .expect("normative materialization limits validate")
}

fn decode_limits(contract: &CaseContract) -> DecodeLimits {
    DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: contract.max_input_bytes,
        max_filters: u16::try_from(contract.max_objects).expect("filter count fits u16"),
        max_layer_output_bytes: contract.max_stream_output_bytes,
        max_total_output_bytes: contract.max_total_decode_bytes,
        max_final_output_bytes: contract.max_stream_output_bytes,
        max_retained_capacity_bytes: contract.max_total_decode_bytes,
        max_fuel: contract.decode_fuel,
        cancellation_check_interval_fuel: contract.decode_fuel.min(256),
    })
    .expect("normative decoder limits validate")
}

fn page_content_limits(contract: &CaseContract) -> PageContentLimits {
    PageContentLimits::validate(PageContentLimitConfig {
        max_streams: contract.max_objects,
        max_array_entries: contract.max_objects,
        max_objects: contract.max_objects,
        max_reference_edges: contract.max_objects,
        max_alias_depth: contract.max_resolve_depth,
        max_total_object_read_bytes: object_work_bytes(contract),
        max_total_object_parse_bytes: object_work_bytes(contract),
        max_total_encoded_bytes: contract.max_input_bytes,
        max_total_decoded_bytes: contract.max_total_decode_bytes,
        max_total_decode_fuel: contract.decode_fuel,
        max_retained_state_bytes: object_work_bytes(contract),
        decode_limits: decode_limits(contract),
    })
    .expect("normative Page content limits validate")
}

fn content_limits(contract: &CaseContract) -> ContentLimits {
    ContentLimits::validate(ContentLimitConfig {
        max_streams: u32::try_from(contract.max_objects).expect("stream count fits u32"),
        max_total_decoded_bytes: contract.max_total_decode_bytes,
        max_tokens: contract.operator_fuel,
        max_token_bytes: contract.max_input_bytes,
        max_operands_per_operator: u32::try_from(contract.max_objects)
            .expect("operand count fits u32"),
        max_nesting_depth: u16::try_from(contract.max_resolve_depth)
            .expect("content nesting fits u16"),
        max_operators: contract.operator_fuel,
        max_fuel: contract.operator_fuel,
        max_retained_bytes: object_work_bytes(contract),
    })
    .expect("normative content scanner limits validate")
}

fn vm_limits(contract: &CaseContract) -> ContentVmLimits {
    ContentVmLimits::validate(ContentVmLimitConfig {
        max_operators: contract.operator_fuel,
        max_fuel: contract.operator_fuel,
        max_graphics_state_depth: u32::try_from(contract.max_group_depth)
            .expect("graphics depth fits u32"),
        max_compatibility_depth: u32::try_from(contract.max_group_depth)
            .expect("compatibility depth fits u32"),
        max_marked_content_depth: u32::try_from(contract.max_group_depth)
            .expect("marked-content depth fits u32"),
        max_property_uses: contract.max_scene_commands,
        max_retained_bytes: object_work_bytes(contract),
    })
    .expect("normative Content VM limits validate")
}

fn property_limits(contract: &CaseContract) -> PagePropertyLookupLimits {
    PagePropertyLookupLimits::validate(PagePropertyLookupLimitConfig {
        max_lookups: contract.max_scene_commands,
        max_entry_visits: checked_product(
            contract.max_objects,
            contract.max_resolve_depth,
            "property entry visits",
        ),
    })
    .expect("normative Page property limits validate")
}

fn scene_limits(contract: &CaseContract) -> SceneLimits {
    SceneLimits::validate(SceneLimitConfig {
        max_commands: u32::try_from(contract.max_scene_commands)
            .expect("Scene command count fits u32"),
        max_resources: u32::try_from(contract.max_objects).expect("Scene resources fit u32"),
        max_marked_content_depth: u32::try_from(contract.max_group_depth)
            .expect("Scene marked-content depth fits u32"),
        max_name_bytes: u32::try_from(contract.max_input_bytes).expect("Scene name bytes fit u32"),
        max_retained_bytes: object_work_bytes(contract),
        max_resource_index_work: contract.operator_fuel,
        max_canonical_bytes: contract.max_total_decode_bytes,
    })
    .expect("normative Scene limits validate")
}

fn object_work_bytes(contract: &CaseContract) -> u64 {
    checked_product(
        contract.max_input_bytes,
        contract.max_objects,
        "per-object source work",
    )
}

fn checked_product(left: u64, right: u64, label: &str) -> u64 {
    left.checked_mul(right)
        .unwrap_or_else(|| panic!("normative case budget overflow: {label}"))
}
