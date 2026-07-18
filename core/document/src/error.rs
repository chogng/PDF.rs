use std::error::Error;
use std::fmt;

use pdf_rs_bytes::{SourceError, SourceErrorCategory, SourceRecoverability};
use pdf_rs_object::{ObjectError, ObjectErrorCategory, ObjectErrorCode, ObjectRecoverability};
use pdf_rs_syntax::{ObjectRef, SyntaxError, SyntaxErrorCategory, SyntaxRecoverability};

use crate::text_string::{
    TextStringError, TextStringErrorCategory, TextStringErrorCode, TextStringLimitKind,
    TextStringRecoverability,
};

/// Deterministic document-composition budget that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentLimitKind {
    /// Total xref rows in one candidate revision.
    TotalEntries,
    /// In-use xref rows in one candidate revision.
    InUseEntries,
    /// Conservatively accounted allocator-reported logical and physical entry capacity.
    LogicalIndexBytes,
    /// Comparisons and swaps performed while sorting by physical offset.
    SortSteps,
    /// Object and xref anchors retained by the revision-aware resolver index.
    RevisionResolverAnchors,
    /// Allocator-reported bytes retained for revision-aware physical anchors.
    RevisionResolverIndexBytes,
    /// Comparisons and swaps performed while sorting revision-aware physical anchors.
    RevisionResolverSortSteps,
    /// Cumulative exact source bytes across target and indirect-Length resolver children.
    RevisionResolverObjectReadBytes,
    /// Cumulative parser-window bytes across target and indirect-Length resolver children.
    RevisionResolverObjectParseBytes,
    /// Complete source-acquisition proof, cloned revision semantics, and resolver anchors.
    AcquiredDocumentRetainedBytes,
    /// Cumulative exact source bytes used to resolve one acquired-chain object.
    AcquiredObjectReadBytes,
    /// Cumulative framing, decoding, and direct-syntax bytes for one acquired-chain object.
    AcquiredObjectParseBytes,
    /// Framing, decode, and object-stream proof storage retained by one resolved object.
    AcquiredObjectRetainedBytes,
    /// Fallible bounded index-capacity reservation using conservative byte accounting.
    Allocation,
    /// Immutable source bytes addressable by the revision-attestation profile.
    AttestationSourceBytes,
    /// In-use objects framed by one revision-attestation job.
    AttestationObjects,
    /// Cumulative prefix and inter-object trivia bytes.
    AttestationTriviaBytes,
    /// Bytes in one top-level PDF comment.
    AttestationCommentBytes,
    /// Cumulative exact ranges requested by child object jobs.
    AttestationObjectReadBytes,
    /// Cumulative parser-window bytes charged by child object jobs.
    AttestationObjectParseBytes,
    /// Conservatively accounted allocator capacity for retained fixed-size evidence.
    AttestationEvidenceBytes,
    /// Objects started by one local-repair first-pass probe.
    RepairProbeObjects,
    /// Cumulative exact source bytes charged by local-repair first-pass objects.
    RepairProbeReadBytes,
    /// Cumulative parser-window bytes charged by local-repair first-pass objects.
    RepairProbeParseBytes,
    /// Cumulative repair-only source bytes scanned by local-repair first-pass objects.
    RepairProbeScanBytes,
    /// Matching object-header candidates accepted by local-repair first-pass scans.
    RepairProbeHeaderCandidates,
    /// Stream-boundary candidates accepted by local-repair first-pass scans.
    RepairProbeBoundaryCandidates,
    /// Allocator-reported capacity retained for the complete first-pass repair proof plan.
    RepairProbeEvidenceBytes,
    /// Indirect objects started by one bounded reference-chain job.
    ReferenceChainObjects,
    /// Top-level indirect-reference edges followed by one bounded reference-chain job.
    ReferenceChainEdges,
    /// Distinct exact references retained in one active reference chain.
    ReferenceChainDepth,
    /// Cumulative exact object-read bytes across one reference-chain job.
    ReferenceChainObjectReadBytes,
    /// Cumulative object-parser window bytes across one reference-chain job.
    ReferenceChainObjectParseBytes,
    /// Allocator-reported capacity retained for one reference-chain path.
    ReferenceChainPathBytes,
    /// Page or Pages nodes started by one bounded page-count job.
    PageTreeNodes,
    /// Greatest root-relative Page/Pages depth accepted by one page-count job.
    PageTreeDepth,
    /// Leaf Page objects accepted by one page-count job.
    PageTreePages,
    /// Direct children declared by one Pages node.
    PageTreeKids,
    /// Cumulative exact object-read bytes across one page-count job.
    PageTreeObjectReadBytes,
    /// Cumulative object-parser window bytes across one page-count job.
    PageTreeObjectParseBytes,
    /// Allocator-reported work-stack and visited-reference capacity.
    PageTreeTraversalBytes,
    /// Allocator-reported ordered page-reference capacity retained by one page index.
    PageIndexBytes,
    /// Page or Pages dictionaries opened while resolving inherited page values.
    PageMaterializationAncestors,
    /// Proof-preserving object jobs started across page ancestors and value aliases.
    PageMaterializationObjects,
    /// Whole-object direct-reference edges followed for inherited page values.
    PageMaterializationReferenceEdges,
    /// Cumulative exact object-read bytes across one page materialization job.
    PageMaterializationObjectReadBytes,
    /// Cumulative object-parser window bytes across one page materialization job.
    PageMaterializationObjectParseBytes,
    /// Allocator-reported state and proof-bearing value capacity retained by materialization.
    PageMaterializationStateBytes,
    /// Ordered content streams published for one exact Page.
    PageContentStreams,
    /// Entries admitted from one direct or aliased Page Contents array.
    PageContentArrayEntries,
    /// Whole-object Contents aliases active on one resolution chain.
    PageContentAliasDepth,
    /// Proof-preserving Page, alias, and stream object jobs started during acquisition.
    PageContentObjects,
    /// Whole-object Contents alias and array-entry reference edges followed.
    PageContentReferenceEdges,
    /// Cumulative exact object-read bytes across one page-content acquisition job.
    PageContentObjectReadBytes,
    /// Cumulative object-parser window bytes across one page-content acquisition job.
    PageContentObjectParseBytes,
    /// Cumulative exact encoded stream-payload bytes acquired for one Page.
    PageContentEncodedBytes,
    /// Cumulative final decoded content bytes retained for one Page.
    PageContentDecodedBytes,
    /// Cumulative deterministic stream-decoder fuel consumed for one Page.
    PageContentDecodeFuel,
    /// Allocator-reported acquisition state and proof-bearing decoded capacity retained.
    PageContentRetainedStateBytes,
    /// Encoded input bytes admitted by one stream's intrinsic decoder profile.
    PageContentStreamInputBytes,
    /// Filter stages admitted by one stream's intrinsic decoder profile.
    PageContentStreamFilters,
    /// Canonical filter-plan bytes admitted by one stream's intrinsic decoder profile.
    PageContentStreamFilterPlanBytes,
    /// One filter layer's output bytes admitted by one stream's intrinsic decoder profile.
    PageContentStreamLayerOutputBytes,
    /// Cumulative filter output bytes admitted by one stream's intrinsic decoder profile.
    PageContentStreamTotalOutputBytes,
    /// Final output bytes admitted by one stream's intrinsic decoder profile.
    PageContentStreamFinalOutputBytes,
    /// Deterministic fuel admitted by one stream's intrinsic decoder profile.
    PageContentStreamDecodeFuel,
    /// Decoder-owned retained capacity admitted by one stream's intrinsic profile.
    PageContentStreamRetainedBytes,
    /// Marked-content property names resolved through one borrowed page-resource resolver.
    PagePropertyLookups,
    /// Outer resource and inner property dictionary entries visited during property lookup.
    PagePropertyEntryVisits,
    /// Page resource names resolved through one borrowed XObject resolver.
    PageXObjectLookups,
    /// Outer resource and inner XObject dictionary entries visited during XObject lookup.
    PageXObjectEntryVisits,
    /// Page resource names resolved through one borrowed Font resolver.
    PageFontLookups,
    /// Outer resource and inner Font dictionary entries visited during Font lookup.
    PageFontEntryVisits,
    /// Page external graphics-state names resolved through one borrowed resource resolver.
    PageExtGStateLookups,
    /// Outer resource and inner ExtGState dictionary entries visited during lookup.
    PageExtGStateEntryVisits,
    /// Polls admitted while one embedded Font acquisition remained active.
    FontResourcePolls,
    /// Proof-preserving Font, descriptor, and FontFile2 objects opened.
    FontResourceObjects,
    /// Font-to-descriptor and descriptor-to-program indirect reference edges followed.
    FontResourceReferenceEdges,
    /// Top-level Font, descriptor, and FontFile2 metadata entries visited.
    FontResourceMetadataEntries,
    /// Entries admitted from the direct PDF `/Widths` array.
    FontResourceWidths,
    /// Cumulative exact source bytes consumed by proof-bound Font object jobs.
    FontResourceObjectReadBytes,
    /// Cumulative parser-window bytes consumed by proof-bound Font object jobs.
    FontResourceObjectParseBytes,
    /// Exact encoded FontFile2 payload bytes.
    FontResourceEncodedBytes,
    /// Exact decoded TrueType program bytes.
    FontResourceDecodedBytes,
    /// Deterministic foundational stream-decoder fuel for FontFile2.
    FontResourceDecodeFuel,
    /// Deterministic lower TrueType parser work.
    FontResourceParserWork,
    /// Records in the embedded sfnt table directory.
    FontResourceTables,
    /// Glyphs declared by the embedded TrueType `maxp` table.
    FontResourceGlyphs,
    /// Segments in the selected embedded TrueType character map.
    FontResourceCmapSegments,
    /// Bytes addressed by the embedded TrueType `glyf`/`loca` pair.
    FontResourceGlyphDataBytes,
    /// Bytes in one embedded TrueType glyph description.
    FontResourceGlyphBytes,
    /// Contours in one embedded simple TrueType glyph.
    FontResourceGlyphContours,
    /// Source contours across all embedded simple TrueType glyphs.
    FontResourceTotalContours,
    /// Points in one embedded simple TrueType glyph.
    FontResourceGlyphPoints,
    /// Source points across all embedded simple TrueType glyphs.
    FontResourceTotalPoints,
    /// Direct component records across all embedded compound TrueType glyphs.
    FontResourceComponents,
    /// Recursive embedded compound-glyph expansion depth.
    FontResourceComponentDepth,
    /// Project-owned outline segments after embedded compound-glyph expansion.
    FontResourcePathSegments,
    /// Conservatively accounted objects, decoded program, and parsed TrueType state.
    FontResourceRetainedBytes,
    /// Top-level and nested Image XObject metadata entries visited before decode.
    ImageXObjectMetadataEntries,
    /// Exact source bytes consumed while reopening one proof-bound Image XObject.
    ImageXObjectObjectReadBytes,
    /// Parser-window bytes consumed while reopening one proof-bound Image XObject.
    ImageXObjectObjectParseBytes,
    /// Positive pixel columns declared by one Image XObject.
    ImageXObjectWidth,
    /// Positive pixel rows declared by one Image XObject.
    ImageXObjectHeight,
    /// Checked source pixels declared by one Image XObject.
    ImageXObjectPixels,
    /// Tightly packed decoded bytes in one Image XObject row.
    ImageXObjectStrideBytes,
    /// Exact encoded stream-payload bytes for one Image XObject.
    ImageXObjectEncodedBytes,
    /// Exact final decoded component bytes for one Image XObject.
    ImageXObjectDecodedBytes,
    /// Deterministic foundational decode fuel for one Image XObject.
    ImageXObjectDecodeFuel,
    /// Conservatively accounted object, filter-plan, and decoded capacity.
    ImageXObjectRetainedBytes,
    /// Distinct outline item identities scheduled by one bounded outline job.
    OutlineItems,
    /// Greatest root-relative outline item depth accepted by one outline job.
    OutlineDepth,
    /// Items traversed in one sibling list.
    OutlineSiblings,
    /// Decoded bytes in one outline title string.
    OutlineTitleInputBytes,
    /// Logical or allocator-retained UTF-8 bytes in one outline title.
    OutlineTitleUtf8Bytes,
    /// Cumulative decoded title bytes across one outline job.
    OutlineTotalTitleInputBytes,
    /// Cumulative logical or allocator-retained UTF-8 title bytes across one outline job.
    OutlineTotalTitleUtf8Bytes,
    /// Cumulative exact object-read bytes across one outline job.
    OutlineObjectReadBytes,
    /// Cumulative object-parser window bytes across one outline job.
    OutlineObjectParseBytes,
    /// Allocator-reported outline result and traversal capacity.
    OutlineRetainedBytes,
}

