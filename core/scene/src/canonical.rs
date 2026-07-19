use pdf_rs_syntax::ObjectRef;

use crate::{
    BlendMode, CapabilityContext, CapabilityDecision, CapabilityStatus, CommandSource, DeviceColor,
    FillRule, GlyphPainting, GraphicsCapability, GraphicsCommand, GraphicsCommandRecord,
    GraphicsResource, GraphicsResourceSource, GraphicsScene, ImageColorSpace, LineCap, LineJoin,
    LineStyle, Matrix, PageGeometry, PathResource, PathSegment, Scene, SceneBounds,
    SceneCanonicalObserver, SceneCommand, SceneCommandKind, SceneError, SceneErrorCode,
    SceneFeature, SceneLimitKind, ScenePoint, SceneRect, SceneResource, SceneResourceKind,
};

impl Scene {
    /// Serializes this Scene into compact deterministic schema-1 JSON bytes.
    ///
    /// Object fields use fixed lexical order, semantic arrays retain their declared order, PDF
    /// name bytes use lowercase hexadecimal, and numeric values use scaled integers. Runtime
    /// [`pdf_rs_bytes::SourceIdentity`] is deliberately omitted.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, SceneError> {
        self.canonical_json_bytes_impl(None)
    }

    /// Serializes canonical JSON while allowing a caller-owned observer to interrupt bounded work.
    ///
    /// Successful output is byte-identical to [`Self::canonical_json_bytes`]. If the observer
    /// returns `false`, no byte vector is published.
    pub fn canonical_json_bytes_observed(
        &self,
        observer: &mut dyn SceneCanonicalObserver,
    ) -> Result<Vec<u8>, SceneError> {
        self.canonical_json_bytes_impl(Some(observer))
    }

    fn canonical_json_bytes_impl(
        &self,
        observer: Option<&mut dyn SceneCanonicalObserver>,
    ) -> Result<Vec<u8>, SceneError> {
        if let Some(graphics) = self.graphics() {
            return canonical_graphics_json_bytes(self, graphics, observer);
        }
        if self.commands().len() != self.provenance().len() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidProvenance,
                None,
            ));
        }
        let mut writer = CanonicalWriter::new(
            self.limits().max_canonical_bytes(),
            SceneLimitKind::CanonicalBytes,
            observer,
        );
        writer.push(b"{\"binding\":{\"page_index\":")?;
        writer.push_u32(self.binding().page_index())?;
        writer.push(b",\"page_object\":")?;
        write_object_ref(&mut writer, self.binding().page_object())?;
        writer.push(b",\"revision_startxref\":")?;
        writer.push_u64(self.binding().revision_startxref())?;
        writer.push(b"},\"commands\":[")?;
        for (index, command) in self.commands().iter().enumerate() {
            writer.separator(index)?;
            write_command(&mut writer, command)?;
        }
        writer.push(b"],\"features\":{\"decision\":")?;
        match self.features().decision() {
            CapabilityDecision::Supported => writer.push(b"\"supported\"")?,
            CapabilityDecision::Unsupported => writer.push(b"\"unsupported\"")?,
        }
        writer.push(b",\"tags\":[")?;
        for (index, feature) in self.features().tags().iter().copied().enumerate() {
            writer.separator(index)?;
            writer.push(match feature {
                SceneFeature::MarkedContent => b"\"marked-content\"",
                SceneFeature::MarkedContentProperties => b"\"marked-content-properties\"",
            })?;
        }
        writer.push(b"]},\"geometry\":")?;
        write_geometry(&mut writer, self.geometry())?;
        writer.push(b",\"provenance\":[")?;
        for (index, source) in self.provenance().iter().copied().enumerate() {
            writer.separator(index)?;
            write_source(&mut writer, source)?;
        }
        writer.push(b"],\"resources\":[")?;
        for (index, resource) in self.resources().iter().copied().enumerate() {
            writer.separator(index)?;
            write_resource(&mut writer, resource)?;
        }
        writer.push(b"],\"schema\":{\"major\":")?;
        writer.push_u16(self.version().major())?;
        writer.push(b",\"minor\":")?;
        writer.push_u16(self.version().minor())?;
        writer.push(b"}}")?;
        Ok(writer.finish())
    }
}

