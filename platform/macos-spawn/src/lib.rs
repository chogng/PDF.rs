//! Fixed Darwin `posix_spawn` boundary for the desktop Native worker.
//!
//! The crate deliberately exposes one fixed launch operation and its matching
//! worker-entry signal reset instead of a general process builder. On
//! non-macOS targets it exports neither API.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_os = "macos")]
mod darwin {
    use std::env;
    use std::ffi::{CString, OsStr};
    use std::fs::OpenOptions;
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::ptr;

    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, WaitOptions};

    const WORKER_ARGUMENT: &str = "--pdf-rs-desktop-child";
    const FIRST_PRIVATE_FD: libc::c_int = 3;

    /// One spawned desktop worker whose terminal status is cached after reap.
    ///
    /// Dropping this value does not signal or reap the child. The owning
    /// supervisor must call [`try_wait`](Self::try_wait), [`kill`](Self::kill),
    /// and [`wait`](Self::wait) according to its lifecycle policy.
    #[derive(Debug)]
    pub struct SpawnedChild {
        pid: Pid,
        status: Option<ExitStatus>,
    }

    impl SpawnedChild {
        /// Returns the positive operating-system process identifier.
        pub fn id(&self) -> u32 {
            u32::try_from(self.pid.as_raw_pid()).expect("Darwin child PID is positive")
        }

        /// Returns a cached terminal status or performs one nonblocking reap.
        pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if let Some(status) = self.status {
                return Ok(Some(status));
            }
            let status = wait_for_pid(self.pid, WaitOptions::NOHANG)?;
            if let Some(status) = status {
                self.status = Some(status);
            }
            Ok(status)
        }

        /// Sends SIGKILL to an unreaped child.
        ///
        /// A cached terminal child is never signaled because its PID may have
        /// been reused after reap.
        pub fn kill(&mut self) -> io::Result<()> {
            if self.status.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "desktop worker was already reaped",
                ));
            }
            rustix::process::kill_process(self.pid, Signal::KILL).map_err(io::Error::from)
        }

        /// Blocks until the child is reaped and returns its cached raw status.
        pub fn wait(&mut self) -> io::Result<ExitStatus> {
            if let Some(status) = self.status {
                return Ok(status);
            }
            let status = wait_for_pid(self.pid, WaitOptions::empty())?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "blocking wait returned without a child status",
                )
            })?;
            self.status = Some(status);
            Ok(status)
        }
    }

    /// Spawns the fixed desktop worker with a default-close descriptor policy.
    ///
    /// `program` must be absolute. The exact argv is the executable path plus
    /// `--pdf-rs-desktop-child`; PATH is never searched. The child environment
    /// is empty except for an absolute, NUL-free `TMPDIR` copied from the Host
    /// when present. File descriptor 0 is the duplex `worker_socket`, while
    /// descriptors 1 and 2 refer to `/dev/null`.
    pub fn spawn_desktop_worker(
        program: &Path,
        worker_socket: BorrowedFd<'_>,
    ) -> io::Result<SpawnedChild> {
        let program = validated_program(program)?;
        let worker_argument = CString::new(WORKER_ARGUMENT).expect("fixed argv has no NUL");
        let environment = validated_environment()?;
        let mut argv = [
            program.as_ptr().cast_mut(),
            worker_argument.as_ptr().cast_mut(),
            ptr::null_mut(),
        ];
        let mut envp = environment
            .iter()
            .map(|entry| entry.as_ptr().cast_mut())
            .collect::<Vec<_>>();
        envp.push(ptr::null_mut());

        let socket_source = private_duplicate(worker_socket)?;
        let null_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?;
        let null_source = private_duplicate(null_file.as_fd())?;

        let mut attributes = SpawnAttributes::new()?;
        attributes.configure_signals_and_flags()?;
        let mut actions = SpawnFileActions::new()?;
        actions.add_dup2(socket_source.as_raw_fd(), libc::STDIN_FILENO)?;
        actions.add_dup2(null_source.as_raw_fd(), libc::STDOUT_FILENO)?;
        actions.add_dup2(null_source.as_raw_fd(), libc::STDERR_FILENO)?;
        actions.add_close(socket_source.as_raw_fd())?;
        actions.add_close(null_source.as_raw_fd())?;

        let mut raw_pid: libc::pid_t = 0;
        // SAFETY:
        // - `program`, both argv CStrings, every environment CString, and both
        //   null-terminated pointer arrays remain live for the whole call.
        // - Darwin declares argv/envp mutable for historical ABI reasons but
        //   `posix_spawn` only reads these caller-owned strings.
        // - both initialized guards remain live, and every file-action source
        //   is a valid owned descriptor at or above fd 3 until the call returns.
        // - `raw_pid` is writable and is read only after a zero return value.
        let result = unsafe {
            libc::posix_spawn(
                &mut raw_pid,
                program.as_ptr(),
                actions.as_ptr(),
                attributes.as_ptr(),
                argv.as_mut_ptr(),
                envp.as_mut_ptr(),
            )
        };
        posix_result(result)?;
        // SAFETY: on a zero `posix_spawn` return Darwin has created exactly one
        // child and writes its strictly positive process ID to `raw_pid`.
        // Treating that ABI guarantee as the success invariant avoids turning
        // an already-created child into an unowned error path.
        let pid = unsafe { Pid::from_raw_unchecked(raw_pid) };
        Ok(SpawnedChild { pid, status: None })
    }

    /// Restores the fixed desktop-worker signal state after Rust startup.
    ///
    /// Rust startup ignores SIGPIPE before `main`, after the Darwin spawn
    /// attributes have installed the default disposition. The worker binary
    /// must call this as its first operation so SIGPIPE is default and the
    /// process signal mask is empty before it touches inherited transport.
    pub fn restore_desktop_worker_signal_state() -> io::Result<()> {
        let signal_mask = empty_signal_set()?;
        let signal_default = libc::sigaction {
            sa_sigaction: libc::SIG_DFL,
            sa_mask: signal_mask,
            sa_flags: 0,
        };
        // SAFETY: `signal_default` is a fully initialized Darwin `sigaction`;
        // SIGPIPE is valid, and no old-action output is requested.
        unix_result(unsafe { libc::sigaction(libc::SIGPIPE, &signal_default, ptr::null_mut()) })?;
        // SAFETY: `signal_mask` is a fully initialized empty set and no old
        // mask output is requested. `pthread_sigmask` returns an errno value.
        posix_result(unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &signal_mask, ptr::null_mut())
        })
    }

    fn validated_program(program: &Path) -> io::Result<CString> {
        if !program.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "desktop worker executable must be absolute",
            ));
        }
        c_string(
            program.as_os_str(),
            "desktop worker executable contains NUL",
        )
    }

    fn validated_environment() -> io::Result<Vec<CString>> {
        let Some(tmpdir) = env::var_os("TMPDIR") else {
            return Ok(Vec::new());
        };
        if !Path::new(&tmpdir).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TMPDIR must be absolute",
            ));
        }
        let mut assignment = b"TMPDIR=".to_vec();
        assignment.extend_from_slice(tmpdir.as_bytes());
        CString::new(assignment)
            .map(|value| vec![value])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TMPDIR contains NUL"))
    }

    fn c_string(value: &OsStr, message: &'static str) -> io::Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, message))
    }

    fn private_duplicate(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
        let duplicate =
            rustix::io::fcntl_dupfd_cloexec(fd, FIRST_PRIVATE_FD).map_err(io::Error::from)?;
        if duplicate.as_raw_fd() < FIRST_PRIVATE_FD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "private spawn source descriptor overlaps stdio",
            ));
        }
        Ok(duplicate)
    }

    fn wait_for_pid(pid: Pid, options: WaitOptions) -> io::Result<Option<ExitStatus>> {
        loop {
            match rustix::process::waitpid(Some(pid), options) {
                Ok(Some((observed, status))) if observed == pid => {
                    return Ok(Some(ExitStatus::from_raw(status.as_raw())));
                }
                Ok(Some(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "waitpid returned a foreign child",
                    ));
                }
                Ok(None) => return Ok(None),
                Err(error) if error == Errno::INTR => {}
                Err(error) => return Err(io::Error::from(error)),
            }
        }
    }

    struct SpawnAttributes {
        raw: libc::posix_spawnattr_t,
    }

    impl SpawnAttributes {
        fn new() -> io::Result<Self> {
            let mut raw = ptr::null_mut();
            // SAFETY: `raw` is a writable Darwin attribute handle slot. It is
            // not observed or destroyed unless initialization returns zero.
            let result = unsafe { libc::posix_spawnattr_init(&mut raw) };
            posix_result(result)?;
            Ok(Self { raw })
        }

        fn configure_signals_and_flags(&mut self) -> io::Result<()> {
            let mut signal_default = empty_signal_set()?;
            // SAFETY: `signal_default` is initialized by `sigemptyset` and
            // SIGPIPE is a valid Darwin signal number.
            unix_result(unsafe { libc::sigaddset(&mut signal_default, libc::SIGPIPE) })?;
            let signal_mask = empty_signal_set()?;
            // SAFETY: this guard owns a live initialized attribute handle, and
            // both signal-set pointers remain valid for each complete call.
            posix_result(unsafe {
                libc::posix_spawnattr_setsigdefault(&mut self.raw, &signal_default)
            })?;
            // SAFETY: same initialized-handle invariant; an empty mask is a
            // fully initialized `sigset_t`.
            posix_result(unsafe { libc::posix_spawnattr_setsigmask(&mut self.raw, &signal_mask) })?;
            let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                | libc::POSIX_SPAWN_SETSIGDEF
                | libc::POSIX_SPAWN_SETSIGMASK;
            let flags = libc::c_short::try_from(flags).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "spawn flags exceed c_short")
            })?;
            // SAFETY: this guard owns a live initialized attribute handle and
            // the checked flag value contains only public Darwin spawn flags.
            posix_result(unsafe { libc::posix_spawnattr_setflags(&mut self.raw, flags) })
        }

        fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
            &self.raw
        }
    }

    impl Drop for SpawnAttributes {
        fn drop(&mut self) {
            // SAFETY: `SpawnAttributes` exists only after successful init and
            // owns the handle exactly once. A destroy failure is intentionally
            // ignored so a successful spawn is never reported as childless.
            let _ = unsafe { libc::posix_spawnattr_destroy(&mut self.raw) };
        }
    }

    struct SpawnFileActions {
        raw: libc::posix_spawn_file_actions_t,
    }

    impl SpawnFileActions {
        fn new() -> io::Result<Self> {
            let mut raw = ptr::null_mut();
            // SAFETY: `raw` is a writable Darwin file-actions handle slot. It
            // is not observed or destroyed unless initialization returns zero.
            let result = unsafe { libc::posix_spawn_file_actions_init(&mut raw) };
            posix_result(result)?;
            Ok(Self { raw })
        }

        fn add_dup2(&mut self, source: libc::c_int, target: libc::c_int) -> io::Result<()> {
            if source < FIRST_PRIVATE_FD || !(0..FIRST_PRIVATE_FD).contains(&target) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid desktop worker dup2 action",
                ));
            }
            // SAFETY: this guard owns an initialized actions handle; `source`
            // stays open through spawn and `target` is one of fd 0, 1, or 2.
            posix_result(unsafe {
                libc::posix_spawn_file_actions_adddup2(&mut self.raw, source, target)
            })
        }

        fn add_close(&mut self, source: libc::c_int) -> io::Result<()> {
            if source < FIRST_PRIVATE_FD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid desktop worker close action",
                ));
            }
            // SAFETY: this guard owns an initialized actions handle and source
            // is a live private descriptor until `posix_spawn` returns.
            posix_result(unsafe { libc::posix_spawn_file_actions_addclose(&mut self.raw, source) })
        }

        fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
            &self.raw
        }
    }

    impl Drop for SpawnFileActions {
        fn drop(&mut self) {
            // SAFETY: `SpawnFileActions` exists only after successful init and
            // owns the handle exactly once. See the matching attribute guard.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(&mut self.raw) };
        }
    }

    fn empty_signal_set() -> io::Result<libc::sigset_t> {
        let mut set = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `set` points to writable storage of the exact ABI type and
        // is assumed initialized only after `sigemptyset` reports success.
        unix_result(unsafe { libc::sigemptyset(set.as_mut_ptr()) })?;
        // SAFETY: the immediately preceding call initialized every byte of the
        // Darwin `sigset_t` on its success path.
        Ok(unsafe { set.assume_init() })
    }

    fn posix_result(result: libc::c_int) -> io::Result<()> {
        if result == 0 {
            Ok(())
        } else if result > 0 {
            Err(io::Error::from_raw_os_error(result))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Darwin spawn API returned a negative error",
            ))
        }
    }

    fn unix_result(result: libc::c_int) -> io::Result<()> {
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(target_os = "macos")]
pub use darwin::{SpawnedChild, restore_desktop_worker_signal_state, spawn_desktop_worker};
