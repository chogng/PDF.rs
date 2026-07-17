use std::process::Command;

#[test]
fn generated_typescript_runtime_vectors_pass() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/typescript_runtime.mts");
    let output = Command::new("node")
        .args([
            "--no-warnings",
            "--experimental-transform-types",
            script.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
