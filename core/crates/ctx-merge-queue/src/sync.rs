use std::path::PathBuf;

use ctx_fs::git::rev_parse_ref;

use super::storage::{merge_queue_repo_root, write_log_line};
use super::*;

pub(super) async fn ensure_jj_working_copy(
    worktree_path: &Path,
    target_head: &str,
    log_file: &mut fs::File,
    vcs: &dyn VcsDriver,
) -> std::result::Result<(), QueueError> {
    let current = vcs
        .rev_parse_head(worktree_path)
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if current.trim() != target_head.trim() {
        return Ok(());
    }
    write_log_line(log_file, "jj new\n")
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    let output = vcs::jj_command_output(worktree_path, &["new"])
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    write_log_line(log_file, &String::from_utf8_lossy(&output.stdout))
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    write_log_line(log_file, &String::from_utf8_lossy(&output.stderr))
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if !output.status.success() {
        return Err(QueueError::fail(
            "jj new failed".to_string(),
            Some(output.status.code().unwrap_or(1) as i64),
            None,
        ));
    }
    Ok(())
}

pub(super) async fn ensure_merge_queue_repo<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    workspace: &Workspace,
    cfg: &MergeQueueConfig,
    log_file: &mut fs::File,
) -> std::result::Result<PathBuf, QueueError> {
    let workspace_root = Path::new(&workspace.root_path);
    let repo_root = merge_queue_repo_root(workspace_root);
    if let Some(parent) = repo_root.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    }

    let needs_init = match fs::metadata(&repo_root).await {
        Ok(meta) => !meta.is_dir() || !repo_root.join(".git").exists(),
        Err(_) => true,
    };
    if needs_init {
        fs::create_dir_all(&repo_root)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        write_log_line(log_file, "init merge queue repo\n")
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        let mut cmd =
            merge_queue_command(state, entry, "git init", "git", Some(&repo_root), &[]).await;
        let output = cmd
            .arg("-C")
            .arg(&repo_root)
            .arg("init")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(QueueError::fail(
                format!("merge queue repo init failed: {stderr}"),
                Some(output.status.code().unwrap_or(1) as i64),
                None,
            ));
        }
        let mut cmd = merge_queue_command(
            state,
            entry,
            "git symbolic-ref HEAD",
            "git",
            Some(&repo_root),
            &[],
        )
        .await;
        let output = cmd
            .arg("-C")
            .arg(&repo_root)
            .args(["symbolic-ref", "HEAD", MERGE_QUEUE_HEAD_REF])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(QueueError::fail(
                format!("merge queue repo HEAD update failed: {stderr}"),
                Some(output.status.code().unwrap_or(1) as i64),
                None,
            ));
        }
    }

    ensure_git_remote(
        state,
        entry,
        &repo_root,
        MERGE_QUEUE_CANONICAL_REMOTE,
        workspace.root_path.as_str(),
    )
    .await?;

    if cfg.push_remote != MERGE_QUEUE_CANONICAL_REMOTE {
        if let Some(url) =
            git_remote_get_url(state, entry, workspace_root, &cfg.push_remote).await?
        {
            ensure_git_remote(state, entry, &repo_root, &cfg.push_remote, url.trim()).await?;
        } else if cfg.push_on_success {
            return Err(QueueError::fail(
                format!(
                    "merge queue push remote {} not configured in canonical repo",
                    cfg.push_remote
                ),
                None,
                None,
            ));
        }
    }

    Ok(repo_root)
}

