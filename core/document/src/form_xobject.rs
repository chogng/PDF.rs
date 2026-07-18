use std::fmt;
use std::sync::Arc;

use pdf_rs_bytes::{
    ByteRange, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceErrorCategory, SourceSnapshot,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeErrorCode, DecodeLimits,
    DecodeProfile, DecodeRequest, DecodedStream, FilterPlan, decode_stream,
};
use pdf_rs_object::{IndirectObjectValue, ObjectWorkCaps};
use pdf_rs_syntax::{ObjectRef, PdfDictionary, SyntaxObject};

use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentCancellation,
    DocumentError, DocumentErrorCode, OpenAttestedObjectJob, PageCoordinate, PageRectangle,
    PageResourceScope, PageXObjectReference, SharedAttestedRevisionIndex,
    page_materialization::parse_page_real,
};

const MAX_METADATA_ENTRIES: u64 = 512;
const MAX_ENCODED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RETAINED_BYTES: u64 = 128 * 1024 * 1024;

/// Stable reason why a selected Form XObject is outside the initial recursive subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormXObjectUnsupportedKind {
    /// The selected XObject is not a Form stream.
    NonFormXObject,
    /// `/Resources` is absent or outside the direct/one-hop-indirect dictionary subset.
    UnsupportedResources,
    /// The Form declares a filter outside the foundational stream-decoder subset.
    UnsupportedFilter,
    /// `/Matrix` uses an indirect or otherwise unsupported representation.
    UnsupportedMatrix,
    /// `/Group` selects transparency semantics outside the registered simple group subset.
    UnsupportedGroup,
}

/// Source-redacted typed Form XObject capability outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormXObjectUnsupported {
    kind: FormXObjectUnsupportedKind,
    reference: ObjectRef,
    offset: u64,
}

impl FormXObjectUnsupported {
    const fn new(kind: FormXObjectUnsupportedKind, reference: ObjectRef, offset: u64) -> Self {
        Self {
            kind,
            reference,
            offset,
        }
    }

    /// Returns the stable unsupported capability kind.
    pub const fn kind(self) -> FormXObjectUnsupportedKind {
        self.kind
    }

    /// Returns the selected indirect object identity.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the exact physical source offset associated with the capability.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns a stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            FormXObjectUnsupportedKind::NonFormXObject => "RPE-DOCUMENT-FORM-0001",
            FormXObjectUnsupportedKind::UnsupportedResources => "RPE-DOCUMENT-FORM-0002",
            FormXObjectUnsupportedKind::UnsupportedFilter => "RPE-DOCUMENT-FORM-0003",
            FormXObjectUnsupportedKind::UnsupportedMatrix => "RPE-DOCUMENT-FORM-0004",
            FormXObjectUnsupportedKind::UnsupportedGroup => "RPE-DOCUMENT-FORM-0005",
        }
    }
}

/// Runtime identity and exact checkpoints for one Form object, an optional indirect Resources
/// dictionary, and payload acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormXObjectJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    resources_envelope_checkpoint: ResumeCheckpoint,
    resources_boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl FormXObjectJobContext {
    /// Creates a context whose proof-preserving checkpoints remain runtime-owned.
    pub const fn new(
        job: JobId,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        resources_envelope_checkpoint: ResumeCheckpoint,
        resources_boundary_checkpoint: ResumeCheckpoint,
        payload_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            resources_envelope_checkpoint,
            resources_boundary_checkpoint,
            payload_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the object-envelope checkpoint.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the stream-boundary checkpoint.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the indirect Resources object envelope checkpoint.
    pub const fn resources_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.resources_envelope_checkpoint
    }

    /// Returns the indirect Resources object boundary checkpoint.
    pub const fn resources_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.resources_boundary_checkpoint
    }

    /// Returns the exact payload checkpoint.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }

    /// Returns the scheduling priority copied to object and payload requests.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Public phase of one proof-bound Form XObject acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormXObjectPhase {
    /// Reopening and inspecting the Form stream object.
    Object,
    /// Reopening an indirect Resources dictionary selected by the Form.
    Resources,
    /// Reading and decoding the exact Form content payload.
    Payload,
    /// A proof-bearing Form was published.
    Ready,
    /// A typed unsupported capability was reached.
    Unsupported,
    /// A structured terminal failure was reached.
    Failed,
}

