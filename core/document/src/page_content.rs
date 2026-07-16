use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceErrorCategory, SourceSnapshot,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeErrorCode,
    DecodeFuelScheduleVersion, DecodeLimitConfig, DecodeLimitKind, DecodeLimits, DecodeProfile,
    DecodeRequest, DecodedStream, FilterPlan, decode_stream,
};
use pdf_rs_object::{
    IndirectObjectValue, ObjectErrorCode, ObjectLimitKind, ObjectStats, ObjectWorkCaps,
};
use pdf_rs_syntax::{ByteSpan, Located, ObjectRef, PdfArray, SyntaxLimitKind, SyntaxObject};

use crate::model::AttestedRevisionIndexOwner;
use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, AttestedRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind, MaterializedPage,
    OpenAttestedObjectJob, PageContentLimits, PageHandle, PageIndex, SharedAttestedRevisionIndex,
};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

/// Runtime identity and checkpoints for Page dictionary, stream framing, and payload reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageContentJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl PageContentJobContext {
    /// Creates a context whose three proof-preserving checkpoints remain runtime-owned.
    pub const fn new(
        job: JobId,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        payload_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            payload_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the checkpoint used by child object-envelope reads.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the checkpoint used by child stream-boundary reads.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the checkpoint used by exact encoded-payload reads.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }

    /// Returns the scheduling priority copied to object and payload requests.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Public resumable phase of one Page content acquisition job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageContentPhase {
    /// The exact Page dictionary is being reopened and inspected.
    Page,
    /// A whole-object Contents alias is being resolved.
    Aliases,
    /// Array-selected content-stream objects are being reopened in source order.
    Streams,
    /// One exact encoded stream payload is being acquired and decoded.
    Payload,
    /// The complete ordered content-stream value was published.
    Ready,
    /// The job reached a stable terminal failure.
    Failed,
}

/// Parent-committed and poll-boundary-observable accounting for one Page's content streams.
///
/// Failed lower parser transients and failed decode output, fuel, or retained-capacity transients
/// are not reconstructed when the lower layer does not publish them. Their rejected work remains
/// available through the terminal lower or aggregate limit evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageContentStats {
    objects_started: u64,
    reference_edges: u64,
    max_alias_depth: u64,
    array_entries: u64,
    streams: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    encoded_bytes: u64,
    decoded_bytes: u64,
    decode_fuel: u64,
    retained_state_bytes: u64,
    peak_retained_state_bytes: u64,
}

impl PageContentStats {
    /// Returns proof-preserving Page, alias, and stream object jobs started.
    pub const fn objects_started(self) -> u64 {
        self.objects_started
    }

    /// Returns Contents, alias, and array-entry reference edges followed.
    pub const fn reference_edges(self) -> u64 {
        self.reference_edges
    }

    /// Returns the greatest active whole-object Contents alias depth.
    pub const fn max_alias_depth(self) -> u64 {
        self.max_alias_depth
    }

    /// Returns direct entries accepted from the selected Contents array.
    pub const fn array_entries(self) -> u64 {
        self.array_entries
    }

    /// Returns streams successfully framed, acquired, decoded, and retained.
    pub const fn streams(self) -> u64 {
        self.streams
    }

    /// Returns cumulative exact-read bytes charged by child object jobs.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns cumulative parser-window bytes charged by child object jobs.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns cumulative exact physical stream-payload bytes.
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }

    /// Returns cumulative final decoded bytes.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns cumulative deterministic filter decode fuel for published streams.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }

    /// Returns allocator-accounted state retained by the published result.
    pub const fn retained_state_bytes(self) -> u64 {
        self.retained_state_bytes
    }

    /// Returns the greatest parent state recorded at admission, publication, or child poll bounds.
    ///
    /// This does not reconstruct unreported transient state from a failed lower parser or decoder.
    pub const fn peak_retained_state_bytes(self) -> u64 {
        self.peak_retained_state_bytes
    }
}

/// One ordered, proof-bearing Page content stream.
///
/// The value intentionally does not implement `Clone`. It retains the exact reopened object,
/// framed physical geometry, canonical filter plan, and either a nonempty encoded `ByteSlice`
/// beside its decode attestation or an explicit zero-length identity proof.
pub struct AcquiredPageContentStream {
    stream_index: u32,
    object: AttestedObject,
    decode: PageContentDecode,
}

impl AcquiredPageContentStream {
    /// Returns this stream's zero-based position in Page execution order.
    pub const fn stream_index(&self) -> u32 {
        self.stream_index
    }

    /// Returns the exact indirect stream object identity.
    pub const fn reference(&self) -> ObjectRef {
        self.object.reference()
    }

    /// Borrows the proof-bearing reopened stream object.
    pub const fn object(&self) -> &AttestedObject {
        &self.object
    }

    /// Returns the exact physical span of the stream dictionary.
    pub fn dictionary_span(&self) -> ByteSpan {
        self.decode.dictionary_span()
    }

    /// Returns the exact physical span of the encoded payload.
    pub fn data_span(&self) -> ByteSpan {
        self.decode.encoded_span()
    }

    /// Borrows the canonical ordered filter plan sealed by decoding.
    pub fn filter_plan(&self) -> &FilterPlan {
        self.decode.filter_plan()
    }

    /// Borrows the sealed decoded or zero-length identity proof.
    pub const fn decode(&self) -> &PageContentDecode {
        &self.decode
    }

    /// Borrows a normal decoder result, or `None` for a valid zero-length identity stream.
    pub fn decoded(&self) -> Option<&DecodedStream> {
        self.decode.decoded()
    }

    /// Borrows the final decoded content bytes.
    pub fn decoded_bytes(&self) -> &[u8] {
        self.decode.bytes()
    }
}

impl fmt::Debug for AcquiredPageContentStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredPageContentStream")
            .field("stream_index", &self.stream_index)
            .field("reference", &self.reference())
            .field("dictionary_span", &self.dictionary_span())
            .field("data_span", &self.data_span())
            .field("filter_plan", &self.filter_plan())
            .field("decoded", &"[REDACTED]")
            .finish()
    }
}

/// Sealed decode outcome for one Page content stream.
///
/// A valid zero-length unfiltered stream cannot be represented by the non-empty physical
/// `ByteSlice` required by the foundational decoder, so it has an explicit identity proof rather
/// than a fabricated source range.
pub enum PageContentDecode {
    /// A non-empty encoded payload decoded by the foundational filter crate.
    Decoded(DecodedStream),
    /// A valid zero-length stream whose canonical filter plan is empty.
    EmptyIdentity(EmptyIdentityContent),
}

impl PageContentDecode {
    /// Borrows the normal decode result, if the encoded payload was non-empty.
    pub const fn decoded(&self) -> Option<&DecodedStream> {
        match self {
            Self::Decoded(decoded) => Some(decoded),
            Self::EmptyIdentity(_) => None,
        }
    }

    /// Borrows the explicit zero-length identity proof, if selected.
    pub const fn empty_identity(&self) -> Option<&EmptyIdentityContent> {
        match self {
            Self::Decoded(_) => None,
            Self::EmptyIdentity(proof) => Some(proof),
        }
    }

    /// Borrows final decoded bytes; the empty identity variant returns an empty slice.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Decoded(decoded) => decoded.bytes(),
            Self::EmptyIdentity(_) => &[],
        }
    }

    /// Returns the exact physical stream dictionary span.
    pub fn dictionary_span(&self) -> ByteSpan {
        match self {
            Self::Decoded(decoded) => decoded.attestation().dictionary_span(),
            Self::EmptyIdentity(proof) => proof.dictionary_span(),
        }
    }

    /// Returns the exact physical encoded span, which may be empty only for identity proof.
    pub fn encoded_span(&self) -> ByteSpan {
        match self {
            Self::Decoded(decoded) => decoded.attestation().encoded_span(),
            Self::EmptyIdentity(proof) => proof.encoded_span(),
        }
    }

    /// Borrows the canonical ordered filter plan.
    pub fn filter_plan(&self) -> &FilterPlan {
        match self {
            Self::Decoded(decoded) => decoded.attestation().filter_plan(),
            Self::EmptyIdentity(proof) => proof.filter_plan(),
        }
    }

    fn retained_heap_bytes(&self) -> Result<u64, DocumentError> {
        match self {
            Self::Decoded(decoded) => decoded
                .attestation()
                .peak_retained_capacity_bytes()
                .checked_add(decoded.attestation().plan_retained_heap_bytes())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(decoded.attestation().owner()),
                        Some(decoded.attestation().encoded_span().start()),
                    )
                }),
            Self::EmptyIdentity(proof) => Ok(proof.plan_retained_heap_bytes),
        }
    }
}

impl fmt::Debug for PageContentDecode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decoded(decoded) => formatter.debug_tuple("Decoded").field(decoded).finish(),
            Self::EmptyIdentity(proof) => {
                formatter.debug_tuple("EmptyIdentity").field(proof).finish()
            }
        }
    }
}

