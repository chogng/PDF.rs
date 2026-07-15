use std::fmt;
use std::mem;

use pdf_rs_bytes::{SourceIdentity, SourceSnapshot};
use pdf_rs_syntax::ObjectRef;

use crate::{
    XrefCancellation, XrefEntry, XrefEntryKind, XrefRecoverability, XrefStreamEntry,
    XrefStreamEntryKind,
};

const HARD_MAX_REVISIONS: u32 = 1024;
const HARD_MAX_SECTIONS: u32 = 2048;
const HARD_MAX_ENTRIES: u64 = 4_000_000;
const HARD_MAX_RETAINED_BYTES: u64 = 512 * 1024 * 1024;
const CANCELLATION_INTERVAL: usize = 256;

/// Unvalidated limits for one already-parsed revision-chain composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionLimitConfig {
    /// Maximum primary revisions, including the base revision.
    pub max_revisions: u32,
    /// Maximum primary sections plus hybrid supplements.
    pub max_sections: u32,
    /// Maximum primary plus hybrid-supplement entries inspected and retained.
    pub max_entries: u64,
    /// Maximum allocator-reported bytes retained by revision and entry vectors.
    pub max_retained_bytes: u64,
}

impl Default for RevisionLimitConfig {
    fn default() -> Self {
        Self {
            max_revisions: 128,
            max_sections: 256,
            max_entries: 500_000,
            max_retained_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Validated limits for one already-parsed revision-chain composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionLimits {
    max_revisions: u32,
    max_sections: u32,
    max_entries: u64,
    max_retained_bytes: u64,
}

impl RevisionLimits {
    /// Validates one complete revision-chain budget profile.
    pub fn validate(config: RevisionLimitConfig) -> Result<Self, RevisionError> {
        if config.max_revisions == 0
            || config.max_revisions > HARD_MAX_REVISIONS
            || config.max_sections == 0
            || config.max_sections > HARD_MAX_SECTIONS
            || config.max_sections < config.max_revisions
            || config.max_entries == 0
            || config.max_entries > HARD_MAX_ENTRIES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(RevisionError::for_code(
                RevisionErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_revisions: config.max_revisions,
            max_sections: config.max_sections,
            max_entries: config.max_entries,
            max_retained_bytes: config.max_retained_bytes,
        })
    }

    /// Returns the primary revision ceiling.
    pub const fn max_revisions(self) -> u32 {
        self.max_revisions
    }

    /// Returns the primary plus hybrid section ceiling.
    pub const fn max_sections(self) -> u32 {
        self.max_sections
    }

    /// Returns the primary plus supplement entry ceiling.
    pub const fn max_entries(self) -> u64 {
        self.max_entries
    }

    /// Returns the retained vector-capacity byte ceiling.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }
}

impl Default for RevisionLimits {
    fn default() -> Self {
        Self::validate(RevisionLimitConfig::default())
            .expect("built-in revision limits satisfy hard ceilings")
    }
}

/// Revision-composition budget dimension that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionLimitKind {
    /// Primary revisions.
    Revisions,
    /// Primary sections plus hybrid supplements.
    Sections,
    /// Primary plus hybrid-supplement entries.
    Entries,
    /// Allocator-reported retained vector capacity.
    RetainedBytes,
}

/// Stable machine-readable revision-chain failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionErrorCode {
    /// The caller supplied an invalid limit profile.
    InvalidLimits,
    /// The caller supplied no primary revision.
    EmptyChain,
    /// Source snapshots or physical offsets are inconsistent.
    SourceMismatch,
    /// `/Prev` does not name the next older primary revision exactly.
    InvalidPrevious,
    /// `/Size`, root, entry ordering, or entry geometry is invalid.
    InvalidRevision,
    /// A hybrid supplement is misplaced or conflicts with its primary table.
    InvalidHybrid,
    /// The newest trailer root is missing, free, or has the wrong generation.
    InvalidRoot,
    /// A deterministic entry or retained-memory budget was exceeded.
    ResourceLimit,
    /// The owning runtime cancelled composition.
    Cancelled,
    /// A checked implementation invariant could not be maintained.
    InternalState,
}

/// Coarse revision-chain failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Immutable source identity or geometry mismatch.
    Source,
    /// Malformed revision metadata or entries.
    Syntax,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation failure.
    Internal,
}

