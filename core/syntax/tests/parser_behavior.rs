use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{
    ByteRange, ByteSource, JobId, RangeResponse, RangeStore, ReadPoll, ReadRequest,
    RequestPriority, ResumeCheckpoint, SourceIdentity, SourceRevision, SourceSnapshot,
    SourceStableId, SourceValidator, SourceValidatorKind,
};
use pdf_rs_syntax::{
    DictionaryEntry, InputExtent, Located, NeverCancelled, ObjectRef, PdfDictionary, RealNotation,
    StringKind, SyntaxCancellation, SyntaxError, SyntaxErrorCategory, SyntaxErrorCode, SyntaxInput,
    SyntaxLimitConfig, SyntaxLimitKind, SyntaxLimits, SyntaxObject, SyntaxParser, SyntaxPoll,
    SyntaxRecoverability,
};

const BASE: u64 = 100;

fn identity() -> SourceIdentity {
    SourceIdentity::new(SourceStableId::new([0x5a; 32]), SourceRevision::new(7))
}

fn input<'a>(bytes: &'a [u8], base: u64, end_of_input: bool) -> SyntaxInput<'a> {
    SyntaxInput::new(
        identity(),
        base,
        bytes,
        if end_of_input {
            InputExtent::KnownSourceEnd
        } else {
            InputExtent::MayContinue
        },
    )
    .expect("test input is valid")
}

fn parser<'a>(bytes: &'a [u8], end_of_input: bool) -> SyntaxParser<'a> {
    parser_at(bytes, 0, end_of_input, SyntaxLimits::default())
}

fn parser_at<'a>(
    bytes: &'a [u8],
    base: u64,
    end_of_input: bool,
    limits: SyntaxLimits,
) -> SyntaxParser<'a> {
    SyntaxParser::new(input(bytes, base, end_of_input), limits).expect("parser input fits limits")
}

fn retained_parser<'a>(bytes: &'a [u8], max_retained_bytes: u64) -> SyntaxParser<'a> {
    SyntaxParser::new_with_cancellation_and_retained_cap(
        input(bytes, 0, true),
        SyntaxLimits::default(),
        &NeverCancelled,
        max_retained_bytes,
    )
    .expect("parser input fits limits")
}

fn ready<T>(poll: SyntaxPoll<T>) -> T {
    match poll {
        SyntaxPoll::Ready(value) => value,
        SyntaxPoll::NeedMore { .. } => panic!("expected ready value, got NeedMore"),
        SyntaxPoll::EndOfInput => panic!("expected ready value, got EndOfInput"),
        SyntaxPoll::Failed(error) => panic!("expected ready value, got {error}"),
    }
}

fn failed<T>(poll: SyntaxPoll<T>) -> SyntaxError {
    match poll {
        SyntaxPoll::Failed(error) => error,
        SyntaxPoll::Ready(_) => panic!("expected failure, got ready value"),
        SyntaxPoll::NeedMore { .. } => panic!("expected failure, got NeedMore"),
        SyntaxPoll::EndOfInput => panic!("expected failure, got EndOfInput"),
    }
}

fn object(bytes: &[u8]) -> Located<SyntaxObject> {
    ready(parser(bytes, true).parse_object())
}

fn dictionary(value: &Located<SyntaxObject>) -> &PdfDictionary {
    value
        .value()
        .as_dictionary()
        .expect("test object is a dictionary")
}

fn allocator_capacity_bytes<T: Clone>(template: &T, len: usize) -> u64 {
    let mut probe = Vec::<T>::new();
    for _ in 0..len {
        probe
            .try_reserve(1)
            .expect("small test vector allocation succeeds");
        probe.push(template.clone());
    }
    u64::try_from(probe.capacity())
        .expect("test vector capacity fits u64")
        .checked_mul(u64::try_from(std::mem::size_of::<T>()).expect("element size fits u64"))
        .expect("small test vector capacity bytes fit u64")
}

fn array_capacity_bytes(len: usize) -> u64 {
    allocator_capacity_bytes(&object(b"null"), len)
}

