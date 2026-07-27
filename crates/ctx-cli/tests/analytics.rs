mod support;

use support::*;

#[test]
fn eligible_json_command_analytics_reports_default_off_auto_upgrade_status() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["doctor", "--json"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let event = read_analytics_events(&events_path).remove(0);
    assert_operation_event(&event, "doctor", "success");
    let properties = analytics_event_properties(&event);
    assert_eq!(properties["output"], "json");
    assert_eq!(properties["auto_upgrade_probe"], true);
    assert_eq!(properties["auto_upgrade_due"], false);
    assert_eq!(properties["auto_upgrade_spawned"], false);
    assert_eq!(properties["auto_upgrade_spawn_status"], "auto_disabled");
    assert_eq!(properties["auto_upgrade_channel"], "stable");
    assert_analytics_properties_are_allowlisted(properties);
}

#[test]
fn capability_snapshot_is_sent_once_after_successful_delivery() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    for _ in 0..2 {
        ctx(&temp)
            .arg("doctor")
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_UPGRADE_AUTO", "off")
            .assert()
            .success();
    }

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 2, "one request should be made per invocation");
    assert!(events
        .iter()
        .all(|event| event["events"].as_array().unwrap().len() == 1));
    let first_properties = analytics_event_properties(&events[0]);
    assert_capability_snapshot_is_coarse(first_properties);
    assert_analytics_properties_are_allowlisted(first_properties);
    let second_properties = analytics_event_properties(&events[1]);
    for key in CAPABILITY_PROPERTY_KEYS {
        assert!(
            !second_properties.contains_key(key),
            "capability property {key} was sent more than once: {events:#?}"
        );
    }
    assert_analytics_properties_are_allowlisted(second_properties);

    let marker_path = expected_capability_marker_path(&home, &state);
    assert!(marker_path.exists());
    assert!(!marker_path.starts_with(&data_root));
    assert_eq!(
        fs::read_to_string(&marker_path).unwrap(),
        "schema_version=1\n"
    );
    assert!(!expected_capability_claim_path(&home, &state).exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(marker_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn capability_snapshot_failure_keeps_claim_and_suppresses_replay() {
    let temp = tempdir();
    let delivery_dir = temp.path().join("missing-delivery-dir");
    let events_path = delivery_dir.join("analytics.jsonl");
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
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let marker_path = expected_capability_marker_path(&home, &state);
    let claim_path = expected_capability_claim_path(&home, &state);
    assert!(!events_path.exists());
    assert!(
        !marker_path.exists(),
        "failed delivery must not look successfully reported"
    );
    assert!(
        claim_path.exists(),
        "uncertain delivery must retain the claim"
    );

    fs::create_dir_all(&delivery_dir).unwrap();
    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["events"].as_array().unwrap().len(), 1);
    let properties = analytics_event_properties(&events[0]);
    for key in CAPABILITY_PROPERTY_KEYS {
        assert!(!properties.contains_key(key));
    }
    assert_analytics_properties_are_allowlisted(properties);
    assert!(!marker_path.exists());
    assert!(claim_path.exists());
}

#[test]
fn concurrent_invocations_claim_at_most_one_capability_snapshot() {
    let temp = tempdir();
    let binary = copied_ctx_binary(&temp);
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    fs::create_dir_all(&home).unwrap();

    let mut children = Vec::new();
    for index in 0..8 {
        children.push(
            std::process::Command::new(&binary)
                .arg("doctor")
                .env("CTX_DATA_ROOT", temp.path().join(format!("data-{index}")))
                .env("HOME", &home)
                .env("XDG_STATE_HOME", &state)
                .env("LOCALAPPDATA", &state)
                .env_remove("CTX_ANALYTICS_ENABLED")
                .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
                .env("CTX_UPGRADE_AUTO", "off")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    let body = fs::read_to_string(&events_path).unwrap();
    assert_eq!(body.matches("capability_snapshot_schema").count(), 1);
    assert!(expected_capability_marker_path(&home, &state).exists());
    assert!(!expected_capability_claim_path(&home, &state).exists());
}

#[test]
fn existing_claim_suppresses_replay_without_being_rewritten() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    let claim_path = expected_capability_claim_path(&home, &state);
    fs::create_dir_all(claim_path.parent().unwrap()).unwrap();
    fs::write(&claim_path, "existing-claim\n").unwrap();

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let event = read_analytics_events(&events_path).remove(0);
    for key in CAPABILITY_PROPERTY_KEYS {
        assert!(!analytics_event_properties(&event).contains_key(key));
    }
    assert_eq!(fs::read_to_string(claim_path).unwrap(), "existing-claim\n");
}

#[cfg(unix)]
#[test]
fn capability_claim_symlink_is_never_followed_or_overwritten() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    let sentinel = temp.path().join("sentinel.txt");
    let claim_path = expected_capability_claim_path(&home, &state);
    fs::create_dir_all(claim_path.parent().unwrap()).unwrap();
    fs::write(&sentinel, "do-not-touch\n").unwrap();
    symlink(&sentinel, &claim_path).unwrap();

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "do-not-touch\n");
    assert!(fs::symlink_metadata(claim_path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn analytics_device_identity_symlink_is_never_followed_or_overwritten() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    let sentinel = temp.path().join("sentinel.json");
    let device_path = expected_device_path(&home, &state);
    fs::create_dir_all(device_path.parent().unwrap()).unwrap();
    fs::write(&sentinel, "{}\n").unwrap();
    symlink(&sentinel, &device_path).unwrap();

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    assert!(!events_path.exists());
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "{}\n");
    assert!(fs::symlink_metadata(device_path)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn status_emits_one_typed_event_when_enabled() {
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
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "status", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["initialized"], false);
    assert_eq!(properties["indexed_items_bucket"], "0");
    assert_analytics_properties_are_allowlisted(properties);
    assert!(data_root.join("install.json").exists());
    assert!(expected_device_path(&home, &state).exists());
}

#[test]
fn help_version_and_parse_errors_are_unobserved() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    for args in [vec!["--help"], vec!["--version"], vec!["not-a-command"]] {
        let should_fail = args == ["not-a-command"];
        let mut command = ctx(&temp);
        command
            .args(args)
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
        let assertion = command.assert();
        if should_fail {
            assertion.failure();
        } else {
            assertion.success();
        }
    }

    assert!(!events_path.exists());
    assert!(!data_root.join("install.json").exists());
    assert!(!expected_device_path(&home, &state).exists());
    assert_no_capability_state(&home, &state);
}

#[test]
fn import_index_and_sql_emit_closed_safe_summaries() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("private-home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    let fixture = provider_history_fixture("codex-sessions");
    fs::create_dir_all(&home).unwrap();

    for args in [
        vec![
            "import".to_owned(),
            "--provider".to_owned(),
            "codex".to_owned(),
            "--path".to_owned(),
            fixture.clone(),
            "--json".to_owned(),
            "--no-daemon".to_owned(),
        ],
        vec!["index".to_owned(), "status".to_owned(), "--json".to_owned()],
        vec![
            "sql".to_owned(),
            "SELECT 'raw-query-secret' AS value".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
    ] {
        ctx(&temp)
            .args(args)
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_UPGRADE_AUTO", "off")
            .assert()
            .success();
    }

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 3);
    assert_operation_event(&events[0], "import", "success");
    assert_operation_event(&events[1], "index", "success");
    assert_operation_event(&events[2], "sql", "success");
    let import = analytics_event_properties(&events[0]);
    assert_eq!(import["source_mode"], "explicit_path");
    assert_eq!(import["provider_filter"], "codex");
    assert_eq!(import["import_outcome"], "success");
    let index = analytics_event_properties(&events[1]);
    assert_eq!(index["index_operation"], "status");
    assert!(index["lexical_state"].as_str().is_some());
    let sql = analytics_event_properties(&events[2]);
    assert_eq!(sql["input"], "inline");
    assert_eq!(sql["returned_rows_bucket"], "1");
    assert_eq!(sql["returned_columns_bucket"], "1");
    for event in &events {
        assert_analytics_properties_are_allowlisted(analytics_event_properties(event));
        assert_no_json_string_contains(
            event,
            &["raw-query-secret", fixture.as_str(), home.to_str().unwrap()],
        );
    }
}

#[test]
fn daemon_status_emits_one_typed_event_when_enabled() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["daemon", "status"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let payloads = read_analytics_events(&events_path);
    assert_eq!(payloads.len(), 1);
    let events = payloads[0]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_name"], "operation_completed");
    assert_eq!(events[0]["surface"], "daemon");
    assert_eq!(events[0]["operation"], "status");
    assert_eq!(events[0]["outcome"], "success");
    assert!(data_root.join("install.json").exists());
    assert!(expected_device_path(&home, &state).exists());
    assert!(expected_capability_marker_path(&home, &state).exists());
}

#[test]
fn daemon_status_opt_out_emits_nothing_and_creates_no_analytics_identities() {
    let temp = tempdir();
    let events_path = temp.path().join("analytics.jsonl");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["daemon", "status"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    assert!(!events_path.exists());
    assert!(!data_root.exists());
    assert!(!expected_device_path(&home, &state).exists());
    assert_no_capability_state(&home, &state);
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
            .arg("doctor")
            .env("CTX_DATA_ROOT", data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .assert()
            .success();
    }

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 2);
    let install_a = events[0]["data_root_id"].as_str().unwrap();
    let install_b = events[1]["data_root_id"].as_str().unwrap();
    let device_a = events[0]["client_profile_id"].as_str().unwrap();
    let device_b = events[1]["client_profile_id"].as_str().unwrap();
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
    let private_query = "prompt text source-body-secret /home/alice/private/acme-secret \
        repo@example.com host.internal 192.0.2.44 bearer-token-secret private-credential-secret";

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
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["docs", "search", "private prompt text", "--limit", "1"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["upgrade", "status"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    ctx(&temp)
        .args(["show", "session", "not-a-uuid-secret"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 4);
    let operations = events
        .iter()
        .map(|event| event["events"][0]["operation"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(operations, ["search", "docs", "upgrade", "show"]);

    let search_properties = analytics_event_properties(&events[0]);
    assert_eq!(search_properties["query_length_bucket"], "101-500");
    assert_eq!(search_properties["query_term_count_bucket"], "6-20");
    assert_eq!(search_properties["search_refresh_mode"], "off");
    assert_eq!(search_properties["search_refresh_status"], "skipped");
    assert_eq!(search_properties["zero_result"], true);
    assert_eq!(search_properties["had_existing_store_before_search"], true);
    assert_eq!(
        search_properties["indexed_content_before_search_known"],
        true
    );
    assert_eq!(
        search_properties["had_indexed_content_before_search"],
        false
    );
    assert_eq!(search_properties["store_created_by_search"], false);
    assert_eq!(search_properties["has_indexed_content_after_search"], false);
    assert!(search_properties.get("query_duration_bucket").is_some());
    assert!(search_properties.get("render_duration_bucket").is_some());
    assert_eq!(events[3]["events"][0]["outcome"], "failure");

    for event in &events {
        assert_analytics_properties_are_allowlisted(analytics_event_properties(event));
        assert_no_json_string_contains(
            event,
            &[
                private_query,
                "private prompt text",
                "source-body-secret",
                "bearer-token-secret",
                "private-credential-secret",
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
fn search_analytics_reports_when_search_creates_empty_store() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("ctx-data");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["search", "activation telemetry"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "search", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["search_refresh_mode"], "background");
    assert_eq!(properties["search_refresh_status"], "no_sources");
    assert_eq!(properties["had_existing_store_before_search"], false);
    assert_eq!(properties["indexed_content_before_search_known"], true);
    assert_eq!(properties["had_indexed_content_before_search"], false);
    assert_eq!(properties["store_created_by_search"], true);
    assert_eq!(properties["has_indexed_content_after_search"], false);
    assert!(!data_root.join("config.toml").exists());
    assert!(!data_root.join("daemon/status.json").exists());
    assert!(!data_root.join("daemon/daemon.lock").exists());
    assert_analytics_properties_are_allowlisted(properties);
}

#[test]
fn search_analytics_reports_existing_indexed_content() {
    let temp = tempdir();
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let data_root = temp.path().join("ctx-data");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();
    let fixture = provider_history_fixture("codex-sessions");

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--json",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    ctx(&temp)
        .args(["search", "test failure", "--refresh", "off"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "search", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["had_existing_store_before_search"], true);
    assert_eq!(properties["indexed_content_before_search_known"], true);
    assert_eq!(properties["had_indexed_content_before_search"], true);
    assert_eq!(properties["store_created_by_search"], false);
    assert_eq!(properties["has_indexed_content_after_search"], true);
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_manual_dry_run_outcome() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--dry-run", "--json"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off");
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "dry_run");
    assert_eq!(properties["dry_run"], true);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_eq!(properties["self_upgrade_allowed"], true);
    assert_eq!(properties["auto_upgrade_allowed"], true);
    assert!(properties.get("upgrade_warning_count_bucket").is_some());
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_manual_apply_success() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--json"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off");
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "applied");
    assert_eq!(properties["dry_run"], false);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], true);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_manual_failure_kind() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                release.artifact_sha
            ),
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                "f".repeat(64)
            ),
        )
    });
    let events_path = temp.path().join("analytics.jsonl");

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--json"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off");
    fake_release_env(&mut command, &release).assert().failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "failure");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "failed");
    assert_eq!(properties["upgrade_failure_kind"], "artifact_verify");
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_background_auto_upgrade_outcome() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--background"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_UPGRADE_AUTO", "apply")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "applied");
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], true);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_background_failure_kind() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    rewrite_fake_release_metadata(&release, |metadata| {
        metadata.replace(
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                release.artifact_sha
            ),
            &format!(
                "CTX_RELEASE_SHA256_{}={}\n",
                test_platform_key(),
                "f".repeat(64)
            ),
        )
    });
    let events_path = temp.path().join("analytics.jsonl");

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--background"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_UPGRADE_AUTO", "apply")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "failure");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "failed");
    assert_eq!(properties["upgrade_failure_kind"], "artifact_verify");
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_analytics_properties_are_allowlisted(properties);
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_background_locked_skip_and_backs_off() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        data_root.join("upgrade.lock"),
        format!("{} {now}\n", std::process::id()),
    )
    .unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "--background"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_UPGRADE_AUTO", "apply")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "locked");
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_analytics_properties_are_allowlisted(properties);

    let state_json: Value =
        serde_json::from_slice(&fs::read(data_root.join("upgrade-state.json")).unwrap()).unwrap();
    assert_eq!(state_json["status"], "locked");
    assert!(state_json["last_checked_unix_s"].as_u64().is_some());
}