/// Source-redacted revision-chain failure with stable policy metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RevisionError {
    code: RevisionErrorCode,
    category: RevisionErrorCategory,
    recoverability: XrefRecoverability,
    diagnostic_id: &'static str,
    startxref: Option<u64>,
    object_number: Option<u32>,
    limit: Option<(RevisionLimitKind, u64, u64)>,
}

impl RevisionError {
    fn for_code(code: RevisionErrorCode, startxref: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = revision_policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            startxref,
            object_number: None,
            limit: None,
        }
    }

    fn for_object(code: RevisionErrorCode, startxref: u64, object_number: u32) -> Self {
        let mut error = Self::for_code(code, Some(startxref));
        error.object_number = Some(object_number);
        error
    }

    fn resource(kind: RevisionLimitKind, limit: u64, attempted: u64) -> Self {
        let mut error = Self::for_code(RevisionErrorCode::ResourceLimit, None);
        error.limit = Some((kind, limit, attempted));
        error
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> RevisionErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> RevisionErrorCategory {
        self.category
    }

    /// Returns the stable approved recovery policy.
    pub const fn recoverability(self) -> XrefRecoverability {
        self.recoverability
    }

    /// Returns the stable redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the relevant primary or supplement offset.
    pub const fn startxref(self) -> Option<u64> {
        self.startxref
    }

    /// Returns the relevant object number without object bytes.
    pub const fn object_number(self) -> Option<u32> {
        self.object_number
    }

    /// Returns resource-limit context as kind, limit, and attempted amount.
    pub const fn limit(self) -> Option<(RevisionLimitKind, u64, u64)> {
        self.limit
    }
}

impl fmt::Debug for RevisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevisionError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("startxref", &self.startxref)
            .field("object_number", &self.object_number)
            .field("limit", &self.limit)
            .finish()
    }
}

impl fmt::Display for RevisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {:?}", self.diagnostic_id, self.code)
    }
}

impl std::error::Error for RevisionError {}

const fn revision_policy(
    code: RevisionErrorCode,
) -> (RevisionErrorCategory, XrefRecoverability, &'static str) {
    match code {
        RevisionErrorCode::InvalidLimits => (
            RevisionErrorCategory::Configuration,
            XrefRecoverability::CorrectConfiguration,
            "RPE-XREF-0201",
        ),
        RevisionErrorCode::EmptyChain => (
            RevisionErrorCategory::Configuration,
            XrefRecoverability::CorrectConfiguration,
            "RPE-XREF-0202",
        ),
        RevisionErrorCode::SourceMismatch => (
            RevisionErrorCategory::Source,
            XrefRecoverability::ReopenSource,
            "RPE-XREF-0203",
        ),
        RevisionErrorCode::InvalidPrevious => (
            RevisionErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0204",
        ),
        RevisionErrorCode::InvalidRevision => (
            RevisionErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0205",
        ),
        RevisionErrorCode::InvalidHybrid => (
            RevisionErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0206",
        ),
        RevisionErrorCode::InvalidRoot => (
            RevisionErrorCategory::Syntax,
            XrefRecoverability::CorrectInput,
            "RPE-XREF-0207",
        ),
        RevisionErrorCode::ResourceLimit => (
            RevisionErrorCategory::Resource,
            XrefRecoverability::ReduceWorkload,
            "RPE-XREF-0208",
        ),
        RevisionErrorCode::Cancelled => (
            RevisionErrorCategory::Cancellation,
            XrefRecoverability::AbandonOperation,
            "RPE-XREF-0209",
        ),
        RevisionErrorCode::InternalState => (
            RevisionErrorCategory::Internal,
            XrefRecoverability::DoNotRetry,
            "RPE-XREF-0210",
        ),
    }
}

/// Semantic payload of one parsed revision entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionEntryKind {
    /// An unknown xref-stream entry type interpreted as null and hiding older definitions.
    Null {
        /// Encoded future entry type.
        encoded_type: u64,
    },
    /// A free entry that hides every older definition of the object number.
    Free {
        /// Next object number in the free chain.
        next_free: u32,
        /// Generation number of the free entry.
        generation: u16,
    },
    /// An uncompressed indirect object at a physical source offset.
    Uncompressed {
        /// Absolute offset of the indirect object header.
        offset: u64,
        /// Generation number of the indirect object.
        generation: u16,
    },
    /// A generation-zero object inside an object stream.
    Compressed {
        /// Object number of the containing object stream.
        object_stream: u32,
        /// Zero-based index inside the object stream.
        index: u32,
    },
}

