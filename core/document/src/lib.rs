//! Strict base-revision indexing and resumable top-level object attestation.
//!
//! This crate first derives explicitly unauthenticated physical intervals from
//! one traditional xref section. A separate consuming job frames every in-use
//! object in physical order and proves trivia closure through `startxref`
//! before publishing an attested typestate. Only that typestate can mint a
//! bounded job that reopens one parsed object into a wrapper retaining its
//! proof. A second bounded job can iteratively follow whole-object direct
//! reference aliases, but dictionaries, arrays, streams, and nested references
//! remain terminal; this is not a complete object-graph resolver. Successful
//! proof-bearing values expose checked value-owned footprint components as
//! evidence for a future cache owner, but this crate does not cache them.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(
    clippy::result_large_err,
    reason = "document errors retain complete copyable lower-layer errors without fallback allocation"
)]

mod access;
mod attestation;
mod attestation_limits;
mod error;
mod index;
mod limits;
mod model;
mod reference_chain;
mod reference_chain_limits;
mod residency;

pub use access::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPhase, AttestedObjectPoll,
    OpenAttestedObjectJob,
};
pub use attestation::{
    AttestRevisionJob, RevisionAttestationJobContext, RevisionAttestationPhase,
    RevisionAttestationPoll, RevisionAttestationStats,
};
pub use attestation_limits::{RevisionAttestationLimitConfig, RevisionAttestationLimits};
pub use error::{
    DocumentError, DocumentErrorCategory, DocumentErrorCode, DocumentLimit, DocumentLimitKind,
    DocumentRecoverability,
};
pub use index::{DocumentCancellation, NeverCancelled};
pub use limits::{DocumentLimitConfig, DocumentLimits};
pub use model::{
    AttestedRevisionIndex, CandidateRevisionIndex, DocumentIndexStats, ObjectAttestation,
    ObjectAttestationKind, PhysicalObjectInterval, RevisionId,
};
pub use reference_chain::{
    ReferenceChain, ReferenceChainError, ReferenceChainJobContext, ReferenceChainPhase,
    ReferenceChainPoll, ReferenceChainStats, ResolveReferenceChainJob, ResolvedReference,
};
pub use reference_chain_limits::{ReferenceChainLimitConfig, ReferenceChainLimits};
pub use residency::DocumentResidentFootprint;
