#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::process::ExitCode;

use pdf_rs_generate::generate_one_page_pdf;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(output_path) = arguments.next() else {
        eprintln!("usage: pdf-rs-generate <output.pdf>");
        return ExitCode::from(2);
    };

    if arguments.next().is_some() {
        eprintln!("usage: pdf-rs-generate <output.pdf>");
        return ExitCode::from(2);
    }

    let pdf = match generate_one_page_pdf() {
        Ok(pdf) => pdf,
        Err(error) => {
            eprintln!("failed to generate PDF: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = fs::write(output_path, pdf) {
        eprintln!("failed to write PDF: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
