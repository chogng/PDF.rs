use std::error::Error;
use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, DataTicket, JobId, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_object::{
    LocalObjectJobContext, LocalObjectPhase, LocalObjectPoll, ObjectError, ObjectErrorCategory,
    ObjectLimitKind, ObjectLimits, ObjectRepairLimits, ObjectRepairStats, ObjectRepairWorkCaps,
    ObjectWorkCaps, OpenLocalObjectJob,
};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    LocalXrefJobContext, LocalXrefPhase, LocalXrefPoll, OpenLocalXrefJob, XrefCancellation,
    XrefError, XrefErrorCategory, XrefLimits, XrefRepairLimits, XrefRepairStats,
};

use crate::{
    AttestLocalRepairRevisionJob, DocumentCancellation, DocumentError, DocumentErrorCategory,
    DocumentErrorCode, DocumentIndexStats, DocumentLimitKind, DocumentLimits,
    EffectiveObjectOffset, LocalRepairPlanningRevision, LocalRevisionAttestationJobContext,
    LocalRevisionAttestationPoll, LocallyRepairedRevisionIndex, RepairGeometryStats,
    RevisionAttestationLimits, RevisionAttestationPhase, RevisionAttestationStats, RevisionId,
};

const HARD_MAX_PROBE_OBJECTS: u64 = 4_000_000;
const HARD_MAX_PROBE_WORK_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_PROBE_CANDIDATES: u64 = 8_000_000;
const HARD_MAX_PROBE_EVIDENCE_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated aggregate ceilings for the object-probe pass of one local-repair open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRepairProbeLimitConfig {
    /// Maximum in-use objects started before geometry rebuild.
    pub max_objects: u64,
    /// Maximum cumulative exact source bytes charged by all first-pass object children.
    pub max_total_read_bytes: u64,
    /// Maximum cumulative parser-window bytes charged by all first-pass object children.
    pub max_total_parse_bytes: u64,
    /// Maximum cumulative repair-only source bytes scanned by all first-pass objects.
    pub max_total_repair_scan_bytes: u64,
    /// Maximum cumulative matching object-header candidates.
    pub max_total_header_candidates: u64,
    /// Maximum cumulative looks-like stream-boundary candidates.
    pub max_total_boundary_candidates: u64,
    /// Maximum allocator-reported capacity retained for fixed-size repair evidence.
    pub max_retained_evidence_bytes: u64,
}

impl Default for LocalRepairProbeLimitConfig {
    fn default() -> Self {
        Self {
            max_objects: 25_000,
            max_total_read_bytes: 64 * 1024 * 1024,
            max_total_parse_bytes: 64 * 1024 * 1024,
            max_total_repair_scan_bytes: 16 * 1024 * 1024,
            max_total_header_candidates: 25_000,
            max_total_boundary_candidates: 25_000,
            max_retained_evidence_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Validated aggregate ceilings for one complete local-repair object-probe pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRepairProbeLimits {
    max_objects: u64,
    max_total_read_bytes: u64,
    max_total_parse_bytes: u64,
    max_total_repair_scan_bytes: u64,
    max_total_header_candidates: u64,
    max_total_boundary_candidates: u64,
    max_retained_evidence_bytes: u64,
}

impl LocalRepairProbeLimits {
    /// Validates one first-pass aggregate profile beneath fixed implementation ceilings.
    ///
    /// Repair-only scan and candidate totals may be zero to permit strict-valid objects while
    /// disabling the corresponding recovery work. Object, validation-work, and evidence limits
    /// remain positive because every published repaired revision contains at least its root.
    pub fn validate(config: LocalRepairProbeLimitConfig) -> Result<Self, DocumentError> {
        if config.max_objects == 0
            || config.max_objects > HARD_MAX_PROBE_OBJECTS
            || config.max_total_read_bytes == 0
            || config.max_total_read_bytes > HARD_MAX_PROBE_WORK_BYTES
            || config.max_total_parse_bytes == 0
            || config.max_total_parse_bytes > HARD_MAX_PROBE_WORK_BYTES
            || config.max_total_repair_scan_bytes > HARD_MAX_PROBE_WORK_BYTES
            || config.max_total_header_candidates > HARD_MAX_PROBE_CANDIDATES
            || config.max_total_boundary_candidates > HARD_MAX_PROBE_CANDIDATES
            || config.max_retained_evidence_bytes == 0
            || config.max_retained_evidence_bytes > HARD_MAX_PROBE_EVIDENCE_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_objects: config.max_objects,
            max_total_read_bytes: config.max_total_read_bytes,
            max_total_parse_bytes: config.max_total_parse_bytes,
            max_total_repair_scan_bytes: config.max_total_repair_scan_bytes,
            max_total_header_candidates: config.max_total_header_candidates,
            max_total_boundary_candidates: config.max_total_boundary_candidates,
            max_retained_evidence_bytes: config.max_retained_evidence_bytes,
        })
    }

    /// Returns the complete first-pass object-count ceiling.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the cumulative validation and scan exact-read ceiling.
    pub const fn max_total_read_bytes(self) -> u64 {
        self.max_total_read_bytes
    }

    /// Returns the cumulative validation parser-window ceiling.
    pub const fn max_total_parse_bytes(self) -> u64 {
        self.max_total_parse_bytes
    }

    /// Returns the cumulative repair-only scan-byte ceiling.
    pub const fn max_total_repair_scan_bytes(self) -> u64 {
        self.max_total_repair_scan_bytes
    }

    /// Returns the cumulative matching object-header candidate ceiling.
    pub const fn max_total_header_candidates(self) -> u64 {
        self.max_total_header_candidates
    }

    /// Returns the cumulative stream-boundary candidate ceiling.
    pub const fn max_total_boundary_candidates(self) -> u64 {
        self.max_total_boundary_candidates
    }

    /// Returns the allocator-reported repair-plan capacity ceiling.
    pub const fn max_retained_evidence_bytes(self) -> u64 {
        self.max_retained_evidence_bytes
    }
}

impl Default for LocalRepairProbeLimits {
    fn default() -> Self {
        Self::validate(LocalRepairProbeLimitConfig::default())
            .expect("built-in local-repair probe limits satisfy hard ceilings")
    }
}

/// Runtime identity and all phase-specific checkpoints for one local-repair base open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRepairOpenContext {
    xref: LocalXrefJobContext,
    first_pass_object: LocalObjectJobContext,
    final_attestation: LocalRevisionAttestationJobContext,
}

