use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_syntax::ObjectRef;

use crate::{CommandSource, GraphicsSceneLimits, Matrix, SceneError, SceneErrorCode, SceneScalar};

/// One point in PDF user space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScenePoint {
    x: SceneScalar,
    y: SceneScalar,
}

impl ScenePoint {
    /// Creates one exact user-space point.
    pub const fn new(x: SceneScalar, y: SceneScalar) -> Self {
        Self { x, y }
    }

    /// Returns the horizontal coordinate.
    pub const fn x(self) -> SceneScalar {
        self.x
    }

    /// Returns the vertical coordinate.
    pub const fn y(self) -> SceneScalar {
        self.y
    }
}

/// Conservative command bounds retained by Scene v2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneBounds {
    /// The command is known to cover no page area.
    Empty,
    /// Finite inclusive user-space bounds.
    Finite {
        /// Lower-left bound.
        minimum: ScenePoint,
        /// Upper-right bound.
        maximum: ScenePoint,
    },
    /// Conservative fallback to the complete page crop.
    Page,
}

impl SceneBounds {
    /// Creates finite bounds, allowing zero-area bounds for degenerate geometry.
    pub fn finite(minimum: ScenePoint, maximum: ScenePoint) -> Result<Self, SceneError> {
        if maximum.x() < minimum.x() || maximum.y() < minimum.y() {
            return Err(SceneError::for_code(SceneErrorCode::InvalidGeometry, None));
        }
        Ok(Self::Finite { minimum, maximum })
    }
}

/// One exact path-construction segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathSegment {
    /// Starts a new subpath.
    MoveTo(ScenePoint),
    /// Appends one straight segment.
    LineTo(ScenePoint),
    /// Appends one cubic Bézier segment.
    CubicTo {
        /// First control point.
        control_1: ScenePoint,
        /// Second control point.
        control_2: ScenePoint,
        /// Segment endpoint.
        end: ScenePoint,
    },
    /// Closes the current subpath.
    ClosePath,
}

/// Immutable validated path geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathResource {
    segments: Arc<Vec<PathSegment>>,
}

impl PathResource {
    /// Validates and retains normalized exact construction segments.
    ///
    /// Producers must make PDF's implicit post-close subpath restart explicit with `MoveTo`.
    pub fn new(segments: Vec<PathSegment>) -> Result<Self, SceneError> {
        let mut builder = PathResourceBuilder::new();
        builder.try_reserve_exact(segments.len())?;
        for segment in segments {
            builder.try_push(segment)?;
        }
        Ok(builder.finish())
    }

    /// Borrows exact path segments.
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.segments)
    }
}

/// Incrementally validated path construction with an O(1), element-buffer-copy-free handoff.
///
/// Producers that already charge and validate each appended segment can use this builder to avoid
/// rescanning or copying a potentially long path when it becomes an immutable resource.
#[derive(Debug, Default)]
pub struct PathResourceBuilder {
    segments: Vec<PathSegment>,
    active_subpath: bool,
    current_point: Option<ScenePoint>,
}

impl PathResourceBuilder {
    /// Creates an empty path construction.
    pub const fn new() -> Self {
        Self {
            segments: Vec::new(),
            active_subpath: false,
            current_point: None,
        }
    }

    /// Returns the validated segment count.
    pub const fn len(&self) -> usize {
        self.segments.len()
    }

    /// Returns allocator-reported segment capacity.
    pub const fn capacity(&self) -> usize {
        self.segments.capacity()
    }

    /// Returns whether no segments have been appended.
    pub const fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Returns allocator-reported retained segment capacity.
    pub fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.segments)
    }

    /// Fallibly reserves capacity for exactly the requested additional segment count.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), SceneError> {
        reserve_exact(&mut self.segments, additional)
    }

    /// Validates and appends one normalized construction segment.
    pub fn try_push(&mut self, segment: PathSegment) -> Result<(), SceneError> {
        let (next_active, next_point) = match segment {
            PathSegment::MoveTo(point) => (true, Some(point)),
            PathSegment::LineTo(_) | PathSegment::CubicTo { .. } if !self.active_subpath => {
                return Err(SceneError::for_code(
                    SceneErrorCode::InvalidCommandSequence,
                    None,
                ));
            }
            PathSegment::ClosePath if !self.active_subpath => {
                return Err(SceneError::for_code(
                    SceneErrorCode::InvalidCommandSequence,
                    None,
                ));
            }
            PathSegment::ClosePath => (false, None),
            PathSegment::LineTo(point) => (true, Some(point)),
            PathSegment::CubicTo { end, .. } => (true, Some(end)),
        };
        if self.segments.len() == self.segments.capacity() {
            self.try_reserve_exact(1)?;
        }
        self.segments.push(segment);
        self.active_subpath = next_active;
        self.current_point = next_point;
        Ok(())
    }

    /// Appends one quadratic Bézier as its mathematically equivalent cubic.
    ///
    /// Scene paths deliberately expose one canonical cubic representation. Both generated
    /// controls are rounded once to the nearest nine-decimal Scene scalar, with ties away from
    /// zero, so TrueType producers do not depend on floating-point behavior.
    pub fn try_push_quadratic(
        &mut self,
        control: ScenePoint,
        end: ScenePoint,
    ) -> Result<(), SceneError> {
        let start = self
            .current_point
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InvalidCommandSequence, None))?;
        let control_1 = quadratic_cubic_control(start, control)?;
        let control_2 = quadratic_cubic_control(end, control)?;
        self.try_push(PathSegment::CubicTo {
            control_1,
            control_2,
            end,
        })
    }

    /// Seals the already validated segments in O(1) without rescanning or copying their buffer.
    pub fn finish(self) -> PathResource {
        PathResource {
            segments: Arc::new(self.segments),
        }
    }
}

