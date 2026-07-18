use pdf_rs_browser_worker::{
    BrowserNativeWorkerEvent, NativeBrowserWorker, NativeBrowserWorkerError,
    NativeBrowserWorkerPhase,
};
use pdf_rs_engine::NativeWorkerLimitConfig;
use pdf_rs_protocol::{
    ByteRange, CancelCommand, CapabilityProfileId, CloseSessionCommand, Command, CommandEnvelope,
    Correlation, DataAttachmentRole, DataSegment, ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
    EndpointCapabilities, EndpointRole, Event, FailDataCommand, GetPageMetricsCommand,
    HelloAcceptCommand, HelloCommand, MAX_DATA_TICKET_BYTES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS,
    MESSAGE_ID_CANCEL, MESSAGE_ID_CLOSE_SESSION, MESSAGE_ID_FAIL_DATA, MESSAGE_ID_GET_PAGE_METRICS,
    MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT, MESSAGE_ID_OPEN, MESSAGE_ID_PROVIDE_DATA,
    MESSAGE_ID_RELEASE_SURFACE, MESSAGE_ID_SET_VIEWPORT, MESSAGE_ID_SHUTDOWN, OpenCommand,
    OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR, PageCoordinateSpace, PageGeometry, PageRotation,
    PageViewport, ProtocolHello, ProvideDataCommand, QualityPolicy, ReleaseSurfaceCommand,
    RequestId, SCHEMA_HASH, SetViewportCommand, ShutdownCommand, SourceDescriptor,
    SourceFailureCode, SourceIdentity, SurfaceMetadata, ViewportRequest, WorkerId,
};
use pdf_rs_surface::WorkerEpoch;

const WORKER_VALUE: u64 = 41;
const SOURCE_REVISION: u64 = 7;
const DOCUMENT_REVISION: u64 = 1;

fn worker() -> NativeBrowserWorker {
    worker_with_limits(NativeWorkerLimitConfig::default())
}

fn worker_with_limits(limits: NativeWorkerLimitConfig) -> NativeBrowserWorker {
    NativeBrowserWorker::new(
        WorkerId::new(WORKER_VALUE),
        WorkerEpoch::new(3).unwrap(),
        5,
        limits,
    )
    .unwrap()
}

fn worker_with_surface_copy_budget(budget: u64) -> NativeBrowserWorker {
    NativeBrowserWorker::new_with_browser_surface_copy_budget(
        WorkerId::new(WORKER_VALUE),
        WorkerEpoch::new(3).unwrap(),
        5,
        NativeWorkerLimitConfig::default(),
        budget,
    )
    .unwrap()
}

fn correlation(
    session: Option<pdf_rs_protocol::SessionId>,
    request: Option<RequestId>,
    generation: Option<u64>,
) -> Correlation {
    Correlation {
        worker: WorkerId::new(WORKER_VALUE),
        session,
        request,
        generation,
    }
}

fn command(message_type: u16, correlation: Correlation, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        header: pdf_rs_protocol::EnvelopeHeader {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            message_type,
            flags: 0,
            payload_len: 0,
            sequence: 1,
        },
        correlation,
        command,
    }
}

fn negotiate(worker: &mut NativeBrowserWorker) {
    let host_hello = ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role: EndpointRole::Host,
        capabilities: EndpointCapabilities {
            supported: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
            mandatory: ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
        },
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    worker
        .handle_command(
            command(
                MESSAGE_ID_HELLO,
                correlation(None, None, None),
                Command::Hello(HelloCommand { hello: host_hello }),
            ),
            &[],
        )
        .unwrap();
    assert!(matches!(
        worker.next_event().unwrap(),
        Some(BrowserNativeWorkerEvent { .. })
    ));
    worker
        .handle_command(
            command(
                MESSAGE_ID_HELLO_ACCEPT,
                correlation(None, None, None),
                Command::HelloAccept(HelloAcceptCommand {
                    negotiated_minor: PROTOCOL_MINOR,
                    schema_hash: SCHEMA_HASH,
                }),
            ),
            &[],
        )
        .unwrap();
    let ready = worker.next_event().unwrap().unwrap();
    assert!(matches!(
        ready.event(),
        Event::Ready(event)
            if event.capability_profiles == vec![CapabilityProfileId::BaselineNative]
                && event.output_profiles == vec![OutputProfile::Srgb]
    ));
    assert_eq!(worker.phase(), NativeBrowserWorkerPhase::Ready);
}

