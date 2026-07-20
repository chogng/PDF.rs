use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../shaders/solid_rect.metal");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let source = manifest.join("../../shaders/solid_rect.metal");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let air = output.join("solid_rect.air");
    let library = output.join("solid_rect.metallib");

    if let Err(error) = run(
        "metal",
        [
            "-c",
            source.to_str().expect("UTF-8 shader path"),
            "-o",
            air.to_str().expect("UTF-8 AIR path"),
        ],
    ) {
        println!("cargo:warning={error}; Metal shader compilation skipped");
        return;
    }
    if let Err(error) = run(
        "metallib",
        [
            air.to_str().expect("UTF-8 AIR path"),
            "-o",
            library.to_str().expect("UTF-8 Metal library path"),
        ],
    ) {
        println!("cargo:warning={error}; Metal shader library generation skipped");
        return;
    }
    println!(
        "cargo:rustc-env=PDF_RS_SKIA_METAL_LIBRARY={}",
        library.display()
    );
}

fn run<'a>(tool: &str, arguments: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let status = Command::new("xcrun")
        .args(["--sdk", "macosx", tool])
        .args(arguments)
        .status()
        .map_err(|error| format!("failed to launch {tool}: {error}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{tool} failed with {status}"))
}
