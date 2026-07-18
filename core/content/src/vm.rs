use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_document::{
    AcquiredFormXObject, AcquiredPageContent, DocumentCancellation, FontResourceJobContext,
    FontResourceLimits, FormXObjectJobContext, ImageXObjectJobContext, ImageXObjectLimits,
    PageFontLookupLimits, PageFontLookupOutcome, PageFontLookupStats, PagePropertyLookupLimits,
    PagePropertyLookupStats, PageResourceScope, PageXObjectLookupLimits, PageXObjectLookupOutcome,
    PageXObjectLookupStats, SharedAttestedRevisionIndex,
};
use pdf_rs_font::{GlyphId, OutlineSegment};
use pdf_rs_scene::{
    CommandSource, DashPattern, DashPatternBuilder, FillRule, GlyphOutline, GlyphUse,
    GraphicsSceneBuilder, GraphicsSceneLimits, LineStyle, Matrix, PageGeometry,
    PageRotation as ScenePageRotation, Paint, PathResource, PathResourceBuilder, PathSegment,
    Scene, SceneBinding, SceneBounds, SceneBuilder, SceneError, SceneLimits, ScenePoint, SceneRect,
    SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

use crate::scanner::{ScanTerminal, run_scan};
use crate::{
    ContentCancellation, ContentExtGStateProfile, ContentFontLimits, ContentFontStats,
    ContentGraphicsLimits, ContentImageLimits, ContentImageStats, ContentLimits, ContentName,
    ContentNumber, ContentOperand, ContentOperatorSource, ContentProgram, ContentScanStats,
    ContentUnsupported, ContentUnsupportedKind, ContentVmError, ContentVmErrorCode,
    ContentVmFailure, ContentVmLimit, ContentVmLimitKind, ContentVmLimits, ContentVmPhase,
    ContentVmStats, DecodedContentStream, InterpretedForm, InterpretedPage, LocatedOperand,
    OperatorContext, OperatorKind, OperatorOperandShape, ResolvedFontUse, ResolvedFormUse,
    ResolvedImageUse, ResolvedPropertyUse,
};

mod font;
mod graphics;
mod image;

use font::{FontPlanningPoll, FontRuntime};
use graphics::{DashRetentionAdmission, GraphicsExecutionError, GraphicsVm, VmRetention};
use image::{ImagePlanningPoll, ImageRuntime, ResolvedXObject};

const DASH_CANCELLATION_INTERVAL: usize = 256;
const MAX_FORM_RECURSION_DEPTH: u16 = 64;

/// One replayable sealed Page-interpretation outcome.
#[derive(Clone)]
pub enum ContentVmPoll {
    /// Complete immutable interpreted Page.
    Ready(Arc<InterpretedPage>),
    /// Validated feature outside the bounded initial VM profile.
    Unsupported(ContentUnsupported),
    /// One proof-bound Image XObject or embedded Font object/payload requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the request.
        missing: SmallRanges,
        /// Image checkpoint or Font/descriptor/program envelope, boundary, or payload checkpoint.
        checkpoint: ResumeCheckpoint,
    },
    /// Terminal lower-layer or VM failure.
    Failed(ContentVmFailure),
}

impl fmt::Debug for ContentVmPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(page) => formatter
                .debug_tuple("Ready")
                .field(&page.acquired_content().handle())
                .finish(),
            Self::Unsupported(error) => formatter.debug_tuple("Unsupported").field(error).finish(),
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

/// One replayable sealed Form-XObject interpretation outcome.
#[derive(Clone)]
pub enum ContentFormPoll {
    /// Complete immutable interpreted Form.
    Ready(Arc<InterpretedForm>),
    /// Validated feature outside the bounded Form profile.
    Unsupported(ContentUnsupported),
    /// One proof-bound child resource requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the request.
        missing: SmallRanges,
        /// Child acquisition checkpoint.
        checkpoint: ResumeCheckpoint,
    },
    /// Terminal lower-layer or VM failure.
    Failed(ContentVmFailure),
}

impl fmt::Debug for ContentFormPoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(form) => formatter
                .debug_tuple("Ready")
                .field(&form.acquired_form().reference())
                .finish(),
            Self::Unsupported(error) => formatter.debug_tuple("Unsupported").field(error).finish(),
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

enum JobState {
    Pending,
    Ready(Arc<InterpretedPage>),
    Unsupported(ContentUnsupported),
    Failed(ContentVmFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentVmProfile {
    SceneV1 {
        scene_limits: SceneLimits,
    },
    GraphicsV2 {
        graphics_limits: ContentGraphicsLimits,
        scene_limits: GraphicsSceneLimits,
    },
}

/// Proof authority, child runtime context, and independent limits for Image XObject execution.
#[derive(Clone, Debug)]
pub struct ContentImageProfile {
    authority: SharedAttestedRevisionIndex,
    lookup_limits: PageXObjectLookupLimits,
    context: ImageXObjectJobContext,
    acquisition_limits: ImageXObjectLimits,
    content_limits: ContentImageLimits,
}

/// Proof authority, child runtime context, and independent limits for embedded-font execution.
#[derive(Clone, Debug)]
pub struct ContentFontProfile {
    authority: SharedAttestedRevisionIndex,
    lookup_limits: PageFontLookupLimits,
    context: FontResourceJobContext,
    acquisition_limits: FontResourceLimits,
    content_limits: ContentFontLimits,
}

/// Proof authority, child profiles, and bounded recursion context for Form XObject execution.
#[derive(Clone, Debug)]
pub struct ContentFormProfile {
    authority: SharedAttestedRevisionIndex,
    context: FormXObjectJobContext,
    remaining_depth: u16,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    graphics_limits: ContentGraphicsLimits,
    property_limits: PagePropertyLookupLimits,
    image_profile: ContentImageProfile,
    font_profile: ContentFontProfile,
    scene_limits: GraphicsSceneLimits,
}

impl ContentFormProfile {
    /// Creates one explicit recursively bounded Form execution profile.
    #[allow(
        clippy::too_many_arguments,
        reason = "Form recursion keeps each proof authority and independently validated child limit explicit"
    )]
    pub fn new(
        authority: SharedAttestedRevisionIndex,
        context: FormXObjectJobContext,
        max_depth: u16,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        image_profile: ContentImageProfile,
        font_profile: ContentFontProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Result<Self, ContentVmError> {
        let expected = authority.as_attested();
        let image = image_profile.authority().as_attested();
        let font = font_profile.authority().as_attested();
        if max_depth == 0
            || max_depth > MAX_FORM_RECURSION_DEPTH
            || expected.snapshot() != image.snapshot()
            || expected.snapshot() != font.snapshot()
            || expected.revision_id() != image.revision_id()
            || expected.revision_id() != font.revision_id()
            || expected.startxref() != image.startxref()
            || expected.startxref() != font.startxref()
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            authority,
            context,
            remaining_depth: max_depth,
            scan_limits,
            vm_limits,
            graphics_limits,
            property_limits,
            image_profile,
            font_profile,
            scene_limits,
        })
    }

    fn child(&self) -> Option<Self> {
        let remaining_depth = self.remaining_depth.checked_sub(1)?;
        (remaining_depth != 0).then(|| Self {
            authority: self.authority.clone(),
            context: self.context,
            remaining_depth,
            scan_limits: self.scan_limits,
            vm_limits: self.vm_limits,
            graphics_limits: self.graphics_limits,
            property_limits: self.property_limits,
            image_profile: self.image_profile.clone(),
            font_profile: self.font_profile.clone(),
            scene_limits: self.scene_limits,
        })
    }
}

impl ContentFontProfile {
    /// Creates an explicit proof-bound embedded simple TrueType profile.
    pub const fn new(
        authority: SharedAttestedRevisionIndex,
        lookup_limits: PageFontLookupLimits,
        context: FontResourceJobContext,
        acquisition_limits: FontResourceLimits,
        content_limits: ContentFontLimits,
    ) -> Self {
        Self {
            authority,
            lookup_limits,
            context,
            acquisition_limits,
            content_limits,
        }
    }

    /// Borrows the strict revision authority used to reopen selected fonts.
    pub const fn authority(&self) -> &SharedAttestedRevisionIndex {
        &self.authority
    }

    /// Returns Page Font name-lookup limits.
    pub const fn lookup_limits(&self) -> PageFontLookupLimits {
        self.lookup_limits
    }

    /// Returns the runtime-owned child acquisition context.
    pub const fn context(&self) -> FontResourceJobContext {
        self.context
    }

    /// Returns per-font proof, decode, parse, and retention limits.
    pub const fn acquisition_limits(&self) -> FontResourceLimits {
        self.acquisition_limits
    }

    /// Returns aggregate Content text, glyph, plan, and cache limits.
    pub const fn content_limits(&self) -> ContentFontLimits {
        self.content_limits
    }
}

impl ContentImageProfile {
    /// Creates an explicit proof-bound Image XObject profile.
    pub const fn new(
        authority: SharedAttestedRevisionIndex,
        lookup_limits: PageXObjectLookupLimits,
        context: ImageXObjectJobContext,
        acquisition_limits: ImageXObjectLimits,
        content_limits: ContentImageLimits,
    ) -> Self {
        Self {
            authority,
            lookup_limits,
            context,
            acquisition_limits,
            content_limits,
        }
    }

    /// Borrows the strict revision authority used to reopen selected images.
    pub const fn authority(&self) -> &SharedAttestedRevisionIndex {
        &self.authority
    }

    /// Returns the Page XObject name-lookup limits.
    pub const fn lookup_limits(&self) -> PageXObjectLookupLimits {
        self.lookup_limits
    }

    /// Returns the runtime-owned child acquisition context.
    pub const fn context(&self) -> ImageXObjectJobContext {
        self.context
    }

    /// Returns per-image proof, metadata, decode, and retention limits.
    pub const fn acquisition_limits(&self) -> ImageXObjectLimits {
        self.acquisition_limits
    }

    /// Returns aggregate Content image-use and exact-cache limits.
    pub const fn content_limits(&self) -> ContentImageLimits {
        self.content_limits
    }
}

/// Single-owner sealed interpreter for one exact proof-bearing acquired Page.
pub struct InterpretPageJob {
    acquired: Option<AcquiredPageContent>,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    xobject_limits: PageXObjectLookupLimits,
    font_lookup_limits: PageFontLookupLimits,
    profile: ContentVmProfile,
    image_runtime: Option<ImageRuntime>,
    font_runtime: Option<FontRuntime>,
    ext_gstate_profile: Option<ContentExtGStateProfile>,
    program: Option<ContentProgram>,
    plan: Option<ExecutionPlan>,
    scan_peak_retained: u64,
    state: JobState,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    image_stats: ContentImageStats,
    font_lookup_stats: PageFontLookupStats,
    font_stats: ContentFontStats,
}

/// Single-owner sealed interpreter for one exact proof-bearing Form XObject.
pub struct InterpretFormJob {
    acquired: Option<Arc<AcquiredFormXObject>>,
    binding: SceneBinding,
    geometry: PageGeometry,
    initial_ctm: Matrix,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    xobject_limits: PageXObjectLookupLimits,
    font_lookup_limits: PageFontLookupLimits,
    profile: ContentVmProfile,
    image_runtime: Option<ImageRuntime>,
    font_runtime: Option<FontRuntime>,
    ext_gstate_profile: Option<ContentExtGStateProfile>,
    program: Option<ContentProgram>,
    plan: Option<ExecutionPlan>,
    scan_peak_retained: u64,
    state: FormJobState,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    image_stats: ContentImageStats,
    font_lookup_stats: PageFontLookupStats,
    font_stats: ContentFontStats,
}

enum FormJobState {
    Pending,
    Ready(Arc<InterpretedForm>),
    Unsupported(ContentUnsupported),
    Failed(ContentVmFailure),
}

impl InterpretPageJob {
    /// Creates a pending interpreter whose only input is an exact acquired Page.
    pub fn new(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        property_limits: PagePropertyLookupLimits,
        scene_limits: SceneLimits,
    ) -> Self {
        Self {
            acquired: Some(acquired),
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits: PageXObjectLookupLimits::default(),
            font_lookup_limits: PageFontLookupLimits::default(),
            profile: ContentVmProfile::SceneV1 { scene_limits },
            image_runtime: None,
            font_runtime: None,
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        }
    }

    /// Creates a pending interpreter for the explicit graphics-capable Scene-v2 profile.
    pub fn new_graphics_v2(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        scene_limits: GraphicsSceneLimits,
    ) -> Self {
        Self {
            acquired: Some(acquired),
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits: PageXObjectLookupLimits::default(),
            font_lookup_limits: PageFontLookupLimits::default(),
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: None,
            font_runtime: None,
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        }
    }

    /// Creates a graphics-v2 interpreter with proof-bound basic Image XObject execution.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor keeps scanner, VM, graphics, properties, images, and Scene limits independently validated"
    )]
    pub fn new_graphics_v2_with_images(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        image_profile: ContentImageProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Self {
        let xobject_limits = image_profile.lookup_limits();
        Self {
            acquired: Some(acquired),
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits,
            font_lookup_limits: PageFontLookupLimits::default(),
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: Some(ImageRuntime::new(image_profile)),
            font_runtime: None,
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        }
    }

    /// Creates a graphics-v2 interpreter with proof-bound embedded simple TrueType execution.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor keeps scanner, VM, graphics, properties, fonts, and Scene limits independently validated"
    )]
    pub fn new_graphics_v2_with_fonts(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        font_profile: ContentFontProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Self {
        let font_lookup_limits = font_profile.lookup_limits();
        Self {
            acquired: Some(acquired),
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits: PageXObjectLookupLimits::default(),
            font_lookup_limits,
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: None,
            font_runtime: Some(FontRuntime::new(font_profile)),
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        }
    }

    /// Creates a graphics-v2 interpreter with both basic images and embedded simple TrueType.
    #[allow(
        clippy::too_many_arguments,
        reason = "the combined constructor keeps all proof authorities and independent limits explicit"
    )]
    pub fn new_graphics_v2_with_images_and_fonts(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        image_profile: ContentImageProfile,
        font_profile: ContentFontProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Self {
        let xobject_limits = image_profile.lookup_limits();
        let font_lookup_limits = font_profile.lookup_limits();
        Self {
            acquired: Some(acquired),
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits,
            font_lookup_limits,
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: Some(ImageRuntime::new(image_profile)),
            font_runtime: Some(FontRuntime::new(font_profile)),
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        }
    }

    /// Creates a graphics-v2 interpreter with images, embedded TrueType fonts, and proof-bound
    /// external alpha/blend graphics states.
    #[allow(
        clippy::too_many_arguments,
        reason = "the combined constructor keeps all proof authorities and independent limits explicit"
    )]
    pub fn new_graphics_v2_with_resources(
        acquired: AcquiredPageContent,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        image_profile: ContentImageProfile,
        font_profile: ContentFontProfile,
        ext_gstate_profile: ContentExtGStateProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Self {
        let mut job = Self::new_graphics_v2_with_images_and_fonts(
            acquired,
            scan_limits,
            vm_limits,
            graphics_limits,
            property_limits,
            image_profile,
            font_profile,
            scene_limits,
        );
        job.ext_gstate_profile = Some(ext_gstate_profile);
        job
    }

    /// Enables bounded Form XObject classification and recursive execution for `Do`.
    pub fn with_forms(mut self, profile: ContentFormProfile) -> Result<Self, ContentVmError> {
        let runtime = self
            .image_runtime
            .as_mut()
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InvalidLimits, None))?;
        runtime.enable_forms(profile)?;
        Ok(self)
    }

    /// Returns the pending or terminal phase.
    pub const fn phase(&self) -> ContentVmPhase {
        match self.state {
            JobState::Pending => ContentVmPhase::Pending,
            JobState::Ready(_) => ContentVmPhase::Ready,
            JobState::Unsupported(_) => ContentVmPhase::Unsupported,
            JobState::Failed(_) => ContentVmPhase::Failed,
        }
    }

    /// Returns lower scanner work from the first attempt or terminal replay.
    pub const fn scan_stats(&self) -> ContentScanStats {
        self.scan_stats
    }

    /// Returns VM work from the first attempt or terminal replay.
    pub const fn vm_stats(&self) -> ContentVmStats {
        self.vm_stats
    }

    /// Returns property lookup work from the first attempt or terminal replay.
    pub const fn property_stats(&self) -> PagePropertyLookupStats {
        self.property_stats
    }

    /// Returns Page XObject lookup work from the latest interpretation attempt.
    pub const fn xobject_stats(&self) -> PageXObjectLookupStats {
        self.xobject_stats
    }

    /// Returns aggregate Image XObject acquisition and exact-cache work.
    pub const fn image_stats(&self) -> ContentImageStats {
        self.image_stats
    }

    /// Returns Page Font lookup work from the latest interpretation attempt.
    pub const fn font_lookup_stats(&self) -> PageFontLookupStats {
        self.font_lookup_stats
    }

    /// Returns aggregate embedded-font acquisition, cache, text, and glyph work.
    pub const fn font_stats(&self) -> ContentFontStats {
        self.font_stats
    }

    /// Executes once against the current source generation, then replays the exact terminal result.
    pub fn poll(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ContentVmPoll {
        match &self.state {
            JobState::Ready(page) => return ContentVmPoll::Ready(Arc::clone(page)),
            JobState::Unsupported(error) => return ContentVmPoll::Unsupported(*error),
            JobState::Failed(error) => return ContentVmPoll::Failed(*error),
            JobState::Pending => {}
        }
        let report = {
            let acquired = self
                .acquired
                .as_ref()
                .expect("pending interpretation retains its acquired Page");
            run_interpretation(
                AcquiredContentInput::Page(acquired),
                &mut self.program,
                &mut self.plan,
                self.scan_limits,
                self.vm_limits,
                self.property_limits,
                self.xobject_limits,
                self.font_lookup_limits,
                self.profile,
                self.image_runtime.as_mut(),
                self.font_runtime.as_mut(),
                self.ext_gstate_profile.as_ref(),
                self.scan_stats,
                self.xobject_stats,
                self.scan_peak_retained,
                source,
                cancellation,
            )
        };
        self.scan_stats = report.scan_stats;
        self.vm_stats = report.vm_stats;
        self.property_stats = report.property_stats;
        self.xobject_stats = report.xobject_stats;
        self.scan_peak_retained = report.scan_peak_retained;
        self.image_stats = self
            .image_runtime
            .as_ref()
            .map_or(ContentImageStats::default(), ImageRuntime::stats);
        self.font_stats = self
            .font_runtime
            .as_ref()
            .map_or(ContentFontStats::default(), FontRuntime::stats);
        self.font_lookup_stats = self
            .font_runtime
            .as_ref()
            .map_or(PageFontLookupStats::default(), FontRuntime::lookup_stats);

        match report.terminal {
            RunTerminal::Planned(_) => {
                unreachable!("semantic plans are retained internally before polling returns")
            }
            RunTerminal::Ready(execution) => {
                let acquired = self
                    .acquired
                    .take()
                    .expect("successful pending interpretation retains its acquired Page");
                let page = Arc::new(InterpretedPage::new(
                    acquired,
                    execution.scene,
                    execution.property_uses,
                    execution.image_uses,
                    execution.form_uses,
                    execution.font_uses,
                    execution.final_ctm,
                    self.scan_stats,
                    self.vm_stats,
                    self.property_stats,
                    self.xobject_stats,
                    self.image_stats,
                    self.font_lookup_stats,
                    self.font_stats,
                ));
                self.image_runtime.take();
                self.font_runtime.take();
                self.program.take();
                self.plan.take();
                self.state = JobState::Ready(Arc::clone(&page));
                ContentVmPoll::Ready(page)
            }
            RunTerminal::Unsupported(error) => {
                self.acquired.take();
                self.image_runtime.take();
                self.font_runtime.take();
                self.program.take();
                self.plan.take();
                self.state = JobState::Unsupported(error);
                ContentVmPoll::Unsupported(error)
            }
            RunTerminal::Failed(error) => {
                self.acquired.take();
                self.image_runtime.take();
                self.font_runtime.take();
                self.program.take();
                self.plan.take();
                self.state = JobState::Failed(error);
                ContentVmPoll::Failed(error)
            }
            RunTerminal::Pending {
                ticket,
                missing,
                checkpoint,
            } => ContentVmPoll::Pending {
                ticket,
                missing,
                checkpoint,
            },
        }
    }
}

