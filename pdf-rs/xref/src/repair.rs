use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceIdentity, SourceSnapshot,
};
use pdf_rs_syntax::{InputExtent, ObjectRef, PdfDictionary, SyntaxLimits};

use crate::parser::{SectionWindow, parse_section};
use crate::{
    OpenXrefJob, XrefCancellation, XrefEntry, XrefError, XrefErrorCategory, XrefErrorCode,
    XrefJobContext, XrefLimitKind, XrefLimits, XrefPhase, XrefSection, XrefStats,
};

const HARD_MAX_STARTXREF_DELTA: u64 = 64 * 1024;
const HARD_MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_WORKING_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_CANDIDATES: u64 = 256;
const HARD_MAX_WHITESPACE_EDITS: u64 = 4096;
const HARD_MAX_REPAIRS: u64 = 4096;
const HARD_MAX_DIAGNOSTIC_BYTES: u64 = 1024 * 1024;

/// Caller-configurable deterministic ceilings for explicit local xref repair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefRepairLimitConfig {
    /// Maximum absolute distance from the rejected final `startxref` declaration.
    pub max_startxref_delta: u64,
    /// Cumulative bytes read and examined after strict opening fails.
    pub max_scan_bytes: u64,
    /// Peak canonical section copy plus row-evidence capacity for one candidate.
    pub max_working_bytes: u64,
    /// Token-boundary `xref` anchors retained from the local scan window.
    pub max_candidates: u64,
    /// Fixed-width row whitespace bytes that may be canonicalized.
    pub max_whitespace_edits: u64,
    /// Diagnostics retained with one locally opened section.
    pub max_repairs: u64,
    /// Allocator-reported capacity retained by repair diagnostics.
    pub max_diagnostic_bytes: u64,
}

impl Default for XrefRepairLimitConfig {
    fn default() -> Self {
        Self {
            max_startxref_delta: 1024,
            max_scan_bytes: 16 * 1024 * 1024,
            max_working_bytes: 2 * 1024 * 1024,
            max_candidates: 8,
            max_whitespace_edits: 256,
            max_repairs: 256,
            max_diagnostic_bytes: 64 * 1024,
        }
    }
}

/// Validated local-xref repair ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefRepairLimits {
    max_startxref_delta: u64,
    max_scan_bytes: u64,
    max_working_bytes: u64,
    max_candidates: u64,
    max_whitespace_edits: u64,
    max_repairs: u64,
    max_diagnostic_bytes: u64,
}

impl XrefRepairLimits {
    /// Validates positive values beneath fixed hard ceilings.
    pub fn validate(config: XrefRepairLimitConfig) -> Result<Self, XrefError> {
        if config.max_startxref_delta == 0
            || config.max_startxref_delta > HARD_MAX_STARTXREF_DELTA
            || config.max_scan_bytes == 0
            || config.max_scan_bytes > HARD_MAX_SCAN_BYTES
            || config.max_working_bytes == 0
            || config.max_working_bytes > HARD_MAX_WORKING_BYTES
            || config.max_candidates == 0
            || config.max_candidates > HARD_MAX_CANDIDATES
            || config.max_whitespace_edits == 0
            || config.max_whitespace_edits > HARD_MAX_WHITESPACE_EDITS
            || config.max_repairs == 0
            || config.max_repairs > HARD_MAX_REPAIRS
            || config.max_diagnostic_bytes == 0
            || config.max_diagnostic_bytes > HARD_MAX_DIAGNOSTIC_BYTES
        {
            return Err(XrefError::for_code(
                XrefErrorCode::InvalidRepairLimits,
                None,
            ));
        }
        Ok(Self {
            max_startxref_delta: config.max_startxref_delta,
            max_scan_bytes: config.max_scan_bytes,
            max_working_bytes: config.max_working_bytes,
            max_candidates: config.max_candidates,
            max_whitespace_edits: config.max_whitespace_edits,
            max_repairs: config.max_repairs,
            max_diagnostic_bytes: config.max_diagnostic_bytes,
        })
    }

    /// Returns the accepted absolute final-anchor deviation.
    pub const fn max_startxref_delta(self) -> u64 {
        self.max_startxref_delta
    }

    /// Returns the cumulative local scan-byte ceiling.
    pub const fn max_scan_bytes(self) -> u64 {
        self.max_scan_bytes
    }

    /// Returns the peak repair workspace ceiling.
    pub const fn max_working_bytes(self) -> u64 {
        self.max_working_bytes
    }

    /// Returns the local anchor-candidate ceiling.
    pub const fn max_candidates(self) -> u64 {
        self.max_candidates
    }

    /// Returns the fixed-width whitespace-edit ceiling.
    pub const fn max_whitespace_edits(self) -> u64 {
        self.max_whitespace_edits
    }

    /// Returns the repair-diagnostic count ceiling.
    pub const fn max_repairs(self) -> u64 {
        self.max_repairs
    }

    /// Returns the retained diagnostic-capacity ceiling.
    pub const fn max_diagnostic_bytes(self) -> u64 {
        self.max_diagnostic_bytes
    }
}

impl Default for XrefRepairLimits {
    fn default() -> Self {
        Self::validate(XrefRepairLimitConfig::default())
            .expect("built-in xref repair limits satisfy hard ceilings")
    }
}

/// Strict-child identity plus repair-only phase checkpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalXrefJobContext {
    strict: XrefJobContext,
    anchor_scan_checkpoint: ResumeCheckpoint,
    candidate_section_checkpoint: ResumeCheckpoint,
}

impl LocalXrefJobContext {
    /// Creates an explicit R1 context without changing the strict child context.
    pub const fn new(
        strict: XrefJobContext,
        anchor_scan_checkpoint: ResumeCheckpoint,
        candidate_section_checkpoint: ResumeCheckpoint,
    ) -> Self {
        Self {
            strict,
            anchor_scan_checkpoint,
            candidate_section_checkpoint,
        }
    }

    /// Returns the unmodified R0 strict-child context.
    pub const fn strict(self) -> XrefJobContext {
        self.strict
    }

    /// Returns the checkpoint for the bounded local anchor scan.
    pub const fn anchor_scan_checkpoint(self) -> ResumeCheckpoint {
        self.anchor_scan_checkpoint
    }

    /// Returns the checkpoint for repaired-candidate normal validation.
    pub const fn candidate_section_checkpoint(self) -> ResumeCheckpoint {
        self.candidate_section_checkpoint
    }
}

/// Machine-readable local xref repair action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefRepairKind {
    /// The final tail declaration was adjusted to a nearby unique traditional-xref anchor.
    StartXrefOffset,
    /// PDF horizontal whitespace in one fixed-width traditional row was canonicalized.
    EntryWhitespace,
}

