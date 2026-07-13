#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Bounded, deterministic compilation of self-authored PDF fixture DSL sources.
//!
//! The current `m0.one-page-table.v1` profile accepts one PDF 1.7 page with a
//! fixed Catalog/Pages/Page/Stream topology, configurable MediaBox coordinates
//! and content bytes, and a traditional cross-reference table. Broader PDF
//! variation modes remain explicit unsupported results.

mod dsl;

use std::error::Error;
use std::fmt;

use pdf_rs_digest::{hex_digest, sha256};

use dsl::OnePageSpec;

/// Schema of the accepted one-page fixture DSL.
pub const DSL_SCHEMA: u32 = 1;

/// Schema written into generated-file metadata comments.
pub const GENERATOR_SCHEMA: u32 = 2;

/// Generator revision written into generated-file metadata comments.
pub const GENERATOR_REVISION: &str = env!("CARGO_PKG_VERSION");

/// Canonical project-authored source for the M0 one-page fixture.
pub const ONE_PAGE_DSL: &str = concat!(
    "document(version: \"1.7\") {\n",
    "  object(1) = catalog(pages: ref(2));\n",
    "  object(2) = pages(kids: [ref(3)], count: 1);\n",
    "  object(3) = page(\n",
    "    media_box: [0, 0, 200, 200],\n",
    "    resources: {},\n",
    "    contents: ref(4)\n",
    "  );\n",
    "  stream(4) { \"q\\nQ\\n\" }\n",
    "  xref(kind: table);\n",
    "}\n",
);

const OBJECT_COUNT: usize = 4;
const MAX_PDF_INTEGER: u64 = i64::MAX as u64;
const MAX_XREF_OFFSET: u64 = 9_999_999_999;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_TOKENS: usize = 100_000;
const MAX_OBJECTS: usize = 10_000;
const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// Exact machine-readable generator failure code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GenerateErrorCode {
    /// A configured limit is zero, below the fixed profile minimum, or above a tool ceiling.
    InvalidLimits,
    /// DSL source bytes exceed the configured ceiling.
    SourceLimit,
    /// Lexical tokens exceed the configured ceiling.
    TokenLimit,
    /// Indirect objects exceed the configured ceiling.
    ObjectLimit,
    /// Decoded content bytes exceed the configured ceiling.
    ContentLimit,
    /// Generated PDF bytes exceed the configured ceiling.
    OutputLimit,
    /// The DSL is not valid UTF-8 or violates the bounded grammar.
    InvalidSyntax,
    /// The DSL requests a feature outside the current profile.
    UnsupportedFeature,
    /// The fixed one-page object topology or geometry is inconsistent.
    InvalidTopology,
    /// A byte offset cannot fit a traditional xref entry.
    OffsetOutOfRange,
    /// A byte length cannot fit the supported non-negative PDF integer range.
    LengthOutOfRange,
    /// Adding the mandatory free xref entry overflowed the object count.
    ObjectCountOverflow,
    /// Hashing failed despite the configured source/output ceilings.
    HashFailed,
}

/// Stable coarse category for generator failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerateErrorCategory {
    /// Caller-supplied limits are invalid.
    Configuration,
    /// A deterministic resource boundary was reached.
    ResourceLimit,
    /// Source text violates the DSL grammar.
    Syntax,
    /// Source text requests a deliberately unavailable profile capability.
    Unsupported,
    /// Source text violates the selected profile's semantic topology.
    Structure,
    /// Internal checked bookkeeping or hashing failed.
    Internal,
}

/// Stable recovery class for generator failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerateRecoverability {
    /// Supply limits within the documented tool ceilings.
    CorrectConfiguration,
    /// Reduce the bounded source, token, object, content, or output workload.
    ReduceInput,
    /// Correct the DSL source without changing the selected profile.
    CorrectSource,
    /// Select or implement an explicitly supported generator profile.
    SelectSupportedProfile,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Stable, source-redacted generator failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerateError {
    /// Exact machine-readable failure code.
    pub code: GenerateErrorCode,
    /// Coarse policy category derived from [`Self::code`].
    pub category: GenerateErrorCategory,
    /// Approved recovery class derived from [`Self::code`].
    pub recoverability: GenerateRecoverability,
    /// Stable project diagnostic identifier.
    pub diagnostic_id: &'static str,
    /// Byte offset in the DSL source when one is available.
    pub byte_offset: Option<usize>,
    detail: &'static str,
}

