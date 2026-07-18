#![cfg(unix)]

use std::fs::{OpenOptions, read_dir};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pdf_rs_desktop::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopChildSupervisor, DesktopDirection,
    DesktopEpochCleanup, DesktopIpcLimitConfig, DesktopIpcLimits, DesktopRecordBinding,
    DesktopSupervisionError, DesktopSupervisorConfig, DesktopSupervisorState, DesktopWireRecord,
    DesktopWorkerFault, DesktopWorkerFaultKind, ReadOnlySharedRegion, send_capability_fds,
};
use pdf_rs_protocol::{
    AlphaMode, CapabilityDecisionHash, Correlation, DesktopFrameDecoder,
    ENDPOINT_CAPABILITY_SHARED_MEMORY, EndpointCapabilities, EndpointRole, EnvelopeHeader, Event,
    EventEnvelope, HelloAcceptCommand, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES,
    MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_SHUTDOWN, MESSAGE_ID_SURFACE_READY, NativeBackend, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PayloadCodecLimits, PixelFormat, ProtocolHello, ProtocolLimits, ProtocolValidator,
    RenderConfigHash, RenderPlanHash, RenderPlanId, RendererEpoch, SCHEMA_HASH, SceneHash,
    SessionId, ShutdownCommand, SurfaceCoordinateSpace, SurfaceId, SurfaceMetadata, SurfaceOwner,
    SurfaceReadyEvent, SurfaceRegion, SurfaceTransport, WorkerId, encode_correlation_payload,
    encode_hello_accept_command_payload, encode_hello_command_payload,
    encode_shutdown_command_payload, encode_surface_ready_event_payload,
};
use pdf_rs_surface::WorkerEpoch;

const WIRE_FIXED_HEADER_BYTES: usize = 74;
const WIRE_CAPABILITY_BYTES: usize = 36;
const WIRE_TOKEN_OFFSET: usize = 4 + 36;
const WIRE_TOKEN_BYTES: usize = 32;
const SURFACE_BYTES: u64 = 64;
const INVALID_RECORD_MARKER: &str = ".invalid-record-sent";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdversarialCase {
    SurplusDescriptors,
    ForeignOwner,
    ForeignEpoch,
    WritableRights,
}

impl AdversarialCase {
    const ALL: [Self; 4] = [
        Self::SurplusDescriptors,
        Self::ForeignOwner,
        Self::ForeignEpoch,
        Self::WritableRights,
    ];

    const fn program_name(self) -> &'static str {
        match self {
            Self::SurplusDescriptors => "pdf-rs-adversarial-surplus-fds",
            Self::ForeignOwner => "pdf-rs-adversarial-foreign-owner",
            Self::ForeignEpoch => "pdf-rs-adversarial-foreign-epoch",
            Self::WritableRights => "pdf-rs-adversarial-writable-rights",
        }
    }

    fn from_argv_zero(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|case| value.ends_with(case.program_name()))
    }
}

#[derive(Clone, Copy)]
struct LaunchBootstrap {
    launch: u64,
    token: [u8; 32],
    epoch: WorkerEpoch,
    worker: WorkerId,
}

#[derive(Clone, Copy)]
struct RawCapability {
    id: u64,
    class: u8,
    rights: u8,
    owner: SessionId,
    epoch: WorkerEpoch,
    byte_length: u64,
}

#[derive(Default)]
struct RetiredEpochs(Vec<(WorkerId, WorkerEpoch)>);

impl DesktopEpochCleanup for RetiredEpochs {
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch) {
        self.0.push((worker, epoch));
    }
}

struct FixtureProgram {
    directory: PathBuf,
    program: PathBuf,
}

impl FixtureProgram {
    fn new(case: AdversarialCase) -> Self {
        let directory = (0_u8..32)
            .find_map(|attempt| {
                let candidate = std::env::temp_dir().join(format!(
                    "pdf-rs-desktop-adversarial-{}-{}-{attempt}",
                    std::process::id(),
                    case.program_name()
                ));
                match std::fs::create_dir(&candidate) {
                    Ok(()) => Some(candidate),
                    Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(failure) => panic!("create adversarial fixture directory: {failure}"),
                }
            })
            .expect("fresh adversarial fixture directory");
        let program = directory.join(case.program_name());
        symlink(
            std::env::current_exe().expect("current adversarial test executable"),
            &program,
        )
        .expect("link adversarial child executable");
        Self { directory, program }
    }

    fn path(&self) -> &Path {
        &self.program
    }
}