/// Explicit proof for a valid zero-length content stream with no PDF filters.
///
/// This value is move-only and binds the empty result to the exact snapshot, stream owner,
/// dictionary and zero-length payload geometry, canonical empty plan, strict profile, limits, and
/// the identity layer's deterministic setup fuel.
pub struct EmptyIdentityContent {
    snapshot: SourceSnapshot,
    owner: ObjectRef,
    dictionary_span: ByteSpan,
    encoded_span: ByteSpan,
    plan: FilterPlan,
    profile: DecodeProfile,
    limits: DecodeLimits,
    fuel_schedule: DecodeFuelScheduleVersion,
    fuel_consumed: u64,
    plan_retained_heap_bytes: u64,
}

impl EmptyIdentityContent {
    /// Returns the immutable source snapshot owning the empty stream.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact indirect stream owner.
    pub const fn owner(&self) -> ObjectRef {
        self.owner
    }

    /// Returns the exact physical stream dictionary span.
    pub const fn dictionary_span(&self) -> ByteSpan {
        self.dictionary_span
    }

    /// Returns the exact valid empty payload boundary span.
    pub const fn encoded_span(&self) -> ByteSpan {
        self.encoded_span
    }

    /// Borrows the canonical empty filter plan.
    pub const fn filter_plan(&self) -> &FilterPlan {
        &self.plan
    }

    /// Returns the strict decode profile whose identity semantics were applied.
    pub const fn profile(&self) -> DecodeProfile {
        self.profile
    }

    /// Returns the deterministic decoder limits bound to this proof.
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }

    /// Returns the versioned identity-layer fuel schedule.
    pub const fn fuel_schedule(&self) -> DecodeFuelScheduleVersion {
        self.fuel_schedule
    }

    /// Returns the identity layer's deterministic setup fuel.
    pub const fn fuel_consumed(&self) -> u64 {
        self.fuel_consumed
    }

    /// Returns allocator-visible heap bytes retained by the canonical empty plan.
    pub const fn plan_retained_heap_bytes(&self) -> u64 {
        self.plan_retained_heap_bytes
    }
}

impl fmt::Debug for EmptyIdentityContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EmptyIdentityContent")
            .field("snapshot", &self.snapshot)
            .field("owner", &self.owner)
            .field("dictionary_span", &self.dictionary_span)
            .field("encoded_span", &self.encoded_span)
            .field("plan", &self.plan)
            .field("profile", &self.profile)
            .field("limits", &self.limits)
            .field("fuel_schedule", &self.fuel_schedule)
            .field("fuel_consumed", &self.fuel_consumed)
            .finish()
    }
}

/// Complete source-ordered content streams for one exact Page handle.
///
/// Absence or direct/aliased `null` is represented by an empty stream slice. The value is
/// move-only so no decoded bytes can be detached from their object and filter attestations.
pub struct AcquiredPageContent {
    page: MaterializedPage,
    streams: Vec<AcquiredPageContentStream>,
    limits: PageContentLimits,
    stats: PageContentStats,
}

impl AcquiredPageContent {
    /// Borrows the exact materialized Page and its proof-bearing Resources scope.
    pub const fn page(&self) -> &MaterializedPage {
        &self.page
    }

    /// Returns the source- and revision-bound Page handle retained by the materialized Page.
    pub const fn handle(&self) -> PageHandle {
        self.page.handle()
    }

    /// Borrows content streams in normative Page execution order.
    pub fn streams(&self) -> &[AcquiredPageContentStream] {
        &self.streams
    }

    /// Returns the number of ordered content streams.
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    /// Reports whether Contents was absent or resolved to `null` or an empty array.
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Returns the validated acquisition and decode profile.
    pub const fn limits(&self) -> PageContentLimits {
        self.limits
    }

    /// Returns deterministic acquisition, decode, and retained-state accounting.
    pub const fn stats(&self) -> PageContentStats {
        self.stats
    }
}

impl fmt::Debug for AcquiredPageContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredPageContent")
            .field("handle", &self.handle())
            .field("stream_count", &self.streams.len())
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one Page content acquisition job.
#[allow(
    clippy::large_enum_variant,
    reason = "the move-only Ready value retains proof-bearing stream ownership inline"
)]
pub enum PageContentPoll {
    /// The complete ordered content-stream sequence is ready.
    Ready(AcquiredPageContent),
    /// One object or exact stream payload requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the request.
        missing: SmallRanges,
        /// Object-envelope, stream-boundary, or payload checkpoint to retain.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable terminal failure.
    Failed(DocumentError),
}

impl fmt::Debug for PageContentPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(content) => formatter.debug_tuple("Ready").field(content).finish(),
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

#[derive(Clone, Copy)]
enum ContentState {
    Active,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
struct StreamSeed {
    reference: ObjectRef,
    edge_offset: u64,
}

struct AliasState {
    chain: Vec<ObjectRef>,
}

#[derive(Clone, Copy)]
enum CurrentTarget {
    Page,
    Alias { reference: ObjectRef },
    Stream { seed: StreamSeed, index: u32 },
}

struct ChildState {
    job: OpenAttestedObjectJob,
    accounted_stats: ObjectStats,
    work_caps: ObjectWorkCaps,
    reference: ObjectRef,
    offset: u64,
}

struct ActiveStream {
    index: u32,
    object: AttestedObject,
}

enum PayloadPoll {
    Continue,
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
    },
    Failed(DocumentError),
}

struct DecodeCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl DecodeCancellation for DecodeCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParentDecodeBudget {
    None,
    DecodedBytes,
    RetainedStateBytes,
}

#[derive(Clone, Copy)]
struct CappedDecodeLimits {
    limits: DecodeLimits,
    final_output_parent: ParentDecodeBudget,
    fuel_parent: bool,
    retained_parent: bool,
    retained_consumed: u64,
}

impl CappedDecodeLimits {
    const fn intrinsic(limits: DecodeLimits) -> Self {
        Self {
            limits,
            final_output_parent: ParentDecodeBudget::None,
            fuel_parent: false,
            retained_parent: false,
            retained_consumed: 0,
        }
    }
}

/// One-shot bounded acquisition and canonical decode of one exact Page's content streams.
///
/// The job reopens the exact Page proven by the paired [`PageIndex`], accepts one unique
/// `/Contents`, follows only whole-object root aliases, opens array-selected streams in order,
/// requests each exact framed payload, and publishes only after every stream has decoded.
pub struct AcquirePageContentJob<'index> {
    authority: AttestedRevisionIndexOwner<'index>,
    snapshot: SourceSnapshot,
    context: PageContentJobContext,
    limits: PageContentLimits,
    handle: PageHandle,
    page: Option<MaterializedPage>,
    page_opened: bool,
    active_alias: Option<AliasState>,
    stream_seeds: Vec<StreamSeed>,
    next_stream_seed: usize,
    current: Option<CurrentTarget>,
    child: Option<ChildState>,
    active_stream: Option<ActiveStream>,
    streams: Vec<AcquiredPageContentStream>,
    result_heap_bytes: u64,
    stats: PageContentStats,
    state: ContentState,
    terminal_error: DocumentError,
}

