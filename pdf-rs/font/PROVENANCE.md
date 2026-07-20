# Native font-core provenance

`pdf-rs-font` is the project-owned pure Rust authority for the registered
`m3.simple-truetype-winansi-ascii.v1`, `m4.simple-truetype-winansi.v1`, and
`m4.simple-type1c-standard.v1` parser profiles. It consumes one caller-owned immutable font-program
byte slice and publishes an immutable project-owned font only after input, table or INDEX, glyph,
contour, point, component or subroutine recursion, path, retained memory, allocation, fuel, and
cancellation checks succeed.

The profile accepts sfnt scaler type `0x00010000` with bounded direct `head`, `hhea`, `maxp`,
`loca`, `glyf`, `hmtx`, and `cmap` tables. Character selection is Windows Unicode BMP format 4
restricted at the public boundary to printable WinAnsi ASCII (`0x20..=0x7e`). Outlines support
simple quadratic glyphs and translation-only compound glyphs. Implied on-curve points remain exact
in half-font-unit project coordinates. TrueType instruction bytes are bounded and skipped; they are
never executed.

The Type1C profile accepts standalone CFF1 with one non-CID Top DICT, ISOAdobe or bounded custom
charsets, the default StandardEncoding model, structurally bounded custom Encoding data, bounded
local and global subroutines, hint masks, lines, and Type 2 cubic curves. Custom Encoding entries
are validated for framing, glyph counts, numeric ranges, and supplement SIDs but do not override
the owning PDF simple Font's code-to-name mapping; repeated non-authoritative CFF codes therefore
remain usable. CFF coordinates are retained to the nearest half font unit in the shared exact
outline model. CID-keyed fonts, CFF2, ExpertEncoding, explicit FontMatrix, escaped Type 2 operators,
and legacy `seac` composition remain typed unsupported capabilities.

This crate does not resolve PDF dictionaries, decode font streams, apply PDF text state, shape text,
rasterize glyphs, use a system-font fallback, call platform antialiasing or font services, or depend
on an external PDF/font engine. Non-TrueType sfnt flavors, other character maps, point-attached
components, transformed components, and the excluded CFF capabilities above are structured
unsupported outcomes. Malformed bytes, resource exhaustion, and cancellation are distinct terminal
non-publication outcomes with deterministic statistics.
