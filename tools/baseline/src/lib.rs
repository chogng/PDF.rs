#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Versioned, development-only protocol for process-isolated PDF baselines.
//!
//! The provided direct-child supervisor uses bounded concurrent pipes, a
//! watchdog, and kill/reap cleanup. It is not a platform sandbox: callers must
//! add reviewed descendant, resource, filesystem, and network containment
//! before using it with a real external engine.

mod pdfium;
mod pdfium_page_count;
mod process;

use std::fmt;

use pdf_rs_digest::{Sha256, sha256};

pub use pdfium::{
    PDFIUM_OUTLINE_ADAPTER_MAX_PARSE_BYTES, PDFIUM_OUTLINE_ADAPTER_PROFILE,
    PDFIUM_PIXEL_ADAPTER_MAX_RGBA_BYTES, PDFIUM_PIXEL_ADAPTER_PROFILE, PdfiumOutlineAdapter,
    PdfiumPixelAdapter,
};
pub use pdfium_page_count::{
    PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES, PDFIUM_PAGE_COUNT_ADAPTER_PROFILE,
    PdfiumPageCountAdapter,
};
pub use process::{ProcessBaselineRunner, ProcessLimits, ProcessSpec};

const REQUEST_MAGIC: &[u8; 8] = b"PRSBREQ2";
const RESPONSE_MAGIC: &[u8; 8] = b"PRSBOBS2";
const SCHEMA_VERSION: u16 = 2;
const REQUEST_HEADER_LEN: usize = 96;
const RESPONSE_HEADER_LEN: usize = 112;

/// The only authority an external black-box observation may carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleAuthority {
    /// Untrusted external implementation output used only to discover disagreements.
    O4Observation,
}

/// Complete identity of a separately built baseline environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaselineDescriptor {
    /// Stable adapter configuration identifier.
    pub id: String,
    /// External engine name.
    pub engine: String,
    /// Immutable upstream source revision.
    pub upstream_revision: String,
    /// Digest of the executable and runtime binary payloads.
    pub build_hash: [u8; 32],
    /// Digest of canonical build flags and toolchain configuration.
    pub build_flags_hash: [u8; 32],
    /// Digest of the host, sandbox, locale, and invocation environment.
    pub environment_hash: [u8; 32],
    /// Digest of canonical executable path, argv, environment, cwd, transport, and isolation policy.
    pub invocation_hash: [u8; 32],
    /// Digest of the reviewed dependency and license manifest.
    pub license_manifest_hash: [u8; 32],
    /// Digest of the exact font files visible to the runner.
    pub fonts_hash: [u8; 32],
    /// Digest of the renderer and color-management configuration.
    pub color_hash: [u8; 32],
    /// Canonical target platform identifier.
    pub platform: String,
}

/// A verified immutable corpus object and bounded page observation request.
///
/// PDF bytes are private and this type has no `Debug` implementation.
pub struct BaselineRequest {
    source_hash: [u8; 32],
    pdf: Vec<u8>,
    page: u32,
    width: u32,
    height: u32,
}

impl BaselineRequest {
    /// Verifies that `pdf` has the caller's fixed corpus identity before storing it.
    pub fn new(
        expected_source_hash: [u8; 32],
        pdf: Vec<u8>,
        page: u32,
        width: u32,
        height: u32,
    ) -> Result<Self, BaselineError> {
        expected_rgba_len(width, height)?;
        let actual = sha256(&pdf).map_err(|_| invalid_request())?;
        if actual != expected_source_hash {
            return Err(BaselineError::new(
                BaselineErrorCode::SourceHashMismatch,
                "RPE-BASELINE-0002",
                "PDF bytes do not match the fixed corpus identity",
            ));
        }
        Ok(Self {
            source_hash: actual,
            pdf,
            page,
            width,
            height,
        })
    }

    /// Returns the verified immutable PDF identity.
    pub const fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }

    /// Borrows the private PDF bytes for protocol encoding.
    pub fn pdf(&self) -> &[u8] {
        &self.pdf
    }

    /// Returns the zero-based requested page index.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Returns the required output width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the required output height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// A validated schema-2 request as seen by an external adapter process.
///
/// The request retains the private PDF bytes and the expected descriptor
/// identity from the host frame. It deliberately has no `Debug`
/// implementation.
pub struct AdapterRequest {
    source_hash: [u8; 32],
    descriptor_identity: [u8; 32],
    pdf: Vec<u8>,
    page: u32,
    width: u32,
    height: u32,
}

impl AdapterRequest {
    /// Returns the verified immutable PDF identity.
    pub const fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }

    /// Returns the host-supplied descriptor identity that the response must echo.
    pub const fn descriptor_identity(&self) -> [u8; 32] {
        self.descriptor_identity
    }

    /// Borrows the private PDF bytes.
    pub fn pdf(&self) -> &[u8] {
        &self.pdf
    }

    /// Returns the zero-based requested page index.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Returns the exact requested output width.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the exact requested output height.
    pub const fn height(&self) -> u32 {
        self.height
    }
}

/// Per-channel result from an external baseline.
///
/// `Unsupported` and `Failed` never carry placeholder content, preventing an
/// absent baseline capability from being confused with a produced empty artifact.
#[derive(Clone, Eq, PartialEq)]
pub enum BaselineChannel<T> {
    /// The runner produced and framed this channel.
    Produced(T),
    /// The configured runner cannot produce this channel.
    Unsupported,
    /// The runner supports the channel but could not produce it for this request.
    Failed,
}