fn quadratic_cubic_control(
    endpoint: ScenePoint,
    control: ScenePoint,
) -> Result<ScenePoint, SceneError> {
    Ok(ScenePoint::new(
        weighted_third(endpoint.x(), control.x())?,
        weighted_third(endpoint.y(), control.y())?,
    ))
}

fn weighted_third(endpoint: SceneScalar, control: SceneScalar) -> Result<SceneScalar, SceneError> {
    let numerator = i128::from(endpoint.scaled())
        .checked_add(
            i128::from(control.scaled())
                .checked_mul(2)
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?,
        )
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
    let quotient = numerator / 3;
    let remainder = numerator % 3;
    let rounded = if remainder.abs() * 2 >= 3 {
        quotient
            .checked_add(if numerator.is_negative() { -1 } else { 1 })
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?
    } else {
        quotient
    };
    i64::try_from(rounded)
        .map(SceneScalar::from_scaled)
        .map_err(|_| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
}

/// PDF path fill rule.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FillRule {
    /// Nonzero winding rule.
    Nonzero,
    /// Even-odd parity rule.
    EvenOdd,
}

/// Stroke endpoint shape.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LineCap {
    /// Butt cap.
    Butt,
    /// Round cap.
    Round,
    /// Projecting square cap.
    Square,
}

/// Stroke join shape.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LineJoin {
    /// Miter join.
    Miter,
    /// Round join.
    Round,
    /// Bevel join.
    Bevel,
}

/// Validated dash array and phase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashPattern {
    array: Arc<Vec<SceneScalar>>,
    phase: SceneScalar,
}

impl DashPattern {
    /// Creates a nonnegative dash pattern; a nonempty all-zero array is rejected.
    pub fn new(array: Vec<SceneScalar>, phase: SceneScalar) -> Result<Self, SceneError> {
        let mut builder = DashPatternBuilder::new();
        builder.try_reserve_exact(array.len())?;
        for value in array {
            builder.try_push(value)?;
        }
        builder.finish(phase)
    }

    /// Borrows dash lengths in user-space units.
    pub fn array(&self) -> &[SceneScalar] {
        &self.array
    }

    /// Returns the nonnegative dash phase.
    pub const fn phase(&self) -> SceneScalar {
        self.phase
    }

    /// Returns allocator-reported retained dash-array capacity.
    pub fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.array)
    }
}

/// Incrementally validated dash-array construction with an O(1), element-buffer-copy-free handoff.
#[derive(Debug, Default)]
pub struct DashPatternBuilder {
    array: Vec<SceneScalar>,
    any_nonzero: bool,
}

impl DashPatternBuilder {
    /// Creates an empty dash-array construction.
    pub const fn new() -> Self {
        Self {
            array: Vec::new(),
            any_nonzero: false,
        }
    }

    /// Returns the appended dash-entry count.
    pub const fn len(&self) -> usize {
        self.array.len()
    }

    /// Returns whether no dash entries have been appended.
    pub const fn is_empty(&self) -> bool {
        self.array.is_empty()
    }

    /// Returns allocator-reported retained dash-array capacity.
    pub fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.array)
    }

    /// Fallibly reserves capacity for exactly the requested additional entry count.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), SceneError> {
        reserve_exact(&mut self.array, additional)
    }

    /// Validates and appends one nonnegative dash length.
    pub fn try_push(&mut self, value: SceneScalar) -> Result<(), SceneError> {
        if value < SceneScalar::ZERO {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        if self.array.len() == self.array.capacity() {
            self.try_reserve_exact(1)?;
        }
        self.array.push(value);
        self.any_nonzero |= value != SceneScalar::ZERO;
        Ok(())
    }

    /// Seals a valid pattern in O(1) without rescanning or copying its array buffer.
    pub fn finish(self, phase: SceneScalar) -> Result<DashPattern, SceneError> {
        if phase < SceneScalar::ZERO || (!self.array.is_empty() && !self.any_nonzero) {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(DashPattern {
            array: Arc::new(self.array),
            phase,
        })
    }
}

/// Complete stroke state at paint time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineStyle {
    width: SceneScalar,
    cap: LineCap,
    join: LineJoin,
    miter_limit: SceneScalar,
    dash: DashPattern,
    stroke_transform: Matrix,
}

