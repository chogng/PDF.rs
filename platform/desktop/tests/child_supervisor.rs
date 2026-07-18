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
    ByteRange, Command, Correlation, DataTicket, DesktopFrameDecoder, EndpointCapabilities,
    EndpointRole, Event, HelloAcceptCommand, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES,
    MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_SHUTDOWN, PROTOCOL_MAJOR, PROTOCOL_MINOR, PayloadCodecLimits, ProtocolHello,
    ProtocolLimits, ProtocolValidator, SCHEMA_HASH, SessionId, ShutdownCommand, SourceDescriptor,
    SourceIdentity, WorkerId, encode_correlation_payload, encode_hello_accept_command_payload,
    encode_hello_command_payload, encode_shutdown_command_payload,
};
use pdf_rs_surface::WorkerEpoch;
use rustix::process::{Pid, Signal, kill_process};

fn limits() -> DesktopIpcLimits {
    DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("default limits")
}

fn worker_path() -> &'static str {
    env!("CARGO_BIN_EXE_pdf-rs-desktop-worker")
}

fn command_frame(sequence: u64, worker: WorkerId, command: Command) -> Vec<u8> {
    let correlation = Correlation {
        worker,
        session: None,
        request: None,
        generation: None,
    };
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
    let record = supervisor
        .new_host_record(
            sequence,
            command_frame(sequence, worker, command),
            Vec::new(),
            limits,
        )
        .expect("current authenticated record");
    supervisor
        .send(&record, &[], limits)
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

fn signal_active<C: DesktopEpochCleanup>(supervisor: &DesktopChildSupervisor<C>, signal: Signal) {
    let raw_pid = i32::try_from(supervisor.process_id().expect("active child PID"))
        .expect("PID fits platform type");
    let pid = Pid::from_raw(raw_pid).expect("nonzero child PID");
    kill_process(pid, signal).expect("signal child");
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