fn canonical_graphics_json_bytes(
    scene: &Scene,
    graphics: &GraphicsScene,
    observer: Option<&mut dyn SceneCanonicalObserver>,
) -> Result<Vec<u8>, SceneError> {
    let mut writer = CanonicalWriter::new(
        graphics.limits().max_canonical_bytes(),
        SceneLimitKind::CanonicalBytes,
        observer,
    );
    writer.push(b"{\"binding\":{\"page_index\":")?;
    writer.push_u32(scene.binding().page_index())?;
    writer.push(b",\"page_object\":")?;
    write_object_ref(&mut writer, scene.binding().page_object())?;
    writer.push(b",\"revision_startxref\":")?;
    writer.push_u64(scene.binding().revision_startxref())?;
    writer.push(b"},\"commands\":[")?;
    for (index, record) in graphics.commands().iter().enumerate() {
        writer.separator(index)?;
        write_graphics_command_record(&mut writer, record)?;
    }
    writer.push(b"],\"geometry\":")?;
    write_geometry(&mut writer, scene.geometry())?;
    writer.push(b",\"requirements\":[")?;
    for (index, requirement) in graphics.requirements().iter().enumerate() {
        writer.separator(index)?;
        writer.push(b"{\"capability\":")?;
        writer.push(graphics_capability_label(requirement.capability()))?;
        writer.push(b",\"context\":")?;
        write_capability_context(&mut writer, requirement.context())?;
        writer.push(b",\"dependencies\":[")?;
        for (dependency_index, dependency) in requirement.dependencies().iter().enumerate() {
            writer.separator(dependency_index)?;
            writer.push_u32(dependency.value())?;
        }
        writer.push(b"],\"id\":")?;
        writer.push_u32(requirement.id().value())?;
        writer.push(b",\"parameter\":")?;
        writer.push_u64(requirement.parameter())?;
        writer.push(b",\"status\":")?;
        writer.push(match requirement.status() {
            CapabilityStatus::Supported => b"\"supported\"",
            CapabilityStatus::Unsupported => b"\"unsupported\"",
        })?;
        writer.push(b"}")?;
    }
    writer.push(b"],\"resources\":[")?;
    for (index, entry) in graphics.resources().iter().enumerate() {
        writer.separator(index)?;
        writer.push(b"{\"id\":")?;
        writer.push_u32(entry.id().value())?;
        writer.push(b",\"resource\":")?;
        write_graphics_resource(&mut writer, entry.resource())?;
        writer.push(b"}")?;
    }
    writer.push(b"],\"schema\":{\"major\":")?;
    writer.push_u16(scene.version().major())?;
    writer.push(b",\"minor\":")?;
    writer.push_u16(scene.version().minor())?;
    writer.push(b"}}")?;
    Ok(writer.finish())
}

fn write_graphics_command_record(
    writer: &mut CanonicalWriter<'_>,
    record: &GraphicsCommandRecord,
) -> Result<(), SceneError> {
    writer.push(b"{\"bounds\":")?;
    write_bounds(writer, record.bounds())?;
    writer.push(b",\"command\":")?;
    write_graphics_command(writer, record.command())?;
    writer.push(b",\"source\":")?;
    write_source(writer, record.source())?;
    writer.push(b"}")
}

