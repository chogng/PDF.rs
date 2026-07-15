use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{ByteRange, ReadRequest, RequestPriority, SourceSnapshot};

const HARD_MAX_REQUESTS: usize = 4096;
const HARD_MAX_GROUPS: usize = 4096;
const HARD_MAX_MEMBERS_PER_GROUP: usize = 4096;

/// Unvalidated deterministic budgets for one Range-coalescing plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeCoalescerLimitConfig {
    /// Maximum source requests accepted by one planning call.
    pub max_requests: usize,
    /// Maximum disjoint host ranges emitted by one planning call.
    pub max_groups: usize,
    /// Maximum source requests retained in one merged host range.
    pub max_members_per_group: usize,
    /// Maximum checked sum of exact member-request lengths.
    pub max_requested_bytes: u64,
    /// Maximum checked sum of emitted merged-range lengths, including filled gaps.
    pub max_merged_bytes: u64,
}

impl Default for RangeCoalescerLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 256,
            max_groups: 256,
            max_members_per_group: 256,
            max_requested_bytes: 16 * 1024 * 1024,
            max_merged_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Validated deterministic budgets below fixed metadata-count ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeCoalescerLimits {
    max_requests: usize,
    max_groups: usize,
    max_members_per_group: usize,
    max_requested_bytes: u64,
    max_merged_bytes: u64,
}

impl RangeCoalescerLimits {
    /// Validates one complete Range-coalescing budget profile.
    pub fn validate(config: RangeCoalescerLimitConfig) -> Result<Self, RangeCoalescerError> {
        if config.max_requests == 0
            || config.max_requests > HARD_MAX_REQUESTS
            || config.max_groups == 0
            || config.max_groups > HARD_MAX_GROUPS
            || config.max_groups > config.max_requests
            || config.max_members_per_group == 0
            || config.max_members_per_group > HARD_MAX_MEMBERS_PER_GROUP
            || config.max_members_per_group > config.max_requests
            || config.max_requested_bytes == 0
            || config.max_merged_bytes == 0
        {
            return Err(RangeCoalescerError::for_code(
                RangeCoalescerErrorCode::InvalidLimits,
            ));
        }
        Ok(Self {
            max_requests: config.max_requests,
            max_groups: config.max_groups,
            max_members_per_group: config.max_members_per_group,
            max_requested_bytes: config.max_requested_bytes,
            max_merged_bytes: config.max_merged_bytes,
        })
    }

    /// Returns the maximum input request count.
    pub const fn max_requests(self) -> usize {
        self.max_requests
    }

    /// Returns the maximum emitted group count.
    pub const fn max_groups(self) -> usize {
        self.max_groups
    }

    /// Returns the maximum member count retained by one group.
    pub const fn max_members_per_group(self) -> usize {
        self.max_members_per_group
    }

    /// Returns the maximum checked sum of exact member lengths.
    pub const fn max_requested_bytes(self) -> u64 {
        self.max_requested_bytes
    }

    /// Returns the maximum checked sum of merged host-range lengths.
    pub const fn max_merged_bytes(self) -> u64 {
        self.max_merged_bytes
    }
}

impl Default for RangeCoalescerLimits {
    fn default() -> Self {
        Self::validate(RangeCoalescerLimitConfig::default())
            .expect("built-in Range-coalescer limits satisfy hard ceilings")
    }
}

/// Resource dimension that rejected a bounded Range-coalescing plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeCoalescerLimitKind {
    /// Input source request count.
    Requests,
    /// Emitted disjoint host-range count.
    Groups,
    /// Requests retained by one emitted group.
    MembersPerGroup,
    /// Checked sum of exact member-request lengths.
    RequestedBytes,
    /// Checked sum of emitted merged-range lengths.
    MergedBytes,
    /// Fallible allocation for bounded planning metadata.
    Allocation,
}

/// Source-redacted Range-coalescer budget context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeCoalescerLimit {
    kind: RangeCoalescerLimitKind,
    limit: u64,
    attempted: u64,
}

