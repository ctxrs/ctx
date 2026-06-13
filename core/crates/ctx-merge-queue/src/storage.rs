use std::path::PathBuf;

use super::*;

pub(super) fn merge_queue_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".ctx").join("merge-queue")
}

pub(super) fn merge_queue_repo_root(workspace_root: &Path) -> PathBuf {
    merge_queue_root(workspace_root).join("repo")
}

pub(super) fn merge_queue_patch_path(
    workspace_root: &Path,
    entry_id: MergeQueueEntryId,
) -> PathBuf {
    merge_queue_root(workspace_root)
        .join("patches")
        .join(format!("{}.patch", entry_id.0))
}

pub(super) fn merge_queue_log_path(workspace_root: &Path, run_id: MergeQueueRunId) -> PathBuf {
    merge_queue_root(workspace_root)
        .join("logs")
        .join(format!("{}.log", run_id.0))
}

pub(super) fn merge_queue_worktree_path(
    workspace_root: &Path,
    workspace_id: WorkspaceId,
    entry_id: MergeQueueEntryId,
) -> PathBuf {
    merge_queue_root(workspace_root)
        .join("worktrees")
        .join(workspace_id.0.to_string())
        .join(entry_id.0.to_string())
}

pub(super) async fn write_patch_file(
    workspace_root: &Path,
    entry_id: MergeQueueEntryId,
    patch: &str,
) -> Result<PathBuf> {
    let path = merge_queue_patch_path(workspace_root, entry_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .context("creating merge queue patch dir")?;
    }
    fs::write(&path, patch)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub(super) async fn read_patch_file(path: &str) -> Result<String> {
    let data = fs::read_to_string(path)
        .await
        .with_context(|| format!("reading {path}"))?;
    Ok(data)
}

pub(super) async fn open_log_file(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .context("creating merge queue log dir")?;
    }
    let file = fs::File::create(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    Ok(file)
}

pub(super) async fn write_log_line(file: &mut fs::File, line: &str) -> Result<()> {
    file.write_all(line.as_bytes()).await?;
    Ok(())
}
