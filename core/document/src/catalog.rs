use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_syntax::{ObjectRef, SyntaxObject};

use crate::dictionary::{
    collect_structural_fields, direct_dictionary, reject_duplicate_field, required_field,
};
use crate::{
    AttestedObject, AttestedRevisionIndex, DocumentCancellation, DocumentError, DocumentErrorCode,
    RevisionId,
};

/// Source- and revision-bound summary of one validated strict Catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrictCatalog {
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    root: ObjectRef,
    pages: ObjectRef,
}

impl StrictCatalog {
    /// Returns the immutable source snapshot containing the Catalog.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the attested strict-base revision identity.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the `startxref` anchor of the attested strict-base revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the trailer's exact Catalog object identity.
    pub const fn root(self) -> ObjectRef {
        self.root
    }

    /// Returns the exact page-tree root referenced by the Catalog.
    pub const fn pages(self) -> ObjectRef {
        self.pages
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LocatedObjectRef {
    reference: ObjectRef,
    value_offset: u64,
}

impl LocatedObjectRef {
    pub(crate) const fn reference(self) -> ObjectRef {
        self.reference
    }

    pub(crate) const fn value_offset(self) -> u64 {
        self.value_offset
    }
}

pub(crate) struct ParsedCatalog {
    summary: StrictCatalog,
    pages: LocatedObjectRef,
}

impl ParsedCatalog {
    pub(crate) const fn summary(&self) -> StrictCatalog {
        self.summary
    }

    pub(crate) const fn pages_entry(&self) -> LocatedObjectRef {
        self.pages
    }
}

pub(crate) fn parse_strict_catalog(
    index: &AttestedRevisionIndex,
    object: &AttestedObject,
    cancellation: &dyn DocumentCancellation,
) -> Result<ParsedCatalog, DocumentError> {
    let reference = object.reference();
    let offset = object.attestation().xref_offset();
    if reference != index.root() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(reference),
            Some(offset),
        ));
    }
    if object.snapshot() != index.snapshot()
        || object.revision_id() != index.revision_id()
        || object.revision_startxref() != index.startxref()
        || object.object_limits() != index.object_limits()
        || object.syntax_limits() != index.syntax_limits()
    {
        return Err(DocumentError::for_code(
            DocumentErrorCode::AttestedObjectEvidenceMismatch,
            Some(reference),
            Some(offset),
        ));
    }

    let dictionary =
        direct_dictionary(object, index.snapshot(), DocumentErrorCode::InvalidCatalog)?;
    let fields = collect_structural_fields(
        dictionary,
        [b"Type".as_slice(), b"Pages".as_slice()],
        reference,
        cancellation,
    )?;
    reject_duplicate_field(&fields, 0, reference)?;
    reject_duplicate_field(&fields, 1, reference)?;
    let type_value = required_field(
        &fields,
        0,
        reference,
        offset,
        DocumentErrorCode::InvalidCatalog,
    )?;
    if !matches!(
        type_value.value(),
        SyntaxObject::Name(name) if name.bytes() == b"Catalog"
    ) {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidCatalog,
            Some(reference),
            Some(type_value.span().start()),
        ));
    }
    let pages_value = required_field(
        &fields,
        1,
        reference,
        offset,
        DocumentErrorCode::InvalidCatalog,
    )?;
    let Some(pages) = pages_value.value().as_reference() else {
        return Err(DocumentError::for_code(
            DocumentErrorCode::InvalidCatalog,
            Some(reference),
            Some(pages_value.span().start()),
        ));
    };

    let summary = StrictCatalog {
        snapshot: index.snapshot(),
        revision_id: index.revision_id(),
        revision_startxref: index.startxref(),
        root: reference,
        pages,
    };
    Ok(ParsedCatalog {
        summary,
        pages: LocatedObjectRef {
            reference: pages,
            value_offset: pages_value.span().start(),
        },
    })
}
