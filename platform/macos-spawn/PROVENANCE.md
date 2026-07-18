# Darwin desktop spawn boundary

This private crate is the repository-owned unsafe boundary for one fixed macOS
desktop-worker launch. It does not expose a general command builder, PATH
search, inherited environment, arbitrary file actions, or sandbox controls.

`spawn_desktop_worker` accepts only an absolute executable path and one borrowed
Unix worker socket. It calls Darwin `posix_spawn` with
`POSIX_SPAWN_CLOEXEC_DEFAULT`, an exact two-element argv, an environment
containing at most one validated absolute `TMPDIR`, SIGPIPE restored to default,
and an empty signal mask. File actions install only the duplex worker socket as
fd 0 and a parent-opened `/dev/null` as fd 1 and fd 2. Both action source
descriptors are owned CLOEXEC duplicates at or above fd 3 and are explicitly
closed after the dup actions.

The safe `SpawnedChild` owns a positive child PID and caches the first terminal
wait status. `try_wait`, `kill`, and `wait` use safe `rustix` process APIs.
Dropping the handle neither signals nor reaps the child, matching the ownership
assumption of `std::process::Child`; the desktop supervisor remains responsible
for terminal cleanup before restart. The desktop owner treats an unresolved
post-spawn cleanup or final Drop as a Host fail-stop condition, so this
non-reaping handle is never discarded while the child may still be live.

The harness challenges the spawn boundary with two inherited non-CLOEXEC high
descriptors, a parent-ignored SIGPIPE disposition, a blocked parent signal, and
a fixed absolute `TMPDIR`. Rust startup ignores SIGPIPE after the spawn
attributes run, so the harness and real desktop worker immediately call the
boundary's safe `restore_desktop_worker_signal_state` entry hook before
touching inherited transport. The child then enumerates its descriptor table and proves
that only fd 0/1/2 plus its post-exec probe-socket duplicate are open, then
checks the exact environment, default SIGPIPE disposition, and empty signal
mask. It also covers duplex fd 0, `/dev/null` output descriptors, terminal
status caching, SIGKILL, invalid inputs, and descriptor stability.

The unsafe invariants are local to initialized spawn attribute/action guards,
live CString pointer arrays, valid borrowed descriptor lifetimes, and direct
POSIX error-number handling. A successful spawn is never converted into an
error because guard destruction failed. This boundary supplies FD hygiene only.
It is not App Sandbox enforcement, code-signing evidence, or a filesystem or
network denial proof.
