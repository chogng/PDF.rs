use std::fmt;
use std::mem;

use pdf_rs_bytes::{ByteSource, SourceSnapshot};
use pdf_rs_object::IndirectObjectValue;
use pdf_rs_syntax::{ObjectRef, PdfDictionary, SyntaxObject};

use crate::{
    AttestedObject, DocumentCancellation, DocumentError, DocumentErrorCode, DocumentLimitKind,
    FontResourceUnsupported, FontResourceUnsupportedKind, ImageXObjectUnsupported,
    ImageXObjectUnsupportedKind, PageExtGStateLookupLimits, PageExtGStateLookupStats,
    PageFontLookupLimits, PageFontLookupStats, PagePropertyLookupLimits, PagePropertyLookupStats,
    PageXObjectLookupLimits, PageXObjectLookupStats, RevisionId,
};

const CANCELLATION_PROBE_INTERVAL: u64 = 256;

enum PageResourceOwner {
    Direct {
        object: AttestedObject,
    },
    Indirect {
        terminal_object: AttestedObject,
        alias_chain: Vec<ObjectRef>,
    },
    Form {
        object: AttestedObject,
        resources_offset: u64,
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
            PageResourceOwner::Direct { .. } | PageResourceOwner::Form { .. } => None,
            PageResourceOwner::Indirect { alias_chain, .. } => alias_chain.first().copied(),
        }
    }

    /// Returns the terminal non-reference resource object, or `None` for a direct dictionary.
    pub fn terminal_resource_object(&self) -> Option<ObjectRef> {
        match &self.owner {
            PageResourceOwner::Direct { .. } | PageResourceOwner::Form { .. } => None,
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
            PageResourceOwner::Direct { .. } | PageResourceOwner::Form { .. } => &[],
            PageResourceOwner::Indirect { alias_chain, .. } => alias_chain,
        }
    }

    /// Returns allocator-reported bytes reserved by the ancestor and Resources alias chains.
    pub fn retained_lookup_chain_bytes(&self) -> Option<u64> {
        let ancestor = retained_reference_bytes(self.ancestor_lookup_chain.capacity())?;
        let alias = match &self.owner {
            PageResourceOwner::Direct { .. } | PageResourceOwner::Form { .. } => 0,
            PageResourceOwner::Indirect { alias_chain, .. } => {
                retained_reference_bytes(alias_chain.capacity())?
            }
        };
        ancestor.checked_add(alias)
    }

    /// Creates a no-I/O resolver borrowing this exact resource dictionary proof.
    ///
    /// The resolver retains cumulative lookup and entry-visit accounting. It does not expose the
    /// underlying dictionary, open referenced objects, or allocate a copy of the requested name.
    pub const fn property_resolver(
        &self,
        limits: PagePropertyLookupLimits,
    ) -> PagePropertyResolver<'_> {
        PagePropertyResolver {
            scope: self,
            limits,
            stats: PagePropertyLookupStats {
                lookups: 0,
                entry_visits: 0,
            },
        }
    }

    /// Creates a no-I/O resolver borrowing this exact resource dictionary proof.
    ///
    /// The resolver returns only a fixed-size indirect-reference proof. It does not open,
    /// decode, or retain the selected XObject payload.
    pub const fn xobject_resolver(
        &self,
        limits: PageXObjectLookupLimits,
    ) -> PageXObjectResolver<'_> {
        PageXObjectResolver {
            scope: self,
            limits,
            stats: PageXObjectLookupStats {
                lookups: 0,
                entry_visits: 0,
            },
        }
    }

    /// Creates a no-I/O resolver borrowing this exact resource dictionary proof.
    ///
    /// The resolver returns only a fixed-size indirect-reference proof. It does not open or
    /// retain the selected font, descriptor, or embedded program.
    pub const fn font_resolver(&self, limits: PageFontLookupLimits) -> PageFontResolver<'_> {
        PageFontResolver {
            scope: self,
            limits,
            stats: PageFontLookupStats {
                lookups: 0,
                entry_visits: 0,
            },
        }
    }

    /// Creates a no-I/O resolver borrowing this exact resource dictionary proof.
    ///
    /// The resolver returns only a fixed-size indirect-reference proof. It does not open or
    /// interpret the selected external graphics-state dictionary.
    pub const fn ext_gstate_resolver(
        &self,
        limits: PageExtGStateLookupLimits,
    ) -> PageExtGStateResolver<'_> {
        PageExtGStateResolver {
            scope: self,
            limits,
            stats: PageExtGStateLookupStats {
                lookups: 0,
                entry_visits: 0,
            },
        }
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
            PageResourceOwner::Form { object, .. } => object.syntax_heap_bytes(),
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

    pub(crate) fn form(
        object: AttestedObject,
        resources_offset: u64,
    ) -> Result<Self, DocumentError> {
        let reference = object.reference();
        if form_resource_dictionary(&object, resources_offset).is_none() {
            return Err(internal_error(reference, Some(resources_offset)));
        }
        Ok(Self {
            defining_object: reference,
            defining_value_offset: resources_offset,
            ancestor_lookup_chain: vec![reference],
            owner: PageResourceOwner::Form {
                object,
                resources_offset,
            },
        })
    }

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
            PageResourceOwner::Form {
                object,
                resources_offset,
            } => form_resource_dictionary(object, *resources_offset)
                .ok_or_else(|| internal_error(object.reference(), Some(*resources_offset))),
        }
    }

    fn dictionary_owner(&self) -> &AttestedObject {
        match &self.owner {
            PageResourceOwner::Direct { object } => object,
            PageResourceOwner::Indirect {
                terminal_object, ..
            } => terminal_object,
            PageResourceOwner::Form { object, .. } => object,
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

/// Fixed-size proof that one marked-content property name selected an indirect object reference.
///
/// The requested name bytes are intentionally not retained or copied. Exact key/value offsets
/// bind this proof to the source occurrence, while callers that need the spelling continue to own
/// their already-bounded operator operand.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PagePropertyReference {
    target: ObjectRef,
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    scope_defining_object: ObjectRef,
    scope_defining_value_offset: u64,
    resource_dictionary_owner: ObjectRef,
    properties_key_offset: u64,
    properties_value_offset: u64,
    property_key_offset: u64,
    property_value_offset: u64,
}

impl PagePropertyReference {
    /// Returns the indirect object named by the selected property entry.
    pub const fn target(self) -> ObjectRef {
        self.target
    }

    /// Returns the immutable source snapshot retained by the resource dictionary owner.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned revision identity of the resource dictionary owner.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the `startxref` anchor of the resource dictionary owner's revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the Page or Pages object whose Resources field ended inheritance lookup.
    pub const fn scope_defining_object(self) -> ObjectRef {
        self.scope_defining_object
    }

    /// Returns the source offset of that exact Resources field value.
    pub const fn scope_defining_value_offset(self) -> u64 {
        self.scope_defining_value_offset
    }

    /// Returns the indirect object physically owning the selected resource dictionary.
    pub const fn resource_dictionary_owner(self) -> ObjectRef {
        self.resource_dictionary_owner
    }

    /// Returns the source offset of the unique `/Properties` key.
    pub const fn properties_key_offset(self) -> u64 {
        self.properties_key_offset
    }

    /// Returns the source offset of the unique `/Properties` value.
    pub const fn properties_value_offset(self) -> u64 {
        self.properties_value_offset
    }

    /// Returns the source offset of the selected property-name key.
    pub const fn property_key_offset(self) -> u64 {
        self.property_key_offset
    }

    /// Returns the source offset of the selected indirect-reference value.
    pub const fn property_value_offset(self) -> u64 {
        self.property_value_offset
    }
}

impl fmt::Debug for PagePropertyReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PagePropertyReference")
            .field("target", &self.target)
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("revision_startxref", &self.revision_startxref)
            .field("scope_defining_object", &self.scope_defining_object)
            .field(
                "scope_defining_value_offset",
                &self.scope_defining_value_offset,
            )
            .field("resource_dictionary_owner", &self.resource_dictionary_owner)
            .field("properties_key_offset", &self.properties_key_offset)
            .field("properties_value_offset", &self.properties_value_offset)
            .field("property_name", &"[NOT RETAINED]")
            .field("property_key_offset", &self.property_key_offset)
            .field("property_value_offset", &self.property_value_offset)
            .finish()
    }
}

