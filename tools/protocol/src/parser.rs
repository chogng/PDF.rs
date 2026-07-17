use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::codec::{CodecErrorKind, CodecLimits, FixedLeCodec};
use crate::model::{
    EnumDef, Field, Message, MessageKind, Outcome, OutcomeDisposition, Presence, Primitive,
    Privacy, Protocol, Record, Scalar, Type, Union, UnionField, UnionVariant, Variant,
};

pub(crate) const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_DEFINITIONS: usize = 1024;
const MAX_FIELDS: usize = 256;
const MAX_TYPE_DEPTH: usize = 32;
const MAX_WIRE_GRAPH_DEPTH: usize = 64;
const STATE_PRECONDITIONS: &[&str] = &[
    "ActiveOrTerminalRequest",
    "Any",
    "Closing",
    "DrainingOrStopped",
    "NonClosedOrClosed",
    "Opening",
    "OpeningOrReady",
    "Ready",
    "ReadyOrClosing",
    "ReadyOrDrainingOrStopped",
    "Starting",
    "SurfaceAliveOrReclaimed",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaError {
    line: usize,
    code: &'static str,
    detail: String,
}

impl SchemaError {
    fn new(line: usize, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            line,
            code,
            detail: detail.into(),
        }
    }

    #[cfg(test)]
    const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} line={} detail={}",
            self.code, self.line, self.detail
        )
    }
}

impl std::error::Error for SchemaError {}

pub fn parse_schema(input: &str) -> Result<Protocol, SchemaError> {
    if input.len() > MAX_SCHEMA_BYTES {
        return Err(SchemaError::new(0, "RPE-PROTOCOL-SCHEMA-0001", "too-large"));
    }
    if !input.is_ascii() || !input.ends_with('\n') || input.contains('\r') {
        return Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0002",
            "schema-must-be-ascii-lf-terminated",
        ));
    }
    let lines: Vec<&str> = input.lines().collect();
    let mut cursor = Cursor::new(&lines);
    let (name, major, minor) = parse_header(&mut cursor)?;
    let max_message_bytes = parse_limit(&mut cursor, "max_message_bytes")?
        .try_into()
        .map_err(|_| cursor.error("RPE-PROTOCOL-SCHEMA-0003", "message-limit-overflow"))?;
    let max_transfer_slots = parse_limit(&mut cursor, "max_transfer_slots")?
        .try_into()
        .map_err(|_| cursor.error("RPE-PROTOCOL-SCHEMA-0003", "slot-limit-overflow"))?;
    let max_data_segment_bytes = parse_limit(&mut cursor, "max_data_segment_bytes")?;
    let max_data_ticket_bytes = parse_limit(&mut cursor, "max_data_ticket_bytes")?;
    let payload_codec = parse_codec(&mut cursor)?;
    if max_message_bytes == 0
        || max_transfer_slots == 0
        || max_data_segment_bytes == 0
        || max_data_ticket_bytes == 0
        || max_data_segment_bytes > max_data_ticket_bytes
    {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0004", "zero-limit"));
    }

    let mut scalars = Vec::new();
    let mut enums = Vec::new();
    let mut records = Vec::new();
    let mut unions = Vec::new();
    let mut messages = Vec::new();
    while let Some(line) = cursor.next_nonblank()? {
        let words: Vec<&str> = line.split(' ').collect();
        match words.first().copied() {
            Some("scalar") => scalars.push(parse_scalar(&cursor, &words)?),
            Some("enum") => enums.push(parse_enum(&mut cursor, &words)?),
            Some("record") => records.push(parse_record(&mut cursor, &words)?),
            Some("union") => unions.push(parse_union(&mut cursor, &words)?),
            Some("message") => messages.push(parse_message(&cursor, &words)?),
            _ => {
                return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0005", "unknown-top-level-directive"));
            }
        }
        if scalars.len() + enums.len() + records.len() + unions.len() + messages.len()
            > MAX_DEFINITIONS
        {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0001", "too-many-definitions"));
        }
    }
    let protocol = Protocol {
        name,
        major,
        minor,
        max_message_bytes,
        max_transfer_slots,
        max_data_segment_bytes,
        max_data_ticket_bytes,
        payload_codec,
        scalars,
        enums,
        records,
        unions,
        messages,
    };
    validate(&protocol)?;
    validate_wire_contract(&protocol)?;
    if protocol.canonical_text() != input {
        return Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0006",
            "non-canonical-layout",
        ));
    }
    Ok(protocol)
}

