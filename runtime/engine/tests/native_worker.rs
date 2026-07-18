mod support;

use std::{num::NonZeroU32, sync::Arc};

use pdf_rs_engine::{
    ActorProgress, NativeTaskPoll, NativeWorkerConfig, NativeWorkerEvent, NativeWorkerLimitConfig,
    NativeWorkerPhase, Reentry, SessionPhase,
};
use pdf_rs_policy::{NeverCancelled as NeverCancelledPolicy, PolicyPollBudget};
use pdf_rs_protocol::{
    CancelCommand, CloseSessionCommand, FailDataCommand, GetPageMetricsCommand, OpenCommand,
    ReleaseSurfaceCommand, RequestId, SessionId, ShutdownCommand, SourceFailureCode, WorkerId,
};
use pdf_rs_raster::fast::{
    FastRasterLimitConfig, FastRasterLimits, FastRasterPollBudget, NeverCancelled,
};
use pdf_rs_surface::WorkerEpoch;

use support::{
    DOCUMENT_REVISION, PAGE_INDEX, generation_correlation, open_correlation, open_ready,
    open_ready_with_request, scene_at, scene_at_size, scene_with_geometry,
    scene_with_scaled_geometry, session_correlation, source_descriptor, supported_scene,
    unsupported_scene, viewport, wire_geometry, worker,
};

fn drive_to_queued_raster(
    worker: &mut pdf_rs_engine::NativeWorkerRegistry,
    session: SessionId,
    generation: u64,
) {
    let task = drive_to_raster_task(worker, session, generation);
    let completion = task.run(&NeverCancelled);
    worker.enqueue_reentry(completion).unwrap();
}

fn drive_to_raster_task(
    worker: &mut pdf_rs_engine::NativeWorkerRegistry,
    session: SessionId,
    generation: u64,
) -> pdf_rs_engine::NativeRasterTask {
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, generation),
            &viewport(generation),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    complete_next_policy(worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    complete_next_policy(worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    worker
        .next_raster_task()
        .expect("raster dispatch must expose one external task")
}

fn complete_next_policy(worker: &mut pdf_rs_engine::NativeWorkerRegistry) {
    let completion = worker
        .next_policy_task()
        .expect("policy dispatch must expose one external task")
        .run(&pdf_rs_policy::NeverCancelled);
    worker.enqueue_reentry(completion).unwrap();
}

fn reenter_next_policy(worker: &mut pdf_rs_engine::NativeWorkerRegistry) {
    complete_next_policy(worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
}

fn complete_next_raster(worker: &mut pdf_rs_engine::NativeWorkerRegistry) {
    let completion = worker
        .next_raster_task()
        .expect("raster dispatch must expose one external task")
        .run(&NeverCancelled);
    worker.enqueue_reentry(completion).unwrap();
}

fn drain_events(worker: &mut pdf_rs_engine::NativeWorkerRegistry) -> Vec<NativeWorkerEvent> {
    let mut events = Vec::new();
    while let Some(event) = worker.next_event() {
        events.push(event);
    }
    events
}

fn planned_viewport_region(
    scene: pdf_rs_scene::Scene,
    mut command: pdf_rs_protocol::SetViewportCommand,
) -> pdf_rs_protocol::SurfaceRegion {
    command.viewport.visible_pages[0].geometry = wire_geometry(&scene);
    let mut worker = worker();
    let session = open_ready(&mut worker, scene);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { .. })
    ));
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, command.viewport.generation),
            &command,
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    match worker.next_event() {
        Some(NativeWorkerEvent::GenerationPlanned { event, .. }) => event.manifest.viewport_clip,
        other => panic!("expected GenerationPlanned, got {other:?}"),
    }
}

fn publish_one_surface(
    worker: &mut pdf_rs_engine::NativeWorkerRegistry,
    session: SessionId,
) -> pdf_rs_engine::SurfacePublication {
    drive_to_queued_raster(worker, session, 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    for _ in 0..8 {
        assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
        if let Some(event) = worker.next_event() {
            return match event {
                NativeWorkerEvent::SurfaceReady(publication) => publication,
                other => panic!("expected SurfaceReady, got {other:?}"),
            };
        }
    }
    panic!("expected staged SurfaceReady");
}

#[test]
fn capability_and_plan_delivery_barriers_precede_fast_surface_publication() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { .. })
    ));

    let command = viewport(1);
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &command,
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    assert_eq!(worker.resources().surface().published_surfaces(), 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Idle);
    reenter_next_policy(&mut worker);

    let capability = worker.next_event().unwrap();
    assert!(matches!(
        capability,
        NativeWorkerEvent::CapabilityReported { .. }
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Idle);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { .. })
    ));

    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    assert_eq!(worker.resources().surface().published_surfaces(), 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    let publication = (0..8)
        .find_map(|_| {
            assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
            worker.next_event()
        })
        .map(|event| match event {
            NativeWorkerEvent::SurfaceReady(publication) => publication,
            other => panic!("expected SurfaceReady, got {other:?}"),
        })
        .expect("expected staged SurfaceReady");
    let metadata = &publication.event().metadata;
    assert_eq!(metadata.owner.worker, worker.worker());
    assert_eq!(metadata.owner.session, session);
    assert_eq!(metadata.generation, 1);
    assert_eq!(metadata.backend, pdf_rs_protocol::NativeBackend::FastCpu);
    assert_eq!(metadata.renderer_epoch.value(), 7);
    assert_ne!(metadata.plan_id.value(), 0);
    assert!(metadata.plan_hash.digest().iter().any(|byte| *byte != 0));

    assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Completed
                && event.produced_regions == 1
    ));

    worker
        .release_surface(
            &session_correlation(worker.worker(), session),
            &ReleaseSurfaceCommand {
                surface: metadata.id,
                lease_token: metadata.lease_token,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
            if event.surface == metadata.id
                && event.lease_token == metadata.lease_token
                && event.reason == pdf_rs_protocol::SurfaceReclaimReason::ReleasedByHost
    ));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReleaseAcknowledged { .. })
    ));

    worker
        .release_surface(
            &session_correlation(worker.worker(), session),
            &ReleaseSurfaceCommand {
                surface: metadata.id,
                lease_token: metadata.lease_token,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReleaseAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::AlreadyApplied
    ));
    assert!(worker.next_event().is_none());
}

#[test]
fn viewport_geometry_must_exactly_match_the_source_bound_scene() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();

    let mut wrong_identity = viewport(1);
    wrong_identity.viewport.visible_pages[0].geometry.identity[0] ^= 1;
    let mut wrong_extent = viewport(1);
    wrong_extent.viewport.visible_pages[0]
        .geometry
        .crop_box_width_milli_points += 1;
    let mut wrong_rotation = viewport(1);
    wrong_rotation.viewport.visible_pages[0]
        .geometry
        .intrinsic_rotation = pdf_rs_protocol::PageRotation::Degrees90;

    for command in [wrong_identity, wrong_extent, wrong_rotation] {
        let error = worker
            .set_viewport(
                &generation_correlation(worker.worker(), session, 1),
                &command,
            )
            .unwrap_err();
        assert_eq!(
            error.code(),
            pdf_rs_engine::EngineIntegrationErrorCode::IdentityMismatch
        );
    }
    assert_eq!(worker.pump().unwrap(), ActorProgress::Idle);
}

#[test]
fn page_metrics_are_the_only_source_of_set_viewport_geometry() {
    let mut worker = worker();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: (0..3)
                .map(|page_index| {
                    Arc::new(scene_at(
                        page_index,
                        pdf_rs_scene::CapabilityStatus::Supported,
                    ))
                })
                .collect(),
        }))
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    worker.next_event();

    let metrics_correlation = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: Some(session),
        request: Some(RequestId::new(2)),
        generation: None,
    };
    worker
        .get_page_metrics(
            &metrics_correlation,
            &GetPageMetricsCommand {
                document_revision: DOCUMENT_REVISION,
                start_index: 1,
                max_count: 2,
            },
        )
        .unwrap();
    let pages = match worker.next_event() {
        Some(NativeWorkerEvent::PageMetrics { correlation, event }) => {
            assert_eq!(correlation, metrics_correlation);
            assert_eq!(event.start_index, 1);
            assert_eq!(event.total_pages, 3);
            event.pages
        }
        other => panic!("expected PageMetrics, got {other:?}"),
    };
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].page_index, 1);
    assert_eq!(pages[1].page_index, 2);
    assert_ne!(pages[0].geometry.identity, pages[1].geometry.identity);
    assert!(
        worker
            .get_page_metrics(
                &metrics_correlation,
                &GetPageMetricsCommand {
                    document_revision: DOCUMENT_REVISION,
                    start_index: 0,
                    max_count: 1,
                },
            )
            .is_err()
    );

    let mut command = viewport(1);
    command.viewport.visible_pages[0].page_index = pages[0].page_index;
    command.viewport.visible_pages[0].geometry = pages[0].geometry.clone();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &command,
        )
        .unwrap();
}

#[test]
fn pdf_bottom_left_clips_map_to_crop_relative_top_left_device_regions() {
    let mut bottom_half = viewport(1);
    let page = &mut bottom_half.viewport.visible_pages[0];
    page.clip_y_milli_points = 0;
    page.clip_height_milli_points = 8_000;
    let bottom = planned_viewport_region(supported_scene(), bottom_half);
    assert_eq!(
        (bottom.x, bottom.y, bottom.width, bottom.height),
        (0, 8, 16, 8)
    );

    let mut top_half = viewport(1);
    let page = &mut top_half.viewport.visible_pages[0];
    page.clip_y_milli_points = 8_000;
    page.clip_height_milli_points = 8_000;
    let top = planned_viewport_region(supported_scene(), top_half);
    assert_eq!((top.x, top.y, top.width, top.height), (0, 0, 16, 8));

    let offset_scene = scene_with_geometry(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        [1_000, 2_000, 17_000, 10_000],
        pdf_rs_scene::PageRotation::Degrees0,
    );
    let mut offset_command = viewport(1);
    let page = &mut offset_command.viewport.visible_pages[0];
    page.clip_x_milli_points = 5_000;
    page.clip_y_milli_points = 4_000;
    page.clip_width_milli_points = 4_000;
    page.clip_height_milli_points = 2_000;
    let offset = planned_viewport_region(offset_scene, offset_command);
    assert_eq!(
        (offset.x, offset.y, offset.width, offset.height),
        (4, 4, 4, 2)
    );
}

