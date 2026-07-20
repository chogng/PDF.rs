use std::fmt;
use std::mem;

use pdf_rs_bytes::{
    ByteRange, ByteSlice, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceSnapshot,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeLimitConfig, DecodeLimitKind,
    DecodeLimits, DecodeProfile, DecodeRequest, FilterPlan, decode_stream,
};
use pdf_rs_object::{
    DecodedLocatedObject, FilteredObjectStream, IndirectObjectValue, ObjectCancellation,
    ObjectStream, ObjectStreamError, ObjectStreamErrorCategory, ObjectStreamLimitConfig,
    ObjectStreamLimitKind, ObjectStreamLimits, ObjectStreamStats, parse_filtered_object_stream,
    parse_unfiltered_object_stream,
};
use pdf_rs_syntax::{Located, ObjectRef, SyntaxObject};

use crate::{
    DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind, DocumentLimits,
    EffectiveObjectLocator, ResolveObjectJob, ResolvedObject, RevisionObjectIndex,
    RevisionResolverJobContext, RevisionResolverLimits, RevisionResolverPoll,
    RevisionResolverStats, RevisionResolverWorkCaps, SourceAcquiredRevisionChain,
    UncompressedObjectLocator,
};

const HARD_MAX_OWNER_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_OBJECT_WORK_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_OBJECT_RETAINED_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated bounds for a source-acquired revision owner and each object it resolves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceAcquiredDocumentLimitConfig {
    /// Bounded revision-object index construction profile.
    pub document: DocumentLimits,
    /// Per-object top-level framing and indirect-Length profile.
    pub resolver: RevisionResolverLimits,
    /// Strict foundational-filter decode profile for filtered object streams.
    pub decode: DecodeLimits,
    /// Decoded object-stream semantic profile.
    pub object_stream: ObjectStreamLimits,
    /// Maximum conservative retained bytes across source proof, chain clone, and anchors.
    pub max_owner_retained_bytes: u64,
    /// Maximum exact source bytes charged by one direct object or object-stream resolution.
    pub max_object_read_bytes: u64,
    /// Maximum framing, decode-output, and direct-syntax bytes charged by one resolution.
    pub max_object_parse_bytes: u64,
    /// Maximum conservative simultaneous retained-capacity bound for one resolution.
    pub max_object_retained_bytes: u64,
}

impl Default for SourceAcquiredDocumentLimitConfig {
    fn default() -> Self {
        Self {
            document: DocumentLimits::default(),
            resolver: RevisionResolverLimits::default(),
            decode: DecodeLimits::default(),
            object_stream: ObjectStreamLimits::default(),
            max_owner_retained_bytes: 512 * 1024 * 1024,
            max_object_read_bytes: 64 * 1024 * 1024,
            max_object_parse_bytes: 256 * 1024 * 1024,
            max_object_retained_bytes: 384 * 1024 * 1024,
        }
    }
}

/// Validated limits retained by one move-only source-acquired document owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceAcquiredDocumentLimits {
    document: DocumentLimits,
    resolver: RevisionResolverLimits,
    decode: DecodeLimits,
    object_stream: ObjectStreamLimits,
    max_owner_retained_bytes: u64,
    max_object_read_bytes: u64,
    max_object_parse_bytes: u64,
    max_object_retained_bytes: u64,
}

impl SourceAcquiredDocumentLimits {
    /// Validates aggregate owner, work, and proof-retention ceilings.
    pub fn validate(config: SourceAcquiredDocumentLimitConfig) -> Result<Self, DocumentError> {
        if config.max_owner_retained_bytes == 0
            || config.max_owner_retained_bytes > HARD_MAX_OWNER_RETAINED_BYTES
            || config.max_object_read_bytes == 0
            || config.max_object_read_bytes > HARD_MAX_OBJECT_WORK_BYTES
            || config.max_object_parse_bytes == 0
            || config.max_object_parse_bytes > HARD_MAX_OBJECT_WORK_BYTES
            || config.max_object_retained_bytes == 0
            || config.max_object_retained_bytes > HARD_MAX_OBJECT_RETAINED_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            document: config.document,
            resolver: config.resolver,
            decode: config.decode,
            object_stream: config.object_stream,
            max_owner_retained_bytes: config.max_owner_retained_bytes,
            max_object_read_bytes: config.max_object_read_bytes,
            max_object_parse_bytes: config.max_object_parse_bytes,
            max_object_retained_bytes: config.max_object_retained_bytes,
        })
    }

    /// Returns the revision-index construction profile.
    pub const fn document(self) -> DocumentLimits {
        self.document
    }

    /// Returns the top-level object framing profile.
    pub const fn resolver(self) -> RevisionResolverLimits {
        self.resolver
    }

    /// Returns the strict stream-decode profile.
    pub const fn decode(self) -> DecodeLimits {
        self.decode
    }

    /// Returns the decoded object-stream profile.
    pub const fn object_stream(self) -> ObjectStreamLimits {
        self.object_stream
    }

    /// Returns the complete owner retained-byte ceiling.
    pub const fn max_owner_retained_bytes(self) -> u64 {
        self.max_owner_retained_bytes
    }

    /// Returns the per-resolution exact-read ceiling.
    pub const fn max_object_read_bytes(self) -> u64 {
        self.max_object_read_bytes
    }

    /// Returns the per-resolution parser-work ceiling.
    pub const fn max_object_parse_bytes(self) -> u64 {
        self.max_object_parse_bytes
    }

    /// Returns the per-resolution conservative retained-capacity ceiling.
    pub const fn max_object_retained_bytes(self) -> u64 {
        self.max_object_retained_bytes
    }
}

impl Default for SourceAcquiredDocumentLimits {
    fn default() -> Self {
        Self::validate(SourceAcquiredDocumentLimitConfig::default())
            .expect("built-in acquired-document limits satisfy hard ceilings")
    }
}

/// Owner construction and retained-proof accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceAcquiredDocumentStats {
    source_proof_retained_bound_bytes: u64,
    cloned_chain_retained_bytes: u64,
    resolver_anchor_retained_bound_bytes: u64,
    resolver_anchor_retained_bytes: u64,
    owner_retained_bound_bytes: u64,
}

impl SourceAcquiredDocumentStats {
    /// Returns the conservative bound retained by the original source acquisition proof.
    pub const fn source_proof_retained_bound_bytes(self) -> u64 {
        self.source_proof_retained_bound_bytes
    }

    /// Returns allocator-reported revision storage retained by the resolver's private clone.
    pub const fn cloned_chain_retained_bytes(self) -> u64 {
        self.cloned_chain_retained_bytes
    }

    /// Returns the input-derived physical-anchor capacity admitted before index construction.
    pub const fn resolver_anchor_retained_bound_bytes(self) -> u64 {
        self.resolver_anchor_retained_bound_bytes
    }

    /// Returns allocator-reported physical-anchor storage retained by the resolver index.
    pub const fn resolver_anchor_retained_bytes(self) -> u64 {
        self.resolver_anchor_retained_bytes
    }

    /// Returns the checked aggregate retained owner bound.
    pub const fn owner_retained_bound_bytes(self) -> u64 {
        self.owner_retained_bound_bytes
    }
}

/// Move-only source acquisition proof paired with private latest-wins object geometry.
///
/// Construction retains the complete raw revision proofs and original composed chain. A private
/// chain clone feeds [`RevisionObjectIndex`]; neither representation can be extracted by value.
pub struct SourceAcquiredDocument {
    acquisition: SourceAcquiredRevisionChain,
    index: RevisionObjectIndex,
    limits: SourceAcquiredDocumentLimits,
    stats: SourceAcquiredDocumentStats,
}

/// Complete read-only classification of one latest-wins acquired object target.
///
/// This crate-private result is shared by object and document-service constructors so terminal
/// xref states and invalid object-stream container geometry are never reclassified as a budget
/// failure or a generic missing object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ClassifiedAcquiredObjectTarget {
    compressed: bool,
    framed_reference: ObjectRef,
    source_offset: u64,
}