impl Drop for FixtureProgram {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.program);
        let _ = std::fs::remove_file(self.directory.join(INVALID_RECORD_MARKER));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn main() {
    let argv_zero = std::env::args().next().expect("process argv[0]");
    if let Some(case) = AdversarialCase::from_argv_zero(&argv_zero) {
        if first_fixture_launch(&argv_zero) {
            run_adversarial_child(case);
        } else {
            pdf_rs_desktop::run_child_stdio(limits()).expect("healthy replacement worker");
        }
        return;
    }

    prove_public_record_layout_matches_fixture_encoding();
    for case in AdversarialCase::ALL {
        rejects_adversarial_child_transfer_without_fd_leak(case);
    }
}

fn first_fixture_launch(argv_zero: &str) -> bool {
    let marker = Path::new(argv_zero)
        .parent()
        .expect("fixture executable directory")
        .join(INVALID_RECORD_MARKER);
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(file) => {
            drop(file);
            true
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(failure) => panic!("create fixture launch marker: {failure}"),
    }
}

fn limits() -> DesktopIpcLimits {
    DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("default desktop IPC limits")
}

fn protocol_decoder() -> DesktopFrameDecoder {
    let hello = |endpoint_role| ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role,
        capabilities: EndpointCapabilities {
            supported: ENDPOINT_CAPABILITY_SHARED_MEMORY,
            mandatory: ENDPOINT_CAPABILITY_SHARED_MEMORY,
        },
        max_message_bytes: pdf_rs_protocol::MAX_MESSAGE_BYTES,
        max_transfer_slots: pdf_rs_protocol::MAX_TRANSFER_SLOTS,
    };
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .expect("shared-memory compatible handshake");
    DesktopFrameDecoder::for_handshake(handshake)
}

fn host_hello() -> ProtocolHello {
    ProtocolHello {
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
    }
}

fn command_frame(
    sequence: u64,
    correlation: Correlation,
    message_type: u16,
    command_payload: Vec<u8>,
) -> Vec<u8> {
    let mut payload =
        encode_correlation_payload(&correlation, PayloadCodecLimits::protocol_default())
            .expect("correlation payload");
    payload.extend_from_slice(&command_payload);

    let mut frame = Vec::with_capacity(20 + payload.len());
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("command payload length")
            .to_le_bytes(),
    );
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn send_command(
    supervisor: &mut DesktopChildSupervisor<RetiredEpochs>,
    sequence: u64,
    message_type: u16,
    command_payload: Vec<u8>,
    limits: DesktopIpcLimits,
) {
    let correlation = Correlation {
        worker: supervisor.worker_id().expect("active replacement worker"),
        session: None,
        request: None,
        generation: None,
    };
    let record = supervisor
        .new_host_record(
            sequence,
            command_frame(sequence, correlation, message_type, command_payload),
            Vec::new(),
            limits,
        )
        .expect("current authenticated Host record");
    supervisor
        .send(&record, &[], limits)
        .expect("send replacement command");
}

fn negotiate_replacement(
    supervisor: &mut DesktopChildSupervisor<RetiredEpochs>,
    limits: DesktopIpcLimits,
) -> pdf_rs_protocol::CompatibleHandshake {
    let hello = host_hello();
    send_command(
        supervisor,
        1,
        MESSAGE_ID_HELLO,
        encode_hello_command_payload(
            &HelloCommand {
                hello: hello.clone(),
            },
            PayloadCodecLimits::protocol_default(),
        )
        .expect("Hello command payload"),
        limits,
    );

    let engine_hello_event = supervisor
        .receive_handshake_event(limits)
        .expect("replacement EngineHello");
    let engine_hello = match engine_hello_event.event {
        Event::EngineHello(engine_hello) => engine_hello,
        other => panic!("expected replacement EngineHello, got {other:?}"),
    };
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello, &engine_hello.hello)
        .expect("compatible replacement handshake");

    send_command(
        supervisor,
        2,
        MESSAGE_ID_HELLO_ACCEPT,
        encode_hello_accept_command_payload(
            &HelloAcceptCommand {
                negotiated_minor: handshake.minor(),
                schema_hash: SCHEMA_HASH,
            },
            PayloadCodecLimits::protocol_default(),
        )
        .expect("HelloAccept command payload"),
        limits,
    );
    let ready = supervisor
        .receive_handshake_event(limits)
        .expect("replacement Ready");
    match ready.event {
        Event::Ready(_) => {}
        other => panic!("expected replacement Ready, got {other:?}"),
    }
    handshake
}

