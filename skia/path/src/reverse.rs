use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};
use pdf_rs_skia_geometry::Point;

use super::{ConicWeight, Path, PathBuilder, PathVerb};

#[derive(Clone, Copy)]
enum ContourSegment {
    Line {
        start: Point,
        end: Point,
    },
    Quad {
        start: Point,
        control: Point,
        end: Point,
    },
    Conic {
        start: Point,
        control: Point,
        end: Point,
        weight: ConicWeight,
    },
    Cubic {
        start: Point,
        first_control: Point,
        second_control: Point,
        end: Point,
    },
}

impl ContourSegment {
    fn end(self) -> Point {
        match self {
            Self::Line { end, .. }
            | Self::Quad { end, .. }
            | Self::Conic { end, .. }
            | Self::Cubic { end, .. } => end,
        }
    }
}

pub(super) struct Contour {
    start: Point,
    segments: Vec<ContourSegment>,
    pub(super) closed: bool,
}

pub(super) fn split_contours(path: &Path) -> Result<Vec<Contour>, SkiaError> {
    let mut contours = Vec::new();
    contours
        .try_reserve(path.verbs().len())
        .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
    let mut current = None;
    for verb in path.verbs() {
        match *verb {
            PathVerb::MoveTo(point) => {
                if let Some(contour) = current.take() {
                    contours.push(contour);
                }
                current = Some(Contour {
                    start: point,
                    segments: Vec::new(),
                    closed: false,
                });
            }
            PathVerb::LineTo(end) => {
                push_segment(&mut current, |start| ContourSegment::Line { start, end })?
            }
            PathVerb::QuadTo(control, end) => {
                push_segment(&mut current, |start| ContourSegment::Quad {
                    start,
                    control,
                    end,
                })?
            }
            PathVerb::ConicTo(control, end, weight) => {
                push_segment(&mut current, |start| ContourSegment::Conic {
                    start,
                    control,
                    end,
                    weight,
                })?
            }
            PathVerb::CubicTo(first_control, second_control, end) => {
                push_segment(&mut current, |start| ContourSegment::Cubic {
                    start,
                    first_control,
                    second_control,
                    end,
                })?
            }
            PathVerb::Close => {
                let mut contour = current
                    .take()
                    .ok_or(SkiaError::new(SkiaErrorCode::InvalidPath))?;
                contour.closed = true;
                contours.push(contour);
            }
        }
    }
    if let Some(contour) = current {
        contours.push(contour);
    }
    Ok(contours)
}

fn push_segment(
    current: &mut Option<Contour>,
    make: impl FnOnce(Point) -> ContourSegment,
) -> Result<(), SkiaError> {
    let contour = current
        .as_mut()
        .ok_or(SkiaError::new(SkiaErrorCode::InvalidPath))?;
    let start = contour
        .segments
        .last()
        .map(|segment| segment.end())
        .unwrap_or(contour.start);
    contour
        .segments
        .try_reserve(1)
        .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
    contour.segments.push(make(start));
    Ok(())
}

pub(super) fn append_reversed_contour(
    builder: &mut PathBuilder,
    contour: Contour,
) -> Result<(), SkiaError> {
    let last = contour
        .segments
        .last()
        .map(|segment| segment.end())
        .unwrap_or(contour.start);
    if contour.closed {
        builder.move_to(contour.start)?;
        if last != contour.start {
            builder.line_to(last)?;
        }
    } else {
        builder.move_to(last)?;
    }
    for segment in contour.segments.iter().rev() {
        match *segment {
            ContourSegment::Line { start, .. } => builder.line_to(start)?,
            ContourSegment::Quad { start, control, .. } => builder.quad_to(control, start)?,
            ContourSegment::Conic {
                start,
                control,
                weight,
                ..
            } => builder.conic_to(control, start, weight)?,
            ContourSegment::Cubic {
                start,
                first_control,
                second_control,
                ..
            } => builder.cubic_to(second_control, first_control, start)?,
        }
    }
    if contour.closed {
        builder.close()?;
    }
    Ok(())
}
