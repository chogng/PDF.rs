# Test suites

The repository follows RPE-STD-003's layered layout. Every non-trivial fixture
under `cases/` carries a validated `case.toml`; generated inputs keep their source
description and provenance beside the bytes. The M0 input is generated locally by
the CI entry point and intentionally ignored until its custom test-data license is
approved for redistribution.

- `cases/`: T0 atomic and regression fixtures.
- `models/`, `properties/`, `metamorphic/`: deterministic model-based suites.
- `lifecycle/`: controlled scheduler, Range, cancellation, and resource tests.
- `browser/`, `desktop/`: platform end-to-end suites.
- `fuzz/`: targets, dictionaries, seeds, owners, and minimizer contracts.
- `corpus/manifests/`: licensed, hashed T1-T3 corpus indices.
- `performance/`: fixed-scenario component and user-path benchmarks.

An empty suite is pending, not passing. CI lanes must report why each suite was
selected or explicitly unavailable.
