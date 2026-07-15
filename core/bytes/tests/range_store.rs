use std::sync::{Arc, Barrier};

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, RangeStoreLimitConfig,
    RangeStoreLimits, ReadPoll, ReadRequest, RequestPriority, ResumeCheckpoint, SourceError,
    SourceErrorCategory, SourceErrorCode, SourceIdentity, SourceLimitKind, SourceRecoverability,
    SourceRevision, SourceSnapshot, SourceStableId, SourceValidator, SourceValidatorKind,
    TicketStatus,
};

fn snapshot(len: Option<u64>) -> SourceSnapshot {
    snapshot_with(1, 1, 2, len)
}

fn snapshot_with(
    stable_byte: u8,
    revision: u64,
    validator_byte: u8,
    len: Option<u64>,
) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([stable_byte; 32]),
            SourceRevision::new(revision),
        ),
        len,
        SourceValidator::new(SourceValidatorKind::StrongEntityTag, [validator_byte; 32]),
    )
}

fn limits() -> RangeStoreLimits {
    RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 128,
        max_read_bytes: 64,
        max_cached_bytes: 128,
        max_resident_bytes: 256,
        max_segments: 16,
        max_tickets: 16,
        max_subscribers_per_ticket: 4,
        max_total_subscriptions: 16,
        max_missing_ranges: 16,
    })
    .expect("test limits are valid")
}

fn request(start: u64, len: u64, job: u64, checkpoint: u64) -> ReadRequest {
    ReadRequest::new(
        ByteRange::new(start, len).expect("test range is valid"),
        RequestPriority::VisiblePage,
        JobId::new(job),
        ResumeCheckpoint::new(checkpoint),
    )
}

fn response(
    observed: SourceSnapshot,
    start: u64,
    bytes: &[u8],
) -> Result<RangeResponse, SourceError> {
    RangeResponse::new(
        observed,
        ByteRange::new(
            start,
            u64::try_from(bytes.len()).expect("test byte count fits u64"),
        )?,
        bytes.to_vec(),
    )
}

#[test]
fn ranges_are_non_empty_checked_and_priority_is_explicit() {
    let range = ByteRange::new(u64::MAX - 3, 3).expect("exclusive end may equal u64::MAX");
    assert_eq!(range.start(), u64::MAX - 3);
    assert_eq!(range.end_exclusive(), u64::MAX);
    assert!(!range.is_empty());

    for error in [
        ByteRange::new(0, 0).expect_err("zero length must fail"),
        ByteRange::new(u64::MAX, 1).expect_err("exclusive end overflow must fail"),
    ] {
        assert_eq!(error.code(), SourceErrorCode::InvalidRange);
        assert_eq!(error.category(), SourceErrorCategory::Input);
        assert_eq!(error.recoverability(), SourceRecoverability::CorrectInput);
        assert_eq!(error.diagnostic_id(), "RPE-BYTES-0001");
    }

    assert!(RequestPriority::VisiblePage.rank() > RequestPriority::BackgroundPrefetch.rank());
}

#[test]
fn responses_validate_geometry_and_redact_source_bytes() {
    let observed = snapshot(Some(4));
    let range = ByteRange::new(0, 4).expect("range is valid");
    let length_error =
        RangeResponse::new(observed, range, vec![1, 2]).expect_err("length must match");
    assert_eq!(length_error.code(), SourceErrorCode::ResponseLengthMismatch);

    let out_of_bounds = response(observed, 3, &[1, 2]).expect_err("known end must be enforced");
    assert_eq!(out_of_bounds.code(), SourceErrorCode::ResponseOutOfBounds);

    let debug = format!(
        "{:?}",
        response(observed, 0, &[17, 23, 42, 99]).expect("response is valid")
    );
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("17"));
    assert!(!debug.contains("23"));
}

