use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pdf_rs_content::{
    ContentCancellation, ContentError, ContentErrorCode, ContentLimitConfig, ContentLimitKind,
    ContentLimits, ContentOperand, ContentOperator, ContentScanJob, ContentScanPhase,
    ContentScanPoll, ContentStringKind, DecodedContentStream, NeverCancelled, OperatorContext,
    OperatorFailurePolicy, OperatorKind, OperatorOperandShape, scan_content_streams,
};
use pdf_rs_syntax::ObjectRef;

fn object(number: u32) -> ObjectRef {
    ObjectRef::new(number, 0).expect("test object reference is valid")
}

fn streams<'a>(values: &[&'a [u8]]) -> Vec<DecodedContentStream<'a>> {
    values
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            DecodedContentStream::new(
                object(u32::try_from(index + 1).expect("test stream count fits u32")),
                u32::try_from(index).expect("test stream count fits u32"),
                bytes,
            )
        })
        .collect()
}

fn scan(values: &[&[u8]]) -> Result<pdf_rs_content::ContentProgram, ContentError> {
    let inputs = streams(values);
    scan_content_streams(&inputs, ContentLimits::default(), &NeverCancelled)
}

fn failed(values: &[&[u8]]) -> ContentError {
    scan(values).expect_err("fixture must fail")
}

#[test]
fn scans_owned_operands_known_operators_and_exact_provenance() {
    let program = scan(&[
        b"% prefix\nnull true false -12 1.25 /A#20B (a\\n\\(b\\)) <4142F> \
          [1 /N] << /K (v) >> Custom q Q 1 0 0 1 2 3 cm BT ET BX EX \
          /Tag MP /Tag << /MCID 7 >> DP /Tag BMC /Tag <<>> BDC EMC",
    ])
    .expect("fixture scans");
    let operators = program.operators();
    assert_eq!(operators.len(), 13);
    assert!(matches!(
        operators[0].operator(),
        ContentOperator::Unknown(token) if token == b"Custom"
    ));
    assert_eq!(operators[0].operands().len(), 10);
    assert_eq!(operators[0].source().page_operator_ordinal(), 0);
    assert_eq!(operators[0].source().span().object(), object(1));
    assert_eq!(
        operators[1].operator().known(),
        Some(OperatorKind::SaveGraphicsState)
    );
    assert_eq!(
        operators[2].operator().known(),
        Some(OperatorKind::RestoreGraphicsState)
    );
    assert_eq!(
        operators[3].operator().known(),
        Some(OperatorKind::ConcatMatrix)
    );
    assert_eq!(operators[3].operands().len(), 6);
    assert_eq!(
        operators
            .last()
            .expect("operator")
            .source()
            .page_operator_ordinal(),
        12
    );

    assert!(matches!(
        operators[0].operands()[0].value(),
        ContentOperand::Null
    ));
    assert!(matches!(
        operators[0].operands()[1].value(),
        ContentOperand::Boolean(true)
    ));
    assert!(matches!(
        operators[0].operands()[3].value(),
        ContentOperand::Integer(-12)
    ));
    assert!(matches!(
        operators[0].operands()[4].value(),
        ContentOperand::Real(value) if value.raw() == b"1.25"
    ));
    assert!(matches!(
        operators[0].operands()[5].value(),
        ContentOperand::Name(value) if value.bytes() == b"A B"
    ));
    assert!(matches!(
        operators[0].operands()[6].value(),
        ContentOperand::String(value)
            if value.kind() == ContentStringKind::Literal && value.bytes() == b"a\n(b)"
    ));
    assert!(matches!(
        operators[0].operands()[7].value(),
        ContentOperand::String(value)
            if value.kind() == ContentStringKind::Hexadecimal
                && value.bytes() == [0x41, 0x42, 0xf0]
    ));
    assert!(matches!(
        operators[0].operands()[8].value(),
        ContentOperand::Array(values) if values.len() == 2
    ));
    assert!(matches!(
        operators[0].operands()[9].value(),
        ContentOperand::Dictionary(entries)
            if entries.len() == 1 && entries[0].key().bytes() == b"K"
    ));
    assert_eq!(program.stats().operators(), 13);
    assert_eq!(program.stats().unknown_operators(), 1);
    assert!(program.stats().retained_bytes() > 0);
}

