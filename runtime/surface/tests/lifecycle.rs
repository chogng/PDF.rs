use pdf_rs_protocol::{
    AlphaMode, CapabilityDecisionHash, NativeBackend, PixelFormat, ProtocolLimitConfig,
    ProtocolLimits, RenderConfigHash, RenderPlanHash, RenderPlanId, RendererEpoch, SceneHash,
    SessionId, SurfaceCoordinateSpace, SurfaceId, SurfacePlanBinding, SurfaceRegion,
    SurfaceRenderIdentity, SurfaceTransport, WorkerId,
};
use pdf_rs_surface::{
    FakeHandleDescriptor, FakeHandleId, HandleAccess, HandleClass, ImportedSurface, ReleaseOutcome,
    RetireReason, SurfaceAccess, SurfaceAllocation, SurfaceConsumerContext, SurfaceErrorCode,
    SurfaceLimitConfig, SurfaceLimits, SurfaceOwner, SurfacePlanIdentity, SurfaceTransfer,
    WorkerEpoch,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);
const WORKER_EPOCH: WorkerEpoch = WorkerEpoch::new(3).expect("the test Worker epoch is nonzero");
const RENDERER_EPOCH: RendererEpoch = RendererEpoch::new(17);
const GENERATION: u64 = 13;
const WIDTH: u32 = 4;
const HEIGHT: u32 = 3;
const STRIDE: u32 = 16;
const BYTE_OFFSET: u64 = 8;
const LAYOUT_BYTES: u64 = 48;
const REGION_LENGTH: u64 = 64;

fn protocol_limits() -> ProtocolLimits {
    ProtocolLimits::new(ProtocolLimitConfig {
        max_surface_dimension: 16,
        max_surface_stride_bytes: 256,
        max_surface_bytes: 256,
        ..ProtocolLimitConfig::default()
    })
    .expect("test protocol limits are valid")
}

fn limit_config() -> SurfaceLimitConfig {
    SurfaceLimitConfig {
        protocol: protocol_limits(),
        max_sessions_per_epoch: 4,
        max_live_surfaces: 4,
        max_handles: 4,
        max_surface_ids_per_epoch: 16,
        max_total_bytes: 256,
        lease_ticks: 5,
    }
}

fn limits() -> SurfaceLimits {
    SurfaceLimits::new(limit_config()).expect("test Surface limits are valid")
}

fn test_owner() -> SurfaceOwner {
    SurfaceOwner::new(WORKER, WORKER_EPOCH, RENDERER_EPOCH, limits())
        .expect("test owner identities are valid")
}

fn region() -> SurfaceRegion {
    SurfaceRegion {
        page_index: 2,
        x: -3,
        y: 5,
        width: WIDTH,
        height: HEIGHT,
        coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
    }
}

fn identity(
    generation: u64,
    renderer_epoch: RendererEpoch,
    alpha: AlphaMode,
    digest: u8,
    plan_id: u64,
) -> SurfacePlanIdentity {
    let render = SurfaceRenderIdentity::new(
        RenderConfigHash::new([digest; 32]),
        renderer_epoch,
        RenderPlanId::new(plan_id),
        RenderPlanHash::new([digest.wrapping_add(1); 32]),
        SceneHash::new([digest.wrapping_add(2); 32]),
        CapabilityDecisionHash::new([digest.wrapping_add(3); 32]),
        NativeBackend::FastCpu,
    );
    SurfacePlanIdentity::from_protocol(
        generation,
        SurfacePlanBinding::new(region(), render),
        PixelFormat::Rgba8,
        alpha,
    )
}

fn plan(generation: u64) -> SurfacePlanIdentity {
    identity(generation, RENDERER_EPOCH, AlphaMode::Premultiplied, 1, 19)
}

