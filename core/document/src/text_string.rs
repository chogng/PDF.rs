use std::error::Error;
use std::fmt;

use pdf_rs_syntax::PdfString;

use crate::DocumentCancellation;

const CANCELLATION_PROBE_INTERVAL: usize = 256;
const HARD_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_UTF8_BYTES: u64 = 64 * 1024 * 1024;

/// Unvalidated deterministic limits for decoding one PDF text string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextStringLimitConfig {
    /// Maximum decoded PDF string bytes accepted as input.
    pub max_input_bytes: u64,
    /// Maximum logical and allocator-retained UTF-8 bytes in the result.
    pub max_utf8_bytes: u64,
}

impl Default for TextStringLimitConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024,
            max_utf8_bytes: 1024 * 1024,
        }
    }
}

/// Validated deterministic limits for decoding one PDF text string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextStringLimits {
    max_input_bytes: u64,
    max_utf8_bytes: u64,
}

impl TextStringLimits {
    /// Validates nonzero limits against fixed implementation hard ceilings.
    pub fn validate(config: TextStringLimitConfig) -> Result<Self, TextStringError> {
        if config.max_input_bytes == 0
            || config.max_input_bytes > HARD_MAX_INPUT_BYTES
            || config.max_utf8_bytes == 0
            || config.max_utf8_bytes > HARD_MAX_UTF8_BYTES
        {
            return Err(TextStringError::for_code(
                TextStringErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_input_bytes: config.max_input_bytes,
            max_utf8_bytes: config.max_utf8_bytes,
        })
    }

    /// Returns the accepted decoded PDF string byte ceiling.
    pub const fn max_input_bytes(self) -> u64 {
        self.max_input_bytes
    }

    /// Returns the logical and allocator-retained UTF-8 byte ceiling.
    pub const fn max_utf8_bytes(self) -> u64 {
        self.max_utf8_bytes
    }
}

impl Default for TextStringLimits {
    fn default() -> Self {
        Self::validate(TextStringLimitConfig::default())
            .expect("built-in text-string limits satisfy hard ceilings")
    }
}

/// Encoding selected by the normative PDF text-string lead-byte rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStringEncoding {
    /// Single-byte PDFDocEncoding from ISO 32000-1:2008, Annex D.3.
    PdfDocEncoding,
    /// UTF-16 big-endian following the required `FE FF` byte-order marker.
    Utf16Be,
}

/// Deterministic text-string budget dimension that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStringLimitKind {
    /// Decoded bytes retained by the lexical PDF string value.
    InputBytes,
    /// Logical or allocator-retained UTF-8 result bytes.
    Utf8Bytes,
}

/// Structured text-string resource-limit context without string content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextStringLimit {
    kind: TextStringLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl TextStringLimit {
    const fn new(kind: TextStringLimitKind, limit: u64, consumed: u64, attempted: u64) -> Self {
        Self {
            kind,
            limit,
            consumed,
            attempted,
        }
    }

    /// Returns the rejected text-string budget dimension.
    pub const fn kind(self) -> TextStringLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount the rejected operation would add or require.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable text-string failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStringErrorCode {
    /// A deterministic limit is zero or above its fixed hard ceiling.
    InvalidLimits,
    /// Input, output, or fallible output allocation exceeded a bounded profile.
    ResourceLimit,
    /// The owning document runtime cancelled decoding at a bounded probe.
    Cancelled,
    /// A byte has no character assignment in PDFDocEncoding.
    UndefinedPdfDocEncoding,
    /// A BOM-selected UTF-16BE string has odd bytes or invalid surrogate structure.
    InvalidUtf16,
}

/// Coarse text-string failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStringErrorCategory {
    /// Invalid deterministic configuration.
    Configuration,
    /// Deterministic input, output, or allocation exhaustion.
    Resource,
    /// Malformed or undefined text-string data.
    Syntax,
    /// Normal cooperative cancellation.
    Cancellation,
}

/// Stable recovery policy for a text-string failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextStringRecoverability {
    /// Correct the deterministic limit profile before retrying.
    CorrectConfiguration,
    /// Reduce input or select an approved larger deterministic budget.
    ReduceWorkload,
    /// Correct the PDF bytes or select an explicitly approved recovery profile.
    CorrectInput,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
}

/// Content-redacted PDF text-string decoding error.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct TextStringError {
    code: TextStringErrorCode,
    category: TextStringErrorCategory,
    recoverability: TextStringRecoverability,
    diagnostic_id: &'static str,
    byte_offset: Option<u64>,
    limit: Option<TextStringLimit>,
}

