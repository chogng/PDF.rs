use std::fmt;
use std::sync::Arc;

use pdf_rs_bytes::{
    ByteRange, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceErrorCategory, SourceSnapshot,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeErrorCode, DecodeLimitConfig,
    DecodeLimitKind, DecodeLimits, DecodeProfile, DecodeRequest, DecodedStream,
    FilterDecodeParameters, FilterPlan, FilterStage, PredictorParameters, StreamFilter,
    decode_stream,
};
use pdf_rs_object::{IndirectObjectValue, ObjectErrorCode, ObjectLimitKind, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, PdfDictionary, PdfReal, SyntaxLimitKind, SyntaxObject};

use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, ImageXObjectLimits, OpenAttestedObjectJob,
    PageXObjectReference, SharedAttestedRevisionIndex,
};

const METADATA_CANCELLATION_INTERVAL: u64 = 256;
const DECODE_CONTEXT_VERSION: u64 = 1;

/// Registered reason why a Page XObject cannot be published as a basic image.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageXObjectUnsupportedKind {
    /// The Page resource category is stored in an indirect dictionary.
    IndirectXObjectDictionary,
    /// The selected Page resource embeds a direct XObject dictionary.
    DirectXObject,
    /// The selected indirect object is a whole-object reference alias.
    XObjectAlias,
    /// The selected stream is not an Image XObject.
    NonImageXObject,
    /// The image uses the stencil-mask profile.
    ImageMask,
    /// The image declares an explicit hard mask.
    ExplicitMask,
    /// The image declares a soft mask.
    SoftMask,
    /// The image color space is outside direct DeviceGray, DeviceRGB, and DeviceCMYK.
    UnsupportedColorSpace,
    /// The image component width is outside the registered eight-bit profile.
    UnsupportedBitsPerComponent,
    /// The image decode array is not the exact default for its direct device color space.
    UnsupportedDecodeArray,
    /// The image requests interpolated sampling.
    Interpolation,
    /// The image filter shape or codec is outside identity and one direct FlateDecode.
    UnsupportedFilter,
    /// The image filter parameters are recognized but outside the registered default profile.
    UnsupportedDecodeParameters,
    /// Required image metadata is stored through an indirect reference.
    IndirectMetadata,
}

/// Source-redacted typed capability outcome for Page XObject lookup or image acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageXObjectUnsupported {
    kind: ImageXObjectUnsupportedKind,
    reference: ObjectRef,
    offset: u64,
}

impl ImageXObjectUnsupported {
    pub(crate) const fn new(
        kind: ImageXObjectUnsupportedKind,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        Self {
            kind,
            reference,
            offset,
        }
    }

    /// Returns the stable unsupported capability kind.
    pub const fn kind(self) -> ImageXObjectUnsupportedKind {
        self.kind
    }

    /// Returns the relevant indirect object identity.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the exact source offset that selected the unsupported representation.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns a stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            ImageXObjectUnsupportedKind::IndirectXObjectDictionary => "RPE-DOCUMENT-XOBJECT-0001",
            ImageXObjectUnsupportedKind::DirectXObject => "RPE-DOCUMENT-XOBJECT-0002",
            ImageXObjectUnsupportedKind::XObjectAlias => "RPE-DOCUMENT-XOBJECT-0003",
            ImageXObjectUnsupportedKind::NonImageXObject => "RPE-DOCUMENT-XOBJECT-0004",
            ImageXObjectUnsupportedKind::ImageMask => "RPE-DOCUMENT-XOBJECT-0005",
            ImageXObjectUnsupportedKind::ExplicitMask => "RPE-DOCUMENT-XOBJECT-0006",
            ImageXObjectUnsupportedKind::SoftMask => "RPE-DOCUMENT-XOBJECT-0007",
            ImageXObjectUnsupportedKind::UnsupportedColorSpace => "RPE-DOCUMENT-XOBJECT-0008",
            ImageXObjectUnsupportedKind::UnsupportedBitsPerComponent => "RPE-DOCUMENT-XOBJECT-0009",
            ImageXObjectUnsupportedKind::UnsupportedDecodeArray => "RPE-DOCUMENT-XOBJECT-0010",
            ImageXObjectUnsupportedKind::Interpolation => "RPE-DOCUMENT-XOBJECT-0011",
            ImageXObjectUnsupportedKind::UnsupportedFilter => "RPE-DOCUMENT-XOBJECT-0012",
            ImageXObjectUnsupportedKind::UnsupportedDecodeParameters => "RPE-DOCUMENT-XOBJECT-0013",
            ImageXObjectUnsupportedKind::IndirectMetadata => "RPE-DOCUMENT-XOBJECT-0014",
        }
    }
}

/// Direct device color spaces registered for basic Image XObjects.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageXObjectColorSpace {
    /// One gray component per pixel.
    DeviceGray,
    /// Three red, green, and blue components per pixel.
    DeviceRgb,
    /// Four cyan, magenta, yellow, and black components per pixel.
    DeviceCmyk,
}

impl ImageXObjectColorSpace {
    /// Returns tightly interleaved components per pixel.
    pub const fn components(self) -> u8 {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRgb => 3,
            Self::DeviceCmyk => 4,
        }
    }

    const fn context_code(self) -> u64 {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRgb => 2,
            Self::DeviceCmyk => 3,
        }
    }
}

/// Runtime identity and checkpoints for one Image XObject object and payload acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageXObjectJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl ImageXObjectJobContext {
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

    /// Returns the checkpoint used by the exact encoded-payload read.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }

    /// Returns the scheduling priority copied to object and payload requests.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }
}

/// Public resumable phase of one basic Image XObject acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageXObjectPhase {
    /// The selected indirect object is being reopened and its metadata inspected.
    Object,
    /// The exact encoded stream payload is being acquired and decoded.
    Payload,
    /// A proof-bearing decoded image was published.
    Ready,
    /// A stable typed unsupported capability was reached.
    Unsupported,
    /// A stable structured failure was reached.
    Failed,
}

/// Deterministic work and retained-state accounting for one Image XObject.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImageXObjectStats {
    object_read_bytes: u64,
    object_parse_bytes: u64,
    metadata_entries: u64,
    encoded_bytes: u64,
    decoded_bytes: u64,
    decode_fuel: u64,
    retained_bytes: u64,
    peak_retained_bytes: u64,
}

impl ImageXObjectStats {
    /// Returns exact source bytes consumed while reopening the image object.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }

    /// Returns parser-window bytes consumed while reopening the image object.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }

    /// Returns top-level and nested image metadata entries visited.
    pub const fn metadata_entries(self) -> u64 {
        self.metadata_entries
    }

