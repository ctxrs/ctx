use super::projection::refresh_worktree_vcs_projection;
use super::{WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};

async fn run_worktree_vcs_job(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree_id: ctx_core::ids::WorktreeId,
    refresh_summary: bool,
    refresh_touched_files: bool,
) {
    let result = match execution.store_for_worktree(worktree_id).await {
        Ok(store) => match store.get_worktree(worktree_id).await {
            Ok(Some(worktree)) => {
                refresh_worktree_vcs_projection(
                    &runtime,
                    &execution,
                    &worktree,
                    refresh_summary,
                    refresh_touched_files,
                    true,
                )
                .await
            }
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    };

    if let Err(err) = result {
        tracing::warn!(
            worktree_id = %worktree_id.0,
            "worktree vcs scheduler refresh failed: {err:#}"
        );
    }

    let should_notify = runtime.finish_worktree_vcs_job(worktree_id).await;
    if should_notify {
        runtime.notify_worktree_vcs_scheduler();
    }
}

async fn next_worktree_vcs_job(
    runtime: &WorktreeVcsRuntimeHost,
) -> Option<(ctx_core::ids::WorktreeId, bool, bool)> {
    runtime.claim_next_worktree_vcs_job().await.map(|job| {
        (
            job.worktree_id,
            job.refresh_summary,
            job.refresh_touched_files,
        )
    })
}

async fn run_worktree_vcs_scheduler(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
) {
    loop {
        runtime.wait_worktree_vcs_scheduler_notification().await;
        loop {
            let permit = match runtime.try_acquire_worktree_vcs_scheduler_permit() {
                Some(permit) => permit,
                None => break,
            };
            let Some((worktree_id, refresh_summary, refresh_touched_files)) =
                next_worktree_vcs_job(&runtime).await
            else {
                drop(permit);
                break;
            };
            let runtime = runtime.clone();
            let execution = execution.clone();
            tokio::spawn(async move {
                let _permit = permit;
                run_worktree_vcs_job(
                    runtime,
                    execution,
                    worktree_id,
                    refresh_summary,
                    refresh_touched_files,
                )
                .await;
            });
        }
    }
}

pub(super) async fn ensure_worktree_vcs_scheduler_started(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
) {
    if !runtime.mark_worktree_vcs_scheduler_started() {
        return;
    }
    tokio::spawn(async move {
        run_worktree_vcs_scheduler(runtime, execution).await;
    });
}
