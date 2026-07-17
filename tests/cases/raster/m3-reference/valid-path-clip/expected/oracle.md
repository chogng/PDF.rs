# O0 path-and-clip oracle

The literal page is 100 by 100 units and the output is 2 by 1 pixels. Each output
pixel is evaluated on the fixed 8 by 8 grid at subpixel centers.

The first rectangle clips all samples to `0 <= x <= 50`; filling the whole page
with DeviceGray 0 therefore makes the left pixel opaque black and leaves the
right pixel white. Restoring graphics state removes the clip. The second
rectangle covers `50 <= x <= 100` and fills every sample of the right pixel with
DeviceRGB `(1, 0, 0)`.

Thus the top-left RGBA8 row is exactly black then red:
`00 00 00 ff  ff 00 00 ff`. This derivation uses only the literal PDF geometry
and color operands; no raster output was used to choose the bytes.