#[test]
fn clip_mapping_applies_every_combined_quarter_turn_before_device_scaling() {
    let expected = [
        (pdf_rs_protocol::PageRotation::Degrees0, (0, 6, 4, 2)),
        (pdf_rs_protocol::PageRotation::Degrees90, (0, 0, 2, 4)),
        (pdf_rs_protocol::PageRotation::Degrees180, (12, 0, 4, 2)),
        (pdf_rs_protocol::PageRotation::Degrees270, (6, 12, 2, 4)),
    ];
    for (rotation, expected_region) in expected {
        let scene = scene_with_geometry(
            PAGE_INDEX,
            pdf_rs_scene::CapabilityStatus::Supported,
            [0, 0, 16_000, 8_000],
            pdf_rs_scene::PageRotation::Degrees0,
        );
        let mut command = viewport(1);
        command.viewport.rotation = rotation;
        let page = &mut command.viewport.visible_pages[0];
        page.clip_x_milli_points = 0;
        page.clip_y_milli_points = 0;
        page.clip_width_milli_points = 4_000;
        page.clip_height_milli_points = 2_000;
        let region = planned_viewport_region(scene, command);
        assert_eq!(
            (region.x, region.y, region.width, region.height),
            expected_region
        );
    }

    let intrinsically_rotated = scene_with_geometry(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        [0, 0, 16_000, 8_000],
        pdf_rs_scene::PageRotation::Degrees90,
    );
    let mut command = viewport(1);
    command.viewport.rotation = pdf_rs_protocol::PageRotation::Degrees270;
    let page = &mut command.viewport.visible_pages[0];
    page.clip_width_milli_points = 4_000;
    page.clip_height_milli_points = 2_000;
    let region = planned_viewport_region(intrinsically_rotated, command);
    assert_eq!(
        (region.x, region.y, region.width, region.height),
        (0, 6, 4, 2)
    );
}

#[test]
fn high_zoom_clip_mapping_uses_exact_scene_crop_not_rounded_wire_origin() {
    let scene = scene_with_scaled_geometry(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        [500_000, 0, 16_000_500_000, 16_000_000_000],
        pdf_rs_scene::PageRotation::Degrees0,
    );
    let mut command = viewport(1);
    command.viewport.zoom_numerator = 1_000_000;
    let page = &mut command.viewport.visible_pages[0];
    page.clip_x_milli_points = 1;
    page.clip_y_milli_points = 0;
    page.clip_width_milli_points = 1;
    page.clip_height_milli_points = 1;
    let region = planned_viewport_region(scene, command);
    assert_eq!(
        (region.x, region.y, region.width, region.height),
        (500, 15_999_000, 1_000, 1_000)
    );
}

#[test]
fn unsupported_capability_never_enters_raster_or_surface_paths() {
    let mut worker = worker();
    let session = open_ready(&mut worker, unsupported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { event, .. })
            if event.decision.status == pdf_rs_protocol::SupportStatus::Unsupported
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Failed
    ));
    assert!(worker.resources().surface().has_zero_surface_resources());
}

#[test]
fn queued_viewport_replacement_emits_exact_superseded_terminal() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &viewport(2),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        }) if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
    ));
    assert_eq!(worker.resources().queued_normal(), 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { correlation, .. })
            if correlation.generation == Some(2)
    ));
}

#[test]
fn replacement_rewrites_queued_completed_terminal_and_drops_undelivered_surfaces() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    for _ in 0..16 {
        assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
        if worker.resources().queued_events() == 2 {
            break;
        }
    }
    assert_eq!(worker.resources().queued_events(), 2);
    assert_eq!(worker.resources().surface().published_surfaces(), 1);

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &viewport(2),
        )
        .unwrap();
    assert!(worker.next_event().is_none());
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        }) if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
            && event.produced_regions == 0
            && event.error.is_none()
    ));
    assert!(worker.next_event().is_none());
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
}

#[test]
fn generation_reclaims_stream_beyond_critical_event_capacity_before_new_work() {
    let scheduler = pdf_rs_scheduler::SchedulerLimits::new(1, 1, 1, 1, 1, 1, 2, 1, 1, 1).unwrap();
    let limits = NativeWorkerLimitConfig {
        critical_event_capacity: 3,
        scheduler,
        pending_resource_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let scene = scene_at_size(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        1_000_000,
        16_000,
    );
    let session = open_ready(&mut worker, scene);
    worker.next_event();
    let geometry = wire_geometry(&scene_at_size(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        1_000_000,
        16_000,
    ));
    let wide_viewport = |generation| {
        let mut command = viewport(generation);
        let page = &mut command.viewport.visible_pages[0];
        page.geometry = geometry.clone();
        page.clip_width_milli_points = 1_000_000;
        command
    };
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &wide_viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let mut delivered = 0;
    for _ in 0..64 {
        assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
        if let Some(event) = worker.next_event() {
            match event {
                NativeWorkerEvent::SurfaceReady(_) => delivered += 1,
                NativeWorkerEvent::GenerationCompleted { .. } => break,
                other => panic!("unexpected publication event: {other:?}"),
            }
        }
    }
    assert_eq!(delivered, 4);
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &wide_viewport(2),
        )
        .unwrap();

    for _ in 0..delivered {
        assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
                if event.reason == pdf_rs_protocol::SurfaceReclaimReason::GenerationReplaced
        ));
    }
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
}

#[test]
fn mixed_multi_page_viewport_has_one_terminal_and_publishes_no_partial_surface() {
    let mut worker = worker();
    let open = open_correlation(worker.worker(), 1);
    let session = worker
        .open(
            &open,
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request: RequestId::new(1),
            document_revision: DOCUMENT_REVISION,
            scenes: vec![
                Arc::new(scene_at(
                    PAGE_INDEX + 1,
                    pdf_rs_scene::CapabilityStatus::Unsupported,
                )),
                Arc::new(scene_at(
                    PAGE_INDEX,
                    pdf_rs_scene::CapabilityStatus::Supported,
                )),
            ],
        }))
        .unwrap();
    worker.pump().unwrap();
    worker.next_event();

    let mut command = viewport(1);
    let mut unsupported_page = command.viewport.visible_pages[0].clone();
    unsupported_page.page_index = PAGE_INDEX + 1;
    unsupported_page.geometry = wire_geometry(&scene_at(
        PAGE_INDEX + 1,
        pdf_rs_scene::CapabilityStatus::Unsupported,
    ));
    command.viewport.visible_pages.insert(0, unsupported_page);
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &command,
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { event, .. })
            if event.decision.status == pdf_rs_protocol::SupportStatus::Unsupported
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { event, .. })
            if event.decision.status == pdf_rs_protocol::SupportStatus::Supported
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Failed
    ));
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert_eq!(worker.resources().queued_publications(), 0);
}

#[test]
fn close_and_restart_invalidate_surfaces_queued_publications_and_epoch_ids() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    worker.pump().unwrap();
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.resources().surface().published_surfaces(), 1);

    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    worker.pump().unwrap();
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert_eq!(worker.resources().queued_publications(), 0);

    let replacement = NativeWorkerConfig::new(
        WorkerId::new(2),
        WorkerEpoch::new(2).unwrap(),
        8,
        Default::default(),
    )
    .unwrap();
    worker
        .enqueue_reentry(Reentry::Restart {
            config: replacement,
        })
        .unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.worker(), WorkerId::new(2));
    assert_eq!(worker.worker_epoch(), WorkerEpoch::new(2).unwrap());
    assert_eq!(worker.phase(), NativeWorkerPhase::Ready);
    assert!(worker.resources().has_zero_live_resources());

    let new_session = worker
        .open(
            &open_correlation(worker.worker(), 1),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    assert_eq!(new_session, SessionId::new(1));
}

#[test]
fn source_change_wins_before_queued_publication_and_leaves_no_surface() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.resources().queued_publications(), 1);

    worker
        .enqueue_reentry(Reentry::SourceChanged {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
        })
        .unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.resources().queued_publications(), 0);
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert!(!matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReady(_))
    ));
}

#[test]
fn request_ids_never_reuse_and_critical_close_bypasses_normal_reentry_pressure() {
    let limits = NativeWorkerLimitConfig {
        reentry_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let open = open_correlation(worker.worker(), 9);
    let session = worker
        .open(
            &open,
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    assert!(
        worker
            .open(
                &open,
                &OpenCommand {
                    source: source_descriptor(),
                },
            )
            .is_err()
    );

    let need = pdf_rs_protocol::NeedDataEvent {
        ticket: pdf_rs_protocol::DataTicket::new(1),
        source: source_descriptor().identity,
        ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
        priority: pdf_rs_protocol::DataPriority::Metadata,
        checkpoint: 1,
    };
    worker
        .enqueue_reentry(Reentry::NeedData {
            worker_epoch: worker.worker_epoch(),
            correlation: pdf_rs_protocol::Correlation {
                worker: worker.worker(),
                session: Some(session),
                request: Some(RequestId::new(9)),
                generation: None,
            },
            event: need,
        })
        .unwrap();
    let second = Reentry::NeedData {
        worker_epoch: worker.worker_epoch(),
        correlation: pdf_rs_protocol::Correlation {
            worker: worker.worker(),
            session: Some(session),
            request: Some(RequestId::new(9)),
            generation: None,
        },
        event: pdf_rs_protocol::NeedDataEvent {
            ticket: pdf_rs_protocol::DataTicket::new(2),
            source: source_descriptor().identity,
            ranges: vec![pdf_rs_protocol::ByteRange { start: 4, len: 4 }],
            priority: pdf_rs_protocol::DataPriority::Metadata,
            checkpoint: 2,
        },
    };
    assert!(worker.enqueue_reentry(second).is_err());
    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    worker.pump().unwrap();
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
}

#[test]
fn open_cancel_has_one_request_terminal_and_replay_ack() {
    let mut worker = worker();
    let request = RequestId::new(17);
    let open = open_correlation(worker.worker(), request.value());
    let session = worker
        .open(
            &open,
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let correlation = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: Some(session),
        request: Some(request),
        generation: None,
    };
    worker
        .cancel(&correlation, &CancelCommand { target: request })
        .unwrap();
    worker.pump().unwrap();
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::RequestCancelled { .. })
    ));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CancelAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::Applied
    ));
    worker.pump().unwrap();
    worker.pump().unwrap();
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));

    worker
        .cancel(&correlation, &CancelCommand { target: request })
        .unwrap();
    worker.pump().unwrap();
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SessionClosed { .. })
    ));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CancelAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::AlreadyTerminal
    ));
}

