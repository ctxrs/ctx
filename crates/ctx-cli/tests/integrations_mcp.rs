mod support;

use support::*;

#[test]
fn integrations_mcp_install_defaults_to_detected_agents_and_is_idempotent() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex")).unwrap();
    fs::create_dir_all(temp.path().join(".cursor")).unwrap();

    let first = json_output(ctx(&temp).args(["integrations", "install", "mcp", "--json"]));
    assert_eq!(first["integration"], "mcp");
    assert_eq!(first["server"]["command"], "ctx");
    let agents = first["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["agent"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(agents, vec!["codex", "cursor"]);
    assert!(first["results"].as_array().unwrap().iter().all(|row| {
        row["success"] == true && row["status"] == "current" && row["modified"] == true
    }));

    let codex_config = fs::read_to_string(temp.path().join(".codex").join("config.toml")).unwrap();
    assert!(codex_config.contains("[mcp_servers.ctx]"));
    assert!(codex_config.contains("command = \"ctx\""));

    let cursor_config = fs::read_to_string(temp.path().join(".cursor").join("mcp.json")).unwrap();
    let cursor_json: Value = serde_json::from_str(&cursor_config).unwrap();
    assert_eq!(cursor_json["mcpServers"]["ctx"]["type"], "stdio");
    assert_eq!(
        cursor_json["mcpServers"]["ctx"]["args"],
        json!(["mcp", "serve"])
    );

    let second = json_output(ctx(&temp).args(["integrations", "install", "mcp", "--json"]));
    assert!(second["results"].as_array().unwrap().iter().all(|row| {
        row["success"] == true && row["already_installed"] == true && row["modified"] == false
    }));
}

#[test]
fn integrations_mcp_provider_alias_installs_explicit_undetected_agent() {
    let temp = tempdir();

    let output = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--provider",
        "qwen-code",
        "--json",
    ]));
    assert_eq!(output["results"][0]["agent"], "qwen-code");
    assert_eq!(output["results"][0]["detected"], false);
    assert_eq!(output["results"][0]["modified"], true);

    let qwen_config = fs::read_to_string(temp.path().join(".qwen").join("settings.json")).unwrap();
    let qwen_json: Value = serde_json::from_str(&qwen_config).unwrap();
    assert_eq!(qwen_json["mcpServers"]["ctx"]["command"], "ctx");
    assert_eq!(
        qwen_json["mcpServers"]["ctx"]["args"],
        json!(["mcp", "serve"])
    );
}

#[test]
fn integrations_mcp_refuses_conflicting_ctx_entry_unless_forced() {
    let temp = tempdir();
    let cursor_dir = temp.path().join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"mcpServers":{"ctx":{"command":"old-ctx","args":[]}}}"#,
    )
    .unwrap();

    let output = ctx(&temp)
        .args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            "cursor",
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["success"], false);
    assert_eq!(json["results"][0]["status"], "conflict");
    assert!(json["results"][0]["error"]
        .as_str()
        .unwrap()
        .contains("--force"));
    assert!(fs::read_to_string(cursor_dir.join("mcp.json"))
        .unwrap()
        .contains("old-ctx"));

    let forced = json_output(ctx(&temp).args([
        "integrations",
        "install",
        "mcp",
        "--agent",
        "cursor",
        "--force",
        "--json",
    ]));
    assert_eq!(forced["results"][0]["success"], true);
    assert_eq!(forced["results"][0]["previous_status"], "conflict");
    let cursor_config = fs::read_to_string(cursor_dir.join("mcp.json")).unwrap();
    let cursor_json: Value = serde_json::from_str(&cursor_config).unwrap();
    assert_eq!(cursor_json["mcpServers"]["ctx"]["command"], "ctx");
}

#[test]
fn integrations_mcp_reports_invalid_config_without_overwriting() {
    let temp = tempdir();
    let qwen_dir = temp.path().join(".qwen");
    fs::create_dir_all(&qwen_dir).unwrap();
    fs::write(qwen_dir.join("settings.json"), "{ not json").unwrap();

    let output = ctx(&temp)
        .args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            "qwen-code",
            "--json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["results"][0]["status"], "invalid_config");
    assert_eq!(
        fs::read_to_string(qwen_dir.join("settings.json")).unwrap(),
        "{ not json"
    );
}

#[test]
fn integrations_mcp_project_reports_unsupported_global_only_agents() {
    let temp = tempdir();

    let output = json_output(ctx(&temp).args([
        "integrations",
        "status",
        "mcp",
        "--project",
        "--agent",
        "github-copilot",
        "--json",
    ]));
    assert_eq!(output["results"][0]["status"], "unsupported");
    assert_eq!(output["results"][0]["supported"], false);
}

#[test]
fn integrations_mcp_project_default_only_uses_detected_project_configs() {
    let temp = tempdir();

    let empty = json_output(ctx(&temp).current_dir(temp.path()).args([
        "integrations",
        "install",
        "mcp",
        "--project",
        "--json",
    ]));
    assert_eq!(empty["results"].as_array().unwrap().len(), 0);

    fs::create_dir_all(temp.path().join(".warp")).unwrap();
    let output = json_output(ctx(&temp).current_dir(temp.path()).args([
        "integrations",
        "install",
        "mcp",
        "--project",
        "--json",
    ]));
    assert_eq!(output["results"].as_array().unwrap().len(), 1);
    assert_eq!(output["results"][0]["agent"], "warp");

    let warp_config = fs::read_to_string(temp.path().join(".warp").join(".mcp.json")).unwrap();
    let warp_json: Value = serde_json::from_str(&warp_config).unwrap();
    assert_eq!(warp_json["mcpServers"]["ctx"]["command"], "ctx");
}