impl AcquirePageContentJob<'_> {
    /// Returns the immutable source snapshot covered by the authority and Page handle.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns runtime identity, object checkpoints, payload checkpoint, and priority.
    pub const fn context(&self) -> PageContentJobContext {
        self.context
    }

    /// Returns the validated acquisition and decode profile.
    pub const fn limits(&self) -> PageContentLimits {
        self.limits
    }

    /// Returns the exact Page handle whose Contents is being acquired.
    pub const fn handle(&self) -> PageHandle {
        self.handle
    }

    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> PageContentStats {
        self.stats
    }

    /// Returns the public resumable acquisition phase.
    pub const fn phase(&self) -> PageContentPhase {
        match self.state {
            ContentState::Ready => PageContentPhase::Ready,
            ContentState::Failed => PageContentPhase::Failed,
            ContentState::Active if self.active_stream.is_some() => PageContentPhase::Payload,
            ContentState::Active if self.active_alias.is_some() => PageContentPhase::Aliases,
            ContentState::Active if self.page_opened => PageContentPhase::Streams,
            ContentState::Active => PageContentPhase::Page,
        }
    }

    /// Advances acquisition without platform I/O or callback-owned resumption.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PageContentPoll {
        if !matches!(self.state, ContentState::Active) {
            return PageContentPoll::Failed(self.terminal_error);
        }

        loop {
            if let Err(error) = self.runtime_guard(
                source,
                cancellation,
                self.current_reference(),
                self.current_offset(),
            ) {
                return self.fail(error);
            }

            if self.active_stream.is_some() {
                match self.poll_payload(source, cancellation) {
                    PayloadPoll::Continue => continue,
                    PayloadPoll::Pending { ticket, missing } => {
                        return PageContentPoll::Pending {
                            ticket,
                            missing,
                            checkpoint: self.context.payload_checkpoint(),
                        };
                    }
                    PayloadPoll::Failed(error) => {
                        let error = self.prioritize_runtime_error(source, cancellation, error);
                        return self.fail(error);
                    }
                }
            }

            if self.child.is_none() {
                if self.current.is_none() {
                    match self.schedule_next_target() {
                        Ok(true) => {}
                        Ok(false) => return self.finish_ready(),
                        Err(error) => {
                            let error = self.prioritize_runtime_error(source, cancellation, error);
                            return self.fail(error);
                        }
                    }
                }
                if let Err(error) = self.start_child() {
                    let error = self.prioritize_runtime_error(source, cancellation, error);
                    return self.fail(error);
                }
            }

            let Some(mut child) = self.child.take() else {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    self.current_reference(),
                    self.current_offset(),
                ));
            };
            let outcome = child.job.poll(source, cancellation);
            let lower_runtime_error = match &outcome {
                AttestedObjectPoll::Failed(error)
                    if matches!(
                        error.code(),
                        DocumentErrorCode::SourceSnapshotMismatch | DocumentErrorCode::Cancelled
                    ) =>
                {
                    Some(*error)
                }
                AttestedObjectPoll::Pending { .. }
                | AttestedObjectPoll::Ready(_)
                | AttestedObjectPoll::Failed(_) => None,
            };
            if let Err(error) = self.account_child_stats(&mut child) {
                if let Some(lower) = lower_runtime_error {
                    let lower = self.prioritize_runtime_error(source, cancellation, lower);
                    return self.fail(lower);
                }
                let error = self.prioritize_runtime_error(source, cancellation, error);
                return self.fail(error);
            }
            let child_retained = child.job.stats().retained_heap_bytes();
            if let Err(error) =
                self.refresh_peak_state_with(child_retained, child.reference, Some(child.offset))
            {
                if let Some(lower) = lower_runtime_error {
                    let lower = self.prioritize_runtime_error(source, cancellation, lower);
                    return self.fail(lower);
                }
                let error = self.prioritize_runtime_error(source, cancellation, error);
                return self.fail(error);
            }
            if let Some(lower) = lower_runtime_error {
                let lower = self.prioritize_runtime_error(source, cancellation, lower);
                return self.fail(lower);
            }
            if let Err(error) = self.runtime_guard(
                source,
                cancellation,
                Some(child.reference),
                Some(child.offset),
            ) {
                return self.fail(error);
            }

            match outcome {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    self.child = Some(child);
                    return PageContentPoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    let error = self.map_child_error(error, &child);
                    let error = self.prioritize_runtime_error(source, cancellation, error);
                    return self.fail(error);
                }
                AttestedObjectPoll::Ready(object) => {
                    let Some(target) = self.current.take() else {
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.offset),
                        ));
                    };
                    let accepted = match target {
                        CurrentTarget::Page => {
                            self.accept_page(object, cancellation, child_retained)
                        }
                        CurrentTarget::Alias { reference } => {
                            self.accept_alias(reference, object, cancellation, child_retained)
                        }
                        CurrentTarget::Stream { seed, index } => {
                            self.accept_stream(seed, index, object, child_retained)
                        }
                    };
                    if let Err(error) = accepted {
                        let error = self.prioritize_runtime_error(source, cancellation, error);
                        return self.fail(error);
                    }
                }
            }
        }
    }
}

impl AcquirePageContentJob<'_> {
    fn schedule_next_target(&mut self) -> Result<bool, DocumentError> {
        if self.current.is_some() || self.child.is_some() || self.active_stream.is_some() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        }
        if !self.page_opened {
            self.current = Some(CurrentTarget::Page);
            return Ok(true);
        }
        if let Some(alias) = &self.active_alias {
            let reference = alias.chain.last().copied().ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.handle.object()),
                    None,
                )
            })?;
            self.current = Some(CurrentTarget::Alias { reference });
            return Ok(true);
        }
        if let Some(seed) = self.stream_seeds.get(self.next_stream_seed).copied() {
            let index = u32::try_from(self.next_stream_seed).map_err(|_| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(seed.reference),
                    Some(seed.edge_offset),
                )
            })?;
            self.current = Some(CurrentTarget::Stream { seed, index });
            return Ok(true);
        }
        Ok(false)
    }

    fn start_child(&mut self) -> Result<(), DocumentError> {
        let target = self.current.ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            )
        })?;
        let reference = match target {
            CurrentTarget::Page => self.handle.object(),
            CurrentTarget::Alias { reference } => reference,
            CurrentTarget::Stream { seed, .. } => seed.reference,
        };
        let attestation = self.authority.attestation(reference)?;
        let offset = attestation.xref_offset();
        if self.stats.objects_started >= self.limits.max_objects() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentObjects,
                self.limits.max_objects(),
                self.stats.objects_started,
                1,
                reference,
                Some(offset),
            ));
        }
        let read_remaining = self
            .limits
            .max_total_object_read_bytes()
            .checked_sub(self.stats.object_read_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if read_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentObjectReadBytes,
                self.limits.max_total_object_read_bytes(),
                self.stats.object_read_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let parse_remaining = self
            .limits
            .max_total_object_parse_bytes()
            .checked_sub(self.stats.object_parse_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if parse_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentObjectParseBytes,
                self.limits.max_total_object_parse_bytes(),
                self.stats.object_parse_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let current_parent_retained = self.current_retained_state_bytes()?;
        let remaining_retained = self
            .limits
            .max_retained_state_bytes()
            .checked_sub(current_parent_retained)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let syntax_limits = self.authority.syntax_limits();
        let intrinsic_combined_retained = syntax_limits
            .max_owned_bytes()
            .checked_add(syntax_limits.max_container_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let lent_retained = remaining_retained.min(intrinsic_combined_retained);
        let work_caps = ObjectWorkCaps::new_with_retained_bytes(
            read_remaining.min(self.authority.object_limits().max_total_read_bytes()),
            parse_remaining.min(self.authority.object_limits().max_total_parse_bytes()),
            lent_retained,
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, reference, offset))?;
        let object_context = AttestedObjectJobContext::new(
            self.context.job(),
            self.context.object_envelope_checkpoint(),
            self.context.object_boundary_checkpoint(),
            self.context.priority(),
        );
        let job = self
            .authority
            .open_object(reference, object_context, work_caps)?;
        self.stats.objects_started =
            self.stats.objects_started.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        self.child = Some(ChildState {
            job,
            accounted_stats: ObjectStats::default(),
            work_caps,
            reference,
            offset,
        });
        Ok(())
    }

    fn account_child_stats(&mut self, child: &mut ChildState) -> Result<(), DocumentError> {
        let current = child.job.stats();
        let read_delta = current
            .read_bytes()
            .checked_sub(child.accounted_stats.read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        let parse_delta = current
            .parse_bytes()
            .checked_sub(child.accounted_stats.parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        self.stats.object_read_bytes = self
            .stats
            .object_read_bytes
            .checked_add(read_delta)
            .filter(|value| *value <= self.limits.max_total_object_read_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        self.stats.object_parse_bytes = self
            .stats
            .object_parse_bytes
            .checked_add(parse_delta)
            .filter(|value| *value <= self.limits.max_total_object_parse_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(child.reference),
                    Some(child.offset),
                )
            })?;
        child.accounted_stats = current;
        Ok(())
    }

    fn map_child_error(&self, error: DocumentError, child: &ChildState) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(lower) = error.object_error()
        {
            if lower.code() == ObjectErrorCode::SyntaxFailure
                && let Some(syntax_error) = lower.syntax_error()
                && let Some(limit) = syntax_error.limit()
                && limit.kind() == SyntaxLimitKind::RetainedBytes
                && let Some(child_cap) = child.work_caps.max_retained_bytes()
            {
                let syntax_limits = self.authority.syntax_limits();
                let intrinsic_combined_retained = match syntax_limits
                    .max_owned_bytes()
                    .checked_add(syntax_limits.max_container_bytes())
                {
                    Some(limit) => limit,
                    None => {
                        return DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(child.reference),
                            Some(child.offset),
                        );
                    }
                };
                if child_cap < intrinsic_combined_retained {
                    let prior_child = match child_cap.checked_sub(limit.limit()) {
                        Some(consumed) => consumed,
                        None => {
                            return DocumentError::for_code(
                                DocumentErrorCode::InternalState,
                                Some(child.reference),
                                Some(child.offset),
                            );
                        }
                    };
                    let aggregate_consumed = match self
                        .current_retained_state_bytes()
                        .and_then(|base| {
                            base.checked_add(prior_child).ok_or_else(|| {
                                DocumentError::for_code(
                                    DocumentErrorCode::InternalState,
                                    Some(child.reference),
                                    Some(child.offset),
                                )
                            })
                        })
                        .and_then(|consumed| {
                            consumed.checked_add(limit.consumed()).ok_or_else(|| {
                                DocumentError::for_code(
                                    DocumentErrorCode::InternalState,
                                    Some(child.reference),
                                    Some(child.offset),
                                )
                            })
                        }) {
                        Ok(consumed) => consumed,
                        Err(error) => return error,
                    };
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageContentRetainedStateBytes,
                        self.limits.max_retained_state_bytes(),
                        aggregate_consumed,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
            }
            let Some(limit) = lower.limit() else {
                return error;
            };
            match limit.kind() {
                ObjectLimitKind::TotalReadBytes
                    if child.work_caps.max_read_bytes()
                        < self.authority.object_limits().max_total_read_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageContentObjectReadBytes,
                        self.limits.max_total_object_read_bytes(),
                        self.stats.object_read_bytes,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
                ObjectLimitKind::TotalParseBytes
                    if child.work_caps.max_parse_bytes()
                        < self.authority.object_limits().max_total_parse_bytes() =>
                {
                    return DocumentError::aggregate_object_resource(
                        DocumentLimitKind::PageContentObjectParseBytes,
                        self.limits.max_total_object_parse_bytes(),
                        self.stats.object_parse_bytes,
                        limit.attempted(),
                        lower,
                        child.reference,
                        child.offset,
                    );
                }
                ObjectLimitKind::SourceBytes
                | ObjectLimitKind::EnvelopeBytes
                | ObjectLimitKind::BoundaryBytes
                | ObjectLimitKind::StreamBytes
                | ObjectLimitKind::TotalReadBytes
                | ObjectLimitKind::TotalParseBytes
                | ObjectLimitKind::RepairScanBytes
                | ObjectLimitKind::RepairHeaderCandidates
                | ObjectLimitKind::RepairBoundaryCandidates => {}
            }
        }
        error
    }
}

