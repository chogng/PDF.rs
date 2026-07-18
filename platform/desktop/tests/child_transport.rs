#![cfg(unix)]

use std::os::fd::OwnedFd;
use std::sync::Arc;

use pdf_rs_desktop::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
    DesktopEpochManager, DesktopIpcLimitConfig, DesktopIpcLimits, HostRangeBridge,
    HostSourceSnapshot, ReadOnlySharedRegion, receive_capability_fds, send_capability_fds,
    validate_engine_hello_event, validate_read_only_fd,
};
use pdf_rs_protocol::{
    ByteRange, Command, CommandEnvelope, Correlation, DataTicket, EndpointCapabilities,
    EndpointRole, EngineExecutionCapabilities, EngineHelloEvent, EnvelopeHeader, Event,
    EventEnvelope, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES,
    MAX_TRANSFER_SLOTS, PROTOCOL_MAJOR, PROTOCOL_MINOR, PayloadCodecLimits, ProtocolHello,
    SCHEMA_HASH, SequenceTracker, SessionId, SourceDescriptor, SourceIdentity, WorkerId,
    encode_command_payload, encode_event_payload,
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
fn unanswered_legal_handshake_times_out_and_poison_closes_process() {
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
    assert!(host.receive(limits).is_err());
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
