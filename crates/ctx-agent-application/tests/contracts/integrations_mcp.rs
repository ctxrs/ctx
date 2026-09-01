mod support;

use support::*;

fn human_stdout(command: &mut assert_cmd::Command) -> String {
    let output = command.assert().success().get_output().clone();
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(output.stdout.contains(&0x1b), "{:?}", output.stdout);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn integrations_mcp_human_receipts_use_ui_and_json_stays_plain() {
    let temp = tempdir();

    let missing = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "status",
        "mcp",
        "--agent",
        "codex",
    ]));
    assert!(
        missing.contains("ctx MCP integration needs attention"),
        "{missing}"
    );
    assert!(missing.contains("missing"), "{missing}");

    let installed = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "install",
        "mcp",
        "--agent",
        "codex",
    ]));
    assert!(
        installed.contains("ctx MCP integration installed"),
        "{installed}"
    );
    assert!(installed.contains("modified"), "{installed}");

    let current = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "status",
        "mcp",
        "--agent",
        "codex",
    ]));
    assert!(
        current.contains("ctx MCP integration is current"),
        "{current}"
    );
    assert!(current.contains("current"), "{current}");

    let machine = ctx(&temp)
        .args([
            "--color=always",
            "integrations",
            "status",
            "mcp",
            "--agent",
            "codex",
            "--format=json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!machine.stdout.contains(&0x1b), "{:?}", machine.stdout);
    let value: Value = serde_json::from_slice(&machine.stdout).unwrap();
    assert_eq!(value["results"][0]["status"], "current");

    let removed = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "remove",
        "mcp",
        "--agent",
        "codex",
    ]));
    assert!(removed.contains("ctx MCP integration removed"), "{removed}");

    let absent = human_stdout(ctx(&temp).args([
        "--color=always",
        "integrations",
        "remove",
        "mcp",
        "--agent",
        "codex",
    ]));
    assert!(
        absent.contains("ctx MCP integration is already absent"),
        "{absent}"
    );
}

#[test]
fn integrations_mcp_install_defaults_to_detected_agents_and_is_idempotent() {
    let temp = tempdir();
    fs::create_dir_all(temp.path().join(".codex")).unwrap();
    fs::create_dir_all(temp.path().join(".cursor")).unwrap();

    let first = json_output(ctx(&temp).args(["integrations", "install", "mcp", "--format=json"]));
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

    let second = json_output(ctx(&temp).args(["integrations", "install", "mcp", "--format=json"]));
    assert!(second["results"].as_array().unwrap().iter().all(|row| {
        row["success"] == true && row["already_installed"] == true && row["modified"] == false
    }));
}

#[test]
fn integrations_mcp_remove_is_idempotent_and_preserves_unrelated_configuration() {
    let temp = tempdir();
    let cursor_dir = temp.path().join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    fs::write(
        cursor_dir.join("mcp.json"),
        r#"{"theme":"dark","mcpServers":{"ctx":{"type":"stdio","command":"ctx","args":["mcp","serve"]},"other":{"command":"other","args":[]}}}"#,
    )
    .unwrap();

    let first = json_output(ctx(&temp).args([
        "integrations",
        "remove",
        "mcp",
        "--agent",
        "cursor",
        "--format=json",
    ]));
    assert_eq!(first["integration"], "mcp");
    assert_eq!(first["results"][0]["success"], true);
    assert_eq!(first["results"][0]["previous_status"], "current");
    assert_eq!(first["results"][0]["status"], "missing");
    assert_eq!(first["results"][0]["already_absent"], false);
    assert_eq!(first["results"][0]["modified"], true);

    let config: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(config["theme"], "dark");
    assert!(config["mcpServers"].get("ctx").is_none());
    assert_eq!(config["mcpServers"]["other"]["command"], "other");

    let second = json_output(ctx(&temp).args([
        "integrations",
        "remove",
        "mcp",
        "--agent",
        "cursor",
        "--format=json",
    ]));
    assert_eq!(second["results"][0]["success"], true);
    assert_eq!(second["results"][0]["already_absent"], true);
    assert_eq!(second["results"][0]["modified"], false);
}

