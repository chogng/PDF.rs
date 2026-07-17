mod support;

use pdf_rs_policy::{
    CapabilityEvaluator, CapabilityProfile, CapabilityStatus as ProductStatus, DeviceRect,
    OptionalContentIdentity, PolicyErrorCode, PolicyLimitConfig, PolicyLimitKind, PolicyLimits,
    RenderConfig, RenderConfigInput, RenderPlanOutcome, RenderPlanRequest, RendererEpoch,
    ZoomRatio, create_render_plan,
};
use pdf_rs_scene::{CapabilityStatus, GraphicsCapability, PageRotation};

use support::{
    CancelAfter, CountingNever, Never, RequirementSpec, evaluate, fast_config,
    legacy_scene_with_commands_and_resources, plan, ready, request, scene, scene_with_identity,
};

#[test]
fn legacy_non_graphics_scene_never_publishes_a_render_plan() {
    let scene = legacy_scene_with_commands_and_resources();
    let decision = evaluate(&scene, 23);
    assert_eq!(decision.status(), ProductStatus::Rejected);
    let outcome = plan(
        &scene,
        decision.clone(),
        request(41, 256, 256),
        fast_config(),
        7,
        PolicyLimits::default(),
    );
    assert_eq!(outcome, RenderPlanOutcome::NotPublishable(decision));
}

#[test]
fn plan_tiles_are_bounded_complete_and_canonical_row_major() {
    let scene = scene(&[RequirementSpec {
        capability: GraphicsCapability::PathFill,
        parameter: 0,
        dependencies: &[],
        status: CapabilityStatus::Supported,
    }]);
    let decision = evaluate(&scene, 23);
    let plan = ready(plan(
        &scene,
        decision,
        request(41, 513, 257),
        fast_config(),
        7,
        PolicyLimits::default(),
    ));
    assert_eq!(plan.decision().status(), ProductStatus::Supported);
    assert_eq!(plan.tiles().len(), 6);
    assert_eq!(
        plan.tiles()
            .iter()
            .map(|tile| tile.content_key().tile())
            .collect::<Vec<_>>(),
        [
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            DeviceRect::new(256, 0, 256, 256).unwrap(),
            DeviceRect::new(512, 0, 1, 256).unwrap(),
            DeviceRect::new(0, 256, 256, 1).unwrap(),
            DeviceRect::new(256, 256, 256, 1).unwrap(),
            DeviceRect::new(512, 256, 1, 1).unwrap(),
        ]
    );
    assert_ne!(plan.id().value(), 0);
    assert!(!plan.hash().is_zero());
    for (ordinal, tile) in plan.tiles().iter().enumerate() {
        assert_eq!(tile.ordinal(), u32::try_from(ordinal).unwrap());
        assert_eq!(tile.content_key().decision_hash(), plan.decision().hash());
        assert_eq!(tile.generation(), 41);
        assert_eq!(tile.plan_id(), plan.id());
        assert_eq!(tile.plan_hash(), plan.hash());
        assert!(!tile.hash().is_zero());
    }
}

#[test]
fn generation_is_excluded_from_content_keys_and_included_in_planned_identities() {
    let scene = scene(&[]);
    let first = ready(plan(
        &scene,
        evaluate(&scene, 23),
        request(41, 256, 256),
        fast_config(),
        7,
        PolicyLimits::default(),
    ));
    let second = ready(plan(
        &scene,
        evaluate(&scene, 23),
        request(42, 256, 256),
        fast_config(),
        7,
        PolicyLimits::default(),
    ));
    assert_eq!(
        first.tiles()[0].content_key(),
        second.tiles()[0].content_key()
    );
    assert_eq!(
        first.tiles()[0].content_key().hash(),
        second.tiles()[0].content_key().hash()
    );
    assert_ne!(first.hash(), second.hash());
    assert_ne!(first.tiles()[0].hash(), second.tiles()[0].hash());
}

