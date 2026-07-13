# Test suites

The repository follows RPE-STD-003's layered layout. Every non-trivial fixture
under `cases/` carries a validated `case.toml`; generated inputs keep their
executable source and provenance beside the bytes. CI replays the bounded M0 DSL
source before validating its output. The generated PDF remains locally ignored
until its custom test-data license is approved for redistribution.

The canonical T0 corpus manifest under `corpus/manifests/` binds that generated
object by SHA-256, tier, page count, provenance, access, redistribution, feature
tags, path, and an object byte ceiling. CI regenerates and re-hashes the object;
the manifest does not make the PDF redistributable.

- `cases/`: T0 atomic and regression fixtures.
- `models/`, `properties/`, `metamorphic/`: deterministic model-based suites.
- `lifecycle/`: controlled scheduler, Range, cancellation, and resource tests.
- `browser/`, `desktop/`: platform end-to-end suites.
- `fuzz/`: targets, dictionaries, seeds, owners, and minimizer contracts.
- `corpus/manifests/`: canonical hashed corpus indices; M0 currently contains
  only the generated T0 object, while licensed T1-T3 indices remain pending.
- `performance/`: benchmark-report contract fixtures; M0 currently contains
  only canonical synthetic pipeline validation data that is explicitly
  ineligible for performance decisions or release evidence.

An empty suite is pending, not passing. CI lanes must report why each suite was
selected or explicitly unavailable.
