//! Emits dependency-free typed `fixed_le_v1` payload codecs.
//!
//! The generated source is designed to be appended to the declarations emitted by the primary
//! protocol generator. Message dispatch encodes `Correlation || message-specific record` and
//! uses the outer header only to bind message id and actual payload length; negotiated-schema
//! selection, the 20-byte header codec, and out-of-band attachments remain boundary concerns.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::model::{
    EnumDef, MessageKind, Primitive, Protocol, Record, Scalar, Type, Union, UnionVariant,
};

const DEFAULT_MAX_DEPTH: usize = 64;
const DEFAULT_MAX_CONTAINER_ITEMS: usize = 1_048_576;

/// Generates the Rust typed payload codec appended after the existing generated declarations.
pub(crate) fn generate_rust_payload_codec(protocol: &Protocol) -> String {
    let mut out = String::new();
    write_rust_runtime(&mut out, protocol);
    for scalar in &protocol.scalars {
        write_rust_scalar_codec(&mut out, scalar);
    }
    for enumeration in &protocol.enums {
        write_rust_enum_codec(&mut out, enumeration);
    }
    for record in &protocol.records {
        write_rust_record_codec(&mut out, protocol, record);
    }
    for union in &protocol.unions {
        write_rust_union_codec(&mut out, protocol, union);
    }
    write_rust_dispatch(&mut out, protocol);
    write_rust_hash_preimage_helpers(&mut out, protocol);
    out
}

/// Generates the TypeScript typed payload codec appended after the browser declarations.
pub(crate) fn generate_typescript_payload_codec(protocol: &Protocol) -> String {
    let mut out = String::new();
    write_typescript_runtime(&mut out, protocol);
    for scalar in &protocol.scalars {
        write_typescript_scalar_codec(&mut out, scalar);
    }
    for enumeration in &protocol.enums {
        write_typescript_enum_codec(&mut out, enumeration);
    }
    for record in &protocol.records {
        write_typescript_record_codec(&mut out, protocol, record);
    }
    for union in &protocol.unions {
        write_typescript_union_codec(&mut out, protocol, union);
    }
    write_typescript_dispatch(&mut out, protocol);
    write_typescript_hash_preimage_helpers(&mut out, protocol);
    out
}

fn write_rust_runtime(out: &mut String, protocol: &Protocol) {
    writeln!(
        out,
        "\n// Canonical fixed_le_v1 typed payload codec.\n\
         pub const DEFAULT_PAYLOAD_CODEC_MAX_DEPTH: usize = {DEFAULT_MAX_DEPTH};\n\
         pub const DEFAULT_PAYLOAD_CODEC_MAX_CONTAINER_ITEMS: usize = {DEFAULT_MAX_CONTAINER_ITEMS};"
    )
    .unwrap();
    out.push_str(
        r#"
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadCodecErrorCode {
    LimitExceeded = 1,
    Truncated = 2,
    TrailingBytes = 3,
    InvalidBooleanMarker = 4,
    InvalidOptionalMarker = 5,
    UnknownTag = 6,
    UnknownMessage = 7,
    InvalidValue = 8,
    SharedArrayBuffer = 9,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadCodecError {
    pub code: PayloadCodecErrorCode,
    pub offset: usize,
}

impl PayloadCodecError {
    const fn new(code: PayloadCodecErrorCode, offset: usize) -> Self {
        Self { code, offset }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadCodecLimits {
    pub max_depth: usize,
    pub max_bytes: usize,
    pub max_container_items: usize,
}

impl PayloadCodecLimits {
    pub const fn new(max_depth: usize, max_bytes: usize, max_container_items: usize) -> Self {
        Self { max_depth, max_bytes, max_container_items }
    }

    pub const fn protocol_default() -> Self {
        Self {
            max_depth: DEFAULT_PAYLOAD_CODEC_MAX_DEPTH,
            max_bytes: MAX_MESSAGE_BYTES as usize,
            max_container_items: DEFAULT_PAYLOAD_CODEC_MAX_CONTAINER_ITEMS,
        }
    }

    const fn capped_bytes(self, maximum: usize) -> Self {
        Self {
            max_depth: self.max_depth,
            max_bytes: if self.max_bytes < maximum { self.max_bytes } else { maximum },
            max_container_items: self.max_container_items,
        }
    }

    const fn valid(self) -> bool {
        self.max_depth != 0 && self.max_bytes != 0 && self.max_container_items != 0
    }
}

struct PayloadWriter {
    bytes: Vec<u8>,
    limits: PayloadCodecLimits,
    remaining_container_items: usize,
}

impl PayloadWriter {
    fn new(limits: PayloadCodecLimits) -> Result<Self, PayloadCodecError> {
        if !limits.valid() {
            return Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0));
        }
        Ok(Self {
            bytes: Vec::new(),
            limits,
            remaining_container_items: limits.max_container_items,
        })
    }

    fn offset(&self) -> usize {
        self.bytes.len()
    }

    fn check_depth(&self, depth: usize) -> Result<(), PayloadCodecError> {
        if depth > self.limits.max_depth {
            Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, self.offset()))
        } else {
            Ok(())
        }
    }

    fn charge_container(&mut self, count: usize) -> Result<(), PayloadCodecError> {
        if count > self.remaining_container_items {
            return Err(PayloadCodecError::new(
                PayloadCodecErrorCode::LimitExceeded,
                self.offset(),
            ));
        }
        self.remaining_container_items -= count;
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), PayloadCodecError> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, self.offset()))?;
        if length > self.limits.max_bytes {
            return Err(PayloadCodecError::new(
                PayloadCodecErrorCode::LimitExceeded,
                self.offset(),
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_optional(&mut self, present: bool) -> Result<(), PayloadCodecError> {
        if present {
            self.charge_container(1)?;
        }
        self.write(&[u8::from(present)])
    }

    fn write_count(&mut self, count: usize, maximum: u32) -> Result<(), PayloadCodecError> {
        let offset = self.offset();
        let count_u32 = u32::try_from(count)
            .map_err(|_| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, offset))?;
        if count_u32 > maximum {
            return Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, offset));
        }
        self.charge_container(count)?;
        self.write(&count_u32.to_le_bytes())
    }

    #[allow(dead_code)]
    fn write_bytes(&mut self, value: &[u8], maximum: u32) -> Result<(), PayloadCodecError> {
        self.write_count(value.len(), maximum)?;
        self.write(value)
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct PayloadReader<'a> {
    input: &'a [u8],
    offset: usize,
    limits: PayloadCodecLimits,
    remaining_container_items: usize,
}

impl<'a> PayloadReader<'a> {
    fn new(input: &'a [u8], limits: PayloadCodecLimits) -> Result<Self, PayloadCodecError> {
        if !limits.valid() {
            return Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0));
        }
        if input.len() > limits.max_bytes {
            return Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0));
        }
        Ok(Self {
            input,
            offset: 0,
            limits,
            remaining_container_items: limits.max_container_items,
        })
    }

    fn offset(&self) -> usize {
        self.offset
    }

    fn check_depth(&self, depth: usize) -> Result<(), PayloadCodecError> {
        if depth > self.limits.max_depth {
            Err(PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, self.offset))
        } else {
            Ok(())
        }
    }

    fn charge_container(&mut self, count: usize) -> Result<(), PayloadCodecError> {
        if count > self.remaining_container_items {
            return Err(PayloadCodecError::new(
                PayloadCodecErrorCode::LimitExceeded,
                self.offset,
            ));
        }
        self.remaining_container_items -= count;
        Ok(())
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PayloadCodecError> {
        let start = self.offset;
        let end = start
            .checked_add(count)
            .ok_or_else(|| PayloadCodecError::new(PayloadCodecErrorCode::Truncated, start))?;
        let bytes = self
            .input
            .get(start..end)
            .ok_or_else(|| PayloadCodecError::new(PayloadCodecErrorCode::Truncated, start))?;
        self.offset = end;
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8, PayloadCodecError> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, PayloadCodecError> {
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, PayloadCodecError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, PayloadCodecError> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, PayloadCodecError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(i32::from_le_bytes(bytes))
    }

    fn read_bool(&mut self) -> Result<bool, PayloadCodecError> {
        let offset = self.offset;
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidBooleanMarker, offset)),
        }
    }

    fn read_optional(&mut self) -> Result<bool, PayloadCodecError> {
        let offset = self.offset;
        match self.read_u8()? {
            0 => Ok(false),
            1 => {
                self.charge_container(1)?;
                Ok(true)
            }
            _ => Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidOptionalMarker, offset)),
        }
    }

    fn read_fixed_16(&mut self) -> Result<[u8; 16], PayloadCodecError> {
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(bytes)
    }

    fn read_fixed_32(&mut self) -> Result<[u8; 32], PayloadCodecError> {
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(self.take(32)?);
        Ok(bytes)
    }

    fn read_count(
        &mut self,
        maximum: u32,
        minimum_item_bytes: usize,
    ) -> Result<usize, PayloadCodecError> {
        let count_offset = self.offset;
        let count_u32 = self.read_u32()?;
        if count_u32 > maximum {
            return Err(PayloadCodecError::new(
                PayloadCodecErrorCode::LimitExceeded,
                count_offset,
            ));
        }
        let count = usize::try_from(count_u32)
            .map_err(|_| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, count_offset))?;
        self.charge_container(count)?;
        let minimum = count
            .checked_mul(minimum_item_bytes)
            .ok_or_else(|| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, count_offset))?;
        if minimum > self.input.len().saturating_sub(self.offset) {
            return Err(PayloadCodecError::new(
                PayloadCodecErrorCode::Truncated,
                self.offset,
            ));
        }
        Ok(count)
    }

    #[allow(dead_code)]
    fn read_bytes(&mut self, maximum: u32) -> Result<Vec<u8>, PayloadCodecError> {
        let count = self.read_count(maximum, 1)?;
        Ok(self.take(count)?.to_vec())
    }

    fn finish(&self) -> Result<(), PayloadCodecError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(PayloadCodecError::new(
                PayloadCodecErrorCode::TrailingBytes,
                self.offset,
            ))
        }
    }
}
"#,
    );
    writeln!(
        out,
        "const _: () = {{ assert!(MAX_MESSAGE_BYTES == {}); }};\n",
        protocol.max_message_bytes
    )
    .unwrap();
    if protocol_uses_variable_bytes(protocol) {
        *out = out
            .replace(
                "    #[allow(dead_code)]\n    fn write_bytes",
                "    fn write_bytes",
            )
            .replace(
                "    #[allow(dead_code)]\n    fn read_bytes",
                "    fn read_bytes",
            );
    }
}

