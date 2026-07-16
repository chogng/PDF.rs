use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_scene::{
    BlendMode, DashPattern, DeviceColor, FillRule, GraphicsSceneBuilder, LineCap, LineJoin,
    LineStyle, Matrix, Paint, PathResource, PathResourceBuilder, PathSegment, SceneBounds,
    SceneError, ScenePoint, SceneScalar, SceneUnit,
};

use super::{Accounting, ValidatedOperands, command_source, geometric_capacity, vm_error};
use crate::{
    ContentGraphicsLimitKind, ContentGraphicsLimits, ContentNumber, ContentOperatorSource,
    ContentVmError, ContentVmErrorCode, ContentVmLimit, ContentVmLimitKind, ContentVmLimits,
    OperatorKind,
};

pub(super) enum GraphicsExecutionError {
    Vm(ContentVmError),
    Scene(SceneError),
}

impl From<ContentVmError> for GraphicsExecutionError {
    fn from(value: ContentVmError) -> Self {
        Self::Vm(value)
    }
}

impl From<SceneError> for GraphicsExecutionError {
    fn from(value: SceneError) -> Self {
        Self::Scene(value)
    }
}

#[derive(Clone)]
pub(super) struct GraphicsState {
    ctm: Matrix,
    line_width: SceneScalar,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: SceneScalar,
    dash: DashPattern,
    dash_ownership: Arc<DashOwnership>,
    stroking: Paint,
    nonstroking: Paint,
    clip_depth: u32,
}

struct DashOwnership {
    retained_bytes: u64,
}