fn allocation_for(
    worker: WorkerId,
    session: SessionId,
    worker_epoch: WorkerEpoch,
    plan: SurfacePlanIdentity,
) -> SurfaceAllocation {
    SurfaceAllocation {
        worker,
        session,
        worker_epoch,
        plan,
        width: WIDTH,
        height: HEIGHT,
        stride: STRIDE,
        format: PixelFormat::Rgba8,
        alpha: AlphaMode::Premultiplied,
        byte_offset: BYTE_OFFSET,
        region_length: REGION_LENGTH,
    }
}

fn allocation(plan: SurfacePlanIdentity) -> SurfaceAllocation {
    allocation_for(WORKER, SESSION, WORKER_EPOCH, plan)
}

fn context_for(
    worker: WorkerId,
    session: SessionId,
    worker_epoch: WorkerEpoch,
    plan: SurfacePlanIdentity,
) -> SurfaceConsumerContext {
    SurfaceConsumerContext {
        worker,
        session,
        worker_epoch,
        plan,
    }
}

fn context(plan: SurfacePlanIdentity) -> SurfaceConsumerContext {
    context_for(WORKER, SESSION, WORKER_EPOCH, plan)
}

fn open(owner: &mut SurfaceOwner) {
    owner
        .open_session(SESSION, GENERATION)
        .expect("test Session opens");
}

fn complete_private(owner: &mut SurfaceOwner, request: SurfaceAllocation) -> SurfaceAccess {
    let allocated = owner.allocate(request).expect("allocation succeeds");
    assert_eq!(allocated.layout_bytes(), LAYOUT_BYTES);
    assert_eq!(allocated.region_length(), REGION_LENGTH);
    let pixels = (0_u8..48).collect::<Vec<_>>();
    owner
        .write_private_pixels(allocated.access(), &pixels)
        .expect("the complete pixel range is accepted");
    allocated.access()
}

fn publish_and_transfer(
    owner: &mut SurfaceOwner,
    request: SurfaceAllocation,
) -> (SurfaceAccess, SurfaceTransfer) {
    let access = complete_private(owner, request);
    owner.publish(access).expect("publication succeeds");
    let transfer = owner.transfer(access).expect("transfer succeeds");
    (access, transfer)
}

fn assert_code<T>(result: Result<T, pdf_rs_surface::SurfaceError>, code: SurfaceErrorCode) {
    assert_eq!(
        result
            .err()
            .expect("operation must fail with a stable Surface error")
            .code(),
        code
    );
}

#[test]
fn publication_transfer_import_acquire_release_is_atomic_and_immutable() {
    let mut owner = test_owner();
    open(&mut owner);
    let allocated = owner
        .allocate(allocation(plan(GENERATION)))
        .expect("allocation succeeds");
    let access = allocated.access();

    assert_eq!(owner.current_resources().private_surfaces(), 1);
    assert_code(owner.publish(access), SurfaceErrorCode::IncompleteSurface);
    assert_eq!(owner.current_resources().private_surfaces(), 1);
    assert_code(
        owner.write_private_pixels(access, &[0; 47]),
        SurfaceErrorCode::InvalidLayout,
    );

    let pixels = (0_u8..48).collect::<Vec<_>>();
    owner
        .write_private_pixels(access, &pixels)
        .expect("the exact range completes the Surface");
    let published = owner.publish(access).expect("publication succeeds");
    assert_eq!(published.access(), access);
    assert_eq!(published.metadata().byte_offset, BYTE_OFFSET);
    assert_eq!(published.metadata().byte_length, LAYOUT_BYTES);
    assert_eq!(
        published.transport(),
        &SurfaceTransport::SharedMemory {
            slot: 0,
            region_length: REGION_LENGTH
        }
    );
    assert_code(
        owner.write_private_pixels(access, &pixels),
        SurfaceErrorCode::InvalidState,
    );

    let transfer = owner.transfer(access).expect("one transfer succeeds");
    assert_code(owner.transfer(access), SurfaceErrorCode::TransferConsumed);
    let imported = owner
        .import(transfer.clone(), &context(plan(GENERATION)))
        .expect("the exact transfer imports");
    assert_code(
        owner.import(transfer, &context(plan(GENERATION))),
        SurfaceErrorCode::TransferConsumed,
    );

    {
        let acquired = owner
            .acquire(imported, &context(plan(GENERATION)))
            .expect("the imported Surface acquires");
        let _: &[u8] = acquired.bytes();
        assert_eq!(acquired.bytes(), pixels);
        assert_eq!(acquired.metadata(), published.metadata());
    }

    let ReleaseOutcome::Released(report) = owner.release(access).expect("release succeeds") else {
        panic!("the first release must remove the live Surface");
    };
    assert_eq!(report.private_surfaces(), 0);
    assert_eq!(report.published_surfaces(), 0);
    assert_eq!(report.imported_surfaces(), 1);
    assert_eq!(report.handles(), 1);
    assert_eq!(report.released_bytes(), REGION_LENGTH);
    assert!(owner.current_resources().has_zero_surface_resources());
    assert_eq!(
        owner
            .release(access)
            .expect("duplicate release is accepted"),
        ReleaseOutcome::AlreadyRetired(RetireReason::ReleasedByHost)
    );

    let wrong_lease = SurfaceAccess::new(
        access.worker(),
        access.session(),
        access.worker_epoch(),
        access.surface(),
        published.metadata().lease_token.wrapping_add(1),
    );
    assert_code(owner.release(wrong_lease), SurfaceErrorCode::InvalidLease);
}