    /// Returns exact encoded stream-payload bytes acquired.
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }

    /// Returns exact tightly packed decoded component bytes published.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    /// Returns deterministic foundational decode fuel consumed.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }

    /// Returns conservatively accounted state retained by the published image.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns the greatest conservatively accounted retained state observed.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

/// Published proof-bearing decoded bytes and canonical metadata for one basic Image XObject.
pub struct AcquiredImageXObject {
    proof: PageXObjectReference,
    object: AttestedObject,
    width: u32,
    height: u32,
    color_space: ImageXObjectColorSpace,
    stride_bytes: u64,
    decode_context: u64,
    decoded: DecodedStream,
    limits: ImageXObjectLimits,
    stats: ImageXObjectStats,
}

impl AcquiredImageXObject {
    /// Returns the exact Page-resource lookup proof authorizing this acquisition.
    pub const fn proof(&self) -> PageXObjectReference {
        self.proof
    }

    /// Returns the exact indirect Image XObject identity.
    pub const fn reference(&self) -> ObjectRef {
        self.proof.target()
    }

    /// Borrows the proof-bound reopened stream object.
    pub const fn object(&self) -> &AttestedObject {
        &self.object
    }

    /// Returns positive pixel columns.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns positive pixel rows.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the direct registered device color space.
    pub const fn color_space(&self) -> ImageXObjectColorSpace {
        self.color_space
    }

    /// Returns tightly interleaved components per pixel.
    pub const fn components(&self) -> u8 {
        self.color_space.components()
    }

    /// Returns the registered component width, always eight.
    pub const fn bits_per_component(&self) -> u8 {
        8
    }

    /// Reports the registered nearest-sample interpolation policy, always false.
    pub const fn interpolate(&self) -> bool {
        false
    }

    /// Returns tightly packed decoded bytes per row.
    pub const fn stride_bytes(&self) -> u64 {
        self.stride_bytes
    }

    /// Borrows the sealed foundational decode proof.
    pub const fn decoded(&self) -> &DecodedStream {
        &self.decoded
    }

    /// Borrows tightly packed source-order component bytes.
    pub fn decoded_bytes(&self) -> &[u8] {
        self.decoded.bytes()
    }

    /// Borrows the canonical identity or single-Flate filter plan.
    pub const fn filter_plan(&self) -> &FilterPlan {
        self.decoded.attestation().filter_plan()
    }

    /// Returns the versioned canonical decode context used by Scene resource identity.
    pub const fn decode_context(&self) -> u64 {
        self.decode_context
    }

    /// Returns the validated image acquisition profile.
    pub const fn limits(&self) -> ImageXObjectLimits {
        self.limits
    }

    /// Returns deterministic acquisition, decode, and retained-state accounting.
    pub const fn stats(&self) -> ImageXObjectStats {
        self.stats
    }
}

impl fmt::Debug for AcquiredImageXObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredImageXObject")
            .field("reference", &self.reference())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("color_space", &self.color_space)
            .field("stride_bytes", &self.stride_bytes)
            .field("decode_context", &self.decode_context)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("decoded", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one basic Image XObject acquisition.
pub enum ImageXObjectPoll {
    /// The proof-bearing decoded image is ready.
    Ready(Arc<AcquiredImageXObject>),
    /// One object or exact payload request requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active request.
        missing: SmallRanges,
        /// Object-envelope, stream-boundary, or payload checkpoint to retain.
        checkpoint: ResumeCheckpoint,
    },
    /// The selected representation is valid but outside the registered image subset.
    Unsupported(ImageXObjectUnsupported),
    /// The job reached a stable structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for ImageXObjectPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(image) => formatter.debug_tuple("Ready").field(image).finish(),
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
            Self::Unsupported(unsupported) => formatter
                .debug_tuple("Unsupported")
                .field(unsupported)
                .finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

#[derive(Clone, Copy)]
enum RegisteredFilter {
    Identity,
    Flate {
        parameters: Option<PredictorParameters>,
    },
}

impl RegisteredFilter {
    const fn context_code(self) -> u64 {
        match self {
            Self::Identity => 0,
            Self::Flate { .. } => 1,
        }
    }
}

#[derive(Clone, Copy)]
struct ImageMetadata {
    width: u32,
    height: u32,
    color_space: ImageXObjectColorSpace,
    stride_bytes: u64,
    decoded_bytes: u64,
    filter: RegisteredFilter,
}

#[derive(Clone, Copy)]
struct ImageDecodeAdmission {
    limits: DecodeLimits,
    retained_prefix: u64,
}

struct ActiveImage {
    object: AttestedObject,
    metadata: ImageMetadata,
}

struct ChildState {
    job: OpenAttestedObjectJob,
    work_caps: ObjectWorkCaps,
}

enum ImageJobState {
    Active,
    Ready(Arc<AcquiredImageXObject>),
    Unsupported(ImageXObjectUnsupported),
    Failed(DocumentError),
}

enum MetadataOutcome {
    Ready(ImageMetadata),
    Unsupported(ImageXObjectUnsupported),
}

enum DecodeParametersOutcome {
    Ready(PredictorParameters),
    Unsupported(ImageXObjectUnsupported),
}

enum PayloadOutcome {
    Ready(Arc<AcquiredImageXObject>),
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
    },
    Unsupported(ImageXObjectUnsupported),
    Failed(DocumentError),
}

struct DecodeCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl DecodeCancellation for DecodeCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// Resumable proof-bound object acquisition and canonical decode for one basic Image XObject.
pub struct AcquireImageXObjectJob {
    authority: SharedAttestedRevisionIndex,
    snapshot: SourceSnapshot,
    proof: PageXObjectReference,
    context: ImageXObjectJobContext,
    limits: ImageXObjectLimits,
    child: Option<ChildState>,
    active: Option<ActiveImage>,
    stats: ImageXObjectStats,
    state: ImageJobState,
}

impl AcquireImageXObjectJob {
    /// Returns the immutable source snapshot covered by the lookup and revision proofs.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact Page-resource lookup proof being acquired.
    pub const fn proof(&self) -> PageXObjectReference {
        self.proof
    }

    /// Returns runtime identity, object checkpoints, payload checkpoint, and priority.
    pub const fn context(&self) -> ImageXObjectJobContext {
        self.context
    }

    /// Returns the validated image acquisition profile.
    pub const fn limits(&self) -> ImageXObjectLimits {
        self.limits
    }

    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> ImageXObjectStats {
        self.stats
    }

    /// Returns the public resumable acquisition phase.
    pub const fn phase(&self) -> ImageXObjectPhase {
        match self.state {
            ImageJobState::Ready(_) => ImageXObjectPhase::Ready,
            ImageJobState::Unsupported(_) => ImageXObjectPhase::Unsupported,
            ImageJobState::Failed(_) => ImageXObjectPhase::Failed,
            ImageJobState::Active if self.active.is_some() => ImageXObjectPhase::Payload,
            ImageJobState::Active => ImageXObjectPhase::Object,
        }
    }