impl LineStyle {
    /// Creates checked stroke state, retaining the paint-time transform.
    pub fn new(
        width: SceneScalar,
        cap: LineCap,
        join: LineJoin,
        miter_limit: SceneScalar,
        dash: DashPattern,
        stroke_transform: Matrix,
    ) -> Result<Self, SceneError> {
        if width < SceneScalar::ZERO || miter_limit < SceneScalar::ONE {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            width,
            cap,
            join,
            miter_limit,
            dash,
            stroke_transform,
        })
    }

    /// Returns line width in user-space units.
    pub const fn width(&self) -> SceneScalar {
        self.width
    }

    /// Returns the line cap.
    pub const fn cap(&self) -> LineCap {
        self.cap
    }

    /// Returns the line join.
    pub const fn join(&self) -> LineJoin {
        self.join
    }

    /// Returns the miter-limit ratio.
    pub const fn miter_limit(&self) -> SceneScalar {
        self.miter_limit
    }

    /// Borrows the dash pattern.
    pub const fn dash(&self) -> &DashPattern {
        &self.dash
    }

    /// Returns the exact transform active when the stroke was painted.
    pub const fn stroke_transform(&self) -> Matrix {
        self.stroke_transform
    }
}

/// Unsigned normalized Scene channel with exact `0..=65535` representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneUnit(u16);

impl SceneUnit {
    /// Fully transparent or zero-intensity value.
    pub const ZERO: Self = Self(0);
    /// Fully opaque or full-intensity value.
    pub const ONE: Self = Self(u16::MAX);

    /// Creates a value from its exact Q0.16 endpoint-inclusive representation.
    pub const fn from_u16(value: u16) -> Self {
        Self(value)
    }

    /// Returns the exact endpoint-inclusive representation.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Registered device color values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceColor {
    /// DeviceGray.
    Gray(SceneUnit),
    /// DeviceRGB.
    Rgb {
        /// Red channel.
        red: SceneUnit,
        /// Green channel.
        green: SceneUnit,
        /// Blue channel.
        blue: SceneUnit,
    },
    /// DeviceCMYK.
    Cmyk {
        /// Cyan component.
        cyan: SceneUnit,
        /// Magenta component.
        magenta: SceneUnit,
        /// Yellow component.
        yellow: SceneUnit,
        /// Black component.
        black: SceneUnit,
    },
}

/// Registered separable blend mode.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BlendMode {
    /// Normal source-over.
    Normal,
    /// Multiply.
    Multiply,
    /// Screen.
    Screen,
}

/// Complete constant-color paint state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Paint {
    color: DeviceColor,
    alpha: SceneUnit,
    blend_mode: BlendMode,
}

impl Paint {
    /// Creates constant-color paint.
    pub const fn new(color: DeviceColor, alpha: SceneUnit, blend_mode: BlendMode) -> Self {
        Self {
            color,
            alpha,
            blend_mode,
        }
    }

    /// Returns the device color.
    pub const fn color(self) -> DeviceColor {
        self.color
    }

    /// Returns constant alpha.
    pub const fn alpha(self) -> SceneUnit {
        self.alpha
    }

    /// Returns the blend mode.
    pub const fn blend_mode(self) -> BlendMode {
        self.blend_mode
    }
}

/// Stable Scene-v2 graphics resource identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GraphicsResourceId(u32);

impl GraphicsResourceId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the zero-based identifier.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Registered decoded image color space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageColorSpace {
    /// One DeviceGray component.
    DeviceGray,
    /// Three DeviceRGB components.
    DeviceRgb,
    /// Four DeviceCMYK components.
    DeviceCmyk,
}

impl ImageColorSpace {
    /// Returns the exact component count.
    pub const fn components(self) -> u8 {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRgb => 3,
            Self::DeviceCmyk => 4,
        }
    }
}

/// Exact source/decode identity for one indirect graphics resource.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GraphicsResourceSource {
    object: ObjectRef,
    revision_startxref: u64,
    decode_context: u64,
}

impl GraphicsResourceSource {
    /// Creates one stable resource key component.
    pub const fn new(object: ObjectRef, revision_startxref: u64, decode_context: u64) -> Self {
        Self {
            object,
            revision_startxref,
            decode_context,
        }
    }

    /// Returns the defining PDF object.
    pub const fn object(self) -> ObjectRef {
        self.object
    }

    /// Returns the defining revision anchor.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the caller-defined canonical decode context.
    pub const fn decode_context(self) -> u64 {
        self.decode_context
    }
}

/// Basic decoded image with an optional same-size 8-bit grayscale soft mask.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageResource {
    source: GraphicsResourceSource,
    width: u32,
    height: u32,
    color_space: ImageColorSpace,
    bits_per_component: u8,
    interpolate: bool,
    decoded: Arc<Vec<u8>>,
    soft_mask: Option<Arc<Vec<u8>>>,
}