#[test]
fn producer_checks_identity_layout_and_capacity_before_mutation() {
    let mut owner = test_owner();
    open(&mut owner);
    let before = owner.current_resources();

    assert_code(
        owner.allocate(allocation_for(
            WorkerId::new(8),
            SESSION,
            WORKER_EPOCH,
            plan(GENERATION),
        )),
        SurfaceErrorCode::InvalidWorker,
    );
    assert_code(
        owner.allocate(allocation_for(
            WORKER,
            SESSION,
            WorkerEpoch::new(4).expect("nonzero"),
            plan(GENERATION),
        )),
        SurfaceErrorCode::InvalidWorker,
    );
    assert_code(
        owner.allocate(allocation_for(
            WORKER,
            SessionId::new(12),
            WORKER_EPOCH,
            plan(GENERATION),
        )),
        SurfaceErrorCode::InvalidSession,
    );
    assert_code(
        owner.allocate(allocation(plan(GENERATION - 1))),
        SurfaceErrorCode::InvalidGeneration,
    );
    assert_code(
        owner.allocate(allocation(identity(
            GENERATION,
            RendererEpoch::new(18),
            AlphaMode::Premultiplied,
            1,
            19,
        ))),
        SurfaceErrorCode::InvalidPlan,
    );
    assert_code(
        owner.allocate(allocation(identity(
            GENERATION,
            RENDERER_EPOCH,
            AlphaMode::Premultiplied,
            0,
            19,
        ))),
        SurfaceErrorCode::InvalidPlan,
    );
    assert_code(
        owner.allocate(allocation(identity(
            GENERATION,
            RENDERER_EPOCH,
            AlphaMode::Premultiplied,
            1,
            0,
        ))),
        SurfaceErrorCode::InvalidPlan,
    );

    let mut bad = allocation(plan(GENERATION));
    bad.width = WIDTH + 1;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.height = HEIGHT + 1;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.alpha = AlphaMode::Straight;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.stride = 15;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.stride = 18;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.stride = 260;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.byte_offset = u64::MAX;
    assert_code(owner.allocate(bad), SurfaceErrorCode::NumericOverflow);
    let mut bad = allocation(plan(GENERATION));
    bad.region_length = BYTE_OFFSET + LAYOUT_BYTES - 1;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    let mut bad = allocation(plan(GENERATION));
    bad.region_length = 257;
    assert_code(owner.allocate(bad), SurfaceErrorCode::InvalidLayout);
    assert_eq!(owner.current_resources(), before);

    let constrained = SurfaceLimits::new(SurfaceLimitConfig {
        max_sessions_per_epoch: 2,
        max_live_surfaces: 2,
        max_handles: 2,
        max_surface_ids_per_epoch: 8,
        max_total_bytes: 128,
        ..limit_config()
    })
    .expect("constrained limits are valid");
    let mut constrained_owner =
        SurfaceOwner::new(WORKER, WORKER_EPOCH, RENDERER_EPOCH, constrained).expect("owner");
    open(&mut constrained_owner);
    constrained_owner
        .allocate(allocation(plan(GENERATION)))
        .expect("first exact allocation fits");
    constrained_owner
        .allocate(allocation(plan(GENERATION)))
        .expect("second exact allocation fits");
    let full = constrained_owner.current_resources();
    assert_eq!(full.private_surfaces(), 2);
    assert_eq!(full.handles(), 2);
    assert_eq!(full.retained_bytes(), 128);
    assert_code(
        constrained_owner.allocate(allocation(plan(GENERATION))),
        SurfaceErrorCode::CapacityExceeded,
    );
    assert_eq!(constrained_owner.current_resources(), full);

    let id_limits = SurfaceLimits::new(SurfaceLimitConfig {
        max_surface_ids_per_epoch: 2,
        ..limit_config()
    })
    .expect("ID limits are valid");
    let mut id_owner =
        SurfaceOwner::new(WORKER, WORKER_EPOCH, RENDERER_EPOCH, id_limits).expect("owner");
    open(&mut id_owner);
    for reason in [RetireReason::Cancelled, RetireReason::Failed] {
        let access = id_owner
            .allocate(allocation(plan(GENERATION)))
            .expect("ID remains")
            .access();
        id_owner
            .discard_private(access, reason)
            .expect("private storage is dropped");
    }
    assert!(id_owner.current_resources().has_zero_surface_resources());
    assert_code(
        id_owner.allocate(allocation(plan(GENERATION))),
        SurfaceErrorCode::CapacityExceeded,
    );

    let session_limits = SurfaceLimits::new(SurfaceLimitConfig {
        max_sessions_per_epoch: 1,
        ..limit_config()
    })
    .expect("Session limits are valid");
    let mut session_owner =
        SurfaceOwner::new(WORKER, WORKER_EPOCH, RENDERER_EPOCH, session_limits).expect("owner");
    open(&mut session_owner);
    session_owner
        .close_session(SESSION)
        .expect("close succeeds");
    assert_code(
        session_owner.open_session(SessionId::new(12), GENERATION),
        SurfaceErrorCode::CapacityExceeded,
    );
}

