use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};
use pdf_rs_skia_geometry::Rect;

use super::{
    Angle, ArcDirection, ArcStart, PathBuilder, build_ellipse_arc, build_rotated_ellipse_arc,
};

impl PathBuilder {
    /// Starts a contour with an elliptical arc of one to four quarter turns.
    ///
    /// The arc endpoints are the four exact cardinal points of `bounds`. This
    /// fixed-point contract avoids platform trigonometry while covering circle,
    /// oval, and rounded-rectangle construction deterministically.
    pub fn add_arc(
        &mut self,
        bounds: Rect,
        start: ArcStart,
        direction: ArcDirection,
        quarter_turns: u8,
    ) -> Result<(), SkiaError> {
        if !(1..=4).contains(&quarter_turns) {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        self.reserve_verbs(usize::from(quarter_turns) + 1)?;
        self.add_arc_unchecked(bounds, start, direction, quarter_turns)
    }

    /// Starts a contour with an ellipse arc at an arbitrary angle.
    ///
    /// `start` and `sweep` use clockwise canvas degrees. A non-zero sweep up
    /// to one full turn is split into at most four cubic Bézier segments.
    pub fn add_arc_degrees(
        &mut self,
        bounds: Rect,
        start: Angle,
        sweep: Angle,
    ) -> Result<(), SkiaError> {
        let (start_point, segments) = build_ellipse_arc(bounds, start, sweep)?;
        self.reserve_verbs(segments.len() + 1)?;
        self.move_to(start_point)?;
        for segment in segments {
            self.cubic_to(segment.first_control, segment.second_control, segment.end)?;
        }
        Ok(())
    }

    /// Starts a contour with an arbitrarily rotated ellipse arc.
    ///
    /// `rotation`, `start`, and `sweep` use clockwise canvas degrees. The
    /// ellipse is rotated about the center of `bounds` before being emitted as
    /// cubic Bézier segments.
    pub fn add_rotated_arc_degrees(
        &mut self,
        bounds: Rect,
        rotation: Angle,
        start: Angle,
        sweep: Angle,
    ) -> Result<(), SkiaError> {
        let (start_point, segments) = build_rotated_ellipse_arc(bounds, rotation, start, sweep)?;
        self.reserve_verbs(segments.len() + 1)?;
        self.move_to(start_point)?;
        for segment in segments {
            self.cubic_to(segment.first_control, segment.second_control, segment.end)?;
        }
        Ok(())
    }

    /// Connects the active contour to, then appends, an arbitrary ellipse arc.
    ///
    /// If the active contour does not already end at the arc start point, one
    /// line segment is inserted before the cubic arc segments.
    pub fn arc_to(&mut self, bounds: Rect, start: Angle, sweep: Angle) -> Result<(), SkiaError> {
        let current = self
            .current_point
            .filter(|_| self.has_active_contour)
            .ok_or(SkiaError::new(SkiaErrorCode::InvalidPath))?;
        let (start_point, segments) = build_ellipse_arc(bounds, start, sweep)?;
        let connector = usize::from(current != start_point);
        self.reserve_verbs(segments.len() + connector)?;
        if connector == 1 {
            self.line_to(start_point)?;
        }
        for segment in segments {
            self.cubic_to(segment.first_control, segment.second_control, segment.end)?;
        }
        Ok(())
    }

    /// Connects the active contour to, then appends, a rotated ellipse arc.
    pub fn arc_to_rotated(
        &mut self,
        bounds: Rect,
        rotation: Angle,
        start: Angle,
        sweep: Angle,
    ) -> Result<(), SkiaError> {
        let current = self
            .current_point
            .filter(|_| self.has_active_contour)
            .ok_or(SkiaError::new(SkiaErrorCode::InvalidPath))?;
        let (start_point, segments) = build_rotated_ellipse_arc(bounds, rotation, start, sweep)?;
        let connector = usize::from(current != start_point);
        self.reserve_verbs(segments.len() + connector)?;
        if connector == 1 {
            self.line_to(start_point)?;
        }
        for segment in segments {
            self.cubic_to(segment.first_control, segment.second_control, segment.end)?;
        }
        Ok(())
    }
}