#[test]
fn response_capacity_is_charged_before_resident_adoption() {
    let bounded = RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 4,
        max_read_bytes: 4,
        max_cached_bytes: 4,
        max_resident_bytes: 8,
        max_segments: 2,
        max_tickets: 2,
        max_subscribers_per_ticket: 2,
        max_total_subscriptions: 2,
        max_missing_ranges: 2,
    })
    .unwrap();
    let observed = snapshot(Some(4));
    let store = RangeStore::new(observed, bounded).unwrap();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(b"ABCD");
    let large_capacity_response =
        RangeResponse::new(observed, ByteRange::new(0, 4).unwrap(), bytes).unwrap();

    let error = store
        .supply(large_capacity_response)
        .expect_err("excess response capacity participates in the resident budget");
    let limit = error.limit().expect("resident failure has context");
    assert_eq!(limit.kind(), SourceLimitKind::ResidentBytes);
    assert_eq!(limit.limit(), 8);
    assert!(limit.attempted() >= 32);
    assert_eq!(store.cached_bytes().unwrap(), 0);
    assert_eq!(store.resident_bytes(), 0);

    store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .expect("an exact response fits the resident budget");
    assert_eq!(store.cached_bytes().unwrap(), 4);
    assert_eq!(store.resident_bytes(), 4);
}

#[test]
fn out_of_order_responses_complete_once_and_return_stable_bytes() {
    let observed = snapshot(Some(8));
    let store = RangeStore::new(observed, limits()).expect("store is valid");
    let pending = store.poll(request(0, 8, 7, 11));
    let (ticket, missing) = match pending {
        ReadPoll::Pending { ticket, missing } => (ticket, missing),
        other => panic!("expected pending read, got {other:?}"),
    };
    assert_eq!(missing.as_slice(), &[ByteRange::new(0, 8).unwrap()]);

    let upper = store
        .supply(response(observed, 4, b"EFGH").unwrap())
        .expect("upper half is accepted");
    assert!(upper.ready_tickets().is_empty());
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 1
        }
    );

    let lower = store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .expect("lower half is accepted");
    assert_eq!(lower.ready_tickets(), &[ticket]);
    assert_eq!(lower.cached_bytes(), 8);
    assert_eq!(store.ticket_status(ticket).unwrap(), TicketStatus::Ready);

    let slice = match store.poll(request(0, 8, 7, 11)) {
        ReadPoll::Ready(slice) => slice,
        other => panic!("expected ready bytes, got {other:?}"),
    };
    assert_eq!(slice.identity(), observed.identity());
    assert_eq!(slice.range(), ByteRange::new(0, 8).unwrap());
    assert_eq!(slice.bytes(), b"ABCDEFGH");
    let cloned = slice.clone();

    let subscriptions = store.take_subscriptions(ticket).unwrap();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].job(), JobId::new(7));
    assert_eq!(subscriptions[0].checkpoint(), ResumeCheckpoint::new(11));
    assert!(store.take_subscriptions(ticket).unwrap().is_empty());
    store.release_ticket(ticket).unwrap();
    drop(store);
    assert_eq!(cloned.bytes(), b"ABCDEFGH");
}

#[test]
fn missing_ranges_are_sorted_disjoint_and_minimal() {
    let observed = snapshot(Some(12));
    let store = RangeStore::new(observed, limits()).unwrap();
    store.supply(response(observed, 0, b"AB").unwrap()).unwrap();
    store.supply(response(observed, 4, b"EF").unwrap()).unwrap();
    store
        .supply(response(observed, 9, b"JKL").unwrap())
        .unwrap();

    let missing = match store.poll(request(0, 12, 1, 1)) {
        ReadPoll::Pending { missing, .. } => missing,
        other => panic!("expected holes, got {other:?}"),
    };
    assert_eq!(
        missing.as_slice(),
        &[ByteRange::new(2, 2).unwrap(), ByteRange::new(6, 3).unwrap(),]
    );
}

