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
        .arg("doctor")
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
    assert_eq!(event["events"][0]["properties"]["action"], "doctor");
    assert_eq!(
        event["events"][0]["properties"]["analytics_client"],
        "ctx-cli"
    );
    assert_eq!(
        event["events"][0]["properties"]["finding_count_bucket"],
        "2-5"
    );
    assert_eq!(
        event["events"][0]["properties"]["auto_upgrade_spawn_status"],
        "marker_invalid"
    );
    assert_eq!(event["events"][0]["properties"]["auto_upgrade_probe"], true);
    assert_eq!(event["events"][0]["properties"]["auto_upgrade_due"], true);
    assert_eq!(
        event["events"][0]["properties"]["auto_upgrade_spawned"],
        false
    );
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
fn status_does_not_emit_analytics_or_create_identities_when_enabled() {
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
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "status must not write analytics events"
    );
    assert!(
        !data_root.exists(),
        "status must not create the data root for install identity"
    );
    assert!(
        !expected_device_path(&home, &state).exists(),
        "status must not create a device identity"
    );
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "search");
    assert_eq!(properties["search_refresh_status"], "no_sources");
    assert_eq!(properties["had_existing_store_before_search"], false);
    assert_eq!(properties["indexed_content_before_search_known"], true);
    assert_eq!(properties["had_indexed_content_before_search"], false);
    assert_eq!(properties["store_created_by_search"], true);
    assert_eq!(properties["has_indexed_content_after_search"], false);
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
        .env("CTX_ANALYTICS_OFF", "1")
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .success();

    ctx(&temp)
        .args(["search", "test failure", "--refresh", "off"])
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
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "search");
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1");
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "dry_run");
    assert_eq!(properties["dry_run"], true);
    assert_eq!(properties["background"], false);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_eq!(properties["self_upgrade_allowed"], true);
    assert_eq!(properties["auto_upgrade_allowed"], true);
    assert!(properties.get("upgrade_warning_count_bucket").is_some());
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1");
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "applied");
    assert_eq!(properties["dry_run"], false);
    assert_eq!(properties["background"], false);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], true);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1");
    fake_release_env(&mut command, &release).assert().failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "manual");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "failed");
    assert_eq!(properties["upgrade_failure_kind"], "artifact_verify");
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(analytics_cli_event(&events[0])["success"], false);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "applied");
    assert_eq!(properties["background"], true);
    assert_eq!(properties["update_available"], true);
    assert_eq!(properties["upgrade_applied"], true);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(properties["managed_install"], true);
    assert_eq!(properties["upgrade_channel"], "stable");
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "failed");
    assert_eq!(properties["upgrade_failure_kind"], "artifact_verify");
    assert_eq!(properties["background"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(analytics_cli_event(&events[0])["success"], false);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path));
    fake_release_env(&mut command, &release).assert().success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "locked");
    assert_eq!(properties["background"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 1);
    let properties = analytics_event_properties(&events[0]);
    assert_eq!(properties["action"], "upgrade");
    assert_eq!(properties["upgrade_mode"], "auto");
    assert_eq!(properties["upgrade_operation"], "apply");
    assert_eq!(properties["upgrade_status"], "skipped");
    assert_eq!(properties["background"], true);
    assert_eq!(properties["upgrade_applied"], false);
    assert_eq!(properties["upgrade_scheduled"], false);
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
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
        .arg("doctor")
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
        .arg("doctor")
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
    let started_properties = analytics_event_properties(&events[0]);
    let completed_properties = analytics_event_properties(&events[1]);
    assert!(started_properties.get("setup_attempt_key").is_none());
    assert!(completed_properties.get("setup_attempt_key").is_none());
    assert!(started_properties.get("setup_completed").is_none());
    assert!(started_properties.get("setup_result").is_none());
    assert!(started_properties
        .get("has_indexed_content_after_setup")
        .is_none());
    assert_eq!(completed_properties["setup_completed"], true);
    assert_eq!(completed_properties["setup_result"], "success");
    assert_eq!(
        completed_properties["has_indexed_content_after_setup"],
        false
    );
    for event in &events {
        assert_eq!(analytics_cli_event(event)["event_name"], "cli_invocation");
        assert_eq!(analytics_cli_event(event)["status"], "ok");
        assert_eq!(analytics_cli_event(event)["success"], true);
        assert_analytics_properties_are_allowlisted(analytics_event_properties(event));
    }
}

#[test]
fn setup_analytics_emits_failure_completion_event() {
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
        .env_remove("CTX_ANALYTICS_OFF")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .env("CTX_UPGRADE_OFF", "1")
        .assert()
        .failure();

    let events = read_analytics_events(&events_path);
    assert_eq!(events.len(), 2);
    let started_properties = analytics_event_properties(&events[0]);
    let completed_properties = analytics_event_properties(&events[1]);
    assert_eq!(started_properties["action"], "setup_started");
    assert_eq!(completed_properties["action"], "setup");
    assert_eq!(analytics_cli_event(&events[0])["success"], true);
    assert_eq!(analytics_cli_event(&events[1])["success"], false);
    assert_eq!(completed_properties["setup_completed"], false);
    assert_eq!(completed_properties["setup_result"], "failure");
    assert_eq!(completed_properties["failure_kind"], "command_error");
    assert!(completed_properties
        .get("has_indexed_content_after_setup")
        .is_none());
    for event in &events {
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
        .arg("doctor")
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
        .arg("doctor")
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
        .arg("doctor")
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
