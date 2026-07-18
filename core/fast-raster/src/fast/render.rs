//! Scene validation, command binning, and atomic tile publication.

use core::mem::size_of;

use pdf_rs_policy::{
    AlphaMode, AntialiasMode, CapabilityEvaluator, CompositingMode, GlyphSampling, ImageSampling,
    NativeBackend, OutputProfile, PixelFormat, PolicyCancellation, PolicyErrorCode,
    PolicyLimitConfig, PolicyLimits, RenderPlan,
};
use pdf_rs_scene::{
    BlendMode, FillRule, GraphicsCommand, GraphicsScene, Matrix, Paint, Scene, SceneUnit,
};

use crate::fast::kernels::{
    Coverage, FlatPath, KernelWork, PageMap, Pixel, WorkRect, composite_coverage, draw_image,
    fill_coverage, fill_coverage_bounded, flatten_path, lookup_glyph, lookup_image, lookup_path,
    vector_bytes,
};
use crate::fast::limits::checked_total;
use crate::fast::stroke::stroke_coverage;
use crate::fast::{
    FastRasterCancellation, FastRasterError, FastRasterErrorCode, FastRasterIdentity,
    FastRasterLimitKind, FastRasterLimits, FastRasterStats, FastTile, FastTileBins, FastTileSet,
};

/// Immutable preflighted and deterministically binned Fast CPU render job.
///
/// Construction consumes no pixel storage. [`Self::render_all`] renders tiles into private
/// buffers in any caller-selected permutation and publishes the complete set only if every tile
/// and the final cancellation probe succeed.
pub struct FastRasterJob<'a> {
    scene: &'a Scene,
    plan: &'a RenderPlan,
    graphics: &'a GraphicsScene,
    limits: FastRasterLimits,
    identity: FastRasterIdentity,
    bins: FastTileBins,
    product_pixels: u64,
    bin_fuel: u64,
    bin_cancellation_checks: u64,
    bin_peak_intermediate: u64,
}

enum GroupFrame {
    Offscreen {
        parent: Vec<Pixel>,
        alpha: SceneUnit,
        blend_mode: BlendMode,
    },
}

impl<'a> FastRasterJob<'a> {
    /// Validates the exact Fast configuration and Scene/plan identities, then builds bounded bins.
    pub fn new(
        scene: &'a Scene,
        plan: &'a RenderPlan,
        limits: FastRasterLimits,
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<Self, FastRasterError> {
        validate_config(plan, limits)?;
        let graphics = validate_subject(scene, plan, cancellation)?;
        let product_pixels = preflight_pixels(plan, limits)?;
        let interval = u64::from(plan.config().input().cancellation_interval);
        let mut work = Work::new(limits, interval, cancellation, 0, 0)?;
        let map = PageMap::new(scene, plan)?;
        let bins = build_bins(graphics, plan, map, limits, &mut work)?;
        work.check()?;
        Ok(Self {
            scene,
            plan,
            graphics,
            limits,
            identity: FastRasterIdentity::scalar_v1(plan.config().hash()),
            bins,
            product_pixels,
            bin_fuel: work.fuel,
            bin_cancellation_checks: work.cancellation_checks,
            bin_peak_intermediate: work.peak_intermediate,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the owned job transfers every validated scene, plan, bin, and accounting component without cloning them"
    )]
    pub(super) fn from_prepared(
        scene: &'a Scene,
        plan: &'a RenderPlan,
        limits: FastRasterLimits,
        bins: FastTileBins,
        product_pixels: u64,
        bin_fuel: u64,
        bin_cancellation_checks: u64,
        bin_peak_intermediate: u64,
    ) -> Result<Self, FastRasterError> {
        let graphics = scene.graphics().ok_or_else(identity)?;
        Ok(Self {
            scene,
            plan,
            graphics,
            limits,
            identity: FastRasterIdentity::scalar_v1(plan.config().hash()),
            bins,
            product_pixels,
            bin_fuel,
            bin_cancellation_checks,
            bin_peak_intermediate,
        })
    }

    pub(super) fn into_bins(self) -> Vec<Vec<u32>> {
        self.bins.into_inner_bins()
    }

    /// Returns the complete implementation and RenderConfig identity.
    pub const fn identity(&self) -> FastRasterIdentity {
        self.identity
    }

    /// Borrows deterministic source-ordered tile bins.
    pub const fn bins(&self) -> &FastTileBins {
        &self.bins
    }

    /// Renders a complete permutation of tile ordinals and atomically publishes the resulting set.
    ///
    /// `tile_order` must contain every ordinal exactly once. Returned tile order follows this
    /// permutation, while command execution inside each tile always follows Scene source order.
    pub fn render_all(
        &self,
        tile_order: &[u32],
        cancellation: &dyn FastRasterCancellation,
    ) -> Result<FastTileSet, FastRasterError> {
        let interval = u64::from(self.plan.config().input().cancellation_interval);
        let mut work = Work::new(
            self.limits,
            interval,
            cancellation,
            self.bin_fuel,
            self.bin_cancellation_checks,
        )?;
        work.peak_intermediate = self.bin_peak_intermediate;
        validate_permutation(tile_order, self.plan.tiles().len(), &mut work)?;

        let pixel_bytes = self.product_pixels.checked_mul(4).ok_or_else(numeric)?;
        let expected_metadata = u64::try_from(self.plan.tiles().len())
            .ok()
            .and_then(|count| {
                u64::try_from(size_of::<FastTile>())
                    .ok()
                    .and_then(|width| count.checked_mul(width))
            })
            .ok_or_else(numeric)?;
        let durable = checked_total(
            FastRasterLimitKind::RetainedBytes,
            self.bins.retained_bytes(),
            pixel_bytes,
            self.limits.max_retained_bytes(),
        )?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            durable,
            expected_metadata,
            self.limits.max_retained_bytes(),
        )?;

        let mut tiles = Vec::new();
        reserve(&mut tiles, tile_order.len())?;
        let metadata_bytes = vector_bytes(&tiles)?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            durable,
            metadata_bytes,
            self.limits.max_retained_bytes(),
        )?;

        let mut published_pixel_capacity = 0_u64;
        for &ordinal in tile_order {
            let index = usize::try_from(ordinal).map_err(|_| numeric())?;
            let tile = self.render_one(index, &mut work)?;
            published_pixel_capacity = published_pixel_capacity
                .checked_add(tile.retained_bytes()?)
                .ok_or_else(numeric)?;
            let actual_durable = self
                .bins
                .retained_bytes()
                .checked_add(metadata_bytes)
                .and_then(|value| value.checked_add(published_pixel_capacity))
                .ok_or_else(numeric)?;
            if actual_durable > self.limits.max_retained_bytes() {
                return Err(FastRasterError::resource(
                    FastRasterLimitKind::RetainedBytes,
                    self.limits.max_retained_bytes(),
                    actual_durable,
                ));
            }
            tiles.push(tile);
        }
        work.check()?;

        let retained_bytes = self
            .bins
            .retained_bytes()
            .checked_add(published_pixel_capacity)
            .and_then(|value| value.checked_add(metadata_bytes))
            .ok_or_else(numeric)?;
        let stats = FastRasterStats::new(
            u64::try_from(self.graphics.commands().len()).map_err(|_| numeric())?,
            self.bins.entries(),
            u64::try_from(tiles.len()).map_err(|_| numeric())?,
            self.product_pixels,
            retained_bytes,
            work.peak_intermediate,
            work.fuel,
            work.cancellation_checks,
        );
        Ok(FastTileSet::new(self.plan.hash(), tiles, stats))
    }