/// Fixed-size proof that one Page resource name selected an indirect Font reference.
///
/// Requested name bytes are not retained. Exact category and selected-entry offsets bind this
/// proof to the source occurrence and the retained revision owner.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PageFontReference {
    target: ObjectRef,
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    scope_defining_object: ObjectRef,
    scope_defining_value_offset: u64,
    resource_dictionary_owner: ObjectRef,
    font_key_offset: u64,
    font_value_offset: u64,
    entry_key_offset: u64,
    entry_value_offset: u64,
}

impl PageFontReference {
    /// Returns the indirect font object named by the selected entry.
    pub const fn target(self) -> ObjectRef {
        self.target
    }
    /// Returns the immutable source snapshot retained by the resource owner.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }
    /// Returns the caller-assigned revision identity of the resource owner.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }
    /// Returns the `startxref` anchor of the resource owner's revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }
    /// Returns the Page or Pages object whose Resources field ended inheritance lookup.
    pub const fn scope_defining_object(self) -> ObjectRef {
        self.scope_defining_object
    }
    /// Returns the exact Resources value offset in the defining object.
    pub const fn scope_defining_value_offset(self) -> u64 {
        self.scope_defining_value_offset
    }
    /// Returns the indirect object physically owning the resource dictionary.
    pub const fn resource_dictionary_owner(self) -> ObjectRef {
        self.resource_dictionary_owner
    }
    /// Returns the source offset of the unique `/Font` key.
    pub const fn font_key_offset(self) -> u64 {
        self.font_key_offset
    }
    /// Returns the source offset of the unique `/Font` value.
    pub const fn font_value_offset(self) -> u64 {
        self.font_value_offset
    }
    /// Returns the source offset of the selected font-name key.
    pub const fn entry_key_offset(self) -> u64 {
        self.entry_key_offset
    }
    /// Returns the source offset of the selected indirect-reference value.
    pub const fn entry_value_offset(self) -> u64 {
        self.entry_value_offset
    }
}

impl fmt::Debug for PageFontReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageFontReference")
            .field("target", &self.target)
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("revision_startxref", &self.revision_startxref)
            .field("scope_defining_object", &self.scope_defining_object)
            .field(
                "scope_defining_value_offset",
                &self.scope_defining_value_offset,
            )
            .field("resource_dictionary_owner", &self.resource_dictionary_owner)
            .field("font_key_offset", &self.font_key_offset)
            .field("font_value_offset", &self.font_value_offset)
            .field("resource_name", &"[NOT RETAINED]")
            .field("entry_key_offset", &self.entry_key_offset)
            .field("entry_value_offset", &self.entry_value_offset)
            .finish()
    }
}