/// Source-redacted, source-bound evidence for one xref repair action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefRepairDiagnostic {
    snapshot: SourceSnapshot,
    kind: XrefRepairKind,
    declared_startxref: u64,
    effective_startxref: u64,
    subject_offset: u64,
    scan_bytes: u64,
    candidates_examined: u64,
    whitespace_edits: u64,
}

impl XrefRepairDiagnostic {
    const fn offset(
        snapshot: SourceSnapshot,
        declared_startxref: u64,
        effective_startxref: u64,
        scan_bytes: u64,
        candidates_examined: u64,
    ) -> Self {
        Self {
            snapshot,
            kind: XrefRepairKind::StartXrefOffset,
            declared_startxref,
            effective_startxref,
            subject_offset: declared_startxref,
            scan_bytes,
            candidates_examined,
            whitespace_edits: 0,
        }
    }

    const fn whitespace(
        snapshot: SourceSnapshot,
        declared_startxref: u64,
        effective_startxref: u64,
        row_offset: u64,
        scan_bytes: u64,
        whitespace_edits: u64,
    ) -> Self {
        Self {
            snapshot,
            kind: XrefRepairKind::EntryWhitespace,
            declared_startxref,
            effective_startxref,
            subject_offset: row_offset,
            scan_bytes,
            candidates_examined: 1,
            whitespace_edits,
        }
    }

    /// Returns the stable diagnostic identifier for this action.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            XrefRepairKind::StartXrefOffset => "RPE-XREF-REPAIR-0001",
            XrefRepairKind::EntryWhitespace => "RPE-XREF-REPAIR-0002",
        }
    }

    /// Returns the immutable snapshot that supplied every examined byte.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the repair action kind.
    pub const fn kind(self) -> XrefRepairKind {
        self.kind
    }

    /// Returns the final anchor declared in the source tail.
    pub const fn declared_startxref(self) -> u64 {
        self.declared_startxref
    }

    /// Returns the unique anchor that passed normal section validation.
    pub const fn effective_startxref(self) -> u64 {
        self.effective_startxref
    }

    /// Returns the repaired anchor or row source offset.
    pub const fn subject_offset(self) -> u64 {
        self.subject_offset
    }

    /// Returns source bytes examined for this repair decision.
    pub const fn scan_bytes(self) -> u64 {
        self.scan_bytes
    }

    /// Returns local anchor candidates considered for this action.
    pub const fn candidates_examined(self) -> u64 {
        self.candidates_examined
    }

    /// Returns fixed-width whitespace bytes changed for this action.
    pub const fn whitespace_edits(self) -> u64 {
        self.whitespace_edits
    }
}

/// Cumulative strict and repair work charged by one local-xref job.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XrefRepairStats {
    strict: XrefStats,
    repair_scan_bytes: u64,
    candidates: u64,
    candidate_section_attempts: u64,
    repairs: u64,
    whitespace_edits: u64,
    repair_working_bytes: u64,
    diagnostic_bytes: u64,
}

impl XrefRepairStats {
    /// Returns work charged by the unchanged strict child.
    pub const fn strict(self) -> XrefStats {
        self.strict
    }

    /// Returns cumulative repair-only source bytes read and examined.
    pub const fn repair_scan_bytes(self) -> u64 {
        self.repair_scan_bytes
    }

    /// Returns token-boundary anchor candidates retained from the local scan.
    pub const fn candidates(self) -> u64 {
        self.candidates
    }

    /// Returns distinct repaired-candidate section windows requested.
    pub const fn candidate_section_attempts(self) -> u64 {
        self.candidate_section_attempts
    }

    /// Returns repair diagnostics retained or considered before terminal failure.
    pub const fn repairs(self) -> u64 {
        self.repairs
    }

    /// Returns noncanonical fixed-row whitespace bytes canonicalized.
    pub const fn whitespace_edits(self) -> u64 {
        self.whitespace_edits
    }

    /// Returns peak canonical-section plus row-evidence capacity in bytes.
    pub const fn repair_working_bytes(self) -> u64 {
        self.repair_working_bytes
    }

    /// Returns allocator-reported diagnostic capacity in bytes.
    pub const fn diagnostic_bytes(self) -> u64 {
        self.diagnostic_bytes
    }
}

/// Proof-bearing R1 result that cannot be mistaken for a bare strict section.
pub struct LocallyParsedXrefSection {
    section: XrefSection,
    declared_startxref: u64,
    diagnostics: Vec<XrefRepairDiagnostic>,
    stats: XrefRepairStats,
}

impl LocallyParsedXrefSection {
    fn new(
        section: XrefSection,
        declared_startxref: u64,
        diagnostics: Vec<XrefRepairDiagnostic>,
        stats: XrefRepairStats,
    ) -> Self {
        Self {
            section,
            declared_startxref,
            diagnostics,
            stats,
        }
    }

    /// Returns the source snapshot for both the section and every diagnostic.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.section.snapshot()
    }

    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.section.source()
    }

    /// Returns the final anchor originally declared in the tail.
    pub const fn declared_startxref(&self) -> u64 {
        self.declared_startxref
    }

    /// Returns the strict or locally repaired anchor used by normal validation.
    pub const fn effective_startxref(&self) -> u64 {
        self.section.startxref()
    }

    /// Returns the exact normally validated section span.
    pub const fn span(&self) -> pdf_rs_syntax::ByteSpan {
        self.section.span()
    }

    /// Returns the normally validated trailer `/Size`.
    pub const fn declared_size(&self) -> u32 {
        self.section.declared_size()
    }

    /// Returns immutable repair diagnostics; canonical strict success has an empty slice.
    pub fn diagnostics(&self) -> &[XrefRepairDiagnostic] {
        &self.diagnostics
    }

    /// Returns all strict-child and repair-only work charged before publication.
    pub const fn stats(&self) -> XrefRepairStats {
        self.stats
    }

    /// Returns the normally validated trailer root.
    pub const fn root(&self) -> ObjectRef {
        self.section.root()
    }

    /// Returns normally validated entries in object-number order.
    pub fn entries(&self) -> &[XrefEntry] {
        self.section.entries()
    }

    /// Looks up one normally validated object-number row.
    pub fn entry(&self, object_number: u32) -> Option<&XrefEntry> {
        self.section.entry(object_number)
    }

    /// Returns the normally validated trailer dictionary.
    pub const fn trailer(&self) -> &pdf_rs_syntax::Located<PdfDictionary> {
        self.section.trailer()
    }
}

