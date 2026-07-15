use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceSnapshot,
};

use crate::parser::{TailParse, parse_tail};
use crate::{XrefCancellation, XrefError, XrefErrorCode, XrefLimitKind, XrefLimits};

/// One source-bound final `startxref` declaration discovered from the PDF tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalStartXref {
    snapshot: SourceSnapshot,
    startxref: u64,
    tail_start: u64,
}

impl FinalStartXref {
    const fn new(snapshot: SourceSnapshot, startxref: u64, tail_start: u64) -> Self {
        Self {
            snapshot,
            startxref,
            tail_start,
        }
    }

    /// Returns the immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot used for discovery.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the final declared cross-reference anchor.
    pub const fn startxref(self) -> u64 {
        self.startxref
    }

    /// Returns the physical start of the final `startxref` keyword.
    ///
    /// A later acquisition layer may use this as an exclusive upper bound for the final xref
    /// section. This type does not itself classify or parse the declared target.
    pub const fn tail_start(self) -> u64 {
        self.tail_start
    }
}

/// Runtime identity and resume checkpoint for final-marker discovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalStartXrefJobContext {
    job: JobId,
    tail_checkpoint: ResumeCheckpoint,
}

impl FinalStartXrefJobContext {
    /// Creates a final-marker context owned by one runtime job.
    pub const fn new(job: JobId, tail_checkpoint: ResumeCheckpoint) -> Self {
        Self {
            job,
            tail_checkpoint,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used for bounded tail reads.
    pub const fn tail_checkpoint(self) -> ResumeCheckpoint {
        self.tail_checkpoint
    }
}

/// Coarse state of one final-marker discovery job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalStartXrefPhase {
    /// Reading and parsing a bounded suffix window.
    Tail,
    /// The source-bound final declaration was returned.
    Complete,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Deterministic work charged by final-marker discovery.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FinalStartXrefStats {
    read_bytes: u64,
    parse_bytes: u64,
    tail_attempts: u64,
}

impl FinalStartXrefStats {
    /// Returns bytes charged for newly installed suffix windows.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns complete suffix-window bytes charged before parser attempts.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns the number of distinct suffix windows requested.
    pub const fn tail_attempts(self) -> u64 {
        self.tail_attempts
    }
}

/// Result of polling one final-marker discovery job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalStartXrefPoll {
    /// A complete source-bound final declaration is ready.
    Ready(FinalStartXref),
    /// Required suffix bytes are absent and the runtime must retain the checkpoint.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical exact missing ranges.
        missing: SmallRanges,
        /// The tail checkpoint to requeue.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure.
    Failed(XrefError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobState {
    Tail { window: u64, charged: bool },
    Complete,
    Failed(XrefError),
}

/// One-shot, snapshot-bound job that discovers only the final `startxref` declaration.
///
/// This is the reusable tail half of [`crate::OpenXrefJob`]. It performs no target
/// classification and parses no xref section.
#[derive(Debug)]
pub struct OpenFinalStartXrefJob {
    snapshot: SourceSnapshot,
    source_len: u64,
    context: FinalStartXrefJobContext,
    limits: XrefLimits,
    stats: FinalStartXrefStats,
    state: JobState,
}

impl OpenFinalStartXrefJob {
    /// Validates source geometry and creates a final-marker discovery job.
    pub fn new(
        snapshot: SourceSnapshot,
        context: FinalStartXrefJobContext,
        limits: XrefLimits,
    ) -> Result<Self, XrefError> {
        let source_len = snapshot
            .len()
            .ok_or_else(|| XrefError::for_code(XrefErrorCode::UnknownSourceLength, None))?;
        if source_len == 0 {
            return Err(XrefError::for_code(XrefErrorCode::EmptySource, Some(0)));
        }
        if source_len > limits.max_source_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::SourceBytes,
                limits.max_source_bytes,
                0,
                source_len,
                None,
            ));
        }
        Ok(Self {
            snapshot,
            source_len,
            context,
            limits,
            stats: FinalStartXrefStats::default(),
            state: JobState::Tail {
                window: limits.initial_tail_bytes.min(source_len),
                charged: false,
            },
        })
    }

    /// Returns the immutable snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the runtime identity and tail checkpoint.
    pub const fn context(&self) -> FinalStartXrefJobContext {
        self.context
    }

    /// Returns the validated deterministic xref limits.
    pub const fn limits(&self) -> XrefLimits {
        self.limits
    }

    /// Returns cumulative work through the latest poll.
    pub const fn stats(&self) -> FinalStartXrefStats {
        self.stats
    }

    /// Returns the current coarse job phase.
    pub const fn phase(&self) -> FinalStartXrefPhase {
        match self.state {
            JobState::Tail { .. } => FinalStartXrefPhase::Tail,
            JobState::Complete => FinalStartXrefPhase::Complete,
            JobState::Failed(_) => FinalStartXrefPhase::Failed,
        }
    }