    pub(super) fn render_one(
        &self,
        tile_index: usize,
        work: &mut Work<'_>,
    ) -> Result<FastTile, FastRasterError> {
        let planned = self.plan.tiles().get(tile_index).ok_or_else(identity)?;
        let command_bin = self.bins.bins().get(tile_index).ok_or_else(identity)?;
        let config = self.plan.config().input();
        let tile = planned.content_key().tile();
        let rect = WorkRect::expanded(tile, config.tile_halo)?;
        let surface_len = usize::try_from(rect.pixels()?).map_err(|_| numeric())?;
        let logical_surface_bytes = logical_vector_bytes::<Pixel>(surface_len)?;
        work.admit_intermediate(logical_surface_bytes)?;
        let mut surface = Vec::new();
        reserve(&mut surface, surface_len)?;
        let mut surface_bytes = vector_bytes(&surface)?;
        work.admit_intermediate(surface_bytes)?;
        for _ in 0..surface_len {
            work.step()?;
            surface.push(Pixel::WHITE);
        }

        let logical_clip_bytes = logical_vector_bytes::<u16>(surface_len)?;
        work.admit_intermediate(add(surface_bytes, logical_clip_bytes)?)?;
        let mut clip = Coverage::full(rect, surface_bytes, work)?;
        let mut stack = Vec::<Coverage>::new();
        let mut stack_payload_bytes = 0_u64;
        let mut groups = Vec::<GroupFrame>::new();
        let mut group_stack_bytes = 0_u64;
        let map = PageMap::new(self.scene, self.plan)?;

        for &command_index in command_bin {
            let command = self
                .graphics
                .commands()
                .get(usize::try_from(command_index).map_err(|_| numeric())?)
                .ok_or_else(identity)?
                .command();
            match command {
                GraphicsCommand::Save => {
                    if stack.len() == stack.capacity() {
                        let next_len = stack.len().checked_add(1).ok_or_else(numeric)?;
                        let logical_stack_bytes = logical_vector_bytes::<Coverage>(next_len)?;
                        let state_payload = state_payload_bytes(
                            surface_bytes,
                            &clip,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?;
                        let old_stack_bytes = vector_bytes(&stack)?;
                        let allocation_peak = state_payload
                            .checked_add(old_stack_bytes)
                            .and_then(|value| value.checked_add(logical_stack_bytes))
                            .ok_or_else(numeric)?;
                        work.admit_intermediate(allocation_peak)?;
                        reserve(&mut stack, 1)?;
                        let new_stack_bytes = vector_bytes(&stack)?;
                        work.admit_intermediate(
                            state_payload
                                .checked_add(old_stack_bytes)
                                .and_then(|value| value.checked_add(new_stack_bytes))
                                .ok_or_else(numeric)?,
                        )?;
                        work.admit_intermediate(state_bytes(
                            surface_bytes,
                            &clip,
                            &stack,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?)?;
                    }
                    let base = state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?;
                    work.admit_intermediate(base)?;
                    work.admit_intermediate(add(base, logical_clip_bytes)?)?;
                    let saved = Coverage::copy_from(&clip, base, work)?;
                    stack_payload_bytes = stack_payload_bytes
                        .checked_add(saved.retained_bytes())
                        .ok_or_else(numeric)?;
                    work.step()?;
                    stack.push(saved);
                }
                GraphicsCommand::Restore => {
                    let restored = stack.pop().ok_or_else(command_sequence)?;
                    stack_payload_bytes = stack_payload_bytes
                        .checked_sub(restored.retained_bytes())
                        .ok_or_else(numeric)?;
                    clip = restored;
                }
                GraphicsCommand::Clip {
                    path,
                    rule,
                    transform,
                } => {
                    let base = state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?;
                    let flat = self.flatten(*path, *transform, 1, map, base, work)?;
                    let operation =
                        fill_coverage(&flat, rect, *rule, add(base, flat.retained_bytes())?, work)?;
                    clip.intersect(&operation, work)?;
                }
                GraphicsCommand::Fill {
                    path,
                    rule,
                    paint,
                    transform,
                } => self.paint_fill(
                    &mut surface,
                    &clip,
                    state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?,
                    rect,
                    *path,
                    *transform,
                    1,
                    *rule,
                    *paint,
                    map,
                    work,
                )?,
                GraphicsCommand::Stroke {
                    path,
                    paint,
                    style,
                    transform,
                } => self.paint_stroke(
                    &mut surface,
                    &clip,
                    state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?,
                    rect,
                    *path,
                    *transform,
                    *paint,
                    style,
                    map,
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
                    self.paint_fill(
                        &mut surface,
                        &clip,
                        state_bytes(
                            surface_bytes,
                            &clip,
                            &stack,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?,
                        rect,
                        *path,
                        *transform,
                        1,
                        *rule,
                        *fill,
                        map,
                        work,
                    )?;
                    self.paint_stroke(
                        &mut surface,
                        &clip,
                        state_bytes(
                            surface_bytes,
                            &clip,
                            &stack,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?,
                        rect,
                        *path,
                        *transform,
                        *stroke,
                        style,
                        map,
                        work,
                    )?;
                }
                GraphicsCommand::DrawImage {
                    image,
                    transform,
                    alpha,
                    blend_mode,
                } => {
                    let image = lookup_image(self.graphics, *image)?;
                    draw_image(
                        &mut surface,
                        &clip,
                        rect,
                        image,
                        *transform,
                        *alpha,
                        *blend_mode,
                        map,
                        work,
                    )?;
                }
                GraphicsCommand::DrawGlyphRun(run) => {
                    let base = state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?;
                    if let Some(fill) = run.painting().fill() {
                        self.paint_glyph_fill(
                            &mut surface,
                            &clip,
                            base,
                            rect,
                            run,
                            fill,
                            map,
                            work,
                        )?;
                    }
                    if let Some((stroke, style)) = run.painting().stroke() {
                        self.paint_glyph_stroke(
                            &mut surface,
                            &clip,
                            base,
                            rect,
                            run,
                            stroke,
                            style,
                            map,
                            work,
                        )?;
                    }
                }
                GraphicsCommand::BeginIsolatedGroup { alpha, blend_mode } => {
                    if groups.len() == groups.capacity() {
                        let next_len = groups.len().checked_add(1).ok_or_else(numeric)?;
                        let logical_group_stack = logical_vector_bytes::<GroupFrame>(next_len)?;
                        let base = state_bytes(
                            surface_bytes,
                            &clip,
                            &stack,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?;
                        work.admit_intermediate(add(base, logical_group_stack)?)?;
                        reserve(&mut groups, 1)?;
                        group_stack_bytes = vector_bytes(&groups)?;
                        work.admit_intermediate(state_bytes(
                            surface_bytes,
                            &clip,
                            &stack,
                            stack_payload_bytes,
                            group_stack_bytes,
                        )?)?;
                    }

                    let base = state_bytes(
                        surface_bytes,
                        &clip,
                        &stack,
                        stack_payload_bytes,
                        group_stack_bytes,
                    )?;
                    work.admit_intermediate(add(base, logical_surface_bytes)?)?;
                    let mut group = Vec::new();
                    reserve(&mut group, surface_len)?;
                    let group_bytes = vector_bytes(&group)?;
                    work.admit_intermediate(add(base, group_bytes)?)?;
                    for _ in 0..surface_len {
                        work.step()?;
                        group.push(Pixel::TRANSPARENT);
                    }
                    surface_bytes = surface_bytes.checked_add(group_bytes).ok_or_else(numeric)?;
                    let parent = std::mem::replace(&mut surface, group);
                    groups.push(GroupFrame::Offscreen {
                        parent,
                        alpha: *alpha,
                        blend_mode: *blend_mode,
                    });
                }
                GraphicsCommand::EndIsolatedGroup => {
                    let frame = groups.pop().ok_or_else(command_sequence)?;
                    let GroupFrame::Offscreen {
                        parent,
                        alpha,
                        blend_mode,
                    } = frame;
                    let group = std::mem::replace(&mut surface, parent);
                    if group.len() != surface.len() {
                        return Err(command_sequence());
                    }
                    for (backdrop, source) in surface.iter_mut().zip(&group) {
                        work.step()?;
                        *backdrop = source
                            .apply_constant_alpha(alpha)
                            .source_over(*backdrop, blend_mode);
                    }
                    surface_bytes = surface_bytes
                        .checked_sub(vector_bytes(&group)?)
                        .ok_or_else(numeric)?;
                }
            }
        }
        if !stack.is_empty() || !groups.is_empty() {
            return Err(command_sequence());
        }

        let stride = tile.width().checked_mul(4).ok_or_else(numeric)?;
        let output_len = u64::from(stride)
            .checked_mul(u64::from(tile.height()))
            .ok_or_else(numeric)?;
        let output_len = usize::try_from(output_len).map_err(|_| numeric())?;
        let logical_output_bytes = logical_vector_bytes::<u8>(output_len)?;
        let output_base = state_bytes(
            surface_bytes,
            &clip,
            &stack,
            stack_payload_bytes,
            group_stack_bytes,
        )?;
        work.admit_intermediate(add(output_base, logical_output_bytes)?)?;
        let mut output = Vec::new();
        reserve(&mut output, output_len)?;
        let output_capacity = vector_bytes(&output)?;
        work.admit_intermediate(add(output_base, output_capacity)?)?;
        let halo = u32::from(config.tile_halo);
        for row in 0..tile.height() {
            let source_row = row.checked_add(halo).ok_or_else(numeric)?;
            for column in 0..tile.width() {
                let source_column = column.checked_add(halo).ok_or_else(numeric)?;
                let source_index = u64::from(source_row)
                    .checked_mul(u64::from(rect.width))
                    .and_then(|value| value.checked_add(u64::from(source_column)))
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(numeric)?;
                for channel in surface[source_index].to_rgba8() {
                    work.step()?;
                    output.push(channel);
                }
            }
        }
        if output.len() != output_len {
            return Err(identity());
        }
        work.check()?;
        Ok(FastTile::new(
            planned.clone(),
            self.identity,
            stride,
            output,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_fill(
        &self,
        surface: &mut [Pixel],
        clip: &Coverage,
        base_intermediate: u64,
        rect: WorkRect,
        path: pdf_rs_scene::GraphicsResourceId,
        transform: Matrix,
        divisor: u16,
        rule: FillRule,
        paint: Paint,
        map: PageMap,
        work: &mut Work<'_>,
    ) -> Result<(), FastRasterError> {
        let flat = self.flatten(path, transform, divisor, map, base_intermediate, work)?;
        let operation = fill_coverage(
            &flat,
            rect,
            rule,
            add(base_intermediate, flat.retained_bytes())?,
            work,
        )?;
        composite_coverage(surface, &operation, clip, paint, work)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_stroke(
        &self,
        surface: &mut [Pixel],
        clip: &Coverage,
        base_intermediate: u64,
        rect: WorkRect,
        path: pdf_rs_scene::GraphicsResourceId,
        transform: Matrix,
        paint: Paint,
        style: &pdf_rs_scene::LineStyle,
        map: PageMap,
        work: &mut Work<'_>,
    ) -> Result<(), FastRasterError> {
        let config = self.plan.config().input();
        let operation = stroke_coverage(
            lookup_path(self.graphics, path)?,
            transform,
            1,
            style,
            map,
            rect,
            config.curve_flatness_denominator,
            config.curve_recursion,
            base_intermediate,
            work,
        )?;
        composite_coverage(surface, &operation, clip, paint, work)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_glyph_fill(
        &self,
        surface: &mut [Pixel],
        clip: &Coverage,
        base: u64,
        rect: WorkRect,
        run: &pdf_rs_scene::GlyphRun,
        paint: Paint,
        map: PageMap,
        work: &mut Work<'_>,
    ) -> Result<(), FastRasterError> {
        let config = self.plan.config().input();
        let mut union = Coverage::empty(rect, base, work)?;
        for glyph_use in run.glyphs() {
            let glyph = lookup_glyph(self.graphics, glyph_use.outline())?;
            let flat = flatten_path(
                glyph.outline(),
                map,
                glyph_use.transform(),
                glyph.units_per_em(),
                config.curve_flatness_denominator,
                config.curve_recursion,
                add(base, union.retained_bytes())?,
                work,
            )?;
            let (coverage, window) = fill_coverage_bounded(
                &flat,
                rect,
                FillRule::Nonzero,
                add(add(base, union.retained_bytes())?, flat.retained_bytes())?,
                work,
            )?;
            if let Some(window) = window {
                union.union(&coverage, rect, window, work)?;
            }
        }
        composite_coverage(surface, &union, clip, paint, work)
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_glyph_stroke(
        &self,
        surface: &mut [Pixel],
        clip: &Coverage,
        base: u64,
        rect: WorkRect,
        run: &pdf_rs_scene::GlyphRun,
        paint: Paint,
        style: &pdf_rs_scene::LineStyle,
        map: PageMap,
        work: &mut Work<'_>,
    ) -> Result<(), FastRasterError> {
        let config = self.plan.config().input();
        let mut union = Coverage::empty(rect, base, work)?;
        for glyph_use in run.glyphs() {
            let glyph = lookup_glyph(self.graphics, glyph_use.outline())?;
            let coverage = stroke_coverage(
                glyph.outline(),
                glyph_use.transform(),
                glyph.units_per_em(),
                style,
                map,
                rect,
                config.curve_flatness_denominator,
                config.curve_recursion,
                add(base, union.retained_bytes())?,
                work,
            )?;
            union.union_all(&coverage, work)?;
        }
        composite_coverage(surface, &union, clip, paint, work)
    }

    #[allow(clippy::too_many_arguments)]
    fn flatten(
        &self,
        path: pdf_rs_scene::GraphicsResourceId,
        transform: Matrix,
        divisor: u16,
        map: PageMap,
        base: u64,
        work: &mut Work<'_>,
    ) -> Result<FlatPath, FastRasterError> {
        let config = self.plan.config().input();
        flatten_path(
            lookup_path(self.graphics, path)?,
            map,
            transform,
            divisor,
            config.curve_flatness_denominator,
            config.curve_recursion,
            base,
            work,
        )
    }
}

pub(super) fn validate_config(
    plan: &RenderPlan,
    limits: FastRasterLimits,
) -> Result<(), FastRasterError> {
    let config = plan.config();
    let input = config.input();
    if input.backend != NativeBackend::FastCpu
        || input.output_profile != OutputProfile::SRGB_RGBA8_STRAIGHT
        || input.output_profile.format() != PixelFormat::Rgba8
        || input.output_profile.alpha() != AlphaMode::Straight
        || input.antialias != AntialiasMode::Coverage4x4
        || input.image_sampling != ImageSampling::Nearest
        || input.glyph_sampling != GlyphSampling::OutlineCoverage
        || input.compositing != CompositingMode::PremultipliedQ16
    {
        return Err(invalid_config());
    }
    let interval = u64::from(input.cancellation_interval);
    if interval > limits.max_cancellation_interval() {
        return Err(FastRasterError::resource(
            FastRasterLimitKind::CancellationInterval,
            limits.max_cancellation_interval(),
            interval,
        ));
    }
    Ok(())
}

fn validate_subject<'a>(
    scene: &'a Scene,
    plan: &RenderPlan,
    cancellation: &dyn FastRasterCancellation,
) -> Result<&'a GraphicsScene, FastRasterError> {
    let subject = plan.decision().subject();
    let policy_limits = PolicyLimits::validate(PolicyLimitConfig {
        max_requirements: plan.decision().evaluated_requirements().max(1),
        max_dependencies: plan.decision().evaluated_dependencies().max(1),
        max_parameters: plan.decision().evaluated_parameters().max(1),
        cancellation_interval: plan.config().input().cancellation_interval,
        ..PolicyLimitConfig::default()
    })
    .map_err(|_| identity())?;
    let evaluated = CapabilityEvaluator::new(plan.decision().profile(), policy_limits)
        .evaluate(
            scene,
            subject.document_revision(),
            &PolicyCancellationAdapter(cancellation),
        )
        .map_err(map_policy_error)?;
    let graphics = validate_subject_base(scene, plan, &evaluated)?;
    for ordinal in 0..plan.tiles().len() {
        validate_tile_identity(scene, plan, ordinal)?;
    }
    Ok(graphics)
}

pub(super) fn validate_subject_base<'a>(
    scene: &'a Scene,
    plan: &RenderPlan,
    evaluated: &pdf_rs_policy::CapabilityDecision,
) -> Result<&'a GraphicsScene, FastRasterError> {
    let graphics = scene.graphics().ok_or_else(identity)?;
    let subject = plan.decision().subject();
    if evaluated != plan.decision() {
        return Err(identity());
    }
    let binding = scene.binding();
    let object = binding.page_object();
    if subject.source() != binding.source()
        || subject.revision_startxref() != binding.revision_startxref()
        || subject.page_index() != binding.page_index()
        || subject.page_object_number() != object.number()
        || subject.page_object_generation() != object.generation()
        || subject.scene_schema_major() != scene.version().major()
        || subject.scene_schema_minor() != scene.version().minor()
        || plan.decision().evaluated_commands()
            != u32::try_from(graphics.commands().len()).map_err(|_| numeric())?
        || plan.decision().evaluated_resources()
            != u32::try_from(graphics.resources().len()).map_err(|_| numeric())?
    {
        return Err(identity());
    }
    Ok(graphics)
}

pub(super) fn validate_tile_identity(
    _scene: &Scene,
    plan: &RenderPlan,
    ordinal: usize,
) -> Result<(), FastRasterError> {
    let subject = plan.decision().subject();
    let tile = plan.tiles().get(ordinal).ok_or_else(identity)?;
    let key = tile.content_key();
    if tile.ordinal() != u32::try_from(ordinal).map_err(|_| numeric())?
        || tile.plan_id() != plan.id()
        || tile.plan_hash() != plan.hash()
        || tile.generation() != plan.viewport().generation()
        || key.source() != subject.source()
        || key.document_revision() != subject.document_revision()
        || key.revision_startxref() != subject.revision_startxref()
        || key.page_index() != subject.page_index()
        || key.page_object_number() != subject.page_object_number()
        || key.page_object_generation() != subject.page_object_generation()
        || key.scene_hash() != subject.scene_hash()
        || key.decision_hash() != plan.decision().hash()
        || key.geometry_hash() != plan.viewport().geometry_hash()
        || key.render_config_hash() != plan.config().hash()
        || key.renderer_epoch() != plan.renderer_epoch()
        || key.backend() != NativeBackend::FastCpu
    {
        return Err(identity());
    }
    Ok(())
}

struct PolicyCancellationAdapter<'a>(&'a dyn FastRasterCancellation);

impl PolicyCancellation for PolicyCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

pub(super) fn map_policy_error(error: pdf_rs_policy::PolicyError) -> FastRasterError {
    match error.code() {
        PolicyErrorCode::Cancelled => FastRasterError::for_code(FastRasterErrorCode::Cancelled),
        PolicyErrorCode::Allocation => FastRasterError::for_code(FastRasterErrorCode::Allocation),
        PolicyErrorCode::NumericOverflow => {
            FastRasterError::for_code(FastRasterErrorCode::NumericOverflow)
        }
        PolicyErrorCode::InvalidLimits
        | PolicyErrorCode::ResourceLimit
        | PolicyErrorCode::SceneCanonicalization
        | PolicyErrorCode::InvalidRenderConfig
        | PolicyErrorCode::InvalidRenderRequest
        | PolicyErrorCode::IdentityMismatch
        | PolicyErrorCode::InvalidDocumentRevision => identity(),
    }
}

fn preflight_pixels(plan: &RenderPlan, limits: FastRasterLimits) -> Result<u64, FastRasterError> {
    let mut pixels = 0_u64;
    for tile in plan.tiles() {
        let rect = tile.content_key().tile();
        let count = u64::from(rect.width())
            .checked_mul(u64::from(rect.height()))
            .ok_or_else(numeric)?;
        pixels = checked_total(
            FastRasterLimitKind::Pixels,
            pixels,
            count,
            limits.max_pixels(),
        )?;
    }
    Ok(pixels)
}

fn build_bins(
    graphics: &GraphicsScene,
    plan: &RenderPlan,
    map: PageMap,
    limits: FastRasterLimits,
    work: &mut Work<'_>,
) -> Result<FastTileBins, FastRasterError> {
    let command_count = u64::try_from(graphics.commands().len()).map_err(|_| numeric())?;
    if command_count > limits.max_commands() {
        return Err(FastRasterError::resource(
            FastRasterLimitKind::Commands,
            limits.max_commands(),
            command_count,
        ));
    }
    let mut counts = Vec::<usize>::new();
    let tile_count = plan.tiles().len();
    let logical_counts_bytes = logical_vector_bytes::<usize>(tile_count)?;
    work.admit_intermediate(logical_counts_bytes)?;
    reserve(&mut counts, tile_count)?;
    let counts_bytes = vector_bytes(&counts)?;
    work.admit_intermediate(counts_bytes)?;
    for _ in 0..tile_count {
        work.step()?;
        counts.push(0);
    }
    let mut entries = 0_u64;
    for record in graphics.commands() {
        for (index, tile) in plan.tiles().iter().enumerate() {
            work.step()?;
            if command_belongs(
                record.command(),
                record.bounds(),
                tile.content_key().tile(),
                plan,
                map,
            )? {
                counts[index] = counts[index].checked_add(1).ok_or_else(numeric)?;
                entries = checked_total(
                    FastRasterLimitKind::BinEntries,
                    entries,
                    1,
                    limits.max_bin_entries(),
                )?;
            }
        }
    }

    let mut bins = Vec::new();
    let logical_bin_storage = logical_vector_bytes::<Vec<u32>>(tile_count)?
        .checked_add(
            entries
                .checked_mul(u64::try_from(size_of::<u32>()).map_err(|_| numeric())?)
                .ok_or_else(numeric)?,
        )
        .ok_or_else(numeric)?;
    checked_total(
        FastRasterLimitKind::RetainedBytes,
        0,
        logical_bin_storage,
        limits.max_retained_bytes(),
    )?;
    work.admit_intermediate(add(counts_bytes, logical_bin_storage)?)?;
    reserve(&mut bins, tile_count)?;
    let outer_bin_bytes = vector_bytes(&bins)?;
    checked_total(
        FastRasterLimitKind::RetainedBytes,
        0,
        outer_bin_bytes,
        limits.max_retained_bytes(),
    )?;
    work.admit_intermediate(add(counts_bytes, outer_bin_bytes)?)?;
    let mut allocated_bin_bytes = outer_bin_bytes;
    for count in &counts {
        let mut bin = Vec::new();
        let logical_bin_bytes = logical_vector_bytes::<u32>(*count)?;
        let logical_retained = allocated_bin_bytes
            .checked_add(logical_bin_bytes)
            .ok_or_else(numeric)?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            0,
            logical_retained,
            limits.max_retained_bytes(),
        )?;
        work.admit_intermediate(add(counts_bytes, logical_retained)?)?;
        reserve(&mut bin, *count)?;
        allocated_bin_bytes = allocated_bin_bytes
            .checked_add(vector_bytes(&bin)?)
            .ok_or_else(numeric)?;
        checked_total(
            FastRasterLimitKind::RetainedBytes,
            0,
            allocated_bin_bytes,
            limits.max_retained_bytes(),
        )?;
        work.admit_intermediate(add(counts_bytes, allocated_bin_bytes)?)?;
        work.step()?;
        bins.push(bin);
    }
    let retained = bins_retained_bytes(&bins)?;
    if retained != allocated_bin_bytes {
        return Err(identity());
    }
    if retained > limits.max_retained_bytes() {
        return Err(FastRasterError::resource(
            FastRasterLimitKind::RetainedBytes,
            limits.max_retained_bytes(),
            retained,
        ));
    }

    for (command_index, record) in graphics.commands().iter().enumerate() {
        for (tile_index, tile) in plan.tiles().iter().enumerate() {
            work.step()?;
            if command_belongs(
                record.command(),
                record.bounds(),
                tile.content_key().tile(),
                plan,
                map,
            )? {
                let bin = bins.get(tile_index).ok_or_else(identity)?;
                if bin.len() == bin.capacity() {
                    return Err(identity());
                }
                bins[tile_index].push(u32::try_from(command_index).map_err(|_| numeric())?);
            }
        }
    }
    if bins.iter().map(Vec::len).ne(counts.iter().copied()) {
        return Err(identity());
    }
    Ok(FastTileBins::new(plan.hash(), bins, entries, retained))
}

pub(super) fn command_belongs(
    command: &GraphicsCommand,
    bounds: pdf_rs_scene::SceneBounds,
    tile: pdf_rs_policy::DeviceRect,
    plan: &RenderPlan,
    map: PageMap,
) -> Result<bool, FastRasterError> {
    if matches!(
        command,
        GraphicsCommand::Save
            | GraphicsCommand::Restore
            | GraphicsCommand::Clip { .. }
            | GraphicsCommand::BeginIsolatedGroup { .. }
            | GraphicsCommand::EndIsolatedGroup
    ) {
        return Ok(true);
    }
    if !command.is_visible() {
        return Ok(false);
    }
    map.bounds_intersect(
        bounds,
        WorkRect::expanded(tile, plan.config().input().tile_halo)?,
    )
}

pub(super) fn bins_retained_bytes(bins: &Vec<Vec<u32>>) -> Result<u64, FastRasterError> {
    bins.iter().try_fold(vector_bytes(bins)?, |total, bin| {
        total.checked_add(vector_bytes(bin)?).ok_or_else(numeric)
    })
}

fn validate_permutation(
    order: &[u32],
    tile_count: usize,
    work: &mut Work<'_>,
) -> Result<(), FastRasterError> {
    if order.len() != tile_count {
        return Err(identity());
    }
    let mut seen = Vec::new();
    let logical_seen_bytes = logical_vector_bytes::<bool>(tile_count)?;
    work.admit_intermediate(logical_seen_bytes)?;
    reserve(&mut seen, tile_count)?;
    work.admit_intermediate(vector_bytes(&seen)?)?;
    for _ in 0..tile_count {
        work.step()?;
        seen.push(false);
    }
    for ordinal in order {
        work.step()?;
        let index = usize::try_from(*ordinal).map_err(|_| numeric())?;
        let slot = seen.get_mut(index).ok_or_else(identity)?;
        if *slot {
            return Err(identity());
        }
        *slot = true;
    }
    Ok(())
}

pub(super) struct Work<'a> {
    pub(super) limits: FastRasterLimits,
    cancellation: &'a dyn FastRasterCancellation,
    interval: u64,
    next_probe: u64,
    pub(super) fuel: u64,
    pub(super) cancellation_checks: u64,
    pub(super) peak_intermediate: u64,
    turn_start_fuel: u64,
    turn_fuel_limit: u64,
}

impl<'a> Work<'a> {
    pub(super) fn new(
        limits: FastRasterLimits,
        interval: u64,
        cancellation: &'a dyn FastRasterCancellation,
        fuel: u64,
        cancellation_checks: u64,
    ) -> Result<Self, FastRasterError> {
        Self::new_bounded(
            limits,
            interval,
            cancellation,
            fuel,
            cancellation_checks,
            u64::MAX,
        )
    }

    pub(super) fn new_bounded(
        limits: FastRasterLimits,
        interval: u64,
        cancellation: &'a dyn FastRasterCancellation,
        fuel: u64,
        cancellation_checks: u64,
        turn_fuel_limit: u64,
    ) -> Result<Self, FastRasterError> {
        if turn_fuel_limit == 0 {
            return Err(invalid_config());
        }
        if interval == 0 || interval > limits.max_cancellation_interval() {
            return Err(invalid_config());
        }
        let next_probe = fuel
            .checked_div(interval)
            .and_then(|value| value.checked_add(1))
            .and_then(|value| value.checked_mul(interval))
            .ok_or_else(numeric)?;
        let mut work = Self {
            limits,
            cancellation,
            interval,
            next_probe,
            fuel,
            cancellation_checks,
            peak_intermediate: 0,
            turn_start_fuel: fuel,
            turn_fuel_limit,
        };
        work.check()?;
        Ok(work)
    }

    pub(super) fn check(&mut self) -> Result<(), FastRasterError> {
        self.cancellation_checks = self
            .cancellation_checks
            .checked_add(1)
            .ok_or_else(numeric)?;
        if self.cancellation.is_cancelled() {
            return Err(FastRasterError::for_code(FastRasterErrorCode::Cancelled));
        }
        Ok(())
    }
}

impl KernelWork for Work<'_> {
    fn step(&mut self) -> Result<(), FastRasterError> {
        let turn_fuel = self
            .fuel
            .checked_sub(self.turn_start_fuel)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(numeric)?;
        if turn_fuel > self.turn_fuel_limit {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::AtomicTileFuel,
                self.turn_fuel_limit,
                turn_fuel,
            ));
        }
        self.fuel = checked_total(
            FastRasterLimitKind::Fuel,
            self.fuel,
            1,
            self.limits.max_fuel(),
        )?;
        if self.fuel == self.next_probe {
            self.check()?;
            self.next_probe = self
                .next_probe
                .checked_add(self.interval)
                .ok_or_else(numeric)?;
        }
        Ok(())
    }

    fn admit_intermediate(&mut self, bytes: u64) -> Result<(), FastRasterError> {
        if bytes > self.limits.max_intermediate_bytes() {
            return Err(FastRasterError::resource(
                FastRasterLimitKind::IntermediateBytes,
                self.limits.max_intermediate_bytes(),
                bytes,
            ));
        }
        self.peak_intermediate = self.peak_intermediate.max(bytes);
        Ok(())
    }
}

fn state_bytes(
    surface_bytes: u64,
    clip: &Coverage,
    stack: &Vec<Coverage>,
    stack_payload_bytes: u64,
    group_stack_bytes: u64,
) -> Result<u64, FastRasterError> {
    state_payload_bytes(surface_bytes, clip, stack_payload_bytes, group_stack_bytes)?
        .checked_add(vector_bytes(stack)?)
        .ok_or_else(numeric)
}

fn state_payload_bytes(
    surface_bytes: u64,
    clip: &Coverage,
    stack_payload_bytes: u64,
    group_stack_bytes: u64,
) -> Result<u64, FastRasterError> {
    surface_bytes
        .checked_add(clip.retained_bytes())
        .and_then(|value| value.checked_add(stack_payload_bytes))
        .and_then(|value| value.checked_add(group_stack_bytes))
        .ok_or_else(numeric)
}

pub(super) fn logical_vector_bytes<T>(length: usize) -> Result<u64, FastRasterError> {
    u64::try_from(length)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(numeric)
}

pub(super) fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), FastRasterError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| FastRasterError::for_code(FastRasterErrorCode::Allocation))
}

fn add(left: u64, right: u64) -> Result<u64, FastRasterError> {
    left.checked_add(right).ok_or_else(numeric)
}

fn invalid_config() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::InvalidRenderConfig)
}

pub(super) fn identity() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::IdentityMismatch)
}

fn command_sequence() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::InvalidCommandSequence)
}

pub(super) fn numeric() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::NumericOverflow)
}