impl TextStringError {
    const fn for_code(code: TextStringErrorCode, byte_offset: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            TextStringErrorCode::InvalidLimits => (
                TextStringErrorCategory::Configuration,
                TextStringRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-TEXT-0001",
            ),
            TextStringErrorCode::ResourceLimit => (
                TextStringErrorCategory::Resource,
                TextStringRecoverability::ReduceWorkload,
                "RPE-DOCUMENT-TEXT-0002",
            ),
            TextStringErrorCode::Cancelled => (
                TextStringErrorCategory::Cancellation,
                TextStringRecoverability::AbandonOperation,
                "RPE-DOCUMENT-TEXT-0003",
            ),
            TextStringErrorCode::UndefinedPdfDocEncoding => (
                TextStringErrorCategory::Syntax,
                TextStringRecoverability::CorrectInput,
                "RPE-DOCUMENT-TEXT-0004",
            ),
            TextStringErrorCode::InvalidUtf16 => (
                TextStringErrorCategory::Syntax,
                TextStringRecoverability::CorrectInput,
                "RPE-DOCUMENT-TEXT-0005",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            byte_offset,
            limit: None,
        }
    }

    const fn resource(
        kind: TextStringLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        byte_offset: Option<u64>,
    ) -> Self {
        Self {
            limit: Some(TextStringLimit::new(kind, limit, consumed, attempted)),
            ..Self::for_code(TextStringErrorCode::ResourceLimit, byte_offset)
        }
    }

