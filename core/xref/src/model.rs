use std::fmt;

use pdf_rs_bytes::{SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::{ByteSpan, Located, ObjectRef, PdfDictionary};

/// Semantic payload of one traditional xref row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefEntryKind {
    /// A free entry whose offset field names the next free object number.
    Free {
        /// Next object number in the free-entry chain.
        next_free: u32,
    },
    /// An in-use entry whose offset points to an indirect object header.
    InUse {
        /// Absolute byte offset of the indirect object header.
        offset: u64,
    },
}

/// One validated traditional xref row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefEntry {
    object_number: u32,
    generation: u16,
    kind: XrefEntryKind,
}

impl XrefEntry {
    pub(crate) const fn new(object_number: u32, generation: u16, kind: XrefEntryKind) -> Self {
        Self {
            object_number,
            generation,
            kind,
        }
    }

    /// Returns the indexed object number, including reserved object zero.
    pub const fn object_number(self) -> u32 {
        self.object_number
    }

    /// Returns the entry generation.
    pub const fn generation(self) -> u16 {
        self.generation
    }

    /// Returns the validated free or in-use payload.
    pub const fn kind(self) -> XrefEntryKind {
        self.kind
    }
}

/// One source-bound traditional xref section and its validated trailer.
#[derive(Clone, Eq, PartialEq)]
pub struct XrefSection {
    snapshot: SourceSnapshot,
    startxref: u64,
    span: ByteSpan,
    declared_size: u32,
    root: ObjectRef,
    entries: Vec<XrefEntry>,
    trailer: Located<PdfDictionary>,
}

impl XrefSection {
    pub(crate) fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        span: ByteSpan,
        declared_size: u32,
        root: ObjectRef,
        entries: Vec<XrefEntry>,
        trailer: Located<PdfDictionary>,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            span,
            declared_size,
            root,
            entries,
            trailer,
        }
    }

    /// Returns the immutable source identity.
    pub const fn source(&self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot bound during parsing.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the final `startxref` value that located this section.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the exact span from `xref` through the trailer dictionary.
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Returns the validated trailer `/Size`.
    pub const fn declared_size(&self) -> u32 {
        self.declared_size
    }

    /// Returns the validated trailer `/Root` reference.
    pub const fn root(&self) -> ObjectRef {
        self.root
    }

    /// Returns entries in strictly increasing object-number order.
    pub fn entries(&self) -> &[XrefEntry] {
        &self.entries
    }

    /// Looks up one object number using the validated ordering.
    pub fn entry(&self, object_number: u32) -> Option<&XrefEntry> {
        self.entries
            .binary_search_by_key(&object_number, |entry| entry.object_number)
            .ok()
            .map(|index| &self.entries[index])
    }

    /// Returns the source-located trailer dictionary.
    pub const fn trailer(&self) -> &Located<PdfDictionary> {
        &self.trailer
    }
}

impl fmt::Debug for XrefSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XrefSection")
            .field("snapshot", &self.snapshot)
            .field("startxref", &self.startxref)
            .field("span", &self.span)
            .field("declared_size", &self.declared_size)
            .field("root", &self.root)
            .field("entry_count", &self.entries.len())
            .field("trailer", &"[REDACTED]")
            .finish()
    }
}
