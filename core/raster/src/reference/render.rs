use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_scene::{
    CapabilityContext, CapabilityRequirement, CapabilityStatus, GraphicsCapability,
    GraphicsCommand, GraphicsResource, GraphicsResourceEntry, GraphicsResourceId, GraphicsScene,
    Paint, PathResource, Scene, SceneCommandKind,
};

use super::coverage::{ClipStack, CoverageMask, FillEdges, SAMPLES_PER_PIXEL, rasterize_fill};
use super::geometry::{
    Affine, Fixed, GeometryCancellation, GeometryFailure, GeometryLimitKind, GeometryLimits,
    GeometryWork, PageDeviceMap, flatten_path,
};
use super::glyph::{
    GlyphCancellation, GlyphFailure, GlyphLimitKind, GlyphLimits, GlyphStats, paint_glyph_run,
};
use super::image::{
    ImageCancellation, ImageFailure, ImageLimitKind, ImageLimits, ImageStats, paint_image,
};
use super::stroke::rasterize_stroke;
use super::surface::{ReferenceSurface, SurfaceFailure};
use crate::reference::{
    CanonicalPixelBuffer, NormalizedQ16, PremultipliedRgbaQ16, ReferenceColorProfile,
    ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderError, ReferenceRenderErrorCode,
    ReferenceRenderIdentity, ReferenceRenderLimitKind, ReferenceRenderStats,
    ReferenceRenderUnsupported,
};

const RGBA_BYTES_PER_PIXEL: u64 = 4;
const FLATNESS_TOLERANCE_DENOMINATOR: i64 = 256;
const REFERENCE_CURVE_RECURSION: u8 = 16;
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
    /// One Scene requirement or command was outside the exact Reference profile.
    Unsupported,
    /// One terminal structured failure was published.
    Failed,
}

