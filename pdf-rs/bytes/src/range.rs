use crate::{SourceError, SourceErrorCode};

/// Non-empty half-open byte range with a checked exclusive end.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteRange {
    start: u64,
    len: u64,
}

impl ByteRange {
    /// Creates a non-empty range and rejects exclusive-end overflow.
    pub fn new(start: u64, len: u64) -> Result<Self, SourceError> {
        if len == 0 || start.checked_add(len).is_none() {
            return Err(SourceError::for_code(SourceErrorCode::InvalidRange));
        }
        Ok(Self { start, len })
    }

    /// Returns the first included byte offset.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the number of requested bytes.
    pub const fn len(self) -> u64 {
        self.len
    }

    /// Reports whether the range contains no bytes.
    ///
    /// Valid ranges are always non-empty; this method is provided for generic
    /// collection code and therefore always returns `false`.
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Returns the checked exclusive end established by the constructor.
    pub const fn end_exclusive(self) -> u64 {
        self.start + self.len
    }
}

/// Canonical sorted, disjoint missing ranges returned with a data ticket.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SmallRanges {
    ranges: Vec<ByteRange>,
}

impl SmallRanges {
    pub(crate) fn try_from_canonical(
        ranges: Vec<ByteRange>,
        max_ranges: usize,
    ) -> Result<Self, SourceError> {
        if ranges.is_empty() {
            return Err(SourceError::for_code(SourceErrorCode::InternalState));
        }
        if ranges.len() > max_ranges {
            return Err(SourceError::for_code(SourceErrorCode::ResourceLimit));
        }
        let mut total_bytes = 0_u64;
        let mut previous_end = None;
        for range in &ranges {
            if previous_end.is_some_and(|end| end >= range.start()) {
                return Err(SourceError::for_code(SourceErrorCode::InternalState));
            }
            total_bytes = total_bytes
                .checked_add(range.len())
                .ok_or_else(|| SourceError::for_code(SourceErrorCode::ResourceLimit))?;
            previous_end = Some(range.end_exclusive());
        }
        let _ = total_bytes;
        Ok(Self { ranges })
    }

    /// Returns the sorted disjoint missing ranges.
    pub fn as_slice(&self) -> &[ByteRange] {
        &self.ranges
    }

    /// Returns the number of disjoint ranges.
    pub const fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Reports whether no ranges are missing.
    pub const fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}
