use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{ByteSource, SourceSnapshot};
use pdf_rs_document::{
    AcquiredPageContent, DocumentCancellation, PagePropertyLookupLimits, PagePropertyLookupStats,
};
use pdf_rs_scene::{
    CommandSource, Matrix, PageGeometry, PageRotation as ScenePageRotation, Scene, SceneBinding,
    SceneBuilder, SceneError, SceneLimits, SceneRect, SceneScalar,
};

use crate::scanner::{ScanTerminal, run_scan};
use crate::{
    ContentCancellation, ContentLimits, ContentName, ContentNumber, ContentOperand,
    ContentOperatorSource, ContentProgram, ContentScanStats, ContentUnsupported,
    ContentUnsupportedKind, ContentVmError, ContentVmErrorCode, ContentVmFailure, ContentVmLimit,
    ContentVmLimitKind, ContentVmLimits, ContentVmPhase, ContentVmStats, DecodedContentStream,
    InterpretedPage, LocatedOperand, OperatorKind, ResolvedPropertyUse,
};

/// One replayable sealed Page-interpretation outcome.
#[derive(Clone)]
pub enum ContentVmPoll {
    /// Complete immutable interpreted Page.
    Ready(Arc<InterpretedPage>),
    /// Validated feature outside the bounded initial VM profile.
    Unsupported(ContentUnsupported),
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

/// Single-owner sealed interpreter for one exact proof-bearing acquired Page.
pub struct InterpretPageJob {
    acquired: Option<AcquiredPageContent>,
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    scene_limits: SceneLimits,
    state: JobState,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
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
            scene_limits,
            state: JobState::Pending,
            scan_stats: ContentScanStats::default(),
            vm_stats: ContentVmStats::default(),
            property_stats: PagePropertyLookupStats::default(),
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
                self.scan_limits,
                self.vm_limits,
                self.property_limits,
                self.scene_limits,
                source,
                cancellation,
            )
        };
        self.scan_stats = report.scan_stats;
        self.vm_stats = report.vm_stats;
        self.property_stats = report.property_stats;

        match report.terminal {
            RunTerminal::Ready(execution) => {
                let acquired = self
                    .acquired
                    .take()
                    .expect("successful pending interpretation retains its acquired Page");
                let page = Arc::new(InterpretedPage::new(
                    acquired,
                    execution.scene,
                    execution.property_uses,
                    execution.final_ctm,
                    self.scan_stats,
                    self.vm_stats,
                    self.property_stats,
                ));
                self.state = JobState::Ready(Arc::clone(&page));
                ContentVmPoll::Ready(page)
            }
            RunTerminal::Unsupported(error) => {
                self.acquired.take();
                self.state = JobState::Unsupported(error);
                ContentVmPoll::Unsupported(error)
            }
            RunTerminal::Failed(error) => {
                self.acquired.take();
                self.state = JobState::Failed(error);
                ContentVmPoll::Failed(error)
            }
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
            .field("scene_limits", &self.scene_limits)
            .field("scan_stats", &self.scan_stats)
            .field("vm_stats", &self.vm_stats)
            .field("property_stats", &self.property_stats)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

struct Execution {
    scene: Scene,
    property_uses: Vec<ResolvedPropertyUse>,
    property_capacity_bytes: u64,
    final_ctm: Matrix,
}

enum RunTerminal {
    Ready(Execution),
    Unsupported(ContentUnsupported),
    Failed(ContentVmFailure),
}

struct RunReport {
    terminal: RunTerminal,
    scan_stats: ContentScanStats,
    vm_stats: ContentVmStats,
    property_stats: PagePropertyLookupStats,
}

#[derive(Default)]
struct Accounting {
    operators: u64,
    fuel: u64,
    max_graphics_depth: u32,
    max_compatibility_depth: u32,
    max_marked_depth: u32,
    property_uses: u64,
    peak_retained: u64,
}

impl Accounting {
    fn observe_retained(&mut self, retained: u64) {
        self.peak_retained = self.peak_retained.max(retained);
    }

    fn snapshot(&self, retained: u64) -> ContentVmStats {
        ContentVmStats::new(
            self.operators,
            self.fuel,
            self.max_graphics_depth,
            self.max_compatibility_depth,
            self.max_marked_depth,
            self.property_uses,
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
    scan_limits: ContentLimits,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    scene_limits: SceneLimits,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
) -> RunReport {
    let snapshot = acquired.handle().snapshot();
    let mut accounting = Accounting::default();
    let mut scan_stats = ContentScanStats::default();
    let mut property_stats = PagePropertyLookupStats::default();

    if let Err(failure) = runtime_guard(snapshot, source, cancellation, None) {
        return report(
            RunTerminal::Failed(failure),
            scan_stats,
            &accounting,
            property_stats,
            0,
        );
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
            let terminal = prioritize(
                snapshot,
                source,
                cancellation,
                None,
                RunTerminal::Failed(ContentVmFailure::Vm(error)),
            );
            return report(terminal, scan_stats, &accounting, property_stats, 0);
        }
    };
    accounting.observe_retained(descriptor_bytes);
    for stream in acquired.streams() {
        descriptors.push(DecodedContentStream::new(
            stream.reference(),
            stream.stream_index(),
            stream.decoded_bytes(),
        ));
    }

    let scan_cancellation = DocumentCancellationAdapter(cancellation);
    let scan = run_scan(&descriptors, scan_limits, &scan_cancellation);
    scan_stats = scan.stats();
    let program = match scan.into_terminal() {
        ScanTerminal::Ready(program) => program,
        ScanTerminal::Failed(error) => {
            accounting
                .observe_retained(descriptor_bytes.saturating_add(scan_stats.retained_bytes()));
            let terminal = prioritize(
                snapshot,
                source,
                cancellation,
                None,
                RunTerminal::Failed(ContentVmFailure::Content(error)),
            );
            return report(terminal, scan_stats, &accounting, property_stats, 0);
        }
    };
    let transient = descriptor_bytes.saturating_add(scan_stats.retained_bytes());
    accounting.observe_retained(transient);
    if let Err(error) = vm_limits.preflight(
        ContentVmLimitKind::RetainedBytes,
        descriptor_bytes,
        scan_stats.retained_bytes(),
        None,
    ) {
        let terminal = prioritize(
            snapshot,
            source,
            cancellation,
            None,
            RunTerminal::Failed(ContentVmFailure::Vm(error)),
        );
        return report(terminal, scan_stats, &accounting, property_stats, 0);
    }
    if let Err(failure) = runtime_guard(snapshot, source, cancellation, None) {
        return report(
            RunTerminal::Failed(failure),
            scan_stats,
            &accounting,
            property_stats,
            0,
        );
    }
    drop(descriptors);

    let program_bytes = scan_stats.retained_bytes();
    let execution = execute_program(
        acquired,
        &program,
        program_bytes,
        vm_limits,
        property_limits,
        scene_limits,
        source,
        cancellation,
        &mut accounting,
    );
    property_stats = execution.property_stats;
    let retained = match &execution.terminal {
        RunTerminal::Ready(value) => value.property_capacity_bytes,
        RunTerminal::Unsupported(_) | RunTerminal::Failed(_) => 0,
    };
    report(
        execution.terminal,
        scan_stats,
        &accounting,
        property_stats,
        retained,
    )
}

fn report(
    terminal: RunTerminal,
    scan_stats: ContentScanStats,
    accounting: &Accounting,
    property_stats: PagePropertyLookupStats,
    retained: u64,
) -> RunReport {
    RunReport {
        terminal,
        scan_stats,
        vm_stats: accounting.snapshot(retained),
        property_stats,
    }
}

struct ExecutionReport {
    terminal: RunTerminal,
    property_stats: PagePropertyLookupStats,
}

#[allow(
    clippy::too_many_arguments,
    reason = "execution keeps source guards and independent sealed budgets explicit"
)]
fn execute_program(
    acquired: &AcquiredPageContent,
    program: &ContentProgram,
    program_bytes: u64,
    vm_limits: ContentVmLimits,
    property_limits: PagePropertyLookupLimits,
    scene_limits: SceneLimits,
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
        let mut scene = SceneBuilder::new(binding, geometry, scene_limits);
        let mut graphics = Vec::new();
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

            let validated = match validate_operands(kind, operator.operands(), operator_source) {
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
            if let Err(error) = admit_operator(
                accounting,
                vm_limits,
                u64::from(kind.spec().base_fuel()),
                operator_source,
            ) {
                return prioritize_vm(snapshot, byte_source, cancellation, operator_source, error);
            }

            match kind {
                OperatorKind::SaveGraphicsState => {
                    let graphics_depth = match u64::try_from(graphics.len()) {
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
                    let retained = match reserve_vm_slot(
                        &mut graphics,
                        program_bytes,
                        capacity_bytes(&property_uses).unwrap_or(u64::MAX),
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
                    accounting.observe_retained(retained);
                    graphics.push(current_ctm);
                    let graphics_depth = match u32::try_from(graphics.len()) {
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
                OperatorKind::ConcatMatrix => {
                    let ValidatedOperands::Matrix(numbers) = validated else {
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
                    if let Err(error) =
                        scene.begin_marked_content(tag.bytes(), None, command_source)
                    {
                        return prioritize_scene(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
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
                    let retained = match reserve_vm_slot(
                        &mut property_uses,
                        program_bytes,
                        capacity_bytes(&graphics).unwrap_or(u64::MAX),
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
                    if let Err(error) = scene.begin_marked_content(
                        tag.bytes(),
                        Some(proof.target()),
                        command_source,
                    ) {
                        return prioritize_scene(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
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
                    if let Err(error) = scene.end_marked_content(command_source) {
                        return prioritize_scene(
                            snapshot,
                            byte_source,
                            cancellation,
                            operator_source,
                            error,
                        );
                    }
                    marked_depth -= 1;
                }
            }
        }

        for (unbalanced, code) in [
            (
                !graphics.is_empty(),
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
        let property_capacity_bytes = capacity_bytes(&property_uses).unwrap_or(u64::MAX);
        RunTerminal::Ready(Execution {
            scene,
            property_uses,
            property_capacity_bytes,
            final_ctm: current_ctm,
        })
    })();

    ExecutionReport {
        terminal,
        property_stats: resolver.stats(),
    }
}

enum PropertyOperand<'a> {
    Name(&'a ContentName),
    Dictionary,
}

enum ValidatedOperands<'a> {
    None,
    Matrix([ContentNumber; 6]),
    Name(&'a ContentName),
    NameAndProperty {
        tag: &'a ContentName,
        property: PropertyOperand<'a>,
    },
}

fn validate_operands<'a>(
    kind: OperatorKind,
    operands: &'a [LocatedOperand],
    source: ContentOperatorSource,
) -> Result<ValidatedOperands<'a>, ContentVmError> {
    let spec = kind.spec();
    if operands.len() != usize::from(spec.min_operands()) {
        return Err(vm_error(ContentVmErrorCode::InvalidOperandCount, source));
    }
    match kind {
        OperatorKind::ConcatMatrix => {
            let mut numbers = [ContentNumber::ZERO; 6];
            for (output, operand) in numbers.iter_mut().zip(operands) {
                *output = match operand.value() {
                    ContentOperand::Integer(value) => ContentNumber::from_integer(*value),
                    ContentOperand::Real(value) => ContentNumber::parse(value.raw()),
                    _ => Err(vm_error(ContentVmErrorCode::InvalidOperandType, source)),
                }
                .map_err(|error| error.with_source(source))?;
            }
            Ok(ValidatedOperands::Matrix(numbers))
        }
        OperatorKind::MarkedContentPoint | OperatorKind::BeginMarkedContent => {
            let ContentOperand::Name(name) = operands[0].value() else {
                return Err(vm_error(ContentVmErrorCode::InvalidOperandType, source));
            };
            Ok(ValidatedOperands::Name(name))
        }
        OperatorKind::MarkedContentPointProperties | OperatorKind::BeginMarkedContentProperties => {
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
        _ => Ok(ValidatedOperands::None),
    }
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
) -> Result<u64, ContentVmError> {
    if values.len() == values.capacity() {
        let consumed = program_bytes
            .checked_add(other_capacity_bytes)
            .and_then(|value| value.checked_add(capacity_bytes(values).ok()?))
            .unwrap_or(u64::MAX);
        let attempted = byte_width::<T>(1)?;
        limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            consumed,
            attempted,
            Some(source),
        )?;
        values.try_reserve_exact(1).map_err(|_| {
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
    Ok(total)
}

fn capacity_bytes<T>(values: &Vec<T>) -> Result<u64, ContentVmError> {
    byte_width::<T>(values.capacity())
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