impl ImageResource {
    /// Creates one exact 8-bit decoded image.
    pub fn new(
        source: GraphicsResourceSource,
        width: u32,
        height: u32,
        color_space: ImageColorSpace,
        bits_per_component: u8,
        interpolate: bool,
        decoded: Vec<u8>,
    ) -> Result<Self, SceneError> {
        Self::new_with_soft_mask(
            source,
            width,
            height,
            color_space,
            bits_per_component,
            interpolate,
            decoded,
            None,
        )
    }

    /// Creates one exact 8-bit decoded image with an optional 8-bit alpha plane.
    #[allow(
        clippy::too_many_arguments,
        reason = "image construction keeps identity, geometry, color, sampling, pixels, and alpha explicit"
    )]
    pub fn new_with_soft_mask(
        source: GraphicsResourceSource,
        width: u32,
        height: u32,
        color_space: ImageColorSpace,
        bits_per_component: u8,
        interpolate: bool,
        decoded: Vec<u8>,
        soft_mask: Option<Vec<u8>>,
    ) -> Result<Self, SceneError> {
        if width == 0 || height == 0 || bits_per_component != 8 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(u64::from(color_space.components())))
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
        if u64::try_from(decoded.len()).unwrap_or(u64::MAX) != expected {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        let mask_bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
        if soft_mask
            .as_ref()
            .is_some_and(|mask| u64::try_from(mask.len()).unwrap_or(u64::MAX) != mask_bytes)
        {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            source,
            width,
            height,
            color_space,
            bits_per_component,
            interpolate,
            decoded: Arc::new(exact_vec(decoded)?),
            soft_mask: match soft_mask {
                Some(mask) => Some(Arc::new(exact_vec(mask)?)),
                None => None,
            },
        })
    }

    /// Returns exact source/decode identity.
    pub const fn source(&self) -> GraphicsResourceSource {
        self.source
    }

    /// Returns image width.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns image height.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the decoded color space.
    pub const fn color_space(&self) -> ImageColorSpace {
        self.color_space
    }

    /// Returns bits per component.
    pub const fn bits_per_component(&self) -> u8 {
        self.bits_per_component
    }

    /// Returns the registered interpolation flag.
    pub const fn interpolate(&self) -> bool {
        self.interpolate
    }

    /// Borrows exact decoded component bytes in row-major order.
    pub fn decoded(&self) -> &[u8] {
        &self.decoded
    }

    /// Borrows the optional same-size 8-bit grayscale alpha plane.
    pub fn soft_mask(&self) -> Option<&[u8]> {
        self.soft_mask.as_deref().map(Vec::as_slice)
    }

    pub(crate) fn payload_bytes(&self) -> Result<u64, SceneError> {
        u64::try_from(self.decoded.len())
            .ok()
            .and_then(|decoded| {
                decoded.checked_add(
                    self.soft_mask
                        .as_ref()
                        .map_or(0, |mask| u64::try_from(mask.len()).unwrap_or(u64::MAX)),
                )
            })
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        let decoded = vec_capacity_bytes(&self.decoded)?;
        match &self.soft_mask {
            Some(mask) => decoded
                .checked_add(vec_capacity_bytes(mask)?)
                .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None)),
            None => Ok(decoded),
        }
    }
}

/// One project-owned glyph outline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphOutline {
    source: GraphicsResourceSource,
    glyph_id: u32,
    units_per_em: u16,
    outline: PathResource,
}

impl GlyphOutline {
    /// Creates a checked glyph outline.
    pub fn new(
        source: GraphicsResourceSource,
        glyph_id: u32,
        units_per_em: u16,
        outline: PathResource,
    ) -> Result<Self, SceneError> {
        if units_per_em == 0 {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            source,
            glyph_id,
            units_per_em,
            outline,
        })
    }

    /// Returns exact font-object/decode identity.
    pub const fn source(&self) -> GraphicsResourceSource {
        self.source
    }

    /// Returns the font-local glyph identifier.
    pub const fn glyph_id(&self) -> u32 {
        self.glyph_id
    }

    /// Returns font design units per em.
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Borrows the exact outline.
    pub const fn outline(&self) -> &PathResource {
        &self.outline
    }
}

/// One positioned glyph use in a Scene command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionedGlyph {
    outline: GraphicsResourceId,
    transform: Matrix,
    character_code: u32,
}

/// One unresolved glyph use supplied to the graphics Scene builder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphUse {
    outline: GlyphOutline,
    transform: Matrix,
    character_code: u32,
}

impl GlyphUse {
    /// Creates one glyph use whose outline will be interned at first command use.
    pub const fn new(outline: GlyphOutline, transform: Matrix, character_code: u32) -> Self {
        Self {
            outline,
            transform,
            character_code,
        }
    }

    /// Borrows the project-owned glyph outline.
    pub const fn outline(&self) -> &GlyphOutline {
        &self.outline
    }

    /// Returns the exact glyph-to-page transform.
    pub const fn transform(&self) -> Matrix {
        self.transform
    }

    /// Returns the source PDF character code.
    pub const fn character_code(&self) -> u32 {
        self.character_code
    }
}

