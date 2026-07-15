use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSlice, SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{ByteSpan, ObjectRef};

use crate::limits::HARD_MAX_FILTERS;
use crate::{DecodeError, DecodeErrorCode, DecodeLimitKind, DecodeLimits};

/// Foundational PDF stream filter implemented by this crate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StreamFilter {
    /// The canonical PDF `FlateDecode` filter with a zlib wrapper.
    FlateDecode,
    /// The canonical PDF `ASCIIHexDecode` filter.
    AsciiHexDecode,
    /// The canonical PDF `ASCII85Decode` filter.
    Ascii85Decode,
    /// The canonical PDF `RunLengthDecode` filter.
    RunLengthDecode,
}

impl StreamFilter {
    /// Returns the canonical PDF name bytes without the leading slash.
    pub const fn canonical_pdf_name(self) -> &'static [u8] {
        match self {
            Self::FlateDecode => b"FlateDecode",
            Self::AsciiHexDecode => b"ASCIIHexDecode",
            Self::Ascii85Decode => b"ASCII85Decode",
            Self::RunLengthDecode => b"RunLengthDecode",
        }
    }
}

/// Ordered canonical stream-filter plan.
///
/// An empty plan selects the crate's internal identity path. `Identity` is not
/// accepted as a PDF filter name and is intentionally absent from
/// [`StreamFilter`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterPlan {
    filters: Vec<StreamFilter>,
    stages: Vec<FilterStage>,
}

impl FilterPlan {
    /// Builds an ordered plan from already canonical filter identifiers.
    pub fn new(filters: &[StreamFilter]) -> Result<Self, DecodeError> {
        validate_hard_filter_count(filters.len())?;
        let mut owned_filters = Vec::new();
        owned_filters
            .try_reserve_exact(filters.len())
            .map_err(|_| {
                DecodeError::resource(
                    DecodeLimitKind::Allocation,
                    u64::from(HARD_MAX_FILTERS),
                    0,
                    u64::try_from(filters.len()).unwrap_or(u64::MAX),
                    None,
                )
            })?;
        let mut stages = Vec::new();
        stages.try_reserve_exact(filters.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(filters.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        for filter in filters.iter().copied() {
            owned_filters.push(filter);
            stages.push(FilterStage::without_parameters(filter));
        }
        let plan = Self {
            filters: owned_filters,
            stages,
        };
        plan.validate_retained_heap_limit(HARD_MAX_FILTERS)?;
        Ok(plan)
    }

    /// Builds an ordered plan whose stages retain their canonical decode parameters.
    pub fn from_stages(stages: &[FilterStage]) -> Result<Self, DecodeError> {
        validate_hard_filter_count(stages.len())?;
        let mut owned_stages = Vec::new();
        owned_stages.try_reserve_exact(stages.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(stages.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        let mut filters = Vec::new();
        filters.try_reserve_exact(stages.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(stages.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        for (index, stage) in stages.iter().enumerate() {
            stage.validate(Some(
                u16::try_from(index).expect("hard filter count fits u16"),
            ))?;
            owned_stages.push(*stage);
            filters.push(stage.filter);
        }
        let plan = Self {
            filters,
            stages: owned_stages,
        };
        plan.validate_retained_heap_limit(HARD_MAX_FILTERS)?;
        Ok(plan)
    }

    /// Canonicalizes strict full PDF filter names in source order.
    ///
    /// Abbreviations and the non-standard name `Identity` are rejected as
    /// unsupported. Unknown name bytes are never retained in the returned error.
    pub fn from_pdf_names(names: &[&[u8]]) -> Result<Self, DecodeError> {
        validate_hard_filter_count(names.len())?;
        let mut filters = Vec::new();
        filters.try_reserve_exact(names.len()).map_err(|_| {
            DecodeError::resource(
                DecodeLimitKind::Allocation,
                u64::from(HARD_MAX_FILTERS),
                0,
                u64::try_from(names.len()).unwrap_or(u64::MAX),
                None,
            )
        })?;
        for (index, name) in names.iter().enumerate() {
            let filter = match *name {
                b"FlateDecode" => StreamFilter::FlateDecode,
                b"ASCIIHexDecode" => StreamFilter::AsciiHexDecode,
                b"ASCII85Decode" => StreamFilter::Ascii85Decode,
                b"RunLengthDecode" => StreamFilter::RunLengthDecode,
                _ => {
                    return Err(DecodeError::for_code(
                        DecodeErrorCode::UnsupportedFilter,
                        Some(u16::try_from(index).expect("hard filter count fits u16")),
                    ));
                }
            };
            filters.push(filter);
        }
        Self::new(&filters)
    }

    /// Returns the canonical filters in decode order.
    pub fn filters(&self) -> &[StreamFilter] {
        &self.filters
    }

    /// Returns canonical filter stages, including per-layer decode parameters.
    pub fn stages(&self) -> &[FilterStage] {
        &self.stages
    }

    /// Returns the number of explicit PDF filters.
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// Reports whether no explicit PDF filter is present.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Returns allocator-visible heap bytes retained by the two canonical plan vectors.
    ///
    /// The evidence uses the actual `Vec` capacities for both filters and stages, not
    /// their logical lengths. Inline `Vec` headers and constructor-local temporary
    /// buffers are not heap storage retained by this plan. A platform-size conversion,
    /// multiplication, or sum overflow is reported as [`DecodeErrorCode::InternalState`].
    pub fn retained_heap_bytes(&self) -> Result<u64, DecodeError> {
        retained_vec_bytes::<StreamFilter>(self.filters.capacity())?
            .checked_add(retained_vec_bytes::<FilterStage>(self.stages.capacity())?)
            .ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InternalState, None))
    }

    /// Returns the checked retained-heap upper bound for a filter-count ceiling.
    ///
    /// The bound covers the element storage of both canonical plan vectors at
    /// `max_filters` capacity. Zero or a value above the implementation's hard filter
    /// ceiling returns [`DecodeErrorCode::InvalidLimits`]; platform-size conversion,
    /// multiplication, or addition overflow returns [`DecodeErrorCode::InternalState`].
    /// Successful plans are checked against this bound before publication.
    pub fn retained_heap_upper_bound(max_filters: u16) -> Result<u64, DecodeError> {
        if max_filters == 0 || max_filters > HARD_MAX_FILTERS {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidLimits, None));
        }
        let count = u64::from(max_filters);
        u64::try_from(mem::size_of::<StreamFilter>())
            .ok()
            .and_then(|filter_width| count.checked_mul(filter_width))
            .and_then(|filters| {
                u64::try_from(mem::size_of::<FilterStage>())
                    .ok()
                    .and_then(|stage_width| count.checked_mul(stage_width))
                    .and_then(|stages| filters.checked_add(stages))
            })
            .ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InternalState, None))
    }

