#![allow(
    dead_code,
    reason = "the shared staged geometry support exposes kernels exercised by its companion test"
)]

#[path = "reference_image_support/mod.rs"]
mod reference;

use std::cell::Cell;

use pdf_rs_raster::reference::{NormalizedQ16, PremultipliedRgbaQ16, ReferenceSrgbQ16};
use pdf_rs_scene::{
    BlendMode, GraphicsResourceSource, ImageColorSpace, ImageResource, Matrix, PageGeometry,
    PageRotation, SceneRect, SceneScalar, SceneUnit,
};
use pdf_rs_syntax::ObjectRef;

use reference::coverage::CoverageMask;
use reference::geometry::{
    GeometryCancellation, GeometryFailure, GeometryLimitKind, GeometryLimits, GeometryWork,
};
use reference::image::{
    ImageCancellation, ImageFailure, ImageLimitKind, ImageLimits, ImageRaster, ImageStats,
    paint_image, rasterize_image, unit_index,
};

struct NeverCancel;

impl ImageCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl GeometryCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn geometry_limit_conversion_preserves_the_exact_typed_dimension() {
    let failure = GeometryFailure::Limit {
        kind: GeometryLimitKind::Samples,
        limit: 17,
        consumed: 16,
        attempted: 2,
    };
    assert_eq!(
        ImageFailure::from(failure),
        ImageFailure::GeometryLimit {
            kind: GeometryLimitKind::Samples,
            limit: 17,
            consumed: 16,
            attempted: 2,
        }
    );
}

struct CancelAtCheck {
    checks: Cell<u64>,
    cancel_at: u64,
}

impl ImageCancellation for CancelAtCheck {
    fn is_cancelled(&self) -> bool {
        let next = self.checks.get() + 1;
        self.checks.set(next);
        next >= self.cancel_at
    }
}

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).unwrap()
}

fn geometry() -> PageGeometry {
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        SceneScalar::ONE,
    ])
    .unwrap();
    PageGeometry::new(bounds, bounds, PageRotation::Degrees0)
}

fn rotated_geometry(rotation: PageRotation) -> PageGeometry {
    let geometry = geometry();
    PageGeometry::new(geometry.media_box(), geometry.crop_box(), rotation)
}

fn matrix(values: [&str; 6]) -> Matrix {
    Matrix::new(values.map(scalar))
}

fn image(
    width: u32,
    height: u32,
    color_space: ImageColorSpace,
    interpolate: bool,
    decoded: Vec<u8>,
) -> ImageResource {
    ImageResource::new(
        GraphicsResourceSource::new(ObjectRef::new(9, 0).unwrap(), 42, 7),
        width,
        height,
        color_space,
        8,
        interpolate,
        decoded,
    )
    .unwrap()
}

fn opaque(color: ReferenceSrgbQ16) -> PremultipliedRgbaQ16 {
    color.with_constant_alpha(NormalizedQ16::ONE)
}

fn white() -> PremultipliedRgbaQ16 {
    opaque(ReferenceSrgbQ16::gray(NormalizedQ16::ONE))
}

fn black() -> PremultipliedRgbaQ16 {
    opaque(ReferenceSrgbQ16::gray(NormalizedQ16::ZERO))
}

fn raster(
    image: &ImageResource,
    transform: Matrix,
    width: u32,
    height: u32,
    backdrop: &[PremultipliedRgbaQ16],
) -> Result<ImageRaster, ImageFailure> {
    rasterize_image(
        image,
        geometry(),
        transform,
        width,
        height,
        SceneUnit::ONE,
        BlendMode::Normal,
        backdrop,
        None,
        ImageLimits::default(),
        &NeverCancel,
    )
}

fn rgba(raster: &ImageRaster) -> Vec<[u8; 4]> {
    raster
        .pixels()
        .iter()
        .copied()
        .map(PremultipliedRgbaQ16::to_straight_rgba8)
        .collect()
}

