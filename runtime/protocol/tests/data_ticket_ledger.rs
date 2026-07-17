use pdf_rs_protocol::{
    ByteRange, Correlation, DataAttachmentRole, DataPriority, DataSegment, DataTicket,
    DataTicketCommitOutcome, DataTicketLedger, DataTicketTerminalKind, FailDataCommand,
    MAX_DATA_SEGMENT_BYTES, MAX_DATA_TICKET_BYTES, MAX_OUTSTANDING_DATA_TICKETS,
    NEED_DATA_EVENT_RANGES_MAX_COUNT, NeedDataEvent, ProtocolError, ProtocolErrorCode,
    ProvideDataCommand, RequestId, SessionId, SourceDescriptor, SourceFailureCode, SourceIdentity,
    WorkerId,
};

fn source(byte: u8, revision: u64) -> SourceIdentity {
    SourceIdentity {
        stable_id: [byte; 32],
        revision,
    }
}

fn source_descriptor(identity: SourceIdentity, length: Option<u64>) -> SourceDescriptor {
    SourceDescriptor {
        identity,
        length,
        validator: [0x5a; 32],
    }
}

fn register(
    ledger: &mut DataTicketLedger,
    correlation: &Correlation,
    event: &NeedDataEvent,
    length: Option<u64>,
) -> Result<(), ProtocolError> {
    let session = correlation.session.unwrap_or_else(|| SessionId::new(0));
    ledger.bind_session(
        correlation.worker,
        session,
        &source_descriptor(event.source.clone(), length),
    )?;
    ledger.register_need_data(correlation, event)
}

fn request_correlation(worker: u64, session: u64, request: u64) -> Correlation {
    Correlation {
        worker: WorkerId::new(worker),
        session: Some(SessionId::new(session)),
        request: Some(RequestId::new(request)),
        generation: None,
    }
}

fn response_correlation(worker: u64, session: u64) -> Correlation {
    Correlation {
        worker: WorkerId::new(worker),
        session: Some(SessionId::new(session)),
        request: None,
        generation: None,
    }
}

fn need_data(ticket: u64, source: SourceIdentity, ranges: Vec<ByteRange>) -> NeedDataEvent {
    NeedDataEvent {
        ticket: DataTicket::new(ticket),
        source,
        ranges,
        priority: DataPriority::VisiblePage,
        checkpoint: 91,
    }
}

fn segment(start: u64, len: u64, slot: u16) -> DataSegment {
    DataSegment {
        range: ByteRange { start, len },
        slot,
        byte_length: len,
        role: DataAttachmentRole::ImmutableRangeBytes,
    }
}

fn provide(ticket: u64, source: SourceIdentity, segments: Vec<DataSegment>) -> ProvideDataCommand {
    ProvideDataCommand {
        ticket: DataTicket::new(ticket),
        source,
        segments,
    }
}

fn assert_code<T: std::fmt::Debug>(result: Result<T, ProtocolError>, code: ProtocolErrorCode) {
    assert_eq!(result.unwrap_err().code(), code);
}

fn assert_prepare_code<T: std::fmt::Debug>(
    result: Result<T, ProtocolError>,
    code: ProtocolErrorCode,
) {
    assert_eq!(result.unwrap_err().code(), code);
}

