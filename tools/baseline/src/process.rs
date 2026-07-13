use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use pdf_rs_digest::Sha256;

use crate::{
    BaselineDescriptor, BaselineError, BaselineObservation, BaselineRequest, BaselineRunner,
    containment_failed, decode_response, descriptor_identity, encode_request,
    invalid_process_config, output_limit, process_spawn_failed, request_limit, runner_failed,
    transport_failed, watchdog_expired,
};

const MAX_REQUEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STDOUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WATCHDOG: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(2);
const CLEANUP_GRACE: Duration = Duration::from_millis(250);
const PIPE_BUFFER_BYTES: usize = 8 * 1024;

/// Hard limits for one direct-child baseline invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessLimits {
    max_request_bytes: u64,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    watchdog: Duration,
}

impl ProcessLimits {
    /// Creates non-zero per-invocation limits under the tool's fixed safety ceilings.
    pub fn new(
        max_request_bytes: u64,
        max_stdout_bytes: u64,
        max_stderr_bytes: u64,
        watchdog: Duration,
    ) -> Result<Self, BaselineError> {
        if max_request_bytes == 0
            || max_request_bytes > MAX_REQUEST_BYTES
            || max_stdout_bytes == 0
            || max_stdout_bytes > MAX_STDOUT_BYTES
            || max_stderr_bytes == 0
            || max_stderr_bytes > MAX_STDERR_BYTES
            || watchdog.is_zero()
            || watchdog > MAX_WATCHDOG
        {
            return Err(invalid_process_config());
        }
        Ok(Self {
            max_request_bytes,
            max_stdout_bytes,
            max_stderr_bytes,
            watchdog,
        })
    }

    /// Returns the encoded stdin-frame ceiling.
    pub const fn max_request_bytes(self) -> u64 {
        self.max_request_bytes
    }

    /// Returns the captured stdout-frame ceiling.
    pub const fn max_stdout_bytes(self) -> u64 {
        self.max_stdout_bytes
    }

    /// Returns the discarded stderr ceiling.
    pub const fn max_stderr_bytes(self) -> u64 {
        self.max_stderr_bytes
    }

    /// Returns the direct-child wall-clock safety deadline.
    pub const fn watchdog(self) -> Duration {
        self.watchdog
    }
}

/// Canonical direct-child invocation with no implicitly inserted shell.
///
/// Paths are canonicalized at construction. Arguments and environment entries
/// are passed as individual values after `env_clear`; this type deliberately has
/// no `Debug` implementation. The isolation profile is identity evidence only:
/// the caller must arrange the named platform sandbox or container externally.
pub struct ProcessSpec {
    executable: PathBuf,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    working_directory: PathBuf,
    isolation_profile: String,
}

impl ProcessSpec {
    /// Validates and canonicalizes a direct executable invocation.
    pub fn new(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        environment: Vec<(String, String)>,
        working_directory: impl Into<PathBuf>,
        isolation_profile: impl Into<String>,
    ) -> Result<Self, BaselineError> {
        let executable = canonical_file(executable.into())?;
        let working_directory = canonical_directory(working_directory.into())?;
        let isolation_profile = isolation_profile.into();
        if isolation_profile.trim().is_empty()
            || contains_nul(&isolation_profile)
            || arguments.iter().any(|value| contains_nul(value))
        {
            return Err(invalid_process_config());
        }

        let mut environment = environment;
        if environment.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || contains_nul(key) || contains_nul(value)
        }) {
            return Err(invalid_process_config());
        }
        environment.sort_by(|left, right| left.0.cmp(&right.0));
        if environment
            .windows(2)
            .any(|entries| entries[0].0 == entries[1].0)
        {
            return Err(invalid_process_config());
        }

        Ok(Self {
            executable,
            arguments,
            environment,
            working_directory,
            isolation_profile,
        })
    }

    /// Hashes the canonical executable path, argv, environment, cwd, transport, isolation, and limits.
    pub fn identity(&self, limits: ProcessLimits) -> Result<[u8; 32], BaselineError> {
        let executable = utf8_path(&self.executable)?;
        let working_directory = utf8_path(&self.working_directory)?;
        let mut hasher = Sha256::new();
        hasher
            .update(b"PDFRS-BASELINE-INVOCATION-2")
            .map_err(|_| invalid_process_config())?;
        update_framed(&mut hasher, executable.as_bytes())?;
        update_sequence(&mut hasher, &self.arguments)?;
        let environment_count =
            u64::try_from(self.environment.len()).map_err(|_| invalid_process_config())?;
        hasher
            .update(&environment_count.to_be_bytes())
            .map_err(|_| invalid_process_config())?;
        for (key, value) in &self.environment {
            update_framed(&mut hasher, key.as_bytes())?;
            update_framed(&mut hasher, value.as_bytes())?;
        }
        update_framed(&mut hasher, working_directory.as_bytes())?;
        update_framed(&mut hasher, b"stdin-frame-v2")?;
        update_framed(&mut hasher, self.isolation_profile.as_bytes())?;
        for value in [
            limits.max_request_bytes,
            limits.max_stdout_bytes,
            limits.max_stderr_bytes,
            limits.watchdog.as_secs(),
        ] {
            hasher
                .update(&value.to_be_bytes())
                .map_err(|_| invalid_process_config())?;
        }
        hasher
            .update(&limits.watchdog.subsec_nanos().to_be_bytes())
            .map_err(|_| invalid_process_config())?;
        hasher.finalize().map_err(|_| invalid_process_config())
    }
}

