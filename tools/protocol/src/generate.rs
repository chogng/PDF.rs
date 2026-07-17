use std::fmt::Write;
use std::path::{Component, Path, PathBuf};

use crate::codec::{CodecErrorKind, CodecLimits, FixedLeCodec, WireValue};
use crate::generate_codec::{generate_rust_payload_codec, generate_typescript_payload_codec};
use crate::hash::{lowercase_hex, sha256};
use crate::model::{EnumDef, MessageKind, Presence, Primitive, Privacy, Protocol, Type};

pub const SCHEMA_HASH_TRUNCATION: &str = "sha256-first-16-bytes";
pub const GENERATOR_VERSION: &str = "0.2.0";
pub const WIRE_IDENTITY_DOMAIN: &str = "PDF.rs/EngineProtocol/WireIdentity/v1";
pub const PAYLOAD_CODEC_ABI_VERSION: u16 = 1;
pub const CAPABILITY_DECISION_HASH_DOMAIN: &str =
    "PDF.rs/EngineProtocol/CapabilityDecision/fixed_le_v1/v1";
pub const RENDER_PLAN_MANIFEST_HASH_DOMAIN: &str =
    "PDF.rs/EngineProtocol/RenderPlanManifest/fixed_le_v1/v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedFile {
    pub relative_path: PathBuf,
    pub contents: String,
}

pub fn generated_files(protocol: &Protocol, canonical_schema: &str) -> Vec<GeneratedFile> {
    let canonical_digest = sha256(canonical_schema.as_bytes());
    let digest = wire_identity_digest(protocol, canonical_schema);
    vec![
        file(
            "runtime/protocol/src/generated.rs",
            generate_rust(protocol, &digest),
        ),
        file(
            "platform/browser/generated/engine-protocol.ts",
            generate_typescript(protocol, &digest),
        ),
        file(
            "platform/desktop/generated/engine-protocol.registry",
            generate_desktop_registry(protocol, &digest),
        ),
        file(
            "protocol/generated/schema-hash.txt",
            generate_hash_registry(&digest, &canonical_digest),
        ),
        file(
            "protocol/generated/compatibility-vectors.json",
            generate_compatibility_vectors(protocol, &digest),
        ),
        file(
            "protocol/generated/invalid-vectors.json",
            generate_invalid_vectors(protocol, &digest),
        ),
        file(
            "protocol/generated/payload-codec-vectors.json",
            generate_payload_codec_vectors(protocol, &digest),
        ),
    ]
}

fn file(path: &str, contents: String) -> GeneratedFile {
    GeneratedFile {
        relative_path: PathBuf::from(path),
        contents,
    }
}

fn wire_identity_digest(protocol: &Protocol, canonical_schema: &str) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(
        WIRE_IDENTITY_DOMAIN.len() + protocol.payload_codec.len() + canonical_schema.len() + 24,
    );
    preimage.extend_from_slice(WIRE_IDENTITY_DOMAIN.as_bytes());
    preimage.push(0);
    preimage.extend_from_slice(&PAYLOAD_CODEC_ABI_VERSION.to_le_bytes());
    preimage.extend_from_slice(
        &u32::try_from(protocol.payload_codec.len())
            .expect("validated codec name length fits u32")
            .to_le_bytes(),
    );
    preimage.extend_from_slice(protocol.payload_codec.as_bytes());
    preimage.extend_from_slice(
        &u64::try_from(canonical_schema.len())
            .expect("bounded canonical schema length fits u64")
            .to_le_bytes(),
    );
    preimage.extend_from_slice(canonical_schema.as_bytes());
    sha256(&preimage)
}

pub fn write_generated(root: &Path, files: &[GeneratedFile]) -> Result<(), String> {
    write_generated_transaction(root, files, None)
}

fn write_generated_transaction(
    root: &Path,
    files: &[GeneratedFile],
    fail_before_replace: Option<usize>,
) -> Result<(), String> {
    let _lock = GenerationLock::acquire(root)?;
    let mut staged = Vec::with_capacity(files.len());
    for file in files {
        reject_symlink_path(root, &file.relative_path)?;
        let target = root.join(&file.relative_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        reject_symlink_path(root, &file.relative_path)?;
        let temporary = target.with_extension("protocol-codegen.new");
        let backup = target.with_extension("protocol-codegen.previous");
        prepare_auxiliary_path(&temporary)?;
        prepare_auxiliary_path(&backup)?;
        std::fs::write(&temporary, file.contents.as_bytes())
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        staged.push((target, temporary, backup));
    }

    let mut replaced = Vec::with_capacity(staged.len());
    for (index, (target, temporary, backup)) in staged.iter().enumerate() {
        if fail_before_replace == Some(index) {
            rollback_generated(&replaced, &staged);
            return Err(format!("injected replacement failure at index {index}"));
        }
        let had_existing = match std::fs::symlink_metadata(target) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                rollback_generated(&replaced, &staged);
                return Err(format!(
                    "refusing generated target symlink: {}",
                    target.display()
                ));
            }
            Ok(_) => {
                if let Err(error) = std::fs::rename(target, backup) {
                    rollback_generated(&replaced, &staged);
                    return Err(format!(
                        "stage existing {} for replacement: {error}",
                        target.display()
                    ));
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                rollback_generated(&replaced, &staged);
                return Err(format!("inspect {}: {error}", target.display()));
            }
        };
        if let Err(error) = std::fs::rename(temporary, target) {
            if had_existing {
                let _ = std::fs::rename(backup, target);
            }
            rollback_generated(&replaced, &staged);
            return Err(format!("replace {}: {error}", target.display()));
        }
        replaced.push((target.clone(), backup.clone(), had_existing));
    }
    for (_, backup, had_existing) in &replaced {
        if *had_existing {
            std::fs::remove_file(backup).map_err(|error| {
                format!("remove committed backup {}: {error}", backup.display())
            })?;
        }
    }
    Ok(())
}

fn prepare_auxiliary_path(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing generated auxiliary symlink: {}",
            path.display()
        )),
        Ok(_) => std::fs::remove_file(path)
            .map_err(|error| format!("remove stale auxiliary {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect auxiliary {}: {error}", path.display())),
    }
}

fn rollback_generated(
    replaced: &[(PathBuf, PathBuf, bool)],
    staged: &[(PathBuf, PathBuf, PathBuf)],
) {
    for (target, backup, had_existing) in replaced.iter().rev() {
        let _ = std::fs::remove_file(target);
        if *had_existing {
            let _ = std::fs::rename(backup, target);
        }
    }
    for (_, temporary, backup) in staged {
        let _ = std::fs::remove_file(temporary);
        let _ = std::fs::remove_file(backup);
    }
}

struct GenerationLock {
    path: PathBuf,
}

impl GenerationLock {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join(".protocol-codegen.lock");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        use std::io::Write as _;
        let mut file = options
            .open(&path)
            .map_err(|error| format!("acquire generator lock {}: {error}", path.display()))?;
        writeln!(file, "pid={}", std::process::id())
            .map_err(|error| format!("write generator lock {}: {error}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for GenerationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn check_generated(root: &Path, files: &[GeneratedFile]) -> Result<(), Vec<PathBuf>> {
    let mut stale = Vec::new();
    for file in files {
        if reject_symlink_path(root, &file.relative_path).is_err() {
            stale.push(file.relative_path.clone());
            continue;
        }
        let target = root.join(&file.relative_path);
        match std::fs::read(&target) {
            Ok(existing) if existing == file.contents.as_bytes() => {}
            _ => stale.push(file.relative_path.clone()),
        }
    }
    if stale.is_empty() { Ok(()) } else { Err(stale) }
}

pub fn reject_symlink_path(root: &Path, relative: &Path) -> Result<(), String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "generated path must be a fixed relative path: {}",
            relative.display()
        ));
    }
    if std::fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(format!(
            "repository root must not be a symlink: {}",
            root.display()
        ));
    }
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            unreachable!("component shape checked above");
        };
        candidate.push(component);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing symlink in generated path: {}",
                    candidate.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect {}: {error}", candidate.display())),
        }
    }
    Ok(())
}

fn generate_rust(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "// @generated by pdf-rs-protocol-codegen; DO NOT EDIT."
    )
    .unwrap();
    writeln!(out, "// Source: protocol/engine.protocol").unwrap();
    writeln!(
        out,
        "// Generated declarations are documented by the canonical schema and registries."
    )
    .unwrap();
    writeln!(out, "#![allow(missing_docs)]\n").unwrap();
    writeln!(out, "pub const PROTOCOL_MAJOR: u16 = {};", protocol.major).unwrap();
    writeln!(out, "pub const PROTOCOL_MINOR: u16 = {};", protocol.minor).unwrap();
    writeln!(
        out,
        "pub const PROTOCOL_GENERATOR_VERSION: &str = \"{GENERATOR_VERSION}\";"
    )
    .unwrap();
    writeln!(
        out,
        "pub const WIRE_IDENTITY_DOMAIN: &str = \"{WIRE_IDENTITY_DOMAIN}\";"
    )
    .unwrap();
    writeln!(
        out,
        "pub const PAYLOAD_CODEC_ABI_VERSION: u16 = {PAYLOAD_CODEC_ABI_VERSION};"
    )
    .unwrap();
    writeln!(
        out,
        "pub const MIN_COMPATIBLE_MINOR: u16 = {};",
        protocol.minor
    )
    .unwrap();
    writeln!(
        out,
        "pub const MAX_MESSAGE_BYTES: u32 = {};",
        protocol.max_message_bytes
    )
    .unwrap();
    writeln!(
        out,
        "pub const MAX_TRANSFER_SLOTS: u16 = {};",
        protocol.max_transfer_slots
    )
    .unwrap();
    writeln!(
        out,
        "pub const MAX_DATA_SEGMENT_BYTES: u64 = {};",
        protocol.max_data_segment_bytes
    )
    .unwrap();
    writeln!(
        out,
        "pub const MAX_DATA_TICKET_BYTES: u64 = {};",
        protocol.max_data_ticket_bytes
    )
    .unwrap();
    writeln!(
        out,
        "pub const PAYLOAD_CODEC: &str = \"{}\";",
        protocol.payload_codec
    )
    .unwrap();
    writeln!(
        out,
        "pub const CAPABILITY_DECISION_HASH_DOMAIN: &str = \"{CAPABILITY_DECISION_HASH_DOMAIN}\";"
    )
    .unwrap();
    writeln!(
        out,
        "pub const RENDER_PLAN_MANIFEST_HASH_DOMAIN: &str = \"{RENDER_PLAN_MANIFEST_HASH_DOMAIN}\";"
    )
    .unwrap();
    writeln!(
        out,
        "pub const SCHEMA_SHA256: [u8; 32] = {};",
        rust_bytes(digest)
    )
    .unwrap();
    writeln!(
        out,
        "pub const SCHEMA_SHA256_HEX: &str = \"{}\";",
        lowercase_hex(digest)
    )
    .unwrap();
    writeln!(
        out,
        "pub const SCHEMA_HASH: [u8; 16] = {};",
        rust_bytes(&digest[..16])
    )
    .unwrap();
    writeln!(
        out,
        "pub const SCHEMA_HASH_TRUNCATION: &str = \"{SCHEMA_HASH_TRUNCATION}\";"
    )
    .unwrap();
    writeln!(
        out,
        "pub const DESKTOP_BYTE_ORDER: &str = \"little-endian\";"
    )
    .unwrap();
    writeln!(out, "pub const ENVELOPE_HEADER_BYTES: usize = 20;\n").unwrap();
    for record in &protocol.records {
        for field in &record.fields {
            if let Type::List(_, limit) | Type::Bytes(limit) = &field.ty {
                writeln!(
                    out,
                    "pub const {}_{}_MAX_COUNT: usize = {};",
                    screaming_snake_case(&record.name),
                    screaming_snake_case(&field.name),
                    limit
                )
                .unwrap();
            }
        }
    }
    out.push('\n');

    for scalar in &protocol.scalars {
        let rust = rust_primitive(scalar.primitive);
        let redact_debug = matches!(
            scalar.name.as_str(),
            "PlatformHandle" | "SceneHash" | "CapabilityDecisionHash"
        );
        if redact_debug {
            writeln!(
                out,
                "#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]\npub struct {}({rust});",
                scalar.name
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]\npub struct {}({rust});",
                scalar.name
            )
            .unwrap();
        }
        if matches!(scalar.primitive, Primitive::Bytes16 | Primitive::Bytes32) {
            writeln!(
                out,
                "impl {} {{\n    pub const fn new(value: {rust}) -> Self {{ Self(value) }}\n    pub const fn digest(&self) -> &{rust} {{ &self.0 }}\n    pub const fn into_digest(self) -> {rust} {{ self.0 }}\n}}\n",
                scalar.name
            )
            .unwrap();
        } else {
            writeln!(
                out,
                "impl {} {{\n    pub const fn new(value: {rust}) -> Self {{ Self(value) }}\n    pub const fn value(self) -> {rust} {{ self.0 }}\n}}\n",
                scalar.name
            )
            .unwrap();
        }
        if redact_debug {
            writeln!(
                out,
                "impl core::fmt::Debug for {} {{\n    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n        formatter.write_str(\"{}([REDACTED])\")\n    }}\n}}\n",
                scalar.name, scalar.name
            )
            .unwrap();
        }
    }

    for enumeration in &protocol.enums {
        write_rust_enum(&mut out, enumeration);
    }
    write_endpoint_capability_constants(&mut out, protocol);
    write_engine_execution_capability_constants(&mut out, protocol);

    for record in &protocol.records {
        let has_redacted_field = record
            .fields
            .iter()
            .any(|field| field.privacy != Privacy::Public);
        if has_redacted_field {
            writeln!(out, "#[derive(Clone, Eq, PartialEq)]").unwrap();
        } else {
            writeln!(out, "#[derive(Clone, Debug, Eq, PartialEq)]").unwrap();
        }
        writeln!(out, "pub struct {} {{", record.name).unwrap();
        for field in &record.fields {
            writeln!(out, "    pub {}: {},", field.name, rust_type(&field.ty)).unwrap();
        }
        writeln!(out, "}}\n").unwrap();
        if has_redacted_field {
            writeln!(
                out,
                "impl core::fmt::Debug for {} {{\n    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n        let mut output = formatter.debug_struct(\"{}\");",
                record.name, record.name
            )
            .unwrap();
            for field in &record.fields {
                if field.privacy == Privacy::Public {
                    writeln!(
                        out,
                        "        output.field(\"{}\", &self.{});",
                        field.name, field.name
                    )
                    .unwrap();
                } else {
                    writeln!(
                        out,
                        "        output.field(\"{}\", &\"[REDACTED]\");",
                        field.name
                    )
                    .unwrap();
                }
            }
            writeln!(out, "        output.finish()\n    }}\n}}\n").unwrap();
        }
    }
    write_rust_engine_error_descriptors(&mut out, protocol);

    for union in &protocol.unions {
        let has_redacted_field = union
            .variants
            .iter()
            .flat_map(|variant| &variant.fields)
            .any(|field| field.privacy != Privacy::Public);
        if has_redacted_field {
            writeln!(out, "#[derive(Clone, Eq, PartialEq)]").unwrap();
        } else {
            writeln!(out, "#[derive(Clone, Debug, Eq, PartialEq)]").unwrap();
        }
        writeln!(out, "pub enum {} {{", union.name).unwrap();
        for variant in &union.variants {
            if variant.fields.is_empty() {
                writeln!(out, "    {},", variant.name).unwrap();
            } else {
                writeln!(out, "    {} {{", variant.name).unwrap();
                for field in &variant.fields {
                    writeln!(out, "        {}: {},", field.name, rust_type(&field.ty)).unwrap();
                }
                writeln!(out, "    }},").unwrap();
            }
        }
        writeln!(out, "}}\n").unwrap();
        if has_redacted_field {
            writeln!(
                out,
                "impl core::fmt::Debug for {} {{\n    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{\n        formatter.write_str(\"{}([REDACTED])\")\n    }}\n}}\n",
                union.name, union.name
            )
            .unwrap();
        }
    }

    write_rust_union_capability_requirements(&mut out, protocol);
    write_message_enums(&mut out, protocol);
    write_descriptors(&mut out, protocol);
    write_capability_decision_invariants(&mut out);
    out.push_str(&generate_rust_payload_codec(protocol));
    finish_generated_text(out)
}

fn engine_error_policies() -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    &[
        ("InvalidDocument", "Document", "Fatal", "ReopenSession"),
        ("SourceChanged", "Source", "Recoverable", "ReopenSession"),
        ("SourceUnavailable", "Source", "Recoverable", "RetryRequest"),
        ("InvalidPassword", "Document", "Recoverable", "RetryRequest"),
        (
            "UnsupportedFeature",
            "Capability",
            "Recoverable",
            "RetryNativeRenderer",
        ),
        (
            "ResourceLimit",
            "Resource",
            "Recoverable",
            "RetryNativeRenderer",
        ),
        ("Cancelled", "Cancelled", "Info", "None"),
        ("StaleGeneration", "Cancelled", "Info", "None"),
        (
            "SurfaceImportFailed",
            "Resource",
            "Recoverable",
            "RetryRequest",
        ),
        ("Internal", "Internal", "Fatal", "RestartWorker"),
        ("ProtocolViolation", "Protocol", "Fatal", "RestartWorker"),
    ]
}

