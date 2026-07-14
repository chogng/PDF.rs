use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    ByteRange, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SourceIdentity, SourceRevision,
    SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_document::{
    CandidateRevisionIndex, DocumentErrorCode, DocumentLimitConfig, DocumentLimitKind,
    DocumentLimits, NeverCancelled, RevisionId,
};
use pdf_rs_syntax::{ObjectRef, SyntaxLimits};
use pdf_rs_xref::{
    OpenXrefJob, XrefError, XrefErrorCode, XrefJobContext, XrefLimits, XrefPoll, XrefSection,
};

const CANONICAL_STARTXREF: u64 = 449;

#[derive(Clone, Copy)]
enum Row {
    Free { next_free: u32, generation: u16 },
    InUse { offset: u64, generation: u16 },
}

fn snapshot(len: u64) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new([0x64; 32]), SourceRevision::new(3)),
        Some(len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0xa7; 32]),
    )
}

fn build_pdf(startxref: u64, rows: &[Row], root: (u32, u16)) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let prefix_end = usize::try_from(startxref).expect("fixture startxref fits usize");
    assert!(prefix_end > pdf.len());
    pdf.resize(prefix_end - 1, b'x');
    pdf.push(b'\n');
    assert_eq!(pdf.len(), prefix_end);

    pdf.extend_from_slice(format!("xref\n0 {}\n", rows.len()).as_bytes());
    for row in rows {
        let encoded = match *row {
            Row::Free {
                next_free,
                generation,
            } => format!("{next_free:010} {generation:05} f \n"),
            Row::InUse { offset, generation } => {
                format!("{offset:010} {generation:05} n \n")
            }
        };
        assert_eq!(encoded.len(), 20, "xref rows remain fixed-width");
        pdf.extend_from_slice(encoded.as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {} {} R >>\nstartxref\n{startxref}\n%%EOF\n",
            rows.len(),
            root.0,
            root.1
        )
        .as_bytes(),
    );
    pdf
}

fn parse_xref(bytes: &[u8]) -> Result<XrefSection, XrefError> {
    let source = snapshot(u64::try_from(bytes.len()).expect("fixture length fits u64"));
    let store = RangeStore::new(source, Default::default()).expect("test RangeStore is valid");
    let complete = ByteRange::new(0, source.len().expect("fixture length is known"))
        .expect("complete test range is valid");
    store
        .supply(
            RangeResponse::new(source, complete, bytes.to_vec())
                .expect("test response matches its snapshot"),
        )
        .expect("test bytes fit default RangeStore limits");
    let mut job = OpenXrefJob::new(
        source,
        XrefJobContext::new(
            JobId::new(90),
            ResumeCheckpoint::new(91),
            ResumeCheckpoint::new(92),
        ),
        XrefLimits::default(),
        SyntaxLimits::default(),
    )
    .expect("public xref job configuration is valid");
    match job.poll(&store, &pdf_rs_xref::NeverCancelled) {
        XrefPoll::Ready(section) => Ok(section),
        XrefPoll::Failed(error) => Err(error),
        XrefPoll::Pending { .. } => panic!("a completely supplied fixture must not suspend"),
    }
}

fn index(bytes: &[u8]) -> CandidateRevisionIndex {
    let section = parse_xref(bytes).expect("fixture xref must parse");
    CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(7),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .expect("fixture candidate index must build")
}

fn canonical_rows() -> [Row; 5] {
    [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::InUse {
            offset: 186,
            generation: 0,
        },
        Row::InUse {
            offset: 235,
            generation: 0,
        },
        Row::InUse {
            offset: 292,
            generation: 0,
        },
        Row::InUse {
            offset: 396,
            generation: 0,
        },
    ]
}

