mod support;

use pdf_rs_policy::{
    CapabilityContributorKind, CapabilityEvaluator, CapabilityProfile, CapabilityRejectionCode,
    CapabilityStatus as ProductStatus, CollectionCompleteness, PolicyErrorCategory,
    PolicyErrorCode, PolicyLimitConfig, PolicyLimitKind, PolicyLimits,
};
use pdf_rs_scene::{CapabilityStatus, GraphicsCapability};

use support::{
    CancelAfter, CountingNever, Never, RequirementSpec, legacy_scene_with_commands_and_resources,
    scene, scene_with_canonical_limit,
};

#[test]
fn legacy_non_graphics_scene_is_structurally_rejected_and_never_supported() {
    let scene = legacy_scene_with_commands_and_resources();
    let decision = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(decision.status(), ProductStatus::Rejected);
    assert_eq!(
        decision.rejection_code(),
        Some(CapabilityRejectionCode::UnsupportedSceneSchema)
    );
    assert_eq!(decision.evaluated_requirements(), 0);
    assert_eq!(decision.evaluated_dependencies(), 0);
    assert_eq!(decision.evaluated_parameters(), 0);
    assert_eq!(decision.evaluated_commands(), 2);
    assert_eq!(decision.evaluated_resources(), 1);
    assert_eq!(decision.contributors_total(), 1);
    assert_eq!(
        decision.contributors()[0].code(),
        CapabilityRejectionCode::UnsupportedSceneSchema as u32
    );
    assert!(!decision.hash().is_zero());
    assert!(
        decision
            .protocol_projection()
            .unwrap()
            .wire_invariants_valid()
    );
}

#[test]
fn zero_document_revision_wins_before_cancellation_and_canonicalization() {
    let scene = scene_with_canonical_limit(&[], 1);
    let error = CapabilityEvaluator::default()
        .evaluate(&scene, 0, &CancelAfter::new(0))
        .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::InvalidDocumentRevision);
    assert_eq!(error.category(), PolicyErrorCategory::InvalidInput);
}

#[test]
fn all_graph_cardinality_limits_preflight_before_scene_canonicalization() {
    let scene = scene_with_canonical_limit(
        &[
            RequirementSpec {
                capability: GraphicsCapability::PathFill,
                parameter: 0,
                dependencies: &[],
                status: CapabilityStatus::Supported,
            },
            RequirementSpec {
                capability: GraphicsCapability::PathStroke,
                parameter: 0,
                dependencies: &[0],
                status: CapabilityStatus::Supported,
            },
            RequirementSpec {
                capability: GraphicsCapability::Clip,
                parameter: 0,
                dependencies: &[0, 1],
                status: CapabilityStatus::Supported,
            },
        ],
        1,
    );
    for (kind, config) in [
        (
            PolicyLimitKind::Requirements,
            PolicyLimitConfig {
                max_requirements: 1,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::Parameters,
            PolicyLimitConfig {
                max_parameters: 1,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::Dependencies,
            PolicyLimitConfig {
                max_dependencies: 1,
                ..PolicyLimitConfig::default()
            },
        ),
    ] {
        let error = CapabilityEvaluator::new(
            CapabilityProfile::default(),
            PolicyLimits::validate(config).unwrap(),
        )
        .evaluate(&scene, 23, &Never)
        .unwrap_err();
        assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), kind);
    }
}

