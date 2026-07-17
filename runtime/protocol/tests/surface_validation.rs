use pdf_rs_protocol::{
    AlphaMode, BrowserTransferKind, CanvasId, CapabilityDecisionHash, MemoryEpoch, NativeBackend,
    PixelFormat, PlatformHandle, ProtocolErrorCode, ProtocolLimits, ProtocolValidator,
    RenderConfigHash, RenderPlanHash, RenderPlanId, RendererEpoch, SceneHash, SessionId,
    SurfaceCoordinateSpace, SurfaceId, SurfaceMetadata, SurfaceOwner, SurfacePlanBinding,
    SurfaceRegion, SurfaceRenderIdentity, SurfaceTransport, SurfaceValidationContext, WorkerId,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);
const GENERATION: u64 = 13;
const REGION_BYTES: u64 = 64;
const LAYOUT_BYTES: u64 = 48;

fn region() -> SurfaceRegion {
    SurfaceRegion {
        page_index: 2,
        x: -17,
        y: 23,
        width: 101,
        height: 103,
        coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
    }
}

fn render_identity() -> SurfaceRenderIdentity {
    SurfaceRenderIdentity::new(
        RenderConfigHash::new([1; 32]),
        RendererEpoch::new(17),
        RenderPlanId::new(19),
        RenderPlanHash::new([2; 32]),
        SceneHash::new([3; 32]),
        CapabilityDecisionHash::new([4; 32]),
        NativeBackend::ReferenceCpu,
    )
}

fn plan() -> SurfacePlanBinding {
    SurfacePlanBinding::new(region(), render_identity())
}

fn metadata() -> SurfaceMetadata {
    let render = render_identity();
    SurfaceMetadata {
        id: SurfaceId::new(29),
        owner: SurfaceOwner {
            worker: WORKER,
            session: SESSION,
        },
        generation: GENERATION,
        region: region(),
        width: 4,
        height: 3,
        stride: 16,
        format: PixelFormat::Rgba8,
        alpha: AlphaMode::Premultiplied,
        byte_offset: 8,
        byte_length: LAYOUT_BYTES,
        render_config: render.render_config(),
        renderer_epoch: render.renderer_epoch(),
        plan_id: render.plan_id(),
        plan_hash: render.plan_hash(),
        scene_hash: render.scene_hash(),
        decision_hash: render.decision_hash(),
        backend: render.backend(),
    }
}

fn context(transfer_slots: usize) -> SurfaceValidationContext {
    SurfaceValidationContext::new(WORKER, SESSION, GENERATION, plan(), transfer_slots)
}

fn validator() -> ProtocolValidator {
    ProtocolValidator::new(ProtocolLimits::default())
}

#[test]
fn every_surface_transport_binds_the_actual_receiver_resource() {
    let metadata = metadata();
    let cases = [
        (
            SurfaceTransport::OffscreenCanvasCommit {
                canvas: CanvasId::new(31),
                region_length: REGION_BYTES,
            },
            context(0).with_offscreen_canvas(CanvasId::new(31), REGION_BYTES),
        ),
        (
            SurfaceTransport::BrowserTransfer {
                slot: 0,
                transfer_kind: BrowserTransferKind::ArrayBuffer,
                transfer_length: REGION_BYTES,
            },
            context(1).with_browser_transfer(0, BrowserTransferKind::ArrayBuffer, REGION_BYTES),
        ),
        (
            SurfaceTransport::SharedMemory {
                handle: PlatformHandle::new(37),
                region_length: REGION_BYTES,
                release_token: 41,
            },
            context(1).with_shared_memory(PlatformHandle::new(37), REGION_BYTES),
        ),
        (
            SurfaceTransport::LocalMemory {
                region_length: REGION_BYTES,
                memory_epoch: MemoryEpoch::new(43),
            },
            context(0).with_local_memory(MemoryEpoch::new(43), REGION_BYTES),
        ),
    ];

    for (transport, context) in cases {
        let surface = validator()
            .validate_surface(&metadata, &transport, &context)
            .unwrap();
        assert_eq!(surface.layout_bytes(), LAYOUT_BYTES);
        assert_eq!(surface.metadata(), &metadata);
        assert_eq!(surface.transport(), &transport);
    }
}