/// Terminal result of one no-I/O Page Font name lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageFontLookupOutcome {
    /// One exact indirect Font reference was proven.
    Ready(PageFontReference),
    /// The selected representation is valid but outside the registered subset.
    Unsupported(FontResourceUnsupported),
}

/// Borrowed no-I/O resolver for one exact inherited Page resource dictionary.
pub struct PageFontResolver<'scope> {
    scope: &'scope PageResourceScope,
    limits: PageFontLookupLimits,
    stats: PageFontLookupStats,
}

impl PageFontResolver<'_> {
    /// Returns the validated independent lookup and entry-visit profile.
    pub const fn limits(&self) -> PageFontLookupLimits {
        self.limits
    }
    /// Returns cumulative work, including work retained after failed lookups.
    pub const fn stats(&self) -> PageFontLookupStats {
        self.stats
    }

    /// Resolves one Page Font name without polling or opening the target object.
    ///
    /// This profile accepts only `/Font << /Name n 0 R >>`. Indirect category dictionaries and
    /// directly embedded selected Font dictionaries are typed unsupported outcomes. Missing,
    /// malformed, and duplicate structures are document failures.
    pub fn lookup_font(
        &mut self,
        name: &[u8],
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<PageFontLookupOutcome, DocumentError> {
        let scope = self.scope;
        let limits = self.limits;
        let stats = &mut self.stats;
        let owner = scope.dictionary_owner();
        let snapshot = owner.snapshot();
        let owner_reference = owner.reference();
        let scope_offset = scope.defining_value_offset;

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Err(fallback) = charge_font_lookup(stats, limits, owner_reference, scope_offset) {
            return Err(prioritize_runtime_error(
                snapshot,
                source,
                cancellation,
                fallback,
                owner_reference,
                scope_offset,
            ));
        }
        let dictionary = scope.dictionary().map_err(|fallback| {
            prioritize_runtime_error(
                snapshot,
                source,
                cancellation,
                fallback,
                owner_reference,
                scope_offset,
            )
        })?;

        let mut font_key_offset = None;
        let mut font_value = None;
        let mut duplicate_font_offset = None;
        for entry in dictionary.entries() {
            let key_offset = entry.key().span().start();
            probe_font_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) =
                charge_font_entry_visit(stats, limits, owner_reference, key_offset)
            {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != b"Font" {
                continue;
            }
            if font_value.is_some() {
                duplicate_font_offset.get_or_insert(key_offset);
            } else {
                font_key_offset = Some(key_offset);
                font_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Some(offset) = duplicate_font_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(font_value) = font_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageFontResource,
                Some(owner_reference),
                Some(scope_offset),
            ));
        };
        let font_value_offset = font_value.span().start();
        let Some(font_key_offset) = font_key_offset else {
            return Err(internal_error(owner_reference, Some(font_value_offset)));
        };
        let fonts = match font_value.value() {
            SyntaxObject::Dictionary(dictionary) => dictionary,
            SyntaxObject::Reference(reference) => {
                return Ok(PageFontLookupOutcome::Unsupported(
                    FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::IndirectFontDictionary,
                        *reference,
                        font_value_offset,
                    ),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageFontResource,
                    Some(owner_reference),
                    Some(font_value_offset),
                ));
            }
        };

        let mut entry_key_offset = None;
        let mut entry_value = None;
        let mut duplicate_entry_offset = None;
        for entry in fonts.entries() {
            let key_offset = entry.key().span().start();
            probe_font_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) =
                charge_font_entry_visit(stats, limits, owner_reference, key_offset)
            {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != name {
                continue;
            }
            if entry_value.is_some() {
                duplicate_entry_offset.get_or_insert(key_offset);
            } else {
                entry_key_offset = Some(key_offset);
                entry_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            font_value_offset,
        )?;
        if let Some(offset) = duplicate_entry_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(entry_value) = entry_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageFontResource,
                Some(owner_reference),
                Some(font_value_offset),
            ));
        };
        let entry_value_offset = entry_value.span().start();
        let Some(entry_key_offset) = entry_key_offset else {
            return Err(internal_error(owner_reference, Some(entry_value_offset)));
        };
        let target = match entry_value.value() {
            SyntaxObject::Reference(reference) => *reference,
            SyntaxObject::Dictionary(_) => {
                return Ok(PageFontLookupOutcome::Unsupported(
                    FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::DirectFont,
                        owner_reference,
                        entry_value_offset,
                    ),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageFontResource,
                    Some(owner_reference),
                    Some(entry_value_offset),
                ));
            }
        };

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            entry_value_offset,
        )?;
        Ok(PageFontLookupOutcome::Ready(PageFontReference {
            target,
            snapshot,
            revision_id: owner.revision_id(),
            revision_startxref: owner.revision_startxref(),
            scope_defining_object: scope.defining_object,
            scope_defining_value_offset: scope_offset,
            resource_dictionary_owner: owner_reference,
            font_key_offset,
            font_value_offset,
            entry_key_offset,
            entry_value_offset,
        }))
    }
}

impl fmt::Debug for PageFontResolver<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageFontResolver")
            .field("scope", &self.scope)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("dictionary", &"[REDACTED]")
            .finish()
    }
}

