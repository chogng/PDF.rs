#![cfg(unix)]

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::{fs::File, io::Read};

use pdf_rs_desktop::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
    DesktopEpochManager, DesktopIpcLimitConfig, DesktopIpcLimits, HostRangeBridge,
    HostSourceSnapshot, ReadOnlySharedRegion, receive_capability_fds, send_capability_fds,
    validate_engine_hello_event, validate_read_only_fd,
};
use pdf_rs_protocol::{
    ByteRange, CloseSessionCommand, Command, CommandEnvelope, Correlation, DataAttachmentRole,
    DataSegment, DataTicket, DesktopFrameDecoder, EndpointCapabilities, EndpointRole,
    EngineExecutionCapabilities, EngineHelloEvent, EnvelopeHeader, Event, EventEnvelope,
    GetPageMetricsCommand, HelloAcceptCommand, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES,
    MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, MESSAGE_ID_CANCEL, MESSAGE_ID_CLOSE_SESSION,
    MESSAGE_ID_FAIL_DATA, MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_OPEN, MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_RELEASE_SURFACE, MESSAGE_ID_SET_VIEWPORT,
    MESSAGE_ID_SHUTDOWN, OpenCommand, OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PageCoordinateSpace, PageGeometry, PageRotation, PageViewport, PayloadCodecLimits,
    ProtocolHello, ProtocolLimits, ProtocolValidator, ProvideDataCommand, QualityPolicy,
    ReleaseSurfaceCommand, RequestId, SCHEMA_HASH, SequenceTracker, SessionId, SetViewportCommand,
    ShutdownCommand, SourceDescriptor, SourceIdentity, ViewportRequest, WorkerId,
    encode_cancel_command_payload, encode_close_session_command_payload, encode_command_payload,
    encode_correlation_payload, encode_event_payload, encode_fail_data_command_payload,
    encode_get_page_metrics_command_payload, encode_hello_accept_command_payload,
    encode_hello_command_payload, encode_open_command_payload, encode_provide_data_command_payload,
    encode_release_surface_command_payload, encode_set_viewport_command_payload,
    encode_shutdown_command_payload,
};
use pdf_rs_surface::WorkerEpoch;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

fn limits() -> DesktopIpcLimits {
    DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("default limits")
}

fn worker_path() -> &'static str {
    env!("CARGO_BIN_EXE_pdf-rs-desktop-worker")
}

