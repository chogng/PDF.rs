#![cfg(unix)]

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use pdf_rs_desktop::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
    DesktopChildSupervisor, DesktopEpochCleanup, DesktopIpcLimitConfig, DesktopIpcLimits,
    DesktopSupervisionError, DesktopSupervisorConfig, DesktopSupervisorState, DesktopWorkerFault,
    DesktopWorkerFaultKind, HostRangeBridge, HostSourceSnapshot, ReadOnlySharedRegion,
    validate_read_only_fd,
};
use pdf_rs_protocol::{
    ByteRange, Command, Correlation, DataAttachmentRole, DataSegment, DataTicket,
    DesktopFrameDecoder, EndpointCapabilities, EndpointRole, Event, GetPageMetricsCommand,
    HelloAcceptCommand, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES,
    MAX_TRANSFER_SLOTS, MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_OPEN, MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_SET_VIEWPORT, MESSAGE_ID_SHUTDOWN,
    OpenCommand, OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR, PageCoordinateSpace, PageGeometry,
    PageRotation, PageViewport, PayloadCodecLimits, ProtocolHello, ProtocolLimits,
    ProtocolValidator, ProvideDataCommand, QualityPolicy, RequestId, SCHEMA_HASH, SessionId,
    SetViewportCommand, ShutdownCommand, SourceDescriptor, SourceIdentity, ViewportRequest,
    WorkerId, encode_correlation_payload, encode_get_page_metrics_command_payload,
    encode_hello_accept_command_payload, encode_hello_command_payload, encode_open_command_payload,
    encode_provide_data_command_payload, encode_set_viewport_command_payload,
    encode_shutdown_command_payload,
};
use pdf_rs_surface::WorkerEpoch;
use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};

fn limits() -> DesktopIpcLimits {
    DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("default limits")
}

fn worker_path() -> &'static str {
    env!("CARGO_BIN_EXE_pdf-rs-desktop-worker")
}

fn command_frame(sequence: u64, correlation: Correlation, command: Command) -> Vec<u8> {
    let mut payload =
        encode_correlation_payload(&correlation, PayloadCodecLimits::protocol_default())
            .expect("correlation payload");
    let (message_type, command_payload) = match &command {
        Command::Hello(value) => (
            MESSAGE_ID_HELLO,
            encode_hello_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("Hello payload"),
        ),
        Command::HelloAccept(value) => (
            MESSAGE_ID_HELLO_ACCEPT,
            encode_hello_accept_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("HelloAccept payload"),
        ),
        Command::Shutdown(value) => (
            MESSAGE_ID_SHUTDOWN,
            encode_shutdown_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("Shutdown payload"),
        ),
        Command::Open(value) => (
            MESSAGE_ID_OPEN,
            encode_open_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("Open payload"),
        ),
        Command::ProvideData(value) => (
            MESSAGE_ID_PROVIDE_DATA,
            encode_provide_data_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("ProvideData payload"),
        ),
        Command::GetPageMetrics(value) => (
            MESSAGE_ID_GET_PAGE_METRICS,
            encode_get_page_metrics_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("GetPageMetrics payload"),
        ),
        Command::SetViewport(value) => (
            MESSAGE_ID_SET_VIEWPORT,
            encode_set_viewport_command_payload(value, PayloadCodecLimits::protocol_default())
                .expect("SetViewport payload"),
        ),
        _ => panic!("unsupported supervisor test command"),
    };
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

fn send_command<C: DesktopEpochCleanup>(
    supervisor: &mut DesktopChildSupervisor<C>,
    sequence: u64,
    command: Command,
    limits: DesktopIpcLimits,
) {
    let worker = supervisor.worker_id().expect("active worker");
    send_correlated_command(
        supervisor,
        sequence,
        Correlation {
            worker,
            session: None,
            request: None,
            generation: None,
        },
        command,
        Vec::new(),
        &[],
        limits,
    );
}

fn send_correlated_command<C: DesktopEpochCleanup>(
    supervisor: &mut DesktopChildSupervisor<C>,
    sequence: u64,
    correlation: Correlation,
    command: Command,
    capabilities: Vec<DesktopCapability>,
    fds: &[OwnedFd],
    limits: DesktopIpcLimits,
) {
    let record = supervisor
        .new_host_record(
            sequence,
            command_frame(sequence, correlation, command),
            capabilities,
            limits,
        )
        .expect("current authenticated record");
    supervisor
        .send(&record, fds, limits)
        .expect("supervised command send");
}

fn negotiate<C: DesktopEpochCleanup>(
    supervisor: &mut DesktopChildSupervisor<C>,
    limits: DesktopIpcLimits,
) -> pdf_rs_protocol::CompatibleHandshake {
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
        supervisor,
        1,
        Command::Hello(HelloCommand {
            hello: hello.clone(),
        }),
        limits,
    );
    let engine_hello = supervisor
        .receive_handshake_event(limits)
        .expect("EngineHello");
    let Event::EngineHello(engine_hello) = engine_hello.event else {
        panic!("expected EngineHello");
    };
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello, &engine_hello.hello)
        .expect("compatible desktop handshake");
    send_command(
        supervisor,
        2,
        Command::HelloAccept(HelloAcceptCommand {
            negotiated_minor: handshake.minor(),
            schema_hash: SCHEMA_HASH,
        }),
        limits,
    );
    let ready = supervisor.receive_handshake_event(limits).expect("Ready");
    assert!(matches!(ready.event, Event::Ready(_)));
    handshake
}