impl<T> BaselineChannel<T> {
    /// Returns a shared reference to produced content and preserves non-produced states.
    pub const fn as_ref(&self) -> BaselineChannel<&T> {
        match self {
            Self::Produced(value) => BaselineChannel::Produced(value),
            Self::Unsupported => BaselineChannel::Unsupported,
            Self::Failed => BaselineChannel::Failed,
        }
    }

    /// Reports whether the runner produced this channel.
    pub const fn is_produced(&self) -> bool {
        matches!(self, Self::Produced(_))
    }
}

impl<T> fmt::Debug for BaselineChannel<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Produced(_) => "Produced([REDACTED])",
            Self::Unsupported => "Unsupported",
            Self::Failed => "Failed",
        })
    }
}

/// Borrowed channel payloads supplied by an adapter for one successful response.
///
/// Produced values must be non-empty. Unsupported and failed channels carry no
/// placeholder bytes. This type deliberately has no `Debug` implementation.
pub struct AdapterResponseChannels<'a> {
    /// Canonical parse artifact outcome.
    pub parse_json: BaselineChannel<&'a [u8]>,
    /// Canonical Scene artifact outcome.
    pub scene_json: BaselineChannel<&'a [u8]>,
    /// Canonical positioned-text artifact outcome.
    pub text_json: BaselineChannel<&'a [u8]>,
    /// Row-major straight-alpha RGBA8 outcome.
    pub rgba: BaselineChannel<&'a [u8]>,
}

impl<'a> AdapterResponseChannels<'a> {
    /// Creates an explicit four-channel response without synthesizing missing data.
    pub const fn new(
        parse_json: BaselineChannel<&'a [u8]>,
        scene_json: BaselineChannel<&'a [u8]>,
        text_json: BaselineChannel<&'a [u8]>,
        rgba: BaselineChannel<&'a [u8]>,
    ) -> Self {
        Self {
            parse_json,
            scene_json,
            text_json,
            rgba,
        }
    }
}

/// Canonical artifacts bound to one descriptor and verified request.
///
/// This type deliberately has no `Debug` implementation because the artifacts
/// can contain document text and pixels.
pub struct BaselineObservation {
    /// Complete build and environment identity supplied by the adapter.
    pub descriptor: BaselineDescriptor,
    /// Echoed and verified immutable PDF identity.
    pub source_hash: [u8; 32],
    /// Echoed and verified zero-based page index.
    pub page: u32,
    /// Canonical parse artifact bytes; potentially document-sensitive.
    pub parse_json: BaselineChannel<Vec<u8>>,
    /// Canonical Scene artifact bytes; potentially document-sensitive.
    pub scene_json: BaselineChannel<Vec<u8>>,
    /// Canonical positioned-text artifact bytes; document-sensitive.
    pub text_json: BaselineChannel<Vec<u8>>,
    /// Verified RGBA width.
    pub width: u32,
    /// Verified RGBA height.
    pub height: u32,
    /// Row-major RGBA8 pixels; potentially document-sensitive.
    pub rgba: BaselineChannel<Vec<u8>>,
}

impl BaselineObservation {
    /// Returns the fixed O4 authority ceiling for every external observation.
    pub const fn authority(&self) -> OracleAuthority {
        OracleAuthority::O4Observation
    }
}

/// Stable baseline-protocol failure classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaselineErrorCode {
    /// Request size, geometry, or descriptor framing is invalid.
    InvalidRequest,
    /// Supplied PDF bytes do not match the declared source digest.
    SourceHashMismatch,
    /// A response exceeds the caller's byte ceiling or cannot be allocated safely.
    OutputLimit,
    /// An encoded request exceeds the configured process input ceiling.
    RequestLimit,
    /// A response violates the fixed wire schema.
    MalformedResponse,
    /// The external runner reported a terminal observation failure.
    RunnerFailed,
    /// Source, descriptor, page, or geometry does not match the request.
    IdentityMismatch,
    /// Executable, invocation identity, limits, or isolation metadata is invalid.
    InvalidProcessConfig,
    /// The configured process could not be started directly.
    ProcessSpawnFailed,
    /// A stdin/stdout/stderr transport operation failed.
    TransportFailed,
    /// The direct child exceeded its wall-clock safety deadline.
    WatchdogExpired,
    /// The host could not prove that inherited process resources were contained.
    ContainmentFailed,
}

/// Stable coarse category for baseline-tool failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaselineErrorCategory {
    /// The immutable request or its declared identity is invalid.
    Request,
    /// A configured byte or wall-clock resource boundary was reached.
    ResourceLimit,
    /// The child response violated the baseline wire contract.
    Protocol,
    /// The executable or invocation configuration is not acceptable.
    Configuration,
    /// Starting or communicating with the direct child failed.
    Process,
    /// The supervisor could not prove direct-child or pipe containment.
    Containment,
}

/// Stable recovery policy class for baseline-tool failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BaselineRecoverability {
    /// The caller must supply a corrected immutable request.
    CorrectRequest,
    /// The reviewed adapter configuration or resource policy must change.
    CorrectConfiguration,
    /// Policy may retry with a fresh, independently contained direct child.
    RetryFreshProcess,
    /// Repeating the same observation is not an approved recovery action.
    DoNotRetry,
}

/// Stable, content-redacted baseline protocol failure.
#[derive(Debug)]
pub struct BaselineError {
    /// Exact machine-classifiable failure code.
    pub code: BaselineErrorCode,
    /// Coarse subsystem and policy category derived from [`Self::code`].
    pub category: BaselineErrorCategory,
    /// Approved caller recovery class derived from [`Self::code`].
    pub recoverability: BaselineRecoverability,
    /// Stable project diagnostic identifier.
    pub diagnostic_id: &'static str,
    detail: &'static str,
}