fn write_rust_engine_error_descriptors(out: &mut String, protocol: &Protocol) {
    if !protocol
        .records
        .iter()
        .any(|record| record.name == "EngineError")
    {
        return;
    }
    out.push_str(
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct EngineErrorDescriptor {\n\
    pub code: EngineErrorCode,\n\
    pub category: ErrorCategory,\n\
    pub severity: ErrorSeverity,\n\
    pub recoverability: ErrorRecoverability,\n\
}\n\n\
pub const ENGINE_ERROR_DESCRIPTORS: &[EngineErrorDescriptor] = &[\n",
    );
    for (code, category, severity, recoverability) in engine_error_policies() {
        writeln!(
            out,
            "    EngineErrorDescriptor {{ code: EngineErrorCode::{code}, category: ErrorCategory::{category}, severity: ErrorSeverity::{severity}, recoverability: ErrorRecoverability::{recoverability} }},"
        )
        .unwrap();
    }
    out.push_str(
        "];\n\n\
impl EngineError {\n\
    pub fn wire_invariants_valid(&self) -> bool {\n\
        self.diagnostic_id.value() != 0\n\
            && ENGINE_ERROR_DESCRIPTORS.iter().any(|descriptor| descriptor.code == self.code\n\
                && descriptor.category == self.category\n\
                && descriptor.severity == self.severity\n\
                && descriptor.recoverability == self.recoverability)\n\
    }\n\
}\n\n",
    );
}

fn write_rust_union_capability_requirements(out: &mut String, protocol: &Protocol) {
    out.push_str(
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct UnionVariantCapabilityRequirement {\n\
    pub union_name: &'static str,\n\
    pub variant_name: &'static str,\n\
    pub capability: u64,\n\
}\n\n\
pub const UNION_VARIANT_CAPABILITY_REQUIREMENTS: &[UnionVariantCapabilityRequirement] = &[\n",
    );
    for union in &protocol.unions {
        for variant in &union.variants {
            if let Some(capability) = &variant.required_capability {
                writeln!(
                    out,
                    "    UnionVariantCapabilityRequirement {{ union_name: \"{}\", variant_name: \"{}\", capability: {} }},",
                    union.name,
                    variant.name,
                    rust_required_capability(Some(capability))
                )
                .unwrap();
            }
        }
    }
    out.push_str("];\n\n");
    for union in &protocol.unions {
        writeln!(
            out,
            "pub const fn {}_required_capability(value: &{}) -> u64 {{\n    match value {{",
            snake_case(&union.name),
            union.name
        )
        .unwrap();
        for variant in &union.variants {
            let pattern = if variant.fields.is_empty() {
                format!("{}::{}", union.name, variant.name)
            } else {
                format!("{}::{} {{ .. }}", union.name, variant.name)
            };
            writeln!(
                out,
                "        {pattern} => {},",
                rust_required_capability(variant.required_capability.as_deref())
            )
            .unwrap();
        }
        out.push_str("    }\n}\n\n");
    }
}

fn write_rust_enum(out: &mut String, enumeration: &EnumDef) {
    writeln!(
        out,
        "#[repr({})]\n#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]",
        rust_primitive(enumeration.repr)
    )
    .unwrap();
    writeln!(out, "pub enum {} {{", enumeration.name).unwrap();
    for variant in &enumeration.variants {
        writeln!(out, "    {} = {},", variant.name, variant.tag).unwrap();
    }
    writeln!(out, "}}\n").unwrap();
}

fn write_endpoint_capability_constants(out: &mut String, protocol: &Protocol) {
    let Some(capabilities) = protocol
        .enums
        .iter()
        .find(|value| value.name == "EndpointCapability")
    else {
        return;
    };
    let mut expression = Vec::new();
    for variant in &capabilities.variants {
        let constant = format!(
            "ENDPOINT_CAPABILITY_{}",
            screaming_snake_case(&variant.name)
        );
        writeln!(
            out,
            "pub const {constant}: u64 = EndpointCapability::{} as u64;",
            variant.name
        )
        .unwrap();
        expression.push(constant);
    }
    writeln!(
        out,
        "pub const KNOWN_ENDPOINT_CAPABILITIES: u64 = {};",
        expression.join(" | ")
    )
    .unwrap();
    out.push('\n');
}

fn write_engine_execution_capability_constants(out: &mut String, protocol: &Protocol) {
    let Some(capabilities) = protocol
        .enums
        .iter()
        .find(|value| value.name == "EngineExecutionCapability")
    else {
        return;
    };
    let mut expression = Vec::new();
    for variant in &capabilities.variants {
        let constant = format!(
            "ENGINE_EXECUTION_CAPABILITY_{}",
            screaming_snake_case(&variant.name)
        );
        writeln!(
            out,
            "pub const {constant}: u64 = EngineExecutionCapability::{} as u64;",
            variant.name
        )
        .unwrap();
        expression.push(constant);
    }
    writeln!(
        out,
        "pub const KNOWN_ENGINE_EXECUTION_CAPABILITIES: u64 = {};\n",
        expression.join(" | ")
    )
    .unwrap();
}

fn write_message_enums(out: &mut String, protocol: &Protocol) {
    for (kind, name) in [
        (MessageKind::Command, "Command"),
        (MessageKind::Event, "Event"),
    ] {
        writeln!(out, "#[derive(Clone, Debug, Eq, PartialEq)]").unwrap();
        writeln!(out, "pub enum {name} {{").unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(out, "    {}({}),", message.name, message.payload).unwrap();
        }
        writeln!(out, "}}\n").unwrap();
    }
    writeln!(
        out,
        "#[derive(Clone, Debug, Eq, PartialEq)]\npub struct CommandEnvelope {{\n    pub header: EnvelopeHeader,\n    pub correlation: Correlation,\n    pub command: Command,\n}}\n"
    )
    .unwrap();
    writeln!(
        out,
        "#[derive(Clone, Debug, Eq, PartialEq)]\npub struct EventEnvelope {{\n    pub header: EnvelopeHeader,\n    pub correlation: Correlation,\n    pub event: Event,\n}}\n"
    )
    .unwrap();
}

fn write_descriptors(out: &mut String, protocol: &Protocol) {
    let codec = FixedLeCodec::new(
        protocol,
        CodecLimits::new(
            64,
            protocol.max_message_bytes as usize,
            protocol.max_message_bytes as usize,
        ),
    )
    .expect("parser validates fixed_le_v1 schema");
    for message in &protocol.messages {
        writeln!(
            out,
            "pub const MESSAGE_ID_{}: u16 = {};",
            screaming_snake_case(&message.name),
            message.id
        )
        .unwrap();
    }
    out.push('\n');
    out.push_str(
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub enum MessageKind { Command, Event }\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub enum CorrelationRequirement { Required, Optional, Forbidden }\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct CorrelationShape {\n\
    pub worker: CorrelationRequirement,\n\
    pub session: CorrelationRequirement,\n\
    pub request: CorrelationRequirement,\n\
    pub generation: CorrelationRequirement,\n\
}\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub enum FieldPrivacy { Public, Private, Sensitive }\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub enum OutcomeDisposition { Stream, Terminal }\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct OutcomeDescriptor {\n\
    pub event_id: u16,\n\
    pub disposition: OutcomeDisposition,\n\
}\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct FieldDescriptor {\n\
    pub name: &'static str,\n\
    pub wire_type: &'static str,\n\
    pub required: bool,\n\
    pub privacy: FieldPrivacy,\n\
    pub max_count: u32,\n\
}\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct TypeFieldDescriptor {\n\
    pub owner: &'static str,\n\
    pub variant: Option<&'static str>,\n\
    pub name: &'static str,\n\
    pub wire_type: &'static str,\n\
    pub required: bool,\n\
    pub privacy: FieldPrivacy,\n\
    pub max_count: u32,\n\
}\n\n\
#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n\
pub struct MessageDescriptor {\n\
    pub kind: MessageKind,\n\
    pub name: &'static str,\n\
    pub id: u16,\n\
    pub payload: &'static str,\n\
    pub state_precondition: &'static str,\n\
    pub correlation: &'static str,\n\
    pub correlation_shape: CorrelationShape,\n\
    pub replayable: bool,\n\
    pub allowed_flags: u16,\n\
    pub min_transfer_slots: u16,\n\
    pub max_transfer_slots: u16,\n\
    pub max_payload_bytes: u32,\n\
    pub maximum_encoded_payload_bytes: u32,\n\
    pub required_capability: u64,\n\
    pub fields: &'static [FieldDescriptor],\n\
    pub outcomes: &'static [OutcomeDescriptor],\n\
}\n\n",
    );
    let states = protocol
        .messages
        .iter()
        .map(|message| format!("\"{}\"", message.state))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "pub const STATE_PRECONDITIONS: &[&str] = &[{states}];\n"
    )
    .unwrap();

    out.push_str("pub const TYPE_FIELD_DESCRIPTORS: &[TypeFieldDescriptor] = &[\n");
    for record in &protocol.records {
        for field in &record.fields {
            writeln!(
                out,
                "    TypeFieldDescriptor {{ owner: \"{}\", variant: None, name: \"{}\", wire_type: \"{}\", required: {}, privacy: FieldPrivacy::{}, max_count: {} }},",
                record.name,
                field.name,
                field.ty.schema_name(),
                field.presence == Presence::Required,
                rust_privacy(field.privacy),
                type_limit(&field.ty)
            )
            .unwrap();
        }
    }
    for union in &protocol.unions {
        for variant in &union.variants {
            for field in &variant.fields {
                writeln!(
                    out,
                    "    TypeFieldDescriptor {{ owner: \"{}\", variant: Some(\"{}\"), name: \"{}\", wire_type: \"{}\", required: true, privacy: FieldPrivacy::{}, max_count: {} }},",
                    union.name,
                    variant.name,
                    field.name,
                    field.ty.schema_name(),
                    rust_privacy(field.privacy),
                    type_limit(&field.ty)
                )
                .unwrap();
            }
        }
    }
    out.push_str("];\n\n");

    for message in &protocol.messages {
        let record = protocol
            .records
            .iter()
            .find(|record| record.name == message.payload)
            .expect("parser validates payload type");
        writeln!(
            out,
            "const {}_FIELDS: &[FieldDescriptor] = &[",
            screaming_snake_case(&message.name)
        )
        .unwrap();
        for field in &record.fields {
            writeln!(
                out,
                "    FieldDescriptor {{ name: \"{}\", wire_type: \"{}\", required: {}, privacy: FieldPrivacy::{}, max_count: {} }},",
                field.name,
                field.ty.schema_name(),
                field.presence == Presence::Required,
                rust_privacy(field.privacy),
                type_limit(&field.ty)
            )
            .unwrap();
        }
        writeln!(out, "];").unwrap();
        if message.kind == MessageKind::Command {
            let outcomes = message
                .outcomes
                .iter()
                .map(|outcome| {
                    let id = protocol
                        .messages
                        .iter()
                        .find(|candidate| {
                            candidate.kind == MessageKind::Event
                                && candidate.name == outcome.name
                        })
                        .expect("parser validates outcomes")
                        .id;
                    format!(
                        "OutcomeDescriptor {{ event_id: {id}, disposition: OutcomeDisposition::{} }}",
                        match outcome.disposition {
                            crate::model::OutcomeDisposition::Stream => "Stream",
                            crate::model::OutcomeDisposition::Terminal => "Terminal",
                        }
                    )
                })
                .collect::<Vec<_>>();
            writeln!(
                out,
                "const {}_OUTCOMES: &[OutcomeDescriptor] = &[{}];",
                screaming_snake_case(&message.name),
                outcomes.join(", ")
            )
            .unwrap();
        }
    }
    out.push('\n');

    for (kind, constant) in [
        (MessageKind::Command, "COMMAND_DESCRIPTORS"),
        (MessageKind::Event, "EVENT_DESCRIPTORS"),
    ] {
        writeln!(out, "pub const {constant}: &[MessageDescriptor] = &[").unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            let prefix = screaming_snake_case(&message.name);
            let outcomes = if kind == MessageKind::Command {
                format!("{prefix}_OUTCOMES")
            } else {
                "&[]".into()
            };
            let (worker, session, request, generation) = correlation_shape(&message.correlation);
            let required_capability =
                rust_required_capability(message.required_capability.as_deref());
            let maximum_encoded_payload_bytes: u32 = maximum_message_payload(&codec, message)
                .try_into()
                .expect("payload maximum fits u32");
            writeln!(
                out,
                "    MessageDescriptor {{ kind: MessageKind::{}, name: \"{}\", id: {}, payload: \"{}\", state_precondition: \"{}\", correlation: \"{}\", correlation_shape: CorrelationShape {{ worker: CorrelationRequirement::{worker}, session: CorrelationRequirement::{session}, request: CorrelationRequirement::{request}, generation: CorrelationRequirement::{generation} }}, replayable: {}, allowed_flags: {}, min_transfer_slots: {}, max_transfer_slots: {}, max_payload_bytes: {}, maximum_encoded_payload_bytes: {maximum_encoded_payload_bytes}, required_capability: {required_capability}, fields: {prefix}_FIELDS, outcomes: {outcomes} }},",
                if kind == MessageKind::Command { "Command" } else { "Event" },
                message.name,
                message.id,
                message.payload,
                message.state,
                message.correlation,
                message.disposition == "yes",
                message.allowed_flags,
                message.min_transfer_slots,
                message.max_transfer_slots,
                message.max_payload_bytes
            )
            .unwrap();
        }
        writeln!(out, "];\n").unwrap();
    }
    out.push_str(
        "pub fn descriptor_by_id(id: u16) -> Option<&'static MessageDescriptor> {\n\
    COMMAND_DESCRIPTORS.iter().chain(EVENT_DESCRIPTORS.iter()).find(|value| value.id == id)\n\
}\n\n",
    );
}

fn write_capability_decision_invariants(out: &mut String) {
    out.push_str(
        "impl CapabilityDecision {\n\
    /// Checks bounded-list accounting and status-dependent wire invariants.\n\
    pub fn wire_invariants_valid(&self) -> bool {\n\
        let missing_len = u32::try_from(self.missing.len()).ok();\n\
        let contributors_len = u32::try_from(self.contributors.len()).ok();\n\
        let missing_accounted = match self.missing_completeness {\n\
            CollectionCompleteness::Complete => missing_len == Some(self.missing_total),\n\
            CollectionCompleteness::Truncated => missing_len.is_some_and(|len| len < self.missing_total),\n\
        };\n\
        let contributors_accounted = match self.contributors_completeness {\n\
            CollectionCompleteness::Complete => contributors_len == Some(self.contributors_total),\n\
            CollectionCompleteness::Truncated => contributors_len.is_some_and(|len| len < self.contributors_total),\n\
        };\n\
        let contributor_ids: std::collections::BTreeSet<u32> = self.contributors.iter().map(|value| value.id).collect();\n\
        let requirement_ids: std::collections::BTreeSet<u32> = self.missing.iter().map(|value| value.id).collect();\n\
        let bounded_and_canonical = self.missing.len() <= CAPABILITY_DECISION_MISSING_MAX_COUNT\n\
            && self.contributors.len() <= CAPABILITY_DECISION_CONTRIBUTORS_MAX_COUNT\n\
            && self.missing.windows(2).all(|pair| pair[0].id < pair[1].id)\n\
            && self.contributors.windows(2).all(|pair| pair[0].id < pair[1].id)\n\
            && requirement_ids.len() == self.missing.len()\n\
            && contributor_ids.len() == self.contributors.len()\n\
            && self.missing.iter().all(|requirement| requirement.id != 0\n\
                && requirement.dependencies.len() <= CAPABILITY_REQUIREMENT_DEPENDENCIES_MAX_COUNT\n\
                && requirement.contributor_ids.len() <= CAPABILITY_REQUIREMENT_CONTRIBUTOR_IDS_MAX_COUNT\n\
                && requirement.dependencies.windows(2).all(|pair| pair[0] < pair[1])\n\
                && requirement.contributor_ids.windows(2).all(|pair| pair[0] < pair[1])\n\
                && requirement.dependencies.iter().all(|id| *id != requirement.id && requirement_ids.contains(id))\n\
                && requirement.contributor_ids.iter().all(|id| contributor_ids.contains(id)))\n\
            && self.contributors.iter().all(|contributor| contributor.id != 0);\n\
        let status_valid = match self.status {\n\
            SupportStatus::Supported => self.missing_total == 0\n\
                && self.missing.is_empty()\n\
                && self.location.is_none()\n\
                && self.rejection_code.is_none(),\n\
            SupportStatus::Unsupported => self.rejection_code.is_none(),\n\
            SupportStatus::Rejected => self.rejection_code.is_some(),\n\
        };\n\
        missing_accounted && contributors_accounted && bounded_and_canonical && status_valid\n\
    }\n\
}\n",
    );
}

fn rust_primitive(value: Primitive) -> &'static str {
    match value {
        Primitive::U8 => "u8",
        Primitive::U16 => "u16",
        Primitive::U32 => "u32",
        Primitive::U64 => "u64",
        Primitive::I32 => "i32",
        Primitive::Bool => "bool",
        Primitive::Bytes16 => "[u8; 16]",
        Primitive::Bytes32 => "[u8; 32]",
    }
}

fn rust_type(value: &Type) -> String {
    match value {
        Type::Primitive(value) => rust_primitive(*value).into(),
        Type::Named(value) => value.clone(),
        Type::Optional(inner) => format!("Option<{}>", rust_type(inner)),
        Type::List(inner, _) => format!("Vec<{}>", rust_type(inner)),
        Type::Bytes(_) => "Vec<u8>".into(),
    }
}

fn rust_privacy(value: Privacy) -> &'static str {
    match value {
        Privacy::Public => "Public",
        Privacy::Private => "Private",
        Privacy::Sensitive => "Sensitive",
    }
}

fn ts_privacy(value: Privacy) -> &'static str {
    match value {
        Privacy::Public => "public",
        Privacy::Private => "private",
        Privacy::Sensitive => "sensitive",
    }
}

fn rust_required_capability(value: Option<&str>) -> String {
    value.map_or_else(
        || "0".into(),
        |capability| format!("ENDPOINT_CAPABILITY_{}", screaming_snake_case(capability)),
    )
}

fn ts_required_capability(value: Option<&str>) -> String {
    value.map_or_else(
        || "0n".into(),
        |capability| format!("ENDPOINT_CAPABILITY_{}", screaming_snake_case(capability)),
    )
}

fn type_limit(value: &Type) -> u32 {
    match value {
        Type::List(_, limit) | Type::Bytes(limit) => *limit,
        Type::Optional(inner) => type_limit(inner),
        _ => 0,
    }
}

