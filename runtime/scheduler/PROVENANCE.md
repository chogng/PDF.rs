# Purpose

`pdf-rs-scheduler` owns the bounded deterministic viewport work queue and its single terminal
arbiter. It orders replaceable viewport and tile jobs, reserves queue space per session, applies
bounded cross-session fairness, and prevents stale generations from publishing completed
resources.

# Dependency direction

The crate has no product or third-party dependencies. It consumes only scheduler metadata and
opaque integer identities; render plans, pixels, protocol messages, platform handles, clocks,
threads, and I/O remain outside this boundary. A later runtime integration may project accepted
Native `RenderPlan` identities into these opaque scheduler identities.

# Determinism and time

All order is derived from canonical integer fields. The normal scheduling key includes bounded
aging, P0-P4 priority, predicted scroll relation, center distance, edge distance, enqueue order,
session identity, and work identity. Cross-session last-service turns restrict the candidates
that may use that key, bounding dispatch skew while multiple sessions remain backlogged without
making a newly active session repay historical idle time.

Time is a caller-advanced virtual tick. The crate does not read a wall clock, spawn a thread,
sleep, perform I/O, or depend on allocator addresses or hash iteration order.

# Capacity and lifecycle

Normal queue, per-session queue, session registry, work-ID history, in-flight work, and critical
queue capacities are validated before use. Registering a session precharges its normal-queue
reservation; shared capacity cannot consume another registered session's unused reservation.
Cancel, close, release, failure, completion, and shutdown ingress uses a dedicated bounded
critical queue which normal work can never occupy.

`TerminalArbiter` is the only component allowed to turn completion into publication. It
matches the complete work/session/generation identity, checks the current generation and
lifecycle, removes the in-flight identity exactly once, and returns either `Publish` or
`DiscardAndRelease`. Generation replacement, close, and shutdown therefore cannot leak a stale
completion into the visible stream.

# Known limitations

- Scheduling metadata is opaque; the runtime integration must retain the full immutable
  `RenderPlan` and complete tile identity outside this crate.
- Reservations cover queued normal work. In-flight work has its own independent global bound.
- Fairness is per registered session rather than weighted by document size or viewport area.
- Work and session identities are never reusable within one scheduler instance; integrations
  create a new instance for a new Worker epoch.
