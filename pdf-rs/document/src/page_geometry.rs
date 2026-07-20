use pdf_rs_syntax::ObjectRef;

/// Signed page-space coordinate with nine decimal fractional digits.
///
/// The representation matches the canonical Scene scalar scale without creating a dependency
/// from the document model to the Scene crate. Parsing code must reject source numbers that
/// cannot be represented exactly at this scale.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageCoordinate(i64);

impl PageCoordinate {
    /// Number of scaled units in one integer page-space unit.
    pub const SCALE: i64 = 1_000_000_000;

    /// Exact zero.
    pub const ZERO: Self = Self(0);

    /// Creates one coordinate from its canonical scaled integer.
    pub const fn from_scaled(value: i64) -> Self {
        Self(value)
    }

    /// Creates one exact integral coordinate when scaling does not overflow.
    pub fn from_integer(value: i64) -> Option<Self> {
        value.checked_mul(Self::SCALE).map(Self)
    }

    /// Returns the canonical scaled integer.
    pub const fn scaled(self) -> i64 {
        self.0
    }
}

/// Positive-area page boundary in `[left, bottom, right, top]` order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PageRectangle {
    coordinates: [PageCoordinate; 4],
}

impl PageRectangle {
    /// Validates strictly increasing horizontal and vertical bounds with representable extents.
    pub fn new(coordinates: [PageCoordinate; 4]) -> Option<Self> {
        if coordinates[2] <= coordinates[0]
            || coordinates[3] <= coordinates[1]
            || coordinates[2]
                .scaled()
                .checked_sub(coordinates[0].scaled())
                .is_none()
            || coordinates[3]
                .scaled()
                .checked_sub(coordinates[1].scaled())
                .is_none()
        {
            return None;
        }
        Some(Self { coordinates })
    }

    /// Returns `[left, bottom, right, top]` in canonical fixed-point coordinates.
    pub const fn coordinates(self) -> [PageCoordinate; 4] {
        self.coordinates
    }

    /// Returns the left boundary.
    pub const fn left(self) -> PageCoordinate {
        self.coordinates[0]
    }

    /// Returns the bottom boundary.
    pub const fn bottom(self) -> PageCoordinate {
        self.coordinates[1]
    }

    /// Returns the right boundary.
    pub const fn right(self) -> PageCoordinate {
        self.coordinates[2]
    }

    /// Returns the top boundary.
    pub const fn top(self) -> PageCoordinate {
        self.coordinates[3]
    }

    /// Returns the positive horizontal extent.
    pub fn width(self) -> PageCoordinate {
        PageCoordinate::from_scaled(
            self.right()
                .scaled()
                .checked_sub(self.left().scaled())
                .expect("validated page rectangle has a representable width"),
        )
    }

    /// Returns the positive vertical extent.
    pub fn height(self) -> PageCoordinate {
        PageCoordinate::from_scaled(
            self.top()
                .scaled()
                .checked_sub(self.bottom().scaled())
                .expect("validated page rectangle has a representable height"),
        )
    }
}

/// Exact field and alias provenance that supplied one inherited page value.
#[derive(Eq, Hash, PartialEq)]
pub struct PageValueProvenance {
    defining_object: ObjectRef,
    defining_value_offset: u64,
    alias_chain: Vec<ObjectRef>,
}

impl PageValueProvenance {
    /// Records a value stored directly in the defining Page or Pages dictionary.
    pub(crate) const fn direct(defining_object: ObjectRef, defining_value_offset: u64) -> Self {
        Self {
            defining_object,
            defining_value_offset,
            alias_chain: Vec::new(),
        }
    }

    /// Records the complete root-to-terminal alias chain for an indirect field value.
    ///
    /// Returns `None` when the chain is empty. Reference-cycle and duplicate policy belongs to the
    /// bounded materialization job because only that job owns the complete traversal evidence.
    pub(crate) fn indirect(
        defining_object: ObjectRef,
        defining_value_offset: u64,
        alias_chain: Vec<ObjectRef>,
    ) -> Option<Self> {
        if alias_chain.is_empty() {
            return None;
        }
        Some(Self {
            defining_object,
            defining_value_offset,
            alias_chain,
        })
    }