impl fmt::Debug for LocallyParsedXrefSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocallyParsedXrefSection")
            .field("source", &self.section.source())
            .field("declared_startxref", &self.declared_startxref)
            .field("effective_startxref", &self.section.startxref())
            .field("entry_count", &self.section.entries().len())
            .field("diagnostics", &self.diagnostics)
            .field("stats", &self.stats)
            .field("trailer", &"[REDACTED]")
            .finish()
    }
}

/// Coarse phase of an explicit local-xref job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalXrefPhase {
    /// Running the unchanged R0 strict child.
    Strict(XrefPhase),
    /// Scanning only around the rejected final anchor.
    AnchorScan,
    /// Revalidating bounded local candidates through the normal section parser.
    CandidateSection,
    /// One proof-bearing result was returned.
    Complete,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Result of polling one explicit R1 local-xref job.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "proof-bearing ready values and retained lower errors stay inline without an unbudgeted box"
)]
pub enum LocalXrefPoll {
    /// A canonical or locally repaired section is ready with inseparable evidence.
    Ready(LocallyParsedXrefSection),
    /// Required source bytes are absent.
    Pending {
        /// One-shot source ticket.
        ticket: DataTicket,
        /// Canonical missing exact ranges.
        missing: SmallRanges,
        /// Exact strict or repair checkpoint to requeue.
        checkpoint: ResumeCheckpoint,
    },
    /// The strict child or bounded repair reached a stable failure.
    Failed(XrefError),
}

struct CandidateResult {
    section: XrefSection,
    diagnostics: Vec<XrefRepairDiagnostic>,
}

#[derive(Clone, Copy)]
struct CandidateContext {
    declared: u64,
    candidate: u64,
    anchor_scan_bytes: u64,
    section_scan_bytes: usize,
    build_evidence: bool,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot state owns its bounded selected proof without an unbudgeted box"
)]
enum RepairState {
    Strict,
    AnchorScan {
        declared: u64,
        range: ByteRange,
        charged: bool,
    },
    CandidateSection {
        declared: u64,
        candidates: Vec<u64>,
        index: usize,
        window: u64,
        charged: bool,
        selected: Option<CandidateResult>,
        anchor_scan_bytes: u64,
    },
    Transition,
    Complete,
    Failed(XrefError),
}

/// Explicit R1 sibling that first exhausts the unchanged strict xref job.
pub struct OpenLocalXrefJob {
    snapshot: SourceSnapshot,
    source_len: u64,
    context: LocalXrefJobContext,
    xref_limits: XrefLimits,
    repair_limits: XrefRepairLimits,
    syntax_limits: SyntaxLimits,
    strict: OpenXrefJob,
    stats: XrefRepairStats,
    state: RepairState,
}

