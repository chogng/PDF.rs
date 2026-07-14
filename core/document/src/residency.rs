use std::mem;

use pdf_rs_syntax::ObjectRef;

use crate::{DocumentError, DocumentErrorCode};

/// Checked value-owned memory footprint for one proof-bearing document result.
///
/// The measurement includes the result's inline Rust representation, syntax
/// heap capacity retained by its parsed terminal object, and any separately
/// allocated reference-chain capacity. It excludes allocator metadata, source
/// and byte-cache storage, stream payloads, and any outer cache container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentResidentFootprint {
    inline_bytes: u64,
    syntax_heap_bytes: u64,
    chain_capacity_bytes: u64,
    total_bytes: u64,
}

impl DocumentResidentFootprint {
    pub(crate) fn for_value<T>(
        syntax_heap_bytes: u64,
        chain_capacity_bytes: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<Self, DocumentError> {
        let inline_bytes =
            u64::try_from(mem::size_of::<T>()).map_err(|_| internal_error(reference, offset))?;
        Self::from_components(
            inline_bytes,
            syntax_heap_bytes,
            chain_capacity_bytes,
            reference,
            offset,
        )
    }

    fn from_components(
        inline_bytes: u64,
        syntax_heap_bytes: u64,
        chain_capacity_bytes: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Result<Self, DocumentError> {
        let total_bytes = inline_bytes
            .checked_add(syntax_heap_bytes)
            .and_then(|value| value.checked_add(chain_capacity_bytes))
            .ok_or_else(|| internal_error(reference, offset))?;
        Ok(Self {
            inline_bytes,
            syntax_heap_bytes,
            chain_capacity_bytes,
            total_bytes,
        })
    }

    /// Returns bytes occupied by the complete result's inline Rust representation.
    pub const fn inline_bytes(self) -> u64 {
        self.inline_bytes
    }

    /// Returns allocator-reported syntax heap capacity retained by the terminal object.
    pub const fn syntax_heap_bytes(self) -> u64 {
        self.syntax_heap_bytes
    }

    /// Returns allocator-reported backing capacity retained by the reference chain.
    pub const fn chain_capacity_bytes(self) -> u64 {
        self.chain_capacity_bytes
    }

    /// Returns the checked sum of all footprint components.
    pub const fn total_bytes(self) -> u64 {
        self.total_bytes
    }
}

pub(crate) fn checked_capacity_bytes(
    capacity: usize,
    element_bytes: usize,
    reference: ObjectRef,
    offset: Option<u64>,
) -> Result<u64, DocumentError> {
    let capacity = u64::try_from(capacity).map_err(|_| internal_error(reference, offset))?;
    let element_bytes =
        u64::try_from(element_bytes).map_err(|_| internal_error(reference, offset))?;
    checked_capacity_product(capacity, element_bytes)
        .ok_or_else(|| internal_error(reference, offset))
}

const fn checked_capacity_product(capacity: u64, element_bytes: u64) -> Option<u64> {
    capacity.checked_mul(element_bytes)
}

const fn internal_error(reference: ObjectRef, offset: Option<u64>) -> DocumentError {
    DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentErrorCategory, DocumentRecoverability};

    fn reference() -> ObjectRef {
        ObjectRef::new(1, 0).expect("test reference is valid")
    }

    #[test]
    fn checked_components_round_trip_and_debug_as_numbers() {
        let footprint =
            DocumentResidentFootprint::from_components(11, 13, 17, reference(), Some(19))
                .expect("small components fit");
        assert_eq!(footprint.inline_bytes(), 11);
        assert_eq!(footprint.syntax_heap_bytes(), 13);
        assert_eq!(footprint.chain_capacity_bytes(), 17);
        assert_eq!(footprint.total_bytes(), 41);
        assert_eq!(
            format!("{footprint:?}"),
            "DocumentResidentFootprint { inline_bytes: 11, syntax_heap_bytes: 13, chain_capacity_bytes: 17, total_bytes: 41 }"
        );
    }

    #[test]
    fn component_and_capacity_overflow_map_to_internal_failure() {
        let component =
            DocumentResidentFootprint::from_components(u64::MAX, 1, 0, reference(), Some(23))
                .expect_err("component total must overflow");
        assert_eq!(component.code(), DocumentErrorCode::InternalState);
        assert_eq!(component.category(), DocumentErrorCategory::Internal);
        assert_eq!(
            component.recoverability(),
            DocumentRecoverability::DoNotRetry
        );
        assert_eq!(component.reference(), Some(reference()));
        assert_eq!(component.offset(), Some(23));

        assert_eq!(checked_capacity_product(u64::MAX, 2), None);
    }
}
