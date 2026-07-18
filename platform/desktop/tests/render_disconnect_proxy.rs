#![cfg(unix)]

use std::fs::{OpenOptions, read_dir};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use pdf_rs_desktop::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopCapabilityTable,
    DesktopChildSupervisor, DesktopDirection, DesktopEpochCleanup, DesktopIpcLimitConfig,
    DesktopIpcLimits, DesktopLaunchAuth, DesktopRecordBinding, DesktopSupervisionError,
    DesktopSupervisorConfig, DesktopSupervisorState, DesktopWireRecord, DesktopWorkerFault,
    DesktopWorkerFaultKind, HostRangeBridge, HostSourceSnapshot, ReadOnlySharedRegion,
    receive_capability_fds, send_capability_fds,
};
use pdf_rs_protocol::{
    ByteRange, Command, Correlation, DataAttachmentRole, DataSegment, DataTicket,
    DesktopFrameDecoder, EndpointCapabilities, EndpointRole, Event, GetPageMetricsCommand,
    HelloAcceptCommand, HelloCommand, KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES,
    MAX_TRANSFER_SLOTS, MESSAGE_ID_GET_PAGE_METRICS, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_OPEN, MESSAGE_ID_PROVIDE_DATA, MESSAGE_ID_SET_VIEWPORT, MESSAGE_ID_SHUTDOWN,
    MESSAGE_ID_SURFACE_READY, OpenCommand, OutputProfile, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PageCoordinateSpace, PageGeometry, PageRotation, PageViewport, PayloadCodecLimits,
    ProtocolHello, ProtocolLimits, ProtocolValidator, ProvideDataCommand, QualityPolicy, RequestId,
    SCHEMA_HASH, SessionId, SetViewportCommand, ShutdownCommand, SourceDescriptor, SourceIdentity,
    ViewportRequest, WorkerId, encode_correlation_payload, encode_get_page_metrics_command_payload,
    encode_hello_accept_command_payload, encode_hello_command_payload, encode_open_command_payload,
    encode_provide_data_command_payload, encode_set_viewport_command_payload,
    encode_shutdown_command_payload,
};
use pdf_rs_surface::WorkerEpoch;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

const PROXY_PROGRAM_NAME: &str = "pdf-rs-render-disconnect-proxy";
const FIRST_LAUNCH_MARKER: &str = ".first-launch-complete";
const PROXY_PROOF: &str = ".post-plan-proxy-proof";
const PROXY_PROOF_BYTES: &[u8] = b"generation-planned-forwarded;real-worker-killed-and-reaped\n";
const BOOTSTRAP_BYTES: usize = 8 + 32 + 4 + 8 + 8;
const RECORD_PREFIX_BYTES: usize = 4;
const RECORD_FIXED_HEADER_BYTES: usize = 74;
const RECORD_CAPABILITY_BYTES: usize = 36;
const RECORD_DIRECTION_OFFSET: usize = RECORD_PREFIX_BYTES + 5;
const RECORD_SENDER_PID_OFFSET: usize = RECORD_PREFIX_BYTES + 16;
const RECORD_FRAME_LENGTH_OFFSET: usize = RECORD_PREFIX_BYTES + 68;
const RECORD_CAPABILITY_COUNT_OFFSET: usize = RECORD_PREFIX_BYTES + 72;
const RECORD_FRAME_OFFSET: usize = RECORD_PREFIX_BYTES + RECORD_FIXED_HEADER_BYTES;
const FRAME_MESSAGE_TYPE_OFFSET: usize = 4;
const MAX_PROXY_EVENTS_BEFORE_PLAN: usize = 12;
const MAX_HOST_PREPUBLICATION_EVENTS: usize = 4;
const MAX_SHUTDOWN_EVENTS: usize = 4;

struct TrackedResources {
    capabilities: DesktopCapabilityTable,
    bridge: Option<HostRangeBridge>,
    epoch_fds: Vec<OwnedFd>,
    retired: Vec<(WorkerId, WorkerEpoch)>,
}

impl DesktopEpochCleanup for TrackedResources {
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch) {
        self.capabilities.retire_epoch(worker, epoch);
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.retire_epoch(worker, epoch);
        }
        self.epoch_fds.clear();
        self.retired.push((worker, epoch));
    }
}

struct FixtureProgram {
    directory: PathBuf,
    program: PathBuf,
}

