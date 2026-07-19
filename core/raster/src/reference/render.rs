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
    CanonicalPixelBuffer, NormalizedQ16, PremultipliedRgbaQ16, ReferenceBlendMode,
    ReferenceColorProfile, ReferenceRasterLimits, ReferenceRenderConfig, ReferenceRenderError,
    ReferenceRenderErrorCode, ReferenceRenderIdentity, ReferenceRenderLimitKind,
    ReferenceRenderStats, ReferenceRenderUnsupported,
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
    *stats = ReferenceRenderStats::new(commands, requirements, pixels, 0, 0, 0);
    stats.resources = resources;
    let reserved_pixel_fuel = pixels.checked_mul(2).ok_or_else(numeric_overflow)?;
    let mut work = RenderWork::new(limits, reserved_pixel_fuel, stats, cancellation)?;

    let mut graphics_preflight = GraphicsPreflight::default();
    if let Some(graphics) = scene.graphics() {
        admit_nested_cardinalities(graphics, &mut work)?;
        if let Some(unsupported) = preflight_graphics(graphics, &mut graphics_preflight, &mut work)?
        {
            return Ok(ExecuteTerminal::Unsupported(unsupported));
        }
    }
    for command in scene.commands() {
        match command.kind() {
            SceneCommandKind::BeginMarkedContent | SceneCommandKind::EndMarkedContent => {}
        }
        work.charge_raster_fuel(1)?;
    }
    let group_pixel_fuel = pixels
        .checked_mul(2)
        .and_then(|value| value.checked_mul(graphics_preflight.group_count))
        .ok_or_else(numeric_overflow)?;
    work.reserve_pixel_fuel(group_pixel_fuel)?;

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
    work.ensure_working(surface_semantic_bytes, 0, 0, 0)?;
    work.check_cancellation()?;
    let mut surface = ReferenceSurface::reserve(config.size().width(), config.size().height())
        .map_err(|failure| map_surface_failure(failure, limits, work.stats))?;
    work.postflight_surface_capacity(surface.retained_bytes())?;
    let mut remaining = usize::try_from(pixels).map_err(|_| numeric_overflow())?;
    while remaining != 0 {
        let chunk =
            remaining.min(usize::try_from(CANCELLATION_WORK_INTERVAL).unwrap_or(usize::MAX));
        work.charge_pixel_fuel(u64::try_from(chunk).map_err(|_| numeric_overflow())?)?;
        surface
            .initialize_white(chunk)
            .map_err(|failure| map_surface_failure(failure, limits, work.stats))?;
        remaining -= chunk;
    }
    if !surface.is_initialized() {
        return Err(ReferenceRenderError::for_code(
            ReferenceRenderErrorCode::InternalState,
        ));
    }

    if let Some(graphics) = scene.graphics() {
        let mut clips = ClipStack::new(surface.width(), surface.height()).map_err(|failure| {
            map_geometry_failure(failure, limits, work.stats, work.reserved_pixel_fuel)
        })?;
        dispatch_graphics(
            scene,
            graphics,
            &mut surface,
            &mut clips,
            graphics_preflight.max_group_depth,
            &mut work,
        )?;
        work.stats.clip_depth = u64::try_from(clips.depth()).map_err(|_| numeric_overflow())?;
        work.stats.clip_bytes = clips.retained_bytes().map_err(|failure| {
            map_geometry_failure(failure, limits, work.stats, work.reserved_pixel_fuel)
        })?;
        work.stats.peak_clip_bytes = work.stats.peak_clip_bytes.max(clips.peak_retained_bytes());
    }

    let required_capacity = usize::try_from(output_bytes).map_err(|_| numeric_overflow())?;
    let final_peak = surface
        .retained_bytes()
        .checked_add(output_bytes)
        .and_then(|value| value.checked_add(work.stats.clip_bytes))
        .ok_or_else(numeric_overflow)?;
    work.ensure_working(final_peak, 0, 0, 0)?;
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
    work.postflight_output_capacity(surface.retained_bytes(), retained_bytes)?;
    for pixel in surface.pixels() {
        work.charge_pixel_fuel(1)?;
        rgba.extend_from_slice(&pixel.to_straight_rgba8());
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
    work.check_cancellation()?;
    work.stats.retained_bytes = retained_bytes;

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

fn admit_nested_cardinalities(
    graphics: &GraphicsScene,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let mut dependencies = 0_u64;
    for requirement in graphics.requirements() {
        let additional =
            u64::try_from(requirement.dependencies().len()).map_err(|_| numeric_overflow())?;
        ensure_additional(
            ReferenceRenderLimitKind::Dependencies,
            work.limits.max_dependencies(),
            dependencies,
            additional,
        )?;
        dependencies = dependencies
            .checked_add(additional)
            .ok_or_else(numeric_overflow)?;
        work.charge_raster_fuel(1)?;
    }

    let mut positioned_glyphs = 0_u64;
    for record in graphics.commands() {
        if let GraphicsCommand::DrawGlyphRun(run) = record.command() {
            let additional = u64::try_from(run.glyphs().len()).map_err(|_| numeric_overflow())?;
            ensure_additional(
                ReferenceRenderLimitKind::Glyphs,
                work.limits.max_glyphs(),
                positioned_glyphs,
                additional,
            )?;
            ensure_additional(
                ReferenceRenderLimitKind::GlyphResourceLookups,
                work.limits.max_glyph_resource_lookups(),
                positioned_glyphs,
                additional,
            )?;
            positioned_glyphs = positioned_glyphs
                .checked_add(additional)
                .ok_or_else(numeric_overflow)?;
        }
        work.charge_raster_fuel(1)?;
    }
    work.stats.dependencies = dependencies;
    Ok(())
}

#[derive(Default)]
struct GraphicsPreflight {
    group_count: u64,
    max_group_depth: usize,
}

fn preflight_graphics(
    graphics: &GraphicsScene,
    preflight: &mut GraphicsPreflight,
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
    let mut group_depth = 0_usize;
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
                    work.charge_raster_fuel(1)?;
                }
            }
            GraphicsCommand::BeginIsolatedGroup { .. } => {
                group_depth = group_depth.checked_add(1).ok_or_else(numeric_overflow)?;
                let (passthrough, inspected) = inspect_isolated_group(graphics, index)?;
                work.charge_raster_fuel(inspected)?;
                if !passthrough {
                    preflight.group_count = preflight
                        .group_count
                        .checked_add(1)
                        .ok_or_else(numeric_overflow)?;
                }
                preflight.max_group_depth = preflight.max_group_depth.max(group_depth);
            }
            GraphicsCommand::EndIsolatedGroup => {
                group_depth = group_depth.checked_sub(1).ok_or_else(invalid_scene)?;
            }
        }
        work.charge_raster_fuel(1)?;
    }
    if saved != 0 || group_depth != 0 {
        return Err(invalid_scene());
    }
    Ok(None)
}