    /// Returns the machine-readable decoding failure code.
    pub const fn code(self) -> TextStringErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> TextStringErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> TextStringRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the relative byte offset within decoded PDF string bytes, when known.
    pub const fn byte_offset(self) -> Option<u64> {
        self.byte_offset
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(self) -> Option<TextStringLimit> {
        self.limit
    }
}

impl fmt::Debug for TextStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextStringError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("byte_offset", &self.byte_offset)
            .field("detail", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for TextStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(offset) = self.byte_offset {
            write!(formatter, " at decoded-string byte {offset}")?;
        }
        if let Some(limit) = self.limit {
            write!(
                formatter,
                " limit_kind={:?} limit={} consumed={} attempted={}",
                limit.kind, limit.limit, limit.consumed, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for TextStringError {}

/// Owned Unicode result of one bounded PDF text-string decode.
#[derive(Eq, PartialEq)]
pub struct DecodedTextString {
    encoding: TextStringEncoding,
    value: String,
    input_bytes: u64,
    utf8_bytes: u64,
    reserved_utf8_bytes: u64,
}

impl DecodedTextString {
    /// Borrows the decoded Unicode scalar sequence as UTF-8.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Returns the encoding selected from the PDF text-string lead bytes.
    pub const fn encoding(&self) -> TextStringEncoding {
        self.encoding
    }

    /// Returns decoded PDF string bytes consumed, including a UTF-16BE BOM.
    pub const fn input_bytes(&self) -> u64 {
        self.input_bytes
    }

    /// Returns logical UTF-8 bytes in the decoded string.
    pub const fn utf8_bytes(&self) -> u64 {
        self.utf8_bytes
    }

    /// Returns allocator-reported UTF-8 backing capacity retained by this value.
    pub const fn reserved_utf8_bytes(&self) -> u64 {
        self.reserved_utf8_bytes
    }

    /// Consumes the proof-independent decoded value and returns its UTF-8 string.
    pub fn into_string(self) -> String {
        self.value
    }
}

impl fmt::Debug for DecodedTextString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedTextString")
            .field("encoding", &self.encoding)
            .field("input_bytes", &self.input_bytes)
            .field("utf8_bytes", &self.utf8_bytes)
            .field("reserved_utf8_bytes", &self.reserved_utf8_bytes)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Decodes one lexical PDF string under the ISO 32000-1 text-string convention.
///
/// A leading `FE FF` selects UTF-16BE and is not retained in the result. Every
/// other prefix selects PDFDocEncoding, including `FF FE`; undefined codes are
/// rejected instead of silently substituted. Both validation and materialization
/// probe cooperative cancellation after at most 256 input code units.
pub fn decode_text_string(
    value: &PdfString,
    limits: TextStringLimits,
    cancellation: &(dyn DocumentCancellation + '_),
) -> Result<DecodedTextString, TextStringError> {
    check_cancelled(cancellation, None)?;
    let bytes = value.bytes();
    let input_bytes = u64::try_from(bytes.len()).map_err(|_| {
        TextStringError::resource(
            TextStringLimitKind::InputBytes,
            limits.max_input_bytes(),
            0,
            u64::MAX,
            None,
        )
    })?;
    if input_bytes > limits.max_input_bytes() {
        return Err(TextStringError::resource(
            TextStringLimitKind::InputBytes,
            limits.max_input_bytes(),
            0,
            input_bytes,
            None,
        ));
    }

    let (encoding, encoded) = if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (TextStringEncoding::Utf16Be, payload)
    } else {
        (TextStringEncoding::PdfDocEncoding, bytes)
    };

    let mut utf8_bytes = 0_u64;
    scan_text(encoding, encoded, cancellation, |character, offset| {
        let attempted = u64::try_from(character.len_utf8()).unwrap_or(u64::MAX);
        let next = utf8_bytes.checked_add(attempted).ok_or_else(|| {
            TextStringError::resource(
                TextStringLimitKind::Utf8Bytes,
                limits.max_utf8_bytes(),
                utf8_bytes,
                attempted,
                Some(offset),
            )
        })?;
        if next > limits.max_utf8_bytes() {
            return Err(TextStringError::resource(
                TextStringLimitKind::Utf8Bytes,
                limits.max_utf8_bytes(),
                utf8_bytes,
                attempted,
                Some(offset),
            ));
        }
        utf8_bytes = next;
        Ok(())
    })?;

    let capacity = usize::try_from(utf8_bytes).map_err(|_| {
        TextStringError::resource(
            TextStringLimitKind::Utf8Bytes,
            limits.max_utf8_bytes(),
            0,
            utf8_bytes,
            None,
        )
    })?;
    let mut output = String::new();
    output.try_reserve_exact(capacity).map_err(|_| {
        TextStringError::resource(
            TextStringLimitKind::Utf8Bytes,
            limits.max_utf8_bytes(),
            0,
            utf8_bytes,
            None,
        )
    })?;
    let reserved_utf8_bytes = u64::try_from(output.capacity()).map_err(|_| {
        TextStringError::resource(
            TextStringLimitKind::Utf8Bytes,
            limits.max_utf8_bytes(),
            utf8_bytes,
            u64::MAX,
            None,
        )
    })?;
    if reserved_utf8_bytes > limits.max_utf8_bytes() {
        return Err(TextStringError::resource(
            TextStringLimitKind::Utf8Bytes,
            limits.max_utf8_bytes(),
            utf8_bytes,
            reserved_utf8_bytes,
            None,
        ));
    }

    scan_text(encoding, encoded, cancellation, |character, _| {
        output.push(character);
        Ok(())
    })?;
    if output.len() != capacity {
        return Err(TextStringError::resource(
            TextStringLimitKind::Utf8Bytes,
            limits.max_utf8_bytes(),
            utf8_bytes,
            u64::MAX,
            None,
        ));
    }
    check_cancelled(cancellation, Some(input_bytes))?;

    Ok(DecodedTextString {
        encoding,
        value: output,
        input_bytes,
        utf8_bytes,
        reserved_utf8_bytes,
    })
}

fn scan_text(
    encoding: TextStringEncoding,
    bytes: &[u8],
    cancellation: &(dyn DocumentCancellation + '_),
    visitor: impl FnMut(char, u64) -> Result<(), TextStringError>,
) -> Result<(), TextStringError> {
    match encoding {
        TextStringEncoding::PdfDocEncoding => scan_pdf_doc(bytes, cancellation, visitor),
        TextStringEncoding::Utf16Be => scan_utf16_be(bytes, cancellation, visitor),
    }
}

fn scan_pdf_doc(
    bytes: &[u8],
    cancellation: &(dyn DocumentCancellation + '_),
    mut visitor: impl FnMut(char, u64) -> Result<(), TextStringError>,
) -> Result<(), TextStringError> {
    check_cancelled(cancellation, None)?;
    let mut since_probe = 0_usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let offset = u64::try_from(index).unwrap_or(u64::MAX);
        let character = pdf_doc_character(byte).ok_or_else(|| {
            TextStringError::for_code(TextStringErrorCode::UndefinedPdfDocEncoding, Some(offset))
        })?;
        visitor(character, offset)?;
        probe_after_unit(cancellation, &mut since_probe, offset)?;
    }
    check_cancelled(
        cancellation,
        Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
    )
}

fn scan_utf16_be(
    bytes: &[u8],
    cancellation: &(dyn DocumentCancellation + '_),
    mut visitor: impl FnMut(char, u64) -> Result<(), TextStringError>,
) -> Result<(), TextStringError> {
    check_cancelled(cancellation, Some(2))?;
    if !bytes.len().is_multiple_of(2) {
        let offset = u64::try_from(bytes.len().saturating_sub(1))
            .unwrap_or(u64::MAX)
            .saturating_add(2);
        return Err(TextStringError::for_code(
            TextStringErrorCode::InvalidUtf16,
            Some(offset),
        ));
    }

    let mut index = 0_usize;
    let mut since_probe = 0_usize;
    while index < bytes.len() {
        let offset = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(2);
        let unit = u16::from_be_bytes([bytes[index], bytes[index + 1]]);
        index += 2;
        probe_after_unit(cancellation, &mut since_probe, offset)?;

        let scalar = if (0xd800..=0xdbff).contains(&unit) {
            if index == bytes.len() {
                return Err(TextStringError::for_code(
                    TextStringErrorCode::InvalidUtf16,
                    Some(offset),
                ));
            }
            let low_offset = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(2);
            let low = u16::from_be_bytes([bytes[index], bytes[index + 1]]);
            index += 2;
            probe_after_unit(cancellation, &mut since_probe, low_offset)?;
            if !(0xdc00..=0xdfff).contains(&low) {
                return Err(TextStringError::for_code(
                    TextStringErrorCode::InvalidUtf16,
                    Some(low_offset),
                ));
            }
            0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&unit) {
            return Err(TextStringError::for_code(
                TextStringErrorCode::InvalidUtf16,
                Some(offset),
            ));
        } else {
            u32::from(unit)
        };

        let character = char::from_u32(scalar).ok_or_else(|| {
            TextStringError::for_code(TextStringErrorCode::InvalidUtf16, Some(offset))
        })?;
        visitor(character, offset)?;
    }
    check_cancelled(
        cancellation,
        Some(
            u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(2),
        ),
    )
}

fn probe_after_unit(
    cancellation: &(dyn DocumentCancellation + '_),
    since_probe: &mut usize,
    offset: u64,
) -> Result<(), TextStringError> {
    *since_probe += 1;
    if *since_probe == CANCELLATION_PROBE_INTERVAL {
        *since_probe = 0;
        check_cancelled(cancellation, Some(offset))?;
    }
    Ok(())
}

fn check_cancelled(
    cancellation: &(dyn DocumentCancellation + '_),
    offset: Option<u64>,
) -> Result<(), TextStringError> {
    if cancellation.is_cancelled() {
        Err(TextStringError::for_code(
            TextStringErrorCode::Cancelled,
            offset,
        ))
    } else {
        Ok(())
    }
}

fn pdf_doc_character(byte: u8) -> Option<char> {
    let scalar = match byte {
        0x09 | 0x0a | 0x0d => u32::from(byte),
        0x18 => 0x02d8,
        0x19 => 0x02c7,
        0x1a => 0x02c6,
        0x1b => 0x02d9,
        0x1c => 0x02dd,
        0x1d => 0x02db,
        0x1e => 0x02da,
        0x1f => 0x02dc,
        0x20..=0x7e => u32::from(byte),
        0x80 => 0x2022,
        0x81 => 0x2020,
        0x82 => 0x2021,
        0x83 => 0x2026,
        0x84 => 0x2014,
        0x85 => 0x2013,
        0x86 => 0x0192,
        0x87 => 0x2044,
        0x88 => 0x2039,
        0x89 => 0x203a,
        0x8a => 0x2212,
        0x8b => 0x2030,
        0x8c => 0x201e,
        0x8d => 0x201c,
        0x8e => 0x201d,
        0x8f => 0x2018,
        0x90 => 0x2019,
        0x91 => 0x201a,
        0x92 => 0x2122,
        0x93 => 0xfb01,
        0x94 => 0xfb02,
        0x95 => 0x0141,
        0x96 => 0x0152,
        0x97 => 0x0160,
        0x98 => 0x0178,
        0x99 => 0x017d,
        0x9a => 0x0131,
        0x9b => 0x0142,
        0x9c => 0x0153,
        0x9d => 0x0161,
        0x9e => 0x017e,
        0xa0 => 0x20ac,
        0xa1..=0xac | 0xae..=0xff => u32::from(byte),
        _ => return None,
    };
    char::from_u32(scalar)
}
