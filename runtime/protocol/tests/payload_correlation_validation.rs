use pdf_rs_protocol::{
    AlphaMode, CancelCommand, CapabilityDecision, CapabilityDecisionHash, CapabilityProfileId,
    CapabilityReportedEvent, CapabilityScope, CapabilityScopeKind, CapabilitySubject,
    CollectionCompleteness, Command, CommandEnvelope, Correlation, DocumentReadyEvent,
    ENDPOINT_CAPABILITY_LOCAL_MEMORY, EndpointCapabilities, EndpointRole,
    EngineExecutionCapabilities, EnvelopeHeader, Event, EventEnvelope, GenerationPlannedEvent,
    GeometryHash, MESSAGE_ID_CANCEL, MESSAGE_ID_CAPABILITY_REPORTED, MESSAGE_ID_DOCUMENT_READY,
    MESSAGE_ID_GENERATION_PLANNED, MESSAGE_ID_OPEN, MESSAGE_ID_READY, MESSAGE_ID_REQUEST_CANCELLED,
    MESSAGE_ID_SESSION_CLOSED, MESSAGE_ID_SURFACE_READY, MESSAGE_ID_WORKER_STOPPED, MemoryEpoch,
    NativeBackend, OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR, PageRotation, PixelFormat,
    ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProtocolValidator, QualityPolicy, ReadyEvent,
    RenderConfigHash, RenderPlanHash, RenderPlanId, RenderPlanManifest, RendererEpoch,
    RequestCancelledEvent, RequestId, SCHEMA_HASH, SceneHash, SessionClosedEvent, SessionId,
    SourceIdentity, SupportStatus, SurfaceCoordinateSpace, SurfaceId, SurfaceMetadata,
    SurfaceOwner, SurfacePlanBinding, SurfaceReadyEvent, SurfaceRegion, SurfaceRenderIdentity,
    SurfaceTransport, SurfaceValidationContext, TileContentHash, WorkerId, WorkerStoppedEvent,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);
const REQUEST: RequestId = RequestId::new(13);
const GENERATION: u64 = 17;

fn header(message_type: u16) -> EnvelopeHeader {
    EnvelopeHeader {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        message_type,
        flags: 0,
        payload_len: 0,
        sequence: 1,
    }
}

fn correlation(
    session: Option<SessionId>,
    request: Option<RequestId>,
    generation: Option<u64>,
) -> Correlation {
    Correlation {
        worker: WORKER,
        session,
        request,
        generation,
    }
}

fn validator() -> ProtocolValidator {
    ProtocolValidator::new(ProtocolLimits::default())
}

#[test]
fn command_header_variant_and_cancel_target_are_bound_atomically() {
    let valid = CommandEnvelope {
        header: header(MESSAGE_ID_CANCEL),
        correlation: correlation(Some(SESSION), Some(REQUEST), None),
        command: Command::Cancel(CancelCommand { target: REQUEST }),
    };
    validator()
        .validate_command_payload_correlation(&valid, WORKER, Some(SESSION))
        .unwrap();

    let mut wrong_target = valid.clone();
    let Command::Cancel(command) = &mut wrong_target.command else {
        unreachable!()
    };
    command.target = RequestId::new(19);
    assert_eq!(
        validator()
            .validate_command_payload_correlation(&wrong_target, WORKER, Some(SESSION))
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );

    let mut wrong_header = valid;
    wrong_header.header.message_type = MESSAGE_ID_OPEN;
    let error = validator()
        .validate_command_payload_correlation(&wrong_header, WORKER, Some(SESSION))
        .unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::InvalidMessageBinding);
    assert_eq!(error.diagnostic_id(), "RPE-PROTOCOL-0033");
}

