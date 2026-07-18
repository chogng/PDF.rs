use std::fmt;
use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

use pdf_rs_protocol::{
    DesktopFrameDecoder, DiagnosticId, EngineError, EngineErrorCode, ErrorCategory,
    ErrorRecoverability, ErrorSeverity, EventEnvelope, WorkerFaultEvent, WorkerId,
};
use pdf_rs_surface::WorkerEpoch;

use crate::{
    DesktopCapabilityTable, DesktopEpochManager, DesktopHostProcess, DesktopIpcError,
    DesktopIpcErrorCode, DesktopIpcLimits, DesktopWireRecord, HostRangeBridge,
    error::error,
    process::{DESKTOP_CHILD_PANIC_EXIT_CODE, abort_unresolved_child},
    sandbox::DesktopProductSandboxGate,
};

const DEFAULT_RESTART_LIMIT: u8 = 2;
const MAX_RESTART_LIMIT: u8 = 8;
const DEFAULT_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounded Host policy for one supervised desktop child lineage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopSupervisorConfig {
    max_restarts: u8,
    transport_timeout: Duration,
}

impl DesktopSupervisorConfig {
    /// Creates a validated restart budget and per-record transport watchdog.
    pub fn new(max_restarts: u8, transport_timeout: Duration) -> Result<Self, DesktopIpcError> {
        if max_restarts > MAX_RESTART_LIMIT
            || transport_timeout.is_zero()
            || transport_timeout > MAX_TRANSPORT_TIMEOUT
        {
            return Err(error(DesktopIpcErrorCode::InvalidConfiguration));
        }
        Ok(Self {
            max_restarts,
            transport_timeout,
        })
    }

    /// Returns the maximum number of replacement children in this lineage.
    pub const fn max_restarts(self) -> u8 {
        self.max_restarts
    }

    /// Returns the bounded transport watchdog applied to every spawned child.
    pub const fn transport_timeout(self) -> Duration {
        self.transport_timeout
    }
}

impl Default for DesktopSupervisorConfig {
    fn default() -> Self {
        Self {
            max_restarts: DEFAULT_RESTART_LIMIT,
            transport_timeout: DEFAULT_TRANSPORT_TIMEOUT,
        }
    }
}

/// Stable, content-free reason that made one desktop Worker epoch terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DesktopWorkerFaultKind {
    /// The authenticated transport closed without a graceful Worker terminal.
    UnexpectedEof = 1,
    /// The child terminated with a nonzero process exit code.
    NonzeroExit = 2,
    /// The child panic boundary contained an unwind.
    ChildPanic = 3,
    /// A bounded transport read or write watchdog elapsed.
    TransportTimeout = 4,
    /// A canonical frame, credential, sequence, or capability was invalid.
    ProtocolViolation = 5,
    /// A checked transport allocation or fixed capacity was exhausted.
    ResourceLimit = 6,
    /// The child lifecycle became internally inconsistent.
    Lifecycle = 7,
}

impl DesktopWorkerFaultKind {
    const fn diagnostic_id(self) -> DiagnosticId {
        let value = match self {
            Self::UnexpectedEof => 0x4453_4b00_0000_0001,
            Self::NonzeroExit => 0x4453_4b00_0000_0002,
            Self::ChildPanic => 0x4453_4b00_0000_0003,
            Self::TransportTimeout => 0x4453_4b00_0000_0004,
            Self::ProtocolViolation => 0x4453_4b00_0000_0005,
            Self::ResourceLimit => 0x4453_4b00_0000_0006,
            Self::Lifecycle => 0x4453_4b00_0000_0007,
        };
        DiagnosticId::new(value)
    }
}

/// Terminal fault evidence for one old epoch and its optional replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopWorkerFault {
    kind: DesktopWorkerFaultKind,
    worker: WorkerId,
    epoch: WorkerEpoch,
    restart_attempt: Option<u8>,
    replacement_worker: Option<WorkerId>,
    replacement_epoch: Option<WorkerEpoch>,
}