impl OpenLocalXrefJob {
    /// Creates an explicit local-repair job with pairwise-distinct checkpoints.
    pub fn new(
        snapshot: SourceSnapshot,
        context: LocalXrefJobContext,
        xref_limits: XrefLimits,
        repair_limits: XrefRepairLimits,
        syntax_limits: SyntaxLimits,
    ) -> Result<Self, XrefError> {
        let checkpoints = [
            context.strict().tail_checkpoint(),
            context.strict().section_checkpoint(),
            context.anchor_scan_checkpoint(),
            context.candidate_section_checkpoint(),
        ];
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[..index].contains(checkpoint) {
                return Err(XrefError::for_code(
                    XrefErrorCode::InvalidRepairJobContext,
                    None,
                ));
            }
        }
        let strict = OpenXrefJob::new(snapshot, context.strict(), xref_limits, syntax_limits)?;
        let source_len = snapshot
            .len()
            .ok_or_else(|| XrefError::for_code(XrefErrorCode::UnknownSourceLength, None))?;
        Ok(Self {
            snapshot,
            source_len,
            context,
            xref_limits,
            repair_limits,
            syntax_limits,
            strict,
            stats: XrefRepairStats::default(),
            state: RepairState::Strict,
        })
    }

    /// Returns the immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns strict and repair phase identities.
    pub const fn context(&self) -> LocalXrefJobContext {
        self.context
    }

    /// Returns the validated local-repair ceilings.
    pub const fn repair_limits(&self) -> XrefRepairLimits {
        self.repair_limits
    }

    /// Returns cumulative strict-child and repair-only work.
    pub fn stats(&self) -> XrefRepairStats {
        XrefRepairStats {
            strict: self.strict.stats(),
            ..self.stats
        }
    }

    /// Returns the current coarse phase.
    pub const fn phase(&self) -> LocalXrefPhase {
        match self.state {
            RepairState::Strict => LocalXrefPhase::Strict(self.strict.phase()),
            RepairState::AnchorScan { .. } => LocalXrefPhase::AnchorScan,
            RepairState::CandidateSection { .. } => LocalXrefPhase::CandidateSection,
            RepairState::Complete => LocalXrefPhase::Complete,
            RepairState::Failed(_) | RepairState::Transition => LocalXrefPhase::Failed,
        }
    }

    /// Advances strict opening or explicit bounded repair without performing host I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn XrefCancellation + '_),
    ) -> LocalXrefPoll {
        match self.state {
            RepairState::Complete => {
                return LocalXrefPoll::Failed(XrefError::for_code(
                    XrefErrorCode::JobAlreadyComplete,
                    None,
                ));
            }
            RepairState::Failed(error) => return LocalXrefPoll::Failed(error),
            RepairState::Strict
            | RepairState::AnchorScan { .. }
            | RepairState::CandidateSection { .. } => {}
            RepairState::Transition => {
                return self.fail(XrefError::for_code(XrefErrorCode::InternalState, None));
            }
        }

        loop {
            if source.snapshot() != self.snapshot {
                return self.fail(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
            }
            if cancellation.is_cancelled() {
                return self.fail(XrefError::for_code(XrefErrorCode::Cancelled, None));
            }

            let state = mem::replace(&mut self.state, RepairState::Transition);
            match state {
                RepairState::Strict => match self.strict.poll(source, cancellation) {
                    crate::XrefPoll::Ready(section) => {
                        let declared = section.startxref();
                        self.state = RepairState::Complete;
                        return LocalXrefPoll::Ready(LocallyParsedXrefSection::new(
                            section,
                            declared,
                            Vec::new(),
                            self.stats(),
                        ));
                    }
                    crate::XrefPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    } => {
                        self.state = RepairState::Strict;
                        return LocalXrefPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        };
                    }
                    crate::XrefPoll::Failed(error)
                        if matches!(
                            error.code(),
                            XrefErrorCode::InvalidEntry | XrefErrorCode::InvalidXrefKeyword
                        ) =>
                    {
                        let Some(declared) = self.strict.discovered_startxref() else {
                            return self.fail(error);
                        };
                        let range = match repair_anchor_range(
                            declared,
                            self.source_len,
                            self.repair_limits.max_startxref_delta,
                        ) {
                            Ok(range) => range,
                            Err(error) => return self.fail(error),
                        };
                        self.state = RepairState::AnchorScan {
                            declared,
                            range,
                            charged: false,
                        };
                    }
                    crate::XrefPoll::Failed(error) => return self.fail(error),
                },
                RepairState::AnchorScan {
                    declared,
                    range,
                    charged,
                } => {
                    if !charged
                        && let Err(error) = self.charge_scan(range.len(), Some(range.start()))
                    {
                        return self.fail(error);
                    }
                    let request = ReadRequest::new(
                        range,
                        RequestPriority::Metadata,
                        self.context.strict().job(),
                        self.context.anchor_scan_checkpoint(),
                    );
                    match source.poll(request) {
                        ReadPoll::Pending { ticket, missing } => {
                            self.state = RepairState::AnchorScan {
                                declared,
                                range,
                                charged: true,
                            };
                            return LocalXrefPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.anchor_scan_checkpoint(),
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(XrefError::for_code(
                                XrefErrorCode::UnexpectedEndOfSource,
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(XrefError::from_source(error));
                        }
                        ReadPoll::Ready(bytes) => {
                            if let Err(error) = self.validate_slice(&bytes, range) {
                                return self.fail(error);
                            }
                            let candidates = match scan_xref_anchors(
                                &bytes,
                                declared,
                                self.repair_limits,
                                cancellation,
                            ) {
                                Ok(candidates) => candidates,
                                Err(error) => return self.fail(error),
                            };
                            self.stats.candidates = match u64::try_from(candidates.len()) {
                                Ok(value) => value,
                                Err(_) => {
                                    return self.fail(XrefError::for_code(
                                        XrefErrorCode::InternalState,
                                        Some(declared),
                                    ));
                                }
                            };
                            if candidates.is_empty() {
                                return self.fail(XrefError::for_code(
                                    XrefErrorCode::LocalRepairFailed,
                                    Some(declared),
                                ));
                            }
                            let first = candidates[0];
                            let remaining = self.source_len - first;
                            self.state = RepairState::CandidateSection {
                                declared,
                                candidates,
                                index: 0,
                                window: self.xref_limits.initial_section_bytes().min(remaining),
                                charged: false,
                                selected: None,
                                anchor_scan_bytes: range.len(),
                            };
                        }
                    }
                }
                RepairState::CandidateSection {
                    declared,
                    candidates,
                    index,
                    window,
                    charged,
                    selected,
                    anchor_scan_bytes,
                } => {
                    if index >= candidates.len() {
                        let Some(selected) = selected else {
                            return self.fail(XrefError::for_code(
                                XrefErrorCode::LocalRepairFailed,
                                Some(declared),
                            ));
                        };
                        self.state = RepairState::Complete;
                        return LocalXrefPoll::Ready(LocallyParsedXrefSection::new(
                            selected.section,
                            declared,
                            selected.diagnostics,
                            self.stats(),
                        ));
                    }
                    let candidate = candidates[index];
                    let range = match ByteRange::new(candidate, window) {
                        Ok(range) if range.end_exclusive() <= self.source_len => range,
                        _ => {
                            return self.fail(XrefError::for_code(
                                XrefErrorCode::InternalState,
                                Some(candidate),
                            ));
                        }
                    };
                    if !charged {
                        if let Err(error) = self.charge_scan(window, Some(candidate)) {
                            return self.fail(error);
                        }
                        self.stats.candidate_section_attempts =
                            match self.stats.candidate_section_attempts.checked_add(1) {
                                Some(value) => value,
                                None => {
                                    return self.fail(XrefError::for_code(
                                        XrefErrorCode::InternalState,
                                        Some(candidate),
                                    ));
                                }
                            };
                    }
                    let request = ReadRequest::new(
                        range,
                        RequestPriority::Metadata,
                        self.context.strict().job(),
                        self.context.candidate_section_checkpoint(),
                    );
                    match source.poll(request) {
                        ReadPoll::Pending { ticket, missing } => {
                            self.state = RepairState::CandidateSection {
                                declared,
                                candidates,
                                index,
                                window,
                                charged: true,
                                selected,
                                anchor_scan_bytes,
                            };
                            return LocalXrefPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.candidate_section_checkpoint(),
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(XrefError::for_code(
                                XrefErrorCode::UnexpectedEndOfSource,
                                Some(candidate),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(XrefError::from_source(error));
                        }
                        ReadPoll::Ready(bytes) => {
                            if let Err(error) = self.validate_slice(&bytes, range) {
                                return self.fail(error);
                            }
                            let extent = if range.end_exclusive() == self.source_len {
                                InputExtent::KnownSourceEnd
                            } else {
                                InputExtent::MayContinue
                            };
                            let outcome = match self.parse_candidate(
                                CandidateContext {
                                    declared,
                                    candidate,
                                    anchor_scan_bytes,
                                    section_scan_bytes: bytes.bytes().len(),
                                    build_evidence: selected.is_none(),
                                },
                                bytes.bytes(),
                                extent,
                                cancellation,
                            ) {
                                Ok(outcome) => outcome,
                                Err(error) => return self.fail(error),
                            };
                            match outcome {
                                CandidateParse::NeedMore => {
                                    let remaining = self.source_len - candidate;
                                    let cap = self.xref_limits.max_section_bytes().min(remaining);
                                    if window < cap {
                                        let Some(next) = grow_window(window, cap) else {
                                            return self.fail(XrefError::for_code(
                                                XrefErrorCode::InternalState,
                                                Some(candidate),
                                            ));
                                        };
                                        self.state = RepairState::CandidateSection {
                                            declared,
                                            candidates,
                                            index,
                                            window: next,
                                            charged: false,
                                            selected,
                                            anchor_scan_bytes,
                                        };
                                    } else {
                                        self.advance_candidate(
                                            declared,
                                            candidates,
                                            index,
                                            selected,
                                            anchor_scan_bytes,
                                        );
                                    }
                                }
                                CandidateParse::Invalid => self.advance_candidate(
                                    declared,
                                    candidates,
                                    index,
                                    selected,
                                    anchor_scan_bytes,
                                ),
                                CandidateParse::Valid(result) => {
                                    if selected.is_some() {
                                        return self.fail(XrefError::for_code(
                                            XrefErrorCode::AmbiguousRepair,
                                            Some(declared),
                                        ));
                                    }
                                    self.advance_candidate(
                                        declared,
                                        candidates,
                                        index,
                                        Some(result),
                                        anchor_scan_bytes,
                                    );
                                }
                            }
                        }
                    }
                }
                RepairState::Complete => {
                    return self.fail(XrefError::for_code(XrefErrorCode::JobAlreadyComplete, None));
                }
                RepairState::Failed(error) => return LocalXrefPoll::Failed(error),
                RepairState::Transition => {
                    return self.fail(XrefError::for_code(XrefErrorCode::InternalState, None));
                }
            }
        }
    }

    fn parse_candidate(
        &mut self,
        context: CandidateContext,
        bytes: &[u8],
        extent: InputExtent,
        cancellation: &dyn XrefCancellation,
    ) -> Result<CandidateParse, XrefError> {
        let parsed = parse_section(
            SectionWindow::new(
                self.snapshot,
                context.candidate,
                bytes,
                extent,
                self.source_len,
            ),
            self.xref_limits,
            self.syntax_limits,
            cancellation,
        );
        match parsed {
            Ok(Some(section)) => self.valid_candidate(section, context, &[], 0),
            Ok(None) => Ok(CandidateParse::NeedMore),
            Err(error) if error.code() == XrefErrorCode::InvalidEntry => {
                let normalized = normalize_fixed_width_rows(
                    bytes,
                    context.candidate,
                    cancellation,
                    self.repair_limits,
                    RowEvidenceBudget {
                        retain: context.build_evidence,
                        offset_diagnostics: u64::from(context.candidate != context.declared),
                        consumed_repairs: self.stats.repairs,
                        consumed_whitespace_edits: self.stats.whitespace_edits,
                        consumed_diagnostic_bytes: self.stats.diagnostic_bytes,
                    },
                )?;
                let NormalizedRows::Changed {
                    bytes: normalized_bytes,
                    rows,
                    whitespace_edits,
                    working_bytes,
                } = normalized
                else {
                    return Ok(CandidateParse::Invalid);
                };
                self.stats.repair_working_bytes =
                    self.stats.repair_working_bytes.max(working_bytes);
                match parse_section(
                    SectionWindow::new(
                        self.snapshot,
                        context.candidate,
                        &normalized_bytes,
                        extent,
                        self.source_len,
                    ),
                    self.xref_limits,
                    self.syntax_limits,
                    cancellation,
                ) {
                    Ok(Some(section)) => {
                        self.valid_candidate(section, context, &rows, whitespace_edits)
                    }
                    Ok(None) => Ok(CandidateParse::NeedMore),
                    Err(error) if candidate_error_is_malformed(error) => {
                        Ok(CandidateParse::Invalid)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) if candidate_error_is_malformed(error) => Ok(CandidateParse::Invalid),
            Err(error) => Err(error),
        }
    }

    fn valid_candidate(
        &mut self,
        section: XrefSection,
        context: CandidateContext,
        rows: &[RowRepair],
        whitespace_edits: u64,
    ) -> Result<CandidateParse, XrefError> {
        if !context.build_evidence {
            return Ok(CandidateParse::Valid(CandidateResult {
                section,
                diagnostics: Vec::new(),
            }));
        }
        if whitespace_edits != 0 {
            self.charge_whitespace_edits(whitespace_edits, Some(context.candidate))?;
        }
        let mut diagnostics = Vec::new();
        let section_scan_bytes = u64::try_from(context.section_scan_bytes).map_err(|_| {
            XrefError::for_code(XrefErrorCode::InternalState, Some(context.candidate))
        })?;
        if context.candidate != context.declared {
            let total_scan_bytes = context
                .anchor_scan_bytes
                .checked_add(section_scan_bytes)
                .ok_or_else(|| {
                    XrefError::for_code(XrefErrorCode::InternalState, Some(context.candidate))
                })?;
            self.push_diagnostic(
                &mut diagnostics,
                XrefRepairDiagnostic::offset(
                    self.snapshot,
                    context.declared,
                    context.candidate,
                    total_scan_bytes,
                    self.stats.candidates,
                ),
            )?;
        }
        for row in rows {
            self.push_diagnostic(
                &mut diagnostics,
                XrefRepairDiagnostic::whitespace(
                    self.snapshot,
                    context.declared,
                    context.candidate,
                    row.offset,
                    section_scan_bytes,
                    row.edits,
                ),
            )?;
        }
        if diagnostics.is_empty() {
            return Ok(CandidateParse::Invalid);
        }
        Ok(CandidateParse::Valid(CandidateResult {
            section,
            diagnostics,
        }))
    }

    fn advance_candidate(
        &mut self,
        declared: u64,
        candidates: Vec<u64>,
        index: usize,
        selected: Option<CandidateResult>,
        anchor_scan_bytes: u64,
    ) {
        let next_index = index + 1;
        let next_window = candidates
            .get(next_index)
            .map(|candidate| {
                self.xref_limits
                    .initial_section_bytes()
                    .min(self.source_len - *candidate)
            })
            .unwrap_or(1);
        self.state = RepairState::CandidateSection {
            declared,
            candidates,
            index: next_index,
            window: next_window,
            charged: false,
            selected,
            anchor_scan_bytes,
        };
    }

    fn validate_slice(&self, bytes: &ByteSlice, range: ByteRange) -> Result<(), XrefError> {
        if bytes.identity() != self.snapshot.identity() {
            return Err(XrefError::for_code(XrefErrorCode::SnapshotMismatch, None));
        }
        if bytes.range() != range {
            return Err(XrefError::for_code(
                XrefErrorCode::InternalState,
                Some(range.start()),
            ));
        }
        Ok(())
    }

    fn charge_scan(&mut self, amount: u64, offset: Option<u64>) -> Result<(), XrefError> {
        let Some(total) = self.stats.repair_scan_bytes.checked_add(amount) else {
            return Err(XrefError::resource(
                XrefLimitKind::RepairScanBytes,
                self.repair_limits.max_scan_bytes,
                self.stats.repair_scan_bytes,
                amount,
                offset,
            ));
        };
        if total > self.repair_limits.max_scan_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::RepairScanBytes,
                self.repair_limits.max_scan_bytes,
                self.stats.repair_scan_bytes,
                amount,
                offset,
            ));
        }
        self.stats.repair_scan_bytes = total;
        Ok(())
    }

    fn charge_whitespace_edits(
        &mut self,
        amount: u64,
        offset: Option<u64>,
    ) -> Result<(), XrefError> {
        let Some(total) = self.stats.whitespace_edits.checked_add(amount) else {
            return Err(XrefError::resource(
                XrefLimitKind::RepairWhitespaceEdits,
                self.repair_limits.max_whitespace_edits,
                self.stats.whitespace_edits,
                amount,
                offset,
            ));
        };
        if total > self.repair_limits.max_whitespace_edits {
            return Err(XrefError::resource(
                XrefLimitKind::RepairWhitespaceEdits,
                self.repair_limits.max_whitespace_edits,
                self.stats.whitespace_edits,
                amount,
                offset,
            ));
        }
        self.stats.whitespace_edits = total;
        Ok(())
    }

    fn push_diagnostic(
        &mut self,
        diagnostics: &mut Vec<XrefRepairDiagnostic>,
        diagnostic: XrefRepairDiagnostic,
    ) -> Result<(), XrefError> {
        let next_repairs = self.stats.repairs.checked_add(1).ok_or_else(|| {
            XrefError::resource(
                XrefLimitKind::RepairDiagnostics,
                self.repair_limits.max_repairs,
                self.stats.repairs,
                1,
                Some(diagnostic.subject_offset),
            )
        })?;
        if next_repairs > self.repair_limits.max_repairs {
            return Err(XrefError::resource(
                XrefLimitKind::RepairDiagnostics,
                self.repair_limits.max_repairs,
                self.stats.repairs,
                1,
                Some(diagnostic.subject_offset),
            ));
        }
        let element_bytes =
            u64::try_from(mem::size_of::<XrefRepairDiagnostic>()).map_err(|_| {
                XrefError::for_code(
                    XrefErrorCode::InternalState,
                    Some(diagnostic.subject_offset),
                )
            })?;
        let before_bytes = capacity_bytes::<XrefRepairDiagnostic>(diagnostics.capacity())
            .ok_or_else(|| {
                XrefError::for_code(
                    XrefErrorCode::InternalState,
                    Some(diagnostic.subject_offset),
                )
            })?;
        let minimum_growth = if diagnostics.len() == diagnostics.capacity() {
            element_bytes
        } else {
            0
        };
        let logical_total = self
            .stats
            .diagnostic_bytes
            .checked_add(minimum_growth)
            .ok_or_else(|| {
                XrefError::resource(
                    XrefLimitKind::RepairDiagnosticBytes,
                    self.repair_limits.max_diagnostic_bytes,
                    self.stats.diagnostic_bytes,
                    minimum_growth,
                    Some(diagnostic.subject_offset),
                )
            })?;
        if logical_total > self.repair_limits.max_diagnostic_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::RepairDiagnosticBytes,
                self.repair_limits.max_diagnostic_bytes,
                self.stats.diagnostic_bytes,
                minimum_growth,
                Some(diagnostic.subject_offset),
            ));
        }
        diagnostics.try_reserve_exact(1).map_err(|_| {
            XrefError::resource(
                XrefLimitKind::RepairDiagnosticBytes,
                self.repair_limits.max_diagnostic_bytes,
                self.stats.diagnostic_bytes,
                element_bytes,
                Some(diagnostic.subject_offset),
            )
        })?;
        let after_bytes = capacity_bytes::<XrefRepairDiagnostic>(diagnostics.capacity())
            .ok_or_else(|| {
                XrefError::for_code(
                    XrefErrorCode::InternalState,
                    Some(diagnostic.subject_offset),
                )
            })?;
        let growth = after_bytes.checked_sub(before_bytes).ok_or_else(|| {
            XrefError::for_code(
                XrefErrorCode::InternalState,
                Some(diagnostic.subject_offset),
            )
        })?;
        let total = self
            .stats
            .diagnostic_bytes
            .checked_add(growth)
            .ok_or_else(|| {
                XrefError::resource(
                    XrefLimitKind::RepairDiagnosticBytes,
                    self.repair_limits.max_diagnostic_bytes,
                    self.stats.diagnostic_bytes,
                    growth,
                    Some(diagnostic.subject_offset),
                )
            })?;
        if total > self.repair_limits.max_diagnostic_bytes {
            return Err(XrefError::resource(
                XrefLimitKind::RepairDiagnosticBytes,
                self.repair_limits.max_diagnostic_bytes,
                self.stats.diagnostic_bytes,
                growth,
                Some(diagnostic.subject_offset),
            ));
        }
        diagnostics.push(diagnostic);
        self.stats.repairs = next_repairs;
        self.stats.diagnostic_bytes = total;
        Ok(())
    }

    fn fail(&mut self, error: XrefError) -> LocalXrefPoll {
        self.state = RepairState::Failed(error);
        LocalXrefPoll::Failed(error)
    }
}

