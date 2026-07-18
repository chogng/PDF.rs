//! Isolated fixture-only desktop worker entrypoint.

use pdf_rs_desktop::{DesktopIpcLimitConfig, DesktopIpcLimits, run_child_stdio};

fn main() {
    if std::env::args().nth(1).as_deref() != Some("--pdf-rs-desktop-child") {
        std::process::exit(64);
    }
    let result = DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).and_then(run_child_stdio);
    if result.is_err() {
        std::process::exit(70);
    }
}
