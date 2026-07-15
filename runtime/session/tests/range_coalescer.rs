use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, JobId, ReadRequest, RequestPriority, ResumeCheckpoint, SourceErrorCode,
    SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_session::{
    NeverCancelledRangeCoalescer, RangeCoalescerCancellation, RangeCoalescerErrorCategory,
    RangeCoalescerErrorCode, RangeCoalescerLimitConfig, RangeCoalescerLimitKind,
    RangeCoalescerLimits, RangeCoalescerRecoverability, RangeCoalescerRequest,
    RangeRequestCoalescer, RangeRequestId,
};

fn snapshot(seed: u8, len: Option<u64>) -> SourceSnapshot {
    SourceSnapshot::new(
        SourceIdentity::new(
            SourceStableId::new([seed; 32]),
            SourceRevision::new(u64::from(seed)),
        ),
        len,
        SourceValidator::new(
            SourceValidatorKind::StrongEntityTag,
            [seed.wrapping_add(1); 32],
        ),
    )
}

fn limits(config: RangeCoalescerLimitConfig) -> RangeCoalescerLimits {
    RangeCoalescerLimits::validate(config).expect("test coalescer limits are valid")
}

fn generous_limits() -> RangeCoalescerLimits {
    limits(RangeCoalescerLimitConfig {
        max_requests: 16,
        max_groups: 16,
        max_members_per_group: 16,
        max_requested_bytes: 1024,
        max_merged_bytes: 1024,
    })
}

fn request(
    bound: SourceSnapshot,
    id: u64,
    start: u64,
    len: u64,
    priority: RequestPriority,
) -> RangeCoalescerRequest {
    RangeCoalescerRequest::new(
        RangeRequestId::new(id),
        bound,
        ReadRequest::new(
            ByteRange::new(start, len).expect("test range is valid"),
            priority,
            JobId::new(id + 100),
            ResumeCheckpoint::new(id + 200),
        ),
    )
}

fn member_ids(group: &pdf_rs_session::CoalescedRangeGroup) -> Vec<u64> {
    group
        .members()
        .iter()
        .map(|member| member.id().value())
        .collect()
}

#[test]
fn out_of_order_input_has_one_deterministic_plan_with_highest_priorities() {
    let bound = snapshot(0x21, Some(100));
    let input = [
        request(bound, 4, 47, 2, RequestPriority::AdjacentPage),
        request(bound, 2, 12, 4, RequestPriority::VisiblePage),
        request(bound, 3, 40, 5, RequestPriority::BackgroundPrefetch),
        request(bound, 1, 10, 4, RequestPriority::Metadata),
    ];
    let planner = RangeRequestCoalescer::new(bound, 3, generous_limits());

    let first = planner
        .plan(&input, &NeverCancelledRangeCoalescer)
        .expect("bounded requests coalesce");
    let mut reversed = input;
    reversed.reverse();
    let second = planner
        .plan(&reversed, &NeverCancelledRangeCoalescer)
        .expect("input order cannot affect the plan");

    assert_eq!(first, second);
    assert_eq!(first.snapshot(), bound);
    assert_eq!(first.request_count(), 4);
    assert_eq!(first.requested_bytes(), 15);
    assert_eq!(first.merged_bytes(), 15);
    assert_eq!(first.groups().len(), 2);
    assert_eq!(first.groups()[0].range(), ByteRange::new(10, 6).unwrap());
    assert_eq!(first.groups()[0].priority(), RequestPriority::VisiblePage);
    assert_eq!(member_ids(&first.groups()[0]), vec![1, 2]);
    assert_eq!(first.groups()[1].range(), ByteRange::new(40, 9).unwrap());
    assert_eq!(first.groups()[1].priority(), RequestPriority::AdjacentPage);
    assert_eq!(member_ids(&first.groups()[1]), vec![3, 4]);
    assert_eq!(first.groups()[0].members()[0].snapshot(), bound);
    assert_eq!(
        first.groups()[0].members()[1].request().job(),
        JobId::new(102)
    );
}

