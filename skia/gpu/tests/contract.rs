use std::fmt;

use pdf_rs_skia_core::{
    BlendMode, Color, FillRule, Paint, PathBuilder, Point, Rect, Scalar, Transform,
};
use pdf_rs_skia_gpu::{
    GpuBackend, GpuCommand, GpuCommandEncoder, GpuCommandErrorCode, GpuCommandLimits,
    GpuSurfaceDescriptor, software::SoftwareGpuBackend,
};
use pdf_rs_skia_image::Image;

fn scalar(value: i32) -> Scalar {
    Scalar::from_i32(value).unwrap()
}

fn point(x: i32, y: i32) -> Point {
    Point::new(scalar(x), scalar(y))
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect::new(scalar(left), scalar(top), scalar(right), scalar(bottom)).unwrap()
}

#[derive(Debug)]
struct BackendError;

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("backend failure")
    }
}

impl std::error::Error for BackendError {}

#[derive(Default)]
struct RecordingBackend {
    submitted: Vec<GpuCommand>,
}

impl GpuBackend for RecordingBackend {
    type Surface = GpuSurfaceDescriptor;
    type Error = BackendError;

    fn create_surface(
        &mut self,
        descriptor: GpuSurfaceDescriptor,
    ) -> Result<Self::Surface, Self::Error> {
        Ok(descriptor)
    }

    fn submit(
        &mut self,
        _surface: &mut Self::Surface,
        commands: &pdf_rs_skia_gpu::GpuCommandBuffer,
    ) -> Result<(), Self::Error> {
        self.submitted.extend_from_slice(commands.commands());
        Ok(())
    }
}

#[test]
fn gpu_commands_own_resources_and_preserve_submission_order() {
    let mut path = PathBuilder::new(3).unwrap();
    path.move_to(point(0, 0)).unwrap();
    path.line_to(point(2, 0)).unwrap();
    let path = path.finish().unwrap();
    let image = Image::from_rgba8(1, 1, vec![0, 255, 0, 255]).unwrap();

    let mut encoder = GpuCommandEncoder::new(4).unwrap();
    let path = encoder.add_path(path).unwrap();
    let image = encoder.add_image(image).unwrap();
    encoder.clear(Color::WHITE).unwrap();
    encoder.set_transform(Transform::translate(scalar(1), scalar(2)));
    encoder
        .fill_path(path, FillRule::NonZero, Paint::new(Color::BLACK))
        .unwrap();
    encoder
        .draw_image(image, rect(2, 3, 4, 5), 128, BlendMode::SourceOver)
        .unwrap();
    let commands = encoder.finish();

    let mut backend = RecordingBackend::default();
    let mut surface = backend
        .create_surface(GpuSurfaceDescriptor::new(12, 8).unwrap())
        .unwrap();
    backend.submit(&mut surface, &commands).unwrap();

    assert_eq!(surface.width(), 12);
    assert_eq!(surface.height(), 8);
    assert!(matches!(
        backend.submitted[0],
        GpuCommand::Clear(Color::WHITE)
    ));
    assert!(matches!(backend.submitted[1], GpuCommand::FillPath { .. }));
    assert!(matches!(backend.submitted[2], GpuCommand::DrawImage { .. }));
    let GpuCommand::FillPath { transform, .. } = &backend.submitted[1] else {
        panic!("expected path command");
    };
    assert_eq!(transform.map_point(point(1, 1)).unwrap(), point(2, 3));
}

#[test]
fn gpu_contract_rejects_invalid_resource_descriptors_and_limits() {
    assert_eq!(
        GpuSurfaceDescriptor::new(0, 1).unwrap_err().code(),
        GpuCommandErrorCode::InvalidSurface
    );
    assert_eq!(
        GpuCommandEncoder::new(0).unwrap_err().code(),
        GpuCommandErrorCode::InvalidLimits
    );
}

