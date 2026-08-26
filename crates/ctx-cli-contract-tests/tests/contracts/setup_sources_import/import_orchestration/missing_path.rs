use super::*;

#[test]
fn import_missing_native_provider_path_reports_import_path_does_not_exist() {
    let temp = tempdir();
    let missing = temp.path().join("no-such-codex-session.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        missing.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("import path does not exist: "),
        "expected clean missing-path error, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("approve explicit source path"),
        "raw OS error chain must not leak, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("No such file or directory"),
        "raw OS error chain must not leak, got:\n{stderr}"
    );
}

#[test]
fn import_missing_custom_jsonl_path_reports_import_path_does_not_exist() {
    let temp = tempdir();
    let missing = temp.path().join("no-such-custom.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        missing.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        stderr.contains("import path does not exist: "),
        "expected clean missing-path error, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("approve explicit source path"),
        "raw OS error chain must not leak, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("No such file or directory"),
        "raw OS error chain must not leak, got:\n{stderr}"
    );
}