fn expect_worker_fault<T>(result: Result<T, DesktopSupervisionError>) -> DesktopWorkerFault {
    match result {
        Err(DesktopSupervisionError::WorkerFault(fault)) => fault,
        Err(other) => panic!("expected WorkerFault, got {other:?}"),
        Ok(_) => panic!("expected WorkerFault"),
    }
}

fn active_pid<C: DesktopEpochCleanup>(supervisor: &DesktopChildSupervisor<C>) -> Pid {
    let raw_pid = i32::try_from(supervisor.process_id().expect("active child PID"))
        .expect("PID fits platform type");
    Pid::from_raw(raw_pid).expect("nonzero child PID")
}

fn signal_active<C: DesktopEpochCleanup>(supervisor: &DesktopChildSupervisor<C>, signal: Signal) {
    let pid = active_pid(supervisor);
    kill_process(pid, signal).expect("signal child");
}

fn stop_active<C: DesktopEpochCleanup>(supervisor: &DesktopChildSupervisor<C>) {
    let pid = active_pid(supervisor);
    kill_process(pid, Signal::STOP).expect("stop child");
    let (observed, status) = waitpid(Some(pid), WaitOptions::UNTRACED)
        .expect("observe stopped child")
        .expect("stopped child status");
    assert_eq!(observed, pid);
    assert_eq!(status.stopping_signal(), Some(Signal::STOP.as_raw()));
}

struct TrackedResources {
    capabilities: DesktopCapabilityTable,
    bridge: HostRangeBridge,
    epoch_fds: Vec<OwnedFd>,
    retired: Vec<(WorkerId, WorkerEpoch)>,
}

impl DesktopEpochCleanup for TrackedResources {
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch) {
        self.capabilities.retire_epoch(worker, epoch);
        self.bridge.retire_epoch(worker, epoch);
        self.epoch_fds.clear();
        self.retired.push((worker, epoch));
    }
}

fn cleanup_resources(limits: DesktopIpcLimits) -> TrackedResources {
    let bytes: Arc<[u8]> = Arc::from(&b"supervised"[..]);
    let snapshot = HostSourceSnapshot::new(
        SourceDescriptor {
            identity: SourceIdentity {
                stable_id: [3; 32],
                revision: 1,
            },
            length: Some(10),
            validator: [4; 32],
        },
        bytes,
        limits,
    )
    .expect("source snapshot");
    TrackedResources {
        capabilities: DesktopCapabilityTable::new(limits),
        bridge: HostRangeBridge::new(snapshot, limits),
        epoch_fds: Vec::new(),
        retired: Vec::new(),
    }
}