/// Deterministic work and retained-state accounting for one acquired Form.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FormXObjectStats {
    object_read_bytes: u64,
    object_parse_bytes: u64,
    metadata_entries: u64,
    encoded_bytes: u64,
    decoded_bytes: u64,
    decode_fuel: u64,
    retained_bytes: u64,
}

impl FormXObjectStats {
    /// Returns exact object-read bytes.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns object parser-window bytes.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns top-level Form dictionary entries inspected.
    pub const fn metadata_entries(self) -> u64 {
        self.metadata_entries
    }

    /// Returns exact identity-encoded content bytes retained.
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }

    /// Returns exact bytes published after the canonical Form filter plan.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns deterministic foundational stream-decoder fuel consumed.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }

    /// Returns conservatively accounted retained bytes.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
}

/// One proof-bearing decoded Form XObject and its resource scope.
pub struct AcquiredFormXObject {
    proof: PageXObjectReference,
    resources: PageResourceScope,
    form_object: Option<AttestedObject>,
    bbox: PageRectangle,
    matrix: [PageCoordinate; 6],
    simple_transparency_group: bool,
    content: DecodedStream,
    stats: FormXObjectStats,
}

impl AcquiredFormXObject {
    /// Returns the Page resource proof that selected this Form.
    pub const fn proof(&self) -> PageXObjectReference {
        self.proof
    }

    /// Returns the Form object identity.
    pub const fn reference(&self) -> ObjectRef {
        self.proof.target()
    }

    /// Borrows the direct resource scope owned by the Form stream dictionary.
    pub const fn resources(&self) -> &PageResourceScope {
        &self.resources
    }

    /// Returns the required Form bounding box.
    pub const fn bbox(&self) -> PageRectangle {
        self.bbox
    }

    /// Returns the Form matrix in PDF six-number order.
    pub const fn matrix(&self) -> [PageCoordinate; 6] {
        self.matrix
    }

    /// Reports the accepted DeviceRGB transparency-group declaration.
    pub const fn simple_transparency_group(&self) -> bool {
        self.simple_transparency_group
    }

    /// Borrows the exact decoded Form content bytes.
    pub fn content_bytes(&self) -> &[u8] {
        self.content.bytes()
    }

    /// Borrows the proof-bearing decoded Form content.
    pub const fn content(&self) -> &DecodedStream {
        &self.content
    }

    /// Borrows the retained Form stream object when its Resources dictionary is indirect.
    ///
    /// Direct Resources are owned by [`Self::resources`], so this returns `None` in that case.
    pub const fn form_object(&self) -> Option<&AttestedObject> {
        self.form_object.as_ref()
    }

    /// Returns deterministic acquisition accounting.
    pub const fn stats(&self) -> FormXObjectStats {
        self.stats
    }
}

impl fmt::Debug for AcquiredFormXObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredFormXObject")
            .field("reference", &self.reference())
            .field("bbox", &self.bbox)
            .field("matrix", &self.matrix)
            .field("simple_transparency_group", &self.simple_transparency_group)
            .field("content_len", &self.content.bytes().len())
            .field("stats", &self.stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one Form XObject acquisition.
#[allow(
    clippy::large_enum_variant,
    reason = "the terminal source proof remains inline and move-only"
)]
pub enum FormXObjectPoll {
    /// A complete proof-bearing Form is ready.
    Ready(Arc<AcquiredFormXObject>),
    /// One exact object or payload range is absent.
    Pending {
        /// One-shot data-arrival ticket.
        ticket: DataTicket,
        /// Canonical missing ranges.
        missing: SmallRanges,
        /// Exact checkpoint for requeueing.
        checkpoint: ResumeCheckpoint,
    },
    /// A valid Form capability is outside the registered subset.
    Unsupported(FormXObjectUnsupported),
    /// Acquisition failed structurally or by resource policy.
    Failed(DocumentError),
}

impl fmt::Debug for FormXObjectPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(form) => formatter.debug_tuple("Ready").field(form).finish(),
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
            Self::Unsupported(value) => formatter.debug_tuple("Unsupported").field(value).finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

struct FormMetadata {
    bbox: PageRectangle,
    matrix: [PageCoordinate; 6],
    resources: FormResourcesPlan,
    filter_plan: FilterPlan,
    simple_transparency_group: bool,
}

enum FormResourcesPlan {
    Direct { offset: u64 },
    Indirect { reference: ObjectRef, offset: u64 },
}