#[test]
fn consumer_revalidates_metadata_transport_and_handle_without_consuming_failure() {
    let mut owner = test_owner();
    open(&mut owner);
    let (_access, transfer) = publish_and_transfer(&mut owner, allocation(plan(GENERATION)));
    let consumer = context(plan(GENERATION));

    let mut bad = transfer.clone();
    bad.metadata.lease_token = bad.metadata.lease_token.wrapping_add(1);
    assert_code(owner.import(bad, &consumer), SurfaceErrorCode::InvalidLease);

    let mut bad = transfer.clone();
    bad.metadata.owner.worker = WorkerId::new(8);
    assert_code(owner.import(bad, &consumer), SurfaceErrorCode::InvalidOwner);
    let mut bad = transfer.clone();
    bad.metadata.owner.session = SessionId::new(12);
    assert_code(owner.import(bad, &consumer), SurfaceErrorCode::InvalidOwner);

    let mut bad = transfer.clone();
    bad.metadata.generation += 1;
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidGeneration,
    );
    let mut bad = transfer.clone();
    bad.metadata.alpha = AlphaMode::Straight;
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidLayout,
    );
    let mut bad = transfer.clone();
    bad.metadata.stride += 4;
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidLayout,
    );
    let mut bad = transfer.clone();
    bad.metadata.byte_offset += 1;
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidLayout,
    );
    let mut bad = transfer.clone();
    bad.metadata.plan_hash = RenderPlanHash::new([99; 32]);
    assert_code(owner.import(bad, &consumer), SurfaceErrorCode::InvalidPlan);
    let alternate_plan = identity(GENERATION, RENDERER_EPOCH, AlphaMode::Premultiplied, 44, 20);
    let alternate_render = alternate_plan.binding().render();
    let mut coordinated_plan_swap = transfer.clone();
    coordinated_plan_swap.metadata.render_config = alternate_render.render_config();
    coordinated_plan_swap.metadata.plan_id = alternate_render.plan_id();
    coordinated_plan_swap.metadata.plan_hash = alternate_render.plan_hash();
    coordinated_plan_swap.metadata.scene_hash = alternate_render.scene_hash();
    coordinated_plan_swap.metadata.decision_hash = alternate_render.decision_hash();
    coordinated_plan_swap.metadata.backend = alternate_render.backend();
    assert_code(
        owner.import(coordinated_plan_swap, &context(alternate_plan)),
        SurfaceErrorCode::InvalidPlan,
    );

    let mut bad = transfer.clone();
    bad.transport = SurfaceTransport::SharedMemory {
        slot: 1,
        region_length: REGION_LENGTH,
    };
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidHandle,
    );
    let mut bad = transfer.clone();
    bad.transport = SurfaceTransport::SharedMemory {
        slot: 0,
        region_length: REGION_LENGTH - 1,
    };
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidHandle,
    );

    let original_parts = transfer.handle.parts();
    let mut parts = original_parts;
    parts.class = HandleClass::File;
    let mut bad = transfer.clone();
    bad.handle = FakeHandleDescriptor::from_parts(parts);
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidHandleClass,
    );
    let mut parts = original_parts;
    parts.access = HandleAccess::ReadWrite;
    let mut bad = transfer.clone();
    bad.handle = FakeHandleDescriptor::from_parts(parts);
    assert_code(
        owner.import(bad, &consumer),
        SurfaceErrorCode::InvalidHandleAccess,
    );

    let mut handle_variants = Vec::new();
    let mut parts = original_parts;
    parts.id = FakeHandleId::new(parts.id.value() + 1);
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.transfer_token = parts.transfer_token.wrapping_add(1);
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.region_length -= 1;
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.worker = WorkerId::new(8);
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.session = SessionId::new(12);
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.worker_epoch = WorkerEpoch::new(4).expect("nonzero");
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.surface = SurfaceId::new(parts.surface.value() + 1);
    handle_variants.push(parts);
    let mut parts = original_parts;
    parts.generation += 1;
    handle_variants.push(parts);

    for parts in handle_variants {
        let mut bad = transfer.clone();
        bad.handle = FakeHandleDescriptor::from_parts(parts);
        assert_code(
            owner.import(bad, &consumer),
            SurfaceErrorCode::InvalidHandle,
        );
    }

    let imported = owner
        .import(transfer, &consumer)
        .expect("failed validation did not consume the valid transfer");
    let foreign_context = context_for(WorkerId::new(8), SESSION, WORKER_EPOCH, plan(GENERATION));
    assert_code(
        owner.acquire(imported, &foreign_context),
        SurfaceErrorCode::InvalidWorker,
    );
    let stale_context = context(plan(GENERATION - 1));
    assert_code(
        owner.acquire(imported, &stale_context),
        SurfaceErrorCode::InvalidGeneration,
    );
    assert_eq!(
        owner
            .acquire(imported, &consumer)
            .expect("exact acquire succeeds")
            .bytes()
            .len(),
        usize::try_from(LAYOUT_BYTES).expect("small test layout")
    );
}