#[test]
fn canonical_revision_yields_exact_intervals_and_five_field_unattested_targets() {
    let bytes = build_pdf(CANONICAL_STARTXREF, &canonical_rows(), (1, 0));
    let candidate = index(&bytes);
    let source = snapshot(u64::try_from(bytes.len()).unwrap());

    assert_eq!(candidate.snapshot(), source);
    assert_eq!(candidate.revision_id(), RevisionId::new(7));
    assert_eq!(candidate.startxref(), CANONICAL_STARTXREF);
    assert_eq!(candidate.root(), ObjectRef::new(1, 0).unwrap());
    assert_eq!(candidate.stats().total_entries(), 5);
    assert_eq!(candidate.stats().in_use_entries(), 4);
    assert_eq!(candidate.stats().logical_index_bytes(), 416);

    let expected = [
        (1, 186, 235),
        (2, 235, 292),
        (3, 292, 396),
        (4, 396, CANONICAL_STARTXREF),
    ];
    assert_eq!(candidate.physical_intervals().len(), expected.len());
    for (interval, (number, offset, upper_bound)) in
        candidate.physical_intervals().iter().zip(expected)
    {
        let reference = ObjectRef::new(number, 0).unwrap();
        assert_eq!(interval.revision_id(), RevisionId::new(7));
        assert_eq!(interval.reference(), reference);
        assert_eq!(interval.xref_offset(), offset);
        assert_eq!(interval.object_upper_bound(), upper_bound);
        assert_eq!(interval.len(), upper_bound - offset);
        assert!(!interval.is_empty());
        assert_eq!(candidate.interval(reference).unwrap(), interval);

        let target = candidate.unattested_target(reference).unwrap();
        assert_eq!(target.snapshot(), source);
        assert_eq!(target.reference(), reference);
        assert_eq!(target.xref_offset(), offset);
        assert_eq!(target.object_upper_bound(), upper_bound);
        assert_eq!(target.revision_startxref(), CANONICAL_STARTXREF);
    }
}

#[test]
fn logical_object_order_is_independent_from_physical_offset_order() {
    let rows = [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::InUse {
            offset: 80,
            generation: 0,
        },
        Row::InUse {
            offset: 20,
            generation: 0,
        },
        Row::InUse {
            offset: 50,
            generation: 0,
        },
    ];
    let candidate = index(&build_pdf(128, &rows, (1, 0)));
    let physical = candidate.physical_intervals();
    assert_eq!(
        physical
            .iter()
            .map(|interval| (
                interval.reference().number(),
                interval.xref_offset(),
                interval.object_upper_bound()
            ))
            .collect::<Vec<_>>(),
        [(2, 20, 50), (3, 50, 80), (1, 80, 128)]
    );
    assert_eq!(
        candidate
            .interval(ObjectRef::new(1, 0).unwrap())
            .unwrap()
            .xref_offset(),
        80
    );
}

#[test]
fn duplicate_in_use_physical_offsets_are_rejected() {
    let rows = [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::InUse {
            offset: 20,
            generation: 0,
        },
        Row::InUse {
            offset: 20,
            generation: 0,
        },
    ];
    let section = parse_xref(&build_pdf(128, &rows, (1, 0))).unwrap();
    let error = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(1),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::DuplicatePhysicalOffset);
    assert_eq!(error.offset(), Some(20));
}

#[test]
fn offsets_equal_to_or_larger_than_startxref_are_rejected() {
    for offset in [128, 129] {
        let rows = [
            Row::Free {
                next_free: 0,
                generation: u16::MAX,
            },
            Row::InUse {
                offset,
                generation: 0,
            },
        ];
        let section = parse_xref(&build_pdf(128, &rows, (1, 0))).unwrap();
        let error = CandidateRevisionIndex::from_xref(
            &section,
            RevisionId::new(1),
            DocumentLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::InvalidPhysicalOffset);
        assert_eq!(error.offset(), Some(offset));
    }
}