/// Deadline- and byte-limited supervisor for one externally contained direct child.
///
/// The runner captures no stderr content, launches the configured executable
/// directly without implicitly inserting a shell, clears inherited environment
/// variables, and writes document bytes to the child's stdin. The caller is
/// responsible for reviewing the executable, argv, cwd, and allowlisted
/// environment. Safe `std::process` can supervise only the direct child; a
/// platform sandbox or container must provide descendant, CPU, memory,
/// filesystem, and network containment before this runner is used for real
/// PDFium observations. There is no caller cancellation token; the watchdog is
/// the outer child deadline. Concurrent calls share the configured working
/// directory, so an approved wrapper must give each child private writable
/// storage or otherwise prevent cross-invocation file collisions.
pub struct ProcessBaselineRunner {
    descriptor: BaselineDescriptor,
    process: ProcessSpec,
    limits: ProcessLimits,
}

impl ProcessBaselineRunner {
    /// Binds a validated invocation to a caller-supplied baseline descriptor.
    pub fn new(
        descriptor: BaselineDescriptor,
        process: ProcessSpec,
        limits: ProcessLimits,
    ) -> Result<Self, BaselineError> {
        descriptor_identity(&descriptor)?;
        if descriptor.invocation_hash != process.identity(limits)? {
            return Err(invalid_process_config());
        }
        Ok(Self {
            descriptor,
            process,
            limits,
        })
    }

    fn run(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError> {
        let frame_len = crate::REQUEST_HEADER_LEN
            .checked_add(request.pdf().len())
            .ok_or_else(request_limit)?;
        if u64::try_from(frame_len).map_err(|_| request_limit())? > self.limits.max_request_bytes {
            return Err(request_limit());
        }
        let frame = encode_request(request, &self.descriptor)?;
        debug_assert_eq!(frame.len(), frame_len);

        let mut command = Command::new(&self.process.executable);
        command
            .args(&self.process.arguments)
            .current_dir(&self.process.working_directory)
            .env_clear()
            .envs(
                self.process
                    .environment
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|_| process_spawn_failed())?;
        let Some(stdin) = child.stdin.take() else {
            return Err(abort_setup(&mut child, Vec::new(), None));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(abort_setup(&mut child, Vec::new(), None));
        };
        let Some(stderr) = child.stderr.take() else {
            return Err(abort_setup(&mut child, Vec::new(), None));
        };

        let exceeded = Arc::new(AtomicBool::new(false));
        let (transport_failure_sender, transport_failure_receiver) = mpsc::channel();
        let stdout_handle = match spawn_stdout(
            stdout,
            self.limits.max_stdout_bytes,
            Arc::clone(&exceeded),
            transport_failure_sender.clone(),
        ) {
            Ok(handle) => handle,
            Err(_) => return Err(abort_setup(&mut child, Vec::new(), None)),
        };
        let stderr_handle = match spawn_stderr(
            stderr,
            self.limits.max_stderr_bytes,
            Arc::clone(&exceeded),
            transport_failure_sender.clone(),
        ) {
            Ok(handle) => handle,
            Err(_) => return Err(abort_setup(&mut child, vec![stdout_handle], None)),
        };
        let writer_handle = match spawn_writer(stdin, frame, transport_failure_sender.clone()) {
            Ok(handle) => handle,
            Err(_) => {
                return Err(abort_setup(
                    &mut child,
                    vec![stdout_handle, stderr_handle],
                    None,
                ));
            }
        };
        drop(transport_failure_sender);

        let supervision = supervise(
            &mut child,
            &exceeded,
            &transport_failure_receiver,
            self.limits.watchdog,
        );
        if supervision == Supervision::ContainmentFailed {
            return Err(containment_failed());
        }

        let cleanup_deadline = Instant::now()
            .checked_add(CLEANUP_GRACE)
            .ok_or_else(containment_failed)?;
        let writer = join_before(writer_handle, cleanup_deadline)?;
        let stdout = join_before(stdout_handle, cleanup_deadline)?;
        let stderr = join_before(stderr_handle, cleanup_deadline)?;

        if matches!(supervision, Supervision::OutputLimit) || exceeded.load(Ordering::Acquire) {
            return Err(output_limit());
        }
        if supervision == Supervision::WatchdogExpired {
            return Err(watchdog_expired());
        }
        writer.map_err(|_| transport_failed())?;
        let stdout = stdout.map_err(|_| transport_failed())?;
        stderr.map_err(|_| transport_failed())?;

        let Supervision::Exited(status) = supervision else {
            return Err(transport_failed());
        };
        if !status.success() {
            return Err(runner_failed());
        }
        decode_response(
            &stdout,
            request,
            self.descriptor.clone(),
            self.limits.max_stdout_bytes,
        )
    }
}

impl BaselineRunner for ProcessBaselineRunner {
    fn describe(&self) -> Result<BaselineDescriptor, BaselineError> {
        Ok(self.descriptor.clone())
    }