fn parse_codec(cursor: &mut Cursor<'_>) -> Result<String, SchemaError> {
    let line = cursor.required_nonblank()?;
    let words: Vec<&str> = line.split(' ').collect();
    exact_words(cursor, &words, 2)?;
    if words[0] != "codec" || words[1] != "fixed_le_v1" {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0037", "unsupported-payload-codec"));
    }
    Ok(words[1].into())
}

fn parse_header(cursor: &mut Cursor<'_>) -> Result<(String, u16, u16), SchemaError> {
    let line = cursor.required_nonblank()?;
    let words: Vec<&str> = line.split(' ').collect();
    exact_words(cursor, &words, 4)?;
    if words[0] != "protocol" || !valid_name(words[1]) {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0007", "invalid-protocol-header"));
    }
    Ok((
        words[1].into(),
        parse_number(cursor, words[2])?,
        parse_number(cursor, words[3])?,
    ))
}

fn parse_limit(cursor: &mut Cursor<'_>, expected: &str) -> Result<u64, SchemaError> {
    let line = cursor.required_nonblank()?;
    let words: Vec<&str> = line.split(' ').collect();
    exact_words(cursor, &words, 3)?;
    if words[0] != "limit" || words[1] != expected {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0008", expected));
    }
    parse_number(cursor, words[2])
}

fn parse_scalar(cursor: &Cursor<'_>, words: &[&str]) -> Result<Scalar, SchemaError> {
    exact_words(cursor, words, 3)?;
    require_name(cursor, words[1])?;
    Ok(Scalar {
        name: words[1].into(),
        primitive: parse_primitive(cursor, words[2])?,
    })
}

fn parse_enum(cursor: &mut Cursor<'_>, words: &[&str]) -> Result<EnumDef, SchemaError> {
    exact_words(cursor, words, 3)?;
    require_name(cursor, words[1])?;
    let repr = parse_primitive(cursor, words[2])?;
    let mut variants = Vec::new();
    loop {
        let line = cursor.required_line()?;
        if line == "end" {
            break;
        }
        let fields: Vec<&str> = line.split(' ').collect();
        exact_words(cursor, &fields, 3)?;
        if fields[0] != "variant" {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0009", "expected-variant"));
        }
        require_name(cursor, fields[1])?;
        variants.push(Variant {
            name: fields[1].into(),
            tag: parse_number(cursor, fields[2])?,
        });
        if variants.len() > MAX_FIELDS {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0001", "too-many-variants"));
        }
    }
    if variants.is_empty() {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0010", "empty-enum"));
    }
    Ok(EnumDef {
        name: words[1].into(),
        repr,
        variants,
    })
}

fn parse_record(cursor: &mut Cursor<'_>, words: &[&str]) -> Result<Record, SchemaError> {
    exact_words(cursor, words, 2)?;
    require_name(cursor, words[1])?;
    let mut fields = Vec::new();
    loop {
        let line = cursor.required_line()?;
        if line == "end" {
            break;
        }
        let parts: Vec<&str> = line.split(' ').collect();
        exact_words(cursor, &parts, 5)?;
        if parts[0] != "field" || !valid_field_name(parts[1]) {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0011", "invalid-field"));
        }
        let presence = match parts[3] {
            "required" => Presence::Required,
            "optional" => Presence::Optional,
            _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0012", "invalid-presence")),
        };
        let privacy = parse_privacy(cursor, parts[4])?;
        let ty = parse_type(cursor, parts[2])?;
        if (presence == Presence::Optional) != matches!(ty, Type::Optional(_)) {
            return Err(cursor.error(
                "RPE-PROTOCOL-SCHEMA-0014",
                "optional-presence-type-mismatch",
            ));
        }
        fields.push(Field {
            name: parts[1].into(),
            ty,
            presence,
            privacy,
        });
        if fields.len() > MAX_FIELDS {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0001", "too-many-fields"));
        }
    }
    Ok(Record {
        name: words[1].into(),
        fields,
    })
}