impl AcquirePageContentJob<'_> {
    fn accept_page(
        &mut self,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if self.page_opened
            || object.reference() != self.handle.object()
            || self.active_alias.is_some()
            || !self.stream_seeds.is_empty()
            || self.active_stream.is_some()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(object.reference()),
                Some(object.attestation().xref_offset()),
            ));
        }
        let page_reference = object.reference();
        let page_offset = object.attestation().xref_offset();
        let dictionary = match object.value() {
            IndirectObjectValue::Direct(value) if value.source() == self.snapshot.identity() => {
                value.value().as_dictionary().ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InvalidPageContents,
                        Some(page_reference),
                        Some(value.span().start()),
                    )
                })?
            }
            IndirectObjectValue::Direct(_) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::AttestedObjectEvidenceMismatch,
                    Some(page_reference),
                    Some(page_offset),
                ));
            }
            IndirectObjectValue::Stream(_) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageContents,
                    Some(page_reference),
                    Some(page_offset),
                ));
            }
        };
        let contents = unique_contents(dictionary.entries(), page_reference, cancellation)?;
        self.page_opened = true;
        match contents.map(|value| value.value()) {
            None | Some(SyntaxObject::Null) => self.refresh_peak_state_with(
                transient_object_bytes,
                page_reference,
                Some(page_offset),
            ),
            Some(SyntaxObject::Reference(reference)) => {
                let located = contents.expect("mapped Contents exists");
                self.start_alias(*reference, located.span().start(), transient_object_bytes)
            }
            Some(SyntaxObject::Array(array)) => {
                let located = contents.expect("mapped Contents exists");
                self.install_stream_array(
                    array,
                    located.span(),
                    page_reference,
                    cancellation,
                    transient_object_bytes,
                )
            }
            Some(SyntaxObject::Dictionary(_)) => {
                let located = contents.expect("mapped Contents exists");
                Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageContents,
                    Some(page_reference),
                    Some(located.span().start()),
                ))
            }
            Some(
                SyntaxObject::Boolean(_)
                | SyntaxObject::Integer(_)
                | SyntaxObject::Real(_)
                | SyntaxObject::Name(_)
                | SyntaxObject::String(_),
            ) => {
                let located = contents.expect("mapped Contents exists");
                Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageContents,
                    Some(page_reference),
                    Some(located.span().start()),
                ))
            }
        }
    }

    fn start_alias(
        &mut self,
        reference: ObjectRef,
        edge_offset: u64,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        self.charge_reference_edge(reference, edge_offset)?;
        let requested = capacity_bytes::<ObjectRef>(1)?;
        self.ensure_state_budget_with_extra(
            requested,
            transient_object_bytes,
            reference,
            Some(edge_offset),
        )?;
        let mut chain = Vec::new();
        chain.try_reserve_exact(1).map_err(|_| {
            DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                self.current_retained_state_bytes().unwrap_or(u64::MAX),
                requested,
                reference,
                Some(edge_offset),
            )
        })?;
        chain.push(reference);
        self.active_alias = Some(AliasState { chain });
        self.stats.max_alias_depth = self.stats.max_alias_depth.max(1);
        self.refresh_peak_state_with(transient_object_bytes, reference, Some(edge_offset))
    }

    fn accept_alias(
        &mut self,
        reference: ObjectRef,
        object: AttestedObject,
        cancellation: &dyn DocumentCancellation,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if object.reference() != reference {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(object.attestation().xref_offset()),
            ));
        }
        let terminal_offset = object.attestation().xref_offset();
        match object.value() {
            IndirectObjectValue::Stream(_) => {
                self.active_alias = None;
                self.reserve_result_capacity(
                    1,
                    reference,
                    Some(terminal_offset),
                    transient_object_bytes,
                )?;
                self.active_stream = Some(ActiveStream { index: 0, object });
                self.refresh_peak_state(reference, Some(terminal_offset))
            }
            IndirectObjectValue::Direct(value) => {
                if value.source() != self.snapshot.identity() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::AttestedObjectEvidenceMismatch,
                        Some(reference),
                        Some(terminal_offset),
                    ));
                }
                match value.value() {
                    SyntaxObject::Reference(next) => {
                        self.advance_alias(*next, value.span().start(), transient_object_bytes)
                    }
                    SyntaxObject::Array(array) => {
                        let span = value.span();
                        self.active_alias = None;
                        self.install_stream_array(
                            array,
                            span,
                            reference,
                            cancellation,
                            transient_object_bytes,
                        )
                    }
                    SyntaxObject::Null => {
                        self.active_alias = None;
                        self.refresh_peak_state_with(
                            transient_object_bytes,
                            reference,
                            Some(terminal_offset),
                        )
                    }
                    SyntaxObject::Dictionary(_) => Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidPageContents,
                        Some(reference),
                        Some(value.span().start()),
                    )),
                    SyntaxObject::Boolean(_)
                    | SyntaxObject::Integer(_)
                    | SyntaxObject::Real(_)
                    | SyntaxObject::Name(_)
                    | SyntaxObject::String(_) => Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidPageContents,
                        Some(reference),
                        Some(value.span().start()),
                    )),
                }
            }
        }
    }

    fn advance_alias(
        &mut self,
        next: ObjectRef,
        edge_offset: u64,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        let Some(alias) = self.active_alias.as_ref() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(next),
                Some(edge_offset),
            ));
        };
        let chain_len = alias.chain.len();
        let chain_capacity = alias.chain.capacity();
        let chain_depth = u64::try_from(chain_len).unwrap_or(u64::MAX);
        if chain_depth >= self.limits.max_alias_depth() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentAliasDepth,
                self.limits.max_alias_depth(),
                chain_depth,
                1,
                next,
                Some(edge_offset),
            ));
        }
        let contains_next = alias.chain.contains(&next);
        if contains_next {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageContentAliasCycle,
                Some(next),
                Some(edge_offset),
            ));
        }
        self.charge_reference_edge(next, edge_offset)?;
        let requested_capacity = chain_len.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(next),
                Some(edge_offset),
            )
        })?;
        let current_capacity_bytes = capacity_bytes::<ObjectRef>(chain_capacity)?;
        let requested_capacity_bytes = capacity_bytes::<ObjectRef>(requested_capacity)?;
        let additional = requested_capacity_bytes.saturating_sub(current_capacity_bytes);
        self.ensure_state_budget_with_extra(
            additional,
            transient_object_bytes,
            next,
            Some(edge_offset),
        )?;
        let Some(alias) = self.active_alias.as_mut() else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(next),
                Some(edge_offset),
            ));
        };
        alias.chain.try_reserve_exact(1).map_err(|_| {
            DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                u64::MAX,
                capacity_bytes::<ObjectRef>(1).unwrap_or(u64::MAX),
                next,
                Some(edge_offset),
            )
        })?;
        alias.chain.push(next);
        self.stats.max_alias_depth = self
            .stats
            .max_alias_depth
            .max(u64::try_from(alias.chain.len()).unwrap_or(u64::MAX));
        self.refresh_peak_state_with(transient_object_bytes, next, Some(edge_offset))
    }

    fn install_stream_array(
        &mut self,
        array: &PdfArray,
        array_span: ByteSpan,
        owner: ObjectRef,
        cancellation: &dyn DocumentCancellation,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if !self.stream_seeds.is_empty()
            || self.next_stream_seed != 0
            || !self.streams.is_empty()
            || self.active_stream.is_some()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(owner),
                Some(array_span.start()),
            ));
        }
        let count = u64::try_from(array.values().len()).unwrap_or(u64::MAX);
        if count > self.limits.max_array_entries() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentArrayEntries,
                self.limits.max_array_entries(),
                0,
                count,
                owner,
                Some(array_span.start()),
            ));
        }
        if count > self.limits.max_streams() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentStreams,
                self.limits.max_streams(),
                0,
                count,
                owner,
                Some(array_span.start()),
            ));
        }
        if array.values().is_empty() {
            self.stats.array_entries = 0;
            return self.refresh_peak_state_with(
                transient_object_bytes,
                owner,
                Some(array_span.start()),
            );
        }

        for (entry_index, value) in array.values().iter().enumerate() {
            if entry_index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(owner),
                    Some(value.span().start()),
                ));
            }
            match value.value() {
                SyntaxObject::Reference(_) => {}
                SyntaxObject::Array(_) | SyntaxObject::Dictionary(_) => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedPageContentsRepresentation,
                        Some(owner),
                        Some(value.span().start()),
                    ));
                }
                SyntaxObject::Null
                | SyntaxObject::Boolean(_)
                | SyntaxObject::Integer(_)
                | SyntaxObject::Real(_)
                | SyntaxObject::Name(_)
                | SyntaxObject::String(_) => {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidPageContents,
                        Some(owner),
                        Some(value.span().start()),
                    ));
                }
            }
        }

        let count_usize = array.values().len();
        let seed_bytes = capacity_bytes::<StreamSeed>(count_usize)?;
        let stream_bytes = capacity_bytes::<AcquiredPageContentStream>(count_usize)?;
        let allocation_bytes = seed_bytes.checked_add(stream_bytes).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(owner),
                Some(array_span.start()),
            )
        })?;
        self.ensure_state_budget_with_extra(
            allocation_bytes,
            transient_object_bytes,
            owner,
            Some(array_span.start()),
        )?;

        let mut seeds = Vec::new();
        seeds.try_reserve_exact(count_usize).map_err(|_| {
            DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                self.current_retained_state_bytes().unwrap_or(u64::MAX),
                seed_bytes,
                owner,
                Some(array_span.start()),
            )
        })?;
        for (entry_index, value) in array.values().iter().enumerate() {
            if entry_index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::Cancelled,
                    Some(owner),
                    Some(value.span().start()),
                ));
            }
            let SyntaxObject::Reference(reference) = value.value() else {
                unreachable!("array entries were validated above");
            };
            self.charge_reference_edge(*reference, value.span().start())?;
            seeds.push(StreamSeed {
                reference: *reference,
                edge_offset: value.span().start(),
            });
        }
        self.reserve_result_capacity(
            count_usize,
            owner,
            Some(array_span.start()),
            transient_object_bytes
                .checked_add(capacity_bytes::<StreamSeed>(seeds.capacity())?)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(owner),
                        Some(array_span.start()),
                    )
                })?,
        )?;
        self.stream_seeds = seeds;
        self.stats.array_entries = count;
        self.refresh_peak_state_with(transient_object_bytes, owner, Some(array_span.start()))
    }

    fn reserve_result_capacity(
        &mut self,
        total: usize,
        reference: ObjectRef,
        offset: Option<u64>,
        transient: u64,
    ) -> Result<(), DocumentError> {
        if self.streams.capacity() >= total {
            return Ok(());
        }
        if !self.streams.is_empty() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                offset,
            ));
        }
        let requested = capacity_bytes::<AcquiredPageContentStream>(total)?;
        self.ensure_state_budget_with_extra(requested, transient, reference, offset)?;
        self.streams.try_reserve_exact(total).map_err(|_| {
            DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                self.current_retained_state_bytes().unwrap_or(u64::MAX),
                requested,
                reference,
                offset,
            )
        })?;
        self.ensure_state_budget_with_extra(0, transient, reference, offset)
    }

    fn accept_stream(
        &mut self,
        seed: StreamSeed,
        index: u32,
        object: AttestedObject,
        transient_object_bytes: u64,
    ) -> Result<(), DocumentError> {
        if object.reference() != seed.reference
            || usize::try_from(index).ok() != Some(self.next_stream_seed)
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(seed.reference),
                Some(seed.edge_offset),
            ));
        }
        match object.value() {
            IndirectObjectValue::Stream(_) => {}
            IndirectObjectValue::Direct(value)
                if matches!(value.value(), SyntaxObject::Reference(_)) =>
            {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedPageContentsRepresentation,
                    Some(seed.reference),
                    Some(value.span().start()),
                ));
            }
            IndirectObjectValue::Direct(value) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageContents,
                    Some(seed.reference),
                    Some(value.span().start()),
                ));
            }
        }
        self.next_stream_seed = self.next_stream_seed.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(seed.reference),
                Some(seed.edge_offset),
            )
        })?;
        let object_offset = object.attestation().xref_offset();
        let _ = transient_object_bytes;
        self.active_stream = Some(ActiveStream { index, object });
        self.refresh_peak_state(seed.reference, Some(object_offset))
    }
}