impl PositionedGlyph {
    /// Creates one glyph use with exact text-to-page transform and PDF character code.
    pub const fn new(outline: GraphicsResourceId, transform: Matrix, character_code: u32) -> Self {
        Self {
            outline,
            transform,
            character_code,
        }
    }

    /// Returns the glyph-outline resource.
    pub const fn outline(self) -> GraphicsResourceId {
        self.outline
    }

    /// Returns the exact glyph-to-page transform.
    pub const fn transform(self) -> Matrix {
        self.transform
    }

    /// Returns the source PDF character code.
    pub const fn character_code(self) -> u32 {
        self.character_code
    }
}

/// Paint operation applied to one positioned glyph sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlyphPainting {
    /// Fill glyph outlines with the nonstroking paint.
    Fill(Paint),
    /// Stroke glyph outlines with the stroking paint and complete line state.
    Stroke {
        /// Stroke paint.
        paint: Paint,
        /// Complete line state.
        style: LineStyle,
    },
    /// Fill and then stroke glyph outlines.
    FillStroke {
        /// Fill paint.
        fill: Paint,
        /// Stroke paint.
        stroke: Paint,
        /// Complete line state.
        style: LineStyle,
    },
}

impl GlyphPainting {
    /// Returns the fill paint when this operation fills glyph outlines.
    pub const fn fill(&self) -> Option<Paint> {
        match self {
            Self::Fill(paint) | Self::FillStroke { fill: paint, .. } => Some(*paint),
            Self::Stroke { .. } => None,
        }
    }

    /// Returns the stroke paint and line state when this operation strokes glyph outlines.
    pub const fn stroke(&self) -> Option<(Paint, &LineStyle)> {
        match self {
            Self::Stroke { paint, style } => Some((*paint, style)),
            Self::FillStroke { stroke, style, .. } => Some((*stroke, style)),
            Self::Fill(_) => None,
        }
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        self.stroke()
            .map_or(Ok(0), |(_, style)| style.dash().retained_bytes())
    }
}

/// Immutable positioned glyph sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphRun {
    glyphs: Arc<Vec<PositionedGlyph>>,
    painting: GlyphPainting,
}

impl GlyphRun {
    /// Creates one nonempty filled glyph run.
    pub fn new(glyphs: Vec<PositionedGlyph>, paint: Paint) -> Result<Self, SceneError> {
        Self::new_painted(glyphs, GlyphPainting::Fill(paint))
    }

    /// Creates one nonempty glyph run with an explicit paint operation.
    pub fn new_painted(
        glyphs: Vec<PositionedGlyph>,
        painting: GlyphPainting,
    ) -> Result<Self, SceneError> {
        if glyphs.is_empty() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            glyphs: Arc::new(exact_vec(glyphs)?),
            painting,
        })
    }

    pub(crate) fn from_reserved_painted(
        glyphs: Vec<PositionedGlyph>,
        painting: GlyphPainting,
    ) -> Result<Self, SceneError> {
        if glyphs.is_empty() {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            glyphs: Arc::new(glyphs),
            painting,
        })
    }

    /// Borrows positioned glyphs.
    pub fn glyphs(&self) -> &[PositionedGlyph] {
        &self.glyphs
    }

    /// Returns the primary paint, preserving the fill-only API for existing callers.
    ///
    /// Fill-stroke runs return their fill paint and stroke-only runs return their stroke paint.
    pub const fn paint(&self) -> Paint {
        match &self.painting {
            GlyphPainting::Fill(paint) | GlyphPainting::FillStroke { fill: paint, .. } => *paint,
            GlyphPainting::Stroke { paint, .. } => *paint,
        }
    }

    /// Borrows the explicit glyph paint operation.
    pub const fn painting(&self) -> &GlyphPainting {
        &self.painting
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.glyphs)?
            .checked_add(self.painting.retained_bytes()?)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
    }
}

/// Graphics resource payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsResource {
    /// One content-created path.
    Path(PathResource),
    /// One decoded basic image.
    Image(ImageResource),
    /// One embedded glyph outline.
    GlyphOutline(GlyphOutline),
}

impl GraphicsResource {
    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        match self {
            Self::Path(path) => path.retained_bytes(),
            Self::Image(image) => image.retained_bytes(),
            Self::GlyphOutline(glyph) => glyph.outline().retained_bytes(),
        }
    }

    pub(crate) fn comparison_work(&self, other: &Self) -> Result<u64, SceneError> {
        let payload = match (self, other) {
            (Self::Path(left), Self::Path(right)) => {
                left.segments().len().max(right.segments().len())
            }
            (Self::Image(left), Self::Image(right)) => {
                usize::try_from(left.payload_bytes()?.max(right.payload_bytes()?))
                    .map_err(|_| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?
            }
            (Self::GlyphOutline(left), Self::GlyphOutline(right)) => left
                .outline()
                .segments()
                .len()
                .max(right.outline().segments().len()),
            _ => 0,
        };
        u64::try_from(payload)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
    }

    pub(crate) fn has_conflicting_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Image(left), Self::Image(right)) => {
                left.source() == right.source() && left != right
            }
            (Self::GlyphOutline(left), Self::GlyphOutline(right)) => {
                left.source() == right.source()
                    && left.glyph_id() == right.glyph_id()
                    && left != right
            }
            (Self::Path(_), Self::Path(_))
            | (Self::Path(_), Self::Image(_) | Self::GlyphOutline(_))
            | (Self::Image(_), Self::Path(_) | Self::GlyphOutline(_))
            | (Self::GlyphOutline(_), Self::Path(_) | Self::Image(_)) => false,
        }
    }
}

