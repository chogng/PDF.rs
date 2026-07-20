use pdf_rs_object::{ObjectErrorCode, ObjectLimitConfig, ObjectLimits};

const MIB: u64 = 1024 * 1024;
const HARD_MAX_SOURCE_BYTES: u64 = 1024 * MIB;
const HARD_MAX_ENVELOPE_BYTES: u64 = 64 * MIB;
const HARD_MAX_BOUNDARY_BYTES: u64 = 4 * MIB;
const HARD_MAX_STREAM_BYTES: u64 = 1024 * MIB;
const HARD_MAX_TOTAL_BYTES: u64 = 256 * MIB;

type ConfigMutation = (&'static str, fn(&mut ObjectLimitConfig));

fn minimum_config() -> ObjectLimitConfig {
    ObjectLimitConfig {
        max_source_bytes: 1,
        initial_envelope_bytes: 1,
        max_envelope_bytes: 1,
        initial_boundary_bytes: 1,
        max_boundary_bytes: 1,
        max_stream_bytes: 1,
        max_total_read_bytes: 2,
        max_total_parse_bytes: 2,
    }
}

fn hard_ceiling_config() -> ObjectLimitConfig {
    ObjectLimitConfig {
        max_source_bytes: HARD_MAX_SOURCE_BYTES,
        initial_envelope_bytes: HARD_MAX_ENVELOPE_BYTES,
        max_envelope_bytes: HARD_MAX_ENVELOPE_BYTES,
        initial_boundary_bytes: HARD_MAX_BOUNDARY_BYTES,
        max_boundary_bytes: HARD_MAX_BOUNDARY_BYTES,
        max_stream_bytes: HARD_MAX_STREAM_BYTES,
        max_total_read_bytes: HARD_MAX_TOTAL_BYTES,
        max_total_parse_bytes: HARD_MAX_TOTAL_BYTES,
    }
}

fn assert_invalid(label: &str, config: ObjectLimitConfig) {
    let error = match ObjectLimits::validate(config) {
        Ok(_) => panic!("{label} unexpectedly passed validation"),
        Err(error) => error,
    };
    assert_eq!(error.code(), ObjectErrorCode::InvalidLimits, "{label}");
}

fn assert_getters(limits: ObjectLimits, expected: ObjectLimitConfig) {
    assert_eq!(limits.max_source_bytes(), expected.max_source_bytes);
    assert_eq!(
        limits.initial_envelope_bytes(),
        expected.initial_envelope_bytes
    );
    assert_eq!(limits.max_envelope_bytes(), expected.max_envelope_bytes);
    assert_eq!(
        limits.initial_boundary_bytes(),
        expected.initial_boundary_bytes
    );
    assert_eq!(limits.max_boundary_bytes(), expected.max_boundary_bytes);
    assert_eq!(limits.max_stream_bytes(), expected.max_stream_bytes);
    assert_eq!(limits.max_total_read_bytes(), expected.max_total_read_bytes);
    assert_eq!(
        limits.max_total_parse_bytes(),
        expected.max_total_parse_bytes
    );
}

#[test]
fn defaults_validate_and_all_getters_round_trip() {
    let config = ObjectLimitConfig::default();
    let validated = ObjectLimits::validate(config).expect("built-in configuration must validate");
    assert_getters(validated, config);
    assert_eq!(ObjectLimits::default(), validated);
}

#[test]
fn positive_minima_and_equality_relationships_are_accepted() {
    let minimum = minimum_config();
    assert_getters(ObjectLimits::validate(minimum).unwrap(), minimum);

    let windows_and_stream_equal_source = ObjectLimitConfig {
        max_source_bytes: 64,
        initial_envelope_bytes: 63,
        max_envelope_bytes: 64,
        initial_boundary_bytes: 63,
        max_boundary_bytes: 64,
        max_stream_bytes: 64,
        max_total_read_bytes: 128,
        max_total_parse_bytes: 128,
    };
    assert_getters(
        ObjectLimits::validate(windows_and_stream_equal_source).unwrap(),
        windows_and_stream_equal_source,
    );

    let initial_windows_equal_maxima = ObjectLimitConfig {
        initial_envelope_bytes: 64,
        initial_boundary_bytes: 64,
        ..windows_and_stream_equal_source
    };
    assert_getters(
        ObjectLimits::validate(initial_windows_equal_maxima).unwrap(),
        initial_windows_equal_maxima,
    );
}

#[test]
fn hard_ceiling_minus_one_and_equality_are_accepted_and_round_trip() {
    let below = ObjectLimitConfig {
        max_source_bytes: HARD_MAX_SOURCE_BYTES - 1,
        initial_envelope_bytes: HARD_MAX_ENVELOPE_BYTES - 1,
        max_envelope_bytes: HARD_MAX_ENVELOPE_BYTES - 1,
        initial_boundary_bytes: HARD_MAX_BOUNDARY_BYTES - 1,
        max_boundary_bytes: HARD_MAX_BOUNDARY_BYTES - 1,
        max_stream_bytes: HARD_MAX_STREAM_BYTES - 1,
        max_total_read_bytes: HARD_MAX_TOTAL_BYTES - 1,
        max_total_parse_bytes: HARD_MAX_TOTAL_BYTES - 1,
    };
    assert_getters(ObjectLimits::validate(below).unwrap(), below);

    let equal = hard_ceiling_config();
    assert_getters(ObjectLimits::validate(equal).unwrap(), equal);
}

#[test]
fn every_zero_field_is_rejected() {
    let cases: [ConfigMutation; 8] = [
        ("max_source_bytes", |config| config.max_source_bytes = 0),
        ("initial_envelope_bytes", |config| {
            config.initial_envelope_bytes = 0;
        }),
        ("max_envelope_bytes", |config| {
            config.max_envelope_bytes = 0;
        }),
        ("initial_boundary_bytes", |config| {
            config.initial_boundary_bytes = 0;
        }),
        ("max_boundary_bytes", |config| {
            config.max_boundary_bytes = 0;
        }),
        ("max_stream_bytes", |config| config.max_stream_bytes = 0),
        ("max_total_read_bytes", |config| {
            config.max_total_read_bytes = 0;
        }),
        ("max_total_parse_bytes", |config| {
            config.max_total_parse_bytes = 0;
        }),
    ];

    for (label, mutate) in cases {
        let mut config = minimum_config();
        mutate(&mut config);
        assert_invalid(label, config);
    }
}

#[test]
fn inconsistent_window_stream_and_total_relationships_are_rejected() {
    let cases = [
        (
            "initial envelope exceeds maximum envelope",
            ObjectLimitConfig {
                initial_envelope_bytes: 2,
                ..minimum_config()
            },
        ),
        (
            "maximum envelope exceeds source",
            ObjectLimitConfig {
                max_envelope_bytes: 2,
                max_total_read_bytes: 3,
                max_total_parse_bytes: 3,
                ..minimum_config()
            },
        ),
        (
            "initial boundary exceeds maximum boundary",
            ObjectLimitConfig {
                initial_boundary_bytes: 2,
                ..minimum_config()
            },
        ),
        (
            "maximum boundary exceeds source",
            ObjectLimitConfig {
                max_boundary_bytes: 2,
                max_total_read_bytes: 3,
                max_total_parse_bytes: 3,
                ..minimum_config()
            },
        ),
        (
            "maximum stream exceeds source",
            ObjectLimitConfig {
                max_stream_bytes: 2,
                ..minimum_config()
            },
        ),
        (
            "total read is below envelope plus boundary",
            ObjectLimitConfig {
                max_total_read_bytes: 1,
                ..minimum_config()
            },
        ),
        (
            "total parse is below envelope plus boundary",
            ObjectLimitConfig {
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
    let cases: [ConfigMutation; 6] = [
        ("max_source_bytes", |config| {
            config.max_source_bytes = HARD_MAX_SOURCE_BYTES + 1;
        }),
        ("max_envelope_bytes", |config| {
            config.max_envelope_bytes = HARD_MAX_ENVELOPE_BYTES + 1;
        }),
        ("max_boundary_bytes", |config| {
            config.max_boundary_bytes = HARD_MAX_BOUNDARY_BYTES + 1;
        }),
        ("max_stream_bytes", |config| {
            config.max_stream_bytes = HARD_MAX_STREAM_BYTES + 1;
        }),
        ("max_total_read_bytes", |config| {
            config.max_total_read_bytes = HARD_MAX_TOTAL_BYTES + 1;
        }),
        ("max_total_parse_bytes", |config| {
            config.max_total_parse_bytes = HARD_MAX_TOTAL_BYTES + 1;
        }),
    ];

    for (label, mutate) in cases {
        let mut config = hard_ceiling_config();
        mutate(&mut config);
        assert_invalid(label, config);
    }
}

#[test]
fn extreme_values_and_overflowing_window_sum_are_rejected() {
    assert_invalid(
        "all fields at u64::MAX",
        ObjectLimitConfig {
            max_source_bytes: u64::MAX,
            initial_envelope_bytes: u64::MAX,
            max_envelope_bytes: u64::MAX,
            initial_boundary_bytes: u64::MAX,
            max_boundary_bytes: u64::MAX,
            max_stream_bytes: u64::MAX,
            max_total_read_bytes: u64::MAX,
            max_total_parse_bytes: u64::MAX,
        },
    );

    assert_invalid(
        "envelope plus boundary overflows",
        ObjectLimitConfig {
            max_source_bytes: u64::MAX,
            initial_envelope_bytes: 1,
            max_envelope_bytes: u64::MAX,
            initial_boundary_bytes: 1,
            max_boundary_bytes: 1,
            max_stream_bytes: 1,
            max_total_read_bytes: u64::MAX,
            max_total_parse_bytes: u64::MAX,
        },
    );
}