#[test]
fn generation_replacement_and_private_terminal_paths_drop_storage() {
    let mut owner = test_owner();
    open(&mut owner);
    let private = owner
        .allocate(allocation(plan(GENERATION)))
        .expect("private allocation")
        .access();
    let published = complete_private(&mut owner, allocation(plan(GENERATION)));
    owner.publish(published).expect("publication");
    assert_eq!(owner.current_resources().retained_bytes(), 128);

    let replaced = owner
        .replace_generation(SESSION, GENERATION + 1)
        .expect("strictly newer generation is accepted");
    assert_eq!(replaced.released().private_surfaces(), 1);
    assert_eq!(replaced.released().published_surfaces(), 1);
    assert_eq!(replaced.released().imported_surfaces(), 0);
    assert_eq!(replaced.released().handles(), 2);
    assert_eq!(replaced.released().released_bytes(), 128);
    assert!(replaced.current().has_zero_surface_resources());
    assert_eq!(
        owner
            .release(private)
            .expect("exact stale private tombstone"),
        ReleaseOutcome::AlreadyRetired(RetireReason::StaleGeneration)
    );
    assert_eq!(
        owner.release(published).expect("exact replaced tombstone"),
        ReleaseOutcome::AlreadyRetired(RetireReason::GenerationReplaced)
    );
    assert_code(
        owner.write_private_pixels(private, &[0; 48]),
        SurfaceErrorCode::UnknownSurface,
    );
    assert_code(
        owner.replace_generation(SESSION, GENERATION + 1),
        SurfaceErrorCode::InvalidGeneration,
    );

    for reason in [RetireReason::Cancelled, RetireReason::Failed] {
        let access = owner
            .allocate(allocation(plan(GENERATION + 1)))
            .expect("new-generation private allocation")
            .access();
        let report = owner
            .discard_private(access, reason)
            .expect("valid private terminal path");
        assert_eq!(report.private_surfaces(), 1);
        assert_eq!(report.handles(), 1);
        assert_eq!(report.released_bytes(), REGION_LENGTH);
        assert_eq!(
            owner.release(access).expect("terminal path is remembered"),
            ReleaseOutcome::AlreadyRetired(reason)
        );
    }
    assert!(owner.current_resources().has_zero_surface_resources());
}

