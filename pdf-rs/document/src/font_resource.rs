use std::fmt;
use std::mem::size_of;
use std::sync::Arc;

use pdf_rs_bytes::{
    ByteRange, ByteSource, DataTicket, JobId, ReadPoll, ReadRequest, RequestPriority,
    ResumeCheckpoint, SmallRanges, SourceErrorCategory, SourceSnapshot,
};
use pdf_rs_filters::{
    DecodeCancellation, DecodeError, DecodeErrorCategory, DecodeErrorCode, DecodeLimitConfig,
    DecodeLimitKind, DecodeLimits, DecodeProfile, DecodeRequest, DecodedStream, FilterPlan,
    StreamFilter, decode_stream,
};
use pdf_rs_font::{
    CffParseOutcome, FontCancellation, FontError, FontErrorCategory, FontLimitKind,
    FontParseOutcome, FontProfile, FontProgram, FontStats, FontUnsupported, GlyphId, parse_cff,
    parse_truetype,
};
use pdf_rs_object::{IndirectObjectValue, ObjectErrorCode, ObjectLimitKind, ObjectWorkCaps};
use pdf_rs_syntax::{Located, ObjectRef, PdfDictionary, SyntaxLimitKind, SyntaxObject};

use crate::{
    AttestedObject, AttestedObjectJobContext, AttestedObjectPoll, DocumentCancellation,
    DocumentError, DocumentErrorCode, DocumentLimitKind, FontResourceLimits, OpenAttestedObjectJob,
    PageFontReference, SharedAttestedRevisionIndex,
};

const METADATA_CANCELLATION_INTERVAL: u64 = 256;

/// Registered reason why a Page font cannot be acquired by the embedded simple-font profile.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FontResourceUnsupportedKind {
    /// The Page `/Font` category is stored in an indirect dictionary.
    IndirectFontDictionary,
    /// The selected Page resource embeds a direct Font dictionary.
    DirectFont,
    /// The selected indirect Font object is a whole-object reference alias.
    FontAlias,
    /// The selected Font subtype is neither registered simple TrueType nor Type1.
    NonTrueType,
    /// Encoding is outside the registered direct WinAnsi or Type1 StandardEncoding subset.
    UnsupportedEncoding,
    /// A metadata field is stored through an unsupported indirect reference.
    IndirectMetadata,
    /// The PDF Widths representation is outside the registered direct integer profile.
    UnsupportedWidths,
    /// The declared character-code interval does not cover printable ASCII.
    UnsupportedCharacterRange,
    /// No registered embedded font program is selected.
    MissingEmbeddedProgram,
    /// A FontDescriptor object is a whole-object alias.
    FontDescriptorAlias,
    /// An embedded font-program object is a whole-object alias or a non-stream object.
    FontFileAlias,
    /// The embedded program uses a filter outside identity or one direct FlateDecode.
    UnsupportedFilter,
    /// The embedded program declares non-default or indirect decode parameters.
    UnsupportedDecodeParameters,
    /// The decoded TrueType parser selected a capability outside its registered profile.
    TrueTypeProgram,
    /// The decoded Type1C parser selected a capability outside its registered profile.
    Type1CProgram,
    /// A FontFile3 stream does not directly declare `/Subtype /Type1C`.
    UnsupportedProgramSubtype,
}

/// Source-redacted typed capability outcome for Page Font lookup or acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontResourceUnsupported {
    kind: FontResourceUnsupportedKind,
    reference: ObjectRef,
    offset: u64,
    font: Option<FontUnsupported>,
}

impl FontResourceUnsupported {
    pub(crate) const fn new(
        kind: FontResourceUnsupportedKind,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        Self {
            kind,
            reference,
            offset,
            font: None,
        }
    }

    const fn from_font(
        kind: PdfFontKind,
        reference: ObjectRef,
        offset: u64,
        font: FontUnsupported,
    ) -> Self {
        Self {
            kind: match kind {
                PdfFontKind::TrueType => FontResourceUnsupportedKind::TrueTypeProgram,
                PdfFontKind::Type1C => FontResourceUnsupportedKind::Type1CProgram,
            },
            reference,
            offset,
            font: Some(font),
        }
    }

    /// Returns the stable unsupported capability kind.
    pub const fn kind(self) -> FontResourceUnsupportedKind {
        self.kind
    }
    /// Returns the relevant indirect object identity.
    pub const fn reference(self) -> ObjectRef {
        self.reference
    }
    /// Returns the exact source offset selecting the unsupported representation.
    pub const fn offset(self) -> u64 {
        self.offset
    }
    /// Returns the lower typed embedded-font capability, when parsing selected it.
    pub const fn font_unsupported(self) -> Option<FontUnsupported> {
        self.font
    }
    /// Returns a stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            FontResourceUnsupportedKind::IndirectFontDictionary => "RPE-DOCUMENT-FONT-0001",
            FontResourceUnsupportedKind::DirectFont => "RPE-DOCUMENT-FONT-0002",
            FontResourceUnsupportedKind::FontAlias => "RPE-DOCUMENT-FONT-0003",
            FontResourceUnsupportedKind::NonTrueType => "RPE-DOCUMENT-FONT-0004",
            FontResourceUnsupportedKind::UnsupportedEncoding => "RPE-DOCUMENT-FONT-0005",
            FontResourceUnsupportedKind::IndirectMetadata => "RPE-DOCUMENT-FONT-0006",
            FontResourceUnsupportedKind::UnsupportedWidths => "RPE-DOCUMENT-FONT-0007",
            FontResourceUnsupportedKind::MissingEmbeddedProgram => "RPE-DOCUMENT-FONT-0008",
            FontResourceUnsupportedKind::FontDescriptorAlias => "RPE-DOCUMENT-FONT-0009",
            FontResourceUnsupportedKind::FontFileAlias => "RPE-DOCUMENT-FONT-0010",
            FontResourceUnsupportedKind::UnsupportedFilter => "RPE-DOCUMENT-FONT-0011",
            FontResourceUnsupportedKind::UnsupportedDecodeParameters => "RPE-DOCUMENT-FONT-0012",
            FontResourceUnsupportedKind::TrueTypeProgram => "RPE-DOCUMENT-FONT-0013",
            FontResourceUnsupportedKind::UnsupportedCharacterRange => "RPE-DOCUMENT-FONT-0014",
            FontResourceUnsupportedKind::Type1CProgram => "RPE-DOCUMENT-FONT-0015",
            FontResourceUnsupportedKind::UnsupportedProgramSubtype => "RPE-DOCUMENT-FONT-0016",
        }
    }
}

/// Runtime identity and exact checkpoints for Font, descendant container/CIDFont, encoding,
/// descriptor, program, and payload acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontResourceJobContext {
    job: JobId,
    font_envelope_checkpoint: ResumeCheckpoint,
    font_boundary_checkpoint: ResumeCheckpoint,
    descendant_envelope_checkpoint: ResumeCheckpoint,
    descendant_boundary_checkpoint: ResumeCheckpoint,
    encoding_envelope_checkpoint: ResumeCheckpoint,
    encoding_boundary_checkpoint: ResumeCheckpoint,
    descriptor_envelope_checkpoint: ResumeCheckpoint,
    descriptor_boundary_checkpoint: ResumeCheckpoint,
    program_envelope_checkpoint: ResumeCheckpoint,
    program_boundary_checkpoint: ResumeCheckpoint,
    payload_checkpoint: ResumeCheckpoint,
    priority: RequestPriority,
}

impl FontResourceJobContext {
    /// Creates a context whose proof-preserving checkpoints remain runtime-owned.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        job: JobId,
        font_envelope_checkpoint: ResumeCheckpoint,
        font_boundary_checkpoint: ResumeCheckpoint,
        descendant_envelope_checkpoint: ResumeCheckpoint,
        descendant_boundary_checkpoint: ResumeCheckpoint,
        encoding_envelope_checkpoint: ResumeCheckpoint,
        encoding_boundary_checkpoint: ResumeCheckpoint,
        descriptor_envelope_checkpoint: ResumeCheckpoint,
        descriptor_boundary_checkpoint: ResumeCheckpoint,
        program_envelope_checkpoint: ResumeCheckpoint,
        program_boundary_checkpoint: ResumeCheckpoint,
        payload_checkpoint: ResumeCheckpoint,
        priority: RequestPriority,
    ) -> Self {
        Self {
            job,
            font_envelope_checkpoint,
            font_boundary_checkpoint,
            descendant_envelope_checkpoint,
            descendant_boundary_checkpoint,
            encoding_envelope_checkpoint,
            encoding_boundary_checkpoint,
            descriptor_envelope_checkpoint,
            descriptor_boundary_checkpoint,
            program_envelope_checkpoint,
            program_boundary_checkpoint,
            payload_checkpoint,
            priority,
        }
    }

    /// Returns the owning runtime job identity.
    pub const fn job(self) -> JobId {
        self.job
    }
    /// Returns the selected Font object envelope checkpoint.
    pub const fn font_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.font_envelope_checkpoint
    }
    /// Returns the selected Font object boundary checkpoint.
    pub const fn font_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.font_boundary_checkpoint
    }
    /// Returns the Type0 descendant Font object envelope checkpoint.
    pub const fn descendant_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.descendant_envelope_checkpoint
    }
    /// Returns the Type0 descendant Font object boundary checkpoint.
    pub const fn descendant_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.descendant_boundary_checkpoint
    }
    /// Returns the indirect Encoding object envelope checkpoint.
    pub const fn encoding_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.encoding_envelope_checkpoint
    }
    /// Returns the indirect Encoding object boundary checkpoint.
    pub const fn encoding_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.encoding_boundary_checkpoint
    }
    /// Returns the FontDescriptor object envelope checkpoint.
    pub const fn descriptor_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.descriptor_envelope_checkpoint
    }
    /// Returns the FontDescriptor object boundary checkpoint.
    pub const fn descriptor_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.descriptor_boundary_checkpoint
    }
    /// Returns the embedded-program object envelope checkpoint.
    pub const fn program_envelope_checkpoint(self) -> ResumeCheckpoint {
        self.program_envelope_checkpoint
    }
    /// Returns the embedded-program object boundary checkpoint.
    pub const fn program_boundary_checkpoint(self) -> ResumeCheckpoint {
        self.program_boundary_checkpoint
    }
    /// Returns the exact embedded-program payload checkpoint.
    pub const fn payload_checkpoint(self) -> ResumeCheckpoint {
        self.payload_checkpoint
    }
    /// Returns the scheduling priority copied to all source requests.
    pub const fn priority(self) -> RequestPriority {
        self.priority
    }

    fn checkpoints(self) -> [ResumeCheckpoint; 11] {
        [
            self.font_envelope_checkpoint,
            self.font_boundary_checkpoint,
            self.descendant_envelope_checkpoint,
            self.descendant_boundary_checkpoint,
            self.encoding_envelope_checkpoint,
            self.encoding_boundary_checkpoint,
            self.descriptor_envelope_checkpoint,
            self.descriptor_boundary_checkpoint,
            self.program_envelope_checkpoint,
            self.program_boundary_checkpoint,
            self.payload_checkpoint,
        ]
    }
}

/// Public resumable phase of one embedded simple-font acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontResourcePhase {
    /// The selected simple Font dictionary is being reopened.
    Font,
    /// A Type0 descendant array or CIDFontType2 dictionary is being reopened.
    Descendant,
    /// An indirect simple-font Encoding dictionary is being reopened.
    Encoding,
    /// An indirect FontDescriptor dictionary is being reopened.
    Descriptor,
    /// The indirect embedded font-program stream object is being reopened.
    Program,
    /// The exact embedded program payload is being acquired, decoded, and parsed.
    Payload,
    /// A proof-bearing immutable parsed font was published.
    Ready,
    /// A stable typed unsupported capability was reached.
    Unsupported,
    /// A stable structured failure was reached.
    Failed,
}

/// Deterministic work and retained-state accounting for one Font resource.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FontResourceStats {
    polls: u64,
    objects: u64,
    reference_edges: u64,
    object_read_bytes: u64,
    object_parse_bytes: u64,
    metadata_entries: u64,
    widths: u64,
    encoded_bytes: u64,
    decoded_bytes: u64,
    decode_fuel: u64,
    font: FontStats,
    retained_bytes: u64,
    peak_retained_bytes: u64,
}

impl FontResourceStats {
    /// Returns active poll calls admitted by the acquisition budget.
    pub const fn polls(self) -> u64 {
        self.polls
    }
    /// Returns proof-preserving indirect objects started.
    pub const fn objects(self) -> u64 {
        self.objects
    }
    /// Returns exact indirect structural edges followed.
    pub const fn reference_edges(self) -> u64 {
        self.reference_edges
    }
    /// Returns cumulative exact-read bytes across object children.
    pub const fn object_read_bytes(self) -> u64 {
        self.object_read_bytes
    }
    /// Returns cumulative parser-window bytes across object children.
    pub const fn object_parse_bytes(self) -> u64 {
        self.object_parse_bytes
    }
    /// Returns Font, descriptor, and embedded-program metadata entries visited.
    pub const fn metadata_entries(self) -> u64 {
        self.metadata_entries
    }
    /// Returns entries visited in the direct PDF Widths array.
    pub const fn widths(self) -> u64 {
        self.widths
    }
    /// Returns exact encoded embedded-program bytes acquired.
    pub const fn encoded_bytes(self) -> u64 {
        self.encoded_bytes
    }
    /// Returns exact embedded-program bytes produced by a successful decode.
    ///
    /// This includes output subsequently rejected because it does not equal `/Length1`.
    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }
    /// Returns foundational stream-decoder fuel consumed.
    pub const fn decode_fuel(self) -> u64 {
        self.decode_fuel
    }
    /// Returns complete lower embedded-font parser statistics.
    pub const fn font(self) -> FontStats {
        self.font
    }
    /// Returns conservatively accounted state retained by the published font.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
    /// Returns greatest conservatively accounted retained state observed.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