fn write_rust_scalar_codec(out: &mut String, scalar: &Scalar) {
    let function = snake_case(&scalar.name);
    writeln!(
        out,
        "fn payload_encode_{function}_into(value: &{}, depth: usize, writer: &mut PayloadWriter) -> Result<(), PayloadCodecError> {{\n    writer.check_depth(depth)?;",
        scalar.name
    )
    .unwrap();
    match scalar.primitive {
        Primitive::Bytes16 | Primitive::Bytes32 => {
            out.push_str("    writer.write(value.digest())?;\n");
        }
        primitive => {
            writeln!(out, "    let primitive = value.value();").unwrap();
            write_rust_encode_primitive(out, primitive, "&primitive", "writer", "    ");
        }
    }
    out.push_str("    Ok(())\n}\n\n");

    writeln!(
        out,
        "fn payload_decode_{function}_from(depth: usize, reader: &mut PayloadReader<'_>) -> Result<{}, PayloadCodecError> {{\n    reader.check_depth(depth)?;\n    Ok({}::new({}))\n}}\n",
        scalar.name,
        scalar.name,
        rust_decode_primitive_expression(scalar.primitive, "reader")
    )
    .unwrap();
    write_rust_root_functions(out, &scalar.name);
}

fn write_rust_enum_codec(out: &mut String, enumeration: &EnumDef) {
    let function = snake_case(&enumeration.name);
    writeln!(
        out,
        "fn payload_encode_{function}_into(value: &{}, depth: usize, writer: &mut PayloadWriter) -> Result<(), PayloadCodecError> {{\n    writer.check_depth(depth)?;\n    match value {{",
        enumeration.name
    )
    .unwrap();
    for variant in &enumeration.variants {
        writeln!(
            out,
            "        {}::{} => {},",
            enumeration.name,
            variant.name,
            rust_write_tag_expression(enumeration.repr, variant.tag, "writer")
        )
        .unwrap();
    }
    out.push_str("    }\n}\n\n");

    writeln!(
        out,
        "fn payload_decode_{function}_from(depth: usize, reader: &mut PayloadReader<'_>) -> Result<{}, PayloadCodecError> {{\n    reader.check_depth(depth)?;\n    let tag_offset = reader.offset();\n    let tag = {};\n    match tag {{",
        enumeration.name,
        rust_read_tag_expression(enumeration.repr, "reader")
    )
    .unwrap();
    for variant in &enumeration.variants {
        writeln!(
            out,
            "        {} => Ok({}::{}),",
            rust_tag_literal(enumeration.repr, variant.tag),
            enumeration.name,
            variant.name
        )
        .unwrap();
    }
    out.push_str(
        "        _ => Err(PayloadCodecError::new(PayloadCodecErrorCode::UnknownTag, tag_offset)),\n    }\n}\n\n",
    );
    write_rust_root_functions(out, &enumeration.name);
}

fn write_rust_record_codec(out: &mut String, protocol: &Protocol, record: &Record) {
    let function = snake_case(&record.name);
    let value_name = if record.fields.is_empty() {
        "_value"
    } else {
        "value"
    };
    writeln!(
        out,
        "fn payload_encode_{function}_into({value_name}: &{}, depth: usize, writer: &mut PayloadWriter) -> Result<(), PayloadCodecError> {{\n    writer.check_depth(depth)?;",
        record.name,
    )
    .unwrap();
    for field in &record.fields {
        write_rust_encode_type(
            out,
            protocol,
            &field.ty,
            &format!("&value.{}", field.name),
            "depth + 1",
            "writer",
            "    ",
        );
    }
    out.push_str("    Ok(())\n}\n\n");

    writeln!(
        out,
        "fn payload_decode_{function}_from(depth: usize, reader: &mut PayloadReader<'_>) -> Result<{}, PayloadCodecError> {{\n    reader.check_depth(depth)?;\n    Ok({} {{",
        record.name, record.name
    )
    .unwrap();
    for field in &record.fields {
        writeln!(
            out,
            "        {}: {},",
            field.name,
            rust_decode_type_expression(protocol, &field.ty, "depth + 1", "reader")
        )
        .unwrap();
    }
    out.push_str("    })\n}\n\n");
    write_rust_root_functions(out, &record.name);
}

