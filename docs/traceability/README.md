# Traceability ledgers

These files are the machine-readable starting point for the audit chain defined by
[`docs/standards/traceability-and-provenance.md`](../standards/traceability-and-provenance.md).
Some ledgers now contain implementation records while approval-sensitive ledgers
remain empty. The dependency ledger separately records conditional non-product fuzz inputs;
neither record enters a product closure. `status = "active"` means records exist, not that their requirements
are covered or release-approved; `status = "initial"` means only that the ledger
schema exists. In particular, the baseline ledger remains empty until a reviewed
external executable and complete fingerprint are available.

Each ledger uses schema version `1` and a versioned document revision. Records use
the following array-of-table forms:

| File | Record form |
| --- | --- |
| `spec-map.toml` | `[[requirement]]` |
| `feature-map.toml` | `[[feature]]` |
| `capability-profiles.toml` | `[[profile]]` |
| `dependency-ledger.toml` | `[[dependency]]` |
| `data-ledger.toml` | `[[data]]` |
| `baseline-ledger.toml` | `[[baseline]]` |

Record fields and approval rules come from the governance standard. In particular,
an empty ledger must not be interpreted as an allowlist, a release approval, or
evidence that a requirement is covered. Increment `version` for semantic ledger
changes and increment `schema` only for incompatible structural changes.

`capability-profiles.toml` freezes the supported and excluded surface of the M1 strict,
local-repair, page-count, and outline capabilities. The PR lane validates it before product build
proof. A profile cannot be relabelled `REFERENCE` without O0/O1 cases, a concrete
reference/target pair, and independent review. `DIFFERENTIAL` additionally requires O2
adjudication, registered fuzz/minimization, at least two disjoint content-addressed holdouts,
eligible benchmark and differential reports, and a complete reference fingerprint. Strict R0 and
bounded local R1 are registered at `REFERENCE`; strict page-count and outline are registered at
`DIFFERENTIAL` through an atomic two-gate review with no product or CANARY exposure. The other M1
component profiles remain `PLANNED`. PDFium observations remain outside this graph as unregistered,
non-gating O4 probes and are not correctness or release oracles.

The M2 Scene gate is milestone evidence, not a maturity promotion. It registers exactly six
self-authored valid, invalid, unsupported, resource, cancellation, and source-change cases, with
six input hashes and two canonical Scene hashes. Each gate invocation uses two fresh strict
pipelines; CI performs two debug and two release invocations and byte-compares the normalized
artifacts for profile-stable replay. The linked features therefore remain `PLANNED`. This closure
does not include paths, painting, clipping, text showing, fonts, images, Forms, rendering, broader
resources, product Session/IPC, or browser and desktop integration.

The [font/text roadmap audit](font-text-roadmap-audit.md) records the current implementation and
evidence boundary, the M4/M5 non-expansion decision, the executable M6 and Post-R0 delivery plans,
the advanced encoding/CID matrix, the authoring-only shaping/writer boundary, the controlled
system-font-fallback decision and conditional implementation gates, and the deferred traceability
updates that must not rewrite the stored, superseded hash-bound M4 candidate.
