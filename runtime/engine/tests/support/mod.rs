#![allow(dead_code)]

use std::sync::Arc;

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_engine::{NativeWorkerConfig, NativeWorkerRegistry, OpenCompletion, Reentry};
use pdf_rs_protocol::{
    Correlation, OpenCommand, OutputProfile, PageCoordinateSpace, PageGeometry as WirePageGeometry,
    PageRotation as WirePageRotation, PageViewport, QualityPolicy as WireQualityPolicy, RequestId,
    SessionId, SetViewportCommand, SourceDescriptor, SourceIdentity as WireSourceIdentity,
    ViewportRequest, WorkerId,
};
use pdf_rs_scene::{
    CapabilityContext, CapabilityStatus, GraphicsCapability, GraphicsSceneBuilder,
    GraphicsSceneLimits, PageGeometry, PageRotation, Scene, SceneBinding, SceneRect, SceneScalar,
};
use pdf_rs_surface::WorkerEpoch;
use pdf_rs_syntax::ObjectRef;

pub const PAGE_INDEX: u32 = 0;
pub const DOCUMENT_REVISION: u64 = 23;

pub fn source_descriptor() -> SourceDescriptor {
    SourceDescriptor {
        identity: WireSourceIdentity {
            stable_id: [7; 32],
            revision: 11,
        },
        length: Some(4096),
        validator: [9; 32],
    }
}

pub fn open_correlation(worker: WorkerId, request: u64) -> Correlation {
    Correlation {
        worker,
        session: None,
        request: Some(RequestId::new(request)),
        generation: None,
    }
}

pub fn session_correlation(worker: WorkerId, session: SessionId) -> Correlation {
    Correlation {
        worker,
        session: Some(session),
        request: None,
        generation: None,
    }
}

pub fn generation_correlation(
    worker: WorkerId,
    session: SessionId,
    generation: u64,
) -> Correlation {
    Correlation {
        worker,
        session: Some(session),
        request: None,
        generation: Some(generation),
    }
}

pub fn worker() -> NativeWorkerRegistry {
    NativeWorkerRegistry::new(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            Default::default(),
        )
        .unwrap(),
    )
    .unwrap()
}

pub fn open_ready(worker: &mut NativeWorkerRegistry, scene: Scene) -> SessionId {
    open_ready_with_request(worker, scene, 1)
}

pub fn open_ready_with_request(
    worker: &mut NativeWorkerRegistry,
    scene: Scene,
    request_value: u64,
) -> SessionId {
    let correlation = open_correlation(worker.worker(), request_value);
    let session = worker
        .open(
            &correlation,
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let worker_id = worker.worker();
    let worker_epoch = worker.worker_epoch();
    worker
        .enqueue_reentry(Reentry::Open(OpenCompletion::Ready {
            worker: worker_id,
            worker_epoch,
            session,
            request: RequestId::new(request_value),
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(scene)],
        }))
        .unwrap();
    worker.pump().unwrap();
    session
}

pub fn viewport(generation: u64) -> SetViewportCommand {
    let scene = supported_scene();
    SetViewportCommand {
        viewport: ViewportRequest {
            generation,
            document_revision: DOCUMENT_REVISION,
            annotation_revision: 1,
            zoom_numerator: 1,
            zoom_denominator: 1,
            visible_pages: vec![PageViewport {
                page_index: PAGE_INDEX,
                coordinate_space: PageCoordinateSpace::PdfPointsBottomLeft,
                geometry: wire_geometry(&scene),
                clip_x_milli_points: 0,
                clip_y_milli_points: 0,
                clip_width_milli_points: 16_000,
                clip_height_milli_points: 16_000,
            }],
            quality: WireQualityPolicy::Full,
            output_profile: OutputProfile::Srgb,
            device_scale_milli: 1_000,
            rotation: WirePageRotation::Degrees0,
            optional_content_id: 1,
        },
    }
}

pub fn supported_scene() -> Scene {
    scene_at(PAGE_INDEX, CapabilityStatus::Supported)
}