impl DesktopWorkerFault {
    /// Returns the stable redacted fault category.
    pub const fn kind(&self) -> DesktopWorkerFaultKind {
        self.kind
    }

    /// Returns the terminal canonical Worker identity.
    pub const fn worker_id(&self) -> WorkerId {
        self.worker
    }

    /// Returns the terminal process epoch.
    pub const fn worker_epoch(&self) -> WorkerEpoch {
        self.epoch
    }

    /// Returns the bounded replacement attempt number, if one was permitted.
    pub const fn restart_attempt(&self) -> Option<u8> {
        self.restart_attempt
    }

    /// Returns the replacement canonical Worker identity after a successful restart.
    pub const fn replacement_worker_id(&self) -> Option<WorkerId> {
        self.replacement_worker
    }

    /// Returns the strictly newer replacement epoch after a successful restart.
    pub const fn replacement_epoch(&self) -> Option<WorkerEpoch> {
        self.replacement_epoch
    }

    /// Maps this Host-observed terminal to the canonical content-free WorkerFault payload.
    pub fn protocol_event(&self) -> WorkerFaultEvent {
        let (code, category) = if self.kind == DesktopWorkerFaultKind::ProtocolViolation {
            (EngineErrorCode::ProtocolViolation, ErrorCategory::Protocol)
        } else {
            (EngineErrorCode::Internal, ErrorCategory::Internal)
        };
        WorkerFaultEvent {
            error: EngineError {
                code,
                category,
                severity: ErrorSeverity::Fatal,
                recoverability: ErrorRecoverability::RestartWorker,
                diagnostic_id: self.kind.diagnostic_id(),
            },
        }
    }
}

/// Observable state of one bounded desktop child supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopSupervisorState {
    /// One authenticated child epoch is available.
    Running,
    /// The Host has declared intentional shutdown and no fault may restart it.
    ShuttingDown,
    /// Intentional shutdown completed and no child remains.
    Stopped,
    /// The configured replacement budget was exhausted.
    RestartLimitReached,
    /// A single permitted replacement spawn failed closed.
    RestartFailed,
}

/// Content-free outcome when one supervised transport operation cannot complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopSupervisionError {
    /// The active epoch became terminal; replacement metadata is carried by the fault.
    WorkerFault(DesktopWorkerFault),
    /// Host-originated stale traffic was rejected before any bytes or descriptors moved.
    Rejected(DesktopIpcErrorCode),
    /// No transport operation is legal after intentional or failed terminal state.
    Stopped,
}

impl fmt::Display for DesktopSupervisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerFault(fault) => {
                write!(formatter, "desktop Worker fault ({:?})", fault.kind())
            }
            Self::Rejected(code) => write!(formatter, "desktop traffic rejected ({code:?})"),
            Self::Stopped => formatter.write_str("desktop child supervisor stopped"),
        }
    }
}

impl std::error::Error for DesktopSupervisionError {}

/// Host resource owner notified exactly once before a failed epoch is replaced.
pub trait DesktopEpochCleanup {
    /// Makes all Host state and resources bound to `worker` and `epoch` terminal.
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch);
}

impl DesktopEpochCleanup for () {
    fn retire_epoch(&mut self, _worker: WorkerId, _epoch: WorkerEpoch) {}
}

impl DesktopEpochCleanup for DesktopCapabilityTable {
    fn retire_epoch(&mut self, _worker: WorkerId, _epoch: WorkerEpoch) {
        self.revoke_all();
    }
}

impl DesktopEpochCleanup for HostRangeBridge {
    fn retire_epoch(&mut self, _worker: WorkerId, _epoch: WorkerEpoch) {
        self.invalidate();
    }
}

impl<T: DesktopEpochCleanup> DesktopEpochCleanup for Vec<T> {
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch) {
        for resources in self {
            resources.retire_epoch(worker, epoch);
        }
    }
}

impl<A: DesktopEpochCleanup, B: DesktopEpochCleanup> DesktopEpochCleanup for (A, B) {
    fn retire_epoch(&mut self, worker: WorkerId, epoch: WorkerEpoch) {
        self.0.retire_epoch(worker, epoch);
        self.1.retire_epoch(worker, epoch);
    }
}

