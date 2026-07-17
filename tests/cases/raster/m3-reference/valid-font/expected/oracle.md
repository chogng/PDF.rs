# O1 embedded-triangle glyph oracle

The embedded TrueType program maps ASCII `A` to one simple glyph with units per
em 1000 and vertices `(0, 0)`, `(100, 0)`, and `(0, 100)`. Font size 1000 and
the identity text matrix therefore place that triangle directly in page
coordinates. The 100 by 100 page maps to an 8 by 8 top-left output.

At each of the 64 subpixel centers, the filled triangle test reduces to
`sample_x < sample_y` in device-local coordinates. Pixels strictly below the
diagonal have 64 black samples, pixels strictly above it have zero, and each
diagonal pixel has 28 black samples. Averaging opaque black over opaque white
gives Q16 value 36864 on the diagonal, which converts to RGBA8 gray `8f`; full
coverage is `00` and no coverage is `ff`. Alpha is always `ff`.

The independent model enumerates this predicate for all 4096 sample positions;
no platform font, hinting, shaping, antialiaser, or renderer output contributes
to the expected bytes.
