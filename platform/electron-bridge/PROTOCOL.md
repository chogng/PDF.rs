# Electron development bridge protocol

The bridge is a local development adapter. It does not parse or render PDF
content outside the Rust process.

Requests are ASCII lines:

- `OPEN <request-id> <UTF-8-path-as-lowercase-hex>`
- `RENDER <request-id> <document-id> <zero-based-page> <width>`
- `CLOSE <request-id> <document-id>`
- `SHUTDOWN <request-id>`

Responses are ASCII lines except for a `SURFACE` payload:

- `OPENED <request-id> <document-id> <page-count>`
- `SURFACE <request-id> <document-id> <page> <renderer> <width> <height> <stride> <length>\n<RGBA bytes>\n`
- `CLOSED <request-id> <document-id>`
- `BYE <request-id>`
- `ERROR <request-id> <stable-code>`

Paths, source bytes, page indices, output dimensions, line lengths, and Surface
lengths are bounded before allocation or access. The raw Surface is top-down
straight-alpha sRGB RGBA8. `renderer` is a stable PDF.rs Native renderer
identifier such as `reference-cpu-v1`.