#[test]
fn public_xref_gate_rejects_free_or_wrong_generation_trailer_roots() {
    let free_root_rows = [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::Free {
            next_free: 0,
            generation: 3,
        },
    ];
    let error = parse_xref(&build_pdf(128, &free_root_rows, (1, 3))).unwrap_err();
    assert_eq!(error.code(), XrefErrorCode::InvalidTrailer);

    let generation_rows = [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::InUse {
            offset: 20,
            generation: 2,
        },
    ];
    let error = parse_xref(&build_pdf(128, &generation_rows, (1, 1))).unwrap_err();
    assert_eq!(error.code(), XrefErrorCode::InvalidTrailer);
}

#[test]
fn lookup_distinguishes_missing_free_and_generation_mismatch() {
    let rows = [
        Row::Free {
            next_free: 0,
            generation: u16::MAX,
        },
        Row::InUse {
            offset: 20,
            generation: 0,
        },
        Row::Free {
            next_free: 0,
            generation: 3,
        },
        Row::InUse {
            offset: 60,
            generation: 4,
        },
    ];
    let candidate = index(&build_pdf(128, &rows, (1, 0)));

    let missing = candidate
        .interval(ObjectRef::new(5, 0).unwrap())
        .unwrap_err();
    assert_eq!(missing.code(), DocumentErrorCode::MissingObject);

    let free = candidate
        .interval(ObjectRef::new(2, 3).unwrap())
        .unwrap_err();
    assert_eq!(free.code(), DocumentErrorCode::FreeObject);

    let generation = candidate
        .interval(ObjectRef::new(3, 5).unwrap())
        .unwrap_err();
    assert_eq!(generation.code(), DocumentErrorCode::GenerationMismatch);
}

#[test]
fn pre_cancelled_construction_stops_before_index_work() {
    let bytes = build_pdf(CANONICAL_STARTXREF, &canonical_rows(), (1, 0));
    let section = parse_xref(&bytes).unwrap();
    let cancellation = AtomicBool::new(true);
    let error = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(1),
        DocumentLimits::default(),
        &cancellation,
    )
    .unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::Cancelled);
}

#[test]
fn construction_budgets_accept_exact_work_and_reject_one_less() {
    let bytes = build_pdf(CANONICAL_STARTXREF, &canonical_rows(), (1, 0));
    let section = parse_xref(&bytes).unwrap();
    let baseline = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(1),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let stats = baseline.stats();
    assert!(stats.sort_steps() > 1);

    let exact = DocumentLimitConfig {
        max_total_entries: stats.total_entries(),
        max_in_use_entries: stats.in_use_entries(),
        max_logical_index_bytes: stats.logical_index_bytes(),
        max_sort_steps: stats.sort_steps(),
    };
    let exact_index = CandidateRevisionIndex::from_xref(
        &section,
        RevisionId::new(1),
        DocumentLimits::validate(exact).unwrap(),
        &NeverCancelled,
    )
    .expect("the exact observed construction budget must be accepted");
    assert_eq!(exact_index.stats(), stats);

    let cases = [
        (
            DocumentLimitConfig {
                max_total_entries: stats.total_entries() - 1,
                ..exact
            },
            DocumentLimitKind::TotalEntries,
        ),
        (
            DocumentLimitConfig {
                max_in_use_entries: stats.in_use_entries() - 1,
                ..exact
            },
            DocumentLimitKind::InUseEntries,
        ),
        (
            DocumentLimitConfig {
                max_logical_index_bytes: stats.logical_index_bytes() - 1,
                ..exact
            },
            DocumentLimitKind::LogicalIndexBytes,
        ),
        (
            DocumentLimitConfig {
                max_sort_steps: stats.sort_steps() - 1,
                ..exact
            },
            DocumentLimitKind::SortSteps,
        ),
    ];
    for (config, expected_kind) in cases {
        let error = CandidateRevisionIndex::from_xref(
            &section,
            RevisionId::new(1),
            DocumentLimits::validate(config).unwrap(),
            &NeverCancelled,
        )
        .unwrap_err();
        assert_eq!(error.code(), DocumentErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), expected_kind);
    }
}
