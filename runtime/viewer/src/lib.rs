//! Proof-backed, fully Native PDF.rs document viewing composition.
//!
//! This crate is intentionally UI- and transport-neutral. A Rust-native UI can
//! call it directly; process bridges such as the local Electron development
//! shell may expose the same bounded interface without moving PDF parsing or
//! rendering into the UI process.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pdf_rs_bytes::{
    ByteRange, DataTicket, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_content::{
    ContentCancellation, ContentErrorCategory, ContentExtGStateAcquisitionProfile,
    ContentExtGStateJobContext, ContentFontLimits, ContentFontProfile, ContentFormProfile,
    ContentGraphicsLimits, ContentImageLimits, ContentImageProfile, ContentLimits,
    ContentVmErrorCategory, ContentVmFailure, ContentVmLimits, ContentVmPoll, InterpretPageJob,
};
use pdf_rs_document::{
    AcquiredObjectJobContext, AcquiredPageCountPoll, AttestRevisionJob, CandidateRevisionIndex,
    DocumentCancellation, DocumentError, DocumentErrorCategory, DocumentLimits,
    FontResourceJobContext, FontResourceLimits, FormXObjectJobContext, ImageXObjectJobContext,
    ImageXObjectLimits, NeverCancelSourceRevisionChain, NeverCancelled, OpenSourceRevisionChainJob,
    OpenStrictBaseRevisionJob, PageContentJobContext, PageContentLimits, PageContentPoll,
    PageExtGStateLookupLimits, PageFontLookupLimits, PageIndex, PageIndexBuildPoll,
    PageIndexLimits, PageLookupPoll, PageMaterializationJobContext, PageMaterializationLimits,
    PageMaterializationPoll, PagePropertyLookupLimits, PageTreeJobContext, PageTreeLimits,
    PageXObjectLookupLimits, RevisionAttestationJobContext, RevisionAttestationLimits,
    RevisionAttestationPoll, RevisionId, SharedAttestedRevisionIndex, SourceAcquiredDocument,
    SourceAcquiredDocumentLimits, SourceAcquiredRevisionChain, SourceRevisionChainError,
    SourceRevisionChainErrorCategory, SourceRevisionChainJobContext, SourceRevisionChainLimits,
    SourceRevisionChainPoll, StrictBaseOpenContext, StrictBaseOpenError, StrictBaseOpenLimits,
    StrictBaseOpenPoll,
};
use pdf_rs_fast_raster::fast::{
    FastRasterCancellation, FastRasterErrorCategory, FastRasterJob, FastRasterLimits,
};
use pdf_rs_filters::DecodeLimits;
use pdf_rs_object::ObjectLimits;
use pdf_rs_policy::{
    CapabilityEvaluator, CapabilityProfile, DeviceRect, OptionalContentIdentity,
    PolicyCancellation, PolicyErrorCategory, PolicyLimits, RenderConfig, RenderConfigInput,
    RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio, create_render_plan,
};
use pdf_rs_raster::reference::{
    ReferenceRasterCancellation, ReferenceRasterLimits, ReferenceRenderConfig,
    ReferenceRenderErrorCategory, ReferenceRenderJob, ReferenceRenderPoll,
};
use pdf_rs_scene::{GraphicsSceneLimits, PageRotation, Scene, SceneRect, SceneScalar};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{
    RevisionLimits, XrefAnchorLimits, XrefErrorCategory, XrefJobContext, XrefLimits,
    XrefStreamLimits,
};

const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OUTPUT_WIDTH: u32 = 4_096;
const MAX_OUTPUT_HEIGHT: u32 = 8_192;
const MAX_PENDING_TURNS: usize = 4_096;

/// Stable, content-free Native viewer failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeViewerErrorCode {
    /// A caller-supplied source, page, or output dimension is invalid.
    InvalidInput,
    /// The immutable source could not be supplied exactly to a bounded job.
    Source,
    /// Strict document opening, page lookup, or materialization failed.
    Document,
    /// Content interpretation failed.
    Content,
    /// The page is outside the currently registered Native graphics profile.
    Unsupported,
    /// Native pixel production failed.
    Render,
    /// Cooperative cancellation stopped work before publication.
    Cancelled,
    /// A configured source, work, or output ceiling was exceeded.
    ResourceLimit,
    /// An internal lifecycle invariant failed.
    Internal,
}

/// Content-free Native viewer failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeViewerError {
    code: NativeViewerErrorCode,
}

impl NativeViewerError {
    const fn new(code: NativeViewerErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure category.
    pub const fn code(self) -> NativeViewerErrorCode {
        self.code
    }
}

impl fmt::Display for NativeViewerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Native viewer failure ({:?})", self.code)
    }
}

impl std::error::Error for NativeViewerError {}

