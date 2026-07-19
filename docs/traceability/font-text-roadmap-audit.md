# Font/Text roadmap audit

- Audit date: 2026-07-18
- Repository revision audited: `4b235ab8b8d0bd11988bdb7ca10363ede0b0225b`
- Local PDFium observation revision: `c040cf96106a87220b814a1a892649cf2d7f1934`
- Scope: implementation, tests, milestone plans, capability/release profiles, protocol, platform surfaces, and RPE-ARCH-001
- Decision: M4/M5 remain pixel-vertical-slice milestones; M6 owns R0 horizontal font/text semantics and interaction; FT1 owns advanced Post-R0 text/structure

This record is a roadmap and traceability audit, not capability-maturity evidence. It does not promote a profile, approve a release, or change a historic review.

## 1. Audited conclusion

| Area | Evidence-backed current state | Stage owner |
| --- | --- | --- |
| Embedded horizontal glyph rendering | M3 accepted a bounded embedded simple TrueType horizontal rendering path with deterministic project-owned outlines and no system font, shaping, hinting, or platform antialiasing dependency. | Complete only for the registered M3 milestone subset |
| Later rendering primitives | The current tree also implements full WinAnsi simple-font mapping, a bounded simple Type1C/CFF1 subset, and Identity-H/CIDFontType2 with `CIDToGIDMap Identity`. These have unit/integration tests, but their `m4.*` implementation profile names are not M4 milestone ownership or capability maturity. | Register and close under M6-01/M6-04 |
| CMap and Unicode semantics | The document layer currently decodes simple text as fixed one-byte codes and Identity-H as fixed two-byte big-endian codes. It has no general codespace/usecmap/cidchar/cidrange layer and no ToUnicode semantic mapper. | M6-02/M6-03 |
| Text semantic model | Scene glyphs retain outline, transform, glyph id, and character code. They do not retain Unicode sequence, mapping confidence, CID, quad, baseline, writing mode, font identity, MCID, or source/logical/visual order. | M6-05 |
| Text product behavior | The canonical Engine protocol and browser/desktop surfaces have no text-page, selection, copy, search, or link messages/layers. Current product pages are Canvas/surface presentation. | M6-06 through M6-09 |
| Advanced text/structure | RTL/bidi order, vertical CMaps/CIDFont metrics, ActualText replacement, Tagged PDF structure order, and structure-derived accessibility are not implemented or evidenced. | FT1, Post-R0 |
| System-font fallback | No product font dependency or fallback is present. Unsupported/missing fonts remain explicit. | Keep fail-closed for R0; FT1-07 is a separate decision gate |

## 2. Evidence boundary

### 2.1 What M3 completed

[`plan/m3.toml`](../../plan/m3.toml) registers “Basic embedded simple-font text showing with project-owned deterministic glyph outlines and no operating-system font or hinting dependency.” Its excluded scope names system-font fallback, operating-system hinting/antialiasing, advanced CMap/shaping, and complete extraction/accessibility.

M3-09 and the independent review under
[`docs/traceability/evidence/m3/basic-embedded-text/`](evidence/m3/basic-embedded-text/)
therefore establish only:

- embedded simple-font horizontal text showing;
- project-owned deterministic outlines and PDF horizontal positioning;
- bounded Reference pixel behavior for the registered cases;
- no system font, shaping, platform hinting, or platform antialiasing dependency.

The independent review is milestone evidence for M3-09. It explicitly does not promote a broad font/text capability. In
[`feature-map.toml`](feature-map.toml), `core.basic-embedded-text` remains `PLANNED`; the only registered M3 Reference capability in
[`capability-profiles.toml`](capability-profiles.toml) is the bounded raster profile `m3.reference-raster-v1.v1`.

### 2.2 What exists after M3 but is not mature

The audited tree contains the following additional primitives:

- [`core/font/src/model.rs`](../../core/font/src/model.rs) exposes `SimpleTrueTypeWinAnsiV1`, `SimpleType1CStandardV1`, and `CidFontType2IdentityV1`;
- [`core/document/src/font_resource.rs`](../../core/document/src/font_resource.rs) acquires simple fonts and Identity-H Type0 descendants;
- [`core/content/src/lib.rs`](../../core/content/src/lib.rs) and the text executor emit horizontally positioned glyph uses;
- [`core/font/tests/`](../../core/font/tests/), [`core/document/tests/font_resource.rs`](../../core/document/tests/font_resource.rs), and [`core/content/tests/vm_graphics.rs`](../../core/content/tests/vm_graphics.rs) cover WinAnsi, Type1C, indirect font objects/encodings, and Identity-H rendering.

