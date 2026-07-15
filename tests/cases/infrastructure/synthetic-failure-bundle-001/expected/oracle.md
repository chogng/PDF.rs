# Oracle derivation

This case has O1 analytic authority for the deterministic failure-bundle
contract. The adjacent 612-byte PDF is generated from `source.dsl`, has four
generation-zero indirect objects, one 200-by-200-point page, and a `q Q` content
stream. Its visual page is blank. The Parse, Scene, Text, and Pixel artifacts
below are deliberate project-authored synthetic channel data; the Text and
Pixel values come from synthetic artifact constructors and are not claims about
extracting or rendering the blank PDF.

## Parse

Reference and Native each contain exactly these schema-1 objects and no parse
diagnostics:

- object 1: `catalog`, semantic hash `synthetic-catalog-v1`;
- object 2: `pages`, semantic hash `synthetic-pages-v1`;
- object 3: `page`, semantic hash `synthetic-page-v1`;
- object 4: `stream`, semantic hash `synthetic-stream-v1`.

The exact comparison therefore has zero metadata differences. Its diagnostics
section is 0 expected and 0 actual records; its objects section is 4 expected
and 4 actual records, with zero changed, missing, or unexpected records.

## Scene

The schema-1 reference contains `save/q` followed by `restore/Q`. Both commands
name source object 4 and use transform microunits
`[1000000, 0, 0, 1000000, 0, 0]`. Native contains one `save` command at the
same source and transform, with semantic hash
`synthetic-intentional-mismatch`.

The commands comparison is therefore not exact: 2 records are expected and 1
is actual, `first_difference=0`, `changed_records=1`, `missing_records=1`, and
`unexpected_records=0`. The changed and missing records are two distinct diff
counts.

## Text

Reference has one horizontal-LTR run with Unicode `PDF`, glyph IDs
`[80, 68, 70]`, and quad micropoints
`[0, 0, 3000000, 0, 3000000, 1000000, 0, 1000000]`. Native uses the same
writing mode and quad but Unicode `PBF` and glyph IDs `[80, 66, 70]`.

The runs comparison is therefore not exact: 1 record is expected and 1 is
actual, `first_difference=0`, `changed_records=1`, and both missing and
unexpected counts are zero.

## Pixel

The schema-1 artifacts are 4 by 4 straight-alpha RGBA8. Baseline contains 16
white pixels `[255, 255, 255, 255]`. Native changes only the first pixel to
`[0, 64, 255, 255]`. The exact comparison therefore reports 1 different pixel,
2 different channels, maximum per-channel delta `[255, 191, 0, 0]`, and total
absolute delta 446. The visible diff image has first pixel
`[255, 191, 0, 255]`; all other pixels are transparent.

The synthetic counterpart is not an external-engine observation and must not
be promoted to an O3 renderer golden. The spec-conformance role independently
reviewed this derivation and its complete canonical artifact assertions on
2026-07-15.