/// Cooperative cancellation shared by UI-neutral Native viewer operations.
pub trait NativeViewerCancellation: Send + Sync {
    /// Reports whether the current result is no longer useful.
    fn is_cancelled(&self) -> bool;
}

impl NativeViewerCancellation for AtomicBool {
    fn is_cancelled(&self) -> bool {
        self.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NeverNativeViewerCancellation;

impl NativeViewerCancellation for NeverNativeViewerCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct CancellationAdapter<'a>(&'a dyn NativeViewerCancellation);

impl ContentCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl DocumentCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl FastRasterCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl PolicyCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl ReferenceRasterCancellation for CancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn document_failure(error: DocumentError) -> NativeViewerError {
    let code = match error.category() {
        DocumentErrorCategory::Syntax | DocumentErrorCategory::Lookup => {
            NativeViewerErrorCode::Document
        }
        DocumentErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
        DocumentErrorCategory::Source => NativeViewerErrorCode::Source,
        DocumentErrorCategory::Unsupported => NativeViewerErrorCode::Unsupported,
        DocumentErrorCategory::Cancellation => NativeViewerErrorCode::Cancelled,
        DocumentErrorCategory::Configuration | DocumentErrorCategory::Internal => {
            NativeViewerErrorCode::Internal
        }
    };
    NativeViewerError::new(code)
}

fn strict_open_failure(error: StrictBaseOpenError) -> NativeViewerError {
    match error {
        StrictBaseOpenError::Document(error) => document_failure(error),
        StrictBaseOpenError::Xref(error) => {
            let code = match error.category() {
                XrefErrorCategory::Syntax => NativeViewerErrorCode::Document,
                XrefErrorCategory::Source => NativeViewerErrorCode::Source,
                XrefErrorCategory::Unsupported => NativeViewerErrorCode::Unsupported,
                XrefErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
                XrefErrorCategory::Cancellation => NativeViewerErrorCode::Cancelled,
                XrefErrorCategory::Configuration | XrefErrorCategory::Internal => {
                    NativeViewerErrorCode::Internal
                }
            };
            NativeViewerError::new(code)
        }
    }
}

fn source_chain_failure(error: SourceRevisionChainError) -> NativeViewerError {
    let code = match error.category() {
        SourceRevisionChainErrorCategory::Syntax => NativeViewerErrorCode::Document,
        SourceRevisionChainErrorCategory::Source => NativeViewerErrorCode::Source,
        SourceRevisionChainErrorCategory::Unsupported => NativeViewerErrorCode::Unsupported,
        SourceRevisionChainErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
        SourceRevisionChainErrorCategory::Cancellation => NativeViewerErrorCode::Cancelled,
        SourceRevisionChainErrorCategory::Configuration
        | SourceRevisionChainErrorCategory::Internal => NativeViewerErrorCode::Internal,
    };
    NativeViewerError::new(code)
}

/// Native raster implementation that produced a complete viewer surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRendererKind {
    /// Independently reviewed PDF.rs Reference CPU rasterizer.
    ReferenceCpu,
    /// Product-tiled PDF.rs Fast CPU rasterizer.
    FastCpu,
}

impl NativeRendererKind {
    /// Returns the stable renderer identifier exposed across UI adapters.
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::ReferenceCpu => "reference-cpu-v1",
            Self::FastCpu => "fast-cpu-v1",
        }
    }
}

/// Complete immutable top-down straight-alpha sRGB RGBA8 page result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePageSurface {
    page_index: u32,
    renderer: NativeRendererKind,
    width: u32,
    height: u32,
    stride: u32,
    pixels: Vec<u8>,
}

impl NativePageSurface {
    /// Returns the zero-based rendered page index.
    pub const fn page_index(&self) -> u32 {
        self.page_index
    }

    /// Returns the Native raster implementation that produced the pixels.
    pub const fn renderer(&self) -> NativeRendererKind {
        self.renderer
    }

    /// Returns the output width in device pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the output height in device pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the exact top-down RGBA8 row stride.
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Borrows the complete immutable RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the surface and returns its complete RGBA8 pixels.
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

/// One opened immutable PDF source backed entirely by PDF.rs Native components.
pub struct NativeDocument {
    source: Arc<Vec<u8>>,
    snapshot: SourceSnapshot,
    authority: NativeDocumentAuthority,
    page_count: u32,
    next_job: u64,
}

#[allow(
    clippy::large_enum_variant,
    reason = "move-only acquired source proofs remain inline so their accounted ownership is not hidden behind an untracked allocation"
)]
enum NativeDocumentAuthority {
    Strict {
        authority: SharedAttestedRevisionIndex,
        page_index: PageIndex,
    },
    Acquired {
        _authority: SourceAcquiredDocument,
    },
}