The implementation identifiers use an `m4.*` prefix, but [`plan/m4.toml`](../../plan/m4.toml) registers only the M3 Reference graphics subset and explicitly excludes system-font fallback, selection, copy, search, links, accessibility, advanced text, and the M6 release experience. A code-local profile string does not override the milestone plan or create maturity evidence.

These primitives are useful M6 inputs, but they currently prove neither:

- a general horizontal CMap layer;
- ToUnicode or trustworthy Unicode extraction;
- the complete R0 embedded Type 1/TrueType/OpenType/CFF/horizontal CIDFont combinations;
- TextAtom geometry/confidence;
- selection, copy, search, links, protocol, platform behavior, or release eligibility.

## 3. Missing model and product surfaces

The current mapping/rendering path is effectively:

```text
simple:     one byte -> simple encoding/profile -> glyph id -> outline -> horizontal PDF advance
Identity-H: two bytes, big endian -> same numeric glyph id -> outline -> horizontal PDF advance
```

The required R0 semantic path is:

```text
PDF bytes
  -> CMap codespace tokenization
  -> character code
  -> CID (for Type0/CIDFont)
  -> glyph id for drawing
  -> ToUnicode / declared encoding / registered CID mapping
  -> Unicode sequence + MappingConfidence
  -> TextAtom quad/baseline/source order
  -> selection/copy/search/link protocol and platform layers
```

The two branches must share source and geometry identities but must not be conflated. A glyph id or character code is not trusted Unicode, and a ToUnicode mapping must not silently replace the glyph selected for drawing.

## 4. Stage contract

| Stage | Required delivery | Explicitly excluded |
| --- | --- | --- |
| M3, complete bounded subset | Embedded simple-font deterministic horizontal glyph rendering | General CMap, ToUnicode, complete extraction, shaping, system fonts, advanced interaction |
| M4 | Fast CPU, cache/scheduler/surface, desktop Native pixel loop against registered M3 graphics scope | Font/text expansion, semantic text protocol, selection/copy/search/links, system-font fallback |
| M5 | Browser Native Worker, surface transports, thin viewer, three-engine pixel loop | Font/CMap/ToUnicode semantics in TS/main or Native; selection/copy/search/links; complete R0 text experience |
| M6/R0 | Horizontal CMap, ToUnicode/encoding Unicode semantics, complete registered embedded horizontal font profile, TextAtom, selection, copy, search, links, protocol/platform/release evidence | RTL, vertical CIDFont, ActualText, Tagged PDF, structure-derived accessibility, shaping existing PDF text, system-font fallback |
| FT1/Post-R0 | RTL/bidi, vertical CIDFont, ActualText, Tagged PDF/structure/accessibility; independent fallback decision gate | Retroactive R0 widening; authoring/reflow shaping; uncontrolled system fonts or external engine |

The executable plans are [`plan/m6.toml`](../../plan/m6.toml) and
[`plan/post-r0-font-text.toml`](../../plan/post-r0-font-text.toml).

## 5. Why the hash-bound M4 inputs were not rewritten

The stored M4 automated exit candidate
[`docs/traceability/evidence/m4/fast-cpu-canary/exit-candidate.toml`](evidence/m4/fast-cpu-canary/exit-candidate.toml)
content-addresses:

- `plan/m4.toml`;
- `plan/r0.toml`;
- `docs/traceability/capability-profiles.toml`.

Changing any of those bytes invalidates the candidate. Updating its hashes without replay and independent review would misrepresent evidence. The audit therefore:

- leaves the hash-bound, already explicit M4 exclusion unchanged and strengthens the unbound M5 plan to name CMap, ToUnicode, Unicode semantics, text protocol, and Native text expansion explicitly;
- does not add unevidenced R0 capability records to the frozen capability ledger;
- upgrades the mutable root roadmap to v0.4.0, links the new M6 plan, registers FT1, and thereby explicitly supersedes rather than silently rebinding the stored candidate;
- makes M6-01 responsible for registering `r0.font.horizontal.v1` and `r0.text.horizontal-ltr.v1` and obtaining fresh evidence without modifying historic review records.

The stored candidate was already stale at the audited base revision for unrelated committed runtime/viewer changes: `m4_exit` reported a hash mismatch for `runtime/viewer/tests/native_preview.rs`. The intentional `plan/r0.toml` v0.4.0 update now makes supersession explicit. Fresh replay/review is required; old evidence hashes remain untouched.

[`release/profiles/r0.toml`](../../release/profiles/r0.toml) already names those two R0 target profiles, but
[`capability-profiles.toml`](capability-profiles.toml) does not define them. They are therefore target identifiers in a `candidate` release profile, not current support or maturity claims. Release remains blocked until M6 registers and matures them.

## 6. Shaping boundary

For reading/rendering an existing PDF, the content stream and font resources are authoritative for character-code boundaries, glyph choice, advances, `TJ` adjustments, text matrices, rise, horizontal scaling, and vertical metrics. The renderer must not send the recovered Unicode through a general shaping engine and then use different glyphs or positions.