/// Published PDF character metrics and proof-bearing immutable embedded font program.
pub struct AcquiredFontResource {
    proof: PageFontReference,
    font_object: AttestedObject,
    descendant_array_object: Option<AttestedObject>,
    descendant_object: Option<AttestedObject>,
    encoding_object: Option<AttestedObject>,
    descriptor_object: Option<AttestedObject>,
    program_object: AttestedObject,
    codes: AcquiredFontCodes,
    decoded_program: DecodedStream,
    font: FontProgram,
    limits: FontResourceLimits,
    stats: FontResourceStats,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CidWidth {
    code: u16,
    width: u32,
}

#[allow(
    clippy::large_enum_variant,
    reason = "the fixed simple-font maps deliberately avoid a fallible publication allocation"
)]
enum AcquiredFontCodes {
    Simple {
        first_char: u8,
        last_char: u8,
        widths: [u32; 256],
        glyph_ids: [GlyphId; 256],
    },
    IdentityH {
        default_width: u32,
        widths: Box<[CidWidth]>,
    },
}

impl AcquiredFontResource {
    /// Returns the exact Page Font lookup proof authorizing this acquisition.
    pub const fn proof(&self) -> PageFontReference {
        self.proof
    }
    /// Returns the selected indirect Font identity.
    pub const fn reference(&self) -> ObjectRef {
        self.proof.target()
    }
    /// Borrows the proof-bound selected Font dictionary object.
    pub const fn font_object(&self) -> &AttestedObject {
        &self.font_object
    }
    /// Borrows an indirect Type0 `/DescendantFonts` array, when the array is indirect.
    pub const fn descendant_array_object(&self) -> Option<&AttestedObject> {
        self.descendant_array_object.as_ref()
    }
    /// Borrows the Type0 descendant CIDFont object, when this is a composite font.
    pub const fn descendant_object(&self) -> Option<&AttestedObject> {
        self.descendant_object.as_ref()
    }
    /// Borrows the indirect Encoding object, or `None` for a direct or predefined encoding.
    pub const fn encoding_object(&self) -> Option<&AttestedObject> {
        self.encoding_object.as_ref()
    }
    /// Borrows the indirect FontDescriptor object, or `None` for an embedded direct descriptor.
    pub const fn descriptor_object(&self) -> Option<&AttestedObject> {
        self.descriptor_object.as_ref()
    }
    /// Borrows the proof-bound indirect embedded font-program stream object.
    pub const fn program_object(&self) -> &AttestedObject {
        &self.program_object
    }
    /// Returns the direct simple-font FirstChar value, or `None` for a composite font.
    pub const fn first_char(&self) -> Option<u8> {
        match &self.codes {
            AcquiredFontCodes::Simple { first_char, .. } => Some(*first_char),
            AcquiredFontCodes::IdentityH { .. } => None,
        }
    }
    /// Returns the direct simple-font LastChar value, or `None` for a composite font.
    pub const fn last_char(&self) -> Option<u8> {
        match &self.codes {
            AcquiredFontCodes::Simple { last_char, .. } => Some(*last_char),
            AcquiredFontCodes::IdentityH { .. } => None,
        }
    }
    /// Reports whether strings use two-byte big-endian Identity-H character codes.
    pub const fn uses_identity_h(&self) -> bool {
        matches!(&self.codes, AcquiredFontCodes::IdentityH { .. })
    }
    /// Returns the number of character codes in one PDF string under this font.
    pub fn character_code_count(&self, bytes: &[u8]) -> Option<usize> {
        match &self.codes {
            AcquiredFontCodes::Simple { .. } => Some(bytes.len()),
            AcquiredFontCodes::IdentityH { .. } => {
                bytes.len().is_multiple_of(2).then_some(bytes.len() / 2)
            }
        }
    }
    /// Decodes the next font-owned character code and advances the caller's byte cursor.
    pub fn decode_next_character_code(&self, bytes: &[u8], cursor: &mut usize) -> Option<u32> {
        match &self.codes {
            AcquiredFontCodes::Simple { .. } => {
                let byte = *bytes.get(*cursor)?;
                *cursor = (*cursor).checked_add(1)?;
                Some(u32::from(byte))
            }
            AcquiredFontCodes::IdentityH { .. } => {
                let pair: [u8; 2] = bytes
                    .get(*cursor..(*cursor).checked_add(2)?)?
                    .try_into()
                    .ok()?;
                *cursor = (*cursor).checked_add(2)?;
                Some(u32::from(u16::from_be_bytes(pair)))
            }
        }
    }
    /// Returns the PDF Widths advance for one represented character byte.
    ///
    /// This intentionally does not consult the embedded program's own metrics: PDF advancement is
    /// governed by the simple Font dictionary's Widths array.
    pub fn pdf_width_for_code(&self, byte: u8) -> Option<u32> {
        match &self.codes {
            AcquiredFontCodes::Simple {
                first_char,
                last_char,
                widths,
                ..
            } if byte >= *first_char && byte <= *last_char => {
                widths.get(usize::from(byte)).copied()
            }
            _ => None,
        }
    }
    /// Returns the PDF Widths advance for one represented WinAnsi character byte.
    ///
    /// This compatibility alias is retained for simple TrueType callers while the resource model
    /// also admits Type1 encodings whose valid code range begins below `0x20`.
    pub fn pdf_width_for_winansi(&self, byte: u8) -> Option<u32> {
        self.pdf_width_for_code(byte)
    }
    /// Resolves one represented PDF character code to the embedded program glyph.
    pub fn glyph_id_for_code(&self, byte: u8) -> Option<GlyphId> {
        match &self.codes {
            AcquiredFontCodes::Simple {
                first_char,
                last_char,
                glyph_ids,
                ..
            } if byte >= *first_char && byte <= *last_char => {
                glyph_ids.get(usize::from(byte)).copied()
            }
            _ => None,
        }
    }
    /// Returns the PDF advance for one font-owned character code.
    pub fn pdf_width_for_character_code(&self, code: u32) -> Option<u32> {
        match &self.codes {
            AcquiredFontCodes::Simple { .. } => self.pdf_width_for_code(u8::try_from(code).ok()?),
            AcquiredFontCodes::IdentityH {
                default_width,
                widths,
            } => {
                let code = u16::try_from(code).ok()?;
                Some(
                    widths
                        .binary_search_by_key(&code, |entry| entry.code)
                        .ok()
                        .and_then(|index| widths.get(index))
                        .map_or(*default_width, |entry| entry.width),
                )
            }
        }
    }
    /// Resolves one font-owned character code to an embedded glyph.
    pub fn glyph_id_for_character_code(&self, code: u32) -> Option<GlyphId> {
        match &self.codes {
            AcquiredFontCodes::Simple { .. } => self.glyph_id_for_code(u8::try_from(code).ok()?),
            AcquiredFontCodes::IdentityH { .. } => {
                let glyph = u16::try_from(code).ok()?;
                (glyph < self.font.glyph_count()).then_some(GlyphId::new(glyph))
            }
        }
    }
    /// Borrows the sealed decoded embedded-program stream proof and bytes.
    pub const fn decoded_program(&self) -> &DecodedStream {
        &self.decoded_program
    }
    /// Borrows the immutable project-owned parsed font program.
    pub const fn font(&self) -> &FontProgram {
        &self.font
    }
    /// Returns the validated acquisition and lower-parser profile.
    pub const fn limits(&self) -> FontResourceLimits {
        self.limits
    }
    /// Returns deterministic acquisition, decode, parse, and retention accounting.
    pub const fn stats(&self) -> FontResourceStats {
        self.stats
    }
}

impl fmt::Debug for AcquiredFontResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquiredFontResource")
            .field("reference", &self.reference())
            .field(
                "descendant_array_reference",
                &self
                    .descendant_array_object
                    .as_ref()
                    .map(AttestedObject::reference),
            )
            .field(
                "descendant_reference",
                &self
                    .descendant_object
                    .as_ref()
                    .map(AttestedObject::reference),
            )
            .field(
                "encoding_reference",
                &self.encoding_object.as_ref().map(AttestedObject::reference),
            )
            .field(
                "descriptor_reference",
                &self
                    .descriptor_object
                    .as_ref()
                    .map(AttestedObject::reference),
            )
            .field("program_reference", &self.program_object.reference())
            .field("first_char", &self.first_char())
            .field("last_char", &self.last_char())
            .field("identity_h", &self.uses_identity_h())
            .field("limits", &self.limits)
            .field("stats", &self.stats)
            .field("program", &"[REDACTED]")
            .finish()
    }
}

/// Result of polling one embedded simple-font acquisition.
pub enum FontResourcePoll {
    /// The proof-bearing parsed font is ready.
    Ready(Arc<AcquiredFontResource>),
    /// One object or exact payload request requires absent source bytes.
    Pending {
        /// One-shot data-arrival ticket returned by the byte source.
        ticket: DataTicket,
        /// Canonical exact ranges still missing from the active request.
        missing: SmallRanges,
        /// Exact object or payload checkpoint to retain.
        checkpoint: ResumeCheckpoint,
    },
    /// The selected representation is valid but outside the registered subset.
    Unsupported(FontResourceUnsupported),
    /// The job reached a stable structured failure.
    Failed(DocumentError),
}

impl fmt::Debug for FontResourcePoll {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(font) => formatter.debug_tuple("Ready").field(font).finish(),
            Self::Pending {
                ticket,
                missing,
                checkpoint,
            } => formatter
                .debug_struct("Pending")
                .field("ticket", ticket)
                .field("missing", missing)
                .field("checkpoint", checkpoint)
                .finish(),
            Self::Unsupported(value) => formatter.debug_tuple("Unsupported").field(value).finish(),
            Self::Failed(error) => formatter.debug_tuple("Failed").field(error).finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildKind {
    Font,
    DescendantArray,
    Descendant,
    Encoding,
    Descriptor,
    Program,
}

struct ChildState {
    kind: ChildKind,
    job: OpenAttestedObjectJob,
    work_caps: ObjectWorkCaps,
    base_read_bytes: u64,
    base_parse_bytes: u64,
}

#[derive(Clone, Copy)]
enum DescriptorPlan {
    Indirect(ObjectRef),
    Program(ObjectRef),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PdfFontKind {
    TrueType,
    Type1C,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SimpleEncoding {
    WinAnsi,
    Type1Standard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Type1Difference {
    code: u8,
    name: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedType1Encoding {
    encoding: SimpleEncoding,
    differences: Vec<Type1Difference>,
}

enum Type1EncodingPlan {
    Ready(ParsedType1Encoding),
    Indirect(ObjectRef),
}

#[allow(
    clippy::large_enum_variant,
    reason = "the bounded fixed simple-font metadata stays allocation-free while it is inspected"
)]
enum FontInspection {
    Ready(PdfFontMetadata),
    DescendantArray(ObjectRef),
    Descendant(ObjectRef),
}

#[derive(Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "the fixed simple-font width map deliberately avoids parser-owned heap state"
)]
enum PdfFontCodes {
    Simple {
        encoding: SimpleEncoding,
        pending_encoding: Option<ObjectRef>,
        first_char: u8,
        last_char: u8,
        widths: [u32; 256],
        type1_differences: Vec<Type1Difference>,
    },
    IdentityH {
        default_width: u32,
        widths: Vec<CidWidth>,
    },
}

#[derive(Clone)]
struct PdfFontMetadata {
    kind: PdfFontKind,
    codes: PdfFontCodes,
    descriptor: DescriptorPlan,
}

#[derive(Clone, Copy)]
enum RegisteredFilter {
    Identity,
    Flate,
}

#[derive(Clone, Copy)]
struct ProgramMetadata {
    kind: PdfFontKind,
    filter: RegisteredFilter,
    decoded_bytes: Option<u64>,
}

enum FontJobState {
    Active,
    Ready(Arc<AcquiredFontResource>),
    Unsupported(FontResourceUnsupported),
    Failed(DocumentError),
}

enum StageResult {
    Continue,
    Unsupported(FontResourceUnsupported),
    Failed(DocumentError),
}

enum PayloadResult {
    Ready(Arc<AcquiredFontResource>),
    Pending {
        ticket: DataTicket,
        missing: SmallRanges,
    },
    Unsupported(FontResourceUnsupported),
    Failed(DocumentError),
}

struct DecodeCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl DecodeCancellation for DecodeCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

struct FontCancellationAdapter<'a>(&'a dyn DocumentCancellation);

impl FontCancellation for FontCancellationAdapter<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

/// Resumable proof-bound acquisition of one embedded simple Font resource.
pub struct AcquireFontResourceJob {
    authority: SharedAttestedRevisionIndex,
    snapshot: SourceSnapshot,
    proof: PageFontReference,
    context: FontResourceJobContext,
    limits: FontResourceLimits,
    child: Option<ChildState>,
    font_object: Option<AttestedObject>,
    descendant_array_object: Option<AttestedObject>,
    descendant_object: Option<AttestedObject>,
    encoding_object: Option<AttestedObject>,
    descriptor_object: Option<AttestedObject>,
    program_object: Option<AttestedObject>,
    metadata: Option<PdfFontMetadata>,
    program_metadata: Option<ProgramMetadata>,
    stats: FontResourceStats,
    state: FontJobState,
}

