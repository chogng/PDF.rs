use std::mem::size_of;

use crate::{
    BlendMode, CapabilityContext, CapabilityRequirement, CapabilityRequirementId, CapabilityStatus,
    CommandSource, FillRule, GlyphRun, GlyphUse, GraphicsCapability, GraphicsCommand,
    GraphicsCommandRecord, GraphicsResource, GraphicsResourceEntry, GraphicsResourceId,
    GraphicsScene, GraphicsSceneStats, ImageResource, LineStyle, Matrix, PageGeometry, Paint,
    PathResource, PositionedGlyph, Scene, SceneBinding, SceneBounds, SceneError, SceneErrorCode,
    SceneLimitKind, ScenePoint, SceneUnit,
};

const HARD_MAX_GRAPHICS_COMMANDS: u32 = 4_000_000;
const HARD_MAX_GRAPHICS_RESOURCES: u32 = 1_000_000;
const HARD_MAX_REQUIREMENTS: u32 = 4_000_000;
const HARD_MAX_DEPENDENCIES: u32 = 16_000_000;
const HARD_MAX_PATH_SEGMENTS: u64 = 100_000_000;
const HARD_MAX_IMAGE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_GLYPHS: u64 = 100_000_000;
const HARD_MAX_STATE_DEPTH: u32 = 65_536;
const HARD_MAX_GROUP_DEPTH: u32 = 65_536;
const HARD_MAX_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_RESOURCE_INDEX_WORK: u64 = 1_000_000_000_000;
const HARD_MAX_CANONICAL_BYTES: u64 = 1024 * 1024 * 1024;

/// Unvalidated limits for one graphics-capable Scene v2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphicsSceneLimitConfig {
    /// Maximum graphics command records.
    pub max_commands: u32,
    /// Maximum first-use graphics resources.
    pub max_resources: u32,
    /// Maximum capability requirement nodes.
    pub max_requirements: u32,
    /// Maximum aggregate capability dependency identifiers.
    pub max_dependencies: u32,
    /// Maximum aggregate path segments retained by resources.
    pub max_path_segments: u64,
    /// Maximum aggregate decoded image bytes retained by resources.
    pub max_image_bytes: u64,
    /// Maximum positioned glyphs retained by commands.
    pub max_glyphs: u64,
    /// Maximum saved graphics-state nesting depth.
    pub max_state_depth: u32,
    /// Maximum isolated-group nesting depth.
    pub max_group_depth: u32,
    /// Maximum builder transaction-live and published retained bytes.
    ///
    /// Transaction admission includes simultaneously live inputs, pending values, and replacement
    /// vectors, so the minimum successful limit can exceed the final published Scene statistic.
    pub max_retained_bytes: u64,
    /// Maximum resource comparisons and payload comparison units.
    pub max_resource_index_work: u64,
    /// Maximum canonical Scene v2 output bytes.
    pub max_canonical_bytes: u64,
}

impl Default for GraphicsSceneLimitConfig {
    fn default() -> Self {
        Self {
            max_commands: 250_000,
            max_resources: 65_536,
            max_requirements: 250_000,
            max_dependencies: 1_000_000,
            max_path_segments: 4_000_000,
            max_image_bytes: 256 * 1024 * 1024,
            max_glyphs: 4_000_000,
            max_state_depth: 4_096,
            max_group_depth: 1_024,
            max_retained_bytes: 512 * 1024 * 1024,
            max_resource_index_work: 4_000_000_000,
            max_canonical_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Validated limits for one graphics-capable Scene v2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphicsSceneLimits {
    config: GraphicsSceneLimitConfig,
}

impl GraphicsSceneLimits {
    /// Validates every nonzero dimension against fixed hard ceilings.
    pub fn validate(config: GraphicsSceneLimitConfig) -> Result<Self, SceneError> {
        if config.max_commands == 0
            || config.max_commands > HARD_MAX_GRAPHICS_COMMANDS
            || config.max_resources == 0
            || config.max_resources > HARD_MAX_GRAPHICS_RESOURCES
            || config.max_requirements == 0
            || config.max_requirements > HARD_MAX_REQUIREMENTS
            || config.max_dependencies == 0
            || config.max_dependencies > HARD_MAX_DEPENDENCIES
            || config.max_path_segments == 0
            || config.max_path_segments > HARD_MAX_PATH_SEGMENTS
            || config.max_image_bytes == 0
            || config.max_image_bytes > HARD_MAX_IMAGE_BYTES
            || config.max_glyphs == 0
            || config.max_glyphs > HARD_MAX_GLYPHS
            || config.max_state_depth == 0
            || config.max_state_depth > HARD_MAX_STATE_DEPTH
            || config.max_group_depth == 0
            || config.max_group_depth > HARD_MAX_GROUP_DEPTH
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
            || config.max_resource_index_work == 0
            || config.max_resource_index_work > HARD_MAX_RESOURCE_INDEX_WORK
            || config.max_canonical_bytes == 0
            || config.max_canonical_bytes > HARD_MAX_CANONICAL_BYTES
        {
            return Err(SceneError::for_code(SceneErrorCode::InvalidLimits, None));
        }
        Ok(Self { config })
    }

    /// Returns the maximum graphics command count.
    pub const fn max_commands(self) -> u32 {
        self.config.max_commands
    }

    /// Returns the maximum graphics resource count.
    pub const fn max_resources(self) -> u32 {
        self.config.max_resources
    }

    /// Returns the maximum capability requirement count.
    pub const fn max_requirements(self) -> u32 {
        self.config.max_requirements
    }

    /// Returns the maximum aggregate dependency count.
    pub const fn max_dependencies(self) -> u32 {
        self.config.max_dependencies
    }

    /// Returns the maximum aggregate path segment count.
    pub const fn max_path_segments(self) -> u64 {
        self.config.max_path_segments
    }

    /// Returns the maximum aggregate decoded image byte count.
    pub const fn max_image_bytes(self) -> u64 {
        self.config.max_image_bytes
    }

    /// Returns the maximum positioned glyph count.
    pub const fn max_glyphs(self) -> u64 {
        self.config.max_glyphs
    }

    /// Returns the maximum saved graphics-state depth.
    pub const fn max_state_depth(self) -> u32 {
        self.config.max_state_depth
    }

    /// Returns the maximum isolated-group depth.
    pub const fn max_group_depth(self) -> u32 {
        self.config.max_group_depth
    }

    /// Returns the maximum builder transaction-live and published retained bytes.
    ///
    /// This is a working-peak bound, not only a bound on final `GraphicsSceneStats` retention.
    pub const fn max_retained_bytes(self) -> u64 {
        self.config.max_retained_bytes
    }

    /// Returns the maximum resource-index comparison work.
    pub const fn max_resource_index_work(self) -> u64 {
        self.config.max_resource_index_work
    }

    /// Returns the maximum canonical Scene v2 bytes.
    pub const fn max_canonical_bytes(self) -> u64 {
        self.config.max_canonical_bytes
    }
}

impl Default for GraphicsSceneLimits {
    fn default() -> Self {
        Self::validate(GraphicsSceneLimitConfig::default())
            .expect("built-in graphics Scene limits satisfy hard ceilings")
    }
}

/// Bounded single-owner builder for one immutable graphics-capable Scene v2.
pub struct GraphicsSceneBuilder {
    binding: SceneBinding,
    geometry: PageGeometry,
    limits: GraphicsSceneLimits,
    commands: Vec<GraphicsCommandRecord>,
    resources: Vec<GraphicsResourceEntry>,
    requirements: Vec<CapabilityRequirement>,
    nested_retained_bytes: u64,
    resource_index_work: u64,
    dependency_count: u32,
    path_segments: u64,
    image_bytes: u64,
    glyphs: u64,
    state_depth: u32,
    group_depth: u32,
}

impl GraphicsSceneBuilder {
    /// Creates an empty Scene v2 builder without allocating command or resource capacity.
    pub fn new_v2(
        binding: SceneBinding,
        geometry: PageGeometry,
        limits: GraphicsSceneLimits,
    ) -> Self {
        Self {
            binding,
            geometry,
            limits,
            commands: Vec::new(),
            resources: Vec::new(),
            requirements: Vec::new(),
            nested_retained_bytes: 0,
            resource_index_work: 0,
            dependency_count: 0,
            path_segments: 0,
            image_bytes: 0,
            glyphs: 0,
            state_depth: 0,
            group_depth: 0,
        }
    }

    /// Appends one complete graphics-state save.
    pub fn append_save(
        &mut self,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        let next = self.state_depth.checked_add(1).ok_or_else(internal)?;
        if next > self.limits.max_state_depth() {
            return Err(limit(
                SceneLimitKind::GraphicsStateDepth,
                u64::from(self.limits.max_state_depth()),
                u64::from(self.state_depth),
                1,
            ));
        }
        self.append_simple(GraphicsCommand::Save, bounds, source)?;
        self.state_depth = next;
        Ok(())
    }

    /// Appends one complete graphics-state restore.
    pub fn append_restore(
        &mut self,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        if self.state_depth == 0 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            ));
        }
        self.append_simple(GraphicsCommand::Restore, bounds, source)?;
        self.state_depth -= 1;
        Ok(())
    }

