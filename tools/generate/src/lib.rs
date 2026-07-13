#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Deterministic, self-authored PDF fixtures for PDF.rs tests.
//!
//! The initial API emits one simple page with a traditional cross-reference table. Every stored
//! byte offset and emitted PDF integer is checked before it is serialized.

use std::error::Error;
use std::fmt;

/// Schema written into generated-file metadata comments.
pub const GENERATOR_SCHEMA: u32 = 1;

/// Generator revision written into generated-file metadata comments.
pub const GENERATOR_REVISION: &str = env!("CARGO_PKG_VERSION");

const CONTENT_STREAM: &[u8] = b"q\nQ\n";
const OBJECT_COUNT: usize = 4;
const MAX_PDF_INTEGER: u64 = i64::MAX as u64;
const MAX_XREF_OFFSET: u64 = 9_999_999_999;

/// Failure to represent generator bookkeeping in the emitted PDF syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GenerateError {
    /// A byte offset cannot fit the ten-digit field of a traditional xref entry.
    OffsetOutOfRange {
        /// Attempted byte offset.
        offset: usize,
    },
    /// A byte length cannot fit the generator's non-negative PDF integer range.
    LengthOutOfRange {
        /// Attempted byte length.
        length: usize,
    },
    /// Adding the mandatory free xref entry overflowed the object count.
    ObjectCountOverflow {
        /// Number of non-free indirect objects.
        object_count: usize,
    },
}

impl fmt::Display for GenerateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OffsetOutOfRange { offset } => {
                write!(
                    formatter,
                    "byte offset {offset} does not fit a traditional xref entry"
                )
            }
            Self::LengthOutOfRange { length } => {
                write!(formatter, "byte length {length} does not fit a PDF integer")
            }
            Self::ObjectCountOverflow { object_count } => write!(
                formatter,
                "xref size overflows after accounting for {object_count} objects"
            ),
        }
    }
}

impl Error for GenerateError {}

/// Generates a deterministic PDF 1.7 document containing one 200-by-200-point blank page.
///
/// The document has four generation-zero indirect objects: catalog, page tree, page, and content
/// stream. It uses a traditional xref table and includes reproducibility metadata as PDF comments.
pub fn generate_one_page_pdf() -> Result<Vec<u8>, GenerateError> {
    let mut output = Vec::with_capacity(640);
    output.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
    append_metadata(&mut output);

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
    append_plain_object(
        &mut output,
        &mut object_offsets,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>\n",
    )?;
    append_stream_object(&mut output, &mut object_offsets, 4, CONTENT_STREAM)?;

    let startxref = checked_xref_offset(output.len())?;
    let xref_size = checked_xref_size(object_offsets.len())?;
    let xref_size = checked_pdf_length(xref_size)?;

    output.extend_from_slice(b"xref\n");
    append_text(&mut output, &format!("0 {xref_size}\n"));
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in object_offsets {
        append_text(&mut output, &format!("{offset:010} 00000 n \n"));
    }

    output.extend_from_slice(b"trailer\n");
    append_text(
        &mut output,
        &format!("<< /Size {xref_size} /Root 1 0 R >>\n"),
    );
    output.extend_from_slice(b"startxref\n");
    append_text(&mut output, &format!("{startxref}\n%%EOF\n"));

    Ok(output)
}

fn append_metadata(output: &mut Vec<u8>) {
    output.extend_from_slice(b"% generated_by=pdf-rs-generate\n");
    output.extend_from_slice(b"% input_hashes=none\n");
    append_text(
        output,
        &format!("% generator_revision={GENERATOR_REVISION}\n"),
    );
    append_text(output, &format!("% schema={GENERATOR_SCHEMA}\n"));
}

fn append_plain_object(
    output: &mut Vec<u8>,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
    body: &[u8],
) -> Result<(), GenerateError> {
    append_object_header(output, object_offsets, object_number)?;
    output.extend_from_slice(body);
    output.extend_from_slice(b"endobj\n");
    Ok(())
}

