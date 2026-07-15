use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceSnapshot,
};
use pdf_rs_syntax::{
    ByteSpan, InputExtent, ObjectRef, SyntaxCancellation, SyntaxErrorCategory, SyntaxInput,
    SyntaxParser, SyntaxPoll,
};

use crate::parser::is_horizontal_whitespace;
use crate::{XrefCancellation, XrefError, XrefErrorCode, XrefLimitKind};

const HARD_MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_ANCHOR_BYTES: u64 = 4096;

/// Unvalidated deterministic limits for one xref-anchor classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefAnchorLimitConfig {
    /// Maximum immutable source length accepted by the classifier.
    pub max_source_bytes: u64,
    /// Maximum bytes read and parsed from the exact anchor.
    pub max_anchor_bytes: u64,
}

impl Default for XrefAnchorLimitConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1024 * 1024,
            max_anchor_bytes: 256,
        }
    }
}

/// Validated xref-anchor limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefAnchorLimits {
    max_source_bytes: u64,
    max_anchor_bytes: u64,
}

impl XrefAnchorLimits {
    /// Validates one anchor-classification budget profile.
    pub fn validate(config: XrefAnchorLimitConfig) -> Result<Self, XrefError> {
        if config.max_source_bytes == 0
            || config.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || config.max_anchor_bytes < 5
            || config.max_anchor_bytes > HARD_MAX_ANCHOR_BYTES
            || config.max_anchor_bytes > config.max_source_bytes
        {
            return Err(XrefError::for_code(XrefErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_source_bytes: config.max_source_bytes,
            max_anchor_bytes: config.max_anchor_bytes,
        })
    }

    /// Returns the maximum accepted immutable source length.
    pub const fn max_source_bytes(self) -> u64 {
        self.max_source_bytes
    }

    /// Returns the maximum exact anchor window.
    pub const fn max_anchor_bytes(self) -> u64 {
        self.max_anchor_bytes
    }
}

impl Default for XrefAnchorLimits {
    fn default() -> Self {
        Self::validate(XrefAnchorLimitConfig::default())
            .expect("built-in xref-anchor limits satisfy hard ceilings")
    }
}

/// Runtime identity and resume checkpoint for one xref-anchor classifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefAnchorJobContext {
    job: JobId,
    checkpoint: ResumeCheckpoint,
}

impl XrefAnchorJobContext {
    /// Creates a context owned by one runtime job.
    pub const fn new(job: JobId, checkpoint: ResumeCheckpoint) -> Self {
        Self { job, checkpoint }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used for the exact anchor read.
    pub const fn checkpoint(self) -> ResumeCheckpoint {
        self.checkpoint
    }
}

/// Exact syntax found at one declared cross-reference anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefAnchorKind {
    /// A traditional table beginning with `xref`, accepted horizontal whitespace, and a line end.
    Traditional,
    /// An indirect-object header whose object may later be framed as an xref stream.
    StreamObject(ObjectRef),
}

/// One bounded, source-bound classification of a declared xref anchor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefAnchor {
    snapshot: SourceSnapshot,
    startxref: u64,
    header_span: ByteSpan,
    kind: XrefAnchorKind,
}

impl XrefAnchor {
    /// Returns the immutable source identity.
    pub const fn source(self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot used for classification.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact declared xref anchor.
    pub const fn startxref(self) -> u64 {
        self.startxref
    }

    /// Returns the exact traditional prefix or indirect-object header span.
    pub const fn header_span(self) -> ByteSpan {
        self.header_span
    }

    /// Returns the classified traditional or indirect-object syntax.
    pub const fn kind(self) -> XrefAnchorKind {
        self.kind
    }

    /// Returns the indirect-object reference when this is a stream-object candidate.
    pub const fn stream_object(self) -> Option<ObjectRef> {
        match self.kind {
            XrefAnchorKind::Traditional => None,
            XrefAnchorKind::StreamObject(reference) => Some(reference),
        }
    }
}

/// Deterministic work charged by one xref-anchor classifier.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XrefAnchorStats {
    read_bytes: u64,
    parse_bytes: u64,
    attempts: u64,
}

