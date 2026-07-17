use std::process::Command;

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pdf-rs-protocol-codegen"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn usage_errors_exit_with_code_two_without_writing() {
    for arguments in [
        Vec::<&str>::new(),
        vec!["."],
        vec!["generate"],
        vec!["--check"],
        vec!["unknown", "."],
        vec!["generate", ".", "extra"],
    ] {
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("usage: pdf-rs-protocol-codegen"));
    }
}