impl AcquirePageContentJob<'_> {
    fn poll_payload(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PayloadPoll {
        let Some(work) = self.active_stream.take() else {
            return PayloadPoll::Failed(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                self.current_reference(),
                self.current_offset(),
            ));
        };
        let reference = work.object.reference();
        let object_offset = work.object.attestation().xref_offset();
        let (dictionary_span, data_span) = match work.object.value() {
            IndirectObjectValue::Stream(stream) => (stream.dictionary().span(), stream.data_span()),
            IndirectObjectValue::Direct(_) => {
                return PayloadPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(object_offset),
                ));
            }
        };
        if data_span.is_empty() {
            return match self.finish_empty_identity_stream(
                work,
                dictionary_span,
                data_span,
                source,
                cancellation,
            ) {
                Ok(()) => PayloadPoll::Continue,
                Err(error) => PayloadPoll::Failed(error),
            };
        }
        if let Err(error) =
            self.preflight_payload_input(data_span.len(), reference, data_span.start())
        {
            return PayloadPoll::Failed(error);
        }
        let range = match ByteRange::new(data_span.start(), data_span.len()) {
            Ok(range) => range,
            Err(_) => {
                return PayloadPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
        };
        let request = ReadRequest::new(
            range,
            self.context.priority(),
            self.context.job(),
            self.context.payload_checkpoint(),
        );
        let read = source.poll(request);
        if let ReadPoll::Ready(bytes) = &read
            && bytes.identity() != self.snapshot.identity()
        {
            return PayloadPoll::Failed(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        if let ReadPoll::Failed(error) = &read
            && error.category() == SourceErrorCategory::Integrity
        {
            return PayloadPoll::Failed(DocumentError::from_source(*error, data_span.start()));
        }
        if let Err(error) = self.runtime_guard(
            source,
            cancellation,
            Some(reference),
            Some(data_span.start()),
        ) {
            return PayloadPoll::Failed(error);
        }
        let encoded = match read {
            ReadPoll::Ready(bytes) => bytes,
            ReadPoll::Pending { ticket, missing } => {
                self.active_stream = Some(work);
                return PayloadPoll::Pending { ticket, missing };
            }
            ReadPoll::EndOfFile => {
                return PayloadPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::UnexpectedEndOfSource,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
            ReadPoll::Failed(error) => {
                return PayloadPoll::Failed(DocumentError::from_source(error, data_span.start()));
            }
        };
        if encoded.range().start() != data_span.start() || encoded.range().len() != data_span.len()
        {
            return PayloadPoll::Failed(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        self.stats.encoded_bytes = match self.stats.encoded_bytes.checked_add(data_span.len()) {
            Some(value) if value <= self.limits.max_total_encoded_bytes() => value,
            _ => {
                return PayloadPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
        };

        let object_heap_bytes = work.object.syntax_heap_bytes();
        let intrinsic_limits = CappedDecodeLimits::intrinsic(self.limits.decode_limits());
        let adapter = DecodeCancellationAdapter(cancellation);
        let dictionary = match work.object.value() {
            IndirectObjectValue::Stream(stream) => stream.dictionary().value(),
            IndirectObjectValue::Direct(_) => unreachable!("stream shape was checked above"),
        };
        let declared_filters = match FilterPlan::preflight_pdf_dictionary(
            dictionary,
            intrinsic_limits.limits,
            &adapter,
        ) {
            Ok(count) => count,
            Err(error) => {
                return PayloadPoll::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    intrinsic_limits,
                ));
            }
        };
        let admitted_filters = declared_filters.max(1);
        let plan_retained_upper_bound =
            match FilterPlan::retained_heap_upper_bound(admitted_filters) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return PayloadPoll::Failed(self.map_decode_error(
                        error,
                        reference,
                        dictionary_span.start(),
                        intrinsic_limits,
                    ));
                }
            };
        let metadata_limits = match self.remaining_decode_limits(
            reference,
            data_span.start(),
            object_heap_bytes,
            plan_retained_upper_bound,
            admitted_filters,
        ) {
            Ok(limits) => limits,
            Err(error) => return PayloadPoll::Failed(error),
        };
        let preallocated_retained = match object_heap_bytes.checked_add(plan_retained_upper_bound) {
            Some(bytes) => bytes,
            None => {
                return PayloadPoll::Failed(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
        };
        if let Err(error) =
            self.refresh_peak_state_with(preallocated_retained, reference, Some(data_span.start()))
        {
            return PayloadPoll::Failed(error);
        }
        let plan =
            match FilterPlan::from_pdf_dictionary(dictionary, metadata_limits.limits, &adapter) {
                Ok(plan) => plan,
                Err(error) => {
                    return PayloadPoll::Failed(self.map_decode_error(
                        error,
                        reference,
                        dictionary_span.start(),
                        metadata_limits,
                    ));
                }
            };
        let plan_retained_heap_bytes = match plan.retained_heap_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                return PayloadPoll::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    metadata_limits,
                ));
            }
        };
        if plan_retained_heap_bytes > plan_retained_upper_bound {
            return PayloadPoll::Failed(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(dictionary_span.start()),
            ));
        }
        let decode_limits = match self.remaining_decode_limits(
            reference,
            data_span.start(),
            object_heap_bytes,
            plan_retained_heap_bytes,
            admitted_filters,
        ) {
            Ok(limits) => limits,
            Err(error) => return PayloadPoll::Failed(error),
        };
        let decode_request = match DecodeRequest::new(
            self.snapshot,
            reference,
            dictionary_span,
            data_span,
            encoded,
            plan,
            DecodeProfile::M1StrictV1,
            decode_limits.limits,
        ) {
            Ok(request) => request,
            Err(error) => {
                return PayloadPoll::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    decode_limits,
                ));
            }
        };
        let decoded = match decode_stream(decode_request, &adapter) {
            Ok(decoded) => decoded,
            Err(error) => {
                return PayloadPoll::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    decode_limits,
                ));
            }
        };
        if let Err(error) = self.runtime_guard(
            source,
            cancellation,
            Some(reference),
            Some(data_span.start()),
        ) {
            return PayloadPoll::Failed(error);
        }

        let decoded_bytes = decoded.len();
        let decode_fuel = decoded.attestation().fuel_consumed();
        if let Err(error) = self.publish_stream(
            work,
            PageContentDecode::Decoded(decoded),
            decoded_bytes,
            decode_fuel,
            data_span.start(),
        ) {
            return PayloadPoll::Failed(error);
        }
        PayloadPoll::Continue
    }

    fn finish_empty_identity_stream(
        &mut self,
        work: ActiveStream,
        dictionary_span: ByteSpan,
        data_span: ByteSpan,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<(), DocumentError> {
        let reference = work.object.reference();
        let adapter = DecodeCancellationAdapter(cancellation);
        let dictionary = match work.object.value() {
            IndirectObjectValue::Stream(stream) => stream.dictionary().value(),
            IndirectObjectValue::Direct(_) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
        };
        let object_heap_bytes = work.object.syntax_heap_bytes();
        let intrinsic_limits = CappedDecodeLimits::intrinsic(self.limits.decode_limits());
        let declared_filters =
            FilterPlan::preflight_pdf_dictionary(dictionary, intrinsic_limits.limits, &adapter)
                .map_err(|error| {
                    self.map_decode_error(
                        error,
                        reference,
                        dictionary_span.start(),
                        intrinsic_limits,
                    )
                })?;
        if declared_filters != 0 {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageContentDecodeFailure,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        let admitted_filters = declared_filters.max(1);
        let plan_retained_upper_bound = FilterPlan::retained_heap_upper_bound(admitted_filters)
            .map_err(|error| {
                self.map_decode_error(error, reference, dictionary_span.start(), intrinsic_limits)
            })?;
        let metadata_limits = self.remaining_empty_identity_limits(
            reference,
            data_span.start(),
            object_heap_bytes,
            plan_retained_upper_bound,
            admitted_filters,
        )?;
        let preallocated_retained = object_heap_bytes
            .checked_add(plan_retained_upper_bound)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                )
            })?;
        self.refresh_peak_state_with(preallocated_retained, reference, Some(data_span.start()))?;
        let plan = FilterPlan::from_pdf_dictionary(dictionary, metadata_limits.limits, &adapter)
            .map_err(|error| {
                self.map_decode_error(error, reference, dictionary_span.start(), metadata_limits)
            })?;
        if !plan.is_empty() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::PageContentDecodeFailure,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        self.runtime_guard(
            source,
            cancellation,
            Some(reference),
            Some(data_span.start()),
        )?;
        let fuel_consumed = 1;
        let prospective_fuel = self
            .stats
            .decode_fuel
            .checked_add(fuel_consumed)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                )
            })?;
        if prospective_fuel > self.limits.max_total_decode_fuel() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodeFuel,
                self.limits.max_total_decode_fuel(),
                self.stats.decode_fuel,
                fuel_consumed,
                reference,
                Some(data_span.start()),
            ));
        }
        let plan_retained_heap_bytes = plan.retained_heap_bytes().map_err(|error| {
            self.map_decode_error(error, reference, dictionary_span.start(), metadata_limits)
        })?;
        if plan_retained_heap_bytes > plan_retained_upper_bound {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(dictionary_span.start()),
            ));
        }
        let proof_limits = self.remaining_empty_identity_limits(
            reference,
            data_span.start(),
            object_heap_bytes,
            plan_retained_heap_bytes,
            admitted_filters,
        )?;
        let retained = work
            .object
            .syntax_heap_bytes()
            .checked_add(plan_retained_heap_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(data_span.start()),
                )
            })?;
        self.ensure_state_budget_with(retained, reference, Some(data_span.start()))?;
        let profile = DecodeProfile::M1StrictV1;
        let proof = EmptyIdentityContent {
            snapshot: self.snapshot,
            owner: reference,
            dictionary_span,
            encoded_span: data_span,
            plan,
            profile,
            limits: proof_limits.limits,
            fuel_schedule: profile.fuel_schedule(),
            fuel_consumed,
            plan_retained_heap_bytes,
        };
        self.publish_stream(
            work,
            PageContentDecode::EmptyIdentity(proof),
            0,
            fuel_consumed,
            data_span.start(),
        )
    }

    fn publish_stream(
        &mut self,
        work: ActiveStream,
        decode: PageContentDecode,
        decoded_bytes: u64,
        decode_fuel: u64,
        offset: u64,
    ) -> Result<(), DocumentError> {
        let reference = work.object.reference();
        let next_decoded = self
            .stats
            .decoded_bytes
            .checked_add(decoded_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_decoded > self.limits.max_total_decoded_bytes() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodedBytes,
                self.limits.max_total_decoded_bytes(),
                self.stats.decoded_bytes,
                decoded_bytes,
                reference,
                Some(offset),
            ));
        }
        let next_fuel = self
            .stats
            .decode_fuel
            .checked_add(decode_fuel)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if next_fuel > self.limits.max_total_decode_fuel() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodeFuel,
                self.limits.max_total_decode_fuel(),
                self.stats.decode_fuel,
                decode_fuel,
                reference,
                Some(offset),
            ));
        }
        let retained_heap = work
            .object
            .syntax_heap_bytes()
            .checked_add(decode.retained_heap_bytes()?)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        self.ensure_state_budget_with(retained_heap, reference, Some(offset))?;
        self.result_heap_bytes = self
            .result_heap_bytes
            .checked_add(retained_heap)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        self.streams.push(AcquiredPageContentStream {
            stream_index: work.index,
            object: work.object,
            decode,
        });
        self.stats.streams = self.stats.streams.checked_add(1).ok_or_else(|| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            )
        })?;
        if self.stats.streams > self.limits.max_streams() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        self.stats.decoded_bytes = next_decoded;
        self.stats.decode_fuel = next_fuel;
        self.refresh_peak_state(reference, Some(offset))
    }

    fn preflight_payload_input(
        &self,
        attempted: u64,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        let encoded_remaining = self
            .limits
            .max_total_encoded_bytes()
            .checked_sub(self.stats.encoded_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let intrinsic_input = self.limits.decode_limits().max_input_bytes();
        if attempted <= intrinsic_input.min(encoded_remaining) {
            return Ok(());
        }
        if intrinsic_input <= encoded_remaining {
            Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentStreamInputBytes,
                intrinsic_input,
                0,
                attempted,
                reference,
                Some(offset),
            ))
        } else {
            Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentEncodedBytes,
                self.limits.max_total_encoded_bytes(),
                self.stats.encoded_bytes,
                attempted,
                reference,
                Some(offset),
            ))
        }
    }

    fn remaining_decode_limits(
        &self,
        reference: ObjectRef,
        offset: u64,
        object_heap_bytes: u64,
        plan_heap_bytes: u64,
        admitted_filters: u16,
    ) -> Result<CappedDecodeLimits, DocumentError> {
        let decoded_remaining = self
            .limits
            .max_total_decoded_bytes()
            .checked_sub(self.stats.decoded_bytes)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if decoded_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodedBytes,
                self.limits.max_total_decoded_bytes(),
                self.stats.decoded_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let fuel_remaining = self
            .limits
            .max_total_decode_fuel()
            .checked_sub(self.stats.decode_fuel)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if fuel_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodeFuel,
                self.limits.max_total_decode_fuel(),
                self.stats.decode_fuel,
                1,
                reference,
                Some(offset),
            ));
        }
        let current_retained = self.current_retained_state_bytes()?;
        let additional_retained =
            object_heap_bytes
                .checked_add(plan_heap_bytes)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(offset),
                    )
                })?;
        let retained_consumed = current_retained
            .checked_add(additional_retained)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let retained_remaining = self
            .limits
            .max_retained_state_bytes()
            .checked_sub(retained_consumed)
            .ok_or_else(|| {
                DocumentError::page_content_resource(
                    DocumentLimitKind::PageContentRetainedStateBytes,
                    self.limits.max_retained_state_bytes(),
                    current_retained,
                    additional_retained,
                    reference,
                    Some(offset),
                )
            })?;
        if retained_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                retained_consumed,
                1,
                reference,
                Some(offset),
            ));
        }
        let base = self.limits.decode_limits();
        if admitted_filters == 0 || admitted_filters > base.max_filters() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        let (aggregate_final_output, aggregate_final_parent) =
            if decoded_remaining <= retained_remaining {
                (decoded_remaining, ParentDecodeBudget::DecodedBytes)
            } else {
                (retained_remaining, ParentDecodeBudget::RetainedStateBytes)
            };
        let final_output = base.max_final_output_bytes().min(aggregate_final_output);
        let final_output_parent = if final_output < base.max_final_output_bytes() {
            aggregate_final_parent
        } else {
            ParentDecodeBudget::None
        };
        let retained_capacity = base.max_retained_capacity_bytes().min(retained_remaining);
        let fuel = base.max_fuel().min(fuel_remaining);
        let cancellation_interval = base.cancellation_check_interval_fuel().min(fuel);
        let limits = DecodeLimits::validate(DecodeLimitConfig {
            max_input_bytes: base.max_input_bytes(),
            max_filters: admitted_filters,
            max_layer_output_bytes: base.max_layer_output_bytes(),
            max_total_output_bytes: base.max_total_output_bytes(),
            max_final_output_bytes: final_output,
            max_retained_capacity_bytes: retained_capacity,
            max_fuel: fuel,
            cancellation_check_interval_fuel: cancellation_interval,
        })
        .map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            )
        })?;
        Ok(CappedDecodeLimits {
            limits,
            final_output_parent,
            fuel_parent: fuel < base.max_fuel(),
            retained_parent: retained_capacity < base.max_retained_capacity_bytes(),
            retained_consumed,
        })
    }

    fn remaining_empty_identity_limits(
        &self,
        reference: ObjectRef,
        offset: u64,
        object_heap_bytes: u64,
        plan_heap_bytes: u64,
        admitted_filters: u16,
    ) -> Result<CappedDecodeLimits, DocumentError> {
        let fuel_remaining = self
            .limits
            .max_total_decode_fuel()
            .checked_sub(self.stats.decode_fuel)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        if fuel_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentDecodeFuel,
                self.limits.max_total_decode_fuel(),
                self.stats.decode_fuel,
                1,
                reference,
                Some(offset),
            ));
        }
        let additional_retained =
            object_heap_bytes
                .checked_add(plan_heap_bytes)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(reference),
                        Some(offset),
                    )
                })?;
        let current_retained = self.current_retained_state_bytes()?;
        let retained_consumed = current_retained
            .checked_add(additional_retained)
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        let retained_remaining = self
            .limits
            .max_retained_state_bytes()
            .checked_sub(retained_consumed)
            .ok_or_else(|| {
                DocumentError::page_content_resource(
                    DocumentLimitKind::PageContentRetainedStateBytes,
                    self.limits.max_retained_state_bytes(),
                    current_retained,
                    additional_retained,
                    reference,
                    Some(offset),
                )
            })?;
        if retained_remaining == 0 {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                retained_consumed,
                1,
                reference,
                Some(offset),
            ));
        }

        // No lower decode is run for this explicit proof, so zero aggregate decoded-byte
        // remainder is valid. Filter-plan allocation and the nonzero lower retained profile are
        // still lent from the remaining aggregate. Only final output is reduced as needed to keep
        // the sealed profile valid; layer and cumulative output remain intrinsic per-stream caps.
        let base = self.limits.decode_limits();
        if admitted_filters == 0 || admitted_filters > base.max_filters() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            ));
        }
        let final_output = base.max_final_output_bytes().min(retained_remaining);
        let final_output_parent = if final_output < base.max_final_output_bytes() {
            ParentDecodeBudget::RetainedStateBytes
        } else {
            ParentDecodeBudget::None
        };
        let retained_capacity = base.max_retained_capacity_bytes().min(retained_remaining);
        let fuel = base.max_fuel().min(fuel_remaining);
        let cancellation_interval = base.cancellation_check_interval_fuel().min(fuel);
        let limits = DecodeLimits::validate(DecodeLimitConfig {
            max_input_bytes: base.max_input_bytes(),
            max_filters: admitted_filters,
            max_layer_output_bytes: base.max_layer_output_bytes(),
            max_total_output_bytes: base.max_total_output_bytes(),
            max_final_output_bytes: final_output,
            max_retained_capacity_bytes: retained_capacity,
            max_fuel: fuel,
            cancellation_check_interval_fuel: cancellation_interval,
        })
        .map_err(|_| {
            DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(reference),
                Some(offset),
            )
        })?;
        Ok(CappedDecodeLimits {
            limits,
            final_output_parent,
            fuel_parent: fuel < base.max_fuel(),
            retained_parent: retained_capacity < base.max_retained_capacity_bytes(),
            retained_consumed,
        })
    }

    fn map_decode_error(
        &self,
        error: DecodeError,
        reference: ObjectRef,
        offset: u64,
        capped: CappedDecodeLimits,
    ) -> DocumentError {
        if let Some(limit) = error.limit() {
            let aggregate_attempted = limit
                .attempted()
                .checked_sub(limit.consumed())
                .unwrap_or(u64::MAX);
            let intrinsic = |kind| {
                DocumentError::page_content_resource(
                    kind,
                    limit.limit(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    Some(offset),
                )
            };
            return match limit.kind() {
                DecodeLimitKind::InputBytes => {
                    intrinsic(DocumentLimitKind::PageContentStreamInputBytes)
                }
                DecodeLimitKind::FilterCount => {
                    intrinsic(DocumentLimitKind::PageContentStreamFilters)
                }
                DecodeLimitKind::FilterPlanBytes => {
                    intrinsic(DocumentLimitKind::PageContentStreamFilterPlanBytes)
                }
                DecodeLimitKind::LayerOutputBytes => {
                    intrinsic(DocumentLimitKind::PageContentStreamLayerOutputBytes)
                }
                DecodeLimitKind::TotalOutputBytes => {
                    intrinsic(DocumentLimitKind::PageContentStreamTotalOutputBytes)
                }
                DecodeLimitKind::FinalOutputBytes => match capped.final_output_parent {
                    ParentDecodeBudget::None => {
                        intrinsic(DocumentLimitKind::PageContentStreamFinalOutputBytes)
                    }
                    ParentDecodeBudget::DecodedBytes => DocumentError::page_content_resource(
                        DocumentLimitKind::PageContentDecodedBytes,
                        self.limits.max_total_decoded_bytes(),
                        self.stats.decoded_bytes.saturating_add(limit.consumed()),
                        aggregate_attempted,
                        reference,
                        Some(offset),
                    ),
                    ParentDecodeBudget::RetainedStateBytes => DocumentError::page_content_resource(
                        DocumentLimitKind::PageContentRetainedStateBytes,
                        self.limits.max_retained_state_bytes(),
                        capped.retained_consumed.saturating_add(limit.consumed()),
                        aggregate_attempted,
                        reference,
                        Some(offset),
                    ),
                },
                DecodeLimitKind::Fuel if capped.fuel_parent => {
                    DocumentError::page_content_resource(
                        DocumentLimitKind::PageContentDecodeFuel,
                        self.limits.max_total_decode_fuel(),
                        self.stats.decode_fuel.saturating_add(limit.consumed()),
                        aggregate_attempted,
                        reference,
                        Some(offset),
                    )
                }
                DecodeLimitKind::Fuel => intrinsic(DocumentLimitKind::PageContentStreamDecodeFuel),
                DecodeLimitKind::RetainedCapacityBytes if capped.retained_parent => {
                    DocumentError::page_content_resource(
                        DocumentLimitKind::PageContentRetainedStateBytes,
                        self.limits.max_retained_state_bytes(),
                        capped.retained_consumed.saturating_add(limit.consumed()),
                        aggregate_attempted,
                        reference,
                        Some(offset),
                    )
                }
                DecodeLimitKind::RetainedCapacityBytes | DecodeLimitKind::Allocation => {
                    intrinsic(DocumentLimitKind::PageContentStreamRetainedBytes)
                }
            };
        }
        let code = match error.category() {
            DecodeErrorCategory::Unsupported => DocumentErrorCode::UnsupportedPageContentFilter,
            DecodeErrorCategory::Syntax => DocumentErrorCode::PageContentDecodeFailure,
            DecodeErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
            DecodeErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
            DecodeErrorCategory::Configuration | DecodeErrorCategory::Internal => {
                DocumentErrorCode::InternalState
            }
            DecodeErrorCategory::Resource => DocumentErrorCode::InternalState,
        };
        let code = match error.code() {
            DecodeErrorCode::SourceChanged => DocumentErrorCode::SourceSnapshotMismatch,
            DecodeErrorCode::Cancelled => DocumentErrorCode::Cancelled,
            _ => code,
        };
        DocumentError::for_code(code, Some(reference), Some(offset))
    }
}