fn source(bytes: &[u8]) -> SourceDescriptor {
    SourceDescriptor {
        identity: SourceIdentity {
            stable_id: [7; 32],
            revision: SOURCE_REVISION,
        },
        length: Some(u64::try_from(bytes.len()).unwrap()),
        validator: [9; 32],
    }
}

fn open_to_need_data(
    worker: &mut NativeBrowserWorker,
    bytes: &[u8],
    request: u64,
) -> (pdf_rs_protocol::SessionId, pdf_rs_protocol::NeedDataEvent) {
    worker
        .handle_command(
            command(
                MESSAGE_ID_OPEN,
                correlation(None, Some(RequestId::new(request)), None),
                Command::Open(OpenCommand {
                    source: source(bytes),
                }),
            ),
            &[],
        )
        .unwrap();
    let need = worker.next_event().unwrap().unwrap();
    match (need.correlation(), need.event()) {
        (
            Correlation {
                session: Some(session),
                request: Some(actual),
                ..
            },
            Event::NeedData(event),
        ) => {
            assert_eq!(*actual, RequestId::new(request));
            (*session, event.clone())
        }
        other => panic!("expected NeedData, got {other:?}"),
    }
}

fn provide_data(
    worker: &mut NativeBrowserWorker,
    bytes: &[u8],
    session: pdf_rs_protocol::SessionId,
    need: &pdf_rs_protocol::NeedDataEvent,
) {
    let mut segments = Vec::new();
    let mut transfers = Vec::new();
    for (index, range) in need.ranges.iter().enumerate() {
        let start = usize::try_from(range.start).unwrap();
        let end = usize::try_from(range.start + range.len).unwrap();
        segments.push(DataSegment {
            range: range.clone(),
            slot: u16::try_from(index).unwrap(),
            byte_length: range.len,
            role: DataAttachmentRole::ImmutableRangeBytes,
        });
        transfers.push(bytes[start..end].to_vec());
    }
    worker
        .handle_command(
            command(
                MESSAGE_ID_PROVIDE_DATA,
                correlation(Some(session), None, None),
                Command::ProvideData(ProvideDataCommand {
                    ticket: need.ticket,
                    source: need.source.clone(),
                    segments,
                }),
            ),
            &transfers,
        )
        .unwrap();
}

fn document_ready(worker: &mut NativeBrowserWorker) -> pdf_rs_protocol::SessionId {
    let ready = next_until(worker, |event| matches!(event, Event::DocumentReady(_)));
    match ready.event() {
        Event::DocumentReady(event) => {
            assert_eq!(event.document_revision, DOCUMENT_REVISION);
            assert_eq!(event.page_count, 1);
            event.session
        }
        other => panic!("expected DocumentReady, got {other:?}"),
    }
}

fn page_geometry(
    worker: &mut NativeBrowserWorker,
    session: pdf_rs_protocol::SessionId,
) -> PageGeometry {
    worker
        .handle_command(
            command(
                MESSAGE_ID_GET_PAGE_METRICS,
                correlation(Some(session), Some(RequestId::new(100)), None),
                Command::GetPageMetrics(GetPageMetricsCommand {
                    document_revision: DOCUMENT_REVISION,
                    start_index: 0,
                    max_count: 1,
                }),
            ),
            &[],
        )
        .unwrap();
    let metrics = next_until(worker, |event| matches!(event, Event::PageMetrics(_)));
    match metrics.event() {
        Event::PageMetrics(event) => {
            assert_eq!(event.document_revision, DOCUMENT_REVISION);
            assert_eq!(event.total_pages, 1);
            assert_eq!(event.pages.len(), 1);
            event.pages[0].geometry.clone()
        }
        other => panic!("expected PageMetrics, got {other:?}"),
    }
}

