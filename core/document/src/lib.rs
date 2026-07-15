//! Strict base-revision indexing and resumable top-level object attestation.
//!
//! This crate first derives explicitly unauthenticated physical intervals from
//! one traditional xref section. A separate consuming job frames every in-use
//! object in physical order and proves trivia closure through `startxref`
//! before publishing an attested typestate. Only that typestate can mint a
//! bounded job that reopens one parsed object into a wrapper retaining its
//! proof. A second bounded job can iteratively follow whole-object direct
//! reference aliases. Separate strict-base jobs validate the trailer Catalog
//! and either enumerate its bounded document outline or traverse the complete
//! Page/Pages tree. The outline job checks linked-list topology, signed
//! visible-item counts, decoded titles, and direct target shape; the page job
//! publishes a scalar page count with exact Parent, Count, cycle, and
//! duplicate-child checks. The crate also owns bounded ISO 32000-1 text-string
//! decoding from lexical PDF strings into UTF-8 without exposing source content
//! in diagnostics. Other dictionary, array, stream, and nested-reference
//! semantics remain outside a complete object-graph resolver.
//! A strict base-open composition job connects traditional xref parsing,
//! candidate indexing, and top-level attestation without exposing an
//! unauthenticated intermediate typestate.
//! A separate source xref-stream job frames an already-classified primary or
//! hybrid stream anchor, acquires its exact direct-Length payload, and retains
//! the framed container with an unfiltered table proof. A parent source-chain
//! job discovers the final marker, classifies and acquires traditional,
//! primary-stream, and hybrid sections newest-to-oldest, and publishes only
//! after pure revision composition succeeds. Both jobs still reject indirect
//! Length and filtered payloads, and neither integrates the chain into the
//! strict attestation opener or a Session.
//! An explicit local-repair planning surface can instead retain xref and
//! object-offset proof, rebuild every effective interval atomically, and
//! publish only an explicitly unauthenticated wrapper. A consuming sibling
//! then reruns complete header/object/gap attestation over that rebuilt geometry,
//! revalidates planned direct-length repairs under aggregate work caps, and
//! publishes a distinct repaired typestate retaining the full repair ledger.
//! `OpenLocallyRepairedBaseRevisionJob` is the single core entry that owns this
//! complete R1 sequence, including local xref discovery, aggregate-bounded
//! first-pass object probes, atomic geometry rebuild, and final attestation.
//! Successful proof-bearing values retain their resolution profile and expose
//! checked value-owned footprint components as evidence for a future cache owner,
//! but this crate does not cache them.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::result_large_err,
    reason = "document errors retain complete copyable lower-layer errors without fallback allocation"
)]

mod access;
mod attestation;
mod attestation_limits;
mod catalog;
mod dictionary;
mod error;
mod index;
mod limits;
mod local_repair_open;
mod model;
mod outline;
mod outline_limits;
mod page_tree;
mod page_tree_limits;
mod reference_chain;
mod reference_chain_limits;
mod repair;
mod residency;
mod revision_resolver;
mod source_revision_chain;
mod source_xref_stream;
mod strict_base_open;
mod text_string;