/// Fixed-size proof that one Page resource name selected an indirect ExtGState reference.
///
/// Requested name bytes are not retained. Exact category and selected-entry offsets bind this
/// proof to the source occurrence and retained revision owner.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PageExtGStateReference {
    target: ObjectRef,
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    scope_defining_object: ObjectRef,
    scope_defining_value_offset: u64,
    resource_dictionary_owner: ObjectRef,
    category_key_offset: u64,
    category_value_offset: u64,
    entry_key_offset: u64,
    entry_value_offset: u64,
}

impl PageExtGStateReference {
    /// Returns the indirect graphics-state object named by the selected entry.
    pub const fn target(self) -> ObjectRef {
        self.target
    }

    /// Returns the immutable source snapshot retained by the resource owner.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned revision identity of the resource owner.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the `startxref` anchor of the resource owner's revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the Page or Pages object whose Resources field ended inheritance lookup.
    pub const fn scope_defining_object(self) -> ObjectRef {
        self.scope_defining_object
    }

    /// Returns the exact Resources value offset in the defining object.
    pub const fn scope_defining_value_offset(self) -> u64 {
        self.scope_defining_value_offset
    }

    /// Returns the indirect object physically owning the resource dictionary.
    pub const fn resource_dictionary_owner(self) -> ObjectRef {
        self.resource_dictionary_owner
    }

    /// Returns the source offset of the unique `/ExtGState` key.
    pub const fn category_key_offset(self) -> u64 {
        self.category_key_offset
    }

    /// Returns the source offset of the unique `/ExtGState` value.
    pub const fn category_value_offset(self) -> u64 {
        self.category_value_offset
    }

    /// Returns the source offset of the selected graphics-state name key.
    pub const fn entry_key_offset(self) -> u64 {
        self.entry_key_offset
    }

    /// Returns the source offset of the selected indirect-reference value.
    pub const fn entry_value_offset(self) -> u64 {
        self.entry_value_offset
    }
}

impl fmt::Debug for PageExtGStateReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageExtGStateReference")
            .field("target", &self.target)
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("revision_startxref", &self.revision_startxref)
            .field("scope_defining_object", &self.scope_defining_object)
            .field(
                "scope_defining_value_offset",
                &self.scope_defining_value_offset,
            )
            .field("resource_dictionary_owner", &self.resource_dictionary_owner)
            .field("category_key_offset", &self.category_key_offset)
            .field("category_value_offset", &self.category_value_offset)
            .field("resource_name", &"[NOT RETAINED]")
            .field("entry_key_offset", &self.entry_key_offset)
            .field("entry_value_offset", &self.entry_value_offset)
            .finish()
    }
}

/// Borrowed no-I/O resolver for one exact inherited Page ExtGState dictionary.
pub struct PageExtGStateResolver<'scope> {
    scope: &'scope PageResourceScope,
    limits: PageExtGStateLookupLimits,
    stats: PageExtGStateLookupStats,
}

impl PageExtGStateResolver<'_> {
    /// Returns the validated independent lookup and entry-visit profile.
    pub const fn limits(&self) -> PageExtGStateLookupLimits {
        self.limits
    }

    /// Returns cumulative work, including work retained after failed lookups.
    pub const fn stats(&self) -> PageExtGStateLookupStats {
        self.stats
    }

    /// Resolves one Page ExtGState name without polling or opening the target object.
    ///
    /// This profile accepts only `/ExtGState << /Name n 0 R >>`. Indirect category dictionaries,
    /// direct selected dictionaries, malformed structures, and duplicates fail closed.
    pub fn lookup_ext_gstate(
        &mut self,
        name: &[u8],
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<PageExtGStateReference, DocumentError> {
        let scope = self.scope;
        let limits = self.limits;
        let stats = &mut self.stats;
        let owner = scope.dictionary_owner();
        let snapshot = owner.snapshot();
        let owner_reference = owner.reference();
        let scope_offset = scope.defining_value_offset;

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        charge_ext_gstate_lookup(stats, limits, owner_reference, scope_offset)?;
        let dictionary = scope.dictionary()?;

        let mut category_key_offset = None;
        let mut category_value = None;
        let mut duplicate_category_offset = None;
        for entry in dictionary.entries() {
            let key_offset = entry.key().span().start();
            probe_ext_gstate_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            charge_ext_gstate_entry_visit(stats, limits, owner_reference, key_offset)?;
            if entry.key().value().bytes() != b"ExtGState" {
                continue;
            }
            if category_value.is_some() {
                duplicate_category_offset.get_or_insert(key_offset);
            } else {
                category_key_offset = Some(key_offset);
                category_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Some(offset) = duplicate_category_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(category_value) = category_value else {
            return Err(invalid_ext_gstate(owner_reference, scope_offset));
        };
        let category_value_offset = category_value.span().start();
        let Some(category_key_offset) = category_key_offset else {
            return Err(internal_error(owner_reference, Some(category_value_offset)));
        };
        let SyntaxObject::Dictionary(states) = category_value.value() else {
            return Err(invalid_ext_gstate(owner_reference, category_value_offset));
        };

        let mut entry_key_offset = None;
        let mut entry_value = None;
        let mut duplicate_entry_offset = None;
        for entry in states.entries() {
            let key_offset = entry.key().span().start();
            probe_ext_gstate_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            charge_ext_gstate_entry_visit(stats, limits, owner_reference, key_offset)?;
            if entry.key().value().bytes() != name {
                continue;
            }
            if entry_value.is_some() {
                duplicate_entry_offset.get_or_insert(key_offset);
            } else {
                entry_key_offset = Some(key_offset);
                entry_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            category_value_offset,
        )?;
        if let Some(offset) = duplicate_entry_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(entry_value) = entry_value else {
            return Err(invalid_ext_gstate(owner_reference, category_value_offset));
        };
        let entry_value_offset = entry_value.span().start();
        let Some(entry_key_offset) = entry_key_offset else {
            return Err(internal_error(owner_reference, Some(entry_value_offset)));
        };
        let SyntaxObject::Reference(target) = entry_value.value() else {
            return Err(invalid_ext_gstate(owner_reference, entry_value_offset));
        };

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            entry_value_offset,
        )?;
        Ok(PageExtGStateReference {
            target: *target,
            snapshot,
            revision_id: owner.revision_id(),
            revision_startxref: owner.revision_startxref(),
            scope_defining_object: scope.defining_object,
            scope_defining_value_offset: scope_offset,
            resource_dictionary_owner: owner_reference,
            category_key_offset,
            category_value_offset,
            entry_key_offset,
            entry_value_offset,
        })
    }
}

impl fmt::Debug for PageExtGStateResolver<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageExtGStateResolver")
            .field("scope", &self.scope)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("dictionary", &"[REDACTED]")
            .finish()
    }
}

