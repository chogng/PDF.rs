use std::fmt;
use std::mem;

use pdf_rs_bytes::SourceSnapshot;
use pdf_rs_object::{
    IndirectObjectTarget, IndirectObjectValue, LocallyFramedObject, ObjectRepairDiagnostic,
    ObjectRepairKind,
};
use pdf_rs_syntax::ObjectRef;
use pdf_rs_xref::{LocallyParsedXrefSection, XrefRepairDiagnostic};

use crate::{
    CandidateRevisionIndex, DocumentCancellation, DocumentError, DocumentErrorCode,
    DocumentIndexStats, DocumentLimitKind, DocumentLimits, PhysicalObjectInterval, RevisionId,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectiveObjectDiagnostics {
    None,
    One(ObjectRepairDiagnostic),
    Two([ObjectRepairDiagnostic; 2]),
}

impl EffectiveObjectDiagnostics {
    fn from_object(object: &LocallyFramedObject) -> Result<Self, DocumentError> {
        let diagnostics = object.diagnostics();
        let mut offset_seen = false;
        let mut length_seen = false;
        for diagnostic in diagnostics {
            if diagnostic.snapshot() != object.snapshot()
                || diagnostic.reference() != object.reference()
            {
                return Err(invalid_object_evidence(object));
            }
            match diagnostic.kind() {
                ObjectRepairKind::ObjectOffset
                    if diagnostic.declared() == object.declared_xref_offset()
                        && diagnostic.effective() == object.effective_xref_offset()
                        && !offset_seen =>
                {
                    offset_seen = true;
                }
                ObjectRepairKind::DirectStreamLength => {
                    if length_seen {
                        return Err(invalid_object_evidence(object));
                    }
                    length_seen = true;
                    let IndirectObjectValue::Stream(stream) = object.value() else {
                        return Err(invalid_object_evidence(object));
                    };
                    if stream.length_claim().declaration().direct_value()
                        != Some(diagnostic.declared())
                        || stream.length_claim().value() != diagnostic.effective()
                    {
                        return Err(invalid_object_evidence(object));
                    }
                }
                ObjectRepairKind::ObjectOffset => return Err(invalid_object_evidence(object)),
            }
        }
        if let IndirectObjectValue::Stream(stream) = object.value()
            && stream.length_claim().declaration().direct_value()
                != Some(stream.length_claim().value())
            && !length_seen
        {
            return Err(invalid_object_evidence(object));
        }
        match diagnostics {
            [] => Ok(Self::None),
            [diagnostic] => Ok(Self::One(*diagnostic)),
            [first, second] => Ok(Self::Two([*first, *second])),
            _ => Err(invalid_object_evidence(object)),
        }
    }

    fn as_slice(&self) -> &[ObjectRepairDiagnostic] {
        match self {
            Self::None => &[],
            Self::One(diagnostic) => std::slice::from_ref(diagnostic),
            Self::Two(diagnostics) => diagnostics,
        }
    }
}

fn invalid_object_evidence(object: &LocallyFramedObject) -> DocumentError {
    DocumentError::for_code(
        DocumentErrorCode::InternalState,
        Some(object.reference()),
        Some(object.effective_xref_offset()),
    )
}

/// Fixed-size proof that one xref-declared object offset was normally framed at an effective offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectiveObjectOffset {
    snapshot: SourceSnapshot,
    reference: ObjectRef,
    declared_offset: u64,
    effective_offset: u64,
    original_upper_bound: u64,
    revision_startxref: u64,
    diagnostics: EffectiveObjectDiagnostics,
}

impl EffectiveObjectOffset {
    /// Derives geometry and all object-repair evidence from one proof-bearing object.
    pub fn from_locally_framed(object: &LocallyFramedObject) -> Result<Self, DocumentError> {
        let diagnostics = EffectiveObjectDiagnostics::from_object(object)?;
        let offset_diagnostic = diagnostics
            .as_slice()
            .iter()
            .find(|diagnostic| diagnostic.kind() == ObjectRepairKind::ObjectOffset);
        if offset_diagnostic.is_none()
            && object.declared_xref_offset() != object.effective_xref_offset()
        {
            return Err(invalid_object_evidence(object));
        }
        if object.header_span().start() != object.effective_xref_offset()
            || object.object_span().start() != object.effective_xref_offset()
            || object.object_span().end_exclusive() > object.object_upper_bound()
        {
            return Err(invalid_object_evidence(object));
        }
        Ok(Self {
            snapshot: object.snapshot(),
            reference: object.reference(),
            declared_offset: object.declared_xref_offset(),
            effective_offset: object.effective_xref_offset(),
            original_upper_bound: object.object_upper_bound(),
            revision_startxref: object.revision_startxref(),
            diagnostics,
        })
    }