pub(super) async fn ensure_merge_queue_target_branch<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    target_branch: &str,
) -> std::result::Result<String, QueueError> {
    if merge_queue_branch_exists(state, entry, repo_root, target_branch).await? {
        return rev_parse_ref(repo_root, target_branch)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None));
    }

    let mut cmd = merge_queue_command(
        state,
        entry,
        "git fetch canonical",
        "git",
        Some(repo_root),
        &[],
    )
    .await;
    let refspec = format!("refs/heads/{target_branch}:refs/heads/{target_branch}");
    let output = cmd
        .arg("-C")
        .arg(repo_root)
        .args(["fetch", MERGE_QUEUE_CANONICAL_REMOTE, &refspec])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(QueueError::fail(
            format!("merge queue fetch failed: {stderr}"),
            Some(output.status.code().unwrap_or(1) as i64),
            None,
        ));
    }

    rev_parse_ref(repo_root, target_branch)
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))
}

pub(super) async fn merge_queue_branch_exists<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    target_branch: &str,
) -> std::result::Result<bool, QueueError> {
    let mut cmd = merge_queue_command(
        state,
        entry,
        "git show-ref --verify",
        "git",
        Some(repo_root),
        &[],
    )
    .await;
    let output = cmd
        .arg("-C")
        .arg(repo_root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{target_branch}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(QueueError::fail(
        format!("git show-ref failed: {stderr}"),
        Some(output.status.code().unwrap_or(1) as i64),
        None,
    ))
}

pub(super) async fn git_remote_get_url<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    remote: &str,
) -> std::result::Result<Option<String>, QueueError> {
    let mut cmd = merge_queue_command(
        state,
        entry,
        "git remote get-url",
        "git",
        Some(repo_root),
        &[],
    )
    .await;
    let output = cmd
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", remote])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ));
    }
    if output.status.code() == Some(2) {
        return Ok(None);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(QueueError::fail(
        format!("git remote get-url failed: {stderr}"),
        Some(output.status.code().unwrap_or(1) as i64),
        None,
    ))
}

pub(super) async fn ensure_git_remote<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    repo_root: &Path,
    remote: &str,
    url: &str,
) -> std::result::Result<(), QueueError> {
    if let Some(existing) = git_remote_get_url(state, entry, repo_root, remote).await? {
        if existing.trim() == url {
            return Ok(());
        }
        let mut cmd = merge_queue_command(
            state,
            entry,
            "git remote set-url",
            "git",
            Some(repo_root),
            &[],
        )
        .await;
        let output = cmd
            .arg("-C")
            .arg(repo_root)
            .args(["remote", "set-url", remote, url])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(QueueError::fail(
                format!("git remote set-url failed: {stderr}"),
                Some(output.status.code().unwrap_or(1) as i64),
                None,
            ));
        }
        return Ok(());
    }

    let mut cmd =
        merge_queue_command(state, entry, "git remote add", "git", Some(repo_root), &[]).await;
    let output = cmd
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "add", remote, url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(QueueError::fail(
            format!("git remote add failed: {stderr}"),
            Some(output.status.code().unwrap_or(1) as i64),
            None,
        ));
    }
    Ok(())
}

pub(super) async fn maybe_update_worktree_base_commit_for_path<H: MergeQueueHost>(
    state: &H,
    workspace_id: WorkspaceId,
    worktree_path: &str,
    commit_sha: &str,
) -> Result<Option<Worktree>> {
    let store = H::protected_workspace_store(state, workspace_id).await?;
    let worktrees = store.list_worktrees(workspace_id).await?;
    let checkout_path = fs::canonicalize(worktree_path)
        .await
        .unwrap_or_else(|_| PathBuf::from(worktree_path));
    for worktree in worktrees {
        let root_path = fs::canonicalize(&worktree.root_path)
            .await
            .unwrap_or_else(|_| PathBuf::from(&worktree.root_path));
        if root_path == checkout_path {
            store
                .update_worktree_base_commit(worktree.id, commit_sha)
                .await?;
            return Ok(Some(worktree));
        }
    }
    Ok(None)
}