impl NativeDocument {
    /// Opens an immutable local PDF and publishes its validated logical page count.
    pub fn open(source: Vec<u8>) -> Result<Self, NativeViewerError> {
        let source_len = u64::try_from(source.len())
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
        if source_len == 0 || source_len > MAX_SOURCE_BYTES {
            return Err(NativeViewerError::new(NativeViewerErrorCode::ResourceLimit));
        }
        let snapshot = source_snapshot(&source, source_len);
        let source = Arc::new(source);
        let (authority, page_count) = match open_strict_document(snapshot, &source) {
            Ok((authority, page_index)) => {
                let page_count = page_index.len();
                (
                    NativeDocumentAuthority::Strict {
                        authority,
                        page_index,
                    },
                    page_count,
                )
            }
            Err(error)
                if matches!(
                    error.code(),
                    NativeViewerErrorCode::Unsupported | NativeViewerErrorCode::Document
                ) =>
            {
                match open_acquired_document(snapshot, &source) {
                    Ok((authority, page_count)) => (authority, page_count),
                    Err(_) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        if page_count == 0 {
            return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
        }
        Ok(Self {
            source,
            snapshot,
            authority,
            page_count,
            next_job: 10,
        })
    }

    /// Returns the validated logical page count.
    pub const fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Interprets and renders one page at the requested output width.
    ///
    /// Height is derived from the page crop box and intrinsic page rotation.
    pub fn render_page(
        &mut self,
        page_index: u32,
        width: u32,
    ) -> Result<NativePageSurface, NativeViewerError> {
        self.render_page_with_renderer_and_cancellation(
            page_index,
            width,
            NativeRendererKind::ReferenceCpu,
            &NeverNativeViewerCancellation,
        )
    }

    /// Interprets and renders one page with an explicitly selected PDF.rs Native renderer.
    ///
    /// This qualification boundary keeps the Reference and Fast CPU implementations observable
    /// without changing the default renderer before the Fast profile completes its CANARY gate.
    pub fn render_page_with_renderer(
        &mut self,
        page_index: u32,
        width: u32,
        renderer: NativeRendererKind,
    ) -> Result<NativePageSurface, NativeViewerError> {
        self.render_page_with_renderer_and_cancellation(
            page_index,
            width,
            renderer,
            &NeverNativeViewerCancellation,
        )
    }

    /// Interprets and renders one page while observing cooperative cancellation.
    ///
    /// Cancellation is checked before job ownership changes and at the bounded probe intervals
    /// owned by document, content, policy, and raster jobs. No Surface is published after
    /// cancellation is observed.
    pub fn render_page_with_renderer_and_cancellation(
        &mut self,
        page_index: u32,
        width: u32,
        renderer: NativeRendererKind,
        cancellation: &dyn NativeViewerCancellation,
    ) -> Result<NativePageSurface, NativeViewerError> {
        if page_index >= self.page_count() || width == 0 || width > MAX_OUTPUT_WIDTH {
            return Err(NativeViewerError::new(NativeViewerErrorCode::InvalidInput));
        }
        if cancellation.is_cancelled() {
            return Err(NativeViewerError::new(NativeViewerErrorCode::Cancelled));
        }
        let ids = self.allocate_render_jobs()?;
        let snapshot = self.snapshot;
        let source = Arc::clone(&self.source);
        let (authority, page_index_state) = match &mut self.authority {
            NativeDocumentAuthority::Strict {
                authority,
                page_index,
            } => (authority, page_index),
            NativeDocumentAuthority::Acquired { .. } => {
                return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
            }
        };
        render_strict_page(
            authority,
            page_index_state,
            snapshot,
            &source,
            page_index,
            width,
            renderer,
            ids,
            cancellation,
        )
    }

    fn allocate_render_jobs(&mut self) -> Result<RenderJobs, NativeViewerError> {
        let base = self.next_job;
        self.next_job = self
            .next_job
            .checked_add(10_000)
            .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
        Ok(RenderJobs {
            base,
            lookup: JobId::new(base + 1),
            materialize: JobId::new(base + 2),
            content: JobId::new(base + 3),
            image: JobId::new(base + 4),
            font: JobId::new(base + 5),
            form: JobId::new(base + 6),
        })
    }
}

fn open_strict_document(
    snapshot: SourceSnapshot,
    source: &[u8],
) -> Result<(SharedAttestedRevisionIndex, PageIndex), NativeViewerError> {
    let open_job = JobId::new(1);
    let mut open = OpenStrictBaseRevisionJob::new(
        snapshot,
        RevisionId::new(1),
        StrictBaseOpenContext::new(
            XrefJobContext::new(
                open_job,
                ResumeCheckpoint::new(1_001),
                ResumeCheckpoint::new(1_002),
            ),
            RevisionAttestationJobContext::new(
                open_job,
                ResumeCheckpoint::new(1_003),
                ResumeCheckpoint::new(1_004),
                ResumeCheckpoint::new(1_005),
                RequestPriority::VisiblePage,
            ),
        ),
        StrictBaseOpenLimits::new(
            XrefLimits::default(),
            pdf_rs_document::DocumentLimits::default(),
            RevisionAttestationLimits::default(),
            ObjectLimits::default(),
            SyntaxLimits::default(),
        ),
    )
    .map_err(strict_open_failure)?;
    let open_store = range_store(snapshot)?;
    let authority = loop {
        match open.poll(&open_store, &NeverCancelled) {
            StrictBaseOpenPoll::Ready(authority) => break authority.into_shared(),
            StrictBaseOpenPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &open_store,
                snapshot,
                source,
                open_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            StrictBaseOpenPoll::Failed(error) => return Err(strict_open_failure(error)),
        }
    };

    let build_job = JobId::new(2);
    let tree_limits = PageTreeLimits::default();
    let mut build = authority
        .build_page_index_owned(
            page_tree_context(build_job, 2_100),
            tree_limits,
            PageIndexLimits::default(),
        )
        .map_err(document_failure)?;
    let build_store = range_store(snapshot)?;
    let page_index = loop {
        match build.poll(&build_store, &NeverCancelled) {
            PageIndexBuildPoll::Ready(index) => break index,
            PageIndexBuildPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &build_store,
                snapshot,
                source,
                build_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            PageIndexBuildPoll::Failed(error) => return Err(document_failure(error)),
        }
    };
    if page_index.is_empty() {
        return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
    }
    Ok((authority, page_index))
}

fn open_acquired_document(
    snapshot: SourceSnapshot,
    source: &[u8],
) -> Result<(NativeDocumentAuthority, u32), NativeViewerError> {
    let open_job = JobId::new(1);
    let mut open = OpenSourceRevisionChainJob::new_with_decode_limits(
        snapshot,
        SourceRevisionChainJobContext::new(
            open_job,
            ResumeCheckpoint::new(1_001),
            ResumeCheckpoint::new(1_002),
            ResumeCheckpoint::new(1_003),
            ResumeCheckpoint::new(1_004),
            ResumeCheckpoint::new(1_005),
            ResumeCheckpoint::new(1_006),
        ),
        SourceRevisionChainLimits::default(),
        XrefLimits::default(),
        XrefAnchorLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
        XrefStreamLimits::default(),
        DecodeLimits::default(),
        RevisionLimits::default(),
    )
    .map_err(source_chain_failure)?;
    let open_store = range_store(snapshot)?;
    let acquisition = loop {
        match open.poll(&open_store, &NeverCancelSourceRevisionChain) {
            SourceRevisionChainPoll::Ready(acquisition) => break acquisition,
            SourceRevisionChainPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &open_store,
                snapshot,
                source,
                open_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            SourceRevisionChainPoll::Failed(error) => return Err(source_chain_failure(error)),
        }
    };
    if let Ok((authority, page_index)) =
        attest_single_stream_document(snapshot, source, &acquisition)
    {
        let page_count = page_index.len();
        return Ok((
            NativeDocumentAuthority::Strict {
                authority,
                page_index,
            },
            page_count,
        ));
    }
    let authority = SourceAcquiredDocument::new(
        acquisition,
        SourceAcquiredDocumentLimits::default(),
        &NeverCancelled,
    )
    .map_err(document_failure)?;
    let count_job = JobId::new(2);
    let mut count = authority
        .count_acquired_pages(
            AcquiredObjectJobContext::new(
                count_job,
                ResumeCheckpoint::new(2_001),
                ResumeCheckpoint::new(2_002),
                ResumeCheckpoint::new(2_003),
                ResumeCheckpoint::new(2_004),
                ResumeCheckpoint::new(2_005),
                RequestPriority::VisiblePage,
            ),
            PageTreeLimits::default(),
        )
        .map_err(document_failure)?;
    let count_store = range_store(snapshot)?;
    let page_count = loop {
        match count.poll(&count_store, &NeverCancelled) {
            AcquiredPageCountPoll::Ready(result) => break result.page_count(),
            AcquiredPageCountPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &count_store,
                snapshot,
                source,
                count_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            AcquiredPageCountPoll::Failed(error) => return Err(document_failure(error)),
        }
    };
    let page_count = u32::try_from(page_count)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    Ok((
        NativeDocumentAuthority::Acquired {
            _authority: authority,
        },
        page_count,
    ))
}

fn attest_single_stream_document(
    snapshot: SourceSnapshot,
    source: &[u8],
    acquisition: &SourceAcquiredRevisionChain,
) -> Result<(SharedAttestedRevisionIndex, PageIndex), NativeViewerError> {
    let candidate = CandidateRevisionIndex::from_single_stream_revision(
        acquisition,
        RevisionId::new(1),
        DocumentLimits::default(),
        &NeverCancelled,
    )
    .map_err(document_failure)?;
    let attest_job = JobId::new(2);
    let mut attest = AttestRevisionJob::new(
        candidate,
        RevisionAttestationJobContext::new(
            attest_job,
            ResumeCheckpoint::new(2_001),
            ResumeCheckpoint::new(2_002),
            ResumeCheckpoint::new(2_003),
            RequestPriority::Metadata,
        ),
        RevisionAttestationLimits::default(),
        ObjectLimits::default(),
        SyntaxLimits::default(),
    )
    .map_err(document_failure)?;
    let attest_store = range_store(snapshot)?;
    let authority = loop {
        match attest.poll(&attest_store, &NeverCancelled) {
            RevisionAttestationPoll::Ready(authority) => break authority.into_shared(),
            RevisionAttestationPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &attest_store,
                snapshot,
                source,
                attest_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            RevisionAttestationPoll::Failed(error) => return Err(document_failure(error)),
        }
    };

    let build_job = JobId::new(3);
    let mut build = authority
        .build_page_index_owned(
            page_tree_context(build_job, 3_100),
            PageTreeLimits::default(),
            PageIndexLimits::default(),
        )
        .map_err(document_failure)?;
    let build_store = range_store(snapshot)?;
    let page_index = loop {
        match build.poll(&build_store, &NeverCancelled) {
            PageIndexBuildPoll::Ready(index) => break index,
            PageIndexBuildPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &build_store,
                snapshot,
                source,
                build_job,
                ticket,
                &missing,
                checkpoint,
            )?,
            PageIndexBuildPoll::Failed(error) => return Err(document_failure(error)),
        }
    };
    if page_index.is_empty() {
        return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
    }
    Ok((authority, page_index))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the strict rendering branch receives explicit immutable source and job ownership"
)]
fn render_strict_page(
    authority: &SharedAttestedRevisionIndex,
    page_index_state: &mut PageIndex,
    snapshot: SourceSnapshot,
    source: &[u8],
    page_index: u32,
    width: u32,
    renderer: NativeRendererKind,
    ids: RenderJobs,
    cancellation: &dyn NativeViewerCancellation,
) -> Result<NativePageSurface, NativeViewerError> {
    let cancellation = CancellationAdapter(cancellation);
    let tree_limits = PageTreeLimits::default();
    let mut lookup = authority
        .lookup_page_owned(
            page_index_state,
            page_index,
            page_tree_context(ids.lookup, ids.base + 100),
            tree_limits,
        )
        .map_err(document_failure)?;
    let lookup_store = range_store(snapshot)?;
    let lookup = loop {
        match lookup.poll(&lookup_store, &cancellation) {
            PageLookupPoll::Ready(lookup) => break lookup,
            PageLookupPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &lookup_store,
                snapshot,
                source,
                ids.lookup,
                ticket,
                &missing,
                checkpoint,
            )?,
            PageLookupPoll::Failed(error) => return Err(document_failure(error)),
        }
    };
    let (refined_index, handle) = lookup.into_parts();
    *page_index_state = refined_index;