fn write_rust_union_codec(out: &mut String, protocol: &Protocol, union: &Union) {
    let function = snake_case(&union.name);
    writeln!(
        out,
        "fn payload_encode_{function}_into(value: &{}, depth: usize, writer: &mut PayloadWriter) -> Result<(), PayloadCodecError> {{\n    writer.check_depth(depth)?;\n    match value {{",
        union.name
    )
    .unwrap();
    for variant in &union.variants {
        if variant.fields.is_empty() {
            writeln!(
                out,
                "        {}::{} => {},",
                union.name,
                variant.name,
                rust_write_tag_expression(union.repr, variant.tag, "writer")
            )
            .unwrap();
            continue;
        }
        writeln!(
            out,
            "        {}::{} {{ {} }} => {{",
            union.name,
            variant.name,
            variant
                .fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .unwrap();
        writeln!(
            out,
            "            {}?;",
            rust_write_tag_expression(union.repr, variant.tag, "writer")
        )
        .unwrap();
        for field in &variant.fields {
            write_rust_encode_type(
                out,
                protocol,
                &field.ty,
                &field.name,
                "depth + 1",
                "writer",
                "            ",
            );
        }
        out.push_str("            Ok(())\n        }\n");
    }
    out.push_str("    }\n}\n\n");

    writeln!(
        out,
        "fn payload_decode_{function}_from(depth: usize, reader: &mut PayloadReader<'_>) -> Result<{}, PayloadCodecError> {{\n    reader.check_depth(depth)?;\n    let tag_offset = reader.offset();\n    let tag = {};\n    match tag {{",
        union.name,
        rust_read_tag_expression(union.repr, "reader")
    )
    .unwrap();
    for variant in &union.variants {
        if variant.fields.is_empty() {
            writeln!(
                out,
                "        {} => Ok({}::{}),",
                rust_tag_literal(union.repr, variant.tag),
                union.name,
                variant.name
            )
            .unwrap();
            continue;
        }
        writeln!(
            out,
            "        {} => Ok({}::{} {{",
            rust_tag_literal(union.repr, variant.tag),
            union.name,
            variant.name
        )
        .unwrap();
        for field in &variant.fields {
            writeln!(
                out,
                "            {}: {},",
                field.name,
                rust_decode_type_expression(protocol, &field.ty, "depth + 1", "reader")
            )
            .unwrap();
        }
        out.push_str("        }),\n");
    }
    out.push_str(
        "        _ => Err(PayloadCodecError::new(PayloadCodecErrorCode::UnknownTag, tag_offset)),\n    }\n}\n\n",
    );
    write_rust_root_functions(out, &union.name);
}

fn write_rust_root_functions(out: &mut String, type_name: &str) {
    let function = snake_case(type_name);
    writeln!(
        out,
        "pub fn encode_{function}_payload(value: &{type_name}, limits: PayloadCodecLimits) -> Result<Vec<u8>, PayloadCodecError> {{\n    let mut writer = PayloadWriter::new(limits)?;\n    payload_encode_{function}_into(value, 0, &mut writer)?;\n    Ok(writer.finish())\n}}\n"
    )
    .unwrap();
    writeln!(
        out,
        "pub fn decode_{function}_payload(input: &[u8], limits: PayloadCodecLimits) -> Result<{type_name}, PayloadCodecError> {{\n    let mut reader = PayloadReader::new(input, limits)?;\n    let value = payload_decode_{function}_from(0, &mut reader)?;\n    reader.finish()?;\n    Ok(value)\n}}\n"
    )
    .unwrap();
}

fn write_rust_dispatch(out: &mut String, protocol: &Protocol) {
    for (kind, enum_name, function) in [
        (MessageKind::Command, "Command", "command"),
        (MessageKind::Event, "Event", "event"),
    ] {
        let envelope_name = format!("{enum_name}Envelope");
        let envelope_field = function;
        writeln!(
            out,
            "pub fn encode_{function}_payload(value: &{envelope_name}, limits: PayloadCodecLimits) -> Result<(u16, Vec<u8>), PayloadCodecError> {{\n    let (message_id, message_limit) = match &value.{envelope_field} {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "        {enum_name}::{}(_) => ({}, {}_usize),",
                message.name, message.id, message.max_payload_bytes
            )
            .unwrap();
        }
        out.push_str(
            "    };\n    if value.header.message_type != message_id {\n        return Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidValue, 0));\n    }\n    let mut writer = PayloadWriter::new(limits.capped_bytes(message_limit))?;\n    payload_encode_correlation_into(&value.correlation, 0, &mut writer)?;\n    match &value.",
        );
        out.push_str(envelope_field);
        out.push_str(" {\n");
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "        {enum_name}::{}(payload) => payload_encode_{}_into(payload, 0, &mut writer)?,",
                message.name,
                snake_case(&message.payload)
            )
            .unwrap();
        }
        out.push_str(
            "    }\n    let bytes = writer.finish();\n    let actual = u32::try_from(bytes.len()).map_err(|_| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0))?;\n    if value.header.payload_len != actual {\n        return Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidValue, 0));\n    }\n    Ok((message_id, bytes))\n}\n\n",
        );

        writeln!(
            out,
            "pub fn decode_{function}_payload(header: EnvelopeHeader, input: &[u8], limits: PayloadCodecLimits) -> Result<{envelope_name}, PayloadCodecError> {{\n    let actual = u32::try_from(input.len()).map_err(|_| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0))?;\n    if header.payload_len != actual {{\n        return Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidValue, 0));\n    }}\n    let message_limit = match header.message_type {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "        {} => {}_usize,",
                message.id, message.max_payload_bytes
            )
            .unwrap();
        }
        out.push_str(
            "        _ => return Err(PayloadCodecError::new(PayloadCodecErrorCode::UnknownMessage, 0)),\n    };\n    let mut reader = PayloadReader::new(input, limits.capped_bytes(message_limit))?;\n    let correlation = payload_decode_correlation_from(0, &mut reader)?;\n    let ",
        );
        out.push_str(envelope_field);
        out.push_str(" = match header.message_type {\n");
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "        {} => {enum_name}::{}(payload_decode_{}_from(0, &mut reader)?),",
                message.id,
                message.name,
                snake_case(&message.payload)
            )
            .unwrap();
        }
        out.push_str(
            "        _ => return Err(PayloadCodecError::new(PayloadCodecErrorCode::UnknownMessage, 0)),\n    };\n    reader.finish()?;\n    Ok(",
        );
        out.push_str(&envelope_name);
        out.push_str(" { header, correlation, ");
        out.push_str(envelope_field);
        out.push_str(" })\n}\n\n");
    }
}

fn write_rust_hash_preimage_helpers(out: &mut String, protocol: &Protocol) {
    let targets = hash_preimage_targets(protocol);
    if targets.is_empty() {
        return;
    }
    out.push_str(
        "fn payload_codec_hash_preimage(domain: &str, payload: Vec<u8>) -> Result<Vec<u8>, PayloadCodecError> {\n\
    let payload_len = u64::try_from(payload.len())\n\
        .map_err(|_| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0))?;\n\
    let capacity = domain.len().checked_add(9)\n\
        .and_then(|length| length.checked_add(payload.len()))\n\
        .ok_or_else(|| PayloadCodecError::new(PayloadCodecErrorCode::LimitExceeded, 0))?;\n\
    let mut preimage = Vec::with_capacity(capacity);\n\
    preimage.extend_from_slice(domain.as_bytes());\n\
    preimage.push(0);\n\
    preimage.extend_from_slice(&payload_len.to_le_bytes());\n\
    preimage.extend_from_slice(&payload);\n\
    Ok(preimage)\n\
}\n\n",
    );
    for (type_name, function, domain) in targets {
        writeln!(
            out,
            "pub fn {function}_hash_preimage(value: &{type_name}, limits: PayloadCodecLimits) -> Result<Vec<u8>, PayloadCodecError> {{\n    let payload = encode_{}_payload(value, limits)?;\n    payload_codec_hash_preimage({domain}, payload)\n}}\n",
            snake_case(type_name)
        )
        .unwrap();
    }
}

fn hash_preimage_targets(protocol: &Protocol) -> Vec<(&'static str, &'static str, &'static str)> {
    [
        (
            "CapabilityDecision",
            "capability_decision",
            "CAPABILITY_DECISION_HASH_DOMAIN",
        ),
        (
            "RenderPlanManifest",
            "render_plan_manifest",
            "RENDER_PLAN_MANIFEST_HASH_DOMAIN",
        ),
    ]
    .into_iter()
    .filter(|(type_name, _, _)| {
        protocol
            .records
            .iter()
            .any(|record| record.name == *type_name)
    })
    .collect()
}

fn write_rust_encode_type(
    out: &mut String,
    protocol: &Protocol,
    ty: &Type,
    value: &str,
    depth: &str,
    writer: &str,
    indent: &str,
) {
    match ty {
        Type::Primitive(primitive) => {
            write_rust_encode_primitive(out, *primitive, value, writer, indent);
        }
        Type::Named(name) => {
            writeln!(
                out,
                "{indent}payload_encode_{}_into({value}, {depth}, {writer})?;",
                snake_case(name)
            )
            .unwrap();
        }
        Type::Optional(inner) => {
            writeln!(out, "{indent}match {value} {{").unwrap();
            writeln!(
                out,
                "{indent}    Some(payload_value) => {{\n{indent}        {writer}.write_optional(true)?;"
            )
            .unwrap();
            write_rust_encode_type(
                out,
                protocol,
                inner,
                "payload_value",
                &format!("{depth} + 1"),
                writer,
                &format!("{indent}        "),
            );
            writeln!(
                out,
                "{indent}    }}\n{indent}    None => {writer}.write_optional(false)?,\n{indent}}}"
            )
            .unwrap();
        }
        Type::List(inner, maximum) => {
            writeln!(
                out,
                "{indent}{{\n{indent}    {writer}.write_count(({value}).len(), {maximum})?;\n{indent}    for payload_item in {value} {{"
            )
            .unwrap();
            write_rust_encode_type(
                out,
                protocol,
                inner,
                "payload_item",
                &format!("{depth} + 1"),
                writer,
                &format!("{indent}        "),
            );
            writeln!(out, "{indent}    }}\n{indent}}}").unwrap();
        }
        Type::Bytes(maximum) => {
            writeln!(
                out,
                "{indent}{writer}.write_bytes(({value}).as_slice(), {maximum})?;"
            )
            .unwrap();
        }
    }
    let _ = protocol;
}

fn write_rust_encode_primitive(
    out: &mut String,
    primitive: Primitive,
    value: &str,
    writer: &str,
    indent: &str,
) {
    let expression = match primitive {
        Primitive::U8 => format!("{writer}.write(&[*({value})])?;"),
        Primitive::U16 => format!("{writer}.write(&(*({value})).to_le_bytes())?;"),
        Primitive::U32 => format!("{writer}.write(&(*({value})).to_le_bytes())?;"),
        Primitive::U64 => format!("{writer}.write(&(*({value})).to_le_bytes())?;"),
        Primitive::I32 => format!("{writer}.write(&(*({value})).to_le_bytes())?;"),
        Primitive::Bool => format!("{writer}.write(&[u8::from(*({value}))])?;"),
        Primitive::Bytes16 | Primitive::Bytes32 => format!("{writer}.write({value})?;"),
    };
    writeln!(out, "{indent}{expression}").unwrap();
}

