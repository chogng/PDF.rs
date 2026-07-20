use std::fmt;

use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeLimitKind, DecodeProfile,
    DecodedStream, FilterPlan,
};

use crate::object_stream::{
    ObjectStreamPayloadCoordinates, check_cancelled, parse_decoded_object_stream,
    require_generation_zero_stream,
};
use crate::{
    IndirectObject, ObjectCancellation, ObjectStream, ObjectStreamError, ObjectStreamErrorCode,
    ObjectStreamLimits,
};

/// Proof-bound filtered object stream retaining framing, decoding, and semantic evidence.
///
/// The three owned proofs cannot be extracted by value. Borrowed access preserves their shared
/// lifetime while allowing later resolution layers to query the parsed object-stream semantics.
pub struct FilteredObjectStream {
    framed_container: IndirectObject,
    decoded_proof: DecodedStream,
    object_stream: ObjectStream,
    retained_proof_bytes: u64,
}

impl FilteredObjectStream {
    /// Borrows the complete generation-zero framed `/ObjStm` container.
    pub const fn framed_container(&self) -> &IndirectObject {
        &self.framed_container
    }

    /// Borrows the sealed decoder output and its complete attestation.
    pub const fn decoded_proof(&self) -> &DecodedStream {
        &self.decoded_proof
    }

    /// Borrows the decoded-coordinate object-stream semantics.
    pub const fn object_stream(&self) -> &ObjectStream {
        &self.object_stream
    }

    /// Returns conservative heap evidence retained by all three owned proofs.
    ///
    /// This adds framed syntax heap, decoder peak output capacity, actual canonical-plan heap,
    /// and parsed entry/value capacity exactly once. The encoded `ByteSlice` backing remains
    /// charged to its source store and is not counted again here.
    pub const fn retained_proof_bytes(&self) -> u64 {
        self.retained_proof_bytes
    }
}

impl fmt::Debug for FilteredObjectStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilteredObjectStream")
            .field("framed_container", &self.framed_container)
            .field("decoded_proof", &self.decoded_proof)
            .field("object_stream", &self.object_stream)
            .field("retained_proof_bytes", &self.retained_proof_bytes)
            .finish()
    }
}

/// Parses a filtered object stream only from matching framed and sealed decode proofs.
///
/// This function consumes both proofs and publishes them only inside one inseparable result. It
/// verifies the immutable snapshot, owner, dictionary and encoded spans, exact encoded
/// `ByteSlice`, strict decode profile, and nonempty canonical filter plan before reusing the same
/// decoded-coordinate semantic parser as [`crate::parse_unfiltered_object_stream`]. The object
/// layer delegates all filter metadata interpretation to the filter crate, then requires its
/// reconstructed plan to equal the decoder attestation exactly. The attested plan remains the
/// only retained canonical authority.
pub fn parse_filtered_object_stream(
    framed_container: IndirectObject,
    decoded_proof: DecodedStream,
    limits: ObjectStreamLimits,
    cancellation: &(dyn ObjectCancellation + '_),
) -> Result<FilteredObjectStream, ObjectStreamError> {
    check_cancelled(cancellation)?;
    let stream = require_generation_zero_stream(&framed_container)?;
    validate_decode_authority(&framed_container, stream, &decoded_proof, cancellation)?;
    check_cancelled(cancellation)?;
    let object_stream = parse_decoded_object_stream(
        &framed_container,
        stream,
        decoded_proof.bytes(),
        ObjectStreamPayloadCoordinates::Decoded,
        limits,
        cancellation,
    )?;
    let attestation = decoded_proof.attestation();
    let semantic = object_stream.stats();
    let retained_proof_bytes = framed_container
        .retained_heap_bytes()
        .checked_add(attestation.peak_retained_capacity_bytes())
        .and_then(|value| value.checked_add(attestation.plan_retained_heap_bytes()))
        .and_then(|value| value.checked_add(semantic.retained_entry_bytes()))
        .and_then(|value| value.checked_add(semantic.retained_value_bytes()))
        .ok_or_else(|| {
            ObjectStreamError::at_source(
                ObjectStreamErrorCode::InternalState,
                Some(stream.data_span().start()),
            )
        })?;
    check_cancelled(cancellation)?;

    Ok(FilteredObjectStream {
        framed_container,
        decoded_proof,
        object_stream,
        retained_proof_bytes,
    })
}

fn validate_decode_authority(
    container: &IndirectObject,
    stream: &crate::FramedStream,
    decoded: &DecodedStream,
    cancellation: &dyn ObjectCancellation,
) -> Result<(), ObjectStreamError> {
    let attestation = decoded.attestation();
    let encoded = attestation.encoded();
    let encoded_range = encoded.range();
    let encoded_length = u64::try_from(encoded.bytes().len()).ok();
    let decoded_length = u64::try_from(decoded.bytes().len()).ok();
    if attestation.snapshot() != container.snapshot()
        || attestation.source_identity() != container.snapshot().identity()
        || attestation.owner() != container.reference()
        || attestation.dictionary_span() != stream.dictionary().span()
        || attestation.encoded_span() != stream.data_span()
        || encoded.identity() != container.snapshot().identity()
        || encoded_range.start() != stream.data_span().start()
        || encoded_range.len() != stream.data_span().len()
        || encoded_length != Some(stream.data_span().len())
        || decoded_length != Some(attestation.decoded_length())
        || attestation.profile() != DecodeProfile::M1StrictV1
    {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::DecodeProofMismatch,
            Some(stream.data_span().start()),
        ));
    }
    let canonical_plan = FilterPlan::from_pdf_dictionary(
        stream.dictionary().value(),
        attestation.limits(),
        &FilterMetadataCancellation(cancellation),
    )
    .map_err(|error| map_filter_metadata_error(error, stream.dictionary().span().start()))?;
    if canonical_plan.is_empty() || &canonical_plan != attestation.filter_plan() {
        return Err(ObjectStreamError::at_source(
            ObjectStreamErrorCode::DecodeProofMismatch,
            Some(stream.data_span().start()),
        ));
    }
    Ok(())
}