fn stop_replacement_cleanly(
    supervisor: &mut DesktopChildSupervisor<RetiredEpochs>,
    handshake: pdf_rs_protocol::CompatibleHandshake,
    limits: DesktopIpcLimits,
) {
    supervisor
        .begin_graceful_shutdown()
        .expect("begin replacement graceful shutdown");
    send_command(
        supervisor,
        3,
        MESSAGE_ID_SHUTDOWN,
        encode_shutdown_command_payload(
            &ShutdownCommand { deadline_ms: 1_000 },
            PayloadCodecLimits::protocol_default(),
        )
        .expect("Shutdown command payload"),
        limits,
    );

    let decoder = DesktopFrameDecoder::for_handshake(handshake);
    let mut stopped = false;
    for _ in 0..4 {
        let (event, descriptors) = supervisor
            .receive_event(decoder, limits)
            .expect("replacement graceful terminal event");
        assert!(
            descriptors.is_empty(),
            "replacement terminal event carried descriptors"
        );
        match event.event {
            Event::ShutdownAcknowledged(_) => {}
            Event::WorkerStopped(_) => {
                stopped = true;
                break;
            }
            other => panic!("unexpected replacement shutdown event: {other:?}"),
        }
    }
    assert!(stopped, "replacement did not report WorkerStopped");
    supervisor.complete_graceful_shutdown();
}

fn rejects_adversarial_child_transfer_without_fd_leak(case: AdversarialCase) {
    let limits = limits();
    let fixture = FixtureProgram::new(case);
    let before_spawn = open_fd_count();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor = DesktopChildSupervisor::start(
        fixture.path().to_str().expect("UTF-8 fixture path"),
        config,
        RetiredEpochs::default(),
    )
    .expect("spawn adversarial real child");
    let old_worker = supervisor.worker_id().expect("initial worker");
    let old_epoch = supervisor.worker_epoch().expect("initial epoch");
    let active_fd_count = open_fd_count();

    let fault = expect_worker_fault(supervisor.receive_event(protocol_decoder(), limits));
    assert_eq!(
        fault.kind(),
        DesktopWorkerFaultKind::ProtocolViolation,
        "{case:?}"
    );
    assert_eq!(fault.worker_id(), old_worker, "{case:?}");
    assert_eq!(fault.worker_epoch(), old_epoch, "{case:?}");
    assert_eq!(fault.restart_attempt(), Some(1), "{case:?}");
    assert_eq!(supervisor.restart_count(), 1, "{case:?}");
    assert_eq!(
        supervisor.state(),
        DesktopSupervisorState::Running,
        "{case:?}"
    );
    assert!(
        fault
            .replacement_epoch()
            .is_some_and(|epoch| epoch.value() > old_epoch.value()),
        "{case:?}"
    );
    assert_eq!(
        supervisor.cleanup().0,
        vec![(old_worker, old_epoch)],
        "{case:?}"
    );
    assert_eq!(
        supervisor.worker_id(),
        fault.replacement_worker_id(),
        "{case:?}"
    );
    assert_eq!(
        supervisor.worker_epoch(),
        fault.replacement_epoch(),
        "{case:?}"
    );

    // The old imported SCM_RIGHTS entries were owned by the failed receive
    // transaction and must have closed before the replacement socket exists.
    assert_eq!(open_fd_count(), active_fd_count, "{case:?} leaked an FD");

    let handshake = negotiate_replacement(&mut supervisor, limits);
    assert_eq!(
        open_fd_count(),
        active_fd_count,
        "{case:?} leaked during replacement handshake"
    );
    stop_replacement_cleanly(&mut supervisor, handshake, limits);
    assert_eq!(
        supervisor.state(),
        DesktopSupervisorState::Stopped,
        "{case:?}"
    );
    assert_eq!(
        open_fd_count(),
        before_spawn,
        "{case:?} leaked after graceful stop"
    );
    drop(supervisor);
    assert_eq!(
        open_fd_count(),
        before_spawn,
        "{case:?} leaked after shutdown"
    );
}

fn expect_worker_fault<T>(result: Result<T, DesktopSupervisionError>) -> DesktopWorkerFault {
    match result {
        Err(DesktopSupervisionError::WorkerFault(fault)) => fault,
        Err(other) => panic!("expected WorkerFault, got {other:?}"),
        Ok(_) => panic!("adversarial Surface was partially delivered"),
    }
}

fn open_fd_count() -> usize {
    ["/proc/self/fd", "/dev/fd"]
        .into_iter()
        .find_map(|path| read_dir(path).ok().map(Iterator::count))
        .expect("platform exposes process descriptor directory")
}