#[test]
fn one_pixel_gray_and_cmyk_conversion_are_literal() {
    let gray = image(1, 1, ImageColorSpace::DeviceGray, false, vec![0]);
    let gray = raster(&gray, Matrix::IDENTITY, 1, 1, &[white()]).unwrap();
    assert_eq!(ImageRaster::PROFILE, "reference-image-v1");
    assert_eq!((gray.width(), gray.height()), (1, 1));
    assert_eq!(rgba(&gray), vec![[0, 0, 0, 255]]);
    assert_eq!(gray.stats().source_pixels(), 1);
    assert_eq!(gray.stats().stride_bytes(), 1);
    assert_eq!(gray.stats().decoded_bytes(), 1);
    assert_eq!(gray.stats().output_pixels(), 1);
    assert_eq!(gray.stats().samples(), 64);
    assert_eq!(gray.stats().conversions(), 64);
    assert_eq!(gray.stats().fuel(), 130);
    assert_eq!(gray.stats().cancellation_checks(), 3);

    let cmyk = image(
        1,
        1,
        ImageColorSpace::DeviceCmyk,
        false,
        vec![0, 255, 255, 0],
    );
    assert_eq!(
        rgba(&raster(&cmyk, Matrix::IDENTITY, 1, 1, &[white()]).unwrap()),
        vec![[255, 0, 0, 255]]
    );
}

