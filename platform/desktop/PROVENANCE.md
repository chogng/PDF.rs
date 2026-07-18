# Desktop transport provenance

M4-09 owns the host-to-child Native worker boundary only. It reuses the
canonical generated protocol frame validator and never parses PDF bytes,
renders, opens local paths, or imports an external PDF engine.

The host owns immutable source snapshots and all opaque capability backing.
The child receives only read-only shared-memory descriptors through Unix
`SCM_RIGHTS`, authenticated by a launch-private inherited `socketpair`, sender
PID, launch token, direction, and epoch. POSIX shared memory is created RW,
written, independently reopened RDONLY, and unlinked before transfer. Sandboxed
macOS hosts which deny `shm_open` use the same unlink-before-transfer and
independent-RDONLY-reopen invariant with a private temporary file.

`rustix 1.1.4` is deliberately the sole platform boundary: its safe owned-FD
APIs replace raw libc calls and ensure rejected or extra descriptors close
immediately. The hard per-record descriptor cap is 64, intentionally below
platform ancillary-message limits.

On macOS, `SOCK_CLOEXEC` is unavailable. The desktop worker spawn path
serializes its socketpair-to-exec interval, marks both original endpoints
close-on-exec, and lets `Stdio` install only the child endpoint as fd 0/1.
This constrains concurrent spawns through this crate; a future OS sandbox gate
must still prevent unrelated host threads from deliberately exporting FDs.