fn write_graphics_command(
    writer: &mut CanonicalWriter<'_>,
    command: &GraphicsCommand,
) -> Result<(), SceneError> {
    match command {
        GraphicsCommand::Save => writer.push(b"{\"kind\":\"save\"}"),
        GraphicsCommand::Restore => writer.push(b"{\"kind\":\"restore\"}"),
        GraphicsCommand::Clip {
            path,
            rule,
            transform,
        } => {
            writer.push(b"{\"kind\":\"clip\",\"path\":")?;
            writer.push_u32(path.value())?;
            writer.push(b",\"rule\":")?;
            writer.push(fill_rule_label(*rule))?;
            writer.push(b",\"transform\":")?;
            write_matrix(writer, *transform)?;
            writer.push(b"}")
        }
        GraphicsCommand::Fill {
            path,
            rule,
            paint,
            transform,
        } => {
            writer.push(b"{\"kind\":\"fill\",\"paint\":")?;
            write_paint(writer, *paint)?;
            writer.push(b",\"path\":")?;
            writer.push_u32(path.value())?;
            writer.push(b",\"rule\":")?;
            writer.push(fill_rule_label(*rule))?;
            writer.push(b",\"transform\":")?;
            write_matrix(writer, *transform)?;
            writer.push(b"}")
        }
        GraphicsCommand::Stroke {
            path,
            paint,
            style,
            transform,
        } => {
            writer.push(b"{\"kind\":\"stroke\",\"paint\":")?;
            write_paint(writer, *paint)?;
            writer.push(b",\"path\":")?;
            writer.push_u32(path.value())?;
            writer.push(b",\"style\":")?;
            write_line_style(writer, style)?;
            writer.push(b",\"transform\":")?;
            write_matrix(writer, *transform)?;
            writer.push(b"}")
        }
        GraphicsCommand::FillStroke {
            path,
            rule,
            fill,
            stroke,
            style,
            transform,
        } => {
            writer.push(b"{\"kind\":\"fill-stroke\",\"fill\":")?;
            write_paint(writer, *fill)?;
            writer.push(b",\"path\":")?;
            writer.push_u32(path.value())?;
            writer.push(b",\"rule\":")?;
            writer.push(fill_rule_label(*rule))?;
            writer.push(b",\"stroke\":")?;
            write_paint(writer, *stroke)?;
            writer.push(b",\"style\":")?;
            write_line_style(writer, style)?;
            writer.push(b",\"transform\":")?;
            write_matrix(writer, *transform)?;
            writer.push(b"}")
        }
        GraphicsCommand::DrawImage {
            image,
            transform,
            alpha,
            blend_mode,
        } => {
            writer.push(b"{\"kind\":\"draw-image\",\"alpha\":")?;
            writer.push_u16(alpha.get())?;
            writer.push(b",\"blend_mode\":")?;
            writer.push(blend_mode_label(*blend_mode))?;
            writer.push(b",\"image\":")?;
            writer.push_u32(image.value())?;
            writer.push(b",\"transform\":")?;
            write_matrix(writer, *transform)?;
            writer.push(b"}")
        }
        GraphicsCommand::DrawGlyphRun(run) => {
            writer.push(b"{\"kind\":\"draw-glyph-run\",\"glyphs\":[")?;
            for (index, glyph) in run.glyphs().iter().enumerate() {
                writer.separator(index)?;
                writer.push(b"{\"character_code\":")?;
                writer.push_u32(glyph.character_code())?;
                writer.push(b",\"outline\":")?;
                writer.push_u32(glyph.outline().value())?;
                writer.push(b",\"transform\":")?;
                write_matrix(writer, glyph.transform())?;
                writer.push(b"}")?;
            }
            match run.painting() {
                GlyphPainting::Fill(paint) => {
                    writer.push(b"],\"paint\":")?;
                    write_paint(writer, *paint)?;
                }
                GlyphPainting::Stroke { paint, style } => {
                    writer.push(b"],\"painting\":\"stroke\",\"paint\":")?;
                    write_paint(writer, *paint)?;
                    writer.push(b",\"style\":")?;
                    write_line_style(writer, style)?;
                }
                GlyphPainting::FillStroke {
                    fill,
                    stroke,
                    style,
                } => {
                    writer.push(b"],\"painting\":\"fill-stroke\",\"fill\":")?;
                    write_paint(writer, *fill)?;
                    writer.push(b",\"stroke\":")?;
                    write_paint(writer, *stroke)?;
                    writer.push(b",\"style\":")?;
                    write_line_style(writer, style)?;
                }
            }
            writer.push(b"}")
        }
        GraphicsCommand::BeginIsolatedGroup {
            alpha,
            blend_mode,
            knockout,
        } => {
            writer.push(if *knockout {
                b"{\"kind\":\"begin-knockout-group\",\"alpha\":"
            } else {
                b"{\"kind\":\"begin-isolated-group\",\"alpha\":"
            })?;
            writer.push_u16(alpha.get())?;
            writer.push(b",\"blend_mode\":")?;
            writer.push(blend_mode_label(*blend_mode))?;
            writer.push(b"}")
        }
        GraphicsCommand::EndIsolatedGroup => writer.push(b"{\"kind\":\"end-isolated-group\"}"),
    }
}

