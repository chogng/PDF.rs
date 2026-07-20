use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_document::{
    DocumentCancellation, NeverCancelled, TextStringEncoding, TextStringError, TextStringErrorCode,
    TextStringLimitConfig, TextStringLimitKind, TextStringLimits, decode_text_string,
};
use pdf_rs_syntax::{
    InputExtent, PdfString, SyntaxInput, SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
};

fn identity() -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([0x74; 32]), SourceRevision::new(1))
}

fn pdf_string(bytes: &[u8]) -> PdfString {
    let mut source = String::with_capacity(
        bytes
            .len()
            .checked_mul(2)
            .and_then(|length| length.checked_add(2))
            .expect("small test string source length is representable"),
    );
    source.push('<');
    for byte in bytes {
        write!(&mut source, "{byte:02X}").expect("writing to a String cannot fail");
    }
    source.push('>');

    let input = SyntaxInput::new(
        identity(),
        0,
        source.as_bytes(),
        InputExtent::KnownSourceEnd,
    )
    .expect("test syntax input is valid");
    let mut parser = SyntaxParser::new(input, SyntaxLimits::default())
        .expect("test syntax input fits the default parser limits");
    let parsed = match parser.parse_object() {
        SyntaxPoll::Ready(value) => value,
        SyntaxPoll::NeedMore { .. } => {
            panic!("complete hexadecimal string unexpectedly needs more")
        }
        SyntaxPoll::EndOfInput => panic!("complete hexadecimal string unexpectedly reached EOF"),
        SyntaxPoll::Failed(error) => panic!("test hexadecimal string failed to parse: {error}"),
    };

    match parsed.into_value() {
        SyntaxObject::String(value) => value,
        other => panic!("test hexadecimal string parsed as {other:?}"),
    }
}

fn limits(max_input_bytes: u64, max_utf8_bytes: u64) -> TextStringLimits {
    TextStringLimits::validate(TextStringLimitConfig {
        max_input_bytes,
        max_utf8_bytes,
    })
    .expect("test text-string limits are valid")
}

