use pdf_rs_content::{
    ContentNumber, ContentVmErrorCategory, ContentVmErrorCode, ContentVmRecoverability,
};

fn scaled(raw: &[u8]) -> i64 {
    ContentNumber::parse(raw)
        .expect("fixture must be exactly representable")
        .scaled()
}

fn error(raw: &[u8]) -> pdf_rs_content::ContentVmError {
    ContentNumber::parse(raw).expect_err("fixture must be rejected")
}

#[test]
fn parses_integer_decimal_and_exponent_forms_without_floating_point() {
    let cases: &[(&[u8], i64)] = &[
        (b"0", 0),
        (b"-0", 0),
        (b"+0.000000000", 0),
        (b"1", 1_000_000_000),
        (b"-1", -1_000_000_000),
        (b".5", 500_000_000),
        (b"5.", 5_000_000_000),
        (b"1.25e2", 125_000_000_000),
        (b"125E-2", 1_250_000_000),
        (b"+10e-10", 1),
        (b"-10e-10", -1),
    ];
    for (raw, expected) in cases {
        assert_eq!(scaled(raw), *expected, "fixture: {raw:?}");
    }

    assert_eq!(
        ContentNumber::from_integer(9_223_372_036)
            .expect("integer fits fixed-point range")
            .scaled(),
        9_223_372_036_000_000_000
    );
}

#[test]
fn trailing_zeros_are_eliminated_before_precision_and_overflow_checks() {
    let cases: &[(&[u8], i64)] = &[
        (b"1.0000000000", 1_000_000_000),
        (b"100e-11", 1),
        (b"1000000000000000000000000000e-27", 1_000_000_000),
        (b"92233720368547758070e-10", i64::MAX),
        (b"-92233720368547758080e-10", i64::MIN),
    ];
    for (raw, expected) in cases {
        assert_eq!(scaled(raw), *expected, "fixture: {raw:?}");
    }
}

#[test]
fn exponent_extremes_are_stable_and_zero_normalizes_before_range_classification() {
    assert_eq!(
        scaled(b"0e999999999999999999999999999999999999999999999999"),
        0
    );
    assert_eq!(
        scaled(b"-0e-999999999999999999999999999999999999999999999999"),
        0
    );
    assert_eq!(
        scaled(b"1e000000000000000000000000000000000000000000000000"),
        1_000_000_000
    );

    assert_eq!(
        error(b"1e999999999999999999999999999999999999999999999999").code(),
        ContentVmErrorCode::NumericOverflow
    );
    assert_eq!(
        error(b"1e-999999999999999999999999999999999999999999999999").code(),
        ContentVmErrorCode::NumericPrecision
    );
}

#[test]
fn signed_i64_boundaries_are_exact_and_one_unit_beyond_is_overflow() {
    assert_eq!(scaled(b"9223372036.854775807"), i64::MAX);
    assert_eq!(scaled(b"-9223372036.854775808"), i64::MIN);
    assert_eq!(
        error(b"9223372036.854775808").code(),
        ContentVmErrorCode::NumericOverflow
    );
    assert_eq!(
        error(b"-9223372036.854775809").code(),
        ContentVmErrorCode::NumericOverflow
    );
    assert_eq!(
        ContentNumber::from_integer(i64::MAX)
            .expect_err("scaled integer overflows")
            .code(),
        ContentVmErrorCode::NumericOverflow
    );
}

#[test]
fn invalid_precision_and_overflow_outcomes_are_distinct() {
    for raw in [
        b"".as_slice(),
        b"+",
        b"-",
        b".",
        b"e1",
        b"1e",
        b"1e+",
        b"1.2.3",
        b"1e2x",
        b"--1",
        b" 1",
    ] {
        assert_eq!(
            error(raw).code(),
            ContentVmErrorCode::InvalidNumber,
            "fixture: {raw:?}"
        );
    }
    for raw in [
        b"0.0000000001".as_slice(),
        b"1e-10",
        b"100e-12",
        b"1.0000000001",
    ] {
        assert_eq!(
            error(raw).code(),
            ContentVmErrorCode::NumericPrecision,
            "fixture: {raw:?}"
        );
    }
    for raw in [
        b"9223372037".as_slice(),
        b"-9223372037",
        b"99999999999999999999999999999999999999",
        b"1e100",
    ] {
        assert_eq!(
            error(raw).code(),
            ContentVmErrorCode::NumericOverflow,
            "fixture: {raw:?}"
        );
    }
}

#[test]
fn numeric_errors_have_stable_policy_and_number_debug_is_redacted() {
    let invalid = error(b"invalid");
    assert_eq!(invalid.category(), ContentVmErrorCategory::Numeric);
    assert_eq!(
        invalid.recoverability(),
        ContentVmRecoverability::CorrectInput
    );
    assert_eq!(invalid.diagnostic_id(), "RPE-CONTENT-VM-0004");
    assert_eq!(invalid.source(), None);
    assert_eq!(invalid.limit(), None);
    assert_eq!(invalid.to_string(), "RPE-CONTENT-VM-0004");

    let number = ContentNumber::parse(b"1234.5").expect("fixture is exact");
    let debug = format!("{number:?}");
    assert!(debug.contains("ContentNumber"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("1234"));
    assert!(!debug.contains("1234500000000"));
}