#[test]
fn shutdown_drains_to_zero_and_replay_is_acknowledged_after_stop() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let correlation = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: None,
        request: None,
        generation: None,
    };
    worker
        .shutdown(&correlation, &ShutdownCommand { deadline_ms: 100 })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.phase(), NativeWorkerPhase::Draining);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Stopped);
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::ShutdownAcknowledged { .. })
    ));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::WorkerStopped { .. })
    ));
    assert!(worker.resources().has_zero_live_resources());

    worker
        .shutdown(&correlation, &ShutdownCommand { deadline_ms: 100 })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::ShutdownAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::AlreadyApplied
    ));
}

#[test]
fn full_scheduler_critical_queue_is_dispatched_before_later_close_reentry() {
    let limits = NativeWorkerLimitConfig {
        scheduler: pdf_rs_scheduler::SchedulerLimits::new(2, 1, 1, 1, 1, 2, 3, 1, 1, 1).unwrap(),
        pending_resource_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let first = worker
        .open(
            &open_correlation(worker.worker(), 1),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let second = worker
        .open(
            &open_correlation(worker.worker(), 2),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    for session in [first, second] {
        worker
            .close_session(
                &session_correlation(worker.worker(), session),
                &CloseSessionCommand {},
            )
            .unwrap();
    }
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(
        worker.pump().unwrap(),
        ActorProgress::Lifecycle,
        "the first close must reserve the only scheduler lifecycle slot"
    );
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.session_phase(first), Some(SessionPhase::Closed));
    assert_eq!(worker.session_phase(second), Some(SessionPhase::Closed));
}

#[test]
fn opening_close_cancels_open_and_discards_late_parser_completion() {
    let mut worker = worker();
    let request = RequestId::new(41);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));

    let events = drain_events(&mut worker);
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        NativeWorkerEvent::RequestCancelled { correlation, event }
            if correlation.request == Some(request) && event.target == request
    ));
    assert!(matches!(
        &events[1],
        NativeWorkerEvent::CloseSessionAcknowledged { event, .. }
            if event.status == pdf_rs_protocol::OperationAckStatus::Applied
    ));
    assert!(matches!(
        &events[2],
        NativeWorkerEvent::SessionClosed { .. }
    ));
    assert!(worker.resources().has_zero_live_resources());
}

#[test]
fn source_change_wins_over_later_cancel_for_opening_request() {
    let mut worker = worker();
    let request = RequestId::new(42);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::SourceChanged {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
        })
        .unwrap();
    let cancel_correlation = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: Some(session),
        request: Some(request),
        generation: None,
    };
    worker
        .cancel(&cancel_correlation, &CancelCommand { target: request })
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let events = drain_events(&mut worker);
    assert!(events.iter().any(|event| matches!(
        event,
        NativeWorkerEvent::CancelAcknowledged { event, .. }
            if event.status == pdf_rs_protocol::OperationAckStatus::AlreadyTerminal
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        NativeWorkerEvent::RequestFailed { correlation, event }
            if correlation.request == Some(request)
                && event.error.code == pdf_rs_protocol::EngineErrorCode::SourceChanged
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::RequestCancelled { .. }))
    );
}

#[test]
fn shutdown_during_opening_emits_one_request_terminal_before_worker_stop() {
    let mut worker = worker();
    let request = RequestId::new(43);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let shutdown = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: None,
        request: None,
        generation: None,
    };
    worker
        .shutdown(&shutdown, &ShutdownCommand { deadline_ms: 100 })
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Stopped);

    let events = drain_events(&mut worker);
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    NativeWorkerEvent::DocumentReady { .. }
                        | NativeWorkerEvent::RequestCancelled { .. }
                        | NativeWorkerEvent::RequestFailed { .. }
                )
            })
            .count(),
        1
    );
    assert!(matches!(
        &events[0],
        NativeWorkerEvent::RequestCancelled { correlation, event }
            if correlation.request == Some(request) && event.target == request
    ));
    assert!(matches!(
        &events[1],
        NativeWorkerEvent::ShutdownAcknowledged { .. }
    ));
    assert!(matches!(
        &events[2],
        NativeWorkerEvent::WorkerStopped { .. }
    ));
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(worker.resources().has_zero_live_resources());
}

#[test]
fn stale_queued_raster_is_dropped_when_generation_is_replaced() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    assert_eq!(worker.resources().pending_rasters(), 1);

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &viewport(2),
        )
        .unwrap();
    assert_eq!(worker.resources().pending_rasters(), 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        }) if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
}

#[test]
fn source_change_discards_a_raster_completion_queued_before_lifecycle() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    worker
        .enqueue_reentry(Reentry::SourceChanged {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
        })
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert_eq!(worker.resources().queued_publications(), 0);
    assert!(drain_events(&mut worker).iter().any(|event| matches!(
        event,
        NativeWorkerEvent::GenerationCompleted { event, .. }
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
    )));
}

#[test]
fn close_preserves_active_generation_terminal_until_delayed_event_drain() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let events = drain_events(&mut worker);
    assert!(matches!(
        &events[0],
        NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        } if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::CloseSessionAcknowledged { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::SessionClosed { .. }))
    );
}

#[test]
fn shutdown_preserves_active_generation_terminal_until_delayed_event_drain() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    let shutdown = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: None,
        request: None,
        generation: None,
    };
    worker
        .shutdown(&shutdown, &ShutdownCommand { deadline_ms: 100 })
        .unwrap();

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Stopped);

    let events = drain_events(&mut worker);
    assert!(matches!(
        &events[0],
        NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        } if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::ShutdownAcknowledged { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::WorkerStopped { .. }))
    );
    assert!(worker.resources().has_zero_live_resources());
}

#[test]
fn unsupported_and_raster_resource_failures_keep_distinct_error_codes() {
    let mut unsupported_worker = worker();
    let unsupported_session = open_ready(&mut unsupported_worker, unsupported_scene());
    unsupported_worker.next_event();
    unsupported_worker
        .set_viewport(
            &generation_correlation(unsupported_worker.worker(), unsupported_session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(unsupported_worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(
        unsupported_worker.pump().unwrap(),
        ActorProgress::Capability
    );
    reenter_next_policy(&mut unsupported_worker);
    unsupported_worker.next_event();
    unsupported_worker.pump().unwrap();
    unsupported_worker.pump().unwrap();
    assert!(matches!(
        unsupported_worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.error.as_ref().is_some_and(|error| {
                error.code == pdf_rs_protocol::EngineErrorCode::UnsupportedFeature
            })
    ));

    let raster_config = FastRasterLimitConfig {
        max_pixels: 1,
        ..Default::default()
    };
    let limits = NativeWorkerLimitConfig {
        raster: FastRasterLimits::validate(raster_config).unwrap(),
        ..Default::default()
    };
    let mut raster_worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let raster_session = open_ready(&mut raster_worker, supported_scene());
    raster_worker.next_event();
    drive_to_queued_raster(&mut raster_worker, raster_session, 1);
    assert_eq!(raster_worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(raster_worker.pump().unwrap(), ActorProgress::Scheduled);
    assert!(matches!(
        raster_worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.error.as_ref().is_some_and(|error| {
                error.code == pdf_rs_protocol::EngineErrorCode::ResourceLimit
            })
    ));
}

#[test]
fn critical_event_capacity_boundary_is_live_and_rejects_undercharge() {
    let scheduler = pdf_rs_scheduler::SchedulerLimits::new(1, 1, 1, 1, 1, 1, 2, 1, 1, 1).unwrap();
    let valid_limits = NativeWorkerLimitConfig {
        critical_event_capacity: 3,
        scheduler,
        pending_resource_capacity: 1,
        ..Default::default()
    };
    let invalid_limits = NativeWorkerLimitConfig {
        critical_event_capacity: 2,
        ..valid_limits
    };
    assert!(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            invalid_limits,
        )
        .is_err()
    );

    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            valid_limits,
        )
        .unwrap(),
    )
    .unwrap();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
}

#[test]
fn cancel_backlog_exact_capacity_keeps_two_replacements_transactional() {
    let limits = NativeWorkerLimitConfig {
        scheduler: pdf_rs_scheduler::SchedulerLimits::new(4, 1, 2, 4, 2, 2, 6, 1, 1, 1).unwrap(),
        pending_resource_capacity: 2,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let first = open_ready_with_request(&mut worker, supported_scene(), 1);
    worker.next_event();
    let second = open_ready_with_request(&mut worker, supported_scene(), 2);
    worker.next_event();
    for session in [first, second] {
        worker
            .set_viewport(
                &generation_correlation(worker.worker(), session, 1),
                &viewport(1),
            )
            .unwrap();
    }
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    for session in [first, second] {
        worker
            .set_viewport(
                &generation_correlation(worker.worker(), session, 2),
                &viewport(2),
            )
            .unwrap();
    }
    assert_eq!(worker.resources().in_flight(), 2);
    assert_eq!(worker.resources().queued_normal(), 2);
    assert_eq!(worker.resources().queued_reentries(), 2);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    let events = drain_events(&mut worker);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                NativeWorkerEvent::GenerationCompleted { event, .. }
                    if event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
            ))
            .count(),
        2
    );
}

