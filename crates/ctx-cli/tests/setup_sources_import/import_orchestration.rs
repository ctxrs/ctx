use super::{support::*, write_codex_setup_session};

#[test]
fn import_accepts_deprecated_partial_as_a_compatibility_noop() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let source_root = temp.path().join(".codex").join("sessions");

    ctx(&temp)
        .args([
            "import",
            "--partial",
            "--quiet",
            "--provider",
            "codex",
            "--path",
            source_root.to_str().unwrap(),
            "--no-daemon",
            "--progress",
            "none",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: --partial is deprecated and no longer changes import behavior; tolerant import is now unconditional",
        ));
}

#[test]
fn import_progress_json_goes_to_stderr_without_polluting_stdout() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let output = ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--json",
            "--progress",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""operation":"import""#), "{stderr}");
}

#[test]
fn import_custom_history_jsonl_format_is_searchable_and_idempotent() {
    let temp = tempdir();
    let fixture = custom_history_fixture("basic.jsonl");

    let first = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["totals"]["imported_sessions"], 2);
    assert_eq!(first["totals"]["imported_events"], 2);
    assert_eq!(first["totals"]["imported_edges"], 2);
    assert_eq!(first["sources"][0]["provider"], "custom");
    assert_eq!(first["sources"][0]["format"], "ctx-history-jsonl-v1");

    let search = json_output(ctx(&temp).args([
        "search",
        "parser test",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import was not searchable: {search:#}"
    );

    let second = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["imported_edges"], 0);
    assert_eq!(second["totals"]["skipped"], 6);
}

#[test]
fn import_custom_history_jsonl_format_imports_valid_rows_and_reports_rejections() {
    let temp = tempdir();
    let fixture = custom_history_fixture("malformed-mixed.jsonl");

    let import = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(import["totals"]["imported_sessions"], 1);
    assert_eq!(import["totals"]["imported_events"], 1);
    assert_eq!(import["totals"]["rejected_records"], 1);
    assert_eq!(import["sources"][0]["rejected_records"], 1);

    let search = json_output(ctx(&temp).args([
        "search",
        "Valid event before malformed record.",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !search["results"].as_array().unwrap().is_empty(),
        "custom import with rejections was not searchable: {search:#}"
    );
}

#[test]
fn all_invalid_custom_import_cleans_up_and_retries_after_source_is_fixed() {
    let temp = tempdir();
    let fixture = temp.path().join("custom-retry.jsonl");
    let records = |event_index: &str| {
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}
{"record_type":"source","source_id":"retry-source","provider_key":"retry-agent","source_format":"retry-jsonl","cursor":{"after":{"stream":"retry-agent:retry-source","cursor":"1","observed_at":"2026-07-13T12:00:00Z"}}}
{"record_type":"session","source_id":"retry-source","session_id":"retry-session","started_at":"2026-07-13T12:00:00Z"}
{"record_type":"event","source_id":"retry-source","session_id":"retry-session","event_index":EVENT_INDEX,"event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"retry oracle"}}
"#
        .replace("EVENT_INDEX", event_index)
    };
    fs::write(&fixture, records(r#""invalid""#)).unwrap();

    let failed = ctx(&temp)
        .args([
            "import",
            "--format",
            "ctx-history-jsonl-v1",
            "--path",
            fixture.to_str().unwrap(),
            "--json",
            "--progress",
            "none",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["outcome"], "failure", "{report:#}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
    assert_eq!(sqlite_count(&conn, "SELECT COUNT(*) FROM sync_cursors"), 0);
    drop(conn);

    fs::write(&fixture, records("0")).unwrap();
    let retry = json_output(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        fixture.to_str().unwrap(),
        "--json",
        "--progress",
        "none",
    ]));
    assert_eq!(retry["outcome"], "success", "{retry:#}");
    assert_eq!(retry["totals"]["imported_events"], 1, "{retry:#}");
}

#[test]
fn import_custom_history_format_is_not_a_native_provider_importer() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "custom"]));
    assert!(stderr.contains("invalid value 'custom'"), "{stderr}");

    let fixture = custom_history_fixture("basic.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--format",
        "ctx-history-jsonl-v1",
        "--path",
        &fixture,
        "--all",
    ]));
    assert!(stderr.contains("--format"), "{stderr}");
    assert!(stderr.contains("--all"), "{stderr}");
}

