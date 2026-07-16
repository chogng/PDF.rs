# Native font-core provenance

`pdf-rs-font` is the project-owned pure Rust authority for the registered
`m3.simple-truetype-winansi-ascii.v1` parser profile. It consumes one caller-owned immutable sfnt
byte slice, measures the complete accepted outline product, and publishes an immutable
`TrueTypeFont` only after input, table, glyph, contour, point, component, recursion, path, retained
memory, allocation, fuel, and cancellation checks succeed.

The profile accepts sfnt scaler type `0x00010000` with bounded direct `head`, `hhea`, `maxp`,
`loca`, `glyf`, `hmtx`, and `cmap` tables. Character selection is Windows Unicode BMP format 4
restricted at the public boundary to printable WinAnsi ASCII (`0x20..=0x7e`). Outlines support
simple quadratic glyphs and translation-only compound glyphs. Implied on-curve points remain exact
in half-font-unit project coordinates. TrueType instruction bytes are bounded and skipped; they are
never executed.

This crate does not resolve PDF dictionaries, decode font streams, apply PDF text state, shape text,
rasterize glyphs, use a system-font fallback, call platform antialiasing or font services, or depend
on an external PDF/font engine. Non-TrueType sfnt flavors, other character maps, point-attached
components, and transformed components are structured unsupported outcomes. Malformed bytes,
resource exhaustion, and cancellation are distinct terminal non-publication outcomes with
deterministic statistics.