/// Stable graphics resource table entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsResourceEntry {
    id: GraphicsResourceId,
    resource: GraphicsResource,
}

impl GraphicsResourceEntry {
    pub(crate) const fn new(id: GraphicsResourceId, resource: GraphicsResource) -> Self {
        Self { id, resource }
    }

    /// Returns the first-use identifier.
    pub const fn id(&self) -> GraphicsResourceId {
        self.id
    }

    /// Borrows the resource payload.
    pub const fn resource(&self) -> &GraphicsResource {
        &self.resource
    }
}

/// Stable capability-requirement identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityRequirementId(u32);

impl CapabilityRequirementId {
    pub(crate) const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the zero-based identifier.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Registered Scene-v2 semantic capability.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GraphicsCapability {
    /// Path filling.
    PathFill,
    /// Path stroking.
    PathStroke,
    /// Nested clipping.
    Clip,
    /// Device color conversion.
    DeviceColor,
    /// Constant alpha.
    ConstantAlpha,
    /// Multiply or Screen blending.
    Blend,
    /// Soft-mask transparency.
    SoftMask,
    /// Basic unmasked images.
    Image,
    /// Embedded glyph outlines.
    Glyph,
    /// Isolated transparency groups.
    IsolatedGroup,
    /// Knockout transparency groups in the registered atomic-child subset.
    KnockoutGroup,
}

/// Declared support status of one requirement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CapabilityStatus {
    /// The producing profile supports the exact requirement.
    Supported,
    /// The exact requirement remains outside the producing profile.
    Unsupported,
}

/// Optional stable requirement context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityContext {
    /// Requirement applies to the whole Scene.
    Scene,
    /// Requirement applies to one command index.
    Command(u32),
    /// Requirement applies to one resource.
    Resource(GraphicsResourceId),
}

/// One bounded capability-graph node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequirement {
    id: CapabilityRequirementId,
    capability: GraphicsCapability,
    parameter: u64,
    context: CapabilityContext,
    dependencies: Arc<Vec<CapabilityRequirementId>>,
    status: CapabilityStatus,
}

impl CapabilityRequirement {
    pub(crate) fn new(
        id: CapabilityRequirementId,
        capability: GraphicsCapability,
        parameter: u64,
        context: CapabilityContext,
        dependencies: Vec<CapabilityRequirementId>,
        status: CapabilityStatus,
    ) -> Result<Self, SceneError> {
        if dependencies.contains(&id) {
            return Err(SceneError::for_code(
                SceneErrorCode::InvalidCommandSequence,
                None,
            ));
        }
        Ok(Self {
            id,
            capability,
            parameter,
            context,
            dependencies: Arc::new(exact_vec(dependencies)?),
            status,
        })
    }

    /// Returns the stable graph identifier.
    pub const fn id(&self) -> CapabilityRequirementId {
        self.id
    }

    /// Returns the typed capability.
    pub const fn capability(&self) -> GraphicsCapability {
        self.capability
    }

    /// Returns capability-specific canonical parameters.
    pub const fn parameter(&self) -> u64 {
        self.parameter
    }

    /// Returns command/resource context.
    pub const fn context(&self) -> CapabilityContext {
        self.context
    }

    /// Borrows dependency identifiers in canonical order.
    pub fn dependencies(&self) -> &[CapabilityRequirementId] {
        &self.dependencies
    }

    /// Returns declared support status.
    pub const fn status(&self) -> CapabilityStatus {
        self.status
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        vec_capacity_bytes(&self.dependencies)
    }
}

/// Scene-v2 graphics command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsCommand {
    /// Save complete graphics state.
    Save,
    /// Restore complete graphics state.
    Restore,
    /// Intersect the current clip with one path.
    Clip {
        /// Path resource.
        path: GraphicsResourceId,
        /// Clip fill rule.
        rule: FillRule,
        /// Path-to-page transform at clip time.
        transform: Matrix,
    },
    /// Fill one path.
    Fill {
        /// Path resource.
        path: GraphicsResourceId,
        /// Fill rule.
        rule: FillRule,
        /// Fill paint.
        paint: Paint,
        /// Path-to-page transform at paint time.
        transform: Matrix,
    },
    /// Stroke one path.
    Stroke {
        /// Path resource.
        path: GraphicsResourceId,
        /// Stroke paint.
        paint: Paint,
        /// Complete line state.
        style: LineStyle,
        /// Path-to-page transform at paint time.
        transform: Matrix,
    },
    /// Fill then stroke one path.
    FillStroke {
        /// Path resource.
        path: GraphicsResourceId,
        /// Fill rule.
        rule: FillRule,
        /// Fill paint.
        fill: Paint,
        /// Stroke paint.
        stroke: Paint,
        /// Complete line state.
        style: LineStyle,
        /// Path-to-page transform at paint time.
        transform: Matrix,
    },
    /// Paint one image into the transformed unit square.
    DrawImage {
        /// Image resource.
        image: GraphicsResourceId,
        /// Image-to-page transform.
        transform: Matrix,
        /// Constant alpha.
        alpha: SceneUnit,
        /// Blend mode.
        blend_mode: BlendMode,
    },
    /// Paint positioned embedded glyph outlines.
    DrawGlyphRun(GlyphRun),
    /// Begin one offscreen transparency group.
    BeginIsolatedGroup {
        /// Group constant alpha.
        alpha: SceneUnit,
        /// Group blend mode.
        blend_mode: BlendMode,
        /// Whether immediate child objects use knockout compositing.
        knockout: bool,
    },
    /// End the current offscreen transparency group.
    EndIsolatedGroup,
}