#[test]
fn every_owner_plan_epoch_hash_backend_and_region_mutation_is_rejected() {
    let transport = SurfaceTransport::SharedMemory {
        handle: PlatformHandle::new(37),
        region_length: REGION_BYTES,
        release_token: 41,
    };
    let shared_context = context(1).with_shared_memory(PlatformHandle::new(37), REGION_BYTES);
    let original = metadata();
    let mut cases = Vec::new();

    let mut value = original.clone();
    value.id = SurfaceId::new(0);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceOwner));
    let mut value = original.clone();
    value.owner.worker = WorkerId::new(8);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceOwner));
    let mut value = original.clone();
    value.owner.session = SessionId::new(12);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceOwner));
    let mut value = original.clone();
    value.generation += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceEpoch));
    let mut value = original.clone();
    value.renderer_epoch = RendererEpoch::new(18);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceEpoch));
    let mut value = original.clone();
    value.render_config = RenderConfigHash::new([9; 32]);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.plan_id = RenderPlanId::new(20);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.plan_hash = RenderPlanHash::new([9; 32]);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.scene_hash = SceneHash::new([9; 32]);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.decision_hash = CapabilityDecisionHash::new([9; 32]);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.backend = NativeBackend::FastCpu;
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.region.page_index += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));
    let mut value = original.clone();
    value.region.x += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));
    let mut value = original.clone();
    value.region.y += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));
    let mut value = original.clone();
    value.region.width += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));
    let mut value = original.clone();
    value.region.height += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));

    for (mutated, expected) in cases {
        assert_eq!(
            validator()
                .validate_surface(&mutated, &transport, &shared_context)
                .unwrap_err()
                .code(),
            expected
        );
    }
}

#[test]
fn checked_layout_and_metadata_range_fail_closed() {
    let transport = SurfaceTransport::SharedMemory {
        handle: PlatformHandle::new(37),
        region_length: REGION_BYTES,
        release_token: 41,
    };
    let shared_context = context(1).with_shared_memory(PlatformHandle::new(37), REGION_BYTES);
    let original = metadata();
    let mut cases = Vec::new();

    let mut value = original.clone();
    value.width = 0;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.height = 0;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.stride = 0;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.stride = 15;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.height = 32_768;
    value.stride = 262_144;
    value.byte_length = u64::from(value.height) * u64::from(value.stride);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.byte_length = 0;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRange));
    let mut value = original.clone();
    value.byte_length -= 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRange));
    let mut value = original.clone();
    value.byte_offset = u64::MAX;
    cases.push((value, ProtocolErrorCode::NumericOverflow));
    let mut value = original.clone();
    value.byte_offset += 9;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRange));

    for (mutated, expected) in cases {
        assert_eq!(
            validator()
                .validate_surface(&mutated, &transport, &shared_context)
                .unwrap_err()
                .code(),
            expected
        );
    }

    let short_declared = SurfaceTransport::SharedMemory {
        handle: PlatformHandle::new(37),
        region_length: REGION_BYTES - 1,
        release_token: 41,
    };
    assert_eq!(
        validator()
            .validate_surface(&original, &short_declared, &shared_context)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceRange
    );
    let zero_declared = SurfaceTransport::SharedMemory {
        handle: PlatformHandle::new(37),
        region_length: 0,
        release_token: 41,
    };
    assert_eq!(
        validator()
            .validate_surface(&original, &zero_declared, &shared_context)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceRange
    );
    let short_actual = context(1).with_shared_memory(PlatformHandle::new(37), REGION_BYTES - 1);
    assert_eq!(
        validator()
            .validate_surface(&original, &transport, &short_actual)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceRange
    );
}