    let mut materialize = authority
        .materialize_page_owned(
            page_index_state,
            handle,
            PageMaterializationJobContext::new(
                ids.materialize,
                ResumeCheckpoint::new(ids.base + 201),
                ResumeCheckpoint::new(ids.base + 202),
                RequestPriority::VisiblePage,
            ),
            PageMaterializationLimits::default(),
        )
        .map_err(document_failure)?;
    let materialize_store = range_store(snapshot)?;
    let page = loop {
        match materialize.poll(&materialize_store, &cancellation) {
            PageMaterializationPoll::Ready(page) => break page,
            PageMaterializationPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &materialize_store,
                snapshot,
                source,
                ids.materialize,
                ticket,
                &missing,
                checkpoint,
            )?,
            PageMaterializationPoll::Failed(error) => return Err(document_failure(error)),
        }
    };

    let mut content = authority
        .acquire_page_content_owned(
            page_index_state,
            page,
            PageContentJobContext::new(
                ids.content,
                ResumeCheckpoint::new(ids.base + 301),
                ResumeCheckpoint::new(ids.base + 302),
                ResumeCheckpoint::new(ids.base + 303),
                RequestPriority::VisiblePage,
            ),
            PageContentLimits::default(),
        )
        .map_err(document_failure)?;
    let content_store = range_store(snapshot)?;
    let acquired = loop {
        match content.poll(&content_store, &cancellation) {
            PageContentPoll::Ready(acquired) => break acquired,
            PageContentPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => complete_pending(
                &content_store,
                snapshot,
                source,
                ids.content,
                ticket,
                &missing,
                checkpoint,
            )?,
            PageContentPoll::Failed(error) => return Err(document_failure(error)),
        }
    };

    let image_profile = ContentImageProfile::new(
        authority.clone(),
        PageXObjectLookupLimits::default(),
        ImageXObjectJobContext::new(
            ids.image,
            ResumeCheckpoint::new(ids.base + 401),
            ResumeCheckpoint::new(ids.base + 402),
            ResumeCheckpoint::new(ids.base + 403),
            RequestPriority::FirstViewportResource,
        ),
        ImageXObjectLimits::default(),
        ContentImageLimits::default(),
    );
    let font_profile = ContentFontProfile::new(
        authority.clone(),
        PageFontLookupLimits::default(),
        FontResourceJobContext::new(
            ids.font,
            ResumeCheckpoint::new(ids.base + 501),
            ResumeCheckpoint::new(ids.base + 502),
            ResumeCheckpoint::new(ids.base + 503),
            ResumeCheckpoint::new(ids.base + 504),
            ResumeCheckpoint::new(ids.base + 505),
            ResumeCheckpoint::new(ids.base + 506),
            ResumeCheckpoint::new(ids.base + 507),
            RequestPriority::FirstViewportResource,
        ),
        FontResourceLimits::default(),
        ContentFontLimits::default(),
    );
    let ext_gstate_profile = ContentExtGStateAcquisitionProfile::new(
        authority.clone(),
        PageExtGStateLookupLimits::default(),
        ContentExtGStateJobContext::new(
            ids.content,
            ResumeCheckpoint::new(ids.base + 700),
            RequestPriority::FirstViewportResource,
        ),
    );
    let form_profile = ContentFormProfile::new(
        authority.clone(),
        FormXObjectJobContext::new(
            ids.form,
            ResumeCheckpoint::new(ids.base + 9_001),
            ResumeCheckpoint::new(ids.base + 9_002),
            ResumeCheckpoint::new(ids.base + 9_003),
            RequestPriority::FirstViewportResource,
        ),
        64,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile.clone(),
        font_profile.clone(),
        GraphicsSceneLimits::default(),
    )
    .and_then(|profile| profile.with_ext_gstates(ext_gstate_profile.clone()))
    .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Content))?;
    let mut vm = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
        acquired,
        ContentLimits::default(),
        ContentVmLimits::default(),
        ContentGraphicsLimits::default(),
        PagePropertyLookupLimits::default(),
        image_profile,
        font_profile,
        GraphicsSceneLimits::default(),
    )
    .with_dynamic_ext_gstates(ext_gstate_profile)
    .with_forms(form_profile)
    .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Content))?;
    let vm_store = range_store(snapshot)?;
    let interpreted = loop {
        match vm.poll(&vm_store, &cancellation) {
            ContentVmPoll::Ready(page) => break page,
            ContentVmPoll::Unsupported(_) => {
                return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
            }
            ContentVmPoll::Failed(error) => {
                let code = match error {
                    ContentVmFailure::Content(error) => match error.category() {
                        ContentErrorCategory::Cancellation => NativeViewerErrorCode::Cancelled,
                        ContentErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
                        _ => NativeViewerErrorCode::Content,
                    },
                    ContentVmFailure::Document(error) => return Err(document_failure(error)),
                    ContentVmFailure::Scene(_) => NativeViewerErrorCode::Content,
                    ContentVmFailure::Vm(error) => match error.category() {
                        ContentVmErrorCategory::Cancellation => NativeViewerErrorCode::Cancelled,
                        ContentVmErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
                        _ => NativeViewerErrorCode::Content,
                    },
                };
                return Err(NativeViewerError::new(code));
            }
            ContentVmPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => {
                let job = vm_pending_job(checkpoint, ids);
                complete_pending(
                    &vm_store, snapshot, source, job, ticket, &missing, checkpoint,
                )?;
            }
        }
    };

    let scene = interpreted.scene_arc();
    let height = output_height(
        scene.geometry().crop_box(),
        scene.geometry().rotation(),
        width,
    )?;
    match renderer {
        NativeRendererKind::ReferenceCpu => {
            let config = ReferenceRenderConfig::opaque_srgb(width, height)
                .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::InvalidInput))?;
            let mut render =
                ReferenceRenderJob::new(scene, config, ReferenceRasterLimits::default());
            let pixels = match render.poll(&cancellation) {
                ReferenceRenderPoll::Ready(pixels) => pixels,
                ReferenceRenderPoll::Unsupported(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
                }
                ReferenceRenderPoll::Failed(error) => {
                    let code = match error.category() {
                        ReferenceRenderErrorCategory::Cancellation => {
                            NativeViewerErrorCode::Cancelled
                        }
                        ReferenceRenderErrorCategory::Resource => {
                            NativeViewerErrorCode::ResourceLimit
                        }
                        _ => NativeViewerErrorCode::Render,
                    };
                    return Err(NativeViewerError::new(code));
                }
            };
            let stride = u32::try_from(pixels.stride_bytes())
                .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
            Ok(NativePageSurface {
                page_index,
                renderer,
                width: pixels.width(),
                height: pixels.height(),
                stride,
                pixels: pixels.rgba().to_vec(),
            })
        }
        NativeRendererKind::FastCpu => {
            render_fast_page(page_index, width, height, &scene, &cancellation)
        }
    }
}