impl fmt::Debug for InterpretPageJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterpretPageJob")
            .field("phase", &self.phase())
            .field(
                "handle",
                &self.acquired.as_ref().map(AcquiredPageContent::handle),
            )
            .field("scan_limits", &self.scan_limits)
            .field("vm_limits", &self.vm_limits)
            .field("property_limits", &self.property_limits)
            .field("xobject_limits", &self.xobject_limits)
            .field("font_lookup_limits", &self.font_lookup_limits)
            .field("images_enabled", &self.image_runtime.is_some())
            .field("fonts_enabled", &self.font_runtime.is_some())
            .field("program_retained", &self.program.is_some())
            .field("plan_retained", &self.plan.is_some())
            .field("profile", &self.profile)
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("property_stats", &self.property_stats)
            .field("xobject_stats", &self.xobject_stats)
            .field("image_stats", &self.image_stats)
            .field("font_lookup_stats", &self.font_lookup_stats)
            .field("font_stats", &self.font_stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl InterpretFormJob {
    /// Creates a graphics-v2 Form interpreter with proof-bound image and Font resources.
    ///
    /// The Form matrix is concatenated after the caller's invocation CTM before any Form
    /// operator is planned.
    #[allow(
        clippy::too_many_arguments,
        reason = "the Form constructor keeps proof authorities, coordinate context, and independent limits explicit"
    )]
    pub fn new_graphics_v2_with_images_and_fonts(
        acquired: Arc<AcquiredFormXObject>,
        binding: SceneBinding,
        geometry: PageGeometry,
        invocation_ctm: Matrix,
        scan_limits: ContentLimits,
        vm_limits: ContentVmLimits,
        graphics_limits: ContentGraphicsLimits,
        property_limits: PagePropertyLookupLimits,
        image_profile: ContentImageProfile,
        font_profile: ContentFontProfile,
        scene_limits: GraphicsSceneLimits,
    ) -> Result<Self, SceneError> {
        let form_matrix = Matrix::new(
            acquired
                .matrix()
                .map(|coordinate| SceneScalar::from_scaled(coordinate.scaled())),
        );
        let initial_ctm = invocation_ctm.checked_multiply(form_matrix)?;
        Ok(Self {
            acquired: Some(acquired),
            binding,
            geometry,
            initial_ctm,
            scan_limits,
            vm_limits,
            property_limits,
            xobject_limits: image_profile.lookup_limits(),
            font_lookup_limits: font_profile.lookup_limits(),
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: Some(ImageRuntime::new(image_profile)),
            font_runtime: Some(FontRuntime::new(font_profile)),
            ext_gstate_profile: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: FormJobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
            font_lookup_stats: PageFontLookupStats::default(),
            font_stats: ContentFontStats::default(),
        })
    }

    /// Installs a proof-bound external graphics-state registry owned by this Form scope.
    pub fn with_ext_gstates(mut self, profile: ContentExtGStateProfile) -> Self {
        self.ext_gstate_profile = Some(profile);
        self
    }

    /// Enables recursively bounded nested Form XObjects within this Form scope.
    pub fn with_forms(mut self, profile: ContentFormProfile) -> Result<Self, ContentVmError> {
        let runtime = self
            .image_runtime
            .as_mut()
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))?;
        runtime.enable_forms(profile)?;
        Ok(self)
    }

    /// Returns the pending or terminal Form phase.
    pub const fn phase(&self) -> ContentVmPhase {
        match self.state {
            FormJobState::Pending => ContentVmPhase::Pending,
            FormJobState::Ready(_) => ContentVmPhase::Ready,
            FormJobState::Unsupported(_) => ContentVmPhase::Unsupported,
            FormJobState::Failed(_) => ContentVmPhase::Failed,
        }
    }

    /// Executes once against the current source generation, then replays the terminal result.
    pub fn poll(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> ContentFormPoll {
        match &self.state {
            FormJobState::Ready(form) => return ContentFormPoll::Ready(Arc::clone(form)),
            FormJobState::Unsupported(error) => return ContentFormPoll::Unsupported(*error),
            FormJobState::Failed(error) => return ContentFormPoll::Failed(*error),
            FormJobState::Pending => {}
        }
        let report = {
            let acquired = self
                .acquired
                .as_ref()
                .expect("pending Form interpretation retains its acquisition");
            run_interpretation(
                AcquiredContentInput::Form {
                    acquired,
                    binding: self.binding,
                    geometry: self.geometry,
                    initial_ctm: self.initial_ctm,
                },
                &mut self.program,
                &mut self.plan,
                self.scan_limits,
                self.vm_limits,
                self.property_limits,
                self.xobject_limits,
                self.font_lookup_limits,
                self.profile,
                self.image_runtime.as_mut(),
                self.font_runtime.as_mut(),
                self.ext_gstate_profile.as_ref(),
                self.scan_stats,
                self.xobject_stats,
                self.scan_peak_retained,
                source,
                cancellation,
            )
        };
        self.scan_stats = report.scan_stats;
        self.vm_stats = report.vm_stats;
        self.property_stats = report.property_stats;
        self.xobject_stats = report.xobject_stats;
        self.scan_peak_retained = report.scan_peak_retained;
        self.image_stats = self
            .image_runtime
            .as_ref()
            .map_or(ContentImageStats::default(), ImageRuntime::stats);
        self.font_stats = self
            .font_runtime
            .as_ref()
            .map_or(ContentFontStats::default(), FontRuntime::stats);
        self.font_lookup_stats = self
            .font_runtime
            .as_ref()
            .map_or(PageFontLookupStats::default(), FontRuntime::lookup_stats);

        match report.terminal {
            RunTerminal::Planned(_) => {
                unreachable!("semantic plans are retained internally before polling returns")
            }
            RunTerminal::Ready(execution) => {
                let acquired = self
                    .acquired
                    .take()
                    .expect("successful Form interpretation retains its acquisition");
                let form = Arc::new(InterpretedForm::new(
                    acquired,
                    execution.scene,
                    execution.property_uses,
                    execution.image_uses,
                    execution.form_uses,
                    execution.font_uses,
                    execution.final_ctm,
                    self.scan_stats,
                    self.vm_stats,
                    self.property_stats,
                    self.xobject_stats,
                    self.image_stats,
                    self.font_lookup_stats,
                    self.font_stats,
                ));
                self.image_runtime.take();
                self.font_runtime.take();
                self.ext_gstate_profile.take();
                self.program.take();
                self.plan.take();
                self.state = FormJobState::Ready(Arc::clone(&form));
                ContentFormPoll::Ready(form)
            }
            RunTerminal::Unsupported(error) => {
                self.clear_transient();
                self.state = FormJobState::Unsupported(error);
                ContentFormPoll::Unsupported(error)
            }
            RunTerminal::Failed(error) => {
                self.clear_transient();
                self.state = FormJobState::Failed(error);
                ContentFormPoll::Failed(error)
            }
            RunTerminal::Pending {
                ticket,
                missing,
                checkpoint,
            } => ContentFormPoll::Pending {
                ticket,
                missing,
                checkpoint,
            },
        }
    }

    fn clear_transient(&mut self) {
        self.acquired.take();
        self.image_runtime.take();
        self.font_runtime.take();
        self.ext_gstate_profile.take();
        self.program.take();
        self.plan.take();
    }
}

impl fmt::Debug for InterpretFormJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InterpretFormJob")
            .field("phase", &self.phase())
            .field(
                "reference",
                &self.acquired.as_ref().map(|form| form.reference()),
            )
            .field("images_enabled", &self.image_runtime.is_some())
            .field("fonts_enabled", &self.font_runtime.is_some())
            .field("ext_gstates_enabled", &self.ext_gstate_profile.is_some())
            .field("program_retained", &self.program.is_some())
            .field("plan_retained", &self.plan.is_some())
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

struct Execution {
    scene: Scene,
    property_uses: Vec<ResolvedPropertyUse>,
    image_uses: Vec<ResolvedImageUse>,
    form_uses: Vec<ResolvedFormUse>,
    font_uses: Vec<ResolvedFontUse>,
    retained_use_capacity_bytes: u64,
    final_ctm: Matrix,
}

#[derive(Clone, Copy)]
enum AcquiredContentInput<'a> {
    Page(&'a AcquiredPageContent),
    Form {
        acquired: &'a AcquiredFormXObject,
        binding: SceneBinding,
        geometry: PageGeometry,
        initial_ctm: Matrix,
    },
}

impl<'a> AcquiredContentInput<'a> {
    fn snapshot(self) -> SourceSnapshot {
        match self {
            Self::Page(acquired) => acquired.handle().snapshot(),
            Self::Form { acquired, .. } => acquired.proof().snapshot(),
        }
    }

    fn resources(self) -> &'a PageResourceScope {
        match self {
            Self::Page(acquired) => acquired.page().resources(),
            Self::Form { acquired, .. } => acquired.resources(),
        }
    }

    fn stream_count(self) -> usize {
        match self {
            Self::Page(acquired) => acquired.streams().len(),
            Self::Form { .. } => 1,
        }
    }

    fn decoded_bytes(self) -> u64 {
        match self {
            Self::Page(acquired) => acquired
                .streams()
                .iter()
                .try_fold(0_u64, |total, stream| {
                    total.checked_add(u64::try_from(stream.decoded_bytes().len()).ok()?)
                })
                .unwrap_or(u64::MAX),
            Self::Form { acquired, .. } => {
                u64::try_from(acquired.content_bytes().len()).unwrap_or(u64::MAX)
            }
        }
    }

    fn append_descriptors(self, descriptors: &mut Vec<DecodedContentStream<'a>>) {
        match self {
            Self::Page(acquired) => {
                for stream in acquired.streams() {
                    descriptors.push(DecodedContentStream::new(
                        stream.reference(),
                        stream.stream_index(),
                        stream.decoded_bytes(),
                    ));
                }
            }
            Self::Form { acquired, .. } => {
                descriptors.push(DecodedContentStream::new(
                    acquired.reference(),
                    0,
                    acquired.content_bytes(),
                ));
            }
        }
    }

    fn scene_context(self) -> Result<(SceneBinding, PageGeometry), SceneError> {
        match self {
            Self::Page(acquired) => page_scene_context(acquired),
            Self::Form {
                binding, geometry, ..
            } => Ok((binding, geometry)),
        }
    }

    fn initial_ctm(self) -> Matrix {
        match self {
            Self::Page(_) => Matrix::IDENTITY,
            Self::Form { initial_ctm, .. } => initial_ctm,
        }
    }

    fn form(self) -> Option<&'a AcquiredFormXObject> {
        match self {
            Self::Page(_) => None,
            Self::Form { acquired, .. } => Some(acquired),
        }
    }
}

#[derive(Clone, Copy)]
struct FormEnvelope {
    source: ContentOperatorSource,
    command_source: CommandSource,
    bounds: SceneBounds,
    simple_transparency_group: bool,
    retained_bytes: u64,
}

struct PlannedImageInvocation {
    source: ContentOperatorSource,
    name: Vec<u8>,
    transform: Matrix,
}

#[derive(Clone, Copy, Default)]
struct TextPlanningState {
    font_selected: bool,
}

enum TextShowItem {
    Bytes(Vec<u8>),
    Adjustment(ContentNumber),
}

enum TextAction {
    Begin {
        source: ContentOperatorSource,
    },
    End {
        source: ContentOperatorSource,
    },
    SetCharacterSpacing {
        value: ContentNumber,
        source: ContentOperatorSource,
    },
    SetWordSpacing {
        value: ContentNumber,
        source: ContentOperatorSource,
    },
    SetHorizontalScaling {
        value: ContentNumber,
        source: ContentOperatorSource,
    },
    SetLeading {
        value: ContentNumber,
        source: ContentOperatorSource,
    },
    SetFont {
        name: Vec<u8>,
        size: ContentNumber,
        source: ContentOperatorSource,
    },
    SetRenderMode {
        value: i64,
        source: ContentOperatorSource,
    },
    SetRise {
        value: ContentNumber,
        source: ContentOperatorSource,
    },
    MovePosition {
        translation: [ContentNumber; 2],
        set_leading: bool,
        source: ContentOperatorSource,
    },
    SetMatrix {
        matrix: [ContentNumber; 6],
        source: ContentOperatorSource,
    },
    NextLine {
        source: ContentOperatorSource,
    },
    Show {
        items: Vec<TextShowItem>,
        character_spacing: Option<ContentNumber>,
        word_spacing: Option<ContentNumber>,
        next_line: bool,
        paint: Paint,
        ctm: Matrix,
        command_source: CommandSource,
        source: ContentOperatorSource,
    },
}

impl TextAction {
    const fn source(&self) -> ContentOperatorSource {
        match self {
            Self::Begin { source }
            | Self::End { source }
            | Self::SetCharacterSpacing { source, .. }
            | Self::SetWordSpacing { source, .. }
            | Self::SetHorizontalScaling { source, .. }
            | Self::SetLeading { source, .. }
            | Self::SetFont { source, .. }
            | Self::SetRenderMode { source, .. }
            | Self::SetRise { source, .. }
            | Self::MovePosition { source, .. }
            | Self::SetMatrix { source, .. }
            | Self::NextLine { source }
            | Self::Show { source, .. } => *source,
        }
    }
}

enum ExecutionAction {
    BeginMarkedContent {
        tag: Vec<u8>,
        properties: Option<ObjectRef>,
        source: CommandSource,
    },
    EndMarkedContent {
        source: CommandSource,
    },
    Save {
        bounds: SceneBounds,
        source: CommandSource,
    },
    Restore {
        bounds: SceneBounds,
        source: CommandSource,
    },
    BeginGroup {
        alpha: SceneUnit,
        blend_mode: pdf_rs_scene::BlendMode,
        bounds: SceneBounds,
        source: CommandSource,
    },
    EndGroup {
        bounds: SceneBounds,
        source: CommandSource,
    },
    Clip {
        path: PathResource,
        rule: FillRule,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    },
    Fill {
        path: PathResource,
        rule: FillRule,
        paint: Paint,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    },
    Stroke {
        path: PathResource,
        paint: Paint,
        style: LineStyle,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    },
    FillStroke {
        path: PathResource,
        rule: FillRule,
        fill: Paint,
        stroke: Paint,
        style: LineStyle,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    },
    DrawImage {
        source: ContentOperatorSource,
        command_source: CommandSource,
        transform: Matrix,
        alpha: SceneUnit,
        blend_mode: pdf_rs_scene::BlendMode,
        bounds: SceneBounds,
    },
    Text(TextAction),
}

struct ExecutionPlan {
    binding: SceneBinding,
    geometry: PageGeometry,
    actions: Vec<ExecutionAction>,
    image_invocations: Vec<PlannedImageInvocation>,
    property_uses: Vec<ResolvedPropertyUse>,
    image_uses: Vec<ResolvedImageUse>,
    font_use_count: usize,
    first_font_source: Option<ContentOperatorSource>,
    final_ctm: Matrix,
    action_payload_retained_bytes: u64,
    owned_name_retained_bytes: u64,
    accounting: Accounting,
    property_stats: PagePropertyLookupStats,
}

impl ExecutionPlan {
    fn vm_retained_bytes(&self) -> Result<u64, ContentVmError> {
        execution_plan_capacity_bytes(
            &self.property_uses,
            &self.image_uses,
            &self.image_invocations,
            &self.actions,
            self.owned_name_retained_bytes,
        )?
        .checked_add(self.action_payload_retained_bytes)
        .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
    }

    fn image_plan_retained_bytes(&self) -> Result<u64, ContentVmError> {
        capacity_bytes(&self.actions)?
            .checked_add(capacity_bytes(&self.image_invocations)?)
            .and_then(|value| value.checked_add(self.owned_name_retained_bytes))
            .and_then(|value| value.checked_add(self.action_payload_retained_bytes))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
    }

    fn font_plan_retained_bytes(&self) -> Result<u64, ContentVmError> {
        capacity_bytes(&self.actions)?
            .checked_add(self.action_payload_retained_bytes)
            .and_then(|value| value.checked_add(self.owned_name_retained_bytes))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
    }
}

enum TextPlanningTerminal {
    Failed(ContentVmFailure),
    Unsupported(ContentUnsupported),
}

impl From<ContentVmError> for TextPlanningTerminal {
    fn from(value: ContentVmError) -> Self {
        Self::Failed(ContentVmFailure::Vm(value))
    }
}

impl From<SceneError> for TextPlanningTerminal {
    fn from(value: SceneError) -> Self {
        Self::Failed(ContentVmFailure::Scene(value))
    }
}

enum SceneSink {
    V1(SceneBuilder),
    V2(GraphicsSceneBuilder),
}

impl SceneSink {
    fn begin_marked_content(
        &mut self,
        tag: &[u8],
        properties: Option<pdf_rs_syntax::ObjectRef>,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        match self {
            Self::V1(builder) => builder.begin_marked_content(tag, properties, source),
            Self::V2(_) => Ok(()),
        }
    }

    fn end_marked_content(&mut self, source: CommandSource) -> Result<(), SceneError> {
        match self {
            Self::V1(builder) => builder.end_marked_content(source),
            Self::V2(_) => Ok(()),
        }
    }

    fn finish(self) -> Result<Scene, SceneError> {
        match self {
            Self::V1(builder) => builder.finish(),
            Self::V2(builder) => builder.finish(),
        }
    }

    fn graphics_mut(&mut self) -> Option<&mut GraphicsSceneBuilder> {
        match self {
            Self::V1(_) => None,
            Self::V2(builder) => Some(builder),
        }
    }
}

enum RunTerminal {
    Ready(Execution),
    Planned(ExecutionPlan),
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
        checkpoint: ResumeCheckpoint,
    },
    Unsupported(ContentUnsupported),
    Failed(ContentVmFailure),
}

struct RunReport {
    terminal: RunTerminal,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    scan_peak_retained: u64,
}

#[derive(Clone, Copy, Default)]
struct Accounting {
    operators: u64,
    fuel: u64,
    max_graphics_depth: u32,
    max_compatibility_depth: u32,
    max_marked_depth: u32,
    property_uses: u64,
    image_uses: u64,
    peak_retained: u64,
}

impl Accounting {
    fn observe_retained(&mut self, retained: u64) {
        self.peak_retained = self.peak_retained.max(retained);
    }

    fn charge_fuel(
        &mut self,
        limits: ContentVmLimits,
        amount: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        limits.preflight(ContentVmLimitKind::Fuel, self.fuel, amount, Some(source))?;
        self.fuel = self
            .fuel
            .checked_add(amount)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        Ok(())
    }