#[test]
fn integrations_mcp_remove_requires_force_for_a_conflicting_entry() {
    let temp = tempdir();
    let cursor_dir = temp.path().join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    let path = cursor_dir.join("mcp.json");
    let original = r#"{"unrelated":true,"mcpServers":{"ctx":{"command":"custom","args":[]}}}"#;
    fs::write(&path, original).unwrap();

    let human = ctx(&temp)
        .args(["integrations", "remove", "mcp", "--agent", "cursor"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(
        stderr.contains("ctx integrations remove mcp --agent cursor --force"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    let output = ctx(&temp)
        .args([
            "integrations",
            "remove",
            "mcp",
            "--agent",
            "cursor",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["status"], "conflict");
    assert_eq!(value["results"][0]["success"], false);
    assert_eq!(fs::read_to_string(&path).unwrap(), original);

    let forced = json_output(ctx(&temp).args([
        "integrations",
        "remove",
        "mcp",
        "--agent",
        "cursor",
        "--force",
        "--format=json",
    ]));
    assert_eq!(forced["results"][0]["success"], true);
    assert_eq!(forced["results"][0]["previous_status"], "conflict");
    assert_eq!(forced["results"][0]["status"], "missing");
    let config: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(config["unrelated"], true);
    assert!(config["mcpServers"].get("ctx").is_none());
}

#[test]
fn integrations_mcp_remove_rejects_structurally_invalid_yaml_even_with_force() {
    let temp = tempdir();
    let cases = [
        (
            "continue",
            temp.path().join(".continue").join("config.yaml"),
            "[]",
        ),
        (
            "goose",
            temp.path()
                .join(".config")
                .join("goose")
                .join("config.yaml"),
            "extensions: []\n",
        ),
    ];

    for (agent, path, original) in cases {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, original).unwrap();
        let output = ctx(&temp)
            .args([
                "integrations",
                "remove",
                "mcp",
                "--agent",
                agent,
                "--force",
                "--format=json",
            ])
            .assert()
            .failure()
            .get_output()
            .clone();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["results"][0]["status"], "invalid_config");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn integrations_mcp_remove_preserves_duplicate_json_keys_even_with_force() {
    let temp = tempdir();
    let cursor_path = temp.path().join(".cursor").join("mcp.json");
    let cursor_config = r#"{
  "mcpServers": {"other": {"command": "other"}},
  "mcpServers": {"ctx": {"command": "ctx", "args": ["mcp", "serve"]}}
}"#;
    fs::create_dir_all(cursor_path.parent().unwrap()).unwrap();
    fs::write(&cursor_path, cursor_config).unwrap();

    let cursor = ctx(&temp)
        .args([
            "integrations",
            "remove",
            "mcp",
            "--agent",
            "cursor",
            "--force",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let cursor_json: Value = serde_json::from_slice(&cursor.stdout).unwrap();
    assert_eq!(cursor_json["results"][0]["status"], "invalid_config");
    assert_eq!(fs::read_to_string(&cursor_path).unwrap(), cursor_config);

    let mimocode_dir = temp.path().join("mimocode-duplicate-config");
    let mimocode_path = mimocode_dir.join("mimocode.jsonc");
    let mimocode_config = r#"{
  // Both entries must remain byte-for-byte intact.
  "mcp": {
    "ctx": {"type": "local", "command": ["custom"]},
    "ctx": {"type": "local", "command": ["ctx", "mcp", "serve"]},
  },
}"#;
    fs::create_dir_all(&mimocode_dir).unwrap();
    fs::write(&mimocode_path, mimocode_config).unwrap();

    let mimocode = ctx(&temp)
        .env("MIMOCODE_CONFIG_DIR", &mimocode_dir)
        .args([
            "integrations",
            "remove",
            "mcp",
            "--agent",
            "mimocode",
            "--force",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let mimocode_json: Value = serde_json::from_slice(&mimocode.stdout).unwrap();
    assert_eq!(mimocode_json["results"][0]["status"], "invalid_config");
    assert_eq!(fs::read_to_string(&mimocode_path).unwrap(), mimocode_config);
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
        "--format=json",
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

    ctx(&temp)
        .args(["integrations", "install", "mcp", "--agent", "cursor"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("rerun with --force").not())
        .stderr(
            predicate::str::contains("Cursor MCP configuration was not changed")
                .and(predicate::str::contains(
                    "ctx integrations install mcp --agent cursor --force",
                ))
                .and(predicate::str::contains("failed to install MCP integration").not()),
        );

    let output = ctx(&temp)
        .args([
            "integrations",
            "install",
            "mcp",
            "--agent",
            "cursor",
            "--format=json",
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
    assert_eq!(
        output.stderr,
        b"Error: failed to install MCP integration for 1 target(s)\n"
    );
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
        "--format=json",
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
            "--format=json",
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

    let remove = ctx(&temp)
        .args([
            "integrations",
            "remove",
            "mcp",
            "--agent",
            "qwen-code",
            "--force",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let json: Value = serde_json::from_slice(&remove.stdout).unwrap();
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
        "--format=json",
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
        "--format=json",
    ]));
    assert_eq!(empty["results"].as_array().unwrap().len(), 0);

    fs::create_dir_all(temp.path().join(".warp")).unwrap();
    let output = json_output(ctx(&temp).current_dir(temp.path()).args([
        "integrations",
        "install",
        "mcp",
        "--project",
        "--format=json",
    ]));
    assert_eq!(output["results"].as_array().unwrap().len(), 1);
    assert_eq!(output["results"][0]["agent"], "warp");

    let warp_config = fs::read_to_string(temp.path().join(".warp").join(".mcp.json")).unwrap();
    let warp_json: Value = serde_json::from_str(&warp_config).unwrap();
    assert_eq!(warp_json["mcpServers"]["ctx"]["command"], "ctx");
}