fn parse_union(cursor: &mut Cursor<'_>, words: &[&str]) -> Result<Union, SchemaError> {
    exact_words(cursor, words, 3)?;
    require_name(cursor, words[1])?;
    let repr = parse_primitive(cursor, words[2])?;
    let mut variants = Vec::new();
    loop {
        let line = cursor.required_line()?;
        if line == "end" {
            break;
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if !(3..=5).contains(&parts.len()) || parts[0] != "variant" {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-variant"));
        }
        require_name(cursor, parts[1])?;
        let mut fields = Vec::new();
        let (field_list, required_capability) = match parts.as_slice() {
            [_, _, _] => (None, None),
            [_, _, _, annotation] if annotation.starts_with("requires=") => {
                (None, Some(parse_capability_annotation(cursor, annotation)?))
            }
            [_, _, _, list] => (Some(*list), None),
            [_, _, _, list, annotation] => (
                Some(*list),
                Some(parse_capability_annotation(cursor, annotation)?),
            ),
            _ => unreachable!("union arity checked"),
        };
        if let Some(list) = field_list {
            for pair in list.split(',') {
                let mut components = pair.split(':');
                let field = components.next().unwrap_or_default();
                let ty = components.next().ok_or_else(|| {
                    cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-field")
                })?;
                let privacy = components.next().ok_or_else(|| {
                    cursor.error("RPE-PROTOCOL-SCHEMA-0015", "missing-union-field-privacy")
                })?;
                if components.next().is_some() {
                    return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-field"));
                }
                if !valid_field_name(field) {
                    return Err(
                        cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-field-name")
                    );
                }
                if field == "kind" {
                    return Err(
                        cursor.error("RPE-PROTOCOL-SCHEMA-0015", "union-field-kind-is-reserved")
                    );
                }
                fields.push(UnionField {
                    name: field.into(),
                    ty: parse_type(cursor, ty)?,
                    privacy: parse_privacy(cursor, privacy)?,
                });
                if fields.len() > MAX_FIELDS {
                    return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0001", "too-many-union-fields"));
                }
            }
        }
        variants.push(UnionVariant {
            name: parts[1].into(),
            tag: parse_number(cursor, parts[2])?,
            required_capability,
            fields,
        });
        if variants.len() > MAX_FIELDS {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0001", "too-many-union-variants"));
        }
    }
    if variants.is_empty() {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0016", "empty-union"));
    }
    Ok(Union {
        name: words[1].into(),
        repr,
        variants,
    })
}

fn parse_message(cursor: &Cursor<'_>, words: &[&str]) -> Result<Message, SchemaError> {
    if words.len() < 12 || words.len() > 14 {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-shape"));
    }
    let kind = match words[1] {
        "command" => MessageKind::Command,
        "event" => MessageKind::Event,
        _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-kind")),
    };
    let has_capability = match kind {
        MessageKind::Command => words.len() == 14,
        MessageKind::Event => words.len() == 13,
    };
    let expected_without_capability = match kind {
        MessageKind::Command => 13,
        MessageKind::Event => 12,
    };
    if words.len() != expected_without_capability + usize::from(has_capability) {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "message-kind-shape-mismatch"));
    }
    for word in [words[2], words[4], words[5], words[6]] {
        if !valid_name(word) {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-token"));
        }
    }
    let disposition = words[7];
    match (kind, disposition) {
        (MessageKind::Command, "yes" | "no") | (MessageKind::Event, "event") => {}
        _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-disposition")),
    }
    let required_capability = if has_capability {
        Some(parse_capability_annotation(cursor, words[12])?)
    } else {
        None
    };
    let outcomes = if kind == MessageKind::Command {
        let raw = words[12 + usize::from(has_capability)];
        if raw == "none" {
            Vec::new()
        } else {
            raw.split(',')
                .map(|value| {
                    let (name, disposition) = value.split_once(':').ok_or_else(|| {
                        cursor.error("RPE-PROTOCOL-SCHEMA-0042", "outcome-missing-disposition")
                    })?;
                    require_name(cursor, name)?;
                    let disposition = match disposition {
                        "stream" => OutcomeDisposition::Stream,
                        "terminal" => OutcomeDisposition::Terminal,
                        _ => {
                            return Err(cursor
                                .error("RPE-PROTOCOL-SCHEMA-0042", "invalid-outcome-disposition"));
                        }
                    };
                    Ok(Outcome {
                        name: name.into(),
                        disposition,
                    })
                })
                .collect::<Result<Vec<_>, SchemaError>>()?
        }
    } else {
        Vec::new()
    };
    Ok(Message {
        kind,
        name: words[2].into(),
        id: parse_number(cursor, words[3])?,
        payload: words[4].into(),
        state: words[5].into(),
        correlation: words[6].into(),
        disposition: disposition.into(),
        allowed_flags: parse_number(cursor, words[8])?,
        min_transfer_slots: parse_number(cursor, words[9])?,
        max_transfer_slots: parse_number(cursor, words[10])?,
        max_payload_bytes: parse_number(cursor, words[11])?,
        required_capability,
        outcomes,
    })
}