    fn snapshot(&self, retained: u64) -> ContentVmStats {
        ContentVmStats::new(
            self.operators,
            self.fuel,
            self.max_graphics_depth,
            self.max_compatibility_depth,
            self.max_marked_depth,
            self.property_uses,
            self.image_uses,
            retained,
            self.peak_retained,
        )
    }
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "the sealed interpreter receives each independently validated lower limit profile"
)]
fn run_interpretation(
    input: AcquiredContentInput<'_>,
    program_slot: &mut Option<ContentProgram>,
    plan_slot: &mut Option<ExecutionPlan>,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    xobject_limits: PageXObjectLookupLimits,
    font_lookup_limits: PageFontLookupLimits,
    profile: ContentVmProfile,
    mut image_runtime: Option<&mut ImageRuntime>,
    mut font_runtime: Option<&mut FontRuntime>,
    ext_gstate_profile: Option<&ContentExtGStateProfile>,
    mut scan_stats: ContentScanStats,
    mut xobject_stats: PageXObjectLookupStats,
    mut scan_peak_retained: u64,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> RunReport {
    let snapshot = input.snapshot();
    let mut accounting = Accounting::default();
    let mut property_stats = PagePropertyLookupStats::default();

    if let Err(failure) = runtime_guard(snapshot, source, cancellation, None) {
        return report(
            RunTerminal::Failed(failure),
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    if program_slot.is_none() && plan_slot.is_none() {
        if let Some(runtime) = image_runtime.as_deref_mut() {
            let input_bytes = input.decoded_bytes();
            if input_bytes == u64::MAX {
                return report(
                    RunTerminal::Failed(ContentVmFailure::Vm(ContentVmError::new(
                        ContentVmErrorCode::InternalState,
                        None,
                    ))),
                    scan_stats,
                    &accounting,
                    property_stats,
                    xobject_stats,
                    scan_peak_retained,
                    0,
                );
            }
            if let Err(error) = runtime.record_scan(input_bytes) {
                return report(
                    prioritize_vm_without_source(snapshot, source, cancellation, error),
                    scan_stats,
                    &accounting,
                    property_stats,
                    xobject_stats,
                    scan_peak_retained,
                    0,
                );
            }
        }
        let mut descriptors = Vec::new();
        let descriptor_bytes =
            match reserve_exact_slots(&mut descriptors, input.stream_count(), 0, vm_limits, None) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return report(
                        prioritize_vm_without_source(snapshot, source, cancellation, error),
                        scan_stats,
                        &accounting,
                        property_stats,
                        xobject_stats,
                        scan_peak_retained,
                        0,
                    );
                }
            };
        input.append_descriptors(&mut descriptors);
        let scan = run_scan(
            &descriptors,
            scan_limits,
            &DocumentCancellationAdapter(cancellation),
        );
        scan_stats = scan.stats();
        scan_peak_retained = descriptor_bytes.saturating_add(scan_stats.retained_bytes());
        let program = match scan.into_terminal() {
            ScanTerminal::Ready(program) => program,
            ScanTerminal::Failed(error) => {
                accounting.observe_retained(scan_peak_retained);
                return report(
                    prioritize(
                        snapshot,
                        source,
                        cancellation,
                        None,
                        RunTerminal::Failed(ContentVmFailure::Content(error)),
                    ),
                    scan_stats,
                    &accounting,
                    property_stats,
                    xobject_stats,
                    scan_peak_retained,
                    0,
                );
            }
        };
        if let Err(error) = vm_limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            descriptor_bytes,
            scan_stats.retained_bytes(),
            None,
        ) {
            return report(
                prioritize_vm_without_source(snapshot, source, cancellation, error),
                scan_stats,
                &accounting,
                property_stats,
                xobject_stats,
                scan_peak_retained,
                0,
            );
        }
        if let Err(failure) = runtime_guard(snapshot, source, cancellation, None) {
            return report(
                RunTerminal::Failed(failure),
                scan_stats,
                &accounting,
                property_stats,
                xobject_stats,
                scan_peak_retained,
                0,
            );
        }
        *program_slot = Some(program);
    }
    let program_bytes = scan_stats.retained_bytes();

    if plan_slot.is_none() {
        accounting.observe_retained(scan_peak_retained);
        let program = program_slot
            .as_ref()
            .expect("a successful scan remains retained until semantic planning completes");
        let planning = build_execution_plan(
            input,
            program,
            program_bytes,
            vm_limits,
            property_limits,
            profile,
            image_runtime.as_deref_mut(),
            font_runtime.as_deref_mut(),
            ext_gstate_profile,
            source,
            cancellation,
            &mut accounting,
        );
        property_stats = planning.property_stats;
        match planning.terminal {
            RunTerminal::Planned(plan) => {
                accounting = plan.accounting;
                property_stats = plan.property_stats;
                *plan_slot = Some(plan);
                program_slot.take();
            }
            terminal => {
                return report(
                    terminal,
                    scan_stats,
                    &accounting,
                    property_stats,
                    xobject_stats,
                    scan_peak_retained,
                    0,
                );
            }
        }
    } else {
        let plan = plan_slot
            .as_ref()
            .expect("a retained execution plan remains immutable across resource Pending");
        accounting = plan.accounting;
        property_stats = plan.property_stats;
    }

    let plan = plan_slot
        .as_ref()
        .expect("semantic planning publishes one immutable execution plan");

    if let Some(runtime) = image_runtime.as_deref_mut()
        && !runtime.plan_complete()
        && let Err(terminal) = plan_image_resources(
            input,
            plan,
            xobject_limits,
            runtime,
            source,
            cancellation,
            &mut xobject_stats,
        )
    {
        return report(
            terminal,
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    if let Some(runtime) = font_runtime.as_deref_mut()
        && !runtime.plan_complete()
        && let Err(terminal) = plan_font_resources(
            input,
            plan,
            font_lookup_limits,
            runtime,
            source,
            cancellation,
        )
    {
        return report(
            terminal,
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    if let Some(runtime) = image_runtime.as_deref_mut()
        && !runtime.acquisitions_complete()
    {
        let terminal =
            match runtime.poll_acquisitions(plan.binding, plan.geometry, source, cancellation) {
                ImagePlanningPoll::Ready => None,
                ImagePlanningPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => Some(RunTerminal::Pending {
                    ticket,
                    missing,
                    checkpoint,
                }),
                ImagePlanningPoll::Unsupported(unsupported) => {
                    Some(RunTerminal::Unsupported(unsupported))
                }
                ImagePlanningPoll::Failed(failure) => Some(RunTerminal::Failed(failure)),
            };
        if let Some(terminal) = terminal {
            return report(
                terminal,
                scan_stats,
                &accounting,
                property_stats,
                xobject_stats,
                scan_peak_retained,
                0,
            );
        }
    }
    if let Some(runtime) = font_runtime.as_deref_mut()
        && !runtime.acquisitions_complete()
    {
        let terminal = match runtime.poll_acquisitions(source, cancellation) {
            FontPlanningPoll::Ready => None,
            FontPlanningPoll::Pending {
                ticket,
                missing,
                checkpoint,
            } => Some(RunTerminal::Pending {
                ticket,
                missing,
                checkpoint,
            }),
            FontPlanningPoll::Unsupported {
                unsupported,
                source,
            } => Some(RunTerminal::Unsupported(ContentUnsupported::from_font(
                unsupported,
                source,
            ))),
            FontPlanningPoll::Failed(failure) => Some(RunTerminal::Failed(failure)),
        };
        if let Some(terminal) = terminal {
            return report(
                terminal,
                scan_stats,
                &accounting,
                property_stats,
                xobject_stats,
                scan_peak_retained,
                0,
            );
        }
    }
    if let Some(runtime) = image_runtime.as_deref_mut()
        && let Err(error) = runtime.begin_execution()
    {
        return report(
            prioritize_vm_without_source(snapshot, source, cancellation, error),
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    if let Some(runtime) = font_runtime.as_deref_mut()
        && let Err(error) = runtime.begin_execution()
    {
        return report(
            prioritize_vm_without_source(snapshot, source, cancellation, error),
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    let plan = plan_slot
        .take()
        .expect("all resources ready before consuming the immutable execution plan");
    let mut materialization_peak_retained = 0_u64;
    let terminal = materialize_execution_plan(
        input,
        plan,
        profile,
        vm_limits,
        image_runtime.as_deref_mut(),
        font_runtime.as_deref_mut(),
        source,
        cancellation,
        &mut materialization_peak_retained,
    );
    accounting.observe_retained(materialization_peak_retained);
    if matches!(terminal, RunTerminal::Ready(_))
        && let Some(runtime) = image_runtime.as_deref()
        && let Err(error) = runtime.finish_execution()
    {
        return report(
            prioritize_vm_without_source(snapshot, source, cancellation, error),
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    if matches!(terminal, RunTerminal::Ready(_))
        && let Some(runtime) = font_runtime.as_deref()
        && let Err(error) = runtime.finish_execution()
    {
        return report(
            prioritize_vm_without_source(snapshot, source, cancellation, error),
            scan_stats,
            &accounting,
            property_stats,
            xobject_stats,
            scan_peak_retained,
            0,
        );
    }
    let retained = match &terminal {
        RunTerminal::Ready(value) => value.retained_use_capacity_bytes,
        RunTerminal::Planned(_) => 0,
        RunTerminal::Pending { .. } | RunTerminal::Unsupported(_) | RunTerminal::Failed(_) => 0,
    };
    report(
        terminal,
        scan_stats,
        &accounting,
        property_stats,
        xobject_stats,
        scan_peak_retained,
        retained,
    )
}

fn report(
    terminal: RunTerminal,
    scan_stats: ContentScanStats,
    accounting: &Accounting,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    scan_peak_retained: u64,
    retained: u64,
) -> RunReport {
    RunReport {
        terminal,
        scan_stats,
        vm_stats: accounting.snapshot(retained),
        property_stats,
        xobject_stats,
        scan_peak_retained,
    }
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "image planning keeps each sealed lower limit and runtime authority explicit"
)]
fn plan_image_resources(
    input: AcquiredContentInput<'_>,
    plan: &ExecutionPlan,
    xobject_limits: PageXObjectLookupLimits,
    runtime: &mut ImageRuntime,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    xobject_stats: &mut PageXObjectLookupStats,
) -> Result<(), RunTerminal> {
    let snapshot = input.snapshot();
    let first_image_source = plan.image_invocations.first().map(|image| image.source);
    runtime
        .begin_plan(plan.image_invocations.len(), first_image_source)
        .map_err(|error| {
            prioritize(
                snapshot,
                byte_source,
                cancellation,
                first_image_source,
                RunTerminal::Failed(ContentVmFailure::Vm(error)),
            )
        })?;

    let mut resolver = input.resources().xobject_resolver(xobject_limits);
    let result = (|| {
        for image in &plan.image_invocations {
            let source = image.source;
            runtime_guard(snapshot, byte_source, cancellation, Some(source))
                .map_err(RunTerminal::Failed)?;
            runtime.admit_lookup(source).map_err(|error| {
                prioritize_vm(snapshot, byte_source, cancellation, source, error)
            })?;
            let proof = match resolver.lookup_image_xobject(&image.name, byte_source, cancellation)
            {
                Ok(PageXObjectLookupOutcome::Ready(proof)) => proof,
                Ok(PageXObjectLookupOutcome::Unsupported(unsupported)) => {
                    return Err(prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(source),
                        RunTerminal::Unsupported(ContentUnsupported::from_image(
                            unsupported,
                            source,
                        )),
                    ));
                }
                Err(error) => {
                    return Err(prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(source),
                        RunTerminal::Failed(ContentVmFailure::Document(error)),
                    ));
                }
            };
            runtime_guard(snapshot, byte_source, cancellation, Some(source))
                .map_err(RunTerminal::Failed)?;
            runtime
                .register_proof(proof, image.transform, source, byte_source, cancellation)
                .map_err(RunTerminal::Failed)?;
        }
        runtime.finish_plan().map_err(|error| {
            prioritize(
                snapshot,
                byte_source,
                cancellation,
                first_image_source,
                RunTerminal::Failed(ContentVmFailure::Vm(error)),
            )
        })
    })();
    *xobject_stats = resolver.stats();
    result
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "font planning keeps proof lookup, runtime authority, and source guards explicit"
)]
fn plan_font_resources(
    input: AcquiredContentInput<'_>,
    plan: &ExecutionPlan,
    lookup_limits: PageFontLookupLimits,
    runtime: &mut FontRuntime,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> Result<(), RunTerminal> {
    let snapshot = input.snapshot();
    let expected = plan.font_use_count;
    let first_source = plan.first_font_source;
    runtime
        .begin_plan(expected, first_source)
        .map_err(|error| {
            prioritize(
                snapshot,
                byte_source,
                cancellation,
                first_source,
                RunTerminal::Failed(ContentVmFailure::Vm(error)),
            )
        })?;

    let mut resolver = input.resources().font_resolver(lookup_limits);
    let result = (|| {
        for action in &plan.actions {
            let action_source = action_operator_source(action);
            runtime_guard(snapshot, byte_source, cancellation, Some(action_source))
                .map_err(RunTerminal::Failed)?;
            let ExecutionAction::Text(TextAction::SetFont { name, source, .. }) = action else {
                continue;
            };
            runtime.admit_lookup(*source).map_err(|error| {
                prioritize_vm(snapshot, byte_source, cancellation, *source, error)
            })?;
            let proof = match resolver.lookup_font(name, byte_source, cancellation) {
                Ok(PageFontLookupOutcome::Ready(proof)) => proof,
                Ok(PageFontLookupOutcome::Unsupported(unsupported)) => {
                    return Err(prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(*source),
                        RunTerminal::Unsupported(ContentUnsupported::from_font(
                            unsupported,
                            *source,
                        )),
                    ));
                }
                Err(error) => {
                    return Err(prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(*source),
                        RunTerminal::Failed(ContentVmFailure::Document(error)),
                    ));
                }
            };
            runtime_guard(snapshot, byte_source, cancellation, Some(*source))
                .map_err(RunTerminal::Failed)?;
            runtime
                .register_proof(proof, *source, byte_source, cancellation)
                .map_err(RunTerminal::Failed)?;
        }
        runtime.finish_plan().map_err(|error| {
            prioritize(
                snapshot,
                byte_source,
                cancellation,
                first_source,
                RunTerminal::Failed(ContentVmFailure::Vm(error)),
            )
        })
    })();
    runtime.set_lookup_stats(resolver.stats());
    result
}

struct ExecutionReport {
    terminal: RunTerminal,
    property_stats: PagePropertyLookupStats,
}

fn is_text_operator(kind: OperatorKind) -> bool {
    matches!(
        kind,
        OperatorKind::BeginText
            | OperatorKind::EndText
            | OperatorKind::SetCharacterSpacing
            | OperatorKind::SetWordSpacing
            | OperatorKind::SetHorizontalScaling
            | OperatorKind::SetTextLeading
            | OperatorKind::SetTextFont
            | OperatorKind::SetTextRenderMode
            | OperatorKind::SetTextRise
            | OperatorKind::MoveTextPosition
            | OperatorKind::MoveTextPositionSetLeading
            | OperatorKind::SetTextMatrix
            | OperatorKind::MoveToNextTextLine
            | OperatorKind::ShowText
            | OperatorKind::ShowTextAdjusted
            | OperatorKind::MoveNextLineShowText
            | OperatorKind::SetSpacingMoveNextLineShowText
    )
}

enum TextItemsInput<'a> {
    String(&'a crate::ContentString),
    Array(&'a [LocatedOperand]),
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "text planning keeps source guards, profile ownership, immutable-plan retention, and provenance explicit"
)]
fn plan_text_operator(
    kind: OperatorKind,
    operands: &ValidatedOperands<'_>,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    font_runtime: Option<&mut FontRuntime>,
    graphics: Option<&GraphicsVm>,
    text_active: &mut bool,
    text_state: &mut TextPlanningState,
    planned_font_uses: &mut u64,
    actions: &mut Vec<ExecutionAction>,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: &mut u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<(), TextPlanningTerminal> {
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;

    if kind == OperatorKind::BeginText {
        if *text_active {
            return Err(vm_error(ContentVmErrorCode::InvalidTextObject, source).into());
        }
        append_text_action(
            actions,
            TextAction::Begin { source },
            property_uses,
            image_uses,
            planned_images,
            *planned_name_bytes,
            program_bytes,
            vm_limits,
            source,
            accounting,
        )?;
        *text_active = true;
        return Ok(());
    }
    if kind == OperatorKind::EndText {
        if !*text_active {
            return Err(vm_error(ContentVmErrorCode::InvalidTextObject, source).into());
        }
        append_text_action(
            actions,
            TextAction::End { source },
            property_uses,
            image_uses,
            planned_images,
            *planned_name_bytes,
            program_bytes,
            vm_limits,
            source,
            accounting,
        )?;
        *text_active = false;
        return Ok(());
    }

    let Some(runtime) = font_runtime else {
        return Err(TextPlanningTerminal::Unsupported(ContentUnsupported::new(
            ContentUnsupportedKind::FontProfileRequired,
            source,
        )));
    };
    let Some(graphics) = graphics else {
        return Err(TextPlanningTerminal::Unsupported(ContentUnsupported::new(
            ContentUnsupportedKind::GraphicsV2Operator,
            source,
        )));
    };

    let action = match kind {
        OperatorKind::SetCharacterSpacing => {
            let ValidatedOperands::OneNumber(value) = operands else {
                unreachable!("validated Tc operands have one-number shape");
            };
            TextAction::SetCharacterSpacing {
                value: *value,
                source,
            }
        }
        OperatorKind::SetWordSpacing => {
            let ValidatedOperands::OneNumber(value) = operands else {
                unreachable!("validated Tw operands have one-number shape");
            };
            TextAction::SetWordSpacing {
                value: *value,
                source,
            }
        }
        OperatorKind::SetHorizontalScaling => {
            let ValidatedOperands::OneNumber(value) = operands else {
                unreachable!("validated Tz operands have one-number shape");
            };
            TextAction::SetHorizontalScaling {
                value: *value,
                source,
            }
        }
        OperatorKind::SetTextLeading => {
            let ValidatedOperands::OneNumber(value) = operands else {
                unreachable!("validated TL operands have one-number shape");
            };
            TextAction::SetLeading {
                value: *value,
                source,
            }
        }
        OperatorKind::SetTextFont => {
            let ValidatedOperands::NameAndNumber(name, size) = operands else {
                unreachable!("validated Tf operands have name-and-number shape");
            };
            runtime.admit_planned_use(*planned_font_uses, source)?;
            let other_capacity = execution_plan_capacity_bytes(
                property_uses,
                image_uses,
                planned_images,
                actions,
                *planned_name_bytes,
            )?;
            let (name, retained) = copy_text_plan_bytes(
                name.bytes(),
                program_bytes,
                other_capacity,
                vm_limits,
                snapshot,
                byte_source,
                cancellation,
                source,
                accounting,
            )?;
            *planned_name_bytes = planned_name_bytes
                .checked_add(retained)
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            *planned_font_uses = planned_font_uses
                .checked_add(1)
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            text_state.font_selected = true;
            TextAction::SetFont {
                name,
                size: *size,
                source,
            }
        }
        OperatorKind::SetTextRenderMode => {
            let ValidatedOperands::OneInteger(value) = operands else {
                unreachable!("validated Tr operands have integer shape");
            };
            if !(0..=7).contains(value) {
                return Err(vm_error(ContentVmErrorCode::InvalidGraphicsParameter, source).into());
            }
            if *value != 0 {
                return Err(TextPlanningTerminal::Unsupported(ContentUnsupported::new(
                    ContentUnsupportedKind::TextRenderMode,
                    source,
                )));
            }
            TextAction::SetRenderMode {
                value: *value,
                source,
            }
        }
        OperatorKind::SetTextRise => {
            let ValidatedOperands::OneNumber(value) = operands else {
                unreachable!("validated Ts operands have one-number shape");
            };
            TextAction::SetRise {
                value: *value,
                source,
            }
        }
        OperatorKind::MoveTextPosition | OperatorKind::MoveTextPositionSetLeading => {
            let ValidatedOperands::TwoNumbers(translation) = operands else {
                unreachable!("validated Td/TD operands have two-number shape");
            };
            TextAction::MovePosition {
                translation: *translation,
                set_leading: kind == OperatorKind::MoveTextPositionSetLeading,
                source,
            }
        }
        OperatorKind::SetTextMatrix => {
            let ValidatedOperands::SixNumbers(matrix) = operands else {
                unreachable!("validated Tm operands have six-number shape");
            };
            TextAction::SetMatrix {
                matrix: *matrix,
                source,
            }
        }
        OperatorKind::MoveToNextTextLine => TextAction::NextLine { source },
        OperatorKind::ShowText
        | OperatorKind::ShowTextAdjusted
        | OperatorKind::MoveNextLineShowText
        | OperatorKind::SetSpacingMoveNextLineShowText => {
            if !text_state.font_selected {
                return Err(vm_error(ContentVmErrorCode::InvalidTextObject, source).into());
            }
            let (input, character_spacing, word_spacing, next_line) = match (kind, operands) {
                (OperatorKind::ShowText, ValidatedOperands::String(value)) => {
                    (TextItemsInput::String(value), None, None, false)
                }
                (OperatorKind::ShowTextAdjusted, ValidatedOperands::Array(values)) => {
                    (TextItemsInput::Array(values), None, None, false)
                }
                (OperatorKind::MoveNextLineShowText, ValidatedOperands::String(value)) => {
                    (TextItemsInput::String(value), None, None, true)
                }
                (
                    OperatorKind::SetSpacingMoveNextLineShowText,
                    ValidatedOperands::TwoNumbersAndString(spacing, value),
                ) => (
                    TextItemsInput::String(value),
                    Some(spacing[1]),
                    Some(spacing[0]),
                    true,
                ),
                _ => unreachable!("validated text-show operands have registered shape"),
            };
            let (items, retained) = seal_text_items(
                input,
                runtime,
                snapshot,
                byte_source,
                cancellation,
                actions,
                property_uses,
                image_uses,
                planned_images,
                *planned_name_bytes,
                program_bytes,
                vm_limits,
                source,
                accounting,
            )?;
            *planned_name_bytes = planned_name_bytes
                .checked_add(retained)
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            TextAction::Show {
                items,
                character_spacing,
                word_spacing,
                next_line,
                paint: graphics.image_paint(),
                ctm: graphics.current_ctm(),
                command_source: command_source(source)?,
                source,
            }
        }
        OperatorKind::BeginText | OperatorKind::EndText => {
            unreachable!("text boundaries return before profile dispatch")
        }
        _ => unreachable!("only registered text operators reach text planning"),
    };

    append_text_action(
        actions,
        action,
        property_uses,
        image_uses,
        planned_images,
        *planned_name_bytes,
        program_bytes,
        vm_limits,
        source,
        accounting,
    )
    .map_err(Into::into)
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "text item sealing keeps source/cancellation precedence and all live plan capacities explicit"
)]
fn seal_text_items(
    input: TextItemsInput<'_>,
    runtime: &mut FontRuntime,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    actions: &Vec<ExecutionAction>,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<(Vec<TextShowItem>, u64), TextPlanningTerminal> {
    let (item_count, bytes, adjustments) = match input {
        TextItemsInput::String(value) => (
            1_usize,
            u64::try_from(value.bytes().len())
                .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?,
            0_u64,
        ),
        TextItemsInput::Array(values) => {
            let mut bytes = 0_u64;
            let mut adjustments = 0_u64;
            for (index, value) in values.iter().enumerate() {
                if index.is_multiple_of(256) {
                    runtime_guard(snapshot, byte_source, cancellation, Some(source))
                        .map_err(TextPlanningTerminal::Failed)?;
                }
                match value.value() {
                    ContentOperand::String(value) => {
                        bytes = bytes
                            .checked_add(
                                u64::try_from(value.bytes().len()).map_err(|_| {
                                    vm_error(ContentVmErrorCode::InternalState, source)
                                })?,
                            )
                            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
                    }
                    ContentOperand::Integer(_) | ContentOperand::Real(_) => {
                        adjustments = adjustments
                            .checked_add(1)
                            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
                    }
                    _ => {
                        return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source).into());
                    }
                }
            }
            (values.len(), bytes, adjustments)
        }
    };
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;
    if matches!(input, TextItemsInput::Array(_)) {
        accounting.charge_fuel(vm_limits, bytes, source)?;
    }
    runtime.admit_text(bytes, adjustments, source)?;

    match input {
        TextItemsInput::String(value) => {
            validate_printable_text(value.bytes(), snapshot, byte_source, cancellation, source)?;
        }
        TextItemsInput::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                if index.is_multiple_of(256) {
                    runtime_guard(snapshot, byte_source, cancellation, Some(source))
                        .map_err(TextPlanningTerminal::Failed)?;
                }
                match value.value() {
                    ContentOperand::String(value) => validate_printable_text(
                        value.bytes(),
                        snapshot,
                        byte_source,
                        cancellation,
                        source,
                    )?,
                    ContentOperand::Integer(_) | ContentOperand::Real(_) => {
                        parse_number(value, source)?;
                    }
                    _ => unreachable!("TJ child shape was counted before admission"),
                }
            }
        }
    }
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;

    let plan_capacity = execution_plan_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        actions,
        planned_name_bytes,
    )?;
    let consumed = program_bytes
        .checked_add(plan_capacity)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    let mut items = Vec::new();
    let mut retained = reserve_exact_slots_accounted(
        &mut items,
        item_count,
        consumed,
        vm_limits,
        Some(source),
        accounting,
    )?;

    match input {
        TextItemsInput::String(value) => {
            retained = push_text_string_item(
                &mut items,
                value,
                retained,
                snapshot,
                byte_source,
                cancellation,
                actions,
                property_uses,
                image_uses,
                planned_images,
                planned_name_bytes,
                program_bytes,
                vm_limits,
                source,
                accounting,
            )?;
        }
        TextItemsInput::Array(values) => {
            for value in values {
                runtime_guard(snapshot, byte_source, cancellation, Some(source))
                    .map_err(TextPlanningTerminal::Failed)?;
                match value.value() {
                    ContentOperand::String(value) => {
                        retained = push_text_string_item(
                            &mut items,
                            value,
                            retained,
                            snapshot,
                            byte_source,
                            cancellation,
                            actions,
                            property_uses,
                            image_uses,
                            planned_images,
                            planned_name_bytes,
                            program_bytes,
                            vm_limits,
                            source,
                            accounting,
                        )?;
                    }
                    ContentOperand::Integer(_) | ContentOperand::Real(_) => {
                        items.push(TextShowItem::Adjustment(parse_number(value, source)?));
                    }
                    _ => unreachable!("TJ child shape was validated before allocation"),
                }
            }
        }
    }
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;
    Ok((items, retained))
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "string ownership admission keeps every live plan component and guard input explicit"
)]
fn push_text_string_item(
    items: &mut Vec<TextShowItem>,
    value: &crate::ContentString,
    retained: u64,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    actions: &Vec<ExecutionAction>,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<u64, TextPlanningTerminal> {
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;
    let nested = planned_name_bytes
        .checked_add(retained)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    let other_capacity =
        execution_plan_capacity_bytes(property_uses, image_uses, planned_images, actions, nested)?;
    let (copied, copied_retained) = copy_text_plan_bytes(
        value.bytes(),
        program_bytes,
        other_capacity,
        vm_limits,
        snapshot,
        byte_source,
        cancellation,
        source,
        accounting,
    )?;
    items.push(TextShowItem::Bytes(copied));
    retained
        .checked_add(copied_retained)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source).into())
}