fn dictionary_capacity_bytes(len: usize) -> u64 {
    let parsed = object(b"<< /Template null >>");
    let template: &DictionaryEntry = &dictionary(&parsed).entries()[0];
    allocator_capacity_bytes(template, len)
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

impl SyntaxCancellation for CancelOnProbe {
    fn is_cancelled(&self) -> bool {
        self.probes.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_at
    }
}

#[test]
fn located_try_map_preserves_the_bound_source_and_span() {
    let located = object(b"<< /Answer 42 >>");
    let source = located.source();
    let span = located.span();
    let mapped = located
        .try_map(|value| match value {
            SyntaxObject::Dictionary(dictionary) => Ok::<_, ()>(dictionary),
            _ => Err(()),
        })
        .expect("the parsed object is a dictionary");

    assert_eq!(mapped.source(), source);
    assert_eq!(mapped.span(), span);
    assert_eq!(
        mapped
            .value()
            .get(b"Answer")
            .and_then(|value| value.value().as_integer()),
        Some(42)
    );
}

#[test]
fn cancellation_has_stable_terminal_policy_metadata() {
    let cancellation = CancelOnProbe::new(1);
    let mut parser = SyntaxParser::new_with_cancellation(
        input(b"null", BASE, true),
        SyntaxLimits::default(),
        &cancellation,
    )
    .expect("valid bounded input constructs a parser");

    let error = failed(parser.parse_object());
    assert_eq!(error.code(), SyntaxErrorCode::Cancelled);
    assert_eq!(error.category(), SyntaxErrorCategory::Cancellation);
    assert_eq!(
        error.recoverability(),
        SyntaxRecoverability::AbandonOperation
    );
    assert_eq!(error.diagnostic_id(), "RPE-SYNTAX-0017");
    assert_eq!(error.offset(), Some(BASE));
    assert_eq!(cancellation.probes(), 1);
}

#[test]
fn long_scanners_probe_cancellation_at_a_fixed_interval() {
    let mut source = vec![b'a'; 600];
    source.push(b' ');
    let cancellation = CancelOnProbe::new(2);
    let mut parser = SyntaxParser::new_with_cancellation(
        input(&source, BASE, true),
        SyntaxLimits::default(),
        &cancellation,
    )
    .expect("valid bounded input constructs a parser");

    let error = failed(parser.parse_object());
    assert_eq!(error.code(), SyntaxErrorCode::Cancelled);
    assert_eq!(error.category(), SyntaxErrorCategory::Cancellation);
    assert_eq!(cancellation.probes(), 2);
    assert!(error.offset().is_some_and(|offset| offset > BASE));
}

fn configured(mut update: impl FnMut(&mut SyntaxLimitConfig)) -> SyntaxLimits {
    let mut config = SyntaxLimitConfig::default();
    update(&mut config);
    SyntaxLimits::validate(config).expect("test syntax limits are internally consistent")
}

#[test]
fn headers_are_versioned_source_located_and_bounded() {
    let mut pdf_17 = parser_at(b"%PDF-1.7", BASE, true, SyntaxLimits::default());
    let header = ready(pdf_17.parse_header());
    assert_eq!(header.source(), identity());
    assert_eq!(header.span().start(), BASE);
    assert_eq!(header.span().end_exclusive(), BASE + 8);
    assert_eq!((header.value().major(), header.value().minor()), (1, 7));
    assert_eq!(pdf_17.position(), BASE + 8);
    assert_eq!(pdf_17.remaining(), 0);

    let header = ready(parser(b"%PDF-2.0", true).parse_header());
    assert_eq!((header.value().major(), header.value().minor()), (2, 0));

    for unsupported in [b"%PDF-1.8".as_slice(), b"%PDF-2.1", b"not-pdf!"] {
        assert_eq!(
            failed(parser(unsupported, true).parse_header()).code(),
            SyntaxErrorCode::InvalidHeader
        );
    }
}

#[test]
fn incomplete_header_requests_more_but_final_truncation_is_stable() {
    match parser_at(b"%PDF-1.", BASE, false, SyntaxLimits::default()).parse_header() {
        SyntaxPoll::NeedMore { minimum_end } => assert!(minimum_end > BASE + 7),
        _ => panic!("a non-final partial header must request more bytes"),
    }

    let error = failed(parser_at(b"%PDF-1.", BASE, true, SyntaxLimits::default()).parse_header());
    assert_eq!(error.code(), SyntaxErrorCode::InvalidHeader);
    assert_eq!(error.offset(), Some(BASE));
    assert_eq!(error.diagnostic_id(), "RPE-SYNTAX-0002");
}

#[test]
fn whitespace_and_comments_are_skipped_without_losing_absolute_spans() {
    let mut syntax_parser = parser_at(b" \t%note\r\nnull", BASE, true, SyntaxLimits::default());
    let parsed = ready(syntax_parser.parse_object());
    assert_eq!(parsed.source(), identity());
    assert_eq!(parsed.span().start(), BASE + 9);
    assert_eq!(parsed.span().end_exclusive(), BASE + 13);
    assert_eq!(parsed.value(), &SyntaxObject::Null);
    assert_eq!(syntax_parser.position(), BASE + 13);
    assert_eq!(syntax_parser.remaining(), 0);

    assert!(matches!(
        parser(b"%comment without line ending", false).parse_object(),
        SyntaxPoll::NeedMore { .. }
    ));
    assert!(matches!(
        parser(b"%a final comment may end at EOF", true).parse_object(),
        SyntaxPoll::EndOfInput
    ));
}

#[test]
fn integers_reals_and_exponents_keep_their_semantics() {
    assert_eq!(object(b"-17").value().as_integer(), Some(-17));
    assert_eq!(object(b"+0").value().as_integer(), Some(0));

    let decimal = object(b"+3.50");
    let SyntaxObject::Real(decimal) = decimal.value() else {
        panic!("decimal token must produce a real object");
    };
    assert_eq!(decimal.raw(), b"+3.50");
    assert_eq!(decimal.notation(), RealNotation::Decimal);

    let exponent = object(b"6.02E+23");
    let SyntaxObject::Real(exponent) = exponent.value() else {
        panic!("exponent token must produce a real object");
    };
    assert_eq!(exponent.raw(), b"6.02E+23");
    assert_eq!(exponent.notation(), RealNotation::Exponent);

    assert_eq!(
        failed(parser(b"1e+", true).parse_object()).code(),
        SyntaxErrorCode::InvalidNumber
    );
    assert_eq!(
        failed(parser(b"9223372036854775808", true).parse_object()).code(),
        SyntaxErrorCode::IntegerOutOfRange
    );
}

#[test]
fn indirect_references_are_recognized_without_consuming_plain_integers() {
    assert_eq!(
        ObjectRef::new(0, 0)
            .expect_err("object zero is reserved")
            .code(),
        SyntaxErrorCode::InvalidReference
    );
    assert_eq!(ObjectRef::new(1, 0).unwrap().number(), 1);

    let reference = object(b"12 34 R");
    let reference = reference
        .value()
        .as_reference()
        .expect("three-token reference is recognized");
    assert_eq!(reference.number(), 12);
    assert_eq!(reference.generation(), 34);

    let mut plain = parser(b"12 34 false", true);
    assert_eq!(ready(plain.parse_object()).value().as_integer(), Some(12));
    assert_eq!(plain.position(), 2);
    assert_eq!(ready(plain.parse_object()).value().as_integer(), Some(34));
    assert_eq!(plain.position(), 5);
    assert_eq!(
        ready(plain.parse_object()).value(),
        &SyntaxObject::Boolean(false)
    );

    assert!(matches!(
        parser(b"12 0", false).parse_object(),
        SyntaxPoll::NeedMore { .. }
    ));
    assert_eq!(
        ready(parser(b"1 (", false).parse_object())
            .value()
            .as_integer(),
        Some(1)
    );
    assert_eq!(
        ready(parser(b"1 .", false).parse_object())
            .value()
            .as_integer(),
        Some(1)
    );
    let mut final_plain = parser(b"12 0", true);
    assert_eq!(
        ready(final_plain.parse_object()).value().as_integer(),
        Some(12)
    );
    assert_eq!(
        ready(final_plain.parse_object()).value().as_integer(),
        Some(0)
    );

    assert_eq!(
        failed(parser(b"1 65536 R", true).parse_object()).code(),
        SyntaxErrorCode::InvalidReference
    );
}

#[test]
fn names_decode_hex_escapes_and_preserve_non_utf8_bytes() {
    let parsed = object(b"/A#20B#ff");
    let SyntaxObject::Name(name) = parsed.value() else {
        panic!("name token must produce a name object");
    };
    assert_eq!(name.bytes(), b"A B\xff");
    assert_eq!(parsed.span().start(), 0);
    assert_eq!(parsed.span().end_exclusive(), 9);

    assert_eq!(
        failed(parser(b"/bad#G0", true).parse_object()).code(),
        SyntaxErrorCode::InvalidNameEscape
    );
    assert!(matches!(
        parser(b"/partial#", false).parse_object(),
        SyntaxPoll::NeedMore { .. }
    ));
}

#[test]
fn literal_strings_handle_nesting_escapes_octal_and_line_continuation() {
    let parsed = object(b"(line\\n\\053\\\r\n\\(x\\))");
    let SyntaxObject::String(value) = parsed.value() else {
        panic!("literal token must produce a string object");
    };
    assert_eq!(value.kind(), StringKind::Literal);
    assert_eq!(value.bytes(), b"line\n+(x)");

    let nested = object(b"(a(b)c)");
    let SyntaxObject::String(value) = nested.value() else {
        panic!("nested literal must produce a string object");
    };
    assert_eq!(value.bytes(), b"a(b)c");

    let normalized = object(b"(a\r\nb\rc\n)");
    let SyntaxObject::String(value) = normalized.value() else {
        panic!("line-ending example must produce a string object");
    };
    assert_eq!(value.bytes(), b"a\nb\nc\n");
}

#[test]
fn hexadecimal_strings_ignore_whitespace_and_pad_an_odd_nibble() {
    let parsed = object(b"<41 4>");
    let SyntaxObject::String(value) = parsed.value() else {
        panic!("hex token must produce a string object");
    };
    assert_eq!(value.kind(), StringKind::Hexadecimal);
    assert_eq!(value.bytes(), &[0x41, 0x40]);

    assert_eq!(
        failed(parser(b"<4G>", true).parse_object()).code(),
        SyntaxErrorCode::InvalidHexString
    );
}

#[test]
fn arrays_keep_order_and_nested_source_locations() {
    let source = b"[null true false 42 /N (s)]";
    let parsed = object(source);
    let SyntaxObject::Array(array) = parsed.value() else {
        panic!("array delimiters must produce an array object");
    };
    assert_eq!(array.values().len(), 6);
    assert_eq!(array.values()[0].value(), &SyntaxObject::Null);
    assert_eq!(array.values()[1].value(), &SyntaxObject::Boolean(true));
    assert_eq!(array.values()[2].value(), &SyntaxObject::Boolean(false));
    assert_eq!(array.values()[3].value().as_integer(), Some(42));
    assert_eq!(array.values()[3].span().start(), 17);
    assert_eq!(
        parsed.span().end_exclusive(),
        u64::try_from(source.len()).unwrap()
    );
}

#[test]
fn dictionaries_preserve_duplicates_and_last_value_lookup() {
    let parsed = object(b"<< /A 1 /A 2 /Ref 9 0 R >>");
    let dictionary = dictionary(&parsed);
    assert_eq!(dictionary.entries().len(), 3);
    assert_eq!(dictionary.entries()[0].key().value().bytes(), b"A");
    assert_eq!(dictionary.entries()[1].key().value().bytes(), b"A");
    assert_eq!(
        dictionary
            .get(b"A")
            .and_then(|value| value.value().as_integer()),
        Some(2)
    );
    let reference = dictionary
        .get(b"Ref")
        .and_then(|value| value.value().as_reference())
        .expect("reference value is available through dictionary lookup");
    assert_eq!((reference.number(), reference.generation()), (9, 0));
    assert!(dictionary.get(b"Missing").is_none());
}

#[test]
fn compound_delimiters_and_dictionary_keys_are_strict() {
    assert_eq!(
        failed(parser(b"[1 >>", true).parse_object()).code(),
        SyntaxErrorCode::MismatchedDelimiter
    );
    assert_eq!(
        failed(parser(b"<< 1 2 >>", true).parse_object()).code(),
        SyntaxErrorCode::UnexpectedToken
    );
}

#[test]
fn compound_truncation_distinguishes_retryable_windows_from_final_input() {
    for partial in [
        b"[1".as_slice(),
        b"<< /A 1".as_slice(),
        b"(unterminated".as_slice(),
        b"<4142".as_slice(),
    ] {
        assert!(matches!(
            parser(partial, false).parse_object(),
            SyntaxPoll::NeedMore { .. }
        ));
    }

    assert_eq!(
        failed(parser(b"[1", true).parse_object()).code(),
        SyntaxErrorCode::UnexpectedEndOfInput
    );
    assert_eq!(
        failed(parser(b"<< /A 1", true).parse_object()).code(),
        SyntaxErrorCode::UnexpectedEndOfInput
    );
    assert_eq!(
        failed(parser(b"(unterminated", true).parse_object()).code(),
        SyntaxErrorCode::UnterminatedLiteralString
    );
    assert_eq!(
        failed(parser(b"<4142", true).parse_object()).code(),
        SyntaxErrorCode::InvalidHexString
    );
}

#[test]
fn keywords_stream_boundaries_and_raw_bytes_share_one_cursor() {
    let mut syntax_parser = parser_at(
        b" % framing\nstream\r\nABCD",
        BASE,
        true,
        SyntaxLimits::default(),
    );
    let keyword = ready(syntax_parser.parse_keyword());
    assert_eq!(keyword.source(), identity());
    assert_eq!(keyword.span().start(), BASE + 11);
    assert_eq!(keyword.span().end_exclusive(), BASE + 17);
    assert_eq!(keyword.bytes(), b"stream");
    assert_eq!(syntax_parser.stats().tokens(), 1);
    assert_eq!(syntax_parser.stats().owned_bytes(), 0);
    assert_eq!(syntax_parser.stats().container_entries(), 0);
    assert_eq!(syntax_parser.stats().max_depth(), 0);
    assert_eq!(syntax_parser.stats().input_bytes(), 23);

    let line_ending = ready(syntax_parser.consume_stream_line_ending());
    assert_eq!(line_ending.start(), BASE + 17);
    assert_eq!(line_ending.end_exclusive(), BASE + 19);

    let raw = ready(syntax_parser.take_raw_bytes(4));
    assert_eq!(raw.source(), identity());
    assert_eq!(raw.span().start(), BASE + 19);
    assert_eq!(raw.span().end_exclusive(), BASE + 23);
    assert_eq!(raw.bytes(), b"ABCD");
    assert_eq!(syntax_parser.position(), BASE + 23);
    assert_eq!(syntax_parser.remaining(), 0);

    let empty = ready(syntax_parser.take_raw_bytes(0));
    assert!(empty.span().is_empty());
    assert!(empty.bytes().is_empty());

    let mut lf = parser(b"stream\n", true);
    ready(lf.expect_keyword(b"stream"));
    let ending = ready(lf.consume_stream_line_ending());
    assert_eq!(ending.len(), 1);

    let mut bad = parser(b"stream\rX", true);
    ready(bad.expect_keyword(b"stream"));
    assert_eq!(
        failed(bad.consume_stream_line_ending()).code(),
        SyntaxErrorCode::InvalidStreamBoundary
    );
}

#[test]
fn borrowed_keyword_distinguishes_incomplete_and_non_keyword_input() {
    assert!(matches!(
        parser_at(b"", BASE, false, SyntaxLimits::default()).parse_keyword(),
        SyntaxPoll::NeedMore {
            minimum_end: value
        } if value == BASE + 1
    ));
    let error = failed(parser_at(b"", BASE, true, SyntaxLimits::default()).parse_keyword());
    assert_eq!(error.code(), SyntaxErrorCode::UnexpectedEndOfInput);
    assert_eq!(error.offset(), Some(BASE));

    let mut partial = parser_at(b"endst", BASE, false, SyntaxLimits::default());
    assert!(matches!(
        partial.parse_keyword(),
        SyntaxPoll::NeedMore {
            minimum_end: value
        } if value == BASE + 6
    ));
    assert_eq!(partial.position(), BASE);

    let mut final_keyword = parser_at(b"endstream", BASE, true, SyntaxLimits::default());
    assert_eq!(ready(final_keyword.parse_keyword()).bytes(), b"endstream");

    for bytes in [
        b"/classified".as_slice(),
        b"(classified)".as_slice(),
        b"1.25".as_slice(),
    ] {
        let mut non_keyword = parser_at(bytes, BASE, true, SyntaxLimits::default());
        let error = failed(non_keyword.parse_keyword());
        assert_eq!(error.code(), SyntaxErrorCode::UnexpectedToken);
        assert_eq!(error.offset(), Some(BASE));
        assert_eq!(non_keyword.stats().tokens(), 1);
        assert_eq!(non_keyword.stats().owned_bytes(), 0);
        assert_eq!(non_keyword.position(), BASE);
    }
}

#[test]
fn borrowed_keyword_honors_cancellation_and_token_limits() {
    let mut long_keyword = vec![b'k'; 600];
    long_keyword.push(b' ');
    let cancellation = CancelOnProbe::new(2);
    let mut cancelled = SyntaxParser::new_with_cancellation(
        input(&long_keyword, BASE, true),
        SyntaxLimits::default(),
        &cancellation,
    )
    .expect("valid bounded input constructs a parser");
    let error = failed(cancelled.parse_keyword());
    assert_eq!(error.code(), SyntaxErrorCode::Cancelled);
    assert_eq!(cancellation.probes(), 2);
    assert_eq!(cancelled.stats().owned_bytes(), 0);

    let limits = configured(|config| {
        config.max_token_bytes = 4;
        config.max_comment_bytes = 4;
        config.max_name_bytes = 4;
    });
    assert_eq!(
        ready(parser_at(b"word ", BASE, true, limits).parse_keyword()).bytes(),
        b"word"
    );
    let error = failed(parser_at(b"words ", BASE, true, limits).parse_keyword());
    let limit = error.limit().expect("token failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::TokenBytes);
    assert_eq!((limit.limit(), limit.attempted()), (4, 5));
}

#[test]
fn raw_byte_truncation_is_retryable_only_before_final_input() {
    let mut partial = parser_at(b"AB", BASE, false, SyntaxLimits::default());
    match partial.take_raw_bytes(4) {
        SyntaxPoll::NeedMore { minimum_end } => assert_eq!(minimum_end, BASE + 4),
        _ => panic!("non-final raw data must request its exact required end"),
    }
    assert_eq!(partial.position(), BASE);

    let error = failed(parser_at(b"AB", BASE, true, SyntaxLimits::default()).take_raw_bytes(4));
    assert_eq!(error.code(), SyntaxErrorCode::UnexpectedEndOfInput);
    assert_eq!(error.offset(), Some(BASE + 2));
}

#[test]
fn byte_slice_inputs_keep_the_store_identity_and_range() {
    let snapshot = SourceSnapshot::new(
        identity(),
        Some(104),
        SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x33; 32]),
    );
    let range = ByteRange::new(BASE, 4).unwrap();
    let store = RangeStore::new(snapshot, Default::default()).unwrap();
    store
        .supply(RangeResponse::new(snapshot, range, b"null".to_vec()).unwrap())
        .unwrap();
    let slice = match store.poll(ReadRequest::new(
        range,
        RequestPriority::Metadata,
        JobId::new(1),
        ResumeCheckpoint::new(1),
    )) {
        ReadPoll::Ready(slice) => slice,
        _ => panic!("supplied range must be immediately readable"),
    };

    let syntax_input = SyntaxInput::from_byte_slice(&slice, InputExtent::KnownSourceEnd)
        .expect("ByteSlice geometry is already checked");
    let mut parser = SyntaxParser::new(syntax_input, SyntaxLimits::default()).unwrap();
    let parsed = ready(parser.parse_object());
    assert_eq!(parsed.source(), identity());
    assert_eq!(parsed.span().start(), BASE);
    assert_eq!(parsed.span().end_exclusive(), BASE + 4);
}