#[derive(Default)]
struct FormEntries<'a> {
    type_value: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    subtype: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    bbox: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    resources: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    matrix: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    group: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    filter: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
    decode_parameters: Option<&'a pdf_rs_syntax::Located<SyntaxObject>>,
}

enum FormState {
    Object,
    Resources,
    Payload,
    Ready(Arc<AcquiredFormXObject>),
    Unsupported(FormXObjectUnsupported),
    Failed(DocumentError),
}

/// Resumable proof-bound acquisition of one decoded Form XObject.
pub struct AcquireFormXObjectJob {
    authority: SharedAttestedRevisionIndex,
    snapshot: SourceSnapshot,
    proof: PageXObjectReference,
    context: FormXObjectJobContext,
    stats: FormXObjectStats,
    object_job: Option<OpenAttestedObjectJob>,
    resources_job: Option<OpenAttestedObjectJob>,
    object: Option<AttestedObject>,
    resources_object: Option<AttestedObject>,
    form_object_read_bytes: u64,
    form_object_parse_bytes: u64,
    metadata: Option<FormMetadata>,
    state: FormState,
}

impl AcquireFormXObjectJob {
    /// Returns the immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the selected Page resource proof.
    pub const fn proof(&self) -> PageXObjectReference {
        self.proof
    }

    /// Returns runtime identity and checkpoints.
    pub const fn context(&self) -> FormXObjectJobContext {
        self.context
    }

    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> FormXObjectStats {
        self.stats
    }

    /// Returns the public resumable phase.
    pub const fn phase(&self) -> FormXObjectPhase {
        match self.state {
            FormState::Object => FormXObjectPhase::Object,
            FormState::Resources => FormXObjectPhase::Resources,
            FormState::Payload => FormXObjectPhase::Payload,
            FormState::Ready(_) => FormXObjectPhase::Ready,
            FormState::Unsupported(_) => FormXObjectPhase::Unsupported,
            FormState::Failed(_) => FormXObjectPhase::Failed,
        }
    }