#[allow(
    clippy::result_large_err,
    reason = "text validation preserves structured unsupported and VM guard failures"
)]
fn validate_printable_text(
    bytes: &[u8],
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    source: ContentOperatorSource,
) -> Result<(), TextPlanningTerminal> {
    for chunk in bytes.chunks(256) {
        runtime_guard(snapshot, byte_source, cancellation, Some(source))
            .map_err(TextPlanningTerminal::Failed)?;
        if !chunk.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
            return Err(TextPlanningTerminal::Unsupported(ContentUnsupported::new(
                ContentUnsupportedKind::TextEncoding,
                source,
            )));
        }
    }
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "guarded text copying preserves source-first cancellation precedence during large allocations"
)]
fn copy_text_plan_bytes(
    source_bytes: &[u8],
    program_bytes: u64,
    other_capacity_bytes: u64,
    limits: ContentVmLimits,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<(Vec<u8>, u64), TextPlanningTerminal> {
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;
    let attempted = u64::try_from(source_bytes.len())
        .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
    let consumed = program_bytes
        .checked_add(other_capacity_bytes)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        attempted,
        Some(source),
    )?;
    let mut copied = Vec::new();
    copied.try_reserve_exact(source_bytes.len()).map_err(|_| {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                limits.max_retained_bytes(),
                consumed,
                attempted,
            ),
            Some(source),
        )
    })?;
    let retained = capacity_bytes(&copied)?;
    accounting.observe_retained(consumed.saturating_add(retained));
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        retained,
        Some(source),
    )?;
    for chunk in source_bytes.chunks(256) {
        runtime_guard(snapshot, byte_source, cancellation, Some(source))
            .map_err(TextPlanningTerminal::Failed)?;
        copied.extend_from_slice(chunk);
    }
    runtime_guard(snapshot, byte_source, cancellation, Some(source))
        .map_err(TextPlanningTerminal::Failed)?;
    Ok((copied, retained))
}

#[allow(
    clippy::too_many_arguments,
    reason = "text action growth accounts every independent retained plan component"
)]
fn append_text_action(
    actions: &mut Vec<ExecutionAction>,
    action: TextAction,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<(), ContentVmError> {
    let other_capacity = plan_value_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        planned_name_bytes,
    )?;
    push_execution_action(
        actions,
        ExecutionAction::Text(action),
        program_bytes,
        other_capacity,
        vm_limits,
        source,
        accounting,
    )
}

#[allow(
    clippy::too_many_arguments,
    clippy::result_large_err,
    reason = "implicit Form framing accounts for the complete live execution-plan state"
)]
fn append_form_prologue(
    input: AcquiredContentInput<'_>,
    program: &ContentProgram,
    actions: &mut Vec<ExecutionAction>,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    accounting: &mut Accounting,
) -> Result<Option<FormEnvelope>, ContentVmFailure> {
    let Some(form) = input.form() else {
        return Ok(None);
    };
    let Some(source) = program
        .operators()
        .first()
        .map(|operator| operator.source())
    else {
        return Ok(None);
    };
    vm_limits
        .preflight(ContentVmLimitKind::GraphicsStateDepth, 0, 1, Some(source))
        .map_err(ContentVmFailure::Vm)?;
    let command_source = command_source(source).map_err(ContentVmFailure::Scene)?;
    let matrix = input.initial_ctm();
    let bbox = form.bbox();
    let lower_left = matrix
        .checked_transform_point(ScenePoint::new(
            SceneScalar::from_scaled(bbox.left().scaled()),
            SceneScalar::from_scaled(bbox.bottom().scaled()),
        ))
        .map_err(ContentVmFailure::Scene)?;
    let lower_right = matrix
        .checked_transform_point(ScenePoint::new(
            SceneScalar::from_scaled(bbox.right().scaled()),
            SceneScalar::from_scaled(bbox.bottom().scaled()),
        ))
        .map_err(ContentVmFailure::Scene)?;
    let upper_right = matrix
        .checked_transform_point(ScenePoint::new(
            SceneScalar::from_scaled(bbox.right().scaled()),
            SceneScalar::from_scaled(bbox.top().scaled()),
        ))
        .map_err(ContentVmFailure::Scene)?;
    let upper_left = matrix
        .checked_transform_point(ScenePoint::new(
            SceneScalar::from_scaled(bbox.left().scaled()),
            SceneScalar::from_scaled(bbox.top().scaled()),
        ))
        .map_err(ContentVmFailure::Scene)?;
    let corners = [lower_left, lower_right, upper_right, upper_left];
    let minimum = ScenePoint::new(
        corners.iter().map(|point| point.x()).min().ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?,
        corners.iter().map(|point| point.y()).min().ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?,
    );
    let maximum = ScenePoint::new(
        corners.iter().map(|point| point.x()).max().ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?,
        corners.iter().map(|point| point.y()).max().ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?,
    );
    let bounds = SceneBounds::finite(minimum, maximum).map_err(ContentVmFailure::Scene)?;
    let mut path = PathResourceBuilder::new();
    path.try_reserve_exact(5).map_err(ContentVmFailure::Scene)?;
    path.try_push(PathSegment::MoveTo(lower_left))
        .map_err(ContentVmFailure::Scene)?;
    path.try_push(PathSegment::LineTo(lower_right))
        .map_err(ContentVmFailure::Scene)?;
    path.try_push(PathSegment::LineTo(upper_right))
        .map_err(ContentVmFailure::Scene)?;
    path.try_push(PathSegment::LineTo(upper_left))
        .map_err(ContentVmFailure::Scene)?;
    path.try_push(PathSegment::ClosePath)
        .map_err(ContentVmFailure::Scene)?;
    let retained_bytes = path.retained_bytes().map_err(ContentVmFailure::Scene)?;
    let base_capacity = execution_plan_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        actions,
        planned_name_bytes,
    )
    .map_err(ContentVmFailure::Vm)?;
    let total = program_bytes
        .checked_add(base_capacity)
        .and_then(|value| value.checked_add(retained_bytes))
        .ok_or_else(|| ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source)))?;
    vm_limits
        .preflight(ContentVmLimitKind::RetainedBytes, 0, total, Some(source))
        .map_err(ContentVmFailure::Vm)?;
    accounting.observe_retained(total);
    let other_capacity = plan_value_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        planned_name_bytes,
    )
    .and_then(|value| {
        value
            .checked_add(retained_bytes)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))
    })
    .map_err(ContentVmFailure::Vm)?;
    push_execution_action(
        actions,
        ExecutionAction::Save {
            bounds,
            source: command_source,
        },
        program_bytes,
        other_capacity,
        vm_limits,
        source,
        accounting,
    )
    .map_err(ContentVmFailure::Vm)?;
    push_execution_action(
        actions,
        ExecutionAction::Clip {
            path: path.finish(),
            rule: FillRule::Nonzero,
            transform: Matrix::IDENTITY,
            bounds,
            source: command_source,
        },
        program_bytes,
        other_capacity,
        vm_limits,
        source,
        accounting,
    )
    .map_err(ContentVmFailure::Vm)?;
    if form.simple_transparency_group() {
        push_execution_action(
            actions,
            ExecutionAction::BeginGroup {
                alpha: SceneUnit::ONE,
                blend_mode: pdf_rs_scene::BlendMode::Normal,
                bounds: SceneBounds::Page,
                source: command_source,
            },
            program_bytes,
            other_capacity,
            vm_limits,
            source,
            accounting,
        )
        .map_err(ContentVmFailure::Vm)?;
    }
    accounting.max_graphics_depth = accounting.max_graphics_depth.max(1);
    Ok(Some(FormEnvelope {
        source,
        command_source,
        bounds,
        simple_transparency_group: form.simple_transparency_group(),
        retained_bytes,
    }))
}