#[test]
fn gpu_encoder_scopes_transform_clip_and_resource_limits() {
    let limits = GpuCommandLimits::new(3, 1, 1, 1).unwrap();
    let mut encoder = GpuCommandEncoder::with_limits(limits).unwrap();
    encoder.save().unwrap();
    encoder.set_transform(Transform::translate(scalar(3), scalar(4)));
    encoder.clip_rect(rect(0, 0, 2, 2)).unwrap();
    encoder
        .fill_rect(rect(0, 0, 4, 4), Paint::new(Color::BLACK))
        .unwrap();
    encoder.restore().unwrap();
    encoder
        .fill_rect(rect(0, 0, 1, 1), Paint::new(Color::WHITE))
        .unwrap();

    let mut path = PathBuilder::new(2).unwrap();
    path.move_to(point(0, 0)).unwrap();
    let path = path.finish().unwrap();
    encoder.add_path(path.clone()).unwrap();
    assert_eq!(
        encoder.add_path(path).unwrap_err().code(),
        GpuCommandErrorCode::ResourceLimit
    );
    assert_eq!(
        encoder.restore().unwrap_err().code(),
        GpuCommandErrorCode::RestoreUnderflow
    );

    let commands = encoder.finish();
    let GpuCommand::FillRect {
        transform, clip, ..
    } = &commands.commands()[0]
    else {
        panic!("expected clipped rectangle command");
    };
    assert_eq!(transform.map_point(point(0, 0)).unwrap(), point(3, 4));
    assert_eq!(*clip, Some(rect(3, 4, 5, 6)));
    let GpuCommand::FillRect {
        transform, clip, ..
    } = &commands.commands()[1]
    else {
        panic!("expected restored rectangle command");
    };
    assert_eq!(*transform, Transform::IDENTITY);
    assert_eq!(*clip, None);
}

#[test]
fn gpu_empty_scissors_skip_draws_but_not_target_clears() {
    let mut encoder = GpuCommandEncoder::new(2).unwrap();
    encoder.clip_rect(rect(0, 0, 1, 1)).unwrap();
    encoder.clip_rect(rect(2, 2, 3, 3)).unwrap();
    encoder
        .fill_rect(rect(0, 0, 3, 3), Paint::new(Color::BLACK))
        .unwrap();
    encoder.clear(Color::WHITE).unwrap();
    let commands = encoder.finish();
    assert_eq!(commands.commands(), &[GpuCommand::Clear(Color::WHITE)]);
}

#[test]
fn software_replay_is_a_pixel_oracle_for_gpu_command_state() {
    let image = Image::from_rgba8(1, 1, vec![255, 0, 0, 255]).unwrap();
    let mut encoder = GpuCommandEncoder::new(4).unwrap();
    let image = encoder.add_image(image).unwrap();
    encoder.clear(Color::BLACK).unwrap();
    encoder.save().unwrap();
    encoder.set_transform(Transform::translate(scalar(1), scalar(0)));
    encoder.clip_rect(rect(0, 0, 2, 1)).unwrap();
    encoder
        .fill_rect(rect(0, 0, 3, 1), Paint::new(Color::rgba(0, 0, 255, 255)))
        .unwrap();
    encoder.restore().unwrap();
    encoder
        .draw_image(image, rect(0, 1, 1, 2), 255, BlendMode::SourceOver)
        .unwrap();
    let commands = encoder.finish();

    let mut backend = SoftwareGpuBackend::default();
    let mut surface = backend
        .create_surface(GpuSurfaceDescriptor::new(4, 2).unwrap())
        .unwrap();
    backend.submit(&mut surface, &commands).unwrap();

    assert_eq!(pixel(&surface, 0, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&surface, 1, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(&surface, 2, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(&surface, 3, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&surface, 0, 1), [255, 0, 0, 255]);
}

fn pixel(surface: &pdf_rs_skia_cpu::Surface, x: usize, y: usize) -> [u8; 4] {
    let offset = (y * surface.width() as usize + x) * 4;
    surface.pixels()[offset..offset + 4].try_into().unwrap()
}