fn write_graphics_resource(
    writer: &mut CanonicalWriter<'_>,
    resource: &GraphicsResource,
) -> Result<(), SceneError> {
    match resource {
        GraphicsResource::Path(path) => {
            writer.push(b"{\"kind\":\"path\",\"segments\":")?;
            write_path(writer, path)?;
            writer.push(b"}")
        }
        GraphicsResource::Image(image) => {
            writer.push(b"{\"kind\":\"image\",\"bits_per_component\":")?;
            writer.push_u16(u16::from(image.bits_per_component()))?;
            writer.push(b",\"color_space\":")?;
            writer.push(match image.color_space() {
                ImageColorSpace::DeviceGray => b"\"device-gray\"",
                ImageColorSpace::DeviceRgb => b"\"device-rgb\"",
                ImageColorSpace::DeviceCmyk => b"\"device-cmyk\"",
            })?;
            writer.push(b",\"decoded_hex\":\"")?;
            writer.push_hex(image.decoded())?;
            writer.push(b"\"")?;
            if let Some(mask) = image.soft_mask() {
                writer.push(b",\"soft_mask_hex\":\"")?;
                writer.push_hex(mask)?;
                writer.push(b"\"")?;
            }
            writer.push(b",\"height\":")?;
            writer.push_u32(image.height())?;
            writer.push(b",\"interpolate\":")?;
            write_bool(writer, image.interpolate())?;
            writer.push(b",\"source\":")?;
            write_graphics_resource_source(writer, image.source())?;
            writer.push(b",\"width\":")?;
            writer.push_u32(image.width())?;
            writer.push(b"}")
        }
        GraphicsResource::GlyphOutline(glyph) => {
            writer.push(b"{\"kind\":\"glyph-outline\",\"glyph_id\":")?;
            writer.push_u32(glyph.glyph_id())?;
            writer.push(b",\"outline\":")?;
            write_path(writer, glyph.outline())?;
            writer.push(b",\"source\":")?;
            write_graphics_resource_source(writer, glyph.source())?;
            writer.push(b",\"units_per_em\":")?;
            writer.push_u16(glyph.units_per_em())?;
            writer.push(b"}")
        }
    }
}

fn write_graphics_resource_source(
    writer: &mut CanonicalWriter<'_>,
    source: GraphicsResourceSource,
) -> Result<(), SceneError> {
    writer.push(b"{\"decode_context\":")?;
    writer.push_u64(source.decode_context())?;
    writer.push(b",\"object\":")?;
    write_object_ref(writer, source.object())?;
    writer.push(b",\"revision_startxref\":")?;
    writer.push_u64(source.revision_startxref())?;
    writer.push(b"}")
}

fn write_path(writer: &mut CanonicalWriter<'_>, path: &PathResource) -> Result<(), SceneError> {
    writer.push(b"[")?;
    for (index, segment) in path.segments().iter().enumerate() {
        writer.separator(index)?;
        match segment {
            PathSegment::MoveTo(point) => {
                writer.push(b"{\"kind\":\"move-to\",\"point\":")?;
                write_point(writer, *point)?;
                writer.push(b"}")?;
            }
            PathSegment::LineTo(point) => {
                writer.push(b"{\"kind\":\"line-to\",\"point\":")?;
                write_point(writer, *point)?;
                writer.push(b"}")?;
            }
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                writer.push(b"{\"kind\":\"cubic-to\",\"control_1\":")?;
                write_point(writer, *control_1)?;
                writer.push(b",\"control_2\":")?;
                write_point(writer, *control_2)?;
                writer.push(b",\"end\":")?;
                write_point(writer, *end)?;
                writer.push(b"}")?;
            }
            PathSegment::ClosePath => writer.push(b"{\"kind\":\"close-path\"}")?,
        }
    }
    writer.push(b"]")
}

fn write_bounds(writer: &mut CanonicalWriter<'_>, bounds: SceneBounds) -> Result<(), SceneError> {
    match bounds {
        SceneBounds::Empty => writer.push(b"\"empty\""),
        SceneBounds::Page => writer.push(b"\"page\""),
        SceneBounds::Finite { minimum, maximum } => {
            writer.push(b"[")?;
            writer.push_i64(minimum.x().scaled())?;
            writer.push(b",")?;
            writer.push_i64(minimum.y().scaled())?;
            writer.push(b",")?;
            writer.push_i64(maximum.x().scaled())?;
            writer.push(b",")?;
            writer.push_i64(maximum.y().scaled())?;
            writer.push(b"]")
        }
    }
}

fn write_point(writer: &mut CanonicalWriter<'_>, point: ScenePoint) -> Result<(), SceneError> {
    writer.push(b"[")?;
    writer.push_i64(point.x().scaled())?;
    writer.push(b",")?;
    writer.push_i64(point.y().scaled())?;
    writer.push(b"]")
}