impl GraphicsState {
    fn initial() -> Self {
        let black = Paint::new(
            DeviceColor::Gray(SceneUnit::ZERO),
            SceneUnit::ONE,
            BlendMode::Normal,
        );
        let dash =
            DashPattern::new(Vec::new(), SceneScalar::ZERO).expect("the PDF default dash is valid");
        Self {
            ctm: Matrix::IDENTITY,
            line_width: SceneScalar::ONE,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: SceneScalar::from_scaled(10_000_000_000),
            dash,
            dash_ownership: Arc::new(DashOwnership { retained_bytes: 0 }),
            stroking: black,
            nonstroking: black,
            clip_depth: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct PendingClip {
    rule: FillRule,
    source: ContentOperatorSource,
}

#[derive(Default)]
struct CurrentPath {
    segments: PathResourceBuilder,
    current_point: Option<ScenePoint>,
    subpath_start: Option<ScenePoint>,
    pending_clip: Option<PendingClip>,
}

impl CurrentPath {
    fn reserve(
        &mut self,
        additional: usize,
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), ContentVmError> {
        if additional == 0 {
            return Ok(());
        }
        let consumed = u64::try_from(self.segments.len())
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        let attempted = u64::try_from(additional)
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        limits.preflight(
            ContentGraphicsLimitKind::PathSegments,
            consumed,
            attempted,
            source,
        )?;

        let desired = self
            .segments
            .len()
            .checked_add(additional)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let width = u64::try_from(size_of::<PathSegment>())
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        let target_capacity = geometric_capacity(self.segments.capacity(), desired);
        let target_bytes = u64::try_from(target_capacity)
            .ok()
            .and_then(|value| value.checked_mul(width))
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let current_bytes = self
            .segments
            .retained_bytes()
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        let attempted_bytes = target_bytes.saturating_sub(current_bytes);
        limits.preflight(
            ContentGraphicsLimitKind::PathRetainedBytes,
            current_bytes,
            attempted_bytes,
            source,
        )?;
        let vm_consumed = vm_consumed_without_path.saturating_add(current_bytes);
        vm_limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            vm_consumed,
            attempted_bytes,
            Some(source),
        )?;
        if target_capacity > self.segments.capacity() {
            accounting.charge_fuel(
                vm_limits,
                u64::try_from(self.segments.len())
                    .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?,
                source,
            )?;
            let reserve_additional = target_capacity
                .checked_sub(self.segments.len())
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            self.segments
                .try_reserve_exact(reserve_additional)
                .map_err(|_| {
                    ContentVmError::resource(
                        ContentVmLimit::new(
                            ContentVmLimitKind::Allocation,
                            vm_limits.max_retained_bytes(),
                            vm_consumed,
                            attempted_bytes,
                        ),
                        Some(source),
                    )
                })?;
        }
        let actual = self
            .segments
            .retained_bytes()
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        let actual_attempted = actual.saturating_sub(current_bytes);
        limits.preflight(
            ContentGraphicsLimitKind::PathRetainedBytes,
            current_bytes,
            actual_attempted,
            source,
        )?;
        vm_limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            vm_consumed_without_path,
            actual,
            Some(source),
        )
    }

    fn move_to(
        &mut self,
        point: ScenePoint,
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), ContentVmError> {
        self.reserve(
            1,
            limits,
            vm_limits,
            vm_consumed_without_path,
            source,
            accounting,
        )?;
        self.push(PathSegment::MoveTo(point), source)?;
        self.current_point = Some(point);
        self.subpath_start = Some(point);
        Ok(())
    }

    fn line_to(
        &mut self,
        point: ScenePoint,
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), ContentVmError> {
        let Some(current) = self.current_point else {
            return Err(vm_error(ContentVmErrorCode::InvalidPathState, source));
        };
        let reopen = self.subpath_start.is_none();
        self.reserve(
            if reopen { 2 } else { 1 },
            limits,
            vm_limits,
            vm_consumed_without_path,
            source,
            accounting,
        )?;
        if reopen {
            self.push(PathSegment::MoveTo(current), source)?;
            self.subpath_start = Some(current);
        }
        self.push(PathSegment::LineTo(point), source)?;
        self.current_point = Some(point);
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one cubic append keeps its three points and both independent retention budgets explicit"
    )]
    fn cubic_to(
        &mut self,
        control_1: ScenePoint,
        control_2: ScenePoint,
        end: ScenePoint,
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), ContentVmError> {
        let Some(current) = self.current_point else {
            return Err(vm_error(ContentVmErrorCode::InvalidPathState, source));
        };
        let reopen = self.subpath_start.is_none();
        self.reserve(
            if reopen { 2 } else { 1 },
            limits,
            vm_limits,
            vm_consumed_without_path,
            source,
            accounting,
        )?;
        if reopen {
            self.push(PathSegment::MoveTo(current), source)?;
            self.subpath_start = Some(current);
        }
        self.push(
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            },
            source,
        )?;
        self.current_point = Some(end);
        Ok(())
    }

    fn close(
        &mut self,
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), ContentVmError> {
        let Some(start) = self.subpath_start else {
            return Ok(());
        };
        self.reserve(
            1,
            limits,
            vm_limits,
            vm_consumed_without_path,
            source,
            accounting,
        )?;
        self.push(PathSegment::ClosePath, source)?;
        self.current_point = Some(start);
        self.subpath_start = None;
        Ok(())
    }

    fn take_resource(&mut self) -> (PathResource, Matrix, SceneBounds, Option<PendingClip>) {
        let segments = std::mem::take(&mut self.segments);
        let bounds = if segments.is_empty() {
            SceneBounds::Empty
        } else {
            SceneBounds::Page
        };
        let resource = segments.finish();
        let pending_clip = self.pending_clip.take();
        self.current_point = None;
        self.subpath_start = None;
        (resource, Matrix::IDENTITY, bounds, pending_clip)
    }

    fn retained_bytes(&self, source: ContentOperatorSource) -> Result<u64, ContentVmError> {
        self.segments
            .retained_bytes()
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))
    }

    fn push(
        &mut self,
        segment: PathSegment,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        self.segments
            .try_push(segment)
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))
    }
}

pub(super) struct GraphicsVm {
    current: GraphicsState,
    saved: Vec<GraphicsState>,
    path: CurrentPath,
    dash_retained_bytes: u64,
}

#[derive(Clone, Copy)]
pub(super) struct VmRetention {
    program_bytes: u64,
    property_bytes: u64,
    limits: ContentVmLimits,
}

impl VmRetention {
    pub(super) const fn new(
        program_bytes: u64,
        property_bytes: u64,
        limits: ContentVmLimits,
    ) -> Self {
        Self {
            program_bytes,
            property_bytes,
            limits,
        }
    }

    fn total_with(self, machine_bytes: u64) -> u64 {
        self.program_bytes
            .checked_add(self.property_bytes)
            .and_then(|value| value.checked_add(machine_bytes))
            .unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Copy)]
pub(super) struct DashRetentionAdmission {
    graphics_consumed: u64,
    vm_consumed: u64,
    graphics_limits: ContentGraphicsLimits,
    vm_limits: ContentVmLimits,
    source: ContentOperatorSource,
}