impl RangeCoalescerLimit {
    const fn new(kind: RangeCoalescerLimitKind, limit: u64, attempted: u64) -> Self {
        Self {
            kind,
            limit,
            attempted,
        }
    }

    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> RangeCoalescerLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the rejected count or byte total.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable Range-coalescing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeCoalescerErrorCode {
    /// Configured limits are zero, inconsistent, or above hard metadata ceilings.
    InvalidLimits,
    /// A request identifier appeared more than once in one plan.
    DuplicateRequestId,
    /// A request is bound to a different immutable source snapshot.
    SourceChanged,
    /// A request extends beyond the snapshot's known source length.
    RequestOutOfBounds,
    /// The owning runtime requested cooperative cancellation.
    Cancelled,
    /// A configured deterministic count or byte budget was exceeded.
    ResourceLimit,
    /// Checked request or plan byte arithmetic overflowed `u64`.
    ArithmeticOverflow,
    /// Bounded planning metadata could not be allocated.
    AllocationFailed,
}

/// Coarse Range-coalescing failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeCoalescerErrorCategory {
    /// Caller-supplied limits, identities, or range geometry are invalid.
    Input,
    /// Immutable source snapshot isolation no longer holds.
    Integrity,
    /// The owning request was cancelled at a bounded probe.
    Lifecycle,
    /// A deterministic planning budget was exhausted.
    Resource,
}

/// Stable recovery policy for a Range-coalescing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeCoalescerRecoverability {
    /// Correct the request identity, range, or limit configuration.
    CorrectInput,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Stop the cancelled request without retrying this planning call.
    DoNotRetry,
    /// Reduce the batch or increase an approved deterministic budget.
    ReduceWorkload,
}

/// Source-redacted failure returned by the Range request coalescer.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RangeCoalescerError {
    code: RangeCoalescerErrorCode,
    category: RangeCoalescerErrorCategory,
    recoverability: RangeCoalescerRecoverability,
    diagnostic_id: &'static str,
    limit: Option<RangeCoalescerLimit>,
}

impl RangeCoalescerError {
    const fn for_code(code: RangeCoalescerErrorCode) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            RangeCoalescerErrorCode::InvalidLimits => (
                RangeCoalescerErrorCategory::Input,
                RangeCoalescerRecoverability::CorrectInput,
                "RPE-SESSION-RANGE-0001",
            ),
            RangeCoalescerErrorCode::DuplicateRequestId => (
                RangeCoalescerErrorCategory::Input,
                RangeCoalescerRecoverability::CorrectInput,
                "RPE-SESSION-RANGE-0002",
            ),
            RangeCoalescerErrorCode::SourceChanged => (
                RangeCoalescerErrorCategory::Integrity,
                RangeCoalescerRecoverability::ReopenSource,
                "RPE-SESSION-RANGE-0003",
            ),
            RangeCoalescerErrorCode::RequestOutOfBounds => (
                RangeCoalescerErrorCategory::Input,
                RangeCoalescerRecoverability::CorrectInput,
                "RPE-SESSION-RANGE-0004",
            ),
            RangeCoalescerErrorCode::Cancelled => (
                RangeCoalescerErrorCategory::Lifecycle,
                RangeCoalescerRecoverability::DoNotRetry,
                "RPE-SESSION-RANGE-0005",
            ),
            RangeCoalescerErrorCode::ResourceLimit => (
                RangeCoalescerErrorCategory::Resource,
                RangeCoalescerRecoverability::ReduceWorkload,
                "RPE-SESSION-RANGE-0006",
            ),
            RangeCoalescerErrorCode::ArithmeticOverflow => (
                RangeCoalescerErrorCategory::Resource,
                RangeCoalescerRecoverability::ReduceWorkload,
                "RPE-SESSION-RANGE-0007",
            ),
            RangeCoalescerErrorCode::AllocationFailed => (
                RangeCoalescerErrorCategory::Resource,
                RangeCoalescerRecoverability::ReduceWorkload,
                "RPE-SESSION-RANGE-0008",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
        }
    }

    const fn for_limit(kind: RangeCoalescerLimitKind, limit: u64, attempted: u64) -> Self {
        Self {
            code: RangeCoalescerErrorCode::ResourceLimit,
            category: RangeCoalescerErrorCategory::Resource,
            recoverability: RangeCoalescerRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SESSION-RANGE-0009",
            limit: Some(RangeCoalescerLimit::new(kind, limit, attempted)),
        }
    }

    const fn allocation_failed(attempted: usize) -> Self {
        Self {
            code: RangeCoalescerErrorCode::AllocationFailed,
            category: RangeCoalescerErrorCategory::Resource,
            recoverability: RangeCoalescerRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SESSION-RANGE-0010",
            limit: Some(RangeCoalescerLimit::new(
                RangeCoalescerLimitKind::Allocation,
                HARD_MAX_REQUESTS as u64,
                attempted as u64,
            )),
        }
    }

    /// Returns the stable machine-readable code.
    pub const fn code(self) -> RangeCoalescerErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> RangeCoalescerErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> RangeCoalescerRecoverability {
        self.recoverability
    }

    /// Returns the stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns resource-limit context when a deterministic budget rejected the plan.
    pub const fn limit(self) -> Option<RangeCoalescerLimit> {
        self.limit
    }
}