impl FixtureProgram {
    fn new() -> Self {
        let directory = (0_u8..32)
            .find_map(|attempt| {
                let candidate = std::env::temp_dir().join(format!(
                    "pdf-rs-render-disconnect-{}-{attempt}",
                    std::process::id()
                ));
                match std::fs::create_dir(&candidate) {
                    Ok(()) => Some(candidate),
                    Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(failure) => panic!("create proxy fixture directory: {failure}"),
                }
            })
            .expect("fresh proxy fixture directory");
        let program = directory.join(PROXY_PROGRAM_NAME);
        symlink(
            std::env::current_exe().expect("current proxy test executable"),
            &program,
        )
        .expect("link proxy child executable");
        Self { directory, program }
    }

    fn path(&self) -> &Path {
        &self.program
    }

    fn proof_path(&self) -> PathBuf {
        self.directory.join(PROXY_PROOF)
    }
}

impl Drop for FixtureProgram {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.program);
        let _ = std::fs::remove_file(self.directory.join(FIRST_LAUNCH_MARKER));
        let _ = std::fs::remove_file(self.directory.join(PROXY_PROOF));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

struct RawPacket {
    record: Vec<u8>,
    fds: Vec<OwnedFd>,
}

struct RealWorker(Option<Child>);

impl RealWorker {
    fn id(&self) -> u32 {
        self.0.as_ref().expect("live real worker").id()
    }

    fn kill_and_reap(&mut self) -> ExitStatus {
        let mut child = self.0.take().expect("live real worker");
        child
            .kill()
            .expect("kill real worker immediately after forwarding GenerationPlanned");
        child.wait().expect("reap killed real worker")
    }
}

impl Drop for RealWorker {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn main() {
    let argv_zero = std::env::args().next().expect("process argv[0]");
    if argv_zero.ends_with(PROXY_PROGRAM_NAME) {
        if first_fixture_launch(&argv_zero) {
            run_disconnect_proxy(&argv_zero);
        } else {
            pdf_rs_desktop::run_child_stdio(limits()).expect("healthy replacement worker");
        }
        return;
    }