/// One parsed entry supplied to revision-chain composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionEntry {
    object_number: u32,
    kind: RevisionEntryKind,
}

impl RevisionEntry {
    /// Creates a parsed null entry for later chain validation.
    pub const fn null(object_number: u32, encoded_type: u64) -> Self {
        Self {
            object_number,
            kind: RevisionEntryKind::Null { encoded_type },
        }
    }

    /// Creates a parsed free entry for later chain validation.
    pub const fn free(object_number: u32, next_free: u32, generation: u16) -> Self {
        Self {
            object_number,
            kind: RevisionEntryKind::Free {
                next_free,
                generation,
            },
        }
    }

    /// Creates a parsed uncompressed entry for later chain validation.
    pub const fn uncompressed(object_number: u32, offset: u64, generation: u16) -> Self {
        Self {
            object_number,
            kind: RevisionEntryKind::Uncompressed { offset, generation },
        }
    }

    /// Creates a parsed compressed entry for later chain validation.
    pub const fn compressed(object_number: u32, object_stream: u32, index: u32) -> Self {
        Self {
            object_number,
            kind: RevisionEntryKind::Compressed {
                object_stream,
                index,
            },
        }
    }

    /// Returns the indexed object number.
    pub const fn object_number(self) -> u32 {
        self.object_number
    }

    /// Returns the parsed entry payload.
    pub const fn kind(self) -> RevisionEntryKind {
        self.kind
    }
}

impl From<XrefEntry> for RevisionEntry {
    fn from(entry: XrefEntry) -> Self {
        let kind = match entry.kind() {
            XrefEntryKind::Free { next_free } => RevisionEntryKind::Free {
                next_free,
                generation: entry.generation(),
            },
            XrefEntryKind::InUse { offset } => RevisionEntryKind::Uncompressed {
                offset,
                generation: entry.generation(),
            },
        };
        Self {
            object_number: entry.object_number(),
            kind,
        }
    }
}

impl From<XrefStreamEntry> for RevisionEntry {
    fn from(entry: XrefStreamEntry) -> Self {
        let kind = match entry.kind() {
            XrefStreamEntryKind::Null { encoded_type } => RevisionEntryKind::Null { encoded_type },
            XrefStreamEntryKind::Free {
                next_free,
                generation,
            } => RevisionEntryKind::Free {
                next_free,
                generation,
            },
            XrefStreamEntryKind::Uncompressed { offset, generation } => {
                RevisionEntryKind::Uncompressed { offset, generation }
            }
            XrefStreamEntryKind::Compressed {
                object_stream,
                index,
            } => RevisionEntryKind::Compressed {
                object_stream,
                index,
            },
        };
        Self {
            object_number: entry.object_number(),
            kind,
        }
    }
}

/// Kind of the primary cross-reference section for one revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionPrimaryKind {
    /// Traditional textual xref table.
    Traditional,
    /// Cross-reference stream reached directly from `startxref`.
    Stream,
}

/// Parsed hybrid xref-stream supplement attached to a traditional update table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HybridSupplement {
    snapshot: SourceSnapshot,
    startxref: u64,
    container: ObjectRef,
    declared_size: u32,
    previous: Option<u64>,
    entries: Vec<RevisionEntry>,
}

impl HybridSupplement {
    /// Creates an untrusted parsed supplement for validation during composition.
    pub fn new(
        snapshot: SourceSnapshot,
        startxref: u64,
        container: ObjectRef,
        declared_size: u32,
        previous: Option<u64>,
        entries: Vec<RevisionEntry>,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            container,
            declared_size,
            previous,
            entries,
        }
    }

    /// Returns the supplement's physical xref-stream object offset.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the xref-stream container object identity.
    pub const fn container(&self) -> ObjectRef {
        self.container
    }

    /// Returns trailer-like `/Prev` metadata retained but ignored for primary-chain traversal.
    pub const fn previous(&self) -> Option<u64> {
        self.previous
    }

    /// Returns entries in required strictly increasing object-number order.
    pub fn entries(&self) -> &[RevisionEntry] {
        &self.entries
    }
}

/// One parsed primary revision supplied newest-to-oldest for bounded composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionCandidate {
    snapshot: SourceSnapshot,
    startxref: u64,
    declared_size: u32,
    root: ObjectRef,
    previous: Option<u64>,
    primary_kind: RevisionPrimaryKind,
    primary_container: Option<ObjectRef>,
    primary_entries: Vec<RevisionEntry>,
    supplement: Option<HybridSupplement>,
}