#[test]
fn stream_boundaries_are_semantic_whitespace_but_groups_and_containers_continue() {
    let inputs = [
        DecodedContentStream::new(object(10), 0, b"1 2 [3"),
        DecodedContentStream::new(object(11), 1, b"4]"),
        DecodedContentStream::new(object(12), 2, b"cm q"),
    ];
    let program =
        scan_content_streams(&inputs, ContentLimits::default(), &NeverCancelled).expect("scan");
    assert_eq!(program.operators().len(), 2);
    let first = &program.operators()[0];
    assert_eq!(first.operands().len(), 3);
    assert!(matches!(
        first.operands()[2].value(),
        ContentOperand::Array(values) if values.len() == 2
    ));
    assert_eq!(first.source().span().object(), object(12));
    assert_eq!(first.source().span().stream_ordinal(), 2);
    assert_eq!(first.source().span().decoded_start(), 0);
    assert_eq!(first.source().span().decoded_len(), 2);
    let array_extent = first.operands()[2].extent();
    assert_eq!(array_extent.start().object(), object(10));
    assert_eq!(array_extent.end_exclusive().object(), object(11));
    assert!(array_extent.single_stream_span().is_none());
    assert_eq!(program.operators()[1].source().page_operator_ordinal(), 1);
}

#[test]
fn lexical_tokens_and_strings_never_join_across_stream_boundaries() {
    let split_operator = scan(&[b"Fo", b"o"]).expect("each regular fragment is an operator");
    assert_eq!(split_operator.operators().len(), 2);
    assert_eq!(split_operator.operators()[0].operator().token(), b"Fo");
    assert_eq!(split_operator.operators()[1].operator().token(), b"o");

    let split_name = scan(&[b"/A", b"B"]).expect("boundary terminates the name token");
    assert_eq!(split_name.operators().len(), 1);
    assert!(matches!(
        split_name.operators()[0].operands()[0].value(),
        ContentOperand::Name(name) if name.bytes() == b"A"
    ));
    assert_eq!(split_name.operators()[0].operator().token(), b"B");

    assert_eq!(
        failed(&[b"(unterminated", b") q"]).code(),
        ContentErrorCode::UnterminatedString
    );
    assert_eq!(
        failed(&[b"<41", b"> q"]).code(),
        ContentErrorCode::UnterminatedString
    );
}

#[test]
fn comments_whitespace_and_literal_escape_rules_are_deterministic() {
    let program = scan(&[
        b"\0\t\x0c\r\n 1% comment without shared boundary newline",
        b"2 (a\\\r\nb\\053\\q\r\nc) <41 4> Op",
    ])
    .expect("scan");
    let operation = &program.operators()[0];
    assert_eq!(operation.operands().len(), 4);
    assert!(matches!(
        operation.operands()[2].value(),
        ContentOperand::String(value) if value.bytes() == b"ab+q\nc"
    ));
    assert!(matches!(
        operation.operands()[3].value(),
        ContentOperand::String(value) if value.bytes() == b"A@"
    ));
}

#[test]
fn unknown_operators_remain_distinct_from_malformed_input() {
    let program = scan(&[b"foo Bar_17"]).expect("regular tokens are valid unknown operators");
    assert_eq!(program.operators().len(), 2);
    assert!(
        program
            .operators()
            .iter()
            .all(|operator| operator.operator().is_unknown())
    );

    let cases: &[(&[u8], ContentErrorCode)] = &[
        (b"12x foo", ContentErrorCode::InvalidNumber),
        (b"]", ContentErrorCode::MismatchedDelimiter),
        (b"/A#G0 foo", ContentErrorCode::InvalidNameEscape),
        (b"(abc", ContentErrorCode::UnterminatedString),
        (b"<0G> foo", ContentErrorCode::InvalidHexString),
        (b"<< 1 2 >> foo", ContentErrorCode::InvalidDictionaryKey),
        (b"[1", ContentErrorCode::MismatchedDelimiter),
        (b"1", ContentErrorCode::DanglingOperands),
        (b"{", ContentErrorCode::MalformedToken),
    ];
    for (bytes, code) in cases {
        assert_eq!(failed(&[bytes]).code(), *code, "fixture: {bytes:?}");
    }
}