#[cfg(unix)]
#[test]
fn upgrade_analytics_reports_background_skipped_in_ci() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["upgrade", "--background"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CI", "1")
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_UPGRADE_AUTO", "apply")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "upgrade", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "skipped");
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_analytics_properties_are_allowlisted(properties);
}

#[test]
fn hosted_install_marker_enriches_analytics_event_without_properties_leak() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let binary = copied_ctx_binary(&temp);
    let install_attempt_id = "ia_01JZCTXHOSTED";
    let marker_secret = "marker-secret-must-not-leak";
    fs::write(
        hosted_install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": install_attempt_id,
            "installed_at": ctx_history_core::utc_now(),
            "installer_private_note": marker_secret,
        }))
        .unwrap(),
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
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
fn expired_hosted_install_attribution_is_ignored() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let binary = copied_ctx_binary(&temp);
    fs::write(
        hosted_install_marker_path(&binary),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_expired_hosted_attempt",
            "installed_at": ctx_history_core::utc_now() - chrono::TimeDelta::days(7),
        }))
        .unwrap(),
    )
    .unwrap();

    ctx_from_binary(&temp, &binary)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let cli_event = analytics_cli_event(&events[0]);
    assert!(cli_event.get("install_attempt_id").is_none());
    assert!(analytics_event_properties(&events[0])
        .get("install_manager")
        .is_none());
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
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
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
fn setup_analytics_emits_one_terminal_event() {
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
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "setup", "success");
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["catalog_only"], true);
    assert_eq!(properties["setup_mode"], "catalog_only");
    assert_eq!(properties["has_indexed_content_after_setup"], false);
    assert_capability_snapshot_is_coarse(properties);
    assert_analytics_properties_are_allowlisted(properties);
}