    /// Appends one path clipping operation.
    pub fn append_clip(
        &mut self,
        path: PathResource,
        rule: FillRule,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_fill_bounds(&path, transform, bounds)?;
        self.append_with_resources(
            vec![GraphicsResource::Path(path)],
            |ids, _| {
                Ok(GraphicsCommand::Clip {
                    path: ids[0],
                    rule,
                    transform,
                })
            },
            bounds,
            source,
            vec![(
                GraphicsCapability::Clip,
                u64::from(rule == FillRule::EvenOdd),
            )],
            0,
            0,
            0,
        )
    }

    /// Appends one path fill.
    pub fn append_fill(
        &mut self,
        path: PathResource,
        rule: FillRule,
        paint: Paint,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_fill_bounds(&path, transform, bounds)?;
        let capabilities =
            paint_capabilities(paint, GraphicsCapability::PathFill, rule_parameter(rule));
        self.append_with_resources(
            vec![GraphicsResource::Path(path)],
            |ids, _| {
                Ok(GraphicsCommand::Fill {
                    path: ids[0],
                    rule,
                    paint,
                    transform,
                })
            },
            bounds,
            source,
            capabilities,
            0,
            0,
            0,
        )
    }

    /// Appends one path stroke.
    pub fn append_stroke(
        &mut self,
        path: PathResource,
        paint: Paint,
        style: LineStyle,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_nonempty_path_bounds(&path, bounds)?;
        let capabilities = paint_capabilities(paint, GraphicsCapability::PathStroke, 0);
        self.append_with_resources(
            vec![GraphicsResource::Path(path)],
            |ids, _| {
                Ok(GraphicsCommand::Stroke {
                    path: ids[0],
                    paint,
                    style,
                    transform,
                })
            },
            bounds,
            source,
            capabilities,
            0,
            0,
            0,
        )
    }