#[test]
fn known_operator_table_declares_token_arity_context_and_cost() {
    let expected = [
        (
            OperatorKind::SaveGraphicsState,
            b"q".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::Any,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::RestoreGraphicsState,
            b"Q".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::Any,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::ConcatMatrix,
            b"cm".as_slice(),
            6,
            OperatorOperandShape::SixNumbers,
            OperatorContext::Any,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::BeginText,
            b"BT".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::TextObjectBoundary,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::EndText,
            b"ET".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::TextObjectBoundary,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::BeginCompatibility,
            b"BX".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::CompatibilityBoundary,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::EndCompatibility,
            b"EX".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::CompatibilityBoundary,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::MarkedContentPoint,
            b"MP".as_slice(),
            1,
            OperatorOperandShape::Name,
            OperatorContext::MarkedContent,
            OperatorFailurePolicy::ValidateThenUnsupported,
        ),
        (
            OperatorKind::MarkedContentPointProperties,
            b"DP".as_slice(),
            2,
            OperatorOperandShape::NameAndNameOrDictionary,
            OperatorContext::MarkedContent,
            OperatorFailurePolicy::ValidateThenUnsupported,
        ),
        (
            OperatorKind::BeginMarkedContent,
            b"BMC".as_slice(),
            1,
            OperatorOperandShape::Name,
            OperatorContext::MarkedContent,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::BeginMarkedContentProperties,
            b"BDC".as_slice(),
            2,
            OperatorOperandShape::NameAndNameOrDictionary,
            OperatorContext::MarkedContent,
            OperatorFailurePolicy::Execute,
        ),
        (
            OperatorKind::EndMarkedContent,
            b"EMC".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::MarkedContent,
            OperatorFailurePolicy::Execute,
        ),
    ];
    for (kind, token, operands, operand_shape, context, failure_policy) in expected {
        let spec = kind.spec();
        assert_eq!(spec.token(), token);
        assert_eq!(spec.min_operands(), operands);
        assert_eq!(spec.max_operands(), operands);
        assert_eq!(spec.operand_shape(), operand_shape);
        assert_eq!(spec.context(), context);
        assert_eq!(spec.failure_policy(), failure_policy);
        assert!(spec.base_fuel() > 0);
    }
}

