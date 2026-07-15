use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_syntax::{InputExtent, SyntaxLimits};

use crate::parser::{SectionWindow, TailParse, parse_section, parse_tail};
use crate::{XrefError, XrefErrorCode, XrefLimitKind, XrefLimits, XrefSection};

/// Runtime identity and phase-specific resume checkpoints for one xref job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefJobContext {
    job: JobId,
    tail_checkpoint: ResumeCheckpoint,
    section_checkpoint: ResumeCheckpoint,
}

impl XrefJobContext {
    /// Creates a context with distinct checkpoints for tail and section reads.
    ///
    /// [`OpenXrefJob::new`] rejects a context whose checkpoints are equal.
    pub const fn new(
        job: JobId,
        tail_checkpoint: ResumeCheckpoint,
        section_checkpoint: ResumeCheckpoint,
    ) -> Self {
        Self {
            job,
            tail_checkpoint,
            section_checkpoint,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used while locating `startxref`.
    pub const fn tail_checkpoint(self) -> ResumeCheckpoint {
        self.tail_checkpoint
    }

    /// Returns the checkpoint used while reading the xref section.
    pub const fn section_checkpoint(self) -> ResumeCheckpoint {
        self.section_checkpoint
    }
}

/// Coarse resumable phase of an open xref job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefPhase {
    /// Locating the final `startxref` in a bounded suffix window.
    Tail,
    /// Reading and parsing the traditional xref section.
    Section,
    /// The section was returned and the one-shot job is complete.
    Complete,
    /// The job reached a terminal structured failure.
    Failed,
}

/// Cumulative deterministic work charged by one open xref job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XrefStats {
    read_bytes: u64,
    parse_bytes: u64,
    tail_attempts: u64,
    section_attempts: u64,
    entries: u64,
}

impl XrefStats {
    /// Returns bytes charged for newly installed exact source windows.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns complete window bytes charged before parser attempts.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns the number of distinct tail windows requested.
    pub const fn tail_attempts(self) -> u64 {
        self.tail_attempts
    }

    /// Returns the number of distinct xref-section windows requested.
    pub const fn section_attempts(self) -> u64 {
        self.section_attempts
    }

    /// Returns the number of entries in the completed section, or zero before completion.
    pub const fn entries(self) -> u64 {
        self.entries
    }
}