    /// Returns the immutable snapshot supplying the framed object and diagnostic.
    pub const fn snapshot(self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the exact object number and generation proven by normal framing.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }

    /// Returns the offset claimed by the original xref-derived interval.
    pub const fn declared_offset(self) -> u64 {
        self.declared_offset
    }

    /// Returns the exact normally validated object-header offset.
    pub const fn effective_offset(self) -> u64 {
        self.effective_offset
    }

    /// Returns the original next-object or revision upper bound used during probing.
    pub const fn original_upper_bound(self) -> u64 {
        self.original_upper_bound
    }

    /// Returns the revision anchor used during normal object framing.
    pub const fn revision_startxref(self) -> u64 {
        self.revision_startxref
    }

    /// Returns every source-bound object repair diagnostic, including direct-length evidence.
    pub fn diagnostics(&self) -> &[ObjectRepairDiagnostic] {
        self.diagnostics.as_slice()
    }

    /// Returns source-bound object-offset evidence, or `None` for an unchanged header offset.
    pub fn offset_diagnostic(&self) -> Option<ObjectRepairDiagnostic> {
        self.diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.kind() == ObjectRepairKind::ObjectOffset)
            .copied()
    }

    /// Reports whether the xref-declared header offset required local correction.
    pub fn is_offset_repaired(&self) -> bool {
        self.offset_diagnostic().is_some()
    }
}

/// Geometry-rebuild work and retained-plan accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepairGeometryStats {
    plan_bytes: u64,
    repaired_offsets: u64,
    object_repairs: u64,
    additional_sort_steps: u64,
}

impl RepairGeometryStats {
    /// Returns allocator-reported capacity retained by the complete offset plan.
    pub const fn plan_bytes(self) -> u64 {
        self.plan_bytes
    }

    /// Returns object offsets whose effective header differs from the xref declaration.
    pub const fn repaired_offsets(self) -> u64 {
        self.repaired_offsets
    }

    /// Returns all retained object repair diagnostics, including direct stream lengths.
    pub const fn object_repairs(self) -> u64 {
        self.object_repairs
    }

    /// Returns comparisons and swaps added while sorting all effective offsets together.
    pub const fn additional_sort_steps(self) -> u64 {
        self.additional_sort_steps
    }
}

/// Proof-bound but explicitly unauthenticated planning view for local object-offset probes.
pub struct LocalRepairPlanningRevision {
    xref: LocallyParsedXrefSection,
    candidate: CandidateRevisionIndex,
    limits: DocumentLimits,
}

impl LocalRepairPlanningRevision {
    /// Builds original xref-derived intervals while retaining the local-xref proof wrapper.
    pub fn new(
        xref: LocallyParsedXrefSection,
        revision_id: RevisionId,
        limits: DocumentLimits,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Self, DocumentError> {
        let candidate = CandidateRevisionIndex::from_locally_parsed_xref(
            &xref,
            revision_id,
            limits,
            cancellation,
        )?;
        Ok(Self {
            xref,
            candidate,
            limits,
        })
    }

    /// Returns the immutable source snapshot bound to xref and candidate geometry.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.candidate.snapshot()
    }

    /// Returns the caller-assigned revision identity.
    pub const fn revision_id(&self) -> RevisionId {
        self.candidate.revision_id()
    }

    /// Returns the normally validated strict or repaired xref anchor.
    pub const fn startxref(&self) -> u64 {
        self.candidate.startxref()
    }

    /// Returns the exact-generation in-use trailer root.
    pub const fn root(&self) -> ObjectRef {
        self.candidate.root()
    }

    /// Returns the original xref-derived intervals in increasing declared-offset order.
    pub fn physical_intervals(&self) -> &[PhysicalObjectInterval] {
        self.candidate.physical_intervals()
    }

    /// Forms one explicitly unattested target bound to the original candidate interval.
    pub fn unattested_target(
        &self,
        reference: ObjectRef,
    ) -> Result<IndirectObjectTarget, DocumentError> {
        self.candidate.unattested_target(reference)
    }

    /// Returns local-xref repair evidence retained by the planning wrapper.
    pub fn xref_diagnostics(&self) -> &[XrefRepairDiagnostic] {
        self.xref.diagnostics()
    }

    /// Returns original candidate-index allocation and sort accounting.
    pub const fn index_stats(&self) -> DocumentIndexStats {
        self.candidate.stats()
    }