impl fmt::Debug for RangeCoalescerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RangeCoalescerError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("limit", &self.limit)
            .finish()
    }
}

impl fmt::Display for RangeCoalescerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Range coalescing failed with {:?} ({})",
            self.code, self.diagnostic_id
        )
    }
}

impl Error for RangeCoalescerError {}

/// Cooperative cancellation probe supplied by the owning runtime turn.
pub trait RangeCoalescerCancellation: Send + Sync {
    /// Reports whether planning must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests Range-planning cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelledRangeCoalescer;

impl RangeCoalescerCancellation for NeverCancelledRangeCoalescer {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl RangeCoalescerCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

/// Opaque identity for one member request retained across host-range merging.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RangeRequestId(u64);

impl RangeRequestId {
    /// Creates an opaque request identity allocated by the owning runtime.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric value for host-adapter bookkeeping.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// One exact source request submitted for snapshot-bound host-range merging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeCoalescerRequest {
    id: RangeRequestId,
    snapshot: SourceSnapshot,
    request: ReadRequest,
}

impl RangeCoalescerRequest {
    /// Binds one exact resumable read request to its snapshot and runtime identity.
    pub const fn new(id: RangeRequestId, snapshot: SourceSnapshot, request: ReadRequest) -> Self {
        Self {
            id,
            snapshot,
            request,
        }
    }

    /// Returns the runtime-owned request identity.
    pub const fn id(self) -> RangeRequestId {
        self.id
    }

    /// Returns the immutable source snapshot carried by this request.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact resumable byte request retained for completion routing.
    pub const fn request(self) -> ReadRequest {
        self.request
    }
}

/// One deterministic merged host range and its exact member requests.
#[derive(Debug, Eq, PartialEq)]
pub struct CoalescedRangeGroup {
    range: ByteRange,
    priority: RequestPriority,
    members: Vec<RangeCoalescerRequest>,
}

impl CoalescedRangeGroup {
    /// Returns the checked merged half-open range, including any admitted gap bytes.
    pub const fn range(&self) -> ByteRange {
        self.range
    }

    /// Returns the most urgent priority among all retained members.
    pub const fn priority(&self) -> RequestPriority {
        self.priority
    }

    /// Returns deterministically ordered exact member requests.
    pub fn members(&self) -> &[RangeCoalescerRequest] {
        &self.members
    }
}

/// Immutable output of one bounded snapshot-bound coalescing pass.
#[derive(Debug, Eq, PartialEq)]
pub struct RangeCoalescingPlan {
    snapshot: SourceSnapshot,
    groups: Vec<CoalescedRangeGroup>,
    request_count: usize,
    requested_bytes: u64,
    merged_bytes: u64,
}

impl RangeCoalescingPlan {
    /// Returns the immutable snapshot shared by every emitted group.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns merged groups in ascending source-offset order.
    pub fn groups(&self) -> &[CoalescedRangeGroup] {
        &self.groups
    }