    prove_sender_pid_rewrite_matches_public_encoding();
    post_plan_proxy_disconnect_never_publishes_surface_and_restarts_once();
}

fn limits() -> DesktopIpcLimits {
    DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("default desktop IPC limits")
}

fn first_fixture_launch(argv_zero: &str) -> bool {
    let marker = Path::new(argv_zero)
        .parent()
        .expect("fixture executable directory")
        .join(FIRST_LAUNCH_MARKER);
    match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(file) => {
            drop(file);
            true
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(failure) => panic!("create fixture launch marker: {failure}"),
    }
}

fn run_disconnect_proxy(argv_zero: &str) {
    let limits = limits();
    let input = rustix::io::dup(std::io::stdin()).expect("duplicate inherited Host socket");
    rustix::io::fcntl_setfd(&input, rustix::io::FdFlags::CLOEXEC)
        .expect("protect Host endpoint from real-worker exec");
    let mut host_socket = UnixStream::from(input);
    set_socket_timeouts(&host_socket);

    let mut bootstrap = [0_u8; BOOTSTRAP_BYTES];
    host_socket
        .read_exact(&mut bootstrap)
        .expect("read exact Host bootstrap");
    let host_pid = u32::from_le_bytes(
        bootstrap[40..44]
            .try_into()
            .expect("bootstrap Host PID bytes"),
    );
    assert_ne!(host_pid, 0);

    let (mut worker, mut worker_socket) = spawn_real_worker();
    let real_worker_pid = worker.id();
    worker_socket
        .write_all(&bootstrap)
        .expect("forward exact bootstrap");
    worker_socket.flush().expect("flush real-worker bootstrap");

    let proxy_pid = std::process::id();
    let stopping = Arc::new(AtomicBool::new(false));
    let forward_stopping = Arc::clone(&stopping);
    let host_reader = host_socket.try_clone().expect("clone Host socket");
    let worker_writer = worker_socket.try_clone().expect("clone worker socket");
    let forwarder = std::thread::spawn(move || {
        forward_host_commands(
            host_reader,
            worker_writer,
            limits,
            host_pid,
            &forward_stopping,
        )
    });

    let mut saw_generation_planned = false;
    for _ in 0..MAX_PROXY_EVENTS_BEFORE_PLAN {
        let mut packet =
            receive_packet(&mut worker_socket, limits).expect("receive real-worker packet");
        assert_eq!(
            packet_direction(&packet.record),
            DesktopDirection::WorkerToHost as u8,
            "real worker sent wrong record direction"
        );
        assert_eq!(
            packet_sender_pid(&packet.record),
            real_worker_pid,
            "record did not originate from the spawned real worker"
        );
        let message_type = packet_message_type(&packet.record);
        assert_ne!(
            message_type, MESSAGE_ID_SURFACE_READY,
            "real worker violated GenerationPlanned-before-SurfaceReady ordering"
        );
        rewrite_sender_pid(&mut packet.record, proxy_pid);
        send_packet(&mut host_socket, packet, limits).expect("forward worker packet to Host");
        saw_generation_planned = message_type == pdf_rs_protocol::MESSAGE_ID_GENERATION_PLANNED;
        if saw_generation_planned {
            break;
        }
    }
    assert!(
        saw_generation_planned,
        "real worker exceeded the hard event limit before GenerationPlanned"
    );

    let status = worker.kill_and_reap();
    assert!(
        !status.success(),
        "real worker unexpectedly exited successfully"
    );
    write_proxy_proof(argv_zero);

    stopping.store(true, Ordering::Release);
    let _ = worker_socket.shutdown(Shutdown::Both);
    let _ = host_socket.shutdown(Shutdown::Both);
    drop(worker_socket);
    drop(host_socket);
    forwarder
        .join()
        .expect("Host-command forwarder did not panic")
        .expect("Host-command forwarder failed before intentional shutdown");
}

fn spawn_real_worker() -> (RealWorker, UnixStream) {
    let (proxy_fd, worker_fd) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::empty(),
        None,
    )
    .expect("create private proxy-to-worker socketpair");
    rustix::io::fcntl_setfd(&proxy_fd, rustix::io::FdFlags::CLOEXEC)
        .expect("protect proxy endpoint across exec");
    let worker_stdout = rustix::io::dup(&worker_fd).expect("duplicate real-worker endpoint");
    rustix::io::fcntl_setfd(&worker_fd, rustix::io::FdFlags::CLOEXEC)
        .expect("protect real-worker stdin source");
    rustix::io::fcntl_setfd(&worker_stdout, rustix::io::FdFlags::CLOEXEC)
        .expect("protect real-worker stdout source");
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_pdf-rs-desktop-worker"))
        .arg("--pdf-rs-desktop-child")
        .stdin(Stdio::from(worker_fd))
        .stdout(Stdio::from(worker_stdout))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn real desktop worker");
    let socket = UnixStream::from(proxy_fd);
    set_socket_timeouts(&socket);
    (RealWorker(Some(child)), socket)
}

fn set_socket_timeouts(socket: &UnixStream) {
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set bounded proxy read timeout");
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set bounded proxy write timeout");
}

fn forward_host_commands(
    mut host_socket: UnixStream,
    mut worker_socket: UnixStream,
    limits: DesktopIpcLimits,
    expected_host_pid: u32,
    stopping: &AtomicBool,
) -> Result<(), String> {
    loop {
        let packet = match receive_packet(&mut host_socket, limits) {
            Ok(packet) => packet,
            Err(failure) if stopping.load(Ordering::Acquire) => return Ok(()),
            Err(failure) => return Err(failure),
        };
        if packet_direction(&packet.record) != DesktopDirection::HostToWorker as u8 {
            return Err("Host sent wrong record direction".to_owned());
        }
        if packet_sender_pid(&packet.record) != expected_host_pid {
            return Err("Host record sender PID differs from bootstrap".to_owned());
        }
        if let Err(failure) = send_packet(&mut worker_socket, packet, limits) {
            if stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            return Err(failure);
        }
    }
}

fn receive_packet(socket: &mut UnixStream, limits: DesktopIpcLimits) -> Result<RawPacket, String> {
    let fds = receive_capability_fds(&*socket, limits)
        .map_err(|failure| format!("receive SCM_RIGHTS table: {failure}"))?;
    let mut prefix = [0_u8; RECORD_PREFIX_BYTES];
    socket
        .read_exact(&mut prefix)
        .map_err(|failure| format!("read record prefix: {failure}"))?;
    let body_length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| "record body length does not fit usize".to_owned())?;
    if body_length < RECORD_FIXED_HEADER_BYTES || body_length > limits.max_record_bytes() {
        return Err(format!("record body length out of bounds: {body_length}"));
    }
    let mut record = Vec::new();
    record
        .try_reserve_exact(RECORD_PREFIX_BYTES + body_length)
        .map_err(|_| "reserve bounded record".to_owned())?;
    record.extend_from_slice(&prefix);
    record.resize(RECORD_PREFIX_BYTES + body_length, 0);
    socket
        .read_exact(&mut record[RECORD_PREFIX_BYTES..])
        .map_err(|failure| format!("read bounded record body: {failure}"))?;
    validate_raw_record_layout(&record, fds.len())?;
    Ok(RawPacket { record, fds })
}

