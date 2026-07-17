mod support;

use pdf_rs_policy::{
    AlphaMode, CapabilityContributorKind, CapabilityStatus, CollectionCompleteness, NativeBackend,
    PixelFormat, QualityPolicy,
};
use pdf_rs_protocol as wire;
use pdf_rs_scene::{CapabilityStatus as SceneCapabilityStatus, GraphicsCapability};

use support::{RequirementSpec, evaluate, fast_config, plan, ready, request, scene};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn product_discriminants_match_the_frozen_generated_wire_contract() {
    assert_eq!(
        CapabilityStatus::Supported as u8,
        wire::SupportStatus::Supported as u8
    );
    assert_eq!(
        CapabilityStatus::Unsupported as u8,
        wire::SupportStatus::Unsupported as u8
    );
    assert_eq!(
        CapabilityStatus::Rejected as u8,
        wire::SupportStatus::Rejected as u8
    );
    assert_eq!(
        CollectionCompleteness::Complete as u8,
        wire::CollectionCompleteness::Complete as u8
    );
    assert_eq!(
        CollectionCompleteness::Truncated as u8,
        wire::CollectionCompleteness::Truncated as u8
    );
    assert_eq!(
        CapabilityContributorKind::SceneRequirement as u8,
        wire::CapabilityContributorKind::Scene as u8
    );
    assert_eq!(
        CapabilityContributorKind::PolicyDependencyClosure as u8,
        wire::CapabilityContributorKind::Policy as u8
    );
    assert_eq!(
        NativeBackend::ReferenceCpu as u8,
        wire::NativeBackend::ReferenceCpu as u8
    );
    assert_eq!(
        NativeBackend::FastCpu as u8,
        wire::NativeBackend::FastCpu as u8
    );
    assert_eq!(
        QualityPolicy::Preview as u8,
        wire::QualityPolicy::Preview as u8
    );
    assert_eq!(QualityPolicy::Full as u8, wire::QualityPolicy::Full as u8);
    assert_eq!(PixelFormat::Rgba8 as u8, wire::PixelFormat::Rgba8 as u8);
    assert_eq!(AlphaMode::Straight as u8, wire::AlphaMode::Straight as u8);
    assert_eq!(
        AlphaMode::Premultiplied as u8,
        wire::AlphaMode::Premultiplied as u8
    );
}