fn validate(protocol: &Protocol) -> Result<(), SchemaError> {
    let mut names = BTreeSet::new();
    let mut types = BTreeSet::new();
    for primitive in [
        "u8", "u16", "u32", "u64", "i32", "bool", "bytes16", "bytes32",
    ] {
        types.insert(primitive.to_owned());
    }
    for name in protocol
        .scalars
        .iter()
        .map(|value| &value.name)
        .chain(protocol.enums.iter().map(|value| &value.name))
        .chain(protocol.records.iter().map(|value| &value.name))
        .chain(protocol.unions.iter().map(|value| &value.name))
    {
        if !names.insert(name.clone()) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0018",
                format!("duplicate-type-{name}"),
            ));
        }
        types.insert(name.clone());
    }
    for scalar in &protocol.scalars {
        let _ = scalar;
    }
    for enumeration in &protocol.enums {
        validate_unsigned_repr(enumeration.repr, &enumeration.name)?;
        validate_variants(
            &enumeration
                .variants
                .iter()
                .map(|value| (&value.name, value.tag))
                .collect::<Vec<_>>(),
        )?;
        validate_variant_tags(
            enumeration.repr,
            &enumeration
                .variants
                .iter()
                .map(|value| value.tag)
                .collect::<Vec<_>>(),
            &enumeration.name,
        )?;
    }
    for record in &protocol.records {
        let mut fields = BTreeSet::new();
        for field in &record.fields {
            if !fields.insert(&field.name) {
                return Err(SchemaError::new(
                    0,
                    "RPE-PROTOCOL-SCHEMA-0019",
                    format!("duplicate-field-{}-{}", record.name, field.name),
                ));
            }
            validate_type(&field.ty, &types)?;
        }
    }
    for union in &protocol.unions {
        validate_unsigned_repr(union.repr, &union.name)?;
        validate_variants(
            &union
                .variants
                .iter()
                .map(|value| (&value.name, value.tag))
                .collect::<Vec<_>>(),
        )?;
        validate_variant_tags(
            union.repr,
            &union
                .variants
                .iter()
                .map(|value| value.tag)
                .collect::<Vec<_>>(),
            &union.name,
        )?;
        for variant in &union.variants {
            validate_required_capability(protocol, variant.required_capability.as_deref())?;
            let mut fields = BTreeSet::new();
            for field in &variant.fields {
                if !fields.insert(&field.name) {
                    return Err(SchemaError::new(
                        0,
                        "RPE-PROTOCOL-SCHEMA-0019",
                        format!("duplicate-union-field-{}-{}", union.name, field.name),
                    ));
                }
                validate_type(&field.ty, &types)?;
            }
        }
    }
    let record_names: BTreeSet<_> = protocol.records.iter().map(|value| &value.name).collect();
    let mut ids = BTreeSet::new();
    let mut message_names = BTreeSet::new();
    let mut events = BTreeMap::new();
    for message in &protocol.messages {
        validate_required_capability(protocol, message.required_capability.as_deref())?;
        if message.id == 0 || !ids.insert(message.id) || !message_names.insert(&message.name) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0020",
                format!("duplicate-or-zero-message-{}", message.name),
            ));
        }
        if !record_names.contains(&message.payload) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0021",
                format!("unknown-payload-{}", message.payload),
            ));
        }
        if message.max_payload_bytes == 0 || message.max_payload_bytes > protocol.max_message_bytes
        {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0022",
                format!("invalid-message-limit-{}", message.name),
            ));
        }
        if !STATE_PRECONDITIONS.contains(&message.state.as_str()) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0043",
                format!("unknown-state-precondition-{}", message.state),
            ));
        }
        if message.min_transfer_slots > message.max_transfer_slots
            || message.max_transfer_slots > protocol.max_transfer_slots
        {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0033",
                format!("invalid-transfer-slot-bounds-{}", message.name),
            ));
        }
        if message.kind == MessageKind::Event {
            events.insert(&message.name, message);
        }
        if !matches!(
            message.correlation.as_str(),
            "Worker" | "Session" | "Request" | "OpenRequest" | "SessionRequest" | "Generation"
        ) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0034",
                format!("unknown-correlation-shape-{}", message.correlation),
            ));
        }
    }
    for command in protocol
        .messages
        .iter()
        .filter(|value| value.kind == MessageKind::Command)
    {
        for outcome in &command.outcomes {
            let Some(event) = events.get(&outcome.name) else {
                return Err(SchemaError::new(
                    0,
                    "RPE-PROTOCOL-SCHEMA-0023",
                    format!("unknown-outcome-{}", outcome.name),
                ));
            };
            if !outcome_correlation_constructible(&command.correlation, &event.correlation) {
                return Err(SchemaError::new(
                    0,
                    "RPE-PROTOCOL-SCHEMA-0041",
                    format!(
                        "unconstructible-outcome-correlation-{}-{}",
                        command.name, event.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_wire_contract(protocol: &Protocol) -> Result<(), SchemaError> {
    let codec = FixedLeCodec::new(
        protocol,
        CodecLimits::new(
            MAX_WIRE_GRAPH_DEPTH,
            usize::try_from(protocol.max_message_bytes).unwrap_or(usize::MAX),
            usize::MAX,
        ),
    )
    .map_err(codec_schema_error)?;
    for message in &protocol.messages {
        let payload_maximum = codec
            .maximum_named_encoded_size(&message.payload)
            .map_err(codec_schema_error)?;
        let maximum = payload_maximum
            .checked_add(correlation_wire_maximum(&message.correlation))
            .ok_or_else(|| {
                SchemaError::new(
                    0,
                    "RPE-PROTOCOL-SCHEMA-0045",
                    format!("message-wire-maximum-overflow-{}", message.name),
                )
            })?;
        if maximum > usize::try_from(message.max_payload_bytes).unwrap_or(usize::MAX) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0045",
                format!(
                    "message-wire-maximum-exceeds-limit-{}-{maximum}-{}",
                    message.name, message.max_payload_bytes
                ),
            ));
        }
    }
    Ok(())
}

fn correlation_wire_maximum(shape: &str) -> usize {
    match shape {
        // WorkerId plus the three canonical optional markers.
        "Worker" => 11,
        // WorkerId, one present optional scalar, and two absent markers.
        "Session" | "OpenRequest" => 19,
        // WorkerId, two present optional scalars, and one absent marker.
        "Request" | "SessionRequest" | "Generation" => 27,
        other => panic!("validated correlation shape {other}"),
    }
}

fn codec_schema_error(error: crate::codec::CodecError) -> SchemaError {
    let code = match error.kind {
        CodecErrorKind::RecursiveType => "RPE-PROTOCOL-SCHEMA-0044",
        CodecErrorKind::LimitExceeded => "RPE-PROTOCOL-SCHEMA-0045",
        _ => "RPE-PROTOCOL-SCHEMA-0046",
    };
    SchemaError::new(0, code, error.to_string())
}

fn outcome_correlation_constructible(command: &str, event: &str) -> bool {
    event == "Worker"
        || command == event
        || matches!(
            (command, event),
            ("OpenRequest", "SessionRequest" | "Request") | ("SessionRequest", "Request")
        )
}

fn validate_required_capability(
    protocol: &Protocol,
    required: Option<&str>,
) -> Result<(), SchemaError> {
    let Some(required) = required else {
        return Ok(());
    };
    let known = protocol
        .enums
        .iter()
        .find(|value| value.name == "EndpointCapability")
        .is_some_and(|value| {
            value
                .variants
                .iter()
                .any(|variant| variant.name == required)
        });
    if known {
        Ok(())
    } else {
        Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0038",
            format!("unknown-required-capability-{required}"),
        ))
    }
}

fn validate_variants(variants: &[(&String, u16)]) -> Result<(), SchemaError> {
    let mut names = BTreeSet::new();
    let mut tags = BTreeSet::new();
    for (name, tag) in variants {
        if *tag == 0 || !names.insert(*name) || !tags.insert(*tag) {
            return Err(SchemaError::new(
                0,
                "RPE-PROTOCOL-SCHEMA-0024",
                "duplicate-or-zero-variant",
            ));
        }
    }
    Ok(())
}

fn validate_unsigned_repr(repr: Primitive, name: &str) -> Result<(), SchemaError> {
    if matches!(
        repr,
        Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::U64
    ) {
        Ok(())
    } else {
        Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0035",
            format!("invalid-enum-or-union-repr-{name}"),
        ))
    }
}