#[test]
fn first_profile_matches_the_registered_requirement_predicate_independently() {
    let supported = [
        (GraphicsCapability::PathFill, 0),
        (GraphicsCapability::PathFill, 1),
        (GraphicsCapability::PathStroke, 0),
        (GraphicsCapability::Clip, 1),
        (GraphicsCapability::DeviceColor, 1),
        (GraphicsCapability::DeviceColor, 3),
        (GraphicsCapability::DeviceColor, 4),
        (GraphicsCapability::ConstantAlpha, u64::from(u16::MAX)),
        (GraphicsCapability::Blend, 0),
        (GraphicsCapability::Blend, 2),
        (GraphicsCapability::Image, 3 | (8 << 8)),
        (GraphicsCapability::Glyph, 1),
    ];
    for (capability, parameter) in supported {
        let scene = scene(&[RequirementSpec {
            capability,
            parameter,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        }]);
        let decision = CapabilityEvaluator::default()
            .evaluate(&scene, 23, &Never)
            .unwrap();
        assert_eq!(
            decision.status(),
            ProductStatus::Supported,
            "{capability:?} parameter {parameter}"
        );
    }

    let unsupported = [
        (GraphicsCapability::PathFill, 2),
        (GraphicsCapability::PathStroke, 1),
        (GraphicsCapability::Clip, 2),
        (GraphicsCapability::DeviceColor, 2),
        (GraphicsCapability::ConstantAlpha, u64::from(u16::MAX) + 1),
        (GraphicsCapability::Blend, 3),
        (GraphicsCapability::Image, 3 | (8 << 8) | (1 << 16)),
        (GraphicsCapability::Image, 3 | (16 << 8)),
        (GraphicsCapability::Glyph, 0),
        (GraphicsCapability::SoftMask, 0),
        (GraphicsCapability::IsolatedGroup, 0),
    ];
    for (capability, parameter) in unsupported {
        let scene = scene(&[RequirementSpec {
            capability,
            parameter,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        }]);
        let decision = CapabilityEvaluator::default()
            .evaluate(&scene, 23, &Never)
            .unwrap();
        assert_eq!(
            decision.status(),
            ProductStatus::Unsupported,
            "{capability:?} parameter {parameter}"
        );
    }
}

#[test]
fn m4_fast_profile_admits_only_registered_isolated_groups() {
    let profile = CapabilityProfile::m4_fast_v1();
    assert_eq!(profile.profile_version(), 2);
    assert_eq!(profile.policy_version(), 2);

    for (capability, parameter, expected) in [
        (
            GraphicsCapability::IsolatedGroup,
            0,
            ProductStatus::Supported,
        ),
        (
            GraphicsCapability::IsolatedGroup,
            1,
            ProductStatus::Unsupported,
        ),
        (GraphicsCapability::SoftMask, 0, ProductStatus::Unsupported),
    ] {
        let scene = scene(&[RequirementSpec {
            capability,
            parameter,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        }]);
        let decision = CapabilityEvaluator::new(profile, PolicyLimits::default())
            .evaluate(&scene, 23, &Never)
            .unwrap();
        assert_eq!(decision.status(), expected);
    }
}

#[test]
fn producer_status_is_consumed_but_never_used_as_a_page_summary() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::PathFill,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::DeviceColor,
            parameter: 3,
            dependencies: &[],
            status: CapabilityStatus::Unsupported,
        },
        RequirementSpec {
            capability: GraphicsCapability::PathStroke,
            parameter: 0,
            dependencies: &[1],
            status: CapabilityStatus::Supported,
        },
    ]);
    let decision = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(decision.status(), ProductStatus::Unsupported);
    assert_eq!(decision.evaluated_requirements(), 3);
    assert_eq!(decision.evaluated_dependencies(), 1);
    assert_eq!(decision.missing_total(), 2);
    assert_eq!(
        decision
            .missing()
            .iter()
            .map(|value| value.id())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        decision.contributors()[0].kind(),
        CapabilityContributorKind::SceneRequirement
    );
    assert_eq!(
        decision.contributors()[1].kind(),
        CapabilityContributorKind::PolicyDependencyClosure
    );
    assert_eq!(decision.contributors()[1].code(), 1);
    assert_eq!(decision.missing()[1].dependencies(), [1]);
}

