//! Backend-neutral GPU submission contracts for `pdf-rs-skia`.
//!
//! This crate deliberately contains no Metal, Vulkan, OpenGL, WebGPU, window,
//! thread, or foreign-function binding. Product-specific backend crates own
//! those details and implement [`GpuBackend`]. The command buffer is reusable
//! by PDF renderers, editors, and other clients without coupling them to a
//! particular graphics API.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

/// Deterministic CPU replay backend for GPU command conformance tests.
#[cfg(feature = "software")]
pub mod software;

use std::fmt;

use pdf_rs_skia_core::{BlendMode, Color, FillRule, Paint, Path, Point, Rect, Transform};
use pdf_rs_skia_image::Image;

/// Stable machine-readable GPU command recording failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GpuCommandErrorCode {
    /// A command-buffer ceiling is invalid.
    InvalidLimits,
    /// A GPU surface descriptor has an empty dimension.
    InvalidSurface,
    /// A state restore was requested without a matching save.
    RestoreUnderflow,
    /// The operation needs an unsupported transform mode.
    UnsupportedTransform,
    /// A transform or intermediate geometry calculation overflowed.
    NumericOverflow,
    /// Recording would exceed a configured ceiling.
    ResourceLimit,
    /// A command referred to a resource that is not registered in this encoder.
    InvalidResource,
    /// Recording could not reserve command storage.
    AllocationFailed,
}

/// Source-redacted GPU command recording error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuCommandError {
    code: GpuCommandErrorCode,
}

impl GpuCommandError {
    const fn new(code: GpuCommandErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> GpuCommandErrorCode {
        self.code
    }
}

impl fmt::Display for GpuCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for GpuCommandError {}

/// Bounded dimensions of a GPU render target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuSurfaceDescriptor {
    width: u32,
    height: u32,
}

impl GpuSurfaceDescriptor {
    /// Creates a non-empty GPU render-target descriptor.
    pub fn new(width: u32, height: u32) -> Result<Self, GpuCommandError> {
        if width == 0 || height == 0 {
            return Err(GpuCommandError::new(GpuCommandErrorCode::InvalidSurface));
        }
        Ok(Self { width, height })
    }

    /// Returns the target width in physical pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Returns the target height in physical pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// Opaque, command-buffer-local identifier for one immutable vector path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuPathId(u32);

/// Opaque, command-buffer-local identifier for one immutable RGBA8 image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuImageId(u32);

/// Independent command, resource, and state-stack ceilings for one encoder.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GpuCommandLimits {
    max_commands: usize,
    max_paths: usize,
    max_images: usize,
    max_save_depth: usize,
}

impl GpuCommandLimits {
    /// Creates positive, bounded limits for one command encoder.
    pub fn new(
        max_commands: usize,
        max_paths: usize,
        max_images: usize,
        max_save_depth: usize,
    ) -> Result<Self, GpuCommandError> {
        if max_commands == 0 || max_paths == 0 || max_images == 0 || max_save_depth == 0 {
            return Err(GpuCommandError::new(GpuCommandErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_commands,
            max_paths,
            max_images,
            max_save_depth,
        })
    }
}

/// One backend-neutral GPU drawing command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GpuCommand {
    /// Clears the full render target, without inheriting prior state.
    Clear(Color),
    /// Fills one axis-aligned logical rectangle.
    FillRect {
        /// Logical rectangle to fill.
        rect: Rect,
        /// Immutable source paint.
        paint: Paint,
        /// Logical-to-target transform selected when the command was recorded.
        transform: Transform,
        /// Target-space scissor rectangle, if clipping is active.
        clip: Option<Rect>,
    },
    /// Fills one registered vector path.
    FillPath {
        /// Path resource local to this command buffer.
        path: GpuPathId,
        /// Containment rule for the path's contours.
        rule: FillRule,
        /// Immutable source paint.
        paint: Paint,
        /// Logical-to-target transform selected when the command was recorded.
        transform: Transform,
        /// Target-space scissor rectangle, if clipping is active.
        clip: Option<Rect>,
    },
    /// Draws one registered image into a logical rectangle.
    DrawImage {
        /// Image resource local to this command buffer.
        image: GpuImageId,
        /// Logical destination rectangle.
        destination: Rect,
        /// Additional straight-alpha opacity multiplier.
        opacity: u8,
        /// Compositing operation for the source image.
        blend_mode: BlendMode,
        /// Logical-to-target transform selected when the command was recorded.
        transform: Transform,
        /// Target-space scissor rectangle, if clipping is active.
        clip: Option<Rect>,
    },
}

/// Immutable, ordered GPU command buffer with locally owned resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GpuCommandBuffer {
    commands: Vec<GpuCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
}

impl GpuCommandBuffer {
    /// Borrows commands in submission order.
    pub fn commands(&self) -> &[GpuCommand] {
        &self.commands
    }