fn rust_decode_type_expression(
    protocol: &Protocol,
    ty: &Type,
    depth: &str,
    reader: &str,
) -> String {
    match ty {
        Type::Primitive(primitive) => rust_decode_primitive_expression(*primitive, reader),
        Type::Named(name) => format!(
            "payload_decode_{}_from({depth}, {reader})?",
            snake_case(name)
        ),
        Type::Optional(inner) => format!(
            "if {reader}.read_optional()? {{ Some({}) }} else {{ None }}",
            rust_decode_type_expression(protocol, inner, &format!("{depth} + 1"), reader)
        ),
        Type::List(inner, maximum) => {
            let minimum = minimum_wire_size(protocol, inner).unwrap_or(0);
            let item =
                rust_decode_type_expression(protocol, inner, &format!("{depth} + 1"), reader);
            format!(
                "{{ let payload_count = {reader}.read_count({maximum}, {minimum})?; \
                 let mut payload_values = Vec::with_capacity(payload_count); \
                 for _ in 0..payload_count {{ payload_values.push({item}); }} payload_values }}"
            )
        }
        Type::Bytes(maximum) => format!("{reader}.read_bytes({maximum})?"),
    }
}

fn rust_decode_primitive_expression(primitive: Primitive, reader: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{reader}.read_u8()?"),
        Primitive::U16 => format!("{reader}.read_u16()?"),
        Primitive::U32 => format!("{reader}.read_u32()?"),
        Primitive::U64 => format!("{reader}.read_u64()?"),
        Primitive::I32 => format!("{reader}.read_i32()?"),
        Primitive::Bool => format!("{reader}.read_bool()?"),
        Primitive::Bytes16 => format!("{reader}.read_fixed_16()?"),
        Primitive::Bytes32 => format!("{reader}.read_fixed_32()?"),
    }
}

fn rust_write_tag_expression(primitive: Primitive, tag: u16, writer: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{writer}.write(&[{}])", tag as u8),
        Primitive::U16 => format!("{writer}.write(&({tag}_u16).to_le_bytes())"),
        Primitive::U32 => format!("{writer}.write(&({tag}_u32).to_le_bytes())"),
        Primitive::U64 => format!("{writer}.write(&({tag}_u64).to_le_bytes())"),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
            "Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidValue, writer.offset()))"
                .into()
        }
    }
}

fn rust_read_tag_expression(primitive: Primitive, reader: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{reader}.read_u8()?"),
        Primitive::U16 => format!("{reader}.read_u16()?"),
        Primitive::U32 => format!("{reader}.read_u32()?"),
        Primitive::U64 => format!("{reader}.read_u64()?"),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
            "return Err(PayloadCodecError::new(PayloadCodecErrorCode::InvalidValue, reader.offset()))"
                .into()
        }
    }
}

fn rust_tag_literal(primitive: Primitive, tag: u16) -> String {
    match primitive {
        Primitive::U8 => format!("{tag}_u8"),
        Primitive::U16 => format!("{tag}_u16"),
        Primitive::U32 => format!("{tag}_u32"),
        Primitive::U64 => format!("{tag}_u64"),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => "0".into(),
    }
}