#[test]
fn identical_overlap_is_idempotent_but_conflict_poisons_store() {
    let observed = snapshot(Some(8));
    let store = RangeStore::new(observed, limits()).unwrap();
    store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    store
        .supply(response(observed, 1, b"BC").unwrap())
        .expect("identical overlap is accepted");
    assert_eq!(store.cached_bytes().unwrap(), 4);

    let ticket = match store.poll(request(4, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending tail, got {other:?}"),
    };
    let conflict = store
        .supply(response(observed, 2, b"XY").unwrap())
        .expect_err("different overlap must fail integrity");
    assert_eq!(conflict.code(), SourceErrorCode::ConflictingBytes);
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::SourceChanged
    );
    assert!(matches!(
        store.poll(request(0, 1, 9, 9)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
}

#[test]
fn unknown_length_binds_once_and_metadata_drift_is_terminal() {
    let initial = snapshot(None);
    let observed_len_eight = snapshot(Some(8));
    let store = RangeStore::new(initial, limits()).unwrap();
    store
        .supply(response(observed_len_eight, 0, b"ABCD").unwrap())
        .expect("first known total length binds");
    let ticket = match store.poll(request(4, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending tail, got {other:?}"),
    };

    let changed_length = snapshot(Some(9));
    let error = store
        .supply(response(changed_length, 4, b"EFGH").unwrap())
        .expect_err("total length may not drift");
    assert_eq!(error.code(), SourceErrorCode::SourceChanged);
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::SourceChanged
    );

    for changed in [
        snapshot_with(9, 1, 2, Some(8)),
        snapshot_with(1, 2, 2, Some(8)),
        snapshot_with(1, 1, 9, Some(8)),
    ] {
        let candidate = RangeStore::new(observed_len_eight, limits()).unwrap();
        let mismatch = candidate
            .supply(response(changed, 0, b"ABCD").unwrap())
            .expect_err("identity, revision, and validator are immutable");
        assert_eq!(mismatch.code(), SourceErrorCode::SourceChanged);
    }
}

#[test]
fn newly_observed_eof_wakes_crossing_reads() {
    let initial = snapshot(None);
    let observed_len_four = snapshot(Some(4));
    let store = RangeStore::new(initial, limits()).unwrap();
    let ticket = match store.poll(request(0, 8, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending unknown-length read, got {other:?}"),
    };

    let outcome = store
        .supply(response(observed_len_four, 0, b"ABCD").unwrap())
        .expect("the first known length binds with the response");
    assert_eq!(outcome.ready_tickets(), &[ticket]);
    assert_eq!(store.ticket_status(ticket).unwrap(), TicketStatus::Ready);
    assert!(matches!(
        store.poll(request(0, 8, 1, 1)),
        ReadPoll::EndOfFile
    ));
    assert_eq!(store.take_subscriptions(ticket).unwrap().len(), 1);
    store.release_ticket(ticket).unwrap();
}

#[test]
fn metadata_only_observation_binds_empty_source_and_preserves_in_range_waits() {
    let initial = snapshot(None);
    let empty_store = RangeStore::new(initial, limits()).unwrap();
    let empty_ticket = match empty_store.poll(request(0, 1, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending empty-source probe, got {other:?}"),
    };
    let empty_outcome = empty_store
        .observe_snapshot(snapshot(Some(0)))
        .expect("zero length can bind without a non-empty response");
    assert_eq!(empty_outcome.ready_tickets(), &[empty_ticket]);
    assert_eq!(
        empty_store.ticket_status(empty_ticket).unwrap(),
        TicketStatus::Ready
    );
    assert!(matches!(
        empty_store.poll(request(0, 1, 1, 1)),
        ReadPoll::EndOfFile
    ));
    assert_eq!(empty_store.cached_bytes().unwrap(), 0);
    assert_eq!(empty_store.resident_bytes(), 0);

    let bounded_store = RangeStore::new(initial, limits()).unwrap();
    let in_range_ticket = match bounded_store.poll(request(0, 4, 2, 2)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending in-range read, got {other:?}"),
    };
    let bounded_outcome = bounded_store
        .observe_snapshot(snapshot(Some(4)))
        .expect("known nonzero length binds without bytes");
    assert!(bounded_outcome.ready_tickets().is_empty());
    assert_eq!(
        bounded_store.ticket_status(in_range_ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 1
        }
    );
}

#[test]
fn first_known_length_rejects_historical_bytes_beyond_end() {
    let initial = snapshot(None);
    let store = RangeStore::new(initial, limits()).unwrap();
    store
        .supply(response(initial, 8, b"IJKL").unwrap())
        .expect("unknown-length source may initially provide a high range");
    assert_eq!(store.cached_bytes().unwrap(), 4);

    let observed_len_eight = snapshot(Some(8));
    let error = store
        .supply(response(observed_len_eight, 0, b"ABCD").unwrap())
        .expect_err("known total length cannot contradict retained bytes");
    assert_eq!(error.code(), SourceErrorCode::SourceChanged);
    assert!(matches!(
        store.poll(request(0, 1, 1, 1)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
}

#[test]
fn tickets_deduplicate_subscribers_and_abandon_after_last_job() {
    let observed = snapshot(Some(4));
    let store = RangeStore::new(observed, limits()).unwrap();
    let first = store.poll(request(0, 4, 1, 10));
    let ticket = match first {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected first ticket, got {other:?}"),
    };
    let second_ticket = match store.poll(request(0, 4, 2, 20)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected shared ticket, got {other:?}"),
    };
    assert_eq!(ticket, second_ticket);
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 2
        }
    );

    let duplicate = store.poll(request(0, 4, 1, 10));
    assert!(matches!(duplicate, ReadPoll::Pending { ticket: same, .. } if same == ticket));
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 2
        }
    );
    assert!(matches!(
        store.poll(request(0, 4, 1, 11)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::CheckpointConflict
    ));

    store.unsubscribe(ticket, JobId::new(1)).unwrap();
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 1
        }
    );
    store.unsubscribe(ticket, JobId::new(2)).unwrap();
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Abandoned
    );

    let outcome = store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    assert!(outcome.ready_tickets().is_empty());
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Abandoned
    );
    assert!(store.take_subscriptions(ticket).unwrap().is_empty());
}

#[test]
fn partial_supply_updates_ticket_missing_ranges_without_duplication() {
    let observed = snapshot(Some(8));
    let store = RangeStore::new(observed, limits()).unwrap();
    let ticket = match store.poll(request(0, 8, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected whole-range ticket, got {other:?}"),
    };
    let partial = store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    assert!(partial.ready_tickets().is_empty());

    let (same_ticket, missing) = match store.poll(request(0, 8, 2, 2)) {
        ReadPoll::Pending { ticket, missing } => (ticket, missing),
        other => panic!("expected remaining-range ticket, got {other:?}"),
    };
    assert_eq!(same_ticket, ticket);
    assert_eq!(missing.as_slice(), &[ByteRange::new(4, 4).unwrap()]);
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 2
        }
    );

    let completed = store
        .supply(response(observed, 4, b"EFGH").unwrap())
        .unwrap();
    assert_eq!(completed.ready_tickets(), &[ticket]);
}

