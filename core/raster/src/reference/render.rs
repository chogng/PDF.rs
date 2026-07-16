use std::sync::Arc;

use pdf_rs_scene::{CapabilityStatus, Scene, SceneCommandKind};

use crate::reference::{
    CanonicalPixelBuffer, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderError,
    ReferenceRenderErrorCode, ReferenceRenderLimitKind, ReferenceRenderStats,
    ReferenceRenderUnsupported,
};

const RGBA_BYTES_PER_PIXEL: u64 = 4;
const OPAQUE_WHITE_RGBA8: [u8; 4] = [255, 255, 255, 255];
const CANCELLATION_WORK_INTERVAL: u64 = 256;

/// Cooperative cancellation observed by pure Reference raster work.
pub trait ReferenceRasterCancellation: Send + Sync {
    /// Returns whether the caller no longer needs the result.
    fn is_cancelled(&self) -> bool;
}

/// Observable phase of one Reference pixel-production job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderPhase {
    /// No render attempt has run.
    Pending,
    /// One complete immutable pixel buffer was published.
    Ready,
    /// One visible Scene capability was outside the current Reference profile.
    Unsupported,
    /// One terminal structured failure was published.
    Failed,
}

/// Terminal result of one Reference pixel-production poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceRenderPoll {
    /// One complete immutable canonical pixel buffer.
    Ready(Arc<CanonicalPixelBuffer>),
    /// One structured unsupported visible Scene capability.
    Unsupported(ReferenceRenderUnsupported),
    /// One structured terminal failure.
    Failed(ReferenceRenderError),
}

#[derive(Clone)]
enum RunTerminal {
    Ready(Arc<CanonicalPixelBuffer>),
    Unsupported(ReferenceRenderUnsupported),
    Failed(ReferenceRenderError),
}

/// Single-owner bounded Reference pixel-production job.
pub struct ReferenceRenderJob {
    scene: Option<Arc<Scene>>,
    config: ReferenceRenderConfig,
    limits: ReferenceRasterLimits,
    phase: ReferenceRenderPhase,
    terminal: Option<RunTerminal>,
}

impl ReferenceRenderJob {
    /// Creates one pending job without allocating pixel storage.
    pub const fn new(
        scene: Arc<Scene>,
        config: ReferenceRenderConfig,
        limits: ReferenceRasterLimits,
    ) -> Self {
        Self {
            scene: Some(scene),
            config,
            limits,
            phase: ReferenceRenderPhase::Pending,
            terminal: None,
        }
    }

    /// Returns the observable job phase.
    pub const fn phase(&self) -> ReferenceRenderPhase {
        self.phase
    }

    /// Executes the current non-painting Scene subset and publishes exactly one terminal result.
    ///
    /// A terminal result replays without consulting cancellation or performing additional work.
    pub fn poll(&mut self, cancellation: &dyn ReferenceRasterCancellation) -> ReferenceRenderPoll {
        if let Some(terminal) = &self.terminal {
            return terminal.poll();
        }
        let result = match self.scene.take() {
            Some(scene) => {
                let result = execute(&scene, self.config, self.limits, cancellation);
                drop(scene);
                result
            }
            None => Err(ReferenceRenderError::for_code(
                ReferenceRenderErrorCode::InternalState,
            )),
        };
        let terminal = match result {
            Ok(ExecuteTerminal::Ready(buffer)) => {
                self.phase = ReferenceRenderPhase::Ready;
                RunTerminal::Ready(Arc::new(buffer))
            }
            Ok(ExecuteTerminal::Unsupported(unsupported)) => {
                self.phase = ReferenceRenderPhase::Unsupported;
                RunTerminal::Unsupported(unsupported)
            }
            Err(error) => {
                self.phase = ReferenceRenderPhase::Failed;
                RunTerminal::Failed(error)
            }
        };
        let output = terminal.poll();
        self.terminal = Some(terminal);
        output
    }
}

impl RunTerminal {
    fn poll(&self) -> ReferenceRenderPoll {
        match self {
            Self::Ready(buffer) => ReferenceRenderPoll::Ready(Arc::clone(buffer)),
            Self::Unsupported(unsupported) => ReferenceRenderPoll::Unsupported(*unsupported),
            Self::Failed(error) => ReferenceRenderPoll::Failed(*error),
        }
    }
}

enum ExecuteTerminal {
    Ready(CanonicalPixelBuffer),
    Unsupported(ReferenceRenderUnsupported),
}