    /// Advances acquisition without platform I/O or callback-owned resumption.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> ImageXObjectPoll {
        match &self.state {
            ImageJobState::Ready(image) => return ImageXObjectPoll::Ready(Arc::clone(image)),
            ImageJobState::Unsupported(unsupported) => {
                return ImageXObjectPoll::Unsupported(*unsupported);
            }
            ImageJobState::Failed(error) => return ImageXObjectPoll::Failed(*error),
            ImageJobState::Active => {}
        }
        if let Err(error) = self.runtime_guard(source, cancellation, None) {
            return self.fail(error);
        }

        if self.active.is_some() {
            return match self.poll_payload(source, cancellation) {
                PayloadOutcome::Ready(image) => self.ready(image),
                PayloadOutcome::Pending { ticket, missing } => ImageXObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint: self.context.payload_checkpoint(),
                },
                PayloadOutcome::Unsupported(unsupported) => self.unsupported(unsupported),
                PayloadOutcome::Failed(error) => {
                    let error = self.prioritize_runtime_error(source, cancellation, error);
                    self.fail(error)
                }
            };
        }

        let Some(child) = self.child.as_mut() else {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.proof.target()),
                self.current_offset(),
            ));
        };
        let outcome = child.job.poll(source, cancellation);
        self.stats.object_read_bytes = child.job.stats().read_bytes();
        self.stats.object_parse_bytes = child.job.stats().parse_bytes();
        self.stats.peak_retained_bytes = self
            .stats
            .peak_retained_bytes
            .max(child.job.stats().retained_heap_bytes());
        if let Err(error) = self.runtime_guard(source, cancellation, self.current_offset()) {
            return self.fail(error);
        }

        match outcome {
            AttestedObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => ImageXObjectPoll::Pending {
                ticket,
                missing,
                checkpoint,
            },
            AttestedObjectPoll::Failed(error) => {
                let error = self.map_child_error(error);
                let error = self.prioritize_runtime_error(source, cancellation, error);
                self.fail(error)
            }
            AttestedObjectPoll::Ready(object) => {
                self.child = None;
                match self.inspect_object(&object, source, cancellation) {
                    Ok(MetadataOutcome::Ready(metadata)) => {
                        self.stats.peak_retained_bytes = self
                            .stats
                            .peak_retained_bytes
                            .max(object.syntax_heap_bytes());
                        self.active = Some(ActiveImage { object, metadata });
                        self.poll(source, cancellation)
                    }
                    Ok(MetadataOutcome::Unsupported(unsupported)) => self.unsupported(unsupported),
                    Err(error) => {
                        let error = self.prioritize_runtime_error(source, cancellation, error);
                        self.fail(error)
                    }
                }
            }
        }
    }

    fn poll_payload(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> PayloadOutcome {
        let Some(active) = self.active.take() else {
            return PayloadOutcome::Failed(self.internal_error(None));
        };
        let reference = active.object.reference();
        let stream = match active.object.value() {
            IndirectObjectValue::Stream(stream) => stream,
            IndirectObjectValue::Direct(_) => {
                return PayloadOutcome::Failed(self.internal_error(None));
            }
        };
        let dictionary_span = stream.dictionary().span();
        let data_span = stream.data_span();
        if data_span.is_empty() {
            return PayloadOutcome::Failed(DocumentError::for_code(
                DocumentErrorCode::InvalidImageXObject,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        if let Err(error) =
            self.check_payload_geometry(active.metadata, data_span.len(), data_span.start())
        {
            return PayloadOutcome::Failed(error);
        }
        let range = match ByteRange::new(data_span.start(), data_span.len()) {
            Ok(range) => range,
            Err(_) => return PayloadOutcome::Failed(self.internal_error(Some(data_span.start()))),
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
            return PayloadOutcome::Failed(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        if let ReadPoll::Failed(error) = &read
            && error.category() == SourceErrorCategory::Integrity
        {
            return PayloadOutcome::Failed(DocumentError::from_source(*error, data_span.start()));
        }
        if let Err(error) = self.runtime_guard(source, cancellation, Some(data_span.start())) {
            return PayloadOutcome::Failed(error);
        }
        let encoded = match read {
            ReadPoll::Ready(bytes) => bytes,
            ReadPoll::Pending { ticket, missing } => {
                self.active = Some(active);
                return PayloadOutcome::Pending { ticket, missing };
            }
            ReadPoll::EndOfFile => {
                return PayloadOutcome::Failed(DocumentError::for_code(
                    DocumentErrorCode::UnexpectedEndOfSource,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
            ReadPoll::Failed(error) => {
                return PayloadOutcome::Failed(DocumentError::from_source(
                    error,
                    data_span.start(),
                ));
            }
        };
        if encoded.range().start() != data_span.start() || encoded.range().len() != data_span.len()
        {
            return PayloadOutcome::Failed(self.internal_error(Some(data_span.start())));
        }
        self.stats.encoded_bytes = data_span.len();

        let plan_upper = match FilterPlan::retained_heap_upper_bound(1) {
            Ok(value) => value,
            Err(error) => {
                return PayloadOutcome::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    active.metadata.decoded_bytes,
                    None,
                ));
            }
        };
        let object_heap = active.object.syntax_heap_bytes();
        let preallocated = match object_heap.checked_add(plan_upper) {
            Some(value) => value,
            None => return PayloadOutcome::Failed(self.internal_error(Some(data_span.start()))),
        };
        if preallocated > self.limits.max_retained_bytes() {
            return PayloadOutcome::Failed(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectRetainedBytes,
                self.limits.max_retained_bytes(),
                object_heap,
                plan_upper,
                reference,
                Some(dictionary_span.start()),
            ));
        }
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(preallocated);
        let plan = match canonical_filter_plan(active.metadata.filter) {
            Ok(plan) => plan,
            Err(error) => {
                return PayloadOutcome::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    active.metadata.decoded_bytes,
                    None,
                ));
            }
        };
        let plan_retained = match plan.retained_heap_bytes() {
            Ok(value) if value <= plan_upper => value,
            Ok(_) => return PayloadOutcome::Failed(self.internal_error(Some(data_span.start()))),
            Err(error) => {
                return PayloadOutcome::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    active.metadata.decoded_bytes,
                    None,
                ));
            }
        };
        let decode_admission = match self.decode_limits(
            active.metadata,
            data_span.len(),
            object_heap,
            plan_retained,
            data_span.start(),
        ) {
            Ok(limits) => limits,
            Err(error) => return PayloadOutcome::Failed(error),
        };
        let request = match DecodeRequest::new(
            self.snapshot,
            reference,
            dictionary_span,
            data_span,
            encoded,
            plan,
            DecodeProfile::M1StrictV1,
            decode_admission.limits,
        ) {
            Ok(request) => request,
            Err(error) => {
                return PayloadOutcome::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    active.metadata.decoded_bytes,
                    Some(decode_admission),
                ));
            }
        };
        let decoded = match decode_stream(request, &DecodeCancellationAdapter(cancellation)) {
            Ok(decoded) => decoded,
            Err(error) => {
                if error.category() == DecodeErrorCategory::Unsupported {
                    return PayloadOutcome::Unsupported(ImageXObjectUnsupported::new(
                        match error.code() {
                            DecodeErrorCode::UnsupportedDecodeParameters
                            | DecodeErrorCode::UnsupportedPredictor => {
                                ImageXObjectUnsupportedKind::UnsupportedDecodeParameters
                            }
                            _ => ImageXObjectUnsupportedKind::UnsupportedFilter,
                        },
                        reference,
                        data_span.start(),
                    ));
                }
                return PayloadOutcome::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    active.metadata.decoded_bytes,
                    Some(decode_admission),
                ));
            }
        };
        if let Err(error) = self.runtime_guard(source, cancellation, Some(data_span.start())) {
            return PayloadOutcome::Failed(error);
        }
        if decoded.len() != active.metadata.decoded_bytes {
            return PayloadOutcome::Failed(DocumentError::for_code(
                DocumentErrorCode::InvalidImageXObject,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        let decode_fuel = decoded.attestation().fuel_consumed();
        let retained = match object_heap
            .checked_add(decoded.attestation().plan_retained_heap_bytes())
            .and_then(|value| {
                value.checked_add(decoded.attestation().peak_retained_capacity_bytes())
            }) {
            Some(value) => value,
            None => return PayloadOutcome::Failed(self.internal_error(Some(data_span.start()))),
        };
        if retained > self.limits.max_retained_bytes() {
            return PayloadOutcome::Failed(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectRetainedBytes,
                self.limits.max_retained_bytes(),
                0,
                retained,
                reference,
                Some(data_span.start()),
            ));
        }
        self.stats.decoded_bytes = active.metadata.decoded_bytes;
        self.stats.decode_fuel = decode_fuel;
        self.stats.retained_bytes = retained;
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(retained);
        let image = AcquiredImageXObject {
            proof: self.proof,
            object: active.object,
            width: active.metadata.width,
            height: active.metadata.height,
            color_space: active.metadata.color_space,
            stride_bytes: active.metadata.stride_bytes,
            decode_context: decode_context(active.metadata),
            decoded,
            limits: self.limits,
            stats: self.stats,
        };
        PayloadOutcome::Ready(Arc::new(image))
    }

    fn inspect_object(
        &mut self,
        object: &AttestedObject,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<MetadataOutcome, DocumentError> {
        let reference = object.reference();
        if object.snapshot() != self.snapshot
            || object.revision_id() != self.proof.revision_id()
            || object.revision_startxref() != self.proof.revision_startxref()
        {
            return Err(self.internal_error(None));
        }
        let stream = match object.value() {
            IndirectObjectValue::Direct(value) => {
                return Ok(MetadataOutcome::Unsupported(ImageXObjectUnsupported::new(
                    match value.value() {
                        SyntaxObject::Reference(_) => ImageXObjectUnsupportedKind::XObjectAlias,
                        _ => ImageXObjectUnsupportedKind::NonImageXObject,
                    },
                    reference,
                    value.span().start(),
                )));
            }
            IndirectObjectValue::Stream(stream) => stream,
        };
        let dictionary = stream.dictionary().value();
        let slots = self.scan_metadata(dictionary, reference, source, cancellation)?;
        let dictionary_offset = stream.dictionary().span().start();

        let type_value = required_slot(slots.type_value, reference, dictionary_offset)?;
        match type_value.value() {
            SyntaxObject::Name(name) if name.bytes() == b"XObject" => {}
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, type_value.span().start()));
            }
            _ => {
                return Err(invalid_image(reference, type_value.span().start()));
            }
        }
        let subtype = required_slot(slots.subtype, reference, dictionary_offset)?;
        match subtype.value() {
            SyntaxObject::Name(name) if name.bytes() == b"Image" => {}
            SyntaxObject::Name(_) => {
                return Ok(MetadataOutcome::Unsupported(ImageXObjectUnsupported::new(
                    ImageXObjectUnsupportedKind::NonImageXObject,
                    reference,
                    subtype.span().start(),
                )));
            }
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, subtype.span().start()));
            }
            _ => return Err(invalid_image(reference, subtype.span().start())),
        }

        if let Some(mask) = slots.image_mask {
            match mask.value() {
                SyntaxObject::Boolean(false) | SyntaxObject::Null => {}
                SyntaxObject::Boolean(true) => {
                    return Ok(unsupported(
                        ImageXObjectUnsupportedKind::ImageMask,
                        reference,
                        mask.span().start(),
                    ));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(indirect_metadata(reference, mask.span().start()));
                }
                _ => return Err(invalid_image(reference, mask.span().start())),
            }
        }
        if let Some(mask) = slots.mask
            && !matches!(mask.value(), SyntaxObject::Null)
        {
            return Ok(unsupported(
                ImageXObjectUnsupportedKind::ExplicitMask,
                reference,
                mask.span().start(),
            ));
        }
        if let Some(mask) = slots.soft_mask
            && !matches!(mask.value(), SyntaxObject::Null)
        {
            return Ok(unsupported(
                ImageXObjectUnsupportedKind::SoftMask,
                reference,
                mask.span().start(),
            ));
        }

        let width_value = required_slot(slots.width, reference, dictionary_offset)?;
        let width = match width_value.value() {
            SyntaxObject::Integer(value) => u32::try_from(*value)
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| invalid_image(reference, width_value.span().start()))?,
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, width_value.span().start()));
            }
            _ => return Err(invalid_image(reference, width_value.span().start())),
        };
        check_scalar_limit(
            u64::from(width),
            u64::from(self.limits.max_width()),
            DocumentLimitKind::ImageXObjectWidth,
            reference,
            width_value.span().start(),
        )?;
        let height_value = required_slot(slots.height, reference, dictionary_offset)?;
        let height = match height_value.value() {
            SyntaxObject::Integer(value) => u32::try_from(*value)
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| invalid_image(reference, height_value.span().start()))?,
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, height_value.span().start()));
            }
            _ => return Err(invalid_image(reference, height_value.span().start())),
        };
        check_scalar_limit(
            u64::from(height),
            u64::from(self.limits.max_height()),
            DocumentLimitKind::ImageXObjectHeight,
            reference,
            height_value.span().start(),
        )?;

        let color_value = required_slot(slots.color_space, reference, dictionary_offset)?;
        let color_space = match color_value.value() {
            SyntaxObject::Name(name) => match name.bytes() {
                b"DeviceGray" => ImageXObjectColorSpace::DeviceGray,
                b"DeviceRGB" => ImageXObjectColorSpace::DeviceRgb,
                b"DeviceCMYK" => ImageXObjectColorSpace::DeviceCmyk,
                _ => {
                    return Ok(unsupported(
                        ImageXObjectUnsupportedKind::UnsupportedColorSpace,
                        reference,
                        color_value.span().start(),
                    ));
                }
            },
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, color_value.span().start()));
            }
            SyntaxObject::Array(_) => {
                return Ok(unsupported(
                    ImageXObjectUnsupportedKind::UnsupportedColorSpace,
                    reference,
                    color_value.span().start(),
                ));
            }
            _ => return Err(invalid_image(reference, color_value.span().start())),
        };

        let bits_value = required_slot(slots.bits_per_component, reference, dictionary_offset)?;
        match bits_value.value() {
            SyntaxObject::Integer(8) => {}
            SyntaxObject::Integer(_) => {
                return Ok(unsupported(
                    ImageXObjectUnsupportedKind::UnsupportedBitsPerComponent,
                    reference,
                    bits_value.span().start(),
                ));
            }
            SyntaxObject::Reference(_) => {
                return Ok(indirect_metadata(reference, bits_value.span().start()));
            }
            _ => return Err(invalid_image(reference, bits_value.span().start())),
        }

        if let Some(decode) = slots.decode {
            let SyntaxObject::Array(values) = decode.value() else {
                if matches!(decode.value(), SyntaxObject::Reference(_)) {
                    return Ok(indirect_metadata(reference, decode.span().start()));
                }
                return Err(invalid_image(reference, decode.span().start()));
            };
            let expected = usize::from(color_space.components()) * 2;
            if values.values().len() != expected {
                return Ok(unsupported(
                    ImageXObjectUnsupportedKind::UnsupportedDecodeArray,
                    reference,
                    decode.span().start(),
                ));
            }
            for (index, value) in values.values().iter().enumerate() {
                self.charge_metadata(reference, value.span().start())?;
                let expected_one = index % 2 == 1;
                if !numeric_is_exact(value.value(), expected_one) {
                    return Ok(unsupported(
                        ImageXObjectUnsupportedKind::UnsupportedDecodeArray,
                        reference,
                        value.span().start(),
                    ));
                }
            }
        }
        if let Some(interpolate) = slots.interpolate {
            match interpolate.value() {
                SyntaxObject::Boolean(false) | SyntaxObject::Null => {}
                SyntaxObject::Boolean(true) => {
                    return Ok(unsupported(
                        ImageXObjectUnsupportedKind::Interpolation,
                        reference,
                        interpolate.span().start(),
                    ));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(indirect_metadata(reference, interpolate.span().start()));
                }
                _ => return Err(invalid_image(reference, interpolate.span().start())),
            }
        }

        let filter = match slots.filter {
            None => {
                if let Some(parameters) = slots.decode_parameters
                    && !matches!(parameters.value(), SyntaxObject::Null)
                {
                    return Err(invalid_image(reference, parameters.span().start()));
                }
                RegisteredFilter::Identity
            }
            Some(value) => match value.value() {
                SyntaxObject::Name(name) if name.bytes() == b"FlateDecode" => {
                    let parameters = match slots.decode_parameters {
                        None => None,
                        Some(parameters) => match parameters.value() {
                            SyntaxObject::Null => None,
                            SyntaxObject::Dictionary(dictionary) => {
                                match self.decode_parameters(
                                    dictionary,
                                    reference,
                                    color_space,
                                    width,
                                    parameters.span().start(),
                                )? {
                                    DecodeParametersOutcome::Ready(parameters) => Some(parameters),
                                    DecodeParametersOutcome::Unsupported(unsupported) => {
                                        return Ok(MetadataOutcome::Unsupported(unsupported));
                                    }
                                }
                            }
                            SyntaxObject::Reference(_) => {
                                return Ok(indirect_metadata(reference, parameters.span().start()));
                            }
                            SyntaxObject::Array(_) => {
                                return Ok(unsupported(
                                    ImageXObjectUnsupportedKind::UnsupportedDecodeParameters,
                                    reference,
                                    parameters.span().start(),
                                ));
                            }
                            _ => {
                                return Err(invalid_image(reference, parameters.span().start()));
                            }
                        },
                    };
                    RegisteredFilter::Flate { parameters }
                }
                SyntaxObject::Name(_) | SyntaxObject::Array(_) => {
                    return Ok(unsupported(
                        ImageXObjectUnsupportedKind::UnsupportedFilter,
                        reference,
                        value.span().start(),
                    ));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(indirect_metadata(reference, value.span().start()));
                }
                _ => return Err(invalid_image(reference, value.span().start())),
            },
        };

        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| self.internal_error(Some(width_value.span().start())))?;
        check_scalar_limit(
            pixels,
            self.limits.max_pixels(),
            DocumentLimitKind::ImageXObjectPixels,
            reference,
            width_value.span().start(),
        )?;
        let stride_bytes = u64::from(width)
            .checked_mul(u64::from(color_space.components()))
            .ok_or_else(|| self.internal_error(Some(width_value.span().start())))?;
        check_scalar_limit(
            stride_bytes,
            self.limits.max_stride_bytes(),
            DocumentLimitKind::ImageXObjectStrideBytes,
            reference,
            width_value.span().start(),
        )?;
        let decoded_bytes = stride_bytes
            .checked_mul(u64::from(height))
            .ok_or_else(|| self.internal_error(Some(height_value.span().start())))?;
        check_scalar_limit(
            decoded_bytes,
            self.limits.max_decoded_bytes(),
            DocumentLimitKind::ImageXObjectDecodedBytes,
            reference,
            height_value.span().start(),
        )?;
        Ok(MetadataOutcome::Ready(ImageMetadata {
            width,
            height,
            color_space,
            stride_bytes,
            decoded_bytes,
            filter,
        }))
    }

    fn decode_parameters(
        &mut self,
        dictionary: &PdfDictionary,
        reference: ObjectRef,
        color_space: ImageXObjectColorSpace,
        width: u32,
        offset: u64,
    ) -> Result<DecodeParametersOutcome, DocumentError> {
        let mut predictor = None;
        let mut colors = None;
        let mut bits = None;
        let mut columns = None;
        for entry in dictionary.entries() {
            let entry_offset = entry.key().span().start();
            self.charge_metadata(reference, entry_offset)?;
            let slot = match entry.key().value().bytes() {
                b"Predictor" => &mut predictor,
                b"Colors" => &mut colors,
                b"BitsPerComponent" => &mut bits,
                b"Columns" => &mut columns,
                _ => return Err(invalid_image(reference, entry_offset)),
            };
            if slot.is_some() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicateStructuralKey,
                    Some(reference),
                    Some(entry_offset),
                ));
            }
            let SyntaxObject::Integer(value) = entry.value().value() else {
                if matches!(entry.value().value(), SyntaxObject::Reference(_)) {
                    return Ok(DecodeParametersOutcome::Unsupported(
                        ImageXObjectUnsupported::new(
                            ImageXObjectUnsupportedKind::IndirectMetadata,
                            reference,
                            entry.value().span().start(),
                        ),
                    ));
                }
                return Err(invalid_image(reference, entry.value().span().start()));
            };
            *slot = Some((*value, entry.value().span().start()));
        }
        if predictor.is_some_and(|(value, _)| value != 1)
            || colors.is_some_and(|(value, _)| value != i64::from(color_space.components()))
            || bits.is_some_and(|(value, _)| value != 8)
            || columns.is_some_and(|(value, _)| value != i64::from(width))
        {
            let mismatch_offset = predictor
                .filter(|(value, _)| *value != 1)
                .or_else(|| {
                    colors.filter(|(value, _)| *value != i64::from(color_space.components()))
                })
                .or_else(|| bits.filter(|(value, _)| *value != 8))
                .or_else(|| columns.filter(|(value, _)| *value != i64::from(width)))
                .map_or(offset, |(_, offset)| offset);
            return Ok(DecodeParametersOutcome::Unsupported(
                ImageXObjectUnsupported::new(
                    ImageXObjectUnsupportedKind::UnsupportedDecodeParameters,
                    reference,
                    mismatch_offset,
                ),
            ));
        }
        PredictorParameters::new(1, i64::from(color_space.components()), 8, i64::from(width))
            .map(DecodeParametersOutcome::Ready)
            .map_err(|_| self.internal_error(Some(offset)))
    }

    fn scan_metadata<'a>(
        &mut self,
        dictionary: &'a PdfDictionary,
        reference: ObjectRef,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<ImageSlots<'a>, DocumentError> {
        let mut slots = ImageSlots::default();
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            if self
                .stats
                .metadata_entries
                .is_multiple_of(METADATA_CANCELLATION_INTERVAL)
            {
                self.runtime_guard(source, cancellation, Some(offset))?;
            }
            let slot = match entry.key().value().bytes() {
                b"Type" => Some(&mut slots.type_value),
                b"Subtype" => Some(&mut slots.subtype),
                b"Width" => Some(&mut slots.width),
                b"Height" => Some(&mut slots.height),
                b"ColorSpace" => Some(&mut slots.color_space),
                b"BitsPerComponent" => Some(&mut slots.bits_per_component),
                b"ImageMask" => Some(&mut slots.image_mask),
                b"Mask" => Some(&mut slots.mask),
                b"SMask" => Some(&mut slots.soft_mask),
                b"Decode" => Some(&mut slots.decode),
                b"Interpolate" => Some(&mut slots.interpolate),
                b"Filter" => Some(&mut slots.filter),
                b"DecodeParms" => Some(&mut slots.decode_parameters),
                _ => None,
            };
            let Some(slot) = slot else {
                continue;
            };
            if slot.is_some() {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::DuplicateStructuralKey,
                    Some(reference),
                    Some(offset),
                ));
            }
            *slot = Some(entry.value());
        }
        self.runtime_guard(
            source,
            cancellation,
            Some(
                dictionary
                    .entries()
                    .last()
                    .map_or(self.proof.entry_value_offset(), |entry| {
                        entry.value().span().start()
                    }),
            ),
        )?;
        Ok(slots)
    }

    fn charge_metadata(&mut self, reference: ObjectRef, offset: u64) -> Result<(), DocumentError> {
        if self.stats.metadata_entries >= self.limits.max_metadata_entries() {
            return Err(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectMetadataEntries,
                self.limits.max_metadata_entries(),
                self.stats.metadata_entries,
                1,
                reference,
                Some(offset),
            ));
        }
        self.stats.metadata_entries = self
            .stats
            .metadata_entries
            .checked_add(1)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        Ok(())
    }

    fn check_payload_geometry(
        &self,
        metadata: ImageMetadata,
        encoded_bytes: u64,
        offset: u64,
    ) -> Result<(), DocumentError> {
        check_scalar_limit(
            encoded_bytes,
            self.limits.max_encoded_bytes(),
            DocumentLimitKind::ImageXObjectEncodedBytes,
            self.proof.target(),
            offset,
        )?;
        let base = self.limits.decode_limits();
        if encoded_bytes > base.max_input_bytes() {
            return Err(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectEncodedBytes,
                base.max_input_bytes(),
                0,
                encoded_bytes,
                self.proof.target(),
                Some(offset),
            ));
        }
        let decoded_limit = base
            .max_layer_output_bytes()
            .min(base.max_total_output_bytes())
            .min(base.max_final_output_bytes());
        if metadata.decoded_bytes > decoded_limit {
            return Err(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectDecodedBytes,
                decoded_limit,
                0,
                metadata.decoded_bytes,
                self.proof.target(),
                Some(offset),
            ));
        }
        Ok(())
    }

    fn decode_limits(
        &self,
        metadata: ImageMetadata,
        encoded_bytes: u64,
        object_heap: u64,
        plan_heap: u64,
        offset: u64,
    ) -> Result<ImageDecodeAdmission, DocumentError> {
        let base = self.limits.decode_limits();
        let retained_consumed = object_heap
            .checked_add(plan_heap)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        let image_retained_remaining = self
            .limits
            .max_retained_bytes()
            .checked_sub(retained_consumed)
            .ok_or_else(|| {
                DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectRetainedBytes,
                    self.limits.max_retained_bytes(),
                    object_heap,
                    plan_heap,
                    self.proof.target(),
                    Some(offset),
                )
            })?;
        let decoder_retained_limit =
            image_retained_remaining.min(base.max_retained_capacity_bytes());
        let effective_total_retained = retained_consumed
            .checked_add(decoder_retained_limit)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        if decoder_retained_limit < metadata.decoded_bytes {
            return Err(DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectRetainedBytes,
                effective_total_retained,
                retained_consumed,
                metadata.decoded_bytes,
                self.proof.target(),
                Some(offset),
            ));
        }
        let fuel = self.limits.max_decode_fuel().min(base.max_fuel());
        let limits = DecodeLimits::validate(DecodeLimitConfig {
            max_input_bytes: encoded_bytes,
            max_filters: 1,
            max_layer_output_bytes: metadata.decoded_bytes,
            max_total_output_bytes: metadata.decoded_bytes,
            max_final_output_bytes: metadata.decoded_bytes,
            max_retained_capacity_bytes: decoder_retained_limit,
            max_fuel: fuel,
            cancellation_check_interval_fuel: base.cancellation_check_interval_fuel().min(fuel),
        })
        .map_err(|_| self.internal_error(Some(offset)))?;
        Ok(ImageDecodeAdmission {
            limits,
            retained_prefix: retained_consumed,
        })
    }

    fn map_child_error(&self, error: DocumentError) -> DocumentError {
        let Some(lower) = error.object_error() else {
            return error;
        };
        if lower.code() == ObjectErrorCode::SyntaxFailure
            && let Some(syntax_limit) = lower.syntax_error().and_then(|error| error.limit())
            && syntax_limit.kind() == SyntaxLimitKind::RetainedBytes
            && self
                .child
                .as_ref()
                .and_then(|child| child.work_caps.max_retained_bytes())
                .is_some_and(|cap| {
                    let syntax = self.authority.as_attested().syntax_limits();
                    syntax
                        .max_owned_bytes()
                        .checked_add(syntax.max_container_bytes())
                        .is_some_and(|intrinsic| cap < intrinsic)
                })
        {
            return DocumentError::image_xobject_resource(
                DocumentLimitKind::ImageXObjectRetainedBytes,
                self.limits.max_retained_bytes(),
                syntax_limit.consumed(),
                syntax_limit.attempted(),
                self.proof.target(),
                error.offset().or_else(|| self.current_offset()),
            );
        }
        let Some(limit) = lower.limit() else {
            return error;
        };
        let reference = self.proof.target();
        let offset = error.offset().or_else(|| self.current_offset());
        match limit.kind() {
            ObjectLimitKind::TotalReadBytes
                if self.child.as_ref().is_some_and(|child| {
                    child.work_caps.max_read_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_read_bytes()
                }) =>
            {
                DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectObjectReadBytes,
                    self.limits.max_object_read_bytes(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    offset,
                )
            }
            ObjectLimitKind::TotalParseBytes
                if self.child.as_ref().is_some_and(|child| {
                    child.work_caps.max_parse_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_parse_bytes()
                }) =>
            {
                DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectObjectParseBytes,
                    self.limits.max_object_parse_bytes(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    offset,
                )
            }
            _ => error,
        }
    }

    fn map_decode_error(
        &self,
        error: DecodeError,
        reference: ObjectRef,
        offset: u64,
        expected_decoded_bytes: u64,
        admission: Option<ImageDecodeAdmission>,
    ) -> DocumentError {
        if let Some(limit) = error.limit() {
            return match limit.kind() {
                DecodeLimitKind::InputBytes => DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectEncodedBytes,
                    self.limits.max_encoded_bytes(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    Some(offset),
                ),
                DecodeLimitKind::LayerOutputBytes
                | DecodeLimitKind::TotalOutputBytes
                | DecodeLimitKind::FinalOutputBytes
                    if limit.limit() == expected_decoded_bytes =>
                {
                    invalid_image(reference, offset)
                }
                DecodeLimitKind::LayerOutputBytes
                | DecodeLimitKind::TotalOutputBytes
                | DecodeLimitKind::FinalOutputBytes => DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectDecodedBytes,
                    self.limits.max_decoded_bytes(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    Some(offset),
                ),
                DecodeLimitKind::Fuel => DocumentError::image_xobject_resource(
                    DocumentLimitKind::ImageXObjectDecodeFuel,
                    limit.limit(),
                    limit.consumed(),
                    limit.attempted(),
                    reference,
                    Some(offset),
                ),
                DecodeLimitKind::RetainedCapacityBytes
                | DecodeLimitKind::FilterPlanBytes
                | DecodeLimitKind::Allocation => {
                    let retained_prefix =
                        admission.map_or(0, |admission| admission.retained_prefix);
                    let effective_limit = retained_prefix.saturating_add(limit.limit());
                    let effective_consumed = retained_prefix.saturating_add(limit.consumed());
                    DocumentError::image_xobject_resource(
                        DocumentLimitKind::ImageXObjectRetainedBytes,
                        effective_limit,
                        effective_consumed,
                        limit.attempted(),
                        reference,
                        Some(offset),
                    )
                }
                DecodeLimitKind::FilterCount => self.internal_error(Some(offset)),
            };
        }
        match error.code() {
            DecodeErrorCode::SourceChanged => DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(offset),
            ),
            DecodeErrorCode::Cancelled => {
                DocumentError::for_code(DocumentErrorCode::Cancelled, Some(reference), Some(offset))
            }
            DecodeErrorCode::InvalidLimits
            | DecodeErrorCode::InvalidRequest
            | DecodeErrorCode::InternalState => self.internal_error(Some(offset)),
            _ => DocumentError::for_code(
                DocumentErrorCode::ImageXObjectDecodeFailure,
                Some(reference),
                Some(offset),
            ),
        }
    }

    fn runtime_guard(
        &self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
            ));
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
            ));
        }
        if cancelled {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
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
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        if fallback.code() == DocumentErrorCode::Cancelled {
            return fallback;
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        if cancelled {
            return DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.proof.target()),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        fallback
    }

    fn current_offset(&self) -> Option<u64> {
        self.authority
            .as_attested()
            .attestation(self.proof.target())
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn internal_error(&self, offset: Option<u64>) -> DocumentError {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(self.proof.target()),
            offset.or_else(|| self.current_offset()),
        )
    }

    fn ready(&mut self, image: Arc<AcquiredImageXObject>) -> ImageXObjectPoll {
        self.state = ImageJobState::Ready(Arc::clone(&image));
        ImageXObjectPoll::Ready(image)
    }

    fn unsupported(&mut self, unsupported: ImageXObjectUnsupported) -> ImageXObjectPoll {
        self.state = ImageJobState::Unsupported(unsupported);
        ImageXObjectPoll::Unsupported(unsupported)
    }

    fn fail(&mut self, error: DocumentError) -> ImageXObjectPoll {
        self.state = ImageJobState::Failed(error);
        ImageXObjectPoll::Failed(error)
    }
}

