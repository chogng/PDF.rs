use pdf_rs_document::{DocumentErrorCode, DocumentLimitConfig, DocumentLimits};

const HARD_MAX_TOTAL_ENTRIES: u64 = 4_000_000;
const HARD_MAX_IN_USE_ENTRIES: u64 = 4_000_000;
const HARD_MAX_LOGICAL_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_SORT_STEPS: u64 = 1_000_000_000;

fn minimum_config() -> DocumentLimitConfig {
    DocumentLimitConfig {
        max_total_entries: 1,
        max_in_use_entries: 1,
        max_logical_index_bytes: 1,
        max_sort_steps: 1,
    }
}

fn assert_getters(limits: DocumentLimits, config: DocumentLimitConfig) {
    assert_eq!(limits.max_total_entries(), config.max_total_entries);
    assert_eq!(limits.max_in_use_entries(), config.max_in_use_entries);
    assert_eq!(
        limits.max_logical_index_bytes(),
        config.max_logical_index_bytes
    );
    assert_eq!(limits.max_sort_steps(), config.max_sort_steps);
}

fn assert_invalid(config: DocumentLimitConfig) {
    let error = DocumentLimits::validate(config).unwrap_err();
    assert_eq!(error.code(), DocumentErrorCode::InvalidLimits);
}

#[test]
fn defaults_validate_and_round_trip() {
    let config = DocumentLimitConfig::default();
    let limits = DocumentLimits::validate(config).unwrap();
    assert_getters(limits, config);
    assert_eq!(DocumentLimits::default(), limits);
}

#[test]
fn positive_minima_and_hard_ceiling_equality_are_accepted() {
    let minimum = minimum_config();
    assert_getters(DocumentLimits::validate(minimum).unwrap(), minimum);

    let maximum = DocumentLimitConfig {
        max_total_entries: HARD_MAX_TOTAL_ENTRIES,
        max_in_use_entries: HARD_MAX_IN_USE_ENTRIES,
        max_logical_index_bytes: HARD_MAX_LOGICAL_INDEX_BYTES,
        max_sort_steps: HARD_MAX_SORT_STEPS,
    };
    assert_getters(DocumentLimits::validate(maximum).unwrap(), maximum);
}

#[test]
fn every_zero_field_is_rejected() {
    let mutations: [fn(&mut DocumentLimitConfig); 4] = [
        |config| config.max_total_entries = 0,
        |config| config.max_in_use_entries = 0,
        |config| config.max_logical_index_bytes = 0,
        |config| config.max_sort_steps = 0,
    ];
    for mutation in mutations {
        let mut config = minimum_config();
        mutation(&mut config);
        assert_invalid(config);
    }
}

#[test]
fn in_use_ceiling_cannot_exceed_total_ceiling() {
    assert_invalid(DocumentLimitConfig {
        max_total_entries: 1,
        max_in_use_entries: 2,
        ..minimum_config()
    });
}

#[test]
fn every_hard_ceiling_plus_one_is_rejected() {
    let cases = [
        DocumentLimitConfig {
            max_total_entries: HARD_MAX_TOTAL_ENTRIES + 1,
            max_in_use_entries: 1,
            ..minimum_config()
        },
        DocumentLimitConfig {
            max_total_entries: HARD_MAX_TOTAL_ENTRIES,
            max_in_use_entries: HARD_MAX_IN_USE_ENTRIES + 1,
            ..minimum_config()
        },
        DocumentLimitConfig {
            max_logical_index_bytes: HARD_MAX_LOGICAL_INDEX_BYTES + 1,
            ..minimum_config()
        },
        DocumentLimitConfig {
            max_sort_steps: HARD_MAX_SORT_STEPS + 1,
            ..minimum_config()
        },
    ];
    for config in cases {
        assert_invalid(config);
    }
}