impl AcquirePageContentJob<'_> {
    fn runtime_guard(
        &self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                reference,
                offset,
            ));
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                reference,
                offset,
            ));
        }
        if cancelled {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                reference,
                offset,
            ));
        }
        Ok(())
    }

    fn prioritize_runtime_error(
        &self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
        fallback: DocumentError,
    ) -> DocumentError {
        if fallback.code() == DocumentErrorCode::SourceSnapshotMismatch {
            return fallback;
        }
        let reference = fallback.reference().or_else(|| self.current_reference());
        let offset = fallback.offset().or_else(|| self.current_offset());
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                reference,
                offset,
            );
        }
        if fallback.code() == DocumentErrorCode::Cancelled {
            return fallback;
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                reference,
                offset,
            );
        }
        if cancelled {
            return DocumentError::for_code(DocumentErrorCode::Cancelled, reference, offset);
        }
        fallback
    }

    fn charge_reference_edge(
        &mut self,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        if self.stats.reference_edges >= self.limits.max_reference_edges() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentReferenceEdges,
                self.limits.max_reference_edges(),
                self.stats.reference_edges,
                1,
                reference,
                Some(offset),
            ));
        }
        self.stats.reference_edges =
            self.stats.reference_edges.checked_add(1).ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(reference),
                    Some(offset),
                )
            })?;
        Ok(())
    }

    fn current_reference(&self) -> Option<ObjectRef> {
        self.active_stream
            .as_ref()
            .map(|stream| stream.object.reference())
            .or_else(|| {
                self.current.map(|target| match target {
                    CurrentTarget::Page => self.handle.object(),
                    CurrentTarget::Alias { reference } => reference,
                    CurrentTarget::Stream { seed, .. } => seed.reference,
                })
            })
            .or_else(|| {
                self.active_alias
                    .as_ref()
                    .and_then(|alias| alias.chain.last().copied())
            })
            .or_else(|| {
                self.stream_seeds
                    .get(self.next_stream_seed)
                    .map(|seed| seed.reference)
            })
            .or(Some(self.handle.object()))
    }

    fn current_offset(&self) -> Option<u64> {
        self.current_reference()
            .and_then(|reference| self.authority.attestation(reference).ok())
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn current_retained_state_bytes(&self) -> Result<u64, DocumentError> {
        let mut total = capacity_bytes::<AcquiredPageContentStream>(self.streams.capacity())?
            .checked_add(capacity_bytes::<StreamSeed>(self.stream_seeds.capacity())?)
            .and_then(|value| value.checked_add(self.result_heap_bytes))
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    self.current_reference(),
                    self.current_offset(),
                )
            })?;
        if let Some(alias) = &self.active_alias {
            total = total
                .checked_add(capacity_bytes::<ObjectRef>(alias.chain.capacity())?)
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        self.current_reference(),
                        self.current_offset(),
                    )
                })?;
        }
        if let Some(stream) = &self.active_stream {
            total = total
                .checked_add(stream.object.syntax_heap_bytes())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(stream.object.reference()),
                        Some(stream.object.attestation().xref_offset()),
                    )
                })?;
        }
        if let Some(child) = &self.child {
            total = total
                .checked_add(child.job.stats().retained_heap_bytes())
                .ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(child.reference),
                        Some(child.offset),
                    )
                })?;
        }
        Ok(total)
    }

    fn ensure_state_budget_with(
        &self,
        additional: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        self.ensure_state_budget_with_extra(additional, 0, reference, offset)
    }

    fn ensure_state_budget_with_extra(
        &self,
        additional: u64,
        transient: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        let consumed = self.current_retained_state_bytes()?;
        let attempted = additional.checked_add(transient).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        let prospective = consumed.checked_add(attempted).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
        })?;
        if prospective > self.limits.max_retained_state_bytes() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                consumed,
                attempted,
                reference,
                offset,
            ));
        }
        Ok(())
    }

    fn refresh_peak_state(
        &mut self,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        self.refresh_peak_state_with(0, reference, offset)
    }

    fn refresh_peak_state_with(
        &mut self,
        transient: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        let retained = self
            .current_retained_state_bytes()?
            .checked_add(transient)
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
            })?;
        if retained > self.limits.max_retained_state_bytes() {
            return Err(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                0,
                retained,
                reference,
                offset,
            ));
        }
        self.stats.peak_retained_state_bytes = self.stats.peak_retained_state_bytes.max(retained);
        Ok(())
    }

    fn finish_ready(&mut self) -> PageContentPoll {
        if self.current.is_some()
            || self.child.is_some()
            || self.active_alias.is_some()
            || self.active_stream.is_some()
            || !self.page_opened
            || self.next_stream_seed != self.stream_seeds.len()
        {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.handle.object()),
                None,
            ));
        }
        self.stream_seeds = Vec::new();
        self.next_stream_seed = 0;
        let retained = match capacity_bytes::<AcquiredPageContentStream>(self.streams.capacity())
            .and_then(|bytes| {
                bytes.checked_add(self.result_heap_bytes).ok_or_else(|| {
                    DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.handle.object()),
                        None,
                    )
                })
            }) {
            Ok(retained) => retained,
            Err(error) => return self.fail(error),
        };
        if retained > self.limits.max_retained_state_bytes() {
            return self.fail(DocumentError::page_content_resource(
                DocumentLimitKind::PageContentRetainedStateBytes,
                self.limits.max_retained_state_bytes(),
                0,
                retained,
                self.handle.object(),
                None,
            ));
        }
        self.stats.retained_state_bytes = retained;
        self.stats.peak_retained_state_bytes = self.stats.peak_retained_state_bytes.max(retained);
        let value = AcquiredPageContent {
            page: match self.page.take() {
                Some(page) => page,
                None => {
                    return self.fail(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.handle.object()),
                        None,
                    ));
                }
            },
            streams: mem::take(&mut self.streams),
            limits: self.limits,
            stats: self.stats,
        };
        self.result_heap_bytes = 0;
        self.state = ContentState::Ready;
        self.terminal_error = DocumentError::for_code(
            DocumentErrorCode::JobAlreadyComplete,
            Some(self.handle.object()),
            None,
        );
        PageContentPoll::Ready(value)
    }

    fn fail(&mut self, error: DocumentError) -> PageContentPoll {
        self.current = None;
        self.child = None;
        self.active_alias = None;
        self.active_stream = None;
        self.page = None;
        self.stream_seeds = Vec::new();
        self.next_stream_seed = 0;
        self.streams = Vec::new();
        self.result_heap_bytes = 0;
        self.state = ContentState::Failed;
        self.terminal_error = error;
        PageContentPoll::Failed(error)
    }
}

