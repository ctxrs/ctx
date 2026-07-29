mod support;

use std::fs;

use predicates::prelude::*;
use rusqlite::Connection;
use support::*;

fn enabled(command: &mut assert_cmd::Command) -> &mut assert_cmd::Command {
    command.env_remove("CTX_LOCAL_USAGE_ENABLED")
}

#[test]
fn default_on_usage_is_independent_of_analytics_and_reports_stable_json() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"]))
        .env("CTX_ANALYTICS_ENABLED", "false")
        .assert()
        .success();

    assert!(temp.path().join("usage.sqlite").exists());
    assert!(!temp.path().join("install.json").exists());

    let report = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "detail",
        "--format=json",
    ])));
    let usage = &report["local_usage"];
    assert_eq!(usage["schema_version"], 1);
    assert_eq!(usage["enabled"], true);
    assert_eq!(usage["state"], "ready");
    assert_eq!(usage["definition_version"], 1);
    assert_eq!(usage["retention_days"], 400);
    assert_eq!(usage["summary"]["calls"], 1);
    assert_eq!(usage["summary"]["successful_calls"], 1);
    assert_eq!(usage["summary"]["failed_calls"], 0);
    assert_eq!(usage["summary"]["result_bearing_calls"], 0);
    assert_eq!(usage["summary"]["empty_calls"], 0);
    assert_eq!(usage["summary"]["not_applicable_calls"], 1);
    assert_eq!(
        usage["summary"]["pro_blame"]["produced_attribution_requests"],
        0
    );
    assert!(usage["summary"]["pro_blame"]
        .get("cited_provenance_requests")
        .is_none());
    assert_eq!(
        usage["summary"]["ctx_versions"],
        serde_json::json!([env!("CARGO_PKG_VERSION")])
    );
    assert_eq!(usage["details"]["by_operation"][0]["surface"], "cli");
    assert_eq!(usage["details"]["by_operation"][0]["operation"], "doctor");
    assert_eq!(
        usage["details"]["by_operation"][0]["not_applicable_calls"],
        1
    );
    assert!(usage.get("store_path").is_none());
    assert!(usage.get("tokens_saved").is_none());
    assert!(usage.get("dollars_saved").is_none());
}

#[test]
fn local_usage_report_is_content_free_and_absent_from_status_analytics() {
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&data_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        data_root.join("config.toml"),
        format!(
            "# {content_marker} {raw_id_marker}\n\
             [local_usage]\nenabled = true\n"
        ),
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

    let output = enabled(
        ctx(&temp)
            .args(["status", "--usage", "detail", "--format=json"])
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env("CTX_ANALYTICS_ENABLED", "true")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path)),
    )
    .assert()
    .success()
    .get_output()
    .stdout
    .clone();
    let status: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let local_usage = &status["local_usage"];
    assert_eq!(local_usage["state"], "ready");

    let install: serde_json::Value =
        serde_json::from_slice(&fs::read(data_root.join("install.json")).unwrap()).unwrap();
    let device: serde_json::Value =
        serde_json::from_slice(&fs::read(expected_device_path(&home, &state)).unwrap()).unwrap();
    let forbidden_values = [
        data_root.to_string_lossy().into_owned(),
        path_marker.to_owned(),
        content_marker.to_owned(),
        raw_id_marker.to_owned(),
        install["install_id"].as_str().unwrap().to_owned(),
        device["device_id"].as_str().unwrap().to_owned(),
    ];
    let encoded_usage = serde_json::to_string(local_usage).unwrap();
    for forbidden in &forbidden_values {
        assert!(
            !encoded_usage.contains(forbidden),
            "local usage report leaked {forbidden:?}: {encoded_usage}"
        );
    }
    for forbidden_key in [
        "data_root",
        "database_path",
        "store_path",
        "query",
        "session_id",
        "event_id",
        "citation_id",
        "client_profile_id",
        "data_root_id",
    ] {
        assert!(
            !encoded_usage.contains(&format!("\"{forbidden_key}\"")),
            "local usage report exposed {forbidden_key}: {local_usage:#}"
        );
    }

    let payloads = read_analytics_events(&events_path);
    assert_eq!(payloads.len(), 1);
    assert_operation_event(&payloads[0], "status", "success");
    let properties = analytics_event_properties(&payloads[0]);
    assert_analytics_properties_are_allowlisted(properties);
    let encoded_properties = serde_json::to_string(properties).unwrap();
    for forbidden in &forbidden_values {
        assert!(
            !encoded_properties.contains(forbidden),
            "status analytics properties leaked local usage data {forbidden:?}: \
             {encoded_properties}"
        );
    }
    for usage_key in [
        "local_usage",
        "usage_calls",
        "active_days",
        "ctx_versions",
        "result_bearing_calls",
        "empty_calls",
        "not_applicable_calls",
        "result_count",
        "citation_count",
        "mcp_response_bytes",
        "pro_blame",
    ] {
        assert!(
            !properties.contains_key(usage_key),
            "status analytics attached local usage field {usage_key}: {properties:#?}"
        );
    }
}