impl fmt::Debug for AcquireImageXObjectJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquireImageXObjectJob")
            .field("snapshot", &self.snapshot)
            .field("proof", &self.proof)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("phase", &self.phase())
            .field("stats", &self.stats)
            .field("active", &self.active.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl SharedAttestedRevisionIndex {
    /// Acquires one Page-selected Image XObject in a job owning this shared strict proof.
    pub fn acquire_image_xobject(
        &self,
        proof: PageXObjectReference,
        context: ImageXObjectJobContext,
        limits: ImageXObjectLimits,
    ) -> Result<AcquireImageXObjectJob, DocumentError> {
        let authority = self.as_attested();
        let target = proof.target();
        let attestation = authority.attestation(target)?;
        let offset = attestation.xref_offset();
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
        let envelope = context.object_envelope_checkpoint();
        let boundary = context.object_boundary_checkpoint();
        let payload = context.payload_checkpoint();
        if envelope == boundary || envelope == payload || boundary == payload {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidImageXObjectJobContext,
                Some(target),
                Some(offset),
            ));
        }
        let syntax = authority.syntax_limits();
        let intrinsic_retained = syntax
            .max_owned_bytes()
            .checked_add(syntax.max_container_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(target),
                    Some(offset),
                )
            })?;
        let work_caps = ObjectWorkCaps::new_with_retained_bytes(
            limits
                .max_object_read_bytes()
                .min(authority.object_limits().max_total_read_bytes()),
            limits
                .max_object_parse_bytes()
                .min(authority.object_limits().max_total_parse_bytes()),
            limits.max_retained_bytes().min(intrinsic_retained),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, target, offset))?;
        let object_context = AttestedObjectJobContext::new(
            context.job(),
            context.object_envelope_checkpoint(),
            context.object_boundary_checkpoint(),
            context.priority(),
        );
        let job = authority.open_object(target, object_context, work_caps)?;
        Ok(AcquireImageXObjectJob {
            authority: self.clone(),
            snapshot: authority.snapshot(),
            proof,
            context,
            limits,
            child: Some(ChildState { job, work_caps }),
            active: None,
            stats: ImageXObjectStats::default(),
            state: ImageJobState::Active,
        })
    }
}