fn inspect_isolated_group(
    graphics: &GraphicsScene,
    begin: usize,
) -> Result<(bool, u64), ReferenceRenderError> {
    let mut inspected = 1_u64;
    let Some(GraphicsCommand::BeginIsolatedGroup { alpha, blend_mode }) = graphics
        .commands()
        .get(begin)
        .map(|record| record.command())
    else {
        return Ok((false, inspected));
    };
    if *alpha != pdf_rs_scene::SceneUnit::ONE || *blend_mode != pdf_rs_scene::BlendMode::Normal {
        return Ok((false, inspected));
    }
    for record in &graphics.commands()[begin.saturating_add(1)..] {
        inspected = inspected.checked_add(1).ok_or_else(numeric_overflow)?;
        match record.command() {
            GraphicsCommand::BeginIsolatedGroup { .. } => return Ok((false, inspected)),
            GraphicsCommand::EndIsolatedGroup => return Ok((true, inspected)),
            GraphicsCommand::Fill { paint, .. } | GraphicsCommand::Stroke { paint, .. } => {
                if paint.blend_mode() != pdf_rs_scene::BlendMode::Normal {
                    return Ok((false, inspected));
                }
            }
            GraphicsCommand::FillStroke { fill, stroke, .. } => {
                if fill.blend_mode() != pdf_rs_scene::BlendMode::Normal
                    || stroke.blend_mode() != pdf_rs_scene::BlendMode::Normal
                {
                    return Ok((false, inspected));
                }
            }
            GraphicsCommand::DrawImage { blend_mode, .. } => {
                if *blend_mode != pdf_rs_scene::BlendMode::Normal {
                    return Ok((false, inspected));
                }
            }
            GraphicsCommand::DrawGlyphRun(run) => {
                if run.paint().blend_mode() != pdf_rs_scene::BlendMode::Normal {
                    return Ok((false, inspected));
                }
            }
            GraphicsCommand::Save | GraphicsCommand::Restore | GraphicsCommand::Clip { .. } => {}
        }
    }
    Ok((false, inspected))
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
        GraphicsCapability::IsolatedGroup => parameter == 0,
        GraphicsCapability::SoftMask => parameter == 0,
    }
}

fn dispatch_graphics(
    scene: &Scene,
    graphics: &GraphicsScene,
    surface: &mut ReferenceSurface,
    clips: &mut ClipStack,
    max_group_depth: usize,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let page_map = PageDeviceMap::new(scene.geometry(), surface.width(), surface.height())
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let nominal_group_stack_bytes = capacity_bytes::<GroupFrame>(max_group_depth)?;
    let candidate_working = work
        .working_surface_bytes()?
        .checked_add(nominal_group_stack_bytes)
        .ok_or_else(numeric_overflow)?;
    work.ensure_working(candidate_working, clip_bytes(clips, work)?, 0, 0)?;
    let mut groups = Vec::new();
    groups.try_reserve_exact(max_group_depth).map_err(|_| {
        ReferenceRenderError::resource(
            ReferenceRenderLimitKind::Allocation,
            work.limits.max_peak_working_bytes(),
            work.working_surface_bytes().unwrap_or(u64::MAX),
            nominal_group_stack_bytes,
        )
    })?;
    work.set_group_stack_bytes(capacity_bytes::<GroupFrame>(groups.capacity())?, clips)?;
    for (index, record) in graphics.commands().iter().enumerate() {
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
            GraphicsCommand::BeginIsolatedGroup { alpha, blend_mode } => {
                let (passthrough, inspected) = inspect_isolated_group(graphics, index)?;
                work.charge_raster_fuel(inspected)?;
                begin_isolated_group(
                    surface,
                    &mut groups,
                    passthrough,
                    (*alpha).into(),
                    (*blend_mode).into(),
                    clips,
                    work,
                )?;
            }
            GraphicsCommand::EndIsolatedGroup => {
                end_isolated_group(surface, &mut groups, clips, work)?;
            }
        }
    }
    if !groups.is_empty() {
        return Err(invalid_scene());
    }
    drop(groups);
    work.set_group_stack_bytes(0, clips)?;
    Ok(())
}

enum GroupFrame {
    Passthrough,
    Offscreen {
        parent: ReferenceSurface,
        alpha: NormalizedQ16,
        blend_mode: ReferenceBlendMode,
    },
}