impl LocalRepairOpenContext {
    /// Creates a context later validated for one JobId and seventeen distinct checkpoints.
    pub const fn new(
        xref: LocalXrefJobContext,
        first_pass_object: LocalObjectJobContext,
        final_attestation: LocalRevisionAttestationJobContext,
    ) -> Self {
        Self {
            xref,
            first_pass_object,
            final_attestation,
        }
    }

    /// Returns the strict-first xref discovery and repair context.
    pub const fn xref(self) -> LocalXrefJobContext {
        self.xref
    }

    /// Returns the context used to probe every xref-derived object once.
    pub const fn first_pass_object(self) -> LocalObjectJobContext {
        self.first_pass_object
    }

    /// Returns the independent context used for final top-level attestation.
    pub const fn final_attestation(self) -> LocalRevisionAttestationJobContext {
        self.final_attestation
    }

    /// Returns the one runtime job identity shared by every child phase.
    pub const fn job(self) -> JobId {
        self.xref.strict().job()
    }

    fn is_valid(self) -> bool {
        let xref = self.xref;
        let first = self.first_pass_object;
        let final_context = self.final_attestation;
        let final_object = final_context.object_context();
        if xref.strict().job() != first.strict().job()
            || xref.strict().job() != final_object.strict().job()
            || first.strict().priority() != final_object.strict().priority()
        {
            return false;
        }
        let checkpoints = [
            xref.strict().tail_checkpoint(),
            xref.strict().section_checkpoint(),
            xref.anchor_scan_checkpoint(),
            xref.candidate_section_checkpoint(),
            first.strict().envelope_checkpoint(),
            first.strict().boundary_checkpoint(),
            first.candidate().envelope_checkpoint(),
            first.candidate().boundary_checkpoint(),
            first.header_scan_checkpoint(),
            first.length_scan_checkpoint(),
            final_context.scan_checkpoint(),
            final_object.strict().envelope_checkpoint(),
            final_object.strict().boundary_checkpoint(),
            final_object.candidate().envelope_checkpoint(),
            final_object.candidate().boundary_checkpoint(),
            final_object.header_scan_checkpoint(),
            final_object.length_scan_checkpoint(),
        ];
        checkpoints
            .iter()
            .enumerate()
            .all(|(index, checkpoint)| !checkpoints[..index].contains(checkpoint))
    }
}

/// Complete validated child profiles for one local-repair base open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalRepairOpenLimits {
    xref: XrefLimits,
    xref_repair: XrefRepairLimits,
    document: DocumentLimits,
    first_pass: LocalRepairProbeLimits,
    attestation: RevisionAttestationLimits,
    object: ObjectLimits,
    object_repair: ObjectRepairLimits,
    syntax: SyntaxLimits,
}

