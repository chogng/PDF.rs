use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{ByteSource, DataTicket, ResumeCheckpoint, SmallRanges, SourceSnapshot};
use pdf_rs_document::{
    AcquiredPageContent, DocumentCancellation, ImageXObjectJobContext, ImageXObjectLimits,
    PagePropertyLookupLimits, PagePropertyLookupStats, PageXObjectLookupLimits,
    PageXObjectLookupOutcome, PageXObjectLookupStats, SharedAttestedRevisionIndex,
};
use pdf_rs_scene::{
    CommandSource, DashPattern, DashPatternBuilder, FillRule, GraphicsSceneBuilder,
    GraphicsSceneLimits, LineStyle, Matrix, PageGeometry, PageRotation as ScenePageRotation, Paint,
    PathResource, Scene, SceneBinding, SceneBounds, SceneBuilder, SceneError, SceneLimits,
    SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

use crate::scanner::{ScanTerminal, run_scan};
use crate::{
    ContentCancellation, ContentGraphicsLimits, ContentImageLimits, ContentImageStats,
    ContentLimits, ContentName, ContentNumber, ContentOperand, ContentOperatorSource,
    ContentProgram, ContentScanStats, ContentUnsupported, ContentUnsupportedKind, ContentVmError,
    ContentVmErrorCode, ContentVmFailure, ContentVmLimit, ContentVmLimitKind, ContentVmLimits,
    ContentVmPhase, ContentVmStats, DecodedContentStream, InterpretedPage, LocatedOperand,
    OperatorContext, OperatorKind, OperatorOperandShape, ResolvedImageUse, ResolvedPropertyUse,
};

mod graphics;
mod image;

use graphics::{DashRetentionAdmission, GraphicsExecutionError, GraphicsVm, VmRetention};
use image::{ImagePlanningPoll, ImageRuntime};

const DASH_CANCELLATION_INTERVAL: usize = 256;

/// One replayable sealed Page-interpretation outcome.
#[derive(Clone)]
pub enum ContentVmPoll {
    /// Complete immutable interpreted Page.
    Ready(Arc<InterpretedPage>),
    /// Validated feature outside the bounded initial VM profile.
    Unsupported(ContentUnsupported),
    /// One proof-bound Image XObject object or payload requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the request.
        missing: SmallRanges,
        /// Object-envelope, stream-boundary, or payload checkpoint to retain.
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
    profile: ContentVmProfile,
    image_runtime: Option<ImageRuntime>,
    program: Option<ContentProgram>,
    plan: Option<ExecutionPlan>,
    scan_peak_retained: u64,
    state: JobState,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
    xobject_stats: PageXObjectLookupStats,
    image_stats: ContentImageStats,
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
            profile: ContentVmProfile::SceneV1 { scene_limits },
            image_runtime: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
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
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: None,
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
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
            profile: ContentVmProfile::GraphicsV2 {
                graphics_limits,
                scene_limits,
            },
            image_runtime: Some(ImageRuntime::new(image_profile)),
            program: None,
            plan: None,
            scan_peak_retained: 0,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
            xobject_stats: PageXObjectLookupStats::default(),
            image_stats: ContentImageStats::default(),
        }
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
                acquired,
                &mut self.program,
                &mut self.plan,
                self.scan_limits,
                self.vm_limits,
                self.property_limits,
                self.xobject_limits,
                self.profile,
                self.image_runtime.as_mut(),
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
                    execution.final_ctm,
                    self.scan_stats,
                    self.vm_stats,
                    self.property_stats,
                    self.xobject_stats,
                    self.image_stats,
                ));
                self.image_runtime.take();
                self.program.take();
                self.plan.take();
                self.state = JobState::Ready(Arc::clone(&page));
                ContentVmPoll::Ready(page)
            }
            RunTerminal::Unsupported(error) => {
                self.acquired.take();
                self.image_runtime.take();
                self.program.take();
                self.plan.take();
                self.state = JobState::Unsupported(error);
                ContentVmPoll::Unsupported(error)
            }
            RunTerminal::Failed(error) => {
                self.acquired.take();
                self.image_runtime.take();
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
            .field("images_enabled", &self.image_runtime.is_some())
            .field("program_retained", &self.program.is_some())
            .field("plan_retained", &self.plan.is_some())
            .field("profile", &self.profile)
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("property_stats", &self.property_stats)
            .field("xobject_stats", &self.xobject_stats)
            .field("image_stats", &self.image_stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

struct Execution {
    scene: Scene,
    property_uses: Vec<ResolvedPropertyUse>,
    image_uses: Vec<ResolvedImageUse>,
    retained_use_capacity_bytes: u64,
    final_ctm: Matrix,
}

struct PlannedImageInvocation {
    source: ContentOperatorSource,
    name: Vec<u8>,
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
}

struct ExecutionPlan {
    binding: SceneBinding,
    geometry: PageGeometry,
    actions: Vec<ExecutionAction>,
    image_invocations: Vec<PlannedImageInvocation>,
    property_uses: Vec<ResolvedPropertyUse>,
    image_uses: Vec<ResolvedImageUse>,
    final_ctm: Matrix,
    action_payload_retained_bytes: u64,
    owned_name_retained_bytes: u64,
    accounting: Accounting,
    property_stats: PagePropertyLookupStats,
}

impl ExecutionPlan {
    fn image_plan_retained_bytes(&self) -> Result<u64, ContentVmError> {
        capacity_bytes(&self.actions)?
            .checked_add(capacity_bytes(&self.image_invocations)?)
            .and_then(|value| value.checked_add(self.owned_name_retained_bytes))
            .and_then(|value| value.checked_add(self.action_payload_retained_bytes))
            .ok_or_else(|| ContentVmError::new(ContentVmErrorCode::InternalState, None))
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
    clippy::too_many_arguments,
    reason = "the sealed interpreter receives each independently validated lower limit profile"
)]
fn run_interpretation(
    acquired: &AcquiredPageContent,
    program_slot: &mut Option<ContentProgram>,
    plan_slot: &mut Option<ExecutionPlan>,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    xobject_limits: PageXObjectLookupLimits,
    profile: ContentVmProfile,
    mut image_runtime: Option<&mut ImageRuntime>,
    mut scan_stats: ContentScanStats,
    mut xobject_stats: PageXObjectLookupStats,
    mut scan_peak_retained: u64,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> RunReport {
    let snapshot = acquired.handle().snapshot();
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
            let Some(input_bytes) = acquired.streams().iter().try_fold(0_u64, |total, stream| {
                total.checked_add(u64::try_from(stream.decoded_bytes().len()).ok()?)
            }) else {
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
            };
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
        let descriptor_bytes = match reserve_exact_slots(
            &mut descriptors,
            acquired.streams().len(),
            0,
            vm_limits,
            None,
        ) {
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
        for stream in acquired.streams() {
            descriptors.push(DecodedContentStream::new(
                stream.reference(),
                stream.stream_index(),
                stream.decoded_bytes(),
            ));
        }
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
            acquired,
            program,
            program_bytes,
            vm_limits,
            property_limits,
            profile,
            image_runtime.as_deref_mut(),
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
            .expect("a retained execution plan remains immutable across image Pending");
        accounting = plan.accounting;
        property_stats = plan.property_stats;
    }

    let plan = plan_slot
        .as_ref()
        .expect("semantic planning publishes one immutable execution plan");

    if let Some(runtime) = image_runtime.as_deref_mut() {
        if !runtime.plan_complete()
            && let Err(terminal) = plan_image_resources(
                acquired,
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
        if !runtime.acquisitions_complete() {
            let terminal = match runtime.poll_acquisitions(source, cancellation) {
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
                ImagePlanningPoll::Unsupported {
                    unsupported,
                    source,
                } => Some(RunTerminal::Unsupported(ContentUnsupported::from_image(
                    unsupported,
                    source,
                ))),
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
        if let Err(error) = runtime.begin_execution() {
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
    let plan = plan_slot
        .take()
        .expect("all resources ready before consuming the immutable execution plan");
    let terminal = materialize_execution_plan(
        acquired,
        plan,
        profile,
        image_runtime.as_deref_mut(),
        source,
        cancellation,
    );
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
    clippy::too_many_arguments,
    reason = "image planning keeps each sealed lower limit and runtime authority explicit"
)]
#[allow(
    clippy::result_large_err,
    reason = "planning preserves typed Unsupported, Document, VM, and source-guard terminals"
)]
fn plan_image_resources(
    acquired: &AcquiredPageContent,
    plan: &ExecutionPlan,
    xobject_limits: PageXObjectLookupLimits,
    runtime: &mut ImageRuntime,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    xobject_stats: &mut PageXObjectLookupStats,
) -> Result<(), RunTerminal> {
    let snapshot = acquired.handle().snapshot();
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

    let mut resolver = acquired.page().resources().xobject_resolver(xobject_limits);
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
                .register_proof(proof, source, byte_source, cancellation)
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

struct ExecutionReport {
    terminal: RunTerminal,
    property_stats: PagePropertyLookupStats,
}

#[allow(
    clippy::too_many_arguments,
    reason = "execution keeps source guards and independent sealed budgets explicit"
)]
fn build_execution_plan(
    acquired: &AcquiredPageContent,
    program: &ContentProgram,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    profile: ContentVmProfile,
    mut image_runtime: Option<&mut ImageRuntime>,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    accounting: &mut Accounting,
) -> ExecutionReport {
    let snapshot = acquired.handle().snapshot();
    let mut resolver = acquired
        .page()
        .resources()
        .property_resolver(property_limits);
    let mut property_uses = Vec::new();
    let mut planned_images = Vec::new();
    let mut image_uses = Vec::new();
    let mut planned_name_bytes = 0_u64;
    let terminal = (|| {
        let (binding, geometry) = match scene_context(acquired) {
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
        let mut current_ctm = Matrix::IDENTITY;
        let mut text_active = false;
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

            match kind {
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
                OperatorKind::BeginText => {
                    if text_active {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InvalidTextObject, operator_source),
                        );
                    }
                    text_active = true;
                }
                OperatorKind::EndText => {
                    if !text_active {
                        return prioritize_vm(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            vm_error(ContentVmErrorCode::InvalidTextObject, operator_source),
                        );
                    }
                    text_active = false;
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
        let final_ctm = graphics_v2
            .as_ref()
            .map_or(current_ctm, GraphicsVm::current_ctm);
        let action_payload_retained_bytes = match graphics_v2
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
        let plan = ExecutionPlan {
            binding,
            geometry,
            actions,
            image_invocations: planned_images,
            property_uses,
            image_uses,
            final_ctm,
            action_payload_retained_bytes,
            owned_name_retained_bytes: planned_name_bytes,
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
        RunTerminal::Planned(plan)
    })();

    ExecutionReport {
        terminal,
        property_stats: resolver.stats(),
    }
}

fn materialize_execution_plan(
    acquired: &AcquiredPageContent,
    plan: ExecutionPlan,
    profile: ContentVmProfile,
    mut image_runtime: Option<&mut ImageRuntime>,
    byte_source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> RunTerminal {
    let snapshot = acquired.handle().snapshot();
    let mut scene = match profile {
        ContentVmProfile::SceneV1 { scene_limits } => {
            SceneSink::V1(SceneBuilder::new(plan.binding, plan.geometry, scene_limits))
        }
        ContentVmProfile::GraphicsV2 { scene_limits, .. } => SceneSink::V2(
            GraphicsSceneBuilder::new_v2(plan.binding, plan.geometry, scene_limits),
        ),
    };
    let mut image_uses = plan.image_uses;
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
            ExecutionAction::Save { bounds, source } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_save(bounds, source),
            ExecutionAction::Restore { bounds, source } => scene
                .graphics_mut()
                .expect("only graphics-v2 plans contain graphics actions")
                .append_restore(bounds, source),
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
                let (proof, image) = match runtime.resolve_planned(source) {
                    Ok(value) => value,
                    Err(error) => {
                        return prioritize_vm(snapshot, byte_source, cancellation, source, error);
                    }
                };
                let resource_source = image.source();
                let result = scene
                    .graphics_mut()
                    .expect("image plans require the graphics-v2 Scene")
                    .draw_image(image, transform, alpha, blend_mode, bounds, command_source);
                if result.is_ok() {
                    image_uses.push(ResolvedImageUse::new(source, proof, resource_source));
                    if let Err(error) = runtime.record_executed_use(source) {
                        return prioritize_vm(snapshot, byte_source, cancellation, source, error);
                    }
                }
                result
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
        .unwrap_or(u64::MAX);
    RunTerminal::Ready(Execution {
        scene,
        property_uses: plan.property_uses,
        image_uses,
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
        | ExecutionAction::Clip { source, .. }
        | ExecutionAction::Fill { source, .. }
        | ExecutionAction::Stroke { source, .. }
        | ExecutionAction::FillStroke { source, .. } => *source,
        ExecutionAction::DrawImage { source, .. } => return *source,
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

fn scene_context(
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