fn write_matrix(writer: &mut CanonicalWriter<'_>, matrix: Matrix) -> Result<(), SceneError> {
    writer.push(b"[")?;
    for (index, value) in matrix.components().iter().copied().enumerate() {
        writer.separator(index)?;
        writer.push_i64(value.scaled())?;
    }
    writer.push(b"]")
}

fn write_paint(writer: &mut CanonicalWriter<'_>, paint: crate::Paint) -> Result<(), SceneError> {
    writer.push(b"{\"alpha\":")?;
    writer.push_u16(paint.alpha().get())?;
    writer.push(b",\"blend_mode\":")?;
    writer.push(blend_mode_label(paint.blend_mode()))?;
    writer.push(b",\"color\":")?;
    match paint.color() {
        DeviceColor::Gray(gray) => {
            writer.push(b"{\"components\":[")?;
            writer.push_u16(gray.get())?;
            writer.push(b"],\"space\":\"device-gray\"}")?;
        }
        DeviceColor::Rgb { red, green, blue } => {
            writer.push(b"{\"components\":[")?;
            writer.push_u16(red.get())?;
            writer.push(b",")?;
            writer.push_u16(green.get())?;
            writer.push(b",")?;
            writer.push_u16(blue.get())?;
            writer.push(b"],\"space\":\"device-rgb\"}")?;
        }
        DeviceColor::Cmyk {
            cyan,
            magenta,
            yellow,
            black,
        } => {
            writer.push(b"{\"components\":[")?;
            writer.push_u16(cyan.get())?;
            writer.push(b",")?;
            writer.push_u16(magenta.get())?;
            writer.push(b",")?;
            writer.push_u16(yellow.get())?;
            writer.push(b",")?;
            writer.push_u16(black.get())?;
            writer.push(b"],\"space\":\"device-cmyk\"}")?;
        }
    }
    writer.push(b"}")
}

fn write_line_style(writer: &mut CanonicalWriter<'_>, style: &LineStyle) -> Result<(), SceneError> {
    writer.push(b"{\"cap\":")?;
    writer.push(match style.cap() {
        LineCap::Butt => b"\"butt\"",
        LineCap::Round => b"\"round\"",
        LineCap::Square => b"\"square\"",
    })?;
    writer.push(b",\"dash\":{\"array\":[")?;
    for (index, value) in style.dash().array().iter().copied().enumerate() {
        writer.separator(index)?;
        writer.push_i64(value.scaled())?;
    }
    writer.push(b"],\"phase\":")?;
    writer.push_i64(style.dash().phase().scaled())?;
    writer.push(b"},\"join\":")?;
    writer.push(match style.join() {
        LineJoin::Miter => b"\"miter\"",
        LineJoin::Round => b"\"round\"",
        LineJoin::Bevel => b"\"bevel\"",
    })?;
    writer.push(b",\"miter_limit\":")?;
    writer.push_i64(style.miter_limit().scaled())?;
    writer.push(b",\"stroke_transform\":")?;
    write_matrix(writer, style.stroke_transform())?;
    writer.push(b",\"width\":")?;
    writer.push_i64(style.width().scaled())?;
    writer.push(b"}")
}

fn write_capability_context(
    writer: &mut CanonicalWriter<'_>,
    context: CapabilityContext,
) -> Result<(), SceneError> {
    match context {
        CapabilityContext::Scene => writer.push(b"{\"kind\":\"scene\"}"),
        CapabilityContext::Command(index) => {
            writer.push(b"{\"kind\":\"command\",\"value\":")?;
            writer.push_u32(index)?;
            writer.push(b"}")
        }
        CapabilityContext::Resource(id) => {
            writer.push(b"{\"kind\":\"resource\",\"value\":")?;
            writer.push_u32(id.value())?;
            writer.push(b"}")
        }
    }
}