#[test]
fn disable_enable_and_reset_are_explicit_and_do_not_record_the_control() {
    let temp = tempdir();

    let disabled = json_output(ctx(&temp).args(["status", "--usage", "disable", "--format=json"]));
    assert_eq!(disabled["local_usage_action"]["effective_enabled"], false);
    assert_eq!(disabled["local_usage_action"]["persisted_enabled"], false);
    assert_eq!(
        disabled["local_usage_action"]["environment_override"],
        "disabled"
    );
    assert!(disabled.get("local_usage").is_none());
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    assert!(!temp.path().join("usage.sqlite").exists());

    let enabled_report =
        json_output(ctx(&temp).args(["status", "--usage", "enable", "--format=json"]));
    assert_eq!(
        enabled_report["local_usage_action"]["effective_enabled"], false,
        "the inherited hard-disable must remain effective for this process"
    );
    assert_eq!(
        enabled_report["local_usage_action"]["environment_override"],
        "disabled"
    );
    enabled(
        ctx(&temp)
            .args(["status", "--usage", "enable", "--format=json"])
            .env_remove("CTX_LOCAL_USAGE_ENABLED"),
    )
    .assert()
    .success();
    assert!(!temp.path().join("usage.sqlite").exists());

    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let reset = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "reset",
        "--format=json",
    ])));
    assert_eq!(reset["local_usage_action"]["store_state"], "cleared");
    assert!(reset.get("local_usage").is_none());

    let conn = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    let calls: u64 = conn
        .query_row(
            "SELECT COALESCE(SUM(calls), 0) FROM daily_usage",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(calls, 0, "reset itself must not become a usage row");
}

#[test]
fn usage_reports_are_literal_read_only_and_never_create_or_increment_the_store() {
    let temp = tempdir();
    for mode in ["summary", "detail"] {
        let report = json_output(enabled(ctx(&temp).args([
            "status",
            "--usage",
            mode,
            "--format=json",
        ])));
        assert_eq!(report["read_only"], true);
        assert_eq!(report["local_usage"]["state"], "empty");
        assert!(!temp.path().join("usage.sqlite").exists());
    }

    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let usage_path = temp.path().join("usage.sqlite");
    let before_bytes = fs::read(&usage_path).unwrap();
    let before_modified = fs::metadata(&usage_path).unwrap().modified().unwrap();
    let wal_path = temp.path().join("usage.sqlite-wal");
    let shm_path = temp.path().join("usage.sqlite-shm");
    let before_auxiliaries = (wal_path.exists(), shm_path.exists());
    for _ in 0..3 {
        json_output(enabled(ctx(&temp).args([
            "status",
            "--usage",
            "detail",
            "--format=json",
        ])));
    }
    let report = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "summary",
        "--format=json",
    ])));
    assert_eq!(report["local_usage"]["summary"]["calls"], 1);
    assert_eq!(fs::read(&usage_path).unwrap(), before_bytes);
    assert_eq!(
        fs::metadata(&usage_path).unwrap().modified().unwrap(),
        before_modified
    );
    assert_eq!(
        (wal_path.exists(), shm_path.exists()),
        before_auxiliaries,
        "read-only usage reports must not create SQLite auxiliaries"
    );
}

