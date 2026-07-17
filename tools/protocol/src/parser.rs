use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::{
    EnumDef, Field, Message, MessageKind, Presence, Primitive, Privacy, Protocol, Record, Scalar,
    Type, Union, UnionField, UnionVariant, Variant,
};

pub(crate) const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_DEFINITIONS: usize = 1024;
const MAX_FIELDS: usize = 256;

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
    if max_message_bytes == 0 || max_transfer_slots == 0 {
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
        scalars,
        enums,
        records,
        unions,
        messages,
    };
    validate(&protocol)?;
    if protocol.canonical_text() != input {
        return Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0006",
            "non-canonical-layout",
        ));
    }
    Ok(protocol)
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
        let privacy = match parts[4] {
            "public" => Privacy::Public,
            "private" => Privacy::Private,
            "sensitive" => Privacy::Sensitive,
            _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0013", "invalid-privacy")),
        };
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
        if !(parts.len() == 3 || parts.len() == 4) || parts[0] != "variant" {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-variant"));
        }
        require_name(cursor, parts[1])?;
        let mut fields = Vec::new();
        if let Some(list) = parts.get(3) {
            for pair in list.split(',') {
                let (field, ty) = pair.split_once(':').ok_or_else(|| {
                    cursor.error("RPE-PROTOCOL-SCHEMA-0015", "invalid-union-field")
                })?;
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
                });
            }
        }
        variants.push(UnionVariant {
            name: parts[1].into(),
            tag: parse_number(cursor, parts[2])?,
            fields,
        });
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
    if words.len() < 12 || words.len() > 13 {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-shape"));
    }
    let kind = match words[1] {
        "command" => MessageKind::Command,
        "event" => MessageKind::Event,
        _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-kind")),
    };
    if (kind == MessageKind::Command) != (words.len() == 13) {
        return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "message-kind-shape-mismatch"));
    }
    for word in [words[2], words[4], words[5], words[6]] {
        if !valid_name(word) {
            return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-message-token"));
        }
    }
    let disposition = words[7];
    match (kind, disposition) {
        (MessageKind::Command, "yes" | "no") | (MessageKind::Event, "terminal" | "stream") => {}
        _ => return Err(cursor.error("RPE-PROTOCOL-SCHEMA-0017", "invalid-disposition")),
    }
    let outcomes = if kind == MessageKind::Command {
        let raw = words[12];
        if raw == "none" {
            Vec::new()
        } else {
            raw.split(',')
                .map(|value| {
                    require_name(cursor, value)?;
                    Ok(value.into())
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
        validate_variants(
            &enumeration
                .variants
                .iter()
                .map(|value| (&value.name, value.tag))
                .collect::<Vec<_>>(),
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
        validate_variants(
            &union
                .variants
                .iter()
                .map(|value| (&value.name, value.tag))
                .collect::<Vec<_>>(),
        )?;
        for variant in &union.variants {
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
            "Worker" | "Session" | "Request" | "Generation"
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
            if !events.contains_key(outcome) {
                return Err(SchemaError::new(
                    0,
                    "RPE-PROTOCOL-SCHEMA-0023",
                    format!("unknown-outcome-{outcome}"),
                ));
            }
        }
    }
    Ok(())
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

fn validate_type(ty: &Type, names: &BTreeSet<String>) -> Result<(), SchemaError> {
    match ty {
        Type::Primitive(_) => Ok(()),
        Type::Named(name) if names.contains(name) => Ok(()),
        Type::Named(name) => Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0025",
            format!("unknown-type-{name}"),
        )),
        Type::Optional(inner) => validate_type(inner, names),
        Type::List(inner, limit) if *limit != 0 => validate_type(inner, names),
        Type::List(_, _) | Type::Bytes(0) => Err(SchemaError::new(
            0,
            "RPE-PROTOCOL-SCHEMA-0026",
            "zero-container-limit",
        )),
        Type::Bytes(_) => Ok(()),
    }
}

fn parse_type(cursor: &Cursor<'_>, value: &str) -> Result<Type, SchemaError> {
    if let Ok(primitive) = parse_primitive(cursor, value) {
        return Ok(Type::Primitive(primitive));
    }
    if let Some(inner) = value
        .strip_prefix("optional<")
        .and_then(|value| value.strip_suffix('>'))
    {
        return Ok(Type::Optional(Box::new(parse_type(cursor, inner)?)));
    }
    if let Some(inner) = value
        .strip_prefix("list<")
        .and_then(|value| value.strip_suffix('>'))
    {
        let (ty, limit) = inner
            .rsplit_once(',')
            .ok_or_else(|| cursor.error("RPE-PROTOCOL-SCHEMA-0027", "invalid-list"))?;
        return Ok(Type::List(
            Box::new(parse_type(cursor, ty)?),
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

    const MINIMAL: &str = "protocol Test 1 0\nlimit max_message_bytes 64\nlimit max_transfer_slots 1\n\nrecord PingCommand\nend\n\nrecord PongEvent\nend\n\nmessage command Ping 1 PingCommand Ready Request no 0 0 0 32 Pong\nmessage event Pong 2 PongEvent Ready Request terminal 0 0 0 32\n";

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
        let unknown = MINIMAL.replace("32 Pong", "32 Missing");
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
            "\n\nunion Choice u8\nvariant Value 1 kind:u8\nend\n\nrecord PingCommand",
        );
        assert_eq!(
            parse_schema(&reserved_union_field).unwrap_err().code(),
            "RPE-PROTOCOL-SCHEMA-0015"
        );
    }
}
