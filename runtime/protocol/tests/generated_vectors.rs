use std::collections::{BTreeMap, BTreeSet};

use pdf_rs_protocol::{
    DesktopFrameDecoder, EndpointCapabilities, EndpointRole, HandshakeCompatibility,
    MIN_COMPATIBLE_MINOR, PROTOCOL_GENERATOR_VERSION, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProtocolValidator, SCHEMA_HASH,
    SCHEMA_SHA256_HEX, SequenceTracker,
};

const COMPATIBILITY_VECTORS: &str =
    include_str!("../../../protocol/generated/compatibility-vectors.json");
const INVALID_VECTORS: &str = include_str!("../../../protocol/generated/invalid-vectors.json");
const DESKTOP_REGISTRY: &str =
    include_str!("../../../platform/desktop/generated/engine-protocol.registry");

#[derive(Clone, Debug, Eq, PartialEq)]
enum FlatValue {
    String(String),
    Number(u64),
}

#[test]
fn generated_compatibility_vectors_replay_against_rust_handshake() {
    assert_eq!(
        root_string(COMPATIBILITY_VECTORS, "generator_version"),
        PROTOCOL_GENERATOR_VERSION
    );
    assert_eq!(
        root_string(COMPATIBILITY_VECTORS, "schema_sha256"),
        SCHEMA_SHA256_HEX
    );
    assert_eq!(
        root_string(COMPATIBILITY_VECTORS, "wire_schema_hash"),
        hex(&SCHEMA_HASH)
    );
    assert_eq!(
        root_number(COMPATIBILITY_VECTORS, "minimum_compatible_minor"),
        u64::from(MIN_COMPATIBLE_MINOR)
    );

    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let mut replayed = BTreeSet::new();
    for vector in flat_vector_objects(COMPATIBILITY_VECTORS) {
        let name = string(&vector, "name");
        assert!(replayed.insert(name.to_owned()), "duplicate vector {name}");
        let local = ProtocolHello {
            major: number(&vector, "local_major") as u16,
            minor: number(&vector, "local_minor") as u16,
            schema_hash: SCHEMA_HASH,
            endpoint_role: EndpointRole::Host,
            capabilities: EndpointCapabilities {
                supported: 0x3f,
                mandatory: 0,
            },
            max_message_bytes: 1_048_576,
            max_transfer_slots: 8,
        };
        let peer = ProtocolHello {
            major: number(&vector, "peer_major") as u16,
            minor: number(&vector, "peer_minor") as u16,
            schema_hash: fixed_hex::<16>(string(&vector, "peer_schema_hash")),
            endpoint_role: EndpointRole::Engine,
            capabilities: EndpointCapabilities {
                supported: hex_u64(string(&vector, "peer_supported")),
                mandatory: hex_u64(string(&vector, "peer_mandatory")),
            },
            max_message_bytes: 1_048_576,
            max_transfer_slots: 8,
        };

        if let Some(expected) = optional_string(&vector, "expected") {
            let accepted = validator
                .validate_handshake(&local, &peer)
                .unwrap_or_else(|error| panic!("{name}: unexpected {error:?}"));
            assert_eq!(expected, "ExactSchema", "{name}");
            assert_eq!(
                accepted.compatibility(),
                HandshakeCompatibility::ExactSchema,
                "{name}"
            );
        } else {
            let expected = protocol_error(string(&vector, "expected_error"));
            assert_eq!(
                validator
                    .validate_handshake(&local, &peer)
                    .unwrap_err()
                    .code(),
                expected,
                "{name}"
            );
        }
    }
    assert_eq!(
        replayed,
        [
            "exact-schema",
            "future-minor",
            "major-mismatch",
            "mandatory-not-supported-by-endpoint",
            "same-minor-schema-fork",
            "unknown-mandatory-capability",
            "unknown-optional-capability",
            "unregistered-older-minor",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert_eq!(PROTOCOL_MAJOR, 0);
    assert_eq!(PROTOCOL_MINOR, MIN_COMPATIBLE_MINOR);
}

#[test]
fn generated_invalid_frame_vectors_replay_against_desktop_decoder() {
    assert_eq!(
        root_string(INVALID_VECTORS, "generator_version"),
        PROTOCOL_GENERATOR_VERSION
    );
    assert_eq!(
        root_string(INVALID_VECTORS, "schema_sha256"),
        SCHEMA_SHA256_HEX
    );
    let limits = ProtocolLimits::default();
    let validator = ProtocolValidator::new(limits);
    let hello = validator.frame_policy(1).unwrap();
    let mut replayed = BTreeSet::new();
    for vector in INVALID_VECTORS
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("{\"name\":") && line.contains("\"frame_hex\""))
        .map(parse_flat_object)
    {
        let name = string(&vector, "name");
        assert!(replayed.insert(name.to_owned()), "duplicate vector {name}");
        let frame = bytes_hex(string(&vector, "frame_hex"));
        let mut sequence = SequenceTracker::new();
        let error = DesktopFrameDecoder::current(limits)
            .decode(
                &frame,
                number(&vector, "transfer_slots") as usize,
                hello,
                &mut sequence,
            )
            .unwrap_err();
        assert_eq!(
            error.code(),
            protocol_error(string(&vector, "expected_error")),
            "{name}"
        );
        assert_eq!(sequence.last_accepted(), None, "{name}");
    }
    assert_eq!(
        replayed,
        [
            "payload-length-mismatch",
            "truncated-header",
            "zero-sequence"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn desktop_registry_contains_nested_codec_tags_fields_privacy_and_outcomes() {
    for exact in [
        "generator_version 0.1.0",
        "compatible_minor_min 2",
        "enum SurfaceReclaimReason u8",
        "enum_variant SurfaceReclaimReason ReleasedByHost 1",
        "record SurfaceMetadata",
        "record_field SurfaceMetadata plan_hash RenderPlanHash required public 0",
        "union SurfaceTransport u8",
        "union_variant SurfaceTransport OffscreenCanvasCommit 1",
        "union_variant SurfaceTransport BrowserTransfer 2",
        "union_variant SurfaceTransport SharedMemory 3",
        "union_variant SurfaceTransport LocalMemory 4",
        "union_variant_field SurfaceTransport SharedMemory handle PlatformHandle sensitive 0",
        "union_variant_field SurfaceTransport SharedMemory release_token u64 sensitive 0",
        "outcome 9 113",
    ] {
        assert_eq!(
            DESKTOP_REGISTRY
                .lines()
                .filter(|line| *line == exact)
                .count(),
            1,
            "{exact}"
        );
    }
}

fn flat_vector_objects(document: &str) -> Vec<BTreeMap<String, FlatValue>> {
    document
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("{\"name\":"))
        .map(parse_flat_object)
        .collect()
}

fn parse_flat_object(line: &str) -> BTreeMap<String, FlatValue> {
    let line = line.strip_suffix(',').unwrap_or(line);
    let body = line
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .expect("generated vector is one flat object");
    let mut output = BTreeMap::new();
    for entry in body.split(',') {
        let (raw_key, raw_value) = entry
            .split_once(':')
            .expect("generated flat vector entry has a colon");
        let key = unquote(raw_key);
        let value = if raw_value.starts_with('"') {
            FlatValue::String(unquote(raw_value).to_owned())
        } else {
            FlatValue::Number(raw_value.parse().expect("generated number"))
        };
        assert!(output.insert(key.to_owned(), value).is_none());
    }
    output
}

fn root_string<'a>(document: &'a str, key: &str) -> &'a str {
    let prefix = format!("  \"{key}\": \"");
    let line = document
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing root string {key}"));
    line.strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix("\","))
        .unwrap_or_else(|| panic!("invalid root string {key}"))
}