impl RevisionCandidate {
    /// Creates an untrusted parsed traditional revision for later chain validation.
    pub fn traditional(
        snapshot: SourceSnapshot,
        startxref: u64,
        declared_size: u32,
        root: ObjectRef,
        previous: Option<u64>,
        primary_entries: Vec<RevisionEntry>,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            declared_size,
            root,
            previous,
            primary_kind: RevisionPrimaryKind::Traditional,
            primary_container: None,
            primary_entries,
            supplement: None,
        }
    }

    /// Creates an untrusted parsed primary xref-stream revision for later chain validation.
    pub fn xref_stream(
        snapshot: SourceSnapshot,
        startxref: u64,
        container: ObjectRef,
        declared_size: u32,
        root: ObjectRef,
        previous: Option<u64>,
        primary_entries: Vec<RevisionEntry>,
    ) -> Self {
        Self {
            snapshot,
            startxref,
            declared_size,
            root,
            previous,
            primary_kind: RevisionPrimaryKind::Stream,
            primary_container: Some(container),
            primary_entries,
            supplement: None,
        }
    }

    /// Attaches one parsed hybrid supplement; composition validates its placement and metadata.
    pub fn with_hybrid_supplement(mut self, supplement: HybridSupplement) -> Self {
        self.supplement = Some(supplement);
        self
    }

    /// Returns the immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the primary section's physical offset.
    pub const fn startxref(&self) -> u64 {
        self.startxref
    }

    /// Returns the declared object-number space.
    pub const fn declared_size(&self) -> u32 {
        self.declared_size
    }

    /// Returns the trailer root selected by this revision.
    pub const fn root(&self) -> ObjectRef {
        self.root
    }

    /// Returns the older primary offset named by `/Prev`.
    pub const fn previous(&self) -> Option<u64> {
        self.previous
    }

    /// Returns the primary table representation.
    pub const fn primary_kind(&self) -> RevisionPrimaryKind {
        self.primary_kind
    }

    /// Returns the primary xref-stream container for a stream revision.
    pub const fn primary_container(&self) -> Option<ObjectRef> {
        self.primary_container
    }

    /// Returns primary entries in required strictly increasing object-number order.
    pub fn primary_entries(&self) -> &[RevisionEntry] {
        &self.primary_entries
    }

    /// Returns an optional validated-after-composition hybrid supplement.
    pub const fn hybrid_supplement(&self) -> Option<&HybridSupplement> {
        self.supplement.as_ref()
    }
}

/// Stable ordinal in a composed chain; zero is the oldest base revision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RevisionId(u32);

impl RevisionId {
    /// Returns the oldest-to-newest zero-based ordinal.
    pub const fn ordinal(self) -> u32 {
        self.0
    }
}

/// Layer that supplied the winning entry inside one revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionEntryOrigin {
    /// The primary traditional table or xref stream.
    Primary,
    /// The same-revision xref stream referenced by a traditional table's `/XRefStm`.
    HybridSupplement,
}

/// One latest-wins lookup result with revision and layer provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedXrefEntry {
    revision: RevisionId,
    revision_startxref: u64,
    origin: RevisionEntryOrigin,
    entry: RevisionEntry,
}

impl ResolvedXrefEntry {
    /// Returns the winning revision ordinal.
    pub const fn revision(self) -> RevisionId {
        self.revision
    }

    /// Returns the winning primary revision offset.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns the primary or hybrid-supplement layer.
    pub const fn origin(self) -> RevisionEntryOrigin {
        self.origin
    }

    /// Returns the winning parsed entry.
    pub const fn entry(self) -> RevisionEntry {
        self.entry
    }
}

/// Bounded work and retained-capacity evidence for one composed chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionStats {
    revisions: u32,
    sections: u32,
    entries: u64,
    hybrid_supplements: u32,
    retained_bytes: u64,
}

impl RevisionStats {
    /// Returns the number of primary revisions.
    pub const fn revisions(self) -> u32 {
        self.revisions
    }

    /// Returns primary sections plus hybrid supplements.
    pub const fn sections(self) -> u32 {
        self.sections
    }

    /// Returns primary plus supplement entries inspected.
    pub const fn entries(self) -> u64 {
        self.entries
    }

