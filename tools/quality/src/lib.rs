#![forbid(unsafe_code)]

//! Reusable repository-quality validation primitives.

/// Read-only validation of manifests and their linked expected artifacts.
pub mod case_contract;
/// Canonical schema-1 case-manifest parsing and validation.
pub mod manifest;