impl AcquireFontResourceJob {
    /// Returns the immutable source snapshot covered by lookup and revision proofs.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }
    /// Returns the exact Page Font lookup proof being acquired.
    pub const fn proof(&self) -> PageFontReference {
        self.proof
    }
    /// Returns runtime identity and all exact resume checkpoints.
    pub const fn context(&self) -> FontResourceJobContext {
        self.context
    }
    /// Returns the validated acquisition and lower-parser profile.
    pub const fn limits(&self) -> FontResourceLimits {
        self.limits
    }
    /// Returns deterministic accounting through the latest poll.
    pub const fn stats(&self) -> FontResourceStats {
        self.stats
    }
    /// Returns the public resumable acquisition phase.
    pub const fn phase(&self) -> FontResourcePhase {
        match self.state {
            FontJobState::Ready(_) => FontResourcePhase::Ready,
            FontJobState::Unsupported(_) => FontResourcePhase::Unsupported,
            FontJobState::Failed(_) => FontResourcePhase::Failed,
            FontJobState::Active => match self.child.as_ref() {
                Some(child) => match child.kind {
                    ChildKind::Font => FontResourcePhase::Font,
                    ChildKind::DescendantArray | ChildKind::Descendant => {
                        FontResourcePhase::Descendant
                    }
                    ChildKind::Encoding => FontResourcePhase::Encoding,
                    ChildKind::Descriptor => FontResourcePhase::Descriptor,
                    ChildKind::Program => FontResourcePhase::Program,
                },
                None => FontResourcePhase::Payload,
            },
        }
    }

    /// Advances acquisition without platform font services or callback-owned resumption.
    pub fn poll(
        &mut self,
        source: &(dyn ByteSource + '_),
        cancellation: &(dyn DocumentCancellation + '_),
    ) -> FontResourcePoll {
        match &self.state {
            FontJobState::Ready(font) => return FontResourcePoll::Ready(Arc::clone(font)),
            FontJobState::Unsupported(value) => return FontResourcePoll::Unsupported(*value),
            FontJobState::Failed(error) => return FontResourcePoll::Failed(*error),
            FontJobState::Active => {}
        }
        if let Err(error) = self.runtime_guard(source, cancellation, None) {
            return self.fail(error);
        }
        if let Err(fallback) = self.charge_poll() {
            let error = self.prioritize_runtime_error(source, cancellation, fallback);
            return self.fail(error);
        }

        loop {
            if self.child.is_none() {
                return match self.poll_payload(source, cancellation) {
                    PayloadResult::Ready(font) => self.ready(font),
                    PayloadResult::Pending { ticket, missing } => FontResourcePoll::Pending {
                        ticket,
                        missing,
                        checkpoint: self.context.payload_checkpoint(),
                    },
                    PayloadResult::Unsupported(value) => self.unsupported(value),
                    PayloadResult::Failed(fallback) => {
                        let error = self.prioritize_runtime_error(source, cancellation, fallback);
                        self.fail(error)
                    }
                };
            }

            let retained_objects = match self.retained_object_bytes() {
                Ok(value) => value,
                Err(error) => return self.fail(error),
            };
            let outcome = {
                let child = self.child.as_mut().expect("checked above");
                let outcome = child.job.poll(source, cancellation);
                let read = child
                    .base_read_bytes
                    .checked_add(child.job.stats().read_bytes());
                let parse = child
                    .base_parse_bytes
                    .checked_add(child.job.stats().parse_bytes());
                match (read, parse) {
                    (Some(read), Some(parse)) => {
                        self.stats.object_read_bytes = read;
                        self.stats.object_parse_bytes = parse;
                        let retained = match retained_objects
                            .checked_add(child.job.stats().retained_heap_bytes())
                        {
                            Some(value) => value,
                            None => return self.fail(self.internal_error(None)),
                        };
                        self.stats.peak_retained_bytes =
                            self.stats.peak_retained_bytes.max(retained);
                    }
                    _ => return self.fail(self.internal_error(None)),
                }
                outcome
            };
            if let Err(error) = self.runtime_guard(source, cancellation, self.current_offset()) {
                return self.fail(error);
            }
            match outcome {
                AttestedObjectPoll::Pending {
                    ticket,
                    missing,
                    checkpoint,
                } => {
                    return FontResourcePoll::Pending {
                        ticket,
                        missing,
                        checkpoint,
                    };
                }
                AttestedObjectPoll::Failed(error) => {
                    let fallback = self.map_child_error(error);
                    let error = self.prioritize_runtime_error(source, cancellation, fallback);
                    return self.fail(error);
                }
                AttestedObjectPoll::Ready(object) => {
                    let kind = self.child.as_ref().expect("child produced object").kind;
                    self.child = None;
                    match self.accept_object(kind, object, source, cancellation) {
                        StageResult::Continue => continue,
                        StageResult::Unsupported(value) => return self.unsupported(value),
                        StageResult::Failed(fallback) => {
                            let error =
                                self.prioritize_runtime_error(source, cancellation, fallback);
                            return self.fail(error);
                        }
                    }
                }
            }
        }
    }

    fn accept_object(
        &mut self,
        kind: ChildKind,
        object: AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> StageResult {
        if object.snapshot() != self.snapshot
            || object.revision_id() != self.proof.revision_id()
            || object.revision_startxref() != self.proof.revision_startxref()
        {
            return StageResult::Failed(self.internal_error(Some(object.object_span().start())));
        }
        let result = match kind {
            ChildKind::Font => match self.inspect_font(&object, source, cancellation) {
                Ok(Ok(FontInspection::Ready(metadata))) => {
                    let pending_encoding = match &metadata.codes {
                        PdfFontCodes::Simple {
                            pending_encoding, ..
                        } => *pending_encoding,
                        PdfFontCodes::IdentityH { .. } => None,
                    };
                    let descriptor = metadata.descriptor;
                    self.metadata = Some(metadata);
                    self.font_object = Some(object);
                    match pending_encoding {
                        Some(reference) => self.follow_and_start(reference, ChildKind::Encoding),
                        None => match descriptor {
                            DescriptorPlan::Indirect(reference) => {
                                self.follow_and_start(reference, ChildKind::Descriptor)
                            }
                            DescriptorPlan::Program(reference) => {
                                self.follow_and_start(reference, ChildKind::Program)
                            }
                        },
                    }
                }
                Ok(Ok(FontInspection::Descendant(reference))) => {
                    self.font_object = Some(object);
                    self.follow_and_start(reference, ChildKind::Descendant)
                }
                Ok(Ok(FontInspection::DescendantArray(reference))) => {
                    self.font_object = Some(object);
                    self.follow_and_start(reference, ChildKind::DescendantArray)
                }
                Ok(Err(value)) => return StageResult::Unsupported(value),
                Err(error) => return StageResult::Failed(error),
            },
            ChildKind::DescendantArray => {
                let reference = match self.inspect_descendant_array(&object, source, cancellation) {
                    Ok(Ok(reference)) => reference,
                    Ok(Err(value)) => return StageResult::Unsupported(value),
                    Err(error) => return StageResult::Failed(error),
                };
                self.descendant_array_object = Some(object);
                self.follow_and_start(reference, ChildKind::Descendant)
            }
            ChildKind::Descendant => {
                let metadata = match self.inspect_descendant(&object, source, cancellation) {
                    Ok(Ok(metadata)) => metadata,
                    Ok(Err(value)) => return StageResult::Unsupported(value),
                    Err(error) => return StageResult::Failed(error),
                };
                let descriptor = metadata.descriptor;
                self.metadata = Some(metadata);
                self.descendant_object = Some(object);
                match descriptor {
                    DescriptorPlan::Indirect(reference) => {
                        self.follow_and_start(reference, ChildKind::Descriptor)
                    }
                    DescriptorPlan::Program(reference) => {
                        self.follow_and_start(reference, ChildKind::Program)
                    }
                }
            }
            ChildKind::Encoding => {
                let parsed = match self.inspect_type1_encoding_object(&object, source, cancellation)
                {
                    Ok(Ok(parsed)) => parsed,
                    Ok(Err(value)) => return StageResult::Unsupported(value),
                    Err(error) => return StageResult::Failed(error),
                };
                let descriptor = match self.metadata.as_mut() {
                    Some(PdfFontMetadata {
                        codes:
                            PdfFontCodes::Simple {
                                encoding,
                                pending_encoding,
                                type1_differences,
                                ..
                            },
                        descriptor,
                        ..
                    }) if *pending_encoding == Some(object.reference()) => {
                        *encoding = parsed.encoding;
                        *type1_differences = parsed.differences;
                        *pending_encoding = None;
                        *descriptor
                    }
                    Some(_) => {
                        return StageResult::Failed(
                            self.internal_error(Some(object.object_span().start())),
                        );
                    }
                    None => {
                        return StageResult::Failed(
                            self.internal_error(Some(object.object_span().start())),
                        );
                    }
                };
                self.encoding_object = Some(object);
                match descriptor {
                    DescriptorPlan::Indirect(reference) => {
                        self.follow_and_start(reference, ChildKind::Descriptor)
                    }
                    DescriptorPlan::Program(reference) => {
                        self.follow_and_start(reference, ChildKind::Program)
                    }
                }
            }
            ChildKind::Descriptor => {
                let reference = match self.inspect_descriptor(&object, source, cancellation) {
                    Ok(Ok(reference)) => reference,
                    Ok(Err(value)) => return StageResult::Unsupported(value),
                    Err(error) => return StageResult::Failed(error),
                };
                self.descriptor_object = Some(object);
                self.follow_and_start(reference, ChildKind::Program)
            }
            ChildKind::Program => {
                let metadata = match self.inspect_program(&object, source, cancellation) {
                    Ok(Ok(metadata)) => metadata,
                    Ok(Err(value)) => return StageResult::Unsupported(value),
                    Err(error) => return StageResult::Failed(error),
                };
                self.program_metadata = Some(metadata);
                self.program_object = Some(object);
                Ok(())
            }
        };
        match result {
            Ok(()) => StageResult::Continue,
            Err(error) => StageResult::Failed(error),
        }
    }

    fn follow_and_start(
        &mut self,
        reference: ObjectRef,
        kind: ChildKind,
    ) -> Result<(), DocumentError> {
        self.charge_reference_edge(reference)?;
        self.start_child(reference, kind)
    }

    fn start_child(&mut self, reference: ObjectRef, kind: ChildKind) -> Result<(), DocumentError> {
        let offset = self
            .authority
            .as_attested()
            .attestation(reference)?
            .xref_offset();
        if self.stats.objects >= self.limits.max_objects() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceObjects,
                self.limits.max_objects(),
                self.stats.objects,
                1,
                reference,
                Some(offset),
            ));
        }
        if self.reference_seen(reference) {
            return Err(invalid_font(reference, offset));
        }
        let work_caps = self.child_work_caps(reference, offset)?;
        let (envelope, boundary) = match kind {
            ChildKind::Font => (
                self.context.font_envelope_checkpoint(),
                self.context.font_boundary_checkpoint(),
            ),
            ChildKind::DescendantArray | ChildKind::Descendant => (
                self.context.descendant_envelope_checkpoint(),
                self.context.descendant_boundary_checkpoint(),
            ),
            ChildKind::Encoding => (
                self.context.encoding_envelope_checkpoint(),
                self.context.encoding_boundary_checkpoint(),
            ),
            ChildKind::Descriptor => (
                self.context.descriptor_envelope_checkpoint(),
                self.context.descriptor_boundary_checkpoint(),
            ),
            ChildKind::Program => (
                self.context.program_envelope_checkpoint(),
                self.context.program_boundary_checkpoint(),
            ),
        };
        let context = AttestedObjectJobContext::new(
            self.context.job(),
            envelope,
            boundary,
            self.context.priority(),
        );
        let job = self
            .authority
            .as_attested()
            .open_object(reference, context, work_caps)?;
        self.stats.objects = self
            .stats
            .objects
            .checked_add(1)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        self.child = Some(ChildState {
            kind,
            job,
            work_caps,
            base_read_bytes: self.stats.object_read_bytes,
            base_parse_bytes: self.stats.object_parse_bytes,
        });
        Ok(())
    }

    fn reference_seen(&self, reference: ObjectRef) -> bool {
        self.proof.target() == reference
            || self
                .descendant_array_object
                .as_ref()
                .is_some_and(|object| object.reference() == reference)
            || self
                .descendant_object
                .as_ref()
                .is_some_and(|object| object.reference() == reference)
            || self
                .encoding_object
                .as_ref()
                .is_some_and(|object| object.reference() == reference)
            || self
                .descriptor_object
                .as_ref()
                .is_some_and(|object| object.reference() == reference)
            || self
                .program_object
                .as_ref()
                .is_some_and(|object| object.reference() == reference)
    }