    /// Returns the number of hybrid supplements.
    pub const fn hybrid_supplements(self) -> u32 {
        self.hybrid_supplements
    }

    /// Returns allocator-reported retained revision and entry vector bytes.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
}

/// Source-bound newest-to-oldest revision chain with strict latest-wins lookup.
#[derive(Clone, Eq, PartialEq)]
pub struct RevisionChain {
    snapshot: SourceSnapshot,
    root: ObjectRef,
    revisions: Vec<RevisionCandidate>,
    stats: RevisionStats,
}

impl RevisionChain {
    /// Returns the immutable source identity shared by every revision.
    pub const fn source(&self) -> SourceIdentity {
        self.snapshot.identity()
    }

    /// Returns the complete immutable source snapshot.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the newest trailer root after its winning xref entry was validated.
    pub const fn root(&self) -> ObjectRef {
        self.root
    }

    /// Returns primary revisions from newest to oldest.
    pub fn revisions(&self) -> &[RevisionCandidate] {
        &self.revisions
    }

    /// Returns bounded work and retained-capacity evidence.
    pub const fn stats(&self) -> RevisionStats {
        self.stats
    }

    /// Looks up the latest definition, including a latest free entry that hides older objects.
    ///
    /// Within a hybrid revision the traditional primary table is searched before its supplement;
    /// the supplement is searched before the previous primary revision.
    pub fn entry(&self, object_number: u32) -> Option<ResolvedXrefEntry> {
        for (newest_index, revision) in self.revisions.iter().enumerate() {
            if let Some(entry) = find_entry(&revision.primary_entries, object_number) {
                return self.resolved(newest_index, RevisionEntryOrigin::Primary, *entry);
            }
            if let Some(entry) = revision
                .supplement
                .as_ref()
                .and_then(|supplement| find_entry(&supplement.entries, object_number))
            {
                return self.resolved(newest_index, RevisionEntryOrigin::HybridSupplement, *entry);
            }
        }
        None
    }

    fn resolved(
        &self,
        newest_index: usize,
        origin: RevisionEntryOrigin,
        entry: RevisionEntry,
    ) -> Option<ResolvedXrefEntry> {
        let revision_count = u32::try_from(self.revisions.len()).ok()?;
        let newest_index = u32::try_from(newest_index).ok()?;
        let ordinal = revision_count.checked_sub(newest_index)?.checked_sub(1)?;
        Some(ResolvedXrefEntry {
            revision: RevisionId(ordinal),
            revision_startxref: self.revisions[usize::try_from(newest_index).ok()?].startxref,
            origin,
            entry,
        })
    }
}

impl fmt::Debug for RevisionChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RevisionChain")
            .field("snapshot", &self.snapshot)
            .field("root", &self.root)
            .field("revision_count", &self.revisions.len())
            .field("stats", &self.stats)
            .finish()
    }
}

