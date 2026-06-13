use super::*;

#[derive(Debug, Clone)]
pub struct WorktreeBootstrapConfig {
    pub setup_command: Option<String>,
    pub timeout_sec: Option<u64>,
    pub wait_for_completion: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeBootstrapConfigUpdate {
    pub setup_command: Option<String>,
    pub timeout_sec: Option<u64>,
    pub wait_for_completion: Option<bool>,
}

pub async fn load_worktree_bootstrap_config(
    store: &Store,
) -> Result<Option<WorktreeBootstrapConfig>> {
    let cfg = load_workspace_settings_doc(store).await?;
    let bootstrap = cfg.worktree_bootstrap.map(|b| WorktreeBootstrapConfig {
        setup_command: b
            .setup_command
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        timeout_sec: b.timeout_sec,
        wait_for_completion: b.wait_for_completion,
    });
    Ok(bootstrap)
}

pub async fn update_worktree_bootstrap_config(
    store: &Store,
    update: WorktreeBootstrapConfigUpdate,
) -> Result<()> {
    let setup_command = update
        .setup_command
        .as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    mutate_workspace_settings_doc(store, "worktree_bootstrap", move |cfg| {
        if setup_command.is_none()
            && update.timeout_sec.is_none()
            && update.wait_for_completion.is_none()
        {
            cfg.worktree_bootstrap = None;
        } else {
            cfg.worktree_bootstrap = Some(WorkspaceWorktreeBootstrapConfig {
                setup_command,
                timeout_sec: update.timeout_sec,
                wait_for_completion: update.wait_for_completion,
            });
        }
        Ok(())
    })
    .await
}