#[test]
fn overlap_and_gap_threshold_have_distinct_strict_boundaries() {
    let bound = snapshot(0x22, Some(32));
    let gap_two = [
        request(bound, 1, 0, 4, RequestPriority::Metadata),
        request(bound, 2, 6, 2, RequestPriority::Metadata),
    ];
    let at_threshold = RangeRequestCoalescer::new(bound, 2, generous_limits())
        .plan(&gap_two, &NeverCancelledRangeCoalescer)
        .unwrap();
    assert_eq!(at_threshold.groups().len(), 2);

    let below_threshold = RangeRequestCoalescer::new(bound, 3, generous_limits())
        .plan(&gap_two, &NeverCancelledRangeCoalescer)
        .unwrap();
    assert_eq!(below_threshold.groups().len(), 1);
    assert_eq!(
        below_threshold.groups()[0].range(),
        ByteRange::new(0, 8).unwrap()
    );

    let adjacent = [
        request(bound, 3, 10, 2, RequestPriority::Metadata),
        request(bound, 4, 12, 2, RequestPriority::Metadata),
    ];
    assert_eq!(
        RangeRequestCoalescer::new(bound, 0, generous_limits())
            .plan(&adjacent, &NeverCancelledRangeCoalescer)
            .unwrap()
            .groups()
            .len(),
        2
    );

    let overlap = [
        request(bound, 5, 20, 3, RequestPriority::Metadata),
        request(bound, 6, 22, 3, RequestPriority::Metadata),
    ];
    assert_eq!(
        RangeRequestCoalescer::new(bound, 0, generous_limits())
            .plan(&overlap, &NeverCancelledRangeCoalescer)
            .unwrap()
            .groups()
            .len(),
        1
    );
}

#[test]
fn checked_byte_geometry_rejects_range_and_aggregate_overflow() {
    let invalid = ByteRange::new(u64::MAX, 1).expect_err("exclusive end must not wrap");
    assert_eq!(invalid.code(), SourceErrorCode::InvalidRange);

    let bound = snapshot(0x23, None);
    let huge = ByteRange::new(0, u64::MAX).unwrap();
    let input = [
        RangeCoalescerRequest::new(
            RangeRequestId::new(1),
            bound,
            ReadRequest::new(
                huge,
                RequestPriority::BackgroundPrefetch,
                JobId::new(1),
                ResumeCheckpoint::new(1),
            ),
        ),
        RangeCoalescerRequest::new(
            RangeRequestId::new(2),
            bound,
            ReadRequest::new(
                huge,
                RequestPriority::VisiblePage,
                JobId::new(2),
                ResumeCheckpoint::new(2),
            ),
        ),
    ];
    let unlimited_bytes = limits(RangeCoalescerLimitConfig {
        max_requests: 2,
        max_groups: 2,
        max_members_per_group: 2,
        max_requested_bytes: u64::MAX,
        max_merged_bytes: u64::MAX,
    });
    let error = RangeRequestCoalescer::new(bound, 0, unlimited_bytes)
        .plan(&input, &NeverCancelledRangeCoalescer)
        .expect_err("overlapping member lengths still use checked aggregate accounting");
    assert_eq!(error.code(), RangeCoalescerErrorCode::ArithmeticOverflow);
    assert_eq!(error.category(), RangeCoalescerErrorCategory::Resource);
    assert_eq!(
        error.recoverability(),
        RangeCoalescerRecoverability::ReduceWorkload
    );
}

struct CancelAfterProbes {
    probes: AtomicUsize,
    cancel_at: usize,
}

impl RangeCoalescerCancellation for CancelAfterProbes {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::AcqRel) >= self.cancel_at
    }
}

#[test]
fn cancellation_is_checked_before_and_during_bounded_planning() {
    let bound = snapshot(0x24, Some(32));
    let input = [
        request(bound, 1, 0, 2, RequestPriority::Metadata),
        request(bound, 2, 4, 2, RequestPriority::Metadata),
    ];
    let already_cancelled = AtomicBool::new(true);
    let error = RangeRequestCoalescer::new(bound, 1, generous_limits())
        .plan(&input, &already_cancelled)
        .expect_err("pre-cancelled planning must do no work");
    assert_eq!(error.code(), RangeCoalescerErrorCode::Cancelled);
    assert_eq!(error.category(), RangeCoalescerErrorCategory::Lifecycle);
    assert_eq!(
        error.recoverability(),
        RangeCoalescerRecoverability::DoNotRetry
    );

    let mid_plan = CancelAfterProbes {
        probes: AtomicUsize::new(0),
        cancel_at: 2,
    };
    let error = RangeRequestCoalescer::new(bound, 1, generous_limits())
        .plan(&input, &mid_plan)
        .expect_err("planning probes cancellation between bounded members");
    assert_eq!(error.code(), RangeCoalescerErrorCode::Cancelled);
    assert!(mid_plan.probes.load(Ordering::Acquire) >= 3);
}

