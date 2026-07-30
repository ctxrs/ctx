mod support;

use std::fs;

use rusqlite::Connection;
use serde_json::Value;
use support::*;

fn enabled(command: &mut assert_cmd::Command) -> &mut assert_cmd::Command {
    command.env_remove("CTX_LOCAL_USAGE_ENABLED")
}

fn definition(report: &Value, version: i64) -> &Value {
    report["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["definition_version"] == version)
        .unwrap()
}

#[test]
fn default_on_usage_is_independent_of_commercial_telemetry_and_reports_definition_two() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"]))
        .env("CTX_ANALYTICS_ENABLED", "false")
        .assert()
        .success();

    assert!(temp.path().join("usage.sqlite").exists());
    assert!(!temp.path().join("install.json").exists());

    let report = json_output(enabled(ctx(&temp).args([
        "stats",
        "--detail",
        "--format=json",
    ])));
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["local_only"], true);
    assert_eq!(report["read_only"], true);
    assert_eq!(report["enabled"], true);
    assert_eq!(report["state"], "ready");
    assert_eq!(report["retention_days"], 400);
    assert!(report.get("definition_version").is_none());
    assert!(report.get("summary").is_none());

    let current = definition(&report, 2);
    assert_eq!(current["summary"]["calls"], 1);
    assert_eq!(current["summary"]["successful_calls"], 1);
    assert_eq!(current["summary"]["failed_calls"], 0);
    assert_eq!(current["summary"]["not_applicable_calls"], 1);
    assert_eq!(
        current["summary"]["delivered_output_bytes"], 0,
        "unmeasured CLI output must not be inferred"
    );
    assert_eq!(current["summary"]["complete_context_eligible_calls"], 0);
    assert_eq!(current["by_operation"][0]["operation"], "doctor");
    assert!(report["estimates"].is_null());
}

#[test]
fn docs_records_the_exact_rendered_stdout_bytes() {
    let temp = tempdir();
    let stdout = enabled(ctx(&temp).args(["docs", "list", "--format=json"]))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    let delivered_output_bytes: i64 = connection
        .query_row(
            "SELECT delivered_output_bytes FROM daily_usage \
             WHERE surface = 'cli' AND operation = 'docs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(delivered_output_bytes, i64::try_from(stdout.len()).unwrap());
    assert!(delivered_output_bytes > 0);
}

#[test]
fn report_is_content_free_and_stats_emit_no_commercial_analytics() {
    let temp = tempdir();
    let path_marker = "PRIVATE_USAGE_PATH_7d31";
    let content_marker = "PRIVATE_USAGE_CONTENT_62af";
    let raw_id_marker = "session-raw-id-9184";
    let data_root = temp.path().join(path_marker);
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("status-analytics.jsonl");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(
        data_root.join("config.toml"),
        format!("# {content_marker} {raw_id_marker}\n[local_usage]\nenabled = true\n"),
    )
    .unwrap();

    enabled(ctx(&temp).args(["doctor"]))
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .assert()
        .success();

    let report = json_output(enabled(
        ctx(&temp)
            .args(["stats", "--detail", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env("CTX_ANALYTICS_ENABLED", "true")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path)),
    ));
    assert_eq!(report["state"], "ready");

    let encoded = serde_json::to_string(&report).unwrap();
    for forbidden in [
        data_root.to_string_lossy().as_ref(),
        path_marker,
        content_marker,
        raw_id_marker,
    ] {
        assert!(!encoded.contains(forbidden), "report leaked {forbidden:?}");
    }
    for forbidden_key in [
        "data_root",
        "database_path",
        "store_path",
        "query",
        "path",
        "content",
        "session_id",
        "event_id",
        "citation_id",
        "client_profile_id",
        "data_root_id",
    ] {
        assert!(
            !encoded.contains(&format!("\"{forbidden_key}\"")),
            "report exposed {forbidden_key}: {encoded}"
        );
    }

    assert!(!events_path.exists(), "ctx stats emitted remote analytics");
    assert!(!data_root.join("install.json").exists());
    assert!(!expected_device_path(&home, &state).exists());
}

#[test]
fn stats_are_literal_read_only_and_do_not_count_themselves() {
    let temp = tempdir();
    let empty = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(empty["read_only"], true);
    assert_eq!(empty["state"], "empty");
    assert_eq!(empty["definitions"], serde_json::json!([]));
    assert!(!temp.path().join("usage.sqlite").exists());

    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let usage_path = temp.path().join("usage.sqlite");
    let before_bytes = fs::read(&usage_path).unwrap();
    let before_modified = fs::metadata(&usage_path).unwrap().modified().unwrap();
    for _ in 0..3 {
        json_output(enabled(ctx(&temp).args([
            "stats",
            "--detail",
            "--format=json",
        ])));
    }
    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    assert_eq!(definition(&report, 2)["summary"]["calls"], 1);
    assert_eq!(fs::read(&usage_path).unwrap(), before_bytes);
    assert_eq!(
        fs::metadata(&usage_path).unwrap().modified().unwrap(),
        before_modified
    );
    assert!(!temp.path().join("usage.sqlite-wal").exists());
    assert!(!temp.path().join("usage.sqlite-shm").exists());
}

#[test]
fn cli_non_search_results_are_not_reclassified_as_value_or_context() {
    let temp = tempdir();
    let sql = json_output(enabled(ctx(&temp).args([
        "sql",
        "SELECT 1 AS one",
        "--format=json",
    ])));
    assert_eq!(sql["rows"], serde_json::json!([[1]]));

    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    let row: (i64, String, String, i64, i64, i64) = connection
        .query_row(
            "SELECT definition_version, value_class, context_coverage, result_count, \
                    delivered_context_bytes, matched_normalized_session_bytes \
             FROM daily_usage WHERE surface = 'cli' AND operation = 'sql'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            2,
            "not_applicable".to_owned(),
            "not_applicable".to_owned(),
            0,
            0,
            0,
        )
    );
}

