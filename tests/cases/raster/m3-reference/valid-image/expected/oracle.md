# O1 image oracle

The literal image is 2 by 1, DeviceRGB, eight bits per component, and contains
the six component bytes `ff 00 00 00 00 ff`. The content matrix maps the image
unit square exactly onto the 100 by 100 page, and the requested output is 2 by
1. Inverting that map places the two output-pixel centers at image x positions
0.25 and 0.75. Deterministic nearest-neighbor selection therefore chooses
source texels 0 and 1 respectively.

The only output row is opaque red then opaque blue:
`ff 00 00 ff  00 00 ff ff`. This finite source-byte and coordinate derivation
does not use raster output.