#[derive(Default)]
struct ImageSlots<'a> {
    type_value: Option<&'a Located<SyntaxObject>>,
    subtype: Option<&'a Located<SyntaxObject>>,
    width: Option<&'a Located<SyntaxObject>>,
    height: Option<&'a Located<SyntaxObject>>,
    color_space: Option<&'a Located<SyntaxObject>>,
    bits_per_component: Option<&'a Located<SyntaxObject>>,
    image_mask: Option<&'a Located<SyntaxObject>>,
    mask: Option<&'a Located<SyntaxObject>>,
    soft_mask: Option<&'a Located<SyntaxObject>>,
    decode: Option<&'a Located<SyntaxObject>>,
    interpolate: Option<&'a Located<SyntaxObject>>,
    filter: Option<&'a Located<SyntaxObject>>,
    decode_parameters: Option<&'a Located<SyntaxObject>>,
}

fn required_slot(
    slot: Option<&Located<SyntaxObject>>,
    reference: ObjectRef,
    offset: u64,
) -> Result<&Located<SyntaxObject>, DocumentError> {
    slot.ok_or_else(|| invalid_image(reference, offset))
}

fn unsupported(
    kind: ImageXObjectUnsupportedKind,
    reference: ObjectRef,
    offset: u64,
) -> MetadataOutcome {
    MetadataOutcome::Unsupported(ImageXObjectUnsupported::new(kind, reference, offset))
}