impl LocalRepairOpenLimits {
    /// Bundles already-validated xref, document, probe, attestation, object, and syntax limits.
    #[allow(
        clippy::too_many_arguments,
        reason = "the open profile keeps every independently validated lower capability explicit"
    )]
    pub const fn new(
        xref: XrefLimits,
        xref_repair: XrefRepairLimits,
        document: DocumentLimits,
        first_pass: LocalRepairProbeLimits,
        attestation: RevisionAttestationLimits,
        object: ObjectLimits,
        object_repair: ObjectRepairLimits,
        syntax: SyntaxLimits,
    ) -> Self {
        Self {
            xref,
            xref_repair,
            document,
            first_pass,
            attestation,
            object,
            object_repair,
            syntax,
        }
    }

    /// Returns the traditional-xref validation profile.
    pub const fn xref(self) -> XrefLimits {
        self.xref
    }

    /// Returns the bounded local-xref repair profile.
    pub const fn xref_repair(self) -> XrefRepairLimits {
        self.xref_repair
    }

    /// Returns candidate-index and geometry-rebuild limits.
    pub const fn document(self) -> DocumentLimits {
        self.document
    }

    /// Returns document-wide first-pass object-probe limits.
    pub const fn first_pass(self) -> LocalRepairProbeLimits {
        self.first_pass
    }

    /// Returns final top-level attestation limits.
    pub const fn attestation(self) -> RevisionAttestationLimits {
        self.attestation
    }

    /// Returns per-object framing limits used in both passes.
    pub const fn object(self) -> ObjectLimits {
        self.object
    }

    /// Returns per-object local repair limits used in both passes.
    pub const fn object_repair(self) -> ObjectRepairLimits {
        self.object_repair
    }

    /// Returns direct syntax limits used by all parser children.
    pub const fn syntax(self) -> SyntaxLimits {
        self.syntax
    }
}

impl Default for LocalRepairOpenLimits {
    fn default() -> Self {
        Self::new(
            XrefLimits::default(),
            XrefRepairLimits::default(),
            DocumentLimits::default(),
            LocalRepairProbeLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            ObjectRepairLimits::default(),
            SyntaxLimits::default(),
        )
    }
}

/// Aggregate work and retained-plan accounting for the first-pass object probes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalRepairProbeStats {
    objects_started: u64,
    objects_completed: u64,
    read_bytes: u64,
    parse_bytes: u64,
    repair_scan_bytes: u64,
    header_candidates: u64,
    boundary_candidates: u64,
    retained_evidence_bytes: u64,
}

impl LocalRepairProbeStats {
    /// Returns object children constructed before the latest poll.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns proof-bearing objects converted into fixed-size evidence.
    pub const fn objects_completed(self) -> u64 {
        self.objects_completed
    }

    /// Returns cumulative validation plus repair-scan exact-read bytes.
    pub const fn read_bytes(self) -> u64 {
        self.read_bytes
    }

    /// Returns cumulative validation parser-window bytes.
    pub const fn parse_bytes(self) -> u64 {
        self.parse_bytes
    }

    /// Returns cumulative repair-only scan bytes.
    pub const fn repair_scan_bytes(self) -> u64 {
        self.repair_scan_bytes
    }

    /// Returns cumulative matching object-header candidates.
    pub const fn header_candidates(self) -> u64 {
        self.header_candidates
    }

    /// Returns cumulative looks-like stream-boundary candidates.
    pub const fn boundary_candidates(self) -> u64 {
        self.boundary_candidates
    }

    /// Returns allocator-reported capacity retained for the complete proof plan.
    pub const fn retained_evidence_bytes(self) -> u64 {
        self.retained_evidence_bytes
    }
}

/// Cumulative phase evidence for one local-repair base open.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalRepairOpenStats {
    xref: XrefRepairStats,
    initial_index: Option<DocumentIndexStats>,
    first_pass: LocalRepairProbeStats,
    geometry: Option<RepairGeometryStats>,
    final_attestation: RevisionAttestationStats,
}

impl LocalRepairOpenStats {
    /// Returns strict plus local-xref work.
    pub const fn xref(self) -> XrefRepairStats {
        self.xref
    }

    /// Returns initial xref-derived candidate-index accounting once available.
    pub const fn initial_index(self) -> Option<DocumentIndexStats> {
        self.initial_index
    }

    /// Returns aggregate first-pass object-probe accounting.
    pub const fn first_pass(self) -> LocalRepairProbeStats {
        self.first_pass
    }

    /// Returns effective-geometry rebuild accounting once available.
    pub const fn geometry(self) -> Option<RepairGeometryStats> {
        self.geometry
    }

    /// Returns complete final top-level attestation work.
    pub const fn final_attestation(self) -> RevisionAttestationStats {
        self.final_attestation
    }
}

/// Stable lower-layer failure for one local-repair base open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRepairOpenError {
    /// Local xref discovery or repair failed with its original evidence.
    Xref(XrefError),
    /// One first-pass local object failed outside a document aggregate limit.
    Object(ObjectError),
    /// Candidate, aggregate, geometry, or final attestation failed.
    Document(DocumentError),
}