#[test]
fn two_by_two_top_row_orientation_flip_and_page_rotations_are_exact() {
    let image = image(
        2,
        2,
        ImageColorSpace::DeviceRgb,
        false,
        vec![
            255, 0, 0, 0, 255, 0, // top: red, green
            0, 0, 255, 255, 255, 255, // bottom: blue, white
        ],
    );
    let background = vec![white(); 4];
    assert_eq!(
        rgba(&raster(&image, Matrix::IDENTITY, 2, 2, &background).unwrap()),
        vec![
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ]
    );

    let flip = matrix(["-1", "0", "0", "1", "1", "0"]);
    assert_eq!(
        rgba(&raster(&image, flip, 2, 2, &background).unwrap()),
        vec![
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 255, 255],
        ]
    );

    let rotated = rasterize_image(
        &image,
        rotated_geometry(PageRotation::Degrees90),
        Matrix::IDENTITY,
        2,
        2,
        SceneUnit::ONE,
        BlendMode::Normal,
        &background,
        None,
        ImageLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(
        rgba(&rotated),
        vec![
            [0, 0, 255, 255],
            [255, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 255, 0, 255],
        ]
    );

    let rotated = |rotation| {
        rasterize_image(
            &image,
            rotated_geometry(rotation),
            Matrix::IDENTITY,
            2,
            2,
            SceneUnit::ONE,
            BlendMode::Normal,
            &background,
            None,
            ImageLimits::default(),
            &NeverCancel,
        )
        .unwrap()
    };
    assert_eq!(
        rgba(&rotated(PageRotation::Degrees180)),
        vec![
            [255, 255, 255, 255],
            [0, 0, 255, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
        ]
    );
    assert_eq!(
        rgba(&rotated(PageRotation::Degrees270)),
        vec![
            [0, 255, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        ]
    );
}

#[test]
fn subpixel_edge_and_clip_masks_share_the_same_eight_by_eight_samples() {
    let image = image(1, 1, ImageColorSpace::DeviceGray, false, vec![0]);
    let half_width = matrix(["0.5", "0", "0", "1", "0", "0"]);
    let transformed = raster(&image, half_width, 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&transformed), vec![[128, 128, 128, 255]]);

    let mut work = GeometryWork::new(GeometryLimits::default(), &NeverCancel).unwrap();
    let mut clip = CoverageMask::empty(1, 1, &mut work).unwrap();
    let mut left_half = 0_u64;
    for sample_y in 0..8 {
        for sample_x in 0..4 {
            left_half |= 1_u64 << (sample_y * 8 + sample_x);
        }
    }
    clip.set_sample_mask(0, 0, left_half).unwrap();
    let clipped = rasterize_image(
        &image,
        geometry(),
        Matrix::IDENTITY,
        1,
        1,
        SceneUnit::ONE,
        BlendMode::Normal,
        &[white()],
        Some(&clip),
        ImageLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(rgba(&clipped), vec![[128, 128, 128, 255]]);
}

#[test]
fn alpha_and_multiply_are_applied_before_sample_averaging() {
    let red = image(1, 1, ImageColorSpace::DeviceRgb, false, vec![255, 0, 0]);
    let half = NormalizedQ16::from_bits(32_768).unwrap();
    let gray = opaque(ReferenceSrgbQ16::gray(half));
    let raster = rasterize_image(
        &red,
        geometry(),
        Matrix::IDENTITY,
        1,
        1,
        SceneUnit::ONE,
        BlendMode::Multiply,
        &[gray],
        None,
        ImageLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(rgba(&raster), vec![[128, 0, 0, 255]]);

    let half_alpha = rasterize_image(
        &red,
        geometry(),
        Matrix::IDENTITY,
        1,
        1,
        SceneUnit::from_u16(32_768),
        BlendMode::Normal,
        &[black()],
        None,
        ImageLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(rgba(&half_alpha), vec![[128, 0, 0, 255]]);
}

#[test]
fn singular_point_and_line_collapses_are_valid_no_ops() {
    let normal = image(1, 1, ImageColorSpace::DeviceGray, false, vec![0]);
    for transform in [
        matrix(["0", "0", "0", "0", "0", "0"]),
        matrix(["0", "0", "0", "1", "0", "0"]),
        matrix(["1", "1", "1", "1", "0", "0"]),
    ] {
        let result = raster(&normal, transform, 1, 1, &[white()]).unwrap();
        assert_eq!(rgba(&result), vec![[255, 255, 255, 255]]);
        assert_eq!(result.stats().samples(), 0);
        assert_eq!(result.stats().conversions(), 0);
        assert_eq!(result.stats().fuel(), 1);
    }
}

#[test]
fn interpolated_and_mismatched_inputs_fail_structurally() {
    let normal = image(1, 1, ImageColorSpace::DeviceGray, false, vec![0]);
    let interpolated = image(1, 1, ImageColorSpace::DeviceGray, true, vec![0]);
    assert_eq!(
        raster(&interpolated, Matrix::IDENTITY, 1, 1, &[white()]),
        Err(ImageFailure::UnsupportedInterpolation)
    );
    assert_eq!(
        raster(&normal, Matrix::IDENTITY, 2, 1, &[white()]),
        Err(ImageFailure::InvalidImage)
    );
}

#[test]
fn unit_and_texel_boundaries_are_lower_inclusive_and_upper_exclusive() {
    use reference::geometry::Fixed;

    assert_eq!(unit_index(Fixed::ZERO, 2).unwrap(), Some(0));
    assert_eq!(
        unit_index(Fixed::from_scene(scalar("0.5")).unwrap(), 2).unwrap(),
        Some(1)
    );
    assert_eq!(unit_index(Fixed::ONE, 2).unwrap(), None);
    assert_eq!(
        unit_index(Fixed::from_scene(scalar("-0.0001")).unwrap(), 2).unwrap(),
        None
    );

    let image = image(
        2,
        1,
        ImageColorSpace::DeviceRgb,
        false,
        vec![255, 0, 0, 0, 0, 255],
    );
    // Under this flip, the leftmost 8x8 sample lies exactly on u=1 and is excluded. Four
    // columns, including the exact u=0.5 boundary, belong to the right (blue) texel; the
    // remaining three belong to the left (red) texel.
    let flipped_outer_boundary = matrix(["-1", "0", "0", "1", "1.0625", "0"]);
    assert_eq!(
        rgba(&raster(&image, flipped_outer_boundary, 1, 1, &[white()]).unwrap()),
        vec![[128, 32, 159, 255]]
    );
}

#[test]
fn clipped_and_off_page_commands_use_conservative_admission_and_exact_stats() {
    let image = image(1, 1, ImageColorSpace::DeviceGray, false, vec![0]);
    let mut work = GeometryWork::new(GeometryLimits::default(), &NeverCancel).unwrap();
    let clip = CoverageMask::empty(1, 1, &mut work).unwrap();
    let baseline = rasterize_image(
        &image,
        geometry(),
        Matrix::IDENTITY,
        1,
        1,
        SceneUnit::ONE,
        BlendMode::Normal,
        &[white()],
        Some(&clip),
        ImageLimits::default(),
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(rgba(&baseline), vec![[255, 255, 255, 255]]);
    assert_eq!(baseline.stats().samples(), 64);
    assert_eq!(baseline.stats().conversions(), 0);

    let conservative = ImageLimits {
        max_conversions: 63,
        ..ImageLimits::default()
    };
    assert!(matches!(
        rasterize_image(
            &image,
            geometry(),
            Matrix::IDENTITY,
            1,
            1,
            SceneUnit::ONE,
            BlendMode::Normal,
            &[white()],
            Some(&clip),
            conservative,
            &NeverCancel,
        ),
        Err(ImageFailure::Limit {
            kind: ImageLimitKind::Conversions,
            limit: 63,
            consumed: 0,
            attempted: 64,
        })
    ));

    let off_page = matrix(["1", "0", "0", "1", "2", "0"]);
    let off_page = raster(&image, off_page, 1, 1, &[white()]).unwrap();
    assert_eq!(rgba(&off_page), vec![[255, 255, 255, 255]]);
    assert_eq!(off_page.stats().samples(), 64);
    assert_eq!(off_page.stats().conversions(), 0);
}

#[test]
fn every_image_budget_has_an_exact_and_one_less_boundary() {
    let image = image(
        2,
        2,
        ImageColorSpace::DeviceRgb,
        false,
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
    );
    let backdrop = vec![white(); 4];
    let baseline = raster(&image, Matrix::IDENTITY, 2, 2, &backdrop).unwrap();
    let stats = baseline.stats();
    let exact = ImageLimits {
        max_source_pixels: stats.source_pixels(),
        max_stride_bytes: stats.stride_bytes(),
        max_decoded_bytes: stats.decoded_bytes(),
        max_output_pixels: stats.output_pixels(),
        max_samples: stats.samples(),
        max_conversions: stats.conversions(),
        max_retained_bytes: stats.retained_bytes(),
        max_fuel: stats.fuel(),
    };
    let exact_result = rasterize_image(
        &image,
        geometry(),
        Matrix::IDENTITY,
        2,
        2,
        SceneUnit::ONE,
        BlendMode::Normal,
        &backdrop,
        None,
        exact,
        &NeverCancel,
    )
    .unwrap();
    assert_eq!(exact_result, baseline);

    for (kind, tight) in [
        (
            ImageLimitKind::SourcePixels,
            ImageLimits {
                max_source_pixels: stats.source_pixels() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::StrideBytes,
            ImageLimits {
                max_stride_bytes: stats.stride_bytes() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::DecodedBytes,
            ImageLimits {
                max_decoded_bytes: stats.decoded_bytes() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::OutputPixels,
            ImageLimits {
                max_output_pixels: stats.output_pixels() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::Samples,
            ImageLimits {
                max_samples: stats.samples() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::Conversions,
            ImageLimits {
                max_conversions: stats.conversions() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::RetainedBytes,
            ImageLimits {
                max_retained_bytes: stats.retained_bytes() - 1,
                ..exact
            },
        ),
        (
            ImageLimitKind::Fuel,
            ImageLimits {
                max_fuel: stats.fuel() - 1,
                ..exact
            },
        ),
    ] {
        let error = rasterize_image(
            &image,
            geometry(),
            Matrix::IDENTITY,
            2,
            2,
            SceneUnit::ONE,
            BlendMode::Normal,
            &backdrop,
            None,
            tight,
            &NeverCancel,
        )
        .unwrap_err();
        assert!(
            matches!(error, ImageFailure::Limit { kind: actual, .. } if actual == kind),
            "{kind:?} produced {error:?}"
        );
    }
}

#[test]
fn cancellation_is_observed_before_allocation_and_during_fixed_fuel_work() {
    let image = image(
        2,
        2,
        ImageColorSpace::DeviceRgb,
        false,
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
    );
    let backdrop = vec![white(); 4];
    for cancel_at in [1, 2, 3] {
        let cancellation = CancelAtCheck {
            checks: Cell::new(0),
            cancel_at,
        };
        assert_eq!(
            rasterize_image(
                &image,
                geometry(),
                Matrix::IDENTITY,
                2,
                2,
                SceneUnit::ONE,
                BlendMode::Normal,
                &backdrop,
                None,
                ImageLimits::default(),
                &cancellation,
            ),
            Err(ImageFailure::Cancelled)
        );
    }
}

#[test]
fn mounted_conversion_cancellation_counts_only_completed_samples_before_mutation() {
    let image = image(
        2,
        2,
        ImageColorSpace::DeviceRgb,
        false,
        vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
    );
    let cancellation = CancelAtCheck {
        checks: Cell::new(0),
        cancel_at: 2,
    };
    let mut pixels = [white(); 4];
    let mut stats = ImageStats::default();
    assert_eq!(
        paint_image(
            &image,
            geometry(),
            Matrix::IDENTITY,
            2,
            2,
            SceneUnit::ONE,
            BlendMode::Normal,
            &mut pixels,
            None,
            ImageLimits::default(),
            &cancellation,
            &mut stats,
        ),
        Err(ImageFailure::Cancelled)
    );
    assert_eq!(stats.fuel(), 257);
    assert_eq!(stats.samples(), 125);
    assert_eq!(stats.conversions(), 125);
    assert_ne!(pixels[0], white());
    assert_eq!(pixels[1], white());
    assert_eq!(stats.cancellation_checks(), cancellation.checks.get());
}