impl fmt::Debug for OpenLocalXrefJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenLocalXrefJob")
            .field("snapshot", &self.snapshot)
            .field("context", &self.context)
            .field("xref_limits", &self.xref_limits)
            .field("repair_limits", &self.repair_limits)
            .field("syntax_limits", &self.syntax_limits)
            .field("phase", &self.phase())
            .field("stats", &self.stats())
            .finish()
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "a validated candidate stays inline until uniqueness is established"
)]
enum CandidateParse {
    NeedMore,
    Invalid,
    Valid(CandidateResult),
}

struct RowRepair {
    offset: u64,
    edits: u64,
}

#[derive(Clone, Copy)]
struct RowEvidenceBudget {
    retain: bool,
    offset_diagnostics: u64,
    consumed_repairs: u64,
    consumed_whitespace_edits: u64,
    consumed_diagnostic_bytes: u64,
}

enum NormalizedRows {
    NotApplicable,
    Changed {
        bytes: Vec<u8>,
        rows: Vec<RowRepair>,
        whitespace_edits: u64,
        working_bytes: u64,
    },
}

fn capacity_bytes<T>(capacity: usize) -> Option<u64> {
    u64::try_from(capacity)
        .ok()?
        .checked_mul(u64::try_from(mem::size_of::<T>()).ok()?)
}