impl XrefAnchorStats {
    /// Returns exact source bytes charged before the one logical read.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns exact source bytes charged before classification.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns the number of distinct anchor windows installed.
    pub const fn attempts(self) -> u64 {
        self.attempts
    }
}

/// Coarse state of one xref-anchor classifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefAnchorPhase {
    /// Waiting for or parsing the exact bounded anchor window.
    Anchor,
    /// The source-bound classification was returned.
    Complete,
    /// The classifier reached a stable terminal failure.
    Failed,
}

/// Result of polling one xref-anchor classifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XrefAnchorPoll {
    /// A complete source-bound anchor classification is ready.
    Ready(XrefAnchor),
    /// Required anchor bytes are absent and the runtime must retain the checkpoint.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical exact missing ranges.
        missing: SmallRanges,
        /// The anchor checkpoint to requeue.
        checkpoint: ResumeCheckpoint,
    },
    /// The classifier reached a stable terminal failure.
    Failed(XrefError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobState {
    Anchor { charged: bool },
    Complete,
    Failed(XrefError),
}

/// One-shot, bounded Range-resumable classifier for a declared xref anchor.
///
/// The stream-object result proves only an exact indirect-object header. Object framing and
/// `/Type /XRef` validation remain separate later boundaries.
#[derive(Debug)]
pub struct OpenXrefAnchorJob {
    snapshot: SourceSnapshot,
    startxref: u64,
    upper_bound: u64,
    range: ByteRange,
    context: XrefAnchorJobContext,
    limits: XrefAnchorLimits,
    stats: XrefAnchorStats,
    state: JobState,
}

impl OpenXrefAnchorJob {
    /// Validates source geometry and creates an exact anchor-classification job.
    pub fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        upper_bound: u64,
        context: XrefAnchorJobContext,
        limits: XrefAnchorLimits,
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
        let available = upper_bound
            .checked_sub(startxref)
            .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
        let range = ByteRange::new(startxref, limits.max_anchor_bytes.min(available))
            .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
        Ok(Self {
            snapshot,
            startxref,
            upper_bound,
            range,
            context,
            limits,
            stats: XrefAnchorStats::default(),
            state: JobState::Anchor { charged: false },
        })
    }

    /// Returns the immutable snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact declared xref anchor.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the exclusive physical bound supplied by the caller.
    pub const fn upper_bound(&self) -> u64 {
        self.upper_bound
    }

    /// Returns runtime identity and the anchor checkpoint.
    pub const fn context(&self) -> XrefAnchorJobContext {
        self.context
    }

    /// Returns the validated anchor-classification limits.
    pub const fn limits(&self) -> XrefAnchorLimits {
        self.limits
    }

    /// Returns cumulative work through the latest poll.
    pub const fn stats(&self) -> XrefAnchorStats {
        self.stats
    }

    /// Returns the current coarse classifier phase.
    pub const fn phase(&self) -> XrefAnchorPhase {
        match self.state {
            JobState::Anchor { .. } => XrefAnchorPhase::Anchor,
            JobState::Complete => XrefAnchorPhase::Complete,
            JobState::Failed(_) => XrefAnchorPhase::Failed,
        }
    }

    /// Advances classification without file, network, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn XrefCancellation + '_),
    ) -> XrefAnchorPoll {
        let charged = match self.state {
            JobState::Anchor { charged } => charged,
            JobState::Complete => {
                return XrefAnchorPoll::Failed(XrefError::for_code(
                    XrefErrorCode::JobAlreadyComplete,
                    Some(self.startxref),
                ));
            }
            JobState::Failed(error) => return XrefAnchorPoll::Failed(error),
        };
        if source.snapshot() != self.snapshot {
            return self.fail(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
        }
        if cancellation.is_cancelled() {
            return self.fail(XrefError::for_code(
                XrefErrorCode::Cancelled,
                Some(self.startxref),
            ));
        }
        if !charged {
            self.stats.read_bytes = self.range.len();
            self.stats.attempts = 1;
            self.state = JobState::Anchor { charged: true };
        }
        let request = ReadRequest::new(
            self.range,
            RequestPriority::Metadata,
            self.context.job,
            self.context.checkpoint,
        );
        match source.poll(request) {
            ReadPoll::Pending { ticket, missing } => XrefAnchorPoll::Pending {
                ticket,
                missing,
                checkpoint: self.context.checkpoint,
            },
            ReadPoll::EndOfFile => self.fail(XrefError::for_code(
                XrefErrorCode::UnexpectedEndOfSource,
                Some(self.startxref),
            )),
            ReadPoll::Failed(error) => self.fail(XrefError::from_source(error)),
            ReadPoll::Ready(bytes) => {
                if let Err(error) = self.validate_slice(&bytes) {
                    return self.fail(error);
                }
                self.stats.parse_bytes = self.range.len();
                let extent = match self.snapshot.len() {
                    Some(source_len) if self.range.end_exclusive() == source_len => {
                        InputExtent::KnownSourceEnd
                    }
                    Some(_) | None => InputExtent::MayContinue,
                };
                match parse_anchor(
                    self.snapshot,
                    self.startxref,
                    bytes.bytes(),
                    extent,
                    cancellation,
                ) {
                    Ok(AnchorParse::Ready { kind, header_span }) => {
                        self.state = JobState::Complete;
                        XrefAnchorPoll::Ready(XrefAnchor {
                            snapshot: self.snapshot,
                            startxref: self.startxref,
                            header_span,
                            kind,
                        })
                    }
                    Ok(AnchorParse::NeedMore) if self.range.end_exclusive() == self.upper_bound => {
                        self.fail(invalid_anchor(self.startxref))
                    }
                    Ok(AnchorParse::NeedMore) => self.fail(XrefError::resource(
                        XrefLimitKind::AnchorBytes,
                        self.limits.max_anchor_bytes,
                        self.range.len(),
                        1,
                        Some(self.startxref),
                    )),
                    Err(error) => self.fail(error),
                }
            }
        }
    }

    fn validate_slice(&self, bytes: &ByteSlice) -> Result<(), XrefError> {
        if bytes.identity() != self.snapshot.identity() {
            return Err(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
        }
        if bytes.range() != self.range {
            return Err(XrefError::for_code(
                XrefErrorCode::InternalState,
                Some(self.startxref),
            ));
        }
        Ok(())
    }

    fn fail(&mut self, error: XrefError) -> XrefAnchorPoll {
        self.state = JobState::Failed(error);
        XrefAnchorPoll::Failed(error)
    }
}

enum AnchorParse {
    Ready {
        kind: XrefAnchorKind,
        header_span: ByteSpan,
    },
    NeedMore,
}

struct SyntaxCancellationAdapter<'a>(&'a dyn XrefCancellation);