    fn child_work_caps(
        &self,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<ObjectWorkCaps, DocumentError> {
        let authority = self.authority.as_attested();
        let remaining_read = self
            .limits
            .max_object_read_bytes()
            .checked_sub(self.stats.object_read_bytes)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        let remaining_parse = self
            .limits
            .max_object_parse_bytes()
            .checked_sub(self.stats.object_parse_bytes)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        if remaining_read == 0 {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceObjectReadBytes,
                self.limits.max_object_read_bytes(),
                self.stats.object_read_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        if remaining_parse == 0 {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceObjectParseBytes,
                self.limits.max_object_parse_bytes(),
                self.stats.object_parse_bytes,
                1,
                reference,
                Some(offset),
            ));
        }
        let intrinsic = authority
            .syntax_limits()
            .max_owned_bytes()
            .checked_add(authority.syntax_limits().max_container_bytes())
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        let retained_objects = self.retained_object_bytes()?;
        let remaining_retained = self
            .limits
            .max_retained_bytes()
            .checked_sub(retained_objects)
            .ok_or_else(|| {
                DocumentError::font_resource(
                    DocumentLimitKind::FontResourceRetainedBytes,
                    self.limits.max_retained_bytes(),
                    retained_objects,
                    1,
                    reference,
                    Some(offset),
                )
            })?;
        ObjectWorkCaps::new_with_retained_bytes(
            remaining_read.min(authority.object_limits().max_total_read_bytes()),
            remaining_parse.min(authority.object_limits().max_total_parse_bytes()),
            remaining_retained.min(intrinsic),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, reference, offset))
    }

    fn retained_object_bytes(&self) -> Result<u64, DocumentError> {
        let metadata_bytes = self
            .metadata
            .as_ref()
            .map_or(Some(0_u64), |metadata| match &metadata.codes {
                PdfFontCodes::Simple {
                    type1_differences, ..
                } => {
                    let entries = type1_differences
                        .capacity()
                        .checked_mul(size_of::<Type1Difference>())?;
                    type1_differences
                        .iter()
                        .try_fold(entries, |total, difference| {
                            total.checked_add(difference.name.len())
                        })
                        .and_then(|bytes| u64::try_from(bytes).ok())
                }
                PdfFontCodes::IdentityH { widths, .. } => widths
                    .capacity()
                    .checked_mul(size_of::<CidWidth>())
                    .and_then(|bytes| u64::try_from(bytes).ok()),
            })
            .ok_or_else(|| self.internal_error(self.current_offset()))?;
        [
            self.font_object.as_ref(),
            self.descendant_array_object.as_ref(),
            self.descendant_object.as_ref(),
            self.encoding_object.as_ref(),
            self.descriptor_object.as_ref(),
            self.program_object.as_ref(),
        ]
        .into_iter()
        .flatten()
        .try_fold(metadata_bytes, |sum, object| {
            sum.checked_add(object.syntax_heap_bytes())
                .ok_or_else(|| self.internal_error(Some(object.object_span().start())))
        })
    }

    fn charge_poll(&mut self) -> Result<(), DocumentError> {
        if self.stats.polls >= self.limits.max_polls() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourcePolls,
                self.limits.max_polls(),
                self.stats.polls,
                1,
                self.proof.target(),
                self.current_offset(),
            ));
        }
        self.stats.polls = self
            .stats
            .polls
            .checked_add(1)
            .ok_or_else(|| self.internal_error(None))?;
        Ok(())
    }

    fn charge_reference_edge(&mut self, reference: ObjectRef) -> Result<(), DocumentError> {
        let offset = self.current_offset();
        if self.stats.reference_edges >= self.limits.max_reference_edges() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceReferenceEdges,
                self.limits.max_reference_edges(),
                self.stats.reference_edges,
                1,
                reference,
                offset,
            ));
        }
        self.stats.reference_edges = self
            .stats
            .reference_edges
            .checked_add(1)
            .ok_or_else(|| self.internal_error(offset))?;
        Ok(())
    }

    fn inspect_font(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<FontInspection, FontResourceUnsupported>, DocumentError> {
        let reference = object.reference();
        let value = match object.value() {
            IndirectObjectValue::Stream(stream) => {
                return Err(invalid_font(reference, stream.dictionary().span().start()));
            }
            IndirectObjectValue::Direct(value) => value,
        };
        let dictionary = match value.value() {
            SyntaxObject::Dictionary(dictionary) => dictionary,
            SyntaxObject::Reference(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::FontAlias,
                    reference,
                    value.span().start(),
                )));
            }
            _ => return Err(invalid_font(reference, value.span().start())),
        };
        let slots = self.scan_font_metadata(dictionary, reference, source, cancellation)?;
        let dictionary_offset = value.span().start();

        if let Some(type_value) = slots.type_value {
            match type_value.value() {
                SyntaxObject::Name(name) if name.bytes() == b"Font" => {}
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, type_value.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, type_value.span().start())),
            }
        }
        let subtype = required_slot(slots.subtype, reference, dictionary_offset)?;
        if matches!(
            subtype.value(),
            SyntaxObject::Name(name) if name.bytes() == b"Type0"
        ) {
            let encoding = required_slot(slots.encoding, reference, dictionary_offset)?;
            match encoding.value() {
                SyntaxObject::Name(name) if name.bytes() == b"Identity-H" => {}
                SyntaxObject::Name(_) | SyntaxObject::Dictionary(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedEncoding,
                        reference,
                        encoding.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, encoding.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, encoding.span().start())),
            }
            let descendants = required_slot(slots.descendant_fonts, reference, dictionary_offset)?;
            let descendant = match descendants.value() {
                SyntaxObject::Array(values) => match values.values() {
                    [value] => match value.value() {
                        SyntaxObject::Reference(reference) => *reference,
                        _ => return Err(invalid_font(reference, value.span().start())),
                    },
                    _ => return Err(invalid_font(reference, descendants.span().start())),
                },
                SyntaxObject::Reference(target) => {
                    self.runtime_guard(source, cancellation, Some(descendants.span().start()))?;
                    return Ok(Ok(FontInspection::DescendantArray(*target)));
                }
                _ => return Err(invalid_font(reference, descendants.span().start())),
            };
            self.runtime_guard(source, cancellation, Some(descendants.span().start()))?;
            return Ok(Ok(FontInspection::Descendant(descendant)));
        }
        let kind = match subtype.value() {
            SyntaxObject::Name(name) if name.bytes() == b"TrueType" => PdfFontKind::TrueType,
            SyntaxObject::Name(name) if name.bytes() == b"Type1" => PdfFontKind::Type1C,
            SyntaxObject::Name(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::NonTrueType,
                    reference,
                    subtype.span().start(),
                )));
            }
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, subtype.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, subtype.span().start())),
        };

        let mut encoding_kind = SimpleEncoding::WinAnsi;
        let mut pending_encoding = None;
        let mut type1_differences = Vec::new();
        match kind {
            PdfFontKind::TrueType => {
                let encoding = match slots.encoding {
                    Some(value) => value,
                    None => {
                        return Ok(Err(FontResourceUnsupported::new(
                            FontResourceUnsupportedKind::UnsupportedEncoding,
                            reference,
                            dictionary_offset,
                        )));
                    }
                };
                match encoding.value() {
                    SyntaxObject::Name(name) if name.bytes() == b"WinAnsiEncoding" => {}
                    SyntaxObject::Name(_) | SyntaxObject::Dictionary(_) => {
                        return Ok(Err(FontResourceUnsupported::new(
                            FontResourceUnsupportedKind::UnsupportedEncoding,
                            reference,
                            encoding.span().start(),
                        )));
                    }
                    SyntaxObject::Reference(_) => {
                        return Ok(Err(
                            self.indirect_metadata(reference, encoding.span().start())
                        ));
                    }
                    _ => return Err(invalid_font(reference, encoding.span().start())),
                }
            }
            PdfFontKind::Type1C => {
                match self.inspect_type1_encoding(
                    slots.encoding,
                    reference,
                    dictionary_offset,
                    source,
                    cancellation,
                )? {
                    Ok(Type1EncodingPlan::Ready(parsed)) => {
                        encoding_kind = parsed.encoding;
                        type1_differences = parsed.differences;
                    }
                    Ok(Type1EncodingPlan::Indirect(reference)) => {
                        pending_encoding = Some(reference);
                        encoding_kind = SimpleEncoding::Type1Standard;
                    }
                    Err(value) => return Ok(Err(value)),
                }
            }
        }

        let first_value = required_slot(slots.first_char, reference, dictionary_offset)?;
        let first_char = match first_value.value() {
            SyntaxObject::Integer(value) => u8::try_from(*value)
                .map_err(|_| invalid_font(reference, first_value.span().start()))?,
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, first_value.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, first_value.span().start())),
        };
        let last_value = required_slot(slots.last_char, reference, dictionary_offset)?;
        let last_char = match last_value.value() {
            SyntaxObject::Integer(value) => u8::try_from(*value)
                .map_err(|_| invalid_font(reference, last_value.span().start()))?,
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, last_value.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, last_value.span().start())),
        };
        if first_char > last_char {
            return Err(invalid_font(reference, last_value.span().start()));
        }
        let widths_value = required_slot(slots.widths, reference, dictionary_offset)?;
        let widths = match widths_value.value() {
            SyntaxObject::Array(values) => values,
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, widths_value.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, widths_value.span().start())),
        };
        let expected_widths = u64::from(last_char) - u64::from(first_char) + 1;
        if expected_widths > self.limits.max_widths() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceWidths,
                self.limits.max_widths(),
                0,
                expected_widths,
                reference,
                Some(widths_value.span().start()),
            ));
        }
        if u64::try_from(widths.values().len()).ok() != Some(expected_widths) {
            return Err(invalid_font(reference, widths_value.span().start()));
        }
        let mut simple_widths = [0_u32; 256];
        for (index, width) in widths.values().iter().enumerate() {
            self.charge_width(reference, width.span().start())?;
            if self
                .stats
                .widths
                .is_multiple_of(METADATA_CANCELLATION_INTERVAL)
            {
                self.runtime_guard(source, cancellation, Some(width.span().start()))?;
            }
            let numeric = match width.value() {
                SyntaxObject::Integer(value) => u32::try_from(*value)
                    .map_err(|_| invalid_font(reference, width.span().start()))?,
                SyntaxObject::Real(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedWidths,
                        reference,
                        width.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, width.span().start())));
                }
                _ => return Err(invalid_font(reference, width.span().start())),
            };
            let code = usize::from(first_char)
                .checked_add(index)
                .ok_or_else(|| self.internal_error(Some(width.span().start())))?;
            let slot = simple_widths
                .get_mut(code)
                .ok_or_else(|| self.internal_error(Some(width.span().start())))?;
            *slot = numeric;
        }

        let descriptor_value = match slots.font_descriptor {
            Some(value) => value,
            None => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::MissingEmbeddedProgram,
                    reference,
                    dictionary_offset,
                )));
            }
        };
        let descriptor = match descriptor_value.value() {
            SyntaxObject::Reference(reference) => DescriptorPlan::Indirect(*reference),
            SyntaxObject::Dictionary(dictionary) => {
                match self.inspect_descriptor_dictionary(
                    dictionary,
                    reference,
                    descriptor_value.span().start(),
                    kind,
                    source,
                    cancellation,
                )? {
                    Ok(program) => DescriptorPlan::Program(program),
                    Err(value) => return Ok(Err(value)),
                }
            }
            SyntaxObject::Null => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::MissingEmbeddedProgram,
                    reference,
                    descriptor_value.span().start(),
                )));
            }
            _ => return Err(invalid_font(reference, descriptor_value.span().start())),
        };
        self.runtime_guard(source, cancellation, Some(descriptor_value.span().start()))?;
        Ok(Ok(FontInspection::Ready(PdfFontMetadata {
            kind,
            codes: PdfFontCodes::Simple {
                encoding: encoding_kind,
                pending_encoding,
                first_char,
                last_char,
                widths: simple_widths,
                type1_differences,
            },
            descriptor,
        })))
    }

    fn inspect_descendant_array(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ObjectRef, FontResourceUnsupported>, DocumentError> {
        let reference = object.reference();
        let value = match object.value() {
            IndirectObjectValue::Stream(stream) => {
                return Err(invalid_font(reference, stream.dictionary().span().start()));
            }
            IndirectObjectValue::Direct(value) => value,
        };
        let descendant = match value.value() {
            SyntaxObject::Array(values) => match values.values() {
                [value] => match value.value() {
                    SyntaxObject::Reference(reference) => *reference,
                    _ => return Err(invalid_font(reference, value.span().start())),
                },
                _ => return Err(invalid_font(reference, value.span().start())),
            },
            SyntaxObject::Reference(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::FontAlias,
                    reference,
                    value.span().start(),
                )));
            }
            _ => return Err(invalid_font(reference, value.span().start())),
        };
        self.runtime_guard(source, cancellation, Some(value.span().start()))?;
        Ok(Ok(descendant))
    }

    fn inspect_type1_encoding(
        &mut self,
        encoding: Option<&Located<SyntaxObject>>,
        reference: ObjectRef,
        dictionary_offset: u64,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<Type1EncodingPlan, FontResourceUnsupported>, DocumentError> {
        let Some(encoding) = encoding else {
            return Ok(Ok(Type1EncodingPlan::Ready(ParsedType1Encoding {
                encoding: SimpleEncoding::Type1Standard,
                differences: Vec::new(),
            })));
        };
        match encoding.value() {
            SyntaxObject::Name(name) if name.bytes() == b"StandardEncoding" => {
                Ok(Ok(Type1EncodingPlan::Ready(ParsedType1Encoding {
                    encoding: SimpleEncoding::Type1Standard,
                    differences: Vec::new(),
                })))
            }
            SyntaxObject::Name(name) if name.bytes() == b"WinAnsiEncoding" => {
                Ok(Ok(Type1EncodingPlan::Ready(ParsedType1Encoding {
                    encoding: SimpleEncoding::WinAnsi,
                    differences: Vec::new(),
                })))
            }
            SyntaxObject::Name(_) => Ok(Err(FontResourceUnsupported::new(
                FontResourceUnsupportedKind::UnsupportedEncoding,
                reference,
                encoding.span().start(),
            ))),
            SyntaxObject::Dictionary(dictionary) => self
                .inspect_type1_encoding_dictionary(
                    dictionary,
                    reference,
                    dictionary_offset,
                    source,
                    cancellation,
                )
                .map(|result| result.map(Type1EncodingPlan::Ready)),
            SyntaxObject::Reference(target) => Ok(Ok(Type1EncodingPlan::Indirect(*target))),
            _ => Err(invalid_font(reference, encoding.span().start())),
        }
    }

    fn inspect_descendant(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<PdfFontMetadata, FontResourceUnsupported>, DocumentError> {
        let reference = object.reference();
        let value = match object.value() {
            IndirectObjectValue::Stream(stream) => {
                return Err(invalid_font(reference, stream.dictionary().span().start()));
            }
            IndirectObjectValue::Direct(value) => value,
        };
        let dictionary = match value.value() {
            SyntaxObject::Dictionary(dictionary) => dictionary,
            SyntaxObject::Reference(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::FontAlias,
                    reference,
                    value.span().start(),
                )));
            }
            _ => return Err(invalid_font(reference, value.span().start())),
        };
        let dictionary_offset = value.span().start();
        let slots = self.scan_descendant_metadata(dictionary, reference, source, cancellation)?;
        if let Some(type_value) = slots.type_value {
            match type_value.value() {
                SyntaxObject::Name(name) if name.bytes() == b"Font" => {}
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, type_value.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, type_value.span().start())),
            }
        }
        let subtype = required_slot(slots.subtype, reference, dictionary_offset)?;
        match subtype.value() {
            SyntaxObject::Name(name) if name.bytes() == b"CIDFontType2" => {}
            SyntaxObject::Name(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::NonTrueType,
                    reference,
                    subtype.span().start(),
                )));
            }
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, subtype.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, subtype.span().start())),
        }
        let cid_to_gid = required_slot(slots.cid_to_gid_map, reference, dictionary_offset)?;
        match cid_to_gid.value() {
            SyntaxObject::Name(name) if name.bytes() == b"Identity" => {}
            SyntaxObject::Name(_) => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::UnsupportedEncoding,
                    reference,
                    cid_to_gid.span().start(),
                )));
            }
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, cid_to_gid.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, cid_to_gid.span().start())),
        }
        let default_width = match slots.default_width {
            None => 1_000,
            Some(value) => match value.value() {
                SyntaxObject::Integer(width) => u32::try_from(*width)
                    .map_err(|_| invalid_font(reference, value.span().start()))?,
                SyntaxObject::Real(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedWidths,
                        reference,
                        value.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, value.span().start())));
                }
                _ => return Err(invalid_font(reference, value.span().start())),
            },
        };
        let widths = match slots.widths {
            None => Vec::new(),
            Some(value) => match self.parse_cid_widths(value, reference, source, cancellation)? {
                Ok(widths) => widths,
                Err(unsupported) => return Ok(Err(unsupported)),
            },
        };
        let descriptor_value = required_slot(slots.font_descriptor, reference, dictionary_offset)?;
        let descriptor = match descriptor_value.value() {
            SyntaxObject::Reference(reference) => DescriptorPlan::Indirect(*reference),
            SyntaxObject::Dictionary(dictionary) => {
                match self.inspect_descriptor_dictionary(
                    dictionary,
                    reference,
                    descriptor_value.span().start(),
                    PdfFontKind::TrueType,
                    source,
                    cancellation,
                )? {
                    Ok(program) => DescriptorPlan::Program(program),
                    Err(value) => return Ok(Err(value)),
                }
            }
            SyntaxObject::Null => {
                return Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::MissingEmbeddedProgram,
                    reference,
                    descriptor_value.span().start(),
                )));
            }
            _ => return Err(invalid_font(reference, descriptor_value.span().start())),
        };
        self.runtime_guard(source, cancellation, Some(descriptor_value.span().start()))?;
        Ok(Ok(PdfFontMetadata {
            kind: PdfFontKind::TrueType,
            codes: PdfFontCodes::IdentityH {
                default_width,
                widths,
            },
            descriptor,
        }))
    }

    fn parse_cid_widths(
        &mut self,
        value: &Located<SyntaxObject>,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<Vec<CidWidth>, FontResourceUnsupported>, DocumentError> {
        let values = match value.value() {
            SyntaxObject::Array(values) => values.values(),
            SyntaxObject::Reference(_) => {
                return Ok(Err(self.indirect_metadata(reference, value.span().start())));
            }
            _ => return Err(invalid_font(reference, value.span().start())),
        };
        let mut parsed = Vec::new();
        let mut cursor = 0_usize;
        let mut previous = None;
        while cursor < values.len() {
            let start_value = values
                .get(cursor)
                .ok_or_else(|| invalid_font(reference, value.span().start()))?;
            let start = match start_value.value() {
                SyntaxObject::Integer(code) => u16::try_from(*code)
                    .map_err(|_| invalid_font(reference, start_value.span().start()))?,
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, start_value.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, start_value.span().start())),
            };
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| self.internal_error(Some(start_value.span().start())))?;
            let body = values
                .get(cursor)
                .ok_or_else(|| invalid_font(reference, start_value.span().start()))?;
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| self.internal_error(Some(body.span().start())))?;
            match body.value() {
                SyntaxObject::Array(widths) => {
                    for (index, width_value) in widths.values().iter().enumerate() {
                        let code = usize::from(start)
                            .checked_add(index)
                            .and_then(|value| u16::try_from(value).ok())
                            .ok_or_else(|| invalid_font(reference, width_value.span().start()))?;
                        let width = match width_value.value() {
                            SyntaxObject::Integer(width) => u32::try_from(*width)
                                .map_err(|_| invalid_font(reference, width_value.span().start()))?,
                            SyntaxObject::Real(_) => {
                                return Ok(Err(FontResourceUnsupported::new(
                                    FontResourceUnsupportedKind::UnsupportedWidths,
                                    reference,
                                    width_value.span().start(),
                                )));
                            }
                            SyntaxObject::Reference(_) => {
                                return Ok(Err(
                                    self.indirect_metadata(reference, width_value.span().start())
                                ));
                            }
                            _ => {
                                return Err(invalid_font(reference, width_value.span().start()));
                            }
                        };
                        self.push_cid_width(
                            &mut parsed,
                            &mut previous,
                            CidWidth { code, width },
                            reference,
                            width_value.span().start(),
                            source,
                            cancellation,
                        )?;
                    }
                }
                SyntaxObject::Integer(end) => {
                    let end = u16::try_from(*end)
                        .map_err(|_| invalid_font(reference, body.span().start()))?;
                    if end < start {
                        return Err(invalid_font(reference, body.span().start()));
                    }
                    let width_value = values
                        .get(cursor)
                        .ok_or_else(|| invalid_font(reference, body.span().start()))?;
                    cursor = cursor
                        .checked_add(1)
                        .ok_or_else(|| self.internal_error(Some(width_value.span().start())))?;
                    let width = match width_value.value() {
                        SyntaxObject::Integer(width) => u32::try_from(*width)
                            .map_err(|_| invalid_font(reference, width_value.span().start()))?,
                        SyntaxObject::Real(_) => {
                            return Ok(Err(FontResourceUnsupported::new(
                                FontResourceUnsupportedKind::UnsupportedWidths,
                                reference,
                                width_value.span().start(),
                            )));
                        }
                        SyntaxObject::Reference(_) => {
                            return Ok(Err(
                                self.indirect_metadata(reference, width_value.span().start())
                            ));
                        }
                        _ => return Err(invalid_font(reference, width_value.span().start())),
                    };
                    for code in start..=end {
                        self.push_cid_width(
                            &mut parsed,
                            &mut previous,
                            CidWidth { code, width },
                            reference,
                            width_value.span().start(),
                            source,
                            cancellation,
                        )?;
                    }
                }
                SyntaxObject::Real(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedWidths,
                        reference,
                        body.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, body.span().start())));
                }
                _ => return Err(invalid_font(reference, body.span().start())),
            }
        }
        Ok(Ok(parsed))
    }

    #[allow(clippy::too_many_arguments)]
    fn push_cid_width(
        &mut self,
        parsed: &mut Vec<CidWidth>,
        previous: &mut Option<u16>,
        entry: CidWidth,
        reference: ObjectRef,
        offset: u64,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<(), DocumentError> {
        if previous.is_some_and(|previous| entry.code <= previous) {
            return Err(invalid_font(reference, offset));
        }
        self.charge_width(reference, offset)?;
        if self
            .stats
            .widths
            .is_multiple_of(METADATA_CANCELLATION_INTERVAL)
        {
            self.runtime_guard(source, cancellation, Some(offset))?;
        }
        parsed.try_reserve_exact(1).map_err(|_| {
            DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                self.limits.max_retained_bytes(),
                u64::try_from(parsed.capacity())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(u64::try_from(size_of::<CidWidth>()).unwrap_or(u64::MAX)),
                u64::try_from(size_of::<CidWidth>()).unwrap_or(u64::MAX),
                reference,
                Some(offset),
            )
        })?;
        parsed.push(entry);
        *previous = Some(entry.code);
        Ok(())
    }

    fn inspect_type1_encoding_object(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ParsedType1Encoding, FontResourceUnsupported>, DocumentError> {
        let reference = object.reference();
        let value = match object.value() {
            IndirectObjectValue::Stream(stream) => {
                return Err(invalid_font(reference, stream.dictionary().span().start()));
            }
            IndirectObjectValue::Direct(value) => value,
        };
        match value.value() {
            SyntaxObject::Dictionary(dictionary) => self.inspect_type1_encoding_dictionary(
                dictionary,
                reference,
                value.span().start(),
                source,
                cancellation,
            ),
            SyntaxObject::Name(name) if name.bytes() == b"StandardEncoding" => {
                Ok(Ok(ParsedType1Encoding {
                    encoding: SimpleEncoding::Type1Standard,
                    differences: Vec::new(),
                }))
            }
            SyntaxObject::Name(name) if name.bytes() == b"WinAnsiEncoding" => {
                Ok(Ok(ParsedType1Encoding {
                    encoding: SimpleEncoding::WinAnsi,
                    differences: Vec::new(),
                }))
            }
            SyntaxObject::Reference(_) | SyntaxObject::Name(_) => {
                Ok(Err(FontResourceUnsupported::new(
                    FontResourceUnsupportedKind::UnsupportedEncoding,
                    reference,
                    value.span().start(),
                )))
            }
            _ => Err(invalid_font(reference, value.span().start())),
        }
    }

    fn inspect_type1_encoding_dictionary(
        &mut self,
        dictionary: &PdfDictionary,
        reference: ObjectRef,
        dictionary_offset: u64,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ParsedType1Encoding, FontResourceUnsupported>, DocumentError> {
        let mut parsed_differences = Vec::new();
        let mut base_encoding = None;
        let mut differences = None;
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            self.probe_metadata(source, cancellation, offset)?;
            let slot = match entry.key().value().bytes() {
                b"BaseEncoding" => Some(&mut base_encoding),
                b"Differences" => Some(&mut differences),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::DuplicateStructuralKey,
                        Some(reference),
                        Some(offset),
                    ));
                }
                *slot = Some(entry.value());
            }
        }
        let encoding = if let Some(base) = base_encoding {
            match base.value() {
                SyntaxObject::Name(name) if name.bytes() == b"StandardEncoding" => {
                    SimpleEncoding::Type1Standard
                }
                SyntaxObject::Name(name) if name.bytes() == b"WinAnsiEncoding" => {
                    SimpleEncoding::WinAnsi
                }
                SyntaxObject::Name(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedEncoding,
                        reference,
                        base.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, base.span().start())));
                }
                _ => return Err(invalid_font(reference, base.span().start())),
            }
        } else {
            SimpleEncoding::Type1Standard
        };
        let Some(differences) = differences else {
            self.runtime_guard(source, cancellation, Some(dictionary_offset))?;
            return Ok(Ok(ParsedType1Encoding {
                encoding,
                differences: parsed_differences,
            }));
        };
        let values = match differences.value() {
            SyntaxObject::Array(values) => values.values(),
            SyntaxObject::Reference(_) => {
                return Ok(Err(
                    self.indirect_metadata(reference, differences.span().start())
                ));
            }
            _ => return Err(invalid_font(reference, differences.span().start())),
        };
        let mut next_code = None;
        for value in values {
            self.charge_metadata(reference, value.span().start())?;
            self.probe_metadata(source, cancellation, value.span().start())?;
            match value.value() {
                SyntaxObject::Integer(code) => {
                    next_code = Some(
                        u8::try_from(*code)
                            .map_err(|_| invalid_font(reference, value.span().start()))?,
                    );
                }
                SyntaxObject::Name(name) => {
                    let code =
                        next_code.ok_or_else(|| invalid_font(reference, value.span().start()))?;
                    let name = std::str::from_utf8(name.bytes())
                        .map_err(|_| invalid_font(reference, value.span().start()))?;
                    if parsed_differences.try_reserve(1).is_err() {
                        let retained = self.retained_object_bytes()?;
                        return Err(DocumentError::font_resource(
                            DocumentLimitKind::FontResourceRetainedBytes,
                            self.limits.max_retained_bytes(),
                            retained,
                            u64::try_from(name.len()).unwrap_or(u64::MAX),
                            reference,
                            Some(value.span().start()),
                        ));
                    }
                    parsed_differences.push(Type1Difference {
                        code,
                        name: Box::from(name),
                    });
                    next_code = code.checked_add(1);
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, value.span().start())));
                }
                _ => return Err(invalid_font(reference, value.span().start())),
            }
        }
        self.runtime_guard(
            source,
            cancellation,
            values
                .last()
                .map_or(Some(dictionary_offset), |value| Some(value.span().start())),
        )?;
        Ok(Ok(ParsedType1Encoding {
            encoding,
            differences: parsed_differences,
        }))
    }

    fn inspect_descriptor(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ObjectRef, FontResourceUnsupported>, DocumentError> {
        let kind = self
            .metadata
            .as_ref()
            .ok_or_else(|| self.internal_error(Some(object.object_span().start())))?
            .kind;
        let reference = object.reference();
        let value = match object.value() {
            IndirectObjectValue::Stream(stream) => {
                return Err(invalid_font(reference, stream.dictionary().span().start()));
            }
            IndirectObjectValue::Direct(value) => value,
        };
        match value.value() {
            SyntaxObject::Dictionary(dictionary) => self.inspect_descriptor_dictionary(
                dictionary,
                reference,
                value.span().start(),
                kind,
                source,
                cancellation,
            ),
            SyntaxObject::Reference(_) => Ok(Err(FontResourceUnsupported::new(
                FontResourceUnsupportedKind::FontDescriptorAlias,
                reference,
                value.span().start(),
            ))),
            _ => Err(invalid_font(reference, value.span().start())),
        }
    }

    fn inspect_descriptor_dictionary(
        &mut self,
        dictionary: &PdfDictionary,
        reference: ObjectRef,
        offset: u64,
        kind: PdfFontKind,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ObjectRef, FontResourceUnsupported>, DocumentError> {
        let slots = self.scan_descriptor_metadata(dictionary, reference, source, cancellation)?;
        let mut program_declarations = 0_u8;
        let mut conflict_offset = offset;
        for value in [slots.font_file, slots.font_file2, slots.font_file3]
            .into_iter()
            .flatten()
        {
            program_declarations = program_declarations
                .checked_add(1)
                .ok_or_else(|| self.internal_error(Some(value.span().start())))?;
            conflict_offset = conflict_offset.max(value.span().start());
        }
        if program_declarations > 1 {
            return Err(invalid_font(reference, conflict_offset));
        }
        if let Some(type_value) = slots.type_value {
            match type_value.value() {
                SyntaxObject::Name(name) if name.bytes() == b"FontDescriptor" => {}
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, type_value.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, type_value.span().start())),
            }
        }
        let program = match kind {
            PdfFontKind::TrueType => slots.font_file2,
            PdfFontKind::Type1C => slots.font_file3,
        };
        let Some(program) = program else {
            let selected_offset = slots
                .font_file
                .or(slots.font_file3)
                .map_or(offset, |value| value.span().start());
            return Ok(Err(FontResourceUnsupported::new(
                FontResourceUnsupportedKind::MissingEmbeddedProgram,
                reference,
                selected_offset,
            )));
        };
        match program.value() {
            SyntaxObject::Reference(reference) => Ok(Ok(*reference)),
            SyntaxObject::Null => Ok(Err(FontResourceUnsupported::new(
                FontResourceUnsupportedKind::MissingEmbeddedProgram,
                reference,
                program.span().start(),
            ))),
            _ => Err(invalid_font(reference, program.span().start())),
        }
    }

    fn inspect_program(
        &mut self,
        object: &AttestedObject,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<Result<ProgramMetadata, FontResourceUnsupported>, DocumentError> {
        let reference = object.reference();
        let stream = match object.value() {
            IndirectObjectValue::Stream(stream) => stream,
            IndirectObjectValue::Direct(value) => {
                return match value.value() {
                    SyntaxObject::Reference(_) => Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::FontFileAlias,
                        reference,
                        value.span().start(),
                    ))),
                    _ => Err(invalid_font(reference, value.span().start())),
                };
            }
        };
        let slots = self.scan_program_metadata(
            stream.dictionary().value(),
            reference,
            source,
            cancellation,
        )?;
        let dictionary_offset = stream.dictionary().span().start();
        let kind = self
            .metadata
            .as_ref()
            .ok_or_else(|| self.internal_error(Some(dictionary_offset)))?
            .kind;
        if kind == PdfFontKind::Type1C {
            let subtype = required_slot(slots.subtype, reference, dictionary_offset)?;
            match subtype.value() {
                SyntaxObject::Name(name) if name.bytes() == b"Type1C" => {}
                SyntaxObject::Name(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedProgramSubtype,
                        reference,
                        subtype.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, subtype.span().start())
                    ));
                }
                _ => return Err(invalid_font(reference, subtype.span().start())),
            }
        }
        let decoded_bytes = match (kind, slots.length1) {
            (PdfFontKind::TrueType, None) => {
                return Err(invalid_font(reference, dictionary_offset));
            }
            (_, Some(length1)) => {
                let decoded_bytes = match length1.value() {
                    SyntaxObject::Integer(value) => u64::try_from(*value)
                        .ok()
                        .filter(|value| *value != 0)
                        .ok_or_else(|| invalid_font(reference, length1.span().start()))?,
                    SyntaxObject::Reference(_) => {
                        return Ok(Err(
                            self.indirect_metadata(reference, length1.span().start())
                        ));
                    }
                    _ => return Err(invalid_font(reference, length1.span().start())),
                };
                self.check_scalar_limit(
                    decoded_bytes,
                    self.limits.max_decoded_bytes(),
                    DocumentLimitKind::FontResourceDecodedBytes,
                    reference,
                    length1.span().start(),
                )?;
                if decoded_bytes > self.limits.font_limits().max_input_bytes() {
                    return Err(DocumentError::font_resource(
                        DocumentLimitKind::FontResourceDecodedBytes,
                        self.limits.font_limits().max_input_bytes(),
                        0,
                        decoded_bytes,
                        reference,
                        Some(length1.span().start()),
                    ));
                }
                Some(decoded_bytes)
            }
            (PdfFontKind::Type1C, None) => None,
        };
        let filter = match slots.filter {
            None => RegisteredFilter::Identity,
            Some(filter) => match filter.value() {
                SyntaxObject::Name(name) if name.bytes() == b"FlateDecode" => {
                    RegisteredFilter::Flate
                }
                SyntaxObject::Name(_) | SyntaxObject::Array(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedFilter,
                        reference,
                        filter.span().start(),
                    )));
                }
                SyntaxObject::Reference(_) => {
                    return Ok(Err(self.indirect_metadata(reference, filter.span().start())));
                }
                _ => return Err(invalid_font(reference, filter.span().start())),
            },
        };
        if let Some(parameters) = slots.decode_parameters {
            match parameters.value() {
                SyntaxObject::Null => {}
                SyntaxObject::Reference(_) => {
                    return Ok(Err(
                        self.indirect_metadata(reference, parameters.span().start())
                    ));
                }
                SyntaxObject::Dictionary(dictionary) if dictionary.entries().is_empty() => {}
                SyntaxObject::Dictionary(_) | SyntaxObject::Array(_) => {
                    return Ok(Err(FontResourceUnsupported::new(
                        FontResourceUnsupportedKind::UnsupportedDecodeParameters,
                        reference,
                        parameters.span().start(),
                    )));
                }
                _ => return Err(invalid_font(reference, parameters.span().start())),
            }
            if matches!(filter, RegisteredFilter::Identity)
                && !matches!(parameters.value(), SyntaxObject::Null)
            {
                return Err(invalid_font(reference, parameters.span().start()));
            }
        }
        self.runtime_guard(source, cancellation, Some(stream.data_span().start()))?;
        Ok(Ok(ProgramMetadata {
            kind,
            filter,
            decoded_bytes,
        }))
    }
}