fn send_packet(
    socket: &mut UnixStream,
    packet: RawPacket,
    limits: DesktopIpcLimits,
) -> Result<(), String> {
    send_capability_fds(&*socket, &packet.fds, limits)
        .map_err(|failure| format!("send SCM_RIGHTS table: {failure}"))?;
    socket
        .write_all(&packet.record)
        .map_err(|failure| format!("write bounded record: {failure}"))?;
    socket
        .flush()
        .map_err(|failure| format!("flush bounded record: {failure}"))
}

fn validate_raw_record_layout(record: &[u8], received_fds: usize) -> Result<(), String> {
    if record.len() < RECORD_FRAME_OFFSET {
        return Err("record shorter than fixed encoding".to_owned());
    }
    let body_length = read_u32(record, 0)? as usize;
    if record.len() != RECORD_PREFIX_BYTES + body_length {
        return Err("record prefix does not match exact body length".to_owned());
    }
    if record.get(4..8) != Some(b"PD09") || record.get(8) != Some(&1) {
        return Err("record magic or version mismatch".to_owned());
    }
    let frame_length = read_u32(record, RECORD_FRAME_LENGTH_OFFSET)? as usize;
    let capability_count = usize::from(read_u16(record, RECORD_CAPABILITY_COUNT_OFFSET)?);
    let expected = RECORD_FRAME_OFFSET
        .checked_add(frame_length)
        .and_then(|length| {
            length.checked_add(capability_count.checked_mul(RECORD_CAPABILITY_BYTES)?)
        })
        .ok_or_else(|| "record extent overflow".to_owned())?;
    if expected != record.len() {
        return Err("record frame/capability extents are not exact".to_owned());
    }
    if capability_count != received_fds {
        return Err("record capability count differs from SCM_RIGHTS count".to_owned());
    }
    if frame_length < 20 {
        return Err("canonical frame shorter than envelope header".to_owned());
    }
    Ok(())
}

fn packet_direction(record: &[u8]) -> u8 {
    *record
        .get(RECORD_DIRECTION_OFFSET)
        .expect("validated record direction")
}

fn packet_message_type(record: &[u8]) -> u16 {
    read_u16(record, RECORD_FRAME_OFFSET + FRAME_MESSAGE_TYPE_OFFSET)
        .expect("validated canonical message type")
}

fn packet_sender_pid(record: &[u8]) -> u32 {
    read_u32(record, RECORD_SENDER_PID_OFFSET).expect("validated sender PID")
}

fn rewrite_sender_pid(record: &mut [u8], sender_pid: u32) {
    assert_ne!(sender_pid, 0);
    record
        .get_mut(RECORD_SENDER_PID_OFFSET..RECORD_SENDER_PID_OFFSET + 4)
        .expect("validated sender PID field")
        .copy_from_slice(&sender_pid.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| format!("missing u16 at offset {offset}"))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| format!("missing u32 at offset {offset}"))
}

fn write_proxy_proof(argv_zero: &str) {
    let path = Path::new(argv_zero)
        .parent()
        .expect("fixture executable directory")
        .join(PROXY_PROOF);
    let mut proof = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("create one-shot proxy proof");
    proof
        .write_all(PROXY_PROOF_BYTES)
        .expect("write exact proxy proof");
    proof.flush().expect("flush proxy proof");
}