impl GenerateError {
    fn new(
        code: GenerateErrorCode,
        diagnostic_id: &'static str,
        byte_offset: Option<usize>,
        detail: &'static str,
    ) -> Self {
        let (category, recoverability) = error_policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            byte_offset,
            detail,
        }
    }
}

impl fmt::Display for GenerateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} ({:?}): {}",
            self.diagnostic_id, self.code, self.detail
        )?;
        if let Some(offset) = self.byte_offset {
            write!(formatter, " at byte {offset}")?;
        }
        Ok(())
    }
}

impl Error for GenerateError {}

/// Per-compilation deterministic resource ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerateLimits {
    max_source_bytes: usize,
    max_tokens: usize,
    max_objects: usize,
    max_content_bytes: usize,
    max_output_bytes: usize,
}

impl GenerateLimits {
    /// Creates limits valid for the fixed four-object profile and hard ceilings.
    pub fn new(
        max_source_bytes: usize,
        max_tokens: usize,
        max_objects: usize,
        max_content_bytes: usize,
        max_output_bytes: usize,
    ) -> Result<Self, GenerateError> {
        if max_source_bytes == 0
            || max_source_bytes > MAX_SOURCE_BYTES
            || max_tokens == 0
            || max_tokens > MAX_TOKENS
            || !(OBJECT_COUNT..=MAX_OBJECTS).contains(&max_objects)
            || max_content_bytes == 0
            || max_content_bytes > MAX_CONTENT_BYTES
            || max_output_bytes == 0
            || max_output_bytes > MAX_OUTPUT_BYTES
        {
            return Err(invalid_limits());
        }
        Ok(Self {
            max_source_bytes,
            max_tokens,
            max_objects,
            max_content_bytes,
            max_output_bytes,
        })
    }

    /// Returns the maximum accepted DSL source bytes.
    pub const fn max_source_bytes(self) -> usize {
        self.max_source_bytes
    }

    /// Returns the maximum lexical token count.
    pub const fn max_tokens(self) -> usize {
        self.max_tokens
    }

    /// Returns the maximum indirect-object count.
    pub const fn max_objects(self) -> usize {
        self.max_objects
    }

    /// Returns the maximum decoded content-string bytes.
    pub const fn max_content_bytes(self) -> usize {
        self.max_content_bytes
    }

    /// Returns the maximum complete generated PDF bytes.
    pub const fn max_output_bytes(self) -> usize {
        self.max_output_bytes
    }
}

impl Default for GenerateLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024,
            max_tokens: 4096,
            max_objects: 64,
            max_content_bytes: 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024,
        }
    }
}

/// Generated PDF bytes and the exact source/output identities that bind them.
///
/// This type deliberately has no `Debug` implementation because DSL content can
/// contain document-sensitive fixture bytes.
pub struct GeneratedPdf {
    bytes: Vec<u8>,
    source_sha256: [u8; 32],
    output_sha256: [u8; 32],
}

impl GeneratedPdf {
    /// Borrows the generated PDF bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes the artifact and returns its generated PDF bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Returns the SHA-256 of the exact DSL source bytes.
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }

    /// Returns the SHA-256 of the complete generated PDF bytes.
    pub const fn output_sha256(&self) -> [u8; 32] {
        self.output_sha256
    }
}

/// Compiles one bounded `m0.one-page-table.v1` fixture DSL source.
pub fn compile_dsl(source: &[u8], limits: GenerateLimits) -> Result<GeneratedPdf, GenerateError> {
    if source.len() > limits.max_source_bytes {
        return Err(source_limit());
    }
    let source_sha256 = sha256(source).map_err(|_| hash_failed())?;
    let spec = dsl::parse(source, limits)?;
    let bytes = serialize(&spec, source_sha256, limits.max_output_bytes)?;
    let output_sha256 = sha256(&bytes).map_err(|_| hash_failed())?;
    Ok(GeneratedPdf {
        bytes,
        source_sha256,
        output_sha256,
    })
}

/// Generates the canonical repository one-page fixture through the DSL compiler.
pub fn generate_one_page_pdf() -> Result<Vec<u8>, GenerateError> {
    compile_dsl(ONE_PAGE_DSL.as_bytes(), GenerateLimits::default()).map(GeneratedPdf::into_bytes)
}