#[test]
fn virtual_clock_reclaims_at_exact_deadline_and_overflow_is_atomic() {
    let mut owner = test_owner();
    open(&mut owner);
    let published = complete_private(&mut owner, allocation(plan(GENERATION)));
    owner.publish(published).expect("publication");
    let before_deadline = owner.advance_clock(4).expect("clock advances");
    assert!(before_deadline.released().is_zero());
    assert_eq!(before_deadline.current().published_surfaces(), 1);
    let at_deadline = owner.advance_clock(1).expect("exact deadline advances");
    assert_eq!(at_deadline.released().published_surfaces(), 1);
    assert_eq!(at_deadline.released().released_bytes(), REGION_LENGTH);
    assert!(at_deadline.current().has_zero_surface_resources());
    assert_eq!(
        owner.release(published).expect("expiry is remembered"),
        ReleaseOutcome::AlreadyRetired(RetireReason::LeaseExpired)
    );

    let private = owner
        .allocate(allocation(plan(GENERATION)))
        .expect("private storage")
        .access();
    owner.advance_clock(100).expect("private work has no lease");
    assert_eq!(owner.current_resources().private_surfaces(), 1);
    owner
        .discard_private(private, RetireReason::Cancelled)
        .expect("cleanup");

    let mut overflow_owner = test_owner();
    open(&mut overflow_owner);
    overflow_owner
        .advance_clock(u64::MAX)
        .expect("maximum tick from zero is representable");
    let private = complete_private(&mut overflow_owner, allocation(plan(GENERATION)));
    assert_code(
        overflow_owner.publish(private),
        SurfaceErrorCode::NumericOverflow,
    );
    assert_eq!(overflow_owner.current_resources().private_surfaces(), 1);
    assert_eq!(overflow_owner.virtual_tick(), u64::MAX);
    assert_code(
        overflow_owner.advance_clock(1),
        SurfaceErrorCode::NumericOverflow,
    );
    assert_eq!(overflow_owner.virtual_tick(), u64::MAX);
}

