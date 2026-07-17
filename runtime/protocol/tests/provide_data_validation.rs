use pdf_rs_protocol::{
    ByteRange, Correlation, DataSegment, DataTicket, ProtocolErrorCode, ProtocolLimits,
    ProtocolValidator, ProvideDataCommand, SessionId, SourceIdentity, WorkerId,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);

fn correlation() -> Correlation {
    Correlation {
        worker: WORKER,
        session: Some(SESSION),
        request: None,
        generation: None,
    }
}

fn segment(start: u64, len: u64, slot: u16) -> DataSegment {
    DataSegment {
        range: ByteRange { start, len },
        slot,
        byte_length: len,
    }
}

fn command(segments: Vec<DataSegment>) -> ProvideDataCommand {
    ProvideDataCommand {
        ticket: DataTicket::new(13),
        source: SourceIdentity {
            stable_id: [0xa5; 32],
            revision: 17,
        },
        segments,
    }
}

#[test]
fn exact_nonempty_checked_ranges_bind_each_actual_transfer_once() {
    ProtocolValidator::new(ProtocolLimits::default())
        .validate_provide_data(
            &correlation(),
            &command(vec![segment(0, 4, 0), segment(9, 3, 1)]),
            WORKER,
            SESSION,
            &[4, 3],
        )
        .unwrap();
}

#[test]
fn correlation_and_transfer_count_fail_before_segment_use() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let value = command(vec![segment(0, 4, 0)]);
    let mut wrong_owner = correlation();
    wrong_owner.session = Some(SessionId::new(99));
    assert_eq!(
        validator
            .validate_provide_data(&wrong_owner, &value, WORKER, SESSION, &[4])
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );

    for (value, lengths) in [
        (command(Vec::new()), Vec::new()),
        (command(vec![segment(0, 4, 0)]), Vec::new()),
        (
            command(
                (0_u16..17)
                    .map(|slot| segment(u64::from(slot), 1, slot))
                    .collect(),
            ),
            vec![1; 17],
        ),
    ] {
        assert_eq!(
            validator
                .validate_provide_data(&correlation(), &value, WORKER, SESSION, &lengths)
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidTransferCount
        );
    }
}

#[test]
fn zero_overflow_and_declared_length_mismatch_are_distinct() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let mut zero = segment(0, 0, 0);
    zero.byte_length = 0;
    let mut mismatch = segment(0, 4, 0);
    mismatch.byte_length = 3;

    for value in [zero, mismatch] {
        assert_eq!(
            validator
                .validate_provide_data(
                    &correlation(),
                    &command(vec![value]),
                    WORKER,
                    SESSION,
                    &[3],
                )
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidDataRange
        );
    }

    assert_eq!(
        validator
            .validate_provide_data(
                &correlation(),
                &command(vec![segment(u64::MAX, 1, 0)]),
                WORKER,
                SESSION,
                &[1],
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::NumericOverflow
    );
}

#[test]
fn duplicate_missing_and_wrong_length_transfer_slots_fail_closed() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    for (segments, lengths) in [
        (vec![segment(0, 1, 0), segment(1, 1, 0)], vec![1, 1]),
        (vec![segment(0, 1, 1)], vec![1]),
        (vec![segment(0, 4, 0)], vec![3]),
    ] {
        let error = validator
            .validate_provide_data(
                &correlation(),
                &command(segments),
                WORKER,
                SESSION,
                &lengths,
            )
            .unwrap_err();
        assert_eq!(error.code(), ProtocolErrorCode::InvalidTransferBinding);
        assert_eq!(error.diagnostic_id(), "RPE-PROTOCOL-0032");
    }
}
