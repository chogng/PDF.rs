//! Executable entry point for the local Electron-to-PDF.rs bridge.

fn main() {
    std::process::exit(i32::from(pdf_rs_electron_bridge::run_from_environment()));
}