fn failed(bytes: &[u8]) -> TextStringError {
    decode_text_string(
        &pdf_string(bytes),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect_err("test text string must be rejected")
}

#[test]
fn pdfdoc_encoding_maps_every_non_identity_special_byte() {
    let special = [
        (0x18, '\u{02d8}'),
        (0x19, '\u{02c7}'),
        (0x1a, '\u{02c6}'),
        (0x1b, '\u{02d9}'),
        (0x1c, '\u{02dd}'),
        (0x1d, '\u{02db}'),
        (0x1e, '\u{02da}'),
        (0x1f, '\u{02dc}'),
        (0x80, '\u{2022}'),
        (0x81, '\u{2020}'),
        (0x82, '\u{2021}'),
        (0x83, '\u{2026}'),
        (0x84, '\u{2014}'),
        (0x85, '\u{2013}'),
        (0x86, '\u{0192}'),
        (0x87, '\u{2044}'),
        (0x88, '\u{2039}'),
        (0x89, '\u{203a}'),
        (0x8a, '\u{2212}'),
        (0x8b, '\u{2030}'),
        (0x8c, '\u{201e}'),
        (0x8d, '\u{201c}'),
        (0x8e, '\u{201d}'),
        (0x8f, '\u{2018}'),
        (0x90, '\u{2019}'),
        (0x91, '\u{201a}'),
        (0x92, '\u{2122}'),
        (0x93, '\u{fb01}'),
        (0x94, '\u{fb02}'),
        (0x95, '\u{0141}'),
        (0x96, '\u{0152}'),
        (0x97, '\u{0160}'),
        (0x98, '\u{0178}'),
        (0x99, '\u{017d}'),
        (0x9a, '\u{0131}'),
        (0x9b, '\u{0142}'),
        (0x9c, '\u{0153}'),
        (0x9d, '\u{0161}'),
        (0x9e, '\u{017e}'),
        (0xa0, '\u{20ac}'),
    ];
    let bytes = special.iter().map(|(byte, _)| *byte).collect::<Vec<_>>();
    let expected = special
        .iter()
        .map(|(_, scalar)| *scalar)
        .collect::<String>();

    let decoded = decode_text_string(
        &pdf_string(&bytes),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("every defined PDFDocEncoding special byte decodes");

    assert_eq!(decoded.encoding(), TextStringEncoding::PdfDocEncoding);
    assert_eq!(decoded.as_str(), expected);
    assert_eq!(decoded.input_bytes(), bytes.len() as u64);
    assert_eq!(decoded.utf8_bytes(), expected.len() as u64);
    assert!(decoded.reserved_utf8_bytes() >= decoded.utf8_bytes());
}

#[test]
fn pdfdoc_encoding_preserves_ascii_and_defined_latin_boundaries() {
    let decoded = decode_text_string(
        &pdf_string(&[0x20, 0x7e, 0xa1, 0xac, 0xae, 0xff]),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("identity PDFDocEncoding ranges decode directly");

    assert_eq!(decoded.encoding(), TextStringEncoding::PdfDocEncoding);
    assert_eq!(decoded.as_str(), " ~¡¬®ÿ");
}

#[test]
fn pdfdoc_encoding_rejects_each_undefined_byte() {
    for byte in [0x7f, 0x9f, 0xad] {
        let error = failed(&[b'A', byte, b'B']);
        assert_eq!(
            error.code(),
            TextStringErrorCode::UndefinedPdfDocEncoding,
            "undefined PDFDocEncoding byte {byte:#04x}"
        );
        assert!(error.limit().is_none());
    }
}

#[test]
fn pdfdoc_encoding_rejects_undefined_controls_but_accepts_sr_controls() {
    let undefined_controls = (0x00..=0x08).chain(0x0b..=0x0c).chain(0x0e..=0x17);
    for byte in undefined_controls {
        assert_eq!(
            failed(&[byte]).code(),
            TextStringErrorCode::UndefinedPdfDocEncoding,
            "Annex D.3 marks control byte {byte:#04x} as undefined"
        );
    }

    let decoded = decode_text_string(
        &pdf_string(b"\t\n\r"),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("Annex D.3 SR controls remain defined");
    assert_eq!(decoded.encoding(), TextStringEncoding::PdfDocEncoding);
    assert_eq!(decoded.as_str(), "\t\n\r");
}

#[test]
fn utf16be_decodes_bmp_scalars_surrogate_pairs_and_bom_only() {
    let bmp_bytes = [0xfe, 0xff, 0x00, 0x41, 0x03, 0xa9, 0x4e, 0x2d];
    let bmp = decode_text_string(
        &pdf_string(&bmp_bytes),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("well-formed UTF-16BE BMP text decodes");
    assert_eq!(bmp.encoding(), TextStringEncoding::Utf16Be);
    assert_eq!(bmp.as_str(), "AΩ中");
    assert_eq!(bmp.input_bytes(), bmp_bytes.len() as u64);
    assert_eq!(bmp.utf8_bytes(), "AΩ中".len() as u64);

    let supplementary_bytes = [0xfe, 0xff, 0xd8, 0x3c, 0xdf, 0xa8];
    let supplementary = decode_text_string(
        &pdf_string(&supplementary_bytes),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("a well-formed UTF-16BE surrogate pair decodes");
    assert_eq!(supplementary.encoding(), TextStringEncoding::Utf16Be);
    assert_eq!(supplementary.as_str(), "🎨");
    assert_eq!(supplementary.utf8_bytes(), 4);

    let bom_only = decode_text_string(
        &pdf_string(&[0xfe, 0xff]),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("a UTF-16BE BOM without code units is an empty text string");
    assert_eq!(bom_only.encoding(), TextStringEncoding::Utf16Be);
    assert_eq!(bom_only.as_str(), "");
    assert_eq!(bom_only.input_bytes(), 2);
    assert_eq!(bom_only.utf8_bytes(), 0);
    assert_eq!(bom_only.reserved_utf8_bytes(), 0);
}

#[test]
fn utf16be_rejects_odd_payloads_and_unpaired_surrogates() {
    for bytes in [
        &[0xfe, 0xff, 0x00][..],
        &[0xfe, 0xff, 0xd8, 0x00][..],
        &[0xfe, 0xff, 0xdc, 0x00][..],
        &[0xfe, 0xff, 0xd8, 0x00, 0x00, 0x41][..],
    ] {
        assert_eq!(failed(bytes).code(), TextStringErrorCode::InvalidUtf16);
    }
}

#[test]
fn little_endian_bom_is_pdfdocencoding_data_not_an_encoding_marker() {
    let decoded = decode_text_string(
        &pdf_string(&[0xff, 0xfe]),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("FF FE bytes are both defined in PDFDocEncoding");

    assert_eq!(decoded.encoding(), TextStringEncoding::PdfDocEncoding);
    assert_eq!(decoded.as_str(), "ÿþ");
    assert_eq!(decoded.input_bytes(), 2);
    assert_eq!(decoded.utf8_bytes(), 4);
}

#[test]
fn input_byte_limit_accepts_exact_and_rejects_one_less() {
    let bytes = b"boundary";
    let value = pdf_string(bytes);
    let baseline = decode_text_string(&value, TextStringLimits::default(), &NeverCancelled)
        .expect("baseline text string decodes");
    let output_limit = baseline.reserved_utf8_bytes();
    let input_bytes = bytes.len() as u64;

    let exact = decode_text_string(&value, limits(input_bytes, output_limit), &NeverCancelled)
        .expect("the exact input byte budget is accepted");
    assert_eq!(exact.as_str(), "boundary");

    let error = decode_text_string(
        &value,
        limits(input_bytes - 1, output_limit),
        &NeverCancelled,
    )
    .expect_err("one byte less than the input length is rejected");
    assert_eq!(error.code(), TextStringErrorCode::ResourceLimit);
    let limit = error.limit().expect("resource errors expose limit context");
    assert_eq!(limit.kind(), TextStringLimitKind::InputBytes);
    assert_eq!(limit.limit(), input_bytes - 1);
    assert!(limit.attempted() > limit.limit());
}

#[test]
fn utf8_byte_limit_accepts_measured_capacity_and_rejects_one_less() {
    let bytes = [0x18; 17];
    let value = pdf_string(&bytes);
    let baseline = decode_text_string(&value, TextStringLimits::default(), &NeverCancelled)
        .expect("baseline text string decodes");
    let reserved_utf8_bytes = baseline.reserved_utf8_bytes();
    assert!(reserved_utf8_bytes >= baseline.utf8_bytes());
    assert!(reserved_utf8_bytes > 0);

    let exact = decode_text_string(
        &value,
        limits(bytes.len() as u64, reserved_utf8_bytes),
        &NeverCancelled,
    )
    .expect("the allocator-reported UTF-8 capacity budget is accepted exactly");
    assert_eq!(exact.as_str(), baseline.as_str());
    assert_eq!(exact.reserved_utf8_bytes(), reserved_utf8_bytes);

    let error = decode_text_string(
        &value,
        limits(bytes.len() as u64, reserved_utf8_bytes - 1),
        &NeverCancelled,
    )
    .expect_err("one byte less than the retained UTF-8 capacity is rejected");
    assert_eq!(error.code(), TextStringErrorCode::ResourceLimit);
    let limit = error.limit().expect("resource errors expose limit context");
    assert_eq!(limit.kind(), TextStringLimitKind::Utf8Bytes);
    assert_eq!(limit.limit(), reserved_utf8_bytes - 1);
    assert!(limit.consumed().saturating_add(limit.attempted()) > limit.limit());
}

#[test]
fn limit_validation_rejects_zero_dimensions_and_preserves_valid_values() {
    for config in [
        TextStringLimitConfig {
            max_input_bytes: 0,
            max_utf8_bytes: 1,
        },
        TextStringLimitConfig {
            max_input_bytes: 1,
            max_utf8_bytes: 0,
        },
    ] {
        let error = TextStringLimits::validate(config).expect_err("zero limits are invalid");
        assert_eq!(error.code(), TextStringErrorCode::InvalidLimits);
    }

    let validated = limits(17, 53);
    assert_eq!(validated.max_input_bytes(), 17);
    assert_eq!(validated.max_utf8_bytes(), 53);

    let hard_ceiling = TextStringLimits::validate(TextStringLimitConfig {
        max_input_bytes: 16 * 1024 * 1024,
        max_utf8_bytes: 64 * 1024 * 1024,
    })
    .expect("fixed text-string hard ceilings are accepted exactly");
    assert_eq!(hard_ceiling.max_input_bytes(), 16 * 1024 * 1024);
    assert_eq!(hard_ceiling.max_utf8_bytes(), 64 * 1024 * 1024);
    for config in [
        TextStringLimitConfig {
            max_input_bytes: 16 * 1024 * 1024 + 1,
            max_utf8_bytes: 1,
        },
        TextStringLimitConfig {
            max_input_bytes: 1,
            max_utf8_bytes: 64 * 1024 * 1024 + 1,
        },
    ] {
        let error = TextStringLimits::validate(config)
            .expect_err("one above a fixed hard ceiling is invalid");
        assert_eq!(error.code(), TextStringErrorCode::InvalidLimits);
    }
}

struct CancelOnProbe {
    probes: AtomicUsize,
    cancel_at: usize,
}

impl CancelOnProbe {
    const fn new(cancel_at: usize) -> Self {
        Self {
            probes: AtomicUsize::new(0),
            cancel_at,
        }
    }

    fn probes(&self) -> usize {
        self.probes.load(Ordering::Acquire)
    }
}

impl DocumentCancellation for CancelOnProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_at
    }
}

#[test]
fn decoding_probes_cancellation_during_long_input() {
    let cancellation = CancelOnProbe::new(3);
    let value = pdf_string(&vec![b'A'; 1024]);

    let error = decode_text_string(&value, TextStringLimits::default(), &cancellation)
        .expect_err("cancellation is observed during a long decode loop");

    assert_eq!(error.code(), TextStringErrorCode::Cancelled);
    assert!(cancellation.probes() >= 3);
}

#[test]
fn decoded_value_and_error_debug_are_source_redacted() {
    let secret = "outline-title-private";
    let decoded = decode_text_string(
        &pdf_string(secret.as_bytes()),
        TextStringLimits::default(),
        &NeverCancelled,
    )
    .expect("secret test text decodes");
    let decoded_debug = format!("{decoded:?}");
    assert!(decoded_debug.contains("REDACTED"));
    assert!(!decoded_debug.contains(secret));

    let mut invalid = secret.as_bytes().to_vec();
    invalid.push(0x7f);
    let error_debug = format!("{:?}", failed(&invalid));
    assert!(!error_debug.contains(secret));
}
