#[allow(unused_imports)]
use super::*;

#[cfg(unix)]
#[test]
pub(crate) fn upgrade_status_check_and_apply_support_managed_installs() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");

    let status = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "status", "--json"]),
        &release,
    ));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["install"]["managed"], true);

    let check = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "check", "--json"]),
        &release,
    ));
    assert_eq!(check["status"], "available");
    assert_eq!(check["latest_version"], "9.9.9");
    assert_eq!(check["managed"], true);

    let dry_run = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--dry-run", "--json"]),
        &release,
    ));
    assert_eq!(dry_run["status"], "dry_run");
    assert_eq!(dry_run["applied"], false);

    let applied = json_output(fake_release_env(
        ctx(&temp).args(["upgrade", "--json"]),
        &release,
    ));
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["applied"], true);
    assert_eq!(
        fs::read_to_string(&release.target).unwrap(),
        "#!/bin/sh\nprintf 'ctx 9.9.9\\n'\n"
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&release.target)).unwrap()).unwrap();
    assert_eq!(marker["version"], "9.9.9");
    assert_eq!(marker["sha256"], release.artifact_sha);
    assert_eq!(marker["install_attempt_id"], "ia_test_upgrade_attempt");
}

#[cfg(unix)]
#[test]
pub(crate) fn upgrade_status_reports_path_shadowing() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let shadow_dir = temp.path().join("shadow-bin");
    fs::create_dir_all(&shadow_dir).unwrap();
    let shadow_ctx = shadow_dir.join("ctx");
    write_fake_ctx_binary(&shadow_ctx, "0.9.0");
    let managed_dir = release.target.parent().unwrap();
    let path = std::env::join_paths([shadow_dir.as_path(), managed_dir]).unwrap();

    let mut command = ctx(&temp);
    command
        .args(["upgrade", "status", "--json"])
        .env("PATH", path);
    let status = json_output(fake_release_env(&mut command, &release));

    assert_eq!(status["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        status["path"]["entries"][0]["path"],
        shadow_ctx.display().to_string()
    );
    assert!(status["path"]["entries"][0]["version"].is_null());
    assert!(status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| { warning.as_str().unwrap().contains("PATH resolves ctx to") }));
}

#[cfg(unix)]
#[test]
pub(crate) fn upgrade_commands_do_not_execute_hanging_shadow_path_ctx() {
    for args in [
        ["upgrade", "status", "--json"].as_slice(),
        ["upgrade", "check", "--json"].as_slice(),
        ["upgrade", "--json"].as_slice(),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let shadow_dir = temp.path().join("shadow-bin");
        fs::create_dir_all(&shadow_dir).unwrap();
        let shadow_ctx = shadow_dir.join("ctx");
        write_hanging_ctx_binary(&shadow_ctx);
        let marker = temp.path().join("shadow-ran");
        let managed_dir = release.target.parent().unwrap();
        let path = std::env::join_paths([shadow_dir.as_path(), managed_dir]).unwrap();

        let started = Instant::now();
        let mut command = ctx(&temp);
        command
            .args(args)
            .env("PATH", &path)
            .env("CTX_SHADOW_MARKER", &marker);
        let output = json_output(fake_release_env(&mut command, &release));
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "ctx {args:?} should not wait for shadow PATH binaries; elapsed {elapsed:?}"
        );
        assert_eq!(
            output["path"]["entries"][0]["path"],
            shadow_ctx.display().to_string()
        );
        assert!(
            output["path"]["entries"][0]["version"].is_null(),
            "shadow ctx versions should not be probed"
        );
        assert!(
            !marker.exists(),
            "PATH shadow ctx should not have been executed"
        );
    }
}

#[test]
pub(crate) fn analytics_env_opt_out_wins_over_enable_flag() {
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
pub(crate) fn mcp_status_and_tools_list_are_read_only_without_initialized_store() {
    let temp = tempdir();
    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "status",
                    "arguments": {}
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ctx");
    assert_eq!(
        responses[0]["result"]["capabilities"]["tools"]["listChanged"],
        false
    );

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    for expected in [
        "status",
        "sources",
        "search",
        "sql",
        "show_session",
        "show_event",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == expected),
            "missing MCP tool {expected} in {tools:#?}"
        );
    }
    assert!(
        tools.iter().all(|tool| tool["name"] != "research"),
        "MCP research tool should not be exposed in {tools:#?}"
    );
    let search_tool = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    let providers = search_tool["inputSchema"]["properties"]["provider"]["enum"]
        .as_array()
        .unwrap();
    assert!(providers.iter().any(|provider| provider == "copilot-cli"));
    assert!(providers.iter().any(|provider| provider == "copilot_cli"));
    assert!(providers.iter().any(|provider| provider == "qwen-code"));
    assert!(providers.iter().any(|provider| provider == "qwen_code"));
    assert!(providers.iter().any(|provider| provider == "kimi-code-cli"));
    assert!(providers.iter().any(|provider| provider == "kimi_code_cli"));
    assert!(providers.iter().any(|provider| provider == "kiro-cli"));
    assert!(providers.iter().any(|provider| provider == "kiro_cli"));
    assert!(providers.iter().any(|provider| provider == "lingma"));
    assert!(providers.iter().any(|provider| provider == "codebuddy"));
    assert!(providers.iter().any(|provider| provider == "auggie"));
    assert!(providers.iter().any(|provider| provider == "zed"));
    assert!(providers.iter().any(|provider| provider == "forgecode"));
    assert!(providers.iter().any(|provider| provider == "deepagents"));
    assert!(providers.iter().any(|provider| provider == "mistral-vibe"));
    assert!(providers.iter().any(|provider| provider == "mistral_vibe"));
    assert!(providers.iter().any(|provider| provider == "mux"));
    assert!(providers.iter().any(|provider| provider == "rovodev"));
    assert!(providers.iter().any(|provider| provider == "cline"));
    assert!(providers.iter().any(|provider| provider == "roo"));
    assert!(providers.iter().any(|provider| provider == "roo_code"));
    let status = &responses[2]["result"]["structuredContent"];
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["read_only"], true);
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "MCP status should not initialize the ctx store"
    );
}