fn serialize(
    spec: &OnePageSpec,
    source_sha256: [u8; 32],
    max_output_bytes: usize,
) -> Result<Vec<u8>, GenerateError> {
    let mut output = PdfOutput::new(max_output_bytes);
    output.append(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")?;
    append_metadata(&mut output, &source_sha256)?;

    let mut object_offsets = Vec::with_capacity(OBJECT_COUNT);
    append_plain_object(
        &mut output,
        &mut object_offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>\n",
    )?;
    append_plain_object(
        &mut output,
        &mut object_offsets,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>\n",
    )?;
    append_object_header(&mut output, &mut object_offsets, 3)?;
    output.append_text(&format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [{} {} {} {}] /Resources << >> /Contents 4 0 R >>\n",
        spec.media_box[0], spec.media_box[1], spec.media_box[2], spec.media_box[3]
    ))?;
    output.append(b"endobj\n")?;
    append_stream_object(&mut output, &mut object_offsets, 4, &spec.content)?;

    let startxref = checked_xref_offset(output.len())?;
    let xref_size = checked_pdf_length(checked_xref_size(object_offsets.len())?)?;
    output.append(b"xref\n")?;
    output.append_text(&format!("0 {xref_size}\n"))?;
    output.append(b"0000000000 65535 f \n")?;
    for offset in object_offsets {
        output.append_text(&format!("{offset:010} 00000 n \n"))?;
    }
    output.append(b"trailer\n")?;
    output.append_text(&format!("<< /Size {xref_size} /Root 1 0 R >>\n"))?;
    output.append(b"startxref\n")?;
    output.append_text(&format!("{startxref}\n%%EOF\n"))?;
    Ok(output.finish())
}

fn append_metadata(output: &mut PdfOutput, source_sha256: &[u8; 32]) -> Result<(), GenerateError> {
    output.append(b"% generated_by=pdf-rs-generate\n")?;
    output.append_text(&format!(
        "% input_hashes=sha256:{}\n",
        hex_digest(source_sha256)
    ))?;
    output.append_text(&format!("% generator_revision={GENERATOR_REVISION}\n"))?;
    output.append_text(&format!("% dsl_schema={DSL_SCHEMA}\n"))?;
    output.append_text(&format!("% schema={GENERATOR_SCHEMA}\n"))
}

fn append_plain_object(
    output: &mut PdfOutput,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
    body: &[u8],
) -> Result<(), GenerateError> {
    append_object_header(output, object_offsets, object_number)?;
    output.append(body)?;
    output.append(b"endobj\n")
}

fn append_stream_object(
    output: &mut PdfOutput,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
    content: &[u8],
) -> Result<(), GenerateError> {
    let content_length = checked_pdf_length(content.len())?;
    append_object_header(output, object_offsets, object_number)?;
    output.append_text(&format!("<< /Length {content_length} >>\n"))?;
    output.append(b"stream\n")?;
    output.append(content)?;
    output.append(b"\nendstream\nendobj\n")
}

fn append_object_header(
    output: &mut PdfOutput,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
) -> Result<(), GenerateError> {
    let offset = checked_xref_offset(output.len())?;
    object_offsets
        .try_reserve(1)
        .map_err(|_| object_limit(None))?;
    object_offsets.push(offset);
    output.append_text(&format!("{object_number} 0 obj\n"))
}

struct PdfOutput {
    bytes: Vec<u8>,
    limit: usize,
}