impl ClassifiedAcquiredObjectTarget {
    const fn compressed(self) -> bool {
        self.compressed
    }

    const fn framed_reference(self) -> ObjectRef {
        self.framed_reference
    }

    pub(crate) const fn source_offset(self) -> u64 {
        self.source_offset
    }
}

impl SourceAcquiredDocument {
    /// Builds a proof-preserving resolver owner from one complete acquired chain.
    pub fn new(
        acquisition: SourceAcquiredRevisionChain,
        limits: SourceAcquiredDocumentLimits,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Self, DocumentError> {
        if cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                None,
                None,
            ));
        }
        let chain_stats = acquisition
            .stats()
            .chain()
            .ok_or_else(|| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        let cloned_chain_retained_bytes = chain_stats.retained_bytes();
        let source_proof_retained_bound_bytes = acquisition.stats().retained_bound_bytes();
        let accounted_anchor_bytes = u64::try_from(mem::size_of::<u64>())
            .map_err(|_| DocumentError::for_code(DocumentErrorCode::InternalState, None, None))?;
        let resolver_anchor_retained_bound_bytes = chain_stats
            .entries()
            .checked_add(u64::from(chain_stats.sections()))
            .and_then(|anchors| anchors.checked_mul(accounted_anchor_bytes))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AcquiredDocumentRetainedBytes,
                    limits.max_owner_retained_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        let owner_retained_bound_bytes = source_proof_retained_bound_bytes
            .checked_add(cloned_chain_retained_bytes)
            .and_then(|value| value.checked_add(resolver_anchor_retained_bound_bytes))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AcquiredDocumentRetainedBytes,
                    limits.max_owner_retained_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if owner_retained_bound_bytes > limits.max_owner_retained_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AcquiredDocumentRetainedBytes,
                limits.max_owner_retained_bytes,
                0,
                owner_retained_bound_bytes,
                None,
            ));
        }
        let snapshot = acquisition.snapshot();
        let root = acquisition.root();
        let index = RevisionObjectIndex::new(
            acquisition.revision_chain().clone(),
            limits.document,
            cancellation,
        )?;
        if index.snapshot() != snapshot || index.root() != root {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                None,
            ));
        }
        let resolver_anchor_retained_bytes = index.stats().retained_anchor_bytes();
        let actual_retained_bytes = source_proof_retained_bound_bytes
            .checked_add(cloned_chain_retained_bytes)
            .and_then(|value| value.checked_add(resolver_anchor_retained_bytes))
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(root), None)
            })?;
        if resolver_anchor_retained_bytes > resolver_anchor_retained_bound_bytes
            || actual_retained_bytes > owner_retained_bound_bytes
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(root),
                None,
            ));
        }
        if cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(root),
                None,
            ));
        }
        Ok(Self {
            acquisition,
            index,
            limits,
            stats: SourceAcquiredDocumentStats {
                source_proof_retained_bound_bytes,
                cloned_chain_retained_bytes,
                resolver_anchor_retained_bound_bytes,
                resolver_anchor_retained_bytes,
                owner_retained_bound_bytes,
            },
        })
    }

    /// Returns the immutable snapshot shared by the acquisition proof and resolver index.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.index.snapshot()
    }

    /// Returns the latest-wins inherited trailer root.
    pub const fn root(&self) -> ObjectRef {
        self.index.root()
    }

    /// Borrows the complete move-only source acquisition proof.
    pub const fn acquisition(&self) -> &SourceAcquiredRevisionChain {
        &self.acquisition
    }

    /// Returns the validated owner and per-object limits.
    pub const fn limits(&self) -> SourceAcquiredDocumentLimits {
        self.limits
    }

    /// Returns retained owner accounting.
    pub const fn stats(&self) -> SourceAcquiredDocumentStats {
        self.stats
    }

    /// Looks up the effective locator without lending the private revision chain.
    pub fn locator(&self, object_number: u32) -> Option<EffectiveObjectLocator> {
        self.index.locator(object_number)
    }

    /// Classifies one exact reference and its physical source anchor before any child budget.
    pub(crate) fn classify_object_target(
        &self,
        reference: ObjectRef,
    ) -> Result<ClassifiedAcquiredObjectTarget, DocumentError> {
        match self.index.locator(reference.number()) {
            None => Err(DocumentError::for_code(
                DocumentErrorCode::MissingObject,
                Some(reference),
                None,
            )),
            Some(EffectiveObjectLocator::Null { .. }) => Err(DocumentError::for_code(
                DocumentErrorCode::NullObject,
                Some(reference),
                None,
            )),
            Some(EffectiveObjectLocator::Free { .. }) => Err(DocumentError::for_code(
                DocumentErrorCode::FreeObject,
                Some(reference),
                None,
            )),
            Some(EffectiveObjectLocator::Compressed(locator)) => {
                if reference.generation() != 0 {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::GenerationMismatch,
                        Some(reference),
                        None,
                    ));
                }
                let container = ObjectRef::new(locator.object_stream(), 0).map_err(|_| {
                    DocumentError::for_code(
                        DocumentErrorCode::InvalidObjectStreamContainer,
                        Some(reference),
                        Some(locator.provenance().revision_startxref()),
                    )
                })?;
                let container_locator = match self.index.locator(container.number()) {
                    Some(EffectiveObjectLocator::Uncompressed(container_locator))
                        if container_locator.generation() == 0 =>
                    {
                        container_locator
                    }
                    _ => {
                        return Err(DocumentError::for_code(
                            DocumentErrorCode::InvalidObjectStreamContainer,
                            Some(reference),
                            Some(locator.provenance().revision_startxref()),
                        ));
                    }
                };
                if container_locator.offset() >= container_locator.provenance().revision_startxref()
                {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedXrefStreamContainer,
                        Some(container),
                        Some(container_locator.offset()),
                    ));
                }
                Ok(ClassifiedAcquiredObjectTarget {
                    compressed: true,
                    framed_reference: container,
                    source_offset: container_locator.offset(),
                })
            }
            Some(EffectiveObjectLocator::Uncompressed(locator)) => {
                if locator.generation() != reference.generation() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::GenerationMismatch,
                        Some(reference),
                        Some(locator.offset()),
                    ));
                }
                if locator.offset() >= locator.provenance().revision_startxref() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedXrefStreamContainer,
                        Some(reference),
                        Some(locator.offset()),
                    ));
                }
                Ok(ClassifiedAcquiredObjectTarget {
                    compressed: false,
                    framed_reference: reference,
                    source_offset: locator.offset(),
                })
            }
        }
    }

    /// Returns the effective top-level source anchor used to resolve this exact reference.
    ///
    /// Compressed values report their generation-zero object-stream container offset; decoded
    /// entry positions remain available only through [`AcquiredObjectCoordinate`].
    pub fn object_source_offset(&self, reference: ObjectRef) -> Option<u64> {
        self.classify_object_target(reference)
            .ok()
            .map(ClassifiedAcquiredObjectTarget::source_offset)
    }

    /// Starts one proof-preserving direct or compressed object resolution.
    pub fn open_object(
        &self,
        reference: ObjectRef,
        context: AcquiredObjectJobContext,
    ) -> Result<OpenAcquiredObjectJob<'_>, DocumentError> {
        OpenAcquiredObjectJob::new_with_work_caps(
            self,
            reference,
            context,
            AcquiredObjectWorkCaps::from_limits(self.limits),
        )
    }

    /// Starts one resolution under parent-supplied cumulative work caps.
    pub fn open_object_with_work_caps(
        &self,
        reference: ObjectRef,
        context: AcquiredObjectJobContext,
        work_caps: AcquiredObjectWorkCaps,
    ) -> Result<OpenAcquiredObjectJob<'_>, DocumentError> {
        OpenAcquiredObjectJob::new_with_work_caps(self, reference, context, work_caps)
    }
}

