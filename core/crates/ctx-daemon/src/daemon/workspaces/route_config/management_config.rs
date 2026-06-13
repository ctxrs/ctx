use ctx_route_contracts::workspaces::{
    UpdateWorkspaceMergeQueueConfigRequest, UpdateWorktreeBootstrapConfigRequest,
    WorkspaceExecutionConfigRouteSnapshot, WorkspaceMergeQueueConfigRouteResponse,
    WorkspaceWorktreeBootstrapConfigRouteResponse,
};
use ctx_workspace_config as workspace_config;

pub(in crate::daemon::workspaces) fn workspace_execution_config_route_snapshot(
    snapshot: workspace_config::ExecutionConfigSnapshot,
) -> WorkspaceExecutionConfigRouteSnapshot {
    WorkspaceExecutionConfigRouteSnapshot {
        source: snapshot.source,
        environment: snapshot.environment,
        network_mode: snapshot.network_mode,
        allowlist: snapshot.allowlist,
    }
}

pub(in crate::daemon::workspaces) fn merge_queue_config_route_response(
    cfg: workspace_config::MergeQueueConfig,
) -> WorkspaceMergeQueueConfigRouteResponse {
    WorkspaceMergeQueueConfigRouteResponse {
        enabled: cfg.enabled,
        target_branch: cfg.target_branch,
        verify_command: cfg.verify_commands.into_iter().next(),
        push_on_success: cfg.push_on_success,
        push_remote: cfg.push_remote,
        push_branch: cfg.push_branch,
    }
}

pub(in crate::daemon::workspaces) fn worktree_bootstrap_config_route_response(
    cfg: Option<workspace_config::WorktreeBootstrapConfig>,
) -> WorkspaceWorktreeBootstrapConfigRouteResponse {
    WorkspaceWorktreeBootstrapConfigRouteResponse {
        setup_command: cfg.as_ref().and_then(|value| value.setup_command.clone()),
        timeout_sec: cfg.as_ref().and_then(|value| value.timeout_sec),
        wait_for_completion: cfg.as_ref().and_then(|value| value.wait_for_completion),
    }
}

pub(in crate::daemon::workspaces) fn merge_queue_config_update(
    request: UpdateWorkspaceMergeQueueConfigRequest,
) -> workspace_config::MergeQueueConfigUpdate {
    let verify_commands = request
        .verify_command
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| vec![value])
        .unwrap_or_default();

    workspace_config::MergeQueueConfigUpdate {
        enabled: request.enabled,
        target_branch: request.target_branch,
        verify_commands,
        push_on_success: request.push_on_success,
        push_remote: request.push_remote,
        push_branch: request.push_branch,
        canonical_sync: Some(workspace_config::MergeQueueCanonicalSync::CleanOnly),
    }
}

pub(in crate::daemon::workspaces) fn worktree_bootstrap_config_update(
    request: UpdateWorktreeBootstrapConfigRequest,
) -> workspace_config::WorktreeBootstrapConfigUpdate {
    workspace_config::WorktreeBootstrapConfigUpdate {
        setup_command: request
            .setup_command
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        timeout_sec: request.timeout_sec,
        wait_for_completion: request.wait_for_completion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_queue_route_response_preserves_default_wire_shape() {
        let response =
            merge_queue_config_route_response(workspace_config::MergeQueueConfig::new_default());

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "enabled": false,
                "target_branch": "main",
                "push_on_success": false,
                "push_remote": "origin",
                "push_branch": "main"
            })
        );
    }

    #[test]
    fn merge_queue_route_response_projects_first_verify_command() {
        let mut config = workspace_config::MergeQueueConfig::new_default();
        config.verify_commands = vec!["pnpm test".to_string(), "cargo test".to_string()];

        let response = merge_queue_config_route_response(config);

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "enabled": false,
                "target_branch": "main",
                "verify_command": "pnpm test",
                "push_on_success": false,
                "push_remote": "origin",
                "push_branch": "main"
            })
        );
    }

    #[test]
    fn merge_queue_update_request_trims_blank_verify_command() {
        let update = UpdateWorkspaceMergeQueueConfigRequest {
            enabled: true,
            target_branch: Some(" main ".to_string()),
            verify_command: Some("   ".to_string()),
            push_on_success: Some(true),
            push_remote: Some(" origin ".to_string()),
            push_branch: Some(" dev ".to_string()),
        };
        let update = merge_queue_config_update(update).normalized();

        assert!(update.verify_commands.is_empty());
        assert_eq!(update.target_branch.as_deref(), Some("main"));
        assert_eq!(update.push_remote.as_deref(), Some("origin"));
        assert_eq!(update.push_branch.as_deref(), Some("dev"));
    }

    #[test]
    fn worktree_bootstrap_route_response_omits_empty_config() {
        let response = worktree_bootstrap_config_route_response(None);

        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn worktree_bootstrap_update_request_trims_blank_setup_command() {
        let update = UpdateWorktreeBootstrapConfigRequest {
            setup_command: Some("   ".to_string()),
            timeout_sec: Some(30),
            wait_for_completion: Some(true),
        };
        let update = worktree_bootstrap_config_update(update);

        assert_eq!(update.setup_command, None);
        assert_eq!(update.timeout_sec, Some(30));
        assert_eq!(update.wait_for_completion, Some(true));
    }
}