#[test]
fn input_and_token_limits_fail_with_structured_context() {
    let input_limits = configured(|config| {
        config.max_input_bytes = 8;
        config.max_token_bytes = 8;
        config.max_comment_bytes = 8;
        config.max_name_bytes = 8;
        config.max_string_source_bytes = 8;
    });
    assert!(SyntaxParser::new(input(b"12345678", BASE, true), input_limits).is_ok());
    let error = match SyntaxParser::new(input(b"123456789", BASE, true), input_limits) {
        Err(error) => error,
        Ok(_) => panic!("nine input bytes exceed an eight-byte window"),
    };
    let limit = error.limit().expect("resource errors carry limit context");
    assert_eq!(error.code(), SyntaxErrorCode::ResourceLimit);
    assert_eq!(limit.kind(), SyntaxLimitKind::InputBytes);
    assert_eq!(
        (limit.limit(), limit.consumed(), limit.attempted()),
        (8, 0, 9)
    );

    let error = failed(parser_at(b"12345678", BASE, false, input_limits).parse_object());
    let limit = error
        .limit()
        .expect("an unsatisfiable NeedMore carries input context");
    assert_eq!(limit.kind(), SyntaxLimitKind::InputBytes);
    assert_eq!((limit.limit(), limit.attempted()), (8, 9));

    let error = failed(parser_at(b"", BASE, false, input_limits).take_raw_bytes(9));
    let limit = error
        .limit()
        .expect("an oversized raw request carries input context");
    assert_eq!(limit.kind(), SyntaxLimitKind::InputBytes);
    assert_eq!((limit.limit(), limit.attempted()), (8, 9));

    let token_limits = configured(|config| {
        config.max_token_bytes = 4;
        config.max_comment_bytes = 4;
        config.max_name_bytes = 4;
    });
    assert_eq!(
        ready(parser_at(b"1234 ", BASE, true, token_limits).parse_object())
            .value()
            .as_integer(),
        Some(1234)
    );
    let error = failed(parser_at(b"12345 ", BASE, true, token_limits).parse_object());
    let limit = error.limit().expect("token failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::TokenBytes);
    assert_eq!(limit.limit(), 4);
    assert_eq!(error.offset(), Some(BASE));
}

#[test]
fn comment_name_and_string_limits_are_enforced_at_the_boundary() {
    let comment_limits = configured(|config| config.max_comment_bytes = 4);
    assert_eq!(
        ready(parser_at(b"%123\nnull", 0, true, comment_limits).parse_object()).value(),
        &SyntaxObject::Null
    );
    assert_eq!(
        failed(parser_at(b"%1234\nnull", 0, true, comment_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::CommentBytes
    );

    let name_limits = configured(|config| config.max_name_bytes = 3);
    let name = ready(parser_at(b"/ABC ", 0, true, name_limits).parse_object());
    let SyntaxObject::Name(name) = name.value() else {
        panic!("boundary-sized name remains valid");
    };
    assert_eq!(name.bytes(), b"ABC");
    assert_eq!(
        failed(parser_at(b"/ABCD ", 0, true, name_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::NameBytes
    );

    let source_limits = configured(|config| config.max_string_source_bytes = 5);
    ready(parser_at(b"(abc)", 0, true, source_limits).parse_object());
    assert_eq!(
        failed(parser_at(b"(abcd)", 0, true, source_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::StringSourceBytes
    );

    let decoded_limits = configured(|config| config.max_string_decoded_bytes = 3);
    ready(parser_at(b"(abc)", 0, true, decoded_limits).parse_object());
    assert_eq!(
        failed(parser_at(b"(abcd)", 0, true, decoded_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::StringDecodedBytes
    );
}

#[test]
fn owned_scalar_budget_is_checked_before_retaining_real_lexemes() {
    let owned_limits = configured(|config| {
        config.max_name_bytes = 1;
        config.max_string_decoded_bytes = 1;
        config.max_owned_bytes = 4;
    });
    let accepted = ready(parser_at(b"1.25 ", 0, true, owned_limits).parse_object());
    let SyntaxObject::Real(accepted) = accepted.value() else {
        panic!("boundary-sized real lexeme remains valid");
    };
    assert_eq!(accepted.raw(), b"1.25");

    let error = failed(parser_at(b"12.25 ", 0, true, owned_limits).parse_object());
    let limit = error.limit().expect("owned-byte failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::OwnedBytes);
    assert_eq!(limit.limit(), 4);
}

#[test]
fn token_entry_and_depth_budgets_are_cumulative() {
    let token_limits = configured(|config| config.max_total_tokens = 3);
    assert_eq!(
        failed(parser_at(b"[1 2]", 0, true, token_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::Tokens
    );

    let entry_limits = configured(|config| config.max_container_entries = 1);
    assert!(matches!(
        parser_at(b"<< /A null >", 0, false, entry_limits).parse_object(),
        SyntaxPoll::NeedMore { .. }
    ));
    assert_eq!(
        failed(parser_at(b"[1 2]", 0, true, entry_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::ContainerEntries
    );
    let mut precharged = parser_at(b"[1 (classified)]", 0, true, entry_limits);
    assert_eq!(
        failed(precharged.parse_object()).limit().unwrap().kind(),
        SyntaxLimitKind::ContainerEntries
    );
    assert_eq!(
        precharged.stats().owned_bytes(),
        0,
        "entry fuel is rejected before the next child allocates"
    );
    let mut precharged_real = parser_at(b"[0 2.123456789]", 0, true, entry_limits);
    assert_eq!(
        failed(precharged_real.parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::ContainerEntries
    );
    assert_eq!(
        precharged_real.stats().owned_bytes(),
        0,
        "reference lookahead never allocates an unrelated real token"
    );

    let depth_limits = configured(|config| config.max_container_depth = 1);
    assert_eq!(
        failed(parser_at(b"[[null]]", 0, true, depth_limits).parse_object())
            .limit()
            .unwrap()
            .kind(),
        SyntaxLimitKind::ContainerDepth
    );
}

#[test]
fn container_bytes_match_allocator_reported_array_and_dictionary_capacity() {
    let mut empty_array = parser(b"[]", true);
    ready(empty_array.parse_object());
    assert_eq!(empty_array.stats().container_bytes(), 0);

    let mut empty_dictionary = parser(b"<<>>", true);
    ready(empty_dictionary.parse_object());
    assert_eq!(empty_dictionary.stats().container_bytes(), 0);

    let mut array = parser(b"[null null null null null]", true);
    ready(array.parse_object());
    assert_eq!(array.stats().container_bytes(), array_capacity_bytes(5));

    let mut dictionary = parser(b"<< /A null /B null /C null /D null /E null >>", true);
    ready(dictionary.parse_object());
    assert_eq!(
        dictionary.stats().container_bytes(),
        dictionary_capacity_bytes(5)
    );
    assert!(dictionary.stats().owned_bytes() > 0);
}

#[test]
fn container_capacity_does_not_consume_the_scalar_owned_byte_budget() {
    let limits = configured(|config| {
        config.max_name_bytes = 1;
        config.max_string_decoded_bytes = 1;
        config.max_owned_bytes = 1;
    });
    let mut syntax_parser = parser_at(b"[null null null null null]", 0, true, limits);
    ready(syntax_parser.parse_object());

    assert_eq!(syntax_parser.stats().owned_bytes(), 0);
    assert_eq!(
        syntax_parser.stats().container_bytes(),
        array_capacity_bytes(5)
    );
    assert!(syntax_parser.stats().container_bytes() > limits.max_owned_bytes());
}

#[test]
fn container_capacity_budget_uses_allocator_reported_bytes() {
    let exact_bytes = array_capacity_bytes(5);
    let exact_limits = configured(|config| config.max_container_bytes = exact_bytes);
    let mut exact = parser_at(b"[null null null null null]", 0, true, exact_limits);
    ready(exact.parse_object());
    assert_eq!(exact.stats().container_bytes(), exact_bytes);

    let one_less_limits = configured(|config| config.max_container_bytes = exact_bytes - 1);
    let mut one_less = parser_at(b"[null null null null null]", 0, true, one_less_limits);
    let limit = failed(one_less.parse_object())
        .limit()
        .expect("container-byte failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::ContainerBytes);
    assert_eq!(limit.limit(), exact_bytes - 1);
    assert!(limit.consumed().checked_add(limit.attempted()).unwrap() > limit.limit());
    assert_eq!(one_less.stats().container_bytes(), limit.consumed());

    let element_bytes = u64::try_from(std::mem::size_of::<Located<SyntaxObject>>()).unwrap();
    let initial_bytes = array_capacity_bytes(4);
    let one_element_remaining = initial_bytes.checked_add(element_bytes).unwrap();
    let preflight_limits = configured(|config| config.max_container_bytes = one_element_remaining);
    let mut preflight = parser_at(b"[null null null null null]", 0, true, preflight_limits);
    let limit = failed(preflight.parse_object())
        .limit()
        .expect("preflight container-byte failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::ContainerBytes);
    assert_eq!(limit.limit(), one_element_remaining);
    assert_eq!(limit.consumed(), initial_bytes);
    assert_eq!(limit.attempted(), initial_bytes);
    assert_eq!(preflight.stats().container_bytes(), initial_bytes);
}

#[test]
fn runtime_retained_cap_uses_owned_allocator_capacity_at_exact_and_one_less_boundaries() {
    let bytes = b"/AllocatorVisible ";
    let mut baseline = parser(bytes, true);
    ready(baseline.parse_object());
    let exact_bytes = baseline.stats().owned_bytes();
    assert!(exact_bytes > 0);
    assert_eq!(baseline.stats().container_bytes(), 0);

    let mut exact = retained_parser(bytes, exact_bytes);
    ready(exact.parse_object());
    assert_eq!(exact.stats().owned_bytes(), exact_bytes);

    let mut one_less = retained_parser(bytes, exact_bytes - 1);
    let limit = failed(one_less.parse_object())
        .limit()
        .expect("retained failure carries allocator-capacity context");
    assert_eq!(limit.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(limit.limit(), exact_bytes - 1);
    assert_eq!(limit.consumed(), 0);
    assert!(limit.attempted() >= exact_bytes);
    assert_eq!(one_less.stats().owned_bytes(), 0);
}

#[test]
fn runtime_retained_cap_preflights_and_rechecks_container_capacity() {
    let bytes = b"[null null null null null]";
    let mut baseline = parser(bytes, true);
    ready(baseline.parse_object());
    let exact_bytes = baseline.stats().container_bytes();
    assert!(exact_bytes > 0);
    assert_eq!(baseline.stats().owned_bytes(), 0);

    let mut exact = retained_parser(bytes, exact_bytes);
    ready(exact.parse_object());
    assert_eq!(exact.stats().container_bytes(), exact_bytes);

    let mut one_less = retained_parser(bytes, exact_bytes - 1);
    let limit = failed(one_less.parse_object())
        .limit()
        .expect("retained failure carries container-capacity context");
    assert_eq!(limit.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(limit.limit(), exact_bytes - 1);
    assert!(limit.consumed().checked_add(limit.attempted()).unwrap() > limit.limit());
    assert_eq!(
        one_less.stats().owned_bytes() + one_less.stats().container_bytes(),
        limit.consumed(),
        "a rejected capacity is never committed to retained statistics"
    );
}

#[test]
fn runtime_retained_cap_combines_owned_and_container_capacity() {
    let bytes = b"[/A (BC)]";
    let mut baseline = parser(bytes, true);
    ready(baseline.parse_object());
    let exact_bytes = baseline
        .stats()
        .owned_bytes()
        .checked_add(baseline.stats().container_bytes())
        .unwrap();
    assert!(baseline.stats().owned_bytes() > 0);
    assert!(baseline.stats().container_bytes() > 0);

    let mut exact = retained_parser(bytes, exact_bytes);
    ready(exact.parse_object());
    assert_eq!(
        exact.stats().owned_bytes() + exact.stats().container_bytes(),
        exact_bytes
    );

    let mut one_less = retained_parser(bytes, exact_bytes - 1);
    let limit = failed(one_less.parse_object()).limit().unwrap();
    assert_eq!(limit.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(limit.limit(), exact_bytes - 1);
    assert!(limit.consumed().checked_add(limit.attempted()).unwrap() > limit.limit());
}

#[test]
fn zero_runtime_retained_cap_allows_scalars_and_legacy_constructors_remain_uncapped() {
    let mut scalar = retained_parser(b"null", 0);
    ready(scalar.parse_object());
    assert_eq!(scalar.stats().owned_bytes(), 0);
    assert_eq!(scalar.stats().container_bytes(), 0);

    let error = failed(retained_parser(b"/A", 0).parse_object());
    let limit = error.limit().unwrap();
    assert_eq!(limit.kind(), SyntaxLimitKind::RetainedBytes);
    assert_eq!(
        (limit.limit(), limit.consumed(), limit.attempted()),
        (0, 0, 1)
    );

    let bytes = b"[/A (BC)]";
    let mut legacy = SyntaxParser::new(input(bytes, 0, true), SyntaxLimits::default()).unwrap();
    let legacy_value = ready(legacy.parse_object());
    let mut legacy_with_cancellation = SyntaxParser::new_with_cancellation(
        input(bytes, 0, true),
        SyntaxLimits::default(),
        &NeverCancelled,
    )
    .unwrap();
    assert_eq!(ready(legacy_with_cancellation.parse_object()), legacy_value);
    assert_eq!(legacy_with_cancellation.stats(), legacy.stats());
}

#[test]
fn nested_container_bytes_sum_each_retained_vector_capacity_once() {
    let mut syntax_parser = parser(
        b"[ [null null] << /A null /B null >> [<< /C null >>] ]",
        true,
    );
    ready(syntax_parser.parse_object());

    let expected = array_capacity_bytes(3)
        .checked_add(array_capacity_bytes(2))
        .and_then(|value| value.checked_add(dictionary_capacity_bytes(2)))
        .and_then(|value| value.checked_add(array_capacity_bytes(1)))
        .and_then(|value| value.checked_add(dictionary_capacity_bytes(1)))
        .expect("small nested capacity sum fits u64");
    assert_eq!(syntax_parser.stats().container_bytes(), expected);
    assert_eq!(syntax_parser.stats().container_entries(), 9);
    assert_eq!(syntax_parser.stats().max_depth(), 3);
}

#[test]
fn container_capacity_accounting_preserves_entry_limits_and_retry_scope() {
    let exact_limits = configured(|config| config.max_container_entries = 2);
    let mut exact = parser_at(b"[null null]", 0, true, exact_limits);
    ready(exact.parse_object());
    assert_eq!(exact.stats().container_bytes(), array_capacity_bytes(2));

    let one_less_limits = configured(|config| config.max_container_entries = 1);
    let mut one_less = parser_at(b"[null null]", 0, true, one_less_limits);
    let limit = failed(one_less.parse_object())
        .limit()
        .expect("entry-limit failure carries context");
    assert_eq!(limit.kind(), SyntaxLimitKind::ContainerEntries);
    assert_eq!(limit.consumed(), 1);
    assert_eq!(limit.attempted(), 1);
    assert_eq!(one_less.stats().container_bytes(), array_capacity_bytes(1));

    let mut incomplete = parser(b"[null", false);
    assert!(matches!(
        incomplete.parse_object(),
        SyntaxPoll::NeedMore { .. }
    ));
    assert_eq!(
        incomplete.stats().container_bytes(),
        array_capacity_bytes(1)
    );

    let mut retried = parser(b"[null null]", true);
    ready(retried.parse_object());
    assert_eq!(
        retried.stats().container_bytes(),
        array_capacity_bytes(2),
        "the successful retry reports only the vector retained by that parser"
    );
}

#[test]
fn statistics_report_bounded_work_without_exposing_content() {
    let source = b"<< /Secret (classified) /Items [1 2] >>";
    let mut syntax_parser = parser(source, true);
    let parsed = ready(syntax_parser.parse_object());
    let stats = syntax_parser.stats();
    assert_eq!(stats.input_bytes(), source.len() as u64);
    assert!(stats.tokens() >= 9);
    assert!(stats.owned_bytes() >= b"SecretclassifiedItems".len() as u64);
    assert_eq!(
        stats.container_bytes(),
        dictionary_capacity_bytes(2) + array_capacity_bytes(2)
    );
    assert_eq!(stats.container_entries(), 4);
    assert_eq!(stats.max_depth(), 2);

    let debug = format!("{parsed:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("Secret"));
    assert!(!debug.contains("classified"));

    let stats_debug = format!("{stats:?}");
    assert!(!stats_debug.contains("Secret"));
    assert!(!stats_debug.contains("classified"));

    let mut raw_parser = parser(b"classified", true);
    let raw = ready(raw_parser.take_raw_bytes(10));
    let raw_debug = format!("{raw:?}");
    assert!(raw_debug.contains("[REDACTED]"));
    assert!(!raw_debug.contains("classified"));
}

#[test]
fn invalid_limit_profiles_are_rejected_deterministically() {
    let zero = SyntaxLimitConfig {
        max_total_tokens: 0,
        ..SyntaxLimitConfig::default()
    };
    let error = SyntaxLimits::validate(zero).expect_err("zero budgets are invalid");
    assert_eq!(error.code(), SyntaxErrorCode::InvalidLimits);
    assert_eq!(error.diagnostic_id(), "RPE-SYNTAX-0001");

    let zero_container_bytes = SyntaxLimitConfig {
        max_container_bytes: 0,
        ..SyntaxLimitConfig::default()
    };
    assert_eq!(
        SyntaxLimits::validate(zero_container_bytes)
            .unwrap_err()
            .code(),
        SyntaxErrorCode::InvalidLimits
    );
    let excessive_container_bytes = SyntaxLimitConfig {
        max_container_bytes: 256 * 1_024 * 1_024 + 1,
        ..SyntaxLimitConfig::default()
    };
    assert_eq!(
        SyntaxLimits::validate(excessive_container_bytes)
            .unwrap_err()
            .code(),
        SyntaxErrorCode::InvalidLimits
    );

    let mut inconsistent = SyntaxLimitConfig::default();
    inconsistent.max_token_bytes = inconsistent.max_input_bytes + 1;
    assert_eq!(
        SyntaxLimits::validate(inconsistent).unwrap_err().code(),
        SyntaxErrorCode::InvalidLimits
    );
}
