use pdf_rs_skia_core::{Color, Paint, Rect, Scalar};
use pdf_rs_skia_gpu::{GpuBackend, GpuCommandEncoder, GpuSurfaceDescriptor};
use pdf_rs_skia_metal::{MetalBackend, MetalErrorCode};

#[test]
fn metal_backend_allocates_a_native_surface_and_submits_a_clear() {
    let Some(mut backend) = backend_or_skip() else {
        return;
    };
    let mut surface = backend
        .create_surface(GpuSurfaceDescriptor::new(4, 4).unwrap())
        .unwrap();
    let mut commands = GpuCommandEncoder::new(1).unwrap();
    commands.clear(Color::rgba(10, 20, 30, 255)).unwrap();
    backend.submit(&mut surface, &commands.finish()).unwrap();
    assert_eq!(surface.descriptor().width(), 4);
    let pixels = surface.read_rgba8().unwrap();
    assert_eq!(&pixels[0..4], &[10, 20, 30, 255]);
    assert_eq!(&pixels[60..64], &[10, 20, 30, 255]);
}

#[test]
fn metal_backend_fails_closed_for_unimplemented_draw_commands() {
    let Some(mut backend) = backend_or_skip() else {
        return;
    };
    let mut surface = backend
        .create_surface(GpuSurfaceDescriptor::new(4, 4).unwrap())
        .unwrap();
    let mut commands = GpuCommandEncoder::new(1).unwrap();
    commands
        .fill_rect(
            Rect::new(
                Scalar::from_i32(0).unwrap(),
                Scalar::from_i32(0).unwrap(),
                Scalar::from_i32(1).unwrap(),
                Scalar::from_i32(1).unwrap(),
            )
            .unwrap(),
            Paint::new(Color::BLACK),
        )
        .unwrap();
    assert_eq!(
        backend
            .submit(&mut surface, &commands.finish())
            .unwrap_err()
            .code(),
        MetalErrorCode::UnsupportedCommand
    );
}

fn backend_or_skip() -> Option<MetalBackend> {
    match MetalBackend::new() {
        Ok(backend) => Some(backend),
        Err(error) if error.code() == MetalErrorCode::DeviceUnavailable => None,
        Err(error) => panic!("unexpected Metal initialization failure: {error}"),
    }
}
