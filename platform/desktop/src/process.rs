use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use pdf_rs_protocol::{
    Command as ProtocolCommand, CommandEnvelope, DesktopFrameDecoder, ENVELOPE_HEADER_BYTES,
    EndpointCapabilities, EndpointRole, Event, EventEnvelope, HandshakeFrameDecoder,
    KNOWN_ENDPOINT_CAPABILITIES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, PayloadCodecLimits, ProtocolHello, ProtocolLimits, ProtocolValidator,
    SequenceTracker, SurfaceTransport, WorkerId, encode_cancel_acknowledged_event_payload,
    encode_capability_reported_event_payload, encode_close_session_acknowledged_event_payload,
    encode_correlation_payload, encode_data_failed_event_payload,
    encode_document_ready_event_payload, encode_engine_hello_event_payload,
    encode_generation_completed_event_payload, encode_generation_planned_event_payload,
    encode_need_data_event_payload, encode_page_metrics_event_payload,
    encode_protocol_fault_event_payload, encode_ready_event_payload,
    encode_request_cancelled_event_payload, encode_request_failed_event_payload,
    encode_session_closed_event_payload, encode_shutdown_acknowledged_event_payload,
    encode_surface_ready_event_payload, encode_surface_reclaimed_event_payload,
    encode_surface_release_acknowledged_event_payload, encode_worker_fault_event_payload,
    encode_worker_stopped_event_payload,
};
use pdf_rs_surface::WorkerEpoch;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use crate::{
    CapabilityClass, CapabilityRights, DesktopCapability, DesktopDirection, DesktopIpcError,
    DesktopIpcErrorCode, DesktopIpcLimits, DesktopLaunchAuth, DesktopLaunchId,
    DesktopRecordBinding, DesktopWireRecord, ReadOnlySharedRegion,
    error::error,
    native_adapter::{
        DesktopNativeEvent, DesktopNativePoll, DesktopNativeWorker, NativeDesktopPhase,
    },
    receive_capability_fds, send_capability_fds,
    unix::{read_read_only_fd, try_receive_capability_fds, wait_receive_capability_fds},
    validate_read_only_fd,
};

// Serializes this crate's socketpair-to-exec interval on platforms without
// SOCK_CLOEXEC, so another desktop worker cannot inherit a sibling endpoint.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// One Host-owned child process epoch. Restart drops every old transport resource.
pub struct DesktopHostProcess {
    child: Option<Child>,
    socket: Option<UnixStream>,
    auth: DesktopLaunchAuth,
    epoch: WorkerEpoch,
    worker: WorkerId,
    host_pid: u32,
    last_sent: Option<u64>,
    last_received: Option<u64>,
    canonical_received: SequenceTracker,
}

/// Host-owned monotonic worker epoch allocator.
pub struct DesktopEpochManager {
    next_epoch: u64,
    next_worker: u64,
}

impl DesktopEpochManager {
    /// Starts allocation at the first nonzero epoch.
    pub const fn new() -> Self {
        Self {
            next_epoch: 1,
            next_worker: 1,
        }
    }
    /// Spawns a new isolated worker with an epoch never reused by this manager.
    pub fn spawn(&mut self, program: &str) -> Result<DesktopHostProcess, DesktopIpcError> {
        let epoch = WorkerEpoch::new(self.next_epoch)
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        let worker = WorkerId::new(self.next_worker);
        let process = DesktopHostProcess::spawn_for_epoch(program, epoch, worker)?;
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        self.next_worker = self
            .next_worker
            .checked_add(1)
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        Ok(process)
    }
}

impl Default for DesktopEpochManager {
    fn default() -> Self {
        Self::new()
    }
}

/// A received record that must validate canonical semantics before it can commit.
pub struct PendingDesktopRecord<'a> {
    host: &'a mut DesktopHostProcess,
    record: Option<DesktopWireRecord>,
    fds: Option<Vec<OwnedFd>>,
    validated: bool,
}