fn validate_variant_tags(repr: Primitive, tags: &[u16], name: &str) -> Result<(), SchemaError> {
    let maximum = match repr {
        Primitive::U8 => u64::from(u8::MAX),
        Primitive::U16 => u64::from(u16::MAX),
        Primitive::U32 => u64::from(u32::MAX),
        Primitive::U64 => u64::MAX,
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
            return validate_unsigned_repr(repr, name);
        }
    };
    if tags.iter().all(|tag| u64::from(*tag) <= maximum) {
        Ok(())
    } else {
        Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0036",
            format!("variant-tag-out-of-range-{name}"),
        ))
    }
}

fn parse_privacy(cursor: &Cursor<'_>, value: &str) -> Result<Privacy, SchemaError> {
    match value {
        "public" => Ok(Privacy::Public),
        "private" => Ok(Privacy::Private),
        "sensitive" => Ok(Privacy::Sensitive),
        _ => Err(cursor.error("RPE-PROTOCOL-SCHEMA-0013", "invalid-privacy")),
    }
}

fn parse_capability_annotation(cursor: &Cursor<'_>, value: &str) -> Result<String, SchemaError> {
    let capability = value
        .strip_prefix("requires=")
        .ok_or_else(|| cursor.error("RPE-PROTOCOL-SCHEMA-0038", "invalid-capability-annotation"))?;
    require_name(cursor, capability)?;
    Ok(capability.into())
}