#[test]
fn m3_graphics_operator_table_is_exact_and_scanner_classifies_every_token() {
    let expected = [
        (
            OperatorKind::MoveTo,
            b"m".as_slice(),
            2,
            OperatorOperandShape::TwoNumbers,
            OperatorContext::PathConstruction,
            3,
        ),
        (
            OperatorKind::LineTo,
            b"l".as_slice(),
            2,
            OperatorOperandShape::TwoNumbers,
            OperatorContext::PathConstruction,
            3,
        ),
        (
            OperatorKind::CubicCurveTo,
            b"c".as_slice(),
            6,
            OperatorOperandShape::SixNumbers,
            OperatorContext::PathConstruction,
            7,
        ),
        (
            OperatorKind::CubicCurveToReplicateInitial,
            b"v".as_slice(),
            4,
            OperatorOperandShape::FourNumbers,
            OperatorContext::PathConstruction,
            5,
        ),
        (
            OperatorKind::CubicCurveToReplicateFinal,
            b"y".as_slice(),
            4,
            OperatorOperandShape::FourNumbers,
            OperatorContext::PathConstruction,
            5,
        ),
        (
            OperatorKind::ClosePath,
            b"h".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathConstruction,
            1,
        ),
        (
            OperatorKind::Rectangle,
            b"re".as_slice(),
            4,
            OperatorOperandShape::FourNumbers,
            OperatorContext::PathConstruction,
            5,
        ),
        (
            OperatorKind::StrokePath,
            b"S".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::CloseAndStrokePath,
            b"s".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::FillNonzero,
            b"f".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::FillNonzeroLegacy,
            b"F".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::FillEvenOdd,
            b"f*".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::FillStrokeNonzero,
            b"B".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::FillStrokeEvenOdd,
            b"B*".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::CloseFillStrokeNonzero,
            b"b".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::CloseFillStrokeEvenOdd,
            b"b*".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::EndPath,
            b"n".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::PathPainting,
            1,
        ),
        (
            OperatorKind::ClipNonzero,
            b"W".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::ClippingPath,
            1,
        ),
        (
            OperatorKind::ClipEvenOdd,
            b"W*".as_slice(),
            0,
            OperatorOperandShape::None,
            OperatorContext::ClippingPath,
            1,
        ),
        (
            OperatorKind::SetLineWidth,
            b"w".as_slice(),
            1,
            OperatorOperandShape::OneNumber,
            OperatorContext::LineState,
            2,
        ),
        (
            OperatorKind::SetLineCap,
            b"J".as_slice(),
            1,
            OperatorOperandShape::OneInteger,
            OperatorContext::LineState,
            2,
        ),
        (
            OperatorKind::SetLineJoin,
            b"j".as_slice(),
            1,
            OperatorOperandShape::OneInteger,
            OperatorContext::LineState,
            2,
        ),
        (
            OperatorKind::SetMiterLimit,
            b"M".as_slice(),
            1,
            OperatorOperandShape::OneNumber,
            OperatorContext::LineState,
            2,
        ),
        (
            OperatorKind::SetLineDash,
            b"d".as_slice(),
            2,
            OperatorOperandShape::NumberArrayAndNumber,
            OperatorContext::LineState,
            3,
        ),
        (
            OperatorKind::SetStrokingGray,
            b"G".as_slice(),
            1,
            OperatorOperandShape::OneNumber,
            OperatorContext::DeviceColor,
            2,
        ),
        (
            OperatorKind::SetNonstrokingGray,
            b"g".as_slice(),
            1,
            OperatorOperandShape::OneNumber,
            OperatorContext::DeviceColor,
            2,
        ),
        (
            OperatorKind::SetStrokingRgb,
            b"RG".as_slice(),
            3,
            OperatorOperandShape::ThreeNumbers,
            OperatorContext::DeviceColor,
            4,
        ),
        (
            OperatorKind::SetNonstrokingRgb,
            b"rg".as_slice(),
            3,
            OperatorOperandShape::ThreeNumbers,
            OperatorContext::DeviceColor,
            4,
        ),
        (
            OperatorKind::SetStrokingCmyk,
            b"K".as_slice(),
            4,
            OperatorOperandShape::FourNumbers,
            OperatorContext::DeviceColor,
            5,
        ),
        (
            OperatorKind::SetNonstrokingCmyk,
            b"k".as_slice(),
            4,
            OperatorOperandShape::FourNumbers,
            OperatorContext::DeviceColor,
            5,
        ),
    ];

    let program = scan(&[b"1 2 m 3 4 l 1 2 3 4 5 6 c 1 2 3 4 v 1 2 3 4 y h \
          1 2 3 4 re S s f F f* B B* b b* n W W* \
          2 w 1 J 2 j 10 M [3 4] 1 d \
          .5 G .25 g 1 0 .5 RG 0 1 .5 rg 0 1 .5 .25 K 1 0 .5 .25 k"])
    .expect("registered M3 graphics operators scan");
    assert_eq!(program.operators().len(), expected.len());

    for (operator, (kind, token, operands, operand_shape, context, fuel)) in
        program.operators().iter().zip(expected)
    {
        assert_eq!(operator.operator().known(), Some(kind));
        assert_eq!(operator.operator().token(), token);
        assert_eq!(operator.operands().len(), operands);

        let spec = kind.spec();
        assert_eq!(spec.token(), token);
        assert_eq!(
            spec.min_operands(),
            u8::try_from(operands).expect("test arity fits")
        );
        assert_eq!(
            spec.max_operands(),
            u8::try_from(operands).expect("test arity fits")
        );
        assert_eq!(spec.operand_shape(), operand_shape);
        assert_eq!(spec.context(), context);
        assert_eq!(spec.failure_policy(), OperatorFailurePolicy::Execute);
        assert_eq!(spec.base_fuel(), fuel);
    }
    assert_eq!(program.stats().unknown_operators(), 0);
}

fn budget_fixture() -> Vec<DecodedContentStream<'static>> {
    streams(&[b"[[1] 2] 3 VeryLongUnknown", b"4 5 6 q"])
}

fn exact_config(stats: pdf_rs_content::ContentScanStats) -> ContentLimitConfig {
    ContentLimitConfig {
        max_streams: stats.streams(),
        max_total_decoded_bytes: stats.total_decoded_bytes(),
        max_tokens: stats.tokens(),
        max_token_bytes: stats.max_token_bytes(),
        max_operands_per_operator: stats.max_operands_per_operator(),
        max_nesting_depth: stats.max_nesting_depth(),
        max_operators: stats.operators(),
        max_fuel: stats.fuel(),
        max_retained_bytes: stats.retained_bytes(),
    }
}