#[test]
fn surface_byte_import_is_exact_one_shot_and_release_rejects_replay() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let publication = publish_one_surface(&mut worker, session);
    let transfer = publication.transfer().clone();
    let replay = transfer.clone();
    let after_release = transfer.clone();

    let imported = worker.import_surface_bytes(&publication, transfer).unwrap();
    assert_eq!(imported.correlation(), publication.correlation());
    assert_eq!(imported.metadata(), &publication.event().metadata);
    assert_eq!(imported.plan(), publication.plan());
    assert_eq!(
        u64::try_from(imported.bytes().len()).unwrap(),
        imported.metadata().byte_length
    );
    assert!(imported.retained_byte_capacity() >= imported.bytes().len());
    assert_eq!(
        worker.resources().retained_raster_bytes(),
        u64::try_from(imported.retained_byte_capacity()).unwrap()
    );
    let debug = format!("{imported:?}");
    assert!(debug.contains("[BYTES:"));
    assert!(!debug.contains("bytes: [0,"));
    assert!(worker.import_surface_bytes(&publication, replay).is_err());

    let metadata = imported.metadata().clone();
    worker
        .release_surface(
            &session_correlation(worker.worker(), session),
            &ReleaseSurfaceCommand {
                surface: metadata.id,
                lease_token: metadata.lease_token,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(worker.resources().surface().has_zero_surface_resources());
    assert!(
        worker
            .import_surface_bytes(&publication, after_release)
            .is_err()
    );
    drop(imported);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn surface_byte_import_rejects_tampered_foreign_and_closed_transfers() {
    let mut owner = worker();
    let session = open_ready(&mut owner, supported_scene());
    owner.next_event();
    let publication = publish_one_surface(&mut owner, session);
    let mut tampered = publication.transfer().clone();
    tampered.metadata.generation += 1;
    assert!(owner.import_surface_bytes(&publication, tampered).is_err());
    owner
        .import_surface_bytes(&publication, publication.transfer().clone())
        .unwrap();

    let foreign_config = NativeWorkerConfig::new(
        WorkerId::new(2),
        WorkerEpoch::new(2).unwrap(),
        7,
        Default::default(),
    )
    .unwrap();
    let mut foreign = pdf_rs_engine::NativeWorkerRegistry::new(foreign_config).unwrap();
    assert!(
        foreign
            .import_surface_bytes(&publication, publication.transfer().clone())
            .is_err()
    );

    let mut closing_owner = worker();
    let closing_session = open_ready(&mut closing_owner, supported_scene());
    closing_owner.next_event();
    let closing_publication = publish_one_surface(&mut closing_owner, closing_session);
    closing_owner
        .close_session(
            &session_correlation(closing_owner.worker(), closing_session),
            &CloseSessionCommand {},
        )
        .unwrap();
    assert_eq!(closing_owner.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(closing_owner.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        closing_owner.next_event(),
        Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
            if event.reason == pdf_rs_protocol::SurfaceReclaimReason::SessionClosed
    ));
    assert_eq!(closing_owner.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(closing_owner.pump().unwrap(), ActorProgress::Scheduled);
    assert!(
        closing_owner
            .import_surface_bytes(&closing_publication, closing_publication.transfer().clone(),)
            .is_err()
    );
    assert!(
        closing_owner
            .resources()
            .surface()
            .has_zero_surface_resources()
    );
}

#[test]
fn surface_import_budget_rejection_does_not_consume_the_one_shot_transfer() {
    let raster = FastRasterLimits::default();
    let retained_raster_byte_capacity = raster
        .max_retained_bytes()
        .checked_add(raster.max_intermediate_bytes())
        .and_then(|bytes| {
            bytes.checked_add(
                NativeWorkerLimitConfig::default()
                    .policy_job
                    .max_retained_bytes(),
            )
        })
        .unwrap();
    let limits = NativeWorkerLimitConfig {
        raster,
        retained_raster_byte_capacity,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let surface_session = open_ready_with_request(&mut worker, supported_scene(), 1);
    worker.next_event();
    let publication = publish_one_surface(&mut worker, surface_session);
    let transfer = publication.transfer().clone();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { .. })
    ));

    let pressure_session = open_ready_with_request(&mut worker, supported_scene(), 2);
    worker.next_event();
    let held = drive_to_raster_task(&mut worker, pressure_session, 1);
    assert_eq!(
        worker.resources().retained_raster_bytes(),
        retained_raster_byte_capacity
    );
    assert!(
        worker
            .import_surface_bytes(&publication, transfer.clone())
            .is_err()
    );

    drop(held);
    let imported = worker
        .import_surface_bytes(&publication, transfer)
        .expect("budget rejection must not consume the Surface transfer");
    assert_eq!(
        worker.resources().retained_raster_bytes(),
        u64::try_from(imported.retained_byte_capacity()).unwrap()
    );
}

#[test]
fn undelivered_surface_reclaim_covers_preimport_and_postimport_failures() {
    let mut preimport = worker();
    let session = open_ready(&mut preimport, supported_scene());
    preimport.next_event();
    let publication = publish_one_surface(&mut preimport, session);
    let expected_length = usize::try_from(publication.event().metadata.byte_length).unwrap();
    let mut allocation_probe = Vec::<u8>::new();
    allocation_probe.try_reserve_exact(expected_length).unwrap();
    let actual_capacity = u64::try_from(allocation_probe.capacity()).unwrap();
    assert!(actual_capacity > 0);
    assert!(
        preimport
            .import_surface_bytes_bounded(
                &publication,
                publication.transfer().clone(),
                actual_capacity - 1,
            )
            .is_err()
    );
    assert_eq!(
        preimport.resources().surface().published_surfaces(),
        1,
        "a pre-import budget failure must not consume the one-shot handle"
    );
    preimport.reclaim_undelivered_surface(&publication).unwrap();
    assert!(preimport.resources().surface().has_zero_surface_resources());
    assert!(
        preimport.reclaim_undelivered_surface(&publication).is_err(),
        "the registry delivery ledger is consumed exactly once"
    );

    let mut postimport = worker();
    let session = open_ready(&mut postimport, supported_scene());
    postimport.next_event();
    let publication = publish_one_surface(&mut postimport, session);
    let imported = postimport
        .import_surface_bytes_bounded(
            &publication,
            publication.transfer().clone(),
            actual_capacity,
        )
        .unwrap();
    assert_eq!(postimport.resources().surface().imported_surfaces(), 1);
    postimport
        .reclaim_undelivered_surface(&publication)
        .unwrap();
    assert!(
        postimport
            .resources()
            .surface()
            .has_zero_surface_resources()
    );
    drop(imported);
    assert_eq!(postimport.resources().retained_raster_bytes(), 0);
}

#[test]
fn surface_resource_limit_never_fabricates_a_byte_export() {
    let surface_config = pdf_rs_surface::SurfaceLimitConfig {
        max_total_bytes: 1_023,
        ..Default::default()
    };
    let limits = NativeWorkerLimitConfig {
        surface: pdf_rs_surface::SurfaceLimits::new(surface_config).unwrap(),
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    drive_to_queued_raster(&mut worker, session, 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);

    let events = drain_events(&mut worker);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::SurfaceReady(_)))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        NativeWorkerEvent::GenerationCompleted { event, .. }
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Failed
    )));
    assert!(worker.resources().surface().has_zero_surface_resources());
}

#[test]
fn crash_restart_allows_same_renderer_epoch_and_invalidates_old_ownership() {
    let mut owner = worker();
    let surface_session = open_ready_with_request(&mut owner, supported_scene(), 1);
    owner.next_event();
    let old_publication = publish_one_surface(&mut owner, surface_session);
    assert_eq!(owner.resources().surface().published_surfaces(), 1);

    let opening_request = RequestId::new(2);
    let opening_session = owner
        .open(
            &open_correlation(owner.worker(), opening_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    owner
        .enqueue_reentry(Reentry::NeedData {
            worker_epoch: owner.worker_epoch(),
            correlation: pdf_rs_protocol::Correlation {
                worker: owner.worker(),
                session: Some(opening_session),
                request: Some(opening_request),
                generation: None,
            },
            event: pdf_rs_protocol::NeedDataEvent {
                ticket: pdf_rs_protocol::DataTicket::new(1),
                source: source_descriptor().identity,
                ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
                priority: pdf_rs_protocol::DataPriority::Metadata,
                checkpoint: 1,
            },
        })
        .unwrap();

    let replacement = NativeWorkerConfig::new(
        WorkerId::new(2),
        WorkerEpoch::new(2).unwrap(),
        7,
        Default::default(),
    )
    .unwrap();
    owner
        .enqueue_reentry(Reentry::Restart {
            config: replacement,
        })
        .unwrap();
    assert_eq!(owner.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(owner.worker(), WorkerId::new(2));
    assert_eq!(owner.worker_epoch(), WorkerEpoch::new(2).unwrap());
    assert_eq!(owner.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        owner.next_event(),
        Some(NativeWorkerEvent::SurfaceReclaimed { correlation, event })
            if correlation.worker == WorkerId::new(1)
                && correlation.session == Some(surface_session)
                && event.surface == old_publication.event().metadata.id
                && event.lease_token == old_publication.event().metadata.lease_token
                && event.reason == pdf_rs_protocol::SurfaceReclaimReason::RendererRestarted
    ));
    assert!(owner.resources().has_zero_live_resources());
    assert!(
        owner
            .import_surface_bytes(&old_publication, old_publication.transfer().clone())
            .is_err()
    );
    assert!(
        owner
            .close_session(
                &session_correlation(WorkerId::new(1), surface_session),
                &CloseSessionCommand {},
            )
            .is_err()
    );
}

#[test]
fn open_scene_admission_is_hard_bounded_and_retains_rejected_ownership() {
    let invalid_limits = NativeWorkerLimitConfig {
        max_scenes_per_open: 0,
        ..Default::default()
    };
    assert!(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            invalid_limits,
        )
        .is_err()
    );

    let limits = NativeWorkerLimitConfig {
        max_scenes_per_open: 1,
        ..Default::default()
    };
    let mut owner = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let request = RequestId::new(1);
    let session = owner
        .open(
            &open_correlation(owner.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let rejected = owner
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: owner.worker(),
            worker_epoch: owner.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![
                Arc::new(scene_at(
                    PAGE_INDEX,
                    pdf_rs_scene::CapabilityStatus::Supported,
                )),
                Arc::new(scene_at(
                    PAGE_INDEX + 1,
                    pdf_rs_scene::CapabilityStatus::Supported,
                )),
            ],
        }))
        .unwrap_err();
    assert_eq!(
        rejected.error().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert!(matches!(
        rejected.into_reentry(),
        Reentry::Open(pdf_rs_engine::OpenCompletion::Ready { scenes, .. })
            if scenes.len() == 2
    ));
    assert_eq!(owner.resources().queued_reentries(), 0);

    owner
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: owner.worker(),
            worker_epoch: owner.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    assert_eq!(owner.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        owner.next_event(),
        Some(NativeWorkerEvent::DocumentReady { event, .. }) if event.page_count == 1
    ));
}