fn viewport(generation: u64, geometry: PageGeometry) -> SetViewportCommand {
    SetViewportCommand {
        viewport: ViewportRequest {
            generation,
            document_revision: DOCUMENT_REVISION,
            annotation_revision: 1,
            zoom_numerator: 1,
            zoom_denominator: 1,
            visible_pages: vec![PageViewport {
                page_index: 0,
                coordinate_space: PageCoordinateSpace::PdfPointsBottomLeft,
                geometry,
                clip_x_milli_points: 0,
                clip_y_milli_points: 0,
                clip_width_milli_points: 16_000,
                clip_height_milli_points: 16_000,
            }],
            quality: QualityPolicy::Full,
            output_profile: OutputProfile::Srgb,
            device_scale_milli: 1_000,
            rotation: PageRotation::Degrees0,
            optional_content_id: 1,
        },
    }
}

fn next_until(
    worker: &mut NativeBrowserWorker,
    predicate: impl Fn(&Event) -> bool,
) -> BrowserNativeWorkerEvent {
    for _ in 0..64 {
        if let Some(event) = worker.next_event().unwrap()
            && predicate(event.event())
        {
            return event;
        }
    }
    panic!("expected event was not produced");
}

fn surface_metadata(event: &BrowserNativeWorkerEvent) -> SurfaceMetadata {
    match event.event() {
        Event::SurfaceReady(surface) => {
            assert_eq!(event.transfers().len(), 1);
            assert_eq!(
                u64::try_from(event.transfers()[0].len()).unwrap(),
                match surface.transport {
                    pdf_rs_protocol::SurfaceTransport::BrowserArrayBuffer {
                        buffer_length, ..
                    } => buffer_length,
                    _ => panic!("surface was not a browser ArrayBuffer"),
                }
            );
            assert!(event.transfers()[0].iter().any(|byte| *byte != 0));
            surface.metadata.clone()
        }
        other => panic!("expected SurfaceReady, got {other:?}"),
    }
}

fn exact_surface_copy_budget(buffer_length: usize, pixel_length: usize) -> u64 {
    let mut destination = Vec::<u8>::new();
    destination.try_reserve_exact(buffer_length).unwrap();
    let mut transfers = Vec::<Vec<u8>>::new();
    transfers.try_reserve_exact(1).unwrap();
    let mut imported = Vec::<u8>::new();
    imported.try_reserve_exact(pixel_length).unwrap();
    u64::try_from(destination.capacity()).unwrap()
        + u64::try_from(transfers.capacity() * std::mem::size_of::<Vec<u8>>()).unwrap()
        + u64::try_from(imported.capacity()).unwrap()
}

