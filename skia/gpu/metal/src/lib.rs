//! macOS Metal submission backend for `pdf-rs-skia-gpu`.
//!
//! This initial adapter creates native Metal textures and command buffers, and
//! executes `Clear` commands as real render passes. Other commands fail closed
//! until their Metal shader and resource contracts are implemented.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

use metal::{
    CommandQueue, Device, MTLClearColor, MTLCommandBufferStatus, MTLLoadAction, MTLOrigin,
    MTLPixelFormat, MTLRegion, MTLSize, MTLStorageMode, MTLStoreAction, MTLTextureUsage,
    RenderPassDescriptor, RenderPipelineDescriptor, RenderPipelineState, Texture,
    TextureDescriptor,
};
use pdf_rs_skia_core::Color;
use pdf_rs_skia_gpu::{GpuBackend, GpuCommand, GpuCommandBuffer, GpuSurfaceDescriptor};

/// Stable machine-readable Metal backend failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MetalErrorCode {
    /// The machine did not expose a default Metal device.
    DeviceUnavailable,
    /// The adapter could not allocate a render-target texture.
    SurfaceAllocationFailed,
    /// The command buffer requested a not-yet-implemented GPU operation.
    UnsupportedCommand,
    /// Metal did not complete the submitted command buffer successfully.
    SubmissionFailed,
    /// The adapter could not allocate an RGBA8 readback buffer.
    ReadbackAllocationFailed,
    /// The compiled shared Metal shader library could not be loaded.
    ShaderLibraryFailed,
    /// The solid-rectangle render pipeline could not be created.
    PipelineCreationFailed,
}

/// Source-redacted Metal backend error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MetalError {
    code: MetalErrorCode,
}

impl MetalError {
    const fn new(code: MetalErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> MetalErrorCode {
        self.code
    }
}

impl fmt::Display for MetalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for MetalError {}

/// Native Metal render target owned by [`MetalBackend`].
pub struct MetalSurface {
    texture: Texture,
    descriptor: GpuSurfaceDescriptor,
}

impl MetalSurface {
    /// Returns the portable descriptor used to allocate this target.
    pub const fn descriptor(&self) -> GpuSurfaceDescriptor {
        self.descriptor
    }

    /// Reads the completed target as tightly packed row-major RGBA8 pixels.
    ///
    /// Call this only after a successful [`GpuBackend::submit`] operation.
    pub fn read_rgba8(&self) -> Result<Vec<u8>, MetalError> {
        let bytes = u64::from(self.descriptor.width())
            .checked_mul(u64::from(self.descriptor.height()))
            .and_then(|value| value.checked_mul(4))
            .ok_or(MetalError::new(MetalErrorCode::ReadbackAllocationFailed))?;
        let length = usize::try_from(bytes)
            .map_err(|_| MetalError::new(MetalErrorCode::ReadbackAllocationFailed))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| MetalError::new(MetalErrorCode::ReadbackAllocationFailed))?;
        output.resize(length, 0);
        self.texture.get_bytes(
            output.as_mut_ptr().cast(),
            u64::from(self.descriptor.width()) * 4,
            MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: u64::from(self.descriptor.width()),
                    height: u64::from(self.descriptor.height()),
                    depth: 1,
                },
            },
            0,
        );
        Ok(output)
    }
}

/// macOS Metal device and queue implementing the portable GPU contract.
pub struct MetalBackend {
    device: Device,
    queue: CommandQueue,
    solid_rect_pipeline: RenderPipelineState,
}