impl GraphicsCommand {
    /// Reports whether this command can directly affect output pixels.
    pub const fn is_visible(&self) -> bool {
        matches!(
            self,
            Self::Fill { .. }
                | Self::Stroke { .. }
                | Self::FillStroke { .. }
                | Self::DrawImage { .. }
                | Self::DrawGlyphRun(_)
        )
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, SceneError> {
        match self {
            Self::Stroke { style, .. } | Self::FillStroke { style, .. } => {
                style.dash().retained_bytes()
            }
            Self::DrawGlyphRun(run) => run.retained_bytes(),
            Self::Save
            | Self::Restore
            | Self::Clip { .. }
            | Self::Fill { .. }
            | Self::DrawImage { .. }
            | Self::BeginIsolatedGroup { .. }
            | Self::EndIsolatedGroup => Ok(0),
        }
    }
}

/// One graphics command paired with conservative bounds and decoded provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsCommandRecord {
    command: GraphicsCommand,
    bounds: SceneBounds,
    source: CommandSource,
}

impl GraphicsCommandRecord {
    pub(crate) const fn new(
        command: GraphicsCommand,
        bounds: SceneBounds,
        source: CommandSource,
    ) -> Self {
        Self {
            command,
            bounds,
            source,
        }
    }

    /// Borrows the semantic command.
    pub const fn command(&self) -> &GraphicsCommand {
        &self.command
    }

    /// Returns conservative bounds.
    pub const fn bounds(&self) -> SceneBounds {
        self.bounds
    }

    /// Returns exact decoded operator provenance.
    pub const fn source(&self) -> CommandSource {
        self.source
    }
}

/// Immutable Scene-v2 graphics payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsScene {
    commands: Arc<Vec<GraphicsCommandRecord>>,
    resources: Arc<Vec<GraphicsResourceEntry>>,
    requirements: Arc<Vec<CapabilityRequirement>>,
    limits: GraphicsSceneLimits,
    stats: GraphicsSceneStats,
}

/// Deterministic Scene-v2 ownership and construction accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphicsSceneStats {
    retained_bytes: u64,
    resource_index_work: u64,
}

impl GraphicsSceneStats {
    pub(crate) const fn new(retained_bytes: u64, resource_index_work: u64) -> Self {
        Self {
            retained_bytes,
            resource_index_work,
        }
    }

    /// Returns final published allocator-reported retained capacity, including nested vectors.
    ///
    /// Builder transaction-live working retention can be higher and is independently admitted by
    /// the same graphics retained-byte limit before publication.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    /// Returns charged resource comparisons and payload comparison units.
    pub const fn resource_index_work(self) -> u64 {
        self.resource_index_work
    }
}

impl GraphicsScene {
    pub(crate) fn new(
        commands: Vec<GraphicsCommandRecord>,
        resources: Vec<GraphicsResourceEntry>,
        requirements: Vec<CapabilityRequirement>,
        limits: GraphicsSceneLimits,
        stats: GraphicsSceneStats,
    ) -> Self {
        Self {
            commands: Arc::new(commands),
            resources: Arc::new(resources),
            requirements: Arc::new(requirements),
            limits,
            stats,
        }
    }

    /// Borrows graphics commands.
    pub fn commands(&self) -> &[GraphicsCommandRecord] {
        &self.commands
    }

    /// Borrows resources in first-command-use identifier order.
    pub fn resources(&self) -> &[GraphicsResourceEntry] {
        &self.resources
    }

    /// Borrows the complete capability graph.
    pub fn requirements(&self) -> &[CapabilityRequirement] {
        &self.requirements
    }

    /// Returns the complete validated graphics Scene limit profile.
    pub const fn limits(&self) -> GraphicsSceneLimits {
        self.limits
    }

    /// Returns complete Scene-v2 ownership and construction accounting.
    pub const fn stats(&self) -> GraphicsSceneStats {
        self.stats
    }

    /// Returns whether every declared requirement is supported.
    pub fn is_supported(&self) -> bool {
        self.requirements
            .iter()
            .all(|requirement| requirement.status() == CapabilityStatus::Supported)
    }
}