fn validate_type(ty: &Type, names: &BTreeSet<String>) -> Result<(), SchemaError> {
    validate_type_at_depth(ty, names, 0)
}

fn validate_type_at_depth(
    ty: &Type,
    names: &BTreeSet<String>,
    depth: usize,
) -> Result<(), SchemaError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0039",
            "type-nesting-too-deep",
        ));
    }
    match ty {
        Type::Primitive(_) => Ok(()),
        Type::Named(name) if names.contains(name) => Ok(()),
        Type::Named(name) => Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0025",
            format!("unknown-type-{name}"),
        )),
        Type::Optional(inner) => validate_type_at_depth(inner, names, depth + 1),
        Type::List(inner, limit) if *limit != 0 => validate_type_at_depth(inner, names, depth + 1),
        Type::List(_, _) | Type::Bytes(0) => Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0026",
            "zero-container-limit",
        )),
        Type::Bytes(_) => Ok(()),
    }
}

fn parse_type(cursor: &Cursor<'_>, value: &str) -> Result<Type, SchemaError> {
    parse_type_at_depth(cursor, value, 0)
}

fn parse_type_at_depth(
    cursor: &Cursor<'_>,
    value: &str,
    depth: usize,
) -> Result<Type, SchemaError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0039", "type-nesting-too-deep"));
    }
    if let Ok(primitive) = parse_primitive(cursor, value) {
        return Ok(Type::Primitive(primitive));
    }
    if let Some(inner) = value
        .strip_prefix("optional<")
        .and_then(|value| value.strip_suffix('>'))
    {
        let parsed = parse_type_at_depth(cursor, inner, depth + 1)?;
        if matches!(parsed, Type::Optional(_)) {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0040", "redundant-nested-optional"));
        }
        return Ok(Type::Optional(Box::new(parsed)));
    }
    if let Some(inner) = value
        .strip_prefix("list<")
        .and_then(|value| value.strip_suffix('>'))
    {
        let (ty, limit) = inner
            .rsplit_once(',')
            .ok_or_else(|| cursor.error("RPE-PROTOCOL-SCHEMA-0027", "invalid-list"))?;
        return Ok(Type::List(
            Box::new(parse_type_at_depth(cursor, ty, depth + 1)?),
            parse_number(cursor, limit)?,
        ));
    }
    if let Some(limit) = value
        .strip_prefix("bytes<")
        .and_then(|value| value.strip_suffix('>'))
    {
        return Ok(Type::Bytes(parse_number(cursor, limit)?));
    }
    require_name(cursor, value)?;
    Ok(Type::Named(value.into()))
}

