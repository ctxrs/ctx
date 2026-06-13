use super::*;

pub async fn load_primary_branch(store: &Store) -> Result<Option<String>> {
    let cfg = load_workspace_settings_doc(store).await?;
    let configured = cfg.vcs.and_then(|vcs| vcs.primary_branch);
    let Some(configured) = configured else {
        return Ok(None);
    };
    let trimmed = configured.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

pub async fn update_primary_branch(store: &Store, primary_branch: &str) -> Result<()> {
    let trimmed = primary_branch.trim().to_string();
    if trimmed.is_empty() {
        bail!("primary_branch is required");
    }
    mutate_workspace_settings_doc(store, "primary_branch", move |cfg| {
        cfg.vcs = Some(WorkspaceVcsConfig {
            primary_branch: Some(trimmed),
        });
        Ok(())
    })
    .await
}

pub async fn update_and_load_primary_branch(store: &Store, primary_branch: &str) -> Result<String> {
    update_primary_branch(store, primary_branch).await?;
    load_primary_branch(store)
        .await?
        .ok_or_else(|| anyhow::anyhow!("primary_branch is required"))
}
