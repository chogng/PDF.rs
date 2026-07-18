# PDF.rs Native viewer provenance

This crate is a repository-owned composition layer over PDF.rs core crates.
It does not contain, link, invoke, or recover through an external PDF engine.

The initial profile opens strict traditional-xref PDFs, materializes one page
at a time, and interprets the registered graphics-v2 subset. Reference CPU
remains the default while the UI-neutral qualification API can explicitly
select the product-tiled PDF.rs Fast CPU backend. Both paths preserve
structured unsupported terminals and record the renderer identity on every
surface. Electron enables Fast only through the versioned, default-off M4
CANARY cohort, so rollback starts the same bridge API on Reference CPU.