    pub(crate) fn validate_retained_heap_limit(
        &self,
        max_filters: u16,
    ) -> Result<u64, DecodeError> {
        let limit = Self::retained_heap_upper_bound(max_filters)?;
        let attempted = self.retained_heap_bytes()?;
        if attempted > limit {
            return Err(DecodeError::resource(
                DecodeLimitKind::FilterPlanBytes,
                limit,
                0,
                attempted,
                None,
            ));
        }
        Ok(attempted)
    }
}

fn retained_vec_bytes<T>(capacity: usize) -> Result<u64, DecodeError> {
    u64::try_from(capacity)
        .ok()
        .and_then(|count| {
            u64::try_from(mem::size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InternalState, None))
}

/// One canonical stream-filter layer and its bound decode parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilterStage {
    filter: StreamFilter,
    parameters: FilterDecodeParameters,
}

impl FilterStage {
    /// Creates a stage and validates whether the parameters apply to its filter.
    pub fn new(
        filter: StreamFilter,
        parameters: FilterDecodeParameters,
    ) -> Result<Self, DecodeError> {
        let stage = Self { filter, parameters };
        stage.validate(None)?;
        Ok(stage)
    }

    /// Creates a stage with no `/DecodeParms` entry.
    pub const fn without_parameters(filter: StreamFilter) -> Self {
        Self {
            filter,
            parameters: FilterDecodeParameters::None,
        }
    }

    /// Returns the canonical filter identifier.
    pub const fn filter(self) -> StreamFilter {
        self.filter
    }

    /// Returns the canonical decode parameters for this layer.
    pub const fn decode_parameters(self) -> FilterDecodeParameters {
        self.parameters
    }

    fn validate(self, filter_index: Option<u16>) -> Result<(), DecodeError> {
        if matches!(self.parameters, FilterDecodeParameters::Predictor(_))
            && self.filter != StreamFilter::FlateDecode
        {
            return Err(DecodeError::for_code(
                DecodeErrorCode::UnsupportedDecodeParameters,
                filter_index,
            ));
        }
        Ok(())
    }
}