fn append_stream_object(
    output: &mut Vec<u8>,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
    content: &[u8],
) -> Result<(), GenerateError> {
    let content_length = checked_pdf_length(content.len())?;
    append_object_header(output, object_offsets, object_number)?;
    append_text(output, &format!("<< /Length {content_length} >>\n"));
    output.extend_from_slice(b"stream\n");
    output.extend_from_slice(content);
    output.extend_from_slice(b"endstream\nendobj\n");
    Ok(())
}

fn append_object_header(
    output: &mut Vec<u8>,
    object_offsets: &mut Vec<u64>,
    object_number: u64,
) -> Result<(), GenerateError> {
    let offset = checked_xref_offset(output.len())?;
    object_offsets.push(offset);
    append_text(output, &format!("{object_number} 0 obj\n"));
    Ok(())
}

fn append_text(output: &mut Vec<u8>, text: &str) {
    output.extend_from_slice(text.as_bytes());
}

fn checked_xref_offset(offset: usize) -> Result<u64, GenerateError> {
    let serialized =
        u64::try_from(offset).map_err(|_| GenerateError::OffsetOutOfRange { offset })?;
    if serialized > MAX_XREF_OFFSET {
        return Err(GenerateError::OffsetOutOfRange { offset });
    }
    Ok(serialized)
}

fn checked_pdf_length(length: usize) -> Result<u64, GenerateError> {
    let serialized =
        u64::try_from(length).map_err(|_| GenerateError::LengthOutOfRange { length })?;
    if serialized > MAX_PDF_INTEGER {
        return Err(GenerateError::LengthOutOfRange { length });
    }
    Ok(serialized)
}

fn checked_xref_size(object_count: usize) -> Result<usize, GenerateError> {
    object_count
        .checked_add(1)
        .ok_or(GenerateError::ObjectCountOverflow { object_count })
}

#[cfg(test)]
mod tests {
    use std::str;

    use super::{
        CONTENT_STREAM, GENERATOR_REVISION, GENERATOR_SCHEMA, GenerateError, MAX_PDF_INTEGER,
        MAX_XREF_OFFSET, checked_pdf_length, checked_xref_offset, checked_xref_size,
        generate_one_page_pdf,
    };

    #[test]
    fn output_is_deterministic_and_self_identifying() {
        let first = generated_pdf();
        let second = generated_pdf();

        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"));
        assert!(contains(&first, b"% generated_by=pdf-rs-generate\n"));
        assert!(contains(&first, b"% input_hashes=none\n"));
        assert!(contains(
            &first,
            format!("% generator_revision={GENERATOR_REVISION}\n").as_bytes()
        ));
        assert!(contains(
            &first,
            format!("% schema={GENERATOR_SCHEMA}\n").as_bytes()
        ));
        assert!(contains(&first, b"/Type /Pages /Kids [3 0 R] /Count 1"));
    }

    #[test]
    fn xref_entries_point_to_each_indirect_object() {
        let pdf = generated_pdf();
        let xref_start = find_bytes(&pdf, b"xref\n");
        let trailer_start = find_bytes(&pdf, b"trailer\n");
        let lines: Vec<&[u8]> = pdf[xref_start..trailer_start]
            .split(|byte| *byte == b'\n')
            .collect();

        assert_eq!(lines.first(), Some(&b"xref".as_slice()));
        assert_eq!(lines.get(1), Some(&b"0 5".as_slice()));
        assert_eq!(lines.get(2), Some(&b"0000000000 65535 f ".as_slice()));

        for object_number in 1..=4 {
            let line = lines
                .get(object_number + 2)
                .unwrap_or_else(|| panic!("missing xref entry for object {object_number}"));
            assert_eq!(line.len(), 19, "xref entries must be fixed-width");
            assert_eq!(&line[10..], b" 00000 n ");

            let declared_offset = parse_usize(&line[..10]);
            let object_header = format!("{object_number} 0 obj\n");
            assert_eq!(declared_offset, find_bytes(&pdf, object_header.as_bytes()));
        }
    }