/// Validates and composes parsed primary revisions supplied from newest to oldest.
///
/// This function performs no source acquisition, stream decoding, object framing, or repair. Its
/// inputs are parsed candidates and remain untrusted until every source, `/Prev`, `/Size`, hybrid,
/// entry, root, resource, and cancellation invariant succeeds.
pub fn compose_revision_chain(
    revisions: Vec<RevisionCandidate>,
    limits: RevisionLimits,
    cancellation: &(dyn XrefCancellation + '_),
) -> Result<RevisionChain, RevisionError> {
    check_cancelled(cancellation)?;
    if revisions.is_empty() {
        return Err(RevisionError::for_code(RevisionErrorCode::EmptyChain, None));
    }
    let revision_count = u32::try_from(revisions.len()).map_err(|_| {
        RevisionError::resource(
            RevisionLimitKind::Revisions,
            u64::from(limits.max_revisions),
            u64::MAX,
        )
    })?;
    if revision_count > limits.max_revisions {
        return Err(RevisionError::resource(
            RevisionLimitKind::Revisions,
            u64::from(limits.max_revisions),
            u64::from(revision_count),
        ));
    }

    let snapshot = revisions[0].snapshot;
    let source_len = snapshot.len().ok_or_else(|| {
        RevisionError::for_code(
            RevisionErrorCode::SourceMismatch,
            Some(revisions[0].startxref),
        )
    })?;
    let mut section_count = revision_count;
    let mut entry_count = 0_u64;
    let mut hybrid_supplements = 0_u32;
    let mut retained_bytes = capacity_bytes::<RevisionCandidate>(revisions.capacity())?;

    validate_unique_anchors(&revisions, cancellation)?;

    for (revision_index, revision) in revisions.iter().enumerate() {
        if revision_index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        validate_revision_source(revision, snapshot, source_len)?;
        if revision.declared_size == 0 || revision.root.number() >= revision.declared_size {
            return Err(RevisionError::for_object(
                RevisionErrorCode::InvalidRevision,
                revision.startxref,
                revision.root.number(),
            ));
        }
        if revision
            .primary_container
            .is_some_and(|container| container.number() >= revision.declared_size)
        {
            return Err(RevisionError::for_code(
                RevisionErrorCode::InvalidRevision,
                Some(revision.startxref),
            ));
        }
        if let Some(older) = revisions.get(revision_index + 1) {
            if revision.previous != Some(older.startxref) || older.startxref >= revision.startxref {
                return Err(RevisionError::for_code(
                    RevisionErrorCode::InvalidPrevious,
                    Some(revision.startxref),
                ));
            }
            if revision.declared_size < older.declared_size {
                return Err(RevisionError::for_code(
                    RevisionErrorCode::InvalidRevision,
                    Some(revision.startxref),
                ));
            }
        } else if revision.previous.is_some() {
            return Err(RevisionError::for_code(
                RevisionErrorCode::InvalidPrevious,
                Some(revision.startxref),
            ));
        }

        let primary_count = validate_entries(
            &revision.primary_entries,
            revision.declared_size,
            revision.startxref,
            revision.primary_container,
            source_len,
            cancellation,
        )?;
        if let Some(container) = revision.primary_container {
            validate_stream_anchor(
                &revision.primary_entries,
                &[],
                container,
                revision.startxref,
            )?;
        }
        entry_count = charge_entries(entry_count, primary_count, limits)?;
        retained_bytes = charge_retained(
            retained_bytes,
            capacity_bytes::<RevisionEntry>(revision.primary_entries.capacity())?,
            limits,
        )?;

        if revision_index + 1 == revisions.len()
            && revision.primary_kind == RevisionPrimaryKind::Traditional
        {
            validate_complete_traditional_base(revision)?;
        }

        if let Some(supplement) = &revision.supplement {
            section_count = section_count.checked_add(1).ok_or_else(|| {
                RevisionError::resource(
                    RevisionLimitKind::Sections,
                    u64::from(limits.max_sections),
                    u64::MAX,
                )
            })?;
            if section_count > limits.max_sections {
                return Err(RevisionError::resource(
                    RevisionLimitKind::Sections,
                    u64::from(limits.max_sections),
                    u64::from(section_count),
                ));
            }
            hybrid_supplements = hybrid_supplements
                .checked_add(1)
                .ok_or_else(|| RevisionError::for_code(RevisionErrorCode::InternalState, None))?;
            validate_hybrid(revision, supplement, snapshot, source_len)?;
            let supplement_count = validate_entries(
                &supplement.entries,
                supplement.declared_size,
                supplement.startxref,
                Some(supplement.container),
                source_len,
                cancellation,
            )?;
            validate_stream_anchor(
                &revision.primary_entries,
                &supplement.entries,
                supplement.container,
                supplement.startxref,
            )?;
            entry_count = charge_entries(entry_count, supplement_count, limits)?;
            retained_bytes = charge_retained(
                retained_bytes,
                capacity_bytes::<RevisionEntry>(supplement.entries.capacity())?,
                limits,
            )?;
        }
    }
    check_cancelled(cancellation)?;
    if retained_bytes > limits.max_retained_bytes {
        return Err(RevisionError::resource(
            RevisionLimitKind::RetainedBytes,
            limits.max_retained_bytes,
            retained_bytes,
        ));
    }

    let root = revisions[0].root;
    let chain = RevisionChain {
        snapshot,
        root,
        revisions,
        stats: RevisionStats {
            revisions: revision_count,
            sections: section_count,
            entries: entry_count,
            hybrid_supplements,
            retained_bytes,
        },
    };
    validate_root(&chain)?;
    Ok(chain)
}

fn validate_revision_source(
    revision: &RevisionCandidate,
    snapshot: SourceSnapshot,
    source_len: u64,
) -> Result<(), RevisionError> {
    if revision.snapshot != snapshot || revision.startxref == 0 || revision.startxref >= source_len
    {
        return Err(RevisionError::for_code(
            RevisionErrorCode::SourceMismatch,
            Some(revision.startxref),
        ));
    }
    Ok(())
}

fn validate_hybrid(
    revision: &RevisionCandidate,
    supplement: &HybridSupplement,
    snapshot: SourceSnapshot,
    source_len: u64,
) -> Result<(), RevisionError> {
    if revision.primary_kind != RevisionPrimaryKind::Traditional
        || revision.previous.is_none()
        || supplement.snapshot != snapshot
        || supplement.startxref == 0
        || supplement.startxref >= revision.startxref
        || supplement.startxref >= source_len
        || supplement.declared_size != revision.declared_size
        || revision
            .previous
            .is_none_or(|previous| supplement.startxref <= previous)
        || supplement.container.number() >= supplement.declared_size
    {
        return Err(RevisionError::for_code(
            RevisionErrorCode::InvalidHybrid,
            Some(supplement.startxref),
        ));
    }
    Ok(())
}

fn validate_entries(
    entries: &[RevisionEntry],
    declared_size: u32,
    startxref: u64,
    self_container: Option<ObjectRef>,
    source_len: u64,
    cancellation: &dyn XrefCancellation,
) -> Result<u64, RevisionError> {
    let mut previous = None;
    for (index, entry) in entries.iter().copied().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        if entry.object_number >= declared_size
            || previous.is_some_and(|value| entry.object_number <= value)
        {
            return Err(RevisionError::for_object(
                RevisionErrorCode::InvalidRevision,
                startxref,
                entry.object_number,
            ));
        }
        match entry.kind {
            RevisionEntryKind::Null { .. } => {}
            RevisionEntryKind::Free { next_free, .. } => {
                if next_free >= declared_size {
                    return Err(RevisionError::for_object(
                        RevisionErrorCode::InvalidRevision,
                        startxref,
                        entry.object_number,
                    ));
                }
            }
            RevisionEntryKind::Uncompressed { offset, generation } => {
                let is_self_entry = self_container.is_some_and(|container| {
                    container.number() == entry.object_number
                        && container.generation() == generation
                });
                if offset == 0
                    || offset >= source_len
                    || (is_self_entry && offset != startxref)
                    || (!is_self_entry && offset >= startxref)
                {
                    return Err(RevisionError::for_object(
                        RevisionErrorCode::InvalidRevision,
                        startxref,
                        entry.object_number,
                    ));
                }
            }
            RevisionEntryKind::Compressed { object_stream, .. } => {
                if object_stream == 0
                    || object_stream >= declared_size
                    || object_stream == entry.object_number
                {
                    return Err(RevisionError::for_object(
                        RevisionErrorCode::InvalidRevision,
                        startxref,
                        entry.object_number,
                    ));
                }
            }
        }
        previous = Some(entry.object_number);
    }
    u64::try_from(entries.len())
        .map_err(|_| RevisionError::for_code(RevisionErrorCode::InternalState, None))
}