fn correlation_shape(value: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match value {
        "Worker" => ("Required", "Forbidden", "Forbidden", "Forbidden"),
        "Session" => ("Required", "Required", "Forbidden", "Forbidden"),
        "Request" => ("Required", "Optional", "Required", "Forbidden"),
        "OpenRequest" => ("Required", "Forbidden", "Required", "Forbidden"),
        "SessionRequest" => ("Required", "Required", "Required", "Forbidden"),
        "Generation" => ("Required", "Required", "Forbidden", "Required"),
        other => panic!("unknown validated correlation shape {other}"),
    }
}

fn rust_bytes(bytes: &[u8]) -> String {
    let values = bytes
        .iter()
        .map(|value| format!("0x{value:02x}"))
        .collect::<Vec<_>>();
    format!("[{}]", values.join(", "))
}

fn screaming_snake_case(value: &str) -> String {
    let mut output = String::new();
    for (index, byte) in value.bytes().enumerate() {
        if byte.is_ascii_uppercase() && index != 0 {
            output.push('_');
        }
        output.push((byte as char).to_ascii_uppercase());
    }
    output
}

fn snake_case(value: &str) -> String {
    screaming_snake_case(value).to_ascii_lowercase()
}

fn lower_camel_case(value: &str) -> String {
    let mut bytes = value.as_bytes().to_vec();
    if let Some(first) = bytes.first_mut() {
        first.make_ascii_lowercase();
    }
    String::from_utf8(bytes).expect("validated schema names are ASCII")
}

fn browser_union_variant(variant: &crate::model::UnionVariant) -> bool {
    !matches!(
        variant.required_capability.as_deref(),
        Some("SharedMemory" | "LocalMemory")
    )
}

fn desktop_union_variant(variant: &crate::model::UnionVariant) -> bool {
    matches!(
        variant.required_capability.as_deref(),
        Some("SharedMemory" | "LocalMemory")
    )
}

fn generate_typescript(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "// @generated by pdf-rs-protocol-codegen; DO NOT EDIT."
    )
    .unwrap();
    writeln!(
        out,
        "export const PROTOCOL_MAJOR = {} as const;",
        protocol.major
    )
    .unwrap();
    writeln!(
        out,
        "export const PROTOCOL_MINOR = {} as const;",
        protocol.minor
    )
    .unwrap();
    writeln!(
        out,
        "export const PROTOCOL_GENERATOR_VERSION = \"{GENERATOR_VERSION}\" as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const WIRE_IDENTITY_DOMAIN = \"{WIRE_IDENTITY_DOMAIN}\" as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const PAYLOAD_CODEC_ABI_VERSION = {PAYLOAD_CODEC_ABI_VERSION} as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const MIN_COMPATIBLE_MINOR = {} as const;",
        protocol.minor
    )
    .unwrap();
    writeln!(
        out,
        "export const MAX_MESSAGE_BYTES = {} as const;",
        protocol.max_message_bytes
    )
    .unwrap();
    writeln!(
        out,
        "export const MAX_TRANSFER_SLOTS = {} as const;",
        protocol.max_transfer_slots
    )
    .unwrap();
    writeln!(
        out,
        "export const MAX_DATA_SEGMENT_BYTES = {}n as const;",
        protocol.max_data_segment_bytes
    )
    .unwrap();
    writeln!(
        out,
        "export const MAX_DATA_TICKET_BYTES = {}n as const;",
        protocol.max_data_ticket_bytes
    )
    .unwrap();
    writeln!(
        out,
        "export const PAYLOAD_CODEC = \"{}\" as const;",
        protocol.payload_codec
    )
    .unwrap();
    writeln!(
        out,
        "export const CAPABILITY_DECISION_HASH_DOMAIN = \"{CAPABILITY_DECISION_HASH_DOMAIN}\" as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const RENDER_PLAN_MANIFEST_HASH_DOMAIN = \"{RENDER_PLAN_MANIFEST_HASH_DOMAIN}\" as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const SCHEMA_SHA256_HEX = \"{}\" as const;",
        lowercase_hex(digest)
    )
    .unwrap();
    writeln!(
        out,
        "export const SCHEMA_HASH_HEX = \"{}\" as const;",
        lowercase_hex(&digest[..16])
    )
    .unwrap();
    writeln!(
        out,
        "export const SCHEMA_HASH = Uint8Array.of({}) as Uint8Array;",
        digest[..16]
            .iter()
            .map(|value| format!("0x{value:02x}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
    .unwrap();
    writeln!(
        out,
        "export const SCHEMA_HASH_TRUNCATION = \"{SCHEMA_HASH_TRUNCATION}\" as const;\n"
    )
    .unwrap();
    out.push_str(
        "// Validators reject unknown payload keys. Encoders must target the negotiated minor;\n\
// compatibility is never implemented by silently accepting an unversioned extension field.\n\
export const UNKNOWN_PAYLOAD_FIELD_POLICY = \"reject\" as const;\n\n",
    );
    out.push_str("export const TARGET_PROTOCOL_PROJECTION = \"browser\" as const;\n\n");
    for record in &protocol.records {
        for field in &record.fields {
            if let Type::List(_, limit) | Type::Bytes(limit) = &field.ty {
                writeln!(
                    out,
                    "export const {}_{}_MAX_COUNT = {} as const;",
                    screaming_snake_case(&record.name),
                    screaming_snake_case(&field.name),
                    limit
                )
                .unwrap();
            }
        }
    }
    out.push('\n');
    write_ts_helpers(&mut out);

    for scalar in &protocol.scalars {
        writeln!(
            out,
            "export type {} = {};\nexport const validate{} = (value: unknown): value is {} => {};\n",
            scalar.name,
            ts_primitive(scalar.primitive),
            scalar.name,
            scalar.name,
            ts_primitive_validator(scalar.primitive, "value")
        )
        .unwrap();
    }
    for enumeration in &protocol.enums {
        write_ts_enum(&mut out, enumeration);
    }
    write_ts_endpoint_capabilities(&mut out, protocol);
    write_ts_engine_execution_capabilities(&mut out, protocol);
    write_ts_engine_error_descriptors(&mut out, protocol);
    write_ts_descriptors(&mut out, protocol);
    for record in &protocol.records {
        write_ts_record(&mut out, record);
    }
    for union in &protocol.unions {
        writeln!(out, "export type {} =", union.name).unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            write!(out, "  | {{ kind: \"{}\"", variant.name).unwrap();
            for field in &variant.fields {
                write!(out, "; {}: {}", field.name, ts_type(&field.ty)).unwrap();
            }
            writeln!(out, " }}").unwrap();
        }
        writeln!(out, ";\n").unwrap();
        writeln!(
            out,
            "export function validate{}(value: unknown): value is {} {{",
            union.name, union.name
        )
        .unwrap();
        writeln!(
            out,
            "  if (!isRecord(value) || typeof value.kind !== \"string\") return false;"
        )
        .unwrap();
        writeln!(out, "  switch (value.kind) {{").unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            let required = std::iter::once("kind")
                .chain(variant.fields.iter().map(|field| field.name.as_str()))
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", ");
            write!(
                out,
                "    case \"{}\": return exactKeys(value, [{required}], [])",
                variant.name
            )
            .unwrap();
            for field in &variant.fields {
                write!(
                    out,
                    " && {}",
                    ts_validator(&field.ty, &format!("value.{}", field.name))
                )
                .unwrap();
            }
            writeln!(out, ";").unwrap();
        }
        writeln!(out, "    default: return false;\n  }}\n}}\n").unwrap();
    }
    write_ts_union_capability_requirements(&mut out, protocol);
    write_ts_redaction_and_snapshots(&mut out, protocol);
    write_ts_messages(&mut out, protocol);
    out.push_str(&generate_typescript_payload_codec(protocol));
    finish_generated_text(out)
}

fn finish_generated_text(mut output: String) -> String {
    while output.ends_with("\n\n") {
        output.pop();
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn write_ts_redaction_and_snapshots(out: &mut String, protocol: &Protocol) {
    for record in &protocol.records {
        let redact_value_name = if record
            .fields
            .iter()
            .any(|field| field.privacy == Privacy::Public)
        {
            "value"
        } else {
            "_value"
        };
        let snapshot_value_name = if record.fields.is_empty() {
            "_value"
        } else {
            "value"
        };
        writeln!(
            out,
            "export function redact{}({redact_value_name}: {}): Readonly<Record<string, unknown>> {{\n  return {{",
            record.name, record.name
        )
        .unwrap();
        for field in &record.fields {
            let expression = if field.privacy == Privacy::Public {
                ts_redaction_expression(protocol, &field.ty, &format!("value.{}", field.name))
            } else {
                "\"[REDACTED]\"".into()
            };
            writeln!(out, "    {}: {},", field.name, expression).unwrap();
        }
        out.push_str("  };\n}\n\n");

        writeln!(
            out,
            "export function snapshot{}({snapshot_value_name}: {}): {} {{\n  return Object.freeze({{",
            record.name, record.name, record.name
        )
        .unwrap();
        for field in &record.fields {
            let expression =
                ts_snapshot_expression(protocol, &field.ty, &format!("value.{}", field.name));
            if field.presence == Presence::Optional {
                writeln!(
                    out,
                    "    ...(value.{} === undefined ? {{}} : {{ {}: {} }}),",
                    field.name, field.name, expression
                )
                .unwrap();
            } else {
                writeln!(out, "    {}: {},", field.name, expression).unwrap();
            }
        }
        writeln!(out, "  }}) as {};\n}}\n", record.name).unwrap();
    }

    for union in &protocol.unions {
        writeln!(
            out,
            "export function redact{}(value: {}): Readonly<Record<string, unknown>> {{\n  switch (value.kind) {{",
            union.name, union.name
        )
        .unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            writeln!(out, "    case \"{}\": return {{", variant.name).unwrap();
            writeln!(out, "      kind: \"{}\",", variant.name).unwrap();
            for field in &variant.fields {
                let expression = if field.privacy == Privacy::Public {
                    ts_redaction_expression(protocol, &field.ty, &format!("value.{}", field.name))
                } else {
                    "\"[REDACTED]\"".into()
                };
                writeln!(out, "      {}: {},", field.name, expression).unwrap();
            }
            out.push_str("    };\n");
        }
        out.push_str("  }\n}\n\n");

        writeln!(
            out,
            "export function snapshot{}(value: {}): {} {{\n  switch (value.kind) {{",
            union.name, union.name, union.name
        )
        .unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            writeln!(
                out,
                "    case \"{}\": return Object.freeze({{",
                variant.name
            )
            .unwrap();
            writeln!(out, "      kind: \"{}\",", variant.name).unwrap();
            for field in &variant.fields {
                writeln!(
                    out,
                    "      {}: {},",
                    field.name,
                    ts_snapshot_expression(protocol, &field.ty, &format!("value.{}", field.name))
                )
                .unwrap();
            }
            writeln!(out, "    }}) as {};", union.name).unwrap();
        }
        out.push_str("  }\n}\n\n");
    }
}

fn ts_redaction_expression(protocol: &Protocol, ty: &Type, expression: &str) -> String {
    match ty {
        Type::Primitive(Primitive::Bytes16 | Primitive::Bytes32) | Type::Bytes(_) => {
            format!("`[BYTES:${{{expression}.byteLength}}]`")
        }
        Type::Primitive(_) => expression.into(),
        Type::Named(name) if matches!(name.as_str(), "SceneHash" | "CapabilityDecisionHash") => {
            "\"[REDACTED]\"".into()
        }
        Type::Named(name)
            if protocol.records.iter().any(|record| record.name == *name)
                || protocol.unions.iter().any(|union| union.name == *name) =>
        {
            format!("redact{name}({expression})")
        }
        Type::Named(_) => expression.into(),
        Type::Optional(inner) => format!(
            "{expression} === undefined ? undefined : {}",
            ts_redaction_expression(protocol, inner, expression)
        ),
        Type::List(inner, _) => format!(
            "{expression}.map((entry) => {})",
            ts_redaction_expression(protocol, inner, "entry")
        ),
    }
}

fn ts_snapshot_expression(protocol: &Protocol, ty: &Type, expression: &str) -> String {
    match ty {
        Type::Primitive(Primitive::Bytes16 | Primitive::Bytes32) | Type::Bytes(_) => {
            format!("new Uint8Array({expression})")
        }
        Type::Primitive(_) => expression.into(),
        Type::Named(name)
            if protocol.records.iter().any(|record| record.name == *name)
                || protocol.unions.iter().any(|union| union.name == *name) =>
        {
            format!("snapshot{name}({expression})")
        }
        Type::Named(name) => {
            let copies_bytes = protocol.scalars.iter().any(|scalar| {
                scalar.name == *name
                    && matches!(scalar.primitive, Primitive::Bytes16 | Primitive::Bytes32)
            });
            if copies_bytes {
                format!("new Uint8Array({expression})")
            } else {
                expression.into()
            }
        }
        Type::Optional(inner) => format!(
            "{expression} === undefined ? undefined : {}",
            ts_snapshot_expression(protocol, inner, expression)
        ),
        Type::List(inner, _) => format!(
            "Object.freeze({expression}.map((entry) => {})) as unknown as {}",
            ts_snapshot_expression(protocol, inner, "entry"),
            ts_type(ty)
        ),
    }
}

fn write_ts_union_capability_requirements(out: &mut String, protocol: &Protocol) {
    out.push_str(
        "export interface UnionVariantCapabilityRequirement {\n\
  readonly union_name: string;\n\
  readonly variant_name: string;\n\
  readonly capability: bigint;\n\
}\n\
export const UNION_VARIANT_CAPABILITY_REQUIREMENTS = [\n",
    );
    for union in &protocol.unions {
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            if let Some(capability) = &variant.required_capability {
                writeln!(
                    out,
                    "  {{ union_name: \"{}\", variant_name: \"{}\", capability: {} }},",
                    union.name,
                    variant.name,
                    ts_required_capability(Some(capability))
                )
                .unwrap();
            }
        }
    }
    out.push_str("] as const satisfies readonly UnionVariantCapabilityRequirement[];\n\n");
    if let Some(surface) = protocol
        .unions
        .iter()
        .find(|union| union.name == "SurfaceTransport")
    {
        let variants = surface
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
            .map(|variant| format!("\"{}\"", variant.name))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "export const BROWSER_ALLOWED_SURFACE_TRANSPORT_KINDS = [{variants}] as const;\n"
        )
        .unwrap();
    }
    for union in &protocol.unions {
        writeln!(
            out,
            "export function {}RequiredCapability(value: {}): bigint {{\n  switch (value.kind) {{",
            lower_camel_case(&union.name),
            union.name
        )
        .unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            writeln!(
                out,
                "    case \"{}\": return {};",
                variant.name,
                ts_required_capability(variant.required_capability.as_deref())
            )
            .unwrap();
        }
        out.push_str("  }\n}\n\n");
    }
}

fn write_ts_engine_error_descriptors(out: &mut String, protocol: &Protocol) {
    if !protocol
        .records
        .iter()
        .any(|record| record.name == "EngineError")
    {
        return;
    }
    out.push_str(
        "export interface EngineErrorDescriptor {\n\
  readonly code: EngineErrorCode;\n\
  readonly category: ErrorCategory;\n\
  readonly severity: ErrorSeverity;\n\
  readonly recoverability: ErrorRecoverability;\n\
}\n\
export const ENGINE_ERROR_DESCRIPTORS = [\n",
    );
    for (code, category, severity, recoverability) in engine_error_policies() {
        writeln!(
            out,
            "  {{ code: EngineErrorCode.{code}, category: ErrorCategory.{category}, severity: ErrorSeverity.{severity}, recoverability: ErrorRecoverability.{recoverability} }},"
        )
        .unwrap();
    }
    out.push_str("] as const satisfies readonly EngineErrorDescriptor[];\n\n");
}

fn write_ts_helpers(out: &mut String) {
    out.push_str(
        "type UnknownRecord = Record<string, unknown>;\n\
const hasOwn = (value: UnknownRecord, key: string): boolean => Object.prototype.hasOwnProperty.call(value, key);\n\
const isRecord = (value: unknown): value is UnknownRecord => typeof value === \"object\" && value !== null && !Array.isArray(value);\n\
const exactKeys = (value: UnknownRecord, required: readonly string[], optional: readonly string[]): boolean => {\n\
  const allowed = new Set([...required, ...optional]);\n\
  return required.every((key) => hasOwn(value, key)) && Object.keys(value).every((key) => allowed.has(key));\n\
};\n\
const isU8 = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= 0 && Number(value) <= 0xff;\n\
const isU16 = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= 0 && Number(value) <= 0xffff;\n\
const isU32 = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= 0 && Number(value) <= 0xffffffff;\n\
const isI32 = (value: unknown): value is number => Number.isInteger(value) && Number(value) >= -0x80000000 && Number(value) <= 0x7fffffff;\n\
const MAX_U64 = 0xffffffffffffffffn;\n\
const isU64 = (value: unknown): value is bigint => typeof value === \"bigint\" && value >= 0n && value <= 0xffffffffffffffffn;\n\
const isFixedBytes = (value: unknown, length: number): value is Uint8Array => value instanceof Uint8Array\n\
  && value.byteLength === length\n\
  && !(typeof SharedArrayBuffer !== \"undefined\" && value.buffer instanceof SharedArrayBuffer);\n\
const fixedBytesEqual = (left: Uint8Array, right: Uint8Array): boolean => left.byteLength === right.byteLength && left.every((byte, index) => byte === right[index]);\n\
const gcdU32 = (left: number, right: number): number => {\n\
  let a = left;\n\
  let b = right;\n\
  while (b !== 0) {\n\
    const remainder = a % b;\n\
    a = b;\n\
    b = remainder;\n\
  }\n\
  return a;\n\
};\n\n",
    );
}