fn execute(
    scene: &Scene,
    config: ReferenceRenderConfig,
    limits: ReferenceRasterLimits,
    cancellation: &dyn ReferenceRasterCancellation,
) -> Result<ExecuteTerminal, ReferenceRenderError> {
    let mut cancellation_checks = 0u64;
    check_cancellation(cancellation, &mut cancellation_checks)?;

    let width = u64::from(config.size().width());
    let height = u64::from(config.size().height());
    ensure_limit(
        ReferenceRenderLimitKind::Width,
        u64::from(limits.max_width()),
        width,
    )?;
    ensure_limit(
        ReferenceRenderLimitKind::Height,
        u64::from(limits.max_height()),
        height,
    )?;
    let pixels = width.checked_mul(height).ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::Pixels,
        limits.max_pixels(),
        pixels,
    )?;
    let stride_bytes = width
        .checked_mul(RGBA_BYTES_PER_PIXEL)
        .ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::StrideBytes,
        limits.max_stride_bytes(),
        stride_bytes,
    )?;
    let output_bytes = stride_bytes
        .checked_mul(height)
        .ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::OutputBytes,
        limits.max_output_bytes(),
        output_bytes,
    )?;
    ensure_limit(
        ReferenceRenderLimitKind::RetainedBytes,
        limits.max_retained_bytes(),
        output_bytes,
    )?;

    let graphics_commands = scene
        .graphics()
        .map_or(0, |graphics| graphics.commands().len());
    let commands = scene
        .commands()
        .len()
        .checked_add(graphics_commands)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::Commands,
        limits.max_commands(),
        commands,
    )?;
    let requirements = scene
        .graphics()
        .map_or(0, |graphics| graphics.requirements().len());
    let requirements = u64::try_from(requirements).map_err(|_| numeric_overflow())?;
    ensure_limit(
        ReferenceRenderLimitKind::Requirements,
        limits.max_requirements(),
        requirements,
    )?;
    let traversal_fuel = commands
        .checked_add(requirements)
        .ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::Fuel,
        limits.max_fuel(),
        traversal_fuel,
    )?;

    let mut work_since_cancellation = 0u64;
    if let Some(graphics) = scene.graphics() {
        for (index, requirement) in graphics.requirements().iter().enumerate() {
            if requirement.status() == CapabilityStatus::Unsupported {
                return Ok(ExecuteTerminal::Unsupported(
                    ReferenceRenderUnsupported::visible_requirement(
                        u32::try_from(index).map_err(|_| numeric_overflow())?,
                    ),
                ));
            }
            charge_work(
                cancellation,
                &mut cancellation_checks,
                &mut work_since_cancellation,
            )?;
        }
        for (index, record) in graphics.commands().iter().enumerate() {
            if record.command().is_visible() {
                return Ok(ExecuteTerminal::Unsupported(
                    ReferenceRenderUnsupported::visible_command(
                        u32::try_from(index).map_err(|_| numeric_overflow())?,
                    ),
                ));
            }
            charge_work(
                cancellation,
                &mut cancellation_checks,
                &mut work_since_cancellation,
            )?;
        }
    }
    for command in scene.commands() {
        match command.kind() {
            SceneCommandKind::BeginMarkedContent | SceneCommandKind::EndMarkedContent => {}
        }
        charge_work(
            cancellation,
            &mut cancellation_checks,
            &mut work_since_cancellation,
        )?;
    }

    let fuel = traversal_fuel
        .checked_add(pixels)
        .ok_or_else(numeric_overflow)?;
    ensure_limit(ReferenceRenderLimitKind::Fuel, limits.max_fuel(), fuel)?;

    let required_capacity = usize::try_from(output_bytes).map_err(|_| numeric_overflow())?;
    check_cancellation(cancellation, &mut cancellation_checks)?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(required_capacity).map_err(|_| {
        ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_retained_bytes(),
            0,
            output_bytes,
        )
    })?;
    let retained_bytes = u64::try_from(rgba.capacity())
        .map_err(|_| ReferenceRenderError::for_code(ReferenceRenderErrorCode::InternalState))?;
    ensure_limit(
        ReferenceRenderLimitKind::RetainedBytes,
        limits.max_retained_bytes(),
        retained_bytes,
    )?;

    for _ in 0..pixels {
        rgba.extend_from_slice(&OPAQUE_WHITE_RGBA8);
        charge_work(
            cancellation,
            &mut cancellation_checks,
            &mut work_since_cancellation,
        )?;
    }
    if rgba.len() != required_capacity {
        return Err(ReferenceRenderError::for_code(
            ReferenceRenderErrorCode::InternalState,
        ));
    }
    check_cancellation(cancellation, &mut cancellation_checks)?;

    Ok(ExecuteTerminal::Ready(CanonicalPixelBuffer::new(
        scene.binding(),
        config,
        stride_bytes,
        rgba,
        ReferenceRenderStats::new(
            commands,
            requirements,
            pixels,
            fuel,
            retained_bytes,
            cancellation_checks,
        ),
    )))
}

fn ensure_limit(
    kind: ReferenceRenderLimitKind,
    limit: u64,
    attempted: u64,
) -> Result<(), ReferenceRenderError> {
    if attempted > limit {
        return Err(ReferenceRenderError::resource(kind, limit, 0, attempted));
    }
    Ok(())
}

fn charge_work(
    cancellation: &dyn ReferenceRasterCancellation,
    cancellation_checks: &mut u64,
    work_since_cancellation: &mut u64,
) -> Result<(), ReferenceRenderError> {
    *work_since_cancellation = work_since_cancellation
        .checked_add(1)
        .ok_or_else(numeric_overflow)?;
    if *work_since_cancellation >= CANCELLATION_WORK_INTERVAL {
        check_cancellation(cancellation, cancellation_checks)?;
        *work_since_cancellation = 0;
    }
    Ok(())
}

fn check_cancellation(
    cancellation: &dyn ReferenceRasterCancellation,
    checks: &mut u64,
) -> Result<(), ReferenceRenderError> {
    *checks = checks
        .checked_add(1)
        .ok_or_else(|| ReferenceRenderError::for_code(ReferenceRenderErrorCode::InternalState))?;
    if cancellation.is_cancelled() {
        return Err(ReferenceRenderError::for_code(
            ReferenceRenderErrorCode::Cancelled,
        ));
    }
    Ok(())
}

const fn numeric_overflow() -> ReferenceRenderError {
    ReferenceRenderError::for_code(ReferenceRenderErrorCode::NumericOverflow)
}
