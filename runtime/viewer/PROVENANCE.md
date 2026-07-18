# PDF.rs Native viewer provenance

This crate is a repository-owned composition layer over PDF.rs core crates.
It does not contain, link, invoke, or recover through an external PDF engine.

The initial profile opens strict traditional-xref PDFs, materializes one page
at a time, interprets the registered graphics-v2 subset, and renders through
the PDF.rs Reference raster backend. Unsupported content remains a structured
terminal result.