fn validate_unique_anchors(
    revisions: &[RevisionCandidate],
    cancellation: &dyn XrefCancellation,
) -> Result<(), RevisionError> {
    for (index, revision) in revisions.iter().enumerate() {
        if index.is_multiple_of(CANCELLATION_INTERVAL) {
            check_cancelled(cancellation)?;
        }
        let Some(supplement) = &revision.supplement else {
            continue;
        };
        for (other_index, other) in revisions.iter().enumerate() {
            if other_index.is_multiple_of(CANCELLATION_INTERVAL) {
                check_cancelled(cancellation)?;
            }
            if supplement.startxref == other.startxref
                || other_index < index
                    && other
                        .supplement
                        .as_ref()
                        .is_some_and(|candidate| candidate.startxref == supplement.startxref)
            {
                return Err(RevisionError::for_code(
                    RevisionErrorCode::InvalidHybrid,
                    Some(supplement.startxref),
                ));
            }
        }
    }
    Ok(())
}

fn validate_stream_anchor(
    primary_entries: &[RevisionEntry],
    supplement_entries: &[RevisionEntry],
    container: ObjectRef,
    startxref: u64,
) -> Result<(), RevisionError> {
    let entry = find_entry(primary_entries, container.number())
        .or_else(|| find_entry(supplement_entries, container.number()));
    let valid = entry.is_some_and(|entry| {
        matches!(
            entry.kind,
            RevisionEntryKind::Uncompressed { offset, generation }
                if offset == startxref && generation == container.generation()
        )
    });
    if !valid {
        return Err(RevisionError::for_object(
            RevisionErrorCode::InvalidRevision,
            startxref,
            container.number(),
        ));
    }
    Ok(())
}