fn write_ts_enum(out: &mut String, enumeration: &EnumDef) {
    if enumeration.repr == Primitive::U64 {
        writeln!(out, "export const {} = {{", enumeration.name).unwrap();
        for variant in &enumeration.variants {
            writeln!(out, "  {}: {}n,", variant.name, variant.tag).unwrap();
        }
        writeln!(
            out,
            "}} as const;\nexport type {} = (typeof {})[keyof typeof {}];",
            enumeration.name, enumeration.name, enumeration.name
        )
        .unwrap();
    } else {
        writeln!(out, "export enum {} {{", enumeration.name).unwrap();
        for variant in &enumeration.variants {
            writeln!(out, "  {} = {},", variant.name, variant.tag).unwrap();
        }
        writeln!(out, "}}").unwrap();
    }
    let values = enumeration
        .variants
        .iter()
        .map(|variant| {
            if enumeration.repr == Primitive::U64 {
                format!("{}n", variant.tag)
            } else {
                variant.tag.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "export const validate{} = (value: unknown): value is {} => [{}].includes(value as never);\n",
        enumeration.name, enumeration.name, values
    )
    .unwrap();
}

fn write_ts_endpoint_capabilities(out: &mut String, protocol: &Protocol) {
    let Some(definition) = protocol
        .enums
        .iter()
        .find(|value| value.name == "EndpointCapability")
    else {
        return;
    };
    for variant in &definition.variants {
        writeln!(
            out,
            "export const ENDPOINT_CAPABILITY_{} = {}n as const;",
            screaming_snake_case(&variant.name),
            variant.tag
        )
        .unwrap();
    }
    let expression = definition
        .variants
        .iter()
        .map(|variant| format!("{}n", variant.tag))
        .collect::<Vec<_>>()
        .join(" | ");
    writeln!(
        out,
        "export const KNOWN_ENDPOINT_CAPABILITIES = {expression};\n"
    )
    .unwrap();
}

fn write_ts_engine_execution_capabilities(out: &mut String, protocol: &Protocol) {
    let Some(definition) = protocol
        .enums
        .iter()
        .find(|value| value.name == "EngineExecutionCapability")
    else {
        return;
    };
    for variant in &definition.variants {
        writeln!(
            out,
            "export const ENGINE_EXECUTION_CAPABILITY_{} = {}n as const;",
            screaming_snake_case(&variant.name),
            variant.tag
        )
        .unwrap();
    }
    let expression = definition
        .variants
        .iter()
        .map(|variant| format!("{}n", variant.tag))
        .collect::<Vec<_>>()
        .join(" | ");
    writeln!(
        out,
        "export const KNOWN_ENGINE_EXECUTION_CAPABILITIES = {expression};\n"
    )
    .unwrap();
}

fn write_ts_descriptors(out: &mut String, protocol: &Protocol) {
    let codec = FixedLeCodec::new(
        protocol,
        CodecLimits::new(
            64,
            protocol.max_message_bytes as usize,
            protocol.max_message_bytes as usize,
        ),
    )
    .expect("parser validates fixed_le_v1 schema");
    out.push_str(
        "export type MessageKind = \"command\" | \"event\";\n\
export type CorrelationRequirement = \"required\" | \"optional\" | \"forbidden\";\n\
export type OutcomeDisposition = \"stream\" | \"terminal\";\n\
export interface OutcomeDescriptor { readonly event_id: number; readonly disposition: OutcomeDisposition }\n\
export interface CorrelationShape {\n\
  readonly worker: CorrelationRequirement;\n\
  readonly session: CorrelationRequirement;\n\
  readonly request: CorrelationRequirement;\n\
  readonly generation: CorrelationRequirement;\n\
}\n\
export interface MessageDescriptor {\n\
  readonly kind: MessageKind;\n\
  readonly name: string;\n\
  readonly id: number;\n\
  readonly payload: string;\n\
  readonly state_precondition: string;\n\
  readonly correlation: string;\n\
  readonly correlation_shape: CorrelationShape;\n\
  readonly replayable: boolean;\n\
  readonly allowed_flags: number;\n\
  readonly min_transfer_slots: number;\n\
  readonly max_transfer_slots: number;\n\
  readonly max_payload_bytes: number;\n\
  readonly maximum_encoded_payload_bytes: number;\n\
  readonly required_capability: bigint;\n\
  readonly outcomes: readonly OutcomeDescriptor[];\n\
}\n\n",
    );
    let states = protocol
        .messages
        .iter()
        .map(|message| format!("\"{}\"", message.state))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "export const STATE_PRECONDITIONS = [{states}] as const;\n"
    )
    .unwrap();
    out.push_str(
        "export type FieldPrivacy = \"public\" | \"private\" | \"sensitive\";\n\
export interface TypeFieldDescriptor {\n\
  readonly owner: string;\n\
  readonly variant?: string;\n\
  readonly name: string;\n\
  readonly wire_type: string;\n\
  readonly required: boolean;\n\
  readonly privacy: FieldPrivacy;\n\
  readonly max_count: number;\n\
}\n\
export const TYPE_FIELD_DESCRIPTORS = [\n",
    );
    for record in &protocol.records {
        for field in &record.fields {
            writeln!(
                out,
                "  {{ owner: \"{}\", name: \"{}\", wire_type: \"{}\", required: {}, privacy: \"{}\", max_count: {} }},",
                record.name,
                field.name,
                field.ty.schema_name(),
                field.presence == Presence::Required,
                ts_privacy(field.privacy),
                type_limit(&field.ty)
            )
            .unwrap();
        }
    }
    for union in &protocol.unions {
        for variant in union
            .variants
            .iter()
            .filter(|variant| browser_union_variant(variant))
        {
            for field in &variant.fields {
                writeln!(
                    out,
                    "  {{ owner: \"{}\", variant: \"{}\", name: \"{}\", wire_type: \"{}\", required: true, privacy: \"{}\", max_count: {} }},",
                    union.name,
                    variant.name,
                    field.name,
                    field.ty.schema_name(),
                    ts_privacy(field.privacy),
                    type_limit(&field.ty)
                )
                .unwrap();
            }
        }
    }
    out.push_str("] as const satisfies readonly TypeFieldDescriptor[];\n\n");
    for message in &protocol.messages {
        writeln!(
            out,
            "export const MESSAGE_ID_{} = {} as const;",
            screaming_snake_case(&message.name),
            message.id
        )
        .unwrap();
    }
    out.push_str("\nexport const MESSAGE_DESCRIPTORS = [\n");
    for message in &protocol.messages {
        let (worker, session, request, generation) = correlation_shape(&message.correlation);
        let outcomes = message
            .outcomes
            .iter()
            .map(|outcome| {
                let id = protocol
                    .messages
                    .iter()
                    .find(|candidate| {
                        candidate.kind == MessageKind::Event && candidate.name == outcome.name
                    })
                    .expect("parser validates outcomes")
                    .id;
                format!(
                    "{{ event_id: {id}, disposition: \"{}\" }}",
                    outcome.disposition.schema_name()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let required_capability = ts_required_capability(message.required_capability.as_deref());
        let maximum_encoded_payload_bytes = maximum_message_payload(&codec, message);
        writeln!(
            out,
            "  {{ kind: \"{}\", name: \"{}\", id: {}, payload: \"{}\", state_precondition: \"{}\", correlation: \"{}\", correlation_shape: {{ worker: \"{}\", session: \"{}\", request: \"{}\", generation: \"{}\" }}, replayable: {}, allowed_flags: {}, min_transfer_slots: {}, max_transfer_slots: {}, max_payload_bytes: {}, maximum_encoded_payload_bytes: {maximum_encoded_payload_bytes}, required_capability: {required_capability}, outcomes: [{}] }},",
            message.kind.schema_name(),
            message.name,
            message.id,
            message.payload,
            message.state,
            message.correlation,
            worker.to_ascii_lowercase(),
            session.to_ascii_lowercase(),
            request.to_ascii_lowercase(),
            generation.to_ascii_lowercase(),
            message.disposition == "yes",
            message.allowed_flags,
            message.min_transfer_slots,
            message.max_transfer_slots,
            message.max_payload_bytes,
            outcomes
        )
        .unwrap();
    }
    out.push_str(
        "] as const satisfies readonly MessageDescriptor[];\n\
const MESSAGE_DESCRIPTOR_BY_ID: ReadonlyMap<number, MessageDescriptor> = new Map(MESSAGE_DESCRIPTORS.map((descriptor) => [descriptor.id, descriptor] as const));\n\
export const descriptorById = (id: number): MessageDescriptor | undefined => MESSAGE_DESCRIPTOR_BY_ID.get(id);\n\n",
    );
}

fn write_ts_record(out: &mut String, record: &crate::model::Record) {
    writeln!(out, "export interface {} {{", record.name).unwrap();
    for field in &record.fields {
        let marker = if field.presence == Presence::Optional {
            "?"
        } else {
            ""
        };
        writeln!(out, "  {}{}: {};", field.name, marker, ts_type(&field.ty)).unwrap();
    }
    writeln!(out, "}}\n").unwrap();
    let required = record
        .fields
        .iter()
        .filter(|field| field.presence == Presence::Required)
        .map(|field| format!("\"{}\"", field.name))
        .collect::<Vec<_>>()
        .join(", ");
    let optional = record
        .fields
        .iter()
        .filter(|field| field.presence == Presence::Optional)
        .map(|field| format!("\"{}\"", field.name))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "export function validate{}(value: unknown): value is {} {{",
        record.name, record.name
    )
    .unwrap();
    writeln!(
        out,
        "  if (!isRecord(value) || !exactKeys(value, [{required}], [{optional}])) return false;"
    )
    .unwrap();
    for field in &record.fields {
        let condition = ts_validator(&field.ty, &format!("value.{}", field.name));
        if field.presence == Presence::Optional {
            writeln!(
                out,
                "  if (hasOwn(value, \"{}\") && !({condition})) return false;",
                field.name
            )
            .unwrap();
        } else {
            writeln!(out, "  if (!({condition})) return false;").unwrap();
        }
    }
    if record.name == "EnvelopeHeader" {
        out.push_str(
            "  const header = value as unknown as EnvelopeHeader;\n\
  const descriptor = descriptorById(header.message_type);\n\
  if (descriptor === undefined) return false;\n\
  if (header.major !== PROTOCOL_MAJOR || header.minor !== PROTOCOL_MINOR) return false;\n\
  if ((header.flags & ~descriptor.allowed_flags) !== 0) return false;\n\
  if (header.payload_len > MAX_MESSAGE_BYTES || header.payload_len > descriptor.max_payload_bytes || header.payload_len > descriptor.maximum_encoded_payload_bytes) return false;\n\
  if (header.sequence === 0n) return false;\n",
        );
    }
    if record.name == "EndpointCapabilities" {
        out.push_str(
            "  const capabilities = value as unknown as EndpointCapabilities;\n\
  if ((capabilities.mandatory & ~KNOWN_ENDPOINT_CAPABILITIES) !== 0n) return false;\n\
  if ((capabilities.mandatory & ~capabilities.supported) !== 0n) return false;\n",
        );
    }
    if record.name == "EngineExecutionCapabilities" {
        out.push_str(
            "  const capabilities = value as unknown as EngineExecutionCapabilities;\n\
  if ((capabilities.supported & ~KNOWN_ENGINE_EXECUTION_CAPABILITIES) !== 0n) return false;\n",
        );
    }
    if record.name == "ByteRange" {
        out.push_str(
            "  const range = value as unknown as ByteRange;\n\
  if (range.len === 0n || range.start > MAX_U64 - range.len) return false;\n",
        );
    }
    if record.name == "SourceIdentity" {
        out.push_str(
            "  const identity = value as unknown as SourceIdentity;\n\
  if (identity.revision === 0n || !identity.stable_id.some((byte) => byte !== 0)) return false;\n",
        );
    }
    if record.name == "DataSegment" {
        out.push_str(
            "  const segment = value as unknown as DataSegment;\n\
  if (segment.byte_length !== segment.range.len) return false;\n",
        );
    }
    if record.name == "ProvideDataCommand" {
        out.push_str(
            "  const command = value as unknown as ProvideDataCommand;\n\
  if (command.ticket === 0n || command.segments.length === 0) return false;\n\
  let aggregate = 0n;\n\
  let previousEnd = 0n;\n\
  for (const [index, segment] of command.segments.entries()) {\n\
    if (segment.slot !== index || segment.byte_length > MAX_DATA_SEGMENT_BYTES) return false;\n\
    if (index !== 0 && segment.range.start <= previousEnd) return false;\n\
    previousEnd = segment.range.start + segment.range.len;\n\
    aggregate += segment.byte_length;\n\
    if (aggregate > MAX_DATA_TICKET_BYTES) return false;\n\
  }\n",
        );
    }
    if record.name == "FailDataCommand" {
        out.push_str(
            "  const failure = value as unknown as FailDataCommand;\n\
  if (failure.ticket === 0n) return false;\n\
  const observedMatches = failure.observed !== undefined\n\
    && failure.observed.revision === failure.expected.revision\n\
    && fixedBytesEqual(failure.observed.stable_id, failure.expected.stable_id);\n\
  if (failure.code === SourceFailureCode.SourceChanged) {\n\
    if (failure.observed === undefined || observedMatches || failure.retryable) return false;\n\
  } else if (failure.observed !== undefined) return false;\n",
        );
    }
    if record.name == "NeedDataEvent" {
        out.push_str(
            "  const need = value as unknown as NeedDataEvent;\n\
  if (need.ticket === 0n || need.ranges.length === 0) return false;\n\
  let aggregate = 0n;\n\
  let previousEnd = 0n;\n\
  for (const [index, range] of need.ranges.entries()) {\n\
    if (range.len > MAX_DATA_SEGMENT_BYTES) return false;\n\
    if (index !== 0 && range.start <= previousEnd) return false;\n\
    previousEnd = range.start + range.len;\n\
    aggregate += range.len;\n\
    if (aggregate > MAX_DATA_TICKET_BYTES) return false;\n\
  }\n",
        );
    }
    if record.name == "RegisterCanvasCommand" {
        out.push_str(
            "  const canvas = value as unknown as RegisterCanvasCommand;\n\
  if (canvas.canvas === 0n || canvas.canvas_epoch === 0n || canvas.width === 0 || canvas.height === 0) return false;\n",
        );
    }
    if record.name == "ReleaseCanvasCommand" {
        out.push_str(
            "  const canvas = value as unknown as ReleaseCanvasCommand;\n\
  if (canvas.canvas === 0n || canvas.canvas_epoch === 0n) return false;\n",
        );
    }
    if record.name == "ResizeCanvasCommand" {
        out.push_str(
            "  const canvas = value as unknown as ResizeCanvasCommand;\n\
  if (canvas.canvas === 0n || canvas.canvas_epoch === 0n || canvas.width === 0 || canvas.height === 0) return false;\n",
        );
    }
    if matches!(
        record.name.as_str(),
        "CanvasRegisteredEvent" | "CanvasResizedEvent"
    ) {
        out.push_str(
            "  const canvas = value as unknown as CanvasRegisteredEvent | CanvasResizedEvent;\n\
  if (canvas.canvas === 0n || canvas.canvas_epoch === 0n || canvas.width === 0 || canvas.height === 0) return false;\n",
        );
    }
    if record.name == "GetPageMetricsCommand" {
        out.push_str(
            "  const request = value as unknown as GetPageMetricsCommand;\n\
  if (request.document_revision === 0n || request.max_count === 0 || request.max_count > 64) return false;\n",
        );
    }
    if record.name == "PageMetricsEvent" {
        out.push_str(
            "  const batch = value as unknown as PageMetricsEvent;\n\
  if (batch.document_revision === 0n || batch.pages.length > 64) return false;\n\
  if (batch.start_index > batch.total_pages || batch.pages.length > batch.total_pages - batch.start_index) return false;\n\
  if (!batch.pages.every((page, index) => page.page_index === batch.start_index + index)) return false;\n",
        );
    }
    if record.name == "GenerationPlannedEvent" {
        out.push_str(
            "  const plan = value as unknown as GenerationPlannedEvent;\n\
  const manifest = plan.manifest;\n\
  if (manifest.document_revision === 0n || manifest.renderer_epoch === 0 || manifest.plan_id === 0n || manifest.regions.length === 0) return false;\n\
  if (!manifest.render_config.some((byte) => byte !== 0) || !plan.plan_hash.some((byte) => byte !== 0) || !manifest.scene_hash.some((byte) => byte !== 0) || !manifest.decision_hash.some((byte) => byte !== 0)) return false;\n\
  const identities = new Set(manifest.regions.map((region) => `${region.page_index}:${region.x}:${region.y}:${region.width}:${region.height}`));\n\
  if (identities.size !== manifest.regions.length || manifest.regions.some((region) => region.width === 0 || region.height === 0)) return false;\n",
        );
    }
    if record.name == "GenerationCompletedEvent" {
        out.push_str(
            "  const completion = value as unknown as GenerationCompletedEvent;\n\
  if (completion.status === GenerationCompletionStatus.Failed ? completion.error === undefined : completion.error !== undefined) return false;\n",
        );
    }
    if record.name == "EngineError" {
        out.push_str(
            "  const error = value as unknown as EngineError;\n\
  if (error.diagnostic_id === 0n || !ENGINE_ERROR_DESCRIPTORS.some((descriptor) => descriptor.code === error.code && descriptor.category === error.category && descriptor.severity === error.severity && descriptor.recoverability === error.recoverability)) return false;\n",
        );
    }
    if record.name == "CapabilityReportedEvent" {
        out.push_str(
            "  const report = value as unknown as CapabilityReportedEvent;\n\
  if (!report.decision_hash.some((byte) => byte !== 0)) return false;\n",
        );
    }
    if record.name == "DataFailedEvent" {
        out.push_str("  if ((value as unknown as DataFailedEvent).ticket === 0n) return false;\n");
    }
    if record.name == "DocumentReadyEvent" {
        out.push_str(
            "  const ready = value as unknown as DocumentReadyEvent;\n\
  if (ready.session === 0n || ready.document_revision === 0n) return false;\n",
        );
    }
    if record.name == "HelloCommand" {
        out.push_str(
            "  if ((value as unknown as HelloCommand).hello.endpoint_role !== EndpointRole.Host) return false;\n",
        );
    }
    if record.name == "EngineHelloEvent" {
        out.push_str(
            "  if ((value as unknown as EngineHelloEvent).hello.endpoint_role !== EndpointRole.Engine) return false;\n",
        );
    }
    if record.name == "ReadyEvent" {
        out.push_str(
            "  const ready = value as unknown as ReadyEvent;\n\
  if (ready.worker === 0n || ready.capability_profiles.length === 0 || ready.output_profiles.length === 0) return false;\n\
  if (!ready.capability_profiles.every((profile, index) => index === 0 || ready.capability_profiles[index - 1]! < profile)) return false;\n\
  if (!ready.output_profiles.every((profile, index) => index === 0 || ready.output_profiles[index - 1]! < profile)) return false;\n",
        );
    }
    if record.name == "ReleaseSurfaceCommand" {
        out.push_str(
            "  const release = value as unknown as ReleaseSurfaceCommand;\n\
  if (release.surface === 0n || release.lease_token === 0n) return false;\n",
        );
    }
    if record.name == "SurfaceReclaimedEvent" {
        out.push_str(
            "  const release = value as unknown as SurfaceReclaimedEvent;\n\
  if (release.surface === 0n || release.lease_token === 0n) return false;\n",
        );
    }
    if record.name == "SurfaceReleaseAcknowledgedEvent" {
        out.push_str(
            "  const release = value as unknown as SurfaceReleaseAcknowledgedEvent;\n\
  if (release.surface === 0n || release.lease_token === 0n) return false;\n",
        );
    }
    if record.name == "ProtocolHello" {
        out.push_str(
            "  const hello = value as unknown as ProtocolHello;\n\
  if (hello.major !== PROTOCOL_MAJOR || hello.minor !== PROTOCOL_MINOR) return false;\n\
  if (hello.max_message_bytes === 0 || hello.max_message_bytes > MAX_MESSAGE_BYTES) return false;\n\
  if (hello.max_transfer_slots === 0 || hello.max_transfer_slots > MAX_TRANSFER_SLOTS) return false;\n",
        );
    }
    if record.name == "PageGeometry" {
        out.push_str(
            "  const geometry = value as unknown as PageGeometry;\n\
  if (!geometry.identity.some((byte) => byte !== 0)) return false;\n\
  if (geometry.media_box_width_milli_points === 0 || geometry.media_box_height_milli_points === 0 || geometry.crop_box_width_milli_points === 0 || geometry.crop_box_height_milli_points === 0) return false;\n",
        );
    }
    if record.name == "PageViewport" {
        out.push_str(
            "  const page = value as unknown as PageViewport;\n\
  if (page.clip_width_milli_points === 0 || page.clip_height_milli_points === 0) return false;\n",
        );
    }
    if record.name == "ViewportRequest" {
        out.push_str(
            "  const viewport = value as unknown as ViewportRequest;\n\
  if (viewport.generation === 0n || viewport.document_revision === 0n || viewport.zoom_numerator === 0 || viewport.zoom_denominator === 0 || viewport.device_scale_milli === 0) return false;\n\
  if (gcdU32(viewport.zoom_numerator, viewport.zoom_denominator) !== 1) return false;\n\
  const pageIndexes = new Set(viewport.visible_pages.map((page) => page.page_index));\n\
  if (pageIndexes.size !== viewport.visible_pages.length) return false;\n\
  const pageIdentities = new Set(viewport.visible_pages.map((page) => Array.from(page.geometry.identity).join(\",\")));\n\
  if (pageIdentities.size !== viewport.visible_pages.length) return false;\n",
        );
    }
    if record.name == "CapabilityDecision" {
        out.push_str(
            "  const decision = value as unknown as CapabilityDecision;\n\
  const missingCount = decision.missing.length;\n\
  const contributorCount = decision.contributors.length;\n\
  if (missingCount > CAPABILITY_DECISION_MISSING_MAX_COUNT || contributorCount > CAPABILITY_DECISION_CONTRIBUTORS_MAX_COUNT) return false;\n\
  if (!decision.missing.every((requirement, index) => requirement.id !== 0 && (index === 0 || decision.missing[index - 1]!.id < requirement.id))) return false;\n\
  if (!decision.contributors.every((contributor, index) => contributor.id !== 0 && (index === 0 || decision.contributors[index - 1]!.id < contributor.id))) return false;\n\
  const requirementIds = new Set(decision.missing.map((requirement) => requirement.id));\n\
  const contributorIds = new Set(decision.contributors.map((contributor) => contributor.id));\n\
  if (!decision.missing.every((requirement) => requirement.dependencies.length <= CAPABILITY_REQUIREMENT_DEPENDENCIES_MAX_COUNT\n\
    && requirement.contributor_ids.length <= CAPABILITY_REQUIREMENT_CONTRIBUTOR_IDS_MAX_COUNT\n\
    && requirement.dependencies.every((id, index) => id !== requirement.id && requirementIds.has(id) && (index === 0 || requirement.dependencies[index - 1]! < id))\n\
    && requirement.contributor_ids.every((id, index) => contributorIds.has(id) && (index === 0 || requirement.contributor_ids[index - 1]! < id)))) return false;\n\
  if (decision.missing_completeness === CollectionCompleteness.Complete ? missingCount !== decision.missing_total : missingCount >= decision.missing_total) return false;\n\
  if (decision.contributors_completeness === CollectionCompleteness.Complete ? contributorCount !== decision.contributors_total : contributorCount >= decision.contributors_total) return false;\n\
  if (decision.status === SupportStatus.Supported && (decision.missing_total !== 0 || missingCount !== 0 || hasOwn(value, \"location\") || hasOwn(value, \"rejection_code\"))) return false;\n\
  if (decision.status === SupportStatus.Unsupported && hasOwn(value, \"rejection_code\")) return false;\n\
  if (decision.status === SupportStatus.Rejected && !hasOwn(value, \"rejection_code\")) return false;\n",
        );
    }
    if record.name == "SurfaceReadyEvent" {
        out.push_str(
            "  const surface = value as unknown as SurfaceReadyEvent;\n\
  const metadata = surface.metadata;\n\
  if (metadata.id === 0n || metadata.lease_token === 0n || metadata.owner.worker === 0n || metadata.owner.session === 0n || metadata.generation === 0n || metadata.renderer_epoch === 0 || metadata.plan_id === 0n) return false;\n\
  if (!metadata.render_config.some((byte) => byte !== 0) || !metadata.plan_hash.some((byte) => byte !== 0) || !metadata.scene_hash.some((byte) => byte !== 0) || !metadata.decision_hash.some((byte) => byte !== 0)) return false;\n\
  if (metadata.width === 0 || metadata.height === 0 || metadata.stride === 0 || metadata.region.width === 0 || metadata.region.height === 0) return false;\n\
  const stride = BigInt(metadata.stride);\n\
  const minimumStride = BigInt(metadata.width) * 4n;\n\
  if (stride < minimumStride || stride % 4n !== 0n) return false;\n\
  const layoutBytes = stride * BigInt(metadata.height);\n\
  if (metadata.byte_length !== layoutBytes) return false;\n\
  const rangeEnd = metadata.byte_offset + metadata.byte_length;\n\
  if (rangeEnd > 0xffffffffffffffffn) return false;\n\
  let regionLength: bigint | undefined;\n\
  switch (surface.transport.kind) {\n\
    case \"BrowserArrayBuffer\":\n\
      if (metadata.alpha !== AlphaMode.Straight) return false;\n\
      regionLength = surface.transport.buffer_length;\n\
      break;\n\
    case \"BrowserImageBitmap\":\n\
      if (metadata.alpha !== AlphaMode.Premultiplied || surface.transport.width !== metadata.width || surface.transport.height !== metadata.height || metadata.byte_offset !== 0n || metadata.stride !== metadata.width * 4) return false;\n\
      break;\n\
    case \"BrowserSharedArrayBuffer\":\n\
      if (metadata.alpha !== AlphaMode.Straight || surface.transport.publication_epoch === 0 || surface.transport.fence_byte_offset % 4n !== 0n) return false;\n\
      if (surface.transport.fence_byte_offset > MAX_U64 - 4n || surface.transport.fence_byte_offset + 4n > surface.transport.buffer_length) return false;\n\
      if (!(surface.transport.fence_byte_offset + 4n <= metadata.byte_offset || surface.transport.fence_byte_offset >= rangeEnd)) return false;\n\
      regionLength = surface.transport.buffer_length;\n\
      break;\n\
  }\n\
  if (regionLength !== undefined && rangeEnd > regionLength) return false;\n",
        );
    }
    writeln!(out, "  return true;\n}}\n").unwrap();
}

fn write_ts_messages(out: &mut String, protocol: &Protocol) {
    for (kind, name) in [
        (MessageKind::Command, "Command"),
        (MessageKind::Event, "Event"),
    ] {
        writeln!(out, "export type {name} =").unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "  | {{ type: \"{}\"; payload: {} }}",
                message.name, message.payload
            )
            .unwrap();
        }
        writeln!(out, ";\n").unwrap();
        writeln!(
            out,
            "export function validate{name}(value: unknown): value is {name} {{\n  if (!isRecord(value) || !exactKeys(value, [\"type\", \"payload\"], []) || typeof value.type !== \"string\") return false;\n  switch (value.type) {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "    case \"{}\": return validate{}(value.payload);",
                message.name, message.payload
            )
            .unwrap();
        }
        writeln!(out, "    default: return false;\n  }}\n}}\n").unwrap();
    }
    for (kind, name) in [
        (MessageKind::Command, "Command"),
        (MessageKind::Event, "Event"),
    ] {
        writeln!(
            out,
            "export function snapshot{name}(value: {name}): {name} {{\n  switch (value.type) {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "    case \"{}\": return Object.freeze({{ type: \"{}\", payload: snapshot{}(value.payload) }});",
                message.name, message.name, message.payload
            )
            .unwrap();
        }
        out.push_str("  }\n}\n\n");
        writeln!(
            out,
            "export function redact{name}(value: {name}): Readonly<Record<string, unknown>> {{\n  switch (value.type) {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "    case \"{}\": return {{ type: \"{}\", payload: redact{}(value.payload) }};",
                message.name, message.name, message.payload
            )
            .unwrap();
        }
        out.push_str("  }\n}\n\n");
    }
    out.push_str(
        "export interface CommandEnvelope { header: EnvelopeHeader; correlation: Correlation; command: Command }\n\
export interface EventEnvelope { header: EnvelopeHeader; correlation: Correlation; event: Event }\n\n\
const CONNECTION_CONTEXT_BRAND: unique symbol = Symbol(\"EngineProtocolConnection\");\n\
export type ProtocolValidationErrorCode = \"InvalidHandshake\" | \"InvalidEnvelope\" | \"NonMonotonicSequence\";\n\
export interface ProtocolValidationError { readonly code: ProtocolValidationErrorCode }\n\
export type ProtocolValidationResult<T> = Readonly<{ ok: true; value: T }> | Readonly<{ ok: false; error: Readonly<ProtocolValidationError> }>;\n\
const protocolValidationOk = <T>(value: T): ProtocolValidationResult<T> => Object.freeze({ ok: true as const, value });\n\
const protocolValidationError = <T>(code: ProtocolValidationErrorCode): ProtocolValidationResult<T> => Object.freeze({ ok: false as const, error: Object.freeze({ code }) });\n\
export interface CompatibleHandshake {\n\
  readonly [CONNECTION_CONTEXT_BRAND]: true;\n\
  readonly minor: number;\n\
  readonly capabilities: bigint;\n\
  readonly max_message_bytes: number;\n\
  readonly max_transfer_slots: number;\n\
}\n\
export function negotiateHandshake(localInput: unknown, peerInput: unknown): CompatibleHandshake | undefined {\n\
  if (!validateProtocolHello(localInput) || !validateProtocolHello(peerInput)) return undefined;\n\
  const local = snapshotProtocolHello(localInput);\n\
  const peer = snapshotProtocolHello(peerInput);\n\
  const opposite = (local.endpoint_role === EndpointRole.Host && peer.endpoint_role === EndpointRole.Engine)\n\
    || (local.endpoint_role === EndpointRole.Engine && peer.endpoint_role === EndpointRole.Host);\n\
  if (!opposite || local.minor !== peer.minor || !fixedBytesEqual(local.schema_hash, SCHEMA_HASH) || !fixedBytesEqual(peer.schema_hash, SCHEMA_HASH)) return undefined;\n\
  if ((local.capabilities.mandatory & ~peer.capabilities.supported) !== 0n || (peer.capabilities.mandatory & ~local.capabilities.supported) !== 0n) return undefined;\n\
  return Object.freeze({\n\
    [CONNECTION_CONTEXT_BRAND]: true as const,\n\
    minor: local.minor,\n\
    capabilities: local.capabilities.supported & peer.capabilities.supported & KNOWN_ENDPOINT_CAPABILITIES,\n\
    max_message_bytes: Math.min(local.max_message_bytes, peer.max_message_bytes),\n\
    max_transfer_slots: Math.min(local.max_transfer_slots, peer.max_transfer_slots),\n\
  });\n\
}\n\
export function negotiateHandshakeResult(localInput: unknown, peerInput: unknown): ProtocolValidationResult<CompatibleHandshake> {\n\
  const value = negotiateHandshake(localInput, peerInput);\n\
  return value === undefined ? protocolValidationError(\"InvalidHandshake\") : protocolValidationOk(value);\n\
}\n\
export function validateHandshakeTranscript(hostHello: Command, engineHello: Event, hostAccept: Command, engineReady: Event): CompatibleHandshake | undefined {\n\
  if (hostHello.type !== \"Hello\" || engineHello.type !== \"EngineHello\" || hostAccept.type !== \"HelloAccept\" || engineReady.type !== \"Ready\") return undefined;\n\
  if (hostHello.payload.hello.endpoint_role !== EndpointRole.Host || engineHello.payload.hello.endpoint_role !== EndpointRole.Engine) return undefined;\n\
  const connection = negotiateHandshake(hostHello.payload.hello, engineHello.payload.hello);\n\
  if (connection === undefined || hostAccept.payload.negotiated_minor !== connection.minor || engineReady.payload.negotiated_minor !== connection.minor) return undefined;\n\
  if (!fixedBytesEqual(hostAccept.payload.schema_hash, SCHEMA_HASH) || !fixedBytesEqual(engineReady.payload.schema_hash, SCHEMA_HASH) || engineReady.payload.worker === 0n || engineReady.payload.execution_capabilities.supported !== engineHello.payload.execution_capabilities.supported) return undefined;\n\
  return connection;\n\
}\n\
export interface PendingSequenceCommit { commit(): boolean }\n\
export class EnvelopeSequenceTracker {\n\
  private lastAcceptedValue: bigint | undefined;\n\
  get lastAccepted(): bigint | undefined { return this.lastAcceptedValue; }\n\
  pending(candidate: bigint): PendingSequenceCommit | undefined {\n\
    if (candidate === 0n || (this.lastAcceptedValue !== undefined && candidate <= this.lastAcceptedValue)) return undefined;\n\
    let consumed = false;\n\
    return Object.freeze({ commit: (): boolean => {\n\
      if (consumed) return false;\n\
      consumed = true;\n\
      if (this.lastAcceptedValue !== undefined && candidate <= this.lastAcceptedValue) return false;\n\
      this.lastAcceptedValue = candidate;\n\
      return true;\n\
    }});\n\
  }\n\
}\n\
const correlationRequirementMet = (present: boolean, requirement: CorrelationRequirement): boolean => requirement === \"required\" ? present : requirement === \"optional\" ? true : !present;\n\
const validateDescriptorCorrelation = (correlation: Correlation, descriptor: MessageDescriptor): boolean => {\n\
  if (correlation.worker === 0n || correlation.session === 0n || correlation.request === 0n || correlation.generation === 0n) return false;\n\
  return correlationRequirementMet(true, descriptor.correlation_shape.worker)\n\
    && correlationRequirementMet(correlation.session !== undefined, descriptor.correlation_shape.session)\n\
    && correlationRequirementMet(correlation.request !== undefined, descriptor.correlation_shape.request)\n\
    && correlationRequirementMet(correlation.generation !== undefined, descriptor.correlation_shape.generation);\n\
};\n\
const validateTransferBinding = (message: Command | Event, transferSlots: number, descriptor: MessageDescriptor, connection: CompatibleHandshake): boolean => {\n\
  if (!isU16(transferSlots) || transferSlots > connection.max_transfer_slots || transferSlots < descriptor.min_transfer_slots || transferSlots > descriptor.max_transfer_slots) return false;\n\
  if ((descriptor.required_capability & ~connection.capabilities) !== 0n) return false;\n\
  switch (message.type) {\n\
    case \"ProvideData\": return message.payload.segments.length === transferSlots && message.payload.segments.every((segment, index) => segment.slot === index);\n\
    case \"SurfaceReady\": {\n\
      const transport = message.payload.transport;\n\
      if ((surfaceTransportRequiredCapability(transport) & ~connection.capabilities) !== 0n) return false;\n\
      switch (transport.kind) {\n\
        case \"BrowserArrayBuffer\":\n\
        case \"BrowserImageBitmap\": return transferSlots === 1 && transport.slot < transferSlots;\n\
        case \"BrowserSharedArrayBuffer\": return transferSlots === 1 && transport.attachment_slot < transferSlots;\n\
      }\n\
    }\n\
    default: return true;\n\
  }\n\
};\n\
export function validateProvideDataTransferLengths(command: ProvideDataCommand, transferLengths: readonly bigint[]): boolean {\n\
  return transferLengths.length > 0\n\
    && transferLengths.length === command.segments.length\n\
    && transferLengths.length <= MAX_TRANSFER_SLOTS\n\
    && command.segments.every((segment, index) => segment.slot === index\n\
      && isU64(transferLengths[index])\n\
      && transferLengths[index] === segment.byte_length);\n\
}\n\
const validatePayloadCorrelation = (correlation: Correlation, message: Command | Event): boolean => {\n\
  switch (message.type) {\n\
    case \"SetViewport\": return correlation.generation === message.payload.viewport.generation;\n\
    case \"Cancel\": return correlation.request === message.payload.target;\n\
    case \"Ready\": return correlation.worker === message.payload.worker;\n\
    case \"DocumentReady\": return correlation.session === undefined || correlation.session === message.payload.session;\n\
    case \"SurfaceReady\": return correlation.worker === message.payload.metadata.owner.worker\n\
      && correlation.session === message.payload.metadata.owner.session\n\
      && correlation.generation === message.payload.metadata.generation;\n\
    case \"RequestCancelled\": return correlation.request === message.payload.target;\n\
    case \"SessionClosed\": return correlation.session === message.payload.session;\n\
    case \"WorkerStopped\": return correlation.worker === message.payload.worker;\n\
    default: return true;\n\
  }\n\
};\n\
const validateEnvelopeDescriptor = (header: EnvelopeHeader, correlation: Correlation, message: Command | Event, kind: MessageKind, transferSlots: number, actualPayloadBytes: number, connection: CompatibleHandshake): boolean => {\n\
  const descriptor = descriptorById(header.message_type);\n\
  return connection[CONNECTION_CONTEXT_BRAND] === true\n\
    && descriptor !== undefined\n\
    && descriptor.kind === kind\n\
    && descriptor.name === message.type\n\
    && header.minor === connection.minor\n\
    && isU32(actualPayloadBytes)\n\
    && actualPayloadBytes === header.payload_len\n\
    && actualPayloadBytes <= connection.max_message_bytes\n\
    && validateDescriptorCorrelation(correlation, descriptor)\n\
    && validateTransferBinding(message, transferSlots, descriptor, connection)\n\
    && validatePayloadCorrelation(correlation, message);\n\
};\n\
export function validateEnvelopeHeaderForMinor(value: unknown, negotiatedMinor: number): value is EnvelopeHeader {\n\
  return Number.isInteger(negotiatedMinor)\n\
    && negotiatedMinor === PROTOCOL_MINOR\n\
    && validateEnvelopeHeader(value)\n\
    && value.minor === negotiatedMinor;\n\
}\n\
export interface PendingCommandEnvelope { readonly envelope: CommandEnvelope; commitSequence(): boolean }\n\
export interface PendingEventEnvelope { readonly envelope: EventEnvelope; commitSequence(): boolean }\n\
export function beginValidateCommandEnvelope(value: unknown, transferSlots: number, actualPayloadBytes: number, connection: CompatibleHandshake, sequence: EnvelopeSequenceTracker): PendingCommandEnvelope | undefined {\n\
  if (!isRecord(value) || !exactKeys(value, [\"header\", \"correlation\", \"command\"], []) || !validateEnvelopeHeaderForMinor(value.header, connection.minor) || !validateCorrelation(value.correlation) || !validateCommand(value.command)) return undefined;\n\
  const input = value as unknown as CommandEnvelope;\n\
  const envelope = Object.freeze({ header: snapshotEnvelopeHeader(input.header), correlation: snapshotCorrelation(input.correlation), command: snapshotCommand(input.command) });\n\
  if (!validateEnvelopeDescriptor(envelope.header, envelope.correlation, envelope.command, \"command\", transferSlots, actualPayloadBytes, connection)) return undefined;\n\
  const pending = sequence.pending(envelope.header.sequence);\n\
  return pending === undefined ? undefined : Object.freeze({ envelope, commitSequence: (): boolean => pending.commit() });\n\
}\n\
export function beginValidateEventEnvelope(value: unknown, transferSlots: number, actualPayloadBytes: number, connection: CompatibleHandshake, sequence: EnvelopeSequenceTracker): PendingEventEnvelope | undefined {\n\
  if (!isRecord(value) || !exactKeys(value, [\"header\", \"correlation\", \"event\"], []) || !validateEnvelopeHeaderForMinor(value.header, connection.minor) || !validateCorrelation(value.correlation) || !validateEvent(value.event)) return undefined;\n\
  const input = value as unknown as EventEnvelope;\n\
  const envelope = Object.freeze({ header: snapshotEnvelopeHeader(input.header), correlation: snapshotCorrelation(input.correlation), event: snapshotEvent(input.event) });\n\
  if (!validateEnvelopeDescriptor(envelope.header, envelope.correlation, envelope.event, \"event\", transferSlots, actualPayloadBytes, connection)) return undefined;\n\
  const pending = sequence.pending(envelope.header.sequence);\n\
  return pending === undefined ? undefined : Object.freeze({ envelope, commitSequence: (): boolean => pending.commit() });\n\
}\n\
export function beginValidateCommandEnvelopeResult(value: unknown, transferSlots: number, actualPayloadBytes: number, connection: CompatibleHandshake, sequence: EnvelopeSequenceTracker): ProtocolValidationResult<PendingCommandEnvelope> {\n\
  const pending = beginValidateCommandEnvelope(value, transferSlots, actualPayloadBytes, connection, sequence);\n\
  if (pending !== undefined) return protocolValidationOk(pending);\n\
  if (isRecord(value) && isRecord(value.header) && typeof value.header.sequence === \"bigint\" && (value.header.sequence === 0n || (sequence.lastAccepted !== undefined && value.header.sequence <= sequence.lastAccepted))) return protocolValidationError(\"NonMonotonicSequence\");\n\
  return protocolValidationError(\"InvalidEnvelope\");\n\
}\n\
export function beginValidateEventEnvelopeResult(value: unknown, transferSlots: number, actualPayloadBytes: number, connection: CompatibleHandshake, sequence: EnvelopeSequenceTracker): ProtocolValidationResult<PendingEventEnvelope> {\n\
  const pending = beginValidateEventEnvelope(value, transferSlots, actualPayloadBytes, connection, sequence);\n\
  if (pending !== undefined) return protocolValidationOk(pending);\n\
  if (isRecord(value) && isRecord(value.header) && typeof value.header.sequence === \"bigint\" && (value.header.sequence === 0n || (sequence.lastAccepted !== undefined && value.header.sequence <= sequence.lastAccepted))) return protocolValidationError(\"NonMonotonicSequence\");\n\
  return protocolValidationError(\"InvalidEnvelope\");\n\
}\n",
    );
}