impl LocalRepairOpenError {
    /// Returns whether a lower child reported normal runtime cancellation.
    pub const fn is_cancelled(self) -> bool {
        match self {
            Self::Xref(error) => matches!(error.category(), XrefErrorCategory::Cancellation),
            Self::Object(error) => {
                matches!(error.category(), ObjectErrorCategory::Cancellation)
            }
            Self::Document(error) => {
                matches!(error.category(), DocumentErrorCategory::Cancellation)
            }
        }
    }

    /// Returns retained xref failure evidence, if any.
    pub const fn xref(self) -> Option<XrefError> {
        match self {
            Self::Xref(error) => Some(error),
            Self::Object(_) | Self::Document(_) => None,
        }
    }

    /// Returns retained first-pass object failure evidence, if any.
    pub const fn object(self) -> Option<ObjectError> {
        match self {
            Self::Object(error) => Some(error),
            Self::Xref(_) | Self::Document(_) => None,
        }
    }

    /// Returns retained document-composition failure evidence, if any.
    pub const fn document(self) -> Option<DocumentError> {
        match self {
            Self::Document(error) => Some(error),
            Self::Xref(_) | Self::Object(_) => None,
        }
    }
}

impl fmt::Display for LocalRepairOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xref(error) => write!(formatter, "local repair open xref failed: {error}"),
            Self::Object(error) => {
                write!(formatter, "local repair open object probe failed: {error}")
            }
            Self::Document(error) => {
                write!(formatter, "local repair open document failed: {error}")
            }
        }
    }
}

impl Error for LocalRepairOpenError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Xref(error) => Some(error),
            Self::Object(error) => Some(error),
            Self::Document(error) => Some(error),
        }
    }
}

impl From<XrefError> for LocalRepairOpenError {
    fn from(error: XrefError) -> Self {
        Self::Xref(error)
    }
}

impl From<ObjectError> for LocalRepairOpenError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

impl From<DocumentError> for LocalRepairOpenError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

/// Coarse resumable phase of one complete local-repair base open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalRepairOpenPhase {
    /// Locating, strictly parsing, or locally repairing the final traditional xref.
    Xref(LocalXrefPhase),
    /// Probing every declared physical object before geometry rebuild.
    FirstPass {
        /// Zero-based physical object index currently active or next to start.
        next_object: u64,
        /// Active local-object child phase, absent only between objects.
        active: Option<LocalObjectPhase>,
    },
    /// Revalidating header, effective objects, and top-level trivia before publication.
    FinalAttestation(RevisionAttestationPhase),
    /// The repaired index was returned and the one-shot open is complete.
    Ready,
    /// The open reached a stable terminal failure.
    Failed,
}

/// Result of polling one complete local-repair base open.
#[allow(
    clippy::large_enum_variant,
    reason = "the move-only repaired index stays inline without an untracked allocation"
)]
pub enum LocalRepairOpenPoll {
    /// One traditional base revision is fully re-attested with an inseparable repair ledger.
    Ready(LocallyRepairedRevisionIndex),
    /// The active xref, first-pass object, or final attestation child requires exact ranges.
    Pending {
        /// One-shot source ticket.
        ticket: DataTicket,
        /// Canonical exact missing ranges.
        missing: SmallRanges,
        /// Exact child checkpoint to retain when requeueing the job.
        checkpoint: ResumeCheckpoint,
    },
    /// The open reached a stable structured failure.
    Failed(LocalRepairOpenError),
}

impl fmt::Debug for LocalRepairOpenPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(index) => formatter.debug_tuple("Ready").field(index).finish(),
            Self::Pending {
                ticket,
                missing,
                checkpoint,
            } => formatter
                .debug_struct("Pending")
                .field("ticket", ticket)
                .field("missing", missing)
                .field("checkpoint", checkpoint)
                .finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

struct XrefCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl XrefCancellation for XrefCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct ProbeChildState {
    physical_index: usize,
    reference: pdf_rs_syntax::ObjectRef,
    offset: u64,
    child: OpenLocalObjectJob,
    accounted: ObjectRepairStats,
    base_read_bytes: u64,
    base_parse_bytes: u64,
    base_scan_bytes: u64,
    base_header_candidates: u64,
    base_boundary_candidates: u64,
}

struct FirstPassState {
    plan: LocalRepairPlanningRevision,
    next: usize,
    evidence: Vec<EffectiveObjectOffset>,
    child: Option<ProbeChildState>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "exactly one bounded child and its proof plan remain owned across suspension"
)]
enum JobState {
    Xref(OpenLocalXrefJob),
    FirstPass(FirstPassState),
    FinalAttestation(AttestLocalRepairRevisionJob),
    Transition,
    Complete,
    Failed(LocalRepairOpenError),
}