#[test]
fn session_close_and_worker_restart_return_zero_current_evidence() {
    let mut owner = test_owner();
    open(&mut owner);
    let second_session = SessionId::new(12);
    owner
        .open_session(second_session, GENERATION)
        .expect("second Session opens");

    let (first_access, first_transfer) =
        publish_and_transfer(&mut owner, allocation(plan(GENERATION)));
    owner
        .import(first_transfer, &context(plan(GENERATION)))
        .expect("first Session imports");
    let second_access = owner
        .allocate(allocation_for(
            WORKER,
            second_session,
            WORKER_EPOCH,
            plan(GENERATION),
        ))
        .expect("second Session allocation")
        .access();

    let closed = owner.close_session(SESSION).expect("close succeeds");
    assert_eq!(closed.released().imported_surfaces(), 1);
    assert_eq!(closed.current().active_sessions(), 1);
    assert_eq!(closed.current().private_surfaces(), 1);
    assert_eq!(
        owner.release(first_access).expect("close tombstone"),
        ReleaseOutcome::AlreadyRetired(RetireReason::SessionClosed)
    );
    let repeated = owner.close_session(SESSION).expect("repeat close succeeds");
    assert!(repeated.released().is_zero());
    assert_eq!(repeated.current(), owner.current_resources());
    assert_code(
        owner.allocate(allocation(plan(GENERATION))),
        SurfaceErrorCode::InvalidSession,
    );
    owner
        .advance_clock(2)
        .expect("old epoch virtual clock advances");

    let before_invalid_restart = owner.current_resources();
    assert_code(
        owner.restart(WORKER, WORKER_EPOCH, RendererEpoch::new(18)),
        SurfaceErrorCode::InvalidWorker,
    );
    assert_eq!(owner.current_resources(), before_invalid_restart);

    let restarted = owner
        .restart(
            WorkerId::new(8),
            WorkerEpoch::new(4).expect("nonzero"),
            RendererEpoch::new(18),
        )
        .expect("distinct increasing epoch restarts");
    assert_eq!(restarted.released().private_surfaces(), 1);
    assert_eq!(restarted.released().handles(), 1);
    assert_eq!(restarted.current().active_sessions(), 0);
    assert!(restarted.current().has_zero_surface_resources());
    assert_eq!(owner.virtual_tick(), 0);
    assert_code(
        owner.release(second_access),
        SurfaceErrorCode::InvalidWorker,
    );

    let new_worker = WorkerId::new(8);
    let new_epoch = WorkerEpoch::new(4).expect("nonzero");
    let new_renderer = RendererEpoch::new(18);
    owner
        .open_session(SESSION, GENERATION)
        .expect("new epoch admits the numeric Session ID again");
    let new_access = owner
        .allocate(allocation_for(
            new_worker,
            SESSION,
            new_epoch,
            identity(GENERATION, new_renderer, AlphaMode::Premultiplied, 1, 19),
        ))
        .expect("new epoch allocation")
        .access();
    assert_eq!(new_access.surface().value(), first_access.surface().value());
    assert_ne!(new_access, first_access);
}