/// Host-owned child lineage with automatic fault containment and bounded restart.
pub struct DesktopChildSupervisor<C: DesktopEpochCleanup> {
    program: String,
    config: DesktopSupervisorConfig,
    launch: DesktopSupervisorLaunch,
    epochs: DesktopEpochManager,
    current: Option<DesktopHostProcess>,
    cleanup: C,
    restarts: u8,
    state: DesktopSupervisorState,
}

enum DesktopSupervisorLaunch {
    #[cfg(any(test, feature = "transport-fixture"))]
    TransportFixture,
    ProductMacos(DesktopProductSandboxGate),
}

impl<C: DesktopEpochCleanup> DesktopChildSupervisor<C> {
    /// Spawns a transport-only test child and begins a supervised epoch lineage.
    ///
    /// This exercises the authenticated process boundary but is not evidence of
    /// filesystem or network isolation.
    #[cfg(any(test, feature = "transport-fixture"))]
    pub fn start_transport_fixture(
        program: impl Into<String>,
        config: DesktopSupervisorConfig,
        cleanup: C,
    ) -> Result<Self, DesktopIpcError> {
        Self::start_with_launch(
            program,
            config,
            cleanup,
            DesktopSupervisorLaunch::TransportFixture,
        )
    }

    fn start_with_launch(
        program: impl Into<String>,
        config: DesktopSupervisorConfig,
        cleanup: C,
        launch: DesktopSupervisorLaunch,
    ) -> Result<Self, DesktopIpcError> {
        let program = program.into();
        if program.is_empty() {
            return Err(error(DesktopIpcErrorCode::InvalidConfiguration));
        }
        let mut epochs = DesktopEpochManager::new();
        let current =
            Self::spawn_for_launch(&mut epochs, &program, config.transport_timeout(), &launch)?;
        Ok(Self {
            program,
            config,
            launch,
            epochs,
            current: Some(current),
            cleanup,
            restarts: 0,
            state: DesktopSupervisorState::Running,
        })
    }

    /// Starts the selected signed macOS product worker or fails closed.
    ///
    /// The gate cannot be supplied by a caller. Until signed parent/helper
    /// packaging and live sandbox probes are committed, this returns
    /// [`DesktopIpcErrorCode::IsolationUnavailable`] without spawning.
    pub fn start_product_macos(
        program: impl Into<String>,
        config: DesktopSupervisorConfig,
        cleanup: C,
    ) -> Result<Self, DesktopIpcError> {
        let gate = DesktopProductSandboxGate::acquire()?;
        Self::start_with_launch(
            program,
            config,
            cleanup,
            DesktopSupervisorLaunch::ProductMacos(gate),
        )
    }

    /// Returns the current supervisor state.
    pub const fn state(&self) -> DesktopSupervisorState {
        self.state
    }

    /// Returns the number of replacement spawns already attempted.
    pub const fn restart_count(&self) -> u8 {
        self.restarts
    }

    /// Returns the active Worker identity, if a child remains.
    pub fn worker_id(&self) -> Option<WorkerId> {
        self.current.as_ref().map(DesktopHostProcess::worker_id)
    }

    /// Returns the active Worker epoch, if a child remains.
    pub fn worker_epoch(&self) -> Option<WorkerEpoch> {
        self.current.as_ref().map(DesktopHostProcess::worker_epoch)
    }

    /// Returns the active operating-system process identifier for watchdog integration.
    pub fn process_id(&self) -> Result<u32, DesktopIpcError> {
        self.current
            .as_ref()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?
            .process_id()
    }

    /// Borrows the Host resource owner for read-only zero-leak evidence.
    pub const fn cleanup(&self) -> &C {
        &self.cleanup
    }

    /// Borrows the Host resource owner to register current-epoch state.
    pub fn cleanup_mut(&mut self) -> &mut C {
        &mut self.cleanup
    }

