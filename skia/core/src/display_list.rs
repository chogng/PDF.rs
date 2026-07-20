use crate::{Color, FillRule, Paint, Path, Rect, Scalar, SkiaError, SkiaErrorCode, Transform};
use pdf_rs_skia_image::Image;
use pdf_rs_skia_text::GlyphRun;

/// Command-buffer-local identifier for an immutable path resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PathId(u32);

/// Command-buffer-local identifier for an immutable image resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImageId(u32);

/// Command-buffer-local identifier for an immutable shaped glyph run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GlyphRunId(u32);

/// Backend-neutral drawing operation in declaration order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawCommand {
    /// Clears the entire target without inheriting state.
    Clear(Color),
    /// Saves the current transform and clip state.
    Save,
    /// Restores the most recently saved state.
    Restore,
    /// Intersects following draws with a logical rectangle.
    ClipRect(Rect),
    /// Replaces the transform for following draws.
    SetTransform(Transform),
    /// Concatenates an affine transform onto the current drawing state.
    ConcatTransform(Transform),
    /// Fills a registered path.
    FillPath {
        /// Local path resource.
        path: PathId,
        /// Fill containment rule.
        rule: FillRule,
        /// Source paint.
        paint: Paint,
    },
    /// Strokes a registered path with a positive logical width.
    StrokePath {
        /// Local path resource.
        path: PathId,
        /// Positive stroke width.
        width: Scalar,
        /// Source paint.
        paint: Paint,
    },
    /// Draws a registered image into a logical destination rectangle.
    DrawImage {
        /// Local image resource.
        image: ImageId,
        /// Logical destination rectangle.
        destination: Rect,
        /// Additional source opacity.
        opacity: u8,
        /// Source paint and blend mode.
        paint: Paint,
    },
    /// Draws one registered shaped glyph run.
    DrawGlyphRun {
        /// Local shaped glyph-run resource.
        run: GlyphRunId,
        /// Source paint and blend mode.
        paint: Paint,
    },
}

/// Immutable portable drawing list and the resources it owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayList {
    commands: Vec<DrawCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
    glyph_runs: Vec<GlyphRun>,
}

impl DisplayList {
    /// Borrows commands in declaration order.
    pub fn commands(&self) -> &[DrawCommand] {
        &self.commands
    }
    /// Resolves a local path resource.
    pub fn path(&self, id: PathId) -> Option<&Path> {
        self.paths.get(usize::try_from(id.0).ok()?)
    }
    /// Resolves a local image resource.
    pub fn image(&self, id: ImageId) -> Option<&Image> {
        self.images.get(usize::try_from(id.0).ok()?)
    }
    /// Resolves a local shaped glyph-run resource.
    pub fn glyph_run(&self, id: GlyphRunId) -> Option<&GlyphRun> {
        self.glyph_runs.get(usize::try_from(id.0).ok()?)
    }
}

/// Bounded recorder for one immutable [`DisplayList`].
#[derive(Debug)]
pub struct DisplayListBuilder {
    commands: Vec<DrawCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
    glyph_runs: Vec<GlyphRun>,
    max_items: usize,
}

impl DisplayListBuilder {
    /// Creates a builder with one positive per-kind resource and command ceiling.
    pub fn new(max_items: usize) -> Result<Self, SkiaError> {
        if max_items == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidLimits));
        }
        Ok(Self {
            commands: Vec::new(),
            paths: Vec::new(),
            images: Vec::new(),
            glyph_runs: Vec::new(),
            max_items,
        })
    }
    /// Registers an immutable path.
    pub fn add_path(&mut self, path: Path) -> Result<PathId, SkiaError> {
        let id = self.resource_id(self.paths.len())?;
        self.paths
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.paths.push(path);
        Ok(PathId(id))
    }
    /// Registers an immutable image.
    pub fn add_image(&mut self, image: Image) -> Result<ImageId, SkiaError> {
        let id = self.resource_id(self.images.len())?;
        self.images
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.images.push(image);
        Ok(ImageId(id))
    }
    /// Registers immutable shaped glyph output.
    pub fn add_glyph_run(&mut self, run: GlyphRun) -> Result<GlyphRunId, SkiaError> {
        let id = self.resource_id(self.glyph_runs.len())?;
        self.glyph_runs
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.glyph_runs.push(run);
        Ok(GlyphRunId(id))
    }
    /// Records a target-wide clear that ignores canvas state.
    pub fn clear(&mut self, color: Color) -> Result<(), SkiaError> {
        self.push(DrawCommand::Clear(color))
    }
    /// Records a canvas-state save.
    pub fn save(&mut self) -> Result<(), SkiaError> {
        self.push(DrawCommand::Save)
    }
    /// Records a canvas-state restore.
    pub fn restore(&mut self) -> Result<(), SkiaError> {
        self.push(DrawCommand::Restore)
    }
    /// Records an intersection clip rectangle.
    pub fn clip_rect(&mut self, rect: Rect) -> Result<(), SkiaError> {
        self.push(DrawCommand::ClipRect(rect))
    }
    /// Records a replacement canvas transform.
    pub fn set_transform(&mut self, transform: Transform) -> Result<(), SkiaError> {
        self.push(DrawCommand::SetTransform(transform))
    }
    /// Records an affine transform concatenation for following draws.
    pub fn concat_transform(&mut self, transform: Transform) -> Result<(), SkiaError> {
        self.push(DrawCommand::ConcatTransform(transform))
    }
    /// Records a fill of a registered path.
    pub fn fill_path(
        &mut self,
        path: PathId,
        rule: FillRule,
        paint: Paint,
    ) -> Result<(), SkiaError> {
        self.push(DrawCommand::FillPath { path, rule, paint })
    }
    /// Records a positive-width stroke of a registered path.
    pub fn stroke_path(
        &mut self,
        path: PathId,
        width: Scalar,
        paint: Paint,
    ) -> Result<(), SkiaError> {
        if width.bits() <= 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        self.push(DrawCommand::StrokePath { path, width, paint })
    }
    /// Records one registered image draw.
    pub fn draw_image(
        &mut self,
        image: ImageId,
        destination: Rect,
        opacity: u8,
        paint: Paint,
    ) -> Result<(), SkiaError> {
        self.push(DrawCommand::DrawImage {
            image,
            destination,
            opacity,
            paint,
        })
    }
    /// Records one registered shaped glyph run draw.
    pub fn draw_glyph_run(&mut self, run: GlyphRunId, paint: Paint) -> Result<(), SkiaError> {
        self.push(DrawCommand::DrawGlyphRun { run, paint })
    }
    /// Records one command.
    pub fn push(&mut self, command: DrawCommand) -> Result<(), SkiaError> {
        if self.commands.len() == self.max_items {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        self.commands
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.commands.push(command);
        Ok(())
    }
    /// Publishes the list.
    pub fn finish(self) -> DisplayList {
        DisplayList {
            commands: self.commands,
            paths: self.paths,
            images: self.images,
            glyph_runs: self.glyph_runs,
        }
    }
    fn resource_id(&self, length: usize) -> Result<u32, SkiaError> {
        if length == self.max_items {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        u32::try_from(length).map_err(|_| SkiaError::new(SkiaErrorCode::ResourceLimit))
    }
}