fn open_fixture_for_render<C: DesktopEpochCleanup>(
    supervisor: &mut DesktopChildSupervisor<C>,
    decoder: DesktopFrameDecoder,
    limits: DesktopIpcLimits,
) -> (SessionId, PageGeometry) {
    const SOURCE_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n%%EOF\n";

    let worker = supervisor.worker_id().expect("active worker");
    let epoch = supervisor.worker_epoch().expect("active epoch");
    let source = SourceDescriptor {
        identity: SourceIdentity {
            stable_id: [7; 32],
            revision: 1,
        },
        length: Some(u64::try_from(SOURCE_BYTES.len()).expect("source length")),
        validator: [9; 32],
    };
    send_correlated_command(
        supervisor,
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
    let (need, need_fds) = supervisor
        .receive_event(decoder, limits)
        .expect("NeedData from real child");
    assert!(need_fds.is_empty());
    let (session, need) = match (need.correlation.session, need.event) {
        (Some(session), Event::NeedData(need)) => (session, need),
        other => panic!("expected NeedData, got {other:?}"),
    };

    let snapshot = HostSourceSnapshot::new(source.clone(), Arc::from(SOURCE_BYTES), limits)
        .expect("fixture source snapshot");
    let mut bridge = HostRangeBridge::new(snapshot, limits);
    bridge
        .register(need.ticket, session, epoch, need.ranges.clone())
        .expect("register requested source ranges");
    let mut capability_table = DesktopCapabilityTable::new(limits);
    let segments = bridge
        .provide(need.ticket, &mut capability_table)
        .expect("grant immutable source ranges");
    let mut descriptors = Vec::new();
    let mut source_fds = Vec::new();
    let mut wire_segments = Vec::new();
    for (slot, segment) in segments.iter().enumerate() {
        let region = ReadOnlySharedRegion::from_bytes(segment.bytes(), limits)
            .expect("source shared region");
        descriptors.push(segment.capability);
        source_fds.push(region.into_fd());
        wire_segments.push(DataSegment {
            range: segment.range.clone(),
            slot: u16::try_from(slot).expect("bounded transfer slot"),
            byte_length: segment.capability.byte_length(),
            role: DataAttachmentRole::ImmutableRangeBytes,
        });
    }
    send_correlated_command(
        supervisor,
        4,
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: None,
        },
        Command::ProvideData(ProvideDataCommand {
            ticket: need.ticket,
            source: source.identity,
            segments: wire_segments,
        }),
        descriptors.clone(),
        &source_fds,
        limits,
    );
    for descriptor in descriptors {
        capability_table
            .release(descriptor.id())
            .expect("release sent source capability");
    }
    assert_eq!(capability_table.live_count(), 0);
    let (ready, ready_fds) = supervisor
        .receive_event(decoder, limits)
        .expect("DocumentReady from real child");
    assert!(ready_fds.is_empty());
    assert!(matches!(ready.event, Event::DocumentReady(_)));

    send_correlated_command(
        supervisor,
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
    let (metrics, metrics_fds) = supervisor
        .receive_event(decoder, limits)
        .expect("PageMetrics from real child");
    assert!(metrics_fds.is_empty());
    let geometry = match metrics.event {
        Event::PageMetrics(metrics) => metrics.pages[0].geometry.clone(),
        other => panic!("expected PageMetrics, got {other:?}"),
    };
    (session, geometry)
}

fn render_viewport(geometry: PageGeometry) -> SetViewportCommand {
    const RENDER_EDGE_MILLI_POINTS: u32 = 2_048_000;

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
                clip_width_milli_points: RENDER_EDGE_MILLI_POINTS,
                clip_height_milli_points: RENDER_EDGE_MILLI_POINTS,
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
fn crash_restarts_new_epoch_cleans_resources_and_rejects_late_record_and_fd() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(2, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, cleanup_resources(limits))
            .expect("initial child");
    let old_worker = supervisor.worker_id().expect("old worker");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let handshake = negotiate(&mut supervisor, limits);
    let decoder = DesktopFrameDecoder::for_handshake(handshake);

    let live = DesktopCapability::new(
        1,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        old_epoch,
        4,
    )
    .expect("live capability");
    {
        let resources = supervisor.cleanup_mut();
        resources
            .capabilities
            .insert(live)
            .expect("track capability");
        resources
            .bridge
            .register(
                DataTicket::new(1),
                SessionId::new(1),
                old_epoch,
                vec![ByteRange { start: 0, len: 4 }],
            )
            .expect("track source ticket");
        resources.epoch_fds.push(
            ReadOnlySharedRegion::from_bytes(b"owned", limits)
                .expect("tracked old-epoch region")
                .into_fd(),
        );
    }

    let late_region =
        ReadOnlySharedRegion::from_bytes(b"late", limits).expect("late immutable region");
    let late_fd = late_region.into_fd();
    let late_capability = DesktopCapability::new(
        2,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        old_epoch,
        4,
    )
    .expect("late descriptor");
    let late_record = supervisor
        .new_host_record(3, vec![1], vec![late_capability], limits)
        .expect("late old-epoch record");

    signal_active(&supervisor, Signal::KILL);
    let fault = expect_worker_fault(supervisor.receive_event(decoder, limits));
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::UnexpectedEof);
    assert_eq!(fault.worker_id(), old_worker);
    assert_eq!(fault.worker_epoch(), old_epoch);
    assert_eq!(fault.restart_attempt(), Some(1));
    assert!(fault.protocol_event().error.wire_invariants_valid());
    let replacement_epoch = fault.replacement_epoch().expect("replacement epoch");
    assert!(replacement_epoch.value() > old_epoch.value());
    assert_eq!(supervisor.worker_epoch(), Some(replacement_epoch));
    assert_eq!(supervisor.cleanup().capabilities.live_count(), 0);
    assert_eq!(supervisor.cleanup().capabilities.retained_bytes(), 0);
    assert_eq!(supervisor.cleanup().bridge.outstanding(), 0);
    assert!(supervisor.cleanup().epoch_fds.is_empty());
    assert_eq!(supervisor.cleanup().retired, vec![(old_worker, old_epoch)]);

    assert_eq!(
        supervisor.send(&late_record, std::slice::from_ref(&late_fd), limits),
        Err(DesktopSupervisionError::Rejected(
            pdf_rs_desktop::DesktopIpcErrorCode::Authentication
        ))
    );
    validate_read_only_fd(&late_fd, 4).expect("late FD stayed Host-owned");
    assert_eq!(supervisor.worker_epoch(), Some(replacement_epoch));
    assert_eq!(supervisor.restart_count(), 1);

    negotiate(&mut supervisor, limits);
    supervisor.shutdown();
    assert_eq!(supervisor.state(), DesktopSupervisorState::Stopped);
    assert_eq!(supervisor.worker_epoch(), None);
    assert_eq!(supervisor.cleanup().capabilities.live_count(), 0);
    assert_eq!(supervisor.cleanup().bridge.outstanding(), 0);
    assert!(supervisor.cleanup().epoch_fds.is_empty());
    assert_eq!(supervisor.cleanup().retired.len(), 2);
}