impl AcquireFontResourceJob {
    fn scan_font_metadata<'a>(
        &mut self,
        dictionary: &'a PdfDictionary,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<FontSlots<'a>, DocumentError> {
        let mut slots = FontSlots::default();
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            self.probe_metadata(source, cancellation, offset)?;
            let slot = match entry.key().value().bytes() {
                b"Type" => Some(&mut slots.type_value),
                b"Subtype" => Some(&mut slots.subtype),
                b"Encoding" => Some(&mut slots.encoding),
                b"DescendantFonts" => Some(&mut slots.descendant_fonts),
                b"FirstChar" => Some(&mut slots.first_char),
                b"LastChar" => Some(&mut slots.last_char),
                b"Widths" => Some(&mut slots.widths),
                b"FontDescriptor" => Some(&mut slots.font_descriptor),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::DuplicateStructuralKey,
                        Some(reference),
                        Some(offset),
                    ));
                }
                *slot = Some(entry.value());
            }
        }
        self.runtime_guard(
            source,
            cancellation,
            dictionary
                .entries()
                .last()
                .map(|entry| entry.value().span().start()),
        )?;
        Ok(slots)
    }

    fn scan_descriptor_metadata<'a>(
        &mut self,
        dictionary: &'a PdfDictionary,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<DescriptorSlots<'a>, DocumentError> {
        let mut slots = DescriptorSlots::default();
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            self.probe_metadata(source, cancellation, offset)?;
            let slot = match entry.key().value().bytes() {
                b"Type" => Some(&mut slots.type_value),
                b"FontFile" => Some(&mut slots.font_file),
                b"FontFile2" => Some(&mut slots.font_file2),
                b"FontFile3" => Some(&mut slots.font_file3),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::DuplicateStructuralKey,
                        Some(reference),
                        Some(offset),
                    ));
                }
                *slot = Some(entry.value());
            }
        }
        self.runtime_guard(
            source,
            cancellation,
            dictionary
                .entries()
                .last()
                .map(|entry| entry.value().span().start()),
        )?;
        Ok(slots)
    }

    fn scan_descendant_metadata<'a>(
        &mut self,
        dictionary: &'a PdfDictionary,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<DescendantSlots<'a>, DocumentError> {
        let mut slots = DescendantSlots::default();
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            self.probe_metadata(source, cancellation, offset)?;
            let slot = match entry.key().value().bytes() {
                b"Type" => Some(&mut slots.type_value),
                b"Subtype" => Some(&mut slots.subtype),
                b"CIDToGIDMap" => Some(&mut slots.cid_to_gid_map),
                b"DW" => Some(&mut slots.default_width),
                b"W" => Some(&mut slots.widths),
                b"FontDescriptor" => Some(&mut slots.font_descriptor),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::DuplicateStructuralKey,
                        Some(reference),
                        Some(offset),
                    ));
                }
                *slot = Some(entry.value());
            }
        }
        self.runtime_guard(
            source,
            cancellation,
            dictionary
                .entries()
                .last()
                .map(|entry| entry.value().span().start()),
        )?;
        Ok(slots)
    }

    fn scan_program_metadata<'a>(
        &mut self,
        dictionary: &'a PdfDictionary,
        reference: ObjectRef,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> Result<ProgramSlots<'a>, DocumentError> {
        let mut slots = ProgramSlots::default();
        for entry in dictionary.entries() {
            let offset = entry.key().span().start();
            self.charge_metadata(reference, offset)?;
            self.probe_metadata(source, cancellation, offset)?;
            let slot = match entry.key().value().bytes() {
                b"Subtype" => Some(&mut slots.subtype),
                b"Length1" => Some(&mut slots.length1),
                b"Filter" => Some(&mut slots.filter),
                b"DecodeParms" => Some(&mut slots.decode_parameters),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_some() {
                    return Err(DocumentError::for_code(
                        DocumentErrorCode::DuplicateStructuralKey,
                        Some(reference),
                        Some(offset),
                    ));
                }
                *slot = Some(entry.value());
            }
        }
        self.runtime_guard(
            source,
            cancellation,
            dictionary
                .entries()
                .last()
                .map(|entry| entry.value().span().start()),
        )?;
        Ok(slots)
    }

    fn probe_metadata(
        &self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        offset: u64,
    ) -> Result<(), DocumentError> {
        if self.stats.metadata_entries != 0
            && self
                .stats
                .metadata_entries
                .is_multiple_of(METADATA_CANCELLATION_INTERVAL)
        {
            self.runtime_guard(source, cancellation, Some(offset))?;
        }
        Ok(())
    }

    fn charge_metadata(&mut self, reference: ObjectRef, offset: u64) -> Result<(), DocumentError> {
        if self.stats.metadata_entries >= self.limits.max_metadata_entries() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceMetadataEntries,
                self.limits.max_metadata_entries(),
                self.stats.metadata_entries,
                1,
                reference,
                Some(offset),
            ));
        }
        self.stats.metadata_entries = self
            .stats
            .metadata_entries
            .checked_add(1)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        Ok(())
    }

    fn charge_width(&mut self, reference: ObjectRef, offset: u64) -> Result<(), DocumentError> {
        if self.stats.widths >= self.limits.max_widths() {
            return Err(DocumentError::font_resource(
                DocumentLimitKind::FontResourceWidths,
                self.limits.max_widths(),
                self.stats.widths,
                1,
                reference,
                Some(offset),
            ));
        }
        self.stats.widths = self
            .stats
            .widths
            .checked_add(1)
            .ok_or_else(|| self.internal_error(Some(offset)))?;
        Ok(())
    }

    fn indirect_metadata(&self, reference: ObjectRef, offset: u64) -> FontResourceUnsupported {
        FontResourceUnsupported::new(
            FontResourceUnsupportedKind::IndirectMetadata,
            reference,
            offset,
        )
    }

    fn check_scalar_limit(
        &self,
        value: u64,
        limit: u64,
        kind: DocumentLimitKind,
        reference: ObjectRef,
        offset: u64,
    ) -> Result<(), DocumentError> {
        if value > limit {
            return Err(DocumentError::font_resource(
                kind,
                limit,
                0,
                value,
                reference,
                Some(offset),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct FontSlots<'a> {
    type_value: Option<&'a Located<SyntaxObject>>,
    subtype: Option<&'a Located<SyntaxObject>>,
    encoding: Option<&'a Located<SyntaxObject>>,
    descendant_fonts: Option<&'a Located<SyntaxObject>>,
    first_char: Option<&'a Located<SyntaxObject>>,
    last_char: Option<&'a Located<SyntaxObject>>,
    widths: Option<&'a Located<SyntaxObject>>,
    font_descriptor: Option<&'a Located<SyntaxObject>>,
}

#[derive(Default)]
struct DescriptorSlots<'a> {
    type_value: Option<&'a Located<SyntaxObject>>,
    font_file: Option<&'a Located<SyntaxObject>>,
    font_file2: Option<&'a Located<SyntaxObject>>,
    font_file3: Option<&'a Located<SyntaxObject>>,
}

#[derive(Default)]
struct DescendantSlots<'a> {
    type_value: Option<&'a Located<SyntaxObject>>,
    subtype: Option<&'a Located<SyntaxObject>>,
    cid_to_gid_map: Option<&'a Located<SyntaxObject>>,
    default_width: Option<&'a Located<SyntaxObject>>,
    widths: Option<&'a Located<SyntaxObject>>,
    font_descriptor: Option<&'a Located<SyntaxObject>>,
}

#[derive(Default)]
struct ProgramSlots<'a> {
    subtype: Option<&'a Located<SyntaxObject>>,
    length1: Option<&'a Located<SyntaxObject>>,
    filter: Option<&'a Located<SyntaxObject>>,
    decode_parameters: Option<&'a Located<SyntaxObject>>,
}

