use std::fmt;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceSnapshot,
};
use pdf_rs_syntax::{ByteSpan, InputExtent, Located, ObjectRef, PdfDictionary, SyntaxLimits};

use crate::parser::{SectionWindow, parse_traditional_revision_section};
use crate::{XrefCancellation, XrefEntry, XrefError, XrefErrorCode, XrefLimitKind, XrefLimits};

/// One source-bound traditional xref revision section parsed at a caller-supplied anchor.
///
/// Unlike [`crate::XrefSection`], this candidate may contain sparse update rows and retains
/// optional `/Prev`, `/XRefStm`, and `/Root` metadata for later acquisition and composition.
#[derive(Clone, Eq, PartialEq)]
pub struct TraditionalRevisionSection {
    snapshot: SourceSnapshot,
    startxref: u64,
    span: ByteSpan,
    declared_size: u32,
    root: Option<ObjectRef>,
    previous: Option<u64>,
    xref_stream: Option<u64>,
    entries: Vec<XrefEntry>,
    trailer: Located<PdfDictionary>,
}

impl TraditionalRevisionSection {
    #[allow(
        clippy::too_many_arguments,
        reason = "construction copies one complete validated section record"
    )]
    pub(crate) const fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        span: ByteSpan,
        declared_size: u32,
        root: Option<ObjectRef>,
        previous: Option<u64>,
        xref_stream: Option<u64>,
        entries: Vec<XrefEntry>,
        trailer: Located<PdfDictionary>,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            span,
            declared_size,
            root,
            previous,
            xref_stream,
            entries,
            trailer,
        }
    }

    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable snapshot used for parsing.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-supplied physical section anchor.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the exact span from `xref` through the trailer dictionary.
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Returns the trailer `/Size` value.
    pub const fn declared_size(&self) -> u32 {
        self.declared_size
    }

    /// Returns the optional trailer `/Root` reference without applying inheritance.
    pub const fn root(&self) -> Option<ObjectRef> {
        self.root
    }

    /// Returns the optional older primary section anchor from `/Prev`.
    pub const fn previous(&self) -> Option<u64> {
        self.previous
    }

    /// Returns the optional same-revision hybrid supplement anchor from `/XRefStm`.
    pub const fn xref_stream(&self) -> Option<u64> {
        self.xref_stream
    }

    /// Returns sparse entries in strictly increasing object-number order.
    pub fn entries(&self) -> &[XrefEntry] {
        &self.entries
    }

    /// Consumes the section and returns its validated sparse entries.
    pub fn into_entries(self) -> Vec<XrefEntry> {
        self.entries
    }

    /// Returns the source-located trailer dictionary.
    pub const fn trailer(&self) -> &Located<PdfDictionary> {
        &self.trailer
    }
}

impl fmt::Debug for TraditionalRevisionSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraditionalRevisionSection")
            .field("snapshot", &self.snapshot)
            .field("startxref", &self.startxref)
            .field("span", &self.span)
            .field("declared_size", &self.declared_size)
            .field("root", &self.root)
            .field("previous", &self.previous)
            .field("xref_stream", &self.xref_stream)
            .field("entry_count", &self.entries.len())
            .field("trailer", &"[REDACTED]")
            .finish()
    }
}

/// Runtime identity and resume checkpoint for one anchored section job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraditionalRevisionJobContext {
    job: JobId,
    section_checkpoint: ResumeCheckpoint,
}

impl TraditionalRevisionJobContext {
    /// Creates a context owned by one runtime job.
    pub const fn new(job: JobId, section_checkpoint: ResumeCheckpoint) -> Self {
        Self {
            job,
            section_checkpoint,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used for section reads.
    pub const fn section_checkpoint(self) -> ResumeCheckpoint {
        self.section_checkpoint
    }
}

/// Coarse state of one anchored traditional revision job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraditionalRevisionPhase {
    /// Reading and parsing the caller-anchored section.
    Section,
    /// The parsed candidate was returned.
    Complete,
    /// The job reached a terminal failure.
    Failed,
}

/// Deterministic work charged by one anchored section job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraditionalRevisionStats {
    read_bytes: u64,
    parse_bytes: u64,
    section_attempts: u64,
    entries: u64,
}

