//! Deterministic path-to-triangle conversion for drawing backends.
//!
//! The initial implementation accepts one closed, convex, line-only contour.
//! Unsupported topology fails closed instead of silently producing a different
//! fill. Future releases extend this contract with curve flattening, holes,
//! non-convex contours, and stroke meshes.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

use pdf_rs_skia_geometry::Point;
use pdf_rs_skia_path::{Path, PathVerb};

/// Stable tessellation failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TessellationErrorCode {
    /// A configured geometry ceiling is invalid.
    InvalidLimits,
    /// The path has no supported closed contour.
    InvalidPath,
    /// The path uses curves, holes, open contours, or non-convex geometry.
    UnsupportedTopology,
    /// The output would exceed a configured ceiling.
    ResourceLimit,
    /// Output allocation failed.
    AllocationFailed,
}

/// Source-redacted tessellation failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TessellationError {
    code: TessellationErrorCode,
}

impl TessellationError {
    const fn new(code: TessellationErrorCode) -> Self {
        Self { code }
    }
    /// Returns the stable failure code.
    pub const fn code(self) -> TessellationErrorCode {
        self.code
    }
}

impl fmt::Display for TessellationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.code)
    }
}
impl std::error::Error for TessellationError {}

/// Output ceilings for one tessellation operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TessellationLimits {
    max_vertices: usize,
    max_indices: usize,
}

impl TessellationLimits {
    /// Creates positive output ceilings.
    pub fn new(max_vertices: usize, max_indices: usize) -> Result<Self, TessellationError> {
        if max_vertices < 3 || max_indices < 3 {
            return Err(TessellationError::new(TessellationErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_vertices,
            max_indices,
        })
    }
}

/// Immutable triangle mesh using counter-consistent indices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriangleMesh {
    vertices: Vec<Point>,
    indices: Vec<u32>,
}

impl TriangleMesh {
    /// Borrows mesh vertices.
    pub fn vertices(&self) -> &[Point] {
        &self.vertices
    }
    /// Borrows triangles as consecutive index triples.
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }
}

/// Deterministic convex fill tessellator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tessellator {
    limits: TessellationLimits,
}

impl Tessellator {
    /// Creates one tessellator with explicit output ceilings.
    pub const fn new(limits: TessellationLimits) -> Self {
        Self { limits }
    }
    /// Triangulates one closed, convex, line-only path using a triangle fan.
    pub fn tessellate_fill(&self, path: &Path) -> Result<TriangleMesh, TessellationError> {
        let contour = contour(path)?;
        if contour.len() > self.limits.max_vertices {
            return Err(TessellationError::new(TessellationErrorCode::ResourceLimit));
        }
        let triangle_count = contour.len() - 2;
        let index_count = triangle_count
            .checked_mul(3)
            .ok_or(TessellationError::new(TessellationErrorCode::ResourceLimit))?;
        if index_count > self.limits.max_indices {
            return Err(TessellationError::new(TessellationErrorCode::ResourceLimit));
        }
        if !is_convex(&contour) {
            return Err(TessellationError::new(
                TessellationErrorCode::UnsupportedTopology,
            ));
        }
        let mut indices = Vec::new();
        indices
            .try_reserve_exact(index_count)
            .map_err(|_| TessellationError::new(TessellationErrorCode::AllocationFailed))?;
        for index in 1..(contour.len() - 1) {
            indices.extend([
                0,
                u32::try_from(index).unwrap_or(u32::MAX),
                u32::try_from(index + 1).unwrap_or(u32::MAX),
            ]);
        }
        Ok(TriangleMesh {
            vertices: contour,
            indices,
        })
    }
}

fn contour(path: &Path) -> Result<Vec<Point>, TessellationError> {
    let mut current = Vec::new();
    let mut closed = false;
    for verb in path.verbs() {
        match *verb {
            PathVerb::MoveTo(point) if current.is_empty() => current.push(point),
            PathVerb::LineTo(point) if !current.is_empty() && !closed => current.push(point),
            PathVerb::Close if !current.is_empty() && !closed => closed = true,
            PathVerb::QuadTo(..) | PathVerb::ConicTo(..) | PathVerb::CubicTo(..) => {
                return Err(TessellationError::new(
                    TessellationErrorCode::UnsupportedTopology,
                ));
            }
            _ => {
                return Err(TessellationError::new(
                    TessellationErrorCode::UnsupportedTopology,
                ));
            }
        }
    }
    if !closed || current.len() < 3 {
        return Err(TessellationError::new(TessellationErrorCode::InvalidPath));
    }
    Ok(current)
}

fn is_convex(points: &[Point]) -> bool {
    let mut sign = 0_i128;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        let c = points[(index + 2) % points.len()];
        let abx = i128::from(b.x().bits()) - i128::from(a.x().bits());
        let aby = i128::from(b.y().bits()) - i128::from(a.y().bits());
        let bcx = i128::from(c.x().bits()) - i128::from(b.x().bits());
        let bcy = i128::from(c.y().bits()) - i128::from(b.y().bits());
        let cross = abx * bcy - aby * bcx;
        if cross == 0 {
            return false;
        }
        if sign == 0 {
            sign = cross.signum();
        } else if sign != cross.signum() {
            return false;
        }
    }
    true
}
