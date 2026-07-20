use crate::{Point, SkiaError, SkiaErrorCode};

/// Fill decision for closed path contours.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FillRule {
    /// A point is inside when it has odd crossing parity.
    EvenOdd,
    /// A point is inside when its signed winding number is non-zero.
    NonZero,
}

/// One immutable line-path operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PathVerb {
    /// Starts a new contour.
    MoveTo(Point),
    /// Appends a straight segment to the active contour.
    LineTo(Point),
    /// Closes the active contour to its starting point.
    Close,
}

/// Immutable path containing line contours.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path {
    verbs: Vec<PathVerb>,
}

impl Path {
    /// Borrows path operations in declaration order.
    pub fn verbs(&self) -> &[PathVerb] {
        &self.verbs
    }
}

/// Bounded, fallible builder for an immutable line path.
#[derive(Debug)]
pub struct PathBuilder {
    verbs: Vec<PathVerb>,
    has_active_contour: bool,
    max_verbs: usize,
}

impl PathBuilder {
    /// Creates a builder with a positive maximum number of path operations.
    pub fn new(max_verbs: usize) -> Result<Self, SkiaError> {
        if max_verbs == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidLimits));
        }
        Ok(Self {
            verbs: Vec::new(),
            has_active_contour: false,
            max_verbs,
        })
    }

    /// Starts a new contour.
    pub fn move_to(&mut self, point: Point) -> Result<(), SkiaError> {
        self.push(PathVerb::MoveTo(point))?;
        self.has_active_contour = true;
        Ok(())
    }

    /// Appends a line to the active contour.
    pub fn line_to(&mut self, point: Point) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::LineTo(point))
    }

    /// Closes the active contour.
    pub fn close(&mut self) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::Close)?;
        self.has_active_contour = false;
        Ok(())
    }

    /// Publishes an immutable path. Open contours are implicitly closed by filling operations.
    pub fn finish(self) -> Result<Path, SkiaError> {
        if self.verbs.is_empty() {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        Ok(Path { verbs: self.verbs })
    }

    fn push(&mut self, verb: PathVerb) -> Result<(), SkiaError> {
        if self.verbs.len() == self.max_verbs {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        self.verbs
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.verbs.push(verb);
        Ok(())
    }
}
