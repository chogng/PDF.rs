use std::process::ExitCode;

use pdf_rs_protocol_codegen::{check_repository, generate_repository, parse_cli};

fn main() -> ExitCode {
    let (check, root) = match parse_cli(std::env::args().skip(1)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("protocol-codegen: {error}");
            eprintln!(
                "usage: pdf-rs-protocol-codegen generate <repository-root>\n       pdf-rs-protocol-codegen --check <repository-root>"
            );
            return ExitCode::from(2);
        }
    };
    let result = if check {
        check_repository(&root)
    } else {
        generate_repository(&root)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("protocol-codegen: {error}");
            ExitCode::FAILURE
        }
    }
}