#[test]
fn ready_document_and_cancelled_payload_ids_match_optional_correlation() {
    let ready = EventEnvelope {
        header: header(MESSAGE_ID_READY),
        correlation: correlation(None, None, None),
        event: Event::Ready(ReadyEvent {
            worker: WORKER,
            negotiated_minor: PROTOCOL_MINOR,
            schema_hash: SCHEMA_HASH,
            execution_capabilities: EngineExecutionCapabilities { supported: 0 },
            capability_profiles: vec![CapabilityProfileId::BaselineNative],
            output_profiles: vec![OutputProfile::Srgb],
        }),
    };
    validator()
        .validate_event_payload_correlation(&ready, WORKER, None)
        .unwrap();
    let mut wrong_ready = ready;
    let Event::Ready(event) = &mut wrong_ready.event else {
        unreachable!()
    };
    event.worker = WorkerId::new(23);
    assert_invalid_event(&wrong_ready, None);

    let document = EventEnvelope {
        header: header(MESSAGE_ID_DOCUMENT_READY),
        correlation: correlation(Some(SESSION), Some(REQUEST), None),
        event: Event::DocumentReady(DocumentReadyEvent {
            session: SESSION,
            document_revision: 1,
            page_count: 1,
            profile: CapabilityProfileId::BaselineNative,
            policy_version: 1,
        }),
    };
    validator()
        .validate_event_payload_correlation(&document, WORKER, Some(SESSION))
        .unwrap();
    let mut correlated_document = document;
    let Event::DocumentReady(event) = &mut correlated_document.event else {
        unreachable!()
    };
    event.session = SessionId::new(29);
    assert_invalid_event(&correlated_document, Some(SESSION));

    let cancelled = EventEnvelope {
        header: header(MESSAGE_ID_REQUEST_CANCELLED),
        correlation: correlation(Some(SESSION), Some(REQUEST), None),
        event: Event::RequestCancelled(RequestCancelledEvent { target: REQUEST }),
    };
    validator()
        .validate_event_payload_correlation(&cancelled, WORKER, Some(SESSION))
        .unwrap();
    let mut wrong_cancelled = cancelled;
    let Event::RequestCancelled(event) = &mut wrong_cancelled.event else {
        unreachable!()
    };
    event.target = RequestId::new(31);
    assert_invalid_event(&wrong_cancelled, Some(SESSION));
}

#[test]
fn session_closed_and_worker_stopped_payload_ids_match_correlation() {
    let closed = EventEnvelope {
        header: header(MESSAGE_ID_SESSION_CLOSED),
        correlation: correlation(Some(SESSION), None, None),
        event: Event::SessionClosed(SessionClosedEvent { session: SESSION }),
    };
    validator()
        .validate_event_payload_correlation(&closed, WORKER, Some(SESSION))
        .unwrap();
    let mut wrong_closed = closed;
    let Event::SessionClosed(event) = &mut wrong_closed.event else {
        unreachable!()
    };
    event.session = SessionId::new(37);
    assert_invalid_event(&wrong_closed, Some(SESSION));

    let stopped = EventEnvelope {
        header: header(MESSAGE_ID_WORKER_STOPPED),
        correlation: correlation(None, None, None),
        event: Event::WorkerStopped(WorkerStoppedEvent { worker: WORKER }),
    };
    validator()
        .validate_event_payload_correlation(&stopped, WORKER, None)
        .unwrap();
    let mut wrong_stopped = stopped;
    let Event::WorkerStopped(event) = &mut wrong_stopped.event else {
        unreachable!()
    };
    event.worker = WorkerId::new(41);
    assert_invalid_event(&wrong_stopped, None);
}