fn repair_working_bytes(canonical: &Vec<u8>, rows: &Vec<RowRepair>) -> Option<u64> {
    capacity_bytes::<u8>(canonical.capacity())?
        .checked_add(capacity_bytes::<RowRepair>(rows.capacity())?)
}

fn normalized_rows(
    canonical: Vec<u8>,
    rows: Vec<RowRepair>,
    whitespace_edits: u64,
) -> Result<NormalizedRows, XrefError> {
    if whitespace_edits == 0 {
        return Ok(NormalizedRows::NotApplicable);
    }
    let working_bytes = repair_working_bytes(&canonical, &rows)
        .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, None))?;
    Ok(NormalizedRows::Changed {
        bytes: canonical,
        rows,
        whitespace_edits,
        working_bytes,
    })
}

fn candidate_error_is_malformed(error: XrefError) -> bool {
    match error.code() {
        XrefErrorCode::InvalidXrefKeyword
        | XrefErrorCode::InvalidSubsection
        | XrefErrorCode::InvalidEntry
        | XrefErrorCode::InvalidTrailer => true,
        XrefErrorCode::SyntaxFailure => error.category() == XrefErrorCategory::Syntax,
        _ => false,
    }
}

fn repair_anchor_range(declared: u64, source_len: u64, delta: u64) -> Result<ByteRange, XrefError> {
    let lower = declared.saturating_sub(delta);
    let upper = declared
        .saturating_add(delta)
        .min(source_len.saturating_sub(1));
    let start = lower.saturating_sub(1);
    let end = upper.saturating_add(5).min(source_len);
    let len = end
        .checked_sub(start)
        .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(declared)))?;
    ByteRange::new(start, len)
        .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(declared)))
}

