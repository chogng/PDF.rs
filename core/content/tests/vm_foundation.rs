use pdf_rs_content::{
    ContentVmErrorCategory, ContentVmErrorCode, ContentVmLimitConfig, ContentVmLimitKind,
    ContentVmLimits, ContentVmRecoverability, ContentVmStats,
};

#[test]
fn default_vm_limits_validate_and_round_trip_every_dimension() {
    let config = ContentVmLimitConfig::default();
    let limits = ContentVmLimits::validate(config).expect("built-in VM limits are valid");
    assert_eq!(limits.max_operators(), config.max_operators);
    assert_eq!(limits.max_fuel(), config.max_fuel);
    assert_eq!(
        limits.max_graphics_state_depth(),
        config.max_graphics_state_depth
    );
    assert_eq!(
        limits.max_compatibility_depth(),
        config.max_compatibility_depth
    );
    assert_eq!(
        limits.max_marked_content_depth(),
        config.max_marked_content_depth
    );
    assert_eq!(limits.max_property_uses(), config.max_property_uses);
    assert_eq!(limits.max_retained_bytes(), config.max_retained_bytes);
    assert_eq!(ContentVmLimits::default(), limits);
}

#[test]
fn every_vm_limit_dimension_rejects_zero_and_values_above_hard_ceiling() {
    let zero_mutations: &[fn(&mut ContentVmLimitConfig)] = &[
        |value| value.max_operators = 0,
        |value| value.max_fuel = 0,
        |value| value.max_graphics_state_depth = 0,
        |value| value.max_compatibility_depth = 0,
        |value| value.max_marked_content_depth = 0,
        |value| value.max_property_uses = 0,
        |value| value.max_retained_bytes = 0,
    ];
    for mutate in zero_mutations {
        let mut config = ContentVmLimitConfig::default();
        mutate(&mut config);
        let error = ContentVmLimits::validate(config).expect_err("zero limit must fail");
        assert_eq!(error.code(), ContentVmErrorCode::InvalidLimits);
        assert_eq!(error.category(), ContentVmErrorCategory::Configuration);
        assert_eq!(
            error.recoverability(),
            ContentVmRecoverability::CorrectConfiguration
        );
        assert_eq!(error.diagnostic_id(), "RPE-CONTENT-VM-0001");
    }

    let above_hard_mutations: &[fn(&mut ContentVmLimitConfig)] = &[
        |value| value.max_operators = u64::MAX,
        |value| value.max_fuel = u64::MAX,
        |value| value.max_graphics_state_depth = u32::MAX,
        |value| value.max_compatibility_depth = u32::MAX,
        |value| value.max_marked_content_depth = u32::MAX,
        |value| value.max_property_uses = u64::MAX,
        |value| value.max_retained_bytes = u64::MAX,
    ];
    for mutate in above_hard_mutations {
        let mut config = ContentVmLimitConfig::default();
        mutate(&mut config);
        assert_eq!(
            ContentVmLimits::validate(config)
                .expect_err("above-hard limit must fail")
                .code(),
            ContentVmErrorCode::InvalidLimits
        );
    }
}

#[test]
fn default_vm_stats_start_every_dimension_at_zero() {
    let stats = ContentVmStats::default();
    assert_eq!(stats.operators(), 0);
    assert_eq!(stats.fuel(), 0);
    assert_eq!(stats.max_graphics_state_depth(), 0);
    assert_eq!(stats.max_compatibility_depth(), 0);
    assert_eq!(stats.max_marked_content_depth(), 0);
    assert_eq!(stats.property_uses(), 0);
    assert_eq!(stats.retained_bytes(), 0);
}

#[test]
fn structured_vm_resource_context_is_exact_and_redacted() {
    let config = ContentVmLimitConfig {
        max_retained_bytes: 64,
        ..ContentVmLimitConfig::default()
    };
    let limits = ContentVmLimits::validate(config).expect("fixture limits are valid");
    assert_eq!(
        limits.preflight(ContentVmLimitKind::RetainedBytes, 56, 8, None),
        Ok(())
    );
    let error = limits
        .preflight(ContentVmLimitKind::RetainedBytes, 56, 16, None)
        .expect_err("charge exceeds retained limit");
    assert_eq!(error.code(), ContentVmErrorCode::ResourceLimit);
    assert_eq!(error.category(), ContentVmErrorCategory::Resource);
    assert_eq!(
        error.recoverability(),
        ContentVmRecoverability::ReduceWorkload
    );
    assert_eq!(error.diagnostic_id(), "RPE-CONTENT-VM-0012");
    let limit = error.limit().expect("resource error has limit context");
    assert_eq!(limit.kind(), ContentVmLimitKind::RetainedBytes);
    assert_eq!(limit.limit(), 64);
    assert_eq!(limit.consumed(), 56);
    assert_eq!(limit.attempted(), 16);
    assert_eq!(error.to_string(), "RPE-CONTENT-VM-0012");
}

#[test]
fn preflight_maps_every_kind_to_its_independent_validated_limit() {
    let limits = ContentVmLimits::default();
    let cases = [
        (ContentVmLimitKind::Operators, limits.max_operators()),
        (ContentVmLimitKind::Fuel, limits.max_fuel()),
        (
            ContentVmLimitKind::GraphicsStateDepth,
            u64::from(limits.max_graphics_state_depth()),
        ),
        (
            ContentVmLimitKind::CompatibilityDepth,
            u64::from(limits.max_compatibility_depth()),
        ),
        (
            ContentVmLimitKind::MarkedContentDepth,
            u64::from(limits.max_marked_content_depth()),
        ),
        (ContentVmLimitKind::PropertyUses, limits.max_property_uses()),
        (
            ContentVmLimitKind::RetainedBytes,
            limits.max_retained_bytes(),
        ),
        (ContentVmLimitKind::Allocation, limits.max_retained_bytes()),
    ];
    for (kind, limit) in cases {
        assert_eq!(limits.preflight(kind, limit - 1, 1, None), Ok(()));
        let error = limits
            .preflight(kind, limit - 1, 2, None)
            .expect_err("one-over charge must fail");
        let context = error.limit().expect("resource error has context");
        assert_eq!(context.kind(), kind);
        assert_eq!(context.limit(), limit);
        assert_eq!(context.consumed(), limit - 1);
        assert_eq!(context.attempted(), 2);
    }

    let overflow = limits
        .preflight(ContentVmLimitKind::Fuel, u64::MAX, 1, None)
        .expect_err("checked addition overflow must fail as resource exhaustion");
    let context = overflow.limit().expect("overflow error has context");
    assert_eq!(context.kind(), ContentVmLimitKind::Fuel);
    assert_eq!(context.consumed(), u64::MAX);
    assert_eq!(context.attempted(), 1);
}