fn unique_contents<'a>(
    entries: &'a [pdf_rs_syntax::DictionaryEntry],
    reference: ObjectRef,
    cancellation: &dyn DocumentCancellation,
) -> Result<Option<&'a Located<SyntaxObject>>, DocumentError> {
    let mut contents = None;
    for (index, entry) in entries.iter().enumerate() {
        if index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(reference),
                Some(entry.key().span().start()),
            ));
        }
        if entry.key().value().bytes() != b"Contents" {
            continue;
        }
        if contents.is_some() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicatePageContents,
                Some(reference),
                Some(entry.key().span().start()),
            ));
        }
        contents = Some(entry.value());
    }
    Ok(contents)
}

fn capacity_bytes<T>(capacity: usize) -> Result<u64, DocumentError> {
    u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(mem::size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))
}

impl fmt::Debug for AcquirePageContentJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquirePageContentJob")
            .field("snapshot", &self.snapshot)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("handle", &self.handle)
            .field("phase", &self.phase())
            .field("stats", &self.stats)
            .field("pending_streams", &self.stream_seeds.len())
            .field("next_stream", &self.next_stream_seed)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl AttestedRevisionIndex {
    /// Acquires and decodes Page content streams while borrowing this strict revision proof.
    pub fn acquire_page_content(
        &self,
        page_index: &PageIndex,
        page: MaterializedPage,
        context: PageContentJobContext,
        limits: PageContentLimits,
    ) -> Result<AcquirePageContentJob<'_>, DocumentError> {
        acquire_page_content_with_owner(
            AttestedRevisionIndexOwner::Borrowed(self),
            page_index,
            page,
            context,
            limits,
        )
    }
}

