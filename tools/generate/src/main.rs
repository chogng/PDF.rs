#![forbid(unsafe_code)]

use std::env;
use std::fs::{self, File};
use std::io::{Read, Take};
use std::path::Path;
use std::process::ExitCode;

use pdf_rs_generate::{GenerateLimits, compile_dsl, generate_readable_preview_pdf};

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(source_path) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    if source_path == "--readable-preview" {
        let Some(output_path) = arguments.next() else {
            usage();
            return ExitCode::from(2);
        };
        if arguments.next().is_some() {
            usage();
            return ExitCode::from(2);
        }
        let generated = match generate_readable_preview_pdf() {
            Ok(generated) => generated,
            Err(error) => {
                eprintln!("failed to generate readable preview PDF: {error}");
                return ExitCode::FAILURE;
            }
        };
        if fs::write(output_path, generated).is_err() {
            eprintln!("failed to write generated PDF");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    let Some(output_path) = arguments.next() else {
        usage();
        return ExitCode::from(2);
    };
    if arguments.next().is_some() {
        usage();
        return ExitCode::from(2);
    }

    let limits = GenerateLimits::default();
    let source = match read_bounded(Path::new(&source_path), limits.max_source_bytes()) {
        Ok(source) => source,
        Err(()) => {
            eprintln!("failed to read bounded DSL source");
            return ExitCode::FAILURE;
        }
    };
    let generated = match compile_dsl(&source, limits) {
        Ok(generated) => generated,
        Err(error) => {
            eprintln!("failed to compile PDF fixture: {error}");
            return ExitCode::FAILURE;
        }
    };
    if fs::write(output_path, generated.bytes()).is_err() {
        eprintln!("failed to write generated PDF");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if !metadata.file_type().is_file() || metadata.len() > u64::try_from(limit).map_err(|_| ())? {
        return Err(());
    }
    let capacity = limit.checked_add(1).ok_or(())?;
    let mut source = Vec::new();
    source.try_reserve_exact(capacity).map_err(|_| ())?;
    let file = File::open(path).map_err(|_| ())?;
    let take_limit = u64::try_from(capacity).map_err(|_| ())?;
    let mut bounded: Take<File> = file.take(take_limit);
    bounded.read_to_end(&mut source).map_err(|_| ())?;
    if source.len() > limit {
        return Err(());
    }
    Ok(source)
}

fn usage() {
    eprintln!(
        "usage: pdf-rs-generate <source.dsl> <output.pdf>\n       \
         pdf-rs-generate --readable-preview <output.pdf>"
    );
}