    /// Returns the number of exact member requests retained by the plan.
    pub const fn request_count(&self) -> usize {
        self.request_count
    }

    /// Returns the checked sum of exact member-request lengths.
    pub const fn requested_bytes(&self) -> u64 {
        self.requested_bytes
    }

    /// Returns the checked sum of emitted merged-range lengths.
    pub const fn merged_bytes(&self) -> u64 {
        self.merged_bytes
    }
}

/// Pure bounded planner for snapshot-bound host Range requests.
///
/// This component performs no transport, scheduling, parser execution, or ticket
/// completion. It only produces deterministic groups for a host adapter. Two
/// non-empty half-open ranges merge when they overlap or when the gap between
/// them is strictly less than `gap_threshold`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeRequestCoalescer {
    snapshot: SourceSnapshot,
    gap_threshold: u64,
    limits: RangeCoalescerLimits,
}

impl RangeRequestCoalescer {
    /// Creates a planner bound to one immutable source snapshot.
    pub const fn new(
        snapshot: SourceSnapshot,
        gap_threshold: u64,
        limits: RangeCoalescerLimits,
    ) -> Self {
        Self {
            snapshot,
            gap_threshold,
            limits,
        }
    }

    /// Returns the immutable source snapshot accepted by this planner.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the strict gap threshold used for merging.
    pub const fn gap_threshold(self) -> u64 {
        self.gap_threshold
    }

    /// Returns the validated planning budgets.
    pub const fn limits(self) -> RangeCoalescerLimits {
        self.limits
    }