impl DashRetentionAdmission {
    pub(super) fn preflight_actual(self, actual: u64) -> Result<(), ContentVmError> {
        self.graphics_limits.preflight(
            ContentGraphicsLimitKind::DashRetainedBytes,
            self.graphics_consumed,
            actual,
            self.source,
        )?;
        self.vm_limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            self.vm_consumed,
            actual,
            Some(self.source),
        )
    }

    pub(super) fn allocation_error(self, attempted: u64) -> ContentVmError {
        ContentVmError::resource(
            ContentVmLimit::new(
                ContentVmLimitKind::Allocation,
                self.vm_limits.max_retained_bytes(),
                self.vm_consumed,
                attempted,
            ),
            Some(self.source),
        )
    }

    pub(super) fn retained_with_candidate(self, actual: u64) -> u64 {
        self.vm_consumed.saturating_add(actual)
    }
}

impl GraphicsVm {
    pub(super) fn new() -> Self {
        Self {
            current: GraphicsState::initial(),
            saved: Vec::new(),
            path: CurrentPath::default(),
            dash_retained_bytes: 0,
        }
    }

    pub(super) const fn current_ctm(&self) -> Matrix {
        self.current.ctm
    }

    pub(super) fn set_ctm(&mut self, value: Matrix) {
        self.current.ctm = value;
    }

    pub(super) fn saved(&self) -> &[GraphicsState] {
        &self.saved
    }

    pub(super) fn push_current(&mut self) {
        self.saved.push(self.current.clone());
    }

    pub(super) fn restore(
        &mut self,
        source: ContentOperatorSource,
    ) -> Result<Option<Matrix>, ContentVmError> {
        let Some(restored) = self.saved.pop() else {
            return Ok(None);
        };
        let released = if Arc::strong_count(&self.current.dash_ownership) == 1 {
            self.current.dash_ownership.retained_bytes
        } else {
            0
        };
        self.dash_retained_bytes = self
            .dash_retained_bytes
            .checked_sub(released)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        self.current = restored;
        Ok(Some(self.current.ctm))
    }