#[test]
fn decision_projection_uses_generated_tags_and_little_endian_codec() {
    let scene = scene(&[RequirementSpec {
        capability: GraphicsCapability::SoftMask,
        parameter: 0x0102_0304_0506_0708,
        dependencies: &[],
        status: SceneCapabilityStatus::Unsupported,
    }]);
    let decision = evaluate(&scene, 0x0102_0304_0506_0708);
    assert_eq!(
        hex(decision.hash().digest()),
        "40f18f36b42cbf0723345d50d5c2bdf175d20d4c775cb9ba593de33faabf63fc"
    );
    let projection = decision.protocol_projection().unwrap();

    assert!(projection.wire_invariants_valid());
    assert_eq!(
        projection.profile,
        wire::CapabilityProfileId::BaselineNative
    );
    assert_eq!(projection.missing[0].id, 1);
    assert_eq!(projection.contributors[0].id, 1);
    assert_eq!(
        projection.missing[0].scope.kind,
        wire::CapabilityScopeKind::Page
    );
    assert_eq!(
        projection.missing[0].context.code,
        wire::CapabilityScopeKind::Page as u32
    );
    assert_eq!(projection.missing[0].context.value, 3);
    assert_eq!(projection.locations_total, decision.locations_total());
    assert_eq!(
        projection.locations_completeness as u8,
        decision.locations_completeness() as u8
    );
    assert_eq!(
        projection.evaluated_requirements,
        decision.evaluated_requirements()
    );
    assert_eq!(
        projection.evaluated_dependencies,
        decision.evaluated_dependencies()
    );
    assert_eq!(
        projection.evaluated_parameters,
        decision.evaluated_parameters()
    );
    assert_eq!(projection.evaluated_commands, decision.evaluated_commands());
    assert_eq!(
        projection.evaluated_resources,
        decision.evaluated_resources()
    );

    let payload = wire::encode_capability_decision_payload(
        &projection,
        wire::PayloadCodecLimits::protocol_default(),
    )
    .unwrap();
    assert_eq!(&payload[0..2], &1_u16.to_le_bytes());
    assert_eq!(payload[2], wire::SupportStatus::Unsupported as u8);
    assert_eq!(
        &payload[3..7],
        &(wire::CapabilityProfileId::BaselineNative as u32).to_le_bytes()
    );
    assert_eq!(
        wire::decode_capability_decision_payload(
            &payload,
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap(),
        projection
    );

    let changed = evaluate(&scene, 0x0102_0304_0506_0709);
    assert_ne!(decision.hash(), changed.hash());
    assert_ne!(
        decision.protocol_projection().unwrap(),
        changed.protocol_projection().unwrap()
    );
}

#[test]
fn decision_projection_rebases_ids_and_preserves_retained_references() {
    let scene = scene(&[
        RequirementSpec {
            capability: GraphicsCapability::PathFill,
            parameter: 0,
            dependencies: &[],
            status: SceneCapabilityStatus::Supported,
        },
        RequirementSpec {
            capability: GraphicsCapability::DeviceColor,
            parameter: 3,
            dependencies: &[],
            status: SceneCapabilityStatus::Unsupported,
        },
        RequirementSpec {
            capability: GraphicsCapability::PathStroke,
            parameter: 0,
            dependencies: &[1],
            status: SceneCapabilityStatus::Supported,
        },
    ]);
    let projection = evaluate(&scene, 23).protocol_projection().unwrap();

    assert!(projection.wire_invariants_valid());
    assert_eq!(
        projection
            .missing
            .iter()
            .map(|requirement| requirement.id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(projection.missing[1].dependencies, [2]);
    assert_eq!(projection.missing[0].contributor_ids, [1]);
    assert_eq!(projection.missing[1].contributor_ids, [2]);
    assert_eq!(
        projection
            .contributors
            .iter()
            .map(|contributor| contributor.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn render_plan_hash_binds_the_exact_generated_manifest_deterministically() {
    let scene = scene(&[]);
    let first = ready(plan(
        &scene,
        evaluate(&scene, 0x0102_0304_0506_0708),
        request(41, 513, 257),
        fast_config(),
        7,
        pdf_rs_policy::PolicyLimits::default(),
    ));
    let second = ready(plan(
        &scene,
        evaluate(&scene, 0x0102_0304_0506_0708),
        request(41, 513, 257),
        fast_config(),
        7,
        pdf_rs_policy::PolicyLimits::default(),
    ));
    assert_eq!(
        hex(first.hash().digest()),
        "2a8549079a52f489c403a538206dce4411d1b95ebbe694a5c0fc147368f897dc"
    );

    assert_eq!(first.hash(), second.hash());
    assert_eq!(first.protocol_manifest(), second.protocol_manifest());
    assert_eq!(first.protocol_manifest().plan_id.value(), 41);
    assert_eq!(first.protocol_manifest().plan_schema_version, 1);
    assert_eq!(first.protocol_manifest().generation, 41);
    assert_eq!(
        first.protocol_manifest().geometry_hash.into_digest(),
        first.viewport().geometry_hash().into_digest()
    );
    assert_eq!(first.protocol_manifest().zoom_numerator, 3);
    assert_eq!(first.protocol_manifest().zoom_denominator, 2);
    assert_eq!(first.protocol_manifest().device_scale_milli, 2_000);
    assert_eq!(
        first.protocol_manifest().rotation,
        wire::PageRotation::Degrees0
    );
    assert_eq!(first.protocol_manifest().optional_content, 5);
    assert_eq!(first.protocol_manifest().annotation_revision, 9);
    assert_eq!(first.protocol_manifest().regions.len(), 6);
    assert_eq!(first.protocol_manifest().tile_content_hashes.len(), 6);
    assert_eq!(
        first.protocol_manifest().decision_hash.into_digest(),
        first.decision().hash().into_digest()
    );
    assert!(first.protocol_manifest().wire_invariants_valid());
    assert_eq!(
        first
            .protocol_manifest()
            .tile_content_hashes
            .iter()
            .map(|hash| hash.into_digest())
            .collect::<Vec<_>>(),
        first
            .tiles()
            .iter()
            .map(|tile| {
                assert_eq!(tile.content_key().decision_hash(), first.decision().hash());
                tile.content_key().hash().into_digest()
            })
            .collect::<Vec<_>>()
    );
    assert!(
        first
            .protocol_manifest()
            .regions
            .iter()
            .all(|region| region.coordinate_space
                == wire::SurfaceCoordinateSpace::DevicePixelsTopLeft)
    );

    let payload = wire::encode_render_plan_manifest_payload(
        first.protocol_manifest(),
        wire::PayloadCodecLimits::protocol_default(),
    )
    .unwrap();
    assert_eq!(&payload[0..2], &1_u16.to_le_bytes());
    assert_eq!(&payload[2..10], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(
        wire::decode_render_plan_manifest_payload(
            &payload,
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap(),
        *first.protocol_manifest()
    );

    let changed_epoch = ready(plan(
        &scene,
        evaluate(&scene, 0x0102_0304_0506_0708),
        request(41, 513, 257),
        fast_config(),
        8,
        pdf_rs_policy::PolicyLimits::default(),
    ));
    assert_ne!(first.protocol_manifest(), changed_epoch.protocol_manifest());
    assert_ne!(first.hash(), changed_epoch.hash());
}