fn hello_frame(sequence: u64, worker: u64) -> Vec<u8> {
    let payload_len = 54_u32;
    let envelope = CommandEnvelope {
        header: EnvelopeHeader {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            message_type: 1,
            flags: 0,
            payload_len,
            sequence,
        },
        correlation: Correlation {
            worker: WorkerId::new(worker),
            session: None,
            request: None,
            generation: None,
        },
        command: Command::Hello(HelloCommand {
            hello: ProtocolHello {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                schema_hash: SCHEMA_HASH,
                endpoint_role: EndpointRole::Host,
                capabilities: EndpointCapabilities {
                    supported: KNOWN_ENDPOINT_CAPABILITIES,
                    mandatory: 0,
                },
                max_message_bytes: MAX_MESSAGE_BYTES,
                max_transfer_slots: MAX_TRANSFER_SLOTS,
            },
        }),
    };
    let (message_type, payload) =
        encode_command_payload(&envelope, PayloadCodecLimits::protocol_default())
            .expect("generated Hello payload");
    assert_eq!(
        payload.len(),
        usize::try_from(payload_len).expect("constant")
    );
    let mut frame = Vec::new();
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn engine_hello_frame(sequence: u64, worker: u64) -> Vec<u8> {
    let payload_len = 62_u32;
    let envelope = EventEnvelope {
        header: EnvelopeHeader {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            message_type: 114,
            flags: 0,
            payload_len,
            sequence,
        },
        correlation: Correlation {
            worker: WorkerId::new(worker),
            session: None,
            request: None,
            generation: None,
        },
        event: Event::EngineHello(EngineHelloEvent {
            hello: ProtocolHello {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                schema_hash: SCHEMA_HASH,
                endpoint_role: EndpointRole::Engine,
                capabilities: EndpointCapabilities {
                    supported: KNOWN_ENDPOINT_CAPABILITIES,
                    mandatory: 0,
                },
                max_message_bytes: MAX_MESSAGE_BYTES,
                max_transfer_slots: MAX_TRANSFER_SLOTS,
            },
            execution_capabilities: EngineExecutionCapabilities { supported: 0 },
        }),
    };
    let (message_type, payload) =
        encode_event_payload(&envelope, PayloadCodecLimits::protocol_default())
            .expect("generated EngineHello payload");
    let mut frame = Vec::new();
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn command_frame(sequence: u64, worker: WorkerId, command: Command) -> Vec<u8> {
    correlated_command_frame(
        sequence,
        Correlation {
            worker,
            session: None,
            request: None,
            generation: None,
        },
        command,
    )
}

fn correlated_command_frame(sequence: u64, correlation: Correlation, command: Command) -> Vec<u8> {
    let message_type = match &command {
        Command::Hello(_) => MESSAGE_ID_HELLO,
        Command::HelloAccept(_) => MESSAGE_ID_HELLO_ACCEPT,
        Command::Open(_) => MESSAGE_ID_OPEN,
        Command::ProvideData(_) => MESSAGE_ID_PROVIDE_DATA,
        Command::SetViewport(_) => MESSAGE_ID_SET_VIEWPORT,
        Command::Cancel(_) => MESSAGE_ID_CANCEL,
        Command::ReleaseSurface(_) => MESSAGE_ID_RELEASE_SURFACE,
        Command::CloseSession(_) => MESSAGE_ID_CLOSE_SESSION,
        Command::Shutdown(_) => MESSAGE_ID_SHUTDOWN,
        Command::FailData(_) => MESSAGE_ID_FAIL_DATA,
        Command::GetPageMetrics(_) => MESSAGE_ID_GET_PAGE_METRICS,
    };
    let mut payload =
        encode_correlation_payload(&correlation, PayloadCodecLimits::protocol_default())
            .expect("correlation payload");
    let command_payload = match &command {
        Command::Hello(value) => {
            encode_hello_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::HelloAccept(value) => {
            encode_hello_accept_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::Open(value) => {
            encode_open_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::ProvideData(value) => {
            encode_provide_data_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::SetViewport(value) => {
            encode_set_viewport_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::Cancel(value) => {
            encode_cancel_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::ReleaseSurface(value) => {
            encode_release_surface_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::CloseSession(value) => {
            encode_close_session_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::Shutdown(value) => {
            encode_shutdown_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::FailData(value) => {
            encode_fail_data_command_payload(value, PayloadCodecLimits::protocol_default())
        }
        Command::GetPageMetrics(value) => {
            encode_get_page_metrics_command_payload(value, PayloadCodecLimits::protocol_default())
        }
    }
    .expect("command value payload");
    payload.extend_from_slice(&command_payload);
    let mut frame = Vec::new();
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("payload length")
            .to_le_bytes(),
    );
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn send_command(
    host: &mut pdf_rs_desktop::DesktopHostProcess,
    sequence: u64,
    correlation: Correlation,
    command: Command,
    capabilities: Vec<DesktopCapability>,
    fds: &[OwnedFd],
    limits: DesktopIpcLimits,
) {
    let record = host
        .new_host_record(
            sequence,
            correlated_command_frame(sequence, correlation, command),
            capabilities,
            limits,
        )
        .expect("authenticated command record");
    host.send(&record, fds, limits)
        .expect("send canonical command");
}

fn receive_handshake_event(
    host: &mut pdf_rs_desktop::DesktopHostProcess,
    limits: DesktopIpcLimits,
) -> EventEnvelope {
    let mut pending = host.receive(limits).expect("receive handshake event");
    let event = pending
        .decode_handshake_event()
        .expect("decode handshake event");
    pending.commit().expect("commit handshake event");
    event
}

fn receive_event(
    host: &mut pdf_rs_desktop::DesktopHostProcess,
    decoder: DesktopFrameDecoder,
    limits: DesktopIpcLimits,
) -> (EventEnvelope, Vec<OwnedFd>) {
    let mut pending = host.receive(limits).expect("receive negotiated event");
    let event = pending
        .decode_event(decoder)
        .expect("decode negotiated event");
    let (_, fds) = pending.commit().expect("commit negotiated event");
    (event, fds)
}

fn negotiate(
    host: &mut pdf_rs_desktop::DesktopHostProcess,
    limits: DesktopIpcLimits,
) -> pdf_rs_protocol::CompatibleHandshake {
    let worker = host.worker_id();
    let hello = ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role: EndpointRole::Host,
        capabilities: EndpointCapabilities {
            supported: KNOWN_ENDPOINT_CAPABILITIES,
            mandatory: 0,
        },
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    send_command(
        host,
        1,
        Correlation {
            worker,
            session: None,
            request: None,
            generation: None,
        },
        Command::Hello(HelloCommand {
            hello: hello.clone(),
        }),
        Vec::new(),
        &[],
        limits,
    );
    let engine_hello = receive_handshake_event(host, limits);
    let Event::EngineHello(engine_hello) = engine_hello.event else {
        panic!("expected EngineHello");
    };
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello, &engine_hello.hello)
        .expect("shared-memory handshake");
    send_command(
        host,
        2,
        Correlation {
            worker,
            session: None,
            request: None,
            generation: None,
        },
        Command::HelloAccept(HelloAcceptCommand {
            negotiated_minor: handshake.minor(),
            schema_hash: SCHEMA_HASH,
        }),
        Vec::new(),
        &[],
        limits,
    );
    let ready = receive_handshake_event(host, limits);
    assert!(matches!(ready.event, Event::Ready(_)));
    handshake
}

fn viewport(geometry: PageGeometry) -> SetViewportCommand {
    SetViewportCommand {
        viewport: ViewportRequest {
            generation: 1,
            document_revision: 1,
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

#[test]
fn generated_engine_hello_event_has_directional_host_validator() {
    let worker = WorkerId::new(77);
    let frame = engine_hello_frame(1, worker.value());
    let mut sequence = SequenceTracker::new();
    validate_engine_hello_event(&frame, 0, worker, &mut sequence).expect("legal EngineHello");
    let mut malformed = frame;
    malformed.truncate(20);
    assert!(
        validate_engine_hello_event(&malformed, 0, worker, &mut SequenceTracker::new()).is_err()
    );
}

#[test]
fn real_child_authenticates_and_shuts_down_cleanly() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let record = host
        .new_host_record(
            1,
            hello_frame(1, host.worker_id().value()),
            Vec::new(),
            limits,
        )
        .expect("authenticated host record");
    host.send(&record, &[], limits).expect("send record");
    host.shutdown();
}

#[test]
fn real_child_completes_generated_shared_memory_handshake() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let worker = host.worker_id();
    let hello = ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role: EndpointRole::Host,
        capabilities: EndpointCapabilities {
            supported: KNOWN_ENDPOINT_CAPABILITIES,
            mandatory: 0,
        },
        max_message_bytes: MAX_MESSAGE_BYTES,
        max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    let hello_record = host
        .new_host_record(
            1,
            command_frame(
                1,
                worker,
                Command::Hello(HelloCommand {
                    hello: hello.clone(),
                }),
            ),
            Vec::new(),
            limits,
        )
        .expect("hello record");
    host.send(&hello_record, &[], limits).expect("send Hello");
    let mut engine_hello = host.receive(limits).expect("receive EngineHello");
    let engine_hello_event = engine_hello
        .decode_handshake_event()
        .expect("decode EngineHello");
    let Event::EngineHello(engine_hello_value) = engine_hello_event.event else {
        panic!("expected EngineHello");
    };
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello, &engine_hello_value.hello)
        .expect("shared-memory handshake");
    engine_hello.commit().expect("commit EngineHello");

    let accept_record = host
        .new_host_record(
            2,
            command_frame(
                2,
                worker,
                Command::HelloAccept(HelloAcceptCommand {
                    negotiated_minor: handshake.minor(),
                    schema_hash: SCHEMA_HASH,
                }),
            ),
            Vec::new(),
            limits,
        )
        .expect("accept record");
    host.send(&accept_record, &[], limits)
        .expect("send HelloAccept");
    let mut ready_record = host.receive(limits).expect("receive Ready");
    let ready = ready_record.decode_handshake_event().expect("decode Ready");
    assert!(matches!(ready.event, Event::Ready(_)));
    ready_record.commit().expect("commit Ready");
    host.shutdown();
}

#[test]
fn real_child_runs_native_source_surface_release_and_shutdown_flow() {
    const SOURCE_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";

    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let worker = host.worker_id();
    let epoch = host.worker_epoch();
    let handshake = negotiate(&mut host, limits);
    let decoder = DesktopFrameDecoder::for_handshake(handshake);
    let source = SourceDescriptor {
        identity: SourceIdentity {
            stable_id: [7; 32],
            revision: 1,
        },
        length: Some(u64::try_from(SOURCE_BYTES.len()).expect("source length")),
        validator: [9; 32],
    };
    send_command(
        &mut host,
        3,
        Correlation {
            worker,
            session: None,
            request: Some(RequestId::new(1)),
            generation: None,
        },
        Command::Open(OpenCommand {
            source: source.clone(),
        }),
        Vec::new(),
        &[],
        limits,
    );
    let (need, need_fds) = receive_event(&mut host, decoder, limits);
    assert!(need_fds.is_empty());
    let (session, need) = match (need.correlation.session, need.event) {
        (Some(session), Event::NeedData(need)) => (session, need),
        other => panic!("expected NeedData, got {other:?}"),
    };

    let snapshot = HostSourceSnapshot::new(source.clone(), Arc::from(SOURCE_BYTES), limits)
        .expect("Host snapshot");
    let mut bridge = HostRangeBridge::new(snapshot, limits);
    bridge
        .register(need.ticket, session, epoch, need.ranges.clone())
        .expect("register exact NeedData ranges");
    let mut capability_table = DesktopCapabilityTable::new(limits);
    let segments = bridge
        .provide(need.ticket, &mut capability_table)
        .expect("grant immutable ranges");
    let mut descriptors = Vec::new();
    let mut source_fds = Vec::new();
    let mut wire_segments = Vec::new();
    for (slot, segment) in segments.iter().enumerate() {
        let region =
            ReadOnlySharedRegion::from_bytes(segment.bytes(), limits).expect("source segment shm");
        assert_eq!(region.byte_length(), segment.capability.byte_length());
        descriptors.push(segment.capability);
        source_fds.push(region.into_fd());
        wire_segments.push(DataSegment {
            range: segment.range.clone(),
            slot: u16::try_from(slot).expect("bounded slot"),
            byte_length: segment.capability.byte_length(),
            role: DataAttachmentRole::ImmutableRangeBytes,
        });
    }
    send_command(
        &mut host,
        4,
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: None,
        },
        Command::ProvideData(ProvideDataCommand {
            ticket: need.ticket,
            source: source.identity.clone(),
            segments: wire_segments,
        }),
        descriptors.clone(),
        &source_fds,
        limits,
    );
    for descriptor in descriptors {
        capability_table
            .release(descriptor.id())
            .expect("release sent Host segment");
    }
    assert_eq!(capability_table.live_count(), 0);
    let (ready, ready_fds) = receive_event(&mut host, decoder, limits);
    assert!(ready_fds.is_empty());
    assert!(matches!(ready.event, Event::DocumentReady(_)));

    send_command(
        &mut host,
        5,
        Correlation {
            worker,
            session: Some(session),
            request: Some(RequestId::new(2)),
            generation: None,
        },
        Command::GetPageMetrics(GetPageMetricsCommand {
            document_revision: 1,
            start_index: 0,
            max_count: 1,
        }),
        Vec::new(),
        &[],
        limits,
    );
    let (metrics, metrics_fds) = receive_event(&mut host, decoder, limits);
    assert!(metrics_fds.is_empty());
    let geometry = match metrics.event {
        Event::PageMetrics(metrics) => metrics.pages[0].geometry.clone(),
        other => panic!("expected PageMetrics, got {other:?}"),
    };

    send_command(
        &mut host,
        6,
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: Some(1),
        },
        Command::SetViewport(viewport(geometry)),
        Vec::new(),
        &[],
        limits,
    );
    let mut surface = None;
    for _ in 0..8 {
        let (event, fds) = receive_event(&mut host, decoder, limits);
        match event.event {
            Event::SurfaceReady(ready) => {
                assert_eq!(fds.len(), 1);
                surface = Some((ready, fds.into_iter().next().expect("surface FD")));
                break;
            }
            Event::CapabilityReported(_) | Event::GenerationPlanned(_) => {
                assert!(fds.is_empty());
            }
            other => panic!("unexpected pre-Surface event: {other:?}"),
        }
    }
    let (surface, surface_fd) = surface.expect("Native SurfaceReady");
    let mut pixels = Vec::new();
    File::from(surface_fd)
        .read_to_end(&mut pixels)
        .expect("read shared Surface");
    assert_eq!(
        u64::try_from(pixels.len()).expect("surface length"),
        match surface.transport {
            pdf_rs_protocol::SurfaceTransport::SharedMemory { region_length, .. } => region_length,
            other => panic!("expected shared memory transport, got {other:?}"),
        }
    );
    assert!(pixels.iter().any(|byte| *byte != 0));

    send_command(
        &mut host,
        7,
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: None,
        },
        Command::ReleaseSurface(ReleaseSurfaceCommand {
            surface: surface.metadata.id,
            lease_token: surface.metadata.lease_token,
        }),
        Vec::new(),
        &[],
        limits,
    );
    let mut released = false;
    for _ in 0..8 {
        let (event, fds) = receive_event(&mut host, decoder, limits);
        assert!(fds.is_empty());
        match event.event {
            Event::SurfaceReleaseAcknowledged(ack) => {
                assert_eq!(ack.surface, surface.metadata.id);
                released = true;
                break;
            }
            Event::GenerationCompleted(_) | Event::SurfaceReclaimed(_) => {}
            other => panic!("unexpected release event: {other:?}"),
        }
    }
    assert!(released);

    send_command(
        &mut host,
        8,
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: None,
        },
        Command::CloseSession(CloseSessionCommand {}),
        Vec::new(),
        &[],
        limits,
    );
    let mut closed = false;
    for _ in 0..8 {
        let (event, fds) = receive_event(&mut host, decoder, limits);
        assert!(fds.is_empty());
        match event.event {
            Event::SessionClosed(_) => {
                closed = true;
                break;
            }
            Event::CloseSessionAcknowledged(_)
            | Event::GenerationCompleted(_)
            | Event::SurfaceReclaimed(_) => {}
            other => panic!("unexpected close event: {other:?}"),
        }
    }
    assert!(closed);

    send_command(
        &mut host,
        9,
        Correlation {
            worker,
            session: None,
            request: None,
            generation: None,
        },
        Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
        Vec::new(),
        &[],
        limits,
    );
    let mut stopped = false;
    for _ in 0..8 {
        let (event, fds) = receive_event(&mut host, decoder, limits);
        assert!(fds.is_empty());
        match event.event {
            Event::WorkerStopped(_) => {
                stopped = true;
                break;
            }
            Event::ShutdownAcknowledged(_) => {}
            other => panic!("unexpected shutdown event: {other:?}"),
        }
    }
    assert!(stopped);
    host.shutdown();
}

#[test]
fn malformed_canonical_payload_causes_child_disconnect_without_echo() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let record = host
        .new_host_record(1, vec![1], Vec::new(), limits)
        .expect("authenticated envelope");
    host.send(&record, &[], limits).expect("transport send");
    assert!(host.receive(limits).is_err());
    host.shutdown();
}

#[test]
fn structurally_valid_header_with_empty_hello_payload_is_rejected() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let mut frame = hello_frame(1, host.worker_id().value());
    frame.truncate(20);
    let record = host
        .new_host_record(1, frame, Vec::new(), limits)
        .expect("outer envelope");
    host.send(&record, &[], limits).expect("transport send");
    assert!(host.receive(limits).is_err());
    host.shutdown();
}

#[test]
fn read_only_shared_descriptor_preserves_extent() {
    let limits = limits();
    let epoch = WorkerEpoch::new(8).expect("nonzero epoch");
    let region = ReadOnlySharedRegion::from_bytes(b"immutable", limits).expect("shared region");
    let descriptor = DesktopCapability::new(
        1,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        epoch,
        region.byte_length(),
    )
    .expect("descriptor");
    let fd: OwnedFd = region.into_fd();
    validate_read_only_fd(&fd, descriptor.byte_length()).expect("descriptor remains read-only");
}

#[test]
fn kernel_scm_rights_roundtrip_preserves_read_only_cloexec_descriptor() {
    let limits = limits();
    let region = ReadOnlySharedRegion::from_bytes(b"immutable", limits).expect("shared region");
    let fd = region.into_fd();
    let (sender, receiver) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::empty(),
        None,
    )
    .expect("socketpair");
    send_capability_fds(&sender, &[fd], limits).expect("send rights");
    let received = receive_capability_fds(&receiver, limits).expect("receive rights");
    assert_eq!(received.len(), 1);
    validate_read_only_fd(&received[0], 9).expect("read-only exact extent");
    assert!(
        rustix::io::fcntl_getfd(&received[0])
            .expect("fd flags")
            .contains(rustix::io::FdFlags::CLOEXEC)
    );
}

#[test]
fn clean_peer_close_is_a_disconnect_not_a_malformed_marker() {
    let (sender, receiver) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::empty(),
        None,
    )
    .expect("socketpair");
    drop(sender);
    let failure = receive_capability_fds(&receiver, limits()).expect_err("peer closed");
    assert_eq!(
        failure.code(),
        pdf_rs_desktop::DesktopIpcErrorCode::Disconnected
    );
}

#[test]
fn read_only_shared_region_rejects_wrong_extent() {
    let region = ReadOnlySharedRegion::from_bytes(b"immutable", limits()).expect("shared region");
    let fd = region.into_fd();
    validate_read_only_fd(&fd, 9).expect("correct extent");
    assert!(validate_read_only_fd(&fd, 8).is_err());
}

#[test]
fn restart_rejects_a_late_record_from_the_old_launch() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut old = epochs.spawn(worker_path()).expect("old child");
    let late = old
        .new_host_record(
            1,
            hello_frame(1, old.worker_id().value()),
            Vec::new(),
            limits,
        )
        .expect("old launch record");
    old.shutdown();

    let mut replacement = epochs.spawn(worker_path()).expect("replacement child");
    assert!(replacement.worker_epoch().value() > old.worker_epoch().value());
    assert!(replacement.send(&late, &[], limits).is_err());
    replacement.shutdown();
}

#[test]
fn dropped_uncommitted_engine_hello_poison_closes_process() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("spawn child");
    let record = host
        .new_host_record(
            1,
            hello_frame(1, host.worker_id().value()),
            Vec::new(),
            limits,
        )
        .expect("record");
    host.send(&record, &[], limits)
        .expect("send legal handshake");
    let pending = host.receive(limits).expect("pending EngineHello");
    drop(pending);
    assert!(
        host.new_host_record(
            2,
            hello_frame(2, host.worker_epoch().value()),
            Vec::new(),
            limits
        )
        .is_ok()
    );
    assert!(host.send(&record, &[], limits).is_err());
}