impl PdfOutput {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn append(&mut self, bytes: &[u8]) -> Result<(), GenerateError> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(output_limit)?;
        if next > self.limit {
            return Err(output_limit());
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| output_limit())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn append_text(&mut self, text: &str) -> Result<(), GenerateError> {
        self.append(text.as_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn checked_xref_offset(offset: usize) -> Result<u64, GenerateError> {
    let serialized = u64::try_from(offset).map_err(|_| offset_out_of_range())?;
    if serialized > MAX_XREF_OFFSET {
        return Err(offset_out_of_range());
    }
    Ok(serialized)
}

fn checked_pdf_length(length: usize) -> Result<u64, GenerateError> {
    let serialized = u64::try_from(length).map_err(|_| length_out_of_range())?;
    if serialized > MAX_PDF_INTEGER {
        return Err(length_out_of_range());
    }
    Ok(serialized)
}

fn checked_xref_size(object_count: usize) -> Result<usize, GenerateError> {
    object_count
        .checked_add(1)
        .ok_or_else(object_count_overflow)
}

const fn error_policy(code: GenerateErrorCode) -> (GenerateErrorCategory, GenerateRecoverability) {
    match code {
        GenerateErrorCode::InvalidLimits => (
            GenerateErrorCategory::Configuration,
            GenerateRecoverability::CorrectConfiguration,
        ),
        GenerateErrorCode::SourceLimit
        | GenerateErrorCode::TokenLimit
        | GenerateErrorCode::ObjectLimit
        | GenerateErrorCode::ContentLimit
        | GenerateErrorCode::OutputLimit => (
            GenerateErrorCategory::ResourceLimit,
            GenerateRecoverability::ReduceInput,
        ),
        GenerateErrorCode::InvalidSyntax => (
            GenerateErrorCategory::Syntax,
            GenerateRecoverability::CorrectSource,
        ),
        GenerateErrorCode::UnsupportedFeature => (
            GenerateErrorCategory::Unsupported,
            GenerateRecoverability::SelectSupportedProfile,
        ),
        GenerateErrorCode::InvalidTopology => (
            GenerateErrorCategory::Structure,
            GenerateRecoverability::CorrectSource,
        ),
        GenerateErrorCode::OffsetOutOfRange
        | GenerateErrorCode::LengthOutOfRange
        | GenerateErrorCode::ObjectCountOverflow
        | GenerateErrorCode::HashFailed => (
            GenerateErrorCategory::Internal,
            GenerateRecoverability::DoNotRetry,
        ),
    }
}

fn invalid_limits() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::InvalidLimits,
        "RPE-GENERATE-0001",
        None,
        "generator limits are invalid",
    )
}

fn source_limit() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::SourceLimit,
        "RPE-GENERATE-0002",
        None,
        "DSL source exceeds its byte limit",
    )
}

pub(crate) fn token_limit(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::TokenLimit,
        "RPE-GENERATE-0003",
        offset,
        "DSL token count exceeds its limit",
    )
}

pub(crate) fn object_limit(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::ObjectLimit,
        "RPE-GENERATE-0004",
        offset,
        "DSL object count exceeds its limit",
    )
}

pub(crate) fn content_limit(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::ContentLimit,
        "RPE-GENERATE-0005",
        offset,
        "decoded DSL content exceeds its byte limit",
    )
}

fn output_limit() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::OutputLimit,
        "RPE-GENERATE-0006",
        None,
        "generated PDF exceeds its byte limit",
    )
}

pub(crate) fn syntax_error(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::InvalidSyntax,
        "RPE-GENERATE-0007",
        offset,
        "DSL syntax is invalid",
    )
}

pub(crate) fn unsupported(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::UnsupportedFeature,
        "RPE-GENERATE-0008",
        offset,
        "DSL feature is outside the selected profile",
    )
}

pub(crate) fn topology_error(offset: Option<usize>) -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::InvalidTopology,
        "RPE-GENERATE-0009",
        offset,
        "DSL object topology or geometry is invalid",
    )
}

fn offset_out_of_range() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::OffsetOutOfRange,
        "RPE-GENERATE-0010",
        None,
        "PDF offset does not fit a traditional xref entry",
    )
}

fn length_out_of_range() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::LengthOutOfRange,
        "RPE-GENERATE-0011",
        None,
        "byte length does not fit a supported PDF integer",
    )
}

fn object_count_overflow() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::ObjectCountOverflow,
        "RPE-GENERATE-0012",
        None,
        "xref size overflows after adding the free entry",
    )
}

fn hash_failed() -> GenerateError {
    GenerateError::new(
        GenerateErrorCode::HashFailed,
        "RPE-GENERATE-0013",
        None,
        "bounded generator hashing failed",
    )
}

#[cfg(test)]
mod tests {
    use std::str;

    use pdf_rs_digest::{hex_digest, sha256};

    use super::*;