impl MetalBackend {
    /// Opens the default system Metal device and one persistent command queue.
    pub fn new() -> Result<Self, MetalError> {
        let device =
            Device::system_default().ok_or(MetalError::new(MetalErrorCode::DeviceUnavailable))?;
        let queue = device.new_command_queue();
        let library = device
            .new_library_with_file(env!("PDF_RS_SKIA_METAL_LIBRARY"))
            .map_err(|_| MetalError::new(MetalErrorCode::ShaderLibraryFailed))?;
        let vertex = library
            .get_function("skia_solid_rect_vertex", None)
            .map_err(|_| MetalError::new(MetalErrorCode::ShaderLibraryFailed))?;
        let fragment = library
            .get_function("skia_solid_rect_fragment", None)
            .map_err(|_| MetalError::new(MetalErrorCode::ShaderLibraryFailed))?;
        let descriptor = RenderPipelineDescriptor::new();
        descriptor.set_vertex_function(Some(&vertex));
        descriptor.set_fragment_function(Some(&fragment));
        descriptor
            .color_attachments()
            .object_at(0)
            .ok_or(MetalError::new(MetalErrorCode::PipelineCreationFailed))?
            .set_pixel_format(MTLPixelFormat::RGBA8Unorm);
        let solid_rect_pipeline = device
            .new_render_pipeline_state(&descriptor)
            .map_err(|_| MetalError::new(MetalErrorCode::PipelineCreationFailed))?;
        Ok(Self {
            device,
            queue,
            solid_rect_pipeline,
        })
    }
}

impl GpuBackend for MetalBackend {
    type Surface = MetalSurface;
    type Error = MetalError;

    fn create_surface(
        &mut self,
        descriptor: GpuSurfaceDescriptor,
    ) -> Result<Self::Surface, Self::Error> {
        let texture_descriptor = TextureDescriptor::new();
        texture_descriptor.set_width(u64::from(descriptor.width()));
        texture_descriptor.set_height(u64::from(descriptor.height()));
        texture_descriptor.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
        texture_descriptor.set_usage(MTLTextureUsage::RenderTarget);
        texture_descriptor.set_storage_mode(MTLStorageMode::Managed);
        let texture = self.device.new_texture(&texture_descriptor);
        Ok(MetalSurface {
            texture,
            descriptor,
        })
    }

    fn submit(
        &mut self,
        surface: &mut Self::Surface,
        commands: &GpuCommandBuffer,
    ) -> Result<(), Self::Error> {
        // Keep the validated shader pipeline resident for subsequent FillRect commands.
        let _solid_rect_pipeline = &self.solid_rect_pipeline;
        let mut clear = None;
        for command in commands.commands() {
            match command {
                GpuCommand::Clear(color) => clear = Some(*color),
                GpuCommand::FillRect { .. }
                | GpuCommand::FillPath { .. }
                | GpuCommand::DrawImage { .. } => {
                    return Err(MetalError::new(MetalErrorCode::UnsupportedCommand));
                }
            }
        }
        let Some(clear) = clear else {
            return Ok(());
        };
        let descriptor = RenderPassDescriptor::new();
        let attachment = descriptor
            .color_attachments()
            .object_at(0)
            .ok_or(MetalError::new(MetalErrorCode::SurfaceAllocationFailed))?;
        attachment.set_texture(Some(&surface.texture));
        attachment.set_load_action(MTLLoadAction::Clear);
        attachment.set_store_action(MTLStoreAction::Store);
        attachment.set_clear_color(clear_color(clear));

        let command_buffer = self.queue.new_command_buffer();
        let encoder = command_buffer.new_render_command_encoder(descriptor);
        encoder.end_encoding();
        let blit = command_buffer.new_blit_command_encoder();
        blit.synchronize_resource(&surface.texture);
        blit.end_encoding();
        command_buffer.commit();
        command_buffer.wait_until_completed();
        if command_buffer.status() != MTLCommandBufferStatus::Completed {
            return Err(MetalError::new(MetalErrorCode::SubmissionFailed));
        }
        Ok(())
    }
}

fn clear_color(color: Color) -> MTLClearColor {
    let [red, green, blue, alpha] = color.channels();
    let scale = f64::from(u8::MAX);
    MTLClearColor::new(
        f64::from(red) / scale,
        f64::from(green) / scale,
        f64::from(blue) / scale,
        f64::from(alpha) / scale,
    )
}