#[test]
fn queued_render_disconnect_before_worker_execution_retires_epoch_and_restarts_once() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, cleanup_resources(limits))
            .expect("initial child");
    let old_worker = supervisor.worker_id().expect("old worker");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let handshake = negotiate(&mut supervisor, limits);
    let decoder = DesktopFrameDecoder::for_handshake(handshake);
    let (session, geometry) = open_fixture_for_render(&mut supervisor, decoder, limits);

    let tracked = DesktopCapability::new(
        101,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        session,
        old_epoch,
        4,
    )
    .expect("tracked render capability");
    {
        let resources = supervisor.cleanup_mut();
        resources
            .capabilities
            .insert(tracked)
            .expect("track current render capability");
        resources
            .bridge
            .register(
                DataTicket::new(101),
                session,
                old_epoch,
                vec![ByteRange { start: 0, len: 4 }],
            )
            .expect("track current render source range");
        resources.epoch_fds.push(
            ReadOnlySharedRegion::from_bytes(b"render-owned", limits)
                .expect("tracked render FD")
                .into_fd(),
        );
    }

    // Establish a scheduler-independent boundary: wait until the kernel has
    // stopped the real child, then enqueue a valid render command while it
    // cannot execute actor or raster work. This proves the queued-command
    // disconnect boundary; a post-plan/mid-raster boundary requires an
    // explicit child-to-Host test barrier.
    stop_active(&supervisor);
    send_correlated_command(
        &mut supervisor,
        6,
        Correlation {
            worker: old_worker,
            session: Some(session),
            request: None,
            generation: Some(1),
        },
        Command::SetViewport(render_viewport(geometry)),
        Vec::new(),
        &[],
        limits,
    );
    signal_active(&supervisor, Signal::KILL);
    let fault = expect_worker_fault(supervisor.receive_event(decoder, limits));
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::UnexpectedEof);
    assert_eq!(fault.worker_id(), old_worker);
    assert_eq!(fault.worker_epoch(), old_epoch);
    assert_eq!(fault.restart_attempt(), Some(1));
    let replacement_epoch = fault.replacement_epoch().expect("replacement epoch");
    assert!(replacement_epoch.value() > old_epoch.value());
    assert_eq!(supervisor.restart_count(), 1);
    assert_eq!(supervisor.state(), DesktopSupervisorState::Running);

    // The stopped worker could not consume the queued render command, so no
    // Surface event or descriptor can precede this terminal fault. Every
    // old-epoch Host resource is retired before the replacement is observable.
    assert_eq!(supervisor.cleanup().capabilities.live_count(), 0);
    assert_eq!(supervisor.cleanup().capabilities.retained_bytes(), 0);
    assert_eq!(supervisor.cleanup().bridge.outstanding(), 0);
    assert!(supervisor.cleanup().epoch_fds.is_empty());
    assert_eq!(supervisor.cleanup().retired, vec![(old_worker, old_epoch)]);

    negotiate(&mut supervisor, limits);
    supervisor.shutdown();
    assert_eq!(supervisor.state(), DesktopSupervisorState::Stopped);
    assert_eq!(supervisor.cleanup().retired.len(), 2);
}

