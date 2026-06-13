use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ctx_core::ids::{WorkspaceId, WorktreeId};
use ctx_core::models::Workspace;
use ctx_sandbox_container_runtime::SandboxCommandMode;
use ctx_sandbox_contract::container_worktree_root;
use ctx_sandbox_contract::sandbox_workspace_root;
use ctx_storage_admission::StorageAdmissionOperation;
use ctx_workspace_container::workspace_container_name;

mod copy;
mod sandbox;
mod storage;

pub use storage::set_test_preflight_storage_samples_override;

pub async fn remove_live_worktree_root(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace_id: WorkspaceId,
    live_worktree_root: &Path,
) -> Result<()> {
    let container_id = workspace_container_name(workspace_id);
    sandbox::remove_live_worktree_root(data_root, mode, &container_id, live_worktree_root).await
}

pub async fn ensure_worktree_from_host_copy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace_id: WorkspaceId,
    worktree_id: WorktreeId,
    host_source_root: &Path,
    base_commit_sha: &str,
    branch_name: &str,
) -> Result<PathBuf> {
    let container_id = workspace_container_name(workspace_id);
    let dest_root = container_worktree_root(worktree_id);
    tracing::info!(
        workspace_id = %workspace_id.0,
        worktree_id = %worktree_id.0,
        container_id = %container_id,
        dest_root = %dest_root.display(),
        "provisioning disk-isolated worktree from host copy"
    );

    let estimated_copy_bytes = copy::estimate_self_contained_copy_size_bytes(host_source_root)
        .await
        .with_context(|| {
            format!(
                "estimating self-contained sandbox copy size from {}",
                host_source_root.display()
            )
        })?;
    let workspace_root = sandbox_workspace_root();
    storage::preflight_disk_isolated_copy(
        data_root,
        mode,
        &container_id,
        estimated_copy_bytes,
        &workspace_root,
        StorageAdmissionOperation::DiskIsolatedWorktreeMaterialization,
    )
    .await
    .context("preflighting disk-isolated worktree materialization")?;

    let (copy_root, _staging_guard) =
        copy::prepare_self_contained_copy_root(data_root, host_source_root)
            .await
            .with_context(|| {
                format!(
                    "preparing self-contained sandbox copy root from {}",
                    host_source_root.display()
                )
            })?;

    sandbox::ensure_directory(data_root, mode, &container_id, &workspace_root)
        .await
        .context("ensuring disk-isolated workspace volume root")?;
    sandbox::ensure_directory(data_root, mode, &container_id, &dest_root)
        .await
        .context("creating disk-isolated worktree root")?;
    copy::stream_dir_to_container(data_root, mode, &container_id, &copy_root, &dest_root)
        .await
        .context("streaming host copy into disk-isolated worktree")?;
    tracing::debug!(
        workspace_id = %workspace_id.0,
        worktree_id = %worktree_id.0,
        "disk-isolated host copy completed"
    );
    sandbox::best_effort_make_user_writable(data_root, mode, &container_id, &dest_root).await?;

    sandbox::checkout_branch_at_base(
        data_root,
        mode,
        &container_id,
        &dest_root,
        branch_name,
        base_commit_sha,
    )
    .await
    .context("resetting disk-isolated worktree branch")?;
    tracing::debug!(
        workspace_id = %workspace_id.0,
        worktree_id = %worktree_id.0,
        base_commit_sha = %base_commit_sha,
        branch_name = %branch_name,
        "disk-isolated checkout completed"
    );

    sandbox::verify_container_git_repo(data_root, mode, &container_id, &dest_root).await?;
    tracing::info!(
        workspace_id = %workspace_id.0,
        worktree_id = %worktree_id.0,
        "disk-isolated worktree repo verification succeeded"
    );

    Ok(dest_root)
}

pub async fn ensure_workspace_root_from_host_copy(
    data_root: &Path,
    mode: &SandboxCommandMode,
    workspace: &Workspace,
) -> Result<PathBuf> {
    let container_id = workspace_container_name(workspace.id);
    let dest_root = sandbox_workspace_root();
    if sandbox::verify_container_git_repo(data_root, mode, &container_id, &dest_root)
        .await
        .is_ok()
    {
        return Ok(dest_root);
    }

    #[cfg(windows)]
    {
        let _ = data_root;
        let _ = workspace;
        anyhow::bail!("pre-task sandbox workspace materialization is unsupported on Windows");
    }

    let host_workspace_root = Path::new(&workspace.root_path);
    if !host_workspace_root.exists() {
        anyhow::bail!(
            "host workspace root is unavailable for sandbox materialization: {}",
            host_workspace_root.display()
        );
    }

    let estimated_copy_bytes = copy::estimate_self_contained_copy_size_bytes(host_workspace_root)
        .await
        .with_context(|| {
            format!(
                "estimating self-contained sandbox workspace copy size from {}",
                host_workspace_root.display()
            )
        })?;
    storage::preflight_disk_isolated_copy(
        data_root,
        mode,
        &container_id,
        estimated_copy_bytes,
        &dest_root,
        StorageAdmissionOperation::DiskIsolatedWorkspaceMaterialization,
    )
    .await
    .context("preflighting disk-isolated workspace materialization")?;

    let (copy_root, _staging_guard) =
        copy::prepare_self_contained_copy_root(data_root, host_workspace_root)
            .await
            .with_context(|| {
                format!(
                    "preparing self-contained sandbox workspace copy root from {}",
                    host_workspace_root.display()
                )
            })?;
    sandbox::ensure_empty_container_root(data_root, mode, &container_id, &dest_root)
        .await
        .context("preparing disk-isolated workspace root")?;
    copy::stream_dir_to_container(data_root, mode, &container_id, &copy_root, &dest_root)
        .await
        .context("streaming host copy into disk-isolated workspace root")?;
    sandbox::best_effort_make_user_writable(data_root, mode, &container_id, &dest_root).await?;
    sandbox::verify_container_git_repo(data_root, mode, &container_id, &dest_root)
        .await
        .context("verifying seeded disk-isolated workspace root")?;
    Ok(dest_root)
}

#[cfg(test)]
mod tests;