#[test]
fn ticket_failures_have_one_terminal_transition() {
    let observed = snapshot(Some(4));
    let store = RangeStore::new(observed, limits()).unwrap();
    let ticket = match store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending ticket, got {other:?}"),
    };
    let failure = SourceError::source_unavailable();
    store.fail_ticket(ticket, failure).unwrap();
    assert_eq!(
        store.ticket_status(ticket).unwrap(),
        TicketStatus::Failed(failure)
    );
    assert_eq!(
        store.fail_ticket(ticket, failure).unwrap_err().code(),
        SourceErrorCode::TicketAlreadyTerminal
    );
    assert_eq!(
        store.release_ticket(ticket).unwrap_err().code(),
        SourceErrorCode::SubscriptionsNotTaken
    );
    assert_eq!(store.take_subscriptions(ticket).unwrap().len(), 1);
    store.release_ticket(ticket).unwrap();
    assert_eq!(
        store.ticket_status(ticket).unwrap_err().code(),
        SourceErrorCode::UnknownTicket
    );
}

#[test]
fn ticket_identity_is_bound_to_one_range_store_namespace() {
    let observed = snapshot(Some(4));
    let first_store = RangeStore::new(observed, limits()).unwrap();
    let second_store = RangeStore::new(observed, limits()).unwrap();
    let first = match first_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected first pending ticket, got {other:?}"),
    };
    let second = match second_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected second pending ticket, got {other:?}"),
    };

    assert_eq!(first.value(), 1);
    assert_eq!(second.value(), 1);
    assert_ne!(first, second);
    assert_eq!(
        first_store.ticket_status(second).unwrap_err().code(),
        SourceErrorCode::UnknownTicket
    );
    assert_eq!(
        first_store
            .fail_ticket(second, SourceError::source_unavailable())
            .unwrap_err()
            .code(),
        SourceErrorCode::UnknownTicket
    );
    assert_eq!(
        first_store.ticket_status(first).unwrap(),
        TicketStatus::Pending {
            subscriber_count: 1
        }
    );
}