    #[test]
    fn startxref_points_to_the_xref_keyword() {
        let pdf = generated_pdf();
        let xref_start = find_bytes(&pdf, b"xref\n");
        let marker = b"startxref\n";
        let value_start = find_bytes(&pdf, marker) + marker.len();
        let value_end = value_start
            + pdf[value_start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or_else(|| panic!("startxref value is not newline-terminated"));
        let declared_offset = parse_usize(&pdf[value_start..value_end]);

        assert_eq!(declared_offset, xref_start);
        assert_eq!(
            pdf.get(declared_offset..declared_offset + 5),
            Some(b"xref\n".as_slice())
        );
        assert!(contains(&pdf, b"trailer\n<< /Size 5 /Root 1 0 R >>\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn stream_length_matches_the_exact_content_bytes() {
        let pdf = generated_pdf();
        let length_marker = b"<< /Length ";
        let length_start = find_bytes(&pdf, length_marker) + length_marker.len();
        let stream_marker = b" >>\nstream\n";
        let stream_marker_offset = find_bytes(&pdf[length_start..], stream_marker);
        let length_end = length_start + stream_marker_offset;
        let content_start = length_end + stream_marker.len();
        let content_end = content_start + find_bytes(&pdf[content_start..], b"endstream\n");
        let declared_length = parse_usize(&pdf[length_start..length_end]);

        assert_eq!(declared_length, CONTENT_STREAM.len());
        assert_eq!(declared_length, content_end - content_start);
        assert_eq!(&pdf[content_start..content_end], CONTENT_STREAM);
    }

    #[test]
    fn offset_conversion_enforces_the_xref_field_width() {
        if let Ok(maximum) = usize::try_from(MAX_XREF_OFFSET) {
            assert_eq!(checked_xref_offset(maximum), Ok(MAX_XREF_OFFSET));

            let too_large = maximum
                .checked_add(1)
                .unwrap_or_else(|| panic!("test target cannot represent an oversized xref offset"));
            assert_eq!(
                checked_xref_offset(too_large),
                Err(GenerateError::OffsetOutOfRange { offset: too_large })
            );
        } else {
            let platform_maximum = u64::try_from(usize::MAX)
                .unwrap_or_else(|_| panic!("usize does not fit u64 on this target"));
            assert_eq!(checked_xref_offset(usize::MAX), Ok(platform_maximum));
        }
    }

    #[test]
    fn length_and_count_overflow_paths_are_rejected() {
        if let Ok(maximum) = usize::try_from(MAX_PDF_INTEGER) {
            assert_eq!(checked_pdf_length(maximum), Ok(MAX_PDF_INTEGER));

            let too_large = maximum
                .checked_add(1)
                .unwrap_or_else(|| panic!("test target cannot represent an oversized PDF integer"));
            assert_eq!(
                checked_pdf_length(too_large),
                Err(GenerateError::LengthOutOfRange { length: too_large })
            );
        } else {
            let platform_maximum = u64::try_from(usize::MAX)
                .unwrap_or_else(|_| panic!("usize does not fit u64 on this target"));
            assert_eq!(checked_pdf_length(usize::MAX), Ok(platform_maximum));
        }

        assert_eq!(
            checked_xref_size(usize::MAX),
            Err(GenerateError::ObjectCountOverflow {
                object_count: usize::MAX
            })
        );
    }

    fn generated_pdf() -> Vec<u8> {
        generate_one_page_pdf().unwrap_or_else(|error| panic!("generation failed: {error}"))
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
        let text = str::from_utf8(bytes)
            .unwrap_or_else(|error| panic!("numeric field is not UTF-8: {error}"));
        text.parse::<usize>()
            .unwrap_or_else(|error| panic!("numeric field is not an unsigned integer: {error}"))
    }
}