    /// Appends one fill-then-stroke path operation.
    #[allow(
        clippy::too_many_arguments,
        reason = "the semantic command retains independent fill, stroke, line, transform, bounds, and provenance values"
    )]
    pub fn append_fill_stroke(
        &mut self,
        path: PathResource,
        rule: FillRule,
        fill: Paint,
        stroke: Paint,
        style: LineStyle,
        transform: Matrix,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_nonempty_path_bounds(&path, bounds)?;
        let mut capabilities =
            paint_capabilities(fill, GraphicsCapability::PathFill, rule_parameter(rule));
        capabilities.extend(paint_capabilities(
            stroke,
            GraphicsCapability::PathStroke,
            0,
        ));
        self.append_with_resources(
            vec![GraphicsResource::Path(path)],
            |ids, _| {
                Ok(GraphicsCommand::FillStroke {
                    path: ids[0],
                    rule,
                    fill,
                    stroke,
                    style,
                    transform,
                })
            },
            bounds,
            source,
            capabilities,
            0,
            0,
            0,
        )
    }

    /// Appends one basic decoded image draw.
    #[allow(
        clippy::too_many_arguments,
        reason = "the image command retains resource, transform, alpha, blend, bounds, and provenance independently"
    )]
    pub fn draw_image(
        &mut self,
        image: ImageResource,
        transform: Matrix,
        alpha: SceneUnit,
        blend_mode: BlendMode,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_nonempty_bounds(bounds)?;
        let parameter = image_parameter(&image);
        let mut capabilities = vec![(GraphicsCapability::Image, parameter)];
        append_alpha_blend_capabilities(&mut capabilities, alpha, blend_mode);
        self.append_with_resources(
            vec![GraphicsResource::Image(image)],
            |ids, _| {
                Ok(GraphicsCommand::DrawImage {
                    image: ids[0],
                    transform,
                    alpha,
                    blend_mode,
                })
            },
            bounds,
            source,
            capabilities,
            0,
            0,
            0,
        )
    }

    /// Appends one positioned embedded-glyph run.
    pub fn draw_glyph_run(
        &mut self,
        glyphs: Vec<GlyphUse>,
        paint: Paint,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        if glyphs.is_empty() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            ));
        }
        validate_nonempty_bounds(bounds)?;
        let glyph_count = u64::try_from(glyphs.len()).map_err(|_| internal())?;
        ensure_total(
            SceneLimitKind::Glyphs,
            self.limits.max_glyphs(),
            self.glyphs,
            glyph_count,
        )?;
        let persistent_retained = self.retained_bytes()?;
        let glyph_input_retained = capacity_bytes::<GlyphUse>(glyphs.capacity())?;
        let glyph_input_nested = glyphs.iter().try_fold(0_u64, |retained, glyph| {
            retained
                .checked_add(glyph.outline().outline().retained_bytes()?)
                .ok_or_else(internal)
        })?;
        let positioned_retained = capacity_bytes::<PositionedGlyph>(glyphs.len())?;
        let resource_retained = capacity_bytes::<GraphicsResource>(glyphs.len())?;
        let id_retained = capacity_bytes::<GraphicsResourceId>(glyphs.len())?;
        let append_nominal = resource_retained
            .checked_mul(2)
            .and_then(|value| value.checked_add(id_retained))
            .ok_or_else(internal)?;
        let command_nominal = resource_retained
            .checked_add(id_retained)
            .and_then(|value| value.checked_add(positioned_retained))
            .ok_or_else(internal)?;
        let glyph_append_nominal = glyph_input_retained
            .checked_add(glyph_input_nested)
            .and_then(|value| value.checked_add(append_nominal.max(command_nominal)))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            glyph_append_nominal,
        )?;
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(glyphs.len())
            .map_err(|_| allocation(self.limits.max_retained_bytes()))?;
        let resource_actual = capacity_bytes::<GraphicsResource>(resources.capacity())?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            glyph_input_retained
                .checked_add(glyph_input_nested)
                .and_then(|value| value.checked_add(resource_actual))
                .and_then(|value| value.checked_add(resource_retained))
                .and_then(|value| value.checked_add(id_retained))
                .ok_or_else(internal)?,
        )?;
        for glyph in &glyphs {
            resources.push(GraphicsResource::GlyphOutline(glyph.outline().clone()));
        }
        let capabilities = paint_capabilities(paint, GraphicsCapability::Glyph, glyph_count);
        let allocation_limit = self.limits.max_retained_bytes();
        self.append_with_resources(
            resources,
            move |ids, append_transient| {
                ensure_total(
                    SceneLimitKind::RetainedBytes,
                    allocation_limit,
                    persistent_retained,
                    append_transient
                        .checked_add(positioned_retained)
                        .ok_or_else(internal)?,
                )?;
                let mut positioned = Vec::new();
                positioned
                    .try_reserve_exact(glyphs.len())
                    .map_err(|_| allocation(allocation_limit))?;
                let positioned_actual = capacity_bytes::<PositionedGlyph>(positioned.capacity())?;
                ensure_total(
                    SceneLimitKind::RetainedBytes,
                    allocation_limit,
                    persistent_retained,
                    append_transient
                        .checked_add(positioned_actual)
                        .ok_or_else(internal)?,
                )?;
                for (glyph, id) in glyphs.into_iter().zip(ids) {
                    positioned.push(PositionedGlyph::new(
                        *id,
                        glyph.transform(),
                        glyph.character_code(),
                    ));
                }
                Ok(GraphicsCommand::DrawGlyphRun(GlyphRun::from_reserved(
                    positioned, paint,
                )?))
            },
            bounds,
            source,
            capabilities,
            glyph_count,
            glyph_input_retained,
            glyph_input_nested,
        )
    }

    /// Begins one isolated transparency group.
    pub fn begin_group(
        &mut self,
        alpha: SceneUnit,
        blend_mode: BlendMode,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_nonempty_bounds(bounds)?;
        let next = self.group_depth.checked_add(1).ok_or_else(internal)?;
        if next > self.limits.max_group_depth() {
            return Err(limit(
                SceneLimitKind::GraphicsGroupDepth,
                u64::from(self.limits.max_group_depth()),
                u64::from(self.group_depth),
                1,
            ));
        }
        let mut capabilities = vec![(GraphicsCapability::IsolatedGroup, 0)];
        append_alpha_blend_capabilities(&mut capabilities, alpha, blend_mode);
        self.append_with_resources(
            Vec::new(),
            |_, _| Ok(GraphicsCommand::BeginIsolatedGroup { alpha, blend_mode }),
            bounds,
            source,
            capabilities,
            0,
            0,
            0,
        )?;
        self.group_depth = next;
        Ok(())
    }

    /// Ends the current isolated transparency group.
    pub fn end_group(
        &mut self,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        validate_nonempty_bounds(bounds)?;
        if self.group_depth == 0 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            ));
        }
        self.append_simple(GraphicsCommand::EndIsolatedGroup, bounds, source)?;
        self.group_depth -= 1;
        Ok(())
    }

    /// Replays one compatible graphics Scene into this builder with fresh resource interning.
    ///
    /// The imported Scene must have the same page binding and geometry and contain only supported
    /// graphics-v2 semantics. Each imported command is revalidated against this builder's
    /// remaining limits, and referenced paths, images, and glyph outlines receive IDs in this
    /// builder's first-use order. Callers must discard the builder if this method returns an error,
    /// because an already validated command prefix may have been appended.
    pub fn append_scene(&mut self, scene: &Scene) -> Result<(), SceneError> {
        if scene.binding() != self.binding || scene.geometry() != self.geometry {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            ));
        }
        let graphics = scene.graphics().ok_or_else(|| {
            SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            )
        })?;
        if !graphics.is_supported() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.next_command_index().ok(),
            ));
        }
        for record in graphics.commands() {
            let bounds = record.bounds();
            let source = record.source();
            match record.command() {
                GraphicsCommand::Save => self.append_save(bounds, source)?,
                GraphicsCommand::Restore => self.append_restore(bounds, source)?,
                GraphicsCommand::Clip {
                    path,
                    rule,
                    transform,
                } => self.append_clip(
                    imported_path(graphics, *path)?,
                    *rule,
                    *transform,
                    bounds,
                    source,
                )?,
                GraphicsCommand::Fill {
                    path,
                    rule,
                    paint,
                    transform,
                } => self.append_fill(
                    imported_path(graphics, *path)?,
                    *rule,
                    *paint,
                    *transform,
                    bounds,
                    source,
                )?,
                GraphicsCommand::Stroke {
                    path,
                    paint,
                    style,
                    transform,
                } => self.append_stroke(
                    imported_path(graphics, *path)?,
                    *paint,
                    style.clone(),
                    *transform,
                    bounds,
                    source,
                )?,
                GraphicsCommand::FillStroke {
                    path,
                    rule,
                    fill,
                    stroke,
                    style,
                    transform,
                } => self.append_fill_stroke(
                    imported_path(graphics, *path)?,
                    *rule,
                    *fill,
                    *stroke,
                    style.clone(),
                    *transform,
                    bounds,
                    source,
                )?,
                GraphicsCommand::DrawImage {
                    image,
                    transform,
                    alpha,
                    blend_mode,
                } => self.draw_image(
                    imported_image(graphics, *image)?,
                    *transform,
                    *alpha,
                    *blend_mode,
                    bounds,
                    source,
                )?,
                GraphicsCommand::DrawGlyphRun(run) => {
                    let mut glyphs = Vec::new();
                    glyphs
                        .try_reserve_exact(run.glyphs().len())
                        .map_err(|_| allocation(self.limits.max_retained_bytes()))?;
                    for glyph in run.glyphs() {
                        glyphs.push(GlyphUse::new(
                            imported_glyph(graphics, glyph.outline())?,
                            glyph.transform(),
                            glyph.character_code(),
                        ));
                    }
                    self.draw_glyph_run(glyphs, run.paint(), bounds, source)?;
                }
                GraphicsCommand::BeginIsolatedGroup { alpha, blend_mode } => {
                    self.begin_group(*alpha, *blend_mode, bounds, source)?;
                }
                GraphicsCommand::EndIsolatedGroup => self.end_group(bounds, source)?,
            }
        }
        Ok(())
    }

    /// Adds one explicit capability requirement.
    ///
    /// Dependencies must be unique identifiers already present in this builder. That strict
    /// backward-only rule prevents cycles without an unbounded graph traversal.
    pub fn add_requirement(
        &mut self,
        capability: GraphicsCapability,
        parameter: u64,
        context: CapabilityContext,
        dependencies: Vec<CapabilityRequirementId>,
        status: CapabilityStatus,
    ) -> Result<CapabilityRequirementId, SceneError> {
        self.validate_context(context)?;
        let id = self.next_requirement_id()?;
        self.validate_dependencies(id, &dependencies)?;
        let dependency_len = dependencies.len();
        self.ensure_requirement_capacity(1, dependency_len)?;
        let requirement =
            CapabilityRequirement::new(id, capability, parameter, context, dependencies, status)?;
        let additional_nested = requirement.retained_bytes()?;
        let (resources_replacement, commands_replacement, requirements_replacement, retained) =
            self.prepare_storage(0, 0, 1, additional_nested, additional_nested)?;
        install_replacement(&mut self.resources, resources_replacement);
        install_replacement(&mut self.commands, commands_replacement);
        install_replacement(&mut self.requirements, requirements_replacement);
        self.requirements.push(requirement);
        self.nested_retained_bytes = self
            .nested_retained_bytes
            .checked_add(additional_nested)
            .ok_or_else(internal)?;
        debug_assert_eq!(self.retained_bytes().ok(), Some(retained));
        self.dependency_count = self
            .dependency_count
            .checked_add(u32::try_from(dependency_len).map_err(|_| internal())?)
            .ok_or_else(internal)?;
        Ok(id)
    }

    /// Validates terminal balance and atomically publishes one immutable Scene v2.
    pub fn finish(self) -> Result<Scene, SceneError> {
        if self.state_depth != 0 || self.group_depth != 0 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                self.commands
                    .len()
                    .checked_sub(1)
                    .and_then(|index| u32::try_from(index).ok()),
            ));
        }
        let retained_bytes = self.retained_bytes()?;
        let graphics = GraphicsScene::new(
            self.commands,
            self.resources,
            self.requirements,
            self.limits,
            GraphicsSceneStats::new(retained_bytes, self.resource_index_work),
        );
        Ok(Scene::new_graphics(self.binding, self.geometry, graphics))
    }

    /// Returns current allocator-reported outer and nested retained capacity.
    pub fn retained_bytes(&self) -> Result<u64, SceneError> {
        retained_for_capacities(
            self.resources.capacity(),
            self.commands.capacity(),
            self.requirements.capacity(),
            self.nested_retained_bytes,
        )
    }

    /// Returns resource comparisons and payload comparison units consumed so far.
    pub const fn resource_index_work(&self) -> u64 {
        self.resource_index_work
    }

    fn append_simple(
        &mut self,
        command: GraphicsCommand,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Result<(), SceneError> {
        self.append_with_resources(
            Vec::new(),
            |_, _| Ok(command),
            bounds,
            source,
            Vec::new(),
            0,
            0,
            0,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the transaction receives independent resource, command, bounds, provenance, capability, and glyph inputs"
    )]
    fn append_with_resources(
        &mut self,
        requested: Vec<GraphicsResource>,
        make_command: impl FnOnce(&[GraphicsResourceId], u64) -> Result<GraphicsCommand, SceneError>,
        bounds: SceneBounds,
        source: CommandSource,
        capabilities: Vec<(GraphicsCapability, u64)>,
        glyph_count: u64,
        command_input_transient: u64,
        preindex_nested_transient: u64,
    ) -> Result<(), SceneError> {
        let command_index = self.next_command_index()?;
        let persistent_retained = self.retained_bytes()?;
        let requested_retained = capacity_bytes::<GraphicsResource>(requested.capacity())?;
        let capabilities_retained =
            capacity_bytes::<(GraphicsCapability, u64)>(capabilities.capacity())?;
        let pending_nominal = capacity_bytes::<GraphicsResource>(requested.len())?;
        let ids_nominal = capacity_bytes::<GraphicsResourceId>(requested.len())?;
        let initial_transient = requested_retained
            .checked_add(capabilities_retained)
            .and_then(|value| value.checked_add(command_input_transient))
            .and_then(|value| value.checked_add(preindex_nested_transient))
            .and_then(|value| value.checked_add(pending_nominal))
            .and_then(|value| value.checked_add(ids_nominal))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            initial_transient,
        )?;
        let mut pending = Vec::<GraphicsResource>::new();
        pending
            .try_reserve_exact(requested.len())
            .map_err(|_| allocation(self.limits.max_retained_bytes()))?;
        let pending_actual = capacity_bytes::<GraphicsResource>(pending.capacity())?;
        let pending_transient = requested_retained
            .checked_add(capabilities_retained)
            .and_then(|value| value.checked_add(command_input_transient))
            .and_then(|value| value.checked_add(preindex_nested_transient))
            .and_then(|value| value.checked_add(pending_actual))
            .and_then(|value| value.checked_add(ids_nominal))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            pending_transient,
        )?;
        let mut ids = Vec::<GraphicsResourceId>::new();
        ids.try_reserve_exact(requested.len())
            .map_err(|_| allocation(self.limits.max_retained_bytes()))?;
        let ids_actual = capacity_bytes::<GraphicsResourceId>(ids.capacity())?;
        let indexed_transient = requested_retained
            .checked_add(capabilities_retained)
            .and_then(|value| value.checked_add(command_input_transient))
            .and_then(|value| value.checked_add(preindex_nested_transient))
            .and_then(|value| value.checked_add(pending_actual))
            .and_then(|value| value.checked_add(ids_actual))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            indexed_transient,
        )?;
        for resource in requested {
            let mut existing = None;
            for index in 0..self.resources.len() {
                let comparison_work = resource.comparison_work(self.resources[index].resource())?;
                self.charge_resource_comparison(comparison_work, command_index)?;
                let entry = &self.resources[index];
                if resource.has_conflicting_identity(entry.resource()) {
                    return Err(SceneError::for_code(
                        SceneErrorCode::InvalidCommandSequence,
                        Some(command_index),
                    ));
                }
                if entry.resource() == &resource {
                    existing = Some(entry.id());
                    break;
                }
            }
            if let Some(id) = existing {
                ids.push(id);
                continue;
            }
            let mut pending_index = None;
            for (index, entry) in pending.iter().enumerate() {
                self.charge_resource_comparison(resource.comparison_work(entry)?, command_index)?;
                if resource.has_conflicting_identity(entry) {
                    return Err(SceneError::for_code(
                        SceneErrorCode::InvalidCommandSequence,
                        Some(command_index),
                    ));
                }
                if entry == &resource {
                    pending_index = Some(index);
                    break;
                }
            }
            if let Some(index) = pending_index {
                let offset = u32::try_from(index).map_err(|_| internal())?;
                let value = u32::try_from(self.resources.len())
                    .map_err(|_| internal())?
                    .checked_add(offset)
                    .ok_or_else(internal)?;
                ids.push(GraphicsResourceId::new(value));
                continue;
            }
            let value = u32::try_from(self.resources.len())
                .map_err(|_| internal())?
                .checked_add(u32::try_from(pending.len()).map_err(|_| internal())?)
                .ok_or_else(internal)?;
            ids.push(GraphicsResourceId::new(value));
            pending.push(resource);
        }

        let (pending_path_segments, pending_image_bytes) = resource_totals(&pending)?;
        let next_path_segments = self
            .path_segments
            .checked_add(pending_path_segments)
            .ok_or_else(internal)?;
        let next_image_bytes = self
            .image_bytes
            .checked_add(pending_image_bytes)
            .ok_or_else(internal)?;
        let next_glyphs = self.glyphs.checked_add(glyph_count).ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::PathSegments,
            self.limits.max_path_segments(),
            self.path_segments,
            pending_path_segments,
        )?;
        ensure_total(
            SceneLimitKind::ImageBytes,
            self.limits.max_image_bytes(),
            self.image_bytes,
            pending_image_bytes,
        )?;
        ensure_total(
            SceneLimitKind::Glyphs,
            self.limits.max_glyphs(),
            self.glyphs,
            glyph_count,
        )?;
        let pending_resource_count = u32::try_from(pending.len()).map_err(|_| internal())?;
        ensure_total(
            SceneLimitKind::GraphicsResources,
            u64::from(self.limits.max_resources()),
            u64::try_from(self.resources.len()).map_err(|_| internal())?,
            u64::from(pending_resource_count),
        )?;

        let pending_resource_nested = nested_retained_for_resources(&pending)?;
        let precommand_resource_nested = pending_resource_nested.max(preindex_nested_transient);
        let requirement_count = unique_capability_count(&capabilities);
        let requirements_nominal = capacity_bytes::<CapabilityRequirement>(requirement_count)?;
        let before_requirements = pending_actual
            .checked_add(ids_actual)
            .and_then(|value| value.checked_add(capabilities_retained))
            .and_then(|value| value.checked_add(command_input_transient))
            .and_then(|value| value.checked_add(precommand_resource_nested))
            .and_then(|value| value.checked_add(requirements_nominal))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            before_requirements,
        )?;
        let pending_requirements =
            self.prepare_auto_requirements(command_index, &capabilities, requirement_count)?;
        self.ensure_requirement_capacity(pending_requirements.len(), 0)?;
        let pending_requirements_retained =
            capacity_bytes::<CapabilityRequirement>(pending_requirements.capacity())?;
        let pending_requirement_nested = nested_retained_for_requirements(&pending_requirements)?;
        let append_transient = pending_actual
            .checked_add(ids_actual)
            .and_then(|value| value.checked_add(capabilities_retained))
            .and_then(|value| value.checked_add(command_input_transient))
            .and_then(|value| value.checked_add(precommand_resource_nested))
            .and_then(|value| value.checked_add(pending_requirements_retained))
            .and_then(|value| value.checked_add(pending_requirement_nested))
            .ok_or_else(internal)?;
        ensure_total(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            persistent_retained,
            append_transient,
        )?;
        let command = make_command(&ids, append_transient)?;
        let command_nested = command.retained_bytes()?;
        let additional_nested = pending_resource_nested
            .checked_add(command_nested)
            .and_then(|value| value.checked_add(pending_requirement_nested))
            .ok_or_else(internal)?;
        let transaction_transient = pending_actual
            .checked_add(ids_actual)
            .and_then(|value| value.checked_add(capabilities_retained))
            .and_then(|value| value.checked_add(pending_requirements_retained))
            .and_then(|value| value.checked_add(additional_nested))
            .ok_or_else(internal)?;
        let (resources_replacement, commands_replacement, requirements_replacement, retained) =
            self.prepare_storage(
                pending.len(),
                1,
                pending_requirements.len(),
                additional_nested,
                transaction_transient,
            )?;
        install_replacement(&mut self.resources, resources_replacement);
        install_replacement(&mut self.commands, commands_replacement);
        install_replacement(&mut self.requirements, requirements_replacement);

        for resource in pending {
            let id = GraphicsResourceId::new(
                u32::try_from(self.resources.len()).map_err(|_| internal())?,
            );
            self.resources
                .push(GraphicsResourceEntry::new(id, resource));
        }
        for requirement in pending_requirements {
            self.requirements.push(requirement);
        }
        self.commands
            .push(GraphicsCommandRecord::new(command, bounds, source));
        self.nested_retained_bytes = self
            .nested_retained_bytes
            .checked_add(additional_nested)
            .ok_or_else(internal)?;
        debug_assert_eq!(self.retained_bytes().ok(), Some(retained));
        self.path_segments = next_path_segments;
        self.image_bytes = next_image_bytes;
        self.glyphs = next_glyphs;
        Ok(())
    }

    fn prepare_auto_requirements(
        &self,
        command_index: u32,
        capabilities: &[(GraphicsCapability, u64)],
        requirement_count: usize,
    ) -> Result<Vec<CapabilityRequirement>, SceneError> {
        let context = CapabilityContext::Command(command_index);
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(requirement_count)
            .map_err(|_| allocation(self.limits.max_retained_bytes()))?;
        for &(capability, parameter) in capabilities {
            if pending.iter().any(|requirement: &CapabilityRequirement| {
                requirement.capability() == capability
                    && requirement.parameter() == parameter
                    && requirement.context() == context
            }) {
                continue;
            }
            let value = u32::try_from(self.requirements.len())
                .map_err(|_| internal())?
                .checked_add(u32::try_from(pending.len()).map_err(|_| internal())?)
                .ok_or_else(internal)?;
            pending.push(CapabilityRequirement::new(
                CapabilityRequirementId::new(value),
                capability,
                parameter,
                context,
                Vec::new(),
                CapabilityStatus::Supported,
            )?);
        }
        Ok(pending)
    }

    fn validate_context(&self, context: CapabilityContext) -> Result<(), SceneError> {
        match context {
            CapabilityContext::Scene => Ok(()),
            CapabilityContext::Command(index)
                if usize::try_from(index)
                    .ok()
                    .is_some_and(|index| index < self.commands.len()) =>
            {
                Ok(())
            }
            CapabilityContext::Resource(id)
                if usize::try_from(id.value())
                    .ok()
                    .is_some_and(|index| index < self.resources.len()) =>
            {
                Ok(())
            }
            CapabilityContext::Command(_) | CapabilityContext::Resource(_) => Err(
                SceneError::for_code(SceneErrorCode::InvalidCommandSequence, None),
            ),
        }
    }

    fn validate_dependencies(
        &self,
        next_id: CapabilityRequirementId,
        dependencies: &[CapabilityRequirementId],
    ) -> Result<(), SceneError> {
        let mut previous = None;
        for dependency in dependencies.iter().copied() {
            if dependency.value() >= next_id.value()
                || usize::try_from(dependency.value())
                    .ok()
                    .is_none_or(|value| value >= self.requirements.len())
                || previous.is_some_and(|previous: CapabilityRequirementId| {
                    previous.value() >= dependency.value()
                })
            {
                return Err(SceneError::for_code(
                    SceneErrorCode::InvalidCommandSequence,
                    None,
                ));
            }
            previous = Some(dependency);
        }
        Ok(())
    }

    fn ensure_requirement_capacity(
        &self,
        additional: usize,
        additional_dependencies: usize,
    ) -> Result<(), SceneError> {
        ensure_total(
            SceneLimitKind::GraphicsRequirements,
            u64::from(self.limits.max_requirements()),
            u64::try_from(self.requirements.len()).map_err(|_| internal())?,
            u64::try_from(additional).map_err(|_| internal())?,
        )?;
        ensure_total(
            SceneLimitKind::GraphicsDependencies,
            u64::from(self.limits.max_dependencies()),
            u64::from(self.dependency_count),
            u64::try_from(additional_dependencies).map_err(|_| internal())?,
        )
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple keeps three independently committed Scene tables atomic"
    )]
    fn prepare_storage(
        &self,
        additional_resources: usize,
        additional_commands: usize,
        additional_requirements: usize,
        additional_nested: u64,
        transaction_transient: u64,
    ) -> Result<
        (
            Option<Vec<GraphicsResourceEntry>>,
            Option<Vec<GraphicsCommandRecord>>,
            Option<Vec<CapabilityRequirement>>,
            u64,
        ),
        SceneError,
    > {
        let geometric = self.prepare_storage_mode(
            additional_resources,
            additional_commands,
            additional_requirements,
            additional_nested,
            transaction_transient,
            true,
        )?;
        if geometric.3 <= self.limits.max_retained_bytes()
            && geometric.4 <= self.limits.max_retained_bytes()
        {
            return Ok((geometric.0, geometric.1, geometric.2, geometric.3));
        }
        drop(geometric);
        let exact = self.prepare_storage_mode(
            additional_resources,
            additional_commands,
            additional_requirements,
            additional_nested,
            transaction_transient,
            false,
        )?;
        if exact.3 <= self.limits.max_retained_bytes()
            && exact.4 <= self.limits.max_retained_bytes()
        {
            return Ok((exact.0, exact.1, exact.2, exact.3));
        }
        let consumed = self.retained_bytes()?;
        let required = exact.3.max(exact.4);
        Err(limit(
            SceneLimitKind::RetainedBytes,
            self.limits.max_retained_bytes(),
            consumed,
            required.saturating_sub(consumed),
        ))
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple keeps three independently committed Scene tables atomic"
    )]
    fn prepare_storage_mode(
        &self,
        additional_resources: usize,
        additional_commands: usize,
        additional_requirements: usize,
        additional_nested: u64,
        transaction_transient: u64,
        geometric: bool,
    ) -> Result<
        (
            Option<Vec<GraphicsResourceEntry>>,
            Option<Vec<GraphicsCommandRecord>>,
            Option<Vec<CapabilityRequirement>>,
            u64,
            u64,
        ),
        SceneError,
    > {
        let resource_capacity = planned_capacity(
            &self.resources,
            additional_resources,
            usize::try_from(self.limits.max_resources()).map_err(|_| internal())?,
            geometric,
            self.limits.max_retained_bytes(),
        )?;
        let command_capacity = planned_capacity(
            &self.commands,
            additional_commands,
            usize::try_from(self.limits.max_commands()).map_err(|_| internal())?,
            geometric,
            self.limits.max_retained_bytes(),
        )?;
        let requirement_capacity = planned_capacity(
            &self.requirements,
            additional_requirements,
            usize::try_from(self.limits.max_requirements()).map_err(|_| internal())?,
            geometric,
            self.limits.max_retained_bytes(),
        )?;
        let nested = self
            .nested_retained_bytes
            .checked_add(additional_nested)
            .ok_or_else(internal)?;
        let planned_retained = retained_for_capacities(
            resource_capacity,
            command_capacity,
            requirement_capacity,
            nested,
        )?;
        let persistent_retained = self.retained_bytes()?;
        let planned_replacements = replacement_capacity_bytes(&self.resources, resource_capacity)?
            .checked_add(replacement_capacity_bytes(
                &self.commands,
                command_capacity,
            )?)
            .and_then(|value| {
                value.checked_add(
                    replacement_capacity_bytes(&self.requirements, requirement_capacity).ok()?,
                )
            })
            .ok_or_else(internal)?;
        let planned_live_retained = persistent_retained
            .checked_add(transaction_transient)
            .and_then(|value| value.checked_add(planned_replacements))
            .ok_or_else(internal)?;
        if planned_retained > self.limits.max_retained_bytes()
            || planned_live_retained > self.limits.max_retained_bytes()
        {
            return Ok((None, None, None, planned_retained, planned_live_retained));
        }
        let resources = prepare_replacement(
            &self.resources,
            resource_capacity,
            self.limits.max_retained_bytes(),
        )?;
        let commands = prepare_replacement(
            &self.commands,
            command_capacity,
            self.limits.max_retained_bytes(),
        )?;
        let requirements = prepare_replacement(
            &self.requirements,
            requirement_capacity,
            self.limits.max_retained_bytes(),
        )?;
        let retained = retained_for_capacities(
            resources
                .as_ref()
                .map_or(self.resources.capacity(), Vec::capacity),
            commands
                .as_ref()
                .map_or(self.commands.capacity(), Vec::capacity),
            requirements
                .as_ref()
                .map_or(self.requirements.capacity(), Vec::capacity),
            nested,
        )?;
        let replacement_retained = option_capacity_bytes(&resources)?
            .checked_add(option_capacity_bytes(&commands)?)
            .and_then(|value| value.checked_add(option_capacity_bytes(&requirements).ok()?))
            .ok_or_else(internal)?;
        let live_retained = persistent_retained
            .checked_add(transaction_transient)
            .and_then(|value| value.checked_add(replacement_retained))
            .ok_or_else(internal)?;
        Ok((resources, commands, requirements, retained, live_retained))
    }

    fn charge_resource_comparison(
        &mut self,
        attempted: u64,
        command_index: u32,
    ) -> Result<(), SceneError> {
        let prospective = self
            .resource_index_work
            .checked_add(attempted)
            .ok_or_else(internal)?;
        if prospective > self.limits.max_resource_index_work() {
            return Err(SceneError::resource(
                SceneLimitKind::ResourceIndexWork,
                self.limits.max_resource_index_work(),
                self.resource_index_work,
                attempted,
                Some(command_index),
            ));
        }
        self.resource_index_work = prospective;
        Ok(())
    }

    fn next_command_index(&self) -> Result<u32, SceneError> {
        let value = u32::try_from(self.commands.len()).map_err(|_| internal())?;
        if value >= self.limits.max_commands() {
            return Err(limit(
                SceneLimitKind::GraphicsCommands,
                u64::from(self.limits.max_commands()),
                u64::from(value),
                1,
            ));
        }
        Ok(value)
    }

    fn next_requirement_id(&self) -> Result<CapabilityRequirementId, SceneError> {
        Ok(CapabilityRequirementId::new(
            u32::try_from(self.requirements.len()).map_err(|_| internal())?,
        ))
    }
}