impl PendingDesktopRecord<'_> {
    /// Validates and commits the generated handshake sequence before dispatch.
    pub fn validate_handshake(&mut self) -> Result<(), DesktopIpcError> {
        let Some(record) = self.record.as_ref() else {
            return Err(error(DesktopIpcErrorCode::Lifecycle));
        };
        let result = crate::validate_engine_hello_event(
            record.frame(),
            self.fds.as_ref().map_or(0, Vec::len),
            self.host.worker,
            &mut self.host.canonical_received,
        );
        if result.is_err() {
            self.host.poison();
        }
        result?;
        self.validated = true;
        Ok(())
    }

    /// Decodes and transactionally commits one generated handshake event.
    pub fn decode_handshake_event(&mut self) -> Result<EventEnvelope, DesktopIpcError> {
        let pending = {
            let record = self
                .record
                .as_ref()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
            HandshakeFrameDecoder::new(ProtocolLimits::default())
                .prepare(
                    record.frame(),
                    self.fds.as_ref().map_or(0, Vec::len),
                    &self.host.canonical_received,
                )
                .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?
        };
        let envelope = pending
            .decode_event()
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        validate_received_event(
            &envelope,
            self.record
                .as_ref()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?
                .capabilities(),
            self.host.worker,
            self.host.epoch,
        )?;
        validate_received_handshake_event(&envelope)?;
        pending
            .commit(&mut self.host.canonical_received)
            .map_err(|_| error(DesktopIpcErrorCode::Sequence))?;
        self.validated = true;
        Ok(envelope)
    }

    /// Decodes and transactionally commits one negotiated generated event.
    pub fn decode_event(
        &mut self,
        decoder: DesktopFrameDecoder,
    ) -> Result<EventEnvelope, DesktopIpcError> {
        let pending = {
            let record = self
                .record
                .as_ref()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
            decoder
                .prepare(
                    record.frame(),
                    self.fds.as_ref().map_or(0, Vec::len),
                    &self.host.canonical_received,
                )
                .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?
        };
        let envelope = pending
            .decode_event()
            .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        validate_received_event(
            &envelope,
            self.record
                .as_ref()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?
                .capabilities(),
            self.host.worker,
            self.host.epoch,
        )?;
        pending
            .commit(&mut self.host.canonical_received)
            .map_err(|_| error(DesktopIpcErrorCode::Sequence))?;
        self.validated = true;
        Ok(envelope)
    }

    /// Commits the outer transport sequence and consumes the immutable payload.
    pub fn commit(mut self) -> Result<(Vec<u8>, Vec<OwnedFd>), DesktopIpcError> {
        if !self.validated {
            self.host.poison();
            return Err(error(DesktopIpcErrorCode::InvalidFrame));
        }
        let record = self
            .record
            .take()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
        let child_pid = match self.host.child_pid() {
            Ok(pid) => pid,
            Err(failure) => {
                self.host.poison();
                return Err(failure);
            }
        };
        let result = record
            .authenticate(
                &self.host.auth,
                child_pid,
                DesktopDirection::WorkerToHost,
                self.host.epoch,
                &mut self.host.last_received,
            )
            .and_then(|()| record.commit_outer_sequence(&mut self.host.last_received));
        if let Err(failure) = result {
            self.host.poison();
            return Err(failure);
        }
        self.validated = false;
        Ok((record.into_frame(), self.fds.take().unwrap_or_default()))
    }
}

impl Drop for PendingDesktopRecord<'_> {
    fn drop(&mut self) {
        if self.record.is_some() {
            self.host.poison();
        }
    }
}