pub fn unsupported_scene() -> Scene {
    scene_at(PAGE_INDEX, CapabilityStatus::Unsupported)
}

pub fn scene_at(page_index: u32, status: CapabilityStatus) -> Scene {
    scene_at_size(page_index, status, 16_000, 16_000)
}

pub fn scene_at_size(
    page_index: u32,
    status: CapabilityStatus,
    width_milli_points: i64,
    height_milli_points: i64,
) -> Scene {
    scene_with_geometry(
        page_index,
        status,
        [0, 0, width_milli_points, height_milli_points],
        PageRotation::Degrees0,
    )
}

pub fn scene_with_geometry(
    page_index: u32,
    status: CapabilityStatus,
    crop_milli_points: [i64; 4],
    rotation: PageRotation,
) -> Scene {
    scene_with_scaled_geometry(
        page_index,
        status,
        crop_milli_points.map(|coordinate| coordinate * 1_000_000),
        rotation,
    )
}

pub fn scene_with_scaled_geometry(
    page_index: u32,
    status: CapabilityStatus,
    crop_scaled: [i64; 4],
    rotation: PageRotation,
) -> Scene {
    let source = SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(11));
    let binding = SceneBinding::new(
        source,
        19,
        page_index,
        ObjectRef::new(41 + page_index, 0).unwrap(),
    );
    let page = SceneRect::new([
        SceneScalar::from_scaled(crop_scaled[0]),
        SceneScalar::from_scaled(crop_scaled[1]),
        SceneScalar::from_scaled(crop_scaled[2]),
        SceneScalar::from_scaled(crop_scaled[3]),
    ])
    .unwrap();
    let mut builder = GraphicsSceneBuilder::new_v2(
        binding,
        PageGeometry::new(page, page, rotation),
        GraphicsSceneLimits::default(),
    );
    builder
        .add_requirement(
            GraphicsCapability::PathFill,
            0,
            CapabilityContext::Scene,
            Vec::new(),
            status,
        )
        .unwrap();
    builder.finish().unwrap()
}

pub fn wire_geometry(scene: &Scene) -> WirePageGeometry {
    let geometry = scene.geometry();
    let media = geometry
        .media_box()
        .coordinates()
        .map(|value| value.scaled());
    let crop = geometry
        .crop_box()
        .coordinates()
        .map(|value| value.scaled());
    WirePageGeometry {
        identity: pdf_rs_policy::page_geometry_identity(scene).unwrap(),
        media_box_x_milli_points: i32::try_from(rounded_milli_points(media[0])).unwrap(),
        media_box_y_milli_points: i32::try_from(rounded_milli_points(media[1])).unwrap(),
        media_box_width_milli_points: u32::try_from(rounded_milli_points(media[2] - media[0]))
            .unwrap(),
        media_box_height_milli_points: u32::try_from(rounded_milli_points(media[3] - media[1]))
            .unwrap(),
        crop_box_x_milli_points: i32::try_from(rounded_milli_points(crop[0])).unwrap(),
        crop_box_y_milli_points: i32::try_from(rounded_milli_points(crop[1])).unwrap(),
        crop_box_width_milli_points: u32::try_from(rounded_milli_points(crop[2] - crop[0]))
            .unwrap(),
        crop_box_height_milli_points: u32::try_from(rounded_milli_points(crop[3] - crop[1]))
            .unwrap(),
        intrinsic_rotation: match geometry.rotation() {
            PageRotation::Degrees0 => WirePageRotation::Degrees0,
            PageRotation::Degrees90 => WirePageRotation::Degrees90,
            PageRotation::Degrees180 => WirePageRotation::Degrees180,
            PageRotation::Degrees270 => WirePageRotation::Degrees270,
        },
    }
}

fn rounded_milli_points(scaled: i64) -> i64 {
    let quotient = scaled / 1_000_000;
    let remainder = scaled % 1_000_000;
    if remainder.unsigned_abs() * 2 >= 1_000_000 {
        quotient + if scaled.is_negative() { -1 } else { 1 }
    } else {
        quotient
    }
}