fn ts_primitive(value: Primitive) -> &'static str {
    match value {
        Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::I32 => "number",
        Primitive::U64 => "bigint",
        Primitive::Bool => "boolean",
        Primitive::Bytes16 | Primitive::Bytes32 => "Uint8Array",
    }
}

fn ts_primitive_validator(value: Primitive, expression: &str) -> String {
    match value {
        Primitive::U8 => format!("isU8({expression})"),
        Primitive::U16 => format!("isU16({expression})"),
        Primitive::U32 => format!("isU32({expression})"),
        Primitive::U64 => format!("isU64({expression})"),
        Primitive::I32 => format!("isI32({expression})"),
        Primitive::Bool => format!("typeof {expression} === \"boolean\""),
        Primitive::Bytes16 => format!("isFixedBytes({expression}, 16)"),
        Primitive::Bytes32 => format!("isFixedBytes({expression}, 32)"),
    }
}

fn ts_type(value: &Type) -> String {
    match value {
        Type::Primitive(value) => ts_primitive(*value).into(),
        Type::Named(value) => value.clone(),
        Type::Optional(inner) => ts_type(inner),
        Type::List(inner, _) => format!("{}[]", ts_type(inner)),
        Type::Bytes(_) => "Uint8Array".into(),
    }
}

fn ts_validator(value: &Type, expression: &str) -> String {
    match value {
        Type::Primitive(value) => ts_primitive_validator(*value, expression),
        Type::Named(value) => format!("validate{value}({expression})"),
        Type::Optional(inner) => ts_validator(inner, expression),
        Type::List(inner, limit) => format!(
            "Array.isArray({expression}) && {expression}.length <= {limit} && {expression}.every((entry) => {})",
            ts_validator(inner, "entry")
        ),
        Type::Bytes(limit) => {
            format!(
                "{expression} instanceof Uint8Array && {expression}.byteLength <= {limit} && !(typeof SharedArrayBuffer !== \"undefined\" && {expression}.buffer instanceof SharedArrayBuffer)"
            )
        }
    }
}