fn parse_primitive(cursor: &Cursor<'_>, value: &str) -> Result<Primitive, SchemaError> {
    match value {
        "u8" => Ok(Primitive::U8),
        "u16" => Ok(Primitive::U16),
        "u32" => Ok(Primitive::U32),
        "u64" => Ok(Primitive::U64),
        "i32" => Ok(Primitive::I32),
        "bool" => Ok(Primitive::Bool),
        "bytes16" => Ok(Primitive::Bytes16),
        "bytes32" => Ok(Primitive::Bytes32),
        _ => Err(cursor.error("RPE-PROTOCOL-SCHEMA-0028", "not-primitive")),
    }
}

fn parse_number<T>(cursor: &Cursor<'_>, value: &str) -> Result<T, SchemaError>
where
    T: TryFrom<u64>,
{
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0029", "invalid-number"));
    }
    let parsed: u64 = value
        .parse()
        .map_err(|_| cursor.error("RPE-PROTOCOL-SCHEMA-0029", "number-overflow"))?;
    parsed
        .try_into()
        .map_err(|_| cursor.error("RPE-PROTOCOL-SCHEMA-0029", "number-range"))
}

fn exact_words(cursor: &Cursor<'_>, words: &[&str], count: usize) -> Result<(), SchemaError> {
    if words.len() != count || words.iter().any(|word| word.is_empty()) {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0030", "invalid-spacing-or-arity"));
    }
    Ok(())
}

fn require_name(cursor: &Cursor<'_>, value: &str) -> Result<(), SchemaError> {
    if valid_name(value) {
        Ok(())
    } else {
        Err(cursor.error("RPE-PROTOCOL-SCHEMA-0031", "invalid-name"))
    }
}

fn valid_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_field_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

struct Cursor<'a> {
    lines: &'a [&'a str],
    index: usize,
}

