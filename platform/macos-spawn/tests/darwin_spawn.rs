#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::ptr;

    use pdf_rs_macos_spawn::{
        SpawnedChild, restore_desktop_worker_signal_state, spawn_desktop_worker,
    };
    use rustix::io::FdFlags;

    const INSPECT_FDS: u8 = 1;
    const EXIT_ZERO: u8 = 2;
    const EXIT_PANIC_CODE: u8 = 3;
    const SENTINEL_FD_A: libc::c_int = 200;
    const SENTINEL_FD_B: libc::c_int = 201;
    const EXPECTED_TMPDIR: &str = "/private/tmp/pdf-rs-macos-spawn-harness";

    pub fn run() {
        if std::env::args().nth(1).as_deref() == Some("--pdf-rs-desktop-child") {
            restore_desktop_worker_signal_state()
                .expect("restore fixed worker signal state after Rust startup");
            std::process::exit(child_main());
        }
        proves_default_close_and_exact_stdio();
        proves_wait_status_and_signal_lifecycle();
        rejects_invalid_programs_without_fd_growth();
    }

    fn child_main() -> i32 {
        let input =
            rustix::io::dup(std::io::stdin()).expect("duplicate installed duplex worker socket");
        let input_fd = input.as_raw_fd();
        let mut socket = UnixStream::from(input);
        let mut command = [0_u8; 1];
        socket
            .read_exact(&mut command)
            .expect("receive harness command on fd 0");
        match command[0] {
            INSPECT_FDS => {
                let report = [
                    u8::from(only_expected_child_fds_are_open(input_fd)),
                    u8::from(raw_fd_is_closed(SENTINEL_FD_A)),
                    u8::from(raw_fd_is_closed(SENTINEL_FD_B)),
                    u8::from(same_file_as_dev_null(std::io::stdout())),
                    u8::from(same_file_as_dev_null(std::io::stderr())),
                    u8::from(argv_is_fixed()),
                    u8::from(environment_is_exact()),
                    u8::from(sigpipe_is_default()),
                    u8::from(signal_mask_is_empty()),
                ];
                socket
                    .write_all(&report)
                    .expect("return inherited-FD report over fd 0");
                0
            }
            EXIT_ZERO => 0,
            EXIT_PANIC_CODE => 71,
            other => panic!("unknown Darwin spawn harness command {other}"),
        }
    }

    fn only_expected_child_fds_are_open(socket_fd: libc::c_int) -> bool {
        // SAFETY: `getdtablesize` takes no pointers and returns this process's
        // descriptor-table bound.
        let limit = unsafe { libc::getdtablesize() };
        if limit <= socket_fd {
            return false;
        }
        for raw in 0..limit {
            // SAFETY: `fcntl(F_GETFD)` accepts every integer in the descriptor
            // table range and does not dereference caller memory.
            let result = unsafe { libc::fcntl(raw, libc::F_GETFD) };
            if matches!(
                raw,
                libc::STDIN_FILENO | libc::STDOUT_FILENO | libc::STDERR_FILENO
            ) || raw == socket_fd
            {
                if result < 0 {
                    return false;
                }
            } else if result != -1
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::EBADF)
            {
                return false;
            }
        }
        true
    }

    fn raw_fd_is_closed(raw: libc::c_int) -> bool {
        // SAFETY: `fcntl(F_GETFD)` accepts any integer descriptor, including an
        // intentionally invalid one, and does not dereference caller memory.
        let result = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
    }

    fn same_file_as_dev_null(fd: impl AsFd) -> bool {
        let actual = rustix::fs::fstat(fd).expect("fstat installed stdio");
        let null = File::options()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open comparison /dev/null");
        let expected = rustix::fs::fstat(null).expect("fstat /dev/null");
        actual.st_dev == expected.st_dev && actual.st_ino == expected.st_ino
    }

    fn argv_is_fixed() -> bool {
        let argv = std::env::args_os().collect::<Vec<_>>();
        argv.len() == 2 && Path::new(&argv[0]).is_absolute() && argv[1] == "--pdf-rs-desktop-child"
    }

    fn environment_is_exact() -> bool {
        let environment = std::env::vars_os().collect::<Vec<_>>();
        environment.len() == 1
            && environment[0].0 == "TMPDIR"
            && environment[0].1 == EXPECTED_TMPDIR
    }

    fn sigpipe_is_default() -> bool {
        let mut action = MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: a null action pointer requests a read-only query, while
        // `action` points to writable storage of the exact Darwin ABI type.
        let result = unsafe { libc::sigaction(libc::SIGPIPE, ptr::null(), action.as_mut_ptr()) };
        if result != 0 {
            return false;
        }
        // SAFETY: successful `sigaction` initialized the output structure.
        unsafe { action.assume_init() }.sa_sigaction == libc::SIG_DFL
    }

    fn signal_mask_is_empty() -> bool {
        let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: a null set pointer requests a read-only query, while `mask`
        // points to writable storage of the exact Darwin ABI type.
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, ptr::null(), mask.as_mut_ptr()) };
        // Darwin's `sigset_t` is the complete signal bit mask; zero means no
        // blocked signals.
        result == 0 && unsafe { mask.assume_init() } == 0
    }

    fn spawn_harness() -> (SpawnedChild, UnixStream) {
        let program = std::env::current_exe().expect("absolute current harness path");
        let (host, worker) = UnixStream::pair().expect("private worker socketpair");
        let child =
            spawn_desktop_worker(&program, worker.as_fd()).expect("spawn Darwin test worker");
        drop(worker);
        (child, host)
    }

    fn proves_default_close_and_exact_stdio() {
        let tmpdir_guard = TmpdirGuard::install();
        let signal_guard = ParentSignalStateGuard::install();
        let null = File::options()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open sentinel source");
        let sentinel_a = rustix::io::fcntl_dupfd_cloexec(&null, SENTINEL_FD_A)
            .expect("install high sentinel fd 200");
        assert_eq!(sentinel_a.as_raw_fd(), SENTINEL_FD_A);
        let sentinel_b = rustix::io::fcntl_dupfd_cloexec(&null, SENTINEL_FD_B)
            .expect("install high sentinel fd 201");
        assert_eq!(sentinel_b.as_raw_fd(), SENTINEL_FD_B);
        for sentinel in [&sentinel_a, &sentinel_b] {
            rustix::io::fcntl_setfd(sentinel, FdFlags::empty()).expect("clear sentinel CLOEXEC");
            assert!(
                !rustix::io::fcntl_getfd(sentinel)
                    .expect("sentinel fd flags")
                    .contains(FdFlags::CLOEXEC),
                "sentinel must challenge default-close rather than ordinary CLOEXEC"
            );
        }

        let (mut child, mut socket) = spawn_harness();
        drop(signal_guard);
        drop(tmpdir_guard);
        socket
            .write_all(&[INSPECT_FDS])
            .expect("exercise fd0 Host-to-worker direction");
        let mut report = [0_u8; 9];
        socket
            .read_exact(&mut report)
            .expect("exercise fd0 worker-to-Host direction");
        assert_eq!(report, [1; 9]);
        assert_eq!(child.wait().expect("reap fd probe").code(), Some(0));
        assert_eq!(
            child.try_wait().expect("cached fd probe status"),
            Some(child.wait().expect("same cached fd probe status"))
        );
    }

    fn proves_wait_status_and_signal_lifecycle() {
        let (mut zero, mut zero_socket) = spawn_harness();
        assert_eq!(zero.try_wait().expect("nonblocking live check"), None);
        zero_socket
            .write_all(&[EXIT_ZERO])
            .expect("request zero exit");
        let zero_status = zero.wait().expect("wait for zero exit");
        assert_eq!(zero_status.code(), Some(0));
        assert_eq!(
            zero.try_wait().expect("cached zero exit"),
            Some(zero_status)
        );

        let (mut panic_code, mut panic_socket) = spawn_harness();
        panic_socket
            .write_all(&[EXIT_PANIC_CODE])
            .expect("request reserved exit");
        let panic_status = panic_code.wait().expect("wait for reserved exit");
        assert_eq!(panic_status.code(), Some(71));

        let (mut killed, _blocked_socket) = spawn_harness();
        assert_eq!(killed.try_wait().expect("child blocks on fd0"), None);
        killed.kill().expect("SIGKILL live child");
        let killed_status = killed.wait().expect("reap killed child");
        assert_eq!(killed_status.signal(), Some(libc::SIGKILL));
        assert!(killed.kill().is_err(), "reaped PID must never be signaled");
    }

    fn rejects_invalid_programs_without_fd_growth() {
        let (_host, worker) = UnixStream::pair().expect("validation socketpair");
        let relative = spawn_desktop_worker(Path::new("relative-worker"), worker.as_fd())
            .expect_err("relative executable must fail");
        assert_eq!(relative.kind(), std::io::ErrorKind::InvalidInput);

        let missing = unique_missing_program();
        let failure = spawn_desktop_worker(&missing, worker.as_fd())
            .expect_err("missing absolute executable must fail");
        assert_eq!(failure.kind(), std::io::ErrorKind::NotFound);

        let nul = PathBuf::from(OsString::from_vec(
            b"/tmp/pdf-rs-macos-spawn-\0-invalid".to_vec(),
        ));
        let failure =
            spawn_desktop_worker(&nul, worker.as_fd()).expect_err("NUL executable must fail");
        assert_eq!(failure.kind(), std::io::ErrorKind::InvalidInput);

        let before_success = open_fd_count();
        for _ in 0..32 {
            let (mut child, mut socket) = spawn_harness();
            socket
                .write_all(&[EXIT_ZERO])
                .expect("request repeated zero exit");
            assert_eq!(child.wait().expect("reap repeated child").code(), Some(0));
        }
        assert_eq!(
            open_fd_count(),
            before_success,
            "successful spawn/reap cycles leaked descriptors"
        );

        let before = open_fd_count();
        for _ in 0..64 {
            let failure = spawn_desktop_worker(&missing, worker.as_fd())
                .expect_err("repeated missing executable must fail");
            assert_eq!(failure.kind(), std::io::ErrorKind::NotFound);
        }
        assert_eq!(open_fd_count(), before, "spawn failures leaked descriptors");

        let original_tmpdir = std::env::var_os("TMPDIR");
        // SAFETY: this harness has no test runner or background threads and
        // performs no concurrent environment access while the value is set.
        unsafe { std::env::set_var("TMPDIR", "relative-tmpdir-is-rejected") };
        let failure = spawn_desktop_worker(&missing, worker.as_fd())
            .expect_err("relative TMPDIR must fail before posix_spawn");
        assert_eq!(failure.kind(), std::io::ErrorKind::InvalidInput);
        // SAFETY: the same single-threaded harness invariant applies while the
        // original process environment is restored.
        unsafe {
            match original_tmpdir {
                Some(value) => std::env::set_var("TMPDIR", value),
                None => std::env::remove_var("TMPDIR"),
            }
        }
    }

    fn unique_missing_program() -> PathBuf {
        std::env::temp_dir().join(format!("pdf-rs-macos-spawn-missing-{}", std::process::id()))
    }

    fn open_fd_count() -> usize {
        std::fs::read_dir("/dev/fd")
            .expect("enumerate process descriptors")
            .count()
    }

    struct TmpdirGuard {
        original: Option<OsString>,
    }

    impl TmpdirGuard {
        fn install() -> Self {
            let original = std::env::var_os("TMPDIR");
            // SAFETY: this harness has no test runner or background threads
            // and performs no concurrent environment access.
            unsafe { std::env::set_var("TMPDIR", EXPECTED_TMPDIR) };
            Self { original }
        }
    }

    impl Drop for TmpdirGuard {
        fn drop(&mut self) {
            // SAFETY: the harness remains single-threaded while restoring the
            // exact parent environment.
            unsafe {
                match self.original.take() {
                    Some(value) => std::env::set_var("TMPDIR", value),
                    None => std::env::remove_var("TMPDIR"),
                }
            }
        }
    }

    struct ParentSignalStateGuard {
        previous_pipe: libc::sigaction,
        previous_mask: libc::sigset_t,
    }

    impl ParentSignalStateGuard {
        fn install() -> Self {
            let mut ignored_mask = MaybeUninit::<libc::sigset_t>::uninit();
            // SAFETY: the pointer targets writable storage of the exact ABI
            // type and is read only after successful initialization.
            assert_eq!(unsafe { libc::sigemptyset(ignored_mask.as_mut_ptr()) }, 0);
            // SAFETY: the preceding call initialized the signal set.
            let ignored_mask = unsafe { ignored_mask.assume_init() };
            let ignored = libc::sigaction {
                sa_sigaction: libc::SIG_IGN,
                sa_mask: ignored_mask,
                sa_flags: 0,
            };
            let mut previous_pipe = MaybeUninit::<libc::sigaction>::uninit();
            // SAFETY: both action pointers reference initialized/readable or
            // writable Darwin `sigaction` storage.
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGPIPE, &ignored, previous_pipe.as_mut_ptr()) },
                0
            );

            let mut blocked = MaybeUninit::<libc::sigset_t>::uninit();
            // SAFETY: initialize the complete Darwin signal-mask value.
            assert_eq!(unsafe { libc::sigemptyset(blocked.as_mut_ptr()) }, 0);
            // SAFETY: the set is initialized and SIGUSR1 is valid.
            assert_eq!(
                unsafe { libc::sigaddset(blocked.as_mut_ptr(), libc::SIGUSR1) },
                0
            );
            let mut previous_mask = MaybeUninit::<libc::sigset_t>::uninit();
            // SAFETY: the blocked set is initialized and the output pointer is
            // writable for the exact Darwin ABI type.
            assert_eq!(
                unsafe {
                    libc::pthread_sigmask(
                        libc::SIG_BLOCK,
                        blocked.as_ptr(),
                        previous_mask.as_mut_ptr(),
                    )
                },
                0
            );

            Self {
                // SAFETY: successful APIs initialized both output values.
                previous_pipe: unsafe { previous_pipe.assume_init() },
                // SAFETY: successful APIs initialized both output values.
                previous_mask: unsafe { previous_mask.assume_init() },
            }
        }
    }

    impl Drop for ParentSignalStateGuard {
        fn drop(&mut self) {
            // SAFETY: both saved values were returned by the matching Darwin
            // APIs and remain live for the complete restore calls.
            let _ = unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous_mask, ptr::null_mut())
            };
            // SAFETY: restore the exact prior SIGPIPE disposition; no output
            // structure is requested.
            let _ = unsafe { libc::sigaction(libc::SIGPIPE, &self.previous_pipe, ptr::null_mut()) };
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