impl SyntaxCancellation for SyntaxCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn parse_anchor(
    snapshot: SourceSnapshot,
    startxref: u64,
    bytes: &[u8],
    extent: InputExtent,
    cancellation: &dyn XrefCancellation,
) -> Result<AnchorParse, XrefError> {
    if cancellation.is_cancelled() {
        return Err(XrefError::for_code(
            XrefErrorCode::Cancelled,
            Some(startxref),
        ));
    }
    if bytes.len() < 4 {
        return if b"xref".starts_with(bytes) && extent == InputExtent::MayContinue {
            Ok(AnchorParse::NeedMore)
        } else {
            Err(invalid_anchor(startxref))
        };
    }
    if &bytes[..4] == b"xref" {
        let mut delimiter_index = 4_usize;
        while bytes
            .get(delimiter_index)
            .is_some_and(|byte| is_horizontal_whitespace(*byte))
        {
            delimiter_index += 1;
            if delimiter_index.is_multiple_of(256) && cancellation.is_cancelled() {
                return Err(XrefError::for_code(
                    XrefErrorCode::Cancelled,
                    Some(startxref),
                ));
            }
        }
        let Some(delimiter) = bytes.get(delimiter_index).copied() else {
            return if extent == InputExtent::MayContinue {
                Ok(AnchorParse::NeedMore)
            } else {
                Err(invalid_anchor(startxref))
            };
        };
        let header_len = match delimiter {
            b'\n' => u64::try_from(delimiter_index + 1)
                .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?,
            b'\r' => {
                if bytes.get(delimiter_index + 1) == Some(&b'\n') {
                    u64::try_from(delimiter_index + 2).map_err(|_| {
                        XrefError::for_code(XrefErrorCode::InternalState, Some(startxref))
                    })?
                } else if bytes.len() == delimiter_index + 1 && extent == InputExtent::MayContinue {
                    return Ok(AnchorParse::NeedMore);
                } else {
                    u64::try_from(delimiter_index + 1).map_err(|_| {
                        XrefError::for_code(XrefErrorCode::InternalState, Some(startxref))
                    })?
                }
            }
            _ => return Err(invalid_anchor(startxref)),
        };
        let header_span = ByteSpan::new(startxref, header_len)
            .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
        return Ok(AnchorParse::Ready {
            kind: XrefAnchorKind::Traditional,
            header_span,
        });
    }
    if !matches!(bytes[0], b'+' | b'-') && !bytes[0].is_ascii_digit() {
        return Err(invalid_anchor(startxref));
    }

    let input = SyntaxInput::new(snapshot.identity(), startxref, bytes, extent)
        .map_err(XrefError::from_syntax)?;
    let adapter = SyntaxCancellationAdapter(cancellation);
    let mut parser = SyntaxParser::new_with_cancellation(input, Default::default(), &adapter)
        .map_err(XrefError::from_syntax)?;
    let number = required(parser.parse_object(), startxref)?;
    let Some(number) = number else {
        return Ok(AnchorParse::NeedMore);
    };
    if number.span().start() != startxref {
        return Err(invalid_anchor(startxref));
    }
    let Some(number) = number
        .value()
        .as_integer()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value != 0)
    else {
        return Err(invalid_anchor(startxref));
    };
    let generation = required(parser.parse_object(), startxref)?;
    let Some(generation) = generation else {
        return Ok(AnchorParse::NeedMore);
    };
    let Some(generation) = generation
        .value()
        .as_integer()
        .and_then(|value| u16::try_from(value).ok())
    else {
        return Err(invalid_anchor(startxref));
    };
    let keyword = required(parser.expect_keyword(b"obj"), startxref)?;
    let Some(keyword) = keyword else {
        return Ok(AnchorParse::NeedMore);
    };
    let reference = ObjectRef::new(number, generation).map_err(|_| invalid_anchor(startxref))?;
    let header_len = keyword
        .end_exclusive()
        .checked_sub(startxref)
        .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
    let header_span = ByteSpan::new(startxref, header_len)
        .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(startxref)))?;
    Ok(AnchorParse::Ready {
        kind: XrefAnchorKind::StreamObject(reference),
        header_span,
    })
}

fn required<T>(poll: SyntaxPoll<T>, offset: u64) -> Result<Option<T>, XrefError> {
    match poll {
        SyntaxPoll::Ready(value) => Ok(Some(value)),
        SyntaxPoll::NeedMore { .. } => Ok(None),
        SyntaxPoll::EndOfInput => Err(invalid_anchor(offset)),
        SyntaxPoll::Failed(error) if error.category() == SyntaxErrorCategory::Syntax => {
            Err(invalid_anchor(offset))
        }
        SyntaxPoll::Failed(error) => Err(XrefError::from_syntax(error)),
    }
}

fn invalid_anchor(offset: u64) -> XrefError {
    XrefError::for_code(XrefErrorCode::InvalidXrefAnchor, Some(offset))
}