fn generate_desktop_registry(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let mut out = String::new();
    writeln!(out, "# @generated by pdf-rs-protocol-codegen; DO NOT EDIT.").unwrap();
    writeln!(
        out,
        "registry {} {} {}",
        protocol.name, protocol.major, protocol.minor
    )
    .unwrap();
    writeln!(out, "generator_version {GENERATOR_VERSION}").unwrap();
    writeln!(out, "wire_identity_domain {WIRE_IDENTITY_DOMAIN}").unwrap();
    writeln!(out, "payload_codec_abi_version {PAYLOAD_CODEC_ABI_VERSION}").unwrap();
    writeln!(out, "compatible_minor_min {}", protocol.minor).unwrap();
    writeln!(out, "schema_sha256 {}", lowercase_hex(digest)).unwrap();
    writeln!(out, "wire_schema_hash {}", lowercase_hex(&digest[..16])).unwrap();
    writeln!(out, "schema_hash_truncation {SCHEMA_HASH_TRUNCATION}").unwrap();
    writeln!(out, "target_projection desktop").unwrap();
    writeln!(out, "payload_codec {}", protocol.payload_codec).unwrap();
    writeln!(
        out,
        "hash_domain CapabilityDecision {CAPABILITY_DECISION_HASH_DOMAIN}"
    )
    .unwrap();
    writeln!(
        out,
        "hash_domain RenderPlanManifest {RENDER_PLAN_MANIFEST_HASH_DOMAIN}"
    )
    .unwrap();
    writeln!(
        out,
        "max_data_segment_bytes {}",
        protocol.max_data_segment_bytes
    )
    .unwrap();
    writeln!(
        out,
        "max_data_ticket_bytes {}",
        protocol.max_data_ticket_bytes
    )
    .unwrap();
    writeln!(out, "byte_order little-endian").unwrap();
    writeln!(out, "header_bytes 20").unwrap();
    for (name, ty, offset, bytes) in [
        ("major", "u16", 0, 2),
        ("minor", "u16", 2, 2),
        ("message_type", "u16", 4, 2),
        ("flags", "u16", 6, 2),
        ("payload_len", "u32", 8, 4),
        ("sequence", "u64", 12, 8),
    ] {
        writeln!(out, "header_field {name} {ty} {offset} {bytes}").unwrap();
    }
    for scalar in &protocol.scalars {
        writeln!(
            out,
            "scalar {} {}",
            scalar.name,
            scalar.primitive.schema_name()
        )
        .unwrap();
    }
    for enumeration in &protocol.enums {
        writeln!(
            out,
            "enum {} {}",
            enumeration.name,
            enumeration.repr.schema_name()
        )
        .unwrap();
        for variant in &enumeration.variants {
            writeln!(
                out,
                "enum_variant {} {} {}",
                enumeration.name, variant.name, variant.tag
            )
            .unwrap();
        }
    }
    for record in &protocol.records {
        writeln!(out, "record {}", record.name).unwrap();
        for field in &record.fields {
            writeln!(
                out,
                "record_field {} {} {} {} {} {}",
                record.name,
                field.name,
                field.ty.schema_name(),
                if field.presence == Presence::Required {
                    "required"
                } else {
                    "optional"
                },
                match field.privacy {
                    Privacy::Public => "public",
                    Privacy::Private => "private",
                    Privacy::Sensitive => "sensitive",
                },
                type_limit(&field.ty)
            )
            .unwrap();
        }
    }
    for union in &protocol.unions {
        writeln!(out, "union {} {}", union.name, union.repr.schema_name()).unwrap();
        for variant in union
            .variants
            .iter()
            .filter(|variant| desktop_union_variant(variant))
        {
            writeln!(
                out,
                "union_variant {} {} {}",
                union.name, variant.name, variant.tag
            )
            .unwrap();
            if let Some(capability) = &variant.required_capability {
                writeln!(
                    out,
                    "union_variant_capability {} {} {}",
                    union.name, variant.name, capability
                )
                .unwrap();
            }
            for field in &variant.fields {
                writeln!(
                    out,
                    "union_variant_field {} {} {} {} {} {}",
                    union.name,
                    variant.name,
                    field.name,
                    field.ty.schema_name(),
                    match field.privacy {
                        Privacy::Public => "public",
                        Privacy::Private => "private",
                        Privacy::Sensitive => "sensitive",
                    },
                    type_limit(&field.ty)
                )
                .unwrap();
            }
        }
    }
    if let Some(surface) = protocol
        .unions
        .iter()
        .find(|union| union.name == "SurfaceTransport")
    {
        for variant in surface
            .variants
            .iter()
            .filter(|variant| desktop_union_variant(variant))
        {
            writeln!(
                out,
                "target_union_variant SurfaceTransport {}",
                variant.name
            )
            .unwrap();
        }
    }
    if let Some(capabilities) = protocol
        .enums
        .iter()
        .find(|definition| definition.name == "EndpointCapability")
    {
        for variant in &capabilities.variants {
            writeln!(out, "endpoint_capability {} {}", variant.name, variant.tag).unwrap();
        }
    }
    for message in &protocol.messages {
        writeln!(
            out,
            "message {} {} {} {} {} {} {} {} {} {} {} {}",
            message.kind.schema_name(),
            message.id,
            message.name,
            message.payload,
            message.state,
            message.correlation,
            message.allowed_flags,
            message.min_transfer_slots,
            message.max_transfer_slots,
            message.max_payload_bytes,
            message.required_capability.as_deref().unwrap_or("none"),
            message.disposition
        )
        .unwrap();
        let record = protocol
            .records
            .iter()
            .find(|record| record.name == message.payload)
            .expect("validated payload");
        for field in &record.fields {
            writeln!(
                out,
                "field {} {} {} {} {} {}",
                message.id,
                field.name,
                field.ty.schema_name(),
                if field.presence == Presence::Required {
                    "required"
                } else {
                    "optional"
                },
                match field.privacy {
                    Privacy::Public => "public",
                    Privacy::Private => "private",
                    Privacy::Sensitive => "sensitive",
                },
                type_limit(&field.ty)
            )
            .unwrap();
        }
        for outcome in &message.outcomes {
            let id = protocol
                .messages
                .iter()
                .find(|candidate| {
                    candidate.kind == MessageKind::Event && candidate.name == outcome.name
                })
                .expect("validated outcome")
                .id;
            writeln!(
                out,
                "outcome {} {} {}",
                message.id,
                id,
                outcome.disposition.schema_name()
            )
            .unwrap();
        }
    }
    out
}

fn generate_hash_registry(digest: &[u8; 32], canonical_digest: &[u8; 32]) -> String {
    format!(
        "algorithm sha256\ngenerator_version {GENERATOR_VERSION}\nwire_identity_domain {WIRE_IDENTITY_DOMAIN}\npayload_codec_abi_version {PAYLOAD_CODEC_ABI_VERSION}\ncanonical_source protocol/engine.protocol\ncanonical_schema_sha256 {}\nfull_sha256 {}\nwire_hash {}\nwire_hash_bytes 16\ntruncation_policy {SCHEMA_HASH_TRUNCATION}\n",
        lowercase_hex(canonical_digest),
        lowercase_hex(digest),
        lowercase_hex(&digest[..16])
    )
}

fn generate_compatibility_vectors(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let full = lowercase_hex(digest);
    let wire = lowercase_hex(&digest[..16]);
    let minimum_minor = protocol.minor;
    let mut fork_hash = digest[..16].to_vec();
    fork_hash[0] ^= 0xff;
    let fork_wire = lowercase_hex(&fork_hash);
    let endpoint_capabilities = protocol
        .enums
        .iter()
        .find(|definition| definition.name == "EndpointCapability")
        .expect("canonical schema has EndpointCapability");
    let known_capabilities = endpoint_capabilities
        .variants
        .iter()
        .fold(0_u64, |mask, variant| mask | u64::from(variant.tag));
    let first_capability = u64::from(endpoint_capabilities.variants[0].tag);
    let known_hex = format!("0x{known_capabilities:x}");
    let known_with_unknown_hex = format!("0x{:x}", known_capabilities | (1_u64 << 63));
    let missing_first_hex = format!("0x{:x}", known_capabilities & !first_capability);
    let first_hex = format!("0x{first_capability:x}");
    let mut vectors = vec![format!(
        "    {{\"name\":\"exact-schema\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{wire}\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x0\",\"expected\":\"ExactSchema\"}}",
        protocol.major, protocol.minor, protocol.major, protocol.minor
    )];
    if protocol.minor > 0 {
        vectors.push(format!(
            "    {{\"name\":\"unregistered-older-minor\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"11111111111111111111111111111111\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x0\",\"expected_error\":\"UnsupportedMinor\"}}",
            protocol.major,
            protocol.minor,
            protocol.major,
            protocol.minor - 1
        ));
    }
    if protocol.minor < u16::MAX {
        vectors.push(format!(
            "    {{\"name\":\"future-minor\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{fork_wire}\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x0\",\"expected_error\":\"UnsupportedMinor\"}}",
            protocol.major,
            protocol.minor,
            protocol.major,
            protocol.minor + 1
        ));
    }
    vectors.extend([
        format!(
            "    {{\"name\":\"same-minor-schema-fork\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{fork_wire}\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x0\",\"expected_error\":\"IncompatibleSchema\"}}",
            protocol.major, protocol.minor, protocol.major, protocol.minor
        ),
        format!(
            "    {{\"name\":\"unknown-optional-capability\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{wire}\",\"peer_supported\":\"{known_with_unknown_hex}\",\"peer_mandatory\":\"0x0\",\"expected\":\"ExactSchema\"}}",
            protocol.major, protocol.minor, protocol.major, protocol.minor
        ),
        format!(
            "    {{\"name\":\"unknown-mandatory-capability\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{wire}\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x8000000000000000\",\"expected_error\":\"UnknownMandatoryCapability\"}}",
            protocol.major, protocol.minor, protocol.major, protocol.minor
        ),
        format!(
            "    {{\"name\":\"mandatory-not-supported-by-endpoint\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{wire}\",\"peer_supported\":\"{missing_first_hex}\",\"peer_mandatory\":\"{first_hex}\",\"expected_error\":\"InvalidEndpointCapabilities\"}}",
            protocol.major, protocol.minor, protocol.major, protocol.minor
        ),
        format!(
            "    {{\"name\":\"major-mismatch\",\"local_major\":{},\"local_minor\":{},\"peer_major\":{},\"peer_minor\":{},\"peer_schema_hash\":\"{wire}\",\"peer_supported\":\"{known_hex}\",\"peer_mandatory\":\"0x0\",\"expected_error\":\"UnsupportedMajor\"}}",
            protocol.major,
            protocol.minor,
            if protocol.major == u16::MAX {
                protocol.major - 1
            } else {
                protocol.major + 1
            },
            protocol.minor
        ),
    ]);
    format!(
        "{{\n  \"generator_version\": \"{GENERATOR_VERSION}\",\n  \"wire_identity_domain\": \"{WIRE_IDENTITY_DOMAIN}\",\n  \"payload_codec_abi_version\": {PAYLOAD_CODEC_ABI_VERSION},\n  \"schema_sha256\": \"{full}\",\n  \"wire_schema_hash\": \"{wire}\",\n  \"truncation_policy\": \"{SCHEMA_HASH_TRUNCATION}\",\n  \"minimum_compatible_minor\": {minimum_minor},\n  \"vectors\": [\n{}\n  ]\n}}\n",
        vectors.join(",\n")
    )
}