#[test]
fn capability_and_generation_events_require_semantic_invariants_before_dispatch() {
    let capability = EventEnvelope {
        header: header(MESSAGE_ID_CAPABILITY_REPORTED),
        correlation: correlation(Some(SESSION), None, Some(GENERATION)),
        event: Event::CapabilityReported(CapabilityReportedEvent {
            decision: supported_decision(),
            decision_hash: CapabilityDecisionHash::new([2; 32]),
        }),
    };
    validator()
        .validate_event_payload_correlation(&capability, WORKER, Some(SESSION))
        .unwrap();

    let mut invalid_accounting = capability.clone();
    let Event::CapabilityReported(event) = &mut invalid_accounting.event else {
        unreachable!()
    };
    event.decision.locations_total = 2;
    assert_eq!(
        validator()
            .validate_event_payload_correlation(&invalid_accounting, WORKER, Some(SESSION))
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidPayloadEncoding
    );

    let mut zero_decision_hash = capability;
    let Event::CapabilityReported(event) = &mut zero_decision_hash.event else {
        unreachable!()
    };
    event.decision_hash = CapabilityDecisionHash::new([0; 32]);
    assert_eq!(
        validator()
            .validate_event_payload_correlation(&zero_decision_hash, WORKER, Some(SESSION))
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidPayloadEncoding
    );

    let generation = EventEnvelope {
        header: header(MESSAGE_ID_GENERATION_PLANNED),
        correlation: correlation(Some(SESSION), None, Some(GENERATION)),
        event: Event::GenerationPlanned(GenerationPlannedEvent {
            manifest: render_plan_manifest(),
            plan_hash: RenderPlanHash::new([7; 32]),
        }),
    };
    validator()
        .validate_event_payload_correlation(&generation, WORKER, Some(SESSION))
        .unwrap();

    let mut wrong_generation = generation.clone();
    wrong_generation.correlation.generation = Some(GENERATION + 1);
    assert_eq!(
        validator()
            .validate_event_payload_correlation(&wrong_generation, WORKER, Some(SESSION))
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );

    let mut missing_tile_hash = generation;
    let Event::GenerationPlanned(event) = &mut missing_tile_hash.event else {
        unreachable!()
    };
    event.manifest.tile_content_hashes.clear();
    assert_eq!(
        validator()
            .validate_event_payload_correlation(&missing_tile_hash, WORKER, Some(SESSION))
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidPayloadEncoding
    );
}

#[test]
fn surface_ready_correlation_and_resource_validation_are_one_operation() {
    let render = render_identity();
    let region = surface_region();
    let event = SurfaceReadyEvent {
        metadata: SurfaceMetadata {
            id: SurfaceId::new(43),
            lease_token: 45,
            owner: SurfaceOwner {
                worker: WORKER,
                session: SESSION,
            },
            generation: GENERATION,
            region: region.clone(),
            width: 4,
            height: 3,
            stride: 16,
            format: PixelFormat::Rgba8,
            alpha: AlphaMode::Premultiplied,
            byte_offset: 8,
            byte_length: 48,
            render_config: render.render_config(),
            renderer_epoch: render.renderer_epoch(),
            plan_id: render.plan_id(),
            plan_hash: render.plan_hash(),
            scene_hash: render.scene_hash(),
            decision_hash: render.decision_hash(),
            backend: render.backend(),
        },
        transport: SurfaceTransport::LocalMemory {
            region_length: 64,
            memory_epoch: MemoryEpoch::new(47),
        },
    };
    let envelope = EventEnvelope {
        header: header(MESSAGE_ID_SURFACE_READY),
        correlation: correlation(Some(SESSION), None, Some(GENERATION)),
        event: Event::SurfaceReady(event),
    };
    let context = SurfaceValidationContext::new(
        WORKER,
        SESSION,
        GENERATION,
        SurfacePlanBinding::new(region, render),
        local_memory_handshake(),
        0,
    )
    .with_local_memory(MemoryEpoch::new(47), 64);

    let validated = validator()
        .validate_surface_ready(&envelope, &context)
        .unwrap();
    assert_eq!(validated.layout_bytes(), 48);

    let mut wrong_generation = envelope.clone();
    wrong_generation.correlation.generation = Some(GENERATION + 1);
    assert_eq!(
        validator()
            .validate_surface_ready(&wrong_generation, &context)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );

    let mut wrong_owner = envelope;
    let Event::SurfaceReady(event) = &mut wrong_owner.event else {
        unreachable!()
    };
    event.metadata.owner.worker = WorkerId::new(53);
    assert_eq!(
        validator()
            .validate_surface_ready(&wrong_owner, &context)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );
}

