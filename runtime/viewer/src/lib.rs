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

use pdf_rs_bytes::{
    ByteRange, DataTicket, JobId, RangeResponse, RangeStore, RequestPriority, ResumeCheckpoint,
    SmallRanges, SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
    SourceValidatorKind,
};
use pdf_rs_content::{
    ContentFontLimits, ContentFontProfile, ContentGraphicsLimits, ContentImageLimits,
    ContentImageProfile, ContentLimits, ContentVmLimits, ContentVmPoll, InterpretPageJob,
};
use pdf_rs_document::{
    FontResourceJobContext, FontResourceLimits, ImageXObjectJobContext, ImageXObjectLimits,
    NeverCancelled, OpenStrictBaseRevisionJob, PageContentJobContext, PageContentLimits,
    PageContentPoll, PageFontLookupLimits, PageIndex, PageIndexBuildPoll, PageIndexLimits,
    PageLookupPoll, PageMaterializationJobContext, PageMaterializationLimits,
    PageMaterializationPoll, PagePropertyLookupLimits, PageTreeJobContext, PageTreeLimits,
    PageXObjectLookupLimits, RevisionAttestationJobContext, RevisionAttestationLimits, RevisionId,
    SharedAttestedRevisionIndex, StrictBaseOpenContext, StrictBaseOpenLimits, StrictBaseOpenPoll,
};
use pdf_rs_object::ObjectLimits;
use pdf_rs_raster::reference::{
    ReferenceRasterCancellation, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderJob,
    ReferenceRenderPoll,
};
use pdf_rs_scene::{GraphicsSceneLimits, PageRotation, SceneRect};
use pdf_rs_syntax::SyntaxLimits;
use pdf_rs_xref::{XrefJobContext, XrefLimits};

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

/// Native raster implementation that produced a complete viewer surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeRendererKind {
    /// Independently reviewed PDF.rs Reference CPU rasterizer.
    ReferenceCpu,
}

impl NativeRendererKind {
    /// Returns the stable renderer identifier exposed across UI adapters.
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::ReferenceCpu => "reference-cpu-v1",
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
    authority: SharedAttestedRevisionIndex,
    page_index: PageIndex,
    next_job: u64,
}