fn run_adversarial_child(case: AdversarialCase) {
    let input =
        rustix::io::dup(std::io::stdin()).expect("duplicate inherited private child socket");
    let mut socket = UnixStream::from(input);
    let bootstrap = read_bootstrap(&mut socket);
    let frame = surface_ready_frame(bootstrap.worker);
    let descriptor = RawCapability {
        id: 1,
        class: CapabilityClass::SurfaceRegion as u8,
        rights: CapabilityRights::ReadOnly as u8,
        owner: if case == AdversarialCase::ForeignOwner {
            SessionId::new(2)
        } else {
            SessionId::new(1)
        },
        epoch: if case == AdversarialCase::ForeignEpoch {
            WorkerEpoch::new(
                bootstrap
                    .epoch
                    .value()
                    .checked_add(1)
                    .expect("fixture epoch increment"),
            )
            .expect("nonzero fixture epoch")
        } else {
            bootstrap.epoch
        },
        byte_length: SURFACE_BYTES,
    };
    let record = raw_worker_record(bootstrap, &frame, descriptor);
    let mut fds = vec![fixture_fd(case)];
    if case == AdversarialCase::SurplusDescriptors {
        fds.push(
            ReadOnlySharedRegion::from_bytes(&[0x5a; SURFACE_BYTES as usize], limits())
                .expect("surplus read-only region")
                .into_fd(),
        );
    }

    send_capability_fds(&socket, &fds, limits()).expect("send real kernel SCM_RIGHTS");
    socket
        .write_all(&record)
        .expect("send adversarial authenticated record");
    socket.flush().expect("flush adversarial record");

    // Keep a real child alive until the Host closes the poisoned transport.
    let mut terminal = [0_u8; 1];
    let _ = socket.read(&mut terminal);
}

fn read_bootstrap(socket: &mut UnixStream) -> LaunchBootstrap {
    let mut launch = [0_u8; 8];
    let mut token = [0_u8; 32];
    let mut host_pid = [0_u8; 4];
    let mut epoch = [0_u8; 8];
    let mut worker = [0_u8; 8];
    socket.read_exact(&mut launch).expect("launch identity");
    socket.read_exact(&mut token).expect("launch token");
    socket.read_exact(&mut host_pid).expect("Host PID");
    socket.read_exact(&mut epoch).expect("worker epoch");
    socket.read_exact(&mut worker).expect("worker identity");
    assert_ne!(u32::from_le_bytes(host_pid), 0);
    LaunchBootstrap {
        launch: u64::from_le_bytes(launch),
        token,
        epoch: WorkerEpoch::new(u64::from_le_bytes(epoch)).expect("nonzero worker epoch"),
        worker: WorkerId::new(u64::from_le_bytes(worker)),
    }
}