    /// Advances object and payload acquisition.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> FormXObjectPoll {
        match &self.state {
            FormState::Ready(form) => return FormXObjectPoll::Ready(Arc::clone(form)),
            FormState::Unsupported(value) => return FormXObjectPoll::Unsupported(*value),
            FormState::Failed(error) => return FormXObjectPoll::Failed(*error),
            FormState::Object | FormState::Resources | FormState::Payload => {}
        }
        if let Err(error) = runtime_guard(
            self.snapshot,
            self.proof.target(),
            source,
            cancellation,
            None,
        ) {
            return self.fail(error);
        }
        loop {
            match &mut self.state {
                FormState::Object => {
                    let Some(job) = self.object_job.as_mut() else {
                        return self.fail(internal(self.proof.target(), None));
                    };
                    let outcome = job.poll(source, cancellation);
                    self.stats.object_read_bytes = job.stats().read_bytes();
                    self.stats.object_parse_bytes = job.stats().parse_bytes();
                    match outcome {
                        AttestedObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            return FormXObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        AttestedObjectPoll::Failed(error) => return self.fail(error),
                        AttestedObjectPoll::Ready(object) => {
                            match inspect_form(&object, &mut self.stats, cancellation) {
                                Ok(Ok(metadata)) => {
                                    let resources_reference = match metadata.resources {
                                        FormResourcesPlan::Direct { .. } => None,
                                        FormResourcesPlan::Indirect { reference, .. } => {
                                            Some(reference)
                                        }
                                    };
                                    self.form_object_read_bytes = self.stats.object_read_bytes;
                                    self.form_object_parse_bytes = self.stats.object_parse_bytes;
                                    self.object_job = None;
                                    self.object = Some(object);
                                    self.metadata = Some(metadata);
                                    if let Some(reference) = resources_reference {
                                        if let Err(error) = self.start_resources_job(reference) {
                                            return self.fail(error);
                                        }
                                        self.state = FormState::Resources;
                                    } else {
                                        self.state = FormState::Payload;
                                    }
                                }
                                Ok(Err(unsupported)) => return self.unsupported(unsupported),
                                Err(error) => return self.fail(error),
                            }
                        }
                    }
                }
                FormState::Resources => {
                    let Some(job) = self.resources_job.as_mut() else {
                        return self.fail(internal(self.proof.target(), None));
                    };
                    let outcome = job.poll(source, cancellation);
                    self.stats.object_read_bytes = match self
                        .form_object_read_bytes
                        .checked_add(job.stats().read_bytes())
                    {
                        Some(value) => value,
                        None => return self.fail(internal(self.proof.target(), None)),
                    };
                    self.stats.object_parse_bytes = match self
                        .form_object_parse_bytes
                        .checked_add(job.stats().parse_bytes())
                    {
                        Some(value) => value,
                        None => return self.fail(internal(self.proof.target(), None)),
                    };
                    match outcome {
                        AttestedObjectPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            return FormXObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        AttestedObjectPoll::Failed(error) => return self.fail(error),
                        AttestedObjectPoll::Ready(object) => {
                            let expected =
                                match self.metadata.as_ref().map(|value| &value.resources) {
                                    Some(FormResourcesPlan::Indirect { reference, .. }) => {
                                        *reference
                                    }
                                    _ => return self.fail(internal(self.proof.target(), None)),
                                };
                            if object.reference() != expected
                                || !matches!(
                                    object.value(),
                                    IndirectObjectValue::Direct(value)
                                        if matches!(value.value(), SyntaxObject::Dictionary(_))
                                )
                            {
                                return self.unsupported(FormXObjectUnsupported::new(
                                    FormXObjectUnsupportedKind::UnsupportedResources,
                                    expected,
                                    object.object_span().start(),
                                ));
                            }
                            self.resources_job = None;
                            self.resources_object = Some(object);
                            self.state = FormState::Payload;
                        }
                    }
                }
                FormState::Payload => {
                    let Some(object_ref) = self.object.as_ref() else {
                        return self.fail(internal(self.proof.target(), None));
                    };
                    let IndirectObjectValue::Stream(stream) = object_ref.value() else {
                        return self.fail(internal(self.proof.target(), None));
                    };
                    let dictionary_span = stream.dictionary().span();
                    let span = stream.data_span();
                    if span.is_empty() || span.len() > MAX_ENCODED_BYTES {
                        return self.fail(invalid_form(self.proof.target(), span.start()));
                    }
                    let range = match ByteRange::new(span.start(), span.len()) {
                        Ok(range) => range,
                        Err(_) => {
                            return self.fail(internal(self.proof.target(), Some(span.start())));
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
                        return self.fail(DocumentError::for_code(
                            DocumentErrorCode::SourceSnapshotMismatch,
                            Some(self.proof.target()),
                            Some(span.start()),
                        ));
                    }
                    if let ReadPoll::Failed(error) = &read
                        && error.category() == SourceErrorCategory::Integrity
                    {
                        return self.fail(DocumentError::from_source(*error, span.start()));
                    }
                    if let Err(error) = runtime_guard(
                        self.snapshot,
                        self.proof.target(),
                        source,
                        cancellation,
                        Some(span.start()),
                    ) {
                        return self.fail(error);
                    }
                    let encoded = match read {
                        ReadPoll::Ready(bytes) => bytes,
                        ReadPoll::Pending { ticket, missing } => {
                            return FormXObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.payload_checkpoint(),
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(DocumentError::for_code(
                                DocumentErrorCode::UnexpectedEndOfSource,
                                Some(self.proof.target()),
                                Some(span.start()),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(DocumentError::from_source(error, span.start()));
                        }
                    };
                    if encoded.range() != range {
                        return self.fail(internal(self.proof.target(), Some(span.start())));
                    }
                    let Some(object) = self.object.take() else {
                        return self.fail(internal(self.proof.target(), Some(span.start())));
                    };
                    let Some(metadata) = self.metadata.take() else {
                        return self.fail(internal(self.proof.target(), Some(span.start())));
                    };
                    let request = match DecodeRequest::new(
                        self.snapshot,
                        self.proof.target(),
                        dictionary_span,
                        span,
                        encoded,
                        metadata.filter_plan,
                        DecodeProfile::M1StrictV1,
                        DecodeLimits::default(),
                    ) {
                        Ok(request) => request,
                        Err(error) => {
                            return self.fail(map_decode_error(
                                error,
                                self.proof.target(),
                                span.start(),
                            ));
                        }
                    };
                    let decoded = match decode_stream(
                        request,
                        &FormDecodeCancellationAdapter(cancellation),
                    ) {
                        Ok(decoded) => decoded,
                        Err(error) if error.category() == DecodeErrorCategory::Unsupported => {
                            return self.unsupported(FormXObjectUnsupported::new(
                                FormXObjectUnsupportedKind::UnsupportedFilter,
                                self.proof.target(),
                                span.start(),
                            ));
                        }
                        Err(error) => {
                            return self.fail(map_decode_error(
                                error,
                                self.proof.target(),
                                span.start(),
                            ));
                        }
                    };
                    if let Err(error) = runtime_guard(
                        self.snapshot,
                        self.proof.target(),
                        source,
                        cancellation,
                        Some(span.start()),
                    ) {
                        return self.fail(error);
                    }
                    let (resources, form_object) = match metadata.resources {
                        FormResourcesPlan::Direct { offset } => {
                            match PageResourceScope::form(object, offset) {
                                Ok(resources) => (resources, None),
                                Err(error) => return self.fail(error),
                            }
                        }
                        FormResourcesPlan::Indirect { reference, offset } => {
                            let Some(resources_object) = self.resources_object.take() else {
                                return self
                                    .fail(internal(self.proof.target(), Some(span.start())));
                            };
                            let defining_object = object.reference();
                            let resources = match PageResourceScope::indirect(
                                defining_object,
                                offset,
                                vec![defining_object],
                                vec![reference],
                                resources_object,
                            ) {
                                Ok(resources) => resources,
                                Err(error) => return self.fail(error),
                            };
                            (resources, Some(object))
                        }
                    };
                    let retained_scope = match resources.checked_retained_state_bytes() {
                        Ok(value) => value,
                        Err(error) => return self.fail(error),
                    };
                    let retained = form_object
                        .as_ref()
                        .map_or(Some(retained_scope), |object| {
                            retained_scope.checked_add(object.syntax_heap_bytes())
                        })
                        .and_then(|value| {
                            value.checked_add(decoded.attestation().plan_retained_heap_bytes())
                        })
                        .and_then(|value| {
                            value.checked_add(decoded.attestation().peak_retained_capacity_bytes())
                        });
                    let Some(retained) = retained.filter(|value| *value <= MAX_RETAINED_BYTES)
                    else {
                        return self.fail(invalid_form(self.proof.target(), span.start()));
                    };
                    self.stats.encoded_bytes = span.len();
                    self.stats.decoded_bytes = decoded.len();
                    self.stats.decode_fuel = decoded.attestation().fuel_consumed();
                    self.stats.retained_bytes = retained;
                    let form = Arc::new(AcquiredFormXObject {
                        proof: self.proof,
                        resources,
                        form_object,
                        bbox: metadata.bbox,
                        matrix: metadata.matrix,
                        simple_transparency_group: metadata.simple_transparency_group,
                        content: decoded,
                        stats: self.stats,
                    });
                    self.state = FormState::Ready(Arc::clone(&form));
                    return FormXObjectPoll::Ready(form);
                }
                FormState::Ready(form) => return FormXObjectPoll::Ready(Arc::clone(form)),
                FormState::Unsupported(value) => return FormXObjectPoll::Unsupported(*value),
                FormState::Failed(error) => return FormXObjectPoll::Failed(*error),
            }
        }
    }

    fn start_resources_job(&mut self, reference: ObjectRef) -> Result<(), DocumentError> {
        if reference == self.proof.target() {
            return Err(invalid_form(reference, self.object_offset()));
        }
        let authority = self.authority.as_attested();
        let offset = authority.attestation(reference)?.xref_offset();
        let object_limits = authority.object_limits();
        let syntax_limits = authority.syntax_limits();
        let retained = syntax_limits
            .max_owned_bytes()
            .checked_add(syntax_limits.max_container_bytes())
            .ok_or_else(|| internal(reference, Some(offset)))?;
        let caps = ObjectWorkCaps::new_with_retained_bytes(
            object_limits.max_total_read_bytes(),
            object_limits.max_total_parse_bytes(),
            retained.min(MAX_RETAINED_BYTES),
        )
        .map_err(|_| internal(reference, Some(offset)))?;
        self.resources_job = Some(authority.open_object(
            reference,
            AttestedObjectJobContext::new(
                self.context.job(),
                self.context.resources_envelope_checkpoint(),
                self.context.resources_boundary_checkpoint(),
                self.context.priority(),
            ),
            caps,
        )?);
        Ok(())
    }

    fn object_offset(&self) -> u64 {
        self.object
            .as_ref()
            .map_or(0, |object| object.object_span().start())
    }

    fn unsupported(&mut self, value: FormXObjectUnsupported) -> FormXObjectPoll {
        self.state = FormState::Unsupported(value);
        FormXObjectPoll::Unsupported(value)
    }

    fn fail(&mut self, error: DocumentError) -> FormXObjectPoll {
        self.state = FormState::Failed(error);
        FormXObjectPoll::Failed(error)
    }
}

impl fmt::Debug for AcquireFormXObjectJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquireFormXObjectJob")
            .field("snapshot", &self.snapshot)
            .field("proof", &self.proof)
            .field("context", &self.context)
            .field("phase", &self.phase())
            .field("stats", &self.stats)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

impl SharedAttestedRevisionIndex {
    /// Acquires one Page-selected identity-encoded Form XObject under this strict proof.
    pub fn acquire_form_xobject(
        &self,
        proof: PageXObjectReference,
        context: FormXObjectJobContext,
    ) -> Result<AcquireFormXObjectJob, DocumentError> {
        let authority = self.as_attested();
        let target = proof.target();
        let offset = authority.attestation(target)?.xref_offset();
        if proof.snapshot() != authority.snapshot()
            || proof.revision_id() != authority.revision_id()
            || proof.revision_startxref() != authority.startxref()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(target),
                Some(offset),
            ));
        }
        let checkpoints = [
            context.object_envelope_checkpoint(),
            context.object_boundary_checkpoint(),
            context.resources_envelope_checkpoint(),
            context.resources_boundary_checkpoint(),
            context.payload_checkpoint(),
        ];
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[index + 1..].contains(checkpoint) {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidFormXObjectJobContext,
                    Some(target),
                    Some(offset),
                ));
            }
        }
        let object_limits = authority.object_limits();
        let syntax_limits = authority.syntax_limits();
        let retained = syntax_limits
            .max_owned_bytes()
            .checked_add(syntax_limits.max_container_bytes())
            .ok_or_else(|| internal(target, Some(offset)))?;
        let work_caps = ObjectWorkCaps::new_with_retained_bytes(
            object_limits.max_total_read_bytes(),
            object_limits.max_total_parse_bytes(),
            retained.min(MAX_RETAINED_BYTES),
        )
        .map_err(|_| internal(target, Some(offset)))?;
        let object = authority.open_object(
            target,
            AttestedObjectJobContext::new(
                context.job(),
                context.object_envelope_checkpoint(),
                context.object_boundary_checkpoint(),
                context.priority(),
            ),
            work_caps,
        )?;
        Ok(AcquireFormXObjectJob {
            authority: self.clone(),
            snapshot: authority.snapshot(),
            proof,
            context,
            stats: FormXObjectStats::default(),
            object_job: Some(object),
            resources_job: None,
            object: None,
            resources_object: None,
            form_object_read_bytes: 0,
            form_object_parse_bytes: 0,
            metadata: None,
            state: FormState::Object,
        })
    }
}