fn imported_resource(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<&GraphicsResource, SceneError> {
    let entry = usize::try_from(id.value())
        .ok()
        .and_then(|index| graphics.resources().get(index))
        .filter(|entry| entry.id() == id)
        .ok_or_else(internal)?;
    Ok(entry.resource())
}

fn imported_path(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<PathResource, SceneError> {
    let GraphicsResource::Path(path) = imported_resource(graphics, id)? else {
        return Err(internal());
    };
    Ok(path.clone())
}

fn imported_image(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<ImageResource, SceneError> {
    let GraphicsResource::Image(image) = imported_resource(graphics, id)? else {
        return Err(internal());
    };
    Ok(image.clone())
}

fn imported_glyph(
    graphics: &GraphicsScene,
    id: GraphicsResourceId,
) -> Result<crate::GlyphOutline, SceneError> {
    let GraphicsResource::GlyphOutline(glyph) = imported_resource(graphics, id)? else {
        return Err(internal());
    };
    Ok(glyph.clone())
}

fn paint_capabilities(
    paint: Paint,
    primary: GraphicsCapability,
    parameter: u64,
) -> Vec<(GraphicsCapability, u64)> {
    let mut values = vec![
        (GraphicsCapability::DeviceColor, color_parameter(paint)),
        (primary, parameter),
    ];
    append_alpha_blend_capabilities(&mut values, paint.alpha(), paint.blend_mode());
    values
}

fn append_alpha_blend_capabilities(
    values: &mut Vec<(GraphicsCapability, u64)>,
    alpha: SceneUnit,
    blend_mode: BlendMode,
) {
    if alpha != SceneUnit::ONE {
        values.push((GraphicsCapability::ConstantAlpha, u64::from(alpha.get())));
    }
    if blend_mode != BlendMode::Normal {
        values.push((
            GraphicsCapability::Blend,
            match blend_mode {
                BlendMode::Normal => 0,
                BlendMode::Multiply => 1,
                BlendMode::Screen => 2,
            },
        ));
    }
}

fn color_parameter(paint: Paint) -> u64 {
    match paint.color() {
        crate::DeviceColor::Gray(_) => 1,
        crate::DeviceColor::Rgb { .. } => 3,
        crate::DeviceColor::Cmyk { .. } => 4,
    }
}

fn rule_parameter(rule: FillRule) -> u64 {
    u64::from(rule == FillRule::EvenOdd)
}

fn image_parameter(image: &ImageResource) -> u64 {
    u64::from(image.color_space().components())
        | (u64::from(image.bits_per_component()) << 8)
        | (u64::from(image.interpolate()) << 16)
}

fn validate_fill_bounds(
    path: &PathResource,
    transform: Matrix,
    bounds: SceneBounds,
) -> Result<(), SceneError> {
    if path.segments().is_empty() || bounds == SceneBounds::Page {
        return Ok(());
    }
    let mut minimum = None::<ScenePoint>;
    let mut maximum = None::<ScenePoint>;
    for segment in path.segments() {
        let points: &[ScenePoint] = match segment {
            crate::PathSegment::MoveTo(point) | crate::PathSegment::LineTo(point) => {
                std::slice::from_ref(point)
            }
            crate::PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                for point in [*control_1, *control_2, *end] {
                    include_bound_point(
                        transform.checked_transform_point(point)?,
                        &mut minimum,
                        &mut maximum,
                    );
                }
                continue;
            }
            crate::PathSegment::ClosePath => continue,
        };
        include_bound_point(
            transform.checked_transform_point(points[0])?,
            &mut minimum,
            &mut maximum,
        );
    }
    let (Some(required_minimum), Some(required_maximum)) = (minimum, maximum) else {
        return Ok(());
    };
    if let SceneBounds::Finite { minimum, maximum } = bounds
        && minimum.x() <= required_minimum.x()
        && minimum.y() <= required_minimum.y()
        && maximum.x() >= required_maximum.x()
        && maximum.y() >= required_maximum.y()
    {
        return Ok(());
    }
    Err(SceneError::for_code(SceneErrorCode::InvalidGeometry, None))
}

fn include_bound_point(
    point: ScenePoint,
    minimum: &mut Option<ScenePoint>,
    maximum: &mut Option<ScenePoint>,
) {
    let current_minimum = minimum.unwrap_or(point);
    let current_maximum = maximum.unwrap_or(point);
    *minimum = Some(ScenePoint::new(
        current_minimum.x().min(point.x()),
        current_minimum.y().min(point.y()),
    ));
    *maximum = Some(ScenePoint::new(
        current_maximum.x().max(point.x()),
        current_maximum.y().max(point.y()),
    ));
}

fn validate_nonempty_path_bounds(
    path: &PathResource,
    bounds: SceneBounds,
) -> Result<(), SceneError> {
    if path.segments().is_empty() || bounds == SceneBounds::Page {
        return Ok(());
    }
    Err(SceneError::for_code(SceneErrorCode::InvalidGeometry, None))
}

fn validate_nonempty_bounds(bounds: SceneBounds) -> Result<(), SceneError> {
    if bounds == SceneBounds::Page {
        return Ok(());
    }
    Err(SceneError::for_code(SceneErrorCode::InvalidGeometry, None))
}

fn resource_totals(resources: &[GraphicsResource]) -> Result<(u64, u64), SceneError> {
    let mut path_segments = 0_u64;
    let mut image_bytes = 0_u64;
    for resource in resources {
        match resource {
            GraphicsResource::Path(path) => {
                path_segments = path_segments
                    .checked_add(u64::try_from(path.segments().len()).map_err(|_| internal())?)
                    .ok_or_else(internal)?;
            }
            GraphicsResource::Image(image) => {
                image_bytes = image_bytes
                    .checked_add(u64::try_from(image.decoded().len()).map_err(|_| internal())?)
                    .ok_or_else(internal)?;
            }
            GraphicsResource::GlyphOutline(glyph) => {
                path_segments = path_segments
                    .checked_add(
                        u64::try_from(glyph.outline().segments().len()).map_err(|_| internal())?,
                    )
                    .ok_or_else(internal)?;
            }
        }
    }
    Ok((path_segments, image_bytes))
}

fn nested_retained_for_resources(resources: &[GraphicsResource]) -> Result<u64, SceneError> {
    let mut retained = 0_u64;
    for resource in resources {
        retained = retained
            .checked_add(resource.retained_bytes()?)
            .ok_or_else(internal)?;
    }
    Ok(retained)
}

fn nested_retained_for_requirements(
    requirements: &[CapabilityRequirement],
) -> Result<u64, SceneError> {
    let mut retained = 0_u64;
    for requirement in requirements {
        retained = retained
            .checked_add(requirement.retained_bytes()?)
            .ok_or_else(internal)?;
    }
    Ok(retained)
}

fn unique_capability_count(capabilities: &[(GraphicsCapability, u64)]) -> usize {
    capabilities
        .iter()
        .enumerate()
        .filter(|(index, capability)| !capabilities[..*index].contains(capability))
        .count()
}

fn retained_for_capacities(
    resources: usize,
    commands: usize,
    requirements: usize,
    nested: u64,
) -> Result<u64, SceneError> {
    capacity_bytes::<GraphicsResourceEntry>(resources)?
        .checked_add(capacity_bytes::<GraphicsCommandRecord>(commands)?)
        .and_then(|value| {
            value.checked_add(capacity_bytes::<CapabilityRequirement>(requirements).ok()?)
        })
        .and_then(|value| value.checked_add(nested))
        .ok_or_else(internal)
}

fn capacity_bytes<T>(count: usize) -> Result<u64, SceneError> {
    u64::try_from(count)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(internal)
}

fn planned_capacity<T>(
    values: &Vec<T>,
    additional: usize,
    maximum: usize,
    geometric: bool,
    allocation_limit: u64,
) -> Result<usize, SceneError> {
    let target = values.len().checked_add(additional).ok_or_else(internal)?;
    if target <= values.capacity() {
        return Ok(values.capacity());
    }
    let doubled = values.capacity().max(1).checked_mul(2).unwrap_or(maximum);
    let requested = if geometric {
        target.max(doubled).min(maximum)
    } else {
        target
    };
    let requested = if geometric && capacity_bytes::<T>(requested)? > allocation_limit {
        target
    } else {
        requested
    };
    Ok(requested)
}

fn prepare_replacement<T>(
    values: &Vec<T>,
    requested: usize,
    allocation_limit: u64,
) -> Result<Option<Vec<T>>, SceneError> {
    if requested <= values.capacity() {
        return Ok(None);
    }
    let mut replacement = Vec::new();
    replacement
        .try_reserve_exact(requested)
        .map_err(|_| allocation(allocation_limit))?;
    Ok(Some(replacement))
}

fn replacement_capacity_bytes<T>(values: &Vec<T>, requested: usize) -> Result<u64, SceneError> {
    if requested <= values.capacity() {
        Ok(0)
    } else {
        capacity_bytes::<T>(requested)
    }
}

fn option_capacity_bytes<T>(values: &Option<Vec<T>>) -> Result<u64, SceneError> {
    values
        .as_ref()
        .map_or(Ok(0), |values| capacity_bytes::<T>(values.capacity()))
}

fn install_replacement<T>(values: &mut Vec<T>, replacement: Option<Vec<T>>) {
    if let Some(mut replacement) = replacement {
        replacement.append(values);
        *values = replacement;
    }
}

fn ensure_total(
    kind: SceneLimitKind,
    maximum: u64,
    consumed: u64,
    attempted: u64,
) -> Result<(), SceneError> {
    if consumed
        .checked_add(attempted)
        .is_none_or(|value| value > maximum)
    {
        return Err(limit(kind, maximum, consumed, attempted));
    }
    Ok(())
}

fn limit(kind: SceneLimitKind, maximum: u64, consumed: u64, attempted: u64) -> SceneError {
    SceneError::resource(kind, maximum, consumed, attempted, None)
}

fn allocation(limit_value: u64) -> SceneError {
    limit(SceneLimitKind::Allocation, limit_value, 0, 1)
}

fn internal() -> SceneError {
    SceneError::for_code(SceneErrorCode::InternalState, None)
}