    /// Resolves a path resource referenced by a command in this buffer.
    pub fn path(&self, id: GpuPathId) -> Option<&Path> {
        self.paths.get(usize::try_from(id.0).ok()?)
    }

    /// Resolves an image resource referenced by a command in this buffer.
    pub fn image(&self, id: GpuImageId) -> Option<&Image> {
        self.images.get(usize::try_from(id.0).ok()?)
    }
}

/// Bounded recorder for an immutable GPU command buffer.
#[derive(Debug)]
pub struct GpuCommandEncoder {
    commands: Vec<GpuCommand>,
    paths: Vec<Path>,
    images: Vec<Image>,
    limits: GpuCommandLimits,
    state: GpuState,
    saves: Vec<GpuState>,
}

#[derive(Clone, Copy, Debug)]
struct GpuState {
    transform: Transform,
    clip: ClipState,
}

#[derive(Clone, Copy, Debug)]
enum ClipState {
    Unbounded,
    Empty,
    Rect(Rect),
}

impl GpuCommandEncoder {
    /// Creates an encoder with a positive command and per-kind resource ceiling.
    pub fn new(max_commands: usize) -> Result<Self, GpuCommandError> {
        Self::with_limits(GpuCommandLimits::new(
            max_commands,
            max_commands,
            max_commands,
            max_commands,
        )?)
    }

    /// Creates an encoder with independent command, resource, and stack ceilings.
    pub fn with_limits(limits: GpuCommandLimits) -> Result<Self, GpuCommandError> {
        Ok(Self {
            commands: Vec::new(),
            paths: Vec::new(),
            images: Vec::new(),
            limits,
            state: GpuState {
                transform: Transform::IDENTITY,
                clip: ClipState::Unbounded,
            },
            saves: Vec::new(),
        })
    }

    /// Replaces the logical-to-target transform recorded into following draw commands.
    pub fn set_transform(&mut self, transform: Transform) {
        self.state.transform = transform;
    }

    /// Concatenates an affine transform onto the following draw commands.
    pub fn concat_transform(&mut self, transform: Transform) -> Result<(), GpuCommandError> {
        self.state.transform = self
            .state
            .transform
            .concat(transform)
            .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::NumericOverflow))?;
        Ok(())
    }

    /// Saves the current transform and target-space scissor state.
    pub fn save(&mut self) -> Result<(), GpuCommandError> {
        if self.saves.len() == self.limits.max_save_depth {
            return Err(GpuCommandError::new(GpuCommandErrorCode::ResourceLimit));
        }
        self.saves
            .try_reserve(1)
            .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::AllocationFailed))?;
        self.saves.push(self.state);
        Ok(())
    }

    /// Restores the most recently saved transform and target-space scissor state.
    pub fn restore(&mut self) -> Result<(), GpuCommandError> {
        self.state = self
            .saves
            .pop()
            .ok_or(GpuCommandError::new(GpuCommandErrorCode::RestoreUnderflow))?;
        Ok(())
    }

    /// Intersects the active target-space scissor with one axis-aligned transformed rectangle.
    pub fn clip_rect(&mut self, rect: Rect) -> Result<(), GpuCommandError> {
        if !self.state.transform.is_axis_aligned() {
            return Err(GpuCommandError::new(
                GpuCommandErrorCode::UnsupportedTransform,
            ));
        }
        let clip = map_axis_aligned_rect(self.state.transform, rect)?;
        self.state.clip = match self.state.clip {
            ClipState::Unbounded => ClipState::Rect(clip),
            ClipState::Empty => ClipState::Empty,
            ClipState::Rect(current) => intersect_rect(current, clip)
                .map(ClipState::Rect)
                .unwrap_or(ClipState::Empty),
        };
        Ok(())
    }

    /// Registers an immutable vector path and returns its command-buffer-local ID.
    pub fn add_path(&mut self, path: Path) -> Result<GpuPathId, GpuCommandError> {
        let id = resource_id(self.paths.len(), self.limits.max_paths)?;
        self.paths
            .try_reserve(1)
            .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::AllocationFailed))?;
        self.paths.push(path);
        Ok(GpuPathId(id))
    }

    /// Registers an immutable image and returns its command-buffer-local ID.
    pub fn add_image(&mut self, image: Image) -> Result<GpuImageId, GpuCommandError> {
        let id = resource_id(self.images.len(), self.limits.max_images)?;
        self.images
            .try_reserve(1)
            .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::AllocationFailed))?;
        self.images.push(image);
        Ok(GpuImageId(id))
    }

    /// Records a full-target clear.
    pub fn clear(&mut self, color: Color) -> Result<(), GpuCommandError> {
        self.push_unclipped(GpuCommand::Clear(color))
    }

    /// Records one logical rectangle fill.
    pub fn fill_rect(&mut self, rect: Rect, paint: Paint) -> Result<(), GpuCommandError> {
        self.push(GpuCommand::FillRect {
            rect,
            paint,
            transform: self.state.transform,
            clip: self.clip(),
        })
    }

    /// Records a fill of a registered path.
    pub fn fill_path(
        &mut self,
        path: GpuPathId,
        rule: FillRule,
        paint: Paint,
    ) -> Result<(), GpuCommandError> {
        if self.path(path).is_none() {
            return Err(GpuCommandError::new(GpuCommandErrorCode::InvalidResource));
        }
        self.push(GpuCommand::FillPath {
            path,
            rule,
            paint,
            transform: self.state.transform,
            clip: self.clip(),
        })
    }

    /// Records one draw of a registered RGBA8 image.
    pub fn draw_image(
        &mut self,
        image: GpuImageId,
        destination: Rect,
        opacity: u8,
        blend_mode: BlendMode,
    ) -> Result<(), GpuCommandError> {
        if self.image(image).is_none() {
            return Err(GpuCommandError::new(GpuCommandErrorCode::InvalidResource));
        }
        self.push(GpuCommand::DrawImage {
            image,
            destination,
            opacity,
            blend_mode,
            transform: self.state.transform,
            clip: self.clip(),
        })
    }

    /// Publishes commands and their owned resources for later submission.
    pub fn finish(self) -> GpuCommandBuffer {
        GpuCommandBuffer {
            commands: self.commands,
            paths: self.paths,
            images: self.images,
        }
    }

    fn push(&mut self, command: GpuCommand) -> Result<(), GpuCommandError> {
        if matches!(self.state.clip, ClipState::Empty) {
            return Ok(());
        }
        self.push_unclipped(command)
    }

    fn push_unclipped(&mut self, command: GpuCommand) -> Result<(), GpuCommandError> {
        if self.commands.len() == self.limits.max_commands {
            return Err(GpuCommandError::new(GpuCommandErrorCode::ResourceLimit));
        }
        self.commands
            .try_reserve(1)
            .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::AllocationFailed))?;
        self.commands.push(command);
        Ok(())
    }

    fn path(&self, id: GpuPathId) -> Option<&Path> {
        self.paths.get(usize::try_from(id.0).ok()?)
    }

    fn image(&self, id: GpuImageId) -> Option<&Image> {
        self.images.get(usize::try_from(id.0).ok()?)
    }

    fn clip(&self) -> Option<Rect> {
        match self.state.clip {
            ClipState::Unbounded => None,
            ClipState::Empty => None,
            ClipState::Rect(rect) => Some(rect),
        }
    }
}

