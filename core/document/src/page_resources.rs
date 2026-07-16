use std::fmt;
use std::mem;

use pdf_rs_object::IndirectObjectValue;
use pdf_rs_syntax::{ObjectRef, PdfDictionary};

use crate::{AttestedObject, DocumentError, DocumentErrorCode};

enum PageResourceOwner {
    Direct {
        object: AttestedObject,
    },
    Indirect {
        terminal_object: AttestedObject,
        alias_chain: Vec<ObjectRef>,
    },
}

/// Move-only inherited Resources scope with exact lookup and value provenance.
///
/// The ancestor chain is ordered from the exact Page leaf through each visited Pages dictionary
/// to the defining object. Direct Resources retain that defining object; indirect Resources retain
/// the complete alias resolution and terminal object. This preserves a bounded owner from which
/// later content processing can borrow the actual dictionary without an unbudgeted deep clone.
pub struct PageResourceScope {
    defining_object: ObjectRef,
    defining_value_offset: u64,
    ancestor_lookup_chain: Vec<ObjectRef>,
    owner: PageResourceOwner,
}

impl PageResourceScope {
    /// Returns the Page or Pages dictionary whose Resources field ended inheritance lookup.
    pub const fn defining_object(&self) -> ObjectRef {
        self.defining_object
    }

    /// Returns the source offset of the exact Resources field value in the defining dictionary.
    pub const fn defining_value_offset(&self) -> u64 {
        self.defining_value_offset
    }

    /// Returns the indirect object named by Resources, or `None` for a direct dictionary.
    pub fn resource_object(&self) -> Option<ObjectRef> {
        match &self.owner {
            PageResourceOwner::Direct { .. } => None,
            PageResourceOwner::Indirect { alias_chain, .. } => alias_chain.first().copied(),
        }
    }

    /// Returns the terminal non-reference resource object, or `None` for a direct dictionary.
    pub fn terminal_resource_object(&self) -> Option<ObjectRef> {
        match &self.owner {
            PageResourceOwner::Direct { .. } => None,
            PageResourceOwner::Indirect { alias_chain, .. } => alias_chain.last().copied(),
        }
    }

    /// Returns the exact Page-to-defining-object ancestor lookup chain.
    pub fn ancestor_lookup_chain(&self) -> &[ObjectRef] {
        &self.ancestor_lookup_chain
    }

    /// Returns the complete Resources alias chain when the field was indirect.
    pub fn resource_alias_chain(&self) -> &[ObjectRef] {
        match &self.owner {
            PageResourceOwner::Direct { .. } => &[],
            PageResourceOwner::Indirect { alias_chain, .. } => alias_chain,
        }
    }

    /// Returns allocator-reported bytes reserved by the ancestor and Resources alias chains.
    pub fn retained_lookup_chain_bytes(&self) -> Option<u64> {
        let ancestor = retained_reference_bytes(self.ancestor_lookup_chain.capacity())?;
        let alias = match &self.owner {
            PageResourceOwner::Direct { .. } => 0,
            PageResourceOwner::Indirect { alias_chain, .. } => {
                retained_reference_bytes(alias_chain.capacity())?
            }
        };
        ancestor.checked_add(alias)
    }

    pub(crate) fn checked_retained_state_bytes(&self) -> Result<u64, DocumentError> {
        let chain_bytes = self.retained_lookup_chain_bytes().ok_or_else(|| {
            internal_error(self.defining_object, Some(self.defining_value_offset))
        })?;
        let syntax_heap_bytes = match &self.owner {
            PageResourceOwner::Direct { object } => object.syntax_heap_bytes(),
            PageResourceOwner::Indirect {
                terminal_object, ..
            } => terminal_object.syntax_heap_bytes(),
        };
        chain_bytes
            .checked_add(syntax_heap_bytes)
            .ok_or_else(|| internal_error(self.defining_object, Some(self.defining_value_offset)))
    }

    pub(crate) fn direct(
        defining_object: ObjectRef,
        defining_value_offset: u64,
        ancestor_lookup_chain: Vec<ObjectRef>,
        object: AttestedObject,
    ) -> Result<Self, DocumentError> {
        validate_ancestor_lookup_chain(defining_object, &ancestor_lookup_chain)?;
        if object.reference() != defining_object
            || direct_resource_dictionary(&object, defining_value_offset).is_none()
        {
            return Err(internal_error(defining_object, Some(defining_value_offset)));
        }
        Ok(Self {
            defining_object,
            defining_value_offset,
            ancestor_lookup_chain,
            owner: PageResourceOwner::Direct { object },
        })
    }