impl fmt::Debug for SourceAcquiredDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceAcquiredDocument")
            .field("snapshot", &self.snapshot())
            .field("root", &self.root())
            .field("proof_count", &self.acquisition.proofs().len())
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .finish()
    }
}

/// Runtime identity, five distinct lower checkpoints, and source priority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquiredObjectJobContext {
    job: JobId,
    object_envelope_checkpoint: ResumeCheckpoint,
    object_boundary_checkpoint: ResumeCheckpoint,
    length_envelope_checkpoint: ResumeCheckpoint,
    length_boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl AcquiredObjectJobContext {
    /// Creates a context whose five checkpoints must be pairwise distinct.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        job: JobId,
        object_envelope_checkpoint: ResumeCheckpoint,
        object_boundary_checkpoint: ResumeCheckpoint,
        length_envelope_checkpoint: ResumeCheckpoint,
        length_boundary_checkpoint: ResumeCheckpoint,
        payload_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            object_envelope_checkpoint,
            object_boundary_checkpoint,
            length_envelope_checkpoint,
            length_boundary_checkpoint,
            payload_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the target or container envelope checkpoint.
    pub const fn object_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.object_envelope_checkpoint
    }

    /// Returns the target or container boundary checkpoint.
    pub const fn object_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.object_boundary_checkpoint
    }

    /// Returns the indirect-Length envelope checkpoint.
    pub const fn length_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.length_envelope_checkpoint
    }

    /// Returns the indirect-Length boundary checkpoint.
    pub const fn length_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.length_boundary_checkpoint
    }

    /// Returns the exact object-stream payload checkpoint.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }

    /// Returns the source scheduling priority.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    const fn resolver(self) -> RevisionResolverJobContext {
        RevisionResolverJobContext::new(
            self.job,
            self.object_envelope_checkpoint,
            self.object_boundary_checkpoint,
            self.length_envelope_checkpoint,
            self.length_boundary_checkpoint,
            self.priority,
        )
    }

    fn is_valid(self) -> bool {
        let checkpoints = [
            self.object_envelope_checkpoint,
            self.object_boundary_checkpoint,
            self.length_envelope_checkpoint,
            self.length_boundary_checkpoint,
            self.payload_checkpoint,
        ];
        checkpoints
            .iter()
            .enumerate()
            .all(|(index, checkpoint)| !checkpoints[index + 1..].contains(checkpoint))
    }
}

/// Parent-supplied cumulative caps for one acquired direct or compressed object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquiredObjectWorkCaps {
    max_read_bytes: u64,
    max_parse_bytes: u64,
}

impl AcquiredObjectWorkCaps {
    /// Validates positive caps no larger than the owner profile.
    pub fn new(
        max_read_bytes: u64,
        max_parse_bytes: u64,
        limits: SourceAcquiredDocumentLimits,
    ) -> Result<Self, DocumentError> {
        if max_read_bytes == 0
            || max_parse_bytes == 0
            || max_read_bytes > limits.max_object_read_bytes()
            || max_parse_bytes > limits.max_object_parse_bytes()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_read_bytes,
            max_parse_bytes,
        })
    }

    /// Uses the complete per-object owner profile.
    pub const fn from_limits(limits: SourceAcquiredDocumentLimits) -> Self {
        Self {
            max_read_bytes: limits.max_object_read_bytes(),
            max_parse_bytes: limits.max_object_parse_bytes(),
        }
    }

    /// Returns the cumulative exact-read cap.
    pub const fn max_read_bytes(self) -> u64 {
        self.max_read_bytes
    }

    /// Returns the cumulative framing, decoding, and semantic-parse cap.
    pub const fn max_parse_bytes(self) -> u64 {
        self.max_parse_bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AcquiredObjectRetainedPlan {
    single_framed_syntax_bytes: u64,
    resolver_syntax_bytes: u64,
    filter_plan_bytes: u64,
    decode_capacity_bytes: u64,
    object_stream_entry_bytes: u64,
    object_stream_value_bytes: u64,
    admitted_bytes: u64,
}

impl AcquiredObjectRetainedPlan {
    fn new(
        limits: SourceAcquiredDocumentLimits,
        compressed: bool,
        reference: ObjectRef,
    ) -> Result<Self, DocumentError> {
        let syntax = limits.object_stream.syntax();
        let single_framed_syntax_bytes = syntax
            .max_owned_bytes()
            .checked_add(syntax.max_container_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
            })?;
        let resolver_syntax_bytes = single_framed_syntax_bytes.checked_mul(2).ok_or_else(|| {
            DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
        })?;
        let (
            filter_plan_bytes,
            decode_capacity_bytes,
            object_stream_working_bytes,
            object_stream_entry_bytes,
            object_stream_value_bytes,
        ) = if compressed {
            let filter_plan_bytes = FilterPlan::retained_heap_upper_bound(
                limits.decode.max_filters(),
            )
            .map_err(|_| {
                DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), None)
            })?;
            (
                filter_plan_bytes,
                limits.decode.max_retained_capacity_bytes(),
                limits.object_stream.max_working_bytes(),
                limits.object_stream.max_retained_entry_bytes(),
                limits.object_stream.max_retained_value_bytes(),
            )
        } else {
            (0, 0, 0, 0, 0)
        };
        let admitted_bytes = resolver_syntax_bytes
            .checked_add(filter_plan_bytes)
            .and_then(|value| value.checked_add(decode_capacity_bytes))
            .and_then(|value| value.checked_add(object_stream_working_bytes))
            .and_then(|value| value.checked_add(object_stream_entry_bytes))
            .and_then(|value| value.checked_add(object_stream_value_bytes))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AcquiredObjectRetainedBytes,
                    limits.max_object_retained_bytes,
                    0,
                    u64::MAX,
                    None,
                )
            })?;
        if admitted_bytes > limits.max_object_retained_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AcquiredObjectRetainedBytes,
                limits.max_object_retained_bytes,
                0,
                admitted_bytes,
                None,
            ));
        }
        Ok(Self {
            single_framed_syntax_bytes,
            resolver_syntax_bytes,
            filter_plan_bytes,
            decode_capacity_bytes,
            object_stream_entry_bytes,
            object_stream_value_bytes,
            admitted_bytes,
        })
    }
}

/// Coarse state of one acquired-chain object job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquiredObjectPhase {
    /// Framing the effective direct target or its effective object-stream container.
    Framing,
    /// Reading the exact encoded object-stream payload.
    Payload,
    /// Decoding and validating object-stream semantics.
    ObjectStream,
    /// The proof-bound value was returned.
    Complete,
    /// The one-shot job reached a stable failure.
    Failed,
}

/// Deterministic work and retained proof accounting for one acquired-chain object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AcquiredObjectStats {
    resolver: RevisionResolverStats,
    resolver_peak_retained_bytes: u64,
    payload_read_bytes: u64,
    decode_output_bytes: u64,
    decode_fuel: u64,
    object_stream: Option<ObjectStreamStats>,
    total_read_bytes: u64,
    total_parse_bytes: u64,
    admitted_retained_bound_bytes: u64,
    retained_proof_bytes: u64,
}

impl AcquiredObjectStats {
    /// Returns lower top-level object framing work.
    pub const fn resolver(self) -> RevisionResolverStats {
        self.resolver
    }

    /// Returns the greatest simultaneous target-plus-indirect-Length syntax heap observed.
    pub const fn resolver_peak_retained_bytes(self) -> u64 {
        self.resolver_peak_retained_bytes
    }

    /// Returns exact encoded object-stream bytes acquired from the source.
    pub const fn payload_read_bytes(self) -> u64 {
        self.payload_read_bytes
    }

    /// Returns cumulative bytes emitted by all strict decoder layers.
    pub const fn decode_output_bytes(self) -> u64 {
        self.decode_output_bytes
    }

    /// Returns deterministic strict-decoder fuel consumed.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }

    /// Returns decoded object-stream semantic accounting when applicable.
    pub const fn object_stream(self) -> Option<ObjectStreamStats> {
        self.object_stream
    }

    /// Returns framing reads plus exact payload bytes.
    pub const fn total_read_bytes(self) -> u64 {
        self.total_read_bytes
    }

    /// Returns framing parser windows plus decode output and embedded syntax windows.
    pub const fn total_parse_bytes(self) -> u64 {
        self.total_parse_bytes
    }

    /// Returns the branch-specific retained-capacity bound admitted before any child work.
    ///
    /// This is a conservative reservation only; construction does not allocate this amount.
    pub const fn admitted_retained_bound_bytes(self) -> u64 {
        self.admitted_retained_bound_bytes
    }

    /// Returns proof storage retained by the published value.
    pub const fn retained_proof_bytes(self) -> u64 {
        self.retained_proof_bytes
    }
}

/// Physical or decoded location of a resolved direct semantic value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquiredObjectCoordinate {
    /// The value occupies the top-level physical source object at this xref offset.
    Physical(u64),
    /// The value occupies decoded coordinates inside this object-stream container.
    Decoded {
        /// Effective generation-zero object-stream container.
        container: ObjectRef,
        /// Relative decoded entry start.
        start: u64,
        /// Relative decoded entry length.
        len: u64,
    },
}

/// Borrowed semantic value that keeps physical and decoded coordinate types distinct.
pub enum AcquiredObjectValue<'value> {
    /// A top-level direct value retaining its physical source location.
    Uncompressed(&'value Located<SyntaxObject>),
    /// An embedded direct value retaining decoded object-stream coordinates.
    Compressed(&'value DecodedLocatedObject),
}

impl fmt::Debug for AcquiredObjectValue<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uncompressed(_) => formatter.write_str("Uncompressed([REDACTED])"),
            Self::Compressed(_) => formatter.write_str("Compressed([REDACTED])"),
        }
    }
}

struct UnfilteredObjectStreamProof {
    container: ResolvedObject,
    payload: ByteSlice,
    stream: ObjectStream,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the move-only proof remains inline so no unbudgeted box allocation can separate ownership evidence"
)]
enum AcquiredObjectProof {
    Uncompressed(ResolvedObject),
    UnfilteredCompressed(UnfilteredObjectStreamProof),
    FilteredCompressed {
        container_locator: UncompressedObjectLocator,
        stream: FilteredObjectStream,
    },
}

/// One resolved direct value inseparable from the acquired chain and all lower proofs.
pub struct AcquiredObject<'owner> {
    owner: &'owner SourceAcquiredDocument,
    reference: ObjectRef,
    proof: AcquiredObjectProof,
    stats: AcquiredObjectStats,
}

impl AcquiredObject<'_> {
    /// Returns the exact effective object identity.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns the immutable source snapshot shared by every retained proof.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.owner.snapshot()
    }

    /// Returns deterministic work and proof-retention accounting.
    pub const fn stats(&self) -> AcquiredObjectStats {
        self.stats
    }

    /// Revalidates latest-wins provenance and borrows the direct semantic value.
    pub fn value(&self) -> Result<AcquiredObjectValue<'_>, DocumentError> {
        match &self.proof {
            AcquiredObjectProof::Uncompressed(resolved) => {
                let locator = resolved.locator();
                if self.owner.index.locator(self.reference.number())
                    != Some(EffectiveObjectLocator::Uncompressed(locator))
                    || self.reference.generation() != locator.generation()
                    || resolved.object().snapshot() != self.owner.snapshot()
                    || resolved.object().reference() != self.reference
                    || resolved.object().xref_offset() != locator.offset()
                    || resolved.object().object_upper_bound() != locator.object_upper_bound()
                {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::ObjectResolutionFailure,
                        Some(self.reference),
                        Some(locator.offset()),
                    ));
                }
                match resolved.object().value() {
                    IndirectObjectValue::Direct(value) => {
                        Ok(AcquiredObjectValue::Uncompressed(value))
                    }
                    IndirectObjectValue::Stream(_) => Err(DocumentError::for_code(
                        DocumentErrorCode::UnsupportedObjectFraming,
                        Some(self.reference),
                        Some(locator.offset()),
                    )),
                }
            }
            AcquiredObjectProof::UnfilteredCompressed(proof) => {
                if proof.payload.identity() != self.owner.snapshot().identity()
                    || proof.payload.range().start() != proof.stream.encoded_payload_span().start()
                    || proof.payload.range().len() != proof.stream.encoded_payload_span().len()
                    || proof.container.object().reference() != proof.stream.container()
                {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidObjectStreamContainer,
                        Some(self.reference),
                        Some(proof.stream.encoded_payload_span().start()),
                    ));
                }
                let resolved = self
                    .owner
                    .index
                    .resolve_compressed(self.reference, &proof.stream)?;
                Ok(AcquiredObjectValue::Compressed(resolved.entry().value()))
            }
            AcquiredObjectProof::FilteredCompressed {
                container_locator,
                stream,
            } => {
                if stream.framed_container().xref_offset() != container_locator.offset()
                    || stream.framed_container().object_upper_bound()
                        != container_locator.object_upper_bound()
                {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::InvalidObjectStreamContainer,
                        Some(self.reference),
                        Some(container_locator.offset()),
                    ));
                }
                let resolved = self
                    .owner
                    .index
                    .resolve_compressed(self.reference, stream.object_stream())?;
                Ok(AcquiredObjectValue::Compressed(resolved.entry().value()))
            }
        }
    }

    /// Returns the value location without conflating source and decoded coordinates.
    pub fn coordinate(&self) -> Result<AcquiredObjectCoordinate, DocumentError> {
        match self.value()? {
            AcquiredObjectValue::Uncompressed(_) => match &self.proof {
                AcquiredObjectProof::Uncompressed(resolved) => Ok(
                    AcquiredObjectCoordinate::Physical(resolved.locator().offset()),
                ),
                _ => Err(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.reference),
                    None,
                )),
            },
            AcquiredObjectValue::Compressed(value) => {
                let container = match &self.proof {
                    AcquiredObjectProof::UnfilteredCompressed(proof) => proof.stream.container(),
                    AcquiredObjectProof::FilteredCompressed { stream, .. } => {
                        stream.object_stream().container()
                    }
                    AcquiredObjectProof::Uncompressed(_) => {
                        return Err(DocumentError::for_code(
                            DocumentErrorCode::InternalState,
                            Some(self.reference),
                            None,
                        ));
                    }
                };
                let span = value.span();
                Ok(AcquiredObjectCoordinate::Decoded {
                    container,
                    start: span.start(),
                    len: span.len(),
                })
            }
        }
    }
}

impl fmt::Debug for AcquiredObject<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredObject")
            .field("snapshot", &self.snapshot())
            .field("reference", &self.reference)
            .field("stats", &self.stats)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one direct or compressed acquired-chain object.
#[allow(
    clippy::large_enum_variant,
    reason = "the one-shot proof result remains inline without an unbudgeted outer allocation"
)]
pub enum AcquiredObjectPoll<'owner> {
    /// The effective direct value and every authorizing proof are ready.
    Ready(AcquiredObject<'owner>),
    /// The active lower child requires exact source bytes.
    Pending {
        /// One-shot source data ticket.
        ticket: DataTicket,
        /// Canonical missing ranges.
        missing: SmallRanges,
        /// Exact lower checkpoint retained by the caller while waiting.
        checkpoint: ResumeCheckpoint,
    },
    /// The job reached a stable structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for AcquiredObjectPoll<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(value) => formatter.debug_tuple("Ready").field(value).finish(),
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

struct ObjectCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl ObjectCancellation for ObjectCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct DecodeCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl DecodeCancellation for DecodeCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "exactly one lower job remains inline and all of its owned heap is already covered by lower limits"
)]
enum ObjectJobState<'owner> {
    Framing {
        compressed_target: bool,
        job: ResolveObjectJob<'owner>,
    },
    Payload {
        container: ResolvedObject,
        range: ByteRange,
        plan: FilterPlan,
    },
    Parse {
        container: ResolvedObject,
        payload: ByteSlice,
        plan: FilterPlan,
    },
    Complete,
    Failed(DocumentError),
    Transition,
}