fn resource_id(length: usize, max_resources: usize) -> Result<u32, GpuCommandError> {
    if length == max_resources {
        return Err(GpuCommandError::new(GpuCommandErrorCode::ResourceLimit));
    }
    u32::try_from(length).map_err(|_| GpuCommandError::new(GpuCommandErrorCode::ResourceLimit))
}

fn map_axis_aligned_rect(transform: Transform, rect: Rect) -> Result<Rect, GpuCommandError> {
    let first = transform
        .map_point(Point::new(rect.left(), rect.top()))
        .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::NumericOverflow))?;
    let second = transform
        .map_point(Point::new(rect.right(), rect.bottom()))
        .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::NumericOverflow))?;
    let left = first.x().min(second.x());
    let top = first.y().min(second.y());
    let right = first.x().max(second.x());
    let bottom = first.y().max(second.y());
    Rect::new(left, top, right, bottom)
        .map_err(|_| GpuCommandError::new(GpuCommandErrorCode::NumericOverflow))
}

fn intersect_rect(first: Rect, second: Rect) -> Option<Rect> {
    Rect::new(
        first.left().max(second.left()),
        first.top().max(second.top()),
        first.right().min(second.right()),
        first.bottom().min(second.bottom()),
    )
    .ok()
}

/// Backend-specific error contract for GPU device operations.
pub trait GpuBackendError: std::error::Error + Send + Sync + 'static {}

impl<T> GpuBackendError for T where T: std::error::Error + Send + Sync + 'static {}

/// Platform-specific implementation of GPU surface allocation and submission.
///
/// Backends must validate device limits, resource ownership, and command support
/// before submission. This crate does not make a GPU backend authoritative for
/// the CPU reference rasterizer's pixels.
pub trait GpuBackend {
    /// Opaque backend-owned surface or texture target.
    type Surface;
    /// Backend-specific, source-redacted operational failure.
    type Error: GpuBackendError;

    /// Allocates one backend-owned render target.
    fn create_surface(
        &mut self,
        descriptor: GpuSurfaceDescriptor,
    ) -> Result<Self::Surface, Self::Error>;

    /// Submits one immutable command buffer to an existing target.
    fn submit(
        &mut self,
        surface: &mut Self::Surface,
        commands: &GpuCommandBuffer,
    ) -> Result<(), Self::Error>;
}
