use pdf_rs_protocol as wire;
use pdf_rs_scene::PageRotation;

use crate::capability::CancellationWork;
use crate::render_plan::RENDER_PLAN_SCHEMA_VERSION;
use crate::{
    CapabilityContributorKind, CapabilityDecision, CapabilityLocation, CapabilityScope,
    CapabilityStatus, CollectionCompleteness, NativeBackend, OutputProfile, PolicyError,
    QualityPolicy, RenderConfig, RenderPlanId, RendererEpoch, TileContentKey, ViewportIdentity,
};

pub(crate) fn capability_decision(
    decision: &CapabilityDecision,
) -> Result<wire::CapabilityDecision, PolicyError> {
    capability_decision_with_observer(decision, || Ok(()))
}

fn capability_decision_observed(
    decision: &CapabilityDecision,
    work: &mut CancellationWork<'_>,
) -> Result<wire::CapabilityDecision, PolicyError> {
    capability_decision_with_observer(decision, || work.step())
}

fn capability_decision_with_observer(
    decision: &CapabilityDecision,
    mut observe: impl FnMut() -> Result<(), PolicyError>,
) -> Result<wire::CapabilityDecision, PolicyError> {
    let mut missing = Vec::new();
    missing
        .try_reserve_exact(decision.missing().len())
        .map_err(|_| PolicyError::allocation())?;
    for requirement in decision.missing() {
        observe()?;
        let id = protocol_id(requirement.id())?;
        let location = requirement.location().map(capability_location);
        let scope = capability_scope(requirement.scope());
        let (context_code, context_value) = capability_context(requirement.scope());

        let mut dependencies = Vec::new();
        dependencies
            .try_reserve_exact(requirement.dependencies().len())
            .map_err(|_| PolicyError::allocation())?;
        for dependency in requirement.dependencies().iter().copied() {
            observe()?;
            if decision
                .missing()
                .binary_search_by_key(&dependency, |candidate| candidate.id())
                .is_ok()
            {
                dependencies.push(protocol_id(dependency)?);
            }
        }

        let mut contributor_ids = Vec::new();
        contributor_ids
            .try_reserve_exact(requirement.contributor_ids().len())
            .map_err(|_| PolicyError::allocation())?;
        for contributor in requirement.contributor_ids().iter().copied() {
            observe()?;
            contributor_ids.push(protocol_id(contributor)?);
        }

        missing.push(wire::CapabilityRequirement {
            id,
            capability: u16::try_from(super::capability::capability_code(requirement.capability()))
                .map_err(|_| PolicyError::numeric_overflow())?,
            parameter: requirement.parameter(),
            context: wire::CapabilityContext {
                code: context_code,
                value: context_value,
                location: location.clone(),
            },
            dependencies,
            scope,
            contributor_ids,
            location,
        });
    }

    let mut contributors = Vec::new();
    contributors
        .try_reserve_exact(decision.contributors().len())
        .map_err(|_| PolicyError::allocation())?;
    for contributor in decision.contributors() {
        observe()?;
        contributors.push(wire::CapabilityContributor {
            id: protocol_id(contributor.id())?,
            kind: match contributor.kind() {
                CapabilityContributorKind::SceneRequirement => {
                    wire::CapabilityContributorKind::Scene
                }
                CapabilityContributorKind::PolicyDependencyClosure => {
                    wire::CapabilityContributorKind::Policy
                }
            },
            code: contributor.code(),
            location: contributor.location().map(capability_location),
        });
    }

    let projection = wire::CapabilityDecision {
        decision_schema_version: decision.schema_version(),
        status: match decision.status() {
            CapabilityStatus::Supported => wire::SupportStatus::Supported,
            CapabilityStatus::Unsupported => wire::SupportStatus::Unsupported,
            CapabilityStatus::Rejected => wire::SupportStatus::Rejected,
        },
        profile: decision.profile().id(),
        profile_version: decision.profile().profile_version(),
        policy_version: decision.profile().policy_version(),
        subject: capability_subject(decision.subject()),
        missing,
        missing_total: decision.missing_total(),
        missing_completeness: collection_completeness(decision.missing_completeness()),
        contributors,
        contributors_total: decision.contributors_total(),
        contributors_completeness: collection_completeness(decision.contributors_completeness()),
        locations_total: decision.locations_total(),
        locations_completeness: collection_completeness(decision.locations_completeness()),
        evaluated_requirements: decision.evaluated_requirements(),
        evaluated_dependencies: decision.evaluated_dependencies(),
        evaluated_parameters: decision.evaluated_parameters(),
        evaluated_commands: decision.evaluated_commands(),
        evaluated_resources: decision.evaluated_resources(),
        scope: capability_scope(decision.scope()),
        location: decision.location().map(capability_location),
        rejection_code: decision.rejection_code().map(|code| code as u32),
    };
    if !projection.wire_invariants_valid() {
        return Err(PolicyError::identity_mismatch());
    }
    Ok(projection)
}