fn validate_complete_traditional_base(revision: &RevisionCandidate) -> Result<(), RevisionError> {
    let expected_len = usize::try_from(revision.declared_size).map_err(|_| {
        RevisionError::for_code(RevisionErrorCode::InternalState, Some(revision.startxref))
    })?;
    let complete = revision.primary_entries.len() == expected_len
        && revision
            .primary_entries
            .iter()
            .enumerate()
            .all(|(index, entry)| {
                u32::try_from(index).is_ok_and(|object_number| object_number == entry.object_number)
            })
        && revision
            .primary_entries
            .first()
            .is_some_and(|entry| matches!(entry.kind, RevisionEntryKind::Free { .. }));
    if !complete {
        return Err(RevisionError::for_code(
            RevisionErrorCode::InvalidRevision,
            Some(revision.startxref),
        ));
    }
    Ok(())
}

fn validate_root(chain: &RevisionChain) -> Result<(), RevisionError> {
    let root = chain.root;
    let Some(resolved) = chain.entry(root.number()) else {
        return Err(RevisionError::for_object(
            RevisionErrorCode::InvalidRoot,
            chain.revisions[0].startxref,
            root.number(),
        ));
    };
    let valid = match resolved.entry.kind {
        RevisionEntryKind::Null { .. } => false,
        RevisionEntryKind::Free { .. } => false,
        RevisionEntryKind::Uncompressed { generation, .. } => generation == root.generation(),
        RevisionEntryKind::Compressed { .. } => root.generation() == 0,
    };
    if !valid {
        return Err(RevisionError::for_object(
            RevisionErrorCode::InvalidRoot,
            chain.revisions[0].startxref,
            root.number(),
        ));
    }
    if chain.revisions[0].primary_kind == RevisionPrimaryKind::Traditional
        && chain.revisions[0].supplement.is_some()
        && resolved.revision_startxref == chain.revisions[0].startxref
        && resolved.origin == RevisionEntryOrigin::HybridSupplement
    {
        return Err(RevisionError::for_object(
            RevisionErrorCode::InvalidRoot,
            chain.revisions[0].startxref,
            root.number(),
        ));
    }
    Ok(())
}

fn find_entry(entries: &[RevisionEntry], object_number: u32) -> Option<&RevisionEntry> {
    entries
        .binary_search_by_key(&object_number, |entry| entry.object_number)
        .ok()
        .map(|index| &entries[index])
}

fn charge_entries(
    consumed: u64,
    attempted: u64,
    limits: RevisionLimits,
) -> Result<u64, RevisionError> {
    let total = consumed.checked_add(attempted).ok_or_else(|| {
        RevisionError::resource(RevisionLimitKind::Entries, limits.max_entries, u64::MAX)
    })?;
    if total > limits.max_entries {
        return Err(RevisionError::resource(
            RevisionLimitKind::Entries,
            limits.max_entries,
            total,
        ));
    }
    Ok(total)
}

fn charge_retained(
    consumed: u64,
    attempted: u64,
    limits: RevisionLimits,
) -> Result<u64, RevisionError> {
    let total = consumed.checked_add(attempted).ok_or_else(|| {
        RevisionError::resource(
            RevisionLimitKind::RetainedBytes,
            limits.max_retained_bytes,
            u64::MAX,
        )
    })?;
    if total > limits.max_retained_bytes {
        return Err(RevisionError::resource(
            RevisionLimitKind::RetainedBytes,
            limits.max_retained_bytes,
            total,
        ));
    }
    Ok(total)
}

fn capacity_bytes<T>(capacity: usize) -> Result<u64, RevisionError> {
    let bytes = capacity
        .checked_mul(mem::size_of::<T>())
        .ok_or_else(|| RevisionError::for_code(RevisionErrorCode::InternalState, None))?;
    u64::try_from(bytes)
        .map_err(|_| RevisionError::for_code(RevisionErrorCode::InternalState, None))
}

fn check_cancelled(cancellation: &dyn XrefCancellation) -> Result<(), RevisionError> {
    if cancellation.is_cancelled() {
        Err(RevisionError::for_code(RevisionErrorCode::Cancelled, None))
    } else {
        Ok(())
    }
}
