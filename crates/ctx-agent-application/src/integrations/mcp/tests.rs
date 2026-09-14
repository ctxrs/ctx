use std::fs;

use ctx_agent_integrations::mcp_config::{
    install_target, ConfigStatus, McpAgentArg, McpInstallRequest, McpPathContext, McpRemoveRequest,
    McpStatusRequest,
};

use super::*;
use crate::IntegrationResultFact;

const PRODUCT: ProductIdentity<'static> = ProductIdentity {
    name: "ctx",
    version: "1.0.0-test",
};

#[test]
fn current_target_is_not_rewritten() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = b"{\n  \"unrelated\": true,\n  \"mcpServers\": {\n    \"ctx\": {\"command\": \"ctx\", \"args\": [\"mcp\", \"serve\"]}\n  }\n}\n";
    fs::write(path, original).unwrap();

    let outcome = install(
        McpInstallRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );

    assert_eq!(outcome.telemetry.result, Some(IntegrationResultFact::Ok));
    assert!(outcome.receipt.results[0].already_installed);
    assert!(!outcome.receipt.results[0].modified);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn status_recovery_preserves_selection_scope_and_conflict_force() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::Codex.target(true, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "[mcp_servers.ctx]\ncommand = 'other'\nargs = []\n").unwrap();

    let outcome = status(
        McpStatusRequest {
            agents: vec![McpAgentArg::Codex],
            all_agents: false,
            project: true,
        },
        &context,
        PRODUCT,
    );

    assert_eq!(outcome.receipt.results[0].status, ConfigStatus::Conflict);
    assert_eq!(
        outcome.recovery_command.as_deref(),
        Some("ctx integrations install mcp --agent codex --project --force")
    );
    assert_eq!(outcome.telemetry.conflicting_targets, Some(1));
}

#[test]
fn unsupported_project_target_is_counted_without_path_or_error_telemetry() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let outcome = status(
        McpStatusRequest {
            agents: vec![McpAgentArg::GitHubCopilot],
            all_agents: false,
            project: true,
        },
        &context,
        PRODUCT,
    );

    assert_eq!(outcome.receipt.results[0].status, ConfigStatus::Unsupported);
    assert_eq!(outcome.telemetry.unsupported_targets, Some(1));
    assert_eq!(outcome.recovery_command, None);
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
#[test]
fn update_preserves_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "{\"unrelated\":true}").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();

    let result = install_target(&target, false);

    assert!(result.success);
    assert!(result.modified);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn remove_is_idempotent_and_preserves_unrelated_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        r#"{"theme":"dark","mcpServers":{"ctx":{"command":"ctx","args":["mcp","serve"]}}}"#,
    )
    .unwrap();

    let first = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );
    let result = &first.receipt.results[0];
    assert!(result.success);
    assert_eq!(result.previous_status, ConfigStatus::Current);
    assert_eq!(result.status, ConfigStatus::Missing);
    assert!(!result.already_absent);
    assert!(result.modified);
    assert_eq!(first.receipt.modified, 1);
    assert_eq!(first.telemetry.result, Some(IntegrationResultFact::Ok));
    assert_eq!(first.telemetry.modified_targets, Some(1));
    assert!(path.is_file());
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["theme"], "dark");
    assert!(value["mcpServers"].as_object().unwrap().is_empty());

    let second = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );
    let result = &second.receipt.results[0];
    assert!(result.success);
    assert_eq!(result.previous_status, ConfigStatus::Missing);
    assert_eq!(result.status, ConfigStatus::Missing);
    assert!(result.already_absent);
    assert!(!result.modified);
    assert_eq!(second.receipt.modified, 0);
}

#[test]
fn remove_preserves_conflict_without_force_and_removes_it_with_force() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = r#"{"unrelated":true,"mcpServers":{"ctx":{"command":"custom","args":[]}}}"#;
    fs::write(path, original).unwrap();

    let blocked = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );
    let result = &blocked.receipt.results[0];
    assert!(!result.success);
    assert_eq!(result.previous_status, ConfigStatus::Conflict);
    assert_eq!(result.status, ConfigStatus::Conflict);
    assert!(!result.modified);
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert_eq!(
        blocked.telemetry.result,
        Some(IntegrationResultFact::PartialError)
    );
    assert_eq!(
        force_remove_command(PRODUCT, &result.target),
        "ctx integrations remove mcp --agent qwen-code --force"
    );

    let forced = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: true,
        },
        &context,
    );
    let result = &forced.receipt.results[0];
    assert!(result.success);
    assert_eq!(result.previous_status, ConfigStatus::Conflict);
    assert_eq!(result.status, ConfigStatus::Missing);
    assert!(result.modified);
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["unrelated"], true);
    assert!(value["mcpServers"].as_object().unwrap().is_empty());
}

#[test]
fn remove_never_overwrites_invalid_or_empty_config() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "{ not json").unwrap();

    let invalid = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: true,
        },
        &context,
    );
    let result = &invalid.receipt.results[0];
    assert!(!result.success);
    assert_eq!(result.previous_status, ConfigStatus::Invalid);
    assert_eq!(result.status, ConfigStatus::Invalid);
    assert_eq!(fs::read_to_string(path).unwrap(), "{ not json");

    fs::write(path, b"").unwrap();
    let empty = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );
    let result = &empty.receipt.results[0];
    assert!(result.success);
    assert!(result.already_absent);
    assert!(!result.modified);
    assert!(path.is_file());
    assert!(fs::read(path).unwrap().is_empty());
}

#[test]
fn remove_missing_entry_does_not_create_a_config_file() {
    let temp = tempfile::tempdir().unwrap();
    let context = McpPathContext::for_tests(temp.path().join("home"), temp.path().join("repo"));
    let target = McpAgentArg::QwenCode.target(false, &context);
    let path = target.path.as_ref().unwrap();

    let outcome = remove(
        McpRemoveRequest {
            agents: vec![McpAgentArg::QwenCode],
            all_agents: false,
            project: false,
            force: false,
        },
        &context,
    );

    let result = &outcome.receipt.results[0];
    assert!(result.success);
    assert_eq!(result.previous_status, ConfigStatus::Missing);
    assert_eq!(result.status, ConfigStatus::Missing);
    assert!(result.already_absent);
    assert!(!result.modified);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}