#[derive(Clone, Copy)]
struct RenderJobs {
    base: u64,
    lookup: JobId,
    materialize: JobId,
    content: JobId,
    image: JobId,
    font: JobId,
    form: JobId,
}

fn source_snapshot(source: &[u8], source_len: u64) -> SourceSnapshot {
    let mut stable = [0_u8; 32];
    for (index, byte) in source.iter().copied().enumerate() {
        let slot = index % stable.len();
        stable[slot] = stable[slot]
            .rotate_left(1)
            .wrapping_add(byte)
            .wrapping_add(u8::try_from(index % 251).expect("modulo fits u8"));
    }
    let mut validator = stable;
    validator.reverse();
    SourceSnapshot::new(
        SourceIdentity::new(SourceStableId::new(stable), SourceRevision::new(1)),
        Some(source_len),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, validator),
    )
}

fn range_store(snapshot: SourceSnapshot) -> Result<RangeStore, NativeViewerError> {
    RangeStore::new(snapshot, pdf_rs_bytes::RangeStoreLimits::default())
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))
}

fn page_tree_context(job: JobId, seed: u64) -> PageTreeJobContext {
    PageTreeJobContext::new(
        job,
        ResumeCheckpoint::new(seed + 1),
        ResumeCheckpoint::new(seed + 2),
        RequestPriority::VisiblePage,
    )
}