    /// Returns the Page or Pages dictionary whose field ended the ancestor search.
    pub const fn defining_object(&self) -> ObjectRef {
        self.defining_object
    }

    /// Returns the source offset of the exact field value in the defining dictionary.
    pub const fn defining_value_offset(&self) -> u64 {
        self.defining_value_offset
    }

    /// Returns the exact indirect object referenced by that field, if any.
    pub fn value_object(&self) -> Option<ObjectRef> {
        self.alias_chain.first().copied()
    }

    /// Returns the terminal non-reference object that supplied the value, if indirect.
    pub fn terminal_object(&self) -> Option<ObjectRef> {
        self.alias_chain.last().copied()
    }

    /// Returns the complete root-to-terminal alias chain, empty for a direct value.
    pub fn alias_chain(&self) -> &[ObjectRef] {
        &self.alias_chain
    }

    /// Returns allocator-reported bytes reserved by the alias chain.
    pub fn retained_alias_chain_bytes(&self) -> Option<u64> {
        retained_reference_bytes(self.alias_chain.capacity())
    }

    /// Reports whether the inherited field was resolved through an indirect object.
    pub const fn is_indirect(&self) -> bool {
        !self.alias_chain.is_empty()
    }
}

impl std::fmt::Debug for PageValueProvenance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PageValueProvenance")
            .field("defining_object", &self.defining_object)
            .field("defining_value_offset", &self.defining_value_offset)
            .field("alias_depth", &self.alias_chain.len())
            .field("value_object", &self.value_object())
            .field("terminal_object", &self.terminal_object())
            .finish()
    }
}

/// One materialized inherited value paired with exact object provenance.
#[derive(Debug, Eq, Hash, PartialEq)]
pub struct InheritedPageValue<T> {
    value: T,
    provenance: PageValueProvenance,
}

impl<T> InheritedPageValue<T> {
    /// Creates one already-validated inherited value.
    pub(crate) const fn new(value: T, provenance: PageValueProvenance) -> Self {
        Self { value, provenance }
    }

    /// Borrows the materialized value.
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Returns the exact defining and optional indirect object provenance.
    pub const fn provenance(&self) -> &PageValueProvenance {
        &self.provenance
    }

    /// Consumes the wrapper and returns the materialized value.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Consumes the wrapper and returns the value with its inseparable provenance.
    pub fn into_parts(self) -> (T, PageValueProvenance) {
        (self.value, self.provenance)
    }
}

/// Materialized inherited MediaBox and effective CropBox values.
#[derive(Debug, Eq, Hash, PartialEq)]
pub struct PageBoxes {
    media_box: InheritedPageValue<PageRectangle>,
    crop_box: Option<InheritedPageValue<PageRectangle>>,
}

impl PageBoxes {
    /// Creates page boxes, applying the normative CropBox-to-MediaBox default when absent.
    pub(crate) const fn new(
        media_box: InheritedPageValue<PageRectangle>,
        crop_box: Option<InheritedPageValue<PageRectangle>>,
    ) -> Self {
        Self {
            media_box,
            crop_box,
        }
    }

    /// Returns the required inherited MediaBox.
    pub const fn media_box(&self) -> PageRectangle {
        *self.media_box.value()
    }

    /// Returns the explicit inherited CropBox or the effective MediaBox default.
    pub const fn crop_box(&self) -> PageRectangle {
        match &self.crop_box {
            Some(crop_box) => *crop_box.value(),
            None => *self.media_box.value(),
        }
    }

    /// Returns the exact MediaBox field provenance.
    pub const fn media_box_provenance(&self) -> &PageValueProvenance {
        self.media_box.provenance()
    }

    /// Returns the effective CropBox provenance.
    ///
    /// When CropBox defaulted to MediaBox, this is the MediaBox provenance and
    /// [`Self::crop_box_defaults_to_media_box`] distinguishes the default.
    pub const fn crop_box_provenance(&self) -> &PageValueProvenance {
        match &self.crop_box {
            Some(crop_box) => crop_box.provenance(),
            None => self.media_box.provenance(),
        }
    }