/// One-shot proof-preserving acquired-chain object resolver.
pub struct OpenAcquiredObjectJob<'owner> {
    owner: &'owner SourceAcquiredDocument,
    reference: ObjectRef,
    context: AcquiredObjectJobContext,
    work_caps: AcquiredObjectWorkCaps,
    retained_plan: AcquiredObjectRetainedPlan,
    stats: AcquiredObjectStats,
    state: ObjectJobState<'owner>,
}

impl<'owner> OpenAcquiredObjectJob<'owner> {
    fn new_with_work_caps(
        owner: &'owner SourceAcquiredDocument,
        reference: ObjectRef,
        context: AcquiredObjectJobContext,
        work_caps: AcquiredObjectWorkCaps,
    ) -> Result<Self, DocumentError> {
        if !context.is_valid() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidRevisionResolverJobContext,
                Some(reference),
                None,
            ));
        }
        if work_caps.max_read_bytes > owner.limits.max_object_read_bytes
            || work_caps.max_parse_bytes > owner.limits.max_object_parse_bytes
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                Some(reference),
                None,
            ));
        }
        let target = owner.classify_object_target(reference)?;
        let compressed_target = target.compressed();
        let framed_reference = target.framed_reference();
        let retained_plan =
            AcquiredObjectRetainedPlan::new(owner.limits, compressed_target, reference)?;
        let resolver_caps = RevisionResolverWorkCaps::new(
            work_caps
                .max_read_bytes
                .min(owner.limits.resolver.max_total_object_read_bytes()),
            work_caps
                .max_parse_bytes
                .min(owner.limits.resolver.max_total_object_parse_bytes()),
            owner.limits.resolver,
        )?;
        let job = ResolveObjectJob::new_with_work_caps(
            &owner.index,
            framed_reference,
            context.resolver(),
            owner.limits.resolver,
            owner.limits.object_stream.syntax(),
            resolver_caps,
        )?;
        let stats = AcquiredObjectStats {
            admitted_retained_bound_bytes: retained_plan.admitted_bytes,
            ..Default::default()
        };
        Ok(Self {
            owner,
            reference,
            context,
            work_caps,
            retained_plan,
            stats,
            state: ObjectJobState::Framing {
                compressed_target,
                job,
            },
        })
    }

    /// Returns the immutable owner snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.owner.snapshot()
    }

    /// Returns the exact requested reference.
    pub const fn reference(&self) -> ObjectRef {
        self.reference
    }

    /// Returns runtime identity and lower checkpoints.
    pub const fn context(&self) -> AcquiredObjectJobContext {
        self.context
    }

    /// Returns parent-supplied cumulative work caps.
    pub const fn work_caps(&self) -> AcquiredObjectWorkCaps {
        self.work_caps
    }

    /// Returns current deterministic work accounting.
    pub const fn stats(&self) -> AcquiredObjectStats {
        self.stats
    }

    /// Returns the public coarse phase.
    pub const fn phase(&self) -> AcquiredObjectPhase {
        match self.state {
            ObjectJobState::Framing { .. } => AcquiredObjectPhase::Framing,
            ObjectJobState::Payload { .. } => AcquiredObjectPhase::Payload,
            ObjectJobState::Parse { .. } => AcquiredObjectPhase::ObjectStream,
            ObjectJobState::Complete => AcquiredObjectPhase::Complete,
            ObjectJobState::Failed(_) | ObjectJobState::Transition => AcquiredObjectPhase::Failed,
        }
    }

    /// Advances the fixed resolver without performing host I/O itself.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> AcquiredObjectPoll<'owner> {
        if let ObjectJobState::Failed(error) = self.state {
            return AcquiredObjectPoll::Failed(error);
        }
        if matches!(self.state, ObjectJobState::Complete) {
            return AcquiredObjectPoll::Failed(DocumentError::for_code(
                DocumentErrorCode::JobAlreadyComplete,
                Some(self.reference),
                None,
            ));
        }
        if source.snapshot() != self.owner.snapshot() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.reference),
                None,
            ));
        }
        if cancellation.is_cancelled() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.reference),
                None,
            ));
        }

        loop {
            let state = mem::replace(&mut self.state, ObjectJobState::Transition);
            match state {
                ObjectJobState::Framing {
                    compressed_target,
                    mut job,
                } => {
                    let poll = job.poll(source, &ObjectCancellationAdapter(cancellation));
                    self.stats.resolver = job.stats();
                    if let Err(error) = self.update_resolver_retained() {
                        return self.fail(error);
                    }
                    if let Err(error) = self.update_totals() {
                        return self.fail(error);
                    }
                    match poll {
                        RevisionResolverPoll::Pending {
                            ticket,
                            missing,
                            checkpoint,
                        } => {
                            self.state = ObjectJobState::Framing {
                                compressed_target,
                                job,
                            };
                            return AcquiredObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint,
                            };
                        }
                        RevisionResolverPoll::Failed(error) => {
                            return self.fail(self.map_resolver_work_error(error));
                        }
                        RevisionResolverPoll::Ready(resolved) if !compressed_target => {
                            let retained = resolved.object().retained_heap_bytes();
                            self.stats.retained_proof_bytes = retained;
                            if let Err(error) =
                                self.validate_retained(retained, resolved.locator().offset())
                            {
                                return self.fail(error);
                            }
                            self.state = ObjectJobState::Complete;
                            return AcquiredObjectPoll::Ready(AcquiredObject {
                                owner: self.owner,
                                reference: self.reference,
                                proof: AcquiredObjectProof::Uncompressed(resolved),
                                stats: self.stats,
                            });
                        }
                        RevisionResolverPoll::Ready(container) => {
                            if let Err(error) = self.validate_retained_component(
                                container.object().retained_heap_bytes(),
                                self.retained_plan.single_framed_syntax_bytes,
                                container.locator().offset(),
                            ) {
                                return self.fail(error);
                            }
                            let IndirectObjectValue::Stream(stream) = container.object().value()
                            else {
                                return self.fail(DocumentError::for_code(
                                    DocumentErrorCode::InvalidObjectStreamContainer,
                                    Some(self.reference),
                                    Some(container.locator().offset()),
                                ));
                            };
                            let plan = match FilterPlan::from_pdf_dictionary(
                                stream.dictionary().value(),
                                self.owner.limits.decode,
                                &DecodeCancellationAdapter(cancellation),
                            ) {
                                Ok(plan) => plan,
                                Err(error) => {
                                    return self.fail(map_decode_error(
                                        error,
                                        self.reference,
                                        stream.dictionary().span().start(),
                                    ));
                                }
                            };
                            let plan_retained = match plan.retained_heap_bytes() {
                                Ok(retained) => retained,
                                Err(error) => {
                                    return self.fail(map_decode_error(
                                        error,
                                        self.reference,
                                        stream.dictionary().span().start(),
                                    ));
                                }
                            };
                            if let Err(error) = self.validate_retained_component(
                                plan_retained,
                                self.retained_plan.filter_plan_bytes,
                                stream.dictionary().span().start(),
                            ) {
                                return self.fail(error);
                            }
                            let span = stream.data_span();
                            if span.is_empty() {
                                return self.fail(DocumentError::for_code(
                                    DocumentErrorCode::CompressedObjectMismatch,
                                    Some(self.reference),
                                    Some(span.start()),
                                ));
                            }
                            let payload_limit = if plan.is_empty() {
                                self.owner.limits.object_stream.max_decoded_bytes()
                            } else {
                                self.owner.limits.decode.max_input_bytes()
                            };
                            let read_consumed = self.stats.resolver.total_read_bytes();
                            let read_remaining =
                                match self.work_caps.max_read_bytes.checked_sub(read_consumed) {
                                    Some(remaining) => remaining,
                                    None => {
                                        return self.fail(DocumentError::for_code(
                                            DocumentErrorCode::InternalState,
                                            Some(self.reference),
                                            Some(span.start()),
                                        ));
                                    }
                                };
                            if read_remaining < payload_limit && span.len() > read_remaining {
                                return self.fail(DocumentError::resource(
                                    DocumentLimitKind::AcquiredObjectReadBytes,
                                    self.work_caps.max_read_bytes,
                                    read_consumed,
                                    span.len(),
                                    Some(span.start()),
                                ));
                            }
                            if span.len() > payload_limit {
                                return self.fail(DocumentError::for_code(
                                    DocumentErrorCode::ResourceLimit,
                                    Some(self.reference),
                                    Some(span.start()),
                                ));
                            }
                            if span.len() > read_remaining {
                                return self.fail(DocumentError::resource(
                                    DocumentLimitKind::AcquiredObjectReadBytes,
                                    self.work_caps.max_read_bytes,
                                    read_consumed,
                                    span.len(),
                                    Some(span.start()),
                                ));
                            }
                            let range = match ByteRange::new(span.start(), span.len()) {
                                Ok(range) => range,
                                Err(_) => {
                                    return self.fail(DocumentError::for_code(
                                        DocumentErrorCode::ObjectResolutionFailure,
                                        Some(self.reference),
                                        Some(span.start()),
                                    ));
                                }
                            };
                            self.state = ObjectJobState::Payload {
                                container,
                                range,
                                plan,
                            };
                        }
                    }
                }
                ObjectJobState::Payload {
                    container,
                    range,
                    plan,
                } => {
                    let request = ReadRequest::new(
                        range,
                        self.context.priority,
                        self.context.job,
                        self.context.payload_checkpoint,
                    );
                    match source.poll(request) {
                        ReadPoll::Pending { ticket, missing } => {
                            self.state = ObjectJobState::Payload {
                                container,
                                range,
                                plan,
                            };
                            return AcquiredObjectPoll::Pending {
                                ticket,
                                missing,
                                checkpoint: self.context.payload_checkpoint,
                            };
                        }
                        ReadPoll::EndOfFile => {
                            return self.fail(DocumentError::for_code(
                                DocumentErrorCode::UnexpectedEndOfSource,
                                Some(self.reference),
                                Some(range.start()),
                            ));
                        }
                        ReadPoll::Failed(error) => {
                            return self.fail(DocumentError::from_source(error, range.start()));
                        }
                        ReadPoll::Ready(payload) => {
                            if payload.identity() != self.owner.snapshot().identity()
                                || payload.range() != range
                                || u64::try_from(payload.bytes().len()).ok() != Some(range.len())
                            {
                                return self.fail(DocumentError::for_code(
                                    DocumentErrorCode::SourceSnapshotMismatch,
                                    Some(self.reference),
                                    Some(range.start()),
                                ));
                            }
                            self.stats.payload_read_bytes = range.len();
                            if let Err(error) = self.update_totals() {
                                return self.fail(error);
                            }
                            self.state = ObjectJobState::Parse {
                                container,
                                payload,
                                plan,
                            };
                        }
                    }
                }
                ObjectJobState::Parse {
                    container,
                    payload,
                    plan,
                } => return self.parse_object_stream(container, payload, plan, cancellation),
                ObjectJobState::Complete => {
                    self.state = ObjectJobState::Complete;
                    return AcquiredObjectPoll::Failed(DocumentError::for_code(
                        DocumentErrorCode::JobAlreadyComplete,
                        Some(self.reference),
                        None,
                    ));
                }
                ObjectJobState::Failed(error) => {
                    self.state = ObjectJobState::Failed(error);
                    return AcquiredObjectPoll::Failed(error);
                }
                ObjectJobState::Transition => {
                    return self.fail(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.reference),
                        None,
                    ));
                }
            }
        }
    }

    fn parse_object_stream(
        &mut self,
        container: ResolvedObject,
        payload: ByteSlice,
        plan: FilterPlan,
        cancellation: &dyn DocumentCancellation,
    ) -> AcquiredObjectPoll<'owner> {
        if cancellation.is_cancelled() {
            return self.fail(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.reference),
                None,
            ));
        }
        let container_offset = container.locator().offset();
        let parse_remaining = match self
            .work_caps
            .max_parse_bytes
            .checked_sub(self.stats.resolver.total_parse_bytes())
        {
            Some(value) if value > 0 => value,
            _ => {
                return self.fail(DocumentError::resource(
                    DocumentLimitKind::AcquiredObjectParseBytes,
                    self.work_caps.max_parse_bytes,
                    self.stats.resolver.total_parse_bytes(),
                    1,
                    Some(container_offset),
                ));
            }
        };
        let proof = if plan.is_empty() {
            let object_stream_limits = match capped_object_stream_limits(
                self.owner.limits.object_stream,
                parse_remaining,
                self.retained_plan.object_stream_entry_bytes,
                self.retained_plan.object_stream_value_bytes,
            ) {
                Ok(limits) => limits,
                Err(error) => return self.fail(error),
            };
            let stream = match parse_unfiltered_object_stream(
                container.object(),
                &payload,
                object_stream_limits.limits,
                &ObjectCancellationAdapter(cancellation),
            ) {
                Ok(stream) => stream,
                Err(error) => {
                    return self.fail(self.map_object_stream_work_error(
                        error,
                        container_offset,
                        object_stream_limits.parent_syntax_tightened,
                    ));
                }
            };
            if let Err(error) = self.owner.index.resolve_compressed(self.reference, &stream) {
                return self.fail(error);
            }
            let semantic = stream.stats();
            if let Err(error) = self.validate_object_stream_retained(semantic, container_offset) {
                return self.fail(error);
            }
            let retained = match container
                .object()
                .retained_heap_bytes()
                .checked_add(semantic.retained_entry_bytes())
                .and_then(|value| value.checked_add(semantic.retained_value_bytes()))
            {
                Some(value) => value,
                None => {
                    return self.fail(DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.reference),
                        Some(container_offset),
                    ));
                }
            };
            self.stats.object_stream = Some(semantic);
            self.stats.retained_proof_bytes = retained;
            AcquiredObjectProof::UnfilteredCompressed(UnfilteredObjectStreamProof {
                container,
                payload,
                stream,
            })
        } else {
            if parse_remaining < 2 {
                return self.fail(DocumentError::resource(
                    DocumentLimitKind::AcquiredObjectParseBytes,
                    self.work_caps.max_parse_bytes,
                    self.stats.resolver.total_parse_bytes(),
                    2,
                    Some(container_offset),
                ));
            }
            let decode_cap = parse_remaining - 1;
            let decode_limits = match capped_decode_limits(
                self.owner.limits.decode,
                decode_cap,
                self.retained_plan.decode_capacity_bytes,
            ) {
                Ok(limits) => limits,
                Err(error) => return self.fail(error),
            };
            let locator = container.locator();
            let (container_locator, framed) = container.into_parts();
            if container_locator != locator {
                return self.fail(DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.reference),
                    Some(container_offset),
                ));
            }
            let (dictionary_span, data_span) = match framed.value() {
                IndirectObjectValue::Stream(stream) => {
                    (stream.dictionary().span(), stream.data_span())
                }
                IndirectObjectValue::Direct(_) => {
                    return self.fail(DocumentError::for_code(
                        DocumentErrorCode::InvalidObjectStreamContainer,
                        Some(self.reference),
                        Some(container_offset),
                    ));
                }
            };
            let request = match DecodeRequest::new(
                self.owner.snapshot(),
                framed.reference(),
                dictionary_span,
                data_span,
                payload,
                plan,
                DecodeProfile::M1StrictV1,
                decode_limits.limits,
            ) {
                Ok(request) => request,
                Err(error) => {
                    return self.fail(map_decode_error(error, self.reference, data_span.start()));
                }
            };
            let decoded = match decode_stream(request, &DecodeCancellationAdapter(cancellation)) {
                Ok(decoded) => decoded,
                Err(error) => {
                    return self.fail(self.map_decode_runtime_error(
                        error,
                        data_span.start(),
                        decode_limits,
                    ));
                }
            };
            self.stats.decode_output_bytes = decoded.attestation().cumulative_output_bytes();
            self.stats.decode_fuel = decoded.attestation().fuel_consumed();
            if let Err(error) = self.validate_retained_component(
                decoded.attestation().peak_retained_capacity_bytes(),
                self.retained_plan.decode_capacity_bytes,
                data_span.start(),
            ) {
                return self.fail(error);
            }
            let semantic_cap = match parse_remaining.checked_sub(self.stats.decode_output_bytes) {
                Some(value) if value > 0 => value,
                _ => {
                    return self.fail(DocumentError::resource(
                        DocumentLimitKind::AcquiredObjectParseBytes,
                        self.work_caps.max_parse_bytes,
                        self.stats
                            .resolver
                            .total_parse_bytes()
                            .saturating_add(self.stats.decode_output_bytes),
                        1,
                        Some(container_offset),
                    ));
                }
            };
            let object_stream_limits = match capped_object_stream_limits(
                self.owner.limits.object_stream,
                semantic_cap,
                self.retained_plan.object_stream_entry_bytes,
                self.retained_plan.object_stream_value_bytes,
            ) {
                Ok(limits) => limits,
                Err(error) => return self.fail(error),
            };
            let filtered = match parse_filtered_object_stream(
                framed,
                decoded,
                object_stream_limits.limits,
                &ObjectCancellationAdapter(cancellation),
            ) {
                Ok(stream) => stream,
                Err(error) => {
                    return self.fail(self.map_object_stream_work_error(
                        error,
                        container_offset,
                        object_stream_limits.parent_syntax_tightened,
                    ));
                }
            };
            if let Err(error) = self
                .owner
                .index
                .resolve_compressed(self.reference, filtered.object_stream())
            {
                return self.fail(error);
            }
            let semantic = filtered.object_stream().stats();
            if let Err(error) = self.validate_object_stream_retained(semantic, container_offset) {
                return self.fail(error);
            }
            self.stats.object_stream = Some(semantic);
            self.stats.retained_proof_bytes = filtered.retained_proof_bytes();
            AcquiredObjectProof::FilteredCompressed {
                container_locator,
                stream: filtered,
            }
        };
        if let Err(error) = self.update_totals() {
            return self.fail(error);
        }
        if let Err(error) =
            self.validate_retained(self.stats.retained_proof_bytes, container_offset)
        {
            return self.fail(error);
        }
        self.state = ObjectJobState::Complete;
        AcquiredObjectPoll::Ready(AcquiredObject {
            owner: self.owner,
            reference: self.reference,
            proof,
            stats: self.stats,
        })
    }

    fn update_totals(&mut self) -> Result<(), DocumentError> {
        let total_read_bytes = self
            .stats
            .resolver
            .total_read_bytes()
            .checked_add(self.stats.payload_read_bytes)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AcquiredObjectReadBytes,
                    self.work_caps.max_read_bytes,
                    self.stats.resolver.total_read_bytes(),
                    u64::MAX,
                    None,
                )
            })?;
        if total_read_bytes > self.work_caps.max_read_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AcquiredObjectReadBytes,
                self.work_caps.max_read_bytes,
                self.stats.resolver.total_read_bytes(),
                self.stats.payload_read_bytes,
                None,
            ));
        }
        let semantic_parse = self
            .stats
            .object_stream
            .map_or(0, ObjectStreamStats::syntax_input_bytes);
        let total_parse_bytes = self
            .stats
            .resolver
            .total_parse_bytes()
            .checked_add(self.stats.decode_output_bytes)
            .and_then(|value| value.checked_add(semantic_parse))
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::AcquiredObjectParseBytes,
                    self.work_caps.max_parse_bytes,
                    self.stats.resolver.total_parse_bytes(),
                    u64::MAX,
                    None,
                )
            })?;
        if total_parse_bytes > self.work_caps.max_parse_bytes {
            return Err(DocumentError::resource(
                DocumentLimitKind::AcquiredObjectParseBytes,
                self.work_caps.max_parse_bytes,
                self.stats.resolver.total_parse_bytes(),
                self.stats
                    .decode_output_bytes
                    .saturating_add(semantic_parse),
                None,
            ));
        }
        self.stats.total_read_bytes = total_read_bytes;
        self.stats.total_parse_bytes = total_parse_bytes;
        Ok(())
    }

    fn update_resolver_retained(&mut self) -> Result<(), DocumentError> {
        let retained = self
            .stats
            .resolver
            .object()
            .retained_heap_bytes()
            .checked_add(
                self.stats
                    .resolver
                    .length_dependency()
                    .retained_heap_bytes(),
            )
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.reference),
                    self.owner.object_source_offset(self.reference),
                )
            })?;
        self.stats.resolver_peak_retained_bytes =
            self.stats.resolver_peak_retained_bytes.max(retained);
        self.validate_retained_component(
            retained,
            self.retained_plan.resolver_syntax_bytes,
            self.owner.object_source_offset(self.reference).unwrap_or(0),
        )
    }

    fn validate_retained(&self, retained: u64, offset: u64) -> Result<(), DocumentError> {
        if retained > self.retained_plan.admitted_bytes {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.reference),
                Some(offset),
            ));
        }
        Ok(())
    }

    fn validate_retained_component(
        &self,
        retained: u64,
        admitted: u64,
        offset: u64,
    ) -> Result<(), DocumentError> {
        if retained > admitted {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InternalState,
                Some(self.reference),
                Some(offset),
            ));
        }
        Ok(())
    }

    fn validate_object_stream_retained(
        &self,
        stats: ObjectStreamStats,
        offset: u64,
    ) -> Result<(), DocumentError> {
        self.validate_retained_component(
            stats.retained_entry_bytes(),
            self.retained_plan.object_stream_entry_bytes,
            offset,
        )?;
        self.validate_retained_component(
            stats.retained_value_bytes(),
            self.retained_plan.object_stream_value_bytes,
            offset,
        )
    }

    fn map_decode_runtime_error(
        &self,
        error: DecodeError,
        offset: u64,
        capped: CappedDecodeLimits,
    ) -> DocumentError {
        if error.category() == DecodeErrorCategory::Resource
            && let Some(limit) = error.limit()
            && capped.is_parent_output_limit(limit.kind(), limit.limit())
        {
            let decode_delta = match limit.attempted().checked_sub(limit.consumed()) {
                Some(value) => value,
                None => {
                    return DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.reference),
                        Some(offset),
                    );
                }
            };
            let consumed = match self
                .stats
                .resolver
                .total_parse_bytes()
                .checked_add(limit.consumed())
            {
                Some(value) => value,
                None => {
                    return DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.reference),
                        Some(offset),
                    );
                }
            };
            let attempted = match decode_delta.checked_add(1) {
                Some(value) => value,
                None => {
                    return DocumentError::for_code(
                        DocumentErrorCode::InternalState,
                        Some(self.reference),
                        Some(offset),
                    );
                }
            };
            return DocumentError::resource(
                DocumentLimitKind::AcquiredObjectParseBytes,
                self.work_caps.max_parse_bytes,
                consumed,
                attempted,
                Some(offset),
            );
        }
        map_decode_error(error, self.reference, offset)
    }

    fn map_object_stream_work_error(
        &self,
        error: ObjectStreamError,
        offset: u64,
        parent_syntax_tightened: bool,
    ) -> DocumentError {
        if error.category() == ObjectStreamErrorCategory::Resource
            && let Some(limit) = error.limit()
            && parent_syntax_tightened
            && limit.kind() == ObjectStreamLimitKind::TotalSyntaxBytes
        {
            let Some(consumed) = self
                .stats
                .resolver
                .total_parse_bytes()
                .checked_add(self.stats.decode_output_bytes)
                .and_then(|value| value.checked_add(limit.consumed()))
            else {
                return DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(self.reference),
                    Some(offset),
                );
            };
            return DocumentError::resource(
                DocumentLimitKind::AcquiredObjectParseBytes,
                self.work_caps.max_parse_bytes,
                consumed,
                limit.attempted(),
                error.source_offset().or(Some(offset)),
            );
        }
        map_object_stream_error(error, self.reference, offset)
    }

    fn map_resolver_work_error(&self, error: DocumentError) -> DocumentError {
        if error.code() == DocumentErrorCode::ResourceLimit
            && let Some(limit) = error.limit()
        {
            let kind = match limit.kind() {
                DocumentLimitKind::RevisionResolverObjectReadBytes
                    if self.work_caps.max_read_bytes
                        < self.owner.limits.resolver.max_total_object_read_bytes() =>
                {
                    Some(DocumentLimitKind::AcquiredObjectReadBytes)
                }
                DocumentLimitKind::RevisionResolverObjectParseBytes
                    if self.work_caps.max_parse_bytes
                        < self.owner.limits.resolver.max_total_object_parse_bytes() =>
                {
                    Some(DocumentLimitKind::AcquiredObjectParseBytes)
                }
                _ => None,
            };
            if let Some(kind) = kind {
                let ceiling = if kind == DocumentLimitKind::AcquiredObjectReadBytes {
                    self.work_caps.max_read_bytes
                } else {
                    self.work_caps.max_parse_bytes
                };
                return DocumentError::resource(
                    kind,
                    ceiling,
                    limit.consumed(),
                    limit.attempted(),
                    error.offset(),
                );
            }
        }
        error
    }

    fn fail(&mut self, error: DocumentError) -> AcquiredObjectPoll<'owner> {
        self.state = ObjectJobState::Failed(error);
        AcquiredObjectPoll::Failed(error)
    }
}