#[test]
fn usage_actions_preserve_opaque_prior_epoch_and_fail_with_stable_json_for_owned_inputs() {
    let temp = tempdir();
    let prior_epoch_path = temp.path().join("work.sqlite");
    let prior_epoch_bytes = b"opaque v0.25 prior-epoch sentinel";
    fs::write(&prior_epoch_path, prior_epoch_bytes).unwrap();
    let enabled_action = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "enable",
        "--format=json",
    ])));
    assert_eq!(enabled_action["local_usage_action"]["ok"], true);
    assert_eq!(
        enabled_action["local_usage_action"]["effective_enabled"],
        true
    );

    fs::write(
        temp.path().join("config.toml"),
        "[local_usage]\nenabled = invalid\nSECRET_PATH=/tmp/private\n",
    )
    .unwrap();
    let output = enabled(ctx(&temp).args(["status", "--usage", "disable", "--format=json"]))
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_DEBUG", "1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        error["local_usage_action"]["error"]["code"],
        "usage_control_failed"
    );
    assert!(!String::from_utf8(output.stderr)
        .unwrap()
        .contains("SECRET_PATH"));
    assert!(!temp.path().join("install.json").exists());

    fs::write(temp.path().join("usage.sqlite"), b"SECRET_STORE_PATH").unwrap();
    let output = enabled(ctx(&temp).args(["status", "--usage", "reset", "--format=json"]))
        .assert()
        .failure()
        .get_output()
        .clone();
    let encoded = String::from_utf8(output.stderr).unwrap();
    let error: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        error["local_usage_action"]["error"]["code"],
        "usage_reset_failed"
    );
    assert!(!encoded.contains("SECRET_STORE_PATH"));
    assert_eq!(fs::read(&prior_epoch_path).unwrap(), prior_epoch_bytes);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", prior_epoch_path.display())).exists(),
            "local usage action created a prior-epoch SQLite auxiliary: {suffix}"
        );
    }
}

#[test]
fn reset_rejects_constraint_bypassed_rows_without_deleting_them() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let path = temp.path().join("usage.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    conn.execute("UPDATE daily_usage SET calls = -1", [])
        .unwrap();
    drop(conn);
    let before = fs::read(&path).unwrap();

    let output = enabled(ctx(&temp).args(["status", "--usage", "reset", "--format=json"]))
        .assert()
        .failure()
        .get_output()
        .clone();
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(
        error["local_usage_action"]["error"]["code"],
        "usage_reset_failed"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT calls FROM daily_usage", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        -1
    );
}

#[test]
fn malformed_status_config_has_one_content_free_parseable_json_error() {
    let temp = tempdir();
    let marker = "SECRET_CONFIG_PATH_71af";
    fs::write(
        temp.path().join("config.toml"),
        format!("malformed config /tmp/{marker}/bearer-secret\n"),
    )
    .unwrap();

    let output = enabled(ctx(&temp).args(["status", "--format=json"]))
        .assert()
        .failure()
        .get_output()
        .clone();
    let encoded = String::from_utf8(output.stderr).unwrap();
    let error: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(error["local_usage"]["state"], "error");
    assert_eq!(
        error["local_usage"]["error"]["code"],
        "local_usage_config_unavailable"
    );
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains("bearer-secret"));
    assert!(!encoded.contains(temp.path().to_string_lossy().as_ref()));
}

#[test]
fn malformed_config_daemon_status_resolves_local_usage_fail_closed() {
    for (name, config, environment) in [
        (
            "persisted_false",
            "unrelated malformed line\n[local_usage]\nenabled = false\n",
            None,
        ),
        ("unresolved", "unrelated malformed line\n", None),
        (
            "false_environment",
            "unrelated malformed line\n",
            Some("false"),
        ),
        (
            "invalid_environment",
            "unrelated malformed line\n",
            Some("invalid"),
        ),
    ] {
        let temp = tempdir();
        fs::write(temp.path().join("config.toml"), config).unwrap();
        let mut command = ctx(&temp);
        command.args(["daemon", "status", "--format=json"]);
        enabled(&mut command);
        if let Some(value) = environment {
            command.env("CTX_LOCAL_USAGE_ENABLED", value);
        }
        command.assert().success();
        assert!(
            !temp.path().join("usage.sqlite").exists(),
            "{name} unexpectedly recorded daemon status"
        );
    }
}

#[test]
fn malformed_global_config_does_not_block_mcp_or_enable_local_or_remote_state() {
    let temp = tempdir();
    fs::write(
        temp.path().join("config.toml"),
        "unrelated malformed line without local usage control\n",
    )
    .unwrap();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "malformed-config-test", "version": "0"}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
        ],
        &[
            ("CTX_LOCAL_USAGE_ENABLED", "true"),
            ("CTX_ANALYTICS_ENABLED", "true"),
            ("CTX_ANALYTICS_DEBUG", "1"),
        ],
    );
    assert_eq!(responses.len(), 2);
    assert!(responses[0].get("result").is_some());
    assert!(!temp.path().join("usage.sqlite").exists());
    assert!(!temp.path().join("install.json").exists());
}