impl BaselineError {
    fn new(code: BaselineErrorCode, diagnostic_id: &'static str, detail: &'static str) -> Self {
        let (category, recoverability) = error_policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            detail,
        }
    }
}

impl fmt::Display for BaselineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.diagnostic_id, self.code, self.detail
        )
    }
}

impl std::error::Error for BaselineError {}

const fn error_policy(code: BaselineErrorCode) -> (BaselineErrorCategory, BaselineRecoverability) {
    match code {
        BaselineErrorCode::InvalidRequest | BaselineErrorCode::SourceHashMismatch => (
            BaselineErrorCategory::Request,
            BaselineRecoverability::CorrectRequest,
        ),
        BaselineErrorCode::OutputLimit | BaselineErrorCode::RequestLimit => (
            BaselineErrorCategory::ResourceLimit,
            BaselineRecoverability::CorrectConfiguration,
        ),
        BaselineErrorCode::MalformedResponse | BaselineErrorCode::IdentityMismatch => (
            BaselineErrorCategory::Protocol,
            BaselineRecoverability::CorrectConfiguration,
        ),
        BaselineErrorCode::InvalidProcessConfig => (
            BaselineErrorCategory::Configuration,
            BaselineRecoverability::CorrectConfiguration,
        ),
        BaselineErrorCode::ProcessSpawnFailed | BaselineErrorCode::TransportFailed => (
            BaselineErrorCategory::Process,
            BaselineRecoverability::RetryFreshProcess,
        ),
        BaselineErrorCode::RunnerFailed => (
            BaselineErrorCategory::Process,
            BaselineRecoverability::DoNotRetry,
        ),
        BaselineErrorCode::WatchdogExpired => (
            BaselineErrorCategory::ResourceLimit,
            BaselineRecoverability::DoNotRetry,
        ),
        BaselineErrorCode::ContainmentFailed => (
            BaselineErrorCategory::Containment,
            BaselineRecoverability::CorrectConfiguration,
        ),
    }
}

/// Interface for reviewed, process-contained O4 observation adapters.
pub trait BaselineRunner {
    /// Describes the exact build and environment before accepting observations.
    fn describe(&self) -> Result<BaselineDescriptor, BaselineError>;