fn write_typescript_runtime(out: &mut String, protocol: &Protocol) {
    writeln!(
        out,
        "\n// Canonical fixed_le_v1 typed payload codec.\n\
         export const DEFAULT_PAYLOAD_CODEC_MAX_DEPTH = {DEFAULT_MAX_DEPTH} as const;\n\
         export const DEFAULT_PAYLOAD_CODEC_MAX_CONTAINER_ITEMS = {DEFAULT_MAX_CONTAINER_ITEMS} as const;"
    )
    .unwrap();
    writeln!(
        out,
        "export const DEFAULT_PAYLOAD_CODEC_LIMITS: Readonly<PayloadCodecLimits> = Object.freeze({{ maxDepth: DEFAULT_PAYLOAD_CODEC_MAX_DEPTH, maxBytes: {}, maxContainerItems: DEFAULT_PAYLOAD_CODEC_MAX_CONTAINER_ITEMS }});",
        protocol.max_message_bytes
    )
    .unwrap();
    out.push_str(
        r#"
export type PayloadCodecErrorCode =
  | "LimitExceeded"
  | "Truncated"
  | "TrailingBytes"
  | "InvalidBooleanMarker"
  | "InvalidOptionalMarker"
  | "UnknownTag"
  | "UnknownMessage"
  | "InvalidValue"
  | "SharedArrayBuffer";

export interface PayloadCodecError {
  readonly code: PayloadCodecErrorCode;
  readonly offset: number;
}

export type PayloadCodecResult<T> =
  | Readonly<{ ok: true; value: T }>
  | Readonly<{ ok: false; error: Readonly<PayloadCodecError> }>;

export interface PayloadCodecLimits {
  readonly maxDepth: number;
  readonly maxBytes: number;
  readonly maxContainerItems: number;
}

class PayloadCodecFailure {
  readonly code: PayloadCodecErrorCode;
  readonly offset: number;

  constructor(code: PayloadCodecErrorCode, offset: number) {
    this.code = code;
    this.offset = offset;
  }
}

const payloadCodecOk = <T>(value: T): PayloadCodecResult<T> =>
  Object.freeze({ ok: true as const, value });

const payloadCodecError = <T>(
  code: PayloadCodecErrorCode,
  offset: number,
): PayloadCodecResult<T> => {
  const error = Object.freeze({ code, offset });
  return Object.freeze({ ok: false as const, error });
};

const payloadCodecFailureResult = <T>(failure: unknown): PayloadCodecResult<T> =>
  failure instanceof PayloadCodecFailure
    ? payloadCodecError(failure.code, failure.offset)
    : payloadCodecError("InvalidValue", 0);

const payloadCodecFail = (code: PayloadCodecErrorCode, offset: number): never => {
  throw new PayloadCodecFailure(code, offset);
};

const payloadCodecValidLimit = (value: number): boolean =>
  Number.isSafeInteger(value) && value > 0;

const payloadCodecNormalizeLimits = (
  limits: Readonly<PayloadCodecLimits>,
): Readonly<PayloadCodecLimits> => {
  if (
    typeof limits !== "object"
    || limits === null
    || !payloadCodecValidLimit(limits.maxDepth)
    || !payloadCodecValidLimit(limits.maxBytes)
    || !payloadCodecValidLimit(limits.maxContainerItems)
  ) {
    payloadCodecFail("LimitExceeded", 0);
  }
  return Object.freeze({
    maxDepth: limits.maxDepth,
    maxBytes: limits.maxBytes,
    maxContainerItems: limits.maxContainerItems,
  });
};

const payloadCodecCapBytes = (
  limits: Readonly<PayloadCodecLimits>,
  maximum: number,
): Readonly<PayloadCodecLimits> => Object.freeze({
  maxDepth: limits.maxDepth,
  maxBytes: Math.min(limits.maxBytes, maximum),
  maxContainerItems: limits.maxContainerItems,
});

const payloadCodecSnapshotHeader = (header: EnvelopeHeader): EnvelopeHeader => {
  const candidate = header as unknown as Record<string, unknown>;
  if (
    typeof candidate !== "object"
    || candidate === null
    || !Number.isInteger(candidate.major)
    || Number(candidate.major) < 0
    || Number(candidate.major) > 0xffff
    || !Number.isInteger(candidate.minor)
    || Number(candidate.minor) < 0
    || Number(candidate.minor) > 0xffff
    || !Number.isInteger(candidate.message_type)
    || Number(candidate.message_type) < 0
    || Number(candidate.message_type) > 0xffff
    || !Number.isInteger(candidate.flags)
    || Number(candidate.flags) < 0
    || Number(candidate.flags) > 0xffff
    || !Number.isInteger(candidate.payload_len)
    || Number(candidate.payload_len) < 0
    || Number(candidate.payload_len) > 0xffffffff
    || typeof candidate.sequence !== "bigint"
    || candidate.sequence < 0n
    || candidate.sequence > 0xffffffffffffffffn
  ) {
    return payloadCodecFail("InvalidValue", 0);
  }
  return Object.freeze({
    major: Number(candidate.major),
    minor: Number(candidate.minor),
    message_type: Number(candidate.message_type),
    flags: Number(candidate.flags),
    payload_len: Number(candidate.payload_len),
    sequence: candidate.sequence,
  });
};

const payloadCodecIsSharedArrayBufferView = (value: Uint8Array): boolean =>
  typeof SharedArrayBuffer !== "undefined" && value.buffer instanceof SharedArrayBuffer;

class PayloadWriter {
  private readonly output: number[] = [];
  private readonly limits: Readonly<PayloadCodecLimits>;
  private remainingContainerItems: number;

  constructor(limits: Readonly<PayloadCodecLimits>) {
    this.limits = payloadCodecNormalizeLimits(limits);
    this.remainingContainerItems = this.limits.maxContainerItems;
  }

  get offset(): number {
    return this.output.length;
  }

  checkDepth(depth: number): void {
    if (!Number.isSafeInteger(depth) || depth < 0 || depth > this.limits.maxDepth) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
  }

  private chargeContainer(count: number): void {
    if (!Number.isSafeInteger(count) || count < 0 || count > this.remainingContainerItems) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
    this.remainingContainerItems -= count;
  }

  private writeRaw(bytes: ArrayLike<number>): void {
    const length = this.output.length + bytes.length;
    if (!Number.isSafeInteger(length) || length > this.limits.maxBytes) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
    for (let index = 0; index < bytes.length; index += 1) {
      this.output.push(Number(bytes[index]));
    }
  }

  writeU8(value: number): void {
    if (!isU8(value)) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    this.writeRaw([value]);
  }

  writeU16(value: number): void {
    if (!Number.isInteger(value) || value < 0 || value > 0xffff) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    this.writeRaw([value & 0xff, (value >>> 8) & 0xff]);
  }

  writeU32(value: number): void {
    if (!Number.isInteger(value) || value < 0 || value > 0xffffffff) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    this.writeRaw([
      value & 0xff,
      (value >>> 8) & 0xff,
      (value >>> 16) & 0xff,
      Math.floor(value / 0x1000000) & 0xff,
    ]);
  }

  writeU64(value: bigint): void {
    if (typeof value !== "bigint" || value < 0n || value > 0xffffffffffffffffn) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    const bytes = new Array<number>(8);
    let remaining = value;
    for (let index = 0; index < 8; index += 1) {
      bytes[index] = Number(remaining & 0xffn);
      remaining >>= 8n;
    }
    this.writeRaw(bytes);
  }

  writeI32(value: number): void {
    if (!Number.isInteger(value) || value < -0x80000000 || value > 0x7fffffff) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    this.writeU32(value < 0 ? value + 0x100000000 : value);
  }

  writeBool(value: boolean): void {
    if (typeof value !== "boolean") {
      payloadCodecFail("InvalidValue", this.offset);
    }
    this.writeU8(value ? 1 : 0);
  }

  writeOptional(present: boolean): void {
    if (present) {
      this.chargeContainer(1);
    }
    this.writeU8(present ? 1 : 0);
  }

  writeCount(count: number, maximum: number): void {
    if (
      !Number.isInteger(count)
      || count < 0
      || count > maximum
      || count > 0xffffffff
    ) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
    this.chargeContainer(count);
    this.writeU32(count);
  }

  writeFixed(value: Uint8Array, length: number): void {
    if (!(value instanceof Uint8Array) || value.byteLength !== length) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    if (payloadCodecIsSharedArrayBufferView(value)) {
      payloadCodecFail("SharedArrayBuffer", this.offset);
    }
    this.writeRaw(value);
  }

  writeBytes(value: Uint8Array, maximum: number): void {
    if (!(value instanceof Uint8Array)) {
      payloadCodecFail("InvalidValue", this.offset);
    }
    if (payloadCodecIsSharedArrayBufferView(value)) {
      payloadCodecFail("SharedArrayBuffer", this.offset);
    }
    this.writeCount(value.byteLength, maximum);
    this.writeRaw(value);
  }

  finish(): Uint8Array {
    return Uint8Array.from(this.output);
  }
}

class PayloadReader {
  private readonly input: Uint8Array;
  private readonly limits: Readonly<PayloadCodecLimits>;
  private position = 0;
  private remainingContainerItems: number;

  constructor(input: Uint8Array, limits: Readonly<PayloadCodecLimits>) {
    this.limits = payloadCodecNormalizeLimits(limits);
    if (!(input instanceof Uint8Array)) {
      payloadCodecFail("InvalidValue", 0);
    }
    if (payloadCodecIsSharedArrayBufferView(input)) {
      payloadCodecFail("SharedArrayBuffer", 0);
    }
    if (input.byteLength > this.limits.maxBytes) {
      payloadCodecFail("LimitExceeded", 0);
    }
    this.input = Uint8Array.from(input);
    this.remainingContainerItems = this.limits.maxContainerItems;
  }

  get offset(): number {
    return this.position;
  }

  checkDepth(depth: number): void {
    if (!Number.isSafeInteger(depth) || depth < 0 || depth > this.limits.maxDepth) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
  }

  private chargeContainer(count: number): void {
    if (!Number.isSafeInteger(count) || count < 0 || count > this.remainingContainerItems) {
      payloadCodecFail("LimitExceeded", this.offset);
    }
    this.remainingContainerItems -= count;
  }

  private take(count: number): Uint8Array {
    const end = this.position + count;
    if (!Number.isSafeInteger(end) || count < 0 || end > this.input.byteLength) {
      payloadCodecFail("Truncated", this.position);
    }
    const bytes = this.input.subarray(this.position, end);
    this.position = end;
    return bytes;
  }

  readU8(): number {
    return Number(this.take(1)[0]);
  }

  readU16(): number {
    const bytes = this.take(2);
    return Number(bytes[0]) | (Number(bytes[1]) << 8);
  }

  readU32(): number {
    const bytes = this.take(4);
    return (
      Number(bytes[0])
      + Number(bytes[1]) * 0x100
      + Number(bytes[2]) * 0x10000
      + Number(bytes[3]) * 0x1000000
    );
  }

  readU64(): bigint {
    const bytes = this.take(8);
    let value = 0n;
    for (let index = 7; index >= 0; index -= 1) {
      value = (value << 8n) | BigInt(Number(bytes[index]));
    }
    return value;
  }

  readI32(): number {
    const value = this.readU32();
    return value > 0x7fffffff ? value - 0x100000000 : value;
  }

  readBool(): boolean {
    const offset = this.offset;
    const marker = this.readU8();
    if (marker === 0) return false;
    if (marker === 1) return true;
    return payloadCodecFail("InvalidBooleanMarker", offset);
  }

  readOptional(): boolean {
    const offset = this.offset;
    const marker = this.readU8();
    if (marker === 0) return false;
    if (marker === 1) {
      this.chargeContainer(1);
      return true;
    }
    return payloadCodecFail("InvalidOptionalMarker", offset);
  }

  readFixed(length: number): Uint8Array {
    return Uint8Array.from(this.take(length));
  }

  readCount(maximum: number, minimumItemBytes: number): number {
    const countOffset = this.offset;
    const count = this.readU32();
    if (count > maximum) {
      payloadCodecFail("LimitExceeded", countOffset);
    }
    this.chargeContainer(count);
    const minimum = count * minimumItemBytes;
    if (!Number.isSafeInteger(minimum)) {
      payloadCodecFail("LimitExceeded", countOffset);
    }
    if (minimum > this.input.byteLength - this.position) {
      payloadCodecFail("Truncated", this.position);
    }
    return count;
  }

  readBytes(maximum: number): Uint8Array {
    const count = this.readCount(maximum, 1);
    return Uint8Array.from(this.take(count));
  }

  finish(): void {
    if (this.position !== this.input.byteLength) {
      payloadCodecFail("TrailingBytes", this.position);
    }
  }
}

const payloadCodecEncodeRoot = <T>(
  value: T,
  limits: Readonly<PayloadCodecLimits>,
  encode: (value: T, depth: number, writer: PayloadWriter) => void,
): PayloadCodecResult<Uint8Array> => {
  try {
    const writer = new PayloadWriter(limits);
    encode(value, 0, writer);
    return payloadCodecOk(writer.finish());
  } catch (failure: unknown) {
    return payloadCodecFailureResult(failure);
  }
};

const payloadCodecDecodeRoot = <T>(
  input: Uint8Array,
  limits: Readonly<PayloadCodecLimits>,
  decode: (depth: number, reader: PayloadReader) => T,
): PayloadCodecResult<T> => {
  try {
    const reader = new PayloadReader(input, limits);
    const value = decode(0, reader);
    reader.finish();
    return payloadCodecOk(value);
  } catch (failure: unknown) {
    return payloadCodecFailureResult(failure);
  }
};
"#,
    );
}

fn write_typescript_scalar_codec(out: &mut String, scalar: &Scalar) {
    let camel = lower_camel_case(&scalar.name);
    writeln!(
        out,
        "function payloadEncode{}Into(value: {}, depth: number, writer: PayloadWriter): void {{\n  writer.checkDepth(depth);",
        scalar.name, scalar.name
    )
    .unwrap();
    write_typescript_encode_primitive(out, scalar.primitive, "value", "writer", "  ");
    out.push_str("}\n\n");
    writeln!(
        out,
        "function payloadDecode{}From(depth: number, reader: PayloadReader): {} {{\n  reader.checkDepth(depth);\n  return {} as {};\n}}\n",
        scalar.name,
        scalar.name,
        typescript_decode_primitive_expression(scalar.primitive, "reader"),
        scalar.name
    )
    .unwrap();
    write_typescript_root_functions(out, &scalar.name, &camel);
}