fn begin_isolated_group(
    surface: &mut ReferenceSurface,
    groups: &mut Vec<GroupFrame>,
    passthrough: bool,
    alpha: NormalizedQ16,
    blend_mode: ReferenceBlendMode,
    clips: &ClipStack,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    if groups.len() == groups.capacity() {
        return Err(invalid_scene());
    }
    if passthrough {
        groups.push(GroupFrame::Passthrough);
        return Ok(());
    }
    let mut group = ReferenceSurface::reserve(surface.width(), surface.height())
        .map_err(|failure| map_surface_failure(failure, work.limits, work.stats))?;
    work.admit_group_surface(group.retained_bytes(), clips)?;
    let pixels = u64::from(group.width())
        .checked_mul(u64::from(group.height()))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(numeric_overflow)?;
    let mut remaining = pixels;
    while remaining != 0 {
        let chunk =
            remaining.min(usize::try_from(CANCELLATION_WORK_INTERVAL).unwrap_or(usize::MAX));
        work.charge_pixel_fuel(u64::try_from(chunk).map_err(|_| numeric_overflow())?)?;
        group
            .initialize_transparent(chunk)
            .map_err(|failure| map_surface_failure(failure, work.limits, work.stats))?;
        remaining -= chunk;
    }
    if !group.is_initialized() {
        return Err(ReferenceRenderError::for_code(
            ReferenceRenderErrorCode::InternalState,
        ));
    }
    let parent = std::mem::replace(surface, group);
    groups.push(GroupFrame::Offscreen {
        parent,
        alpha,
        blend_mode,
    });
    Ok(())
}

fn end_isolated_group(
    surface: &mut ReferenceSurface,
    groups: &mut Vec<GroupFrame>,
    clips: &ClipStack,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let frame = groups.pop().ok_or_else(invalid_scene)?;
    let GroupFrame::Offscreen {
        parent,
        alpha,
        blend_mode,
    } = frame
    else {
        return Ok(());
    };
    let group = std::mem::replace(surface, parent);
    if group.width() != surface.width()
        || group.height() != surface.height()
        || group.pixels().len() != surface.pixels().len()
    {
        return Err(invalid_scene());
    }
    for index in 0..group.pixels().len() {
        work.charge_pixel_fuel(1)?;
        let source = group.pixels()[index].apply_constant_alpha(alpha);
        let backdrop = surface.pixels()[index];
        surface.pixels_mut()[index] = blend_mode.source_over(source, backdrop);
    }
    work.release_group_surface(group.retained_bytes(), clips)?;
    Ok(())
}

fn save_clip(clips: &mut ClipStack, work: &mut RenderWork<'_>) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let surface_bytes = work.working_surface_bytes()?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new_deferred(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let initial = child.check_cancellation();
    work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, initial)?;
    let result = clips.save(&mut child);
    if let Err(error) = work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, result) {
        work.commit_clip(clips)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        surface_bytes,
        clips.operation_peak_retained_bytes(),
        0,
        child.geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes, 0)?;
        work.commit_clip(clips)?;
        return Err(error);
    }
    work.absorb_geometry(&child, surface_bytes, clip_bytes, 0)?;
    work.commit_clip(clips)?;
    Ok(())
}

fn restore_clip(
    clips: &mut ClipStack,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let clip_bytes = clip_bytes(clips, work)?;
    let surface_bytes = work.working_surface_bytes()?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new_deferred(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let initial = child.check_cancellation();
    work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, initial)?;
    let result = clips.restore(&mut child);
    if let Err(error) = work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, result) {
        work.commit_clip(clips)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        surface_bytes,
        clips.operation_peak_retained_bytes(),
        0,
        child.geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes, 0)?;
        work.commit_clip(clips)?;
        return Err(error);
    }
    work.absorb_geometry(&child, surface_bytes, clip_bytes, 0)?;
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
    let surface_bytes = work.working_surface_bytes()?;
    let working_bytes = work.remaining_working_bytes(clip_bytes_before)?;
    let had_mask = clips.has_mask();
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new_deferred(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let initial = child.check_cancellation();
    work.finish_geometry_step(&child, surface_bytes, clip_bytes_before, 0, initial)?;
    let combined = page_map.combined(transform);
    let path_to_device =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes_before, 0, combined)?;
    let flattened_result = flatten_reference_path(path, path_to_device, &mut child);
    let flattened = work.finish_geometry_step(
        &child,
        surface_bytes,
        clip_bytes_before,
        0,
        flattened_result,
    )?;
    let edges_result = FillEdges::from_path(&flattened, &mut child);
    let edges =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes_before, 0, edges_result)?;
    let mask_result = rasterize_fill(&edges, rule, surface.width(), surface.height(), &mut child);
    let mask =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes_before, 0, mask_result)?;
    let retained_result = mask.retained_bytes();
    let coverage_bytes =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes_before, 0, retained_result)?;
    if let Err(error) = work.observe_working(
        surface_bytes,
        clip_bytes_before,
        0,
        child.peak_geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes_before, coverage_bytes)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        surface_bytes,
        clip_bytes_before,
        coverage_bytes,
        child.geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes_before, coverage_bytes)?;
        return Err(error);
    }
    let intersect_result = clips.intersect(mask, &mut child);
    if let Err(error) = work.finish_geometry_step(
        &child,
        surface_bytes,
        clip_bytes_before,
        coverage_bytes,
        intersect_result,
    ) {
        work.commit_clip(clips)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        surface_bytes,
        clips.operation_peak_retained_bytes(),
        if had_mask { coverage_bytes } else { 0 },
        child.geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes_before, coverage_bytes)?;
        work.commit_clip(clips)?;
        return Err(error);
    }
    work.absorb_geometry(&child, surface_bytes, clip_bytes_before, coverage_bytes)?;
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
    let surface_bytes = work.working_surface_bytes()?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new_deferred(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let initial = child.check_cancellation();
    work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, initial)?;
    let combined = page_map.combined(transform);
    let path_to_device =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, combined)?;
    let flattened_result = flatten_reference_path(path, path_to_device, &mut child);
    let flattened =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, flattened_result)?;
    let edges_result = FillEdges::from_path(&flattened, &mut child);
    let edges = work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, edges_result)?;
    let mask_result = rasterize_fill(&edges, rule, surface.width(), surface.height(), &mut child);
    let mask = work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, mask_result)?;
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
    let surface_bytes = work.working_surface_bytes()?;
    let working_bytes = work.remaining_working_bytes(clip_bytes)?;
    let cancellation = KernelCancellation(work.cancellation);
    let mut child = GeometryWork::new_deferred(work.geometry_limits(working_bytes)?, &cancellation)
        .map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?;
    let initial = child.check_cancellation();
    work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, initial)?;
    let path_result = Affine::from_scene(transform);
    let path_to_page =
        work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, path_result)?;
    let mask_result = rasterize_stroke(
        path,
        path_to_page,
        page_map.affine(),
        style,
        surface.width(),
        surface.height(),
        &mut child,
    );
    let mask = work.finish_geometry_step(&child, surface_bytes, clip_bytes, 0, mask_result)?;
    finish_path_paint(mask, paint, clips, surface, child, work)
}