#[test]
fn foreground_setup_analytics_includes_import_outcome() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let sessions = home.join(".codex").join("sessions").join("2026/07/13");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-setup-analytics.jsonl"),
        concat!(
            r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"setup-analytics","timestamp":"2026-07-13T12:00:00.000Z","cwd":"/repo","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"setup analytics import oracle"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    ctx(&temp)
        .args(["setup", "--wait", "--progress", "none"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "setup", "success");
    let completed = analytics_event_properties(&events[0]);
    assert_eq!(completed["import_outcome"], "success");
    assert_eq!(completed["import_failure_scope"], "none");
    assert_eq!(completed["import_failure_type"], "none");
    assert_analytics_properties_are_allowlisted(completed);
}

#[test]
fn foreground_provider_refreshes_batch_once_and_report_changed_then_no_op() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let fixture = provider_history_fixture("codex-sessions");
    fs::create_dir_all(&home).unwrap();

    for _ in 0..2 {
        ctx(&temp)
            .args([
                "import",
                "--provider",
                "codex",
                "--path",
                &fixture,
                "--json",
                "--no-daemon",
            ])
            .env("CTX_DATA_ROOT", &data_root)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("LOCALAPPDATA", &state)
            .env_remove("CTX_ANALYTICS_ENABLED")
            .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
            .env("CTX_UPGRADE_AUTO", "off")
            .assert()
            .success();
    }

    let payloads = read_analytics_events(&events_path);
    assert_eq!(payloads.len(), 2, "each invocation must send one batch");
    for (payload, expected_change) in payloads.iter().zip(["changed", "no_op"]) {
        let events = payload["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "records and files must not emit events");
        assert_eq!(events[0]["event_name"], "provider_refresh_completed");
        assert_eq!(events[1]["event_name"], "operation_completed");
        assert_eq!(events[1]["operation"], "import");

        let refresh = events[0]["properties"].as_object().unwrap();
        assert_eq!(refresh["provider"], "codex");
        assert_eq!(refresh["trigger"], "import");
        assert_eq!(refresh["source_mode"], "explicit_path");
        assert_eq!(refresh["change"], expected_change);
        assert_eq!(refresh["work_remaining"], false);
        for bucket in [
            "sources_bucket",
            "sessions_bucket",
            "events_bucket",
            "edges_bucket",
            "skips_bucket",
            "rejections_bucket",
            "failures_bucket",
            "bytes_bucket",
        ] {
            assert!(refresh[bucket].as_str().is_some(), "missing {bucket}");
        }
        for forbidden in [
            "content",
            "path",
            "source_id",
            "session_id",
            "record_id",
            "provider_key",
            "source_format",
            "ingestion_mode",
            "rewrite_reason",
        ] {
            assert!(
                !refresh.contains_key(forbidden),
                "provider refresh exposed {forbidden}: {refresh:#?}"
            );
        }
        assert_no_json_string_contains(payload, &[fixture.as_str(), home.to_str().unwrap()]);
    }
}

#[test]
fn foreground_provider_refresh_opt_out_suppresses_the_whole_batch() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    let fixture = provider_history_fixture("codex-sessions");
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--json",
            "--no-daemon",
        ])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env("CTX_ANALYTICS_ENABLED", "false")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .success();

    assert!(!events_path.exists());
    assert!(!expected_device_path(&home, &state).exists());
    assert_no_capability_state(&home, &state);
}

#[test]
fn setup_analytics_emits_one_failure_event() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(data_root.join("work.sqlite")).unwrap();
    fs::create_dir_all(&home).unwrap();

    ctx(&temp)
        .args(["setup", "--progress", "none"])
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
        .assert()
        .failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    assert_operation_event(&events[0], "setup", "failure");
    let properties = analytics_event_properties(&events[0]);
    assert!(properties.get("has_indexed_content_after_setup").is_none());
    assert_capability_snapshot_is_coarse(properties);
    assert_analytics_properties_are_allowlisted(properties);
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
        .env("CTX_UPGRADE_AUTO", "off")
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
    assert!(
        !expected_capability_marker_path(&home, &state).exists(),
        "setup analytics opt-out should not create a capability marker"
    );
    assert_no_capability_state(&home, &state);
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
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_DRY_RUN", "1")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_AUTO", "off")
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
    assert!(
        !expected_capability_marker_path(&home, &state).exists(),
        "setup analytics dry run should not create a capability marker"
    );
    assert_no_capability_state(&home, &state);
}