fn assert_invalid_event(envelope: &EventEnvelope, session: Option<SessionId>) {
    assert_eq!(
        validator()
            .validate_event_payload_correlation(envelope, WORKER, session)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );
}

fn render_identity() -> SurfaceRenderIdentity {
    SurfaceRenderIdentity::new(
        RenderConfigHash::new([1; 32]),
        RendererEpoch::new(3),
        RenderPlanId::new(5),
        RenderPlanHash::new([2; 32]),
        SceneHash::new([3; 32]),
        CapabilityDecisionHash::new([4; 32]),
        NativeBackend::ReferenceCpu,
    )
}

fn surface_region() -> SurfaceRegion {
    SurfaceRegion {
        page_index: 0,
        x: 0,
        y: 0,
        width: 4,
        height: 3,
        coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
    }
}

fn supported_decision() -> CapabilityDecision {
    CapabilityDecision {
        decision_schema_version: 1,
        status: SupportStatus::Supported,
        profile: CapabilityProfileId::BaselineNative,
        profile_version: 1,
        policy_version: 1,
        subject: CapabilitySubject {
            source: SourceIdentity {
                stable_id: [1; 32],
                revision: 1,
            },
            document_revision: 1,
            revision_startxref: 1,
            page_index: 0,
            page_object_number: 1,
            page_object_generation: 0,
            scene_schema_major: 1,
            scene_schema_minor: 0,
            scene_hash: SceneHash::new([1; 32]),
        },
        missing: Vec::new(),
        missing_total: 0,
        missing_completeness: CollectionCompleteness::Complete,
        contributors: Vec::new(),
        contributors_total: 0,
        contributors_completeness: CollectionCompleteness::Complete,
        locations_total: 0,
        locations_completeness: CollectionCompleteness::Complete,
        evaluated_requirements: 0,
        evaluated_dependencies: 0,
        evaluated_parameters: 0,
        evaluated_commands: 1,
        evaluated_resources: 0,
        scope: CapabilityScope {
            kind: CapabilityScopeKind::Page,
            page: Some(0),
            command: None,
            resource: None,
        },
        location: None,
        rejection_code: None,
    }
}

fn render_plan_manifest() -> RenderPlanManifest {
    let region = SurfaceRegion {
        page_index: 0,
        x: 0,
        y: 0,
        width: 8,
        height: 8,
        coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
    };
    RenderPlanManifest {
        plan_schema_version: 1,
        document_revision: 1,
        render_config: RenderConfigHash::new([1; 32]),
        renderer_epoch: RendererEpoch::new(1),
        plan_id: RenderPlanId::new(GENERATION),
        generation: GENERATION,
        scene_hash: SceneHash::new([2; 32]),
        decision_hash: CapabilityDecisionHash::new([3; 32]),
        geometry_hash: GeometryHash::new([4; 32]),
        viewport_clip: region.clone(),
        zoom_numerator: 1,
        zoom_denominator: 1,
        device_scale_milli: 1_000,
        rotation: PageRotation::Degrees0,
        optional_content: 0,
        annotation_revision: 0,
        backend: NativeBackend::ReferenceCpu,
        output_profile: OutputProfile::Srgb,
        quality: QualityPolicy::Full,
        regions: vec![region],
        tile_content_hashes: vec![TileContentHash::new([5; 32])],
    }
}

fn local_memory_handshake() -> pdf_rs_protocol::CompatibleHandshake {
    let hello = |endpoint_role| ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role,
        capabilities: EndpointCapabilities {
            supported: ENDPOINT_CAPABILITY_LOCAL_MEMORY,
            mandatory: 0,
        },
        max_message_bytes: 1_048_576,
        max_transfer_slots: 8,
    };
    validator()
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .unwrap()
}