fn root_number(document: &str, key: &str) -> u64 {
    let prefix = format!("  \"{key}\": ");
    let line = document
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing root number {key}"));
    line.strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(','))
        .unwrap_or_else(|| panic!("invalid root number {key}"))
        .parse()
        .expect("root number")
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .expect("generated JSON string has quotes")
}

fn string<'a>(object: &'a BTreeMap<String, FlatValue>, key: &str) -> &'a str {
    match object.get(key) {
        Some(FlatValue::String(value)) => value,
        _ => panic!("missing generated string {key}"),
    }
}

fn optional_string<'a>(object: &'a BTreeMap<String, FlatValue>, key: &str) -> Option<&'a str> {
    match object.get(key) {
        Some(FlatValue::String(value)) => Some(value),
        None => None,
        Some(FlatValue::Number(_)) => panic!("generated {key} is not a string"),
    }
}

fn number(object: &BTreeMap<String, FlatValue>, key: &str) -> u64 {
    match object.get(key) {
        Some(FlatValue::Number(value)) => *value,
        _ => panic!("missing generated number {key}"),
    }
}

fn protocol_error(value: &str) -> ProtocolErrorCode {
    match value {
        "TruncatedHeader" => ProtocolErrorCode::TruncatedHeader,
        "FrameLengthMismatch" => ProtocolErrorCode::FrameLengthMismatch,
        "NonMonotonicSequence" => ProtocolErrorCode::NonMonotonicSequence,
        "UnsupportedMajor" => ProtocolErrorCode::UnsupportedMajor,
        "UnsupportedMinor" => ProtocolErrorCode::UnsupportedMinor,
        "IncompatibleSchema" => ProtocolErrorCode::IncompatibleSchema,
        "UnknownMandatoryCapability" => ProtocolErrorCode::UnknownMandatoryCapability,
        "InvalidEndpointCapabilities" => ProtocolErrorCode::InvalidEndpointCapabilities,
        _ => panic!("unregistered generated error {value}"),
    }
}

fn hex_u64(value: &str) -> u64 {
    u64::from_str_radix(value.strip_prefix("0x").expect("generated hex prefix"), 16)
        .expect("generated u64 hex")
}

fn fixed_hex<const N: usize>(value: &str) -> [u8; N] {
    let bytes = bytes_hex(value);
    bytes.try_into().unwrap_or_else(|bytes: Vec<u8>| {
        panic!("expected {N} generated bytes, got {}", bytes.len())
    })
}

fn bytes_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "generated hex length");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("generated ASCII hex");
            u8::from_str_radix(pair, 16).expect("generated hex byte")
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(output, "{byte:02x}").unwrap();
    }
    output
}