/// One-shot core coordinator for a bounded R1 traditional base-revision open.
///
/// The coordinator is the sole owner of local xref discovery, every first-pass object probe,
/// effective-geometry rebuild, and complete final top-level attestation. It performs no host I/O
/// and exposes no unauthenticated intermediate value.
pub struct OpenLocallyRepairedBaseRevisionJob {
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    context: LocalRepairOpenContext,
    limits: LocalRepairOpenLimits,
    stats: LocalRepairOpenStats,
    state: JobState,
}

impl OpenLocallyRepairedBaseRevisionJob {
    /// Validates the complete cross-phase identity and creates the initial local-xref child.
    pub fn new(
        snapshot: SourceSnapshot,
        revision_id: RevisionId,
        context: LocalRepairOpenContext,
        limits: LocalRepairOpenLimits,
    ) -> Result<Self, LocalRepairOpenError> {
        if !context.is_valid() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLocalRepairOpenContext,
                None,
                None,
            )
            .into());
        }
        let xref = OpenLocalXrefJob::new(
            snapshot,
            context.xref,
            limits.xref,
            limits.xref_repair,
            limits.syntax,
        )?;
        Ok(Self {
            snapshot,
            revision_id,
            context,
            limits,
            stats: LocalRepairOpenStats::default(),
            state: JobState::Xref(xref),
        })
    }

    /// Returns the immutable source snapshot bound at construction.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned base revision identity.
    pub const fn revision_id(&self) -> RevisionId {
        self.revision_id
    }

    /// Returns the one-job, seventeen-checkpoint phase context.
    pub const fn context(&self) -> LocalRepairOpenContext {
        self.context
    }

    /// Returns every validated child and aggregate profile.
    pub const fn limits(&self) -> LocalRepairOpenLimits {
        self.limits
    }

    /// Returns cumulative work through the latest poll.
    pub const fn stats(&self) -> LocalRepairOpenStats {
        self.stats
    }

    /// Returns the current coarse open phase.
    pub fn phase(&self) -> LocalRepairOpenPhase {
        match &self.state {
            JobState::Xref(job) => LocalRepairOpenPhase::Xref(job.phase()),
            JobState::FirstPass(state) => LocalRepairOpenPhase::FirstPass {
                next_object: u64::try_from(state.next).unwrap_or(u64::MAX),
                active: state.child.as_ref().map(|child| child.child.phase()),
            },
            JobState::FinalAttestation(job) => LocalRepairOpenPhase::FinalAttestation(job.phase()),
            JobState::Complete => LocalRepairOpenPhase::Ready,
            JobState::Transition | JobState::Failed(_) => LocalRepairOpenPhase::Failed,
        }
    }

    /// Advances the complete repaired open without host, callback, or async-runtime I/O.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> LocalRepairOpenPoll {
        loop {
            let state = mem::replace(&mut self.state, JobState::Transition);
            match state {
                JobState::Xref(mut job) => {
                    let outcome = job.poll(source, &XrefCancellationAdapter(cancellation));
                    self.stats.xref = job.stats();
                    match outcome {
                        LocalXrefPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::Xref(job);
                            return LocalRepairOpenPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        LocalXrefPoll::Failed(error) => return self.fail(error.into()),
                        LocalXrefPoll::Ready(xref) => {
                            let plan = match LocalRepairPlanningRevision::new(
                                xref,
                                self.revision_id,
                                self.limits.document,
                                cancellation,
                            ) {
                                Ok(plan) => plan,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.stats.initial_index = Some(plan.index_stats());
                            let evidence = match self.reserve_evidence(&plan, cancellation) {
                                Ok(evidence) => evidence,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.state = JobState::FirstPass(FirstPassState {
                                plan,
                                next: 0,
                                evidence,
                                child: None,
                            });
                        }
                    }
                }
                JobState::FirstPass(mut state) => {
                    if state.child.is_none() {
                        if state.next == state.plan.physical_intervals().len() {
                            let rebuilt = match state.plan.rebuild(state.evidence, cancellation) {
                                Ok(rebuilt) => rebuilt,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.stats.geometry = Some(rebuilt.geometry_stats());
                            let attestation = match AttestLocalRepairRevisionJob::new(
                                rebuilt,
                                self.context.final_attestation,
                                self.limits.attestation,
                                self.limits.object,
                                self.limits.object_repair,
                                self.limits.syntax,
                            ) {
                                Ok(attestation) => attestation,
                                Err(error) => return self.fail(error.into()),
                            };
                            self.state = JobState::FinalAttestation(attestation);
                            continue;
                        }
                        let child = match self.start_probe(&state.plan, state.next) {
                            Ok(child) => child,
                            Err(error) => return self.fail(error),
                        };
                        state.child = Some(child);
                    }
                    let mut child = match state.child.take() {
                        Some(child) => child,
                        None => return self.fail_internal(),
                    };
                    let outcome = child
                        .child
                        .poll(source, &ObjectCancellationAdapter(cancellation));
                    if let Err(error) = self.account_probe(&mut child) {
                        return self.fail(error.into());
                    }
                    match outcome {
                        LocalObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            state.child = Some(child);
                            self.state = JobState::FirstPass(state);
                            return LocalRepairOpenPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        LocalObjectPoll::Failed(error) => {
                            let mapped = self.map_probe_error(error, &child);
                            return self.fail(mapped);
                        }
                        LocalObjectPoll::Ready(object) => {
                            let proof = match EffectiveObjectOffset::from_locally_framed(&object) {
                                Ok(proof) => proof,
                                Err(error) => return self.fail(error.into()),
                            };
                            if child.physical_index != state.next
                                || state.evidence.len() != state.next
                                || state.evidence.len() >= state.evidence.capacity()
                            {
                                return self.fail_internal();
                            }
                            state.evidence.push(proof);
                            state.next = match state.next.checked_add(1) {
                                Some(next) => next,
                                None => return self.fail_internal(),
                            };
                            self.stats.first_pass.objects_completed =
                                match self.stats.first_pass.objects_completed.checked_add(1) {
                                    Some(value) => value,
                                    None => return self.fail_internal(),
                                };
                            self.state = JobState::FirstPass(state);
                        }
                    }
                }
                JobState::FinalAttestation(mut job) => {
                    let outcome = job.poll(source, cancellation);
                    self.stats.final_attestation = job.stats();
                    match outcome {
                        LocalRevisionAttestationPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = JobState::FinalAttestation(job);
                            return LocalRepairOpenPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        LocalRevisionAttestationPoll::Failed(error) => {
                            return self.fail(error.into());
                        }
                        LocalRevisionAttestationPoll::Ready(index) => {
                            self.state = JobState::Complete;
                            return LocalRepairOpenPoll::Ready(index);
                        }
                    }
                }
                JobState::Complete => {
                    self.state = JobState::Complete;
                    return LocalRepairOpenPoll::Failed(
                        DocumentError::for_code(DocumentErrorCode::JobAlreadyComplete, None, None)
                            .into(),
                    );
                }
                JobState::Failed(error) => {
                    self.state = JobState::Failed(error);
                    return LocalRepairOpenPoll::Failed(error);
                }
                JobState::Transition => return self.fail_internal(),
            }
        }
    }

    fn reserve_evidence(
        &mut self,
        plan: &LocalRepairPlanningRevision,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Vec<EffectiveObjectOffset>, DocumentError> {
        if cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                None,
                None,
            ));
        }
        let object_count = u64::try_from(plan.physical_intervals().len()).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::RepairProbeObjects,
                self.limits.first_pass.max_objects,
                0,
                u64::MAX,
                None,
            )
        })?;
        if object_count > self.limits.first_pass.max_objects {
            return Err(DocumentError::resource(
                DocumentLimitKind::RepairProbeObjects,
                self.limits.first_pass.max_objects,
                0,
                object_count,
                None,
            ));
        }
        let requested_bytes = object_count
            .checked_mul(
                u64::try_from(mem::size_of::<EffectiveObjectOffset>()).map_err(|_| {
                    DocumentError::for_code(DocumentErrorCode::InternalState, None, None)
                })?,
            )
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RepairProbeEvidenceBytes,
                    self.limits.first_pass.max_retained_evidence_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if requested_bytes > self.limits.first_pass.max_retained_evidence_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::RepairProbeEvidenceBytes,
                self.limits.first_pass.max_retained_evidence_bytes,
                0,
                requested_bytes,
                None,
            ));
        }
        let capacity = usize::try_from(object_count).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::RepairProbeEvidenceBytes,
                self.limits.first_pass.max_retained_evidence_bytes,
                0,
                requested_bytes,
                None,
            )
        })?;
        let mut evidence = Vec::new();
        evidence.try_reserve_exact(capacity).map_err(|_| {
            DocumentError::resource(
                DocumentLimitKind::RepairProbeEvidenceBytes,
                self.limits.first_pass.max_retained_evidence_bytes,
                0,
                requested_bytes,
                None,
            )
        })?;
        let retained_bytes = u64::try_from(evidence.capacity())
            .ok()
            .and_then(|actual| {
                actual.checked_mul(u64::try_from(mem::size_of::<EffectiveObjectOffset>()).ok()?)
            })
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::RepairProbeEvidenceBytes,
                    self.limits.first_pass.max_retained_evidence_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if retained_bytes > self.limits.first_pass.max_retained_evidence_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::RepairProbeEvidenceBytes,
                self.limits.first_pass.max_retained_evidence_bytes,
                0,
                retained_bytes,
                None,
            ));
        }
        self.stats.first_pass.retained_evidence_bytes = retained_bytes;
        Ok(evidence)
    }

    fn start_probe(
        &mut self,
        plan: &LocalRepairPlanningRevision,
        physical_index: usize,
    ) -> Result<ProbeChildState, LocalRepairOpenError> {
        let interval = plan
            .physical_intervals()
            .get(physical_index)
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        if self.stats.first_pass.objects_started >= self.limits.first_pass.max_objects {
            return Err(DocumentError::resource(
                DocumentLimitKind::RepairProbeObjects,
                self.limits.first_pass.max_objects,
                self.stats.first_pass.objects_started,
                1,
                Some(interval.xref_offset()),
            )
            .into());
        }
        let read_remaining = remaining_positive(
            self.limits.first_pass.max_total_read_bytes,
            self.stats.first_pass.read_bytes,
            DocumentLimitKind::RepairProbeReadBytes,
            interval.xref_offset(),
        )?;
        let parse_remaining = remaining_positive(
            self.limits.first_pass.max_total_parse_bytes,
            self.stats.first_pass.parse_bytes,
            DocumentLimitKind::RepairProbeParseBytes,
            interval.xref_offset(),
        )?;
        let validation_caps = ObjectWorkCaps::new(
            read_remaining.min(self.limits.object.max_total_read_bytes()),
            parse_remaining.min(self.limits.object.max_total_parse_bytes()),
        )?;
        let scan_remaining = self
            .limits
            .first_pass
            .max_total_repair_scan_bytes
            .checked_sub(self.stats.first_pass.repair_scan_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference()),
                    Some(interval.xref_offset()),
                )
            })?;
        let header_remaining = self
            .limits
            .first_pass
            .max_total_header_candidates
            .checked_sub(self.stats.first_pass.header_candidates)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference()),
                    Some(interval.xref_offset()),
                )
            })?;
        let boundary_remaining = self
            .limits
            .first_pass
            .max_total_boundary_candidates
            .checked_sub(self.stats.first_pass.boundary_candidates)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference()),
                    Some(interval.xref_offset()),
                )
            })?;
        let repair_caps = ObjectRepairWorkCaps::new(
            scan_remaining.min(self.limits.object_repair.max_scan_bytes()),
            header_remaining.min(self.limits.object_repair.max_header_candidates()),
            boundary_remaining.min(self.limits.object_repair.max_boundary_candidates()),
        )?;
        let target = plan.unattested_target(interval.reference())?;
        let child = OpenLocalObjectJob::new_with_parent_caps(
            target,
            self.context.first_pass_object,
            self.limits.object,
            self.limits.object_repair,
            self.limits.syntax,
            validation_caps,
            repair_caps,
        )?;
        let state = ProbeChildState {
            physical_index,
            reference: interval.reference(),
            offset: interval.xref_offset(),
            child,
            accounted: ObjectRepairStats::default(),
            base_read_bytes: self.stats.first_pass.read_bytes,
            base_parse_bytes: self.stats.first_pass.parse_bytes,
            base_scan_bytes: self.stats.first_pass.repair_scan_bytes,
            base_header_candidates: self.stats.first_pass.header_candidates,
            base_boundary_candidates: self.stats.first_pass.boundary_candidates,
        };
        self.stats.first_pass.objects_started = self
            .stats
            .first_pass
            .objects_started
            .checked_add(1)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(interval.reference()),
                    Some(interval.xref_offset()),
                )
            })?;
        Ok(state)
    }

    fn account_probe(&mut self, child: &mut ProbeChildState) -> Result<(), DocumentError> {
        let current = child.child.stats();
        self.stats.first_pass.read_bytes = account_dimension(
            self.stats.first_pass.read_bytes,
            child.accounted.read_bytes(),
            current.read_bytes(),
            self.limits.first_pass.max_total_read_bytes,
            child.reference,
            child.offset,
        )?;
        self.stats.first_pass.parse_bytes = account_dimension(
            self.stats.first_pass.parse_bytes,
            child.accounted.parse_bytes(),
            current.parse_bytes(),
            self.limits.first_pass.max_total_parse_bytes,
            child.reference,
            child.offset,
        )?;
        self.stats.first_pass.repair_scan_bytes = account_dimension(
            self.stats.first_pass.repair_scan_bytes,
            child.accounted.repair_scan_bytes(),
            current.repair_scan_bytes(),
            self.limits.first_pass.max_total_repair_scan_bytes,
            child.reference,
            child.offset,
        )?;
        self.stats.first_pass.header_candidates = account_dimension(
            self.stats.first_pass.header_candidates,
            child.accounted.header_candidates(),
            current.header_candidates(),
            self.limits.first_pass.max_total_header_candidates,
            child.reference,
            child.offset,
        )?;
        self.stats.first_pass.boundary_candidates = account_dimension(
            self.stats.first_pass.boundary_candidates,
            child.accounted.boundary_candidates(),
            current.boundary_candidates(),
            self.limits.first_pass.max_total_boundary_candidates,
            child.reference,
            child.offset,
        )?;
        child.accounted = current;
        Ok(())
    }

    fn map_probe_error(
        &mut self,
        error: ObjectError,
        child: &ProbeChildState,
    ) -> LocalRepairOpenError {
        let Some(limit) = error.limit() else {
            return error.into();
        };
        let mapped = match limit.kind() {
            ObjectLimitKind::TotalReadBytes => Some((
                DocumentLimitKind::RepairProbeReadBytes,
                self.limits.first_pass.max_total_read_bytes,
                child.base_read_bytes,
                &mut self.stats.first_pass.read_bytes,
            )),
            ObjectLimitKind::TotalParseBytes => Some((
                DocumentLimitKind::RepairProbeParseBytes,
                self.limits.first_pass.max_total_parse_bytes,
                child.base_parse_bytes,
                &mut self.stats.first_pass.parse_bytes,
            )),
            ObjectLimitKind::RepairScanBytes => Some((
                DocumentLimitKind::RepairProbeScanBytes,
                self.limits.first_pass.max_total_repair_scan_bytes,
                child.base_scan_bytes,
                &mut self.stats.first_pass.repair_scan_bytes,
            )),
            ObjectLimitKind::RepairHeaderCandidates => Some((
                DocumentLimitKind::RepairProbeHeaderCandidates,
                self.limits.first_pass.max_total_header_candidates,
                child.base_header_candidates,
                &mut self.stats.first_pass.header_candidates,
            )),
            ObjectLimitKind::RepairBoundaryCandidates => Some((
                DocumentLimitKind::RepairProbeBoundaryCandidates,
                self.limits.first_pass.max_total_boundary_candidates,
                child.base_boundary_candidates,
                &mut self.stats.first_pass.boundary_candidates,
            )),
            _ => None,
        };
        let Some((kind, parent_limit, base, observed)) = mapped else {
            return error.into();
        };
        let lower_consumed = match base.checked_add(limit.consumed()) {
            Some(value) => value,
            None => return self.internal_error(child.reference, child.offset).into(),
        };
        // Nested candidate/replay limits report consumption relative to that lower child, while
        // scan candidate failures may report work not yet copied into the outer stats. Preserve
        // whichever view contains more of the parent-owned aggregate before adding the attempt.
        let consumed = (*observed).max(lower_consumed);
        if *observed < consumed {
            *observed = consumed;
        }
        let attempted_total = consumed.saturating_add(limit.attempted());
        if attempted_total <= parent_limit {
            return error.into();
        }
        DocumentError::aggregate_object_resource(
            kind,
            parent_limit,
            consumed,
            limit.attempted(),
            error,
            child.reference,
            child.offset,
        )
        .into()
    }

    fn internal_error(&self, reference: pdf_rs_syntax::ObjectRef, offset: u64) -> DocumentError {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(offset),
        )
    }

    fn fail_internal(&mut self) -> LocalRepairOpenPoll {
        self.fail(DocumentError::for_code(DocumentErrorCode::InternalState, None, None).into())
    }

    fn fail(&mut self, error: LocalRepairOpenError) -> LocalRepairOpenPoll {
        self.state = JobState::Failed(error);
        LocalRepairOpenPoll::Failed(error)
    }
}