/// Terminal result of one Reference pixel-production poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceRenderPoll {
    /// One complete immutable canonical pixel buffer.
    Ready(Arc<CanonicalPixelBuffer>),
    /// One structured unsupported Scene capability.
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
    identity: ReferenceRenderIdentity,
    stats: ReferenceRenderStats,
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
            identity: ReferenceRenderIdentity::reference_v1(config.profile()),
            stats: ReferenceRenderStats::new(0, 0, 0, 0, 0, 0),
            phase: ReferenceRenderPhase::Pending,
            terminal: None,
        }
    }

    /// Returns the observable job phase.
    pub const fn phase(&self) -> ReferenceRenderPhase {
        self.phase
    }

    /// Returns the exact renderer identity used by this job.
    pub const fn identity(&self) -> ReferenceRenderIdentity {
        self.identity
    }

    /// Returns the complete validated aggregate limit profile.
    pub const fn limits(&self) -> ReferenceRasterLimits {
        self.limits
    }

    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> ReferenceRenderStats {
        self.stats
    }

    /// Executes the exact mounted Reference Scene subset and publishes one terminal result.
    ///
    /// A terminal result replays without consulting cancellation or performing additional work.
    pub fn poll(&mut self, cancellation: &dyn ReferenceRasterCancellation) -> ReferenceRenderPoll {
        if let Some(terminal) = &self.terminal {
            return terminal.poll();
        }
        let result = match self.scene.take() {
            Some(scene) => {
                let result = execute(
                    &scene,
                    self.config,
                    self.limits,
                    self.identity,
                    &mut self.stats,
                    cancellation,
                );
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

#[allow(
    clippy::large_enum_variant,
    reason = "boxing the private ready value would introduce an unbudgeted allocation before atomic publication"
)]
enum ExecuteTerminal {
    Ready(CanonicalPixelBuffer),
    Unsupported(ReferenceRenderUnsupported),
}

fn execute(
    scene: &Scene,
    config: ReferenceRenderConfig,
    limits: ReferenceRasterLimits,
    identity: ReferenceRenderIdentity,
    stats: &mut ReferenceRenderStats,
    cancellation: &dyn ReferenceRasterCancellation,
) -> Result<ExecuteTerminal, ReferenceRenderError> {
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

    let graphics_commands = scene.graphics().map_or(0, |value| value.commands().len());
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
    let resources = scene.graphics().map_or(0, |value| value.resources().len());
    let resources = u64::try_from(resources).map_err(|_| numeric_overflow())?;
    ensure_limit(
        ReferenceRenderLimitKind::Resources,
        limits.max_resources(),
        resources,
    )?;
    let requirements = scene
        .graphics()
        .map_or(0, |value| value.requirements().len());
    let requirements = u64::try_from(requirements).map_err(|_| numeric_overflow())?;
    ensure_limit(
        ReferenceRenderLimitKind::Requirements,
        limits.max_requirements(),
        requirements,
    )?;
    let dependencies = match scene.graphics() {
        None => 0,
        Some(graphics) => graphics
            .requirements()
            .iter()
            .try_fold(0_u64, |total, value| {
                total.checked_add(u64::try_from(value.dependencies().len()).ok()?)
            })
            .ok_or_else(numeric_overflow)?,
    };
    ensure_limit(
        ReferenceRenderLimitKind::Dependencies,
        limits.max_dependencies(),
        dependencies,
    )?;

    *stats = ReferenceRenderStats::new(commands, requirements, pixels, 0, 0, 0);
    stats.resources = resources;
    stats.dependencies = dependencies;
    let mut work = RenderWork::new(limits, pixels, stats, cancellation)?;

    if let Some(graphics) = scene.graphics()
        && let Some(unsupported) = preflight_graphics(graphics, &mut work)?
    {
        return Ok(ExecuteTerminal::Unsupported(unsupported));
    }
    for command in scene.commands() {
        match command.kind() {
            SceneCommandKind::BeginMarkedContent | SceneCommandKind::EndMarkedContent => {}
        }
        work.charge_raster_fuel(1)?;
    }

    let surface_semantic_bytes = pixels
        .checked_mul(
            u64::try_from(size_of::<PremultipliedRgbaQ16>()).map_err(|_| numeric_overflow())?,
        )
        .ok_or_else(numeric_overflow)?;
    ensure_limit(
        ReferenceRenderLimitKind::SurfaceBytes,
        limits.max_surface_bytes(),
        surface_semantic_bytes,
    )?;
    work.observe_working(surface_semantic_bytes, 0, 0, 0)?;
    work.check_cancellation()?;
    let mut surface = ReferenceSurface::new_white(config.size().width(), config.size().height())
        .map_err(|failure| map_surface_failure(failure, limits, work.stats))?;
    ensure_limit(
        ReferenceRenderLimitKind::SurfaceBytes,
        limits.max_surface_bytes(),
        surface.retained_bytes(),
    )?;
    work.stats.surface_bytes = surface.retained_bytes();
    work.observe_working(surface.retained_bytes(), 0, 0, 0)?;

    if let Some(graphics) = scene.graphics() {
        let mut clips = ClipStack::new(surface.width(), surface.height())
            .map_err(|failure| map_geometry_failure(failure, limits, work.stats))?;
        dispatch_graphics(scene, graphics, &mut surface, &mut clips, &mut work)?;
        work.stats.clip_depth = u64::try_from(clips.depth()).map_err(|_| numeric_overflow())?;
        work.stats.clip_bytes = clips
            .retained_bytes()
            .map_err(|failure| map_geometry_failure(failure, limits, work.stats))?;
        work.stats.peak_clip_bytes = work.stats.peak_clip_bytes.max(clips.peak_retained_bytes());
    }

    let required_capacity = usize::try_from(output_bytes).map_err(|_| numeric_overflow())?;
    let final_peak = surface
        .retained_bytes()
        .checked_add(output_bytes)
        .and_then(|value| value.checked_add(work.stats.clip_bytes))
        .ok_or_else(numeric_overflow)?;
    work.observe_working(final_peak, 0, 0, 0)?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(required_capacity).map_err(|_| {
        ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_peak_working_bytes(),
            work.stats.peak_working_bytes,
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
    let actual_peak = surface
        .retained_bytes()
        .checked_add(retained_bytes)
        .and_then(|value| value.checked_add(work.stats.clip_bytes))
        .ok_or_else(numeric_overflow)?;
    work.observe_working(actual_peak, 0, 0, 0)?;
    for pixel in surface.pixels() {
        rgba.extend_from_slice(&pixel.to_straight_rgba8());
        work.charge_final_fuel(1)?;
        work.stats.final_conversion_pixels = work
            .stats
            .final_conversion_pixels
            .checked_add(1)
            .ok_or_else(numeric_overflow)?;
    }
    if rgba.len() != required_capacity {
        return Err(ReferenceRenderError::for_code(
            ReferenceRenderErrorCode::InternalState,
        ));
    }
    work.stats.retained_bytes = retained_bytes;
    work.check_cancellation()?;

    Ok(ExecuteTerminal::Ready(CanonicalPixelBuffer::new(
        identity,
        scene.binding(),
        config,
        limits,
        stride_bytes,
        rgba,
        *work.stats,
    )))
}

fn preflight_graphics(
    graphics: &GraphicsScene,
    work: &mut RenderWork<'_>,
) -> Result<Option<ReferenceRenderUnsupported>, ReferenceRenderError> {
    for (index, requirement) in graphics.requirements().iter().enumerate() {
        let index = u32::try_from(index).map_err(|_| numeric_overflow())?;
        if requirement.id().value() != index || !valid_requirement_context(graphics, requirement) {
            return Err(invalid_scene());
        }
        let mut previous = None;
        for dependency in requirement.dependencies() {
            if dependency.value() >= requirement.id().value()
                || previous.is_some_and(|value| value >= dependency.value())
            {
                return Err(invalid_scene());
            }
            previous = Some(dependency.value());
            work.charge_raster_fuel(1)?;
        }
        if !reference_supports_requirement(requirement) {
            return Ok(Some(ReferenceRenderUnsupported::requirement(
                index,
                requirement,
            )));
        }
        work.charge_raster_fuel(1)?;
    }

    for (index, entry) in graphics.resources().iter().enumerate() {
        if entry.id().value() != u32::try_from(index).map_err(|_| numeric_overflow())? {
            return Err(invalid_scene());
        }
        work.charge_raster_fuel(1)?;
    }

    let mut saved = 0_u64;
    for (index, record) in graphics.commands().iter().enumerate() {
        let command = record.command();
        match command {
            GraphicsCommand::Save => {
                saved = saved.checked_add(1).ok_or_else(numeric_overflow)?;
            }
            GraphicsCommand::Restore => {
                saved = saved.checked_sub(1).ok_or_else(invalid_scene)?;
            }
            GraphicsCommand::Clip { path, .. }
            | GraphicsCommand::Fill { path, .. }
            | GraphicsCommand::Stroke { path, .. }
            | GraphicsCommand::FillStroke { path, .. } => {
                resolve_path(graphics, *path)?;
            }
            GraphicsCommand::DrawImage { image, .. } => {
                let image = resolve_image(graphics, *image)?;
                if image.interpolate() {
                    return Ok(Some(ReferenceRenderUnsupported::command(
                        u32::try_from(index).map_err(|_| numeric_overflow())?,
                        command,
                    )));
                }
            }
            GraphicsCommand::DrawGlyphRun(run) => {
                if run.glyphs().is_empty() {
                    return Err(invalid_scene());
                }
                for glyph in run.glyphs() {
                    resolve_glyph(graphics, glyph.outline())?;
                }
            }
            GraphicsCommand::BeginIsolatedGroup { .. } | GraphicsCommand::EndIsolatedGroup => {
                return Ok(Some(ReferenceRenderUnsupported::command(
                    u32::try_from(index).map_err(|_| numeric_overflow())?,
                    command,
                )));
            }
        }
        work.charge_raster_fuel(1)?;
    }
    if saved != 0 {
        return Err(invalid_scene());
    }
    Ok(None)
}

fn valid_requirement_context(
    graphics: &GraphicsScene,
    requirement: &CapabilityRequirement,
) -> bool {
    match requirement.context() {
        CapabilityContext::Scene => true,
        CapabilityContext::Command(index) => usize::try_from(index)
            .ok()
            .is_some_and(|value| value < graphics.commands().len()),
        CapabilityContext::Resource(id) => usize::try_from(id.value()).ok().is_some_and(|value| {
            graphics
                .resources()
                .get(value)
                .is_some_and(|entry| entry.id() == id)
        }),
    }
}

fn reference_supports_requirement(requirement: &CapabilityRequirement) -> bool {
    if requirement.status() == CapabilityStatus::Unsupported {
        return false;
    }
    let parameter = requirement.parameter();
    match requirement.capability() {
        GraphicsCapability::PathFill | GraphicsCapability::Clip => parameter <= 1,
        GraphicsCapability::PathStroke => parameter == 0,
        GraphicsCapability::DeviceColor => matches!(parameter, 1 | 3 | 4),
        GraphicsCapability::ConstantAlpha => parameter <= u64::from(u16::MAX),
        GraphicsCapability::Blend => matches!(parameter, 0..=2),
        GraphicsCapability::Image => {
            let components = parameter & 0xff;
            let bits = (parameter >> 8) & 0xff;
            let interpolate = (parameter >> 16) & 1;
            parameter >> 17 == 0 && matches!(components, 1 | 3 | 4) && bits == 8 && interpolate == 0
        }
        GraphicsCapability::Glyph => parameter != 0,
        GraphicsCapability::SoftMask | GraphicsCapability::IsolatedGroup => false,
    }
}

fn dispatch_graphics(
    scene: &Scene,
    graphics: &GraphicsScene,
    surface: &mut ReferenceSurface,
    clips: &mut ClipStack,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let page_map = PageDeviceMap::new(scene.geometry(), surface.width(), surface.height())
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    for record in graphics.commands() {
        match record.command() {
            GraphicsCommand::Save => save_clip(clips, work)?,
            GraphicsCommand::Restore => restore_clip(clips, work)?,
            GraphicsCommand::Clip {
                path,
                rule,
                transform,
            } => clip_path(
                resolve_path(graphics, *path)?,
                *rule,
                *transform,
                page_map,
                clips,
                surface,
                work,
            )?,
            GraphicsCommand::Fill {
                path,
                rule,
                paint,
                transform,
            } => fill_path(
                resolve_path(graphics, *path)?,
                *rule,
                *paint,
                *transform,
                page_map,
                clips,
                surface,
                work,
            )?,
            GraphicsCommand::Stroke {
                path,
                paint,
                style,
                transform,
            } => stroke_path(
                resolve_path(graphics, *path)?,
                *paint,
                style,
                *transform,
                page_map,
                clips,
                surface,
                work,
            )?,
            GraphicsCommand::FillStroke {
                path,
                rule,
                fill,
                stroke,
                style,
                transform,
            } => {
                let path = resolve_path(graphics, *path)?;
                fill_path(
                    path, *rule, *fill, *transform, page_map, clips, surface, work,
                )?;
                stroke_path(
                    path, *stroke, style, *transform, page_map, clips, surface, work,
                )?;
            }
            GraphicsCommand::DrawImage {
                image,
                transform,
                alpha,
                blend_mode,
            } => draw_image(
                resolve_image(graphics, *image)?,
                scene,
                *transform,
                *alpha,
                *blend_mode,
                clips,
                surface,
                work,
            )?,
            GraphicsCommand::DrawGlyphRun(run) => {
                draw_glyphs(run, graphics, scene, clips, surface, work)?;
            }
            GraphicsCommand::BeginIsolatedGroup { .. } | GraphicsCommand::EndIsolatedGroup => {
                return Err(invalid_scene());
            }
        }
    }
    Ok(())
}

fn save_clip(clips: &mut ClipStack, work: &mut RenderWork<'_>) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    clips
        .save(&mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        work.stats.surface_bytes,
        clips.operation_peak_retained_bytes(),
        0,
        child.geometry_bytes(),
    )?;
    work.commit_geometry(&child, 0)?;
    work.commit_clip(clips)?;
    Ok(())
}

fn restore_clip(
    clips: &mut ClipStack,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    clips
        .restore(&mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        work.stats.surface_bytes,
        clips.operation_peak_retained_bytes(),
        0,
        child.geometry_bytes(),
    )?;
    work.commit_geometry(&child, 0)?;
    work.commit_clip(clips)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn clip_path(
    path: &PathResource,
    rule: pdf_rs_scene::FillRule,
    transform: pdf_rs_scene::Matrix,
    page_map: PageDeviceMap,
    clips: &mut ClipStack,
    surface: &ReferenceSurface,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes_before = clip_bytes(clips, work)?;
    let working_bytes = work.remaining_working_bytes(clip_bytes_before)?;
    let had_mask = clips.has_mask();
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let path_to_device = page_map
        .combined(transform)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let flattened = flatten_reference_path(path, path_to_device, &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let edges = FillEdges::from_path(&flattened, &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let mask = rasterize_fill(&edges, rule, surface.width(), surface.height(), &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let coverage_bytes = mask
        .retained_bytes()
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        surface.retained_bytes(),
        clip_bytes_before,
        0,
        child.peak_geometry_bytes(),
    )?;
    work.observe_working(
        surface.retained_bytes(),
        clip_bytes_before,
        coverage_bytes,
        child.geometry_bytes(),
    )?;
    clips
        .intersect(mask, &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        surface.retained_bytes(),
        clips.operation_peak_retained_bytes(),
        if had_mask { coverage_bytes } else { 0 },
        child.geometry_bytes(),
    )?;
    work.commit_geometry(&child, coverage_bytes)?;
    work.commit_clip(clips)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn fill_path(
    path: &PathResource,
    rule: pdf_rs_scene::FillRule,
    paint: Paint,
    transform: pdf_rs_scene::Matrix,
    page_map: PageDeviceMap,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let path_to_device = page_map
        .combined(transform)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let flattened = flatten_reference_path(path, path_to_device, &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let edges = FillEdges::from_path(&flattened, &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let mask = rasterize_fill(&edges, rule, surface.width(), surface.height(), &mut child)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    finish_path_paint(mask, paint, clips, surface, child, work)
}

#[allow(clippy::too_many_arguments)]
fn stroke_path(
    path: &PathResource,
    paint: Paint,
    style: &pdf_rs_scene::LineStyle,
    transform: pdf_rs_scene::Matrix,
    page_map: PageDeviceMap,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let path_to_page = Affine::from_scene(transform)
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let mask = rasterize_stroke(
        path,
        path_to_page,
        page_map.affine(),
        style,
        surface.width(),
        surface.height(),
        &mut child,
    )
    .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    finish_path_paint(mask, paint, clips, surface, child, work)
}

fn finish_path_paint(
    mask: CoverageMask,
    paint: Paint,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    child: GeometryWork<'_>,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let coverage_bytes = mask
        .retained_bytes()
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    let clip_bytes = clips
        .retained_bytes()
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        surface.retained_bytes(),
        clip_bytes,
        0,
        child.peak_geometry_bytes(),
    )?;
    work.observe_working(
        surface.retained_bytes(),
        clip_bytes,
        coverage_bytes,
        child.geometry_bytes(),
    )?;
    work.commit_geometry(&child, coverage_bytes)?;
    paint_coverage(surface, &mask, clips, paint, work)?;
    work.stats.coverage_bytes = 0;
    Ok(())
}

fn flatten_reference_path(
    path: &PathResource,
    transform: Affine,
    work: &mut GeometryWork<'_>,
) -> Result<super::geometry::FlattenedPath, GeometryFailure> {
    flatten_path(
        path,
        transform,
        transform,
        Fixed::from_raw(Fixed::ONE.raw() / FLATNESS_TOLERANCE_DENOMINATOR),
        REFERENCE_CURVE_RECURSION.min(work.limits().max_curve_recursion),
        work,
    )
}

fn paint_coverage(
    surface: &mut ReferenceSurface,
    mask: &CoverageMask,
    clips: &ClipStack,
    paint: Paint,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    if mask.width() != surface.width() || mask.height() != surface.height() {
        return Err(invalid_scene());
    }
    let (source, blend) = ReferenceColorProfile::ReferenceColorV1.prepare_paint(paint);
    for y in 0..surface.height() {
        for x in 0..surface.width() {
            let index =
                pixel_index(surface.width(), surface.height(), x, y).ok_or_else(invalid_scene)?;
            let coverage = mask.sample_mask(x, y).ok_or_else(invalid_scene)?
                & clips.sample_mask(x, y).ok_or_else(invalid_scene)?;
            let covered = u64::from(coverage.count_ones());
            if covered != 0 {
                let backdrop = surface.pixels()[index];
                let painted = blend.source_over(source, backdrop);
                surface.pixels_mut()[index] = coverage_average(backdrop, painted, covered)?;
            }
            work.charge_raster_fuel(covered.checked_add(1).ok_or_else(numeric_overflow)?)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_image(
    image: &pdf_rs_scene::ImageResource,
    scene: &Scene,
    transform: pdf_rs_scene::Matrix,
    alpha: pdf_rs_scene::SceneUnit,
    blend_mode: pdf_rs_scene::BlendMode,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let limits = work.image_limits()?;
    let cancellation = KernelCancellation(work.cancellation);
    let width = surface.width();
    let height = surface.height();
    let stats = paint_image(
        image,
        scene.geometry(),
        transform,
        width,
        height,
        alpha,
        blend_mode,
        surface.pixels_mut(),
        Some(clips),
        limits,
        &cancellation,
    )
    .map_err(|failure| map_image_failure(failure, work.limits, work.stats))?;
    work.commit_image(stats)?;
    work.observe_working(
        surface.retained_bytes(),
        clips
            .retained_bytes()
            .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))?,
        0,
        0,
    )?;
    Ok(())
}

fn draw_glyphs(
    run: &pdf_rs_scene::GlyphRun,
    graphics: &GraphicsScene,
    scene: &Scene,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let available = work.remaining_working_bytes(clip_bytes)?;
    if available == 0 {
        return Err(ReferenceRenderError::resource(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            work.limits.max_peak_working_bytes(),
            work.limits.max_peak_working_bytes(),
            1,
        ));
    }
    let limits = work.glyph_limits(available)?;
    let cancellation = KernelCancellation(work.cancellation);
    let width = surface.width();
    let height = surface.height();
    let stats = paint_glyph_run(
        run,
        graphics.resources(),
        scene.geometry(),
        width,
        height,
        surface.pixels_mut(),
        Some(clips),
        limits,
        &cancellation,
    )
    .map_err(|failure| map_glyph_failure(failure, work.limits, work.stats))?;
    work.observe_working(
        surface.retained_bytes(),
        clip_bytes,
        stats.retained_bytes(),
        0,
    )?;
    work.commit_glyph(stats)?;
    Ok(())
}

fn clip_bytes(clips: &ClipStack, work: &RenderWork<'_>) -> Result<u64, ReferenceRenderError> {
    clips
        .retained_bytes()
        .map_err(|failure| map_geometry_failure(failure, work.limits, work.stats))
}

fn resolve_entry(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<&GraphicsResourceEntry, ReferenceRenderError> {
    let index = usize::try_from(id.value()).map_err(|_| numeric_overflow())?;
    graphics
        .resources()
        .get(index)
        .filter(|entry| entry.id() == id)
        .ok_or_else(invalid_scene)
}

fn resolve_path(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<&PathResource, ReferenceRenderError> {
    match resolve_entry(graphics, id)?.resource() {
        GraphicsResource::Path(path) => Ok(path),
        GraphicsResource::Image(_) | GraphicsResource::GlyphOutline(_) => Err(invalid_scene()),
    }
}

fn resolve_image(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<&pdf_rs_scene::ImageResource, ReferenceRenderError> {
    match resolve_entry(graphics, id)?.resource() {
        GraphicsResource::Image(image) => Ok(image),
        GraphicsResource::Path(_) | GraphicsResource::GlyphOutline(_) => Err(invalid_scene()),
    }
}

fn resolve_glyph(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<&pdf_rs_scene::GlyphOutline, ReferenceRenderError> {
    match resolve_entry(graphics, id)?.resource() {
        GraphicsResource::GlyphOutline(glyph) => Ok(glyph),
        GraphicsResource::Path(_) | GraphicsResource::Image(_) => Err(invalid_scene()),
    }
}

struct KernelCancellation<'a>(&'a dyn ReferenceRasterCancellation);

impl GeometryCancellation for KernelCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl ImageCancellation for KernelCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl GlyphCancellation for KernelCancellation<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct RenderWork<'a> {
    limits: ReferenceRasterLimits,
    reserved_final_fuel: u64,
    stats: &'a mut ReferenceRenderStats,
    cancellation: &'a dyn ReferenceRasterCancellation,
    fuel_since_cancellation: u64,
}

impl<'a> RenderWork<'a> {
    fn new(
        limits: ReferenceRasterLimits,
        reserved_final_fuel: u64,
        stats: &'a mut ReferenceRenderStats,
        cancellation: &'a dyn ReferenceRasterCancellation,
    ) -> Result<Self, ReferenceRenderError> {
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            limits.max_fuel(),
            0,
            reserved_final_fuel,
        )?;
        let mut work = Self {
            limits,
            reserved_final_fuel,
            stats,
            cancellation,
            fuel_since_cancellation: 0,
        };
        work.check_cancellation()?;
        Ok(work)
    }

    fn charge_raster_fuel(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        let consumed_with_reserved = self
            .stats
            .fuel
            .checked_add(self.reserved_final_fuel)
            .ok_or_else(numeric_overflow)?;
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            consumed_with_reserved,
            amount,
        )?;
        self.charge_fuel_unreserved(amount)
    }

    fn charge_final_fuel(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            self.stats.fuel,
            amount,
        )?;
        self.reserved_final_fuel =
            self.reserved_final_fuel
                .checked_sub(amount)
                .ok_or_else(|| {
                    ReferenceRenderError::for_code(ReferenceRenderErrorCode::InternalState)
                })?;
        self.charge_fuel_unreserved(amount)
    }

    fn charge_fuel_unreserved(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        self.stats.fuel = self
            .stats
            .fuel
            .checked_add(amount)
            .ok_or_else(numeric_overflow)?;
        self.fuel_since_cancellation = self
            .fuel_since_cancellation
            .checked_add(amount)
            .ok_or_else(numeric_overflow)?;
        while self.fuel_since_cancellation >= CANCELLATION_WORK_INTERVAL {
            self.check_cancellation()?;
            self.fuel_since_cancellation -= CANCELLATION_WORK_INTERVAL;
        }
        Ok(())
    }

    fn remaining_fuel(&self) -> Result<u64, ReferenceRenderError> {
        self.limits
            .max_fuel()
            .checked_sub(self.stats.fuel)
            .and_then(|value| value.checked_sub(self.reserved_final_fuel))
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                ReferenceRenderError::resource(
                    ReferenceRenderLimitKind::Fuel,
                    self.limits.max_fuel(),
                    self.stats.fuel,
                    1,
                )
            })
    }

    fn check_cancellation(&mut self) -> Result<(), ReferenceRenderError> {
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(1)
            .ok_or_else(numeric_overflow)?;
        if self.cancellation.is_cancelled() {
            return Err(ReferenceRenderError::for_code(
                ReferenceRenderErrorCode::Cancelled,
            ));
        }
        Ok(())
    }

    fn geometry_limits(
        &self,
        max_working_bytes: u64,
    ) -> Result<GeometryLimits, ReferenceRenderError> {
        Ok(GeometryLimits {
            max_curve_recursion: self
                .limits
                .max_curve_recursion()
                .min(REFERENCE_CURVE_RECURSION),
            max_segments: remaining_or_one(
                self.limits.max_geometry_segments(),
                self.stats.geometry_segments,
            )?,
            max_edges: remaining_or_one(
                self.limits.max_geometry_edges(),
                self.stats.geometry_edges,
            )?,
            max_samples: remaining_or_one(
                self.limits.max_geometry_samples(),
                self.stats.geometry_samples,
            )?,
            max_coverage_bytes: self.limits.max_coverage_bytes(),
            max_dash_chunks: remaining_or_one(
                self.limits.max_dash_chunks(),
                self.stats.dash_chunks,
            )?,
            max_stroke_runs: remaining_or_one(
                self.limits.max_stroke_runs(),
                self.stats.stroke_runs,
            )?,
            max_stroke_primitives: remaining_or_one(
                self.limits.max_stroke_primitives(),
                self.stats.stroke_primitives,
            )?,
            max_geometry_bytes: self.limits.max_geometry_bytes(),
            max_working_bytes,
            max_clip_depth: self.limits.max_clip_depth(),
            max_clip_bytes: self.limits.max_clip_bytes(),
            max_fuel: self.remaining_fuel()?,
        })
    }

    fn image_limits(&self) -> Result<ImageLimits, ReferenceRenderError> {
        Ok(ImageLimits {
            max_source_pixels: remaining_or_one(
                self.limits.max_image_source_pixels(),
                self.stats.image_source_pixels,
            )?,
            max_stride_bytes: self.limits.max_image_stride_bytes(),
            max_decoded_bytes: remaining_or_one(
                self.limits.max_image_decoded_bytes(),
                self.stats.image_decoded_bytes,
            )?,
            max_output_pixels: self.limits.max_pixels(),
            max_samples: remaining_or_one(
                self.limits.max_image_samples(),
                self.stats.image_samples,
            )?,
            max_conversions: remaining_or_one(
                self.limits.max_image_conversions(),
                self.stats.image_conversions,
            )?,
            max_retained_bytes: self.limits.max_peak_working_bytes(),
            max_fuel: self.remaining_fuel()?,
        })
    }

    fn glyph_limits(&self, max_retained_bytes: u64) -> Result<GlyphLimits, ReferenceRenderError> {
        let remaining_shared_samples = remaining_or_one(
            self.limits.max_geometry_samples(),
            self.stats.geometry_samples,
        )?;
        let remaining_glyph_samples =
            remaining_or_one(self.limits.max_glyph_samples(), self.stats.glyph_samples)?;
        Ok(GlyphLimits {
            max_glyphs: remaining_or_one(self.limits.max_glyphs(), self.stats.glyphs)?,
            max_resource_lookups: remaining_or_one(
                self.limits.max_glyph_resource_lookups(),
                self.stats.glyph_resource_lookups,
            )?,
            max_outline_segments: remaining_or_one(
                self.limits.max_glyph_outline_segments(),
                self.stats.glyph_outline_segments,
            )?,
            max_flattened_segments: remaining_or_one(
                self.limits.max_geometry_segments(),
                self.stats.geometry_segments,
            )?,
            max_edges: remaining_or_one(
                self.limits.max_geometry_edges(),
                self.stats.geometry_edges,
            )?,
            max_samples: remaining_shared_samples.min(remaining_glyph_samples),
            max_coverage_bytes: self.limits.max_coverage_bytes(),
            max_output_pixels: self.limits.max_pixels(),
            max_composites: remaining_or_one(
                self.limits.max_glyph_composites(),
                self.stats.glyph_composites,
            )?,
            max_geometry_bytes: self.limits.max_geometry_bytes(),
            max_retained_bytes,
            max_geometry_fuel: self.remaining_fuel()?,
            max_fuel: self.remaining_fuel()?,
            max_curve_recursion: self
                .limits
                .max_curve_recursion()
                .min(REFERENCE_CURVE_RECURSION),
        })
    }

    fn commit_geometry(
        &mut self,
        child: &GeometryWork<'_>,
        coverage_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        add_counter(
            &mut self.stats.geometry_segments,
            child.segments(),
            self.limits.max_geometry_segments(),
            ReferenceRenderLimitKind::GeometrySegments,
        )?;
        add_counter(
            &mut self.stats.geometry_edges,
            child.edges(),
            self.limits.max_geometry_edges(),
            ReferenceRenderLimitKind::GeometryEdges,
        )?;
        add_counter(
            &mut self.stats.geometry_samples,
            child.samples(),
            self.limits.max_geometry_samples(),
            ReferenceRenderLimitKind::GeometrySamples,
        )?;
        add_counter(
            &mut self.stats.dash_chunks,
            child.dash_chunks(),
            self.limits.max_dash_chunks(),
            ReferenceRenderLimitKind::DashChunks,
        )?;
        add_counter(
            &mut self.stats.stroke_runs,
            child.stroke_runs(),
            self.limits.max_stroke_runs(),
            ReferenceRenderLimitKind::StrokeRuns,
        )?;
        add_counter(
            &mut self.stats.stroke_primitives,
            child.stroke_primitives(),
            self.limits.max_stroke_primitives(),
            ReferenceRenderLimitKind::StrokePrimitives,
        )?;
        self.stats.coverage_bytes = coverage_bytes;
        self.stats.peak_coverage_bytes = self.stats.peak_coverage_bytes.max(coverage_bytes);
        self.stats.geometry_bytes = self.stats.geometry_bytes.max(child.geometry_bytes());
        self.stats.peak_geometry_bytes = self
            .stats
            .peak_geometry_bytes
            .max(child.peak_geometry_bytes());
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        self.charge_raster_fuel(child.fuel())
    }

    fn commit_clip(&mut self, clips: &ClipStack) -> Result<(), ReferenceRenderError> {
        self.stats.clip_depth = u64::try_from(clips.depth()).map_err(|_| numeric_overflow())?;
        self.stats.clip_bytes = clips
            .retained_bytes()
            .map_err(|failure| map_geometry_failure(failure, self.limits, self.stats))?;
        self.stats.peak_clip_bytes = self.stats.peak_clip_bytes.max(clips.peak_retained_bytes());
        Ok(())
    }

    fn remaining_working_bytes(&self, clip_bytes: u64) -> Result<u64, ReferenceRenderError> {
        let live_base = self
            .stats
            .surface_bytes
            .checked_add(clip_bytes)
            .ok_or_else(numeric_overflow)?;
        ensure_limit(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            self.limits.max_peak_working_bytes(),
            live_base,
        )?;
        self.limits
            .max_peak_working_bytes()
            .checked_sub(live_base)
            .ok_or_else(numeric_overflow)
    }

    fn commit_image(&mut self, child: ImageStats) -> Result<(), ReferenceRenderError> {
        add_counter(
            &mut self.stats.image_commands,
            1,
            self.limits.max_commands(),
            ReferenceRenderLimitKind::Commands,
        )?;
        add_counter(
            &mut self.stats.image_source_pixels,
            child.source_pixels(),
            self.limits.max_image_source_pixels(),
            ReferenceRenderLimitKind::ImageSourcePixels,
        )?;
        self.stats.image_stride_bytes = self.stats.image_stride_bytes.max(child.stride_bytes());
        ensure_limit(
            ReferenceRenderLimitKind::ImageStrideBytes,
            self.limits.max_image_stride_bytes(),
            self.stats.image_stride_bytes,
        )?;
        add_counter(
            &mut self.stats.image_decoded_bytes,
            child.decoded_bytes(),
            self.limits.max_image_decoded_bytes(),
            ReferenceRenderLimitKind::ImageDecodedBytes,
        )?;
        add_counter(
            &mut self.stats.image_samples,
            child.samples(),
            self.limits.max_image_samples(),
            ReferenceRenderLimitKind::ImageSamples,
        )?;
        add_counter(
            &mut self.stats.image_conversions,
            child.conversions(),
            self.limits.max_image_conversions(),
            ReferenceRenderLimitKind::ImageConversions,
        )?;
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        self.charge_raster_fuel(child.fuel())
    }

    fn commit_glyph(&mut self, child: GlyphStats) -> Result<(), ReferenceRenderError> {
        add_counter(
            &mut self.stats.glyph_runs,
            1,
            self.limits.max_commands(),
            ReferenceRenderLimitKind::Commands,
        )?;
        add_counter(
            &mut self.stats.glyphs,
            child.glyphs(),
            self.limits.max_glyphs(),
            ReferenceRenderLimitKind::Glyphs,
        )?;
        add_counter(
            &mut self.stats.glyph_resource_lookups,
            child.resource_lookups(),
            self.limits.max_glyph_resource_lookups(),
            ReferenceRenderLimitKind::GlyphResourceLookups,
        )?;
        add_counter(
            &mut self.stats.glyph_outline_segments,
            child.outline_segments(),
            self.limits.max_glyph_outline_segments(),
            ReferenceRenderLimitKind::GlyphOutlineSegments,
        )?;
        add_counter(
            &mut self.stats.geometry_segments,
            child.flattened_segments(),
            self.limits.max_geometry_segments(),
            ReferenceRenderLimitKind::GeometrySegments,
        )?;
        add_counter(
            &mut self.stats.geometry_edges,
            child.edges(),
            self.limits.max_geometry_edges(),
            ReferenceRenderLimitKind::GeometryEdges,
        )?;
        add_counter(
            &mut self.stats.geometry_samples,
            child.samples(),
            self.limits.max_geometry_samples(),
            ReferenceRenderLimitKind::GeometrySamples,
        )?;
        add_counter(
            &mut self.stats.glyph_samples,
            child.samples(),
            self.limits.max_glyph_samples(),
            ReferenceRenderLimitKind::GlyphSamples,
        )?;
        add_counter(
            &mut self.stats.glyph_composites,
            child.composites(),
            self.limits.max_glyph_composites(),
            ReferenceRenderLimitKind::GlyphComposites,
        )?;
        self.stats.peak_coverage_bytes = self.stats.peak_coverage_bytes.max(child.coverage_bytes());
        self.stats.geometry_bytes = self.stats.geometry_bytes.max(child.geometry_bytes());
        self.stats.peak_geometry_bytes = self
            .stats
            .peak_geometry_bytes
            .max(child.peak_geometry_bytes());
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        let fuel = child
            .fuel()
            .checked_add(child.geometry_fuel())
            .ok_or_else(numeric_overflow)?;
        self.charge_raster_fuel(fuel)
    }

    fn observe_working(
        &mut self,
        surface_or_total: u64,
        clip: u64,
        coverage: u64,
        geometry: u64,
    ) -> Result<(), ReferenceRenderError> {
        let total = surface_or_total
            .checked_add(clip)
            .and_then(|value| value.checked_add(coverage))
            .and_then(|value| value.checked_add(geometry))
            .ok_or_else(numeric_overflow)?;
        ensure_limit(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            self.limits.max_peak_working_bytes(),
            total,
        )?;
        self.stats.peak_working_bytes = self.stats.peak_working_bytes.max(total);
        Ok(())
    }
}

impl GeometryCancellation for RenderWork<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

fn coverage_average(
    backdrop: PremultipliedRgbaQ16,
    painted: PremultipliedRgbaQ16,
    covered: u64,
) -> Result<PremultipliedRgbaQ16, ReferenceRenderError> {
    if covered > u64::from(SAMPLES_PER_PIXEL) {
        return Err(invalid_scene());
    }
    let uncovered = u64::from(SAMPLES_PER_PIXEL)
        .checked_sub(covered)
        .ok_or_else(numeric_overflow)?;
    let average = |background: NormalizedQ16,
                   foreground: NormalizedQ16|
     -> Result<NormalizedQ16, ReferenceRenderError> {
        let sum = u64::from(background.bits())
            .checked_mul(uncovered)
            .and_then(|value| {
                u64::from(foreground.bits())
                    .checked_mul(covered)
                    .and_then(|foreground| value.checked_add(foreground))
            })
            .ok_or_else(numeric_overflow)?;
        let rounded = sum
            .checked_add(u64::from(SAMPLES_PER_PIXEL / 2))
            .ok_or_else(numeric_overflow)?
            / u64::from(SAMPLES_PER_PIXEL);
        let bits = u32::try_from(rounded).map_err(|_| numeric_overflow())?;
        NormalizedQ16::from_bits(bits).ok_or_else(numeric_overflow)
    };
    PremultipliedRgbaQ16::new(
        average(backdrop.red(), painted.red())?,
        average(backdrop.green(), painted.green())?,
        average(backdrop.blue(), painted.blue())?,
        average(backdrop.alpha(), painted.alpha())?,
    )
    .ok_or_else(numeric_overflow)
}

fn pixel_index(width: u32, height: u32, x: u32, y: u32) -> Option<usize> {
    if x >= width || y >= height {
        return None;
    }
    u64::from(y)
        .checked_mul(u64::from(width))?
        .checked_add(u64::from(x))?
        .try_into()
        .ok()
}

fn add_counter(
    consumed: &mut u64,
    amount: u64,
    limit: u64,
    kind: ReferenceRenderLimitKind,
) -> Result<(), ReferenceRenderError> {
    ensure_additional(kind, limit, *consumed, amount)?;
    *consumed = consumed.checked_add(amount).ok_or_else(numeric_overflow)?;
    Ok(())
}

fn remaining_or_one(limit: u64, consumed: u64) -> Result<u64, ReferenceRenderError> {
    limit
        .checked_sub(consumed)
        .map(|value| value.max(1))
        .ok_or_else(numeric_overflow)
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

fn ensure_additional(
    kind: ReferenceRenderLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
) -> Result<(), ReferenceRenderError> {
    let total = consumed
        .checked_add(attempted)
        .ok_or_else(numeric_overflow)?;
    if total > limit {
        return Err(ReferenceRenderError::resource(
            kind, limit, consumed, attempted,
        ));
    }
    Ok(())
}

fn map_surface_failure(
    failure: SurfaceFailure,
    limits: ReferenceRasterLimits,
    stats: &ReferenceRenderStats,
) -> ReferenceRenderError {
    match failure {
        SurfaceFailure::NumericOverflow => numeric_overflow(),
        SurfaceFailure::InvalidSurface => invalid_scene(),
        SurfaceFailure::Allocation { attempted_bytes } => ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_surface_bytes(),
            stats.surface_bytes,
            attempted_bytes,
        ),
    }
}

fn map_geometry_failure(
    failure: GeometryFailure,
    limits: ReferenceRasterLimits,
    stats: &ReferenceRenderStats,
) -> ReferenceRenderError {
    match failure {
        GeometryFailure::NumericOverflow => numeric_overflow(),
        GeometryFailure::InvalidGeometry => invalid_scene(),
        GeometryFailure::Cancelled => {
            ReferenceRenderError::for_code(ReferenceRenderErrorCode::Cancelled)
        }
        GeometryFailure::Allocation { attempted_bytes } => ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_peak_working_bytes(),
            stats.peak_working_bytes,
            attempted_bytes,
        ),
        GeometryFailure::Limit {
            kind,
            limit: child_limit,
            consumed,
            attempted,
        } => {
            if kind == GeometryLimitKind::WorkingBytes {
                let live_base = limits.max_peak_working_bytes().saturating_sub(child_limit);
                return ReferenceRenderError::resource(
                    ReferenceRenderLimitKind::PeakWorkingBytes,
                    limits.max_peak_working_bytes(),
                    live_base.saturating_add(consumed),
                    attempted,
                );
            }
            let (kind, limit, base) = match kind {
                GeometryLimitKind::CurveRecursion => (
                    ReferenceRenderLimitKind::CurveRecursion,
                    u64::from(limits.max_curve_recursion()),
                    0,
                ),
                GeometryLimitKind::Segments => (
                    ReferenceRenderLimitKind::GeometrySegments,
                    limits.max_geometry_segments(),
                    stats.geometry_segments,
                ),
                GeometryLimitKind::Edges => (
                    ReferenceRenderLimitKind::GeometryEdges,
                    limits.max_geometry_edges(),
                    stats.geometry_edges,
                ),
                GeometryLimitKind::Samples => (
                    ReferenceRenderLimitKind::GeometrySamples,
                    limits.max_geometry_samples(),
                    stats.geometry_samples,
                ),
                GeometryLimitKind::CoverageBytes => (
                    ReferenceRenderLimitKind::CoverageBytes,
                    limits.max_coverage_bytes(),
                    0,
                ),
                GeometryLimitKind::DashChunks => (
                    ReferenceRenderLimitKind::DashChunks,
                    limits.max_dash_chunks(),
                    stats.dash_chunks,
                ),
                GeometryLimitKind::StrokeRuns => (
                    ReferenceRenderLimitKind::StrokeRuns,
                    limits.max_stroke_runs(),
                    stats.stroke_runs,
                ),
                GeometryLimitKind::StrokePrimitives => (
                    ReferenceRenderLimitKind::StrokePrimitives,
                    limits.max_stroke_primitives(),
                    stats.stroke_primitives,
                ),
                GeometryLimitKind::GeometryBytes => (
                    ReferenceRenderLimitKind::GeometryBytes,
                    limits.max_geometry_bytes(),
                    0,
                ),
                GeometryLimitKind::WorkingBytes => unreachable!("handled above"),
                GeometryLimitKind::ClipDepth => (
                    ReferenceRenderLimitKind::ClipDepth,
                    u64::from(limits.max_clip_depth()),
                    0,
                ),
                GeometryLimitKind::ClipBytes => (
                    ReferenceRenderLimitKind::ClipBytes,
                    limits.max_clip_bytes(),
                    0,
                ),
                GeometryLimitKind::Fuel => (
                    ReferenceRenderLimitKind::Fuel,
                    limits.max_fuel(),
                    stats.fuel,
                ),
            };
            ReferenceRenderError::resource(kind, limit, base.saturating_add(consumed), attempted)
        }
    }
}

fn map_image_failure(
    failure: ImageFailure,
    limits: ReferenceRasterLimits,
    stats: &ReferenceRenderStats,
) -> ReferenceRenderError {
    match failure {
        ImageFailure::NumericOverflow => numeric_overflow(),
        ImageFailure::InvalidImage | ImageFailure::UnsupportedInterpolation => invalid_scene(),
        ImageFailure::Cancelled => {
            ReferenceRenderError::for_code(ReferenceRenderErrorCode::Cancelled)
        }
        ImageFailure::Allocation { attempted_bytes } => ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_peak_working_bytes(),
            stats.peak_working_bytes,
            attempted_bytes,
        ),
        ImageFailure::GeometryLimit {
            kind,
            limit,
            consumed,
            attempted,
        } => map_geometry_failure(
            GeometryFailure::Limit {
                kind,
                limit,
                consumed,
                attempted,
            },
            limits,
            stats,
        ),
        ImageFailure::Limit {
            kind,
            consumed,
            attempted,
            ..
        } => {
            let (kind, limit, base) = match kind {
                ImageLimitKind::SourcePixels => (
                    ReferenceRenderLimitKind::ImageSourcePixels,
                    limits.max_image_source_pixels(),
                    stats.image_source_pixels,
                ),
                ImageLimitKind::StrideBytes => (
                    ReferenceRenderLimitKind::ImageStrideBytes,
                    limits.max_image_stride_bytes(),
                    0,
                ),
                ImageLimitKind::DecodedBytes => (
                    ReferenceRenderLimitKind::ImageDecodedBytes,
                    limits.max_image_decoded_bytes(),
                    stats.image_decoded_bytes,
                ),
                ImageLimitKind::OutputPixels => {
                    (ReferenceRenderLimitKind::Pixels, limits.max_pixels(), 0)
                }
                ImageLimitKind::Samples => (
                    ReferenceRenderLimitKind::ImageSamples,
                    limits.max_image_samples(),
                    stats.image_samples,
                ),
                ImageLimitKind::Conversions => (
                    ReferenceRenderLimitKind::ImageConversions,
                    limits.max_image_conversions(),
                    stats.image_conversions,
                ),
                ImageLimitKind::RetainedBytes => (
                    ReferenceRenderLimitKind::PeakWorkingBytes,
                    limits.max_peak_working_bytes(),
                    stats.peak_working_bytes,
                ),
                ImageLimitKind::Fuel => (
                    ReferenceRenderLimitKind::Fuel,
                    limits.max_fuel(),
                    stats.fuel,
                ),
            };
            ReferenceRenderError::resource(kind, limit, base.saturating_add(consumed), attempted)
        }
    }
}

fn map_glyph_failure(
    failure: GlyphFailure,
    limits: ReferenceRasterLimits,
    stats: &ReferenceRenderStats,
) -> ReferenceRenderError {
    match failure {
        GlyphFailure::NumericOverflow => numeric_overflow(),
        GlyphFailure::InvalidGlyph | GlyphFailure::InvalidResource { .. } => invalid_scene(),
        GlyphFailure::Cancelled => {
            ReferenceRenderError::for_code(ReferenceRenderErrorCode::Cancelled)
        }
        GlyphFailure::Allocation { attempted_bytes } => ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            limits.max_peak_working_bytes(),
            stats.peak_working_bytes,
            attempted_bytes,
        ),
        GlyphFailure::Limit {
            kind,
            limit: child_limit,
            consumed,
            attempted,
        } => {
            if kind == GlyphLimitKind::Samples {
                let remaining_geometry = limits
                    .max_geometry_samples()
                    .saturating_sub(stats.geometry_samples)
                    .max(1);
                let remaining_glyph = limits
                    .max_glyph_samples()
                    .saturating_sub(stats.glyph_samples)
                    .max(1);
                let (kind, limit, base) = if remaining_geometry <= remaining_glyph {
                    (
                        ReferenceRenderLimitKind::GeometrySamples,
                        limits.max_geometry_samples(),
                        stats.geometry_samples,
                    )
                } else {
                    (
                        ReferenceRenderLimitKind::GlyphSamples,
                        limits.max_glyph_samples(),
                        stats.glyph_samples,
                    )
                };
                return ReferenceRenderError::resource(
                    kind,
                    limit,
                    base.saturating_add(consumed),
                    attempted,
                );
            }
            if kind == GlyphLimitKind::RetainedBytes {
                let live_base = limits.max_peak_working_bytes().saturating_sub(child_limit);
                return ReferenceRenderError::resource(
                    ReferenceRenderLimitKind::PeakWorkingBytes,
                    limits.max_peak_working_bytes(),
                    live_base.saturating_add(consumed),
                    attempted,
                );
            }
            let (kind, limit, base) = match kind {
                GlyphLimitKind::Glyphs => (
                    ReferenceRenderLimitKind::Glyphs,
                    limits.max_glyphs(),
                    stats.glyphs,
                ),
                GlyphLimitKind::ResourceLookups => (
                    ReferenceRenderLimitKind::GlyphResourceLookups,
                    limits.max_glyph_resource_lookups(),
                    stats.glyph_resource_lookups,
                ),
                GlyphLimitKind::OutlineSegments => (
                    ReferenceRenderLimitKind::GlyphOutlineSegments,
                    limits.max_glyph_outline_segments(),
                    stats.glyph_outline_segments,
                ),
                GlyphLimitKind::FlattenedSegments => (
                    ReferenceRenderLimitKind::GeometrySegments,
                    limits.max_geometry_segments(),
                    stats.geometry_segments,
                ),
                GlyphLimitKind::Edges => (
                    ReferenceRenderLimitKind::GeometryEdges,
                    limits.max_geometry_edges(),
                    stats.geometry_edges,
                ),
                GlyphLimitKind::Samples => unreachable!("handled above"),
                GlyphLimitKind::CoverageBytes => (
                    ReferenceRenderLimitKind::CoverageBytes,
                    limits.max_coverage_bytes(),
                    0,
                ),
                GlyphLimitKind::OutputPixels => {
                    (ReferenceRenderLimitKind::Pixels, limits.max_pixels(), 0)
                }
                GlyphLimitKind::Composites => (
                    ReferenceRenderLimitKind::GlyphComposites,
                    limits.max_glyph_composites(),
                    stats.glyph_composites,
                ),
                GlyphLimitKind::GeometryBytes => (
                    ReferenceRenderLimitKind::GeometryBytes,
                    limits.max_geometry_bytes(),
                    0,
                ),
                GlyphLimitKind::RetainedBytes => unreachable!("handled above"),
                GlyphLimitKind::GeometryFuel | GlyphLimitKind::Fuel => (
                    ReferenceRenderLimitKind::Fuel,
                    limits.max_fuel(),
                    stats.fuel,
                ),
                GlyphLimitKind::CurveRecursion => (
                    ReferenceRenderLimitKind::CurveRecursion,
                    u64::from(limits.max_curve_recursion()),
                    0,
                ),
            };
            ReferenceRenderError::resource(kind, limit, base.saturating_add(consumed), attempted)
        }
    }
}

const fn invalid_scene() -> ReferenceRenderError {
    ReferenceRenderError::for_code(ReferenceRenderErrorCode::InvalidScene)
}

const fn numeric_overflow() -> ReferenceRenderError {
    ReferenceRenderError::for_code(ReferenceRenderErrorCode::NumericOverflow)
}
