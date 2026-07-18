# Desktop transport provenance

M4-09 owns the host-to-child Native worker boundary. It reuses the canonical
generated protocol validator and the repository's Native engine, syntax,
Scene, policy, and raster crates. The current vertical fixture validates the
PDF header and builds a self-authored nonblank Scene; it does not claim general
PDF document parsing and imports no external PDF engine.

The host owns immutable source snapshots. The child never receives a source
path or original source file descriptor; it receives only exact ticket-bound
read-only range segments through Unix
`SCM_RIGHTS`, authenticated by a launch-private inherited `socketpair`, sender
PID, launch token, direction, and epoch. POSIX shared memory is created RW,
written, independently reopened RDONLY, and unlinked before transfer. Sandboxed
macOS hosts which deny `shm_open` use the same unlink-before-transfer and
independent-RDONLY-reopen invariant with a private temporary file.

`rustix 1.1.4` is deliberately the sole platform boundary: its safe owned-FD
APIs replace raw libc calls and ensure rejected or extra descriptors close
immediately. The hard per-record descriptor cap is 64, intentionally below
platform ancillary-message limits.

The worker publishes each Native Surface through a fresh unlinked read-only
shared object. Logical and allocator-capacity checks cover the simultaneous
pixel import, destination buffer, and shared-memory staging extent. Delivery
failure reclaims the exact undelivered Surface lease before the process faults.

The Host supervisor maps unexpected EOF, nonzero exit, contained panic, and
transport watchdog expiry to stable content-free `WorkerFault` diagnostics.
Each failure first waits or terminates the old process and retires its
capability and Range owners, then performs at most one replacement spawn with a
strictly newer Worker identity and epoch. A lineage defaults to two
replacements and has a hard cap of eight; graceful shutdown suppresses restart,
and stale old-epoch records and descriptors are rejected before the new socket
is touched.

Restart requires an explicit successful `try_wait` or `wait` reap result. A
reap, kill, or wait failure retains the old child handle and Host resource
ownership, enters `RestartFailed`, and neither increments the restart attempt
count nor starts a replacement.

On macOS, `SOCK_CLOEXEC` is unavailable. The desktop worker spawn path
serializes its socketpair-to-exec interval, marks both original endpoints
close-on-exec, and lets `Stdio` install only the child endpoint as fd 0/1.
This constrains concurrent spawns through this crate; a future OS sandbox gate
must still prevent unrelated host threads from deliberately exporting FDs.