fn prove_sender_pid_rewrite_matches_public_encoding() {
    let limits = limits();
    let auth = DesktopLaunchAuth::new().expect("fresh record auth");
    let epoch = WorkerEpoch::new(7).expect("test worker epoch");
    let mut frame = Vec::new();
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&pdf_rs_protocol::MESSAGE_ID_GENERATION_PLANNED.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(&0_u32.to_le_bytes());
    frame.extend_from_slice(&1_u64.to_le_bytes());
    let original_pid = 0x1020_3040;
    let rewritten_pid = 0x5060_7080;
    let encode = |sender_pid| {
        let record = DesktopWireRecord::new(
            &auth,
            DesktopRecordBinding {
                direction: DesktopDirection::WorkerToHost,
                sender_pid,
                worker_epoch: epoch,
                sequence: 9,
            },
            frame.clone(),
            Vec::new(),
            limits,
        )
        .expect("public record");
        let mut encoded = Vec::new();
        record
            .write_to(&mut encoded, limits)
            .expect("public record encoding");
        encoded
    };
    let original = encode(original_pid);
    let expected = encode(rewritten_pid);
    validate_raw_record_layout(&original, 0).expect("public record matches proxy layout");
    assert_eq!(
        packet_message_type(&original),
        pdf_rs_protocol::MESSAGE_ID_GENERATION_PLANNED
    );
    assert_eq!(
        packet_direction(&original),
        DesktopDirection::WorkerToHost as u8
    );
    assert_eq!(packet_sender_pid(&original), original_pid);
    let mut actual = original.clone();
    rewrite_sender_pid(&mut actual, rewritten_pid);
    assert_eq!(actual, expected);
    assert_eq!(
        &original[RECORD_SENDER_PID_OFFSET..RECORD_SENDER_PID_OFFSET + 4],
        &original_pid.to_le_bytes()
    );
    assert!(
        original
            .iter()
            .zip(&expected)
            .enumerate()
            .all(|(index, (left, right))| {
                (RECORD_SENDER_PID_OFFSET..RECORD_SENDER_PID_OFFSET + 4).contains(&index)
                    || left == right
            }),
        "public encodings differ outside sender_pid"
    );
}

fn post_plan_proxy_disconnect_never_publishes_surface_and_restarts_once() {
    let limits = limits();
    let fixture = FixtureProgram::new();
    let before_spawn = open_fd_count();
    let config =
        DesktopSupervisorConfig::new(1, Duration::from_secs(2)).expect("supervisor config");
    let mut supervisor = DesktopChildSupervisor::start(
        fixture.path().to_str().expect("UTF-8 fixture path"),
        config,
        cleanup_resources(limits),
    )
    .expect("spawn supervised proxy");
    let old_worker = supervisor.worker_id().expect("old worker");
    let old_epoch = supervisor.worker_epoch().expect("old epoch");
    let active_fd_count = open_fd_count();
    let handshake = negotiate(&mut supervisor, limits);
    let decoder = DesktopFrameDecoder::for_handshake(handshake);
    let (session, geometry) = open_fixture_for_render(&mut supervisor, decoder, limits);
    track_old_epoch_resources(&mut supervisor, session, old_epoch, limits);
    assert_eq!(
        open_fd_count(),
        active_fd_count + 1,
        "tracked old-epoch FD is live before disconnect"
    );

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
    let mut saw_capability = false;
    let mut saw_generation_planned = false;
    for _ in 0..MAX_HOST_PREPUBLICATION_EVENTS {
        let (event, fds) = supervisor
            .receive_event(decoder, limits)
            .expect("real prepublication event");
        assert!(fds.is_empty(), "proxy forwarded an unexpected descriptor");
        match event.event {
            Event::CapabilityReported(_) => saw_capability = true,
            Event::GenerationPlanned(_) => {
                assert!(saw_capability, "plan preceded capability decision");
                saw_generation_planned = true;
                break;
            }
            Event::SurfaceReady(_) => panic!("proxy delivered Surface before disconnect"),
            other => panic!("unexpected prepublication event: {other:?}"),
        }
    }
    assert!(
        saw_generation_planned,
        "Host exceeded the hard event limit before GenerationPlanned"
    );

    let fault = expect_worker_fault(supervisor.receive_event(decoder, limits));
    assert_eq!(fault.kind(), DesktopWorkerFaultKind::UnexpectedEof);
    assert_eq!(fault.worker_id(), old_worker);
    assert_eq!(fault.worker_epoch(), old_epoch);
    assert_eq!(fault.restart_attempt(), Some(1));
    let replacement_epoch = fault.replacement_epoch().expect("replacement epoch");
    assert!(replacement_epoch.value() > old_epoch.value());
    assert_eq!(supervisor.restart_count(), 1);
    assert_eq!(supervisor.state(), DesktopSupervisorState::Running);
    assert_eq!(supervisor.worker_epoch(), Some(replacement_epoch));
    assert_eq!(supervisor.cleanup().capabilities.live_count(), 0);
    assert_eq!(supervisor.cleanup().capabilities.retained_bytes(), 0);
    assert_eq!(
        supervisor
            .cleanup()
            .bridge
            .as_ref()
            .expect("Range bridge")
            .outstanding(),
        0
    );
    assert!(supervisor.cleanup().epoch_fds.is_empty());
    assert_eq!(supervisor.cleanup().retired, vec![(old_worker, old_epoch)]);
    assert_eq!(
        std::fs::read(fixture.proof_path()).expect("read proxy proof"),
        PROXY_PROOF_BYTES
    );
    assert_eq!(
        open_fd_count(),
        active_fd_count,
        "old epoch or proxy leaked a Host FD"
    );

    let replacement_handshake = negotiate(&mut supervisor, limits);
    let replacement_decoder = DesktopFrameDecoder::for_handshake(replacement_handshake);
    supervisor
        .begin_graceful_shutdown()
        .expect("begin replacement graceful shutdown");
    send_command(
        &mut supervisor,
        3,
        Command::Shutdown(ShutdownCommand { deadline_ms: 1_000 }),
        limits,
    );
    let mut acknowledged = false;
    let mut stopped = false;
    for _ in 0..MAX_SHUTDOWN_EVENTS {
        let (event, fds) = supervisor
            .receive_event(replacement_decoder, limits)
            .expect("replacement graceful terminal event");
        assert!(fds.is_empty(), "graceful shutdown transferred an FD");
        match event.event {
            Event::ShutdownAcknowledged(_) => acknowledged = true,
            Event::WorkerStopped(_) => {
                assert!(
                    acknowledged,
                    "WorkerStopped preceded ShutdownAcknowledged on replacement"
                );
                stopped = true;
                break;
            }
            other => panic!("unexpected replacement shutdown event: {other:?}"),
        }
    }
    assert!(acknowledged, "replacement did not acknowledge Shutdown");
    assert!(stopped, "replacement did not report WorkerStopped");
    supervisor.complete_graceful_shutdown();
    assert_eq!(supervisor.state(), DesktopSupervisorState::Stopped);
    assert_eq!(supervisor.worker_epoch(), None);
    assert_eq!(supervisor.restart_count(), 1);
    assert_eq!(supervisor.cleanup().retired.len(), 2);
    drop(supervisor);
    assert_eq!(
        open_fd_count(),
        before_spawn,
        "proxy fixture leaked a Host FD after shutdown"
    );
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
        bridge: Some(HostRangeBridge::new(snapshot, limits)),
        epoch_fds: Vec::new(),
        retired: Vec::new(),
    }
}