fn write_typescript_enum_codec(out: &mut String, enumeration: &EnumDef) {
    let camel = lower_camel_case(&enumeration.name);
    writeln!(
        out,
        "function payloadEncode{}Into(value: {}, depth: number, writer: PayloadWriter): void {{\n  writer.checkDepth(depth);\n  switch (value) {{",
        enumeration.name, enumeration.name
    )
    .unwrap();
    for variant in &enumeration.variants {
        writeln!(
            out,
            "    case {}.{}: {} return;",
            enumeration.name,
            variant.name,
            typescript_write_tag_statement(enumeration.repr, variant.tag, "writer")
        )
        .unwrap();
    }
    out.push_str(
        "    default: return payloadCodecFail(\"InvalidValue\", writer.offset);\n  }\n}\n\n",
    );
    writeln!(
        out,
        "function payloadDecode{}From(depth: number, reader: PayloadReader): {} {{\n  reader.checkDepth(depth);\n  const tagOffset = reader.offset;\n  const tag = {};\n  switch (tag) {{",
        enumeration.name,
        enumeration.name,
        typescript_read_tag_expression(enumeration.repr, "reader")
    )
    .unwrap();
    for variant in &enumeration.variants {
        writeln!(
            out,
            "    case {}: return {}.{};",
            typescript_tag_literal(enumeration.repr, variant.tag),
            enumeration.name,
            variant.name
        )
        .unwrap();
    }
    out.push_str("    default: return payloadCodecFail(\"UnknownTag\", tagOffset);\n  }\n}\n\n");
    write_typescript_root_functions(out, &enumeration.name, &camel);
}

fn write_typescript_record_codec(out: &mut String, protocol: &Protocol, record: &Record) {
    let camel = lower_camel_case(&record.name);
    writeln!(
        out,
        "function payloadEncode{}Into(value: {}, depth: number, writer: PayloadWriter): void {{\n  writer.checkDepth(depth);",
        record.name, record.name
    )
    .unwrap();
    if record.fields.is_empty() {
        out.push_str("  void value;\n");
    }
    for field in &record.fields {
        write_typescript_encode_type(
            out,
            protocol,
            &field.ty,
            &format!("value.{}", field.name),
            "depth + 1",
            "writer",
            "  ",
        );
    }
    out.push_str("}\n\n");

    writeln!(
        out,
        "function payloadDecode{}From(depth: number, reader: PayloadReader): {} {{\n  reader.checkDepth(depth);",
        record.name, record.name
    )
    .unwrap();
    for field in &record.fields {
        writeln!(
            out,
            "  const payload_{} = {};",
            field.name,
            typescript_decode_type_expression(protocol, &field.ty, "depth + 1", "reader")
        )
        .unwrap();
    }
    out.push_str("  return Object.freeze({\n");
    for field in &record.fields {
        if matches!(field.ty, Type::Optional(_)) {
            writeln!(
                out,
                "    ...(payload_{} === undefined ? {{}} : {{ {}: payload_{} }}),",
                field.name, field.name, field.name
            )
            .unwrap();
        } else {
            writeln!(out, "    {}: payload_{},", field.name, field.name).unwrap();
        }
    }
    writeln!(out, "  }}) as {};\n}}\n", record.name).unwrap();
    write_typescript_root_functions(out, &record.name, &camel);
}

fn write_typescript_union_codec(out: &mut String, protocol: &Protocol, union: &Union) {
    let variants = union
        .variants
        .iter()
        .filter(|variant| browser_union_variant(variant))
        .collect::<Vec<_>>();
    let camel = lower_camel_case(&union.name);
    writeln!(
        out,
        "function payloadEncode{}Into(value: {}, depth: number, writer: PayloadWriter): void {{\n  writer.checkDepth(depth);\n  switch (value.kind) {{",
        union.name, union.name
    )
    .unwrap();
    for variant in &variants {
        writeln!(out, "    case \"{}\":", variant.name).unwrap();
        writeln!(
            out,
            "      {}",
            typescript_write_tag_statement(union.repr, variant.tag, "writer")
        )
        .unwrap();
        for field in &variant.fields {
            write_typescript_encode_type(
                out,
                protocol,
                &field.ty,
                &format!("value.{}", field.name),
                "depth + 1",
                "writer",
                "      ",
            );
        }
        out.push_str("      return;\n");
    }
    out.push_str(
        "    default: return payloadCodecFail(\"InvalidValue\", writer.offset);\n  }\n}\n\n",
    );

    writeln!(
        out,
        "function payloadDecode{}From(depth: number, reader: PayloadReader): {} {{\n  reader.checkDepth(depth);\n  const tagOffset = reader.offset;\n  const tag = {};\n  switch (tag) {{",
        union.name,
        union.name,
        typescript_read_tag_expression(union.repr, "reader")
    )
    .unwrap();
    for variant in &variants {
        writeln!(
            out,
            "    case {}:",
            typescript_tag_literal(union.repr, variant.tag)
        )
        .unwrap();
        out.push_str("      return Object.freeze({\n");
        writeln!(out, "        kind: \"{}\" as const,", variant.name).unwrap();
        for field in &variant.fields {
            writeln!(
                out,
                "        {}: {},",
                field.name,
                typescript_decode_type_expression(protocol, &field.ty, "depth + 1", "reader")
            )
            .unwrap();
        }
        writeln!(out, "      }}) as {};", union.name).unwrap();
    }
    out.push_str("    default: return payloadCodecFail(\"UnknownTag\", tagOffset);\n  }\n}\n\n");
    write_typescript_root_functions(out, &union.name, &camel);
}

fn write_typescript_root_functions(out: &mut String, type_name: &str, camel: &str) {
    writeln!(
        out,
        "export function encode{}Payload(value: {}, limits: Readonly<PayloadCodecLimits> = DEFAULT_PAYLOAD_CODEC_LIMITS): PayloadCodecResult<Uint8Array> {{\n  return payloadCodecEncodeRoot(value, limits, payloadEncode{}Into);\n}}\n",
        type_name, type_name, type_name
    )
    .unwrap();
    writeln!(
        out,
        "export function decode{}Payload(input: Uint8Array, limits: Readonly<PayloadCodecLimits> = DEFAULT_PAYLOAD_CODEC_LIMITS): PayloadCodecResult<{}> {{\n  return payloadCodecDecodeRoot(input, limits, payloadDecode{}From);\n}}\n",
        type_name, type_name, type_name
    )
    .unwrap();
    let _ = camel;
}

fn write_typescript_dispatch(out: &mut String, protocol: &Protocol) {
    out.push_str(
        "export interface EncodedPayload { readonly messageId: number; readonly bytes: Uint8Array }\n\n",
    );
    for (kind, type_name, function) in [
        (MessageKind::Command, "Command", "Command"),
        (MessageKind::Event, "Event", "Event"),
    ] {
        let envelope_name = format!("{type_name}Envelope");
        let envelope_field = type_name.to_ascii_lowercase();
        writeln!(
            out,
            "export function encode{function}Payload(value: {envelope_name}, limits: Readonly<PayloadCodecLimits> = DEFAULT_PAYLOAD_CODEC_LIMITS): PayloadCodecResult<EncodedPayload> {{\n  try {{\n    const header = payloadCodecSnapshotHeader(value.header);\n    const message = value.{envelope_field};\n    switch (message.type) {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "      case \"{}\": {{\n        if (header.message_type !== {}) return payloadCodecError(\"InvalidValue\", 0);\n        const payload: {} = message.payload;\n        const encoded = payloadCodecEncodeRoot(value, payloadCodecCapBytes(limits, {}), (envelope, depth, writer) => {{\n          payloadEncodeCorrelationInto(envelope.correlation, depth, writer);\n          payloadEncode{}Into(payload, depth, writer);\n        }});\n        if (!encoded.ok) return encoded;\n        if (header.payload_len !== encoded.value.byteLength) return payloadCodecError(\"InvalidValue\", 0);\n        return payloadCodecOk(Object.freeze({{ messageId: {}, bytes: encoded.value }}));\n      }}",
                message.name,
                message.id,
                message.payload,
                message.max_payload_bytes,
                message.payload,
                message.id
            )
            .unwrap();
        }
        out.push_str(
            "      default: return payloadCodecError(\"UnknownMessage\", 0);\n    }\n  } catch (failure: unknown) {\n    return payloadCodecFailureResult(failure);\n  }\n}\n\n",
        );

        writeln!(
            out,
            "export function decode{function}Payload(headerInput: EnvelopeHeader, input: Uint8Array, limits: Readonly<PayloadCodecLimits> = DEFAULT_PAYLOAD_CODEC_LIMITS): PayloadCodecResult<{envelope_name}> {{\n  let header: EnvelopeHeader;\n  try {{\n    header = payloadCodecSnapshotHeader(headerInput);\n    if (!(input instanceof Uint8Array) || header.payload_len !== input.byteLength) return payloadCodecError(\"InvalidValue\", 0);\n  }} catch (failure: unknown) {{\n    return payloadCodecFailureResult(failure);\n  }}\n  switch (header.message_type) {{"
        )
        .unwrap();
        for message in protocol
            .messages
            .iter()
            .filter(|message| message.kind == kind)
        {
            writeln!(
                out,
                "    case {}:\n      return payloadCodecDecodeRoot(input, payloadCodecCapBytes(limits, {}), (depth, reader) => {{\n        const correlation = payloadDecodeCorrelationFrom(depth, reader);\n        const {envelope_field} = Object.freeze({{ type: \"{}\" as const, payload: payloadDecode{}From(depth, reader) }});\n        return Object.freeze({{ header, correlation, {envelope_field} }});\n      }});",
                message.id, message.max_payload_bytes, message.name, message.payload
            )
            .unwrap();
        }
        out.push_str("    default: return payloadCodecError(\"UnknownMessage\", 0);\n  }\n}\n\n");
    }
}