#[test]
fn retention_limits_keep_canonical_prefixes_and_exact_totals() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::SoftMask,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Unsupported,
        },
        RequirementSpec {
            capability: GraphicsCapability::IsolatedGroup,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Unsupported,
        },
    ]);
    let limits = PolicyLimits::validate(PolicyLimitConfig {
        max_missing_retained: 1,
        max_contributors_retained: 0,
        max_locations_retained: 1,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    let decision = CapabilityEvaluator::new(CapabilityProfile::m3_reference_v1(), limits)
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(decision.missing_total(), 2);
    assert_eq!(decision.missing().len(), 1);
    assert_eq!(
        decision.missing_completeness(),
        CollectionCompleteness::Truncated
    );
    assert_eq!(decision.contributors_total(), 2);
    assert!(decision.contributors().is_empty());
    assert_eq!(
        decision.contributors_completeness(),
        CollectionCompleteness::Truncated
    );
    assert!(decision.missing()[0].contributor_ids().is_empty());
    assert_eq!(decision.locations_total(), 2);
    assert_eq!(
        decision.locations_completeness(),
        CollectionCompleteness::Truncated
    );
    assert!(decision.location().is_some());
}

#[test]
fn every_retention_dimension_has_exact_and_one_less_boundaries() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::SoftMask,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Unsupported,
        },
        RequirementSpec {
            capability: GraphicsCapability::IsolatedGroup,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Unsupported,
        },
    ]);
    let exact = CapabilityEvaluator::new(
        CapabilityProfile::default(),
        PolicyLimits::validate(PolicyLimitConfig {
            max_missing_retained: 2,
            max_contributors_retained: 2,
            max_locations_retained: 2,
            ..PolicyLimitConfig::default()
        })
        .unwrap(),
    )
    .evaluate(&scene, 23, &Never)
    .unwrap();
    assert_eq!(exact.missing().len(), 2);
    assert_eq!(exact.contributors().len(), 2);
    assert!(
        exact
            .missing()
            .iter()
            .all(|value| value.location().is_some())
    );
    assert_eq!(
        exact.missing_completeness(),
        CollectionCompleteness::Complete
    );
    assert_eq!(
        exact.contributors_completeness(),
        CollectionCompleteness::Complete
    );
    assert_eq!(
        exact.locations_completeness(),
        CollectionCompleteness::Truncated
    );

    for (missing, contributors, locations) in [(1, 2, 2), (2, 1, 2), (2, 2, 1)] {
        let one_less = CapabilityEvaluator::new(
            CapabilityProfile::default(),
            PolicyLimits::validate(PolicyLimitConfig {
                max_missing_retained: missing,
                max_contributors_retained: contributors,
                max_locations_retained: locations,
                ..PolicyLimitConfig::default()
            })
            .unwrap(),
        )
        .evaluate(&scene, 23, &Never)
        .unwrap();
        assert_eq!(one_less.missing().len(), usize::try_from(missing).unwrap());
        assert_eq!(
            one_less.contributors().len(),
            usize::try_from(contributors).unwrap()
        );
        assert_eq!(
            one_less
                .missing()
                .iter()
                .filter(|value| value.location().is_some())
                .count(),
            usize::try_from(locations.min(missing)).unwrap()
        );
        assert_eq!(
            one_less.missing_completeness(),
            if missing == 2 {
                CollectionCompleteness::Complete
            } else {
                CollectionCompleteness::Truncated
            }
        );
        assert_eq!(
            one_less.contributors_completeness(),
            if contributors == 2 {
                CollectionCompleteness::Complete
            } else {
                CollectionCompleteness::Truncated
            }
        );
        assert_eq!(
            one_less.locations_completeness(),
            CollectionCompleteness::Truncated
        );
    }
}