#[test]
fn host_records_reject_foreign_class_epoch_and_aggregate_bytes() {
    let limits = limits();
    let mut epochs = DesktopEpochManager::new();
    let mut host = epochs.spawn(worker_path()).expect("child");
    let epoch = host.worker_epoch();
    let foreign_class = DesktopCapability::new(
        1,
        CapabilityClass::SurfaceRegion,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        epoch,
        1,
    )
    .expect("descriptor");
    assert!(
        host.new_host_record(
            1,
            hello_frame(1, epoch.value()),
            vec![foreign_class],
            limits
        )
        .is_err()
    );
    let foreign_epoch = DesktopCapability::new(
        2,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        WorkerEpoch::new(11).expect("epoch"),
        1,
    )
    .expect("descriptor");
    assert!(
        host.new_host_record(
            1,
            hello_frame(1, epoch.value()),
            vec![foreign_epoch],
            limits
        )
        .is_err()
    );
    let tight = DesktopIpcLimits::new(DesktopIpcLimitConfig {
        max_capability_bytes: 8,
        ..DesktopIpcLimitConfig::default()
    })
    .expect("tight limits");
    let over = DesktopCapability::new(
        3,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        epoch,
        9,
    )
    .expect("descriptor");
    assert!(
        host.new_host_record(1, hello_frame(1, epoch.value()), vec![over], tight)
            .is_err()
    );
    host.shutdown();
}