#[allow(
    clippy::too_many_arguments,
    reason = "implicit Form framing accounts for the complete live execution-plan state"
)]
fn append_form_epilogue(
    envelope: FormEnvelope,
    actions: &mut Vec<ExecutionAction>,
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    accounting: &mut Accounting,
) -> Result<(), ContentVmError> {
    let other_capacity = plan_value_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        planned_name_bytes,
    )?
    .checked_add(envelope.retained_bytes)
    .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, envelope.source))?;
    if envelope.simple_transparency_group {
        push_execution_action(
            actions,
            ExecutionAction::EndGroup {
                bounds: SceneBounds::Page,
                source: envelope.command_source,
            },
            program_bytes,
            other_capacity,
            vm_limits,
            envelope.source,
            accounting,
        )?;
    }
    push_execution_action(
        actions,
        ExecutionAction::Restore {
            bounds: envelope.bounds,
            source: envelope.command_source,
        },
        program_bytes,
        other_capacity,
        vm_limits,
        envelope.source,
        accounting,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "execution keeps source guards and independent sealed budgets explicit"
)]
fn build_execution_plan(
    input: AcquiredContentInput<'_>,
    program: &ContentProgram,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    profile: ContentVmProfile,
    mut image_runtime: Option<&mut ImageRuntime>,
    mut font_runtime: Option<&mut FontRuntime>,
    ext_gstate_profile: Option<&ContentExtGStateProfile>,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    accounting: &mut Accounting,
) -> ExecutionReport {
    let snapshot = input.snapshot();
    let mut resolver = input.resources().property_resolver(property_limits);
    let mut property_uses = Vec::new();
    let mut planned_images = Vec::new();
    let mut image_uses = Vec::new();
    let mut planned_name_bytes = 0_u64;
    let mut planned_font_uses = 0_u64;
    let mut first_font_source = None;
    let terminal = (|| {
        let (binding, geometry) = match input.scene_context() {
            Ok(value) => value,
            Err(error) => {
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    None,
                    RunTerminal::Failed(ContentVmFailure::Scene(error)),
                );
            }
        };
        let mut actions = Vec::new();
        let mut graphics = Vec::new();
        let mut graphics_v2 = match profile {
            ContentVmProfile::SceneV1 { .. } => None,
            ContentVmProfile::GraphicsV2 { .. } => Some(GraphicsVm::new()),
        };
        let mut current_ctm = input.initial_ctm();
        if let Some(machine) = graphics_v2.as_mut() {
            machine.set_ctm(current_ctm);
        }
        let form_envelope = match append_form_prologue(
            input,
            program,
            &mut actions,
            &property_uses,
            &image_uses,
            &planned_images,
            planned_name_bytes,
            program_bytes,
            vm_limits,
            accounting,
        ) {
            Ok(value) => value,
            Err(ContentVmFailure::Vm(error)) => {
                return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
            }
            Err(ContentVmFailure::Scene(error)) => {
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    program
                        .operators()
                        .first()
                        .map(|operator| operator.source()),
                    RunTerminal::Failed(ContentVmFailure::Scene(error)),
                );
            }
            Err(failure) => return RunTerminal::Failed(failure),
        };
        let mut text_active = false;
        let mut text_state = TextPlanningState::default();
        let mut saved_text_states = Vec::new();
        let mut compatibility_depth = 0_u32;
        let mut marked_depth = 0_u32;

        for operator in program.operators() {
            let operator_source = operator.source();
            if let Err(failure) =
                runtime_guard(snapshot, byte_source, cancellation, Some(operator_source))
            {
                return RunTerminal::Failed(failure);
            }
            if let Some(runtime) = image_runtime.as_deref_mut()
                && let Err(error) = runtime.admit_planning_operator(operator_source)
            {
                return prioritize_vm(snapshot, byte_source, cancellation, operator_source, error);
            }
            if let Some(runtime) = font_runtime.as_deref_mut()
                && let Err(error) = runtime.admit_planning_operator(operator_source)
            {
                return prioritize_vm(snapshot, byte_source, cancellation, operator_source, error);
            }
            let Some(kind) = operator.operator().known() else {
                if let Err(error) = admit_operator(accounting, vm_limits, 1, operator_source) {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        error,
                    );
                }
                if compatibility_depth != 0 {
                    continue;
                }
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    Some(operator_source),
                    RunTerminal::Unsupported(ContentUnsupported::new(
                        ContentUnsupportedKind::UnknownOperator,
                        operator_source,
                    )),
                );
            };

            if let Err(error) =
                validate_operand_structure(kind, operator.operands(), operator_source)
            {
                return prioritize_vm(snapshot, byte_source, cancellation, operator_source, error);
            }
            if matches!(profile, ContentVmProfile::GraphicsV2 { .. })
                && let Err(error) = validate_operator_context(kind, text_active, operator_source)
            {
                return prioritize_vm(snapshot, byte_source, cancellation, operator_source, error);
            }
            let validated = if kind == OperatorKind::SetLineDash {
                let (dash_values, dash_phase) = dash_operands(operator.operands());
                let dash_entries = match u64::try_from(dash_values.len()) {
                    Ok(value) => value,
                    Err(_) => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InternalState, operator_source),
                        );
                    }
                };
                let fuel = match u64::from(kind.spec().base_fuel()).checked_add(dash_entries) {
                    Some(value) => value,
                    None => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InternalState, operator_source),
                        );
                    }
                };
                if let Err(error) = admit_operator(accounting, vm_limits, fuel, operator_source) {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        error,
                    );
                }

                let ContentVmProfile::GraphicsV2 {
                    graphics_limits, ..
                } = profile
                else {
                    if let Err(error) = validate_legacy_dash_operands(
                        dash_values,
                        dash_phase,
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    return prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(operator_source),
                        RunTerminal::Unsupported(ContentUnsupported::new(
                            ContentUnsupportedKind::GraphicsV2Operator,
                            operator_source,
                        )),
                    );
                };
                let use_bytes = execution_plan_capacity_bytes(
                    &property_uses,
                    &image_uses,
                    &planned_images,
                    &actions,
                    planned_name_bytes,
                )
                .unwrap_or(u64::MAX);
                let retention = VmRetention::new(program_bytes, use_bytes, vm_limits);
                let expected_bytes = match byte_width::<SceneScalar>(dash_values.len()) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error.with_source(operator_source),
                        );
                    }
                };
                let admission = match graphics_v2
                    .as_ref()
                    .expect("graphics-v2 profile owns graphics VM state")
                    .preflight_dash_candidate(
                        dash_entries,
                        expected_bytes,
                        graphics_limits,
                        retention,
                        operator_source,
                    ) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                };
                let pattern = match convert_dash_operands(
                    dash_values,
                    dash_phase,
                    admission,
                    expected_bytes,
                    snapshot,
                    byte_source,
                    cancellation,
                    operator_source,
                    accounting,
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                };
                ValidatedOperands::Dash { pattern }
            } else {
                let validated = match convert_operands(kind, operator.operands(), operator_source) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                };
                let fuel = match u64::from(kind.spec().base_fuel())
                    .checked_add(validated.dynamic_fuel(kind))
                {
                    Some(value) => value,
                    None => {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InternalState, operator_source),
                        );
                    }
                };
                if let Err(error) = admit_operator(accounting, vm_limits, fuel, operator_source) {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        error,
                    );
                }
                validated
            };

            if is_text_operator(kind) {
                if let Err(terminal) = plan_text_operator(
                    kind,
                    &validated,
                    snapshot,
                    byte_source,
                    cancellation,
                    font_runtime.as_deref_mut(),
                    graphics_v2.as_ref(),
                    &mut text_active,
                    &mut text_state,
                    &mut planned_font_uses,
                    &mut actions,
                    &property_uses,
                    &image_uses,
                    &planned_images,
                    &mut planned_name_bytes,
                    program_bytes,
                    vm_limits,
                    operator_source,
                    accounting,
                ) {
                    let fallback = match terminal {
                        TextPlanningTerminal::Failed(failure) => RunTerminal::Failed(failure),
                        TextPlanningTerminal::Unsupported(unsupported) => {
                            RunTerminal::Unsupported(unsupported)
                        }
                    };
                    return prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(operator_source),
                        fallback,
                    );
                }
                if kind == OperatorKind::SetTextFont && first_font_source.is_none() {
                    first_font_source = Some(operator_source);
                }
                continue;
            }

            match kind {
                OperatorKind::SetGraphicsState => {
                    let Some(graphics) = graphics_v2.as_mut() else {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::GraphicsV2Operator,
                                operator_source,
                            )),
                        );
                    };
                    let ValidatedOperands::Name(name) = validated else {
                        unreachable!("validated gs operands have name shape");
                    };
                    let Some(profile) = ext_gstate_profile else {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::ExtGStateProfileRequired,
                                operator_source,
                            )),
                        );
                    };
                    let Some(resource) = profile.find(name.bytes()) else {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::ExtGStateResource,
                                operator_source,
                            )),
                        );
                    };
                    graphics.apply_ext_gstate(
                        resource.stroking_alpha(),
                        resource.nonstroking_alpha(),
                        resource.blend_mode(),
                    );
                }
                OperatorKind::SaveGraphicsState => {
                    let saved_len = graphics_v2
                        .as_ref()
                        .map_or(graphics.len(), |machine| machine.saved().len());
                    let graphics_depth = match u64::try_from(saved_len) {
                        Ok(value) => value,
                        Err(_) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    let graphics_depth =
                        match graphics_depth.checked_add(u64::from(form_envelope.is_some())) {
                            Some(value) => value,
                            None => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    vm_error(ContentVmErrorCode::InternalState, operator_source),
                                );
                            }
                        };
                    if let Err(error) = vm_limits.preflight(
                        ContentVmLimitKind::GraphicsStateDepth,
                        graphics_depth,
                        1,
                        Some(operator_source),
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let use_bytes = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX);
                    let retained_result = match graphics_v2.as_mut() {
                        Some(machine) => machine.reserve_saved_slot(
                            VmRetention::new(program_bytes, use_bytes, vm_limits),
                            operator_source,
                            accounting,
                        ),
                        None => reserve_vm_slot(
                            &mut graphics,
                            program_bytes,
                            use_bytes,
                            vm_limits,
                            operator_source,
                            accounting,
                        ),
                    };
                    let retained = match retained_result {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    accounting.observe_retained(retained);
                    if let Some(machine) = graphics_v2.as_mut() {
                        let command_source = match command_source(operator_source) {
                            Ok(value) => value,
                            Err(error) => {
                                return prioritize_scene(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                );
                            }
                        };
                        let other_capacity = plan_value_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| {
                            value
                                .checked_add(machine.retained_capacity_bytes(operator_source).ok()?)
                        })
                        .unwrap_or(u64::MAX);
                        if let Err(error) = reserve_vm_slot(
                            &mut actions,
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                            accounting,
                        ) {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                        actions.push(ExecutionAction::Save {
                            bounds: SceneBounds::Empty,
                            source: command_source,
                        });
                        machine.push_current();
                    } else {
                        graphics.push(current_ctm);
                    }
                    let graphics_capacity = graphics_v2
                        .as_ref()
                        .and_then(|machine| machine.retained_capacity_bytes(operator_source).ok())
                        .or_else(|| capacity_bytes(&graphics).ok())
                        .unwrap_or(u64::MAX);
                    let plan_capacity = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX);
                    let old_text_stack_bytes =
                        capacity_bytes(&saved_text_states).unwrap_or(u64::MAX);
                    if let Err(error) = reserve_vm_slot(
                        &mut saved_text_states,
                        program_bytes,
                        graphics_capacity.saturating_add(plan_capacity),
                        vm_limits,
                        operator_source,
                        accounting,
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let text_stack_bytes = capacity_bytes(&saved_text_states).unwrap_or(u64::MAX);
                    planned_name_bytes = match planned_name_bytes
                        .checked_add(text_stack_bytes.saturating_sub(old_text_stack_bytes))
                    {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    saved_text_states.push(text_state);
                    let saved_len = graphics_v2
                        .as_ref()
                        .map_or(graphics.len(), |machine| machine.saved().len());
                    let graphics_depth = match u32::try_from(saved_len) {
                        Ok(value) => value,
                        Err(_) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    let graphics_depth =
                        match graphics_depth.checked_add(u32::from(form_envelope.is_some())) {
                            Some(value) => value,
                            None => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    vm_error(ContentVmErrorCode::InternalState, operator_source),
                                );
                            }
                        };
                    accounting.max_graphics_depth =
                        accounting.max_graphics_depth.max(graphics_depth);
                }
                OperatorKind::RestoreGraphicsState => {
                    if let Some(machine) = graphics_v2.as_mut() {
                        if machine.saved().is_empty() {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InvalidGraphicsState, operator_source),
                            );
                        }
                        let command_source = match command_source(operator_source) {
                            Ok(value) => value,
                            Err(error) => {
                                return prioritize_scene(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                );
                            }
                        };
                        let other_capacity = plan_value_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| {
                            value
                                .checked_add(machine.retained_capacity_bytes(operator_source).ok()?)
                        })
                        .unwrap_or(u64::MAX);
                        if let Err(error) = reserve_vm_slot(
                            &mut actions,
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                            accounting,
                        ) {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                        actions.push(ExecutionAction::Restore {
                            bounds: SceneBounds::Empty,
                            source: command_source,
                        });
                        current_ctm = match machine.restore(operator_source) {
                            Ok(Some(value)) => value,
                            Ok(None) => {
                                unreachable!("validated graphics-v2 restore has saved state");
                            }
                            Err(error) => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                );
                            }
                        };
                    } else {
                        let Some(restored) = graphics.pop() else {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InvalidGraphicsState, operator_source),
                            );
                        };
                        current_ctm = restored;
                    }
                    let Some(restored_text_state) = saved_text_states.pop() else {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InternalState, operator_source),
                        );
                    };
                    text_state = restored_text_state;
                }
                OperatorKind::ConcatMatrix => {
                    let ValidatedOperands::SixNumbers(numbers) = validated else {
                        unreachable!("validated cm operands have matrix shape");
                    };
                    let operand = Matrix::new(
                        numbers.map(|number| SceneScalar::from_scaled(number.scaled())),
                    );
                    current_ctm = match current_ctm.checked_multiply(operand) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_scene(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    if let Some(machine) = graphics_v2.as_mut() {
                        machine.set_ctm(current_ctm);
                    }
                }
                OperatorKind::BeginText
                | OperatorKind::EndText
                | OperatorKind::SetCharacterSpacing
                | OperatorKind::SetWordSpacing
                | OperatorKind::SetHorizontalScaling
                | OperatorKind::SetTextLeading
                | OperatorKind::SetTextFont
                | OperatorKind::SetTextRenderMode
                | OperatorKind::SetTextRise
                | OperatorKind::MoveTextPosition
                | OperatorKind::MoveTextPositionSetLeading
                | OperatorKind::SetTextMatrix
                | OperatorKind::MoveToNextTextLine
                | OperatorKind::ShowText
                | OperatorKind::ShowTextAdjusted
                | OperatorKind::MoveNextLineShowText
                | OperatorKind::SetSpacingMoveNextLineShowText => {
                    unreachable!("text operators are sealed by the text planner")
                }
                OperatorKind::BeginCompatibility => {
                    if let Err(error) = vm_limits.preflight(
                        ContentVmLimitKind::CompatibilityDepth,
                        u64::from(compatibility_depth),
                        1,
                        Some(operator_source),
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    compatibility_depth = match compatibility_depth.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    accounting.max_compatibility_depth =
                        accounting.max_compatibility_depth.max(compatibility_depth);
                }
                OperatorKind::EndCompatibility => {
                    if compatibility_depth == 0 {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(
                                ContentVmErrorCode::InvalidCompatibilityState,
                                operator_source,
                            ),
                        );
                    }
                    compatibility_depth -= 1;
                }
                OperatorKind::MarkedContentPoint => {
                    return prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(operator_source),
                        RunTerminal::Unsupported(ContentUnsupported::new(
                            ContentUnsupportedKind::MarkedContentPoint,
                            operator_source,
                        )),
                    );
                }
                OperatorKind::MarkedContentPointProperties => {
                    return prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(operator_source),
                        RunTerminal::Unsupported(ContentUnsupported::new(
                            ContentUnsupportedKind::MarkedContentPointProperties,
                            operator_source,
                        )),
                    );
                }
                OperatorKind::BeginMarkedContent => {
                    let ValidatedOperands::Name(tag) = validated else {
                        unreachable!("validated BMC operands have name shape");
                    };
                    if let Err(error) =
                        preflight_marked_depth(marked_depth, vm_limits, operator_source)
                    {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let command_source = match command_source(operator_source) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_scene(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    if matches!(profile, ContentVmProfile::SceneV1 { .. }) {
                        let other_capacity = execution_plan_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            &actions,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| value.checked_add(capacity_bytes(&graphics).ok()?))
                        .unwrap_or(u64::MAX);
                        let (tag, retained) = match copy_plan_bytes(
                            tag.bytes(),
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                        ) {
                            Ok(value) => value,
                            Err(error) => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                );
                            }
                        };
                        planned_name_bytes = match planned_name_bytes.checked_add(retained) {
                            Some(value) => value,
                            None => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    vm_error(ContentVmErrorCode::InternalState, operator_source),
                                );
                            }
                        };
                        let other_capacity = plan_value_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| value.checked_add(capacity_bytes(&graphics).ok()?))
                        .unwrap_or(u64::MAX);
                        if let Err(error) = reserve_vm_slot(
                            &mut actions,
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                            accounting,
                        ) {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                        actions.push(ExecutionAction::BeginMarkedContent {
                            tag,
                            properties: None,
                            source: command_source,
                        });
                    }
                    marked_depth = match marked_depth.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    accounting.max_marked_depth = accounting.max_marked_depth.max(marked_depth);
                }
                OperatorKind::BeginMarkedContentProperties => {
                    let ValidatedOperands::NameAndProperty { tag, property } = validated else {
                        unreachable!("validated BDC operands have tag/property shape");
                    };
                    let PropertyOperand::Name(property_name) = property else {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::DirectContentPropertyDictionary,
                                operator_source,
                            )),
                        );
                    };
                    if let Err(error) =
                        preflight_marked_depth(marked_depth, vm_limits, operator_source)
                    {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    if let Err(error) = vm_limits.preflight(
                        ContentVmLimitKind::PropertyUses,
                        accounting.property_uses,
                        1,
                        Some(operator_source),
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let graphics_capacity = graphics_v2
                        .as_ref()
                        .map_or_else(
                            || capacity_bytes(&graphics),
                            |machine| machine.retained_capacity_bytes(operator_source),
                        )
                        .unwrap_or(u64::MAX);
                    let plan_capacity = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX)
                    .saturating_sub(capacity_bytes(&property_uses).unwrap_or(u64::MAX));
                    let other_capacity = graphics_capacity.saturating_add(plan_capacity);
                    let retained = match reserve_vm_slot(
                        &mut property_uses,
                        program_bytes,
                        other_capacity,
                        vm_limits,
                        operator_source,
                        accounting,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    accounting.observe_retained(retained);
                    if let Err(failure) =
                        runtime_guard(snapshot, byte_source, cancellation, Some(operator_source))
                    {
                        return RunTerminal::Failed(failure);
                    }
                    let proof = match resolver.lookup_marked_content_property(
                        property_name.bytes(),
                        byte_source,
                        cancellation,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            let terminal =
                                match ContentUnsupported::from_document(error, operator_source) {
                                    Some(unsupported) => RunTerminal::Unsupported(unsupported),
                                    None => RunTerminal::Failed(ContentVmFailure::Document(error)),
                                };
                            return prioritize(
                                snapshot,
                                byte_source,
                                cancellation,
                                Some(operator_source),
                                terminal,
                            );
                        }
                    };
                    if let Err(failure) =
                        runtime_guard(snapshot, byte_source, cancellation, Some(operator_source))
                    {
                        return RunTerminal::Failed(failure);
                    }
                    let command_source = match command_source(operator_source) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_scene(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    if matches!(profile, ContentVmProfile::SceneV1 { .. }) {
                        let other_capacity = execution_plan_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            &actions,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| value.checked_add(graphics_capacity))
                        .unwrap_or(u64::MAX);
                        let (tag, retained) = match copy_plan_bytes(
                            tag.bytes(),
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                        ) {
                            Ok(value) => value,
                            Err(error) => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                );
                            }
                        };
                        planned_name_bytes = match planned_name_bytes.checked_add(retained) {
                            Some(value) => value,
                            None => {
                                return prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    vm_error(ContentVmErrorCode::InternalState, operator_source),
                                );
                            }
                        };
                        let other_capacity = plan_value_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| value.checked_add(graphics_capacity))
                        .unwrap_or(u64::MAX);
                        if let Err(error) = reserve_vm_slot(
                            &mut actions,
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                            accounting,
                        ) {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                        actions.push(ExecutionAction::BeginMarkedContent {
                            tag,
                            properties: Some(proof.target()),
                            source: command_source,
                        });
                    }
                    property_uses.push(ResolvedPropertyUse::new(operator_source, proof));
                    accounting.property_uses = match accounting.property_uses.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    marked_depth = match marked_depth.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    accounting.max_marked_depth = accounting.max_marked_depth.max(marked_depth);
                }
                OperatorKind::EndMarkedContent => {
                    if marked_depth == 0 {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(
                                ContentVmErrorCode::InvalidMarkedContentState,
                                operator_source,
                            ),
                        );
                    }
                    let command_source = match command_source(operator_source) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_scene(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    if matches!(profile, ContentVmProfile::SceneV1 { .. }) {
                        let other_capacity = plan_value_capacity_bytes(
                            &property_uses,
                            &image_uses,
                            &planned_images,
                            planned_name_bytes,
                        )
                        .ok()
                        .and_then(|value| value.checked_add(capacity_bytes(&graphics).ok()?))
                        .unwrap_or(u64::MAX);
                        if let Err(error) = reserve_vm_slot(
                            &mut actions,
                            program_bytes,
                            other_capacity,
                            vm_limits,
                            operator_source,
                            accounting,
                        ) {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                        actions.push(ExecutionAction::EndMarkedContent {
                            source: command_source,
                        });
                    }
                    marked_depth -= 1;
                }
                OperatorKind::PaintXObject => {
                    if matches!(profile, ContentVmProfile::SceneV1 { .. }) {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::GraphicsV2Operator,
                                operator_source,
                            )),
                        );
                    }
                    let Some(runtime) = image_runtime.as_deref_mut() else {
                        return prioritize(
                            snapshot,
                            byte_source,
                            cancellation,
                            Some(operator_source),
                            RunTerminal::Unsupported(ContentUnsupported::new(
                                ContentUnsupportedKind::ImageProfileRequired,
                                operator_source,
                            )),
                        );
                    };
                    let ValidatedOperands::Name(name) = validated else {
                        unreachable!("validated Do operands have name shape");
                    };
                    if let Err(error) =
                        runtime.admit_planned_use(accounting.image_uses, operator_source)
                    {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let machine = graphics_v2
                        .as_ref()
                        .expect("graphics-v2 profile owns graphics VM state");
                    let transform = machine.current_ctm();
                    let paint = machine.image_paint();
                    let command_source = match command_source(operator_source) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_scene(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    let graphics_capacity = machine
                        .retained_capacity_bytes(operator_source)
                        .unwrap_or(u64::MAX);
                    let plan_capacity = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX)
                    .saturating_sub(capacity_bytes(&image_uses).unwrap_or(u64::MAX));
                    let planned_use_slots = accounting
                        .image_uses
                        .checked_add(1)
                        .and_then(|value| usize::try_from(value).ok());
                    let Some(planned_use_slots) = planned_use_slots else {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InternalState, operator_source),
                        );
                    };
                    let retained = match reserve_vm_additional(
                        &mut image_uses,
                        planned_use_slots,
                        program_bytes,
                        graphics_capacity.saturating_add(plan_capacity),
                        vm_limits,
                        operator_source,
                        accounting,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    accounting.observe_retained(retained);
                    let plan_capacity = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX)
                    .saturating_sub(capacity_bytes(&planned_images).unwrap_or(u64::MAX));
                    let retained = match reserve_vm_slot(
                        &mut planned_images,
                        program_bytes,
                        graphics_capacity.saturating_add(plan_capacity),
                        vm_limits,
                        operator_source,
                        accounting,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    accounting.observe_retained(retained);
                    let other_capacity = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .ok()
                    .and_then(|value| value.checked_add(graphics_capacity))
                    .unwrap_or(u64::MAX);
                    let (planned_name, retained) = match copy_plan_bytes(
                        name.bytes(),
                        program_bytes,
                        other_capacity,
                        vm_limits,
                        operator_source,
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                error,
                            );
                        }
                    };
                    planned_name_bytes = match planned_name_bytes.checked_add(retained) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    let other_capacity = plan_value_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        planned_name_bytes,
                    )
                    .ok()
                    .and_then(|value| value.checked_add(graphics_capacity))
                    .unwrap_or(u64::MAX);
                    if let Err(error) = reserve_vm_slot(
                        &mut actions,
                        program_bytes,
                        other_capacity,
                        vm_limits,
                        operator_source,
                        accounting,
                    ) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    let next_uses = match accounting.image_uses.checked_add(1) {
                        Some(value) => value,
                        None => {
                            return prioritize_vm(
                                snapshot,
                                byte_source,
                                cancellation,
                                operator_source,
                                vm_error(ContentVmErrorCode::InternalState, operator_source),
                            );
                        }
                    };
                    planned_images.push(PlannedImageInvocation {
                        source: operator_source,
                        name: planned_name,
                        transform,
                    });
                    actions.push(ExecutionAction::DrawImage {
                        source: operator_source,
                        command_source,
                        transform,
                        alpha: paint.alpha(),
                        blend_mode: paint.blend_mode(),
                        bounds: SceneBounds::Page,
                    });
                    accounting.image_uses = next_uses;
                }
                OperatorKind::MoveTo
                | OperatorKind::LineTo
                | OperatorKind::CubicCurveTo
                | OperatorKind::CubicCurveToReplicateInitial
                | OperatorKind::CubicCurveToReplicateFinal
                | OperatorKind::ClosePath
                | OperatorKind::Rectangle
                | OperatorKind::StrokePath
                | OperatorKind::CloseAndStrokePath
                | OperatorKind::FillNonzero
                | OperatorKind::FillNonzeroLegacy
                | OperatorKind::FillEvenOdd
                | OperatorKind::FillStrokeNonzero
                | OperatorKind::FillStrokeEvenOdd
                | OperatorKind::CloseFillStrokeNonzero
                | OperatorKind::CloseFillStrokeEvenOdd
                | OperatorKind::EndPath
                | OperatorKind::ClipNonzero
                | OperatorKind::ClipEvenOdd
                | OperatorKind::SetLineWidth
                | OperatorKind::SetLineCap
                | OperatorKind::SetLineJoin
                | OperatorKind::SetMiterLimit
                | OperatorKind::SetLineDash
                | OperatorKind::SetStrokingGray
                | OperatorKind::SetNonstrokingGray
                | OperatorKind::SetStrokingRgb
                | OperatorKind::SetNonstrokingRgb
                | OperatorKind::SetStrokingCmyk
                | OperatorKind::SetNonstrokingCmyk => {
                    let graphics_limits = match profile {
                        ContentVmProfile::SceneV1 { .. } => {
                            return prioritize(
                                snapshot,
                                byte_source,
                                cancellation,
                                Some(operator_source),
                                RunTerminal::Unsupported(ContentUnsupported::new(
                                    ContentUnsupportedKind::GraphicsV2Operator,
                                    operator_source,
                                )),
                            );
                        }
                        ContentVmProfile::GraphicsV2 {
                            graphics_limits, ..
                        } => graphics_limits,
                    };
                    let machine = graphics_v2
                        .as_mut()
                        .expect("graphics-v2 profile owns graphics VM state");
                    let action_other_capacity = plan_value_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX);
                    let use_bytes = execution_plan_capacity_bytes(
                        &property_uses,
                        &image_uses,
                        &planned_images,
                        &actions,
                        planned_name_bytes,
                    )
                    .unwrap_or(u64::MAX);
                    let retention = VmRetention::new(program_bytes, use_bytes, vm_limits);
                    match machine.execute(
                        kind,
                        &validated,
                        graphics_limits,
                        retention,
                        action_other_capacity,
                        &mut actions,
                        operator_source,
                        accounting,
                    ) {
                        Ok(retained) => accounting.observe_retained(retained),
                        Err(error) => {
                            return match error {
                                GraphicsExecutionError::Vm(error) => prioritize_vm(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                ),
                                GraphicsExecutionError::Scene(error) => prioritize_scene(
                                    snapshot,
                                    byte_source,
                                    cancellation,
                                    operator_source,
                                    error,
                                ),
                            };
                        }
                    }
                }
            }
        }

        let graphics_unbalanced = graphics_v2
            .as_ref()
            .map_or(!graphics.is_empty(), |machine| !machine.saved().is_empty());
        for (unbalanced, code) in [
            (
                graphics_unbalanced,
                ContentVmErrorCode::InvalidGraphicsState,
            ),
            (text_active, ContentVmErrorCode::InvalidTextObject),
            (
                compatibility_depth != 0,
                ContentVmErrorCode::InvalidCompatibilityState,
            ),
            (
                marked_depth != 0,
                ContentVmErrorCode::InvalidMarkedContentState,
            ),
        ] {
            if unbalanced {
                let error = ContentVmError::new(code, None);
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    None,
                    RunTerminal::Failed(ContentVmFailure::Vm(error)),
                );
            }
        }
        if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, None) {
            return RunTerminal::Failed(failure);
        }
        if let Some(envelope) = form_envelope
            && let Err(error) = append_form_epilogue(
                envelope,
                &mut actions,
                &property_uses,
                &image_uses,
                &planned_images,
                planned_name_bytes,
                program_bytes,
                vm_limits,
                accounting,
            )
        {
            return prioritize_vm(snapshot, byte_source, cancellation, envelope.source, error);
        }
        let final_ctm = graphics_v2
            .as_ref()
            .map_or(current_ctm, GraphicsVm::current_ctm);
        let graphics_action_payload_retained_bytes = match graphics_v2
            .as_ref()
            .map_or(Some(0), GraphicsVm::action_payload_retained_bytes)
        {
            Some(value) => value,
            None => {
                return prioritize_vm_without_source(
                    snapshot,
                    byte_source,
                    cancellation,
                    ContentVmError::new(ContentVmErrorCode::InternalState, None),
                );
            }
        };
        let action_payload_retained_bytes = match graphics_action_payload_retained_bytes
            .checked_add(form_envelope.map_or(0, |envelope| envelope.retained_bytes))
        {
            Some(value) => value,
            None => {
                return prioritize_vm_without_source(
                    snapshot,
                    byte_source,
                    cancellation,
                    ContentVmError::new(ContentVmErrorCode::InternalState, None),
                );
            }
        };
        let planning_text_stack_bytes = match capacity_bytes(&saved_text_states) {
            Ok(value) => value,
            Err(error) => {
                return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
            }
        };
        let owned_name_retained_bytes =
            match planned_name_bytes.checked_sub(planning_text_stack_bytes) {
                Some(value) => value,
                None => {
                    return prioritize_vm_without_source(
                        snapshot,
                        byte_source,
                        cancellation,
                        ContentVmError::new(ContentVmErrorCode::InternalState, None),
                    );
                }
            };
        let plan = ExecutionPlan {
            binding,
            geometry,
            actions,
            image_invocations: planned_images,
            property_uses,
            image_uses,
            font_use_count: match usize::try_from(planned_font_uses) {
                Ok(value) => value,
                Err(_) => {
                    return prioritize_vm_without_source(
                        snapshot,
                        byte_source,
                        cancellation,
                        ContentVmError::new(ContentVmErrorCode::InternalState, None),
                    );
                }
            },
            first_font_source,
            final_ctm,
            action_payload_retained_bytes,
            owned_name_retained_bytes,
            accounting: *accounting,
            property_stats: resolver.stats(),
        };
        if let Some(runtime) = image_runtime {
            let source = plan.image_invocations.first().map(|image| image.source);
            let retained = match plan.image_plan_retained_bytes() {
                Ok(value) => value,
                Err(error) => {
                    return prioritize_vm_without_source(
                        snapshot,
                        byte_source,
                        cancellation,
                        error,
                    );
                }
            };
            if let Err(error) = runtime.record_execution_plan_retained(retained, source) {
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    source,
                    RunTerminal::Failed(ContentVmFailure::Vm(error)),
                );
            }
        }
        if let Some(runtime) = font_runtime {
            let source = plan.first_font_source;
            let retained = match plan.font_plan_retained_bytes() {
                Ok(value) => value,
                Err(error) => {
                    return prioritize_vm_without_source(
                        snapshot,
                        byte_source,
                        cancellation,
                        error,
                    );
                }
            };
            if let Err(error) = runtime.record_execution_plan_retained(retained, source) {
                return prioritize(
                    snapshot,
                    byte_source,
                    cancellation,
                    source,
                    RunTerminal::Failed(ContentVmFailure::Vm(error)),
                );
            }
        }
        RunTerminal::Planned(plan)
    })();

    ExecutionReport {
        terminal,
        property_stats: resolver.stats(),
    }
}