fn write_typescript_hash_preimage_helpers(out: &mut String, protocol: &Protocol) {
    let targets = hash_preimage_targets(protocol);
    if targets.is_empty() {
        return;
    }
    out.push_str(
        r#"const payloadCodecHashPreimage = (
  domain: string,
  payload: Uint8Array,
): PayloadCodecResult<Uint8Array> => {
  try {
    const domainBytes = new TextEncoder().encode(domain);
    const lengthOffset = domainBytes.byteLength + 1;
    const payloadOffset = lengthOffset + 8;
    const totalLength = payloadOffset + payload.byteLength;
    if (!Number.isSafeInteger(totalLength)) return payloadCodecError("LimitExceeded", 0);
    const preimage = new Uint8Array(totalLength);
    preimage.set(domainBytes, 0);
    preimage[domainBytes.byteLength] = 0;
    let payloadLength = BigInt(payload.byteLength);
    for (let index = 0; index < 8; index += 1) {
      preimage[lengthOffset + index] = Number(payloadLength & 0xffn);
      payloadLength >>= 8n;
    }
    preimage.set(payload, payloadOffset);
    return payloadCodecOk(preimage);
  } catch (failure: unknown) {
    return payloadCodecFailureResult(failure);
  }
};

"#,
    );
    for (type_name, _, domain) in targets {
        let function = lower_camel_case(type_name);
        writeln!(
            out,
            "export function {function}HashPreimage(value: {type_name}, limits: Readonly<PayloadCodecLimits> = DEFAULT_PAYLOAD_CODEC_LIMITS): PayloadCodecResult<Uint8Array> {{\n  const encoded = encode{type_name}Payload(value, limits);\n  if (!encoded.ok) return encoded;\n  return payloadCodecHashPreimage({domain}, encoded.value);\n}}\n"
        )
        .unwrap();
    }
}

fn write_typescript_encode_type(
    out: &mut String,
    protocol: &Protocol,
    ty: &Type,
    value: &str,
    depth: &str,
    writer: &str,
    indent: &str,
) {
    match ty {
        Type::Primitive(primitive) => {
            write_typescript_encode_primitive(out, *primitive, value, writer, indent);
        }
        Type::Named(name) => {
            writeln!(
                out,
                "{indent}payloadEncode{name}Into({value}, {depth}, {writer});"
            )
            .unwrap();
        }
        Type::Optional(inner) => {
            writeln!(
                out,
                "{indent}{{\n{indent}  const payloadOptional = {value};\n{indent}  if (payloadOptional === undefined) {{\n{indent}    {writer}.writeOptional(false);\n{indent}  }} else {{\n{indent}    {writer}.writeOptional(true);"
            )
            .unwrap();
            write_typescript_encode_type(
                out,
                protocol,
                inner,
                "payloadOptional",
                &format!("{depth} + 1"),
                writer,
                &format!("{indent}    "),
            );
            writeln!(out, "{indent}  }}\n{indent}}}").unwrap();
        }
        Type::List(inner, maximum) => {
            writeln!(
                out,
                "{indent}{{\n{indent}  const payloadList = {value};\n{indent}  if (!Array.isArray(payloadList)) payloadCodecFail(\"InvalidValue\", {writer}.offset);\n{indent}  {writer}.writeCount(payloadList.length, {maximum});\n{indent}  for (const payloadItem of payloadList) {{"
            )
            .unwrap();
            write_typescript_encode_type(
                out,
                protocol,
                inner,
                "payloadItem",
                &format!("{depth} + 1"),
                writer,
                &format!("{indent}    "),
            );
            writeln!(out, "{indent}  }}\n{indent}}}").unwrap();
        }
        Type::Bytes(maximum) => {
            writeln!(out, "{indent}{writer}.writeBytes({value}, {maximum});").unwrap();
        }
    }
    let _ = protocol;
}

fn write_typescript_encode_primitive(
    out: &mut String,
    primitive: Primitive,
    value: &str,
    writer: &str,
    indent: &str,
) {
    let method = match primitive {
        Primitive::U8 => "writeU8",
        Primitive::U16 => "writeU16",
        Primitive::U32 => "writeU32",
        Primitive::U64 => "writeU64",
        Primitive::I32 => "writeI32",
        Primitive::Bool => "writeBool",
        Primitive::Bytes16 | Primitive::Bytes32 => "writeFixed",
    };
    match primitive {
        Primitive::Bytes16 => {
            writeln!(out, "{indent}{writer}.{method}({value}, 16);").unwrap();
        }
        Primitive::Bytes32 => {
            writeln!(out, "{indent}{writer}.{method}({value}, 32);").unwrap();
        }
        _ => {
            writeln!(out, "{indent}{writer}.{method}({value});").unwrap();
        }
    }
}

fn typescript_decode_type_expression(
    protocol: &Protocol,
    ty: &Type,
    depth: &str,
    reader: &str,
) -> String {
    match ty {
        Type::Primitive(primitive) => typescript_decode_primitive_expression(*primitive, reader),
        Type::Named(name) => format!("payloadDecode{name}From({depth}, {reader})"),
        Type::Optional(inner) => format!(
            "{reader}.readOptional() ? {} : undefined",
            typescript_decode_type_expression(protocol, inner, &format!("{depth} + 1"), reader)
        ),
        Type::List(inner, maximum) => {
            let minimum = minimum_wire_size(protocol, inner).unwrap_or(0);
            let item =
                typescript_decode_type_expression(protocol, inner, &format!("{depth} + 1"), reader);
            format!(
                "(() => {{ const payloadCount = {reader}.readCount({maximum}, {minimum}); \
                 const payloadValues = []; for (let index = 0; index < payloadCount; index += 1) \
                 payloadValues.push({item}); return Object.freeze(payloadValues) as unknown as {}[]; }})()",
                typescript_type(inner)
            )
        }
        Type::Bytes(maximum) => format!("{reader}.readBytes({maximum})"),
    }
}

fn typescript_decode_primitive_expression(primitive: Primitive, reader: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{reader}.readU8()"),
        Primitive::U16 => format!("{reader}.readU16()"),
        Primitive::U32 => format!("{reader}.readU32()"),
        Primitive::U64 => format!("{reader}.readU64()"),
        Primitive::I32 => format!("{reader}.readI32()"),
        Primitive::Bool => format!("{reader}.readBool()"),
        Primitive::Bytes16 => format!("{reader}.readFixed(16)"),
        Primitive::Bytes32 => format!("{reader}.readFixed(32)"),
    }
}

fn typescript_write_tag_statement(primitive: Primitive, tag: u16, writer: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{writer}.writeU8({tag});"),
        Primitive::U16 => format!("{writer}.writeU16({tag});"),
        Primitive::U32 => format!("{writer}.writeU32({tag});"),
        Primitive::U64 => format!("{writer}.writeU64({tag}n);"),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
            format!("payloadCodecFail(\"InvalidValue\", {writer}.offset);")
        }
    }
}

fn typescript_read_tag_expression(primitive: Primitive, reader: &str) -> String {
    match primitive {
        Primitive::U8 => format!("{reader}.readU8()"),
        Primitive::U16 => format!("{reader}.readU16()"),
        Primitive::U32 => format!("{reader}.readU32()"),
        Primitive::U64 => format!("{reader}.readU64()"),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
            format!("payloadCodecFail(\"InvalidValue\", {reader}.offset)")
        }
    }
}

fn typescript_tag_literal(primitive: Primitive, tag: u16) -> String {
    if primitive == Primitive::U64 {
        format!("{tag}n")
    } else {
        tag.to_string()
    }
}

fn minimum_wire_size(protocol: &Protocol, ty: &Type) -> Option<usize> {
    minimum_type_size(protocol, ty, &mut BTreeMap::new(), &mut BTreeSet::new())
}

fn minimum_type_size(
    protocol: &Protocol,
    ty: &Type,
    cache: &mut BTreeMap<String, usize>,
    visiting: &mut BTreeSet<String>,
) -> Option<usize> {
    match ty {
        Type::Primitive(primitive) => Some(primitive_width(*primitive)),
        Type::Named(name) => minimum_named_size(protocol, name, cache, visiting),
        Type::Optional(_) => Some(1),
        Type::List(_, _) | Type::Bytes(_) => Some(4),
    }
}