impl fmt::Debug for OpenAcquiredObjectJob<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAcquiredObjectJob")
            .field("snapshot", &self.snapshot())
            .field("reference", &self.reference)
            .field("context", &self.context)
            .field("work_caps", &self.work_caps)
            .field("stats", &self.stats)
            .field("phase", &self.phase())
            .finish()
    }
}

#[derive(Clone, Copy)]
struct CappedDecodeLimits {
    limits: DecodeLimits,
    parent_layer_tightened: bool,
    parent_total_tightened: bool,
    parent_final_tightened: bool,
}

impl CappedDecodeLimits {
    const fn is_parent_output_limit(self, kind: DecodeLimitKind, limit: u64) -> bool {
        match kind {
            DecodeLimitKind::LayerOutputBytes => {
                self.parent_layer_tightened && limit == self.limits.max_layer_output_bytes()
            }
            DecodeLimitKind::TotalOutputBytes => {
                self.parent_total_tightened && limit == self.limits.max_total_output_bytes()
            }
            DecodeLimitKind::FinalOutputBytes => {
                self.parent_final_tightened && limit == self.limits.max_final_output_bytes()
            }
            DecodeLimitKind::InputBytes
            | DecodeLimitKind::FilterCount
            | DecodeLimitKind::FilterPlanBytes
            | DecodeLimitKind::RetainedCapacityBytes
            | DecodeLimitKind::Fuel
            | DecodeLimitKind::Allocation => false,
        }
    }
}