impl NativeDocument {
    /// Strictly opens an immutable local PDF and publishes its page index.
    pub fn open(source: Vec<u8>) -> Result<Self, NativeViewerError> {
        let source_len = u64::try_from(source.len())
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
        if source_len == 0 || source_len > MAX_SOURCE_BYTES {
            return Err(NativeViewerError::new(NativeViewerErrorCode::ResourceLimit));
        }
        let snapshot = source_snapshot(&source, source_len);
        let source = Arc::new(source);
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
        .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
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
                    &source,
                    open_job,
                    ticket,
                    &missing,
                    checkpoint,
                )?,
                StrictBaseOpenPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
                }
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
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
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
                    &source,
                    build_job,
                    ticket,
                    &missing,
                    checkpoint,
                )?,
                PageIndexBuildPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
                }
            }
        };
        if page_index.is_empty() {
            return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
        }
        Ok(Self {
            source,
            snapshot,
            authority,
            page_index,
            next_job: 10,
        })
    }

    /// Returns the strict document's declared logical page count.
    pub const fn page_count(&self) -> u32 {
        self.page_index.len()
    }

    /// Interprets and renders one page at the requested output width.
    ///
    /// Height is derived from the page crop box and intrinsic page rotation.
    pub fn render_page(
        &mut self,
        page_index: u32,
        width: u32,
    ) -> Result<NativePageSurface, NativeViewerError> {
        if page_index >= self.page_count() || width == 0 || width > MAX_OUTPUT_WIDTH {
            return Err(NativeViewerError::new(NativeViewerErrorCode::InvalidInput));
        }
        let ids = self.allocate_render_jobs()?;
        let tree_limits = PageTreeLimits::default();
        let mut lookup = self
            .authority
            .lookup_page_owned(
                &self.page_index,
                page_index,
                page_tree_context(ids.lookup, ids.base + 100),
                tree_limits,
            )
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
        let lookup_store = range_store(self.snapshot)?;
        let lookup = loop {
            match lookup.poll(&lookup_store, &NeverCancelled) {
                PageLookupPoll::Ready(lookup) => break lookup,
                PageLookupPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => complete_pending(
                    &lookup_store,
                    self.snapshot,
                    &self.source,
                    ids.lookup,
                    ticket,
                    &missing,
                    checkpoint,
                )?,
                PageLookupPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
                }
            }
        };
        let (refined_index, handle) = lookup.into_parts();
        self.page_index = refined_index;

        let mut materialize = self
            .authority
            .materialize_page_owned(
                &self.page_index,
                handle,
                PageMaterializationJobContext::new(
                    ids.materialize,
                    ResumeCheckpoint::new(ids.base + 201),
                    ResumeCheckpoint::new(ids.base + 202),
                    RequestPriority::VisiblePage,
                ),
                PageMaterializationLimits::default(),
            )
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
        let materialize_store = range_store(self.snapshot)?;
        let page = loop {
            match materialize.poll(&materialize_store, &NeverCancelled) {
                PageMaterializationPoll::Ready(page) => break page,
                PageMaterializationPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => complete_pending(
                    &materialize_store,
                    self.snapshot,
                    &self.source,
                    ids.materialize,
                    ticket,
                    &missing,
                    checkpoint,
                )?,
                PageMaterializationPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
                }
            }
        };

        let mut content = self
            .authority
            .acquire_page_content_owned(
                &self.page_index,
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
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::Document))?;
        let content_store = range_store(self.snapshot)?;
        let acquired = loop {
            match content.poll(&content_store, &NeverCancelled) {
                PageContentPoll::Ready(acquired) => break acquired,
                PageContentPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => complete_pending(
                    &content_store,
                    self.snapshot,
                    &self.source,
                    ids.content,
                    ticket,
                    &missing,
                    checkpoint,
                )?,
                PageContentPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Document));
                }
            }
        };

        let image_profile = ContentImageProfile::new(
            self.authority.clone(),
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
            self.authority.clone(),
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
        let mut vm = InterpretPageJob::new_graphics_v2_with_images_and_fonts(
            acquired,
            ContentLimits::default(),
            ContentVmLimits::default(),
            ContentGraphicsLimits::default(),
            PagePropertyLookupLimits::default(),
            image_profile,
            font_profile,
            GraphicsSceneLimits::default(),
        );
        let vm_store = range_store(self.snapshot)?;
        let interpreted = loop {
            match vm.poll(&vm_store, &NeverCancelled) {
                ContentVmPoll::Ready(page) => break page,
                ContentVmPoll::Unsupported(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
                }
                ContentVmPoll::Failed(_) => {
                    return Err(NativeViewerError::new(NativeViewerErrorCode::Content));
                }
                ContentVmPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    let job = vm_pending_job(checkpoint, ids);
                    complete_pending(
                        &vm_store,
                        self.snapshot,
                        &self.source,
                        job,
                        ticket,
                        &missing,
                        checkpoint,
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
        let config = ReferenceRenderConfig::opaque_srgb(width, height)
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::InvalidInput))?;
        let mut render = ReferenceRenderJob::new(scene, config, ReferenceRasterLimits::default());
        let pixels = match render.poll(&NeverRasterCancelled) {
            ReferenceRenderPoll::Ready(pixels) => pixels,
            ReferenceRenderPoll::Unsupported(_) => {
                return Err(NativeViewerError::new(NativeViewerErrorCode::Unsupported));
            }
            ReferenceRenderPoll::Failed(_) => {
                return Err(NativeViewerError::new(NativeViewerErrorCode::Render));
            }
        };
        let stride = u32::try_from(pixels.stride_bytes())
            .map_err(|_| NativeViewerError::new(NativeViewerErrorCode::ResourceLimit))?;
        Ok(NativePageSurface {
            page_index,
            renderer: NativeRendererKind::ReferenceCpu,
            width: pixels.width(),
            height: pixels.height(),
            stride,
            pixels: pixels.rgba().to_vec(),
        })
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
        })
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
}

struct NeverRasterCancelled;

impl ReferenceRasterCancellation for NeverRasterCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
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