fn exact_vec<T>(values: Vec<T>) -> Result<Vec<T>, SceneError> {
    let mut exact = Vec::new();
    reserve_exact(&mut exact, values.len())?;
    exact.extend(values);
    Ok(exact)
}

fn reserve_exact<T>(values: &mut Vec<T>, additional: usize) -> Result<(), SceneError> {
    let current = vec_capacity_bytes(values)?;
    let attempted = u64::try_from(additional)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
    values.try_reserve_exact(additional).map_err(|_| {
        SceneError::resource(
            crate::SceneLimitKind::Allocation,
            u64::MAX,
            current,
            attempted,
            None,
        )
    })
}

fn vec_capacity_bytes<T>(values: &Vec<T>) -> Result<u64, SceneError> {
    u64::try_from(values.capacity())
        .ok()
        .and_then(|capacity| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| capacity.checked_mul(width))
        })
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
}

#[cfg(test)]
mod tests {
    use super::{
        DashPattern, DashPatternBuilder, PathResource, PathResourceBuilder, PathSegment,
        SceneBounds, ScenePoint,
    };
    use crate::{SceneErrorCode, SceneScalar};

    #[test]
    fn path_and_bounds_constructors_reject_invalid_structure() {
        let point = ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO);
        assert_eq!(
            PathResource::new(vec![PathSegment::LineTo(point)])
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );
        assert_eq!(
            SceneBounds::finite(ScenePoint::new(SceneScalar::ONE, SceneScalar::ONE), point)
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidGeometry
        );
        assert_eq!(
            DashPattern::new(vec![SceneScalar::ZERO], SceneScalar::ZERO)
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );
    }

    #[test]
    fn incremental_path_and_dash_builders_validate_before_o1_handoff() {
        let point = ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO);
        let mut invalid_path = PathResourceBuilder::new();
        assert_eq!(
            invalid_path
                .try_push(PathSegment::LineTo(point))
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );

        let mut path = PathResourceBuilder::new();
        path.try_reserve_exact(2).unwrap();
        path.try_push(PathSegment::MoveTo(point)).unwrap();
        path.try_push(PathSegment::LineTo(point)).unwrap();
        let path_bytes = path.retained_bytes().unwrap();
        let path_buffer = path.segments.as_ptr();
        let path = path.finish();
        assert_eq!(path.segments().len(), 2);
        assert_eq!(path.retained_bytes().unwrap(), path_bytes);
        assert_eq!(path.segments.as_ptr(), path_buffer);

        let mut dash = DashPatternBuilder::new();
        dash.try_reserve_exact(2).unwrap();
        dash.try_push(SceneScalar::ONE).unwrap();
        dash.try_push(SceneScalar::from_scaled(2_000_000_000))
            .unwrap();
        let dash_bytes = dash.retained_bytes().unwrap();
        let dash_buffer = dash.array.as_ptr();
        let dash = dash.finish(SceneScalar::ZERO).unwrap();
        assert_eq!(dash.array().len(), 2);
        assert_eq!(dash.retained_bytes().unwrap(), dash_bytes);
        assert_eq!(dash.array.as_ptr(), dash_buffer);

        let mut negative_dash = DashPatternBuilder::new();
        assert_eq!(
            negative_dash
                .try_push(SceneScalar::from_scaled(-1))
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );
        let mut all_zero_dash = DashPatternBuilder::new();
        all_zero_dash.try_push(SceneScalar::ZERO).unwrap();
        assert_eq!(
            all_zero_dash.finish(SceneScalar::ZERO).unwrap_err().code(),
            SceneErrorCode::InvalidCommandSequence
        );
    }

    #[test]
    fn quadratic_segments_convert_to_one_deterministically_rounded_cubic() {
        let mut missing_start = PathResourceBuilder::new();
        let control = ScenePoint::new(SceneScalar::ONE, SceneScalar::from_scaled(-1_000_000_000));
        let end = ScenePoint::new(
            SceneScalar::from_scaled(2_000_000_000),
            SceneScalar::from_scaled(-2_000_000_000),
        );
        assert_eq!(
            missing_start
                .try_push_quadratic(control, end)
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );

        let start = ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO);
        let mut path = PathResourceBuilder::new();
        path.try_push(PathSegment::MoveTo(start)).unwrap();
        path.try_push_quadratic(control, end).unwrap();
        path.try_push(PathSegment::LineTo(start)).unwrap();
        assert_eq!(
            path.finish().segments(),
            [
                PathSegment::MoveTo(start),
                PathSegment::CubicTo {
                    control_1: ScenePoint::new(
                        SceneScalar::from_scaled(666_666_667),
                        SceneScalar::from_scaled(-666_666_667),
                    ),
                    control_2: ScenePoint::new(
                        SceneScalar::from_scaled(1_333_333_333),
                        SceneScalar::from_scaled(-1_333_333_333),
                    ),
                    end,
                },
                PathSegment::LineTo(start),
            ]
        );
    }
}