    /// Runs one deadline- and byte-limited O4 observation.
    ///
    /// Implementations must concurrently drain pipes, enforce a watchdog, and
    /// attempt child termination on every timeout or transport failure. They
    /// must return [`BaselineErrorCode::ContainmentFailed`] unless reaping and
    /// pipe closure are proven.
    fn observe(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError>;
}

/// Hashes every baseline build and environment identity field in canonical order.
pub fn descriptor_identity(descriptor: &BaselineDescriptor) -> Result<[u8; 32], BaselineError> {
    if [
        descriptor.id.as_str(),
        descriptor.engine.as_str(),
        descriptor.upstream_revision.as_str(),
        descriptor.platform.as_str(),
    ]
    .into_iter()
    .any(|value| value.trim().is_empty())
        || [
            descriptor.build_hash,
            descriptor.build_flags_hash,
            descriptor.environment_hash,
            descriptor.invocation_hash,
            descriptor.license_manifest_hash,
            descriptor.fonts_hash,
            descriptor.color_hash,
        ]
        .into_iter()
        .any(|value| value == [0; 32])
    {
        return Err(invalid_request());
    }

    let mut hasher = Sha256::new();
    hasher
        .update(b"PDFRS-BASELINE-DESCRIPTOR-2")
        .map_err(|_| invalid_request())?;
    for value in [
        descriptor.id.as_bytes(),
        descriptor.engine.as_bytes(),
        descriptor.upstream_revision.as_bytes(),
        descriptor.platform.as_bytes(),
    ] {
        let length = u64::try_from(value.len()).map_err(|_| invalid_request())?;
        hasher
            .update(&length.to_be_bytes())
            .map_err(|_| invalid_request())?;
        hasher.update(value).map_err(|_| invalid_request())?;
    }
    for value in [
        descriptor.build_hash,
        descriptor.build_flags_hash,
        descriptor.environment_hash,
        descriptor.invocation_hash,
        descriptor.license_manifest_hash,
        descriptor.fonts_hash,
        descriptor.color_hash,
    ] {
        hasher.update(&value).map_err(|_| invalid_request())?;
    }
    hasher.finalize().map_err(|_| invalid_request())
}

/// Encodes a request bound to the complete expected baseline identity.
pub fn encode_request(
    request: &BaselineRequest,
    descriptor: &BaselineDescriptor,
) -> Result<Vec<u8>, BaselineError> {
    let identity = descriptor_identity(descriptor)?;
    let pdf_len = u64::try_from(request.pdf.len()).map_err(|_| invalid_request())?;
    let capacity = REQUEST_HEADER_LEN
        .checked_add(request.pdf.len())
        .ok_or_else(invalid_request)?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(capacity)
        .map_err(|_| invalid_request())?;
    frame.extend_from_slice(REQUEST_MAGIC);
    frame.extend_from_slice(&SCHEMA_VERSION.to_be_bytes());
    frame.extend_from_slice(&0_u16.to_be_bytes());
    frame.extend_from_slice(&request.page.to_be_bytes());
    frame.extend_from_slice(&request.width.to_be_bytes());
    frame.extend_from_slice(&request.height.to_be_bytes());
    frame.extend_from_slice(&pdf_len.to_be_bytes());
    frame.extend_from_slice(&request.source_hash);
    frame.extend_from_slice(&identity);
    debug_assert_eq!(frame.len(), REQUEST_HEADER_LEN);
    frame.extend_from_slice(&request.pdf);
    Ok(frame)
}

/// Decodes and verifies one complete schema-2 request frame for an adapter.
///
/// The frame must fit `max_frame_bytes`, contain exactly the declared PDF
/// length, have zero reserved bits, and bind the embedded bytes to the supplied
/// source digest. PDF content is never included in diagnostics.
pub fn decode_adapter_request(
    mut frame: Vec<u8>,
    max_frame_bytes: u64,
) -> Result<AdapterRequest, BaselineError> {
    if u64::try_from(frame.len()).map_err(|_| request_limit())? > max_frame_bytes {
        return Err(request_limit());
    }
    if frame.len() < REQUEST_HEADER_LEN
        || &frame[..8] != REQUEST_MAGIC
        || read_request_u16(&frame, 8)? != SCHEMA_VERSION
        || read_request_u16(&frame, 10)? != 0
    {
        return Err(invalid_request());
    }

    let page = read_request_u32(&frame, 12)?;
    let width = read_request_u32(&frame, 16)?;
    let height = read_request_u32(&frame, 20)?;
    expected_rgba_len(width, height)?;
    let pdf_len = usize::try_from(read_request_u64(&frame, 24)?).map_err(|_| invalid_request())?;
    let expected_len = REQUEST_HEADER_LEN
        .checked_add(pdf_len)
        .ok_or_else(invalid_request)?;
    if frame.len() != expected_len {
        return Err(invalid_request());
    }

    let source_hash: [u8; 32] = frame[32..64]
        .try_into()
        .expect("fixed request source identity is 32 bytes");
    let descriptor_identity: [u8; 32] = frame[64..96]
        .try_into()
        .expect("fixed request descriptor identity is 32 bytes");
    let pdf = frame.split_off(REQUEST_HEADER_LEN);
    if sha256(&pdf).map_err(|_| invalid_request())? != source_hash {
        return Err(BaselineError::new(
            BaselineErrorCode::SourceHashMismatch,
            "RPE-BASELINE-0002",
            "PDF bytes do not match the fixed corpus identity",
        ));
    }

    Ok(AdapterRequest {
        source_hash,
        descriptor_identity,
        pdf,
        page,
        width,
        height,
    })
}

/// Encodes a successful schema-2 adapter response bound to its request.
///
/// Produced RGBA must have exactly `width * height * 4` bytes. Unsupported and
/// failed channels are encoded without payloads, and the complete frame must
/// fit `max_frame_bytes`.
pub fn encode_adapter_response(
    request: &AdapterRequest,
    channels: AdapterResponseChannels<'_>,
    max_frame_bytes: u64,
) -> Result<Vec<u8>, BaselineError> {
    encode_adapter_frame(request, 0, channels, max_frame_bytes)
}

/// Encodes an identity-bound terminal adapter failure.
///
/// A terminal failure marks every channel failed and carries no document data.
pub fn encode_adapter_failure(
    request: &AdapterRequest,
    max_frame_bytes: u64,
) -> Result<Vec<u8>, BaselineError> {
    encode_adapter_frame(
        request,
        1,
        AdapterResponseChannels::new(
            BaselineChannel::Failed,
            BaselineChannel::Failed,
            BaselineChannel::Failed,
            BaselineChannel::Failed,
        ),
        max_frame_bytes,
    )
}

fn encode_adapter_frame(
    request: &AdapterRequest,
    outcome: u16,
    channels: AdapterResponseChannels<'_>,
    max_frame_bytes: u64,
) -> Result<Vec<u8>, BaselineError> {
    let (parse_status, parse) = adapter_channel_parts(channels.parse_json)?;
    let (scene_status, scene) = adapter_channel_parts(channels.scene_json)?;
    let (text_status, text) = adapter_channel_parts(channels.text_json)?;
    let (pixel_status, rgba) = adapter_channel_parts(channels.rgba)?;
    if outcome == 1
        && [parse_status, scene_status, text_status, pixel_status]
            .into_iter()
            .any(|status| status != WireChannelStatus::Failed)
    {
        return Err(invalid_request());
    }
    if !pixel_length_is_valid(pixel_status, rgba.len(), request.width, request.height)? {
        return Err(invalid_request());
    }

    let payload_len = parse
        .len()
        .checked_add(scene.len())
        .and_then(|value| value.checked_add(text.len()))
        .and_then(|value| value.checked_add(rgba.len()))
        .ok_or_else(output_limit)?;
    let frame_len = RESPONSE_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(output_limit)?;
    if u64::try_from(frame_len).map_err(|_| output_limit())? > max_frame_bytes {
        return Err(output_limit());
    }

    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_len)
        .map_err(|_| output_limit())?;
    frame.extend_from_slice(RESPONSE_MAGIC);
    frame.extend_from_slice(&SCHEMA_VERSION.to_be_bytes());
    frame.extend_from_slice(&outcome.to_be_bytes());
    frame.extend_from_slice(&[
        adapter_status_byte(parse_status),
        adapter_status_byte(scene_status),
        adapter_status_byte(text_status),
        adapter_status_byte(pixel_status),
    ]);
    frame.extend_from_slice(
        &u32::try_from(parse.len())
            .map_err(|_| output_limit())?
            .to_be_bytes(),
    );
    frame.extend_from_slice(
        &u32::try_from(scene.len())
            .map_err(|_| output_limit())?
            .to_be_bytes(),
    );
    frame.extend_from_slice(
        &u32::try_from(text.len())
            .map_err(|_| output_limit())?
            .to_be_bytes(),
    );
    frame.extend_from_slice(&request.page.to_be_bytes());
    frame.extend_from_slice(&request.width.to_be_bytes());
    frame.extend_from_slice(&request.height.to_be_bytes());
    frame.extend_from_slice(
        &u64::try_from(rgba.len())
            .map_err(|_| output_limit())?
            .to_be_bytes(),
    );
    frame.extend_from_slice(&request.source_hash);
    frame.extend_from_slice(&request.descriptor_identity);
    debug_assert_eq!(frame.len(), RESPONSE_HEADER_LEN);
    frame.extend_from_slice(parse);
    frame.extend_from_slice(scene);
    frame.extend_from_slice(text);
    frame.extend_from_slice(rgba);
    Ok(frame)
}