/// Cooperative cancellation probe supplied by the owning runtime.
pub trait XrefCancellation: Send + Sync {
    /// Reports whether the job must stop at the next bounded probe.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation probe that never requests cancellation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NeverCancelled;

impl XrefCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl XrefCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

/// Result of polling one resumable traditional-xref job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XrefPoll {
    /// The complete source-bound section is ready.
    Ready(XrefSection),
    /// Required bytes are absent and the runtime must wait for the ticket.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the requested window.
        missing: SmallRanges,
        /// Phase checkpoint that the runtime must retain when requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a terminal structured failure.
    Failed(XrefError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobState {
    Tail {
        window: u64,
        charged: bool,
    },
    Section {
        startxref: u64,
        window: u64,
        charged: bool,
    },
    Complete,
    Failed(XrefError),
}

/// One-shot, snapshot-bound job for locating and parsing a traditional xref table.
#[derive(Debug)]
pub struct OpenXrefJob {
    snapshot: SourceSnapshot,
    source_len: u64,
    context: XrefJobContext,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    stats: XrefStats,
    discovered_startxref: Option<u64>,
    state: JobState,
}

impl OpenXrefJob {
    /// Validates configuration and binds a new job to an immutable source snapshot.
    pub fn new(
        snapshot: SourceSnapshot,
        context: XrefJobContext,
        limits: XrefLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, XrefError> {
        if context.tail_checkpoint == context.section_checkpoint {
            return Err(XrefError::for_code(XrefErrorCode::InvalidJobContext, None));
        }
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
        if limits.max_section_bytes > syntax_limits.max_input_bytes() {
            return Err(XrefError::for_code(XrefErrorCode::InvalidLimits, None));
        }
        let window = limits.initial_tail_bytes.min(source_len);
        Ok(Self {
            snapshot,
            source_len,
            context,
            limits,
            syntax_limits,
            stats: XrefStats::default(),
            discovered_startxref: None,
            state: JobState::Tail {
                window,
                charged: false,
            },
        })
    }

    /// Returns the immutable source snapshot bound at job creation.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the runtime identity and phase checkpoints.
    pub const fn context(&self) -> XrefJobContext {
        self.context
    }

    /// Returns the validated deterministic xref limits.
    pub const fn limits(&self) -> XrefLimits {
        self.limits
    }

    /// Returns cumulative work charged through the most recent poll.
    pub const fn stats(&self) -> XrefStats {
        self.stats
    }

    /// Returns the final tail-declared xref anchor after the tail phase succeeds.
    ///
    /// The value remains available after a section-phase failure so an explicit sibling repair
    /// policy can reason about the rejected declaration without changing this strict job's
    /// behavior.
    pub const fn discovered_startxref(&self) -> Option<u64> {
        self.discovered_startxref
    }

    /// Returns the job's current coarse phase.
    pub const fn phase(&self) -> XrefPhase {
        match self.state {
            JobState::Tail { .. } => XrefPhase::Tail,
            JobState::Section { .. } => XrefPhase::Section,
            JobState::Complete => XrefPhase::Complete,
            JobState::Failed(_) => XrefPhase::Failed,
        }
    }

    /// Advances the job without performing file, network, callback, or async-runtime I/O.
    ///
    /// A [`XrefPoll::Pending`] result preserves the current logical window. Re-polling that
    /// window after its ticket completes does not charge the read budget a second time.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn XrefCancellation + '_),
    ) -> XrefPoll {
        match self.state {
            JobState::Failed(error) => return XrefPoll::Failed(error),
            JobState::Complete => {
                return XrefPoll::Failed(XrefError::for_code(
                    XrefErrorCode::JobAlreadyComplete,
                    None,
                ));
            }
            JobState::Tail { .. } | JobState::Section { .. } => {}
        }
        loop {
            if source.snapshot() != self.snapshot {
                return self.fail(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
            }
            if cancellation.is_cancelled() {
                return self.fail(XrefError::for_code(XrefErrorCode::Cancelled, None));
            }
            let outcome = match self.state {
                JobState::Tail { window, charged } => {
                    self.poll_tail(source, cancellation, window, charged)
                }
                JobState::Section {
                    startxref,
                    window,
                    charged,
                } => self.poll_section(source, cancellation, startxref, window, charged),
                JobState::Complete => {
                    return XrefPoll::Failed(XrefError::for_code(
                        XrefErrorCode::JobAlreadyComplete,
                        None,
                    ));
                }
                JobState::Failed(error) => return XrefPoll::Failed(error),
            };
            if let Some(result) = outcome {
                return result;
            }
        }
    }

    fn poll_tail(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn XrefCancellation,
        window: u64,
        charged: bool,
    ) -> Option<XrefPoll> {
        let start = match self.source_len.checked_sub(window) {
            Some(value) => value,
            None => return Some(self.fail_internal(None)),
        };
        let range = match ByteRange::new(start, window) {
            Ok(value) => value,
            Err(_) => return Some(self.fail_internal(Some(start))),
        };
        if !charged {
            if let Err(error) = self.charge_read(window, Some(start)) {
                return Some(self.fail(error));
            }
            self.stats.tail_attempts = match self.stats.tail_attempts.checked_add(1) {
                Some(value) => value,
                None => return Some(self.fail_internal(Some(start))),
            };
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
            ReadPoll::Pending { ticket, missing } => Some(XrefPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.tail_checkpoint,
            }),
            ReadPoll::EndOfFile => Some(self.fail(XrefError::for_code(
                XrefErrorCode::UnexpectedEndOfSource,
                Some(start),
            ))),
            ReadPoll::Failed(error) => Some(self.fail(XrefError::from_source(error))),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if let Err(error) = self.charge_parse(window, Some(start)) {
                    return Some(self.fail(error));
                }
                match parse_tail(bytes.bytes(), start, self.source_len, cancellation) {
                    Ok(TailParse::Found(startxref)) => {
                        self.discovered_startxref = Some(startxref);
                        let remaining = match self.source_len.checked_sub(startxref) {
                            Some(value) if value != 0 => value,
                            _ => return Some(self.fail_internal(Some(startxref))),
                        };
                        self.state = JobState::Section {
                            startxref,
                            window: self.limits.initial_section_bytes.min(remaining),
                            charged: false,
                        };
                        None
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
                            return Some(self.fail(error));
                        }
                        let next = match grow_window(window, cap) {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(start))),
                        };
                        self.state = JobState::Tail {
                            window: next,
                            charged: false,
                        };
                        None
                    }
                    Err(error) => Some(self.fail(error)),
                }
            }
        }
    }

    fn poll_section(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn XrefCancellation,
        startxref: u64,
        window: u64,
        charged: bool,
    ) -> Option<XrefPoll> {
        let range = match ByteRange::new(startxref, window) {
            Ok(value) if value.end_exclusive() <= self.source_len => value,
            _ => return Some(self.fail_internal(Some(startxref))),
        };
        if !charged {
            if let Err(error) = self.charge_read(window, Some(startxref)) {
                return Some(self.fail(error));
            }
            self.stats.section_attempts = match self.stats.section_attempts.checked_add(1) {
                Some(value) => value,
                None => return Some(self.fail_internal(Some(startxref))),
            };
            self.state = JobState::Section {
                startxref,
                window,
                charged: true,
            };
        }
        let request = ReadRequest::new(
            range,
            RequestPriority::Metadata,
            self.context.job,
            self.context.section_checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => Some(XrefPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.section_checkpoint,
            }),
            ReadPoll::EndOfFile => Some(self.fail(XrefError::for_code(
                XrefErrorCode::UnexpectedEndOfSource,
                Some(startxref),
            ))),
            ReadPoll::Failed(error) => Some(self.fail(XrefError::from_source(error))),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if let Err(error) = self.charge_parse(window, Some(startxref)) {
                    return Some(self.fail(error));
                }
                let extent = if range.end_exclusive() == self.source_len {
                    InputExtent::KnownSourceEnd
                } else {
                    InputExtent::MayContinue
                };
                match parse_section(
                    SectionWindow::new(
                        self.snapshot,
                        startxref,
                        bytes.bytes(),
                        extent,
                        self.source_len,
                    ),
                    self.limits,
                    self.syntax_limits,
                    cancellation,
                ) {
                    Ok(Some(section)) => {
                        self.stats.entries = match u64::try_from(section.entries().len()) {
                            Ok(value) => value,
                            Err(_) => return Some(self.fail_internal(Some(startxref))),
                        };
                        self.state = JobState::Complete;
                        Some(XrefPoll::Ready(section))
                    }
                    Ok(None) => {
                        let remaining = match self.source_len.checked_sub(startxref) {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(startxref))),
                        };
                        let cap = self.limits.max_section_bytes.min(remaining);
                        if window >= cap {
                            let error = if cap == remaining {
                                XrefError::for_code(
                                    XrefErrorCode::UnexpectedEndOfSource,
                                    Some(range.end_exclusive()),
                                )
                            } else {
                                XrefError::resource(
                                    XrefLimitKind::SectionBytes,
                                    self.limits.max_section_bytes,
                                    window,
                                    1,
                                    Some(startxref),
                                )
                            };
                            return Some(self.fail(error));
                        }
                        let next = match grow_window(window, cap) {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(startxref))),
                        };
                        self.state = JobState::Section {
                            startxref,
                            window: next,
                            charged: false,
                        };
                        None
                    }
                    Err(error) => Some(self.fail(error)),
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

    fn fail(&mut self, error: XrefError) -> XrefPoll {
        self.state = JobState::Failed(error);
        XrefPoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> XrefPoll {
        self.fail(XrefError::for_code(XrefErrorCode::InternalState, offset))
    }
}

fn grow_window(current: u64, cap: u64) -> Option<u64> {
    current
        .checked_mul(2)
        .map(|doubled| doubled.min(cap))
        .filter(|next| *next > current)
}