fn assert_one_less(
    baseline: ContentLimitConfig,
    kind: ContentLimitKind,
    mutate: impl FnOnce(&mut ContentLimitConfig),
) {
    let mut config = baseline;
    mutate(&mut config);
    let limits = ContentLimits::validate(config).expect("one-less config remains valid");
    let inputs = budget_fixture();
    let error = scan_content_streams(&inputs, limits, &NeverCancelled)
        .expect_err("one-less budget must fail");
    assert_eq!(error.code(), ContentErrorCode::ResourceLimit);
    assert_eq!(error.limit().expect("limit context").kind(), kind);
}

#[test]
fn every_budget_accepts_exact_and_rejects_one_less() {
    let inputs = budget_fixture();
    let baseline =
        scan_content_streams(&inputs, ContentLimits::default(), &NeverCancelled).expect("baseline");
    let exact = exact_config(baseline.stats());
    let exact_limits = ContentLimits::validate(exact).expect("observed exact limits are valid");
    let exact_program =
        scan_content_streams(&inputs, exact_limits, &NeverCancelled).expect("exact budgets pass");
    assert_eq!(exact_program.stats(), baseline.stats());

    assert_one_less(exact, ContentLimitKind::Streams, |value| {
        value.max_streams -= 1;
    });
    assert_one_less(exact, ContentLimitKind::TotalDecodedBytes, |value| {
        value.max_total_decoded_bytes -= 1;
    });
    assert_one_less(exact, ContentLimitKind::Tokens, |value| {
        value.max_tokens -= 1;
    });
    assert_one_less(exact, ContentLimitKind::TokenBytes, |value| {
        value.max_token_bytes -= 1;
    });
    assert_one_less(exact, ContentLimitKind::OperandsPerOperator, |value| {
        value.max_operands_per_operator -= 1;
    });
    assert_one_less(exact, ContentLimitKind::NestingDepth, |value| {
        value.max_nesting_depth -= 1;
    });
    assert_one_less(exact, ContentLimitKind::Operators, |value| {
        value.max_operators -= 1;
    });
    assert_one_less(exact, ContentLimitKind::Fuel, |value| {
        value.max_fuel -= 1;
    });
    assert_one_less(exact, ContentLimitKind::RetainedBytes, |value| {
        value.max_retained_bytes -= 1;
    });
}

#[test]
fn cancellation_and_both_terminal_outcomes_replay_without_more_work() {
    let inputs = streams(&[b"1 2 cm q"]);
    let cancelled = AtomicBool::new(true);
    let mut failed_job =
        ContentScanJob::new(&inputs, ContentLimits::default()).expect("job admission");
    let first = match failed_job.poll(&cancelled) {
        ContentScanPoll::Failed(error) => error,
        ContentScanPoll::Ready(_) => panic!("pre-cancelled scan must fail"),
    };
    assert_eq!(first.code(), ContentErrorCode::Cancelled);
    assert_eq!(failed_job.phase(), ContentScanPhase::Failed);
    let failed_stats = failed_job.stats();
    cancelled.store(false, Ordering::Release);
    let second = match failed_job.poll(&cancelled) {
        ContentScanPoll::Failed(error) => error,
        ContentScanPoll::Ready(_) => panic!("terminal failure must replay"),
    };
    assert_eq!(second, first);
    assert_eq!(failed_job.stats(), failed_stats);

    let mut ready_job =
        ContentScanJob::new(&inputs, ContentLimits::default()).expect("job admission");
    let first = match ready_job.poll(&NeverCancelled) {
        ContentScanPoll::Ready(program) => program,
        ContentScanPoll::Failed(error) => panic!("scan failed: {error}"),
    };
    let ready_stats = ready_job.stats();
    let second = match ready_job.poll(&cancelled) {
        ContentScanPoll::Ready(program) => program,
        ContentScanPoll::Failed(error) => panic!("ready replay failed: {error}"),
    };
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(ready_job.stats(), ready_stats);
    assert_eq!(ready_job.phase(), ContentScanPhase::Ready);
}

struct CancelAfterChecks {
    checks: AtomicUsize,
    cancel_at: usize,
}

impl ContentCancellation for CancelAfterChecks {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::AcqRel) + 1 >= self.cancel_at
    }
}