impl fmt::Debug for OpenLocallyRepairedBaseRevisionJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenLocallyRepairedBaseRevisionJob")
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .field("repair_evidence", &"[REDACTED]")
            .finish()
    }
}

struct ObjectCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl pdf_rs_object::ObjectCancellation for ObjectCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn remaining_positive(
    limit: u64,
    consumed: u64,
    kind: DocumentLimitKind,
    offset: u64,
) -> Result<u64, DocumentError> {
    let remaining = limit.checked_sub(consumed).ok_or_else(|| {
        DocumentError::for_code(DocumentErrorCode::InternalState, None, Some(offset))
    })?;
    if remaining == 0 {
        return Err(DocumentError::resource(
            kind,
            limit,
            consumed,
            1,
            Some(offset),
        ));
    }
    Ok(remaining)
}

fn account_dimension(
    aggregate: u64,
    accounted: u64,
    current: u64,
    limit: u64,
    reference: pdf_rs_syntax::ObjectRef,
    offset: u64,
) -> Result<u64, DocumentError> {
    let delta = current.checked_sub(accounted).ok_or_else(|| {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(offset),
        )
    })?;
    match aggregate.checked_add(delta) {
        Some(value) if value <= limit => Ok(value),
        _ => Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(offset),
        )),
    }
}
