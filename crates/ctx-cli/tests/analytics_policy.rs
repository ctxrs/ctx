mod support;

use support::*;

#[test]
fn analytics_sends_coarse_cli_metadata_by_default() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let event = read_analytics_events(&events_path).remove(0);
    assert!(uuid::Uuid::parse_str(event["data_root_id"].as_str().unwrap()).is_ok());
    assert!(uuid::Uuid::parse_str(event["client_profile_id"].as_str().unwrap()).is_ok());
    assert_operation_event(&event, "doctor", "success");
    assert_eq!(event["events"][0]["properties"]["output"], "human");
    assert_eq!(
        event["events"][0]["properties"]["finding_count_bucket"],
        "2-5"
    );
    assert_capability_snapshot_is_coarse(analytics_event_properties(&event));
    assert_analytics_properties_are_allowlisted(analytics_event_properties(&event));
    for forbidden in [
        "command",
        "credential",
        "history",
        "prompt",
        "query",
        "query_text",
        "path",
        "file_path",
        "raw_path",
        "repo",
        "repo_name",
        "branch",
        "source_body",
        "token",
        "error",
        "error_message",
        "session_id",
        "item_id",
    ] {
        assert!(
            event["events"][0]["properties"].get(forbidden).is_none(),
            "analytics leaked forbidden property {forbidden}: {event:#}"
        );
    }
}

#[test]
fn analytics_config_opt_out_suppresses_delivery() {
    let temp = tempdir();
    let state = temp.path().join("state");
    let data_root = data_root(&temp);
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        "[analytics]\nenabled = false\n",
    )
    .unwrap();
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("doctor")
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "analytics endpoint should not be touched"
    );
    assert!(
        !temp.path().join("install.json").exists(),
        "disabled analytics should not create an install identity"
    );
    assert!(
        !expected_device_path(temp.path(), &state).exists(),
        "disabled analytics should not create a device identity"
    );
    assert!(
        !expected_capability_marker_path(temp.path(), &state).exists(),
        "disabled analytics should not create a capability marker"
    );
    assert_no_capability_state(temp.path(), &state);
}

#[test]
fn analytics_env_opt_out_suppresses_delivery() {
    let temp = tempdir();
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("doctor")
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "CTX_ANALYTICS_ENABLED=false should suppress analytics delivery"
    );
    assert!(
        !expected_device_path(temp.path(), &state).exists(),
        "hard opt-out should not create a device identity"
    );
    assert!(
        !expected_capability_marker_path(temp.path(), &state).exists(),
        "hard opt-out should not create a capability marker"
    );
    assert_no_capability_state(temp.path(), &state);
}

#[test]
fn deprecated_privacy_opt_outs_suppress_delivery_and_warn_once() {
    for name in [
        "CTX_ANALYTICS_OFF",
        "CTX_DISABLE_ANALYTICS",
        "CTX_INSTALL_DIAGNOSTICS_OFF",
    ] {
        let temp = tempdir();
        let events_path = temp.path().join("analytics.jsonl");
        let assert = ctx(&temp)
            .args(["doctor"])
            .env("CTX_ANALYTICS_ENABLED", "true")
            .env(name, "yes")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .assert()
            .success();
        let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
        assert_eq!(
            stderr
                .matches("deprecated environment variables detected")
                .count(),
            1
        );
        assert!(stderr.contains(name), "{stderr}");
        assert!(!events_path.exists(), "{name} must suppress analytics");
    }
}

#[test]
fn deprecated_warning_is_combined_and_suppressed_for_machine_output() {
    let temp = tempdir();
    let text = ctx(&temp)
        .args(["docs", "list"])
        .env("CTX_DAEMON_OFF", "0")
        .env("CTX_DISABLE_AUTO_UPGRADE", "false")
        .assert()
        .success();
    let stderr = String::from_utf8(text.get_output().stderr.clone()).unwrap();
    assert_eq!(
        stderr
            .matches("deprecated environment variables detected")
            .count(),
        1
    );
    assert!(stderr.contains("CTX_DAEMON_OFF -> CTX_DAEMON_ENABLED=false"));
    assert!(stderr.contains("CTX_DISABLE_AUTO_UPGRADE -> CTX_UPGRADE_AUTO=off"));
    assert!(!String::from_utf8(text.get_output().stdout.clone())
        .unwrap()
        .contains("deprecated environment variables"));

    ctx(&temp)
        .args(["status", "--format=json"])
        .env("CTX_DAEMON_OFF", "1")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());

    ctx(&temp)
        .args(["mcp", "serve"])
        .env("CTX_DAEMON_OFF", "1")
        .write_stdin("")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[test]
fn nonprivacy_deprecated_ids_are_reported_on_only_one_eligible_event() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let data_root = temp.path().join("data");
    let state = temp.path().join("state");
    fs::create_dir_all(temp.path().join(".codex").join("sessions")).unwrap();

    ctx(&temp)
        .args([
            "setup",
            "--catalog-only",
            "--no-daemon",
            "--progress",
            "none",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_DAEMON_OFF", "0")
        .env("CTX_UPGRADE_OFF", "false")
        .assert()
        .success();

    let payloads = read_analytics_events(&events_path);
    assert_eq!(payloads.len(), 1, "{payloads:#?}");
    assert_operation_event(&payloads[0], "setup", "success");
    let properties = analytics_event_properties(&payloads[0]);
    assert_eq!(properties["deprecated_daemon_control"], true);
    assert_eq!(properties["deprecated_upgrade_control"], true);
}
