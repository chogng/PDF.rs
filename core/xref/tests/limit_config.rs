use pdf_rs_xref::{XrefErrorCode, XrefLimitConfig, XrefLimits};

const MIB: u64 = 1024 * 1024;
const HARD_MAX_SOURCE_BYTES: u64 = 1024 * MIB;
const HARD_MAX_TAIL_BYTES: u64 = 4 * MIB;
const HARD_MAX_SECTION_BYTES: u64 = 64 * MIB;
const HARD_MAX_TOTAL_BYTES: u64 = 256 * MIB;
const HARD_MAX_SUBSECTIONS: u64 = 65_536;
const HARD_MAX_ENTRIES: u64 = 4_000_000;

type ConfigMutation = (&'static str, fn(&mut XrefLimitConfig));

fn minimum_config() -> XrefLimitConfig {
    XrefLimitConfig {
        max_source_bytes: 1,
        initial_tail_bytes: 1,
        max_tail_bytes: 1,
        initial_section_bytes: 1,
        max_section_bytes: 1,
        max_total_read_bytes: 2,
        max_total_parse_bytes: 2,
        max_subsections: 1,
        max_entries: 1,
    }
}

fn hard_ceiling_config() -> XrefLimitConfig {
    XrefLimitConfig {
        max_source_bytes: HARD_MAX_SOURCE_BYTES,
        initial_tail_bytes: HARD_MAX_TAIL_BYTES,
        max_tail_bytes: HARD_MAX_TAIL_BYTES,
        initial_section_bytes: HARD_MAX_SECTION_BYTES,
        max_section_bytes: HARD_MAX_SECTION_BYTES,
        max_total_read_bytes: HARD_MAX_TOTAL_BYTES,
        max_total_parse_bytes: HARD_MAX_TOTAL_BYTES,
        max_subsections: HARD_MAX_SUBSECTIONS,
        max_entries: HARD_MAX_ENTRIES,
    }
}

fn assert_invalid(label: &str, config: XrefLimitConfig) {
    let error = match XrefLimits::validate(config) {
        Ok(_) => panic!("{label} unexpectedly passed validation"),
        Err(error) => error,
    };
    assert_eq!(error.code(), XrefErrorCode::InvalidLimits, "{label}");
}

fn assert_getters(limits: XrefLimits, expected: XrefLimitConfig) {
    assert_eq!(limits.max_source_bytes(), expected.max_source_bytes);
    assert_eq!(limits.initial_tail_bytes(), expected.initial_tail_bytes);
    assert_eq!(limits.max_tail_bytes(), expected.max_tail_bytes);
    assert_eq!(
        limits.initial_section_bytes(),
        expected.initial_section_bytes
    );
    assert_eq!(limits.max_section_bytes(), expected.max_section_bytes);
    assert_eq!(limits.max_total_read_bytes(), expected.max_total_read_bytes);
    assert_eq!(
        limits.max_total_parse_bytes(),
        expected.max_total_parse_bytes
    );
    assert_eq!(limits.max_subsections(), expected.max_subsections);
    assert_eq!(limits.max_entries(), expected.max_entries);
}

#[test]
fn positive_minima_and_equality_relationships_are_accepted() {
    let minimum = minimum_config();
    assert_getters(XrefLimits::validate(minimum).unwrap(), minimum);

    let windows_equal_source = XrefLimitConfig {
        max_source_bytes: 64,
        initial_tail_bytes: 63,
        max_tail_bytes: 64,
        initial_section_bytes: 63,
        max_section_bytes: 64,
        max_total_read_bytes: 128,
        max_total_parse_bytes: 128,
        max_subsections: 2,
        max_entries: 2,
    };
    assert_getters(
        XrefLimits::validate(windows_equal_source).unwrap(),
        windows_equal_source,
    );

    let initial_windows_equal_maxima = XrefLimitConfig {
        initial_tail_bytes: 64,
        initial_section_bytes: 64,
        ..windows_equal_source
    };
    assert_getters(
        XrefLimits::validate(initial_windows_equal_maxima).unwrap(),
        initial_windows_equal_maxima,
    );
}

#[test]
fn hard_ceiling_minus_one_and_equality_are_accepted_and_round_trip() {
    let below = XrefLimitConfig {
        max_source_bytes: HARD_MAX_SOURCE_BYTES - 1,
        initial_tail_bytes: HARD_MAX_TAIL_BYTES - 1,
        max_tail_bytes: HARD_MAX_TAIL_BYTES - 1,
        initial_section_bytes: HARD_MAX_SECTION_BYTES - 1,
        max_section_bytes: HARD_MAX_SECTION_BYTES - 1,
        max_total_read_bytes: HARD_MAX_TOTAL_BYTES - 1,
        max_total_parse_bytes: HARD_MAX_TOTAL_BYTES - 1,
        max_subsections: HARD_MAX_SUBSECTIONS - 1,
        max_entries: HARD_MAX_ENTRIES - 1,
    };
    assert_getters(XrefLimits::validate(below).unwrap(), below);

    let equal = hard_ceiling_config();
    assert_getters(XrefLimits::validate(equal).unwrap(), equal);
}

#[test]
fn every_zero_field_is_rejected() {
    let cases: [ConfigMutation; 9] = [
        ("max_source_bytes", |config| config.max_source_bytes = 0),
        ("initial_tail_bytes", |config| config.initial_tail_bytes = 0),
        ("max_tail_bytes", |config| config.max_tail_bytes = 0),
        ("initial_section_bytes", |config| {
            config.initial_section_bytes = 0;
        }),
        ("max_section_bytes", |config| config.max_section_bytes = 0),
        ("max_total_read_bytes", |config| {
            config.max_total_read_bytes = 0;
        }),
        ("max_total_parse_bytes", |config| {
            config.max_total_parse_bytes = 0;
        }),
        ("max_subsections", |config| config.max_subsections = 0),
        ("max_entries", |config| config.max_entries = 0),
    ];

    for (label, mutate) in cases {
        let mut config = minimum_config();
        mutate(&mut config);
        assert_invalid(label, config);
    }
}

#[test]
fn inconsistent_window_and_total_relationships_are_rejected() {
    let cases = [
        (
            "initial tail exceeds maximum tail",
            XrefLimitConfig {
                initial_tail_bytes: 2,
                ..minimum_config()
            },
        ),
        (
            "maximum tail exceeds source",
            XrefLimitConfig {
                max_tail_bytes: 2,
                max_total_read_bytes: 3,
                max_total_parse_bytes: 3,
                ..minimum_config()
            },
        ),
        (
            "initial section exceeds maximum section",
            XrefLimitConfig {
                initial_section_bytes: 2,
                ..minimum_config()
            },
        ),
        (
            "maximum section exceeds source",
            XrefLimitConfig {
                max_section_bytes: 2,
                max_total_read_bytes: 3,
                max_total_parse_bytes: 3,
                ..minimum_config()
            },
        ),
        (
            "total read is below tail plus section",
            XrefLimitConfig {
                max_total_read_bytes: 1,
                ..minimum_config()
            },
        ),
        (
            "total parse is below tail plus section",
            XrefLimitConfig {
                max_total_parse_bytes: 1,
                ..minimum_config()
            },
        ),
    ];

    for (label, config) in cases {
        assert_invalid(label, config);
    }
}

#[test]
fn each_fixed_hard_ceiling_plus_one_is_rejected() {
    let cases: [ConfigMutation; 7] = [
        ("max_source_bytes", |config| {
            config.max_source_bytes = HARD_MAX_SOURCE_BYTES + 1;
        }),
        ("max_tail_bytes", |config| {
            config.max_tail_bytes = HARD_MAX_TAIL_BYTES + 1;
        }),
        ("max_section_bytes", |config| {
            config.max_section_bytes = HARD_MAX_SECTION_BYTES + 1;
        }),
        ("max_total_read_bytes", |config| {
            config.max_total_read_bytes = HARD_MAX_TOTAL_BYTES + 1;
        }),
        ("max_total_parse_bytes", |config| {
            config.max_total_parse_bytes = HARD_MAX_TOTAL_BYTES + 1;
        }),
        ("max_subsections", |config| {
            config.max_subsections = HARD_MAX_SUBSECTIONS + 1;
        }),
        ("max_entries", |config| {
            config.max_entries = HARD_MAX_ENTRIES + 1;
        }),
    ];

    for (label, mutate) in cases {
        let mut config = hard_ceiling_config();
        mutate(&mut config);
        assert_invalid(label, config);
    }
}

#[test]
fn extreme_values_are_rejected_without_arithmetic_wraparound() {
    assert_invalid(
        "all fields at u64::MAX",
        XrefLimitConfig {
            max_source_bytes: u64::MAX,
            initial_tail_bytes: u64::MAX,
            max_tail_bytes: u64::MAX,
            initial_section_bytes: u64::MAX,
            max_section_bytes: u64::MAX,
            max_total_read_bytes: u64::MAX,
            max_total_parse_bytes: u64::MAX,
            max_subsections: u64::MAX,
            max_entries: u64::MAX,
        },
    );
}