/// Canonical per-filter `/DecodeParms` supported by the M1 profile.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FilterDecodeParameters {
    /// No decode-parameter dictionary is present for this filter layer.
    #[default]
    None,
    /// TIFF or PNG prediction parameters applied after `FlateDecode`.
    Predictor(PredictorParameters),
}

/// Validated TIFF/PNG predictor parameters with PDF defaults made explicit.
///
/// Positive PNG predictor integers are retained through `i64::MAX`, the upper
/// bound of the syntax layer's PDF integer model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PredictorParameters {
    predictor: i64,
    colors: u32,
    bits_per_component: u8,
    columns: u32,
}

impl PredictorParameters {
    /// Validates explicit predictor, color, component-width, and column values.
    ///
    /// Predictor 1 selects no transform, 2 selects TIFF horizontal
    /// differencing, and every value from 10 through `i64::MAX` selects PNG
    /// row-tag prediction.
    pub fn new(
        predictor: i64,
        colors: i64,
        bits_per_component: i64,
        columns: i64,
    ) -> Result<Self, DecodeError> {
        if predictor <= 0 || colors <= 0 || columns <= 0 || bits_per_component <= 0 {
            return Err(DecodeError::for_code(
                DecodeErrorCode::InvalidDecodeParameters,
                None,
            ));
        }
        if (predictor != 1 && predictor != 2 && predictor < 10)
            || !matches!(bits_per_component, 1 | 2 | 4 | 8 | 16)
        {
            return Err(DecodeError::for_code(
                DecodeErrorCode::UnsupportedPredictor,
                None,
            ));
        }
        let bits_per_component = u8::try_from(bits_per_component)
            .map_err(|_| DecodeError::for_code(DecodeErrorCode::UnsupportedPredictor, None))?;
        let colors = u32::try_from(colors)
            .map_err(|_| DecodeError::for_code(DecodeErrorCode::InvalidDecodeParameters, None))?;
        let columns = u32::try_from(columns)
            .map_err(|_| DecodeError::for_code(DecodeErrorCode::InvalidDecodeParameters, None))?;
        let row_bits = u64::from(colors)
            .checked_mul(u64::from(columns))
            .and_then(|value| value.checked_mul(u64::from(bits_per_component)))
            .ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InvalidDecodeParameters, None))?;
        row_bits
            .checked_add(7)
            .ok_or_else(|| DecodeError::for_code(DecodeErrorCode::InvalidDecodeParameters, None))?;
        Ok(Self {
            predictor,
            colors,
            bits_per_component,
            columns,
        })
    }

    /// Returns the PDF `/Predictor` value.
    pub const fn predictor(self) -> i64 {
        self.predictor
    }

    /// Returns the positive PDF `/Colors` value.
    pub const fn colors(self) -> u32 {
        self.colors
    }

    /// Returns the supported PDF `/BitsPerComponent` value.
    pub const fn bits_per_component(self) -> u8 {
        self.bits_per_component
    }

    /// Returns the positive PDF `/Columns` value.
    pub const fn columns(self) -> u32 {
        self.columns
    }
}

impl Default for PredictorParameters {
    fn default() -> Self {
        Self {
            predictor: 1,
            colors: 1,
            bits_per_component: 8,
            columns: 1,
        }
    }
}

fn validate_hard_filter_count(count: usize) -> Result<(), DecodeError> {
    if count > usize::from(HARD_MAX_FILTERS) {
        return Err(DecodeError::resource(
            DecodeLimitKind::FilterCount,
            u64::from(HARD_MAX_FILTERS),
            0,
            u64::try_from(count).unwrap_or(u64::MAX),
            None,
        ));
    }
    Ok(())
}

/// Version of deterministic stream-decoding fuel weights.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DecodeFuelScheduleVersion {
    /// One unit per setup/algorithm step, consumed input byte, and emitted output byte.
    M1V1,
}

impl DecodeFuelScheduleVersion {
    pub(crate) const fn layer_setup_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn input_byte_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn output_byte_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }

    pub(crate) const fn algorithm_step_cost(self) -> u64 {
        match self {
            Self::M1V1 => 1,
        }
    }
}

/// Strict decoding semantics selected for one request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DecodeProfile {
    /// M1 strict profile with required end markers and no trailing data.
    M1StrictV1,
}