fn capped_decode_limits(
    limits: DecodeLimits,
    max_total_output_bytes: u64,
    max_retained_capacity_bytes: u64,
) -> Result<CappedDecodeLimits, DocumentError> {
    let max_final_output_bytes = limits.max_final_output_bytes().min(max_total_output_bytes);
    let max_layer_output_bytes = limits
        .max_layer_output_bytes()
        .min(max_total_output_bytes)
        .max(max_final_output_bytes);
    let max_total_output_bytes = limits
        .max_total_output_bytes()
        .min(max_total_output_bytes)
        .max(max_final_output_bytes);
    let capped = DecodeLimits::validate(DecodeLimitConfig {
        max_input_bytes: limits.max_input_bytes(),
        max_filters: limits.max_filters(),
        max_layer_output_bytes,
        max_total_output_bytes,
        max_final_output_bytes,
        max_retained_capacity_bytes: limits
            .max_retained_capacity_bytes()
            .min(max_retained_capacity_bytes),
        max_fuel: limits.max_fuel(),
        cancellation_check_interval_fuel: limits.cancellation_check_interval_fuel(),
    })
    .map_err(|_| DocumentError::for_code(DocumentErrorCode::InvalidLimits, None, None))?;
    Ok(CappedDecodeLimits {
        limits: capped,
        parent_layer_tightened: capped.max_layer_output_bytes() < limits.max_layer_output_bytes(),
        parent_total_tightened: capped.max_total_output_bytes() < limits.max_total_output_bytes(),
        parent_final_tightened: capped.max_final_output_bytes() < limits.max_final_output_bytes(),
    })
}

