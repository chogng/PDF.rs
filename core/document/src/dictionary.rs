use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_object::IndirectObjectValue;
use pdf_rs_syntax::{Located, ObjectRef, PdfDictionary, SyntaxObject};

use crate::{AttestedObject, DocumentCancellation, DocumentError, DocumentErrorCode};

const CANCELLATION_PROBE_INTERVAL: usize = 256;

pub(crate) fn direct_dictionary(
    object: &AttestedObject,
    snapshot: SourceSnapshot,
    invalid_code: DocumentErrorCode,
) -> Result<&PdfDictionary, DocumentError> {
    let reference = object.reference();
    let offset = object.attestation().xref_offset();
    match object.value() {
        IndirectObjectValue::Direct(value) if value.source() == snapshot.identity() => value
            .value()
            .as_dictionary()
            .ok_or_else(|| DocumentError::for_code(invalid_code, Some(reference), Some(offset))),
        IndirectObjectValue::Direct(_) => Err(DocumentError::for_code(
            DocumentErrorCode::AttestedObjectEvidenceMismatch,
            Some(reference),
            Some(offset),
        )),
        IndirectObjectValue::Stream(_) => Err(DocumentError::for_code(
            invalid_code,
            Some(reference),
            Some(offset),
        )),
    }
}

pub(crate) struct StructuralFields<'dictionary, const N: usize> {
    values: [Option<&'dictionary Located<SyntaxObject>>; N],
    duplicate_offsets: [Option<u64>; N],
}

pub(crate) fn collect_structural_fields<'dictionary, const N: usize>(
    dictionary: &'dictionary PdfDictionary,
    keys: [&[u8]; N],
    reference: ObjectRef,
    cancellation: &dyn DocumentCancellation,
) -> Result<StructuralFields<'dictionary, N>, DocumentError> {
    let mut fields = StructuralFields {
        values: [None; N],
        duplicate_offsets: [None; N],
    };
    for (entry_index, entry) in dictionary.entries().iter().enumerate() {
        if entry_index % CANCELLATION_PROBE_INTERVAL == 0 && cancellation.is_cancelled() {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(reference),
                Some(entry.key().span().start()),
            ));
        }
        for (field_index, key) in keys.iter().enumerate() {
            if entry.key().value().bytes() != *key {
                continue;
            }
            if fields.values[field_index].is_some() {
                fields.duplicate_offsets[field_index].get_or_insert(entry.key().span().start());
            } else {
                fields.values[field_index] = Some(entry.value());
            }
            break;
        }
    }
    Ok(fields)
}

pub(crate) fn reject_duplicate_field<const N: usize>(
    fields: &StructuralFields<'_, N>,
    index: usize,
    reference: ObjectRef,
) -> Result<(), DocumentError> {
    if let Some(offset) = fields.duplicate_offsets.get(index).copied().flatten() {
        return Err(DocumentError::for_code(
            DocumentErrorCode::DuplicateStructuralKey,
            Some(reference),
            Some(offset),
        ));
    }
    Ok(())
}

pub(crate) fn optional_field<'dictionary, const N: usize>(
    fields: &StructuralFields<'dictionary, N>,
    index: usize,
) -> Option<&'dictionary Located<SyntaxObject>> {
    fields.values.get(index).copied().flatten()
}

pub(crate) fn optional_non_null_field<'dictionary, const N: usize>(
    fields: &StructuralFields<'dictionary, N>,
    index: usize,
) -> Option<&'dictionary Located<SyntaxObject>> {
    optional_field(fields, index).filter(|value| !matches!(value.value(), SyntaxObject::Null))
}

pub(crate) fn required_field<'dictionary, const N: usize>(
    fields: &StructuralFields<'dictionary, N>,
    index: usize,
    reference: ObjectRef,
    object_offset: u64,
    missing_code: DocumentErrorCode,
) -> Result<&'dictionary Located<SyntaxObject>, DocumentError> {
    optional_field(fields, index)
        .ok_or_else(|| DocumentError::for_code(missing_code, Some(reference), Some(object_offset)))
}