    /// Updates the bounded watchdog for the active child and future replacements.
    pub fn set_transport_timeout(
        &mut self,
        transport_timeout: Duration,
    ) -> Result<(), DesktopIpcError> {
        let config = DesktopSupervisorConfig::new(self.config.max_restarts(), transport_timeout)?;
        self.current
            .as_mut()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?
            .set_transport_timeout(transport_timeout)?;
        self.config = config;
        Ok(())
    }

    /// Builds one current-epoch Host record without performing transport I/O.
    pub fn new_host_record(
        &self,
        sequence: u64,
        frame: Vec<u8>,
        capabilities: Vec<crate::DesktopCapability>,
        limits: DesktopIpcLimits,
    ) -> Result<DesktopWireRecord, DesktopIpcError> {
        self.current
            .as_ref()
            .ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?
            .new_host_record(sequence, frame, capabilities, limits)
    }

    /// Sends one current-epoch record under automatic fault supervision.
    ///
    /// A late old-epoch record and its borrowed descriptors are rejected
    /// before the active child socket is touched.
    pub fn send(
        &mut self,
        record: &DesktopWireRecord,
        fds: &[OwnedFd],
        limits: DesktopIpcLimits,
    ) -> Result<(), DesktopSupervisionError> {
        let current_epoch = self
            .worker_epoch()
            .ok_or(DesktopSupervisionError::Stopped)?;
        if record.worker_epoch() != current_epoch {
            return Err(DesktopSupervisionError::Rejected(
                DesktopIpcErrorCode::Authentication,
            ));
        }
        self.supervise(|process| process.send(record, fds, limits))
    }

    /// Receives, validates, and commits one generated handshake event atomically.
    pub fn receive_handshake_event(
        &mut self,
        limits: DesktopIpcLimits,
    ) -> Result<EventEnvelope, DesktopSupervisionError> {
        self.supervise(|process| {
            let mut pending = process.receive(limits)?;
            let event = pending.decode_handshake_event()?;
            let _ = pending.commit()?;
            Ok(event)
        })
    }

    /// Receives, validates, and commits one negotiated event and its owned descriptors.
    pub fn receive_event(
        &mut self,
        decoder: DesktopFrameDecoder,
        limits: DesktopIpcLimits,
    ) -> Result<(EventEnvelope, Vec<OwnedFd>), DesktopSupervisionError> {
        self.supervise(|process| {
            let mut pending = process.receive(limits)?;
            let event = pending.decode_event(decoder)?;
            let (_, fds) = pending.commit()?;
            Ok((event, fds))
        })
    }

    /// Runs one complete transport transaction and automatically contains any failure.
    ///
    /// Borrowed pending records cannot escape the closure, so canonical
    /// validation and commit finish before a restart decision is made.
    pub fn supervise<T>(
        &mut self,
        operation: impl FnOnce(&mut DesktopHostProcess) -> Result<T, DesktopIpcError>,
    ) -> Result<T, DesktopSupervisionError> {
        if !matches!(
            self.state,
            DesktopSupervisorState::Running | DesktopSupervisorState::ShuttingDown
        ) {
            return Err(DesktopSupervisionError::Stopped);
        }
        let result = {
            let process = self
                .current
                .as_mut()
                .ok_or(DesktopSupervisionError::Stopped)?;
            operation(process)
        };
        match result {
            Ok(value) => Ok(value),
            Err(_failure) if self.state == DesktopSupervisorState::ShuttingDown => {
                self.state = if self.retire_current() {
                    DesktopSupervisorState::Stopped
                } else {
                    DesktopSupervisorState::RestartFailed
                };
                Err(DesktopSupervisionError::Stopped)
            }
            Err(failure) => match self.handle_fault(failure) {
                Some(fault) => Err(DesktopSupervisionError::WorkerFault(fault)),
                None => Err(DesktopSupervisionError::Stopped),
            },
        }
    }