#[test]
fn scene_admission_charges_fixed_ownership_and_outer_vector_capacity() {
    let scene = Arc::new(supported_scene());
    let payload_bytes = scene.stats().retained_bytes();
    assert!(payload_bytes > 0);
    let limits = NativeWorkerLimitConfig {
        retained_scene_byte_capacity: payload_bytes,
        max_scene_bytes_per_open: payload_bytes,
        max_scenes_per_open: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let mut scenes = Vec::with_capacity(8);
    scenes.push(scene);
    let rejected = worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes,
        }))
        .unwrap_err();
    assert_eq!(
        rejected.error().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert!(matches!(
        rejected.into_reentry(),
        Reentry::Open(pdf_rs_engine::OpenCompletion::Ready { scenes, .. })
            if scenes.len() == 1 && scenes.capacity() >= 8
    ));
}

#[test]
fn open_rejects_nonzero_start_and_sparse_scene_page_domains() {
    for scenes in [
        vec![Arc::new(scene_at(
            1,
            pdf_rs_scene::CapabilityStatus::Supported,
        ))],
        vec![
            Arc::new(scene_at(0, pdf_rs_scene::CapabilityStatus::Supported)),
            Arc::new(scene_at(2, pdf_rs_scene::CapabilityStatus::Supported)),
        ],
    ] {
        let mut worker = worker();
        let request = RequestId::new(1);
        let session = worker
            .open(
                &open_correlation(worker.worker(), request.value()),
                &OpenCommand {
                    source: source_descriptor(),
                },
            )
            .unwrap();
        worker
            .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
                worker: worker.worker(),
                worker_epoch: worker.worker_epoch(),
                session,
                request,
                document_revision: DOCUMENT_REVISION,
                scenes,
            }))
            .unwrap();
        assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::RequestFailed { event, .. })
                if event.error.code == pdf_rs_protocol::EngineErrorCode::Internal
        ));
        assert_ne!(worker.session_phase(session), Some(SessionPhase::Ready));
    }
}

#[test]
fn open_rejects_scenes_that_cannot_project_to_wire_page_geometry() {
    for scene in [
        scene_with_scaled_geometry(
            0,
            pdf_rs_scene::CapabilityStatus::Supported,
            [0, 0, 499_999, 1_000_000_000],
            pdf_rs_scene::PageRotation::Degrees0,
        ),
        scene_with_scaled_geometry(
            0,
            pdf_rs_scene::CapabilityStatus::Supported,
            [
                i64::MAX - 2_000_000_000,
                0,
                i64::MAX - 1_000_000_000,
                1_000_000_000,
            ],
            pdf_rs_scene::PageRotation::Degrees0,
        ),
    ] {
        let mut worker = worker();
        let request = RequestId::new(1);
        let session = worker
            .open(
                &open_correlation(worker.worker(), request.value()),
                &OpenCommand {
                    source: source_descriptor(),
                },
            )
            .unwrap();
        worker
            .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
                worker: worker.worker(),
                worker_epoch: worker.worker_epoch(),
                session,
                request,
                document_revision: DOCUMENT_REVISION,
                scenes: vec![Arc::new(scene)],
            }))
            .unwrap();
        assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::RequestFailed { .. })
        ));
        assert!(!matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::DocumentReady { .. })
        ));
        assert_ne!(worker.session_phase(session), Some(SessionPhase::Ready));
    }
}

#[test]
fn external_raster_task_keeps_actor_lifecycle_live_and_observes_close_cancellation() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let task = drive_to_raster_task(&mut worker, session, 1);
    assert_eq!(
        worker.resources().retained_raster_bytes(),
        NativeWorkerLimitConfig::default()
            .raster
            .max_retained_bytes()
            + NativeWorkerLimitConfig::default()
                .raster
                .max_intermediate_bytes()
            + NativeWorkerLimitConfig::default()
                .policy_job
                .max_retained_bytes()
    );

    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);

    let cancelled = task.run(&NeverCancelled);
    assert!(format!("{cancelled:?}").contains("Cancelled"));
    worker.enqueue_reentry(cancelled).unwrap();
    for _ in 0..8 {
        let _ = worker.pump().unwrap();
    }
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
    assert!(worker.resources().surface().has_zero_surface_resources());
}

#[test]
fn external_policy_task_keeps_actor_lifecycle_live_and_observes_close_cancellation() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    let task = worker
        .next_policy_task()
        .expect("capability work must execute outside the actor");
    assert_eq!(worker.resources().pending_policy_tasks(), 1);
    assert!(worker.resources().retained_policy_job_bytes() > 0);

    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);

    let cancelled = task.run(&pdf_rs_policy::NeverCancelled);
    assert!(format!("{cancelled:?}").contains("Cancelled"));
    worker.enqueue_reentry(cancelled).unwrap();
    for _ in 0..8 {
        let _ = worker.pump().unwrap();
    }
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert_eq!(worker.resources().pending_policy_tasks(), 0);
    assert_eq!(worker.resources().retained_policy_job_bytes(), 0);
    assert!(worker.resources().surface().has_zero_surface_resources());
}

#[test]
fn pending_policy_poll_retains_and_drop_releases_the_single_permit() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    let task = worker.next_policy_task().unwrap();
    let budget = PolicyPollBudget::new(NonZeroU32::new(1).unwrap()).unwrap();
    let pending = match task.poll(budget, &NeverCancelledPolicy) {
        NativeTaskPoll::Pending(task) => task,
        NativeTaskPoll::Ready(_) => panic!("one work unit must not finish capability policy"),
    };
    assert_eq!(worker.resources().pending_policy_tasks(), 1);
    assert_eq!(
        worker.resources().retained_policy_job_bytes(),
        NativeWorkerLimitConfig::default()
            .policy_job
            .max_retained_bytes()
    );
    drop(pending);
    assert_eq!(worker.resources().pending_policy_tasks(), 0);
    assert_eq!(worker.resources().retained_policy_job_bytes(), 0);
}

#[test]
fn policy_byte_reservation_crosses_completion_and_delivery_barriers() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);

    complete_next_policy(&mut worker);
    let reserved = NativeWorkerLimitConfig::default()
        .policy_job
        .max_retained_bytes();
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);
    complete_next_policy(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { .. })
    ));
    assert_eq!(worker.resources().retained_policy_job_bytes(), reserved);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    assert_eq!(worker.resources().retained_policy_job_bytes(), 0);
    drop(worker.next_raster_task().unwrap());
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn pending_raster_poll_retains_and_drop_releases_the_full_reservation() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let task = drive_to_raster_task(&mut worker, session, 1);
    assert!(worker.resources().retained_raster_bytes() > 0);
    let budget = FastRasterPollBudget::new(NonZeroU32::new(1).unwrap()).unwrap();
    let pending = match task.poll(budget, &NeverCancelled) {
        NativeTaskPoll::Pending(task) => task,
        NativeTaskPoll::Ready(_) => panic!("one work unit must not finish Fast raster"),
    };
    assert!(worker.resources().retained_raster_bytes() > 0);
    drop(pending);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn full_progress_queue_retains_opaque_plan_completion_until_delivery_frees_space() {
    let limits = NativeWorkerLimitConfig {
        progress_event_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let first = open_ready_with_request(&mut worker, supported_scene(), 1);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { .. })
    ));

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), first, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    complete_next_policy(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.resources().pending_policy_tasks(), 1);

    let second = open_ready_with_request(&mut worker, supported_scene(), 2);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { .. })
    ));
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), second, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    complete_next_policy(&mut worker);
    assert_eq!(worker.resources().pending_policy_tasks(), 2);

    assert_eq!(worker.pump().unwrap(), ActorProgress::Idle);
    assert_eq!(worker.resources().pending_policy_tasks(), 2);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { correlation, .. })
            if correlation.session == Some(first)
    ));

    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.resources().pending_policy_tasks(), 2);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { correlation, .. })
            if correlation.session == Some(second)
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    drop(worker.next_raster_task().unwrap());
    assert_eq!(worker.resources().pending_policy_tasks(), 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    drop(worker.next_raster_task().unwrap());
    assert_eq!(worker.resources().pending_policy_tasks(), 0);
}

#[test]
fn second_generation_hits_complete_cache_and_dispatches_no_raster_task() {
    let mut worker = worker();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let first = publish_one_surface(&mut worker, session);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted { event, .. })
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Completed
    ));
    worker
        .release_surface(
            &session_correlation(worker.worker(), session),
            &ReleaseSurfaceCommand {
                surface: first.event().metadata.id,
                lease_token: first.event().metadata.lease_token,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
            if event.reason == pdf_rs_protocol::SurfaceReclaimReason::ReleasedByHost
    ));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReleaseAcknowledged { .. })
    ));

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &viewport(2),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CapabilityReported { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationPlanned { .. })
    ));
    assert_eq!(worker.pump().unwrap(), ActorProgress::CacheHit);
    assert!(worker.next_raster_task().is_none());
    assert!(worker.resources().retained_raster_bytes() > 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let second = (0..8)
        .find_map(|_| {
            assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
            worker.next_event()
        })
        .map(|event| match event {
            NativeWorkerEvent::SurfaceReady(publication) => publication,
            other => panic!("expected cache-hit SurfaceReady, got {other:?}"),
        })
        .expect("cache hit must publish a Surface");
    assert_eq!(second.correlation().generation, Some(2));
    assert_eq!(second.event().metadata.generation, 2);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn cache_hit_copy_is_sliced_and_close_preempts_the_next_chunk() {
    let mut worker = worker();
    let session = open_ready(
        &mut worker,
        scene_at_size(
            PAGE_INDEX,
            pdf_rs_scene::CapabilityStatus::Supported,
            300_000,
            16_000,
        ),
    );
    worker.next_event();
    let wide_geometry = wire_geometry(&scene_at_size(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        300_000,
        16_000,
    ));
    let wide_viewport = |generation| {
        let mut command = viewport(generation);
        let page = &mut command.viewport.visible_pages[0];
        page.geometry = wide_geometry.clone();
        page.clip_width_milli_points = 300_000;
        command
    };

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &wide_viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    let mut first_completed = false;
    for _ in 0..128 {
        let _ = worker.pump().unwrap();
        while let Some(event) = worker.next_event() {
            if matches!(
                event,
                NativeWorkerEvent::GenerationCompleted { correlation, .. }
                    if correlation.generation == Some(1)
            ) {
                first_completed = true;
            }
        }
        if first_completed {
            break;
        }
    }
    assert!(first_completed, "first generation must populate the cache");

    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &wide_viewport(2),
        )
        .unwrap();
    let mut reclaimed = 0;
    loop {
        let progress = worker.pump().unwrap();
        if progress == ActorProgress::Scheduled {
            break;
        }
        assert_eq!(progress, ActorProgress::Lifecycle);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
                if event.reason == pdf_rs_protocol::SurfaceReclaimReason::GenerationReplaced
        ));
        reclaimed += 1;
    }
    assert!(reclaimed > 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::CacheHit);
    let raster = NativeWorkerLimitConfig::default().raster;
    assert_eq!(
        worker.resources().retained_raster_bytes(),
        raster.max_retained_bytes()
            + raster.max_intermediate_bytes()
            + NativeWorkerLimitConfig::default()
                .policy_job
                .max_retained_bytes(),
        "one cache-copy chunk must leave the worst-case reservation live"
    );
    assert!(worker.next_raster_task().is_none());

    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
    for _ in 0..16 {
        let _ = worker.pump().unwrap();
    }
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    assert!(!drain_events(&mut worker).iter().any(|event| matches!(
        event,
        NativeWorkerEvent::SurfaceReady(publication)
            if publication.correlation().generation == Some(2)
    )));
}

