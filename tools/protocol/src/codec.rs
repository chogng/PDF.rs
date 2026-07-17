//! Bounded generic implementation of the canonical `fixed_le_v1` payload codec.
//!
//! This module deliberately operates on a small schema-shaped value tree instead of generated
//! Rust types. The generator can therefore use one implementation to build byte-exact fixtures
//! and to check that generated, language-specific codecs agree with the canonical wire format.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::{
    EnumDef, Presence, Primitive, Protocol, Record, Scalar, Type, Union, UnionVariant,
};

const FIXED_LE_V1: &str = "fixed_le_v1";

/// Resource limits applied independently to every encode or decode operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CodecLimits {
    /// Maximum number of schema/value edges followed from the root.
    pub(crate) max_depth: usize,
    /// Maximum encoded payload size.
    pub(crate) max_bytes: usize,
    /// Cumulative maximum for optional values, list elements, and variable-length bytes.
    pub(crate) max_container_items: usize,
}

impl CodecLimits {
    /// Creates explicit fail-closed codec limits.
    pub(crate) const fn new(
        max_depth: usize,
        max_bytes: usize,
        max_container_items: usize,
    ) -> Self {
        Self {
            max_depth,
            max_bytes,
            max_container_items,
        }
    }
}

/// Schema-shaped generic value accepted and returned by [`FixedLeCodec`].
///
/// Record and union fields are named so callers cannot accidentally rely on their input order.
/// The encoder rejects missing, duplicate, and unknown fields, then emits values in schema order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WireValue {
    /// An unsigned eight-bit integer.
    U8(u8),
    /// An unsigned sixteen-bit integer.
    U16(u16),
    /// An unsigned thirty-two-bit integer.
    U32(u32),
    /// An unsigned sixty-four-bit integer.
    U64(u64),
    /// A signed thirty-two-bit integer.
    I32(i32),
    /// A canonical boolean.
    Bool(bool),
    /// Exactly sixteen raw bytes.
    Bytes16([u8; 16]),
    /// Exactly thirty-two raw bytes.
    Bytes32([u8; 32]),
    /// A schema-bounded variable-length byte string.
    Bytes(Vec<u8>),
    /// An enum variant name.
    Enum(String),
    /// An optional value. `None` encodes as marker zero and `Some` as marker one.
    Optional(Option<Box<WireValue>>),
    /// A schema-bounded list.
    List(Vec<WireValue>),
    /// Record fields keyed by their schema names.
    Record(Vec<(String, WireValue)>),
    /// A tagged-union variant and its named fields.
    Union {
        /// Variant name from the schema.
        variant: String,
        /// Variant fields keyed by their schema names.
        fields: Vec<(String, WireValue)>,
    },
}

/// Stable categories for codec failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodecErrorKind {
    /// The protocol does not select `fixed_le_v1`.
    UnsupportedCodec,
    /// The model violates an invariant normally enforced by the schema parser.
    InvalidSchema,
    /// A named type could not be resolved.
    UnknownType,
    /// The named type graph is recursive.
    RecursiveType,
    /// The value variant does not match the schema type.
    TypeMismatch,
    /// A record or union value omits a schema field.
    MissingField,
    /// A record or union value contains a field absent from the schema.
    UnknownField,
    /// A record or union value repeats a field.
    DuplicateField,
    /// An enum or union value names an absent variant.
    UnknownVariant,
    /// A decoded enum or union discriminant is not declared by the schema.
    UnknownTag,
    /// A decoded boolean byte is not canonical zero or one.
    InvalidBooleanMarker,
    /// A decoded optional byte is not canonical zero or one.
    InvalidOptionalMarker,
    /// A depth, byte, schema-container, or cumulative-container limit was exceeded.
    LimitExceeded,
    /// The payload ended before the schema value was complete.
    Truncated,
    /// Bytes remain after decoding one complete root value.
    TrailingBytes,
}

/// A bounded codec error with the payload offset at which it was detected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodecError {
    /// Machine-comparable failure category.
    pub(crate) kind: CodecErrorKind,
    /// Encode output or decode input offset associated with the failure.
    pub(crate) offset: usize,
    /// Short diagnostic context.
    pub(crate) detail: String,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} at byte {}: {}",
            self.kind, self.offset, self.detail
        )
    }
}

impl std::error::Error for CodecError {}