/// Fixed-size proof that one Page resource name selected an indirect XObject reference.
///
/// The requested name bytes are intentionally not retained or copied. Exact key/value offsets
/// bind the proof to the source occurrence, while acquisition remains separately proof-bound to
/// the retained revision authority.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PageXObjectReference {
    target: ObjectRef,
    snapshot: SourceSnapshot,
    revision_id: RevisionId,
    revision_startxref: u64,
    scope_defining_object: ObjectRef,
    scope_defining_value_offset: u64,
    resource_dictionary_owner: ObjectRef,
    xobject_key_offset: u64,
    xobject_value_offset: u64,
    entry_key_offset: u64,
    entry_value_offset: u64,
}

impl PageXObjectReference {
    /// Returns the indirect object named by the selected XObject entry.
    pub const fn target(self) -> ObjectRef {
        self.target
    }

    /// Returns the immutable source snapshot retained by the resource dictionary owner.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the caller-assigned revision identity of the resource dictionary owner.
    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    /// Returns the `startxref` anchor of the resource dictionary owner's revision.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the Page or Pages object whose Resources field ended inheritance lookup.
    pub const fn scope_defining_object(self) -> ObjectRef {
        self.scope_defining_object
    }

    /// Returns the source offset of that exact Resources field value.
    pub const fn scope_defining_value_offset(self) -> u64 {
        self.scope_defining_value_offset
    }

    /// Returns the indirect object physically owning the selected resource dictionary.
    pub const fn resource_dictionary_owner(self) -> ObjectRef {
        self.resource_dictionary_owner
    }

    /// Returns the source offset of the unique `/XObject` key.
    pub const fn xobject_key_offset(self) -> u64 {
        self.xobject_key_offset
    }

    /// Returns the source offset of the unique `/XObject` value.
    pub const fn xobject_value_offset(self) -> u64 {
        self.xobject_value_offset
    }

    /// Returns the source offset of the selected resource-name key.
    pub const fn entry_key_offset(self) -> u64 {
        self.entry_key_offset
    }

    /// Returns the source offset of the selected indirect-reference value.
    pub const fn entry_value_offset(self) -> u64 {
        self.entry_value_offset
    }
}

impl fmt::Debug for PageXObjectReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageXObjectReference")
            .field("target", &self.target)
            .field("snapshot", &self.snapshot)
            .field("revision_id", &self.revision_id)
            .field("revision_startxref", &self.revision_startxref)
            .field("scope_defining_object", &self.scope_defining_object)
            .field(
                "scope_defining_value_offset",
                &self.scope_defining_value_offset,
            )
            .field("resource_dictionary_owner", &self.resource_dictionary_owner)
            .field("xobject_key_offset", &self.xobject_key_offset)
            .field("xobject_value_offset", &self.xobject_value_offset)
            .field("resource_name", &"[NOT RETAINED]")
            .field("entry_key_offset", &self.entry_key_offset)
            .field("entry_value_offset", &self.entry_value_offset)
            .finish()
    }
}

/// Terminal result of one no-I/O Page XObject name lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageXObjectLookupOutcome {
    /// One exact indirect XObject reference was proven.
    Ready(PageXObjectReference),
    /// The selected resource representation is valid but outside the registered subset.
    Unsupported(ImageXObjectUnsupported),
}

/// Borrowed no-I/O resolver for one exact inherited Page resource dictionary.
pub struct PageXObjectResolver<'scope> {
    scope: &'scope PageResourceScope,
    limits: PageXObjectLookupLimits,
    stats: PageXObjectLookupStats,
}