#[test]
fn raster_budget_charges_tasks_held_outside_the_actor() {
    let raster = FastRasterLimits::validate(FastRasterLimitConfig {
        max_retained_bytes: 4_096,
        max_intermediate_bytes: 1,
        ..Default::default()
    })
    .unwrap();
    let task_bytes = 4_097
        + NativeWorkerLimitConfig::default()
            .policy_job
            .max_retained_bytes();
    let invalid = NativeWorkerLimitConfig {
        raster,
        retained_raster_byte_capacity: task_bytes - 1,
        ..Default::default()
    };
    assert!(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, invalid,)
            .is_err()
    );
    let limits = NativeWorkerLimitConfig {
        raster,
        retained_raster_byte_capacity: task_bytes,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let first = open_ready_with_request(&mut worker, supported_scene(), 1);
    worker.next_event();
    let held = drive_to_raster_task(&mut worker, first, 1);
    assert_eq!(worker.resources().retained_raster_bytes(), task_bytes);

    let second = open_ready_with_request(&mut worker, supported_scene(), 2);
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), second, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Idle);
    assert!(worker.next_raster_task().is_none());

    drop(held);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    let second_task = worker.next_raster_task().unwrap();
    assert_eq!(worker.resources().retained_raster_bytes(), task_bytes);
    drop(second_task);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn restart_rejects_an_external_policy_task_before_mutation_then_succeeds_after_drop() {
    let limits = NativeWorkerLimitConfig::default();
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    let task = worker.next_policy_task().unwrap();
    let scene_bytes = worker.resources().retained_scene_bytes();
    assert!(scene_bytes > 0);
    assert!(worker.resources().retained_policy_job_bytes() > 0);

    let replacement = || Reentry::Restart {
        config: NativeWorkerConfig::new(WorkerId::new(2), WorkerEpoch::new(2).unwrap(), 7, limits)
            .unwrap(),
    };
    worker.enqueue_reentry(replacement()).unwrap();
    assert_eq!(
        worker.pump().unwrap_err().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert_eq!(worker.worker(), WorkerId::new(1));
    assert_eq!(worker.worker_epoch(), WorkerEpoch::new(1).unwrap());
    assert_eq!(worker.resources().retained_scene_bytes(), scene_bytes);
    assert!(worker.resources().retained_policy_job_bytes() > 0);

    drop(task);
    assert_eq!(worker.resources().retained_policy_job_bytes(), 0);
    worker.enqueue_reentry(replacement()).unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.worker(), WorkerId::new(2));
    assert_eq!(worker.resources().retained_scene_bytes(), 0);
}

#[test]
fn restart_rejects_an_external_raster_task_before_mutation_then_succeeds_after_drop() {
    let raster = FastRasterLimits::validate(FastRasterLimitConfig {
        max_retained_bytes: 4_096,
        max_intermediate_bytes: 1,
        ..Default::default()
    })
    .unwrap();
    let task_bytes = 4_097
        + NativeWorkerLimitConfig::default()
            .policy_job
            .max_retained_bytes();
    let limits = NativeWorkerLimitConfig {
        raster,
        retained_raster_byte_capacity: task_bytes,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let old_session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let old_task = drive_to_raster_task(&mut worker, old_session, 1);
    assert_eq!(worker.resources().retained_raster_bytes(), task_bytes);

    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                limits,
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(
        worker.pump().unwrap_err().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert_eq!(worker.worker(), WorkerId::new(1));
    assert_eq!(worker.worker_epoch(), WorkerEpoch::new(1).unwrap());
    assert_eq!(worker.resources().retained_raster_bytes(), task_bytes);

    drop(old_task);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                limits,
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);

    let current_session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let current_task = drive_to_raster_task(&mut worker, current_session, 1);
    assert_eq!(worker.resources().retained_raster_bytes(), task_bytes);

    drop(current_task);
    assert_eq!(worker.resources().retained_raster_bytes(), 0);
}

#[test]
fn three_page_viewport_releases_worst_case_permits_and_completes() {
    let mut worker = worker();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: (0..3)
                .map(|offset| {
                    Arc::new(scene_at(
                        PAGE_INDEX + offset,
                        pdf_rs_scene::CapabilityStatus::Supported,
                    ))
                })
                .collect(),
        }))
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { .. })
    ));

    let mut command = viewport(1);
    let base_page = command.viewport.visible_pages[0].clone();
    command.viewport.visible_pages = (0..3)
        .map(|offset| {
            let mut page = base_page.clone();
            page.page_index = PAGE_INDEX + offset;
            page.geometry = wire_geometry(&scene_at(
                PAGE_INDEX + offset,
                pdf_rs_scene::CapabilityStatus::Supported,
            ));
            page
        })
        .collect();
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &command,
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    for _ in 0..3 {
        assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
        reenter_next_policy(&mut worker);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::CapabilityReported { .. })
        ));
        assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
        reenter_next_policy(&mut worker);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::GenerationPlanned { .. })
        ));
        assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
        let completion = worker
            .next_raster_task()
            .expect("each page must dispatch despite prior retained results")
            .run(&NeverCancelled);
        worker.enqueue_reentry(completion).unwrap();
        assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
        assert!(
            worker.resources().retained_raster_bytes()
                < NativeWorkerLimitConfig::default().retained_raster_byte_capacity
        );
    }
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let mut surface_count = 0_u32;
    let mut completed = None;
    for _ in 0..32 {
        let _ = worker.pump().unwrap();
        while let Some(event) = worker.next_event() {
            match event {
                NativeWorkerEvent::SurfaceReady(_) => surface_count += 1,
                NativeWorkerEvent::GenerationCompleted { event, .. } => {
                    completed = Some(event);
                }
                _ => {}
            }
        }
        if completed.is_some() {
            break;
        }
    }
    let completed = completed.expect("three-page generation must reach one terminal");
    assert_eq!(
        completed.status,
        pdf_rs_protocol::GenerationCompletionStatus::Completed
    );
    assert_eq!(completed.produced_regions, surface_count);
    assert_eq!(surface_count, 3);
}

#[test]
fn multi_tile_surface_failure_emits_no_partial_surface_ready() {
    let surface = pdf_rs_surface::SurfaceLimits::new(pdf_rs_surface::SurfaceLimitConfig {
        max_total_bytes: 17_000,
        ..Default::default()
    })
    .unwrap();
    let limits = NativeWorkerLimitConfig {
        surface,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let session = open_ready(
        &mut worker,
        scene_at_size(
            PAGE_INDEX,
            pdf_rs_scene::CapabilityStatus::Supported,
            300_000,
            16_000,
        ),
    );
    worker.next_event();
    let mut command = viewport(1);
    let page = &mut command.viewport.visible_pages[0];
    page.geometry = wire_geometry(&scene_at_size(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        300_000,
        16_000,
    ));
    page.clip_width_milli_points = 300_000;
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &command,
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
    assert!(worker.next_event().is_none());
    assert_eq!(worker.resources().surface().published_surfaces(), 1);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);

    let events = drain_events(&mut worker);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NativeWorkerEvent::SurfaceReady(_)))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        NativeWorkerEvent::GenerationCompleted { event, .. }
            if event.status == pdf_rs_protocol::GenerationCompletionStatus::Failed
                && event.produced_regions == 0
                && event.error.as_ref().is_some_and(|error| {
                    error.code == pdf_rs_protocol::EngineErrorCode::ResourceLimit
                })
    )));
    assert!(worker.resources().surface().has_zero_surface_resources());
}

#[test]
fn replacement_reports_only_surface_regions_already_delivered() {
    let mut worker = worker();
    let session = open_ready(
        &mut worker,
        scene_at_size(
            PAGE_INDEX,
            pdf_rs_scene::CapabilityStatus::Supported,
            300_000,
            16_000,
        ),
    );
    worker.next_event();
    let wide_geometry = wire_geometry(&scene_at_size(
        PAGE_INDEX,
        pdf_rs_scene::CapabilityStatus::Supported,
        300_000,
        16_000,
    ));
    let wide_viewport = |generation| {
        let mut command = viewport(generation);
        let page = &mut command.viewport.visible_pages[0];
        page.geometry = wide_geometry.clone();
        page.clip_width_milli_points = 300_000;
        command
    };
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 1),
            &wide_viewport(1),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Capability);
    reenter_next_policy(&mut worker);
    worker.next_event();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Raster);
    complete_next_raster(&mut worker);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);

    let first = (0..16)
        .find_map(|_| {
            assert_eq!(worker.pump().unwrap(), ActorProgress::Published);
            worker.next_event()
        })
        .expect("all tiles must stage before the first SurfaceReady");
    assert!(matches!(first, NativeWorkerEvent::SurfaceReady(_)));
    worker
        .set_viewport(
            &generation_correlation(worker.worker(), session, 2),
            &wide_viewport(2),
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReclaimed { event, .. })
            if event.reason == pdf_rs_protocol::SurfaceReclaimReason::GenerationReplaced
    ));

    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::GenerationCompleted {
            correlation,
            event,
        }) if correlation.generation == Some(1)
            && event.status == pdf_rs_protocol::GenerationCompletionStatus::Superseded
            && event.produced_regions == 1
    ));
    assert!(!matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::SurfaceReady(publication))
            if publication.correlation().generation == Some(1)
    ));
}