fn finish_path_paint(
    mask: CoverageMask,
    paint: Paint,
    clips: &ClipStack,
    surface: &mut ReferenceSurface,
    mut child: GeometryWork<'_>,
    work: &mut RenderWork<'_>,
) -> Result<(), ReferenceRenderError> {
    let surface_bytes = work.working_surface_bytes()?;
    let retained_result = mask.retained_bytes();
    let coverage_bytes = work.finish_geometry_step(
        &child,
        surface_bytes,
        work.stats.clip_bytes,
        0,
        retained_result,
    )?;
    let clip_result = clips.retained_bytes();
    let clip_bytes = work.finish_geometry_step(
        &child,
        surface_bytes,
        work.stats.clip_bytes,
        coverage_bytes,
        clip_result,
    )?;
    let base_result = child.set_working_base_bytes(coverage_bytes);
    work.finish_geometry_step(
        &child,
        surface_bytes,
        clip_bytes,
        coverage_bytes,
        base_result,
    )?;
    if let Err(error) =
        work.observe_working(surface_bytes, clip_bytes, 0, child.peak_geometry_bytes())
    {
        work.absorb_geometry(&child, surface_bytes, clip_bytes, coverage_bytes)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        surface_bytes,
        clip_bytes,
        coverage_bytes,
        child.geometry_bytes(),
    ) {
        work.absorb_geometry(&child, surface_bytes, clip_bytes, coverage_bytes)?;
        return Err(error);
    }
    work.absorb_geometry(&child, surface_bytes, clip_bytes, coverage_bytes)?;
    paint_coverage(surface, &mask, clips, paint, work)?;
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
            work.charge_raster_fuel(covered.checked_add(1).ok_or_else(numeric_overflow)?)?;
            if covered != 0 {
                let backdrop = surface.pixels()[index];
                let painted = blend.source_over(source, backdrop);
                surface.pixels_mut()[index] = coverage_average(backdrop, painted, covered)?;
            }
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
    let mut stats = ImageStats::default();
    let result = paint_image(
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
        &mut stats,
    );
    if let Err(failure) = result {
        let error = map_image_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel);
        work.absorb_image(stats)?;
        return Err(error);
    }
    work.absorb_image(stats)?;
    work.observe_working(
        work.working_surface_bytes()?,
        clips.retained_bytes().map_err(|failure| {
            map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
        })?,
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
    let mut stats = GlyphStats::default();
    let result = paint_glyph_run(
        run,
        graphics.resources(),
        scene.geometry(),
        width,
        height,
        surface.pixels_mut(),
        Some(clips),
        limits,
        &cancellation,
        &mut stats,
    );
    if let Err(failure) = result {
        work.note_working(
            work.working_surface_bytes()?,
            clip_bytes,
            stats.peak_working_bytes(),
            0,
        )?;
        let error = map_glyph_failure(
            failure,
            work.limits,
            work.stats,
            stats,
            work.reserved_pixel_fuel,
        );
        work.absorb_glyph(stats)?;
        return Err(error);
    }
    if let Err(error) = work.observe_working(
        work.working_surface_bytes()?,
        clip_bytes,
        stats.peak_working_bytes(),
        0,
    ) {
        work.absorb_glyph(stats)?;
        return Err(error);
    }
    work.absorb_glyph(stats)?;
    Ok(())
}

fn clip_bytes(clips: &ClipStack, work: &RenderWork<'_>) -> Result<u64, ReferenceRenderError> {
    clips.retained_bytes().map_err(|failure| {
        map_geometry_failure(failure, work.limits, work.stats, work.reserved_pixel_fuel)
    })
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
    reserved_pixel_fuel: u64,
    live_group_surface_bytes: u64,
    group_stack_bytes: u64,
    stats: &'a mut ReferenceRenderStats,
    cancellation: &'a dyn ReferenceRasterCancellation,
    fuel_since_cancellation: u64,
}