impl AcquireFontResourceJob {
    fn poll_payload(
        &mut self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
    ) -> PayloadResult {
        let (reference, dictionary_span, data_span) = {
            let Some(object) = self.program_object.as_ref() else {
                return PayloadResult::Failed(self.internal_error(None));
            };
            let IndirectObjectValue::Stream(stream) = object.value() else {
                return PayloadResult::Failed(self.internal_error(None));
            };
            (
                object.reference(),
                stream.dictionary().span(),
                stream.data_span(),
            )
        };
        let Some(metadata) = self.program_metadata else {
            return PayloadResult::Failed(self.internal_error(Some(dictionary_span.start())));
        };
        if data_span.is_empty() {
            return PayloadResult::Failed(invalid_font(reference, data_span.start()));
        }
        if let Err(error) = self.check_scalar_limit(
            data_span.len(),
            self.limits.max_encoded_bytes(),
            DocumentLimitKind::FontResourceEncodedBytes,
            reference,
            data_span.start(),
        ) {
            return PayloadResult::Failed(error);
        }
        let base_decode = self.limits.decode_limits();
        if data_span.len() > base_decode.max_input_bytes() {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceEncodedBytes,
                base_decode.max_input_bytes(),
                0,
                data_span.len(),
                reference,
                Some(data_span.start()),
            ));
        }
        let decoded_ceiling = self
            .limits
            .max_decoded_bytes()
            .min(self.limits.font_limits().max_input_bytes())
            .min(base_decode.max_layer_output_bytes())
            .min(base_decode.max_total_output_bytes())
            .min(base_decode.max_final_output_bytes());
        if let Some(decoded_bytes) = metadata.decoded_bytes
            && decoded_bytes > decoded_ceiling
        {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceDecodedBytes,
                decoded_ceiling,
                0,
                decoded_bytes,
                reference,
                Some(data_span.start()),
            ));
        }
        let range = match ByteRange::new(data_span.start(), data_span.len()) {
            Ok(range) => range,
            Err(_) => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        let request = ReadRequest::new(
            range,
            self.context.priority(),
            self.context.job(),
            self.context.payload_checkpoint(),
        );
        let read = source.poll(request);
        if let ReadPoll::Ready(bytes) = &read
            && bytes.identity() != self.snapshot.identity()
        {
            return PayloadResult::Failed(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(data_span.start()),
            ));
        }
        if let ReadPoll::Failed(error) = &read
            && error.category() == SourceErrorCategory::Integrity
        {
            return PayloadResult::Failed(DocumentError::from_source(*error, data_span.start()));
        }
        if let Err(error) = self.runtime_guard(source, cancellation, Some(data_span.start())) {
            return PayloadResult::Failed(error);
        }
        let encoded = match read {
            ReadPoll::Ready(bytes) => bytes,
            ReadPoll::Pending { ticket, missing } => {
                return PayloadResult::Pending { ticket, missing };
            }
            ReadPoll::EndOfFile => {
                return PayloadResult::Failed(DocumentError::for_code(
                    DocumentErrorCode::UnexpectedEndOfSource,
                    Some(reference),
                    Some(data_span.start()),
                ));
            }
            ReadPoll::Failed(error) => {
                return PayloadResult::Failed(DocumentError::from_source(error, data_span.start()));
            }
        };
        if encoded.range().start() != data_span.start() || encoded.range().len() != data_span.len()
        {
            return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
        }
        self.stats.encoded_bytes = data_span.len();

        let plan = match metadata.filter {
            RegisteredFilter::Identity => FilterPlan::new(&[]),
            RegisteredFilter::Flate => FilterPlan::new(&[StreamFilter::FlateDecode]),
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                return PayloadResult::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    None,
                ));
            }
        };
        let plan_retained = match plan.retained_heap_bytes() {
            Ok(value) => value,
            Err(error) => {
                return PayloadResult::Failed(self.map_decode_error(
                    error,
                    reference,
                    dictionary_span.start(),
                    None,
                ));
            }
        };
        let object_retained = match self.retained_object_bytes() {
            Ok(value) => value,
            Err(error) => return PayloadResult::Failed(error),
        };
        let parser_reserve = self.limits.font_limits().max_retained_bytes();
        let retained_prefix = match object_retained
            .checked_add(plan_retained)
            .and_then(|value| value.checked_add(parser_reserve))
        {
            Some(value) => value,
            None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        let decoder_retained = match self
            .limits
            .max_retained_bytes()
            .checked_sub(retained_prefix)
        {
            Some(value) => value.min(base_decode.max_retained_capacity_bytes()),
            None => {
                return PayloadResult::Failed(DocumentError::font_resource(
                    DocumentLimitKind::FontResourceRetainedBytes,
                    self.limits.max_retained_bytes(),
                    object_retained,
                    plan_retained.saturating_add(parser_reserve),
                    reference,
                    Some(data_span.start()),
                ));
            }
        };
        if decoder_retained < decoded_ceiling {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                retained_prefix.saturating_add(decoder_retained),
                retained_prefix,
                decoded_ceiling,
                reference,
                Some(data_span.start()),
            ));
        }
        let fuel = self.limits.max_decode_fuel().min(base_decode.max_fuel());
        let decode_limits = match DecodeLimits::validate(DecodeLimitConfig {
            max_input_bytes: data_span.len(),
            max_filters: 1,
            max_layer_output_bytes: decoded_ceiling,
            // The decoder's capacity guard subtracts committed output from this cumulative
            // ceiling. Retain the validated lower cumulative budget so a final output above its
            // initial allocation chunk can grow, while the layer/final ceilings still bind the
            // registered identity or Flate layer to the approved decoded-program budget.
            max_total_output_bytes: base_decode.max_total_output_bytes(),
            max_final_output_bytes: decoded_ceiling,
            max_retained_capacity_bytes: decoder_retained,
            max_fuel: fuel,
            cancellation_check_interval_fuel: base_decode
                .cancellation_check_interval_fuel()
                .min(fuel),
        }) {
            Ok(limits) => limits,
            Err(_) => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        let admitted_peak =
            match retained_prefix.checked_add(metadata.decoded_bytes.unwrap_or(decoded_ceiling)) {
                Some(value) => value,
                None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
            };
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(admitted_peak);
        let request = match DecodeRequest::new(
            self.snapshot,
            reference,
            dictionary_span,
            data_span,
            encoded,
            plan,
            DecodeProfile::M1StrictV1,
            decode_limits,
        ) {
            Ok(request) => request,
            Err(error) => {
                return PayloadResult::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    Some(retained_prefix),
                ));
            }
        };
        let decoded = match decode_stream(request, &DecodeCancellationAdapter(cancellation)) {
            Ok(decoded) => decoded,
            Err(error) if error.category() == DecodeErrorCategory::Unsupported => {
                return PayloadResult::Unsupported(FontResourceUnsupported::new(
                    match error.code() {
                        DecodeErrorCode::UnsupportedDecodeParameters
                        | DecodeErrorCode::UnsupportedPredictor => {
                            FontResourceUnsupportedKind::UnsupportedDecodeParameters
                        }
                        _ => FontResourceUnsupportedKind::UnsupportedFilter,
                    },
                    reference,
                    data_span.start(),
                ));
            }
            Err(error) => {
                return PayloadResult::Failed(self.map_decode_error(
                    error,
                    reference,
                    data_span.start(),
                    Some(retained_prefix),
                ));
            }
        };
        if let Err(error) = self.runtime_guard(source, cancellation, Some(data_span.start())) {
            return PayloadResult::Failed(error);
        }
        self.stats.decoded_bytes = decoded.len();
        self.stats.decode_fuel = decoded.attestation().fuel_consumed();

        let decode_plan_prefix =
            match object_retained.checked_add(decoded.attestation().plan_retained_heap_bytes()) {
                Some(value) => value,
                None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
            };
        let font_retained_prefix = match decode_plan_prefix
            .checked_add(decoded.attestation().peak_retained_capacity_bytes())
        {
            Some(value) => value,
            None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(font_retained_prefix);
        if font_retained_prefix > self.limits.max_retained_bytes() {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                self.limits.max_retained_bytes(),
                decode_plan_prefix,
                decoded.attestation().peak_retained_capacity_bytes(),
                reference,
                Some(data_span.start()),
            ));
        }
        if metadata
            .decoded_bytes
            .is_some_and(|decoded_bytes| decoded.len() != decoded_bytes)
        {
            return PayloadResult::Failed(invalid_font(reference, data_span.start()));
        }

        let font = match metadata.kind {
            PdfFontKind::TrueType => {
                let profile = match self.metadata.as_ref().map(|metadata| &metadata.codes) {
                    Some(PdfFontCodes::Simple { .. }) => FontProfile::SimpleTrueTypeWinAnsiV1,
                    Some(PdfFontCodes::IdentityH { .. }) => FontProfile::CidFontType2IdentityV1,
                    None => {
                        return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
                    }
                };
                let report = parse_truetype(
                    decoded.bytes(),
                    profile,
                    self.limits.font_limits(),
                    &FontCancellationAdapter(cancellation),
                );
                self.stats.font = report.stats();
                match report.into_outcome() {
                    FontParseOutcome::Ready(font) => FontProgram::from(font),
                    FontParseOutcome::Unsupported(value) => {
                        return PayloadResult::Unsupported(FontResourceUnsupported::from_font(
                            metadata.kind,
                            reference,
                            data_span.start(),
                            value,
                        ));
                    }
                    FontParseOutcome::Cancelled(_) => {
                        return PayloadResult::Failed(DocumentError::for_code(
                            DocumentErrorCode::Cancelled,
                            Some(reference),
                            Some(data_span.start()),
                        ));
                    }
                    FontParseOutcome::Failed(error) => {
                        return PayloadResult::Failed(self.map_font_error(
                            error,
                            reference,
                            data_span.start(),
                            font_retained_prefix,
                        ));
                    }
                }
            }
            PdfFontKind::Type1C => {
                let report = parse_cff(
                    decoded.bytes(),
                    FontProfile::SimpleType1CStandardV1,
                    self.limits.font_limits(),
                    &FontCancellationAdapter(cancellation),
                );
                self.stats.font = report.stats();
                match report.into_outcome() {
                    CffParseOutcome::Ready(font) => FontProgram::from(font),
                    CffParseOutcome::Unsupported(value) => {
                        return PayloadResult::Unsupported(FontResourceUnsupported::from_font(
                            metadata.kind,
                            reference,
                            data_span.start(),
                            value,
                        ));
                    }
                    CffParseOutcome::Cancelled(_) => {
                        return PayloadResult::Failed(DocumentError::for_code(
                            DocumentErrorCode::Cancelled,
                            Some(reference),
                            Some(data_span.start()),
                        ));
                    }
                    CffParseOutcome::Failed(error) => {
                        return PayloadResult::Failed(self.map_font_error(
                            error,
                            reference,
                            data_span.start(),
                            font_retained_prefix,
                        ));
                    }
                }
            }
        };
        if let Err(error) = self.runtime_guard(source, cancellation, Some(data_span.start())) {
            return PayloadResult::Failed(error);
        }
        let actual_retained = match font_retained_prefix.checked_add(font.stats().retained_bytes())
        {
            Some(value) => value,
            None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        if actual_retained > self.limits.max_retained_bytes() {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                self.limits.max_retained_bytes(),
                0,
                actual_retained,
                reference,
                Some(data_span.start()),
            ));
        }
        let actual_peak = match font_retained_prefix.checked_add(font.stats().peak_retained_bytes())
        {
            Some(value) => value,
            None => return PayloadResult::Failed(self.internal_error(Some(data_span.start()))),
        };
        if actual_peak > self.limits.max_retained_bytes() {
            return PayloadResult::Failed(DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                self.limits.max_retained_bytes(),
                0,
                actual_peak,
                reference,
                Some(data_span.start()),
            ));
        }
        self.stats.retained_bytes = actual_retained;
        self.stats.peak_retained_bytes = self.stats.peak_retained_bytes.max(actual_peak);
        let Some(metadata) = self.metadata.take() else {
            return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
        };
        let (Some(font_object), Some(program_object)) =
            (self.font_object.take(), self.program_object.take())
        else {
            return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
        };
        let codes = match metadata.codes {
            PdfFontCodes::Simple {
                encoding,
                pending_encoding,
                first_char,
                last_char,
                widths,
                type1_differences,
            } => {
                if pending_encoding.is_some() {
                    return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
                }
                let mut glyph_ids = [GlyphId::new(0); 256];
                match &font {
                    FontProgram::TrueType(_) => {
                        for code in 0_u8..=u8::MAX {
                            if let Some(glyph_id) = font.glyph_id_for_winansi(code) {
                                glyph_ids[usize::from(code)] = glyph_id;
                            }
                        }
                    }
                    FontProgram::Type1C(cff) => {
                        for code in 0_u8..=u8::MAX {
                            let glyph_id = match encoding {
                                SimpleEncoding::WinAnsi => cff.glyph_id_for_winansi_code(code),
                                SimpleEncoding::Type1Standard => {
                                    cff.glyph_id_for_standard_code(code)
                                }
                            };
                            if let Some(glyph_id) = glyph_id {
                                glyph_ids[usize::from(code)] = glyph_id;
                            }
                        }
                        for difference in &type1_differences {
                            if let Some(glyph_id) = cff.glyph_id_for_name(&difference.name) {
                                glyph_ids[usize::from(difference.code)] = glyph_id;
                            }
                        }
                    }
                }
                AcquiredFontCodes::Simple {
                    first_char,
                    last_char,
                    widths,
                    glyph_ids,
                }
            }
            PdfFontCodes::IdentityH {
                default_width,
                widths,
            } => {
                if !matches!(&font, FontProgram::TrueType(_)) {
                    return PayloadResult::Failed(self.internal_error(Some(data_span.start())));
                }
                AcquiredFontCodes::IdentityH {
                    default_width,
                    widths: widths.into_boxed_slice(),
                }
            }
        };
        let acquired = AcquiredFontResource {
            proof: self.proof,
            font_object,
            descendant_array_object: self.descendant_array_object.take(),
            descendant_object: self.descendant_object.take(),
            encoding_object: self.encoding_object.take(),
            descriptor_object: self.descriptor_object.take(),
            program_object,
            codes,
            decoded_program: decoded,
            font,
            limits: self.limits,
            stats: self.stats,
        };
        PayloadResult::Ready(Arc::new(acquired))
    }

    fn map_decode_error(
        &self,
        error: DecodeError,
        reference: ObjectRef,
        offset: u64,
        retained_prefix: Option<u64>,
    ) -> DocumentError {
        if let Some(limit) = error.limit() {
            let (kind, prefix) = match limit.kind() {
                DecodeLimitKind::InputBytes => (DocumentLimitKind::FontResourceEncodedBytes, 0),
                DecodeLimitKind::LayerOutputBytes
                | DecodeLimitKind::TotalOutputBytes
                | DecodeLimitKind::FinalOutputBytes => {
                    (DocumentLimitKind::FontResourceDecodedBytes, 0)
                }
                DecodeLimitKind::Fuel => (DocumentLimitKind::FontResourceDecodeFuel, 0),
                DecodeLimitKind::RetainedCapacityBytes
                | DecodeLimitKind::FilterPlanBytes
                | DecodeLimitKind::Allocation => (
                    DocumentLimitKind::FontResourceRetainedBytes,
                    retained_prefix.unwrap_or(0),
                ),
                DecodeLimitKind::FilterCount => return self.internal_error(Some(offset)),
            };
            let Some(mapped_limit) = prefix.checked_add(limit.limit()) else {
                return self.internal_error(Some(offset));
            };
            let Some(mapped_consumed) = prefix.checked_add(limit.consumed()) else {
                return self.internal_error(Some(offset));
            };
            return DocumentError::font_resource(
                kind,
                mapped_limit,
                mapped_consumed,
                limit.attempted(),
                reference,
                Some(offset),
            );
        }
        match error.code() {
            DecodeErrorCode::SourceChanged => DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(reference),
                Some(offset),
            ),
            DecodeErrorCode::Cancelled => {
                DocumentError::for_code(DocumentErrorCode::Cancelled, Some(reference), Some(offset))
            }
            DecodeErrorCode::InvalidLimits
            | DecodeErrorCode::InvalidRequest
            | DecodeErrorCode::InternalState => self.internal_error(Some(offset)),
            _ => DocumentError::for_code(
                DocumentErrorCode::FontResourceDecodeFailure,
                Some(reference),
                Some(offset),
            ),
        }
    }

    fn map_font_error(
        &self,
        error: FontError,
        reference: ObjectRef,
        offset: u64,
        retained_prefix: u64,
    ) -> DocumentError {
        if let Some(limit) = error.limit() {
            let kind = match limit.kind() {
                FontLimitKind::InputBytes => DocumentLimitKind::FontResourceDecodedBytes,
                FontLimitKind::Tables => DocumentLimitKind::FontResourceTables,
                FontLimitKind::Glyphs => DocumentLimitKind::FontResourceGlyphs,
                FontLimitKind::CmapSegments => DocumentLimitKind::FontResourceCmapSegments,
                FontLimitKind::GlyphDataBytes => DocumentLimitKind::FontResourceGlyphDataBytes,
                FontLimitKind::GlyphBytes => DocumentLimitKind::FontResourceGlyphBytes,
                FontLimitKind::GlyphContours => DocumentLimitKind::FontResourceGlyphContours,
                FontLimitKind::TotalContours => DocumentLimitKind::FontResourceTotalContours,
                FontLimitKind::GlyphPoints => DocumentLimitKind::FontResourceGlyphPoints,
                FontLimitKind::TotalPoints => DocumentLimitKind::FontResourceTotalPoints,
                FontLimitKind::Components => DocumentLimitKind::FontResourceComponents,
                FontLimitKind::ComponentDepth => DocumentLimitKind::FontResourceComponentDepth,
                FontLimitKind::PathSegments => DocumentLimitKind::FontResourcePathSegments,
                FontLimitKind::RetainedBytes | FontLimitKind::Allocation => {
                    DocumentLimitKind::FontResourceRetainedBytes
                }
                FontLimitKind::Fuel => DocumentLimitKind::FontResourceParserWork,
            };
            let (mapped_limit, mapped_consumed) = match limit.kind() {
                FontLimitKind::RetainedBytes | FontLimitKind::Allocation => {
                    let Some(mapped_limit) = retained_prefix.checked_add(limit.limit()) else {
                        return self.internal_error(Some(offset));
                    };
                    let Some(mapped_consumed) = retained_prefix.checked_add(limit.consumed())
                    else {
                        return self.internal_error(Some(offset));
                    };
                    (mapped_limit, mapped_consumed)
                }
                _ => (limit.limit(), limit.consumed()),
            };
            return DocumentError::font_resource(
                kind,
                mapped_limit,
                mapped_consumed,
                limit.attempted(),
                reference,
                Some(offset),
            );
        }
        match error.category() {
            FontErrorCategory::Cancellation => {
                DocumentError::for_code(DocumentErrorCode::Cancelled, Some(reference), Some(offset))
            }
            FontErrorCategory::Internal => self.internal_error(Some(offset)),
            FontErrorCategory::Resource => DocumentError::font_resource(
                DocumentLimitKind::FontResourceParserWork,
                self.limits.font_limits().max_fuel(),
                self.stats.font.fuel(),
                1,
                reference,
                Some(offset),
            ),
            FontErrorCategory::Configuration | FontErrorCategory::Syntax => {
                DocumentError::for_code(
                    DocumentErrorCode::FontProgramFailure,
                    Some(reference),
                    Some(offset),
                )
            }
        }
    }
}