struct FilterMetadataCancellation<'a>(&'a dyn ObjectCancellation);

impl DecodeCancellation for FilterMetadataCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn map_filter_metadata_error(error: DecodeError, source_offset: u64) -> ObjectStreamError {
    match error.category() {
        DecodeErrorCategory::Syntax => ObjectStreamError::at_source(
            ObjectStreamErrorCode::InvalidDictionary,
            Some(source_offset),
        ),
        DecodeErrorCategory::Unsupported => ObjectStreamError::at_source(
            ObjectStreamErrorCode::UnsupportedFilter,
            Some(source_offset),
        ),
        DecodeErrorCategory::Resource => error.limit().map_or_else(
            || {
                ObjectStreamError::at_source(
                    ObjectStreamErrorCode::InternalState,
                    Some(source_offset),
                )
            },
            |limit| {
                let kind = match limit.kind() {
                    DecodeLimitKind::FilterCount => crate::ObjectStreamLimitKind::FilterCount,
                    DecodeLimitKind::FilterPlanBytes => {
                        crate::ObjectStreamLimitKind::FilterPlanBytes
                    }
                    DecodeLimitKind::Allocation => {
                        crate::ObjectStreamLimitKind::FilterPlanAllocation
                    }
                    DecodeLimitKind::InputBytes
                    | DecodeLimitKind::LayerOutputBytes
                    | DecodeLimitKind::TotalOutputBytes
                    | DecodeLimitKind::FinalOutputBytes
                    | DecodeLimitKind::RetainedCapacityBytes
                    | DecodeLimitKind::Fuel => {
                        return ObjectStreamError::at_source(
                            ObjectStreamErrorCode::InternalState,
                            Some(source_offset),
                        );
                    }
                };
                ObjectStreamError::resource(
                    kind,
                    limit.limit(),
                    limit.consumed(),
                    limit.attempted(),
                    Some(source_offset),
                    None,
                )
            },
        ),
        DecodeErrorCategory::Cancellation => {
            ObjectStreamError::at_source(ObjectStreamErrorCode::Cancelled, None)
        }
        DecodeErrorCategory::Integrity => {
            ObjectStreamError::at_source(ObjectStreamErrorCode::SourceMismatch, Some(source_offset))
        }
        DecodeErrorCategory::Configuration | DecodeErrorCategory::Internal => {
            ObjectStreamError::at_source(ObjectStreamErrorCode::InternalState, Some(source_offset))
        }
    }
}