pub use access::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPhase, AttestedObjectPoll,
    OpenAttestedObjectJob,
};
pub use attestation::{
    AttestRevisionJob, RevisionAttestationJobContext, RevisionAttestationPhase,
    RevisionAttestationPoll, RevisionAttestationStats,
};
pub use attestation_limits::{RevisionAttestationLimitConfig, RevisionAttestationLimits};
pub use catalog::StrictCatalog;
pub use error::{
    DocumentError, DocumentErrorCategory, DocumentErrorCode, DocumentLimit, DocumentLimitKind,
    DocumentRecoverability,
};
pub use index::{DocumentCancellation, NeverCancelled};
pub use limits::{DocumentLimitConfig, DocumentLimits};
pub use local_repair_open::{
    LocalRepairOpenContext, LocalRepairOpenError, LocalRepairOpenLimits, LocalRepairOpenPhase,
    LocalRepairOpenPoll, LocalRepairOpenStats, LocalRepairProbeLimitConfig, LocalRepairProbeLimits,
    LocalRepairProbeStats, OpenLocallyRepairedBaseRevisionJob,
};
pub use model::{
    AttestedRevisionIndex, CandidateRevisionIndex, DocumentIndexStats, ObjectAttestation,
    ObjectAttestationKind, PhysicalObjectInterval, RevisionId, SharedAttestedRevisionIndex,
};
pub use outline::{
    Outline, OutlineItem, OutlineJobContext, OutlinePhase, OutlinePoll, OutlineStats,
    OutlineTargetKind, ReadOutlineJob,
};
pub use outline_limits::{OutlineLimitConfig, OutlineLimits};
pub use page_tree::{
    CountPagesJob, PageCount, PageCountPoll, PageTreeJobContext, PageTreePhase, PageTreeStats,
};
pub use page_tree_limits::{PageTreeLimitConfig, PageTreeLimits};
pub use reference_chain::{
    ReferenceChain, ReferenceChainError, ReferenceChainJobContext, ReferenceChainPhase,
    ReferenceChainPoll, ReferenceChainStats, ResolveReferenceChainJob, ResolvedReference,
};
pub use reference_chain_limits::{ReferenceChainLimitConfig, ReferenceChainLimits};
pub use repair::{
    AttestLocalRepairRevisionJob, EffectiveObjectOffset, LocalRepairPlanningRevision,
    LocalRevisionAttestationJobContext, LocalRevisionAttestationPoll,
    LocallyRebuiltCandidateRevision, LocallyRepairedRevisionIndex, RepairGeometryStats,
};
pub use residency::DocumentResidentFootprint;
pub use revision_resolver::{
    CompressedObjectLocator, EffectiveObjectLocator, ResolveObjectJob, ResolvedCompressedObject,
    ResolvedObject, RevisionObjectIndex, RevisionObjectIndexStats, RevisionResolverJobContext,
    RevisionResolverLimits, RevisionResolverPhase, RevisionResolverPoll, RevisionResolverStats,
    UncompressedObjectLocator,
};
pub use source_revision_chain::{
    NeverCancelSourceRevisionChain, OpenSourceRevisionChainJob, SourceAcquiredRevisionChain,
    SourceHybridRevisionProof, SourceRevisionChainCancellation, SourceRevisionChainError,
    SourceRevisionChainErrorCategory, SourceRevisionChainErrorCode, SourceRevisionChainJobContext,
    SourceRevisionChainLimit, SourceRevisionChainLimitConfig, SourceRevisionChainLimitKind,
    SourceRevisionChainLimits, SourceRevisionChainPhase, SourceRevisionChainPoll,
    SourceRevisionChainRecoverability, SourceRevisionChainStats, SourceRevisionPrimaryProof,
    SourceRevisionProof,
};
pub use source_xref_stream::{
    NeverCancelSourceXrefStream, OpenSourceXrefStreamJob, SourceAcquiredXrefStream,
    SourceXrefStreamCancellation, SourceXrefStreamError, SourceXrefStreamErrorCategory,
    SourceXrefStreamErrorCode, SourceXrefStreamJobContext, SourceXrefStreamLimit,
    SourceXrefStreamLimitKind, SourceXrefStreamPhase, SourceXrefStreamPoll,
    SourceXrefStreamRecoverability, SourceXrefStreamStats,
};
pub use strict_base_open::{
    OpenStrictBaseRevisionJob, StrictBaseOpenContext, StrictBaseOpenError, StrictBaseOpenLimits,
    StrictBaseOpenPhase, StrictBaseOpenPoll, StrictBaseOpenStats,
};
pub use text_string::{
    DecodedTextString, TextStringEncoding, TextStringError, TextStringErrorCategory,
    TextStringErrorCode, TextStringLimit, TextStringLimitConfig, TextStringLimitKind,
    TextStringLimits, TextStringRecoverability, decode_text_string,
};