#[test]
fn long_trivia_observes_cooperative_cancellation_before_publication() {
    let mut decoded = vec![b'%'];
    decoded.extend(std::iter::repeat_n(b'a', 700));
    decoded.extend_from_slice(b"\nq");
    let inputs = [DecodedContentStream::new(object(1), 0, &decoded)];
    let cancellation = CancelAfterChecks {
        checks: AtomicUsize::new(0),
        cancel_at: 4,
    };
    let mut job = ContentScanJob::new(&inputs, ContentLimits::default()).expect("job admission");
    let error = match job.poll(&cancellation) {
        ContentScanPoll::Failed(error) => error,
        ContentScanPoll::Ready(_) => panic!("mid-scan cancellation must prevent publication"),
    };
    assert_eq!(error.code(), ContentErrorCode::Cancelled);
    assert!(job.stats().fuel() >= 256);
    assert!(job.stats().fuel() < 700);
    let stats = job.stats();
    let replay = match job.poll(&NeverCancelled) {
        ContentScanPoll::Failed(error) => error,
        ContentScanPoll::Ready(_) => panic!("cancelled job must remain terminal"),
    };
    assert_eq!(replay, error);
    assert_eq!(job.stats(), stats);
}

fn semantic_operand(value: &ContentOperand) -> String {
    match value {
        ContentOperand::Null => "null".into(),
        ContentOperand::Boolean(value) => value.to_string(),
        ContentOperand::Integer(value) => value.to_string(),
        ContentOperand::Real(value) => String::from_utf8_lossy(value.raw()).into_owned(),
        ContentOperand::Name(value) => format!("/{}", hex(value.bytes())),
        ContentOperand::String(value) => format!("s{}", hex(value.bytes())),
        ContentOperand::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|value| semantic_operand(value.value()))
                .collect::<Vec<_>>()
                .join(",")
        ),
        ContentOperand::Dictionary(entries) => format!(
            "<<{}>>",
            entries
                .iter()
                .map(|entry| format!(
                    "{}={}",
                    hex(entry.key().bytes()),
                    semantic_operand(entry.value().value())
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

fn semantic_program(program: &pdf_rs_content::ContentProgram) -> Vec<(Vec<String>, Vec<u8>)> {
    program
        .operators()
        .iter()
        .map(|operator| {
            (
                operator
                    .operands()
                    .iter()
                    .map(|value| semantic_operand(value.value()))
                    .collect(),
                operator.operator().token().to_vec(),
            )
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::new();
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[test]
fn split_merge_metamorphism_holds_when_no_token_crosses_a_boundary() {
    let merged = scan(&[b"[1 2] /T (x) Foo q"]).expect("merged scan");
    let split = scan(&[b"[1", b" 2] /T", b" (x) Foo q"]).expect("split scan");
    assert_eq!(semantic_program(&merged), semantic_program(&split));
}

#[test]
fn repeated_scans_and_debug_output_are_deterministic_and_redacted() {
    let inputs = streams(&[b"(super-secret) /private Unknown"]);
    let first =
        scan_content_streams(&inputs, ContentLimits::default(), &NeverCancelled).expect("scan");
    let second =
        scan_content_streams(&inputs, ContentLimits::default(), &NeverCancelled).expect("scan");
    assert_eq!(first, second);
    assert_eq!(first.stats(), second.stats());
    let debug = format!("{first:?}");
    assert!(!debug.contains("super-secret"));
    assert!(!debug.contains("private"));
    assert!(!debug.contains("Unknown"));

    let malformed = failed(&[b"/secret#xx Op"]);
    let error_debug = format!("{malformed:?}");
    assert!(!error_debug.contains("secret"));
}

#[test]
fn stream_order_and_limit_configuration_are_validated() {
    let invalid = [DecodedContentStream::new(object(1), 1, b"q")];
    let error = ContentScanJob::new(&invalid, ContentLimits::default())
        .err()
        .expect("invalid order");
    assert_eq!(error.code(), ContentErrorCode::InvalidStreamOrder);

    let config = ContentLimitConfig {
        max_tokens: 0,
        ..ContentLimitConfig::default()
    };
    let error = ContentLimits::validate(config).expect_err("zero limit");
    assert_eq!(error.code(), ContentErrorCode::InvalidLimits);
}