#[test]
fn reset_reports_missing_without_creating_a_store() {
    let temp = tempdir();
    let reset = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "reset",
        "--format=json",
    ])));
    assert_eq!(reset["local_usage_action"]["store_state"], "missing");
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn human_usage_actions_report_effective_override_and_cleared_vs_missing() {
    let temp = tempdir();
    ctx(&temp)
        .args(["status", "--usage", "enable"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "local_usage_persisted_enabled: true",
        ))
        .stdout(predicates::str::contains(
            "local_usage_effective_enabled: false",
        ))
        .stdout(predicates::str::contains(
            "local_usage_environment_override: disabled",
        ));
    enabled(ctx(&temp).args(["status", "--usage", "reset"]))
        .assert()
        .success()
        .stdout(predicates::str::contains("local_usage_store: missing"));

    enabled(ctx(&temp).args(["doctor"])).assert().success();
    enabled(ctx(&temp).args(["status", "--usage", "reset"]))
        .assert()
        .success()
        .stdout(predicates::str::contains("local_usage_store: cleared"));
}

#[test]
fn environment_disable_creates_no_sidecar() {
    let temp = tempdir();
    ctx(&temp).args(["doctor"]).assert().success();
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn foreground_sql_uses_fresh_projection_and_is_recorded_once_as_not_applicable() {
    let temp = tempdir();
    let prior_epoch_path = temp.path().join("work.sqlite");
    let prior_epoch_bytes = b"not sqlite: opaque v0.25 prior-epoch sentinel";
    fs::write(&prior_epoch_path, prior_epoch_bytes).unwrap();

    let sql = json_output(enabled(ctx(&temp).args([
        "sql",
        "SELECT 1 AS one",
        "--format=json",
    ])));
    assert_eq!(sql["rows"], json!([[1]]));
    assert!(temp.path().join("relational.sqlite").is_file());
    assert_eq!(fs::read(&prior_epoch_path).unwrap(), prior_epoch_bytes);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", prior_epoch_path.display())).exists(),
            "source-backed SQL opened the opaque prior epoch and created {suffix}"
        );
    }

    let report = json_output(enabled(ctx(&temp).args([
        "status",
        "--usage",
        "detail",
        "--format=json",
    ])));
    let usage = &report["local_usage"];
    assert_eq!(usage["summary"]["calls"], 1);
    assert_eq!(usage["summary"]["successful_calls"], 1);
    assert_eq!(usage["summary"]["failed_calls"], 0);
    assert_eq!(usage["summary"]["result_bearing_calls"], 0);
    assert_eq!(usage["summary"]["empty_calls"], 0);
    assert_eq!(usage["summary"]["not_applicable_calls"], 1);
    assert_eq!(usage["details"]["by_operation"][0]["operation"], "sql");
    assert_eq!(usage["details"]["by_operation"][0]["successful_calls"], 1);
    assert_eq!(usage["details"]["by_operation"][0]["failed_calls"], 0);
}

#[test]
fn human_report_uses_utc_classification_and_conservative_attribution_wording() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let conn = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            '2026-07-25', 1, '0.26.0', 'mcp', 'blame', 'success',
            'result_bearing', 'under_10_ms', 'file', 'produced',
            1, 1, 1, 100
        );
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            '2026-07-25', 1, '0.26.0', 'mcp', 'blame', 'success',
            'result_bearing', 'under_10_ms', 'commit', 'possible',
            1, 1, 0, 100
        );
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            '2026-07-25', 1, '0.26.0', 'mcp', 'blame', 'success',
            'empty', 'under_10_ms', 'pull_request', 'none',
            1, 0, 0, 100
        );
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            '2026-07-25', 1, '0.26.0', 'mcp', 'blame', 'failure',
            'not_applicable', 'under_10_ms', 'not_applicable', 'error',
            1, 0, 0, 100
        );
        "#,
    )
    .unwrap();
    drop(conn);

    enabled(ctx(&temp).args(["status", "--usage", "summary"]))
        .assert()
        .success()
        .stdout(predicates::str::contains("usage_active_utc_days:"))
        .stdout(predicates::str::contains(
            "usage_mcp_pro_result_classification: 2 nonempty, 1 empty",
        ))
        .stdout(predicates::str::contains(
            "usage_mcp_pro_result_classification_not_applicable: 2 calls",
        ))
        .stdout(predicates::str::contains("usage_classified_result_sets").not())
        .stdout(predicates::str::contains("usage_no_result_set_classification").not())
        .stdout(predicates::str::contains(
            "Pro returned produced attribution in 1 of 4 blame requests.",
        ))
        .stdout(predicates::str::contains(
            "pro_blame_outcomes: produced-attribution 1, possible-only 1, none 1, error 1",
        ))
        .stdout(predicates::str::contains("produced-attribution 1"))
        .stdout(predicates::str::contains("cited provenance").not());
}