fn graphics_capability_label(capability: GraphicsCapability) -> &'static [u8] {
    match capability {
        GraphicsCapability::PathFill => b"\"path-fill\"",
        GraphicsCapability::PathStroke => b"\"path-stroke\"",
        GraphicsCapability::Clip => b"\"clip\"",
        GraphicsCapability::DeviceColor => b"\"device-color\"",
        GraphicsCapability::ConstantAlpha => b"\"constant-alpha\"",
        GraphicsCapability::Blend => b"\"blend\"",
        GraphicsCapability::SoftMask => b"\"soft-mask\"",
        GraphicsCapability::Image => b"\"image\"",
        GraphicsCapability::Glyph => b"\"glyph\"",
        GraphicsCapability::IsolatedGroup => b"\"isolated-group\"",
        GraphicsCapability::KnockoutGroup => b"\"knockout-group\"",
    }
}

fn fill_rule_label(rule: FillRule) -> &'static [u8] {
    match rule {
        FillRule::Nonzero => b"\"nonzero\"",
        FillRule::EvenOdd => b"\"even-odd\"",
    }
}

fn blend_mode_label(mode: BlendMode) -> &'static [u8] {
    match mode {
        BlendMode::Normal => b"\"normal\"",
        BlendMode::Multiply => b"\"multiply\"",
        BlendMode::Screen => b"\"screen\"",
    }
}

fn write_bool(writer: &mut CanonicalWriter<'_>, value: bool) -> Result<(), SceneError> {
    writer.push(if value { b"true" } else { b"false" })
}

fn write_command(
    writer: &mut CanonicalWriter<'_>,
    command: &SceneCommand,
) -> Result<(), SceneError> {
    match command.kind() {
        SceneCommandKind::BeginMarkedContent => {
            let tag = command
                .tag()
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
            writer.push(b"{\"kind\":\"begin-marked-content\",\"properties\":")?;
            if let Some(resource) = command.properties() {
                writer.push_u32(resource.value())?;
            } else {
                writer.push(b"null")?;
            }
            writer.push(b",\"tag_hex\":\"")?;
            writer.push_hex(tag.bytes())?;
            writer.push(b"\"}")
        }
        SceneCommandKind::EndMarkedContent => writer.push(b"{\"kind\":\"end-marked-content\"}"),
    }
}

fn write_geometry(
    writer: &mut CanonicalWriter<'_>,
    geometry: PageGeometry,
) -> Result<(), SceneError> {
    writer.push(b"{\"crop_box\":")?;
    write_rect(writer, geometry.crop_box())?;
    writer.push(b",\"media_box\":")?;
    write_rect(writer, geometry.media_box())?;
    writer.push(b",\"rotation\":")?;
    writer.push_u16(geometry.rotation().degrees())?;
    writer.push(b"}")
}

fn write_rect(writer: &mut CanonicalWriter<'_>, rect: SceneRect) -> Result<(), SceneError> {
    writer.push(b"[")?;
    for (index, value) in rect.coordinates().iter().copied().enumerate() {
        writer.separator(index)?;
        writer.push_i64(value.scaled())?;
    }
    writer.push(b"]")
}

fn write_source(writer: &mut CanonicalWriter<'_>, source: CommandSource) -> Result<(), SceneError> {
    writer.push(b"{\"decoded_length\":")?;
    writer.push_u64(source.decoded_length())?;
    writer.push(b",\"decoded_start\":")?;
    writer.push_u64(source.decoded_start())?;
    writer.push(b",\"object\":")?;
    write_object_ref(writer, source.object())?;
    writer.push(b",\"operator_index\":")?;
    writer.push_u32(source.operator_index())?;
    writer.push(b",\"stream_index\":")?;
    writer.push_u32(source.stream_index())?;
    writer.push(b"}")
}

fn write_resource(
    writer: &mut CanonicalWriter<'_>,
    resource: SceneResource,
) -> Result<(), SceneError> {
    writer.push(b"{\"id\":")?;
    writer.push_u32(resource.id().value())?;
    writer.push(b",\"kind\":")?;
    match resource.kind() {
        SceneResourceKind::MarkedContentProperties => {
            writer.push(b"\"marked-content-properties\"")?;
        }
    }
    writer.push(b",\"object\":")?;
    write_object_ref(writer, resource.object())?;
    writer.push(b"}")
}

fn write_object_ref(
    writer: &mut CanonicalWriter<'_>,
    reference: ObjectRef,
) -> Result<(), SceneError> {
    writer.push(b"{\"generation\":")?;
    writer.push_u16(reference.generation())?;
    writer.push(b",\"number\":")?;
    writer.push_u32(reference.number())?;
    writer.push(b"}")
}

