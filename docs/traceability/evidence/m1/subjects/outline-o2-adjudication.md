# Outline O2 adjudication

The content-addressed input `document/m1-adjudication/ill-typed-optional-references` places integer `42` in the optional `Catalog.Outlines` slot. The frozen pre-fix projection treated that present-but-ill-typed value as absent and returned an empty outline. Native instead returns `RPE-DOCUMENT-0036`, and the independent reference returns its semantic-rejection outcome on the identical bytes.

Commit `a5ec35b` is the correction identity for the independent reference's absent/null/typed/ill-typed split. `same_input_ill_typed_optional_reference_adjudication_replays` hashes the pre-fix, Native, and independent-reference canonical outputs and binds them to the adjudication report. `service-evidence-review` reviewed the correction, and `maturity-evidence-review` independently reviewed the same-input O2 oracle semantics. That scoped verdict is distinct from the final complete-evidence-graph review.

This is project-internal O2 evidence. `external_observation=false`; PDFium and every O4 probe remain excluded from the promotion graph.
