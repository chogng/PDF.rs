use crate::{
    Color, FillRule, Image, Paint, Path, Rect, Scalar, SkiaError, SkiaErrorCode, Transform,
};

/// Command-buffer-local identifier for an immutable path resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PathId(u32);

/// Command-buffer-local identifier for an immutable image resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImageId(u32);

/// Backend-neutral drawing operation in declaration order.
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

/// Immutable portable drawing list and the resources it owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayList {
    commands: Vec<DrawCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
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
}

/// Bounded recorder for one immutable [`DisplayList`].
#[derive(Debug)]
pub struct DisplayListBuilder {
    commands: Vec<DrawCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
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
        }
    }
    fn resource_id(&self, length: usize) -> Result<u32, SkiaError> {
        if length == self.max_items {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        u32::try_from(length).map_err(|_| SkiaError::new(SkiaErrorCode::ResourceLimit))
    }
}