impl PageXObjectResolver<'_> {
    /// Returns the validated independent lookup and entry-visit profile.
    pub const fn limits(&self) -> PageXObjectLookupLimits {
        self.limits
    }

    /// Returns cumulative work, including work retained after failed lookups.
    pub const fn stats(&self) -> PageXObjectLookupStats {
        self.stats
    }

    /// Resolves one Page XObject name without polling or opening the target object.
    ///
    /// This bounded profile accepts only `/XObject << /Name n 0 R >>`. An indirect
    /// `/XObject` dictionary and a directly embedded selected XObject are reported through the
    /// typed unsupported boundary. Malformed, missing, or duplicate structures remain document
    /// failures.
    pub fn lookup_image_xobject(
        &mut self,
        name: &[u8],
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<PageXObjectLookupOutcome, DocumentError> {
        let scope = self.scope;
        let limits = self.limits;
        let stats = &mut self.stats;
        let owner = scope.dictionary_owner();
        let snapshot = owner.snapshot();
        let owner_reference = owner.reference();
        let scope_offset = scope.defining_value_offset;

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Err(fallback) = charge_xobject_lookup(stats, limits, owner_reference, scope_offset) {
            return Err(prioritize_runtime_error(
                snapshot,
                source,
                cancellation,
                fallback,
                owner_reference,
                scope_offset,
            ));
        }
        let dictionary = scope.dictionary().map_err(|fallback| {
            prioritize_runtime_error(
                snapshot,
                source,
                cancellation,
                fallback,
                owner_reference,
                scope_offset,
            )
        })?;

        let mut xobject_key_offset = None;
        let mut xobject_value = None;
        let mut duplicate_xobject_offset = None;
        for entry in dictionary.entries() {
            let key_offset = entry.key().span().start();
            probe_xobject_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) =
                charge_xobject_entry_visit(stats, limits, owner_reference, key_offset)
            {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != b"XObject" {
                continue;
            }
            if xobject_value.is_some() {
                duplicate_xobject_offset.get_or_insert(key_offset);
            } else {
                xobject_key_offset = Some(key_offset);
                xobject_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Some(offset) = duplicate_xobject_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(xobject_value) = xobject_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageXObjectResource,
                Some(owner_reference),
                Some(scope_offset),
            ));
        };
        let xobject_value_offset = xobject_value.span().start();
        let Some(xobject_key_offset) = xobject_key_offset else {
            return Err(internal_error(owner_reference, Some(xobject_value_offset)));
        };
        let xobjects = match xobject_value.value() {
            SyntaxObject::Dictionary(dictionary) => dictionary,
            SyntaxObject::Reference(reference) => {
                return Ok(PageXObjectLookupOutcome::Unsupported(
                    ImageXObjectUnsupported::new(
                        ImageXObjectUnsupportedKind::IndirectXObjectDictionary,
                        *reference,
                        xobject_value_offset,
                    ),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageXObjectResource,
                    Some(owner_reference),
                    Some(xobject_value_offset),
                ));
            }
        };

        let mut entry_key_offset = None;
        let mut entry_value = None;
        let mut duplicate_entry_offset = None;
        for entry in xobjects.entries() {
            let key_offset = entry.key().span().start();
            probe_xobject_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) =
                charge_xobject_entry_visit(stats, limits, owner_reference, key_offset)
            {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != name {
                continue;
            }
            if entry_value.is_some() {
                duplicate_entry_offset.get_or_insert(key_offset);
            } else {
                entry_key_offset = Some(key_offset);
                entry_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            xobject_value_offset,
        )?;
        if let Some(offset) = duplicate_entry_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(entry_value) = entry_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPageXObjectResource,
                Some(owner_reference),
                Some(xobject_value_offset),
            ));
        };
        let entry_value_offset = entry_value.span().start();
        let Some(entry_key_offset) = entry_key_offset else {
            return Err(internal_error(owner_reference, Some(entry_value_offset)));
        };
        let target = match entry_value.value() {
            SyntaxObject::Reference(reference) => *reference,
            SyntaxObject::Dictionary(_) => {
                return Ok(PageXObjectLookupOutcome::Unsupported(
                    ImageXObjectUnsupported::new(
                        ImageXObjectUnsupportedKind::DirectXObject,
                        owner_reference,
                        entry_value_offset,
                    ),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPageXObjectResource,
                    Some(owner_reference),
                    Some(entry_value_offset),
                ));
            }
        };

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            entry_value_offset,
        )?;
        Ok(PageXObjectLookupOutcome::Ready(PageXObjectReference {
            target,
            snapshot,
            revision_id: owner.revision_id(),
            revision_startxref: owner.revision_startxref(),
            scope_defining_object: scope.defining_object,
            scope_defining_value_offset: scope_offset,
            resource_dictionary_owner: owner_reference,
            xobject_key_offset,
            xobject_value_offset,
            entry_key_offset,
            entry_value_offset,
        }))
    }
}

impl fmt::Debug for PageXObjectResolver<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageXObjectResolver")
            .field("scope", &self.scope)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("dictionary", &"[REDACTED]")
            .finish()
    }
}

/// Borrowed no-I/O resolver for one exact inherited page resource dictionary.
pub struct PagePropertyResolver<'scope> {
    scope: &'scope PageResourceScope,
    limits: PagePropertyLookupLimits,
    stats: PagePropertyLookupStats,
}

