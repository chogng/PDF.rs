//! Separate-process authenticated desktop Native worker entrypoint.

use pdf_rs_desktop::{
    DESKTOP_CHILD_PANIC_EXIT_CODE, DesktopIpcErrorCode, DesktopIpcLimitConfig, DesktopIpcLimits,
    run_child_stdio,
};

#[cfg(not(feature = "transport-fixture"))]
const DESKTOP_WORKER_FEATURE_CLOSURE_MARKER: &str =
    "PDF_RS_DESKTOP_FEATURE_CLOSURE:NO_DEFAULT_FEATURES:v1";
#[cfg(feature = "transport-fixture")]
const DESKTOP_WORKER_FEATURE_CLOSURE_MARKER: &str =
    "PDF_RS_DESKTOP_FEATURE_CLOSURE:TRANSPORT_FIXTURE:v1";

fn main() {
    #[cfg(target_os = "macos")]
    if pdf_rs_macos_spawn::restore_desktop_worker_signal_state().is_err() {
        std::process::exit(70);
    }
    std::hint::black_box(DESKTOP_WORKER_FEATURE_CLOSURE_MARKER);
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