    #[test]
    fn canonical_source_is_deterministic_and_identity_bound() {
        let first = compile_default();
        let second = compile_default();
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(
            first.source_sha256(),
            sha256(ONE_PAGE_DSL.as_bytes()).unwrap()
        );
        assert_eq!(first.output_sha256(), sha256(first.bytes()).unwrap());
        assert!(first.bytes().starts_with(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"));
        assert!(contains(
            first.bytes(),
            format!(
                "% input_hashes=sha256:{}\n",
                hex_digest(&first.source_sha256())
            )
            .as_bytes()
        ));
        assert!(contains(first.bytes(), b"% dsl_schema=1\n% schema=2\n"));
    }

    #[test]
    fn configurable_media_box_and_escaped_content_are_serialized() {
        let source = ONE_PAGE_DSL
            .replace("[0, 0, 200, 200]", "[-10, 5, 320, 480]")
            .replace("q\\nQ\\n", "A\\x00B\\tC");
        let generated = compile_dsl(source.as_bytes(), GenerateLimits::default()).unwrap();
        assert!(contains(generated.bytes(), b"/MediaBox [-10 5 320 480]"));
        assert!(contains(
            generated.bytes(),
            b"<< /Length 5 >>\nstream\nA\0B\tC"
        ));
        assert_ne!(generated.output_sha256(), compile_default().output_sha256());
    }

    #[test]
    fn xref_entries_and_startxref_match_canonical_offsets() {
        let pdf = generate_one_page_pdf().unwrap();
        let xref_start = find_bytes(&pdf, b"xref\n");
        let trailer_start = find_bytes(&pdf, b"trailer\n");
        assert!(contains(
            &pdf[trailer_start..],
            b"trailer\n<< /Size 5 /Root 1 0 R >>\n"
        ));
        let lines: Vec<&[u8]> = pdf[xref_start..trailer_start]
            .split(|byte| *byte == b'\n')
            .collect();
        assert_eq!(lines.first(), Some(&b"xref".as_slice()));
        assert_eq!(lines.get(1), Some(&b"0 5".as_slice()));
        assert_eq!(lines.get(2), Some(&b"0000000000 65535 f ".as_slice()));
        for object_number in 1..=4 {
            let line = lines.get(object_number + 2).unwrap();
            assert_eq!(line.len(), 19);
            let offset = parse_usize(&line[..10]);
            assert_eq!(
                offset,
                find_bytes(&pdf, format!("{object_number} 0 obj\n").as_bytes())
            );
        }
        let marker = b"startxref\n";
        let value_start = find_bytes(&pdf, marker) + marker.len();
        let value_end = value_start
            + pdf[value_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap();
        assert_eq!(parse_usize(&pdf[value_start..value_end]), xref_start);
    }

    #[test]
    fn stream_length_matches_decoded_content() {
        let pdf = generate_one_page_pdf().unwrap();
        let marker = b"<< /Length ";
        let length_start = find_bytes(&pdf, marker) + marker.len();
        let stream_marker = b" >>\nstream\n";
        let length_end = length_start + find_bytes(&pdf[length_start..], stream_marker);
        let content_start = length_end + stream_marker.len();
        let content_length = parse_usize(&pdf[length_start..length_end]);
        let content_end = content_start + content_length;
        assert_eq!(&pdf[content_start..content_end], b"q\nQ\n");
        assert_eq!(&pdf[content_end..content_end + 11], b"\nendstream\n");
    }

    #[test]
    fn stream_delimiter_is_not_counted_as_content() {
        let source = ONE_PAGE_DSL.replace("q\\nQ\\n", "ABC");
        let pdf = compile_dsl(source.as_bytes(), GenerateLimits::default()).unwrap();
        assert!(contains(
            pdf.bytes(),
            b"<< /Length 3 >>\nstream\nABC\nendstream\n"
        ));
    }

    #[test]
    fn syntax_topology_and_unsupported_failures_are_stable_and_redacted() {
        let cases = [
            (
                ONE_PAGE_DSL.replace("document", "document!"),
                GenerateErrorCode::InvalidSyntax,
                GenerateErrorCategory::Syntax,
                GenerateRecoverability::CorrectSource,
                "RPE-GENERATE-0007",
            ),
            (
                ONE_PAGE_DSL.replace("ref(4)", "ref(9)"),
                GenerateErrorCode::InvalidTopology,
                GenerateErrorCategory::Structure,
                GenerateRecoverability::CorrectSource,
                "RPE-GENERATE-0009",
            ),
            (
                ONE_PAGE_DSL.replace("\"1.7\"", "\"2.0\""),
                GenerateErrorCode::UnsupportedFeature,
                GenerateErrorCategory::Unsupported,
                GenerateRecoverability::SelectSupportedProfile,
                "RPE-GENERATE-0008",
            ),
            (
                ONE_PAGE_DSL.replace("kind: table", "kind: stream"),
                GenerateErrorCode::UnsupportedFeature,
                GenerateErrorCategory::Unsupported,
                GenerateRecoverability::SelectSupportedProfile,
                "RPE-GENERATE-0008",
            ),
        ];
        for (source, code, category, recoverability, diagnostic_id) in cases {
            let error = compile_dsl(source.as_bytes(), GenerateLimits::default())
                .err()
                .unwrap();
            assert_eq!(error.code, code);
            assert_eq!(error.category, category);
            assert_eq!(error.recoverability, recoverability);
            assert_eq!(error.diagnostic_id, diagnostic_id);
            assert!(!error.to_string().contains("ref(9)"));
            assert!(!format!("{error:?}").contains("ref(9)"));
        }
    }

    #[test]
    fn every_canonical_source_truncation_is_rejected_without_content_in_diagnostics() {
        let required_end = ONE_PAGE_DSL.rfind('}').unwrap() + 1;
        for end in 0..required_end {
            let error = compile_dsl(&ONE_PAGE_DSL.as_bytes()[..end], GenerateLimits::default())
                .err()
                .unwrap();
            assert!(matches!(
                error.code,
                GenerateErrorCode::InvalidSyntax
                    | GenerateErrorCode::UnsupportedFeature
                    | GenerateErrorCode::InvalidTopology
            ));
            assert!(!error.to_string().contains("q\\nQ"));
        }
    }

    #[test]
    fn source_object_content_and_output_limits_accept_exact_boundaries() {
        let defaults = GenerateLimits::default();
        let source_len = ONE_PAGE_DSL.len();
        assert!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, 4096, 4, 4, 4096)
            )
            .is_ok()
        );
        assert_eq!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len - 1, 4096, 4, 4, 4096)
            )
            .err()
            .unwrap()
            .code,
            GenerateErrorCode::SourceLimit
        );
        assert!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len + 1, 4096, 5, 5, 4096)
            )
            .is_ok()
        );

        assert_eq!(
            GenerateLimits::new(source_len, 4096, 3, 4, 4096)
                .unwrap_err()
                .code,
            GenerateErrorCode::InvalidLimits
        );
        assert_eq!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, 4096, 4, 3, 4096)
            )
            .err()
            .unwrap()
            .code,
            GenerateErrorCode::ContentLimit
        );

        let output_len = compile_default().bytes().len();
        assert!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, 4096, 4, 4, output_len)
            )
            .is_ok()
        );
        assert_eq!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, 4096, 4, 4, output_len - 1)
            )
            .err()
            .unwrap()
            .code,
            GenerateErrorCode::OutputLimit
        );
        assert!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, 4096, 4, 4, output_len + 1)
            )
            .is_ok()
        );
        assert_eq!(defaults.max_objects(), 64);

        let one_byte_content = ONE_PAGE_DSL.replace("q\\nQ\\n", "A");
        assert!(
            compile_dsl(
                one_byte_content.as_bytes(),
                limits(one_byte_content.len(), 4096, 4, 1, 4096)
            )
            .is_ok()
        );
    }

    #[test]
    fn token_limit_accepts_exact_boundary() {
        let source_len = ONE_PAGE_DSL.len();
        let exact = (1..4096)
            .find(|count| {
                compile_dsl(
                    ONE_PAGE_DSL.as_bytes(),
                    limits(source_len, *count, 4, 4, 4096),
                )
                .is_ok()
            })
            .unwrap();
        assert_eq!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, exact - 1, 4, 4, 4096)
            )
            .err()
            .unwrap()
            .code,
            GenerateErrorCode::TokenLimit
        );
        assert!(
            compile_dsl(
                ONE_PAGE_DSL.as_bytes(),
                limits(source_len, exact + 1, 4, 4, 4096)
            )
            .is_ok()
        );
    }

    #[test]
    fn token_limit_is_charged_before_over_budget_string_decoding() {
        let source = ONE_PAGE_DSL.replace("q\\nQ\\n", "AB");
        let error = compile_dsl(source.as_bytes(), limits(source.len(), 80, 4, 1, 4096))
            .err()
            .unwrap();
        assert_eq!(error.code, GenerateErrorCode::TokenLimit);
    }

    #[test]
    fn utf8_and_integer_boundaries_are_checked() {
        let invalid_utf8 = compile_dsl(&[0xff], GenerateLimits::default())
            .err()
            .unwrap();
        assert_eq!(invalid_utf8.code, GenerateErrorCode::InvalidSyntax);
        assert_eq!(invalid_utf8.byte_offset, Some(0));

        let overflow = ONE_PAGE_DSL.replace("200, 200", "9223372036854775808, 200");
        assert_eq!(
            compile_dsl(overflow.as_bytes(), GenerateLimits::default())
                .err()
                .unwrap()
                .code,
            GenerateErrorCode::InvalidSyntax
        );

        let extremes = ONE_PAGE_DSL.replace(
            "[0, 0, 200, 200]",
            "[-9223372036854775808, -1, 9223372036854775807, 1]",
        );
        let generated = compile_dsl(extremes.as_bytes(), GenerateLimits::default()).unwrap();
        assert!(contains(
            generated.bytes(),
            b"/MediaBox [-9223372036854775808 -1 9223372036854775807 1]"
        ));
    }

    #[test]
    fn invalid_limit_configurations_and_hard_ceilings_are_rejected() {
        for result in [
            GenerateLimits::new(0, 1, 4, 1, 1),
            GenerateLimits::new(1, 0, 4, 1, 1),
            GenerateLimits::new(1, 1, 3, 1, 1),
            GenerateLimits::new(1, 1, 4, 0, 1),
            GenerateLimits::new(1, 1, 4, 1, 0),
            GenerateLimits::new(MAX_SOURCE_BYTES + 1, 1, 4, 1, 1),
            GenerateLimits::new(1, MAX_TOKENS + 1, 4, 1, 1),
            GenerateLimits::new(1, 1, MAX_OBJECTS + 1, 1, 1),
            GenerateLimits::new(1, 1, 4, MAX_CONTENT_BYTES + 1, 1),
            GenerateLimits::new(1, 1, 4, 1, MAX_OUTPUT_BYTES + 1),
        ] {
            assert_eq!(result.unwrap_err().code, GenerateErrorCode::InvalidLimits);
        }
    }

    #[test]
    fn bookkeeping_overflow_helpers_have_stable_internal_codes() {
        if let Ok(maximum) = usize::try_from(MAX_XREF_OFFSET) {
            assert_eq!(checked_xref_offset(maximum).unwrap(), MAX_XREF_OFFSET);
            if let Some(too_large) = maximum.checked_add(1) {
                assert_eq!(
                    checked_xref_offset(too_large).unwrap_err().code,
                    GenerateErrorCode::OffsetOutOfRange
                );
            }
        }
        if let Ok(maximum) = usize::try_from(MAX_PDF_INTEGER) {
            assert_eq!(checked_pdf_length(maximum).unwrap(), MAX_PDF_INTEGER);
            if let Some(too_large) = maximum.checked_add(1) {
                assert_eq!(
                    checked_pdf_length(too_large).unwrap_err().code,
                    GenerateErrorCode::LengthOutOfRange
                );
            }
        }
        assert_eq!(
            checked_xref_size(usize::MAX).unwrap_err().code,
            GenerateErrorCode::ObjectCountOverflow
        );
    }

    fn compile_default() -> GeneratedPdf {
        compile_dsl(ONE_PAGE_DSL.as_bytes(), GenerateLimits::default()).unwrap()
    }

    fn limits(
        source: usize,
        tokens: usize,
        objects: usize,
        content: usize,
        output: usize,
    ) -> GenerateLimits {
        GenerateLimits::new(source, tokens, objects, content, output).unwrap()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| {
                panic!(
                    "missing byte sequence {:?}",
                    String::from_utf8_lossy(needle)
                )
            })
    }

    fn parse_usize(bytes: &[u8]) -> usize {
        str::from_utf8(bytes).unwrap().parse::<usize>().unwrap()
    }
}
