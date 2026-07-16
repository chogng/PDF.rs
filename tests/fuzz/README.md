# Parser mutation registry

No fuzz target was registered for the completed M0 gate. Its feature-map records therefore retain
empty `fuzz_targets` arrays.

`tools/quality/tests/parser_mutation_smoke.rs` is an ordinary deterministic property/component
test with profile `m0.parser-mutation-smoke.v1`. It reuses the canonical generator DSL, T0 corpus
manifest, and synthetic benchmark report already governed elsewhere in the repository; it does not
copy or acquire a new seed corpus.

For each parser, the test applies exactly 103 fixed mutations at seven stable byte anchors:
truncation, deletion, selected byte replacement, insertion of six named boundary tokens, fixed
eight-byte duplication, and one input-limit violation. Mutants are produced and discarded one at a
time, never exceed 4,096 bytes, and are parsed twice under an explicit 2,048-byte codec limit.
Canonical seeds must succeed; a mutation may succeed or return a stable non-internal error. Accepted
artifacts and complete redacted error fingerprints must be byte-for-byte repeatable.

This M0 smoke test has no coverage guidance, random loop, time budget, corpus growth, sanitizer,
dictionary, automatic minimizer, structure-aware failure bundle, or nightly campaign. A mutation ID
is replayable but is not a minimization result. Passing this test is not continuous-fuzz evidence,
does not satisfy a release fuzz gate, and does not make any parser safe for untrusted production
input.

M1 separately registers `tools/quality/fuzz/fuzz_targets/m1_document_services.rs` with three
content-addressed seeds, a fixed 64-run coverage-guided replay, and a real `cargo fuzz cmin` corpus
minimization that retained three coverage-selected outputs. That bounded, non-product campaign is
part of the M1 page-count and outline maturity graph; continuous or nightly fuzzing, sanitizer
evidence, long-running campaign-level watchdog supervision, and wider compatibility coverage remain
open.