pub(crate) fn capability_decision_hash_preimage(
    decision: &CapabilityDecision,
    work: &mut CancellationWork<'_>,
) -> Result<Vec<u8>, PolicyError> {
    let projection = capability_decision_observed(decision, work)?;
    let mut observer = ProtocolWorkObserver::new(work);
    let result = wire::capability_decision_hash_preimage_observed(
        &projection,
        wire::PayloadCodecLimits::protocol_default(),
        &mut observer,
    );
    observer.finish(result)
}

#[allow(dead_code)]
pub(crate) fn render_plan_manifest(
    decision: &CapabilityDecision,
    viewport: ViewportIdentity,
    config: RenderConfig,
    renderer_epoch: RendererEpoch,
    plan_id: RenderPlanId,
    tiles: &[TileContentKey],
    work: &mut CancellationWork<'_>,
) -> Result<wire::RenderPlanManifest, PolicyError> {
    if plan_id.value() == 0
        || plan_id.value() != viewport.generation()
        || tiles.is_empty()
        || config.output_profile() != OutputProfile::SRGB_RGBA8_STRAIGHT
    {
        return Err(PolicyError::identity_mismatch());
    }
    let subject = decision.subject();
    let mut regions = Vec::new();
    regions
        .try_reserve_exact(tiles.len())
        .map_err(|_| PolicyError::allocation())?;
    let mut tile_content_hashes = Vec::new();
    tile_content_hashes
        .try_reserve_exact(tiles.len())
        .map_err(|_| PolicyError::allocation())?;
    for tile in tiles {
        work.step()?;
        if tile.source() != subject.source()
            || tile.document_revision() != subject.document_revision()
            || tile.revision_startxref() != subject.revision_startxref()
            || tile.page_index() != subject.page_index()
            || tile.page_object_number() != subject.page_object_number()
            || tile.page_object_generation() != subject.page_object_generation()
            || tile.scene_hash() != subject.scene_hash()
            || tile.decision_hash() != decision.hash()
            || tile.geometry_hash() != viewport.geometry_hash()
            || tile.viewport_clip() != viewport.clip()
            || tile.zoom() != viewport.zoom()
            || tile.device_scale_milli() != viewport.device_scale_milli()
            || tile.rotation() != viewport.rotation()
            || tile.optional_content() != viewport.optional_content()
            || tile.annotation_revision() != viewport.annotation_revision()
            || tile.quality() != config.quality()
            || tile.output_profile() != config.output_profile()
            || tile.render_config_hash() != config.hash()
            || tile.renderer_epoch() != renderer_epoch
            || tile.backend() != config.backend()
            || tile.hash().is_zero()
        {
            return Err(PolicyError::identity_mismatch());
        }
        let rectangle = tile.tile();
        regions.push(wire::SurfaceRegion {
            page_index: tile.page_index(),
            x: rectangle.x(),
            y: rectangle.y(),
            width: rectangle.width(),
            height: rectangle.height(),
            coordinate_space: wire::SurfaceCoordinateSpace::DevicePixelsTopLeft,
        });
        tile_content_hashes.push(wire::TileContentHash::new(tile.hash().into_digest()));
    }
    if regions.len() > wire::RENDER_PLAN_MANIFEST_REGIONS_MAX_COUNT {
        return Err(PolicyError::identity_mismatch());
    }
    let clip = viewport.clip();
    let manifest = wire::RenderPlanManifest {
        plan_schema_version: RENDER_PLAN_SCHEMA_VERSION,
        document_revision: subject.document_revision(),
        render_config: wire::RenderConfigHash::new(config.hash().into_digest()),
        renderer_epoch: wire::RendererEpoch::new(renderer_epoch.value()),
        plan_id,
        generation: viewport.generation(),
        scene_hash: wire::SceneHash::new(subject.scene_hash().into_digest()),
        decision_hash: wire::CapabilityDecisionHash::new(decision.hash().into_digest()),
        geometry_hash: wire::GeometryHash::new(viewport.geometry_hash().into_digest()),
        viewport_clip: wire::SurfaceRegion {
            page_index: subject.page_index(),
            x: clip.x(),
            y: clip.y(),
            width: clip.width(),
            height: clip.height(),
            coordinate_space: wire::SurfaceCoordinateSpace::DevicePixelsTopLeft,
        },
        zoom_numerator: viewport.zoom().numerator(),
        zoom_denominator: viewport.zoom().denominator(),
        device_scale_milli: viewport.device_scale_milli(),
        rotation: page_rotation(viewport.rotation()),
        optional_content: viewport.optional_content().value(),
        annotation_revision: viewport.annotation_revision(),
        backend: match config.backend() {
            NativeBackend::ReferenceCpu => wire::NativeBackend::ReferenceCpu,
            NativeBackend::FastCpu => wire::NativeBackend::FastCpu,
        },
        output_profile: wire::OutputProfile::Srgb,
        quality: match config.quality() {
            QualityPolicy::Preview => wire::QualityPolicy::Preview,
            QualityPolicy::Full => wire::QualityPolicy::Full,
        },
        regions,
        tile_content_hashes,
    };
    if !manifest.wire_invariants_valid() {
        return Err(PolicyError::identity_mismatch());
    }
    Ok(manifest)
}