impl DesktopHostProcess {
    /// Returns this process's manager-issued immutable worker epoch.
    pub const fn worker_epoch(&self) -> WorkerEpoch {
        self.epoch
    }
    /// Returns this process's canonical protocol worker identity.
    pub const fn worker_id(&self) -> WorkerId {
        self.worker
    }
    /// Spawns a child with one private inherited Unix socket endpoint.
    ///
    /// The launch identity, Host PID, epoch, and secret travel only through
    /// that endpoint; none are placed in argv or the environment.
    fn spawn_for_epoch(
        program: &str,
        epoch: WorkerEpoch,
        worker: WorkerId,
    ) -> Result<Self, DesktopIpcError> {
        let _spawn_guard = SPAWN_LOCK
            .lock()
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let auth = DesktopLaunchAuth::new()?;
        let host_pid = std::process::id();
        let (host_fd, child_fd) = socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::empty(),
            None,
        )
        .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        // macOS lacks SOCK_CLOEXEC.  Mark both originals close-on-exec before
        // Stdio performs its controlled dup2 into child stdin/stdout.
        rustix::io::fcntl_setfd(&host_fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let child_stdout =
            rustix::io::dup(&child_fd).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        rustix::io::fcntl_setfd(&child_fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        rustix::io::fcntl_setfd(&child_stdout, rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let mut child = Command::new(program)
            .arg("--pdf-rs-desktop-child")
            .stdin(Stdio::from(child_fd))
            .stdout(Stdio::from(child_stdout))
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let mut socket = UnixStream::from(host_fd);
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let bootstrap = (|| {
            socket
                .write_all(&auth.launch().value().to_le_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
            socket
                .write_all(auth.token())
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
            socket
                .write_all(&host_pid.to_le_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
            socket
                .write_all(&epoch.value().to_le_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
            socket
                .write_all(&worker.value().to_le_bytes())
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
            socket
                .flush()
                .map_err(|_| error(DesktopIpcErrorCode::Disconnected))
        })();
        if let Err(failure) = bootstrap {
            drop(socket);
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure);
        }
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(_)) => {
                drop(socket);
                let _ = child.wait();
                return Err(error(DesktopIpcErrorCode::Lifecycle));
            }
            Err(_) => {
                drop(socket);
                let _ = child.kill();
                let _ = child.wait();
                return Err(error(DesktopIpcErrorCode::Lifecycle));
            }
        }
        Ok(Self {
            child: Some(child),
            socket: Some(socket),
            auth,
            epoch,
            worker,
            host_pid,
            last_sent: None,
            last_received: None,
            canonical_received: SequenceTracker::new(),
        })
    }

    /// Builds a Host-originated record with this process's private launch proof.
    pub fn new_host_record(
        &self,
        sequence: u64,
        frame: Vec<u8>,
        capabilities: Vec<crate::DesktopCapability>,
        limits: DesktopIpcLimits,
    ) -> Result<DesktopWireRecord, DesktopIpcError> {
        DesktopWireRecord::new(
            &self.auth,
            DesktopRecordBinding {
                direction: DesktopDirection::HostToWorker,
                sender_pid: self.host_pid,
                worker_epoch: self.epoch,
                sequence,
            },
            frame,
            capabilities,
            limits,
        )
    }

    /// Sends one Host-to-child authenticated record and its exact immutable FDs.
    pub fn send(
        &mut self,
        record: &DesktopWireRecord,
        fds: &[OwnedFd],
        limits: DesktopIpcLimits,
    ) -> Result<(), DesktopIpcError> {
        let outcome = (|| {
            if record.capabilities().len() != fds.len() {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            record.authenticate(
                &self.auth,
                self.host_pid,
                DesktopDirection::HostToWorker,
                self.epoch,
                &mut self.last_sent,
            )?;
            let socket = self
                .socket
                .as_mut()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
            send_capability_fds(&*socket, fds, limits)?;
            record.write_to(socket, limits)?;
            record.commit_outer_sequence(&mut self.last_sent)
        })();
        if outcome.is_err() {
            self.poison();
        }
        outcome
    }

    /// Receives one immutable pending record. Dropping it without commit poisons the worker.
    pub fn receive(
        &mut self,
        limits: DesktopIpcLimits,
    ) -> Result<PendingDesktopRecord<'_>, DesktopIpcError> {
        let outcome = (|| {
            let child_pid = self.child_pid()?;
            let socket = self
                .socket
                .as_mut()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
            let fds = receive_capability_fds(&*socket, limits)?;
            let record = DesktopWireRecord::read_authenticated_from(
                socket,
                limits,
                &self.auth,
                child_pid,
                DesktopDirection::WorkerToHost,
                self.epoch,
                &mut self.last_received,
            )?;
            if record.capabilities().len() != fds.len() {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            for (descriptor, fd) in record.capabilities().iter().zip(&fds) {
                validate_read_only_fd(fd, descriptor.byte_length())?;
            }
            Ok((record, fds))
        })();
        if outcome.is_err() {
            self.poison();
        }
        outcome.map(|(record, fds)| PendingDesktopRecord {
            host: self,
            record: Some(record),
            fds: Some(fds),
            validated: false,
        })
    }

    fn child_pid(&self) -> Result<u32, DesktopIpcError> {
        self.child
            .as_ref()
            .map(Child::id)
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))
    }

    /// Terminates the child and invalidates every remaining old-epoch transport path.
    pub fn shutdown(&mut self) {
        self.socket.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.last_sent = None;
        self.last_received = None;
    }

    fn poison(&mut self) {
        self.shutdown();
    }
}

impl Drop for DesktopHostProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Runs the isolated authenticated child transport and bounded Native actor loop.
pub fn run_child_stdio(limits: DesktopIpcLimits) -> Result<(), DesktopIpcError> {
    let run = catch_unwind(AssertUnwindSafe(|| {
        // `StdinLock` may prefetch stream bytes while reading bootstrap data.
        // Use duplicate unbuffered files so SCM_RIGHTS stays attached to the
        // exact marker that follows the fixed bootstrap.
        let input_fd =
            rustix::io::dup(std::io::stdin()).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        rustix::io::fcntl_setfd(&input_fd, rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        rustix::io::fcntl_setfd(std::io::stdin(), rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        rustix::io::fcntl_setfd(std::io::stdout(), rustix::io::FdFlags::CLOEXEC)
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let mut input = UnixStream::from(input_fd);
        input
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        let mut launch = [0_u8; 8];
        let mut token = [0_u8; 32];
        let mut host_pid = [0_u8; 4];
        let mut epoch = [0_u8; 8];
        let mut worker = [0_u8; 8];
        input
            .read_exact(&mut launch)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        input
            .read_exact(&mut token)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        input
            .read_exact(&mut host_pid)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        input
            .read_exact(&mut epoch)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        input
            .read_exact(&mut worker)
            .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
        let launch = DesktopLaunchId::from_bootstrap(u64::from_le_bytes(launch))
            .ok_or_else(|| error(DesktopIpcErrorCode::Authentication))?;
        let auth = DesktopLaunchAuth::from_bootstrap(launch, token)?;
        let host_pid = u32::from_le_bytes(host_pid);
        let epoch = WorkerEpoch::new(u64::from_le_bytes(epoch))
            .ok_or_else(|| error(DesktopIpcErrorCode::Authentication))?;
        let worker = WorkerId::new(u64::from_le_bytes(worker));
        if host_pid == 0 {
            return Err(error(DesktopIpcErrorCode::Authentication));
        }
        run_authenticated_child(input, limits, auth, host_pid, epoch, worker)
    }));
    match run {
        Ok(result) => result,
        Err(_) => Err(error(DesktopIpcErrorCode::ChildPanic)),
    }
}

fn run_authenticated_child(
    mut socket: UnixStream,
    limits: DesktopIpcLimits,
    auth: DesktopLaunchAuth,
    host_pid: u32,
    epoch: WorkerEpoch,
    worker: WorkerId,
) -> Result<(), DesktopIpcError> {
    let mut incoming = None;
    let mut outgoing = None;
    let mut canonical_incoming = SequenceTracker::new();
    let mut canonical_outgoing = SequenceTracker::new();
    let mut native = DesktopNativeWorker::new(worker, epoch, limits)?;
    let mut next_outgoing_sequence = 1_u64;
    let mut next_capability = 1_u64;
    loop {
        if let Some(fds) = try_receive_capability_fds(&socket, limits)? {
            receive_and_dispatch_command(
                &mut socket,
                fds,
                limits,
                &auth,
                host_pid,
                epoch,
                worker,
                &mut incoming,
                &mut canonical_incoming,
                &mut native,
            )?;
        }
        match native.poll()? {
            DesktopNativePoll::Event(event) => {
                send_native_event(
                    &mut socket,
                    &auth,
                    epoch,
                    worker,
                    event,
                    limits,
                    &mut next_outgoing_sequence,
                    &mut next_capability,
                    &mut outgoing,
                    &mut canonical_outgoing,
                    native.handshake(),
                )?;
                if native.phase() == NativeDesktopPhase::Stopped {
                    return Ok(());
                }
            }
            DesktopNativePoll::Progressed => {}
            DesktopNativePoll::Idle => {
                let fds = match wait_receive_capability_fds(&socket, limits) {
                    Ok(Some(fds)) => fds,
                    Ok(None) => continue,
                    Err(failure) if failure.code() == DesktopIpcErrorCode::Disconnected => {
                        return Ok(());
                    }
                    Err(failure) => return Err(failure),
                };
                receive_and_dispatch_command(
                    &mut socket,
                    fds,
                    limits,
                    &auth,
                    host_pid,
                    epoch,
                    worker,
                    &mut incoming,
                    &mut canonical_incoming,
                    &mut native,
                )?;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn receive_and_dispatch_command(
    socket: &mut UnixStream,
    fds: Vec<OwnedFd>,
    limits: DesktopIpcLimits,
    auth: &DesktopLaunchAuth,
    host_pid: u32,
    epoch: WorkerEpoch,
    worker: WorkerId,
    outer_sequence: &mut Option<u64>,
    canonical_sequence: &mut SequenceTracker,
    native: &mut DesktopNativeWorker,
) -> Result<(), DesktopIpcError> {
    let record = DesktopWireRecord::read_authenticated_from(
        socket,
        limits,
        auth,
        host_pid,
        DesktopDirection::HostToWorker,
        epoch,
        outer_sequence,
    )?;
    if record.capabilities().len() != fds.len() {
        return Err(error(DesktopIpcErrorCode::Capability));
    }
    for (descriptor, fd) in record.capabilities().iter().zip(&fds) {
        validate_read_only_fd(fd, descriptor.byte_length())?;
    }
    let pending = match native.phase() {
        NativeDesktopPhase::Starting | NativeDesktopPhase::AwaitingAccept => {
            HandshakeFrameDecoder::new(ProtocolLimits::default()).prepare(
                record.frame(),
                fds.len(),
                canonical_sequence,
            )
        }
        NativeDesktopPhase::Ready => {
            let handshake = native
                .handshake()
                .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
            DesktopFrameDecoder::for_handshake(handshake).prepare(
                record.frame(),
                fds.len(),
                canonical_sequence,
            )
        }
        NativeDesktopPhase::Stopped => return Err(error(DesktopIpcErrorCode::Lifecycle)),
    }
    .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let command = pending
        .decode_command()
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    validate_child_command_capabilities(&command, record.capabilities(), worker, epoch)?;

    let mut transfers = Vec::new();
    transfers
        .try_reserve_exact(fds.len())
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    for (descriptor, fd) in record.capabilities().iter().zip(&fds) {
        transfers.push(read_read_only_fd(fd, descriptor.byte_length(), limits)?);
    }

    // All outer credentials, canonical values, transfer bindings, FD rights,
    // extents, and bounded copies are proven before either direction-local
    // sequence commits or actor state changes.
    pending
        .commit(canonical_sequence)
        .map_err(|_| error(DesktopIpcErrorCode::Sequence))?;
    record.commit_outer_sequence(outer_sequence)?;
    native.handle_command(command, &transfers)
}

fn validate_child_command_capabilities(
    envelope: &CommandEnvelope,
    capabilities: &[DesktopCapability],
    worker: WorkerId,
    epoch: WorkerEpoch,
) -> Result<(), DesktopIpcError> {
    ProtocolValidator::new(ProtocolLimits::default())
        .validate_command_payload_correlation(envelope, worker, envelope.correlation.session)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    match &envelope.command {
        ProtocolCommand::ProvideData(command) => {
            let session = envelope
                .correlation
                .session
                .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?;
            if capabilities.len() != command.segments.len() {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            for (index, (segment, capability)) in
                command.segments.iter().zip(capabilities).enumerate()
            {
                if usize::from(segment.slot) != index
                    || capability.class() != CapabilityClass::SourceSegment
                    || capability.rights() != CapabilityRights::ReadOnly
                    || capability.owner() != session
                    || capability.worker_epoch() != epoch
                    || capability.byte_length() != segment.byte_length
                    || segment.byte_length != segment.range.len
                {
                    return Err(error(DesktopIpcErrorCode::Capability));
                }
            }
        }
        _ if !capabilities.is_empty() => return Err(error(DesktopIpcErrorCode::Capability)),
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn send_native_event(
    socket: &mut UnixStream,
    auth: &DesktopLaunchAuth,
    epoch: WorkerEpoch,
    worker: WorkerId,
    event: DesktopNativeEvent,
    limits: DesktopIpcLimits,
    next_sequence: &mut u64,
    next_capability: &mut u64,
    last_outer_sequence: &mut Option<u64>,
    canonical_sequence: &mut SequenceTracker,
    handshake: Option<pdf_rs_protocol::CompatibleHandshake>,
) -> Result<(), DesktopIpcError> {
    let sequence = *next_sequence;
    let frame = encode_event_frame(sequence, &event.correlation, &event.event)?;
    let mut capabilities = Vec::new();
    let mut fds = Vec::new();
    match (
        event.shared_region,
        event.shared_memory_budget,
        &event.event,
    ) {
        (Some(bytes), Some(shared_memory_budget), Event::SurfaceReady(surface)) => {
            let session = event
                .correlation
                .session
                .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
            let SurfaceTransport::SharedMemory {
                slot,
                region_length,
            } = surface.transport
            else {
                return Err(error(DesktopIpcErrorCode::Capability));
            };
            if slot != 0
                || region_length
                    != u64::try_from(bytes.len())
                        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                || surface.metadata.owner.worker != worker
                || surface.metadata.owner.session != session
                || u64::try_from(bytes.len())
                    .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?
                    > shared_memory_budget
            {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            let region = ReadOnlySharedRegion::from_bytes(&bytes, limits)?;
            let capability = DesktopCapability::new(
                *next_capability,
                CapabilityClass::SurfaceRegion,
                CapabilityRights::ReadOnly,
                session,
                epoch,
                region.byte_length(),
            )?;
            capabilities.push(capability);
            fds.push(region.into_fd());
        }
        (None, None, Event::SurfaceReady(_))
        | (Some(_), None, _)
        | (None, Some(_), _)
        | (Some(_), Some(_), _) => {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        (None, None, _) => {}
    }
    validate_outgoing_event(
        &frame,
        &capabilities,
        worker,
        epoch,
        canonical_sequence,
        handshake,
    )?;
    let record = DesktopWireRecord::new(
        auth,
        DesktopRecordBinding {
            direction: DesktopDirection::WorkerToHost,
            sender_pid: std::process::id(),
            worker_epoch: epoch,
            sequence,
        },
        frame,
        capabilities,
        limits,
    )?;
    send_capability_fds(&*socket, &fds, limits)?;
    record.write_to(socket, limits)?;
    record.commit_outer_sequence(last_outer_sequence)?;
    *next_sequence = sequence
        .checked_add(1)
        .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    if !fds.is_empty() {
        *next_capability = next_capability
            .checked_add(1)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    }
    Ok(())
}

fn validate_outgoing_event(
    frame: &[u8],
    capabilities: &[DesktopCapability],
    worker: WorkerId,
    epoch: WorkerEpoch,
    sequence: &mut SequenceTracker,
    handshake: Option<pdf_rs_protocol::CompatibleHandshake>,
) -> Result<(), DesktopIpcError> {
    let message_type = u16::from_le_bytes(
        frame
            .get(4..6)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| error(DesktopIpcErrorCode::InvalidFrame))?,
    );
    let pending = if matches!(
        message_type,
        pdf_rs_protocol::MESSAGE_ID_ENGINE_HELLO | pdf_rs_protocol::MESSAGE_ID_READY
    ) {
        HandshakeFrameDecoder::new(ProtocolLimits::default()).prepare(
            frame,
            capabilities.len(),
            sequence,
        )
    } else {
        DesktopFrameDecoder::for_handshake(
            handshake.ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?,
        )
        .prepare(frame, capabilities.len(), sequence)
    }
    .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let envelope = pending
        .decode_event()
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    validate_received_event(&envelope, capabilities, worker, epoch)?;
    if matches!(
        message_type,
        pdf_rs_protocol::MESSAGE_ID_ENGINE_HELLO | pdf_rs_protocol::MESSAGE_ID_READY
    ) {
        validate_received_handshake_event(&envelope)?;
    }
    pending
        .commit(sequence)
        .map(|_| ())
        .map_err(|_| error(DesktopIpcErrorCode::Sequence))
}

fn validate_received_event(
    envelope: &EventEnvelope,
    capabilities: &[DesktopCapability],
    worker: WorkerId,
    epoch: WorkerEpoch,
) -> Result<(), DesktopIpcError> {
    ProtocolValidator::new(ProtocolLimits::default())
        .validate_event_payload_correlation(envelope, worker, envelope.correlation.session)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    match &envelope.event {
        Event::SurfaceReady(surface) => {
            let descriptor = capabilities
                .first()
                .filter(|_| capabilities.len() == 1)
                .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
            let session = envelope
                .correlation
                .session
                .ok_or_else(|| error(DesktopIpcErrorCode::Capability))?;
            let SurfaceTransport::SharedMemory {
                slot,
                region_length,
            } = surface.transport
            else {
                return Err(error(DesktopIpcErrorCode::Capability));
            };
            let range_end = surface
                .metadata
                .byte_offset
                .checked_add(surface.metadata.byte_length)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
            let layout_bytes = u64::from(surface.metadata.stride)
                .checked_mul(u64::from(surface.metadata.height))
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
            if slot != 0
                || descriptor.class() != CapabilityClass::SurfaceRegion
                || descriptor.rights() != CapabilityRights::ReadOnly
                || descriptor.owner() != session
                || descriptor.worker_epoch() != epoch
                || descriptor.byte_length() != region_length
                || surface.metadata.owner.worker != worker
                || surface.metadata.owner.session != session
                || surface.metadata.byte_length != layout_bytes
                || range_end > region_length
            {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
        }
        _ if !capabilities.is_empty() => return Err(error(DesktopIpcErrorCode::Capability)),
        _ => {}
    }
    Ok(())
}

fn validate_received_handshake_event(envelope: &EventEnvelope) -> Result<(), DesktopIpcError> {
    match &envelope.event {
        Event::EngineHello(engine_hello) => {
            let host = ProtocolHello {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
                schema_hash: pdf_rs_protocol::SCHEMA_HASH,
                endpoint_role: EndpointRole::Host,
                capabilities: EndpointCapabilities {
                    supported: KNOWN_ENDPOINT_CAPABILITIES,
                    mandatory: 0,
                },
                max_message_bytes: MAX_MESSAGE_BYTES,
                max_transfer_slots: MAX_TRANSFER_SLOTS,
            };
            ProtocolValidator::new(ProtocolLimits::default())
                .validate_handshake(&host, &engine_hello.hello)
                .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
        }
        Event::Ready(ready)
            if ready.worker == envelope.correlation.worker
                && ready.schema_hash == pdf_rs_protocol::SCHEMA_HASH
                && ready.negotiated_minor == PROTOCOL_MINOR => {}
        Event::ProtocolFault(_) => {}
        _ => return Err(error(DesktopIpcErrorCode::InvalidFrame)),
    }
    Ok(())
}

fn encode_event_frame(
    sequence: u64,
    correlation: &pdf_rs_protocol::Correlation,
    event: &Event,
) -> Result<Vec<u8>, DesktopIpcError> {
    let limits = PayloadCodecLimits::protocol_default();
    let mut payload = encode_correlation_payload(correlation, limits)
        .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    let event_payload = match event {
        Event::Ready(value) => encode_ready_event_payload(value, limits),
        Event::NeedData(value) => encode_need_data_event_payload(value, limits),
        Event::DocumentReady(value) => encode_document_ready_event_payload(value, limits),
        Event::CapabilityReported(value) => encode_capability_reported_event_payload(value, limits),
        Event::SurfaceReady(value) => encode_surface_ready_event_payload(value, limits),
        Event::RequestCancelled(value) => encode_request_cancelled_event_payload(value, limits),
        Event::RequestFailed(value) => encode_request_failed_event_payload(value, limits),
        Event::SessionClosed(value) => encode_session_closed_event_payload(value, limits),
        Event::WorkerStopped(value) => encode_worker_stopped_event_payload(value, limits),
        Event::WorkerFault(value) => encode_worker_fault_event_payload(value, limits),
        Event::ProtocolFault(value) => encode_protocol_fault_event_payload(value, limits),
        Event::SurfaceReclaimed(value) => encode_surface_reclaimed_event_payload(value, limits),
        Event::EngineHello(value) => encode_engine_hello_event_payload(value, limits),
        Event::DataFailed(value) => encode_data_failed_event_payload(value, limits),
        Event::PageMetrics(value) => encode_page_metrics_event_payload(value, limits),
        Event::GenerationPlanned(value) => encode_generation_planned_event_payload(value, limits),
        Event::GenerationCompleted(value) => {
            encode_generation_completed_event_payload(value, limits)
        }
        Event::CancelAcknowledged(value) => encode_cancel_acknowledged_event_payload(value, limits),
        Event::SurfaceReleaseAcknowledged(value) => {
            encode_surface_release_acknowledged_event_payload(value, limits)
        }
        Event::CloseSessionAcknowledged(value) => {
            encode_close_session_acknowledged_event_payload(value, limits)
        }
        Event::ShutdownAcknowledged(value) => {
            encode_shutdown_acknowledged_event_payload(value, limits)
        }
    }
    .map_err(|_| error(DesktopIpcErrorCode::InvalidFrame))?;
    payload
        .try_reserve_exact(event_payload.len())
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    payload.extend_from_slice(&event_payload);
    let message_type = event_message_type(event);
    let payload_len =
        u32::try_from(payload.len()).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    let total = ENVELOPE_HEADER_BYTES
        .checked_add(payload.len())
        .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(total)
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    frame.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    frame.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    frame.extend_from_slice(&message_type.to_le_bytes());
    frame.extend_from_slice(&0_u16.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn event_message_type(event: &Event) -> u16 {
    match event {
        Event::Ready(_) => pdf_rs_protocol::MESSAGE_ID_READY,
        Event::NeedData(_) => pdf_rs_protocol::MESSAGE_ID_NEED_DATA,
        Event::DocumentReady(_) => pdf_rs_protocol::MESSAGE_ID_DOCUMENT_READY,
        Event::CapabilityReported(_) => pdf_rs_protocol::MESSAGE_ID_CAPABILITY_REPORTED,
        Event::SurfaceReady(_) => pdf_rs_protocol::MESSAGE_ID_SURFACE_READY,
        Event::RequestCancelled(_) => pdf_rs_protocol::MESSAGE_ID_REQUEST_CANCELLED,
        Event::RequestFailed(_) => pdf_rs_protocol::MESSAGE_ID_REQUEST_FAILED,
        Event::SessionClosed(_) => pdf_rs_protocol::MESSAGE_ID_SESSION_CLOSED,
        Event::WorkerStopped(_) => pdf_rs_protocol::MESSAGE_ID_WORKER_STOPPED,
        Event::WorkerFault(_) => pdf_rs_protocol::MESSAGE_ID_WORKER_FAULT,
        Event::ProtocolFault(_) => pdf_rs_protocol::MESSAGE_ID_PROTOCOL_FAULT,
        Event::SurfaceReclaimed(_) => pdf_rs_protocol::MESSAGE_ID_SURFACE_RECLAIMED,
        Event::EngineHello(_) => pdf_rs_protocol::MESSAGE_ID_ENGINE_HELLO,
        Event::DataFailed(_) => pdf_rs_protocol::MESSAGE_ID_DATA_FAILED,
        Event::PageMetrics(_) => pdf_rs_protocol::MESSAGE_ID_PAGE_METRICS,
        Event::GenerationPlanned(_) => pdf_rs_protocol::MESSAGE_ID_GENERATION_PLANNED,
        Event::GenerationCompleted(_) => pdf_rs_protocol::MESSAGE_ID_GENERATION_COMPLETED,
        Event::CancelAcknowledged(_) => pdf_rs_protocol::MESSAGE_ID_CANCEL_ACKNOWLEDGED,
        Event::SurfaceReleaseAcknowledged(_) => {
            pdf_rs_protocol::MESSAGE_ID_SURFACE_RELEASE_ACKNOWLEDGED
        }
        Event::CloseSessionAcknowledged(_) => {
            pdf_rs_protocol::MESSAGE_ID_CLOSE_SESSION_ACKNOWLEDGED
        }
        Event::ShutdownAcknowledged(_) => pdf_rs_protocol::MESSAGE_ID_SHUTDOWN_ACKNOWLEDGED,
    }
}

#[cfg(test)]
mod tests {
    use super::run_authenticated_child;
    use crate::{
        DesktopIpcErrorCode, DesktopIpcLimitConfig, DesktopIpcLimits, DesktopLaunchAuth,
        send_capability_fds,
    };
    use pdf_rs_protocol::WorkerId;
    use pdf_rs_surface::WorkerEpoch;
    use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn partial_authenticated_record_stall_exits_at_record_timeout() {
        let limits =
            DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).expect("desktop limits");
        let auth = DesktopLaunchAuth::new().expect("launch auth");
        let child_auth = DesktopLaunchAuth::from_bootstrap(auth.launch(), *auth.token())
            .expect("child launch auth");
        let (host_fd, child_fd) = socketpair(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::empty(),
            None,
        )
        .expect("socketpair");
        let mut host = UnixStream::from(host_fd);
        let child = UnixStream::from(child_fd);
        child
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("record timeout");
        child
            .set_write_timeout(Some(Duration::from_millis(25)))
            .expect("write timeout");
        let epoch = WorkerEpoch::new(1).expect("epoch");
        let handle = std::thread::spawn(move || {
            run_authenticated_child(
                child,
                limits,
                child_auth,
                std::process::id(),
                epoch,
                WorkerId::new(1),
            )
        });

        send_capability_fds(&host, &[], limits).expect("empty capability marker");
        host.write_all(&100_u32.to_le_bytes())
            .expect("declared record length");
        let failure = handle
            .join()
            .expect("child thread joined")
            .expect_err("partial record must fail closed");
        assert_eq!(failure.code(), DesktopIpcErrorCode::Disconnected);
    }
}