fn inspect_form(
    object: &AttestedObject,
    stats: &mut FormXObjectStats,
    cancellation: &dyn DocumentCancellation,
) -> Result<Result<FormMetadata, FormXObjectUnsupported>, DocumentError> {
    let reference = object.reference();
    let IndirectObjectValue::Stream(stream) = object.value() else {
        return Ok(Err(FormXObjectUnsupported::new(
            FormXObjectUnsupportedKind::NonFormXObject,
            reference,
            object.object_span().start(),
        )));
    };
    let dictionary = stream.dictionary().value();
    let dictionary_offset = stream.dictionary().span().start();
    let entries = inspect_entries(dictionary, reference, stats)?;

    match entries.type_value.map(|value| value.value()) {
        Some(SyntaxObject::Name(name)) if name.bytes() == b"XObject" => {}
        _ => return Err(invalid_form(reference, dictionary_offset)),
    }
    match entries.subtype.map(|value| value.value()) {
        Some(SyntaxObject::Name(name)) if name.bytes() == b"Form" => {}
        Some(_) => {
            return Ok(Err(FormXObjectUnsupported::new(
                FormXObjectUnsupportedKind::NonFormXObject,
                reference,
                entries
                    .subtype
                    .map_or(dictionary_offset, |value| value.span().start()),
            )));
        }
        None => return Err(invalid_form(reference, dictionary_offset)),
    }
    let filter_plan = match FilterPlan::from_pdf_dictionary(
        dictionary,
        DecodeLimits::default(),
        &FormDecodeCancellationAdapter(cancellation),
    ) {
        Ok(plan) => plan,
        Err(error) if error.category() == DecodeErrorCategory::Unsupported => {
            let offset = entries
                .filter
                .or(entries.decode_parameters)
                .map_or(dictionary_offset, |value| value.span().start());
            return Ok(Err(FormXObjectUnsupported::new(
                FormXObjectUnsupportedKind::UnsupportedFilter,
                reference,
                offset,
            )));
        }
        Err(error) => {
            return Err(map_decode_error(error, reference, dictionary_offset));
        }
    };
    if filter_plan.retained_heap_bytes().is_err() {
        let offset = entries
            .filter
            .or(entries.decode_parameters)
            .map_or(dictionary_offset, |value| value.span().start());
        return Err(internal(reference, Some(offset)));
    }
    let bbox = entries
        .bbox
        .and_then(|value| parse_rectangle(value.value()))
        .ok_or_else(|| {
            invalid_form(
                reference,
                entries
                    .bbox
                    .map_or(dictionary_offset, |value| value.span().start()),
            )
        })?;
    let resources = match entries.resources {
        Some(value) if matches!(value.value(), SyntaxObject::Dictionary(_)) => {
            FormResourcesPlan::Direct {
                offset: value.span().start(),
            }
        }
        Some(value) if matches!(value.value(), SyntaxObject::Reference(_)) => {
            let SyntaxObject::Reference(reference) = value.value() else {
                unreachable!("guarded above")
            };
            FormResourcesPlan::Indirect {
                reference: *reference,
                offset: value.span().start(),
            }
        }
        Some(value) => {
            return Ok(Err(FormXObjectUnsupported::new(
                FormXObjectUnsupportedKind::UnsupportedResources,
                reference,
                value.span().start(),
            )));
        }
        None => {
            return Ok(Err(FormXObjectUnsupported::new(
                FormXObjectUnsupportedKind::UnsupportedResources,
                reference,
                dictionary_offset,
            )));
        }
    };
    let matrix = match entries.matrix {
        None => identity_matrix(),
        Some(value) => match parse_matrix(value.value()) {
            Some(matrix) => matrix,
            None => {
                return Ok(Err(FormXObjectUnsupported::new(
                    FormXObjectUnsupportedKind::UnsupportedMatrix,
                    reference,
                    value.span().start(),
                )));
            }
        },
    };
    let simple_transparency_group = match entries.group {
        None => false,
        Some(value) => match validate_group(value.value()) {
            Some(simple) => simple,
            None => {
                return Ok(Err(FormXObjectUnsupported::new(
                    FormXObjectUnsupportedKind::UnsupportedGroup,
                    reference,
                    value.span().start(),
                )));
            }
        },
    };
    Ok(Ok(FormMetadata {
        bbox,
        matrix,
        resources,
        filter_plan,
        simple_transparency_group,
    }))
}