pub(crate) struct CanonicalWriter<'a> {
    bytes: Vec<u8>,
    limit: u64,
    limit_kind: SceneLimitKind,
    observer: Option<&'a mut dyn SceneCanonicalObserver>,
}

impl<'a> CanonicalWriter<'a> {
    pub(crate) const fn new(
        limit: u64,
        limit_kind: SceneLimitKind,
        observer: Option<&'a mut dyn SceneCanonicalObserver>,
    ) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_kind,
            observer,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<(), SceneError> {
        self.observe(bytes)?;
        self.reserve_output(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn observe(&mut self, bytes: &[u8]) -> Result<(), SceneError> {
        if self
            .observer
            .as_deref_mut()
            .is_some_and(|observer| !observer.observe(bytes))
        {
            return Err(SceneError::for_code(
                SceneErrorCode::CanonicalizationInterrupted,
                None,
            ));
        }
        Ok(())
    }

    fn reserve_output(&mut self, additional: usize) -> Result<(), SceneError> {
        let (consumed, attempted) = self.ensure_output_limit(additional)?;
        let required = self
            .bytes
            .len()
            .checked_add(additional)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        if required <= self.bytes.capacity() {
            return Ok(());
        }
        let limit = usize::try_from(self.limit)
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let grown = if self.bytes.capacity() == 0 {
            64
        } else {
            self.bytes.capacity().saturating_mul(2)
        };
        let target = grown.max(required).min(limit);
        let reserve = target
            .checked_sub(self.bytes.len())
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        self.bytes.try_reserve_exact(reserve).map_err(|_| {
            SceneError::resource(
                SceneLimitKind::Allocation,
                self.limit,
                consumed,
                attempted,
                None,
            )
        })?;
        Ok(())
    }

    fn ensure_output_limit(&self, additional: usize) -> Result<(u64, u64), SceneError> {
        let consumed = u64::try_from(self.bytes.len())
            .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let attempted = u64::try_from(additional).unwrap_or(u64::MAX);
        let next = consumed.checked_add(attempted).ok_or_else(|| {
            SceneError::resource(self.limit_kind, self.limit, consumed, attempted, None)
        })?;
        if next > self.limit {
            return Err(SceneError::resource(
                self.limit_kind,
                self.limit,
                consumed,
                attempted,
                None,
            ));
        }
        Ok((consumed, attempted))
    }

    pub(crate) fn separator(&mut self, index: usize) -> Result<(), SceneError> {
        if index != 0 {
            self.push(b",")?;
        }
        Ok(())
    }

    pub(crate) fn push_u16(&mut self, value: u16) -> Result<(), SceneError> {
        self.push_u64(u64::from(value))
    }

    pub(crate) fn push_u32(&mut self, value: u32) -> Result<(), SceneError> {
        self.push_u64(u64::from(value))
    }

    fn push_u64(&mut self, mut value: u64) -> Result<(), SceneError> {
        let mut buffer = [0_u8; 20];
        let mut index = buffer.len();
        loop {
            index -= 1;
            buffer[index] = b'0' + u8::try_from(value % 10).expect("one digit fits u8");
            value /= 10;
            if value == 0 {
                return self.push(&buffer[index..]);
            }
        }
    }

    fn push_i64(&mut self, value: i64) -> Result<(), SceneError> {
        if value < 0 {
            self.push(b"-")?;
            self.push_u64(value.unsigned_abs())
        } else {
            self.push_u64(value as u64)
        }
    }

    fn push_hex(&mut self, bytes: &[u8]) -> Result<(), SceneError> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        const INPUT_CHUNK_BYTES: usize = 256;
        let encoded_len = bytes.len().checked_mul(2).ok_or_else(|| {
            SceneError::resource(
                self.limit_kind,
                self.limit,
                u64::try_from(self.bytes.len()).unwrap_or(u64::MAX),
                u64::MAX,
                None,
            )
        })?;
        self.ensure_output_limit(encoded_len)?;
        for chunk in bytes.chunks(INPUT_CHUNK_BYTES) {
            let mut encoded = [0_u8; INPUT_CHUNK_BYTES * 2];
            for (target, byte) in encoded.chunks_exact_mut(2).zip(chunk) {
                target[0] = HEX[usize::from(byte >> 4)];
                target[1] = HEX[usize::from(byte & 0x0f)];
            }
            let chunk_encoded_len = chunk
                .len()
                .checked_mul(2)
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
            self.push(&encoded[..chunk_encoded_len])?;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}