pub(crate) fn render_plan_manifest_hash_preimage(
    manifest: &wire::RenderPlanManifest,
    work: &mut CancellationWork<'_>,
) -> Result<Vec<u8>, PolicyError> {
    let mut observer = ProtocolWorkObserver::new(work);
    let result = wire::render_plan_manifest_hash_preimage_observed(
        manifest,
        wire::PayloadCodecLimits::protocol_default(),
        &mut observer,
    );
    observer.finish(result)
}

struct ProtocolWorkObserver<'work, 'cancellation> {
    work: &'work mut CancellationWork<'cancellation>,
    error: Option<PolicyError>,
}

impl<'work, 'cancellation> ProtocolWorkObserver<'work, 'cancellation> {
    fn new(work: &'work mut CancellationWork<'cancellation>) -> Self {
        Self { work, error: None }
    }

    fn finish<T>(self, result: Result<T, wire::PayloadCodecError>) -> Result<T, PolicyError> {
        match (result, self.error) {
            (_, Some(error)) => Err(error),
            (Ok(value), None) => Ok(value),
            (Err(_), None) => Err(PolicyError::identity_mismatch()),
        }
    }
}

impl wire::PayloadCodecObserver for ProtocolWorkObserver<'_, '_> {
    fn observe(&mut self) -> bool {
        if self.error.is_some() {
            return false;
        }
        match self.work.step() {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
}

fn protocol_id(zero_based: u32) -> Result<u32, PolicyError> {
    zero_based
        .checked_add(1)
        .ok_or_else(PolicyError::numeric_overflow)
}

#[allow(dead_code)]
const fn page_rotation(rotation: PageRotation) -> wire::PageRotation {
    match rotation {
        PageRotation::Degrees0 => wire::PageRotation::Degrees0,
        PageRotation::Degrees90 => wire::PageRotation::Degrees90,
        PageRotation::Degrees180 => wire::PageRotation::Degrees180,
        PageRotation::Degrees270 => wire::PageRotation::Degrees270,
    }
}

fn capability_subject(subject: crate::CapabilitySubject) -> wire::CapabilitySubject {
    wire::CapabilitySubject {
        source: wire::SourceIdentity {
            stable_id: subject.source().stable_id().digest(),
            revision: subject.source().revision().value(),
        },
        document_revision: subject.document_revision(),
        revision_startxref: subject.revision_startxref(),
        page_index: subject.page_index(),
        page_object_number: subject.page_object_number(),
        page_object_generation: subject.page_object_generation(),
        scene_schema_major: subject.scene_schema_major(),
        scene_schema_minor: subject.scene_schema_minor(),
        scene_hash: wire::SceneHash::new(subject.scene_hash().into_digest()),
    }
}

fn capability_location(location: CapabilityLocation) -> wire::CapabilityLocation {
    wire::CapabilityLocation {
        page_index: location.page_index(),
        object_number: location.object_number(),
        object_generation: location.object_generation(),
        source_offset: location.source_offset(),
        command_index: location.command_index(),
        resource_id: location.resource_id(),
    }
}

fn capability_scope(scope: CapabilityScope) -> wire::CapabilityScope {
    match scope {
        CapabilityScope::Page { page } => wire::CapabilityScope {
            kind: wire::CapabilityScopeKind::Page,
            page: Some(page),
            command: None,
            resource: None,
        },
        CapabilityScope::Command { page, command } => wire::CapabilityScope {
            kind: wire::CapabilityScopeKind::Command,
            page: Some(page),
            command: Some(command),
            resource: None,
        },
        CapabilityScope::Resource { page, resource } => wire::CapabilityScope {
            kind: wire::CapabilityScopeKind::Resource,
            page: Some(page),
            command: None,
            resource: Some(resource),
        },
    }
}

fn capability_context(scope: CapabilityScope) -> (u32, u64) {
    match scope {
        CapabilityScope::Page { page } => (wire::CapabilityScopeKind::Page as u32, u64::from(page)),
        CapabilityScope::Command { command, .. } => (
            wire::CapabilityScopeKind::Command as u32,
            u64::from(command),
        ),
        CapabilityScope::Resource { resource, .. } => (
            wire::CapabilityScopeKind::Resource as u32,
            u64::from(resource),
        ),
    }
}

fn collection_completeness(completeness: CollectionCompleteness) -> wire::CollectionCompleteness {
    match completeness {
        CollectionCompleteness::Complete => wire::CollectionCompleteness::Complete,
        CollectionCompleteness::Truncated => wire::CollectionCompleteness::Truncated,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::wire;
    use crate::capability::CancellationWork;
    use crate::{PolicyCancellation, PolicyErrorCode};

    const PAYLOAD_CODEC_VECTORS: &str =
        include_str!("../../../protocol/generated/payload-codec-vectors.json");

    #[test]
    fn generated_hash_known_answers_drive_exact_policy_preimages_and_sha256() {
        verify_hash_known_answer("CapabilityDecision", |payload| {
            let value = wire::decode_capability_decision_payload(
                payload,
                wire::PayloadCodecLimits::protocol_default(),
            )
            .unwrap();
            wire::capability_decision_hash_preimage(
                &value,
                wire::PayloadCodecLimits::protocol_default(),
            )
            .unwrap()
        });
        verify_hash_known_answer("RenderPlanManifest", |payload| {
            let value = wire::decode_render_plan_manifest_payload(
                payload,
                wire::PayloadCodecLimits::protocol_default(),
            )
            .unwrap();
            wire::render_plan_manifest_hash_preimage(
                &value,
                wire::PayloadCodecLimits::protocol_default(),
            )
            .unwrap()
        });
    }

    #[test]
    fn generated_hash_payload_encoding_observes_policy_cancellation() {
        let decision = wire::decode_capability_decision_payload(
            &hash_known_answer_payload("CapabilityDecision"),
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap();
        let cancellation = CancelOnSecondPoll::default();
        let mut work = CancellationWork::new(&cancellation, 1).unwrap();
        let mut observer = super::ProtocolWorkObserver::new(&mut work);
        let result = wire::capability_decision_hash_preimage_observed(
            &decision,
            wire::PayloadCodecLimits::protocol_default(),
            &mut observer,
        );
        assert_eq!(
            observer.finish(result).unwrap_err().code(),
            PolicyErrorCode::Cancelled
        );

        let manifest = wire::decode_render_plan_manifest_payload(
            &hash_known_answer_payload("RenderPlanManifest"),
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap();
        let cancellation = CancelOnSecondPoll::default();
        let mut work = CancellationWork::new(&cancellation, 1).unwrap();
        assert_eq!(
            super::render_plan_manifest_hash_preimage(&manifest, &mut work)
                .unwrap_err()
                .code(),
            PolicyErrorCode::Cancelled
        );
    }

    #[test]
    fn every_mutable_capability_decision_field_changes_the_hash() {
        let mut decision = wire::decode_capability_decision_payload(
            &hash_known_answer_payload("CapabilityDecision"),
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap();
        let requirement = wire::CapabilityRequirement {
            id: 11,
            capability: 12,
            parameter: 13,
            context: wire::CapabilityContext {
                code: 14,
                value: 15,
                location: Some(location(16)),
            },
            dependencies: vec![17, 18],
            scope: wire::CapabilityScope {
                kind: wire::CapabilityScopeKind::Resource,
                page: Some(19),
                command: Some(20),
                resource: Some(21),
            },
            contributor_ids: vec![22, 23],
            location: Some(location(24)),
        };
        let mut second_requirement = requirement.clone();
        second_requirement.id = 25;
        second_requirement.capability = 26;
        decision.missing = vec![requirement, second_requirement];
        decision.missing_total = 2;
        decision.contributors = vec![
            wire::CapabilityContributor {
                id: 27,
                kind: wire::CapabilityContributorKind::Scene,
                code: 28,
                location: Some(location(29)),
            },
            wire::CapabilityContributor {
                id: 30,
                kind: wire::CapabilityContributorKind::Policy,
                code: 31,
                location: Some(location(32)),
            },
        ];
        decision.contributors_total = 2;
        decision.locations_total = 8;
        decision.scope = wire::CapabilityScope {
            kind: wire::CapabilityScopeKind::Command,
            page: Some(33),
            command: Some(34),
            resource: Some(35),
        };
        decision.location = Some(location(36));
        decision.rejection_code = Some(37);
        let expected = decision_digest(&decision);
        let mut variants = Vec::new();

        macro_rules! push_changed {
            ($body:expr) => {{
                let mut changed = decision.clone();
                $body(&mut changed);
                variants.push(changed);
            }};
        }

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.decision_schema_version += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.status = wire::SupportStatus::Unsupported;
        });
        assert_eq!(
            decision.profile,
            wire::CapabilityProfileId::BaselineNative,
            "CapabilityProfileId has one frozen typed variant, so no safe alternate profile value exists"
        );
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.profile_version += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.policy_version += 1;
        });

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.source.stable_id[0] ^= 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.source.revision += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.document_revision += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.revision_startxref += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.page_index += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.page_object_number += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.page_object_generation += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.scene_schema_major += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.scene_schema_minor += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.subject.scene_hash = wire::SceneHash::new([0x78; 32]);
        });

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].id += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].capability += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].parameter += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].context.code += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].context.value += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .page_index = Some(38);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .object_number = Some(39);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .object_generation = Some(40);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .source_offset = Some(41);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .command_index = Some(42);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .context
                .location
                .as_mut()
                .unwrap()
                .resource_id = Some(43);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].context.location = None;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].dependencies[0] += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].dependencies.push(44);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].dependencies.swap(0, 1);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].scope.kind = wire::CapabilityScopeKind::Page;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].scope.page = Some(45);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].scope.command = Some(46);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].scope.resource = Some(47);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].contributor_ids[0] += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].contributor_ids.push(48);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].contributor_ids.swap(0, 1);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location.as_mut().unwrap().page_index = Some(49);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location.as_mut().unwrap().object_number = Some(50);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0]
                .location
                .as_mut()
                .unwrap()
                .object_generation = Some(51);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location.as_mut().unwrap().source_offset = Some(52);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location.as_mut().unwrap().command_index = Some(53);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location.as_mut().unwrap().resource_id = Some(54);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing[0].location = None;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing.pop();
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing.swap(0, 1);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing_total += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.missing_completeness = wire::CollectionCompleteness::Truncated;
        });

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0].id += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0].kind = wire::CapabilityContributorKind::Renderer;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0].code += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .page_index = Some(55);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .object_number = Some(56);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .object_generation = Some(57);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .source_offset = Some(58);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .command_index = Some(59);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0]
                .location
                .as_mut()
                .unwrap()
                .resource_id = Some(60);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors[0].location = None;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors.pop();
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors.swap(0, 1);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors_total += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.contributors_completeness = wire::CollectionCompleteness::Truncated;
        });

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.locations_total += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.locations_completeness = wire::CollectionCompleteness::Truncated;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.evaluated_requirements += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.evaluated_dependencies += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.evaluated_parameters += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.evaluated_commands += 1;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.evaluated_resources += 1;
        });

        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.scope.kind = wire::CapabilityScopeKind::Session;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.scope.page = Some(61);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.scope.command = Some(62);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.scope.resource = Some(63);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().page_index = Some(64);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().object_number = Some(65);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().object_generation = Some(66);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().source_offset = Some(67);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().command_index = Some(68);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location.as_mut().unwrap().resource_id = Some(69);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.location = None;
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.rejection_code = Some(70);
        });
        push_changed!(|changed: &mut wire::CapabilityDecision| {
            changed.rejection_code = None;
        });

        for variant in variants {
            assert_ne!(decision_digest(&variant), expected);
        }
    }

    fn verify_hash_known_answer(
        type_name: &str,
        preimage_for_payload: impl FnOnce(&[u8]) -> Vec<u8>,
    ) {
        let payload = hash_known_answer_payload(type_name);
        let hash_section = PAYLOAD_CODEC_VECTORS
            .split_once("\"hash_known_answers\":")
            .unwrap()
            .1;
        let marker = format!("{{\"type\":\"{type_name}\"");
        let entry = hash_section.split_once(&marker).unwrap().1;
        let entry = entry.split_once('}').unwrap().0;
        let domain = string_field(entry, "domain");
        let expected_preimage = decode_hex(string_field(entry, "preimage_hex"));
        let expected_digest = decode_hex(string_field(entry, "sha256"));

        let actual_preimage = preimage_for_payload(&payload);
        assert_eq!(actual_preimage, expected_preimage, "{type_name} preimage");
        assert_eq!(
            actual_preimage
                .strip_prefix(domain.as_bytes())
                .and_then(|suffix| suffix.first()),
            Some(&0),
            "{type_name} domain separator"
        );
        assert_eq!(
            crate::canonical_hash::hash_preimage(&actual_preimage)
                .unwrap()
                .as_slice(),
            expected_digest,
            "{type_name} SHA-256"
        );
    }

    fn hash_known_answer_payload(type_name: &str) -> Vec<u8> {
        let hash_section = PAYLOAD_CODEC_VECTORS
            .split_once("\"hash_known_answers\":")
            .unwrap()
            .1;
        let marker = format!("{{\"type\":\"{type_name}\"");
        let entry = hash_section.split_once(&marker).unwrap().1;
        let entry = entry.split_once('}').unwrap().0;
        decode_hex(string_field(entry, "payload_hex"))
    }

    fn decision_digest(decision: &wire::CapabilityDecision) -> [u8; 32] {
        let preimage = wire::capability_decision_hash_preimage(
            decision,
            wire::PayloadCodecLimits::protocol_default(),
        )
        .unwrap();
        crate::canonical_hash::hash_preimage(&preimage).unwrap()
    }

    fn location(seed: u32) -> wire::CapabilityLocation {
        wire::CapabilityLocation {
            page_index: Some(seed),
            object_number: Some(seed + 1),
            object_generation: Some(u16::try_from(seed + 2).unwrap()),
            source_offset: Some(u64::from(seed + 3)),
            command_index: Some(seed + 4),
            resource_id: Some(seed + 5),
        }
    }

    #[derive(Default)]
    struct CancelOnSecondPoll {
        polls: AtomicUsize,
    }

    impl PolicyCancellation for CancelOnSecondPoll {
        fn is_cancelled(&self) -> bool {
            self.polls.fetch_add(1, Ordering::SeqCst) >= 1
        }
    }

    fn string_field<'a>(entry: &'a str, name: &str) -> &'a str {
        let marker = format!("\"{name}\":\"");
        entry
            .split_once(&marker)
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0
    }

    fn decode_hex(input: &str) -> Vec<u8> {
        assert!(input.len().is_multiple_of(2));
        input
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| (hex_digit(pair[0]) << 4) | hex_digit(pair[1]))
            .collect()
    }

    fn hex_digit(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("generated KAT contains non-hex byte"),
        }
    }
}