fn generate_invalid_vectors(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let hello = protocol
        .messages
        .iter()
        .find(|message| message.name == "Hello")
        .expect("canonical schema has Hello");
    let header = desktop_header_hex(protocol.major, protocol.minor, hello.id, 0, 0, 1);
    let zero_sequence = desktop_header_hex(protocol.major, protocol.minor, hello.id, 0, 0, 0);
    let endpoint_capabilities = protocol
        .enums
        .iter()
        .find(|definition| definition.name == "EndpointCapability")
        .expect("canonical schema has EndpointCapability");
    let known_capabilities = endpoint_capabilities
        .variants
        .iter()
        .fold(0_u64, |mask, variant| mask | u64::from(variant.tag));
    let first_capability = u64::from(endpoint_capabilities.variants[0].tag);
    let vectors = [
        format!(
            "    {{\"name\":\"truncated-header\",\"frame_hex\":\"{}\",\"transfer_slots\":0,\"expected_error\":\"TruncatedHeader\"}}",
            &header[..18]
        ),
        format!(
            "    {{\"name\":\"payload-length-mismatch\",\"frame_hex\":\"{header}00\",\"transfer_slots\":0,\"expected_error\":\"FrameLengthMismatch\"}}"
        ),
        format!(
            "    {{\"name\":\"zero-sequence\",\"frame_hex\":\"{zero_sequence}\",\"transfer_slots\":0,\"expected_error\":\"NonMonotonicSequence\"}}"
        ),
        "    {\"name\":\"unknown-message\",\"message_type\":65535,\"expected_error\":\"UnknownMessage\"}".to_owned(),
        format!(
            "    {{\"name\":\"unsupported-flags\",\"message_type\":{},\"flags\":1,\"expected_error\":\"InvalidFlags\"}}",
            hello.id
        ),
        format!(
            "    {{\"name\":\"missing-required-correlation\",\"message_type\":{},\"correlation\":{{}},\"expected_error\":\"InvalidCorrelation\"}}",
            hello.id
        ),
        format!(
            "    {{\"name\":\"transfer-count-out-of-range\",\"message_type\":{},\"transfer_slots\":1,\"expected_error\":\"InvalidTransferCount\"}}",
            hello.id
        ),
        "    {\"name\":\"provide-data-duplicate-slot\",\"message_type\":4,\"transfer_slots\":2,\"slots\":[0,0],\"expected_error\":\"InvalidTransferBinding\"}".to_owned(),
        "    {\"name\":\"provide-data-zero-range\",\"range_start\":\"0\",\"range_len\":\"0\",\"byte_length\":\"0\",\"transfer_length\":\"0\",\"expected_error\":\"InvalidDataRange\"}".to_owned(),
        "    {\"name\":\"provide-data-range-overflow\",\"range_start\":\"18446744073709551615\",\"range_len\":\"1\",\"byte_length\":\"1\",\"transfer_length\":\"1\",\"expected_error\":\"NumericOverflow\"}".to_owned(),
        "    {\"name\":\"provide-data-length-mismatch\",\"range_start\":\"0\",\"range_len\":\"4\",\"byte_length\":\"3\",\"transfer_length\":\"3\",\"expected_error\":\"InvalidDataRange\"}".to_owned(),
        "    {\"name\":\"provide-data-transfer-length-mismatch\",\"range_start\":\"0\",\"range_len\":\"4\",\"byte_length\":\"4\",\"transfer_length\":\"3\",\"expected_error\":\"InvalidTransferBinding\"}".to_owned(),
        "    {\"name\":\"surface-stride-too-small\",\"width\":100,\"height\":1,\"stride\":1,\"byte_length\":\"1\",\"expected_error\":\"InvalidSurfaceLayout\"}".to_owned(),
        "    {\"name\":\"surface-range-overflow\",\"byte_offset\":\"18446744073709551615\",\"byte_length\":\"1\",\"region_length\":\"18446744073709551615\",\"expected_error\":\"NumericOverflow\"}".to_owned(),
        "    {\"name\":\"surface-reclaimed-missing-reason\",\"message_type\":113,\"payload\":{\"surface\":1},\"expected_error\":\"MissingRequiredField\"}".to_owned(),
        "    {\"name\":\"unknown-mandatory-capability\",\"mandatory\":\"0x8000000000000000\",\"expected_error\":\"UnknownMandatoryCapability\"}".to_owned(),
        format!(
            "    {{\"name\":\"mandatory-not-supported-by-endpoint\",\"supported\":\"0x{:x}\",\"mandatory\":\"0x{first_capability:x}\",\"expected_error\":\"InvalidEndpointCapabilities\"}}",
            known_capabilities & !first_capability
        ),
        "    {\"name\":\"silent-decision-truncation\",\"missing_total\":17,\"missing_count\":16,\"missing_completeness\":\"Complete\",\"expected_error\":\"InvalidCapabilityDecision\"}".to_owned(),
    ];
    format!(
        "{{\n  \"generator_version\": \"{GENERATOR_VERSION}\",\n  \"wire_identity_domain\": \"{WIRE_IDENTITY_DOMAIN}\",\n  \"payload_codec_abi_version\": {PAYLOAD_CODEC_ABI_VERSION},\n  \"schema_sha256\": \"{}\",\n  \"vectors\": [\n{}\n  ]\n}}\n",
        lowercase_hex(digest),
        vectors.join(",\n")
    )
}

fn generate_payload_codec_vectors(protocol: &Protocol, digest: &[u8; 32]) -> String {
    let maximum_message_bytes =
        usize::try_from(protocol.max_message_bytes).expect("u32 fits into usize");
    let codec = FixedLeCodec::new(
        protocol,
        CodecLimits::new(64, maximum_message_bytes, maximum_message_bytes),
    )
    .expect("parser validates fixed_le_v1 schema");

    let fail_data_absent = record([
        ("ticket", WireValue::U64(9)),
        ("expected", source_identity(0x11, 7)),
        ("observed", WireValue::Optional(None)),
        ("code", WireValue::Enum("Unavailable".into())),
        ("retryable", WireValue::Bool(true)),
    ]);
    let fail_data_present = record([
        ("ticket", WireValue::U64(10)),
        ("expected", source_identity(0x22, 8)),
        (
            "observed",
            WireValue::Optional(Some(Box::new(source_identity(0x33, 9)))),
        ),
        ("code", WireValue::Enum("SourceChanged".into())),
        ("retryable", WireValue::Bool(false)),
    ]);
    let need_data = record([
        ("ticket", WireValue::U64(12)),
        ("source", source_identity(0x44, 10)),
        (
            "ranges",
            WireValue::List(vec![
                record([("start", WireValue::U64(0)), ("len", WireValue::U64(16))]),
                record([("start", WireValue::U64(32)), ("len", WireValue::U64(8))]),
            ]),
        ),
        ("priority", WireValue::Enum("VisiblePage".into())),
        ("checkpoint", WireValue::U64(77)),
    ]);
    let shared_surface = WireValue::Union {
        variant: "BrowserSharedArrayBuffer".into(),
        fields: vec![
            ("attachment_slot".into(), WireValue::U16(2)),
            ("buffer_length".into(), WireValue::U64(4096)),
            ("fence_byte_offset".into(), WireValue::U64(4080)),
            ("publication_epoch".into(), WireValue::U32(3)),
        ],
    };

    let fail_absent_bytes = encode_vector(&codec, "FailDataCommand", &fail_data_absent);
    let fail_present_bytes = encode_vector(&codec, "FailDataCommand", &fail_data_present);
    let need_data_bytes = encode_vector(&codec, "NeedDataEvent", &need_data);
    let shared_surface_bytes = encode_vector(&codec, "SurfaceTransport", &shared_surface);
    let fail_message = message_id(protocol, "FailData");
    let need_data_message = message_id(protocol, "NeedData");
    let fail_frame_payload =
        encode_message_payload(&codec, "Session", "FailDataCommand", &fail_data_absent);
    let need_data_frame_payload =
        encode_message_payload(&codec, "SessionRequest", "NeedDataEvent", &need_data);
    let valid = [
        format!(
            "    {{\"name\":\"fail-data-absent-optional\",\"type\":\"FailDataCommand\",\"message_kind\":\"command\",\"message_type\":{fail_message},\"payload_hex\":\"{}\",\"frame_payload_hex\":\"{}\"}}",
            lowercase_hex(&fail_absent_bytes),
            lowercase_hex(&fail_frame_payload)
        ),
        format!(
            "    {{\"name\":\"fail-data-present-optional\",\"type\":\"FailDataCommand\",\"message_kind\":\"command\",\"message_type\":{fail_message},\"payload_hex\":\"{}\"}}",
            lowercase_hex(&fail_present_bytes)
        ),
        format!(
            "    {{\"name\":\"need-data-nested-list\",\"type\":\"NeedDataEvent\",\"message_kind\":\"event\",\"message_type\":{need_data_message},\"payload_hex\":\"{}\",\"frame_payload_hex\":\"{}\"}}",
            lowercase_hex(&need_data_bytes),
            lowercase_hex(&need_data_frame_payload)
        ),
        format!(
            "    {{\"name\":\"surface-tagged-union\",\"type\":\"SurfaceTransport\",\"payload_hex\":\"{}\"}}",
            lowercase_hex(&shared_surface_bytes)
        ),
    ];
    let hash_known_answers = payload_hash_known_answers(&codec)
        .into_iter()
        .map(|answer| {
            format!(
                "    {{\"type\":\"{}\",\"domain\":\"{}\",\"payload_hex\":\"{}\",\"preimage_hex\":\"{}\",\"sha256\":\"{}\"}}",
                answer.type_name,
                answer.domain,
                lowercase_hex(&answer.payload),
                lowercase_hex(&answer.preimage),
                lowercase_hex(&answer.sha256)
            )
        })
        .collect::<Vec<_>>();

    let mut invalid_optional = fail_absent_bytes.clone();
    invalid_optional[48] = 2;
    let mut invalid_boolean = fail_absent_bytes.clone();
    *invalid_boolean
        .last_mut()
        .expect("FailDataCommand contains a boolean") = 2;
    let mut invalid_union_tag = shared_surface_bytes.clone();
    invalid_union_tag[0] = u8::MAX;
    let mut over_limit_list = need_data_bytes.clone();
    over_limit_list[48..52].copy_from_slice(&17_u32.to_le_bytes());
    let mut impossible_list = need_data_bytes.clone();
    impossible_list[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut trailing = fail_absent_bytes.clone();
    trailing.push(0);
    let mut truncated = need_data_bytes.clone();
    truncated.pop();

    let invalid_cases = [
        (
            "noncanonical-optional-marker",
            "FailDataCommand",
            invalid_optional,
            CodecErrorKind::InvalidOptionalMarker,
        ),
        (
            "noncanonical-boolean-marker",
            "FailDataCommand",
            invalid_boolean,
            CodecErrorKind::InvalidBooleanMarker,
        ),
        (
            "unknown-union-tag",
            "SurfaceTransport",
            invalid_union_tag,
            CodecErrorKind::UnknownTag,
        ),
        (
            "list-count-above-schema-limit",
            "NeedDataEvent",
            over_limit_list,
            CodecErrorKind::LimitExceeded,
        ),
        (
            "list-count-impossible-for-remaining-input",
            "NeedDataEvent",
            impossible_list,
            CodecErrorKind::LimitExceeded,
        ),
        (
            "trailing-payload-byte",
            "FailDataCommand",
            trailing,
            CodecErrorKind::TrailingBytes,
        ),
        (
            "truncated-nested-payload",
            "NeedDataEvent",
            truncated,
            CodecErrorKind::Truncated,
        ),
    ];
    let invalid = invalid_cases
        .into_iter()
        .map(|(name, type_name, bytes, expected)| {
            let error = codec
                .decode_named(type_name, &bytes)
                .expect_err("invalid codec vector must fail");
            assert_eq!(error.kind, expected, "invalid vector {name}");
            format!(
                "    {{\"name\":\"{name}\",\"type\":\"{type_name}\",\"payload_hex\":\"{}\",\"expected_error\":\"{}\"}}",
                lowercase_hex(&bytes),
                codec_error_name(expected)
            )
        })
        .collect::<Vec<_>>();

    let maximum_payload_bytes = protocol
        .messages
        .iter()
        .map(|message| {
            let maximum = maximum_message_payload(&codec, message);
            assert!(maximum <= message.max_payload_bytes as usize);
            format!(
                "    {{\"message_type\":{},\"name\":\"{}\",\"payload_type\":\"{}\",\"schema_maximum\":{},\"declared_limit\":{}}}",
                message.id,
                message.name,
                message.payload,
                maximum,
                message.max_payload_bytes
            )
        })
        .collect::<Vec<_>>();

    format!(
        "{{\n  \"generator_version\": \"{GENERATOR_VERSION}\",\n  \"wire_identity_domain\": \"{WIRE_IDENTITY_DOMAIN}\",\n  \"payload_codec_abi_version\": {PAYLOAD_CODEC_ABI_VERSION},\n  \"schema_sha256\": \"{}\",\n  \"codec\": \"{}\",\n  \"byte_order\": \"little-endian\",\n  \"record_framing\": \"none\",\n  \"frame_payload_layout\": \"Correlation || message-specific payload\",\n  \"minor_compatibility\": \"exact-schema-layout-only\",\n  \"valid\": [\n{}\n  ],\n  \"hash_known_answers\": [\n{}\n  ],\n  \"invalid\": [\n{}\n  ],\n  \"maximum_payload_bytes\": [\n{}\n  ]\n}}\n",
        lowercase_hex(digest),
        protocol.payload_codec,
        valid.join(",\n"),
        hash_known_answers.join(",\n"),
        invalid.join(",\n"),
        maximum_payload_bytes.join(",\n")
    )
}

struct PayloadHashKnownAnswer {
    type_name: &'static str,
    domain: &'static str,
    payload: Vec<u8>,
    preimage: Vec<u8>,
    sha256: [u8; 32],
}

fn payload_hash_known_answers(codec: &FixedLeCodec<'_>) -> [PayloadHashKnownAnswer; 2] {
    let decision = capability_decision_hash_fixture();
    assert!(
        capability_decision_hash_fixture_wire_invariants_valid(&decision),
        "CapabilityDecision hash fixture must satisfy generated wire_invariants_valid"
    );
    let decision_payload = encode_vector(codec, "CapabilityDecision", &decision);
    let decision_preimage =
        frozen_payload_hash_preimage(CAPABILITY_DECISION_HASH_DOMAIN, &decision_payload);
    let decision_sha256 = sha256(&decision_preimage);

    let manifest = render_plan_manifest_hash_fixture(decision_sha256);
    assert!(
        render_plan_manifest_hash_fixture_wire_invariants_valid(&manifest),
        "RenderPlanManifest hash fixture must satisfy projection wire invariants"
    );
    let manifest_payload = encode_vector(codec, "RenderPlanManifest", &manifest);
    let manifest_preimage =
        frozen_payload_hash_preimage(RENDER_PLAN_MANIFEST_HASH_DOMAIN, &manifest_payload);
    let manifest_sha256 = sha256(&manifest_preimage);

    [
        PayloadHashKnownAnswer {
            type_name: "CapabilityDecision",
            domain: CAPABILITY_DECISION_HASH_DOMAIN,
            payload: decision_payload,
            preimage: decision_preimage,
            sha256: decision_sha256,
        },
        PayloadHashKnownAnswer {
            type_name: "RenderPlanManifest",
            domain: RENDER_PLAN_MANIFEST_HASH_DOMAIN,
            payload: manifest_payload,
            preimage: manifest_preimage,
            sha256: manifest_sha256,
        },
    ]
}

fn frozen_payload_hash_preimage(domain: &str, payload: &[u8]) -> Vec<u8> {
    let payload_len = u64::try_from(payload.len()).expect("bounded payload length fits u64");
    let capacity = domain
        .len()
        .checked_add(9)
        .and_then(|length| length.checked_add(payload.len()))
        .expect("bounded hash preimage length");
    let mut preimage = Vec::with_capacity(capacity);
    preimage.extend_from_slice(domain.as_bytes());
    preimage.push(0);
    preimage.extend_from_slice(&payload_len.to_le_bytes());
    preimage.extend_from_slice(payload);
    preimage
}

fn capability_decision_hash_fixture() -> WireValue {
    record([
        ("decision_schema_version", WireValue::U16(1)),
        ("status", WireValue::Enum("Supported".into())),
        ("profile", WireValue::Enum("BaselineNative".into())),
        ("profile_version", WireValue::U32(1)),
        ("policy_version", WireValue::U32(1)),
        (
            "subject",
            record([
                ("source", source_identity(0x55, 11)),
                ("document_revision", WireValue::U64(12)),
                ("revision_startxref", WireValue::U64(13)),
                ("page_index", WireValue::U32(2)),
                ("page_object_number", WireValue::U32(44)),
                ("page_object_generation", WireValue::U16(0)),
                ("scene_schema_major", WireValue::U16(1)),
                ("scene_schema_minor", WireValue::U16(0)),
                ("scene_hash", WireValue::Bytes32([0x77; 32])),
            ]),
        ),
        ("missing", WireValue::List(Vec::new())),
        ("missing_total", WireValue::U32(0)),
        ("missing_completeness", WireValue::Enum("Complete".into())),
        ("contributors", WireValue::List(Vec::new())),
        ("contributors_total", WireValue::U32(0)),
        (
            "contributors_completeness",
            WireValue::Enum("Complete".into()),
        ),
        (
            "scope",
            record([
                ("kind", WireValue::Enum("Page".into())),
                (
                    "page",
                    WireValue::Optional(Some(Box::new(WireValue::U32(2)))),
                ),
                ("command", WireValue::Optional(None)),
                ("resource", WireValue::Optional(None)),
            ]),
        ),
        ("location", WireValue::Optional(None)),
        ("rejection_code", WireValue::Optional(None)),
    ])
}

fn capability_decision_hash_fixture_wire_invariants_valid(value: &WireValue) -> bool {
    matches!(
        record_field(value, "status"),
        Some(WireValue::Enum(status)) if status == "Supported"
    ) && matches!(
        record_field(value, "missing"),
        Some(WireValue::List(missing)) if missing.is_empty()
    ) && matches!(
        record_field(value, "missing_total"),
        Some(WireValue::U32(0))
    ) && matches!(
        record_field(value, "missing_completeness"),
        Some(WireValue::Enum(completeness)) if completeness == "Complete"
    ) && matches!(
        record_field(value, "contributors"),
        Some(WireValue::List(contributors)) if contributors.is_empty()
    ) && matches!(
        record_field(value, "contributors_total"),
        Some(WireValue::U32(0))
    ) && matches!(
        record_field(value, "contributors_completeness"),
        Some(WireValue::Enum(completeness)) if completeness == "Complete"
    ) && matches!(
        record_field(value, "location"),
        Some(WireValue::Optional(None))
    ) && matches!(
        record_field(value, "rejection_code"),
        Some(WireValue::Optional(None))
    )
}

fn render_plan_manifest_hash_fixture(decision_hash: [u8; 32]) -> WireValue {
    record([
        ("document_revision", WireValue::U64(12)),
        ("render_config", WireValue::Bytes32([0x66; 32])),
        ("renderer_epoch", WireValue::U32(4)),
        ("plan_id", WireValue::U64(21)),
        ("scene_hash", WireValue::Bytes32([0x77; 32])),
        ("decision_hash", WireValue::Bytes32(decision_hash)),
        ("backend", WireValue::Enum("ReferenceCpu".into())),
        ("output_profile", WireValue::Enum("Srgb".into())),
        ("quality", WireValue::Enum("Full".into())),
        (
            "regions",
            WireValue::List(vec![record([
                ("page_index", WireValue::U32(2)),
                ("x", WireValue::I32(-10)),
                ("y", WireValue::I32(20)),
                ("width", WireValue::U32(640)),
                ("height", WireValue::U32(480)),
                (
                    "coordinate_space",
                    WireValue::Enum("DevicePixelsTopLeft".into()),
                ),
            ])]),
        ),
    ])
}

fn render_plan_manifest_hash_fixture_wire_invariants_valid(value: &WireValue) -> bool {
    let valid_region = match record_field(value, "regions") {
        Some(WireValue::List(regions)) if regions.len() == 1 => regions.iter().all(|region| {
            matches!(
                record_field(region, "width"),
                Some(WireValue::U32(width)) if *width != 0
            ) && matches!(
                record_field(region, "height"),
                Some(WireValue::U32(height)) if *height != 0
            )
        }),
        _ => false,
    };
    matches!(
        record_field(value, "plan_id"),
        Some(WireValue::U64(plan_id)) if *plan_id != 0
    ) && matches!(
        record_field(value, "output_profile"),
        Some(WireValue::Enum(profile)) if profile == "Srgb"
    ) && valid_region
}

fn record_field<'a>(value: &'a WireValue, name: &str) -> Option<&'a WireValue> {
    match value {
        WireValue::Record(fields) => fields
            .iter()
            .find_map(|(field_name, value)| (field_name == name).then_some(value)),
        _ => None,
    }
}