fn complete_pending(
    store: &RangeStore,
    snapshot: SourceSnapshot,
    source: &[u8],
    expected_job: JobId,
    ticket: DataTicket,
    missing: &SmallRanges,
    checkpoint: ResumeCheckpoint,
) -> Result<(), NativeViewerError> {
    if missing.is_empty() || missing.as_slice().len() > MAX_PENDING_TURNS {
        return Err(NativeViewerError::new(NativeViewerErrorCode::Internal));
    }
    for range in missing.as_slice() {
        supply_range(store, snapshot, source, *range)?;
    }
    let subscriptions = store
        .take_subscriptions(ticket)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))?;
    if subscriptions.len() != 1
        || subscriptions[0].job() != expected_job
        || subscriptions[0].checkpoint() != checkpoint
    {
        return Err(NativeViewerError::new(NativeViewerErrorCode::Internal));
    }
    store
        .release_ticket(ticket)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))
}

fn supply_range(
    store: &RangeStore,
    snapshot: SourceSnapshot,
    source: &[u8],
    range: ByteRange,
) -> Result<(), NativeViewerError> {
    let start = usize::try_from(range.start())
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))?;
    let end = usize::try_from(range.end_exclusive())
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))?;
    let bytes = source
        .get(start..end)
        .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Source))?
        .to_vec();
    let response = RangeResponse::new(snapshot, range, bytes)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))?;
    store
        .supply(response)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Source))?;
    Ok(())
}