fn fixture_fd(case: AdversarialCase) -> OwnedFd {
    if case != AdversarialCase::WritableRights {
        return ReadOnlySharedRegion::from_bytes(&[0x39; SURFACE_BYTES as usize], limits())
            .expect("read-only fixture region")
            .into_fd();
    }

    let path = std::env::temp_dir().join(format!(
        "pdf-rs-adversarial-writable-{}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create writable fixture region");
    file.write_all(&[0x77; SURFACE_BYTES as usize])
        .expect("size writable fixture region");
    std::fs::remove_file(path).expect("unlink writable fixture region");
    file.into()
}

fn surface_ready_frame(worker: WorkerId) -> Vec<u8> {
    let correlation = Correlation {
        worker,
        session: Some(SessionId::new(1)),
        request: None,
        generation: Some(1),
    };
    let event = SurfaceReadyEvent {
        metadata: SurfaceMetadata {
            id: SurfaceId::new(1),
            lease_token: 1,
            owner: SurfaceOwner {
                worker,
                session: SessionId::new(1),
            },
            generation: 1,
            region: SurfaceRegion {
                page_index: 0,
                x: 0,
                y: 0,
                width: 4,
                height: 4,
                coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
            },
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Rgba8,
            alpha: AlphaMode::Premultiplied,
            byte_offset: 0,
            byte_length: SURFACE_BYTES,
            render_config: RenderConfigHash::new([1; 32]),
            renderer_epoch: RendererEpoch::new(1),
            plan_id: RenderPlanId::new(1),
            plan_hash: RenderPlanHash::new([2; 32]),
            scene_hash: SceneHash::new([3; 32]),
            decision_hash: CapabilityDecisionHash::new([4; 32]),
            backend: NativeBackend::FastCpu,
        },
        transport: SurfaceTransport::SharedMemory {
            slot: 0,
            region_length: SURFACE_BYTES,
        },
    };
    let limits = PayloadCodecLimits::protocol_default();
    let mut payload =
        encode_correlation_payload(&correlation, limits).expect("correlation payload");
    payload.extend_from_slice(
        &encode_surface_ready_event_payload(&event, limits).expect("SurfaceReady payload"),
    );
    let envelope = EventEnvelope {
        header: EnvelopeHeader {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            message_type: MESSAGE_ID_SURFACE_READY,
            flags: 0,
            payload_len: u32::try_from(payload.len()).expect("payload length"),
            sequence: 1,
        },
        correlation,
        event: Event::SurfaceReady(event),
    };
    let (_, canonical_payload) =
        pdf_rs_protocol::encode_event_payload(&envelope, limits).expect("canonical event payload");
    assert_eq!(canonical_payload, payload);

    let mut frame = Vec::with_capacity(20 + payload.len());
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&MESSAGE_ID_SURFACE_READY.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("payload length")
            .to_le_bytes(),
    );
    frame.extend_from_slice(&1_u64.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn raw_worker_record(
    bootstrap: LaunchBootstrap,
    frame: &[u8],
    descriptor: RawCapability,
) -> Vec<u8> {
    let body_length = WIRE_FIXED_HEADER_BYTES
        .checked_add(frame.len())
        .and_then(|length| length.checked_add(WIRE_CAPABILITY_BYTES))
        .expect("bounded fixture record length");
    let mut record = Vec::with_capacity(4 + body_length);
    record.extend_from_slice(
        &u32::try_from(body_length)
            .expect("fixture record length")
            .to_le_bytes(),
    );
    record.extend_from_slice(b"PD09");
    record.extend_from_slice(&[1, DesktopDirection::WorkerToHost as u8, 0, 0]);
    record.extend_from_slice(&bootstrap.launch.to_le_bytes());
    record.extend_from_slice(&std::process::id().to_le_bytes());
    record.extend_from_slice(&bootstrap.epoch.value().to_le_bytes());
    record.extend_from_slice(&1_u64.to_le_bytes());
    record.extend_from_slice(&bootstrap.token);
    record.extend_from_slice(
        &u32::try_from(frame.len())
            .expect("fixture frame length")
            .to_le_bytes(),
    );
    record.extend_from_slice(&1_u16.to_le_bytes());
    record.extend_from_slice(frame);
    record.extend_from_slice(&descriptor.id.to_le_bytes());
    record.extend_from_slice(&[descriptor.class, descriptor.rights, 0, 0]);
    record.extend_from_slice(&descriptor.owner.value().to_le_bytes());
    record.extend_from_slice(&descriptor.epoch.value().to_le_bytes());
    record.extend_from_slice(&descriptor.byte_length.to_le_bytes());
    assert_eq!(record.len(), 4 + body_length);
    record
}

fn prove_public_record_layout_matches_fixture_encoding() {
    let limits = limits();
    let auth = pdf_rs_desktop::DesktopLaunchAuth::new().expect("fresh auth");
    let epoch = WorkerEpoch::new(1).expect("epoch");
    let descriptor = DesktopCapability::new(
        1,
        CapabilityClass::SurfaceRegion,
        CapabilityRights::ReadOnly,
        SessionId::new(1),
        epoch,
        SURFACE_BYTES,
    )
    .expect("descriptor");
    let record = DesktopWireRecord::new(
        &auth,
        DesktopRecordBinding {
            direction: DesktopDirection::WorkerToHost,
            sender_pid: std::process::id(),
            worker_epoch: epoch,
            sequence: 1,
        },
        surface_ready_frame(WorkerId::new(1)),
        vec![descriptor],
        limits,
    )
    .expect("public record");
    let mut encoded = Vec::new();
    record
        .write_to(&mut encoded, limits)
        .expect("public encoding");
    let mut token = [0_u8; WIRE_TOKEN_BYTES];
    token.copy_from_slice(
        encoded
            .get(WIRE_TOKEN_OFFSET..WIRE_TOKEN_OFFSET + WIRE_TOKEN_BYTES)
            .expect("opaque public record token"),
    );
    let expected = raw_worker_record(
        LaunchBootstrap {
            launch: auth.launch().value(),
            token,
            epoch,
            worker: WorkerId::new(1),
        },
        record.frame(),
        RawCapability {
            id: descriptor.id(),
            class: descriptor.class() as u8,
            rights: descriptor.rights() as u8,
            owner: descriptor.owner(),
            epoch: descriptor.worker_epoch(),
            byte_length: descriptor.byte_length(),
        },
    );
    assert_eq!(expected, encoded);
}
