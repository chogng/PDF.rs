#![forbid(unsafe_code)]

//! Reusable repository-quality validation primitives.

/// Read-only validation of manifests and their linked expected artifacts.
pub mod case_contract;
/// Fail-closed verification of the selected signed universal macOS package.
pub mod macos_package;
/// Canonical schema-1 case-manifest parsing and validation.
pub mod manifest;
