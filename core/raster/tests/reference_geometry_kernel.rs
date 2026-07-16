#[path = "reference_geometry_kernel_support/mod.rs"]
mod reference;

use pdf_rs_scene::{Matrix, PageGeometry, PageRotation, SceneRect, SceneScalar};

use reference::coverage::CoverageMask;
use reference::geometry::{GeometryCancellation, GeometryLimits, GeometryWork, PageDeviceMap};

struct NeverCancel;

impl GeometryCancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn staged_geometry_kernel_surface_remains_reachable_before_renderer_integration() {
    let mut work = GeometryWork::new(GeometryLimits::default(), &NeverCancel).unwrap();
    assert_eq!(work.edges(), 0);

    let mask = CoverageMask::empty(1, 1, &mut work).unwrap();
    assert_eq!((mask.width(), mask.height()), (1, 1));

    let scalar = |value| SceneScalar::from_decimal(value).unwrap();
    let bounds = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar("1"),
        scalar("1"),
    ])
    .unwrap();
    let map = PageDeviceMap::new(
        PageGeometry::new(bounds, bounds, PageRotation::Degrees0),
        1,
        1,
    )
    .unwrap();
    assert_eq!(map.combined(Matrix::IDENTITY).unwrap(), map.affine());
}