fn minimum_named_size(
    protocol: &Protocol,
    name: &str,
    cache: &mut BTreeMap<String, usize>,
    visiting: &mut BTreeSet<String>,
) -> Option<usize> {
    if let Some(value) = cache.get(name) {
        return Some(*value);
    }
    if !visiting.insert(name.to_owned()) {
        return None;
    }
    let value = if let Some(scalar) = protocol.scalars.iter().find(|value| value.name == name) {
        primitive_width(scalar.primitive)
    } else if let Some(enumeration) = protocol.enums.iter().find(|value| value.name == name) {
        primitive_width(enumeration.repr)
    } else if let Some(record) = protocol.records.iter().find(|value| value.name == name) {
        let mut total = 0_usize;
        for field in &record.fields {
            total = total.checked_add(minimum_type_size(protocol, &field.ty, cache, visiting)?)?;
        }
        total
    } else if let Some(union) = protocol.unions.iter().find(|value| value.name == name) {
        let tag = primitive_width(union.repr);
        let mut minimum = None;
        for variant in &union.variants {
            let mut size = tag;
            for field in &variant.fields {
                size =
                    size.checked_add(minimum_type_size(protocol, &field.ty, cache, visiting)?)?;
            }
            minimum = Some(minimum.map_or(size, |current: usize| current.min(size)));
        }
        minimum?
    } else {
        return None;
    };
    visiting.remove(name);
    cache.insert(name.to_owned(), value);
    Some(value)
}

const fn primitive_width(primitive: Primitive) -> usize {
    match primitive {
        Primitive::U8 | Primitive::Bool => 1,
        Primitive::U16 => 2,
        Primitive::U32 | Primitive::I32 => 4,
        Primitive::U64 => 8,
        Primitive::Bytes16 => 16,
        Primitive::Bytes32 => 32,
    }
}

fn typescript_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(Primitive::U8)
        | Type::Primitive(Primitive::U16)
        | Type::Primitive(Primitive::U32)
        | Type::Primitive(Primitive::I32) => "number".into(),
        Type::Primitive(Primitive::U64) => "bigint".into(),
        Type::Primitive(Primitive::Bool) => "boolean".into(),
        Type::Primitive(Primitive::Bytes16 | Primitive::Bytes32) | Type::Bytes(_) => {
            "Uint8Array".into()
        }
        Type::Named(name) => name.clone(),
        Type::Optional(inner) => typescript_type(inner),
        Type::List(inner, _) => format!("{}[]", typescript_type(inner)),
    }
}

fn browser_union_variant(variant: &UnionVariant) -> bool {
    !matches!(
        variant.required_capability.as_deref(),
        Some("SharedMemory" | "LocalMemory")
    )
}

fn protocol_uses_variable_bytes(protocol: &Protocol) -> bool {
    protocol
        .records
        .iter()
        .flat_map(|record| record.fields.iter().map(|field| &field.ty))
        .chain(protocol.unions.iter().flat_map(|union| {
            union
                .variants
                .iter()
                .flat_map(|variant| variant.fields.iter().map(|field| &field.ty))
        }))
        .any(type_contains_variable_bytes)
}

fn type_contains_variable_bytes(ty: &Type) -> bool {
    match ty {
        Type::Bytes(_) => true,
        Type::Optional(inner) | Type::List(inner, _) => type_contains_variable_bytes(inner),
        Type::Primitive(_) | Type::Named(_) => false,
    }
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
    String::from_utf8(bytes).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Field, Message, Outcome, Presence, Privacy, Record, Scalar, Union, UnionField,
        UnionVariant, Variant,
    };

    fn fixture() -> Protocol {
        Protocol {
            name: "CodecFixture".into(),
            major: 0,
            minor: 1,
            max_message_bytes: 4_096,
            max_transfer_slots: 4,
            max_data_segment_bytes: 1_024,
            max_data_ticket_bytes: 4_096,
            payload_codec: "fixed_le_v1".into(),
            scalars: vec![Scalar {
                name: "Ticket".into(),
                primitive: Primitive::U64,
            }],
            enums: vec![EnumDef {
                name: "Mode".into(),
                repr: Primitive::U8,
                variants: vec![
                    Variant {
                        name: "One".into(),
                        tag: 1,
                    },
                    Variant {
                        name: "Two".into(),
                        tag: 2,
                    },
                ],
            }],
            records: vec![Record {
                name: "DoCommand".into(),
                fields: vec![
                    Field {
                        name: "ticket".into(),
                        ty: Type::Named("Ticket".into()),
                        presence: Presence::Required,
                        privacy: Privacy::Public,
                    },
                    Field {
                        name: "note".into(),
                        ty: Type::Optional(Box::new(Type::Bytes(8))),
                        presence: Presence::Optional,
                        privacy: Privacy::Private,
                    },
                    Field {
                        name: "items".into(),
                        ty: Type::List(Box::new(Type::Named("Choice".into())), 4),
                        presence: Presence::Required,
                        privacy: Privacy::Public,
                    },
                ],
            }],
            unions: vec![Union {
                name: "Choice".into(),
                repr: Primitive::U16,
                variants: vec![
                    UnionVariant {
                        name: "Browser".into(),
                        tag: 1,
                        required_capability: Some("SharedArrayBuffer".into()),
                        fields: vec![UnionField {
                            name: "mode".into(),
                            ty: Type::Named("Mode".into()),
                            privacy: Privacy::Public,
                        }],
                    },
                    UnionVariant {
                        name: "Desktop".into(),
                        tag: 2,
                        required_capability: Some("SharedMemory".into()),
                        fields: vec![UnionField {
                            name: "slot".into(),
                            ty: Type::Primitive(Primitive::U16),
                            privacy: Privacy::Sensitive,
                        }],
                    },
                ],
            }],
            messages: vec![Message {
                kind: MessageKind::Command,
                name: "Do".into(),
                id: 7,
                payload: "DoCommand".into(),
                state: "Ready".into(),
                correlation: "Worker".into(),
                disposition: "no".into(),
                allowed_flags: 0,
                min_transfer_slots: 0,
                max_transfer_slots: 0,
                max_payload_bytes: 128,
                required_capability: None,
                outcomes: Vec::<Outcome>::new(),
            }],
        }
    }

    #[test]
    fn rust_generation_contains_typed_exact_bounded_dispatch() {
        let generated = generate_rust_payload_codec(&fixture());
        assert!(generated.contains("pub enum PayloadCodecErrorCode"));
        assert!(generated.contains("InvalidBooleanMarker"));
        assert!(generated.contains("InvalidOptionalMarker"));
        assert!(generated.contains("fn payload_encode_do_command_into"));
        assert!(generated.contains("reader.finish()?"));
        assert!(generated.contains("read_count(4, 3)"));
        assert!(generated.contains("7 => Command::Do"));
        assert!(generated.contains("Choice::Desktop"));
    }

    #[test]
    fn typescript_generation_is_frozen_owned_and_browser_projected() {
        let generated = generate_typescript_payload_codec(&fixture());
        assert!(generated.contains("Object.freeze({ ok: true as const, value })"));
        assert!(generated.contains("Uint8Array.from(input)"));
        assert!(generated.contains("payloadCodecIsSharedArrayBufferView"));
        assert!(generated.contains("InvalidBooleanMarker"));
        assert!(generated.contains("InvalidOptionalMarker"));
        assert!(generated.contains("case \"Browser\""));
        assert!(!generated.contains("case \"Desktop\""));
        assert!(generated.contains("case 7:"));
    }

    #[test]
    fn hash_preimages_use_frozen_domain_length_payload_framing() {
        let mut protocol = fixture();
        protocol.records.extend([
            Record {
                name: "CapabilityDecision".into(),
                fields: Vec::new(),
            },
            Record {
                name: "RenderPlanManifest".into(),
                fields: Vec::new(),
            },
        ]);

        let rust = generate_rust_payload_codec(&protocol);
        assert!(rust.contains("pub fn capability_decision_hash_preimage"));
        assert!(rust.contains("pub fn render_plan_manifest_hash_preimage"));
        assert!(rust.contains("preimage.extend_from_slice(domain.as_bytes())"));
        assert!(rust.contains("preimage.push(0)"));
        assert!(rust.contains("payload_len.to_le_bytes()"));
        assert!(rust.contains("encode_capability_decision_payload(value, limits)?"));

        let typescript = generate_typescript_payload_codec(&protocol);
        assert!(typescript.contains("export function capabilityDecisionHashPreimage"));
        assert!(typescript.contains("export function renderPlanManifestHashPreimage"));
        assert!(typescript.contains("new TextEncoder().encode(domain)"));
        assert!(typescript.contains("preimage[domainBytes.byteLength] = 0"));
        assert!(typescript.contains("let payloadLength = BigInt(payload.byteLength)"));
        assert!(typescript.contains("encodeCapabilityDecisionPayload(value, limits)"));
    }
}