#[test]
fn slot_kind_handle_release_and_memory_epoch_mismatches_fail_closed() {
    let metadata = metadata();
    let browser = SurfaceTransport::BrowserTransfer {
        slot: 0,
        transfer_kind: BrowserTransferKind::ArrayBuffer,
        transfer_length: REGION_BYTES,
    };
    let browser_context =
        context(1).with_browser_transfer(0, BrowserTransferKind::ArrayBuffer, REGION_BYTES);
    for transport in [
        SurfaceTransport::BrowserTransfer {
            slot: 1,
            transfer_kind: BrowserTransferKind::ArrayBuffer,
            transfer_length: REGION_BYTES,
        },
        SurfaceTransport::BrowserTransfer {
            slot: 0,
            transfer_kind: BrowserTransferKind::ImageBitmap,
            transfer_length: REGION_BYTES,
        },
    ] {
        assert_eq!(
            validator()
                .validate_surface(&metadata, &transport, &browser_context)
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidSurfaceSlot
        );
    }
    for transfer_slots in [0, 2] {
        assert_eq!(
            validator()
                .validate_surface(
                    &metadata,
                    &browser,
                    &context(transfer_slots).with_browser_transfer(
                        0,
                        BrowserTransferKind::ArrayBuffer,
                        REGION_BYTES,
                    ),
                )
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidSurfaceSlot
        );
    }

    let canvas = SurfaceTransport::OffscreenCanvasCommit {
        canvas: CanvasId::new(31),
        region_length: REGION_BYTES,
    };
    assert_eq!(
        validator()
            .validate_surface(
                &metadata,
                &canvas,
                &context(0).with_offscreen_canvas(CanvasId::new(32), REGION_BYTES),
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceSlot
    );

    let shared_context = context(1).with_shared_memory(PlatformHandle::new(37), REGION_BYTES);
    for transport in [
        SurfaceTransport::SharedMemory {
            handle: PlatformHandle::new(38),
            region_length: REGION_BYTES,
            release_token: 41,
        },
        SurfaceTransport::SharedMemory {
            handle: PlatformHandle::new(37),
            region_length: REGION_BYTES,
            release_token: 0,
        },
    ] {
        assert_eq!(
            validator()
                .validate_surface(&metadata, &transport, &shared_context)
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidSurfaceSlot
        );
    }

    let local = SurfaceTransport::LocalMemory {
        region_length: REGION_BYTES,
        memory_epoch: MemoryEpoch::new(44),
    };
    assert_eq!(
        validator()
            .validate_surface(
                &metadata,
                &local,
                &context(0).with_local_memory(MemoryEpoch::new(43), REGION_BYTES),
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceEpoch
    );
    assert_eq!(
        validator()
            .validate_surface(
                &metadata,
                &SurfaceTransport::LocalMemory {
                    region_length: REGION_BYTES,
                    memory_epoch: MemoryEpoch::new(0),
                },
                &context(0).with_local_memory(MemoryEpoch::new(0), REGION_BYTES),
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceEpoch
    );
    assert_eq!(
        validator()
            .validate_surface(
                &metadata,
                &SurfaceTransport::SharedMemory {
                    handle: PlatformHandle::new(37),
                    region_length: REGION_BYTES,
                    release_token: 41,
                },
                &context(usize::MAX).with_shared_memory(PlatformHandle::new(37), REGION_BYTES),
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSurfaceSlot
    );
}

#[test]
fn surface_debug_output_redacts_platform_handles() {
    let context = context(1).with_shared_memory(PlatformHandle::new(0xdead_beef), REGION_BYTES);
    let debug = format!("{context:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("PlatformHandle"));
    assert!(!debug.contains("3735928559"));

    let transport = SurfaceTransport::SharedMemory {
        handle: PlatformHandle::new(0xdead_beef),
        region_length: REGION_BYTES,
        release_token: 41,
    };
    let surface = validator()
        .validate_surface(&metadata(), &transport, &context)
        .unwrap();
    let debug = format!("{surface:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("PlatformHandle"));
    assert!(!debug.contains("3735928559"));
}
