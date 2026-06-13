use super::*;

pub(crate) async fn run_entry_inner<H: MergeQueueHost>(
    state: &Arc<H>,
    workspace: &Workspace,
    entry: &MergeQueueEntry,
    cfg: &MergeQueueConfig,
    log_file: &mut fs::File,
) -> std::result::Result<String, QueueError> {
    let vcs = vcs::driver_for_path(Path::new(&workspace.root_path))
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
    vcs.assert_repo(Path::new(&workspace.root_path))
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;

    let worktree_path =
        merge_queue_worktree_path(Path::new(&workspace.root_path), workspace.id, entry.id);
    let worktree_branch = if vcs.kind() == VcsKind::Jj {
        format!("ctx-merge-queue-{}", entry.id.0)
    } else {
        format!("ctx-merge-queue/{}", entry.id.0)
    };
    let (repo_root, target_head) = if vcs.kind() == VcsKind::Git {
        let repo_root =
            ensure_merge_queue_repo(state.as_ref(), entry, workspace, cfg, log_file).await?;
        let target_head = ensure_merge_queue_target_branch(
            state.as_ref(),
            entry,
            &repo_root,
            &entry.target_branch,
        )
        .await?;
        let _ = remove_worktree(&repo_root, &worktree_path).await;
        let _ = delete_branch(&repo_root, &worktree_branch).await;
        create_worktree(&repo_root, &worktree_path, &target_head, &worktree_branch)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        (Some(repo_root), target_head)
    } else {
        let target_head =
            resolve_target_head(vcs.as_ref(), &workspace.root_path, &entry.target_branch)
                .await
                .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        let _ = remove_worktree(&workspace.root_path, &worktree_path).await;
        create_worktree(
            &workspace.root_path,
            &worktree_path,
            &target_head,
            &worktree_branch,
        )
        .await
        .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        (None, target_head)
    };
    let git_repo_root = repo_root
        .as_deref()
        .unwrap_or_else(|| Path::new(&workspace.root_path));

    if vcs.kind() == VcsKind::Jj {
        ensure_jj_working_copy(&worktree_path, &target_head, log_file, vcs.as_ref()).await?;
    }

    let result = async {
        let patch = read_patch_file(&entry.patch_path)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        write_log_line(log_file, "apply patch\n")
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;
        let apply_target = if vcs.kind() == VcsKind::Git {
            ApplyPatchTarget::Index
        } else {
            ApplyPatchTarget::Worktree
        };
        if let Err(err) = apply_patch(
            state.as_ref(),
            entry,
            vcs.as_ref(),
            git_repo_root,
            &worktree_path,
            &patch,
            apply_target,
        )
        .await
        {
            match err {
                QueueError::Conflict { message } => {
                    let _ = write_log_line(log_file, &format!("apply patch conflict: {message}\n"))
                        .await;
                    return Err(QueueError::Conflict {
                        message: MERGE_QUEUE_CONFLICT_MESSAGE.to_string(),
                    });
                }
                other => return Err(other),
            }
        }

        let has_changes = if vcs.kind() == VcsKind::Git {
            has_staged_changes(state.as_ref(), entry, &worktree_path).await?
        } else {
            has_worktree_changes(vcs.as_ref(), &worktree_path, &target_head).await?
        };
        if !has_changes {
            return Err(QueueError::fail(
                "patch did not produce any changes".to_string(),
                None,
                None,
            ));
        }

        let message = entry
            .message
            .as_deref()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or("merge queue entry");
        commit_changes(
            state.as_ref(),
            entry,
            &worktree_path,
            vcs.kind(),
            message,
            log_file,
        )
        .await?;
        let commit_sha = vcs
            .rev_parse_head(&worktree_path)
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, None))?;

        for cmd in &cfg.verify_commands {
            run_verify_command(
                state.as_ref(),
                &worktree_path,
                entry,
                cmd,
                &commit_sha,
                log_file,
            )
            .await?;
        }

        let target_checkout = if vcs.kind() == VcsKind::Git {
            find_checked_out_worktree_for_branch(
                state.as_ref(),
                entry,
                git_repo_root,
                &entry.target_branch,
            )
            .await
            .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.clone())))?
        } else {
            None
        };
        if let Some(path) = target_checkout.as_ref() {
            let dirty = vcs
                .status_porcelain(Path::new(path))
                .await
                .map_err(|e| QueueError::fail(e.to_string(), None, Some(commit_sha.clone())))?;
            if !dirty.is_empty() {
                return Err(QueueError::fail(
                    format!(
                        "target branch {} is checked out at {} with uncommitted changes",
                        entry.target_branch, path
                    ),
                    None,
                    Some(commit_sha.clone()),
                ));
            }
        }

        finalize_target_branch(
            state.as_ref(),
            workspace,
            entry,
            cfg,
            vcs.as_ref(),
            git_repo_root,
            target_checkout.as_deref(),
            &target_head,
            &commit_sha,
            log_file,
        )
        .await?;

        if vcs.kind() == VcsKind::Git {
            if let Some(repo_root) = repo_root.as_ref() {
                if let Err(err) = maybe_sync_canonical_worktree(
                    state,
                    workspace,
                    entry,
                    repo_root,
                    &commit_sha,
                    cfg.canonical_sync,
                    log_file,
                )
                .await
                {
                    let _ = write_log_line(log_file, &format!("canonical sync failed: {err:#}\n"))
                        .await;
                    tracing::warn!("merge queue canonical sync failed: {err:#}");
                }
            }
        }

        Ok(commit_sha)
    }
    .await;

    if vcs.kind() == VcsKind::Git {
        if let Some(repo_root) = repo_root.as_ref() {
            let _ = remove_worktree(repo_root, &worktree_path).await;
            let _ = delete_branch(repo_root, &worktree_branch).await;
        }
    } else {
        let _ = remove_worktree(&workspace.root_path, &worktree_path).await;
    }
    result
}
