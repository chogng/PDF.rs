# Traceability ledgers

These files are the machine-readable starting point for the audit chain defined by
[`docs/standards/traceability-and-provenance.md`](../standards/traceability-and-provenance.md).
Some ledgers now contain implementation records while approval-sensitive ledgers
remain empty. `status = "active"` means records exist, not that their requirements
are covered or release-approved; `status = "initial"` means only that the ledger
schema exists. In particular, the baseline ledger remains empty until a reviewed
external executable and complete fingerprint are available.

Each ledger uses schema version `1` and a versioned document revision. Records use
the following array-of-table forms:

| File | Record form |
| --- | --- |
| `spec-map.toml` | `[[requirement]]` |
| `feature-map.toml` | `[[feature]]` |
| `dependency-ledger.toml` | `[[dependency]]` |
| `data-ledger.toml` | `[[data]]` |
| `baseline-ledger.toml` | `[[baseline]]` |

Record fields and approval rules come from the governance standard. In particular,
an empty ledger must not be interpreted as an allowlist, a release approval, or
evidence that a requirement is covered. Increment `version` for semantic ledger
changes and increment `schema` only for incompatible structural changes.