fn vm_pending_job(checkpoint: ResumeCheckpoint, jobs: RenderJobs) -> JobId {
    let local = checkpoint.value().saturating_sub(jobs.base);
    if (401..=403).contains(&local) {
        jobs.image
    } else if (501..=507).contains(&local) {
        jobs.font
    } else if (9_001..=9_003).contains(&local) {
        jobs.form
    } else {
        jobs.content
    }
}

fn output_height(
    crop: SceneRect,
    rotation: PageRotation,
    width: u32,
) -> Result<u32, NativeViewerError> {
    let page_width = crop
        .width()
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?
        .scaled();
    let page_height = crop
        .height()
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?
        .scaled();
    let (page_width, page_height) = match rotation {
        PageRotation::Degrees0 | PageRotation::Degrees180 => (page_width, page_height),
        PageRotation::Degrees90 | PageRotation::Degrees270 => (page_height, page_width),
    };
    let page_width = u64::try_from(page_width)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
    let page_height = u64::try_from(page_height)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
    let height = u64::from(width)
        .checked_mul(page_height)
        .and_then(|value| value.checked_add(page_width / 2))
        .and_then(|value| value.checked_div(page_width))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0 && *value <= MAX_OUTPUT_HEIGHT)
        .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    Ok(height)
}

fn render_fast_page(
    page_index: u32,
    width: u32,
    height: u32,
    scene: &Scene,
    cancellation: &CancellationAdapter<'_>,
) -> Result<NativePageSurface, NativeViewerError> {
    let decision =
        CapabilityEvaluator::new(CapabilityProfile::m4_fast_v1(), PolicyLimits::default())
            .evaluate(scene, 1, cancellation)
            .map_err(|error| {
                let code = match error.category() {
                    PolicyErrorCategory::Cancelled => NativeViewerErrorCode::Cancelled,
                    PolicyErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
                    _ => NativeViewerErrorCode::Render,
                };
                NativeViewerError::new(code)
            })?;
    let config = RenderConfig::validate(RenderConfigInput::fast_cpu_full())
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Render))?;
    let request = RenderPlanRequest::new(
        1,
        DeviceRect::new(0, 0, width, height)
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::InvalidInput))?,
        output_zoom(
            scene.geometry().crop_box(),
            scene.geometry().rotation(),
            width,
        )?,
        1_000,
        PageRotation::Degrees0,
        OptionalContentIdentity::new(1),
        1,
    )
    .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::InvalidInput))?;
    let plan = match create_render_plan(
        scene,
        decision,
        config,
        request,
        RendererEpoch::new(1).expect("fixed Fast renderer epoch is nonzero"),
        PolicyLimits::default(),
        cancellation,
    )
    .map_err(|error| {
        let code = match error.category() {
            PolicyErrorCategory::Cancelled => NativeViewerErrorCode::Cancelled,
            PolicyErrorCategory::Resource => NativeViewerErrorCode::ResourceLimit,
            _ => NativeViewerErrorCode::Render,
        };
        NativeViewerError::new(code)
    })? {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(_) => {
            return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
        }
    };
    let order = (0..plan.tiles().len())
        .map(|ordinal| {
            u32::try_from(ordinal)
                .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let job = FastRasterJob::new(scene, &plan, FastRasterLimits::default(), cancellation).map_err(
        |error| {
            let code = match error.category() {
                FastRasterErrorCategory::Cancelled => NativeViewerErrorCode::Cancelled,
                FastRasterErrorCategory::ResourceLimit => NativeViewerErrorCode::ResourceLimit,
                _ => NativeViewerErrorCode::Render,
            };
            NativeViewerError::new(code)
        },
    )?;
    let tiles = job.render_all(&order, cancellation).map_err(|error| {
        let code = match error.category() {
            FastRasterErrorCategory::Cancelled => NativeViewerErrorCode::Cancelled,
            FastRasterErrorCategory::ResourceLimit => NativeViewerErrorCode::ResourceLimit,
            _ => NativeViewerErrorCode::Render,
        };
        NativeViewerError::new(code)
    })?;
    let stride = width
        .checked_mul(4)
        .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    let pixel_len = u64::from(stride)
        .checked_mul(u64::from(height))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(pixel_len)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    pixels.resize(pixel_len, 0);
    for tile in tiles.tiles() {
        let rect = tile.identity().content_key().tile();
        let x = u32::try_from(rect.x())
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
        let y = u32::try_from(rect.y())
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
        let row_bytes = rect
            .width()
            .checked_mul(4)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
        for row in 0..rect.height() {
            let source_start = u64::from(row)
                .checked_mul(u64::from(tile.stride()))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
            let target_start = u64::from(
                y.checked_add(row)
                    .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Internal))?,
            )
            .checked_mul(u64::from(stride))
            .and_then(|value| {
                u64::from(x)
                    .checked_mul(4)
                    .and_then(|offset| value.checked_add(offset))
            })
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
            let source = tile
                .pixels()
                .get(source_start..source_start + row_bytes)
                .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
            let target = pixels
                .get_mut(target_start..target_start + row_bytes)
                .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
            target.copy_from_slice(source);
        }
    }
    Ok(NativePageSurface {
        page_index,
        renderer: NativeRendererKind::FastCpu,
        width,
        height,
        stride,
        pixels,
    })
}

fn output_zoom(
    crop: SceneRect,
    rotation: PageRotation,
    width: u32,
) -> Result<ZoomRatio, NativeViewerError> {
    let page_width = match rotation {
        PageRotation::Degrees0 | PageRotation::Degrees180 => crop.width(),
        PageRotation::Degrees90 | PageRotation::Degrees270 => crop.height(),
    }
    .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?
    .scaled();
    let page_width = u64::try_from(page_width)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
    let scene_one = u64::try_from(SceneScalar::ONE.scaled())
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Internal))?;
    let numerator = u64::from(width)
        .checked_mul(scene_one)
        .ok_or_else(|| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    let divisor = gcd(numerator, page_width);
    let numerator = u32::try_from(numerator / divisor)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    let denominator = u32::try_from(page_width / divisor)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
    ZoomRatio::new(numerator, denominator)
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::InvalidInput))
}

const fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}