/// Structured document-composition resource-limit context without document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimit {
    kind: DocumentLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl DocumentLimit {
    pub(crate) const fn new(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> Self {
        Self {
            kind,
            limit,
            consumed,
            attempted,
        }
    }

    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> DocumentLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount the rejected operation would add or require.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable document-composition failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentErrorCode {
    /// Document limit configuration is zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// A deterministic index or allocation budget was exhausted.
    ResourceLimit,
    /// The owning runtime cancelled one document-composition job.
    Cancelled,
    /// An in-use row has an offset outside the candidate revision object area.
    InvalidPhysicalOffset,
    /// Two in-use rows claim the same physical object offset.
    DuplicatePhysicalOffset,
    /// An in-use object-zero row or another impossible xref record was observed.
    InvalidXrefEntry,
    /// The trailer root is not an exact-generation in-use row in this candidate revision.
    InvalidTrailerRoot,
    /// No xref row exists for the requested object number.
    MissingObject,
    /// The requested object number is represented by a free xref row.
    FreeObject,
    /// The requested generation does not match the candidate xref row.
    GenerationMismatch,
    /// Candidate interval geometry could not form an unattested object target.
    TargetConstructionFailure,
    /// A checked candidate-index invariant could not be maintained.
    InternalState,
    /// Runtime identity or phase checkpoints for revision attestation are inconsistent.
    InvalidAttestationJobContext,
    /// Runtime identity or phase checkpoints for proof-preserving object access are inconsistent.
    InvalidObjectAccessJobContext,
    /// The source does not begin with a supported header followed by a line ending.
    InvalidDocumentHeader,
    /// A non-trivia byte occurs between top-level object frames.
    TopLevelData,
    /// A top-level comment reaches an object or xref boundary without a line ending.
    UnterminatedTopLevelComment,
    /// One candidate object could not be strictly framed and authenticated.
    ObjectAttestationFailure,
    /// Valid object syntax requires a deliberately unsupported framing capability.
    UnsupportedObjectFraming,
    /// The byte source no longer matches the attestation job's immutable snapshot.
    SourceSnapshotMismatch,
    /// The injected byte source failed while scanning top-level trivia.
    SourceFailure,
    /// An exact request inside the known immutable source unexpectedly reached EOF.
    UnexpectedEndOfSource,
    /// A completed one-shot document job was polled again.
    JobAlreadyComplete,
    /// Reopening an attested object did not reproduce its retained framing evidence.
    AttestedObjectEvidenceMismatch,
    /// Runtime identity or phase checkpoints for reference-chain resolution are inconsistent.
    InvalidReferenceChainJobContext,
    /// Following top-level indirect-reference values revisited one exact object identity.
    ReferenceCycle,
    /// Runtime identity or child checkpoints for page-tree traversal are inconsistent.
    InvalidPageTreeJobContext,
    /// The trailer root is not a strict direct Catalog with one valid Pages reference.
    InvalidCatalog,
    /// A page-tree object or one of its required structural fields has the wrong shape.
    InvalidPageTreeNode,
    /// A page-tree child points to one of its active ancestors.
    PageTreeCycle,
    /// One exact Page/Pages object is reachable from more than one Kids position.
    DuplicatePageTreeNode,
    /// A Page or non-root Pages node does not point back to its exact parent.
    PageTreeParentMismatch,
    /// A Pages Count differs from its validated direct partition or complete leaf subtree.
    PageTreeCountMismatch,
    /// A strict Catalog or page-tree dictionary repeats a structural key.
    DuplicateStructuralKey,
    /// A requested zero-based logical page index is outside the validated document range.
    PageIndexOutOfBounds,
    /// A page handle belongs to another binding or its paired index lacks the exact Page proof.
    StalePageHandle,
    /// Runtime identity or child checkpoints for inherited page-value materialization are invalid.
    InvalidPageMaterializationJobContext,
    /// MediaBox or CropBox is missing, malformed, non-finite, or not exactly representable.
    InvalidPageBox,
    /// Rotate is not an integer multiple of ninety degrees.
    InvalidPageRotation,
    /// Resources does not resolve to one direct PDF dictionary.
    InvalidPageResources,
    /// No inheritable MediaBox definition exists on the validated Page-to-root chain.
    MissingPageMediaBox,
    /// No inheritable Resources definition exists on the validated Page-to-root chain.
    MissingPageResources,
    /// A whole-object inherited-value alias revisited an active reference.
    PageValueAliasCycle,
    /// An inherited value uses a valid alias or nested representation outside this bounded profile.
    UnsupportedPageValueRepresentation,
    /// Runtime identity or child checkpoints for page-content acquisition are invalid.
    InvalidPageContentJobContext,
    /// The exact Page dictionary contains more than one Contents key.
    DuplicatePageContents,
    /// Contents or one referenced content stream has an invalid semantic shape.
    InvalidPageContents,
    /// Contents uses a valid representation outside the bounded acquisition profile.
    UnsupportedPageContentsRepresentation,
    /// A whole-object Contents alias revisited an active reference.
    PageContentAliasCycle,
    /// A supported content stream failed canonical filter planning or decoding.
    PageContentDecodeFailure,
    /// A content stream names a filter outside the bounded foundational decoder profile.
    UnsupportedPageContentFilter,
    /// `/Properties` or one requested marked-content property has an invalid semantic shape.
    InvalidPagePropertyResource,
    /// The page resource dictionary stores `/Properties` through an unsupported indirect object.
    UnsupportedIndirectPageProperties,
    /// A requested marked-content property resolves to an unsupported direct dictionary.
    UnsupportedDirectPagePropertyDictionary,
    /// `/XObject` or one requested Page XObject name has an invalid semantic shape.
    InvalidPageXObjectResource,
    /// Runtime identity or checkpoints for Image XObject acquisition are inconsistent.
    InvalidImageXObjectJobContext,
    /// An otherwise selected Image XObject has malformed required metadata or decoded geometry.
    InvalidImageXObject,
    /// A registered Image XObject stream failed canonical filter planning or decoding.
    ImageXObjectDecodeFailure,
    /// `/Font` or one requested Page Font name has an invalid semantic shape.
    InvalidPageFontResource,
    /// `/ExtGState` or one requested Page graphics-state name has an invalid semantic shape.
    InvalidPageExtGStateResource,
    /// Runtime identity or checkpoints for Font resource acquisition are inconsistent.
    InvalidFontResourceJobContext,
    /// A selected simple Font, descriptor, or embedded program has malformed metadata.
    InvalidFontResource,
    /// A registered FontFile2 stream failed canonical planning or decoding.
    FontResourceDecodeFailure,
    /// A decoded embedded TrueType program is malformed under the registered profile.
    FontProgramFailure,
    /// Runtime identity or child checkpoints for outline traversal are inconsistent.
    InvalidOutlineJobContext,
    /// The Catalog outline entry or outline root dictionary has the wrong shape.
    InvalidOutlineDictionary,
    /// An outline item or one of its required structural fields has the wrong shape.
    InvalidOutlineItem,
    /// An outline semantic field uses an indirect form outside this bootstrap profile.
    UnsupportedOutlineRepresentation,
    /// An outline title is not a valid supported PDF text string.
    InvalidOutlineTitle,
    /// An outline item does not point back to its exact traversed parent.
    OutlineParentMismatch,
    /// Prev, Next, First, or Last does not describe one closed sibling list.
    OutlineSiblingMismatch,
    /// An outline structural edge points to an active or prior item in the same chain.
    OutlineCycle,
    /// One exact outline item is reachable from more than one structural position.
    DuplicateOutlineItem,
    /// An item or root Count differs from the recursively validated visibility formula.
    OutlineCountMismatch,
    /// An outline item has mutually exclusive or malformed activation targets.
    InvalidOutlineTarget,
    /// Runtime identity or checkpoints across xref and attestation phases are inconsistent.
    InvalidStrictBaseOpenContext,
    /// Runtime identity or checkpoints across local-repair open phases are inconsistent.
    InvalidLocalRepairOpenContext,
    /// Runtime identity or child checkpoints for revision-aware resolution are inconsistent.
    InvalidRevisionResolverJobContext,
    /// The effective xref-stream definition is null and hides every older definition.
    NullObject,
    /// The effective object is compressed and requires decoded object-stream coordinates.
    UnsupportedCompressedObject,
    /// A primary xref-stream container cannot use the ordinary pre-xref physical target model.
    UnsupportedXrefStreamContainer,
    /// The effective uncompressed object failed exact header or framing validation.
    ObjectResolutionFailure,
    /// A stream declares its own object identity as the indirect length dependency.
    IndirectLengthCycle,
    /// An indirect stream length did not resolve to one uncompressed nonnegative integer object.
    InvalidIndirectLength,
    /// Decoded object-stream evidence does not match its effective uncompressed container.
    InvalidObjectStreamContainer,
    /// A compressed xref row does not match the indexed decoded object-stream entry.
    CompressedObjectMismatch,
    /// The effective object definition is not a compressed xref row.
    NotCompressedObject,
}

/// Coarse document-composition failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentErrorCategory {
    /// Invalid deterministic configuration.
    Configuration,
    /// Malformed or inconsistent xref-derived candidate metadata.
    Syntax,
    /// A requested object identity is absent, free, or has another generation.
    Lookup,
    /// Deterministic work or allocation exhaustion.
    Resource,
    /// Immutable byte-source failure or snapshot-integrity change.
    Source,
    /// Valid syntax requiring a deliberately unsupported capability.
    Unsupported,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal checked invariant failure.
    Internal,
}

/// Stable recovery policy for a document-composition failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentRecoverability {
    /// Correct the deterministic limit profile before retrying.
    CorrectConfiguration,
    /// Correct the PDF bytes or select an explicitly approved recovery policy.
    CorrectInput,
    /// Supply a reference that exists with the indexed generation and is in use.
    CorrectReference,
    /// Reduce work or select an approved larger deterministic budget.
    ReduceWorkload,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Retry the host source operation while preserving snapshot identity.
    RetrySource,
    /// Select an implementation profile supporting the required feature.
    UseSupportedFeature,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DocumentErrorDetail {
    None,
    Limit(DocumentLimit),
    Object {
        error: ObjectError,
        aggregate_limit: Option<DocumentLimit>,
    },
    Text {
        error: TextStringError,
        aggregate_limit: Option<DocumentLimit>,
    },
    Source(SourceError),
    Syntax(SyntaxError),
}

/// Source-redacted document-composition error with stable policy metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DocumentError {
    code: DocumentErrorCode,
    category: DocumentErrorCategory,
    recoverability: DocumentRecoverability,
    diagnostic_id: &'static str,
    reference: Option<ObjectRef>,
    offset: Option<u64>,
    detail: DocumentErrorDetail,
}