#[test]
fn limits_errors_and_debug_output_are_stable_and_redacted() {
    let hard_max = SurfaceLimitConfig {
        protocol: ProtocolLimits::default(),
        max_sessions_per_epoch: 65_536,
        max_live_surfaces: 65_536,
        max_handles: 65_536,
        max_surface_ids_per_epoch: 16 * 1024 * 1024,
        max_total_bytes: 16 * 1024 * 1024 * 1024,
        lease_ticks: 1_000_000_000_000,
    };
    SurfaceLimits::new(hard_max).expect("exact hard ceilings are accepted");

    let invalid = [
        SurfaceLimitConfig {
            max_sessions_per_epoch: 0,
            ..hard_max
        },
        SurfaceLimitConfig {
            max_live_surfaces: 65_537,
            ..hard_max
        },
        SurfaceLimitConfig {
            max_handles: 0,
            ..hard_max
        },
        SurfaceLimitConfig {
            max_surface_ids_per_epoch: 16 * 1024 * 1024 + 1,
            ..hard_max
        },
        SurfaceLimitConfig {
            max_total_bytes: 16 * 1024 * 1024 * 1024 + 1,
            ..hard_max
        },
        SurfaceLimitConfig {
            lease_ticks: 1_000_000_000_001,
            ..hard_max
        },
    ];
    for config in invalid {
        assert_code(SurfaceLimits::new(config), SurfaceErrorCode::InvalidLimits);
    }

    let mut owner = test_owner();
    open(&mut owner);
    let allocated = owner
        .allocate(allocation(plan(GENERATION)))
        .expect("allocation");
    let pixels = vec![0x6d; 48];
    owner
        .write_private_pixels(allocated.access(), &pixels)
        .expect("complete");
    let published = owner.publish(allocated.access()).expect("publish");
    let transfer = owner.transfer(allocated.access()).expect("transfer");
    let parts = transfer.handle.parts();
    let secret_strings = [
        published.metadata().lease_token.to_string(),
        parts.transfer_token.to_string(),
        "109".repeat(8),
    ];

    for debug in [
        format!("{:?}", allocated.access()),
        format!("{allocated:?}"),
        format!("{published:?}"),
        format!("{transfer:?}"),
        format!("{parts:?}"),
        format!("{:?}", transfer.handle),
        format!("{owner:?}"),
    ] {
        assert!(debug.contains("[REDACTED]"));
        for secret in &secret_strings {
            assert!(
                !debug.contains(secret),
                "sensitive value leaked through Debug: {debug}"
            );
        }
    }
    assert!(!format!("{parts:?}").contains(&format!("FakeHandleId({})", parts.id.value())));
    assert!(
        !format!("{:?}", transfer.handle).contains(&format!("FakeHandleId({})", parts.id.value()))
    );

    let imported = owner
        .import(transfer, &context(plan(GENERATION)))
        .expect("import");
    let imported_debug = format!("{imported:?}");
    assert!(imported_debug.contains("[REDACTED]"));
    for secret in &secret_strings {
        assert!(!imported_debug.contains(secret));
    }
    let acquired = owner
        .acquire(imported, &context(plan(GENERATION)))
        .expect("acquire");
    let acquired_debug = format!("{acquired:?}");
    assert!(acquired_debug.contains("[BYTES:48]"));
    assert!(!acquired_debug.contains(&"109".repeat(8)));

    let bad_access = SurfaceAccess::new(
        WORKER,
        SESSION,
        WORKER_EPOCH,
        allocated.access().surface(),
        0,
    );
    let error = owner
        .release(bad_access)
        .expect_err("zero lease is rejected");
    assert_eq!(error.code(), SurfaceErrorCode::InvalidLease);
    assert_eq!(error.stable_id(), "RPE-SURFACE-0013");
    assert_eq!(error.to_string(), "RPE-SURFACE-0013");
    assert_eq!(
        format!("{error:?}"),
        "SurfaceError { code: InvalidLease, stable_id: \"RPE-SURFACE-0013\" }"
    );

    let _: fn(&pdf_rs_policy::RenderPlan, usize) -> Result<SurfacePlanIdentity, _> =
        SurfacePlanIdentity::from_render_plan;
    let _: Option<WorkerEpoch> = WorkerEpoch::new(1);
    let _: Option<WorkerEpoch> = WorkerEpoch::new(0);
    let _: Option<ImportedSurface> = None;
}