#[test]
fn fail_data_source_change_uses_lifecycle_queue_when_completion_queue_is_full() {
    let limits = NativeWorkerLimitConfig {
        reentry_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let opening_request = RequestId::new(1);
    let opening = worker
        .open(
            &open_correlation(worker.worker(), opening_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let ticket = pdf_rs_protocol::DataTicket::new(7);
    worker
        .enqueue_reentry(Reentry::NeedData {
            worker_epoch: worker.worker_epoch(),
            correlation: pdf_rs_protocol::Correlation {
                worker: worker.worker(),
                session: Some(opening),
                request: Some(opening_request),
                generation: None,
            },
            event: pdf_rs_protocol::NeedDataEvent {
                ticket,
                source: source_descriptor().identity,
                ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
                priority: pdf_rs_protocol::DataPriority::Metadata,
                checkpoint: 1,
            },
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    worker.next_event();

    let ready = open_ready_with_request(&mut worker, supported_scene(), 2);
    worker.next_event();
    drive_to_queued_raster(&mut worker, ready, 1);
    assert_eq!(worker.resources().queued_reentries(), 1);

    let expected = source_descriptor().identity;
    let mut observed = expected.clone();
    observed.revision += 1;
    worker
        .fail_data(
            &session_correlation(worker.worker(), opening),
            &FailDataCommand {
                ticket,
                expected,
                observed: Some(observed),
                code: SourceFailureCode::SourceChanged,
                retryable: false,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.session_phase(opening), Some(SessionPhase::Closing));
    assert_eq!(worker.resources().queued_reentries(), 2);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Lifecycle);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    for _ in 0..4 {
        let _ = worker.pump().unwrap();
        if worker.session_phase(opening) == Some(SessionPhase::Closed) {
            break;
        }
    }
    assert_eq!(worker.session_phase(opening), Some(SessionPhase::Closed));
}

#[test]
fn replayable_unknown_close_and_cross_session_cancel_are_stable_acks() {
    let mut worker = worker();
    let unknown = SessionId::new(999);
    worker
        .close_session(
            &session_correlation(worker.worker(), unknown),
            &CloseSessionCommand {},
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CloseSessionAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::UnknownTarget
    ));

    let first_request = RequestId::new(1);
    let first = worker
        .open(
            &open_correlation(worker.worker(), first_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let second_request = RequestId::new(2);
    let second = worker
        .open(
            &open_correlation(worker.worker(), second_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .cancel(
            &pdf_rs_protocol::Correlation {
                worker: worker.worker(),
                session: Some(first),
                request: Some(second_request),
                generation: None,
            },
            &CancelCommand {
                target: second_request,
            },
        )
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::CancelAcknowledged { event, .. })
            if event.status == pdf_rs_protocol::OperationAckStatus::UnknownTarget
    ));
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session: second,
            request: second_request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.session_phase(second), Some(SessionPhase::Ready));
}

#[test]
fn malformed_need_data_and_open_completion_fail_once_and_close() {
    let mut worker = worker();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::NeedData {
            worker_epoch: worker.worker_epoch(),
            correlation: pdf_rs_protocol::Correlation {
                worker: worker.worker(),
                session: Some(session),
                request: Some(RequestId::new(99)),
                generation: None,
            },
            event: pdf_rs_protocol::NeedDataEvent {
                ticket: pdf_rs_protocol::DataTicket::new(1),
                source: source_descriptor().identity,
                ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
                priority: pdf_rs_protocol::DataPriority::Metadata,
                checkpoint: 1,
            },
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::RequestFailed { correlation, event })
            if correlation.request == Some(request)
                && event.error.code == pdf_rs_protocol::EngineErrorCode::Internal
    ));
    for _ in 0..4 {
        let _ = worker.pump().unwrap();
    }
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));

    let mut invalid_open = support::worker();
    let request = RequestId::new(2);
    let session = invalid_open
        .open(
            &open_correlation(invalid_open.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    invalid_open
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: invalid_open.worker(),
            worker_epoch: invalid_open.worker_epoch(),
            session,
            request,
            document_revision: 0,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    assert_eq!(invalid_open.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(
        drain_events(&mut invalid_open)
            .iter()
            .filter(|event| matches!(event, NativeWorkerEvent::RequestFailed { .. }))
            .count(),
        1
    );
}

#[test]
fn stale_open_completion_is_epoch_bound_across_restart() {
    let mut worker = worker();
    let old_worker = worker.worker();
    let old_epoch = worker.worker_epoch();
    let request = RequestId::new(1);
    let old_session = worker
        .open(
            &open_correlation(old_worker, request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let delayed = Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
        worker: old_worker,
        worker_epoch: old_epoch,
        session: old_session,
        request,
        document_revision: DOCUMENT_REVISION,
        scenes: vec![Arc::new(supported_scene())],
    });
    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                Default::default(),
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    let current_session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    assert_eq!(current_session, old_session);
    let rejected = worker.enqueue_reentry(delayed).unwrap_err();
    assert_eq!(
        rejected.error().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::InvalidIdentity
    );
    drop(rejected.into_reentry());
    assert_eq!(
        worker.session_phase(current_session),
        Some(SessionPhase::Opening)
    );
    assert!(worker.next_event().is_none());
}

#[test]
fn lifecycle_reentry_rejection_retains_message_and_redacts_lease() {
    let limits = NativeWorkerLimitConfig {
        lifecycle_reentry_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    worker
        .enqueue_reentry(Reentry::Close {
            worker_epoch: worker.worker_epoch(),
            correlation: session_correlation(worker.worker(), SessionId::new(1)),
        })
        .unwrap();
    let secret = 9_876_543_210_u64;
    let rejected = worker
        .enqueue_reentry(Reentry::Release {
            worker_epoch: worker.worker_epoch(),
            correlation: session_correlation(worker.worker(), SessionId::new(1)),
            surface: pdf_rs_protocol::SurfaceId::new(1),
            lease_token: secret,
        })
        .unwrap_err();
    let debug = format!("{rejected:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(&secret.to_string()));
    assert!(matches!(
        rejected.into_reentry(),
        Reentry::Release { lease_token, .. } if lease_token == secret
    ));
}

#[test]
fn absurd_native_queue_capacity_is_rejected_without_allocation() {
    let limits = NativeWorkerLimitConfig {
        reentry_capacity: usize::MAX,
        ..Default::default()
    };
    assert!(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits,)
            .is_err()
    );
}

#[test]
fn absurd_scheduler_capacity_is_rejected_before_scheduler_allocation() {
    let scheduler =
        pdf_rs_scheduler::SchedulerLimits::new(1_000_001, 1, 1, 1, 1, 1, 1_000_002, 1, 1, 1)
            .unwrap();
    let limits = NativeWorkerLimitConfig {
        scheduler,
        ..Default::default()
    };
    assert!(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).is_err()
    );
}

#[test]
fn duplicate_need_data_fails_the_open_instead_of_leaving_it_hung() {
    let mut worker = worker();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let correlation = pdf_rs_protocol::Correlation {
        worker: worker.worker(),
        session: Some(session),
        request: Some(request),
        generation: None,
    };
    let need = pdf_rs_protocol::NeedDataEvent {
        ticket: pdf_rs_protocol::DataTicket::new(1),
        source: source_descriptor().identity,
        ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
        priority: pdf_rs_protocol::DataPriority::Metadata,
        checkpoint: 1,
    };
    for _ in 0..2 {
        worker
            .enqueue_reentry(Reentry::NeedData {
                worker_epoch: worker.worker_epoch(),
                correlation: correlation.clone(),
                event: need.clone(),
            })
            .unwrap();
        assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    }
    let events = drain_events(&mut worker);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, NativeWorkerEvent::RequestFailed { .. }))
            .count(),
        1
    );
    for _ in 0..4 {
        let _ = worker.pump().unwrap();
    }
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
}