fn indirect_metadata(reference: ObjectRef, offset: u64) -> MetadataOutcome {
    unsupported(
        ImageXObjectUnsupportedKind::IndirectMetadata,
        reference,
        offset,
    )
}

fn invalid_image(reference: ObjectRef, offset: u64) -> DocumentError {
    DocumentError::for_code(
        DocumentErrorCode::InvalidImageXObject,
        Some(reference),
        Some(offset),
    )
}

fn check_scalar_limit(
    value: u64,
    limit: u64,
    kind: DocumentLimitKind,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if value > limit {
        return Err(DocumentError::image_xobject_resource(
            kind,
            limit,
            0,
            value,
            reference,
            Some(offset),
        ));
    }
    Ok(())
}

fn canonical_filter_plan(filter: RegisteredFilter) -> Result<FilterPlan, DecodeError> {
    match filter {
        RegisteredFilter::Identity => FilterPlan::new(&[]),
        RegisteredFilter::Flate { parameters: None } => {
            FilterPlan::new(&[StreamFilter::FlateDecode])
        }
        RegisteredFilter::Flate {
            parameters: Some(parameters),
        } => FilterPlan::from_stages(&[FilterStage::new(
            StreamFilter::FlateDecode,
            FilterDecodeParameters::Predictor(parameters),
        )?]),
    }
}