#[derive(Clone)]
struct TextParameters {
    character_spacing: SceneScalar,
    word_spacing: SceneScalar,
    horizontal_scaling: SceneScalar,
    leading: SceneScalar,
    font: Option<Arc<pdf_rs_document::AcquiredFontResource>>,
    font_size: SceneScalar,
    render_mode: i64,
    rise: SceneScalar,
}

impl Default for TextParameters {
    fn default() -> Self {
        Self {
            character_spacing: SceneScalar::ZERO,
            word_spacing: SceneScalar::ZERO,
            horizontal_scaling: SceneScalar::ONE,
            leading: SceneScalar::ZERO,
            font: None,
            font_size: SceneScalar::ZERO,
            render_mode: 0,
            rise: SceneScalar::ZERO,
        }
    }
}

struct TextExecutor {
    parameters: TextParameters,
    saved_parameters: Vec<TextParameters>,
    text_matrix: Matrix,
    line_matrix: Matrix,
    active: bool,
    vm_limits: ContentVmLimits,
    materialization_base_retained: u64,
    peak_glyph_retained: u64,
}

impl TextExecutor {
    fn new(
        saved_slots: usize,
        vm_limits: ContentVmLimits,
        consumed: u64,
        materialization_peak: &mut u64,
    ) -> Result<Self, ContentVmError> {
        let mut saved_parameters = Vec::new();
        reserve_exact_slots_observed(
            &mut saved_parameters,
            saved_slots,
            consumed,
            vm_limits,
            None,
            materialization_peak,
        )?;
        let materialization_base_retained = consumed
            .checked_add(capacity_bytes(&saved_parameters)?)
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))?;
        Ok(Self {
            parameters: TextParameters::default(),
            saved_parameters,
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            active: false,
            vm_limits,
            materialization_base_retained,
            peak_glyph_retained: 0,
        })
    }

    fn save_parameters(&mut self, source: ContentOperatorSource) -> Result<(), ContentVmError> {
        if self.saved_parameters.len() == self.saved_parameters.capacity() {
            return Err(vm_error(ContentVmErrorCode::InternalState, source));
        }
        self.saved_parameters.push(self.parameters.clone());
        Ok(())
    }

    fn peak_retained_bytes(&self) -> u64 {
        self.materialization_base_retained
            .saturating_add(self.peak_glyph_retained)
    }

    fn observe_glyph_retained(&mut self, retained: u64, materialization_peak: &mut u64) {
        self.peak_glyph_retained = self.peak_glyph_retained.max(retained);
        *materialization_peak = (*materialization_peak).max(self.peak_retained_bytes());
    }

    fn preflight_glyph_candidate(
        &self,
        runtime: &FontRuntime,
        consumed: u64,
        attempted: u64,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        runtime.preflight_glyph_retained(consumed, attempted, source)?;
        let vm_consumed = self
            .materialization_base_retained
            .checked_add(consumed)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        self.vm_limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            vm_consumed,
            attempted,
            Some(source),
        )
    }

    fn restore_parameters(&mut self, source: ContentOperatorSource) -> Result<(), ContentVmError> {
        self.parameters = self
            .saved_parameters
            .pop()
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        Ok(())
    }

    fn execute_boundary(&mut self, action: TextAction) -> Result<(), ContentVmError> {
        match action {
            TextAction::Begin { source } => {
                if self.active {
                    return Err(vm_error(ContentVmErrorCode::InternalState, source));
                }
                self.text_matrix = Matrix::IDENTITY;
                self.line_matrix = Matrix::IDENTITY;
                self.active = true;
            }
            TextAction::End { source } => {
                if !self.active {
                    return Err(vm_error(ContentVmErrorCode::InternalState, source));
                }
                self.active = false;
            }
            _ => unreachable!("only BT/ET reach boundary execution"),
        }
        Ok(())
    }

    #[allow(
        clippy::result_large_err,
        clippy::too_many_arguments,
        reason = "text execution keeps resource proof, Scene publication, and runtime guards explicit"
    )]
    fn execute(
        &mut self,
        action: TextAction,
        runtime: &mut FontRuntime,
        scene: &mut GraphicsSceneBuilder,
        font_uses: &mut Vec<ResolvedFontUse>,
        snapshot: SourceSnapshot,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        materialization_peak: &mut u64,
    ) -> Result<(), ContentVmFailure> {
        let source = action.source();
        if !self.active {
            return Err(ContentVmFailure::Vm(vm_error(
                ContentVmErrorCode::InternalState,
                source,
            )));
        }
        runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
        match action {
            TextAction::SetCharacterSpacing { value, .. } => {
                self.parameters.character_spacing = SceneScalar::from_scaled(value.scaled());
            }
            TextAction::SetWordSpacing { value, .. } => {
                self.parameters.word_spacing = SceneScalar::from_scaled(value.scaled());
            }
            TextAction::SetHorizontalScaling { value, .. } => {
                self.parameters.horizontal_scaling =
                    divide_scalar(SceneScalar::from_scaled(value.scaled()), 100)
                        .map_err(ContentVmFailure::Scene)?;
            }
            TextAction::SetLeading { value, .. } => {
                self.parameters.leading = SceneScalar::from_scaled(value.scaled());
            }
            TextAction::SetFont { size, .. } => {
                let (proof, font) = runtime
                    .resolve_planned(source)
                    .map_err(ContentVmFailure::Vm)?;
                font_uses.push(ResolvedFontUse::new(
                    source,
                    proof,
                    font::resource_source(&font),
                ));
                runtime
                    .record_executed_use(source)
                    .map_err(ContentVmFailure::Vm)?;
                self.parameters.font = Some(font);
                self.parameters.font_size = SceneScalar::from_scaled(size.scaled());
            }
            TextAction::SetRenderMode { value, .. } => {
                if value != 0 {
                    return Err(ContentVmFailure::Vm(vm_error(
                        ContentVmErrorCode::InternalState,
                        source,
                    )));
                }
                self.parameters.render_mode = value;
            }
            TextAction::SetRise { value, .. } => {
                self.parameters.rise = SceneScalar::from_scaled(value.scaled());
            }
            TextAction::MovePosition {
                translation,
                set_leading,
                ..
            } => {
                let [tx, ty] = translation.map(|value| SceneScalar::from_scaled(value.scaled()));
                if set_leading {
                    self.parameters.leading = SceneScalar::ZERO
                        .checked_sub(ty)
                        .map_err(ContentVmFailure::Scene)?;
                }
                self.move_position(tx, ty)
                    .map_err(ContentVmFailure::Scene)?;
            }
            TextAction::SetMatrix { matrix, .. } => {
                let matrix =
                    Matrix::new(matrix.map(|value| SceneScalar::from_scaled(value.scaled())));
                self.text_matrix = matrix;
                self.line_matrix = matrix;
            }
            TextAction::NextLine { .. } => {
                self.next_line().map_err(ContentVmFailure::Scene)?;
            }
            TextAction::Show {
                items,
                character_spacing,
                word_spacing,
                next_line,
                paint,
                ctm,
                command_source,
                ..
            } => {
                if let Some(value) = word_spacing {
                    self.parameters.word_spacing = SceneScalar::from_scaled(value.scaled());
                }
                if let Some(value) = character_spacing {
                    self.parameters.character_spacing = SceneScalar::from_scaled(value.scaled());
                }
                if next_line {
                    self.next_line().map_err(ContentVmFailure::Scene)?;
                }
                self.show(
                    &items,
                    paint,
                    ctm,
                    command_source,
                    source,
                    runtime,
                    scene,
                    snapshot,
                    byte_source,
                    cancellation,
                    materialization_peak,
                )?;
            }
            TextAction::Begin { .. } | TextAction::End { .. } => {
                return Err(ContentVmFailure::Vm(vm_error(
                    ContentVmErrorCode::InternalState,
                    source,
                )));
            }
        }
        runtime_guard(snapshot, byte_source, cancellation, Some(source))
    }

    fn move_position(&mut self, tx: SceneScalar, ty: SceneScalar) -> Result<(), SceneError> {
        self.line_matrix = self
            .line_matrix
            .checked_multiply(translation_matrix(tx, ty))?;
        self.text_matrix = self.line_matrix;
        Ok(())
    }

    fn next_line(&mut self) -> Result<(), SceneError> {
        let ty = SceneScalar::ZERO.checked_sub(self.parameters.leading)?;
        self.move_position(SceneScalar::ZERO, ty)
    }

    #[allow(
        clippy::result_large_err,
        clippy::too_many_arguments,
        reason = "glyph materialization keeps deterministic text state, resource limits, and guard inputs explicit"
    )]
    fn show(
        &mut self,
        items: &[TextShowItem],
        paint: Paint,
        ctm: Matrix,
        command_source: CommandSource,
        source: ContentOperatorSource,
        runtime: &mut FontRuntime,
        scene: &mut GraphicsSceneBuilder,
        snapshot: SourceSnapshot,
        byte_source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        materialization_peak: &mut u64,
    ) -> Result<(), ContentVmFailure> {
        let font = Arc::clone(self.parameters.font.as_ref().ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?);
        let mut glyph_count = 0_u64;
        let mut counted_items = 0_u64;
        for item in items {
            guarded_text_probe(
                &mut counted_items,
                snapshot,
                byte_source,
                cancellation,
                source,
            )?;
            if let TextShowItem::Bytes(bytes) = item {
                glyph_count = glyph_count
                    .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                        ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                    })?)
                    .ok_or_else(|| {
                        ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                    })?;
            }
        }
        runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
        runtime
            .preflight_glyphs(glyph_count, 0, source)
            .map_err(ContentVmFailure::Vm)?;

        let mut segment_count = 0_u64;
        let mut planned_codes = [None::<u16>; 95];
        let mut planned_outlines = [None::<(u16, u64)>; 95];
        let mut planned_outline_count = 0_usize;
        let mut probed = 0_u64;
        for item in items {
            guarded_text_probe(&mut probed, snapshot, byte_source, cancellation, source)?;
            let TextShowItem::Bytes(bytes) = item else {
                continue;
            };
            for &byte in bytes {
                guarded_text_probe(&mut probed, snapshot, byte_source, cancellation, source)?;
                let code_index = usize::from(byte.checked_sub(0x20).ok_or_else(|| {
                    ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                })?);
                if planned_codes[code_index].is_some() {
                    continue;
                }
                let outline = font_outline(&font, byte, source)?;
                let glyph_id = outline.glyph_id().get();
                planned_codes[code_index] = Some(glyph_id);
                if !planned_outlines[..planned_outline_count]
                    .iter()
                    .flatten()
                    .any(|(known, _)| *known == glyph_id)
                {
                    let segments = u64::try_from(outline.segments().len()).map_err(|_| {
                        ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                    })?;
                    segment_count = segment_count.checked_add(segments).ok_or_else(|| {
                        ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                    })?;
                    planned_outlines[planned_outline_count] = Some((glyph_id, segments));
                    planned_outline_count += 1;
                }
            }
        }
        runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
        runtime
            .preflight_glyphs(0, segment_count, source)
            .map_err(ContentVmFailure::Vm)?;

        let glyph_slots = usize::try_from(glyph_count).map_err(|_| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?;
        let glyph_nominal = byte_width::<GlyphUse>(glyph_slots).map_err(ContentVmFailure::Vm)?;
        let path_nominal = segment_count
            .checked_mul(u64::try_from(size_of::<PathSegment>()).map_err(|_| {
                ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
            })?)
            .ok_or_else(|| {
                ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
            })?;
        let candidate_nominal = glyph_nominal.checked_add(path_nominal).ok_or_else(|| {
            ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
        })?;
        self.preflight_glyph_candidate(runtime, 0, candidate_nominal, source)
            .map_err(ContentVmFailure::Vm)?;
        let mut glyphs = Vec::new();
        glyphs.try_reserve_exact(glyph_slots).map_err(|_| {
            ContentVmFailure::Vm(runtime.glyph_allocation_error(0, glyph_nominal, source))
        })?;
        let glyph_retained = capacity_bytes(&glyphs).map_err(ContentVmFailure::Vm)?;
        self.observe_glyph_retained(glyph_retained, materialization_peak);
        runtime.record_glyph_retained(glyph_retained);
        self.preflight_glyph_candidate(runtime, 0, glyph_retained, source)
            .map_err(ContentVmFailure::Vm)?;
        let mut outline_retained = 0_u64;
        let mut outline_cache: [Option<(u16, GlyphOutline)>; 95] = std::array::from_fn(|_| None);
        let mut outline_cache_len = 0_usize;
        let mut probed = 0_u64;
        for item in items {
            guarded_text_probe(&mut probed, snapshot, byte_source, cancellation, source)?;
            match item {
                TextShowItem::Bytes(bytes) => {
                    for &byte in bytes {
                        guarded_text_probe(
                            &mut probed,
                            snapshot,
                            byte_source,
                            cancellation,
                            source,
                        )?;
                        let code_index = usize::from(byte.checked_sub(0x20).ok_or_else(|| {
                            ContentVmFailure::Vm(vm_error(
                                ContentVmErrorCode::InternalState,
                                source,
                            ))
                        })?);
                        let glyph_id = planned_codes[code_index].ok_or_else(|| {
                            ContentVmFailure::Vm(vm_error(
                                ContentVmErrorCode::InternalState,
                                source,
                            ))
                        })?;
                        let scene_outline = if let Some((_, cached)) = outline_cache
                            [..outline_cache_len]
                            .iter()
                            .flatten()
                            .find(|(known, _)| *known == glyph_id)
                        {
                            cached.clone()
                        } else {
                            let outline = font
                                .font()
                                .glyph_outline(GlyphId::new(glyph_id))
                                .ok_or_else(|| {
                                    ContentVmFailure::Vm(vm_error(
                                        ContentVmErrorCode::InternalState,
                                        source,
                                    ))
                                })?;
                            let nominal = byte_width::<PathSegment>(outline.segments().len())
                                .map_err(ContentVmFailure::Vm)?;
                            let consumed = glyph_retained
                                .checked_add(outline_retained)
                                .ok_or_else(|| {
                                    ContentVmFailure::Vm(vm_error(
                                        ContentVmErrorCode::InternalState,
                                        source,
                                    ))
                                })?;
                            self.preflight_glyph_candidate(runtime, consumed, nominal, source)
                                .map_err(ContentVmFailure::Vm)?;
                            let (built, retained) = build_scene_glyph_outline(
                                &font,
                                outline,
                                snapshot,
                                byte_source,
                                cancellation,
                                source,
                                runtime,
                                self.vm_limits,
                                self.materialization_base_retained,
                                consumed,
                                nominal,
                                materialization_peak,
                            )?;
                            self.preflight_glyph_candidate(runtime, consumed, retained, source)
                                .map_err(ContentVmFailure::Vm)?;
                            outline_retained =
                                outline_retained.checked_add(retained).ok_or_else(|| {
                                    ContentVmFailure::Vm(vm_error(
                                        ContentVmErrorCode::InternalState,
                                        source,
                                    ))
                                })?;
                            self.observe_glyph_retained(
                                glyph_retained.saturating_add(outline_retained),
                                materialization_peak,
                            );
                            outline_cache[outline_cache_len] = Some((glyph_id, built.clone()));
                            outline_cache_len += 1;
                            built
                        };
                        let transform =
                            self.glyph_transform(ctm).map_err(ContentVmFailure::Scene)?;
                        glyphs.push(GlyphUse::new(scene_outline, transform, u32::from(byte)));
                        let width = font.pdf_width_for_winansi(byte).ok_or_else(|| {
                            ContentVmFailure::Vm(vm_error(
                                ContentVmErrorCode::InternalState,
                                source,
                            ))
                        })?;
                        let advance = text_advance(&self.parameters, width, byte)
                            .map_err(ContentVmFailure::Scene)?;
                        self.text_matrix = self
                            .text_matrix
                            .checked_multiply(translation_matrix(advance, SceneScalar::ZERO))
                            .map_err(ContentVmFailure::Scene)?;
                    }
                }
                TextShowItem::Adjustment(value) => {
                    let adjustment = text_adjustment(&self.parameters, *value)
                        .map_err(ContentVmFailure::Scene)?;
                    self.text_matrix = self
                        .text_matrix
                        .checked_multiply(translation_matrix(adjustment, SceneScalar::ZERO))
                        .map_err(ContentVmFailure::Scene)?;
                }
            }
        }
        runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
        let candidate_retained = glyph_retained
            .checked_add(outline_retained)
            .ok_or_else(|| {
                ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
            })?;
        self.preflight_glyph_candidate(runtime, 0, candidate_retained, source)
            .map_err(ContentVmFailure::Vm)?;
        self.peak_glyph_retained = self.peak_glyph_retained.max(candidate_retained);
        self.observe_glyph_retained(candidate_retained, materialization_peak);
        runtime.record_glyph_retained(candidate_retained);
        if !glyphs.is_empty() {
            scene
                .draw_glyph_run(glyphs, paint, SceneBounds::Page, command_source)
                .map_err(ContentVmFailure::Scene)?;
        }
        runtime
            .record_glyphs(glyph_count, segment_count, source)
            .map_err(ContentVmFailure::Vm)
    }

    fn glyph_transform(&self, ctm: Matrix) -> Result<Matrix, SceneError> {
        let horizontal_size = self
            .parameters
            .font_size
            .checked_mul(self.parameters.horizontal_scaling)?;
        let text_render = Matrix::new([
            horizontal_size,
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            self.parameters.font_size,
            SceneScalar::ZERO,
            self.parameters.rise,
        ]);
        ctm.checked_multiply(self.text_matrix)?
            .checked_multiply(text_render)
    }
}

