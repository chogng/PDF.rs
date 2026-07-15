use std::sync::atomic::AtomicBool;

use pdf_rs_bytes::{
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_syntax::ObjectRef;
use pdf_rs_xref::{
    HybridSupplement, NeverCancelled, RevisionCandidate, RevisionEntry, RevisionEntryKind,
    RevisionEntryOrigin, RevisionErrorCategory, RevisionErrorCode, RevisionLimitConfig,
    RevisionLimitKind, RevisionLimits, RevisionPrimaryKind, XrefRecoverability,
    compose_revision_chain,
};

fn identity(byte: u8) -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([byte; 32]), SourceRevision::new(21))
}

fn snapshot_for(source: SourceIdentity) -> SourceSnapshot {
    SourceSnapshot::new(
        source,
        Some(1200),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x63; 32]),
    )
}

fn root() -> ObjectRef {
    ObjectRef::new(1, 0).unwrap()
}

fn base(snapshot: SourceSnapshot) -> RevisionCandidate {
    RevisionCandidate::traditional(
        snapshot,
        300,
        5,
        root(),
        None,
        vec![
            RevisionEntry::free(0, 0, u16::MAX),
            RevisionEntry::uncompressed(1, 40, 0),
            RevisionEntry::uncompressed(2, 80, 0),
            RevisionEntry::uncompressed(3, 120, 0),
            RevisionEntry::uncompressed(4, 160, 0),
        ],
    )
}

fn hybrid_update(snapshot: SourceSnapshot) -> RevisionCandidate {
    let supplement = HybridSupplement::new(
        snapshot,
        620,
        ObjectRef::new(7, 0).unwrap(),
        8,
        Some(300),
        vec![
            RevisionEntry::compressed(4, 6, 2),
            RevisionEntry::uncompressed(5, 580, 0),
            RevisionEntry::uncompressed(7, 620, 0),
        ],
    );
    RevisionCandidate::traditional(
        snapshot,
        700,
        8,
        root(),
        Some(300),
        vec![
            RevisionEntry::uncompressed(2, 420, 0),
            RevisionEntry::free(3, 0, 1),
            RevisionEntry::uncompressed(4, 500, 0),
            RevisionEntry::uncompressed(6, 540, 0),
        ],
    )
    .with_hybrid_supplement(supplement)
}