impl PagePropertyResolver<'_> {
    /// Returns the validated independent lookup and entry-visit profile.
    pub const fn limits(&self) -> PagePropertyLookupLimits {
        self.limits
    }

    /// Returns cumulative work, including work retained after failed lookups.
    pub const fn stats(&self) -> PagePropertyLookupStats {
        self.stats
    }

    /// Resolves one marked-content property name without polling or opening the target object.
    ///
    /// This bounded profile accepts only `/Properties << /Name n 0 R >>`. An indirect
    /// `/Properties` dictionary and a direct property dictionary are reported as distinct
    /// structured unsupported features. The fixed-size result proves only the exact indirect
    /// reference syntax and its resource-dictionary provenance; it deliberately does not attest
    /// or open the referenced target.
    pub fn lookup_marked_content_property(
        &mut self,
        name: &[u8],
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<PagePropertyReference, DocumentError> {
        let scope = self.scope;
        let limits = self.limits;
        let stats = &mut self.stats;
        let owner = scope.dictionary_owner();
        let snapshot = owner.snapshot();
        let owner_reference = owner.reference();
        let scope_offset = scope.defining_value_offset;

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Err(fallback) = charge_lookup(stats, limits, owner_reference, scope_offset) {
            return Err(prioritize_runtime_error(
                snapshot,
                source,
                cancellation,
                fallback,
                owner_reference,
                scope_offset,
            ));
        }
        let dictionary = match scope.dictionary() {
            Ok(dictionary) => dictionary,
            Err(fallback) => {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    scope_offset,
                ));
            }
        };

        let mut properties_key_offset = None;
        let mut properties_value = None;
        let mut duplicate_properties_offset = None;
        for entry in dictionary.entries() {
            let key_offset = entry.key().span().start();
            probe_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) = charge_entry_visit(stats, limits, owner_reference, key_offset) {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != b"Properties" {
                continue;
            }
            if properties_value.is_some() {
                duplicate_properties_offset.get_or_insert(key_offset);
            } else {
                properties_key_offset = Some(key_offset);
                properties_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            scope_offset,
        )?;
        if let Some(offset) = duplicate_properties_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(properties_value) = properties_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPagePropertyResource,
                Some(owner_reference),
                Some(scope_offset),
            ));
        };
        let properties_value_offset = properties_value.span().start();
        let Some(properties_key_offset) = properties_key_offset else {
            return Err(internal_error(
                owner_reference,
                Some(properties_value_offset),
            ));
        };
        let properties = match properties_value.value() {
            SyntaxObject::Dictionary(dictionary) => dictionary,
            SyntaxObject::Reference(reference) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedIndirectPageProperties,
                    Some(*reference),
                    Some(properties_value_offset),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPagePropertyResource,
                    Some(owner_reference),
                    Some(properties_value_offset),
                ));
            }
        };

        let mut property_key_offset = None;
        let mut property_value = None;
        let mut duplicate_property_offset = None;
        for entry in properties.entries() {
            let key_offset = entry.key().span().start();
            probe_scan(
                stats,
                snapshot,
                source,
                cancellation,
                owner_reference,
                key_offset,
            )?;
            if let Err(fallback) = charge_entry_visit(stats, limits, owner_reference, key_offset) {
                return Err(prioritize_runtime_error(
                    snapshot,
                    source,
                    cancellation,
                    fallback,
                    owner_reference,
                    key_offset,
                ));
            }
            if entry.key().value().bytes() != name {
                continue;
            }
            if property_value.is_some() {
                duplicate_property_offset.get_or_insert(key_offset);
            } else {
                property_key_offset = Some(key_offset);
                property_value = Some(entry.value());
            }
        }
        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            properties_value_offset,
        )?;
        if let Some(offset) = duplicate_property_offset {
            return Err(DocumentError::for_code(
                DocumentErrorCode::DuplicateStructuralKey,
                Some(owner_reference),
                Some(offset),
            ));
        }
        let Some(property_value) = property_value else {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidPagePropertyResource,
                Some(owner_reference),
                Some(properties_value_offset),
            ));
        };
        let property_value_offset = property_value.span().start();
        let Some(property_key_offset) = property_key_offset else {
            return Err(internal_error(owner_reference, Some(property_value_offset)));
        };
        let target = match property_value.value() {
            SyntaxObject::Reference(reference) => *reference,
            SyntaxObject::Dictionary(_) => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::UnsupportedDirectPagePropertyDictionary,
                    Some(owner_reference),
                    Some(property_value_offset),
                ));
            }
            _ => {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidPagePropertyResource,
                    Some(owner_reference),
                    Some(property_value_offset),
                ));
            }
        };

        runtime_guard(
            snapshot,
            source,
            cancellation,
            owner_reference,
            property_value_offset,
        )?;
        Ok(PagePropertyReference {
            target,
            snapshot,
            revision_id: owner.revision_id(),
            revision_startxref: owner.revision_startxref(),
            scope_defining_object: scope.defining_object,
            scope_defining_value_offset: scope_offset,
            resource_dictionary_owner: owner_reference,
            properties_key_offset,
            properties_value_offset,
            property_key_offset,
            property_value_offset,
        })
    }
}

impl fmt::Debug for PagePropertyResolver<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PagePropertyResolver")
            .field("scope", &self.scope)
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("dictionary", &"[REDACTED]")
            .finish()
    }
}

fn probe_scan(
    stats: &PagePropertyLookupStats,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits != 0
        && stats
            .entry_visits
            .is_multiple_of(CANCELLATION_PROBE_INTERVAL)
    {
        runtime_guard(snapshot, source, cancellation, reference, offset)?;
    }
    Ok(())
}