#[test]
fn detailed_usage_operations_are_complete_deterministic_and_at_most_eighty_columns() {
    let temp = tempdir();
    enabled(ctx(&temp).args(["doctor"])).assert().success();
    let long_version = "v".repeat(64);
    let calls = i64::MAX;
    let connection = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE daily_usage \
             SET ctx_version = ?1, operation = 'integrations', calls = ?2",
            rusqlite::params![long_version, calls],
        )
        .unwrap();
    drop(connection);

    let output = enabled(ctx(&temp).args(["status", "--usage", "detail"]))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    let operation_block = output
        .lines()
        .skip_while(|line| !line.starts_with("usage_operation:"))
        .take_while(|line| !line.starts_with("usage_duration:"))
        .collect::<Vec<_>>();
    assert!(!operation_block.is_empty(), "{output}");
    for line in &operation_block {
        assert!(
            line.len() <= 80,
            "usage detail line is {} columns: {line}",
            line.len()
        );
    }
    assert_eq!(operation_block[0], "usage_operation: cli/integrations");
    let flattened = operation_block.join(" ");
    for field in [
        format!("ctx_version={}", "v".repeat(64)),
        format!("calls={calls}"),
        format!("success={calls}"),
        "failure=0".to_owned(),
        "result=0".to_owned(),
        "empty=0".to_owned(),
        format!("not-applicable={calls}"),
    ] {
        assert!(
            flattened.contains(&field),
            "usage detail omitted {field}: {operation_block:#?}"
        );
    }
}

#[test]
fn ordinary_record_failure_is_silent_and_does_not_change_command_success() {
    let temp = tempdir();
    fs::create_dir(temp.path().join("usage.sqlite")).unwrap();

    let output = enabled(ctx(&temp).args(["doctor"]))
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("usage"), "{stderr}");
    assert!(temp.path().join("usage.sqlite").is_dir());
}

#[test]
fn mcp_counts_only_recognized_flushed_tool_responses() {
    let temp = tempdir();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "local-usage-test", "version": "0"}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "unknown", "arguments": {}}
            }),
            serde_json::json!([]),
            serde_json::json!({
                "jsonrpc": "1.0",
                "id": 5,
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": true,
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {"name": "status", "arguments": {}}
            }),
        ],
        &[("CTX_LOCAL_USAGE_ENABLED", "true")],
    );
    assert_eq!(responses.len(), 7);

    let report = json_output(
        ctx(&temp)
            .args(["status", "--usage", "detail", "--format=json"])
            .env("CTX_LOCAL_USAGE_ENABLED", "true"),
    );
    let usage = &report["local_usage"];
    assert_eq!(usage["summary"]["calls"], 1);
    assert_eq!(usage["summary"]["not_applicable_calls"], 1);
    assert!(usage["summary"]["mcp_response_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        usage["details"]["by_operation"].as_array().unwrap().len(),
        1
    );
    assert_eq!(usage["details"]["by_operation"][0]["surface"], "mcp");
    assert_eq!(usage["details"]["by_operation"][0]["operation"], "status");
    assert!(!serde_json::to_string(usage).unwrap().contains("tokens"));
    assert!(!serde_json::to_string(usage).unwrap().contains("savings"));
}

#[test]
fn mcp_pre_init_tool_error_does_not_create_usage_store() {
    let temp = tempdir();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "status", "arguments": {}}
        })],
        &[("CTX_LOCAL_USAGE_ENABLED", "true")],
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["error"]["code"], -32002);
    assert!(!temp.path().join("usage.sqlite").exists());
}

#[test]
fn recognized_mcp_argument_error_counts_with_unvalidated_blame_target_as_na() {
    let temp = tempdir();
    let responses = mcp_roundtrip_with_env(
        &temp,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "local-usage-test", "version": "0"}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "blame",
                    "arguments": {"target": {"kind": "commit", "oid": ""}}
                }
            }),
        ],
        &[("CTX_LOCAL_USAGE_ENABLED", "true")],
    );
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["isError"], true);
    let conn = Connection::open(temp.path().join("usage.sqlite")).unwrap();
    let row: (String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT target_type, pro_outcome, calls, result_count, citation_count \
             FROM daily_usage WHERE operation = 'blame'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        ("not_applicable".to_owned(), "error".to_owned(), 1, 0, 0)
    );
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

    let report = json_output(enabled(ctx(&temp).args(["status", "--format=json"])));
    let encoded = serde_json::to_string(&report["local_usage"]).unwrap();
    assert_eq!(report["local_usage"]["state"], "error");
    assert_eq!(
        report["local_usage"]["error"]["code"],
        "usage_store_unavailable"
    );
    assert!(report["local_usage"].get("summary").is_none());
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains("bearer-secret"));
}
