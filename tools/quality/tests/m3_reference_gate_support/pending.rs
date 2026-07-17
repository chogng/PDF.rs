use pdf_rs_bytes::{
    ByteRange, DataTicket, JobId, RangeResponse, RangeStore, ResumeCheckpoint, SmallRanges,
    SourceSnapshot,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingEvent {
    pub(super) stage: &'static str,
    pub(super) ordinal: u64,
    pub(super) checkpoint_role: &'static str,
    pub(super) ranges: Vec<(u64, u64)>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "the strict test driver keeps source, ticket, subscription, and audit authority explicit"
)]
pub(super) fn complete_pending(
    stage: &'static str,
    store: &RangeStore,
    snapshot: SourceSnapshot,
    input: &[u8],
    expected_jobs: &[JobId],
    ticket: DataTicket,
    missing: &SmallRanges,
    checkpoint: ResumeCheckpoint,
    trace: &mut Vec<PendingEvent>,
) {
    assert!(
        !missing.is_empty(),
        "{stage}: Pending must name missing ranges"
    );
    assert!(
        trace.len() < 128,
        "{stage}: strict pipeline exceeded the Pending-turn ceiling"
    );

    let ranges = missing
        .as_slice()
        .iter()
        .map(|range| (range.start(), range.len()))
        .collect::<Vec<_>>();
    let event = PendingEvent {
        stage,
        ordinal: u64::try_from(trace.len()).expect("Pending ordinal fits u64"),
        checkpoint_role: checkpoint_role(checkpoint),
        ranges,
    };
    assert!(
        trace.last().is_none_or(|last| {
            last.stage != event.stage
                || last.checkpoint_role != event.checkpoint_role
                || last.ranges != event.ranges
        }),
        "{stage}: a supplied Pending request made no progress"
    );
    trace.push(event);

    let mut early = Vec::new();
    let mut late = Vec::new();
    for range in missing.as_slice() {
        if range.len() == 1 {
            late.push(*range);
            continue;
        }
        let lower_len = range.len() / 2;
        let upper_len = range.len() - lower_len;
        late.push(
            ByteRange::new(range.start(), lower_len)
                .expect("a split lower Pending fragment is non-empty"),
        );
        early.push(
            ByteRange::new(range.start() + lower_len, upper_len)
                .expect("a split upper Pending fragment is non-empty"),
        );
    }

    for fragment in early.into_iter().rev() {
        let outcome = supply_fragment(store, snapshot, input, fragment, stage);
        assert!(
            !outcome.ready_tickets().contains(&ticket),
            "{stage}: a deliberately incomplete upper-half schedule woke the ticket"
        );
    }

    let late_len = late.len();
    for (index, fragment) in late.into_iter().rev().enumerate() {
        let outcome = supply_fragment(store, snapshot, input, fragment, stage);
        if index + 1 == late_len {
            assert_eq!(
                outcome.ready_tickets(),
                &[ticket],
                "{stage}: the final exact fragment must wake only the current ticket"
            );
        } else {
            assert!(
                !outcome.ready_tickets().contains(&ticket),
                "{stage}: ticket woke before every exact missing fragment arrived"
            );
        }
    }

    take_and_release(stage, store, expected_jobs, ticket, checkpoint);
}

pub(super) fn source_changed_pending(
    stage: &'static str,
    store: &RangeStore,
    expected_jobs: &[JobId],
    ticket: DataTicket,
    missing: &SmallRanges,
    checkpoint: ResumeCheckpoint,
    trace: &mut Vec<PendingEvent>,
) {
    assert!(
        !missing.is_empty(),
        "{stage}: source-change Pending is empty"
    );
    assert!(
        trace.len() < 128,
        "{stage}: strict pipeline exceeded the Pending-turn ceiling"
    );
    trace.push(PendingEvent {
        stage,
        ordinal: u64::try_from(trace.len()).expect("Pending ordinal fits u64"),
        checkpoint_role: checkpoint_role(checkpoint),
        ranges: missing
            .as_slice()
            .iter()
            .map(|range| (range.start(), range.len()))
            .collect(),
    });
    store
        .signal_source_changed()
        .expect("source-change injection poisons the exact VM store");
    take_and_release(stage, store, expected_jobs, ticket, checkpoint);
}

fn checkpoint_role(checkpoint: ResumeCheckpoint) -> &'static str {
    match checkpoint.value() % 100_000 {
        1_001 => "xref-tail",
        1_002 => "xref-section",
        1_003 => "attestation-scan",
        1_004 => "attestation-object-envelope",
        1_005 => "attestation-object-boundary",
        2_101 => "page-index-object-envelope",
        2_102 => "page-index-object-boundary",
        3_101 => "page-lookup-object-envelope",
        3_102 => "page-lookup-object-boundary",
        4_101 => "page-materialization-object-envelope",
        4_102 => "page-materialization-object-boundary",
        5_101 => "page-content-object-envelope",
        5_102 => "page-content-object-boundary",
        5_103 => "page-content-payload",
        6_101 => "image-object-envelope",
        6_102 => "image-object-boundary",
        6_103 => "image-payload",
        7_101 => "font-object-envelope",
        7_102 => "font-object-boundary",
        7_103 => "font-descriptor-envelope",
        7_104 => "font-descriptor-boundary",
        7_105 => "font-program-envelope",
        7_106 => "font-program-boundary",
        7_107 => "font-program-payload",
        value => panic!("unregistered M3 gate checkpoint role {value}"),
    }
}

fn take_and_release(
    stage: &str,
    store: &RangeStore,
    expected_jobs: &[JobId],
    ticket: DataTicket,
    checkpoint: ResumeCheckpoint,
) {
    let subscriptions = store
        .take_subscriptions(ticket)
        .unwrap_or_else(|error| panic!("{stage}: terminal subscriptions are available: {error}"));
    assert_eq!(
        subscriptions.len(),
        1,
        "{stage}: a sequential gate request owns exactly one subscription"
    );
    let subscription = subscriptions[0];
    assert!(
        expected_jobs.contains(&subscription.job()),
        "{stage}: ticket belongs to an unexpected job"
    );
    assert_eq!(
        subscription.checkpoint(),
        checkpoint,
        "{stage}: poll checkpoint and retained subscription differ"
    );
    store
        .release_ticket(ticket)
        .unwrap_or_else(|error| panic!("{stage}: terminal ticket releases exactly once: {error}"));
}

fn supply_fragment(
    store: &RangeStore,
    snapshot: SourceSnapshot,
    input: &[u8],
    range: ByteRange,
    stage: &str,
) -> pdf_rs_bytes::SupplyOutcome {
    let start = usize::try_from(range.start())
        .unwrap_or_else(|_| panic!("{stage}: fixture range start fits usize"));
    let end = usize::try_from(range.end_exclusive())
        .unwrap_or_else(|_| panic!("{stage}: fixture range end fits usize"));
    let bytes = input
        .get(start..end)
        .unwrap_or_else(|| panic!("{stage}: requested range stays within the fixture"))
        .to_vec();
    let response = RangeResponse::new(snapshot, range, bytes)
        .unwrap_or_else(|error| panic!("{stage}: exact response is snapshot-bound: {error}"));
    store
        .supply(response)
        .unwrap_or_else(|error| panic!("{stage}: exact response fits the Range store: {error}"))
}