fn decode_context(metadata: ImageMetadata) -> u64 {
    (DECODE_CONTEXT_VERSION << 56)
        | (metadata.color_space.context_code() << 48)
        | (8 << 40)
        | (metadata.filter.context_code() << 32)
        | (1 << 24)
}

fn numeric_is_exact(value: &SyntaxObject, expected_one: bool) -> bool {
    match value {
        SyntaxObject::Integer(value) => *value == i64::from(expected_one),
        SyntaxObject::Real(value) => real_is_exact(value, expected_one),
        _ => false,
    }
}

fn real_is_exact(value: &PdfReal, expected_one: bool) -> bool {
    let raw = value.raw();
    let (negative, unsigned) = match raw.first() {
        Some(b'-') => (true, &raw[1..]),
        Some(b'+') => (false, &raw[1..]),
        _ => (false, raw),
    };
    let exponent_start = unsigned
        .iter()
        .position(|byte| matches!(byte, b'e' | b'E'))
        .unwrap_or(unsigned.len());
    let mantissa = &unsigned[..exponent_start];
    let exponent = if exponent_start == unsigned.len() {
        0
    } else {
        match parse_signed_decimal(&unsigned[exponent_start + 1..]) {
            Some(value) => value,
            None => return false,
        }
    };
    let fractional_digits = mantissa
        .iter()
        .position(|byte| *byte == b'.')
        .map_or(0, |dot| mantissa.len().saturating_sub(dot + 1));
    let mut significant_digits = 0_usize;
    for byte in mantissa.iter().copied().filter(|byte| *byte != b'.') {
        if significant_digits == 0 && byte == b'0' {
            continue;
        }
        if significant_digits == 0 {
            if byte != b'1' {
                return false;
            }
        } else if byte != b'0' {
            return false;
        }
        significant_digits = match significant_digits.checked_add(1) {
            Some(value) => value,
            None => return false,
        };
    }
    if significant_digits == 0 {
        return !expected_one;
    }
    if !expected_one || negative {
        return false;
    }
    let trailing_zeros = i64::try_from(significant_digits.saturating_sub(1)).ok();
    let fractional_digits = i64::try_from(fractional_digits).ok();
    match (trailing_zeros, fractional_digits) {
        (Some(trailing_zeros), Some(fractional_digits)) => {
            trailing_zeros
                .checked_add(exponent)
                .and_then(|value| value.checked_sub(fractional_digits))
                == Some(0)
        }
        _ => false,
    }
}

fn parse_signed_decimal(bytes: &[u8]) -> Option<i64> {
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value = 0_i64;
    for byte in digits {
        let digit = byte.checked_sub(b'0')?;
        if digit > 9 {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(i64::from(digit))?;
    }
    if negative {
        value.checked_neg()
    } else {
        Some(value)
    }
}
