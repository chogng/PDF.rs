use pdf_rs_document::{
    DocumentErrorCode, RevisionAttestationLimitConfig, RevisionAttestationLimits,
};

const MIB: u64 = 1024 * 1024;

fn minimum() -> RevisionAttestationLimitConfig {
    RevisionAttestationLimitConfig {
        max_source_bytes: 1,
        max_objects: 1,
        scan_chunk_bytes: 9,
        max_trivia_bytes: 9,
        max_comment_bytes: 1,
        max_total_object_read_bytes: 1,
        max_total_object_parse_bytes: 1,
        max_retained_evidence_bytes: 1,
    }
}

fn assert_round_trip(config: RevisionAttestationLimitConfig) {
    let limits = RevisionAttestationLimits::validate(config).unwrap();
    assert_eq!(limits.max_source_bytes(), config.max_source_bytes);
    assert_eq!(limits.max_objects(), config.max_objects);
    assert_eq!(limits.scan_chunk_bytes(), config.scan_chunk_bytes);
    assert_eq!(limits.max_trivia_bytes(), config.max_trivia_bytes);
    assert_eq!(limits.max_comment_bytes(), config.max_comment_bytes);
    assert_eq!(
        limits.max_total_object_read_bytes(),
        config.max_total_object_read_bytes
    );
    assert_eq!(
        limits.max_total_object_parse_bytes(),
        config.max_total_object_parse_bytes
    );
    assert_eq!(
        limits.max_retained_evidence_bytes(),
        config.max_retained_evidence_bytes
    );
}

fn assert_invalid(config: RevisionAttestationLimitConfig) {
    let error = RevisionAttestationLimits::validate(config).unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
}

#[test]
fn defaults_and_positive_boundaries_round_trip() {
    let default_config = RevisionAttestationLimitConfig::default();
    assert_round_trip(default_config);
    assert_eq!(
        RevisionAttestationLimits::default(),
        RevisionAttestationLimits::validate(default_config).unwrap()
    );
    assert_round_trip(minimum());
    assert_round_trip(RevisionAttestationLimitConfig {
        max_source_bytes: 1024 * MIB,
        max_objects: 4_000_000,
        scan_chunk_bytes: MIB,
        max_trivia_bytes: 1024 * MIB,
        max_comment_bytes: MIB,
        max_total_object_read_bytes: 1024 * MIB,
        max_total_object_parse_bytes: 1024 * MIB,
        max_retained_evidence_bytes: 512 * MIB,
    });
}

#[test]
fn every_zero_field_is_rejected() {
    let mutations: [fn(&mut RevisionAttestationLimitConfig); 8] = [
        |value| value.max_source_bytes = 0,
        |value| value.max_objects = 0,
        |value| value.scan_chunk_bytes = 0,
        |value| value.max_trivia_bytes = 0,
        |value| value.max_comment_bytes = 0,
        |value| value.max_total_object_read_bytes = 0,
        |value| value.max_total_object_parse_bytes = 0,
        |value| value.max_retained_evidence_bytes = 0,
    ];
    for mutation in mutations {
        let mut config = minimum();
        mutation(&mut config);
        assert_invalid(config);
    }
}

#[test]
fn scan_and_header_minima_and_relationships_are_enforced() {
    for value in 1..9 {
        assert_invalid(RevisionAttestationLimitConfig {
            scan_chunk_bytes: value,
            ..minimum()
        });
        assert_invalid(RevisionAttestationLimitConfig {
            max_trivia_bytes: value,
            scan_chunk_bytes: value.max(1),
            ..minimum()
        });
    }
    assert_invalid(RevisionAttestationLimitConfig {
        scan_chunk_bytes: 10,
        max_trivia_bytes: 9,
        ..minimum()
    });
    assert_invalid(RevisionAttestationLimitConfig {
        max_comment_bytes: 10,
        max_trivia_bytes: 9,
        ..minimum()
    });
}

#[test]
fn every_hard_ceiling_plus_one_is_rejected() {
    let cases = [
        RevisionAttestationLimitConfig {
            max_source_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_objects: 4_000_001,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            scan_chunk_bytes: MIB + 1,
            max_trivia_bytes: MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_trivia_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_comment_bytes: MIB + 1,
            max_trivia_bytes: MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_total_object_read_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_total_object_parse_bytes: 1024 * MIB + 1,
            ..minimum()
        },
        RevisionAttestationLimitConfig {
            max_retained_evidence_bytes: 512 * MIB + 1,
            ..minimum()
        },
    ];
    for config in cases {
        assert_invalid(config);
    }
}