impl AcquireFontResourceJob {
    fn map_child_error(&self, error: DocumentError) -> DocumentError {
        let Some(lower) = error.object_error() else {
            return error;
        };
        let reference = error.reference().unwrap_or(self.proof.target());
        let offset = error.offset().or_else(|| self.current_offset());
        if lower.code() == ObjectErrorCode::SyntaxFailure
            && let Some(syntax_limit) = lower.syntax_error().and_then(|value| value.limit())
            && syntax_limit.kind() == SyntaxLimitKind::RetainedBytes
            && self
                .child
                .as_ref()
                .and_then(|child| child.work_caps.max_retained_bytes())
                .is_some_and(|cap| {
                    let syntax = self.authority.as_attested().syntax_limits();
                    syntax
                        .max_owned_bytes()
                        .checked_add(syntax.max_container_bytes())
                        .is_some_and(|intrinsic| cap < intrinsic)
                })
        {
            let retained_prefix = match self.retained_object_bytes() {
                Ok(value) => value,
                Err(error) => return error,
            };
            let Some(consumed) = retained_prefix.checked_add(syntax_limit.consumed()) else {
                return self.internal_error(offset);
            };
            return DocumentError::font_resource(
                DocumentLimitKind::FontResourceRetainedBytes,
                self.limits.max_retained_bytes(),
                consumed,
                syntax_limit.attempted(),
                reference,
                offset,
            );
        }
        let Some(limit) = lower.limit() else {
            return error;
        };
        match limit.kind() {
            ObjectLimitKind::TotalReadBytes
                if self.child.as_ref().is_some_and(|child| {
                    child.work_caps.max_read_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_read_bytes()
                }) =>
            {
                let Some(consumed) = self
                    .child
                    .as_ref()
                    .and_then(|child| child.base_read_bytes.checked_add(limit.consumed()))
                else {
                    return self.internal_error(offset);
                };
                DocumentError::font_resource(
                    DocumentLimitKind::FontResourceObjectReadBytes,
                    self.limits.max_object_read_bytes(),
                    consumed,
                    limit.attempted(),
                    reference,
                    offset,
                )
            }
            ObjectLimitKind::TotalParseBytes
                if self.child.as_ref().is_some_and(|child| {
                    child.work_caps.max_parse_bytes()
                        < self
                            .authority
                            .as_attested()
                            .object_limits()
                            .max_total_parse_bytes()
                }) =>
            {
                let Some(consumed) = self
                    .child
                    .as_ref()
                    .and_then(|child| child.base_parse_bytes.checked_add(limit.consumed()))
                else {
                    return self.internal_error(offset);
                };
                DocumentError::font_resource(
                    DocumentLimitKind::FontResourceObjectParseBytes,
                    self.limits.max_object_parse_bytes(),
                    consumed,
                    limit.attempted(),
                    reference,
                    offset,
                )
            }
            _ => error,
        }
    }