#[test]
fn integrity_ticket_failure_poisons_every_pending_read() {
    let observed = snapshot(Some(8));
    let store = RangeStore::new(observed, limits()).unwrap();
    let first = match store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected first pending ticket, got {other:?}"),
    };
    let second = match store.poll(request(4, 4, 2, 2)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected second pending ticket, got {other:?}"),
    };

    let mismatch_store = RangeStore::new(observed, limits()).unwrap();
    let source_changed = mismatch_store
        .supply(response(snapshot_with(9, 1, 2, Some(8)), 0, b"ABCD").unwrap())
        .expect_err("mismatched source produces an integrity failure");
    store
        .fail_ticket(first, source_changed)
        .expect("integrity failure becomes a global terminal transition");
    assert_eq!(
        store.ticket_status(first).unwrap(),
        TicketStatus::SourceChanged
    );
    assert_eq!(
        store.ticket_status(second).unwrap(),
        TicketStatus::SourceChanged
    );
    assert!(matches!(
        store.poll(request(0, 1, 3, 3)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
}

#[test]
fn known_end_and_empty_snapshot_return_end_of_file() {
    let observed = snapshot(Some(4));
    let store = RangeStore::new(observed, limits()).unwrap();
    assert!(matches!(
        store.poll(request(4, 1, 1, 1)),
        ReadPoll::EndOfFile
    ));
    assert!(matches!(
        store.poll(request(3, 2, 1, 1)),
        ReadPoll::EndOfFile
    ));

    let empty = RangeStore::new(snapshot(Some(0)), limits()).unwrap();
    assert!(matches!(
        empty.poll(request(0, 1, 1, 1)),
        ReadPoll::EndOfFile
    ));
}

#[test]
fn configured_read_cache_ticket_and_subscription_limits_are_enforced() {
    let constrained = RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 8,
        max_read_bytes: 4,
        max_cached_bytes: 4,
        max_resident_bytes: 12,
        max_segments: 1,
        max_tickets: 1,
        max_subscribers_per_ticket: 1,
        max_total_subscriptions: 1,
        max_missing_ranges: 1,
    })
    .unwrap();
    let observed = snapshot(Some(8));
    let store = RangeStore::new(observed, constrained).unwrap();
    assert!(matches!(
        store.poll(request(0, 5, 1, 1)),
        ReadPoll::Failed(error)
            if error.code() == SourceErrorCode::ResourceLimit
                && error.limit().is_some_and(|limit|
                    limit.kind() == SourceLimitKind::ReadBytes
                        && limit.limit() == 4
                        && limit.attempted() == 5)
    ));

    let ticket = match store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected bounded ticket, got {other:?}"),
    };
    assert!(matches!(
        store.poll(request(0, 4, 2, 2)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::ResourceLimit
    ));
    assert!(matches!(
        store.poll(request(4, 4, 3, 3)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::ResourceLimit
    ));
    assert_eq!(
        store
            .supply(response(observed, 0, b"ABCDE").unwrap())
            .expect_err("cache ceiling applies before insertion")
            .code(),
        SourceErrorCode::ResourceLimit
    );
    store.unsubscribe(ticket, JobId::new(1)).unwrap();
    store.release_ticket(ticket).unwrap();
    store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    let cache_error = store
        .supply(response(observed, 4, b"EFGH").unwrap())
        .expect_err("cache byte ceiling must hold");
    assert_eq!(cache_error.code(), SourceErrorCode::ResourceLimit);
    assert!(
        cache_error
            .limit()
            .is_some_and(|limit| limit.kind() == SourceLimitKind::ResidentBytes
                || limit.kind() == SourceLimitKind::CachedBytes)
    );
    assert_eq!(store.cached_bytes().unwrap(), 4);
}

#[test]
fn shared_slices_are_zero_copy_and_old_backings_remain_resident_until_drop() {
    let observed = snapshot(Some(12));
    let bounded = RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 12,
        max_read_bytes: 4,
        max_cached_bytes: 12,
        max_resident_bytes: 24,
        max_segments: 4,
        max_tickets: 4,
        max_subscribers_per_ticket: 2,
        max_total_subscriptions: 4,
        max_missing_ranges: 4,
    })
    .unwrap();
    let store = RangeStore::new(observed, bounded).unwrap();
    store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    assert_eq!(store.resident_bytes(), 4);

    let retained = match store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Ready(slice) => slice,
        other => panic!("expected retained first slice, got {other:?}"),
    };
    let duplicate = match store.poll(request(0, 4, 2, 2)) {
        ReadPoll::Ready(slice) => slice,
        other => panic!("expected duplicate shared slice, got {other:?}"),
    };
    assert_eq!(store.resident_bytes(), 4);
    drop(duplicate);

    store
        .supply(response(observed, 4, b"EFGH").unwrap())
        .expect("first coalescing peak fits exactly");
    assert_eq!(store.cached_bytes().unwrap(), 8);
    assert_eq!(store.resident_bytes(), 12);

    let resident_error = store
        .supply(response(observed, 8, b"IJKL").unwrap())
        .expect_err("retained old backing participates in peak accounting");
    let limit = resident_error.limit().expect("resident error has context");
    assert_eq!(limit.kind(), SourceLimitKind::ResidentBytes);
    assert_eq!(limit.limit(), 24);
    assert_eq!(limit.attempted(), 28);
    assert_eq!(store.cached_bytes().unwrap(), 8);

    drop(retained);
    assert_eq!(store.resident_bytes(), 8);
    store
        .supply(response(observed, 8, b"IJKL").unwrap())
        .expect("reclaimed old backing restores resident budget");
    assert_eq!(store.cached_bytes().unwrap(), 12);
    assert_eq!(store.resident_bytes(), 12);
}

