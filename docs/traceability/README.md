# Traceability ledgers

These files are the machine-readable starting point for the audit chain defined by
[`docs/standards/traceability-and-provenance.md`](../standards/traceability-and-provenance.md).
They intentionally contain no approved records yet; `status = "initial"` means the
ledger exists but does not claim coverage, approval, or baseline availability.

Each ledger uses schema version `1` and a versioned document revision. Add records
by replacing the corresponding empty array with array-of-table entries:

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
