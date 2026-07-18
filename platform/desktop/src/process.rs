use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use pdf_rs_protocol::{SequenceTracker, WorkerId};
use pdf_rs_surface::WorkerEpoch;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

use crate::{
    DesktopDirection, DesktopIpcError, DesktopIpcErrorCode, DesktopIpcLimits, DesktopLaunchAuth,
    DesktopLaunchId, DesktopRecordBinding, DesktopWireRecord, error::error, receive_capability_fds,
    send_capability_fds, validate_read_only_fd,
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

/// Runs the minimal isolated child loop for the test fixture process.
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
        let mut incoming = None;
        let mut canonical = SequenceTracker::new();
        loop {
            let fds = match receive_capability_fds(&input, limits) {
                Ok(fds) => fds,
                Err(failure) if failure.code() == DesktopIpcErrorCode::Disconnected => {
                    return Ok(());
                }
                Err(failure) => return Err(failure),
            };
            let record = DesktopWireRecord::read_authenticated_from(
                &mut input,
                limits,
                &auth,
                host_pid,
                DesktopDirection::HostToWorker,
                epoch,
                &mut incoming,
            )?;
            if record.capabilities().len() != fds.len() {
                return Err(error(DesktopIpcErrorCode::Capability));
            }
            for (descriptor, fd) in record.capabilities().iter().zip(&fds) {
                validate_read_only_fd(fd, descriptor.byte_length())?;
            }
            crate::validate_host_hello_command(record.frame(), fds.len(), worker, &mut canonical)?;
            record.commit_outer_sequence(&mut incoming)?;
            // Foundation worker performs no dispatch. It validates a generated
            // handshake and deliberately emits no echo or arbitrary payload.
        }
    }));
    match run {
        Ok(result) => result,
        Err(_) => Err(error(DesktopIpcErrorCode::ChildPanic)),
    }
}