#[test]
fn each_render_identity_family_changes_content_or_generation_hashes() {
    let base_scene = scene(&[]);
    let base_config = fast_config();
    let base = ready(plan(
        &base_scene,
        evaluate(&base_scene, 23),
        request(41, 256, 256),
        base_config,
        7,
        PolicyLimits::default(),
    ));
    let base_hash = base.tiles()[0].content_key().hash();

    let scene_variants = [
        scene_with_identity(&[], 8, 11, 19, 3, 41, 0),
        scene_with_identity(&[], 7, 12, 19, 3, 41, 0),
        scene_with_identity(&[], 7, 11, 20, 3, 41, 0),
        scene_with_identity(&[], 7, 11, 19, 4, 41, 0),
        scene_with_identity(&[], 7, 11, 19, 3, 42, 0),
        scene_with_identity(&[], 7, 11, 19, 3, 41, 1),
    ];
    for variant in &scene_variants {
        let plan = ready(plan(
            variant,
            evaluate(variant, 23),
            request(41, 256, 256),
            base_config,
            7,
            PolicyLimits::default(),
        ));
        assert_ne!(plan.tiles()[0].content_key().hash(), base_hash);
    }

    let requests = [
        request(41, 255, 256),
        RenderPlanRequest::new(
            41,
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            ZoomRatio::new(5, 3).unwrap(),
            2_000,
            PageRotation::Degrees0,
            OptionalContentIdentity::new(5),
            9,
        )
        .unwrap(),
        RenderPlanRequest::new(
            41,
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            ZoomRatio::new(3, 2).unwrap(),
            2_001,
            PageRotation::Degrees0,
            OptionalContentIdentity::new(5),
            9,
        )
        .unwrap(),
        RenderPlanRequest::new(
            41,
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            ZoomRatio::new(3, 2).unwrap(),
            2_000,
            PageRotation::Degrees90,
            OptionalContentIdentity::new(5),
            9,
        )
        .unwrap(),
        RenderPlanRequest::new(
            41,
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            ZoomRatio::new(3, 2).unwrap(),
            2_000,
            PageRotation::Degrees0,
            OptionalContentIdentity::new(6),
            9,
        )
        .unwrap(),
        RenderPlanRequest::new(
            41,
            DeviceRect::new(0, 0, 256, 256).unwrap(),
            ZoomRatio::new(3, 2).unwrap(),
            2_000,
            PageRotation::Degrees0,
            OptionalContentIdentity::new(5),
            10,
        )
        .unwrap(),
    ];
    for variant in requests {
        let plan = ready(plan(
            &base_scene,
            evaluate(&base_scene, 23),
            variant,
            base_config,
            7,
            PolicyLimits::default(),
        ));
        assert_ne!(plan.tiles()[0].content_key().hash(), base_hash);
    }

    let document_revision = ready(plan(
        &base_scene,
        evaluate(&base_scene, 24),
        request(41, 256, 256),
        base_config,
        7,
        PolicyLimits::default(),
    ));
    assert_ne!(document_revision.tiles()[0].content_key().hash(), base_hash);
    let config_variant = RenderConfig::validate(RenderConfigInput {
        cancellation_interval: 257,
        ..RenderConfigInput::fast_cpu_full()
    })
    .unwrap();
    let changed_config = ready(plan(
        &base_scene,
        evaluate(&base_scene, 23),
        request(41, 256, 256),
        config_variant,
        7,
        PolicyLimits::default(),
    ));
    assert_ne!(changed_config.tiles()[0].content_key().hash(), base_hash);
    let changed_epoch = ready(plan(
        &base_scene,
        evaluate(&base_scene, 23),
        request(41, 256, 256),
        base_config,
        8,
        PolicyLimits::default(),
    ));
    assert_ne!(changed_epoch.tiles()[0].content_key().hash(), base_hash);
}