fn canonical() -> pdf_rs_xref::RevisionChain {
    let snapshot = snapshot_for(identity(0x72));
    compose_revision_chain(
        vec![hybrid_update(snapshot), base(snapshot)],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap()
}

#[test]
fn newest_primary_then_hybrid_then_older_lookup_is_exact() {
    let chain = canonical();
    assert_eq!(chain.root(), root());
    assert_eq!(chain.revisions().len(), 2);
    assert_eq!(chain.stats().revisions(), 2);
    assert_eq!(chain.stats().sections(), 3);
    assert_eq!(chain.stats().entries(), 12);
    assert_eq!(chain.stats().hybrid_supplements(), 1);

    let old = chain.entry(1).unwrap();
    assert_eq!(old.revision().ordinal(), 0);
    assert_eq!(old.revision_startxref(), 300);
    assert_eq!(old.origin(), RevisionEntryOrigin::Primary);

    let replaced = chain.entry(2).unwrap();
    assert_eq!(replaced.revision().ordinal(), 1);
    assert_eq!(replaced.revision_startxref(), 700);
    assert_eq!(
        replaced.entry().kind(),
        RevisionEntryKind::Uncompressed {
            offset: 420,
            generation: 0
        }
    );

    let freed = chain.entry(3).unwrap();
    assert_eq!(freed.revision().ordinal(), 1);
    assert!(matches!(
        freed.entry().kind(),
        RevisionEntryKind::Free { .. }
    ));

    let table_wins = chain.entry(4).unwrap();
    assert_eq!(table_wins.origin(), RevisionEntryOrigin::Primary);
    assert_eq!(
        table_wins.entry().kind(),
        RevisionEntryKind::Uncompressed {
            offset: 500,
            generation: 0
        }
    );

    let supplement = chain.entry(5).unwrap();
    assert_eq!(supplement.origin(), RevisionEntryOrigin::HybridSupplement);
    assert_eq!(supplement.revision().ordinal(), 1);
    assert!(chain.entry(99).is_none());
    assert!(!format!("{chain:?}").contains("entry_count"));
}

#[test]
fn newest_null_entry_hides_an_older_live_definition() {
    let snapshot = snapshot_for(identity(0x7a));
    let update = RevisionCandidate::traditional(
        snapshot,
        700,
        5,
        root(),
        Some(300),
        vec![RevisionEntry::null(2, 9)],
    );
    let chain = compose_revision_chain(
        vec![update, base(snapshot)],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    let entry = chain.entry(2).unwrap();
    assert_eq!(entry.revision().ordinal(), 1);
    assert_eq!(
        entry.entry().kind(),
        RevisionEntryKind::Null { encoded_type: 9 }
    );
}

#[test]
fn a_primary_stream_can_be_the_newest_incremental_revision() {
    let snapshot = snapshot_for(identity(0x73));
    let stream = RevisionCandidate::xref_stream(
        snapshot,
        900,
        ObjectRef::new(7, 0).unwrap(),
        9,
        root(),
        Some(300),
        vec![
            RevisionEntry::compressed(1, 8, 0),
            RevisionEntry::uncompressed(7, 900, 0),
            RevisionEntry::uncompressed(8, 820, 0),
        ],
    );
    let chain = compose_revision_chain(
        vec![stream, base(snapshot)],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_eq!(
        chain.revisions()[0].primary_kind(),
        RevisionPrimaryKind::Stream
    );
    let root = chain.entry(1).unwrap();
    assert_eq!(root.revision().ordinal(), 1);
    assert_eq!(
        root.entry().kind(),
        RevisionEntryKind::Compressed {
            object_stream: 8,
            index: 0
        }
    );
}

#[test]
fn prev_chain_order_size_and_terminal_shape_are_strict() {
    let snapshot = snapshot_for(identity(0x74));
    for revisions in [
        vec![
            RevisionCandidate::traditional(
                snapshot,
                700,
                8,
                root(),
                Some(301),
                vec![RevisionEntry::uncompressed(1, 500, 0)],
            ),
            base(snapshot),
        ],
        vec![
            RevisionCandidate::traditional(
                snapshot,
                700,
                4,
                root(),
                Some(300),
                vec![RevisionEntry::uncompressed(1, 500, 0)],
            ),
            base(snapshot),
        ],
        vec![RevisionCandidate::traditional(
            snapshot,
            300,
            8,
            root(),
            Some(100),
            vec![RevisionEntry::uncompressed(1, 40, 0)],
        )],
    ] {
        let expected = if revisions
            .first()
            .is_some_and(|revision| revision.declared_size() == 4)
        {
            RevisionErrorCode::InvalidRevision
        } else {
            RevisionErrorCode::InvalidPrevious
        };
        assert_eq!(
            compose_revision_chain(revisions, RevisionLimits::default(), &NeverCancelled)
                .unwrap_err()
                .code(),
            expected
        );
    }
}

#[test]
fn hybrid_metadata_and_placement_are_strict() {
    let snapshot = snapshot_for(identity(0x75));
    let valid_entries = vec![RevisionEntry::compressed(5, 6, 0)];
    let invalid_supplements = [
        HybridSupplement::new(
            snapshot,
            620,
            ObjectRef::new(7, 0).unwrap(),
            7,
            Some(300),
            valid_entries.clone(),
        ),
        HybridSupplement::new(
            snapshot,
            700,
            ObjectRef::new(7, 0).unwrap(),
            8,
            Some(300),
            valid_entries.clone(),
        ),
        HybridSupplement::new(
            snapshot,
            200,
            ObjectRef::new(7, 0).unwrap(),
            8,
            Some(200),
            valid_entries.clone(),
        ),
        HybridSupplement::new(
            snapshot_for(identity(0x76)),
            620,
            ObjectRef::new(7, 0).unwrap(),
            8,
            Some(300),
            valid_entries,
        ),
    ];
    for supplement in invalid_supplements {
        let revision = RevisionCandidate::traditional(
            snapshot,
            700,
            8,
            root(),
            Some(300),
            vec![RevisionEntry::uncompressed(1, 500, 0)],
        )
        .with_hybrid_supplement(supplement);
        assert_eq!(
            compose_revision_chain(
                vec![revision, base(snapshot)],
                RevisionLimits::default(),
                &NeverCancelled,
            )
            .unwrap_err()
            .code(),
            RevisionErrorCode::InvalidHybrid
        );
    }

    let stream_with_supplement = RevisionCandidate::xref_stream(
        snapshot,
        700,
        ObjectRef::new(7, 0).unwrap(),
        8,
        root(),
        Some(300),
        vec![
            RevisionEntry::uncompressed(1, 500, 0),
            RevisionEntry::uncompressed(7, 700, 0),
        ],
    )
    .with_hybrid_supplement(HybridSupplement::new(
        snapshot,
        620,
        ObjectRef::new(7, 0).unwrap(),
        8,
        Some(300),
        vec![RevisionEntry::compressed(5, 6, 0)],
    ));
    assert_eq!(
        compose_revision_chain(
            vec![stream_with_supplement, base(snapshot)],
            RevisionLimits::default(),
            &NeverCancelled,
        )
        .unwrap_err()
        .code(),
        RevisionErrorCode::InvalidHybrid
    );

    let ignored_supplement_prev = HybridSupplement::new(
        snapshot,
        620,
        ObjectRef::new(7, 0).unwrap(),
        8,
        Some(111),
        vec![
            RevisionEntry::compressed(5, 6, 0),
            RevisionEntry::uncompressed(7, 620, 0),
        ],
    );
    assert_eq!(ignored_supplement_prev.previous(), Some(111));
    let update = RevisionCandidate::traditional(
        snapshot,
        700,
        8,
        root(),
        Some(300),
        vec![
            RevisionEntry::uncompressed(1, 500, 0),
            RevisionEntry::uncompressed(6, 540, 0),
        ],
    )
    .with_hybrid_supplement(ignored_supplement_prev);
    compose_revision_chain(
        vec![update, base(snapshot)],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
}

#[test]
fn entries_require_ordered_in_range_semantics_and_source_offsets() {
    let snapshot = snapshot_for(identity(0x77));
    for entries in [
        vec![
            RevisionEntry::uncompressed(2, 80, 0),
            RevisionEntry::uncompressed(1, 40, 0),
        ],
        vec![
            RevisionEntry::uncompressed(1, 40, 0),
            RevisionEntry::free(1, 0, 1),
        ],
        vec![RevisionEntry::uncompressed(8, 40, 0)],
        vec![RevisionEntry::uncompressed(1, 300, 0)],
        vec![RevisionEntry::free(0, 8, u16::MAX)],
        vec![RevisionEntry::compressed(2, 0, 0)],
        vec![RevisionEntry::compressed(2, 2, 0)],
        vec![RevisionEntry::compressed(2, 8, 0)],
    ] {
        let revision = RevisionCandidate::traditional(snapshot, 300, 8, root(), None, entries);
        assert_eq!(
            compose_revision_chain(vec![revision], RevisionLimits::default(), &NeverCancelled,)
                .unwrap_err()
                .code(),
            RevisionErrorCode::InvalidRevision
        );
    }
}

#[test]
fn root_and_source_snapshot_are_validated_after_latest_wins() {
    let snapshot = snapshot_for(identity(0x78));
    let free_root = RevisionCandidate::traditional(
        snapshot,
        700,
        8,
        root(),
        Some(300),
        vec![RevisionEntry::free(1, 0, 1)],
    );
    let error = compose_revision_chain(
        vec![free_root, base(snapshot)],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), RevisionErrorCode::InvalidRoot);
    assert_eq!(error.object_number(), Some(1));

    let foreign = base(snapshot_for(identity(0x79)));
    let error = compose_revision_chain(
        vec![hybrid_update(snapshot), foreign],
        RevisionLimits::default(),
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), RevisionErrorCode::SourceMismatch);
    assert_eq!(error.category(), RevisionErrorCategory::Source);
    assert_eq!(error.recoverability(), XrefRecoverability::ReopenSource);
}

#[test]
fn exact_resource_limits_and_cancellation_are_stable() {
    let measured = canonical();
    let stats = measured.stats();
    let exact = RevisionLimits::validate(RevisionLimitConfig {
        max_revisions: stats.revisions(),
        max_sections: stats.sections(),
        max_entries: stats.entries(),
        max_retained_bytes: stats.retained_bytes(),
    })
    .unwrap();
    let snapshot = snapshot_for(identity(0x72));
    compose_revision_chain(
        vec![hybrid_update(snapshot), base(snapshot)],
        exact,
        &NeverCancelled,
    )
    .unwrap();

    let tight = RevisionLimits::validate(RevisionLimitConfig {
        max_entries: stats.entries() - 1,
        ..RevisionLimitConfig {
            max_revisions: stats.revisions(),
            max_sections: stats.sections(),
            max_entries: stats.entries(),
            max_retained_bytes: stats.retained_bytes(),
        }
    })
    .unwrap();
    let error = compose_revision_chain(
        vec![hybrid_update(snapshot), base(snapshot)],
        tight,
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(error.code(), RevisionErrorCode::ResourceLimit);
    assert_eq!(
        error.limit(),
        Some((RevisionLimitKind::Entries, stats.entries() - 1, 12))
    );

    let section_tight = RevisionLimits::validate(RevisionLimitConfig {
        max_revisions: stats.revisions(),
        max_sections: stats.sections() - 1,
        max_entries: stats.entries(),
        max_retained_bytes: stats.retained_bytes(),
    })
    .unwrap();
    let error = compose_revision_chain(
        vec![hybrid_update(snapshot), base(snapshot)],
        section_tight,
        &NeverCancelled,
    )
    .unwrap_err();
    assert_eq!(
        error.limit(),
        Some((
            RevisionLimitKind::Sections,
            u64::from(stats.sections()) - 1,
            3
        ))
    );

    let cancelled = AtomicBool::new(true);
    let error = compose_revision_chain(vec![base(snapshot)], RevisionLimits::default(), &cancelled)
        .unwrap_err();
    assert_eq!(error.code(), RevisionErrorCode::Cancelled);
    assert_eq!(error.recoverability(), XrefRecoverability::AbandonOperation);
}

#[test]
fn limit_profiles_reject_zero_and_hard_ceiling_overrides() {
    let defaults = RevisionLimitConfig::default();
    for invalid in [
        RevisionLimitConfig {
            max_revisions: 0,
            ..defaults
        },
        RevisionLimitConfig {
            max_revisions: 1025,
            ..defaults
        },
        RevisionLimitConfig {
            max_sections: 0,
            ..defaults
        },
        RevisionLimitConfig {
            max_sections: defaults.max_revisions - 1,
            ..defaults
        },
        RevisionLimitConfig {
            max_sections: 2049,
            ..defaults
        },
        RevisionLimitConfig {
            max_entries: 0,
            ..defaults
        },
        RevisionLimitConfig {
            max_entries: 4_000_001,
            ..defaults
        },
        RevisionLimitConfig {
            max_retained_bytes: 0,
            ..defaults
        },
        RevisionLimitConfig {
            max_retained_bytes: 512 * 1024 * 1024 + 1,
            ..defaults
        },
    ] {
        assert_eq!(
            RevisionLimits::validate(invalid).unwrap_err().code(),
            RevisionErrorCode::InvalidLimits
        );
    }
}
