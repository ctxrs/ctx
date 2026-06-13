use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard};
use std::time::Duration;

use anyhow::Result;
use ctx_core::models::Worktree;
use ctx_worktree_vcs_service::{
    normalize_worktree_vcs_watch_path, worktree_vcs_invalidation_for_watch_paths,
    WorktreeVcsInvalidation, WorktreeVcsWatchDebounceState,
};
use notify::{Event, RecommendedWatcher};

use super::super::{mark_worktree_vcs_dirty, WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};

fn lock_watch_pending<'a>(
    pending: &'a Arc<StdMutex<WorktreeVcsWatchDebounceState>>,
) -> StdMutexGuard<'a, WorktreeVcsWatchDebounceState> {
    match pending.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(
                "git status watcher pending-state mutex was poisoned; continuing with inner state"
            );
            poisoned.into_inner()
        }
    }
}

async fn dispatch_invalidation(
    runtime: &WorktreeVcsRuntimeHost,
    execution: &WorktreeVcsExecutionHost,
    worktree: &Worktree,
    pending: WorktreeVcsInvalidation,
) {
    if !pending.any() {
        return;
    }
    let (dirty_bits, candidate_paths) = pending.into_parts();
    if let Err(err) =
        mark_worktree_vcs_dirty(runtime, execution, worktree, dirty_bits, candidate_paths).await
    {
        tracing::warn!(worktree_id = %worktree.id.0, "git status invalidation failed: {err:#}");
    }
}

pub(super) fn build_git_status_watcher(
    runtime: WorktreeVcsRuntimeHost,
    execution: WorktreeVcsExecutionHost,
    worktree: Worktree,
    worktree_root: PathBuf,
    metadata_roots: Vec<PathBuf>,
    debounce: Duration,
) -> Result<RecommendedWatcher> {
    let handle = tokio::runtime::Handle::current();
    let pending = Arc::new(StdMutex::new(WorktreeVcsWatchDebounceState::default()));
    let worktree_root = normalize_worktree_vcs_watch_path(&worktree_root);
    let metadata_roots = metadata_roots
        .into_iter()
        .map(|path| normalize_worktree_vcs_watch_path(&path))
        .collect::<Vec<_>>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let invalidation = worktree_vcs_invalidation_for_watch_paths(
                &event.paths,
                &worktree_root,
                &metadata_roots,
            );
            if invalidation.any() {
                let should_spawn = {
                    let mut guard = lock_watch_pending(&pending);
                    guard.merge_invalidation(invalidation)
                };
                if should_spawn {
                    let pending = pending.clone();
                    let runtime = runtime.clone();
                    let execution = execution.clone();
                    let worktree = worktree.clone();
                    let handle = handle.clone();
                    handle.spawn(async move {
                        loop {
                            tokio::time::sleep(debounce).await;
                            let next = {
                                let mut guard = lock_watch_pending(&pending);
                                guard.take_invalidation()
                            };
                            dispatch_invalidation(&runtime, &execution, &worktree, next).await;
                            let mut guard = lock_watch_pending(&pending);
                            if guard.finish_dispatch_cycle() {
                                continue;
                            }
                            break;
                        }
                    });
                }
            }
        }
    })?;
    Ok(watcher)
}