fn inspect_entries<'a>(
    dictionary: &'a PdfDictionary,
    reference: ObjectRef,
    stats: &mut FormXObjectStats,
) -> Result<FormEntries<'a>, DocumentError> {
    let mut found = FormEntries::default();
    for entry in dictionary.entries() {
        stats.metadata_entries = stats
            .metadata_entries
            .checked_add(1)
            .ok_or_else(|| internal(reference, Some(entry.key().span().start())))?;
        if stats.metadata_entries > MAX_METADATA_ENTRIES {
            return Err(invalid_form(reference, entry.key().span().start()));
        }
        let slot = match entry.key().value().bytes() {
            b"Type" => Some(&mut found.type_value),
            b"Subtype" => Some(&mut found.subtype),
            b"BBox" => Some(&mut found.bbox),
            b"Resources" => Some(&mut found.resources),
            b"Matrix" => Some(&mut found.matrix),
            b"Group" => Some(&mut found.group),
            b"Filter" => Some(&mut found.filter),
            b"DecodeParms" => Some(&mut found.decode_parameters),
            _ => None,
        };
        let Some(slot) = slot else {
            continue;
        };
        if slot.is_some() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(reference),
                Some(entry.key().span().start()),
            ));
        }
        *slot = Some(entry.value());
    }
    Ok(found)
}