fn adapter_channel_parts(
    channel: BaselineChannel<&[u8]>,
) -> Result<(WireChannelStatus, &[u8]), BaselineError> {
    match channel {
        BaselineChannel::Produced(value) if !value.is_empty() => {
            Ok((WireChannelStatus::Produced, value))
        }
        BaselineChannel::Produced(_) => Err(invalid_request()),
        BaselineChannel::Unsupported => Ok((WireChannelStatus::Unsupported, &[])),
        BaselineChannel::Failed => Ok((WireChannelStatus::Failed, &[])),
    }
}

const fn adapter_status_byte(status: WireChannelStatus) -> u8 {
    match status {
        WireChannelStatus::Produced => 0,
        WireChannelStatus::Unsupported => 1,
        WireChannelStatus::Failed => 2,
    }
}

/// Decodes a bounded response and verifies source, request geometry, and build identity.
pub fn decode_response(
    response: &[u8],
    request: &BaselineRequest,
    descriptor: BaselineDescriptor,
    max_output_bytes: u64,
) -> Result<BaselineObservation, BaselineError> {
    if u64::try_from(response.len()).map_err(|_| output_limit())? > max_output_bytes {
        return Err(output_limit());
    }
    if response.len() < RESPONSE_HEADER_LEN || &response[..8] != RESPONSE_MAGIC {
        return Err(malformed_response());
    }
    let schema = read_u16(response, 8)?;
    let outcome = read_u16(response, 10)?;
    if schema != SCHEMA_VERSION || outcome > 1 {
        return Err(malformed_response());
    }

    let parse_status = read_channel_status(response, 12)?;
    let scene_status = read_channel_status(response, 13)?;
    let text_status = read_channel_status(response, 14)?;
    let pixel_status = read_channel_status(response, 15)?;
    let parse_len = usize::try_from(read_u32(response, 16)?).map_err(|_| malformed_response())?;
    let scene_len = usize::try_from(read_u32(response, 20)?).map_err(|_| malformed_response())?;
    let text_len = usize::try_from(read_u32(response, 24)?).map_err(|_| malformed_response())?;
    let page = read_u32(response, 28)?;
    let width = read_u32(response, 32)?;
    let height = read_u32(response, 36)?;
    let rgba_len = usize::try_from(read_u64(response, 40)?).map_err(|_| malformed_response())?;
    let source_hash: [u8; 32] = response[48..80]
        .try_into()
        .expect("fixed response header source identity is 32 bytes");
    let response_identity: [u8; 32] = response[80..112]
        .try_into()
        .expect("fixed response header descriptor identity is 32 bytes");
    let expected_identity = descriptor_identity(&descriptor)?;
    if source_hash != request.source_hash
        || response_identity != expected_identity
        || page != request.page
        || width != request.width
        || height != request.height
    {
        return Err(BaselineError::new(
            BaselineErrorCode::IdentityMismatch,
            "RPE-BASELINE-0007",
            "response identity does not match the request and descriptor",
        ));
    }

    if !channel_length_is_valid(parse_status, parse_len)
        || !channel_length_is_valid(scene_status, scene_len)
        || !channel_length_is_valid(text_status, text_len)
        || !pixel_length_is_valid(pixel_status, rgba_len, width, height)?
    {
        return Err(malformed_response());
    }

    let payload_len = parse_len
        .checked_add(scene_len)
        .and_then(|value| value.checked_add(text_len))
        .and_then(|value| value.checked_add(rgba_len))
        .ok_or_else(malformed_response)?;
    let expected_len = RESPONSE_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(malformed_response)?;
    if response.len() != expected_len {
        return Err(malformed_response());
    }

    if outcome == 1 {
        if [parse_status, scene_status, text_status, pixel_status]
            .into_iter()
            .any(|status| status != WireChannelStatus::Failed)
        {
            return Err(malformed_response());
        }
        return Err(runner_failed());
    }

    let mut cursor = RESPONSE_HEADER_LEN;
    let parse_json = decode_channel(response, &mut cursor, parse_status, parse_len)?;
    let scene_json = decode_channel(response, &mut cursor, scene_status, scene_len)?;
    let text_json = decode_channel(response, &mut cursor, text_status, text_len)?;
    let rgba = decode_channel(response, &mut cursor, pixel_status, rgba_len)?;
    Ok(BaselineObservation {
        descriptor,
        source_hash,
        page,
        parse_json,
        scene_json,
        text_json,
        width,
        height,
        rgba,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WireChannelStatus {
    Produced,
    Unsupported,
    Failed,
}

fn read_channel_status(bytes: &[u8], offset: usize) -> Result<WireChannelStatus, BaselineError> {
    match bytes.get(offset).copied() {
        Some(0) => Ok(WireChannelStatus::Produced),
        Some(1) => Ok(WireChannelStatus::Unsupported),
        Some(2) => Ok(WireChannelStatus::Failed),
        _ => Err(malformed_response()),
    }
}

fn channel_length_is_valid(status: WireChannelStatus, length: usize) -> bool {
    match status {
        WireChannelStatus::Produced => length != 0,
        WireChannelStatus::Unsupported | WireChannelStatus::Failed => length == 0,
    }
}

fn pixel_length_is_valid(
    status: WireChannelStatus,
    length: usize,
    width: u32,
    height: u32,
) -> Result<bool, BaselineError> {
    Ok(match status {
        WireChannelStatus::Produced => {
            length == expected_rgba_len(width, height).map_err(|_| malformed_response())?
        }
        WireChannelStatus::Unsupported | WireChannelStatus::Failed => length == 0,
    })
}

fn decode_channel(
    bytes: &[u8],
    cursor: &mut usize,
    status: WireChannelStatus,
    length: usize,
) -> Result<BaselineChannel<Vec<u8>>, BaselineError> {
    Ok(match status {
        WireChannelStatus::Produced => BaselineChannel::Produced(copy_blob(bytes, cursor, length)?),
        WireChannelStatus::Unsupported => BaselineChannel::Unsupported,
        WireChannelStatus::Failed => BaselineChannel::Failed,
    })
}

fn expected_rgba_len(width: u32, height: u32) -> Result<usize, BaselineError> {
    if width == 0 || height == 0 {
        return Err(invalid_request());
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(invalid_request)?;
    usize::try_from(bytes).map_err(|_| invalid_request())
}

fn read_request_u16(bytes: &[u8], offset: usize) -> Result<u16, BaselineError> {
    let value = bytes.get(offset..offset + 2).ok_or_else(invalid_request)?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

fn read_request_u32(bytes: &[u8], offset: usize) -> Result<u32, BaselineError> {
    let value = bytes.get(offset..offset + 4).ok_or_else(invalid_request)?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_request_u64(bytes: &[u8], offset: usize) -> Result<u64, BaselineError> {
    let value = bytes.get(offset..offset + 8).ok_or_else(invalid_request)?;
    Ok(u64::from_be_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, BaselineError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(malformed_response)?;
    Ok(u16::from_be_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, BaselineError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(malformed_response)?;
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, BaselineError> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(malformed_response)?;
    Ok(u64::from_be_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn copy_blob(bytes: &[u8], cursor: &mut usize, len: usize) -> Result<Vec<u8>, BaselineError> {
    let end = cursor.checked_add(len).ok_or_else(malformed_response)?;
    let source = bytes.get(*cursor..end).ok_or_else(malformed_response)?;
    let mut output = Vec::new();
    output.try_reserve_exact(len).map_err(|_| output_limit())?;
    output.extend_from_slice(source);
    *cursor = end;
    Ok(output)
}

pub(crate) fn invalid_request() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::InvalidRequest,
        "RPE-BASELINE-0001",
        "request hash, length, or geometry is invalid",
    )
}

pub(crate) fn output_limit() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::OutputLimit,
        "RPE-BASELINE-0004",
        "baseline response exceeds its configured output limit",
    )
}

pub(crate) fn malformed_response() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::MalformedResponse,
        "RPE-BASELINE-0005",
        "baseline response frame is malformed",
    )
}

pub(crate) fn runner_failed() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::RunnerFailed,
        "RPE-BASELINE-0006",
        "baseline reported an unsuccessful observation",
    )
}

pub(crate) fn request_limit() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::RequestLimit,
        "RPE-BASELINE-0013",
        "baseline request exceeds its configured input limit",
    )
}