    fn runtime_guard(
        &self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        offset: Option<u64>,
    ) -> Result<(), DocumentError> {
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
            ));
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return Err(DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
            ));
        }
        if cancelled {
            return Err(DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                Some(self.proof.target()),
                offset.or_else(|| self.current_offset()),
            ));
        }
        Ok(())
    }

    fn prioritize_runtime_error(
        &self,
        source: &dyn ByteSource,
        cancellation: &dyn DocumentCancellation,
        fallback: DocumentError,
    ) -> DocumentError {
        if fallback.code() == DocumentErrorCode::SourceSnapshotMismatch {
            return fallback;
        }
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                fallback.reference().or(Some(self.proof.target())),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        if fallback.code() == DocumentErrorCode::Cancelled {
            return fallback;
        }
        let cancelled = cancellation.is_cancelled();
        if source.snapshot() != self.snapshot {
            return DocumentError::for_code(
                DocumentErrorCode::SourceSnapshotMismatch,
                fallback.reference().or(Some(self.proof.target())),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        if cancelled {
            return DocumentError::for_code(
                DocumentErrorCode::Cancelled,
                fallback.reference().or(Some(self.proof.target())),
                fallback.offset().or_else(|| self.current_offset()),
            );
        }
        fallback
    }

    fn current_offset(&self) -> Option<u64> {
        self.authority
            .as_attested()
            .attestation(self.proof.target())
            .ok()
            .map(crate::ObjectAttestation::xref_offset)
    }

    fn internal_error(&self, offset: Option<u64>) -> DocumentError {
        DocumentError::for_code(
            DocumentErrorCode::InternalState,
            Some(self.proof.target()),
            offset.or_else(|| self.current_offset()),
        )
    }

    fn ready(&mut self, font: Arc<AcquiredFontResource>) -> FontResourcePoll {
        self.state = FontJobState::Ready(Arc::clone(&font));
        FontResourcePoll::Ready(font)
    }

    fn unsupported(&mut self, value: FontResourceUnsupported) -> FontResourcePoll {
        self.state = FontJobState::Unsupported(value);
        FontResourcePoll::Unsupported(value)
    }

    fn fail(&mut self, error: DocumentError) -> FontResourcePoll {
        self.state = FontJobState::Failed(error);
        FontResourcePoll::Failed(error)
    }
}

impl fmt::Debug for AcquireFontResourceJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcquireFontResourceJob")
            .field("snapshot", &self.snapshot)
            .field("proof", &self.proof)
            .field("context", &self.context)
            .field("limits", &self.limits)
            .field("phase", &self.phase())
            .field("stats", &self.stats)
            .field("objects", &"[REDACTED]")
            .finish()
    }
}

impl SharedAttestedRevisionIndex {
    /// Acquires one Page-selected embedded simple font under this shared strict proof.
    pub fn acquire_font_resource(
        &self,
        proof: PageFontReference,
        context: FontResourceJobContext,
        limits: FontResourceLimits,
    ) -> Result<AcquireFontResourceJob, DocumentError> {
        let authority = self.as_attested();
        let target = proof.target();
        let attestation = authority.attestation(target)?;
        let offset = attestation.xref_offset();
        if proof.snapshot() != authority.snapshot()
            || proof.revision_id() != authority.revision_id()
            || proof.revision_startxref() != authority.startxref()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::AttestedObjectEvidenceMismatch,
                Some(target),
                Some(offset),
            ));
        }
        let checkpoints = context.checkpoints();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            if checkpoints[index + 1..].contains(checkpoint) {
                return Err(DocumentError::for_code(
                    DocumentErrorCode::InvalidFontResourceJobContext,
                    Some(target),
                    Some(offset),
                ));
            }
        }
        let syntax = authority.syntax_limits();
        let intrinsic_retained = syntax
            .max_owned_bytes()
            .checked_add(syntax.max_container_bytes())
            .ok_or_else(|| {
                DocumentError::for_code(
                    DocumentErrorCode::InternalState,
                    Some(target),
                    Some(offset),
                )
            })?;
        let work_caps = ObjectWorkCaps::new_with_retained_bytes(
            limits
                .max_object_read_bytes()
                .min(authority.object_limits().max_total_read_bytes()),
            limits
                .max_object_parse_bytes()
                .min(authority.object_limits().max_total_parse_bytes()),
            limits.max_retained_bytes().min(intrinsic_retained),
        )
        .map_err(|error| DocumentError::from_object_access_constructor(error, target, offset))?;
        let object_context = AttestedObjectJobContext::new(
            context.job(),
            context.font_envelope_checkpoint(),
            context.font_boundary_checkpoint(),
            context.priority(),
        );
        let job = authority.open_object(target, object_context, work_caps)?;
        let stats = FontResourceStats {
            objects: 1,
            ..FontResourceStats::default()
        };
        Ok(AcquireFontResourceJob {
            authority: self.clone(),
            snapshot: authority.snapshot(),
            proof,
            context,
            limits,
            child: Some(ChildState {
                kind: ChildKind::Font,
                job,
                work_caps,
                base_read_bytes: 0,
                base_parse_bytes: 0,
            }),
            font_object: None,
            descendant_array_object: None,
            descendant_object: None,
            encoding_object: None,
            descriptor_object: None,
            program_object: None,
            metadata: None,
            program_metadata: None,
            stats,
            state: FontJobState::Active,
        })
    }
}

fn required_slot(
    slot: Option<&Located<SyntaxObject>>,
    reference: ObjectRef,
    offset: u64,
) -> Result<&Located<SyntaxObject>, DocumentError> {
    slot.ok_or_else(|| invalid_font(reference, offset))
}

fn invalid_font(reference: ObjectRef, offset: u64) -> DocumentError {
    DocumentError::for_code(
        DocumentErrorCode::InvalidFontResource,
        Some(reference),
        Some(offset),
    )
}