fn parse_rectangle(value: &SyntaxObject) -> Option<PageRectangle> {
    let SyntaxObject::Array(values) = value else {
        return None;
    };
    let [left, bottom, right, top] = values.values() else {
        return None;
    };
    let first_x = parse_coordinate(left.value())?;
    let first_y = parse_coordinate(bottom.value())?;
    let second_x = parse_coordinate(right.value())?;
    let second_y = parse_coordinate(top.value())?;
    PageRectangle::new([
        first_x.min(second_x),
        first_y.min(second_y),
        first_x.max(second_x),
        first_y.max(second_y),
    ])
}

fn parse_matrix(value: &SyntaxObject) -> Option<[PageCoordinate; 6]> {
    let SyntaxObject::Array(values) = value else {
        return None;
    };
    let [a, b, c, d, e, f] = values.values() else {
        return None;
    };
    Some([
        parse_coordinate(a.value())?,
        parse_coordinate(b.value())?,
        parse_coordinate(c.value())?,
        parse_coordinate(d.value())?,
        parse_coordinate(e.value())?,
        parse_coordinate(f.value())?,
    ])
}

fn identity_matrix() -> [PageCoordinate; 6] {
    [
        PageCoordinate::from_scaled(PageCoordinate::SCALE),
        PageCoordinate::ZERO,
        PageCoordinate::ZERO,
        PageCoordinate::from_scaled(PageCoordinate::SCALE),
        PageCoordinate::ZERO,
        PageCoordinate::ZERO,
    ]
}