fn track_old_epoch_resources(
    supervisor: &mut DesktopChildSupervisor<TrackedResources>,
    session: SessionId,
    epoch: WorkerEpoch,
    limits: DesktopIpcLimits,
) {
    let tracked = DesktopCapability::new(
        101,
        CapabilityClass::SourceSegment,
        CapabilityRights::ReadOnly,
        session,
        epoch,
        4,
    )
    .expect("tracked render capability");
    let resources = supervisor.cleanup_mut();
    resources
        .capabilities
        .insert(tracked)
        .expect("track current render capability");
    resources
        .bridge
        .as_mut()
        .expect("Range bridge")
        .register(
            DataTicket::new(101),
            session,
            epoch,
            vec![ByteRange { start: 0, len: 4 }],
        )
        .expect("track current render source range");
    resources.epoch_fds.push(
        ReadOnlySharedRegion::from_bytes(b"render-owned", limits)
            .expect("tracked render FD")
            .into_fd(),
    );
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
                clip_width_milli_points: 2_048_000,
                clip_height_milli_points: 2_048_000,
            }],
            quality: QualityPolicy::Full,
            output_profile: OutputProfile::Srgb,
            device_scale_milli: 1_000,
            rotation: PageRotation::Degrees0,
            optional_content_id: 1,
        },
    }
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
        _ => panic!("unsupported proxy test command"),
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

fn expect_worker_fault<T>(result: Result<T, DesktopSupervisionError>) -> DesktopWorkerFault {
    match result {
        Err(DesktopSupervisionError::WorkerFault(fault)) => fault,
        Err(other) => panic!("expected WorkerFault, got {other:?}"),
        Ok(_) => panic!("Surface or another event escaped the post-plan proxy"),
    }
}

fn open_fd_count() -> usize {
    ["/proc/self/fd", "/dev/fd"]
        .into_iter()
        .find_map(|path| read_dir(path).ok().map(Iterator::count))
        .expect("platform exposes process descriptor directory")
}