#[allow(
    clippy::result_large_err,
    reason = "glyph lookup preserves the complete copyable VM failure contract"
)]
fn font_outline<'a>(
    font: &'a pdf_rs_document::AcquiredFontResource,
    byte: u8,
    source: ContentOperatorSource,
) -> Result<pdf_rs_font::GlyphOutline<'a>, ContentVmFailure> {
    let glyph_id = font
        .font()
        .glyph_id_for_winansi(byte)
        .ok_or_else(|| ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source)))?;
    font.font()
        .glyph_outline(glyph_id)
        .ok_or_else(|| ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source)))
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_arguments,
    reason = "outline handoff keeps proof, guards, independent budgets, and failure peaks explicit"
)]
fn build_scene_glyph_outline(
    font: &pdf_rs_document::AcquiredFontResource,
    outline: pdf_rs_font::GlyphOutline<'_>,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    source: ContentOperatorSource,
    runtime: &mut FontRuntime,
    vm_limits: ContentVmLimits,
    materialization_base_retained: u64,
    candidate_consumed: u64,
    candidate_nominal: u64,
    materialization_peak: &mut u64,
) -> Result<(GlyphOutline, u64), ContentVmFailure> {
    runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
    let mut path = PathResourceBuilder::new();
    path.try_reserve_exact(outline.segments().len())
        .map_err(|_| {
            ContentVmFailure::Vm(runtime.glyph_allocation_error(
                candidate_consumed,
                candidate_nominal,
                source,
            ))
        })?;
    let actual = path.retained_bytes().map_err(ContentVmFailure::Scene)?;
    let candidate = candidate_consumed
        .checked_add(actual)
        .ok_or_else(|| ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source)))?;
    runtime.record_glyph_retained(candidate);
    *materialization_peak =
        (*materialization_peak).max(materialization_base_retained.saturating_add(candidate));
    runtime
        .preflight_glyph_retained(candidate_consumed, actual, source)
        .map_err(ContentVmFailure::Vm)?;
    vm_limits
        .preflight(
            ContentVmLimitKind::RetainedBytes,
            materialization_base_retained
                .checked_add(candidate_consumed)
                .ok_or_else(|| {
                    ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source))
                })?,
            actual,
            Some(source),
        )
        .map_err(ContentVmFailure::Vm)?;
    for (index, segment) in outline.segments().iter().enumerate() {
        if index.is_multiple_of(256) {
            runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
        }
        match *segment {
            OutlineSegment::MoveTo(point) => {
                path.try_push(PathSegment::MoveTo(
                    scene_font_point(point).map_err(ContentVmFailure::Scene)?,
                ))
                .map_err(ContentVmFailure::Scene)?;
            }
            OutlineSegment::LineTo(point) => {
                path.try_push(PathSegment::LineTo(
                    scene_font_point(point).map_err(ContentVmFailure::Scene)?,
                ))
                .map_err(ContentVmFailure::Scene)?;
            }
            OutlineSegment::QuadTo { control, end } => {
                path.try_push_quadratic(
                    scene_font_point(control).map_err(ContentVmFailure::Scene)?,
                    scene_font_point(end).map_err(ContentVmFailure::Scene)?,
                )
                .map_err(ContentVmFailure::Scene)?;
            }
            OutlineSegment::CloseContour => path
                .try_push(PathSegment::ClosePath)
                .map_err(ContentVmFailure::Scene)?,
        }
    }
    runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
    let retained = path.retained_bytes().map_err(ContentVmFailure::Scene)?;
    let outline = GlyphOutline::new(
        font::resource_source(font),
        u32::from(outline.glyph_id().get()),
        font.font().units_per_em(),
        path.finish(),
    )
    .map_err(ContentVmFailure::Scene)?;
    Ok((outline, retained))
}

fn scene_font_point(point: pdf_rs_font::FontPoint) -> Result<ScenePoint, SceneError> {
    let coordinate = |value: pdf_rs_font::FontCoordinate| {
        SceneScalar::from_scaled(i64::from(value.half_units()) * 500_000_000)
    };
    Ok(ScenePoint::new(
        coordinate(point.x()),
        coordinate(point.y()),
    ))
}

fn translation_matrix(tx: SceneScalar, ty: SceneScalar) -> Matrix {
    Matrix::new([
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        tx,
        ty,
    ])
}

fn text_advance(
    parameters: &TextParameters,
    width: u32,
    byte: u8,
) -> Result<SceneScalar, SceneError> {
    let width = integer_product_divide(parameters.font_size, u64::from(width), 1_000)?;
    let mut advance = width.checked_add(parameters.character_spacing)?;
    if byte == 0x20 {
        advance = advance.checked_add(parameters.word_spacing)?;
    }
    advance.checked_mul(parameters.horizontal_scaling)
}

fn text_adjustment(
    parameters: &TextParameters,
    adjustment: ContentNumber,
) -> Result<SceneScalar, SceneError> {
    let adjustment = SceneScalar::from_scaled(adjustment.scaled());
    let scaled = product_divide(adjustment, parameters.font_size, 1_000)?;
    SceneScalar::ZERO
        .checked_sub(scaled)?
        .checked_mul(parameters.horizontal_scaling)
}

fn divide_scalar(value: SceneScalar, denominator: i128) -> Result<SceneScalar, SceneError> {
    rounded_scene_scalar(i128::from(value.scaled()), denominator)
}

fn integer_product_divide(
    value: SceneScalar,
    multiplier: u64,
    denominator: i128,
) -> Result<SceneScalar, SceneError> {
    let numerator = i128::from(value.scaled())
        .checked_mul(i128::from(multiplier))
        .ok_or_else(scene_numeric_overflow)?;
    rounded_scene_scalar(numerator, denominator)
}

fn product_divide(
    left: SceneScalar,
    right: SceneScalar,
    denominator: i128,
) -> Result<SceneScalar, SceneError> {
    let numerator = i128::from(left.scaled())
        .checked_mul(i128::from(right.scaled()))
        .ok_or_else(scene_numeric_overflow)?;
    let divisor = 1_000_000_000_i128
        .checked_mul(denominator)
        .ok_or_else(scene_numeric_overflow)?;
    rounded_scene_scalar(numerator, divisor)
}

fn rounded_scene_scalar(numerator: i128, denominator: i128) -> Result<SceneScalar, SceneError> {
    if denominator <= 0 {
        return Err(scene_numeric_overflow());
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = if remainder
        .abs()
        .checked_mul(2)
        .ok_or_else(scene_numeric_overflow)?
        >= denominator
    {
        quotient
            .checked_add(if numerator.is_negative() { -1 } else { 1 })
            .ok_or_else(scene_numeric_overflow)?
    } else {
        quotient
    };
    i64::try_from(rounded)
        .map(SceneScalar::from_scaled)
        .map_err(|_| scene_numeric_overflow())
}

fn scene_numeric_overflow() -> SceneError {
    SceneScalar::from_scaled(i64::MAX)
        .checked_add(SceneScalar::ONE)
        .expect_err("maximum Scene scalar plus one must overflow")
}

#[allow(
    clippy::result_large_err,
    reason = "cooperative text probing preserves the complete copyable VM failure"
)]
fn guarded_text_probe(
    probed: &mut u64,
    snapshot: SourceSnapshot,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    source: ContentOperatorSource,
) -> Result<(), ContentVmFailure> {
    *probed = probed
        .checked_add(1)
        .ok_or_else(|| ContentVmFailure::Vm(vm_error(ContentVmErrorCode::InternalState, source)))?;
    if probed.is_multiple_of(256) {
        runtime_guard(snapshot, byte_source, cancellation, Some(source))?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "atomic materialization keeps each runtime, guard, budget, and peak sink explicit"
)]
fn materialize_execution_plan(
    input: AcquiredContentInput<'_>,
    plan: ExecutionPlan,
    profile: ContentVmProfile,
    vm_limits: ContentVmLimits,
    mut image_runtime: Option<&mut ImageRuntime>,
    mut font_runtime: Option<&mut FontRuntime>,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    materialization_peak: &mut u64,
) -> RunTerminal {
    let snapshot = input.snapshot();
    let live_plan_retained = match plan.vm_retained_bytes() {
        Ok(value) => value,
        Err(error) => {
            return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
        }
    };
    let mut scene = match profile {
        ContentVmProfile::SceneV1 { scene_limits } => {
            SceneSink::V1(SceneBuilder::new(plan.binding, plan.geometry, scene_limits))
        }
        ContentVmProfile::GraphicsV2 { scene_limits, .. } => SceneSink::V2(
            GraphicsSceneBuilder::new_v2(plan.binding, plan.geometry, scene_limits),
        ),
    };
    let mut image_uses = plan.image_uses;
    let expected_form_uses = image_runtime
        .as_deref()
        .map_or(0, ImageRuntime::form_use_count);
    let mut form_uses = Vec::new();
    if let Err(error) = reserve_exact_slots_observed(
        &mut form_uses,
        expected_form_uses,
        live_plan_retained.saturating_add(capacity_bytes(&image_uses).unwrap_or(u64::MAX)),
        vm_limits,
        None,
        materialization_peak,
    ) {
        return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
    }
    let expected_font_uses = plan.font_use_count;
    let mut font_uses = Vec::new();
    let pre_font_retained = live_plan_retained
        .saturating_add(capacity_bytes(&image_uses).unwrap_or(u64::MAX))
        .saturating_add(capacity_bytes(&form_uses).unwrap_or(u64::MAX));
    if let Err(error) = reserve_exact_slots_observed(
        &mut font_uses,
        expected_font_uses,
        pre_font_retained,
        vm_limits,
        None,
        materialization_peak,
    ) {
        return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
    }
    let text_stack_slots = match usize::try_from(plan.accounting.max_graphics_depth) {
        Ok(value) => value,
        Err(_) => {
            return prioritize_vm_without_source(
                snapshot,
                byte_source,
                cancellation,
                ContentVmError::new(ContentVmErrorCode::InternalState, None),
            );
        }
    };
    let text_consumed =
        pre_font_retained.saturating_add(capacity_bytes(&font_uses).unwrap_or(u64::MAX));
    let mut text = match TextExecutor::new(
        text_stack_slots,
        vm_limits,
        text_consumed,
        materialization_peak,
    ) {
        Ok(value) => value,
        Err(error) => {
            return prioritize_vm_without_source(snapshot, byte_source, cancellation, error);
        }
    };
    *materialization_peak = (*materialization_peak).max(text.peak_retained_bytes());
    for action in plan.actions {
        let operator_source = action_operator_source(&action);
        if let Err(failure) =
            runtime_guard(snapshot, byte_source, cancellation, Some(operator_source))
        {
            return RunTerminal::Failed(failure);
        }
        let result = match action {
            ExecutionAction::BeginMarkedContent {
                tag,
                properties,
                source,
            } => scene.begin_marked_content(&tag, properties, source),
            ExecutionAction::EndMarkedContent { source } => scene.end_marked_content(source),
            ExecutionAction::Save { bounds, source } => {
                if let Err(error) = text.save_parameters(operator_source) {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        error,
                    );
                }
                scene
                    .graphics_mut()
                    .expect("only graphics-v2 plans contain graphics actions")
                    .append_save(bounds, source)
            }
            ExecutionAction::Restore { bounds, source } => {
                if let Err(error) = text.restore_parameters(operator_source) {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        error,
                    );
                }
                scene
                    .graphics_mut()
                    .expect("only graphics-v2 plans contain graphics actions")
                    .append_restore(bounds, source)
            }
            ExecutionAction::BeginGroup {
                alpha,
                blend_mode,
                bounds,
                source,
            } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain group actions")
                .begin_group(alpha, blend_mode, bounds, source),
            ExecutionAction::EndGroup { bounds, source } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain group actions")
                .end_group(bounds, source),
            ExecutionAction::Clip {
                path,
                rule,
                transform,
                bounds,
                source,
            } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_clip(path, rule, transform, bounds, source),
            ExecutionAction::Fill {
                path,
                rule,
                paint,
                transform,
                bounds,
                source,
            } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_fill(path, rule, paint, transform, bounds, source),
            ExecutionAction::Stroke {
                path,
                paint,
                style,
                transform,
                bounds,
                source,
            } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_stroke(path, paint, style, transform, bounds, source),
            ExecutionAction::FillStroke {
                path,
                rule,
                fill,
                stroke,
                style,
                transform,
                bounds,
                source,
            } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_fill_stroke(path, rule, fill, stroke, style, transform, bounds, source),
            ExecutionAction::DrawImage {
                source,
                command_source,
                transform,
                alpha,
                blend_mode,
                bounds,
            } => {
                let Some(runtime) = image_runtime.as_deref_mut() else {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        source,
                        vm_error(ContentVmErrorCode::InternalState, source),
                    );
                };
                let xobject = match runtime.resolve_planned(source) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(snapshot, byte_source, cancellation, source, error);
                    }
                };
                let result = match xobject {
                    ResolvedXObject::Image { proof, image } => {
                        let resource_source = image.source();
                        let result = scene
                            .graphics_mut()
                            .expect("XObject plans require the graphics-v2 Scene")
                            .draw_image(
                                image,
                                transform,
                                alpha,
                                blend_mode,
                                bounds,
                                command_source,
                            );
                        if result.is_ok() {
                            image_uses.push(ResolvedImageUse::new(source, proof, resource_source));
                        }
                        result
                    }
                    ResolvedXObject::Form { proof, form } => {
                        let result = scene
                            .graphics_mut()
                            .expect("XObject plans require the graphics-v2 Scene")
                            .append_scene(form.scene());
                        if result.is_ok() {
                            form_uses.push(ResolvedFormUse::new(source, proof, form));
                        }
                        result
                    }
                };
                if result.is_ok()
                    && let Err(error) = runtime.record_executed_use(source)
                {
                    return prioritize_vm(snapshot, byte_source, cancellation, source, error);
                }
                result
            }
            ExecutionAction::Text(action) => {
                if matches!(action, TextAction::Begin { .. } | TextAction::End { .. }) {
                    if let Err(error) = text.execute_boundary(action) {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    continue;
                }
                let Some(runtime) = font_runtime.as_deref_mut() else {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        vm_error(ContentVmErrorCode::InternalState, operator_source),
                    );
                };
                let Some(builder) = scene.graphics_mut() else {
                    return prioritize_vm(
                        snapshot,
                        byte_source,
                        cancellation,
                        operator_source,
                        vm_error(ContentVmErrorCode::InternalState, operator_source),
                    );
                };
                if let Err(failure) = text.execute(
                    action,
                    runtime,
                    builder,
                    &mut font_uses,
                    snapshot,
                    byte_source,
                    cancellation,
                    materialization_peak,
                ) {
                    return prioritize(
                        snapshot,
                        byte_source,
                        cancellation,
                        Some(operator_source),
                        RunTerminal::Failed(failure),
                    );
                }
                continue;
            }
        };
        if let Err(error) = result {
            return prioritize_scene(snapshot, byte_source, cancellation, operator_source, error);
        }
    }
    let scene = match scene.finish() {
        Ok(value) => value,
        Err(error) => {
            return prioritize(
                snapshot,
                byte_source,
                cancellation,
                None,
                RunTerminal::Failed(ContentVmFailure::Scene(error)),
            );
        }
    };
    if let Err(failure) = runtime_guard(snapshot, byte_source, cancellation, None) {
        return RunTerminal::Failed(failure);
    }
    let retained_use_capacity_bytes = capacity_bytes(&plan.property_uses)
        .ok()
        .and_then(|value| value.checked_add(capacity_bytes(&image_uses).ok()?))
        .and_then(|value| value.checked_add(capacity_bytes(&form_uses).ok()?))
        .and_then(|value| value.checked_add(capacity_bytes(&font_uses).ok()?))
        .unwrap_or(u64::MAX);
    RunTerminal::Ready(Execution {
        scene,
        property_uses: plan.property_uses,
        image_uses,
        form_uses,
        font_uses,
        retained_use_capacity_bytes,
        final_ctm: plan.final_ctm,
    })
}

fn action_operator_source(action: &ExecutionAction) -> ContentOperatorSource {
    let source = match action {
        ExecutionAction::BeginMarkedContent { source, .. }
        | ExecutionAction::EndMarkedContent { source }
        | ExecutionAction::Save { source, .. }
        | ExecutionAction::Restore { source, .. }
        | ExecutionAction::BeginGroup { source, .. }
        | ExecutionAction::EndGroup { source, .. }
        | ExecutionAction::Clip { source, .. }
        | ExecutionAction::Fill { source, .. }
        | ExecutionAction::Stroke { source, .. }
        | ExecutionAction::FillStroke { source, .. } => *source,
        ExecutionAction::DrawImage { source, .. } => return *source,
        ExecutionAction::Text(action) => return action.source(),
    };
    ContentOperatorSource::new(
        crate::DecodedSpan::new(
            source.object(),
            source.stream_index(),
            source.decoded_start(),
            source.decoded_length(),
        ),
        u64::from(source.operator_index()),
    )
}

enum PropertyOperand<'a> {
    Name(&'a ContentName),
    Dictionary,
}

