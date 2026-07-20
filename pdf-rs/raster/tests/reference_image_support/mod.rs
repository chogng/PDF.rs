#[path = "../../src/reference/coverage.rs"]
pub(crate) mod coverage;
#[path = "../../src/reference/geometry.rs"]
pub(crate) mod geometry;
#[path = "../../src/reference/image.rs"]
pub(crate) mod image;

pub(crate) use pdf_rs_raster::reference::{
    NormalizedQ16, PremultipliedRgbaQ16, ReferenceBlendMode, ReferenceColorProfile,
    ReferenceSrgbQ16,
};
