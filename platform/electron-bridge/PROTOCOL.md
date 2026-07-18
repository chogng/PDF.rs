# Electron development bridge protocol

The bridge is a local development adapter. It does not parse or render PDF
content outside the Rust process.

Requests are ASCII lines:

- `OPEN <request-id> <UTF-8-path-as-lowercase-hex>`
- `RENDER <request-id> <document-id> <zero-based-page> <width>`
- `CANCEL <request-id> <target-render-request-id>`
- `CLOSE <request-id> <document-id>`
- `SHUTDOWN <request-id>`

Responses are ASCII lines except for a `SURFACE` payload:

- `OPENED <request-id> <document-id> <page-count>`
- `SURFACE <request-id> <document-id> <page> <renderer> <width> <height> <stride> <length>\n<RGBA bytes>\n`
- `CANCELLED <request-id> <target-render-request-id>`
- `CLOSED <request-id> <document-id>`
- `BYE <request-id>`
- `ERROR <request-id> <stable-code>`

Paths, source bytes, page indices, output dimensions, line lengths, and Surface
lengths are bounded before allocation or access. The raw Surface is top-down
straight-alpha sRGB RGBA8. `renderer` is a stable PDF.rs Native renderer
identifier such as `reference-cpu-v1` or `fast-cpu-v1`.

Rendering runs on a bounded worker queue so the stdio control loop remains
available while Rust is interpreting or rasterizing a page. `CANCEL` atomically
marks the target request stale; the target terminates with `ERROR <target>
cancelled`, and no Surface is published after cancellation wins the terminal
race.

The bridge starts in Reference CPU mode. The versioned M4 CANARY cohort
`m4-r0-basic-page-local-v1` selects Fast CPU only when Electron main passes it
through the private `PDF_RS_FAST_CPU_CANARY_V1` child environment entry.
Removing the cohort returns the next bridge process to Reference CPU without
changing request syntax or unsupported outcomes.