    /// Advances final-marker discovery without file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn XrefCancellation + '_),
    ) -> FinalStartXrefPoll {
        let (window, charged) = match self.state {
            JobState::Tail { window, charged } => (window, charged),
            JobState::Complete => {
                return FinalStartXrefPoll::Failed(XrefError::for_code(
                    XrefErrorCode::JobAlreadyComplete,
                    None,
                ));
            }
            JobState::Failed(error) => return FinalStartXrefPoll::Failed(error),
        };
        if source.snapshot() != self.snapshot {
            return self.fail(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
        }
        if cancellation.is_cancelled() {
            return self.fail(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
        self.poll_tail(source, cancellation, window, charged)
    }

    fn poll_tail(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn XrefCancellation,
        window: u64,
        charged: bool,
    ) -> FinalStartXrefPoll {
        let Some(start) = self.source_len.checked_sub(window) else {
            return self.fail_internal(None);
        };
        let Ok(range) = ByteRange::new(start, window) else {
            return self.fail_internal(Some(start));
        };
        if !charged {
            if let Err(error) = self.charge_read(window, Some(start)) {
                return self.fail(error);
            }
            let Some(attempts) = self.stats.tail_attempts.checked_add(1) else {
                return self.fail_internal(Some(start));
            };
            self.stats.tail_attempts = attempts;
            self.state = JobState::Tail {
                window,
                charged: true,
            };
        }
        let request = ReadRequest::new(
            range,
            RequestPriority::Metadata,
            self.context.job,
            self.context.tail_checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => FinalStartXrefPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.tail_checkpoint,
            },
            ReadPoll::EndOfFile => self.fail(XrefError::for_code(
                XrefErrorCode::UnexpectedEndOfSource,
                Some(start),
            )),
            ReadPoll::Failed(error) => self.fail(XrefError::from_source(error)),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return self.fail(error);
                }
                if let Err(error) = self.charge_parse(window, Some(start)) {
                    return self.fail(error);
                }
                match parse_tail(bytes.bytes(), start, self.source_len, cancellation) {
                    Ok(TailParse::Found {
                        startxref,
                        tail_start,
                    }) => {
                        self.state = JobState::Complete;
                        FinalStartXrefPoll::Ready(FinalStartXref::new(
                            self.snapshot,
                            startxref,
                            tail_start,
                        ))
                    }
                    Ok(TailParse::NeedLarger) => {
                        let cap = self.limits.max_tail_bytes.min(self.source_len);
                        if window >= cap {
                            let error = if cap == self.source_len {
                                XrefError::for_code(XrefErrorCode::StartXrefNotFound, Some(0))
                            } else {
                                XrefError::resource(
                                    XrefLimitKind::TailBytes,
                                    self.limits.max_tail_bytes,
                                    window,
                                    1,
                                    Some(start),
                                )
                            };
                            return self.fail(error);
                        }
                        let Some(next) = grow_window(window, cap) else {
                            return self.fail_internal(Some(start));
                        };
                        self.state = JobState::Tail {
                            window: next,
                            charged: false,
                        };
                        self.poll(source, cancellation)
                    }
                    Err(error) => self.fail(error),
                }
            }
        }
    }

    fn validate_slice(&self, bytes: &ByteSlice, expected: ByteRange) -> Result<(), XrefError> {
        if bytes.identity() != self.snapshot.identity() {
            return Err(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
        }
        if bytes.range() != expected {
            return Err(XrefError::for_code(
                XrefErrorCode::InternalState,
                Some(expected.start()),
            ));
        }
        Ok(())
    }

    fn charge_read(&mut self, amount: u64, offset: Option<u64>) -> Result<(), XrefError> {
        let Some(total) = self.stats.read_bytes.checked_add(amount) else {
            return Err(XrefError::resource(
                XrefLimitKind::TotalReadBytes,
                self.limits.max_total_read_bytes,
                self.stats.read_bytes,
                amount,
                offset,
            ));
        };
        if total > self.limits.max_total_read_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::TotalReadBytes,
                self.limits.max_total_read_bytes,
                self.stats.read_bytes,
                amount,
                offset,
            ));
        }
        self.stats.read_bytes = total;
        Ok(())
    }

    fn charge_parse(&mut self, amount: u64, offset: Option<u64>) -> Result<(), XrefError> {
        let Some(total) = self.stats.parse_bytes.checked_add(amount) else {
            return Err(XrefError::resource(
                XrefLimitKind::TotalParseBytes,
                self.limits.max_total_parse_bytes,
                self.stats.parse_bytes,
                amount,
                offset,
            ));
        };
        if total > self.limits.max_total_parse_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::TotalParseBytes,
                self.limits.max_total_parse_bytes,
                self.stats.parse_bytes,
                amount,
                offset,
            ));
        }
        self.stats.parse_bytes = total;
        Ok(())
    }

    fn fail(&mut self, error: XrefError) -> FinalStartXrefPoll {
        self.state = JobState::Failed(error);
        FinalStartXrefPoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> FinalStartXrefPoll {
        self.fail(XrefError::for_code(XrefErrorCode::InternalState, offset))
    }
}

pub(crate) fn grow_window(current: u64, cap: u64) -> Option<u64> {
    current
        .checked_mul(2)
        .map(|doubled| doubled.min(cap))
        .filter(|next| *next > current)
}
