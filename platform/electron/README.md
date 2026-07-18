# PDF.rs Electron local preview

This is a source-only development shell. It is intentionally not packaged,
signed, notarized, or configured for distribution.

## Run

```sh
cd platform/electron
npm install
npm run dev
```

Choose a PDF from the native file dialog. Electron main owns the file choice
and the Rust bridge process. The context-isolated preload exposes only
`openPdf`, `renderPage`, `closePdf`, and status helpers. The sandboxed renderer
receives validated RGBA8 pixels and presents them with Canvas.

Reference CPU remains the default renderer. The exact default-off M4 CANARY can
be selected for a local run with:

```sh
PDF_RS_ELECTRON_RENDERER_COHORT=m4-r0-basic-page-local-v1 npm run dev
```

Removing that environment entry starts the next bridge on Reference CPU. The
application status bar always names the renderer that produced the current
surface.

## Verify

```sh
npm test
npm run smoke
```

`npm test` exercises both pages of the deterministic readable PDF through the
persistent bridge and verifies structured invalid/unsupported outcomes.
`npm run smoke` starts Electron with that PDF under the Fast CPU CANARY and
drives the source application through its normal renderer controls. It captures
the readable first page, page two, 125% replacement, resized window, and closed state under
`target/electron-preview-smoke*.png`, then verifies bridge exit and clean
application shutdown.

## Current compatibility

The first development slice uses the PDF.rs strict traditional-xref opener,
the registered graphics-v2 content profile, embedded simple TrueType text, and
the PDF.rs Reference raster backend. Its acceptance fixture is a readable,
two-page Letter document generated from project-authored paths and font
outlines. PDFs outside that bounded R0 subset return a structured unsupported
or document failure; Electron never falls back to Chromium's PDF viewer or an
external PDF engine.

The UI-neutral `pdf-rs-viewer` crate is the stable ownership boundary for a
future Rust-native UI. The Electron bridge is replaceable and contains no PDF
parser or raster implementation.