    pub(crate) fn indirect(
        defining_object: ObjectRef,
        defining_value_offset: u64,
        ancestor_lookup_chain: Vec<ObjectRef>,
        alias_chain: Vec<ObjectRef>,
        terminal_object: AttestedObject,
    ) -> Result<Self, DocumentError> {
        validate_ancestor_lookup_chain(defining_object, &ancestor_lookup_chain)?;
        if alias_chain.is_empty()
            || alias_chain.last().copied() != Some(terminal_object.reference())
            || terminal_resource_dictionary(&terminal_object).is_none()
        {
            return Err(internal_error(
                terminal_object.reference(),
                Some(defining_value_offset),
            ));
        }
        Ok(Self {
            defining_object,
            defining_value_offset,
            ancestor_lookup_chain,
            owner: PageResourceOwner::Indirect {
                terminal_object,
                alias_chain,
            },
        })
    }

    #[allow(
        dead_code,
        reason = "M2-05 content-stream resource lookup consumes this proof-preserving bridge"
    )]
    pub(crate) fn dictionary(&self) -> Result<&PdfDictionary, DocumentError> {
        match &self.owner {
            PageResourceOwner::Direct { object } => {
                direct_resource_dictionary(object, self.defining_value_offset).ok_or_else(|| {
                    internal_error(self.defining_object, Some(self.defining_value_offset))
                })
            }
            PageResourceOwner::Indirect {
                terminal_object, ..
            } => terminal_resource_dictionary(terminal_object).ok_or_else(|| {
                internal_error(
                    terminal_object.reference(),
                    Some(self.defining_value_offset),
                )
            }),
        }
    }
}

impl fmt::Debug for PageResourceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageResourceScope")
            .field("defining_object", &self.defining_object)
            .field("defining_value_offset", &self.defining_value_offset)
            .field("ancestor_lookup_depth", &self.ancestor_lookup_chain.len())
            .field("resource_object", &self.resource_object())
            .field("terminal_resource_object", &self.terminal_resource_object())
            .field("owner", &"[REDACTED]")
            .finish()
    }
}

fn validate_ancestor_lookup_chain(
    defining_object: ObjectRef,
    chain: &[ObjectRef],
) -> Result<(), DocumentError> {
    if chain.last().copied() != Some(defining_object) {
        return Err(internal_error(defining_object, None));
    }
    Ok(())
}

fn direct_resource_dictionary(
    object: &AttestedObject,
    defining_value_offset: u64,
) -> Option<&PdfDictionary> {
    let IndirectObjectValue::Direct(value) = object.value() else {
        return None;
    };
    let dictionary = value.value().as_dictionary()?;
    dictionary
        .entries()
        .iter()
        .find(|entry| {
            entry.key().value().bytes() == b"Resources"
                && entry.value().span().start() == defining_value_offset
        })
        .and_then(|entry| entry.value().value().as_dictionary())
}

fn terminal_resource_dictionary(object: &AttestedObject) -> Option<&PdfDictionary> {
    let IndirectObjectValue::Direct(value) = object.value() else {
        return None;
    };
    value.value().as_dictionary()
}

fn retained_reference_bytes(capacity: usize) -> Option<u64> {
    u64::try_from(capacity).ok().and_then(|capacity| {
        u64::try_from(mem::size_of::<ObjectRef>())
            .ok()
            .and_then(|bytes| capacity.checked_mul(bytes))
    })
}

const fn internal_error(reference: ObjectRef, offset: Option<u64>) -> DocumentError {
    DocumentError::for_code(DocumentErrorCode::InternalState, Some(reference), offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(number: u32) -> ObjectRef {
        ObjectRef::new(number, 0).expect("test reference is valid")
    }

    #[test]
    fn ancestor_chain_must_end_at_the_defining_object() {
        assert!(
            validate_ancestor_lookup_chain(reference(3), &[reference(7), reference(3)]).is_ok()
        );
        let error = validate_ancestor_lookup_chain(reference(3), &[reference(7), reference(4)])
            .expect_err("wrong defining endpoint must fail");
        assert_eq!(error.code(), DocumentErrorCode::InternalState);
        assert_eq!(error.reference(), Some(reference(3)));
    }
}