impl TraditionalRevisionStats {
    /// Returns exact source-window bytes charged once per distinct attempt.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns complete parser-window bytes charged across growth attempts.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns distinct section-window attempts.
    pub const fn section_attempts(self) -> u64 {
        self.section_attempts
    }

    /// Returns entries in the completed candidate, or zero before completion.
    pub const fn entries(self) -> u64 {
        self.entries
    }
}

/// Result of polling one anchored traditional revision job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraditionalRevisionPoll {
    /// A complete source-bound candidate section is ready.
    Ready(TraditionalRevisionSection),
    /// Required bytes are absent and the runtime must retain the checkpoint.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical exact missing ranges.
        missing: SmallRanges,
        /// The section checkpoint to requeue.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure.
    Failed(XrefError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobState {
    Section { window: u64, charged: bool },
    Complete,
    Failed(XrefError),
}

/// One-shot job for parsing a traditional revision section at an exact source anchor.
#[derive(Debug)]
pub struct OpenTraditionalRevisionJob {
    snapshot: SourceSnapshot,
    source_len: u64,
    startxref: u64,
    upper_bound: u64,
    context: TraditionalRevisionJobContext,
    limits: XrefLimits,
    syntax_limits: SyntaxLimits,
    stats: TraditionalRevisionStats,
    state: JobState,
}

impl OpenTraditionalRevisionJob {
    /// Validates source geometry and creates an anchored section job.
    pub fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        upper_bound: u64,
        context: TraditionalRevisionJobContext,
        limits: XrefLimits,
        syntax_limits: SyntaxLimits,
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
        if startxref >= upper_bound || upper_bound > source_len {
            return Err(XrefError::for_code(
                XrefErrorCode::StartXrefOutOfBounds,
                Some(startxref),
            ));
        }
        if limits.max_section_bytes > syntax_limits.max_input_bytes() {
            return Err(XrefError::for_code(XrefErrorCode::InvalidLimits, None));
        }
        let available = upper_bound
            .checked_sub(startxref)
            .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
        let window = limits.initial_section_bytes.min(available);
        Ok(Self {
            snapshot,
            source_len,
            startxref,
            upper_bound,
            context,
            limits,
            syntax_limits,
            stats: TraditionalRevisionStats::default(),
            state: JobState::Section {
                window,
                charged: false,
            },
        })
    }

    /// Returns the immutable snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact section anchor.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the exclusive physical bound the job may not cross.
    pub const fn upper_bound(&self) -> u64 {
        self.upper_bound
    }

    /// Returns the runtime job identity and section checkpoint.
    pub const fn context(&self) -> TraditionalRevisionJobContext {
        self.context
    }

    /// Returns the deterministic xref profile.
    pub const fn limits(&self) -> XrefLimits {
        self.limits
    }

    /// Returns cumulative work through the latest poll.
    pub const fn stats(&self) -> TraditionalRevisionStats {
        self.stats
    }

    /// Returns the current coarse job phase.
    pub const fn phase(&self) -> TraditionalRevisionPhase {
        match self.state {
            JobState::Section { .. } => TraditionalRevisionPhase::Section,
            JobState::Complete => TraditionalRevisionPhase::Complete,
            JobState::Failed(_) => TraditionalRevisionPhase::Failed,
        }
    }

    /// Advances parsing without file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn XrefCancellation + '_),
    ) -> TraditionalRevisionPoll {
        match self.state {
            JobState::Failed(error) => return TraditionalRevisionPoll::Failed(error),
            JobState::Complete => {
                return TraditionalRevisionPoll::Failed(XrefError::for_code(
                    XrefErrorCode::JobAlreadyComplete,
                    Some(self.startxref),
                ));
            }
            JobState::Section { .. } => {}
        }
        loop {
            if source.snapshot() != self.snapshot {
                return self.fail(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
            }
            if cancellation.is_cancelled() {
                return self.fail(XrefError::for_code(
                    XrefErrorCode::Cancelled,
                    Some(self.startxref),
                ));
            }
            let (window, charged) = match self.state {
                JobState::Section { window, charged } => (window, charged),
                JobState::Complete => {
                    return TraditionalRevisionPoll::Failed(XrefError::for_code(
                        XrefErrorCode::JobAlreadyComplete,
                        Some(self.startxref),
                    ));
                }
                JobState::Failed(error) => return TraditionalRevisionPoll::Failed(error),
            };
            if let Some(outcome) = self.poll_section(source, cancellation, window, charged) {
                return outcome;
            }
        }
    }

    fn poll_section(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn XrefCancellation,
        window: u64,
        charged: bool,
    ) -> Option<TraditionalRevisionPoll> {
        let range = match ByteRange::new(self.startxref, window) {
            Ok(value) if value.end_exclusive() <= self.upper_bound => value,
            _ => return Some(self.fail_internal(Some(self.startxref))),
        };
        if !charged {
            if let Err(error) = self.charge_read(window) {
                return Some(self.fail(error));
            }
            self.stats.section_attempts = match self.stats.section_attempts.checked_add(1) {
                Some(value) => value,
                None => return Some(self.fail_internal(Some(self.startxref))),
            };
            self.state = JobState::Section {
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
            ReadPoll::Pending { ticket, missing } => Some(TraditionalRevisionPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.section_checkpoint,
            }),
            ReadPoll::EndOfFile => Some(self.fail(XrefError::for_code(
                XrefErrorCode::UnexpectedEndOfSource,
                Some(self.startxref),
            ))),
            ReadPoll::Failed(error) => Some(self.fail(XrefError::from_source(error))),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes, range) {
                    return Some(self.fail(error));
                }
                if let Err(error) = self.charge_parse(window) {
                    return Some(self.fail(error));
                }
                let extent = if range.end_exclusive() == self.upper_bound {
                    InputExtent::KnownSourceEnd
                } else {
                    InputExtent::MayContinue
                };
                match parse_traditional_revision_section(
                    SectionWindow::new(
                        self.snapshot,
                        self.startxref,
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
                            Err(_) => return Some(self.fail_internal(Some(self.startxref))),
                        };
                        self.state = JobState::Complete;
                        Some(TraditionalRevisionPoll::Ready(section))
                    }
                    Ok(None) => {
                        let available = match self.upper_bound.checked_sub(self.startxref) {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(self.startxref))),
                        };
                        let cap = self.limits.max_section_bytes.min(available);
                        if window >= cap {
                            let error = if cap == available {
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
                                    Some(self.startxref),
                                )
                            };
                            return Some(self.fail(error));
                        }
                        let next = match grow_window(window, cap) {
                            Some(value) => value,
                            None => return Some(self.fail_internal(Some(self.startxref))),
                        };
                        self.state = JobState::Section {
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

    fn charge_read(&mut self, amount: u64) -> Result<(), XrefError> {
        let total = self.stats.read_bytes.checked_add(amount).ok_or_else(|| {
            XrefError::resource(
                XrefLimitKind::TotalReadBytes,
                self.limits.max_total_read_bytes,
                self.stats.read_bytes,
                amount,
                Some(self.startxref),
            )
        })?;
        if total > self.limits.max_total_read_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::TotalReadBytes,
                self.limits.max_total_read_bytes,
                self.stats.read_bytes,
                amount,
                Some(self.startxref),
            ));
        }
        self.stats.read_bytes = total;
        Ok(())
    }

    fn charge_parse(&mut self, amount: u64) -> Result<(), XrefError> {
        let total = self.stats.parse_bytes.checked_add(amount).ok_or_else(|| {
            XrefError::resource(
                XrefLimitKind::TotalParseBytes,
                self.limits.max_total_parse_bytes,
                self.stats.parse_bytes,
                amount,
                Some(self.startxref),
            )
        })?;
        if total > self.limits.max_total_parse_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::TotalParseBytes,
                self.limits.max_total_parse_bytes,
                self.stats.parse_bytes,
                amount,
                Some(self.startxref),
            ));
        }
        self.stats.parse_bytes = total;
        Ok(())
    }

    fn fail(&mut self, error: XrefError) -> TraditionalRevisionPoll {
        self.state = JobState::Failed(error);
        TraditionalRevisionPoll::Failed(error)
    }

    fn fail_internal(&mut self, offset: Option<u64>) -> TraditionalRevisionPoll {
        self.fail(XrefError::for_code(XrefErrorCode::InternalState, offset))
    }
}

fn grow_window(current: u64, cap: u64) -> Option<u64> {
    current
        .checked_mul(2)
        .map(|doubled| doubled.min(cap))
        .filter(|next| *next > current)
}