    /// Declares intentional shutdown before sending the canonical Shutdown command.
    pub fn begin_graceful_shutdown(&mut self) -> Result<(), DesktopIpcError> {
        if self.state != DesktopSupervisorState::Running || self.current.is_none() {
            return Err(error(DesktopIpcErrorCode::Lifecycle));
        }
        self.state = DesktopSupervisorState::ShuttingDown;
        Ok(())
    }

    /// Completes intentional shutdown when child reap can be proven.
    ///
    /// A kill or wait failure leaves the supervisor in `RestartFailed` and
    /// preserves Host resource ownership instead of claiming a clean stop.
    pub fn complete_graceful_shutdown(&mut self) {
        self.state = if self.retire_current() {
            DesktopSupervisorState::Stopped
        } else {
            DesktopSupervisorState::RestartFailed
        };
    }

    /// Stops immediately without permitting any replacement spawn.
    pub fn shutdown(&mut self) {
        if self.state == DesktopSupervisorState::Stopped && self.current.is_none() {
            return;
        }
        self.state = DesktopSupervisorState::ShuttingDown;
        self.complete_graceful_shutdown();
    }

    fn handle_fault(&mut self, failure: DesktopIpcError) -> Option<DesktopWorkerFault> {
        let Some(mut process) = self.current.take() else {
            self.state = DesktopSupervisorState::RestartFailed;
            return None;
        };
        let worker = process.worker_id();
        let epoch = process.worker_epoch();
        let status = match process.terminate_for_restart() {
            Ok(status) => status,
            Err(_) => {
                self.current = Some(process);
                self.state = DesktopSupervisorState::RestartFailed;
                return Some(DesktopWorkerFault {
                    kind: DesktopWorkerFaultKind::Lifecycle,
                    worker,
                    epoch,
                    restart_attempt: None,
                    replacement_worker: None,
                    replacement_epoch: None,
                });
            }
        };
        let kind = classify_failure(failure.code(), Some(&status));
        drop(process);

        // Old-epoch terminal evidence exists before every Host resource is
        // invalidated, and cleanup completes before any replacement spawn.
        let mut fault = DesktopWorkerFault {
            kind,
            worker,
            epoch,
            restart_attempt: None,
            replacement_worker: None,
            replacement_epoch: None,
        };
        self.cleanup.retire_epoch(worker, epoch);

        if self.restarts >= self.config.max_restarts() {
            self.state = DesktopSupervisorState::RestartLimitReached;
            return Some(fault);
        }
        self.restarts += 1;
        fault.restart_attempt = Some(self.restarts);
        match Self::spawn_for_launch(
            &mut self.epochs,
            &self.program,
            self.config.transport_timeout(),
            &self.launch,
        ) {
            Ok(replacement) => {
                let replacement_worker = replacement.worker_id();
                let replacement_epoch = replacement.worker_epoch();
                debug_assert!(replacement_epoch.value() > epoch.value());
                fault.replacement_worker = Some(replacement_worker);
                fault.replacement_epoch = Some(replacement_epoch);
                self.current = Some(replacement);
                self.state = DesktopSupervisorState::Running;
            }
            Err(_) => {
                self.state = DesktopSupervisorState::RestartFailed;
            }
        }
        Some(fault)
    }

    fn spawn_for_launch(
        epochs: &mut DesktopEpochManager,
        program: &str,
        transport_timeout: Duration,
        launch: &DesktopSupervisorLaunch,
    ) -> Result<DesktopHostProcess, DesktopIpcError> {
        match launch {
            #[cfg(any(test, feature = "transport-fixture"))]
            DesktopSupervisorLaunch::TransportFixture => {
                epochs.spawn_transport_fixture_with_timeout(program, transport_timeout)
            }
            DesktopSupervisorLaunch::ProductMacos(gate) => {
                epochs.spawn_product_macos_with_timeout(program, transport_timeout, gate)
            }
        }
    }

    fn retire_current(&mut self) -> bool {
        if let Some(mut process) = self.current.take() {
            let worker = process.worker_id();
            let epoch = process.worker_epoch();
            match process.terminate_for_restart() {
                Ok(_) => self.cleanup.retire_epoch(worker, epoch),
                Err(_) => {
                    self.current = Some(process);
                    return false;
                }
            }
        }
        true
    }
}

