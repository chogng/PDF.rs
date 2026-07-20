//! Deterministic CPU replay for validating GPU command-buffer semantics.
//!
//! This module is not a hardware GPU implementation. It exists so Metal,
//! Vulkan, and WebGPU adapters can be compared with one unambiguous execution
//! of the same [`crate::GpuCommandBuffer`].

use std::fmt;

use pdf_rs_skia_core::{Rect, SkiaError, Transform};
use pdf_rs_skia_cpu::{Canvas, ClipRect, Surface, SurfaceLimits};

use crate::{GpuBackend, GpuCommand, GpuCommandBuffer, GpuSurfaceDescriptor};

/// Source-redacted failure from deterministic command replay.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SoftwareGpuError;

impl fmt::Display for SoftwareGpuError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("software GPU command replay failed")
    }
}

impl std::error::Error for SoftwareGpuError {}

/// CPU reference implementation of [`GpuBackend`] for conformance testing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SoftwareGpuBackend {
    limits: SurfaceLimits,
}

impl SoftwareGpuBackend {
    /// Creates one replay backend using explicit CPU surface limits.
    pub const fn new(limits: SurfaceLimits) -> Self {
        Self { limits }
    }
}

impl Default for SoftwareGpuBackend {
    fn default() -> Self {
        Self::new(SurfaceLimits::default())
    }
}

impl GpuBackend for SoftwareGpuBackend {
    type Surface = Surface;
    type Error = SoftwareGpuError;

    fn create_surface(
        &mut self,
        descriptor: GpuSurfaceDescriptor,
    ) -> Result<Self::Surface, Self::Error> {
        Surface::new(descriptor.width(), descriptor.height(), self.limits).map_err(map_error)
    }

    fn submit(
        &mut self,
        surface: &mut Self::Surface,
        commands: &GpuCommandBuffer,
    ) -> Result<(), Self::Error> {
        let mut canvas = surface.canvas();
        for command in commands.commands() {
            match command {
                GpuCommand::Clear(color) => canvas.clear(*color),
                GpuCommand::FillRect {
                    rect,
                    paint,
                    transform,
                    clip,
                } => with_state(&mut canvas, *transform, *clip, |canvas| {
                    canvas.fill_rect(*rect, *paint)
                })?,
                GpuCommand::FillPath {
                    path,
                    rule,
                    paint,
                    transform,
                    clip,
                } => {
                    let path = commands.path(*path).ok_or(SoftwareGpuError)?;
                    with_state(&mut canvas, *transform, *clip, |canvas| {
                        canvas.fill_path(path, *rule, *paint)
                    })?
                }
                GpuCommand::DrawImage {
                    image,
                    destination,
                    opacity,
                    blend_mode,
                    transform,
                    clip,
                } => {
                    let image = commands.image(*image).ok_or(SoftwareGpuError)?;
                    with_state(&mut canvas, *transform, *clip, |canvas| {
                        canvas.draw_image(image, *destination, *opacity, *blend_mode)
                    })?
                }
            }
        }
        Ok(())
    }
}

fn with_state(
    canvas: &mut Canvas<'_>,
    transform: Transform,
    clip: Option<Rect>,
    draw: impl FnOnce(&mut Canvas<'_>) -> Result<(), SkiaError>,
) -> Result<(), SoftwareGpuError> {
    canvas.save().map_err(map_error)?;
    let result = (|| {
        if let Some(clip) = clip {
            canvas.set_transform(Transform::IDENTITY);
            canvas.clip_rect(ClipRect::new(clip))?;
        }
        canvas.set_transform(transform);
        draw(canvas)
    })();
    let restore = canvas.restore();
    result.and(restore).map_err(map_error)
}

fn map_error(_: SkiaError) -> SoftwareGpuError {
    SoftwareGpuError
}
