use std::mem;

use pdf_rs_document::{DocumentErrorCode, ReferenceChainLimitConfig, ReferenceChainLimits};
use pdf_rs_syntax::ObjectRef;

const MIB: u64 = 1024 * 1024;

fn reference_bytes() -> u64 {
    u64::try_from(mem::size_of::<ObjectRef>()).expect("ObjectRef size fits u64")
}

fn minimum() -> ReferenceChainLimitConfig {
    ReferenceChainLimitConfig {
        max_objects: 1,
        max_reference_edges: 1,
        max_depth: 1,
        max_total_object_read_bytes: 1,
        max_total_object_parse_bytes: 1,
        max_retained_path_bytes: reference_bytes(),
    }
}

fn assert_round_trip(config: ReferenceChainLimitConfig) {
    let limits = ReferenceChainLimits::validate(config).expect("profile must validate");
    assert_eq!(limits.max_objects(), config.max_objects);
    assert_eq!(limits.max_reference_edges(), config.max_reference_edges);
    assert_eq!(limits.max_depth(), config.max_depth);
    assert_eq!(
        limits.max_total_object_read_bytes(),
        config.max_total_object_read_bytes
    );
    assert_eq!(
        limits.max_total_object_parse_bytes(),
        config.max_total_object_parse_bytes
    );
    assert_eq!(
        limits.max_retained_path_bytes(),
        config.max_retained_path_bytes
    );
}

fn assert_invalid(config: ReferenceChainLimitConfig) {
    let error = ReferenceChainLimits::validate(config).expect_err("profile must be rejected");
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
}

#[test]
fn defaults_positive_minima_and_hard_ceiling_equality_round_trip() {
    let default_config = ReferenceChainLimitConfig::default();
    assert_round_trip(default_config);
    assert_eq!(
        ReferenceChainLimits::default(),
        ReferenceChainLimits::validate(default_config).unwrap()
    );

    assert_round_trip(minimum());
    assert_round_trip(ReferenceChainLimitConfig {
        max_objects: 256,
        max_reference_edges: 256,
        max_depth: 256,
        max_total_object_read_bytes: 256 * MIB,
        max_total_object_parse_bytes: 256 * MIB,
        max_retained_path_bytes: 64 * 1024,
    });
}

#[test]
fn every_zero_field_is_rejected() {
    let mutations: [fn(&mut ReferenceChainLimitConfig); 6] = [
        |value| value.max_objects = 0,
        |value| value.max_reference_edges = 0,
        |value| value.max_depth = 0,
        |value| value.max_total_object_read_bytes = 0,
        |value| value.max_total_object_parse_bytes = 0,
        |value| value.max_retained_path_bytes = 0,
    ];
    for mutation in mutations {
        let mut config = minimum();
        mutation(&mut config);
        assert_invalid(config);
    }
}

#[test]
fn retained_path_must_cover_the_effective_path_capacity() {
    let bytes = reference_bytes();
    assert_round_trip(ReferenceChainLimitConfig {
        max_objects: 2,
        max_reference_edges: 1,
        max_depth: 2,
        max_retained_path_bytes: 2 * bytes,
        ..minimum()
    });
    assert_invalid(ReferenceChainLimitConfig {
        max_objects: 2,
        max_reference_edges: 1,
        max_depth: 2,
        max_retained_path_bytes: 2 * bytes - 1,
        ..minimum()
    });

    // Object, edge, and depth ceilings are independent. Only the smallest reachable path
    // dimension contributes to the reservation relationship.
    assert_round_trip(ReferenceChainLimitConfig {
        max_objects: 256,
        max_reference_edges: 1,
        max_depth: 256,
        max_retained_path_bytes: 2 * bytes,
        ..minimum()
    });
    assert_round_trip(ReferenceChainLimitConfig {
        max_objects: 1,
        max_reference_edges: 256,
        max_depth: 256,
        max_retained_path_bytes: bytes,
        ..minimum()
    });
}

#[test]
fn every_hard_ceiling_plus_one_is_rejected() {
    let cases = [
        ReferenceChainLimitConfig {
            max_objects: 257,
            ..minimum()
        },
        ReferenceChainLimitConfig {
            max_reference_edges: 257,
            ..minimum()
        },
        ReferenceChainLimitConfig {
            max_depth: 257,
            ..minimum()
        },
        ReferenceChainLimitConfig {
            max_total_object_read_bytes: 256 * MIB + 1,
            ..minimum()
        },
        ReferenceChainLimitConfig {
            max_total_object_parse_bytes: 256 * MIB + 1,
            ..minimum()
        },
        ReferenceChainLimitConfig {
            max_retained_path_bytes: 64 * 1024 + 1,
            ..minimum()
        },
    ];
    for config in cases {
        assert_invalid(config);
    }
}