enum ValidatedOperands<'a> {
    None,
    OneNumber(ContentNumber),
    TwoNumbers([ContentNumber; 2]),
    ThreeNumbers([ContentNumber; 3]),
    FourNumbers([ContentNumber; 4]),
    SixNumbers([ContentNumber; 6]),
    OneInteger(i64),
    Dash {
        pattern: DashPattern,
    },
    Name(&'a ContentName),
    NameAndNumber(&'a ContentName, ContentNumber),
    String(&'a crate::ContentString),
    Array(&'a [LocatedOperand]),
    TwoNumbersAndString([ContentNumber; 2], &'a crate::ContentString),
    NameAndProperty {
        tag: &'a ContentName,
        property: PropertyOperand<'a>,
    },
}

impl ValidatedOperands<'_> {
    fn dynamic_fuel(&self, kind: OperatorKind) -> u64 {
        match self {
            _ if matches!(
                kind,
                OperatorKind::LineTo
                    | OperatorKind::CubicCurveTo
                    | OperatorKind::CubicCurveToReplicateInitial
                    | OperatorKind::CubicCurveToReplicateFinal
            ) =>
            {
                2
            }
            _ if matches!(kind, OperatorKind::MoveTo | OperatorKind::ClosePath) => 1,
            _ if kind == OperatorKind::Rectangle => 5,
            Self::None
            | Self::OneNumber(_)
            | Self::TwoNumbers(_)
            | Self::ThreeNumbers(_)
            | Self::FourNumbers(_)
            | Self::SixNumbers(_)
            | Self::OneInteger(_)
            | Self::Dash { .. }
            | Self::Name(_)
            | Self::NameAndProperty { .. } => 0,
            Self::NameAndNumber(name, _) if kind == OperatorKind::SetTextFont => {
                u64::try_from(name.bytes().len()).unwrap_or(u64::MAX)
            }
            Self::NameAndNumber(_, _) => 0,
            Self::String(value) => u64::try_from(value.bytes().len()).unwrap_or(u64::MAX),
            Self::Array(values) => u64::try_from(values.len()).unwrap_or(u64::MAX),
            Self::TwoNumbersAndString(_, value) => {
                u64::try_from(value.bytes().len()).unwrap_or(u64::MAX)
            }
        }
    }
}

fn validate_operand_structure(
    kind: OperatorKind,
    operands: &[LocatedOperand],
    source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    let spec = kind.spec();
    if operands.len() != usize::from(spec.min_operands()) {
        return Err(vm_error(ContentVmErrorCode::InvalidOperandCount, source));
    }
    let valid = match spec.operand_shape() {
        OperatorOperandShape::None => true,
        OperatorOperandShape::OneNumber
        | OperatorOperandShape::TwoNumbers
        | OperatorOperandShape::ThreeNumbers
        | OperatorOperandShape::FourNumbers
        | OperatorOperandShape::SixNumbers => operands.iter().all(is_number),
        OperatorOperandShape::OneInteger => {
            matches!(operands[0].value(), ContentOperand::Integer(_))
        }
        OperatorOperandShape::NumberArrayAndNumber => {
            matches!(operands[0].value(), ContentOperand::Array(_)) && is_number(&operands[1])
        }
        OperatorOperandShape::Name => {
            matches!(operands[0].value(), ContentOperand::Name(_))
        }
        OperatorOperandShape::NameAndNumber => {
            matches!(operands[0].value(), ContentOperand::Name(_)) && is_number(&operands[1])
        }
        OperatorOperandShape::String => {
            matches!(operands[0].value(), ContentOperand::String(_))
        }
        OperatorOperandShape::Array => {
            matches!(operands[0].value(), ContentOperand::Array(_))
        }
        OperatorOperandShape::TwoNumbersAndString => {
            is_number(&operands[0])
                && is_number(&operands[1])
                && matches!(operands[2].value(), ContentOperand::String(_))
        }
        OperatorOperandShape::NameAndNameOrDictionary => {
            matches!(operands[0].value(), ContentOperand::Name(_))
                && matches!(
                    operands[1].value(),
                    ContentOperand::Name(_) | ContentOperand::Dictionary(_)
                )
        }
    };
    if valid {
        Ok(())
    } else {
        Err(vm_error(ContentVmErrorCode::InvalidOperandType, source))
    }
}

fn validate_operator_context(
    kind: OperatorKind,
    text_active: bool,
    source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    if !text_active && kind.spec().context() == OperatorContext::TextObject {
        return Err(vm_error(ContentVmErrorCode::InvalidOperatorContext, source));
    }
    if text_active
        && matches!(
            kind.spec().context(),
            OperatorContext::PathConstruction
                | OperatorContext::PathPainting
                | OperatorContext::ClippingPath
                | OperatorContext::XObject
        )
    {
        return Err(vm_error(ContentVmErrorCode::InvalidOperatorContext, source));
    }
    Ok(())
}

fn is_number(operand: &LocatedOperand) -> bool {
    matches!(
        operand.value(),
        ContentOperand::Integer(_) | ContentOperand::Real(_)
    )
}

fn dash_operands(operands: &[LocatedOperand]) -> (&[LocatedOperand], &LocatedOperand) {
    let ContentOperand::Array(values) = operands[0].value() else {
        unreachable!("validated line-dash operands start with an array");
    };
    (values, &operands[1])
}

fn validate_legacy_dash_operands(
    values: &[LocatedOperand],
    phase: &LocatedOperand,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    parse_number(phase, operator_source)?;
    guarded_dash_probe(snapshot, source, cancellation, operator_source)?;
    for (index, value) in values.iter().enumerate() {
        parse_number(value, operator_source)?;
        if (index + 1) % DASH_CANCELLATION_INTERVAL == 0 {
            guarded_dash_probe(snapshot, source, cancellation, operator_source)?;
        }
    }
    if !values.len().is_multiple_of(DASH_CANCELLATION_INTERVAL) {
        guarded_dash_probe(snapshot, source, cancellation, operator_source)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "dash conversion keeps admission, source binding, cancellation, and provenance explicit"
)]
fn convert_dash_operands(
    values: &[LocatedOperand],
    phase: &LocatedOperand,
    admission: DashRetentionAdmission,
    expected_bytes: u64,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<DashPattern, ContentVmError> {
    let phase = parse_number(phase, operator_source)?;
    if phase < ContentNumber::ZERO {
        return Err(vm_error(
            ContentVmErrorCode::InvalidGraphicsParameter,
            operator_source,
        ));
    }
    guarded_dash_probe(snapshot, source, cancellation, operator_source)?;

    let mut builder = DashPatternBuilder::new();
    builder
        .try_reserve_exact(values.len())
        .map_err(|_| admission.allocation_error(expected_bytes))?;
    let actual = builder
        .retained_bytes()
        .map_err(|_| vm_error(ContentVmErrorCode::InternalState, operator_source))?;
    admission.preflight_actual(actual)?;
    accounting.observe_retained(admission.retained_with_candidate(actual));

    for (index, value) in values.iter().enumerate() {
        let value = parse_number(value, operator_source)?;
        if value < ContentNumber::ZERO {
            return Err(vm_error(
                ContentVmErrorCode::InvalidGraphicsParameter,
                operator_source,
            ));
        }
        builder
            .try_push(SceneScalar::from_scaled(value.scaled()))
            .map_err(|_| {
                vm_error(
                    ContentVmErrorCode::InvalidGraphicsParameter,
                    operator_source,
                )
            })?;
        if (index + 1) % DASH_CANCELLATION_INTERVAL == 0 {
            guarded_dash_probe(snapshot, source, cancellation, operator_source)?;
        }
    }
    if !values.len().is_multiple_of(DASH_CANCELLATION_INTERVAL) {
        guarded_dash_probe(snapshot, source, cancellation, operator_source)?;
    }
    let pattern = builder
        .finish(SceneScalar::from_scaled(phase.scaled()))
        .map_err(|_| {
            vm_error(
                ContentVmErrorCode::InvalidGraphicsParameter,
                operator_source,
            )
        })?;
    Ok(pattern)
}

fn guarded_dash_probe(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    match runtime_guard(snapshot, source, cancellation, Some(operator_source)) {
        Ok(()) => Ok(()),
        Err(ContentVmFailure::Vm(error)) => Err(error),
        Err(
            ContentVmFailure::Content(_)
            | ContentVmFailure::Document(_)
            | ContentVmFailure::Scene(_),
        ) => unreachable!("runtime guards only produce VM source or cancellation failures"),
    }
}

fn convert_operands<'a>(
    kind: OperatorKind,
    operands: &'a [LocatedOperand],
    source: ContentOperatorSource,
) -> Result<ValidatedOperands<'a>, ContentVmError> {
    let spec = kind.spec();
    match spec.operand_shape() {
        OperatorOperandShape::None => Ok(ValidatedOperands::None),
        OperatorOperandShape::OneNumber => Ok(ValidatedOperands::OneNumber(parse_number(
            &operands[0],
            source,
        )?)),
        OperatorOperandShape::TwoNumbers => Ok(ValidatedOperands::TwoNumbers(parse_numbers(
            operands, source,
        )?)),
        OperatorOperandShape::ThreeNumbers => Ok(ValidatedOperands::ThreeNumbers(parse_numbers(
            operands, source,
        )?)),
        OperatorOperandShape::FourNumbers => Ok(ValidatedOperands::FourNumbers(parse_numbers(
            operands, source,
        )?)),
        OperatorOperandShape::SixNumbers => Ok(ValidatedOperands::SixNumbers(parse_numbers(
            operands, source,
        )?)),
        OperatorOperandShape::OneInteger => {
            let ContentOperand::Integer(value) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::OneInteger(*value))
        }
        OperatorOperandShape::NumberArrayAndNumber => {
            unreachable!("line-dash operands are admitted and converted by the bounded dash path")
        }
        OperatorOperandShape::Name => {
            let ContentOperand::Name(name) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::Name(name))
        }
        OperatorOperandShape::NameAndNumber => {
            let ContentOperand::Name(name) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::NameAndNumber(
                name,
                parse_number(&operands[1], source)?,
            ))
        }
        OperatorOperandShape::String => {
            let ContentOperand::String(value) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::String(value))
        }
        OperatorOperandShape::Array => {
            let ContentOperand::Array(values) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::Array(values))
        }
        OperatorOperandShape::TwoNumbersAndString => {
            let ContentOperand::String(value) = operands[2].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::TwoNumbersAndString(
                parse_numbers(&operands[..2], source)?,
                value,
            ))
        }
        OperatorOperandShape::NameAndNameOrDictionary => {
            let ContentOperand::Name(tag) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            let property = match operands[1].value() {
                ContentOperand::Name(name) => PropertyOperand::Name(name),
                ContentOperand::Dictionary(_) => PropertyOperand::Dictionary,
                _ => return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source)),
            };
            Ok(ValidatedOperands::NameAndProperty { tag, property })
        }
    }
}

fn parse_numbers<const N: usize>(
    operands: &[LocatedOperand],
    source: ContentOperatorSource,
) -> Result<[ContentNumber; N], ContentVmError> {
    let mut numbers = [ContentNumber::ZERO; N];
    for (output, operand) in numbers.iter_mut().zip(operands) {
        *output = parse_number(operand, source)?;
    }
    Ok(numbers)
}

fn parse_number(
    operand: &LocatedOperand,
    source: ContentOperatorSource,
) -> Result<ContentNumber, ContentVmError> {
    match operand.value() {
        ContentOperand::Integer(value) => ContentNumber::from_integer(*value),
        ContentOperand::Real(value) => ContentNumber::parse(value.raw()),
        _ => Err(vm_error(ContentVmErrorCode::InvalidOperandType, source)),
    }
    .map_err(|error| error.with_source(source))
}

fn admit_operator(
    accounting: &mut Accounting,
    limits: ContentVmLimits,
    fuel: u64,
    source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    limits.preflight(
        ContentVmLimitKind::Operators,
        accounting.operators,
        1,
        Some(source),
    )?;
    limits.preflight(
        ContentVmLimitKind::Fuel,
        accounting.fuel,
        fuel,
        Some(source),
    )?;
    accounting.operators = accounting
        .operators
        .checked_add(1)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    accounting.fuel = accounting
        .fuel
        .checked_add(fuel)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    Ok(())
}

fn preflight_marked_depth(
    depth: u32,
    limits: ContentVmLimits,
    source: ContentOperatorSource,
) -> Result<(), ContentVmError> {
    limits.preflight(
        ContentVmLimitKind::MarkedContentDepth,
        u64::from(depth),
        1,
        Some(source),
    )
}

fn command_source(source: ContentOperatorSource) -> Result<CommandSource, SceneError> {
    let span = source.span();
    let operator_index = u32::try_from(source.page_operator_ordinal())
        .expect("validated VM operator hard ceiling fits u32");
    CommandSource::new(
        span.object(),
        span.stream_ordinal(),
        span.decoded_start(),
        span.decoded_len(),
        operator_index,
    )
}

fn page_scene_context(
    acquired: &AcquiredPageContent,
) -> Result<(SceneBinding, PageGeometry), SceneError> {
    let handle = acquired.handle();
    let binding = SceneBinding::new(
        handle.snapshot().identity(),
        handle.revision_startxref(),
        handle.index(),
        handle.object(),
    );
    let boxes = acquired.page().boxes();
    let media = scene_rect(boxes.media_box().coordinates())?;
    let crop = scene_rect(boxes.crop_box().coordinates())?;
    let rotation = match acquired.page().rotation() {
        pdf_rs_document::PageRotation::Degrees0 => ScenePageRotation::Degrees0,
        pdf_rs_document::PageRotation::Degrees90 => ScenePageRotation::Degrees90,
        pdf_rs_document::PageRotation::Degrees180 => ScenePageRotation::Degrees180,
        pdf_rs_document::PageRotation::Degrees270 => ScenePageRotation::Degrees270,
    };
    Ok((binding, PageGeometry::new(media, crop, rotation)))
}

fn scene_rect(coordinates: [pdf_rs_document::PageCoordinate; 4]) -> Result<SceneRect, SceneError> {
    SceneRect::new(coordinates.map(|value| SceneScalar::from_scaled(value.scaled())))
}

#[allow(
    clippy::result_large_err,
    reason = "the terminal failure deliberately preserves copyable lower errors without boxing"
)]
fn runtime_guard(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: Option<ContentOperatorSource>,
) -> Result<(), ContentVmFailure> {
    if source.snapshot() != snapshot {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::SourceSnapshotMismatch,
            operator_source,
        )));
    }
    let cancelled = cancellation.is_cancelled();
    if source.snapshot() != snapshot {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::SourceSnapshotMismatch,
            operator_source,
        )));
    }
    if cancelled {
        return Err(ContentVmFailure::Vm(ContentVmError::new(
            ContentVmErrorCode::Cancelled,
            operator_source,
        )));
    }
    Ok(())
}

fn prioritize(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: Option<ContentOperatorSource>,
    fallback: RunTerminal,
) -> RunTerminal {
    match runtime_guard(snapshot, source, cancellation, operator_source) {
        Ok(()) => fallback,
        Err(failure) => RunTerminal::Failed(failure),
    }
}

fn prioritize_vm(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: ContentOperatorSource,
    error: ContentVmError,
) -> RunTerminal {
    prioritize(
        snapshot,
        source,
        cancellation,
        Some(operator_source),
        RunTerminal::Failed(ContentVmFailure::Vm(error)),
    )
}

fn prioritize_vm_without_source(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    error: ContentVmError,
) -> RunTerminal {
    prioritize(
        snapshot,
        source,
        cancellation,
        None,
        RunTerminal::Failed(ContentVmFailure::Vm(error)),
    )
}

fn prioritize_scene(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    operator_source: ContentOperatorSource,
    error: SceneError,
) -> RunTerminal {
    prioritize(
        snapshot,
        source,
        cancellation,
        Some(operator_source),
        RunTerminal::Failed(ContentVmFailure::Scene(error)),
    )
}

fn vm_error(code: ContentVmErrorCode, source: ContentOperatorSource) -> ContentVmError {
    ContentVmError::new(code, Some(source))
}

fn reserve_exact_slots<T>(
    values: &mut Vec<T>,
    slots: usize,
    consumed: u64,
    limits: ContentVmLimits,
    source: Option<ContentOperatorSource>,
) -> Result<u64, ContentVmError> {
    let attempted = byte_width::<T>(slots)?;
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        attempted,
        source,
    )?;
    values.try_reserve_exact(slots).map_err(|_| {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                limits.max_retained_bytes(),
                consumed,
                attempted,
            ),
            source,
        )
    })?;
    let actual = capacity_bytes(values)?;
    limits.preflight(ContentVmLimitKind::RetainedBytes, consumed, actual, source)?;
    Ok(actual)
}

fn reserve_exact_slots_accounted<T>(
    values: &mut Vec<T>,
    slots: usize,
    consumed: u64,
    limits: ContentVmLimits,
    source: Option<ContentOperatorSource>,
    accounting: &mut Accounting,
) -> Result<u64, ContentVmError> {
    let attempted = byte_width::<T>(slots)?;
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        attempted,
        source,
    )?;
    values.try_reserve_exact(slots).map_err(|_| {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                limits.max_retained_bytes(),
                consumed,
                attempted,
            ),
            source,
        )
    })?;
    let actual = capacity_bytes(values)?;
    accounting.observe_retained(consumed.saturating_add(actual));
    limits.preflight(ContentVmLimitKind::RetainedBytes, consumed, actual, source)?;
    Ok(actual)
}

fn reserve_exact_slots_observed<T>(
    values: &mut Vec<T>,
    slots: usize,
    consumed: u64,
    limits: ContentVmLimits,
    source: Option<ContentOperatorSource>,
    peak_retained: &mut u64,
) -> Result<u64, ContentVmError> {
    let attempted = byte_width::<T>(slots)?;
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        attempted,
        source,
    )?;
    values.try_reserve_exact(slots).map_err(|_| {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                limits.max_retained_bytes(),
                consumed,
                attempted,
            ),
            source,
        )
    })?;
    let actual = capacity_bytes(values)?;
    *peak_retained = (*peak_retained).max(consumed.saturating_add(actual));
    limits.preflight(ContentVmLimitKind::RetainedBytes, consumed, actual, source)?;
    Ok(actual)
}

fn reserve_vm_slot<T>(
    values: &mut Vec<T>,
    program_bytes: u64,
    other_capacity_bytes: u64,
    limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<u64, ContentVmError> {
    if values.len() == values.capacity() {
        let required_capacity = values
            .len()
            .checked_add(1)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let target_capacity = geometric_capacity(values.capacity(), required_capacity);
        let current_bytes = capacity_bytes(values)?;
        let target_bytes = byte_width::<T>(target_capacity)?;
        let consumed = program_bytes
            .checked_add(other_capacity_bytes)
            .and_then(|value| value.checked_add(current_bytes))
            .unwrap_or(u64::MAX);
        let attempted = target_bytes.saturating_sub(current_bytes);
        limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            consumed,
            attempted,
            Some(source),
        )?;
        accounting.charge_fuel(
            limits,
            u64::try_from(values.len())
                .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?,
            source,
        )?;
        let additional = target_capacity
            .checked_sub(values.len())
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        values.try_reserve_exact(additional).map_err(|_| {
            ContentVmError::resource(
                ContentVmLimit::new(
                    ContentVmLimitKind::Allocation,
                    limits.max_retained_bytes(),
                    consumed,
                    attempted,
                ),
                Some(source),
            )
        })?;
    }
    let total = program_bytes
        .checked_add(other_capacity_bytes)
        .and_then(|value| value.checked_add(capacity_bytes(values).ok()?))
        .unwrap_or(u64::MAX);
    limits.preflight(ContentVmLimitKind::RetainedBytes, 0, total, Some(source))?;
    accounting.observe_retained(total);
    Ok(total)
}

#[allow(
    clippy::too_many_arguments,
    reason = "action growth keeps the program, other plan state, limits, source, and accounting explicit"
)]
fn push_execution_action(
    actions: &mut Vec<ExecutionAction>,
    action: ExecutionAction,
    program_bytes: u64,
    other_capacity_bytes: u64,
    limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<(), ContentVmError> {
    reserve_vm_slot(
        actions,
        program_bytes,
        other_capacity_bytes,
        limits,
        source,
        accounting,
    )?;
    actions.push(action);
    Ok(())
}

fn geometric_capacity(current_capacity: usize, required_capacity: usize) -> usize {
    if required_capacity <= current_capacity {
        return current_capacity;
    }
    let grown = if current_capacity == 0 {
        4
    } else {
        current_capacity.checked_mul(2).unwrap_or(required_capacity)
    };
    grown.max(required_capacity)
}

fn capacity_bytes<T>(values: &Vec<T>) -> Result<u64, ContentVmError> {
    byte_width::<T>(values.capacity())
}

fn plan_value_capacity_bytes(
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    planned_name_bytes: u64,
) -> Result<u64, ContentVmError> {
    capacity_bytes(property_uses)?
        .checked_add(capacity_bytes(image_uses)?)
        .and_then(|value| value.checked_add(capacity_bytes(planned_images).ok()?))
        .and_then(|value| value.checked_add(planned_name_bytes))
        .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
}

fn execution_plan_capacity_bytes(
    property_uses: &Vec<ResolvedPropertyUse>,
    image_uses: &Vec<ResolvedImageUse>,
    planned_images: &Vec<PlannedImageInvocation>,
    actions: &Vec<ExecutionAction>,
    planned_name_bytes: u64,
) -> Result<u64, ContentVmError> {
    plan_value_capacity_bytes(
        property_uses,
        image_uses,
        planned_images,
        planned_name_bytes,
    )?
    .checked_add(capacity_bytes(actions)?)
    .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
}

fn copy_plan_bytes(
    source_bytes: &[u8],
    program_bytes: u64,
    other_capacity_bytes: u64,
    limits: ContentVmLimits,
    source: ContentOperatorSource,
) -> Result<(Vec<u8>, u64), ContentVmError> {
    let attempted = u64::try_from(source_bytes.len())
        .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
    let consumed = program_bytes.saturating_add(other_capacity_bytes);
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        attempted,
        Some(source),
    )?;
    let mut copied = Vec::new();
    copied.try_reserve_exact(source_bytes.len()).map_err(|_| {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                limits.max_retained_bytes(),
                consumed,
                attempted,
            ),
            Some(source),
        )
    })?;
    copied.extend_from_slice(source_bytes);
    let retained = capacity_bytes(&copied)?;
    limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        consumed,
        retained,
        Some(source),
    )?;
    Ok((copied, retained))
}

#[allow(
    clippy::too_many_arguments,
    reason = "plan growth keeps the exact additional slots and every live VM retention component explicit"
)]
fn reserve_vm_additional<T>(
    values: &mut Vec<T>,
    additional: usize,
    program_bytes: u64,
    other_capacity_bytes: u64,
    limits: ContentVmLimits,
    source: ContentOperatorSource,
    accounting: &mut Accounting,
) -> Result<u64, ContentVmError> {
    let required_capacity = values
        .len()
        .checked_add(additional)
        .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
    if required_capacity > values.capacity() {
        let target_capacity = geometric_capacity(values.capacity(), required_capacity);
        let current_bytes = capacity_bytes(values)?;
        let target_bytes = byte_width::<T>(target_capacity)?;
        let consumed = program_bytes
            .checked_add(other_capacity_bytes)
            .and_then(|value| value.checked_add(current_bytes))
            .unwrap_or(u64::MAX);
        let attempted = target_bytes
            .checked_sub(current_bytes)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            consumed,
            attempted,
            Some(source),
        )?;
        accounting.charge_fuel(
            limits,
            u64::try_from(values.len())
                .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?,
            source,
        )?;
        let reserve = target_capacity
            .checked_sub(values.len())
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        values.try_reserve_exact(reserve).map_err(|_| {
            ContentVmError::resource(
                ContentVmLimit::new(
                    ContentVmLimitKind::Allocation,
                    limits.max_retained_bytes(),
                    consumed,
                    attempted,
                ),
                Some(source),
            )
        })?;
    }
    let total = program_bytes
        .checked_add(other_capacity_bytes)
        .and_then(|value| value.checked_add(capacity_bytes(values).ok()?))
        .unwrap_or(u64::MAX);
    limits.preflight(ContentVmLimitKind::RetainedBytes, 0, total, Some(source))?;
    accounting.observe_retained(total);
    Ok(total)
}

fn byte_width<T>(count: usize) -> Result<u64, ContentVmError> {
    let count = u64::try_from(count)
        .map_err(|_| ContentVmError::new(ContentVmErrorCode::InternalState, None))?;
    let width = u64::try_from(size_of::<T>())
        .map_err(|_| ContentVmError::new(ContentVmErrorCode::InternalState, None))?;
    count
        .checked_mul(width)
        .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
}

struct DocumentCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl ContentCancellation for DocumentCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

#[cfg(test)]
mod retention_tests {
    use pdf_rs_syntax::ObjectRef;

    use super::*;
    use crate::DecodedSpan;

    #[test]
    fn generic_vm_vector_growth_is_geometric_and_charges_live_move_work() {
        let source =
            ContentOperatorSource::new(DecodedSpan::new(ObjectRef::new(4, 0).unwrap(), 0, 0, 1), 0);
        let limits = ContentVmLimits::default();
        let mut accounting = Accounting::default();
        let mut values = Vec::<u8>::new();

        reserve_vm_slot(&mut values, 0, 0, limits, source, &mut accounting)
            .expect("initial reserve");
        let initial_capacity = values.capacity();
        assert!(initial_capacity >= 4);
        assert_eq!(accounting.fuel, 0);
        values.resize(initial_capacity, 0);

        reserve_vm_slot(&mut values, 0, 0, limits, source, &mut accounting).expect("grown reserve");
        assert!(values.capacity() >= initial_capacity * 2);
        assert_eq!(accounting.fuel, u64::try_from(initial_capacity).unwrap());
    }
}