impl DecodeProfile {
    /// Returns the deterministic fuel schedule bound to this profile.
    pub const fn fuel_schedule(self) -> DecodeFuelScheduleVersion {
        match self {
            Self::M1StrictV1 => DecodeFuelScheduleVersion::M1V1,
        }
    }
}

/// Fully bound request for one exact physical encoded stream slice.
pub struct DecodeRequest {
    pub(crate) snapshot: SourceSnapshot,
    pub(crate) owner: ObjectRef,
    pub(crate) dictionary_span: ByteSpan,
    pub(crate) encoded_span: ByteSpan,
    pub(crate) encoded: ByteSlice,
    pub(crate) plan: FilterPlan,
    pub(crate) profile: DecodeProfile,
    pub(crate) limits: DecodeLimits,
}

impl DecodeRequest {
    /// Validates and binds the complete immutable decode request.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        snapshot: SourceSnapshot,
        owner: ObjectRef,
        dictionary_span: ByteSpan,
        encoded_span: ByteSpan,
        encoded: ByteSlice,
        plan: FilterPlan,
        profile: DecodeProfile,
        limits: DecodeLimits,
    ) -> Result<Self, DecodeError> {
        if encoded.identity() != snapshot.identity() {
            return Err(DecodeError::for_code(DecodeErrorCode::SourceChanged, None));
        }
        let range = encoded.range();
        if range.start() != encoded_span.start() || range.len() != encoded_span.len() {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidRequest, None));
        }
        if snapshot.len().is_some_and(|source_len| {
            encoded_span.end_exclusive() > source_len
                || dictionary_span.end_exclusive() > source_len
        }) {
            return Err(DecodeError::for_code(DecodeErrorCode::InvalidRequest, None));
        }
        Ok(Self {
            snapshot,
            owner,
            dictionary_span,
            encoded_span,
            encoded,
            plan,
            profile,
            limits,
        })
    }
}

impl fmt::Debug for DecodeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeRequest")
            .field("snapshot", &self.snapshot)
            .field("owner", &self.owner)
            .field("dictionary_span", &self.dictionary_span)
            .field("encoded_span", &self.encoded_span)
            .field("encoded", &self.encoded)
            .field("plan", &self.plan)
            .field("profile", &self.profile)
            .field("limits", &self.limits)
            .finish()
    }
}

/// Zero-based position in decoded output, never in physical source bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedOffset(u64);

impl DecodedOffset {
    /// Creates a decoded-relative offset.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the decoded-relative numeric value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Checked half-open range in decoded output coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecodedRange {
    start: DecodedOffset,
    len: u64,
}

impl DecodedRange {
    /// Creates a decoded-relative range and rejects exclusive-end overflow.
    pub const fn new(start: DecodedOffset, len: u64) -> Option<Self> {
        if start.0.checked_add(len).is_some() {
            Some(Self { start, len })
        } else {
            None
        }
    }

    /// Returns the first decoded-relative byte position.
    pub const fn start(self) -> DecodedOffset {
        self.start
    }

    /// Returns the number of decoded bytes.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Reports whether the range contains no decoded bytes.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Returns the checked decoded-relative exclusive end.
    pub const fn end_exclusive(self) -> DecodedOffset {
        DecodedOffset(self.start.0 + self.len)
    }
}

/// Sealed evidence binding decoded bytes to their complete decode context.
///
/// This type intentionally does not implement [`Clone`]. Callers may only
/// borrow it from a successful [`DecodedStream`].
pub struct DecodeAttestation {
    pub(crate) snapshot: SourceSnapshot,
    pub(crate) source_identity: SourceIdentity,
    pub(crate) owner: ObjectRef,
    pub(crate) dictionary_span: ByteSpan,
    pub(crate) encoded_span: ByteSpan,
    pub(crate) encoded: ByteSlice,
    pub(crate) plan: FilterPlan,
    pub(crate) profile: DecodeProfile,
    pub(crate) limits: DecodeLimits,
    pub(crate) fuel_schedule: DecodeFuelScheduleVersion,
    pub(crate) fuel_consumed: u64,
    pub(crate) cumulative_output_bytes: u64,
    pub(crate) peak_retained_capacity_bytes: u64,
    pub(crate) plan_retained_heap_bytes: u64,
    pub(crate) decoded_length: u64,
}

