use std::process::Command;

#[test]
fn lint_rejects_semantically_invalid_definition() {
    let source = include_str!("../examples/pipeline.yaml").replacen("max_attempts: 2", "max_attempts: 101", 1);
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("invalid.yaml");
    std::fs::write(&path, source).expect("write invalid definition");

    let output = Command::new(env!("CARGO_BIN_EXE_definition-lint"))
        .arg(path)
        .output()
        .expect("run definition-lint");

    assert!(!output.status.success());
}