    /// Consumes the complete proof list and rebuilds all effective geometry atomically.
    pub fn rebuild(
        self,
        evidence: Vec<EffectiveObjectOffset>,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<LocallyRebuiltCandidateRevision, DocumentError> {
        LocallyRebuiltCandidateRevision::from_plan(self, evidence, cancellation)
    }
}

impl fmt::Debug for LocalRepairPlanningRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalRepairPlanningRevision")
            .field("snapshot", &self.snapshot())
            .field("revision_id", &self.revision_id())
            .field("startxref", &self.startxref())
            .field("root", &self.root())
            .field("index_stats", &self.index_stats())
            .field("xref_diagnostics", &self.xref.diagnostics())
            .field("physical_intervals", &"[UNATTESTED]")
            .finish()
    }
}

/// Complete effective-offset geometry that still requires normal top-level attestation.
pub struct LocallyRebuiltCandidateRevision {
    xref: LocallyParsedXrefSection,
    candidate: CandidateRevisionIndex,
    evidence: Vec<EffectiveObjectOffset>,
    geometry_stats: RepairGeometryStats,
}

impl LocallyRebuiltCandidateRevision {
    fn from_plan(
        plan: LocalRepairPlanningRevision,
        evidence: Vec<EffectiveObjectOffset>,
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> Result<Self, DocumentError> {
        let plan_bytes = u64::try_from(evidence.capacity())
            .ok()
            .and_then(|capacity| {
                capacity.checked_mul(u64::try_from(mem::size_of::<EffectiveObjectOffset>()).ok()?)
            })
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    plan.limits.max_logical_index_bytes(),
                    plan.candidate.stats().logical_index_bytes(),
                    u64::MAX,
                    None,
                )
            })?;
        let retained = plan
            .candidate
            .stats()
            .logical_index_bytes()
            .checked_add(plan_bytes)
            .ok_or_else(|| {
                DocumentError::resource(
                    DocumentLimitKind::LogicalIndexBytes,
                    plan.limits.max_logical_index_bytes(),
                    plan.candidate.stats().logical_index_bytes(),
                    u64::MAX,
                    None,
                )
            })?;
        if retained > plan.limits.max_logical_index_bytes() {
            return Err(DocumentError::resource(
                DocumentLimitKind::LogicalIndexBytes,
                plan.limits.max_logical_index_bytes(),
                plan.candidate.stats().logical_index_bytes(),
                plan_bytes,
                None,
            ));
        }
        let (candidate, additional_sort_steps, repaired_offsets, object_repairs) =
            plan.candidate
                .rebuild_effective_offsets(&evidence, plan.limits, cancellation)?;
        Ok(Self {
            xref: plan.xref,
            candidate,
            evidence,
            geometry_stats: RepairGeometryStats {
                plan_bytes,
                repaired_offsets,
                object_repairs,
                additional_sort_steps,
            },
        })
    }

    /// Returns the immutable snapshot covering xref, object proofs, and rebuilt geometry.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.candidate.snapshot()
    }

    /// Returns the caller-assigned revision identity.
    pub const fn revision_id(&self) -> RevisionId {
        self.candidate.revision_id()
    }

    /// Returns the normally validated strict or repaired xref anchor.
    pub const fn startxref(&self) -> u64 {
        self.candidate.startxref()
    }

    /// Returns the exact-generation in-use trailer root.
    pub const fn root(&self) -> ObjectRef {
        self.candidate.root()
    }

    /// Returns rebuilt intervals in increasing effective-offset order.
    pub fn physical_intervals(&self) -> &[PhysicalObjectInterval] {
        self.candidate.physical_intervals()
    }

    /// Looks up one exact rebuilt logical identity without minting object-access authority.
    pub fn interval(&self, reference: ObjectRef) -> Result<&PhysicalObjectInterval, DocumentError> {
        self.candidate.interval(reference)
    }

    /// Returns the complete original-to-effective object-offset plan.
    pub fn object_offset_evidence(&self) -> &[EffectiveObjectOffset] {
        &self.evidence
    }

    /// Returns inseparable local-xref repair evidence.
    pub fn xref_diagnostics(&self) -> &[XrefRepairDiagnostic] {
        self.xref.diagnostics()
    }

    /// Returns candidate-index accounting after both declared and effective sorts.
    pub const fn index_stats(&self) -> DocumentIndexStats {
        self.candidate.stats()
    }

    /// Returns retained-plan and effective-geometry rebuild work.
    pub const fn geometry_stats(&self) -> RepairGeometryStats {
        self.geometry_stats
    }
}

impl fmt::Debug for LocallyRebuiltCandidateRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocallyRebuiltCandidateRevision")
            .field("snapshot", &self.snapshot())
            .field("revision_id", &self.revision_id())
            .field("startxref", &self.startxref())
            .field("root", &self.root())
            .field("index_stats", &self.index_stats())
            .field("geometry_stats", &self.geometry_stats)
            .field("xref_diagnostics", &self.xref.diagnostics())
            .field("object_offset_evidence", &"[REDACTED]")
            .field("physical_intervals", &"[UNATTESTED]")
            .finish()
    }
}
