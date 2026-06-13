use super::*;

pub(crate) fn resolve_target_path(
    state: &ConnectionManager,
    target: &DeepLinkTarget,
) -> Result<PathBuf> {
    match target {
        DeepLinkTarget::WorktreeFile { worktree_id, file } => {
            let info = resolve_worktree_info(state, worktree_id)?;
            resolve_worktree_path(&info.root, file)
        }
        DeepLinkTarget::Path { path } => {
            let path = PathBuf::from(path);
            let resolved = std::fs::canonicalize(&path)
                .with_context(|| format!("resolving {}", path.display()))?;
            if !resolved.exists() {
                anyhow::bail!("path does not exist");
            }
            Ok(resolved)
        }
    }
}

pub(crate) fn is_target_in_open_workspace(
    state: &ConnectionManager,
    registry: &WorkspaceWindowRegistry,
    target: &DeepLinkTarget,
) -> Result<bool> {
    let workspace_ids = registry.workspace_ids();
    if workspace_ids.is_empty() {
        return Ok(false);
    }

    match target {
        DeepLinkTarget::WorktreeFile { worktree_id, .. } => {
            let info = resolve_worktree_info(state, worktree_id)?;
            Ok(workspace_ids.iter().any(|id| id == &info.workspace_id))
        }
        DeepLinkTarget::Path { path } => {
            let candidate = std::fs::canonicalize(PathBuf::from(path))?;
            for ws_id in workspace_ids {
                if let Ok(root) = resolve_workspace_root(state, &ws_id) {
                    let root = std::fs::canonicalize(root)?;
                    if candidate.starts_with(&root) {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
    }
}

pub(crate) fn resolve_or_create_workspace_id(
    state: &ConnectionManager,
    root_path: &str,
) -> Result<String> {
    if let Some(existing) = resolve_workspace_id_by_path(state, root_path)? {
        return Ok(existing);
    }

    let body = serde_json::json!({ "root_path": root_path });
    let resp = state.daemon_request(DesktopDaemonRequest {
        method: "POST".to_string(),
        path: "/api/workspaces".to_string(),
        body: Some(body.to_string()),
        headers: vec![("content-type".to_string(), "application/json".to_string())],
    })?;
    if resp.status != 200 && resp.status != 201 {
        anyhow::bail!(
            "failed to create workspace ({status}): {body}",
            status = resp.status,
            body = resp.body
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&resp.body).context("parsing workspace response")?;
    value["id"]
        .as_str()
        .map(|id| id.to_string())
        .ok_or_else(|| anyhow!("workspace id missing"))
}

pub(crate) fn resolve_workspace_id_by_path(
    state: &ConnectionManager,
    root_path: &str,
) -> Result<Option<String>> {
    let resp = state.daemon_request(DesktopDaemonRequest {
        method: "GET".to_string(),
        path: "/api/workspaces".to_string(),
        body: None,
        headers: vec![],
    })?;
    if resp.status != 200 {
        anyhow::bail!(
            "failed to list workspaces ({status}): {body}",
            status = resp.status,
            body = resp.body
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&resp.body).context("parsing workspaces response")?;
    let arr = value
        .as_array()
        .ok_or_else(|| anyhow!("workspaces response is not a list"))?;
    for entry in arr {
        let ws_root = entry
            .get("root_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if ws_root == root_path {
            if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

pub(crate) fn resolve_workspace_root(
    state: &ConnectionManager,
    workspace_id: &str,
) -> Result<PathBuf> {
    let resp = state.daemon_request(DesktopDaemonRequest {
        method: "GET".to_string(),
        path: format!("/api/workspaces/{workspace_id}"),
        body: None,
        headers: vec![],
    })?;
    if resp.status != 200 {
        anyhow::bail!(
            "failed to load workspace ({status}): {body}",
            status = resp.status,
            body = resp.body
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&resp.body).context("parsing workspace response")?;
    let root = value
        .get("root_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("workspace root_path missing"))?;
    Ok(PathBuf::from(root))
}

pub(crate) fn resolve_worktree_info(
    state: &ConnectionManager,
    worktree_id: &str,
) -> Result<WorktreeInfo> {
    let resp = state.daemon_request(DesktopDaemonRequest {
        method: "GET".to_string(),
        path: format!("/api/worktrees/{worktree_id}"),
        body: None,
        headers: vec![],
    })?;
    if resp.status != 200 {
        anyhow::bail!(
            "failed to load worktree ({status}): {body}",
            status = resp.status,
            body = resp.body
        );
    }
    let value: serde_json::Value =
        serde_json::from_str(&resp.body).context("parsing worktree response")?;
    let root = value
        .get("root_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("worktree root_path missing"))?;
    let workspace_id = value
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(|id| id.to_string())
        .ok_or_else(|| anyhow!("worktree workspace_id missing"))?;
    Ok(WorktreeInfo {
        root: PathBuf::from(root),
        workspace_id,
    })
}