impl<'a> RenderWork<'a> {
    fn new(
        limits: ReferenceRasterLimits,
        reserved_pixel_fuel: u64,
        stats: &'a mut ReferenceRenderStats,
        cancellation: &'a dyn ReferenceRasterCancellation,
    ) -> Result<Self, ReferenceRenderError> {
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            limits.max_fuel(),
            0,
            reserved_pixel_fuel,
        )?;
        let mut work = Self {
            limits,
            reserved_pixel_fuel,
            live_group_surface_bytes: 0,
            group_stack_bytes: 0,
            stats,
            cancellation,
            fuel_since_cancellation: 0,
        };
        work.check_cancellation()?;
        Ok(work)
    }

    fn reserve_pixel_fuel(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        let consumed = self
            .stats
            .fuel
            .checked_add(self.reserved_pixel_fuel)
            .ok_or_else(numeric_overflow)?;
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            consumed,
            amount,
        )?;
        self.reserved_pixel_fuel = self
            .reserved_pixel_fuel
            .checked_add(amount)
            .ok_or_else(numeric_overflow)?;
        Ok(())
    }

    fn charge_raster_fuel(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        let consumed_with_reserved = self
            .stats
            .fuel
            .checked_add(self.reserved_pixel_fuel)
            .ok_or_else(numeric_overflow)?;
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            consumed_with_reserved,
            amount,
        )?;
        self.charge_fuel_unreserved(amount)
    }

    fn charge_pixel_fuel(&mut self, amount: u64) -> Result<(), ReferenceRenderError> {
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            self.stats.fuel,
            amount,
        )?;
        self.reserved_pixel_fuel =
            self.reserved_pixel_fuel
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
        let consumed = self
            .stats
            .fuel
            .checked_add(self.reserved_pixel_fuel)
            .ok_or_else(numeric_overflow)?;
        self.limits
            .max_fuel()
            .checked_sub(consumed)
            .ok_or_else(numeric_overflow)
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
            max_segments: remaining(
                self.limits.max_geometry_segments(),
                self.stats.geometry_segments,
            )?,
            max_edges: remaining(self.limits.max_geometry_edges(), self.stats.geometry_edges)?,
            max_samples: remaining(
                self.limits.max_geometry_samples(),
                self.stats.geometry_samples,
            )?,
            max_coverage_bytes: self.limits.max_coverage_bytes(),
            max_dash_chunks: remaining(self.limits.max_dash_chunks(), self.stats.dash_chunks)?,
            max_stroke_runs: remaining(self.limits.max_stroke_runs(), self.stats.stroke_runs)?,
            max_stroke_primitives: remaining(
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
            max_source_pixels: remaining(
                self.limits.max_image_source_pixels(),
                self.stats.image_source_pixels,
            )?,
            max_stride_bytes: self.limits.max_image_stride_bytes(),
            max_decoded_bytes: remaining(
                self.limits.max_image_decoded_bytes(),
                self.stats.image_decoded_bytes,
            )?,
            max_output_pixels: self.limits.max_pixels(),
            max_samples: remaining(self.limits.max_image_samples(), self.stats.image_samples)?,
            max_conversions: remaining(
                self.limits.max_image_conversions(),
                self.stats.image_conversions,
            )?,
            max_retained_bytes: self.limits.max_peak_working_bytes(),
            max_fuel: self.remaining_fuel()?,
        })
    }

    fn glyph_limits(&self, max_retained_bytes: u64) -> Result<GlyphLimits, ReferenceRenderError> {
        let remaining_shared_samples = remaining(
            self.limits.max_geometry_samples(),
            self.stats.geometry_samples,
        )?;
        let remaining_glyph_samples =
            remaining(self.limits.max_glyph_samples(), self.stats.glyph_samples)?;
        Ok(GlyphLimits {
            max_glyphs: remaining(self.limits.max_glyphs(), self.stats.glyphs)?,
            max_resource_lookups: remaining(
                self.limits.max_glyph_resource_lookups(),
                self.stats.glyph_resource_lookups,
            )?,
            max_outline_segments: remaining(
                self.limits.max_glyph_outline_segments(),
                self.stats.glyph_outline_segments,
            )?,
            max_flattened_segments: remaining(
                self.limits.max_geometry_segments(),
                self.stats.geometry_segments,
            )?,
            max_edges: remaining(self.limits.max_geometry_edges(), self.stats.geometry_edges)?,
            max_samples: remaining_shared_samples.min(remaining_glyph_samples),
            max_coverage_bytes: self.limits.max_coverage_bytes(),
            max_output_pixels: self.limits.max_pixels(),
            max_composites: remaining(
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

    fn absorb_geometry(
        &mut self,
        child: &GeometryWork<'_>,
        surface_bytes: u64,
        clip_bytes: u64,
        peak_coverage_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        self.record_geometry_working(child, surface_bytes, clip_bytes)?;
        self.merge_geometry_stats(child, peak_coverage_bytes)
    }

    fn record_geometry_working(
        &mut self,
        child: &GeometryWork<'_>,
        surface_bytes: u64,
        clip_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        self.note_working(surface_bytes, clip_bytes, child.peak_working_bytes(), 0)?;
        Ok(())
    }

    fn merge_geometry_stats(
        &mut self,
        child: &GeometryWork<'_>,
        peak_coverage_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        let geometry_segments = checked_counter_total(
            self.stats.geometry_segments,
            child.segments(),
            self.limits.max_geometry_segments(),
            ReferenceRenderLimitKind::GeometrySegments,
        )?;
        let geometry_edges = checked_counter_total(
            self.stats.geometry_edges,
            child.edges(),
            self.limits.max_geometry_edges(),
            ReferenceRenderLimitKind::GeometryEdges,
        )?;
        let geometry_samples = checked_counter_total(
            self.stats.geometry_samples,
            child.samples(),
            self.limits.max_geometry_samples(),
            ReferenceRenderLimitKind::GeometrySamples,
        )?;
        let dash_chunks = checked_counter_total(
            self.stats.dash_chunks,
            child.dash_chunks(),
            self.limits.max_dash_chunks(),
            ReferenceRenderLimitKind::DashChunks,
        )?;
        let stroke_runs = checked_counter_total(
            self.stats.stroke_runs,
            child.stroke_runs(),
            self.limits.max_stroke_runs(),
            ReferenceRenderLimitKind::StrokeRuns,
        )?;
        let stroke_primitives = checked_counter_total(
            self.stats.stroke_primitives,
            child.stroke_primitives(),
            self.limits.max_stroke_primitives(),
            ReferenceRenderLimitKind::StrokePrimitives,
        )?;
        let cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        let fuel = self.child_fuel_total(child.fuel())?;

        self.stats.geometry_segments = geometry_segments;
        self.stats.geometry_edges = geometry_edges;
        self.stats.geometry_samples = geometry_samples;
        self.stats.dash_chunks = dash_chunks;
        self.stats.stroke_runs = stroke_runs;
        self.stats.stroke_primitives = stroke_primitives;
        self.stats.coverage_bytes = 0;
        self.stats.peak_coverage_bytes = self
            .stats
            .peak_coverage_bytes
            .max(peak_coverage_bytes)
            .max(child.peak_coverage_bytes());
        self.stats.geometry_bytes = self.stats.geometry_bytes.max(child.geometry_bytes());
        self.stats.peak_geometry_bytes = self
            .stats
            .peak_geometry_bytes
            .max(child.peak_geometry_bytes());
        self.stats.cancellation_checks = cancellation_checks;
        self.stats.fuel = fuel;
        Ok(())
    }

    fn finish_geometry_step<T>(
        &mut self,
        child: &GeometryWork<'_>,
        surface_bytes: u64,
        clip_bytes: u64,
        peak_coverage_bytes: u64,
        result: Result<T, GeometryFailure>,
    ) -> Result<T, ReferenceRenderError> {
        match result {
            Ok(value) => Ok(value),
            Err(failure) => {
                self.record_geometry_working(child, surface_bytes, clip_bytes)?;
                let error = map_geometry_failure(
                    failure,
                    self.limits,
                    self.stats,
                    self.reserved_pixel_fuel,
                );
                self.merge_geometry_stats(child, peak_coverage_bytes)?;
                Err(error)
            }
        }
    }

    fn commit_clip(&mut self, clips: &ClipStack) -> Result<(), ReferenceRenderError> {
        self.stats.clip_depth = u64::try_from(clips.depth()).map_err(|_| numeric_overflow())?;
        self.stats.clip_bytes = clips.retained_bytes().map_err(|failure| {
            map_geometry_failure(failure, self.limits, self.stats, self.reserved_pixel_fuel)
        })?;
        self.stats.peak_clip_bytes = self.stats.peak_clip_bytes.max(clips.peak_retained_bytes());
        Ok(())
    }

    fn working_surface_bytes(&self) -> Result<u64, ReferenceRenderError> {
        self.stats
            .surface_bytes
            .checked_add(self.live_group_surface_bytes)
            .and_then(|value| value.checked_add(self.group_stack_bytes))
            .ok_or_else(numeric_overflow)
    }

    fn set_group_stack_bytes(
        &mut self,
        bytes: u64,
        clips: &ClipStack,
    ) -> Result<(), ReferenceRenderError> {
        self.group_stack_bytes = bytes;
        self.observe_working(
            self.working_surface_bytes()?,
            clips.retained_bytes().map_err(|failure| {
                map_geometry_failure(failure, self.limits, self.stats, self.reserved_pixel_fuel)
            })?,
            0,
            0,
        )
    }

    fn admit_group_surface(
        &mut self,
        retained_bytes: u64,
        clips: &ClipStack,
    ) -> Result<(), ReferenceRenderError> {
        ensure_limit(
            ReferenceRenderLimitKind::SurfaceBytes,
            self.limits.max_surface_bytes(),
            retained_bytes,
        )?;
        self.live_group_surface_bytes = self
            .live_group_surface_bytes
            .checked_add(retained_bytes)
            .ok_or_else(numeric_overflow)?;
        self.observe_working(
            self.working_surface_bytes()?,
            clips.retained_bytes().map_err(|failure| {
                map_geometry_failure(failure, self.limits, self.stats, self.reserved_pixel_fuel)
            })?,
            0,
            0,
        )
    }

    fn release_group_surface(
        &mut self,
        retained_bytes: u64,
        clips: &ClipStack,
    ) -> Result<(), ReferenceRenderError> {
        self.live_group_surface_bytes = self
            .live_group_surface_bytes
            .checked_sub(retained_bytes)
            .ok_or_else(numeric_overflow)?;
        self.observe_working(
            self.working_surface_bytes()?,
            clips.retained_bytes().map_err(|failure| {
                map_geometry_failure(failure, self.limits, self.stats, self.reserved_pixel_fuel)
            })?,
            0,
            0,
        )
    }

    fn remaining_working_bytes(&self, clip_bytes: u64) -> Result<u64, ReferenceRenderError> {
        let live_base = self
            .working_surface_bytes()?
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

    fn absorb_image(&mut self, child: ImageStats) -> Result<(), ReferenceRenderError> {
        let image_commands = checked_counter_total(
            self.stats.image_commands,
            1,
            self.limits.max_commands(),
            ReferenceRenderLimitKind::Commands,
        )?;
        let image_source_pixels = checked_counter_total(
            self.stats.image_source_pixels,
            child.source_pixels(),
            self.limits.max_image_source_pixels(),
            ReferenceRenderLimitKind::ImageSourcePixels,
        )?;
        let image_stride_bytes = self.stats.image_stride_bytes.max(child.stride_bytes());
        ensure_limit(
            ReferenceRenderLimitKind::ImageStrideBytes,
            self.limits.max_image_stride_bytes(),
            image_stride_bytes,
        )?;
        let image_decoded_bytes = checked_counter_total(
            self.stats.image_decoded_bytes,
            child.decoded_bytes(),
            self.limits.max_image_decoded_bytes(),
            ReferenceRenderLimitKind::ImageDecodedBytes,
        )?;
        let image_samples = checked_counter_total(
            self.stats.image_samples,
            child.samples(),
            self.limits.max_image_samples(),
            ReferenceRenderLimitKind::ImageSamples,
        )?;
        let image_conversions = checked_counter_total(
            self.stats.image_conversions,
            child.conversions(),
            self.limits.max_image_conversions(),
            ReferenceRenderLimitKind::ImageConversions,
        )?;
        let cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        let fuel = self.child_fuel_total(child.fuel())?;

        self.stats.image_commands = image_commands;
        self.stats.image_source_pixels = image_source_pixels;
        self.stats.image_stride_bytes = image_stride_bytes;
        self.stats.image_decoded_bytes = image_decoded_bytes;
        self.stats.image_samples = image_samples;
        self.stats.image_conversions = image_conversions;
        self.stats.cancellation_checks = cancellation_checks;
        self.stats.coverage_bytes = 0;
        self.stats.fuel = fuel;
        Ok(())
    }

    fn absorb_glyph(&mut self, child: GlyphStats) -> Result<(), ReferenceRenderError> {
        let glyph_runs = checked_counter_total(
            self.stats.glyph_runs,
            1,
            self.limits.max_commands(),
            ReferenceRenderLimitKind::Commands,
        )?;
        let glyphs = checked_counter_total(
            self.stats.glyphs,
            child.glyphs(),
            self.limits.max_glyphs(),
            ReferenceRenderLimitKind::Glyphs,
        )?;
        let glyph_resource_lookups = checked_counter_total(
            self.stats.glyph_resource_lookups,
            child.resource_lookups(),
            self.limits.max_glyph_resource_lookups(),
            ReferenceRenderLimitKind::GlyphResourceLookups,
        )?;
        let glyph_outline_segments = checked_counter_total(
            self.stats.glyph_outline_segments,
            child.outline_segments(),
            self.limits.max_glyph_outline_segments(),
            ReferenceRenderLimitKind::GlyphOutlineSegments,
        )?;
        let geometry_segments = checked_counter_total(
            self.stats.geometry_segments,
            child.flattened_segments(),
            self.limits.max_geometry_segments(),
            ReferenceRenderLimitKind::GeometrySegments,
        )?;
        let geometry_edges = checked_counter_total(
            self.stats.geometry_edges,
            child.edges(),
            self.limits.max_geometry_edges(),
            ReferenceRenderLimitKind::GeometryEdges,
        )?;
        let geometry_samples = checked_counter_total(
            self.stats.geometry_samples,
            child.samples(),
            self.limits.max_geometry_samples(),
            ReferenceRenderLimitKind::GeometrySamples,
        )?;
        let glyph_samples = checked_counter_total(
            self.stats.glyph_samples,
            child.samples(),
            self.limits.max_glyph_samples(),
            ReferenceRenderLimitKind::GlyphSamples,
        )?;
        let glyph_composites = checked_counter_total(
            self.stats.glyph_composites,
            child.composites(),
            self.limits.max_glyph_composites(),
            ReferenceRenderLimitKind::GlyphComposites,
        )?;
        let cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(child.cancellation_checks())
            .ok_or_else(numeric_overflow)?;
        let child_fuel = child
            .fuel()
            .checked_add(child.geometry_fuel())
            .ok_or_else(numeric_overflow)?;
        let fuel = self.child_fuel_total(child_fuel)?;

        self.stats.glyph_runs = glyph_runs;
        self.stats.glyphs = glyphs;
        self.stats.glyph_resource_lookups = glyph_resource_lookups;
        self.stats.glyph_outline_segments = glyph_outline_segments;
        self.stats.geometry_segments = geometry_segments;
        self.stats.geometry_edges = geometry_edges;
        self.stats.geometry_samples = geometry_samples;
        self.stats.glyph_samples = glyph_samples;
        self.stats.glyph_composites = glyph_composites;
        self.stats.peak_coverage_bytes = self.stats.peak_coverage_bytes.max(child.coverage_bytes());
        self.stats.coverage_bytes = 0;
        self.stats.geometry_bytes = self.stats.geometry_bytes.max(child.geometry_bytes());
        self.stats.peak_geometry_bytes = self
            .stats
            .peak_geometry_bytes
            .max(child.peak_geometry_bytes());
        self.stats.cancellation_checks = cancellation_checks;
        self.stats.fuel = fuel;
        Ok(())
    }

    fn child_fuel_total(&self, amount: u64) -> Result<u64, ReferenceRenderError> {
        let consumed_with_reserved = self
            .stats
            .fuel
            .checked_add(self.reserved_pixel_fuel)
            .ok_or_else(numeric_overflow)?;
        ensure_additional(
            ReferenceRenderLimitKind::Fuel,
            self.limits.max_fuel(),
            consumed_with_reserved,
            amount,
        )?;
        self.stats
            .fuel
            .checked_add(amount)
            .ok_or_else(numeric_overflow)
    }

    fn postflight_surface_capacity(
        &mut self,
        retained_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        self.stats.surface_bytes = retained_bytes;
        let actual_peak = self.note_working(retained_bytes, 0, 0, 0)?;
        ensure_limit(
            ReferenceRenderLimitKind::SurfaceBytes,
            self.limits.max_surface_bytes(),
            retained_bytes,
        )?;
        ensure_limit(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            self.limits.max_peak_working_bytes(),
            actual_peak,
        )
    }

    fn postflight_output_capacity(
        &mut self,
        surface_bytes: u64,
        retained_bytes: u64,
    ) -> Result<(), ReferenceRenderError> {
        let actual_peak = surface_bytes
            .checked_add(retained_bytes)
            .and_then(|value| value.checked_add(self.stats.clip_bytes))
            .ok_or_else(numeric_overflow)?;
        self.note_working(actual_peak, 0, 0, 0)?;
        ensure_limit(
            ReferenceRenderLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            retained_bytes,
        )?;
        ensure_limit(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            self.limits.max_peak_working_bytes(),
            actual_peak,
        )
    }

    fn observe_working(
        &mut self,
        surface_or_total: u64,
        clip: u64,
        coverage: u64,
        geometry: u64,
    ) -> Result<(), ReferenceRenderError> {
        let total = self.note_working(surface_or_total, clip, coverage, geometry)?;
        ensure_limit(
            ReferenceRenderLimitKind::PeakWorkingBytes,
            self.limits.max_peak_working_bytes(),
            total,
        )
    }

    fn ensure_working(
        &self,
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
        )
    }

    fn note_working(
        &mut self,
        surface_or_total: u64,
        clip: u64,
        coverage: u64,
        geometry: u64,
    ) -> Result<u64, ReferenceRenderError> {
        let total = surface_or_total
            .checked_add(clip)
            .and_then(|value| value.checked_add(coverage))
            .and_then(|value| value.checked_add(geometry))
            .ok_or_else(numeric_overflow)?;
        self.stats.peak_working_bytes = self.stats.peak_working_bytes.max(total);
        Ok(total)
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

fn capacity_bytes<T>(capacity: usize) -> Result<u64, ReferenceRenderError> {
    capacity
        .checked_mul(size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(numeric_overflow)
}

fn checked_counter_total(
    consumed: u64,
    amount: u64,
    limit: u64,
    kind: ReferenceRenderLimitKind,
) -> Result<u64, ReferenceRenderError> {
    ensure_additional(kind, limit, consumed, amount)?;
    consumed.checked_add(amount).ok_or_else(numeric_overflow)
}

fn remaining(limit: u64, consumed: u64) -> Result<u64, ReferenceRenderError> {
    limit.checked_sub(consumed).ok_or_else(numeric_overflow)
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
    reserved_pixel_fuel: u64,
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
                    match stats.fuel.checked_add(reserved_pixel_fuel) {
                        Some(value) => value,
                        None => return numeric_overflow(),
                    },
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
    reserved_pixel_fuel: u64,
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
            reserved_pixel_fuel,
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
                    match stats.fuel.checked_add(reserved_pixel_fuel) {
                        Some(value) => value,
                        None => return numeric_overflow(),
                    },
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
    child: GlyphStats,
    reserved_pixel_fuel: u64,
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
            if matches!(kind, GlyphLimitKind::GeometryFuel | GlyphLimitKind::Fuel) {
                let other_fuel = if kind == GlyphLimitKind::GeometryFuel {
                    child.fuel()
                } else {
                    child.geometry_fuel()
                };
                let Some(consumed) = stats
                    .fuel
                    .checked_add(reserved_pixel_fuel)
                    .and_then(|value| value.checked_add(other_fuel))
                    .and_then(|value| value.checked_add(consumed))
                else {
                    return numeric_overflow();
                };
                return ReferenceRenderError::resource(
                    ReferenceRenderLimitKind::Fuel,
                    limits.max_fuel(),
                    consumed,
                    attempted,
                );
            }
            if kind == GlyphLimitKind::Samples {
                let remaining_geometry = limits
                    .max_geometry_samples()
                    .saturating_sub(stats.geometry_samples);
                let remaining_glyph = limits
                    .max_glyph_samples()
                    .saturating_sub(stats.glyph_samples);
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
                GlyphLimitKind::GeometryFuel | GlyphLimitKind::Fuel => {
                    unreachable!("handled above")
                }
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

#[cfg(test)]
mod tests {
    use super::{ReferenceRasterCancellation, ReferenceRenderStats, RenderWork};
    use crate::reference::{
        ReferenceRasterLimitConfig, ReferenceRasterLimits, ReferenceRenderLimitKind,
    };

    struct NeverCancelled;

    impl ReferenceRasterCancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn actual_surface_overcapacity_is_recorded_before_postflight_rejection() {
        let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_surface_bytes: 16,
            max_peak_working_bytes: 100,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();
        let mut stats = ReferenceRenderStats::default();
        let mut work = RenderWork::new(limits, 1, &mut stats, &NeverCancelled).unwrap();
        let error = work.postflight_surface_capacity(17).unwrap_err();

        assert_eq!(
            error.limit().unwrap().kind(),
            ReferenceRenderLimitKind::SurfaceBytes
        );
        assert_eq!(work.stats.surface_bytes, 17);
        assert_eq!(work.stats.peak_working_bytes, 17);
    }

    #[test]
    fn actual_output_overcapacity_records_peak_but_not_published_retention() {
        let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_retained_bytes: 4,
            max_peak_working_bytes: 100,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();
        let mut stats = ReferenceRenderStats::default();
        let mut work = RenderWork::new(limits, 1, &mut stats, &NeverCancelled).unwrap();
        let error = work.postflight_output_capacity(16, 5).unwrap_err();

        assert_eq!(
            error.limit().unwrap().kind(),
            ReferenceRenderLimitKind::RetainedBytes
        );
        assert_eq!(work.stats.peak_working_bytes, 21);
        assert_eq!(work.stats.retained_bytes, 0);
    }

    #[test]
    fn every_child_entry_receives_exact_zero_after_mandatory_pixel_fuel_is_reserved() {
        enum ChildEntry {
            Geometry,
            Image,
            Glyph,
        }

        let limits = ReferenceRasterLimits::validate(ReferenceRasterLimitConfig {
            max_fuel: 2,
            ..ReferenceRasterLimitConfig::default()
        })
        .unwrap();

        for child in [ChildEntry::Geometry, ChildEntry::Image, ChildEntry::Glyph] {
            let mut stats = ReferenceRenderStats::default();
            let work = RenderWork::new(limits, 2, &mut stats, &NeverCancelled).unwrap();
            let (label, child_fuel) = match child {
                ChildEntry::Geometry => ("geometry", work.geometry_limits(1).unwrap().max_fuel),
                ChildEntry::Image => ("image", work.image_limits().unwrap().max_fuel),
                ChildEntry::Glyph => ("glyph", work.glyph_limits(1).unwrap().max_fuel),
            };
            assert_eq!(child_fuel, 0, "{label}");
            assert_eq!(work.stats.fuel, 0, "{label}");
        }
    }
}
