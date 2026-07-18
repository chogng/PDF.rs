//! Isolated authenticated desktop Native worker entrypoint.

use pdf_rs_desktop::{
    DESKTOP_CHILD_PANIC_EXIT_CODE, DesktopIpcErrorCode, DesktopIpcLimitConfig, DesktopIpcLimits,
    run_child_stdio,
};

fn main() {
    if std::env::args().nth(1).as_deref() != Some("--pdf-rs-desktop-child") {
        std::process::exit(64);
    }
    let result = DesktopIpcLimits::new(DesktopIpcLimitConfig::default()).and_then(run_child_stdio);
    if let Err(failure) = result {
        let code = if failure.code() == DesktopIpcErrorCode::ChildPanic {
            DESKTOP_CHILD_PANIC_EXIT_CODE
        } else {
            70
        };
        std::process::exit(code);
    }
}