#[derive(Clone, Copy)]
enum NamedType<'a> {
    Scalar(&'a Scalar),
    Enum(&'a EnumDef),
    Record(&'a Record),
    Union(&'a Union),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Complete,
}

/// Resolves a protocol model and encodes or decodes canonical `fixed_le_v1` values.
pub(crate) struct FixedLeCodec<'a> {
    named_types: BTreeMap<String, NamedType<'a>>,
    limits: CodecLimits,
}

impl<'a> FixedLeCodec<'a> {
    /// Builds an indexed codec and rejects invalid or recursive type graphs before processing
    /// untrusted values.
    pub(crate) fn new(protocol: &'a Protocol, mut limits: CodecLimits) -> Result<Self, CodecError> {
        if protocol.payload_codec != FIXED_LE_V1 {
            return Err(error(
                CodecErrorKind::UnsupportedCodec,
                0,
                format!("expected {FIXED_LE_V1}, found {}", protocol.payload_codec),
            ));
        }
        if protocol.max_message_bytes == 0
            || limits.max_bytes == 0
            || limits.max_depth == 0
            || limits.max_container_items == 0
        {
            return Err(error(
                CodecErrorKind::InvalidSchema,
                0,
                "codec and protocol limits must be nonzero",
            ));
        }
        limits.max_bytes = limits
            .max_bytes
            .min(usize::try_from(protocol.max_message_bytes).unwrap_or(usize::MAX));

        let mut named_types = BTreeMap::new();
        for scalar in &protocol.scalars {
            insert_named_type(&mut named_types, &scalar.name, NamedType::Scalar(scalar))?;
        }
        for enumeration in &protocol.enums {
            insert_named_type(
                &mut named_types,
                &enumeration.name,
                NamedType::Enum(enumeration),
            )?;
        }
        for record in &protocol.records {
            insert_named_type(&mut named_types, &record.name, NamedType::Record(record))?;
        }
        for union in &protocol.unions {
            insert_named_type(&mut named_types, &union.name, NamedType::Union(union))?;
        }

        let codec = Self {
            named_types,
            limits,
        };
        codec.validate_schema()?;
        Ok(codec)
    }

    /// Encodes one value described by an arbitrary model type.
    pub(crate) fn encode(&self, ty: &Type, value: &WireValue) -> Result<Vec<u8>, CodecError> {
        let mut state = EncodeState::new(self.limits);
        self.encode_type(ty, value, 0, &mut state)?;
        Ok(state.output)
    }

    /// Encodes one value using a named scalar, enum, record, or union as its root.
    pub(crate) fn encode_named(
        &self,
        type_name: &str,
        value: &WireValue,
    ) -> Result<Vec<u8>, CodecError> {
        let mut state = EncodeState::new(self.limits);
        self.encode_named_type(type_name, value, 0, &mut state)?;
        Ok(state.output)
    }

    /// Decodes exactly one value described by an arbitrary model type.
    pub(crate) fn decode(&self, ty: &Type, input: &[u8]) -> Result<WireValue, CodecError> {
        self.check_input_size(input)?;
        let mut state = DecodeState::new(input, self.limits);
        let value = self.decode_type(ty, 0, &mut state)?;
        state.reject_trailing()?;
        Ok(value)
    }

    /// Decodes exactly one value using a named scalar, enum, record, or union as its root.
    pub(crate) fn decode_named(
        &self,
        type_name: &str,
        input: &[u8],
    ) -> Result<WireValue, CodecError> {
        self.check_input_size(input)?;
        let mut state = DecodeState::new(input, self.limits);
        let value = self.decode_named_type(type_name, 0, &mut state)?;
        state.reject_trailing()?;
        Ok(value)
    }

    /// Decodes and canonically re-encodes an arbitrary model type.
    pub(crate) fn reencode(&self, ty: &Type, input: &[u8]) -> Result<Vec<u8>, CodecError> {
        let value = self.decode(ty, input)?;
        self.encode(ty, &value)
    }

    /// Decodes and canonically re-encodes a named root type.
    pub(crate) fn reencode_named(
        &self,
        type_name: &str,
        input: &[u8],
    ) -> Result<Vec<u8>, CodecError> {
        let value = self.decode_named(type_name, input)?;
        self.encode_named(type_name, &value)
    }

    /// Computes the checked schema-maximum encoded size for an arbitrary model type.
    pub(crate) fn maximum_encoded_size(&self, ty: &Type) -> Result<usize, CodecError> {
        self.maximum_type_size(ty, 0, &mut BTreeMap::new(), &mut BTreeSet::new())
    }

    /// Computes the checked schema-maximum encoded size for a named root type.
    pub(crate) fn maximum_named_encoded_size(&self, type_name: &str) -> Result<usize, CodecError> {
        self.maximum_named_wire_size(type_name, 0, &mut BTreeMap::new(), &mut BTreeSet::new())
    }

    fn check_input_size(&self, input: &[u8]) -> Result<(), CodecError> {
        if input.len() > self.limits.max_bytes {
            Err(error(
                CodecErrorKind::LimitExceeded,
                0,
                format!(
                    "payload is {} bytes; byte budget is {}",
                    input.len(),
                    self.limits.max_bytes
                ),
            ))
        } else {
            Ok(())
        }
    }

    fn validate_schema(&self) -> Result<(), CodecError> {
        for reserved in [
            "u8", "u16", "u32", "u64", "i32", "bool", "bytes16", "bytes32",
        ] {
            if self.named_types.contains_key(reserved) {
                return Err(error(
                    CodecErrorKind::InvalidSchema,
                    0,
                    format!("named type shadows primitive {reserved}"),
                ));
            }
        }

        let mut states = BTreeMap::new();
        for name in self.named_types.keys() {
            self.validate_named_type(name, 0, &mut states)?;
        }
        let mut depth_cache = BTreeMap::new();
        for name in self.named_types.keys() {
            let maximum = self.maximum_named_depth(name, &mut depth_cache, &mut BTreeSet::new())?;
            self.check_depth(maximum, 0)?;
        }
        Ok(())
    }

    fn validate_named_type(
        &self,
        name: &str,
        depth: usize,
        states: &mut BTreeMap<String, VisitState>,
    ) -> Result<(), CodecError> {
        self.check_depth(depth, 0)?;
        match states.get(name) {
            Some(VisitState::Visiting) => {
                return Err(error(
                    CodecErrorKind::RecursiveType,
                    0,
                    format!("recursive edge reaches {name}"),
                ));
            }
            Some(VisitState::Complete) => return Ok(()),
            None => {}
        }

        let definition = self.resolve(name)?;
        states.insert(name.to_owned(), VisitState::Visiting);
        match definition {
            NamedType::Scalar(scalar) => self.validate_primitive(scalar.primitive, &scalar.name)?,
            NamedType::Enum(enumeration) => {
                self.validate_variants(
                    enumeration.repr,
                    &enumeration.name,
                    enumeration
                        .variants
                        .iter()
                        .map(|variant| (variant.name.as_str(), variant.tag)),
                )?;
            }
            NamedType::Record(record) => {
                let mut fields = BTreeSet::new();
                for field in &record.fields {
                    if !fields.insert(field.name.as_str()) {
                        return Err(error(
                            CodecErrorKind::InvalidSchema,
                            0,
                            format!("duplicate field {}.{}", record.name, field.name),
                        ));
                    }
                    let optional_type = matches!(field.ty, Type::Optional(_));
                    if (field.presence == Presence::Optional) != optional_type {
                        return Err(error(
                            CodecErrorKind::InvalidSchema,
                            0,
                            format!(
                                "optional presence/type mismatch for {}.{}",
                                record.name, field.name
                            ),
                        ));
                    }
                    self.validate_type(&field.ty, depth + 1, states)?;
                }
            }
            NamedType::Union(union) => {
                self.validate_variants(
                    union.repr,
                    &union.name,
                    union
                        .variants
                        .iter()
                        .map(|variant| (variant.name.as_str(), variant.tag)),
                )?;
                for variant in &union.variants {
                    let mut fields = BTreeSet::new();
                    for field in &variant.fields {
                        if !fields.insert(field.name.as_str()) {
                            return Err(error(
                                CodecErrorKind::InvalidSchema,
                                0,
                                format!(
                                    "duplicate field {}.{}.{}",
                                    union.name, variant.name, field.name
                                ),
                            ));
                        }
                        self.validate_type(&field.ty, depth + 1, states)?;
                    }
                }
            }
        }
        states.insert(name.to_owned(), VisitState::Complete);
        Ok(())
    }

    fn validate_type(
        &self,
        ty: &Type,
        depth: usize,
        states: &mut BTreeMap<String, VisitState>,
    ) -> Result<(), CodecError> {
        self.check_depth(depth, 0)?;
        match ty {
            Type::Primitive(primitive) => self.validate_primitive(*primitive, "primitive"),
            Type::Named(name) => self.validate_named_type(name, depth, states),
            Type::Optional(inner) => {
                if matches!(inner.as_ref(), Type::Optional(_)) {
                    return Err(error(
                        CodecErrorKind::InvalidSchema,
                        0,
                        "redundant nested optional",
                    ));
                }
                self.validate_type(inner, depth + 1, states)
            }
            Type::List(inner, maximum) => {
                if *maximum == 0 {
                    return Err(error(
                        CodecErrorKind::InvalidSchema,
                        0,
                        "list maximum must be nonzero",
                    ));
                }
                self.validate_type(inner, depth + 1, states)
            }
            Type::Bytes(maximum) => {
                if *maximum == 0 {
                    Err(error(
                        CodecErrorKind::InvalidSchema,
                        0,
                        "byte-string maximum must be nonzero",
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    fn maximum_type_depth(
        &self,
        ty: &Type,
        cache: &mut BTreeMap<String, usize>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<usize, CodecError> {
        match ty {
            Type::Primitive(_) | Type::Bytes(_) => Ok(0),
            Type::Named(name) => self.maximum_named_depth(name, cache, visiting),
            Type::Optional(inner) | Type::List(inner, _) => {
                checked_depth_add(1, self.maximum_type_depth(inner, cache, visiting)?)
            }
        }
    }

    fn maximum_named_depth(
        &self,
        name: &str,
        cache: &mut BTreeMap<String, usize>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<usize, CodecError> {
        if let Some(maximum) = cache.get(name) {
            return Ok(*maximum);
        }
        if !visiting.insert(name.to_owned()) {
            return Err(error(
                CodecErrorKind::RecursiveType,
                0,
                "recursive named type graph",
            ));
        }
        let maximum = match self.resolve(name)? {
            NamedType::Scalar(_) | NamedType::Enum(_) => 0,
            NamedType::Record(record) => {
                let mut maximum = 0;
                for field in &record.fields {
                    maximum = maximum.max(checked_depth_add(
                        1,
                        self.maximum_type_depth(&field.ty, cache, visiting)?,
                    )?);
                }
                maximum
            }
            NamedType::Union(union) => {
                let mut maximum = 0;
                for variant in &union.variants {
                    for field in &variant.fields {
                        maximum = maximum.max(checked_depth_add(
                            1,
                            self.maximum_type_depth(&field.ty, cache, visiting)?,
                        )?);
                    }
                }
                maximum
            }
        };
        visiting.remove(name);
        cache.insert(name.to_owned(), maximum);
        Ok(maximum)
    }

    fn validate_primitive(&self, primitive: Primitive, _owner: &str) -> Result<(), CodecError> {
        match primitive {
            Primitive::U8
            | Primitive::U16
            | Primitive::U32
            | Primitive::U64
            | Primitive::I32
            | Primitive::Bool
            | Primitive::Bytes16
            | Primitive::Bytes32 => Ok(()),
        }
    }

    fn validate_variants<'b>(
        &self,
        repr: Primitive,
        owner: &str,
        variants: impl Iterator<Item = (&'b str, u16)>,
    ) -> Result<(), CodecError> {
        if !unsigned_repr(repr) {
            return Err(error(
                CodecErrorKind::InvalidSchema,
                0,
                format!("{owner} has a non-unsigned discriminant"),
            ));
        }
        let mut names = BTreeSet::new();
        let mut tags = BTreeSet::new();
        let maximum = repr_maximum(repr)?;
        let mut saw_variant = false;
        for (name, tag) in variants {
            saw_variant = true;
            if tag == 0 || u64::from(tag) > maximum || !names.insert(name) || !tags.insert(tag) {
                return Err(error(
                    CodecErrorKind::InvalidSchema,
                    0,
                    format!("{owner} has a duplicate, zero, or out-of-range variant"),
                ));
            }
        }
        if saw_variant {
            Ok(())
        } else {
            Err(error(
                CodecErrorKind::InvalidSchema,
                0,
                format!("{owner} must declare at least one variant"),
            ))
        }
    }

    fn resolve(&self, name: &str) -> Result<NamedType<'a>, CodecError> {
        self.named_types.get(name).copied().ok_or_else(|| {
            error(
                CodecErrorKind::UnknownType,
                0,
                format!("unknown named type {name}"),
            )
        })
    }

    fn check_depth(&self, depth: usize, offset: usize) -> Result<(), CodecError> {
        if depth > self.limits.max_depth {
            Err(error(
                CodecErrorKind::LimitExceeded,
                offset,
                format!(
                    "type depth {depth} exceeds budget {}",
                    self.limits.max_depth
                ),
            ))
        } else {
            Ok(())
        }
    }

    fn encode_type(
        &self,
        ty: &Type,
        value: &WireValue,
        depth: usize,
        state: &mut EncodeState,
    ) -> Result<(), CodecError> {
        self.check_depth(depth, state.output.len())?;
        match ty {
            Type::Primitive(primitive) => self.encode_primitive(*primitive, value, state),
            Type::Named(name) => self.encode_named_type(name, value, depth, state),
            Type::Optional(inner) => match value {
                WireValue::Optional(None) => state.write(&[0]),
                WireValue::Optional(Some(inner_value)) => {
                    state.charge_container(1)?;
                    state.write(&[1])?;
                    self.encode_type(inner, inner_value, depth + 1, state)
                }
                _ => Err(type_mismatch(state.output.len(), "optional", value)),
            },
            Type::List(inner, maximum) => {
                let WireValue::List(values) = value else {
                    return Err(type_mismatch(state.output.len(), "list", value));
                };
                let count = bounded_count(values.len(), *maximum, state.output.len(), "list")?;
                state.charge_container(values.len())?;
                state.write(&count.to_le_bytes())?;
                for item in values {
                    self.encode_type(inner, item, depth + 1, state)?;
                }
                Ok(())
            }
            Type::Bytes(maximum) => {
                let WireValue::Bytes(bytes) = value else {
                    return Err(type_mismatch(state.output.len(), "bytes", value));
                };
                let count = bounded_count(bytes.len(), *maximum, state.output.len(), "bytes")?;
                state.charge_container(bytes.len())?;
                state.write(&count.to_le_bytes())?;
                state.write(bytes)
            }
        }
    }

    fn encode_named_type(
        &self,
        name: &str,
        value: &WireValue,
        depth: usize,
        state: &mut EncodeState,
    ) -> Result<(), CodecError> {
        self.check_depth(depth, state.output.len())?;
        match self.resolve(name)? {
            NamedType::Scalar(scalar) => self.encode_primitive(scalar.primitive, value, state),
            NamedType::Enum(enumeration) => {
                let WireValue::Enum(variant_name) = value else {
                    return Err(type_mismatch(state.output.len(), &enumeration.name, value));
                };
                let variant = enumeration
                    .variants
                    .iter()
                    .find(|variant| variant.name == *variant_name)
                    .ok_or_else(|| {
                        error(
                            CodecErrorKind::UnknownVariant,
                            state.output.len(),
                            format!("unknown variant for {}", enumeration.name),
                        )
                    })?;
                state.write_tag(enumeration.repr, variant.tag)
            }
            NamedType::Record(record) => self.encode_record(record, value, depth, state),
            NamedType::Union(union) => self.encode_union(union, value, depth, state),
        }
    }

    fn encode_primitive(
        &self,
        primitive: Primitive,
        value: &WireValue,
        state: &mut EncodeState,
    ) -> Result<(), CodecError> {
        match (primitive, value) {
            (Primitive::U8, WireValue::U8(value)) => state.write(&[*value]),
            (Primitive::U16, WireValue::U16(value)) => state.write(&value.to_le_bytes()),
            (Primitive::U32, WireValue::U32(value)) => state.write(&value.to_le_bytes()),
            (Primitive::U64, WireValue::U64(value)) => state.write(&value.to_le_bytes()),
            (Primitive::I32, WireValue::I32(value)) => state.write(&value.to_le_bytes()),
            (Primitive::Bool, WireValue::Bool(value)) => state.write(&[u8::from(*value)]),
            (Primitive::Bytes16, WireValue::Bytes16(value)) => state.write(value),
            (Primitive::Bytes32, WireValue::Bytes32(value)) => state.write(value),
            _ => Err(type_mismatch(
                state.output.len(),
                primitive.schema_name(),
                value,
            )),
        }
    }

    fn encode_record(
        &self,
        record: &Record,
        value: &WireValue,
        depth: usize,
        state: &mut EncodeState,
    ) -> Result<(), CodecError> {
        let WireValue::Record(fields) = value else {
            return Err(type_mismatch(state.output.len(), &record.name, value));
        };
        let indexed = index_fields(fields, state.output.len())?;
        reject_field_shape(
            indexed.keys().copied(),
            record.fields.iter().map(|field| field.name.as_str()),
            state.output.len(),
            &record.name,
        )?;
        for field in &record.fields {
            self.encode_type(&field.ty, indexed[field.name.as_str()], depth + 1, state)?;
        }
        Ok(())
    }

    fn encode_union(
        &self,
        union: &Union,
        value: &WireValue,
        depth: usize,
        state: &mut EncodeState,
    ) -> Result<(), CodecError> {
        let WireValue::Union {
            variant: variant_name,
            fields,
        } = value
        else {
            return Err(type_mismatch(state.output.len(), &union.name, value));
        };
        let variant = union_variant_by_name(union, variant_name).ok_or_else(|| {
            error(
                CodecErrorKind::UnknownVariant,
                state.output.len(),
                format!("unknown variant for {}", union.name),
            )
        })?;
        state.write_tag(union.repr, variant.tag)?;
        let indexed = index_fields(fields, state.output.len())?;
        reject_field_shape(
            indexed.keys().copied(),
            variant.fields.iter().map(|field| field.name.as_str()),
            state.output.len(),
            &format!("{}.{}", union.name, variant.name),
        )?;
        for field in &variant.fields {
            self.encode_type(&field.ty, indexed[field.name.as_str()], depth + 1, state)?;
        }
        Ok(())
    }

    fn decode_type(
        &self,
        ty: &Type,
        depth: usize,
        state: &mut DecodeState<'_>,
    ) -> Result<WireValue, CodecError> {
        self.check_depth(depth, state.offset)?;
        match ty {
            Type::Primitive(primitive) => self.decode_primitive(*primitive, state),
            Type::Named(name) => self.decode_named_type(name, depth, state),
            Type::Optional(inner) => {
                let marker_offset = state.offset;
                match state.read_u8()? {
                    0 => Ok(WireValue::Optional(None)),
                    1 => {
                        state.charge_container(1)?;
                        let value = self.decode_type(inner, depth + 1, state)?;
                        Ok(WireValue::Optional(Some(Box::new(value))))
                    }
                    marker => Err(error(
                        CodecErrorKind::InvalidOptionalMarker,
                        marker_offset,
                        format!("optional marker {marker} is not zero or one"),
                    )),
                }
            }
            Type::List(inner, maximum) => {
                let count_offset = state.offset;
                let count = usize_from_u32(state.read_u32()?, count_offset)?;
                reject_count(count, *maximum, count_offset, "list")?;
                state.charge_container(count)?;
                let minimum_item_size = self.minimum_type_size(inner, depth + 1)?;
                state.require_minimum_items(count, minimum_item_size, count_offset)?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.decode_type(inner, depth + 1, state)?);
                }
                Ok(WireValue::List(values))
            }
            Type::Bytes(maximum) => {
                let count_offset = state.offset;
                let count = usize_from_u32(state.read_u32()?, count_offset)?;
                reject_count(count, *maximum, count_offset, "bytes")?;
                state.charge_container(count)?;
                Ok(WireValue::Bytes(state.take(count)?.to_vec()))
            }
        }
    }

    fn decode_named_type(
        &self,
        name: &str,
        depth: usize,
        state: &mut DecodeState<'_>,
    ) -> Result<WireValue, CodecError> {
        self.check_depth(depth, state.offset)?;
        match self.resolve(name)? {
            NamedType::Scalar(scalar) => self.decode_primitive(scalar.primitive, state),
            NamedType::Enum(enumeration) => {
                let tag_offset = state.offset;
                let tag = state.read_tag(enumeration.repr)?;
                let variant = enumeration
                    .variants
                    .iter()
                    .find(|variant| u64::from(variant.tag) == tag)
                    .ok_or_else(|| {
                        error(
                            CodecErrorKind::UnknownTag,
                            tag_offset,
                            format!("unknown {} tag {tag}", enumeration.name),
                        )
                    })?;
                Ok(WireValue::Enum(variant.name.clone()))
            }
            NamedType::Record(record) => {
                let mut fields = Vec::with_capacity(record.fields.len());
                for field in &record.fields {
                    fields.push((
                        field.name.clone(),
                        self.decode_type(&field.ty, depth + 1, state)?,
                    ));
                }
                Ok(WireValue::Record(fields))
            }
            NamedType::Union(union) => {
                let tag_offset = state.offset;
                let tag = state.read_tag(union.repr)?;
                let variant = union_variant_by_tag(union, tag).ok_or_else(|| {
                    error(
                        CodecErrorKind::UnknownTag,
                        tag_offset,
                        format!("unknown {} tag {tag}", union.name),
                    )
                })?;
                let mut fields = Vec::with_capacity(variant.fields.len());
                for field in &variant.fields {
                    fields.push((
                        field.name.clone(),
                        self.decode_type(&field.ty, depth + 1, state)?,
                    ));
                }
                Ok(WireValue::Union {
                    variant: variant.name.clone(),
                    fields,
                })
            }
        }
    }

    fn decode_primitive(
        &self,
        primitive: Primitive,
        state: &mut DecodeState<'_>,
    ) -> Result<WireValue, CodecError> {
        match primitive {
            Primitive::U8 => Ok(WireValue::U8(state.read_u8()?)),
            Primitive::U16 => Ok(WireValue::U16(state.read_u16()?)),
            Primitive::U32 => Ok(WireValue::U32(state.read_u32()?)),
            Primitive::U64 => Ok(WireValue::U64(state.read_u64()?)),
            Primitive::I32 => Ok(WireValue::I32(state.read_i32()?)),
            Primitive::Bool => {
                let marker_offset = state.offset;
                match state.read_u8()? {
                    0 => Ok(WireValue::Bool(false)),
                    1 => Ok(WireValue::Bool(true)),
                    marker => Err(error(
                        CodecErrorKind::InvalidBooleanMarker,
                        marker_offset,
                        format!("boolean marker {marker} is not zero or one"),
                    )),
                }
            }
            Primitive::Bytes16 => {
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(state.take(16)?);
                Ok(WireValue::Bytes16(bytes))
            }
            Primitive::Bytes32 => {
                let mut bytes = [0_u8; 32];
                bytes.copy_from_slice(state.take(32)?);
                Ok(WireValue::Bytes32(bytes))
            }
        }
    }

    fn minimum_type_size(&self, ty: &Type, depth: usize) -> Result<usize, CodecError> {
        self.check_depth(depth, 0)?;
        match ty {
            Type::Primitive(primitive) => Ok(primitive_width(*primitive)),
            Type::Named(name) => self.minimum_named_size(name, depth),
            Type::Optional(_) => Ok(1),
            Type::List(_, _) | Type::Bytes(_) => Ok(4),
        }
    }

    fn minimum_named_size(&self, name: &str, depth: usize) -> Result<usize, CodecError> {
        self.check_depth(depth, 0)?;
        match self.resolve(name)? {
            NamedType::Scalar(scalar) => Ok(primitive_width(scalar.primitive)),
            NamedType::Enum(enumeration) => Ok(primitive_width(enumeration.repr)),
            NamedType::Record(record) => {
                let mut total = 0_usize;
                for field in &record.fields {
                    total =
                        checked_minimum_add(total, self.minimum_type_size(&field.ty, depth + 1)?)?;
                }
                Ok(total)
            }
            NamedType::Union(union) => {
                let tag_size = primitive_width(union.repr);
                let mut minimum = None;
                for variant in &union.variants {
                    let mut variant_size = tag_size;
                    for field in &variant.fields {
                        variant_size = checked_minimum_add(
                            variant_size,
                            self.minimum_type_size(&field.ty, depth + 1)?,
                        )?;
                    }
                    minimum = Some(
                        minimum.map_or(variant_size, |current: usize| current.min(variant_size)),
                    );
                }
                minimum.ok_or_else(|| {
                    error(
                        CodecErrorKind::InvalidSchema,
                        0,
                        format!("union {} has no variants", union.name),
                    )
                })
            }
        }
    }

    fn maximum_type_size(
        &self,
        ty: &Type,
        depth: usize,
        cache: &mut BTreeMap<String, usize>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<usize, CodecError> {
        self.check_depth(depth, 0)?;
        match ty {
            Type::Primitive(primitive) => Ok(primitive_width(*primitive)),
            Type::Named(name) => self.maximum_named_wire_size(name, depth, cache, visiting),
            Type::Optional(inner) => checked_wire_add(
                1,
                self.maximum_type_size(inner, depth + 1, cache, visiting)?,
            ),
            Type::List(inner, maximum) => {
                let item_size = self.maximum_type_size(inner, depth + 1, cache, visiting)?;
                let items =
                    checked_wire_mul(usize::try_from(*maximum).unwrap_or(usize::MAX), item_size)?;
                checked_wire_add(4, items)
            }
            Type::Bytes(maximum) => {
                checked_wire_add(4, usize::try_from(*maximum).unwrap_or(usize::MAX))
            }
        }
    }

    fn maximum_named_wire_size(
        &self,
        name: &str,
        depth: usize,
        cache: &mut BTreeMap<String, usize>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<usize, CodecError> {
        self.check_depth(depth, 0)?;
        let relative_depth =
            self.maximum_named_depth(name, &mut BTreeMap::new(), &mut BTreeSet::new())?;
        self.check_depth(checked_depth_add(depth, relative_depth)?, 0)?;
        if let Some(maximum) = cache.get(name) {
            return Ok(*maximum);
        }
        if !visiting.insert(name.to_owned()) {
            return Err(error(
                CodecErrorKind::RecursiveType,
                0,
                "recursive named type graph",
            ));
        }
        let maximum = match self.resolve(name)? {
            NamedType::Scalar(scalar) => primitive_width(scalar.primitive),
            NamedType::Enum(enumeration) => primitive_width(enumeration.repr),
            NamedType::Record(record) => {
                let mut total = 0_usize;
                for field in &record.fields {
                    total = checked_wire_add(
                        total,
                        self.maximum_type_size(&field.ty, depth + 1, cache, visiting)?,
                    )?;
                }
                total
            }
            NamedType::Union(union) => {
                let tag_size = primitive_width(union.repr);
                let mut maximum_variant = 0_usize;
                for variant in &union.variants {
                    let mut variant_size = tag_size;
                    for field in &variant.fields {
                        variant_size = checked_wire_add(
                            variant_size,
                            self.maximum_type_size(&field.ty, depth + 1, cache, visiting)?,
                        )?;
                    }
                    maximum_variant = maximum_variant.max(variant_size);
                }
                maximum_variant
            }
        };
        visiting.remove(name);
        cache.insert(name.to_owned(), maximum);
        Ok(maximum)
    }
}

struct EncodeState {
    output: Vec<u8>,
    limits: CodecLimits,
    remaining_container_items: usize,
}

impl EncodeState {
    fn new(limits: CodecLimits) -> Self {
        Self {
            output: Vec::new(),
            limits,
            remaining_container_items: limits.max_container_items,
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), CodecError> {
        let Some(new_length) = self.output.len().checked_add(bytes.len()) else {
            return Err(error(
                CodecErrorKind::LimitExceeded,
                self.output.len(),
                "encoded length overflow",
            ));
        };
        if new_length > self.limits.max_bytes {
            return Err(error(
                CodecErrorKind::LimitExceeded,
                self.output.len(),
                format!(
                    "encoded length {new_length} exceeds byte budget {}",
                    self.limits.max_bytes
                ),
            ));
        }
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    fn charge_container(&mut self, count: usize) -> Result<(), CodecError> {
        if count > self.remaining_container_items {
            return Err(error(
                CodecErrorKind::LimitExceeded,
                self.output.len(),
                format!(
                    "container charge {count} exceeds remaining budget {}",
                    self.remaining_container_items
                ),
            ));
        }
        self.remaining_container_items -= count;
        Ok(())
    }

    fn write_tag(&mut self, repr: Primitive, tag: u16) -> Result<(), CodecError> {
        match repr {
            Primitive::U8 => {
                let tag = u8::try_from(tag).map_err(|_| {
                    error(
                        CodecErrorKind::InvalidSchema,
                        self.output.len(),
                        format!("tag {tag} does not fit u8"),
                    )
                })?;
                self.write(&[tag])
            }
            Primitive::U16 => self.write(&tag.to_le_bytes()),
            Primitive::U32 => self.write(&u32::from(tag).to_le_bytes()),
            Primitive::U64 => self.write(&u64::from(tag).to_le_bytes()),
            Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
                Err(error(
                    CodecErrorKind::InvalidSchema,
                    self.output.len(),
                    "enum/union discriminant is not unsigned",
                ))
            }
        }
    }
}

struct DecodeState<'a> {
    input: &'a [u8],
    offset: usize,
    remaining_container_items: usize,
}

impl<'a> DecodeState<'a> {
    fn new(input: &'a [u8], limits: CodecLimits) -> Self {
        Self {
            input,
            offset: 0,
            remaining_container_items: limits.max_container_items,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        let start = self.offset;
        let Some(end) = start.checked_add(count) else {
            return Err(error(
                CodecErrorKind::Truncated,
                start,
                "input offset overflow",
            ));
        };
        let bytes = self.input.get(start..end).ok_or_else(|| {
            error(
                CodecErrorKind::Truncated,
                start,
                format!(
                    "need {count} bytes but only {} remain",
                    self.input.len().saturating_sub(start)
                ),
            )
        })?;
        self.offset = end;
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, CodecError> {
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u64(&mut self) -> Result<u64, CodecError> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_i32(&mut self) -> Result<i32, CodecError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(i32::from_le_bytes(bytes))
    }

    fn read_tag(&mut self, repr: Primitive) -> Result<u64, CodecError> {
        match repr {
            Primitive::U8 => Ok(u64::from(self.read_u8()?)),
            Primitive::U16 => Ok(u64::from(self.read_u16()?)),
            Primitive::U32 => Ok(u64::from(self.read_u32()?)),
            Primitive::U64 => self.read_u64(),
            Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => {
                Err(error(
                    CodecErrorKind::InvalidSchema,
                    self.offset,
                    "enum/union discriminant is not unsigned",
                ))
            }
        }
    }

    fn charge_container(&mut self, count: usize) -> Result<(), CodecError> {
        if count > self.remaining_container_items {
            return Err(error(
                CodecErrorKind::LimitExceeded,
                self.offset,
                format!(
                    "container charge {count} exceeds remaining budget {}",
                    self.remaining_container_items
                ),
            ));
        }
        self.remaining_container_items -= count;
        Ok(())
    }

    fn require_minimum_items(
        &self,
        count: usize,
        minimum_item_size: usize,
        count_offset: usize,
    ) -> Result<(), CodecError> {
        let minimum_bytes = count.checked_mul(minimum_item_size).ok_or_else(|| {
            error(
                CodecErrorKind::LimitExceeded,
                count_offset,
                "minimum list byte size overflow",
            )
        })?;
        let remaining = self.input.len().saturating_sub(self.offset);
        if minimum_bytes > remaining {
            Err(error(
                CodecErrorKind::Truncated,
                self.offset,
                format!("list needs at least {minimum_bytes} bytes but only {remaining} remain"),
            ))
        } else {
            Ok(())
        }
    }

    fn reject_trailing(&self) -> Result<(), CodecError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(error(
                CodecErrorKind::TrailingBytes,
                self.offset,
                format!("{} trailing bytes", self.input.len() - self.offset),
            ))
        }
    }
}

fn insert_named_type<'a>(
    types: &mut BTreeMap<String, NamedType<'a>>,
    name: &str,
    definition: NamedType<'a>,
) -> Result<(), CodecError> {
    if types.insert(name.to_owned(), definition).is_some() {
        Err(error(
            CodecErrorKind::InvalidSchema,
            0,
            format!("duplicate named type {name}"),
        ))
    } else {
        Ok(())
    }
}

fn index_fields(
    fields: &[(String, WireValue)],
    offset: usize,
) -> Result<BTreeMap<&str, &WireValue>, CodecError> {
    let mut indexed = BTreeMap::new();
    for (name, value) in fields {
        if indexed.insert(name.as_str(), value).is_some() {
            return Err(error(
                CodecErrorKind::DuplicateField,
                offset,
                "duplicate field in generic value",
            ));
        }
    }
    Ok(indexed)
}

fn reject_field_shape<'a>(
    actual: impl Iterator<Item = &'a str>,
    expected: impl Iterator<Item = &'a str>,
    offset: usize,
    owner: &str,
) -> Result<(), CodecError> {
    let actual = actual.collect::<BTreeSet<_>>();
    let expected = expected.collect::<BTreeSet<_>>();
    if let Some(missing) = expected.difference(&actual).next() {
        return Err(error(
            CodecErrorKind::MissingField,
            offset,
            format!("{owner} is missing field {missing}"),
        ));
    }
    if actual.difference(&expected).next().is_some() {
        return Err(error(
            CodecErrorKind::UnknownField,
            offset,
            format!("{owner} has an unknown field"),
        ));
    }
    Ok(())
}

fn union_variant_by_name<'a>(union: &'a Union, name: &str) -> Option<&'a UnionVariant> {
    union.variants.iter().find(|variant| variant.name == name)
}

fn union_variant_by_tag(union: &Union, tag: u64) -> Option<&UnionVariant> {
    union
        .variants
        .iter()
        .find(|variant| u64::from(variant.tag) == tag)
}

fn bounded_count(
    count: usize,
    schema_maximum: u32,
    offset: usize,
    owner: &str,
) -> Result<u32, CodecError> {
    reject_count(count, schema_maximum, offset, owner)?;
    u32::try_from(count).map_err(|_| {
        error(
            CodecErrorKind::LimitExceeded,
            offset,
            format!("{owner} count {count} does not fit u32"),
        )
    })
}

fn reject_count(
    count: usize,
    schema_maximum: u32,
    offset: usize,
    owner: &str,
) -> Result<(), CodecError> {
    let maximum = usize::try_from(schema_maximum).unwrap_or(usize::MAX);
    if count > maximum {
        Err(error(
            CodecErrorKind::LimitExceeded,
            offset,
            format!("{owner} count {count} exceeds schema maximum {schema_maximum}"),
        ))
    } else {
        Ok(())
    }
}

fn usize_from_u32(value: u32, offset: usize) -> Result<usize, CodecError> {
    usize::try_from(value).map_err(|_| {
        error(
            CodecErrorKind::LimitExceeded,
            offset,
            format!("container count {value} does not fit usize"),
        )
    })
}

const fn unsigned_repr(primitive: Primitive) -> bool {
    matches!(
        primitive,
        Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::U64
    )
}

fn repr_maximum(primitive: Primitive) -> Result<u64, CodecError> {
    match primitive {
        Primitive::U8 => Ok(u64::from(u8::MAX)),
        Primitive::U16 => Ok(u64::from(u16::MAX)),
        Primitive::U32 => Ok(u64::from(u32::MAX)),
        Primitive::U64 => Ok(u64::MAX),
        Primitive::I32 | Primitive::Bool | Primitive::Bytes16 | Primitive::Bytes32 => Err(error(
            CodecErrorKind::InvalidSchema,
            0,
            "enum/union discriminant is not unsigned",
        )),
    }
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

fn checked_minimum_add(left: usize, right: usize) -> Result<usize, CodecError> {
    left.checked_add(right).ok_or_else(|| {
        error(
            CodecErrorKind::LimitExceeded,
            0,
            "minimum wire size overflow",
        )
    })
}

fn checked_depth_add(left: usize, right: usize) -> Result<usize, CodecError> {
    left.checked_add(right).ok_or_else(|| {
        error(
            CodecErrorKind::LimitExceeded,
            0,
            "maximum type depth overflow",
        )
    })
}

fn checked_wire_add(left: usize, right: usize) -> Result<usize, CodecError> {
    left.checked_add(right).ok_or_else(|| {
        error(
            CodecErrorKind::LimitExceeded,
            0,
            "maximum encoded size overflow",
        )
    })
}

fn checked_wire_mul(left: usize, right: usize) -> Result<usize, CodecError> {
    left.checked_mul(right).ok_or_else(|| {
        error(
            CodecErrorKind::LimitExceeded,
            0,
            "maximum encoded size overflow",
        )
    })
}

fn type_mismatch(offset: usize, expected: &str, actual: &WireValue) -> CodecError {
    error(
        CodecErrorKind::TypeMismatch,
        offset,
        format!("expected {expected}, found {}", wire_value_name(actual)),
    )
}

const fn wire_value_name(value: &WireValue) -> &'static str {
    match value {
        WireValue::U8(_) => "u8",
        WireValue::U16(_) => "u16",
        WireValue::U32(_) => "u32",
        WireValue::U64(_) => "u64",
        WireValue::I32(_) => "i32",
        WireValue::Bool(_) => "bool",
        WireValue::Bytes16(_) => "bytes16",
        WireValue::Bytes32(_) => "bytes32",
        WireValue::Bytes(_) => "bytes",
        WireValue::Enum(_) => "enum",
        WireValue::Optional(_) => "optional",
        WireValue::List(_) => "list",
        WireValue::Record(_) => "record",
        WireValue::Union { .. } => "union",
    }
}

fn error(kind: CodecErrorKind, offset: usize, detail: impl Into<String>) -> CodecError {
    CodecError {
        kind,
        offset,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        EnumDef, Field, Message, Privacy, Record, Scalar, Union, UnionField, UnionVariant, Variant,
    };

    const GENEROUS_LIMITS: CodecLimits = CodecLimits::new(32, 4_096, 1_024);

    fn protocol() -> Protocol {
        Protocol {
            name: "CodecFixture".into(),
            major: 0,
            minor: 1,
            max_message_bytes: 4_096,
            max_transfer_slots: 4,
            max_data_segment_bytes: 1_024,
            max_data_ticket_bytes: 4_096,
            payload_codec: FIXED_LE_V1.into(),
            scalars: vec![Scalar {
                name: "HandleSlot".into(),
                primitive: Primitive::U16,
            }],
            enums: vec![EnumDef {
                name: "Mode".into(),
                repr: Primitive::U8,
                variants: vec![
                    Variant {
                        name: "Cold".into(),
                        tag: 1,
                    },
                    Variant {
                        name: "Warm".into(),
                        tag: 2,
                    },
                ],
            }],
            records: vec![
                Record {
                    name: "Leaf".into(),
                    fields: vec![
                        required_field("active", Type::Primitive(Primitive::Bool), Privacy::Public),
                        required_field(
                            "digest",
                            Type::Primitive(Primitive::Bytes16),
                            Privacy::Private,
                        ),
                    ],
                },
                Record {
                    name: "Root".into(),
                    fields: vec![
                        required_field(
                            "sequence",
                            Type::Primitive(Primitive::U32),
                            Privacy::Public,
                        ),
                        required_field("mode", Type::Named("Mode".into()), Privacy::Public),
                        optional_field("leaf", Type::Named("Leaf".into()), Privacy::Private),
                        required_field(
                            "resources",
                            Type::List(Box::new(Type::Named("Resource".into())), 2),
                            Privacy::Public,
                        ),
                        required_field("payload", Type::Bytes(5), Privacy::Sensitive),
                    ],
                },
            ],
            unions: vec![Union {
                name: "Resource".into(),
                repr: Primitive::U8,
                variants: vec![
                    UnionVariant {
                        name: "Inline".into(),
                        tag: 1,
                        required_capability: None,
                        fields: vec![UnionField {
                            name: "chunks".into(),
                            ty: Type::List(Box::new(Type::Bytes(3)), 2),
                            privacy: Privacy::Public,
                        }],
                    },
                    UnionVariant {
                        name: "Shared".into(),
                        tag: 2,
                        required_capability: Some("SharedMemory".into()),
                        fields: vec![
                            UnionField {
                                name: "slot".into(),
                                ty: Type::Named("HandleSlot".into()),
                                privacy: Privacy::Sensitive,
                            },
                            UnionField {
                                name: "epoch".into(),
                                ty: Type::Primitive(Primitive::U32),
                                privacy: Privacy::Public,
                            },
                        ],
                    },
                ],
            }],
            messages: Vec::<Message>::new(),
        }
    }

    fn required_field(name: &str, ty: Type, privacy: Privacy) -> Field {
        Field {
            name: name.into(),
            ty,
            presence: Presence::Required,
            privacy,
        }
    }

    fn optional_field(name: &str, inner: Type, privacy: Privacy) -> Field {
        Field {
            name: name.into(),
            ty: Type::Optional(Box::new(inner)),
            presence: Presence::Optional,
            privacy,
        }
    }

    fn root_value_in_non_schema_order() -> WireValue {
        WireValue::Record(vec![
            ("payload".into(), WireValue::Bytes(vec![0xaa, 0xbb])),
            (
                "resources".into(),
                WireValue::List(vec![
                    WireValue::Union {
                        variant: "Shared".into(),
                        fields: vec![
                            ("epoch".into(), WireValue::U32(0x0102_0304)),
                            ("slot".into(), WireValue::U16(0x1234)),
                        ],
                    },
                    WireValue::Union {
                        variant: "Inline".into(),
                        fields: vec![(
                            "chunks".into(),
                            WireValue::List(vec![WireValue::Bytes(vec![9, 8, 7])]),
                        )],
                    },
                ]),
            ),
            (
                "leaf".into(),
                WireValue::Optional(Some(Box::new(WireValue::Record(vec![
                    (
                        "digest".into(),
                        WireValue::Bytes16(std::array::from_fn(|i| i as u8)),
                    ),
                    ("active".into(), WireValue::Bool(true)),
                ])))),
            ),
            ("mode".into(), WireValue::Enum("Warm".into())),
            ("sequence".into(), WireValue::U32(0x1122_3344)),
        ])
    }

    fn root_value_in_schema_order() -> WireValue {
        WireValue::Record(vec![
            ("sequence".into(), WireValue::U32(0x1122_3344)),
            ("mode".into(), WireValue::Enum("Warm".into())),
            (
                "leaf".into(),
                WireValue::Optional(Some(Box::new(WireValue::Record(vec![
                    ("active".into(), WireValue::Bool(true)),
                    (
                        "digest".into(),
                        WireValue::Bytes16(std::array::from_fn(|i| i as u8)),
                    ),
                ])))),
            ),
            (
                "resources".into(),
                WireValue::List(vec![
                    WireValue::Union {
                        variant: "Shared".into(),
                        fields: vec![
                            ("slot".into(), WireValue::U16(0x1234)),
                            ("epoch".into(), WireValue::U32(0x0102_0304)),
                        ],
                    },
                    WireValue::Union {
                        variant: "Inline".into(),
                        fields: vec![(
                            "chunks".into(),
                            WireValue::List(vec![WireValue::Bytes(vec![9, 8, 7])]),
                        )],
                    },
                ]),
            ),
            ("payload".into(), WireValue::Bytes(vec![0xaa, 0xbb])),
        ])
    }

    fn expected_root_bytes() -> Vec<u8> {
        let mut expected = vec![
            0x44, 0x33, 0x22, 0x11, // sequence
            0x02, // Mode::Warm
            0x01, // leaf present
            0x01, // active
        ];
        expected.extend(0_u8..16);
        expected.extend([
            0x02, 0x00, 0x00, 0x00, // two resources
            0x02, // Resource::Shared
            0x34, 0x12, // sensitive handle-table slot
            0x04, 0x03, 0x02, 0x01, // epoch
            0x01, // Resource::Inline
            0x01, 0x00, 0x00, 0x00, // one chunk
            0x03, 0x00, 0x00, 0x00, // three chunk bytes
            0x09, 0x08, 0x07, 0x02, 0x00, 0x00, 0x00, // payload length
            0xaa, 0xbb,
        ]);
        expected
    }

    #[test]
    fn nested_values_round_trip_byte_exactly_in_schema_order() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();
        let expected = expected_root_bytes();

        let encoded = codec
            .encode_named("Root", &root_value_in_non_schema_order())
            .unwrap();
        assert_eq!(encoded, expected);
        assert_eq!(
            codec.decode_named("Root", &encoded).unwrap(),
            root_value_in_schema_order()
        );
        assert_eq!(codec.reencode_named("Root", &encoded).unwrap(), encoded);
        assert_eq!(codec.maximum_named_encoded_size("Root").unwrap(), 74);
    }

    #[test]
    fn arbitrary_type_entry_points_are_canonical() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();
        let ty = Type::List(Box::new(Type::Primitive(Primitive::U16)), 2);
        let value = WireValue::List(vec![WireValue::U16(0x1234), WireValue::U16(0xabcd)]);
        let bytes = [2, 0, 0, 0, 0x34, 0x12, 0xcd, 0xab];

        assert_eq!(codec.encode(&ty, &value).unwrap(), bytes);
        assert_eq!(codec.decode(&ty, &bytes).unwrap(), value);
        assert_eq!(codec.reencode(&ty, &bytes).unwrap(), bytes);
        assert_eq!(codec.maximum_encoded_size(&ty).unwrap(), 8);
    }

    #[test]
    fn every_primitive_uses_its_exact_fixed_width_encoding() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();
        let cases = [
            (
                Type::Primitive(Primitive::U8),
                WireValue::U8(0xab),
                vec![0xab],
            ),
            (
                Type::Primitive(Primitive::U16),
                WireValue::U16(0x1234),
                vec![0x34, 0x12],
            ),
            (
                Type::Primitive(Primitive::U32),
                WireValue::U32(0x1234_5678),
                vec![0x78, 0x56, 0x34, 0x12],
            ),
            (
                Type::Primitive(Primitive::U64),
                WireValue::U64(0x0102_0304_0506_0708),
                vec![0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
            ),
            (
                Type::Primitive(Primitive::I32),
                WireValue::I32(-2),
                vec![0xfe, 0xff, 0xff, 0xff],
            ),
            (
                Type::Primitive(Primitive::Bool),
                WireValue::Bool(true),
                vec![1],
            ),
            (
                Type::Primitive(Primitive::Bytes16),
                WireValue::Bytes16([0x16; 16]),
                vec![0x16; 16],
            ),
            (
                Type::Primitive(Primitive::Bytes32),
                WireValue::Bytes32([0x32; 32]),
                vec![0x32; 32],
            ),
        ];

        for (ty, value, expected) in cases {
            assert_eq!(codec.encode(&ty, &value).unwrap(), expected);
            assert_eq!(codec.decode(&ty, &expected).unwrap(), value);
        }
    }

    #[test]
    fn enum_and_union_tags_use_declared_repr_without_truncation() {
        let mut fixture = protocol();
        fixture.enums.push(EnumDef {
            name: "WideMode".into(),
            repr: Primitive::U32,
            variants: vec![Variant {
                name: "Selected".into(),
                tag: 0x1234,
            }],
        });
        fixture.unions.push(Union {
            name: "WideResource".into(),
            repr: Primitive::U64,
            variants: vec![UnionVariant {
                name: "Slot".into(),
                tag: 0x2345,
                required_capability: None,
                fields: vec![UnionField {
                    name: "slot".into(),
                    ty: Type::Named("HandleSlot".into()),
                    privacy: Privacy::Sensitive,
                }],
            }],
        });
        let codec = FixedLeCodec::new(&fixture, GENEROUS_LIMITS).unwrap();

        assert_eq!(
            codec
                .encode_named("WideMode", &WireValue::Enum("Selected".into()))
                .unwrap(),
            [0x34, 0x12, 0x00, 0x00]
        );
        assert_eq!(
            codec
                .encode_named(
                    "WideResource",
                    &WireValue::Union {
                        variant: "Slot".into(),
                        fields: vec![("slot".into(), WireValue::U16(7))],
                    },
                )
                .unwrap(),
            [0x45, 0x23, 0, 0, 0, 0, 0, 0, 7, 0]
        );

        let mut invalid = protocol();
        invalid.enums.push(EnumDef {
            name: "NarrowMode".into(),
            repr: Primitive::U8,
            variants: vec![Variant {
                name: "TooWide".into(),
                tag: 256,
            }],
        });
        assert_eq!(
            FixedLeCodec::new(&invalid, GENEROUS_LIMITS)
                .err()
                .unwrap()
                .kind,
            CodecErrorKind::InvalidSchema
        );
    }

    #[test]
    fn rejects_unknown_tags_and_noncanonical_markers() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();

        assert_eq!(
            codec.decode_named("Resource", &[3]).unwrap_err().kind,
            CodecErrorKind::UnknownTag
        );
        assert_eq!(
            codec.decode_named("Mode", &[9]).unwrap_err().kind,
            CodecErrorKind::UnknownTag
        );

        let mut root = expected_root_bytes();
        root[5] = 2;
        assert_eq!(
            codec.decode_named("Root", &root).unwrap_err().kind,
            CodecErrorKind::InvalidOptionalMarker
        );

        let mut leaf = vec![2];
        leaf.extend(0_u8..16);
        assert_eq!(
            codec.decode_named("Leaf", &leaf).unwrap_err().kind,
            CodecErrorKind::InvalidBooleanMarker
        );
    }

    #[test]
    fn rejects_over_limit_counts_before_allocation() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();
        let mut root = expected_root_bytes();
        let resources_count_offset = 23;
        root[resources_count_offset..resources_count_offset + 4]
            .copy_from_slice(&3_u32.to_le_bytes());
        assert_eq!(
            codec.decode_named("Root", &root).unwrap_err().kind,
            CodecErrorKind::LimitExceeded
        );

        let two_u64s = Type::List(Box::new(Type::Primitive(Primitive::U64)), 2);
        let mut truncated_before_allocation = 2_u32.to_le_bytes().to_vec();
        truncated_before_allocation.extend(1_u64.to_le_bytes());
        assert_eq!(
            codec
                .decode(&two_u64s, &truncated_before_allocation)
                .unwrap_err()
                .kind,
            CodecErrorKind::Truncated
        );

        let tight_codec = FixedLeCodec::new(&protocol, CodecLimits::new(32, 4_096, 1)).unwrap();
        assert_eq!(
            tight_codec
                .decode_named("Root", &expected_root_bytes())
                .unwrap_err()
                .kind,
            CodecErrorKind::LimitExceeded
        );
    }

    #[test]
    fn rejects_trailing_and_truncated_payloads() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();

        assert_eq!(
            codec.decode_named("Mode", &[2, 0]).unwrap_err().kind,
            CodecErrorKind::TrailingBytes
        );

        let mut truncated = expected_root_bytes();
        truncated.pop();
        assert_eq!(
            codec.decode_named("Root", &truncated).unwrap_err().kind,
            CodecErrorKind::Truncated
        );
    }

    #[test]
    fn rejects_byte_budget_and_recursive_type_graphs() {
        let fixture = protocol();
        let expected = expected_root_bytes();
        let tight_codec =
            FixedLeCodec::new(&fixture, CodecLimits::new(32, expected.len() - 1, 1_024)).unwrap();
        assert_eq!(
            tight_codec
                .encode_named("Root", &root_value_in_schema_order())
                .unwrap_err()
                .kind,
            CodecErrorKind::LimitExceeded
        );
        assert_eq!(
            tight_codec
                .decode_named("Root", &expected)
                .unwrap_err()
                .kind,
            CodecErrorKind::LimitExceeded
        );

        assert_eq!(
            FixedLeCodec::new(&fixture, CodecLimits::new(2, 4_096, 1_024))
                .err()
                .unwrap()
                .kind,
            CodecErrorKind::LimitExceeded
        );

        let mut recursive = protocol();
        recursive.records.push(Record {
            name: "Loop".into(),
            fields: vec![required_field(
                "next",
                Type::Named("Loop".into()),
                Privacy::Public,
            )],
        });
        assert_eq!(
            FixedLeCodec::new(&recursive, GENEROUS_LIMITS)
                .err()
                .unwrap()
                .kind,
            CodecErrorKind::RecursiveType
        );
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_fields() {
        let protocol = protocol();
        let codec = FixedLeCodec::new(&protocol, GENEROUS_LIMITS).unwrap();

        let mut missing = match root_value_in_schema_order() {
            WireValue::Record(fields) => fields,
            _ => unreachable!(),
        };
        missing.pop();
        assert_eq!(
            codec
                .encode_named("Root", &WireValue::Record(missing))
                .unwrap_err()
                .kind,
            CodecErrorKind::MissingField
        );

        let mut duplicate = match root_value_in_schema_order() {
            WireValue::Record(fields) => fields,
            _ => unreachable!(),
        };
        duplicate.push(("sequence".into(), WireValue::U32(0)));
        assert_eq!(
            codec
                .encode_named("Root", &WireValue::Record(duplicate))
                .unwrap_err()
                .kind,
            CodecErrorKind::DuplicateField
        );

        let mut unknown = match root_value_in_schema_order() {
            WireValue::Record(fields) => fields,
            _ => unreachable!(),
        };
        unknown.push(("unknown".into(), WireValue::U8(0)));
        assert_eq!(
            codec
                .encode_named("Root", &WireValue::Record(unknown))
                .unwrap_err()
                .kind,
            CodecErrorKind::UnknownField
        );
    }
}
