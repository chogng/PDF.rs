# Provenance

`pdf-rs-engine` is a project-owned Native worker/session integration layer.

It composes the project protocol validator, capability policy, RenderPlan,
scheduler, tile cache, Fast rasterizer, and Surface owner. It contains no
third-party PDF engine, external-renderer adapter, filesystem access, network
access, or platform transport implementation.

Capability evaluation, RenderPlan construction, and Fast raster jobs execute
outside the actor behind bounded move-only permits. Policy results return as
opaque, indivisible completions, while raster work reserves both worst-case
intermediate and retained bytes. Cache-hit pixel copies advance in bounded
chunks so lifecycle traffic can preempt them between actor turns. Ready Scene
sets reuse their admitted vector as an in-place sorted index and charge a
per-Scene ownership floor, avoiding unbudgeted tree-node allocation.
