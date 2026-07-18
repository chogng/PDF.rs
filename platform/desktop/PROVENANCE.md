# Desktop transport provenance

M4-09 owns the host-to-child Native worker boundary. The current
`spawn_transport_fixture` and `start_transport_fixture` entrypoints prove the
process transport but do not claim filesystem or network isolation. They are
compiled only by the default-off `transport-fixture` feature; the product
release build does not enable that feature. The
separate `start_product_macos` entrypoint cannot accept a caller-provided flag,
environment value, or self-authored attestation and currently returns stable
`IsolationUnavailable` before spawn. Behind that gate, the selected product
launch branch is wired to the repository-owned Darwin default-close wrapper;
the unavailable gate still prevents it from executing in this unsigned
workspace.

The selected product target is
`macos/sandbox-target.toml`: a signed macOS App Sandbox host acquires a local
file through NSOpenPanel/PowerBox and directly launches an embedded helper
signed with exactly `com.apple.security.app-sandbox` and
`com.apple.security.inherit`. The helper receives neither the source path nor
the host's dynamic PowerBox extension. Neither entitlement template grants
network access, arbitrary external filesystem access, an app group, or a
temporary sandbox exception. The worker may use only storage made available by
its inherited parent-app container and sandbox-provided temporary directory.
The repository does not yet contain the signed parent app/package or live
denial evidence, so M4-09 remains in progress and product launch fails closed.

The transport reuses the canonical
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
Packaged sandbox closure must record which Surface backend actually ran and
prove the worker's sandbox-provided TMPDIR fallback, read-only reopen, unlink
before `SCM_RIGHTS`, receiver rights/extent validation, and zero residual
objects after child exit. It may not add an app group or temporary exception to
make `/pdf-rs-*` shared-memory names available.

The desktop crate keeps `forbid(unsafe_code)`. It uses `rustix 1.1.4` safe
owned-FD APIs so rejected or extra transport descriptors close immediately.
The separate private `pdf-rs-macos-spawn` crate is the sole repository-owned raw
Darwin FFI boundary: it uses registered `libc 0.2.186` declarations only for
fixed `posix_spawn` attributes and file actions plus the matching worker-entry
signal reset, then returns a safe rustix-owned process lifecycle. The hard
per-record descriptor cap is 64, intentionally below platform
ancillary-message limits.

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
count nor starts a replacement. Worker and epoch identities are checked and
consumed before each launch attempt, so any failed attempt leaves a gap instead
of permitting a later child to reuse an identity that may already have been
observed. If post-spawn socket setup or bootstrap cleanup cannot prove reap, or
if a Host process or supervisor is dropped while it still owns such a child,
the Host aborts rather than return or finish destruction without a child owner.
Failure-injection models cover reap, kill, and wait failures; childless
termination stubs do not trigger the abort policy. A subprocess probe retains a
real `Child`, forces cleanup failure, and observes SIGABRT from final Host Drop.

On macOS, `SOCK_CLOEXEC` is unavailable. The transport-fixture spawn path
serializes its socketpair-to-exec interval, marks both original endpoints
close-on-exec, and lets `Stdio` install only the child endpoint as fd 0/1.
This constrains concurrent spawns through this crate, but unrelated host
threads can still deliberately export a non-CLOEXEC descriptor. The product
branch instead uses Darwin `POSIX_SPAWN_CLOEXEC_DEFAULT`: fixed file actions
install the worker socket only as fd 0, install `/dev/null` as fd 1/2, and close
private action sources at or above fd 3. Its real harness clears CLOEXEC on
sentinel fd 200/201 and proves EBADF plus an exact fd 0/1/2-and-probe-socket
descriptor set in the child. It also proves fd0 duplex transport, `/dev/null`
stdout/stderr, exact argv and `TMPDIR` propagation, SIGPIPE reset from ignored
to default through both the spawn attribute and the worker binary's first
safe call back into the private signal boundary, an empty child mask despite a
blocked parent signal, raw exit/signal mapping, cached wait, invalid-path
rejection, and stable descriptor count across failed spawns. The remaining
cross-process race is the non-atomic macOS socketpair-to-fcntl interval against
a foreign concurrent fork; the packaged product Host must own process
creation. This is FD hygiene only, not an operating-system sandbox. Deprecated
sandbox tooling and private Seatbelt APIs are not product mechanisms.