#[test]
fn external_raster_completion_blocks_restart_until_its_reservation_drops() {
    let mut worker = worker();
    let old_session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    let old_completion = drive_to_raster_task(&mut worker, old_session, 1).run(&NeverCancelled);

    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                Default::default(),
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(
        worker.pump().unwrap_err().code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert_eq!(worker.worker(), WorkerId::new(1));
    drop(old_completion);
    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                Default::default(),
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    let current_session = open_ready(&mut worker, supported_scene());
    assert_eq!(current_session, old_session);
    worker.next_event();
    let current_task = drive_to_raster_task(&mut worker, current_session, 1);

    worker
        .enqueue_reentry(current_task.run(&NeverCancelled))
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert_eq!(worker.pump().unwrap(), ActorProgress::Scheduled);
    assert!(worker.resources().retained_raster_bytes() > 0);
    assert!(
        worker.resources().retained_raster_bytes()
            < FastRasterLimits::default().max_retained_bytes()
    );
}

#[test]
fn closed_session_ids_exhaust_the_epoch_without_partial_reopen() {
    let limits = NativeWorkerLimitConfig {
        scheduler: pdf_rs_scheduler::SchedulerLimits::new(1, 1, 1, 1, 1, 1, 2, 1, 1, 1).unwrap(),
        pending_resource_capacity: 1,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let session = open_ready(&mut worker, supported_scene());
    worker.next_event();
    worker
        .close_session(
            &session_correlation(worker.worker(), session),
            &CloseSessionCommand {},
        )
        .unwrap();
    for _ in 0..5 {
        let _ = worker.pump().unwrap();
        if worker.session_phase(session) == Some(SessionPhase::Closed) {
            break;
        }
    }
    drain_events(&mut worker);
    assert_eq!(worker.session_phase(session), Some(SessionPhase::Closed));
    let rejected = worker
        .open(
            &open_correlation(worker.worker(), 2),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap_err();
    assert_eq!(
        rejected.code(),
        pdf_rs_engine::EngineIntegrationErrorCode::InvalidIdentity
    );
    assert_eq!(worker.resources().sessions(), 0);
    assert_eq!(worker.resources().retained_scene_bytes(), 0);
}

#[test]
fn malformed_open_and_need_data_target_the_unfinished_open() {
    for failed_completion in [false, true] {
        let mut worker = worker();
        let request = RequestId::new(1);
        let session = worker
            .open(
                &open_correlation(worker.worker(), request.value()),
                &OpenCommand {
                    source: source_descriptor(),
                },
            )
            .unwrap();
        let completion = if failed_completion {
            pdf_rs_engine::OpenCompletion::Failed {
                worker: worker.worker(),
                worker_epoch: worker.worker_epoch(),
                session: SessionId::new(999),
                request,
            }
        } else {
            pdf_rs_engine::OpenCompletion::Ready {
                worker: worker.worker(),
                worker_epoch: worker.worker_epoch(),
                session: SessionId::new(999),
                request,
                document_revision: DOCUMENT_REVISION,
                scenes: vec![Arc::new(supported_scene())],
            }
        };
        worker.enqueue_reentry(Reentry::Open(completion)).unwrap();
        assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
        assert!(matches!(
            worker.next_event(),
            Some(NativeWorkerEvent::RequestFailed { correlation, event })
                if correlation.session == Some(session)
                    && correlation.request == Some(request)
                    && event.error.code == pdf_rs_protocol::EngineErrorCode::Internal
        ));
    }

    let mut worker = worker();
    let request = RequestId::new(1);
    let session = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::NeedData {
            worker_epoch: worker.worker_epoch(),
            correlation: pdf_rs_protocol::Correlation {
                worker: worker.worker(),
                session: Some(SessionId::new(999)),
                request: Some(request),
                generation: None,
            },
            event: pdf_rs_protocol::NeedDataEvent {
                ticket: pdf_rs_protocol::DataTicket::new(1),
                source: source_descriptor().identity,
                ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
                priority: pdf_rs_protocol::DataPriority::Metadata,
                checkpoint: 1,
            },
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::RequestFailed { correlation, event })
            if correlation.session == Some(session)
                && correlation.request == Some(request)
                && event.error.code == pdf_rs_protocol::EngineErrorCode::Internal
    ));
}

#[test]
fn cross_wired_open_completion_fails_the_claimed_open_only() {
    let mut worker = worker();
    let first_request = RequestId::new(1);
    let first = worker
        .open(
            &open_correlation(worker.worker(), first_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let second_request = RequestId::new(2);
    let second = worker
        .open(
            &open_correlation(worker.worker(), second_request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session: second,
            request: first_request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::RequestFailed { correlation, .. })
            if correlation.session == Some(second)
                && correlation.request == Some(second_request)
    ));
    assert_eq!(worker.session_phase(first), Some(SessionPhase::Opening));
    assert_eq!(worker.session_phase(second), Some(SessionPhase::Closing));

    worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: worker.worker(),
            worker_epoch: worker.worker_epoch(),
            session: first,
            request: first_request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    let mut consumed = false;
    for _ in 0..4 {
        if worker.pump().unwrap() == ActorProgress::Reentry {
            consumed = true;
            break;
        }
    }
    assert!(consumed);
    assert_eq!(worker.session_phase(first), Some(SessionPhase::Ready));
    assert!(matches!(
        worker.next_event(),
        Some(NativeWorkerEvent::DocumentReady { correlation, .. })
            if correlation.session == Some(first)
    ));
}

#[test]
fn stale_non_raster_reentries_are_rejected_after_restart() {
    let mut worker = worker();
    let old_worker = worker.worker();
    let old_epoch = worker.worker_epoch();
    let request = RequestId::new(1);
    let old_session = worker
        .open(
            &open_correlation(old_worker, request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    let stale = [
        Reentry::NeedData {
            worker_epoch: old_epoch,
            correlation: pdf_rs_protocol::Correlation {
                worker: old_worker,
                session: Some(old_session),
                request: Some(request),
                generation: None,
            },
            event: pdf_rs_protocol::NeedDataEvent {
                ticket: pdf_rs_protocol::DataTicket::new(1),
                source: source_descriptor().identity,
                ranges: vec![pdf_rs_protocol::ByteRange { start: 0, len: 4 }],
                priority: pdf_rs_protocol::DataPriority::Metadata,
                checkpoint: 1,
            },
        },
        Reentry::RangeCompleted {
            worker: old_worker,
            worker_epoch: old_epoch,
            session: old_session,
            ticket: pdf_rs_protocol::DataTicket::new(1),
            source_changed: false,
        },
        Reentry::SourceChanged {
            worker: old_worker,
            worker_epoch: old_epoch,
            session: old_session,
        },
        Reentry::Close {
            worker_epoch: old_epoch,
            correlation: session_correlation(old_worker, old_session),
        },
        Reentry::Shutdown {
            worker_epoch: old_epoch,
            correlation: pdf_rs_protocol::Correlation {
                worker: old_worker,
                session: None,
                request: None,
                generation: None,
            },
        },
    ];
    worker
        .enqueue_reentry(Reentry::Restart {
            config: NativeWorkerConfig::new(
                WorkerId::new(2),
                WorkerEpoch::new(2).unwrap(),
                7,
                Default::default(),
            )
            .unwrap(),
        })
        .unwrap();
    assert_eq!(worker.pump().unwrap(), ActorProgress::Reentry);
    let current = worker
        .open(
            &open_correlation(worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    assert_eq!(current, old_session);
    for reentry in stale {
        let rejected = worker.enqueue_reentry(reentry).unwrap_err();
        assert_eq!(
            rejected.error().code(),
            pdf_rs_engine::EngineIntegrationErrorCode::InvalidIdentity
        );
        drop(rejected.into_reentry());
    }
    assert_eq!(worker.phase(), NativeWorkerPhase::Ready);
    assert_eq!(worker.session_phase(current), Some(SessionPhase::Opening));
}

#[test]
fn scene_reservations_bound_concurrent_external_open_work() {
    let mut probe = worker();
    let _ = open_ready(&mut probe, supported_scene());
    let retained = probe.resources().retained_scene_bytes();
    assert!(retained > supported_scene().stats().retained_bytes());
    let limits = NativeWorkerLimitConfig {
        retained_scene_byte_capacity: retained,
        max_scene_bytes_per_open: retained,
        ..Default::default()
    };
    let mut worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(WorkerId::new(1), WorkerEpoch::new(1).unwrap(), 7, limits).unwrap(),
    )
    .unwrap();
    let first = open_ready(&mut worker, supported_scene());
    worker.next_event();
    assert_eq!(worker.session_phase(first), Some(SessionPhase::Ready));
    assert_eq!(worker.resources().retained_scene_bytes(), retained);
    let rejected = worker
        .open(
            &open_correlation(worker.worker(), 2),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap_err();
    assert_eq!(
        rejected.code(),
        pdf_rs_engine::EngineIntegrationErrorCode::Backpressure
    );
    assert_eq!(worker.resources().retained_scene_bytes(), retained);
}

#[test]
fn worker_cache_budget_bounds_creation_and_cross_session_admission() {
    let mut probe = worker();
    let probe_session = open_ready(&mut probe, supported_scene());
    probe.next_event();
    let metadata_bytes = probe.resources().retained_cache_bytes();
    assert!(metadata_bytes > 0);
    let _probe_surface = publish_one_surface(&mut probe, probe_session);
    let one_tile_bytes = probe.resources().retained_cache_bytes();
    assert!(one_tile_bytes > metadata_bytes);
    let tile_bytes = one_tile_bytes - metadata_bytes;

    let creation_limit = metadata_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_sub(1))
        .unwrap();
    let creation_limits = NativeWorkerLimitConfig {
        retained_cache_byte_capacity: creation_limit,
        ..Default::default()
    };
    let mut creation_worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            creation_limits,
        )
        .unwrap(),
    )
    .unwrap();
    open_ready_with_request(&mut creation_worker, supported_scene(), 1);
    creation_worker.next_event();
    let request = RequestId::new(2);
    let session = creation_worker
        .open(
            &open_correlation(creation_worker.worker(), request.value()),
            &OpenCommand {
                source: source_descriptor(),
            },
        )
        .unwrap();
    creation_worker
        .enqueue_reentry(Reentry::Open(pdf_rs_engine::OpenCompletion::Ready {
            worker: creation_worker.worker(),
            worker_epoch: creation_worker.worker_epoch(),
            session,
            request,
            document_revision: DOCUMENT_REVISION,
            scenes: vec![Arc::new(supported_scene())],
        }))
        .unwrap();
    assert_eq!(creation_worker.pump().unwrap(), ActorProgress::Reentry);
    assert!(matches!(
        creation_worker.next_event(),
        Some(NativeWorkerEvent::RequestFailed { event, .. })
            if event.error.code == pdf_rs_protocol::EngineErrorCode::ResourceLimit
    ));
    assert_eq!(
        creation_worker.resources().retained_cache_bytes(),
        metadata_bytes
    );

    let admission_limit = metadata_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(tile_bytes))
        .unwrap();
    let admission_limits = NativeWorkerLimitConfig {
        retained_cache_byte_capacity: admission_limit,
        ..Default::default()
    };
    let mut admission_worker = pdf_rs_engine::NativeWorkerRegistry::new(
        NativeWorkerConfig::new(
            WorkerId::new(1),
            WorkerEpoch::new(1).unwrap(),
            7,
            admission_limits,
        )
        .unwrap(),
    )
    .unwrap();
    let first = open_ready_with_request(&mut admission_worker, supported_scene(), 1);
    admission_worker.next_event();
    let second = open_ready_with_request(&mut admission_worker, supported_scene(), 2);
    admission_worker.next_event();
    let _first_surface = publish_one_surface(&mut admission_worker, first);
    assert_eq!(
        admission_worker.resources().retained_cache_bytes(),
        admission_limit
    );
    assert_eq!(admission_worker.pump().unwrap(), ActorProgress::Published);
    drain_events(&mut admission_worker);
    let _second_surface = publish_one_surface(&mut admission_worker, second);
    assert_eq!(
        admission_worker.resources().retained_cache_bytes(),
        admission_limit
    );
}