pub(crate) fn invalid_process_config() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::InvalidProcessConfig,
        "RPE-BASELINE-0008",
        "baseline process configuration or invocation identity is invalid",
    )
}

pub(crate) fn process_spawn_failed() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::ProcessSpawnFailed,
        "RPE-BASELINE-0009",
        "baseline direct child could not be started",
    )
}

pub(crate) fn transport_failed() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::TransportFailed,
        "RPE-BASELINE-0010",
        "baseline process transport failed",
    )
}

pub(crate) fn watchdog_expired() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::WatchdogExpired,
        "RPE-BASELINE-0011",
        "baseline direct child exceeded its watchdog",
    )
}

pub(crate) fn containment_failed() -> BaselineError {
    BaselineError::new(
        BaselineErrorCode::ContainmentFailed,
        "RPE-BASELINE-0012",
        "baseline process containment or pipe cleanup could not be proven",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> BaselineDescriptor {
        BaselineDescriptor {
            id: "fixture-v1".into(),
            engine: "fixture".into(),
            upstream_revision: "test".into(),
            build_hash: [7; 32],
            build_flags_hash: [8; 32],
            environment_hash: [9; 32],
            invocation_hash: [13; 32],
            license_manifest_hash: [10; 32],
            fonts_hash: [11; 32],
            color_hash: [12; 32],
            platform: "test-platform".into(),
        }
    }

    fn request() -> BaselineRequest {
        let pdf = b"%PDF-1.7".to_vec();
        BaselineRequest::new(sha256(&pdf).unwrap(), pdf, 2, 1, 1).unwrap()
    }

    fn response(
        outcome: u16,
        channel_statuses: [u8; 4],
        request: &BaselineRequest,
        descriptor: &BaselineDescriptor,
        payloads: [&[u8]; 4],
    ) -> Vec<u8> {
        let [parse, scene, text, rgba] = payloads;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(RESPONSE_MAGIC);
        bytes.extend_from_slice(&SCHEMA_VERSION.to_be_bytes());
        bytes.extend_from_slice(&outcome.to_be_bytes());
        bytes.extend_from_slice(&channel_statuses);
        bytes.extend_from_slice(&u32::try_from(parse.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(scene.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&u32::try_from(text.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&request.page.to_be_bytes());
        bytes.extend_from_slice(&request.width.to_be_bytes());
        bytes.extend_from_slice(&request.height.to_be_bytes());
        bytes.extend_from_slice(&u64::try_from(rgba.len()).unwrap().to_be_bytes());
        bytes.extend_from_slice(&request.source_hash);
        bytes.extend_from_slice(&descriptor_identity(descriptor).unwrap());
        bytes.extend_from_slice(parse);
        bytes.extend_from_slice(scene);
        bytes.extend_from_slice(text);
        bytes.extend_from_slice(rgba);
        bytes
    }

    #[test]
    fn request_frame_is_deterministic_and_identity_bound() {
        let request = request();
        let descriptor = descriptor();
        let first = encode_request(&request, &descriptor).unwrap();
        let second = encode_request(&request, &descriptor).unwrap();
        assert_eq!(first, second);
        assert_eq!(&first[..8], REQUEST_MAGIC);
        assert_eq!(first.len(), REQUEST_HEADER_LEN + request.pdf().len());
        assert_eq!(&first[32..64], &request.source_hash);
        assert_eq!(&first[64..96], &descriptor_identity(&descriptor).unwrap());

        let mut changed_environment = descriptor.clone();
        changed_environment.environment_hash[0] ^= 1;
        assert_ne!(
            first,
            encode_request(&request, &changed_environment).unwrap()
        );

        let mut changed_invocation = descriptor.clone();
        changed_invocation.invocation_hash[0] ^= 1;
        assert_ne!(
            first,
            encode_request(&request, &changed_invocation).unwrap()
        );
    }

    #[test]
    fn adapter_codec_round_trips_identity_and_partial_channels() {
        let request = request();
        let baseline_descriptor = descriptor();
        let frame = encode_request(&request, &baseline_descriptor).unwrap();
        let adapter = decode_adapter_request(frame.clone(), 1024).unwrap();
        assert_eq!(adapter.source_hash(), request.source_hash());
        assert_eq!(
            adapter.descriptor_identity(),
            descriptor_identity(&baseline_descriptor).unwrap()
        );
        assert_eq!(adapter.pdf(), request.pdf());
        assert_eq!(adapter.page(), request.page());
        assert_eq!(adapter.width(), request.width());
        assert_eq!(adapter.height(), request.height());

        let response = encode_adapter_response(
            &adapter,
            AdapterResponseChannels::new(
                BaselineChannel::Unsupported,
                BaselineChannel::Failed,
                BaselineChannel::Produced(b"[]"),
                BaselineChannel::Produced(&[1, 2, 3, 4]),
            ),
            1024,
        )
        .unwrap();
        let observation =
            decode_response(&response, &request, baseline_descriptor.clone(), 1024).unwrap();
        assert_eq!(observation.parse_json, BaselineChannel::Unsupported);
        assert_eq!(observation.scene_json, BaselineChannel::Failed);
        assert_eq!(
            observation.text_json,
            BaselineChannel::Produced(b"[]".to_vec())
        );
        assert_eq!(
            observation.rgba,
            BaselineChannel::Produced(vec![1, 2, 3, 4])
        );

        let failure = encode_adapter_failure(&adapter, 1024).unwrap();
        assert_eq!(
            decode_response(&failure, &request, baseline_descriptor, 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::RunnerFailed
        );
    }

    #[test]
    fn adapter_request_decoder_rejects_noncanonical_or_unbound_frames() {
        let request = request();
        let descriptor = descriptor();
        let frame = encode_request(&request, &descriptor).unwrap();

        assert_eq!(
            decode_adapter_request(frame.clone(), u64::try_from(frame.len() - 1).unwrap())
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::RequestLimit
        );
        for changed in [
            {
                let mut changed = frame.clone();
                changed[10] = 1;
                changed
            },
            {
                let mut changed = frame.clone();
                changed.extend_from_slice(b"trailing");
                changed
            },
            frame[..frame.len() - 1].to_vec(),
        ] {
            assert_eq!(
                decode_adapter_request(changed, 1024).err().unwrap().code,
                BaselineErrorCode::InvalidRequest
            );
        }

        let mut changed_pdf = frame;
        let last = changed_pdf.len() - 1;
        changed_pdf[last] ^= 1;
        assert_eq!(
            decode_adapter_request(changed_pdf, 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::SourceHashMismatch
        );
    }

    #[test]
    fn adapter_response_encoder_rejects_placeholders_geometry_and_limits() {
        let request = request();
        let descriptor = descriptor();
        let frame = encode_request(&request, &descriptor).unwrap();
        let adapter = decode_adapter_request(frame, 1024).unwrap();

        for rgba in [&[][..], &[0, 1, 2][..], &[0; 8][..]] {
            assert_eq!(
                encode_adapter_response(
                    &adapter,
                    AdapterResponseChannels::new(
                        BaselineChannel::Unsupported,
                        BaselineChannel::Unsupported,
                        BaselineChannel::Unsupported,
                        BaselineChannel::Produced(rgba),
                    ),
                    1024,
                )
                .unwrap_err()
                .code,
                BaselineErrorCode::InvalidRequest
            );
        }
        assert_eq!(
            encode_adapter_response(
                &adapter,
                AdapterResponseChannels::new(
                    BaselineChannel::Produced(b""),
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Produced(&[0; 4]),
                ),
                1024,
            )
            .unwrap_err()
            .code,
            BaselineErrorCode::InvalidRequest
        );
        assert_eq!(
            encode_adapter_response(
                &adapter,
                AdapterResponseChannels::new(
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Unsupported,
                    BaselineChannel::Produced(&[0; 4]),
                ),
                115,
            )
            .unwrap_err()
            .code,
            BaselineErrorCode::OutputLimit
        );
    }

    #[test]
    fn request_rejects_source_hash_mismatch_and_invalid_geometry() {
        let mismatch = BaselineRequest::new([0; 32], b"not-empty".to_vec(), 0, 1, 1)
            .err()
            .unwrap();
        assert_eq!(mismatch.code, BaselineErrorCode::SourceHashMismatch);
        let empty = Vec::new();
        let invalid = BaselineRequest::new(sha256(&empty).unwrap(), empty, 0, 0, 1)
            .err()
            .unwrap();
        assert_eq!(invalid.code, BaselineErrorCode::InvalidRequest);
    }

    #[test]
    fn error_codes_have_stable_category_and_recovery_policy() {
        let request_error = invalid_request();
        assert_eq!(request_error.category, BaselineErrorCategory::Request);
        assert_eq!(
            request_error.recoverability,
            BaselineRecoverability::CorrectRequest
        );

        let transport_error = transport_failed();
        assert_eq!(transport_error.category, BaselineErrorCategory::Process);
        assert_eq!(
            transport_error.recoverability,
            BaselineRecoverability::RetryFreshProcess
        );

        let containment_error = containment_failed();
        assert_eq!(
            containment_error.category,
            BaselineErrorCategory::Containment
        );
        assert_eq!(
            containment_error.recoverability,
            BaselineRecoverability::CorrectConfiguration
        );
    }

    #[test]
    fn decodes_success_as_request_bound_o4_observation() {
        let request = request();
        let descriptor = descriptor();
        let bytes = response(
            0,
            [0; 4],
            &request,
            &descriptor,
            [b"{}", b"[]", b"[]", &[1, 2, 3, 4]],
        );
        let observation = decode_response(&bytes, &request, descriptor.clone(), 1024).unwrap();
        assert_eq!(observation.authority(), OracleAuthority::O4Observation);
        assert_eq!(observation.source_hash, request.source_hash);
        assert_eq!(observation.page, request.page);
        assert_eq!(observation.descriptor, descriptor);
        assert_eq!(
            observation.parse_json,
            BaselineChannel::Produced(b"{}".to_vec())
        );
        assert_eq!(
            observation.rgba,
            BaselineChannel::Produced(vec![1, 2, 3, 4])
        );

        let partial = response(
            0,
            [1, 2, 0, 0],
            &request,
            &descriptor,
            [b"", b"", b"[]", &[1, 2, 3, 4]],
        );
        let observation = decode_response(&partial, &request, descriptor, 1024).unwrap();
        assert_eq!(observation.parse_json, BaselineChannel::Unsupported);
        assert_eq!(observation.scene_json, BaselineChannel::Failed);
        assert_eq!(
            observation.text_json,
            BaselineChannel::Produced(b"[]".to_vec())
        );
    }

    #[test]
    fn rejects_malformed_failed_oversized_and_mismatched_responses() {
        let request = request();
        let descriptor = descriptor();
        let valid = response(
            0,
            [0; 4],
            &request,
            &descriptor,
            [b"{}", b"[]", b"[]", &[0; 4]],
        );
        for bytes in [valid[..valid.len() - 1].to_vec(), {
            let mut bad = valid.clone();
            bad[0] = b'X';
            bad
        }] {
            assert_eq!(
                decode_response(&bytes, &request, descriptor.clone(), 1024)
                    .err()
                    .unwrap()
                    .code,
                BaselineErrorCode::MalformedResponse
            );
        }
        let failed = response(1, [2; 4], &request, &descriptor, [b"", b"", b"", b""]);
        assert_eq!(
            decode_response(&failed, &request, descriptor.clone(), 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::RunnerFailed
        );
        assert_eq!(
            decode_response(&valid, &request, descriptor.clone(), 1)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::OutputLimit
        );
        let mut identity = valid.clone();
        identity[48] ^= 1;
        assert_eq!(
            decode_response(&identity, &request, descriptor.clone(), 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::IdentityMismatch
        );
        let mut wrong_page = valid;
        wrong_page[31] ^= 1;
        assert_eq!(
            decode_response(&wrong_page, &request, descriptor.clone(), 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::IdentityMismatch
        );

        let mut failed_identity = failed.clone();
        failed_identity[48] ^= 1;
        assert_eq!(
            decode_response(&failed_identity, &request, descriptor.clone(), 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::IdentityMismatch
        );
        let malformed_channel = response(
            0,
            [1, 0, 0, 0],
            &request,
            &descriptor,
            [b"{}", b"[]", b"[]", &[0; 4]],
        );
        assert_eq!(
            decode_response(&malformed_channel, &request, descriptor, 1024)
                .err()
                .unwrap()
                .code,
            BaselineErrorCode::MalformedResponse
        );
    }
}
