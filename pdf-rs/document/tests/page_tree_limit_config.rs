use pdf_rs_document::{DocumentErrorCode, PageTreeLimitConfig, PageTreeLimits};

const MIB: u64 = 1024 * 1024;

fn minimum() -> PageTreeLimitConfig {
    PageTreeLimitConfig {
        max_nodes: 1,
        max_depth: 1,
        max_pages: 1,
        max_kids_per_node: 1,
        max_total_object_read_bytes: 1,
        max_total_object_parse_bytes: 1,
        max_retained_traversal_bytes: 1,
    }
}

fn assert_round_trip(config: PageTreeLimitConfig) {
    let limits = PageTreeLimits::validate(config).expect("profile must validate");
    assert_eq!(limits.max_nodes(), config.max_nodes);
    assert_eq!(limits.max_depth(), config.max_depth);
    assert_eq!(limits.max_pages(), config.max_pages);
    assert_eq!(limits.max_kids_per_node(), config.max_kids_per_node);
    assert_eq!(
        limits.max_total_object_read_bytes(),
        config.max_total_object_read_bytes
    );
    assert_eq!(
        limits.max_total_object_parse_bytes(),
        config.max_total_object_parse_bytes
    );
    assert_eq!(
        limits.max_retained_traversal_bytes(),
        config.max_retained_traversal_bytes
    );
    assert_eq!(limits.effective_work_items(), 2 * config.max_nodes);
    assert_eq!(limits.effective_seen_references(), config.max_nodes);
}

fn assert_invalid(config: PageTreeLimitConfig) {
    let error = PageTreeLimits::validate(config).expect_err("profile must be rejected");
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
}

#[test]
fn defaults_positive_minima_and_hard_ceiling_equality_round_trip() {
    let default_config = PageTreeLimitConfig::default();
    assert_round_trip(default_config);
    assert_eq!(
        PageTreeLimits::default(),
        PageTreeLimits::validate(default_config).unwrap()
    );

    assert_round_trip(minimum());
    assert_round_trip(PageTreeLimitConfig {
        max_nodes: 4_000_000,
        max_depth: 1024,
        max_pages: 4_000_000,
        max_kids_per_node: MIB,
        max_total_object_read_bytes: 1024 * MIB,
        max_total_object_parse_bytes: 1024 * MIB,
        max_retained_traversal_bytes: 512 * MIB,
    });
}

#[test]
fn every_zero_field_is_rejected() {
    let mutations: [fn(&mut PageTreeLimitConfig); 7] = [
        |value| value.max_nodes = 0,
        |value| value.max_depth = 0,
        |value| value.max_pages = 0,
        |value| value.max_kids_per_node = 0,
        |value| value.max_total_object_read_bytes = 0,
        |value| value.max_total_object_parse_bytes = 0,
        |value| value.max_retained_traversal_bytes = 0,
    ];
    for mutation in mutations {
        let mut config = minimum();
        mutation(&mut config);
        assert_invalid(config);
    }
}

#[test]
fn derived_work_and_seen_bounds_follow_the_node_limit() {
    assert_round_trip(PageTreeLimitConfig {
        max_nodes: 1,
        max_depth: 1024,
        ..minimum()
    });
    assert_round_trip(PageTreeLimitConfig {
        max_nodes: 4_000_000,
        max_depth: 1,
        max_retained_traversal_bytes: 512 * MIB,
        ..minimum()
    });

    assert_invalid(PageTreeLimitConfig {
        max_nodes: u64::MAX,
        ..minimum()
    });
}

#[test]
fn every_hard_ceiling_plus_one_is_rejected() {
    let cases = [
        PageTreeLimitConfig {
            max_nodes: 4_000_001,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_depth: 1025,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_pages: 4_000_001,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_kids_per_node: MIB + 1,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_total_object_read_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_total_object_parse_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        PageTreeLimitConfig {
            max_retained_traversal_bytes: 512 * MIB + 1,
            ..minimum()
        },
    ];
    for config in cases {
        assert_invalid(config);
    }
}