fn parse_coordinate(value: &SyntaxObject) -> Option<PageCoordinate> {
    match value {
        SyntaxObject::Integer(value) => PageCoordinate::from_integer(*value),
        SyntaxObject::Real(value) => parse_page_real(value).map(PageCoordinate::from_scaled),
        _ => None,
    }
}

fn validate_group(value: &SyntaxObject) -> Option<bool> {
    let SyntaxObject::Dictionary(dictionary) = value else {
        return None;
    };
    let mut group_type = false;
    let mut subtype = false;
    let mut color_space = false;
    for entry in dictionary.entries() {
        match entry.key().value().bytes() {
            b"Type" => {
                group_type = matches!(entry.value().value(), SyntaxObject::Name(name) if name.bytes() == b"Group");
            }
            b"S" => {
                subtype = matches!(
                    entry.value().value(),
                    SyntaxObject::Name(name) if name.bytes() == b"Transparency"
                );
            }
            b"CS" => {
                color_space = matches!(
                    entry.value().value(),
                    SyntaxObject::Name(name) if name.bytes() == b"DeviceRGB"
                );
            }
            b"I" | b"K" => {
                if !matches!(
                    entry.value().value(),
                    SyntaxObject::Boolean(false) | SyntaxObject::Null
                ) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (group_type && subtype && color_space).then_some(true)
}

struct FormDecodeCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl DecodeCancellation for FormDecodeCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn map_decode_error(error: DecodeError, reference: ObjectRef, offset: u64) -> DocumentError {
    let code = match error.category() {
        DecodeErrorCategory::Configuration => DocumentErrorCode::InvalidLimits,
        DecodeErrorCategory::Syntax => DocumentErrorCode::InvalidFormXObject,
        DecodeErrorCategory::Unsupported => DocumentErrorCode::InvalidFormXObject,
        DecodeErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
        DecodeErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
        DecodeErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
        DecodeErrorCategory::Internal => DocumentErrorCode::InternalState,
    };
    match error.code() {
        DecodeErrorCode::SourceChanged => DocumentError::for_code(
            DocumentErrorCode::SourceSnapshotMismatch,
            Some(reference),
            Some(offset),
        ),
        _ => DocumentError::for_code(code, Some(reference), Some(offset)),
    }
}

fn runtime_guard(
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    offset: Option<u64>,
) -> Result<(), DocumentError> {
    if source.snapshot() != snapshot {
        return Err(DocumentError::for_code(
            DocumentErrorCode::SourceSnapshotMismatch,
            Some(reference),
            offset,
        ));
    }
    let cancelled = cancellation.is_cancelled();
    if source.snapshot() != snapshot {
        return Err(DocumentError::for_code(
            DocumentErrorCode::SourceSnapshotMismatch,
            Some(reference),
            offset,
        ));
    }
    if cancelled {
        return Err(DocumentError::for_code(
            DocumentErrorCode::Cancelled,
            Some(reference),
            offset,
        ));
    }
    Ok(())
}

fn invalid_form(reference: ObjectRef, offset: u64) -> DocumentError {
    DocumentError::for_code(
        DocumentErrorCode::InvalidFormXObject,
        Some(reference),
        Some(offset),
    )
}

fn internal(reference: ObjectRef, offset: Option<u64>) -> DocumentError {
    DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
}
