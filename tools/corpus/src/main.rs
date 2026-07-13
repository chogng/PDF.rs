#![forbid(unsafe_code)]

use std::env;
use std::path::Path;
use std::process::ExitCode;

use pdf_rs_corpus::{CorpusManifestLimits, validate_manifest_file};
use pdf_rs_digest::hex_digest;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    let Some(manifest_path) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    let Some(object_root) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    if command != "validate" || arguments.next().is_some() {
        usage();
        return ExitCode::from(2);
    }

    let verified = match validate_manifest_file(
        Path::new(&manifest_path),
        Path::new(&object_root),
        CorpusManifestLimits::default(),
    ) {
        Ok(verified) => verified,
        Err(error) => {
            eprintln!("corpus manifest validation failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let manifest = verified.manifest();
    println!("manifest_id={}", manifest.manifest().id());
    println!("manifest_version={}", manifest.manifest().version());
    println!(
        "manifest_sha256=sha256:{}",
        hex_digest(&manifest.source_sha256())
    );
    println!("verified_objects={}", verified.verified_objects());
    println!("verified_bytes={}", verified.verified_bytes());
    ExitCode::SUCCESS
}

fn usage() {
    eprintln!("usage: pdf-rs-corpus validate <manifest.toml> <object-root>");
}
