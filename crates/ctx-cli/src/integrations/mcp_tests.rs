use super::*;

#[test]
fn json_writer_adds_ctx_and_is_idempotent() {
    let first = update_json(
        r#"{"other":true}"#,
        JsonRoot::McpServers,
        JsonServerShape::StdioType,
        false,
    )
    .unwrap();
    let value: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(value["other"], true);
    assert_eq!(value["mcpServers"]["ctx"]["command"], "ctx");
    assert_eq!(value["mcpServers"]["ctx"]["args"], json!(["mcp", "serve"]));
    assert_eq!(value["mcpServers"]["ctx"]["type"], "stdio");
    assert_eq!(
        status_json(&first, JsonRoot::McpServers).unwrap(),
        McpConfigStatus::Current
    );
    let second = update_json(
        &first,
        JsonRoot::McpServers,
        JsonServerShape::StdioType,
        false,
    )
    .unwrap();
    assert_eq!(first, second);
}

#[test]
fn json_writer_preserves_conflicting_ctx_unless_forced() {
    let original = r#"{"mcpServers":{"ctx":{"command":"old","args":[]}}}"#;
    assert_eq!(
        status_json(original, JsonRoot::McpServers).unwrap(),
        McpConfigStatus::Conflict
    );
    assert!(update_json(
        original,
        JsonRoot::McpServers,
        JsonServerShape::Plain,
        false
    )
    .is_err());
    let forced = update_json(original, JsonRoot::McpServers, JsonServerShape::Plain, true).unwrap();
    let value: Value = serde_json::from_str(&forced).unwrap();
    assert_eq!(value["mcpServers"]["ctx"]["command"], "ctx");
}

#[test]
fn json_writer_reports_invalid_shapes() {
    assert!(update_json("[]", JsonRoot::McpServers, JsonServerShape::Plain, false).is_err());
    assert!(update_json(
        r#"{"mcpServers":[]}"#,
        JsonRoot::McpServers,
        JsonServerShape::Plain,
        false,
    )
    .is_err());
}

#[test]
fn codex_toml_writer_preserves_existing_settings() {
    let first = update_codex_toml("model = \"gpt-5\"\n", false).unwrap();
    assert!(first.contains("model = \"gpt-5\""));
    assert!(first.contains("[mcp_servers.ctx]"));
    assert_eq!(status_codex_toml(&first).unwrap(), McpConfigStatus::Current);
    let second = update_codex_toml(&first, false).unwrap();
    assert_eq!(first, second);
}

#[test]
fn opencode_writer_uses_command_array_shape() {
    let body = update_json("", JsonRoot::Mcp, JsonServerShape::OpenCodeLocal, false).unwrap();
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["mcp"]["ctx"]["command"],
        json!(["ctx", "mcp", "serve"])
    );
    assert_eq!(value["mcp"]["ctx"]["type"], "local");
    assert_eq!(
        status_json(&body, JsonRoot::Mcp).unwrap(),
        McpConfigStatus::Current
    );
}

#[test]
fn goose_yaml_writer_adds_extension_and_is_idempotent() {
    let first = update_goose_yaml("GOOSE_MODEL: test\n", false).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&first).unwrap();
    let ctx = yaml_mapping_get(yaml_mapping_get(&value, "extensions").unwrap(), "ctx").unwrap();
    assert_eq!(yaml_mapping_get(ctx, "cmd").unwrap().as_str(), Some("ctx"));
    assert_eq!(status_goose_yaml(&first).unwrap(), McpConfigStatus::Current);
    let second = update_goose_yaml(&first, false).unwrap();
    assert_eq!(first, second);
}

#[test]
fn continue_yaml_writer_adds_named_server_and_is_idempotent() {
    let first = update_continue_yaml("name: Local\nversion: 1.0.0\nschema: v1\n", false).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&first).unwrap();
    let servers = yaml_mapping_get(&value, "mcpServers")
        .unwrap()
        .as_sequence()
        .unwrap();
    let ctx = continue_server_by_name(servers).unwrap();
    assert_eq!(
        yaml_mapping_get(ctx, "command").unwrap().as_str(),
        Some("ctx")
    );
    assert_eq!(
        status_continue_yaml(&first).unwrap(),
        McpConfigStatus::Current
    );
    let second = update_continue_yaml(&first, false).unwrap();
    assert_eq!(first, second);
}

#[test]
fn detection_uses_home_xdg_and_env_paths() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::create_dir_all(xdg.join("opencode")).unwrap();
    fs::create_dir_all(xdg.join("mimocode")).unwrap();
    let context = McpPathContext::for_tests(home, temp.path().join("repo"))
        .with_xdg_config_home(xdg)
        .with_env_override("CODEX_HOME", temp.path().join("codex-home"));
    assert!(McpAgentArg::Codex.detected(&context));
    assert!(McpAgentArg::Cursor.detected(&context));
    assert!(McpAgentArg::OpenCode.detected(&context));
    assert!(McpAgentArg::MiMoCode.detected(&context));
    assert!(!McpAgentArg::QwenCode.detected(&context));
}

#[test]
fn project_target_reports_unsupported_for_global_only_agents() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::GitHubCopilot.target(true, &context);
    assert!(target.path.is_none());
    let status = status_target(&target);
    assert_eq!(status.status, McpConfigStatus::Unsupported);
}