fn record<const N: usize>(fields: [(&str, WireValue); N]) -> WireValue {
    WireValue::Record(
        fields
            .into_iter()
            .map(|(name, value)| (name.into(), value))
            .collect(),
    )
}

fn correlation_value(shape: &str) -> WireValue {
    let present = |value| WireValue::Optional(Some(Box::new(WireValue::U64(value))));
    let absent = || WireValue::Optional(None);
    let (session, request, generation) = match shape {
        "Worker" => (absent(), absent(), absent()),
        "Session" => (present(2), absent(), absent()),
        "Request" => (present(2), present(3), absent()),
        "OpenRequest" => (absent(), present(3), absent()),
        "SessionRequest" => (present(2), present(3), absent()),
        "Generation" => (present(2), absent(), present(4)),
        other => panic!("validated correlation shape {other}"),
    };
    record([
        ("worker", WireValue::U64(1)),
        ("session", session),
        ("request", request),
        ("generation", generation),
    ])
}

fn encode_message_payload(
    codec: &FixedLeCodec<'_>,
    correlation_shape: &str,
    payload_type: &str,
    payload: &WireValue,
) -> Vec<u8> {
    let correlation = encode_vector(codec, "Correlation", &correlation_value(correlation_shape));
    let payload = encode_vector(codec, payload_type, payload);
    let mut output = Vec::with_capacity(correlation.len() + payload.len());
    output.extend_from_slice(&correlation);
    output.extend_from_slice(&payload);
    output
}

fn maximum_message_payload(codec: &FixedLeCodec<'_>, message: &crate::model::Message) -> usize {
    let correlation = codec
        .encode_named("Correlation", &correlation_value(&message.correlation))
        .expect("generated correlation shape matches schema")
        .len();
    let payload = codec
        .maximum_encoded_size(&Type::Named(message.payload.clone()))
        .expect("parser proves bounded payload");
    correlation
        .checked_add(payload)
        .expect("parser proves bounded envelope payload")
}

fn source_identity(stable_byte: u8, revision: u64) -> WireValue {
    record([
        ("stable_id", WireValue::Bytes32([stable_byte; 32])),
        ("revision", WireValue::U64(revision)),
    ])
}

fn encode_vector(codec: &FixedLeCodec<'_>, type_name: &str, value: &WireValue) -> Vec<u8> {
    let ty = Type::Named(type_name.into());
    let encoded = codec
        .encode(&ty, value)
        .expect("generator fixture matches canonical schema");
    assert_eq!(
        codec
            .decode(&ty, &encoded)
            .expect("generated fixture decodes"),
        *value
    );
    assert_eq!(
        codec
            .reencode(&ty, &encoded)
            .expect("generated fixture re-encodes"),
        encoded
    );
    assert_eq!(
        codec
            .reencode_named(type_name, &encoded)
            .expect("generated named fixture re-encodes"),
        encoded
    );
    encoded
}

fn message_id(protocol: &Protocol, name: &str) -> u16 {
    protocol
        .messages
        .iter()
        .find(|message| message.name == name)
        .unwrap_or_else(|| panic!("canonical schema has {name}"))
        .id
}

const fn codec_error_name(kind: CodecErrorKind) -> &'static str {
    match kind {
        CodecErrorKind::UnsupportedCodec => "UnsupportedCodec",
        CodecErrorKind::InvalidSchema => "InvalidSchema",
        CodecErrorKind::UnknownType => "UnknownType",
        CodecErrorKind::RecursiveType => "RecursiveType",
        CodecErrorKind::TypeMismatch => "TypeMismatch",
        CodecErrorKind::MissingField => "MissingField",
        CodecErrorKind::UnknownField => "UnknownField",
        CodecErrorKind::DuplicateField => "DuplicateField",
        CodecErrorKind::UnknownVariant => "UnknownVariant",
        CodecErrorKind::UnknownTag => "UnknownTag",
        CodecErrorKind::InvalidBooleanMarker => "InvalidBooleanMarker",
        CodecErrorKind::InvalidOptionalMarker => "InvalidOptionalMarker",
        CodecErrorKind::LimitExceeded => "LimitExceeded",
        CodecErrorKind::Truncated => "Truncated",
        CodecErrorKind::TrailingBytes => "TrailingBytes",
    }
}

fn desktop_header_hex(
    major: u16,
    minor: u16,
    message_type: u16,
    flags: u16,
    payload_len: u32,
    sequence: u64,
) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&major.to_le_bytes());
    bytes.extend_from_slice(&minor.to_le_bytes());
    bytes.extend_from_slice(&message_type.to_le_bytes());
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(&sequence.to_le_bytes());
    lowercase_hex(&bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        GeneratedFile, generate_compatibility_vectors, generated_files, write_generated,
        write_generated_transaction,
    };
    use crate::parser::parse_schema;
    use std::path::Path;

    const SCHEMA: &str = include_str!("../../../protocol/engine.protocol");

    #[test]
    fn generation_is_byte_deterministic_and_complete() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let first = generated_files(&protocol, SCHEMA);
        let second = generated_files(&protocol, SCHEMA);
        assert_eq!(first, second);
        assert_eq!(first.len(), 7);
        for path in [
            "runtime/protocol/src/generated.rs",
            "platform/browser/generated/engine-protocol.ts",
        ] {
            let contents = &first
                .iter()
                .find(|file| file.relative_path == Path::new(path))
                .unwrap()
                .contents;
            assert!(contents.ends_with('\n'));
            assert!(!contents.ends_with("\n\n"));
        }
        for required in [
            "runtime/protocol/src/generated.rs",
            "platform/browser/generated/engine-protocol.ts",
            "platform/desktop/generated/engine-protocol.registry",
            "protocol/generated/compatibility-vectors.json",
            "protocol/generated/invalid-vectors.json",
            "protocol/generated/payload-codec-vectors.json",
        ] {
            assert!(
                first
                    .iter()
                    .any(|file| file.relative_path == Path::new(required))
            );
        }
    }

    #[test]
    fn wire_identity_binds_codec_abi_and_is_not_the_raw_schema_digest() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let identity = super::wire_identity_digest(&protocol, SCHEMA);
        assert_ne!(identity, crate::hash::sha256(SCHEMA.as_bytes()));
        let registry =
            super::generate_hash_registry(&identity, &crate::hash::sha256(SCHEMA.as_bytes()));
        assert!(registry.contains("wire_identity_domain PDF.rs/EngineProtocol/WireIdentity/v1"));
        assert!(registry.contains("payload_codec_abi_version 1"));
        assert!(registry.contains("canonical_schema_sha256"));
    }

    #[test]
    fn compatibility_replay_has_stable_outcomes() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let vectors = generate_compatibility_vectors(&protocol, &[0x5a; 32]);
        assert!(vectors.contains("\"expected\":\"ExactSchema\""));
        assert!(!vectors.contains("\"expected\":\"CompatibleMinor\""));
        assert!(vectors.contains("\"generator_version\": \"0.2.0\""));
        assert!(vectors.contains("\"minimum_compatible_minor\": 2"));
        assert!(vectors.contains("\"name\":\"unregistered-older-minor\""));
        assert!(vectors.contains("\"name\":\"future-minor\""));
        assert!(vectors.contains("\"name\":\"same-minor-schema-fork\""));
        assert!(vectors.contains("\"peer_schema_hash\""));
        assert!(vectors.contains("\"expected_error\":\"UnknownMandatoryCapability\""));
        assert!(vectors.contains("\"expected_error\":\"InvalidEndpointCapabilities\""));
    }

    #[test]
    fn invalid_correlation_vector_replays_against_generated_shape() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let hello = protocol
            .messages
            .iter()
            .find(|message| message.name == "Hello")
            .unwrap();
        let requirements = super::correlation_shape(&hello.correlation);
        assert!(
            [
                requirements.0,
                requirements.1,
                requirements.2,
                requirements.3
            ]
            .into_iter()
            .any(|requirement| requirement == "Required"),
            "the empty correlation vector must be rejected"
        );
        let vectors = super::generate_invalid_vectors(&protocol, &[0x5a; 32]);
        assert!(vectors.contains("\"correlation\":{}"));
        assert!(vectors.contains("\"name\":\"provide-data-zero-range\""));
        assert!(vectors.contains("\"name\":\"provide-data-range-overflow\""));
        assert!(vectors.contains("\"name\":\"provide-data-length-mismatch\""));
        assert!(vectors.contains("\"name\":\"provide-data-transfer-length-mismatch\""));
    }

    #[test]
    fn payload_codec_vectors_bind_envelope_layout_and_wire_maxima() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let vectors = super::generate_payload_codec_vectors(&protocol, &[0x5a; 32]);
        assert!(
            vectors
                .contains("\"frame_payload_layout\": \"Correlation || message-specific payload\"")
        );
        assert!(vectors.contains("\"name\":\"fail-data-absent-optional\""));
        assert!(vectors.contains("\"name\":\"need-data-nested-list\""));
        assert!(vectors.contains("\"name\":\"surface-tagged-union\""));
        assert!(vectors.contains("\"expected_error\":\"InvalidOptionalMarker\""));
        assert!(vectors.contains("\"expected_error\":\"InvalidBooleanMarker\""));
        assert!(vectors.contains("\"expected_error\":\"UnknownTag\""));
        assert!(vectors.contains("\"expected_error\":\"TrailingBytes\""));
        assert!(vectors.contains("\"maximum_payload_bytes\""));
        assert!(vectors.contains("\"name\":\"GenerationPlanned\""));
    }

    #[test]
    fn payload_hash_known_answers_freeze_domain_length_payload_and_digest() {
        let protocol = parse_schema(SCHEMA).unwrap();
        let maximum_message_bytes =
            usize::try_from(protocol.max_message_bytes).expect("u32 fits into usize");
        let codec = crate::codec::FixedLeCodec::new(
            &protocol,
            crate::codec::CodecLimits::new(64, maximum_message_bytes, maximum_message_bytes),
        )
        .unwrap();
        let answers = super::payload_hash_known_answers(&codec);
        assert_eq!(answers[0].type_name, "CapabilityDecision");
        assert_eq!(answers[0].domain, super::CAPABILITY_DECISION_HASH_DOMAIN);
        assert_eq!(answers[1].type_name, "RenderPlanManifest");
        assert_eq!(answers[1].domain, super::RENDER_PLAN_MANIFEST_HASH_DOMAIN);
        assert!(
            super::capability_decision_hash_fixture_wire_invariants_valid(
                &super::capability_decision_hash_fixture()
            )
        );
        assert!(
            super::render_plan_manifest_hash_fixture_wire_invariants_valid(
                &super::render_plan_manifest_hash_fixture(answers[0].sha256)
            )
        );

        for answer in &answers {
            let domain_len = answer.domain.len();
            let payload_offset = domain_len + 1 + 8;
            assert_eq!(answer.preimage.len(), payload_offset + answer.payload.len());
            assert_eq!(
                &answer.preimage[..domain_len],
                answer.domain.as_bytes(),
                "{} domain",
                answer.type_name
            );
            assert_eq!(
                answer.preimage[domain_len], 0,
                "{} NUL separator",
                answer.type_name
            );
            let length_bytes: [u8; 8] = answer.preimage[domain_len + 1..payload_offset]
                .try_into()
                .unwrap();
            assert_eq!(
                u64::from_le_bytes(length_bytes),
                u64::try_from(answer.payload.len()).unwrap(),
                "{} u64LE payload length",
                answer.type_name
            );
            assert_eq!(
                &answer.preimage[payload_offset..],
                answer.payload.as_slice(),
                "{} payload",
                answer.type_name
            );
            assert_eq!(
                crate::hash::sha256(&answer.preimage),
                answer.sha256,
                "{} SHA-256",
                answer.type_name
            );
        }

        let vectors = super::generate_payload_codec_vectors(&protocol, &[0x5a; 32]);
        assert!(vectors.contains("\"hash_known_answers\""));
        for answer in &answers {
            let serialized = format!(
                "\"type\":\"{}\",\"domain\":\"{}\",\"payload_hex\":\"{}\",\"preimage_hex\":\"{}\",\"sha256\":\"{}\"",
                answer.type_name,
                answer.domain,
                crate::hash::lowercase_hex(&answer.payload),
                crate::hash::lowercase_hex(&answer.preimage),
                crate::hash::lowercase_hex(&answer.sha256)
            );
            assert!(vectors.contains(&serialized), "missing {serialized}");
        }
    }

    #[test]
    fn output_path_guard_rejects_parent_and_absolute_paths() {
        assert!(super::reject_symlink_path(Path::new("."), Path::new("../escape")).is_err());
        assert!(super::reject_symlink_path(Path::new("."), Path::new("/absolute")).is_err());
    }

    #[test]
    fn generated_set_replaces_existing_targets_and_rolls_back_injected_failure() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdf-rs-protocol-codegen-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("generated")).unwrap();
        let files = vec![
            GeneratedFile {
                relative_path: "generated/one.txt".into(),
                contents: "new-one\n".into(),
            },
            GeneratedFile {
                relative_path: "generated/two.txt".into(),
                contents: "new-two\n".into(),
            },
        ];
        std::fs::write(root.join("generated/one.txt"), b"old-one\n").unwrap();
        std::fs::write(root.join("generated/two.txt"), b"old-two\n").unwrap();

        assert!(write_generated_transaction(&root, &files, Some(1)).is_err());
        assert_eq!(
            std::fs::read(root.join("generated/one.txt")).unwrap(),
            b"old-one\n"
        );
        assert_eq!(
            std::fs::read(root.join("generated/two.txt")).unwrap(),
            b"old-two\n"
        );
        assert!(!root.join(".protocol-codegen.lock").exists());

        write_generated(&root, &files).unwrap();
        assert_eq!(
            std::fs::read(root.join("generated/one.txt")).unwrap(),
            b"new-one\n"
        );
        assert_eq!(
            std::fs::read(root.join("generated/two.txt")).unwrap(),
            b"new-two\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_generator_lock_fails_closed() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdf-rs-protocol-codegen-lock-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".protocol-codegen.lock"), b"held\n").unwrap();
        let files = [GeneratedFile {
            relative_path: "value.txt".into(),
            contents: "value\n".into(),
        }];
        assert!(write_generated(&root, &files).is_err());
        assert!(!root.join("value.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
