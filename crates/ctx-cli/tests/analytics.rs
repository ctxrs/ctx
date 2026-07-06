mod support;

use support::*;

#[test]
fn analytics_sends_coarse_cli_metadata_when_enabled() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let event = read_analytics_events(&events_path).remove(0);
    assert_eq!(event["broker_runtime"], "cli");
    assert!(uuid::Uuid::parse_str(event["broker_install_id"].as_str().unwrap()).is_ok());
    assert!(uuid::Uuid::parse_str(event["broker_device_id"].as_str().unwrap()).is_ok());
    assert_eq!(event["events"][0]["event_name"], "cli_invocation");
    assert_eq!(event["events"][0]["origin_runtime"], "cli");
    assert_eq!(event["events"][0]["surface"], "cli");
    assert_eq!(
        event["events"][0]["origin_install_id"],
        event["broker_install_id"]
    );
    assert_eq!(
        event["events"][0]["origin_device_id"],
        event["broker_device_id"]
    );
    assert_eq!(event["events"][0]["properties"]["action"], "status");
    assert_eq!(
        event["events"][0]["properties"]["analytics_client"],
        "ctx-cli"
    );
    assert_eq!(event["events"][0]["properties"]["initialized"], false);
    assert_eq!(
        event["events"][0]["properties"]["indexed_items_bucket"],
        "0"
    );
    assert_eq!(
        event["events"][0]["properties"]["cataloged_sessions_bucket"],
        "0"
    );
    assert_eq!(
        event["events"][0]["properties"]["indexed_sessions_bucket"],
        "0"
    );
    assert_eq!(
        event["events"][0]["properties"]["indexed_events_bucket"],
        "0"
    );
    assert_eq!(event["events"][0]["properties"]["db_size_bucket"], "0");
    assert_analytics_properties_are_allowlisted(analytics_event_properties(&event));
    for forbidden in [
        "command",
        "query",
        "query_text",
        "path",
        "file_path",
        "repo",
        "repo_name",
        "branch",
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
fn analytics_device_id_persists_across_data_roots() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root_a = temp.path().join("data-a");
    let data_root_b = temp.path().join("data-b");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    for data_root in [&data_root_a, &data_root_b] {
        ctx(&temp)
            .arg("status")
            .env("CTX_DATA_ROOT", data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_OFF")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .assert()
            .success();
    }

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 2);
    let install_a = events[0]["broker_install_id"].as_str().unwrap();
    let install_b = events[1]["broker_install_id"].as_str().unwrap();
    let device_a = events[0]["broker_device_id"].as_str().unwrap();
    let device_b = events[1]["broker_device_id"].as_str().unwrap();
    assert_ne!(install_a, install_b);
    assert_eq!(device_a, device_b);
    assert!(uuid::Uuid::parse_str(install_a).is_ok());
    assert!(uuid::Uuid::parse_str(install_b).is_ok());
    assert!(uuid::Uuid::parse_str(device_a).is_ok());

    assert!(data_root_a.join("install.json").exists());
    assert!(data_root_b.join("install.json").exists());
    let device_path = expected_device_path(&home, &state);
    assert!(device_path.exists());
    assert!(!device_path.starts_with(&data_root_a));
    assert!(!device_path.starts_with(&data_root_b));
    let device_json: Value = serde_json::from_slice(&fs::read(&device_path).unwrap()).unwrap();
    assert_eq!(device_json["schema_version"], 1);
    assert_eq!(device_json["device_id"], device_a);
    let device_body = serde_json::to_string(&device_json).unwrap();
    assert!(!device_body.contains(home.to_str().unwrap()));
    assert!(!device_body.contains(data_root_a.to_str().unwrap()));
    assert!(!device_body.contains(data_root_b.to_str().unwrap()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(device_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn analytics_payloads_omit_sensitive_command_data() {
    let temp = tempdir();
    let home = temp.path().join("alice-secret-home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("ctx-data");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();
    initialize_empty_store_with_env(&temp, &data_root, &home, &state);
    let private_query =
        "prompt text /home/alice/private/acme-secret repo@example.com host.internal 192.0.2.44";

    ctx(&temp)
        .args([
            "search",
            private_query,
            "--workspace",
            "acme-secret-repo",
            "--refresh",
            "off",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["docs", "search", "private prompt text", "--limit", "1"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["upgrade", "status"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["show", "session", "not-a-uuid-secret"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 4);
    let actions = events
        .iter()
        .map(|event| {
            event["events"][0]["properties"]["action"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, ["search", "docs", "upgrade", "show"]);

    let search_properties = analytics_event_properties(&events[0]);
    assert_eq!(search_properties["query_length_bucket"], "21-100");
    assert_eq!(search_properties["query_term_count_bucket"], "6-20");
    assert_eq!(search_properties["search_refresh_mode"], "off");
    assert_eq!(search_properties["search_refresh_status"], "skipped");
    assert_eq!(search_properties["zero_result"], true);
    assert!(search_properties.get("query_duration_bucket").is_some());
    assert!(search_properties.get("render_duration_bucket").is_some());
    assert_eq!(events[3]["events"][0]["success"], false);
    assert_eq!(
        events[3]["events"][0]["properties"]["failure_kind"],
        "command_error"
    );

    for event in &events {
        assert_analytics_properties_are_allowlisted(analytics_event_properties(event));
        assert_no_json_string_contains(
            event,
            &[
                private_query,
                "private prompt text",
                "not-a-uuid-secret",
                "acme-secret-repo",
                "/home/alice/private",
                "repo@example.com",
                "host.internal",
                "192.0.2.44",
                home.to_str().unwrap(),
            ],
        );
        let properties = analytics_event_properties(event);
        for forbidden_key in [
            "install_id",
            "origin_install_id",
            "broker_install_id",
            "device_id",
            "origin_device_id",
            "broker_device_id",
            "hostname",
            "username",
            "repo_name",
            "file_path",
            "prompt",
            "transcript",
        ] {
            assert!(
                properties.get(forbidden_key).is_none(),
                "analytics leaked forbidden property {forbidden_key}: {event:#}"
            );
        }
    }
}

#[test]
fn hosted_install_marker_enriches_analytics_event_without_properties_leak() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let binary = copied_ctx_binary(&temp);
    let install_attempt_id = "attempt_01JZCTXHOSTED";
    let marker_secret = "marker-secret-must-not-leak";
    fs::write(
        hosted_install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "install_attempt_id": install_attempt_id,
            "installer_private_note": marker_secret,
        }))
        .unwrap(),
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let cli_event = analytics_cli_event(&events[0]);
    assert_eq!(cli_event["install_attempt_id"], install_attempt_id);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["install_manager"], "ctx-hosted-installer");
    assert!(
        properties.get("install_attempt_id").is_none(),
        "raw marker id must stay out of analytics properties: {properties:#?}"
    );
    assert_no_json_string_contains(
        &Value::Object(properties.clone()),
        &[install_attempt_id, marker_secret],
    );
}

#[test]
fn malformed_hosted_install_marker_is_ignored() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let binary = copied_ctx_binary(&temp);
    fs::write(
        hosted_install_marker_path(&binary),
        b"{not-json marker-secret-must-not-leak",
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let cli_event = analytics_cli_event(&events[0]);
    assert!(cli_event.get("install_attempt_id").is_none());
    let properties = analytics_event_properties(&events[0]);
    assert!(properties.get("install_manager").is_none());
    assert_no_json_string_contains(
        &Value::Object(properties.clone()),
        &["marker-secret-must-not-leak"],
    );
}

#[test]
fn setup_analytics_emits_start_and_completion_events() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(home.join(".codex").join("sessions")).unwrap();

    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 2);
    let actions = events
        .iter()
        .map(|event| {
            analytics_event_properties(event)["action"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, ["setup_started", "setup"]);
    for event in &events {
        assert_eq!(analytics_cli_event(event)["event_name"], "cli_invocation");
        assert_eq!(analytics_cli_event(event)["status"], "ok");
        assert_eq!(analytics_cli_event(event)["success"], true);
        assert_analytics_properties_are_allowlisted(analytics_event_properties(event));
    }
}

#[test]
fn setup_analytics_opt_out_suppresses_start_completion_and_identities() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(home.join(".codex").join("sessions")).unwrap();

    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "setup analytics opt-out should suppress start and completion events"
    );
    assert!(
        !data_root.join("install.json").exists(),
        "setup analytics opt-out should not create an install identity"
    );
    assert!(
        !expected_device_path(&home, &state).exists(),
        "setup analytics opt-out should not create a device identity"
    );
}

#[test]
fn setup_analytics_dry_run_suppresses_start_completion_and_identities() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(home.join(".codex").join("sessions")).unwrap();

    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_DRY_RUN", "1")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "setup analytics dry run should suppress start and completion events"
    );
    assert!(
        !data_root.join("install.json").exists(),
        "setup analytics dry run should not create an install identity"
    );
    assert!(
        !expected_device_path(&home, &state).exists(),
        "setup analytics dry run should not create a device identity"
    );
}

#[test]
fn analytics_config_opt_out_suppresses_delivery() {
    let temp = tempdir();
    let state = temp.path().join("state");
    fs::write(
        temp.path().join("config.toml"),
        "[analytics]\nenabled = false\n",
    )
    .unwrap();
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
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
}

#[test]
fn analytics_env_opt_out_wins_over_enable_flag() {
    let temp = tempdir();
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_OFF", "1")
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "CTX_ANALYTICS_OFF should be a hard process opt-out"
    );
    assert!(
        !expected_device_path(temp.path(), &state).exists(),
        "hard opt-out should not create a device identity"
    );
}

#[test]
fn analytics_refuses_device_identity_under_data_root() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let state = data_root.join("state");
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "device identity under data root should fail closed before delivery"
    );
    assert!(
        !state.join("ctx").join("device.json").exists(),
        "device identity must not be created under CTX_DATA_ROOT"
    );
}