#[test]
fn source_over_cap_keeps_ticket_and_ledger_without_copying_snapshot() {
    let bytes: Arc<[u8]> = Arc::from(&b"abcdefgh"[..]);
    let snapshot = HostSourceSnapshot::new(
        SourceDescriptor {
            identity: SourceIdentity {
                stable_id: [1; 32],
                revision: 1,
            },
            length: Some(8),
            validator: [2; 32],
        },
        Arc::clone(&bytes),
        limits(),
    )
    .expect("snapshot");
    let epoch = WorkerEpoch::new(12).expect("epoch");
    let ticket = DataTicket::new(1);
    let mut bridge = HostRangeBridge::new(snapshot, limits());
    bridge
        .register(
            ticket,
            SessionId::new(1),
            epoch,
            vec![
                ByteRange { start: 0, len: 4 },
                ByteRange { start: 4, len: 4 },
            ],
        )
        .expect("register exact ranges");
    assert!(
        bridge
            .register(
                DataTicket::new(2),
                SessionId::new(1),
                epoch,
                vec![ByteRange { start: 0, len: 1 }]
            )
            .is_err()
    );
    assert_eq!(bridge.outstanding(), 1);
    let one_slot = DesktopIpcLimits::new(DesktopIpcLimitConfig {
        max_capabilities: 1,
        ..DesktopIpcLimitConfig::default()
    })
    .expect("one slot");
    let mut rejected = DesktopCapabilityTable::new(one_slot);
    assert!(bridge.provide(ticket, &mut rejected).is_err());
    assert_eq!(bridge.outstanding(), 1);
    assert_eq!(rejected.live_count(), 0);
    let mut accepted = DesktopCapabilityTable::new(limits());
    let segments = bridge
        .provide(ticket, &mut accepted)
        .expect("retry retained ticket");
    assert_eq!(segments[0].bytes().as_ptr(), bytes.as_ptr());
    assert_eq!(segments[1].bytes().as_ptr(), bytes.as_ptr().wrapping_add(4));
}