impl<C: DesktopEpochCleanup> Drop for DesktopChildSupervisor<C> {
    fn drop(&mut self) {
        let retired = self.retire_current();
        self.state = if retired {
            DesktopSupervisorState::Stopped
        } else {
            DesktopSupervisorState::RestartFailed
        };
        if let Some(failure) = self
            .current
            .as_ref()
            .and_then(DesktopHostProcess::unresolved_child_failure)
        {
            abort_unresolved_child(failure);
        }
    }
}

fn classify_failure(
    failure: DesktopIpcErrorCode,
    status: Option<&ExitStatus>,
) -> DesktopWorkerFaultKind {
    if failure == DesktopIpcErrorCode::TransportTimeout {
        return DesktopWorkerFaultKind::TransportTimeout;
    }
    if failure == DesktopIpcErrorCode::ChildPanic {
        return DesktopWorkerFaultKind::ChildPanic;
    }
    if status.and_then(ExitStatus::code) == Some(DESKTOP_CHILD_PANIC_EXIT_CODE) {
        return DesktopWorkerFaultKind::ChildPanic;
    }
    if status.and_then(|status| status.signal()) == Some(rustix::process::Signal::ABORT.as_raw()) {
        return DesktopWorkerFaultKind::ChildPanic;
    }
    if status
        .and_then(ExitStatus::code)
        .is_some_and(|code| code != 0)
    {
        return DesktopWorkerFaultKind::NonzeroExit;
    }
    match failure {
        DesktopIpcErrorCode::Disconnected => DesktopWorkerFaultKind::UnexpectedEof,
        DesktopIpcErrorCode::InvalidFrame
        | DesktopIpcErrorCode::Authentication
        | DesktopIpcErrorCode::Sequence
        | DesktopIpcErrorCode::Capability
        | DesktopIpcErrorCode::Source => DesktopWorkerFaultKind::ProtocolViolation,
        DesktopIpcErrorCode::ResourceLimit => DesktopWorkerFaultKind::ResourceLimit,
        DesktopIpcErrorCode::InvalidConfiguration
        | DesktopIpcErrorCode::Lifecycle
        | DesktopIpcErrorCode::IsolationUnavailable => DesktopWorkerFaultKind::Lifecycle,
        DesktopIpcErrorCode::ChildPanic => DesktopWorkerFaultKind::ChildPanic,
        DesktopIpcErrorCode::TransportTimeout => DesktopWorkerFaultKind::TransportTimeout,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopChildSupervisor, DesktopEpochCleanup, DesktopSupervisorConfig,
        DesktopSupervisorLaunch, DesktopSupervisorState, DesktopWorkerFault,
        DesktopWorkerFaultKind, classify_failure,
    };
    use crate::{
        DesktopEpochManager, DesktopHostProcess, DesktopIpcErrorCode, error::error,
        process::DesktopChildTerminationFailure,
    };
    use pdf_rs_protocol::WorkerId;
    use pdf_rs_surface::WorkerEpoch;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    struct CountingCleanup(Arc<AtomicUsize>);

    impl DesktopEpochCleanup for CountingCleanup {
        fn retire_epoch(&mut self, _worker: WorkerId, _epoch: WorkerEpoch) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn supervisor_policy_is_hard_bounded() {
        assert!(DesktopSupervisorConfig::new(8, Duration::from_secs(30)).is_ok());
        assert!(DesktopSupervisorConfig::new(9, Duration::from_secs(1)).is_err());
        assert!(DesktopSupervisorConfig::new(1, Duration::ZERO).is_err());
        assert!(DesktopSupervisorConfig::new(1, Duration::from_secs(31)).is_err());
    }

    #[test]
    fn product_macos_start_fails_before_program_launch() {
        let config = DesktopSupervisorConfig::default();
        let failure = DesktopChildSupervisor::start_product_macos(
            "/path/that-must-never-be-launched",
            config,
            (),
        )
        .err()
        .expect("workspace has no signed product sandbox attestation");
        assert_eq!(failure.code(), DesktopIpcErrorCode::IsolationUnavailable);
    }

    #[test]
    fn protocol_fault_mapping_is_content_free_and_wire_valid() {
        for kind in [
            DesktopWorkerFaultKind::UnexpectedEof,
            DesktopWorkerFaultKind::NonzeroExit,
            DesktopWorkerFaultKind::ChildPanic,
            DesktopWorkerFaultKind::TransportTimeout,
            DesktopWorkerFaultKind::ProtocolViolation,
            DesktopWorkerFaultKind::ResourceLimit,
            DesktopWorkerFaultKind::Lifecycle,
        ] {
            let fault = DesktopWorkerFault {
                kind,
                worker: WorkerId::new(7),
                epoch: WorkerEpoch::new(9).expect("epoch"),
                restart_attempt: Some(1),
                replacement_worker: Some(WorkerId::new(8)),
                replacement_epoch: WorkerEpoch::new(10),
            };
            assert!(fault.protocol_event().error.wire_invariants_valid());
            assert_eq!(
                fault.protocol_event().error.diagnostic_id,
                kind.diagnostic_id()
            );
        }
    }

    #[test]
    fn direct_panic_and_timeout_classification_are_stable() {
        assert_eq!(
            classify_failure(DesktopIpcErrorCode::ChildPanic, None),
            DesktopWorkerFaultKind::ChildPanic
        );
        assert_eq!(
            classify_failure(DesktopIpcErrorCode::TransportTimeout, None),
            DesktopWorkerFaultKind::TransportTimeout
        );
    }

    #[test]
    fn failed_reap_after_early_poison_never_cleans_or_spawns_replacement() {
        for failure in [
            DesktopChildTerminationFailure::Reap,
            DesktopChildTerminationFailure::Kill,
            DesktopChildTerminationFailure::Wait,
        ] {
            let cleanup_calls = Arc::new(AtomicUsize::new(0));
            let epoch = WorkerEpoch::new(1).expect("epoch");
            let worker = WorkerId::new(1);
            let mut process =
                DesktopHostProcess::stub_with_termination_failure(epoch, worker, failure)
                    .expect("stub process");

            // send/receive/PendingDesktopRecord failures poison before the
            // supervisor observes their error. Preserve that first failure.
            process.poison_for_test();
            let mut supervisor = DesktopChildSupervisor {
                program: "/must/not/spawn".into(),
                config: DesktopSupervisorConfig::new(2, Duration::from_secs(1)).expect("config"),
                launch: DesktopSupervisorLaunch::TransportFixture,
                epochs: DesktopEpochManager::new(),
                current: Some(process),
                cleanup: CountingCleanup(Arc::clone(&cleanup_calls)),
                restarts: 0,
                state: DesktopSupervisorState::Running,
            };

            let fault = supervisor
                .handle_fault(error(DesktopIpcErrorCode::Disconnected))
                .expect("terminal fault");
            assert_eq!(fault.kind(), DesktopWorkerFaultKind::Lifecycle);
            assert_eq!(fault.worker_epoch(), epoch);
            assert_eq!(fault.restart_attempt(), None);
            assert_eq!(fault.replacement_epoch(), None);
            assert_eq!(supervisor.state(), DesktopSupervisorState::RestartFailed);
            assert_eq!(supervisor.restart_count(), 0);
            assert_eq!(supervisor.worker_epoch(), Some(epoch));
            assert_eq!(cleanup_calls.load(Ordering::SeqCst), 0);

            supervisor.shutdown();
            assert_eq!(supervisor.state(), DesktopSupervisorState::RestartFailed);
            assert_eq!(supervisor.worker_epoch(), Some(epoch));
            assert_eq!(supervisor.restart_count(), 0);
            assert_eq!(cleanup_calls.load(Ordering::SeqCst), 0);

            drop(supervisor);
            assert_eq!(cleanup_calls.load(Ordering::SeqCst), 0);
        }
    }
}