#[test]
fn input_and_response_limits_reject_before_cache_mutation() {
    let bounded = RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 8,
        max_read_bytes: 4,
        max_cached_bytes: 8,
        max_resident_bytes: 16,
        max_segments: 4,
        max_tickets: 4,
        max_subscribers_per_ticket: 2,
        max_total_subscriptions: 4,
        max_missing_ranges: 4,
    })
    .unwrap();
    let known_error = RangeStore::new(snapshot(Some(9)), bounded)
        .expect_err("known input length exceeds profile");
    assert_eq!(
        known_error.limit().expect("input error has context").kind(),
        SourceLimitKind::InputBytes
    );

    let initial = snapshot(None);
    let store = RangeStore::new(initial, bounded).unwrap();
    let observed_too_large = snapshot(Some(9));
    let error = store
        .supply(response(observed_too_large, 0, b"ABCD").unwrap())
        .expect_err("first observed total length is still budgeted");
    let limit = error.limit().expect("input error has context");
    assert_eq!(limit.kind(), SourceLimitKind::InputBytes);
    assert_eq!(limit.limit(), 8);
    assert_eq!(limit.attempted(), 9);
    assert_eq!(store.cached_bytes().unwrap(), 0);

    let response_too_large = store
        .supply(response(initial, 0, b"ABCDE").unwrap())
        .expect_err("supplied ranges use the read ceiling");
    assert_eq!(
        response_too_large
            .limit()
            .expect("read error has context")
            .kind(),
        SourceLimitKind::ReadBytes
    );
    assert_eq!(store.cached_bytes().unwrap(), 0);

    let bound = snapshot(Some(4));
    let drifted = snapshot(Some(9));
    let metadata_store = RangeStore::new(bound, bounded).unwrap();
    let metadata_drift = metadata_store
        .observe_snapshot(drifted)
        .expect_err("bound length drift outranks the input ceiling");
    assert_eq!(metadata_drift.code(), SourceErrorCode::SourceChanged);
    assert!(matches!(
        metadata_store.poll(request(0, 1, 1, 1)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));

    let response_store = RangeStore::new(bound, bounded).unwrap();
    let drifted_response =
        RangeResponse::new(drifted, ByteRange::new(0, 5).unwrap(), b"ABCDE".to_vec()).unwrap();
    let response_drift = response_store
        .supply(drifted_response)
        .expect_err("bound length drift outranks the response read ceiling");
    assert_eq!(response_drift.code(), SourceErrorCode::SourceChanged);
    assert!(matches!(
        response_store.poll(request(0, 1, 1, 1)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
}

#[test]
fn unknown_length_range_end_is_bounded_before_mutation() {
    let bounded = RangeStoreLimits::validate(RangeStoreLimitConfig {
        max_input_bytes: 8,
        max_read_bytes: 4,
        max_cached_bytes: 8,
        max_resident_bytes: 16,
        max_segments: 4,
        max_tickets: 4,
        max_subscribers_per_ticket: 2,
        max_total_subscriptions: 4,
        max_missing_ranges: 4,
    })
    .unwrap();
    let unknown = snapshot(None);
    let polling_store = RangeStore::new(unknown, bounded).unwrap();
    let poll_error = match polling_store.poll(request(8, 1, 1, 1)) {
        ReadPoll::Failed(error) => error,
        other => panic!("expected input limit failure, got {other:?}"),
    };
    let limit = poll_error.limit().expect("input failure has context");
    assert_eq!(limit.kind(), SourceLimitKind::InputBytes);
    assert_eq!(limit.limit(), 8);
    assert_eq!(limit.attempted(), 9);
    let first_ticket = match polling_store.poll(request(4, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("boundary range remains valid, got {other:?}"),
    };
    assert_eq!(first_ticket.value(), 1);

    let supplying_store = RangeStore::new(unknown, bounded).unwrap();
    let high_response =
        RangeResponse::new(unknown, ByteRange::new(8, 1).unwrap(), vec![b'X']).unwrap();
    let supply_error = supplying_store
        .supply(high_response)
        .expect_err("unknown total length does not permit a high offset");
    let limit = supply_error.limit().expect("input failure has context");
    assert_eq!(limit.kind(), SourceLimitKind::InputBytes);
    assert_eq!(limit.limit(), 8);
    assert_eq!(limit.attempted(), 9);
    assert_eq!(supplying_store.cached_bytes().unwrap(), 0);
    assert_eq!(supplying_store.resident_bytes(), 0);
    supplying_store
        .supply(response(unknown, 0, b"ABCD").unwrap())
        .expect("an input limit failure does not poison the snapshot");
    assert_eq!(supplying_store.cached_bytes().unwrap(), 4);
}

#[test]
fn source_change_preserves_committed_ticket_terminal_states() {
    let observed = snapshot(Some(4));

    let ready_store = RangeStore::new(observed, limits()).unwrap();
    let ready_ticket = match ready_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending ready ticket, got {other:?}"),
    };
    let ready = ready_store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .unwrap();
    assert_eq!(ready.ready_tickets(), &[ready_ticket]);
    let mismatch_store = RangeStore::new(observed, limits()).unwrap();
    let source_changed = mismatch_store
        .supply(
            response(snapshot_with(9, 1, 2, Some(4)), 0, b"ABCD")
                .expect("mismatched response geometry remains valid"),
        )
        .expect_err("mismatched identity creates an integrity error");
    ready_store
        .fail_ticket(ready_ticket, source_changed)
        .expect("integrity failure poisons the session after ticket completion");
    assert_eq!(
        ready_store.ticket_status(ready_ticket).unwrap(),
        TicketStatus::Ready
    );
    assert!(matches!(
        ready_store.poll(request(0, 4, 2, 2)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
    assert_eq!(
        ready_store.take_subscriptions(ready_ticket).unwrap().len(),
        1
    );
    assert!(
        ready_store
            .take_subscriptions(ready_ticket)
            .unwrap()
            .is_empty()
    );
    ready_store.release_ticket(ready_ticket).unwrap();

    let failed_store = RangeStore::new(observed, limits()).unwrap();
    let failed_ticket = match failed_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending failure ticket, got {other:?}"),
    };
    let unavailable = SourceError::source_unavailable();
    failed_store
        .fail_ticket(failed_ticket, unavailable)
        .unwrap();
    failed_store.signal_source_changed().unwrap();
    assert_eq!(
        failed_store.ticket_status(failed_ticket).unwrap(),
        TicketStatus::Failed(unavailable)
    );
    assert_eq!(
        failed_store
            .take_subscriptions(failed_ticket)
            .unwrap()
            .len(),
        1
    );
    failed_store.release_ticket(failed_ticket).unwrap();

    let abandoned_store = RangeStore::new(observed, limits()).unwrap();
    let abandoned_ticket = match abandoned_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending abandoned ticket, got {other:?}"),
    };
    abandoned_store
        .unsubscribe(abandoned_ticket, JobId::new(1))
        .unwrap();
    abandoned_store.signal_source_changed().unwrap();
    assert_eq!(
        abandoned_store.ticket_status(abandoned_ticket).unwrap(),
        TicketStatus::Abandoned
    );
    assert!(
        abandoned_store
            .take_subscriptions(abandoned_ticket)
            .unwrap()
            .is_empty()
    );
    abandoned_store.release_ticket(abandoned_ticket).unwrap();

    let pending_store = RangeStore::new(observed, limits()).unwrap();
    let pending_ticket = match pending_store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending source-change ticket, got {other:?}"),
    };
    pending_store.signal_source_changed().unwrap();
    assert_eq!(
        pending_store.ticket_status(pending_ticket).unwrap(),
        TicketStatus::SourceChanged
    );
    let supply_error = pending_store
        .supply(response(observed, 0, b"ABCD").unwrap())
        .expect_err("source change wins before a later supply");
    assert_eq!(supply_error.code(), SourceErrorCode::SourceChanged);
    assert_eq!(pending_store.cached_bytes().unwrap(), 0);
    assert_eq!(pending_store.resident_bytes(), 0);
}

#[test]
fn source_change_and_supply_have_one_linearized_outcome() {
    let observed = snapshot(Some(4));
    let store = Arc::new(RangeStore::new(observed, limits()).unwrap());
    let ticket = match store.poll(request(0, 4, 1, 1)) {
        ReadPoll::Pending { ticket, .. } => ticket,
        other => panic!("expected pending ticket, got {other:?}"),
    };
    let barrier = Arc::new(Barrier::new(3));

    let supplying_store = Arc::clone(&store);
    let supplying_barrier = Arc::clone(&barrier);
    let supply = std::thread::spawn(move || {
        supplying_barrier.wait();
        supplying_store.supply(response(observed, 0, b"ABCD").unwrap())
    });
    let changing_store = Arc::clone(&store);
    let changing_barrier = Arc::clone(&barrier);
    let change = std::thread::spawn(move || {
        changing_barrier.wait();
        changing_store.signal_source_changed()
    });
    barrier.wait();
    let supply_result = supply.join().expect("supply thread does not panic");
    change
        .join()
        .expect("source-change thread does not panic")
        .expect("source change is idempotent");

    assert!(matches!(
        store.poll(request(0, 4, 2, 2)),
        ReadPoll::Failed(error) if error.code() == SourceErrorCode::SourceChanged
    ));
    match supply_result {
        Ok(outcome) => {
            assert_eq!(outcome.ready_tickets(), &[ticket]);
            assert_eq!(store.ticket_status(ticket).unwrap(), TicketStatus::Ready);
        }
        Err(error) => {
            assert_eq!(error.code(), SourceErrorCode::SourceChanged);
            assert_eq!(
                store.ticket_status(ticket).unwrap(),
                TicketStatus::SourceChanged
            );
        }
    }
}

#[test]
fn range_store_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RangeStore>();
}
