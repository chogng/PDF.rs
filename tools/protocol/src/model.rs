use std::fmt::Write;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Protocol {
    pub name: String,
    pub major: u16,
    pub minor: u16,
    pub max_message_bytes: u32,
    pub max_transfer_slots: u16,
    pub max_data_segment_bytes: u64,
    pub max_data_ticket_bytes: u64,
    pub payload_codec: String,
    pub scalars: Vec<Scalar>,
    pub enums: Vec<EnumDef>,
    pub records: Vec<Record>,
    pub unions: Vec<Union>,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scalar {
    pub name: String,
    pub primitive: Primitive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub repr: Primitive,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    pub name: String,
    pub tag: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Union {
    pub name: String,
    pub repr: Primitive,
    pub variants: Vec<UnionVariant>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionVariant {
    pub name: String,
    pub tag: u16,
    pub required_capability: Option<String>,
    pub fields: Vec<UnionField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnionField {
    pub name: String,
    pub ty: Type,
    pub privacy: Privacy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub presence: Presence,
    pub privacy: Privacy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    Required,
    Optional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privacy {
    Public,
    Private,
    Sensitive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    Primitive(Primitive),
    Named(String),
    Optional(Box<Type>),
    List(Box<Type>, u32),
    Bytes(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Primitive {
    U8,
    U16,
    U32,
    U64,
    I32,
    Bool,
    Bytes16,
    Bytes32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageKind {
    Command,
    Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub kind: MessageKind,
    pub name: String,
    pub id: u16,
    pub payload: String,
    pub state: String,
    pub correlation: String,
    pub disposition: String,
    pub allowed_flags: u16,
    pub min_transfer_slots: u16,
    pub max_transfer_slots: u16,
    pub max_payload_bytes: u32,
    pub required_capability: Option<String>,
    pub outcomes: Vec<Outcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub name: String,
    pub disposition: OutcomeDisposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeDisposition {
    Stream,
    Terminal,
}

impl Protocol {
    pub fn canonical_text(&self) -> String {
        let mut out = String::new();
        writeln!(out, "protocol {} {} {}", self.name, self.major, self.minor).unwrap();
        writeln!(out, "limit max_message_bytes {}", self.max_message_bytes).unwrap();
        writeln!(out, "limit max_transfer_slots {}", self.max_transfer_slots).unwrap();
        writeln!(
            out,
            "limit max_data_segment_bytes {}",
            self.max_data_segment_bytes
        )
        .unwrap();
        writeln!(
            out,
            "limit max_data_ticket_bytes {}",
            self.max_data_ticket_bytes
        )
        .unwrap();
        writeln!(out, "codec {}", self.payload_codec).unwrap();
        if !self.scalars.is_empty() {
            out.push('\n');
        }
        for scalar in &self.scalars {
            writeln!(
                out,
                "scalar {} {}",
                scalar.name,
                scalar.primitive.schema_name()
            )
            .unwrap();
        }
        for enumeration in &self.enums {
            writeln!(
                out,
                "\nenum {} {}",
                enumeration.name,
                enumeration.repr.schema_name()
            )
            .unwrap();
            for variant in &enumeration.variants {
                writeln!(out, "variant {} {}", variant.name, variant.tag).unwrap();
            }
            writeln!(out, "end").unwrap();
        }
        for record in &self.records {
            writeln!(out, "\nrecord {}", record.name).unwrap();
            for field in &record.fields {
                writeln!(
                    out,
                    "field {} {} {} {}",
                    field.name,
                    field.ty.schema_name(),
                    field.presence.schema_name(),
                    field.privacy.schema_name()
                )
                .unwrap();
            }
            writeln!(out, "end").unwrap();
        }
        for union in &self.unions {
            writeln!(out, "\nunion {} {}", union.name, union.repr.schema_name()).unwrap();
            for variant in &union.variants {
                write!(out, "variant {} {}", variant.name, variant.tag).unwrap();
                if !variant.fields.is_empty() {
                    out.push(' ');
                    for (index, field) in variant.fields.iter().enumerate() {
                        if index != 0 {
                            out.push(',');
                        }
                        write!(
                            out,
                            "{}:{}:{}",
                            field.name,
                            field.ty.schema_name(),
                            field.privacy.schema_name()
                        )
                        .unwrap();
                    }
                }
                if let Some(capability) = &variant.required_capability {
                    write!(out, " requires={capability}").unwrap();
                }
                out.push('\n');
            }
            writeln!(out, "end").unwrap();
        }
        if !self.messages.is_empty() {
            out.push('\n');
        }
        for message in &self.messages {
            write!(
                out,
                "message {} {} {} {} {} {} {} {} {} {} {}",
                message.kind.schema_name(),
                message.name,
                message.id,
                message.payload,
                message.state,
                message.correlation,
                message.disposition,
                message.allowed_flags,
                message.min_transfer_slots,
                message.max_transfer_slots,
                message.max_payload_bytes
            )
            .unwrap();
            if let Some(capability) = &message.required_capability {
                write!(out, " requires={capability}").unwrap();
            }
            if message.kind == MessageKind::Command {
                out.push(' ');
                if message.outcomes.is_empty() {
                    out.push_str("none");
                } else {
                    out.push_str(
                        &message
                            .outcomes
                            .iter()
                            .map(|outcome| {
                                format!("{}:{}", outcome.name, outcome.disposition.schema_name())
                            })
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                }
            }
            out.push('\n');
        }
        out
    }
}

impl Primitive {
    pub const fn schema_name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I32 => "i32",
            Self::Bool => "bool",
            Self::Bytes16 => "bytes16",
            Self::Bytes32 => "bytes32",
        }
    }
}

impl Type {
    pub fn schema_name(&self) -> String {
        match self {
            Self::Primitive(value) => value.schema_name().into(),
            Self::Named(value) => value.clone(),
            Self::Optional(inner) => format!("optional<{}>", inner.schema_name()),
            Self::List(inner, limit) => format!("list<{},{}>", inner.schema_name(), limit),
            Self::Bytes(limit) => format!("bytes<{limit}>"),
        }
    }
}

impl Presence {
    const fn schema_name(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}

impl Privacy {
    const fn schema_name(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Sensitive => "sensitive",
        }
    }
}

impl MessageKind {
    pub const fn schema_name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Event => "event",
        }
    }
}

impl OutcomeDisposition {
    pub const fn schema_name(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Terminal => "terminal",
        }
    }
}