#[test]
fn parsed_cli_failure_records_once_and_recording_failure_is_best_effort() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["pro", "--referral", "agent-smith", "setup", "--format=json"]))
        .assert()
        .failure();
    let report = json_output(enabled(ctx(&temp).args([
        "stats",
        "--detail",
        "--format=json",
    ])));
    let current = definition(&report, 2);
    assert_eq!(current["summary"]["calls"], 1);
    assert_eq!(current["summary"]["failed_calls"], 1);
    assert_eq!(current["by_operation"][0]["operation"], "pro_setup");

    let unavailable = tempdir();
    fs::create_dir(unavailable.path().join("usage.sqlite")).unwrap();
    enabled(ctx(&unavailable).args(["doctor"]))
        .assert()
        .success()
        .stderr(predicates::str::contains("usage").not());
}

#[test]
fn protocol_control_and_cli_control_paths_do_not_record() {
    let temp = tempdir();
    enabled(ctx(&temp).arg("--help")).assert().success();
    enabled(ctx(&temp).arg("--version")).assert().success();
    enabled(ctx(&temp).args(["stats", "--format=json"]))
        .assert()
        .success();
    enabled(ctx(&temp).args(["status", "--usage", "reset", "--format=json"]))
        .assert()
        .success();
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn malformed_store_is_an_explicit_content_free_report_error() {
    let temp = tempdir();
    let marker = "SECRET_PATH_TOKEN_7f98";
    fs::write(
        temp.path().join("usage.sqlite"),
        format!("not sqlite: /tmp/{marker}/bearer-secret"),
    )
    .unwrap();

    let report = json_output(enabled(ctx(&temp).args(["stats", "--format=json"])));
    let encoded = serde_json::to_string(&report).unwrap();
    assert_eq!(report["state"], "error");
    assert_eq!(report["error"]["code"], "usage_store_unavailable");
    assert!(report.get("definitions").is_none());
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains("bearer-secret"));
}