fn scan_xref_anchors(
    bytes: &ByteSlice,
    declared: u64,
    limits: XrefRepairLimits,
    cancellation: &dyn XrefCancellation,
) -> Result<Vec<u64>, XrefError> {
    let lower = declared.saturating_sub(limits.max_startxref_delta);
    let upper = declared.saturating_add(limits.max_startxref_delta);
    let raw = bytes.bytes();
    let mut candidates = Vec::new();
    for position in 0..raw.len().saturating_sub(3) {
        if position.is_multiple_of(256) && cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
        if &raw[position..position + 4] != b"xref" {
            continue;
        }
        let absolute =
            bytes
                .range()
                .start()
                .checked_add(u64::try_from(position).map_err(|_| {
                    XrefError::for_code(XrefErrorCode::InternalState, Some(declared))
                })?)
                .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(declared)))?;
        if absolute < lower || absolute > upper {
            continue;
        }
        let preceding_ok = if absolute == 0 {
            true
        } else {
            position
                .checked_sub(1)
                .and_then(|index| raw.get(index))
                .is_some_and(|byte| is_pdf_whitespace(*byte))
        };
        let following_ok = raw
            .get(position + 4)
            .is_some_and(|byte| is_pdf_whitespace(*byte));
        if !preceding_ok || !following_ok {
            continue;
        }
        let existing = u64::try_from(candidates.len())
            .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(absolute)))?;
        if existing >= limits.max_candidates {
            return Err(XrefError::resource(
                XrefLimitKind::RepairCandidates,
                limits.max_candidates,
                existing,
                1,
                Some(absolute),
            ));
        }
        candidates.try_reserve_exact(1).map_err(|_| {
            XrefError::resource(
                XrefLimitKind::Allocation,
                limits.max_candidates,
                existing,
                1,
                Some(absolute),
            )
        })?;
        candidates.push(absolute);
    }
    if cancellation.is_cancelled() {
        return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
    }
    Ok(candidates)
}

