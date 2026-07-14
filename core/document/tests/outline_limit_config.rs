use pdf_rs_document::{DocumentErrorCode, OutlineLimitConfig, OutlineLimits, TextStringLimits};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

fn assert_invalid(config: OutlineLimitConfig) {
    let error = OutlineLimits::validate(config).expect_err("outline limits must be rejected");
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
}

fn hard_ceiling_config() -> OutlineLimitConfig {
    OutlineLimitConfig {
        max_items: 65_536,
        max_depth: 4_096,
        max_siblings_per_level: 65_536,
        max_title_input_bytes: MIB,
        max_title_utf8_bytes: 4 * MIB,
        max_total_title_input_bytes: 64 * MIB,
        max_total_title_utf8_bytes: 256 * MIB,
        max_total_object_read_bytes: 1024 * MIB,
        max_total_object_parse_bytes: 1024 * MIB,
        max_retained_bytes: 512 * MIB,
    }
}

#[test]
fn defaults_are_valid_and_match_the_bounded_outline_profile() {
    let config = OutlineLimitConfig::default();
    assert_eq!(config.max_items, 4_096);
    assert_eq!(config.max_depth, 64);
    assert_eq!(config.max_siblings_per_level, 1_024);
    assert_eq!(config.max_title_input_bytes, 64 * KIB);
    assert_eq!(config.max_title_utf8_bytes, 256 * KIB);
    assert_eq!(config.max_total_title_input_bytes, 8 * MIB);
    assert_eq!(config.max_total_title_utf8_bytes, 32 * MIB);
    assert_eq!(config.max_total_object_read_bytes, 64 * MIB);
    assert_eq!(config.max_total_object_parse_bytes, 64 * MIB);
    assert_eq!(config.max_retained_bytes, 64 * MIB);
    assert_eq!(
        OutlineLimits::default(),
        OutlineLimits::validate(config).unwrap()
    );
}

#[test]
fn getters_and_title_limits_preserve_validated_configuration() {
    let config = OutlineLimitConfig {
        max_items: 321,
        max_depth: 23,
        max_siblings_per_level: 123,
        max_title_input_bytes: 12_345,
        max_title_utf8_bytes: 54_321,
        max_total_title_input_bytes: 456_789,
        max_total_title_utf8_bytes: 987_654,
        max_total_object_read_bytes: 1_234_567,
        max_total_object_parse_bytes: 2_345_678,
        max_retained_bytes: 3_456_789,
    };
    let limits = OutlineLimits::validate(config).expect("test limits are valid");

    assert_eq!(limits.max_items(), config.max_items);
    assert_eq!(limits.max_depth(), config.max_depth);
    assert_eq!(
        limits.max_siblings_per_level(),
        config.max_siblings_per_level
    );
    assert_eq!(limits.max_title_input_bytes(), config.max_title_input_bytes);
    assert_eq!(limits.max_title_utf8_bytes(), config.max_title_utf8_bytes);
    assert_eq!(
        limits.max_total_title_input_bytes(),
        config.max_total_title_input_bytes
    );
    assert_eq!(
        limits.max_total_title_utf8_bytes(),
        config.max_total_title_utf8_bytes
    );
    assert_eq!(
        limits.max_total_object_read_bytes(),
        config.max_total_object_read_bytes
    );
    assert_eq!(
        limits.max_total_object_parse_bytes(),
        config.max_total_object_parse_bytes
    );
    assert_eq!(limits.max_retained_bytes(), config.max_retained_bytes);
    assert_eq!(
        limits.title_limits(),
        TextStringLimits::validate(pdf_rs_document::TextStringLimitConfig {
            max_input_bytes: config.max_title_input_bytes,
            max_utf8_bytes: config.max_title_utf8_bytes,
        })
        .unwrap()
    );
}

#[test]
fn every_zero_field_is_rejected() {
    macro_rules! zero_field {
        ($field:ident) => {{
            let mut config = OutlineLimitConfig::default();
            config.$field = 0;
            assert_invalid(config);
        }};
    }

    zero_field!(max_items);
    zero_field!(max_depth);
    zero_field!(max_siblings_per_level);
    zero_field!(max_title_input_bytes);
    zero_field!(max_title_utf8_bytes);
    zero_field!(max_total_title_input_bytes);
    zero_field!(max_total_title_utf8_bytes);
    zero_field!(max_total_object_read_bytes);
    zero_field!(max_total_object_parse_bytes);
    zero_field!(max_retained_bytes);
}

#[test]
fn inconsistent_cross_field_relationships_are_rejected() {
    assert_invalid(OutlineLimitConfig {
        max_items: 10,
        max_depth: 11,
        max_siblings_per_level: 10,
        ..OutlineLimitConfig::default()
    });
    assert_invalid(OutlineLimitConfig {
        max_items: 10,
        max_depth: 10,
        max_siblings_per_level: 11,
        ..OutlineLimitConfig::default()
    });
    assert_invalid(OutlineLimitConfig {
        max_title_input_bytes: 101,
        max_total_title_input_bytes: 100,
        ..OutlineLimitConfig::default()
    });
    assert_invalid(OutlineLimitConfig {
        max_title_utf8_bytes: 101,
        max_total_title_utf8_bytes: 100,
        ..OutlineLimitConfig::default()
    });
}

#[test]
fn every_hard_ceiling_is_inclusive_and_rejects_one_more() {
    OutlineLimits::validate(hard_ceiling_config()).expect("exact hard ceilings are valid");

    macro_rules! above_ceiling {
        ($field:ident) => {{
            let mut config = hard_ceiling_config();
            config.$field += 1;
            assert_invalid(config);
        }};
    }

    above_ceiling!(max_items);
    above_ceiling!(max_depth);
    above_ceiling!(max_siblings_per_level);
    above_ceiling!(max_title_input_bytes);
    above_ceiling!(max_title_utf8_bytes);
    above_ceiling!(max_total_title_input_bytes);
    above_ceiling!(max_total_title_utf8_bytes);
    above_ceiling!(max_total_object_read_bytes);
    above_ceiling!(max_total_object_parse_bytes);
    above_ceiling!(max_retained_bytes);
}