impl DecodeAttestation {
    /// Returns the immutable source snapshot bound to the decode.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the immutable source identity bound to every physical span.
    pub const fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }

    /// Returns the indirect object that owns the stream.
    pub const fn owner(&self) -> ObjectRef {
        self.owner
    }

    /// Returns the exact physical source span of the owning dictionary.
    pub const fn dictionary_span(&self) -> ByteSpan {
        self.dictionary_span
    }

    /// Returns the exact physical source span of the encoded bytes.
    pub const fn encoded_span(&self) -> ByteSpan {
        self.encoded_span
    }

    /// Borrows the exact stable physical encoded byte slice.
    pub const fn encoded(&self) -> &ByteSlice {
        &self.encoded
    }

    /// Returns the canonical ordered filter plan.
    pub const fn filter_plan(&self) -> &FilterPlan {
        &self.plan
    }

    /// Returns the strict decode profile.
    pub const fn profile(&self) -> DecodeProfile {
        self.profile
    }

    /// Returns all deterministic limits bound to the result.
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }

    /// Returns the versioned fuel schedule used by the decode.
    pub const fn fuel_schedule(&self) -> DecodeFuelScheduleVersion {
        self.fuel_schedule
    }

    /// Returns deterministic fuel consumed by the successful decode.
    pub const fn fuel_consumed(&self) -> u64 {
        self.fuel_consumed
    }

    /// Returns cumulative bytes emitted across every layer.
    pub const fn cumulative_output_bytes(&self) -> u64 {
        self.cumulative_output_bytes
    }

    /// Returns peak allocator-reported capacity retained by simultaneous outputs.
    pub const fn peak_retained_capacity_bytes(&self) -> u64 {
        self.peak_retained_capacity_bytes
    }

    /// Returns actual heap capacity retained by the canonical filter plan.
    ///
    /// This excludes decoded output capacity, which is reported separately by
    /// [`Self::peak_retained_capacity_bytes`].
    pub const fn plan_retained_heap_bytes(&self) -> u64 {
        self.plan_retained_heap_bytes
    }

    /// Returns the final decoded byte length.
    pub const fn decoded_length(&self) -> u64 {
        self.decoded_length
    }
}

impl fmt::Debug for DecodeAttestation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodeAttestation")
            .field("snapshot", &self.snapshot)
            .field("source_identity", &self.source_identity)
            .field("owner", &self.owner)
            .field("dictionary_span", &self.dictionary_span)
            .field("encoded_span", &self.encoded_span)
            .field("encoded", &self.encoded)
            .field("plan", &self.plan)
            .field("profile", &self.profile)
            .field("limits", &self.limits)
            .field("fuel_schedule", &self.fuel_schedule)
            .field("fuel_consumed", &self.fuel_consumed)
            .field("cumulative_output_bytes", &self.cumulative_output_bytes)
            .field(
                "peak_retained_capacity_bytes",
                &self.peak_retained_capacity_bytes,
            )
            .field("plan_retained_heap_bytes", &self.plan_retained_heap_bytes)
            .field("decoded_length", &self.decoded_length)
            .finish()
    }
}

/// Immutable decoded bytes inseparable from their sealed attestation.
///
/// This type intentionally does not implement [`Clone`] and exposes no
/// consuming byte extraction that could discard the attestation.
pub struct DecodedStream {
    pub(crate) bytes: Vec<u8>,
    pub(crate) attestation: DecodeAttestation,
}

impl DecodedStream {
    /// Borrows the final decoded bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the final decoded byte length.
    pub const fn len(&self) -> u64 {
        self.attestation.decoded_length
    }

    /// Reports whether the decoded result is empty.
    pub const fn is_empty(&self) -> bool {
        self.attestation.decoded_length == 0
    }

    /// Returns the complete decoded-relative range.
    pub const fn decoded_range(&self) -> DecodedRange {
        DecodedRange {
            start: DecodedOffset(0),
            len: self.attestation.decoded_length,
        }
    }

    /// Borrows a checked decoded-relative subrange.
    pub fn slice(&self, range: DecodedRange) -> Option<&[u8]> {
        if range.end_exclusive().value() > self.attestation.decoded_length {
            return None;
        }
        let start = usize::try_from(range.start().value()).ok()?;
        let end = usize::try_from(range.end_exclusive().value()).ok()?;
        self.bytes.get(start..end)
    }

    /// Borrows the sealed decode attestation.
    pub const fn attestation(&self) -> &DecodeAttestation {
        &self.attestation
    }
}

impl fmt::Debug for DecodedStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedStream")
            .field("bytes", &"[REDACTED]")
            .field("attestation", &self.attestation)
            .finish()
    }
}
