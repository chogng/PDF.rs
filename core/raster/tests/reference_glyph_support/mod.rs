#[path = "../../src/reference/coverage.rs"]
pub(crate) mod coverage;
#[path = "../../src/reference/geometry.rs"]
pub(crate) mod geometry;
#[path = "../../src/reference/glyph.rs"]
pub(crate) mod glyph;
#[path = "../../src/reference/stroke.rs"]
pub(crate) mod stroke;

pub(crate) use pdf_rs_raster::reference::{
    NormalizedQ16, PremultipliedRgbaQ16, ReferenceColorProfile,
};