impl SharedAttestedRevisionIndex {
    /// Acquires Page content in a job owning this shared strict proof handle.
    pub fn acquire_page_content_owned(
        &self,
        page_index: &PageIndex,
        page: MaterializedPage,
        context: PageContentJobContext,
        limits: PageContentLimits,
    ) -> Result<AcquirePageContentJob<'static>, DocumentError> {
        acquire_page_content_with_owner(
            AttestedRevisionIndexOwner::Shared(self.clone()),
            page_index,
            page,
            context,
            limits,
        )
    }
}

fn acquire_page_content_with_owner<'index>(
    authority: AttestedRevisionIndexOwner<'index>,
    page_index: &PageIndex,
    page: MaterializedPage,
    context: PageContentJobContext,
    limits: PageContentLimits,
) -> Result<AcquirePageContentJob<'index>, DocumentError> {
    let handle = page.handle();
    let snapshot = {
        let attested = authority.as_attested();
        let page_offset = attested.attestation(handle.object())?.xref_offset();
        let envelope = context.object_envelope_checkpoint();
        let boundary = context.object_boundary_checkpoint();
        let payload = context.payload_checkpoint();
        if envelope == boundary || envelope == payload || boundary == payload {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageContentJobContext,
                Some(handle.object()),
                Some(page_offset),
            ));
        }
        if !page_index.binding_matches(attested) {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(handle.object()),
                Some(page_offset),
            ));
        }
        page_index.validate_handle(handle)?;
        attested.snapshot()
    };
    Ok(AcquirePageContentJob {
        authority,
        snapshot,
        context,
        limits,
        handle,
        page: Some(page),
        page_opened: false,
        active_alias: None,
        stream_seeds: Vec::new(),
        next_stream_seed: 0,
        current: None,
        child: None,
        active_stream: None,
        streams: Vec::new(),
        result_heap_bytes: 0,
        stats: PageContentStats::default(),
        state: ContentState::Active,
        terminal_error: DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(handle.object()),
            None,
        ),
    })
}