#[test]
fn unsupported_and_rejected_decisions_never_create_partial_tiles() {
    let unsupported_scene = scene(&[RequirementSpec {
        capability: GraphicsCapability::SoftMask,
        parameter: 0,
        dependencies: &[],
        status: CapabilityStatus::Unsupported,
    }]);
    let unsupported = evaluate(&unsupported_scene, 23);
    let outcome = plan(
        &unsupported_scene,
        unsupported.clone(),
        request(41, 256, 256),
        fast_config(),
        7,
        PolicyLimits::default(),
    );
    assert_eq!(outcome, RenderPlanOutcome::NotPublishable(unsupported));

    let rejected_scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::PathFill,
            parameter: 0,
            dependencies: &[],
            status: CapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::PathStroke,
            parameter: 0,
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
    let reject_limits = PolicyLimits::validate(PolicyLimitConfig {
        max_dependencies_per_requirement: 1,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    let rejected = CapabilityEvaluator::new(CapabilityProfile::default(), reject_limits)
        .evaluate(&rejected_scene, 23, &Never)
        .unwrap();
    let outcome = plan(
        &rejected_scene,
        rejected.clone(),
        request(41, 256, 256),
        fast_config(),
        7,
        PolicyLimits::default(),
    );
    assert_eq!(outcome, RenderPlanOutcome::NotPublishable(rejected));
}

#[test]
fn tile_and_output_limits_have_exact_and_one_less_boundaries() {
    let scene = scene(&[]);
    let exact = PolicyLimits::validate(PolicyLimitConfig {
        max_tiles: 2,
        max_output_dimension: 512,
        max_output_pixels: 512 * 256,
        ..PolicyLimitConfig::default()
    })
    .unwrap();
    assert!(matches!(
        plan(
            &scene,
            evaluate(&scene, 23),
            request(41, 512, 256),
            fast_config(),
            7,
            exact
        ),
        RenderPlanOutcome::Ready(_)
    ));

    for (kind, config) in [
        (
            PolicyLimitKind::Tiles,
            PolicyLimitConfig {
                max_tiles: 1,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::OutputDimension,
            PolicyLimitConfig {
                max_output_dimension: 511,
                ..PolicyLimitConfig::default()
            },
        ),
        (
            PolicyLimitKind::OutputPixels,
            PolicyLimitConfig {
                max_output_pixels: 512 * 256 - 1,
                ..PolicyLimitConfig::default()
            },
        ),
    ] {
        let error = create_render_plan(
            &scene,
            evaluate(&scene, 23),
            fast_config(),
            request(41, 512, 256),
            RendererEpoch::new(7).unwrap(),
            PolicyLimits::validate(config).unwrap(),
            &Never,
        )
        .unwrap_err();
        assert_eq!(error.code(), PolicyErrorCode::ResourceLimit);
        assert_eq!(error.limit().unwrap().kind(), kind);
    }
}

#[test]
fn planning_rejects_foreign_scene_identity_and_observes_cancellation() {
    let first = scene(&[]);
    let second = scene_with_identity(&[], 8, 11, 19, 3, 41, 0);
    let error = create_render_plan(
        &second,
        evaluate(&first, 23),
        fast_config(),
        request(41, 256, 256),
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        &Never,
    )
    .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::IdentityMismatch);

    let error = create_render_plan(
        &first,
        evaluate(&first, 23),
        fast_config(),
        request(41, 1024, 1024),
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        &CancelAfter::new(3),
    )
    .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::Cancelled);
}

#[test]
fn cancellation_at_the_final_plan_seal_poll_suppresses_ready_publication() {
    let scene = scene(&[]);
    let decision = evaluate(&scene, 23);
    let counting = CountingNever::default();
    let outcome = create_render_plan(
        &scene,
        decision.clone(),
        fast_config(),
        request(41, 512, 512),
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        &counting,
    )
    .unwrap();
    assert!(matches!(outcome, RenderPlanOutcome::Ready(_)));
    let total_polls = counting.calls();
    assert!(total_polls > 1);

    let error = create_render_plan(
        &scene,
        decision,
        fast_config(),
        request(41, 512, 512),
        RendererEpoch::new(7).unwrap(),
        PolicyLimits::default(),
        &CancelAfter::new(total_polls - 1),
    )
    .unwrap_err();
    assert_eq!(error.code(), PolicyErrorCode::Cancelled);
}

#[test]
fn deterministic_replay_and_native_retry_identity_are_explicit() {
    let scene = scene(&[]);
    let first = ready(plan(
        &scene,
        evaluate(&scene, 23),
        request(41, 512, 512),
        fast_config(),
        7,
        PolicyLimits::default(),
    ));
    let second = ready(plan(
        &scene,
        evaluate(&scene, 23),
        request(41, 512, 512),
        fast_config(),
        7,
        PolicyLimits::default(),
    ));
    assert_eq!(first, second);
    assert!(!first.retry_has_distinct_identity(fast_config(), RendererEpoch::new(7).unwrap()));
    assert!(first.retry_has_distinct_identity(
        RenderConfig::validate(RenderConfigInput::reference_cpu_full()).unwrap(),
        RendererEpoch::new(7).unwrap()
    ));
    assert!(first.retry_has_distinct_identity(fast_config(), RendererEpoch::new(8).unwrap()));
}