Shaping belongs to a different input/output contract:

- authoring new text;
- editing or reflowing content;
- generating a new PDF text run from Unicode.

Those tasks require script/language/direction/features/font selection, cluster mappings, subset/embed, widths, ToUnicode generation, and writer integration. They are not an implementation detail of M6 extraction or rendering and need a future authoring capability and plan.

RTL semantic ordering in FT1 also occurs after the drawing decision: it derives visual/logical mappings for interaction while preserving the glyph geometry already specified by the PDF.

## 7. System-font fallback decision

R0 should remain fail-closed for missing and non-embedded fonts.

Reasons:

- an uncontrolled host font can change glyph identity, metrics, line extent, selection geometry, pixels, search/copy confidence, and cache identity;
- browser and desktop font availability differs and can change after OS/font updates;
- a best-effort match can make CapabilityDecision a dangerous false positive;
- current M3 evidence and product provenance explicitly prove no system-font dependency.

FT1-07 is intentionally a decision gate, not an implementation promise. A future fallback can be considered only with its own ADR and profile. A qualifying design must bind a fixed font pack or platform fingerprint, file hashes, selection rules, metric compatibility, diagnostics, platform matrix, licenses, privacy, cache epoch, O0-O3 evidence, holdouts, and rollback. System font enumeration alone does not qualify.

## 8. PDFium behavior comparison, not dependency

The adjacent PDFium checkout was inspected only to validate decomposition and edge-case categories:

- `core/fpdfapi/font/cpdf_cmap.*` and `cpdf_cmapparser.*` separate CMap parsing/code-to-CID mapping;
- `core/fpdfapi/font/cpdf_tounicodemap.*` separately parses Unicode mapping;
- `core/fpdfapi/font/cpdf_cidfont.*` owns CID mapping, `CIDToGIDMap`, vertical writing, `W2`, and `DW2`;
- `core/fpdftext/cpdf_textpage.*`, `cpdf_textpagefind.*`, and `cpdf_linkextract.*` build text semantics, search, ActualText behavior, bidi order, and detected links above font rendering;
- `core/fpdfdoc/cpdf_structtree.*` and the public structure APIs treat Tagged PDF/ActualText structure as another subsystem;
- visible non-third-party HarfBuzz use is associated with font subsetting/editing support, while page rendering consumes PDF character positions; PDFium also contains system-font substitution, which PDF.rs deliberately does not adopt for R0.

No PDFium source, table, data, library, executable, runtime output, build step, or API is introduced by this audit. `../pdfium` remains a development/CI behavioral reference under the existing product-purity policy.

## 9. Verification obligations

M6 cannot exit until:

- both R0 target profiles are registered and reach the release-required maturity with independent review;
- CMap/ToUnicode/font/TextAtom parsers and transformations have normative, malformed, budget, property, fuzz, minimization, differential, and holdout evidence;
- horizontal selection/copy/search/link behavior passes desktop plus Chromium/Firefox/WebKit E2E;
- release-r0-v1 reports text-class denominators rather than diluting text failures with graphics-only pages;
- product dependency, runtime trace, font access, network, and build scans prove no system-font or external-engine fallback;
- fixed-hardware performance covers text-page construction and search first result in addition to rendering paths.

FT1 applies the same discipline independently to RTL, vertical, ActualText, Tagged PDF, accessibility, and any approved fallback profile.

## 10. Audit validation result

The following checks passed on the audited tree plus this documentation change:

- strict TOML parsing for `plan/m4.toml`, `plan/m5.toml`, `plan/r0.toml`, both new plans, the capability ledger, and the R0 release profile;
- `git diff --check` and local Markdown-link resolution;
- all unit, integration, repository-policy, and doc tests for `pdf-rs-font`, `pdf-rs-document`, and `pdf-rs-content`;
- the updated `m0_exit` roadmap-version/closure check;
- all four `m3_basic_text_trace` tests, including bounded-scope and review-binding checks.

Two existing evidence-closure checks are not green at the audited HEAD:

- `m3_exit`: 9/11 pass; the Reference maturity subject bindings are stale after committed raster changes (the first direct failure is `core/raster/src/reference/color.rs`);
- `m4_exit`: the review-contract test passes, the final-review test remains intentionally ignored, and the automated candidate fails because committed `runtime/viewer/tests/native_preview.rs` no longer matches the stored candidate hash.

Neither stale subject has an uncommitted change in this audit. Their latest commits are `8e5b100` (knockout-group raster work) and `bb35d2d` (viewer cancellation), respectively. The correct remedy is a separately authorized evidence replay/review or candidate supersession, not changing hashes in this roadmap audit.