pub(super) async fn maybe_sync_canonical_worktree<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace: &Workspace,
    entry: &MergeQueueEntry,
    merge_queue_repo_root: &Path,
    commit_sha: &str,
    policy: MergeQueueCanonicalSync,
    log_file: &mut fs::File,
) -> Result<()> {
    if policy == MergeQueueCanonicalSync::Never {
        write_log_line(log_file, "canonical sync disabled\n").await?;
        return Ok(());
    }

    let target_checkout = find_checked_out_worktree_for_branch(
        state.as_ref(),
        entry,
        Path::new(&workspace.root_path),
        &entry.target_branch,
    )
    .await?;
    let Some(path) = target_checkout else {
        write_log_line(
            log_file,
            "canonical sync skipped: target branch not checked out\n",
        )
        .await?;
        return Ok(());
    };
    let canonical_root = fs::canonicalize(&workspace.root_path)
        .await
        .unwrap_or_else(|_| PathBuf::from(&workspace.root_path));
    let checkout_root = fs::canonicalize(&path)
        .await
        .unwrap_or_else(|_| PathBuf::from(&path));
    if checkout_root != canonical_root {
        write_log_line(
            log_file,
            "canonical sync skipped: target branch checked out in another worktree\n",
        )
        .await?;
        return Ok(());
    }

    let dirty = git_status_porcelain(&path).await?;
    if !dirty.is_empty() && policy == MergeQueueCanonicalSync::CleanOnly {
        let message = format!(
            "canonical sync skipped: target branch {} is dirty at {}",
            entry.target_branch, path
        );
        write_log_line(log_file, &format!("{message}\n")).await?;
        if let Some(session_id) = entry.session_id {
            emit_merge_queue_canonical_sync_notice(
                state,
                session_id,
                None,
                &entry.target_branch,
                commit_sha,
                "skipped",
                &message,
            )
            .await?;
        }
        return Ok(());
    }

    write_log_line(
        log_file,
        &format!(
            "canonical sync: fetch {} from merge-queue repo\n",
            entry.target_branch
        ),
    )
    .await?;
    fetch_merge_queue_target_branch(
        state.as_ref(),
        entry,
        Path::new(&workspace.root_path),
        merge_queue_repo_root,
        &entry.target_branch,
    )
    .await?;
    let previous_head = rev_parse_ref(&path, "HEAD")
        .await
        .unwrap_or_else(|_| "unknown".to_string());
    write_log_line(
        log_file,
        &format!(
            "canonical sync: reset {} to {}\n",
            entry.target_branch, commit_sha
        ),
    )
    .await?;
    reset_worktree_to_commit(state.as_ref(), entry, &path, commit_sha).await?;
    let worktree =
        maybe_update_worktree_base_commit_for_path(state.as_ref(), workspace.id, &path, commit_sha)
            .await?;
    if let Some(session_id) = entry.session_id {
        if let Some(worktree) = worktree {
            emit_merge_queue_sync_notice(
                state,
                session_id,
                &worktree,
                &entry.target_branch,
                &previous_head,
                commit_sha,
            )
            .await?;
        } else {
            emit_merge_queue_canonical_sync_notice(
                state,
                session_id,
                None,
                &entry.target_branch,
                commit_sha,
                "applied",
                &format!(
                    "canonical sync applied; reset {path} from {previous_head} to {commit_sha}",
                ),
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn fetch_merge_queue_target_branch<H: MergeQueueHost>(
    state: &H,
    entry: &MergeQueueEntry,
    canonical_root: &Path,
    merge_queue_repo_root: &Path,
    target_branch: &str,
) -> Result<()> {
    let mut cmd = merge_queue_command(
        state,
        entry,
        "git fetch merge-queue repo",
        "git",
        Some(canonical_root),
        &[],
    )
    .await;
    let output = cmd
        .arg("-C")
        .arg(canonical_root)
        .args([
            "fetch",
            merge_queue_repo_root
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid merge queue repo path"))?,
            &format!("refs/heads/{target_branch}"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("running git fetch")?;
    if !output.status.success() {
        bail!(
            "git fetch from merge queue repo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