impl DocumentError {
    pub(crate) const fn for_code(
        code: DocumentErrorCode,
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            DocumentErrorCode::InvalidLimits => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0001",
            ),
            DocumentErrorCode::ResourceLimit => (
                DocumentErrorCategory::Resource,
                DocumentRecoverability::ReduceWorkload,
                "RPE-DOCUMENT-0002",
            ),
            DocumentErrorCode::Cancelled => (
                DocumentErrorCategory::Cancellation,
                DocumentRecoverability::AbandonOperation,
                "RPE-DOCUMENT-0003",
            ),
            DocumentErrorCode::InvalidPhysicalOffset => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0004",
            ),
            DocumentErrorCode::DuplicatePhysicalOffset => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0005",
            ),
            DocumentErrorCode::InvalidXrefEntry => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0006",
            ),
            DocumentErrorCode::InvalidTrailerRoot => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0007",
            ),
            DocumentErrorCode::MissingObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0008",
            ),
            DocumentErrorCode::FreeObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0009",
            ),
            DocumentErrorCode::GenerationMismatch => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0010",
            ),
            DocumentErrorCode::TargetConstructionFailure => (
                DocumentErrorCategory::Internal,
                DocumentRecoverability::DoNotRetry,
                "RPE-DOCUMENT-0011",
            ),
            DocumentErrorCode::InternalState => (
                DocumentErrorCategory::Internal,
                DocumentRecoverability::DoNotRetry,
                "RPE-DOCUMENT-0012",
            ),
            DocumentErrorCode::InvalidAttestationJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0013",
            ),
            DocumentErrorCode::InvalidDocumentHeader => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0014",
            ),
            DocumentErrorCode::TopLevelData => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0015",
            ),
            DocumentErrorCode::UnterminatedTopLevelComment => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0016",
            ),
            DocumentErrorCode::ObjectAttestationFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0017",
            ),
            DocumentErrorCode::UnsupportedObjectFraming => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0018",
            ),
            DocumentErrorCode::SourceSnapshotMismatch => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::ReopenSource,
                "RPE-DOCUMENT-0019",
            ),
            DocumentErrorCode::SourceFailure => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::RetrySource,
                "RPE-DOCUMENT-0020",
            ),
            DocumentErrorCode::UnexpectedEndOfSource => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::ReopenSource,
                "RPE-DOCUMENT-0021",
            ),
            DocumentErrorCode::JobAlreadyComplete => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0022",
            ),
            DocumentErrorCode::InvalidObjectAccessJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0023",
            ),
            DocumentErrorCode::AttestedObjectEvidenceMismatch => (
                DocumentErrorCategory::Internal,
                DocumentRecoverability::DoNotRetry,
                "RPE-DOCUMENT-0024",
            ),
            DocumentErrorCode::InvalidReferenceChainJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0025",
            ),
            DocumentErrorCode::ReferenceCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0026",
            ),
            DocumentErrorCode::InvalidPageTreeJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0027",
            ),
            DocumentErrorCode::InvalidCatalog => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0028",
            ),
            DocumentErrorCode::InvalidPageTreeNode => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0029",
            ),
            DocumentErrorCode::PageTreeCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0030",
            ),
            DocumentErrorCode::DuplicatePageTreeNode => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0031",
            ),
            DocumentErrorCode::PageTreeParentMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0032",
            ),
            DocumentErrorCode::PageTreeCountMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0033",
            ),
            DocumentErrorCode::DuplicateStructuralKey => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0034",
            ),
            DocumentErrorCode::PageIndexOutOfBounds => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0058",
            ),
            DocumentErrorCode::StalePageHandle => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0059",
            ),
            DocumentErrorCode::InvalidPageMaterializationJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0060",
            ),
            DocumentErrorCode::InvalidPageBox => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0061",
            ),
            DocumentErrorCode::InvalidPageRotation => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0062",
            ),
            DocumentErrorCode::InvalidPageResources => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0063",
            ),
            DocumentErrorCode::MissingPageMediaBox => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0064",
            ),
            DocumentErrorCode::MissingPageResources => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0065",
            ),
            DocumentErrorCode::PageValueAliasCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0066",
            ),
            DocumentErrorCode::UnsupportedPageValueRepresentation => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0067",
            ),
            DocumentErrorCode::InvalidPageContentJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0068",
            ),
            DocumentErrorCode::DuplicatePageContents => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0069",
            ),
            DocumentErrorCode::InvalidPageContents => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0070",
            ),
            DocumentErrorCode::UnsupportedPageContentsRepresentation => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0071",
            ),
            DocumentErrorCode::PageContentAliasCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0072",
            ),
            DocumentErrorCode::PageContentDecodeFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0073",
            ),
            DocumentErrorCode::UnsupportedPageContentFilter => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0074",
            ),
            DocumentErrorCode::InvalidPagePropertyResource => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0075",
            ),
            DocumentErrorCode::UnsupportedIndirectPageProperties => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0076",
            ),
            DocumentErrorCode::UnsupportedDirectPagePropertyDictionary => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0077",
            ),
            DocumentErrorCode::InvalidPageXObjectResource => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0078",
            ),
            DocumentErrorCode::InvalidImageXObjectJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0079",
            ),
            DocumentErrorCode::InvalidImageXObject => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0080",
            ),
            DocumentErrorCode::ImageXObjectDecodeFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0081",
            ),
            DocumentErrorCode::InvalidPageFontResource => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0082",
            ),
            DocumentErrorCode::InvalidPageExtGStateResource => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0087",
            ),
            DocumentErrorCode::InvalidFontResourceJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0083",
            ),
            DocumentErrorCode::InvalidFontResource => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0084",
            ),
            DocumentErrorCode::FontResourceDecodeFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0085",
            ),
            DocumentErrorCode::FontProgramFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0086",
            ),
            DocumentErrorCode::InvalidOutlineJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0035",
            ),
            DocumentErrorCode::InvalidOutlineDictionary => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0036",
            ),
            DocumentErrorCode::InvalidOutlineItem => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0037",
            ),
            DocumentErrorCode::UnsupportedOutlineRepresentation => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0038",
            ),
            DocumentErrorCode::InvalidOutlineTitle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0039",
            ),
            DocumentErrorCode::OutlineParentMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0040",
            ),
            DocumentErrorCode::OutlineSiblingMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0041",
            ),
            DocumentErrorCode::OutlineCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0042",
            ),
            DocumentErrorCode::DuplicateOutlineItem => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0043",
            ),
            DocumentErrorCode::OutlineCountMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0044",
            ),
            DocumentErrorCode::InvalidOutlineTarget => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0045",
            ),
            DocumentErrorCode::InvalidStrictBaseOpenContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0046",
            ),
            DocumentErrorCode::InvalidLocalRepairOpenContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0057",
            ),
            DocumentErrorCode::InvalidRevisionResolverJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0047",
            ),
            DocumentErrorCode::NullObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0048",
            ),
            DocumentErrorCode::UnsupportedCompressedObject => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0049",
            ),
            DocumentErrorCode::UnsupportedXrefStreamContainer => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0050",
            ),
            DocumentErrorCode::ObjectResolutionFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0051",
            ),
            DocumentErrorCode::IndirectLengthCycle => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0052",
            ),
            DocumentErrorCode::InvalidIndirectLength => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0053",
            ),
            DocumentErrorCode::InvalidObjectStreamContainer => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0054",
            ),
            DocumentErrorCode::CompressedObjectMismatch => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0055",
            ),
            DocumentErrorCode::NotCompressedObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0056",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            reference,
            offset,
            detail: DocumentErrorDetail::None,
        }
    }

    pub(crate) const fn resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: None,
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn reference_chain_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn page_tree_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn page_materialization_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn page_content_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn page_property_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn image_xobject_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn font_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn outline_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: ObjectRef,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: Some(reference),
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn from_outline_text(
        error: TextStringError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        let code = match error.code() {
            TextStringErrorCode::InvalidLimits => DocumentErrorCode::InvalidLimits,
            TextStringErrorCode::ResourceLimit => DocumentErrorCode::ResourceLimit,
            TextStringErrorCode::Cancelled => DocumentErrorCode::Cancelled,
            TextStringErrorCode::UndefinedPdfDocEncoding | TextStringErrorCode::InvalidUtf16 => {
                DocumentErrorCode::InvalidOutlineTitle
            }
        };
        let category = match error.category() {
            TextStringErrorCategory::Configuration => DocumentErrorCategory::Configuration,
            TextStringErrorCategory::Resource => DocumentErrorCategory::Resource,
            TextStringErrorCategory::Syntax => DocumentErrorCategory::Syntax,
            TextStringErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
        };
        let recoverability = match error.recoverability() {
            TextStringRecoverability::CorrectConfiguration => {
                DocumentRecoverability::CorrectConfiguration
            }
            TextStringRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            TextStringRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
            TextStringRecoverability::AbandonOperation => DocumentRecoverability::AbandonOperation,
        };
        let aggregate_limit = match error.limit() {
            Some(limit) => Some(DocumentLimit::new(
                match limit.kind() {
                    TextStringLimitKind::InputBytes => DocumentLimitKind::OutlineTitleInputBytes,
                    TextStringLimitKind::Utf8Bytes => DocumentLimitKind::OutlineTitleUtf8Bytes,
                },
                limit.limit(),
                limit.consumed(),
                limit.attempted(),
            )),
            None => None,
        };
        let base = Self::for_code(code, Some(reference), Some(offset));
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Text {
                error,
                aggregate_limit,
            },
            ..base
        }
    }

    pub(crate) const fn from_object(error: ObjectError, reference: ObjectRef, offset: u64) -> Self {
        let offset = match error.offset() {
            Some(lower_offset) => lower_offset,
            None => offset,
        };
        Self {
            code: DocumentErrorCode::TargetConstructionFailure,
            category: DocumentErrorCategory::Internal,
            recoverability: DocumentRecoverability::DoNotRetry,
            diagnostic_id: "RPE-DOCUMENT-0011",
            reference: Some(reference),
            offset: Some(offset),
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: None,
            },
        }
    }

    pub(crate) const fn from_attestation_object(
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        let code = match error.category() {
            ObjectErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            _ => match error.code() {
                ObjectErrorCode::Cancelled => DocumentErrorCode::Cancelled,
                ObjectErrorCode::UnsupportedIndirectLength => {
                    DocumentErrorCode::UnsupportedObjectFraming
                }
                ObjectErrorCode::SnapshotMismatch => DocumentErrorCode::SourceSnapshotMismatch,
                ObjectErrorCode::SourceFailure => DocumentErrorCode::SourceFailure,
                ObjectErrorCode::UnexpectedEndOfSource => DocumentErrorCode::UnexpectedEndOfSource,
                ObjectErrorCode::InvalidTarget
                | ObjectErrorCode::InternalState
                | ObjectErrorCode::JobAlreadyComplete => DocumentErrorCode::InternalState,
                _ => DocumentErrorCode::ObjectAttestationFailure,
            },
        };
        let category = match error.category() {
            ObjectErrorCategory::Configuration => DocumentErrorCategory::Configuration,
            ObjectErrorCategory::Source => DocumentErrorCategory::Source,
            ObjectErrorCategory::Syntax => DocumentErrorCategory::Syntax,
            ObjectErrorCategory::Unsupported => DocumentErrorCategory::Unsupported,
            ObjectErrorCategory::Resource => DocumentErrorCategory::Resource,
            ObjectErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
            ObjectErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            ObjectRecoverability::CorrectConfiguration => {
                DocumentRecoverability::CorrectConfiguration
            }
            ObjectRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
            ObjectRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            ObjectRecoverability::RetrySource => DocumentRecoverability::RetrySource,
            ObjectRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            ObjectRecoverability::UseSupportedFeature => {
                DocumentRecoverability::UseSupportedFeature
            }
            ObjectRecoverability::AbandonOperation => DocumentRecoverability::AbandonOperation,
            ObjectRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(
            code,
            match error.reference() {
                Some(lower_reference) => Some(lower_reference),
                None => Some(reference),
            },
            match error.offset() {
                Some(lower_offset) => Some(lower_offset),
                None => Some(offset),
            },
        );
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: None,
            },
            ..base
        }
    }

    pub(crate) const fn from_revision_resolver_object(
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
        constructor: bool,
    ) -> Self {
        let code = match error.source_error() {
            Some(source) => match source.category() {
                SourceErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
                SourceErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
                SourceErrorCategory::Input
                | SourceErrorCategory::Lifecycle
                | SourceErrorCategory::Availability
                | SourceErrorCategory::Internal => DocumentErrorCode::SourceFailure,
            },
            None => match error.category() {
                ObjectErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
                ObjectErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
                ObjectErrorCategory::Source => match error.code() {
                    ObjectErrorCode::SnapshotMismatch => DocumentErrorCode::SourceSnapshotMismatch,
                    ObjectErrorCode::UnexpectedEndOfSource => {
                        DocumentErrorCode::UnexpectedEndOfSource
                    }
                    _ => DocumentErrorCode::SourceFailure,
                },
                ObjectErrorCategory::Configuration => match error.code() {
                    ObjectErrorCode::InvalidLimits => DocumentErrorCode::InvalidLimits,
                    ObjectErrorCode::InvalidJobContext => {
                        DocumentErrorCode::InvalidRevisionResolverJobContext
                    }
                    _ => DocumentErrorCode::InternalState,
                },
                ObjectErrorCategory::Syntax => match error.code() {
                    ObjectErrorCode::InvalidStreamLength
                    | ObjectErrorCode::InvalidStreamLengthClaim => {
                        DocumentErrorCode::InvalidIndirectLength
                    }
                    _ => DocumentErrorCode::ObjectResolutionFailure,
                },
                ObjectErrorCategory::Unsupported => DocumentErrorCode::UnsupportedObjectFraming,
                ObjectErrorCategory::Internal => DocumentErrorCode::InternalState,
            },
        };
        let code = match (constructor, error.code()) {
            (true, ObjectErrorCode::InvalidTarget) => DocumentErrorCode::TargetConstructionFailure,
            _ => code,
        };
        Self::with_object_error(code, error, reference, offset, true)
    }

    pub(crate) const fn from_object_access_constructor(
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        let code = match error.code() {
            ObjectErrorCode::InvalidLimits => DocumentErrorCode::InvalidLimits,
            ObjectErrorCode::InvalidJobContext => DocumentErrorCode::InvalidObjectAccessJobContext,
            _ => DocumentErrorCode::AttestedObjectEvidenceMismatch,
        };
        Self::with_object_error(code, error, reference, offset, false)
    }

    pub(crate) const fn from_object_access_poll(
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        let (code, preserve_lower_policy) = match error.source_error() {
            Some(source) => (
                match source.category() {
                    SourceErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
                    SourceErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
                    SourceErrorCategory::Input
                    | SourceErrorCategory::Lifecycle
                    | SourceErrorCategory::Availability
                    | SourceErrorCategory::Internal => DocumentErrorCode::SourceFailure,
                },
                true,
            ),
            None => match error.category() {
                ObjectErrorCategory::Resource => (DocumentErrorCode::ResourceLimit, true),
                ObjectErrorCategory::Cancellation => (DocumentErrorCode::Cancelled, true),
                ObjectErrorCategory::Source => (
                    match error.code() {
                        ObjectErrorCode::SnapshotMismatch => {
                            DocumentErrorCode::SourceSnapshotMismatch
                        }
                        ObjectErrorCode::UnexpectedEndOfSource => {
                            DocumentErrorCode::UnexpectedEndOfSource
                        }
                        _ => DocumentErrorCode::SourceFailure,
                    },
                    true,
                ),
                ObjectErrorCategory::Configuration
                | ObjectErrorCategory::Syntax
                | ObjectErrorCategory::Unsupported
                | ObjectErrorCategory::Internal => {
                    (DocumentErrorCode::AttestedObjectEvidenceMismatch, false)
                }
            },
        };
        Self::with_object_error(code, error, reference, offset, preserve_lower_policy)
    }

    const fn with_object_error(
        code: DocumentErrorCode,
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
        preserve_lower_policy: bool,
    ) -> Self {
        let base = Self::for_code(
            code,
            match error.reference() {
                Some(lower_reference) => Some(lower_reference),
                None => Some(reference),
            },
            match error.offset() {
                Some(lower_offset) => Some(lower_offset),
                None => Some(offset),
            },
        );
        let (category, recoverability) = if preserve_lower_policy {
            (
                match error.category() {
                    ObjectErrorCategory::Configuration => DocumentErrorCategory::Configuration,
                    ObjectErrorCategory::Source => DocumentErrorCategory::Source,
                    ObjectErrorCategory::Syntax => DocumentErrorCategory::Syntax,
                    ObjectErrorCategory::Unsupported => DocumentErrorCategory::Unsupported,
                    ObjectErrorCategory::Resource => DocumentErrorCategory::Resource,
                    ObjectErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
                    ObjectErrorCategory::Internal => DocumentErrorCategory::Internal,
                },
                match error.recoverability() {
                    ObjectRecoverability::CorrectConfiguration => {
                        DocumentRecoverability::CorrectConfiguration
                    }
                    ObjectRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
                    ObjectRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
                    ObjectRecoverability::RetrySource => DocumentRecoverability::RetrySource,
                    ObjectRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
                    ObjectRecoverability::UseSupportedFeature => {
                        DocumentRecoverability::UseSupportedFeature
                    }
                    ObjectRecoverability::AbandonOperation => {
                        DocumentRecoverability::AbandonOperation
                    }
                    ObjectRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
                },
            )
        } else {
            (base.category, base.recoverability)
        };
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: None,
            },
            ..base
        }
    }

    pub(crate) const fn aggregate_object_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: match error.reference() {
                Some(lower_reference) => Some(lower_reference),
                None => Some(reference),
            },
            offset: match error.offset() {
                Some(lower_offset) => Some(lower_offset),
                None => Some(offset),
            },
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: Some(DocumentLimit::new(kind, limit, consumed, attempted)),
            },
        }
    }

    pub(crate) const fn from_source(error: SourceError, offset: u64) -> Self {
        let code = match error.category() {
            SourceErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
            SourceErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            _ => DocumentErrorCode::SourceFailure,
        };
        let category = match error.category() {
            SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
                DocumentErrorCategory::Configuration
            }
            SourceErrorCategory::Integrity | SourceErrorCategory::Availability => {
                DocumentErrorCategory::Source
            }
            SourceErrorCategory::Resource => DocumentErrorCategory::Resource,
            SourceErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SourceRecoverability::CorrectInput => DocumentRecoverability::CorrectConfiguration,
            SourceRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            SourceRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            SourceRecoverability::RetrySource => DocumentRecoverability::RetrySource,
            SourceRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(code, None, Some(offset));
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Source(error),
            ..base
        }
    }

    pub(crate) const fn from_header_syntax(error: SyntaxError) -> Self {
        let code = match error.category() {
            SyntaxErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            SyntaxErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
            SyntaxErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
            SyntaxErrorCategory::Internal => DocumentErrorCode::InternalState,
            SyntaxErrorCategory::Configuration | SyntaxErrorCategory::Syntax => {
                DocumentErrorCode::InvalidDocumentHeader
            }
        };
        let category = match error.category() {
            SyntaxErrorCategory::Configuration => DocumentErrorCategory::Configuration,
            SyntaxErrorCategory::Syntax => DocumentErrorCategory::Syntax,
            SyntaxErrorCategory::Resource => DocumentErrorCategory::Resource,
            SyntaxErrorCategory::Integrity => DocumentErrorCategory::Source,
            SyntaxErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
            SyntaxErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SyntaxRecoverability::CorrectConfiguration => {
                DocumentRecoverability::CorrectConfiguration
            }
            SyntaxRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
            SyntaxRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            SyntaxRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            SyntaxRecoverability::AbandonOperation => DocumentRecoverability::AbandonOperation,
            SyntaxRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(code, None, error.offset());
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Syntax(error),
            ..base
        }
    }

    /// Returns the machine-readable document-composition failure code.
    pub const fn code(self) -> DocumentErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> DocumentErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> DocumentRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the involved object reference, when one exists.
    pub const fn reference(self) -> Option<ObjectRef> {
        self.reference
    }

    /// Returns the involved absolute source offset, when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(self) -> Option<DocumentLimit> {
        match self.detail {
            DocumentErrorDetail::Limit(limit) => Some(limit),
            DocumentErrorDetail::Object {
                aggregate_limit, ..
            } => aggregate_limit,
            DocumentErrorDetail::Text {
                aggregate_limit, ..
            } => aggregate_limit,
            DocumentErrorDetail::None
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the code of the complete retained lower object error, when applicable.
    pub const fn object_error_code(self) -> Option<ObjectErrorCode> {
        match self.detail {
            DocumentErrorDetail::Object { error, .. } => Some(error.code()),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Text { .. }
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the complete retained lower object error, when applicable.
    pub const fn object_error(self) -> Option<ObjectError> {
        match self.detail {
            DocumentErrorDetail::Object { error, .. } => Some(error),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Text { .. }
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower byte-source error, directly or through an object job.
    pub const fn source_error(self) -> Option<SourceError> {
        match self.detail {
            DocumentErrorDetail::Source(error) => Some(error),
            DocumentErrorDetail::Object { error, .. } => error.source_error(),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Text { .. }
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower syntax error, directly or through an object job.
    pub const fn syntax_error(self) -> Option<SyntaxError> {
        match self.detail {
            DocumentErrorDetail::Syntax(error) => Some(error),
            DocumentErrorDetail::Object { error, .. } => error.syntax_error(),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Text { .. }
            | DocumentErrorDetail::Source(_) => None,
        }
    }

    /// Returns the retained lower PDF text-string error for an outline title.
    pub const fn text_string_error(self) -> Option<TextStringError> {
        match self.detail {
            DocumentErrorDetail::Text { error, .. } => Some(error),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Object { .. }
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }
}

impl fmt::Debug for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("reference", &self.reference)
            .field("offset", &self.offset)
            .field("detail", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(reference) = self.reference {
            write!(
                formatter,
                " for object {} {}",
                reference.number(),
                reference.generation()
            )?;
        }
        if let Some(offset) = self.offset {
            write!(formatter, " at byte {offset}")?;
        }
        if let Some(limit) = self.limit() {
            write!(
                formatter,
                " limit_kind={:?} limit={} consumed={} attempted={}",
                limit.kind, limit.limit, limit.consumed, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for DocumentError {}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{
        SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
        SourceValidatorKind,
    };
    use pdf_rs_object::IndirectObjectTarget;

    use super::*;

    #[test]
    fn lower_object_offset_survives_target_error_conversion() {
        let reference = ObjectRef::new(1, 0).unwrap();
        let snapshot = SourceSnapshot::new(
            SourceIdentity::new(SourceStableId::new([0x37; 32]), SourceRevision::new(1)),
            Some(20),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x91; 32]),
        );
        let lower = IndirectObjectTarget::new(snapshot, reference, 1, 12, 10).unwrap_err();
        assert_eq!(lower.offset(), Some(12));

        let error = DocumentError::from_object(lower, reference, 1);
        assert_eq!(error.code(), DocumentErrorCode::TargetConstructionFailure);
        assert_eq!(error.offset(), Some(12));
        assert_eq!(error.object_error_code(), Some(lower.code()));
        assert!(error.to_string().contains("at byte 12"));
        assert!(!format!("{error:?}").contains("FrozenResponse"));
    }
}