fn normalize_fixed_width_rows(
    bytes: &[u8],
    base: u64,
    cancellation: &dyn XrefCancellation,
    limits: XrefRepairLimits,
    evidence: RowEvidenceBudget,
) -> Result<NormalizedRows, XrefError> {
    if !bytes.starts_with(b"xref") {
        return Ok(NormalizedRows::NotApplicable);
    }
    let input_bytes = u64::try_from(bytes.len())
        .map_err(|_| XrefError::for_code(XrefErrorCode::InternalState, Some(base)))?;
    if input_bytes > limits.max_working_bytes {
        return Err(XrefError::resource(
            XrefLimitKind::RepairWorkingBytes,
            limits.max_working_bytes,
            0,
            input_bytes,
            Some(base),
        ));
    }
    let mut canonical = Vec::new();
    canonical.try_reserve_exact(bytes.len()).map_err(|_| {
        XrefError::resource(
            XrefLimitKind::RepairWorkingBytes,
            limits.max_working_bytes,
            0,
            input_bytes,
            Some(base),
        )
    })?;
    let canonical_bytes = capacity_bytes::<u8>(canonical.capacity())
        .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(base)))?;
    if canonical_bytes > limits.max_working_bytes {
        return Err(XrefError::resource(
            XrefLimitKind::RepairWorkingBytes,
            limits.max_working_bytes,
            0,
            canonical_bytes,
            Some(base),
        ));
    }
    canonical.extend_from_slice(bytes);
    let mut rows = Vec::new();
    let mut edits = 0_u64;
    let mut position = 4;
    if !consume_line_ending(bytes, &mut position) {
        return Ok(NormalizedRows::NotApplicable);
    }
    loop {
        skip_whitespace(bytes, &mut position);
        if bytes.get(position..position.saturating_add(b"trailer".len())) == Some(b"trailer") {
            break;
        }
        if cancellation.is_cancelled() {
            return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
        }
        if parse_decimal(bytes, &mut position).is_none()
            || !consume_horizontal(bytes, &mut position)
        {
            return normalized_rows(canonical, rows, edits);
        }
        let Some(count) = parse_decimal(bytes, &mut position) else {
            return normalized_rows(canonical, rows, edits);
        };
        if count == 0 || !consume_line_ending(bytes, &mut position) {
            return normalized_rows(canonical, rows, edits);
        }
        for row_index in 0..count {
            if row_index.is_multiple_of(256) && cancellation.is_cancelled() {
                return Err(XrefError::for_code(XrefErrorCode::Cancelled, None));
            }
            let Some(row_end) = position.checked_add(20) else {
                return normalized_rows(canonical, rows, edits);
            };
            let Some(row) = bytes.get(position..row_end) else {
                return normalized_rows(canonical, rows, edits);
            };
            if !row[..10].iter().all(u8::is_ascii_digit)
                || !is_horizontal_whitespace(row[10])
                || !row[11..16].iter().all(u8::is_ascii_digit)
                || !is_horizontal_whitespace(row[16])
                || !matches!(row[17], b'n' | b'f')
            {
                return normalized_rows(canonical, rows, edits);
            }
            let ending_is_crlf = (row[18], row[19]) == (b'\r', b'\n');
            let ending_has_horizontal_prefix =
                is_horizontal_whitespace(row[18]) && matches!(row[19], b'\r' | b'\n');
            if !ending_is_crlf && !ending_has_horizontal_prefix {
                return normalized_rows(canonical, rows, edits);
            }
            let mut row_edits = 0_u64;
            for relative in [10, 16] {
                if row[relative] != b' ' {
                    canonical[position + relative] = b' ';
                    row_edits += 1;
                }
            }
            if ending_has_horizontal_prefix && row[18] != b' ' {
                canonical[position + 18] = b' ';
                row_edits += 1;
            }
            if row_edits != 0 {
                let row_offset = base
                    .checked_add(u64::try_from(position).map_err(|_| {
                        XrefError::for_code(XrefErrorCode::InternalState, Some(base))
                    })?)
                    .ok_or_else(|| XrefError::for_code(XrefErrorCode::InternalState, Some(base)))?;
                let next_edits = edits.checked_add(row_edits).ok_or_else(|| {
                    XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                })?;
                let total_edits = evidence
                    .consumed_whitespace_edits
                    .checked_add(next_edits)
                    .ok_or_else(|| {
                        XrefError::resource(
                            XrefLimitKind::RepairWhitespaceEdits,
                            limits.max_whitespace_edits,
                            evidence.consumed_whitespace_edits,
                            next_edits,
                            Some(row_offset),
                        )
                    })?;
                if total_edits > limits.max_whitespace_edits {
                    return Err(XrefError::resource(
                        XrefLimitKind::RepairWhitespaceEdits,
                        limits.max_whitespace_edits,
                        evidence.consumed_whitespace_edits,
                        next_edits,
                        Some(row_offset),
                    ));
                }
                if evidence.retain {
                    let next_rows = u64::try_from(rows.len())
                        .ok()
                        .and_then(|count| count.checked_add(1))
                        .ok_or_else(|| {
                            XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                        })?;
                    let candidate_diagnostics = evidence
                        .offset_diagnostics
                        .checked_add(next_rows)
                        .ok_or_else(|| {
                            XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                        })?;
                    let total_repairs = evidence
                        .consumed_repairs
                        .checked_add(candidate_diagnostics)
                        .ok_or_else(|| {
                            XrefError::resource(
                                XrefLimitKind::RepairDiagnostics,
                                limits.max_repairs,
                                evidence.consumed_repairs,
                                candidate_diagnostics,
                                Some(row_offset),
                            )
                        })?;
                    if total_repairs > limits.max_repairs {
                        return Err(XrefError::resource(
                            XrefLimitKind::RepairDiagnostics,
                            limits.max_repairs,
                            evidence.consumed_repairs,
                            candidate_diagnostics,
                            Some(row_offset),
                        ));
                    }
                    let candidate_diagnostic_bytes = candidate_diagnostics
                        .checked_mul(
                            u64::try_from(mem::size_of::<XrefRepairDiagnostic>()).map_err(
                                |_| {
                                    XrefError::for_code(
                                        XrefErrorCode::InternalState,
                                        Some(row_offset),
                                    )
                                },
                            )?,
                        )
                        .ok_or_else(|| {
                            XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                        })?;
                    let total_diagnostic_bytes = evidence
                        .consumed_diagnostic_bytes
                        .checked_add(candidate_diagnostic_bytes)
                        .ok_or_else(|| {
                            XrefError::resource(
                                XrefLimitKind::RepairDiagnosticBytes,
                                limits.max_diagnostic_bytes,
                                evidence.consumed_diagnostic_bytes,
                                candidate_diagnostic_bytes,
                                Some(row_offset),
                            )
                        })?;
                    if total_diagnostic_bytes > limits.max_diagnostic_bytes {
                        return Err(XrefError::resource(
                            XrefLimitKind::RepairDiagnosticBytes,
                            limits.max_diagnostic_bytes,
                            evidence.consumed_diagnostic_bytes,
                            candidate_diagnostic_bytes,
                            Some(row_offset),
                        ));
                    }
                    let before_working =
                        repair_working_bytes(&canonical, &rows).ok_or_else(|| {
                            XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                        })?;
                    let row_bytes = u64::try_from(mem::size_of::<RowRepair>()).map_err(|_| {
                        XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                    })?;
                    let minimum_growth = if rows.len() == rows.capacity() {
                        row_bytes
                    } else {
                        0
                    };
                    let logical_working =
                        before_working.checked_add(minimum_growth).ok_or_else(|| {
                            XrefError::resource(
                                XrefLimitKind::RepairWorkingBytes,
                                limits.max_working_bytes,
                                before_working,
                                minimum_growth,
                                Some(row_offset),
                            )
                        })?;
                    if logical_working > limits.max_working_bytes {
                        return Err(XrefError::resource(
                            XrefLimitKind::RepairWorkingBytes,
                            limits.max_working_bytes,
                            before_working,
                            minimum_growth,
                            Some(row_offset),
                        ));
                    }
                    rows.try_reserve_exact(1).map_err(|_| {
                        XrefError::resource(
                            XrefLimitKind::RepairWorkingBytes,
                            limits.max_working_bytes,
                            before_working,
                            row_bytes,
                            Some(row_offset),
                        )
                    })?;
                    let after_working =
                        repair_working_bytes(&canonical, &rows).ok_or_else(|| {
                            XrefError::for_code(XrefErrorCode::InternalState, Some(row_offset))
                        })?;
                    if after_working > limits.max_working_bytes {
                        return Err(XrefError::resource(
                            XrefLimitKind::RepairWorkingBytes,
                            limits.max_working_bytes,
                            before_working,
                            after_working.saturating_sub(before_working),
                            Some(row_offset),
                        ));
                    }
                    rows.push(RowRepair {
                        offset: row_offset,
                        edits: row_edits,
                    });
                }
                edits = next_edits;
            }
            position = row_end;
        }
    }
    normalized_rows(canonical, rows, edits)
}

fn parse_decimal(bytes: &[u8], position: &mut usize) -> Option<u64> {
    let start = *position;
    let mut value = 0_u64;
    while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(bytes[*position] - b'0'))?;
        *position += 1;
    }
    (*position != start).then_some(value)
}

fn consume_horizontal(bytes: &[u8], position: &mut usize) -> bool {
    let start = *position;
    while bytes
        .get(*position)
        .is_some_and(|byte| is_horizontal_whitespace(*byte))
    {
        *position += 1;
    }
    *position != start
}

fn consume_line_ending(bytes: &[u8], position: &mut usize) -> bool {
    while bytes
        .get(*position)
        .is_some_and(|byte| is_horizontal_whitespace(*byte))
    {
        *position += 1;
    }
    match bytes.get(*position) {
        Some(b'\n') => {
            *position += 1;
            true
        }
        Some(b'\r') => {
            *position += 1;
            if bytes.get(*position) == Some(&b'\n') {
                *position += 1;
            }
            true
        }
        _ => false,
    }
}

fn skip_whitespace(bytes: &[u8], position: &mut usize) {
    while bytes
        .get(*position)
        .is_some_and(|byte| is_pdf_whitespace(*byte))
    {
        *position += 1;
    }
}

const fn is_pdf_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

const fn is_horizontal_whitespace(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | 12 | b' ')
}

fn grow_window(current: u64, cap: u64) -> Option<u64> {
    if current >= cap {
        return None;
    }
    let doubled = current.checked_mul(2).unwrap_or(cap);
    Some(doubled.max(current + 1).min(cap))
}