#[test]
fn import_all_runs_enabled_history_source_plugins_for_external_shapes() {
    let temp = tempdir();
    let plugin_root = temp.path().join("history-plugins");
    let providers = ["dorkos", "openclaw", "hermes", "nanoclaw"];
    for provider in providers {
        write_history_source_plugin_at(&plugin_root, provider, true, None);
    }
    write_history_source_plugin_at(&plugin_root, "disabled-dorkos", false, None);

    let imported = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin_root)
            .args(["import", "--all", "--json", "--progress", "none"]),
    );
    assert_eq!(imported["totals"]["imported_sources"], 4);
    assert_eq!(imported["totals"]["imported_sessions"], 4);
    assert_eq!(imported["totals"]["imported_events"], 4);
    let sources = imported["sources"].as_array().unwrap();
    for provider in providers {
        assert!(
            sources
                .iter()
                .any(|source| source["history_source"] == format!("{provider}/default")),
            "missing import source for {provider}: {sources:#?}"
        );
        let search = json_output(ctx(&temp).args([
            "search",
            &format!("{provider} plugin initial marker"),
            "--provider",
            "custom",
            "--refresh",
            "off",
            "--json",
        ]));
        assert!(
            !search["results"].as_array().unwrap().is_empty(),
            "{provider} plugin result was not searchable: {search:#}"
        );
    }
    assert!(!sources
        .iter()
        .any(|source| source["history_source"] == "disabled-dorkos/default"));
}

#[test]
fn import_all_discovers_and_imports_providers_together() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let pi_home = temp.path().join(".pi/agent/sessions/--workspace-example--");
    fs::create_dir_all(&pi_home).unwrap();
    fs::copy(
        provider_history_fixture("pi-session.jsonl"),
        pi_home.join("2026-06-24T12-00-00-000Z_pi-session-docs-1.jsonl"),
    )
    .unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() >= 3);
    let sources = stdout["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().any(|source| source["provider"] == "codex"));
    assert!(sources.iter().any(|source| source["provider"] == "pi"));

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""phase":"finalizing""#), "{stderr}");
}

#[test]
fn import_all_without_sources_does_not_report_missing_explicit_path() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--all", "--json"]));

    assert!(stderr.contains("no importable provider history sources found"));
    assert!(!stderr.contains("import path does not exist"), "{stderr}");
}

#[test]
fn import_all_discovers_sources_when_home_unset_and_userprofile_set() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );

    let imported = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .args(["import", "--all", "--json", "--progress", "none"]),
    );
    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source["provider"] == "codex"));
}

#[test]
fn import_all_skips_empty_gemini_source() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--json"]));
    let gemini = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "gemini")
        .unwrap();
    assert_eq!(gemini["status"], "empty");
    assert_eq!(gemini["native_import"], true);
    assert_eq!(gemini["importable"], false);

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--json", "--progress", "none"]));
    assert_eq!(imported["totals"]["imported_sources"], 1);
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert!(imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .all(|source| source["provider"] != "gemini"));
}

#[test]
fn import_all_reports_source_failure_without_losing_successes() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    let output = ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert_eq!(stdout["totals"]["imported_sources"], 1);
    assert_eq!(stdout["totals"]["failed_sources"], 1);
    assert!(stdout["totals"]["imported_sessions"].as_u64().unwrap() > 0);
    let sources = stdout["sources"].as_array().unwrap();
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "codex" && source["status"] == "success"));
    assert!(sources
        .iter()
        .any(|source| source["provider"] == "opencode" && source["status"] == "failure"));
    let opencode_failure = sources
        .iter()
        .find(|source| source["provider"] == "opencode")
        .unwrap();
    assert!(
        opencode_failure["error"]
            .as_str()
            .unwrap()
            .contains("not a database"),
        "{opencode_failure}"
    );
}

#[test]
fn failed_import_attempt_does_not_count_as_indexed_history() {
    let temp = tempdir();
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    ctx(&temp)
        .args(["import", "--all", "--json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("all import sources failed"));

    let status = json_output(ctx(&temp).args(["status", "--json"]));
    assert_eq!(status["indexed_items"], 0);
    assert_eq!(status["indexed_sources"], 0);
}
