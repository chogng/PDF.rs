# O1 dashed-stroke oracle

The page is mapped to an 8 by 8 top-left output. Every pixel uses 64 samples at
the centers of an 8 by 8 subgrid. In page coordinates a sample is
`x = 12.5 * (pixel_x + (sample_x + 0.5) / 8)` and
`y = 100 - 12.5 * (pixel_y + (sample_y + 0.5) / 8)`.

Both rectangles use line width 2, butt caps, miter joins, dash array `[4 2]`,
and phase zero. The outer path has perimeter 360. Its four 90-unit sides each
start an on interval, so its stroke is the union of axis-aligned, one-unit
half-width rectangles for every `[6k, 6k + 4]` arc interval. The inner path has
perimeter 320. The on interval crosses the bottom-right corner at arc 80 and
the closed seam at arc 320/0; those two 90-degree miter joins add the complete
two-unit squares centered on their vertices. No other corner has a connected
on interval.

The inner rectangle first fills with DeviceGray 0.5 and then strokes with the
unchanged stroking DeviceGray 0. For each pixel, let `o`, `f`, and `s` be the
outer-stroke, inner-fill, and inner-stroke sample counts. Starting from Q16
white, apply rounded coverage averages in source order:

`v1 = round((65536 * (64 - o)) / 64)`

`v2 = round((v1 * (64 - f) + 32769 * f) / 64)`

`v3 = round((v2 * (64 - s)) / 64)`

Finally map `v3` to eight-bit with `round(v3 * 255 / 65536)` and set alpha to
255. The independent model test enumerates these finite rectangles and all
4096 sample positions. No renderer output was used to select the result.