#[derive(Clone, Copy)]
struct CappedObjectStreamLimits {
    limits: ObjectStreamLimits,
    parent_syntax_tightened: bool,
}

fn capped_object_stream_limits(
    limits: ObjectStreamLimits,
    max_total_syntax_bytes: u64,
    max_retained_entry_bytes: u64,
    max_retained_value_bytes: u64,
) -> Result<CappedObjectStreamLimits, DocumentError> {
    let capped = ObjectStreamLimits::validate(ObjectStreamLimitConfig {
        max_decoded_bytes: limits.max_decoded_bytes(),
        max_objects: limits.max_objects(),
        max_header_bytes: limits.max_header_bytes(),
        max_working_bytes: limits.max_working_bytes(),
        max_retained_entry_bytes: limits
            .max_retained_entry_bytes()
            .min(max_retained_entry_bytes),
        max_retained_value_bytes: limits
            .max_retained_value_bytes()
            .min(max_retained_value_bytes),
        max_total_syntax_bytes: limits.max_total_syntax_bytes().min(max_total_syntax_bytes),
        syntax: limits.syntax(),
    })
    .map_err(|_| DocumentError::for_code(DocumentErrorCode::InvalidLimits, None, None))?;
    Ok(CappedObjectStreamLimits {
        limits: capped,
        parent_syntax_tightened: capped.max_total_syntax_bytes() < limits.max_total_syntax_bytes(),
    })
}

fn map_decode_error(error: DecodeError, reference: ObjectRef, offset: u64) -> DocumentError {
    let code = match error.category() {
        DecodeErrorCategory::Configuration => DocumentErrorCode::InvalidLimits,
        DecodeErrorCategory::Syntax => DocumentErrorCode::ObjectResolutionFailure,
        DecodeErrorCategory::Unsupported => DocumentErrorCode::UnsupportedObjectFraming,
        DecodeErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
        DecodeErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
        DecodeErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
        DecodeErrorCategory::Internal => DocumentErrorCode::InternalState,
    };
    DocumentError::for_code(code, Some(reference), Some(offset))
}

fn map_object_stream_error(
    error: ObjectStreamError,
    reference: ObjectRef,
    offset: u64,
) -> DocumentError {
    let code = match error.category() {
        ObjectStreamErrorCategory::Configuration => DocumentErrorCode::InvalidLimits,
        ObjectStreamErrorCategory::Source => DocumentErrorCode::SourceSnapshotMismatch,
        ObjectStreamErrorCategory::Syntax => DocumentErrorCode::ObjectResolutionFailure,
        ObjectStreamErrorCategory::Unsupported => DocumentErrorCode::UnsupportedObjectFraming,
        ObjectStreamErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
        ObjectStreamErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
        ObjectStreamErrorCategory::Internal => DocumentErrorCode::InternalState,
    };
    DocumentError::for_code(
        code,
        Some(reference),
        error.source_offset().or(Some(offset)),
    )
}