impl<'a> Cursor<'a> {
    const fn new(lines: &'a [&'a str]) -> Self {
        Self { lines, index: 0 }
    }

    fn required_line(&mut self) -> Result<&'a str, SchemaError> {
        let line =
            self.lines.get(self.index).copied().ok_or_else(|| {
                self.error("RPE-PROTOCOL-SCHEMA-0032", "unexpected-end-of-schema")
            })?;
        self.index += 1;
        if line.is_empty() || line.trim() != line || line.contains("  ") {
            return Err(self.error("RPE-PROTOCOL-SCHEMA-0030", "non-canonical-line"));
        }
        Ok(line)
    }

    fn required_nonblank(&mut self) -> Result<&'a str, SchemaError> {
        while self.lines.get(self.index) == Some(&"") {
            self.index += 1;
        }
        self.required_line()
    }

    fn next_nonblank(&mut self) -> Result<Option<&'a str>, SchemaError> {
        while self.lines.get(self.index) == Some(&"") {
            self.index += 1;
        }
        if self.index == self.lines.len() {
            Ok(None)
        } else {
            self.required_line().map(Some)
        }
    }

    fn error(&self, code: &'static str, detail: impl Into<String>) -> SchemaError {
        SchemaError::new(self.index.max(1), code, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SCHEMA_BYTES, parse_schema};

    const MINIMAL: &str = "protocol Test 1 0\nlimit max_message_bytes 64\nlimit max_transfer_slots 1\nlimit max_data_segment_bytes 16\nlimit max_data_ticket_bytes 64\ncodec fixed_le_v1\n\nrecord PingCommand\nend\n\nrecord PongEvent\nend\n\nmessage command Ping 1 PingCommand Ready Request no 0 0 0 32 Pong:terminal\nmessage event Pong 2 PongEvent Ready Request event 0 0 0 32\n";

    #[test]
    fn parses_and_replays_canonical_schema() {
        let parsed = parse_schema(MINIMAL).unwrap();
        assert_eq!(parsed.canonical_text(), MINIMAL);
    }

    #[test]
    fn rejects_noncanonical_and_unknown_outcomes() {
        let spacing = MINIMAL.replace("protocol Test", "protocol  Test");
        assert_eq!(
            parse_schema(&spacing).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0030"
        );
        let unknown = MINIMAL.replace("32 Pong:terminal", "32 Missing:terminal");
        assert_eq!(
            parse_schema(&unknown).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0023"
        );
    }

    #[test]
    fn rejects_oversized_duplicate_and_over_limit_schema() {
        let oversized = format!("{}\n", "x".repeat(MAX_SCHEMA_BYTES));
        assert_eq!(
            parse_schema(&oversized).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0001"
        );

        let duplicate = MINIMAL.replace(
            "\n\nrecord PongEvent",
            "\n\nrecord PingCommand\nend\n\nrecord PongEvent",
        );
        assert_eq!(
            parse_schema(&duplicate).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0018"
        );

        let over_slots = MINIMAL.replace("Request no 0 0 0 32", "Request no 0 0 2 32");
        assert_eq!(
            parse_schema(&over_slots).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0033"
        );

        let reserved_union_field = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nunion Choice u8\nvariant Value 1 kind:u8:public\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&reserved_union_field).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0015"
        );

        let invalid_repr = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nenum Choice bool\nvariant Value 1\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&invalid_repr).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0035"
        );

        let overflowing_tag = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nenum Choice u8\nvariant Value 256\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&overflowing_tag).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0036"
        );

        let missing_union_privacy = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nunion Choice u8\nvariant Value 1 value:u8\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&missing_union_privacy).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0015"
        );
    }

    #[test]
    fn union_variant_and_field_counts_are_independently_bounded() {
        let variants = (1..=257)
            .map(|tag| format!("variant Value{tag} {tag}"))
            .collect::<Vec<_>>()
            .join("\n");
        let too_many_variants = MINIMAL.replace(
            "\n\nrecord PingCommand",
            &format!("\n\nunion Choice u16\n{variants}\nend\n\nrecord PingCommand"),
        );
        assert_eq!(
            parse_schema(&too_many_variants).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0001"
        );

        let fields = (0..257)
            .map(|index| format!("value{index}:u8:public"))
            .collect::<Vec<_>>()
            .join(",");
        let too_many_fields = MINIMAL.replace(
            "\n\nrecord PingCommand",
            &format!("\n\nunion Choice u8\nvariant Value 1 {fields}\nend\n\nrecord PingCommand"),
        );
        assert_eq!(
            parse_schema(&too_many_fields).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0001"
        );
    }

    #[test]
    fn type_nesting_has_an_exact_stack_budget() {
        let within = format!("{}u8{}", "list<".repeat(32), ",1>".repeat(32));
        let accepted = MINIMAL.replace(
            "\n\nrecord PingCommand",
            &format!(
                "\n\nrecord Nested\nfield value {within} required public\nend\n\nrecord PingCommand"
            ),
        );
        parse_schema(&accepted).unwrap();

        let over = format!("{}u8{}", "list<".repeat(33), ",1>".repeat(33));
        let rejected = MINIMAL.replace(
            "\n\nrecord PingCommand",
            &format!(
                "\n\nrecord Nested\nfield value {over} required public\nend\n\nrecord PingCommand"
            ),
        );
        assert_eq!(
            parse_schema(&rejected).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0039"
        );

        let redundant = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nrecord Nested\nfield value optional<optional<u8>> optional public\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&redundant).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0040"
        );
    }

    #[test]
    fn recursive_types_and_unrepresentable_message_limits_fail_closed() {
        let recursive = MINIMAL.replace(
            "\n\nrecord PingCommand",
            "\n\nrecord Recursive\nfield child optional<Recursive> optional public\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&recursive).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0044"
        );

        let undersized = MINIMAL
            .replace(
                "record PingCommand\nend",
                "record PingCommand\nfield value u64 required public\nend",
            )
            .replace("Request no 0 0 0 32", "Request no 0 0 0 4");
        assert_eq!(
            parse_schema(&undersized).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0045"
        );
    }
}