#[test]
fn nonzero_child_exit_maps_to_fault_and_rehandshakes_once() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, ()).expect("initial child");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let handshake = negotiate(&mut supervisor, limits);
    let malformed = supervisor
        .new_host_record(3, vec![1], Vec::new(), limits)
        .expect("authenticated malformed canonical frame");
    supervisor
        .send(&malformed, &[], limits)
        .expect("outer transport accepts record");

    let fault = expect_worker_fault(
        supervisor.receive_event(DesktopFrameDecoder::for_handshake(handshake), limits),
    );
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::NonzeroExit);
    assert!(fault.replacement_epoch().expect("replacement").value() > old_epoch.value());
    negotiate(&mut supervisor, limits);
    supervisor.shutdown();
}

#[test]
fn abort_child_maps_to_panic_and_restarts_once() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, ()).expect("initial child");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let handshake = negotiate(&mut supervisor, limits);

    signal_active(&supervisor, Signal::ABORT);
    let fault = expect_worker_fault(
        supervisor.receive_event(DesktopFrameDecoder::for_handshake(handshake), limits),
    );
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::ChildPanic);
    assert!(fault.replacement_epoch().expect("replacement").value() > old_epoch.value());
    negotiate(&mut supervisor, limits);
    supervisor.shutdown();
}

