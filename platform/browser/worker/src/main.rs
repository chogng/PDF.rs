//! Minimal link target for the M5 browser Worker Wasm artifact.

#![forbid(unsafe_code)]

use pdf_rs_browser_worker::BrowserWorkerProtocol;

fn main() {
    // Link the generated protocol identity without exposing a pointer-based ABI.
    let _ = BrowserWorkerProtocol::generated();
}