#[test]
fn self_authored_fixture_completes_native_browser_worker_lifecycle() {
    let bytes = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";
    let mut worker = worker();
    negotiate(&mut worker);
    let (session, need) = open_to_need_data(&mut worker, bytes, 1);
    provide_data(&mut worker, bytes, session, &need);
    assert_eq!(document_ready(&mut worker), session);
    let geometry = page_geometry(&mut worker, session);

    worker
        .handle_command(
            command(
                MESSAGE_ID_SET_VIEWPORT,
                correlation(Some(session), None, Some(1)),
                Command::SetViewport(viewport(1, geometry)),
            ),
            &[],
        )
        .unwrap();
    let capability = next_until(&mut worker, |event| {
        matches!(event, Event::CapabilityReported(_))
    });
    assert!(matches!(
        capability.event(),
        Event::CapabilityReported(event)
            if event.decision.status == pdf_rs_protocol::SupportStatus::Supported
    ));
    let surface = next_until(&mut worker, |event| matches!(event, Event::SurfaceReady(_)));
    let metadata = surface_metadata(&surface);

    worker
        .handle_command(
            command(
                MESSAGE_ID_RELEASE_SURFACE,
                correlation(Some(session), None, None),
                Command::ReleaseSurface(ReleaseSurfaceCommand {
                    surface: metadata.id,
                    lease_token: metadata.lease_token,
                }),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut worker, |event| {
        matches!(event, Event::SurfaceReleaseAcknowledged(_))
    });

    worker
        .handle_command(
            command(
                MESSAGE_ID_CLOSE_SESSION,
                correlation(Some(session), None, None),
                Command::CloseSession(CloseSessionCommand {}),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut worker, |event| {
        matches!(event, Event::SessionClosed(_))
    });

    worker
        .handle_command(
            command(
                MESSAGE_ID_SHUTDOWN,
                correlation(None, None, None),
                Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut worker, |event| {
        matches!(event, Event::WorkerStopped(_))
    });
    assert_eq!(worker.phase(), NativeBrowserWorkerPhase::Stopped);
}

#[test]
fn unsupported_fixture_never_publishes_a_surface() {
    let bytes = b"%PDF-2.0\nunsupported fixture\n";
    let mut worker = worker();
    negotiate(&mut worker);
    let (session, need) = open_to_need_data(&mut worker, bytes, 2);
    provide_data(&mut worker, bytes, session, &need);
    document_ready(&mut worker);
    let geometry = page_geometry(&mut worker, session);
    worker
        .handle_command(
            command(
                MESSAGE_ID_SET_VIEWPORT,
                correlation(Some(session), None, Some(1)),
                Command::SetViewport(viewport(1, geometry)),
            ),
            &[],
        )
        .unwrap();
    let mut saw_unsupported = false;
    for _ in 0..64 {
        let Some(event) = worker.next_event().unwrap() else {
            continue;
        };
        assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        if matches!(
            event.event(),
            Event::CapabilityReported(event)
                if event.decision.status == pdf_rs_protocol::SupportStatus::Unsupported
        ) {
            saw_unsupported = true;
        }
        if matches!(event.event(), Event::GenerationCompleted(_)) {
            break;
        }
    }
    assert!(saw_unsupported);
}

#[test]
fn rejected_budget_cancel_source_change_and_close_publish_no_surface() {
    let bytes = b"%PDF-1.7\nbounded fixture\n";

    let mut budget = worker();
    negotiate(&mut budget);
    let mut descriptor = source(bytes);
    descriptor.length = Some(MAX_DATA_TICKET_BYTES + 1);
    assert_eq!(
        budget.handle_command(
            command(
                MESSAGE_ID_OPEN,
                correlation(None, Some(RequestId::new(3)), None),
                Command::Open(OpenCommand { source: descriptor }),
            ),
            &[],
        ),
        Err(NativeBrowserWorkerError::Limit)
    );
    assert!(budget.next_event().unwrap().is_none());

    let mut cancelled = worker();
    negotiate(&mut cancelled);
    let (cancel_session, _) = open_to_need_data(&mut cancelled, bytes, 4);
    cancelled
        .handle_command(
            command(
                MESSAGE_ID_CANCEL,
                correlation(Some(cancel_session), Some(RequestId::new(4)), None),
                Command::Cancel(CancelCommand {
                    target: RequestId::new(4),
                }),
            ),
            &[],
        )
        .unwrap();
    for _ in 0..16 {
        if let Some(event) = cancelled.next_event().unwrap() {
            assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        }
    }

    let mut changed = worker();
    negotiate(&mut changed);
    let (changed_session, need) = open_to_need_data(&mut changed, bytes, 5);
    changed
        .handle_command(
            command(
                MESSAGE_ID_FAIL_DATA,
                correlation(Some(changed_session), None, None),
                Command::FailData(FailDataCommand {
                    ticket: need.ticket,
                    expected: need.source.clone(),
                    observed: Some(SourceIdentity {
                        stable_id: [8; 32],
                        revision: SOURCE_REVISION + 1,
                    }),
                    code: SourceFailureCode::SourceChanged,
                    retryable: false,
                }),
            ),
            &[],
        )
        .unwrap();
    for _ in 0..32 {
        if let Some(event) = changed.next_event().unwrap() {
            assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        }
    }

    let mut closed = worker();
    negotiate(&mut closed);
    let (closed_session, _) = open_to_need_data(&mut closed, bytes, 6);
    closed
        .handle_command(
            command(
                MESSAGE_ID_CLOSE_SESSION,
                correlation(Some(closed_session), None, None),
                Command::CloseSession(CloseSessionCommand {}),
            ),
            &[],
        )
        .unwrap();
    for _ in 0..32 {
        if let Some(event) = closed.next_event().unwrap() {
            assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        }
    }
}

#[test]
fn stale_generation_cannot_publish_after_a_newer_generation() {
    let bytes = b"%PDF-1.7\nstale fixture\n";
    let mut worker = worker();
    negotiate(&mut worker);
    let (session, need) = open_to_need_data(&mut worker, bytes, 7);
    provide_data(&mut worker, bytes, session, &need);
    document_ready(&mut worker);
    let geometry = page_geometry(&mut worker, session);
    worker
        .handle_command(
            command(
                MESSAGE_ID_SET_VIEWPORT,
                correlation(Some(session), None, Some(2)),
                Command::SetViewport(viewport(2, geometry.clone())),
            ),
            &[],
        )
        .unwrap();
    assert!(
        worker
            .handle_command(
                command(
                    MESSAGE_ID_SET_VIEWPORT,
                    correlation(Some(session), None, Some(1)),
                    Command::SetViewport(viewport(1, geometry)),
                ),
                &[],
            )
            .is_err()
    );
    for _ in 0..64 {
        if let Some(event) = worker.next_event().unwrap()
            && let Event::SurfaceReady(surface) = event.event()
        {
            assert_eq!(surface.metadata.generation, 2);
        }
    }
}

#[test]
fn provide_data_segments_are_exact_immutable_ranges() {
    let bytes = b"%PDF-1.7\nrange fixture\n";
    let mut worker = worker();
    negotiate(&mut worker);
    let (session, need) = open_to_need_data(&mut worker, bytes, 8);
    let bad = ProvideDataCommand {
        ticket: need.ticket,
        source: need.source.clone(),
        segments: vec![DataSegment {
            range: ByteRange { start: 0, len: 1 },
            slot: 0,
            byte_length: 1,
            role: DataAttachmentRole::ImmutableRangeBytes,
        }],
    };
    assert!(
        worker
            .handle_command(
                command(
                    MESSAGE_ID_PROVIDE_DATA,
                    correlation(Some(session), None, None),
                    Command::ProvideData(bad),
                ),
                &[vec![bytes[0]]],
            )
            .is_err()
    );
    for _ in 0..8 {
        if let Some(event) = worker.next_event().unwrap() {
            assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        }
    }
}

#[test]
fn complete_source_bytes_precharge_the_adapter_parse_budget() {
    let bytes = b"%PDF-1.7\nbounded source fixture\n";
    let limits = NativeWorkerLimitConfig {
        max_scene_bytes_per_open: 16,
        retained_scene_byte_capacity: 16,
        ..NativeWorkerLimitConfig::default()
    };
    let mut worker = worker_with_limits(limits);
    negotiate(&mut worker);
    let (session, need) = open_to_need_data(&mut worker, bytes, 9);
    let segment = DataSegment {
        range: need.ranges[0].clone(),
        slot: 0,
        byte_length: need.ranges[0].len,
        role: DataAttachmentRole::ImmutableRangeBytes,
    };
    assert_eq!(
        worker.handle_command(
            command(
                MESSAGE_ID_PROVIDE_DATA,
                correlation(Some(session), None, None),
                Command::ProvideData(ProvideDataCommand {
                    ticket: need.ticket,
                    source: need.source.clone(),
                    segments: vec![segment],
                }),
            ),
            &[bytes.to_vec()],
        ),
        Err(NativeBrowserWorkerError::Limit)
    );
    for _ in 0..16 {
        if let Some(event) = worker.next_event().unwrap() {
            assert!(!matches!(event.event(), Event::SurfaceReady(_)));
        }
    }
}

#[test]
fn browser_surface_copy_budget_is_exact_and_one_less_reclaims_every_resource() {
    let bytes = b"%PDF-1.7\nsurface budget fixture\n";
    let pixel_length = 16_usize * 16 * 4;
    let budget = exact_surface_copy_budget(pixel_length, pixel_length);

    let mut exact = worker_with_surface_copy_budget(budget);
    negotiate(&mut exact);
    let (session, need) = open_to_need_data(&mut exact, bytes, 10);
    provide_data(&mut exact, bytes, session, &need);
    document_ready(&mut exact);
    let geometry = page_geometry(&mut exact, session);
    exact
        .handle_command(
            command(
                MESSAGE_ID_SET_VIEWPORT,
                correlation(Some(session), None, Some(1)),
                Command::SetViewport(viewport(1, geometry)),
            ),
            &[],
        )
        .unwrap();
    let surface = next_until(&mut exact, |event| matches!(event, Event::SurfaceReady(_)));
    assert_eq!(surface.transfers()[0].len(), pixel_length);
    let metadata = surface_metadata(&surface);
    exact
        .handle_command(
            command(
                MESSAGE_ID_RELEASE_SURFACE,
                correlation(Some(session), None, None),
                Command::ReleaseSurface(ReleaseSurfaceCommand {
                    surface: metadata.id,
                    lease_token: metadata.lease_token,
                }),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut exact, |event| {
        matches!(event, Event::SurfaceReleaseAcknowledged(_))
    });

    let mut one_less = worker_with_surface_copy_budget(budget - 1);
    negotiate(&mut one_less);
    let (session, need) = open_to_need_data(&mut one_less, bytes, 11);
    provide_data(&mut one_less, bytes, session, &need);
    document_ready(&mut one_less);
    let geometry = page_geometry(&mut one_less, session);
    one_less
        .handle_command(
            command(
                MESSAGE_ID_SET_VIEWPORT,
                correlation(Some(session), None, Some(1)),
                Command::SetViewport(viewport(1, geometry)),
            ),
            &[],
        )
        .unwrap();
    let mut rejected = false;
    for _ in 0..64 {
        match one_less.next_event() {
            Err(error) => {
                assert_eq!(error, NativeBrowserWorkerError::Limit);
                rejected = true;
                break;
            }
            Ok(Some(event)) => {
                assert!(!matches!(event.event(), Event::SurfaceReady(_)));
            }
            Ok(None) => {}
        }
    }
    assert!(rejected);

    one_less
        .handle_command(
            command(
                MESSAGE_ID_CLOSE_SESSION,
                correlation(Some(session), None, None),
                Command::CloseSession(CloseSessionCommand {}),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut one_less, |event| {
        matches!(event, Event::SessionClosed(_))
    });
    one_less
        .handle_command(
            command(
                MESSAGE_ID_SHUTDOWN,
                correlation(None, None, None),
                Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
            ),
            &[],
        )
        .unwrap();
    next_until(&mut one_less, |event| {
        matches!(event, Event::WorkerStopped(_))
    });
    assert!(one_less.can_dispose());
}
