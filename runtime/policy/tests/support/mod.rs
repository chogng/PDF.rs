#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_policy::{
    CapabilityDecision, CapabilityEvaluator, CapabilityProfile, DeviceRect,
    OptionalContentIdentity, PolicyCancellation, PolicyLimits, RenderConfig, RenderConfigInput,
    RenderPlan, RenderPlanOutcome, RenderPlanRequest, RendererEpoch, ZoomRatio, create_render_plan,
};
use pdf_rs_scene::{
    CapabilityContext, CapabilityStatus, CommandSource, GraphicsCapability, GraphicsSceneBuilder,
    GraphicsSceneLimitConfig, GraphicsSceneLimits, PageGeometry, PageRotation, Scene, SceneBinding,
    SceneBuilder, SceneLimits, SceneRect, SceneScalar,
};
use pdf_rs_syntax::ObjectRef;

pub struct RequirementSpec {
    pub capability: GraphicsCapability,
    pub parameter: u64,
    pub dependencies: &'static [u32],
    pub status: CapabilityStatus,
}

pub fn scene(specs: &[RequirementSpec]) -> Scene {
    scene_with_identity(specs, 7, 11, 19, 3, 41, 0)
}

#[allow(clippy::too_many_arguments)]
pub fn scene_with_identity(
    specs: &[RequirementSpec],
    stable_seed: u8,
    source_revision: u64,
    revision_startxref: u64,
    page_index: u32,
    object_number: u32,
    geometry_delta: i64,
) -> Scene {
    scene_with_identity_and_limits(
        specs,
        stable_seed,
        source_revision,
        revision_startxref,
        page_index,
        object_number,
        geometry_delta,
        GraphicsSceneLimits::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn scene_with_identity_and_limits(
    specs: &[RequirementSpec],
    stable_seed: u8,
    source_revision: u64,
    revision_startxref: u64,
    page_index: u32,
    object_number: u32,
    geometry_delta: i64,
    limits: GraphicsSceneLimits,
) -> Scene {
    let source = SourceIdentity::new(
        SourceStableId::new([stable_seed; 32]),
        SourceRevision::new(source_revision),
    );
    let binding = SceneBinding::new(
        source,
        revision_startxref,
        page_index,
        ObjectRef::new(object_number, 0).unwrap(),
    );
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(612_000_000_000 + geometry_delta),
        SceneScalar::from_scaled(792_000_000_000 + geometry_delta),
    ])
    .unwrap();
    let geometry = PageGeometry::new(media, media, PageRotation::Degrees0);
    let mut builder = GraphicsSceneBuilder::new_v2(binding, geometry, limits);
    let mut ids = Vec::new();
    for spec in specs {
        let dependencies = spec
            .dependencies
            .iter()
            .map(|index| ids[usize::try_from(*index).unwrap()])
            .collect();
        ids.push(
            builder
                .add_requirement(
                    spec.capability,
                    spec.parameter,
                    CapabilityContext::Scene,
                    dependencies,
                    spec.status,
                )
                .unwrap(),
        );
    }
    builder.finish().unwrap()
}

pub fn scene_with_canonical_limit(specs: &[RequirementSpec], max_canonical_bytes: u64) -> Scene {
    let limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_canonical_bytes,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    scene_with_identity_and_limits(specs, 7, 11, 19, 3, 41, 0, limits)
}

pub fn legacy_scene_with_commands_and_resources() -> Scene {
    let source = SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(11));
    let binding = SceneBinding::new(source, 19, 3, ObjectRef::new(41, 0).unwrap());
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::from_scaled(612_000_000_000),
        SceneScalar::from_scaled(792_000_000_000),
    ])
    .unwrap();
    let geometry = PageGeometry::new(media, media, PageRotation::Degrees0);
    let mut builder = SceneBuilder::new(binding, geometry, SceneLimits::default());
    let command_source = |operator_index| {
        CommandSource::new(
            ObjectRef::new(42, 0).unwrap(),
            0,
            u64::from(operator_index) * 4,
            3,
            operator_index,
        )
        .unwrap()
    };
    builder
        .begin_marked_content(
            b"Span",
            Some(ObjectRef::new(43, 0).unwrap()),
            command_source(0),
        )
        .unwrap();
    builder.end_marked_content(command_source(1)).unwrap();
    builder.finish().unwrap()
}

pub fn evaluate(scene: &Scene, document_revision: u64) -> CapabilityDecision {
    CapabilityEvaluator::new(
        CapabilityProfile::m3_reference_v1(),
        PolicyLimits::default(),
    )
    .evaluate(scene, document_revision, &Never)
    .unwrap()
}

pub fn request(generation: u64, width: u32, height: u32) -> RenderPlanRequest {
    RenderPlanRequest::new(
        generation,
        DeviceRect::new(0, 0, width, height).unwrap(),
        ZoomRatio::new(3, 2).unwrap(),
        2_000,
        PageRotation::Degrees0,
        OptionalContentIdentity::new(5),
        9,
    )
    .unwrap()
}

pub fn plan(
    scene: &Scene,
    decision: CapabilityDecision,
    request: RenderPlanRequest,
    config: RenderConfig,
    epoch: u32,
    limits: PolicyLimits,
) -> RenderPlanOutcome {
    create_render_plan(
        scene,
        decision,
        config,
        request,
        RendererEpoch::new(epoch).unwrap(),
        limits,
        &Never,
    )
    .unwrap()
}

pub fn ready(outcome: RenderPlanOutcome) -> RenderPlan {
    match outcome {
        RenderPlanOutcome::Ready(plan) => plan,
        RenderPlanOutcome::NotPublishable(decision) => {
            panic!("expected ready plan, got {:?}", decision.status())
        }
    }
}

pub fn fast_config() -> RenderConfig {
    RenderConfig::validate(RenderConfigInput::fast_cpu_full()).unwrap()
}

pub struct Never;

impl PolicyCancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub struct CountingNever {
    calls: AtomicUsize,
}

impl CountingNever {
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PolicyCancellation for CountingNever {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        false
    }
}

pub struct CancelAfter {
    calls: AtomicUsize,
    allowed_calls: usize,
}

impl CancelAfter {
    pub const fn new(allowed_calls: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            allowed_calls,
        }
    }
}

impl PolicyCancellation for CancelAfter {
    fn is_cancelled(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst) >= self.allowed_calls
    }
}