    pub(super) fn reserve_saved_slot(
        &mut self,
        retention: VmRetention,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<u64, ContentVmError> {
        let path_bytes = self.path.retained_bytes(source)?;
        let saved_bytes = retained_bytes(&self.saved)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let fixed = retention
            .program_bytes
            .checked_add(retention.property_bytes)
            .and_then(|value| value.checked_add(path_bytes))
            .and_then(|value| value.checked_add(self.dash_retained_bytes))
            .unwrap_or(u64::MAX);
        if self.saved.len() == self.saved.capacity() {
            let required_capacity = self
                .saved
                .len()
                .checked_add(1)
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            let target_capacity = geometric_capacity(self.saved.capacity(), required_capacity);
            let consumed = fixed.saturating_add(saved_bytes);
            let width = u64::try_from(size_of::<GraphicsState>())
                .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
            let target_bytes = u64::try_from(target_capacity)
                .ok()
                .and_then(|value| value.checked_mul(width))
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            let attempted = target_bytes.saturating_sub(saved_bytes);
            retention.limits.preflight(
                ContentVmLimitKind::RetainedBytes,
                consumed,
                attempted,
                Some(source),
            )?;
            accounting.charge_fuel(
                retention.limits,
                u64::try_from(self.saved.len())
                    .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?,
                source,
            )?;
            let additional = target_capacity
                .checked_sub(self.saved.len())
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
            self.saved.try_reserve_exact(additional).map_err(|_| {
                ContentVmError::resource(
                    ContentVmLimit::new(
                        ContentVmLimitKind::Allocation,
                        retention.limits.max_retained_bytes(),
                        consumed,
                        attempted,
                    ),
                    Some(source),
                )
            })?;
        }
        let actual = retained_bytes(&self.saved)
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let total = fixed.saturating_add(actual);
        retention
            .limits
            .preflight(ContentVmLimitKind::RetainedBytes, 0, total, Some(source))?;
        Ok(total)
    }

    pub(super) fn retained_capacity_bytes(
        &self,
        source: ContentOperatorSource,
    ) -> Result<u64, ContentVmError> {
        retained_bytes(&self.saved)
            .and_then(|saved| {
                self.path
                    .retained_bytes(source)
                    .ok()
                    .and_then(|path| saved.checked_add(path))
            })
            .and_then(|value| value.checked_add(self.dash_retained_bytes))
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))
    }

    pub(super) fn preflight_dash_candidate(
        &self,
        entries: u64,
        expected_bytes: u64,
        graphics_limits: ContentGraphicsLimits,
        retention: VmRetention,
        source: ContentOperatorSource,
    ) -> Result<DashRetentionAdmission, ContentVmError> {
        graphics_limits.preflight(ContentGraphicsLimitKind::DashEntries, 0, entries, source)?;
        graphics_limits.preflight(
            ContentGraphicsLimitKind::DashRetainedBytes,
            self.dash_retained_bytes,
            expected_bytes,
            source,
        )?;
        let vm_consumed = retention.total_with(self.retained_capacity_bytes(source)?);
        retention.limits.preflight(
            ContentVmLimitKind::RetainedBytes,
            vm_consumed,
            expected_bytes,
            Some(source),
        )?;
        Ok(DashRetentionAdmission {
            graphics_consumed: self.dash_retained_bytes,
            vm_consumed,
            graphics_limits,
            vm_limits: retention.limits,
            source,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "graphics execution keeps operands, independent limits, Scene publication, provenance, and move accounting explicit"
    )]
    pub(super) fn execute(
        &mut self,
        kind: OperatorKind,
        operands: &ValidatedOperands<'_>,
        limits: ContentGraphicsLimits,
        retention: VmRetention,
        builder: &mut GraphicsSceneBuilder,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<u64, GraphicsExecutionError> {
        let path_vm_consumed = retention
            .program_bytes
            .checked_add(retention.property_bytes)
            .and_then(|value| {
                retained_bytes(&self.saved).and_then(|saved| value.checked_add(saved))
            })
            .and_then(|value| value.checked_add(self.dash_retained_bytes))
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        let mut transient_machine_bytes = 0;
        match kind {
            OperatorKind::MoveTo => {
                let ValidatedOperands::TwoNumbers(values) = operands else {
                    unreachable!("validated m operands have two-number shape");
                };
                let point = self.current.ctm.checked_transform_point(point(*values))?;
                self.path.move_to(
                    point,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::LineTo => {
                let ValidatedOperands::TwoNumbers(values) = operands else {
                    unreachable!("validated l operands have two-number shape");
                };
                let point = self.current.ctm.checked_transform_point(point(*values))?;
                self.path.line_to(
                    point,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CubicCurveTo => {
                let ValidatedOperands::SixNumbers(values) = operands else {
                    unreachable!("validated c operands have six-number shape");
                };
                let control_1 = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[0], values[1]]))?;
                let control_2 = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[2], values[3]]))?;
                let end = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[4], values[5]]))?;
                self.path.cubic_to(
                    control_1,
                    control_2,
                    end,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CubicCurveToReplicateInitial => {
                let ValidatedOperands::FourNumbers(values) = operands else {
                    unreachable!("validated v operands have four-number shape");
                };
                let current = self
                    .path
                    .current_point
                    .ok_or_else(|| vm_error(ContentVmErrorCode::InvalidPathState, source))?;
                let control_2 = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[0], values[1]]))?;
                let end = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[2], values[3]]))?;
                self.path.cubic_to(
                    current,
                    control_2,
                    end,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CubicCurveToReplicateFinal => {
                let ValidatedOperands::FourNumbers(values) = operands else {
                    unreachable!("validated y operands have four-number shape");
                };
                let control_1 = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[0], values[1]]))?;
                let end = self
                    .current
                    .ctm
                    .checked_transform_point(point([values[2], values[3]]))?;
                self.path.cubic_to(
                    control_1,
                    end,
                    end,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::ClosePath => {
                self.path.close(
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::Rectangle => {
                let ValidatedOperands::FourNumbers(values) = operands else {
                    unreachable!("validated re operands have four-number shape");
                };
                self.append_rectangle(
                    *values,
                    limits,
                    retention.limits,
                    path_vm_consumed,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::StrokePath => {
                transient_machine_bytes = self.paint(
                    PaintOperation::Stroke,
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CloseAndStrokePath => {
                transient_machine_bytes = self.paint(
                    PaintOperation::Stroke,
                    true,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::FillNonzero | OperatorKind::FillNonzeroLegacy => {
                transient_machine_bytes = self.paint(
                    PaintOperation::Fill(FillRule::Nonzero),
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::FillEvenOdd => {
                transient_machine_bytes = self.paint(
                    PaintOperation::Fill(FillRule::EvenOdd),
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::FillStrokeNonzero => {
                transient_machine_bytes = self.paint(
                    PaintOperation::FillStroke(FillRule::Nonzero),
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::FillStrokeEvenOdd => {
                transient_machine_bytes = self.paint(
                    PaintOperation::FillStroke(FillRule::EvenOdd),
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CloseFillStrokeNonzero => {
                transient_machine_bytes = self.paint(
                    PaintOperation::FillStroke(FillRule::Nonzero),
                    true,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::CloseFillStrokeEvenOdd => {
                transient_machine_bytes = self.paint(
                    PaintOperation::FillStroke(FillRule::EvenOdd),
                    true,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::EndPath => {
                transient_machine_bytes = self.paint(
                    PaintOperation::None,
                    false,
                    limits,
                    retention,
                    path_vm_consumed,
                    builder,
                    source,
                    accounting,
                )?;
            }
            OperatorKind::ClipNonzero => {
                self.path.pending_clip = Some(PendingClip {
                    rule: FillRule::Nonzero,
                    source,
                });
            }
            OperatorKind::ClipEvenOdd => {
                self.path.pending_clip = Some(PendingClip {
                    rule: FillRule::EvenOdd,
                    source,
                });
            }
            OperatorKind::SetLineWidth => {
                let ValidatedOperands::OneNumber(value) = operands else {
                    unreachable!("validated w operand has one-number shape");
                };
                self.current.line_width = nonnegative(*value, source)?;
            }
            OperatorKind::SetLineCap => {
                let ValidatedOperands::OneInteger(value) = operands else {
                    unreachable!("validated J operand has integer shape");
                };
                self.current.line_cap = match value {
                    0 => LineCap::Butt,
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => {
                        return Err(
                            vm_error(ContentVmErrorCode::InvalidGraphicsParameter, source).into(),
                        );
                    }
                };
            }
            OperatorKind::SetLineJoin => {
                let ValidatedOperands::OneInteger(value) = operands else {
                    unreachable!("validated j operand has integer shape");
                };
                self.current.line_join = match value {
                    0 => LineJoin::Miter,
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => {
                        return Err(
                            vm_error(ContentVmErrorCode::InvalidGraphicsParameter, source).into(),
                        );
                    }
                };
            }
            OperatorKind::SetMiterLimit => {
                let ValidatedOperands::OneNumber(value) = operands else {
                    unreachable!("validated M operand has one-number shape");
                };
                let value = scalar(*value);
                if value < SceneScalar::ONE {
                    return Err(
                        vm_error(ContentVmErrorCode::InvalidGraphicsParameter, source).into(),
                    );
                }
                self.current.miter_limit = value;
            }
            OperatorKind::SetLineDash => {
                let ValidatedOperands::Dash { pattern } = operands else {
                    unreachable!("validated d operands have dash shape");
                };
                self.set_dash(pattern.clone(), source)?;
            }
            OperatorKind::SetStrokingGray => {
                self.current.stroking = update_color(
                    self.current.stroking,
                    DeviceColor::Gray(unit(one_number(operands))),
                );
            }
            OperatorKind::SetNonstrokingGray => {
                self.current.nonstroking = update_color(
                    self.current.nonstroking,
                    DeviceColor::Gray(unit(one_number(operands))),
                );
            }
            OperatorKind::SetStrokingRgb => {
                self.current.stroking =
                    update_color(self.current.stroking, rgb(three_numbers(operands)));
            }
            OperatorKind::SetNonstrokingRgb => {
                self.current.nonstroking =
                    update_color(self.current.nonstroking, rgb(three_numbers(operands)));
            }
            OperatorKind::SetStrokingCmyk => {
                self.current.stroking =
                    update_color(self.current.stroking, cmyk(four_numbers(operands)));
            }
            OperatorKind::SetNonstrokingCmyk => {
                self.current.nonstroking =
                    update_color(self.current.nonstroking, cmyk(four_numbers(operands)));
            }
            OperatorKind::SaveGraphicsState
            | OperatorKind::RestoreGraphicsState
            | OperatorKind::ConcatMatrix
            | OperatorKind::BeginText
            | OperatorKind::EndText
            | OperatorKind::BeginCompatibility
            | OperatorKind::EndCompatibility
            | OperatorKind::MarkedContentPoint
            | OperatorKind::MarkedContentPointProperties
            | OperatorKind::BeginMarkedContent
            | OperatorKind::BeginMarkedContentProperties
            | OperatorKind::EndMarkedContent => {
                return Err(vm_error(ContentVmErrorCode::InternalState, source).into());
            }
        }
        let retained = retention
            .total_with(self.retained_capacity_bytes(source)?)
            .max(retention.total_with(transient_machine_bytes));
        retention
            .limits
            .preflight(ContentVmLimitKind::RetainedBytes, 0, retained, Some(source))?;
        Ok(retained)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "rectangle construction keeps geometry, both retention budgets, and move accounting explicit"
    )]
    fn append_rectangle(
        &mut self,
        values: [ContentNumber; 4],
        limits: ContentGraphicsLimits,
        vm_limits: ContentVmLimits,
        vm_consumed_without_path: u64,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<(), GraphicsExecutionError> {
        let [x, y, width, height] = values.map(scalar);
        let x2 = x.checked_add(width)?;
        let y2 = y.checked_add(height)?;
        let start = self
            .current
            .ctm
            .checked_transform_point(ScenePoint::new(x, y))?;
        let lower_right = self
            .current
            .ctm
            .checked_transform_point(ScenePoint::new(x2, y))?;
        let upper_right = self
            .current
            .ctm
            .checked_transform_point(ScenePoint::new(x2, y2))?;
        let upper_left = self
            .current
            .ctm
            .checked_transform_point(ScenePoint::new(x, y2))?;
        self.path.reserve(
            5,
            limits,
            vm_limits,
            vm_consumed_without_path,
            source,
            accounting,
        )?;
        for segment in [
            PathSegment::MoveTo(start),
            PathSegment::LineTo(lower_right),
            PathSegment::LineTo(upper_right),
            PathSegment::LineTo(upper_left),
            PathSegment::ClosePath,
        ] {
            self.path.push(segment, source)?;
        }
        self.path.current_point = Some(start);
        self.path.subpath_start = None;
        Ok(())
    }

    fn set_dash(
        &mut self,
        pattern: DashPattern,
        source: ContentOperatorSource,
    ) -> Result<(), ContentVmError> {
        let retained_bytes = pattern
            .retained_bytes()
            .map_err(|_| vm_error(ContentVmErrorCode::InternalState, source))?;
        let released = if Arc::strong_count(&self.current.dash_ownership) == 1 {
            self.current.dash_ownership.retained_bytes
        } else {
            0
        };
        let next = self
            .dash_retained_bytes
            .checked_sub(released)
            .and_then(|value| value.checked_add(retained_bytes))
            .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        self.current.dash = pattern;
        self.current.dash_ownership = Arc::new(DashOwnership { retained_bytes });
        self.dash_retained_bytes = next;
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "path publication keeps paint semantics and both independent retention budgets explicit"
    )]
    fn paint(
        &mut self,
        operation: PaintOperation,
        close: bool,
        limits: ContentGraphicsLimits,
        retention: VmRetention,
        vm_consumed_without_path: u64,
        builder: &mut GraphicsSceneBuilder,
        source: ContentOperatorSource,
        accounting: &mut Accounting,
    ) -> Result<u64, GraphicsExecutionError> {
        if close {
            self.path.close(
                limits,
                retention.limits,
                vm_consumed_without_path,
                source,
                accounting,
            )?;
        }
        let retained_before_handoff = self.retained_capacity_bytes(source)?;
        accounting.observe_retained(retention.total_with(retained_before_handoff));
        let (path, transform, bounds, pending_clip) = self.path.take_resource();
        let paint_source = command_source(source)?;
        match operation {
            PaintOperation::None => {}
            PaintOperation::Stroke => builder.append_stroke(
                path.clone(),
                self.current.stroking,
                self.line_style()?,
                transform,
                bounds,
                paint_source,
            )?,
            PaintOperation::Fill(rule) => builder.append_fill(
                path.clone(),
                rule,
                self.current.nonstroking,
                transform,
                bounds,
                paint_source,
            )?,
            PaintOperation::FillStroke(rule) => builder.append_fill_stroke(
                path.clone(),
                rule,
                self.current.nonstroking,
                self.current.stroking,
                self.line_style()?,
                transform,
                bounds,
                paint_source,
            )?,
        }
        if let Some(pending) = pending_clip {
            builder.append_clip(
                path,
                pending.rule,
                transform,
                bounds,
                command_source(pending.source)?,
            )?;
            self.current.clip_depth = self
                .current
                .clip_depth
                .checked_add(1)
                .ok_or_else(|| vm_error(ContentVmErrorCode::InternalState, source))?;
        }
        Ok(retained_before_handoff)
    }

    fn line_style(&self) -> Result<LineStyle, SceneError> {
        LineStyle::new(
            self.current.line_width,
            self.current.line_cap,
            self.current.line_join,
            self.current.miter_limit,
            self.current.dash.clone(),
            self.current.ctm,
        )
    }
}

#[derive(Clone, Copy)]
enum PaintOperation {
    None,
    Stroke,
    Fill(FillRule),
    FillStroke(FillRule),
}

fn point(values: [ContentNumber; 2]) -> ScenePoint {
    ScenePoint::new(scalar(values[0]), scalar(values[1]))
}

fn scalar(value: ContentNumber) -> SceneScalar {
    SceneScalar::from_scaled(value.scaled())
}

fn nonnegative(
    value: ContentNumber,
    source: ContentOperatorSource,
) -> Result<SceneScalar, ContentVmError> {
    if value < ContentNumber::ZERO {
        return Err(vm_error(
            ContentVmErrorCode::InvalidGraphicsParameter,
            source,
        ));
    }
    Ok(scalar(value))
}

fn unit(value: ContentNumber) -> SceneUnit {
    let scaled = value.scaled().clamp(0, ContentNumber::ONE.scaled());
    let numerator = i128::from(scaled) * i128::from(u16::MAX) + 500_000_000;
    let quantized = numerator / 1_000_000_000;
    SceneUnit::from_u16(u16::try_from(quantized).expect("clamped unit conversion fits u16"))
}

fn update_color(paint: Paint, color: DeviceColor) -> Paint {
    Paint::new(color, paint.alpha(), paint.blend_mode())
}

fn rgb(values: [ContentNumber; 3]) -> DeviceColor {
    DeviceColor::Rgb {
        red: unit(values[0]),
        green: unit(values[1]),
        blue: unit(values[2]),
    }
}

fn cmyk(values: [ContentNumber; 4]) -> DeviceColor {
    DeviceColor::Cmyk {
        cyan: unit(values[0]),
        magenta: unit(values[1]),
        yellow: unit(values[2]),
        black: unit(values[3]),
    }
}

fn one_number(operands: &ValidatedOperands<'_>) -> ContentNumber {
    let ValidatedOperands::OneNumber(value) = operands else {
        unreachable!("operator metadata guarantees one-number operands");
    };
    *value
}

fn three_numbers(operands: &ValidatedOperands<'_>) -> [ContentNumber; 3] {
    let ValidatedOperands::ThreeNumbers(values) = operands else {
        unreachable!("operator metadata guarantees three-number operands");
    };
    *values
}

fn four_numbers(operands: &ValidatedOperands<'_>) -> [ContentNumber; 4] {
    let ValidatedOperands::FourNumbers(values) = operands else {
        unreachable!("operator metadata guarantees four-number operands");
    };
    *values
}

fn retained_bytes<T>(values: &Vec<T>) -> Option<u64> {
    u64::try_from(values.capacity())
        .ok()?
        .checked_mul(u64::try_from(size_of::<T>()).ok()?)
}

#[cfg(test)]
mod tests {
    use pdf_rs_scene::{BlendMode, DeviceColor, SceneUnit};
    use pdf_rs_syntax::ObjectRef;

    use super::*;
    use crate::DecodedSpan;

    #[test]
    fn saved_state_restores_every_registered_paint_line_dash_and_clip_field() {
        let source =
            ContentOperatorSource::new(DecodedSpan::new(ObjectRef::new(4, 0).unwrap(), 0, 0, 1), 0);
        let mut machine = GraphicsVm::new();
        machine.current.line_width = SceneScalar::from_scaled(2_000_000_000);
        machine.current.line_cap = LineCap::Round;
        machine.current.line_join = LineJoin::Bevel;
        machine.current.miter_limit = SceneScalar::from_scaled(12_000_000_000);
        machine.current.stroking = Paint::new(
            DeviceColor::Rgb {
                red: SceneUnit::ONE,
                green: SceneUnit::ZERO,
                blue: SceneUnit::ZERO,
            },
            SceneUnit::from_u16(40_000),
            BlendMode::Multiply,
        );
        machine.current.nonstroking = Paint::new(
            DeviceColor::Gray(SceneUnit::from_u16(12_345)),
            SceneUnit::from_u16(30_000),
            BlendMode::Screen,
        );
        machine.current.clip_depth = 3;
        let outer_dash =
            DashPattern::new(vec![SceneScalar::ONE], SceneScalar::ZERO).expect("outer dash");
        let outer_dash_bytes = outer_dash.retained_bytes().expect("outer dash bytes");
        machine
            .set_dash(outer_dash, source)
            .expect("set outer dash");
        machine.push_current();

        machine.current.line_width = SceneScalar::ONE;
        machine.current.line_cap = LineCap::Butt;
        machine.current.line_join = LineJoin::Miter;
        machine.current.miter_limit = SceneScalar::ONE;
        machine.current.stroking = Paint::new(
            DeviceColor::Gray(SceneUnit::ZERO),
            SceneUnit::ONE,
            BlendMode::Normal,
        );
        machine.current.nonstroking = machine.current.stroking;
        machine.current.clip_depth = 9;
        machine
            .set_dash(
                DashPattern::new(
                    vec![SceneScalar::from_scaled(2_000_000_000)],
                    SceneScalar::ONE,
                )
                .expect("inner dash"),
                source,
            )
            .expect("set inner dash");

        assert_eq!(
            machine.restore(source).expect("restore"),
            Some(Matrix::IDENTITY)
        );
        assert_eq!(
            machine.current.line_width,
            SceneScalar::from_scaled(2_000_000_000)
        );
        assert_eq!(machine.current.line_cap, LineCap::Round);
        assert_eq!(machine.current.line_join, LineJoin::Bevel);
        assert_eq!(
            machine.current.miter_limit,
            SceneScalar::from_scaled(12_000_000_000)
        );
        assert_eq!(
            machine.current.stroking.alpha(),
            SceneUnit::from_u16(40_000)
        );
        assert_eq!(machine.current.stroking.blend_mode(), BlendMode::Multiply);
        assert_eq!(
            machine.current.nonstroking.alpha(),
            SceneUnit::from_u16(30_000)
        );
        assert_eq!(machine.current.nonstroking.blend_mode(), BlendMode::Screen);
        assert_eq!(machine.current.clip_depth, 3);
        assert_eq!(machine.current.dash.array(), [SceneScalar::ONE]);
        assert_eq!(machine.dash_retained_bytes, outer_dash_bytes);
    }

    #[test]
    fn path_and_saved_state_growth_are_geometric_and_charge_live_move_work() {
        let source =
            ContentOperatorSource::new(DecodedSpan::new(ObjectRef::new(4, 0).unwrap(), 0, 0, 1), 0);
        let vm_limits = ContentVmLimits::default();

        let mut path = CurrentPath::default();
        let mut path_accounting = Accounting::default();
        path.reserve(
            1,
            ContentGraphicsLimits::default(),
            vm_limits,
            0,
            source,
            &mut path_accounting,
        )
        .expect("initial path reserve");
        let initial_path_capacity = path.segments.capacity();
        assert!(initial_path_capacity >= 4);
        assert_eq!(path_accounting.fuel, 0);
        let point = ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO);
        path.push(PathSegment::MoveTo(point), source)
            .expect("path move");
        while path.segments.len() < initial_path_capacity {
            path.push(PathSegment::LineTo(point), source)
                .expect("path line");
        }
        path.reserve(
            1,
            ContentGraphicsLimits::default(),
            vm_limits,
            0,
            source,
            &mut path_accounting,
        )
        .expect("grown path reserve");
        assert!(path.segments.capacity() >= initial_path_capacity * 2);
        assert_eq!(
            path_accounting.fuel,
            u64::try_from(initial_path_capacity).unwrap()
        );
        let grown_path_capacity = path.segments.capacity();
        path.reserve(
            1,
            ContentGraphicsLimits::default(),
            vm_limits,
            0,
            source,
            &mut path_accounting,
        )
        .expect("spare path reserve");
        assert_eq!(path.segments.capacity(), grown_path_capacity);
        assert_eq!(
            path_accounting.fuel,
            u64::try_from(initial_path_capacity).unwrap()
        );

        let mut machine = GraphicsVm::new();
        let mut saved_accounting = Accounting::default();
        let retention = VmRetention::new(0, 0, vm_limits);
        machine
            .reserve_saved_slot(retention, source, &mut saved_accounting)
            .expect("initial saved reserve");
        let initial_saved_capacity = machine.saved.capacity();
        assert!(initial_saved_capacity >= 4);
        assert_eq!(saved_accounting.fuel, 0);
        while machine.saved.len() < initial_saved_capacity {
            machine.push_current();
        }
        machine
            .reserve_saved_slot(retention, source, &mut saved_accounting)
            .expect("grown saved reserve");
        assert!(machine.saved.capacity() >= initial_saved_capacity * 2);
        assert_eq!(
            saved_accounting.fuel,
            u64::try_from(initial_saved_capacity).unwrap()
        );
    }
}
