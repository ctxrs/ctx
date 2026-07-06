#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn provider_help_and_errors_do_not_dump_full_provider_list() {
    let temp = tempdir();
    let help = ctx(&temp)
        .args(["import", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help).unwrap();
    assert!(help.contains("for example codex, claude, cursor, pi"));
    assert!(!help.contains("factory-ai-droid"));

    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "nope"]));
    assert!(stderr.contains("invalid value 'nope'"));
    assert!(stderr.contains("examples: codex, claude, cursor, pi"));
    assert!(!stderr.contains("[possible values:"));
    assert!(!stderr.contains("factory-ai-droid"));
}

#[test]
pub(crate) fn explicit_history_source_manifest_reports_parse_errors() {
    let temp = tempdir();
    let bad_manifest = temp.path().join("bad-plugin.json");
    fs::write(&bad_manifest, "{not-json").unwrap();

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        bad_manifest.to_str().unwrap(),
        "--progress",
        "none",
    ]));

    assert!(
        stderr.contains("parse history source plugin manifest"),
        "{stderr}"
    );
}

#[test]
pub(crate) fn explicit_history_source_manifest_reports_nonexistent_path() {
    let temp = tempdir();
    let path = temp.path().join("no-such-manifest.json");

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--history-source-manifest",
        path.to_str().unwrap(),
        "--progress",
        "none",
    ]));

    assert!(stderr.contains("import path does not exist"), "{stderr}");
    assert!(stderr.contains(path.to_str().unwrap()), "{stderr}");
}

#[test]
pub(crate) fn import_all_without_sources_does_not_report_missing_explicit_path() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--all", "--json"]));

    assert!(stderr.contains("no importable provider history sources found"));
    assert!(!stderr.contains("import path does not exist"), "{stderr}");
}

#[test]
pub(crate) fn provider_help_stays_compact_for_large_supported_provider_set() {
    let temp = tempdir();
    let output = ctx(&temp)
        .args(["import", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();

    assert!(help.contains("--provider <PROVIDER>"));
    assert!(help.contains("for example codex, claude, cursor, pi, copilot-cli, or opencode"));
    assert!(
        !help.contains("--provider <PROVIDER>\n          [possible values:"),
        "{help}"
    );
}

#[test]
pub(crate) fn unknown_native_providers_are_rejected_by_public_cli() {
    let temp = tempdir();

    for provider in ["not-a-real-provider", "unsupported-provider-placeholder"] {
        let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", provider, "--json"]));
        assert!(stderr.contains("unknown provider"), "{provider}: {stderr}");
    }
}

#[test]
pub(crate) fn import_rejects_nonexistent_explicit_format_path() {
    let temp = tempdir();
    let path = temp.path().join("missing-file.jsonl");
    let path = path.to_str().unwrap();

    ctx(&temp)
        .args(["import", "--format", "ctx-history-jsonl-v1", "--path", path])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("import path does not exist")
                .and(predicate::str::contains(path)),
        );
}