#[test]
fn exact_requirement_dependency_and_parameter_limits_pass_and_one_less_fails() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::PathFill,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::PathStroke,
            parameter: 0,
            dependencies: &[0],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::Clip,
            parameter: 0,
            dependencies: &[0, 1],
            status: CapabilityStatus::Supported,
        },
    ]);
    let exact = PolicyLimits::validate(PolicyLimitConfig {
        max_requirements: 3,
        max_parameters: 3,
        max_dependencies: 3,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    assert!(
        CapabilityEvaluator::new(CapabilityProfile::default(), exact)
            .evaluate(&scene, 23, &Never)
            .is_ok()
    );

    for (kind, limits) in [
        (
            PolicyLimitKind::Requirements,
            PolicyLimitConfig {
                max_requirements: 2,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::Parameters,
            PolicyLimitConfig {
                max_parameters: 2,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::Dependencies,
            PolicyLimitConfig {
                max_dependencies: 2,
                ..PolicyLimitConfig::default()
            },
        ),
    ] {
        let limits = PolicyLimits::validate(limits).unwrap();
        let error = CapabilityEvaluator::new(CapabilityProfile::default(), limits)
            .evaluate(&scene, 23, &Never)
            .unwrap_err();
        assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
        assert_eq!(error.category(), PolicyErrorCategory::Resource);
        assert_eq!(error.limit().unwrap().kind(), kind);
    }
}

#[test]
fn dependency_fanout_exact_limit_passes_and_one_less_is_an_explicit_rejection() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::PathFill,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::DeviceColor,
            parameter: 3,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::Clip,
            parameter: 0,
            dependencies: &[0, 1],
            status: CapabilityStatus::Supported,
        },
    ]);
    let exact = PolicyLimits::validate(PolicyLimitConfig {
        max_dependencies_per_requirement: 2,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    let exact_decision = CapabilityEvaluator::new(CapabilityProfile::default(), exact)
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(exact_decision.status(), ProductStatus::Supported);

    let one_less = PolicyLimits::validate(PolicyLimitConfig {
        max_dependencies_per_requirement: 1,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    let decision = CapabilityEvaluator::new(CapabilityProfile::default(), one_less)
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(decision.status(), ProductStatus::Rejected);
    assert_eq!(
        decision.rejection_code(),
        Some(CapabilityRejectionCode::DependencyFanoutProhibited)
    );
    assert_eq!(decision.missing_total(), 0);
}

#[test]
fn cancellation_is_a_distinct_terminal_error_and_replay_is_deterministic() {
    let scene = scene(&[RequirementSpec {
        capability: GraphicsCapability::PathFill,
        parameter: 0,
        dependencies: &[],
        status: CapabilityStatus::Supported,
    }]);
    let error = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &CancelAfter::new(2))
        .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::Cancelled);
    assert_eq!(error.category(), PolicyErrorCategory::Cancelled);

    let first = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &Never)
        .unwrap();
    let second = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &Never)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.hash(), second.hash());
    assert!(!first.hash().is_zero());
}

#[test]
fn cancellation_at_the_final_decision_seal_poll_suppresses_publication() {
    let scene = scene(&[RequirementSpec {
        capability: GraphicsCapability::PathFill,
        parameter: 0,
        dependencies: &[],
        status: CapabilityStatus::Supported,
    }]);
    let counting = CountingNever::default();
    let decision = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &counting)
        .unwrap();
    assert_eq!(decision.status(), ProductStatus::Supported);
    let total_polls = counting.calls();
    assert!(total_polls > 1);

    let error = CapabilityEvaluator::default()
        .evaluate(&scene, 23, &CancelAfter::new(total_polls - 1))
        .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::Cancelled);
}

#[test]
fn canonical_scene_serialization_observes_policy_cancellation() {
    let scene = scene(&[]);
    let limits = PolicyLimits::validate(PolicyLimitConfig {
        cancellation_interval: 1,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    let error = CapabilityEvaluator::new(CapabilityProfile::default(), limits)
        .evaluate(&scene, 23, &CancelAfter::new(2))
        .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::Cancelled);
}