#[test]
fn registration_enforces_identity_range_and_capacity_bounds() {
    assert_code(
        DataTicketLedger::new(0).map(|_| ()),
        ProtocolErrorCode::InvalidLimits,
    );
    assert_code(
        DataTicketLedger::new(MAX_OUTSTANDING_DATA_TICKETS + 1).map(|_| ()),
        ProtocolErrorCode::InvalidLimits,
    );

    let correlation = request_correlation(1, 2, 3);
    let mut ledger = DataTicketLedger::new(2).unwrap();
    let exact_limit = need_data(
        10,
        source(7, 8),
        (0..4)
            .map(|index| ByteRange {
                start: index * MAX_DATA_SEGMENT_BYTES,
                len: MAX_DATA_SEGMENT_BYTES,
            })
            .collect(),
    );
    assert_eq!(
        exact_limit
            .ranges
            .iter()
            .map(|range| range.len)
            .sum::<u64>(),
        MAX_DATA_TICKET_BYTES
    );
    register(
        &mut ledger,
        &correlation,
        &exact_limit,
        Some(MAX_DATA_TICKET_BYTES),
    )
    .unwrap();
    assert_eq!(ledger.bound_sessions(), 1);

    let adjacent = need_data(
        11,
        exact_limit.source.clone(),
        vec![
            ByteRange { start: 0, len: 4 },
            ByteRange { start: 4, len: 4 },
        ],
    );
    register(
        &mut ledger,
        &correlation,
        &adjacent,
        Some(MAX_DATA_TICKET_BYTES),
    )
    .unwrap();
    assert_eq!(ledger.len(), ledger.capacity());

    assert_code(
        register(
            &mut ledger,
            &correlation,
            &need_data(12, source(9, 10), vec![ByteRange { start: 20, len: 1 }]),
            None,
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let mut validation = DataTicketLedger::new(1).unwrap();
    let invalid_events = [
        need_data(0, source(1, 1), vec![ByteRange { start: 0, len: 1 }]),
        need_data(1, source(0, 1), vec![ByteRange { start: 0, len: 1 }]),
        need_data(1, source(1, 0), vec![ByteRange { start: 0, len: 1 }]),
    ];
    for event in invalid_events {
        assert_code(
            register(&mut validation, &correlation, &event, None),
            ProtocolErrorCode::InvalidDataTicket,
        );
    }
    let mut zero_checkpoint = need_data(1, source(1, 1), vec![ByteRange { start: 0, len: 1 }]);
    zero_checkpoint.checkpoint = 0;
    assert_code(
        register(&mut validation, &correlation, &zero_checkpoint, None),
        ProtocolErrorCode::InvalidDataTicket,
    );

    for bad_correlation in [
        request_correlation(0, 2, 3),
        request_correlation(1, 0, 3),
        request_correlation(1, 2, 0),
    ] {
        assert_code(
            register(
                &mut validation,
                &bad_correlation,
                &need_data(1, source(1, 1), vec![ByteRange { start: 0, len: 1 }]),
                None,
            ),
            ProtocolErrorCode::InvalidDataTicket,
        );
    }

    let invalid_ranges = [
        Vec::new(),
        vec![ByteRange { start: 0, len: 0 }],
        vec![ByteRange {
            start: 0,
            len: MAX_DATA_SEGMENT_BYTES + 1,
        }],
        vec![
            ByteRange { start: 8, len: 4 },
            ByteRange { start: 10, len: 4 },
        ],
        (0..=NEED_DATA_EVENT_RANGES_MAX_COUNT)
            .map(|index| ByteRange {
                start: u64::try_from(index).unwrap() * 2,
                len: 1,
            })
            .collect(),
        (0..5)
            .map(|index| ByteRange {
                start: index * MAX_DATA_SEGMENT_BYTES,
                len: MAX_DATA_SEGMENT_BYTES,
            })
            .collect(),
    ];
    for ranges in invalid_ranges {
        assert_code(
            register(
                &mut validation,
                &correlation,
                &need_data(1, source(1, 1), ranges),
                None,
            ),
            ProtocolErrorCode::InvalidDataRange,
        );
    }
    assert_code(
        register(
            &mut validation,
            &correlation,
            &need_data(
                1,
                source(1, 1),
                vec![ByteRange {
                    start: u64::MAX,
                    len: 1,
                }],
            ),
            None,
        ),
        ProtocolErrorCode::NumericOverflow,
    );
}

#[test]
fn registration_binds_full_source_descriptor_and_known_length() {
    let identity = source(0x31, 7);
    let event = need_data(19, identity.clone(), vec![ByteRange { start: 8, len: 4 }]);
    let correlation = request_correlation(1, 2, 3);
    let mut ledger = DataTicketLedger::new(1).unwrap();

    assert_code(
        ledger.register_need_data(&correlation, &event),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let exact_source = source_descriptor(identity.clone(), Some(12));
    ledger
        .bind_session(WorkerId::new(1), SessionId::new(2), &exact_source)
        .unwrap();
    ledger
        .bind_session(WorkerId::new(1), SessionId::new(2), &exact_source)
        .unwrap();
    assert_eq!(ledger.bound_sessions(), 1);

    let mismatched_identity = source_descriptor(source(0x32, 7), Some(12));
    assert_code(
        ledger.bind_session(WorkerId::new(1), SessionId::new(2), &mismatched_identity),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let mut changed_validator = exact_source.clone();
    changed_validator.validator = [0x6b; 32];
    assert_code(
        ledger.bind_session(WorkerId::new(1), SessionId::new(2), &changed_validator),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let mut changed_length = exact_source.clone();
    changed_length.length = Some(13);
    assert_code(
        ledger.bind_session(WorkerId::new(1), SessionId::new(2), &changed_length),
        ProtocolErrorCode::InvalidDataTicket,
    );

    assert_code(
        ledger.bind_session(
            WorkerId::new(1),
            SessionId::new(3),
            &source_descriptor(source(0x33, 8), None),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let beyond_length = need_data(20, identity.clone(), vec![ByteRange { start: 9, len: 4 }]);
    assert_code(
        ledger.register_need_data(&correlation, &beyond_length),
        ProtocolErrorCode::InvalidDataRange,
    );
    ledger.register_need_data(&correlation, &event).unwrap();

    let mut zero_validator_ledger = DataTicketLedger::new(1).unwrap();
    let mut zero_validator = source_descriptor(identity, Some(12));
    zero_validator.validator = [0; 32];
    zero_validator_ledger
        .bind_session(WorkerId::new(1), SessionId::new(2), &zero_validator)
        .unwrap();
}

#[test]
fn pending_owns_validated_commands_and_session_source_snapshot() {
    let identity = source(0x41, 9);
    let source_snapshot = source_descriptor(identity.clone(), Some(32));
    let event = need_data(21, identity.clone(), vec![ByteRange { start: 4, len: 8 }]);
    let request = request_correlation(1, 2, 3);
    let response = response_correlation(1, 2);

    let mut provide_ledger = DataTicketLedger::new(1).unwrap();
    {
        let mut caller_source = source_snapshot.clone();
        provide_ledger
            .bind_session(WorkerId::new(1), SessionId::new(2), &caller_source)
            .unwrap();
        caller_source.identity = source(0x42, 10);
        caller_source.length = Some(1);
        caller_source.validator = [0; 32];
        std::hint::black_box(caller_source);
    }
    provide_ledger.register_need_data(&request, &event).unwrap();

    let (provided_pending, expected_provided) = {
        let mut provided = provide(21, identity.clone(), vec![segment(4, 8, 0)]);
        let expected_provided = provided.clone();
        let mut caller_correlation = response.clone();
        let provided_pending = provide_ledger
            .prepare_provide_data(&caller_correlation, &provided)
            .unwrap();
        caller_correlation.worker = WorkerId::new(99);
        caller_correlation.session = Some(SessionId::new(99));
        provided.ticket = DataTicket::new(99);
        provided.source = source(0x43, 11);
        provided.segments.clear();
        std::hint::black_box(caller_correlation);
        std::hint::black_box(provided);
        (provided_pending, expected_provided)
    };

    assert_eq!(
        provided_pending.provided_command(),
        Some(&expected_provided)
    );
    assert_eq!(provided_pending.failed_command(), None);
    assert_eq!(provided_pending.source_descriptor(), &source_snapshot);
    assert_eq!(
        provided_pending.terminal(),
        DataTicketTerminalKind::Provided
    );
    let provided_owner = provided_pending.owner();
    assert_eq!(provided_owner.worker(), WorkerId::new(1));
    assert_eq!(provided_owner.session(), SessionId::new(2));
    assert_eq!(provided_owner.request(), RequestId::new(3));
    assert_eq!(provided_owner.ticket(), DataTicket::new(21));
    assert_eq!(provided_owner.checkpoint(), 91);
    assert_eq!(
        provide_ledger.commit(provided_pending).unwrap(),
        DataTicketCommitOutcome::TicketCompleted {
            owner: provided_owner,
            terminal: DataTicketTerminalKind::Provided,
        }
    );

    let mut fail_ledger = DataTicketLedger::new(1).unwrap();
    fail_ledger
        .bind_session(WorkerId::new(1), SessionId::new(2), &source_snapshot)
        .unwrap();
    fail_ledger.register_need_data(&request, &event).unwrap();
    let (failed_pending, expected_failed) = {
        let mut failed = FailDataCommand {
            ticket: DataTicket::new(21),
            expected: identity,
            observed: None,
            code: SourceFailureCode::Timeout,
            retryable: true,
        };
        let expected_failed = failed.clone();
        let failed_pending = fail_ledger.prepare_fail_data(&response, &failed).unwrap();
        failed.ticket = DataTicket::new(99);
        failed.expected = source(0x44, 12);
        failed.observed = Some(source(0x45, 13));
        failed.code = SourceFailureCode::SourceChanged;
        failed.retryable = false;
        std::hint::black_box(failed);
        (failed_pending, expected_failed)
    };

    assert_eq!(failed_pending.provided_command(), None);
    assert_eq!(failed_pending.failed_command(), Some(&expected_failed));
    assert_eq!(failed_pending.source_descriptor(), &source_snapshot);
    assert_eq!(
        failed_pending.terminal(),
        DataTicketTerminalKind::Failed(SourceFailureCode::Timeout)
    );
    let failed_owner = failed_pending.owner();
    assert_eq!(
        fail_ledger.commit(failed_pending).unwrap(),
        DataTicketCommitOutcome::TicketCompleted {
            owner: failed_owner,
            terminal: DataTicketTerminalKind::Failed(SourceFailureCode::Timeout),
        }
    );
}

#[test]
fn exact_provide_is_two_phase_and_commit_is_entry_cas() {
    let source = source(3, 4);
    let request = request_correlation(1, 2, 8);
    let response = response_correlation(1, 2);
    let mut ledger = DataTicketLedger::new(1).unwrap();
    register(
        &mut ledger,
        &request,
        &need_data(
            5,
            source.clone(),
            vec![
                ByteRange { start: 0, len: 4 },
                ByteRange { start: 10, len: 2 },
            ],
        ),
        None,
    )
    .unwrap();
    let command = provide(5, source, vec![segment(0, 4, 0), segment(10, 2, 1)]);

    {
        let dropped = ledger.prepare_provide_data(&response, &command).unwrap();
        assert_eq!(dropped.request(), RequestId::new(8));
        assert_eq!(dropped.checkpoint(), 91);
        assert_eq!(dropped.terminal(), DataTicketTerminalKind::Provided);
    }

    let winner = ledger.prepare_provide_data(&response, &command).unwrap();
    let stale = ledger.prepare_provide_data(&response, &command).unwrap();
    ledger.commit(winner).unwrap();
    assert!(ledger.is_empty());
    assert_code(ledger.commit(stale), ProtocolErrorCode::InvalidDataTicket);
    assert_prepare_code(
        ledger.prepare_provide_data(&response, &command),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let mut first_ledger = DataTicketLedger::new(1).unwrap();
    let mut second_ledger = DataTicketLedger::new(1).unwrap();
    for target in [&mut first_ledger, &mut second_ledger] {
        register(
            target,
            &request,
            &need_data(
                5,
                command.source.clone(),
                vec![
                    ByteRange { start: 0, len: 4 },
                    ByteRange { start: 10, len: 2 },
                ],
            ),
            None,
        )
        .unwrap();
    }
    let foreign = first_ledger
        .prepare_provide_data(&response, &command)
        .unwrap();
    assert_code(
        second_ledger.commit(foreign),
        ProtocolErrorCode::InvalidDataTicket,
    );
    let second_pending = second_ledger
        .prepare_provide_data(&response, &command)
        .unwrap();
    second_ledger.commit(second_pending).unwrap();
}

#[test]
fn provide_requires_the_exact_requested_partition() {
    let source = source(3, 4);
    let request = request_correlation(1, 2, 8);
    let response = response_correlation(1, 2);
    let mut ledger = DataTicketLedger::new(1).unwrap();
    register(
        &mut ledger,
        &request,
        &need_data(
            5,
            source.clone(),
            vec![
                ByteRange { start: 0, len: 4 },
                ByteRange { start: 10, len: 2 },
            ],
        ),
        None,
    )
    .unwrap();

    let invalid = [
        vec![segment(0, 4, 0)],
        vec![segment(0, 4, 0), segment(10, 2, 1), segment(20, 1, 2)],
        vec![segment(10, 2, 0), segment(0, 4, 1)],
        vec![segment(0, 4, 0), segment(3, 2, 1)],
        vec![segment(0, 4, 0), segment(0, 4, 1)],
    ];
    for segments in invalid {
        assert_prepare_code(
            ledger.prepare_provide_data(&response, &provide(5, source.clone(), segments)),
            ProtocolErrorCode::InvalidDataTicket,
        );
    }

    let mut one_less_range = segment(0, 4, 0);
    one_less_range.range.len -= 1;
    assert_prepare_code(
        ledger.prepare_provide_data(
            &response,
            &provide(5, source.clone(), vec![one_less_range, segment(10, 2, 1)]),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let mut one_less_bytes = segment(0, 4, 0);
    one_less_bytes.byte_length -= 1;
    assert_prepare_code(
        ledger.prepare_provide_data(
            &response,
            &provide(5, source, vec![one_less_bytes, segment(10, 2, 1)]),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );
}

#[test]
fn unsolicited_source_session_and_lifecycle_staleness_are_rejected() {
    let expected = source(4, 7);
    let command = provide(9, expected.clone(), vec![segment(0, 4, 0)]);
    let mut ledger = DataTicketLedger::new(2).unwrap();
    assert_prepare_code(
        ledger.prepare_provide_data(&response_correlation(1, 2), &command),
        ProtocolErrorCode::InvalidDataTicket,
    );

    register(
        &mut ledger,
        &request_correlation(1, 2, 10),
        &need_data(9, expected.clone(), vec![ByteRange { start: 0, len: 4 }]),
        None,
    )
    .unwrap();
    register(
        &mut ledger,
        &request_correlation(1, 3, 11),
        &need_data(9, expected.clone(), vec![ByteRange { start: 0, len: 4 }]),
        None,
    )
    .unwrap();
    assert_eq!(ledger.bound_sessions(), 2);

    assert_prepare_code(
        ledger.prepare_provide_data(&response_correlation(1, 4), &command),
        ProtocolErrorCode::InvalidDataTicket,
    );
    assert_prepare_code(
        ledger.prepare_provide_data(
            &response_correlation(1, 2),
            &provide(9, source(5, 7), vec![segment(0, 4, 0)]),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let stale = ledger
        .prepare_provide_data(&response_correlation(1, 2), &command)
        .unwrap();
    assert_eq!(
        ledger.invalidate_session(WorkerId::new(1), SessionId::new(2)),
        1
    );
    assert_eq!(ledger.bound_sessions(), 1);
    register(
        &mut ledger,
        &request_correlation(1, 2, 12),
        &need_data(9, expected.clone(), vec![ByteRange { start: 0, len: 4 }]),
        None,
    )
    .unwrap();
    assert_eq!(ledger.bound_sessions(), 2);
    assert_code(ledger.commit(stale), ProtocolErrorCode::InvalidDataTicket);
    let replacement = ledger
        .prepare_provide_data(&response_correlation(1, 2), &command)
        .unwrap();
    ledger.commit(replacement).unwrap();

    let isolated = ledger
        .prepare_provide_data(&response_correlation(1, 3), &command)
        .unwrap();
    ledger.commit(isolated).unwrap();
    assert!(ledger.is_empty());
    assert_eq!(ledger.invalidate_worker(WorkerId::new(1)), 0);
    assert_eq!(ledger.bound_sessions(), 0);
}

#[test]
fn fail_data_enforces_source_changed_matrix_and_exact_expected_source() {
    let expected = source(4, 7);
    let request = request_correlation(1, 2, 10);
    let response = response_correlation(1, 2);
    let mut ledger = DataTicketLedger::new(1).unwrap();
    register(
        &mut ledger,
        &request,
        &need_data(9, expected.clone(), vec![ByteRange { start: 0, len: 4 }]),
        None,
    )
    .unwrap();

    let failure = |code, observed, retryable| FailDataCommand {
        ticket: DataTicket::new(9),
        expected: expected.clone(),
        observed,
        code,
        retryable,
    };
    let invalid = [
        failure(SourceFailureCode::SourceChanged, None, false),
        failure(
            SourceFailureCode::SourceChanged,
            Some(expected.clone()),
            false,
        ),
        failure(SourceFailureCode::SourceChanged, Some(source(5, 8)), true),
        failure(SourceFailureCode::Timeout, Some(source(5, 8)), true),
    ];
    for command in invalid {
        assert_prepare_code(
            ledger.prepare_fail_data(&response, &command),
            ProtocolErrorCode::InvalidSourceFailure,
        );
    }

    let retryable = failure(SourceFailureCode::Timeout, None, true);
    {
        let dropped = ledger.prepare_fail_data(&response, &retryable).unwrap();
        assert_eq!(
            dropped.terminal(),
            DataTicketTerminalKind::Failed(SourceFailureCode::Timeout)
        );
    }

    let mut wrong_expected = retryable;
    wrong_expected.expected = source(6, 7);
    assert_prepare_code(
        ledger.prepare_fail_data(&response, &wrong_expected),
        ProtocolErrorCode::InvalidDataTicket,
    );

    let source_changed = failure(SourceFailureCode::SourceChanged, Some(source(5, 8)), false);
    let wrong_commit_path = ledger
        .prepare_fail_data(&response, &source_changed)
        .unwrap();
    assert_code(
        ledger.commit(wrong_commit_path),
        ProtocolErrorCode::InvalidSourceFailure,
    );
    let terminal = ledger
        .prepare_fail_data(&response, &source_changed)
        .unwrap();
    let owner = terminal.owner();
    assert_eq!(
        ledger.commit_source_changed(terminal).unwrap(),
        DataTicketCommitOutcome::SessionSourceChanged {
            owner,
            invalidated_tickets: 1,
        }
    );
    assert!(ledger.is_empty());
    assert_eq!(ledger.bound_sessions(), 1);
    assert_code(
        ledger.bind_session(
            WorkerId::new(1),
            SessionId::new(2),
            &source_descriptor(expected.clone(), None),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );
    assert_prepare_code(
        ledger.prepare_fail_data(&response, &source_changed),
        ProtocolErrorCode::InvalidDataTicket,
    );
}

#[test]
fn observed_source_change_wins_both_data_commit_orders_and_poisons_session() {
    let identity = source(0x51, 12);
    let descriptor = source_descriptor(identity.clone(), Some(64));
    let request_one = request_correlation(1, 2, 31);
    let request_two = request_correlation(1, 2, 32);
    let response = response_correlation(1, 2);
    let data_command = provide(41, identity.clone(), vec![segment(0, 4, 0)]);
    let source_changed = FailDataCommand {
        ticket: DataTicket::new(42),
        expected: identity.clone(),
        observed: Some(source(0x52, 13)),
        code: SourceFailureCode::SourceChanged,
        retryable: false,
    };

    let setup = || {
        let mut ledger = DataTicketLedger::new(2).unwrap();
        ledger
            .bind_session(WorkerId::new(1), SessionId::new(2), &descriptor)
            .unwrap();
        ledger
            .register_need_data(
                &request_one,
                &need_data(41, identity.clone(), vec![ByteRange { start: 0, len: 4 }]),
            )
            .unwrap();
        ledger
            .register_need_data(
                &request_two,
                &need_data(42, identity.clone(), vec![ByteRange { start: 8, len: 4 }]),
            )
            .unwrap();
        ledger
    };

    let mut source_change_first = setup();
    let data_pending = source_change_first
        .prepare_provide_data(&response, &data_command)
        .unwrap();
    let changed_pending = source_change_first
        .prepare_fail_data(&response, &source_changed)
        .unwrap();
    let changed_owner = changed_pending.owner();
    assert_prepare_code(
        source_change_first.prepare_provide_data(&response, &data_command),
        ProtocolErrorCode::InvalidDataTicket,
    );
    assert_eq!(
        source_change_first
            .commit_source_changed(changed_pending)
            .unwrap(),
        DataTicketCommitOutcome::SessionSourceChanged {
            owner: changed_owner,
            invalidated_tickets: 2,
        }
    );
    assert_code(
        source_change_first.commit(data_pending),
        ProtocolErrorCode::InvalidDataTicket,
    );
    assert!(source_change_first.is_empty());
    assert_eq!(source_change_first.bound_sessions(), 1);

    let mut data_commit_attempt_first = setup();
    let data_pending = data_commit_attempt_first
        .prepare_provide_data(&response, &data_command)
        .unwrap();
    let changed_pending = data_commit_attempt_first
        .prepare_fail_data(&response, &source_changed)
        .unwrap();
    assert_code(
        data_commit_attempt_first.commit(data_pending),
        ProtocolErrorCode::InvalidDataTicket,
    );
    let changed_owner = changed_pending.owner();
    assert_eq!(
        data_commit_attempt_first
            .commit_source_changed(changed_pending)
            .unwrap(),
        DataTicketCommitOutcome::SessionSourceChanged {
            owner: changed_owner,
            invalidated_tickets: 2,
        }
    );
    assert!(data_commit_attempt_first.is_empty());
    assert_eq!(data_commit_attempt_first.bound_sessions(), 1);
    assert_code(
        data_commit_attempt_first.register_need_data(
            &request_one,
            &need_data(43, identity.clone(), vec![ByteRange { start: 16, len: 4 }]),
        ),
        ProtocolErrorCode::InvalidDataTicket,
    );

    assert_eq!(
        data_commit_attempt_first.invalidate_session(WorkerId::new(1), SessionId::new(2)),
        0
    );
    assert_eq!(data_commit_attempt_first.bound_sessions(), 0);
}

#[test]
fn debug_output_redacts_ticket_contents_and_source_identity() {
    let mut ledger = DataTicketLedger::new(1).unwrap();
    let source = source(0xab, 7);
    register(
        &mut ledger,
        &request_correlation(1, 2, 3),
        &need_data(
            9,
            source.clone(),
            vec![ByteRange {
                start: 0xfeed,
                len: 4,
            }],
        ),
        None,
    )
    .unwrap();
    let pending = ledger
        .prepare_provide_data(
            &response_correlation(1, 2),
            &provide(9, source, vec![segment(0xfeed, 4, 0)]),
        )
        .unwrap();

    let ledger_debug = format!("{ledger:?}");
    let pending_debug = format!("{pending:?}");
    let owner_debug = format!("{:?}", pending.owner());
    assert!(ledger_debug.contains("[REDACTED]"));
    assert!(pending_debug.contains("[REDACTED]"));
    assert!(owner_debug.contains("[REDACTED]"));
    assert!(!ledger_debug.contains("abababab"));
    assert!(!ledger_debug.contains("feed"));
    assert!(!pending_debug.contains("abababab"));
    assert!(!pending_debug.contains("feed"));
    assert!(!owner_debug.contains("feed"));
}