#[test]
fn count_member_and_byte_budgets_fail_before_unbounded_growth() {
    let bound = snapshot(0x25, Some(64));
    let two_disjoint = [
        request(bound, 1, 0, 2, RequestPriority::Metadata),
        request(bound, 2, 6, 2, RequestPriority::Metadata),
    ];

    let request_limit = limits(RangeCoalescerLimitConfig {
        max_requests: 1,
        max_groups: 1,
        max_members_per_group: 1,
        max_requested_bytes: 64,
        max_merged_bytes: 64,
    });
    let error = RangeRequestCoalescer::new(bound, 0, request_limit)
        .plan(&two_disjoint, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        RangeCoalescerLimitKind::Requests
    );
    assert_eq!(error.limit().unwrap().limit(), 1);
    assert_eq!(error.limit().unwrap().attempted(), 2);

    let group_limit = limits(RangeCoalescerLimitConfig {
        max_requests: 2,
        max_groups: 1,
        max_members_per_group: 2,
        max_requested_bytes: 64,
        max_merged_bytes: 64,
    });
    let error = RangeRequestCoalescer::new(bound, 0, group_limit)
        .plan(&two_disjoint, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        RangeCoalescerLimitKind::Groups
    );

    let overlapping = [
        request(bound, 3, 10, 4, RequestPriority::Metadata),
        request(bound, 4, 12, 4, RequestPriority::Metadata),
    ];
    let member_limit = limits(RangeCoalescerLimitConfig {
        max_requests: 2,
        max_groups: 2,
        max_members_per_group: 1,
        max_requested_bytes: 64,
        max_merged_bytes: 64,
    });
    let error = RangeRequestCoalescer::new(bound, 0, member_limit)
        .plan(&overlapping, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        RangeCoalescerLimitKind::MembersPerGroup
    );

    let requested_limit = limits(RangeCoalescerLimitConfig {
        max_requests: 2,
        max_groups: 2,
        max_members_per_group: 2,
        max_requested_bytes: 3,
        max_merged_bytes: 64,
    });
    let error = RangeRequestCoalescer::new(bound, 0, requested_limit)
        .plan(&two_disjoint, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        RangeCoalescerLimitKind::RequestedBytes
    );
    assert_eq!(error.limit().unwrap().attempted(), 4);

    let merged_limit = limits(RangeCoalescerLimitConfig {
        max_requests: 2,
        max_groups: 2,
        max_members_per_group: 2,
        max_requested_bytes: 4,
        max_merged_bytes: 7,
    });
    let error = RangeRequestCoalescer::new(bound, 5, merged_limit)
        .plan(&two_disjoint, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        RangeCoalescerLimitKind::MergedBytes
    );
    assert_eq!(error.limit().unwrap().attempted(), 8);
}

#[test]
fn snapshot_mismatch_bounds_and_duplicate_ids_fail_closed() {
    let bound = snapshot(0x26, Some(10));
    let changed = snapshot(0x27, Some(10));
    let planner = RangeRequestCoalescer::new(bound, 1, generous_limits());

    let error = planner
        .plan(
            &[request(changed, 1, 0, 2, RequestPriority::Metadata)],
            &NeverCancelledRangeCoalescer,
        )
        .unwrap_err();
    assert_eq!(error.code(), RangeCoalescerErrorCode::SourceChanged);
    assert_eq!(error.category(), RangeCoalescerErrorCategory::Integrity);
    assert_eq!(
        error.recoverability(),
        RangeCoalescerRecoverability::ReopenSource
    );

    let error = planner
        .plan(
            &[request(bound, 2, 9, 2, RequestPriority::Metadata)],
            &NeverCancelledRangeCoalescer,
        )
        .unwrap_err();
    assert_eq!(error.code(), RangeCoalescerErrorCode::RequestOutOfBounds);

    let duplicate = [
        request(bound, 3, 0, 2, RequestPriority::Metadata),
        request(bound, 3, 4, 2, RequestPriority::VisiblePage),
    ];
    let error = planner
        .plan(&duplicate, &NeverCancelledRangeCoalescer)
        .unwrap_err();
    assert_eq!(error.code(), RangeCoalescerErrorCode::DuplicateRequestId);
}

#[test]
fn invalid_limit_relationships_are_rejected() {
    let error = RangeCoalescerLimits::validate(RangeCoalescerLimitConfig {
        max_requests: 1,
        max_groups: 2,
        max_members_per_group: 1,
        max_requested_bytes: 1,
        max_merged_bytes: 1,
    })
    .expect_err("groups cannot exceed the admitted request count");
    assert_eq!(error.code(), RangeCoalescerErrorCode::InvalidLimits);
    assert_eq!(error.category(), RangeCoalescerErrorCategory::Input);
}