#[test]
fn idle_transport_timeout_fault_restarts_and_rehandshakes() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, ()).expect("initial child");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let handshake = negotiate(&mut supervisor, limits);
    supervisor
        .set_transport_timeout(Duration::from_millis(100))
        .expect("short idle watchdog");

    let fault = expect_worker_fault(
        supervisor.receive_event(DesktopFrameDecoder::for_handshake(handshake), limits),
    );
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::TransportTimeout);
    assert!(fault.replacement_epoch().expect("replacement").value() > old_epoch.value());
    supervisor
        .set_transport_timeout(Duration::from_secs(2))
        .expect("handshake watchdog");
    negotiate(&mut supervisor, limits);
    supervisor.shutdown();
}

#[test]
fn graceful_shutdown_never_restarts() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(2, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, ()).expect("initial child");
    let handshake = negotiate(&mut supervisor, limits);
    let decoder = DesktopFrameDecoder::for_handshake(handshake);
    supervisor
        .begin_graceful_shutdown()
        .expect("begin graceful shutdown");
    send_command(
        &mut supervisor,
        3,
        Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
        limits,
    );

    let mut stopped = false;
    for _ in 0..4 {
        let (event, fds) = supervisor
            .receive_event(decoder, limits)
            .expect("graceful terminal event");
        assert!(fds.is_empty());
        match event.event {
            Event::ShutdownAcknowledged(_) => {}
            Event::WorkerStopped(_) => {
                stopped = true;
                break;
            }
            other => panic!("unexpected shutdown event: {other:?}"),
        }
    }
    assert!(stopped);
    supervisor.complete_graceful_shutdown();
    assert_eq!(supervisor.state(), DesktopSupervisorState::Stopped);
    assert_eq!(supervisor.restart_count(), 0);
    assert_eq!(supervisor.worker_epoch(), None);
    assert_eq!(
        supervisor.supervise(|_| Ok(())),
        Err(DesktopSupervisionError::Stopped)
    );
    assert_eq!(supervisor.restart_count(), 0);
}

#[test]
fn restart_limit_is_terminal_and_cannot_storm() {
    let limits = limits();
    let config =
        DesktopSupervisorConfig::new(2, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor =
        DesktopChildSupervisor::start(worker_path(), config, ()).expect("initial child");
    let mut last_epoch = supervisor.worker_epoch().expect("initial epoch");

    for fault_index in 0_u8..=2 {
        let handshake = negotiate(&mut supervisor, limits);
        signal_active(&supervisor, Signal::KILL);
        let fault = expect_worker_fault(
            supervisor.receive_event(DesktopFrameDecoder::for_handshake(handshake), limits),
        );
        assert_eq!(fault.kind(), DesktopWorkerFaultKind::UnexpectedEof);
        assert_eq!(fault.worker_epoch(), last_epoch);
        assert!(fault.protocol_event().error.wire_invariants_valid());
        if fault_index < 2 {
            assert_eq!(fault.restart_attempt(), Some(fault_index + 1));
            let replacement = fault.replacement_epoch().expect("bounded replacement");
            assert!(replacement.value() > last_epoch.value());
            assert_eq!(supervisor.state(), DesktopSupervisorState::Running);
            last_epoch = replacement;
        } else {
            assert_eq!(fault.restart_attempt(), None);
            assert_eq!(fault.replacement_epoch(), None);
            assert_eq!(
                supervisor.state(),
                DesktopSupervisorState::RestartLimitReached
            );
        }
    }

    assert_eq!(supervisor.restart_count(), 2);
    assert_eq!(supervisor.worker_epoch(), None);
    assert_eq!(
        supervisor.receive_handshake_event(limits),
        Err(DesktopSupervisionError::Stopped)
    );
    assert_eq!(supervisor.restart_count(), 2);
}