    fn observe(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError> {
        self.run(request)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Supervision {
    Exited(ExitStatus),
    OutputLimit,
    WatchdogExpired,
    TransportFailed,
    ContainmentFailed,
}

fn supervise(
    child: &mut Child,
    exceeded: &AtomicBool,
    transport_failure: &Receiver<()>,
    watchdog: Duration,
) -> Supervision {
    let Some(deadline) = Instant::now().checked_add(watchdog) else {
        return Supervision::ContainmentFailed;
    };
    loop {
        if exceeded.load(Ordering::Acquire) {
            return match terminate_direct_child(child) {
                Ok(_) => Supervision::OutputLimit,
                Err(_) => Supervision::ContainmentFailed,
            };
        }
        match transport_failure.try_recv() {
            Ok(()) => {
                return match terminate_direct_child(child) {
                    Ok(_) => Supervision::TransportFailed,
                    Err(_) => Supervision::ContainmentFailed,
                };
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => return Supervision::Exited(status),
            Ok(None) => {}
            Err(_) => {
                return match terminate_direct_child(child) {
                    Ok(_) => Supervision::TransportFailed,
                    Err(_) => Supervision::ContainmentFailed,
                };
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return match terminate_direct_child(child) {
                Ok(_) => Supervision::WatchdogExpired,
                Err(_) => Supervision::ContainmentFailed,
            };
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
}

fn terminate_direct_child(child: &mut Child) -> Result<ExitStatus, BaselineError> {
    match child.kill() {
        Ok(()) => {
            let deadline = Instant::now()
                .checked_add(CLEANUP_GRACE)
                .ok_or_else(containment_failed)?;
            wait_for_exit(child, deadline)
        }
        Err(_) => match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            _ => Err(containment_failed()),
        },
    }
}

fn wait_for_exit(child: &mut Child, deadline: Instant) -> Result<ExitStatus, BaselineError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => return Err(containment_failed()),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(containment_failed());
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
}

type DrainHandle = JoinHandle<io::Result<Vec<u8>>>;
type WriterHandle = JoinHandle<io::Result<()>>;

fn spawn_stdout(
    stdout: ChildStdout,
    limit: u64,
    exceeded: Arc<AtomicBool>,
    transport_failure: Sender<()>,
) -> io::Result<DrainHandle> {
    thread::Builder::new()
        .name("pdf-rs-baseline-stdout".into())
        .spawn(move || {
            report_transport_failure(
                drain_pipe(stdout, limit, true, &exceeded),
                transport_failure,
            )
        })
}

fn spawn_stderr(
    stderr: ChildStderr,
    limit: u64,
    exceeded: Arc<AtomicBool>,
    transport_failure: Sender<()>,
) -> io::Result<DrainHandle> {
    thread::Builder::new()
        .name("pdf-rs-baseline-stderr".into())
        .spawn(move || {
            report_transport_failure(
                drain_pipe(stderr, limit, false, &exceeded),
                transport_failure,
            )
        })
}

fn spawn_writer(
    mut stdin: ChildStdin,
    frame: Vec<u8>,
    transport_failure: Sender<()>,
) -> io::Result<WriterHandle> {
    thread::Builder::new()
        .name("pdf-rs-baseline-stdin".into())
        .spawn(move || {
            let result = stdin.write_all(&frame).and_then(|()| stdin.flush());
            report_transport_failure(result, transport_failure)
        })
}

fn report_transport_failure<T>(result: io::Result<T>, sender: Sender<()>) -> io::Result<T> {
    if result.is_err() {
        let _ = sender.send(());
    }
    result
}

fn drain_pipe(
    mut pipe: impl Read,
    limit: u64,
    capture: bool,
    exceeded: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; PIPE_BUFFER_BYTES];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if total > limit {
            exceeded.store(true, Ordering::Release);
        }
        if capture {
            let remaining = usize::try_from(
                limit.saturating_sub(u64::try_from(captured.len()).unwrap_or(u64::MAX)),
            )
            .unwrap_or(usize::MAX);
            let copied = remaining.min(read);
            captured
                .try_reserve_exact(copied)
                .map_err(|_| io::Error::other("bounded pipe capture allocation failed"))?;
            captured.extend_from_slice(&buffer[..copied]);
        }
    }
}

fn abort_setup(
    child: &mut Child,
    drains: Vec<DrainHandle>,
    writer: Option<WriterHandle>,
) -> BaselineError {
    if terminate_direct_child(child).is_err() {
        return containment_failed();
    }
    let Some(deadline) = Instant::now().checked_add(CLEANUP_GRACE) else {
        return containment_failed();
    };
    if let Some(writer) = writer
        && join_before(writer, deadline).is_err()
    {
        return containment_failed();
    }
    for drain in drains {
        if join_before(drain, deadline).is_err() {
            return containment_failed();
        }
    }
    transport_failed()
}

fn join_before<T>(handle: JoinHandle<T>, deadline: Instant) -> Result<T, BaselineError> {
    while !handle.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            return Err(containment_failed());
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
    handle.join().map_err(|_| transport_failed())
}

fn canonical_file(path: PathBuf) -> Result<PathBuf, BaselineError> {
    if !path.is_absolute() {
        return Err(invalid_process_config());
    }
    let canonical = fs::canonicalize(path).map_err(|_| invalid_process_config())?;
    if !fs::metadata(&canonical)
        .map_err(|_| invalid_process_config())?
        .is_file()
    {
        return Err(invalid_process_config());
    }
    utf8_path(&canonical)?;
    Ok(canonical)
}

fn canonical_directory(path: PathBuf) -> Result<PathBuf, BaselineError> {
    if !path.is_absolute() {
        return Err(invalid_process_config());
    }
    let canonical = fs::canonicalize(path).map_err(|_| invalid_process_config())?;
    if !fs::metadata(&canonical)
        .map_err(|_| invalid_process_config())?
        .is_dir()
    {
        return Err(invalid_process_config());
    }
    utf8_path(&canonical)?;
    Ok(canonical)
}

fn utf8_path(path: &Path) -> Result<&str, BaselineError> {
    path.to_str().ok_or_else(invalid_process_config)
}

fn contains_nul(value: &str) -> bool {
    value.as_bytes().contains(&0)
}

fn update_sequence(hasher: &mut Sha256, values: &[String]) -> Result<(), BaselineError> {
    let count = u64::try_from(values.len()).map_err(|_| invalid_process_config())?;
    hasher
        .update(&count.to_be_bytes())
        .map_err(|_| invalid_process_config())?;
    for value in values {
        update_framed(hasher, value.as_bytes())?;
    }
    Ok(())
}

fn update_framed(hasher: &mut Sha256, value: &[u8]) -> Result<(), BaselineError> {
    let length = u64::try_from(value.len()).map_err(|_| invalid_process_config())?;
    hasher
        .update(&length.to_be_bytes())
        .map_err(|_| invalid_process_config())?;
    hasher.update(value).map_err(|_| invalid_process_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_zero_and_tool_ceiling_overrides() {
        for result in [
            ProcessLimits::new(0, 1, 1, Duration::from_millis(1)),
            ProcessLimits::new(1, 0, 1, Duration::from_millis(1)),
            ProcessLimits::new(1, 1, 0, Duration::from_millis(1)),
            ProcessLimits::new(1, 1, 1, Duration::ZERO),
            ProcessLimits::new(MAX_REQUEST_BYTES + 1, 1, 1, Duration::from_millis(1)),
            ProcessLimits::new(1, MAX_STDOUT_BYTES + 1, 1, Duration::from_millis(1)),
            ProcessLimits::new(1, 1, MAX_STDERR_BYTES + 1, Duration::from_millis(1)),
            ProcessLimits::new(1, 1, 1, MAX_WATCHDOG + Duration::from_millis(1)),
        ] {
            assert_eq!(
                result.unwrap_err().code,
                crate::BaselineErrorCode::InvalidProcessConfig
            );
        }
    }

    #[test]
    fn bounded_drain_keeps_only_stdout_limit_and_discards_stderr() {
        let exceeded = AtomicBool::new(false);
        let captured = drain_pipe(&b"abcdef"[..], 4, true, &exceeded).unwrap();
        assert_eq!(captured, b"abcd");
        assert!(exceeded.load(Ordering::Acquire));

        let exceeded = AtomicBool::new(false);
        let captured = drain_pipe(&b"secret"[..], 6, false, &exceeded).unwrap();
        assert!(captured.is_empty());
        assert!(!exceeded.load(Ordering::Acquire));
    }
}