    /// Reports whether the effective CropBox came from the normative MediaBox default.
    pub const fn crop_box_defaults_to_media_box(&self) -> bool {
        self.crop_box.is_none()
    }
}

fn retained_reference_bytes(capacity: usize) -> Option<u64> {
    u64::try_from(capacity).ok().and_then(|capacity| {
        u64::try_from(std::mem::size_of::<ObjectRef>())
            .ok()
            .and_then(|bytes| capacity.checked_mul(bytes))
    })
}

/// Canonical clockwise quarter-turn page rotation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PageRotation {
    /// No rotation.
    #[default]
    Degrees0,
    /// Clockwise quarter turn.
    Degrees90,
    /// Clockwise half turn.
    Degrees180,
    /// Clockwise three-quarter turn.
    Degrees270,
}

impl PageRotation {
    /// Normalizes any positive or negative multiple of 90 degrees.
    pub fn from_degrees(degrees: i64) -> Option<Self> {
        if degrees.rem_euclid(90) != 0 {
            return None;
        }
        match degrees.rem_euclid(360) {
            0 => Some(Self::Degrees0),
            90 => Some(Self::Degrees90),
            180 => Some(Self::Degrees180),
            270 => Some(Self::Degrees270),
            _ => None,
        }
    }

    /// Returns the canonical nonnegative degree value.
    pub const fn degrees(self) -> u16 {
        match self {
            Self::Degrees0 => 0,
            Self::Degrees90 => 90,
            Self::Degrees180 => 180,
            Self::Degrees270 => 270,
        }
    }

    /// Returns the canonical clockwise quarter-turn count.
    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::Degrees0 => 0,
            Self::Degrees90 => 1,
            Self::Degrees180 => 2,
            Self::Degrees270 => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(number: u32) -> ObjectRef {
        ObjectRef::new(number, 0).expect("test reference is valid")
    }

    fn rectangle() -> PageRectangle {
        PageRectangle::new([
            PageCoordinate::from_integer(-10).unwrap(),
            PageCoordinate::from_integer(5).unwrap(),
            PageCoordinate::from_integer(210).unwrap(),
            PageCoordinate::from_integer(305).unwrap(),
        ])
        .expect("test rectangle has positive area")
    }

    #[test]
    fn coordinates_and_rectangles_reject_overflow_and_nonpositive_area() {
        assert_eq!(
            PageCoordinate::from_integer(7).unwrap().scaled(),
            7_000_000_000
        );
        assert_eq!(PageCoordinate::from_integer(i64::MAX), None);

        let rectangle = rectangle();
        assert_eq!(
            rectangle.width(),
            PageCoordinate::from_integer(220).unwrap()
        );
        assert_eq!(
            rectangle.height(),
            PageCoordinate::from_integer(300).unwrap()
        );
        assert_eq!(
            PageRectangle::new([
                PageCoordinate::ZERO,
                PageCoordinate::ZERO,
                PageCoordinate::ZERO,
                PageCoordinate::from_integer(1).unwrap(),
            ]),
            None
        );
    }

    #[test]
    fn crop_box_default_retains_media_box_provenance() {
        let provenance =
            PageValueProvenance::indirect(reference(2), 17, vec![reference(8), reference(9)])
                .expect("nonempty alias chain is valid");
        let media = InheritedPageValue::new(rectangle(), provenance);
        let boxes = PageBoxes::new(media, None);

        assert_eq!(boxes.media_box(), rectangle());
        assert_eq!(boxes.crop_box(), rectangle());
        assert_eq!(boxes.crop_box_provenance().defining_object(), reference(2));
        assert_eq!(boxes.crop_box_provenance().defining_value_offset(), 17);
        assert_eq!(
            boxes.crop_box_provenance().alias_chain(),
            &[reference(8), reference(9)]
        );
        assert!(boxes.crop_box_defaults_to_media_box());
    }

    #[test]
    fn rotation_normalizes_signed_full_turns_and_rejects_other_angles() {
        assert_eq!(
            PageRotation::from_degrees(-90),
            Some(PageRotation::Degrees270)
        );
        assert_eq!(
            PageRotation::from_degrees(450),
            Some(PageRotation::Degrees90)
        );
        assert_eq!(PageRotation::from_degrees(45), None);
    }
}