    /// Produces one deterministic bounded plan without running transport or parser work.
    pub fn plan(
        &self,
        requests: &[RangeCoalescerRequest],
        cancellation: &(dyn RangeCoalescerCancellation + '_),
    ) -> Result<RangeCoalescingPlan, RangeCoalescerError> {
        check_cancelled(cancellation)?;
        if requests.len() > self.limits.max_requests {
            return Err(count_limit(
                RangeCoalescerLimitKind::Requests,
                self.limits.max_requests,
                requests.len(),
            ));
        }

        let mut sorted = Vec::new();
        sorted
            .try_reserve_exact(requests.len())
            .map_err(|_| RangeCoalescerError::allocation_failed(requests.len()))?;
        let mut requested_bytes = 0_u64;
        for request in requests {
            check_cancelled(cancellation)?;
            if request.snapshot != self.snapshot {
                return Err(RangeCoalescerError::for_code(
                    RangeCoalescerErrorCode::SourceChanged,
                ));
            }
            let range = request.request.range();
            if self
                .snapshot
                .len()
                .is_some_and(|source_len| range.end_exclusive() > source_len)
            {
                return Err(RangeCoalescerError::for_code(
                    RangeCoalescerErrorCode::RequestOutOfBounds,
                ));
            }
            if sorted
                .iter()
                .any(|existing: &RangeCoalescerRequest| existing.id == request.id)
            {
                return Err(RangeCoalescerError::for_code(
                    RangeCoalescerErrorCode::DuplicateRequestId,
                ));
            }
            requested_bytes = requested_bytes.checked_add(range.len()).ok_or_else(|| {
                RangeCoalescerError::for_code(RangeCoalescerErrorCode::ArithmeticOverflow)
            })?;
            if requested_bytes > self.limits.max_requested_bytes {
                return Err(RangeCoalescerError::for_limit(
                    RangeCoalescerLimitKind::RequestedBytes,
                    self.limits.max_requested_bytes,
                    requested_bytes,
                ));
            }
            sorted.push(*request);
        }

        sorted.sort_unstable_by(|left, right| {
            let left_range = left.request.range();
            let right_range = right.request.range();
            left_range
                .start()
                .cmp(&right_range.start())
                .then_with(|| left_range.end_exclusive().cmp(&right_range.end_exclusive()))
                .then_with(|| {
                    right
                        .request
                        .priority()
                        .rank()
                        .cmp(&left.request.priority().rank())
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        check_cancelled(cancellation)?;

        let group_capacity = requests.len().min(self.limits.max_groups);
        let mut groups: Vec<CoalescedRangeGroup> = Vec::new();
        groups
            .try_reserve_exact(group_capacity)
            .map_err(|_| RangeCoalescerError::allocation_failed(group_capacity))?;

        for request in sorted {
            check_cancelled(cancellation)?;
            if let Some(group) = groups.last_mut()
                && ranges_merge(group.range, request.request.range(), self.gap_threshold)
            {
                append_member(group, request, self.limits.max_members_per_group)?;
                continue;
            }

            if groups.len() == self.limits.max_groups {
                return Err(count_limit(
                    RangeCoalescerLimitKind::Groups,
                    self.limits.max_groups,
                    groups.len().saturating_add(1),
                ));
            }
            let mut members = Vec::new();
            members
                .try_reserve_exact(1)
                .map_err(|_| RangeCoalescerError::allocation_failed(1))?;
            members.push(request);
            groups.push(CoalescedRangeGroup {
                range: request.request.range(),
                priority: request.request.priority(),
                members,
            });
        }

        let mut merged_bytes = 0_u64;
        for group in &groups {
            check_cancelled(cancellation)?;
            merged_bytes = merged_bytes.checked_add(group.range.len()).ok_or_else(|| {
                RangeCoalescerError::for_code(RangeCoalescerErrorCode::ArithmeticOverflow)
            })?;
            if merged_bytes > self.limits.max_merged_bytes {
                return Err(RangeCoalescerError::for_limit(
                    RangeCoalescerLimitKind::MergedBytes,
                    self.limits.max_merged_bytes,
                    merged_bytes,
                ));
            }
        }

        Ok(RangeCoalescingPlan {
            snapshot: self.snapshot,
            groups,
            request_count: requests.len(),
            requested_bytes,
            merged_bytes,
        })
    }
}

fn ranges_merge(left: ByteRange, right: ByteRange, gap_threshold: u64) -> bool {
    right.start() < left.end_exclusive()
        || right
            .start()
            .checked_sub(left.end_exclusive())
            .is_some_and(|gap| gap < gap_threshold)
}

fn append_member(
    group: &mut CoalescedRangeGroup,
    request: RangeCoalescerRequest,
    max_members: usize,
) -> Result<(), RangeCoalescerError> {
    if group.members.len() == max_members {
        return Err(count_limit(
            RangeCoalescerLimitKind::MembersPerGroup,
            max_members,
            group.members.len().saturating_add(1),
        ));
    }
    group
        .members
        .try_reserve_exact(1)
        .map_err(|_| RangeCoalescerError::allocation_failed(1))?;
    let start = group.range.start();
    let end = group
        .range
        .end_exclusive()
        .max(request.request.range().end_exclusive());
    let len = end.checked_sub(start).ok_or_else(|| {
        RangeCoalescerError::for_code(RangeCoalescerErrorCode::ArithmeticOverflow)
    })?;
    group.range = ByteRange::new(start, len)
        .map_err(|_| RangeCoalescerError::for_code(RangeCoalescerErrorCode::ArithmeticOverflow))?;
    if request.request.priority().rank() > group.priority.rank() {
        group.priority = request.request.priority();
    }
    group.members.push(request);
    Ok(())
}

fn count_limit(
    kind: RangeCoalescerLimitKind,
    limit: usize,
    attempted: usize,
) -> RangeCoalescerError {
    let limit = u64::try_from(limit).unwrap_or(u64::MAX);
    let attempted = u64::try_from(attempted).unwrap_or(u64::MAX);
    RangeCoalescerError::for_limit(kind, limit, attempted)
}

fn check_cancelled(
    cancellation: &(dyn RangeCoalescerCancellation + '_),
) -> Result<(), RangeCoalescerError> {
    if cancellation.is_cancelled() {
        return Err(RangeCoalescerError::for_code(
            RangeCoalescerErrorCode::Cancelled,
        ));
    }
    Ok(())
}