fn charge_lookup(
    stats: &mut PagePropertyLookupStats,
    limits: PagePropertyLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.lookups >= limits.max_lookups() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PagePropertyLookups,
            limits.max_lookups(),
            stats.lookups,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.lookups = stats
        .lookups
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn charge_entry_visit(
    stats: &mut PagePropertyLookupStats,
    limits: PagePropertyLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits >= limits.max_entry_visits() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PagePropertyEntryVisits,
            limits.max_entry_visits(),
            stats.entry_visits,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.entry_visits = stats
        .entry_visits
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn probe_xobject_scan(
    stats: &PageXObjectLookupStats,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits != 0
        && stats
            .entry_visits
            .is_multiple_of(CANCELLATION_PROBE_INTERVAL)
    {
        runtime_guard(snapshot, source, cancellation, reference, offset)?;
    }
    Ok(())
}

fn charge_xobject_lookup(
    stats: &mut PageXObjectLookupStats,
    limits: PageXObjectLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.lookups >= limits.max_lookups() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageXObjectLookups,
            limits.max_lookups(),
            stats.lookups,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.lookups = stats
        .lookups
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn charge_xobject_entry_visit(
    stats: &mut PageXObjectLookupStats,
    limits: PageXObjectLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits >= limits.max_entry_visits() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageXObjectEntryVisits,
            limits.max_entry_visits(),
            stats.entry_visits,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.entry_visits = stats
        .entry_visits
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn probe_font_scan(
    stats: &PageFontLookupStats,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits != 0
        && stats
            .entry_visits
            .is_multiple_of(CANCELLATION_PROBE_INTERVAL)
    {
        runtime_guard(snapshot, source, cancellation, reference, offset)?;
    }
    Ok(())
}

fn charge_font_lookup(
    stats: &mut PageFontLookupStats,
    limits: PageFontLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.lookups >= limits.max_lookups() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageFontLookups,
            limits.max_lookups(),
            stats.lookups,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.lookups = stats
        .lookups
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn charge_font_entry_visit(
    stats: &mut PageFontLookupStats,
    limits: PageFontLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits >= limits.max_entry_visits() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageFontEntryVisits,
            limits.max_entry_visits(),
            stats.entry_visits,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.entry_visits = stats
        .entry_visits
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn probe_ext_gstate_scan(
    stats: &PageExtGStateLookupStats,
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits != 0
        && stats
            .entry_visits
            .is_multiple_of(CANCELLATION_PROBE_INTERVAL)
    {
        runtime_guard(snapshot, source, cancellation, reference, offset)?;
    }
    Ok(())
}

fn charge_ext_gstate_lookup(
    stats: &mut PageExtGStateLookupStats,
    limits: PageExtGStateLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.lookups >= limits.max_lookups() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageExtGStateLookups,
            limits.max_lookups(),
            stats.lookups,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.lookups = stats
        .lookups
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn charge_ext_gstate_entry_visit(
    stats: &mut PageExtGStateLookupStats,
    limits: PageExtGStateLookupLimits,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if stats.entry_visits >= limits.max_entry_visits() {
        return Err(DocumentError::page_property_resource(
            DocumentLimitKind::PageExtGStateEntryVisits,
            limits.max_entry_visits(),
            stats.entry_visits,
            1,
            reference,
            Some(offset),
        ));
    }
    stats.entry_visits = stats
        .entry_visits
        .checked_add(1)
        .ok_or_else(|| internal_error(reference, Some(offset)))?;
    Ok(())
}

fn invalid_ext_gstate(reference: ObjectRef, offset: u64) -> DocumentError {
    DocumentError::for_code(
        DocumentErrorCode::InvalidPageExtGStateResource,
        Some(reference),
        Some(offset),
    )
}

fn runtime_guard(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    reference: ObjectRef,
    offset: u64,
) -> Result<(), DocumentError> {
    if source.snapshot() != snapshot {
        return Err(DocumentError::for_code(
            DocumentErrorCode::SourceSnapshotMismatch,
            Some(reference),
            Some(offset),
        ));
    }
    let cancelled = cancellation.is_cancelled();
    if source.snapshot() != snapshot {
        return Err(DocumentError::for_code(
            DocumentErrorCode::SourceSnapshotMismatch,
            Some(reference),
            Some(offset),
        ));
    }
    if cancelled {
        return Err(DocumentError::for_code(
            DocumentErrorCode::Cancelled,
            Some(reference),
            Some(offset),
        ));
    }
    Ok(())
}

fn prioritize_runtime_error(
    snapshot: SourceSnapshot,
    source: &dyn ByteSource,
    cancellation: &dyn DocumentCancellation,
    fallback: DocumentError,
    default_reference: ObjectRef,
    default_offset: u64,
) -> DocumentError {
    let reference = fallback.reference().unwrap_or(default_reference);
    let offset = fallback.offset().unwrap_or(default_offset);
    match runtime_guard(snapshot, source, cancellation, reference, offset) {
        Ok(()) => fallback,
        Err(runtime) => runtime,
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

fn form_resource_dictionary(
    object: &AttestedObject,
    resources_offset: u64,
) -> Option<&PdfDictionary> {
    let IndirectObjectValue::Stream(stream) = object.value() else {
        return None;
    };
    stream
        .dictionary()
        .value()
        .entries()
        .iter()
        .find(|entry| {
            entry.key().value().bytes() == b"Resources"
                && entry.value().span().start() == resources_offset
        })
        .and_then(|entry| entry.value().value().as_dictionary())
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
